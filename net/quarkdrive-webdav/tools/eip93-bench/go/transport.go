// Copyright 2026 The PonWrt Authors. SPDX-License-Identifier: BSD-3-Clause
// cryptodev UAPI and AF_ALG transport, matching the Rust benchmark's data flow.
// Derived from the Xray private overlay, see ../LICENSE.xray.
package main

import (
	"crypto/aes"
	"crypto/cipher"
	"errors"
	"fmt"
	"runtime"
	"syscall"
	"unsafe"
)

const maxPayload = 32768
const tagSize = 16
const eipDriver = "gcm_base(ctr(aes-eip93),ghash-generic)"

type transport interface {
	exchange(nonce *[12]byte, src, aad []byte, decrypt bool, dst []byte) (int, error)
	close()
}

//go:noinline
func wipe(b []byte) { clear(b); runtime.KeepAlive(b) }

func outputLen(src, aad []byte, decrypt bool, dst []byte) (int, error) {
	n := len(src)
	if decrypt {
		n -= tagSize
	}
	expected := n + tagSize
	if decrypt {
		expected = n
	}
	if n < 0 || n > maxPayload || len(aad) > 256 || len(dst) < expected {
		return 0, syscall.EINVAL
	}
	return expected, nil
}

type software struct {
	aead   cipher.AEAD
	buffer []byte
}

func newSoftware(key []byte) (*software, error) {
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	a, err := cipher.NewGCM(block)
	if err != nil {
		return nil, err
	}
	return &software{a, make([]byte, maxPayload+tagSize)}, nil
}
func (s *software) exchange(nonce *[12]byte, src, aad []byte, decrypt bool, dst []byte) (int, error) {
	expected, err := outputLen(src, aad, decrypt, dst)
	if err != nil {
		return 0, err
	}
	used := max(len(src), expected)
	copy(s.buffer, src)
	var out []byte
	if decrypt {
		out, err = s.aead.Open(s.buffer[:0], nonce[:], s.buffer[:len(src)], aad)
		if err != nil {
			wipe(s.buffer[:used])
			return 0, syscall.EBADMSG
		}
	} else {
		out = s.aead.Seal(s.buffer[:0], nonce[:], s.buffer[:len(src)], aad)
	}
	if len(out) != expected {
		wipe(s.buffer[:used])
		return 0, errors.New("short software output")
	}
	copy(dst, out)
	wipe(s.buffer[:used])
	return expected, nil
}
func (s *software) close() { wipe(s.buffer) }

// Linux cryptodev 1.14 little-endian 64-bit ABI.
type sessionOp struct {
	Cipher, Mac, KeyLen uint32
	Key                 *byte
	MacKeyLen           uint32
	MacKey              *byte
	Session             uint32
}
type algorithmInfo struct{ Name, Driver [64]byte }
type sessionInfo struct {
	Session      uint32
	Cipher, Hash algorithmInfo
	AlignMask    uint16
	Flags        uint32
}
type authOp struct {
	Session                uint32
	Op, Flags              uint16
	Len, AuthLen           uint32
	AuthSrc, Src, Dst, Tag *byte
	TagLen                 uint32
	IV                     *byte
	IVLen                  uint32
}

const getSession = 0xc0306366
const endSession = 0x40046367
const getInfo = 0xc10c636b
const authCrypt = 0xc048636d

func ioctl(fd int, request uintptr, arg unsafe.Pointer) error {
	_, _, errno := syscall.Syscall(syscall.SYS_IOCTL, uintptr(fd), request, uintptr(arg))
	runtime.KeepAlive(arg)
	if errno != 0 {
		return errno
	}
	return nil
}
func aligned(n int, mask uint16) []byte {
	b := make([]byte, n+int(mask))
	start := int(-uintptr(unsafe.Pointer(&b[0])) & uintptr(mask))
	return b[start : start+n]
}
func cString(b []byte) string {
	for i, v := range b {
		if v == 0 {
			return string(b[:i])
		}
	}
	return string(b)
}

type devcrypto struct {
	fd              int
	id              uint32
	live            bool
	buffer, aad, iv []byte
	req             authOp
	call            func(int, uintptr, unsafe.Pointer) error
}

