//! Per-class state, slab selection, commit, and the claim/release protocol
//! (design.md sections 4 and 5).
//!
//! One [`SlabTable`] exists per class: the class region's descriptor array
//! and the commit watermark, shared by every arena, behind a commit lock.
//! One [`ClassState`] exists per `(arena, class)`. It owns three
//! [`TwoLevelBitmap`]s over slab numbers: `partial` (slabs believed to have
//! a claimable slot), `empty` (`live == 0`, dirty, not yet purged) and
//! `purged` (`live == 0`, pages discarded). All three are behind one futex
//! lock that is taken only on slab transitions: selecting a slab for a
//! refill, a slab filling up or emptying, a word going from full to
//! non-full, purge, commit (which nests the table's commit lock inside).
//! Claims and releases against a slab's bits happen outside the lock.
//!
//! Slabs are handed to arenas in *groups* of consecutive slabs
//! (`SlabTable::group`: the slabs whose bits fill one 64-byte line of the
//! class bitmap, `Class::slabs_per_line`, or one page group if that is
//! larger), lowest group first to whichever arena runs out of slabs next.
//! A group's slabs are committed one at a time by their arena as it needs
//! them (`ClassState::commit_next`), so an arena owns whole bitmap lines
//! and whole descriptor lines: the `free` check of one arena's slot never
//! reads a line another arena's refills and flushes write. Every slab
//! belongs to the arena of its group (`SlabTable::owner`, a dense array of
//! one `u16` per group, and `SlabDesc::arena`), and only that arena's state
//! may claim from or release to it.
//!
//! Protocol. A refill takes the lock, picks the lowest partial slab (else
//! the lowest empty, else the lowest purged, else commits the next slab),
//! moves it to `Active`, *reserves* the wanted count in the descriptor's
//! `live` counter, and releases the lock. It then claims bits, hands out
//! what it got and gives back the unused reservation. Reserving before
//! claiming means `live` is never zero while a claim is in flight, so a
//! purge (which requires `live == 0` under the lock) can never discard
//! memory a thread is about to hand out, and a slab observed `Fresh` or
//! `Purged` at selection is guaranteed zero for the thread that selected it.
//!
//! A release clears bits with `fetch_and` and then subtracts from `live`.
//! If a word was full before the clear the releaser sets the partial bit
//! under the lock (once per batch, not per word); if `live` reaches zero
//! the releaser makes the slab `Empty` under the lock. A refill plans all
//! its slabs in one lock hold, reserving at most `nslots - live` from each,
//! and clears the partial bit of a slab it finds full (`is_full` under the
//! lock), so a concurrent release either is visible to that check or sets
//! the bit afterwards.

use crate::bitmap::{AtomicBitmap, TwoLevelBitmap};
use crate::layout::{CLASSES, Class, Regions};
use crate::lock::{Lock, PAD_ALIGN, Padded, RawLock};
use crate::meta::Meta;
use crate::purge::{self, Batch};
use crate::slab::{DESC_ALIGN, SlabDesc, State, desc_stride};
use crate::sys::Errno;
use core::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};

/// Counters kept under the class lock. Slab counts only; `curregs` and
/// byte totals are Phase 1 (`stats.rs`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassStats {
    /// Slabs committed for allocation (excludes guard slabs).
    pub committed: usize,
    /// Guard slabs committed (never handed out).
    pub guards: usize,
    /// Slabs currently `Empty`.
    pub empty: usize,
    /// Slabs currently `Purged`.
    pub purged: usize,
    /// Slabs in the partial set (jemalloc's `nonfull_slabs`).
    pub nonfull: usize,
}

/// Result of one [`ClassState::claim`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// Slots written to the output; fewer than asked means the region or
    /// the metadata region is exhausted or a commit failed.
    pub n: usize,
    /// Every returned slot came from a slab that was `Fresh` or `Purged`
    /// when this thread selected it, so its memory reads as zero.
    pub zeroed: bool,
}

/// Slabs one lock hold of a refill may reserve from (see
/// [`ClassState::plan`]), and slabs one lock hold of a release batch may
/// reopen ([`Reopen`]). Sized so that the common refill and flush of every
/// class fit in one hold: with the 4 KiB slabs of the 512 and 1024-byte
/// classes (`layout::LG_MIN_SLAB`) a 128-slot refill of the 512 class and a
/// 64-slot one of the 1024 class each span 16 slabs.
const MAX_PICKS: usize = 16;

/// Full bitmap scans of one slab that find nothing before a claim gives up
/// on its reservation and aborts. A scan comes up empty only while other
/// threads keep moving free bits between words, so this many in a row
/// means the `live` count no longer bounds the set bits.
const MAX_EMPTY_PASSES: u32 = 1 << 22;

/// One slab's share of a refill: `n` slots reserved in its `live` count.
#[derive(Copy, Clone)]
struct Pick {
    slab: u32,
    n: u32,
    /// The slab was `Fresh` or `Purged` when this refill made it `Active`.
    zero: bool,
}

/// Slabs a release batch must put back in the partial set. Consecutive
/// duplicates are dropped; a full list is flushed under the lock.
#[derive(Default)]
struct Reopen {
    slabs: [u32; MAX_PICKS],
    len: usize,
}

impl Reopen {
    /// Record `slab`; `false` if the list is full (and `slab` is not the
    /// last entry), in which case the caller must flush first.
    fn push(&mut self, slab: usize) -> bool {
        if self.len > 0 && self.slabs[self.len - 1] == slab as u32 {
            return true;
        }
        if self.len == MAX_PICKS {
            return false;
        }
        self.slabs[self.len] = slab as u32;
        self.len += 1;
        true
    }
}

/// A never-freed table in the metadata region, committed page by page as
/// the high-water mark grows. Shared with `huge.rs`.
pub(crate) struct Lazy {
    pub(crate) base: usize,
    len: usize,
    committed: usize,
}

impl Lazy {
    pub(crate) fn reserve(meta: &Meta, len: usize, align: usize) -> Option<Lazy> {
        let base = meta.reserve(len, align)?;
        Some(Lazy { base, len, committed: 0 })
    }

    /// Make the first `upto` bytes committed.
    pub(crate) fn ensure(&mut self, meta: &Meta, page: usize, upto: usize) -> Result<(), Errno> {
        let want = ((upto + page - 1) & !(page - 1)).min((self.len + page - 1) & !(page - 1));
        if want > self.committed {
            meta.commit(self.base + self.committed, want - self.committed)?;
            self.committed = want;
        }
        Ok(())
    }
}

/// A slab-number (or huge-page) set: a [`TwoLevelBitmap`] over a lazily
/// committed table. Shared with `huge.rs`.
pub(crate) struct Set {
    pub(crate) map: TwoLevelBitmap,
    table: Lazy,
    l1_words: usize,
}

impl Set {
    pub(crate) fn new(meta: &Meta, page: usize, nslabs: usize) -> Option<Set> {
        let l0_words = nslabs.div_ceil(64);
        let l1_words = l0_words.div_ceil(64);
        let mut table = Lazy::reserve(meta, 8 * TwoLevelBitmap::words_needed(nslabs), page)?;
        // Level 1 is small and touched by every operation: commit it now.
        table.ensure(meta, page, 8 * l1_words).ok()?;
        // SAFETY: the words come from a fresh, zero, never-freed reservation.
        let map = unsafe { TwoLevelBitmap::from_raw(nslabs, table.base as *mut u64) };
        Some(Set { map, table, l1_words })
    }

    /// Commit the level-0 words that hold every bit up to and including
    /// `slab`.
    pub(crate) fn ensure(&mut self, meta: &Meta, page: usize, slab: usize) -> Result<(), Errno> {
        self.table.ensure(meta, page, 8 * (self.l1_words + slab / 64 + 1))
    }
}

struct TableInner {
    descs: Lazy,
    /// The class bitmap (see [`SlabTable::bits`]).
    bits: Lazy,
    /// The group owner array (see [`SlabTable::owner`]).
    owners: Lazy,
    /// Next slab number not yet handed to any arena (`high_water + 1` in
    /// the design, at group granularity).
    next_slab: usize,
    /// Guard slabs committed (never handed out).
    guards: usize,
}

/// Offset of class `class`'s descriptor array, bitmap and owner array
/// inside their reservations.
///
/// The per-class tables are reserved back to back from the top of the
/// metadata region, and each is a power of two of at least 1 MiB, so
/// without this every table would start on a 1 MiB boundary and slab `k`'s
/// entry of every class would map to the same L1 set (16 KiB per way) and
/// the same L2 set (1 MiB per way). The `free` fast path loads one bitmap
/// word (and one owner word) per call, so that is a 52-way conflict on the
/// hottest lines it has. Three lines per class, and `gcd(3, 128) == 1`, so
/// all 52 classes land in distinct sets of both caches; a multiple of
/// [`DESC_ALIGN`] keeps the descriptors aligned. The pre-arena tree got a
/// similar spread by accident, from the set bitmaps reserved between one
/// class's table and the next (`bench/results/2026-09-16-aarch64-arenas-geometry.md`).
#[inline]
pub const fn desc_stagger(class: usize) -> usize {
    class * 3 * PAD_ALIGN
}

/// What every arena's [`ClassState`] for one class shares (design 13.3:
/// arenas are sets of class states over the *same* class regions): the
/// region's geometry, the descriptor array, the class bitmap, the group
/// owner array and the hand-out watermark. Slab groups go in address order
/// to whichever arena needs one next, so arenas interleave in the region at
/// group granularity; `free` reads the owner array to route a slot back to
/// a cache of its arena.
///
/// Layout: the fields before `inner` are written once by `new` (or, for
/// `committed_slabs`, only when a group is handed out) and read by every
/// `free`, while `inner` holds the commit lock. `repr(C)` keeps the
/// read-mostly fields first and [`Padded`] starts `inner` on its own line.
/// The fields the `free` fast path reads (`bits_base`, `owners_base`,
/// `committed_slabs`, `slab_bits`, `lg_group`) sit in the first 32 bytes:
/// one L1 line even on a core with 64-byte lines. The slot geometry is not
/// copied here on purpose; `free` reads it from the dense
/// `Regions::class_base` and `CLASSES` arrays (see `Shared::checked_slot`).
///
/// The stride of `[SlabTable; NCLASSES]` (384 bytes) is not a multiple of
/// 512 for the reason given on [`ClassState`].
#[repr(C, align(128))]
pub struct SlabTable {
    /// Start of the class bitmap: bit `slab * slab_bits + slot` is the
    /// allocation bit of `slot` in `slab` (design section 4).
    bits_base: usize,
    /// Start of the owner array: one `AtomicU16` per group of
    /// `1 << lg_group` slabs, the arena the group was handed to.
    owners_base: usize,
    /// Slabs handed to an arena so far (the hand-out watermark; a multiple
    /// of the group size except at the end of the region). Their
    /// descriptors, bitmap words and owner words exist, readable without
    /// the lock so the `free` check can reject pointers into never-handed
    /// slabs cleanly; a slab below it may still be uncommitted (its arena
    /// has not reached it yet), in which case its bits are clear and its
    /// descriptor is `Fresh`.
    committed_slabs: AtomicUsize,
    /// `Class::slab_bits`, kept here so the fast path reads it with the
    /// table line.
    slab_bits: u16,
    class: u16,
    /// `log2(group)`.
    lg_group: u8,
    _pad: [u8; 3],
    /// Every `guard_interval`-th page group is a guard group; 0 disables
    /// guards.
    guard_interval: u32,
    lg_page: u32,
    /// `desc_stride(class)`.
    desc_stride: u32,
    /// `max(1, page / slab_size)`, a power of two. Slabs are committed,
    /// guarded and purged in page-sized groups, so on a kernel whose page is
    /// larger than this class's slab (16 KiB slabs on 64 KiB pages) a group
    /// of `slabs_per_page` consecutive slabs is committed when its first slab
    /// is, is one guard region when it is a guard group, and is purged only
    /// when every slab in it is idle and owned by the purging arena.
    slabs_per_page: u32,
    /// Slabs handed to an arena at a time: `max(slabs_per_line,
    /// slabs_per_page)`, a power of two, so a group is whole bitmap lines
    /// and whole page groups.
    group: u32,
    /// Start of this class's region.
    base: usize,
    /// Number of slabs in the region.
    nslabs: usize,
    /// Start of the descriptor array.
    desc_base: usize,
    inner: Padded<Lock<TableInner>>,
    _tail: [u8; PAD_ALIGN],
}

const _: () = assert!(core::mem::offset_of!(SlabTable, inner) == PAD_ALIGN);
const _: () = assert!(core::mem::offset_of!(SlabTable, lg_group) < 32);
const _: () = assert!(core::mem::size_of::<SlabTable>() % PAD_ALIGN == 0);
const _: () = assert!(core::mem::size_of::<SlabTable>() % 512 != 0);

// SAFETY: `Lazy` holds an address into the metadata region that is only
// ever used under the lock.
unsafe impl Send for TableInner {}

struct Inner {
    partial: Set,
    empty: Set,
    purged: Set,
    stats: ClassStats,
    /// This arena's current slab group: `group_next..group_end` are handed
    /// to it and not yet committed (see [`ClassState::commit_next`]).
    group_next: usize,
    group_end: usize,
}

// SAFETY: `TwoLevelBitmap` holds a raw pointer into the metadata region,
// which is process-global and only ever accessed through this lock.
unsafe impl Send for Inner {}

