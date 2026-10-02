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
//! instead, page aligned and writable from the start, so it merges with the
//! objects into the one mapping that grows from the base rather than adding
//! VMAs of its own that every `fork`, its faults and its exit pay for
//! (design 2). Its owner still calls [`Meta::commit`], which is then
//! accounting only. Larger tables stay on the reservation side: making them
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

#[cfg(verus_only)]
use crate::lock::lock_init;
use crate::lock::{Lock, RawLock};
use crate::sys::Errno;
use crate::vspec::addr::addr_to_ptr;
#[cfg(verus_only)]
use crate::vspec::addr::cast_provenance;
#[cfg(verus_only)]
use crate::vspec::layout::{layout_align, layout_ok, layout_size};
#[cfg(verus_only)]
use crate::vspec::meta::{axiom_meta_lock_frame, axiom_meta_lock_inv};
use crate::vspec::unbacked::Unbacked;
use crate::vspec::zero::{Zeroed, write_zero};
#[cfg(verus_only)]
use vstd::arithmetic::div_mod::{lemma_add_mod_noop, lemma_sub_mod_noop};
#[cfg(verus_only)]
use vstd::arithmetic::power2::pow2;
#[cfg(verus_only)]
use vstd::bits::lemma_usize_shl_is_mul;
use vstd::prelude::*;
use vstd::raw_ptr::{PointsTo, PointsToRaw, ptr_mut_write, ptr_ref};
#[cfg(verus_only)]
use vstd::resource::Loc;
use vstd::resource::map::{GhostMapAuth, GhostPointsTo, GhostSubmap};
#[cfg(verus_only)]
use vstd::set_lib::{lemma_int_range, set_int_range};
#[cfg(verus_only)]
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

/// Bins are strictly increasing.
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
    let b = bin_of_spec(layout_size(layout) as int);
    if layout_align(layout) > MIN_ALIGN && b < 4 { 4 } else { b }
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

/// `x` is a power of two (the `is_power_of_two` of `vspec::core_int`).
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

/// `1` is a power of two (`2^0`).
pub proof fn lemma_is_pow2_one()
    ensures
        is_pow2(1),
{
    vstd::arithmetic::power2::lemma2_to64();
    assert(1usize as nat == pow2(0));
}

/// A power of two has one bit set.
pub proof fn lemma_pow2_bits(x: usize)
    requires
        is_pow2(x),
    ensures
        x > 0,
        x & sub(x, 1) == 0,
        x <= 0x8000_0000_0000_0000,
{
    let k = choose|k: nat| k < 64 && x as nat == #[trigger] pow2(k);
    vstd::arithmetic::power2::lemma2_to64();
    vstd::arithmetic::power2::lemma2_to64_rest();
    if k < 63 {
        vstd::arithmetic::power2::lemma_pow2_strictly_increases(k, 63);
    }
    vstd::bits::lemma_usize_pow2_no_overflow(k);
    lemma_usize_shl_is_mul(1, k as usize);
    let kk = k as usize;
    assert((1usize << kk) > 0 && (1usize << kk) & sub(1usize << kk, 1) == 0) by (bit_vector)
        requires
            kk < 64,
    ;
}

/// End of the user address space on every target: x86_64 with 5-level
/// paging and aarch64 with 52-bit addresses both stay below 2^57. The
/// metadata region lies below it, which keeps its address arithmetic far
/// from overflow.
pub open spec fn va_limit() -> int {
    0x200_0000_0000_0000
}

/// [`align_up`] as a function.
pub open spec fn align_up_spec(x: usize, a: usize) -> usize {
    ((x + a - 1) as usize) & !((a - 1) as usize)
}

/// `len` rounded up to whole 8-byte words: what [`Meta::reserve_table`]
/// holds of a table, so that a table of `u64` words ending in a partial
/// word (a class bitmap whose bit count is not a multiple of 64) owns that
/// word.
pub open spec fn words_up(len: int) -> int {
    (len + 7) / 8 * 8
}

/// A length rounded up to whole words fits in a room of whole pages that
/// holds the length.
proof fn lemma_words_up_fit(page: usize, room: int, len: int)
    requires
        MIN_ALIGN <= page,
        page & sub(page, 1) == 0,
        room % (page as int) == 0,
        0 <= len <= room,
    ensures
        words_up(len) <= room,
{
    assert(page % 8 == 0) by (bit_vector)
        requires
            16 <= page,
            page & sub(page, 1) == 0,
    ;
    let (p, k) = (page as int, page as int / 8);
    let m = (room / p) * k;
    assert(room == m * 8) by {
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(room, p);
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(p, 8);
        assert(room == m * 8) by (nonlinear_arith)
            requires
                room == p * (room / p),
                p == 8 * k,
                m == (room / p) * k,
        ;
    }
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(room + 7, 8, m, 7);
    vstd::arithmetic::div_mod::lemma_div_is_ordered(len + 7, room + 7, 8);
    assert((len + 7) / 8 * 8 <= m * 8) by (nonlinear_arith)
        requires
            (len + 7) / 8 <= m,
    ;
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
    /// plus the reservation side's commits.
    pub writable: usize,
    /// Bytes of tables carved from the object side (`reserve_table`,
    /// `reserve_committed` up to `EAGER_MAX`), all writable.
    pub eager: usize,
}

impl MetaStats {
    /// The identities of the type's doc.
    pub open spec fn identities(&self) -> bool {
        &&& self.allocated + self.eager <= self.bumped
        &&& self.bumped <= self.writable
        &&& self.writable <= self.len
        &&& self.committed <= self.writable
        &&& self.bumped + self.reserved <= self.len
    }
}

pub(crate) struct FreeNode {
    next: *mut FreeNode,
}

#[cfg_attr(not(verus_keep_ghost), allow(unused_braces))]
global layout FreeNode is size == 8, align == 8;

/// Whether small tables may be carved from the object side (the module
/// doc): decided on the first table from the kernel's overcommit mode and
/// the THP policy in force, never assumed. A `u8` with three values, not
/// an enum: an enum's niche makes rustc regroup every field of `Inner`
/// and the offsets the inlined object paths use would move (an enum here
/// changes the machine code of `Meta::free` and `claim_with`).
const EAGER_UNPROBED: u8 = 0;
const EAGER_ON: u8 = 1;
const EAGER_OFF: u8 = 2;

pub(crate) struct Inner {
    base: usize,
    len: usize,
    page: usize,
    /// Next byte for objects and eagerly carved tables. `base <= bump <=
    /// committed <= top`.
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
    /// Ghost: the blocks on each bin's free list, head first.
    #[allow(dead_code)]
    lists: Ghost<Seq<Seq<*mut FreeNode>>>,
    /// Ghost: the permission to each free block's link word.
    #[allow(dead_code)]
    perms: Tracked<Map<*mut FreeNode, PointsTo<FreeNode>>>,
    /// Ghost: the live objects, address to bin. [`Meta::alloc`] records
    /// each object it hands out; the claim to one, a `GhostPointsTo` of
    /// this map, is what [`Meta::free`] gives back.
    #[allow(dead_code)]
    live: Tracked<GhostMapAuth<*mut FreeNode, nat>>,
    /// Ghost: the reserved bytes no commit has claimed yet, `true` on the
    /// reservation side, `false` in an eager table. [`reserve`] and
    /// [`reserve_eager`] record each reservation they hand out; the claim
    /// to its bytes, a `GhostSubmap` of this map, is what [`commit`] takes.
    ///
    /// [`reserve`]: Inner::reserve
    /// [`reserve_eager`]: Inner::reserve_eager
    /// [`commit`]: Inner::commit
    #[allow(dead_code)]
    pages: Tracked<GhostMapAuth<int, bool>>,
    /// Ghost: the permission to the committed bytes above the bump
    /// pointer, `[bump, committed)`, from which [`alloc`] hands out each
    /// fresh object's.
    ///
    /// [`alloc`]: Inner::alloc
    #[allow(dead_code)]
    fresh: Tracked<PointsToRaw>,
    /// Ghost: the uncommitted address space between the object side and
    /// the reservations, `[committed, top)`, which grants no access;
    /// [`commit_bump_to`] moves the front it commits to `fresh`, and
    /// [`reserve`] gives up the back it reserves.
    ///
    /// [`commit_bump_to`]: Inner::commit_bump_to
    /// [`reserve`]: Inner::reserve
    #[allow(dead_code)]
    unbacked: Tracked<Unbacked>,
    /// Ghost: the permission to each free block's bytes past its link
    /// word, which [`alloc`] joins back to the link word's.
    ///
    /// [`alloc`]: Inner::alloc
    #[allow(dead_code)]
    tails: Tracked<Map<*mut FreeNode, PointsToRaw>>,
    /// Ghost: the permission to the table [`reserve_eager`] carved or
    /// [`reserve_committed`] committed last, for [`Meta::reserve_committed_raw`]
    /// to take; `wf` says nothing of it.
    ///
    /// [`reserve_eager`]: Inner::reserve_eager
    /// [`reserve_committed`]: Inner::reserve_committed
    #[allow(dead_code)]
    handed: Tracked<PointsToRaw>,
    /// Ghost: the token that `fresh`'s bytes read as zero. They came out
    /// of `unbacked` (`Unbacked::commit`), and since then only `fresh`
    /// has held a permission to them.
    #[allow(dead_code)]
    fresh_zero: Tracked<Zeroed>,
    /// Ghost: the zero token of the fresh object [`alloc`] or the table
    /// [`reserve_eager`] or [`reserve_committed`] handed out last, for
    /// [`Meta::alloc_zeroed`] and [`Meta::reserve_committed_raw`] to take;
    /// `wf` says nothing of it.
    ///
    /// [`alloc`]: Inner::alloc
    /// [`reserve_eager`]: Inner::reserve_eager
    /// [`reserve_committed`]: Inner::reserve_committed
    #[allow(dead_code)]
    handed_zero: Tracked<Zeroed>,
}

/// Bytes on the free lists of bins `0..n`.
spec fn free_bytes(lists: Seq<Seq<*mut FreeNode>>, n: nat) -> int
    decreases n,
{
    if n == 0 {
        0
    } else {
        free_bytes(lists, (n - 1) as nat) + lists[n - 1].len() * bin_size(n - 1)
    }
}

proof fn lemma_free_bytes_nonneg(lists: Seq<Seq<*mut FreeNode>>, n: nat)
    requires
        n <= NBINS,
    ensures
        free_bytes(lists, n) >= 0,
    decreases n,
{
    if n > 0 {
        lemma_free_bytes_nonneg(lists, (n - 1) as nat);
        lemma_bin(n - 1);
        assert(lists[n - 1].len() * bin_size(n - 1) >= 0) by (nonlinear_arith)
            requires
                bin_size(n - 1) >= 0,
        ;
    }
}

proof fn lemma_free_bytes_update(lists: Seq<Seq<*mut FreeNode>>, b: int, l: Seq<*mut FreeNode>, n: nat)
    requires
        0 <= b < lists.len(),
        n <= lists.len(),
    ensures
        free_bytes(lists.update(b, l), n) == free_bytes(lists, n) + (if b < n {
            (l.len() - lists[b].len()) * bin_size(b)
        } else {
            0
        }),
    decreases n,
{
    if n > 0 {
        lemma_free_bytes_update(lists, b, l, (n - 1) as nat);
        if b == n - 1 {
            assert(l.len() * bin_size(b) - lists[b].len() * bin_size(b) == (l.len() - lists[b].len()) * bin_size(b))
                by (nonlinear_arith);
        }
    }
}

/// The unclaimed reserved bytes of `m` on one side: the reservation
/// side (`true`) or the eager tables of the object side (`false`).
spec fn side_set(m: Map<int, bool>, s: bool) -> Set<int> {
    m.dom().filter(|a: int| m[a] == s)
}

/// The claim a reservation of `[lo, hi)` on side `s` hands out.
spec fn claim_map(lo: int, hi: int, s: bool) -> Map<int, bool> {
    Map::new(set_int_range(lo, hi), |a: int| s)
}

