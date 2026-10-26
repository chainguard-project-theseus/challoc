//! Per-class state, slab selection, commit, and the claim/release protocol
//! (design.md sections 4 and 5).
//!
//! One [`SlabTable`] exists per class: the class region's descriptor array
//! and the commit watermark, shared by every arena, behind a commit lock.
//! One [`ClassState`] exists per `(arena, class)`. It holds three
//! [`TwoLevelBitmap`] views over slab numbers, into the arena's
//! [`SetTable`] (`Set::in_table`): `partial` (slabs believed to have
//! a claimable slot), `empty` (`live == 0`, dirty, not yet purged) and
//! `purged` (`live == 0`, pages discarded). All three are behind one futex
//! lock that is taken only on slab transitions: selecting a slab for a
//! refill, a slab filling up or emptying, a word going from full to
//! non-full, purge, commit (which nests the table's commit lock inside).
//! Claims and releases against a slab's bits happen outside the lock.
//!
//! The module is one directory, split by concern. `table` holds the
//! [`SlabTable`] (descriptors, class bitmap, owner array, the hand-out of
//! groups and the commit of slabs); `sets` the lazily committed set tables
//! ([`Lazy`](sets::Lazy), [`Set`], [`SetTable`]); this file the
//! [`ClassState`] and the protocol below (claim, plan, take, release,
//! reopen); `fresh` the fresh-slab path; `decay` the purge and the
//! decay walk; `verify` the
//! `check_zero` debug mode and the test-only pointer checks; `abort` the
//! `#[cold]` heap-inconsistency aborts; `tests` the tests.
//!
//! Slabs are handed to arenas in *groups* of consecutive slabs
//! (`SlabTable::group`: the slabs whose bits fill one 64-byte line of the
//! class bitmap, `Class::slabs_per_line`, or one page group if that is
//! larger), lowest group first to whichever arena runs out of slabs next.
//! A group's slabs are committed by their arena as it needs them, a run
//! up to the next guard slab at a time (`ClassState::next_fresh_slab`,
//! `SlabTable::commit_run`), so an arena owns whole bitmap lines and
//! whole descriptor lines: the `free` check of one arena's slot never
//! reads a line another arena's refills and flushes write. Every slab
//! belongs to the arena of its group (`SlabTable::owner`, a dense array
//! of one `u16` per group, and `SlabDesc::arena`), and only that arena's
//! state may claim from or release to it.
//!
//! A group stays with its arena for good: the owner word is written once,
//! when `take_group` hands the group out, so a slab's arena never changes.
//! `reopen` and `on_live_zero` still test the owner word under the lock
//! before they touch the arena's sets.
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

use crate::layout::{CLASSES, Class, MAX_SLAB_WORDS, Regions};
use crate::lock::{Lock, LockOrder, PAD_ALIGN, Padded, RawLock};
use crate::meta::Meta;
use crate::purge::{self, Decayer};
use crate::slab::{SlabDesc, State};
use crate::vspec::zero::ZeroRaw;
use core::sync::atomic::{AtomicU64, Ordering};
use vstd::prelude::{Ghost, Map, Tracked, int, nat};
use vstd::raw_ptr::PointsTo;

mod abort;
mod decay;
mod fresh;
mod sets;
mod table;
#[cfg(test)]
mod tests;
mod verify;

use abort::{
    double_free, guard_free, owner_mismatch, release_into_fresh_slab, reservation_unmet,
    wrong_arena, zero_slab_had_bits_set,
};
#[cfg(test)]
pub use sets::SET_GROUPS;
pub(crate) use sets::Set;
pub use sets::SetTable;
pub use table::SlabTable;
use verify::{fresh_with_live, release_into_fresh};

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
pub(crate) const MAX_PICKS: usize = 16;