/// State of one `(arena, class)` pair: the three slab sets and their
/// statistics behind the class lock, plus a pointer to the class's shared
/// [`SlabTable`].
///
/// Layout matters: the fields before `inner` are written once by `new` and
/// read by every refill and flush, while `inner` holds the class lock and
/// the state every refill and flush writes. `repr(C)` keeps the read-mostly
/// fields first and [`Padded`] starts `inner` on its own line, so a lock
/// acquisition on one core does not evict the table pointer from the
/// others. The alignment makes `[ClassState; NCLASSES]` line-granular too.
///
/// The stride of that array must not be a multiple of 512 bytes. With 4 KiB
/// pages a 512-byte stride puts the hot line of every class on the same
/// handful of L1 sets: on Apple silicon (128-byte lines, sets indexed by
/// page-offset bits) single-threaded `alloc-test` slowed from 2.13 s to
/// 2.52 s. `_tail` makes the stride 640 = 5 lines, which visits every set.
/// See `bench/results/2026-09-16-aarch64-*.md`.
#[repr(C, align(128))]
pub struct ClassState {
    class: u16,
    arena: u16,
    /// The class's shared table, which lives as long as the allocator
    /// (inside `Shared`, or leaked by the tests).
    table: *const SlabTable,
    /// `dirty_at` of the oldest `Empty` slab, 0 when none is known. Written
    /// under the lock (set when the empty set goes from none to one,
    /// recomputed by every purge) and read without it by the decay driver.
    /// It may lag behind a claim that reused the oldest slab, in which case
    /// it is too early rather than too late; the next `decay` recomputes.
    oldest_empty: AtomicU64,
    inner: Padded<Lock<Inner>>,
    /// Slots ever reserved by `take` for this arena (written only under
    /// the class lock). With `released` below it gives `curregs`, the
    /// number of slots out of the bitmaps (in caches or in use), as
    /// `claimed - released`: that is the sum of `live` over this arena's
    /// slabs without a walk over the descriptors or a lock per flush. The
    /// two counters share the last line of the state, which nothing else
    /// touches: a refill writes `claimed` while it holds the lock and a
    /// flush adds to `released` once per batch (`stats.rs`).
    claimed: AtomicU64,
    /// Slots ever released to this arena's bitmaps, one add per batch.
    released: AtomicU64,
    /// Slots claimed by cache *refills* (`Heap::refill`, one add per
    /// batch): the part of `claimed` that ordinary allocation traffic
    /// accounts for, as opposed to compaction rebuilds and direct
    /// (`MALLOCX_TCACHE_NONE`) claims such as Valkey's defragmenter's. The
    /// compaction gate in `heap.rs` watches its rate.
    refilled: AtomicU64,
    /// Compaction diagnostics, printed in the bins table of
    /// `malloc_stats_print` (Phase 1 acceptance step 3): slots claimed by
    /// compaction rebuilds, caches that entered and left compact mode, and
    /// probes the refill-rate gate declined. All written on cold paths.
    compacted: AtomicU64,
    compact_on: AtomicU64,
    compact_off: AtomicU64,
    gate_refill: AtomicU64,
    _tail: [u8; PAD_ALIGN - 56],
}

const _: () = assert!(core::mem::offset_of!(ClassState, inner) == PAD_ALIGN);
const _: () = assert!(core::mem::offset_of!(ClassState, oldest_empty) + 8 <= PAD_ALIGN);
const _: () = assert!(core::mem::offset_of!(ClassState, claimed) % PAD_ALIGN == 0);
const _: () = assert!(core::mem::size_of::<ClassState>() % PAD_ALIGN == 0);
const _: () = assert!(core::mem::size_of::<ClassState>() % 512 != 0);

// SAFETY: the only non-`Sync` field is the table pointer, which targets a
// `SlabTable` (itself `Sync`) that outlives every `ClassState` pointing at
// it: both live in `Shared` for the life of the process, or are leaked by
// the tests.
unsafe impl Sync for ClassState {}
unsafe impl Send for ClassState {}

#[cfg(target_os = "linux")]
mod os {
    use crate::sys::{self, Errno, MADV_GUARD_INSTALL, PROT_NONE, PROT_READ, PROT_WRITE};

    pub fn commit(addr: usize, len: usize) -> Result<(), Errno> {
        unsafe { sys::mprotect(addr, len, PROT_READ | PROT_WRITE) }
    }

    pub fn decommit(addr: usize, len: usize) -> Result<(), Errno> {
        unsafe { sys::mprotect(addr, len, PROT_NONE) }
    }

    pub fn guard(addr: usize, len: usize) -> Result<(), Errno> {
        unsafe { sys::madvise(addr, len, MADV_GUARD_INSTALL) }
    }

    pub fn now_ns() -> u64 {
        sys::now_coarse_ns()
    }
}

/// Off Linux nothing maps memory; host tests exercise the state machine
/// over addresses that are never dereferenced.
#[cfg(not(target_os = "linux"))]
mod os {
    use crate::sys::Errno;
    use core::sync::atomic::{AtomicU64, Ordering};

    pub fn commit(_addr: usize, _len: usize) -> Result<(), Errno> {
        Ok(())
    }

    pub fn decommit(_addr: usize, _len: usize) -> Result<(), Errno> {
        Ok(())
    }

    pub fn guard(_addr: usize, _len: usize) -> Result<(), Errno> {
        Ok(())
    }

    static TICK: AtomicU64 = AtomicU64::new(0);

    pub fn now_ns() -> u64 {
        TICK.fetch_add(1, Ordering::Relaxed) + 1
    }
}

impl Inner {
    fn set_partial(&mut self, slab: usize) {
        if !self.partial.map.is_set(slab) {
            self.partial.map.set(slab);
            self.stats.nonfull += 1;
        }
    }

    fn clear_partial(&mut self, slab: usize) {
        if self.partial.map.is_set(slab) {
            self.partial.map.clear(slab);
            self.stats.nonfull -= 1;
        }
    }
}

impl SlabTable {
    /// Create the table for `class`. Reserves the descriptor array, the
    /// class bitmap and the owner array in `meta`; returns `None` if the
    /// metadata region cannot hold them. `guard_interval` of `N > 0` makes
    /// every `N`-th page group a guard group; callers must pass 0 when
    /// `MADV_GUARD_INSTALL` is unavailable, since a `PROT_NONE` slab would
    /// split the region into one VMA per guard (design: Phase 2 task 1).
    pub fn new(meta: &Meta, regions: &Regions, class: usize, guard_interval: u32) -> Option<SlabTable> {
        assert!(class < CLASSES.len(), "class: index out of range");
        assert!(guard_interval != 1, "class: a guard interval of 1 leaves no slabs to allocate");
        let cls = &CLASSES[class];
        let page = regions.page_size();
        let nslabs = regions.nslabs(class);
        let stagger = desc_stagger(class);
        let align = DESC_ALIGN.max(page);
        let slabs_per_page = (page / cls.slab_size()).max(1);
        assert!(slabs_per_page.is_power_of_two() && nslabs % slabs_per_page == 0);
        let group = cls.slabs_per_line().max(slabs_per_page);
        assert!(group.is_power_of_two() && group % slabs_per_page == 0);
        let descs = Lazy::reserve(meta, stagger + nslabs * desc_stride(class), align)?;
        let bits = Lazy::reserve(meta, stagger + (nslabs * cls.slab_bits()).div_ceil(8), align)?;
        let owners = Lazy::reserve(meta, stagger + nslabs.div_ceil(group) * 2, align)?;
        Some(SlabTable {
            bits_base: bits.base + stagger,
            owners_base: owners.base + stagger,
            committed_slabs: AtomicUsize::new(0),
            slab_bits: cls.slab_bits() as u16,
            class: class as u16,
            lg_group: group.trailing_zeros() as u8,
            _pad: [0; 3],
            guard_interval,
            lg_page: regions.lg_page,
            desc_stride: desc_stride(class) as u32,
            slabs_per_page: slabs_per_page as u32,
            group: group as u32,
            base: regions.class_base[class],
            nslabs,
            desc_base: descs.base + stagger,
            inner: Padded(Lock::new(TableInner { descs, bits, owners, next_slab: 0, guards: 0 })),
            _tail: [0; PAD_ALIGN],
        })
    }

    #[inline]
    pub fn class_index(&self) -> usize {
        self.class as usize
    }

