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

#[cfg(verus_keep_ghost)]
use crate::arena::{Published, process_pub_loc};
use crate::layout::{CLASSES, Class, MAX_SLAB_WORDS, Regions};
#[cfg(verus_keep_ghost)]
use crate::layout::{NCLASSES, class_table, process_regions};
use crate::lock::{Lock, LockOrder, PAD_ALIGN, Padded, RawLock};
#[cfg(verus_keep_ghost)]
use crate::lock::{lock_frame, lock_init, lock_inv, rank_class, rank_meta, rank_sets};
use crate::meta::Meta;
use crate::purge::{self, Decayer};
use crate::slab::{DescPerm, SlabDesc, State};
#[cfg(verus_keep_ghost)]
use crate::slab::{desc_id, state_byte};
#[cfg(verus_keep_ghost)]
use crate::vspec::addr::cast_provenance;
use crate::vspec::zero::ZeroRaw;
use core::sync::atomic::{AtomicU64, Ordering};
// The whole prelude only under Verus, which builds no tests: in a test
// build it would reach `tests.rs` through `use super::*`.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Map, Tracked, int, nat, verus};
use vstd::raw_ptr::PointsTo;
#[cfg(verus_keep_ghost)]
use vstd::resource::Loc;
use vstd::resource::set::{GhostPersistentSubset, GhostSetAuth};

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
#[cfg(verus_keep_ghost)]
use table::{desc_at, guard_marked, slab_marked, slab_owner};
use verify::{fresh_with_live, release_into_fresh};

verus! {

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
    pub fn curslabs(&self) -> (r: usize)
        requires
            self.empty + self.purged <= self.committed,
        ensures
            r == self.committed - self.empty - self.purged,
    {
        self.committed - self.empty - self.purged
    }
}

} // verus!

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

verus! {

/// Slabs one lock hold of a refill may reserve from (see
/// [`ClassState::plan`]), and slabs one lock hold of a release batch may
/// reopen ([`Reopen`]). Sized so that the common refill and flush of every
/// class fit in one hold: with the 4 KiB slabs of the 512 and 1024-byte
/// classes (`layout::LG_MIN_SLAB`) a 128-slot refill of the 512 class and a
/// 64-slot one of the 1024 class each span 16 slabs.
pub(crate) const MAX_PICKS: usize = 16;

} // verus!

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

