//! `core::fmt::Write`, for the report writers of `api/print.rs`. Nothing is
//! claimed about what a write does or returns: a sink may fail, and the
//! verified callers only propagate the result.

use vstd::prelude::*;

verus! {

#[verifier::external_trait_specification]
pub trait ExWrite {
    type ExternalTraitSpecificationFor: core::fmt::Write;

    fn write_str(&mut self, s: &str) -> core::fmt::Result;

    fn write_char(&mut self, c: char) -> core::fmt::Result;

    fn write_fmt(&mut self, args: core::fmt::Arguments<'_>) -> core::fmt::Result;
}

} // verus!
