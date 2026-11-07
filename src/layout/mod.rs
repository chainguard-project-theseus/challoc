//! Size classes, address-space regions, and pointer classification
//! (design 2 and 3).
//!
//! Everything in this module except [`Regions::reserve_owning`] is pure
//! arithmetic over a class table fixed at init ([`CLASSES`]: the compile-time
//! [`BASE_CLASSES`] geometry, with the slabs of the classes below the
//! kernel page raised to the page by [`adopt_page_geometry`]) and a
//! handful of runtime bases, and is unit tested exhaustively. `reserve_owning`
//! is the piece that probes how much address space the process can get and
//! lays the regions out.
//!
//! Submodules: [`regions`] holds the address-space parameters, [`Regions`]
//! and the reservation; `tests` the pure tests and `linux_tests` the
//! ones that need a kernel. The size-class math and its two statics stay
//! here.
//!
//! Layout recap. One superblock holds every class region at a fixed
//! power-of-two stride; class `c` lives at `superblock + c * stride +
//! random_offset_c + colour_c`, with the rest of the stride left as an
//! unmapped gap. The random offset is a multiple of [`MAX_SLAB`] and the
//! colour (`regions::colour`) a multiple of the class's slab, so every slab
//! is aligned to its size. Separate reservations hold the huge region and
//! the metadata region.

use crate::QUANTUM;
use core::cell::UnsafeCell;
use core::ops::Deref;
use vstd::prelude::*;

verus! {

/// Largest small-tier request.
pub const SMALL_MAX: usize = 1024;
/// Largest request served from a slab; anything above is huge.
pub const MEDIUM_MAX: usize = 256 * 1024;
/// Smallest slab (4 KiB). The classes from [`SMALL_SLAB_BELOW`] to
/// [`SMALL_MAX`] use the smallest power of two from here up whose internal
/// waste (`slab - nslots * size`) is at most a [`SLAB_WASTE_DIV`]th of the
/// slab: 4 KiB for 512 and 1024 bytes (8 and 4 slots), 8 KiB for 896
/// (9 slots), 16 KiB for 640 and 768 (25 and 21 slots; 4 KiB and 8 KiB
/// would waste a sixteenth of the slab, 21 MiB at the Valkey soak's peak
/// for the 640 class alone, and the peak RSS criterion is 5%). Every other
/// class keeps a 64 KiB slab or larger ([`LG_SMALL_SLAB`],
/// [`LG_MEDIUM_MIN_SLAB`]).
///
/// Why these classes and this size (measured on the Valkey soak): the
/// post-delete residue of the
/// Valkey soak is live values the load never touches again, one to fifteen
/// per slab, each pinning its whole slab, and it sits entirely in the
/// 512..1024-byte classes; at 64 KiB a pinned 1 KiB value kept 64 KiB
/// resident where jemalloc's 4-region slab keeps 4 KiB. The classes below
/// 512 bytes keep 64 KiB slabs: a 16 KiB slab for every class halved
/// `xmalloc-test` (64-byte objects, cross-thread frees) through per-slab
/// refill and flush work, and a class with 64 or more regions per slab never empties by chance
/// after a random deletion either way (0.89^64 = 0.06%). A slab smaller
/// than the kernel page never exists on a supported kernel: at init the
/// classes whose table slab is below the page take the page as their slab
/// size, up to [`LG_SMALL_SLAB`] ([`adopt_page_geometry`]: 64 KiB slabs
/// of 128, 102, 85, 73 and 64 slots for the five classes on a 64 KiB
/// kernel, 16 KiB slabs on a 16 KiB one), so that every
/// `Empty` slab is one purge unit; with sub-page slabs the Valkey soak's
/// post-delete RSS at 64 KiB pages was 79.5 MiB of `Empty` 4..16 KiB slabs
/// in mixed page groups that never became idle. A slab smaller
/// than the page (a faked page in the tests, or a page above 64 KiB) is
/// committed, guarded and purged with its whole page of slabs by `class/`
/// (see `SlabTable::slabs_per_page`).
pub const LG_MIN_SLAB: u32 = 12;
pub const MIN_SLAB: usize = 1 << LG_MIN_SLAB;
/// Classes below this size use `1 << LG_SMALL_SLAB` slabs (see above).
pub const SMALL_SLAB_BELOW: usize = 512;
pub const LG_SMALL_SLAB: u32 = 16;
/// Slab floor of the medium tier: `max(64 KiB, 16 * size)`.
pub const LG_MEDIUM_MIN_SLAB: u32 = 16;
/// Largest internal waste of a class between [`SMALL_SLAB_BELOW`] and
/// [`SMALL_MAX`], as a fraction of its slab (see [`LG_MIN_SLAB`]).
pub const SLAB_WASTE_DIV: usize = 32;
/// The compaction thresholds of `heap/` are counted in slabs of this
/// size (64 KiB), so they mean the same bytes whatever a class's slab size
/// (see [`Class::compact_slots`]).
pub const LG_COMPACT_UNIT: u32 = 16;
/// Guard slabs are spaced per this many bytes of slabs (64 KiB) times
/// `guard_slab_interval`, whatever the class's slab size (see
/// `heap::guard_interval_for`).
pub const LG_GUARD_UNIT: u32 = 16;
/// Largest slab: 16 slots of `MEDIUM_MAX`. The random part of a class
/// region's offset is a multiple of this; the colour added to it keeps
/// the region aligned to its own slab, so in-slab offsets are a plain mask.
pub const LG_MAX_SLAB: u32 = 22;
pub const MAX_SLAB: usize = 1 << LG_MAX_SLAB;

/// Number of classes spaced one quantum apart before the four-per-doubling
/// pattern starts: `Q, 2Q, .., 8Q`.
pub const LINEAR_CLASSES: usize = 8;
/// `log2(8 * QUANTUM)`: the size where the doubling pattern begins.
#[cfg(feature = "quantum16")]
pub const LG_LINEAR_MAX: u32 = 7;
#[cfg(feature = "quantum8")]
pub const LG_LINEAR_MAX: u32 = 6;
/// `log2(MEDIUM_MAX)`.
pub const LG_MEDIUM_MAX: u32 = 18;
/// Total number of size classes: 52 for `quantum16`, 56 for `quantum8`.
pub const NCLASSES: usize = LINEAR_CLASSES + 4 * (LG_MEDIUM_MAX - LG_LINEAR_MAX) as usize;

} // verus!

// The literals above, spelled out so the verifier sees their values,
// checked against their definitions.
const _: () = assert!(LG_LINEAR_MAX == (LINEAR_CLASSES * QUANTUM).trailing_zeros());
const _: () = assert!(LG_MEDIUM_MAX == MEDIUM_MAX.trailing_zeros());

