// Copyright 2026 The PonWrt Authors. SPDX-License-Identifier: MIT
package main

import (
	"bytes"
	"crypto/rand"
	"encoding/binary"
	"encoding/csv"
	"errors"
	"flag"
	"fmt"
	"math"
	"os"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unsafe"
)

var sink uint64

type options struct {
	seconds     float64
	repeats     int
	sizes, keys []int
	backends    []string
	driver, csv string
	cpu         int
}

func parseOptions() (options, error) {
	o := options{}
	sizes := flag.String("sizes", "64,256,512,1024,1500,2048,4096,8192,16384,32768", "comma-separated payload bytes, 1..32768")
	keys := flag.String("key-bits", "all", "128, 256, all")
	backend := flag.String("backend", "all", "software, devcrypto, afalg, all")
	flag.Float64Var(&o.seconds, "seconds", 0.5, "measured seconds per sample, 0.01..60")
	flag.IntVar(&o.repeats, "repeats", 3, "samples per case, 1..99; summary uses medians")
	flag.StringVar(&o.driver, "afalg-driver", eipDriver, "explicit AF_ALG driver; override is diagnostic only")
	flag.StringVar(&o.csv, "csv", "", "raw CSV output path")
	flag.IntVar(&o.cpu, "cpu", -1, "pin the worker thread to logical CPU N")
	flag.Usage = func() {
		fmt.Fprintln(flag.CommandLine.Output(), "eip93-bench 0.2.0 go: AES-GCM software vs /dev/crypto vs AF_ALG\nUsage: eip93-bench-go-aarch64 [OPTIONS]")
		flag.PrintDefaults()
		fmt.Fprintln(flag.CommandLine.Output(), "\nSingle worker. Reused private + caller buffers; copies and used-region clearing included.\nKey/session setup, correctness checks and 25ms warmup excluded.\n12-byte nonce, 16-byte tag, 5-byte TLS 1.3-style AAD; no backend fallback.\nproc%=one CPU; machine%=all-CPU /proc/stat busy, includes other workloads.\nDefault full run is about 3 minutes. Go runtime/GC CPU is included in process CPU.")
	}
	flag.Parse()
	if flag.NArg() != 0 {
		return o, errors.New("unexpected positional arguments")
	}
	if math.IsNaN(o.seconds) || math.IsInf(o.seconds, 0) || o.seconds < 0.01 || o.seconds > 60 || o.repeats < 1 || o.repeats > 99 || o.cpu < -1 {
		return o, errors.New("invalid seconds, repeats or CPU")
	}
	seen := map[int]bool{}
	for _, s := range strings.Split(*sizes, ",") {
		n, err := strconv.Atoi(s)
		if err != nil || n < 1 || n > maxPayload {
			return o, errors.New("sizes must be 1..32768")
		}
		if !seen[n] {
			o.sizes = append(o.sizes, n)
			seen[n] = true
		}
	}
	switch *keys {
	case "128":
		o.keys = []int{128}
	case "256":
		o.keys = []int{256}
	case "all":
		o.keys = []int{128, 256}
	default:
		return o, errors.New("invalid key-bits")
	}
	switch *backend {
	case "all":
		o.backends = []string{"software", "devcrypto", "afalg"}
	case "software", "devcrypto", "afalg":
		o.backends = []string{*backend}
	default:
		return o, errors.New("invalid backend")
	}
	if len(o.driver) == 0 || len(o.driver) >= 64 || strings.ContainsRune(o.driver, 0) {
		return o, errors.New("invalid AF_ALG driver")
	}
	return o, nil
}

type cpuTime struct{ user, sys float64 }

func cpu() (cpuTime, error) {
	var r syscall.Rusage
	err := syscall.Getrusage(syscall.RUSAGE_SELF, &r)
	return cpuTime{float64(r.Utime.Sec) + float64(r.Utime.Usec)/1e6, float64(r.Stime.Sec) + float64(r.Stime.Usec)/1e6}, err
}

type machineTime struct {
	busy, total uint64
	ok          bool
}

