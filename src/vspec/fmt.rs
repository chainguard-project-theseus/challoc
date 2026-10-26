//! `core::fmt::Write`, for the sinks inside `verus!` (`sys::Stderr`,
//! `sys::features::FeatureLine`), and the `Formatter` writes of the
//! `Display` impls inside `verus!`. Nothing is
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

/// `Formatter::write_str`: nothing is claimed about what it writes or
/// returns.
pub assume_specification<'a>[ core::fmt::Formatter::<'a>::write_str ](f: &mut core::fmt::Formatter<'a>, data: &str) -> core::fmt::Result;

/// `Formatter::write_fmt`: as for `write_str`.
pub assume_specification<'a>[ core::fmt::Formatter::<'a>::write_fmt ](f: &mut core::fmt::Formatter<'a>, args: core::fmt::Arguments<'_>) -> core::fmt::Result;

} // verus!

verus! {

// `sys::Errno` formats under any formatter (its `Display`, in
// `sys/errno.rs` outside `verus!`, writes a name or a decimal fallback
// and has no precondition). Ghost only:
// ordinary builds have no `std_specs`. Closed, and stated by
// [`lemma_errno_fmt_req`] where a module `broadcast use`s it, so that
// modules that never format an `Errno` do not carry the fact.
#[cfg(verus_keep_ghost)]
impl vstd::std_specs::fmt::DisplaySpecImpl for crate::sys::Errno {
    closed spec fn fmt_req(&self, f: &core::fmt::Formatter<'_>) -> bool {
        true
    }
}

/// Every `Errno` formats (`Display`'s precondition), for `format_args!`.
pub broadcast proof fn lemma_errno_fmt_req(e: crate::sys::Errno, f: &core::fmt::Formatter<'_>)
    ensures
        #[trigger] vstd::std_specs::fmt::DisplaySpec::fmt_req(&e, f),
{
}

} // verus!