/// The atomic steps of the protocol, named in the order the code takes
/// them, for the model (`model::steps`): each enum is one operation of
/// [`ClassState`], each variant one step that interleaves with other
/// threads' steps (a lock hold that only touches lock-protected state is
/// one step; a lock-free atomic is one step), with its discriminant the
/// model's sub-step number. The model matches every enum exhaustively,
/// so a step added here without a model step, or a reordering that
/// leaves the model's numbering behind, does not compile. Kept next to
/// the code it names: a change to the protocol is a change here first.
#[cfg(test)]
pub(crate) mod protocol {
    /// [`claim`](super::ClassState::claim): one round of `claim_with`'s
    /// loop (`plan`, then `finish`; short, plan again).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    #[repr(u8)]
    pub(crate) enum Refill {
        /// `plan`: `self.inner.lock()`.
        Lock = 0,
        /// `plan` under the lock, one step: the partial walk
        /// (`clear_partial` of a full slab), `next_slab` (the empty and
        /// purged sets, the group cursor with `commit_run`, `take_group`),
        /// and `take`
        /// per slab: the `live` reservation, the `Active` transition, the
        /// partial bit, `claim_prefix` for a zero pick.
        PlanLocked = 1,
        /// `plan` returns: the lock hold ends.
        Unlock = 2,
        /// `finish`, one pick at a time: a zero pick's slots `0..n` as
        /// claimed, else one `claim_from` (`fetch_or`) on the slab's word.
        Finish = 3,
    }

    /// [`release_owned`](super::ClassState::release_owned) of a batch
    /// grouped by slab, through `Heap::release_routed` (one run per owner
    /// arena), with `dec_live`, `on_live_zero` and `reopen`.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    #[repr(u8)]
    pub(crate) enum Release {
        /// The batch grouped by slab (`Heap::flush`'s bucket pass),
        /// before anything is touched.
        Group = 0,
        /// `release_word`: the owner word read, the descriptor's arena
        /// checked against it, the `Fresh` check, `bits(slab).release(word,
        /// mask)` (the `fetch_and`) and the double-free check on what it
        /// returned.
        FetchAnd = 1,
        /// `dec_live`: `sub_live(n)`, and whether it reached zero.
        SubLive = 2,
        /// `on_live_zero`: `self.inner.lock()`.
        LockLiveZero = 3,
        /// `on_live_zero` under the lock: the owner test, `Active` with
        /// `live == 0` to `Empty` and into the empty set (`empty_add`),
        /// unlock.
        OnLiveZero = 4,
        /// `release_owned` runs on to the next slab of the batch while it
        /// is the same owner's, else the run is done and `reopen` is
        /// pending for it.
        NextSlab = 5,
        /// `reopen`: `self.inner.lock()`.
        LockReopen = 6,
        /// `reopen` under the lock: per slab whose word was full, the
        /// `Active` and owner test and `set_partial`; unlock.
        Reopen = 7,
    }

    /// [`purge_empty`](super::ClassState::purge_empty).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    #[repr(u8)]
    pub(crate) enum Purge {
        /// `self.inner.lock()`.
        Lock = 0,
        /// Under the lock, one step: `walk_empty` (the state assertion),
        /// `push_group`, the discard (`batch.flush()`) and its `settle`
        /// (`Purged`, out of `empty`, into `purged`), unlock.
        PurgeLocked = 1,
    }

    impl Refill {
        /// The step at sub-step `sub` (`Finish` repeats, one pick at a time).
        pub(crate) fn at(sub: u8) -> Refill {
            match sub {
                0 => Refill::Lock,
                1 => Refill::PlanLocked,
                2 => Refill::Unlock,
                3 => Refill::Finish,
                _ => panic!("protocol: Refill has no step {sub}"),
            }
        }
    }

    impl Release {
        pub(crate) fn at(sub: u8) -> Release {
            match sub {
                0 => Release::Group,
                1 => Release::FetchAnd,
                2 => Release::SubLive,
                3 => Release::LockLiveZero,
                4 => Release::OnLiveZero,
                5 => Release::NextSlab,
                6 => Release::LockReopen,
                7 => Release::Reopen,
                _ => panic!("protocol: Release has no step {sub}"),
            }
        }
    }

    impl Purge {
        pub(crate) fn at(sub: u8) -> Purge {
            match sub {
                0 => Purge::Lock,
                1 => Purge::PurgeLocked,
                _ => panic!("protocol: Purge has no step {sub}"),
            }
        }
    }
}

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
    /// The slab was `Fresh` or `Purged` (and zero) when this refill made
    /// it `Active`, and bits `0..n` were claimed under the lock in `take`
    /// ([`ClassState::claim_prefix`]): `finish` hands out slots `0..n`
    /// without the bit loop.
    zero: bool,
    /// The slab's state when this refill selected it (for `check_zero`).
    state: State,
}