/// Recording the bytes `[lo, hi)`, none recorded yet, on side `s` adds
/// `hi - lo` to that side and leaves the other alone.
proof fn lemma_side_mint(m: Map<int, bool>, lo: int, hi: int, s: bool)
    requires
        lo <= hi,
        m.dom().disjoint(set_int_range(lo, hi)),
    ensures
        side_set(m.union_prefer_right(claim_map(lo, hi, s)), s).len() == side_set(m, s).len() + (hi - lo),
        side_set(m.union_prefer_right(claim_map(lo, hi, s)), !s) == side_set(m, !s),
{
    let r = set_int_range(lo, hi);
    let m2 = m.union_prefer_right(claim_map(lo, hi, s));
    lemma_int_range(lo, hi);
    assert(side_set(m2, s) =~= side_set(m, s) + r);
    assert(side_set(m2, !s) =~= side_set(m, !s));
    assert(side_set(m, s).disjoint(r));
    vstd::set_lib::lemma_set_disjoint_lens(side_set(m, s), r);
}

/// A byte of a claim is recorded, with the claim's side.
proof fn lemma_claim_at(c: Map<int, bool>, m: Map<int, bool>, a: int)
    requires
        c <= m,
        c.dom().contains(a),
    ensures
        m.contains_key(a),
        m[a] == c[a],
{
    assert(c.dom().contains(a) ==> m.dom().contains(a) && c[a] == m[a]);
}

/// Giving back `c`, a claim of bytes on side `s`, takes its size off that
/// side and leaves the other alone.
proof fn lemma_side_consume(m: Map<int, bool>, c: Map<int, bool>, s: bool)
    requires
        c <= m,
        forall|a: int| #[trigger] c.dom().contains(a) ==> c[a] == s,
    ensures
        c.dom().subset_of(side_set(m, s)),
        side_set(m.remove_keys(c.dom()), s).len() == side_set(m, s).len() - c.dom().len(),
        side_set(m.remove_keys(c.dom()), !s) == side_set(m, !s),
{
    let m2 = m.remove_keys(c.dom());
    let t = side_set(m, s);
    assert forall|a: int| c.dom().contains(a) implies t.contains(a) by {
        lemma_claim_at(c, m, a);
    }
    assert(side_set(m2, s) =~= t.difference(c.dom()));
    assert(side_set(m2, !s) =~= side_set(m, !s));
    assert(t.intersect(c.dom()) =~= c.dom());
    vstd::set_lib::lemma_set_difference_len(t, c.dom());
}

/// The live objects of bin `b` in `m`, a map from address to bin.
spec fn live_set(m: Map<*mut FreeNode, nat>, b: int) -> Set<*mut FreeNode> {
    m.dom().filter(|k: *mut FreeNode| m[k] == b)
}

/// Bytes of the live objects of bins `0..n` in `m`.
spec fn live_bytes(m: Map<*mut FreeNode, nat>, n: nat) -> int
    decreases n,
{
    if n == 0 {
        0
    } else {
        live_bytes(m, (n - 1) as nat) + live_set(m, n - 1).len() * bin_size(n - 1)
    }
}

proof fn lemma_live_bytes_nonneg(m: Map<*mut FreeNode, nat>, n: nat)
    requires
        n <= NBINS,
    ensures
        live_bytes(m, n) >= 0,
    decreases n,
{
    if n > 0 {
        lemma_live_bytes_nonneg(m, (n - 1) as nat);
        lemma_bin(n - 1);
        assert(live_set(m, n - 1).len() * bin_size(n - 1) >= 0) by (nonlinear_arith)
            requires
                bin_size(n - 1) >= 0,
        ;
    }
}

/// A change of `d` objects in bin `b` alone changes the bytes by `d` of
/// the bin's size.
proof fn lemma_live_bytes_update(m: Map<*mut FreeNode, nat>, m2: Map<*mut FreeNode, nat>, b: int, d: int, n: nat)
    requires
        forall|c: int| c != b ==> #[trigger] live_set(m2, c).len() == live_set(m, c).len(),
        live_set(m2, b).len() == live_set(m, b).len() + d,
    ensures
        live_bytes(m2, n) == live_bytes(m, n) + (if 0 <= b < n {
            d * bin_size(b)
        } else {
            0
        }),
    decreases n,
{
    if n > 0 {
        lemma_live_bytes_update(m, m2, b, d, (n - 1) as nat);
        if b == n - 1 {
            let (x2, x, s) = (live_set(m2, b).len() as int, live_set(m, b).len() as int, bin_size(b));
            assert(x2 * s == x * s + d * s) by (nonlinear_arith)
                requires
                    x2 == x + d,
            ;
        } else {
            assert(live_set(m2, n - 1).len() == live_set(m, n - 1).len());
        }
    }
}

/// No live objects, no bytes.
proof fn lemma_live_bytes_empty(n: nat)
    ensures
        live_bytes(Map::empty(), n) == 0,
    decreases n,
{
    if n > 0 {
        lemma_live_bytes_empty((n - 1) as nat);
        let e = Map::<*mut FreeNode, nat>::empty();
        assert(live_set(e, n - 1) =~= Set::empty());
        assert(live_set(e, n - 1).len() == 0);
        assert(live_set(e, n - 1).len() * bin_size(n - 1) == 0);
    }
}

/// Recording a fresh object of bin `b` adds its size.
proof fn lemma_live_insert(m: Map<*mut FreeNode, nat>, k: *mut FreeNode, b: nat)
    requires
        !m.contains_key(k),
        b < NBINS,
    ensures
        live_bytes(m.insert(k, b), NBINS as nat) == live_bytes(m, NBINS as nat) + bin_size(b as int),
{
    let m2 = m.insert(k, b);
    assert forall|c: int| c != b implies #[trigger] live_set(m2, c).len() == live_set(m, c).len() by {
        assert(live_set(m2, c) =~= live_set(m, c));
    }
    assert(live_set(m2, b as int) =~= live_set(m, b as int).insert(k));
    assert(!live_set(m, b as int).contains(k));
    lemma_live_bytes_update(m, m2, b as int, 1, NBINS as nat);
}

/// Forgetting the object at `k` subtracts its size, which the bytes hold.
proof fn lemma_live_remove(m: Map<*mut FreeNode, nat>, k: *mut FreeNode)
    requires
        m.contains_key(k),
        m[k] < NBINS,
    ensures
        live_bytes(m.remove(k), NBINS as nat) == live_bytes(m, NBINS as nat) - bin_size(m[k] as int),
        live_bytes(m, NBINS as nat) >= bin_size(m[k] as int),
{
    let m2 = m.remove(k);
    let b = m[k] as int;
    assert forall|c: int| c != b implies #[trigger] live_set(m2, c).len() == live_set(m, c).len() by {
        assert(live_set(m2, c) =~= live_set(m, c));
    }
    assert(live_set(m2, b) =~= live_set(m, b).remove(k));
    assert(live_set(m, b).contains(k));
    lemma_live_bytes_update(m, m2, b, -1, NBINS as nat);
    lemma_live_bytes_nonneg(m2, NBINS as nat);
}

/// The metadata allocator for one region. One per process: `heap/`
/// writes it in place with [`Meta::owning`] when the regions are reserved.
pub struct Meta {
    inner: Lock<Inner>,
}

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
proof fn lemma_align_bits(x: usize, a: usize, y: usize)
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

/// [`align_up`]'s postcondition, of the spec function.
pub(crate) proof fn lemma_align_up(x: usize, a: usize)
    requires
        a > 0,
        a & sub(a, 1) == 0,
        x + a <= usize::MAX,
    ensures
        x <= align_up_spec(x, a),
        align_up_spec(x, a) - x < a,
        align_up_spec(x, a) % a == 0,
        x % a == 0 ==> align_up_spec(x, a) == x,
{
    lemma_align_bits(x, a, (x + a - 1) as usize);
}

/// The commit step, `COMMIT_CHUNK` rounded up to a page, is a power of two
/// and a multiple of the page, and no more than one of them plus a page.
proof fn lemma_step(page: usize, step: usize)
    requires
        page >= MIN_ALIGN,
        page & sub(page, 1) == 0,
        page <= va_limit(),
        step == align_up_spec(COMMIT_CHUNK, page),
    ensures
        step > 0,
        step & sub(step, 1) == 0,
        step % page == 0,
        step <= COMMIT_CHUNK + page,
{
    lemma_align_up(COMMIT_CHUNK, page);
    if page <= COMMIT_CHUNK {
        assert(COMMIT_CHUNK & sub(COMMIT_CHUNK, 1) == 0) by (compute_only);
        lemma_pow2_divides(COMMIT_CHUNK, page);
        assert(step == COMMIT_CHUNK);
    } else {
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(step as int, page as int);
        let k = step as int / page as int;
        assert(k == 1) by (nonlinear_arith)
            requires
                step as int == page * k,
                COMMIT_CHUNK <= step,
                step < COMMIT_CHUNK + page,
                COMMIT_CHUNK < page,
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

/// Of two powers of two, the larger is a multiple of the smaller.
proof fn lemma_pow2_divides(a: usize, p: usize)
    requires
        a & sub(a, 1) == 0,
        p > 0,
        p & sub(p, 1) == 0,
        a >= p,
    ensures
        a % p == 0,
{
    assert(a & sub(p, 1) == 0) by (bit_vector)
        requires
            a & sub(a, 1) == 0,
            p > 0,
            p & sub(p, 1) == 0,
            a >= p,
    ;
    lemma_mod_mask(a, p);
}

/// Divisibility is transitive.
pub(crate) proof fn lemma_mod_trans(r: int, s: int, p: int)
    requires
        r >= 0,
        s > 0,
        p > 0,
        r % s == 0,
        s % p == 0,
    ensures
        r % p == 0,
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(r, s);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(s, p);
    let q = r / s;
    let t = s / p;
    assert(r == (t * q) * p) by (nonlinear_arith)
        requires
            r == s * q,
            s == p * t,
    ;
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(t * q, p);
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

/// `PROT_READ | PROT_WRITE`.
const PROT_RW: i32 = 3;

} // verus!

const _: () = assert!(PROT_RW == crate::sys::PROT_READ | crate::sys::PROT_WRITE);

verus! {

fn commit_range(addr: usize, len: usize) -> Result<(), Errno> {
    // SAFETY: the range is inside our own PROT_NONE reservation and nothing
    // is mapped over it; adding permissions cannot invalidate anything.
    unsafe { crate::sys::mprotect(addr, len, PROT_RW) }
}

/// Entry `i` of bin `b`'s free list: a block of the bin's size and
/// alignment in `[base, bump)` whose link word `perms` holds, pointing at
/// the next entry.
spec fn node_ok(
    lists: Seq<Seq<*mut FreeNode>>,
    perms: Map<*mut FreeNode, PointsTo<FreeNode>>,
    base: usize,
    bump: usize,
    b: int,
    i: int,
) -> bool {
    let l = lists[b];
    let p = l[i];
    &&& perms.contains_key(p)
    &&& perms[p].ptr() == p
    &&& perms[p].is_init()
    &&& perms[p].value().next == (if i + 1 < l.len() {
        l[i + 1]
    } else {
        ptr::null_mut::<FreeNode>()
    })
    &&& p as usize != 0
    &&& base <= p as usize
    &&& p as usize + bin_size(b) <= bump
    &&& (p as usize) as int % bin_align_spec(b) == 0
}

/// Bin `b`'s free list starts at `free[b]` (null when empty) and every
/// entry is well formed.
spec fn list_ok(
    free: Seq<*mut FreeNode>,
    lists: Seq<Seq<*mut FreeNode>>,
    perms: Map<*mut FreeNode, PointsTo<FreeNode>>,
    base: usize,
    bump: usize,
    b: int,
) -> bool {
    let l = lists[b];
    &&& (l.len() == 0) == (free[b] == ptr::null_mut::<FreeNode>())
    &&& l.len() > 0 ==> free[b] == l[0]
    &&& forall|i: int| 0 <= i < l.len() ==> #[trigger] node_ok(lists, perms, base, bump, b, i)
}

/// No block is on two lists, or twice on one.
spec fn distinct(lists: Seq<Seq<*mut FreeNode>>) -> bool {
    forall|b1: int, i1: int, b2: int, i2: int|
        0 <= b1 < NBINS && 0 <= b2 < NBINS && 0 <= i1 < lists[b1].len() && 0 <= i2 < lists[b2].len() && (b1 != b2
            || i1 != i2) ==> #[trigger] lists[b1][i1] != #[trigger] lists[b2][i2]
}

/// Popping bin `bin`'s head keeps every list well formed.
proof fn lemma_pop(
    free: Seq<*mut FreeNode>,
    lists: Seq<Seq<*mut FreeNode>>,
    perms: Map<*mut FreeNode, PointsTo<FreeNode>>,
    base: usize,
    bump: usize,
    bin: int,
)
    requires
        0 <= bin < NBINS,
        free.len() == NBINS,
        lists.len() == NBINS,
        forall|b: int| 0 <= b < NBINS ==> #[trigger] list_ok(free, lists, perms, base, bump, b),
        distinct(lists),
        lists[bin].len() > 0,
    ensures
        ({
            let l = lists[bin];
            let nlists = lists.update(bin, l.drop_first());
            let nperms = perms.remove(l[0]);
            let nfree = free.update(bin, perms[l[0]].value().next);
            &&& forall|b: int| 0 <= b < NBINS ==> #[trigger] list_ok(nfree, nlists, nperms, base, bump, b)
            &&& distinct(nlists)
        }),
{
    let l = lists[bin];
    let nlists = lists.update(bin, l.drop_first());
    let nperms = perms.remove(l[0]);
    let nfree = free.update(bin, perms[l[0]].value().next);
    assert(list_ok(free, lists, perms, base, bump, bin));
    assert(node_ok(lists, perms, base, bump, bin, 0));
    assert forall|b: int| 0 <= b < NBINS implies #[trigger] list_ok(nfree, nlists, nperms, base, bump, b) by {
        assert(list_ok(free, lists, perms, base, bump, b));
        if b == bin {
            assert forall|i: int| 0 <= i < nlists[b].len() implies #[trigger] node_ok(nlists, nperms, base, bump, b, i) by {
                assert(node_ok(lists, perms, base, bump, b, i + 1));
                assert(lists[b][i + 1] != lists[bin][0]);
            }
            if nlists[b].len() > 0 {
                assert(node_ok(lists, perms, base, bump, b, 1));
            }
        } else {
            assert forall|i: int| 0 <= i < nlists[b].len() implies #[trigger] node_ok(nlists, nperms, base, bump, b, i) by {
                assert(node_ok(lists, perms, base, bump, b, i));
                assert(lists[b][i] != lists[bin][0]);
            }
        }
    }
    assert forall|b1: int, i1: int, b2: int, i2: int|
        0 <= b1 < NBINS && 0 <= b2 < NBINS && 0 <= i1 < nlists[b1].len() && 0 <= i2 < nlists[b2].len() && (b1 != b2
            || i1 != i2) implies #[trigger] nlists[b1][i1] != #[trigger] nlists[b2][i2] by {
        let j1 = if b1 == bin { i1 + 1 } else { i1 };
        let j2 = if b2 == bin { i2 + 1 } else { i2 };
        assert(nlists[b1][i1] == lists[b1][j1]);
        assert(nlists[b2][i2] == lists[b2][j2]);
    }
}

