//! The atomics and fences of `core` that `vspec/arena.rs` and
//! `vspec/misc.rs` specify, and `core::mem::drop` (`vspec/class.rs`;
//! `verus/ledger/src/vspec/`), checked against their contracts on one
//! thread. Their contracts claim nothing about values (vstd's atomics
//! claim nothing either): what is checked is that a call under the
//! contract's `requires` returns without a panic or undefined behaviour,
//! and, where the contract's comment says so, which orderings panic.
//! Kani runs one thread, so these are sequential facts; that a load may
//! return any value, the contracts' "any pointer", is what lets other
//! threads in, and no harness constrains it.
//!
//! Then the trusted atomic wrappers of `vspec/byte.rs`,
//! `vspec/word.rs` and `vspec/atom.rs`
//! (all but its `load_usize_seen`, whose persistent set is ghost, and
//! `usize_ptr`, checked in its own file), each run on a real `core`
//! atomic. Their contracts state values through a ghost permission, which
//! Kani cannot see (the harnesses pass the erased one). Each harness assumes
//! what the modules trust: that the permission's value is the atomic's,
//! since every write goes through a trusted writer (these wrappers; for
//! the u16 owner words, `class::table::store_owner`). So each harness starts
//! the atomic at an arbitrary value, the old permission's, calls the
//! wrapper under every ordering its `requires` admits, and checks each
//! `ensures` clause on the returned value and on what the atomic holds
//! afterwards, the final permission's. The `id` and `addr` clauses are
//! about the permission alone, which the wrapper's body does not touch.
//! No harness here checks that assumption, or concurrent interleavings,
//! where the orderings matter (the ledger leaves those to loom/GenMC), so
//! these seams stay partly checked; each ledger status names what else
//! its harness leaves out. Each module's `*_bad_orders` harness shows that
//! the orderings a `requires` excludes are the ones `core` panics on.

use crate::vspec::{atom, byte, word};
use core::cell::Cell;
use core::sync::atomic::{
    AtomicPtr, AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use vstd::prelude::Tracked;

/// Every `Ordering`.
const ORDERS: [Ordering; 5] = [
    Ordering::Relaxed,
    Ordering::Release,
    Ordering::Acquire,
    Ordering::AcqRel,
    Ordering::SeqCst,
];

/// An arbitrary `Ordering`.
fn any_order() -> Ordering {
    let i: usize = kani::any();
    kani::assume(i < ORDERS.len());
    ORDERS[i]
}

/// An arbitrary pointer: to a local, or from any address (null included).
fn any_ptr(x: &mut u64) -> *mut u64 {
    if kani::any() {
        x
    } else {
        core::ptr::without_provenance_mut(kani::any())
    }
}

/// `core::sync::atomic::Atomic::<*mut T>::new`: returns, for every
/// pointer. `core::sync::atomic::Atomic::<*mut T>::load`: under
/// `!(order is Release) && !(order is AcqRel)`, returns.
/// `core::sync::atomic::Atomic::<*mut T>::store`: under
/// `!(order is Acquire) && !(order is AcqRel)`, returns.
/// `core::sync::atomic::Atomic::<*mut T>::swap`: no `requires`; returns
/// under every ordering. Checked at `T = u64`: `AtomicPtr` never reads
/// through the pointer, so `T` does not matter. (Sequentially, each also
/// reads what was last written, which the contracts do not claim.)
#[kani::proof]
fn atomic_ptr() {
    let mut x: u64 = kani::any();
    let mut y: u64 = kani::any();
    let p = any_ptr(&mut x);
    let q = any_ptr(&mut y);
    let a = AtomicPtr::new(p);
    let o = any_order();
    kani::assume(!matches!(o, Ordering::Release | Ordering::AcqRel));
    assert_eq!(a.load(o), p);
    let o = any_order();
    kani::assume(!matches!(o, Ordering::Acquire | Ordering::AcqRel));
    a.store(q, o);
    assert_eq!(a.load(Ordering::Relaxed), q);
    assert_eq!(a.swap(p, any_order()), q);
    assert_eq!(a.load(Ordering::Relaxed), p);
}

/// The orderings the `load` and `store` contracts exclude are the ones
/// `core` panics on (the contracts' comments): a load with `Release` or
/// `AcqRel`, a store with `Acquire` or `AcqRel`. Not part of a contract.
#[kani::proof]
#[kani::should_panic]
fn atomic_ptr_bad_orders() {
    let a = AtomicPtr::new(core::ptr::null_mut::<u64>());
    let o = any_order();
    if kani::any() {
        kani::assume(matches!(o, Ordering::Release | Ordering::AcqRel));
        a.load(o);
    } else {
        kani::assume(matches!(o, Ordering::Acquire | Ordering::AcqRel));
        a.store(core::ptr::null_mut(), o);
    }
    super::must_not_return();
}

/// `core::sync::atomic::Atomic::<u32>::as_ptr`: no `ensures`; it returns
/// the address of the atomic's value (a write through it is what the next
/// load reads).
#[kani::proof]
fn atomic_u32_as_ptr() {
    let a = AtomicU32::new(kani::any());
    let p = a.as_ptr();
    assert_eq!(p as usize, &a as *const AtomicU32 as usize);
    let v: u32 = kani::any();
    // SAFETY: `p` points to `a`'s value, which nothing else accesses.
    unsafe { p.write(v) };
    assert_eq!(a.load(Ordering::Relaxed), v);
}

/// `core::sync::atomic::fence`: under `!(order is Relaxed)`, returns (on
/// every such ordering, so it does not unwind); on `Relaxed` it panics,
/// as its comment says (`fence_relaxed`).
#[kani::proof]
fn fence() {
    let o = any_order();
    kani::assume(!matches!(o, Ordering::Relaxed));
    core::sync::atomic::fence(o);
}

/// `fence(Relaxed)` panics (the excluded ordering). Not part of the
/// contract.
#[kani::proof]
#[kani::should_panic]
fn fence_relaxed() {
    let o = any_order();
    kani::assume(matches!(o, Ordering::Relaxed));
    core::sync::atomic::fence(o);
    super::must_not_return();
}

/// Counts its drops.
struct Counted<'a>(&'a Cell<u32>);

