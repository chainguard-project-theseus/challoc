//! The ghost contents model for zeroed memory: a [`ZeroRaw`] is the
//! permission to bytes that read as zero, from which a word's permission
//! holding 0 is made ([`axiom_zero_word`]).
//! `bitmap::TwoLevelBitmap::from_raw` and `from_raw_chunked` build their
//! views' word permissions from one. A [`Zeroed`] token says a range of
//! bytes reads as zero: `meta::Meta::alloc_zeroed` returns one for the
//! object it hands out, from [`axiom_wrote_zero`] for a reused one.

use vstd::prelude::*;
use vstd::raw_ptr::PointsToRaw;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::{PointsTo, Provenance};

verus! {

/// Ghost token: the `len` bytes at address `addr` read as zero. A token
/// is not a permission to access the bytes, and nothing ties it to the
/// writes the program or the allocator make: it stays true because the
/// allocator hands the bytes to no one while it holds the token.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct Zeroed {
    ghost addr: int,
    ghost len: int,
}

impl Zeroed {
    /// First byte.
    pub closed spec fn addr(&self) -> int {
        self.addr
    }

    /// Bytes covered.
    pub closed spec fn len(&self) -> int {
        self.len
    }

    /// The token covers at least `n` bytes from `p`.
    pub open spec fn covers(&self, p: *mut u8, n: int) -> bool {
        &&& self.addr() == p@.addr
        &&& n <= self.len()
    }

    /// The empty range at `addr` reads as zero: there is nothing to read.
    pub proof fn empty(addr: int) -> (tracked z: Zeroed)
        ensures
            z.addr() == addr,
            z.len() == 0,
    {
        Zeroed { addr, len: 0 }
    }

    /// A zero range is its first `k` bytes and the rest, both zero.
    pub proof fn split(tracked self, k: int) -> (tracked r: (Zeroed, Zeroed))
        requires
            0 <= k <= self.len(),
        ensures
            r.0.addr() == self.addr(),
            r.0.len() == k,
            r.1.addr() == self.addr() + k,
            r.1.len() == self.len() - k,
    {
        (Zeroed { addr: self.addr, len: k }, Zeroed { addr: self.addr + k, len: self.len - k })
    }

    /// Two adjacent zero ranges are one.
    pub proof fn join(tracked self, tracked next: Zeroed) -> (tracked z: Zeroed)
        requires
            self.len() >= 0,
            next.len() >= 0,
            self.addr() + self.len() == next.addr(),
        ensures
            z.addr() == self.addr(),
            z.len() == self.len() + next.len(),
    {
        Zeroed { addr: self.addr, len: self.len + next.len }
    }
}

/// Ghost permission to bytes that read as zero: a raw permission together
/// with the fact that its bytes read as zero. A bitmap built on such bytes
/// starts from words that read as zero (`bitmap::TwoLevelBitmap::from_raw`
/// and `from_raw_chunked`, through [`axiom_zero_word`]). Like a
/// `PointsToRaw` it splits by byte set ([`ZeroRaw::split`]); it is linear,
/// so a `ZeroRaw` makes each word's zero permission once. Its one source
/// is [`ZeroRaw::axiom_new`], in `meta::Meta::reserve_table`; the
/// unverified callers of the bitmap constructors still take theirs on
/// trust (`Tracked::assume_new()` outside `verus!`).
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct ZeroRaw {
    tracked raw: PointsToRaw,
}

impl ZeroRaw {
    /// The bytes.
    pub closed spec fn dom(&self) -> Set<int> {
        self.raw.dom()
    }

    /// The permission, borrowed (for `vspec::ptr::axiom_raw_valid`).
    pub proof fn raw(tracked &self) -> (tracked r: &PointsToRaw)
        ensures
            r.dom() == self.dom(),
            r.provenance() == self.provenance(),
    {
        &self.raw
    }

    /// The provenance of the permission.
    pub closed spec fn provenance(&self) -> Provenance {
        self.raw.provenance()
    }

    /// The bytes are exactly `[start, start + len)`.
    pub open spec fn is_range(&self, start: int, len: int) -> bool {
        Set::range(start, start + len) =~= self.dom()
    }

    /// The bytes of `raw`, which `z` reads as zero. Trusted for one
    /// premise nothing checks: `z`'s bytes have not been written since
    /// `z` was made. A `Zeroed` token is not tied to the writes made to
    /// its bytes (its doc), so pairing a token with a permission to bytes
    /// written since would make [`axiom_zero_word`] claim 0 for a word
    /// that holds something else. Its callers, in
    /// `meta::Meta::reserve_table` (and `meta::reserved_table`, its
    /// reservation path), pair a table's permission with the token cut
    /// with it in the lock hold that carved the table: an eager
    /// table's (the committed part cut from `Inner`'s `fresh` and
    /// `fresh_zero` together, fresh memory above the bump pointer that
    /// nothing has written, and the deferred part from one
    /// `Unbacked::into_lazy`) or a reservation's (both from one
    /// `Unbacked::into_lazy`). `meta::Inner::reserve_committed` does the
    /// same for the tables it commits at once, where the only step between
    /// the carve and the pairing is the commit itself (`Inner::flush` or
    /// `Inner::commit`), which writes none of the bytes.
    #[verifier::external_body]
    pub proof fn axiom_new(tracked raw: PointsToRaw, tracked z: Zeroed) -> (tracked r: ZeroRaw)
        requires
            forall|a: int| #[trigger] raw.dom().contains(a) ==> z.addr() <= a < z.addr() + z.len(),
        ensures
            r.dom() == raw.dom(),
            r.provenance() == raw.provenance(),
    {
        ZeroRaw { raw }
    }

