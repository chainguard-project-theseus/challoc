//! Calls the report writers of `api/print.rs` make into code Verus does
//! not model. Each wrapper claims only what its contract states; the
//! report prints what it gives. Delete a wrapper when its callee enters
//! `verus!` and call the callee directly.

use vstd::prelude::*;

verus! {

/// The smallest and largest [`crate::layout::BASE_CLASSES`] slab size of
/// the classes `lo..=hi` (0 for an empty range), the table geometry the
/// report compares a page-raised range with. Trusted: the iterator chain
/// with function-pointer reducers of the report's code, and
/// `BASE_CLASSES` is an `exec const` whose entries the verifier cannot
/// see. `hi` must index the table: `BASE_CLASSES[c]` panics past it (the
/// one caller passes `page_sized_classes`'s range, `lo <= hi <
/// NCLASSES`).
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

} // verus!
