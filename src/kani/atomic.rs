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

use core::cell::Cell;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

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

/// `core::sync::atomic::Atomic::<*mut T>::new`: returns, for every
/// pointer (to a local, or from any address, null included).
/// `core::sync::atomic::Atomic::<*mut T>::load`: under `!(order is
/// Release) && !(order is AcqRel)`, returns. (Sequentially, the load also
/// reads what `new` stored, which the contracts do not claim.)
#[kani::proof]
fn atomic_ptr_new_load() {
    let mut x: u64 = kani::any();
    let p: *mut u64 = if kani::any() {
        &mut x
    } else {
        core::ptr::without_provenance_mut(kani::any())
    };
    let a = AtomicPtr::new(p);
    let o = any_order();
    kani::assume(!matches!(o, Ordering::Release | Ordering::AcqRel));
    assert_eq!(a.load(o), p);
}

/// The orderings the `load` contract excludes are the ones `core` panics
/// on (the contract's comment): `Release` and `AcqRel`. Not part of the
/// contract.
#[kani::proof]
#[kani::should_panic]
fn atomic_ptr_load_bad_orders() {
    let a = AtomicPtr::new(core::ptr::null_mut::<u64>());
    let o = any_order();
    kani::assume(matches!(o, Ordering::Release | Ordering::AcqRel));
    a.load(o);
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
