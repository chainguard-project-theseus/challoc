//! Specifications of the derived `PartialEq` of three enums verified code
//! compares (`config::Thp`, `sys::thp_host::ThpSetting`,
//! `huge::RunState`), and of `Option::map_or` and `compiler_fence`.

use vstd::prelude::*;

verus! {

// The derived `PartialEq` of these enums compares variants (ghost only:
// ordinary builds have no `std_specs`).

#[cfg(verus_only)]
impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::config::Thp {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

#[cfg(verus_only)]
impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::sys::thp_host::ThpSetting {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

#[cfg(verus_only)]
impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::huge::RunState {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

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