impl Drop for Counted<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

/// `core::mem::drop`: no contract; it returns, having run its argument's
/// destructor once (for a type with a destructor) and done nothing else
/// (for a `Copy` type).
#[kani::proof]
#[allow(dropping_copy_types)] // the `Copy` case is part of the check
fn mem_drop() {
    let n = Cell::new(0);
    core::mem::drop(Counted(&n));
    assert_eq!(n.get(), 1);
    core::mem::drop(kani::any::<u64>());
    core::mem::drop(());
}

/// An ordering a load accepts: not `Release`, not `AcqRel` (the wrappers'
/// load `requires`, and a compare-and-swap's failure ordering).
fn load_order() -> Ordering {
    let o = any_order();
    kani::assume(!matches!(o, Ordering::Release | Ordering::AcqRel));
    o
}

/// An ordering a store accepts: not `Acquire`, not `AcqRel`.
fn store_order() -> Ordering {
    let o = any_order();
    kani::assume(!matches!(o, Ordering::Acquire | Ordering::AcqRel));
    o
}

/// An ordering a load rejects: `Release` or `AcqRel`.
fn bad_load_order() -> Ordering {
    let o = any_order();
    kani::assume(matches!(o, Ordering::Release | Ordering::AcqRel));
    o
}

/// An ordering a store rejects: `Acquire` or `AcqRel`.
fn bad_store_order() -> Ordering {
    let o = any_order();
    kani::assume(matches!(o, Ordering::Acquire | Ordering::AcqRel));
    o
}

/// `vspec::byte::load`: under `!(ordering is Release) && !(ordering is
/// AcqRel)`, `r == perm.value()`, and the byte is unchanged.
#[kani::proof]
fn byte_load() {
    let v0: u8 = kani::any();
    let b = AtomicU8::new(v0);
    assert_eq!(byte::load(&b, load_order(), Tracked::assume_new()), v0);
    assert_eq!(b.load(Ordering::Relaxed), v0);
}

