//! Metadata region allocator (design.md sections 2, 6.2 and 17).
//!
//! Everything the allocator has to remember lives in the metadata region
//! reserved by `layout::Regions`: slab descriptor arrays and slot bitmaps,
//! the huge allocator's tables, thread heaps and their caches, the
//! background thread's stack. None of it may come from `malloc`, so this is
//! the one module that carves that region up, from both ends:
//!
//! - **Reservations** ([`Meta::reserve`], [`Meta::reserve_table`]) are
//!   large, fixed-address arrays that are never freed. They are carved
//!   downward from the top of the region and handed back uncommitted; the
//!   owner calls [`Meta::commit`] on the pages it starts to use
//!   ([`Meta::reserve_committed`] commits the whole range at once).
//! - **Objects** ([`Meta::alloc`] / [`Meta::free`]) are small: heaps, cache
//!   slot stacks, node blocks, set tables, huge descriptors. They come from
//!   a bump pointer growing upward from the base, committed ahead of the
//!   bump in [`COMMIT_CHUNK`] steps, and freed objects go on one of
//!   [`NBINS`] size-classed free lists.
//!
//! Committing is `mprotect(PROT_READ | PROT_WRITE)` on the `PROT_NONE`
//! reservation, so committed but untouched metadata costs address space and
//! overcommit charge, never RSS. Nothing is ever uncommitted.
//!
//! A table of at most [`EAGER_MAX`] bytes is carved from the object side
//! instead, page aligned and writable as a whole, so it merges with the
//! objects into the one mapping that grows from the base rather than adding
//! VMAs of its own that every `fork`, its faults and its exit pay for
//! (design 2). Its owner still calls [`Meta::commit`], which then makes the
//! object side writable through every table carved so far, if it is not
//! already, and otherwise only counts. A carve itself makes no system call:
//! the object side's commit is deferred to the next object or table commit
//! that needs it, so the dozens of tables init carves back to back cost one
//! `mprotect` rather than one each (#29). Larger tables stay on the
//! reservation side: making them
//! writable whole would put never-touched address space into core dumps,
//! `mlockall` and `RLIMIT_DATA`. Eager carving is off when the kernel
//! charges writable bytes against its commit limit (`vm.overcommit_memory=2`)
//! or when the region can take PMD-sized pages (any `thp` key but `never`);
//! both are probed on the first table.
//!
//! All state sits behind one [`Lock`], the metadata lock of design section
//! 9. None of this is on the `malloc`/`free` fast path.

use core::alloc::Layout;
use core::ptr;

use crate::lock::{Lock, RawLock};
use crate::sys::Errno;
#[cfg(verus_keep_ghost)]
use crate::vspec::layout::{layout_align, layout_size};
#[cfg(verus_keep_ghost)]
use vstd::arithmetic::power2::pow2;
#[cfg(verus_keep_ghost)]
use vstd::bits::lemma_usize_shl_is_mul;
use vstd::prelude::*;
#[cfg(verus_keep_ghost)]
use vstd::std_specs::bits::{axiom_u64_leading_zeros, u64_leading_zeros};

verus! {

/// Alignment of every object; the smallest bin is one quantum of 16.
pub const MIN_ALIGN: usize = 16;
/// Objects of 128 bytes or more are line aligned and line sized so heaps and
/// caches of different threads never share a line. 128, not 64: Apple
/// silicon has 128-byte lines and Intel's adjacent-line prefetcher pairs
/// 64-byte lines (the same unit as `lock::PAD_ALIGN`).
pub const CACHE_LINE: usize = 128;

} // verus!

const _: () = assert!(CACHE_LINE == crate::lock::PAD_ALIGN);