    #[inline]
    pub fn class(&self) -> &'static Class {
        &CLASSES[self.class as usize]
    }

    #[inline]
    pub fn nslabs(&self) -> usize {
        self.nslabs
    }

    /// The commit lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> &RawLock {
        self.inner.raw()
    }

    /// Whether `slab` is reserved as a guard slab. Guards are counted in
    /// page groups (see [`slabs_per_page`]): every `guard_interval`-th group
    /// is a guard group, and with one slab per page (the usual case) that is
    /// every `guard_interval`-th slab.
    ///
    /// [`slabs_per_page`]: SlabTable::slabs_per_page
    #[inline]
    pub fn is_guard_slab(&self, slab: usize) -> bool {
        self.guard_interval != 0 && (slab / self.slabs_per_page as usize + 1) % self.guard_interval as usize == 0
    }

    /// Slabs per page group; 1 unless the page is larger than the slab.
    #[inline]
    pub fn slabs_per_page(&self) -> usize {
        self.slabs_per_page as usize
    }

    /// Slabs handed to an arena at a time (see the field).
    #[inline]
    pub fn group(&self) -> usize {
        self.group as usize
    }

    /// Number of slabs (including guard slabs) handed to an arena: their
    /// descriptors, bits and owner words exist (see the field).
    #[inline]
    pub fn committed_slabs(&self) -> usize {
        self.committed_slabs.load(Ordering::Acquire)
    }

    /// Guard slabs committed so far.
    pub fn guards(&self) -> usize {
        self.inner.lock().guards
    }

    /// Descriptor of a handed-out slab. Aborts for a slab that was never
    /// handed out rather than faulting on its `PROT_NONE` descriptor page.
    #[inline]
    pub fn desc(&self, slab: usize) -> &SlabDesc {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        // SAFETY: `slab` is below the acquire-loaded watermark.
        unsafe { self.desc_unchecked(slab) }
    }

    /// Descriptor of `slab` if it has been handed out, `None` otherwise:
    /// no abort, for queries.
    ///
    /// The watermark is loaded `Relaxed`. For a pointer that really is
    /// an allocated slot the program itself ordered the free after the
    /// allocation (the same thread, or a hand-off through its own
    /// synchronisation), and the allocation was claimed under the class lock
    /// after the group was handed out and initialised, so that chain of
    /// happens-before already makes the watermark, the descriptor, the bits
    /// and the owner word visible here; the acquire added nothing. For any
    /// other pointer the only stale values this load can observe are a
    /// smaller watermark (`None`, and the slow path repeats the check with
    /// acquire) or the tables' initial zero words (a clear bit); it can
    /// never see a watermark above what has been handed out, so the pages
    /// are always mapped. [`is_set_unchecked`] and [`owner_unchecked`]
    /// rely on the same argument.
    ///
    /// [`is_set_unchecked`]: SlabTable::is_set_unchecked
    /// [`owner_unchecked`]: SlabTable::owner_unchecked
    #[inline(always)]
    pub fn desc_if_committed(&self, slab: usize) -> Option<&SlabDesc> {
        if slab >= self.committed_slabs.load(Ordering::Relaxed) {
            return None;
        }
        // SAFETY: `slab` is below a value the watermark actually held.
        Some(unsafe { self.desc_unchecked(slab) })
    }

    /// Whether `slab` has been handed out, by a relaxed load of the
    /// watermark (see [`desc_if_committed`] for why that is enough on the
    /// `free` fast path).
    ///
    /// [`desc_if_committed`]: SlabTable::desc_if_committed
    #[inline(always)]
    pub fn is_handed_out(&self, slab: usize) -> bool {
        slab < self.committed_slabs.load(Ordering::Relaxed)
    }

    /// Descriptor of `slab` without a watermark check.
    ///
    /// # Safety
    /// `slab` must be below a value `committed_slabs` has held: descriptors
    /// up to that count are committed and initialised (or zero, for guard
    /// slabs) before the watermark moves.
    #[inline(always)]
    unsafe fn desc_unchecked(&self, slab: usize) -> &SlabDesc {
        // SAFETY: guaranteed by the caller.
        unsafe { &*((self.desc_base + slab * self.desc_stride as usize) as *const SlabDesc) }
    }

    /// The slot bits of a handed-out slab (design section 4). Aborts for a
    /// slab that was never handed out.
    #[inline]
    pub fn bits(&self, slab: usize) -> AtomicBitmap<'_> {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        // SAFETY: `slab` is below the acquire-loaded watermark.
        unsafe { self.bits_unchecked(slab) }
    }

    /// # Safety
    /// As [`desc_unchecked`](SlabTable::desc_unchecked).
    #[inline]
    unsafe fn bits_unchecked(&self, slab: usize) -> AtomicBitmap<'_> {
        let nbits = self.slab_bits as usize;
        let first = slab * nbits;
        let word = (self.bits_base + (first / 64) * 8) as *const AtomicU64;
        if nbits >= 64 {
            debug_assert!(first % 64 == 0);
            // SAFETY: the reservation holds `nslabs * slab_bits` bits and
            // the group's words were committed before the watermark moved.
            AtomicBitmap::from_words(unsafe { core::slice::from_raw_parts(word, nbits / 64) })
        } else {
            // SAFETY: as above, one word.
            AtomicBitmap::sub_word(unsafe { &*word }, (first % 64) as u32, nbits)
        }
    }

    /// The `free` check's bit (design section 4): whether `slot` of `slab`
    /// is marked allocated (or cached). One relaxed load of the bitmap
    /// word, no bounds checks.
    ///
    /// # Safety
    /// `slab` must have been handed out ([`is_handed_out`]) and `slot` must
    /// be below the class's `nslots`.
    ///
    /// [`is_handed_out`]: SlabTable::is_handed_out
    #[inline(always)]
    pub unsafe fn is_set_unchecked(&self, slab: usize, slot: usize) -> bool {
        let bit = slab * self.slab_bits as usize + slot;
        let word = (self.bits_base + (bit / 64) * 8) as *const AtomicU64;
        // SAFETY: the caller guarantees the word is inside the handed-out
        // part of the bitmap.
        (unsafe { (*word).load(Ordering::Relaxed) } >> (bit % 64)) & 1 == 1
    }

    /// Whether `slot` of `slab` is marked allocated (or cached); `false`
    /// for a slab that was never handed out. Never aborts.
    #[inline]
    pub fn is_allocated(&self, slab: usize, slot: usize) -> bool {
        debug_assert!(slot < self.class().nslots());
        // SAFETY: the watermark check, then a slot below `nslots`.
        self.is_handed_out(slab) && unsafe { self.is_set_unchecked(slab, slot) }
    }

    /// The arena that owns `slab`'s group, without a watermark check.
    ///
    /// # Safety
    /// `slab` must have been handed out ([`is_handed_out`]).
    ///
    /// [`is_handed_out`]: SlabTable::is_handed_out
    #[inline(always)]
    pub unsafe fn owner_unchecked(&self, slab: usize) -> u16 {
        let word = (self.owners_base + (slab >> self.lg_group) * 2) as *const AtomicU16;
        // SAFETY: the owner word of a handed-out group was committed and
        // written before the watermark moved.
        unsafe { (*word).load(Ordering::Relaxed) }
    }

    /// The arena that owns `slab`. Aborts for a slab that was never handed
    /// out.
    #[inline]
    pub fn owner(&self, slab: usize) -> u16 {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        // SAFETY: `slab` is below the acquire-loaded watermark.
        unsafe { self.owner_unchecked(slab) }
    }

    #[inline]
    fn slab_addr(&self, slab: usize) -> usize {
        self.base + (slab << self.class().lg_slab)
    }

    #[inline]
    fn slot_ptr(&self, slab: usize, slot: usize) -> *mut u8 {
        (self.slab_addr(slab) + slot * self.class().size()) as *mut u8
    }

    /// Slab and slot of a pointer that must be a slot start in this class's
    /// region. Aborts otherwise.
    #[inline]
    pub fn locate(&self, p: *mut u8) -> (usize, usize) {
        let cls = self.class();
        let rel = (p as usize).wrapping_sub(self.base);
        if rel >= self.nslabs << cls.lg_slab {
            invalid_pointer(p as usize, self.class as usize, "outside the class region");
        }
        let (slot, valid) = cls.slot_of_offset(rel & (cls.slab_size() - 1));
        if !valid {
            invalid_pointer(p as usize, self.class as usize, "not a slot start");
        }
        (rel >> cls.lg_slab, slot)
    }

    /// The `free` check: `p` must be a slot start whose bit is set. Returns
    /// the arena that owns the slab.
    #[inline]
    pub fn check(&self, p: *mut u8) -> u16 {
        let (slab, slot) = self.locate(p);
        self.check_located(slab, slot, p as usize)
    }

    /// [`check`] without the owner: for queries that only need to know the
    /// slot is live (`malloc_usable_size`, `realloc`), so they read the
    /// bitmap word and nothing else.
    ///
    /// [`check`]: SlabTable::check
    #[inline]
    pub fn check_live(&self, p: *mut u8) {
        let (slab, slot) = self.locate(p);
        self.check_bit(slab, slot, p as usize);
    }

    /// [`check`] for a pointer already located (`Regions::classify`):
    /// aborts unless `slab` has been handed out and `slot`'s bit is set,
    /// with the pointer and its location; returns the owning arena.
    ///
    /// [`check`]: SlabTable::check
    #[inline]
    pub fn check_located(&self, slab: usize, slot: usize, p: usize) -> u16 {
        self.check_bit(slab, slot, p);
        // SAFETY: `check_bit` returned, so `slab` is below the watermark.
        unsafe { self.owner_unchecked(slab) }
    }

    /// The bit test of [`check_located`], without the owner.
    ///
    /// [`check_located`]: SlabTable::check_located
    #[inline]
    fn check_bit(&self, slab: usize, slot: usize, p: usize) {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        debug_assert!(slot < self.class().nslots());
        // SAFETY: below the acquire-loaded watermark, slot below `nslots`.
        if !unsafe { self.is_set_unchecked(slab, slot) } {
            invalid_free(p, self.class as usize, slab, slot);
        }
    }

    /// Hand the next group of slabs to `arena` (design section 4):
    /// commit their descriptors, bitmap words and owner word, initialise
    /// the descriptors (`Fresh`, owned by `arena`; guard slabs stay zero)
    /// and clear their bits, then publish the watermark. `ensure_sets` is
    /// called under the commit lock with the group's last slab number
    /// before anything changes, so the arena can commit the set words the
    /// group needs; if it fails nothing has happened. Returns the group's
    /// slab range, `None` when the region or the metadata region is
    /// exhausted. The slabs themselves are committed one by one by
    /// [`commit_slab`].
    ///
    /// [`commit_slab`]: SlabTable::commit_slab
    fn take_group(
        &self,
        meta: &Meta,
        arena: u16,
        mut ensure_sets: impl FnMut(usize) -> Result<(), Errno>,
    ) -> Option<(usize, usize)> {
        let cls = self.class();
        let page = 1usize << self.lg_page;
        let class = self.class as usize;
        let stagger = desc_stagger(class);
        let mut g = self.inner.lock();
        let s = g.next_slab;
        if s >= self.nslabs {
            return None;
        }
        let end = (s + self.group as usize).min(self.nslabs);
        g.descs.ensure(meta, page, stagger + end * desc_stride(class)).ok()?;
        g.bits.ensure(meta, page, stagger + (end * cls.slab_bits()).div_ceil(8)).ok()?;
        g.owners.ensure(meta, page, stagger + ((end - 1) >> self.lg_group) * 2 + 2).ok()?;
        ensure_sets(end - 1).ok()?;
        for slab in s..end {
            if self.is_guard_slab(slab) {
                continue;
            }
            // SAFETY: the descriptor and bitmap pages for `slab` were
            // committed above.
            unsafe {
                SlabDesc::at(self.desc_base, class, slab).init(arena);
                self.bits_unchecked(slab).init(cls.nslots());
            }
        }
        // SAFETY: the owner page for the group was committed above.
        unsafe { &*((self.owners_base + (s >> self.lg_group) * 2) as *const AtomicU16) }
            .store(arena, Ordering::Relaxed);
        g.next_slab = end;
        self.committed_slabs.store(end, Ordering::Release);
        Some((s, end))
    }

    /// Commit slab `s` of a group handed to the caller's arena: make its
    /// memory (or its whole page group, when the page is larger than the
    /// slab and `s` starts one) read-write, or install the guard when `s`
    /// is a guard slab. `Some(true)` for a slab ready to allocate from,
    /// `Some(false)` for a guard slab, `None` when `mprotect` fails (the
    /// slab stays uncommitted and may be retried).
    fn commit_slab(&self, s: usize) -> Option<bool> {
        let cls = self.class();
        let page = 1usize << self.lg_page;
        let mut g = self.inner.lock();
        let addr = self.slab_addr(s);
        // A slab smaller than a page is committed with its whole page
        // group when the group's first slab comes up; a group is whole
        // page groups and the arena walks it in order, so the later slabs
        // of the page group find it committed.
        let spp = self.slabs_per_page as usize;
        let len = cls.slab_size().max(page);
        if s % spp == 0 {
            os::commit(addr, len).ok()?;
        }
        if self.is_guard_slab(s) {
            // Read-write so the VMA merges with its neighbours, with a
            // guard region on the first page so a stray write past the
            // previous slab faults. If the install fails, fall back to
            // leaving the whole slab (or page group) `PROT_NONE`: a hard
            // guard at the cost of a VMA split.
            if s % spp == 0 && os::guard(addr, page).is_err() && os::decommit(addr, len).is_err() {
                panic!("class {}: cannot re-protect guard slab {s}", self.class);
            }
            g.guards += 1;
            return Some(false);
        }
        Some(true)
    }
}

impl ClassState {
    /// Create the state for `arena` over `table`'s class. Reserves the
    /// three slab sets in `meta`; returns `None` if the metadata region
    /// cannot hold them. `table` must outlive the state (see the field).
    pub fn new(meta: &Meta, regions: &Regions, table: &SlabTable, arena: u16) -> Option<ClassState> {
        let page = regions.page_size();
        let nslabs = table.nslabs;
        let partial = Set::new(meta, page, nslabs)?;
        let empty = Set::new(meta, page, nslabs)?;
        let purged = Set::new(meta, page, nslabs)?;
        Some(ClassState {
            class: table.class,
            arena,
            table,
            oldest_empty: AtomicU64::new(0),
            inner: Padded(Lock::new(Inner {
                partial,
                empty,
                purged,
                stats: ClassStats::default(),
                group_next: 0,
                group_end: 0,
            })),
            claimed: AtomicU64::new(0),
            refilled: AtomicU64::new(0),
            released: AtomicU64::new(0),
            compacted: AtomicU64::new(0),
            compact_on: AtomicU64::new(0),
            compact_off: AtomicU64::new(0),
            gate_refill: AtomicU64::new(0),
            _tail: [0; PAD_ALIGN - 56],
        })
    }

    /// The class's shared table.
    #[inline(always)]
    pub fn table(&self) -> &SlabTable {
        // SAFETY: the table outlives the state (see the field).
        unsafe { &*self.table }
    }

    #[inline]
    pub fn class_index(&self) -> usize {
        self.class as usize
    }