verus! {

/// Largest class reported as a jemalloc *bin* (`arenas.nbins`,
/// `arenas.bin.<j>.*`, `stats.arenas.<i>.bins.<j>.*`, `small.*`): the
/// classes up to 4 KiB, i.e. those whose slab is at most 64 KiB. The
/// boundary is a fixed size so the reported bin set does not move with the
/// slab geometry.
/// Classes above it are slab-allocated exactly like the others, but the
/// `mallctl` namespace reports them the way jemalloc reports large extents
/// (in `large.*`, and `(nfree 0, nregs 1, size)` from the utilization
/// queries). Consumers such as Valkey's defragmenter and its
/// `frag_smallbins` metric are calibrated to jemalloc's bins, whose slabs
/// are a few pages: with challoc's `max(MIN_SLAB, 16 * size)` slabs, four
/// live 10 KiB blocks pin a 256 KiB slab and would count as 215 KiB of
/// "fragmentation" that no defragmenter can recover, and one such class
/// per size step adds up to megabytes on an idle server.
pub const BIN_MAX: usize = 4096;

/// Number of classes up to [`BIN_MAX`]: 28 for `quantum16`, 32 for `quantum8`.
pub exec const NBINS: usize
    ensures
        NBINS <= NCLASSES,
        // The classes below it are exactly those up to `BIN_MAX`.
        forall|c: int| 0 <= c < NCLASSES ==> (c < NBINS <==> #[trigger] spec_class_size(c) <= BIN_MAX),
{
    count_bins()
}

const fn count_bins() -> (n: usize)
    ensures
        n <= NCLASSES,
        forall|c: int| 0 <= c < NCLASSES ==> (c < n <==> #[trigger] spec_class_size(c) <= BIN_MAX),
{
    let mut n = 0;
    while n < NCLASSES && class_size_at(n) <= BIN_MAX
        invariant
            n <= NCLASSES,
            forall|c: int| 0 <= c < n ==> #[trigger] spec_class_size(c) <= BIN_MAX,
        decreases NCLASSES - n,
    {
        n += 1;
    }
    proof {
        // The sizes grow with the class, so none past the first one above
        // `BIN_MAX` is at or below it.
        assert forall|c: int| 0 <= c < NCLASSES && spec_class_size(c) <= BIN_MAX implies c < n by {
            if c >= n {
                if c > n {
                    lemma_class_size_mono(n as int, c);
                }
            }
        }
    }
    n
}

} // verus!

const _: () = assert!(NBINS > 0 && NBINS < NCLASSES && class_size_at(NBINS - 1) == BIN_MAX);
const _: () = assert!(BIN_MAX >= SMALL_MAX && lg_slab_for(BIN_MAX) >= LG_MIN_SLAB);

const _: () = assert!(
    NCLASSES <= 64,
    "pointer classification assumes at most 64 classes"
);
const _: () = assert!(MEDIUM_MAX * 16 == MAX_SLAB);
const _: () = assert!(
    LG_MAX_SLAB < 32,
    "slot_of_offset works on 32-bit in-slab offsets"
);

/// Allocation tier of a class (design 3 table); the tests check
/// the geometry per tier, nothing else distinguishes them.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Small,
    Medium,
}

verus! {

/// One size class. `size`, `inv_odd` and `tz` are compile-time constants;
/// the slab geometry (`lg_slab`, `slab_mask`, `nslots`, `slab_bits`) is the
/// compile-time [`BASE_CLASSES`] value unless [`adopt_page_geometry`]
/// raised the slab to the kernel page at init.
///
/// Aligned to 32 bytes (20 bytes of fields): an entry never straddles a
/// cache line, and the `free` path's table address is one shifted add
/// (`c << 5`), where a 20-byte stride costs a multiply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(align(32))]
pub struct Class {
    /// Slot size in bytes; also the usable size of every allocation in it.
    pub size: u32,
    /// `log2(slab size)`.
    pub lg_slab: u8,
    /// Slots per slab, `slab_size / size` rounded down.
    pub nslots: u32,
    /// `slab_size - 1`: the in-slab offset is `rel & slab_mask`.
    pub slab_mask: u32,
    /// The inverse modulo `2^32` of the odd part of `size` (`size >> tz`):
    /// the multiplier of the slot-start test in [`Class::slot_of_offset`].
    pub inv_odd: u32,
    /// `size.trailing_zeros()`.
    pub tz: u8,
    /// Bits one slab occupies in the class's slot bitmap (design section
    /// 4): `nslots` padded so that a slab's bits are either whole words or
    /// one range inside a word, and so that a bitmap cache line is shared
    /// by at most [`LG_GROUP_SPAN`] bytes of slabs. See [`Class::slab_bits`].
    pub slab_bits: u16,
}

/// Cache line the slot bitmaps are laid out for, in bits (64 bytes).
pub const LINE_BITS: usize = 512;

/// Slabs are handed to an arena in groups that own whole bitmap lines
/// (`class/`, design 4); a group spans at most this much of the
/// class region (2 MiB), so a class whose slabs are large pads each slab's
/// bits to a line rather than letting one line's group cover tens of MiB
/// of address space per arena.
pub const LG_GROUP_SPAN: u32 = 21;

impl Class {
    /// The geometry every class in [`CLASSES`] has: a size between the
    /// quantum and [`MEDIUM_MAX`], a slab between [`MIN_SLAB`] and
    /// [`MAX_SLAB`] holding `nslots` slots, and `slab_mask` the slab size
    /// minus one.
    pub open spec fn wf(self) -> bool {
        QUANTUM <= self.size <= MEDIUM_MAX
            && LG_MIN_SLAB <= self.lg_slab <= LG_MAX_SLAB
            && 1 <= self.nslots
            && self.nslots as int * self.size as int <= MAX_SLAB
            && (self.nslots as int) * (self.size as int) <= (1usize << self.lg_slab)
            && self.slab_mask as int == (1usize << self.lg_slab) as int - 1
            && 1 <= self.slab_bits
            && (self.slab_bits <= LINE_BITS ==> crate::meta::is_pow2(self.slab_bits as usize))
            && self.nslots <= self.slab_bits
            && self.slab_bits <= 64 * MAX_SLAB_WORDS
            && (self.slab_bits >= 64 ==> self.slab_bits % 64 == 0)
            && self.inv_ok()
    }

    /// The constants of the slot-start test ([`Class::slot_of_offset`]):
    /// `size` is `d << tz` with `d` odd, and `inv_odd` is the inverse of `d`
    /// modulo `2^32`. Closed: only the test needs it.
    pub closed spec fn inv_ok(self) -> bool {
        let d = self.size >> (self.tz as u32);
        &&& self.tz < 32
        &&& d << (self.tz as u32) == self.size
        &&& d & 1 == 1
        &&& d.wrapping_mul(self.inv_odd) == 1
    }

    #[inline]
    pub const fn size(&self) -> (r: usize)
        ensures r == self.size,
    {
        self.size as usize
    }

    #[inline]
    pub const fn slab_size(&self) -> (r: usize)
        requires self.wf(),
        ensures r == 1usize << self.lg_slab, r >= 0x1000,
        opens_invariants none
    {
        proof {
            let lg = self.lg_slab;
            assert((1usize << lg) >= 0x1000) by (bit_vector) requires 12 <= lg <= 22;
        }
        1 << self.lg_slab
    }

    #[inline]
    pub const fn nslots(&self) -> (r: usize)
        ensures r == self.nslots,
    {
        self.nslots as usize
    }

    #[cfg(test)]
    pub const fn tier(&self) -> Tier {
        if self.size as usize <= SMALL_MAX {
            Tier::Small
        } else {
            Tier::Medium
        }
    }

    /// Slots per [`LG_COMPACT_UNIT`] bytes of slabs (per slab when the slab
    /// is larger): the unit of the compaction thresholds in `heap/`, equal
    /// to `nslots` for every class whose slab is 64 KiB or more.
    #[inline]
    pub const fn compact_slots(&self) -> (r: usize)
        requires self.wf(),
        ensures
            r <= 0x8_0000,
            r == (if (1usize << self.lg_slab) > 0x1_0000usize { (1usize << self.lg_slab) as int } else { 0x1_0000int }) / (self.size as int),
    {
        let unit = 1usize << LG_COMPACT_UNIT;
        let slab = self.slab_size();
        proof {
            let lg = self.lg_slab;
            assert((1usize << lg) <= 0x40_0000) by (bit_vector) requires lg <= 22;
            assert(unit == 0x1_0000) by (bit_vector) requires unit == 1usize << 16u32;
            let m = if slab > unit { slab } else { unit };
            vstd::arithmetic::div_mod::lemma_div_is_ordered_by_denominator(m as int, 8, self.size as int);
        }
        (if slab > unit { slab } else { unit }) / self.size()
    }

    /// Bits one slab occupies in the class bitmap (the `slab_bits` field):
    /// `nslots` rounded up to a power of two below [`LINE_BITS`], or to a
    /// multiple of it from there, so a slab's bits are whole words (64 or
    /// more bits) or one sub-word range (fewer) and a line holds a whole
    /// number of slabs; then raised so that the slabs sharing one line span
    /// at most `1 << LG_GROUP_SPAN` bytes of the region.
    #[inline]
    pub const fn slab_bits(&self) -> (r: usize)
        ensures r == self.slab_bits,
    {
        self.slab_bits as usize
    }

    /// Consecutive slabs whose bits fill one bitmap line: the unit in which
    /// `class/` hands slabs to an arena, so that every line has one owner.
    /// A power of two; 1 when a slab's bits are a line or more.
    #[inline]
    pub const fn slabs_per_line(&self) -> (r: usize)
        requires self.wf(),
        ensures
            1 <= r <= LINE_BITS,
            r == if self.slab_bits >= LINE_BITS { 1 } else { LINE_BITS / (self.slab_bits as usize) },
            crate::meta::is_pow2(r),
    {
        let b = self.slab_bits();
        proof {
            if b < LINE_BITS {
                vstd::arithmetic::div_mod::lemma_div_is_ordered_by_denominator(512, 1, b as int);
                vstd::arithmetic::div_mod::lemma_div_is_ordered(b as int, 512, b as int);
                vstd::arithmetic::div_mod::lemma_div_by_self(b as int);
                // `b` is a power of two below the line, so `512 / b` is one.
                let k = choose|k: nat| k < 64 && b as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
                vstd::arithmetic::power2::lemma2_to64();
                if k > 9 {
                    vstd::arithmetic::power2::lemma_pow2_strictly_increases(9, k);
                }
                assert(k < 9);
                let ku = k as usize;
                vstd::bits::lemma_usize_shl_is_mul(1, ku);
                let t = (9 - ku) as usize;
                assert(512usize / (1usize << ku) == 1usize << t) by (bit_vector)
                    requires ku < 9, t == (9 - ku) as usize;
                crate::meta::lemma_is_pow2_shl(t);
            } else {
                crate::meta::lemma_is_pow2_one();
            }
        }
        if b >= LINE_BITS { 1 } else { LINE_BITS / b }
    }

    /// `u64` words a slab's bits touch in the class bitmap: `slab_bits / 64`
    /// for whole-word slabs, 1 for a sub-word slab.
    #[cfg(test)]
    pub const fn bitmap_words(&self) -> usize {
        self.slab_bits().div_ceil(64)
    }

    /// Slot index for an in-slab offset, plus whether the offset is
    /// exactly a slot start. The index is only meaningful when `valid` is
    /// true. `offset` must be below the slab size (`rel & slab_mask()`,
    /// as every caller passes): the test works on its low 32 bits, so a
    /// larger offset would be truncated and could alias a slot start of
    /// the wrong slab. Debug builds assert it.
    ///
    /// One multiply, one rotate and one compare (the divisibility test of
    /// Granlund and Montgomery, design 3): with `size = d << tz`,
    /// `d` odd, `q = (offset * d^-1 mod 2^32).rotate_right(tz)` is
    /// `offset / size` when `size` divides `offset`; otherwise a low bit
    /// of `offset` below `tz` rotates into the top bits of `q`, or the
    /// multiply of a non-multiple of `d` lands above `(2^(32 - tz) - 1) /
    /// d`, and either way `q >= nslots` (`nslots * size <= slab size <=
    /// 2^22`). So `q < nslots` accepts exactly the slot starts.
    ///
    /// `valid` holds exactly when `offset` is the start of one of the slab's
    /// slots, and `slot` is then that slot.
    #[inline]
    pub const fn slot_of_offset(&self, offset: usize) -> (r: (usize, bool))
        requires self.wf(), offset < (1usize << self.lg_slab),
        ensures
            r.1 ==> r.0 < self.nslots && r.0 * self.size == offset,
            offset % (self.size as usize) == 0 && offset / (self.size as usize) < self.nslots ==> r.1,
        opens_invariants none
    {
        debug_assert!(
            offset < self.slab_size(),
            "slot_of_offset: offset outside the slab"
        );
        let q = (offset as u32)
            .wrapping_mul(self.inv_odd)
            .rotate_right(self.tz as u32);
        proof {
            lemma_slot_test(*self, offset, q);
        }
        (q as usize, q < self.nslots)
    }

    /// `slab_size - 1` (the `slab_mask` field).
    #[inline]
    pub const fn slab_mask(&self) -> (r: usize)
        ensures r == self.slab_mask,
        opens_invariants none
    {
        self.slab_mask as usize
    }
}

/// `a * 1 == a` modulo `2^32`.
proof fn lemma_wmul_one(a: u32)
    ensures
        a.wrapping_mul(1u32) == a,
{
    vstd::arithmetic::div_mod::lemma_small_mod(a as nat, 0x1_0000_0000);
}

/// Multiplication modulo `2^32` is associative.
proof fn lemma_wmul_assoc(a: u32, b: u32, c: u32)
    ensures
        a.wrapping_mul(b).wrapping_mul(c) == a.wrapping_mul(b.wrapping_mul(c)),
{
    let m = 0x1_0000_0000int;
    vstd::arithmetic::div_mod::lemma_mul_mod_noop_right(a as int, (b as int) * (c as int), m);
    vstd::arithmetic::div_mod::lemma_mul_mod_noop_left((a as int) * (b as int), c as int, m);
    vstd::arithmetic::mul::lemma_mul_is_associative(a as int, b as int, c as int);
}

/// The rotate of the slot-start test: `q`, `y` rotated right by `t`, is
/// below a bound of `2^22 >> t` or less only if the low `t` bits of `y`
/// are clear (they would rotate into the top of `q`), so `y` is `q << t`;
/// and it is `y >> t` when they are clear.
proof fn lemma_rotate(y: u32, t: u32, q: u32, bound: u32)
    requires
        t < 32,
        q == crate::vspec::core_int::rotr32(y, t),
        bound <= 0x40_0000u32 >> t,
    ensures
        q < bound ==> y == q << t,
        (y >> t) << t == y && y >> t < bound ==> q == y >> t,
{
    assert(q < bound ==> y == q << t) by (bit_vector)
        requires
            t < 32,
            t == 0 ==> q == y,
            t != 0 ==> q == (y >> t) | (y << ((32 - t) as u32)),
            bound <= 0x40_0000u32 >> t,
    ;
    assert((y >> t) << t == y && y >> t < bound ==> q == y >> t) by (bit_vector)
        requires
            t < 32,
            t == 0 ==> q == y,
            t != 0 ==> q == (y >> t) | (y << ((32 - t) as u32)),
            bound <= 0x40_0000u32 >> t,
    ;
}

/// [`Class::slot_of_offset`]'s divisibility test (Granlund and
/// Montgomery): with `size = d << tz`, `d` odd and `inv_odd` its inverse,
/// `q` is below `nslots` exactly for the slot starts, and is then the
/// slot. A slot start `j * size` multiplies to `j << tz`, which rotates
/// back to `j`; conversely a `q` below `nslots` (at most `2^22 >> tz`)
/// means the product was `q << tz`, and multiplying it back by `d` gives
/// the offset as `q * size`, both below `2^22`.
pub(crate) proof fn lemma_slot_test(c: Class, offset: usize, q: u32)
    requires
        c.wf(),
        offset < (1usize << c.lg_slab),
        q == crate::vspec::core_int::rotr32((offset as u32).wrapping_mul(c.inv_odd), c.tz as u32),
    ensures
        q < c.nslots ==> q * c.size == offset,
        offset % (c.size as usize) == 0 && offset / (c.size as usize) < c.nslots ==> q < c.nslots,
{
    let s = c.size;
    let t = c.tz as u32;
    let d = s >> t;
    let inv = c.inv_odd;
    let n = c.nslots;
    let lg = c.lg_slab as usize;
    assert(MEDIUM_MAX == 0x4_0000);
    assert(MAX_SLAB == 0x40_0000) by (bit_vector);
    assert(offset < 0x40_0000) by (bit_vector)
        requires
            offset < (1usize << lg),
            12 <= lg <= 22,
    ;
    let o = offset as u32;
    let y = o.wrapping_mul(inv);
    // The slot count is at most `2^22 >> t`: `n << t <= n * s <= 2^22`.
    assert(t <= 18 && (1u32 << t) <= s) by (bit_vector)
        requires
            d << t == s,
            d & 1 == 1,
            t < 32,
            s <= 0x4_0000u32,
            d == s >> t,
    ;
    vstd::bits::lemma_u32_pow2_no_overflow(t as nat);
    vstd::bits::lemma_u32_shl_is_mul(1, t);
    vstd::bits::lemma_u32_shr_is_div(0x40_0000u32, t);
    vstd::arithmetic::power2::lemma_pow2_pos(t as nat);
    let p = vstd::arithmetic::power2::pow2(t as nat) as int;
    assert(n as int * p <= 0x40_0000) by (nonlinear_arith)
        requires
            n as int * s as int <= 0x40_0000,
            p <= s as int,
            n >= 1,
            p > 0,
    ;
    vstd::arithmetic::div_mod::lemma_div_is_ordered(n as int * p, 0x40_0000, p);
    vstd::arithmetic::div_mod::lemma_div_multiples_vanish(n as int, p);
    assert(n <= 0x40_0000u32 >> t);
    lemma_rotate(y, t, q, n);
    if q < n {
        // `y` is `q << t`; times `d` it is `o` (`inv * d` is 1) and `q * s`.
        assert(q.wrapping_mul(s) == (q << t).wrapping_mul(d)) by (bit_vector)
            requires
                s == d << t,
                t < 32,
        ;
        lemma_wmul_assoc(o, inv, d);
        assert(inv.wrapping_mul(d) == d.wrapping_mul(inv)) by (bit_vector);
        lemma_wmul_one(o);
        assert(q as int * s as int <= 0x40_0000) by (nonlinear_arith)
            requires
                q < n,
                n as int * s as int <= 0x40_0000,
                s > 0,
        ;
        vstd::arithmetic::div_mod::lemma_small_mod((q as int * s as int) as nat, 0x1_0000_0000);
    }
    if offset % (s as usize) == 0 && offset / (s as usize) < n {
        // `o` is `j * s`, so `y` is `(j << t) * (d * inv)`, `j << t`.
        let j = (offset / (s as usize)) as u32;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(offset as int, s as int);
        assert(j as int == offset as int / s as int);
        vstd::arithmetic::mul::lemma_mul_is_commutative(j as int, s as int);
        assert(j as int * s as int == offset as int);
        vstd::arithmetic::div_mod::lemma_small_mod((j as int * s as int) as nat, 0x1_0000_0000);
        assert(j.wrapping_mul(s) == (j << t).wrapping_mul(d)) by (bit_vector)
            requires
                s == d << t,
                t < 32,
        ;
        lemma_wmul_assoc(j << t, d, inv);
        let u = j << t;
        lemma_wmul_one(u);
        assert(y == u);
        assert(u >> t == j && (u >> t) << t == u) by (bit_vector)
            requires
                u == j << t,
                j < n,
                n <= 0x40_0000u32 >> t,
                t < 32,
        ;
    }
}

/// Multiplication modulo `2^64` is associative.
pub(crate) proof fn lemma_wmul64_assoc(a: u64, b: u64, c: u64)
    ensures
        a.wrapping_mul(b).wrapping_mul(c) == a.wrapping_mul(b.wrapping_mul(c)),
{
    let m = 0x1_0000_0000_0000_0000int;
    vstd::arithmetic::div_mod::lemma_mul_mod_noop_right(a as int, (b as int) * (c as int), m);
    vstd::arithmetic::div_mod::lemma_mul_mod_noop_left((a as int) * (b as int), c as int, m);
    vstd::arithmetic::mul::lemma_mul_is_associative(a as int, b as int, c as int);
}

/// The rotate of the 64-bit slot-start test ([`lemma_rotate`] on 64
/// bits): `q`, `y` rotated right by `t`, is below a bound of `2^22 >> t`
/// or less only if the low `t` bits of `y` are clear, so `y` is `q << t`.
proof fn lemma_rotate64(y: u64, t: u64, q: u64, bound: u64)
    requires
        t < 64,
        q == crate::vspec::core_int::rotr64(y, t as u32),
        bound <= 0x40_0000u64 >> t,
    ensures
        q < bound ==> y == q << t,
{
    assert(q < bound ==> y == q << t) by (bit_vector)
        requires
            t < 64,
            t == 0 ==> q == y,
            t != 0 ==> q == (y >> t) | (y << sub(64u64, t)),
            bound <= 0x40_0000u64 >> t,
    ;
}

/// The sized free's exact-division test on 64 bits
/// (`heap::ClassLine::sized_ok`, [`lemma_slot_test`] widened): with
/// `size = d << tz`, `d` odd and `inv` its inverse modulo `2^64`, a `q =
/// (x * inv).rotate_right(tz)` below `nslots` means `x` is `q * size`,
/// whatever bits `x` has: the product was `q << tz`, and multiplying it
/// back by `d` gives `x` modulo `2^64` and `q * size`, below `2^22`.
pub(crate) proof fn lemma_slot_test64(c: Class, inv: u64, x: u64, q: u64)
    requires
        c.wf(),
        ((c.size as u64) >> (c.tz as u64)).wrapping_mul(inv) == 1,
        q == crate::vspec::core_int::rotr64(x.wrapping_mul(inv), c.tz as u32),
        q < c.nslots as u64,
    ensures
        x as int == q as int * c.size as int,
        (q as int * c.size as int) < 0x40_0000,
{
    let s32 = c.size;
    let t32 = c.tz as u32;
    let s = c.size as u64;
    let t = c.tz as u64;
    let d = s >> t;
    let n = c.nslots as u64;
    assert(MEDIUM_MAX == 0x4_0000);
    assert(MAX_SLAB == 0x40_0000) by (bit_vector);
    let d32 = s32 >> t32;
    assert(t <= 18 && d << t == s && d & 1 == 1 && (1u64 << t) <= s && d <= s) by (bit_vector)
        requires
            d32 << t32 == s32,
            d32 & 1 == 1,
            t32 < 32,
            s32 <= 0x4_0000u32,
            d32 == s32 >> t32,
            s == s32 as u64,
            t == t32 as u64,
            d == s >> t,
    ;
    vstd::bits::lemma_u64_pow2_no_overflow(t as nat);
    vstd::bits::lemma_u64_shl_is_mul(1, t);
    vstd::bits::lemma_u64_shr_is_div(0x40_0000u64, t);
    vstd::arithmetic::power2::lemma_pow2_pos(t as nat);
    let p = vstd::arithmetic::power2::pow2(t as nat) as int;
    assert(n as int * p <= 0x40_0000) by (nonlinear_arith)
        requires
            n as int * s as int <= 0x40_0000,
            p <= s as int,
            n >= 1,
            p > 0,
    ;
    vstd::arithmetic::div_mod::lemma_div_is_ordered(n as int * p, 0x40_0000, p);
    vstd::arithmetic::div_mod::lemma_div_multiples_vanish(n as int, p);
    assert(n <= 0x40_0000u64 >> t);
    let y = x.wrapping_mul(inv);
    lemma_rotate64(y, t, q, n);
    // `y` is `q << t`; times `d` it is `x` (`inv * d` is 1) and `q * s`.
    assert(y == q << t);
    lemma_wmul64_assoc(x, inv, d);
    assert(inv.wrapping_mul(d) == d.wrapping_mul(inv)) by (bit_vector);
    vstd::arithmetic::div_mod::lemma_small_mod(x as nat, 0x1_0000_0000_0000_0000);
    assert(x.wrapping_mul(1u64) == x);
    assert(y.wrapping_mul(d) == x);
    assert((q as int * s as int) < 0x40_0000) by (nonlinear_arith)
        requires
            q < n,
            n as int * s as int <= 0x40_0000,
            s > 0,
    ;
    assert(q as int * p < 0x1_0000_0000_0000_0000 && d as int * p <= 0x10_0000_0000) by (nonlinear_arith)
        requires
            (q as int * s as int) < 0x40_0000,
            p <= s as int,
            q >= 0,
            d as int <= s as int,
            s as int <= 0x4_0000,
            p >= 1,
    ;
    vstd::bits::lemma_u64_shl_is_mul(q, t);
    vstd::bits::lemma_u64_shl_is_mul(d, t);
    assert((q as int * p) * (d as int) == q as int * s as int) by (nonlinear_arith)
        requires
            s as int == d as int * p,
    ;
    assert(q as int * p < 0x1_0000_0000_0000_0000) by (nonlinear_arith)
        requires
            q as int * s as int <= 0x40_0000,
            p <= s as int,
            q >= 0,
    ;
    assert(y as int == q as int * p);
    vstd::arithmetic::div_mod::lemma_small_mod((q as int * s as int) as nat, 0x1_0000_0000_0000_0000);
    assert(y.wrapping_mul(d) as int == q as int * s as int);
}

/// The low half of the 64-bit inverse of a class's odd part is its
/// 32-bit inverse, `inv_odd` (the inverse modulo `2^32` is unique), and
/// the inverse of `size >> trailing_zeros(size)` is that of `size >> tz`.
pub(crate) proof fn lemma_inv_low(c: Class, inv: u64)
    requires
        c.wf(),
        ((c.size as u64) >> (vstd::std_specs::bits::u64_trailing_zeros(c.size as u64) as u64)).wrapping_mul(inv) == 1,
    ensures
        ((c.size as u64) >> (c.tz as u64)).wrapping_mul(inv) == 1,
        inv as u32 == c.inv_odd,
{
    let s32 = c.size;
    let t32 = c.tz as u32;
    let s = c.size as u64;
    let d32 = s32 >> t32;
    vstd::std_specs::bits::axiom_u64_trailing_zeros(s);
    let tz = vstd::std_specs::bits::u64_trailing_zeros(s) as u64;
    let t = c.tz as u64;
    assert(MEDIUM_MAX == 0x4_0000);
    // `tz` is the size's trailing zero count: the odd part is `d`.
    assert(tz == t) by (bit_vector)
        requires
            d32 << t32 == s32,
            d32 & 1 == 1,
            t32 < 32,
            d32 == s32 >> t32,
            s == s32 as u64,
            t == t32 as u64,
            1 <= s,
            tz <= 64,
            tz < 64 ==> (s >> tz) & 1u64 == 1u64,
            s << sub(64, tz) == 0,
            s == 0 <==> tz == 64,
    ;
    let d = s >> t;
    let a = inv as u32;
    assert(d as u32 == d32) by (bit_vector)
        requires d == s >> t, s == s32 as u64, t == t32 as u64, t32 < 32, d32 == s32 >> t32;
    // `d * a` is the low half of `d * inv`, 1.
    let dw = d.wrapping_mul(inv);
    assert(dw as u32 == (d as u32).wrapping_mul(inv as u32)) by (bit_vector)
        requires dw == d.wrapping_mul(inv);
    assert(d32.wrapping_mul(a) == 1);
    // So `a == a * (d * inv_odd) == (a * d) * inv_odd == inv_odd`.
    let w = c.inv_odd;
    assert(d32.wrapping_mul(w) == 1);
    lemma_wmul_assoc(a, d32, w);
    assert(a.wrapping_mul(d32) == d32.wrapping_mul(a)) by (bit_vector);
    lemma_wmul_one(a);
    let one: u32 = 1;
    assert(one.wrapping_mul(w) == w) by (bit_vector)
        requires one == 1u32;
}

} // verus!

verus! {

/// Slot size of class `i` (the `size` field of every table entry):
/// `(i + 1) * QUANTUM` for the [`LINEAR_CLASSES`], then four classes per
/// doubling, `2^lg + k * 2^(lg - 2)` for `k` in `1..=4`.
pub open spec fn spec_class_size(i: int) -> int {
    if i < LINEAR_CLASSES as int {
        (i + 1) * QUANTUM as int
    } else {
        let base = vstd::arithmetic::power2::pow2((LG_LINEAR_MAX as int + (i - LINEAR_CLASSES as int) / 4) as nat) as int;
        base + ((i - LINEAR_CLASSES as int) % 4 + 1) * (base / 4)
    }
}

/// `2^(lg - 2)` is a quarter of `2^lg`.
proof fn lemma_quarter(lg: nat)
    requires
        lg >= 2,
    ensures
        vstd::arithmetic::power2::pow2(lg) / 4 == vstd::arithmetic::power2::pow2((lg - 2) as nat),
        vstd::arithmetic::power2::pow2(lg) == 4 * vstd::arithmetic::power2::pow2((lg - 2) as nat),
{
    vstd::arithmetic::power2::lemma_pow2_adds((lg - 2) as nat, 2);
    vstd::arithmetic::power2::lemma2_to64();
}

/// Every class size is a multiple of the quantum: `(i + 1) * QUANTUM` for
/// the linear classes, and a multiple of `2^(lg - 2)`, at least four
/// quanta, above them.
pub proof fn lemma_class_size_quantum(i: int)
    requires
        0 <= i < NCLASSES,
    ensures
        spec_class_size(i) % (QUANTUM as int) == 0,
{
    let q = QUANTUM as int;
    if i < LINEAR_CLASSES as int {
        vstd::arithmetic::div_mod::lemma_mod_multiples_basic(i + 1, q);
    } else {
        vstd::arithmetic::power2::lemma2_to64();
        let g = (i - LINEAR_CLASSES as int) / 4;
        let k = (i - LINEAR_CLASSES as int) % 4;
        let lg = (LG_LINEAR_MAX as int + g) as nat;
        lemma_quarter(lg);
        let b = vstd::arithmetic::power2::pow2(lg) as int;
        let qq = vstd::arithmetic::power2::pow2((lg - 2) as nat) as int;
        // `QUANTUM` is `2^(LG_LINEAR_MAX - 3)`, and `2^(lg - 2)` that times
        // `2^(g + 1)`.
        let lq = (LG_LINEAR_MAX - 3) as nat;
        assert(q == vstd::arithmetic::power2::pow2(lq));
        vstd::arithmetic::power2::lemma_pow2_adds(lq, (g + 1) as nat);
        assert((lq + (g + 1)) as nat == (lg - 2) as nat);
        let m = vstd::arithmetic::power2::pow2((g + 1) as nat) as int;
        assert(qq == q * m);
        assert(spec_class_size(i) == b + (k + 1) * (b / 4));
        let s = spec_class_size(i);
        assert(s == b + (k + 1) * qq);
        assert(s == (k + 5) * qq) by (nonlinear_arith)
            requires
                s == b + (k + 1) * qq,
                b == 4 * qq,
        ;
        assert((k + 5) * qq == (k + 5) * m * q) by (nonlinear_arith)
            requires
                qq == q * m,
        ;
        vstd::arithmetic::div_mod::lemma_mod_multiples_basic((k + 5) * m, q);
    }
}

/// The class sizes grow strictly with the class number.
pub proof fn lemma_class_size_mono(i: int, j: int)
    requires
        0 <= i < j < NCLASSES,
    ensures
        spec_class_size(i) < spec_class_size(j),
    decreases j - i,
{
    if j > i + 1 {
        lemma_class_size_mono(i, j - 1);
        lemma_class_size_mono(j - 1, j);
    } else if j >= LINEAR_CLASSES {
        vstd::arithmetic::power2::lemma2_to64();
        let gj = (j - LINEAR_CLASSES as int) / 4;
        let kj = (j - LINEAR_CLASSES as int) % 4;
        let lg = (LG_LINEAR_MAX as int + gj) as nat;
        lemma_quarter(lg);
        let b = vstd::arithmetic::power2::pow2(lg) as int;
        let q = vstd::arithmetic::power2::pow2((lg - 2) as nat) as int;
        vstd::arithmetic::power2::lemma_pow2_pos((lg - 2) as nat);
        assert(b / 4 == q && b == 4 * q && q > 0);
        assert(spec_class_size(j) == b + (kj + 1) * q);
        if j == LINEAR_CLASSES {
            assert(gj == 0 && kj == 0 && lg == LG_LINEAR_MAX);
            assert((kj + 1) * q == q) by (nonlinear_arith)
                requires kj == 0;
            assert(b == 8 * QUANTUM);
            assert(spec_class_size(i) == 8 * QUANTUM);
            assert(spec_class_size(j) == b + q);
        } else if kj > 0 {
            assert((i - LINEAR_CLASSES as int) / 4 == gj && (i - LINEAR_CLASSES as int) % 4 == kj - 1);
            assert(spec_class_size(i) == b + kj * q);
            assert(kj * q < (kj + 1) * q) by (nonlinear_arith)
                requires q > 0;
        } else {
            // `i` closes the previous doubling at `2 * 2^(lg - 1)`, which is `2^lg`.
            assert((i - LINEAR_CLASSES as int) / 4 == gj - 1 && (i - LINEAR_CLASSES as int) % 4 == 3);
            lemma_quarter((lg - 1) as nat);
            vstd::arithmetic::power2::lemma_pow2_adds((lg - 1) as nat, 1);
            let b1 = vstd::arithmetic::power2::pow2((lg - 1) as nat) as int;
            assert(spec_class_size(i) == b1 + 4 * (b1 / 4));
            assert((kj + 1) * q == q) by (nonlinear_arith)
                requires kj == 0;
            assert(2 * b1 == b);
            assert(b1 / 4 * 4 == b1) by { lemma_quarter((lg - 1) as nat); }
            assert(spec_class_size(i) == b);
        }
    } else {
        assert(spec_class_size(i) == (i + 1) * QUANTUM);
        assert(spec_class_size(j) == (j + 1) * QUANTUM);
    }
}

/// Above the linear classes, the class before `j` is `j`'s doubling base
/// `2^lg` plus `k` quarters of it, `k` being `j`'s place in its doubling:
/// `(4 + k) * 2^(lg - 2)`.
pub proof fn lemma_class_size_prev(j: int)
    requires
        LINEAR_CLASSES <= j < NCLASSES,
    ensures
        spec_class_size(j - 1) == (4 + (j - LINEAR_CLASSES as int) % 4)
            * vstd::arithmetic::power2::pow2((LG_LINEAR_MAX as int + (j - LINEAR_CLASSES as int) / 4 - 2) as nat),
{
    vstd::arithmetic::power2::lemma2_to64();
    let i = j - 1;
    let gj = (j - LINEAR_CLASSES as int) / 4;
    let kj = (j - LINEAR_CLASSES as int) % 4;
    let lg = (LG_LINEAR_MAX as int + gj) as nat;
    lemma_quarter(lg);
    let b = vstd::arithmetic::power2::pow2(lg) as int;
    let q = vstd::arithmetic::power2::pow2((lg - 2) as nat) as int;
    assert(b / 4 == q && b == 4 * q);
    if j == LINEAR_CLASSES {
        assert(gj == 0 && kj == 0 && lg == LG_LINEAR_MAX);
        assert(b == 8 * QUANTUM);
        assert(spec_class_size(i) == 8 * QUANTUM);
    } else if kj > 0 {
        assert((i - LINEAR_CLASSES as int) / 4 == gj && (i - LINEAR_CLASSES as int) % 4 == kj - 1);
        assert(spec_class_size(i) == b + kj * q);
        assert(b + kj * q == (4 + kj) * q) by (nonlinear_arith)
            requires b == 4 * q;
    } else {
        // `i` closes the previous doubling at `2 * 2^(lg - 1)`, which is `2^lg`.
        assert((i - LINEAR_CLASSES as int) / 4 == gj - 1 && (i - LINEAR_CLASSES as int) % 4 == 3);
        lemma_quarter((lg - 1) as nat);
        vstd::arithmetic::power2::lemma_pow2_adds((lg - 1) as nat, 1);
        let b1 = vstd::arithmetic::power2::pow2((lg - 1) as nat) as int;
        assert(spec_class_size(i) == b1 + 4 * (b1 / 4));
        assert(2 * b1 == b);
        assert(b1 / 4 * 4 == b1) by { lemma_quarter((lg - 1) as nat); }
        assert(spec_class_size(i) == b);
    }
}

/// No class below `c` holds `n` bytes: `c` is the smallest class at or
/// above `n` when it holds them. Opaque, so that its quantifier stays
/// out of the proofs that only pass it on (`build_small_lookup`,
/// `SMALL_LOOKUP`'s postcondition): `reveal` it where it is proved or used.
#[verifier::opaque]
pub open spec fn classes_below(c: int, n: int) -> bool {
    forall|j: int| 0 <= j < c ==> #[trigger] spec_class_size(j) < n
}

/// The last class is [`MEDIUM_MAX`].
pub proof fn lemma_last_class_size()
    ensures
        spec_class_size((NCLASSES - 1) as int) == MEDIUM_MAX,
{
    vstd::arithmetic::power2::lemma2_to64();
    assert((NCLASSES - 1 - LINEAR_CLASSES) / 4 == 17 - LG_LINEAR_MAX && (NCLASSES - 1 - LINEAR_CLASSES) % 4 == 3);
}

const fn class_size_at(i: usize) -> (r: usize)
    requires i < NCLASSES,
    ensures QUANTUM <= r <= MEDIUM_MAX, r == spec_class_size(i as int),
{
    if i < LINEAR_CLASSES {
        (i + 1) * QUANTUM
    } else {
        let g = (i - LINEAR_CLASSES) / 4;
        let k = (i - LINEAR_CLASSES) % 4;
        proof {
            assert(LINEAR_CLASSES * QUANTUM == 1usize << LG_LINEAR_MAX) by (compute_only);
            let lgl = LG_LINEAR_MAX as usize;
            assert(lgl + g <= 17);
            assert(((1usize << lgl) << g) <= 0x2_0000 && ((1usize << lgl) << g) >= (1usize << lgl)
                && (1usize << lgl) >= 64) by (bit_vector)
                requires lgl + g <= 17, 6 <= lgl <= 7;
        }
        let base = (LINEAR_CLASSES * QUANTUM) << g;
        proof {
            assert(base + (k + 1) * (base / 4) <= 2 * base) by (nonlinear_arith)
                requires k < 4;
            let lgl = LG_LINEAR_MAX as usize;
            let e = (lgl + g) as usize;
            assert(base == 1usize << e) by (bit_vector)
                requires base == (1usize << lgl) << g, e == lgl + g, e <= 17;
            vstd::bits::lemma_usize_pow2_no_overflow(e as nat);
            vstd::bits::lemma_usize_shl_is_mul(1, e);
        }
        base + (k + 1) * (base / 4)
    }
}

/// Slab size of a class (see [`LG_MIN_SLAB`]): `1 << LG_SMALL_SLAB` below
/// [`SMALL_SLAB_BELOW`]; from there to [`SMALL_MAX`] the smallest power of
/// two from [`MIN_SLAB`] up whose internal waste is at most
/// `slab / SLAB_WASTE_DIV`; above, `max(1 << LG_MEDIUM_MIN_SLAB, 16 * size)`
/// rounded up to a power of two.
pub const fn lg_slab_for(size: usize) -> (r: u32)
    requires 1 <= size <= MEDIUM_MAX,
    ensures
        LG_MIN_SLAB <= r <= LG_MAX_SLAB,
        size <= SMALL_MAX ==> r <= LG_SMALL_SLAB,
        size > SMALL_MAX ==> (1usize << r) >= 16 * size,
        size > SMALL_MAX ==> r == LG_MEDIUM_MIN_SLAB || (1usize << r) < 32 * size,
        size > SMALL_MAX ==> r >= LG_MEDIUM_MIN_SLAB,
        size < SMALL_SLAB_BELOW ==> r == LG_SMALL_SLAB,
{
    if size < SMALL_SLAB_BELOW {
        return LG_SMALL_SLAB;
    }
    if size <= SMALL_MAX {
        let mut lg = LG_MIN_SLAB;
        while lg < LG_SMALL_SLAB
            invariant LG_MIN_SLAB <= lg <= LG_SMALL_SLAB, 1 <= size,
            decreases LG_SMALL_SLAB - lg,
        {
            let slab = 1usize << lg;
            if slab % size <= slab / SLAB_WASTE_DIV {
                break;
            }
            lg += 1;
        }
        return lg;
    }
    let want = (16 * size).next_power_of_two();
    let lg = want.trailing_zeros();
    proof {
        assert(16 * size <= 0x40_0000);
        let k = choose|k: nat| k < 64 && want as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
        let ku = k as u64;
        vstd::bits::lemma_u64_shl_is_mul(1u64, ku);
        vstd::arithmetic::power2::lemma2_to64();
        let w = want as u64;
        assert(w == 1u64 << ku);
        vstd::std_specs::bits::axiom_u64_trailing_zeros(w);
        let tz = lg as u64;
        assert(w == 1u64 << tz && tz < 23) by (bit_vector)
            requires
                w == 1u64 << ku,
                ku < 64,
                w < 0x80_0000,
                tz < 64,
                (w >> tz) & 1u64 == 1u64,
                w << sub(64, tz) == 0;
        assert((1usize << lg) == want) by (bit_vector)
            requires (want as u64) == 1u64 << (lg as u64), lg < 23;
        assert(lg < 16 ==> (1usize << 16u32) > (1usize << lg)) by (bit_vector)
            requires lg < 23;
    }
    if lg < LG_MEDIUM_MIN_SLAB { LG_MEDIUM_MIN_SLAB } else { lg }
}

} // verus!

const _: () = assert!(LG_SMALL_SLAB >= LG_MIN_SLAB && LG_SMALL_SLAB <= LG_MAX_SLAB);
const _: () = assert!(LG_MEDIUM_MIN_SLAB >= LG_MIN_SLAB && LG_MEDIUM_MIN_SLAB <= LG_MAX_SLAB);
const _: () = assert!(
    SMALL_SLAB_BELOW <= SMALL_MAX && MIN_SLAB >= 4 * SMALL_MAX,
    "a small-slab class has at least four slots"
);
const _: () = assert!(LG_COMPACT_UNIT >= LG_MIN_SLAB && LG_COMPACT_UNIT <= LG_MAX_SLAB);

verus! {

/// `size` is its odd part `d` shifted left by its trailing zeros `t`,
/// fewer than 32.
proof fn lemma_odd_part(size: usize)
    requires 1 <= size <= MEDIUM_MAX,
    ensures
        ({
            let t = vstd::std_specs::bits::u64_trailing_zeros(size as u64);
            let d = (size as u32) >> t;
            &&& t < 32
            &&& d << t == size as u32
            &&& d & 1 == 1
        }),
{
    let w = size as u64;
    vstd::std_specs::bits::axiom_u64_trailing_zeros(w);
    let t = vstd::std_specs::bits::u64_trailing_zeros(w) as u64;
    assert(t < 32) by (bit_vector)
        requires 1 <= w <= 0x4_0000u64, t <= 64, t < 64 ==> (w >> t) & 1u64 == 1u64, w == 0 <==> t == 64;
    let s = size as u32;
    let tt = vstd::std_specs::bits::u64_trailing_zeros(size as u64);
    assert(MEDIUM_MAX == 0x4_0000);
    assert((s >> tt) & 1 == 1 && (s >> tt) << tt == s) by (bit_vector)
        requires
            1 <= w <= 0x4_0000u64,
            s == w as u32,
            tt as u64 == t,
            t < 32,
            (w >> t) & 1u64 == 1u64,
            w << sub(64, t) == 0,
    ;
}

/// Every odd `d` has an inverse modulo `2^32`: four steps of Newton's
/// iteration from `x = d` ([`lemma_newton`]), the steps
/// [`inverse_of_odd_part`]'s loop takes, for spec code, which cannot call
/// it.
proof fn lemma_odd_inverse(d: u32)
    requires d & 1 == 1,
    ensures exists|v: u32| d.wrapping_mul(v) == 1,
{
    assert(d & 1 == 1 ==> d.wrapping_mul(d) & 7 == 1) by (bit_vector);
    let x1 = d.wrapping_mul(2u32.wrapping_sub(d.wrapping_mul(d)));
    lemma_wmul_assoc(d, d, 2u32.wrapping_sub(d.wrapping_mul(d)));
    lemma_newton(d.wrapping_mul(d));
    let x2 = x1.wrapping_mul(2u32.wrapping_sub(d.wrapping_mul(x1)));
    lemma_wmul_assoc(d, x1, 2u32.wrapping_sub(d.wrapping_mul(x1)));
    lemma_newton(d.wrapping_mul(x1));
    let x3 = x2.wrapping_mul(2u32.wrapping_sub(d.wrapping_mul(x2)));
    lemma_wmul_assoc(d, x2, 2u32.wrapping_sub(d.wrapping_mul(x2)));
    lemma_newton(d.wrapping_mul(x2));
    let x4 = x3.wrapping_mul(2u32.wrapping_sub(d.wrapping_mul(x3)));
    lemma_wmul_assoc(d, x3, 2u32.wrapping_sub(d.wrapping_mul(x3)));
    lemma_newton(d.wrapping_mul(x3));
    assert(d.wrapping_mul(x4) == 1);
}

/// The inverse of the odd part of `size` modulo `2^32` (the `inv_odd`
/// field): Newton's iteration `x = x * (2 - d * x)` doubles the correct
/// low bits from the 3 that `x = d` has for odd `d`.
const fn inverse_of_odd_part(size: usize) -> (r: u32)
    requires 1 <= size <= MEDIUM_MAX,
    ensures
        ({
            let t = vstd::std_specs::bits::u64_trailing_zeros(size as u64);
            let d = (size as u32) >> t;
            &&& t < 32
            &&& d << t == size as u32
            &&& d & 1 == 1
            &&& d.wrapping_mul(r) == 1
        }),
{
    let ghost t32 = vstd::std_specs::bits::u64_trailing_zeros(size as u64);
    proof {
        lemma_odd_part(size);
    }
    let d = (size as u32) >> size.trailing_zeros();
    let mut x = d;
    let mut i = 0;
    proof {
        assert(d & 1 == 1 ==> d.wrapping_mul(d) & 7 == 1) by (bit_vector);
    }
    while i < 4
        invariant
            0 <= i <= 4,
            d == (size as u32) >> t32,
            d & 1 == 1,
            i == 0 ==> d.wrapping_mul(x) & 7 == 1,
            i == 1 ==> d.wrapping_mul(x) & 0x3f == 1,
            i == 2 ==> d.wrapping_mul(x) & 0xfff == 1,
            i == 3 ==> d.wrapping_mul(x) & 0xff_ffff == 1,
            i == 4 ==> d.wrapping_mul(x) == 1,
        decreases 4 - i,
    {
        let ghost x0 = x;
        let ghost e = d.wrapping_mul(x);
        x = x.wrapping_mul(2u32.wrapping_sub(d.wrapping_mul(x)));
        proof {
            lemma_wmul_assoc(d, x0, 2u32.wrapping_sub(e));
            lemma_newton(e);
        }
        i += 1;
    }
    x
}

/// One step of Newton's iteration for the inverse modulo `2^32`: if
/// `e = d * x` is 1 in its low `k` bits, `d * x * (2 - e) = e * (2 - e)`
/// is 1 in its low `2k`.
proof fn lemma_newton(e: u32)
    ensures
        e & 7 == 1 ==> e.wrapping_mul(2u32.wrapping_sub(e)) & 0x3f == 1,
        e & 0x3f == 1 ==> e.wrapping_mul(2u32.wrapping_sub(e)) & 0xfff == 1,
        e & 0xfff == 1 ==> e.wrapping_mul(2u32.wrapping_sub(e)) & 0xff_ffff == 1,
        e & 0xff_ffff == 1 ==> e.wrapping_mul(2u32.wrapping_sub(e)) == 1,
{
    assert(e & 7 == 1 ==> e.wrapping_mul(2u32.wrapping_sub(e)) & 0x3f == 1) by (bit_vector);
    assert(e & 0x3f == 1 ==> e.wrapping_mul(2u32.wrapping_sub(e)) & 0xfff == 1) by (bit_vector);
    assert(e & 0xfff == 1 ==> e.wrapping_mul(2u32.wrapping_sub(e)) & 0xff_ffff == 1) by (bit_vector);
    assert(e & 0xff_ffff == 1 ==> e.wrapping_mul(2u32.wrapping_sub(e)) == 1) by (bit_vector);
}

/// See [`Class::slab_bits`].
const fn slab_bits_for(nslots: usize, lg_slab: u32) -> (r: usize)
    requires 1 <= nslots <= 0x2000, LG_MIN_SLAB <= lg_slab <= LG_MAX_SLAB,
    ensures
        nslots <= r,
        1 <= r <= 0x2200,
        r >= 64 ==> r % 64 == 0,
        r <= LINE_BITS || (r % LINE_BITS == 0 && r < nslots + LINE_BITS),
        r <= LINE_BITS ==> crate::meta::is_pow2(r),
{
    let raw = if nslots <= LINE_BITS { nslots.next_power_of_two() } else { (nslots + LINE_BITS - 1) & !(LINE_BITS - 1) };
    proof {
        if nslots > LINE_BITS {
            let x = ((nslots + 511) as usize) & !511usize;
            assert(nslots <= x <= nslots + 511 && x % 512 == 0 && x % 64 == 0) by (bit_vector)
                requires nslots <= 0x2000, x == ((nslots + 511) as usize) & !511usize;
        } else {
            let k = choose|k: nat| k < 64 && raw as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
            let ku = k as u64;
            vstd::bits::lemma_u64_shl_is_mul(1u64, ku);
            vstd::arithmetic::power2::lemma2_to64();
            let w = raw as u64;
            assert(w == 1u64 << ku);
            assert((w >= 64 && w <= 1024 ==> w % 64 == 0) && (w < 1024 ==> w <= 512)) by (bit_vector)
                requires w == 1u64 << ku, ku < 64;
        }
    }
    let per_line = if raw >= LINE_BITS { 1 } else { LINE_BITS / raw };
    let cap = if LG_GROUP_SPAN > lg_slab { 1usize << (LG_GROUP_SPAN - lg_slab) } else { 1 };
    proof {
        let s = (LG_GROUP_SPAN - lg_slab) as usize;
        if LG_GROUP_SPAN > lg_slab {
            assert(1 <= (1usize << s) <= 512) by (bit_vector)
                requires 1 <= s <= 9;
        }
    }
    proof {
        if LG_GROUP_SPAN > lg_slab {
            let s: u32 = (LG_GROUP_SPAN - lg_slab) as u32;
            let q = 512usize / (1usize << s);
            assert(q <= 512 && (q >= 64 ==> q % 64 == 0)) by (bit_vector)
                requires 1 <= s <= 9, q == 512usize / (1usize << s);
            assert(cap == 1usize << s);
        }
    }
    if per_line > cap {
        proof {
            // 512 / raw > cap, so raw * (cap + 1) <= 512 and 512 / cap >= raw.
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(512int, raw as int);
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(512int, cap as int);
            assert(raw * (cap + 1) <= 512) by (nonlinear_arith)
                requires 512int == raw * per_line + 512int % (raw as int), 512int % (raw as int) >= 0, per_line >= cap + 1, raw >= 1;
            assert(512int / (cap as int) >= raw) by (nonlinear_arith)
                requires raw * (cap + 1) <= 512, cap >= 1, 512int == cap * (512int / (cap as int)) + 512int % (cap as int),
                    512int % (cap as int) < cap;
            // `512 / 2^s` is `2^(9 - s)`; `512` itself without a cap.
            if LG_GROUP_SPAN > lg_slab {
                let s: u32 = (LG_GROUP_SPAN - lg_slab) as u32;
                let t = (9 - s) as usize;
                assert(512usize / (1usize << s) == 1usize << t) by (bit_vector)
                    requires 1 <= s <= 9, t == (9 - s) as usize;
                crate::meta::lemma_is_pow2_shl(t);
            } else {
                assert(512usize == 1usize << 9usize) by (bit_vector);
                crate::meta::lemma_is_pow2_shl(9);
            }
        }
        LINE_BITS / cap
    } else {
        raw
    }
}

/// Slab size of a class on a kernel with `1 << lg_page` pages: the
/// compile-time [`lg_slab_for`] value, raised to the page when the page is
/// larger, but never above [`LG_SMALL_SLAB`] (every table slab below
/// 64 KiB belongs to the 512..1024-byte classes, so the rule is exactly
/// "no slab smaller than the page" on every supported page size, 4, 16 and
/// 64 KiB; a page above 64 KiB, which no supported kernel has, keeps the
/// table geometry and the page groups of `class/`).
pub const fn lg_slab_at(size: usize, lg_page: u32) -> (r: u32)
    requires 1 <= size <= MEDIUM_MAX,
    ensures
        LG_MIN_SLAB <= r <= LG_MAX_SLAB,
        size <= SMALL_MAX ==> r <= LG_SMALL_SLAB,
        size > SMALL_MAX ==> (1usize << r) >= 16 * size,
        size > SMALL_MAX ==> r == LG_MEDIUM_MIN_SLAB || (1usize << r) < 32 * size,
        size < SMALL_SLAB_BELOW ==> r == LG_SMALL_SLAB,
{
    let lg = lg_slab_for(size);
    let floor = if lg_page < LG_SMALL_SLAB { lg_page } else { LG_SMALL_SLAB };
    let r = if lg < floor { floor } else { lg };
    proof {
        if size > SMALL_MAX && r != lg {
            assert((1usize << r) >= (1usize << lg)) by (bit_vector)
                requires lg <= r <= 22;
        }
    }
    r
}

/// The class table for a kernel with `1 << lg_page` pages: sizes and
/// their inverses are fixed, the slab geometry follows [`lg_slab_at`].
pub const fn classes_for_page(lg_page: u32) -> (out: [Class; NCLASSES])
    ensures
        forall|i: int| 0 <= i < NCLASSES ==> #[trigger] out[i].wf(),
        forall|i: int| 0 <= i < NCLASSES ==> #[trigger] out[i].size == spec_class_size(i),
{
    let mut out = [Class {
        size: 0,
        lg_slab: 0,
        nslots: 0,
        slab_mask: 0,
        inv_odd: 0,
        tz: 0,
        slab_bits: 0,
    }; NCLASSES];
    let mut i = 0;
    while i < NCLASSES
        invariant
            0 <= i <= NCLASSES,
            forall|j: int| 0 <= j < i ==> #[trigger] out[j].wf(),
            forall|j: int| 0 <= j < i ==> #[trigger] out[j].size == spec_class_size(j),
        decreases NCLASSES - i,
    {
        let size = class_size_at(i);
        let lg_slab = lg_slab_at(size, lg_page);
        proof {
            assert(MAX_SLAB == 0x40_0000) by (bit_vector);
            assert(4096 <= (1usize << lg_slab) <= 0x40_0000) by (bit_vector)
                requires 12 <= lg_slab <= 22;
        }
        let nslots = (1usize << lg_slab) / size;
        proof {
            let slab = (1usize << lg_slab) as int;
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(slab, size as int);
            vstd::arithmetic::div_mod::lemma_mod_bound(slab, size as int);
            if size <= SMALL_MAX {
                assert(slab >= 4 * size);
            }
            assert(1 <= nslots) by (nonlinear_arith)
                requires slab == size * nslots + slab % (size as int), 0 <= slab % (size as int) < size, slab >= 4 * size, size >= 1;
            assert(nslots * size <= slab) by (nonlinear_arith)
                requires slab == size * nslots + slab % (size as int), 0 <= slab % (size as int);
            if size < SMALL_SLAB_BELOW || size <= SMALL_MAX {
                assert((1usize << lg_slab) <= 0x1_0000) by (bit_vector) requires lg_slab <= 16;
                assert(nslots <= 0x2000) by (nonlinear_arith)
                    requires nslots * size <= slab, slab <= 0x1_0000, size >= 8;
                assert(nslots * QUANTUM <= 0x1_0000) by (nonlinear_arith)
                    requires nslots * size <= slab, slab <= 0x1_0000, size >= QUANTUM;
            } else if lg_slab == LG_MEDIUM_MIN_SLAB {
                assert((1usize << lg_slab) == 0x1_0000) by (bit_vector) requires lg_slab == 16;
                assert(nslots <= 0x2000) by (nonlinear_arith)
                    requires nslots * size <= slab, slab <= 0x1_0000, size >= 8;
                assert(nslots * QUANTUM <= 0x1_0000) by (nonlinear_arith)
                    requires nslots * size <= slab, slab <= 0x1_0000, size >= QUANTUM;
            } else {
                assert(nslots < 32) by (nonlinear_arith)
                    requires nslots * size <= slab, slab < 32 * size, size >= 1;
            }
        }
        let slab_bits = slab_bits_for(nslots, lg_slab);
        proof {
            // `64 * MAX_SLAB_WORDS` is `64 KiB / QUANTUM`, a multiple of
            // `LINE_BITS` that bounds every slab of 64 KiB or less.
            assert(64 * MAX_SLAB_WORDS * QUANTUM == 0x1_0000 && (64 * MAX_SLAB_WORDS) % 512 == 0);
            if nslots >= 32 {
                assert(nslots <= 64 * MAX_SLAB_WORDS) by (nonlinear_arith)
                    requires nslots * QUANTUM <= 0x1_0000, 64 * MAX_SLAB_WORDS * QUANTUM == 0x1_0000, QUANTUM > 0;
            }
            if slab_bits > LINE_BITS {
                assert(slab_bits <= 64 * MAX_SLAB_WORDS) by (nonlinear_arith)
                    requires
                        slab_bits % 512 == 0,
                        slab_bits < nslots + 512,
                        nslots <= 64 * MAX_SLAB_WORDS,
                        (64 * MAX_SLAB_WORDS) % 512 == 0;
            }
        }
        out[i] = Class {
            size: size as u32,
            lg_slab: lg_slab as u8,
            nslots: nslots as u32,
            slab_mask: ((1usize << lg_slab) - 1) as u32,
            inv_odd: inverse_of_odd_part(size),
            tz: size.trailing_zeros() as u8,
            slab_bits: slab_bits as u16,
        };
        i += 1;
    }
    out
}

pub(crate) const fn build_classes() -> (r: [Class; NCLASSES])
    ensures
        forall|i: int| 0 <= i < NCLASSES ==> #[trigger] r[i].wf(),
        forall|i: int| 0 <= i < NCLASSES ==> #[trigger] r[i].size == spec_class_size(i),
{
    classes_for_page(LG_MIN_SLAB)
}

} // verus!

verus! {

/// The compile-time class table: the geometry of a kernel whose page is at
/// most [`MIN_SLAB`] (4 KiB, the reference hosts), and the reference the
/// tests and the compile-time constants ([`NBINS`], [`MAX_SLAB_WORDS`],
/// `slab::DESC_STRIDE`) are built from.
pub exec const BASE_CLASSES: [Class; NCLASSES] = build_classes();

} // verus!

verus! {

/// The size-class table in use, indexed by class number: [`BASE_CLASSES`]
/// with the slab geometry of the classes below the kernel page raised to
/// the page by [`adopt_page_geometry`] once at init. It is a table at a
/// fixed address, not a pointer selected at init
/// and not a runtime shift where `lg_slab` is read, so its readers
/// (`Regions::slab_slot` and the slow paths) load a class's geometry
/// exactly as they did from the compile-time table: one indexed load per
/// field from a link-time constant address, nothing dependent added. The
/// `free` and `sdallocx` fast paths read a copy `Shared::init` makes into
/// each class's `heap::ClassLine`, after this adoption. The price is that the table lives in
/// writable memory; the process-wide state the fast paths already trust
/// (`Shared`'s `Regions`) does too.
#[verifier::external_body]
pub struct ClassTable(UnsafeCell<[Class; NCLASSES]>);

/// The contents of [`CLASSES`] after init. Uninterpreted;
/// [`axiom_class_table`] fixes it.
pub uninterp spec fn classes_after_init() -> [Class; NCLASSES];

/// The class table in use: the contents of [`CLASSES`] after init
/// ([`classes_after_init`]) when they are [`table_ok`], as
/// [`adopt_page_geometry`] leaves them, and otherwise a spec table that
/// is [`table_ok`] ([`spec_ok_table`]), so every entry is well formed
/// without an assumption. Opaque; `vspec::class::lemma_class_table_wf` reveals it.
#[verifier::opaque]
pub open(crate) spec fn class_table() -> [Class; NCLASSES] {
    if table_ok(classes_after_init()) {
        classes_after_init()
    } else {
        spec_ok_table(NCLASSES as int)
    }
}

/// A spec entry for class `i` that is well formed and sized
/// [`spec_class_size`] ([`lemma_spec_ok_entry`]): one slot in a
/// [`MAX_SLAB`] slab, one bitmap bit, and the size's slot-start constants.
closed spec fn spec_ok_entry(i: int) -> Class {
    let s = spec_class_size(i) as u32;
    let t = vstd::std_specs::bits::u64_trailing_zeros(s as u64);
    let d = s >> t;
    Class {
        size: s,
        lg_slab: LG_MAX_SLAB as u8,
        nslots: 1,
        slab_mask: (MAX_SLAB - 1) as u32,
        inv_odd: choose|v: u32| d.wrapping_mul(v) == 1,
        tz: t as u8,
        slab_bits: 1,
    }
}

/// A table whose entries below `n` are [`spec_ok_entry`]'s
/// ([`lemma_spec_ok_table`]).
pub(crate) closed spec fn spec_ok_table(n: int) -> [Class; NCLASSES]
    decreases n,
{
    if n <= 0 {
        vstd::array::spec_array_fill_for_copy_type(spec_ok_entry(0))
    } else {
        vstd::array::spec_array_update(spec_ok_table(n - 1), n - 1, spec_ok_entry(n - 1))
    }
}

/// [`spec_ok_entry`] is well formed and sized [`spec_class_size`].
proof fn lemma_spec_ok_entry(i: int)
    requires
        0 <= i < NCLASSES,
    ensures
        spec_ok_entry(i).wf(),
        spec_ok_entry(i).size == spec_class_size(i),
{
    lemma_last_class_size();
    if i > 0 {
        lemma_class_size_mono(0, i);
    }
    if i < NCLASSES - 1 {
        lemma_class_size_mono(i, NCLASSES - 1);
    }
    let z = spec_class_size(i);
    assert(QUANTUM <= z <= MEDIUM_MAX);
    lemma_odd_part(z as usize);
    let e = spec_ok_entry(i);
    let s = z as u32;
    let t = vstd::std_specs::bits::u64_trailing_zeros(s as u64);
    let d = s >> t;
    lemma_odd_inverse(d);
    assert(d.wrapping_mul(e.inv_odd) == 1);
    assert((t as u8) as u32 == t);
    assert(e.inv_ok());
    let lg = e.lg_slab;
    assert(MAX_SLAB == 0x40_0000) by (bit_vector);
    assert((1usize << lg) == 0x40_0000) by (bit_vector)
        requires lg == 22u8;
    vstd::arithmetic::power2::lemma2_to64();
    assert(crate::meta::is_pow2(1usize)) by {
        assert(1usize as nat == vstd::arithmetic::power2::pow2(0));
    }
}

/// The entries of [`spec_ok_table`]`(n)` below `n` are [`spec_ok_entry`]'s.
proof fn lemma_spec_ok_table(n: int)
    requires
        0 <= n <= NCLASSES,
    ensures
        forall|j: int| 0 <= j < n ==> #[trigger] spec_ok_table(n)[j] == spec_ok_entry(j),
    decreases n,
{
    broadcast use vstd::array::axiom_spec_array_update, vstd::array::lemma_array_index;
    if n > 0 {
        lemma_spec_ok_table(n - 1);
        let prev = spec_ok_table(n - 1);
        vstd::array::array_len_matches_n(&prev);
        assert(spec_ok_table(n)@ == prev@.update(n - 1, spec_ok_entry(n - 1)));
        assert forall|j: int| 0 <= j < n implies #[trigger] spec_ok_table(n)[j] == spec_ok_entry(j) by {
            assert(spec_ok_table(n)[j] == spec_ok_table(n)@[j]);
            if j < n - 1 {
                assert(prev[j] == prev@[j]);
            }
        }
    }
}

/// [`spec_ok_table`]`(NCLASSES)` is [`table_ok`]: [`class_table`]'s
/// fallback.
pub(crate) proof fn lemma_spec_ok_table_ok()
    ensures
        table_ok(spec_ok_table(NCLASSES as int)),
{
    lemma_spec_ok_table(NCLASSES as int);
    assert forall|i: int| 0 <= i < NCLASSES implies #[trigger] spec_ok_table(NCLASSES as int)[i].wf() by {
        lemma_spec_ok_entry(i);
    }
    assert forall|i: int| 0 <= i < NCLASSES implies #[trigger] spec_ok_table(NCLASSES as int)[i].size
        == spec_class_size(i) by {
        lemma_spec_ok_entry(i);
    }
}