verus! {

/// Largest alignment [`Meta::alloc`] accepts. Anything needing more (a
/// thread stack) is a reservation.
pub const MAX_ALIGN: usize = CACHE_LINE;
/// Largest object [`Meta::alloc`] serves. Bigger things are reservations.
pub const LG_MAX_BINNED: u32 = 20;
pub const MAX_BINNED: usize = 1 << LG_MAX_BINNED;
/// Bins: 16, 32, 48, 64, 128, 256, then `1.5 * 2^(k-1)` and `2^k` for `k`
/// in `9..=LG_MAX_BINNED`. Every bin from 128 up is a multiple of the cache
/// line (which is why there is no 96 or 192), and consecutive bins differ by
/// at most 2x.
pub const NBINS: usize = 6 + 2 * (LG_MAX_BINNED as usize - 8);
/// Granularity of bump-area commits. Only address space and overcommit
/// charge, not RSS, so a coarse step keeps `mprotect` calls rare.
pub const COMMIT_CHUNK: usize = 64 * 1024;
/// Largest table [`Meta::reserve_table`] carves from the object side,
/// fully writable, instead of reserving it uncommitted at the top (the
/// module doc). At the preferred layout (2^38 per class) the owner arrays
/// of every class and every table of the medium classes are at most
/// 8 MiB (56 of 167 tables, 82 MiB together); the set tables are 96 KiB
/// to 49 MiB; the small classes' descriptor arrays are 64 MiB to 256 MiB
/// and their slot bitmaps 128 MiB to 2 GiB. 8 MiB takes the first group
/// and bounds the writable-but-untouched address space a process carries
/// to under 100 MiB; 16 MiB would take 45 more tables for 450 MiB.
pub const EAGER_MAX: usize = 8 << 20;

/// Size of bin `b` (the doc of [`NBINS`]): 16, 32, 48, 64, 128, 256, then
/// `3 << (7 + j)` and `1 << (9 + j)` for `j` in `0..=11`.
pub open spec fn bin_size(b: int) -> int {
    if b < 4 {
        16 * (b + 1)
    } else if b < 6 {
        128 * (b - 3)
    } else if (b - 6) % 2 == 0 {
        (3usize << ((7 + (b - 6) / 2) as usize)) as int
    } else {
        (1usize << ((9 + (b - 6) / 2) as usize)) as int
    }
}

/// [`bin_align`].
pub open spec fn bin_align_spec(b: int) -> int {
    if b < 4 { 16 } else { 128 }
}

/// A bin's size is a multiple of its alignment, between 16 and
/// `MAX_BINNED`, and above the previous bin's.
pub open spec fn bin_facts(b: int) -> bool {
    &&& 16 <= bin_size(b) <= 0x10_0000
    &&& bin_size(b) % bin_align_spec(b) == 0
    &&& b > 0 ==> bin_size(b - 1) < bin_size(b)
}

spec fn bins_ok(n: nat) -> bool
    decreases n,
{
    n == 0 || (bins_ok((n - 1) as nat) && bin_facts(n - 1))
}

proof fn lemma_bins_ok(n: nat, b: int)
    requires
        bins_ok(n),
        0 <= b < n,
    ensures
        bin_facts(b),
    decreases n,
{
    if b < n - 1 {
        lemma_bins_ok((n - 1) as nat, b);
    }
}

pub proof fn lemma_bin(b: int)
    requires
        0 <= b < NBINS,
    ensures
        bin_facts(b),
{
    assert(bins_ok(30)) by (compute_only);
    lemma_bins_ok(30, b);
}

/// Bin sizes grow with the bin.
pub proof fn lemma_bin_mono(a: int, b: int)
    requires
        0 <= a <= b < NBINS,
    ensures
        bin_size(a) <= bin_size(b),
    decreases b - a,
{
    if a < b {
        lemma_bin(b);
        lemma_bin_mono(a, b - 1);
    }
}

/// `b` is the smallest bin whose size is at least `size` ([`bin_of`]).
pub open spec fn is_bin_of(size: int, b: int) -> bool {
    &&& 0 <= b < NBINS
    &&& size <= bin_size(b)
    &&& b == 0 || bin_size(b - 1) < size
}

/// [`bin_of`] as a function (of a size it serves).
pub open spec fn bin_of_spec(size: int) -> int {
    choose|b: int| is_bin_of(size, b)
}

/// [`bin_for`] as a function (of a layout it serves).
pub open spec fn bin_for_spec(layout: Layout) -> int {
    bin_for_sa(layout_size(layout) as int, layout_align(layout) as int)
}

/// [`bin_for_spec`] of a layout of `size` bytes aligned to `align`.
pub open spec fn bin_for_sa(size: int, align: int) -> int {
    let b = bin_of_spec(size);
    if align > MIN_ALIGN && b < 4 { 4 } else { b }
}

/// A size has one smallest bin.
proof fn lemma_bin_of_unique(size: int, b: int)
    requires
        is_bin_of(size, b),
    ensures
        bin_of_spec(size) == b,
{
    let c = bin_of_spec(size);
    assert(is_bin_of(size, c));
    lemma_bin(b);
    lemma_bin(c);
    if c < b {
        lemma_bin_mono(c, b - 1);
    } else if b < c {
        lemma_bin_mono(b, c - 1);
    }
}

/// `x` is a power of two (what `next_power_of_two` returns, `vspec::core_int`).
pub open spec fn is_pow2(x: usize) -> bool {
    exists|k: nat| k < 64 && x as nat == #[trigger] pow2(k)
}

/// `1 << k` is a power of two.
pub proof fn lemma_is_pow2_shl(k: usize)
    requires
        k < 64,
    ensures
        is_pow2(1usize << k),
{
    vstd::bits::lemma_usize_pow2_no_overflow(k as nat);
    lemma_usize_shl_is_mul(1, k);
    assert((1usize << k) as nat == pow2(k as nat));
}

/// [`align_up`] as a function.
pub open spec fn align_up_spec(x: usize, a: usize) -> usize {
    ((x + a - 1) as usize) & !((a - 1) as usize)
}

/// Bin sizes, ascending.
pub exec static BINS: [u32; NBINS]
    ensures
        forall|i: int| 0 <= i < NBINS ==> BINS[i] as int == #[trigger] bin_size(i),
{
    build_bins()
}

const fn build_bins() -> (r: [u32; NBINS])
    ensures
        forall|i: int| 0 <= i < NBINS ==> r[i] as int == #[trigger] bin_size(i),
{
    let mut b = [0u32; NBINS];
    b[0] = 16;
    b[1] = 32;
    b[2] = 48;
    b[3] = 64;
    b[4] = 128;
    b[5] = 256;
    let mut i = 6;
    let mut lg = 9;
    while lg <= LG_MAX_BINNED
        invariant
            9 <= lg <= LG_MAX_BINNED + 1,
            i == 6 + 2 * (lg - 9),
            forall|j: int| 0 <= j < i ==> b[j] as int == #[trigger] bin_size(j),
        decreases LG_MAX_BINNED + 1 - lg,
    {
        proof {
            let s = (lg - 2) as u32;
            assert((3u32 << s) as usize == 3usize << (s as usize)) by (bit_vector)
                requires
                    s <= 18,
            ;
            assert((1u32 << lg) as usize == 1usize << (lg as usize)) by (bit_vector)
                requires
                    lg <= 20,
            ;
            assert((i as int - 6) / 2 == lg as int - 9);
            assert((i as int + 1 - 6) / 2 == lg as int - 9);
        }
        b[i] = 3 << (lg - 2);
        b[i + 1] = 1 << lg;
        i += 2;
        lg += 1;
    }
    assert!(i == NBINS);
    b
}

/// Smallest bin whose size is at least `size`; `None` above `MAX_BINNED`.
#[inline]
pub const fn bin_of(size: usize) -> (r: Option<usize>)
    ensures
        r is None <==> size > MAX_BINNED,
        r matches Some(b) ==> b < NBINS && size <= bin_size(b as int) && (b == 0 || bin_size(b - 1) < size),
{
    proof {
        lemma_consts();
    }
    if size <= 48 {
        return Some(if size == 0 { 0 } else { (size - 1) / 16 });
    }
    if size > MAX_BINNED {
        return None;
    }
    if size <= 64 {
        return Some(3);
    }
    if size <= 128 {
        return Some(4);
    }
    if size <= 256 {
        return Some(5);
    }
    // `size` is in `(2^lg, 2^(lg+1)]` with `lg >= 8`; bin `6 + 2 * (lg - 8)`
    // holds `1.5 * 2^lg` and the next holds `2^(lg+1)`.
    let ghost m = (size - 1) as usize;
    proof {
        let mu = m as u64;
        axiom_u64_leading_zeros(mu);
        let lz = u64_leading_zeros(mu) as u64;
        assert(44 <= lz <= 55 && (1u64 << sub(63u64, lz)) <= mu && mu < (1u64 << sub(64u64, lz))) by (bit_vector)
            requires
                256 <= mu < 0x10_0000,
                lz <= 64,
                lz < 64 ==> (mu >> sub(63u64, lz)) & 1u64 != 0u64,
                mu >> sub(64u64, lz) == 0,
        ;
        let l = sub(63u64, lz) as usize;
        assert((1usize << l) <= m && m < (1usize << add(l, 1))) by (bit_vector)
            requires
                l <= 19,
                mu == m as u64,
                l as u64 == sub(63u64, lz),
                (1u64 << sub(63u64, lz)) <= mu,
                mu < (1u64 << sub(64u64, lz)),
                lz <= 55,
        ;
    }
    let lg = (usize::BITS - 1 - (size - 1).leading_zeros()) as usize;
    let base = 6 + 2 * (lg - 8);
    proof {
        assert((base - 6) / 2 == lg - 8);
        assert((base + 1 - 6) / 2 == lg - 8);
        if lg > 8 {
            assert((base - 1 - 6) / 2 == lg - 9);
        } else {
            assert(128 * 2 == 1usize << 8usize) by (compute_only);
        }
    }
    if size <= (3 << (lg - 1)) { Some(base) } else { Some(base + 1) }
}

/// Alignment every object of bin `bin` has: 16 up to the 64-byte bin, a
/// full line from 128 up.
#[inline]
pub const fn bin_align(bin: usize) -> (r: usize)
    ensures
        r == bin_align_spec(bin as int),
{
    if bin < 4 { MIN_ALIGN } else { CACHE_LINE }
}

/// Bin serving a `Layout`: the size's bin, promoted to a cache-line bin when
/// the alignment asks for more than 16. `None` if the size is too big.
#[inline]
pub const fn bin_for(layout: Layout) -> (r: Option<usize>)
    ensures
        r is None <==> layout_size(layout) > MAX_BINNED,
        r matches Some(b) ==> {
            &&& b < NBINS
            &&& layout_size(layout) <= bin_size(b as int)
            &&& layout_align(layout) > MIN_ALIGN ==> b >= 4
        },
        r matches Some(b) ==> b as int == bin_for_spec(layout),
{
    let r0 = bin_of(layout.size());
    proof {
        if let Some(b) = r0 {
            lemma_bin_of_unique(layout_size(layout) as int, b as int);
        }
    }
    match r0 {
        Some(b) if layout.align() > MIN_ALIGN && b < 4 => Some(4),
        other => other,
    }
}

} // verus!

/// Counters for `stats.metadata` and diagnostics. Identities:
/// `allocated + eager <= bumped <= writable <= len`, `committed <=
/// writable` and `bumped + reserved <= len` (`committed` counts the bump
/// chunks taken for objects and the pages the owners of tables asked for,
/// on either side; `writable` is what is actually readable and writable,
/// which on the object side includes the tails of the eagerly carved
/// tables).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MetaStats {
    /// Length of the whole region.
    pub len: usize,
    /// Bytes in use at page granularity: object bump chunks plus the pages
    /// table owners have committed (`stats.metadata`, the `resident`
    /// identity of `stats.rs`; the same figure whichever side a table sits
    /// on).
    pub committed: usize,
    /// Bytes in live objects, each rounded to its bin.
    pub allocated: usize,
    /// Bytes the bump pointer has passed, including padding, freed objects
    /// and the tables carved from the object side.
    pub bumped: usize,
    /// Bytes handed out by `reserve` on the reservation side, committed or
    /// not.
    pub reserved: usize,
    /// Bytes readable and writable: the object side's chunked frontier
    /// plus the reservation side's commits (the frontier includes tables
    /// carved but not yet used, whose commit is deferred to their first
    /// use).
    pub writable: usize,
    /// Bytes of tables carved from the object side (`reserve_table`,
    /// `reserve_committed` up to `EAGER_MAX`), all writable.
    pub eager: usize,
}

struct FreeNode {
    next: *mut FreeNode,
}

/// Whether small tables may be carved from the object side (the module
/// doc): decided on the first table from the kernel's overcommit mode and
/// the THP policy in force, never assumed. A `u8` with three values, not
/// an enum: an enum's niche makes rustc regroup every field of `Inner`
/// and the offsets the inlined object paths use would move (an enum here
/// changes the machine code of `Meta::free` and `claim_with`).
const EAGER_UNPROBED: u8 = 0;
const EAGER_ON: u8 = 1;
const EAGER_OFF: u8 = 2;

