//! Helpers of the report writers of `api/print.rs`, and the external type
//! and ghost `std_specs` impls those writers need (`ExFile`,
//! `PercpuArena`'s `PartialEq`). Each helper claims nothing about its
//! result; the report prints what it gives.

use vstd::prelude::*;

verus! {

/// libc's `FILE`, opaque: `malloc_info` only passes the caller's stream on.
#[verifier::external_type_specification]
#[verifier::external_body]
#[allow(dead_code)]
pub struct ExFile(libc::FILE);

/// The smallest and largest [`crate::layout::BASE_CLASSES`] slab size of
/// the classes `lo..=hi` (0 for an empty range), the table geometry the
/// report compares a page-raised range with. `hi` must index the table
/// (the one caller passes `page_sized_classes`, which ensures
/// `lo <= hi < NCLASSES`). A loop rather than the iterator chain with
/// function-pointer reducers, which Verus does not specify.
#[inline(always)]
pub(crate) fn base_slab_sizes(lo: usize, hi: usize) -> (usize, usize)
    requires
        hi < crate::layout::NCLASSES,
{
    if lo > hi {
        return (0, 0);
    }
    let first = crate::layout::BASE_CLASSES[lo].slab_size();
    let (mut min, mut max) = (first, first);
    let mut c = lo + 1;
    while c <= hi
        invariant
            lo < c <= hi + 1,
            hi < crate::layout::NCLASSES,
        decreases hi + 1 - c,
    {
        let s = crate::layout::BASE_CLASSES[c].slab_size();
        if s < min {
            min = s;
        }
        if s > max {
            max = s;
        }
        c += 1;
    }
    (min, max)
}

// The derived `PartialEq` of `config::PercpuArena` compares variants
// (ghost only: ordinary builds have no `std_specs`).
#[cfg(verus_keep_ghost)]
impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::config::PercpuArena {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

} // verus!