func machine() machineTime {
	b, err := os.ReadFile("/proc/stat")
	if err != nil {
		return machineTime{}
	}
	first := strings.SplitN(string(b), "\n", 2)[0]
	v := strings.Fields(first)
	if len(v) < 5 {
		return machineTime{}
	}
	var total, idle uint64
	for i := 1; i < len(v) && i <= 8; i++ {
		n, e := strconv.ParseUint(v[i], 10, 64)
		if e != nil {
			return machineTime{}
		}
		total += n
		if i == 4 || i == 5 {
			idle += n
		}
	}
	return machineTime{total - idle, total, true}
}
func machinePct(b, a machineTime) float64 {
	if !b.ok || !a.ok || a.total <= b.total || a.busy < b.busy {
		return math.NaN()
	}
	return 100 * float64(a.busy-b.busy) / float64(a.total-b.total)
}
func irq() (uint64, bool) {
	b, err := os.ReadFile("/proc/interrupts")
	if err != nil {
		return 0, false
	}
	var total uint64
	found := false
	for _, l := range strings.Split(string(b), "\n") {
		if !strings.Contains(l, "eip93") && !strings.Contains(l, "1fb70000.crypto") {
			continue
		}
		_, tail, ok := strings.Cut(l, ":")
		if !ok {
			continue
		}
		found = true
		for _, s := range strings.Fields(tail) {
			v, e := strconv.ParseUint(s, 10, 64)
			if e != nil {
				break
			}
			total += v
		}
	}
	return total, found
}
func pin(n int) error {
	if n < 0 || n >= 1024 {
		return syscall.EINVAL
	}
	var set [16]uint64
	set[n/64] |= uint64(1) << uint(n%64)
	_, _, e := syscall.RawSyscall(syscall.SYS_SCHED_SETAFFINITY, 0, unsafe.Sizeof(set), uintptr(unsafe.Pointer(&set[0])))
	runtime.KeepAlive(&set)
	if e != 0 {
		return e
	}
	return nil
}
func makeAAD(n int) [5]byte { l := n + tagSize; return [5]byte{23, 3, 3, byte(l >> 8), byte(l)} }
func verify(t transport, key, plain, aad []byte) ([]byte, error) {
	var nonce [12]byte
	sw, err := newSoftware(key)
	if err != nil {
		return nil, err
	}
	defer sw.close()
	expected := make([]byte, len(plain)+tagSize)
	n, err := sw.exchange(&nonce, plain, aad, false, expected)
	if err != nil {
		return nil, err
	}
	ciphertext := make([]byte, n)
	m, err := t.exchange(&nonce, plain, aad, false, ciphertext)
	if err != nil {
		return nil, err
	}
	if m != n || !bytes.Equal(ciphertext, expected) {
		return nil, errors.New("ciphertext/tag differs from software")
	}
	decoded := make([]byte, len(plain)+tagSize)
	m, err = t.exchange(&nonce, ciphertext, aad, true, decoded)
	if err != nil {
		return nil, err
	}
	if m != len(plain) || !bytes.Equal(decoded[:m], plain) {
		return nil, errors.New("plaintext mismatch")
	}
	bad := append([]byte(nil), ciphertext...)
	bad[len(bad)-1] ^= 1
	for i := range decoded {
		decoded[i] = 0xa5
	}
	_, err = t.exchange(&nonce, bad, aad, true, decoded)
	if !errors.Is(err, syscall.EBADMSG) {
		return nil, fmt.Errorf("bad-tag not rejected with EBADMSG: %v", err)
	}
	for _, b := range decoded {
		if b != 0xa5 {
			return nil, errors.New("unauthenticated output exposed")
		}
	}
	m, err = t.exchange(&nonce, ciphertext, aad, true, decoded)
	if err != nil {
		return nil, err
	}
	if m != len(plain) || !bytes.Equal(decoded[:m], plain) {
		return nil, errors.New("session broken after bad-tag")
	}
	return ciphertext, nil
}

type sample struct {
	backend                  string
	bits, bytes              int
	decrypt                  bool
	rep                      int
	ops                      uint64
	wall, user, sys, machine float64
	interrupts               uint64
	irqOK                    bool
}

