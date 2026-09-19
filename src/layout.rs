//! Size classes, address-space regions, and pointer classification
//! (design.md sections 2 and 3).
//!
//! Everything in this module except [`Regions::reserve`] is pure arithmetic
//! over a class table fixed at init ([`CLASSES`]: the compile-time
//! [`BASE_CLASSES`] geometry, with the slabs of the classes below the
//! kernel page raised to the page by [`adopt_page_geometry`]) and a
//! handful of runtime bases, and is unit tested exhaustively on the host. `reserve` is the Linux-only piece
//! that probes how much address space the process can get and lays the
//! regions out.
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
/// Why these classes and this size (measured in
/// `bench/results/2026-09-17-aarch64-gce-compacting-cache-soak3.md` and
/// `2026-09-17-*-gce-slab-geometry.md`): the post-delete residue of the
/// Valkey soak is live values the load never touches again, one to fifteen
/// per slab, each pinning its whole slab, and it sits entirely in the
/// 512..1024-byte classes; at 64 KiB a pinned 1 KiB value kept 64 KiB
/// resident where jemalloc's 4-region slab keeps 4 KiB. The classes below
/// 512 bytes keep 64 KiB slabs: a 16 KiB slab for every class halved
/// `xmalloc-test` (64-byte objects, cross-thread frees) through per-slab
/// refill and flush work (`bench/results/2026-09-16-aarch64-gce-min-slab.md`),
/// and a class with 64 or more regions per slab never empties by chance
/// after a random deletion either way (0.89^64 = 0.06%). A slab smaller
/// than the kernel page never exists on a supported kernel: at init the
/// classes whose table slab is below the page take the page as their slab
/// size, up to [`LG_SMALL_SLAB`] ([`adopt_page_geometry`], plan Phase 2.5
/// task 4e: 64 KiB slabs of 128, 102, 85, 73 and 64 slots for the five
/// classes on a 64 KiB kernel, 16 KiB slabs on a 16 KiB one), so that every
/// `Empty` slab is one purge unit; with sub-page slabs the Valkey soak's
/// post-delete RSS at 64 KiB pages was 79.5 MiB of `Empty` 4..16 KiB slabs
/// in mixed page groups that never became idle
/// (`bench/results/2026-09-19-aarch64-64k-valkey-soak.md`). A slab smaller
/// than the page (a faked page in the tests, or a page above 64 KiB) is
/// committed, guarded and purged with its whole page of slabs by `class.rs`
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
/// The compaction thresholds of `heap.rs` are counted in slabs of this
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
/// Fixed-point shift of the per-class reciprocal (design section 3).
pub const RECIP_SHIFT: u32 = 32;

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

const _: () = assert!(NCLASSES <= 64, "pointer classification assumes at most 64 classes");
const _: () = assert!(MEDIUM_MAX * 16 == MAX_SLAB);

/// Allocation tier of a class (design section 3 table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Small,
    Medium,
}

/// One size class. `size` and `recip` are compile-time constants; the slab
/// geometry (`lg_slab`, `nslots`, `slab_bits`) is the compile-time
/// [`BASE_CLASSES`] value unless [`adopt_page_geometry`] raised the slab
/// to the kernel page at init.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Class {
    /// Slot size in bytes; also the usable size of every allocation in it.
    pub size: u32,
    /// `log2(slab size)`.
    pub lg_slab: u8,
    /// Slots per slab, `slab_size / size` rounded down.
    pub nslots: u32,
    /// `ceil(2^32 / size)`. `(offset * recip) >> 32` equals `offset / size`
    /// whenever `offset` is a multiple of `size` below the slab size, which
    /// is all the `free` path needs (a misaligned offset can never satisfy
    /// `slot * size == offset` for any `slot`).
    pub recip: u32,
    /// Bits one slab occupies in the class's slot bitmap (design section
    /// 4): `nslots` padded so that a slab's bits are either whole words or
    /// one range inside a word, and so that a bitmap cache line is shared
    /// by at most [`LG_GROUP_SPAN`] bytes of slabs. See [`Class::slab_bits`].
    pub slab_bits: u16,
}

/// Cache line the slot bitmaps are laid out for, in bits (64 bytes).
pub const LINE_BITS: usize = 512;

/// Slabs are handed to an arena in groups that own whole bitmap lines
/// (`class.rs`, design section 4); a group spans at most this much of the
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

    #[inline]
    pub const fn tier(&self) -> Tier {
        if self.size as usize <= SMALL_MAX { Tier::Small } else { Tier::Medium }
    }

    /// Slots per [`LG_COMPACT_UNIT`] bytes of slabs (per slab when the slab
    /// is larger): the unit of the compaction thresholds in `heap.rs`, equal
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
    /// `class.rs` hands slabs to an arena, so that every line has one owner.
    /// A power of two; 1 when a slab's bits are a line or more.
    #[inline]
    pub const fn slabs_per_line(&self) -> usize {
        let b = self.slab_bits();
        if b >= LINE_BITS { 1 } else { LINE_BITS / b }
    }

    /// `u64` words a slab's bits touch in the class bitmap: `slab_bits / 64`
    /// for whole-word slabs, 1 for a sub-word slab.
    #[inline]
    pub const fn bitmap_words(&self) -> usize {
        self.slab_bits().div_ceil(64)
    }

    /// Slot index for an in-slab offset, plus whether the offset is exactly a
    /// slot start. The index is only meaningful when `valid` is true.
    #[inline]
    pub const fn slot_of_offset(&self, offset: usize) -> (usize, bool) {
        let slot = ((offset as u64 * self.recip as u64) >> RECIP_SHIFT) as usize;
        let valid = slot < self.nslots as usize && slot * self.size as usize == offset;
        (slot, valid)
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
    if lg < LG_MEDIUM_MIN_SLAB { LG_MEDIUM_MIN_SLAB } else { lg }
}

const _: () = assert!(LG_SMALL_SLAB >= LG_MIN_SLAB && LG_SMALL_SLAB <= LG_MAX_SLAB);
const _: () = assert!(LG_MEDIUM_MIN_SLAB >= LG_MIN_SLAB && LG_MEDIUM_MIN_SLAB <= LG_MAX_SLAB);
const _: () = assert!(SMALL_SLAB_BELOW <= SMALL_MAX && MIN_SLAB >= 4 * SMALL_MAX, "a small-slab class has at least four slots");
const _: () = assert!(LG_COMPACT_UNIT >= LG_MIN_SLAB && LG_COMPACT_UNIT <= LG_MAX_SLAB);

const fn reciprocal(size: usize) -> u32 {
    (((1u64 << RECIP_SHIFT) + size as u64 - 1) / size as u64) as u32
}

/// See [`Class::slab_bits`].
const fn slab_bits_for(nslots: usize, lg_slab: u32) -> usize {
    let raw = if nslots <= LINE_BITS { nslots.next_power_of_two() } else { (nslots + LINE_BITS - 1) & !(LINE_BITS - 1) };
    let per_line = if raw >= LINE_BITS { 1 } else { LINE_BITS / raw };
    let cap = if LG_GROUP_SPAN > lg_slab { 1usize << (LG_GROUP_SPAN - lg_slab) } else { 1 };
    if per_line > cap { LINE_BITS / cap } else { raw }
}

/// Slab size of a class on a kernel with `1 << lg_page` pages: the
/// compile-time [`lg_slab_for`] value, raised to the page when the page is
/// larger, but never above [`LG_SMALL_SLAB`] (every table slab below
/// 64 KiB belongs to the 512..1024-byte classes, so the rule is exactly
/// "no slab smaller than the page" on every supported page size, 4, 16 and
/// 64 KiB; a page above 64 KiB, which no supported kernel has, keeps the
/// table geometry and the page groups of `class.rs`).
pub const fn lg_slab_at(size: usize, lg_page: u32) -> u32 {
    let lg = lg_slab_for(size);
    let floor = if lg_page < LG_SMALL_SLAB { lg_page } else { LG_SMALL_SLAB };
    if lg < floor { floor } else { lg }
}

