//! Reserved address space that is not committed yet: an [`Unbacked`]
//! token owns a range of a `PROT_NONE` reservation and grants no access to
//! it. `meta::Meta::owning` takes the metadata region's, which
//! `layout::Regions::reserve_owning` hands out with the reservation.

use vstd::prelude::*;

verus! {

/// Ghost token: this process owns the `len` reserved, uncommitted bytes
/// at `addr`, and nothing else does. The unverified
/// `Regions::reserve_owning` and the test-only `Meta::new` take theirs on
/// trust (`Tracked::assume_new()`) until the proofs of the metadata
/// allocator mint it from the reservation's token.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct Unbacked {
    ghost addr: int,
    ghost len: int,
}

} // verus!