/// Pushing `node`, whose link word `perm` now points at the old head,
/// onto bin `bin`'s list keeps every list well formed.
proof fn lemma_push(
    free: Seq<*mut FreeNode>,
    lists: Seq<Seq<*mut FreeNode>>,
    perms: Map<*mut FreeNode, PointsTo<FreeNode>>,
    base: usize,
    bump: usize,
    bin: int,
    node: *mut FreeNode,
    perm: PointsTo<FreeNode>,
)
    requires
        0 <= bin < NBINS,
        free.len() == NBINS,
        lists.len() == NBINS,
        forall|b: int| 0 <= b < NBINS ==> #[trigger] list_ok(free, lists, perms, base, bump, b),
        distinct(lists),
        !perms.contains_key(node),
        perm.ptr() == node,
        perm.is_init(),
        perm.value().next == free[bin],
        node as usize != 0,
        base <= node as usize,
        node as usize + bin_size(bin) <= bump,
        node as usize as int % bin_align_spec(bin) == 0,
    ensures
        ({
            let nlists = lists.update(bin, seq![node].add(lists[bin]));
            let nperms = perms.insert(node, perm);
            let nfree = free.update(bin, node);
            &&& forall|b: int| 0 <= b < NBINS ==> #[trigger] list_ok(nfree, nlists, nperms, base, bump, b)
            &&& distinct(nlists)
        }),
{
    let l = lists[bin];
    let nl = seq![node].add(l);
    let nlists = lists.update(bin, nl);
    let nperms = perms.insert(node, perm);
    let nfree = free.update(bin, node);
    assert(list_ok(free, lists, perms, base, bump, bin));
    assert forall|b: int, i: int| 0 <= b < NBINS && 0 <= i < lists[b].len() implies lists[b][i] != node by {
        assert(list_ok(free, lists, perms, base, bump, b));
        assert(node_ok(lists, perms, base, bump, b, i));
    }
    assert forall|b: int| 0 <= b < NBINS implies #[trigger] list_ok(nfree, nlists, nperms, base, bump, b) by {
        assert(list_ok(free, lists, perms, base, bump, b));
        if b == bin {
            assert forall|i: int| 0 <= i < nlists[b].len() implies #[trigger] node_ok(nlists, nperms, base, bump, b, i) by {
                if i > 0 {
                    assert(node_ok(lists, perms, base, bump, b, i - 1));
                    assert(nl[i] == l[i - 1]);
                } else if l.len() > 0 {
                    assert(nl[1] == l[0]);
                }
            }
        } else {
            assert forall|i: int| 0 <= i < nlists[b].len() implies #[trigger] node_ok(nlists, nperms, base, bump, b, i) by {
                assert(node_ok(lists, perms, base, bump, b, i));
            }
        }
    }
    assert forall|b1: int, i1: int, b2: int, i2: int|
        0 <= b1 < NBINS && 0 <= b2 < NBINS && 0 <= i1 < nlists[b1].len() && 0 <= i2 < nlists[b2].len() && (b1 != b2
            || i1 != i2) implies #[trigger] nlists[b1][i1] != #[trigger] nlists[b2][i2] by {
        if b1 == bin && i1 > 0 {
            assert(nlists[b1][i1] == lists[b1][i1 - 1]);
        }
        if b2 == bin && i2 > 0 {
            assert(nlists[b2][i2] == lists[b2][i2 - 1]);
        }
    }
}

/// Raising the bump pointer keeps every list well formed.
proof fn lemma_bump(
    free: Seq<*mut FreeNode>,
    lists: Seq<Seq<*mut FreeNode>>,
    perms: Map<*mut FreeNode, PointsTo<FreeNode>>,
    base: usize,
    bump: usize,
    bump2: usize,
)
    requires
        bump <= bump2,
        forall|b: int| 0 <= b < NBINS ==> #[trigger] list_ok(free, lists, perms, base, bump, b),
    ensures
        forall|b: int| 0 <= b < NBINS ==> #[trigger] list_ok(free, lists, perms, base, bump2, b),
{
    assert forall|b: int| 0 <= b < NBINS implies #[trigger] list_ok(free, lists, perms, base, bump2, b) by {
        assert(list_ok(free, lists, perms, base, bump, b));
        assert forall|i: int| 0 <= i < lists[b].len() implies #[trigger] node_ok(lists, perms, base, bump2, b, i) by {
            assert(node_ok(lists, perms, base, bump, b, i));
        }
    }
}

/// The free block at `p`, of bin `b`: its bytes past the link word are
/// in `tails`, with the block's provenance.
spec fn tail_ok(tails: Map<*mut FreeNode, PointsToRaw>, p: *mut FreeNode, b: int) -> bool {
    &&& tails.contains_key(p)
    &&& tails[p].is_range(p as usize + 8, bin_size(b) - 8)
    &&& tails[p].provenance() == p@.provenance
}

/// Move the permission out of `r`, leaving an empty one of its
/// provenance.
proof fn take_raw(tracked r: &mut PointsToRaw) -> (tracked out: PointsToRaw)
    ensures
        out == *old(r),
        final(r).dom() == Set::<int>::empty(),
        final(r).provenance() == old(r).provenance(),
{
    let tracked mut t = PointsToRaw::empty(r.provenance());
    vstd::modes::tracked_swap(r, &mut t);
    t
}

/// Put `v` into `r`, dropping the permission `r` held.
proof fn put_raw(tracked r: &mut PointsToRaw, tracked v: PointsToRaw)
    ensures
        *final(r) == v,
{
    let tracked mut t = v;
    vstd::modes::tracked_swap(r, &mut t);
}

/// Move the token out of `u`, leaving an empty one.
proof fn take_unbacked(tracked u: &mut Unbacked) -> (tracked out: Unbacked)
    ensures
        out == *old(u),
{
    let tracked mut t = Unbacked::empty(0);
    vstd::modes::tracked_swap(u, &mut t);
    t
}

/// Put `v` into `u`.
proof fn put_unbacked(tracked u: &mut Unbacked, tracked v: Unbacked)
    ensures
        *final(u) == v,
{
    let tracked mut t = v;
    vstd::modes::tracked_swap(u, &mut t);
}

/// Move the token out of `z`, leaving an empty one.
proof fn take_zero(tracked z: &mut Zeroed) -> (tracked out: Zeroed)
    ensures
        out == *old(z),
{
    let tracked mut t = Zeroed::empty(0);
    vstd::modes::tracked_swap(z, &mut t);
    t
}

/// Put `v` into `z`.
proof fn put_zero(tracked z: &mut Zeroed, tracked v: Zeroed)
    ensures
        *final(z) == v,
{
    let tracked mut t = v;
    vstd::modes::tracked_swap(z, &mut t);
}

/// The `n` bytes `skip` bytes into `z`, and the rest past them; the first
/// `skip` bytes are dropped.
proof fn cut_zero(tracked z: Zeroed, skip: int, n: int) -> (tracked r: (Zeroed, Zeroed))
    requires
        0 <= skip,
        0 <= n,
        skip + n <= z.len(),
    ensures
        r.0.addr() == z.addr() + skip,
        r.0.len() == n,
        r.1.addr() == z.addr() + skip + n,
        r.1.len() == z.len() - skip - n,
{
    let tracked (_pad, rest) = z.split(skip);
    rest.split(n)
}

/// `[lo, hi)` split at `mid`.
proof fn split_at(tracked r: PointsToRaw, lo: int, mid: int, hi: int) -> (tracked res: (PointsToRaw, PointsToRaw))
    requires
        r.is_range(lo, hi - lo),
        lo <= mid <= hi,
    ensures
        res.0.is_range(lo, mid - lo),
        res.1.is_range(mid, hi - mid),
        res.0.provenance() == r.provenance(),
        res.1.provenance() == r.provenance(),
{
    let tracked res = r.split(Set::range(lo, mid));
    assert(res.0.dom() =~= Set::range(lo, mid));
    assert(res.1.dom() =~= Set::range(mid, hi));
    res
}

/// The `[lo, mid)` part of `raw` when it is `[lo, hi)` (else `raw`).
proof fn keep_prefix(tracked raw: PointsToRaw, lo: int, mid: int, hi: int) -> (tracked r: PointsToRaw)
    ensures
        raw.is_range(lo, hi - lo) && lo <= mid <= hi ==> r.is_range(lo, mid - lo),
        r.provenance() == raw.provenance(),
{
    if raw.is_range(lo, hi - lo) && lo <= mid <= hi {
        split_at(raw, lo, mid, hi).0
    } else {
        raw
    }
}

/// The first `k` bytes of `z` when it covers them (else `z`).
proof fn zero_prefix(tracked z: Zeroed, k: int) -> (tracked r: Zeroed)
    ensures
        0 <= k <= z.len() ==> r.addr() == z.addr() && r.len() == k,
{
    if 0 <= k <= z.len() {
        z.split(k).0
    } else {
        z
    }
}

/// `raw` when `keep`, else an empty permission of its provenance.
proof fn raw_if(tracked raw: PointsToRaw, keep: bool) -> (tracked r: PointsToRaw)
    ensures
        keep ==> r == raw,
        r.provenance() == raw.provenance(),
{
    if keep {
        raw
    } else {
        PointsToRaw::empty(raw.provenance())
    }
}

