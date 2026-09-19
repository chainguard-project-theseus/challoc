//! Pointer authentication (design.md sections 6.1, 6.2 and 17; plan Phase
//! 2.5 task 7).
//!
//! On an arm64 host whose kernel exposes `HWCAP_PACA` and `HWCAP_PACG`
//! (Armv8.3 FEAT_PAuth with the keys enabled for user space; every
//! process gets fresh keys at `exec`, shared by all its threads), the
//! allocator keeps two kinds of pointer in memory an attacker's write can
//! reach signed with the data key A: every slot pointer stored in a thread
//! cache's slot array (modifier: the address of the cache header, so an
//! entry copied from another cache fails), and the heap pointer in the TLS
//! slot (modifier: the thread pointer `tpidr_el0`, so a heap pointer copied
//! from another thread fails). `sign` is `pacda`, `auth` is `autda`; both
//! are the identity on x86_64 and the caller never emits them unless
//! [`available`] said yes at init (the instructions are UNDEFINED on a CPU
//! without the extension, so they are gated by a runtime flag, never by a
//! build setting: the compiler baseline stays `armv8-a`).
//!
//! What a failed authentication does depends on the CPU: with FEAT_FPAC
//! (Neoverse V2, Apple M2 and later) `autda` itself faults (the kernel
//! delivers `SIGILL`); on the original PAuth (Neoverse N1, Apple M1) it
//! returns the pointer with an error code in the PAC field, a
//! non-canonical address whose first dereference faults (`SIGSEGV`). Either
//! way a corrupted entry never reaches the program as a usable pointer. The
//! PAC is as wide as the address bits above the VA size leave: 7 bits
//! (54:48) on Linux's default 48-bit user VA with top-byte-ignore on, so a
//! forged entry passes one time in 128 and every failure is a crash.
//!
//! The instructions are emitted through `asm!` with `.arch_extension
//! pauth`, which the integrated assembler applies to that block only.

/// Whether the kernel exposes pointer authentication with the address key
/// (`HWCAP_PACA`) and the generic key (`HWCAP_PACG`) to this process.
/// False on x86_64 and off Linux.
pub fn available() -> bool {
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        unsafe extern "C" {
            /// glibc/musl `getauxval(3)`; reads the saved vector, never
            /// allocates (see `features.rs`).
            safe fn getauxval(kind: usize) -> usize;
        }
        const AT_HWCAP: usize = 16;
        let hwcap = getauxval(AT_HWCAP) as u64;
        hwcap & super::cpu::HWCAP_PACA != 0 && hwcap & super::cpu::HWCAP_PACG != 0
    }
    #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
    {
        false
    }
}

/// Whether the CPU has FEAT_FPAC (a failed `autda` faults at the
/// instruction) rather than the original PAuth (it returns a non-canonical
/// pointer). From `ID_AA64ISAR1_EL1.APA`/`.API` (QARMA5 / implementation
/// defined) and `ID_AA64ISAR2_EL1.APA3` (QARMA3), which the kernel emulates
/// for user space; a field value of 4 or 5 means FPAC. Diagnostic only
/// (the report and the tests); the allocator's behaviour does not depend
/// on it. False where `available` is false.
pub fn fpac() -> bool {
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        if !available() {
            return false;
        }
        let (isar1, isar2): (u64, u64);
        // SAFETY: system-register reads the kernel emulates for EL0; no
        // memory or flag effects.
        unsafe {
            core::arch::asm!(
                "mrs {a}, id_aa64isar1_el1",
                "mrs {b}, s3_0_c0_c6_2", // id_aa64isar2_el1, spelled for older assemblers
                a = out(reg) isar1,
                b = out(reg) isar2,
                options(nomem, nostack, preserves_flags),
            );
        }
        let apa = (isar1 >> 4) & 0xf;
        let api = (isar1 >> 8) & 0xf;
        let apa3 = (isar2 >> 12) & 0xf;
        apa >= 4 || api >= 4 || apa3 >= 4
    }
    #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
    {
        false
    }
}

/// Sign `p` with data key A and `modifier` (`pacda`). Only when
/// [`available`] is true; `p` must be a plain (unsigned) pointer, since
/// signing a signed pointer corrupts its PAC.
#[inline(always)]
pub fn sign(p: usize, modifier: usize) -> usize {
    #[cfg(target_arch = "aarch64")]
    {
        let mut v = p;
        // SAFETY: a register-only instruction with no memory or flag
        // effects; the caller has checked the CPU has the extension.
        unsafe {
            core::arch::asm!(
                ".arch_extension pauth",
                "pacda {v}, {m}",
                v = inout(reg) v,
                m = in(reg) modifier,
                options(pure, nomem, nostack, preserves_flags),
            );
        }
        v
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = modifier;
        p
    }
}

/// Authenticate `p`, signed by [`sign`] with the same `modifier`
/// (`autda`): the plain pointer, or a fault / a non-canonical pointer
/// (module doc) when the two disagree.
#[inline(always)]
pub fn auth(p: usize, modifier: usize) -> usize {
    #[cfg(target_arch = "aarch64")]
    {
        let mut v = p;
        // SAFETY: as in `sign`. Not `pure`: on FEAT_FPAC the instruction
        // faults on a bad pointer, an effect the optimiser must keep.
        unsafe {
            core::arch::asm!(
                ".arch_extension pauth",
                "autda {v}, {m}",
                v = inout(reg) v,
                m = in(reg) modifier,
                options(nomem, nostack, preserves_flags),
            );
        }
        v
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = modifier;
        p
    }
}

