// Copyright 2026 The PonWrt Authors.
// SPDX-License-Identifier: BSD-3-Clause
// Linux cryptodev 1.14 UAPI and AF_ALG AEAD transport, little-endian 64-bit ABI.
// Reference: xray-core patches 190/192, BSD-3-Clause, PonWrt Authors / coolsnowwolf.
use super::{Backend, MAX, TAG, Transport, output_len, wipe};
use std::{
    io,
    mem::{size_of, zeroed},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};
use zeroize::Zeroizing;

const SOL_ALG: i32 = 279;
const ALG: &str = "gcm_base(ctr(aes-eip93),ghash-generic)";
const LIMIT: usize = 128;
static SESSIONS: AtomicUsize = AtomicUsize::new(0);
struct Lease(&'static AtomicUsize);
impl Lease {
    fn acquire() -> io::Result<Self> {
        Self::acquire_from(&SESSIONS)
    }
    fn acquire_from(counter: &'static AtomicUsize) -> io::Result<Self> {
        counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < LIMIT).then_some(n + 1)
            })
            .map(|_| Self(counter))
            .map_err(|_| io::Error::from_raw_os_error(libc::EMFILE))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(super) fn open(backend: Backend, key: &[u8]) -> io::Result<Box<dyn Transport>> {
    if key.len() != 16 && key.len() != 32 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    match backend {
        Backend::Devcrypto => Ok(Box::new(Devcrypto::open(key)?)),
        Backend::Afalg => Ok(Box::new(Afalg::open(key, ALG)?)),
    }
}
fn cvt(n: i32) -> io::Result<i32> {
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n)
    }
}
fn owned(n: i32) -> io::Result<OwnedFd> {
    // SAFETY: a successful syscall returns a newly owned file descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(cvt(n)?) })
}
type Call = fn(&OwnedFd, libc::c_ulong, *mut libc::c_void) -> io::Result<()>;
fn ioctl(fd: &OwnedFd, request: libc::c_ulong, data: *mut libc::c_void) -> io::Result<()> {
    // SAFETY: requests below match the repr(C) UAPI structure passed by callers.
    // musl takes c_int; glibc takes c_ulong. Preserve the ioctl bit pattern.
    cvt(unsafe { libc::ioctl(fd.as_raw_fd(), request as _, data) }).map(|_| ())
}

#[repr(C)]
#[derive(Default)]
struct SessionOp {
    cipher: u32,
    mac: u32,
    key_len: u32,
    key: *const u8,
    mac_key_len: u32,
    mac_key: *const u8,
    session: u32,
}
#[repr(C)]
struct AlgorithmInfo {
    name: [u8; 64],
    driver: [u8; 64],
}
#[repr(C)]
struct SessionInfo {
    session: u32,
    cipher: AlgorithmInfo,
    hash: AlgorithmInfo,
    align_mask: u16,
    flags: u32,
}
#[repr(C)]
#[derive(Default)]
struct AuthOp {
    session: u32,
    op: u16,
    flags: u16,
    len: u32,
    auth_len: u32,
    auth_src: *const u8,
    src: *const u8,
    dst: *mut u8,
    tag: *mut u8,
    tag_len: u32,
    iv: *const u8,
    iv_len: u32,
}
const GET_SESSION: libc::c_ulong = 0xc0306366;
const END_SESSION: libc::c_ulong = 0x40046367;
const GET_INFO: libc::c_ulong = 0xc10c636b;
const AUTH_CRYPT: libc::c_ulong = 0xc048636d;
const _: () = {
    assert!(size_of::<SessionOp>() == 48);
    assert!(size_of::<SessionInfo>() == 268);
    assert!(size_of::<AuthOp>() == 72);
};

