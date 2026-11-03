//! Initial-exec TLS accessor (design 6.1).
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

use crate::heap::HeapPerm;
use core::arch::{asm, global_asm};
use core::ffi::c_void;
use vstd::prelude::Tracked;
use vstd::prelude::verus;
#[cfg(verus_keep_ghost)]
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

/// Ghost: the calling thread's slot, as per-thread ghost state, in two
/// parts: the slot cell [`cell`](TlsSlot::cell) (what the slot holds, the
/// `Heap::create` count and the thread pointer), which the accessors
/// below take, and the heap loan [`loan`](TlsSlot::loan), the permission
/// to the thread's heap ([`HeapPerm`]) while the slot holds it. The slot
/// readers lend the heap out of the loan alone
/// (`heap::thread::heap_from_slot`), so a holder that has lent the heap
/// out still has the cell to read and park the slot through: the
/// allocation slow path passes it down to the background thread's
/// `create`. `heap::thread::create_heap_for_thread` puts the permission
/// in the loan when it publishes the heap, so the loan is the thread
/// heap's linear permission.
///
/// Premise of the model (re-entrant heap registration): a token is not
/// read after the slot has changed other than through it.
/// [`entry`](TlsSlot::entry) makes one at each verified top-level user of
/// the slot: `heap_for_thread`, `existing_heap`, `heap_destructor`,
/// `api::libc::free_block`, `api::libc::free_usable_block`, the heap
/// entry points `heap::paths::alloc`, `alloc_declined` and `alloc_zeroed`,
/// `heap::mallocx::alloc` (once in each of its two cache arms, which
/// exclude each other), `mallctl`'s `background_thread` write
/// (`api::mallctl::ctl`), and, in verification only, the two free paths
/// of `api::libc::theorem_no_syscall_on_fast_paths`, which restate the
/// two frees' fast paths. The two frees use their token only on their
/// fast path; the slow path they take after a declined fast path reaches
/// `heap_for_thread`, which makes its own after they are done with the
/// first.
/// An entry nested in another (the allocator re-entered from `pthread_setspecific`,
/// or from `pthread_create` and `pthread_join` in the background thread's
/// `create`) stands for the outer token lent to it. At a sentinel the
/// nested call leaves it as it found it (`heap::thread`); at an empty slot
/// (only `create`'s calls) it may create the thread's heap and fill the
/// slot, which [`TlsCell::kept`] allows; `create` parks a heap its
/// `pthread_join` left there before `pthread_create`.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct TlsSlot {
    /// The slot cell.
    pub tracked cell: TlsCell,
    /// The heap loan.
    pub tracked loan: HeapLoan,
}

/// Ghost: the slot cell of a [`TlsSlot`]. `value` is what the slot
/// holds; `creates` counts the `Heap::create` calls made by the cell's
/// holder: a counter `heap::thread::create_heap_for_thread` bumps right
/// after each of its `Heap::create` calls, the only caller of
/// [`note_create`](TlsCell::note_create); `Heap::create` itself does not
/// take the cell, so the count is a convention of that caller. The
/// accessors below take the cell, so a function handed only `&TlsCell`,
/// or `&mut TlsCell` with an `ensures` that keeps it, neither writes the
/// slot nor creates a thread heap through it; that it does not do so at
/// all also needs that nothing it calls makes a token of its own (only
/// the top-level slot users do, and the nested entries [`TlsSlot`] lists).
/// `tp` is the thread pointer, the modifier
/// a signed slot value is signed with ([`get_tp`]).
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct TlsCell {
    ghost value: usize,
    ghost creates: vstd::prelude::nat,
    ghost tp: usize,
}

/// Ghost: the heap loan of a [`TlsSlot`]: the permission to the thread's
/// heap the slot holds, if any, which the slot readers lend out as the
/// `&mut Heap`.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct HeapLoan {
    tracked heap: Option<HeapPerm>,
}

impl TlsCell {
    /// The slot's value.
    pub closed spec fn value(self) -> usize {
        self.value
    }

