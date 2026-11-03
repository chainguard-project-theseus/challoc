//! Calls the report writers of `api/print.rs` make into code Verus does
//! not model, and the external type and ghost `std_specs` impls those
//! writers need (`ExFile`, `PercpuArena`'s
//! `PartialEq`). Each wrapper claims nothing about its result; the report
//! prints what it gives. Delete `base_slab_sizes` when Verus can check
//! its iterator chain over `BASE_CLASSES`.

use vstd::prelude::*;

verus! {

/// libc's `FILE`, opaque: `malloc_info` only passes the caller's stream on.
#[verifier::external_type_specification]
#[verifier::external_body]
#[allow(dead_code)]
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
