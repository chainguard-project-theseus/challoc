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
//! Then the trusted atomic wrappers of `vspec/byte.rs`, `vspec/count.rs`,
//! `vspec/entry.rs`, `vspec/flags.rs`, `vspec/word.rs` and `vspec/atom.rs`
//! (all but its `load_usize_seen`, whose persistent set is ghost, and
//! `usize_ptr`, checked in its own file), each run on a real `core`
//! atomic. Their contracts state values through a ghost permission, which
//! Kani cannot see (the harnesses pass the erased one; `flags.rs`'s
//! through `fixed`, the bits a `RouteByte` keeps). Each harness assumes
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

use crate::vspec::{atom, byte, count, entry, flags, word};
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

/// `vspec::count::fetch_add`: `prev == old(perm).value()` and
/// `final(perm).value() == wrap32(prev + v)`, the sum reduced mod 2^32,
/// which is `wrapping_add`; under every ordering (it has no ordering
/// `requires`).
#[kani::proof]
fn count_fetch_add() {
    let v0: u32 = kani::any();
    let v: u32 = kani::any();
    let c = AtomicU32::new(v0);
    let prev = count::fetch_add(&c, v, any_order(), Tracked::assume_new());
    assert_eq!(prev, v0);
    assert_eq!(c.load(Ordering::Relaxed), prev.wrapping_add(v));
}

/// `vspec::count::fetch_sub`: `prev == old(perm).value()` and
/// `final(perm).value() == wrap32(prev - v)`, the difference reduced mod
/// 2^32 (Verus's `%` on `int` is Euclidean, so a negative difference
/// wraps up), which is `wrapping_sub`; under every ordering.
#[kani::proof]
fn count_fetch_sub() {
    let v0: u32 = kani::any();
    let v: u32 = kani::any();
    let c = AtomicU32::new(v0);
    let prev = count::fetch_sub(&c, v, any_order(), Tracked::assume_new());
    assert_eq!(prev, v0);
    assert_eq!(c.load(Ordering::Relaxed), prev.wrapping_sub(v));
}

/// `vspec::count::store`: under `!(ordering is Acquire) && !(ordering is
/// AcqRel)`, `final(perm).value() == v`.
#[kani::proof]
fn count_store() {
    let c = AtomicU32::new(kani::any());
    let v: u32 = kani::any();
    count::store(&c, v, store_order(), Tracked::assume_new());
    assert_eq!(c.load(Ordering::Relaxed), v);
}

/// `vspec::count::load`: under `!(ordering is Release) && !(ordering is
/// AcqRel)`, `r == perm.value()`, and the counter is unchanged.
#[kani::proof]
fn count_load() {
    let v0: u32 = kani::any();
    let c = AtomicU32::new(v0);
    assert_eq!(count::load(&c, load_order(), Tracked::assume_new()), v0);
    assert_eq!(c.load(Ordering::Relaxed), v0);
}

/// `vspec::count::swap`: `prev == old(perm).value()` and
/// `final(perm).value() == v`, under every ordering.
#[kani::proof]
fn count_swap() {
    let v0: u32 = kani::any();
    let v: u32 = kani::any();
    let c = AtomicU32::new(v0);
    assert_eq!(count::swap(&c, v, any_order(), Tracked::assume_new()), v0);
    assert_eq!(c.load(Ordering::Relaxed), v);
}

/// `vspec::count::compare_exchange`: under `!(failure is Release) &&
/// !(failure is AcqRel)` and any success ordering, `Ok(prev)` means
/// `prev == current`, the counter held `current` and now holds `new`;
/// `Err(prev)` means `prev` is the old value, not `current`, and the
/// counter still holds it. A `kani::cover!` marks each side (Kani reports
/// whether it is reached, and does not fail when it is not).
#[kani::proof]
fn count_compare_exchange() {
    let v0: u32 = kani::any();
    let current: u32 = kani::any();
    let new: u32 = kani::any();
    let c = AtomicU32::new(v0);
    let r = count::compare_exchange(
        &c,
        current,
        new,
        any_order(),
        load_order(),
        Tracked::assume_new(),
    );
    let now = c.load(Ordering::Relaxed);
    match r {
        Ok(prev) => {
            kani::cover!(true, "count compare_exchange Ok");
            assert!(prev == current && v0 == current && now == new);
        }
        Err(prev) => {
            kani::cover!(true, "count compare_exchange Err");
            assert!(prev == v0 && prev != current && now == prev);
        }
    }
}

