//! Size classes, address-space regions, and pointer classification
//! (design.md sections 2 and 3).
//!
//! Everything in this module except [`Regions::reserve`] is pure arithmetic
//! over a compile-time class table and a handful of runtime bases, and is
//! unit tested exhaustively on the host. `reserve` is the Linux-only piece
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

/// Largest small-tier request.
pub const SMALL_MAX: usize = 1024;
/// Largest request served from a slab; anything above is huge.
pub const MEDIUM_MAX: usize = 256 * 1024;
/// Smallest slab (4 KiB). The classes from [`SMALL_SLAB_BELOW`] to
/// [`SMALL_MAX`] use the smallest power of two from here up whose internal
/// waste (`slab - nslots * size`) is at most a [`SLAB_WASTE_DIV`]th of the
/// slab: 4 KiB for 512, 640, 768 and 1024 bytes (8, 6, 5 and 4 slots) and
/// 8 KiB for 896 bytes (9 slots). Every other class keeps a 64 KiB slab or
/// larger ([`LG_SMALL_SLAB`], [`LG_MEDIUM_MIN_SLAB`]).
///
/// Why these classes and this size (Phase 1 acceptance step 3,
/// `docs/review-2026-09-17.md` P6, measured in
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
/// than the kernel page (4 KiB slabs on a 16 KiB or 64 KiB page kernel) is
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
pub const SLAB_WASTE_DIV: usize = 16;
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

/// One size class. Everything here is a compile-time constant.
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
}

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

    /// `u64` words needed for a per-slab slot bitmap.
    #[inline]
    pub const fn bitmap_words(&self) -> usize {
        (self.nslots as usize).div_ceil(64)
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

const fn build_classes() -> [Class; NCLASSES] {
    let mut out = [Class { size: 0, lg_slab: 0, nslots: 0, recip: 0 }; NCLASSES];
    let mut i = 0;
    while i < NCLASSES {
        let size = class_size_at(i);
        let lg_slab = lg_slab_for(size);
        out[i] = Class {
            size: size as u32,
            lg_slab: lg_slab as u8,
            nslots: ((1usize << lg_slab) / size) as u32,
            recip: reciprocal(size),
        };
        i += 1;
    }
    out
}

/// The size-class table, indexed by class number.
pub static CLASSES: [Class; NCLASSES] = build_classes();

/// Brute-force class lookup, used to build the small table at compile time
/// and by tests as the reference.
const fn class_of_slow(n: usize) -> Option<usize> {
    let mut i = 0;
    while i < NCLASSES {
        if CLASSES[i].size as usize >= n {
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
/// sliding. Kept so later phases can seal and account for the whole range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub addr: usize,
    pub len: usize,
}

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
    pub const fn nslabs(&self, class: usize) -> usize {
        self.region_len() >> CLASSES[class].lg_slab
    }

    /// Pages in the huge region.
    #[inline]
    pub const fn huge_pages(&self) -> usize {
        self.huge_len >> self.lg_page
    }

    #[inline]
    pub const fn slab_addr(&self, class: usize, slab: usize) -> usize {
        self.class_base[class] + (slab << CLASSES[class].lg_slab)
    }

    #[inline]
    pub const fn slot_addr(&self, class: usize, slab: usize, slot: usize) -> usize {
        self.slab_addr(class, slab) + slot * CLASSES[class].size as usize
    }

    #[inline]
    pub const fn huge_addr(&self, page: usize) -> usize {
        self.huge_base + (page << self.lg_page)
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
        // SAFETY: `c < NCLASSES` was just checked.
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

        /// Give the reservations back. The allocator never does this; it
        /// exists so tests can reserve repeatedly in one process.
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
            assert_eq!(c.bitmap_words(), c.nslots().div_ceil(64));
            if c.size() < SMALL_SLAB_BELOW {
                assert_eq!(slab, 1 << LG_SMALL_SLAB, "class {i}: slab below {SMALL_SLAB_BELOW}");
            } else if c.size() <= SMALL_MAX {
                // The smallest slab from MIN_SLAB up with waste under 1/16.
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

    #[test]
    fn small_slab_classes_are_the_pinned_range() {
        // (size, slab, slots, compaction unit in slots): the 512..1024 classes
        // get 4 KiB slabs (8 KiB where 4 KiB would waste an eighth), their
        // neighbours keep 64 KiB.
        for (size, slab, nslots, unit) in [
            (448, 65536, 146, 146),
            (512, 4096, 8, 128),
            (640, 4096, 6, 102),
            (768, 4096, 5, 85),
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
    use crate::sys::{self, PROT_READ, PROT_WRITE};
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
}
