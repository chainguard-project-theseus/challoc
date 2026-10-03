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
use vstd::prelude::Tracked;
use vstd::prelude::verus;
#[cfg(verus_only)]
use vstd::prelude::*;

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

verus! {

/// Ghost: the calling thread's slot, as per-thread ghost state. `value`
/// is what the slot holds; `creates` counts the `Heap::create` calls made
/// with this token (`Heap::create` takes it `&mut` and is the only caller
/// of [`note_create`](TlsSlot::note_create)). The accessors below take
/// it, so a function handed only `&TlsSlot` neither writes the slot nor
/// creates a thread heap through it; that it does not do so at all also
/// needs that nothing it calls makes a token of its own (only the
/// top-level slot users below do).
///
/// Premise of the model (`verus/THEOREMS.md`, "Re-entrant heap
/// registration"): a token is not read after the slot has changed other
/// than through it. [`entry`](TlsSlot::entry) makes one, with an
/// unconstrained value that the first read pins, at each top-level user
/// of the slot: `heap_for_thread`, `existing_heap`, `heap_destructor` and
/// the background thread's `create`. An entry nested in another (the
/// allocator re-entered from `pthread_setspecific`) stands for the outer
/// token lent to it, which the nested call leaves as it found it
/// (`heap::thread`).
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct TlsSlot {
    ghost value: usize,
    ghost creates: vstd::prelude::nat,
}

impl TlsSlot {
    /// The slot's value.
    pub closed spec fn value(self) -> usize {
        self.value
    }

    /// The `Heap::create` calls made with this token.
    pub closed spec fn creates(self) -> nat {
        self.creates
    }

    /// The calling thread's token at a top-level user of the slot:
    /// nothing is known of its value until the slot is read. Trusted in
    /// effect (the slot premise above), though its body is checked.
    pub(crate) proof fn entry() -> (tracked r: TlsSlot) {
        TlsSlot { value: arbitrary(), creates: 0 }
    }

    /// Count a `Heap::create` call (made only there).
    pub(crate) proof fn note_create(tracked &mut self)
        ensures
            final(self).value() == old(self).value(),
            final(self).creates() == old(self).creates() + 1,
    {
        self.creates = self.creates + 1;
    }
}

// The accessors are inline assembly, trusted: a read returns the token's
// value and `set` makes the written value the token's (the premise
// above); nothing else is claimed. The x86_64 `get_tp` is verified: it
// only pairs `get` with 0.

/// Read the calling thread's slot. Null until [`set`] has been called on
/// this thread.
#[inline(always)]
#[verifier::external_body]
pub fn get(Tracked(slot): Tracked<&TlsSlot>) -> (r: *mut c_void)
    ensures
        r as usize == slot.value(),
    opens_invariants none
{
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
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn get_tp(Tracked(slot): Tracked<&TlsSlot>) -> (r: (*mut c_void, usize))
    ensures
        r.0 as usize == slot.value(),
    opens_invariants none
{
    (get(Tracked(slot)), 0)
}

/// Read the calling thread's slot together with the thread pointer
/// (`tpidr_el0`), the modifier the heap pointer is signed with when pointer
/// authentication is on (`sys::pac`, design 6.1): one asm sequence that
/// keeps the thread pointer it loads to reach the slot, so the caller's
/// `autda` costs no extra register move. Trusted: the slot value is the
/// token's; nothing is claimed about the thread pointer.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[verifier::external_body]
pub fn get_tp(Tracked(slot): Tracked<&TlsSlot>) -> (r: (*mut c_void, usize))
    ensures
        r.0 as usize == slot.value(),
    opens_invariants none
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

/// Write the calling thread's slot.
#[inline(always)]
#[verifier::external_body]
pub fn set(value: *mut c_void, Tracked(slot): Tracked<&mut TlsSlot>)
    ensures
        final(slot).value() == value as usize,
        final(slot).creates() == old(slot).creates(),
{
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

} // verus!

// The slot has no exported accessor: a `challoc_tls_set` export would be
// a write primitive to the heap pointer in a hardened allocator. The C
// test `tests/c/tls_threads.c` observes per-thread heaps through
// `mallctl("thread.allocatedp")`.

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::string::String;
    use std::{env, format, vec::Vec};
    use vstd::prelude::Tracked;

    #[test]
    fn slot_starts_null_and_round_trips() {
        // A fresh thread so no other test on this thread has touched the slot.
        std::thread::spawn(|| {
            assert!(get(Tracked::assume_new()).is_null());
            let mut x = 0u8;
            let p = &mut x as *mut u8 as *mut c_void;
            set(p, Tracked::assume_new());
            assert_eq!(get(Tracked::assume_new()), p);
            set(core::ptr::null_mut(), Tracked::assume_new());
            assert!(get(Tracked::assume_new()).is_null());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn get_tp_agrees_with_get_and_names_the_thread_pointer() {
        std::thread::spawn(|| {
            let (v, tp) = get_tp(Tracked::assume_new());
            assert!(v.is_null());
            let mut x = 0u8;
            let p = &mut x as *mut u8 as *mut c_void;
            set(p, Tracked::assume_new());
            let (v2, tp2) = get_tp(Tracked::assume_new());
            assert_eq!(v2, p);
            assert_eq!(v2, get(Tracked::assume_new()));
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
            set(core::ptr::null_mut(), Tracked::assume_new());
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
                    assert!(
                        get(Tracked::assume_new()).is_null(),
                        "thread {i}: slot not null at start"
                    );
                    let mut local = 0u64;
                    let mine = &mut local as *mut u64 as *mut c_void;
                    set(mine, Tracked::assume_new());
                    barrier.wait(); // everyone has written
                    assert_eq!(
                        get(Tracked::assume_new()),
                        mine,
                        "thread {i}: slot clobbered"
                    );
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
        let out = Command::new("readelf")
            .args(args)
            .output()
            .expect("readelf (binutils)");
        assert!(out.status.success(), "readelf {args:?} failed");
        String::from_utf8(out.stdout).unwrap()
    }

    /// The `.so` must reach its slot through initial-exec relocations only.
    #[test]
    fn shared_object_uses_initial_exec_tls() {
        let so = release_so();
        let phdrs = readelf(&["-lW", &so]);
        assert!(
            phdrs.lines().any(|l| l.trim_start().starts_with("TLS")),
            "no PT_TLS:\n{phdrs}"
        );

        let relocs = readelf(&["-rW", &so]);
        #[cfg(target_arch = "x86_64")]
        let want = "R_X86_64_TPOFF64";
        #[cfg(target_arch = "aarch64")]
        let want = "R_AARCH64_TLS_TPREL64";
        assert!(relocs.contains(want), "no {want} relocation:\n{relocs}");
        for bad in [
            "TLSDESC",
            "DTPMOD",
            "DTPOFF",
            "TLSGD",
            "TLSLD",
            "TLS_DTPREL",
        ] {
            assert!(
                !relocs.contains(bad),
                "general-dynamic relocation {bad} present:\n{relocs}"
            );
        }

        let dynsyms = readelf(&["--dyn-syms", "-W", &so]);
        assert!(
            !dynsyms.contains("__tls_get_addr"),
            "imports __tls_get_addr:\n{dynsyms}"
        );
        assert!(
            !dynsyms.contains("_dl_tlsdesc"),
            "imports TLS descriptor resolver:\n{dynsyms}"
        );
        // Neither the slot nor an accessor is a dynamic symbol (no exported
        // write primitive to the heap pointer); the probe C tests use is
        // `challoc_version`.
        for sym in ["challoc_tls_get", "challoc_tls_set"] {
            assert!(!dynsyms.contains(sym), "{sym} exported:\n{dynsyms}");
        }
        assert!(
            dynsyms.contains("challoc_version"),
            "challoc_version not exported:\n{dynsyms}"
        );
        assert!(
            !dynsyms.lines().any(|l| l.ends_with(" challoc_tls")),
            "challoc_tls slot leaked as a dynamic symbol:\n{dynsyms}"
        );
    }
}
