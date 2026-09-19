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
//! An arena owns its groups until it lends an idle one (design 13.3, plan
//! Phase 1.5 task 15). A group is *idle* when none of its slabs is
//! `Active`: every committed slab of it is in its owner's `empty` or
//! `purged` set (so `live == 0`: no slot of it is in the program or in any
//! cache) and the rest are `Fresh`. An arena whose current group is used
//! up and whose sets are empty looks for such a group in the other arenas'
//! sets before it takes a fresh group from the region
//! ([`ClassState::adopt_idle_group`]): under the donor's class lock it
//! scans the donor's `empty` and `purged` sets group by group, and under
//! both class locks it rewrites the owner word and every descriptor's
//! arena, moves the slabs between the two arenas' sets and slab counts,
//! and inherits the donor's commit cursor when the group was the donor's
//! current one. Nothing is added to the transition paths (`take`,
//! `on_live_zero`): the sets are the tracking. Whole groups only, so the
//! line ownership above is untouched: at 16 arenas the `Empty` slabs one
//! arena kept while another committed fresh ones were 40 MiB of mstress's
//! RSS (`bench/results/2026-09-18-phase-1.5-acceptance-3.md`).
//!
//! Ownership can now change between a lock-free step and the lock hold
//! that follows it, so a slab number carried across that gap is checked
//! against the owner word under the lock before the arena touches its
//! sets: `reopen` (a word went full to non-full before `dec_live` let the
//! slab go `Empty`, and another arena may have taken it since) and
//! `on_live_zero`. Live slots pin a slab (they are counted in `live`), so
//! nothing else carries a slab across a gap.
//!
//! Protocol. A refill takes the lock, picks the lowest partial slab (else
//! the lowest empty, else the lowest purged, else commits the next slab),
//! moves it to `Active`, *reserves* the wanted count in the descriptor's
//! `live` counter, and releases the lock. It then claims bits, hands out
//! what it got and gives back the unused reservation. Reserving before
//! claiming means `live` is never zero while a claim is in flight, so a
//! purge (which requires `live == 0` under the lock) can never discard
//! memory a thread is about to hand out. A slab observed `Fresh` or
//! `Purged` at selection is the exception to the unlocked bit claim: its
//! slots (the lowest `n` bits; `live` is zero, so nothing contends) are
//! claimed in the same lock hold, before the slab enters the partial set.
//! That is what makes the refill's zero report sound: a bit decided after
//! the lock is dropped could be one that another thread claimed from the
//! now-partial slab, handed to the program and got back dirty in between,
//! and `calloc` would skip its memset over it (the calloc race).
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
use crate::layout::{CLASSES, Class, LG_MAX_SLAB, LG_MIN_SLAB, MAX_SLAB_WORDS, NCLASSES, Regions};
use crate::lock::{Lock, PAD_ALIGN, Padded, RawLock};
use crate::meta::Meta;
use crate::purge::{self, Batch, Curve, Decayer, STEPS};
use crate::slab::{DESC_ALIGN, SlabDesc, State, desc_stride};
use crate::sys::Errno;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU16, AtomicU64, AtomicUsize, Ordering};

/// Counters kept under the class lock. Slab counts only; `curregs` and
/// the byte totals are derived from them in `stats.rs`.
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

impl ClassStats {
    /// Slabs that can hold live slots (neither `Empty` nor `Purged`):
    /// jemalloc's `curslabs`, and the slab count behind `active` and the
    /// compaction gate's capacity.
    #[inline]
    pub fn curslabs(&self) -> usize {
        self.committed - self.empty - self.purged
    }
}

/// xorshift64 step for `slot_randomize` (the state must not be zero; the
/// heap seeds it so). Not a cryptographic generator: the key randomises
/// the order slots are handed out in, which is a heap-layout obstacle, not
/// a secret.
#[inline]
pub(crate) fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Fisher-Yates shuffle of `out` with [`xorshift`].
pub(crate) fn shuffle(out: &mut [*mut u8], rng: &mut u64) {
    for i in (1..out.len()).rev() {
        let j = (xorshift(rng) % (i as u64 + 1)) as usize;
        out.swap(i, j);
    }
}

/// Result of one [`ClassState::claim`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// Slots written to the output; fewer than asked means the region or
    /// the metadata region is exhausted or a commit failed.
    pub n: usize,
    /// Every returned slot came from a slab that was `Fresh` or `Purged`
    /// when this thread selected it and had its bit claimed in that same
    /// lock hold, before anything else could allocate from the slab, so
    /// its memory reads as zero.
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

/// Groups of one donor's `empty` and `purged` sets a refill inspects for
/// an idle one before moving on to the next donor: a donor with many
/// partly idle groups (a medium class under churn) is not scanned to the
/// end.
const MAX_ADOPT_TRIES: usize = 8;

/// The `group_handoff` key (default off, an opt-in; design 13.3): whether
/// an arena takes another arena's idle group before a fresh one. Set once
/// at init, read on the fresh-commit path only.
static GROUP_HANDOFF: AtomicBool = AtomicBool::new(false);

/// Apply the `group_handoff` key (init; the tests directly).
pub fn set_group_handoff(on: bool) {
    GROUP_HANDOFF.store(on, Ordering::Relaxed);
}

/// Tests: groups that changed hands.
#[cfg(test)]
pub(crate) static HANDOFFS: AtomicUsize = AtomicUsize::new(0);

/// One slab's share of a refill: `n` slots reserved in its `live` count.
#[derive(Copy, Clone)]
struct Pick {
    slab: u32,
    n: u32,
    /// The slab was `Fresh` or `Purged` (and zero) when this refill made
    /// it `Active`, and bits `0..n` were claimed under the lock in `take`
    /// ([`ClassState::claim_prefix`]): `finish` hands out slots `0..n`
    /// without the bit loop.
    zero: bool,
    /// The slab's state when this refill selected it (for `check_zero`).
    state: State,
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
    /// A table of no bytes: `ensure` is a no-op on it.
    pub(crate) const EMPTY: Lazy = Lazy { base: 0, len: 0, committed: 0 };

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
/// committed table, either its own ([`Set::new`], `huge.rs`) or one view
/// of an arena's [`SetTable`] ([`Set::in_table`]). Shared with `huge.rs`.
pub(crate) struct Set {
    pub(crate) map: TwoLevelBitmap,
    store: Store,
    l1_words: usize,
}

enum Store {
    Own(Lazy),
    /// The arena's table, which outlives the set (it is a field of the
    /// same `Arena`, or leaked by the tests), and the set's group in it.
    Shared(*const SetTable, u8),
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
        Some(Set { map, store: Store::Own(table), l1_words })
    }

    /// Set `kind` (0..`SETS_PER_CLASS`) of `class` in `table`, over the
    /// class's slab numbers. `table` must outlive the set.
    pub(crate) fn in_table(table: &SetTable, class: usize, kind: usize) -> Set {
        debug_assert!(kind < SETS_PER_CLASS);
        let g = SetTable::group_of(class);
        let group = &table.groups[g];
        let slot = kind * group.count + table.pos[class] as usize;
        let nslabs = table.nslabs[g];
        // SAFETY: the slot's words are `base + 8 * (slot + r * stride)`,
        // disjoint from every other slot's, zero, never freed; the rows a
        // slab's bit needs are committed by `ensure` before the class
        // touches it (`take_group`).
        let map = unsafe { TwoLevelBitmap::from_raw_chunked(nslabs, (group.base + 8 * slot) as *mut u64, group.stride) };
        Set { map, store: Store::Shared(table, g as u8), l1_words: TwoLevelBitmap::l1_words(nslabs) }
    }

    /// Commit the level-0 words that hold every bit up to and including
    /// `slab`.
    pub(crate) fn ensure(&mut self, meta: &Meta, page: usize, slab: usize) -> Result<(), Errno> {
        match &mut self.store {
            Store::Own(table) => table.ensure(meta, page, 8 * (self.l1_words + slab / 64 + 1)),
            // SAFETY: the table outlives the set (`Store::Shared`).
            Store::Shared(table, g) => unsafe { &**table }.ensure(meta, *g as usize, slab),
        }
    }
}

/// Slab sets per class ([`ClassState`]: partial, empty, purged).
pub const SETS_PER_CLASS: usize = 3;
/// Set table groups: one per slab size (`Class::lg_slab`), since classes
/// with the same slab size have the same slab count.
pub const SET_GROUPS: usize = (LG_MAX_SLAB - LG_MIN_SLAB + 1) as usize;

/// One arena's slab sets for every class: one interleaved, lazily
/// committed table per slab size, holding the `SETS_PER_CLASS` sets of
/// every class of that slab size as chunked [`TwoLevelBitmap`] views (row
/// `r` of the set at `slot = kind * count + pos` is at `base + 8 * (r *
/// stride + slot)`, `stride = SETS_PER_CLASS * count`). Rows are committed
/// as a prefix while slab numbers grow, and a small heap touches only row 0
/// (level-1 word 0) and row 1 (level-0 word 0) of the groups it uses, one
/// page per (arena, slab size), where one page-aligned table per set cost
/// one or two pages per (arena, class) and, at 16 arenas, put alloc-testN's
/// RSS 28% above jemalloc's (`bench/results/2026-09-18-*-gce-arena-rss.md`).
pub struct SetTable {
    groups: [Group; SET_GROUPS],
    /// Slab count of each group's classes.
    nslabs: [usize; SET_GROUPS],
    /// Position of each class within its group.
    pos: [u8; NCLASSES],
    /// Commit watermarks; taken by the fork handlers after the arena's
    /// class locks, which is the order `take_group` uses.
    tables: Lock<[Lazy; SET_GROUPS]>,
    page: usize,
}

// SAFETY: a `SetTable` is shared by the arena's class states; its groups
// and positions are immutable after `new` and the commit watermarks are
// behind `tables`' lock.
unsafe impl Sync for SetTable {}
unsafe impl Send for SetTable {}

#[derive(Clone, Copy)]
struct Group {
    base: usize,
    stride: usize,
    count: usize,
}

impl SetTable {
    /// Group of a class: its slab size.
    #[inline]
    pub fn group_of(class: usize) -> usize {
        (CLASSES[class].lg_slab as u32 - LG_MIN_SLAB) as usize
    }

    /// Reserve the tables (address space only; rows are committed by
    /// `ensure`). `None` if the metadata region cannot hold them.
    pub fn new(meta: &Meta, regions: &Regions) -> Option<SetTable> {
        let page = regions.page_size();
        let mut groups = [Group { base: 0, stride: 0, count: 0 }; SET_GROUPS];
        let mut nslabs = [0usize; SET_GROUPS];
        let mut pos = [0u8; NCLASSES];
        for c in 0..NCLASSES {
            let g = Self::group_of(c);
            pos[c] = groups[g].count as u8;
            groups[g].count += 1;
            nslabs[g] = regions.nslabs(c);
        }
        let mut tables = [Lazy::EMPTY; SET_GROUPS];
        for g in 0..SET_GROUPS {
            if groups[g].count == 0 {
                continue;
            }
            groups[g].stride = SETS_PER_CLASS * groups[g].count;
            let rows = TwoLevelBitmap::chunked_rows(nslabs[g]);
            tables[g] = Lazy::reserve(meta, 8 * rows * groups[g].stride, page)?;
            groups[g].base = tables[g].base;
        }
        Some(SetTable { groups, nslabs, pos, tables: Lock::new(tables), page })
    }

    /// Commit the rows of group `g` up to the level-0 word of `slab` (and
    /// so its chunk's level-1 word and every row before).
    fn ensure(&self, meta: &Meta, g: usize, slab: usize) -> Result<(), Errno> {
        let row = TwoLevelBitmap::chunked_l0_row(slab / 64);
        self.tables.lock()[g].ensure(meta, self.page, 8 * (row + 1) * self.groups[g].stride)
    }

    /// Committed byte ranges of the groups (tests, stats).
    pub fn committed_ranges(&self) -> [(usize, usize); SET_GROUPS] {
        let t = self.tables.lock();
        let mut out = [(0usize, 0usize); SET_GROUPS];
        for g in 0..SET_GROUPS {
            out[g] = (t[g].base, t[g].committed);
        }
        out
    }

    /// The commit lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> &RawLock {
        self.tables.raw()
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
/// `committed_slabs`, only when a group is handed out, and for `states`,
/// when an arena is built) and read by every `free`, while `inner` holds
/// the commit lock. `repr(C)` keeps the
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
    /// `log2(slabs_per_page)`: the guard test on the sized fast path is a
    /// shift, an add and a mask (`is_guard_slab`).
    lg_slabs_per_page: u8,
    _pad: [u8; 2],
    /// `guard_interval - 1`: every `guard_interval`-th page group is a
    /// guard group and the interval is a power of two, so `(group + 1) &
    /// guard_mask == 0` is the test. With guards off the mask is all ones
    /// and no page group a region can have matches (`u32::MAX` would).
    guard_mask: u32,
    lg_page: u32,
    /// `desc_stride(class)`.
    desc_stride: u32,
    /// `max(1, page / slab_size)`, a power of two. Slabs are committed,
    /// guarded and purged in page-sized groups, so on a kernel whose page is
    /// larger than this class's slab a group of `slabs_per_page` consecutive
    /// slabs is committed when its first slab is, is one guard region when
    /// it is a guard group, and is purged only when every slab in it is
    /// idle and owned by the purging arena. Since plan Phase 2.5 task 4e no
    /// supported kernel has such a class (`layout::adopt_page_geometry`
    /// raises a class's slab to the page at init, up to 64 KiB: a group of
    /// `Empty` sub-page slabs was never all idle under Valkey and held
    /// 79.5 MiB after its delete phase at 64 KiB pages), so this is 1
    /// everywhere but in the tests that fake a page above the slab, which
    /// keep the path working for a page above 64 KiB.
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
    /// Every arena's state over this table, an intrusive list through
    /// [`ClassState::next`] pushed by [`register_state`]: the donors a
    /// handoff visits. States are never freed (design 13.3), so the
    /// pointers stay valid, and the list only grows.
    ///
    /// [`register_state`]: SlabTable::register_state
    states: AtomicPtr<ClassState>,
    inner: Padded<Lock<TableInner>>,
    _tail: [u8; TABLE_TAIL],
}