    /// The `Heap::create` calls made with this cell.
    pub closed spec fn creates(self) -> nat {
        self.creates
    }

    /// The calling thread's thread pointer (`tpidr_el0` on aarch64, `0`
    /// on x86_64, [`get_tp`]): one value per thread.
    pub closed spec fn tp(self) -> usize {
        self.tp
    }

    /// `self` is `old` with the value `v`: the same thread and
    /// `Heap::create` count.
    pub open spec fn set_to(self, old: TlsCell, v: usize) -> bool {
        &&& self.value() == v
        &&& self.creates() == old.creates()
        &&& self.tp() == old.tp()
    }

    /// Count a `Heap::create` call (`create_heap_for_thread`, right after
    /// the call).
    ///
    /// Checked by `crate::tests::trusted_mint_sites_are_pinned`, which pins its
    /// call sites to the ones named here: a new one is named here, with its
    /// premise, and added to that test's list in the same change.
    pub(crate) proof fn note_create(tracked &mut self)
        ensures
            final(self).value() == old(self).value(),
            final(self).creates() == old(self).creates() + 1,
            final(self).tp() == old(self).tp(),
    {
        self.creates = self.creates + 1;
    }

    /// `self` is the cell `old` after a call that may create the
    /// background thread (`purge::background::create`): the same cell,
    /// unless `old` is an empty slot, which a nested free in
    /// `pthread_join` or a nested allocation in `pthread_create` may fill
    /// with the thread's new heap. An empty slot ends empty or holding a
    /// heap, never a sentinel ([`reentry_benign`](crate::heap::reentry_benign)):
    /// a heap `create` parked on `TLS_CREATING` is put back. The spec does
    /// not say which: emptying a slot a nested entry filled, and so losing
    /// that heap, also satisfies `kept`, though no verified writer can
    /// store null over a heap ([`slot_step`](crate::heap::slot_step)).
    pub open spec fn kept(self, old: TlsCell) -> bool {
        &&& old.value() != 0 ==> self == old
        &&& old.value() == 0 ==> !crate::heap::reentry_benign(self.value())
    }

    /// Cells with the same value, `Heap::create` count and thread pointer
    /// are the same cell.
    pub proof fn lemma_same(a: TlsCell, b: TlsCell)
        requires
            a.value() == b.value(),
            a.creates() == b.creates(),
            a.tp() == b.tp(),
        ensures
            a == b,
    {
    }

    /// The cell an allocation tick holds in `c`, reborrowed for one call
    /// (`purge::on_tick`'s start); `c` holds it again after.
    pub proof fn lend<'a, 'b>(tracked c: &'a mut Option<&'b mut TlsCell>) -> (tracked r: &'a mut TlsCell)
        requires
            *old(c) is Some,
        ensures
            *final(c) is Some,
            *r == *(*old(c))->Some_0,
            *(*final(c))->Some_0 == *final(r),
            *final((*final(c))->Some_0) == *final((*old(c))->Some_0),
    {
        match c {
            Some(x) => &mut **x,
            None => vstd::pervasive::proof_from_false(),
        }
    }
}

impl HeapLoan {
    /// The permission to the thread's heap the loan holds, if any.
    pub closed spec fn heap(self) -> Option<HeapPerm> {
        self.heap
    }

    /// `self` is `old` after its heap was lent out: the same heap address,
    /// the heap holding whatever the borrower left in it.
    pub open spec fn lent(self, old: HeapLoan) -> bool {
        &&& old.heap() matches Some(o) ==> (self.heap() matches Some(n) && n.addr() == o.addr())
        &&& old.heap() is None ==> self.heap() is None
    }

    /// Put the thread heap's permission in the loan, which holds none.
    pub(crate) proof fn put(tracked &mut self, tracked h: HeapPerm)
        requires
            old(self).heap() is None,
        ensures
            final(self).heap() == Some(h),
    {
        self.heap = Some(h);
    }

