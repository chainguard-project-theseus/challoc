//! The ghost contents model for zeroed memory: a [`ZeroRaw`] is the
//! permission to bytes that read as zero, from which a word's permission
//! holding 0 is made ([`axiom_zero_word`]).
//! `bitmap::TwoLevelBitmap::from_raw` and `from_raw_chunked` build their
//! views' word permissions from one. A [`Zeroed`] token says a range of
//! bytes reads as zero: `meta::Meta::alloc_zeroed` returns one for the
//! object it hands out.

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
/// so a `ZeroRaw` makes each word's zero permission once. Nothing verified
/// mints one yet: `meta::Meta::reserve_table` and the unverified callers
/// of the bitmap constructors take theirs on trust (`Tracked::assume_new()`
/// outside `verus!`).
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

    /// The provenance of the permission.
    pub closed spec fn provenance(&self) -> Provenance {
        self.raw.provenance()
    }

    /// The bytes are exactly `[start, start + len)`.
    pub open spec fn is_range(&self, start: int, len: int) -> bool {
        Set::range(start, start + len) =~= self.dom()
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

} // verus!
