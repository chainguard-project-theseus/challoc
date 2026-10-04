//! Reserved address space that is not committed yet: an [`Unbacked`]
//! token owns a range of a `PROT_NONE` reservation and grants no access to
//! it. Committing the range turns the token into the permission to its
//! bytes ([`Unbacked::commit`]).

#[cfg(verus_only)]
use crate::vspec::addr::cast_provenance;
#[cfg(verus_only)]
use crate::vspec::zero::Zeroed;
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

    /// The permission to the bytes, once they are committed, and the token
    /// that they read as zero. Trusted for two kernel facts, with a premise
    /// its callers keep and nothing checks: call it only after `mprotect`
    /// made exactly this range readable and writable
    /// (`meta::commit_range` returned `Ok`). Then `mprotect(2)`: the range
    /// is ordinary memory this process owns, addressed through
    /// integer-to-pointer casts. And `mmap(2)`: a private anonymous mapping
    /// reads as zero until it is first written, which this range never
    /// was, because an `Unbacked` token stands for bytes that have been
    /// `PROT_NONE` since their reservation mapped them (the token's doc:
    /// minted only from a fresh reservation, and no permission to them
    /// exists before this call consumes the token). Call sites:
    /// `meta::Inner::commit_bump_to` and `meta::Inner::reserve_committed`,
    /// each right after its successful commit of the token's range.
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

    /// The permission to the bytes of a lazily committed table, still
    /// uncommitted. Trusted, with a premise nothing checks: Verus does not
    /// model page protection. The bytes stay `PROT_NONE` until
    /// `Lazy::ensure` commits their page, and the holder of the
    /// permission reads only pages it has committed. `SlabTable` keeps
    /// that by its watermark: a slab's descriptor, bitmap words and owner
    /// word are committed before the watermark passes the slab, and every
    /// read is of a slab below a value the watermark held (the `# Safety`
    /// contracts of `desc_unchecked`, `bits_unchecked`,
    /// `is_set_unchecked`, `owner_word` and `owner_at`); a slab set
    /// commits the words a slab's bit needs before the bit is used
    /// (`Set::ensure`, the `from_raw` and `from_raw_chunked` safety
    /// contracts). The token is used up, so the bytes get one permission
    /// (the derivation of `false` from two calls of the axiom this
    /// replaced, `sets::axiom_table_bytes`, is in
    /// `verus/audit/invariants.md`): [`Meta::reserve_table`](crate::meta::Meta::reserve_table),
    /// its one caller, passes the token `Inner::reserve` just carved for
    /// the table. Not at address 0 (`Meta::reserve_table` ensures it):
    /// a permission there proves `false` (`PointsTo::is_nonnull`).
    #[verifier::external_body]
    pub proof fn into_lazy(tracked self) -> (tracked r: PointsToRaw)
        requires
            self.addr() > 0,
        ensures
            r.is_range(self.addr(), self.len()),
            r.provenance() == cast_provenance(),
    {
        unimplemented!()
    }
}

} // verus!
