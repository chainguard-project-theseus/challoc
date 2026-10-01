//! Size classes, address-space regions, and pointer classification
//! (design.md sections 2 and 3).
//!
//! Everything in this module except [`Regions::reserve`] is pure arithmetic
//! over a class table fixed at init ([`CLASSES`]: the compile-time
//! [`BASE_CLASSES`] geometry, with the slabs of the classes below the
//! kernel page raised to the page by [`adopt_page_geometry`]) and a
//! handful of runtime bases, and is unit tested exhaustively. `reserve` is
//! the piece that probes how much address space the process can get and
//! lays the regions out.
//!
//! Submodules: [`regions`] holds the address-space parameters, [`Regions`]
//! and the reservation; `tests` the pure tests and `linux_tests` the
//! ones that need a kernel. The size-class math and its two statics stay
//! here.
//!
//! Layout recap. One superblock holds every class region at a fixed
//! power-of-two stride; class `c` lives at `superblock + c * stride +
//! random_offset_c`, with the rest of the stride left as an unmapped gap.
//! Class regions are aligned to [`MAX_SLAB`] so that `p & (slab_size - 1)` is
//! the offset inside the slab for every class. Separate reservations hold
//! the huge region and the metadata region.

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
/// Largest slab: 16 slots of `MEDIUM_MAX`. Class regions and their random
/// offsets are aligned to this so that in-slab offsets are a plain mask.
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

} // verus!
verus! {

/// Number of classes up to [`BIN_MAX`]: 28 for `quantum16`, 32 for `quantum8`.
pub exec const NBINS: usize = count_bins();

const fn count_bins() -> (n: usize)
    ensures n <= NCLASSES,
{
    let mut n = 0;
    while n < NCLASSES && class_size_at(n) <= BIN_MAX
        invariant n <= NCLASSES,
        decreases NCLASSES - n,
    {
        n += 1;
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

/// Allocation tier of a class (design section 3 table); the tests check
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
/// (`class/`, design section 4); a group spans at most this much of the
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
        ensures r <= 0x8_0000,
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
    /// Granlund and Montgomery, design section 3): with `size = d << tz`,
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
    // PORT-RESTORE: the divisibility test (inv_odd, tz) replaced the reciprocal; trusts that inv_odd/tz are what classes_for_page computes (add an inv_ok conjunct to Class::wf and prove Newton's iteration and the rotate test).
    #[verifier::external_body]
    pub const fn slot_of_offset(&self, offset: usize) -> (r: (usize, bool))
        requires self.wf(), offset < (1usize << self.lg_slab),
        ensures
            r.1 ==> r.0 < self.nslots && r.0 * self.size == offset,
            offset % (self.size as usize) == 0 && offset / (self.size as usize) < self.nslots ==> r.1,
    {
        debug_assert!(
            offset < self.slab_size(),
            "slot_of_offset: offset outside the slab"
        );
        let q = (offset as u32)
            .wrapping_mul(self.inv_odd)
            .rotate_right(self.tz as u32);
        (q as usize, q < self.nslots)
    }

    /// `slab_size - 1` (the `slab_mask` field).
    #[inline]
    pub const fn slab_mask(&self) -> (r: usize)
        ensures r == self.slab_mask,
    {
        self.slab_mask as usize
    }
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

/// The last class is [`MEDIUM_MAX`].
pub proof fn lemma_last_class_size()
    ensures
        spec_class_size((NCLASSES - 1) as int) == MEDIUM_MAX,
{
    vstd::arithmetic::power2::lemma2_to64();
    assert((NCLASSES - 1 - LINEAR_CLASSES) / 4 == 17 - LG_LINEAR_MAX && (NCLASSES - 1 - LINEAR_CLASSES) % 4 == 3);
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

/// The inverse of the odd part of `size` modulo `2^32` (the `inv_odd`
/// field): Newton's iteration `x = x * (2 - d * x)` doubles the correct
/// low bits from the 3 that `x = d` has for odd `d`.
const fn inverse_of_odd_part(size: usize) -> u32
    requires 1 <= size <= MEDIUM_MAX,
{
    proof {
        let w = size as u64;
        vstd::std_specs::bits::axiom_u64_trailing_zeros(w);
        let t = vstd::std_specs::bits::u64_trailing_zeros(w) as u64;
        assert(t < 32) by (bit_vector)
            requires 1 <= w <= 0x4_0000u64, t <= 64, t < 64 ==> (w >> t) & 1u64 == 1u64, w == 0 <==> t == 64;
    }
    let d = (size as u32) >> size.trailing_zeros();
    let mut x = d;
    let mut i = 0;
    while i < 4
        invariant i <= 4,
        decreases 4 - i,
    {
        x = x.wrapping_mul(2u32.wrapping_sub(d.wrapping_mul(x)));
        i += 1;
    }
    x
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
/// and not a runtime shift where `lg_slab` is read, so the `free` and
/// `sdallocx` fast paths (`Regions::slab_slot`, `Regions::slot_in_class`)
/// load a class's geometry exactly as they did from the compile-time
/// table: one indexed load per field from a link-time constant address,
/// nothing dependent added. The price is that the table lives in
/// writable memory; the process-wide state the fast paths already trust
/// (`Shared`'s `Regions`) does too.
#[verifier::external_body]
pub struct ClassTable(UnsafeCell<[Class; NCLASSES]>);

/// The contents of [`CLASSES`] after init.
pub uninterp spec fn class_table() -> [Class; NCLASSES];

impl ClassTable {
    #[verifier::external_body]
    const fn new(table: [Class; NCLASSES]) -> ClassTable {
        ClassTable(UnsafeCell::new(table))
    }

    /// The table, for verified code. Trusted: the table is [`BASE_CLASSES`]
    /// or, after [`adopt_page_geometry`], the geometry fields of
    /// [`classes_for_page`], both of which have every entry well formed
    /// and sized [`spec_class_size`] (`classes_for_page`'s postcondition;
    /// `adopt_page_geometry` never writes `size`), and it is written only
    /// before the allocator is published.
    #[verifier::external_body]
    #[inline(always)]
    pub fn table(&self) -> (r: &[Class; NCLASSES])
        ensures
            *r == class_table(),
            forall|i: int| 0 <= i < NCLASSES ==> #[trigger] r[i].wf(),
            forall|i: int| 0 <= i < NCLASSES ==> #[trigger] r[i].size == spec_class_size(i),
    {
        // SAFETY: see the `Sync` impl.
        unsafe { &*self.0.get() }
    }
}

} // verus!

// SAFETY: the table is written only by `adopt_page_geometry`, once, by the
// initialising thread before the allocator is published (`api::init`), and
// only the geometry fields are written, which nothing reads before init
// (`size`, the one field pre-init callers such as `nallocx` read, is never
// written). After that it is read-only for the life of the process.
unsafe impl Sync for ClassTable {}

impl Deref for ClassTable {
    type Target = [Class; NCLASSES];

    #[inline(always)]
    fn deref(&self) -> &[Class; NCLASSES] {
        // SAFETY: see the `Sync` impl; no write can be in flight while a
        // reference exists.
        unsafe { &*self.0.get() }
    }
}

verus! {

pub exec static CLASSES: ClassTable = ClassTable::new(BASE_CLASSES);

} // verus!

/// Raise the slab of every class whose table slab is smaller than the
/// kernel page to the page ([`lg_slab_at`]), writing `lg_slab`,
/// `slab_mask`, `nslots` and `slab_bits` of those classes in [`CLASSES`].
/// Returns how many
/// classes changed (0 on a 4 KiB kernel, where the table is untouched).
///
/// # Safety
/// Must be called once, before the allocator is published to any other
/// thread (`api::init` does, right after the page-size probe), and never
/// again: nothing may read a geometry field concurrently.
pub unsafe fn adopt_page_geometry(lg_page: u32) -> usize {
    // SAFETY: the caller guarantees exclusive access during the call.
    unsafe { apply_page_geometry(CLASSES.0.get(), lg_page) }
}

/// [`adopt_page_geometry`] over any table (the tests run it on a copy).
///
/// # Safety
/// `table` must be valid for writes and unaliased for the call.
pub(crate) unsafe fn apply_page_geometry(table: *mut [Class; NCLASSES], lg_page: u32) -> usize {
    let want = classes_for_page(lg_page);
    let mut changed = 0;
    for (c, w) in want.iter().enumerate() {
        // SAFETY: `c < NCLASSES`; the caller guarantees the table is
        // writable and unaliased.
        let cur = unsafe { &raw mut (*table)[c] };
        if unsafe { (*cur).lg_slab } != w.lg_slab {
            unsafe {
                (*cur).lg_slab = w.lg_slab;
                (*cur).nslots = w.nslots;
                (*cur).slab_mask = w.slab_mask;
                (*cur).slab_bits = w.slab_bits;
            }
            changed += 1;
        }
    }
    changed
}

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
        r matches Some(c) ==> forall|j: int| 0 <= j < c ==> #[trigger] spec_class_size(j) < n,
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

/// The `panic!` is unreachable: the last class is `MEDIUM_MAX`, above every
/// entry's request.
const fn build_small_lookup() -> (out: [u8; SMALL_LOOKUP_LEN])
    ensures
        forall|i: int| 0 <= i < SMALL_LOOKUP_LEN ==> (#[trigger] out[i] as int) < NCLASSES
            && i * QUANTUM <= spec_class_size(out[i] as int),
{
    let mut out = [0u8; SMALL_LOOKUP_LEN];
    let mut i = 0;
    while i < SMALL_LOOKUP_LEN
        invariant
            i <= SMALL_LOOKUP_LEN,
            forall|j: int| 0 <= j < i ==> (#[trigger] out[j] as int) < NCLASSES
                && j * QUANTUM <= spec_class_size(out[j] as int),
        decreases SMALL_LOOKUP_LEN - i,
    {
        // Entry `i` serves every request that rounds up to `i` quanta.
        out[i] = match class_of_slow(i * QUANTUM) {
            Some(c) => c as u8,
            None => panic!("small lookup out of range"),
        };
        i += 1;
    }
    out
}

/// Request size rounded up to quanta, then class number (design section 3:
/// "a small table for requests up to 1024").
exec static SMALL_LOOKUP: [u8; SMALL_LOOKUP_LEN]
    ensures
        forall|i: int| 0 <= i < SMALL_LOOKUP_LEN ==> (#[trigger] SMALL_LOOKUP[i] as int) < NCLASSES
            && i * QUANTUM <= spec_class_size(SMALL_LOOKUP[i] as int),
{
    build_small_lookup()
}

/// Class number for a request of `n` bytes, or `None` when `n` is huge.
/// A zero-byte request maps to class 0 so `malloc(0)` yields a real slot.
#[inline]
pub fn class_of(n: usize) -> (r: Option<usize>)
    ensures
        r is Some <==> n <= MEDIUM_MAX,
        r matches Some(c) ==> c < NCLASSES && n <= spec_class_size(c as int),
{
    if n <= SMALL_MAX {
        proof {
            let q = ((n + QUANTUM - 1) as int) / (QUANTUM as int);
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod((n + QUANTUM - 1) as int, QUANTUM as int);
            vstd::arithmetic::div_mod::lemma_mod_bound((n + QUANTUM - 1) as int, QUANTUM as int);
            assert(n <= q * QUANTUM) by (nonlinear_arith)
                requires
                    (n + QUANTUM - 1) as int == QUANTUM * q + ((n + QUANTUM - 1) as int) % (QUANTUM as int),
                    ((n + QUANTUM - 1) as int) % (QUANTUM as int) < QUANTUM;
            assert(q < SMALL_LOOKUP_LEN);
        }
        Some(SMALL_LOOKUP[(n + QUANTUM - 1) / QUANTUM] as usize)
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
        }
        Some(LINEAR_CLASSES + 4 * (lg - LG_LINEAR_MAX) as usize + k)
    } else {
        None
    }
}

/// Slot size for a request, i.e. `nallocx(n, 0)` for non-huge `n`.
#[inline]
#[cfg(test)]
pub fn class_size_of(n: usize) -> Option<usize> {
    class_of(n).map(|c| CLASSES[c].size())
}

} // verus!

const _: () = assert!(MAX_SLAB_WORDS == max_slab_words());

mod regions;

#[cfg(all(verus_only, not(test)))]
pub use regions::LG_REGION_PREFERRED;
pub use regions::{Kind, LG_HUGE_PREFERRED, Regions};
#[cfg(test)]
pub use regions::{
    LG_HUGE_MIN, LG_META_MIN, LG_REGION_MIN, LG_REGION_PREFERRED, MAX_SEALED, Mapping,
};
#[cfg(verus_only)]
pub use regions::{
    bit_slots_in, disjoint, huge_block, lemma_aligned_down, lemma_mod_lin3,
    lemma_pow2_divides_pow2, process_regions, slots_in,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod linux_tests;
