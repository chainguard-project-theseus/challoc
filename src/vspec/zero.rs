//! The ghost contents model for zeroed memory: a [`ZeroRaw`] is the
//! permission to bytes that read as zero, from which a word's permission
//! holding 0 is made ([`axiom_zero_word`]).
//! `bitmap::TwoLevelBitmap::from_raw` and `from_raw_chunked` build their
//! views' word permissions from one. A [`Zeroed`] token says a range of
//! bytes reads as zero: `meta::Meta::alloc_zeroed` returns one for the
//! object it hands out, from [`zero_raw`] for a reused one.

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

    /// Byte `a` is one of the token's: it reads as zero.
    pub open spec fn has(&self, a: int) -> bool {
        self.addr() <= a < self.addr() + self.len()
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
/// is [`ZeroRaw::axiom_new`], whose doc lists its callers; the
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

    /// The permission, borrowed (for `vspec::ptr::axiom_raw_readable`).
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
    /// `huge::table::Table::storage` pairs the permission and the token
    /// `meta::Meta::alloc_zeroed` returns for one object, which by that
    /// function's body were not written since the token was made: a reused
    /// object's token comes from the `zero_raw` that cleared it, and a
    /// fresh one's from the commit of memory above the bump pointer, which
    /// nothing has written; `storage` writes nothing before the pairing.
    ///
    /// Checked by `crate::tests::trusted_mint_sites_are_pinned`, which pins its
    /// call sites to the ones named here: a new one is named here, with its
    /// premise, and added to that test's list in the same change.
    #[verifier::external_body]
    pub proof fn axiom_new(tracked raw: PointsToRaw, tracked z: Zeroed) -> (tracked r: ZeroRaw)
        requires
            forall|a: int| #[trigger] raw.dom().contains(a) ==> z.has(a),
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

    /// The permission, giving up the zero token.
    pub proof fn into_raw(tracked self) -> (tracked r: PointsToRaw)
        ensures
            r.dom() == self.dom(),
            r.provenance() == self.provenance(),
    {
        self.raw
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

/// The usable bytes of a huge run for a request of `size` bytes: `size`
/// rounded up to whole pages of the process, at least one page
/// (`Huge::spec_usable` of the process's huge allocator, whose page is the
/// regions' page: `Shared::wf`).
pub open spec fn huge_usable(size: int) -> int {
    let pg = (1usize << crate::layout::process_regions().lg_page) as int;
    let n = (size + pg - 1) / pg;
    (if n == 0 { 1 } else { n }) * pg
}

/// The usable bytes of a block `p` an allocation of `size` bytes returned:
/// the class's size for a slot of a class (one class per slot:
/// [`lemma_slot_unique`]), [`huge_usable`]`(size)` otherwise. For a live
/// block this is [`Live::len`](crate::heap::Live::len) when the size
/// rounds to the run's usable size (`Live::lemma_block_usable`).
pub open spec fn block_usable(p: usize, size: int) -> int {
    if exists|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p) {
        crate::layout::class_table()[choose|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p)].size as int
    } else {
        huge_usable(size)
    }
}

/// [`run_placeable`] for the process's huge region (`Shared::wf`: the
/// huge allocator's base, page and page count are the regions').
pub open spec fn huge_placeable(size: usize, align: usize) -> bool {
    let r = crate::layout::process_regions();
    run_placeable(r.huge_base as int, (1usize << r.lg_page) as int, (r.huge_len >> r.lg_page) as int, size as int, align as int)
}

/// `z` reads block `p`'s usable bytes from offset `from` to the block's
/// end as zero, for a block an allocation of `size` bytes returned: the
/// end is the class's size for a slot of a class
/// ([`Regions::is_slot`](crate::layout::Regions::is_slot), one class per
/// slot: [`lemma_slot_unique`]) and [`huge_usable`]`(size)` for a page
/// start of the huge region.
pub open spec fn covers_usable(z: Zeroed, p: *mut u8, from: int, size: int) -> bool {
    &&& z.addr() == p@.addr + from
    &&& forall|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr) ==> from + z.len() >= crate::layout::class_table()[c].size
    &&& crate::layout::process_regions().is_huge_block(p@.addr, 1) ==> from + z.len() >= huge_usable(size)
}