struct Aligned {
    bytes: Zeroizing<Vec<u8>>,
    start: usize,
    len: usize,
}
impl Aligned {
    fn new(len: usize, mask: u16) -> Self {
        let bytes = Zeroizing::new(vec![0; len + usize::from(mask)]);
        let start = (bytes.as_ptr() as usize).wrapping_neg() & usize::from(mask);
        Self { bytes, start, len }
    }
    fn data(&mut self) -> &mut [u8] {
        &mut self.bytes[self.start..self.start + self.len]
    }
    fn clear(&mut self, used: usize) {
        wipe(&mut self.data()[..used]);
    }
}
struct SessionFd {
    fd: OwnedFd,
    id: Option<u32>,
    call: Call,
}
impl SessionFd {
    fn request<T>(&self, request: libc::c_ulong, data: &mut T) -> io::Result<()> {
        (self.call)(&self.fd, request, (data as *mut T).cast())
    }
}
impl Drop for SessionFd {
    fn drop(&mut self) {
        if let Some(mut id) = self.id {
            let _ = self.request(END_SESSION, &mut id);
        }
    }
}
struct Devcrypto {
    session: SessionFd,
    buffer: Aligned,
    aad: Aligned,
    iv: Aligned,
    _lease: Lease,
}
fn driver_matches(info: &SessionInfo) -> bool {
    fn name(bytes: &[u8]) -> &[u8] {
        &bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())]
    }
    name(&info.cipher.name) == b"gcm(aes)" && name(&info.cipher.driver) == ALG.as_bytes()
}
impl Devcrypto {
    fn open(key: &[u8]) -> io::Result<Self> {
        Self::open_with(key, c"/dev/crypto", ioctl)
    }
    fn open_with(key: &[u8], path: &std::ffi::CStr, call: Call) -> io::Result<Self> {
        let lease = Lease::acquire()?;
        // SAFETY: static NUL-terminated pathname; no mode argument needed without O_CREAT.
        let fd = owned(unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) })?;
        let mut session = SessionFd { fd, id: None, call };
        let mut op = SessionOp {
            cipher: 50,
            key_len: key.len() as u32,
            key: key.as_ptr(),
            ..Default::default()
        };
        session.request(GET_SESSION, &mut op)?;
        session.id = Some(op.session);
        // SAFETY: SessionInfo contains only integer/byte fields; all-zero is valid.
        let mut info: SessionInfo = unsafe { zeroed() };
        info.session = op.session;
        session.request(GET_INFO, &mut info)?;
        if !driver_matches(&info) {
            return Err(io::Error::other(
                "cryptodev did not select the EIP93 GCM driver",
            ));
        }
        let mask = info.align_mask;
        if u32::from(mask) & (u32::from(mask) + 1) != 0 {
            return Err(io::Error::other("invalid cryptodev alignmask"));
        }
        Ok(Self {
            session,
            buffer: Aligned::new(MAX + TAG, mask),
            aad: Aligned::new(256, mask),
            iv: Aligned::new(16, mask),
            _lease: lease,
        })
    }
}
impl Transport for Devcrypto {
    fn exchange(
        &mut self,
        nonce: &[u8; 12],
        src: &[u8],
        aad: &[u8],
        decrypt: bool,
        dst: &mut [u8],
    ) -> io::Result<usize> {
        let expected = output_len(src, aad, decrypt, dst)?;
        self.buffer.data()[..src.len()].copy_from_slice(src);
        self.aad.data()[..aad.len()].copy_from_slice(aad);
        self.iv.data()[..12].copy_from_slice(nonce);
        let mut req = AuthOp {
            session: self.session.id.unwrap(),
            op: u16::from(decrypt),
            len: src.len() as u32,
            auth_len: aad.len() as u32,
            auth_src: self.aad.data().as_ptr(),
            src: self.buffer.data().as_ptr(),
            dst: self.buffer.data().as_mut_ptr(),
            tag_len: TAG as u32,
            iv: self.iv.data().as_ptr(),
            iv_len: 12,
            ..Default::default()
        };
        let result = self.session.request(AUTH_CRYPT, &mut req).and_then(|_| {
            if req.len as usize != expected {
                return Err(io::Error::other("short cryptodev output"));
            }
            dst[..expected].copy_from_slice(&self.buffer.data()[..expected]);
            Ok(expected)
        });
        self.buffer.clear(src.len().max(expected));
        self.aad.clear(aad.len());
        self.iv.clear(12);
        result
    }
}
struct Afalg {
    fd: OwnedFd,
    input: Zeroizing<Vec<u8>>,
    output: Zeroizing<Vec<u8>>,
    control: [usize; 16],
    _lease: Lease,
}
impl Afalg {
    fn open(key: &[u8], algorithm: &str) -> io::Result<Self> {
        let lease = Lease::acquire()?;
        if algorithm.len() >= 64 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        // SAFETY: all syscall argument buffers below have their UAPI sizes and lifetimes.
        unsafe {
            let tfm = owned(libc::socket(
                libc::AF_ALG,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
            ))?;
            let mut addr: libc::sockaddr_alg = zeroed();
            addr.salg_family = libc::AF_ALG as u16;
            addr.salg_type[..4].copy_from_slice(b"aead");
            addr.salg_name[..algorithm.len()].copy_from_slice(algorithm.as_bytes());
            cvt(libc::bind(
                tfm.as_raw_fd(),
                &addr as *const _ as *const libc::sockaddr,
                size_of::<libc::sockaddr_alg>() as libc::socklen_t,
            ))?;
            cvt(libc::setsockopt(
                tfm.as_raw_fd(),
                SOL_ALG,
                1,
                key.as_ptr().cast(),
                key.len() as libc::socklen_t,
            ))?;
            // ALG_SET_AEAD_AUTHSIZE uses optlen, not optval, for the tag size.
            cvt(libc::setsockopt(
                tfm.as_raw_fd(),
                SOL_ALG,
                5,
                ptr::null(),
                TAG as libc::socklen_t,
            ))?;
            let fd = owned(libc::accept4(
                tfm.as_raw_fd(),
                ptr::null_mut(),
                ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            ))?;
            let timeout = libc::timeval {
                tv_sec: 1,
                tv_usec: 0,
            };
            for option in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
                cvt(libc::setsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    option,
                    &timeout as *const _ as *const libc::c_void,
                    size_of::<libc::timeval>() as libc::socklen_t,
                ))?;
            }
            Ok(Self {
                fd,
                input: Zeroizing::new(vec![0; MAX + TAG + 256]),
                output: Zeroizing::new(vec![0; MAX + TAG + 256]),
                control: [0; 16],
                _lease: lease,
            })
        }
    }
}
// Word-aligned control storage is required by CMSG_FIRSTHDR/CMSG_NXTHDR.
fn cmsg(storage: &mut [usize], used: &mut usize, kind: i32, data: &[u8]) {
    // SAFETY: callers provide enough word-aligned storage for all control messages.
    unsafe {
        let space = libc::CMSG_SPACE(data.len() as u32) as usize;
        assert!(*used + space <= std::mem::size_of_val(storage));
        let header = storage
            .as_mut_ptr()
            .cast::<u8>()
            .add(*used)
            .cast::<libc::cmsghdr>();
        (*header).cmsg_level = SOL_ALG;
        (*header).cmsg_type = kind;
        // These length fields are socklen_t on musl and size_t on glibc.
        (*header).cmsg_len = libc::CMSG_LEN(data.len() as u32) as _;
        ptr::copy_nonoverlapping(data.as_ptr(), libc::CMSG_DATA(header), data.len());
        *used += space;
    }
}
impl Transport for Afalg {
    fn exchange(
        &mut self,
        nonce: &[u8; 12],
        src: &[u8],
        aad: &[u8],
        decrypt: bool,
        dst: &mut [u8],
    ) -> io::Result<usize> {
        let expected = output_len(src, aad, decrypt, dst)?;
        let input = &mut self.input[..aad.len() + src.len()];
        input[..aad.len()].copy_from_slice(aad);
        input[aad.len()..].copy_from_slice(src);
        let output = &mut self.output[..aad.len() + expected];
        let result = (|| {
            let control = &mut self.control;
            let mut used = 0;
            cmsg(control, &mut used, 3, &u32::from(!decrypt).to_ne_bytes());
            let mut iv = [0u8; 16];
            iv[..4].copy_from_slice(&12u32.to_ne_bytes());
            iv[4..].copy_from_slice(nonce);
            cmsg(control, &mut used, 2, &iv);
            cmsg(control, &mut used, 4, &(aad.len() as u32).to_ne_bytes());
            // SAFETY: msghdr/iovec point at live, sized buffers until the synchronous syscalls return.
            unsafe {
                let mut iov = libc::iovec {
                    iov_base: input.as_mut_ptr().cast(),
                    iov_len: input.len(),
                };
                let mut msg: libc::msghdr = zeroed();
                msg.msg_iov = &mut iov;
                msg.msg_iovlen = 1;
                msg.msg_control = control.as_mut_ptr().cast();
                msg.msg_controllen = used as _;
                let sent = libc::sendmsg(self.fd.as_raw_fd(), &msg, 0);
                if sent < 0 {
                    return Err(io::Error::last_os_error());
                }
                if sent as usize != input.len() {
                    return Err(io::Error::other("short AF_ALG send"));
                }
                let read = libc::read(
                    self.fd.as_raw_fd(),
                    output.as_mut_ptr().cast(),
                    output.len(),
                );
                if read < 0 {
                    return Err(io::Error::last_os_error());
                }
                if read as usize != output.len() {
                    return Err(io::Error::other("short AF_ALG read"));
                }
            }
            dst[..expected].copy_from_slice(&output[aad.len()..]);
            Ok(expected)
        })();
        wipe(input);
        wipe(output);
        result
    }
}

