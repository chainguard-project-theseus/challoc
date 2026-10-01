//! Specifications for `config.rs`: `CStr`, `char::to_digit`,
//! `i64::wrapping_neg` and `usize::ilog2`.

#[cfg(verus_only)]
use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;

#[cfg(verus_only)]
use crate::config::to_digit_spec;

verus! {

/// `core::ffi::CStr`, opaque: verified code only passes the static names
/// of `config::Value::Str` on.
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExCStr(core::ffi::CStr);

/// `<*const T>::cast`: the same address as a pointer to `U`.
pub assume_specification<T: core::marker::PointeeSized, U>[ <*const T>::cast::<U> ](p: *const T) -> (r: *const U)
    ensures
        r@.addr == p@.addr,
;

/// `CStr::as_ptr`: the address of the string; nothing is claimed about
/// it.
pub assume_specification[ core::ffi::CStr::as_ptr ](s: &core::ffi::CStr) -> *const core::ffi::c_char;

/// `char::to_digit` (`core` documentation): the digits are `0`..`9`, then
/// `a`..`z` and `A`..`Z` for 10..35, and only those below `radix` count;
/// panics for a radix outside `2..=36`.
pub assume_specification[ char::to_digit ](c: char, radix: u32) -> (r: Option<u32>)
    requires
        2 <= radix <= 36,
    ensures
        r == to_digit_spec(c as u32, radix),
;

/// `i64::wrapping_neg`: `-x`, except that `i64::MIN` is its own negation.
pub assume_specification[ i64::wrapping_neg ](x: i64) -> (r: i64)
    ensures r == if x == i64::MIN { i64::MIN as int } else { -x },
;

/// `usize::ilog2`: the position of the highest set bit; panics for 0.
pub assume_specification[ usize::ilog2 ](x: usize) -> (r: u32)
    requires x > 0,
    ensures r < 64, pow2(r as nat) <= x, x < pow2(r as nat + 1),
;


} // verus!