/// `vspec::byte::store`: under `!(ordering is Acquire) && !(ordering is
/// AcqRel)`, `final(perm).value() == v`.
#[kani::proof]
fn byte_store() {
    let b = AtomicU8::new(kani::any());
    let v: u8 = kani::any();
    byte::store(&b, v, store_order(), Tracked::assume_new());
    assert_eq!(b.load(Ordering::Relaxed), v);
}

/// `vspec::byte::compare_exchange`: under `!(failure is Release) &&
/// !(failure is AcqRel)` and any success ordering, `Ok(prev)` means
/// `prev == current`, the byte held `current` and now holds `new`;
/// `Err(prev)` means `prev` is the old value, not `current`, and the
/// byte still holds it. A `kani::cover!` marks each side (Kani reports
/// whether it is reached, and does not fail when it is not).
#[kani::proof]
fn byte_compare_exchange() {
    let v0: u8 = kani::any();
    let current: u8 = kani::any();
    let new: u8 = kani::any();
    let b = AtomicU8::new(v0);
    let r = byte::compare_exchange(
        &b,
        current,
        new,
        any_order(),
        load_order(),
        Tracked::assume_new(),
    );
    let now = b.load(Ordering::Relaxed);
    match r {
        Ok(prev) => {
            kani::cover!(true, "byte compare_exchange Ok");
            assert!(prev == current && v0 == current && now == new);
        }
        Err(prev) => {
            kani::cover!(true, "byte compare_exchange Err");
            assert!(prev == v0 && prev != current && now == prev);
        }
    }
}

/// The orderings `vspec::byte`'s `requires` exclude panic: a store with
/// `Acquire` or `AcqRel`, a load with `Release` or `AcqRel`, a
/// compare-and-swap with a `Release` or `AcqRel` failure ordering. Not
/// part of a contract.
#[kani::proof]
#[kani::should_panic]
fn byte_bad_orders() {
    let b = AtomicU8::new(kani::any());
    let pick: u8 = kani::any();
    if pick == 0 {
        byte::store(&b, kani::any(), bad_store_order(), Tracked::assume_new());
    } else if pick == 1 {
        byte::load(&b, bad_load_order(), Tracked::assume_new());
    } else {
        let _ = byte::compare_exchange(
            &b,
            kani::any(),
            kani::any(),
            any_order(),
            bad_load_order(),
            Tracked::assume_new(),
        );
    }
    super::must_not_return();
}

/// `vspec::word::fetch_or`: `prev == old(perm).value()` and
/// `final(perm).value() == prev | v`, under every ordering.
#[kani::proof]
fn word_fetch_or() {
    let v0: u64 = kani::any();
    let v: u64 = kani::any();
    let w = AtomicU64::new(v0);
    let prev = word::fetch_or(&w, v, any_order(), Tracked::assume_new());
    assert_eq!(prev, v0);
    assert_eq!(w.load(Ordering::Relaxed), prev | v);
}

/// `vspec::word::fetch_and`: `prev == old(perm).value()` and
/// `final(perm).value() == prev & v`, under every ordering.
#[kani::proof]
fn word_fetch_and() {
    let v0: u64 = kani::any();
    let v: u64 = kani::any();
    let w = AtomicU64::new(v0);
    let prev = word::fetch_and(&w, v, any_order(), Tracked::assume_new());
    assert_eq!(prev, v0);
    assert_eq!(w.load(Ordering::Relaxed), prev & v);
}

/// `vspec::word::store`: under `!(ordering is Acquire) && !(ordering is
/// AcqRel)`, `final(perm).value() == v`.
#[kani::proof]
fn word_store() {
    let w = AtomicU64::new(kani::any());
    let v: u64 = kani::any();
    word::store(&w, v, store_order(), Tracked::assume_new());
    assert_eq!(w.load(Ordering::Relaxed), v);
}

/// `vspec::word::load`: under `!(ordering is Release) && !(ordering is
/// AcqRel)`, `r == perm.value()`, and the word is unchanged.
#[kani::proof]
fn word_load() {
    let v0: u64 = kani::any();
    let w = AtomicU64::new(v0);
    assert_eq!(word::load(&w, load_order(), Tracked::assume_new()), v0);
    assert_eq!(w.load(Ordering::Relaxed), v0);
}