/// Trusted: a [`table_ok`] value that a frozen permission to the storage
/// of [`CLASSES`] holds is [`class_table`], since it is
/// [`classes_after_init`], the contents of [`CLASSES`] after init. `api::init` freezes
/// the one permission to it (`api::axiom_statics` hands it out once, for
/// the process's one-shot token) after [`adopt_page_geometry`], and
/// nothing writes the table through a `'static` shared permission, so
/// every such permission holds that one value.
pub axiom fn axiom_class_table(tracked t: &'static vstd::raw_ptr::PointsTo<[Class; NCLASSES]>)
    requires
        t.ptr() == classes_storage(),
        t.is_init(),
        table_ok(t.value()),
    ensures
        class_table() == t.value(),
;

impl ClassTable {
    #[verifier::external_body]
    const fn new(table: [Class; NCLASSES]) -> ClassTable {
        ClassTable(UnsafeCell::new(table))
    }

    /// The table, for verified code: [`class_table`], whose entries are
    /// well formed and sized [`spec_class_size`]
    /// (`vspec::class::lemma_class_table_wf`). Trusted: the table is
    /// written only before the allocator is published (the `Sync` impl),
    /// its value is [`table_ok`] (`api::axiom_statics` gives
    /// [`BASE_CLASSES`]' value and [`adopt_page_geometry`] keeps it),
    /// without which [`class_table`] is its spec fallback, and what this
    /// assumes is that a reader sees the value `init` left:
    /// `Shared::init` reads the table before the allocator is published,
    /// and `Deref` (`CLASSES[c]`) can state no precondition.
    #[verifier::external_body]
    #[inline(always)]
    pub fn table(&self) -> (r: &[Class; NCLASSES])
        ensures
            *r == class_table(),
    {
        // SAFETY: see the `Sync` impl.
        unsafe { &*self.0.get() }
    }
}

} // verus!

