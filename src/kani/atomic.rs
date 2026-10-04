//! The atomics, fences and cells of `core` that `vspec/arena.rs`,
//! `vspec/misc.rs`, `vspec/misc_extra.rs`, `vspec/class.rs` and
//! `vspec/lock.rs` specify, checked against their contracts on one
//! thread. Their contracts claim nothing about values (vstd's atomics
//! claim nothing either): what is checked is that a call under the
//! contract's `requires` returns without a panic or undefined behaviour,
//! and, where the contract's comment says so, which orderings panic.
//! Kani runs one thread, so these are sequential facts; that a load may
//! return any value, the contracts' "any pointer", is what lets other
//! threads in, and no harness constrains it.

use core::cell::{Cell, UnsafeCell};
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

/// An arbitrary pointer: to a local, or from any address (null included).
fn any_ptr(x: &mut u64) -> *mut u64 {
    if kani::any() {
        x
    } else {
        core::ptr::without_provenance_mut(kani::any())
    }
}

/// S269 `core::sync::atomic::Atomic::<*mut T>::new`: returns, for every pointer.
/// S267 `core::sync::atomic::Atomic::<*mut T>::load`: under `!(order is Release) && !(order is
/// AcqRel)`, returns. S268 `core::sync::atomic::Atomic::<*mut T>::store`: under `!(order is
/// Acquire) && !(order is AcqRel)`, returns. S270 `core::sync::atomic::Atomic::<*mut T>::swap`:
/// no `requires`; returns under every ordering. (Sequentially, each also
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

/// S341 `core::sync::atomic::Atomic::<u32>::as_ptr`: no `ensures`; it returns the address of
/// the atomic's value (a write through it is what the next load reads).
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

/// S337 `core::sync::atomic::compiler_fence`: under `!(order is Relaxed)`, returns (on every
/// such ordering, so it does not unwind); on `Relaxed` it panics, as its
/// comment says (`compiler_fence_relaxed`).
#[kani::proof]
fn compiler_fence() {
    let o = any_order();
    kani::assume(!matches!(o, Ordering::Relaxed));
    core::sync::atomic::compiler_fence(o);
}

/// `compiler_fence(Relaxed)` panics (the excluded ordering). Not part of
/// the contract.
#[kani::proof]
#[kani::should_panic]
fn compiler_fence_relaxed() {
    let o = any_order();
    kani::assume(matches!(o, Ordering::Relaxed));
    core::sync::atomic::compiler_fence(o);
    super::must_not_return();
}

/// S338 `core::sync::atomic::fence`: under `!(order is Relaxed)`, returns (on
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

/// S283 `core::mem::drop`: no contract; it returns, having run its
/// argument's destructor once (for a type with a destructor) and done
/// nothing else (for a `Copy` type).
#[kani::proof]
#[allow(dropping_copy_types)] // the `Copy` case is part of the check
fn mem_drop() {
    let n = Cell::new(0);
    core::mem::drop(Counted(&n));
    assert_eq!(n.get(), 1);
    core::mem::drop(kani::any::<u64>());
    core::mem::drop(());
}

/// S329 `core::cell::UnsafeCell::<T>::new`: no `ensures`; it returns, a cell holding
/// the value, for a word, a zero-sized type and a type with a destructor
/// (which `new` does not run).
#[kani::proof]
fn unsafe_cell_new() {
    let v: u64 = kani::any();
    let c = UnsafeCell::new(v);
    // SAFETY: `c` is a local nothing else accesses.
    assert_eq!(unsafe { *c.get() }, v);
    assert_eq!(c.into_inner(), v);
    let _ = UnsafeCell::new(());
    let n = Cell::new(0);
    let d = UnsafeCell::new(Counted(&n));
    assert_eq!(n.get(), 0);
    drop(d);
    assert_eq!(n.get(), 1);
}
