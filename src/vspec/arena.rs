//! Specifications for `arena.rs`: `AtomicPtr::new`, `load`, `store` and `swap`,
//! and the invariants of the locks whose values have no invariant.

use vstd::prelude::*;

#[cfg(verus_only)]
use crate::vspec::lock::lock_inv;

verus! {

/// `AtomicPtr::load`, which vstd does not specify: any pointer. It panics
/// for a `Release` or `AcqRel` ordering.
pub assume_specification<T>[ core::sync::atomic::Atomic::<*mut T>::load ](
    a: &core::sync::atomic::Atomic<*mut T>,
    order: core::sync::atomic::Ordering,
) -> *mut T
    requires
        !(order is Release),
        !(order is AcqRel),
;

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

/// The statistics an `epoch` stores satisfy the accounting identities
/// (`ArenaStats::identities`). Trusted as every `lock_inv` axiom is: the
/// lock's writers are `Arena`'s initialisation (`ArenaStats::empty`, all
/// zero) and `Arena::store_stats`, which requires them.
pub broadcast axiom fn axiom_arena_stats_lock_inv(v: crate::stats::ArenaStats)
    ensures
        #[trigger] lock_inv(v) == v.identities(),
;

} // verus!