    /// No bytes.
    pub proof fn empty(provenance: Provenance) -> (tracked r: ZeroRaw)
        ensures
            r.dom() == Set::<int>::empty(),
            r.provenance() == provenance,
    {
        ZeroRaw { raw: PointsToRaw::empty(provenance) }
    }

    /// The bytes in `range`, and the rest (`PointsToRaw::split`).
    pub proof fn split(tracked self, range: Set<int>) -> (tracked r: (ZeroRaw, ZeroRaw))
        requires
            range.subset_of(self.dom()),
        ensures
            r.0.dom() == range,
            r.0.provenance() == self.provenance(),
            r.1.dom() == self.dom().difference(range),
            r.1.provenance() == self.provenance(),
    {
        let tracked (a, b) = self.raw.split(range);
        (ZeroRaw { raw: a }, ZeroRaw { raw: b })
    }
}

/// The eight bytes of `z`, which read as zero, as the `u64` at `p`
/// holding 0: `PointsToRaw::into_typed`, initialised with the value the
/// bytes hold. Trusted for that value: it fills a gap in vstd's memory
/// model, whose raw permissions do not track byte values (eight zero
/// bytes are the `u64` 0 in either byte order). The premises are
/// `into_typed`'s (a non-null `p` aligned for `u64` whose eight bytes are
/// exactly `z`'s, of `p`'s provenance), so the permission is one
/// `into_typed` would give; `z` is used up, so the zero fact makes one
/// permission.
#[verifier::external_body]
pub proof fn axiom_zero_word(tracked z: ZeroRaw, p: *mut u64) -> (tracked w: PointsTo<u64>)
    requires
        p@.addr != 0,
        p@.addr as int % 8 == 0,
        z.is_range(p@.addr as int, 8),
        z.provenance() == p@.provenance,
    ensures
        w.ptr() == p,
        w.is_init(),
        w.value() == 0,
{
    unimplemented!()
}

/// The token for what `write_bytes(p, 0, n)` just wrote: the `n` bytes at
/// `p` read as zero. Trusted for the effect of `write_bytes`, which nothing
/// in vstd states. Its specification (`vspec::ptr`) records the call as
/// [`wrote_zero`], which ties the token to a call made with the same `p`
/// and `n`; the caller also shows a raw permission to the bytes, so they
/// are not held typed (written through a `PointsTo`). The fact has no time
/// in it, so a caller must take the token right after the call, before
/// anything else writes the bytes: the one caller is
/// `meta::Meta::alloc_zeroed`, on the line after its `write_bytes`.
///
/// [`wrote_zero`]: crate::vspec::ptr::wrote_zero
#[verifier::external_body]
pub proof fn axiom_wrote_zero(tracked raw: &PointsToRaw, p: *mut u8, n: usize) -> (tracked z: Zeroed)
    requires
        crate::vspec::ptr::wrote_zero(p, n as int),
        raw.contains_range(p@.addr as int, n as int),
        raw.provenance() == p@.provenance,
    ensures
        z.addr() == p@.addr,
        z.len() == n,
{
    unimplemented!()
}

/// Page `q` of a region of `pg`-byte pages at `base` starts at a multiple
/// of `align` (every page does when `align` is at most a page).
pub open spec fn page_aligned(base: int, pg: int, q: int, align: int) -> bool {
    align <= pg || (base + q * pg) % align == 0
}

/// The pages a huge run for `size` bytes takes with its guard page: its
/// usable pages (`size` rounded up to `pg`-byte pages, at least one) and
/// one more.
pub open spec fn run_need(pg: int, size: int) -> int {
    let n = (size + pg - 1) / pg;
    (if n == 0 { 1 } else { n }) + 1
}

/// A huge run for `size` bytes at `align` has a placement in an empty
/// region of `np` pages of `pg` bytes at `base`: its size is representable
/// in whole pages, `align` is at most the region's length, and the run and
/// its guard page fit from some `align`-aligned page (`Huge::placeable`).
pub open spec fn run_placeable(base: int, pg: int, np: int, size: int, align: int) -> bool {
    &&& size + pg - 1 <= usize::MAX
    &&& align <= np * pg
    &&& exists|q: int| 0 <= q && q + run_need(pg, size) <= np && #[trigger] page_aligned(base, pg, q, align)
}

} // verus!