/// Padding that keeps the stride of `[SlabTable; NCLASSES]` at 384 bytes,
/// three lines, off a multiple of 512 (see [`ClassState`]), whatever the
/// commit lock's state takes of the second and third.
const TABLE_TAIL: usize = 2 * PAD_ALIGN - core::mem::size_of::<Padded<Lock<TableInner>>>();

const _: () = assert!(core::mem::offset_of!(SlabTable, inner) == PAD_ALIGN);
const _: () = assert!(core::mem::size_of::<SlabTable>() == 3 * PAD_ALIGN);
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
    /// The decay step gate (`purge::Decayer`): the oldest `Empty` slab's
    /// `dirty_at`, written under the lock (set when the empty set goes
    /// from none to one, recomputed by every purge walk) and read without
    /// it by the decay driver, and the next walk's deadline.
    gate: Decayer,
    /// The next state over the same table (`SlabTable::register_state`),
    /// for the handoff's donor lookup; written once, under the commit
    /// lock.
    next: AtomicPtr<ClassState>,
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
    /// `malloc_stats_print`: slots claimed by
    /// compaction rebuilds, caches that entered and left compact mode, and
    /// probes the refill-rate gate declined. All written on cold paths.
    compacted: AtomicU64,
    compact_on: AtomicU64,
    compact_off: AtomicU64,
    gate_refill: AtomicU64,
    /// Times `bin_stats` clamped `claimed - released` to the capacity of
    /// the slabs that can hold live slots. A transient clamp is legitimate
    /// (`released` is added to after the slab's `live` reached zero, so a
    /// reader between the two sees an `Empty` slab and a count that still
    /// includes its slots); a count that keeps growing while nothing is
    /// freed is a bookkeeping bug. Printed in the bins table.
    clamped: AtomicU64,
    _tail: [u8; PAD_ALIGN - 64],
}

const _: () = assert!(core::mem::offset_of!(ClassState, inner) == PAD_ALIGN);
const _: () = assert!(core::mem::offset_of!(ClassState, gate) + core::mem::size_of::<Decayer>() <= PAD_ALIGN);
const _: () = assert!(core::mem::offset_of!(ClassState, claimed) % PAD_ALIGN == 0);
const _: () = assert!(core::mem::size_of::<ClassState>() % PAD_ALIGN == 0);
const _: () = assert!(core::mem::size_of::<ClassState>() % 512 != 0);

// SAFETY: the only non-`Sync`, non-`Send` field is the table pointer,
// which targets a `SlabTable` (itself `Sync`) that outlives every
// `ClassState` pointing at it: both live in `Shared` for the life of the
// process, or are leaked by the tests. Sharing the state across threads
// (`Sync`) or handing it to another thread (`Send`) therefore hands out
// nothing but a pointer to a process-lifetime, thread-safe table.
unsafe impl Sync for ClassState {}
unsafe impl Send for ClassState {}

#[cfg(target_os = "linux")]
mod os {
    use crate::sys::{self, Errno, MADV_GUARD_INSTALL, PROT_NONE, PROT_READ, PROT_WRITE};