struct Inner {
    base: usize,
    len: usize,
    page: usize,
    /// Next byte for objects and eagerly carved tables. `base <= bump <=
    /// frontier <= top` and `committed <= frontier`.
    bump: usize,
    /// Bump area is writable below here; always page aligned.
    committed: usize,
    /// Reservations occupy `[top, base + len)`; always page aligned.
    top: usize,
    free: [*mut FreeNode; NBINS],
    stats: MetaStats,
    // The fields below sit after the fields the inlined object paths
    // (`Meta::alloc` in `claim_with`, `Meta::free`) address, so those
    // offsets, and their machine code, do not depend on them. New fields
    // go here too.
    /// Bytes committed on the reservation side (`commit` above `top`).
    top_committed: usize,
    /// One of `EAGER_UNPROBED`, `EAGER_ON`, `EAGER_OFF`.
    eager: u8,
    /// The object side's chunked frontier: everything below it counts as
    /// committed, and it is writable below `committed`. The two differ
    /// only while eagerly carved tables wait for their deferred commit
    /// ([`Inner::flush`]); page aligned.
    frontier: usize,
}

// SAFETY: the raw pointers are addresses inside the region, meaningful in
// any thread; the `Lock` serialises every access.
unsafe impl Send for Inner {}

/// The metadata allocator for one region. One per process: `heap/`
/// writes it in place with [`Meta::new`] when the regions are reserved.
pub struct Meta {
    inner: Lock<Inner>,
}

verus! {

#[inline]
const fn align_up(x: usize, a: usize) -> (r: usize)
    requires
        a > 0,
        a & sub(a, 1) == 0,
        x + a <= usize::MAX,
    ensures
        r == align_up_spec(x, a),
        x <= r,
        r - x < a,
        r % a == 0,
        x % a == 0 ==> r == x,
{
    proof {
        lemma_align_bits(x, a, (x + a - 1) as usize);
    }
    (x + a - 1) & !(a - 1)
}

/// Rounding `y = x + a - 1` down to a multiple of `a` rounds `x` up.
pub(crate) proof fn lemma_align_bits(x: usize, a: usize, y: usize)
    requires
        a > 0,
        a & sub(a, 1) == 0,
        y == x + (a - 1),
    ensures
        y & !sub(a, 1) <= y,
        y - (y & !sub(a, 1)) <= a - 1,
        (y & !sub(a, 1)) % a == 0,
        x % a == 0 ==> y & !sub(a, 1) == x,
{
    assert(y & !sub(a, 1) <= y) by (bit_vector);
    assert(sub(y, y & !sub(a, 1)) == y & sub(a, 1)) by (bit_vector);
    assert(y & sub(a, 1) <= sub(a, 1)) by (bit_vector);
    let r = y & !sub(a, 1);
    assert(r & sub(a, 1) == 0) by (bit_vector)
        requires
            r == y & !sub(a, 1),
    ;
    lemma_mod_mask(r, a);
    if x % a == 0 {
        lemma_mod_mask(x, a);
        assert(y & !sub(a, 1) == x) by (bit_vector)
            requires
                x & sub(a, 1) == 0,
                y == add(x, sub(a, 1)),
                y >= x,
        ;
    }
}

/// For a power of two `a`, `x % a` is the low bits of `x`.
proof fn lemma_mod_mask(x: usize, a: usize)
    requires
        a > 0,
        a & sub(a, 1) == 0,
    ensures
        x % a == x & sub(a, 1),
{
    assert(x % a == x & sub(a, 1)) by (bit_vector)
        requires
            a > 0,
            a & sub(a, 1) == 0,
    ;
}

/// The constants' values.
pub(crate) proof fn lemma_consts()
    ensures
        MAX_BINNED == 0x10_0000,
        NBINS == 30,
        EAGER_MAX == 0x80_0000,
{
    assert(MAX_BINNED == 0x10_0000) by (compute_only);
    assert(EAGER_MAX == 0x80_0000) by (compute_only);
}

} // verus!

/// `PROT_READ | PROT_WRITE`.
const PROT_RW: i32 = 3;

const _: () = assert!(PROT_RW == crate::sys::PROT_READ | crate::sys::PROT_WRITE);

fn commit_range(addr: usize, len: usize) -> Result<(), Errno> {
    // SAFETY: the range is inside our own PROT_NONE reservation and nothing
    // is mapped over it; adding permissions cannot invalidate anything.
    unsafe { crate::sys::mprotect(addr, len, PROT_RW) }
}

impl Inner {
    const fn new(base: usize, len: usize, page: usize) -> Inner {
        assert!(
            page.is_power_of_two() && page >= MIN_ALIGN,
            "meta: bad page size"
        );
        assert!(
            base % page == 0 && len % page == 0 && len > 0,
            "meta: region not page aligned"
        );
        assert!(base.checked_add(len).is_some(), "meta: region wraps");
        Inner {
            base,
            len,
            page,
            bump: base,
            committed: base,
            top: base + len,
            free: [ptr::null_mut(); NBINS],
            stats: MetaStats {
                len,
                committed: 0,
                allocated: 0,
                bumped: 0,
                reserved: 0,
                writable: 0,
                eager: 0,
            },
            top_committed: 0,
            eager: EAGER_UNPROBED,
            frontier: base,
        }
    }

    /// The object side's chunked frontier for a bump end of `end`, never
    /// past `top`. Chunks are measured from the base: the region is only
    /// page aligned, so rounding the absolute address would make the first
    /// chunk short and every later one misaligned against the counter.
    fn chunk_end(&self, end: usize) -> usize {
        debug_assert!(end <= self.top);
        let step = align_up(COMMIT_CHUNK, self.page);
        (self.base + align_up(end - self.base, step)).min(self.top)
    }

    /// Make the bump area writable through `end`, in `COMMIT_CHUNK` steps,
    /// never past `top`, together with any deferred table carves below the
    /// frontier, in one `mprotect`. Returns the bytes the frontier moved;
    /// the caller decides how much of that counts as `committed` (an
    /// object's chunk does whole). On failure nothing moves.
    fn commit_bump_to(&mut self, end: usize) -> Result<usize, Errno> {
        let want = self.chunk_end(end).max(self.frontier);
        if want > self.committed {
            commit_range(self.committed, want - self.committed)?;
            // Metadata THP (`thp.rs`): the chunk is the folio's size or a
            // multiple of it (the region base is `MAX_SLAB`-aligned), so
            // the first touch of a heap or cache maps one folio.
            if crate::thp::meta_folio_size() != 0 {
                crate::thp::advise_meta(self.committed, want - self.committed);
            }
            self.committed = want;
        }
        let delta = want - self.frontier;
        self.frontier = want;
        Ok(delta)
    }

    /// Make the object side writable through the frontier: the deferred
    /// commit of the tables carved since the last one (the module doc).
    fn flush(&mut self) -> Result<(), Errno> {
        if self.frontier > self.committed {
            self.commit_bump_to(self.frontier)?;
        }
        Ok(())
    }

    /// Whether a table of `len` bytes goes on the object side (the module
    /// doc): small enough, and the kernel neither charges writable bytes
    /// against a commit limit nor may back the region with PMD pages.
    fn eager_ok(&mut self, len: usize) -> bool {
        if len > EAGER_MAX {
            return false;
        }
        if self.eager == EAGER_UNPROBED {
            self.eager = if eager_policy() { EAGER_ON } else { EAGER_OFF };
        }
        self.eager == EAGER_ON
    }

    /// Carve a page-aligned, never freed table of `len` bytes aligned to
    /// `align` from the object side, writable as a whole once [`flush`]
    /// has run (the owner's first [`commit`], or the next object, does
    /// that). `None` when it would reach the reservation side (the caller
    /// then falls back to a reservation). No system call.
    ///
    /// [`flush`]: Inner::flush
    /// [`commit`]: Inner::commit
    fn reserve_eager(&mut self, len: usize, align: usize) -> Option<usize> {
        let align = align.max(self.page);
        let len = align_up(len.max(1), self.page);
        let start = align_up(self.bump, align);
        let end = start.checked_add(len)?;
        if end > self.top {
            return None;
        }
        let want = self.chunk_end(end);
        let delta = want.saturating_sub(self.frontier);
        self.frontier = self.frontier.max(want);
        // The object side counts as committed everything below its frontier
        // except the tables carved from it (their owners' `commit` calls
        // count their pages as they use them): the chunk's slack around a
        // table, like an object's padding, is in; the table's bytes, new or
        // carved out of slack already counted, are out. `committed + delta
        // >= len` because the table lies inside the frontier and overlaps no
        // other table.
        self.stats.committed = self.stats.committed + delta - len;
        // Fresh memory above the old bump pointer: zero, never written.
        self.bump = end;
        self.stats.bumped = end - self.base;
        self.stats.eager += len;
        Some(start)
    }