/// `[lo, mid)` and `[mid, hi)` joined.
proof fn join_at(tracked a: PointsToRaw, tracked b: PointsToRaw, lo: int, mid: int, hi: int) -> (tracked r: PointsToRaw)
    requires
        a.is_range(lo, mid - lo),
        b.is_range(mid, hi - mid),
        a.provenance() == b.provenance(),
        lo <= mid <= hi,
    ensures
        r.is_range(lo, hi - lo),
        r.provenance() == a.provenance(),
{
    let tracked r = a.join(b);
    assert(r.dom() =~= Set::range(lo, hi));
    r
}

impl Inner {
    /// The region's layout: page-aligned bounds in order, below
    /// [`va_limit`].
    spec fn region_wf(&self) -> bool {
        &&& MIN_ALIGN <= self.page
        &&& self.page & sub(self.page, 1) == 0
        &&& self.base > 0
        &&& self.base % self.page == 0
        &&& self.len % self.page == 0
        &&& self.len > 0
        &&& self.page <= self.len
        &&& self.base + self.len <= va_limit()
        &&& self.base <= self.bump <= self.committed <= self.top <= self.base + self.len
        &&& self.committed % self.page == 0
        &&& self.top % self.page == 0
    }

    /// The counters agree with the bounds, and every byte the bump pointer
    /// passed is a live object, a free block, an eager table or padding.
    spec fn stats_wf(&self) -> bool {
        &&& self.stats.len == self.len
        &&& self.stats.bumped == self.bump - self.base
        &&& self.stats.reserved == self.base + self.len - self.top
        &&& self.top_committed <= self.base + self.len - self.top
        &&& self.stats.committed <= (self.committed - self.base) + self.top_committed
        &&& self.stats.committed + self.stats.eager >= (self.committed - self.base) + self.top_committed
        &&& self.stats.allocated + free_bytes(self.lists@, NBINS as nat) + self.stats.eager <= self.stats.bumped
    }

