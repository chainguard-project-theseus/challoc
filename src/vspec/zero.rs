//! The ghost contents model for zeroed memory: a [`Zeroed`] token says a
//! range of bytes reads as zero. `calloc`, `mallocx(MALLOCX_ZERO)` and
//! their internals return one for the memory they hand out
//! (`verus/TODO.md`, "Zeroed memory").

use vstd::prelude::*;
use vstd::raw_ptr::PointsToRaw;
#[cfg(verus_only)]
use vstd::raw_ptr::{PointsTo, Provenance};

verus! {

/// Ghost token: the `len` bytes at address `addr` read as zero.
///
/// A token is made only where the bytes are known to be zero: by the
/// `write_bytes` of [`write_zero`]; from the kernel's contract for fresh
/// and discarded anonymous memory, for the metadata region's committed
/// bytes (`vspec::unbacked::Unbacked::commit`) and a lazily committed
/// table's (`Unbacked::into_lazy`, read only once committed), a zeroed
/// claim's slots
/// (`class::axiom_fresh_slots_zero` and `axiom_discarded_slots_zero`,
/// called by `ClassState::take`) and a huge run's pages
/// (`vspec::sys::axiom_fresh_anon_zero` and `axiom_discarded_zero`, called
/// by `huge::zero_run`); and by splitting and joining tokens (the bytes of
/// a zero range are zero).
/// A token is not a permission to access the bytes (that is
/// `vspec::ptr::valid_writes`), and nothing ties it to the writes the
/// program or the allocator make: it stays true because the allocator
/// hands the bytes to no one while it holds the token, and the only
/// writes verified code makes to such bytes are zero fills and
/// `rallocx`'s copy into a new block's prefix, which `rallocx_zeroed`
/// splits off first. A token means nothing once its memory is handed to
/// the program: the entry points' postconditions state it at the moment
/// of return.
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
/// with the [`Zeroed`] token for its bytes, which [`ZeroRaw::axiom_new`]
/// consumes. The metadata tables the allocator reserves come with one
/// (`meta::Meta::reserve_table`), and a bitmap built on such a table
/// starts from words that read as zero (`bitmap::TwoLevelBitmap::from_raw`
/// and `from_raw_chunked`, through [`axiom_zero_word`]). Like a
/// `PointsToRaw` it splits by byte set ([`ZeroRaw::split`]) and grants
/// access to its bytes (`vspec::ptr::axiom_raw_valid` on
/// [`ZeroRaw::raw`]); it is linear, so a `ZeroRaw` makes each word's
/// zero permission once (its source, [`ZeroRaw::axiom_new`], has a
/// premise of its own).
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

    /// The bytes of `raw`, which `z` reads as zero. Trusted for one
    /// premise nothing checks: `z`'s bytes have not been written since
    /// `z` was made. A `Zeroed` token is not tied to the writes made to
    /// its bytes (its doc), so pairing a token with a permission to bytes
    /// written since (an `into_typed` write, then `leak_contents` and
    /// `into_raw`) would make [`axiom_zero_word`] claim 0 for a word that
    /// holds something else. Its one caller, `meta::zero_raw` in
    /// `Meta::reserve_table`, pairs a table's permission with the token
    /// made with it: an eager table's (`Inner::handed` and
    /// `Inner::handed_zero`, cut from `fresh` and `fresh_zero` together,
    /// fresh memory above the bump pointer that nothing has written) or a
    /// reservation's (both from one `Unbacked::into_lazy`).
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

    /// The permission alone, without the zero fact.
    pub proof fn into_raw(tracked self) -> (tracked r: PointsToRaw)
        ensures
            r.dom() == self.dom(),
            r.provenance() == self.provenance(),
    {
        self.raw
    }

    /// The permission, borrowed (for `vspec::ptr::axiom_raw_valid`).
    pub proof fn raw(tracked &self) -> (tracked r: &PointsToRaw)
        ensures
            r.dom() == self.dom(),
            r.provenance() == self.provenance(),
    {
        &self.raw
    }
}