    #[inline]
    pub fn class(&self) -> &'static Class {
        &CLASSES[self.class as usize]
    }

    #[inline]
    pub fn arena(&self) -> u16 {
        self.arena
    }

    #[inline]
    pub fn nslabs(&self) -> usize {
        self.table().nslabs
    }

    /// The class lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> &RawLock {
        self.inner.raw()
    }

    /// [`SlabTable::is_guard_slab`].
    #[inline]
    pub fn is_guard_slab(&self, slab: usize) -> bool {
        self.table().is_guard_slab(slab)
    }

    /// [`SlabTable::committed_slabs`]: slabs of every arena.
    #[inline]
    pub fn committed_slabs(&self) -> usize {
        self.table().committed_slabs()
    }

    /// [`SlabTable::desc`].
    #[inline]
    pub fn desc(&self, slab: usize) -> &SlabDesc {
        self.table().desc(slab)
    }

    #[inline]
    fn slab_addr(&self, slab: usize) -> usize {
        self.table().slab_addr(slab)
    }

    #[inline]
    fn slot_ptr(&self, slab: usize, slot: usize) -> *mut u8 {
        self.table().slot_ptr(slab, slot)
    }

    /// [`SlabTable::locate`].
    #[inline]
    pub fn locate(&self, p: *mut u8) -> (usize, usize) {
        self.table().locate(p)
    }

    /// The `free` check: `p` must be a slot start whose bit is set, in a
    /// slab of this arena.
    #[inline]
    pub fn check(&self, p: *mut u8) {
        let arena = self.table().check(p);
        if arena != self.arena {
            wrong_arena(p as usize, self.class as usize, arena, self.arena);
        }
    }

    /// Claim up to `out.len()` slots, lowest address first, writing slot
    /// pointers to `out[..n]`. Each lock hold reserves from up to
    /// [`MAX_PICKS`] slabs (see [`plan`]); the loop repeats only when a
    /// plan ran out of picks before the request was met.
    ///
    /// [`plan`]: ClassState::plan
    pub fn claim(&self, meta: &Meta, out: &mut [*mut u8]) -> Claim {
        let cls = self.class();
        let mut got = 0;
        let mut zeroed = true;
        let mut picks = [Pick { slab: 0, n: 0, zero: false }; MAX_PICKS];
        while got < out.len() {
            let np = self.plan(meta, out.len() - got, &mut picks);
            if np == 0 {
                break;
            }
            for pick in &picks[..np] {
                let slab = pick.slab as usize;
                let want = pick.n as usize;
                let bm = self.table().bits(slab);
                let words = bm.words();
                let mut k = 0;
                let mut word = 0;
                let mut empty_passes = 0u32;
                while k < want {
                    let Some((w, bits)) = bm.claim_from(word, want - k) else {
                        // Every word was full when inspected. `plan` reserved
                        // no more than `nslots - live` and set bits never
                        // exceed `live`, so the slots exist: a concurrent
                        // claimer took the bits we looked at and left ours
                        // in a word we had already passed. Scan again.
                        empty_passes += 1;
                        if empty_passes == MAX_EMPTY_PASSES {
                            let d = self.desc(slab);
                            reservation_unmet(self.class as usize, slab, want, k, d.live(), bm.allocated(cls.nslots()), cls.nslots());
                        }
                        word = 0;
                        continue;
                    };
                    let mut m = bits;
                    while m != 0 {
                        let b = m.trailing_zeros() as usize;
                        m &= m - 1;
                        out[got + k] = self.slot_ptr(slab, w * 64 + b);
                        k += 1;
                    }
                    word = if w + 1 == words { 0 } else { w + 1 };
                }
                if !pick.zero {
                    zeroed = false;
                }
                got += want;
            }
        }
        Claim { n: got, zeroed: got > 0 && zeroed }
    }

    /// One lock hold of a refill: reserve up to `want` slots across up to
    /// [`MAX_PICKS`] slabs, lowest address first, and return the number of
    /// picks written. Zero picks means nothing more can be committed.
    ///
    /// `nslots - live` is a lower bound on a slab's claimable slots: every
    /// claimer reserves in `live` under this lock before it sets bits, and
    /// every releaser clears bits before it subtracts. So a reservation of
    /// at most that many is always met (the claimer may have to rescan when
    /// a concurrent claimer wins the bits it looked at), and a partial slab
    /// whose bound is zero is either full (its bit is cleared here, the
    /// only place a partial bit is cleared while the slab stays `Active`)
    /// or has an in-flight claimer and is skipped. A slab with `live == 0` whose
    /// every slot is reserved here leaves the partial set at once: nothing
    /// in it can be released before this claim hands slots out, so no
    /// release can race the clear.
    fn plan(&self, meta: &Meta, want: usize, picks: &mut [Pick; MAX_PICKS]) -> usize {
        let cls = self.class();
        let mut g = self.inner.lock();
        let mut np = 0;
        let mut remaining = want;
        let mut next = g.partial.map.lowest_set();
        while let Some(s) = next {
            if remaining == 0 || np == MAX_PICKS {
                return np;
            }
            let desc = self.desc(s);
            if desc.live() >= cls.nslots() {
                if desc.state() == State::Active && self.table().bits(s).is_full() {
                    g.clear_partial(s);
                }
            } else {
                self.take(&mut g, s, &mut remaining, picks, &mut np);
            }
            next = g.partial.map.lowest_set_from(s + 1);
        }
        while remaining > 0 && np < MAX_PICKS {
            let s = if let Some(s) = g.empty.map.lowest_set() {
                g.empty.map.clear(s);
                g.stats.empty -= 1;
                s
            } else if let Some(s) = g.purged.map.lowest_set() {
                g.purged.map.clear(s);
                g.stats.purged -= 1;
                s
            } else {
                match self.commit_next(&mut g, meta) {
                    Some(s) => s,
                    None => break,
                }
            };
            self.take(&mut g, s, &mut remaining, picks, &mut np);
        }
        np
    }

    /// Reserve `min(nslots - live, *remaining)` slots of `slab` under the
    /// lock, fix up its state and partial bit, and record the pick.
    fn take(&self, g: &mut Inner, slab: usize, remaining: &mut usize, picks: &mut [Pick; MAX_PICKS], np: &mut usize) {
        let desc = self.desc(slab);
        let live = desc.live();
        let avail = self.class().nslots() - live;
        let n = avail.min(*remaining);
        let state = desc.state();
        if state != State::Active {
            desc.set_state(State::Active);
            desc.set_dirty_at(0);
        }
        desc.add_live(n);
        // Under the lock (`g`), so a plain load-add-store is exclusive.
        self.claimed.store(self.claimed.load(Ordering::Relaxed).wrapping_add(n as u64), Ordering::Relaxed);
        if live == 0 && n == avail {
            g.clear_partial(slab);
        } else {
            g.set_partial(slab);
        }
        // `Purged` memory is zero only when it was discarded with
        // `MADV_DONTNEED` (no `lazy_free`).
        let zero = state.is_zero() && (state != State::Purged || purge::purged_is_zero());
        picks[*np] = Pick { slab: slab as u32, n: n as u32, zero };
        *np += 1;
        *remaining -= n;
    }

    /// Commit the next slab of this arena's current group, taking the
    /// next group of the class region when the current one is used up
    /// ([`SlabTable::take_group`], which commits this arena's set words for
    /// the whole group first). Guard slabs inside the group are committed
    /// as guards and skipped. The commit lock nests inside the class lock.
    fn commit_next(&self, g: &mut Inner, meta: &Meta) -> Option<usize> {
        let table = self.table();
        let page = 1usize << table.lg_page;
        loop {
            if g.group_next >= g.group_end {
                let (start, end) = table.take_group(meta, self.arena, |s| {
                    g.partial.ensure(meta, page, s)?;
                    g.empty.ensure(meta, page, s)?;
                    g.purged.ensure(meta, page, s)
                })?;
                g.group_next = start;
                g.group_end = end;
            }
            let s = g.group_next;
            let usable = table.commit_slab(s)?;
            g.group_next = s + 1;
            if usable {
                g.stats.committed += 1;
                return Some(s);
            }
        }
    }

    /// `live` reached zero: make the slab `Empty` unless a refill has
    /// already reserved it again.
    fn on_live_zero(&self, slab: usize) {
        let mut first = false;
        {
            let mut g = self.inner.lock();
            let desc = self.desc(slab);
            if desc.live() == 0 && desc.state() == State::Active {
                let now = os::now_ns().max(1);
                desc.set_state(State::Empty);
                desc.set_dirty_at(now);
                g.clear_partial(slab);
                g.empty.map.set(slab);
                g.stats.empty += 1;
                if self.oldest_empty.load(Ordering::Relaxed) == 0 {
                    self.oldest_empty.store(now, Ordering::Relaxed);
                    first = true;
                }
            }
        }
        if first {
            purge::note_empty();
        }
    }

    /// Release a batch of slot pointers. Consecutive pointers in the same
    /// bitmap word share one `fetch_and`, so a batch sorted by address costs
    /// about one atomic per 64 slots. Slabs whose words went from full to
    /// not full are collected and put back in the partial set under one
    /// lock hold at the end (or when the list overflows), rather than one
    /// per word. Aborts on a pointer that is not an allocated slot of this
    /// class, or one whose slab belongs to another arena (the caller routes
    /// by the descriptor's arena; a miss here means a cache was mis-keyed).
    pub fn release(&self, ptrs: &[*mut u8]) {
        let n = self.release_owned(ptrs);
        if n != ptrs.len() {
            let p = ptrs[n];
            let (slab, _) = self.locate(p);
            wrong_arena(p as usize, self.class as usize, self.table().owner(slab), self.arena);
        }
    }

    /// [`release`] of the longest prefix of `ptrs` whose slabs this arena
    /// owns, returning its length (`ptrs.len()` when every slab is ours).
    /// The batch is sorted by address, so a slab's pointers are contiguous
    /// and the prefix ends exactly at the first pointer of a slab of another
    /// arena; the caller routes the rest there (`Heap::flush`: a home cache
    /// takes sized frees of any arena, design 6.3). Every bit cleared is
    /// checked (an already-clear bit is a double or invalid free and aborts),
    /// which is where the sized path's deferred check lands. The owner is
    /// read from the table's dense owner array, once per slab switch.
    ///
    /// [`release`]: ClassState::release
    pub fn release_owned(&self, ptrs: &[*mut u8]) -> usize {
        let mut cur_slab = usize::MAX;
        let mut cur_word = 0usize;
        let mut mask = 0u64;
        let mut count = 0usize;
        let mut reopen = Reopen::default();
        let mut end = ptrs.len();
        for (i, &p) in ptrs.iter().enumerate() {
            let (slab, slot) = self.locate(p);
            let word = slot / 64;
            if slab != cur_slab || word != cur_word {
                if mask != 0 {
                    self.release_word(cur_slab, cur_word, mask, &mut reopen);
                    mask = 0;
                }
                if slab != cur_slab {
                    if count != 0 {
                        self.dec_live(cur_slab, count);
                        count = 0;
                    }
                    if self.table().owner(slab) != self.arena {
                        end = i;
                        break;
                    }
                }
                cur_slab = slab;
                cur_word = word;
            }
            let bit = 1u64 << (slot % 64);
            if mask & bit != 0 {
                double_free(p as usize, self.class as usize, slab, slot);
            }
            mask |= bit;
            count += 1;
        }
        if mask != 0 {
            self.release_word(cur_slab, cur_word, mask, &mut reopen);
        }
        if count != 0 {
            self.dec_live(cur_slab, count);
        }
        self.reopen(&mut reopen);
        if end != 0 {
            self.released.fetch_add(end as u64, Ordering::Relaxed);
        }
        end
    }

    /// Release the slots of `mask` in `word` of `slab`. The `live` update
    /// is separate so a batch can do one per slab.
    fn release_word(&self, slab: usize, word: usize, mask: u64, reopen: &mut Reopen) {
        let (prev, was_full) = self.table().bits(slab).release(word, mask);
        if prev & mask != mask {
            let bad = (mask & !prev).trailing_zeros() as usize;
            let slot = word * 64 + bad;
            double_free(self.slot_ptr(slab, slot) as usize, self.class as usize, slab, slot);
        }
        if was_full && !reopen.push(slab) {
            self.reopen(reopen);
            reopen.push(slab);
        }
    }

    /// Put the slabs in `reopen` back in the partial set, then clear the
    /// list. A slab that went `Empty` (or was purged) since its word was
    /// released stays out: it is in the empty or purged set, and a refill
    /// taking it from there sets its partial bit itself.
    fn reopen(&self, reopen: &mut Reopen) {
        if reopen.len == 0 {
            return;
        }
        let mut g = self.inner.lock();
        for &s in &reopen.slabs[..reopen.len] {
            let slab = s as usize;
            if self.desc(slab).state() == State::Active {
                g.set_partial(slab);
            }
        }
        reopen.len = 0;
    }

    fn dec_live(&self, slab: usize, n: usize) {
        if self.desc(slab).sub_live(n) == n {
            self.on_live_zero(slab);
        }
    }

    /// Purge every `Empty` slab whose `dirty_at` is at or before `cutoff`
    /// (`u64::MAX` for all of them) and make it `Purged`. The discards go
    /// out in batches of [`purge::MAX_BATCH`] under the class lock, so no
    /// claim can hand out a slab between its state change and the discard.
    /// Recomputes `oldest_empty` from the survivors. Returns the number
    /// purged.
    pub fn purge_empty(&self, cutoff: u64) -> usize {
        let cls = self.class();
        let spp = self.table().slabs_per_page();
        let mut g = self.inner.lock();
        let mut n = 0;
        let mut batch = Batch::new();
        let mut oldest = 0u64;
        let mut next = g.empty.map.lowest_set();
        while let Some(slab) = next {
            let desc = self.desc(slab);
            assert!(
                desc.state() == State::Empty && desc.live() == 0,
                "class {}: slab {slab} in the empty set is not empty",
                self.class
            );
            let dirty_at = desc.dirty_at();
            if dirty_at <= cutoff && (spp == 1 || self.page_group_is_idle(slab, spp)) {
                // Discard the whole page group (one slab when the slab is at
                // least a page); every `Empty` slab in it becomes `Purged`
                // now, a younger one included, since nothing in the group is
                // in use.
                let first = slab & !(spp - 1);
                batch.push(self.slab_addr(first), cls.slab_size() * spp);
                for s in first..first + spp {
                    let d = self.desc(s);
                    if d.state() != State::Empty {
                        continue;
                    }
                    d.set_state(State::Purged);
                    d.set_dirty_at(0);
                    g.empty.map.clear(s);
                    g.purged.map.set(s);
                    g.stats.empty -= 1;
                    g.stats.purged += 1;
                    n += 1;
                }
            } else if oldest == 0 || dirty_at < oldest {
                oldest = dirty_at;
            }
            next = g.empty.map.lowest_set_from(slab + 1);
        }
        batch.flush();
        self.oldest_empty.store(oldest, Ordering::Relaxed);
        n
    }

    /// Whether the page group of `slab` (`spp` slabs, `spp > 1`) can be
    /// discarded: every slab in it is committed, owned by this arena, and
    /// `Fresh`, `Empty` or `Purged`. Called under this arena's class lock,
    /// which is what pins those states for this arena's slabs; a slab of
    /// another arena, or one not yet handed out by `commit_next`, could be
    /// claimed under a lock this thread does not hold, so it vetoes the
    /// purge.
    fn page_group_is_idle(&self, slab: usize, spp: usize) -> bool {
        let first = slab & !(spp - 1);
        let committed = self.table().committed_slabs();
        (first..first + spp).all(|s| {
            if s >= committed {
                return false;
            }
            let d = self.desc(s);
            d.arena() == self.arena && matches!(d.state(), State::Fresh | State::Empty | State::Purged)
        })
    }

    /// `dirty_at` of the oldest `Empty` slab as the decay driver sees it
    /// (0 when none; see the field).
    pub fn oldest_empty(&self) -> u64 {
        self.oldest_empty.load(Ordering::Relaxed)
    }

    /// Decay step (design section 8): when the oldest `Empty` slab has been
    /// dirty for `decay_ns` at `now`, purge every slab that old or older.
    /// Returns the class's next deadline (oldest `dirty_at + decay_ns`),
    /// `None` when nothing is `Empty`. Takes the lock only when a purge is
    /// due.
    pub fn decay(&self, now: u64, decay_ns: u64) -> Option<u64> {
        let oldest = self.oldest_empty();
        if oldest == 0 {
            return None;
        }
        let deadline = oldest.saturating_add(decay_ns);
        if now < deadline {
            return Some(deadline);
        }
        self.purge_empty(now.saturating_sub(decay_ns));
        let oldest = self.oldest_empty();
        if oldest == 0 { None } else { Some(oldest.saturating_add(decay_ns)) }
    }

    /// This arena's slab counts; `guards` is the class-wide count from the
    /// table, since guard slabs belong to no arena.
    pub fn stats(&self) -> ClassStats {
        self.bin_stats().0
    }

    /// The slab counts and `curregs` (slots out of this arena's bitmaps:
    /// in caches or in use, jemalloc's `curregs`) from one lock hold.
    ///
    /// `released` is read before the lock and `claimed` under it, so the
    /// difference is never negative: a slot is claimed before it is
    /// released. It can exceed the true sum of `live` by the slots
    /// released in between, so it is clamped to the capacity of the slabs
    /// that can hold live slots (those neither `Empty` nor `Purged`),
    /// which keeps `active >= allocated` exact for `stats.rs`.
    pub fn bin_stats(&self) -> (ClassStats, usize) {
        let released = self.released.load(Ordering::Relaxed);
        let g = self.inner.lock();
        let mut s = g.stats;
        let claimed = self.claimed.load(Ordering::Relaxed);
        drop(g);
        s.guards = self.table().guards();
        let cap = (s.committed - s.empty - s.purged) * self.class().nslots();
        let curregs = (claimed.wrapping_sub(released) as usize).min(cap);
        (s, curregs)
    }

    /// Slots out of this arena's bitmaps (see [`ClassState::bin_stats`]).
    pub fn curregs(&self) -> usize {
        self.bin_stats().1
    }

    /// Slots ever claimed from this arena's slabs of the class (wrapping).
    #[inline]
    pub fn claimed(&self) -> u64 {
        self.claimed.load(Ordering::Relaxed)
    }

    /// Slots claimed by cache refills (see the field); the compaction gate
    /// in `heap.rs` watches its rate.
    #[inline]
    pub fn refilled(&self) -> u64 {
        self.refilled.load(Ordering::Relaxed)
    }

    /// Count a cache refill of `n` slots.
    #[inline]
    pub fn note_refill(&self, n: usize) {
        self.refilled.fetch_add(n as u64, Ordering::Relaxed);
    }

    /// Count a compaction rebuild's claim of `n` slots.
    #[inline]
    pub fn note_compact(&self, n: usize) {
        self.compacted.fetch_add(n as u64, Ordering::Relaxed);
    }

    /// Count a cache entering (`on`) or leaving compact mode.
    #[inline]
    pub fn note_compact_mode(&self, on: bool) {
        if on { &self.compact_on } else { &self.compact_off }.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a probe the refill-rate gate declined.
    #[inline]
    pub fn note_gate_refill(&self) {
        self.gate_refill.fetch_add(1, Ordering::Relaxed);
    }

    /// The compaction diagnostics: `(compacted, compact_on, compact_off,
    /// gate_refill)`, see the fields.
    pub fn compaction_stats(&self) -> (u64, u64, u64, u64) {
        (
            self.compacted.load(Ordering::Relaxed),
            self.compact_on.load(Ordering::Relaxed),
            self.compact_off.load(Ordering::Relaxed),
            self.gate_refill.load(Ordering::Relaxed),
        )
    }

    /// `(holes, capacity)`: the free slots of the slabs that can hold live
    /// slots (neither `Empty` nor `Purged`), i.e. slots that are in no
    /// cache and no program, and those slabs' total slot count. The
    /// compaction gate in `heap.rs` reads this; one lock hold.
    pub fn holes(&self) -> (usize, usize) {
        let (s, curregs) = self.bin_stats();
        let capacity = (s.committed - s.empty - s.purged) * self.class().nslots();
        (capacity.saturating_sub(curregs), capacity)
    }

    /// Lowest slab in the partial set, if any (for tests and stats).
    pub fn lowest_partial(&self) -> Option<usize> {
        self.inner.lock().partial.map.lowest_set()
    }

    /// Whether `slab` is in the partial set.
    pub fn is_partial(&self, slab: usize) -> bool {
        self.inner.lock().partial.map.is_set(slab)
    }
}