    // SAFETY (all three): `addr..addr + len` is inside a class region this
    // allocator reserved (the callers pass slab or page-group ranges below
    // the region's end), so changing its protection or installing a guard
    // affects only pages nothing else in the process maps.
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
    /// every `N`-th page group a guard group; `N` must be a power of two
    /// (`config.rs` rounds a configured value up, `heap::guard_interval_for`
    /// scales by a power of two), so that the sized fast path's test is a
    /// mask; callers must pass 0 when `MADV_GUARD_INSTALL` is unavailable,
    /// since a `PROT_NONE` slab would split the region into one VMA per
    /// guard (design section 5).
    pub fn new(meta: &Meta, regions: &Regions, class: usize, guard_interval: u32) -> Option<SlabTable> {
        assert!(class < CLASSES.len(), "class: index out of range");
        assert!(guard_interval != 1, "class: a guard interval of 1 leaves no slabs to allocate");
        assert!(
            guard_interval == 0 || guard_interval.is_power_of_two(),
            "class: guard interval {guard_interval} is not a power of two"
        );
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
            lg_slabs_per_page: slabs_per_page.trailing_zeros() as u8,
            _pad: [0; 2],
            guard_mask: guard_interval.wrapping_sub(1),
            lg_page: regions.lg_page,
            desc_stride: desc_stride(class) as u32,
            slabs_per_page: slabs_per_page as u32,
            group: group as u32,
            base: regions.class_base[class],
            nslabs,
            desc_base: descs.base + stagger,
            states: AtomicPtr::new(core::ptr::null_mut()),
            inner: Padded(Lock::new(TableInner { descs, bits, owners, next_slab: 0, guards: 0 })),
            _tail: [0; TABLE_TAIL],
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
    /// every `guard_interval`-th slab. Arithmetic on the table line (a
    /// shift, an add and a mask: the interval is a power of two, and the
    /// runtime `%` this used to be was a 32-bit `divl` twice per Valkey SET
    /// on the `sdallocx` path, the largest single term of the sized path's
    /// instruction window on small-L3 hosts, plan Phase 2.5 task 4c), so
    /// the sized fast path can reject a slot start in a guard slab without
    /// a memory access: a guard slab's body past its first page is
    /// read-write memory that was never handed out. With guards off
    /// (`guard_mask == u32::MAX`) only page group `u32::MAX` would match,
    /// which no region has (at most 2^22 slabs), so there is no separate
    /// "off" branch.
    ///
    /// [`slabs_per_page`]: SlabTable::slabs_per_page
    #[inline(always)]
    pub fn is_guard_slab(&self, slab: usize) -> bool {
        let page_group = (slab >> self.lg_slabs_per_page) as u32;
        page_group.wrapping_add(1) & self.guard_mask == 0
    }

    /// The guard interval in page groups (0 when guards are off).
    #[inline]
    pub fn guard_interval(&self) -> u32 {
        self.guard_mask.wrapping_add(1)
    }

    /// `guard_interval - 1` (all ones when guards are off): the mask of
    /// [`is_guard_slab`], for the dense copy the sized fast path reads
    /// (`heap::GuardTest`).
    ///
    /// [`is_guard_slab`]: SlabTable::is_guard_slab
    #[inline]
    pub fn guard_mask(&self) -> u32 {
        self.guard_mask
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

    /// Tests: write `arena` into the owner word of `group` (committing its
    /// page first), a wild write into the owner array for `take_group`'s
    /// cross-check to find.
    #[cfg(test)]
    pub(crate) fn corrupt_owner_word(&self, meta: &Meta, group: usize, arena: u16) {
        let page = 1usize << self.lg_page;
        let stagger = desc_stagger(self.class as usize);
        let mut g = self.inner.lock();
        g.owners.ensure(meta, page, stagger + group * 2 + 2).expect("owner page");
        drop(g);
        // SAFETY: the word's page was just committed.
        unsafe { (*((self.owners_base + group * 2) as *const AtomicU16)).store(arena, Ordering::Relaxed) };
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

    /// [`check`] for a pointer already located (`Regions::classify`):
    /// aborts unless `slab` has been handed out and `slot`'s bit is set,
    /// with the pointer and its location; returns the owning arena.
    ///
    /// [`check`]: SlabTable::check
    #[inline]
    pub fn check_located(&self, slab: usize, slot: usize, p: usize) -> u16 {
        self.check_live(slab, slot, p);
        // SAFETY: `check_live` returned, so `slab` is below the watermark.
        unsafe { self.owner_unchecked(slab) }
    }

    /// The bit test of [`check_located`], without the owner: the exact
    /// check of `malloc_usable_size`, `realloc` and `rallocx` on a slot
    /// `Regions::classify` located (plan Phase 1.5 task 14). The loads that
    /// depend on the pointer are the `free` fast path's
    /// (`Shared::checked_slot`): the watermark, `bits_base` and
    /// `slab_bits` from this table line, then the class bitmap word; the
    /// slab and slot come from the caller's dense-array arithmetic and are
    /// not derived again from `base` and `CLASSES[class]` here (that was
    /// `locate`, a second dependent chain in front of the word load,
    /// `bench/results/2026-09-18-aarch64-gce-usable-size-chain.md`). The
    /// watermark is loaded `Relaxed` for the reason [`desc_if_committed`]
    /// gives: for a live slot the program's own ordering makes the group's
    /// words visible, and for anything else every observable value aborts.
    /// Aborts by name for a slab never handed out, a guard slab, or a
    /// clear bit; the guard test (a 32-bit division) runs only once the bit
    /// is clear, which it always is in a guard slab (`take_group` never
    /// sets one), so the messages are unchanged and the hot path is the
    /// watermark compare and the bit test.
    ///
    /// [`check_located`]: SlabTable::check_located
    /// [`desc_if_committed`]: SlabTable::desc_if_committed
    #[inline(always)]
    pub fn check_live(&self, slab: usize, slot: usize, p: usize) {
        if !self.is_handed_out(slab) {
            never_committed(self.class as usize, slab);
        }
        debug_assert!(slot < self.class().nslots());
        // SAFETY: below a value the watermark held, slot below `nslots`.
        if !unsafe { self.is_set_unchecked(slab, slot) } {
            self.not_live(slab, slot, p);
        }
    }

    /// The abort of [`check_live`] for a clear bit: a guard slab by name,
    /// otherwise a slot that is not allocated.
    ///
    /// [`check_live`]: SlabTable::check_live
    #[cold]
    #[inline(never)]
    fn not_live(&self, slab: usize, slot: usize, p: usize) -> ! {
        if self.is_guard_slab(slab) {
            guard_free(p, self.class as usize, slab);
        }
        invalid_free(p, self.class as usize, slab, slot)
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
        if purge::check_zero() && s != self.committed_slabs.load(Ordering::Relaxed) {
            panic!("check_zero: class {class}: take_group at next_slab {s} but the watermark is {}", self.committed_slabs.load(Ordering::Relaxed));
        }
        g.descs.ensure(meta, page, stagger + end * desc_stride(class)).ok()?;
        g.bits.ensure(meta, page, stagger + (end * cls.slab_bits()).div_ceil(8)).ok()?;
        g.owners.ensure(meta, page, stagger + ((end - 1) >> self.lg_group) * 2 + 2).ok()?;
        ensure_sets(end - 1).ok()?;
        // The owner word and the descriptors' arena field record the same
        // fact (design section 4) and are written together only here, so
        // a group about to be handed out must show them agreeing: both
        // zero for never-touched memory. A disagreement is a wild write
        // into one of the two arrays and aborts before the group is used.
        // SAFETY: the owner page for the group was committed above.
        let owner_word = unsafe { &*((self.owners_base + (s >> self.lg_group) * 2) as *const AtomicU16) };
        let previous = owner_word.load(Ordering::Relaxed);
        for slab in s..end {
            if self.is_guard_slab(slab) {
                continue;
            }
            // SAFETY: the descriptor page for `slab` was committed above.
            let recorded = unsafe { SlabDesc::at(self.desc_base, class, slab).arena() };
            if recorded != previous {
                owner_mismatch(class, s >> self.lg_group, slab, previous, recorded);
            }
        }
        for slab in s..end {
            if self.is_guard_slab(slab) {
                continue;
            }
            // SAFETY: the descriptor and bitmap pages for `slab` were
            // committed above.
            unsafe {
                if purge::check_zero() {
                    let d = SlabDesc::at(self.desc_base, class, slab);
                    let (st, sh) = d.state_bytes();
                    if sh != 0 || d.live() != 0 || st != 0 {
                        panic!(
                            "check_zero: class {class}: take_group re-hands slab {slab} (group {s}..{end}, arena {arena}): descriptor at {:#x} has state {st} shadow {sh} live {} arena {} dirty_at {}",
                            d as *const SlabDesc as usize,
                            d.live(),
                            d.arena(),
                            d.dirty_at()
                        );
                    }
                }
                SlabDesc::at(self.desc_base, class, slab).init(arena);
                self.bits_unchecked(slab).init(cls.nslots());
            }
        }
        owner_word.store(arena, Ordering::Relaxed);
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

    /// The owner word of `group`.
    ///
    /// # Safety
    /// The group must have been handed out (its owner page is committed).
    #[inline]
    unsafe fn owner_word(&self, group: usize) -> &AtomicU16 {
        // SAFETY: the caller guarantees the page is committed.
        unsafe { &*((self.owners_base + group * 2) as *const AtomicU16) }
    }

    /// Register an arena's state over this table for the handoff's donor
    /// walk. Called once per `(arena, class)` from `Arena::init`, after
    /// the state has its final address; states are never freed.
    pub fn register_state(&self, state: *const ClassState) {
        let state = state as *mut ClassState;
        let mut head = self.states.load(Ordering::Acquire);
        loop {
            // SAFETY: `state` is a live, never-freed `ClassState` that
            // nothing else reaches until it is published below.
            unsafe { (*state).next.store(head, Ordering::Relaxed) };
            match self.states.compare_exchange(head, state, Ordering::Release, Ordering::Acquire) {
                Ok(_) => return,
                Err(h) => head = h,
            }
        }
    }
}

impl ClassState {
    /// Create the state for `arena` over `table`'s class, with its three
    /// slab sets as views of the arena's `sets` table (slots `kind *
    /// NCLASSES + class`). `table` and `sets` must outlive the state.
    pub fn new(meta: &Meta, regions: &Regions, table: &SlabTable, arena: u16, sets: &SetTable) -> Option<ClassState> {
        let _ = (meta, regions);
        let class = table.class as usize;
        debug_assert_eq!(table.nslabs, sets.nslabs[SetTable::group_of(class)]);
        let partial = Set::in_table(sets, class, 0);
        let empty = Set::in_table(sets, class, 1);
        let purged = Set::in_table(sets, class, 2);
        Some(ClassState {
            class: table.class,
            arena,
            table,
            gate: Decayer::new(),
            next: AtomicPtr::new(core::ptr::null_mut()),
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
            clamped: AtomicU64::new(0),
            _tail: [0; PAD_ALIGN - 64],
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
        self.claim_with(meta, out, None)
    }

    /// [`claim`] for `slot_randomize` (design section 4, plan Phase 2 task
    /// 3): the same slabs and the same reservations (`plan` is untouched,
    /// so the slabs are still the lowest partial ones and a `Fresh` or
    /// `Purged` slab's prefix is still claimed under the lock), but the
    /// bit scan of every other pick starts at a word drawn from `rng`
    /// instead of word 0, and the batch is shuffled with `rng` before it
    /// is returned, so the cache hands out its slots in a random order
    /// instead of by ascending address. Shuffling changes the order of
    /// `out[..n]`, not the set of slots, so [`Claim::zeroed`] means what it
    /// always means: every slot came from a slab nothing else could touch.
    ///
    /// [`claim`]: ClassState::claim
    pub fn claim_random(&self, meta: &Meta, out: &mut [*mut u8], rng: &mut u64) -> Claim {
        let c = self.claim_with(meta, out, Some(rng));
        shuffle(&mut out[..c.n], rng);
        c
    }

    fn claim_with(&self, meta: &Meta, out: &mut [*mut u8], mut rng: Option<&mut u64>) -> Claim {
        let mut got = 0;
        let mut zeroed = true;
        let mut picks = [Pick { slab: 0, n: 0, zero: false, state: State::Active }; MAX_PICKS];
        while got < out.len() {
            let np = self.plan(meta, out.len() - got, &mut picks);
            if np == 0 {
                break;
            }
            got += self.finish(&picks[..np], &mut out[got..], &mut zeroed, rng.as_deref_mut());
        }
        Claim { n: got, zeroed: got > 0 && zeroed }
    }

    /// The unlocked half of a refill: write the slots of `picks` (one
    /// lock hold of [`plan`]) to `out`, lowest address first, and return
    /// how many. A pick flagged zero had its slots decided under the lock
    /// in [`take`] (the lowest `n` of a slab nothing else could touch), so
    /// only the other picks run the bit loop here; `zeroed` is cleared
    /// when any of them is not zero.
    ///
    /// With `rng` (`slot_randomize`) the bit loop of a pick starts at a
    /// random word rather than word 0; a rescan after a full pass still
    /// starts at 0. Without it the code is the address-ordered claim.
    ///
    /// [`plan`]: ClassState::plan
    /// [`take`]: ClassState::take
    fn finish(&self, picks: &[Pick], out: &mut [*mut u8], zeroed: &mut bool, mut rng: Option<&mut u64>) -> usize {
        let cls = self.class();
        let check_zero = purge::check_zero();
        let mut got = 0;
        for pick in picks {
            let slab = pick.slab as usize;
            let want = pick.n as usize;
            if pick.zero {
                // Bits `0..want` were claimed under the lock (`claim_prefix`).
                for k in 0..want {
                    out[got + k] = self.slot_ptr(slab, k);
                }
                if check_zero {
                    self.check_zero(&out[got..got + want], pick);
                }
                got += want;
                continue;
            }
            *zeroed = false;
            let bm = self.table().bits(slab);
            let words = bm.words();
            let mut k = 0;
            let mut word = match rng.as_deref_mut() {
                Some(r) => (xorshift(r) as usize) % words,
                None => 0,
            };
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
            got += want;
        }
        got
    }

    /// The `check_zero` debug mode: the first and
    /// last word of every slot of a pick reported zero must read zero.
    /// Aborts with everything the C-level assertion cannot say: the class,
    /// slab, the slab's state when the refill selected it (`Fresh` blames
    /// the commit path, `Purged` the discard path), the arena, the discard
    /// backend and the offending word.
    #[cold]
    #[inline(never)]
    fn check_zero(&self, slots: &[*mut u8], pick: &Pick) {
        let size = self.class().size();
        for &p in slots {
            for off in [0, size - 8] {
                // SAFETY: `p` is a slot this claim just reserved in a
                // committed slab; slots are at least 16 bytes and 8-aligned.
                let word = unsafe { core::ptr::read_volatile(p.add(off) as *const u64) };
                if word != 0 {
                    let d = self.desc(pick.slab as usize);
                    let bm = self.table().bits(pick.slab as usize);
                    let now = (d.state(), d.live(), d.arena(), bm.allocated(self.class().nslots()), self.class().nslots(), d as *const SlabDesc as usize);
                    dirty_zero_slot(self.class as usize, size, pick, self.arena, p as usize, off, word, now);
                }
            }
        }
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
        let mut wake = false;
        let np = self.plan_locked(meta, want, picks, cls, &mut wake);
        if wake {
            purge::note_empty();
        }
        np
    }

    /// [`plan`] under the lock; `wake` is set when a handoff gave this
    /// arena its first `Empty` slab, which the caller reports to the
    /// background thread once the lock is dropped (as `on_live_zero`).
    ///
    /// [`plan`]: ClassState::plan
    fn plan_locked(&self, meta: &Meta, want: usize, picks: &mut [Pick; MAX_PICKS], cls: &Class, wake: &mut bool) -> usize {
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
            let s = match self.next_slab(&mut g, meta, wake) {
                Some(s) => s,
                None => break,
            };
            self.take(&mut g, s, &mut remaining, picks, &mut np);
        }
        np
    }

    /// The slab a refill uses when no partial slab has room (design
    /// section 5): the lowest `Empty` slab, else the lowest `Purged` one,
    /// else the next slab of this arena's current group, else a slab of
    /// an idle group taken over from another arena
    /// ([`adopt_idle_group`]), else the next fresh group of the class
    /// region ([`SlabTable::take_group`], which commits this arena's set
    /// words for the whole group first). Guard slabs inside a group are
    /// committed as guards and skipped. The commit lock nests inside the
    /// class lock. `None` when the region, the metadata region or a
    /// commit is exhausted.
    ///
    /// [`adopt_idle_group`]: ClassState::adopt_idle_group
    fn next_slab(&self, g: &mut Inner, meta: &Meta, wake: &mut bool) -> Option<usize> {
        let table = self.table();
        let page = 1usize << table.lg_page;
        loop {
            if let Some(s) = g.empty.map.lowest_set() {
                g.empty.map.clear(s);
                g.stats.empty = g.stats.empty.checked_sub(1).unwrap_or_else(|| slab_count_underflow("empty"));
                return Some(s);
            }
            if let Some(s) = g.purged.map.lowest_set() {
                g.purged.map.clear(s);
                g.stats.purged = g.stats.purged.checked_sub(1).unwrap_or_else(|| slab_count_underflow("purged"));
                return Some(s);
            }
            if g.group_next >= g.group_end {
                if self.adopt_idle_group(g, meta, wake) {
                    continue;
                }
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

    /// Take over an idle group of this class from another arena (design
    /// 13.3: an arena owns its groups until it lends an idle one). Called
    /// with this arena's class lock held, its sets empty and its current
    /// group used up. Visits every other registered arena state: locks it,
    /// looks for an idle group in its `empty` and `purged` sets
    /// ([`next_idle_group`]) and, finding one, moves it
    /// ([`transfer_group`]). Returns `true` when a group changed hands:
    /// its `Empty` and `Purged` slabs are now in this arena's sets and its
    /// uncommitted slabs, if it was the donor's current group, are this
    /// arena's current group. Only the fresh-commit path pays for this;
    /// the slab transitions themselves keep no extra state.
    ///
    /// Lock order. This arena's class lock is held and every donor is
    /// only `try_lock`ed, never waited for: a busy donor is skipped and
    /// the fresh group is the fallback, so no thread ever blocks while
    /// holding a class lock and there is no order to keep (the fork
    /// handlers' order, class locks by arena index then the commit locks
    /// then the set tables, is untouched: `take_group`'s nesting is the
    /// only blocking chain). Blocking on higher-index donors was tried
    /// first: on `xmalloc`, where every donor is a producer whose lock
    /// also serves its consumers' flushes, the takers joined the convoy.
    /// The donor's lock pins the states of its slabs and the group's
    /// ownership, so what the scan finds is what the transfer moves.
    ///
    /// [`next_idle_group`]: ClassState::next_idle_group
    /// [`transfer_group`]: ClassState::transfer_group
    fn adopt_idle_group(&self, g: &mut Inner, meta: &Meta, wake: &mut bool) -> bool {
        if !GROUP_HANDOFF.load(Ordering::Relaxed) {
            return false;
        }
        let table = self.table();
        let mut p = table.states.load(Ordering::Acquire);
        while !p.is_null() {
            // SAFETY: registered states are never freed (`register_state`).
            let donor: &ClassState = unsafe { &*p };
            p = donor.next.load(Ordering::Relaxed);
            if donor.arena == self.arena {
                continue;
            }
            debug_assert!(core::ptr::eq(donor.table, self.table));
            let Some(mut dg) = donor.inner.try_lock() else {
                continue;
            };
            let mut budget = MAX_ADOPT_TRIES;
            if let Some(group) = self.next_idle_group(&dg, 0, &mut budget) {
                return self.transfer_group(g, &mut dg, donor, meta, group, wake);
            }
        }
        false
    }

    /// The lowest idle group of the arena whose class lock `dg` is held,
    /// at or above slab `from`: a group whose every committed non-guard
    /// slab is in `dg`'s `empty` or `purged` set (its committed slabs are
    /// those below the arena's cursor when it is the arena's current
    /// group, all of them otherwise). Walks both sets in address order,
    /// one group per step, and gives up after `budget` groups.
    fn next_idle_group(&self, dg: &Inner, mut from: usize, budget: &mut usize) -> Option<usize> {
        let table = self.table();
        while from < table.nslabs && *budget > 0 {
            let s = match (dg.empty.map.lowest_set_from(from), dg.purged.map.lowest_set_from(from)) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => return None,
            };
            let group = s >> table.lg_group;
            let start = group << table.lg_group;
            let end = (start + table.group as usize).min(table.nslabs);
            let cursor = if dg.group_end == end { dg.group_next } else { end };
            if (start..cursor).all(|x| table.is_guard_slab(x) || dg.empty.map.is_set(x) || dg.purged.map.is_set(x)) {
                return Some(group);
            }
            *budget -= 1;
            from = end;
        }
        None
    }

    /// The handoff itself, with this arena's class lock (`g`) and the
    /// donor's (`dg`) held. `false` only when this arena's set words for
    /// the group cannot be committed (done first: the verification reads
    /// them, and on Linux an uncommitted metadata page faults), with
    /// nothing changed. Aborts when
    /// the descriptors disagree with the donor's sets (an `Active` slab, a
    /// live slot, a slab of another arena, a partial bit): a heap
    /// inconsistency. The owner word and the descriptors' arena field are
    /// rewritten together, as `take_group` writes them, so its cross-check
    /// still holds for every group.
    fn transfer_group(&self, g: &mut Inner, dg: &mut Inner, donor: &ClassState, meta: &Meta, group: usize, wake: &mut bool) -> bool {
        let table = self.table();
        let class = self.class as usize;
        let page = 1usize << table.lg_page;
        let start = group << table.lg_group;
        let end = (start + table.group as usize).min(table.nslabs);
        // SAFETY: the group holds a slab of the donor's sets, so it was
        // handed out and its owner page is committed.
        let owner_word = unsafe { table.owner_word(group) };
        let word = owner_word.load(Ordering::Relaxed);
        if word != donor.arena {
            owner_mismatch(class, group, start, word, donor.arena);
        }
        debug_assert!(g.group_next >= g.group_end);
        // This arena's set words for the group, before anything is read
        // from them (they may be beyond this arena's committed rows) or
        // changed: a failure here changes nothing.
        if g.partial.ensure(meta, page, end - 1).is_err()
            || g.empty.ensure(meta, page, end - 1).is_err()
            || g.purged.ensure(meta, page, end - 1).is_err()
        {
            return false;
        }
        // Slabs below the donor's cursor are committed; the rest of its
        // current group is not and becomes this arena's current group.
        let cursor = if dg.group_end == end { dg.group_next } else { end };
        for s in start..end {
            if table.is_guard_slab(s) {
                continue;
            }
            if purge::check_zero() {
                self.check_shadow(s, "adopt");
            }
            let d = self.desc(s);
            if d.arena() != donor.arena {
                owner_mismatch(class, group, s, donor.arena, d.arena());
            }
            let state = d.state();
            let in_sets = match state {
                State::Empty => dg.empty.map.is_set(s),
                State::Purged => dg.purged.map.is_set(s),
                State::Fresh => s >= cursor && !dg.empty.map.is_set(s) && !dg.purged.map.is_set(s),
                State::Active => false,
            };
            if !in_sets || d.live() != 0 || dg.partial.map.is_set(s) || g.partial.map.is_set(s) || g.empty.map.is_set(s) || g.purged.map.is_set(s) {
                idle_group_in_use(class, group, s, donor.arena, self.arena, state, d.live(), in_sets, dg.partial.map.is_set(s));
            }
        }
        let mut oldest = 0u64;
        for s in start..end {
            if table.is_guard_slab(s) {
                continue;
            }
            let d = self.desc(s);
            match d.state() {
                State::Empty => {
                    dg.empty.map.clear(s);
                    dg.stats.empty = dg.stats.empty.checked_sub(1).unwrap_or_else(|| slab_count_underflow("empty"));
                    g.empty.map.set(s);
                    g.stats.empty += 1;
                    let t = d.dirty_at();
                    if oldest == 0 || t < oldest {
                        oldest = t;
                    }
                }
                State::Purged => {
                    dg.purged.map.clear(s);
                    dg.stats.purged = dg.stats.purged.checked_sub(1).unwrap_or_else(|| slab_count_underflow("purged"));
                    g.purged.map.set(s);
                    g.stats.purged += 1;
                }
                State::Fresh | State::Active => {}
            }
            if s < cursor {
                dg.stats.committed = dg.stats.committed.checked_sub(1).unwrap_or_else(|| slab_count_underflow("committed"));
                g.stats.committed += 1;
            }
            d.set_arena(self.arena);
        }
        owner_word.store(self.arena, Ordering::Relaxed);
        #[cfg(test)]
        HANDOFFS.fetch_add(1, Ordering::Relaxed);
        if dg.group_end == end {
            g.group_next = dg.group_next;
            g.group_end = end;
            dg.group_next = end;
            dg.group_end = end;
        }
        // This arena's oldest `Empty` slab may now be one of the moved
        // ones. The donor's gate is left as it is: at worst it is too
        // early, and its next walk recomputes it (see `purge::Decayer`).
        if oldest != 0 {
            let cur = self.gate.oldest_empty();
            if (cur == 0 || oldest < cur) && self.gate.set_oldest_empty(oldest) {
                *wake = true;
            }
        }
        true
    }

    /// Reserve `min(nslots - live, *remaining)` slots of `slab` under the
    /// lock, fix up its state and partial bit, and record the pick.
    fn take(&self, g: &mut Inner, slab: usize, remaining: &mut usize, picks: &mut [Pick; MAX_PICKS], np: &mut usize) {
        let desc = self.desc(slab);
        let live = desc.live();
        debug_assert_eq!(self.table().owner(slab), self.arena, "take: slab {slab} state {:?} live {live}", desc.state());
        debug_assert_eq!(desc.arena(), self.arena, "take: slab {slab} descriptor arena");
        // `live > nslots` would reserve a wrapped count (the release
        // profile does not check overflow; the dev profile does).
        assert!(live <= self.class().nslots(), "class: slab live count above its slot count");
        let avail = self.class().nslots() - live;
        let n = avail.min(*remaining);
        let state = desc.state();
        if purge::check_zero() {
            self.check_shadow(slab, "take");
            if state == State::Fresh {
                if live != 0 {
                    fresh_with_live(self.class as usize, slab, self.arena, desc, self.table().bits(slab).allocated(self.class().nslots()));
                }
                self.check_fresh_slab_is_zero(slab);
            }
        }
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
        if zero {
            self.claim_prefix(slab, n, state, live);
        }
        picks[*np] = Pick { slab: slab as u32, n: n as u32, zero, state };
        *np += 1;
        *remaining -= n;
    }

    /// The slots of a zero pick, decided under the class lock (plan Phase
    /// 1.5 task 7c). A `Fresh` or `Purged` slab has `live == 0` and no bit
    /// set, so its lowest `n` bits are free and nothing contends for them.
    /// Setting them here, in the lock hold that makes the slab `Active` and
    /// partial, is what makes the pick's zero flag sound: decided after
    /// the lock is dropped (the bit loop of [`finish`]), a bit could be one
    /// that another refill claimed from the now-partial slab, handed out,
    /// and got back dirty in the meantime. Anything but a clean claim of
    /// bits `0..n` means `live` no longer bounds the set bits: abort.
    ///
    /// [`finish`]: ClassState::finish
    fn claim_prefix(&self, slab: usize, n: usize, state: State, live: usize) {
        let bm = self.table().bits(slab);
        let mut k = 0;
        let mut w = 0;
        while k < n {
            let want = (n - k).min(64);
            let expect = if want == 64 { !0 } else { (1u64 << want) - 1 };
            let got = bm.claim_word(w, want);
            if got != expect {
                zero_slab_had_bits_set(self.class as usize, slab, self.arena, state, live, w, got, expect);
            }
            k += want;
            w += 1;
        }
    }

    /// `check_zero`: the first word of every slot of a `Fresh` slab must
    /// read zero before its first claim; a dirty one means the memory was
    /// written before this table ever handed the slab out.
    #[cold]
    #[inline(never)]
    fn check_fresh_slab_is_zero(&self, slab: usize) {
        let cls = self.class();
        let bm = self.table().bits(slab);
        for slot in 0..cls.nslots() {
            let p = self.slot_ptr(slab, slot);
            // SAFETY: a slot of a committed slab; the class lock is held.
            let word = unsafe { core::ptr::read_volatile(p as *const u64) };
            if word != 0 {
                panic!(
                    "check_zero: class {} slab {slab} (arena {}) is Fresh before its first claim but slot {slot} at {:#x} reads {word:#x} ({} of {} bits set, watermark {}, descriptor at {:#x})",
                    self.class,
                    self.arena,
                    p as usize,
                    bm.allocated(cls.nslots()),
                    cls.nslots(),
                    self.table().committed_slabs(),
                    self.desc(slab) as *const SlabDesc as usize
                );
            }
        }
    }

    /// `check_zero`: abort when `slab`'s state byte disagrees with its
    /// shadow, i.e. something outside `SlabDesc` wrote the descriptor.
    #[cold]
    #[inline(never)]
    fn check_shadow(&self, slab: usize, at: &str) {
        let d = self.desc(slab);
        let (state, shadow) = d.state_bytes();
        if shadow != state.wrapping_add(1) {
            panic!(
                "check_zero: class {} slab {slab} (arena {}) descriptor at {:#x} was written outside the API, seen at {at}: state byte {state:#x}, shadow {shadow:#x} (says {:?}), live {}, arena field {}, dirty_at {}",
                self.class,
                self.arena,
                d as *const SlabDesc as usize,
                State::try_from_u8(shadow.wrapping_sub(1)),
                d.live(),
                d.arena(),
                d.dirty_at()
            )
        }
    }

    /// `live` reached zero: make the slab `Empty` unless a refill has
    /// already reserved it again.
    fn on_live_zero(&self, slab: usize) {
        let mut first = false;
        {
            let mut g = self.inner.lock();
            if purge::check_zero() {
                self.check_shadow(slab, "on_live_zero");
            }
            // The slab may have changed arena since `sub_live` (another
            // thread of this arena emptied it, another arena adopted it and
            // is using it): then it is no business of this arena's sets.
            if self.table().owner(slab) != self.arena {
                return;
            }
            let desc = self.desc(slab);
            if desc.live() == 0 && desc.state() == State::Active {
                let now = os::now_ns().max(1);
                desc.set_state(State::Empty);
                desc.set_dirty_at(now);
                g.clear_partial(slab);
                g.empty.map.set(slab);
                g.stats.empty += 1;
                if self.gate.note_first_empty(now) {
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
    /// The batch is grouped by slab (`Heap::flush`'s bucket pass; a sorted
    /// batch is one such grouping), so a slab's pointers are contiguous and
    /// the prefix ends exactly at the first pointer of a slab of another
    /// arena; the caller routes the rest there (`Heap::flush`: a home cache
    /// takes sized frees of any arena, design 6.3). Within a slab the order
    /// is free: the slab's bits are accumulated in a scratch bitmap
    /// (`MAX_SLAB_WORDS` words on the stack) and released at the slab
    /// switch, one atomic per touched word, so an unsorted slab costs the
    /// same atomics as a sorted one. Every bit cleared is checked (an
    /// already-clear bit is a double or invalid free and aborts), which is
    /// where the sized path's deferred check lands; a duplicate inside one
    /// slab's group is caught earlier, on the scratch bit. The owner is read
    /// from the table's dense owner array, once per slab switch.
    ///
    /// [`release`]: ClassState::release
    pub fn release_owned(&self, ptrs: &[*mut u8]) -> usize {
        let mut local = [0u64; MAX_SLAB_WORDS];
        let mut cur_slab = usize::MAX;
        let (mut lo_w, mut hi_w) = (usize::MAX, 0usize);
        let mut count = 0usize;
        let mut reopen = Reopen::default();
        let mut end = ptrs.len();
        for (i, &p) in ptrs.iter().enumerate() {
            let (slab, slot) = self.locate(p);
            if slab != cur_slab {
                if count != 0 {
                    self.release_slab(cur_slab, &mut local, lo_w, hi_w, count, &mut reopen);
                    count = 0;
                    lo_w = usize::MAX;
                    hi_w = 0;
                }
                if self.table().owner(slab) != self.arena {
                    end = i;
                    break;
                }
                // The sized path defers its checks to here (design
                // 6.3). A guard slab or a slab nothing was ever
                // allocated from (`Fresh`: handed to an arena but not
                // committed, or committed and never claimed) has no
                // slot to release; its bits are clear, so the bit
                // test below would abort too, but this names the
                // cause. The descriptor line is read by `dec_live`
                // anyway.
                if self.table().is_guard_slab(slab) {
                    guard_free(p as usize, self.class as usize, slab);
                }
                if self.desc(slab).state() == State::Fresh {
                    release_into_fresh_slab(p as usize, self.class as usize, slab);
                }
                cur_slab = slab;
            }
            let word = slot / 64;
            let bit = 1u64 << (slot % 64);
            // `slot < nslots <= slab_bits <= 64 * MAX_SLAB_WORDS`
            // (`locate` checked the slot), so `word` is in range.
            let w = &mut local[word];
            if *w & bit != 0 {
                double_free(p as usize, self.class as usize, slab, slot);
            }
            *w |= bit;
            count += 1;
            lo_w = lo_w.min(word);
            hi_w = hi_w.max(word);
        }
        if count != 0 {
            self.release_slab(cur_slab, &mut local, lo_w, hi_w, count, &mut reopen);
        }
        self.reopen(&mut reopen);
        if end != 0 {
            self.released.fetch_add(end as u64, Ordering::Relaxed);
        }
        end
    }

    /// Clear the bits `local[lo..=hi]` accumulated for `slab` (one atomic
    /// per non-zero word), zero them for the next slab, and take `count`
    /// off the slab's `live`.
    fn release_slab(&self, slab: usize, local: &mut [u64; MAX_SLAB_WORDS], lo: usize, hi: usize, count: usize, reopen: &mut Reopen) {
        for w in lo..=hi {
            let mask = local[w];
            if mask != 0 {
                local[w] = 0;
                self.release_word(slab, w, mask, reopen);
            }
        }
        self.dec_live(slab, count);
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
            // Between the word's clear and this lock hold the slab may have
            // gone `Empty` (in `dec_live`), been adopted by another arena
            // and been made `Active` there: `Active` alone no longer means
            // "ours", the owner word under the lock does.
            if self.desc(slab).state() == State::Active && self.table().owner(slab) == self.arena {
                g.set_partial(slab);
            }
        }
        reopen.len = 0;
    }

    fn dec_live(&self, slab: usize, n: usize) {
        if purge::check_zero() {
            self.check_shadow(slab, "release");
            if self.desc(slab).state() == State::Fresh {
                release_into_fresh(self.class as usize, slab, self.arena, self.desc(slab), n);
            }
        }
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
                n += self.purge_group(&mut g, slab, spp, &mut batch);
            } else if oldest == 0 || dirty_at < oldest {
                oldest = dirty_at;
            }
            next = g.empty.map.lowest_set_from(slab + 1);
        }
        batch.flush();
        self.gate.set_oldest_empty(oldest);
        n
    }

    /// Discard the whole page group of `slab` (one slab when the slab is
    /// at least a page) and make every `Empty` slab in it `Purged`, a
    /// younger one included, since nothing in the group is in use. Under
    /// the class lock; the caller checked the group is idle. Returns the
    /// number of slabs purged.
    fn purge_group(&self, g: &mut Inner, slab: usize, spp: usize, batch: &mut Batch) -> usize {
        let first = slab & !(spp - 1);
        batch.push(self.slab_addr(first), self.class().slab_size() * spp);
        let mut n = 0;
        for s in first..first + spp {
            let d = self.desc(s);
            if d.state() != State::Empty {
                continue;
            }
            d.set_state(State::Purged);
            d.set_dirty_at(0);
            g.empty.map.clear(s);
            g.purged.map.set(s);
            g.stats.empty = g.stats.empty.checked_sub(1).unwrap_or_else(|| slab_count_underflow("empty"));
            g.stats.purged += 1;
            n += 1;
        }
        n
    }

    /// Decay walk (design section 8): purge the oldest `Empty` slabs until
    /// the number left is within the curve's allowance at `now`, the sum
    /// over the `Empty` slabs of the weight of their age, rounded up. Slabs
    /// are purged oldest step first; within the cut-off step the highest
    /// addresses go, so the lowest, which a claim takes first, stay. The
    /// discards go out in batches under the class lock, as `purge_empty`.
    /// Recomputes `oldest_empty` from the survivors. Returns the number
    /// purged.
    fn purge_curve(&self, now: u64, curve: &Curve, keep16: u32) -> usize {
        let spp = self.table().slabs_per_page();
        let mut g = self.inner.lock();
        // First pass: the age histogram of the empty set and its allowance.
        let mut counts = [0u32; STEPS];
        let mut total = 0usize;
        let mut weighted = 0u64;
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
            let step = curve.step(now, dirty_at);
            counts[step] += 1;
            total += 1;
            weighted += Curve::weight(step);
            if oldest == 0 || dirty_at < oldest {
                oldest = dirty_at;
            }
            next = g.empty.map.lowest_set_from(slab + 1);
        }
        // The cgroup policy's retention cap (design 11): this walk's share
        // of the process-wide cap, on top of the curve.
        let mut allow = Curve::allowance(weighted);
        if keep16 < crate::policy::KEEP_ALL {
            allow = allow.min(crate::policy::mul16(total as u64, keep16) as usize);
        }
        if total <= allow {
            self.gate.set_oldest_empty(oldest);
            return 0;
        }
        // The cut-off step: every slab in an older step goes and all but
        // the first `keep` of this one.
        let need = total - allow;
        let mut cut = STEPS;
        let mut acc = 0usize;
        while acc < need {
            cut -= 1;
            acc += counts[cut] as usize;
        }
        let mut keep = acc - need;
        // Second pass: purge.
        let mut n = 0;
        let mut batch = Batch::new();
        oldest = 0;
        let mut next = g.empty.map.lowest_set();
        while let Some(slab) = next {
            let dirty_at = self.desc(slab).dirty_at();
            let step = curve.step(now, dirty_at);
            let purge = step > cut
                || (step == cut && {
                    if keep > 0 {
                        keep -= 1;
                        false
                    } else {
                        true
                    }
                });
            if purge && (spp == 1 || self.page_group_is_idle(slab, spp)) {
                n += self.purge_group(&mut g, slab, spp, &mut batch);
            } else if oldest == 0 || dirty_at < oldest {
                oldest = dirty_at;
            }
            next = g.empty.map.lowest_set_from(slab + 1);
        }
        batch.flush();
        self.gate.set_oldest_empty(oldest);
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
    /// (0 when none; see the gate).
    pub fn oldest_empty(&self) -> u64 {
        self.gate.oldest_empty()
    }

    /// Decay step (design section 8): the gate runs `purge_curve` over
    /// the empty set at most once per step of the curve and returns the
    /// class's next deadline, `None` when nothing is `Empty`. Takes the
    /// lock only when a walk is due.
    pub fn decay(&self, now: u64, decay_ns: u64) -> Option<u64> {
        self.decay_with(now, decay_ns, crate::policy::KEEP_ALL)
    }

    /// [`decay`](ClassState::decay) with the cgroup policy's keep ratio
    /// (design 11): the walk keeps at most `keep16 / 65536` of its `Empty`
    /// slabs on top of the curve's allowance.
    pub fn decay_with(&self, now: u64, decay_ns: u64, keep16: u32) -> Option<u64> {
        self.gate.decay(now, decay_ns, |now, curve| {
            self.purge_curve(now, curve, keep16);
        })
    }

    /// Bytes of this arena's `Empty` slabs of the class (one lock hold).
    pub fn empty_bytes(&self) -> usize {
        self.inner.lock().stats.empty * self.class().slab_size()
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
    /// which keeps `active >= allocated` exact for `stats.rs`. Every clamp
    /// is counted (`clamped`); the count cannot tell the legitimate
    /// transient from a bookkeeping bug, the reader of the bins table can.
    pub fn bin_stats(&self) -> (ClassStats, usize) {
        let released = self.released.load(Ordering::Relaxed);
        let g = self.inner.lock();
        let mut s = g.stats;
        let claimed = self.claimed.load(Ordering::Relaxed);
        drop(g);
        s.guards = self.table().guards();
        let cap = s.curslabs() * self.class().nslots();
        let out = claimed.wrapping_sub(released) as usize;
        let curregs = if out > cap {
            self.clamped.fetch_add(1, Ordering::Relaxed);
            cap
        } else {
            out
        };
        (s, curregs)
    }

    /// Times `bin_stats` clamped `curregs` (see the field).
    #[inline]
    pub fn clamped(&self) -> u64 {
        self.clamped.load(Ordering::Relaxed)
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
        let capacity = s.curslabs() * self.class().nslots();
        (capacity.saturating_sub(curregs), capacity)
    }

    /// Lowest slab in the partial set, if any (for tests and stats).
    pub fn lowest_partial(&self) -> Option<usize> {
        self.inner.lock().partial.map.lowest_set()
    }

    /// Whether `slab` is in the partial set.
    /// Tests: this arena's idle groups, the ones a handoff would offer.
    #[cfg(test)]
    pub(crate) fn idle_groups(&self) -> std::vec::Vec<usize> {
        let g = self.inner.lock();
        let mut out = std::vec::Vec::new();
        let mut from = 0;
        let mut budget = usize::MAX;
        while let Some(group) = self.next_idle_group(&g, from, &mut budget) {
            out.push(group);
            from = (group + 1) << self.table().lg_group;
        }
        out
    }

    /// Tests: whether `slab` is in this arena's partial, empty and purged
    /// sets.
    #[cfg(test)]
    pub(crate) fn set_membership(&self, slab: usize) -> (bool, bool, bool) {
        let g = self.inner.lock();
        (g.partial.map.is_set(slab), g.empty.map.is_set(slab), g.purged.map.is_set(slab))
    }

    pub fn is_partial(&self, slab: usize) -> bool {
        self.inner.lock().partial.map.is_set(slab)
    }
}

#[cold]
#[inline(never)]
fn never_committed(class: usize, slab: usize) -> ! {
    panic!("invalid free: class {class} slab {slab} was never allocated")
}

#[cold]
#[inline(never)]
fn owner_mismatch(class: usize, group: usize, slab: usize, word: u16, desc: u16) -> ! {
    panic!(
        "heap inconsistency: class {class} group {group}: owner word says arena {word}, slab {slab}'s descriptor says arena {desc}"
    )
}

/// A `Fresh` or `Purged` slab selected by a refill had a bit set before
/// its slots were claimed under the lock: `live` no longer bounds the set
/// bits, so the bitmap or the descriptor is corrupt.
#[cold]
#[inline(never)]
fn idle_group_in_use(class: usize, group: usize, slab: usize, donor: u16, taker: u16, state: State, live: usize, in_sets: bool, partial: bool) -> ! {
    panic!(
        "heap inconsistency: class {class} group {group}: idle by arena {donor}'s sets but slab {slab} is {state:?} with live {live}, in its owner's set {in_sets}, partial {partial} (taker arena {taker})"
    )
}

#[cold]
#[inline(never)]
fn zero_slab_had_bits_set(class: usize, slab: usize, arena: u16, state: State, live: usize, word: usize, got: u64, expect: u64) -> ! {
    panic!(
        "class {class} slab {slab} (arena {arena}) is {state:?} with live {live} but word {word} of its bitmap had bits set before the refill's claim: claimed {got:#x}, expected {expect:#x}"
    )
}

/// The `free` check's abort (design section 4): the slot's bit is clear,
/// which is an invalid free or a stale double free.
/// A slot start inside a guard slab (`is_guard_slab`): never allocated
/// from, so any free of it is invalid.
#[cold]
#[inline(never)]
fn guard_free(p: usize, class: usize, slab: usize) -> ! {
    panic!("invalid free of {p:#x} (class {class} slab {slab}): guard slab")
}

/// A release into a `Fresh` slab: handed to an arena but not committed
/// (at or above the arena's commit cursor), or committed and never
/// claimed. No slot of it was ever handed out.
#[cold]
#[inline(never)]
fn release_into_fresh_slab(p: usize, class: usize, slab: usize) -> ! {
    panic!("invalid free of {p:#x} (class {class} slab {slab}): the slab has never been allocated from")
}

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

/// A slab count of `ClassStats` went below zero: the sets and the counts
/// disagree, which is a heap inconsistency.
#[cold]
#[inline(never)]
fn slab_count_underflow(which: &str) -> ! {
    panic!("class: {which} slab count underflow")
}

/// `check_zero` invariant: a slab with live slots was `Fresh` at selection.
#[cold]
#[inline(never)]
fn fresh_with_live(class: usize, slab: usize, arena: u16, desc: &SlabDesc, set: usize) -> ! {
    panic!(
        "check_zero: class {class} slab {slab} (arena {arena}) is Fresh with live {} ({set} bits set, owner {}, dirty_at {}), descriptor at {:#x}",
        desc.live(),
        desc.arena(),
        desc.dirty_at(),
        desc as *const SlabDesc as usize
    )
}

/// `check_zero` invariant: a release reached a slab that reads `Fresh`.
#[cold]
#[inline(never)]
fn release_into_fresh(class: usize, slab: usize, arena: u16, desc: &SlabDesc, n: usize) -> ! {
    panic!(
        "check_zero: release of {n} slot(s) into class {class} slab {slab} (arena {arena}) whose descriptor reads Fresh (live {}, owner {}, dirty_at {}), descriptor at {:#x}",
        desc.live(),
        desc.arena(),
        desc.dirty_at(),
        desc as *const SlabDesc as usize
    )
}

/// The `check_zero` abort: a slot handed out as zero has a dirty word.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn dirty_zero_slot(
    class: usize,
    size: usize,
    pick: &Pick,
    arena: u16,
    p: usize,
    off: usize,
    word: u64,
    now: (State, usize, u16, usize, usize, usize),
) -> ! {
    let (backend, advice) = purge::backend_name();
    let (state, live, owner, set, nslots, _) = now;
    panic!(
        "check_zero: slot {p:#x} (class {class}, size {size}, slab {}, arena {arena}) was handed out as zero from a {:?} slab but its word at offset {off} reads {word:#x} (discard backend {backend}, {advice}; descriptor at {:#x} now {state:?} live {live} owner {owner}, {set} of {nslots} bits set, pick of {} slots)",
        pick.slab, pick.state, now.5, pick.n
    )
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

    /// The page size the environment is built with: the kernel's on Linux
    /// (the reservations are real; the 64 KiB-page matrix cell found the
    /// 4 KiB constant), 4 KiB in the synthetic host environment.
    #[cfg(target_os = "linux")]
    fn page_size() -> usize {
        crate::sys::features::page_size()
    }

    #[cfg(not(target_os = "linux"))]
    fn page_size() -> usize {
        4096
    }
    /// Smallest layout `from_parts` accepts: 4 MiB class regions, so the
    /// 64 KiB classes have 64 slabs and the 4 MiB class has one.
    const LG_REGION: u32 = LG_MAX_SLAB;
    const LG_STRIDE: u32 = LG_REGION + 2;
    /// Test metadata: 8 MiB on a 4 KiB kernel, scaled with the page (every
    /// table reservation is rounded up to a page, and `works_for_every_class`
    /// reserves three per class).
    fn meta_len() -> usize {
        (8 << 20) * (page_size() / 4096).max(1)
    }

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
            Env::with_lg_page(page_size().trailing_zeros())
        }

        /// An environment whose layout believes the page is `1 << lg_page`
        /// bytes. On Linux that must be a multiple of the real page (the
        /// commits and purges are real syscalls over a real reservation);
        /// larger than the real page it stands in for a 16 KiB or 64 KiB
        /// page kernel.
        #[cfg(target_os = "linux")]
        fn with_lg_page(lg_page: u32) -> Env {
            // The handoff is an opt-in (design 13.3); the class tests
            // exercise it, so every test environment turns it on.
            set_group_handoff(true);
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
            let mb_len = meta_len() + page;
            let mb_raw = reserve(mb_len);
            let mb = align_up(mb_raw, page);
            let superblock = align_up(sb, MAX_SLAB);
            let regions = regions_at(superblock, (mb, meta_len()), lg_page);
            Env { meta: Meta::new(mb, meta_len(), page), regions, maps: [(sb, sb_len), (mb_raw, mb_len)] }
        }

        #[cfg(not(target_os = "linux"))]
        fn with_lg_page(lg_page: u32) -> Env {
            // The handoff is an opt-in (design 13.3); the class tests
            // exercise it, so every test environment turns it on.
            set_group_handoff(true);
            let page = 1usize << lg_page;
            let buf = vec![0u8; meta_len() + page];
            let mb = align_up(buf.as_ptr() as usize, page);
            let regions = regions_at(0x1000_0000_0000, (mb, meta_len()), lg_page);
            Env { meta: Meta::new(mb, meta_len(), page), regions, _buf: buf }
        }

        /// A fresh table for `c` (leaked: the state points at it) and the
        /// arena-0 state over it, leaked and registered with the table as
        /// `Arena::init` does, so a handoff can find it as a donor.
        fn class(&self, c: usize, guard_interval: u32) -> &'static ClassState {
            let table = self.table(c, guard_interval);
            let cs = std::boxed::Box::leak(std::boxed::Box::new(
                ClassState::new(&self.meta, &self.regions, table, 0, self.sets()).expect("class state"),
            ));
            table.register_state(cs);
            cs
        }

        /// A fresh set table for one arena (leaked: the states point at it).
        fn sets(&self) -> &'static SetTable {
            std::boxed::Box::leak(std::boxed::Box::new(SetTable::new(&self.meta, &self.regions).expect("set table")))
        }

        fn table(&self, c: usize, guard_interval: u32) -> &'static SlabTable {
            std::boxed::Box::leak(std::boxed::Box::new(
                SlabTable::new(&self.meta, &self.regions, c, guard_interval).expect("slab table"),
            ))
        }

        /// The state of `arena` over the same table as `cs`, leaked and
        /// registered like [`Env::class`].
        fn class_in_arena(&self, cs: &ClassState, arena: u16) -> &'static ClassState {
            let table = cs.table();
            let new = std::boxed::Box::leak(std::boxed::Box::new(
                ClassState::new(&self.meta, &self.regions, table, arena, self.sets()).expect("class state"),
            ));
            table.register_state(new);
            new
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
        let mut set = Set::new(&env.meta, page_size(), nbits).expect("set");
        assert_eq!(set.map.lowest_set(), None);
        assert_eq!(set.map.lowest_set_from(0), None);
        assert_eq!(set.map.lowest_set_from(12345), None);
        assert_eq!(set.map.lowest_set_from(nbits - 1), None);
        set.ensure(&env.meta, page_size(), 100).unwrap();
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
        if (1usize << (LG_MIN_SLAB + 2)) < page_size() {
            // The faked page must be a multiple of the real one; on a
            // 64 KiB kernel the real page already is the larger-page case.
            std::eprintln!("skipped: the real page ({}) is larger than the faked one", page_size());
            return;
        }
        let env = Env::with_lg_page(LG_MIN_SLAB + 2);
        let c = min_slab_class();
        let n = CLASSES[c].nslots();
        assert_eq!(CLASSES[c].slab_size(), MIN_SLAB);
        // Every fourth page group is a guard group: slabs 12..16.
        let cs = env.class(c, 4);
        assert_eq!(cs.table().slabs_per_page(), 4);
        assert_eq!(cs.table().guard_interval(), 4);
        for s in 0..20 {
            assert_eq!(cs.is_guard_slab(s), (12..16).contains(&s), "slab {s}");
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
        // 128..132 is page group 32, not a guard (124..128 is), so its
        // first usable slab is 128. Arena 0 then reuses its Empty slab 0
        // and commits 5 and 6 (a refill takes Empty slabs before Purged
        // ones and Purged ones before committing).
        let other = env.class_in_arena(&cs, 1);
        let (q, _) = env.claim(&other, n);
        assert_eq!(cs.table().group(), 128);
        assert!(cs.is_guard_slab(124) && !cs.is_guard_slab(128));
        assert_eq!(q[0] as usize, env.regions.slab_addr(c, 128));
        assert_eq!(cs.table().owner(128), 1);
        assert_eq!(cs.table().owner(5), 0);
        assert_eq!(cs.table().committed_slabs(), 256);
        assert_eq!(cs.table().guards(), 0);
        let (r, _) = env.claim(&cs, 3 * n);
        assert_eq!(r.len(), 3 * n);
        for s in [0, 5, 6] {
            assert_eq!(in_slab(&r, s).len(), n, "slab {s}");
            assert_eq!(cs.desc(s).state(), State::Active, "slab {s}");
            assert_eq!(cs.desc(s).arena(), 0);
        }
        assert_eq!(cs.desc(128).arena(), 1);
        // Slab 7 was committed with its page group (by slab 4) and handed
        // to arena 0 with the group, but no refill has used it: Fresh.
        assert_eq!(cs.desc(7).state(), State::Fresh);
        assert_eq!(cs.stats(), ClassStats { committed: 7, ..Default::default() });
        // Page group 4..8 with 4, 5, 6 Empty and 7 Fresh is idle, so it is
        // discarded as a whole; 7 stays Fresh (nothing was written to it).
        cs.release(&p[4 * n..]);
        cs.release(&in_slab(&r, 5));
        cs.release(&in_slab(&r, 6));
        assert_eq!(cs.stats(), ClassStats { committed: 7, empty: 3, ..Default::default() });
        assert_eq!(other.purge_empty(u64::MAX), 0);
        assert_eq!(cs.purge_empty(u64::MAX), 3);
        for s in 4..7 {
            assert_eq!(cs.desc(s).state(), State::Purged, "slab {s}");
        }
        assert_eq!(cs.desc(7).state(), State::Fresh);
        assert_eq!(cs.stats(), ClassStats { committed: 7, purged: 3, ..Default::default() });
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
        assert_eq!(cs.stats(), ClassStats { committed: 7, purged: 7, ..Default::default() });
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
        assert_eq!(cs.stats(), ClassStats { committed: 7, purged: 7, ..Default::default() });
        // Past the guard group: the Purged slabs go first, then slabs 7
        // and 8..12 are committed, then the next commit skips the guard
        // group 12..16 and lands on slab 16.
        let (g, _) = env.claim(&cs, 12 * n + 1);
        assert_eq!(g.len(), 12 * n + 1);
        assert_eq!(in_slab(&g, 7).len(), n);
        assert_eq!(in_slab(&g, 11).len(), n);
        assert_eq!(in_slab(&g, 16).len(), 1);
        assert_eq!(cs.table().guards(), 4);
        assert_eq!(cs.stats().committed, 13);
        assert_eq!(cs.table().committed_slabs(), 256);
        for s in 12..16 {
            assert!(cs.is_guard_slab(s));
            assert_eq!(cs.desc(s).state(), State::Fresh);
            assert_eq!(cs.desc(s).live(), 0);
        }
        // The other arena's group is untouched by all of this.
        assert_eq!(other.desc(128).state(), State::Active);
        assert_eq!(other.desc(128).live(), n);
        assert_eq!(other.stats(), ClassStats { committed: 1, guards: 4, ..Default::default() });
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

    /// The decay driver's view (design section 8): nothing is purged in
    /// the first step of the window, about half of a cohort by mid-window
    /// (the highest addresses, so a claim still finds the lowest), all of
    /// it at the end; the walk runs once per step; older cohorts go first;
    /// a lone slab lasts until its weight is zero; a stale (too early)
    /// `oldest_empty` corrects itself; a zero period purges at once.
    #[test]
    fn decay_purges_the_oldest_empty_slabs_along_the_curve() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let nslots = CLASSES[c].nslots();
        let n = cs.nslabs();
        assert!(n >= 64, "test layout holds {n} slabs");
        assert_eq!(cs.oldest_empty(), 0);
        assert_eq!(cs.decay(u64::MAX, 1000), None);
        // A window of 200_000 ns: steps of 1000.
        const W: u64 = 200_000;
        // One cohort of every slab; the clock reads are made uniform.
        let (p, _) = env.claim(&cs, nslots * n);
        cs.release(&p);
        assert_eq!(cs.stats().empty, n);
        let t0 = cs.oldest_empty();
        assert!(t0 > 0);
        for s in 0..n {
            cs.desc(s).set_dirty_at(t0);
        }
        // Before the first step ends: no walk, the deadline is its end.
        assert_eq!(cs.decay(t0 + 999, W), Some(t0 + 1000));
        assert_eq!(cs.stats().empty, n);
        // At it: a walk that purges nothing (weight one), next one a step on.
        assert_eq!(cs.decay(t0 + 1000, W), Some(t0 + 2000));
        assert_eq!(cs.stats().empty, n);
        // Within the step the deadline holds and nothing is walked.
        assert_eq!(cs.decay(t0 + 1500, W), Some(t0 + 2000));
        // Mid-window: about half, the highest slabs first.
        assert_eq!(cs.decay(t0 + W / 2, W), Some(t0 + W / 2 + 1000));
        let st = cs.stats();
        assert_eq!(st.empty + st.purged, n);
        assert!(st.purged.abs_diff(n / 2) <= n / 10 + 1, "{st:?}");
        for s in 0..n {
            let want = if s < st.empty { State::Empty } else { State::Purged };
            assert_eq!(cs.desc(s).state(), want, "slab {s}");
        }
        assert_eq!(cs.oldest_empty(), t0);
        // A quarter of the way in nothing more would have gone; three
        // quarters in most of it; at the end all of it.
        assert_eq!(cs.decay(t0 + 3 * W / 4, W), Some(t0 + 3 * W / 4 + 1000));
        let st2 = cs.stats();
        assert!(st2.purged > st.purged && st2.empty > 0, "{st2:?}");
        assert!(st2.empty * 100 / n <= 15, "{st2:?}");
        assert_eq!(cs.decay(t0 + W, W), None);
        assert_eq!(cs.stats(), ClassStats { committed: n, purged: n, ..Default::default() });
        assert_eq!(cs.oldest_empty(), 0);
        // Two cohorts: slab 0 at t, slab 1 a half window younger. At the
        // last step of slab 0 only it goes; slab 1, alone, lasts until its
        // own last step.
        let (q, _) = env.claim(&cs, nslots * 2);
        cs.release(&q);
        let t = cs.oldest_empty();
        cs.desc(0).set_dirty_at(t);
        cs.desc(1).set_dirty_at(t + W / 2);
        assert_eq!(cs.decay(t + W - 1000, W), Some(t + W));
        assert_eq!(cs.desc(0).state(), State::Purged);
        assert_eq!(cs.desc(1).state(), State::Empty);
        assert_eq!(cs.oldest_empty(), t + W / 2);
        assert_eq!(cs.decay(t + W / 2 + W - 2000, W), Some(t + W / 2 + W - 1000));
        assert_eq!(cs.desc(1).state(), State::Empty);
        assert_eq!(cs.decay(t + W / 2 + W - 1000, W), None);
        assert_eq!(cs.desc(1).state(), State::Purged);
        // A claim reuses slab 0 (the lowest Empty): the atomic is now stale
        // (too early). The next decay finds nothing and clears it.
        let (r, _) = env.claim(&cs, nslots);
        cs.release(&r[..1]);
        assert_eq!(cs.desc(0).state(), State::Active);
        cs.release(&r[1..]);
        assert_eq!(cs.desc(0).state(), State::Empty);
        let t = cs.oldest_empty();
        assert!(t > 0);
        let (r, _) = env.claim(&cs, 1);
        assert_eq!(r[0], q[0]);
        assert_eq!(cs.oldest_empty(), t);
        assert_eq!(cs.decay(u64::MAX / 2, W), None);
        assert_eq!(cs.oldest_empty(), 0);
        // A zero period purges as soon as a slab is Empty, whatever the
        // deadline of the last walk.
        cs.release(&r);
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

    /// A deadline left by a walk under a long period does not delay a
    /// period shortened since (`arena.<i>.dirty_decay_ms`).
    #[test]
    fn decay_walks_at_once_after_the_period_shrinks() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let nslots = CLASSES[c].nslots();
        let (p, _) = env.claim(&cs, nslots * 4);
        cs.release(&p);
        let t0 = cs.oldest_empty();
        for s in 0..4 {
            cs.desc(s).set_dirty_at(t0);
        }
        // A walk under a 20 s period leaves a deadline 100 ms out.
        let long = 20_000_000_000;
        assert_eq!(cs.decay(t0 + long / 200, long), Some(t0 + 2 * (long / 200)));
        assert_eq!(cs.stats().empty, 4);
        // Under a 200 us period the same instant is past the window.
        assert_eq!(cs.decay(t0 + long / 200, 200_000), None);
        assert_eq!(cs.stats().purged, 4);
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
            let buf = vec![0u8; 3 * page_size()];
            let mb = align_up(buf.as_ptr() as usize, page_size());
            (Meta::new(mb, 2 * page_size(), page_size()), buf)
        };
        #[cfg(target_os = "linux")]
        let meta = {
            use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
            let mb = unsafe {
                crate::sys::mmap(0, 2 * page_size(), PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0)
            }
            .unwrap();
            Meta::new(mb, 2 * page_size(), page_size())
        };
        let regions = regions_at(0x1000_0000_0000, (0x2000_0000_0000, 2 * page_size()), page_size().trailing_zeros());
        assert!(SlabTable::new(&meta, &regions, 0, 0).is_none());
    }

    /// Two arenas over one class region: slabs are handed out in address
    /// order to whichever arena commits next, each records its owner, and
    /// each arena's sets and counts see only its own slabs.
    /// The point of the interleaved set table: however many classes an
    /// arena uses, the words their sets touch for low slab numbers share
    /// the first page of their slab-size group (rows 0 and 1), where one
    /// table per set touched a page or two per (arena, class). Creating
    /// the table commits nothing.
    #[test]
    fn set_table_of_many_classes_touches_one_page_per_slab_size() {
        let env = Env::new();
        let before = env.meta.stats().committed;
        let sets = env.sets();
        assert_eq!(env.meta.stats().committed, before, "reservation only");
        let page = env.regions.page_size();
        let classes: Vec<usize> = (0..NCLASSES).step_by(5).collect();
        let states: Vec<ClassState> = classes
            .iter()
            .map(|&c| ClassState::new(&env.meta, &env.regions, env.table(c, 0), 0, sets).expect("class state"))
            .collect();
        for cs in &states {
            let (p, _) = env.claim(cs, 3);
            cs.release(&p[..1]);
            let (q, _) = env.claim(cs, 1);
            cs.release(&q);
            cs.release(&p[1..]);
        }
        // Every state has slabs in its partial or empty set now.
        assert!(states.iter().all(|cs| { let s = cs.stats(); s.nonfull + s.empty > 0 }));
        let mut groups_used: Vec<usize> = classes.iter().map(|&c| SetTable::group_of(c)).collect();
        groups_used.sort_unstable();
        groups_used.dedup();
        let mut pages: Vec<usize> = Vec::new();
        for (base, committed) in sets.committed_ranges() {
            if committed == 0 {
                continue;
            }
            // SAFETY: the committed prefix of a table this test owns.
            let words = unsafe { core::slice::from_raw_parts(base as *const u64, committed / 8) };
            pages.extend(words.iter().enumerate().filter(|(_, w)| **w != 0).map(|(i, _)| (base + 8 * i) / page));
        }
        pages.sort_unstable();
        pages.dedup();
        assert!(!pages.is_empty());
        assert!(
            pages.len() <= groups_used.len(),
            "{} classes in {} slab-size groups touched {} pages of the set tables",
            classes.len(),
            groups_used.len(),
            pages.len()
        );
    }

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

    /// What a handoff offers (plan Phase 1.5 task 15): a group is idle
    /// once its last `Active` slab goes `Empty`, not while a slot is out,
    /// and stays idle through a purge.
    #[test]
    fn a_group_is_idle_between_its_last_empty_and_its_next_take() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        assert_eq!(cs.table().group(), 1, "the 64-byte class hands out one slab per group");
        assert!(cs.idle_groups().is_empty());
        let (p, _) = env.claim(cs, 3);
        assert!(cs.idle_groups().is_empty());
        cs.release(&p[..2]);
        assert!(cs.idle_groups().is_empty(), "a slab with a live slot is not idle");
        cs.release(&p[2..]);
        assert_eq!(cs.desc(0).state(), State::Empty);
        assert_eq!(cs.idle_groups(), vec![0]);
        assert_eq!(cs.purge_empty(u64::MAX), 1);
        assert_eq!(cs.idle_groups(), vec![0], "Empty to Purged keeps the group idle");
        let (q, claim) = env.claim(cs, 1);
        assert!(claim.zeroed);
        assert!(cs.idle_groups().is_empty(), "a take withdraws the group");
        cs.release(&q);
        assert_eq!(cs.idle_groups(), vec![0]);
    }

    /// The handoff: an arena whose group is used up takes an idle group of
    /// another arena instead of a fresh one. Owner word, descriptors, sets,
    /// slab counts and the pointers' routing all move; nothing new is
    /// committed. `donor` and `taker` in both index orders.
    fn handoff_between(donor_ix: u16, taker_ix: u16) {
        let env = Env::new();
        let c = small_class();
        let cs0 = env.class(c, 0);
        let (donor, taker) = if donor_ix == 0 {
            (cs0, env.class_in_arena(cs0, taker_ix))
        } else {
            (env.class_in_arena(cs0, donor_ix), cs0)
        };
        let t = cs0.table();
        let nslots = donor.class().nslots();
        // The donor fills slab 0 (its group) and lets it all go: Empty, idle.
        let (p, _) = env.claim(donor, nslots);
        assert_eq!(t.committed_slabs(), 1);
        assert_eq!(t.owner(0), donor_ix);
        donor.release(&p);
        assert_eq!(donor.desc(0).state(), State::Empty);
        assert_eq!(donor.stats(), ClassStats { committed: 1, guards: 0, empty: 1, purged: 0, nonfull: 0 });
        assert_eq!(donor.idle_groups(), vec![0]);
        let dirty_at = donor.desc(0).dirty_at();
        assert!(dirty_at != 0);
        // The taker's first claim needs a slab: it takes slab 0 over.
        let (q, claim) = env.claim(taker, 5);
        assert_eq!(claim.n, 5);
        assert!(!claim.zeroed, "an Empty slab is dirty");
        assert_eq!(t.committed_slabs(), 1, "no fresh group was taken");
        assert_eq!(taker.locate(q[0]).0, 0);
        assert_eq!(t.owner(0), taker_ix);
        assert_eq!(taker.desc(0).arena(), taker_ix);
        assert_eq!(taker.desc(0).state(), State::Active);
        assert_eq!(taker.desc(0).dirty_at(), 0);
        assert!(taker.is_partial(0) && !donor.is_partial(0));
        assert_eq!(donor.stats(), ClassStats::default());
        assert_eq!(taker.stats(), ClassStats { committed: 1, guards: 0, empty: 0, purged: 0, nonfull: 1 });
        assert!(taker.idle_groups().is_empty() && donor.idle_groups().is_empty());
        // Routing follows the group: the taker releases, the donor may not.
        taker.check(q[0]);
        taker.release(&q[..1]);
        // The donor's next claim takes a fresh group; the taker's slab is
        // not idle (4 live slots).
        let (r, _) = env.claim(donor, 1);
        assert_eq!(donor.locate(r[0]).0, 1);
        assert_eq!(t.committed_slabs(), 2);
        assert_eq!(t.owner(1), donor_ix);
        // The taker empties slab 0 and its decay walk purges it: it owns the
        // slab's decay now.
        taker.release(&q[1..]);
        assert_eq!(taker.desc(0).state(), State::Empty);
        assert_eq!(taker.stats().empty, 1);
        assert_eq!(taker.purge_empty(u64::MAX), 1);
        assert_eq!(taker.stats().purged, 1);
        assert_eq!(donor.stats().purged, 0);
        // And the donor can take it back once its own group is used up.
        donor.release(&r);
        assert_eq!(taker.idle_groups(), vec![0]);
        assert_eq!(donor.idle_groups(), vec![1]);
        let (x, _) = env.claim(donor, nslots + 1);
        assert_eq!(t.committed_slabs(), 2, "slab 0 came back before anything fresh");
        assert_eq!(t.owner(0), donor_ix);
        assert_eq!(donor.desc(0).arena(), donor_ix);
        assert_eq!(taker.stats(), ClassStats::default());
        assert_eq!(donor.stats(), ClassStats { committed: 2, guards: 0, empty: 0, purged: 0, nonfull: 1 });
        assert!(x.iter().any(|&p| donor.locate(p).0 == 0) && x.iter().any(|&p| donor.locate(p).0 == 1));
    }

    #[test]
    fn an_idle_group_is_handed_up_to_a_higher_arena() {
        handoff_between(0, 1);
    }

    #[test]
    fn an_idle_group_is_handed_down_to_a_lower_arena() {
        handoff_between(3, 0);
    }

    /// A donor's `Purged` slab moves too and is still reported zero.
    #[test]
    fn a_purged_group_is_handed_over_and_stays_zero() {
        let env = Env::new();
        let c = small_class();
        let donor = env.class(c, 0);
        let taker = env.class_in_arena(donor, 1);
        let (p, _) = env.claim(donor, 2);
        donor.release(&p);
        assert_eq!(donor.purge_empty(u64::MAX), 1);
        assert_eq!(donor.stats().purged, 1);
        let (q, claim) = env.claim(taker, 1);
        assert!(claim.zeroed);
        assert_eq!(taker.locate(q[0]).0, 0);
        assert_eq!(donor.stats(), ClassStats::default());
        assert_eq!(taker.stats().committed, 1);
        assert_eq!(taker.stats().purged, 0);
        assert_eq!(donor.table().committed_slabs(), 1);
    }

    /// A group with an `Active` slab, or a slab with a slot still out (in
    /// a cache, say), is never taken: the taker commits a fresh group.
    #[test]
    fn a_group_in_use_is_never_handed_over() {
        let env = Env::new();
        let c = small_class();
        let donor = env.class(c, 0);
        let taker = env.class_in_arena(donor, 1);
        let t = donor.table();
        // One slot out of three: live > 0.
        let (p, _) = env.claim(donor, 3);
        donor.release(&p[..2]);
        assert_eq!(donor.desc(0).state(), State::Active);
        assert_eq!(donor.desc(0).live(), 1);
        assert!(donor.idle_groups().is_empty());
        let (q, _) = env.claim(taker, 1);
        assert_eq!(taker.locate(q[0]).0, 1);
        assert_eq!(t.committed_slabs(), 2);
        assert_eq!(t.owner(0), 0);
        assert_eq!(donor.desc(0).arena(), 0);
        assert_eq!(donor.stats().committed, 1);
        // The last slot comes back: now it moves, on the taker's next need.
        donor.release(&p[2..]);
        assert_eq!(donor.idle_groups(), vec![0]);
        let (r, _) = env.claim(taker, taker.class().nslots());
        assert_eq!(t.committed_slabs(), 2);
        assert!(r.iter().any(|&x| taker.locate(x).0 == 0));
        assert_eq!(t.owner(0), 1);
    }

    /// A multi-slab group (the 4 KiB-slab class: 128 slabs per bitmap
    /// line) moves whole, uncommitted slabs included: the taker inherits
    /// the donor's commit cursor and commits the rest itself, and the
    /// donor's next slab comes from a new group. A group with one
    /// `Active` slab among many `Empty` ones stays where it is.
    #[test]
    fn a_multi_slab_group_moves_whole_with_its_cursor() {
        let env = Env::new();
        let c = min_slab_class();
        let donor = env.class(c, 0);
        let taker = env.class_in_arena(donor, 1);
        let t = donor.table();
        let group = t.group();
        assert!(group >= 4, "group of {group}");
        let nslots = donor.class().nslots();
        // Two slabs committed, both emptied.
        let (p, _) = env.claim(donor, 2 * nslots);
        assert_eq!(t.committed_slabs(), group);
        assert_eq!(donor.stats().committed, 2);
        // One slab Active among Empty ones vetoes the group.
        donor.release(&p[nslots..]);
        assert_eq!(donor.stats().empty, 1);
        assert!(donor.idle_groups().is_empty());
        donor.release(&p[..nslots]);
        assert_eq!(donor.stats().empty, 2);
        assert_eq!(donor.idle_groups(), vec![0]);
        // The taker takes the group: 2 Empty slabs and the cursor at 2.
        let (q, _) = env.claim(taker, 1);
        assert_eq!(taker.locate(q[0]).0, 0);
        assert_eq!(t.committed_slabs(), group);
        assert_eq!(t.owner(0), 1);
        for s in 0..group {
            if !t.is_guard_slab(s) {
                assert_eq!(taker.desc(s).arena(), 1, "slab {s}");
            }
        }
        assert_eq!(donor.stats(), ClassStats::default());
        assert_eq!(taker.stats(), ClassStats { committed: 2, guards: 0, empty: 1, purged: 0, nonfull: 1 });
        // The taker's further claims use slab 1 (Empty), then commit slab
        // 2 of the same group: still no fresh group.
        let (r, _) = env.claim(taker, 2 * nslots);
        assert_eq!(taker.locate(r[r.len() - 1]).0, 2);
        assert_eq!(t.committed_slabs(), group);
        assert_eq!(taker.stats().committed, 3);
        // The donor's cursor was surrendered: its next slab is a new group.
        let (x, _) = env.claim(donor, 1);
        assert_eq!(donor.locate(x[0]).0, group);
        assert_eq!(t.committed_slabs(), 2 * group);
        assert_eq!(t.owner(group), 0);
        assert_eq!(donor.stats().committed, 1);
    }

    /// Four arenas, two threads each, claiming, releasing and purging at
    /// random over one table, so groups change hands under contention in
    /// both lock directions. Every purge walk asserts that the empty set
    /// and the descriptors agree; at the end every slab's descriptor must
    /// match the owner word and sit in exactly the right set of exactly
    /// its owner.
    #[test]
    fn concurrent_handoffs_keep_sets_and_descriptors_consistent() {
        let env = Arc::new(Env::new());
        let c = small_class();
        let cs0 = env.class(c, 0);
        let states: Vec<&'static ClassState> = (0..4u16).map(|a| if a == 0 { cs0 } else { env.class_in_arena(cs0, a) }).collect();
        let threads = 8;
        let rounds = 400;
        let mut handles = Vec::new();
        for t in 0..threads {
            let env = Arc::clone(&env);
            let cs = states[t % 4];
            handles.push(std::thread::spawn(move || {
                let mut rng = 0x2545_F491_4F6C_DD1Du64 ^ (t as u64 + 1);
                let mut held: Vec<*mut u8> = Vec::new();
                for r in 0..rounds {
                    let k = xorshift(&mut rng) as usize % 300 + 1;
                    let (p, claim) = env.claim(cs, k);
                    assert_eq!(claim.n, k);
                    for &q in &p {
                        cs.check(q);
                    }
                    held.extend(p);
                    match xorshift(&mut rng) % 8 {
                        0..=4 => {
                            let keep = xorshift(&mut rng) as usize % (held.len() + 1);
                            let mut batch = held.split_off(keep);
                            batch.sort();
                            cs.release(&batch);
                        }
                        5 => {
                            let mut batch = core::mem::take(&mut held);
                            batch.sort();
                            cs.release(&batch);
                        }
                        6 => {
                            cs.purge_empty(u64::MAX);
                        }
                        _ => {}
                    }
                    if r % 50 == 0 {
                        cs.decay(os::now_ns(), 1);
                    }
                }
                held.sort();
                cs.release(&held);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let t = cs0.table();
        assert!(HANDOFFS.load(Ordering::Relaxed) > 0, "no group changed hands");
        let mut committed = 0;
        for a in &states {
            a.purge_empty(u64::MAX);
            committed += a.stats().committed;
        }
        assert_eq!(committed + t.guards(), t.committed_slabs());
        for slab in 0..t.committed_slabs() {
            if t.is_guard_slab(slab) {
                continue;
            }
            let d = cs0.desc(slab);
            let owner = t.owner(slab);
            assert_eq!(d.arena(), owner, "slab {slab}");
            assert_eq!(d.live(), 0, "slab {slab}");
            for a in &states {
                let (partial, empty, purged) = a.set_membership(slab);
                assert!(!partial && !empty, "slab {slab} arena {}", a.arena());
                assert_eq!(purged, a.arena() == owner && d.state() == State::Purged, "slab {slab} arena {}", a.arena());
            }
        }
    }

    /// `take_group` cross-checks the group's owner word against the
    /// descriptors' arena field before handing the group out: a wild write
    /// into the owner array aborts the next hand-out instead of routing
    /// that group's frees through a wrong arena.
    #[test]
    #[should_panic(expected = "owner word says arena 7, slab")]
    fn a_corrupt_owner_word_aborts_the_next_take_group() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let (_p, _) = env.claim(&cs, 1);
        assert_eq!(cs.table().committed_slabs(), cs.table().group());
        cs.table().corrupt_owner_word(&env.meta, 1, 7);
        // The second arena's first claim needs a fresh group: the second.
        let cs1 = env.class_in_arena(&cs, 1);
        let _ = env.claim(&cs1, 1);
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

    /// Plan Phase 2.5 task 1b: a release batch is grouped by slab, not
    /// sorted. Slabs in any order, and a slab's slots in any order, clear
    /// exactly their bits with the right live counts.
    #[test]
    fn release_takes_slab_groups_in_any_order() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let n = CLASSES[c].nslots();
        let (p, _) = env.claim(&cs, 2 * n + 7);
        assert_eq!(p.len(), 2 * n + 7);
        // Slab 1 reversed, then slab 0's odd slots scrambled, then slab 2.
        let mut batch: Vec<*mut u8> = p[n..2 * n].iter().rev().copied().collect();
        let mut odd: Vec<*mut u8> = (1..n).step_by(2).map(|i| p[i]).collect();
        let mut st = 0x9e37_79b9_7f4a_7c15u64;
        for i in (1..odd.len()).rev() {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            let j = (st % (i as u64 + 1)) as usize;
            odd.swap(i, j);
        }
        batch.extend_from_slice(&odd);
        batch.extend_from_slice(&p[2 * n..]);
        cs.release(&batch);
        for (i, &q) in p.iter().enumerate() {
            let (slab, slot) = cs.locate(q);
            let released = i >= n || i % 2 == 1;
            assert_eq!(cs.table().bits(slab).is_set(slot), !released, "slot {i}");
        }
        assert_eq!(cs.desc(0).live(), n - odd.len());
        assert_eq!(cs.desc(1).live(), 0);
        assert_eq!(cs.desc(2).live(), 0);
    }

    /// A duplicate inside one slab's group aborts even when the two copies
    /// are not adjacent and not in the same bitmap word: the group's bits
    /// accumulate in one scratch bitmap.
    #[test]
    #[should_panic(expected = "double free of")]
    fn duplicate_apart_in_one_slab_group_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 80);
        cs.release(&[p[70], p[1], p[5], p[70]]);
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

    /// Plan Phase 1.5 task 9b: a slot start in a guard slab is named as
    /// such by the exact check (the sized fast path rejects it
    /// arithmetically and lands here).
    #[test]
    #[should_panic(expected = "guard slab")]
    fn check_of_a_guard_slab_slot_aborts() {
        let env = Env::new();
        let c = small_class();
        let n = CLASSES[c].nslots();
        // Every second page group is a guard group.
        let cs = env.class(c, 2);
        env.claim(&cs, 2 * n + 1);
        let guard = (0..cs.table().committed_slabs()).find(|&s| cs.is_guard_slab(s)).expect("a guard slab");
        assert!(guard < cs.table().committed_slabs());
        cs.check(env.regions.slab_addr(c, guard) as *mut u8);
    }

    /// Plan Phase 1.5 task 9b: a release into a slab of the arena's group
    /// that the arena has not committed (`Fresh`, below the hand-out
    /// watermark) is where a sized free of such a slot lands at the flush.
    #[test]
    #[should_panic(expected = "the slab has never been allocated from")]
    fn release_into_an_uncommitted_slab_of_the_group_aborts() {
        let env = Env::new();
        // The smallest slabs have several descriptors per line, so a group
        // is several slabs.
        let c = min_slab_class();
        let cs = env.class(c, 0);
        env.claim(&cs, 1);
        // The whole group was handed out; only slab 0 is committed.
        assert!(cs.table().committed_slabs() > 1, "group of {}", cs.table().committed_slabs());
        assert_eq!(cs.desc(1).state(), State::Fresh);
        assert_eq!(cs.stats().committed, 1);
        cs.release(&[env.regions.slab_addr(c, 1) as *mut u8]);
    }

    /// The mask form of `is_guard_slab` agrees with the definition (every
    /// `interval`-th page group, counted from 1) for every page-group ratio
    /// and power-of-two interval the tables use, and with guards off no slab
    /// of a region is a guard.
    #[test]
    fn guard_slab_test_is_the_page_group_modulo() {
        for lg in [0u32, 2] {
            if lg != 0 && (1usize << (LG_MIN_SLAB + lg)) < page_size() {
                continue; // a faked page smaller than the real one cannot be reserved
            }
            let env = if lg == 0 { Env::new() } else { Env::with_lg_page(LG_MIN_SLAB + lg) };
            let c = min_slab_class();
            for interval in [0u32, 2, 4, 8, 32, 1 << 22] {
                let t = env.table(c, interval);
                assert_eq!(t.guard_interval(), interval);
                let spp = t.slabs_per_page();
                for s in 0..256 {
                    let want = interval != 0 && (s / spp + 1) % interval as usize == 0;
                    assert_eq!(t.is_guard_slab(s), want, "lg {lg} interval {interval} slab {s}");
                }
                if interval == 0 {
                    assert!((0..t.nslabs()).step_by(4093).all(|s| !t.is_guard_slab(s)));
                    assert!(!t.is_guard_slab(t.nslabs() - 1));
                }
            }
        }
    }

    /// The interval must be a power of two (the sized path's guard test is
    /// a mask); `config.rs` rounds a configured value up before it gets here.
    #[test]
    #[should_panic(expected = "guard interval 3 is not a power of two")]
    fn guard_interval_must_be_a_power_of_two() {
        let env = Env::new();
        let _ = env.table(min_slab_class(), 3);
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
        // The purger publishes its count as it goes: on a small VM (four
        // vCPUs, four test threads) it can be off the CPU for the whole
        // 20 000-iteration window (Phase 2.5 task 3, the Debian 12 arm64
        // cell), so the loop runs past the window until the purger has
        // purged once, bounded by time. The assertion at the end is about
        // the protocol, not the scheduler.
        let purged = Arc::new(core::sync::atomic::AtomicUsize::new(0));
        let purger = {
            let cs = Arc::clone(&cs);
            let stop = Arc::clone(&stop);
            let purged = Arc::clone(&purged);
            std::thread::spawn(move || {
                let mut n = 0;
                while !stop.load(core::sync::atomic::Ordering::Relaxed) {
                    n += cs.purge_empty(u64::MAX);
                    purged.store(n, core::sync::atomic::Ordering::Relaxed);
                }
                n
            })
        };
        let mut rng = 1u64;
        let started = std::time::Instant::now();
        let mut i = 0u32;
        loop {
            i += 1;
            if i > 20_000
                && (purged.load(core::sync::atomic::Ordering::Relaxed) > 0
                    || started.elapsed() > std::time::Duration::from_secs(30))
            {
                break;
            }
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
        assert!(env.meta.stats().committed >= page_size());
    }

    /// `check_zero` (plan Phase 1.5 task 7b) on a healthy heap: every
    /// `Fresh` and `Purged` claim reads zero, so the mode changes nothing,
    /// and the flag stays on for every later claim of this test process.
    #[cfg(target_os = "linux")]
    #[test]
    fn check_zero_passes_fresh_and_purged_slabs() {
        purge::set_check_zero(true);
        assert!(purge::check_zero());
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, claim) = env.claim(&cs, 5);
        assert!(claim.zeroed);
        for &q in &p {
            unsafe { *q = 0xAB };
        }
        cs.release(&p);
        assert_eq!(cs.purge_empty(u64::MAX), 1);
        let (q, claim) = env.claim(&cs, 5);
        assert_eq!(q, p);
        assert!(claim.zeroed);
    }

    /// `check_zero` fires: a slab purged (so reported zero) whose memory
    /// is dirtied behind the allocator's back aborts at the next claim,
    /// naming the `Purged` state and the discard backend.
    #[cfg(target_os = "linux")]
    #[test]
    #[should_panic(expected = "was handed out as zero from a Purged slab but its word at offset")]
    fn check_zero_aborts_on_a_dirty_purged_slab() {
        purge::set_check_zero(true);
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 2);
        cs.release(&p);
        assert_eq!(cs.purge_empty(u64::MAX), 1);
        // The last word of the second slot: the check reads both ends.
        let size = cs.class().size();
        unsafe { ptr::write_volatile(p[1].add(size - 8) as *mut u64, 0xDEAD_BEEF) };
        let _ = env.claim(&cs, 2);
    }

    /// The calloc race (plan Phase 1.5 task 7c, the interleaving named in
    /// `bench/results/2026-09-18-calloc-zero-campaign.md`): thread A's
    /// `plan` selects a `Fresh` slab, flags its pick zero and drops the
    /// class lock; thread B refills from the same, now partial, slab, hands
    /// the slot to the program, which writes to it and frees it; then A's
    /// `finish` runs. Before the fix A's bit loop won the slot B had just
    /// released and reported it zero. Now A's slots are claimed in `take`,
    /// under the lock, so B gets the next slot and A's reads zero. The
    /// same for a `Purged` slab when the discard backend zeroes.
    #[test]
    fn a_zero_pick_never_receives_a_slot_another_claim_used_in_between() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let mut round = 0;
        loop {
            // Slab 0 is not handed out before the first `plan` (so no
            // `desc(0)` yet): `Fresh` then, `Purged` in round 2.
            let prior = if round == 0 { State::Fresh } else { State::Purged };
            // Thread A: one lock hold, one pick of one slot of slab 0.
            let mut picks = [Pick { slab: 0, n: 0, zero: false, state: State::Active }; MAX_PICKS];
            let np = cs.plan(&env.meta, 1, &mut picks);
            assert_eq!(np, 1);
            assert_eq!((picks[0].slab, picks[0].n, picks[0].state), (0, 1, prior));
            assert!(picks[0].zero, "round {round}: the {prior:?} slab must be a zero pick");
            assert_eq!(cs.desc(0).live(), 1);
            assert_eq!(cs.stats().nonfull, 1, "slab 0 is partial, so B's plan finds it");
            // Thread B: a whole refill of one slot from the same class,
            // the program writes to the slot and frees it.
            let (b, claim) = env.claim(&cs, 1);
            assert_eq!(claim, Claim { n: 1, zeroed: false });
            assert_eq!(cs.locate(b[0]).0, 0, "B refills from the slab A reserved in");
            #[cfg(target_os = "linux")]
            unsafe {
                ptr::write_volatile(b[0], 0xAB);
            }
            cs.release(&b);
            assert_eq!(cs.desc(0).live(), 1);
            // Thread A finishes its refill.
            let mut out = [ptr::null_mut(); 1];
            let mut zeroed = true;
            assert_eq!(cs.finish(&picks[..np], &mut out, &mut zeroed, None), 1);
            assert!(zeroed);
            assert_ne!(out[0], b[0], "round {round}: a slot reported zero was allocated, written and freed by another claim in between");
            assert_eq!(cs.locate(out[0]), (0, 0), "A's slot was decided under the lock: the lowest");
            assert_eq!(cs.locate(b[0]), (0, 1), "B saw A's bit and took the next slot");
            #[cfg(target_os = "linux")]
            assert_eq!(unsafe { ptr::read_volatile(out[0]) }, 0, "round {round}: A's slot reads zero");
            cs.release(&out);
            assert_eq!(cs.desc(0).live(), 0);
            assert_eq!(cs.stats().empty, 1);
            round += 1;
            if round == 2 {
                break;
            }
            // Round 2: the same window on the slab once purged.
            assert_eq!(cs.purge_empty(u64::MAX), 1);
            assert_eq!(cs.desc(0).state(), State::Purged);
            if !purge::purged_is_zero() {
                break;
            }
        }
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


    #[test]
    fn bin_stats_counts_every_curregs_clamp() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (_, curregs) = cs.bin_stats();
        assert_eq!((curregs, cs.clamped()), (0, 0));
        // No slab committed, so the capacity is zero: a `claimed` ahead of
        // `released` (the window `bin_stats` documents) is clamped to it and
        // the clamp is counted.
        cs.claimed.store(3, Ordering::Relaxed);
        let (_, curregs) = cs.bin_stats();
        assert_eq!((curregs, cs.clamped()), (0, 1));
        let (_, curregs) = cs.bin_stats();
        assert_eq!((curregs, cs.clamped()), (0, 2));
        // Caught up: no clamp, the count stays.
        cs.released.store(3, Ordering::Relaxed);
        let (_, curregs) = cs.bin_stats();
        assert_eq!((curregs, cs.clamped()), (0, 2));
    }


    #[test]
    #[should_panic(expected = "slab live count above its slot count")]
    fn a_plan_over_a_slab_with_live_above_nslots_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (b, claim) = env.claim(&cs, 1);
        assert_eq!(claim.n, 1);
        let slab = cs.lowest_partial().expect("the slab is partial");
        cs.release(&b[..1]);
        assert_eq!(cs.stats().empty, 1);
        // Corrupt the descriptor of the now `Empty` slab: `live` past the
        // slot count (the partial walk skips such a slab; the empty set
        // hands it straight to `take`).
        let nslots = cs.class().nslots();
        cs.desc(slab).add_live(nslots + 1);
        let mut picks = [Pick { slab: 0, n: 0, zero: false, state: State::Active }; MAX_PICKS];
        cs.plan(&env.meta, 1, &mut picks);
    }

    #[test]
    #[should_panic(expected = "empty slab count underflow")]
    fn a_plan_that_takes_an_empty_slab_the_counts_do_not_know_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (b, claim) = env.claim(&cs, 1);
        assert_eq!(claim.n, 1);
        cs.release(&b[..1]);
        assert_eq!(cs.stats().empty, 1);
        // Corrupt the count: the set still holds the slab.
        cs.inner.lock().stats.empty = 0;
        let mut picks = [Pick { slab: 0, n: 0, zero: false, state: State::Active }; MAX_PICKS];
        cs.plan(&env.meta, 1, &mut picks);
    }

    #[test]
    #[should_panic(expected = "empty slab count underflow")]
    fn a_purge_of_an_empty_slab_the_counts_do_not_know_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (b, claim) = env.claim(&cs, 1);
        assert_eq!(claim.n, 1);
        cs.release(&b[..1]);
        assert_eq!(cs.stats().empty, 1);
        cs.inner.lock().stats.empty = 0;
        cs.purge_empty(u64::MAX);
    }
}
