//! Specifications for `config.rs`: `c_void`, `CStr`, `<*const T>::cast`
//! and `<*const T>::wrapping_add`.

use vstd::prelude::*;

verus! {

/// `core::ffi::c_void`, opaque: `config::linux` holds the symbol
/// addresses its assembly returns as `*const c_void` and only casts them.
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExCVoid(core::ffi::c_void);

/// `core::ffi::CStr`, opaque: verified code only passes the static names
/// of `config::Name::cstr` on.
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExCStr(core::ffi::CStr);

/// `<*const T>::cast`: a pointer to `U`; nothing is claimed about it
/// (`config::linux` only tests the result for null before a trusted read).
pub assume_specification<T: core::marker::PointeeSized, U>[ <*const T>::cast::<U> ](p: *const T) -> *const U;

/// `<*const T>::wrapping_add`: a pointer; nothing is claimed about it
/// (`config::linux::env_var` walks `environ` with it, and only tests the
/// result for null before a trusted read).
pub assume_specification<T>[ <*const T>::wrapping_add ](p: *const T, count: usize) -> *const T;

/// `CStr::as_ptr`: the address of the string; nothing is claimed about
/// it.
pub assume_specification[ core::ffi::CStr::as_ptr ](s: &core::ffi::CStr) -> *const core::ffi::c_char;

} // verus!
