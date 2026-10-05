//! `core::cell::UnsafeCell`, which `api::Global` keeps the init-once
//! statics in (the lock keeps its value in a vstd `PCell`); and the lock
//! invariant's names at the paths the per-type axioms import them from.
//! The invariant itself ([`lock_inv`], [`lock_init`], [`lock_frame`]) is
//! specified with `Lock` in `lock.rs`.

use vstd::prelude::*;

#[cfg(verus_only)]
pub use crate::lock::{lock_frame, lock_init, lock_inv};

verus! {

/// Opaque: Verus does not model the contents of an `UnsafeCell`, so
/// verified code never reads the value through one (`api::Global::get`
/// is trusted instead).
#[verifier::external_type_specification]
#[verifier::external_body]
#[verifier::reject_recursive_types(T)]
pub struct ExUnsafeCell<T: ?Sized>(core::cell::UnsafeCell<T>);

/// `UnsafeCell::new`: wraps the value; nothing is claimed about the cell.
pub assume_specification<T>[ core::cell::UnsafeCell::<T>::new ](value: T) -> core::cell::UnsafeCell<T>;

} // verus!