/// Strip the PAC from `p` without authenticating it (`xpacd`): the address
/// a signed or corrupted entry names, for abort messages and tests. Only
/// when [`available`] is true.
#[inline]
pub fn strip(p: usize) -> usize {
    #[cfg(target_arch = "aarch64")]
    {
        let mut v = p;
        // SAFETY: as in `sign`.
        unsafe {
            core::arch::asm!(
                ".arch_extension pauth",
                "xpacd {v}",
                v = inout(reg) v,
                options(pure, nomem, nostack, preserves_flags),
            );
        }
        v
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        p
    }
}

/// The bits `sign` writes: the PAC field, above the virtual address bits
/// (54:48 on a 48-bit VA kernel, 54:52 on a 52-bit one; never the top byte,
/// which Linux leaves to top-byte-ignore, never bit 55, the TTBR select
/// whose value `xpacd` extends into the field), found by asking `xpacd`
/// what it clears from an address with bits 54:0 set. `!mask()` is the
/// address key a signed entry sorts by without authentication. Only when
/// [`available`] is true; `0` on x86_64.
#[inline]
pub fn mask() -> usize {
    let probe = 0x007f_ffff_ffff_ffffusize;
    probe & !strip(probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a host with the extension the round trip is exact, a signed
    /// pointer differs from the plain one in the PAC bits only, the
    /// modifier matters, and `strip` recovers the address without a key.
    /// Elsewhere every function is the identity.
    #[test]
    fn sign_auth_round_trip() {
        let p = 0x0000_7f12_3456_7890usize;
        if !available() {
            assert_eq!(sign(p, 1), p);
            assert_eq!(auth(p, 1), p);
            assert_eq!(strip(p), p);
            std::eprintln!("pac: not available on this host, identity checked");
            return;
        }
        let s = sign(p, 0x1000);
        assert_eq!(auth(s, 0x1000), p);
        assert_eq!(s & 0x0000_ffff_ffff_ffff, p, "low 48 bits are the address");
        assert_eq!(strip(s), p);
        let s2 = sign(p, 0x2000);
        assert_eq!(auth(s2, 0x2000), p);
        // The same key and address with different modifiers give different
        // PACs with overwhelming probability; with a 7-bit PAC a collision
        // is 1 in 128, so try a handful of modifiers before calling it wrong.
        let distinct = (1..64usize).any(|m| sign(p, m) != s);
        assert!(distinct, "pacda ignores the modifier");
        // The mask covers every bit any signature touched and no address
        // bit; on a 48-bit VA kernel it is bits 54:48.
        let m = mask();
        assert_eq!(m & 0x0000_ffff_ffff_ffff, 0, "mask {m:#x} reaches into the address");
        assert_eq!(m & (1 << 55), 0, "bit 55 is the TTBR select, never PAC");
        assert_eq!(m >> 56, 0, "the top byte is TBI, never PAC");
        assert_ne!(m, 0);
        for mm in 1..64usize {
            assert_eq!(sign(p, mm) & !m, p, "signature outside the mask");
        }
        std::eprintln!("pac: mask {m:#x} ({} bits)", m.count_ones());
        std::eprintln!("pac: available, fpac={}, {:#x} -> {:#x}", fpac(), p, s);
    }

    /// A mismatched modifier fails: on FEAT_FPAC `autda` itself faults
    /// (`SIGILL`); on the original PAuth the result is non-canonical and its
    /// dereference faults (`SIGSEGV`). Run in a forked child so the test
    /// process survives; skipped where the extension is absent.
    #[cfg(target_os = "linux")]
    #[test]
    fn mismatched_modifier_faults() {
        if !available() {
            std::eprintln!("pac: not available on this host, skipping");
            return;
        }
        let word = std::boxed::Box::new(42u64);
        let p = &*word as *const u64 as usize;
        let s = sign(p, 0x1000);
        // A different modifier: the PAC will not match with probability
        // 127/128 per try; try modifiers until one differs in the signed
        // form, which guarantees the mismatch.
        let bad = (1..1000usize).find(|&m| sign(p, m) != s).expect("a modifier that changes the PAC");
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            let q = auth(s, bad);
            // Non-FPAC: `q` is non-canonical; reading through it faults.
            let v = unsafe { core::ptr::read_volatile(q as *const u64) };
            // Neither path may get here.
            unsafe { libc::_exit(if v == 42 { 3 } else { 4 }) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFSIGNALED(status), "child survived a failed authentication: status {status:#x}");
        let sig = libc::WTERMSIG(status);
        assert!(sig == libc::SIGILL || sig == libc::SIGSEGV || sig == libc::SIGBUS, "signal {sig}");
        if fpac() {
            assert_eq!(sig, libc::SIGILL, "FEAT_FPAC faults at the autda");
        } else {
            assert_eq!(sig, libc::SIGSEGV, "without FPAC the non-canonical pointer faults on use");
        }
        std::eprintln!("pac: mismatched modifier -> signal {sig} (fpac={})", fpac());
    }
}
