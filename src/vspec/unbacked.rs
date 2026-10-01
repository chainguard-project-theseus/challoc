//! Reserved address space that is not committed yet: an [`Unbacked`]
//! token owns a range of a `PROT_NONE` reservation and grants no access to
//! it. Committing the range turns the token into the permission to its
//! bytes ([`Unbacked::commit`]).

#[cfg(verus_only)]
use crate::vspec::addr::cast_provenance;
use vstd::prelude::*;
#[cfg(verus_only)]
use vstd::raw_ptr::PointsToRaw;

verus! {

/// Ghost token: this process owns the `len` reserved, uncommitted bytes
/// at `addr`, and nothing else does. Verified code gets one only from the
/// trusted `layout::Reserved::into_unbacked` (consuming the reservation's
/// token, so each range is handed out once); the test-only `Meta::new`
/// assumes one for a test's own mapping. The fields are private, and
/// splitting and joining tokens keeps them apart.
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

    /// The permission to the bytes, once they are committed. Trusted (the
    /// kernel's `mprotect` contract), with a premise its callers keep and
    /// nothing checks: call it only after `mprotect` made exactly this range
    /// readable and writable (`meta::commit_range` returned `Ok`); the range
    /// is then ordinary memory this process owns, addressed through
    /// integer-to-pointer casts. Call sites: `meta::Inner::commit_bump_to`
    /// and `meta::Inner::reserve_committed`, each right after its
    /// successful commit of the token's range.
    #[verifier::external_body]
    pub proof fn commit(tracked self) -> (tracked r: PointsToRaw)
        ensures
            r.is_range(self.addr(), self.len()),
            r.provenance() == cast_provenance(),
    {
        unimplemented!()
    }
}

} // verus!