/// The orderings `vspec::count`'s `requires` exclude panic: a store with
/// `Acquire` or `AcqRel`, a load with `Release` or `AcqRel`, a
/// compare-and-swap with a `Release` or `AcqRel` failure ordering. Not
/// part of a contract.
#[kani::proof]
#[kani::should_panic]
fn count_bad_orders() {
    let c = AtomicU32::new(kani::any());
    let pick: u8 = kani::any();
    if pick == 0 {
        count::store(&c, kani::any(), bad_store_order(), Tracked::assume_new());
    } else if pick == 1 {
        count::load(&c, bad_load_order(), Tracked::assume_new());
    } else {
        let _ = count::compare_exchange(
            &c,
            kani::any(),
            kani::any(),
            any_order(),
            bad_load_order(),
            Tracked::assume_new(),
        );
    }
    super::must_not_return();
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

/// `vspec::byte::compare_exchange`: as `count_compare_exchange`, on a
/// byte.
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

/// The orderings `vspec::byte`'s `requires` exclude panic, as
/// `count_bad_orders`. Not part of a contract.
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

/// `vspec::entry::store`: under `!(ordering is Acquire) && !(ordering is
/// AcqRel)`, `final(perm).value() == v`, for any two pointers (to a
/// local, or from any address), compared by address (a pointer's
/// provenance is not checked). Checked at `T = u64`, not challoc's
/// `T = Arena`: `AtomicPtr` never reads through the pointer.
#[kani::proof]
fn entry_store() {
    let mut x: u64 = kani::any();
    let mut y: u64 = kani::any();
    let a = AtomicPtr::new(any_ptr(&mut x));
    let v = any_ptr(&mut y);
    entry::store(&a, v, store_order(), Tracked::assume_new());
    assert_eq!(a.load(Ordering::Relaxed), v);
}

/// `vspec::entry::load`: under `!(ordering is Release) && !(ordering is
/// AcqRel)`, `r == perm.value()`, and the atomic is unchanged; at
/// `T = u64`, as `entry_store`.
#[kani::proof]
fn entry_load() {
    let mut x: u64 = kani::any();
    let p = any_ptr(&mut x);
    let a = AtomicPtr::new(p);
    assert_eq!(entry::load(&a, load_order(), Tracked::assume_new()), p);
    assert_eq!(a.load(Ordering::Relaxed), p);
}

/// The orderings `vspec::entry`'s `requires` exclude panic, as
/// `word_bad_orders`. Not part of a contract.
#[kani::proof]
#[kani::should_panic]
fn entry_bad_orders() {
    let a = AtomicPtr::new(core::ptr::null_mut::<u64>());
    if kani::any() {
        entry::store(
            &a,
            core::ptr::null_mut(),
            bad_store_order(),
            Tracked::assume_new(),
        );
    } else {
        entry::load(&a, bad_load_order(), Tracked::assume_new());
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

/// `vspec::flags`: `new(v)` makes a byte whose bits other than `ROUTE`
/// are `v`'s (`fixed(&r) == v & !ROUTE`); then, over any 3 operations,
/// each a `load` under `!(ordering is Release) && !(ordering is AcqRel)`
/// or a `fetch_or` of bits within `ROUTE` (`v & !ROUTE == 0`) under any
/// ordering, every result shows those bits (`r & !ROUTE == fixed(b)`,
/// `prev & !ROUTE == fixed(b)`). `fixed` is uninterpreted, so this is
/// the sequential fact the three contracts stand for: the bits other than
/// `ROUTE` are the made value's in every value the byte holds, and
/// `fetch_or` sets only bits within `ROUTE`.
#[kani::proof]
#[kani::unwind(4)]
fn flags_route_byte() {
    let v: u8 = kani::any();
    let b = flags::new(v);
    let fixed = v & !flags::ROUTE;
    for _ in 0..3 {
        if kani::any() {
            assert_eq!(flags::load(&b, load_order()) & !flags::ROUTE, fixed);
        } else {
            let w: u8 = kani::any();
            kani::assume(w & !flags::ROUTE == 0);
            assert_eq!(flags::fetch_or(&b, w, any_order()) & !flags::ROUTE, fixed);
        }
    }
    assert_eq!(flags::load(&b, Ordering::Relaxed) & !flags::ROUTE, fixed);
}

/// `vspec::flags::load` with `Release` or `AcqRel`, the orderings its
/// `requires` excludes, panics. Not part of a contract.
#[kani::proof]
#[kani::should_panic]
fn flags_bad_orders() {
    let b = flags::new(kani::any());
    flags::load(&b, bad_load_order());
    super::must_not_return();
}
