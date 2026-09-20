//! Initial-exec TLS accessor (design.md section 6.1).
//!
//! The allocator keeps exactly one pointer-sized thread-local slot: the
//! current thread's `Heap` pointer (null until the heap exists, a sentinel
//! while it is being created). Stable Rust has no `#[thread_local]`, and
//! `thread_local!` needs `std`, so the slot is declared in `.tbss` by
//! `global_asm!` and read and written by two short `asm!` sequences using the
//! initial-exec model: one GOT load of the thread-pointer offset, then one
//! load or store relative to the thread pointer. There is no
//! `__tls_get_addr`, no TLS descriptor, and no call, so the fast paths pay
//! two dependent loads. With pointer authentication on (`sys::pac`) the
//! slot holds the heap pointer signed with the thread pointer as the
//! modifier; `get_tp` returns both so the caller can authenticate it, and
//! the sentinels (small integers) are never signed.
//!
//! Initial-exec relies on glibc having reserved static TLS for the object
//! when the program started, which is the case for both `-lchalloc` and
//! `LD_PRELOAD`. `dlopen` of challoc after start is not a supported
//! configuration; glibc reports a shortage of static TLS as a `dlopen`
//! failure rather than a crash.
//!
//! The symbol is hidden so the linker resolves it inside `libchalloc.so`
//! itself: the only dynamic relocation is the `TPOFF64` / `TLS_TPREL64` fixup
//! of the GOT entry, which the test below checks with `readelf`.

use core::arch::{asm, global_asm};
use core::ffi::c_void;

// One zeroed, pointer-aligned, pointer-sized slot in `.tbss`. Fresh threads
// therefore start with a null slot, which the allocator treats as "no heap
// yet". `@`/`%` differ between the two assemblers' directive syntaxes.
#[cfg(target_arch = "x86_64")]
global_asm!(
    ".section .tbss,\"awT\",@nobits",
    ".p2align 3",
    ".globl challoc_tls",
    ".hidden challoc_tls",
    ".type challoc_tls, @object",
    ".size challoc_tls, 8",
    "challoc_tls:",
    ".zero 8",
    ".text",
);

#[cfg(target_arch = "aarch64")]
global_asm!(
    ".section .tbss,\"awT\",%nobits",
    ".p2align 3",
    ".globl challoc_tls",
    ".hidden challoc_tls",
    ".type challoc_tls, %object",
    ".size challoc_tls, 8",
    "challoc_tls:",
    ".zero 8",
    ".text",
);

/// Read the calling thread's slot. Null until [`set`] has been called on
/// this thread.
#[inline(always)]
pub fn get() -> *mut c_void {
    let out: *mut c_void;
    // SAFETY: reads the calling thread's own `.tbss` slot through the thread
    // pointer; the slot exists for every thread because the object is in the
    // static TLS block.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!(
            "mov {out}, qword ptr [rip + challoc_tls@GOTTPOFF]",
            "mov {out}, qword ptr fs:[{out}]",
            out = out(reg) out,
            options(nostack, readonly, preserves_flags),
        );
        #[cfg(target_arch = "aarch64")]
        asm!(
            "adrp {off}, :gottprel:challoc_tls",
            "ldr {off}, [{off}, :gottprel_lo12:challoc_tls]",
            "mrs {tp}, tpidr_el0",
            "ldr {out}, [{tp}, {off}]",
            off = out(reg) _,
            tp = out(reg) _,
            out = out(reg) out,
            options(nostack, readonly, preserves_flags),
        );
    }
    out
}

/// Read the calling thread's slot together with the thread pointer
/// (`tpidr_el0` on aarch64; `0` on x86_64, where it is unused). The thread
/// pointer is the modifier the heap pointer is signed with when pointer
/// authentication is on (`sys::pac`, design 6.1): the accessor already
/// loads it to reach the slot, so the caller's `autda` costs no extra
/// register move.
#[inline(always)]
pub fn get_tp() -> (*mut c_void, usize) {
    #[cfg(target_arch = "x86_64")]
    {
        (get(), 0)
    }
    #[cfg(target_arch = "aarch64")]
    {
        let out: *mut c_void;
        let tp: usize;
        // SAFETY: as in `get`.
        unsafe {
            asm!(
                "adrp {off}, :gottprel:challoc_tls",
                "ldr {off}, [{off}, :gottprel_lo12:challoc_tls]",
                "mrs {tp}, tpidr_el0",
                "ldr {out}, [{tp}, {off}]",
                off = out(reg) _,
                tp = out(reg) tp,
                out = out(reg) out,
                options(nostack, readonly, preserves_flags),
            );
        }
        (out, tp)
    }
}

/// Write the calling thread's slot.
#[inline(always)]
pub fn set(value: *mut c_void) {
    // SAFETY: writes the calling thread's own `.tbss` slot; see `get`.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!(
            "mov {off}, qword ptr [rip + challoc_tls@GOTTPOFF]",
            "mov qword ptr fs:[{off}], {val}",
            off = out(reg) _,
            val = in(reg) value,
            options(nostack, preserves_flags),
        );
        #[cfg(target_arch = "aarch64")]
        asm!(
            "adrp {off}, :gottprel:challoc_tls",
            "ldr {off}, [{off}, :gottprel_lo12:challoc_tls]",
            "mrs {tp}, tpidr_el0",
            "str {val}, [{tp}, {off}]",
            off = out(reg) _,
            tp = out(reg) _,
            val = in(reg) value,
            options(nostack, preserves_flags),
        );
    }
}