// SAFETY: the table is written only by `adopt_page_geometry`, once, by the
// initialising thread before the allocator is published (`api::init`),
// through a `&mut` to the whole table, so nothing may read it during that
// call (`nallocx`, for one, gets the initialised `Shared` before it
// reads a class size). Only the geometry fields change: `size`,
// `inv_odd` and `tz` keep their compile-time values. After that it is
// read-only for the life of the process.
unsafe impl Sync for ClassTable {}

verus! {

impl Deref for ClassTable {
    type Target = [Class; NCLASSES];

    /// The table, as [`ClassTable::table`] gives it.
    #[inline(always)]
    fn deref(&self) -> &[Class; NCLASSES] {
        self.table()
    }
}

pub exec static CLASSES: ClassTable = ClassTable::new(BASE_CLASSES);

/// Every entry of `t` is well formed and sized [`spec_class_size`]: what
/// [`BASE_CLASSES`] is (`build_classes`'s postcondition) and what
/// [`apply_page_geometry`] keeps.
pub open spec fn table_ok(t: [Class; NCLASSES]) -> bool {
    &&& forall|i: int| 0 <= i < NCLASSES ==> #[trigger] t[i].wf()
    &&& forall|i: int| 0 <= i < NCLASSES ==> #[trigger] t[i].size == spec_class_size(i)
}

/// The storage of [`CLASSES`]' table.
pub uninterp spec fn classes_storage() -> *mut [Class; NCLASSES];

/// The storage of [`CLASSES`]' table, as a raw pointer. Trusted: a
/// raw-pointer projection of the static.
#[verifier::external_body]
#[inline(always)]
fn classes_ptr() -> (p: *mut [Class; NCLASSES])
    ensures
        p == classes_storage(),
    opens_invariants none
{
    CLASSES.0.get()
}

/// Raise the slab of every class whose table slab is smaller than the
/// kernel page to the page ([`lg_slab_at`]), writing `lg_slab`,
/// `slab_mask`, `nslots` and `slab_bits` of those classes in [`CLASSES`],
/// through `perm`, the permission to its storage, and keeping
/// [`table_ok`]. Returns how many classes changed (0 on a 4 KiB kernel,
/// where the table is untouched).
///
/// # Safety
/// Must be called once, before the allocator is published to any other
/// thread (`api::init` does, right after the page-size probe), and never
/// again: nothing may access the table concurrently.
pub unsafe fn adopt_page_geometry(
    lg_page: u32,
    Tracked(perm): Tracked<&mut vstd::raw_ptr::PointsTo<[Class; NCLASSES]>>,
) -> (changed: usize)
    requires
        old(perm).ptr() == classes_storage(),
        old(perm).is_init(),
        table_ok(old(perm).value()),
    ensures
        final(perm).ptr() == classes_storage(),
        final(perm).is_init(),
        table_ok(final(perm).value()),
        changed <= NCLASSES,
{
    apply_page_geometry(vstd::raw_ptr::ptr_mut_ref(classes_ptr(), Tracked(perm)), lg_page)
}

} // verus!