/// The class table for a kernel with `1 << lg_page` pages: sizes and
/// reciprocals are fixed, the slab geometry follows [`lg_slab_at`].
pub const fn classes_for_page(lg_page: u32) -> [Class; NCLASSES] {
    let mut out = [Class { size: 0, lg_slab: 0, nslots: 0, recip: 0, slab_bits: 0 }; NCLASSES];
    let mut i = 0;
    while i < NCLASSES {
        let size = class_size_at(i);
        let lg_slab = lg_slab_at(size, lg_page);
        let nslots = (1usize << lg_slab) / size;
        out[i] = Class {
            size: size as u32,
            lg_slab: lg_slab as u8,
            nslots: nslots as u32,
            recip: reciprocal(size),
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
/// the page by [`adopt_page_geometry`] once at init (plan Phase 2.5 task
/// 4e). It is a table at a fixed address, not a pointer selected at init
/// and not a runtime shift where `lg_slab` is read, so the `free` and
/// `sdallocx` fast paths (`Regions::slab_slot`, `Regions::slot_in_class`)
/// load a class's geometry exactly as they did from the compile-time
/// table: one indexed load per field from a link-time constant address,
/// nothing dependent added. The price is that the table lives in
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
/// kernel page to the page ([`lg_slab_at`]), writing `lg_slab`, `nslots`
/// and `slab_bits` of those classes in [`CLASSES`]. Returns how many
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
pub fn class_size_of(n: usize) -> Option<usize> {
    class_of(n).map(|c| CLASSES[c].size())
}

/// Address-space parameters (design section 2 table), as `log2`.
pub const LG_REGION_PREFERRED: u32 = 38;
pub const LG_REGION_MIN: u32 = 32;
pub const LG_HUGE_PREFERRED: u32 = 40;
pub const LG_HUGE_MIN: u32 = 34;
pub const LG_META_PREFERRED: u32 = 36;
pub const LG_META_MIN: u32 = 30;
/// Window within which the huge and metadata bases are slid at random, on
/// top of the kernel's own mmap randomisation.
pub const LG_SLIDE: u32 = 30;

/// One `mmap` reservation as handed back by the kernel, before alignment and
/// sliding. Kept so the never-committed slack around each region can be
/// sealed ([`Regions::never_committed`]) and the whole range accounted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub addr: usize,
    pub len: usize,
}

/// Upper bound on the ranges [`Regions::never_committed`] returns, and so
/// on the `mseal` calls init makes: one before class 0, one after each
/// class, two per side mapping.
pub const MAX_SEALED: usize = NCLASSES + 1 + 4;

/// What a pointer is, according to the regions alone. Slab-tier results
/// still need the slab bitmap consulted to tell live from free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Exact slot start inside a class region.
    Slab { class: u32, slab: u32, slot: u32 },
    /// Page-aligned address inside the huge region; `page` is the index from
    /// the region base. Whether it starts a run is the huge allocator's call.
    Huge { page: usize },
    /// Outside every region, in a gap, or not on a slot/page boundary.
    Invalid,
}

/// The reserved regions of one process. `repr(C)` keeps `superblock`,
/// `lg_region` and `lg_stride` at the front, on the cache line the `free`
/// fast path reads (see `Shared` in `heap.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Regions {
    /// `MAX_SLAB`-aligned start of the class superblock.
    pub superblock: usize,
    pub lg_region: u32,
    pub lg_stride: u32,
    /// Start of each class region, `MAX_SLAB`-aligned, inside its stride.
    pub class_base: [usize; NCLASSES],
    pub huge_base: usize,
    pub huge_len: usize,
    pub meta_base: usize,
    pub meta_len: usize,
    pub lg_page: u32,
    pub superblock_map: Mapping,
    pub huge_map: Mapping,
    pub meta_map: Mapping,
}

impl Regions {
    /// Assemble regions from already chosen bases. `offsets[c]` is the
    /// distance of class `c`'s region from the start of its stride; every
    /// offset must be `MAX_SLAB`-aligned and below the gap, and `superblock`
    /// must be `MAX_SLAB`-aligned.
    pub const fn from_parts(
        superblock: usize,
        lg_region: u32,
        lg_stride: u32,
        offsets: &[usize; NCLASSES],
        huge: (usize, usize),
        meta: (usize, usize),
        lg_page: u32,
        maps: [Mapping; 3],
    ) -> Regions {
        assert!(lg_stride > lg_region && lg_region >= LG_MAX_SLAB);
        assert!(superblock & (MAX_SLAB - 1) == 0);
        let gap = (1usize << lg_stride) - (1usize << lg_region);
        let mut class_base = [0usize; NCLASSES];
        let mut c = 0;
        while c < NCLASSES {
            assert!(offsets[c] & (MAX_SLAB - 1) == 0 && offsets[c] < gap);
            class_base[c] = superblock + (c << lg_stride) + offsets[c];
            c += 1;
        }
        Regions {
            superblock,
            lg_region,
            lg_stride,
            class_base,
            huge_base: huge.0,
            huge_len: huge.1,
            meta_base: meta.0,
            meta_len: meta.1,
            lg_page,
            superblock_map: maps[0],
            huge_map: maps[1],
            meta_map: maps[2],
        }
    }

    #[inline]
    pub const fn region_len(&self) -> usize {
        1 << self.lg_region
    }

    #[inline]
    pub const fn stride(&self) -> usize {
        1 << self.lg_stride
    }

    #[inline]
    pub const fn gap(&self) -> usize {
        self.stride() - self.region_len()
    }

    #[inline]
    pub const fn page_size(&self) -> usize {
        1 << self.lg_page
    }

    /// Slabs that fit in a class region.
    #[inline]
    pub fn nslabs(&self, class: usize) -> usize {
        self.region_len() >> CLASSES[class].lg_slab
    }

    /// Pages in the huge region.
    #[inline]
    pub const fn huge_pages(&self) -> usize {
        self.huge_len >> self.lg_page
    }

    #[inline]
    pub fn slab_addr(&self, class: usize, slab: usize) -> usize {
        self.class_base[class] + (slab << CLASSES[class].lg_slab)
    }

    #[inline]
    pub fn slot_addr(&self, class: usize, slab: usize, slot: usize) -> usize {
        self.slab_addr(class, slab) + slot * CLASSES[class].size as usize
    }

    #[inline]
    pub const fn huge_addr(&self, page: usize) -> usize {
        self.huge_base + (page << self.lg_page)
    }

    /// The parts of the three mappings that no code ever commits, purges
    /// or reprotects, in address order per mapping: the slack before class
    /// 0, the gap between each class region and the next (one contiguous
    /// range: the tail of stride `c` plus the head of stride `c + 1`), the
    /// slack after the last class, and the alignment-plus-slide slack on
    /// both sides of the huge and metadata regions. Empty ranges are
    /// omitted. These are what `mseal` seals at init (design 10 step 5,
    /// section 14): the class regions, the huge region and the metadata
    /// region themselves are committed by `mprotect` and purged by
    /// `madvise` for the life of the process, so they cannot be sealed.
    ///
    /// Returns the ranges and how many are filled in. At most
    /// [`MAX_SEALED`]: `NCLASSES + 1` around the classes and two per side
    /// mapping.
    pub fn never_committed(&self) -> ([Mapping; MAX_SEALED], usize) {
        let mut out = [Mapping { addr: 0, len: 0 }; MAX_SEALED];
        let mut n = 0;
        let mut push = |lo: usize, hi: usize| {
            if hi > lo {
                out[n] = Mapping { addr: lo, len: hi - lo };
                n += 1;
            }
        };
        let mut start = self.superblock_map.addr;
        for c in 0..NCLASSES {
            push(start, self.class_base[c]);
            start = self.class_base[c] + self.region_len();
        }
        push(start, self.superblock_map.addr + self.superblock_map.len);
        for (m, base, len) in [
            (self.huge_map, self.huge_base, self.huge_len),
            (self.meta_map, self.meta_base, self.meta_len),
        ] {
            push(m.addr, base);
            push(base + len, m.addr + m.len);
        }
        (out, n)
    }