    /// Take the heap permission out of the loan, which then holds none:
    /// before the slot is set to a sentinel, so that the token stays as
    /// [`heap::thread::slot_ok`](crate::heap::slot_ok) has a token at a
    /// sentinel (`heap_destructor`).
    pub(crate) proof fn take(tracked &mut self) -> (tracked r: Option<HeapPerm>)
        ensures
            r == old(self).heap(),
            final(self).heap() is None,
    {
        let tracked mut r = None;
        vstd::modes::tracked_swap(&mut r, &mut self.heap);
        r
    }
}

impl TlsSlot {
    /// The slot's value ([`TlsCell::value`]).
    pub open spec fn value(self) -> usize {
        self.cell.value()
    }

    /// The thread pointer ([`TlsCell::tp`]).
    pub open spec fn tp(self) -> usize {
        self.cell.tp()
    }

    /// `self` is `old` after its heap was lent out: the same cell, and
    /// the loan lent ([`HeapLoan::lent`]).
    pub open spec fn lent(self, old: TlsSlot) -> bool {
        &&& self.cell == old.cell
        &&& self.loan.lent(old.loan)
    }

    /// The calling thread's token at a top-level user of the slot, as the
    /// slot's last user left it: what [`heap::thread::slot_ok`] states of
    /// the value it holds (a heap value comes with the heap's permission,
    /// the heap well formed, and nothing else with one), and `tp` this
    /// thread's thread pointer. Trusted: the slot premise above, and that
    /// the thread's heap is as the last verified writer left it (every
    /// writer of a heap proves `Heap::wf` kept, and no ghost state
    /// survives from one top-level call to the next); the `&'static mut`
    /// stands for the thread's one slot, of which a top-level user is the
    /// only one using it while it runs (the slot premise: a nested user is
    /// lent the outer token), so the heap it borrows (a `&'static mut
    /// Heap`, as `existing_heap` hands out) is the only reference to it.
    /// Not checked by verified code, the premise in full: that a slot value
    /// above the sentinels names, through `tp`, the heap whose permission
    /// `create_heap_for_thread` put in the loan (it publishes
    /// `tls_value_for`'s value, `pac::sign` of the heap under PAC, whose
    /// authentication `pac::axiom_auth_sign` gives back for an address below
    /// the user limit, which is not shown of the metadata region here).
    /// The users that set a sentinel take the permission out first
    /// ([`HeapLoan::take`]). Also not checked: that
    /// `heap_destructor` stores `TLS_EXITED` before `Heap::destroy` frees
    /// the heap, so a nested user that allocates during the destroy (a
    /// signal handler) finds a sentinel, not the heap being freed: an
    /// assertion records the token at `TLS_EXITED` just before the call,
    /// but `Heap::destroy` takes no part of the token, so nothing verified
    /// ties its frees to the slot's value; the order is read off the
    /// code. Called only at the top-level users above, once each: a verified
    /// function that used two tokens would hold two copies of the heap's
    /// ghost state (the two frees are done with their token before their
    /// slow path makes the next one), and a stale one would read and
    /// write the slot as it no longer is, so `set`'s
    /// [`slot_step`](crate::heap::slot_step) would be checked against a
    /// value the slot does not hold. The test
    /// `sys::tls::tests::tls_entry_only_at_listed_users` checks the source
    /// for the callers and the count (a new user goes in its list and in
    /// this doc together); that a nested user is lent the
    /// outer token, and that a user is done with its token before
    /// another user runs, are not checked.
    ///
    /// [`heap::thread::slot_ok`]: crate::heap::slot_ok
    pub(crate) axiom fn entry() -> (tracked r: &'static mut TlsSlot)
        ensures
            crate::heap::slot_ok(r.cell, r.loan),
            r.cell.creates() == 0,
    ;
}

/// The thread pointer [`get_tp`] returns: any on aarch64, 0 on x86_64.
#[cfg(target_arch = "x86_64")]
pub open spec fn tp_ok(tp: usize) -> bool {
    tp == 0
}

/// The thread pointer [`get_tp`] returns: any on aarch64, 0 on x86_64.
#[cfg(not(target_arch = "x86_64"))]
pub open spec fn tp_ok(tp: usize) -> bool {
    true
}

// The accessors are inline assembly, trusted: a read returns the cell's
// value (and the aarch64 `get_tp` its thread pointer), and `set` makes the
// written value the cell's and leaves its other ghost fields alone (the
// premise above); nothing else is claimed. The x86_64 `get_tp` is
// verified: it only pairs `get` with 0.

/// Read the calling thread's slot. Null until [`set`] has been called on
/// this thread.
#[inline(always)]
#[verifier::external_body]
pub fn get(Tracked(slot): Tracked<&TlsCell>) -> (r: *mut c_void)
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

/// Read the calling thread's slot together with the thread pointer: `0`
/// on x86_64, where it is unused.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn get_tp(Tracked(slot): Tracked<&TlsCell>) -> (r: (*mut c_void, usize))
    ensures
        r.0 as usize == slot.value(),
        tp_ok(slot.tp()) ==> r.1 == slot.tp(),
    opens_invariants none
{
    (get(Tracked(slot)), 0)
}

/// Read the calling thread's slot together with the thread pointer
/// (`tpidr_el0`), the modifier the heap pointer is signed with when pointer
/// authentication is on (`sys::pac`, design 6.1): one asm sequence that
/// keeps the thread pointer it loads to reach the slot, so the caller's
/// `autda` costs no extra register move. Trusted: the slot value is the
/// cell's, and the thread pointer, `tpidr_el0`, is the calling thread's,
/// which the cell records ([`TlsCell::tp`]).
#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[verifier::external_body]
pub fn get_tp(Tracked(slot): Tracked<&TlsCell>) -> (r: (*mut c_void, usize))
    ensures
        r.0 as usize == slot.value(),
        r.1 == slot.tp(),
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

/// Write the calling thread's slot. The write is a step the slot's
/// phases allow ([`heap::thread::slot_step`](crate::heap::slot_step)):
/// every verified writer proves it of the value it writes and the value
/// its cell holds, so the slot never leaves `TLS_EXITED`.
#[inline(always)]
#[verifier::external_body]
pub fn set(value: *mut c_void, Tracked(slot): Tracked<&mut TlsCell>)
    requires
        crate::heap::slot_step(old(slot).value(), value as usize),
    ensures
        final(slot).set_to(*old(slot), value as usize),
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

