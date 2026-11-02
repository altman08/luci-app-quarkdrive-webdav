// Copyright 2026 The PonWrt Authors. SPDX-License-Identifier: MIT
package main

import (
	"bytes"
	"encoding/hex"
	"errors"
	"os"
	"syscall"
	"testing"
	"unsafe"
)

func TestKnownAnswers(t *testing.T) {
	for _, v := range []struct {
		bits       int
		ciphertext string
	}{
		{128, "0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf"},
		{256, "cea7403d4d606b6e074ec5d3baf39d18d0d1c8a799996bf0265b98b5d48ab919"},
	} {
		sw, e := newSoftware(make([]byte, v.bits/8))
		if e != nil {
			t.Fatal(e)
		}
		want, e := hex.DecodeString(v.ciphertext)
		if e != nil {
			t.Fatal(e)
		}
		var nonce [12]byte
		out := make([]byte, 32)
		n, e := sw.exchange(&nonce, make([]byte, 16), nil, false, out)
		if e != nil || n != 32 || !bytes.Equal(out, want) {
			t.Fatalf("AES-%d known answer: %x %v", v.bits, out, e)
		}
		n, e = sw.exchange(&nonce, want, nil, true, out)
		if e != nil || n != 16 || !bytes.Equal(out[:n], make([]byte, 16)) {
			t.Fatal("known answer decrypt", e)
		}
		sw.close()
	}
}
func TestReuseAndAuthentication(t *testing.T) {
	for _, bits := range []int{128, 256} {
		key := bytes.Repeat([]byte{7}, bits/8)
		sw, e := newSoftware(key)
		if e != nil {
			t.Fatal(e)
		}
		for _, n := range []int{maxPayload, 1, 4096, 64, 16384} {
			plain := bytes.Repeat([]byte{23}, n)
			aad := makeAAD(n)
			if _, e = verify(sw, key, plain, aad[:]); e != nil {
				t.Fatal(e)
			}
			for _, b := range sw.buffer {
				if b != 0 {
					t.Fatal("private buffer not cleared")
				}
			}
		}
		sw.close()
	}
}
func TestUAPI(t *testing.T) {
	if unsafe.Sizeof(sessionOp{}) != 48 || unsafe.Sizeof(sessionInfo{}) != 268 || unsafe.Sizeof(authOp{}) != 72 || unsafe.Sizeof(syscall.Cmsghdr{}) != 16 || unsafe.Sizeof(sockaddrAlg{}) != 88 {
		t.Fatal("UAPI size mismatch")
	}
	if unsafe.Offsetof(sessionOp{}.Key) != 16 || unsafe.Offsetof(sessionOp{}.Session) != 40 || unsafe.Offsetof(authOp{}.Src) != 24 || unsafe.Offsetof(authOp{}.IV) != 56 || unsafe.Offsetof(sessionInfo{}.Flags) != 264 {
		t.Fatal("UAPI offset mismatch")
	}
}
func TestDevcryptoFailurePrivacyAndUsedClear(t *testing.T) {
	for _, mode := range []int{1, 2, 3} {
		d := &devcrypto{fd: -1, id: uint32(mode), buffer: aligned(maxPayload+tagSize, 15), aad: aligned(256, 15), iv: aligned(16, 15)}
		d.call = func(_ int, request uintptr, p unsafe.Pointer) error {
			if request != authCrypt {
				t.Fatal("unexpected ioctl")
			}
			op := (*authOp)(p)
			if op.Src != op.Dst || op.IVLen != 12 || op.TagLen != 16 || uintptr(unsafe.Pointer(op.Dst))&15 != 0 {
				t.Fatal("bad ioctl framing")
			}
			n := int(op.Len) + 16
			if op.Op == 1 {
				n = int(op.Len) - 16
			}
			b := unsafe.Slice(op.Dst, n)
			for i := range b {
				b[i] = 0x42
			}
			op.Len = uint32(n)
			if mode == 2 {
				return syscall.EBADMSG
			}
			if mode == 3 {
				op.Len--
			}
			return nil
		}
		for i := range d.buffer {
			d.buffer[i] = 0xd3
		}
		out := bytes.Repeat([]byte{0xa5}, 128)
		var nonce [12]byte
		n, e := d.exchange(&nonce, make([]byte, 64), make([]byte, 5), false, out)
		if mode == 1 {
			if e != nil || n != 80 || !bytes.Equal(out[:80], bytes.Repeat([]byte{0x42}, 80)) {
				t.Fatal(n, e)
			}
		} else {
			if e == nil || !bytes.Equal(out, bytes.Repeat([]byte{0xa5}, 128)) {
				t.Fatal("failed output exposed")
			}
			if mode == 2 && !errors.Is(e, syscall.EBADMSG) {
				t.Fatal(e)
			}
		}
		if !bytes.Equal(d.buffer[:80], make([]byte, 80)) || d.buffer[80] != 0xd3 {
			t.Fatal("incorrect used-region clearing")
		}
		d.close()
	}
}
func TestAFALGReuseAndNoAllocations(t *testing.T) {
	if os.Getenv("EIP93_TEST_AFALG") != "1" {
		t.Skip("set EIP93_TEST_AFALG=1 on generic AF_ALG development host")
	}
	key := bytes.Repeat([]byte{7}, 32)
	a, e := newAFALG(key, "gcm_base(ctr(aes-generic),ghash-generic)")
	if e != nil {
		t.Fatal(e)
	}
	defer a.close()
	for _, n := range []int{maxPayload, 1, 4096, 64, 16384} {
		plain := bytes.Repeat([]byte{23}, n)
		aad := makeAAD(n)
		if _, e = verify(a, key, plain, aad[:]); e != nil {
			t.Fatal(e)
		}
		output := make([]byte, n+tagSize)
		var nonce [12]byte
		allocations := testing.AllocsPerRun(100, func() {
			if _, err := a.exchange(&nonce, plain, aad[:], false, output); err != nil {
				panic(err)
			}
			wipe(output)
		})
		if allocations != 0 {
			t.Fatalf("hot-path allocations: %f", allocations)
		}
		for _, b := range a.input {
			if b != 0 {
				t.Fatal("input not cleared")
			}
		}
		for _, b := range a.output {
			if b != 0 {
				t.Fatal("output not cleared")
			}
		}
	}
}
func TestCPUUnits(t *testing.T) {
	if machinePct(machineTime{10, 100, true}, machineTime{30, 200, true}) != 20 {
		t.Fatal("machine percentage")
	}
	s := sample{bytes: 1048576, ops: 2, wall: 2, user: 0.4, sys: 0.1}
	if s.speed() != 1 || s.proc() != 25 || s.cost() != 250 {
		t.Fatal("CPU units")
	}
}