    /// Slab-slot lookup of design section 2, the whole of the `free` fast
    /// path's classification: `Some((class, slab, slot))` when `p` is
    /// exactly a slot start inside a class region, `None` for anything else
    /// (a gap, a mid-slot address, the huge or metadata region, or outside
    /// every reservation). Two subtractions and a shift find the class; a
    /// multiply and compare validate the slot. Never aborts: the caller's
    /// slow path decides what `None` means.
    #[inline(always)]
    pub fn slab_slot(&self, p: usize) -> Option<(usize, usize, usize)> {
        let c = p.wrapping_sub(self.superblock) >> self.lg_stride;
        if c >= NCLASSES {
            return None;
        }
        // SAFETY: `c < NCLASSES` was just checked; both arrays have
        // `NCLASSES` entries.
        let cls = unsafe { CLASSES.get_unchecked(c) };
        let rel = p.wrapping_sub(unsafe { *self.class_base.get_unchecked(c) });
        if rel >= self.region_len() {
            return None;
        }
        let slab = rel >> cls.lg_slab;
        let offset = rel & (cls.slab_size() - 1);
        let (slot, valid) = cls.slot_of_offset(offset);
        if !valid {
            return None;
        }
        Some((c, slab, slot))
    }

    /// The sized free's classification (design 6.3, plan Phase 2.5 task
    /// 4c): the size hint names `class`, so the region is known and the
    /// pointer is only checked against it, `Some((slab, slot))` when `p` is
    /// exactly a slot start inside class `class`'s region, `None` otherwise
    /// (outside the region, which covers every other class's region, the
    /// gaps, the huge and metadata regions; or a mid-slot address). One
    /// subtraction and a compare replace [`slab_slot`]'s superblock
    /// subtraction, stride shift, bound check and class compare. `class`
    /// must be below `NCLASSES` (what `class_of`/`class_for` return).
    ///
    /// [`slab_slot`]: Regions::slab_slot
    #[inline(always)]
    pub fn slot_in_class(&self, class: usize, p: usize) -> Option<(usize, usize)> {
        debug_assert!(class < NCLASSES);
        // SAFETY: the caller's `class` is an index `class_of` produced.
        let cls = unsafe { CLASSES.get_unchecked(class) };
        let rel = p.wrapping_sub(unsafe { *self.class_base.get_unchecked(class) });
        if rel >= self.region_len() {
            return None;
        }
        let slab = rel >> cls.lg_slab;
        let (slot, valid) = cls.slot_of_offset(rel & (cls.slab_size() - 1));
        if !valid {
            return None;
        }
        Some((slab, slot))
    }

    /// Whether `p` falls in the class superblock (a class region or a gap).
    #[inline]
    fn in_superblock(&self, p: usize) -> bool {
        p.wrapping_sub(self.superblock) >> self.lg_stride < NCLASSES
    }

    /// Classify a pointer (design section 2): [`slab_slot`] for the class
    /// regions, then the huge region.
    ///
    /// [`slab_slot`]: Regions::slab_slot
    #[inline]
    pub fn classify(&self, p: usize) -> Kind {
        if let Some((class, slab, slot)) = self.slab_slot(p) {
            return Kind::Slab { class: class as u32, slab: slab as u32, slot: slot as u32 };
        }
        if self.in_superblock(p) {
            return Kind::Invalid;
        }
        let rel = p.wrapping_sub(self.huge_base);
        if rel < self.huge_len && rel & (self.page_size() - 1) == 0 {
            return Kind::Huge { page: rel >> self.lg_page };
        }
        Kind::Invalid
    }