    fn alloc(&mut self, bin: usize) -> *mut u8 {
        let size = BINS[bin] as usize;
        let head = self.free[bin];
        if !head.is_null() {
            // SAFETY: `head` came from `free`, so it is a live block of at
            // least `size` bytes inside the bump area holding a `FreeNode`.
            self.free[bin] = unsafe { (*head).next };
            self.stats.allocated += size;
            return head as *mut u8;
        }
        let start = align_up(self.bump, bin_align(bin));
        let Some(end) = start.checked_add(size) else {
            return ptr::null_mut();
        };
        if end > self.top {
            return ptr::null_mut();
        }
        if end > self.committed {
            match self.commit_bump_to(end) {
                Ok(delta) => self.stats.committed += delta,
                Err(_) => return ptr::null_mut(),
            }
        }
        self.bump = end;
        self.stats.bumped = end - self.base;
        self.stats.allocated += size;
        start as *mut u8
    }

    fn free(&mut self, p: *mut u8, bin: usize) {
        let addr = p as usize;
        let size = BINS[bin] as usize;
        assert!(
            addr >= self.base && addr + size <= self.bump && addr % bin_align(bin) == 0,
            "meta: free of a pointer that was never allocated here"
        );
        let node = p as *mut FreeNode;
        // The one double free that costs nothing to see: the block is the
        // list's head. A block deeper in the list is not looked for (that
        // would walk the list on every free).
        assert!(
            self.free[bin] != node,
            "meta: double free of the free list head"
        );
        // SAFETY: the block is `size >= 16` bytes of committed memory that
        // the caller has given up, aligned to at least 16.
        unsafe { (*node).next = self.free[bin] };
        self.free[bin] = node;
        self.stats.allocated -= size;
    }

    fn reserve(&mut self, len: usize, align: usize) -> Option<usize> {
        assert!(
            align.is_power_of_two(),
            "meta: reserve alignment not a power of two"
        );
        if len > self.top - self.frontier {
            return None;
        }
        let align = align.max(self.page);
        let len = align_up(len.max(1), self.page);
        let start = self.top.checked_sub(len)? & !(align - 1);
        if start < self.frontier {
            return None;
        }
        self.stats.reserved += self.top - start;
        self.top = start;
        Some(start)
    }

    fn commit(&mut self, addr: usize, len: usize) -> Result<(), Errno> {
        assert!(
            addr % self.page == 0 && len % self.page == 0 && len > 0,
            "meta: commit range not page aligned"
        );
        assert!(addr.checked_add(len).is_some(), "meta: commit range wraps");
        let end = addr + len;
        if addr < self.top {
            // A table carved from the object side (`reserve_eager`): inside
            // the chunked frontier, so this is the deferred commit if it is
            // still pending, then the owner's accounting of the pages it
            // uses.
            assert!(
                addr >= self.base && end <= self.frontier,
                "meta: commit outside the reserved area"
            );
            self.flush()?;
            self.stats.committed += len;
            return Ok(());
        }
        assert!(
            end <= self.base + self.len,
            "meta: commit outside the reserved area"
        );
        commit_range(addr, len)?;
        // Metadata THP (`thp.rs`): `Lazy::ensure` rounded the range to the
        // folio; the advice makes the kernel map it as one.
        if crate::thp::meta_folio_size() != 0 {
            crate::thp::advise_meta(addr, len);
        }
        self.top_committed += len;
        self.stats.committed += len;
        Ok(())
    }

    /// [`Meta::reserve_committed`] under the lock.
    fn reserve_committed(&mut self, len: usize, align: usize) -> Option<usize> {
        if self.eager_ok(len) {
            let (bump, frontier, stats) = (self.bump, self.frontier, self.stats);
            if let Some(p) = self.reserve_eager(len, align) {
                if self.flush().is_ok() {
                    self.stats.committed += align_up(len.max(1), self.page);
                    return Some(p);
                }
                // The kernel refused the commit: undo the carve (nothing
                // has seen it) and try the reservation side, as when the
                // object side cannot hold the table.
                self.bump = bump;
                self.frontier = frontier;
                self.stats = stats;
            }
        }
        let old_top = self.top;
        let old_reserved = self.stats.reserved;
        let start = self.reserve(len, align)?;
        if self.commit(start, old_top - start).is_err() {
            self.top = old_top;
            self.stats.reserved = old_reserved;
            return None;
        }
        // `commit` made exactly `[start, old_top)` writable: `start` is the
        // new `top`, so it took the `commit_range` branch.
        Some(start)
    }

    fn stats(&self) -> MetaStats {
        MetaStats {
            writable: (self.frontier - self.base) + self.top_committed,
            ..self.stats
        }
    }
}

/// Whether tables may be carved eagerly on this host (the module doc):
/// the `thp` key is `never` (the region is `MADV_NOHUGEPAGE`d, or the
/// kernel has no THP, so a table's first touch costs one page) and the
/// kernel honours `MAP_NORESERVE` (`vm.overcommit_memory` 0 or 1; under 2
/// every writable page is charged to the commit limit and a forked child
/// is charged again). Read once, on the first table.
fn eager_policy() -> bool {
    if !crate::thp::is_never() {
        return false;
    }
    // Spelled as bytes so the verifier sees the NUL `sys::open` requires.
    const OVERCOMMIT: [u8; 31] = [
        b'/', b'p', b'r', b'o', b'c', b'/', b's', b'y', b's', b'/', b'v', b'm', b'/', b'o', b'v',
        b'e', b'r', b'c', b'o', b'm', b'm', b'i', b't', b'_', b'm', b'e', b'm', b'o', b'r', b'y',
        0,
    ];
    let mut buf = [0u8; 8];
    match crate::sys::read_small_file(&OVERCOMMIT, &mut buf) {
        Some(n) if n > 0 => buf[0] != b'2',
        // Unreadable (a sandbox without /proc): the mode cannot be known,
        // so take the safe side.
        _ => false,
    }
}

impl Meta {
    /// An allocator over `[base, base + len)`, both page multiples.
    pub const fn new(base: usize, len: usize, page: usize) -> Meta {
        Meta {
            inner: Lock::new(Inner::new(base, len, page)),
        }
    }