    /// `TlsSlot::entry` makes a slot token only at the top-level slot users
    /// [`TlsSlot`]'s doc lists, once in each, so no verified function holds
    /// two tokens (two `&'static mut` views of the thread's heap and two
    /// copies of its ghost state). `heap::mallocx::alloc` has two calls,
    /// one in each of its `Tcache::Default` and `Tcache::Explicit` match
    /// arms, which exclude each other, so one runs per call; so do the two
    /// free paths of `api::libc::theorem_no_syscall_on_fast_paths`, its
    /// arms `2` and `_` of `match which`.
    #[test]
    fn tls_entry_only_at_listed_users() {
        crate::testutil::assert_only_at(
            concat!("TlsSlot::", "entry()"),
            &[
                ("heap/thread.rs:", "pub fn heap_for_thread(", 1),
                ("heap/thread.rs:", "pub fn existing_heap(", 1),
                (
                    "heap/thread.rs:",
                    "pub unsafe extern \"C\" fn heap_destructor(",
                    1,
                ),
                ("api/libc.rs:", "pub fn free_block(", 1),
                ("api/libc.rs:", "pub fn free_usable_block(", 1),
                ("heap/paths.rs:", "pub fn alloc(", 1),
                ("heap/paths.rs:", "pub fn alloc_declined(", 1),
                ("heap/paths.rs:", "pub fn alloc_zeroed(", 1),
                ("heap/mallocx.rs:", "pub fn alloc(", 2),
                ("api/mallctl.rs:", "fn ctl(", 1),
                ("api/libc.rs:", "fn theorem_no_syscall_on_fast_paths(", 2),
            ],
        );
    }

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