#[cfg(test)]
impl Pick {
    /// Tests: an unused entry of a plan's pick array (no slab, no slots).
    /// `claim_with` spells the same literal out: written as this const
    /// there, the array fill compiles to one instruction fewer
    /// (`scripts/codegen-diff.sh`), which is a code change, not a cleanup.
    const EMPTY: Pick = Pick {
        slab: 0,
        n: 0,
        zero: false,
        state: State::Active,
    };
}

/// Slabs a release batch must put back in the partial set. Consecutive
/// duplicates are dropped; a full list is flushed under the lock.
struct Reopen {
    slabs: [u32; MAX_PICKS],
    len: usize,
}

impl Reopen {
    #[inline(always)]
    fn new() -> Reopen {
        Reopen {
            slabs: [0; MAX_PICKS],
            len: 0,
        }
    }

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

struct Inner {
    partial: Set,
    empty: Set,
    purged: Set,
    stats: ClassStats,
    /// This arena's current slab group: `group_next..group_end` are handed
    /// to it and not yet handed out; the rest of the run that committed
    /// the slab before `group_next` is already read-write (see
    /// [`ClassState::next_fresh_slab`]).
    group_next: usize,
    group_end: usize,
    /// Ghost: this arena's slabs of this class, each with the state its
    /// descriptor holds while this lock is held.
    #[allow(dead_code)]
    states: Ghost<Map<nat, State>>,
    /// Ghost: this arena's committed slabs of this class (guards
    /// excluded), the slabs `stats.committed` counts.
    #[allow(dead_code)]
    committed: Ghost<vstd::set::Set<nat>>,
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
#[repr(C, align(128))]
pub struct ClassState {
    class: u16,
    arena: u16,
    /// The class's shared table, which lives as long as the allocator
    /// (inside `Shared`, or leaked by the tests).
    table: TablePtr,
    /// The decay step gate (`purge::Decayer`): the oldest `Empty` slab's
    /// `dirty_at`, written under the lock (set when the empty set goes
    /// from none to one, recomputed by every purge walk) and read without
    /// it by the decay driver, and the next walk's deadline.
    gate: Decayer,
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
    /// compaction gate in `heap/` watches its rate.
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
const _: () =
    assert!(core::mem::offset_of!(ClassState, gate) + core::mem::size_of::<Decayer>() <= PAD_ALIGN);
const _: () = assert!(core::mem::offset_of!(ClassState, claimed) % PAD_ALIGN == 0);
const _: () = assert!(core::mem::size_of::<ClassState>().is_multiple_of(PAD_ALIGN));
const _: () = assert!(!core::mem::size_of::<ClassState>().is_multiple_of(512));
const _: () = assert!(core::mem::size_of::<TablePtr>() == core::mem::size_of::<*const SlabTable>());

// SAFETY: the only non-`Sync`, non-`Send` field is the table pointer
// (`TablePtr`, whose other fields are ghost), which targets a `SlabTable`
// (itself `Sync`) that outlives every `ClassState` pointing at it: both
// live in `Shared` for the life of the process, or are leaked by the
// tests. Sharing the state across threads (`Sync`) or handing it to
// another thread (`Send`) therefore hands out nothing but a pointer to a
// process-lifetime, thread-safe table.
unsafe impl Sync for ClassState {}
unsafe impl Send for ClassState {}

/// The pointer to its table a [`ClassState`] keeps, in a module of its own
/// so that [`table_ptr`](tptr::table_ptr) is the only maker of one.
mod tptr {
    use super::SlabTable;
    use vstd::prelude::*;
    use vstd::raw_ptr::PointsTo;