// The slot has no exported accessor: a `challoc_tls_set` export would be
// a write primitive to the heap pointer in a hardened allocator, and the C
// tests that once drove it (`tests/c/tls_threads.c`) observe per-thread
// heaps through `mallctl("thread.allocatedp")` instead. The one
// challoc-specific export is `challoc_version` (`src/api/mod.rs`).

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::string::String;
    use std::{env, format, vec::Vec};

    #[test]
    fn slot_starts_null_and_round_trips() {
        // A fresh thread so no other test on this thread has touched the slot.
        std::thread::spawn(|| {
            assert!(get().is_null());
            let mut x = 0u8;
            let p = &mut x as *mut u8 as *mut c_void;
            set(p);
            assert_eq!(get(), p);
            set(core::ptr::null_mut());
            assert!(get().is_null());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn get_tp_agrees_with_get_and_names_the_thread_pointer() {
        std::thread::spawn(|| {
            let (v, tp) = get_tp();
            assert!(v.is_null());
            let mut x = 0u8;
            let p = &mut x as *mut u8 as *mut c_void;
            set(p);
            let (v2, tp2) = get_tp();
            assert_eq!(v2, p);
            assert_eq!(v2, get());
            assert_eq!(tp, tp2, "the thread pointer is stable within a thread");
            #[cfg(target_arch = "aarch64")]
            {
                let mrs: usize;
                unsafe { asm!("mrs {}, tpidr_el0", out(reg) mrs, options(nomem, nostack, preserves_flags)) };
                assert_eq!(tp, mrs);
                assert_ne!(tp, 0);
            }
            #[cfg(target_arch = "x86_64")]
            assert_eq!(tp, 0);
            set(core::ptr::null_mut());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn threads_have_distinct_slots() {
        let (tx, rx) = std::sync::mpsc::channel();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(64));
        let handles: Vec<_> = (0..64)
            .map(|i| {
                let tx = tx.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    assert!(get().is_null(), "thread {i}: slot not null at start");
                    let mut local = 0u64;
                    let mine = &mut local as *mut u64 as *mut c_void;
                    set(mine);
                    barrier.wait(); // everyone has written
                    assert_eq!(get(), mine, "thread {i}: slot clobbered");
                    tx.send(mine as usize).unwrap();
                    barrier.wait(); // keep `local` alive until all have read
                })
            })
            .collect();
        drop(tx);
        for h in handles {
            h.join().unwrap();
        }
        let mut seen: Vec<usize> = rx.iter().collect();
        assert_eq!(seen.len(), 64);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 64, "two threads observed the same slot value");
    }

    /// `target/release/libchalloc.so`, built by `make test` before the tests
    /// run. Honours `CARGO_TARGET_DIR` for people who set one.
    fn release_so() -> String {
        let target = env::var("CARGO_TARGET_DIR")
            .unwrap_or_else(|_| format!("{}/target", env!("CARGO_MANIFEST_DIR")));
        let so = format!("{target}/release/libchalloc.so");
        assert!(
            std::path::Path::new(&so).exists(),
            "{so} missing: run `cargo build --release` first (`make test` does)"
        );
        so
    }

    fn readelf(args: &[&str]) -> String {
        let out = Command::new("readelf").args(args).output().expect("readelf (binutils)");
        assert!(out.status.success(), "readelf {args:?} failed");
        String::from_utf8(out.stdout).unwrap()
    }

    /// The `.so` must reach its slot through initial-exec relocations only.
    #[test]
    fn shared_object_uses_initial_exec_tls() {
        let so = release_so();
        let phdrs = readelf(&["-lW", &so]);
        assert!(phdrs.lines().any(|l| l.trim_start().starts_with("TLS")), "no PT_TLS:\n{phdrs}");

        let relocs = readelf(&["-rW", &so]);
        #[cfg(target_arch = "x86_64")]
        let want = "R_X86_64_TPOFF64";
        #[cfg(target_arch = "aarch64")]
        let want = "R_AARCH64_TLS_TPREL64";
        assert!(relocs.contains(want), "no {want} relocation:\n{relocs}");
        for bad in ["TLSDESC", "DTPMOD", "DTPOFF", "TLSGD", "TLSLD", "TLS_DTPREL"] {
            assert!(!relocs.contains(bad), "general-dynamic relocation {bad} present:\n{relocs}");
        }

        let dynsyms = readelf(&["--dyn-syms", "-W", &so]);
        assert!(!dynsyms.contains("__tls_get_addr"), "imports __tls_get_addr:\n{dynsyms}");
        assert!(!dynsyms.contains("_dl_tlsdesc"), "imports TLS descriptor resolver:\n{dynsyms}");
        // Neither the slot nor an accessor is a dynamic symbol (no exported
        // write primitive to the heap pointer); the probe C tests use is
        // `challoc_version`.
        for sym in ["challoc_tls_get", "challoc_tls_set"] {
            assert!(!dynsyms.contains(sym), "{sym} exported:\n{dynsyms}");
        }
        assert!(dynsyms.contains("challoc_version"), "challoc_version not exported:\n{dynsyms}");
        assert!(
            !dynsyms.lines().any(|l| l.ends_with(" challoc_tls")),
            "challoc_tls slot leaked as a dynamic symbol:\n{dynsyms}"
        );
    }
}
