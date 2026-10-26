//! Specifications for `config.rs`: `c_void`, `<*const T>::cast` and
//! `char::to_digit`.

use vstd::prelude::*;

use crate::config::to_digit_spec;

verus! {

/// `core::ffi::c_void`, opaque: `config::linux` holds the symbol
/// addresses its assembly returns as `*const c_void` and only casts them.
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExCVoid(core::ffi::c_void);

/// `<*const T>::cast`: a pointer to `U`; nothing is claimed about it
/// (`config::linux` only tests the result for null before a trusted read).
pub assume_specification<T: core::marker::PointeeSized, U>[ <*const T>::cast::<U> ](p: *const T) -> *const U;

/// `char::to_digit` (`core` documentation): the digits are `0`..`9`, then
/// `a`..`z` and `A`..`Z` for 10..35, and only those below `radix` count;
/// panics for a radix outside `2..=36`.
pub assume_specification[ char::to_digit ](c: char, radix: u32) -> (r: Option<u32>)
    requires
        2 <= radix <= 36,
    ensures
        r == to_digit_spec(c as u32, radix),
;

} // verus!