verus! {

/// `a` has `b`'s size and slot-start constants: what
/// [`apply_page_geometry`] keeps of every entry.
spec fn keeps_size(a: Class, b: Class) -> bool {
    &&& a.size == b.size
    &&& a.inv_odd == b.inv_odd
    &&& a.tz == b.tz
}

/// [`adopt_page_geometry`] over any table (the tests run it on a copy).
/// It writes the geometry fields only, so a table whose entries are well
/// formed and sized [`spec_class_size`] stays so (what makes
/// [`ClassTable::table`]'s `*r == class_table()` hold after init).
pub(crate) fn apply_page_geometry(table: &mut [Class; NCLASSES], lg_page: u32) -> (changed: usize)
    ensures
        changed <= NCLASSES,
        forall|i: int| #![trigger final(table)[i]] 0 <= i < NCLASSES ==> {
            &&& final(table)[i].size == old(table)[i].size
            &&& final(table)[i].inv_odd == old(table)[i].inv_odd
            &&& final(table)[i].tz == old(table)[i].tz
        },
        table_ok(*old(table)) ==> table_ok(*final(table)),
{
    let want = classes_for_page(lg_page);
    let mut changed = 0;
    for c in 0..NCLASSES
        invariant
            changed <= c,
            forall|i: int| 0 <= i < NCLASSES ==> keeps_size(#[trigger] table[i], old(table)[i]),
            forall|i: int| c <= i < NCLASSES ==> #[trigger] table[i] == old(table)[i],
            forall|i: int| 0 <= i < c ==> #[trigger] table[i] == old(table)[i] || {
                &&& table[i].lg_slab == want[i].lg_slab
                &&& table[i].nslots == want[i].nslots
                &&& table[i].slab_mask == want[i].slab_mask
                &&& table[i].slab_bits == want[i].slab_bits
            },
    {
        let w = &want[c];
        if table[c].lg_slab != w.lg_slab {
            table[c].lg_slab = w.lg_slab;
            table[c].nslots = w.nslots;
            table[c].slab_mask = w.slab_mask;
            table[c].slab_bits = w.slab_bits;
            changed += 1;
        }
    }
    proof {
        assert forall|i: int| #![trigger table[i]] 0 <= i < NCLASSES implies {
            &&& table[i].size == old(table)[i].size
            &&& table[i].inv_odd == old(table)[i].inv_odd
            &&& table[i].tz == old(table)[i].tz
        } by {
            assert(keeps_size(table[i], old(table)[i]));
        }
        if table_ok(*old(table)) {
            assert forall|i: int| 0 <= i < NCLASSES implies #[trigger] table[i].wf() by {
                assert(old(table)[i].wf());
                assert(old(table)[i].size == spec_class_size(i));
                if table[i] != old(table)[i] {
                    assert(want[i].wf() && want[i].size == spec_class_size(i));
                    assert(keeps_size(table[i], old(table)[i]));
                    assert(table[i].inv_ok() == old(table)[i].inv_ok());
                }
            }
            assert forall|i: int| 0 <= i < NCLASSES implies #[trigger] table[i].size == spec_class_size(i) by {
                assert(keeps_size(table[i], old(table)[i]));
                assert(old(table)[i].size == spec_class_size(i));
            }
        }
    }
    changed
}

} // verus!

