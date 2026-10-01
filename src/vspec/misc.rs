//! Specifications of `Option::map_or`, `str::as_ptr`, `compiler_fence` and
//! `Option::as_deref_mut`.

use vstd::prelude::*;

verus! {

// `core` functions vstd does not specify.

pub assume_specification<T, U, F: FnOnce(T) -> U>[ Option::<T>::map_or ](
    option: Option<T>,
    default: U,
    f: F,
) -> (r: U)
    requires
        option matches Some(v) ==> f.requires((v,)),
    ensures
        option is None ==> r == default,
        option matches Some(v) ==> f.ensures((v,), r),
;

/// `str::as_ptr`: the address of the first byte; nothing is claimed
/// about it.
pub assume_specification[ str::as_ptr ](s: &str) -> *const u8;

/// `core::sync::atomic::compiler_fence`: panics on `Relaxed` only, and
/// has no effect Verus models.
pub assume_specification[ core::sync::atomic::compiler_fence ](order: core::sync::atomic::Ordering)
    requires
        !(order is Relaxed),
    opens_invariants none
    no_unwind
;

} // verus!

verus! {

/// `Option::as_deref_mut`: a reborrow of the contents. Nothing is claimed
/// about the result; the one caller (`ClassState::finish`) only passes the
/// reference on to `xorshift`.
pub assume_specification<T: core::ops::DerefMut>[ Option::<T>::as_deref_mut ](o: &mut Option<T>) -> Option<&mut <T as core::ops::Deref>::Target>;

} // verus!