/// A slot address (a guard slab's too) belongs to one class: class `c`'s
/// region lies inside stride `c` of the superblock (`Regions::separated`).
pub proof fn lemma_slot_unique(c1: int, c2: int, p: usize)
    requires
        crate::layout::process_regions().wf(),
        crate::layout::process_regions().separated(),
        crate::layout::process_regions().is_slot_addr(c1, p),
        crate::layout::process_regions().is_slot_addr(c2, p),
    ensures
        c1 == c2,
{
    let r = crate::layout::process_regions();
    let stride = (1usize << r.lg_stride) as int;
    r.lemma_slot_in_region(c1, p);
    r.lemma_slot_in_region(c2, p);
    crate::vspec::class::axiom_class_table_wf(c1);
    crate::vspec::class::axiom_class_table_wf(c2);
    assert(r.superblock + c1 * stride <= p < r.superblock + (c1 + 1) * stride);
    assert(r.superblock + c2 * stride <= p < r.superblock + (c2 + 1) * stride);
    if c1 < c2 {
        assert((c1 + 1) * stride <= c2 * stride) by (nonlinear_arith)
            requires
                c1 + 1 <= c2,
                stride >= 0,
        ;
    } else if c2 < c1 {
        assert((c2 + 1) * stride <= c1 * stride) by (nonlinear_arith)
            requires
                c2 + 1 <= c1,
                stride >= 0,
        ;
    }
}

/// A slot address (a guard slab's too) is no page start of the huge
/// region: its class region lies in the superblock, which
/// `Regions::separated` keeps apart from the huge region.
pub proof fn lemma_slot_not_huge(c: int, p: usize)
    requires
        crate::layout::process_regions().wf(),
        crate::layout::process_regions().separated(),
        crate::layout::process_regions().is_slot_addr(c, p),
    ensures
        !crate::layout::process_regions().is_huge_block(p, 1),
{
    let r = crate::layout::process_regions();
    let stride = (1usize << r.lg_stride) as int;
    r.lemma_slot_in_region(c, p);
    crate::vspec::class::axiom_class_table_wf(c);
    assert(r.superblock + c * stride <= r.class_base[c]);
    assert((c + 1) * stride <= crate::layout::NCLASSES * stride) by (nonlinear_arith)
        requires
            c < crate::layout::NCLASSES,
            stride >= 0,
    ;
}

/// [`covers_usable`] for a slot of class `c` whose token covers the
/// class's size from offset `from`.
pub proof fn lemma_covers_slot(z: Zeroed, p: *mut u8, c: int, from: int, size: int)
    requires
        crate::layout::process_regions().wf(),
        crate::layout::process_regions().separated(),
        crate::layout::process_regions().is_slot(c, p@.addr),
        z.addr() == p@.addr + from,
        from + z.len() >= crate::layout::class_table()[c].size,
    ensures
        covers_usable(z, p, from, size),
{
    assert forall|c2: int| #[trigger] crate::layout::process_regions().is_slot(c2, p@.addr) implies from + z.len() >= crate::layout::class_table()[c2].size by {
        lemma_slot_unique(c, c2, p@.addr);
    }
    lemma_slot_not_huge(c, p@.addr);
}

/// [`covers_usable`] for a page start of the huge region whose token
/// covers [`huge_usable`]`(size)` from offset `from`.
pub proof fn lemma_covers_huge(z: Zeroed, p: *mut u8, from: int, size: int)
    requires
        crate::layout::process_regions().wf(),
        crate::layout::process_regions().separated(),
        crate::layout::process_regions().is_huge_block(p@.addr, 1),
        z.addr() == p@.addr + from,
        from + z.len() >= huge_usable(size),
    ensures
        covers_usable(z, p, from, size),
{
    assert forall|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr) implies from + z.len() >= crate::layout::class_table()[c].size by {
        lemma_slot_not_huge(c, p@.addr);
    }
}