    /// A pointer to the table of class `c`, with (ghost, no bytes: the
    /// layout is the pointer's) the frozen permissions to every class's
    /// table, from `base` on, whose element `c` is the pointer's.
    pub(super) struct TablePtr {
        p: *mut SlabTable,
        #[allow(dead_code)]
        tables: Tracked<&'static Map<int, PointsTo<SlabTable>>>,
        /// Element `c` of `tables`, held apart so that [`TablePtr::get`]
        /// passes it on as it is: a `Tracked(..)` built there is one more
        /// distinct `Tracked` type under `Heap::flush`, which puts it past
        /// the MIR inliner's limit of instances of one generic item and
        /// changes its machine code (x86_64).
        perm: Tracked<&'static PointsTo<SlabTable>>,
        #[allow(dead_code)]
        base: Ghost<*mut SlabTable>,
        #[allow(dead_code)]
        c: Ghost<int>,
    }

    /// The pointer `p` to element `c` of the tables `tables` covers, from
    /// `base` on, as a [`TablePtr`].
    #[inline(always)]
    pub(super) fn table_ptr(
        p: *mut SlabTable,
        tables: Tracked<&'static Map<int, PointsTo<SlabTable>>>,
        base: Ghost<*mut SlabTable>,
        c: Ghost<int>,
    ) -> TablePtr {
        TablePtr {
            p,
            tables,
            perm: Tracked::assume_new(),
            base,
            c,
        }
    }

    impl TablePtr {
        /// The table, through its frozen permission.
        #[inline(always)]
        pub(super) fn get(&self) -> &SlabTable {
            vstd::raw_ptr::ptr_ref(self.p, self.perm)
        }
    }
}

use tptr::{TablePtr, table_ptr};

/// The set/count seam: each of the three sets is one bitmap plus one
/// `ClassStats` count, and the two are written together, here or at the
/// two sites that spell the pair out and say why (`next_slab`,
/// `settle`), so `nonfull`, `empty` and `purged` are always the
/// population of their set. `partial` is idempotent (a release batch may
/// name a slab whose bit a refill set meanwhile); `empty` and `purged`
/// are exact: a slab enters when its state changes and leaves when a
/// refill takes it or a purge moves it, so a count that would go below
/// zero is a heap inconsistency and aborts (`slab_count_underflow`).
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

    /// `slab` into the empty set (it went `Empty`).
    fn empty_add(&mut self, slab: usize) {
        debug_assert!(
            !self.empty.map.is_set(slab),
            "empty_add: slab {slab} already in the empty set"
        );
        self.empty.map.set(slab);
        self.stats.empty += 1;
    }

    /// `slab` out of the empty set. Tests only: a refill and a purge
    /// spell the pair out (`next_slab`, `settle`).
    #[cfg(test)]
    fn empty_take(&mut self, slab: usize) {
        debug_assert!(
            self.empty.map.is_set(slab),
            "empty_take: slab {slab} not in the empty set"
        );
        self.empty.map.clear(slab);
        self.stats.empty = self
            .stats
            .empty
            .checked_sub(1)
            .unwrap_or_else(|| abort::slab_count_underflow("empty"));
    }

    /// `slab` into the purged set. Tests only: a purge spells the pair
    /// out (`settle`).
    #[cfg(test)]
    fn purged_add(&mut self, slab: usize) {
        debug_assert!(
            !self.purged.map.is_set(slab),
            "purged_add: slab {slab} already in the purged set"
        );
        self.purged.map.set(slab);
        self.stats.purged += 1;
    }

