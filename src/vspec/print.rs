//! Calls the report writers of `api/print.rs` make into code outside
//! `verus!`, and the ghost `std_specs` impls those writers need
//! (`Cgroup`'s `Display`, `PercpuArena`'s `PartialEq`). Each wrapper
//! claims only that the call returns (and its result type); the report
//! prints what it gives. Delete a wrapper when its callee enters `verus!`
//! and call the callee directly.

use vstd::prelude::*;

verus! {

/// libc's `FILE`, opaque: `malloc_info` only passes the caller's stream on.
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExFile(libc::FILE);

/// The smallest and largest [`crate::layout::BASE_CLASSES`] slab size of
/// the classes `lo..=hi` (0 for an empty range), the table geometry the
/// report compares a page-raised range with. Trusted: the iterator chain
/// with function-pointer reducers of the report's code, and
/// `BASE_CLASSES` is an `exec const` whose entries the verifier cannot
/// see. `hi` must index the table: `BASE_CLASSES[c]` panics past it (the
/// one caller passes `page_sized_classes`, which ensures
/// `lo <= hi < NCLASSES`).
#[verifier::external_body]
#[inline(always)]
pub(crate) fn base_slab_sizes(lo: usize, hi: usize) -> (usize, usize)
    requires
        hi < crate::layout::NCLASSES,
{
    let base = |f: fn(usize, usize) -> usize| {
        (lo..=hi)
            .map(|c| crate::layout::BASE_CLASSES[c].slab_size())
            .reduce(f)
            .unwrap_or(0)
    };
    (base(usize::min), base(usize::max))
}

// `sys::cgroup::Cgroup` formats under any formatter, for the report's
// `cgroup:` line: its `Display` (in `sys/cgroup.rs`) is verified with no
// precondition, so `fmt_req` is true. Ghost only: ordinary builds have no
// `std_specs`.
#[cfg(verus_only)]
impl vstd::std_specs::fmt::DisplaySpecImpl for crate::sys::cgroup::Cgroup {
    open spec fn fmt_req(&self, f: &core::fmt::Formatter<'_>) -> bool {
        true
    }
}

// The derived `PartialEq` of `config::PercpuArena` compares variants
// (ghost only: ordinary builds have no `std_specs`).
#[cfg(verus_only)]
impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::config::PercpuArena {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

} // verus!

verus! {

/// `s.allocated()`, small plus large, as the build computes it (a panic
/// on overflow where overflow checks are on, wrapping where they are
/// off). Trusted: `ArenaStats::allocated` requires the sum to fit, which
/// no invariant bounds for the process-wide figures; the result is the
/// sum when it fits, as for `stats::fig_add`.
#[verifier::external_body]
#[inline(always)]
pub(crate) fn stats_allocated(s: &crate::stats::ArenaStats) -> (r: usize)
    ensures
        s.small_allocated + s.large_allocated <= usize::MAX ==> r == s.small_allocated + s.large_allocated,
{
    s.allocated()
}

} // verus!
