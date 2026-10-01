//! Specifications of `core` functions that vstd does not cover, for the
//! code verified by Verus (`verus/README.md`). Each is an assumption about
//! `core`, stated from its documentation; together with the
//! `#[verifier::external_body]` functions they are the trusted base of the
//! verified code. One file per area; each file holds a `verus!` block.

// The expansion of `global` in ordinary builds is a braced block.
#![allow(unused_braces)]

use vstd::prelude::*;

pub(crate) mod addr;
pub(crate) mod arena;
pub(crate) mod check;
pub(crate) mod class;
mod config;
pub(crate) mod core_int;
mod fmt;
mod heap;
pub(crate) mod huge;
pub(crate) mod layout;
pub(crate) mod lock;
pub(crate) mod meta;
mod misc;
mod misc_extra;
mod policy;
pub(crate) mod ptr;
pub(crate) mod slice;
pub(crate) mod sys;
pub(crate) mod word;
pub(crate) mod zero;

verus! {

// challoc targets x86_64 and aarch64 only (the `compile_error!`s in
// `lib.rs` refuse anything else).
global size_of usize == 8;

} // verus!