    /// True when `p` is anywhere inside the metadata reservation.
    #[inline]
    pub fn in_meta(&self, p: usize) -> bool {
        p.wrapping_sub(self.meta_base) < self.meta_len
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{
        LG_HUGE_MIN, LG_HUGE_PREFERRED, LG_MAX_SLAB, LG_META_MIN, LG_META_PREFERRED,
        LG_REGION_MIN, LG_REGION_PREFERRED, LG_SLIDE, MAX_SLAB, Mapping, NCLASSES, Regions,
    };
    use crate::sys::{self, Errno, MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};

    /// Reserve address space with `PROT_NONE`, `MAP_NORESERVE`.
    fn try_reserve(len: usize) -> Result<usize, Errno> {
        // SAFETY: fresh anonymous private mapping, no fixed address.
        unsafe { sys::mmap(0, len, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0) }
    }

    /// Reserve `2^lg + extra` bytes, halving `lg` down to `lg_min` on any
    /// failure. Returns the mapping and the `lg` that fit.
    fn reserve_shrinking(what: &str, mut lg: u32, lg_min: u32, extra: usize) -> (Mapping, u32) {
        loop {
            let len = (1usize << lg) + extra;
            match try_reserve(len) {
                Ok(addr) => return (Mapping { addr, len }, lg),
                Err(e) if lg > lg_min => {
                    let _ = e;
                    lg -= 1;
                }
                Err(e) => sys::abort_fmt(format_args!(
                    "cannot reserve {what} region of 2^{lg} bytes: {e}"
                )),
            }
        }
    }

    /// Reserve the class superblock: `NCLASSES` strides plus alignment slack.
    /// Prefers a stride of 4x the region (gap 3x). Shrinks the region down to
    /// the minimum, then as a last resort drops the stride to 2x the region
    /// (gap 1x, below the design's 1.25x preference but still a full gap).
    fn reserve_superblock() -> (Mapping, u32, u32) {
        let mut lg_region = LG_REGION_PREFERRED;
        let mut lg_stride = lg_region + 2;
        loop {
            let len = (NCLASSES << lg_stride) + MAX_SLAB;
            match try_reserve(len) {
                Ok(addr) => return (Mapping { addr, len }, lg_region, lg_stride),
                Err(_) if lg_region > LG_REGION_MIN => {
                    lg_region -= 1;
                    lg_stride = lg_region + 2;
                }
                Err(_) if lg_stride == lg_region + 2 => lg_stride = lg_region + 1,
                Err(e) => sys::abort_fmt(format_args!(
                    "cannot reserve class superblock of {NCLASSES} x 2^{lg_stride} bytes: {e}"
                )),
            }
        }
    }

    const SLIDE_STEPS: usize = (1usize << LG_SLIDE) >> LG_MAX_SLAB;

    fn name_mapping(m: Mapping, name: &'static [u8]) {
        // SAFETY: `m` is a live mapping we own; the name is NUL-terminated.
        let _ = unsafe { sys::set_vma_anon_name(m.addr, m.len, name) };
    }

    impl Regions {
        /// Probe and reserve every region (design section 10 step 2). Never
        /// allocates. Aborts if even the minimum layout cannot be reserved.
        /// `anon_name` should be the probed `PR_SET_VMA_ANON_NAME` support.
        pub fn reserve(page_size: usize, anon_name: bool) -> Regions {
            assert!(page_size.is_power_of_two() && page_size <= MAX_SLAB);
            let lg_page = page_size.trailing_zeros();

            // One 64-bit random per class offset, one each for the two
            // slides, one for the reservation order.
            let mut rnd = [0u8; 8 * (NCLASSES + 3)];
            if let Err(e) = sys::getrandom_exact(&mut rnd) {
                sys::abort_fmt(format_args!("getrandom failed: {e}"));
            }
            let word = |i: usize| -> u64 {
                let mut b = [0u8; 8];
                b.copy_from_slice(&rnd[8 * i..8 * i + 8]);
                u64::from_ne_bytes(b)
            };

            // Reserve the three mappings in a random order so their relative
            // placement is not fixed across processes.
            let order: [u8; 3] = match word(NCLASSES + 2) % 6 {
                0 => [0, 1, 2],
                1 => [0, 2, 1],
                2 => [1, 0, 2],
                3 => [1, 2, 0],
                4 => [2, 0, 1],
                _ => [2, 1, 0],
            };
            let slide = 1usize << LG_SLIDE;
            let mut sb = None;
            let mut huge = None;
            let mut meta = None;
            for role in order {
                match role {
                    0 => sb = Some(reserve_superblock()),
                    1 => huge = Some(reserve_shrinking("huge", LG_HUGE_PREFERRED, LG_HUGE_MIN, slide + MAX_SLAB)),
                    _ => meta = Some(reserve_shrinking("metadata", LG_META_PREFERRED, LG_META_MIN, slide + MAX_SLAB)),
                }
            }
            let (sb_map, lg_region, lg_stride) = sb.unwrap();
            let (huge_map, lg_huge) = huge.unwrap();
            let (meta_map, lg_meta) = meta.unwrap();

            let superblock = (sb_map.addr + MAX_SLAB - 1) & !(MAX_SLAB - 1);
            let gap = (1usize << lg_stride) - (1usize << lg_region);
            let mut offsets = [0usize; NCLASSES];
            for (c, off) in offsets.iter_mut().enumerate() {
                *off = (word(c) as usize % (gap >> LG_MAX_SLAB)) << LG_MAX_SLAB;
            }
            // `mmap` only promises page alignment, so each mapping carries
            // `MAX_SLAB` of slack and its base is aligned up before the slide.
            let align_base = |m: Mapping| (m.addr + MAX_SLAB - 1) & !(MAX_SLAB - 1);
            let huge_base = align_base(huge_map) + ((word(NCLASSES) as usize % SLIDE_STEPS) << LG_MAX_SLAB);
            let meta_base = align_base(meta_map) + ((word(NCLASSES + 1) as usize % SLIDE_STEPS) << LG_MAX_SLAB);

            if anon_name {
                name_mapping(sb_map, b"challoc:classes\0");
                name_mapping(huge_map, b"challoc:huge\0");
                name_mapping(meta_map, b"challoc:meta\0");
            }

            Regions::from_parts(
                superblock,
                lg_region,
                lg_stride,
                &offsets,
                (huge_base, 1usize << lg_huge),
                (meta_base, 1usize << lg_meta),
                lg_page,
                [sb_map, huge_map, meta_map],
            )
        }

        /// Seal every range [`Regions::never_committed`] lists (design 10
        /// step 5): `mseal` forbids `munmap`, `mremap`, `mprotect` and
        /// `MAP_FIXED` over them for the rest of the process, so nothing can
        /// land a mapping in the gap between two class regions or unmap
        /// part of a reservation. Only for kernels whose probe said
        /// `mseal=yes`; each call splits the enclosing VMA, so the VMA count
        /// grows by at most `2 * MAX_SEALED`. Returns the number of ranges
        /// sealed. A refusal after a successful probe (`ENOMEM` at the VMA
        /// limit, for instance) stops the sweep and leaves the rest
        /// unsealed rather than aborting: sealing is a hardening extra, not
        /// a heap invariant, and the count is reported by
        /// `malloc_stats_print`.
        pub fn seal(&self) -> u32 {
            let (ranges, n) = self.never_committed();
            let mut sealed = 0;
            for m in &ranges[..n] {
                // SAFETY: the range is inside a reservation this process
                // owns and, by construction, is never committed, purged,
                // reprotected or unmapped (`release` is test-only and is
                // never called on sealed regions).
                if unsafe { sys::mseal(m.addr, m.len, 0) }.is_err() {
                    break;
                }
                sealed += 1;
            }
            sealed
        }

        /// Give the reservations back. The allocator never does this; it
        /// exists so tests can reserve repeatedly in one process. Must not
        /// be called after [`seal`](Regions::seal): the kernel refuses and
        /// this aborts.
        ///
        /// # Safety
        /// Nothing may reference the regions afterwards.
        pub unsafe fn release(&self) {
            for m in [self.superblock_map, self.huge_map, self.meta_map] {
                // SAFETY: forwarded from the caller.
                if let Err(e) = unsafe { sys::munmap(m.addr, m.len) } {
                    sys::abort_fmt(format_args!("munmap of reservation failed: {e}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    /// Deterministic xorshift for synthetic offsets.
    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    fn synthetic(lg_region: u32, lg_stride: u32) -> Regions {
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let gap = (1usize << lg_stride) - (1usize << lg_region);
        let mut offsets = [0usize; NCLASSES];
        for o in offsets.iter_mut() {
            *o = (xorshift(&mut s) as usize % (gap >> LG_MAX_SLAB)) << LG_MAX_SLAB;
        }
        let superblock = 0x1000_0000_0000usize;
        let sb_len = NCLASSES << lg_stride;
        let huge = (superblock + sb_len + (1 << 30), 1usize << 34);
        let meta = (huge.0 + huge.1 + (1 << 30), 1usize << 30);
        Regions::from_parts(
            superblock,
            lg_region,
            lg_stride,
            &offsets,
            huge,
            meta,
            12,
            [
                Mapping { addr: superblock, len: sb_len },
                Mapping { addr: huge.0, len: huge.1 },
                Mapping { addr: meta.0, len: meta.1 },
            ],
        )
    }

    /// The never-committed ranges are page aligned, disjoint from every
    /// region, and together with the regions cover the three mappings
    /// exactly; there are at most `MAX_SEALED` of them.
    #[test]
    fn never_committed_is_the_complement_of_the_regions() {
        for (lg_region, lg_stride) in [(38, 40), (32, 34), (32, 33)] {
            let r = synthetic(lg_region, lg_stride);
            let (ranges, n) = r.never_committed();
            assert!(n <= MAX_SEALED && n >= NCLASSES, "{n} ranges");
            // Regions, in address order, with the mapping each belongs to.
            let mut regions: Vec<(usize, usize)> =
                (0..NCLASSES).map(|c| (r.class_base[c], r.class_base[c] + r.region_len())).collect();
            regions.push((r.huge_base, r.huge_base + r.huge_len));
            regions.push((r.meta_base, r.meta_base + r.meta_len));
            let page = r.page_size();
            let mut covered = 0usize;
            for m in &ranges[..n] {
                assert!(m.len > 0 && m.addr % page == 0 && m.len % page == 0, "{m:x?}");
                let (lo, hi) = (m.addr, m.addr + m.len);
                for &(a, b) in &regions {
                    assert!(hi <= a || lo >= b, "range {m:x?} overlaps region {a:#x}-{b:#x}");
                }
                assert!(
                    [r.superblock_map, r.huge_map, r.meta_map]
                        .iter()
                        .any(|map| map.addr <= lo && hi <= map.addr + map.len),
                    "range {m:x?} outside every mapping"
                );
                covered += m.len;
            }
            for &(a, b) in &regions {
                covered += b - a;
            }
            let total = r.superblock_map.len + r.huge_map.len + r.meta_map.len;
            assert_eq!(covered, total, "ranges and regions do not tile the mappings");
        }
    }

    #[test]
    fn class_count_matches_quantum() {
        if QUANTUM == 16 {
            assert_eq!(NCLASSES, 52);
        } else {
            assert_eq!(NCLASSES, 56);
        }
        assert_eq!(CLASSES.len(), NCLASSES);
    }

    #[test]
    fn class_sizes_follow_the_design() {
        let expected_head: &[usize] = if QUANTUM == 16 {
            &[16, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512]
        } else {
            &[8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256]
        };
        for (i, &want) in expected_head.iter().enumerate() {
            assert_eq!(CLASSES[i].size(), want, "class {i}");
        }
        assert_eq!(CLASSES[0].size(), QUANTUM);
        assert_eq!(CLASSES[NCLASSES - 1].size(), MEDIUM_MAX);
        for w in CLASSES.windows(2) {
            assert!(w[0].size < w[1].size, "sizes must be strictly monotonic");
        }
        for c in CLASSES.iter() {
            assert_eq!(c.size() % QUANTUM, 0);
        }
    }

    #[test]
    fn slab_geometry() {
        for (i, c) in CLASSES.iter().enumerate() {
            let slab = c.slab_size();
            assert!(slab.is_power_of_two());
            assert!((MIN_SLAB..=MAX_SLAB).contains(&slab), "class {i} slab {slab}");
            assert_eq!(c.nslots(), slab / c.size());
            assert!(c.nslots() * c.size() <= slab);
            // Slab bits: at least nslots, whole words or one sub-word
            // range, a line-sized group of slabs spanning at most
            // 1 << LG_GROUP_SPAN bytes, and no more padding than needed.
            let b = c.slab_bits();
            assert!(b >= c.nslots(), "class {i}");
            assert!(b.is_power_of_two() || b % LINE_BITS == 0, "class {i}: slab_bits {b}");
            assert!(b >= 64 || 64 % b == 0, "class {i}: sub-word slab_bits {b} must divide a word");
            assert_eq!(c.bitmap_words(), b.div_ceil(64));
            let g = c.slabs_per_line();
            assert!(g.is_power_of_two() && (g == 1 || g * b == LINE_BITS), "class {i}");
            assert!(g << c.lg_slab <= 1 << LG_GROUP_SPAN || g == 1, "class {i}: group spans {} bytes", g << c.lg_slab);
            assert_eq!(b, slab_bits_for(c.nslots(), c.lg_slab as u32));
            let cap = (1usize << LG_GROUP_SPAN >> c.lg_slab).max(1);
            if g == cap.min(LINE_BITS / c.nslots().next_power_of_two().min(LINE_BITS)) && g < cap {
                // Not raised for the span cap: no more padding than needed.
                if b <= LINE_BITS {
                    assert!(b / 2 < c.nslots(), "class {i}: slab_bits {b} over-padded for {} slots", c.nslots());
                } else {
                    assert!(b - LINE_BITS < c.nslots(), "class {i}");
                }
            }
            if c.size() < SMALL_SLAB_BELOW {
                assert_eq!(slab, 1 << LG_SMALL_SLAB, "class {i}: slab below {SMALL_SLAB_BELOW}");
            } else if c.size() <= SMALL_MAX {
                // The smallest slab from MIN_SLAB up with waste under 1/32.
                assert!(slab <= 1 << LG_SMALL_SLAB, "class {i}");
                assert!(slab - c.nslots() * c.size() <= slab / SLAB_WASTE_DIV, "class {i} wastes too much");
                if slab > MIN_SLAB {
                    let half = slab / 2;
                    assert!(half % c.size() > half / SLAB_WASTE_DIV, "class {i}: a {half}-byte slab would do");
                }
            } else {
                assert_eq!(slab, (16 * c.size()).next_power_of_two().max(1 << LG_MEDIUM_MIN_SLAB));
            }
            assert!(c.nslots() >= 4, "class {i} has {} slots", c.nslots());
            match c.tier() {
                Tier::Small => {
                    assert!(c.size() <= SMALL_MAX);
                    assert!(c.nslots() <= 8192);
                }
                Tier::Medium => {
                    assert!(c.size() > SMALL_MAX);
                    assert!(c.nslots() >= 16);
                }
            }
            // The thresholds' unit is a 64 KiB slab's worth of slots.
            assert_eq!(c.compact_slots(), slab.max(1 << LG_COMPACT_UNIT) / c.size());
            // The reciprocal is exact for every multiple of the size.
            assert_eq!(c.recip as u64, (1u64 << 32).div_ceil(c.size() as u64));
        }
    }

    /// Design 3, plan Phase 2.5 task 4e: the geometry a kernel with pages
    /// above 4 KiB adopts. Every class's slab is at least the page (up to
    /// 64 KiB), only the 512..1024-byte classes change, and for those the
    /// slot arithmetic, the reciprocal, the bitmap and descriptor
    /// invariants and the compaction unit hold as at 4 KiB.
    #[test]
    fn page_sized_geometry_for_large_pages() {
        assert_eq!(classes_for_page(LG_MIN_SLAB), BASE_CLASSES);
        assert_eq!(classes_for_page(LG_MIN_SLAB - 1), BASE_CLASSES, "a smaller page changes nothing");
        assert_eq!(classes_for_page(LG_MAX_SLAB), classes_for_page(LG_SMALL_SLAB), "capped at 64 KiB");
        for lg_page in LG_MIN_SLAB..=LG_SMALL_SLAB {
            let t = classes_for_page(lg_page);
            let mut changed = 0;
            for (c, (cls, base)) in t.iter().zip(BASE_CLASSES.iter()).enumerate() {
                assert_eq!((cls.size, cls.recip), (base.size, base.recip), "class {c}");
                assert_eq!(cls.lg_slab as u32, (base.lg_slab as u32).max(lg_page), "class {c} at 2^{lg_page}");
                assert_eq!(cls.lg_slab as u32, lg_slab_at(cls.size(), lg_page));
                assert!(cls.slab_size() >= 1 << lg_page, "class {c}: slab below the page");
                if cls == base {
                    continue;
                }
                changed += 1;
                assert!(cls.size() >= SMALL_SLAB_BELOW && cls.size() <= SMALL_MAX, "class {c}: only the small-slab classes move");
                assert_eq!(cls.nslots(), cls.slab_size() / cls.size());
                assert!(cls.nslots() >= 4);
                assert!(cls.slab_size() % cls.size() <= cls.slab_size() / SLAB_WASTE_DIV, "class {c}: waste");
                // Slot arithmetic round-trips for every slot and rejects
                // every other quantum-aligned offset; the reciprocal is exact.
                for slot in 0..cls.nslots() {
                    assert_eq!(cls.slot_of_offset(slot * cls.size()), (slot, true), "class {c} slot {slot}");
                }
                for off in (0..cls.slab_size()).step_by(QUANTUM) {
                    let (slot, valid) = cls.slot_of_offset(off);
                    assert_eq!(valid, off % cls.size() == 0 && off / cls.size() < cls.nslots(), "class {c} offset {off}");
                    if valid {
                        assert_eq!(slot, off / cls.size());
                    }
                }
                // Bitmap layout (design 4): whole words or one sub-word
                // range, a line holds whole slabs, a line's slabs span at
                // most `LG_GROUP_SPAN`, and the release scratch bitmap holds
                // a slab.
                let b = cls.slab_bits();
                assert!(b >= cls.nslots(), "class {c}");
                assert!((b.is_power_of_two() && b <= LINE_BITS) || b % LINE_BITS == 0, "class {c}: slab_bits {b}");
                if b < LINE_BITS {
                    assert_eq!(cls.slabs_per_line() * b, LINE_BITS, "class {c}");
                } else {
                    assert_eq!(cls.slabs_per_line(), 1);
                }
                assert!(cls.slabs_per_line() == 1 || cls.slabs_per_line() * cls.slab_size() <= 1 << LG_GROUP_SPAN, "class {c}");
                assert!(cls.bitmap_words() <= MAX_SLAB_WORDS);
                // Compaction thresholds count 64 KiB of slots (heap.rs).
                assert_eq!(cls.compact_slots(), (1usize << LG_COMPACT_UNIT).max(cls.slab_size()) / cls.size());
                if cls.lg_slab as u32 >= LG_COMPACT_UNIT {
                    assert_eq!(cls.compact_slots(), cls.nslots());
                }
            }
            let expect = BASE_CLASSES.iter().filter(|c| (c.lg_slab as u32) < lg_page).count();
            assert_eq!(changed, expect, "at 2^{lg_page}");
        }
        // The 64 KiB table by the numbers (jemalloc's bins at LG_PAGE 16
        // have the same regs for 512 and 1024), and the 16 KiB one.
        let at = |lg_page: u32, size: u32| -> Class {
            *classes_for_page(lg_page).iter().find(|c| c.size == size).unwrap()
        };
        for (size, nslots) in [(512, 128), (640, 102), (768, 85), (896, 73), (1024, 64)] {
            let c = at(LG_SMALL_SLAB, size);
            assert_eq!((c.lg_slab as u32, c.nslots), (LG_SMALL_SLAB, nslots), "{size} at 64 KiB");
            // 128 bits per slab for the first four (four slabs per bitmap
            // line), 64 for the last (eight per line).
            assert_eq!(c.slab_bits as u32, nslots.next_power_of_two(), "{size} at 64 KiB");
            assert_eq!(c.slabs_per_line(), if size == 1024 { 8 } else { 4 });
        }
        assert!(classes_for_page(LG_SMALL_SLAB).iter().all(|c| c.lg_slab as u32 >= LG_SMALL_SLAB));
        for (size, nslots) in [(512, 32), (640, 25), (768, 21), (896, 18), (1024, 16)] {
            let c = at(14, size);
            assert_eq!((c.lg_slab as u32, c.nslots), (14, nslots), "{size} at 16 KiB");
        }
        assert!(classes_for_page(14).iter().all(|c| c.lg_slab as u32 >= 14));
    }

    /// `adopt_page_geometry`'s writer, on a copy: it writes exactly the
    /// raised classes' geometry and nothing else, and reports the count.
    #[test]
    fn apply_page_geometry_writes_only_the_raised_classes() {
        let mut t = BASE_CLASSES;
        assert_eq!(unsafe { apply_page_geometry(&mut t, LG_MIN_SLAB) }, 0);
        assert_eq!(t, BASE_CLASSES);
        let n = unsafe { apply_page_geometry(&mut t, LG_SMALL_SLAB) };
        assert_eq!(t, classes_for_page(LG_SMALL_SLAB));
        assert_eq!(n, 5, "512, 640, 768, 896 and 1024");
        let mut t = BASE_CLASSES;
        assert_eq!(unsafe { apply_page_geometry(&mut t, 14) }, 3, "512, 896 and 1024; 640 and 768 already have 16 KiB slabs");
        assert_eq!(t, classes_for_page(14));
        // The host test process never adopts a page: the live table is the
        // compile-time one.
        assert_eq!(&*CLASSES, &BASE_CLASSES);
        assert_eq!(page_sized_classes(), None);
    }

    #[test]
    fn small_slab_classes_are_the_pinned_range() {
        // (size, slab, slots, compaction unit in slots): the 512..1024 classes
        // get 4..16 KiB slabs (the smallest wasting under 1/32), their
        // neighbours keep 64 KiB.
        for (size, slab, nslots, unit) in [
            (448, 65536, 146, 146),
            (512, 4096, 8, 128),
            (640, 16384, 25, 102),
            (768, 16384, 21, 85),
            (896, 8192, 9, 73),
            (1024, 4096, 4, 64),
            (1280, 65536, 51, 51),
            (4096, 65536, 16, 16),
            (8192, 131072, 16, 16),
        ] {
            let c = &CLASSES[class_of(size).unwrap()];
            assert_eq!(c.size(), size);
            assert_eq!((c.slab_size(), c.nslots(), c.compact_slots()), (slab, nslots, unit), "size {size}");
        }
        assert_eq!(lg_slab_for(64), 16);
        assert_eq!(lg_slab_for(SMALL_SLAB_BELOW - 1), LG_SMALL_SLAB);
    }

    #[test]
    fn every_request_maps_to_the_tightest_class() {
        for n in 0..=MEDIUM_MAX {
            let c = class_of(n).unwrap_or_else(|| panic!("no class for {n}"));
            assert_eq!(Some(c), class_of_slow(n), "request {n}");
            let sz = CLASSES[c].size();
            assert!(sz >= n, "request {n} got class {c} of {sz}");
            if c > 0 {
                assert!(CLASSES[c - 1].size() < n, "request {n} should fit class {}", c - 1);
            }
            assert_eq!(class_size_of(n), Some(sz));
        }
        assert_eq!(class_of(0), Some(0));
        assert_eq!(class_of(MEDIUM_MAX + 1), None);
        assert_eq!(class_of(usize::MAX), None);
        assert_eq!(class_size_of(1 << 40), None);
    }

    #[test]
    fn from_parts_layout_invariants() {
        let r = synthetic(32, 34);
        assert_eq!(r.region_len(), 1 << 32);
        assert_eq!(r.stride(), 1 << 34);
        assert_eq!(r.gap(), (1 << 34) - (1 << 32));
        for c in 0..NCLASSES {
            let base = r.class_base[c];
            assert_eq!(base & (MAX_SLAB - 1), 0);
            let start = r.superblock + c * r.stride();
            assert!(base >= start && base + r.region_len() <= start + r.stride());
            assert_eq!(r.nslabs(c), r.region_len() / CLASSES[c].slab_size());
            assert_eq!(r.slab_addr(c, 0), base);
            assert_eq!(r.slab_addr(c, 3), base + 3 * CLASSES[c].slab_size());
            assert_eq!(r.slot_addr(c, 3, 2), r.slab_addr(c, 3) + 2 * CLASSES[c].size());
        }
        assert_eq!(r.huge_pages(), r.huge_len / 4096);
        assert_eq!(r.huge_addr(5), r.huge_base + 5 * 4096);
    }

    #[test]
    fn slot_arithmetic_round_trips_for_every_slot() {
        let r = synthetic(32, 34);
        for c in 0..NCLASSES {
            let cls = &CLASSES[c];
            let last = r.nslabs(c) - 1;
            for slab in [0usize, 1, 7, last / 2, last] {
                for slot in 0..cls.nslots() {
                    let p = r.slot_addr(c, slab, slot);
                    assert_eq!(
                        r.classify(p),
                        Kind::Slab { class: c as u32, slab: slab as u32, slot: slot as u32 },
                        "class {c} slab {slab} slot {slot}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_in_slab_offset_classifies_exactly() {
        let r = synthetic(32, 34);
        for c in 0..NCLASSES {
            let cls = &CLASSES[c];
            let last = r.nslabs(c) - 1;
            for slab in [0usize, last] {
                let base = r.slab_addr(c, slab);
                for offset in 0..cls.slab_size() {
                    let want = if offset % cls.size() == 0 && offset / cls.size() < cls.nslots() {
                        Kind::Slab {
                            class: c as u32,
                            slab: slab as u32,
                            slot: (offset / cls.size()) as u32,
                        }
                    } else {
                        Kind::Invalid
                    };
                    assert_eq!(r.classify(base + offset), want, "class {c} slab {slab} offset {offset}");
                }
            }
        }
    }

    #[test]
    fn misaligned_and_out_of_region_pointers_are_invalid() {
        let r = synthetic(32, 34);
        for c in 0..NCLASSES {
            let base = r.class_base[c];
            let end = base + r.region_len();
            let stride_start = r.superblock + c * r.stride();
            assert_eq!(r.classify(base - 1), Kind::Invalid, "byte before class {c}");
            assert_eq!(r.classify(base - MAX_SLAB), Kind::Invalid);
            assert_eq!(r.classify(end), Kind::Invalid, "first gap byte after class {c}");
            assert_eq!(r.classify(end + 12345), Kind::Invalid);
            assert_eq!(r.classify(stride_start + r.stride() - 1), Kind::Invalid);
            if stride_start != base {
                assert_eq!(r.classify(stride_start), Kind::Invalid);
            }
            for k in 1..QUANTUM {
                assert_eq!(r.classify(base + k), Kind::Invalid, "class {c} +{k}");
            }
        }
        assert_eq!(r.classify(0), Kind::Invalid);
        assert_eq!(r.classify(usize::MAX), Kind::Invalid);
        assert_eq!(r.classify(r.superblock - 1), Kind::Invalid);
        assert_eq!(r.classify(r.superblock + NCLASSES * r.stride()), Kind::Invalid);
        assert_eq!(r.classify(r.superblock + (NCLASSES + 3) * r.stride()), Kind::Invalid);
    }

    #[test]
    fn huge_and_meta_classification() {
        let r = synthetic(32, 34);
        let page = r.page_size();
        assert_eq!(r.classify(r.huge_base), Kind::Huge { page: 0 });
        assert_eq!(r.classify(r.huge_base + 17 * page), Kind::Huge { page: 17 });
        assert_eq!(r.classify(r.huge_addr(r.huge_pages() - 1)), Kind::Huge { page: r.huge_pages() - 1 });
        assert_eq!(r.classify(r.huge_base + 1), Kind::Invalid);
        assert_eq!(r.classify(r.huge_base + page - 1), Kind::Invalid);
        assert_eq!(r.classify(r.huge_base + page + 8), Kind::Invalid);
        assert_eq!(r.classify(r.huge_base - page), Kind::Invalid);
        assert_eq!(r.classify(r.huge_base + r.huge_len), Kind::Invalid);
        assert!(r.in_meta(r.meta_base));
        assert!(r.in_meta(r.meta_base + r.meta_len - 1));
        assert!(!r.in_meta(r.meta_base + r.meta_len));
        assert!(!r.in_meta(r.meta_base - 1));
        assert_eq!(r.classify(r.meta_base), Kind::Invalid);
        assert_eq!(r.classify(r.meta_base + page), Kind::Invalid);
    }

    #[test]
    fn classes_do_not_overlap_each_other_or_huge() {
        let r = synthetic(32, 34);
        let mut ranges: Vec<(usize, usize)> =
            (0..NCLASSES).map(|c| (r.class_base[c], r.class_base[c] + r.region_len())).collect();
        ranges.push((r.huge_base, r.huge_base + r.huge_len));
        ranges.push((r.meta_base, r.meta_base + r.meta_len));
        ranges.sort();
        for w in ranges.windows(2) {
            assert!(w[0].1 <= w[1].0, "{:?} overlaps {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn minimal_stride_layout_still_classifies() {
        // The last-resort layout: 4 GiB regions at a 2x stride.
        let r = synthetic(32, 33);
        for c in [0usize, NCLASSES / 2, NCLASSES - 1] {
            let cls = &CLASSES[c];
            let last = r.nslabs(c) - 1;
            for slab in [0usize, last] {
                for slot in [0usize, 1, cls.nslots() - 1] {
                    let p = r.slot_addr(c, slab, slot);
                    assert_eq!(
                        r.classify(p),
                        Kind::Slab { class: c as u32, slab: slab as u32, slot: slot as u32 }
                    );
                }
            }
            assert_eq!(r.classify(r.class_base[c] + r.region_len()), Kind::Invalid);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use crate::sys::{
        self, Errno, MADV_DONTNEED, MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, PROT_NONE, PROT_READ, PROT_WRITE,
    };
    use std::sync::Mutex;
    use std::{eprintln, string::String};

    /// One full layout is up to 53 TiB of address space; two live at once
    /// do not fit in x86_64's 47-bit user space, and Rosetta traps instead of
    /// returning `ENOMEM` when it cannot place a mapping. The allocator only
    /// ever reserves once per process, so the tests take turns too.
    static RESERVE_LOCK: Mutex<()> = Mutex::new(());

    fn maps() -> String {
        std::fs::read_to_string("/proc/self/maps").unwrap()
    }

    /// True when `m` lies inside one `/proc/self/maps` entry with `perms`.
    /// Containment rather than equality: adjacent anonymous mappings with
    /// identical flags merge into one VMA, so our three reservations may show
    /// up as a single line.
    fn has_mapping(maps: &str, m: Mapping, perms: &str) -> bool {
        maps.lines().any(|l| {
            let mut fields = l.split(' ');
            let Some(range) = fields.next() else { return false };
            if fields.next() != Some(perms) {
                return false;
            }
            let Some((lo, hi)) = range.split_once('-') else { return false };
            let (Ok(lo), Ok(hi)) = (usize::from_str_radix(lo, 16), usize::from_str_radix(hi, 16)) else {
                return false;
            };
            lo <= m.addr && m.addr + m.len <= hi
        })
    }

    #[test]
    fn reserve_lays_out_regions_and_commits_a_slab() {
        let _guard = RESERVE_LOCK.lock().unwrap();
        let page = sys::features::page_size();
        let r = Regions::reserve(page, true);
        eprintln!(
            "layout: lg_region={} lg_stride={} huge=2^{} meta=2^{} superblock={:#x}",
            r.lg_region,
            r.lg_stride,
            r.huge_len.trailing_zeros(),
            r.meta_len.trailing_zeros(),
            r.superblock
        );
        assert!(r.lg_region >= LG_REGION_MIN && r.lg_region <= LG_REGION_PREFERRED);
        assert!(r.lg_stride == r.lg_region + 2 || r.lg_stride == r.lg_region + 1);
        assert!(r.huge_len >= 1 << LG_HUGE_MIN && r.meta_len >= 1 << LG_META_MIN);
        assert_eq!(r.page_size(), page);

        // Everything is inside its mapping, and the mappings are PROT_NONE.
        assert!(r.superblock >= r.superblock_map.addr);
        assert!(r.superblock + (NCLASSES << r.lg_stride) <= r.superblock_map.addr + r.superblock_map.len);
        assert!(r.huge_base >= r.huge_map.addr && r.huge_base + r.huge_len <= r.huge_map.addr + r.huge_map.len);
        assert!(r.meta_base >= r.meta_map.addr && r.meta_base + r.meta_len <= r.meta_map.addr + r.meta_map.len);
        assert_eq!(r.superblock & (MAX_SLAB - 1), 0);
        assert_eq!(r.huge_base & (MAX_SLAB - 1), 0);
        assert_eq!(r.meta_base & (MAX_SLAB - 1), 0);
        let m = maps();
        for map in [r.superblock_map, r.huge_map, r.meta_map] {
            assert!(has_mapping(&m, map, "---p"), "missing PROT_NONE mapping {map:x?} in:\n{m}");
        }

        // Classes are where the arithmetic says, and nowhere else.
        for c in 0..NCLASSES {
            let base = r.class_base[c];
            assert_eq!(base & (MAX_SLAB - 1), 0);
            let start = r.superblock + (c << r.lg_stride);
            assert!(base >= start && base + r.region_len() <= start + r.stride());
            assert_eq!(r.classify(base), Kind::Slab { class: c as u32, slab: 0, slot: 0 });
            assert_eq!(r.classify(base + r.region_len()), Kind::Invalid);
        }

        // Committing one slab is an mprotect on the reservation; it reads
        // zero and holds what we write.
        let c = NCLASSES / 2;
        let slab = r.slab_addr(c, 3);
        let len = CLASSES[c].slab_size();
        // SAFETY: the range is inside our own PROT_NONE reservation.
        unsafe { sys::mprotect(slab, len, PROT_READ | PROT_WRITE) }.unwrap();
        let p = r.slot_addr(c, 3, 2) as *mut u64;
        // SAFETY: the slab was just committed read/write.
        unsafe {
            assert_eq!(p.read(), 0);
            p.write(0xDEAD_BEEF);
            assert_eq!(p.read(), 0xDEAD_BEEF);
        }
        assert_eq!(r.classify(p as usize), Kind::Slab { class: c as u32, slab: 3, slot: 2 });
        // The commit left the rest of the mapping PROT_NONE.
        let m = maps();
        assert!(
            has_mapping(&m, Mapping { addr: slab, len }, "rw-p"),
            "committed slab not rw-p in:\n{m}"
        );

        // SAFETY: nothing references the regions any more.
        unsafe { r.release() };
        let m = maps();
        assert!(!has_mapping(&m, r.superblock_map, "---p"));
    }

    #[test]
    fn reserve_randomises_class_offsets_and_order() {
        let _guard = RESERVE_LOCK.lock().unwrap();
        let page = sys::features::page_size();
        // Two sequential reservations: offsets inside the superblock and the
        // slides of the huge and metadata bases must not repeat.
        let a = Regions::reserve(page, false);
        let a_offsets: [usize; NCLASSES] =
            core::array::from_fn(|c| a.class_base[c] - (a.superblock + (c << a.lg_stride)));
        let a_slides = (a.huge_base - a.huge_map.addr, a.meta_base - a.meta_map.addr);
        // SAFETY: nothing references the regions any more.
        unsafe { a.release() };
        let b = Regions::reserve(page, false);
        let b_offsets: [usize; NCLASSES] =
            core::array::from_fn(|c| b.class_base[c] - (b.superblock + (c << b.lg_stride)));
        let b_slides = (b.huge_base - b.huge_map.addr, b.meta_base - b.meta_map.addr);
        // SAFETY: nothing references the regions any more.
        unsafe { b.release() };
        let differing = (0..NCLASSES).filter(|&c| a_offsets[c] != b_offsets[c]).count();
        assert!(differing > NCLASSES / 2, "only {differing} class offsets differ");
        assert_ne!(a_slides, b_slides, "huge/meta slides repeated");
        for off in a_offsets.iter().chain(b_offsets.iter()) {
            assert_eq!(off & (MAX_SLAB - 1), 0);
        }
    }

    /// Exit code of [`seal_child`] when the kernel (or seccomp) has no `mseal`.
    const SKIPPED: i32 = 3;

    /// Runs in a forked child because a sealed layout can never be released
    /// and a second full layout must not stay alive in the test process.
    /// Returns the number of failed checks, or `SKIPPED`.
    fn seal_child(page: usize) -> i32 {
        if !sys::features::probe_mseal(page) {
            return SKIPPED;
        }
        let r = Regions::reserve(page, true);
        let before = maps().lines().count();
        let (ranges, n) = r.never_committed();
        let sealed = r.seal() as usize;
        let after = maps().lines().count();
        let mut fails = 0;
        let mut check = |ok: bool, what: &str| {
            if !ok {
                eprintln!("seal: FAIL {what}");
                fails += 1;
            }
        };
        eprintln!(
            "seal: {sealed} of {n} ranges sealed (max {MAX_SEALED}), /proc/self/maps lines {before} -> {after} (+{})",
            after - before
        );
        check(sealed == n, "not every never-committed range was sealed");
        // Each sealed range splits its VMA into at most two more pieces.
        check(after - before <= 2 * n + 2, "VMA count grew by more than 2 per sealed range");

        // The plan's criterion: munmap of a reserved range fails with EPERM.
        // Likewise mprotect (a commit landing in a gap) and MAP_FIXED over
        // it (a foreign mapping placed in the gap).
        for m in &ranges[..n] {
            // SAFETY: the kernel refuses every one of these; nothing changes.
            unsafe {
                check(sys::munmap(m.addr, m.len) == Err(Errno::EPERM), "munmap of a sealed range did not fail with EPERM");
                check(
                    sys::mprotect(m.addr, page, PROT_READ | PROT_WRITE) == Err(Errno::EPERM),
                    "mprotect of a sealed range did not fail with EPERM"
                );
                check(
                    sys::mmap(m.addr, page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) == Err(Errno::EPERM),
                    "MAP_FIXED over a sealed range did not fail with EPERM"
                );
            }
        }
        // smaps marks sealed VMAs with `sl`; the largest range is a gap.
        let biggest = ranges[..n].iter().max_by_key(|m| m.len).unwrap();
        let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap();
        let mut in_vma = false;
        let mut flagged = false;
        for l in smaps.lines() {
            if let Some((range, _)) = l.split_once(' ') {
                if let Some((lo, _)) = range.split_once('-') {
                    if let Ok(lo) = usize::from_str_radix(lo, 16) {
                        in_vma = lo == biggest.addr;
                    }
                }
            }
            if in_vma && l.starts_with("VmFlags:") {
                flagged = l.split_whitespace().any(|f| f == "sl");
            }
        }
        check(flagged, "the largest sealed gap has no `sl` in smaps VmFlags");

        // The regions themselves are untouched: a slab of the first and
        // last class, the first huge page and the first metadata page still
        // commit, hold data, purge and reprotect.
        let first_slab = CLASSES[0].slab_size();
        let last_slab = CLASSES[NCLASSES - 1].slab_size();
        for (addr, len) in [
            (r.slab_addr(0, 0), first_slab),
            (r.slab_addr(NCLASSES - 1, r.nslabs(NCLASSES - 1) - 1), last_slab),
            (r.huge_addr(0), page),
            (r.meta_base, page),
        ] {
            // SAFETY: inside our own unsealed reservation.
            unsafe {
                check(sys::mprotect(addr, len, PROT_READ | PROT_WRITE).is_ok(), "commit inside a region failed");
                let p = addr as *mut u64;
                p.write_volatile(0x5EA1);
                check(sys::madvise(addr, len, MADV_DONTNEED).is_ok(), "purge inside a region failed");
                check(p.read_volatile() == 0, "purged page did not read zero");
                check(sys::mprotect(addr, len, PROT_NONE).is_ok(), "reprotect inside a region failed");
            }
        }
        fails
    }

    #[test]
    fn seal_locks_the_gaps_and_leaves_the_regions_usable() {
        let _guard = RESERVE_LOCK.lock().unwrap();
        let page = sys::features::page_size();
        // SAFETY: the child only makes syscalls and uses this crate's own
        // allocator, which the fork handlers keep consistent; it exits
        // through `_exit` without returning into the harness.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            let code = seal_child(page);
            // SAFETY: terminating the child.
            unsafe { libc::_exit(code) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status), "seal child died: status {status:#x}");
        let code = libc::WEXITSTATUS(status);
        if code == SKIPPED {
            eprintln!("seal: mseal unavailable here (kernel < 6.10 or seccomp), skipped");
            return;
        }
        assert_eq!(code, 0, "{code} seal checks failed (see stderr)");
    }
}
