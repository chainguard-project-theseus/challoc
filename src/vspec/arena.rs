//! Specifications for `arena.rs`: `AtomicPtr::new`, `load`, `store` and `swap`,
//! and the invariants of the locks whose values have no invariant.

use vstd::prelude::*;

#[cfg(verus_only)]
use crate::vspec::lock::lock_inv;

verus! {

/// `AtomicPtr::load`, which vstd does not specify: any pointer.
pub assume_specification<T>[ core::sync::atomic::Atomic::<*mut T>::load ](
    a: &core::sync::atomic::Atomic<*mut T>,
    order: core::sync::atomic::Ordering,
) -> *mut T;

/// `AtomicPtr::store`, which vstd does not specify. It panics for an
/// `Acquire` or `AcqRel` ordering.
pub assume_specification<T>[ core::sync::atomic::Atomic::<*mut T>::store ](
    a: &core::sync::atomic::Atomic<*mut T>,
    val: *mut T,
    order: core::sync::atomic::Ordering,
)
    requires
        !(order is Acquire),
        !(order is AcqRel),
;

/// `AtomicPtr::new`, which vstd does not specify.
pub assume_specification<T>[ core::sync::atomic::Atomic::<*mut T>::new ](v: *mut T) -> core::sync::atomic::Atomic<*mut T>;

/// `AtomicPtr::swap`, which vstd does not specify: any previous pointer.
pub assume_specification<T>[ core::sync::atomic::Atomic::<*mut T>::swap ](
    a: &core::sync::atomic::Atomic<*mut T>,
    val: *mut T,
    order: core::sync::atomic::Ordering,
) -> *mut T;

/// The statistics an `epoch` stores: any value will do.
pub broadcast axiom fn axiom_arena_stats_lock_inv(v: crate::stats::ArenaStats)
    ensures
        #[trigger] lock_inv(v),
;

} // verus!