func newDevcrypto(key []byte) (*devcrypto, error) {
	fd, err := syscall.Open("/dev/crypto", syscall.O_RDWR|syscall.O_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	d := &devcrypto{fd: fd, call: ioctl}
	op := sessionOp{Cipher: 50, KeyLen: uint32(len(key)), Key: &key[0]}
	if err = d.call(fd, getSession, unsafe.Pointer(&op)); err != nil {
		d.close()
		return nil, err
	}
	runtime.KeepAlive(key)
	d.id = op.Session
	d.live = true
	info := sessionInfo{Session: d.id}
	if err = d.call(fd, getInfo, unsafe.Pointer(&info)); err != nil {
		d.close()
		return nil, err
	}
	if cString(info.Cipher.Name[:]) != "gcm(aes)" || cString(info.Cipher.Driver[:]) != eipDriver {
		d.close()
		return nil, fmt.Errorf("cryptodev selected non-EIP93 GCM driver: %s", cString(info.Cipher.Driver[:]))
	}
	mask := info.AlignMask
	if uint32(mask)&(uint32(mask)+1) != 0 {
		d.close()
		return nil, errors.New("invalid cryptodev alignmask")
	}
	d.buffer = aligned(maxPayload+tagSize, mask)
	d.aad = aligned(256, mask)
	d.iv = aligned(16, mask)
	return d, nil
}
func (d *devcrypto) exchange(nonce *[12]byte, src, aad []byte, decrypt bool, dst []byte) (int, error) {
	expected, err := outputLen(src, aad, decrypt, dst)
	if err != nil {
		return 0, err
	}
	copy(d.buffer, src)
	copy(d.aad, aad)
	copy(d.iv, nonce[:])
	op := uint16(0)
	if decrypt {
		op = 1
	}
	d.req = authOp{Session: d.id, Op: op, Len: uint32(len(src)), AuthLen: uint32(len(aad)), AuthSrc: &d.aad[0], Src: &d.buffer[0], Dst: &d.buffer[0], TagLen: 16, IV: &d.iv[0], IVLen: 12}
	err = d.call(d.fd, authCrypt, unsafe.Pointer(&d.req))
	runtime.KeepAlive(d)
	if err == nil && int(d.req.Len) != expected {
		err = errors.New("short cryptodev output")
	}
	if err == nil {
		copy(dst, d.buffer[:expected])
	}
	wipe(d.buffer[:max(len(src), expected)])
	wipe(d.aad[:len(aad)])
	wipe(d.iv[:12])
	if err != nil {
		return 0, err
	}
	return expected, nil
}
func (d *devcrypto) close() {
	wipe(d.buffer)
	wipe(d.aad)
	wipe(d.iv)
	if d.live {
		_ = d.call(d.fd, endSession, unsafe.Pointer(&d.id))
		d.live = false
	}
	if d.fd >= 0 {
		_ = syscall.Close(d.fd)
		d.fd = -1
	}
}

type sockaddrAlg struct {
	Family        uint16
	Type          [14]byte
	Feature, Mask uint32
	Name          [64]byte
}
type afalg struct {
	fd            int
	input, output []byte
	control       [16]uint64
	iov           syscall.Iovec
	msg           syscall.Msghdr
}

func setSockOpt(fd, option int, ptr unsafe.Pointer, n int) error {
	_, _, err := syscall.Syscall6(syscall.SYS_SETSOCKOPT, uintptr(fd), 279, uintptr(option), uintptr(ptr), uintptr(n), 0)
	runtime.KeepAlive(ptr)
	if err != 0 {
		return err
	}
	return nil
}
func newAFALG(key []byte, driver string) (*afalg, error) {
	fd, err := syscall.Socket(38, syscall.SOCK_SEQPACKET|syscall.SOCK_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	defer syscall.Close(fd)
	addr := sockaddrAlg{Family: 38}
	copy(addr.Type[:], "aead")
	copy(addr.Name[:], driver)
	_, _, errno := syscall.Syscall(syscall.SYS_BIND, uintptr(fd), uintptr(unsafe.Pointer(&addr)), unsafe.Sizeof(addr))
	runtime.KeepAlive(&addr)
	if errno != 0 {
		return nil, errno
	}
	if err = setSockOpt(fd, 1, unsafe.Pointer(&key[0]), len(key)); err != nil {
		return nil, err
	}
	if err = setSockOpt(fd, 5, nil, tagSize); err != nil {
		return nil, err
	}
	op, _, errno := syscall.Syscall6(syscall.SYS_ACCEPT4, uintptr(fd), 0, 0, syscall.SOCK_CLOEXEC, 0, 0)
	if errno != 0 {
		return nil, errno
	}
	a := &afalg{fd: int(op), input: make([]byte, maxPayload+tagSize+256), output: make([]byte, maxPayload+tagSize+256)}
	timeout := syscall.Timeval{Sec: 1}
	for _, opt := range []int{syscall.SO_SNDTIMEO, syscall.SO_RCVTIMEO} {
		if err = syscall.SetsockoptTimeval(a.fd, syscall.SOL_SOCKET, opt, &timeout); err != nil {
			a.close()
			return nil, err
		}
	}
	return a, nil
}
func (a *afalg) exchange(nonce *[12]byte, src, aad []byte, decrypt bool, dst []byte) (int, error) {
	expected, err := outputLen(src, aad, decrypt, dst)
	if err != nil {
		return 0, err
	}
	input := a.input[:len(aad)+len(src)]
	output := a.output[:len(aad)+expected]
	copy(input, aad)
	copy(input[len(aad):], src)
	// Fixed little-endian 64-bit CMSG layout: 24 + 32 + 24 = 80 bytes.
	ctl := unsafe.Slice((*byte)(unsafe.Pointer(&a.control[0])), 128)
	*(*syscall.Cmsghdr)(unsafe.Pointer(&ctl[0])) = syscall.Cmsghdr{Len: 20, Level: 279, Type: 3}
	op := uint32(1)
	if decrypt {
		op = 0
	}
	*(*uint32)(unsafe.Pointer(&ctl[16])) = op
	*(*syscall.Cmsghdr)(unsafe.Pointer(&ctl[24])) = syscall.Cmsghdr{Len: 32, Level: 279, Type: 2}
	*(*uint32)(unsafe.Pointer(&ctl[40])) = 12
	copy(ctl[44:56], nonce[:])
	*(*syscall.Cmsghdr)(unsafe.Pointer(&ctl[56])) = syscall.Cmsghdr{Len: 20, Level: 279, Type: 4}
	*(*uint32)(unsafe.Pointer(&ctl[72])) = uint32(len(aad))
	a.iov = syscall.Iovec{Base: &input[0], Len: uint64(len(input))}
	a.msg = syscall.Msghdr{Iov: &a.iov, Iovlen: 1, Control: &ctl[0], Controllen: 80}
	n, _, errno := syscall.Syscall(syscall.SYS_SENDMSG, uintptr(a.fd), uintptr(unsafe.Pointer(&a.msg)), 0)
	if errno != 0 {
		err = errno
	} else if int(n) != len(input) {
		err = errors.New("short AF_ALG send")
	}
	if err == nil {
		n, _, errno = syscall.Syscall(syscall.SYS_READ, uintptr(a.fd), uintptr(unsafe.Pointer(&output[0])), uintptr(len(output)))
		if errno != 0 {
			err = errno
		} else if int(n) != len(output) {
			err = errors.New("short AF_ALG read")
		}
	}
	runtime.KeepAlive(a)
	if err == nil {
		copy(dst, output[len(aad):])
	}
	wipe(input)
	wipe(output)
	if err != nil {
		return 0, err
	}
	return expected, nil
}
func (a *afalg) close() {
	wipe(a.input)
	wipe(a.output)
	if a.fd >= 0 {
		_ = syscall.Close(a.fd)
		a.fd = -1
	}
}
func openTransport(name string, key []byte, driver string) (transport, error) {
	switch name {
	case "software":
		return newSoftware(key)
	case "devcrypto":
		return newDevcrypto(key)
	case "afalg":
		return newAFALG(key, driver)
	}
	return nil, syscall.EINVAL
}