/// The orderings `vspec::word`'s `requires` exclude panic: a store with
/// `Acquire` or `AcqRel`, a load with `Release` or `AcqRel`. Not part of
/// a contract.
#[kani::proof]
#[kani::should_panic]
fn word_bad_orders() {
    let w = AtomicU64::new(kani::any());
    if kani::any() {
        word::store(&w, kani::any(), bad_store_order(), Tracked::assume_new());
    } else {
        word::load(&w, bad_load_order(), Tracked::assume_new());
    }
    super::must_not_return();
}

/// `vspec::atom::store_usize`: under `!(ordering is Acquire) &&
/// !(ordering is AcqRel)`, `final(perm).value() == v`.
#[kani::proof]
fn atom_store_usize() {
    let a = AtomicUsize::new(kani::any());
    let v: usize = kani::any();
    atom::store_usize(&a, v, store_order(), Tracked::assume_new());
    assert_eq!(a.load(Ordering::Relaxed), v);
}

/// `vspec::atom::load_usize`: under `!(ordering is Release) &&
/// !(ordering is AcqRel)`, `r == perm.value()`, and the atomic is
/// unchanged.
#[kani::proof]
fn atom_load_usize() {
    let v0: usize = kani::any();
    let a = AtomicUsize::new(v0);
    assert_eq!(
        atom::load_usize(&a, load_order(), Tracked::assume_new()),
        v0
    );
    assert_eq!(a.load(Ordering::Relaxed), v0);
}

/// `vspec::atom::store_usize_at`: through a pointer to a live
/// `AtomicUsize` (its `# Safety`), under `!(ordering is Acquire) &&
/// !(ordering is AcqRel)`, `final(perm).value() == v`: the store reaches
/// the atomic the pointer names (`usize_id_at`, here the pointer made from
/// a reference to it, as `usize_ptr` makes it).
#[kani::proof]
fn atom_store_usize_at() {
    let a = AtomicUsize::new(kani::any());
    let v: usize = kani::any();
    // SAFETY: `a` is live and stays in place.
    unsafe { atom::store_usize_at(&a, v, store_order(), Tracked::assume_new()) };
    assert_eq!(a.load(Ordering::Relaxed), v);
}

/// `vspec::atom::load_u16_at`: through a pointer to a live `AtomicU16`
/// (`perm.ptr() == p` and `perm.is_init()`, and its `# Safety`), under
/// `!(ordering is Release) && !(ordering is AcqRel)`, `r ==
/// u16_value(perm.value())`, the value the atomic holds, which stays.
#[kani::proof]
fn atom_load_u16_at() {
    let v0: u16 = kani::any();
    let a = AtomicU16::new(v0);
    // SAFETY: `a` is live.
    let r = unsafe { atom::load_u16_at(&a, load_order(), Tracked::assume_new()) };
    assert_eq!(r, v0);
    assert_eq!(a.load(Ordering::Relaxed), v0);
}

/// The orderings `vspec::atom`'s `requires` exclude panic: its two
/// stores with `Acquire` or `AcqRel`, its two loads with `Release` or
/// `AcqRel`. Not part of a contract.
#[kani::proof]
#[kani::should_panic]
fn atom_bad_orders() {
    let a = AtomicUsize::new(kani::any());
    let h = AtomicU16::new(kani::any());
    let pick: u8 = kani::any();
    if pick == 0 {
        atom::store_usize(&a, kani::any(), bad_store_order(), Tracked::assume_new());
    } else if pick == 1 {
        // SAFETY: `a` is live and stays in place.
        unsafe { atom::store_usize_at(&a, kani::any(), bad_store_order(), Tracked::assume_new()) };
    } else if pick == 2 {
        atom::load_usize(&a, bad_load_order(), Tracked::assume_new());
    } else {
        // SAFETY: `h` is live.
        unsafe { atom::load_u16_at(&h, bad_load_order(), Tracked::assume_new()) };
    }
    super::must_not_return();
}