/// The eight bytes of `z`, which read as zero, as the `u64` at `p`
/// holding 0: `PointsToRaw::into_typed`, initialised with the value the
/// bytes hold. Trusted for that value, which vstd's raw permissions do not
/// track: eight zero bytes are the `u64` 0 in either byte order. The
/// premises are `into_typed`'s (a non-null `p` aligned for `u64` whose
/// eight bytes are exactly `z`'s, of `p`'s provenance), so the permission
/// is one `into_typed` would give; `z` is used up, so the zero fact makes
/// one permission.
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

/// The usable bytes of a huge run for a request of `size` bytes: `size`
/// rounded up to whole pages of the process, at least one page
/// (`Huge::spec_usable` of the process's huge allocator, whose page is the
/// regions' page: `Shared::wf`).
pub open spec fn huge_usable(size: int) -> int {
    let pg = (1usize << crate::layout::process_regions().lg_page) as int;
    let n = (size + pg - 1) / pg;
    (if n == 0 { 1 } else { n }) * pg
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

/// A slot start belongs to one class: class `c`'s region lies inside
/// stride `c` of the superblock (`Regions::separated`).
pub proof fn lemma_slot_unique(c1: int, c2: int, p: usize)
    requires
        crate::layout::process_regions().wf(),
        crate::layout::process_regions().separated(),
        crate::layout::process_regions().is_slot(c1, p),
        crate::layout::process_regions().is_slot(c2, p),
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

/// The usable bytes of a block `p` an allocation of `size` bytes returned:
/// the class's size for a slot of a class (one class per slot:
/// [`lemma_slot_unique`]), [`huge_usable`]`(size)` otherwise.
pub open spec fn block_usable(p: usize, size: int) -> int {
    if exists|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p) {
        crate::layout::class_table()[choose|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p)].size as int
    } else {
        huge_usable(size)
    }
}

/// Zeroed memory (whole-allocator theorem 6, `verus/TODO.md`): what the
/// zeroing entry points' tokens say, byte by byte. A block `p` that an
/// allocation of `size` bytes returned ([`crate::heap::fits`]) with a
/// token that covers its usable bytes from offset `from`
/// ([`covers_usable`]) reads as zero at every byte from `from` to the end
/// of its usable size, which is at least `size`:
///
/// - `mallocx(size, MALLOCX_ZERO)` (`jemalloc::mallocx_zeroed`): `from`
///   is 0, so the whole block, its class's size for a slot and `size`
///   rounded up to whole pages for a huge run.
/// - `rallocx(p, size, MALLOCX_ZERO)` with a new block `q`
///   (`jemalloc::rallocx_zeroed`): `from` is `k`, the bytes copied from
///   `p`, `min(class size, size)` for a slot `p` and 0 for a null `p`;
///   for a huge `p`, `min(old, size)` for an `old` of a run's shape
///   (`vspec::usable::usable_of`).
///
/// `calloc(n, m)` (`libc::calloc_zeroed`) and `challoc_calloc_usable`
/// (`libc::calloc_usable_zeroed`, slab-class requests) state the
/// prefix form: their token [`Zeroed::covers`] the `n * m` bytes at the
/// result, so it [`Zeroed::has`] every byte of `[p, p + n * m)`.
///
/// A token holds at the moment of return: its bytes read as zero then,
/// on the trusted facts the tokens are built from ([`Zeroed`]'s doc, and
/// `verus/TODO.md` for the list).
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
                requires lg < 64;
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

/// `write_bytes(p, 0, n)`, returning the token for what it wrote. Trusted
/// for the one fact `write_bytes`' specification does not state
/// (`vspec::addr`): the `n` bytes at `p` read as zero afterwards.
#[verifier::external_body]
#[inline(always)]
pub unsafe fn write_zero(p: *mut u8, n: usize) -> (z: Tracked<Zeroed>)
    requires
        n == 0 || crate::vspec::ptr::valid_writes(p, n as int),
    ensures
        z@.addr() == p@.addr,
        z@.len() == n,
{
    // SAFETY: the caller's, as for `write_bytes`.
    unsafe { core::ptr::write_bytes(p, 0, n) };
    Tracked::assume_new()
}

} // verus!
