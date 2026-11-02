// Copyright 2026 The PonWrt Authors. SPDX-License-Identifier: MIT
// Standalone transport benchmark, intentionally bypassing the WebDAV device/size gates.
mod linux;

#[cfg(not(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little"
)))]
compile_error!("eip93-bench requires little-endian 64-bit Linux");

use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use std::{
    hint::black_box,
    io::{self, Write},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

const MAX: usize = 32768;
const TAG: usize = 16;
const DRIVER: &str = "gcm_base(ctr(aes-eip93),ghash-generic)";
#[derive(Clone, Copy, PartialEq)]
enum Backend {
    Devcrypto,
    Afalg,
}
// Destination is caller-owned; authenticate before copying any output to it.
trait Transport: Send + Sync {
    fn exchange(
        &mut self,
        nonce: &[u8; 12],
        src: &[u8],
        aad: &[u8],
        decrypt: bool,
        dst: &mut [u8],
    ) -> io::Result<usize>;
}
// Both languages use an out-of-line optimized clear on the used region.
// The black_box keeps this benchmark's memory writes observable to the optimizer.
#[inline(never)]
fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    black_box(bytes);
}
struct Software {
    key: aead::LessSafeKey,
    buffer: Zeroizing<Vec<u8>>,
}
impl Software {
    fn new(key: &[u8]) -> io::Result<Self> {
        let alg = match key.len() {
            16 => &aead::AES_128_GCM,
            32 => &aead::AES_256_GCM,
            _ => return Err(io::ErrorKind::InvalidInput.into()),
        };
        Ok(Self {
            key: aead::LessSafeKey::new(
                aead::UnboundKey::new(alg, key).map_err(|_| io::Error::other("ring key"))?,
            ),
            buffer: Zeroizing::new(vec![0; MAX + TAG]),
        })
    }
}
impl Transport for Software {
    fn exchange(
        &mut self,
        nonce: &[u8; 12],
        src: &[u8],
        aad: &[u8],
        decrypt: bool,
        dst: &mut [u8],
    ) -> io::Result<usize> {
        let expected = output_len(src, aad, decrypt, dst)?;
        let used = src.len().max(expected);
        self.buffer[..src.len()].copy_from_slice(src);
        let result = (|| {
            let nonce = aead::Nonce::assume_unique_for_key(*nonce);
            let aad = aead::Aad::from(aad);
            if decrypt {
                self.key
                    .open_in_place(nonce, aad, &mut self.buffer[..src.len()])
                    .map_err(|_| io::Error::from_raw_os_error(libc::EBADMSG))?;
            } else {
                let tag = self
                    .key
                    .seal_in_place_separate_tag(nonce, aad, &mut self.buffer[..src.len()])
                    .map_err(|_| io::Error::other("ring encrypt"))?;
                self.buffer[src.len()..expected].copy_from_slice(tag.as_ref());
            }
            dst[..expected].copy_from_slice(&self.buffer[..expected]);
            Ok(expected)
        })();
        wipe(&mut self.buffer[..used]);
        result
    }
}
fn output_len(src: &[u8], aad: &[u8], decrypt: bool, dst: &[u8]) -> io::Result<usize> {
    let n = if decrypt {
        src.len().checked_sub(TAG)
    } else {
        Some(src.len())
    }
    .ok_or(io::ErrorKind::InvalidInput)?;
    let expected = if decrypt { n } else { n + TAG };
    if n > MAX || aad.len() > 256 || dst.len() < expected {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(expected)
}
fn open(name: &str, key: &[u8], driver: &str) -> io::Result<Box<dyn Transport>> {
    match name {
        "software" => Ok(Box::new(Software::new(key)?)),
        "devcrypto" => linux::open(Backend::Devcrypto, key),
        "afalg" if driver == DRIVER => linux::open(Backend::Afalg, key),
        "afalg" => linux::open_afalg(key, driver),
        _ => Err(io::ErrorKind::InvalidInput.into()),
    }
}

struct Options {
    seconds: f64,
    repeats: usize,
    sizes: Vec<usize>,
    keys: Vec<usize>,
    backends: Vec<String>,
    driver: String,
    csv: Option<String>,
    cpu: Option<usize>,
}
fn help() {
    println!(
        "eip93-bench 0.2.0 rust: AES-GCM userspace vs /dev/crypto vs AF_ALG\n\
Usage: eip93-bench [OPTIONS]\n\
  --seconds N        Measured seconds per sample (default 0.5)\n\
  --repeats N        Samples per case (default 3; summary uses medians)\n\
  --sizes LIST       Payload bytes (default 64,256,512,1024,1500,2048,4096,8192,16384,32768)\n\
  --key-bits VALUE   128, 256, or all (default all)\n\
  --backend VALUE    all, software, devcrypto, or afalg (default all)\n\
  --csv PATH         Write raw samples to CSV, flushing after every sample\n\
  --cpu N            Pin the benchmark task to logical CPU N (optional)\n\
  --afalg-driver STR Explicit AF_ALG override for diagnostics; default is exact EIP93 driver\n\
  -h, --help         Show help\n\n\
Single worker; no software fallback for kernel backends. Key/session setup and\n\
correctness checks are excluded. Payload copies, used-region clearing and syscalls\n\
are included; buffers are reused without per-record transport allocations. 12-byte nonce, 16-byte tag, 5-byte TLS 1.3-style AAD.\n\
Software uses ring, including CPU AES instructions where available.\n\
proc% = (user+sys)/wall, 100% = one CPU. machine% = all-CPU /proc/stat busy\n\
including IRQ/softIRQ and other workloads; 100% = entire machine.\n\
cpu-ms/MiB uses process CPU only. IRQ delta can include other EIP93 users.\n\
Default full run is about 3 minutes if all three backends are available."
    );
}
fn options() -> Result<Option<Options>, String> {
    let mut o = Options {
        seconds: 0.5,
        repeats: 3,
        sizes: vec![64, 256, 512, 1024, 1500, 2048, 4096, 8192, 16384, 32768],
        keys: vec![128, 256],
        backends: vec!["software".into(), "devcrypto".into(), "afalg".into()],
        driver: DRIVER.into(),
        csv: None,
        cpu: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            help();
            return Ok(None);
        }
        let v = args
            .next()
            .ok_or_else(|| format!("missing value for {arg}"))?;
        match arg.as_str() {
            "--seconds" => o.seconds = v.parse().map_err(|_| "invalid seconds")?,
            "--repeats" => o.repeats = v.parse().map_err(|_| "invalid repeats")?,
            "--sizes" => {
                o.sizes = v
                    .split(',')
                    .map(|s| s.parse().map_err(|_| "invalid size".to_string()))
                    .collect::<Result<_, _>>()?
            }
            "--key-bits" => {
                o.keys = match v.as_str() {
                    "128" => vec![128],
                    "256" => vec![256],
                    "all" => vec![128, 256],
                    _ => return Err("key-bits must be 128, 256, all".into()),
                }
            }
            "--backend" => {
                o.backends = match v.as_str() {
                    "all" => vec!["software".into(), "devcrypto".into(), "afalg".into()],
                    "software" | "devcrypto" | "afalg" => vec![v],
                    _ => return Err("unknown backend".into()),
                }
            }
            "--afalg-driver" => o.driver = v,
            "--csv" => o.csv = Some(v),
            "--cpu" => o.cpu = Some(v.parse().map_err(|_| "invalid CPU")?),
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    if !o.seconds.is_finite()
        || !(0.01..=60.0).contains(&o.seconds)
        || !(1..=99).contains(&o.repeats)
        || o.sizes.is_empty()
        || o.sizes.iter().any(|n| !(1..=MAX).contains(n))
    {
        return Err("seconds must be 0.01..60, repeats 1..99, sizes 1..32768".into());
    }
    if o.driver.is_empty() || o.driver.len() >= 64 || o.driver.as_bytes().contains(&0) {
        return Err("invalid AF_ALG driver".into());
    }
    let mut seen = std::collections::HashSet::new();
    o.sizes.retain(|n| seen.insert(*n));
    Ok(Some(o))
}

#[derive(Clone, Copy)]
struct Cpu {
    user: f64,
    sys: f64,
}
fn cpu() -> io::Result<Cpu> {
    // SAFETY: zeroed rusage is valid; getrusage writes its complete structure.
    unsafe {
        let mut r: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut r) < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Cpu {
            user: r.ru_utime.tv_sec as f64 + r.ru_utime.tv_usec as f64 / 1e6,
            sys: r.ru_stime.tv_sec as f64 + r.ru_stime.tv_usec as f64 / 1e6,
        })
    }
}
#[derive(Clone, Copy)]
struct Machine {
    busy: u64,
    total: u64,
}
fn machine() -> Option<Machine> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let values: Vec<u64> = text
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if values.len() < 4 {
        return None;
    }
    // First eight fields: exclude guest/guest_nice (already counted in user/nice).
    let total = values.iter().sum::<u64>();
    Some(Machine {
        total,
        busy: total - values[3] - values.get(4).copied().unwrap_or(0),
    })
}
fn machine_pct(before: Option<Machine>, after: Option<Machine>) -> f64 {
    match (before, after) {
        (Some(b), Some(a)) if a.total > b.total && a.busy >= b.busy => {
            100.0 * (a.busy - b.busy) as f64 / (a.total - b.total) as f64
        }
        _ => f64::NAN,
    }
}
fn irq() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/interrupts").ok()?;
    let mut found = false;
    let mut total = 0;
    for line in text
        .lines()
        .filter(|l| l.contains("eip93") || l.contains("1fb70000.crypto"))
    {
        let (_, v) = line.split_once(':')?;
        for count in v.split_whitespace().map_while(|s| s.parse::<u64>().ok()) {
            total += count;
        }
        found = true;
    }
    found.then_some(total)
}
fn pin(n: usize) -> io::Result<()> {
    if n >= libc::CPU_SETSIZE as usize {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // SAFETY: CPU index checked against cpu_set_t capacity.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(n, &mut set);
        if libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn verify(t: &mut dyn Transport, key: &[u8], plain: &[u8], aad: &[u8]) -> io::Result<Vec<u8>> {
    let nonce = [0; 12];
    let mut expected = vec![0; plain.len() + TAG];
    let expected_len = Software::new(key)?.exchange(&nonce, plain, aad, false, &mut expected)?;
    let mut ciphertext = vec![0; expected_len];
    if t.exchange(&nonce, plain, aad, false, &mut ciphertext)? != expected_len
        || ciphertext != expected
    {
        return Err(io::Error::other("ciphertext/tag differs from software"));
    }
    let mut decoded = vec![0; plain.len() + TAG];
    let n = t.exchange(&nonce, &ciphertext, aad, true, &mut decoded)?;
    if n != plain.len() || &decoded[..n] != plain {
        return Err(io::Error::other("plaintext mismatch"));
    }
    let mut bad = ciphertext.clone();
    *bad.last_mut().unwrap() ^= 1;
    decoded.fill(0xa5);
    match t.exchange(&nonce, &bad, aad, true, &mut decoded) {
        Err(e) if e.raw_os_error() == Some(libc::EBADMSG) => (),
        Err(e) => return Err(io::Error::other(format!("bad-tag unexpected error: {e}"))),
        Ok(_) => return Err(io::Error::other("backend ACCEPTED invalid tag")),
    }
    if decoded.iter().any(|b| *b != 0xa5) {
        return Err(io::Error::other("unauthenticated output exposed"));
    }
    let n = t.exchange(&nonce, &ciphertext, aad, true, &mut decoded)?;
    if n != plain.len() || &decoded[..n] != plain {
        return Err(io::Error::other("session broken after bad-tag"));
    }
    Ok(ciphertext)
}
fn aad(n: usize) -> [u8; 5] {
    let len = (n + TAG) as u16;
    [23, 3, 3, (len >> 8) as u8, len as u8]
}
#[derive(Clone)]
struct Sample {
    backend: String,
    bits: usize,
    bytes: usize,
    decrypt: bool,
    rep: usize,
    ops: u64,
    wall: f64,
    user: f64,
    sys: f64,
    machine: f64,
    interrupts: Option<u64>,
}
impl Sample {
    fn mib(&self) -> f64 {
        self.ops as f64 * self.bytes as f64 / 1048576.0
    }
    fn speed(&self) -> f64 {
        self.mib() / self.wall
    }
    fn proc(&self) -> f64 {
        100.0 * (self.user + self.sys) / self.wall
    }
    fn cpu_cost(&self) -> f64 {
        1000.0 * (self.user + self.sys) / self.mib()
    }
    fn direction(&self) -> &'static str {
        if self.decrypt { "dec" } else { "enc" }
    }
}
fn measure(
    o: &Options,
    name: &str,
    bits: usize,
    bytes: usize,
    decrypt: bool,
    rep: usize,
) -> io::Result<Sample> {
    let mut key = Zeroizing::new(vec![0; bits / 8]);
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| io::Error::other("random key"))?;
    let plain: Vec<u8> = (0..bytes)
        .map(|n| (n.wrapping_mul(31) ^ (n >> 8)) as u8)
        .collect();
    let aad = aad(bytes);
    let mut transport = open(name, &key, &o.driver)?;
    let ciphertext = verify(&mut *transport, &key, &plain, &aad)?;
    let input = if decrypt {
        ciphertext.as_slice()
    } else {
        plain.as_slice()
    };
    let mut nonce = [0u8; 12];
    let mut seq = 0u64;
    let mut output = vec![0; bytes + TAG];
    let expected = if decrypt { bytes } else { bytes + TAG };
    let mut checksum = 0u64;
    let mut op = || -> io::Result<()> {
        if !decrypt {
            seq += 1;
            nonce[4..].copy_from_slice(&seq.to_be_bytes());
        }
        let n = transport.exchange(&nonce, input, &aad, decrypt, &mut output)?;
        if n != expected {
            return Err(io::Error::other("output length mismatch"));
        }
        checksum ^= u64::from(output[0]) ^ u64::from(output[n - 1]);
        black_box(checksum);
        wipe(&mut output[..n]);
        Ok(())
    };
    let warm = Instant::now();
    while warm.elapsed() < Duration::from_millis(25) {
        op()?;
    }
    let before_machine = machine();
    let before_irq = irq();
    let before_cpu = cpu()?;
    let start = Instant::now();
    let mut ops = 0;
    // Clock check every eight records limits measurement overhead for small blocks.
    let duration = Duration::from_secs_f64(o.seconds);
    while start.elapsed() < duration {
        for _ in 0..8 {
            op()?;
            ops += 1;
        }
    }
    let wall = start.elapsed().as_secs_f64();
    let after_cpu = cpu()?;
    let after_irq = irq();
    let after_machine = machine();
    Ok(Sample {
        backend: name.into(),
        bits,
        bytes,
        decrypt,
        rep,
        ops,
        wall,
        user: after_cpu.user - before_cpu.user,
        sys: after_cpu.sys - before_cpu.sys,
        machine: machine_pct(before_machine, after_machine),
        interrupts: before_irq
            .zip(after_irq)
            .and_then(|(b, a)| a.checked_sub(b)),
    })
}
fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    if v.len() % 2 == 1 {
        v[v.len() / 2]
    } else {
        (v[v.len() / 2 - 1] + v[v.len() / 2]) / 2.0
    }
}
fn csv(s: &Sample, driver: &str, out: &mut dyn Write) -> io::Result<()> {
    let driver = match s.backend.as_str() {
        "software" => "userspace:ring",
        "devcrypto" => DRIVER,
        _ => driver,
    };
    writeln!(
        out,
        "rust,0.2.0,{},\"{}\",{},{},{},{},{},{:.6},{:.6},{:.6},{:.4},{:.4},{:.4},{:.4},{:.4},{}",
        s.backend,
        driver.replace('"', "\"\""),
        s.bits,
        s.bytes,
        s.direction(),
        s.rep,
        s.ops,
        s.wall,
        s.user,
        s.sys,
        s.speed(),
        s.ops as f64 / s.wall,
        s.proc(),
        s.machine,
        s.cpu_cost(),
        s.interrupts.map(|v| v.to_string()).unwrap_or_default()
    )?;
    out.flush()
}
fn run(o: Options) -> io::Result<()> {
    if let Some(n) = o.cpu {
        pin(n)?;
    }
    // SAFETY: sysconf with this constant requires no pointer arguments.
    let online = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    println!(
        "eip93-bench 0.2.0 rust | {} | {} online CPUs | single worker",
        std::env::consts::ARCH,
        online
    );
    println!("AF_ALG driver: {}", o.driver);
    println!("/dev/crypto requires: {DRIVER} (verified via CIOCGSESSINFO)");
    println!("Reused private + caller buffers; copies and used-region clearing INCLUDED.");
    if o.driver != DRIVER {
        println!("DIAGNOSTIC OVERRIDE: AF_ALG results are NOT proof of EIP93 acceleration.");
    }
    let cpus = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    if let Some(features) = cpus
        .lines()
        .find(|l| l.starts_with("Features") || l.starts_with("flags"))
    {
        println!("{features}");
    }
    println!(
        "{:.3}s/sample, {} repeats; medians below. No backend fallback. Press Ctrl-C to stop.\n\
proc%=one-core CPU; machine%=all-core busy, includes other workloads.\n\
Below 4096 B hardware is deliberately forced to measure crossover (WebDAV normally uses software).",
        o.seconds, o.repeats
    );
    io::stdout().flush()?;
    let mut output: Option<std::fs::File> =
        o.csv.as_ref().map(std::fs::File::create).transpose()?;
    if let Some(f) = &mut output {
        writeln!(
            f,
            "implementation,version,backend,driver,key_bits,bytes,direction,repeat,operations,wall_s,user_s,sys_s,MiB_s,ops_s,process_cpu_pct,machine_busy_pct,process_cpu_ms_per_MiB,eip93_irq_delta"
        )?;
        f.flush()?;
    }
    let mut available = Vec::new();
    let mut failures = 0;
    for name in &o.backends {
        match open(name, &[7; 16], &o.driver) {
            Ok(_) => available.push(name.clone()),
            Err(e) => {
                eprintln!("UNAVAILABLE {name}: {e}");
                failures += 1;
            }
        }
    }
    if available.is_empty() {
        return Err(io::Error::other("no requested backend available"));
    }
    let mut samples = Vec::new();
    let total = o.repeats * o.keys.len() * o.sizes.len() * 2 * available.len();
    let mut done = 0;
    for rep in 1..=o.repeats {
        for &bits in &o.keys {
            for &bytes in &o.sizes {
                for decrypt in [false, true] {
                    // Rotate order per case/repeat to reduce systematic thermal/order bias.
                    let offset = (rep + bytes + usize::from(decrypt)) % available.len();
                    for i in 0..available.len() {
                        let name = &available[(i + offset) % available.len()];
                        done += 1;
                        eprint!(
                            "\r[{done}/{total}] {name:9} AES-{bits} {:5} {} rep {rep}     ",
                            bytes,
                            if decrypt { "dec" } else { "enc" }
                        );
                        match measure(&o, name, bits, bytes, decrypt, rep) {
                            Ok(s) => {
                                if let Some(f) = &mut output {
                                    csv(&s, &o.driver, f)?;
                                }
                                samples.push(s);
                            }
                            Err(e) => {
                                eprintln!("\nFAILED {name} AES-{bits} {bytes}B: {e}");
                                failures += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!();
    println!(
        "\n AES  bytes op backend    n    MiB/s     ops/s    us/op   user%    sys%   proc% machine% cpu-ms/MiB   speedup  IRQ-delta*"
    );
    for &bits in &o.keys {
        for &bytes in &o.sizes {
            for decrypt in [false, true] {
                let base: Vec<f64> = samples
                    .iter()
                    .filter(|s| {
                        s.bits == bits
                            && s.bytes == bytes
                            && s.decrypt == decrypt
                            && s.backend == "software"
                    })
                    .map(Sample::speed)
                    .collect();
                let base = if base.is_empty() {
                    f64::NAN
                } else {
                    median(base)
                };
                for name in &available {
                    let group: Vec<&Sample> = samples
                        .iter()
                        .filter(|s| {
                            s.bits == bits
                                && s.bytes == bytes
                                && s.decrypt == decrypt
                                && s.backend == *name
                        })
                        .collect();
                    if group.is_empty() {
                        continue;
                    }
                    let m = |f: fn(&Sample) -> f64| median(group.iter().map(|s| f(s)).collect());
                    let irq = if group.iter().all(|s| s.interrupts.is_some()) {
                        format!("{:.0}", m(|s| s.interrupts.unwrap() as f64))
                    } else {
                        "n/a".into()
                    };
                    println!(
                        "{:4} {:6} {} {:9} {:2} {:8.2} {:9.0} {:8.2} {:7.1} {:7.1} {:7.1} {:8.1} {:10.3} {:8.2}x {:>10}",
                        bits,
                        bytes,
                        if decrypt { "dec" } else { "enc" },
                        name,
                        group.len(),
                        m(Sample::speed),
                        m(|s| s.ops as f64 / s.wall),
                        m(|s| 1e6 * s.wall / s.ops as f64),
                        m(|s| 100.0 * s.user / s.wall),
                        m(|s| 100.0 * s.sys / s.wall),
                        m(Sample::proc),
                        m(|s| s.machine),
                        m(Sample::cpu_cost),
                        m(Sample::speed) / base,
                        irq
                    );
                }
            }
        }
    }
    println!(
        "\nEach column is the median of successful samples; n should equal repeats.\nIRQ-delta*: median EIP93 interrupt count per sample, n/a if not exposed.\nNaN machine%=no usable /proc/stat; NaN speedup=no software baseline.\nFailures/unavailable: {failures}. No measurements from software fallback."
    );
    if failures > 0 {
        return Err(io::Error::other(
            "partial results above; one or more requested cases unavailable/failed",
        ));
    }
    Ok(())
}
fn main() {
    match options() {
        Ok(Some(o)) => {
            if let Err(e) = run(o) {
                eprintln!("eip93-bench: {e}");
                std::process::exit(1);
            }
        }
        Ok(None) => (),
        Err(e) => {
            eprintln!("{e}; use --help");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    struct Counting;
    thread_local! { static ALLOCS: Cell<Option<usize>> = const { Cell::new(None) }; }
    #[global_allocator]
    static ALLOCATOR: Counting = Counting;
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let _ = ALLOCS.try_with(|c| {
                if let Some(n) = c.get() {
                    c.set(Some(n + 1));
                }
            });
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, len: usize) -> *mut u8 {
            let _ = ALLOCS.try_with(|c| {
                if let Some(n) = c.get() {
                    c.set(Some(n + 1));
                }
            });
            unsafe { System.realloc(ptr, layout, len) }
        }
    }
    fn no_allocations(f: impl FnOnce()) {
        ALLOCS.with(|c| c.set(Some(0)));
        f();
        let n = ALLOCS.with(|c| c.replace(None).unwrap());
        assert_eq!(n, 0, "hot path allocated");
    }
    #[test]
    fn nist_aes128_gcm_known_answer() {
        // NIST SP 800-38D, zero 128-bit key/96-bit IV, one zero block, empty AAD.
        let expected = [
            0x03, 0x88, 0xda, 0xce, 0x60, 0xb6, 0xa3, 0x92, 0xf3, 0x28, 0xc2, 0xb9, 0x71, 0xb2,
            0xfe, 0x78, 0xab, 0x6e, 0x47, 0xd4, 0x2c, 0xec, 0x13, 0xbd, 0xf5, 0x3a, 0x67, 0xb2,
            0x12, 0x57, 0xbd, 0xdf,
        ];
        let mut sw = Software::new(&[0; 16]).unwrap();
        let mut out = [0u8; 32];
        assert_eq!(
            sw.exchange(&[0; 12], &[0; 16], &[], false, &mut out)
                .unwrap(),
            32
        );
        assert_eq!(out, expected);
        assert_eq!(
            sw.exchange(&[0; 12], &expected, &[], true, &mut out)
                .unwrap(),
            16
        );
        assert_eq!(&out[..16], &[0; 16]);
    }
    #[test]
    fn reusable_buffers_authentication_and_no_allocations() {
        for bits in [128, 256] {
            let key = vec![7; bits / 8];
            let mut sw = Software::new(&key).unwrap();
            for bytes in [MAX, 1, 4096, 64, 16384] {
                let plain = vec![23; bytes];
                let aad = aad(bytes);
                verify(&mut sw, &key, &plain, &aad).unwrap();
                assert!(sw.buffer.iter().all(|b| *b == 0));
                let mut dst = vec![0; bytes + TAG];
                no_allocations(|| {
                    for i in 1..=100u64 {
                        let mut nonce = [0; 12];
                        nonce[4..].copy_from_slice(&i.to_be_bytes());
                        sw.exchange(&nonce, &plain, &aad, false, &mut dst).unwrap();
                        wipe(&mut dst);
                    }
                });
            }
        }
    }
    #[test]
    #[ignore = "requires generic AF_ALG on the Linux development host"]
    fn afalg_reuse_authentication_and_no_allocations() {
        let key = [7; 32];
        let mut t = linux::open_afalg(&key, "gcm_base(ctr(aes-generic),ghash-generic)").unwrap();
        for bytes in [MAX, 1, 4096, 64, 16384] {
            let plain = vec![23; bytes];
            let aad = aad(bytes);
            verify(&mut *t, &key, &plain, &aad).unwrap();
            let mut out = vec![0; bytes + TAG];
            no_allocations(|| {
                for _ in 0..100 {
                    t.exchange(&[0; 12], &plain, &aad, false, &mut out).unwrap();
                    wipe(&mut out);
                }
            });
        }
    }
    #[test]
    fn cpu_denominators() {
        assert_eq!(
            machine_pct(
                Some(Machine {
                    busy: 10,
                    total: 100
                }),
                Some(Machine {
                    busy: 30,
                    total: 200
                })
            ),
            20.0
        );
        assert!(machine_pct(None, None).is_nan());
        let s = Sample {
            backend: "software".into(),
            bits: 128,
            bytes: 1048576,
            decrypt: false,
            rep: 1,
            ops: 2,
            wall: 2.0,
            user: 0.4,
            sys: 0.1,
            machine: 0.0,
            interrupts: None,
        };
        assert_eq!(s.speed(), 1.0);
        assert_eq!(s.proc(), 25.0);
        assert_eq!(s.cpu_cost(), 250.0);
    }
}