verus! {

/// Slabs a release batch must put back in the partial set. Consecutive
/// duplicates are dropped; a full list is flushed under the lock.
struct Reopen {
    slabs: [u32; MAX_PICKS],
    len: usize,
}

impl Reopen {
    #[inline(always)]
    fn new() -> (r: Reopen)
        ensures
            r.len == 0,
    {
        Reopen {
            slabs: [0; MAX_PICKS],
            len: 0,
        }
    }

    /// Record `slab`; `false` if the list is full (and `slab` is not the
    /// last entry), in which case the caller must flush first.
    fn push(&mut self, slab: usize) -> (r: bool)
        requires
            old(self).len <= MAX_PICKS,
        ensures
            final(self).len <= MAX_PICKS,
            r ==> final(self).len > 0 && final(self).slabs[final(self).len - 1] == slab as u32,
            !r ==> final(self).len == MAX_PICKS && *final(self) == *old(self),
    {
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
    /// Ghost: the slabs this arena owns (their group's owner word names
    /// this arena, guard slabs included), each with the state its
    /// descriptor holds while this lock is held. The accessors that tie it
    /// to the descriptors (`state_locked` and the others) are unverified;
    /// the descriptor permissions ([`Inner::dperms`]) record it.
    #[allow(dead_code)]
    states: Ghost<Map<nat, State>>,
    /// Ghost: this arena's committed slabs of this class (guards
    /// excluded), the slabs `stats.committed` counts.
    #[allow(dead_code)]
    committed: Ghost<vstd::set::Set<nat>>,
    /// Ghost: the `dirty_at` stamp each owned slab's descriptor holds
    /// while this lock is held.
    #[allow(dead_code)]
    dirty: Ghost<Map<nat, u64>>,
    /// Ghost: the arena and the class table this lock value belongs to
    /// (set by `new`, never changed).
    #[allow(dead_code)]
    arena: Ghost<u16>,
    #[allow(dead_code)]
    table: Ghost<SlabTable>,
    /// Ghost: the fresh markers of every slab this arena owns but its
    /// guards, which `AtomicBitmap::init` returned when `take_group`
    /// handed the slab's group out: what a claim from the slab needs
    /// (`AtomicBitmap::inited_by`); and the init markers of its guard
    /// slabs, which `take_group` took as they were (`guard_marked`).
    #[allow(dead_code)]
    inited: Tracked<GhostPersistentSubset<int>>,
    /// Ghost: the permissions ([`DescPerm`]) of the descriptors of the
    /// slabs this arena owns, which `take_group` handed over with the
    /// slabs; each records the state and stamp `states` and `dirty` hold,
    /// and, but for a guard slab, this arena ([`descs_ok`]).
    #[allow(dead_code)]
    dperms: Tracked<Map<nat, DescPerm>>,
    /// Ghost: the authority of the record of this state's owned slabs,
    /// whose every element is an owned slab, at a location of this state
    /// alone (minted by `new`). `take_group` adds a group's slabs as it
    /// stores the group's owner word, and keeps their persistent part in
    /// the word's invariant (`table::OwnerPred`), for the owner checks to
    /// tell that a group whose owner word names this arena is this
    /// state's.
    #[allow(dead_code)]
    oauth: Tracked<GhostSetAuth<int>>,
}

} // verus!

// SAFETY: `TwoLevelBitmap` holds a raw pointer into the metadata region,
// which is process-global and only ever accessed through this lock.
unsafe impl Send for Inner {}

verus! {

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

} // verus!

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

verus! {

/// The tables of every class, from `base` on, through the frozen
/// permissions `m`, one per element: each initialised, at its index's
/// address, and the table of the class it is the index of. What a
/// [`ClassState`] keeps of its class's table ([`TablePtr`]).
pub open spec fn tables_ok(m: Map<int, PointsTo<SlabTable>>, base: *mut SlabTable) -> bool {
    &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] m.contains_key(c)
    &&& forall|c: int| 0 <= c < NCLASSES ==> (#[trigger] m[c]).ptr() == crate::vspec::ptr::ptr_add(base, c as usize)
    &&& forall|c: int| 0 <= c < NCLASSES ==> (#[trigger] m[c]).is_init()
    &&& forall|c: int| 0 <= c < NCLASSES ==> (#[trigger] m[c]).value().spec_class() == c
    &&& crate::vspec::ptr::in_alloc(base, NCLASSES as int * vstd::layout::size_of::<SlabTable>())
    &&& NCLASSES * vstd::layout::size_of::<SlabTable>() <= isize::MAX
}

} // verus!

/// The pointer to its table a [`ClassState`] keeps, in a module of its own
/// so that [`table_ptr`](tptr::table_ptr) is the only maker of one.
mod tptr {
    use super::SlabTable;
    #[cfg(verus_keep_ghost)]
    use super::tables_ok;
    #[cfg(verus_keep_ghost)]
    use crate::layout::NCLASSES;
    use vstd::prelude::*;
    use vstd::raw_ptr::PointsTo;

    verus! {

    /// A pointer to the table of class `c`, with (ghost, no bytes: the
    /// layout is the pointer's) the frozen permissions to every class's
    /// table, from `base` on ([`tables_ok`]), whose element `c` is the
    /// pointer's.
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

    impl TablePtr {
        /// The pointer is to element `c` of the tables its permissions
        /// cover (what [`table_ptr`] requires).
        #[verifier::type_invariant]
        spec fn inv(self) -> bool {
            &&& tables_ok(*self.tables@, self.base@)
            &&& 0 <= self.c@ < NCLASSES
            &&& self.tables@[self.c@].ptr() == self.p
            &&& *self.perm@ == self.tables@[self.c@]
        }
    }

    /// The table `p` points at.
    pub(super) closed spec fn table_at(p: TablePtr) -> SlabTable {
        p.tables@[p.c@].value()
    }

    /// The class whose table `p` points at.
    pub(super) closed spec fn index_of(p: TablePtr) -> int {
        p.c@
    }

    /// The pointer `p` to element `c` of the tables `tables` covers, from
    /// `base` on, as a [`TablePtr`], which [`table_at`] maps to that
    /// table.
    #[inline(always)]
    pub(super) fn table_ptr(
        p: *mut SlabTable,
        Tracked(tables): Tracked<&'static Map<int, PointsTo<SlabTable>>>,
        Ghost(base): Ghost<*mut SlabTable>,
        Ghost(c): Ghost<int>,
    ) -> (r: TablePtr)
        requires
            tables_ok(*tables, base),
            0 <= c < NCLASSES,
            tables[c].ptr() == p,
        ensures
            table_at(r) == tables[c].value(),
            index_of(r) == c,
    {
        TablePtr {
            p,
            tables: Tracked(tables),
            perm: Tracked(tables.tracked_borrow(c)),
            base: Ghost(base),
            c: Ghost(c),
        }
    }

    impl TablePtr {
        /// The table, through its frozen permission.
        #[inline(always)]
        pub(super) fn get(&self) -> (r: &SlabTable)
            ensures
                *r == table_at(*self),
        {
            proof {
                use_type_invariant(self);
            }
            vstd::raw_ptr::ptr_ref(self.p, self.perm)
        }

        /// The first class's table and the permissions to all of them
        /// ([`tables_ok`]), from the pointer to it.
        #[cfg_attr(not(test), allow(dead_code))]
        #[inline(always)]
        pub(super) fn tables(&self) -> (r: (*mut SlabTable, Tracked<&'static Map<int, PointsTo<SlabTable>>>))
            requires
                index_of(*self) == 0,
            ensures
                tables_ok(*r.1@, r.0),
        {
            proof {
                use_type_invariant(self);
                crate::vspec::ptr::axiom_ptr_add_addr(self.base@, 0);
                assert(self.p@ == self.base@@);
            }
            (self.p, Tracked(*self.tables.borrow()))
        }
    }

    } // verus!
}

use tptr::{TablePtr, table_ptr};
#[cfg(verus_keep_ghost)]
use tptr::{index_of, table_at};

verus! {

/// A set of the class state: a well-formed bitmap over `n` slabs whose
/// members are slab numbers below `n`.
spec fn set_wf(s: Set, n: nat) -> bool {
    &&& s.map.wf()
    &&& s.map.spec_nbits() == n
    &&& forall|b: nat| #[trigger] s.map@.contains(b) ==> b < n
}

/// Owned slab `s`'s descriptor permission in `dperms` is to its
/// descriptor in `t`, and records the ghost state `st` and stamp `dirty`
/// and, unless `s` is a guard slab (whose descriptor stays zero), the
/// arena `arena`.
spec fn desc_ok(dperms: Map<nat, DescPerm>, t: SlabTable, arena: u16, s: nat, st: State, dirty: u64) -> bool {
    &&& dperms.contains_key(s)
    &&& dperms[s].id() == desc_id(&desc_at(t, s as usize))
    &&& dperms[s].state() == state_byte(st)
    &&& dperms[s].dirty() == dirty
    &&& !t.spec_is_guard(s as usize) ==> dperms[s].arena() == arena
}

/// [`desc_ok`] at every slab `states` owns: the class lock value holds
/// the permission of every owned slab's descriptor, which agrees with
/// its ghost. Opaque ([`Inner::lemma_desc`] reads it at one slab); a
/// function of the fields it reads, so that a write of any other field
/// keeps it.
#[verifier::opaque]
spec fn descs_ok(states: Map<nat, State>, dirty: Map<nat, u64>, dperms: Map<nat, DescPerm>, t: SlabTable, arena: u16) -> bool {
    forall|s: nat| #[trigger] states.contains_key(s) ==> desc_ok(dperms, t, arena, s, states[s], dirty[s])
}

/// Every slab the record `rec` ([`Inner::oauth`]) has is one `states`
/// owns: only which slabs are owned matters, as for [`whole_groups`].
spec fn owned_rec(rec: vstd::set::Set<int>, states: Map<nat, State>) -> bool {
    &&& forall|x: int| #[trigger] rec.contains(x) ==> 0 <= x
    &&& forall|x: int| #[trigger] rec.contains(x) ==> states.contains_key(x as nat)
}

/// `owned` is whole hand-out groups of `t`: with a slab, every slab of its
/// group ([`SlabTable::spec_group_range`]). `take_group` hands an arena a
/// whole group, which `next_fresh_slab` adds to the ghost at once, and no
/// owned slab leaves. Opaque ([`lemma_whole_group`] reads it at two
/// slabs); a function of the owned set, so that a write of an owned
/// slab's state keeps it.
#[verifier::opaque]
spec fn whole_groups(owned: vstd::set::Set<nat>, t: SlabTable) -> bool {
    forall|x: nat, y: nat| #![trigger owned.contains(x), owned.contains(y)]
        owned.contains(x) && t.spec_group_range((x as usize) >> t.lg_group).0 <= y
            && y < t.spec_group_range((x as usize) >> t.lg_group).1 ==> owned.contains(y)
}

/// [`whole_groups`] at `x` and `y`: a slab `y` of the hand-out group of
/// an owned slab `x` is owned.
proof fn lemma_whole_group(owned: vstd::set::Set<nat>, t: SlabTable, x: nat, y: nat)
    requires
        whole_groups(owned, t),
        owned.contains(x),
        t.spec_group_range((x as usize) >> t.lg_group).0 <= y,
        y < t.spec_group_range((x as usize) >> t.lg_group).1,
    ensures
        owned.contains(y),
{
    reveal(whole_groups);
}

/// [`state_byte`] is one to one.
proof fn lemma_state_byte_inj(a: State, b: State)
    ensures
        state_byte(a) == state_byte(b) ==> a == b,
{
}

/// [`Inner::descs`] after a write of owned slab `k`'s descriptor: `g1`
/// is `g0` with `k`'s state, stamp and permission replaced in agreement.
proof fn lemma_descs_update(g0: Inner, g1: Inner, k: nat)
    requires
        g0.descs(),
        g0.owns(k),
        g1.states@.dom() == g0.states@.dom(),
        forall|s: nat| s != k ==> #[trigger] g1.states@[s] == g0.states@[s],
        forall|s: nat| s != k ==> #[trigger] g1.dirty@[s] == g0.dirty@[s],
        forall|s: nat| s != k ==> #[trigger] g1.dperms@[s] == g0.dperms@[s],
        forall|s: nat| s != k && #[trigger] g0.dperms@.contains_key(s) ==> g1.dperms@.contains_key(s),
        g1.table == g0.table,
        g1.arena == g0.arena,
        desc_ok(g1.dperms@, g1.table@, g1.arena@, k, g1.st(k), g1.dirty@[k]),
    ensures
        g1.descs(),
{
    reveal(descs_ok);
    assert forall|s: nat| #[trigger] g1.states@.contains_key(s)
        implies desc_ok(g1.dperms@, g1.table@, g1.arena@, s, g1.states@[s], g1.dirty@[s]) by {
        if s != k {
            assert(g0.states@.contains_key(s));
            assert(g0.dperms@.contains_key(s));
        }
    }
}

/// A set of the class state that lacks slab `x` holds fewer than `n` slabs.
proof fn lemma_set_len(s: Set, n: nat, x: nat)
    requires
        set_wf(s, n),
        x < n,
        !s.map@.contains(x),
    ensures
        s.map@.len() < n,
{
    broadcast use vstd::set_lib::range_set_properties;
    let r = vstd::set::Set::<nat>::range(0, n).remove(x);
    assert(s.map@.subset_of(r));
    vstd::set_lib::lemma_len_subset(s.map@, r);
}

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
    /// Slabs of the class (the sets' size).
    spec fn nbits(&self) -> nat {
        self.partial.map.spec_nbits()
    }

    /// Slab `s` belongs to this arena (ghost).
    spec fn owns(&self, s: nat) -> bool {
        self.states@.contains_key(s)
    }

    /// The state of owned slab `s` (ghost).
    spec fn st(&self, s: nat) -> State {
        self.states@[s]
    }

    /// The value holds the agreeing permission of every owned slab's
    /// descriptor ([`descs_ok`]).
    spec fn descs(&self) -> bool {
        descs_ok(self.states@, self.dirty@, self.dperms@, self.table@, self.arena@)
    }

    /// [`descs`](Inner::descs) at owned slab `s`.
    proof fn lemma_desc(&self, s: nat)
        requires
            self.descs(),
            self.owns(s),
        ensures
            desc_ok(self.dperms@, self.table@, self.arena@, s, self.st(s), self.dirty@[s]),
    {
        reveal(descs_ok);
    }

    /// The seam's counts and shapes: three well-formed sets over the
    /// class's slabs, each a view of the shared set table over a group of
    /// the class's slab count ([`Set::shared_over`]) and each count the
    /// population of its set, the committed count the size of the
    /// committed set, the owned and committed slabs among the class's (at
    /// most `2^52` of them), and the current group's cursor inside the
    /// class and, while slabs are left in it, inside one hand-out group,
    /// whose end is the current group's end; the slab before the current
    /// group's end is owned, and each owned slab's markers are held (fresh
    /// ones from `init`, a guard slab's init markers), which puts both
    /// below the watermark (`SlabTable::next_slab_locked`); every owned
    /// slab's owner ([`slab_owner`]) is this arena, the owned slabs being
    /// whole hand-out groups ([`whole_groups`]); the value holds every
    /// owned slab's descriptor permission, which agrees with the ghost
    /// ([`Inner::descs`]); and the owned-slab record has owned slabs only
    /// ([`owned_rec`]).
    spec fn base(&self) -> bool {
        &&& set_wf(self.partial, self.nbits())
        &&& set_wf(self.empty, self.nbits())
        &&& set_wf(self.purged, self.nbits())
        &&& self.partial.shared_over(self.nbits())
        &&& self.empty.shared_over(self.nbits())
        &&& self.purged.shared_over(self.nbits())
        &&& self.stats.nonfull == self.partial.map@.len()
        &&& self.stats.empty == self.empty.map@.len()
        &&& self.stats.purged == self.purged.map@.len()
        &&& self.stats.committed == self.committed@.len()
        &&& self.group_next <= self.group_end <= self.nbits()
        &&& self.group_next < self.group_end
            ==> self.group_end == self.table@.spec_group_range(self.group_next >> self.table@.lg_group).1
        &&& self.nbits() <= usize::MAX >> 12
        &&& forall|s: nat| #[trigger] self.committed@.contains(s) ==> s < self.nbits()
        &&& forall|s: nat| #[trigger] self.states@.contains_key(s) ==> s < self.nbits()
        &&& self.inited@.id() == self.table@.spec_slots().mloc
        &&& forall|s: nat| #[trigger] self.states@.contains_key(s) && !self.table@.spec_is_guard(s as usize)
            ==> slab_marked(self.table@, s as int, self.inited@@)
        &&& forall|s: nat| #[trigger] self.states@.contains_key(s) && self.table@.spec_is_guard(s as usize)
            ==> guard_marked(self.table@, s as int, self.inited@@)
        &&& self.group_end > 0 ==> self.owns((self.group_end - 1) as nat)
        &&& forall|s: nat| #[trigger] self.states@.contains_key(s)
            ==> slab_owner(self.table@.spec_class() as int, s as int) == self.arena@
        &&& whole_groups(self.states@.dom(), self.table@)
        &&& self.descs()
        &&& owned_rec(self.oauth@@, self.states@)
    }

    /// The ghost link at slab `s`, apart from the current group: the
    /// committed slabs are exactly the owned ones that left `Fresh` (a
    /// slab `next_fresh_slab` commits is made `Active` by `take` in the
    /// same lock hold); a partial slab is `Active`; the `empty` and
    /// `purged` sets are exactly the owned slabs in those states; and a
    /// guard slab is uncommitted and, when owned, `Fresh` (its descriptor
    /// stays zero).
    spec fn slab_core(&self, s: nat) -> bool {
        &&& self.committed@.contains(s) ==> self.owns(s)
        &&& self.committed@.contains(s) ==> self.st(s) != State::Fresh
        &&& self.owns(s) && self.st(s) != State::Fresh ==> self.committed@.contains(s)
        &&& self.partial.map@.contains(s) ==> self.owns(s) && self.st(s) == State::Active
        &&& self.empty.map@.contains(s) == (self.owns(s) && self.st(s) == State::Empty)
        &&& self.purged.map@.contains(s) == (self.owns(s) && self.st(s) == State::Purged)
        &&& self.table@.spec_is_guard(s as usize) ==> !self.committed@.contains(s)
            && (self.owns(s) ==> self.st(s) == State::Fresh)
    }

    /// The ghost link's current-group part at slab `s`: the current
    /// group's slabs from the cursor on are owned, `Fresh` and not
    /// committed.
    spec fn group_ok(&self, s: nat) -> bool {
        self.group_next <= s < self.group_end ==> self.owns(s) && self.st(s) == State::Fresh
            && !self.committed@.contains(s)
    }

    /// The ghost link at slab `s`.
    spec fn slab_ok(&self, s: nat) -> bool {
        &&& self.slab_core(s)
        &&& self.group_ok(s)
    }

    /// The seam's invariant, with the ghost link at every slab.
    spec fn inv(&self) -> bool {
        &&& self.base()
        &&& forall|s: nat| #[trigger] self.slab_ok(s)
    }

    /// The invariant with slab `x` exempt from the ghost link: the state
    /// between two writes of one transition (a slab's state and its set,
    /// or a slab taken out of a set and not yet made `Active`).
    spec fn inv_but(&self, x: int) -> bool {
        &&& self.base()
        &&& forall|s: nat| s != x ==> #[trigger] self.slab_ok(s)
    }

    /// Slab `s` is ready for `ClassState::take`: owned, committed (so not
    /// a guard slab), in neither idle set nor the current group's
    /// uncommitted rest, and the invariant holds elsewhere (a slab
    /// `next_slab` just took out of an idle set or committed fresh).
    spec fn ready(&self, s: nat) -> bool {
        &&& self.inv_but(s as int)
        &&& s < self.nbits()
        &&& self.owns(s)
        &&& self.committed@.contains(s)
        &&& !self.table@.spec_is_guard(s as usize)
        &&& !self.empty.map@.contains(s)
        &&& !self.purged.map@.contains(s)
        &&& !(self.group_next <= s < self.group_end)
    }

    /// `self` and `o` agree on everything [`Set::ensure`] keeps of a view
    /// of the shared set table (only an own store changes), so on the
    /// invariant too ([`lemma_same_sets_inv`]).
    spec fn same_sets(&self, o: Inner) -> bool {
        &&& self.same_sets_but_rec(o)
        &&& self.oauth@@ == o.oauth@@
        &&& self.oauth@.id() == o.oauth@.id()
    }

    /// [`Inner::same_sets`] but for the owned-slab record
    /// ([`Inner::oauth`]), which `SlabTable::take_group` adds a group to.
    spec fn same_sets_but_rec(&self, o: Inner) -> bool {
        &&& self.partial.map == o.partial.map
        &&& self.empty.map == o.empty.map
        &&& self.purged.map == o.purged.map
        &&& self.partial.store == o.partial.store
        &&& self.empty.store == o.empty.store
        &&& self.purged.store == o.purged.store
        &&& self.stats == o.stats
        &&& self.group_next == o.group_next
        &&& self.group_end == o.group_end
        &&& self.states == o.states
        &&& self.committed == o.committed
        &&& self.dirty == o.dirty
        &&& self.arena == o.arena
        &&& self.table == o.table
        &&& self.inited == o.inited
        &&& self.dperms == o.dperms
    }

    /// Values that agree as [`Inner::same_sets`] says agree on the invariant.
    proof fn lemma_same_sets(&self, o: Inner)
        requires
            self.same_sets(o),
        ensures
            self.nbits() == o.nbits(),
            self.base() == o.base(),
            forall|s: nat| #[trigger] self.slab_ok(s) == o.slab_ok(s),
            self.inv() == o.inv(),
    {
        assert forall|s: nat| #[trigger] self.slab_ok(s) == o.slab_ok(s) by {}
        if o.inv() {
            assert forall|s: nat| #[trigger] self.slab_ok(s) by {
                assert(o.slab_ok(s));
            }
        }
        if self.inv() {
            assert forall|s: nat| #[trigger] o.slab_ok(s) by {
                assert(self.slab_ok(s));
            }
        }
    }

    /// `self` and `o` agree on everything but the sets and counts.
    spec fn same_rest(&self, o: &Inner) -> bool {
        &&& self.stats.committed == o.stats.committed
        &&& self.stats.guards == o.stats.guards
        &&& self.group_next == o.group_next
        &&& self.group_end == o.group_end
        &&& self.nbits() == o.nbits()
        &&& self.states == o.states
        &&& self.committed == o.committed
        &&& self.dirty == o.dirty
        &&& self.arena == o.arena
        &&& self.table == o.table
        &&& self.inited == o.inited
        &&& self.dperms == o.dperms
        &&& self.oauth@@ == o.oauth@@
        &&& self.oauth@.id() == o.oauth@.id()
    }

    /// Commit the set words that hold every bit up to and including
    /// `slab` in the three sets ([`Set::ensure`]), for a group the arena
    /// is about to receive (`take_group`, under the commit lock). The sets
    /// are views of the shared set table, whose commit leaves them as they
    /// are.
    #[inline(always)]
    fn ensure_sets(&mut self, meta: &Meta, page: usize, slab: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Result<(), crate::sys::Errno>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_sets(0)),
            old(ord).allows(rank_meta()),
            old(self).base(),
            slab < old(self).nbits(),
            meta.is_process(),
        ensures
            final(self).same_sets(*old(self)),
            *final(ord) == *old(ord),
    {
        self.partial.ensure(meta, page, slab, Tracked(ord))?;
        self.empty.ensure(meta, page, slab, Tracked(ord))?;
        self.purged.ensure(meta, page, slab, Tracked(ord))
    }

    /// The idle slabs are among the committed ones, which are among the
    /// class's.
    proof fn lemma_counts(&self)
        requires
            self.inv(),
        ensures
            self.stats.empty + self.stats.purged <= self.stats.committed <= self.nbits(),
    {
        broadcast use vstd::set_lib::range_set_properties;
        let e = self.empty.map@;
        let p = self.purged.map@;
        let c = self.committed@;
        assert(e.disjoint(p)) by {
            assert forall|s: nat| !(e.contains(s) && p.contains(s)) by {
                assert(self.slab_ok(s));
            }
        }
        assert((e + p).subset_of(c)) by {
            assert forall|s: nat| #[trigger] (e + p).contains(s) implies c.contains(s) by {
                assert(self.slab_ok(s));
            }
        }
        vstd::set_lib::lemma_set_disjoint_lens(e, p);
        vstd::set_lib::lemma_len_subset(e + p, c);
        let r = vstd::set::Set::<nat>::range(0, self.nbits());
        assert(c.subset_of(r));
        vstd::set_lib::lemma_len_subset(c, r);
    }

    fn set_partial(&mut self, slab: usize)
        requires
            old(self).inv(),
            slab < old(self).nbits(),
            old(self).owns(slab as nat),
            old(self).st(slab as nat) == State::Active,
        ensures
            final(self).inv(),
            final(self).same_rest(old(self)),
            final(self).partial.map@ == old(self).partial.map@.insert(slab as nat),
            final(self).empty == old(self).empty,
            final(self).purged == old(self).purged,
    {
        if !self.partial.map.is_set(slab) {
            proof {
                lemma_set_len(self.partial, self.nbits(), slab as nat);
            }
            self.partial.map.set(slab);
            self.stats.nonfull += 1;
        } else {
            proof {
                assert(self.partial.map@.insert(slab as nat) =~= self.partial.map@);
            }
        }
        proof {
            assert forall|s: nat| #[trigger] self.slab_ok(s) by {
                assert(old(self).slab_ok(s));
            }
        }
    }

    fn clear_partial(&mut self, slab: usize)
        requires
            old(self).base(),
            slab < old(self).nbits(),
        ensures
            final(self).base(),
            final(self).same_rest(old(self)),
            final(self).partial.map@ == old(self).partial.map@.remove(slab as nat),
            final(self).empty == old(self).empty,
            final(self).purged == old(self).purged,
            old(self).inv() ==> final(self).inv(),
            forall|x: int| #[trigger] old(self).inv_but(x) ==> final(self).inv_but(x),
    {
        if self.partial.map.is_set(slab) {
            self.partial.map.clear(slab);
            self.stats.nonfull -= 1;
        } else {
            proof {
                assert(self.partial.map@.remove(slab as nat) =~= self.partial.map@);
            }
        }
        proof {
            assert forall|s: nat| old(self).slab_ok(s) implies #[trigger] self.slab_ok(s) by {}
        }
    }

    /// `slab` into the empty set (it went `Empty`). The caller closes the
    /// link at `slab`.
    fn empty_add(&mut self, slab: usize)
        requires
            old(self).base(),
            slab < old(self).nbits(),
            !old(self).empty.map@.contains(slab as nat),
        ensures
            final(self).base(),
            final(self).same_rest(old(self)),
            final(self).empty.map@ == old(self).empty.map@.insert(slab as nat),
            final(self).partial == old(self).partial,
            final(self).purged == old(self).purged,
            old(self).inv_but(slab as int) ==> final(self).inv_but(slab as int),
    {
        proof {
            lemma_set_len(self.empty, self.nbits(), slab as nat);
        }
        debug_assert!(
            !self.empty.map.is_set(slab),
            "empty_add: slab {slab} already in the empty set"
        );
        self.empty.map.set(slab);
        self.stats.empty += 1;
        proof {
            assert forall|s: nat| s != slab && old(self).slab_ok(s) implies #[trigger] self.slab_ok(s) by {}
        }
    }
}

} // verus!

