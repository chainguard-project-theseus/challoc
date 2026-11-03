//! Specifications for `sys`: the `core` byte, integer, slice and pointer
//! methods its parsers, probes and syscall wrappers call.

use vstd::prelude::*;

verus! {

/// `u8::is_ascii_digit`: `0`..`9`.
pub assume_specification[ u8::is_ascii_digit ](b: &u8) -> (r: bool)
    ensures r <==> 48 <= *b <= 57,
;

/// `u8::is_ascii_whitespace`: space, tab, line feed, form feed and
/// carriage return (not vertical tab).
pub assume_specification[ u8::is_ascii_whitespace ](b: &u8) -> (r: bool)
    ensures r <==> (*b == 32 || *b == 9 || *b == 10 || *b == 12 || *b == 13),
;

/// `u8::is_ascii_graphic`: `!`..`~`.
pub assume_specification[ u8::is_ascii_graphic ](b: &u8) -> (r: bool)
    ensures r <==> 33 <= *b <= 126,
;

/// `isize::wrapping_neg`: `-x`, except that `isize::MIN` is its own
/// negation.
pub assume_specification[ isize::wrapping_neg ](x: isize) -> (r: isize)
    ensures r == if x == isize::MIN { isize::MIN as int } else { -x },
;

/// The address of a slice's first element, for a syscall argument; the
/// verified code never dereferences it. Taking it leaves the slice as
/// long as it was; its contents are left unconstrained, so that a buffer
/// the kernel then writes through the address (`read`, `getrandom`, the
/// mask of `ncpus`), which `raw::syscall3` does not model, is
/// not taken to keep its old bytes.
pub assume_specification<T>[ <[T]>::as_mut_ptr ](s: &mut [T]) -> *mut T
    ensures
        final(s)@.len() == old(s)@.len(),
;

pub assume_specification<T>[ <[T]>::as_ptr ](s: &[T]) -> *const T;

/// A reference as a raw pointer, for a syscall argument.
pub assume_specification<T: core::marker::PointeeSized>[ core::ptr::from_ref ](r: &T) -> *const T;

pub assume_specification<T: core::marker::PointeeSized>[ core::ptr::from_mut ](r: &mut T) -> *mut T;

/// `Option::map_or`: `f` of the value, else `default`.
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

/// `Result::map_or`: `f` of the `Ok` value, else `default`.
pub assume_specification<T, E, U, F: FnOnce(T) -> U>[ Result::<T, E>::map_or ](
    result: Result<T, E>,
    default: U,
    f: F,
) -> (r: U)
    requires
        result matches Ok(v) ==> f.requires((v,)),
    ensures
        result is Err ==> r == default,
        result matches Ok(v) ==> f.ensures((v,), r),
;

/// `Result::unwrap_or`: the `Ok` value, else `default`.
pub assume_specification<T, E>[ Result::<T, E>::unwrap_or ](result: Result<T, E>, default: T) -> (r: T)
    ensures
        r == match result {
            Ok(v) => v,
            Err(_) => default,
        },
;

#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExUtf8Error(core::str::Utf8Error);

/// `core::str::from_utf8`: the bytes as a `str` when they are UTF-8.
pub assume_specification[ core::str::from_utf8 ](v: &[u8]) -> Result<&str, core::str::Utf8Error>;

} // verus!