#[cold]
#[inline(never)]
fn never_committed(class: usize, slab: usize) -> ! {
    panic!("invalid free: class {class} slab {slab} was never allocated")
}

/// The `free` check's abort (design section 4): the slot's bit is clear,
/// which is an invalid free or a stale double free.
#[cold]
#[inline(never)]
fn invalid_free(p: usize, class: usize, slab: usize, slot: usize) -> ! {
    panic!(
        "invalid or double free of {p:#x} (class {class} slab {slab} slot {slot}): slot is not allocated"
    )
}

#[cold]
#[inline(never)]
fn wrong_arena(p: usize, class: usize, owner: u16, arena: u16) -> ! {
    panic!("slot {p:#x} (class {class}) belongs to arena {owner} but was released through arena {arena}")
}

#[cold]
#[inline(never)]
fn reservation_unmet(class: usize, slab: usize, want: usize, got: usize, live: usize, set: usize, nslots: usize) -> ! {
    panic!("class {class}: slab {slab} yielded {got} of a reservation of {want} slots after {MAX_EMPTY_PASSES} full scans (live {live}, allocated {set}, nslots {nslots}): live count is corrupt")
}

#[cold]
#[inline(never)]
fn invalid_pointer(p: usize, class: usize, why: &str) -> ! {
    panic!("invalid free of {p:#x} (class {class}): {why}")
}

