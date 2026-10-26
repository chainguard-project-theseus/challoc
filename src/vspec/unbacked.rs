//! Reserved address space that is not committed yet: an [`Unbacked`]
//! token owns a range of a `PROT_NONE` reservation and grants no access to
//! it. `meta::Meta::owning` takes the metadata region's, which
//! `layout::Regions::reserve_owning` hands out with the reservation.
//! Committing the range turns the token into the permission to its bytes
//! ([`Unbacked::commit`]).

#[cfg(verus_keep_ghost)]
use crate::vspec::addr::cast_provenance;
#[cfg(verus_keep_ghost)]
use crate::vspec::zero::Zeroed;
use vstd::prelude::*;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::PointsToRaw;

verus! {

/// Ghost token: this process owns the `len` reserved, uncommitted bytes
/// at `addr`, and nothing else does. The unverified
/// `Regions::reserve_owning` and the test-only `Meta::new` take theirs on
/// trust (`Tracked::assume_new()`) until the proofs of the metadata
/// allocator mint it from the reservation's token. The fields are
/// private, and splitting and joining tokens keeps them apart.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct Unbacked {
    ghost addr: int,
    ghost len: int,
}

impl Unbacked {
    /// First byte.
    pub closed spec fn addr(&self) -> int {
        self.addr
    }

    /// Bytes covered.
    pub closed spec fn len(&self) -> int {
        self.len
    }

    /// The empty range at `addr`: it owns nothing.
    pub proof fn empty(addr: int) -> (tracked u: Unbacked)
        ensures
            u.addr() == addr,
            u.len() == 0,
    {
        Unbacked { addr, len: 0 }
    }

    /// The first `k` bytes and the rest.
    pub proof fn split(tracked self, k: int) -> (tracked r: (Unbacked, Unbacked))
        requires
            0 <= k <= self.len(),
        ensures
            r.0.addr() == self.addr(),
            r.0.len() == k,
            r.1.addr() == self.addr() + k,
            r.1.len() == self.len() - k,
    {
        (Unbacked { addr: self.addr, len: k }, Unbacked { addr: self.addr + k, len: self.len - k })
    }

    /// Two adjacent ranges as one.
    pub proof fn join(tracked self, tracked next: Unbacked) -> (tracked u: Unbacked)
        requires
            self.len() >= 0,
            next.len() >= 0,
            self.addr() + self.len() == next.addr(),
        ensures
            u.addr() == self.addr(),
            u.len() == self.len() + next.len(),
    {
        Unbacked { addr: self.addr, len: self.len + next.len }
    }

    /// The permission to the bytes, once they are committed, and the token
    /// that they read as zero. Trusted for two kernel facts, with a premise
    /// its callers keep and nothing checks: call it only after `mprotect`
    /// made a range holding this one readable and writable
    /// (`meta::commit_range` returned `Ok`). Then `mprotect(2)`: the range
    /// is ordinary memory this process owns, addressed through
    /// integer-to-pointer casts. And `mmap(2)`: a private anonymous mapping
    /// reads as zero until it is first written, which this range never
    /// was, because an `Unbacked` token stands for bytes that have been
    /// `PROT_NONE` since their reservation mapped them (no permission to
    /// them exists before this call consumes the token). Call sites:
    /// `meta::Inner::commit_bump_to` and `meta::Inner::reserve_committed`,
    /// each after its successful commit of a range holding the token's:
    /// `commit_bump_to` commits `[committed, want)` and takes the part
    /// past the deferred tables, and `reserve_committed` takes the
    /// reservation it just committed or, on its eager path, the deferred
    /// part of the table it just carved, after `Inner::flush` committed
    /// `[committed, frontier)`, which holds it.
    #[verifier::external_body]
    pub proof fn commit(tracked self) -> (tracked r: (PointsToRaw, Zeroed))
        ensures
            r.0.is_range(self.addr(), self.len()),
            r.0.provenance() == cast_provenance(),
            r.1.addr() == self.addr(),
            r.1.len() == self.len(),
    {
        unimplemented!()
    }
}

} // verus!