verus! {

/// Classes whose slab in [`CLASSES`] is not the compile-time one (raised
/// to the page by [`adopt_page_geometry`]), as the inclusive range of
/// class numbers, or `None` when the geometry is the table's. For the
/// report (`malloc_stats_print`) and the tests.
// Verus does not support the closure form.
#[allow(clippy::manual_map)]
pub fn page_sized_classes() -> (r: Option<(usize, usize)>)
    ensures
        r matches Some((lo, hi)) ==> lo <= hi < NCLASSES,
{
    let table = CLASSES.table();
    let mut lo: Option<usize> = None;
    let mut hi: usize = 0;
    for c in it: 0..NCLASSES
        invariant
            hi < NCLASSES,
            lo matches Some(l) ==> l <= hi && hi < it.index(),
    {
        if table[c].lg_slab != BASE_CLASSES[c].lg_slab {
            if lo.is_none() {
                lo = Some(c);
            }
            hi = c;
        }
    }
    match lo {
        Some(lo) => Some((lo, hi)),
        None => None,
    }
}

} // verus!

verus! {

/// Bitmap words one slab occupies at most over all classes (`slab_bits /
/// 64`, at least 1): the size of the scratch bitmap a release batch
/// accumulates a slab's clears in (`ClassState::release_owned`), 128 words
/// (1 KiB) with 8-byte slots in a 64 KiB slab. Spelled per quantum so the
/// verifier sees the value ([`Class::wf`] bounds `slab_bits` by it); the
/// assertion below ties it to the table ([`max_slab_words`]).
#[cfg(feature = "quantum16")]
pub const MAX_SLAB_WORDS: usize = 64;
#[cfg(feature = "quantum8")]
pub const MAX_SLAB_WORDS: usize = 128;

const fn max_slab_words() -> (m: usize)
    ensures m >= 1,
{
    let classes = build_classes();
    let mut m = 1;
    let mut i = 0;
    while i < NCLASSES
        invariant 0 <= i <= NCLASSES, m >= 1,
        decreases NCLASSES - i,
    {
        let w = classes[i].slab_bits() / 64;
        if w > m {
            m = w;
        }
        i += 1;
    }
    m
}

/// Brute-force class lookup, used to build the small table at compile time
/// and by tests as the reference.
const fn class_of_slow(n: usize) -> (r: Option<usize>)
    ensures
        r matches Some(c) ==> c < NCLASSES && n <= spec_class_size(c as int),
        r matches Some(c) ==> classes_below(c as int, n as int),
        n <= MEDIUM_MAX ==> r is Some,
{
    let mut i = 0;
    while i < NCLASSES
        invariant
            0 <= i <= NCLASSES,
            forall|j: int| 0 <= j < i ==> #[trigger] spec_class_size(j) < n,
        decreases NCLASSES - i,
    {
        // `class_size_at(i)` is `BASE_CLASSES[i].size`.
        if class_size_at(i) >= n {
            proof {
                reveal(classes_below);
            }
            return Some(i);
        }
        i += 1;
    }
    proof {
        lemma_last_class_size();
        assert(spec_class_size((NCLASSES - 1) as int) < n);
    }
    None
}

const SMALL_LOOKUP_LEN: usize = SMALL_MAX / QUANTUM + 1;

/// Entry `e` of the small table at index `i` (requests of `i` quanta):
/// a class number times [`CLASS_STRIDE`], of the least class that holds
/// `i` quanta.
pub open spec fn small_entry_ok(e: u16, i: int) -> bool {
    let c = e as int / CLASS_STRIDE as int;
    &&& (e as int) % (CLASS_STRIDE as int) == 0
    &&& c < NCLASSES
    &&& i * QUANTUM <= spec_class_size(c)
    &&& classes_below(c, i * QUANTUM)
}

/// The `panic!` is unreachable: the last class is `MEDIUM_MAX`, above every
/// entry's request.
const fn build_small_lookup() -> (out: [u16; SMALL_LOOKUP_LEN])
    ensures
        forall|i: int| 0 <= i < SMALL_LOOKUP_LEN ==> small_entry_ok(#[trigger] out[i], i),
{
    let mut out = [0u16; SMALL_LOOKUP_LEN];
    let mut i = 0;
    while i < SMALL_LOOKUP_LEN
        invariant
            i <= SMALL_LOOKUP_LEN,
            forall|j: int| 0 <= j < i ==> small_entry_ok(#[trigger] out[j], j),
        decreases SMALL_LOOKUP_LEN - i,
    {
        // Entry `i` serves every request that rounds up to `i` quanta.
        out[i] = match class_of_slow(i * QUANTUM) {
            Some(c) => {
                proof {
                    assert(c * 64 < 0x1_0000 && (c * 64) / 64 == c as int && (c * 64) % 64 == 0) by (nonlinear_arith)
                        requires c < NCLASSES, NCLASSES <= 64;
                }
                (c * CLASS_STRIDE) as u16
            },
            None => panic!("small lookup out of range"),
        };
        i += 1;
    }
    out
}

/// The stride of the per-class lines the fast paths index: the heap's
/// cache headers and the shared class lines are both one 64-byte line
/// per class (`heap/` asserts both).
pub const CLASS_STRIDE: usize = 64;

/// Request size rounded up to quanta, then class number times
/// [`CLASS_STRIDE`] (design 3: "a small table for requests up to
/// 1024"). The fast paths scale the class by the stride to reach its
/// lines; with the scaled entry in the table, [`class_of_small`]'s
/// division and their multiplication cancel, which saves them a copy, a
/// shift and the register they took.
exec static SMALL_LOOKUP: [u16; SMALL_LOOKUP_LEN]
    ensures
        forall|i: int| 0 <= i < SMALL_LOOKUP_LEN ==> small_entry_ok(#[trigger] SMALL_LOOKUP[i], i),
{
    build_small_lookup()
}

} // verus!

const _: () = assert!(NCLASSES * CLASS_STRIDE <= u16::MAX as usize);

verus! {

/// The table entry for `n`: [`class_of`] for a small request, the class
/// number with the compiler told that the entry is a multiple of the
/// stride (so that `class * CLASS_STRIDE` folds back into the entry).
#[inline(always)]
// Hot size-class lookup: written for the codegen, not the idiom.
#[allow(clippy::manual_div_ceil, clippy::manual_is_multiple_of)]
fn small_class(n: usize) -> (r: usize)
    requires
        n <= SMALL_MAX,
    ensures
        r < NCLASSES && n <= spec_class_size(r as int),
        classes_below(r as int, n as int),
    opens_invariants none
{
    proof {
        reveal(classes_below);
        let q = ((n + QUANTUM - 1) as int) / (QUANTUM as int);
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod((n + QUANTUM - 1) as int, QUANTUM as int);
        vstd::arithmetic::div_mod::lemma_mod_bound((n + QUANTUM - 1) as int, QUANTUM as int);
        assert(n <= q * QUANTUM) by (nonlinear_arith)
            requires
                (n + QUANTUM - 1) as int == QUANTUM * q + ((n + QUANTUM - 1) as int) % (QUANTUM as int),
                ((n + QUANTUM - 1) as int) % (QUANTUM as int) < QUANTUM;
        assert(q < SMALL_LOOKUP_LEN);
        // Every class below the entry's is below `q` quanta, so, a
        // multiple of the quantum, at most `q - 1` of them: below `n`.
        // (`SMALL_LOOKUP` cannot be read here; its entry is a class
        // below `NCLASSES` and `classes_below` it at `q` quanta.)
        assert forall|j: int| 0 <= j < NCLASSES && #[trigger] spec_class_size(j) < q * QUANTUM implies spec_class_size(j) < n by {
            lemma_class_size_quantum(j);
            let s = spec_class_size(j);
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(s, QUANTUM as int);
            let t = s / (QUANTUM as int);
            assert(s < q * QUANTUM);
            assert(t * QUANTUM < q * QUANTUM) by (nonlinear_arith)
                requires s == QUANTUM * t + 0, s < q * QUANTUM;
            assert(t <= q - 1) by (nonlinear_arith)
                requires t * QUANTUM < q * QUANTUM, QUANTUM > 0;
            assert(t * QUANTUM <= (q - 1) * QUANTUM) by (nonlinear_arith)
                requires t <= q - 1, QUANTUM > 0;
            assert((q - 1) * QUANTUM < n) by (nonlinear_arith)
                requires
                    (n + QUANTUM - 1) as int == QUANTUM * q + ((n + QUANTUM - 1) as int) % (QUANTUM as int),
                    ((n + QUANTUM - 1) as int) % (QUANTUM as int) >= 0;
        }
    }
    let off = SMALL_LOOKUP[(n + QUANTUM - 1) / QUANTUM] as usize;
    // SAFETY: every entry is a class number times the stride
    // (`build_small_lookup`).
    unsafe { core::hint::assert_unchecked(off % CLASS_STRIDE == 0) };
    off / CLASS_STRIDE
}

/// Class number for a request of at most [`SMALL_MAX`] bytes; `n` must
/// be in range (the table index is bounds-checked regardless).
#[inline(always)]
pub fn class_of_small(n: usize) -> (r: usize)
    requires
        n <= SMALL_MAX,
    ensures
        r < NCLASSES && n <= spec_class_size(r as int),
        classes_below(r as int, n as int),
    opens_invariants none
{
    debug_assert!(n <= SMALL_MAX);
    small_class(n)
}

/// Class number for a request of `n` bytes, or `None` when `n` is huge.
/// A zero-byte request maps to class 0 so `malloc(0)` yields a real slot.
#[inline]
pub fn class_of(n: usize) -> (r: Option<usize>)
    ensures
        r is Some <==> n <= MEDIUM_MAX,
        r matches Some(c) ==> c < NCLASSES && n <= spec_class_size(c as int),
        r matches Some(c) ==> classes_below(c as int, n as int),
    opens_invariants none
{
    proof {
        reveal(classes_below);
    }
    if n <= SMALL_MAX {
        Some(small_class(n))
    } else if n <= MEDIUM_MAX {
        // Classes in (2^lg, 2^(lg+1)] are 2^lg + k * 2^(lg-2) for k = 1..4.
        let m = n - 1;
        proof {
            let mu = m as u64;
            vstd::std_specs::bits::axiom_u64_leading_zeros(mu);
            let lz = vstd::std_specs::bits::u64_leading_zeros(mu) as u64;
            assert(46 <= lz <= 53) by (bit_vector)
                requires
                    1024 <= mu < 0x4_0000,
                    lz <= 64,
                    lz < 64 ==> (mu >> sub(63u64, lz)) & 1u64 != 0u64,
                    mu >> sub(64u64, lz) == 0;
        }
        let lg = usize::BITS - 1 - m.leading_zeros();
        let k = (m >> (lg - 2)) & 3;
        proof {
            assert(k <= 3) by (bit_vector)
                requires k == (m >> sub(lg, 2u32) as usize) & 3usize;
            // `m` is `2^lg` plus `k` quarters of it plus less than one more:
            // `n = m + 1` is at most class `c`'s size, `(5 + k) * 2^(lg - 2)`.
            let mu = m as u64;
            let lz = vstd::std_specs::bits::u64_leading_zeros(mu) as u64;
            let l = lg as u64;
            let ku = k as u64;
            assert(l == 63 - lz);
            assert(ku == (mu >> sub(l, 2u64)) & 3u64) by (bit_vector)
                requires mu == m as u64, ku == k as u64, l == lg as u64, 10 <= l <= 17,
                    k == (m >> sub(lg, 2u32) as usize) & 3usize;
            let top = (5 + ku) as u64;
            assert(mu < top << sub(l, 2u64)) by (bit_vector)
                requires
                    46 <= lz <= 53,
                    l == 63 - lz,
                    top == 5 + ku,
                    (mu >> sub(63u64, lz)) & 1u64 != 0u64,
                    mu >> sub(64u64, lz) == 0,
                    ku == (mu >> sub(l, 2u64)) & 3u64;
            vstd::arithmetic::power2::lemma2_to64();
            if l - 2 < 15 {
                vstd::arithmetic::power2::lemma_pow2_strictly_increases((l - 2) as nat, 15);
            }
            assert(top * vstd::arithmetic::power2::pow2((l - 2) as nat) <= 8 * 0x8000) by (nonlinear_arith)
                requires top <= 8, vstd::arithmetic::power2::pow2((l - 2) as nat) <= 0x8000;
            vstd::bits::lemma_u64_shl_is_mul(top, sub(l, 2u64));
            vstd::bits::lemma_u64_pow2_no_overflow((l - 2) as nat);
            let q = vstd::arithmetic::power2::pow2((l - 2) as nat) as int;
            lemma_quarter(l as nat);
            let c = LINEAR_CLASSES + 4 * (lg - LG_LINEAR_MAX) as usize + k;
            assert((c - LINEAR_CLASSES) / 4 == lg - LG_LINEAR_MAX && (c - LINEAR_CLASSES) % 4 == k);
            assert(spec_class_size(c as int) == 4 * q + (k + 1) * q);
            assert(n <= (5 + k) * q);
            assert(4 * q + (k + 1) * q == (5 + k) * q) by (nonlinear_arith);
            // `m` is at least `2^lg` plus `k` quarters, the size of the
            // class before `c`: every class below `c` is below `n`.
            let low = (4 + ku) as u64;
            assert(low << sub(l, 2u64) <= mu) by (bit_vector)
                requires
                    46 <= lz <= 53,
                    l == 63 - lz,
                    low == 4 + ku,
                    (mu >> sub(63u64, lz)) & 1u64 != 0u64,
                    mu >> sub(64u64, lz) == 0,
                    ku == (mu >> sub(l, 2u64)) & 3u64;
            assert(low * vstd::arithmetic::power2::pow2((l - 2) as nat) <= 8 * 0x8000) by (nonlinear_arith)
                requires low <= 8, vstd::arithmetic::power2::pow2((l - 2) as nat) <= 0x8000;
            vstd::bits::lemma_u64_shl_is_mul(low, sub(l, 2u64));
            lemma_class_size_prev(c as int);
            assert(spec_class_size(c - 1) == (4 + k) * q);
            assert forall|j: int| 0 <= j < c implies #[trigger] spec_class_size(j) < n by {
                if j < c - 1 {
                    lemma_class_size_mono(j, c - 1);
                }
            }
        }
        Some(LINEAR_CLASSES + 4 * (lg - LG_LINEAR_MAX) as usize + k)
    } else {
        None
    }
}

} // verus!

/// Slot size for a request, i.e. `nallocx(n, 0)` for non-huge `n`.
#[inline]
#[cfg(test)]
pub fn class_size_of(n: usize) -> Option<usize> {
    class_of(n).map(|c| CLASSES[c].size())
}

const _: () = assert!(MAX_SLAB_WORDS == max_slab_words());

// The claim's re-check and the decay walk call these (slab-pages, milestone 1).
#[cfg_attr(not(test), allow(dead_code))]
mod pages;
mod regions;

#[cfg(all(verus_keep_ghost, not(test)))]
pub use regions::LG_REGION_PREFERRED;
pub use regions::{Kind, LG_HUGE_PREFERRED, Regions};
#[cfg(test)]
pub use regions::{
    LG_HUGE_MIN, LG_META_MIN, LG_REGION_MIN, LG_REGION_PREFERRED, MAX_SEALED, Mapping,
};
#[cfg(verus_keep_ghost)]
pub use regions::{
    base_aligned, bit_slots_in, disjoint, guard_geom, guard_of, guard_slab, huge_block,
    lemma_aligned_down, lemma_mod_lin3, lemma_pow2_divides_pow2, process_regions,
    shared_static_addr, shared_static_len, slots_in,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod linux_tests;

/// Kani checks of the class table's trusted access, `ClassTable::new`,
/// `ClassTable::table` and `classes_ptr`
/// (`verus/ledger/src/layout/mod.rs.md`; `cfg(kani)` only, see
/// `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_classes {
    use super::{
        BASE_CLASSES, CLASSES, Class, ClassTable, LG_LINEAR_MAX, LG_MAX_SLAB, LG_MIN_SLAB,
        LINE_BITS, LINEAR_CLASSES, MAX_SLAB, MAX_SLAB_WORDS, MEDIUM_MAX, NCLASSES,
        adopt_page_geometry, classes_ptr,
    };
    use crate::QUANTUM;
    use core::mem::{align_of, size_of};
    use vstd::prelude::Tracked;

    /// `spec_class_size(i)`: `(i + 1) * QUANTUM` for the linear classes,
    /// and above them `base + ((i - LINEAR_CLASSES) % 4 + 1) * (base / 4)`
    /// with `base = pow2(LG_LINEAR_MAX + (i - LINEAR_CLASSES) / 4)`.
    fn class_size(i: usize) -> u128 {
        if i < LINEAR_CLASSES {
            (i as u128 + 1) * QUANTUM as u128
        } else {
            let base = 1u128 << (LG_LINEAR_MAX as usize + (i - LINEAR_CLASSES) / 4);
            base + ((i - LINEAR_CLASSES) as u128 % 4 + 1) * (base / 4)
        }
    }

    /// `Class::wf`, with `inv_ok` and `meta::is_pow2` (a power of two
    /// below `2^64`: `is_power_of_two` of a `usize`), conjunct by conjunct.
    fn wf(c: &Class) -> bool {
        let (size, lg) = (c.size as u128, c.lg_slab as u32);
        if !(LG_MIN_SLAB <= lg && lg <= LG_MAX_SLAB) {
            return false;
        }
        let slab = 1u128 << lg;
        let bits = c.slab_bits as usize;
        QUANTUM as u128 <= size
            && size <= MEDIUM_MAX as u128
            && 1 <= c.nslots
            && c.nslots as u128 * size <= MAX_SLAB as u128
            && c.nslots as u128 * size <= slab
            && c.slab_mask as u128 == slab - 1
            && 1 <= bits
            && (bits > LINE_BITS || bits.is_power_of_two())
            && c.nslots as usize <= bits
            && bits <= 64 * MAX_SLAB_WORDS
            && (bits < 64 || bits % 64 == 0)
            && inv_ok(c)
    }

    /// `Class::inv_ok`: `size` is `d << tz` with `d` odd and `tz < 32`,
    /// and `d * inv_odd == 1` modulo `2^32`.
    fn inv_ok(c: &Class) -> bool {
        if c.tz >= 32 {
            return false;
        }
        let d = c.size >> c.tz;
        d << c.tz == c.size && d & 1 == 1 && d.wrapping_mul(c.inv_odd) == 1
    }

    /// `table_ok`: every entry well formed and sized `spec_class_size`.
    fn table_ok(t: &[Class; NCLASSES]) {
        for i in 0..NCLASSES {
            assert!(wf(&t[i]));
            assert!(t[i].size as u128 == class_size(i));
        }
    }

    /// `ClassTable::table`, `ensures *r == class_table()`: with
    /// `classes_after_init()` read as the contents of `CLASSES`' storage
    /// (what `axiom_class_table` fixes, from a permission to
    /// `classes_storage()`), the reference is to that storage
    /// (`classes_ptr`), and its entries are `table_ok`, so the storage is
    /// `class_table()`, before `adopt_page_geometry` (`BASE_CLASSES`) and
    /// after it at the kernel page sizes `lg_page` 12 to 16 (4 KiB to 64
    /// KiB). The adoption runs once, as at init (nothing else runs in a
    /// harness).
    #[kani::proof]
    #[kani::unwind(60)]
    fn class_table_entries() {
        let r = CLASSES.table();
        assert!(core::ptr::eq(r, classes_ptr()));
        assert!(*r == BASE_CLASSES);
        table_ok(r);
        // The address only: `r` must not be used once the adoption writes
        // the storage.
        let first: *const [Class; NCLASSES] = r;
        let lg_page: u32 = kani::any();
        kani::assume(12 <= lg_page && lg_page <= 16);
        // SAFETY: once, before any other access (nothing else runs in a
        // harness); the permission is ghost.
        unsafe { adopt_page_geometry(lg_page, Tracked::assume_new()) };
        let r2 = CLASSES.table();
        assert!(core::ptr::eq(r2, first));
        table_ok(r2);
    }

    /// `ClassTable::new`: no contract; it returns, and (beyond it) the
    /// table it makes holds its argument, at `BASE_CLASSES` (`CLASSES`'
    /// initialiser).
    #[kani::proof]
    #[kani::unwind(60)]
    fn class_table_new() {
        let t = ClassTable::new(BASE_CLASSES);
        assert!(*t.table() == BASE_CLASSES);
    }

    /// `classes_ptr`, `ensures p == classes_storage()`, uninterpreted:
    /// every call returns the same pointer. Beyond the contract, it is
    /// `CLASSES`' storage: non-null, aligned for the table, at the
    /// static's address, and its first and last bytes are writable through
    /// it (Kani fails a write outside the object `p` is derived from).
    #[kani::proof]
    fn classes_ptr_fixed() {
        let p = classes_ptr();
        assert!(classes_ptr() == p);
        assert!(!p.is_null());
        assert!(p as usize % align_of::<[Class; NCLASSES]>() == 0);
        assert!(p as usize == (&raw const CLASSES) as usize);
        let b = p.cast::<u8>();
        // SAFETY: the static's `UnsafeCell` storage, written only here (no
        // other code runs in a harness).
        unsafe {
            b.write(0);
            b.add(size_of::<[Class; NCLASSES]>() - 1).write(0);
        }
    }
}
