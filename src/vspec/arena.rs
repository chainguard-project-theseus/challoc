//! Specifications for `arena.rs`: `AtomicPtr::new`, `load`, `store` and `swap`.

use vstd::prelude::*;

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

} // verus!
