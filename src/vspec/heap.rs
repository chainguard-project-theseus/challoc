//! Specifications for `heap/`: the types and functions outside `verus!`
//! that the heap's verified code calls, opaque to it. Delete each one when
//! its item enters `verus!` (the "duplicate specification" error says
//! which).

use vstd::prelude::*;

verus! {

#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExCVoid(core::ffi::c_void);

} // verus!

verus! {

/// `Option::or_else`: `option` itself when it is `Some`, else `f()`.
pub assume_specification<T, F: FnOnce() -> Option<T>>[ Option::<T>::or_else ](option: Option<T>, f: F) -> (r: Option<T>)
    requires
        option is None ==> f.requires(()),
    ensures
        option is Some ==> r == option,
        option is None ==> f.ensures((), r),
;

} // verus!