impl Inner {
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

verus! {

broadcast use {table::lemma_spec_nslabs, table::lemma_spec_class};

impl ClassState {
    /// `class` is a class number, `table` points at that class's table
    /// (element `class` of the tables it has the permissions to), the
    /// sets behind the lock are over its slabs, and the lock ranks as
    /// this arena's lock of the class. `new` proves it: it takes `class`
    /// from the table and builds the sets over the set table's count for
    /// the class's slab size, which equals `table.nslabs` because both
    /// are the process's regions' count for the class (`SetTable::over`,
    /// `SlabTable::inv`).
    #[verifier::type_invariant]
    spec fn inv(self) -> bool {
        &&& self.class < NCLASSES
        &&& table_at(self.table).inv()
        &&& table_at(self.table).spec_class() == self.class as usize
        &&& index_of(self.table) == self.class as int
        &&& lock_init(self.inner.0).nbits() == table_at(self.table).spec_nslabs()
        &&& lock_init(self.inner.0).arena@ == self.arena
        &&& lock_init(self.inner.0).table@ == table_at(self.table)
        &&& self.inner.0.rank() == rank_class(self.arena as int, self.class as int)
    }

    /// The class's shared table.
    pub closed spec fn spec_table(&self) -> SlabTable {
        table_at(self.table)
    }

    /// Slabs of the class region.
    pub closed spec fn spec_nslabs(&self) -> usize {
        table_at(self.table).spec_nslabs()
    }

    /// The class number.
    pub closed spec fn spec_class(&self) -> usize {
        self.class as usize
    }

    /// The arena this state belongs to.
    pub closed spec fn spec_arena(&self) -> u16 {
        self.arena
    }

    /// The location of this state's owned-slab record ([`Inner::oauth`]):
    /// its class lock value's, made by `new`.
    pub closed spec fn spec_oloc(&self) -> Loc {
        lock_init(self.inner.0).oauth@.id()
    }

    /// `w` names this state as the published state of its class and arena
    /// index: the class `spec_class` state of the arena the process
    /// registry published at `spec_arena` ([`Published`]). Only the
    /// published states take groups (the callers of `take_group` hold such
    /// a record), and two records of one index agree, so one state per
    /// (arena index, class) takes groups.
    pub open spec fn published_by(&self, w: Published) -> bool {
        &&& w.id() == process_pub_loc()
        &&& w.key() == self.spec_arena() as int
        &&& w.value().len() == NCLASSES
        &&& w.value()[self.spec_class() as int] == self.spec_oloc()
    }
}

/// A class region's slots all have numbers in a `usize`: `nslabs` slabs of
/// at most `1 << lg_slab / size` slots each.
proof fn lemma_slots_fit(t: SlabTable)
    requires
        t.inv(),
    ensures
        (t.spec_nslabs() as int) * (class_table()[t.spec_class() as int].nslots as int) <= usize::MAX,
        (t.spec_nslabs() as int) * ((1usize << class_table()[t.spec_class() as int].lg_slab) as int) <= usize::MAX,
        (t.spec_nslabs() as int) * ((1usize << class_table()[t.spec_class() as int].lg_slab) as int) <= 0x200_0000_0000_0000,
        (t.spec_nslabs() as int) * (class_table()[t.spec_class() as int].nslots as int) <= 0x200_0000_0000_0000,
{
    broadcast use {table::lemma_spec_nslabs, table::lemma_spec_class};
    let c = class_table()[t.spec_class() as int];
    let lg = c.lg_slab as usize;
    let n = t.spec_nslabs();
    let m = (usize::MAX - t.base) as usize;
    vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
    vstd::bits::lemma_usize_shl_is_mul(1, lg);
    vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(n, lg, m);
    let m57 = ((1usize << 57usize) - t.base) as usize;
    assert((1usize << 57usize) == 0x200_0000_0000_0000usize) by (bit_vector);
    vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(n, lg, m57);
    let l8 = c.lg_slab;
    assert((1usize << l8) == (1usize << lg)) by (bit_vector)
        requires lg == l8 as usize;
    let p = (1usize << lg) as int;
    assert(n as int * p <= usize::MAX);
    assert((c.nslots as int) <= p) by (nonlinear_arith)
        requires (c.nslots as int) * (c.size as int) <= p, c.size >= 1;
    assert((n as int) * (c.nslots as int) <= (n as int) * p) by (nonlinear_arith)
        requires (c.nslots as int) <= p, n >= 0;
}

/// [`Inner::lemma_counts`] wherever the invariant is known: the idle
/// counts are at most the committed count, which is at most the class's
/// slab count.
broadcast proof fn lemma_inv_counts(v: Inner)
    ensures
        #[trigger] v.inv() ==> v.stats.empty + v.stats.purged <= v.stats.committed <= v.nbits(),
{
    if v.inv() {
        v.lemma_counts();
    }
}

/// Values that agree as [`Inner::same_sets`] says agree on the invariant
/// ([`Inner::lemma_same_sets`]), wherever [`Inner::same_sets`] is known.
broadcast proof fn lemma_same_sets_inv(a: Inner, b: Inner)
    ensures
        #[trigger] a.same_sets(b) ==> a.inv() == b.inv() && a.nbits() == b.nbits(),
{
    if a.same_sets(b) {
        a.lemma_same_sets(b);
    }
}

/// Assumption (`lock.rs`): the class lock's value keeps the set/count
/// seam's invariant, ghost link included, while the lock is free.
/// Verified holders show it through the postconditions of the `Inner`
/// methods and of `next_slab` and `next_fresh_slab` (with
/// `SlabTable::take_group` under it); `ClassState::new` proves it of the
/// value it creates the lock with, and `lowest_partial` and `is_partial`
/// only read the value. The unverified holders are the rest of the refill
/// and release paths (`plan`, `take`, `reopen`, `on_live_zero`), the purge
/// and decay walks (`decay.rs`), `bin_stats`, and the tests, but for those
/// that break the counts on purpose to reach an abort. Each of them
/// writes an owned slab's state and stamp only through the bridges
/// (`set_state_locked`, `set_dirty_locked`; `take`, `on_live_zero` and
/// the decay walk's purge, `settle`), which write the descriptor through
/// the permission the value holds and update the ghost state and stamp
/// with it: verified, so they keep `Inner::descs` when their callers
/// meet their preconditions, which nothing checks at these unverified
/// call sites. None of them updates the other ghost fields (`committed`,
/// `inited`, `oauth`), so for them the assumption is also the set and
/// count shapes (`Inner::base`), the sets' disjointness, and the ghost
/// link between the sets and the ghost states (`Inner::slab_core`), which
/// their set updates and the bridges' state updates are meant to keep
/// together. The fork handlers take the raw lock without touching the
/// value.
broadcast axiom fn axiom_class_lock_inv(v: Inner)
    ensures
        #[trigger] lock_inv(v) == v.inv(),
;

/// Assumption (`lock.rs`): the sets behind a class lock stay over the
/// slab count they were created with, and the value's arena, table and
/// owned-slab record's location are the ones `new` set. Every `Inner`
/// method keeps `nbits` and those fields (`same_rest`, `same_sets`),
/// nothing replaces the sets, and no code writes those ghost fields after
/// `new` (`take_group` adds to the record, at its location).
broadcast axiom fn axiom_class_lock(init: Inner, v: Inner)
    ensures
        #[trigger] lock_frame(init, v) == (v.nbits() == init.nbits() && v.arena == init.arena && v.table == init.table
            && v.oauth@.id() == init.oauth@.id()),
;

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
        Tracked(tables): Tracked<&'static Map<int, PointsTo<SlabTable>>>,
        Ghost(base): Ghost<*mut SlabTable>,
        Ghost(c): Ghost<int>,
        arena: u16,
        sets_ptr: *const SetTable,
        Tracked(sets_perm): Tracked<&'static PointsTo<SetTable>>,
        Tracked(slots): Tracked<&mut Map<(int, int), ZeroRaw>>,
    ) -> (r: Option<ClassState>)
        requires
            tables_ok(*tables, base),
            0 <= c < NCLASSES,
            tables[c].ptr() == tp,
            sets_perm.ptr() == sets_ptr,
            sets_perm.is_init(),
            sets_perm.value().over(process_regions()),
            forall|k: int| 0 <= k < sets::SETS_PER_CLASS ==> #[trigger] sets_perm.value().holds_set(*old(slots), c, k),
        ensures
            r matches Some(s) ==> s.spec_class() == c && s.spec_table() == tables[c].value() && s.spec_arena() == arena,
            *final(slots) == old(slots).remove((c, 0)).remove((c, 1)).remove((c, 2)),
    {
        broadcast use axiom_class_lock_inv;
        let table = vstd::raw_ptr::ptr_ref(tp, Tracked(tables.tracked_borrow(c)));
        proof {
            use_type_invariant(table);
        }
        let _ = (meta, regions);
        let sets = vstd::raw_ptr::ptr_ref(sets_ptr, Tracked(sets_perm));
        let class = table.class as usize;
        proof {
            assert(sets.class_ok(process_regions(), class as int));
            assert(sets.holds_set(*slots, class as int, 0));
            assert(sets.holds_set(*slots, class as int, 1));
            assert(sets.holds_set(*slots, class as int, 2));
        }
        debug_assert!(table.nslabs == sets.nslabs[SetTable::group_of(class)]);
        let tracked w0 = slots.tracked_remove((class as int, 0));
        let tracked w1 = slots.tracked_remove((class as int, 1));
        let tracked w2 = slots.tracked_remove((class as int, 2));
        let partial = Set::in_table(sets_ptr, Tracked(sets_perm), class, 0, Tracked(w0));
        let empty = Set::in_table(sets_ptr, Tracked(sets_perm), class, 1, Tracked(w1));
        let purged = Set::in_table(sets_ptr, Tracked(sets_perm), class, 2, Tracked(w2));
        let tracked im = table.empty_marks();
        let tracked dm = Map::<nat, DescPerm>::tracked_empty();
        let tracked (oa, _) = GhostSetAuth::<int>::new(vstd::set::Set::empty());
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
            // No slab owned or committed yet.
            states: Ghost(Map::empty()),
            committed: Ghost(vstd::set::Set::empty()),
            dirty: Ghost(Map::empty()),
            arena: Ghost(arena),
            table: Ghost(*table),
            inited: Tracked(im),
            dperms: Tracked(dm),
            oauth: Tracked(oa),
        };
        proof {
            let n = inner.nbits();
            assert(set_wf(inner.partial, n) && set_wf(inner.empty, n) && set_wf(inner.purged, n));
            assert(inner.partial.map@.len() == 0 && inner.committed@.len() == 0);
            assert(usize::MAX >> 12 == 0xF_FFFF_FFFF_FFFFusize) by (bit_vector);
            assert(n <= usize::MAX >> 12);
            assert(inner.descs()) by {
                reveal(descs_ok);
            }
            assert(whole_groups(inner.states@.dom(), inner.table@)) by {
                reveal(whole_groups);
            }
            assert(owned_rec(inner.oauth@@, inner.states@));
            assert(inner.base());
            assert forall|s: nat| #[trigger] inner.slab_ok(s) by {}
        }
        Some(ClassState {
            class: table.class,
            arena,
            table: table_ptr(tp, Tracked(tables), Ghost(base), Ghost(c)),
            gate: Decayer::new(),
            inner: Padded(Lock::new(inner, Ghost(rank_class(arena as int, table.class as int)))),
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
    pub fn table(&self) -> (r: &SlabTable)
        ensures
            *r == self.spec_table(),
            r.spec_class() == self.spec_class(),
            r.spec_nslabs() == self.spec_nslabs(),
    {
        proof {
            use_type_invariant(self);
        }
        self.table.get()
    }

    /// Every class's table and the frozen permissions to them
    /// ([`tables_ok`]), from class 0's state: what
    /// [`Arena::init`](crate::arena::Arena::init) builds another arena's
    /// states over.
    // Only the tests call it in ordinary builds: `Arenas::build` passes
    // `Shared::tables` and assumed permissions.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline(always)]
    pub(crate) fn tables(&self) -> (r: (*mut SlabTable, Tracked<&'static Map<int, PointsTo<SlabTable>>>))
        requires
            self.spec_class() == 0,
        ensures
            tables_ok(*r.1@, r.0),
    {
        proof {
            use_type_invariant(self);
        }
        self.table.tables()
    }

    #[inline]
    pub fn class(&self) -> (r: &'static Class)
        ensures
            *r == class_table()[self.spec_class() as int],
            r.wf(),
    {
        proof {
            use_type_invariant(self);
        }
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
    pub fn raw_lock(&self) -> (r: &RawLock)
        ensures
            *r == self.spec_raw_lock(),
            r.rank() == rank_class(self.spec_arena() as int, self.spec_class() as int),
    {
        proof {
            use_type_invariant(self);
        }
        self.inner.raw()
    }

    /// The lock [`Self::raw_lock`] returns.
    pub closed spec fn spec_raw_lock(&self) -> RawLock {
        self.inner.0.spec_raw()
    }

    /// [`SlabTable::is_guard_slab`].
    #[inline]
    pub fn is_guard_slab(&self, slab: usize) -> bool {
        self.table().is_guard_slab(slab)
    }

    /// [`SlabTable::committed_slabs`]: slabs of every arena.
    #[inline]
    pub fn committed_slabs(&self) -> (r: usize)
        ensures
            r <= self.spec_nslabs(),
    {
        self.table().committed_slabs()
    }
}

} // verus!

impl ClassState {
    /// [`SlabTable::desc`].
    #[inline]
    pub fn desc(&self, slab: usize) -> &SlabDesc {
        self.table().desc(slab)
    }
}

verus! {

impl ClassState {
    #[inline]
    fn slab_addr(&self, slab: usize) -> (r: usize)
        requires
            slab < self.spec_nslabs(),
        ensures
            r == self.spec_table().base + slab * ((1usize << class_table()[self.spec_class() as int].lg_slab) as int),
            r + (1usize << class_table()[self.spec_class() as int].lg_slab) <= 1usize << 57,
    {
        self.table().slab_addr(slab)
    }

    #[inline]
    fn slot_ptr(&self, slab: usize, slot: usize) -> (r: *mut u8)
        requires
            slab < self.spec_nslabs(),
            slot < class_table()[self.spec_class() as int].slab_bits,
        ensures
            r@.addr == process_regions().slot_at(self.spec_class() as int, slab as int, slot as int),
            r@.provenance == cast_provenance(),
    {
        self.table().slot_ptr(slab, slot)
    }
}

} // verus!

impl ClassState {
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
}

verus! {

impl ClassState {
    /// The state of slab `slab`'s descriptor `d`, read under this arena's
    /// class lock (`g`): for a slab the arena owns, the state its ghost
    /// records, through the permission `g` holds ([`Inner::descs`]).
    /// Every write of an owned slab's state byte under the lock goes
    /// through that permission, with the ghost update to the same state
    /// ([`set_state_locked`]), and `take_group` initialised the descriptor
    /// through it, `Fresh`, before `next_fresh_slab` added the slab
    /// `Fresh` to the ghost.
    ///
    /// [`set_state_locked`]: ClassState::set_state_locked
    #[allow(unused_variables)]
    #[inline(always)]
    fn state_locked(&self, g: &Inner, d: &SlabDesc, slab: usize) -> (r: State)
        requires
            g.descs(),
            g.table@ == self.spec_table(),
            *d == desc_at(self.spec_table(), slab),
        ensures
            g.owns(slab as nat) ==> r == g.st(slab as nat),
    {
        proof {
            if g.owns(slab as nat) {
                g.lemma_desc(slab as nat);
            }
        }
        let r = d.state_held(Ghost(slab as nat), Tracked(g.dperms.borrow()));
        proof {
            if g.owns(slab as nat) {
                lemma_state_byte_inj(r, g.st(slab as nat));
            }
        }
        r
    }

    /// The `dirty_at` stamp of slab `slab`'s descriptor `d`, read under
    /// this arena's class lock (`g`) through the permission `g` holds, as
    /// [`state_locked`] reads the state. Every write of a stamp goes
    /// through that permission ([`set_dirty_locked`], `take_group`'s
    /// `init`).
    ///
    /// [`state_locked`]: ClassState::state_locked
    /// [`set_dirty_locked`]: ClassState::set_dirty_locked
    #[allow(unused_variables)]
    #[inline(always)]
    fn dirty_locked(&self, g: &Inner, d: &SlabDesc, slab: usize) -> u64 {
        d.dirty_held(Ghost(slab as nat), Tracked(g.dperms.borrow()))
    }

    /// `SlabDesc::set_state` on owned slab `slab`'s descriptor `d` under
    /// this arena's class lock (`g`), through the permission `g` holds for
    /// it, with the ghost state updated to match (the write side of
    /// [`state_locked`]). Nothing else changes. Never to `Fresh`: an owned
    /// slab that left `Fresh` does not return to it.
    ///
    /// [`state_locked`]: ClassState::state_locked
    #[allow(unused_variables)]
    #[inline(always)]
    fn set_state_locked(&self, g: &mut Inner, d: &SlabDesc, slab: usize, s: State)
        requires
            old(g).descs(),
            old(g).table@ == self.spec_table(),
            *d == desc_at(self.spec_table(), slab),
            old(g).owns(slab as nat),
            s != State::Fresh,
        ensures
            final(g).descs(),
            final(g).states@ == old(g).states@.insert(slab as nat, s),
            final(g).states@.dom() == old(g).states@.dom(),
            final(g).partial == old(g).partial,
            final(g).empty == old(g).empty,
            final(g).purged == old(g).purged,
            final(g).stats == old(g).stats,
            final(g).group_next == old(g).group_next,
            final(g).group_end == old(g).group_end,
            final(g).committed == old(g).committed,
            final(g).dirty == old(g).dirty,
            final(g).arena == old(g).arena,
            final(g).table == old(g).table,
            final(g).inited == old(g).inited,
            final(g).oauth == old(g).oauth,
    {
        let ghost g0 = *g;
        let ghost k = slab as nat;
        proof {
            g.lemma_desc(k);
        }
        let tracked mut p = g.dperms.borrow_mut().tracked_remove(slab as nat);
        d.set_state(s, Tracked(&mut p));
        proof {
            g.dperms.borrow_mut().tracked_insert(slab as nat, p);
            g.states@ = g.states@.insert(k, s);
            assert(g.states@.dom() =~= g0.states@.dom());
            lemma_descs_update(g0, *g, k);
        }
    }

    /// `SlabDesc::set_dirty_at` on owned slab `slab`'s descriptor `d`
    /// under this arena's class lock (`g`), through the permission `g`
    /// holds for it, with the ghost stamp updated to match. Nothing else
    /// changes.
    #[allow(unused_variables)]
    #[inline(always)]
    fn set_dirty_locked(&self, g: &mut Inner, d: &SlabDesc, slab: usize, ns: u64)
        requires
            old(g).descs(),
            old(g).table@ == self.spec_table(),
            *d == desc_at(self.spec_table(), slab),
            old(g).owns(slab as nat),
        ensures
            final(g).descs(),
            final(g).dirty@ == old(g).dirty@.insert(slab as nat, ns),
            final(g).states == old(g).states,
            final(g).partial == old(g).partial,
            final(g).empty == old(g).empty,
            final(g).purged == old(g).purged,
            final(g).stats == old(g).stats,
            final(g).group_next == old(g).group_next,
            final(g).group_end == old(g).group_end,
            final(g).committed == old(g).committed,
            final(g).arena == old(g).arena,
            final(g).table == old(g).table,
            final(g).inited == old(g).inited,
            final(g).oauth == old(g).oauth,
    {
        let ghost g0 = *g;
        let ghost k = slab as nat;
        proof {
            g.lemma_desc(k);
        }
        let tracked mut p = g.dperms.borrow_mut().tracked_remove(slab as nat);
        d.set_dirty_at(ns, Tracked(&mut p));
        proof {
            g.dperms.borrow_mut().tracked_insert(slab as nat, p);
            g.dirty@ = g.dirty@.insert(k, ns);
            lemma_descs_update(g0, *g, k);
        }
    }
}

} // verus!

impl ClassState {
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
            let s = match self.next_slab(&mut g, meta, Tracked::assume_new(), Tracked::assume_new())
            {
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
}

verus! {

impl ClassState {
    /// This arena's slab counts; `guards` is the class-wide count from the
    /// table, since guard slabs belong to no arena.
    #[cfg(test)]
    pub fn stats(&self) -> ClassStats {
        self.bin_stats(Tracked::assume_new()).0
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
    /// `ord` is the caller's lock-order token (ghost) for the class lock.
    pub fn bin_stats(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (ClassStats, usize))
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            r.0.empty + r.0.purged <= r.0.committed <= self.spec_nslabs(),
            (r.0.committed as int) * (class_table()[self.spec_class() as int].nslots as int) <= usize::MAX,
            // The clamp: no more slots out than the live-capable slabs hold.
            (r.1 as int) <= ((r.0.committed - r.0.empty - r.0.purged) as int) * (class_table()[self.spec_class() as int].nslots as int),
            *final(ord) == *old(ord),
    {
        broadcast use {axiom_class_lock, axiom_class_lock_inv, lemma_inv_counts};
        proof {
            use_type_invariant(self);
            lemma_slots_fit(table_at(self.table));
        }
        let released = self.released.load(Ordering::Relaxed);
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        let mut s = g.stats;
        let claimed = self.claimed.load(Ordering::Relaxed);
        drop(g);
        s.guards = self.table().guards(Tracked(ord));
        proof {
            let k = class_table()[self.spec_class() as int].nslots as int;
            assert((s.committed as int) * k <= (self.spec_nslabs() as int) * k) by (nonlinear_arith)
                requires s.committed <= self.spec_nslabs(), k >= 0;
            assert(((s.committed - s.empty - s.purged) as int) * k <= (s.committed as int) * k) by (nonlinear_arith)
                requires s.empty + s.purged <= s.committed, k >= 0;
        }
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
        self.bin_stats(Tracked::assume_new()).1
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
    /// compaction gate in `heap/` reads this; one lock hold. `ord` is the
    /// caller's lock-order token (ghost) for the class lock.
    pub fn holes(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (usize, usize))
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
            r.0 <= r.1,
            (r.1 as int) <= (self.spec_nslabs() as int) * (class_table()[self.spec_class() as int].nslots as int),
            // Room for `holes * COMPACT_MIN_RATIO` (32) in `heap::compact`.
            r.1 <= usize::MAX / 32,
    {
        proof {
            use_type_invariant(self);
            lemma_slots_fit(table_at(self.table));
        }
        let (s, curregs) = self.bin_stats(Tracked(ord));
        proof {
            let k = class_table()[self.spec_class() as int].nslots as int;
            assert(((s.committed - s.empty - s.purged) as int) * k <= (s.committed as int) * k) by (nonlinear_arith)
                requires s.empty + s.purged <= s.committed, k >= 0;
            assert((s.committed as int) * k <= (self.spec_nslabs() as int) * k) by (nonlinear_arith)
                requires s.committed <= self.spec_nslabs(), k >= 0;
        }
        let capacity = s.curslabs() * self.class().nslots();
        (capacity.saturating_sub(curregs), capacity)
    }
}

} // verus!

impl ClassState {
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
}

verus! {

impl ClassState {
    /// Lowest slab in the partial set, if any (for tests and stats).
    /// `ord` is the caller's lock-order token (ghost) for the class lock.
    pub fn lowest_partial(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<usize>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            r matches Some(s) ==> s < self.spec_nslabs(),
            *final(ord) == *old(ord),
    {
        broadcast use {axiom_class_lock, axiom_class_lock_inv};
        proof {
            use_type_invariant(self);
        }
        self.inner
            .lock(Tracked(ord), Tracked(&mut LockOrder::spare()))
            .partial
            .map
            .lowest_set()
    }

    /// Whether `slab` is in the partial set (tests and the bins table,
    /// `api::print`). `ord` is the caller's lock-order token (ghost) for the
    /// class lock.
    pub fn is_partial(&self, slab: usize, Tracked(ord): Tracked<&mut LockOrder>) -> bool
        requires
            crate::sys::may_syscall(),
            slab < self.spec_nslabs(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        broadcast use {axiom_class_lock, axiom_class_lock_inv};
        proof {
            use_type_invariant(self);
        }
        self.inner
            .lock(Tracked(ord), Tracked(&mut LockOrder::spare()))
            .partial
            .map
            .is_set(slab)
    }
}

} // verus!