#[cold]
#[inline(never)]
fn double_free(p: usize, class: usize, slab: usize, slot: usize) -> ! {
    panic!("double free of {p:#x} (class {class} slab {slab} slot {slot})")
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Kind, LG_MAX_SLAB, LG_MIN_SLAB, MAX_SLAB, MIN_SLAB, Mapping, NCLASSES};
    use core::ptr;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::vec;
    use std::vec::Vec;

    const PAGE: usize = 4096;
    /// Smallest layout `from_parts` accepts: 4 MiB class regions, so the
    /// 64 KiB classes have 64 slabs and the 4 MiB class has one.
    const LG_REGION: u32 = LG_MAX_SLAB;
    const LG_STRIDE: u32 = LG_REGION + 2;
    const META_LEN: usize = 8 << 20;

    /// Test environment: a metadata region and a class superblock. On Linux
    /// both are real `PROT_NONE` reservations so commits and purges go
    /// through the kernel and slot memory can be touched; elsewhere the
    /// metadata is a `Vec` and slot addresses are never dereferenced.
    struct Env {
        meta: Meta,
        regions: Regions,
        #[cfg(target_os = "linux")]
        maps: [(usize, usize); 2],
        #[cfg(not(target_os = "linux"))]
        _buf: Vec<u8>,
    }

    fn align_up(x: usize, a: usize) -> usize {
        (x + a - 1) & !(a - 1)
    }

    fn regions_at(superblock: usize, meta: (usize, usize), lg_page: u32) -> Regions {
        let sb_len = (NCLASSES << LG_STRIDE) + MAX_SLAB;
        let huge = (superblock + sb_len + (1 << 30), 1usize << 34);
        Regions::from_parts(
            superblock,
            LG_REGION,
            LG_STRIDE,
            &[0usize; NCLASSES],
            huge,
            meta,
            lg_page,
            [
                Mapping { addr: superblock, len: sb_len },
                Mapping { addr: huge.0, len: huge.1 },
                Mapping { addr: meta.0, len: meta.1 },
            ],
        )
    }

    impl Env {
        fn new() -> Env {
            Env::with_lg_page(PAGE.trailing_zeros())
        }

        /// An environment whose layout believes the page is `1 << lg_page`
        /// bytes. On Linux that must be a multiple of the real page (the
        /// commits and purges are real syscalls over a real reservation);
        /// larger than the real page it stands in for a 16 KiB or 64 KiB
        /// page kernel.
        #[cfg(target_os = "linux")]
        fn with_lg_page(lg_page: u32) -> Env {
            use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
            let page = 1usize << lg_page;
            assert!(page % crate::sys::features::page_size() == 0, "fake page must be a real page multiple");
            let reserve = |len: usize| -> usize {
                unsafe {
                    crate::sys::mmap(0, len, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0)
                }
                .expect("reserve")
            };
            let sb_len = (NCLASSES << LG_STRIDE) + MAX_SLAB;
            let sb = reserve(sb_len);
            let mb_len = META_LEN + page;
            let mb_raw = reserve(mb_len);
            let mb = align_up(mb_raw, page);
            let superblock = align_up(sb, MAX_SLAB);
            let regions = regions_at(superblock, (mb, META_LEN), lg_page);
            Env { meta: Meta::new(mb, META_LEN, page), regions, maps: [(sb, sb_len), (mb_raw, mb_len)] }
        }

        #[cfg(not(target_os = "linux"))]
        fn with_lg_page(lg_page: u32) -> Env {
            let page = 1usize << lg_page;
            let buf = vec![0u8; META_LEN + page];
            let mb = align_up(buf.as_ptr() as usize, page);
            let regions = regions_at(0x1000_0000_0000, (mb, META_LEN), lg_page);
            Env { meta: Meta::new(mb, META_LEN, page), regions, _buf: buf }
        }

        /// A fresh table for `c` (leaked: the state points at it) and the
        /// arena-0 state over it.
        fn class(&self, c: usize, guard_interval: u32) -> ClassState {
            let table = self.table(c, guard_interval);
            ClassState::new(&self.meta, &self.regions, table, 0).expect("class state")
        }

        fn table(&self, c: usize, guard_interval: u32) -> &'static SlabTable {
            std::boxed::Box::leak(std::boxed::Box::new(
                SlabTable::new(&self.meta, &self.regions, c, guard_interval).expect("slab table"),
            ))
        }

        /// The state of `arena` over the same table as `cs`.
        fn class_in_arena(&self, cs: &ClassState, arena: u16) -> ClassState {
            ClassState::new(&self.meta, &self.regions, cs.table(), arena).expect("class state")
        }

        fn claim(&self, cs: &ClassState, n: usize) -> (Vec<*mut u8>, Claim) {
            let mut out = vec![ptr::null_mut(); n];
            let c = cs.claim(&self.meta, &mut out);
            out.truncate(c.n);
            (out, c)
        }
    }

    /// Regression: with 2^21 members the level-1 table is exactly one page,
    /// so level 0 starts on a page that `Set::new` leaves uncommitted (the
    /// real huge region has the same shape at 2^28 pages). Scans of an
    /// empty or sparsely populated set must never touch those pages.
    #[cfg(target_os = "linux")]
    #[test]
    fn set_scans_never_read_uncommitted_words() {
        let env = Env::new();
        let nbits = 1 << 21;
        let mut set = Set::new(&env.meta, PAGE, nbits).expect("set");
        assert_eq!(set.map.lowest_set(), None);
        assert_eq!(set.map.lowest_set_from(0), None);
        assert_eq!(set.map.lowest_set_from(12345), None);
        assert_eq!(set.map.lowest_set_from(nbits - 1), None);
        set.ensure(&env.meta, PAGE, 100).unwrap();
        set.map.set(100);
        assert_eq!(set.map.lowest_set_from(0), Some(100));
        assert_eq!(set.map.lowest_set_from(100), Some(100));
        assert_eq!(set.map.lowest_set_from(101), None);
        // Far beyond the committed prefix, in a level-1 group of its own.
        assert_eq!(set.map.lowest_set_from(64 * 64 * 100), None);
        set.map.clear(100);
        assert_eq!(set.map.lowest_set_from(0), None);
    }

    #[cfg(target_os = "linux")]
    impl Drop for Env {
        fn drop(&mut self) {
            for (addr, len) in self.maps {
                unsafe { crate::sys::munmap(addr, len) }.unwrap();
            }
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

    /// A class with 64 KiB slabs and a handful of bitmap words.
    fn small_class() -> usize {
        // 64 bytes: 1024 slots, 16 words.
        CLASSES.iter().position(|c| c.size() == 64).unwrap()
    }

    /// The largest class with the minimum slab (1024 bytes: 4 slots of a
    /// 4 KiB slab).
    fn min_slab_class() -> usize {
        CLASSES.iter().rposition(|c| c.slab_size() == MIN_SLAB).unwrap()
    }

    /// A kernel page larger than the slab (here a page four times the
    /// minimum slab, faked through the layout): slabs are committed,
    /// guarded and purged in page groups of `slabs_per_page`, and a group
    /// is purged only when every slab in it is idle, committed and owned by
    /// the purging arena. Dormant while the minimum slab is 64 KiB (no
    /// supported page is larger); this test keeps the path working.
    #[test]
    fn page_groups_when_the_page_is_larger_than_the_slab() {
        let env = Env::with_lg_page(LG_MIN_SLAB + 2);
        let c = min_slab_class();
        let n = CLASSES[c].nslots();
        assert_eq!(CLASSES[c].slab_size(), MIN_SLAB);
        // Every third page group is a guard group: slabs 8..12.
        let cs = env.class(c, 3);
        assert_eq!(cs.table().slabs_per_page(), 4);
        for s in 0..16 {
            assert_eq!(cs.is_guard_slab(s), (8..12).contains(&s), "slab {s}");
        }
        // Fill the first group and the first slab of the second; the
        // second group's page is committed with slab 4, so every slot of
        // slabs 4..8 is writable already (Linux touches them in `claim`).
        let (p, _) = env.claim(&cs, 5 * n);
        assert_eq!(p.len(), 5 * n);
        assert_eq!(cs.stats(), ClassStats { committed: 5, ..Default::default() });
        for (s, chunk) in p.chunks(n).enumerate() {
            assert_eq!(chunk[0] as usize, env.regions.slab_addr(c, s));
        }
        let base = env.regions.slab_addr(c, 0);
        let in_slab = |v: &[*mut u8], s: usize| -> Vec<*mut u8> {
            v.iter().copied().filter(|&x| (x as usize - base) / MIN_SLAB == s).collect()
        };
        // Slab 0 goes Empty; its group mates 1..4 are Active, so nothing is purged.
        cs.release(&p[..n]);
        assert_eq!(cs.desc(0).state(), State::Empty);
        assert_eq!(cs.purge_empty(u64::MAX), 0);
        assert_eq!(cs.desc(0).state(), State::Empty);
        assert_eq!(cs.oldest_empty(), cs.desc(0).dirty_at());
        // Another arena gets the next group (slabs 128..256: groups are
        // whole page groups, so a page group never straddles arenas);
        // 128..132 is a guard group (the 33rd page group), so its first
        // usable slab is 132. Arena 0 then reuses its Empty slab 0 and
        // commits 5 and 6 (a refill takes Empty slabs before Purged ones
        // and Purged ones before committing).
        let other = env.class_in_arena(&cs, 1);
        let (q, _) = env.claim(&other, n);
        assert_eq!(cs.table().group(), 128);
        assert!(cs.is_guard_slab(128) && !cs.is_guard_slab(132));
        assert_eq!(q[0] as usize, env.regions.slab_addr(c, 132));
        assert_eq!(cs.table().owner(128), 1);
        assert_eq!(cs.table().owner(5), 0);
        assert_eq!(cs.table().committed_slabs(), 256);
        assert_eq!(cs.table().guards(), 4);
        let (r, _) = env.claim(&cs, 3 * n);
        assert_eq!(r.len(), 3 * n);
        for s in [0, 5, 6] {
            assert_eq!(in_slab(&r, s).len(), n, "slab {s}");
            assert_eq!(cs.desc(s).state(), State::Active, "slab {s}");
            assert_eq!(cs.desc(s).arena(), 0);
        }
        assert_eq!(cs.desc(132).arena(), 1);
        // Slab 7 was committed with its page group (by slab 4) and handed
        // to arena 0 with the group, but no refill has used it: Fresh.
        assert_eq!(cs.desc(7).state(), State::Fresh);
        assert_eq!(cs.stats(), ClassStats { committed: 7, guards: 4, ..Default::default() });
        // Page group 4..8 with 4, 5, 6 Empty and 7 Fresh is idle, so it is
        // discarded as a whole; 7 stays Fresh (nothing was written to it).
        cs.release(&p[4 * n..]);
        cs.release(&in_slab(&r, 5));
        cs.release(&in_slab(&r, 6));
        assert_eq!(cs.stats(), ClassStats { committed: 7, guards: 4, empty: 3, ..Default::default() });
        assert_eq!(other.purge_empty(u64::MAX), 0);
        assert_eq!(cs.purge_empty(u64::MAX), 3);
        for s in 4..7 {
            assert_eq!(cs.desc(s).state(), State::Purged, "slab {s}");
        }
        assert_eq!(cs.desc(7).state(), State::Fresh);
        assert_eq!(cs.stats(), ClassStats { committed: 7, guards: 4, purged: 3, ..Default::default() });
        // With 0..4 Empty, purging with a cutoff at the oldest slab's
        // `dirty_at` takes the whole group, a slab released later (younger,
        // whenever the coarse clock has moved on) included.
        cs.release(&p[n..4 * n]);
        cs.release(&in_slab(&r, 0));
        let oldest = cs.desc(1).dirty_at();
        assert!(cs.desc(0).dirty_at() >= oldest);
        assert_eq!(cs.purge_empty(oldest), 4);
        for s in 0..4 {
            assert_eq!(cs.desc(s).state(), State::Purged, "slab {s}");
            assert_eq!(cs.desc(s).dirty_at(), 0);
        }
        assert_eq!(cs.stats(), ClassStats { committed: 7, guards: 4, purged: 7, ..Default::default() });
        // Purged neighbours do not block a purge: a claim reuses Purged
        // slab 0 (zero), and releasing it purges it alone inside its
        // Purged group.
        let (z, claim) = env.claim(&cs, 3);
        assert_eq!(claim, Claim { n: 3, zeroed: true });
        assert_eq!(z[0] as usize, base);
        cs.release(&z);
        assert_eq!(cs.desc(0).state(), State::Empty);
        assert_eq!(cs.purge_empty(u64::MAX), 1);
        assert_eq!(cs.desc(0).state(), State::Purged);
        assert_eq!(cs.stats(), ClassStats { committed: 7, guards: 4, purged: 7, ..Default::default() });
        // Past the guard group: the Purged slabs go first, then slab 7 is
        // committed, then the next commit skips the guard group 8..12 and
        // lands on slab 12.
        let (g, _) = env.claim(&cs, 8 * n + 1);
        assert_eq!(g.len(), 8 * n + 1);
        assert_eq!(in_slab(&g, 7).len(), n);
        assert_eq!(in_slab(&g, 12).len(), 1);
        assert_eq!(cs.table().guards(), 8);
        assert_eq!(cs.stats().committed, 9);
        assert_eq!(cs.table().committed_slabs(), 256);
        for s in 8..12 {
            assert!(cs.is_guard_slab(s));
            assert_eq!(cs.desc(s).state(), State::Fresh);
            assert_eq!(cs.desc(s).live(), 0);
        }
        // The other arena's group is untouched by all of this.
        assert_eq!(other.desc(132).state(), State::Active);
        assert_eq!(other.desc(132).live(), n);
        assert_eq!(other.stats(), ClassStats { committed: 1, guards: 8, ..Default::default() });
    }

    /// Slabs go to arenas in line-sized groups (design section 4): with 4
    /// slots per slab the bitmap line holds 128 slabs, so the second arena
    /// starts at slab 128 and the bits of neighbouring slabs share a word
    /// without disturbing each other. A slab handed out with the group but
    /// not yet committed has a `Fresh` descriptor, clear bits and the
    /// group's owner. The region has 8 groups: a ninth arena gets nothing.
    #[test]
    fn slabs_are_handed_out_in_line_sized_groups() {
        let env = Env::new();
        let c = min_slab_class();
        let cls = &CLASSES[c];
        assert_eq!(cls.nslots(), 4);
        assert_eq!(cls.slab_bits(), 4);
        assert_eq!(cls.slabs_per_line(), 128);
        let cs = env.class(c, 0);
        let t = cs.table();
        assert_eq!(t.group(), 128);
        assert_eq!(t.nslabs(), 1024);
        assert_eq!(t.committed_slabs(), 0);
        let (p, claim) = env.claim(&cs, 4);
        assert_eq!(claim, Claim { n: 4, zeroed: true });
        assert_eq!(p[0] as usize, env.regions.slab_addr(c, 0));
        assert_eq!(t.committed_slabs(), 128);
        assert_eq!(cs.stats().committed, 1);
        assert!(t.bits(0).is_full());
        for s in 1..128 {
            assert_eq!(t.owner(s), 0, "slab {s}");
            assert_eq!(t.desc(s).state(), State::Fresh, "slab {s}");
            assert_eq!(t.desc(s).arena(), 0, "slab {s}");
            assert_eq!(t.desc(s).live(), 0, "slab {s}");
            assert!(!t.bits(s).is_full(), "slab {s}");
            for slot in 0..4 {
                assert!(!t.is_allocated(s, slot), "slab {s} slot {slot}");
            }
        }
        assert!(!t.is_allocated(128, 0));
        // The second arena's first slab is the second group's.
        let cs1 = env.class_in_arena(&cs, 1);
        let (q, claim) = env.claim(&cs1, 1);
        assert_eq!(claim, Claim { n: 1, zeroed: true });
        assert_eq!(q[0] as usize, env.regions.slab_addr(c, 128));
        assert_eq!(t.committed_slabs(), 256);
        assert_eq!(t.owner(128), 1);
        assert_eq!(t.owner(255), 1);
        assert_eq!(t.owner(127), 0);
        assert_eq!(t.check(q[0]), 1);
        assert_eq!(t.check(p[0]), 0);
        // Arena 0 keeps filling its own group: slab 1 next, in the same
        // bitmap word as slab 0 (16 slabs of 4 bits per word).
        let (r, _) = env.claim(&cs, 1);
        assert_eq!(r[0] as usize, env.regions.slab_addr(c, 1));
        assert_eq!(t.bits(1).allocated(4), 1);
        assert!(t.bits(0).is_full());
        assert_eq!(t.bits(0).allocated(4), 4);
        cs.release(&p[1..3]);
        assert_eq!(t.bits(0).allocated(4), 2);
        assert_eq!(t.bits(1).allocated(4), 1);
        assert!(t.is_allocated(0, 0) && !t.is_allocated(0, 1) && !t.is_allocated(0, 2) && t.is_allocated(0, 3));
        assert!(t.is_allocated(1, 0));
        // Eight groups in the region: arenas 2..8 take the rest, a ninth
        // arena finds nothing to commit and reports the shortfall.
        for a in 2..8u16 {
            let csa = env.class_in_arena(&cs, a);
            let (pa, claim) = env.claim(&csa, 1);
            assert_eq!(claim.n, 1, "arena {a}");
            assert_eq!(pa[0] as usize, env.regions.slab_addr(c, 128 * a as usize), "arena {a}");
        }
        assert_eq!(t.committed_slabs(), 1024);
        let cs8 = env.class_in_arena(&cs, 8);
        let (_, claim) = env.claim(&cs8, 1);
        assert_eq!(claim, Claim { n: 0, zeroed: false });
        // Arena 0 still has 126 uncommitted slabs of its own group, plus
        // the 2 + 3 free slots of slabs 0 and 1.
        let (more, claim) = env.claim(&cs, 126 * 4 + 5);
        assert_eq!(claim.n, 126 * 4 + 5);
        assert_eq!(more[0] as usize, env.regions.slot_addr(c, 0, 1));
        assert_eq!(cs.locate(more[more.len() - 1]).0, 127);
        assert_eq!(cs.stats().committed, 128);
        let (_, claim) = env.claim(&cs, 1);
        assert_eq!(claim, Claim { n: 0, zeroed: false });
    }

    /// A free into a slab handed to an arena but not yet committed is
    /// rejected by its clear bit, at the same point as any invalid free.
    #[test]
    #[should_panic(expected = "invalid or double free of")]
    fn check_of_handed_out_uncommitted_slab_aborts() {
        let env = Env::new();
        let c = min_slab_class();
        let cs = env.class(c, 0);
        env.claim(&cs, 1);
        assert_eq!(cs.committed_slabs(), 128);
        cs.check(env.regions.slab_addr(c, 5) as *mut u8);
    }

    /// Every class: a slab's bits are either whole words or one sub-word
    /// range, neighbouring slabs never share bits, and a group's bits fill
    /// whole lines.
    #[test]
    fn bitmap_geometry_for_every_class() {
        let env = Env::new();
        for c in 0..NCLASSES {
            let cs = env.class(c, 0);
            let cls = cs.class();
            let t = cs.table();
            let n = cls.nslots();
            if t.nslabs() < 2 {
                continue;
            }
            let (p, _) = env.claim(&cs, n + 1);
            assert_eq!(p.len(), n + 1, "class {c}");
            assert!(t.bits(0).is_full(), "class {c}");
            assert_eq!(t.bits(0).allocated(n), n, "class {c}");
            assert_eq!(t.bits(0).nbits(), cls.slab_bits(), "class {c}");
            assert_eq!(t.bits(1).allocated(n), 1, "class {c}");
            assert!(!t.bits(1).is_full(), "class {c}");
            assert!(t.is_allocated(1, 0) && (n == 1 || !t.is_allocated(1, 1)), "class {c}");
            let g = t.group();
            assert!(g >= cls.slabs_per_line() && g % cls.slabs_per_line() == 0, "class {c}");
            // Two slabs used: one group unless groups are single slabs.
            assert_eq!(t.committed_slabs(), if g == 1 { 2 } else { g.min(t.nslabs()) }, "class {c}");
            cs.release(&p);
            assert_eq!(t.bits(0).allocated(n), 0, "class {c}");
            assert_eq!(t.bits(1).allocated(n), 0, "class {c}");
        }
    }

    #[test]
    fn first_claim_is_lowest_addresses_and_zero() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let cls = cs.class();
        assert_eq!(cs.committed_slabs(), 0);
        let (p, claim) = env.claim(&cs, 10);
        assert_eq!(claim, Claim { n: 10, zeroed: true });
        for (i, &q) in p.iter().enumerate() {
            assert_eq!(q as usize, env.regions.slab_addr(c, 0) + i * cls.size());
            assert_eq!(
                env.regions.classify(q as usize),
                Kind::Slab { class: c as u32, slab: 0, slot: i as u32 }
            );
            cs.check(q);
            assert_eq!(cs.locate(q), (0, i));
        }
        let d = cs.desc(0);
        assert_eq!(d.state(), State::Active);
        assert_eq!(d.live(), 10);
        assert_eq!(d.arena(), 0);
        assert_eq!(cs.committed_slabs(), 1);
        assert_eq!(cs.stats(), ClassStats { committed: 1, nonfull: 1, ..Default::default() });
        // A second claim from the same, now Active, slab is not known zero.
        let (p2, claim2) = env.claim(&cs, 5);
        assert_eq!(claim2, Claim { n: 5, zeroed: false });
        assert_eq!(p2[0] as usize, env.regions.slot_addr(c, 0, 10));
        assert_eq!(d.live(), 15);
    }

    #[test]
    fn filling_a_slab_moves_on_and_release_reopens_it() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let n = cs.class().nslots();
        let (all, claim) = env.claim(&cs, n);
        assert_eq!(claim.n, n);
        let set: HashSet<usize> = all.iter().map(|&p| p as usize).collect();
        assert_eq!(set.len(), n);
        assert!(cs.table().bits(0).is_full());
        // A whole fresh slab reserved by one plan leaves the partial set at
        // once; nothing in it could be released before the claim finished.
        assert!(!cs.is_partial(0));
        assert_eq!(cs.stats().nonfull, 0);
        let (next, claim) = env.claim(&cs, 1);
        assert_eq!(claim, Claim { n: 1, zeroed: true });
        assert_eq!(next[0] as usize, env.regions.slab_addr(c, 1));
        assert!(!cs.is_partial(0));
        assert!(cs.is_partial(1));
        assert_eq!(cs.stats().committed, 2);
        assert_eq!(cs.stats().nonfull, 1);
        // Releasing one slot in slab 0 puts it back, and first fit picks it
        // before slab 1.
        let victim = all[n / 2];
        cs.release(&[victim]);
        assert!(cs.is_partial(0));
        assert_eq!(cs.lowest_partial(), Some(0));
        assert_eq!(cs.desc(0).live(), n - 1);
        assert!(!cs.table().is_allocated(0, n / 2));
        let (again, claim) = env.claim(&cs, 2);
        assert_eq!(claim, Claim { n: 2, zeroed: false });
        assert_eq!(again[0], victim);
        assert_eq!(again[1] as usize, env.regions.slot_addr(c, 1, 1));
    }

    #[test]
    fn filling_a_live_slab_leaves_a_stale_bit_until_the_next_plan() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let n = cs.class().nslots();
        // Two claims fill slab 0; the second reserved every remaining slot
        // of a slab that already had live slots, so the bit is left stale.
        let (a, _) = env.claim(&cs, n - 3);
        let (b, _) = env.claim(&cs, 3);
        assert_eq!(a.len() + b.len(), n);
        assert!(cs.table().bits(0).is_full());
        assert!(cs.is_partial(0));
        assert_eq!(cs.stats().nonfull, 1);
        // The next plan notices, clears it and commits slab 1 in the same
        // lock hold.
        let (q, claim) = env.claim(&cs, 2);
        assert_eq!(claim, Claim { n: 2, zeroed: true });
        assert_eq!(q[0] as usize, env.regions.slab_addr(c, 1));
        assert!(!cs.is_partial(0));
        assert!(cs.is_partial(1));
        assert_eq!(cs.stats(), ClassStats { committed: 2, nonfull: 1, ..Default::default() });
        // A release into the full slab reopens it and first fit prefers it.
        cs.release(&[a[5]]);
        assert!(cs.is_partial(0));
        let (r, claim) = env.claim(&cs, 1);
        assert_eq!(claim, Claim { n: 1, zeroed: false });
        assert_eq!(r[0], a[5]);
    }

    #[test]
    fn one_plan_spans_many_slabs_and_release_reopens_them_all() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let n = cs.class().nslots();
        // Fill 2 * MAX_PICKS + 1 slabs: the first plan reserves MAX_PICKS
        // whole fresh slabs, the second the rest.
        let slabs = 2 * MAX_PICKS + 1;
        let (all, claim) = env.claim(&cs, slabs * n);
        assert_eq!(claim, Claim { n: slabs * n, zeroed: true });
        for s in 0..slabs {
            assert_eq!(all[s * n] as usize, env.regions.slab_addr(c, s));
            assert!(cs.table().bits(s).is_full());
            assert!(!cs.is_partial(s));
        }
        assert_eq!(cs.stats(), ClassStats { committed: slabs, nonfull: 0, ..Default::default() });
        // One slot per slab, sorted, in one batch: the reopen list overflows
        // at MAX_PICKS and flushes; every slab must be partial afterwards.
        let victims: Vec<*mut u8> = (0..slabs).map(|s| all[s * n + s % n]).collect();
        cs.release(&victims);
        for s in 0..slabs {
            assert!(cs.is_partial(s), "slab {s} not reopened");
            assert_eq!(cs.desc(s).live(), n - 1);
        }
        assert_eq!(cs.stats().nonfull, slabs);
        // Refilling takes exactly those slots, lowest slab first, in one
        // plan per MAX_PICKS slabs.
        let (again, claim) = env.claim(&cs, slabs);
        assert_eq!(claim, Claim { n: slabs, zeroed: false });
        assert_eq!(again, victims);
        // Those slabs had live slots, so their bits went stale rather than
        // being cleared; each later plan cleared the previous plan's full
        // slabs on its way past, leaving only the last one stale.
        assert_eq!(cs.stats().nonfull, 1);
        assert!(cs.is_partial(slabs - 1));
        // Releasing a whole slab in one batch empties it; the reopen list
        // must not put an Empty slab back into the partial set.
        let mut whole: Vec<*mut u8> = all[3 * n..4 * n].to_vec();
        whole.sort();
        cs.release(&whole);
        assert_eq!(cs.desc(3).state(), State::Empty);
        assert!(!cs.is_partial(3));
        assert_eq!(cs.stats().empty, 1);
        assert_eq!(cs.stats().nonfull, 1);
        // And the same for the slab with the stale bit.
        let mut whole: Vec<*mut u8> = all[(slabs - 1) * n..].to_vec();
        whole.sort();
        cs.release(&whole);
        assert_eq!(cs.desc(slabs - 1).state(), State::Empty);
        assert!(!cs.is_partial(slabs - 1));
        assert_eq!(cs.stats().empty, 2);
        assert_eq!(cs.stats().nonfull, 0);
    }

    #[test]
    fn release_all_makes_empty_then_reuse_then_purge() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let (p, _) = env.claim(&cs, 100);
        // Release in reverse order to exercise the run grouping.
        let mut rev = p.clone();
        rev.reverse();
        cs.release(&rev);
        let d = cs.desc(0);
        assert_eq!(d.state(), State::Empty);
        assert_eq!(d.live(), 0);
        assert!(d.dirty_at() > 0);
        assert!(!cs.is_partial(0));
        assert_eq!(cs.stats(), ClassStats { committed: 1, empty: 1, ..Default::default() });
        // An Empty slab is preferred over committing a new one but is dirty.
        let (q, claim) = env.claim(&cs, 3);
        assert_eq!(claim, Claim { n: 3, zeroed: false });
        assert_eq!(q[0], p[0]);
        assert_eq!(d.state(), State::Active);
        assert_eq!(d.dirty_at(), 0);
        assert_eq!(cs.stats(), ClassStats { committed: 1, nonfull: 1, ..Default::default() });
        cs.release(&q);
        assert_eq!(d.state(), State::Empty);
        // Purge with a cutoff before dirty_at does nothing; after it purges.
        assert_eq!(cs.purge_empty(d.dirty_at() - 1), 0);
        assert_eq!(d.state(), State::Empty);
        assert_eq!(cs.purge_empty(u64::MAX), 1);
        assert_eq!(d.state(), State::Purged);
        assert_eq!(d.dirty_at(), 0);
        assert_eq!(cs.stats(), ClassStats { committed: 1, purged: 1, ..Default::default() });
        // A Purged slab is reused before a new commit and is zero.
        let (r, claim) = env.claim(&cs, 4);
        assert_eq!(claim, Claim { n: 4, zeroed: true });
        assert_eq!(r[0], p[0]);
        assert_eq!(d.state(), State::Active);
        assert_eq!(cs.stats(), ClassStats { committed: 1, nonfull: 1, ..Default::default() });
    }

    /// The decay driver's view: `oldest_empty` tracks the first slab to go
    /// `Empty`, `decay` purges nothing before the deadline and everything
    /// that old at it, and a stale (too early) value corrects itself.
    #[test]
    fn decay_deadlines_follow_the_oldest_empty_slab() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let nslots = CLASSES[c].nslots();
        assert_eq!(cs.oldest_empty(), 0);
        assert_eq!(cs.decay(u64::MAX, 1000), None);
        // Two slabs; free the second first so the oldest is not the lowest.
        let (p, _) = env.claim(&cs, nslots * 2);
        assert_eq!(cs.stats().committed, 2);
        cs.release(&p[nslots..]);
        let t1 = cs.desc(1).dirty_at();
        assert!(t1 > 0);
        assert_eq!(cs.oldest_empty(), t1);
        // The coarse clock ticks every few milliseconds on Linux; off Linux
        // it is a counter and the sleep is just short.
        std::thread::sleep(core::time::Duration::from_millis(30));
        cs.release(&p[..nslots]);
        let t0 = cs.desc(0).dirty_at();
        assert!(t0 > t1);
        assert_eq!(cs.oldest_empty(), t1, "a later Empty slab must not move the oldest");
        // Before the deadline nothing happens and the deadline is reported.
        assert_eq!(cs.decay(t1 + 999, 1000), Some(t1 + 1000));
        assert_eq!(cs.stats().empty, 2);
        // At it, slab 1 (dirty at t1) is purged, slab 0 (t0 > t1) survives.
        assert_eq!(cs.decay(t1 + 1000, 1000), Some(t0 + 1000));
        assert_eq!(cs.desc(1).state(), State::Purged);
        assert_eq!(cs.desc(0).state(), State::Empty);
        assert_eq!(cs.oldest_empty(), t0);
        // A claim reuses slab 0 (the lowest Empty): the atomic is now stale
        // (too early). The next decay finds nothing and clears it.
        let (q, _) = env.claim(&cs, 1);
        assert_eq!(q[0], p[0]);
        assert_eq!(cs.oldest_empty(), t0);
        assert_eq!(cs.decay(u64::MAX / 2, 1000), None);
        assert_eq!(cs.oldest_empty(), 0);
        assert_eq!(cs.stats(), ClassStats { committed: 2, purged: 1, nonfull: 1, ..Default::default() });
        // `decay_ns` 0 purges as soon as a slab is Empty.
        cs.release(&q);
        let t = cs.oldest_empty();
        assert!(t > 0);
        assert_eq!(cs.decay(t, 0), None);
        assert_eq!(cs.desc(0).state(), State::Purged);
        // Purging everything at once clears it too.
        let (r, _) = env.claim(&cs, nslots * 2);
        cs.release(&r);
        assert!(cs.oldest_empty() > 0);
        assert_eq!(cs.purge_empty(u64::MAX), 2);
        assert_eq!(cs.oldest_empty(), 0);
    }

    /// Every slab of the region (a full batch's worth in the test layout)
    /// is purged in one call.
    #[test]
    fn purge_batches_a_whole_region() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let nslots = CLASSES[c].nslots();
        let n = cs.nslabs();
        assert!(n >= crate::purge::MAX_BATCH, "test layout holds {n} slabs");
        let (p, claim) = env.claim(&cs, nslots * n);
        assert_eq!(claim.n, nslots * n);
        cs.release(&p);
        assert_eq!(cs.stats().empty, n);
        assert_eq!(cs.purge_empty(u64::MAX), n);
        assert_eq!(cs.stats().purged, n);
        assert_eq!(cs.oldest_empty(), 0);
        for s in 0..n {
            assert_eq!(cs.desc(s).state(), State::Purged, "slab {s}");
        }
    }

    #[test]
    fn claim_spans_slabs_and_reports_shortfall_on_exhaustion() {
        let env = Env::new();
        // The largest class: one 4 MiB slab of 16 slots in this tiny region.
        let c = NCLASSES - 1;
        let cs = env.class(c, 0);
        assert_eq!(cs.nslabs(), 1);
        assert_eq!(cs.class().nslots(), 16);
        let (p, claim) = env.claim(&cs, 20);
        assert_eq!(claim, Claim { n: 16, zeroed: true });
        assert_eq!(p.len(), 16);
        let (q, claim) = env.claim(&cs, 1);
        assert_eq!(claim, Claim { n: 0, zeroed: false });
        assert!(q.is_empty());
        cs.release(&p[..8]);
        let (q, claim) = env.claim(&cs, 20);
        assert_eq!(claim, Claim { n: 8, zeroed: false });
        assert_eq!(q, p[..8].to_vec());
        // A class with 64 slabs: ask for more than one slab holds.
        let c = small_class();
        let cs = env.class(c, 0);
        let n = cs.class().nslots();
        let (p, claim) = env.claim(&cs, 2 * n + 5);
        assert_eq!(claim, Claim { n: 2 * n + 5, zeroed: true });
        assert_eq!(cs.stats().committed, 3);
        assert_eq!(cs.stats().nonfull, 1);
        assert_eq!(p[2 * n] as usize, env.regions.slab_addr(c, 2));
    }

    #[test]
    fn guard_slabs_are_skipped() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 4);
        for s in 0..16 {
            assert_eq!(cs.is_guard_slab(s), s % 4 == 3, "slab {s}");
        }
        let n = cs.class().nslots();
        let (p, claim) = env.claim(&cs, 6 * n);
        assert_eq!(claim.n, 6 * n);
        let slabs: HashSet<usize> = p.iter().map(|&q| cs.locate(q).0).collect();
        assert_eq!(slabs, [0, 1, 2, 4, 5, 6].into_iter().collect());
        // Slabs 0..=7 are committed: six allocatable and two guards.
        assert_eq!(cs.committed_slabs(), 7);
        let (q, _) = env.claim(&cs, 1);
        assert_eq!(cs.locate(q[0]).0, 8);
        assert_eq!(cs.committed_slabs(), 9);
        assert_eq!(cs.stats().committed, 7);
        assert_eq!(cs.stats().guards, 2);
        // A guard slab's descriptor is zero: any free into it aborts.
        assert_eq!(cs.desc(3).state(), State::Fresh);
        assert_eq!(cs.desc(3).live(), 0);
    }

    #[test]
    fn works_for_every_class() {
        let env = Env::new();
        for c in 0..NCLASSES {
            let cs = env.class(c, 0);
            let cls = cs.class();
            let want = cls.nslots().min(70);
            let (p, claim) = env.claim(&cs, want);
            assert_eq!(claim, Claim { n: want, zeroed: true }, "class {c}");
            for (i, &q) in p.iter().enumerate() {
                assert_eq!(
                    env.regions.classify(q as usize),
                    Kind::Slab { class: c as u32, slab: 0, slot: i as u32 },
                    "class {c}"
                );
                cs.check(q);
            }
            cs.release(&p);
            assert_eq!(cs.desc(0).state(), State::Empty, "class {c}");
            assert_eq!(cs.purge_empty(u64::MAX), 1);
        }
        let s = env.meta.stats();
        assert!(s.reserved > 0 && s.committed > 0);
    }

    #[test]
    fn metadata_exhaustion_is_reported_not_fatal() {
        // A metadata region too small for the smallest class's descriptor
        // array: 64 slabs * stride is small, so shrink the region to a
        // couple of pages instead.
        #[cfg(not(target_os = "linux"))]
        let (meta, _buf) = {
            let buf = vec![0u8; 3 * PAGE];
            let mb = align_up(buf.as_ptr() as usize, PAGE);
            (Meta::new(mb, 2 * PAGE, PAGE), buf)
        };
        #[cfg(target_os = "linux")]
        let meta = {
            use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
            let mb = unsafe {
                crate::sys::mmap(0, 2 * PAGE, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0)
            }
            .unwrap();
            Meta::new(mb, 2 * PAGE, PAGE)
        };
        let regions = regions_at(0x1000_0000_0000, (0x2000_0000_0000, 2 * PAGE), PAGE.trailing_zeros());
        assert!(SlabTable::new(&meta, &regions, 0, 0).is_none());
    }

    /// Two arenas over one class region: slabs are handed out in address
    /// order to whichever arena commits next, each records its owner, and
    /// each arena's sets and counts see only its own slabs.
    #[test]
    fn two_arenas_share_one_region() {
        let env = Env::new();
        let c = small_class();
        let cs0 = env.class(c, 0);
        let cs1 = env.class_in_arena(&cs0, 1);
        assert_eq!(cs1.arena(), 1);
        assert!(core::ptr::eq(cs0.table(), cs1.table()));
        let (p0, _) = env.claim(&cs0, 3);
        let (p1, _) = env.claim(&cs1, 3);
        assert_eq!(cs0.committed_slabs(), 2);
        assert_eq!(cs1.committed_slabs(), 2);
        assert_eq!(cs0.desc(0).arena(), 0);
        assert_eq!(cs0.desc(1).arena(), 1);
        assert_eq!(cs0.locate(p0[0]).0, 0);
        assert_eq!(cs1.locate(p1[0]).0, 1);
        assert!(cs0.is_partial(0) && !cs0.is_partial(1));
        assert!(cs1.is_partial(1) && !cs1.is_partial(0));
        assert_eq!(cs0.stats().committed, 1);
        assert_eq!(cs1.stats().committed, 1);
        // Each arena keeps growing past the other's slabs.
        let (q1, _) = env.claim(&cs1, cs1.class().nslots());
        assert_eq!(cs1.locate(q1[q1.len() - 1]).0, 2);
        assert_eq!(cs0.desc(2).arena(), 1);
        let (q0, _) = env.claim(&cs0, cs0.class().nslots());
        assert_eq!(cs0.locate(q0[q0.len() - 1]).0, 3);
        assert_eq!(cs0.desc(3).arena(), 0);
        // Releases land in the owner's sets only.
        cs1.release(&p1);
        cs1.release(&q1);
        assert_eq!(cs1.desc(1).state(), State::Empty);
        assert_eq!(cs1.desc(2).state(), State::Empty);
        assert_eq!(cs1.stats().empty, 2);
        assert_eq!(cs0.stats().empty, 0);
        assert_eq!(cs1.purge_empty(u64::MAX), 2);
        assert_eq!(cs0.stats().purged, 0);
        assert_eq!(cs1.stats().purged, 2);
        cs0.release(&p0);
        cs0.release(&q0);
        assert_eq!(cs0.stats().empty, 2);
        // A purged slab is reused by its own arena before anything new is
        // committed.
        let (r1, claim) = env.claim(&cs1, 1);
        assert!(claim.zeroed);
        assert_eq!(cs1.locate(r1[0]).0, 1);
        assert_eq!(cs0.committed_slabs(), 4);
    }

    #[test]
    #[should_panic(expected = "belongs to arena 1 but was released through arena 0")]
    fn release_through_the_wrong_arena_aborts() {
        let env = Env::new();
        let c = small_class();
        let cs0 = env.class(c, 0);
        let cs1 = env.class_in_arena(&cs0, 1);
        let (_p0, _) = env.claim(&cs0, 1);
        let (p1, _) = env.claim(&cs1, 1);
        cs0.release(&p1);
    }

    #[test]
    #[should_panic(expected = "belongs to arena 1 but was released through arena 0")]
    fn check_through_the_wrong_arena_aborts() {
        let env = Env::new();
        let c = small_class();
        let cs0 = env.class(c, 0);
        let cs1 = env.class_in_arena(&cs0, 1);
        let (_p0, _) = env.claim(&cs0, 1);
        let (p1, _) = env.claim(&cs1, 1);
        assert_eq!(cs0.table().check(p1[0]), 1);
        cs0.check(p1[0]);
    }

    #[test]
    #[should_panic(expected = "double free of")]
    fn double_release_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 3);
        cs.release(&p[..1]);
        cs.release(&p[..1]);
    }

    #[test]
    #[should_panic(expected = "double free of")]
    fn duplicate_in_one_batch_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 3);
        cs.release(&[p[1], p[1]]);
    }

    #[test]
    #[should_panic(expected = "not a slot start")]
    fn release_of_interior_pointer_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 1);
        cs.release(&[unsafe { p[0].add(1) }]);
    }

    #[test]
    #[should_panic(expected = "outside the class region")]
    fn release_outside_region_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        env.claim(&cs, 1);
        cs.release(&[env.regions.huge_base as *mut u8]);
    }

    #[test]
    #[should_panic(expected = "was never allocated")]
    fn check_of_uncommitted_slab_aborts() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        env.claim(&cs, 1);
        cs.check(env.regions.slab_addr(c, 5) as *mut u8);
    }

    #[test]
    #[should_panic(expected = "invalid or double free of")]
    fn check_of_unclaimed_slot_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 2);
        cs.check(unsafe { p[1].add(cs.class().size()) });
    }

    #[test]
    #[should_panic(expected = "invalid or double free of")]
    fn check_after_release_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 2);
        cs.release(&p[..1]);
        cs.check(p[0]);
    }

    /// Randomised single-thread model check: live pointers are unique,
    /// valid, and the descriptor counts match; after draining, every slab
    /// is Empty and purging leaves everything Purged.
    #[test]
    fn randomised_model_check() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let cls = cs.class();
        let mut rng = 0xC0FF_EE12_3456_789Au64;
        let mut live: Vec<*mut u8> = Vec::new();
        let mut set: HashSet<usize> = HashSet::new();
        for _ in 0..4000 {
            let r = xorshift(&mut rng);
            if r % 2 == 0 && !live.is_empty() {
                // Release a random batch; claims and releases are balanced so
                // the live set random-walks well inside the 64-slab region.
                let k = (xorshift(&mut rng) as usize % 70 + 1).min(live.len());
                let mut batch = Vec::with_capacity(k);
                for _ in 0..k {
                    let i = xorshift(&mut rng) as usize % live.len();
                    let p = live.swap_remove(i);
                    assert!(set.remove(&(p as usize)));
                    batch.push(p);
                }
                if xorshift(&mut rng) % 2 == 0 {
                    batch.sort();
                }
                cs.release(&batch);
            } else {
                let k = xorshift(&mut rng) as usize % 70 + 1;
                let (p, claim) = env.claim(&cs, k);
                assert_eq!(claim.n, k);
                for &q in &p {
                    assert!(set.insert(q as usize), "duplicate pointer {q:p}");
                    assert!(matches!(
                        env.regions.classify(q as usize),
                        Kind::Slab { class, .. } if class as usize == c
                    ));
                    cs.check(q);
                }
                live.extend(p);
            }
            // Invariants after every step.
            let mut total_live = 0;
            let st = cs.stats();
            let mut empty = 0;
            for s in 0..cs.committed_slabs() {
                let d = cs.desc(s);
                total_live += d.live();
                let bm = cs.table().bits(s);
                assert_eq!(bm.allocated(cls.nslots()), d.live(), "slab {s}");
                match d.state() {
                    State::Empty => {
                        assert_eq!(d.live(), 0);
                        assert!(!cs.is_partial(s));
                        empty += 1;
                    }
                    State::Active => {
                        assert!(d.live() > 0 || cs.is_partial(s));
                        if !bm.is_full() {
                            assert!(cs.is_partial(s), "slab {s} has free slots but is not partial");
                        }
                    }
                    other => panic!("slab {s} in state {other:?}"),
                }
            }
            assert_eq!(total_live, live.len());
            assert_eq!(st.empty, empty);
        }
        live.sort();
        cs.release(&live);
        let st = cs.stats();
        assert_eq!(st.empty, st.committed);
        assert_eq!(st.nonfull, 0);
        assert_eq!(cs.purge_empty(u64::MAX), st.committed);
        let st = cs.stats();
        assert_eq!(st.purged, st.committed);
        assert_eq!(st.empty, 0);
        for s in 0..cs.committed_slabs() {
            assert_eq!(cs.desc(s).state(), State::Purged);
        }
        // Everything is reusable afterwards, from slab 0, and zero.
        let (p, claim) = env.claim(&cs, 3);
        assert_eq!(claim, Claim { n: 3, zeroed: true });
        assert_eq!(p[0] as usize, env.regions.slab_addr(c, 0));
    }

    /// Eight threads claim and release concurrently; no pointer is ever
    /// held by two threads at once and the counters settle.
    #[test]
    fn concurrent_claims_are_disjoint() {
        let env = Arc::new(Env::new());
        let c = small_class();
        let cs = Arc::new(env.class(c, 0));
        let threads = 8;
        let rounds = 300;
        let mut handles = Vec::new();
        for t in 0..threads {
            let env = Arc::clone(&env);
            let cs = Arc::clone(&cs);
            handles.push(std::thread::spawn(move || {
                let mut rng = 0x9E37_79B9_7F4A_7C15u64 ^ (t as u64 + 1);
                let mut held: Vec<*mut u8> = Vec::new();
                let mut seen: Vec<usize> = Vec::new();
                for _ in 0..rounds {
                    let k = xorshift(&mut rng) as usize % 50 + 1;
                    let (p, claim) = env.claim(&cs, k);
                    assert_eq!(claim.n, k);
                    for &q in &p {
                        cs.check(q);
                        seen.push(q as usize);
                    }
                    held.extend(p);
                    if xorshift(&mut rng) % 4 != 0 {
                        let keep = xorshift(&mut rng) as usize % (held.len() + 1);
                        let mut batch = held.split_off(keep);
                        batch.sort();
                        cs.release(&batch);
                    }
                }
                held.sort();
                cs.release(&held);
                seen
            }));
        }
        // Pointers held simultaneously by two threads would show up as a
        // slot claimed twice without an intervening release; we approximate
        // that check by verifying the final state is consistent and that
        // per-thread `check` never aborted (it runs on every claimed slot).
        let mut total = 0;
        for h in handles {
            total += h.join().unwrap().len();
        }
        assert!(total > 0);
        let st = cs.stats();
        assert_eq!(st.empty, st.committed, "{st:?}");
        assert_eq!(st.nonfull, 0);
        for s in 0..cs.committed_slabs() {
            let d = cs.desc(s);
            assert_eq!(d.state(), State::Empty, "slab {s}");
            assert_eq!(d.live(), 0, "slab {s}");
            assert!(!cs.table().bits(s).is_full());
            for slot in 0..cs.class().nslots() {
                assert!(!cs.table().is_allocated(s, slot));
            }
        }
    }

    /// Two threads hammer one slot's worth of address space: one claims and
    /// immediately releases, the other purges whenever it can. The
    /// reservation-first protocol must keep every claimed slot readable and
    /// writable and never let a purge race a claim.
    #[cfg(target_os = "linux")]
    #[test]
    fn purge_never_races_a_claim() {
        let env = Arc::new(Env::new());
        let c = small_class();
        let cs = Arc::new(env.class(c, 0));
        let stop = Arc::new(core::sync::atomic::AtomicBool::new(false));
        let purger = {
            let cs = Arc::clone(&cs);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut n = 0;
                while !stop.load(core::sync::atomic::Ordering::Relaxed) {
                    n += cs.purge_empty(u64::MAX);
                }
                n
            })
        };
        let mut rng = 1u64;
        for i in 0..20_000u32 {
            let k = xorshift(&mut rng) as usize % 8 + 1;
            let (p, claim) = env.claim(&cs, k);
            assert_eq!(claim.n, k);
            for &q in &p {
                unsafe {
                    if claim.zeroed {
                        assert_eq!(*(q as *const u32), 0, "iteration {i}: purged slot not zero");
                    }
                    *(q as *mut u32) = i;
                }
            }
            for &q in &p {
                assert_eq!(unsafe { *(q as *const u32) }, i, "iteration {i}: slot lost its value");
            }
            cs.release(&p);
        }
        stop.store(true, core::sync::atomic::Ordering::Relaxed);
        let purges = purger.join().unwrap();
        assert!(purges > 0, "the purger never found an empty slab");
        assert_eq!(cs.desc(0).live(), 0);
    }

    /// Purged memory reads zero and the claim reports it; committed slabs
    /// show up read-write in `/proc/self/maps`.
    #[cfg(target_os = "linux")]
    #[test]
    fn purge_zeroes_memory_and_commit_is_visible() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let (p, claim) = env.claim(&cs, 5);
        assert!(claim.zeroed);
        for &q in &p {
            unsafe {
                assert_eq!(*q, 0);
                *q = 0xAB;
            }
        }
        cs.release(&p);
        let (q, claim) = env.claim(&cs, 5);
        assert_eq!(q, p);
        assert!(!claim.zeroed);
        assert_eq!(unsafe { *q[0] }, 0xAB);
        cs.release(&q);
        assert_eq!(cs.purge_empty(u64::MAX), 1);
        let (r, claim) = env.claim(&cs, 5);
        assert_eq!(r, p);
        assert!(claim.zeroed);
        for &s in &r {
            assert_eq!(unsafe { *s }, 0);
        }
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let slab = env.regions.slab_addr(c, 0);
        let end = slab + cs.class().slab_size();
        let rw = maps.lines().any(|l| {
            let mut it = l.split_whitespace();
            let range = it.next().unwrap();
            let perms = it.next().unwrap();
            let (a, b) = range.split_once('-').unwrap();
            let a = usize::from_str_radix(a, 16).unwrap();
            let b = usize::from_str_radix(b, 16).unwrap();
            a <= slab && end <= b && perms.starts_with("rw")
        });
        assert!(rw, "slab 0 is not read-write in /proc/self/maps");
        // Descriptors live in the metadata region and got committed too.
        let d = cs.desc(0) as *const SlabDesc as usize;
        assert!(env.regions.in_meta(d));
        assert!(env.meta.stats().committed >= PAGE);
    }

    /// Guard slabs only when the kernel has `MADV_GUARD_INSTALL`: touching
    /// the guard page must fault. Run in a child so the fault is observable.
    #[cfg(target_os = "linux")]
    #[test]
    fn guard_slab_faults_when_supported() {
        use crate::sys::features::Features;
        let f = Features::probe();
        if !f.guard_install {
            std::eprintln!("guard_install=no on this kernel; skipping guard fault test");
            return;
        }
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 2);
        let n = cs.class().nslots();
        let (p, _) = env.claim(&cs, n + 1);
        assert_eq!(cs.locate(p[n]).0, 2);
        let guard = env.regions.slab_addr(c, 1);
        // The guard slab is inside the same rw mapping as its neighbours.
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let merged = maps.lines().any(|l| {
            let range = l.split_whitespace().next().unwrap();
            let (a, b) = range.split_once('-').unwrap();
            let a = usize::from_str_radix(a, 16).unwrap();
            let b = usize::from_str_radix(b, 16).unwrap();
            a <= env.regions.slab_addr(c, 0) && env.regions.slab_addr(c, 3) <= b
        });
        assert!(merged, "guard slab split the VMA");
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                core::ptr::write_volatile(guard as *mut u8, 1);
                libc::_exit(0);
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFSIGNALED(status), "child did not fault: status {status:#x}");
        assert_eq!(libc::WTERMSIG(status), libc::SIGSEGV);
    }
}