    /// The lock, for the fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> &RawLock {
        self.inner.raw()
    }

    /// Allocate an object. Contents are unspecified: fresh memory is zero,
    /// reused memory holds whatever its last owner left. Null when the region
    /// is exhausted or the kernel refuses to commit.
    ///
    /// Aborts for a layout this allocator does not serve (`size >
    /// MAX_BINNED` or `align > MAX_ALIGN`): that is a bug, not a resource
    /// failure.
    pub fn alloc(&self, layout: Layout) -> *mut u8 {
        assert!(
            layout.align() <= MAX_ALIGN,
            "meta: alignment too large for an object"
        );
        let Some(bin) = bin_for(layout) else {
            panic!("meta: object too large")
        };
        self.inner.lock().alloc(bin)
    }

    /// [`alloc`](Meta::alloc), then zero the object if it was reused.
    pub fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        assert!(
            layout.align() <= MAX_ALIGN,
            "meta: alignment too large for an object"
        );
        let Some(bin) = bin_for(layout) else {
            panic!("meta: object too large")
        };
        let mut g = self.inner.lock();
        let before = g.bump;
        let p = g.alloc(bin);
        if !p.is_null() && (p as usize) < before {
            // Reused: below the old bump pointer. Fresh memory is above it
            // and has never been written.
            // SAFETY: `p` is a live block of `BINS[bin]` bytes we just took.
            unsafe { ptr::write_bytes(p, 0, BINS[bin] as usize) };
        }
        p
    }

    /// Return an object allocated with the same `layout`.
    pub fn free(&self, p: *mut u8, layout: Layout) {
        assert!(!p.is_null(), "meta: free of null");
        let Some(bin) = bin_for(layout) else {
            panic!("meta: free with an impossible layout")
        };
        self.inner.lock().free(p, bin);
    }

    /// Carve `len` bytes (rounded up to pages) aligned to `align` (at least a
    /// page) from the top of the region. Uncommitted: the owner calls
    /// [`commit`](Meta::commit) on the pages it uses. Never freed. `None`
    /// when it would collide with the object area. For a table that is
    /// used from its base up, [`reserve_table`](Meta::reserve_table); this
    /// is for a reservation that needs a `PROT_NONE` part (the background
    /// thread's stack guard).
    pub fn reserve(&self, len: usize, align: usize) -> Option<usize> {
        self.inner.lock().reserve(len, align)
    }

    /// A never-freed table of `len` bytes aligned to `align` whose owner
    /// commits pages from its base as it uses them. Up to [`EAGER_MAX`]
    /// bytes, and where the host allows (the module doc), it is carved
    /// from the object side and is writable as a whole from its owner's
    /// first commit (one deferred `mprotect` for every table carved since
    /// the last, the module doc); otherwise, or
    /// when the object side cannot hold it, it is a
    /// [`reserve`](Meta::reserve). Either way the owner calls
    /// [`commit`](Meta::commit) on the pages it uses before touching them.
    pub fn reserve_table(&self, len: usize, align: usize) -> Option<usize> {
        let mut g = self.inner.lock();
        if g.eager_ok(len) {
            if let Some(p) = g.reserve_eager(len, align) {
                return Some(p);
            }
        }
        g.reserve(len, align)
    }

    /// Make `[addr, addr + len)`, page aligned and inside a reservation or
    /// an eagerly carved table, readable and writable (accounting only for
    /// the latter). Each page should be committed once; the
    /// committed-bytes counter trusts the caller on that.
    pub fn commit(&self, addr: usize, len: usize) -> Result<(), Errno> {
        self.inner.lock().commit(addr, len)
    }

    /// [`reserve_table`](Meta::reserve_table) and commit the whole range.
    /// Zero on return. `None` on either failure, leaving nothing reserved.
    pub fn reserve_committed(&self, len: usize, align: usize) -> Option<usize> {
        self.inner.lock().reserve_committed(len, align)
    }

    pub fn stats(&self) -> MetaStats {
        self.inner.lock().stats()
    }

    /// Tests: force tables onto the object side (`true`) or the
    /// reservation side (`false`), whatever the host's policy says.
    #[cfg(test)]
    pub fn set_eager_for_test(&self, on: bool) {
        self.inner.lock().eager = if on { EAGER_ON } else { EAGER_OFF };
    }

    /// Tests: whether the host's policy allows eager tables.
    #[cfg(test)]
    pub fn eager_allowed(&self) -> bool {
        self.inner.lock().eager_ok(1)
    }

    /// Start of the region.
    #[cfg(test)]
    pub fn base(&self) -> usize {
        self.inner.lock().base
    }

    /// Lowest reserved address; the object area may grow up to here.
    #[cfg(test)]
    pub fn top(&self) -> usize {
        self.inner.lock().top
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// The page size the tests compute with: the kernel's, not a 4 KiB
    /// constant (the reservations are real, so the commit arithmetic
    /// follows the kernel's page, 64 KiB on some aarch64 kernels).
    fn page_size() -> usize {
        crate::sys::features::page_size()
    }

    /// The object side's commit step: [`COMMIT_CHUNK`] rounded up to the
    /// page, so 16 pages under 4 KiB pages and one under 64 KiB pages.
    fn chunk() -> usize {
        align_up(COMMIT_CHUNK, page_size())
    }

    /// A region for tests: a real `PROT_NONE` reservation, so that commit
    /// goes through `mprotect`.
    struct Backing {
        base: usize,
        len: usize,
    }

    impl Backing {
        fn new(len: usize) -> Backing {
            use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
            let base = unsafe {
                crate::sys::mmap(
                    0,
                    len,
                    PROT_NONE,
                    MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE,
                    -1,
                    0,
                )
            }
            .expect("reserve");
            assert_eq!(base % page_size(), 0);
            Backing { base, len }
        }

        fn meta(&self) -> Meta {
            Meta::new(self.base, self.len, page_size())
        }
    }

    impl Drop for Backing {
        fn drop(&mut self) {
            unsafe { crate::sys::munmap(self.base, self.len) }.unwrap();
        }
    }

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn bins_are_well_formed() {
        assert_eq!(NBINS, 30);
        assert_eq!(BINS[0], 16);
        assert_eq!(BINS[NBINS - 1] as usize, MAX_BINNED);
        for w in BINS.windows(2) {
            assert!(w[0] < w[1]);
        }
        for (i, &b) in BINS.iter().enumerate() {
            let b = b as usize;
            assert_eq!(
                b % bin_align(i),
                0,
                "bin {i} ({b}) not a multiple of its alignment"
            );
            // Power of two or 1.5x a power of two; from 128 up, line sized.
            assert!(b.is_power_of_two() || (b % 3 == 0 && (b / 3).is_power_of_two()));
            if i >= 4 {
                assert_eq!(b % CACHE_LINE, 0);
            } else {
                assert_eq!(bin_align(i), MIN_ALIGN);
            }
        }
        // Internal waste is bounded: consecutive bins grow by at most 2x.
        for w in BINS.windows(2) {
            assert!(w[1] <= 2 * w[0]);
        }
    }

    #[test]
    fn bin_of_matches_linear_scan() {
        for size in 0..=MAX_BINNED + 1 {
            let expect = BINS.iter().position(|&b| b as usize >= size);
            assert_eq!(bin_of(size), expect, "size {size}");
        }
        assert_eq!(bin_of(usize::MAX), None);
        assert_eq!(bin_of(MAX_BINNED), Some(NBINS - 1));
        for size in [
            1usize, 16, 17, 32, 33, 48, 49, 64, 65, 128, 129, 256, 257, 384, 385, 512,
        ] {
            let b = bin_of(size).unwrap();
            assert!(BINS[b] as usize >= size);
            assert!(b == 0 || (BINS[b - 1] as usize) < size);
        }
    }

    #[test]
    fn bin_for_promotes_on_alignment() {
        assert_eq!(bin_for(layout(8, 8)), Some(0));
        assert_eq!(bin_for(layout(8, 16)), Some(0));
        assert_eq!(bin_for(layout(8, 32)), Some(4));
        assert_eq!(bin_for(layout(40, 64)), Some(4));
        assert_eq!(bin_for(layout(64, 128)), Some(4));
        assert_eq!(bin_for(layout(100, 64)), Some(4));
        assert_eq!(bin_for(layout(129, 8)), Some(5));
        assert_eq!(BINS[5], 256);
        assert_eq!(bin_for(layout(257, 8)), Some(6));
        assert_eq!(BINS[6], 384);
        assert_eq!(bin_for(layout(MAX_BINNED + 1, 8)), None);
    }

    #[test]
    fn fresh_region_stats() {
        let b = Backing::new(1 << 20);
        let m = b.meta();
        assert_eq!(m.base(), b.base);
        assert_eq!(m.top(), b.base + b.len);
        assert_eq!(
            m.stats(),
            MetaStats {
                len: 1 << 20,
                ..MetaStats::default()
            }
        );
    }

    #[test]
    fn first_objects_come_from_zero_committed_memory() {
        let b = Backing::new(4 << 20);
        let m = b.meta();
        let l = layout(200, 8);
        let p = m.alloc(l);
        assert!(!p.is_null());
        assert_eq!(p as usize, b.base, "first object sits at the base");
        assert_eq!(p as usize % CACHE_LINE, 0);
        let s = m.stats();
        assert_eq!(s.committed, chunk(), "one chunk committed");
        assert_eq!(s.allocated, 256, "200 rounds up to the 256 bin");
        assert_eq!(s.bumped, 256);
        // Writable and zero.
        let slice = unsafe { core::slice::from_raw_parts_mut(p, 200) };
        assert!(slice.iter().all(|&x| x == 0));
        slice.fill(0xA5);
        // Small objects pack at 16 and do not disturb the chunk.
        let q = m.alloc(layout(1, 1));
        let r = m.alloc(layout(16, 16));
        assert_eq!(q as usize, b.base + 256);
        assert_eq!(r as usize, b.base + 272);
        assert_eq!(m.stats().committed, chunk());
        // Crossing the chunk boundary commits the next one.
        let big = m.alloc(layout(MAX_BINNED, 64));
        assert!(!big.is_null());
        assert_eq!(m.stats().committed, align_up(MAX_BINNED + 512, chunk()));
        unsafe { core::slice::from_raw_parts_mut(big, MAX_BINNED) }.fill(1);
    }

    #[test]
    fn free_list_is_lifo_and_per_bin() {
        let b = Backing::new(1 << 20);
        let m = b.meta();
        let l = layout(100, 8);
        let a = m.alloc(l);
        let c = m.alloc(l);
        let d = m.alloc(layout(1000, 8));
        let bumped = m.stats().bumped;
        m.free(a, l);
        m.free(c, l);
        assert_eq!(m.stats().allocated, 1024);
        // Same bin: last freed comes back first, nothing new is bumped.
        assert_eq!(m.alloc(l), c);
        assert_eq!(m.alloc(l), a);
        assert_eq!(m.stats().bumped, bumped);
        // Different bin: not served from that list.
        m.free(a, l);
        let e = m.alloc(layout(200, 8));
        assert_ne!(e, a);
        assert!(m.stats().bumped > bumped);
        m.free(d, layout(1000, 8));
        assert_eq!(m.alloc(layout(1000, 8)), d);
        // alloc_zeroed scrubs a reused block but leaves fresh ones alone.
        unsafe { core::slice::from_raw_parts_mut(d, 1024) }.fill(0xFF);
        m.free(d, layout(1000, 8));
        let z = m.alloc_zeroed(layout(1000, 8));
        assert_eq!(z, d);
        assert!(
            unsafe { core::slice::from_raw_parts(z, 1024) }
                .iter()
                .all(|&x| x == 0)
        );
        let fresh = m.alloc_zeroed(layout(5000, 8));
        assert!(
            unsafe { core::slice::from_raw_parts(fresh, 5000) }
                .iter()
                .all(|&x| x == 0)
        );
    }

    #[test]
    fn alignment_is_honoured() {
        let b = Backing::new(1 << 20);
        let m = b.meta();
        for align in [1usize, 2, 4, 8, 16, 32, 64, 128] {
            for size in [1usize, 15, 16, 17, 48, 49, 64, 65, 96, 300, 4097] {
                let l = layout(size, align);
                let p = m.alloc(l);
                assert!(!p.is_null());
                assert_eq!(p as usize % align, 0, "size {size} align {align}");
                assert_eq!(p as usize % bin_align(bin_for(l).unwrap()), 0);
                if size > 64 || align > 16 {
                    assert_eq!(p as usize % CACHE_LINE, 0);
                }
            }
        }
    }

    #[test]
    fn exhaustion_returns_null_and_recovers() {
        let b = Backing::new(16 * page_size());
        let m = b.meta();
        let l = layout(1024, 8);
        let mut got = Vec::new();
        loop {
            let p = m.alloc(l);
            if p.is_null() {
                break;
            }
            got.push(p);
        }
        assert_eq!(
            got.len(),
            16 * page_size() / 1024,
            "16 pages hold exactly that many 1 KiB objects"
        );
        assert_eq!(m.stats().committed, 16 * page_size());
        assert_eq!(m.stats().bumped, 16 * page_size());
        assert!(m.alloc(layout(1, 1)).is_null());
        assert_eq!(
            m.reserve(page_size(), page_size()),
            None,
            "no room left for a reservation"
        );
        // Freeing one makes exactly one more available.
        m.free(got.pop().unwrap(), l);
        assert!(!m.alloc(l).is_null());
        assert!(m.alloc(l).is_null());
        // Distinct pointers throughout.
        got.sort_unstable();
        got.dedup();
        assert_eq!(got.len(), 16 * page_size() / 1024 - 1);
    }

    #[test]
    fn reservations_come_from_the_top() {
        let b = Backing::new(64 * page_size());
        let m = b.meta();
        let end = b.base + b.len;
        let r1 = m.reserve(3 * page_size(), page_size()).unwrap();
        assert_eq!(r1, end - 3 * page_size());
        assert_eq!(m.top(), r1);
        // Sub-page lengths round up; large alignment rounds the start down.
        let r2 = m.reserve(10, 8 * page_size()).unwrap();
        assert_eq!(r2 % (8 * page_size()), 0);
        assert!(r2 + page_size() <= r1);
        assert_eq!(m.top(), r2);
        let s = m.stats();
        assert_eq!(s.reserved, end - r2);
        assert_eq!(s.committed, 0, "reservations are uncommitted");
        // Commit part of r1, write into it.
        m.commit(r1 + page_size(), page_size()).unwrap();
        assert_eq!(m.stats().committed, page_size());
        unsafe { core::slice::from_raw_parts_mut((r1 + page_size()) as *mut u8, page_size()) }
            .fill(7);
        // A small reserve_committed is zero and writable, and (eager) sits
        // on the object side, not below r2.
        let r3 = m.reserve_committed(2 * page_size(), page_size()).unwrap();
        assert_eq!(
            r3, b.base,
            "a small committed reservation is carved from the object side"
        );
        assert_eq!(m.top(), r2);
        let sl = unsafe { core::slice::from_raw_parts_mut(r3 as *mut u8, 2 * page_size()) };
        assert!(sl.iter().all(|&x| x == 0));
        sl.fill(9);
        let s = m.stats();
        assert_eq!(
            s.committed,
            page_size() + align_up(2 * page_size(), chunk()),
            "r1's page, r3's two and the chunk's slack around r3"
        );
        assert_eq!(s.eager, 2 * page_size());
        assert_eq!(s.bumped, 2 * page_size());
        // Objects and reservations never overlap: fill the rest with
        // objects and check the boundary.
        let mut n = 0;
        while !m.alloc(layout(page_size(), 8)).is_null() {
            n += 1;
        }
        assert_eq!(m.stats().bumped, m.top() - b.base);
        assert_eq!(n, (m.top() - b.base) / page_size() - 2);
        assert_eq!(m.reserve(page_size(), page_size()), None);
        assert_eq!(m.reserve_table(page_size(), page_size()), None);
        // Object side writable exactly up to the reservation boundary;
        // committed counts r1's page, r3 and the objects' chunks.
        let s = m.stats();
        assert_eq!(s.writable, page_size() + (m.top() - b.base));
        assert_eq!(s.committed, page_size() + (m.top() - b.base));
    }

    /// Reservations larger than `EAGER_MAX`, or any table once eager
    /// carving is off, come from the top uncommitted; `commit` then really
    /// makes pages writable and counts them.
    #[test]
    fn large_and_forced_tables_stay_on_the_reservation_side() {
        let b = Backing::new(EAGER_MAX + (EAGER_MAX >> 1) + 64 * page_size());
        let m = b.meta();
        let end = b.base + b.len;
        let big = m
            .reserve_table(EAGER_MAX + page_size(), page_size())
            .unwrap();
        assert_eq!(
            big,
            end - EAGER_MAX - page_size(),
            "larger than EAGER_MAX: from the top"
        );
        assert_eq!(m.stats().eager, 0);
        assert_eq!(m.stats().reserved, EAGER_MAX + page_size());
        m.commit(big, page_size()).unwrap();
        let s = m.stats();
        assert_eq!((s.committed, s.writable), (page_size(), page_size()));
        m.set_eager_for_test(false);
        let small = m.reserve_table(page_size(), page_size()).unwrap();
        assert_eq!(small, big - page_size(), "eager off: from the top too");
        assert_eq!(m.stats().eager, 0);
        let c = m.reserve_committed(3 * page_size(), page_size()).unwrap();
        assert_eq!(c, small - 3 * page_size());
        assert_eq!(m.stats().committed, 4 * page_size());
        m.set_eager_for_test(true);
        let e = m.reserve_table(page_size(), page_size()).unwrap();
        assert_eq!(e, b.base, "eager on again: from the base");
        assert_eq!(m.stats().eager, page_size());
    }

    /// Eager tables, objects and more tables interleave on the object side
    /// and stay one contiguous writable range: no hole, no page committed
    /// twice, every table aligned as asked, and `commit` on such a table
    /// only counts.
    #[test]
    fn eager_tables_pack_on_the_object_side() {
        let b = Backing::new(16 << 20);
        let m = b.meta();
        m.set_eager_for_test(true);
        let o1 = m.alloc(layout(200, 8));
        assert_eq!(o1 as usize, b.base);
        let t1 = m.reserve_table(3 * page_size(), page_size()).unwrap();
        assert_eq!(
            t1,
            b.base + page_size(),
            "page aligned, right after the objects' page"
        );
        let t2 = m.reserve_table(10, 8 * page_size()).unwrap();
        assert_eq!(t2 % (8 * page_size()), 0);
        assert!(t2 >= t1 + 3 * page_size());
        let o2 = m.alloc(layout(1000, 8));
        assert_eq!(
            o2 as usize,
            t2 + page_size(),
            "objects resume right after the table"
        );
        let t3 = m.reserve_table(EAGER_MAX, page_size()).unwrap();
        assert!(t3 >= o2 as usize + 1024);
        let before = m.stats();
        assert_eq!(before.eager, 3 * page_size() + page_size() + EAGER_MAX);
        assert_eq!(before.bumped, t3 + EAGER_MAX - b.base);
        assert!(before.writable >= before.bumped && before.writable % page_size() == 0);
        assert!(before.committed <= before.writable);
        // The owner's commits: writable does not move, committed counts.
        m.commit(t1, page_size()).unwrap();
        m.commit(t3 + EAGER_MAX - page_size(), page_size()).unwrap();
        let after = m.stats();
        assert_eq!(after.writable, before.writable);
        assert_eq!(after.committed, before.committed + 2 * page_size());
        // Every byte of every table is writable and zero.
        for (t, len) in [(t1, 3 * page_size()), (t2, page_size()), (t3, EAGER_MAX)] {
            let sl = unsafe { core::slice::from_raw_parts_mut(t as *mut u8, len) };
            assert!(sl.iter().all(|&x| x == 0), "table at {t:#x} not zero");
            sl[0] = 1;
            sl[len - 1] = 1;
        }
        // Identities.
        let s = m.stats();
        assert!(s.allocated + s.eager <= s.bumped && s.bumped <= s.writable && s.writable <= s.len);
        assert!(s.bumped + s.reserved <= s.len);
    }

    /// An eager table that would reach the reservations is refused (and
    /// the reservation side, already up against the chunk frontier, has no
    /// room either); a smaller one still fits, then nothing fits either
    /// way, and the failure leaves the allocator working for what does.
    #[test]
    fn eager_carve_stops_at_the_reservations() {
        let b = Backing::new(16 * page_size());
        let m = b.meta();
        m.set_eager_for_test(true);
        let top = m.reserve(4 * page_size(), page_size()).unwrap();
        assert_eq!(top, b.base + 12 * page_size());
        // 10 pages fit on the object side (12 free); the chunk commit
        // stops at the reservations (under 4 KiB pages; a 64 KiB-page
        // chunk is one page, so the frontier stops at the table's end).
        let t1 = m.reserve_table(10 * page_size(), page_size()).unwrap();
        assert_eq!(t1, b.base);
        assert_eq!(
            m.stats().writable,
            align_up(10 * page_size(), chunk()).min(12 * page_size())
        );
        // 3 more do not: not eagerly (past `top`), not as a reservation
        // (the frontier is at `top`).
        assert_eq!(m.reserve_table(3 * page_size(), page_size()), None);
        assert_eq!(m.reserve_committed(3 * page_size(), page_size()), None);
        assert_eq!(m.top(), top, "a failed table reserves nothing");
        // 2 do, eagerly; then nothing, while an object in the free list
        // still does.
        let t2 = m.reserve_table(2 * page_size(), page_size()).unwrap();
        assert_eq!(t2, b.base + 10 * page_size());
        assert_eq!(m.reserve_table(page_size(), page_size()), None);
        let l = layout(16, 8);
        assert!(m.alloc(l).is_null());
        let s = m.stats();
        assert_eq!(
            (s.bumped, s.eager, s.reserved, s.writable),
            (
                12 * page_size(),
                12 * page_size(),
                4 * page_size(),
                12 * page_size()
            )
        );
        assert_eq!(s.committed, 0, "nothing asked for yet");
        m.commit(t1, 10 * page_size()).unwrap();
        m.commit(t2, 2 * page_size()).unwrap();
        assert_eq!(m.stats().committed, 12 * page_size());
    }

    /// The host policy: eager carving is on under the default `thp:never`
    /// on a kernel that honours `MAP_NORESERVE`, off
    /// under `vm.overcommit_memory=2`; the probe reads the sysctl.
    #[test]
    fn eager_policy_follows_overcommit_mode() {
        let mode = std::fs::read_to_string("/proc/sys/vm/overcommit_memory").unwrap();
        let expect = mode.trim() != "2" && crate::thp::is_never();
        assert_eq!(eager_policy(), expect, "overcommit_memory {mode:?}");
        let b = Backing::new(1 << 20);
        let m = b.meta();
        assert_eq!(m.eager_allowed(), expect);
    }

    #[test]
    fn reserve_too_large_fails_cleanly() {
        let b = Backing::new(8 * page_size());
        let m = b.meta();
        assert_eq!(m.reserve(9 * page_size(), page_size()), None);
        assert_eq!(m.reserve(usize::MAX, page_size()), None);
        assert_eq!(m.reserve(page_size(), 1 << 40), None);
        assert_eq!(m.stats().reserved, 0);
        assert_eq!(m.top(), b.base + b.len);
        assert_eq!(m.reserve(8 * page_size(), page_size()), Some(b.base));
        assert_eq!(m.alloc(layout(1, 1)), ptr::null_mut());
    }

    #[test]
    #[should_panic(expected = "never allocated here")]
    fn free_of_foreign_pointer_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let _ = m.alloc(layout(64, 8));
        let mut foreign = [0u8; 128];
        m.free(
            align_up(foreign.as_mut_ptr() as usize, 64) as *mut u8,
            layout(64, 8),
        );
    }

    #[test]
    #[should_panic(expected = "never allocated here")]
    fn free_of_misaligned_pointer_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let p = m.alloc(layout(64, 8));
        m.free(unsafe { p.add(8) }, layout(64, 8));
    }

    #[test]
    #[should_panic(expected = "never allocated here")]
    fn free_beyond_bump_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let p = m.alloc(layout(64, 8));
        m.free(unsafe { p.add(64) }, layout(64, 8));
    }

    #[test]
    #[should_panic(expected = "object too large")]
    fn oversized_object_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let _ = m.alloc(layout(MAX_BINNED + 1, 8));
    }

    #[test]
    #[should_panic(expected = "alignment too large")]
    fn overaligned_object_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let _ = m.alloc(layout(64, 256));
    }

    #[test]
    #[should_panic(expected = "commit outside")]
    fn commit_outside_reservations_aborts() {
        // Several pages, so the one-page reservation leaves room below it.
        let b = Backing::new(4 * page_size());
        let m = b.meta();
        let _ = m.reserve(page_size(), page_size());
        let _ = m.commit(b.base, page_size());
    }

    /// Random alloc/free against a model: no overlap, alignment, contents
    /// survive, stats match, and freed space is reused rather than bumped.
    #[test]
    fn matches_model() {
        let b = Backing::new(32 << 20);
        let m = b.meta();
        let mut s = 0xDEAD_BEEF_CAFE_F00Du64;
        let mut live: Vec<(*mut u8, Layout, u8)> = Vec::new();
        let mut expect_allocated = 0usize;
        for _ in 0..20_000 {
            let r = xorshift(&mut s);
            if live.is_empty() || (live.len() < 400 && r % 2 == 0) {
                let size = match xorshift(&mut s) % 10 {
                    0 => (xorshift(&mut s) as usize) % (64 * 1024) + 1,
                    1..=3 => (xorshift(&mut s) as usize) % 4096 + 1,
                    _ => (xorshift(&mut s) as usize) % 256,
                };
                let align = 1usize << (xorshift(&mut s) % 7);
                let l = layout(size, align);
                let p = m.alloc(l);
                assert!(!p.is_null());
                let bin = BINS[bin_for(l).unwrap()] as usize;
                assert_eq!(p as usize % align, 0);
                assert!(
                    b.base <= p as usize && p as usize + bin <= b.base + b.len,
                    "object outside the region"
                );
                for (q, ql, _) in &live {
                    let qb = BINS[bin_for(*ql).unwrap()] as usize;
                    let (a0, a1) = (p as usize, p as usize + bin);
                    let (b0, b1) = (*q as usize, *q as usize + qb);
                    assert!(a1 <= b0 || b1 <= a0, "overlap");
                }
                let fill = (xorshift(&mut s) as u8) | 1;
                unsafe { core::slice::from_raw_parts_mut(p, size) }.fill(fill);
                live.push((p, l, fill));
                expect_allocated += bin;
            } else {
                let i = (xorshift(&mut s) as usize) % live.len();
                let (p, l, fill) = live.swap_remove(i);
                let slice = unsafe { core::slice::from_raw_parts(p, l.size()) };
                assert!(slice.iter().all(|&x| x == fill), "contents clobbered");
                m.free(p, l);
                expect_allocated -= BINS[bin_for(l).unwrap()] as usize;
            }
            let st = m.stats();
            assert_eq!(st.allocated, expect_allocated);
            assert!(st.allocated <= st.bumped && st.bumped <= st.committed);
            assert!(st.committed <= st.len && st.committed % page_size() == 0);
        }
        // Free everything, then the same sequence of layouts reuses memory.
        let layouts: Vec<Layout> = live.iter().map(|(_, l, _)| *l).collect();
        for (p, l, _) in live.drain(..) {
            m.free(p, l);
        }
        assert_eq!(m.stats().allocated, 0);
        let bumped = m.stats().bumped;
        for l in &layouts {
            assert!(!m.alloc(*l).is_null());
        }
        assert_eq!(
            m.stats().bumped,
            bumped,
            "everything came from the free lists"
        );
    }

    /// Eight threads share one allocator; each checks its own objects' fill
    /// patterns so any handout of the same block twice shows up.
    #[test]
    fn threads_get_disjoint_objects() {
        let b = Backing::new(16 << 20);
        let m = b.meta();
        std::thread::scope(|sc| {
            for t in 0..8u64 {
                let m = &m;
                sc.spawn(move || {
                    let mut s = 0x1234_5678 + t * 0x9E37_79B9;
                    let mut live: Vec<(*mut u8, Layout)> = Vec::new();
                    let fill = t as u8 + 1;
                    for _ in 0..5_000 {
                        if live.len() < 64 && xorshift(&mut s) % 3 != 0 {
                            let size = (xorshift(&mut s) as usize) % 2048 + 1;
                            let l = layout(size, 1 << (xorshift(&mut s) % 7));
                            let p = m.alloc(l);
                            assert!(!p.is_null());
                            unsafe { core::slice::from_raw_parts_mut(p, size) }.fill(fill);
                            live.push((p, l));
                        } else if let Some(i) = live.len().checked_sub(1) {
                            let (p, l) = live.swap_remove((xorshift(&mut s) as usize) % (i + 1));
                            let sl = unsafe { core::slice::from_raw_parts(p, l.size()) };
                            assert!(sl.iter().all(|&x| x == fill), "thread {t} block clobbered");
                            m.free(p, l);
                        }
                    }
                    for (p, l) in live {
                        let sl = unsafe { core::slice::from_raw_parts(p, l.size()) };
                        assert!(sl.iter().all(|&x| x == fill));
                        m.free(p, l);
                    }
                });
            }
        });
        assert_eq!(m.stats().allocated, 0);
        assert!(!m.raw_lock().is_locked());
    }

    /// The `rw-p` ranges of `/proc/self/maps` inside `[base, base + len)`,
    /// clipped to it (a committed range at either end merges with an
    /// adjacent `rw-p` anonymous mapping of another test); everything else
    /// in the region must be `---p`.
    fn writable_ranges(base: usize, len: usize) -> Vec<(usize, usize)> {
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let mut rw: Vec<(usize, usize)> = Vec::new();
        for line in maps.lines() {
            let mut it = line.split_whitespace();
            let range = it.next().unwrap();
            let perms = it.next().unwrap();
            let (lo, hi) = range.split_once('-').unwrap();
            let lo = usize::from_str_radix(lo, 16).unwrap();
            let hi = usize::from_str_radix(hi, 16).unwrap();
            if hi <= base || lo >= base + len {
                continue;
            }
            if perms.starts_with("rw") {
                rw.push((lo.max(base), hi.min(base + len)));
            } else {
                assert!(
                    perms.starts_with("---"),
                    "unexpected perms {perms} at {range}"
                );
            }
        }
        rw.sort_unstable();
        rw
    }

    /// A run of eager carves makes nothing writable (no system call) until
    /// the first commit of one of them, which makes the object side
    /// writable through all of them at once; an object allocated after
    /// further carves does the same for those. The counters are the ones
    /// an immediate commit would give.
    #[test]
    fn eager_carves_defer_their_commit_to_first_use() {
        let b = Backing::new(64 << 20);
        let m = b.meta();
        m.set_eager_for_test(true);
        let mut tables = Vec::new();
        for i in 0..20usize {
            let len = (1 + i % 5) * page_size() + 100;
            tables.push((m.reserve_table(len, page_size()).unwrap(), len));
        }
        assert_eq!(writable_ranges(b.base, b.len), vec![], "carves alone");
        let s = m.stats();
        let (last, last_len) = tables[19];
        assert_eq!(s.writable, align_up(last + last_len - b.base, chunk()));
        assert_eq!(s.committed, s.writable - s.eager, "the chunk slack only");
        m.commit(tables[3].0, page_size()).unwrap();
        assert_eq!(
            writable_ranges(b.base, b.len),
            vec![(b.base, b.base + s.writable)],
            "one commit covers every table carved so far"
        );
        assert_eq!(m.stats().committed, s.committed + page_size());
        assert_eq!(m.stats().writable, s.writable);
        for &(t, len) in &tables {
            unsafe { core::ptr::write_volatile((t + len - 1) as *mut u8, 1) };
        }
        // More carves, then an object past them: one commit again.
        let t = m.reserve_table(EAGER_MAX, page_size()).unwrap();
        let before = m.stats();
        assert_eq!(writable_ranges(b.base, b.len).len(), 1);
        assert!(writable_ranges(b.base, b.len)[0].1 < t + EAGER_MAX);
        let o = m.alloc(layout(1000, 8));
        assert_eq!(o as usize, t + EAGER_MAX, "the object follows the table");
        let after = m.stats();
        assert_eq!(
            writable_ranges(b.base, b.len),
            vec![(b.base, b.base + after.writable)]
        );
        assert_eq!(
            after.committed - before.committed,
            after.writable - before.writable,
            "only a new chunk past the frontier counts"
        );
        unsafe { core::ptr::write_volatile(t as *mut u8, 1) };
    }

    /// On Linux the committed part of the region must show up as exactly one
    /// `rw-p` mapping per committed range, and nothing else in the region
    /// may be writable: the object side with a small table inside it is one
    /// range, a page committed in a large reservation another.
    #[test]
    fn maps_show_commits() {
        let b = Backing::new(64 << 20);
        let m = b.meta();
        m.set_eager_for_test(true);
        for _ in 0..10 {
            assert!(!m.alloc(layout(1000, 8)).is_null());
        }
        let small = m.reserve_committed(3 * page_size(), page_size()).unwrap();
        assert_eq!(
            small,
            b.base + align_up(10 * 1024, page_size()),
            "page aligned, after the ten 1 KiB objects"
        );
        let big = m
            .reserve_table(EAGER_MAX + page_size(), page_size())
            .unwrap();
        m.commit(big + page_size(), page_size()).unwrap();
        // The object side's frontier: the chunks through the small table's
        // end (one chunk under 4 KiB pages, four pages under 64 KiB pages).
        let front = align_up(small + 3 * page_size() - b.base, chunk());
        let st = m.stats();
        assert_eq!(
            st.committed,
            front + page_size(),
            "the objects' chunks with the table in them, plus big's page"
        );
        assert_eq!(st.writable, front + page_size());
        assert_eq!(
            writable_ranges(b.base, b.len),
            vec![
                (b.base, b.base + front),
                (big + page_size(), big + 2 * page_size())
            ],
            "writable ranges in the region"
        );
        // The small table's pages are writable, the large table's
        // uncommitted pages are not (a write there faults: checked in a
        // child so the test survives).
        unsafe { core::ptr::write_volatile(small as *mut u8, 1) };
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                core::ptr::write_volatile(big as *mut u8, 1);
                libc::_exit(0);
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(
            libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGSEGV,
            "status {status:#x}"
        );
    }

    #[test]
    #[should_panic(expected = "double free of the free list head")]
    fn a_double_free_of_the_list_head_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let p = m.alloc(layout(64, 8));
        assert!(!p.is_null());
        m.free(p, layout(64, 8));
        m.free(p, layout(64, 8));
    }

    #[test]
    fn a_free_below_the_list_head_is_accepted() {
        // Documents the limit of the check: only the head is compared.
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let p = m.alloc(layout(64, 8));
        let q = m.alloc(layout(64, 8));
        m.free(p, layout(64, 8));
        m.free(q, layout(64, 8));
        assert_eq!(m.alloc(layout(64, 8)), q);
        assert_eq!(m.alloc(layout(64, 8)), p);
    }

    /// Many small tables with objects between them are one `rw-p` mapping
    /// from the base to the chunk frontier, and a forked child sees the same
    /// one mapping: what a PostgreSQL backend inherits (the module doc).
    #[test]
    fn eager_tables_are_one_mapping_in_parent_and_child() {
        let b = Backing::new(64 << 20);
        let m = b.meta();
        m.set_eager_for_test(true);
        let mut tables = Vec::new();
        for i in 0..40usize {
            assert!(!m.alloc(layout(64 + i * 16, 8)).is_null());
            let len = (1 + i % 7) * page_size() + (i % 3) * 100;
            let t = m
                .reserve_table(
                    len,
                    if i % 4 == 0 {
                        16 * page_size()
                    } else {
                        page_size()
                    },
                )
                .unwrap();
            m.commit(t, page_size()).unwrap();
            unsafe { core::ptr::write_volatile(t as *mut u64, i as u64 + 1) };
            tables.push((t, len));
        }
        let s = m.stats();
        assert_eq!(s.reserved, 0, "nothing on the reservation side");
        assert_eq!(
            s.eager,
            tables
                .iter()
                .map(|&(_, l)| align_up(l, page_size()))
                .sum::<usize>()
        );
        let expect = vec![(b.base, b.base + s.writable)];
        assert_eq!(
            writable_ranges(b.base, b.len),
            expect,
            "one writable range for 40 tables and 40 objects"
        );
        // No hole: every byte from the base to the frontier is writable
        // (read back through a volatile touch of each page).
        for off in (0..s.writable).step_by(page_size()) {
            unsafe { core::ptr::read_volatile((b.base + off) as *const u8) };
        }
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            let ok = writable_ranges(b.base, b.len) == expect
                && tables.iter().enumerate().all(|(i, &(t, _))| unsafe { core::ptr::read_volatile(t as *const u64) } == i as u64 + 1);
            unsafe { libc::_exit(if ok { 0 } else { 1 }) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "child saw a different mapping"
        );
    }
}
