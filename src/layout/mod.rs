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
const LINEAR_CLASSES: usize = 8;
/// `log2(8 * QUANTUM)`: the size where the doubling pattern begins.
const LG_LINEAR_MAX: u32 = (LINEAR_CLASSES * QUANTUM).trailing_zeros();
const LG_MEDIUM_MAX: u32 = MEDIUM_MAX.trailing_zeros();
/// Total number of size classes: 52 for `quantum16`, 56 for `quantum8`.
pub const NCLASSES: usize = LINEAR_CLASSES + 4 * (LG_MEDIUM_MAX - LG_LINEAR_MAX) as usize;

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
pub const NBINS: usize = count_bins();

const fn count_bins() -> usize {
    let mut n = 0;
    while n < NCLASSES && class_size_at(n) <= BIN_MAX {
        n += 1;
    }
    n
}

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
    #[inline]
    pub const fn size(&self) -> usize {
        self.size as usize
    }

    #[inline]
    pub const fn slab_size(&self) -> usize {
        1 << self.lg_slab
    }

    #[inline]
    pub const fn nslots(&self) -> usize {
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
    pub const fn compact_slots(&self) -> usize {
        let unit = 1usize << LG_COMPACT_UNIT;
        let slab = self.slab_size();
        (if slab > unit { slab } else { unit }) / self.size()
    }

    /// Bits one slab occupies in the class bitmap (the `slab_bits` field):
    /// `nslots` rounded up to a power of two below [`LINE_BITS`], or to a
    /// multiple of it from there, so a slab's bits are whole words (64 or
    /// more bits) or one sub-word range (fewer) and a line holds a whole
    /// number of slabs; then raised so that the slabs sharing one line span
    /// at most `1 << LG_GROUP_SPAN` bytes of the region.
    #[inline]
    pub const fn slab_bits(&self) -> usize {
        self.slab_bits as usize
    }

    /// Consecutive slabs whose bits fill one bitmap line: the unit in which
    /// `class/` hands slabs to an arena, so that every line has one owner.
    /// A power of two; 1 when a slab's bits are a line or more.
    #[inline]
    pub const fn slabs_per_line(&self) -> usize {
        let b = self.slab_bits();
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
    #[inline]
    pub const fn slot_of_offset(&self, offset: usize) -> (usize, bool) {
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
    pub const fn slab_mask(&self) -> usize {
        self.slab_mask as usize
    }
}

const fn class_size_at(i: usize) -> usize {
    if i < LINEAR_CLASSES {
        (i + 1) * QUANTUM
    } else {
        let g = (i - LINEAR_CLASSES) / 4;
        let k = (i - LINEAR_CLASSES) % 4;
        let base = (LINEAR_CLASSES * QUANTUM) << g;
        base + (k + 1) * (base / 4)
    }
}

/// Slab size of a class (see [`LG_MIN_SLAB`]): `1 << LG_SMALL_SLAB` below
/// [`SMALL_SLAB_BELOW`]; from there to [`SMALL_MAX`] the smallest power of
/// two from [`MIN_SLAB`] up whose internal waste is at most
/// `slab / SLAB_WASTE_DIV`; above, `max(1 << LG_MEDIUM_MIN_SLAB, 16 * size)`
/// rounded up to a power of two.
pub const fn lg_slab_for(size: usize) -> u32 {
    if size < SMALL_SLAB_BELOW {
        return LG_SMALL_SLAB;
    }
    if size <= SMALL_MAX {
        let mut lg = LG_MIN_SLAB;
        while lg < LG_SMALL_SLAB {
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
    if lg < LG_MEDIUM_MIN_SLAB {
        LG_MEDIUM_MIN_SLAB
    } else {
        lg
    }
}

const _: () = assert!(LG_SMALL_SLAB >= LG_MIN_SLAB && LG_SMALL_SLAB <= LG_MAX_SLAB);
const _: () = assert!(LG_MEDIUM_MIN_SLAB >= LG_MIN_SLAB && LG_MEDIUM_MIN_SLAB <= LG_MAX_SLAB);
const _: () = assert!(
    SMALL_SLAB_BELOW <= SMALL_MAX && MIN_SLAB >= 4 * SMALL_MAX,
    "a small-slab class has at least four slots"
);
const _: () = assert!(LG_COMPACT_UNIT >= LG_MIN_SLAB && LG_COMPACT_UNIT <= LG_MAX_SLAB);

/// The inverse of the odd part of `size` modulo `2^32` (the `inv_odd`
/// field): Newton's iteration `x = x * (2 - d * x)` doubles the correct
/// low bits from the 3 that `x = d` has for odd `d`.
const fn inverse_of_odd_part(size: usize) -> u32 {
    let d = (size as u32) >> size.trailing_zeros();
    let mut x = d;
    let mut i = 0;
    while i < 4 {
        x = x.wrapping_mul(2u32.wrapping_sub(d.wrapping_mul(x)));
        i += 1;
    }
    x
}

/// See [`Class::slab_bits`].
const fn slab_bits_for(nslots: usize, lg_slab: u32) -> usize {
    let raw = if nslots <= LINE_BITS {
        nslots.next_power_of_two()
    } else {
        (nslots + LINE_BITS - 1) & !(LINE_BITS - 1)
    };
    let per_line = if raw >= LINE_BITS { 1 } else { LINE_BITS / raw };
    let cap = if LG_GROUP_SPAN > lg_slab {
        1usize << (LG_GROUP_SPAN - lg_slab)
    } else {
        1
    };
    if per_line > cap { LINE_BITS / cap } else { raw }
}

/// Slab size of a class on a kernel with `1 << lg_page` pages: the
/// compile-time [`lg_slab_for`] value, raised to the page when the page is
/// larger, but never above [`LG_SMALL_SLAB`] (every table slab below
/// 64 KiB belongs to the 512..1024-byte classes, so the rule is exactly
/// "no slab smaller than the page" on every supported page size, 4, 16 and
/// 64 KiB; a page above 64 KiB, which no supported kernel has, keeps the
/// table geometry and the page groups of `class/`).
pub const fn lg_slab_at(size: usize, lg_page: u32) -> u32 {
    let lg = lg_slab_for(size);
    let floor = if lg_page < LG_SMALL_SLAB {
        lg_page
    } else {
        LG_SMALL_SLAB
    };
    if lg < floor { floor } else { lg }
}

/// The class table for a kernel with `1 << lg_page` pages: sizes and
/// their inverses are fixed, the slab geometry follows [`lg_slab_at`].
pub const fn classes_for_page(lg_page: u32) -> [Class; NCLASSES] {
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
    while i < NCLASSES {
        let size = class_size_at(i);
        let lg_slab = lg_slab_at(size, lg_page);
        let nslots = (1usize << lg_slab) / size;
        out[i] = Class {
            size: size as u32,
            lg_slab: lg_slab as u8,
            nslots: nslots as u32,
            slab_mask: ((1usize << lg_slab) - 1) as u32,
            inv_odd: inverse_of_odd_part(size),
            tz: size.trailing_zeros() as u8,
            slab_bits: slab_bits_for(nslots, lg_slab) as u16,
        };
        i += 1;
    }
    out
}

const fn build_classes() -> [Class; NCLASSES] {
    classes_for_page(LG_MIN_SLAB)
}

/// The compile-time class table: the geometry of a kernel whose page is at
/// most [`MIN_SLAB`] (4 KiB, the reference hosts), and the reference the
/// tests and the compile-time constants ([`NBINS`], [`MAX_SLAB_WORDS`],
/// `slab::DESC_STRIDE`) are built from.
pub const BASE_CLASSES: [Class; NCLASSES] = build_classes();

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
pub struct ClassTable(UnsafeCell<[Class; NCLASSES]>);

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

pub static CLASSES: ClassTable = ClassTable(UnsafeCell::new(BASE_CLASSES));

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

/// Classes whose slab in [`CLASSES`] is not the compile-time one (raised
/// to the page by [`adopt_page_geometry`]), as the inclusive range of
/// class numbers, or `None` when the geometry is the table's. For the
/// report (`malloc_stats_print`) and the tests.
pub fn page_sized_classes() -> Option<(usize, usize)> {
    let mut lo = None;
    let mut hi = 0;
    for c in 0..NCLASSES {
        if CLASSES[c].lg_slab != BASE_CLASSES[c].lg_slab {
            if lo.is_none() {
                lo = Some(c);
            }
            hi = c;
        }
    }
    lo.map(|lo| (lo, hi))
}

/// Bitmap words one slab occupies at most over all classes (`slab_bits /
/// 64`, at least 1): the size of the scratch bitmap a release batch
/// accumulates a slab's clears in (`ClassState::release_owned`), 128 words
/// (1 KiB) with 8-byte slots in a 64 KiB slab.
pub const MAX_SLAB_WORDS: usize = max_slab_words();

const fn max_slab_words() -> usize {
    let classes = build_classes();
    let mut m = 1;
    let mut i = 0;
    while i < NCLASSES {
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
const fn class_of_slow(n: usize) -> Option<usize> {
    let mut i = 0;
    while i < NCLASSES {
        if BASE_CLASSES[i].size as usize >= n {
            return Some(i);
        }
        i += 1;
    }
    None
}

const SMALL_LOOKUP_LEN: usize = SMALL_MAX / QUANTUM + 1;

const fn build_small_lookup() -> [u8; SMALL_LOOKUP_LEN] {
    let mut out = [0u8; SMALL_LOOKUP_LEN];
    let mut i = 0;
    while i < SMALL_LOOKUP_LEN {
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
static SMALL_LOOKUP: [u8; SMALL_LOOKUP_LEN] = build_small_lookup();

/// Class number for a request of `n` bytes, or `None` when `n` is huge.
/// A zero-byte request maps to class 0 so `malloc(0)` yields a real slot.
#[inline]
pub fn class_of(n: usize) -> Option<usize> {
    if n <= SMALL_MAX {
        Some(SMALL_LOOKUP[(n + QUANTUM - 1) / QUANTUM] as usize)
    } else if n <= MEDIUM_MAX {
        // Classes in (2^lg, 2^(lg+1)] are 2^lg + k * 2^(lg-2) for k = 1..4.
        let m = n - 1;
        let lg = usize::BITS - 1 - m.leading_zeros();
        let k = (m >> (lg - 2)) & 3;
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

mod regions;

pub use regions::{Kind, LG_HUGE_PREFERRED, Regions};
#[cfg(test)]
pub use regions::{
    LG_HUGE_MIN, LG_META_MIN, LG_REGION_MIN, LG_REGION_PREFERRED, MAX_SEALED, Mapping,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod linux_tests;
