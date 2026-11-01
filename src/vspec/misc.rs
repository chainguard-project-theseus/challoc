//! Specifications of the derived `PartialEq` of four enums verified code
//! compares: `config::Thp` and `sys::thp_host::ThpSetting` (`thp.rs`),
//! `huge::RunState`, and `slab::State` (`class::ClassState::take`); and of
//! `fence`, `core::hint::assert_unchecked`, `AtomicU32::as_ptr` and
//! `Option::as_deref_mut`.

use vstd::prelude::*;

verus! {

// The derived `PartialEq` of these enums compares variants.

impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::config::Thp {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::sys::thp_host::ThpSetting {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::huge::RunState {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::slab::State {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

/// `core::sync::atomic::fence`: panics on `Relaxed` only, and has no
/// effect Verus models (Verus's memory model is sequentially consistent;
/// the ordering it adds is checked by the loom models in `tests/loom`, not
/// here).
pub assume_specification[ core::sync::atomic::fence ](order: core::sync::atomic::Ordering)
    requires
        !(order is Relaxed),
    opens_invariants none
    no_unwind
;

/// `str::as_ptr`: the address of the first byte; nothing is claimed
/// about it.
pub assume_specification[ str::as_ptr ](s: &str) -> *const u8;

/// `core::hint::assert_unchecked`: undefined behaviour unless `cond`
/// holds (`core`'s safety contract), which this makes a precondition; it
/// has no effect Verus models. Its one verified caller is
/// `layout::small_class`, where the condition is the small table's
/// postcondition.
pub assume_specification[ core::hint::assert_unchecked ](cond: bool)
    requires
        cond,
    opens_invariants none
    no_unwind
;

/// `AtomicU32::as_ptr`: the address of the atomic's value, for a futex
/// syscall argument. Nothing is claimed about it; the verified callers
/// only pass it to the kernel and never dereference it.
pub assume_specification[ core::sync::atomic::Atomic::<u32>::as_ptr ](a: &core::sync::atomic::Atomic<u32>) -> *mut u32;

} // verus!

verus! {

/// `Option::as_deref_mut`: a reborrow of the contents. Nothing is claimed
/// about the result; its callers (`ClassState::claim_with` and
/// `ClassState::finish`) only pass the reference on, to `finish` and to
/// `xorshift`.
pub assume_specification<T: core::ops::DerefMut>[ Option::<T>::as_deref_mut ](o: &mut Option<T>) -> Option<&mut <T as core::ops::Deref>::Target>;

/// `Option::or_else`: `option` itself when it is `Some`, else `f()`. Its
/// caller is `heap::mallocx::alloc`'s arena choice.
pub assume_specification<T, F: FnOnce() -> Option<T>>[ Option::<T>::or_else ](option: Option<T>, f: F) -> (r: Option<T>)
    requires
        option is None ==> f.requires(()),
    ensures
        option is Some ==> r == option,
        option is None ==> f.ensures((), r),
;

} // verus!