    spec fn lists_wf(&self) -> bool {
        &&& self.lists@.len() == NBINS
        &&& forall|b: int| 0 <= b < NBINS ==> #[trigger] list_ok(self.free@, self.lists@, self.perms@, self.base, self.bump, b)
        &&& distinct(self.lists@)
        &&& forall|k: *mut FreeNode| #[trigger] self.perms@.contains_key(k) ==> {
            &&& self.perms@[k].ptr() == k
            &&& self.base <= k as usize
            &&& k as usize + 8 <= self.bump
        }
    }

    /// Every live object is a block of its bin inside the bump area,
    /// on no free list, and `allocated` is their bytes.
    spec fn live_wf(&self) -> bool {
        let m = self.live@@;
        &&& self.stats.allocated == live_bytes(m, NBINS as nat)
        &&& forall|k: *mut FreeNode| #[trigger] m.contains_key(k) ==> {
            &&& m[k] < NBINS
            &&& self.base <= k as usize
            &&& k as usize + bin_size(m[k] as int) <= self.bump
            &&& (k as usize) as int % bin_align_spec(m[k] as int) == 0
        }
        &&& forall|b: int, i: int|
            0 <= b < NBINS && 0 <= i < self.lists@[b].len() ==> !m.contains_key(#[trigger] self.lists@[b][i])
    }

    /// Every unclaimed byte lies in its side: at or above `top` on the
    /// reservation side, below the bump pointer in an eager table; and
    /// each side's commit counter has room for all of its unclaimed bytes.
    spec fn pages_wf(&self) -> bool {
        let m = self.pages@@;
        &&& forall|a: int| #[trigger] m.contains_key(a) ==> if m[a] {
            self.top <= a < self.base + self.len
        } else {
            self.base <= a < self.bump
        }
        &&& self.top_committed + side_set(m, true).len() <= self.base + self.len - self.top
        &&& self.stats.committed + side_set(m, false).len() <= (self.committed - self.base) + self.top_committed
    }

    /// The ghost state [`Inner`] holds for its bytes: the permission to the
    /// committed bytes above the bump pointer (with the provenance of the
    /// region's addresses) and the token that they read as zero, the
    /// uncommitted address space below the reservations, and the permission
    /// to each free block's bytes past its link word.
    spec fn raw_wf(&self) -> bool {
        &&& self.fresh@.is_range(self.bump as int, self.committed - self.bump)
        &&& self.fresh@.provenance() == cast_provenance()
        &&& self.fresh_zero@.addr() == self.bump
        &&& self.fresh_zero@.len() == self.committed - self.bump
        &&& self.unbacked@.addr() == self.committed
        &&& self.unbacked@.len() == self.top - self.committed
        &&& forall|b: int, i: int|
            0 <= b < NBINS && 0 <= i < self.lists@[b].len() ==> #[trigger] tail_ok(self.tails@, self.lists@[b][i], b)
    }

    /// The ghost identity of the unclaimed-byte map, which
    /// [`Meta::commit`]'s claims name.
    pub(crate) closed spec fn pages_id(&self) -> Loc {
        self.pages@.id()
    }

    /// The page size.
    pub(crate) closed spec fn spec_page(&self) -> usize {
        self.page
    }

    /// The ghost identity of the live-object map, which [`Meta::free`]'s
    /// claims name.
    pub(crate) closed spec fn live_id(&self) -> Loc {
        self.live@.id()
    }

    /// The region's first byte.
    pub(crate) closed spec fn spec_base(&self) -> usize {
        self.base
    }

    /// The region's length in bytes.
    pub(crate) closed spec fn spec_len(&self) -> usize {
        self.len
    }

    /// The link word of every free block, the only memory [`Inner`] writes
    /// through a permission ([`Inner::free`]), lies in the region
    /// (`lists_wf`).
    proof fn lemma_links_in_region(&self)
        requires
            self.wf(),
        ensures
            forall|k: *mut FreeNode| #[trigger] self.perms@.contains_key(k) ==> {
                &&& self.perms@[k].ptr() == k
                &&& self.base <= k as usize
                &&& k as usize + 8 <= self.base + self.len
            },
    {
    }

    /// `a` is the address of a free object's link word: a block on one of
    /// the free lists, whose permission [`Inner::free`] keeps and
    /// [`Inner::alloc`] writes through (whole-allocator theorem 4).
    pub(crate) closed spec fn is_link(&self, a: int) -> bool {
        exists|k: *mut FreeNode| #[trigger] self.perms@.contains_key(k) && k@.addr == a
    }

    /// A free object's link word lies in the region
    /// ([`Inner::lemma_links_in_region`]).
    pub(crate) proof fn lemma_link_in_region(&self, a: int)
        requires
            self.wf(),
            self.is_link(a),
        ensures
            self.spec_base() <= a,
            a + 8 <= self.spec_base() + self.spec_len(),
    {
        self.lemma_links_in_region();
        let k = choose|k: *mut FreeNode| #[trigger] self.perms@.contains_key(k) && k@.addr == a;
    }

    /// The invariant of the metadata lock.
    pub(crate) closed spec fn wf(&self) -> bool {
        &&& self.region_wf()
        &&& self.stats_wf()
        &&& self.lists_wf()
        &&& self.live_wf()
        &&& self.pages_wf()
        &&& self.raw_wf()
    }

    const fn new(base: usize, len: usize, page: usize, Tracked(space): Tracked<Unbacked>) -> (r: Inner)
        requires
            is_pow2(page),
            page >= MIN_ALIGN,
            base > 0,
            base % page == 0,
            len % page == 0,
            len > 0,
            base + len <= va_limit(),
            space.addr() == base,
            space.len() == len,
        ensures
            r.wf(),
            r.base == base,
            r.len == len,
    {
        proof {
            lemma_pow2_bits(page);
        }
        assert!(page.is_power_of_two() && page >= MIN_ALIGN, "meta: bad page size");
        assert!(base % page == 0 && len % page == 0 && len > 0, "meta: region not page aligned");
        assert!(base.checked_add(len).is_some(), "meta: region wraps");
        let tracked perms = Map::tracked_empty();
        let tracked (live, _) = GhostMapAuth::<*mut FreeNode, nat>::new(Map::empty());
        let tracked (pages, _) = GhostMapAuth::<int, bool>::new(Map::empty());
        let tracked fresh = PointsToRaw::empty(cast_provenance());
        let tracked unbacked = space;
        let tracked tails = Map::tracked_empty();
        let tracked handed = PointsToRaw::empty(cast_provenance());
        let tracked fresh_zero = Zeroed::empty(base as int);
        let tracked handed_zero = Zeroed::empty(0);
        let r = Inner {
            base,
            len,
            page,
            bump: base,
            committed: base,
            top: base + len,
            free: [ptr::null_mut(); NBINS],
            stats: MetaStats { len, committed: 0, allocated: 0, bumped: 0, reserved: 0, writable: 0, eager: 0 },
            top_committed: 0,
            eager: EAGER_UNPROBED,
            lists: Ghost(Seq::new(NBINS as nat, |b: int| Seq::empty())),
            perms: Tracked(perms),
            live: Tracked(live),
            pages: Tracked(pages),
            fresh: Tracked(fresh),
            unbacked: Tracked(unbacked),
            tails: Tracked(tails),
            handed: Tracked(handed),
            fresh_zero: Tracked(fresh_zero),
            handed_zero: Tracked(handed_zero),
        };
        proof {
            assert(free_bytes(r.lists@, NBINS as nat) == 0) by {
                lemma_free_bytes_zero(r.lists@, NBINS as nat);
            }
            lemma_live_bytes_empty(NBINS as nat);
            assert(side_set(Map::<int, bool>::empty(), true) =~= Set::empty());
            assert(side_set(Map::<int, bool>::empty(), false) =~= Set::empty());
            assert((base + len) % (page as int) == 0) by {
                lemma_add_mod_noop(base as int, len as int, page as int);
                vstd::arithmetic::div_mod::lemma_small_mod(0, page as nat);
            }
            if len < page {
                vstd::arithmetic::div_mod::lemma_small_mod(len as nat, page as nat);
            }
        }
        r
    }

    /// Make the bump area writable through `end`, in `COMMIT_CHUNK` steps,
    /// never past `top`. Returns the bytes newly made writable; the caller
    /// decides how much of that counts as `committed` (an object's chunk
    /// does whole, an eagerly carved table's own bytes do not: its owner's
    /// `commit` calls count that table's pages as it uses them).
    fn commit_bump_to(&mut self, end: usize) -> (r: Result<usize, Errno>)
        requires
            old(self).region_wf(),
            old(self).raw_wf(),
            old(self).base <= end <= old(self).top,
        ensures
            final(self).region_wf(),
            final(self).raw_wf(),
            r is Err ==> *final(self) == *old(self),
            r matches Ok(delta) ==> {
                &&& final(self).committed == old(self).committed + delta
                &&& end <= final(self).committed
                &&& *final(self) == (Inner {
                    committed: final(self).committed,
                    fresh: final(self).fresh,
                    unbacked: final(self).unbacked,
                    fresh_zero: final(self).fresh_zero,
                    ..*old(self)
                })
            },
    {
        debug_assert!(end <= self.top);
        // Chunks are measured from the base: the region is only page
        // aligned, so rounding the absolute address would make the first
        // chunk short and every later one misaligned against the counter.
        let step = align_up(COMMIT_CHUNK, self.page);
        proof {
            let page = self.page;
            let e = (end - self.base) as usize;
            lemma_step(page, step);
            lemma_align_up(e, step);
            let r = align_up_spec(e, step);
            lemma_mod_trans(r as int, step as int, page as int);
            lemma_add_mod_noop(self.base as int, r as int, page as int);
            vstd::arithmetic::div_mod::lemma_small_mod(0, page as nat);
        }
        let want = (self.base + align_up(end - self.base, step)).min(self.top);
        if want > self.committed {
            commit_range(self.committed, want - self.committed)?;
            // Metadata THP (`thp.rs`): the chunk is the folio's size or a
            // multiple of it (the region base is `MAX_SLAB`-aligned), so
            // the first touch of a heap or cache maps one folio.
            if crate::thp::meta_folio_size() != 0 {
                crate::thp::advise_meta(self.committed, want - self.committed);
            }
            let delta = want - self.committed;
            proof {
                // The pages just committed move from `unbacked` to `fresh`,
                // with their zero token.
                let (lo, c0) = (self.bump as int, self.committed as int);
                let tracked u = take_unbacked(self.unbacked.borrow_mut());
                let tracked (a, b) = u.split(want - c0);
                put_unbacked(self.unbacked.borrow_mut(), b);
                // `commit_range` returned `Ok` for exactly `[c0, want)`.
                let tracked (raw, z) = a.commit();
                let tracked f = take_raw(self.fresh.borrow_mut());
                put_raw(self.fresh.borrow_mut(), join_at(f, raw, lo, c0, want as int));
                let tracked fz = take_zero(self.fresh_zero.borrow_mut());
                put_zero(self.fresh_zero.borrow_mut(), fz.join(z));
            }
            self.committed = want;
            return Ok(delta);
        }
        Ok(0)
    }

    /// Whether a table of `len` bytes goes on the object side (the module
    /// doc): small enough, and the kernel neither charges writable bytes
    /// against a commit limit nor may back the region with PMD pages.
    fn eager_ok(&mut self, len: usize) -> (r: bool)
        ensures
            r ==> len <= EAGER_MAX,
            *final(self) == (Inner { eager: final(self).eager, ..*old(self) }),
    {
        if len > EAGER_MAX {
            return false;
        }
        if self.eager == EAGER_UNPROBED {
            self.eager = if eager_policy() { EAGER_ON } else { EAGER_OFF };
        }
        self.eager == EAGER_ON
    }

    /// Carve a fully writable, page-aligned, never freed table of `len`
    /// bytes aligned to `align` from the object side. `None` when it would
    /// reach the reservation side or the kernel refuses the commit (the
    /// caller then falls back to a reservation).
    fn reserve_eager(
        &mut self,
        len: usize,
        align: usize,
        Tracked(claim): Tracked<&mut GhostSubmap<int, bool>>,
    ) -> (r: Option<usize>)
        requires
            old(self).wf(),
            len <= EAGER_MAX,
            is_pow2(align),
        ensures
            final(self).wf(),
            final(self).live_id() == old(self).live_id(),
            final(self).pages_id() == old(self).pages_id(),
            r is None ==> *final(self) == *old(self),
            r matches Some(s) ==> {
                let tlen = align_up_spec(if len == 0 { 1 } else { len }, old(self).page);
                &&& final(claim).id() == final(self).pages_id()
                &&& final(claim)@ == claim_map(s as int, s + tlen, false)
                &&& final(self).handed@.is_range(s as int, tlen as int)
                &&& final(self).handed@.provenance() == cast_provenance()
                &&& final(self).handed_zero@.addr() == s && final(self).handed_zero@.len() == tlen
                &&& old(self).bump <= s
                &&& s % align == 0
                &&& s % old(self).page == 0
                &&& len <= tlen
                &&& s + tlen == final(self).bump
                &&& final(self).bump <= final(self).committed
                &&& final(self).top == old(self).top
                &&& final(self).stats.committed + tlen <= (final(self).committed - final(self).base)
                    + final(self).top_committed
                &&& final(self).stats.eager == old(self).stats.eager + tlen
                &&& final(self).base == old(self).base
                &&& final(self).page == old(self).page
                &&& final(self).len == old(self).len
                &&& final(self).top_committed == old(self).top_committed
            },
    {
        let ghost align0 = align;
        proof {
            lemma_pow2_bits(align);
            lemma_consts();
        }
        let align = align.max(self.page);
        let len = align_up(len.max(1), self.page);
        let start = align_up(self.bump, align);
        proof {
            lemma_pow2_divides(align, self.page);
            lemma_mod_trans(start as int, align as int, self.page as int);
            lemma_pow2_divides(align, align0);
            lemma_mod_trans(start as int, align as int, align0 as int);
        }
        let end = start.checked_add(len)?;
        if end > self.top {
            return None;
        }
        let delta = if end > self.committed { self.commit_bump_to(end).ok()? } else { 0 };
        proof {
            lemma_free_bytes_nonneg(self.lists@, NBINS as nat);
            lemma_bump(self.free@, self.lists@, self.perms@, self.base, self.bump, end);
        }
        // The object side counts as committed everything below its frontier
        // except the tables carved from it (their owners' `commit` calls
        // count their pages as they use them): the chunk's slack around a
        // table, like an object's padding, is in; the table's bytes, new or
        // carved out of slack already counted, are out. `committed + delta
        // >= len` because the table lies inside the frontier and overlaps no
        // other table.
        self.stats.committed = self.stats.committed + delta - len;
        // Fresh memory above the old bump pointer: zero, never written.
        proof {
            // The table leaves `fresh` for `handed`, and its zero token
            // `fresh_zero` for `handed_zero`; its alignment padding is
            // dropped.
            let tracked f = take_raw(self.fresh.borrow_mut());
            let tracked (_pad, rest) = split_at(f, self.bump as int, start as int, self.committed as int);
            let tracked (mine, rest) = split_at(rest, start as int, end as int, self.committed as int);
            put_raw(self.fresh.borrow_mut(), rest);
            put_raw(self.handed.borrow_mut(), mine);
            let tracked (mine_z, rest_z) = cut_zero(take_zero(self.fresh_zero.borrow_mut()), start - self.bump, end - start);
            put_zero(self.fresh_zero.borrow_mut(), rest_z);
            put_zero(self.handed_zero.borrow_mut(), mine_z);
        }
        self.bump = end;
        self.stats.bumped = end - self.base;
        self.stats.eager += len;
        proof {
            let ghost m0 = self.pages@@;
            lemma_int_range(start as int, end as int);
            assert forall|a: int| m0.contains_key(a) implies !set_int_range(start as int, end as int).contains(a) by {
                assert(old(self).pages@@.contains_key(a));
            }
            assert(m0.dom().disjoint(set_int_range(start as int, end as int)));
            lemma_side_mint(m0, start as int, end as int, false);
            *claim = self.pages.borrow_mut().insert_map(claim_map(start as int, end as int, false));
            assert(self.pages_wf());
        }
        Some(start)
    }

    /// Take an object of bin `bin`, with the permission to its bytes.
    fn alloc(&mut self, bin: usize) -> (res: (*mut u8, Tracked<PointsToRaw>))
        requires
            old(self).wf(),
            bin < NBINS,
        ensures
            final(self).wf(),
            final(self).live_id() == old(self).live_id(),
            final(self).base == old(self).base,
            final(self).len == old(self).len,
            final(self).top == old(self).top,
            res.0 as usize == 0 ==> *final(self) == *old(self),
            res.0 as usize != 0 ==> res.1@.is_range(res.0 as usize as int, bin_size(bin as int)),
            res.0 as usize != 0 ==> res.1@.provenance() == res.0@.provenance,
            ({ let p = res.0; p as usize != 0 ==> {
                &&& p as usize as int % bin_align_spec(bin as int) == 0
                &&& final(self).base <= p as usize
                &&& p as usize + bin_size(bin as int) <= final(self).bump
                &&& final(self).stats.allocated == old(self).stats.allocated + bin_size(bin as int)
                &&& (p as usize == old(self).free[bin as int] as usize && final(self).bump == old(self).bump) || (
                old(self).bump <= p as usize && final(self).bump == p as usize + bin_size(bin as int))
                &&& old(self).bump <= p as usize ==> final(self).handed_zero@.addr() == p as usize
                    && final(self).handed_zero@.len() == bin_size(bin as int)
            } }),
    {
        proof {
            lemma_bin(bin as int);
            lemma_free_bytes_nonneg(self.lists@, NBINS as nat);
            assert(list_ok(self.free@, self.lists@, self.perms@, self.base, self.bump, bin as int));
        }
        let size = BINS[bin] as usize;
        let head = self.free[bin];
        if !head.is_null() {
            let ghost l = self.lists@[bin as int];
            proof {
                assert(node_ok(self.lists@, self.perms@, self.base, self.bump, bin as int, 0));
                lemma_pop(self.free@, self.lists@, self.perms@, self.base, self.bump, bin as int);
            }
            proof {
                assert(tail_ok(self.tails@, self.lists@[bin as int][0], bin as int));
            }
            let tracked mut perm = self.perms.borrow_mut().tracked_remove(head);
            let tracked tail = self.tails.borrow_mut().tracked_remove(head);
            // SAFETY: `head` came from `free`, so it is a live block of at
            // least `size` bytes inside the bump area holding a `FreeNode`.
            self.free[bin] = ptr_ref(head, Tracked(&perm)).next;
            proof {
                let ghost lists0 = self.lists@;
                lemma_free_bytes_update(lists0, bin as int, l.drop_first(), NBINS as nat);
                assert((l.drop_first().len() - l.len()) * bin_size(bin as int) == -bin_size(bin as int)) by (nonlinear_arith)
                    requires
                        l.drop_first().len() == l.len() - 1,
                ;
                self.lists@ = lists0.update(bin as int, l.drop_first());
                assert(self.region_wf());
                assert(self.lists_wf());
                let ghost m0 = self.live@@;
                assert(!m0.contains_key(head)) by {
                    assert(lists0[bin as int][0] == head);
                }
                lemma_live_insert(m0, head, bin as nat);
                let tracked _claim = self.live.borrow_mut().insert(head, bin as nat);
                assert forall|b: int, i: int|
                    0 <= b < NBINS && 0 <= i < self.lists@[b].len() implies !self.live@@.contains_key(#[trigger] self.lists@[b][i]) by {
                    let j = if b == bin as int { i + 1 } else { i };
                    assert(self.lists@[b][i] == lists0[b][j]);
                    assert(lists0[b][j] != lists0[bin as int][0]);
                }
                assert forall|b: int, i: int|
                    0 <= b < NBINS && 0 <= i < self.lists@[b].len() implies #[trigger] tail_ok(self.tails@, self.lists@[b][i], b) by {
                    let j = if b == bin as int { i + 1 } else { i };
                    assert(self.lists@[b][i] == lists0[b][j]);
                    assert(lists0[b][j] != lists0[bin as int][0]);
                    assert(tail_ok(old(self).tails@, lists0[b][j], b));
                }
            }
            self.stats.allocated += size;
            proof {
                assert(size == bin_size(bin as int));
                assert(free_bytes(self.lists@, NBINS as nat) == free_bytes(old(self).lists@, NBINS as nat) - size);
                assert(self.stats_wf());
                assert(self.live_wf());
            }
            // The block's permission: its link word's and the rest.
            proof {
                perm.leak_contents();
            }
            let tracked blk = join_at(perm.into_raw(), tail, head as usize as int, head as usize + 8, head as usize + bin_size(bin as int));
            return (head as *mut u8, Tracked(blk));
        }
        proof {
            assert(16usize & sub(16usize, 1) == 0 && 128usize & sub(128usize, 1) == 0) by (bit_vector);
        }
        let start = align_up(self.bump, bin_align(bin));
        let Some(end) = start.checked_add(size) else { return (ptr::null_mut(), Tracked(PointsToRaw::empty(cast_provenance()))) };
        if end > self.top {
            return (ptr::null_mut(), Tracked(PointsToRaw::empty(cast_provenance())));
        }
        if end > self.committed {
            match self.commit_bump_to(end) {
                Ok(delta) => self.stats.committed += delta,
                Err(_) => return (ptr::null_mut(), Tracked(PointsToRaw::empty(cast_provenance()))),
            }
        }
        proof {
            lemma_bump(self.free@, self.lists@, self.perms@, self.base, self.bump, end);
        }
        // The object's permission, after its alignment padding, from `fresh`.
        let tracked f = take_raw(self.fresh.borrow_mut());
        let tracked (_pad, rest) = split_at(f, self.bump as int, start as int, self.committed as int);
        let tracked (blk, rest) = split_at(rest, start as int, end as int, self.committed as int);
        proof {
            put_raw(self.fresh.borrow_mut(), rest);
            // The object's zero token, the same cut of `fresh_zero`.
            let tracked (mine_z, rest_z) = cut_zero(take_zero(self.fresh_zero.borrow_mut()), start - self.bump, end - start);
            put_zero(self.fresh_zero.borrow_mut(), rest_z);
            put_zero(self.handed_zero.borrow_mut(), mine_z);
        }
        self.bump = end;
        self.stats.bumped = end - self.base;
        self.stats.allocated += size;
        let p = addr_to_ptr(start);
        proof {
            let k = p as *mut FreeNode;
            let ghost m0 = self.live@@;
            assert(k as usize == start);
            assert forall|b: int, i: int|
                0 <= b < NBINS && 0 <= i < self.lists@[b].len() implies self.lists@[b][i] != k by {
                let o = old(self);
                assert(list_ok(o.free@, o.lists@, o.perms@, o.base, o.bump, b));
                assert(node_ok(o.lists@, o.perms@, o.base, o.bump, b, i));
                lemma_bin(b);
            }
            assert(!m0.contains_key(k)) by {
                if m0.contains_key(k) {
                    lemma_bin(m0[k] as int);
                }
            }
            lemma_live_insert(m0, k, bin as nat);
            let tracked _claim = self.live.borrow_mut().insert(k, bin as nat);
            assert(self.live_wf());
        }
        (p, Tracked(blk))
    }

    /// Put `p`, an object of bin `bin`, on the bin's free list. `raw` is
    /// the permission to its bytes and `claim` the record
    /// [`alloc`](Inner::alloc) made of it, which the caller gives up with
    /// the object.
    fn free(
        &mut self,
        p: *mut u8,
        bin: usize,
        Tracked(raw): Tracked<PointsToRaw>,
        Tracked(claim): Tracked<GhostPointsTo<*mut FreeNode, nat>>,
    )
        requires
            old(self).wf(),
            bin < NBINS,
            raw.is_range(p as usize as int, bin_size(bin as int)),
            raw.provenance() == p@.provenance,
            claim.id() == old(self).live_id(),
            claim.key() == p as *mut FreeNode,
            claim.value() == bin as nat,
        ensures
            final(self).wf(),
            final(self).live_id() == old(self).live_id(),
    {
        let addr = p as usize;
        let node = p as *mut FreeNode;
        let ghost m0 = self.live@@;
        proof {
            claim.agree(self.live.borrow());
            assert(m0.contains_key(node));
            lemma_bin(bin as int);
            lemma_mod_trans(addr as int, bin_align_spec(bin as int), 8);
        }
        // The link word's permission, and the rest for `tails`.
        let tracked (link, tail) = split_at(raw, addr as int, addr + 8, addr + bin_size(bin as int));
        let tracked mut perm = link.into_typed::<FreeNode>(addr);
        proof {
            assert(perm.ptr() == node);
            lemma_live_remove(m0, node);
            perm.is_nonnull();
            if self.perms@.contains_key(node) {
                perm.is_disjoint(self.perms.borrow().tracked_borrow(node));
                assert(false);
            }
            assert(node@.addr != 0);
            assert(list_ok(self.free@, self.lists@, self.perms@, self.base, self.bump, bin as int));
            if self.lists@[bin as int].len() > 0 {
                assert(node_ok(self.lists@, self.perms@, self.base, self.bump, bin as int, 0));
                perm.is_disjoint(self.perms.borrow().tracked_borrow(self.free@[bin as int]));
                assert(self.free@[bin as int]@.addr != node@.addr);
            } else {
                assert(self.free@[bin as int] == ptr::null_mut::<FreeNode>());
                assert(self.free@[bin as int]@.addr != node@.addr);
            }
        }
        let size = BINS[bin] as usize;
        assert!(
            addr >= self.base && addr + size <= self.bump && addr % bin_align(bin) == 0,
            "meta: free of a pointer that was never allocated here"
        );
        // The one double free that costs nothing to see: the block is the
        // list's head. A block deeper in the list is not looked for (that
        // would walk the list on every free).
        assert!(self.free[bin] != node, "meta: double free of the free list head");
        // SAFETY: the block is `size >= 16` bytes of committed memory that
        // the caller has given up, aligned to at least 16.
        ptr_mut_write(node, Tracked(&mut perm), FreeNode { next: self.free[bin] });
        proof {
            lemma_push(self.free@, self.lists@, self.perms@, self.base, self.bump, bin as int, node, perm);
        }
        self.free[bin] = node;
        proof {
            let ghost l = self.lists@[bin as int];
            let ghost lists0 = self.lists@;
            let ghost nl = seq![node].add(l);
            lemma_free_bytes_update(lists0, bin as int, nl, NBINS as nat);
            assert((nl.len() - l.len()) * bin_size(bin as int) == bin_size(bin as int)) by (nonlinear_arith)
                requires
                    nl.len() == l.len() + 1,
            ;
            self.perms.borrow_mut().tracked_insert(node, perm);
            self.tails.borrow_mut().tracked_insert(node, tail);
            self.lists@ = lists0.update(bin as int, nl);
            self.live.borrow_mut().delete_points_to(claim);
            assert forall|b: int, i: int|
                0 <= b < NBINS && 0 <= i < self.lists@[b].len() implies !self.live@@.contains_key(#[trigger] self.lists@[b][i]) by {
                if b == bin as int && i > 0 {
                    assert(self.lists@[b][i] == lists0[b][i - 1]);
                } else if b != bin as int {
                    assert(self.lists@[b][i] == lists0[b][i]);
                }
            }
            assert forall|b: int, i: int|
                0 <= b < NBINS && 0 <= i < self.lists@[b].len() implies #[trigger] tail_ok(self.tails@, self.lists@[b][i], b) by {
                if b == bin as int && i == 0 {
                    assert(self.lists@[b][i] == node);
                } else {
                    let j = if b == bin as int { i - 1 } else { i };
                    assert(self.lists@[b][i] == lists0[b][j]);
                    assert(tail_ok(old(self).tails@, lists0[b][j], b));
                    assert(lists0[b][j] != node);
                }
            }
        }
        self.stats.allocated -= size;
        proof {
            assert(self.live_wf());
        }
    }

    fn reserve(
        &mut self,
        len: usize,
        align: usize,
        Tracked(claim): Tracked<&mut GhostSubmap<int, bool>>,
        Tracked(given): Tracked<&mut Unbacked>,
    ) -> (r: Option<usize>)
        requires
            old(self).wf(),
            is_pow2(align),
        ensures
            final(self).wf(),
            final(self).live_id() == old(self).live_id(),
            final(self).pages_id() == old(self).pages_id(),
            r is None ==> *final(self) == *old(self),
            r matches Some(s) ==> {
                &&& final(claim).id() == final(self).pages_id()
                &&& final(claim)@ == claim_map(s as int, old(self).top as int, true)
                &&& final(given).addr() == s
                &&& final(given).len() == old(self).top - s
                &&& final(self).pages@@ == old(self).pages@@.union_prefer_right(final(claim)@)
                &&& s == final(self).top
                &&& old(self).committed <= s
                &&& s + len <= old(self).top
                &&& s < old(self).top
                &&& s % align == 0
                &&& s % old(self).page == 0
                &&& *final(self) == (Inner {
                    top: s,
                    stats: MetaStats { reserved: final(self).stats.reserved, ..old(self).stats },
                    pages: final(self).pages,
                    unbacked: final(self).unbacked,
                    ..*old(self)
                })
            },
    {
        let ghost align0 = align;
        proof {
            lemma_pow2_bits(align);
        }
        assert!(align.is_power_of_two(), "meta: reserve alignment not a power of two");
        if len > self.top - self.committed {
            return None;
        }
        let align = align.max(self.page);
        let len = align_up(len.max(1), self.page);
        let start = self.top.checked_sub(len)? & !(align - 1);
        proof {
            let t = (self.top - len) as usize;
            let a = align;
            assert(start <= t) by (bit_vector)
                requires
                    start == t & !sub(a, 1),
            ;
            assert(start & sub(a, 1) == 0) by (bit_vector)
                requires
                    start == t & !sub(a, 1),
            ;
            lemma_mod_mask(start, a);
            lemma_pow2_divides(align, self.page);
            lemma_mod_trans(start as int, align as int, self.page as int);
            lemma_pow2_divides(align, align0);
            lemma_mod_trans(start as int, align as int, align0 as int);
        }
        if start < self.committed {
            return None;
        }
        let ghost old_top = self.top;
        self.stats.reserved += self.top - start;
        self.top = start;
        proof {
            let ghost m0 = self.pages@@;
            lemma_int_range(start as int, old_top as int);
            assert forall|a: int| m0.contains_key(a) implies !set_int_range(start as int, old_top as int).contains(a) by {
                assert(old(self).pages@@.contains_key(a));
            }
            assert(m0.dom().disjoint(set_int_range(start as int, old_top as int)));
            lemma_side_mint(m0, start as int, old_top as int, true);
            *claim = self.pages.borrow_mut().insert_map(claim_map(start as int, old_top as int, true));
            assert(self.pages_wf());
            // The reserved bytes leave `unbacked` for the caller.
            let tracked u = take_unbacked(self.unbacked.borrow_mut());
            let tracked (rest, mine) = u.split(start - self.committed);
            put_unbacked(self.unbacked.borrow_mut(), rest);
            put_unbacked(given, mine);
        }
        Some(start)
    }

    /// Commit `[addr, addr + len)`, given `claim`, the claim to those
    /// bytes that [`reserve`](Inner::reserve) or
    /// [`reserve_eager`](Inner::reserve_eager) recorded (on the side
    /// `top_side` names). Taken only on success.
    fn commit(
        &mut self,
        addr: usize,
        len: usize,
        Ghost(top_side): Ghost<bool>,
        Tracked(claim): Tracked<&mut GhostSubmap<int, bool>>,
    ) -> (r: Result<(), Errno>)
        requires
            old(self).wf(),
            addr % old(self).page == 0,
            len % old(self).page == 0,
            len > 0,
            old(claim).id() == old(self).pages_id(),
            old(claim)@.dom() == set_int_range(addr as int, addr + len),
            forall|a: int| #[trigger] old(claim)@.dom().contains(a) ==> old(claim)@[a] == top_side,
        ensures
            final(self).wf(),
            final(self).live_id() == old(self).live_id(),
            final(self).pages_id() == old(self).pages_id(),
            final(self).base == old(self).base,
            final(self).len == old(self).len,
            r is Err ==> *final(self) == *old(self) && *final(claim) == *old(claim),
            // A range on the reservation side was committed by `commit_range`.
            r is Ok && old(self).top <= addr ==> final(self).top_committed == old(self).top_committed + len,
    {
        let ghost m = self.pages@@;
        proof {
            claim.agree(self.pages.borrow());
            lemma_int_range(addr as int, addr + len);
            assert(claim@.dom().contains(addr as int));
            assert(claim@.dom().contains(addr + len - 1));
            lemma_claim_at(claim@, m, addr as int);
            lemma_claim_at(claim@, m, addr + len - 1);
            lemma_side_consume(m, claim@, top_side);
            vstd::set_lib::lemma_len_subset(claim@.dom(), side_set(m, top_side));
        }
        assert!(
            addr % self.page == 0 && len % self.page == 0 && len > 0,
            "meta: commit range not page aligned"
        );
        assert!(addr.checked_add(len).is_some(), "meta: commit range wraps");
        let end = addr + len;
        if addr < self.top {
            // A table carved from the object side (`reserve_eager`): inside
            // the chunked commit and writable already, so this is the
            // owner's accounting of the pages it uses.
            assert!(
                addr >= self.base && end <= self.committed,
                "meta: commit outside the reserved area"
            );
            self.stats.committed += len;
            proof {
                let tracked c = claim.take();
                self.pages.borrow_mut().delete(c);
                assert(self.pages_wf());
            }
            return Ok(());
        }
        assert!(end <= self.base + self.len, "meta: commit outside the reserved area");
        commit_range(addr, len)?;
        // Metadata THP (`thp.rs`): `Lazy::ensure` rounded the range to the
        // folio; the advice makes the kernel map it as one.
        if crate::thp::meta_folio_size() != 0 {
            crate::thp::advise_meta(addr, len);
        }
        self.top_committed += len;
        self.stats.committed += len;
        proof {
            let tracked c = claim.take();
            self.pages.borrow_mut().delete(c);
            assert(self.pages_wf());
        }
        Ok(())
    }

    /// [`Meta::reserve_committed`] under the lock.
    fn reserve_committed(&mut self, len: usize, align: usize) -> (r: Option<usize>)
        requires
            old(self).wf(),
            is_pow2(align),
        ensures
            final(self).wf(),
            final(self).live_id() == old(self).live_id(),
            final(self).pages_id() == old(self).pages_id(),
            final(self).base == old(self).base,
            final(self).len == old(self).len,
            r matches Some(s) ==> s % align == 0 && s + len <= va_limit(),
            r matches Some(s) ==> old(self).base <= s && s + len <= old(self).base + old(self).len,
            r matches Some(s) ==> final(self).handed@.contains_range(s as int, len as int),
            r matches Some(s) ==> final(self).handed@.provenance() == cast_provenance(),
            r matches Some(s) ==> final(self).handed_zero@.addr() == s && final(self).handed_zero@.len() >= len,
    {
        let tracked mut claim = GhostSubmap::<int, bool>::dummy();
        if self.eager_ok(len) {
            if let Some(p) = self.reserve_eager(len, align, Tracked(&mut claim)) {
                let ghost m = self.pages@@;
                let ghost tlen = align_up_spec(if len == 0 { 1 } else { len }, self.page);
                proof {
                    claim.agree(self.pages.borrow());
                    lemma_int_range(p as int, p + tlen);
                    lemma_side_consume(m, claim@, false);
                    vstd::set_lib::lemma_len_subset(claim@.dom(), side_set(m, false));
                }
                self.stats.committed += align_up(len.max(1), self.page);
                proof {
                    let tracked c = claim.take();
                    self.pages.borrow_mut().delete(c);
                    assert(self.pages_wf());
                }
                return Some(p);
            }
        }
        let ghost pre = *self;
        let old_top = self.top;
        let old_reserved = self.stats.reserved;
        let tracked mut given = Unbacked::empty(0);
        let start = self.reserve(len, align, Tracked(&mut claim), Tracked(&mut given))?;
        proof {
            lemma_sub_mod_noop(old_top as int, start as int, self.page as int);
            vstd::arithmetic::div_mod::lemma_small_mod(0, self.page as nat);
        }
        if self.commit(start, old_top - start, Ghost(true), Tracked(&mut claim)).is_err() {
            self.top = old_top;
            self.stats.reserved = old_reserved;
            proof {
                let tracked u = take_unbacked(self.unbacked.borrow_mut());
                put_unbacked(self.unbacked.borrow_mut(), u.join(given));
                let ghost m0 = pre.pages@@;
                lemma_int_range(start as int, old_top as int);
                let tracked c = claim.take();
                self.pages.borrow_mut().delete(c);
                assert forall|a: int| m0.contains_key(a) implies !set_int_range(start as int, old_top as int).contains(a) by {
                    assert(pre.pages@@.contains_key(a));
                }
                assert(self.pages@@ =~= m0);
                assert(self.pages_wf());
            }
            return None;
        }
        // `commit` made exactly `[start, old_top)` writable: `start` is the
        // new `top`, so it took the `commit_range` branch.
        proof {
            assert(self.top_committed == pre.top_committed + (old_top - start));
            let tracked (raw, z) = given.commit();
            put_raw(self.handed.borrow_mut(), raw);
            put_zero(self.handed_zero.borrow_mut(), z);
        }
        Some(start)
    }

    fn stats(&self) -> (r: MetaStats)
        requires
            self.wf(),
        ensures
            r.identities(),
    {
        proof {
            lemma_free_bytes_nonneg(self.lists@, NBINS as nat);
        }
        MetaStats { writable: (self.committed - self.base) + self.top_committed, ..self.stats }
    }
}