// Benchmark-only explicit driver override, for kernel transport validation on a host.
pub(super) fn open_afalg(key: &[u8], driver: &str) -> io::Result<Box<dyn Transport>> {
    Ok(Box::new(Afalg::open(key, driver)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model(_: &OwnedFd, request: libc::c_ulong, data: *mut libc::c_void) -> io::Result<()> {
        if request == END_SESSION {
            return Ok(());
        }
        assert_eq!(request, AUTH_CRYPT);
        // SAFETY: test invokes this only with an AuthOp pointing at live model buffers.
        unsafe {
            let op = &mut *data.cast::<AuthOp>();
            assert_eq!(op.src, op.dst);
            assert_eq!(op.iv_len, 12);
            assert_eq!(op.tag_len, 16);
            assert_eq!(op.dst as usize & 15, 0);
            let n = if op.op == 0 { op.len + 16 } else { op.len - 16 };
            ptr::write_bytes(op.dst, 0x42, n as usize);
            op.len = n;
            if op.session == 2 {
                return Err(io::Error::from_raw_os_error(libc::EBADMSG));
            }
            if op.session == 3 {
                op.len -= 1;
            }
        }
        Ok(())
    }
    #[test]
    fn devcrypto_clears_used_region_and_keeps_failed_output_private() {
        for id in [1, 2, 3] {
            let fd: OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
            let mut t = Devcrypto {
                session: SessionFd {
                    fd,
                    id: Some(id),
                    call: model,
                },
                buffer: Aligned::new(MAX + TAG, 15),
                aad: Aligned::new(256, 15),
                iv: Aligned::new(16, 15),
                _lease: Lease::acquire().unwrap(),
            };
            t.buffer.data().fill(0xd3);
            let mut out = [0xa5; 128];
            let result = t.exchange(&[1; 12], &[2; 64], &[3; 5], false, &mut out);
            if id == 1 {
                assert_eq!(result.unwrap(), 80);
                assert_eq!(&out[..80], &[0x42; 80]);
                assert_eq!(&out[80..], &[0xa5; 48]);
            } else {
                assert!(result.is_err());
                assert_eq!(out, [0xa5; 128]);
            }
            assert_eq!(&t.buffer.data()[..80], &[0; 80]);
            assert_eq!(t.buffer.data()[80], 0xd3, "must clear only the used region");
            assert_eq!(&t.aad.data()[..5], &[0; 5]);
            assert_eq!(&t.iv.data()[..12], &[0; 12]);
        }
    }
}