    /// `slab` out of the purged set. Tests only: a refill spells the
    /// pair out (`next_slab`).
    #[cfg(test)]
    fn purged_take(&mut self, slab: usize) {
        debug_assert!(
            self.purged.map.is_set(slab),
            "purged_take: slab {slab} not in the purged set"
        );
        self.purged.map.clear(slab);
        self.stats.purged = self
            .stats
            .purged
            .checked_sub(1)
            .unwrap_or_else(|| abort::slab_count_underflow("purged"));
    }
}

impl ClassState {
    /// Create the state for `arena` over class `c`'s table, at `tp`
    /// (element `c` of the tables `tables` covers from `base` on), with
    /// its three slab sets as views of the arena's set table at
    /// `sets_ptr` (slot `kind * count + pos[class]` of the class's set
    /// group, `Set::in_table`), all empty, and nothing owned or committed.
    /// The state keeps the tables' frozen permissions, and the sets
    /// `sets_perm`, the set table's. The tables and the set table must
    /// outlive the state. `meta` and `regions` are unused: the sets live
    /// in the set table, the descriptors in the class's table. The three
    /// sets take their slots' permissions out of `slots` (`SetTable::new`
    /// handed them out); all of these permissions are ghost.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        meta: &Meta,
        regions: &Regions,
        tp: *mut SlabTable,
        tables: Tracked<&'static Map<int, PointsTo<SlabTable>>>,
        base: Ghost<*mut SlabTable>,
        c: Ghost<int>,
        arena: u16,
        sets_ptr: *const SetTable,
        sets_perm: Tracked<&'static PointsTo<SetTable>>,
        _slots: Tracked<&mut Map<(int, int), ZeroRaw>>,
    ) -> Option<ClassState> {
        let table = vstd::raw_ptr::ptr_ref(tp, Tracked::assume_new());
        let _ = (meta, regions);
        let sets = vstd::raw_ptr::ptr_ref(sets_ptr, sets_perm);
        let class = table.class as usize;
        debug_assert!(table.nslabs == sets.nslabs[SetTable::group_of(class)]);
        let partial = Set::in_table(sets_ptr, sets_perm, class, 0, Tracked::assume_new());
        let empty = Set::in_table(sets_ptr, sets_perm, class, 1, Tracked::assume_new());
        let purged = Set::in_table(sets_ptr, sets_perm, class, 2, Tracked::assume_new());
        let inner = Inner {
            partial,
            empty,
            purged,
            stats: ClassStats {
                committed: 0,
                guards: 0,
                empty: 0,
                purged: 0,
                nonfull: 0,
            },
            group_next: 0,
            group_end: 0,
            states: Ghost::assume_new(),
            committed: Ghost::assume_new(),
        };
        Some(ClassState {
            class: table.class,
            arena,
            table: table_ptr(tp, tables, base, c),
            gate: Decayer::new(),
            inner: Padded(Lock::new(inner, Ghost::assume_new())),
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
        self.table.get()
    }

    #[inline]
    pub fn class(&self) -> &'static Class {
        &CLASSES.table()[self.class as usize]
    }

    #[inline]
    #[cfg(test)]
    pub fn arena(&self) -> u16 {
        self.arena
    }

    #[inline]
    #[cfg(test)]
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

    // The accessors below read and write the owner word and a slab
    // descriptor under this arena's class lock (`g`, the value the caller
    // holds). Each compiles to the one access in its body; `g` and `slab`
    // name what the access is under and of.

    /// The arena that owns `slab` ([`SlabTable::owner`]), read under this
    /// arena's class lock (`g`).
    #[allow(unused_variables)]
    #[inline(always)]
    fn owner_locked(&self, g: &Inner, slab: usize) -> u16 {
        self.table().owner(slab)
    }

    /// The state of slab `slab`'s descriptor `d`, read under this arena's
    /// class lock (`g`).
    #[allow(unused_variables)]
    #[inline(always)]
    fn state_locked(&self, g: &Inner, d: &SlabDesc, slab: usize) -> State {
        d.state()
    }

    /// `SlabDesc::set_state` on slab `slab`'s descriptor `d` under this
    /// arena's class lock (`g`). Callers never pass `Fresh`: an owned slab
    /// that left `Fresh` does not return to it.
    #[allow(unused_variables)]
    #[inline(always)]
    fn set_state_locked(&self, g: &mut Inner, d: &SlabDesc, slab: usize, s: State) {
        d.set_state(s, Tracked::assume_new());
    }

    /// `SlabDesc::set_dirty_at` on slab `slab`'s descriptor `d` under this
    /// arena's class lock (`g`).
    #[allow(unused_variables)]
    #[inline(always)]
    fn set_dirty_locked(&self, g: &mut Inner, d: &SlabDesc, slab: usize, ns: u64) {
        d.set_dirty_at(ns, Tracked::assume_new());
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

    /// [`claim`] for `slot_randomize` (design section 4): the same slabs
    /// and the same reservations (`plan` is untouched,
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
        let mut picks = [Pick {
            slab: 0,
            n: 0,
            zero: false,
            state: State::Active,
        }; MAX_PICKS];
        while got < out.len() {
            let np = self.plan(meta, out.len() - got, &mut picks);
            if np == 0 {
                break;
            }
            got += self.finish(
                &picks[..np],
                &mut out[got..],
                &mut zeroed,
                rng.as_deref_mut(),
            );
        }
        Claim {
            n: got,
            zeroed: got > 0 && zeroed,
        }
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
    /// Modelled at `model::Model::step_refill`, `protocol::Refill::Finish`
    /// (one pick at a time).
    ///
    /// [`plan`]: ClassState::plan
    /// [`take`]: ClassState::take
    // The loop is the spelled-out `for` (see the comment in the body).
    #[allow(clippy::while_let_loop)]
    fn finish(
        &self,
        picks: &[Pick],
        out: &mut [*mut u8],
        zeroed: &mut bool,
        mut rng: Option<&mut u64>,
    ) -> usize {
        let cls = self.class();
        let check_zero = purge::check_zero();
        let mut got = 0;
        // `for pick in picks`, spelled out: Verus rejects `continue` in a
        // `for` loop, and the `continue` below keeps the aarch64 codegen of
        // the plain loop (an `if`/`else` in its place does not).
        let mut iter = picks.iter();
        loop {
            let pick = match iter.next() {
                Some(pick) => pick,
                None => break,
            };
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
                let Some((w, bits)) =
                    bm.claim_from(word, want - k, Tracked::assume_new(), Tracked::assume_new())
                else {
                    // Every word was full when inspected. `plan` reserved
                    // no more than `nslots - live` and set bits never
                    // exceed `live`, so the slots exist: a concurrent
                    // claimer took the bits we looked at and left ours
                    // in a word we had already passed. Scan again.
                    empty_passes += 1;
                    if empty_passes == MAX_EMPTY_PASSES {
                        let d = self.desc(slab);
                        reservation_unmet(
                            self.class as usize,
                            slab,
                            want,
                            k,
                            d.live(),
                            bm.allocated(cls.nslots()),
                            cls.nslots(),
                        );
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
        let mut g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
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
            let s = match self.next_slab(&mut g, meta) {
                Some(s) => s,
                None => break,
            };
            self.take(&mut g, s, &mut remaining, picks, &mut np);
        }
        np
    }

    /// Reserve `min(nslots - live, *remaining)` slots of `slab` under the
    /// lock, fix up its state and partial bit, and record the pick.
    /// Modelled at `model::Model::take`, inside the locked step
    /// `protocol::Refill::PlanLocked` of `model::Model::step_refill`.
    fn take(
        &self,
        g: &mut Inner,
        slab: usize,
        remaining: &mut usize,
        picks: &mut [Pick; MAX_PICKS],
        np: &mut usize,
    ) {
        let desc = self.desc(slab);
        let live = desc.live();
        self.take_checks(g, desc, slab, live);
        // `live > nslots` would reserve a wrapped count (the release
        // profile does not check overflow; the dev profile does).
        assert!(
            live <= self.class().nslots(),
            "class: slab live count above its slot count"
        );
        let avail = self.class().nslots() - live;
        let n = avail.min(*remaining);
        let state = self.state_locked(g, desc, slab);
        if purge::check_zero() {
            self.check_shadow(slab, "take");
            if state == State::Fresh {
                if live != 0 {
                    fresh_with_live(
                        self.class as usize,
                        slab,
                        self.arena,
                        desc,
                        self.table().bits(slab).allocated(self.class().nslots()),
                    );
                }
                self.check_fresh_slab_is_zero(slab);
            }
        }
        if state != State::Active {
            self.set_state_locked(g, desc, slab, State::Active);
            self.set_dirty_locked(g, desc, slab, 0);
        }
        desc.add_live(n);
        // Under the lock (`g`), so a plain load-add-store is exclusive.
        self.claimed.store(
            self.claimed.load(Ordering::Relaxed).wrapping_add(n as u64),
            Ordering::Relaxed,
        );
        if live == 0 && n == avail {
            g.clear_partial(slab);
        } else {
            g.set_partial(slab);
        }
        // `Purged` memory is zero only while every discard so far was
        // `MADV_DONTNEED` (`purge::purged_is_zero`).
        let zero = state.is_zero() && (state != State::Purged || purge::purged_is_zero());
        if zero {
            self.claim_prefix(slab, n, state, live);
        }
        picks[*np] = Pick {
            slab: slab as u32,
            n: n as u32,
            zero,
            state,
        };
        *np += 1;
        *remaining -= n;
    }

    /// `take`'s debug checks: the slab's owner word and its descriptor's
    /// arena name this arena. Inlined and `track_caller`, so a failure
    /// names `take`.
    #[allow(unused_variables)]
    #[inline(always)]
    #[track_caller]
    fn take_checks(&self, g: &Inner, desc: &SlabDesc, slab: usize, live: usize) {
        if cfg!(debug_assertions) {
            let owner = self.table().owner(slab);
            let arena = desc.arena();
            debug_assert!(
                owner == self.arena,
                "take: slab {slab} state {:?} live {live}: owner {owner}, not arena {}",
                desc.state(),
                self.arena
            );
            debug_assert!(
                arena == self.arena,
                "take: slab {slab} descriptor arena {arena}, not arena {}",
                self.arena
            );
        }
    }

    /// The slots of a zero pick, decided under the class lock. A `Fresh`
    /// or `Purged` slab has `live == 0` and no bit
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
            let got = bm.claim_word(w, want, Tracked::assume_new(), Tracked::assume_new());
            if got != expect {
                zero_slab_had_bits_set(
                    self.class as usize,
                    slab,
                    self.arena,
                    state,
                    live,
                    w,
                    got,
                    expect,
                );
            }
            k += want;
            w += 1;
        }
    }

    /// `live` reached zero: make the slab `Empty` unless a refill has
    /// already reserved it again. Modelled at `model::Model::step_release`,
    /// `protocol::Release::LockLiveZero` (the lock) and `OnLiveZero` (the
    /// owner test and the transition).
    fn on_live_zero(&self, slab: usize) {
        let mut first = false;
        {
            let mut g = self
                .inner
                .lock(Tracked::assume_new(), Tracked::assume_new());
            if purge::check_zero() {
                self.check_shadow(slab, "on_live_zero");
            }
            // A slab of another arena is no business of this arena's sets.
            if self.owner_locked(&g, slab) != self.arena {
                return;
            }
            let desc = self.desc(slab);
            if desc.live() == 0 && self.state_locked(&g, desc, slab) == State::Active {
                let now = crate::sys::now_coarse_ns().max(1);
                self.set_state_locked(&mut g, desc, slab, State::Empty);
                self.set_dirty_locked(&mut g, desc, slab, now);
                g.clear_partial(slab);
                g.empty_add(slab);
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
            wrong_arena(
                p as usize,
                self.class as usize,
                self.table().owner(slab),
                self.arena,
            );
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
    /// from the table's dense owner array, once per slab switch, and checked
    /// against the slab descriptor's `arena` (a line `dec_live` reads
    /// anyway): the word routed the batch here, so a descriptor that
    /// disagrees means one of them is corrupt, and the batch aborts before
    /// a bit is cleared (`owner_mismatch`).
    /// Modelled at `model::Model::step_release`, `protocol::Release::FetchAnd`
    /// (the `fetch_and` and the double-free check) and `SubLive`.
    ///
    /// [`release`]: ClassState::release
    // Hot free path: the counter keeps the codegen as measured.
    #[allow(clippy::explicit_counter_loop)]
    pub fn release_owned(&self, ptrs: &[*mut u8]) -> usize {
        let mut local = [0u64; MAX_SLAB_WORDS];
        let mut cur_slab = usize::MAX;
        let (mut lo_w, mut hi_w) = (usize::MAX, 0usize);
        let mut count = 0usize;
        let mut reopen = Reopen::new();
        let mut end = ptrs.len();
        let mut i = 0usize;
        for &p in ptrs.iter() {
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
                let d = self.desc(slab);
                if d.arena() != self.arena {
                    owner_mismatch(
                        self.class as usize,
                        slab >> self.table().lg_group,
                        slab,
                        self.arena,
                        d.arena(),
                    );
                }
                if d.state() == State::Fresh {
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
            i += 1;
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
    // Hot free path: indexing keeps the codegen as measured.
    #[allow(clippy::needless_range_loop)]
    fn release_slab(
        &self,
        slab: usize,
        local: &mut [u64; MAX_SLAB_WORDS],
        lo: usize,
        hi: usize,
        count: usize,
        reopen: &mut Reopen,
    ) {
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
        let (prev, was_full) = self
            .table()
            .bits(slab)
            .release(word, mask, Tracked::assume_new());
        if prev & mask != mask {
            let bad = (mask & !prev).trailing_zeros() as usize;
            let slot = word * 64 + bad;
            double_free(
                self.slot_ptr(slab, slot) as usize,
                self.class as usize,
                slab,
                slot,
            );
        }
        if was_full && !reopen.push(slab) {
            self.reopen(reopen);
            reopen.push(slab);
        }
    }

    /// Put the slabs in `reopen` back in the partial set, then clear the
    /// list. A slab that went `Empty` (or was purged) since its word was
    /// released stays out: it is in the empty or purged set, and a refill
    /// taking it from there sets its partial bit itself. Modelled at
    /// `model::Model::step_release`, `protocol::Release::LockReopen` (the
    /// lock) and `Reopen` (the state and owner test per slab).
    fn reopen(&self, reopen: &mut Reopen) {
        if reopen.len == 0 {
            return;
        }
        let mut g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
        for &s in &reopen.slabs[..reopen.len] {
            let slab = s as usize;
            // Between the word's clear and this lock hold the slab may have
            // gone `Empty` (in `dec_live`); only an `Active` slab of this
            // arena goes back into the partial set.
            if self.state_locked(&g, self.desc(slab), slab) == State::Active
                && self.owner_locked(&g, slab) == self.arena
            {
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

    /// This arena's slab counts; `guards` is the class-wide count from the
    /// table, since guard slabs belong to no arena.
    #[cfg(test)]
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
        let g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
        let mut s = g.stats;
        let claimed = self.claimed.load(Ordering::Relaxed);
        drop(g);
        s.guards = self.table().guards(Tracked::assume_new());
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
    #[cfg(test)]
    pub fn curregs(&self) -> usize {
        self.bin_stats().1
    }

    /// Slots ever claimed from this arena's slabs of the class (wrapping).
    #[inline]
    #[cfg(test)]
    pub fn claimed(&self) -> u64 {
        self.claimed.load(Ordering::Relaxed)
    }

    /// Slots claimed by cache refills (see the field); the compaction gate
    /// in `heap/` watches its rate.
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
        if on {
            &self.compact_on
        } else {
            &self.compact_off
        }
        .fetch_add(1, Ordering::Relaxed);
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
    /// compaction gate in `heap/` reads this; one lock hold.
    pub fn holes(&self) -> (usize, usize) {
        let (s, curregs) = self.bin_stats();
        let capacity = s.curslabs() * self.class().nslots();
        (capacity.saturating_sub(curregs), capacity)
    }

    /// Lowest slab in the partial set, if any (for tests and stats).
    /// `_ord` is the caller's lock-order token (ghost) for the class lock.
    pub fn lowest_partial(&self, _ord: Tracked<&mut LockOrder>) -> Option<usize> {
        self.inner
            .lock(Tracked::assume_new(), Tracked::assume_new())
            .partial
            .map
            .lowest_set()
    }

    /// Tests: whether `slab` is in this arena's partial, empty and purged
    /// sets.
    #[cfg(test)]
    pub(crate) fn set_membership(&self, slab: usize) -> (bool, bool, bool) {
        let g = self.inner.lock_t();
        (
            g.partial.map.is_set(slab),
            g.empty.map.is_set(slab),
            g.purged.map.is_set(slab),
        )
    }

    /// Whether `slab` is in the partial set (tests and the bins table,
    /// `api::print`). `_ord` is the caller's lock-order token (ghost) for the
    /// class lock.
    pub fn is_partial(&self, slab: usize, _ord: Tracked<&mut LockOrder>) -> bool {
        self.inner
            .lock(Tracked::assume_new(), Tracked::assume_new())
            .partial
            .map
            .is_set(slab)
    }
}