proof fn lemma_free_bytes_zero(lists: Seq<Seq<*mut FreeNode>>, n: nat)
    requires
        n <= lists.len(),
        forall|b: int| 0 <= b < n ==> #[trigger] lists[b].len() == 0,
    ensures
        free_bytes(lists, n) == 0,
    decreases n,
{
    if n > 0 {
        lemma_free_bytes_zero(lists, (n - 1) as nat);
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
        b'/', b'p', b'r', b'o', b'c', b'/', b's', b'y', b's', b'/', b'v', b'm', b'/', b'o', b'v', b'e', b'r', b'c', b'o', b'm', b'm', b'i', b't', b'_', b'm', b'e', b'm', b'o', b'r', b'y', 0,
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
    /// An allocator over `[base, base + len)`, both page multiples, given
    /// `space`, the region's uncommitted address space, which it commits
    /// as it hands it out.
    pub const fn owning(base: usize, len: usize, page: usize, Tracked(space): Tracked<Unbacked>) -> (r: Meta)
        requires
            is_pow2(page),
            page >= MIN_ALIGN,
            base > 0,
            base % page == 0,
            len % page == 0,
            len > 0,
            base + len <= va_limit(),
            space.addr() == base,
            space.len() == len,
        ensures
            r.spec_base() == base,
            r.spec_len() == len,
    {
        broadcast use axiom_meta_lock_inv;

        Meta { inner: Lock::new(Inner::new(base, len, page, Tracked(space))) }
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
    #[inline(always)]
    pub fn alloc(&self, layout: Layout) -> (p: *mut u8)
        requires
            layout_align(layout) <= MAX_ALIGN,
            layout_size(layout) <= MAX_BINNED,
        ensures
            p as usize != 0 ==> (p as usize) % layout_align(layout) == 0,
            p as usize != 0 ==> self.holds(p as int, layout_size(layout) as int),
    {
        self.alloc_raw(layout).0
    }

    /// [`alloc`](Meta::alloc), with the permission to the object's
    /// `layout.size()` bytes.
    pub fn alloc_raw(&self, layout: Layout) -> (r: (*mut u8, Tracked<PointsToRaw>))
        requires
            layout_align(layout) <= MAX_ALIGN,
            layout_size(layout) <= MAX_BINNED,
        ensures
            r.0 as usize != 0 ==> (r.0 as usize) % layout_align(layout) == 0,
            r.0 as usize != 0 ==> self.holds(r.0 as int, layout_size(layout) as int),
            r.0 as usize != 0 ==> r.1@.is_range(r.0 as usize as int, layout_size(layout) as int),
            r.0 as usize != 0 ==> r.1@.provenance() == r.0@.provenance,
    {
        broadcast use {axiom_meta_lock_inv, axiom_meta_lock_frame};

        assert!(layout.align() <= MAX_ALIGN, "meta: alignment too large for an object");
        let Some(bin) = bin_for(layout) else { panic!("meta: object too large") };
        let (p, Tracked(raw)) = self.inner.lock().alloc(bin);
        proof {
            lemma_align_in_bin(p as usize, layout, bin);
        }
        // The bin's slack past `layout.size()` is dropped.
        let tracked obj = keep_prefix(raw, p as usize as int, p as usize + layout_size(layout), p as usize + bin_size(bin as int));
        (p, Tracked(obj))
    }

    /// [`alloc`](Meta::alloc), then zero the object if it was reused; with
    /// the permission to the object's `layout.size()` bytes and the token
    /// that they read as zero: a reused object is cleared here, and a fresh
    /// one lies above the old bump pointer, whose bytes carry the token
    /// `Unbacked::commit` returned when they were committed
    /// (`Inner::fresh_zero`).
    pub fn alloc_zeroed(&self, layout: Layout) -> (r: (*mut u8, Tracked<PointsToRaw>, Tracked<Zeroed>))
        requires
            layout_align(layout) <= MAX_ALIGN,
            layout_size(layout) <= MAX_BINNED,
        ensures
            r.0 as usize != 0 ==> (r.0 as usize) % layout_align(layout) == 0,
            r.0 as usize != 0 ==> self.holds(r.0 as int, layout_size(layout) as int),
            r.0 as usize != 0 ==> r.1@.is_range(r.0 as usize as int, layout_size(layout) as int),
            r.0 as usize != 0 ==> r.1@.provenance() == r.0@.provenance,
            r.0 as usize != 0 ==> r.2@.addr() == r.0@.addr && r.2@.len() == layout_size(layout),
            r.0 as usize != 0 ==> r.0 as usize + layout_size(layout) <= va_limit(),
    {
        broadcast use {axiom_meta_lock_inv, axiom_meta_lock_frame};

        assert!(layout.align() <= MAX_ALIGN, "meta: alignment too large for an object");
        let Some(bin) = bin_for(layout) else { panic!("meta: object too large") };
        let mut g = self.inner.lock();
        let before = g.bump;
        let (p, Tracked(raw)) = g.alloc(bin);
        let tracked mut z = Zeroed::empty(0);
        if !p.is_null() && (p as usize) < before {
            // Reused: below the old bump pointer. Fresh memory is above it
            // and has never been written.
            proof {
                broadcast use vstd::layout::align_of_u8;
                lemma_bin(bin as int);
                crate::vspec::ptr::axiom_raw_valid(&raw, p, bin_size(bin as int));
            }
            // SAFETY: `p` is a live block of `BINS[bin]` bytes we just took.
            let Tracked(cleared) = unsafe { write_zero(p, BINS[bin] as usize) };
            proof {
                z = cleared;
            }
        }
        proof {
            lemma_align_in_bin(p as usize, layout, bin);
            assert(g@.wf());
        }
        let _gi: &mut Inner = &mut g;
        let tracked fz = take_zero(_gi.handed_zero.borrow_mut());
        proof {
            if p@.addr != 0 && before <= p@.addr {
                // Fresh: above the old bump pointer, carved from `fresh`
                // with its cut of `fresh_zero` (`Inner::alloc`).
                z = fz;
            }
        }
        // The bin's slack past `layout.size()` is dropped.
        let tracked obj = keep_prefix(raw, p as usize as int, p as usize + layout_size(layout), p as usize + bin_size(bin as int));
        let tracked z = zero_prefix(z, layout_size(layout) as int);
        (p, Tracked(obj), Tracked(z))
    }

    /// Carve `len` bytes (rounded up to pages) aligned to `align` (at least a
    /// page) from the top of the region. Uncommitted: the owner calls
    /// [`commit`](Meta::commit) on the pages it uses. Never freed. `None`
    /// when it would collide with the object area. For a table that is
    /// used from its base up, [`reserve_table`](Meta::reserve_table); this
    /// is for a reservation that needs a `PROT_NONE` part (the background
    /// thread's stack guard).
    pub fn reserve(&self, len: usize, align: usize) -> (r: Option<usize>)
        requires
            is_pow2(align),
        ensures
            r matches Some(s) ==> s % align == 0 && s + len <= va_limit(),
            r matches Some(s) ==> self.holds(s as int, len as int),
    {
        broadcast use {axiom_meta_lock_inv, axiom_meta_lock_frame};

        let tracked mut claim = GhostSubmap::<int, bool>::dummy();
        let tracked mut given = Unbacked::empty(0);
        self.inner.lock().reserve(len, align, Tracked(&mut claim), Tracked(&mut given))
    }

    /// [`reserve_table`](Meta::reserve_table) and commit the whole range.
    /// Zero on return. `None` on either failure, leaving nothing reserved.
    #[inline(always)]
    pub fn reserve_committed(&self, len: usize, align: usize) -> (r: Option<usize>)
        requires
            is_pow2(align),
        ensures
            r matches Some(s) ==> s % align == 0 && s + len <= va_limit(),
            r matches Some(s) ==> self.holds(s as int, len as int) && s > 0,
    {
        self.reserve_committed_raw(len, align).0
    }

    /// [`reserve_committed`](Meta::reserve_committed), with the permission
    /// to the range's bytes and the token that they read as zero. Two
    /// ranges this hands out are apart (`vspec::ptr::lemma_raw_disjoint` of
    /// their permissions). The permission comes back through
    /// `Inner::handed`, not a tracked parameter of `Inner::reserve_committed`
    /// or `reserve_eager`: such a parameter changes how `Huge::alloc_run` is
    /// inlined into its callers.
    pub fn reserve_committed_raw(&self, len: usize, align: usize) -> (r: (Option<usize>, Tracked<PointsToRaw>, Tracked<Zeroed>))
        requires
            is_pow2(align),
        ensures
            r.0 matches Some(s) ==> s % align == 0 && s + len <= va_limit(),
            r.0 matches Some(s) ==> self.holds(s as int, len as int) && s > 0,
            r.0 matches Some(s) ==> r.1@.contains_range(s as int, len as int),
            r.0 matches Some(s) ==> r.1@.provenance() == cast_provenance(),
            r.0 matches Some(s) ==> r.2@.addr() == s && r.2@.len() == len,
    {
        broadcast use {axiom_meta_lock_inv, axiom_meta_lock_frame};

        let mut g = self.inner.lock();
        let r = g.reserve_committed(len, align);
        let _gi: &mut Inner = &mut g;
        let tracked raw = take_raw(_gi.handed.borrow_mut());
        let tracked hz = take_zero(_gi.handed_zero.borrow_mut());
        // On failure `handed` and `handed_zero` hold whatever an earlier
        // call left; none of it goes out.
        let tracked raw = raw_if(raw, r is Some);
        let tracked mut z = Zeroed::empty(0);
        proof {
            if let Some(s) = r {
                // The range was carved from `fresh` with its cut of
                // `fresh_zero`, or committed from `unbacked`
                // (`Inner::reserve_committed`).
                z = zero_prefix(hz, len as int);
            }
        }
        (r, Tracked(raw), Tracked(z))
    }

    pub fn stats(&self) -> (r: MetaStats)
        ensures
            r.identities(),
    {
        broadcast use axiom_meta_lock_inv;

        self.inner.lock().stats()
    }

    /// A never-freed table of `len` bytes aligned to `align` whose owner
    /// commits pages from its base as it uses them. Up to [`EAGER_MAX`]
    /// bytes, and where the host allows (the module doc), it is carved
    /// from the object side and is writable from the start; otherwise, or
    /// when the object side cannot hold it, it is a
    /// [`reserve`](Meta::reserve). Either way the owner calls
    /// [`commit`](Meta::commit) on the pages it uses before touching them.
    /// The table's range rounded up to whole words ([`words_up`]) is the
    /// allocator's too: each table is whole pages.
    pub fn reserve_table(&self, len: usize, align: usize) -> (r: Option<usize>)
        requires
            is_pow2(align),
        ensures
            r matches Some(s) ==> s % align == 0 && s + len <= va_limit(),
            r matches Some(s) ==> self.holds(s as int, len as int),
            r matches Some(s) ==> self.holds(s as int, words_up(len as int)),
    {
        broadcast use {axiom_meta_lock_inv, axiom_meta_lock_frame};

        let tracked mut claim = GhostSubmap::<int, bool>::dummy();
        let mut g = self.inner.lock();
        if g.eager_ok(len) {
            let ghost g1 = g@;
            if let Some(p) = g.reserve_eager(len, align, Tracked(&mut claim)) {
                proof {
                    let (page, l) = (g1.page, if len == 0 { 1usize } else { len });
                    lemma_align_bits(l, page, (l + (page - 1)) as usize);
                    lemma_words_up_fit(page, align_up_spec(l, page) as int, len as int);
                }
                return Some(p);
            }
        }
        let ghost g2 = g@;
        let tracked mut given = Unbacked::empty(0);
        let r = g.reserve(len, align, Tracked(&mut claim), Tracked(&mut given));
        proof {
            if let Some(s) = r {
                assert(g2.wf());
                assert(g2.top as int % g2.page as int == 0);
                assert(s as int % g2.page as int == 0);
                vstd::arithmetic::div_mod::lemma_small_mod(0, g2.page as nat);
                lemma_sub_mod_noop(g2.top as int, s as int, g2.page as int);
                lemma_words_up_fit(g2.page, g2.top - s, len as int);
            }
        }
        r
    }

    /// The ghost identity of the live-object records ([`Inner::live_id`];
    /// the same in every value the lock holds, `vspec/meta.rs`).
    pub closed spec fn live_id(&self) -> Loc {
        lock_init(self.inner).live_id()
    }

    /// The ghost identity of the unclaimed reserved bytes
    /// ([`Inner::pages_id`]; the same in every value the lock holds).
    pub closed spec fn pages_id(&self) -> Loc {
        lock_init(self.inner).pages_id()
    }

    /// The page size (the same in every value the lock holds).
    pub closed spec fn page(&self) -> usize {
        lock_init(self.inner).spec_page()
    }

    /// The region's first byte (the same in every value the lock holds).
    pub closed spec fn spec_base(&self) -> usize {
        lock_init(self.inner).spec_base()
    }

    /// The region's length (the same in every value the lock holds).
    pub closed spec fn spec_len(&self) -> usize {
        lock_init(self.inner).spec_len()
    }

    /// `[a, a + len)` lies in the region: where every object and table
    /// [`alloc`](Meta::alloc), [`alloc_zeroed`](Meta::alloc_zeroed),
    /// [`reserve`](Meta::reserve),
    /// [`reserve_committed`](Meta::reserve_committed) and
    /// [`reserve_table`](Meta::reserve_table) hand out lies, and the only
    /// memory the allocator itself writes (the link words of free objects,
    /// [`Inner::lemma_links_in_region`], and the zeroing of a reused
    /// object in `alloc_zeroed`).
    pub open spec fn holds(&self, a: int, len: int) -> bool {
        &&& self.spec_base() <= a
        &&& a + len <= self.spec_base() + self.spec_len()
    }

    /// `v` is a value the metadata lock may hold: its invariant
    /// ([`Inner::wf`], `vspec::meta::axiom_meta_lock_inv`) over this
    /// allocator's region (`axiom_meta_lock_frame`). Every guard of the lock
    /// reads and writes one.
    pub(crate) closed spec fn admits(&self, v: Inner) -> bool {
        &&& crate::lock::lock_inv(v)
        &&& crate::lock::lock_frame(lock_init(self.inner), v)
    }

    /// `a` is the link word of a free object in some value the lock admits
    /// ([`Meta::admits`], [`Inner::is_link`]): the one word `Meta` writes
    /// outside the objects it hands out.
    pub closed spec fn is_link(&self, a: int) -> bool {
        exists|v: Inner| self.admits(v) && #[trigger] v.is_link(a)
    }

    /// A free object's link word lies in what this allocator
    /// [`holds`](Meta::holds) (whole-allocator theorem 4).
    pub proof fn lemma_link_held(&self, a: int)
        requires
            self.is_link(a),
        ensures
            self.holds(a, 8),
    {
        broadcast use {axiom_meta_lock_inv, axiom_meta_lock_frame};

        let v = choose|v: Inner| self.admits(v) && #[trigger] v.is_link(a);
        v.lemma_link_in_region(a);
    }

    /// This allocator's region is the process's metadata region
    /// (`Shared::wf` says so of the `Shared`'s `meta`), so what it
    /// [`holds`](Meta::holds) is metadata
    /// ([`Regions::in_meta`](crate::layout::Regions::in_meta)).
    pub open spec fn is_process(&self) -> bool {
        &&& self.spec_base() == crate::layout::process_regions().meta_base
        &&& self.spec_len() == crate::layout::process_regions().meta_len
    }

    /// Make `[addr, addr + len)`, page aligned and inside a reservation or
    /// an eagerly carved table, readable and writable (accounting only for
    /// the latter): [`commit_claimed`] with the caller's claim to the
    /// range. Trusted: the claim is the caller contract, which no caller
    /// can show, since callers keep their tables' commit watermarks
    /// without ghost state: the range is page aligned, lies inside one
    /// reservation or eager table this allocator handed out, and no page
    /// of it has been committed before. The callers trusted at each call:
    /// the tables' `Lazy::ensure` (from their commit watermark up) and
    /// the background thread's stack (once, after its reservation).
    ///
    /// [`commit_claimed`]: Meta::commit_claimed
    #[verifier::external_body]
    pub fn commit(&self, addr: usize, len: usize) -> Result<(), Errno> {
        self.commit_claimed(addr, len, Ghost::assume_new(), Tracked::assume_new())
    }

    /// [`commit`](Meta::commit), given `claim`, the claim to the range's
    /// bytes that [`reserve`](Meta::reserve) or
    /// [`reserve_table`](Meta::reserve_table) recorded on the side
    /// `top_side` names (`true` for a reservation, `false` for an eager
    /// table).
    fn commit_claimed(
        &self,
        addr: usize,
        len: usize,
        Ghost(top_side): Ghost<bool>,
        Tracked(claim): Tracked<GhostSubmap<int, bool>>,
    ) -> Result<(), Errno>
        requires
            addr % self.page() == 0,
            len % self.page() == 0,
            len > 0,
            claim.id() == self.pages_id(),
            claim@.dom() == set_int_range(addr as int, addr + len),
            forall|a: int| #[trigger] claim@.dom().contains(a) ==> claim@[a] == top_side,
    {
        broadcast use {axiom_meta_lock_inv, axiom_meta_lock_frame};

        let tracked mut claim = claim;
        self.inner.lock().commit(addr, len, Ghost(top_side), Tracked(&mut claim))
    }

    /// Return an object allocated with the same `layout`: [`free_claimed`]
    /// with the caller's claims to it. Trusted: the claims are the caller
    /// contract, which no caller can show, since callers keep their
    /// objects behind raw pointers: `p` came from [`alloc`](Meta::alloc),
    /// [`alloc_raw`](Meta::alloc_raw) or [`alloc_zeroed`](Meta::alloc_zeroed)
    /// of this allocator with a layout of the same bin, is no longer in use
    /// and has not been freed since, and no permission to its bytes is
    /// held elsewhere (the claims include the permission to the whole bin).
    /// The allocator's invariant follows from the claims; the
    /// run-time checks of the bounds, the alignment and a double free of
    /// the list head stay, for callers that break the contract. The
    /// callers trusted at each call: heap destruction and
    /// `free_caches` (heaps, node blocks and cache stacks they own) and
    /// the huge allocator (`free_desc`, `Table::release`).
    ///
    /// [`free_claimed`]: Meta::free_claimed
    #[verifier::external_body]
    pub fn free(&self, p: *mut u8, layout: Layout) {
        self.free_claimed(p, layout, Tracked::assume_new(), Tracked::assume_new())
    }

    /// [`free`](Meta::free), given `raw`, the permission to the object's
    /// bytes (its whole bin), and `claim`, the record [`alloc`](Meta::alloc)
    /// made of it.
    fn free_claimed(
        &self,
        p: *mut u8,
        layout: Layout,
        Tracked(raw): Tracked<PointsToRaw>,
        Tracked(claim): Tracked<GhostPointsTo<*mut FreeNode, nat>>,
    )
        requires
            p@.addr != 0,
            layout_size(layout) <= MAX_BINNED,
            raw.is_range(p as usize as int, bin_size(bin_for_spec(layout) as int)),
            raw.provenance() == p@.provenance,
            claim.id() == self.live_id(),
            claim.key() == p as *mut FreeNode,
            claim.value() == bin_for_spec(layout),
    {
        broadcast use {axiom_meta_lock_inv, axiom_meta_lock_frame};

        assert!(!p.is_null(), "meta: free of null");
        let Some(bin) = bin_for(layout) else { panic!("meta: free with an impossible layout") };
        self.inner.lock().free(p, bin, Tracked(raw), Tracked(claim));
    }
}

/// An object of `layout` placed at `p`, aligned for bin `bin` (the one
/// `bin_for` chose), is aligned for the layout.
proof fn lemma_align_in_bin(p: usize, layout: Layout, bin: usize)
    requires
        layout_ok(layout),
        layout_align(layout) <= MAX_ALIGN,
        layout_align(layout) > MIN_ALIGN ==> bin >= 4,
        p != 0 ==> p as int % bin_align_spec(bin as int) == 0,
    ensures
        p != 0 ==> p % layout_align(layout) == 0,
{
    let a = layout_align(layout);
    lemma_pow2_bits(a);
    if p != 0 {
        if bin < 4 {
            assert(p % a == 0) by (bit_vector)
                requires
                    p % 16 == 0,
                    a & sub(a, 1) == 0,
                    a > 0,
                    a <= 16,
            ;
        } else {
            assert(p % a == 0) by (bit_vector)
                requires
                    p % 128 == 0,
                    a & sub(a, 1) == 0,
                    a > 0,
                    a <= 128,
            ;
        }
    }
}

} // verus!

// SAFETY: the raw pointers are addresses inside the region, meaningful in
// any thread; the `Lock` serialises every access.
unsafe impl Send for Inner {}

impl Meta {
    /// [`Meta::owning`] for the tests, which hold no `Unbacked` token for
    /// their region. It assumes the token, so the region must be a
    /// `PROT_NONE` mapping the test owns, uncommitted, and given to one
    /// `Meta` only.
    #[cfg(test)]
    pub fn new(base: usize, len: usize, page: usize) -> Meta {
        Meta::owning(base, len, page, Tracked::assume_new())
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
        let z = m.alloc_zeroed(layout(1000, 8)).0;
        assert_eq!(z, d);
        assert!(
            unsafe { core::slice::from_raw_parts(z, 1024) }
                .iter()
                .all(|&x| x == 0)
        );
        let fresh = m.alloc_zeroed(layout(5000, 8)).0;
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
