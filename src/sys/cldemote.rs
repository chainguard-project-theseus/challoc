//! The `cldemote` cache-line hint (design 17):
//! probed, reported, tested both ways, and issued on no path.
//!
//! `cldemote m8` asks the core to move the line holding `m8` out of its own
//! L1/L2 towards the shared level (L3), so the next core to touch the line
//! finds it there instead of paying a cross-core transfer. It is a hint: it
//! never faults, never changes the line's contents and may be ignored.
//! It was tried on the lines a freeing thread writes last and another
//! thread may read next, the class bitmap word a release clears and the
//! slab descriptor whose `live` count it lowers (`class/`, `release_word`
//! and `dec_live`), and it did not pay: the freeing core is usually the
//! next claimer too, so the demotion turns its L1 hit into an L3 hit
//! (xmalloc-test +5% cycles per operation, the single-threaded Valkey
//! model +13..17%, the `io-threads 4` Valkey soak within noise;
//! `bench/results/2026-09-19-x86_64-lane-b-task12-cldemote.md`). The call
//! sites were dropped; the probe (`cpuid` leaf 7 subleaf 0 `ECX[25]`,
//! `sys::cpu`, `X86::cldemote`, `cldemote=yes|no` in the feature report),
//! this module's [`demote`] and its both-ways test stay, so a future
//! cross-thread-only path (one that knows the claimer is another core,
//! which the release path does not) can be measured by adding one call.
//!
//! [`configure`] installs the probe's verdict at init (`api::init`, from
//! `X86::cldemote` of the CPU report; the probe itself is pure);
//! without the bit, on another architecture, or before init, [`demote`] is
//! a predicted-not-taken branch on one byte. Nothing observable changes
//! between hosts.

use core::sync::atomic::{AtomicBool, Ordering};

/// Whether the hint is issued: the probe's verdict, `false` until init.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Install the probe's verdict. `present` is `X86::cldemote`; on any other
/// architecture the hint stays off whatever is passed.
pub fn configure(present: bool) {
    ENABLED.store(present && cfg!(target_arch = "x86_64"), Ordering::Relaxed);
}

/// Whether [`demote`] issues the instruction.
#[inline(always)]
pub fn enabled() -> bool {
    cfg!(target_arch = "x86_64") && ENABLED.load(Ordering::Relaxed)
}

/// Demote the cache line holding `p` when the CPU has `cldemote`; nothing
/// otherwise. `p` need not be valid to dereference (the instruction takes
/// any address and faults on none). Called by the test alone today (module
/// doc); a caller on a hot path must come with the attribution CLAUDE.md
/// asks for.
#[inline(always)]
pub fn demote(p: *const u8) {
    if enabled() {
        // SAFETY: `cldemote` is a hint that reads and writes no memory and
        // raises no fault for any address; `readonly` keeps it after the
        // caller's writes to the line.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("cldemote [{p}]", p = in(reg) p, options(nostack, readonly, preserves_flags));
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = p;
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use std::eprintln;

    /// Both ways: with the probe's bit the instruction runs on a line just
    /// written and leaves it intact; without it (`configure(false)`, or on
    /// arm64 whatever is passed) nothing is issued. The probe itself is
    /// checked against `/proc/cpuinfo` in `sys::cpu::linux_tests`.
    #[test]
    fn demote_follows_the_probe_and_never_changes_memory() {
        let page = crate::sys::features::page_size();
        let cpu = crate::sys::cpu::Cpu::probe(page);
        let present = matches!(cpu.isa, crate::sys::cpu::Isa::X86(x) if x.cldemote);
        let mut buf = [0u8; 256];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = i as u8;
        }
        configure(present);
        assert_eq!(enabled(), present && cfg!(target_arch = "x86_64"));
        for off in [0usize, 63, 64, 130, 255] {
            demote(buf.as_ptr().wrapping_add(off));
        }
        // An address nothing maps: the hint must not fault either.
        demote(core::ptr::null());
        demote(usize::MAX as *const u8);
        for (i, b) in buf.iter().enumerate() {
            assert_eq!(*b, i as u8);
        }
        configure(false);
        assert!(!enabled());
        demote(buf.as_ptr());
        configure(true);
        assert_eq!(enabled(), cfg!(target_arch = "x86_64"));
        // Leave the process-wide verdict as the probe found it.
        configure(present);
        assert_eq!(enabled(), present && cfg!(target_arch = "x86_64"));
        eprintln!("cldemote: present={present} enabled={}", enabled());
    }
}