func (s sample) mib() float64   { return float64(s.ops) * float64(s.bytes) / 1048576 }
func (s sample) speed() float64 { return s.mib() / s.wall }
func (s sample) proc() float64  { return 100 * (s.user + s.sys) / s.wall }
func (s sample) cost() float64  { return 1000 * (s.user + s.sys) / s.mib() }
func (s sample) direction() string {
	if s.decrypt {
		return "dec"
	}
	return "enc"
}
func measure(o options, name string, bits, n int, decrypt bool, rep int) (sample, error) {
	key := make([]byte, bits/8)
	if _, err := rand.Read(key); err != nil {
		return sample{}, err
	}
	defer wipe(key)
	plain := make([]byte, n)
	for i := range plain {
		plain[i] = byte(i*31 ^ (i >> 8))
	}
	aad := makeAAD(n)
	t, err := openTransport(name, key, o.driver)
	if err != nil {
		return sample{}, err
	}
	defer t.close()
	ciphertext, err := verify(t, key, plain, aad[:])
	if err != nil {
		return sample{}, err
	}
	input := plain
	if decrypt {
		input = ciphertext
	}
	output := make([]byte, n+tagSize)
	expected := n + tagSize
	if decrypt {
		expected = n
	}
	var nonce [12]byte
	var seq, checksum uint64
	op := func() error {
		if !decrypt {
			seq++
			binary.BigEndian.PutUint64(nonce[4:], seq)
		}
		m, e := t.exchange(&nonce, input, aad[:], decrypt, output)
		if e != nil {
			return e
		}
		if m != expected {
			return errors.New("output length mismatch")
		}
		checksum ^= uint64(output[0]) ^ uint64(output[m-1])
		sink = checksum
		wipe(output[:m])
		return nil
	}
	warm := time.Now()
	for time.Since(warm) < 25*time.Millisecond {
		if err = op(); err != nil {
			return sample{}, err
		}
	}
	bm := machine()
	bi, bok := irq()
	bc, err := cpu()
	if err != nil {
		return sample{}, err
	}
	start := time.Now()
	var ops uint64
	duration := time.Duration(o.seconds * 1e9)
	for time.Since(start) < duration {
		for i := 0; i < 8; i++ {
			if err = op(); err != nil {
				return sample{}, err
			}
			ops++
		}
	}
	wall := time.Since(start).Seconds()
	ac, err := cpu()
	if err != nil {
		return sample{}, err
	}
	ai, aok := irq()
	am := machine()
	return sample{name, bits, n, decrypt, rep, ops, wall, ac.user - bc.user, ac.sys - bc.sys, machinePct(bm, am), ai - bi, bok && aok && ai >= bi}, nil
}
func median(v []float64) float64 {
	if len(v) == 0 {
		return math.NaN()
	}
	sort.Float64s(v)
	n := len(v)
	if n%2 == 1 {
		return v[n/2]
	}
	return (v[n/2-1] + v[n/2]) / 2
}
func f(v float64) string { return strconv.FormatFloat(v, 'f', 6, 64) }
func writeCSV(s sample, driver string, w *csv.Writer) error {
	if s.backend == "software" {
		driver = "userspace:go-stdlib"
	} else if s.backend == "devcrypto" {
		driver = eipDriver
	}
	interrupts := ""
	if s.irqOK {
		interrupts = strconv.FormatUint(s.interrupts, 10)
	}
	err := w.Write([]string{"go", "0.2.0", s.backend, driver, strconv.Itoa(s.bits), strconv.Itoa(s.bytes), s.direction(), strconv.Itoa(s.rep), strconv.FormatUint(s.ops, 10), f(s.wall), f(s.user), f(s.sys), f(s.speed()), f(float64(s.ops) / s.wall), f(s.proc()), f(s.machine), f(s.cost()), interrupts})
	w.Flush()
	if err != nil {
		return err
	}
	return w.Error()
}
func run(o options) error {
	if runtime.GOOS != "linux" || (runtime.GOARCH != "arm64" && runtime.GOARCH != "amd64") {
		return errors.New("requires little-endian 64-bit Linux")
	}
	runtime.GOMAXPROCS(1)
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	if o.cpu >= 0 {
		if err := pin(o.cpu); err != nil {
			return err
		}
	}
	fmt.Printf("eip93-bench 0.2.0 go | %s | %s | %d online CPUs | single worker, GOMAXPROCS=1\n", runtime.Version(), runtime.GOARCH, runtime.NumCPU())
	fmt.Printf("AF_ALG driver: %s\n/dev/crypto requires: %s (verified via CIOCGSESSINFO)\n", o.driver, eipDriver)
	if o.driver != eipDriver {
		fmt.Println("DIAGNOSTIC OVERRIDE: AF_ALG results are NOT proof of EIP93 acceleration.")
	}
	if b, err := os.ReadFile("/proc/cpuinfo"); err == nil {
		for _, l := range strings.Split(string(b), "\n") {
			if strings.HasPrefix(l, "Features") || strings.HasPrefix(l, "flags") {
				fmt.Println(l)
				break
			}
		}
	}
	fmt.Printf("%.3fs/sample, %d repeats; medians below. No backend fallback.\nReused private + caller buffers; copies and used-region clearing INCLUDED.\nproc%%=one-core CPU; machine%%=all-core busy including other workloads.\nBelow 4096 B hardware is deliberately forced to measure crossover.\n", o.seconds, o.repeats)
	var output *csv.Writer
	if o.csv != "" {
		file, err := os.Create(o.csv)
		if err != nil {
			return err
		}
		defer file.Close()
		output = csv.NewWriter(file)
		if err = output.Write(strings.Split("implementation,version,backend,driver,key_bits,bytes,direction,repeat,operations,wall_s,user_s,sys_s,MiB_s,ops_s,process_cpu_pct,machine_busy_pct,process_cpu_ms_per_MiB,eip93_irq_delta", ",")); err != nil {
			return err
		}
		output.Flush()
		if err = output.Error(); err != nil {
			return err
		}
	}
	var available []string
	failures := 0
	for _, name := range o.backends {
		key := bytes.Repeat([]byte{7}, 16)
		t, err := openTransport(name, key, o.driver)
		if err != nil {
			fmt.Fprintf(os.Stderr, "UNAVAILABLE %s: %v\n", name, err)
			failures++
		} else {
			available = append(available, name)
			t.close()
		}
	}
	if len(available) == 0 {
		return errors.New("no requested backend available")
	}
	total := o.repeats * len(o.keys) * len(o.sizes) * 2 * len(available)
	done := 0
	samples := make([]sample, 0, total)
	for rep := 1; rep <= o.repeats; rep++ {
		for _, bits := range o.keys {
			for _, n := range o.sizes {
				for _, decrypt := range []bool{false, true} {
					d := 0
					if decrypt {
						d = 1
					}
					offset := (rep + n + d) % len(available)
					for i := range available {
						name := available[(i+offset)%len(available)]
						done++
						dir := "enc"
						if decrypt {
							dir = "dec"
						}
						fmt.Fprintf(os.Stderr, "\r[%d/%d] %-9s AES-%d %5d %s rep %d     ", done, total, name, bits, n, dir, rep)
						s, err := measure(o, name, bits, n, decrypt, rep)
						if err != nil {
							fmt.Fprintf(os.Stderr, "\nFAILED %s AES-%d %dB: %v\n", name, bits, n, err)
							failures++
							continue
						}
						if output != nil {
							if err = writeCSV(s, o.driver, output); err != nil {
								return err
							}
						}
						samples = append(samples, s)
					}
				}
			}
		}
	}
	fmt.Fprintln(os.Stderr)
	fmt.Println("\n AES  bytes op backend    n    MiB/s     ops/s    us/op   user%    sys%   proc% machine% cpu-ms/MiB   speedup  IRQ-delta*")
	for _, bits := range o.keys {
		for _, n := range o.sizes {
			for _, decrypt := range []bool{false, true} {
				var base []float64
				for _, s := range samples {
					if s.bits == bits && s.bytes == n && s.decrypt == decrypt && s.backend == "software" {
						base = append(base, s.speed())
					}
				}
				baseline := median(base)
				for _, name := range available {
					var g []sample
					for _, s := range samples {
						if s.bits == bits && s.bytes == n && s.decrypt == decrypt && s.backend == name {
							g = append(g, s)
						}
					}
					if len(g) == 0 {
						continue
					}
					m := func(fn func(sample) float64) float64 {
						v := make([]float64, len(g))
						for i, s := range g {
							v[i] = fn(s)
						}
						return median(v)
					}
					interrupts := "n/a"
					all := true
					for _, s := range g {
						all = all && s.irqOK
					}
					if all {
						interrupts = fmt.Sprintf("%.0f", m(func(s sample) float64 { return float64(s.interrupts) }))
					}
					fmt.Printf("%4d %6d %s %-9s %2d %8.2f %9.0f %8.2f %7.1f %7.1f %7.1f %8.1f %10.3f %8.2fx %10s\n", bits, n, g[0].direction(), name, len(g), m(sample.speed), m(func(s sample) float64 { return float64(s.ops) / s.wall }), m(func(s sample) float64 { return 1e6 * s.wall / float64(s.ops) }), m(func(s sample) float64 { return 100 * s.user / s.wall }), m(func(s sample) float64 { return 100 * s.sys / s.wall }), m(sample.proc), m(func(s sample) float64 { return s.machine }), m(sample.cost), m(sample.speed)/baseline, interrupts)
				}
			}
		}
	}
	fmt.Printf("\nEach column is the median of successful samples; n should equal repeats.\nIRQ-delta*: median EIP93 interrupts per sample, n/a if unavailable.\nNaN machine%%=no usable /proc/stat; NaN speedup=no software baseline.\nFailures/unavailable: %d. No measurements from software fallback.\n", failures)
	if failures > 0 {
		return errors.New("partial results above; requested cases unavailable/failed")
	}
	return nil
}
func main() {
	o, err := parseOptions()
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
	if err = run(o); err != nil {
		fmt.Fprintln(os.Stderr, "eip93-bench:", err)
		os.Exit(1)
	}
}