/// Zeroed memory: what a zeroing entry point's token says, byte by byte.
/// A block `p` that an allocation of `size` bytes returned
/// ([`crate::heap::fits`]) with a token that covers its usable bytes from
/// offset `from` ([`covers_usable`]) reads as zero at every byte from
/// `from` to the end of its usable size ([`block_usable`]), which is at
/// least `size`. `challoc_calloc_usable` (`libc::calloc_usable_zeroed`)
/// returns such a token with `from` 0, so the whole block reads as zero:
/// its class's size for a slot, and `n * m` rounded up to whole pages for
/// a huge run.
///
/// A token holds at the moment of return: its bytes read as zero then, on
/// the trusted facts the tokens are built from ([`Zeroed`]'s doc).
pub proof fn theorem_zeroed_memory(z: Zeroed, p: *mut u8, from: int, size: int)
    requires
        crate::heap::fits(p, size, 1),
        p@.addr != 0,
        covers_usable(z, p, from, size),
    ensures
        size <= block_usable(p@.addr, size),
        forall|a: int| p@.addr + from <= a < p@.addr + block_usable(p@.addr, size) ==> #[trigger] z.has(a),
{
    let r = crate::layout::process_regions();
    let q = p@.addr;
    if exists|c: int| #[trigger] r.is_slot(c, q) {
        let c = choose|c: int| #[trigger] r.is_slot(c, q);
        assert(r.is_slot(c, q));
        assert(r.user_block(q, size));
        if exists|c2: int| #[trigger] r.is_slot(c2, q) && size <= crate::layout::class_table()[c2].size {
            let c2 = choose|c2: int| #[trigger] r.is_slot(c2, q) && size <= crate::layout::class_table()[c2].size;
            lemma_slot_unique(c, c2, q);
        } else {
            // The huge half of `user_block`: no slot is a huge page start.
            lemma_slot_not_huge(c, q);
        }
    } else {
        assert(r.is_huge_block(q, if size < 1 { 1 } else { size }));
        assert(r.is_huge_block(q, 1));
        let pg = (1usize << r.lg_page) as int;
        assert(pg > 0) by {
            let lg = r.lg_page;
            assert((1usize << lg) > 0) by (bit_vector)
                requires
                    lg < 64,
            ;
        }
        let n = (size + pg - 1) / pg;
        assert(size <= (if n == 0 { 1 } else { n }) * pg) by {
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(size + pg - 1, pg);
            vstd::arithmetic::div_mod::lemma_mod_bound(size + pg - 1, pg);
            if n == 0 {
                assert(size + pg - 1 < pg) by (nonlinear_arith)
                    requires
                        size + pg - 1 == pg * n + (size + pg - 1) % pg,
                        n == 0,
                        (size + pg - 1) % pg < pg,
                ;
            } else {
                assert(size <= n * pg) by (nonlinear_arith)
                    requires
                        size + pg - 1 == pg * n + (size + pg - 1) % pg,
                        (size + pg - 1) % pg < pg,
                ;
            }
        }
    }
}

/// `write_bytes(p, 0, n)` through `raw`, the permission to the `n` bytes
/// at `p`, taken `&mut` so that no other view of them (a `PointsTo` held
/// from the same bytes, a [`ZeroRaw`] or a `UserMem` lending them) is
/// held across the fill; the token for what it wrote. Trusted: the
/// `core` contract of `write_bytes` for `u8` (`p` non-null; the bytes
/// valid for writes, which the permission, held `&mut`, stands for: the
/// read-write memory `vspec::ptr::axiom_raw_readable` describes, with
/// that axiom's premise, which a caller keeps too: a lazily committed
/// table's bytes only once their page is committed) and its effect,
/// which nothing in vstd states: the `n` bytes read as zero afterwards.
/// The permission names no contents, so it is unchanged.
#[verifier::external_body]
#[inline(always)]
pub unsafe fn zero_raw(p: *mut u8, n: usize, Tracked(raw): Tracked<&mut PointsToRaw>) -> (z: Tracked<Zeroed>)
    requires
        p@.addr != 0,
        old(raw).contains_range(p@.addr as int, n as int),
        old(raw).provenance() == p@.provenance,
    ensures
        *final(raw) == *old(raw),
        z@.addr() == p@.addr,
        z@.len() == n,
{
    // SAFETY: the permission makes the `n` bytes at `p` writable.
    unsafe { core::ptr::write_bytes(p, 0, n) };
    Tracked::assume_new()
}

} // verus!
