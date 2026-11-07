//! Huge allocations (design 7).
//!
//! Requests above the largest medium class are served from the huge region
//! with page granularity. Every page below the high-water mark `next_page`
//! belongs to exactly one *extent*, described by a [`HugeDesc`]:
//!
//! - an `Active` run: the pages handed to the caller followed by
//!   [`GUARD_PAGES`] guard pages, or
//! - a free extent, `Empty` (dirty, still resident) or `Purged` (discarded,
//!   reads as zero), which is what a freed run becomes.
//!
//! Adjacent free extents are always coalesced, whatever their states (the
//! merge is `Empty` unless both were `Purged`), so a maximal run of free
//! pages is exactly one extent. Free pages are also mirrored in a [`Set`]
//! (a `TwoLevelBitmap` over pages) so that allocation is lowest-address
//! first fit: walk the free extents in address order, take the first that
//! fits (or the last one if it touches the high-water mark and can grow),
//! else carve fresh pages at the mark. Descriptors live in an open-addressing hash table
//! keyed by page number: a run is keyed by its start page, a free extent by
//! its first and last page so that a free can find both neighbours.
//!
//! Guards. In `guard_install` mode the guard page is committed like its
//! neighbours and gets `MADV_GUARD_INSTALL`, so runs stay in one VMA; the
//! guard is removed when the run is freed so free extents never contain
//! one. Otherwise (the fallback mode, kernels before 6.13) the guard page
//! is made `PROT_NONE`, which splits the VMA, and made read-write again
//! when the run is freed, so the free extent merges back into one VMA and
//! the split lasts only as long as the run; if a release ever fails,
//! `mixed_guards` makes every later allocation commit its own range, so
//! that guard is undone on reuse.
//! Under `guards:false` no guard is placed in either mode: the guard page
//! is committed with the run.
//!
//! Alignment above a page is met in `find`: the start of each free extent
//! (and finally the high-water mark) is rounded up to the alignment and the
//! extent is taken when the whole run still fits below its end, or when
//! the extent touches the mark and can grow; `take` leaves the pages
//! before the aligned start and any left after the run as free extents.
//! Runs whose usable size exceeds `retain_max` (default 64 MiB) are
//! `MADV_DONTNEED`ed the moment they are freed (every run under
//! `huge_dirty_decay_ms:0`, [`Huge::set_purge_on_free`]); smaller ones
//! become `Empty` and wait for the decay walk ([`Huge::decay_with`], on arena
//! 0's period or the huge tier's own `huge_dirty_decay_ms`, design 7) or a
//! full purge ([`Huge::purge_empty`]). `Empty` extents sit in a FIFO list ordered by
//! `dirty_at`, so purging walks it from the head; an extent whose discard
//! fails (`mlock`ed pages) stays in its place, `Empty`, as does a run
//! whose discard at free time fails.
//!
//! Everything is under one futex lock: huge operations are rare compared to
//! slab operations and each already costs at least one syscall.
//!
//! Submodules: [`table`] is the open-addressing descriptor table
//! (`Table`), [`free`] frees a run, [`decay`] holds the purge and decay
//! walks, `tests` the allocator's tests and `linux_tests` the ones that
//! need the process's own regions.

use core::alloc::Layout;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::class::Set;
use crate::layout::Regions;
use crate::lock::{Lock, LockOrder, RawLock};
#[cfg(verus_keep_ghost)]
use crate::lock::{axiom_entry, lock_frame, lock_init, rank_huge, rank_meta};
use crate::meta::Meta;
use crate::purge::{self, Batch, Curve, Decayer, Due, Settle};
use crate::sys::{
    self, Errno, MADV_GUARD_INSTALL, MADV_GUARD_REMOVE, MADV_POPULATE_WRITE, PROT_NONE, PROT_RW,
};
#[cfg(verus_keep_ghost)]
use crate::vspec::slice::array_iter_rest;

mod decay;
mod free;
mod table;

use crate::vspec::zero::Zeroed;
#[cfg(verus_keep_ghost)]
use crate::vspec::zero::{page_aligned, run_need, run_placeable};
use table::Table;
#[cfg(verus_keep_ghost)]
use vstd::arithmetic::power2::pow2;
use vstd::cell::pcell::PointsTo as LockPerm;
use vstd::prelude::*;
use vstd::raw_ptr::{PointsTo, PointsToRaw, ptr_mut_ref, ptr_mut_write, ptr_ref};
use vstd::resource::set::{GhostSetAuth, GhostSingleton, GhostSubset};

verus! {

/// Guard pages after every run.
pub const GUARD_PAGES: usize = 1;
/// The fewest bytes of a `Purged` extent [`Huge::rediscard`] discards
/// again for a run placed in it; below this the caller's `memset` is
/// cheaper than the `madvise` call (about 1 us; a 4 KiB page's clear,
/// fault included, is about 2 us on x86_64, so the call wins from two
/// pages).
pub const REDISCARD_MIN: usize = 16 << 10;
/// Runs with more usable bytes than this are purged as soon as they are
/// freed (`huge_retain_max`).
pub const DEFAULT_RETAIN_MAX: usize = 64 << 20;

/// State of an extent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RunState {
    /// Handed to the caller (usable pages plus guard).
    Active = 0,
    /// Freed, dirty, resident.
    Empty = 1,
    /// Freed and discarded (or never touched); reads as zero.
    Purged = 2,
}

impl RunState {
    /// Decode a state byte. Aborts on a value no state has: a corrupted
    /// byte must not read as `Purged`, whose pages `take` hands out as
    /// zero memory. Verified callers pass a valid byte, so they never
    /// reach the abort.
    #[inline]
    fn from_u8(v: u8) -> (r: RunState)
        requires
            v <= 2,
        ensures
            r == state_of(v),
    {
        match RunState::try_from_u8(v) {
            Some(s) => s,
            None => bad_run_state_byte(v),
        }
    }

    /// [`from_u8`](RunState::from_u8) without the abort.
    #[inline]
    const fn try_from_u8(v: u8) -> (r: Option<RunState>)
        ensures
            r == if v <= 2 { Some(state_of(v)) } else { None },
    {
        match v {
            0 => Some(RunState::Active),
            1 => Some(RunState::Empty),
            2 => Some(RunState::Purged),
            _ => None,
        }
    }

    #[inline]
    pub const fn is_free(self) -> (r: bool)
        ensures
            r == (self != RunState::Active),
    {
        !matches!(self, RunState::Active)
    }
}

/// The state a valid state byte encodes.
pub open spec fn state_of(v: u8) -> RunState {
    if v == 0 {
        RunState::Active
    } else if v == 1 {
        RunState::Empty
    } else {
        RunState::Purged
    }
}

/// `state_of` undoes the cast of a state to its byte.
proof fn lemma_state_byte(v: u8)
    requires
        v <= 2,
    ensures
        state_of(v) as u8 == v,
{
}

/// Descriptor of one extent (design 7's `HugeDesc`). Allocated from
/// the metadata region; free extents also carry the FIFO links of the
/// `Empty` list.
#[repr(C)]
pub struct HugeDesc {
    start: u32,
    pages: u32,
    arena: u16,
    state: u8,
    _pad: u8,
    /// Bytes the caller may use; 0 for a free extent.
    usable: usize,
    /// Coarse monotonic time the extent became `Empty`; 0 otherwise.
    dirty_at: u64,
    prev: *mut HugeDesc,
    next: *mut HugeDesc,
}

// Checked by the compiler; `desc_in_meta` reads the size.
#[cfg_attr(not(verus_keep_ghost), allow(unused_braces))]
global layout HugeDesc is size == 48, align == 8;

impl HugeDesc {
    pub closed spec fn spec_start(&self) -> int {
        self.start as int
    }

    pub closed spec fn spec_pages(&self) -> int {
        self.pages as int
    }

    pub closed spec fn spec_arena(&self) -> u16 {
        self.arena
    }

    /// The state byte.
    pub closed spec fn spec_state(&self) -> u8 {
        self.state
    }

    #[inline]
    pub fn start(&self) -> (r: usize)
        ensures
            r == self.spec_start(),
    {
        self.start as usize
    }

    #[inline]
    pub fn pages(&self) -> (r: usize)
        ensures
            r == self.spec_pages(),
    {
        self.pages as usize
    }

    /// One past the last page.
    #[inline]
    pub fn end(&self) -> (r: usize)
        ensures
            r == self.spec_start() + self.spec_pages(),
    {
        self.start() + self.pages()
    }

    #[inline]
    pub fn arena(&self) -> (r: u16)
        ensures
            r == self.spec_arena(),
    {
        self.arena
    }

    /// Aborts unless the state byte is valid ([`RunState::from_u8`]).
    #[inline]
    pub fn state(&self) -> (r: RunState)
        requires
            self.spec_state() <= 2,
        ensures
            r == state_of(self.spec_state()),
    {
        RunState::from_u8(self.state)
    }
}

} // verus!

impl HugeDesc {
    #[cfg(test)]
    const LAYOUT: Layout = Layout::new::<HugeDesc>();
}

verus! {

/// What [`Huge::alloc_run`] placed: the run's start, whether it reads as
/// zero, and its pages with the guard, read from the descriptor it wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    /// Null when nothing was placed.
    pub ptr: *mut u8,
    pub zeroed: bool,
    /// The run's pages, guard included (0 when nothing was placed).
    pub pages: usize,
}

impl Placed {
    /// Nothing placed.
    pub exec const NONE: Placed
        ensures
            Self::NONE.pages == 0,
            Self::NONE.ptr@.addr == 0,
    {
        Placed { ptr: ptr::null_mut(), zeroed: false, pages: 0 }
    }
}

/// A copy of a descriptor for callers outside the lock (tests, stats).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunInfo {
    pub start: usize,
    pub pages: usize,
    pub usable: usize,
    pub arena: u16,
    pub state: RunState,
    pub dirty_at: u64,
}

/// Counters kept under the huge lock. Identity:
/// `run_pages + empty_pages + purged_pages == next_page`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct HugeStats {
    /// Live runs.
    pub runs: usize,
    /// Pages in live runs, guards included.
    pub run_pages: usize,
    /// `Empty` extents.
    pub empty: usize,
    pub empty_pages: usize,
    /// `Purged` extents.
    pub purged: usize,
    pub purged_pages: usize,
    /// High-water mark: pages ever handed out.
    pub next_page: usize,
    /// Descriptor table capacity, for diagnostics; filled in by
    /// [`Huge::stats`] from the table, not tracked.
    pub table_cap: usize,
}

impl HugeStats {
    /// Room to count one more extent of `pages` in `state`.
    pub open spec fn can_add(&self, state: RunState, pages: usize) -> bool {
        match state {
            RunState::Active => self.runs < usize::MAX && self.run_pages + pages <= usize::MAX,
            RunState::Empty => self.empty < usize::MAX && self.empty_pages + pages <= usize::MAX,
            RunState::Purged => self.purged < usize::MAX && self.purged_pages + pages <= usize::MAX,
        }
    }

    /// Count one extent of `pages` in `state`.
    fn add(&mut self, state: RunState, pages: usize)
        requires
            old(self).can_add(state, pages),
        ensures
            *final(self) == match state {
                RunState::Active => HugeStats {
                    runs: (old(self).runs + 1) as usize,
                    run_pages: (old(self).run_pages + pages) as usize,
                    ..*old(self)
                },
                RunState::Empty => HugeStats {
                    empty: (old(self).empty + 1) as usize,
                    empty_pages: (old(self).empty_pages + pages) as usize,
                    ..*old(self)
                },
                RunState::Purged => HugeStats {
                    purged: (old(self).purged + 1) as usize,
                    purged_pages: (old(self).purged_pages + pages) as usize,
                    ..*old(self)
                },
            },
    {
        match state {
            RunState::Active => {
                self.runs += 1;
                self.run_pages += pages;
            }
            RunState::Empty => {
                self.empty += 1;
                self.empty_pages += pages;
            }
            RunState::Purged => {
                self.purged += 1;
                self.purged_pages += pages;
            }
        }
    }

    /// An extent of `pages` in `state` is there to uncount.
    pub open spec fn can_sub(&self, state: RunState, pages: usize) -> bool {
        match state {
            RunState::Active => self.runs >= 1 && self.run_pages >= pages,
            RunState::Empty => self.empty >= 1 && self.empty_pages >= pages,
            RunState::Purged => self.purged >= 1 && self.purged_pages >= pages,
        }
    }

    /// Uncount one extent of `pages` in `state`. Verified callers have the
    /// extent counted, so they never reach the underflow abort.
    fn sub(&mut self, state: RunState, pages: usize)
        requires
            old(self).can_sub(state, pages),
        ensures
            *final(self) == match state {
                RunState::Active => HugeStats {
                    runs: (old(self).runs - 1) as usize,
                    run_pages: (old(self).run_pages - pages) as usize,
                    ..*old(self)
                },
                RunState::Empty => HugeStats {
                    empty: (old(self).empty - 1) as usize,
                    empty_pages: (old(self).empty_pages - pages) as usize,
                    ..*old(self)
                },
                RunState::Purged => HugeStats {
                    purged: (old(self).purged - 1) as usize,
                    purged_pages: (old(self).purged_pages - pages) as usize,
                    ..*old(self)
                },
            },
    {
        // A count going below zero means the extents and the counters
        // disagree; the release profile does not check overflow, so the
        // subtractions are explicit.
        let sub = |x: usize, y: usize| -> (r: usize)
            requires
                x >= y,
            ensures
                r == x - y,
            { x.checked_sub(y).unwrap_or_else(|| -> (r: usize) requires false { huge_count_underflow(state) }) };
        match state {
            RunState::Active => {
                self.runs = sub(self.runs, 1);
                self.run_pages = sub(self.run_pages, pages);
            }
            RunState::Empty => {
                self.empty = sub(self.empty, 1);
                self.empty_pages = sub(self.empty_pages, pages);
            }
            RunState::Purged => {
                self.purged = sub(self.purged, 1);
                self.purged_pages = sub(self.purged_pages, pages);
            }
        }
    }
}

// The two aborts below are unreachable from verified code: their callers'
// preconditions rule out the bad byte and the underflow.
#[cold]
#[inline(never)]
fn bad_run_state_byte(v: u8) -> !
    requires
        false,
{
    panic!("huge: run state byte {v:#x} is not a run state")
}

#[cold]
#[inline(never)]
fn huge_count_underflow(state: RunState) -> !
    requires
        false,
{
    sys::abort_pending(format_args!("huge: {state:?} extent count underflow"))
}

} // verus!

verus! {

pub(crate) struct Inner {
    /// Free pages below `next_page`.
    free: Set,
    table: Table,
    /// High-water mark: pages at or above it were never handed out.
    next_page: usize,
    /// `Empty` extents, oldest `dirty_at` first.
    empty_head: *mut HugeDesc,
    empty_tail: *mut HugeDesc,
    stats: HugeStats,
    /// Ghost: the permission to every live descriptor.
    #[allow(dead_code)]
    descs: Tracked<Map<*mut HugeDesc, PointsTo<HugeDesc>>>,
    /// Ghost: the [`MetaClaim`](crate::meta::MetaClaim) of every live
    /// descriptor's block, for [`free_desc`].
    #[allow(dead_code)]
    dclaims: Tracked<Map<*mut HugeDesc, crate::meta::MetaClaim>>,
    /// Ghost: the live-object records' identity of the `Meta` the
    /// descriptors and the table come from (`Huge::new`'s), fixed at
    /// creation.
    #[allow(dead_code)]
    mid: Ghost<vstd::resource::Loc>,
    /// Ghost: the `Empty` list, head first.
    #[allow(dead_code)]
    elist: Ghost<Seq<*mut HugeDesc>>,
    /// Ghost: every live descriptor (the keys of `descs`), in no order.
    #[allow(dead_code)]
    all: Ghost<Seq<*mut HugeDesc>>,
    /// Ghost: the region's page count (`Huge::npages`), fixed at creation.
    #[allow(dead_code)]
    np: Ghost<usize>,
    /// Ghost: the region's page shift (`Huge::lg_page`), fixed at creation.
    #[allow(dead_code)]
    lg: Ghost<u32>,
    /// Ghost: the region's first byte (`Huge::base`), fixed at creation.
    #[allow(dead_code)]
    base: Ghost<usize>,
    /// Ghost: the authority of the `Active` descriptors' extents, fixed
    /// at creation (`spec_rid`); the process's ([`run_loc`]) in the
    /// process's allocator.
    #[allow(dead_code)]
    runs: Tracked<GhostSetAuth<(int, int)>>,
    /// Ghost: the authority of the `Active` descriptors' pages, fixed at
    /// creation (`spec_pid`); the process's ([`page_loc`]) in the
    /// process's allocator.
    #[allow(dead_code)]
    pgs: Tracked<GhostSetAuth<int>>,
}

/// Pages of the descriptors in `l`.
spec fn epages(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>) -> int
    decreases l.len(),
{
    if l.len() == 0 {
        0
    } else {
        descs[l[0]].value().pages + epages(l.drop_first(), descs)
    }
}

/// `epages` of a suffix: the first entry plus the rest.
proof fn lemma_epages_step(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, k: int)
    requires
        0 <= k < l.len(),
    ensures
        epages(l.subrange(k, l.len() as int), descs) == descs[l[k]].value().pages + epages(
            l.subrange(k + 1, l.len() as int),
            descs,
        ),
{
    assert(l.subrange(k, l.len() as int).drop_first() =~= l.subrange(k + 1, l.len() as int));
}

/// Removing entry `j` removes its pages.
proof fn lemma_epages_remove(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, j: int)
    requires
        0 <= j < l.len(),
    ensures
        epages(l.remove(j), descs) == epages(l, descs) - descs[l[j]].value().pages,
    decreases l.len(),
{
    if j > 0 {
        lemma_epages_remove(l.drop_first(), descs, j - 1);
        assert(l.remove(j).drop_first() =~= l.drop_first().remove(j - 1));
    } else {
        assert(l.remove(0) =~= l.drop_first());
    }
}

/// `epages` depends only on the pages of the entries.
proof fn lemma_epages_frame(
    l: Seq<*mut HugeDesc>,
    d1: Map<*mut HugeDesc, PointsTo<HugeDesc>>,
    d2: Map<*mut HugeDesc, PointsTo<HugeDesc>>,
)
    requires
        forall|i: int| 0 <= i < l.len() ==> #[trigger] d2[l[i]].value().pages == d1[l[i]].value().pages,
    ensures
        epages(l, d2) == epages(l, d1),
    decreases l.len(),
{
    if l.len() > 0 {
        assert forall|i: int| 0 <= i < l.drop_first().len() implies #[trigger] d2[l.drop_first()[i]].value().pages
            == d1[l.drop_first()[i]].value().pages by {
            assert(l.drop_first()[i] == l[i + 1]);
        }
        lemma_epages_frame(l.drop_first(), d1, d2);
    }
}

/// Pages are never negative.
proof fn lemma_epages_nonneg(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>)
    ensures
        epages(l, descs) >= 0,
    decreases l.len(),
{
    if l.len() > 0 {
        lemma_epages_nonneg(l.drop_first(), descs);
    }
}

/// Every entry has a page, so there are at least as many pages as entries.
proof fn lemma_epages_len(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>)
    requires
        forall|i: int| 0 <= i < l.len() ==> #[trigger] descs[l[i]].value().pages >= 1,
    ensures
        epages(l, descs) >= l.len(),
    decreases l.len(),
{
    if l.len() > 0 {
        assert forall|i: int| 0 <= i < l.drop_first().len() implies #[trigger] descs[l.drop_first()[i]].value().pages
            >= 1 by {
            assert(l.drop_first()[i] == l[i + 1]);
        }
        lemma_epages_len(l.drop_first(), descs);
    }
}

/// `epages` of `l.insert(i, x)`.
proof fn lemma_epages_insert(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, i: int, x: *mut HugeDesc)
    requires
        0 <= i <= l.len(),
    ensures
        epages(l.insert(i, x), descs) == epages(l, descs) + descs[x].value().pages,
    decreases l.len(),
{
    if i == 0 {
        assert(l.insert(0, x).drop_first() =~= l);
    } else {
        lemma_epages_insert(l.drop_first(), descs, i - 1, x);
        assert(l.insert(i, x).drop_first() =~= l.drop_first().insert(i - 1, x));
    }
}

/// One entry's pages changed: `epages` changes by the difference.
proof fn lemma_epages_update(
    l: Seq<*mut HugeDesc>,
    d1: Map<*mut HugeDesc, PointsTo<HugeDesc>>,
    d2: Map<*mut HugeDesc, PointsTo<HugeDesc>>,
    x: *mut HugeDesc,
)
    requires
        l.no_duplicates(),
        l.contains(x),
        forall|i: int| 0 <= i < l.len() && l[i] != x ==> #[trigger] d2[l[i]].value().pages == d1[l[i]].value().pages,
    ensures
        epages(l, d2) == epages(l, d1) - d1[x].value().pages + d2[x].value().pages,
{
    let j = l.index_of(x);
    let r = l.remove(j);
    assert forall|i: int| 0 <= i < r.len() implies #[trigger] d2[r[i]].value().pages == d1[r[i]].value().pages by {
        if i < j {
            assert(r[i] == l[i]);
            assert(l[i] != x) by {
                assert(l.no_duplicates());
            }
        } else {
            assert(r[i] == l[i + 1]);
            assert(l[i + 1] != x) by {
                assert(l.no_duplicates());
            }
        }
    }
    lemma_epages_frame(r, d1, d2);
    lemma_epages_remove(l, d1, j);
    lemma_epages_remove(l, d2, j);
}

/// A descriptor's fields make sense below the high-water mark: a valid
/// state, at least one page, and no page at or past `next_page`.
spec fn desc_ok(v: HugeDesc, next_page: usize) -> bool {
    &&& v.state <= 2
    &&& v.pages >= 1
    &&& v.start + v.pages <= next_page
}

/// Every live descriptor's state byte is a run state ([`desc_ok`]'s first
/// clause), so [`HugeDesc::state`] aborts on none of them.
spec fn states_ok(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>) -> bool {
    forall|x: *mut HugeDesc| #[trigger] descs.contains_key(x) ==> descs[x].value().state <= 2
}

/// An `Active` descriptor is a run: its pages are the usable pages and
/// the guard after them, and `usable` is the usable pages' bytes, for
/// pages of `2^lg` bytes ([`Huge::take`] writes it so).
spec fn run_ok(v: HugeDesc, lg: u32) -> bool {
    v.state == RunState::Active as u8 ==> v.pages > GUARD_PAGES && v.usable == (v.pages - GUARD_PAGES) * pow2(lg as nat)
}

/// Every permission in `descs` is for its key, initialised and non-null,
/// and the descriptor lies in the metadata region ([`alloc_desc`]).
spec fn perms_ok(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>) -> bool {
    forall|d: *mut HugeDesc| #[trigger] descs.contains_key(d) ==> {
        &&& descs[d].ptr() == d
        &&& descs[d].is_init()
        &&& d@.addr != 0
        &&& desc_in_meta(d)
    }
}

/// `claims` holds the claim of the descriptor block at `d`
/// ([`desc_claim_for`]), from the `Meta` whose live-object records are
/// `id`.
spec fn desc_claimed(claims: Map<*mut HugeDesc, crate::meta::MetaClaim>, d: *mut HugeDesc, id: vstd::resource::Loc) -> bool {
    &&& claims.contains_key(d)
    &&& desc_claim_for(claims[d], d, id)
}

/// `c` is the claim of the descriptor block at `d`, from the `Meta`
/// whose live-object records are `id`; `Meta::free` takes it back with
/// the block ([`free_desc`]).
spec fn desc_claim_for(c: crate::meta::MetaClaim, d: *mut HugeDesc, id: vstd::resource::Loc) -> bool {
    c.held(id, d as *mut u8, vstd::layout::size_of::<HugeDesc>() as int, vstd::layout::align_of::<HugeDesc>() as int)
}

/// The 48 bytes of the descriptor at `d` are metadata
/// ([`Regions::in_meta`]).
pub open spec fn desc_in_meta(d: *mut HugeDesc) -> bool {
    crate::layout::process_regions().in_meta(d@.addr as int, vstd::layout::size_of::<HugeDesc>() as int)
}

/// The authority of the live runs' extents: one per
/// process, that of the huge allocator of the `Shared` in the process's
/// storage ([`Huge::spec_run_loc`]). Opaque; `Shared::init` reveals it.
#[verifier::opaque]
pub open(crate) spec fn run_loc() -> vstd::resource::Loc {
    crate::heap::Shared::built_at(crate::heap::Shared::storage()).huge.spec_run_loc()
}

/// The authority of the live runs' pages ([`Huge::spec_page_loc`] of the
/// stored `Shared`'s huge allocator). Opaque, as [`run_loc`].
#[verifier::opaque]
pub open(crate) spec fn page_loc() -> vstd::resource::Loc {
    crate::heap::Shared::built_at(crate::heap::Shared::storage()).huge.spec_page_loc()
}

/// Ghost token of a live huge run: its extent `(start, npages)`, the
/// run's first page and its page count with the guard, as a singleton of
/// the extents' authority ([`run_loc`]), and its pages as a subset of the
/// pages' authority ([`page_loc`]). [`Huge::alloc_run`] hands one out with
/// every run and [`Huge::free_run`] takes it back.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct RunToken {
    tracked ext: GhostSingleton<(int, int)>,
    tracked pages: GhostSubset<int>,
}

impl RunToken {
    /// The run's first page (an index into the region).
    pub closed spec fn start(self) -> int {
        self.ext@.0
    }

    /// The run's pages, its guard included.
    pub closed spec fn npages(self) -> int {
        self.ext@.1
    }

    /// The token is a run's: of the process's authorities, its pages
    /// `start..start + npages`, more than the guard.
    pub closed spec fn wf(self) -> bool {
        &&& self.ext.id() == run_loc()
        &&& self.pages.id() == page_loc()
        &&& self.pages@ == vstd::set_lib::set_int_range(self.start(), self.start() + self.npages())
        &&& 0 <= self.start()
        &&& GUARD_PAGES < self.npages()
    }

    /// A run token's extent: from a page of the region on, more than the
    /// guard.
    pub proof fn lemma_wf(&self)
        requires
            self.wf(),
        ensures
            0 <= self.start(),
            GUARD_PAGES < self.npages(),
    {
    }

    /// Two run tokens hold disjoint page ranges.
    pub proof fn lemma_apart(tracked &mut self, tracked other: &RunToken)
        requires
            old(self).wf(),
            other.wf(),
        ensures
            final(self).wf(),
            final(self).start() == old(self).start(),
            final(self).npages() == old(self).npages(),
            final(self).start() + final(self).npages() <= other.start() || other.start() + other.npages() <= final(self).start(),
    {
        self.pages.disjoint(&other.pages);
        let lo = if self.start() < other.start() { other.start() } else { self.start() };
        if lo < self.start() + self.npages() && lo < other.start() + other.npages() {
            assert(self.pages@.contains(lo) && other.pages@.contains(lo));
        }
    }

    /// Purge never discards live data, for huge runs: a byte
    /// [`free_page_byte`] admits for the set of the page authority `auth`
    /// is in no page, guard included, of the run whose token this is,
    /// whoever holds it. Every huge purge's [`Batch`] (`Huge::purge_empty`,
    /// `purge_curve`, `free_run`) may discard only such bytes, for the set
    /// of the authority the huge lock's value holds from the batch's making
    /// to its flush (`Inner::wf`).
    pub proof fn theorem_purge_apart(
        tracked &self,
        tracked auth: &GhostSetAuth<int>,
        base: int,
        pg: int,
        np: int,
        b: int,
    )
        requires
            self.wf(),
            auth.id() == page_loc(),
            pg > 0,
            free_page_byte(base, pg, np, auth@, b),
        ensures
            !(base + self.start() * pg <= b < base + (self.start() + self.npages()) * pg),
    {
        self.pages.agree(auth);
        if base + self.start() * pg <= b < base + (self.start() + self.npages()) * pg {
            crate::purge::lemma_unit_of(base, pg, self.start(), self.npages(), b);
            assert(self.pages@.contains((b - base) / pg));
        }
    }
}

/// `tok` is the token of a live run starting at page `page`.
pub open spec fn holds_run(tok: Option<RunToken>, page: int) -> bool {
    &&& tok matches Some(t)
    &&& t.wf()
    &&& t.start() == page
}

/// Byte `b` lies in one of the `np` pages of `pg` bytes of the region at
/// `base`, and that page is not in `held`, the pages the run authority
/// holds ([`page_loc`]): a page of no live run. What a huge purge's
/// [`Batch`] may discard ([`RunToken::theorem_purge_apart`]).
pub open spec fn free_page_byte(base: int, pg: int, np: int, held: vstd::set::Set<int>, b: int) -> bool {
    &&& base <= b
    &&& (b - base) / pg < np
    &&& !held.contains((b - base) / pg)
}

/// Two live descriptors are at different addresses: their permissions
/// are disjoint.
proof fn lemma_addr_apart(tracked m: &mut Map<*mut HugeDesc, PointsTo<HugeDesc>>, x: *mut HugeDesc, y: *mut HugeDesc)
    requires
        perms_ok(*old(m)),
        old(m).contains_key(x),
        old(m).contains_key(y),
        x != y,
    ensures
        *final(m) == *old(m),
        x@.addr != y@.addr,
{
    let tracked mut px = m.tracked_remove(x);
    px.is_disjoint(m.tracked_borrow(y));
    m.tracked_insert(x, px);
    assert(*m =~= *old(m));
}

/// Every live descriptor in `others` other than `x` is at another address
/// than `x`.
proof fn lemma_addrs_apart(tracked m: &mut Map<*mut HugeDesc, PointsTo<HugeDesc>>, x: *mut HugeDesc, others: Seq<*mut HugeDesc>)
    requires
        perms_ok(*old(m)),
        old(m).contains_key(x),
    ensures
        *final(m) == *old(m),
        forall|i: int| #![trigger others[i]] 0 <= i < others.len() && old(m).contains_key(others[i]) && others[i] != x ==> others[i]@.addr
            != x@.addr,
    decreases others.len(),
{
    if others.len() > 0 {
        let y = others.last();
        lemma_addrs_apart(m, x, others.drop_last());
        if m.contains_key(y) && y != x {
            lemma_addr_apart(m, x, y);
        }
        assert forall|i: int| #![trigger others[i]] 0 <= i < others.len() && old(m).contains_key(others[i]) && others[i] != x implies others[i]@.addr
            != x@.addr by {
            if i < others.len() - 1 {
                assert(others.drop_last()[i] == others[i]);
            }
        }
    }
}

/// Entry `i` of the `Empty` list: a live `Empty` descriptor linked to its
/// neighbours.
spec fn enode(elist: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, i: int) -> bool {
    let d = elist[i];
    &&& descs.contains_key(d)
    &&& descs[d].value().state == RunState::Empty as u8
    &&& descs[d].value().prev == (if i > 0 { elist[i - 1] } else { ptr::null_mut::<HugeDesc>() })
    &&& descs[d].value().next == (if i + 1 < elist.len() { elist[i + 1] } else { ptr::null_mut::<HugeDesc>() })
}

/// The `Empty` list runs from `head` to `tail` through `elist`.
spec fn elist_ok(
    head: *mut HugeDesc,
    tail: *mut HugeDesc,
    elist: Seq<*mut HugeDesc>,
    descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>,
) -> bool {
    &&& elist.len() == 0 ==> head == ptr::null_mut::<HugeDesc>() && tail == ptr::null_mut::<HugeDesc>()
    &&& elist.len() > 0 ==> head == elist[0] && tail == elist[elist.len() - 1]
    &&& forall|i: int| 0 <= i < elist.len() ==> #[trigger] enode(elist, descs, i)
    &&& elist.no_duplicates()
}

/// `a` and `b` are the same descriptor apart from the list links.
spec fn same_but_links(a: HugeDesc, b: HugeDesc) -> bool {
    &&& a.start == b.start
    &&& a.pages == b.pages
    &&& a.arena == b.arena
    &&& a.state == b.state
    &&& a.usable == b.usable
    &&& a.dirty_at == b.dirty_at
}

/// `descs2` has the permissions of `descs1`, and the descriptors differ
/// at most in their list links.
spec fn links_only(descs1: Map<*mut HugeDesc, PointsTo<HugeDesc>>, descs2: Map<*mut HugeDesc, PointsTo<HugeDesc>>) -> bool {
    &&& descs2.dom() == descs1.dom()
    &&& perms_ok(descs2)
    &&& forall|x: *mut HugeDesc| #[trigger] descs1.contains_key(x) ==> same_but_links(descs1[x].value(), descs2[x].value())
}

/// The table keys of an extent: its first page, and its last page too
/// when it is free and longer than a page (so a free finds both
/// neighbours).
spec fn is_key(v: HugeDesc, k: int) -> bool {
    k == v.start || (v.state != RunState::Active as u8 && v.pages > 1 && k == v.start + v.pages - 1)
}

/// The table maps every key of `v` to `d`.
spec fn keyed(t: Table, v: HugeDesc, d: *mut HugeDesc) -> bool {
    &&& t.lookup(v.start as usize) == Some(d)
    &&& (v.state != RunState::Active as u8 && v.pages > 1) ==> t.lookup((v.start + v.pages - 1) as usize) == Some(d)
}

/// Two extents share no page.
spec fn apart(a: HugeDesc, b: HugeDesc) -> bool {
    a.start + a.pages <= b.start || b.start + b.pages <= a.start
}

/// `v` is the `Active` run with extent `e`, `(start, pages)`.
spec fn run_at(v: HugeDesc, e: (int, int)) -> bool {
    &&& v.state == RunState::Active as u8
    &&& v.start == e.0
    &&& v.pages == e.1
}

/// `v` is an `Active` run with page `k`.
spec fn run_page(v: HugeDesc, k: int) -> bool {
    &&& v.state == RunState::Active as u8
    &&& v.start <= k < v.start + v.pages
}

/// The extents of the `Active` descriptors in `descs`: what the extents'
/// authority holds ([`Inner::wf`]).
spec fn active_runs(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>) -> vstd::set::Set<(int, int)> {
    descs.dom().filter(|d: *mut HugeDesc| descs[d].value().state == RunState::Active as u8).map(
        |d: *mut HugeDesc| (descs[d].value().start as int, descs[d].value().pages as int),
    )
}

/// The pages of the `Active` descriptors in `descs`: what the pages'
/// authority holds ([`Inner::wf`]). A descriptor's pages are below `2^33`.
spec fn active_pages(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>) -> vstd::set::Set<int> {
    vstd::set_lib::set_int_range(0, 0x2_0000_0000).filter(
        |k: int| exists|d: *mut HugeDesc| #[trigger] descs.contains_key(d) && run_page(descs[d].value(), k),
    )
}

/// What [`active_runs`] and [`active_pages`] contain.
proof fn lemma_active(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>)
    ensures
        forall|e: (int, int)| #[trigger] active_runs(descs).contains(e)
            <==> exists|d: *mut HugeDesc| #[trigger] descs.contains_key(d) && run_at(descs[d].value(), e),
        forall|k: int| #[trigger] active_pages(descs).contains(k)
            <==> exists|d: *mut HugeDesc| #[trigger] descs.contains_key(d) && run_page(descs[d].value(), k),
{
    broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter, vstd::set::Set::lemma_map_contains};
    let act = descs.dom().filter(|d: *mut HugeDesc| descs[d].value().state == RunState::Active as u8);
    assert forall|e: (int, int)| #[trigger] active_runs(descs).contains(e)
        <==> exists|d: *mut HugeDesc| #[trigger] descs.contains_key(d) && run_at(descs[d].value(), e) by {
        if active_runs(descs).contains(e) {
            let d = choose|d: *mut HugeDesc| act.contains(d) && e == (descs[d].value().start as int, descs[d].value().pages as int);
            assert(descs.contains_key(d) && run_at(descs[d].value(), e));
        }
        if exists|d: *mut HugeDesc| #[trigger] descs.contains_key(d) && run_at(descs[d].value(), e) {
            let d = choose|d: *mut HugeDesc| #[trigger] descs.contains_key(d) && run_at(descs[d].value(), e);
            assert(act.contains(d));
        }
    }
    assert forall|k: int| #[trigger] active_pages(descs).contains(k)
        <==> exists|d: *mut HugeDesc| #[trigger] descs.contains_key(d) && run_page(descs[d].value(), k) by {
        if exists|d: *mut HugeDesc| #[trigger] descs.contains_key(d) && run_page(descs[d].value(), k) {
            let d = choose|d: *mut HugeDesc| #[trigger] descs.contains_key(d) && run_page(descs[d].value(), k);
            let v = descs[d].value();
            assert(v.start + v.pages <= 0x2_0000_0000);
        }
    }
}

/// The `Active` descriptors of `d1` and `d2` are the same, with the same
/// extents: their run authorities' sets are equal.
proof fn lemma_active_frame(d1: Map<*mut HugeDesc, PointsTo<HugeDesc>>, d2: Map<*mut HugeDesc, PointsTo<HugeDesc>>)
    requires
        forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && d1[x].value().state == RunState::Active as u8 ==> {
            &&& d2.contains_key(x)
            &&& d2[x].value().state == RunState::Active as u8
            &&& d2[x].value().start == d1[x].value().start
            &&& d2[x].value().pages == d1[x].value().pages
        },
        forall|x: *mut HugeDesc| #[trigger] d2.contains_key(x) && d2[x].value().state == RunState::Active as u8 ==> {
            &&& d1.contains_key(x)
            &&& d1[x].value().state == RunState::Active as u8
        },
    ensures
        active_runs(d1) == active_runs(d2),
        active_pages(d1) == active_pages(d2),
{
    lemma_active(d1);
    lemma_active(d2);
    assert forall|e: (int, int)| active_runs(d1).contains(e) <==> active_runs(d2).contains(e) by {
        if active_runs(d1).contains(e) {
            let x = choose|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && run_at(d1[x].value(), e);
            assert(d2.contains_key(x) && run_at(d2[x].value(), e));
        }
        if active_runs(d2).contains(e) {
            let x = choose|x: *mut HugeDesc| #[trigger] d2.contains_key(x) && run_at(d2[x].value(), e);
            assert(d1.contains_key(x) && run_at(d1[x].value(), e));
        }
    }
    assert forall|k: int| active_pages(d1).contains(k) <==> active_pages(d2).contains(k) by {
        if active_pages(d1).contains(k) {
            let x = choose|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && run_page(d1[x].value(), k);
            assert(d2.contains_key(x) && run_page(d2[x].value(), k));
        }
        if active_pages(d2).contains(k) {
            let x = choose|x: *mut HugeDesc| #[trigger] d2.contains_key(x) && run_page(d2[x].value(), k);
            assert(d1.contains_key(x) && run_page(d1[x].value(), k));
        }
    }
    assert(active_runs(d1) =~= active_runs(d2));
    assert(active_pages(d1) =~= active_pages(d2));
}

/// No two free extents touch: a free extent never ends where another (or
/// itself) starts, so every free stretch of pages is one extent.
spec fn coalesced(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>) -> bool {
    forall|d1: *mut HugeDesc, d2: *mut HugeDesc|
        #[trigger] descs.contains_key(d1) && #[trigger] descs.contains_key(d2) && descs[d1].value().state
            != RunState::Active as u8 && descs[d2].value().state != RunState::Active as u8 ==> descs[d1].value().start
            + descs[d1].value().pages != descs[d2].value().start
}

/// Every page of `v` is in `f`.
spec fn marked(f: vstd::set::Set<nat>, v: HugeDesc) -> bool {
    forall|q: int| v.start <= q < v.start + v.pages ==> #[trigger] f.contains(q as nat)
}

/// Every page of every free extent is in the free-page set `f`.
spec fn free_marked(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, f: vstd::set::Set<nat>) -> bool {
    forall|d: *mut HugeDesc| #[trigger] descs.contains_key(d) && descs[d].value().state != RunState::Active as u8 ==> marked(
        f,
        descs[d].value(),
    )
}

/// Every page in the free-page set `f` is a page of a free extent: the
/// converse of [`free_marked`], so the set is exactly the free extents'
/// pages (what [`Inner::extent_at`] needs to find an extent at a free bit).
spec fn free_exact(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, f: vstd::set::Set<nat>) -> bool {
    forall|q: nat| #[trigger] f.contains(q) ==> exists|x: *mut HugeDesc| #[trigger] in_free(descs, x, q as int)
}

/// A page of a marked extent is in the set.
proof fn lemma_marked(f: vstd::set::Set<nat>, v: HugeDesc, q: int)
    requires
        marked(f, v),
        v.start <= q < v.start + v.pages,
    ensures
        f.contains(q as nat),
{
}

/// Page `p` lies in `x`, a free extent of `descs`.
spec fn in_free(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, x: *mut HugeDesc, p: int) -> bool {
    &&& descs.contains_key(x)
    &&& descs[x].value().state != RunState::Active as u8
    &&& descs[x].value().start <= p < descs[x].value().start + descs[x].value().pages
}

/// Every free extent starting below `from` ends at or before it.
spec fn ends_by(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, from: int) -> bool {
    forall|x: *mut HugeDesc| #[trigger] descs.contains_key(x) && descs[x].value().state != RunState::Active as u8
        && descs[x].value().start < from ==> descs[x].value().start + descs[x].value().pages <= from
}

/// No free extent in `descs` ends at `page`.
spec fn none_ends_at(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, page: int) -> bool {
    forall|x: *mut HugeDesc| #[trigger] descs.contains_key(x) && descs[x].value().state != RunState::Active as u8
        ==> descs[x].value().start + descs[x].value().pages != page
}

/// No free extent in `descs` starts at `page`.
spec fn none_starts_at(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, page: int) -> bool {
    forall|x: *mut HugeDesc| #[trigger] descs.contains_key(x) && descs[x].value().state != RunState::Active as u8
        ==> descs[x].value().start != page
}

/// Page `p` can hold part of a new run: it is at or past the mark
/// `next_page` (never handed out) or a page of a free extent.
spec fn page_free(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, next_page: int, p: int) -> bool {
    p >= next_page || exists|x: *mut HugeDesc| #[trigger] in_free(descs, x, p)
}

/// `y` in `[x, up)`, with `up` the multiple of `al` that rounds `x` up, is
/// no multiple of `al`.
proof fn lemma_below_align_up(x: int, y: int, up: int, al: int)
    requires
        al > 0,
        x <= y < up,
        up - x < al,
        up % al == 0,
    ensures
        y % al != 0,
{
    if y % al == 0 {
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(y, al);
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(up, al);
        let k = up / al - y / al;
        assert(up - y == al * k) by (nonlinear_arith)
            requires
                y == al * (y / al),
                up == al * (up / al),
                k == up / al - y / al,
        ;
        if k <= 0 {
            assert(al * k <= 0) by (nonlinear_arith)
                requires
                    al > 0,
                    k <= 0,
            ;
        } else {
            assert(al * k >= al) by (nonlinear_arith)
                requires
                    al > 0,
                    k >= 1,
            ;
        }
    }
}

/// What an extent adds to tally `k`: its count (`k` 0 to 2) or its pages
/// (3 to 5) when its state is `k % 3`, and its table keys (6).
spec fn weight(v: HugeDesc, k: int) -> int {
    if k < 3 {
        if v.state as int == k { 1 } else { 0 }
    } else if k < 6 {
        if v.state as int == k - 3 { v.pages as int } else { 0 }
    } else {
        if v.state != RunState::Active as u8 && v.pages > 1 { 2 } else { 1 }
    }
}

/// The sum of `weight(_, k)` over the descriptors in `l`.
spec fn tally(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, k: int) -> int
    decreases l.len(),
{
    if l.len() == 0 {
        0
    } else {
        weight(descs[l[0]].value(), k) + tally(l.drop_first(), descs, k)
    }
}

/// The counter `k` of `s`, numbered as in [`weight`].
spec fn stat(s: HugeStats, k: int) -> int {
    if k == 0 {
        s.runs as int
    } else if k == 1 {
        s.empty as int
    } else if k == 2 {
        s.purged as int
    } else if k == 3 {
        s.run_pages as int
    } else if k == 4 {
        s.empty_pages as int
    } else {
        s.purged_pages as int
    }
}

/// `tally` of `l.remove(j)`.
proof fn lemma_tally_remove(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, j: int, k: int)
    requires
        0 <= j < l.len(),
    ensures
        tally(l.remove(j), descs, k) == tally(l, descs, k) - weight(descs[l[j]].value(), k),
    decreases l.len(),
{
    if j > 0 {
        lemma_tally_remove(l.drop_first(), descs, j - 1, k);
        assert(l.remove(j).drop_first() =~= l.drop_first().remove(j - 1));
    } else {
        assert(l.remove(0) =~= l.drop_first());
    }
}

/// `tally` depends only on the weights of the entries.
proof fn lemma_tally_frame(
    l: Seq<*mut HugeDesc>,
    d1: Map<*mut HugeDesc, PointsTo<HugeDesc>>,
    d2: Map<*mut HugeDesc, PointsTo<HugeDesc>>,
    k: int,
)
    requires
        forall|i: int| 0 <= i < l.len() ==> #[trigger] weight(d2[l[i]].value(), k) == weight(d1[l[i]].value(), k),
    ensures
        tally(l, d2, k) == tally(l, d1, k),
    decreases l.len(),
{
    if l.len() > 0 {
        assert forall|i: int| 0 <= i < l.drop_first().len() implies #[trigger] weight(d2[l.drop_first()[i]].value(), k)
            == weight(d1[l.drop_first()[i]].value(), k) by {
            assert(l.drop_first()[i] == l[i + 1]);
        }
        lemma_tally_frame(l.drop_first(), d1, d2, k);
    }
}

/// One entry's value changed: its weight changes, the others' stay.
proof fn lemma_tally_update(
    l: Seq<*mut HugeDesc>,
    d1: Map<*mut HugeDesc, PointsTo<HugeDesc>>,
    d2: Map<*mut HugeDesc, PointsTo<HugeDesc>>,
    x: *mut HugeDesc,
    k: int,
)
    requires
        l.no_duplicates(),
        l.contains(x),
        forall|i: int| 0 <= i < l.len() && l[i] != x ==> #[trigger] weight(d2[l[i]].value(), k) == weight(d1[l[i]].value(), k),
    ensures
        tally(l, d2, k) == tally(l, d1, k) - weight(d1[x].value(), k) + weight(d2[x].value(), k),
{
    let j = l.index_of(x);
    let r = l.remove(j);
    assert forall|i: int| 0 <= i < r.len() implies #[trigger] weight(d2[r[i]].value(), k) == weight(d1[r[i]].value(), k) by {
        if i < j {
            assert(r[i] == l[i]);
            assert(l[i] != x) by {
                assert(l.no_duplicates());
            }
        } else {
            assert(r[i] == l[i + 1]);
            assert(l[i + 1] != x) by {
                assert(l.no_duplicates());
            }
        }
    }
    lemma_tally_frame(r, d1, d2, k);
    lemma_tally_remove(l, d1, j, k);
    lemma_tally_remove(l, d2, j, k);
}

/// `tally` of `l.push(x)`.
proof fn lemma_tally_push(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, x: *mut HugeDesc, k: int)
    ensures
        tally(l.push(x), descs, k) == tally(l, descs, k) + weight(descs[x].value(), k),
    decreases l.len(),
{
    if l.len() > 0 {
        lemma_tally_push(l.drop_first(), descs, x, k);
        assert(l.push(x).drop_first() =~= l.drop_first().push(x));
    } else {
        assert(l.push(x).drop_first() =~= l);
    }
}

/// Every weight is at least zero, so every tally is.
proof fn lemma_tally_nonneg(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, k: int)
    ensures
        tally(l, descs, k) >= 0,
    decreases l.len(),
{
    if l.len() > 0 {
        lemma_tally_nonneg(l.drop_first(), descs, k);
    }
}

/// With a page or more per extent, a state's count is at most its pages,
/// and the keys are at most twice the extents.
proof fn lemma_tally_counts(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, k: int)
    requires
        0 <= k < 3,
        forall|i: int| 0 <= i < l.len() ==> (#[trigger] descs[l[i]].value()).pages >= 1 && descs[l[i]].value().state <= 2,
    ensures
        tally(l, descs, k) <= tally(l, descs, k + 3),
        tally(l, descs, 6) <= 2 * (tally(l, descs, 0) + tally(l, descs, 1) + tally(l, descs, 2)),
    decreases l.len(),
{
    if l.len() > 0 {
        assert forall|i: int| 0 <= i < l.drop_first().len() implies (#[trigger] descs[l.drop_first()[i]].value()).pages >= 1
            && descs[l.drop_first()[i]].value().state <= 2 by {
            assert(l.drop_first()[i] == l[i + 1]);
        }
        assert(descs[l[0]].value().pages >= 1 && descs[l[0]].value().state <= 2);
        lemma_tally_counts(l.drop_first(), descs, k);
    }
}

/// The run authorities of a well-formed state (`wf`, for proofs that hide
/// it).
proof fn lemma_run_auths(g0: Inner)
    requires
        g0.wf(),
    ensures
        g0.runs@@ == active_runs(g0.descs@),
        g0.pgs@@ == active_pages(g0.descs@),
{
}

/// The extents of the `Active` descriptors among `l`.
spec fn runs_in(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>) -> vstd::set::Set<(int, int)>
    decreases l.len(),
{
    if l.len() == 0 {
        vstd::set::Set::empty()
    } else {
        let v = descs[l[0]].value();
        let s = runs_in(l.drop_first(), descs);
        if v.state == RunState::Active as u8 {
            s.insert((v.start as int, v.pages as int))
        } else {
            s
        }
    }
}

/// The pages of the `Active` descriptors among `l`.
spec fn pages_in(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>) -> vstd::set::Set<int>
    decreases l.len(),
{
    if l.len() == 0 {
        vstd::set::Set::empty()
    } else {
        let v = descs[l[0]].value();
        let s = pages_in(l.drop_first(), descs);
        if v.state == RunState::Active as u8 {
            s.union(vstd::set_lib::set_int_range(v.start as int, v.start + v.pages))
        } else {
            s
        }
    }
}

/// The `Active` descriptors among `l`, pairwise apart, are as many
/// extents as `tally` counts runs, and as many pages as it counts run
/// pages; `runs_in` and `pages_in` hold their extents and pages.
proof fn lemma_counts(l: Seq<*mut HugeDesc>, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, np: usize)
    requires
        l.no_duplicates(),
        forall|i: int| 0 <= i < l.len() ==> #[trigger] descs.contains_key(l[i]),
        forall|d: *mut HugeDesc| #[trigger] descs.contains_key(d) ==> desc_ok(descs[d].value(), np),
        forall|d1: *mut HugeDesc, d2: *mut HugeDesc|
            #[trigger] descs.contains_key(d1) && #[trigger] descs.contains_key(d2) && d1 != d2 ==> apart(
                descs[d1].value(),
                descs[d2].value(),
            ),
    ensures
        runs_in(l, descs).len() == tally(l, descs, 0),
        pages_in(l, descs).len() == tally(l, descs, 3),
        forall|e: (int, int)|
            #[trigger] runs_in(l, descs).contains(e) <==> exists|i: int|
                0 <= i < l.len() && #[trigger] run_at(descs[l[i]].value(), e),
        forall|k: int|
            #[trigger] pages_in(l, descs).contains(k) <==> exists|i: int|
                0 <= i < l.len() && #[trigger] run_page(descs[l[i]].value(), k),
    decreases l.len(),
{
    if l.len() > 0 {
        let rest = l.drop_first();
        assert forall|i: int| 0 <= i < rest.len() implies #[trigger] rest[i] == l[i + 1] by {}
        assert(rest.no_duplicates()) by {
            assert forall|i: int, j: int| 0 <= i < rest.len() && 0 <= j < rest.len() && i != j implies rest[i] != rest[j] by {
                assert(l[i + 1] != l[j + 1]);
            }
        }
        assert forall|i: int| 0 <= i < rest.len() implies #[trigger] descs.contains_key(rest[i]) by {
            assert(descs.contains_key(l[i + 1]));
        }
        lemma_counts(rest, descs, np);
        let d0 = l[0];
        let v = descs[d0].value();
        assert(descs.contains_key(d0));
        assert(desc_ok(v, np));
        let e = (v.start as int, v.pages as int);
        let r = vstd::set_lib::set_int_range(v.start as int, v.start + v.pages);
        vstd::set_lib::lemma_int_range(v.start as int, v.start + v.pages);
        // `l[0]` is apart from every later descriptor.
        assert forall|i: int| 0 <= i < rest.len() implies apart(v, #[trigger] descs[rest[i]].value()) by {
            assert(l[0] != l[i + 1]);
            assert(descs.contains_key(rest[i]));
        }
        assert forall|e2: (int, int)| #[trigger] runs_in(l, descs).contains(e2) <==> exists|i: int|
            0 <= i < l.len() && #[trigger] run_at(descs[l[i]].value(), e2) by {
            if runs_in(l, descs).contains(e2) {
                if runs_in(rest, descs).contains(e2) {
                    let i = choose|i: int| 0 <= i < rest.len() && #[trigger] run_at(descs[rest[i]].value(), e2);
                    assert(run_at(descs[l[i + 1]].value(), e2));
                } else {
                    assert(run_at(descs[l[0]].value(), e2));
                }
            }
            if exists|i: int| 0 <= i < l.len() && #[trigger] run_at(descs[l[i]].value(), e2) {
                let i = choose|i: int| 0 <= i < l.len() && #[trigger] run_at(descs[l[i]].value(), e2);
                if i > 0 {
                    assert(run_at(descs[rest[i - 1]].value(), e2));
                }
            }
        }
        assert forall|k: int| #[trigger] pages_in(l, descs).contains(k) <==> exists|i: int|
            0 <= i < l.len() && #[trigger] run_page(descs[l[i]].value(), k) by {
            if pages_in(l, descs).contains(k) {
                if pages_in(rest, descs).contains(k) {
                    let i = choose|i: int| 0 <= i < rest.len() && #[trigger] run_page(descs[rest[i]].value(), k);
                    assert(run_page(descs[l[i + 1]].value(), k));
                } else {
                    assert(run_page(descs[l[0]].value(), k));
                }
            }
            if exists|i: int| 0 <= i < l.len() && #[trigger] run_page(descs[l[i]].value(), k) {
                let i = choose|i: int| 0 <= i < l.len() && #[trigger] run_page(descs[l[i]].value(), k);
                if i > 0 {
                    assert(run_page(descs[rest[i - 1]].value(), k));
                }
            }
        }
        if v.state == RunState::Active as u8 {
            assert(!runs_in(rest, descs).contains(e)) by {
                if runs_in(rest, descs).contains(e) {
                    let i = choose|i: int| 0 <= i < rest.len() && #[trigger] run_at(descs[rest[i]].value(), e);
                    assert(apart(v, descs[rest[i]].value()));
                }
            }
            assert(pages_in(rest, descs).disjoint(r)) by {
                assert forall|k: int| #[trigger] pages_in(rest, descs).contains(k) implies !r.contains(k) by {
                    let i = choose|i: int| 0 <= i < rest.len() && #[trigger] run_page(descs[rest[i]].value(), k);
                    assert(apart(v, descs[rest[i]].value()));
                }
            }
            vstd::set_lib::lemma_set_disjoint_lens(pages_in(rest, descs), r);
        }
    }
}

/// Exact huge stats: in every value the huge lock admits, `stats.runs`
/// is the number of live runs (the extents the run authority holds) and
/// `stats.run_pages` the number of their pages, guards included (the
/// pages the page authority holds). `malloc_stats_print` reports these
/// counters, and a run token's extent and pages are among the
/// authorities' ([`theorem_run_counted`]).
proof fn theorem_huge_stats(g: Inner)
    requires
        g.wf(),
    ensures
        g.stats.runs == g.runs@@.len(),
        g.stats.run_pages == g.pgs@@.len(),
{
    lemma_active(g.descs@);
    assert forall|i: int| 0 <= i < g.all@.len() implies #[trigger] g.descs@.contains_key(g.all@[i]) by {
        assert(g.all@.contains(g.all@[i]));
    }
    lemma_counts(g.all@, g.descs@, g.next_page);
    assert(g.runs@@ =~= runs_in(g.all@, g.descs@)) by {
        assert forall|e: (int, int)| g.runs@@.contains(e) <==> runs_in(g.all@, g.descs@).contains(e) by {
            if g.runs@@.contains(e) {
                let d = choose|d: *mut HugeDesc| #[trigger] g.descs@.contains_key(d) && run_at(g.descs@[d].value(), e);
                assert(g.all@.contains(d));
                let i = choose|i: int| 0 <= i < g.all@.len() && g.all@[i] == d;
                assert(run_at(g.descs@[g.all@[i]].value(), e));
            }
            if runs_in(g.all@, g.descs@).contains(e) {
                let i = choose|i: int| 0 <= i < g.all@.len() && #[trigger] run_at(g.descs@[g.all@[i]].value(), e);
                assert(g.descs@.contains_key(g.all@[i]));
            }
        }
    }
    assert(g.pgs@@ =~= pages_in(g.all@, g.descs@)) by {
        assert forall|k: int| g.pgs@@.contains(k) <==> pages_in(g.all@, g.descs@).contains(k) by {
            if g.pgs@@.contains(k) {
                let d = choose|d: *mut HugeDesc| #[trigger] g.descs@.contains_key(d) && run_page(g.descs@[d].value(), k);
                assert(g.all@.contains(d));
                let i = choose|i: int| 0 <= i < g.all@.len() && g.all@[i] == d;
                assert(run_page(g.descs@[g.all@[i]].value(), k));
            }
            if pages_in(g.all@, g.descs@).contains(k) {
                let i = choose|i: int| 0 <= i < g.all@.len() && #[trigger] run_page(g.descs@[g.all@[i]].value(), k);
                assert(g.descs@.contains_key(g.all@[i]));
            }
        }
    }
    assert(stat(g.stats, 0) == tally(g.all@, g.descs@, 0));
    assert(stat(g.stats, 3) == tally(g.all@, g.descs@, 3));
}

/// A live run is counted: the run authorities, when they are the
/// process's, hold its token's extent and pages.
proof fn theorem_run_counted(tracked g: &Inner, tracked t: &RunToken)
    requires
        g.wf(),
        g.runs@.id() == run_loc(),
        g.pgs@.id() == page_loc(),
        t.wf(),
    ensures
        g.runs@@.contains((t.start(), t.npages())),
        forall|k: int| t.start() <= k < t.start() + t.npages() ==> #[trigger] g.pgs@@.contains(k),
{
    t.ext.agree(g.runs.borrow());
    t.pages.agree(g.pgs.borrow());
    vstd::set_lib::lemma_int_range(t.start(), t.start() + t.npages());
}

/// The extent `(s, n)` of a run token, which the extents' authority
/// holds, is that of the `Active` descriptor `d` that starts at `s`: no
/// other extent has page `s`.
proof fn lemma_token_extent(g0: Inner, d: *mut HugeDesc, s: int, n: int)
    requires
        g0.wf(),
        g0.runs@@.contains((s, n)),
        g0.descs@.contains_key(d),
        g0.descs@[d].value().state == RunState::Active as u8,
        g0.descs@[d].value().start == s,
    ensures
        n == g0.descs@[d].value().pages,
{
    lemma_active(g0.descs@);
    let x = choose|x: *mut HugeDesc| #[trigger] g0.descs@.contains_key(x) && run_at(g0.descs@[x].value(), (s, n));
    if x != d {
        assert(desc_ok(g0.descs@[x].value(), g0.next_page));
        assert(desc_ok(g0.descs@[d].value(), g0.next_page));
        assert(apart(g0.descs@[x].value(), g0.descs@[d].value()));
    }
}

/// A live descriptor is counted: its state's extent and page counters
/// hold it, so [`HugeStats::sub`] can take it out.
proof fn lemma_can_sub(g: Inner, d: *mut HugeDesc)
    requires
        g.wf(),
        g.descs@.contains_key(d),
    ensures
        g.stats.can_sub(state_of(g.descs@[d].value().state), g.descs@[d].value().pages as usize),
{
    let v = g.descs@[d].value();
    let j = g.all@.index_of(d);
    assert(desc_ok(v, g.next_page));
    let k = v.state as int;
    lemma_tally_remove(g.all@, g.descs@, j, k);
    lemma_tally_nonneg(g.all@.remove(j), g.descs@, k);
    lemma_tally_remove(g.all@, g.descs@, j, k + 3);
    lemma_tally_nonneg(g.all@.remove(j), g.descs@, k + 3);
    assert(stat(g.stats, k) == tally(g.all@, g.descs@, k));
    assert(stat(g.stats, k + 3) == tally(g.all@, g.descs@, k + 3));
}

} // verus!

// SAFETY: the raw pointers address the metadata region, which is process
// global; the `Lock` serialises every access.
unsafe impl Send for Inner {}

verus! {

/// The huge allocator: one per process, shared by every arena (design
/// section 7); the arena an allocation was made through is recorded per
/// run for the per-arena statistics.
pub struct Huge {
    base: usize,
    lg_page: u32,
    npages: usize,
    guard_install: bool,
    /// The `guards` key (design 7): false places no guard after a run at
    /// all, in either mode (`guard_install` is then false too), so a run
    /// and the free pages around it stay one read-write VMA.
    guards: bool,
    retain_max: AtomicUsize,
    /// `huge_dirty_decay_ms:0` (design 7): a freed run is purged as it is
    /// freed, whatever its size, instead of waiting in the `Empty` list
    /// for a decay pass; set once at init (`Shared::init`).
    purge_on_free: AtomicBool,
    /// A `PROT_NONE` page may lie below the mark outside a live run's
    /// guard, so every allocation commits its own pages ([`Huge::prepare`]).
    /// Set in guard-install mode when some guard was refused its marker
    /// (`EINVAL` on an `mlock`ed range) and is a `PROT_NONE` page instead:
    /// `MADV_GUARD_REMOVE` does not touch such a page, so from then on a
    /// free also makes its run's guard read-write again, as the fallback
    /// mode always does; and in either mode when making a freed run's
    /// guard read-write failed. Never cleared: a cost, not a failure, that
    /// sticks.
    mixed_guards: AtomicBool,
    /// The decay step gate (`purge::Decayer`): `dirty_at` of the head of
    /// the `Empty` list (its oldest member), kept in step with the list
    /// under the lock by `sync_oldest` and read without it by the decay
    /// driver, and the next walk's deadline.
    gate: Decayer,
    inner: Lock<Inner>,
}

impl Huge {
    /// The region's geometry: `npages` pages of `2^lg_page` bytes from
    /// `base`, below the end of the user address space; the lock's first
    /// value has the same page count, shift and base, and the lock its
    /// rank.
    #[verifier::type_invariant]
    spec fn inv(&self) -> bool {
        &&& self.lg_page < 64
        &&& 4 <= self.lg_page
        &&& 0 < self.npages <= u32::MAX
        &&& self.base + self.npages * pow2(self.lg_page as nat) <= crate::meta::va_limit()
        &&& lock_init(self.inner).spec_np() == self.npages
        &&& lock_init(self.inner).spec_lg() == self.lg_page
        &&& lock_init(self.inner).spec_base() == self.base
        &&& self.inner.rank() == rank_huge()
    }
}

/// A commit, or a guard marker's removal, of `[addr, addr + len)` leaves
/// the guard page of every live run of `g` (the huge lock's value) but
/// `freed` inaccessible: the range lies in `h`'s region and shares no byte
/// with the [`GUARD_PAGES`] that end such a run (`Active`, `run_ok`).
/// `freed` is the run [`Huge::free_run`] is freeing, whose guard goes
/// with it: when it names a descriptor of `g`, the range is exactly that
/// run's guard. Elsewhere it is null, which names no descriptor
/// (`perms_ok`).
spec fn spares_guards(h: Huge, g: Inner, freed: *mut HugeDesc, addr: int, len: int) -> bool {
    let p = pow2(h.lg_page as nat) as int;
    &&& h.base <= addr
    &&& addr + len <= h.base + h.npages * p
    &&& forall|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && d != freed && g.descs@[d].value().state == RunState::Active as u8 ==> {
            let v = g.descs@[d].value();
            crate::layout::disjoint(addr, len, h.base + (v.start + v.pages - GUARD_PAGES) * p, GUARD_PAGES * p)
        }
    &&& g.descs@.contains_key(freed) ==> {
        let v = g.descs@[freed].value();
        addr == h.base + (v.start + v.pages - GUARD_PAGES) * p && len == GUARD_PAGES * p
    }
}

/// `[addr, addr + len)` shares no byte with any live run of `g` (`Active`),
/// its guard page included: bytes of a run being placed, which nobody
/// holds.
spec fn apart_from_runs(h: Huge, g: Inner, addr: int, len: int) -> bool {
    let p = pow2(h.lg_page as nat) as int;
    forall|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && g.descs@[d].value().state == RunState::Active as u8 ==> {
            let v = g.descs@[d].value();
            crate::layout::disjoint(addr, len, h.base + v.start * p, v.pages * p)
        }
}

/// `lk` is the permission of `h`'s lock cell ([`Lock::has_perm`]): only
/// a holder of the lock has it, from its guard
/// ([`LockGuard::perm`](crate::lock::LockGuard::perm)), so
/// its value is the lock's. And when `freed` names a descriptor of that
/// value, the run's [`RunToken`] has come back: its extent is out of the
/// run authority (`runs`), as [`Huge::free_run`] leaves it before it
/// frees the guard. A run still handed out keeps its token's extent there
/// (the authority agrees with every outstanding token, `tok.ext.agree` in
/// `free_run`), so `freed` names no such run.
spec fn held_by(h: Huge, lk: LockPerm<Inner>, freed: *mut HugeDesc) -> bool {
    let g = *lk.value();
    &&& h.inner.has_perm(lk)
    &&& g.descs@.contains_key(freed) ==> {
        let v = g.descs@[freed].value();
        !g.runs@@.contains((v.start as int, v.pages as int))
    }
}

/// Make `[addr, addr + len)` read-write. Every caller holds the huge lock
/// (`lk`, [`held_by`]) and proves the range apart from its live runs'
/// guards (`spares_guards`). On `Ok` its bytes are
/// [`committed`](crate::vspec::unbacked::committed).
fn commit(addr: usize, len: usize, Ghost(h): Ghost<Huge>, Tracked(lk): Tracked<&LockPerm<Inner>>, Ghost(freed): Ghost<*mut HugeDesc>) -> (r: Result<(), Errno>)
    requires
        crate::sys::may_syscall(),
        held_by(h, *lk, freed),
        spares_guards(h, *lk.value(), freed, addr as int, len as int),
    ensures
        held_by(h, *lk, freed),
        spares_guards(h, *lk.value(), freed, addr as int, len as int),
        r is Ok ==> crate::vspec::unbacked::committed_range(addr as int, len as int),
{
    // SAFETY: inside our own reservation; adding permissions to pages
    // nobody holds cannot break anything.
    unsafe { sys::mprotect(addr, len, PROT_RW) }
}

fn protect_none(addr: usize, len: usize) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: the page is a guard that no caller may touch.
    unsafe { sys::mprotect(addr, len, PROT_NONE) }
}

/// Make the guard page at `guard` `PROT_NONE` for the run at `run`,
/// whose first page and the guard are committed and in one VMA. The
/// split VMAs must be able to merge again when the run is freed and the
/// guard made read-write (`free_run`), and the kernel merges two anonymous
/// VMAs only when they share an `anon_vma`, which a VMA receives at its
/// first write fault (from a compatible neighbour when one has it, the
/// previous run's guard here, else a new one). Split before that fault,
/// the run and its guard would each get their own on first touch and
/// never merge back, leaving one VMA per run ever placed after every run
/// was freed (measured). So the run's first page is populated for write
/// first (`MADV_POPULATE_WRITE`, kernel 5.14, the floor; it writes
/// nothing, the page still reads as zero, and the caller is about to
/// touch it), and both halves of the split inherit the `anon_vma`. If
/// the kernel refuses the populate (a seccomp filter: `EINVAL`, `EPERM` or
/// `ENOSYS`), a zero written to
/// the run's first byte faults the page in the same way ([`write_zero`]).
/// The cost is the run's first page resident from the start (a whole
/// 2 MiB page for a PMD-aligned run under host THP `always`). `populate`
/// may be false for a run inside the mark in the fallback mode: every VMA
/// there was split off one populated this way, so it has its `anon_vma`.
///
/// The caller holds the huge lock (`lk`, [`held_by`]), and the run's first
/// page lies in the region apart from every live run's guard
/// (`spares_guards`) and from every live run (`apart_from_runs`); when it
/// populates, the run's first byte is
/// [`committed`](crate::vspec::unbacked::committed).
fn split_guard(run: usize, guard: usize, page: usize, populate: bool, Ghost(h): Ghost<Huge>, Tracked(lk): Tracked<&LockPerm<Inner>>) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
        held_by(h, *lk, vstd::raw_ptr::ptr_null_mut()),
        !lk.value().descs@.contains_key(vstd::raw_ptr::ptr_null_mut()),
        spares_guards(h, *lk.value(), vstd::raw_ptr::ptr_null_mut(), run as int, page as int),
        apart_from_runs(h, *lk.value(), run as int, page as int),
        1 <= page,
        populate ==> crate::vspec::unbacked::committed(run as int),
{
    proof {
        // The first byte of that page spares the guards as the page does.
        let g = *lk.value();
        assert forall|d: *mut HugeDesc|
            #[trigger] g.descs@.contains_key(d) && d != vstd::raw_ptr::ptr_null_mut() && g.descs@[d].value().state == RunState::Active as u8 implies {
                let v = g.descs@[d].value();
                let p = pow2(h.lg_page as nat) as int;
                crate::layout::disjoint(run as int, 1, h.base + (v.start + v.pages - GUARD_PAGES) * p, GUARD_PAGES * p)
            } by {}
        assert(spares_guards(h, g, vstd::raw_ptr::ptr_null_mut(), run as int, 1));
    }
    if populate {
        // SAFETY: the first page of a run being placed, which nobody
        // holds yet; populating leaves its contents alone.
        if let Err(e) = unsafe { sys::madvise(run, page, MADV_POPULATE_WRITE) } {
            // A filter's refusal (`EINVAL`, `EPERM`, `ENOSYS`) falls back
            // to the write; anything else (`ENOMEM` from a memory cgroup,
            // `EHWPOISON`) is a resource failure the write would only
            // meet again as a fault, so the run is not handed out
            // (design 14.1).
            if e.0 == Errno::EINVAL.0 || e.0 == Errno::EPERM.0 || e.0 == Errno::ENOSYS.0 {
                write_zero(run, Ghost(h), Tracked(lk));
            } else {
                return Err(e);
            }
        }
    }
    protect_none(guard, GUARD_PAGES * page)
}

/// Write a zero to the byte at `addr`, the first byte of a run being
/// placed (`split_guard`), to give its VMA an `anon_vma` by a write fault
/// when `MADV_POPULATE_WRITE` is refused.
///
/// Trusted only under its `requires`: the caller holds the huge lock
/// (`lk`, [`held_by`]), `addr` was made read-write at some point
/// ([`committed`](crate::vspec::unbacked::committed)), the byte lies in
/// the region apart from every live run's guard (`spares_guards`), and it
/// belongs to no live run (`apart_from_runs`), so nobody holds it. A zero
/// leaves a page that read as zero reading as zero, so the zero tokens
/// (`axiom_fresh_anon_zero`, `axiom_discarded_zero`) stay true, and a
/// dirty `Empty` page's contents are not needed.
///
/// Limit, the trusted part: `committed` never expires, so the `requires`
/// does not rule out a byte made `PROT_NONE` after its commit and outside
/// every live run's guard. Below the mark in guard-install mode, with no
/// commit in the same `prepare`, that the byte is read-write rests on
/// [`Huge::mixed_guards`] being set whenever such a page may exist (a
/// guard refused its marker, or a guard whose release failed).
#[verifier::external_body]
fn write_zero(addr: usize, Ghost(h): Ghost<Huge>, Tracked(lk): Tracked<&LockPerm<Inner>>)
    requires
        held_by(h, *lk, vstd::raw_ptr::ptr_null_mut()),
        crate::vspec::unbacked::committed(addr as int),
        spares_guards(h, *lk.value(), vstd::raw_ptr::ptr_null_mut(), addr as int, 1),
        apart_from_runs(h, *lk.value(), addr as int, 1),
{
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(addr as *mut u8, 0) };
}

fn guard_install(addr: usize, len: usize) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: as `protect_none`.
    unsafe { sys::madvise(addr, len, MADV_GUARD_INSTALL) }
}

/// Remove the guard marker on `[addr, addr + len)`, the guard of the run
/// being freed. Every caller proves the range apart from every other live
/// run's guard (`spares_guards`), holding the huge lock (`lk`,
/// [`held_by`]).
fn guard_remove(addr: usize, len: usize, Ghost(h): Ghost<Huge>, Tracked(lk): Tracked<&LockPerm<Inner>>, Ghost(freed): Ghost<*mut HugeDesc>) -> (r: Result<(), Errno>)
    requires
        crate::sys::may_syscall(),
        held_by(h, *lk, freed),
        spares_guards(h, *lk.value(), freed, addr as int, len as int),
    ensures
        held_by(h, *lk, freed),
        spares_guards(h, *lk.value(), freed, addr as int, len as int),
{
    // SAFETY: the run is being freed; the guard protects nothing now.
    unsafe { sys::madvise(addr, len, MADV_GUARD_REMOVE) }
}

/// Null names no descriptor of `g`, and every live run of `g` ends by
/// `watermark`, has its guard page, and shares no page with
/// `[s, s + need)`: what the placement [`Huge::find`] returns has, at the
/// mark.
spec fn runs_apart(g: Inner, s: int, need: int, watermark: int) -> bool {
    &&& !g.descs@.contains_key(vstd::raw_ptr::ptr_null_mut())
    &&& forall|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && g.descs@[d].value().state == RunState::Active as u8 ==> {
            let v = g.descs@[d].value();
            &&& v.pages >= GUARD_PAGES
            &&& v.start + v.pages <= watermark
            &&& (v.start + v.pages <= s || s + need <= v.start)
        }
}

/// The first page of a placement at `s` ([`runs_apart`]) shares no byte
/// with any live run of `g` ([`apart_from_runs`]): each such run ends by
/// `s` or starts at or past `s + need`, with `need` a page or more.
proof fn lemma_page_apart_from_runs(h: Huge, g: Inner, s: int, need: int, watermark: int)
    requires
        h.inv(),
        0 <= s,
        1 <= need,
        runs_apart(g, s, need, watermark),
    ensures
        apart_from_runs(h, g, h.base + s * pow2(h.lg_page as nat), pow2(h.lg_page as nat) as int),
{
    let p = pow2(h.lg_page as nat) as int;
    vstd::arithmetic::power2::lemma_pow2_pos(h.lg_page as nat);
    assert forall|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && g.descs@[d].value().state == RunState::Active as u8 implies {
            let v = g.descs@[d].value();
            crate::layout::disjoint(h.base + s * p, p, h.base + v.start * p, v.pages * p)
        } by {
        let v = g.descs@[d].value();
        let (a, n) = (v.start as int, v.pages as int);
        assert(a * p + n * p == (a + n) * p) by (nonlinear_arith);
        assert(s * p + p == (s + 1) * p) by (nonlinear_arith);
        if a + n <= s {
            vstd::arithmetic::mul::lemma_mul_inequality(a + n, s, p);
        } else {
            vstd::arithmetic::mul::lemma_mul_inequality(s + 1, a, p);
        }
    }
}

/// Pages `x..y` of `h`'s region spare the guards ([`spares_guards`]) of
/// the live runs of `g` but `freed` when each of those ends by `x` or has
/// its guard page at or past `y`.
proof fn lemma_pages_spare_guards(h: Huge, g: Inner, freed: *mut HugeDesc, x: int, y: int)
    requires
        h.inv(),
        0 <= x <= y <= h.npages,
        g.descs@.contains_key(freed) ==> {
            let v = g.descs@[freed].value();
            x == v.start + v.pages - GUARD_PAGES && y == v.start + v.pages
        },
        forall|d: *mut HugeDesc|
            #[trigger] g.descs@.contains_key(d) && d != freed && g.descs@[d].value().state == RunState::Active as u8 ==> {
                let v = g.descs@[d].value();
                v.start + v.pages <= x || y <= v.start + v.pages - GUARD_PAGES
            },
    ensures
        spares_guards(h, g, freed, h.base + x * pow2(h.lg_page as nat), (y - x) * pow2(h.lg_page as nat)),
{
    let p = pow2(h.lg_page as nat) as int;
    vstd::arithmetic::power2::lemma_pow2_pos(h.lg_page as nat);
    assert(x * p + (y - x) * p == y * p) by (nonlinear_arith);
    vstd::arithmetic::mul::lemma_mul_inequality(y, h.npages as int, p);
    assert forall|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && d != freed && g.descs@[d].value().state == RunState::Active as u8 implies {
            let v = g.descs@[d].value();
            crate::layout::disjoint(h.base + x * p, (y - x) * p, h.base + (v.start + v.pages - GUARD_PAGES) * p, GUARD_PAGES * p)
        } by {
        let v = g.descs@[d].value();
        let e = v.start + v.pages;
        assert((e - GUARD_PAGES) * p + GUARD_PAGES * p == e * p) by (nonlinear_arith);
        if e <= x {
            vstd::arithmetic::mul::lemma_mul_inequality(e, x, p);
        } else {
            vstd::arithmetic::mul::lemma_mul_inequality(y, e - GUARD_PAGES, p);
        }
    }
}

/// The guard page of the live run `d` of `g` ([`spares_guards`] with `d`
/// as the run being freed): apart from every other live run's guard, as
/// live extents are pairwise apart (`Inner::wf`).
proof fn lemma_freed_guard_apart(h: Huge, g: Inner, d: *mut HugeDesc)
    requires
        h.inv(),
        g.wf(),
        g.spec_np() == h.npages,
        g.descs@.contains_key(d),
        g.descs@[d].value().state == RunState::Active as u8,
    ensures
        ({
            let (v, p) = (g.descs@[d].value(), pow2(h.lg_page as nat) as int);
            spares_guards(h, g, d, h.base + (v.start + v.pages - GUARD_PAGES) * p, GUARD_PAGES * p)
        }),
{
    g.lemma_wf_for_take();
    let v = g.descs@[d].value();
    assert(desc_ok(v, g.next_page));
    assert(run_ok(v, g.lg@));
    assert forall|x: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(x) && x != d && g.descs@[x].value().state == RunState::Active as u8 implies {
            let w = g.descs@[x].value();
            w.start + w.pages <= v.start + v.pages - GUARD_PAGES || v.start + v.pages <= w.start + w.pages - GUARD_PAGES
        } by {
        assert(apart(g.descs@[x].value(), v));
        assert(run_ok(g.descs@[x].value(), g.lg@));
    }
    lemma_pages_spare_guards(h, g, d, v.start + v.pages - GUARD_PAGES, v.start + v.pages);
}

/// Commits keep the guard pages of live huge runs inaccessible: every
/// call of [`commit`] (`mprotect` read-write) or [`guard_remove`]
/// (`MADV_GUARD_REMOVE`) is made holding the huge lock, whose cell's
/// permission `lk` it takes from the caller's guard (`held_by`, so `g`,
/// the value `lk` holds, is the lock's), and makes accessible only bytes
/// of `h`'s region, and none of the guard page of any live run of `g`
/// other than `freed`, the run [`Huge::free_run`] is freeing. When
/// `freed` names a descriptor of `g`, its run's token has come back to
/// the lock (`held_by`: no live run's has), and the range is exactly that
/// run's guard page (`spares_guards`), so the exception covers nothing
/// else. Both ensure what their precondition asks of each caller
/// ([`Huge::prepare`] for a run being placed, [`Huge::free_run`]), so
/// Verus rejects any caller that has not proved it. For the
/// process's huge allocator ([`Huge::is_process`]) that region is the huge
/// region, apart from every class region and the metadata region
/// (`Regions::separated`), so such a commit touches no class guard slab
/// either. Under `guards:false` no run has a guard to keep; the commits
/// stay apart from the pages all the same.
///
/// Not covered: a freed run's guard, which `free_run` makes read-write
/// again (or whose marker it removes) so that its VMA merges back; and
/// that `prepare` puts each run's guard on the page this names (its
/// `guard_install` and `split_guard` calls, read from the code). Trusted:
/// `mprotect` and `madvise` change only the range they are given.
proof fn theorem_huge_commit_avoids_guards(h: Huge, lk: LockPerm<Inner>, freed: *mut HugeDesc, addr: usize, len: usize, r: Result<(), Errno>, d: *mut HugeDesc)
    requires
        call_ensures(commit, (addr, len, Ghost(h), Tracked(&lk), Ghost(freed)), r)
            || call_ensures(guard_remove, (addr, len, Ghost(h), Tracked(&lk), Ghost(freed)), r),
        lk.value().descs@.contains_key(d),
        d != freed,
        lk.value().descs@[d].value().state == RunState::Active as u8,
    ensures
        held_by(h, lk, freed),
        h.spec_base() <= addr,
        addr + len <= h.spec_base() + h.spec_npages() * h.spec_page(),
        ({
            let (v, p) = (lk.value().descs@[d].value(), h.spec_page());
            crate::layout::disjoint(addr as int, len as int, h.spec_base() + (v.start + v.pages - GUARD_PAGES) * p, GUARD_PAGES * p)
        }),
        h.is_process() ==> ({
            let (r, region) = (crate::layout::process_regions(), (1usize << crate::layout::process_regions().lg_region) as int);
            &&& forall|c: int| 0 <= c < crate::layout::NCLASSES ==> crate::layout::disjoint(addr as int, len as int, #[trigger] r.class_base[c] as int, region)
            &&& crate::layout::disjoint(addr as int, len as int, r.meta_base as int, r.meta_len as int)
        }),
{
    if h.is_process() {
        let r = crate::layout::process_regions();
        let (stride, region) = ((1usize << r.lg_stride) as int, (1usize << r.lg_region) as int);
        let lg = r.lg_page as usize;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(r.huge_len >> lg, lg, r.huge_len);
        vstd::bits::lemma_usize_shl_is_mul(1, lg);
        assert(h.npages * h.spec_page() <= r.huge_len);
        assert forall|c: int| 0 <= c < crate::layout::NCLASSES implies crate::layout::disjoint(addr as int, len as int, #[trigger] r.class_base[c] as int, region) by {
            assert(r.superblock + c * stride <= r.class_base[c] && r.class_base[c] + region <= r.superblock + (c + 1) * stride);
            vstd::arithmetic::mul::lemma_mul_inequality(c + 1, crate::layout::NCLASSES as int, stride);
        }
    }
}

fn now_ns() -> u64
    requires
        crate::sys::may_syscall(),
{
    sys::now_coarse_ns()
}

/// A block for a descriptor from `meta`, with the permission to it
/// (`Meta::alloc_raw`'s, typed) and its claim (`Meta::alloc_raw`'s, which
/// [`free_desc`] gives back); null, with neither, when the metadata region
/// is exhausted. The block is `meta`'s, so metadata when `meta` is the
/// process's.
// The tracked result is part of the signature.
#[allow(clippy::type_complexity)]
#[inline(always)]
fn alloc_desc(
    meta: &Meta,
    Tracked(ord): Tracked<&mut LockOrder>,
) -> (r: (*mut HugeDesc, Tracked<Option<(PointsTo<HugeDesc>, crate::meta::MetaClaim)>>))
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_meta()),
    ensures
        *final(ord) == *old(ord),
        r.0@.addr != 0 ==> r.1@ is Some && (r.1@->0).0.ptr() == r.0 && (r.1@->0).0.is_uninit(),
        r.0@.addr != 0 ==> desc_claim_for((r.1@->0).1, r.0, meta.live_id()),
        r.0@.addr != 0 ==> meta.holds(r.0@.addr as int, vstd::layout::size_of::<HugeDesc>() as int),
        r.0@.addr != 0 && meta.is_process() ==> desc_in_meta(r.0),
{
    proof {
        crate::meta::lemma_consts();
    }
    let (p, Tracked(blk)) = meta.alloc_raw(Layout::new::<HugeDesc>(), Tracked(ord));
    let tracked (raw, claim) = blk;
    let d = p as *mut HugeDesc;
    proof {
        assert((d as *mut u8)@ == p@);
    }
    let tracked perm = desc_perm(raw, claim, d);
    (d, Tracked(perm))
}

/// The descriptor permission of a block of `raw` at `d`, with the block's
/// claim, when there is a block.
proof fn desc_perm(tracked raw: PointsToRaw, tracked claim: crate::meta::MetaClaim, d: *mut HugeDesc) -> (tracked r: Option<
    (PointsTo<HugeDesc>, crate::meta::MetaClaim),
>)
    ensures
        d@.addr != 0 && raw.is_range(d@.addr as int, vstd::layout::size_of::<HugeDesc>() as int)
            && raw.provenance() == d@.provenance && d@.addr as int % vstd::layout::align_of::<HugeDesc>() as int == 0
            ==> r is Some && (r->0).0.ptr() == d && (r->0).0.is_uninit() && (r->0).1 == claim,
{
    if d@.addr != 0 && raw.is_range(d@.addr as int, vstd::layout::size_of::<HugeDesc>() as int)
        && raw.provenance() == d@.provenance && d@.addr as int % vstd::layout::align_of::<HugeDesc>() as int == 0 {
        let tracked perm = raw.into_typed::<HugeDesc>(d@.addr);
        assert(perm.ptr()@ == d@);
        Some((perm, claim))
    } else {
        None
    }
}

/// The permission to the bytes of the descriptor block that `desc.0`
/// is the permission to (its contents dropped), with the block's claim:
/// what [`free_desc`] takes.
proof fn desc_raw(tracked desc: (PointsTo<HugeDesc>, crate::meta::MetaClaim)) -> (tracked r: (
    PointsToRaw,
    crate::meta::MetaClaim,
))
    ensures
        r.0.is_range(desc.0.ptr()@.addr as int, vstd::layout::size_of::<HugeDesc>() as int),
        r.0.provenance() == desc.0.ptr()@.provenance,
        r.1 == desc.1,
{
    let tracked (mut perm, claim) = desc;
    perm.leak_contents();
    (perm.into_raw(), claim)
}

/// Give the block `d` from [`alloc_desc`] back to `meta`, with `blk`,
/// the permission to its bytes and its claim, which `Meta::free` takes.
#[inline(always)]
fn free_desc(
    meta: &Meta,
    d: *mut HugeDesc,
    blk: Tracked<(PointsToRaw, crate::meta::MetaClaim)>,
    Tracked(ord): Tracked<&mut LockOrder>,
)
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_meta()),
        d@.addr != 0,
        blk@.0.is_range(d@.addr as int, vstd::layout::size_of::<HugeDesc>() as int),
        blk@.0.provenance() == d@.provenance,
        desc_claim_for(blk@.1, d, meta.live_id()),
    ensures
        *final(ord) == *old(ord),
{
    proof {
        assert((d as *mut u8)@.addr == d@.addr && (d as *mut u8)@.provenance == d@.provenance);
    }
    meta.free(d as *mut u8, Layout::new::<HugeDesc>(), blk, Tracked(ord));
}

/// Where a run may be taken ([`Huge::find`]'s answer): fresh pages at or
/// past the mark when no free extent touches it, or pages from a free extent
/// that starts at or before the run and either holds it or touches the mark.
spec fn take_ok(p: Placement, descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, next_page: usize) -> bool {
    if p.ext@.addr == 0 {
        p.start >= next_page && none_ends_at(descs, next_page as int)
    } else {
        let v = descs[p.ext].value();
        &&& descs.contains_key(p.ext)
        &&& v.state != RunState::Active as u8
        &&& v.start <= p.start
        &&& (p.start + p.pages <= v.start + v.pages || v.start + v.pages == next_page)
    }
}

/// Every key of `t` names a descriptor of `d` whose extent holds it and
/// ends at or before `mark`. Opaque: `Huge::take` reveals it only where it
/// reasons about keys, since each instance is costly.
#[verifier::opaque]
spec fn keys_inside(t: Table, d: Map<*mut HugeDesc, PointsTo<HugeDesc>>, mark: int) -> bool {
    forall|k: usize| #[trigger] t.lookup(k) is Some ==> d.contains_key(t.lookup(k).unwrap()) && d[t.lookup(
        k,
    ).unwrap()].value().start <= k < d[t.lookup(k).unwrap()].value().start + d[t.lookup(k).unwrap()].value().pages <= mark
}

/// The zero token of a run `Huge::take` placed at page `s` with `usable`
/// pages before its guard, over a region at `base` with pages of `pg`
/// bytes whose high-water mark was `mark`: empty unless `zeroed`, which
/// `take` sets for a run from fresh pages (no extent, or a start at or
/// past the extent it touches, which then ends at the mark) or from a
/// `Purged` extent whose pages under the run [`Huge::rediscard`] has just
/// discarded again under the lock (`ve` the extent). Pages at or past the
/// mark are fresh ([`axiom_fresh_anon_zero`]: only `take` raises the mark,
/// and no run has covered them); the run's pages in a `Purged` extent were
/// discarded with `MADV_DONTNEED` ([`axiom_discarded_zero`]; the
/// `requires` carries `rediscard`'s success for exactly those pages),
/// and when the run runs past such an extent the extent ends at the mark
/// and the rest is fresh.
proof fn zero_run(base: int, pg: int, s: int, usable: int, mark: int, has_ext: bool, ve: HugeDesc, zeroed: bool) -> (tracked z: Zeroed)
    requires
        pg > 0,
        usable >= 0,
        !has_ext ==> s >= mark,
        has_ext ==> ve.start <= s,
        has_ext ==> (s + usable + GUARD_PAGES <= ve.start + ve.pages || ve.start + ve.pages == mark),
        zeroed && has_ext ==> ve.state == RunState::Purged as u8 || s >= ve.start + ve.pages,
        // `Huge::rediscard`'s success for the run's pages in the extent.
        zeroed && has_ext && s < ve.start + ve.pages ==> {
            let hi = if s + usable <= ve.start + ve.pages { s + usable } else { ve.start + ve.pages };
            &&& 0 <= base
            &&& base + hi * pg <= usize::MAX
            &&& crate::sys::madvise_ok((base + s * pg) as usize, ((hi - s) * pg) as usize, crate::sys::MADV_DONTNEED)
        },
    ensures
        zeroed ==> z.addr() == base + s * pg && z.len() == usable * pg,
{
    if !zeroed {
        Zeroed::empty(0)
    } else if !has_ext || s >= ve.start + ve.pages {
        // At or past the mark.
        assert(s >= mark);
        axiom_fresh_anon_zero(base + s * pg, usable * pg)
    } else if s + usable <= ve.start + ve.pages {
        // Inside the `Purged` extent.
        assert(ve.state == RunState::Purged as u8);
        assert(base + s * pg + usable * pg == base + (s + usable) * pg) by (nonlinear_arith);
        assert(s * pg >= 0 && usable * pg >= 0) by (nonlinear_arith)
            requires s >= 0, usable >= 0, pg > 0;
        axiom_discarded_zero(base + s * pg, usable * pg)
    } else {
        // The `Purged` extent up to the mark, then fresh pages.
        let e = ve.start + ve.pages;
        assert(ve.state == RunState::Purged as u8 && e == mark);
        assert(base + s * pg + (e - s) * pg == base + e * pg) by (nonlinear_arith);
        assert((e - s) * pg + (s + usable - e) * pg == usable * pg) by (nonlinear_arith);
        assert(s * pg >= 0 && (e - s) * pg >= 0 && (s + usable - e) * pg >= 0) by (nonlinear_arith)
            requires 0 <= s < e, e < s + usable, pg > 0;
        let tracked lo = axiom_discarded_zero(base + s * pg, (e - s) * pg);
        let tracked hi = axiom_fresh_anon_zero(base + e * pg, (s + usable - e) * pg);
        lo.join(hi)
    }
}

/// `mmap(2)`: a private anonymous mapping reads as zero until it is first
/// written. Changing its protection (`mprotect`), populating it
/// (`MADV_POPULATE_WRITE`), advising it (`MADV_HUGEPAGE`,
/// `MADV_COLLAPSE`) and guard markers (`MADV_GUARD_INSTALL`/`REMOVE`)
/// leave the contents alone.
///
/// Premise, trusted and the caller's: the `len` bytes at `addr` lie in the
/// huge region or a class region (`Regions::reserve`) and nothing has
/// written them since it was mapped, or written anything but zero. Two
/// callers (`huge::tests::zero_axioms_called_from_listed_sites` pins
/// them). [`zero_run`], for pages at or past the high-water mark it is
/// given; its `requires` ties that mark to the run `Huge::take` places
/// (only `take` raises the mark, and no run has covered pages past it).
/// And `class::table::SlabTable::new`, for its whole class region, which
/// it makes one token of: `new` runs once per class (its `words_once`
/// token, `vspec::once`), before any slab of the region is handed out, and
/// the token then goes only one group at a time to the arena `take_group`
/// hands the group to, and one slab at a time to the `take` that makes
/// the slab `Active` (`class::Inner::zeros`).
pub(crate) axiom fn axiom_fresh_anon_zero(addr: int, len: int) -> (tracked z: Zeroed)
    requires
        0 <= len,
    ensures
        z.addr() == addr,
        z.len() == len,
;

/// `madvise(2)`: after `MADV_DONTNEED` on private anonymous memory,
/// reads return zero-fill-on-demand pages (`MADV_FREE` gives no such
/// guarantee: the old contents stay until the kernel reclaims the page).
///
/// Checked, by its `requires`: some `madvise(addr, len, MADV_DONTNEED)`
/// of exactly these bytes succeeded (`sys::madvise_ok`, which says that
/// some such call did, not which one or when). Private to this module,
/// and [`zero_run`] is its one caller (pinned as for
/// [`axiom_fresh_anon_zero`]), for a `Purged` extent; `zero_run`'s
/// `requires` carries [`Huge::rediscard`]'s success, from the call `take`
/// makes in the same lock hold before the run is handed out.
///
/// Premise, trusted and the caller's: nothing has written the bytes since
/// `take`'s `rediscard` call. The extent's own purge is not what the
/// premise rests on: its pages are read-write after it, and a stale
/// pointer or an underflow from the live run above can have written them
/// since.
axiom fn axiom_discarded_zero(addr: int, len: int) -> (tracked z: Zeroed)
    requires
        0 <= addr,
        0 <= len,
        addr + len <= usize::MAX,
        crate::sys::madvise_ok(addr as usize, len as usize, crate::sys::MADV_DONTNEED),
    ensures
        z.addr() == addr,
        z.len() == len,
;

/// Initialise a metadata block, whose permission is `perm`, as a free
/// extent.
fn init_free(
    d: *mut HugeDesc,
    Tracked(perm): Tracked<&mut PointsTo<HugeDesc>>,
    start: usize,
    pages: usize,
    state: RunState,
    dirty_at: u64,
)
    requires
        old(perm).ptr() == d,
        state != RunState::Active,
    ensures
        final(perm).ptr() == d,
        final(perm).is_init(),
        final(perm).value() == (HugeDesc {
            start: start as u32,
            pages: pages as u32,
            arena: 0,
            state: state as u8,
            _pad: 0,
            usable: 0,
            dirty_at,
            prev: ptr::null_mut(),
            next: ptr::null_mut(),
        }),
{
    debug_assert!(state.is_free());
    // `d` is a fresh metadata block for a `HugeDesc`.
    ptr_mut_write(
        d,
        Tracked(perm),
        HugeDesc {
            start: start as u32,
            pages: pages as u32,
            arena: 0,
            state: state as u8,
            _pad: 0,
            usable: 0,
            dirty_at,
            prev: ptr::null_mut(),
            next: ptr::null_mut(),
        },
    )
}

/// What [`Huge::take`] puts in place of the extent `ext` it takes a run
/// from (or of the fresh pages at the mark): the run `vr` at `run`, the
/// extent's head `vh` (kept in `ext`), its tail or the hole alignment
/// left at the mark `vs` (in `spare`). `mark` is `next_page` before,
/// `nm` after.
#[allow(dead_code)]
ghost struct Take {
    ext: *mut HugeDesc,
    has_ext: bool,
    ve: HugeDesc,
    head: bool,
    vh: HugeDesc,
    used: bool,
    spare: *mut HugeDesc,
    vs: HugeDesc,
    run: *mut HugeDesc,
    vr: HugeDesc,
    mark: usize,
    nm: usize,
}

impl Take {
    /// The descriptor at `x` afterwards, up to its list links.
    spec fn model(self, d0: Map<*mut HugeDesc, PointsTo<HugeDesc>>, x: *mut HugeDesc) -> Option<HugeDesc> {
        if x == self.run {
            Some(self.vr)
        } else if self.used && x == self.spare {
            Some(self.vs)
        } else if self.has_ext && x == self.ext {
            if self.head { Some(self.vh) } else { None }
        } else if d0.contains_key(x) {
            Some(d0[x].value())
        } else {
            None
        }
    }

    /// The table's entry for `k` afterwards.
    spec fn lookup(self, t0: Table, k: usize) -> Option<*mut HugeDesc> {
        if is_key(self.vr, k as int) {
            Some(self.run)
        } else if self.head && is_key(self.vh, k as int) {
            Some(self.ext)
        } else if self.used && is_key(self.vs, k as int) {
            Some(self.spare)
        } else if self.has_ext && is_key(self.ve, k as int) {
            None
        } else {
            t0.lookup(k)
        }
    }

    /// What tally `k` gains.
    spec fn delta(self, k: int) -> int {
        (if self.has_ext { -weight(self.ve, k) } else { 0 }) + (if self.head { weight(self.vh, k) } else { 0 }) + (if self.used {
            weight(self.vs, k)
        } else {
            0
        }) + weight(self.vr, k)
    }

    /// `all` without a retired `ext`.
    spec fn base(self, a0: Seq<*mut HugeDesc>) -> Seq<*mut HugeDesc> {
        if self.has_ext && !self.head { a0.remove(a0.index_of(self.ext)) } else { a0 }
    }

    /// `all` afterwards.
    spec fn all_after(self, a0: Seq<*mut HugeDesc>) -> Seq<*mut HugeDesc> {
        (if self.used { self.base(a0).push(self.spare) } else { self.base(a0) }).push(self.run)
    }

    /// The `Empty` list afterwards: an `Empty` tail goes right after
    /// `ext`, and a retired `Empty` `ext` leaves.
    spec fn elist_after(self, l0: Seq<*mut HugeDesc>) -> Seq<*mut HugeDesc> {
        if self.has_ext && self.ve.state == RunState::Empty as u8 {
            let l1 = if self.used { l0.insert(l0.index_of(self.ext) + 1, self.spare) } else { l0 };
            if self.head { l1 } else { l1.remove(l1.index_of(self.ext)) }
        } else {
            l0
        }
    }

    /// A piece lies where it may: inside `ext`, or past it when `ext`
    /// touches the mark; at or past the mark without `ext`.
    spec fn covered(self, v: HugeDesc) -> bool {
        if self.has_ext {
            &&& v.start >= self.ve.start
            &&& (v.start + v.pages <= self.ve.start + self.ve.pages || self.ve.start + self.ve.pages == self.mark)
        } else {
            v.start >= self.mark
        }
    }

    /// A piece's shape: a page or more, a valid state, below `nm`.
    spec fn piece(self, v: HugeDesc) -> bool {
        &&& v.pages >= 1
        &&& v.state <= 2
        &&& v.start + v.pages <= self.nm
        &&& self.covered(v)
    }

    /// The pieces, against the state `g0` they were cut from.
    spec fn pieces_ok(self, g0: Inner) -> bool {
        &&& self.mark == g0.next_page
        &&& self.has_ext == (self.ext@.addr != 0)
        &&& self.mark <= self.nm <= g0.np@
        &&& self.has_ext ==> g0.descs@.contains_key(self.ext) && self.ve == g0.descs@[self.ext].value() && self.ve.state
            != RunState::Active as u8
        &&& !self.has_ext ==> !self.head && (self.used ==> self.vs.state == RunState::Purged as u8)
        &&& self.has_ext && self.used ==> self.vs.state == self.ve.state
        &&& self.head ==> self.vh.state == self.ve.state && self.vh.start == self.ve.start
        &&& self.piece(self.vr) && self.vr.state == RunState::Active as u8
        &&& run_ok(self.vr, g0.lg@)
        &&& self.used ==> self.piece(self.vs) && self.vs.state != RunState::Active as u8 && apart(self.vs, self.vr)
        &&& self.head ==> self.piece(self.vh) && apart(self.vh, self.vr)
        &&& self.head && self.used ==> apart(self.vh, self.vs)
        &&& !g0.descs@.contains_key(self.run) || (self.has_ext && !self.head && self.run == self.ext)
        &&& self.used ==> !g0.descs@.contains_key(self.spare) && self.spare != self.run
        &&& self.run@.addr != 0
        &&& self.used ==> self.spare@.addr != 0
        &&& (if self.has_ext { self.ve.pages as int } else { 0 }) + (self.nm - self.mark) == (if self.used {
            self.vs.pages as int
        } else {
            0
        }) + (if self.head { self.vh.pages as int } else { 0 }) + self.vr.pages
    }

    /// Where the free pieces lie, for coalescing: the head ends where the
    /// run starts; a tail runs from the run's end to the end of `ext`; a
    /// hole runs from the mark, which no free extent touched, to the run.
    /// And they cover what they must, for the exact free-page set: `ext`
    /// keeps a head when it starts below the run and leaves a tail when it
    /// ends past the run, and a hole is left whenever the run starts past
    /// the mark.
    spec fn touch_ok(self, g0: Inner) -> bool {
        &&& self.head ==> self.vh.start + self.vh.pages == self.vr.start
        &&& self.has_ext && self.used ==> self.vs.start == self.vr.start + self.vr.pages && self.vs.start + self.vs.pages
            == self.ve.start + self.ve.pages
        &&& !self.has_ext && self.used ==> self.vs.start == self.mark && self.vs.start + self.vs.pages == self.vr.start
        &&& !self.has_ext ==> none_ends_at(g0.descs@, self.mark as int)
        &&& self.has_ext && !self.head ==> self.vr.start <= self.ve.start
        &&& self.has_ext && !self.used ==> self.ve.start + self.ve.pages <= self.vr.start + self.vr.pages
        &&& !self.has_ext && !self.used ==> self.vr.start <= self.mark
    }
}

impl Inner {
    /// `self` is `g0` with the pieces of `t` in place.
    spec fn took(&self, g0: Inner, t: Take) -> bool {
        &&& t.pieces_ok(g0)
        &&& self.table.wf()
        &&& self.table.spec_count() == g0.table.spec_count() + t.delta(6)
        &&& forall|k: usize| #[trigger] self.table.lookup(k) == t.lookup(g0.table, k)
        &&& perms_ok(self.descs@)
        &&& forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> desc_claimed(self.dclaims@, d, self.mid@)
        &&& self.table.spec_mid() == self.mid@
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) <==> t.model(g0.descs@, x) is Some
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) ==> same_but_links(
            t.model(g0.descs@, x).unwrap(),
            self.descs@[x].value(),
        )
        &&& self.all@ == t.all_after(g0.all@)
        &&& forall|k: int| 0 <= k < 6 ==> #[trigger] stat(self.stats, k) == stat(g0.stats, k) + t.delta(k)
        &&& self.next_page == t.nm
        &&& self.stats.next_page == self.next_page
        &&& self.np == g0.np
        &&& self.lg == g0.lg
        &&& self.free.map.wf()
        &&& self.free.map.in_meta()
        &&& self.free.map.spec_nbits() == self.np@
        &&& self.free.own_claimed()
        &&& self.free.l1_words <= 0x1_0000_0000
        &&& self.elist@ == t.elist_after(g0.elist@)
        &&& elist_ok(self.empty_head, self.empty_tail, self.elist@, self.descs@)
    }
}

/// An old extent other than `ext` shares no page with a piece: it is
/// apart from `ext` and below the mark.
proof fn lemma_took_apart(g0: Inner, t: Take, x: *mut HugeDesc, v: HugeDesc)
    requires
        g0.wf(),
        t.pieces_ok(g0),
        g0.descs@.contains_key(x),
        !(t.has_ext && x == t.ext),
        t.piece(v),
    ensures
        apart(g0.descs@[x].value(), v),
{
    assert(desc_ok(g0.descs@[x].value(), g0.next_page));
    if t.has_ext {
        assert(apart(g0.descs@[x].value(), g0.descs@[t.ext].value()));
    }
}

/// What `l.push(y)` contains.
proof fn lemma_push_contains(l: Seq<*mut HugeDesc>, y: *mut HugeDesc, x: *mut HugeDesc)
    ensures
        l.push(y).contains(x) <==> l.contains(x) || x == y,
{
    if l.contains(x) {
        assert(l.push(y)[l.index_of(x)] == x);
    }
    if x == y {
        assert(l.push(y)[l.len() as int] == x);
    }
    if l.push(y).contains(x) {
        let i = l.push(y).index_of(x);
        if i < l.len() {
            assert(l[i] == x);
        }
    }
}

/// The extents after a take: inside the new mark, disjoint, each under
/// exactly its keys.
proof fn lemma_took_extents(g0: Inner, g1: Inner, t: Take)
    requires
        g0.wf(),
        g1.took(g0, t),
    ensures
        forall|d: *mut HugeDesc| #[trigger] g1.descs@.contains_key(d) ==> desc_ok(g1.descs@[d].value(), g1.next_page),
        forall|k: usize| #[trigger] g1.table.lookup(k) matches Some(d) ==> g1.descs@.contains_key(d),
        forall|d: *mut HugeDesc| #[trigger] g1.descs@.contains_key(d) ==> keyed(g1.table, g1.descs@[d].value(), d),
        forall|k: usize| #[trigger] g1.table.lookup(k) matches Some(d) ==> is_key(g1.descs@[d].value(), k as int),
        forall|d1: *mut HugeDesc, d2: *mut HugeDesc|
            #[trigger] g1.descs@.contains_key(d1) && #[trigger] g1.descs@.contains_key(d2) && d1 != d2 ==> apart(
                g1.descs@[d1].value(),
                g1.descs@[d2].value(),
            ),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let t0 = g0.table;
    // Which descriptors are old.
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && x != t.run && !(t.used && x == t.spare) && !(t.has_ext
        && x == t.ext) implies d0.contains_key(x) && same_but_links(d0[x].value(), d1[x].value()) by {}
    assert forall|d: *mut HugeDesc| #[trigger] d1.contains_key(d) implies desc_ok(d1[d].value(), g1.next_page) by {
        if d != t.run && !(t.used && d == t.spare) && !(t.has_ext && d == t.ext) {
            assert(desc_ok(d0[d].value(), g0.next_page));
        }
    }
    assert forall|d: *mut HugeDesc| #[trigger] d1.contains_key(d) implies keyed(g1.table, d1[d].value(), d) by {
        let v = d1[d].value();
        let k1 = v.start as usize;
        let k2 = (v.start + v.pages - 1) as usize;
        assert(g1.table.lookup(k1) == t.lookup(t0, k1));
        assert(g1.table.lookup(k2) == t.lookup(t0, k2));
        if d != t.run && !(t.used && d == t.spare) && !(t.has_ext && d == t.ext) {
            lemma_took_apart(g0, t, d, t.vr);
            if t.used {
                lemma_took_apart(g0, t, d, t.vs);
            }
            if t.head {
                lemma_took_apart(g0, t, d, t.vh);
            }
            if t.has_ext {
                assert(apart(d0[d].value(), t.ve));
            }
            assert(keyed(t0, d0[d].value(), d));
        }
    }
    assert forall|k: usize| #[trigger] g1.table.lookup(k) is Some implies d1.contains_key(g1.table.lookup(k).unwrap()) && is_key(
        d1[g1.table.lookup(k).unwrap()].value(),
        k as int,
    ) by {
        assert(g1.table.lookup(k) == t.lookup(t0, k));
        let y = g1.table.lookup(k).unwrap();
        if !is_key(t.vr, k as int) && !(t.head && is_key(t.vh, k as int)) && !(t.used && is_key(t.vs, k as int)) {
            assert(t0.lookup(k) == Some(y));
            assert(d0.contains_key(y) && is_key(d0[y].value(), k as int));
            assert(y != t.run && !(t.used && y == t.spare));
            if t.has_ext {
                assert(y != t.ext);
            }
            assert(t.model(d0, y) == Some(d0[y].value()));
            assert(d1.contains_key(y));
            assert(same_but_links(d0[y].value(), d1[y].value()));
        } else if is_key(t.vr, k as int) {
            assert(t.model(d0, t.run) is Some);
            assert(d1.contains_key(t.run));
        } else if t.head && is_key(t.vh, k as int) {
            assert(t.model(d0, t.ext) is Some);
            assert(d1.contains_key(t.ext));
        } else {
            assert(t.model(d0, t.spare) is Some);
            assert(d1.contains_key(t.spare));
        }
    }
    assert forall|x1: *mut HugeDesc, x2: *mut HugeDesc|
        #[trigger] d1.contains_key(x1) && #[trigger] d1.contains_key(x2) && x1 != x2 implies apart(d1[x1].value(), d1[x2].value()) by {
        let new1 = x1 == t.run || (t.used && x1 == t.spare) || (t.has_ext && x1 == t.ext);
        let new2 = x2 == t.run || (t.used && x2 == t.spare) || (t.has_ext && x2 == t.ext);
        if !new1 && !new2 {
            assert(apart(d0[x1].value(), d0[x2].value()));
        } else if !new1 {
            lemma_took_apart(g0, t, x1, t.model(d0, x2).unwrap());
        } else if !new2 {
            lemma_took_apart(g0, t, x2, t.model(d0, x1).unwrap());
        }
    }
}

/// The tallies after a take gain `delta`; `all` stays a list of the live
/// descriptors.
///
/// The proof's solver cost moves with unrelated items elsewhere in the
/// crate; its own solver instance (`spinoff_prover`) keeps them out of
/// its context, and the rlimit leaves it headroom.
#[verifier::spinoff_prover]
#[verifier::rlimit(40)]
proof fn lemma_took_tally(g0: Inner, g1: Inner, t: Take, k: int)
    requires
        g0.wf(),
        g1.took(g0, t),
    ensures
        tally(g1.all@, g1.descs@, k) == tally(g0.all@, g0.descs@, k) + t.delta(k),
        g1.all@.no_duplicates(),
        forall|d: *mut HugeDesc| #[trigger] g1.descs@.contains_key(d) <==> g1.all@.contains(d),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let a0 = g0.all@;
    let b = t.base(a0);
    assert(forall|x: *mut HugeDesc| #[trigger] a0.contains(x) ==> d0.contains_key(x));
    assert forall|i: int| 0 <= i < a0.len() && !(t.has_ext && a0[i] == t.ext) implies #[trigger] weight(d1[a0[i]].value(), k) == weight(
        d0[a0[i]].value(),
        k,
    ) by {
        assert(d0.contains_key(a0[i]));
        assert(a0[i]@.addr != 0);
        assert(a0[i] != t.run && !(t.used && a0[i] == t.spare));
        assert(d1.contains_key(a0[i]));
    }
    if t.has_ext && !t.head {
        let j = a0.index_of(t.ext);
        assert forall|i: int| 0 <= i < b.len() implies #[trigger] weight(d1[b[i]].value(), k) == weight(d0[b[i]].value(), k) by {
            if i < j {
                assert(b[i] == a0[i]);
                assert(a0[i] != t.ext) by {
                    assert(a0.no_duplicates());
                }
            } else {
                assert(b[i] == a0[i + 1]);
                assert(a0[i + 1] != t.ext) by {
                    assert(a0.no_duplicates());
                }
            }
        }
        lemma_tally_frame(b, d0, d1, k);
        lemma_tally_remove(a0, d0, j, k);
    } else if t.has_ext {
        assert(d1.contains_key(t.ext));
        lemma_tally_update(a0, d0, d1, t.ext, k);
    } else {
        lemma_tally_frame(a0, d0, d1, k);
    }
    let c = if t.used { b.push(t.spare) } else { b };
    assert(g1.all@ == c.push(t.run));
    if t.used {
        lemma_tally_push(b, d1, t.spare, k);
        assert(d1.contains_key(t.spare));
    }
    lemma_tally_push(c, d1, t.run, k);
    assert(d1.contains_key(t.run));
    // `all` afterwards.
    assert forall|x: *mut HugeDesc| #[trigger] b.contains(x) <==> a0.contains(x) && !(t.has_ext && !t.head && x == t.ext) by {
        if t.has_ext && !t.head {
            let j = a0.index_of(t.ext);
            if b.contains(x) {
                let i = b.index_of(x);
                let i0 = if i < j { i } else { i + 1 };
                assert(a0[i0] == x);
                assert(x != t.ext) by {
                    assert(a0.no_duplicates());
                }
            }
            if a0.contains(x) && x != t.ext {
                let i0 = a0.index_of(x);
                assert(i0 != j);
                let i = if i0 < j { i0 } else { i0 - 1 };
                assert(b[i] == x);
            }
        }
    }
    assert(b.no_duplicates()) by {
        if t.has_ext && !t.head {
            let j = a0.index_of(t.ext);
            assert forall|i1: int, i2: int| 0 <= i1 < b.len() && 0 <= i2 < b.len() && i1 != i2 implies b[i1] != b[i2] by {
                let j1 = if i1 < j { i1 } else { i1 + 1 };
                let j2 = if i2 < j { i2 } else { i2 + 1 };
                assert(b[i1] == a0[j1] && b[i2] == a0[j2]);
            }
        }
    }
    assert(c.no_duplicates());
    assert(g1.all@.no_duplicates());
    assert forall|d: *mut HugeDesc| #[trigger] d1.contains_key(d) <==> g1.all@.contains(d) by {
        assert(d1.contains_key(d) <==> t.model(d0, d) is Some);
        assert(a0.contains(d) <==> d0.contains_key(d));
        lemma_push_contains(c, t.run, d);
        if t.used {
            lemma_push_contains(b, t.spare, d);
        }
    }
}

/// The `Empty` list after a take: its length and pages are the counters,
/// and it holds every `Empty` extent.
///
/// The proof's solver cost moves with unrelated items elsewhere in the
/// crate; its own solver instance (`spinoff_prover`) keeps them out of
/// its context, and the rlimit leaves it headroom.
#[verifier::rlimit(40)]
#[verifier::spinoff_prover]
proof fn lemma_took_elist(g0: Inner, g1: Inner, t: Take)
    requires
        g0.wf(),
        g1.took(g0, t),
    ensures
        g1.stats.empty == g1.elist@.len(),
        g1.stats.empty_pages == epages(g1.elist@, g1.descs@),
        forall|d: *mut HugeDesc| #[trigger] g1.descs@.contains_key(d) && g1.descs@[d].value().state == RunState::Empty as u8
            ==> g1.elist@.contains(d),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let l0 = g0.elist@;
    assert(stat(g1.stats, 1) == stat(g0.stats, 1) + t.delta(1));
    assert(stat(g1.stats, 4) == stat(g0.stats, 4) + t.delta(4));
    // The old list's members other than `ext` keep their pages.
    assert forall|i: int| #![trigger d0.contains_key(l0[i])] #![trigger d0[l0[i]]] 0 <= i < l0.len() implies d0.contains_key(l0[i]) && d0[l0[i]].value().state == RunState::Empty as u8 by {
        assert(enode(l0, d0, i));
    }
    assert forall|i: int| 0 <= i < l0.len() && l0[i] != t.ext implies #[trigger] d1[l0[i]].value().pages == d0[l0[i]].value().pages by {
        assert(d0.contains_key(l0[i]));
        assert(l0[i] != t.run && !(t.used && l0[i] == t.spare));
        assert(d1.contains_key(l0[i]));
    }
    if t.has_ext && t.ve.state == RunState::Empty as u8 {
        assert(l0.contains(t.ext));
        let j = l0.index_of(t.ext);
        lemma_epages_update(l0, d0, d1, t.ext);
        let l1 = if t.used { l0.insert(j + 1, t.spare) } else { l0 };
        if t.used {
            lemma_epages_insert(l0, d1, j + 1, t.spare);
            assert(d1.contains_key(t.spare));
        }
        assert(l1[j] == t.ext);
        assert(l1.no_duplicates()) by {
            if t.used {
                assert(!l0.contains(t.spare)) by {
                    if l0.contains(t.spare) {
                        assert(d0.contains_key(l0[l0.index_of(t.spare)]));
                    }
                }
            }
        }
        if !t.head {
            assert(l1.index_of(t.ext) == j);
            lemma_epages_remove(l1, d1, j);
        } else {
            assert(d1.contains_key(t.ext));
        }
        assert forall|d: *mut HugeDesc| #[trigger] d1.contains_key(d) && d1[d].value().state == RunState::Empty as u8 implies g1.elist@.contains(
            d,
        ) by {
            assert(t.model(d0, d) is Some);
            if d != t.run && !(t.used && d == t.spare) && !(t.has_ext && d == t.ext) {
                assert(l0.contains(d));
                let i = l0.index_of(d);
                if t.used {
                    if i <= j {
                        assert(l1[i] == d);
                    } else {
                        assert(l1[i + 1] == d);
                    }
                }
                assert(l1.contains(d));
                if !t.head {
                    let i1 = l1.index_of(d);
                    assert(i1 != j);
                    if i1 < j {
                        assert(g1.elist@[i1] == d);
                    } else {
                        assert(g1.elist@[i1 - 1] == d);
                    }
                }
            } else if t.used && d == t.spare {
                assert(l1[j + 1] == d);
                if !t.head {
                    assert(g1.elist@[j] == d);
                }
            } else if t.has_ext && d == t.ext {
                assert(l1[j] == d);
            }
        }
    } else {
        assert forall|i: int| 0 <= i < l0.len() implies l0[i] != t.ext by {
            assert(d0.contains_key(l0[i]));
            if t.has_ext && l0[i] == t.ext {
                assert(d0[l0[i]].value().state == RunState::Empty as u8);
            }
        }
        lemma_epages_frame(l0, d0, d1);
        assert forall|d: *mut HugeDesc| #[trigger] d1.contains_key(d) && d1[d].value().state == RunState::Empty as u8 implies g1.elist@.contains(
            d,
        ) by {
            assert(t.model(d0, d) is Some);
        }
    }
}

/// A take makes the run the one new `Active` descriptor: the run
/// authorities gain its extent and its pages, which no `Active`
/// descriptor had (it is apart from every other extent).
proof fn lemma_took_active(g0: Inner, g1: Inner, t: Take)
    requires
        g0.wf(),
        g1.took(g0, t),
    ensures
        !active_runs(g0.descs@).contains((t.vr.start as int, t.vr.pages as int)),
        active_pages(g0.descs@).disjoint(vstd::set_lib::set_int_range(t.vr.start as int, t.vr.start + t.vr.pages)),
        active_runs(g1.descs@) == active_runs(g0.descs@).insert((t.vr.start as int, t.vr.pages as int)),
        active_pages(g1.descs@) == active_pages(g0.descs@).union(vstd::set_lib::set_int_range(t.vr.start as int, t.vr.start + t.vr.pages)),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    lemma_active(d0);
    lemma_active(d1);
    let er = (t.vr.start as int, t.vr.pages as int);
    let range = vstd::set_lib::set_int_range(t.vr.start as int, t.vr.start + t.vr.pages);
    // An old `Active` extent is not `ext` and keeps its value.
    assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && d0[x].value().state == RunState::Active as u8 implies {
        &&& !(t.has_ext && x == t.ext)
        &&& x != t.run
        &&& d1.contains_key(x)
        &&& same_but_links(d0[x].value(), d1[x].value())
        &&& apart(d0[x].value(), t.vr)
    } by {
        assert(t.model(d0, x) == Some(d0[x].value()));
        assert(d1.contains_key(x) <==> t.model(d0, x) is Some);
        assert(d1.contains_key(x));
        assert(same_but_links(t.model(d0, x).unwrap(), d1[x].value()));
        lemma_took_apart(g0, t, x, t.vr);
    }
    // A new `Active` extent is the run or an old one.
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && d1[x].value().state == RunState::Active as u8 implies
        x == t.run || (d0.contains_key(x) && same_but_links(d0[x].value(), d1[x].value())) by {
        assert(same_but_links(t.model(d0, x).unwrap(), d1[x].value()));
    }
    assert(d1.contains_key(t.run) && same_but_links(t.vr, d1[t.run].value())) by {
        assert(t.model(d0, t.run) == Some(t.vr));
    }
    assert forall|k: int| active_pages(d0).contains(k) implies !range.contains(k) by {
        let x = choose|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && run_page(d0[x].value(), k);
    }
    assert(!active_runs(d0).contains(er)) by {
        if active_runs(d0).contains(er) {
            let x = choose|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && run_at(d0[x].value(), er);
            assert(apart(d0[x].value(), t.vr));
        }
    }
    assert forall|e: (int, int)| active_runs(d1).contains(e) <==> active_runs(d0).insert(er).contains(e) by {
        if active_runs(d1).contains(e) {
            let x = choose|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && run_at(d1[x].value(), e);
            if x != t.run {
                assert(d0.contains_key(x) && run_at(d0[x].value(), e));
            }
        }
        if active_runs(d0).contains(e) {
            let x = choose|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && run_at(d0[x].value(), e);
            assert(d1.contains_key(x) && run_at(d1[x].value(), e));
        }
        if e == er {
            assert(d1.contains_key(t.run) && run_at(d1[t.run].value(), e));
        }
    }
    assert forall|k: int| active_pages(d1).contains(k) <==> active_pages(d0).union(range).contains(k) by {
        if active_pages(d1).contains(k) {
            let x = choose|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && run_page(d1[x].value(), k);
            if x != t.run {
                assert(d0.contains_key(x) && run_page(d0[x].value(), k));
            }
        }
        if active_pages(d0).contains(k) {
            let x = choose|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && run_page(d0[x].value(), k);
            assert(d1.contains_key(x) && run_page(d1[x].value(), k));
        }
        if range.contains(k) {
            assert(d1.contains_key(t.run) && run_page(d1[t.run].value(), k));
        }
    }
    assert(active_runs(d1) =~= active_runs(d0).insert(er));
    assert(active_pages(d1) =~= active_pages(d0).union(range));
}

/// The free-page set after a take, against `f0` before: exactly what was
/// free outside the run `[lo, hi)`, and the stretch `[mark, lo)` alignment
/// skipped at the mark (empty unless the run starts past it).
spec fn took_free(f0: vstd::set::Set<nat>, f1: vstd::set::Set<nat>, lo: int, hi: int, mark: int) -> bool {
    forall|q: nat| #[trigger] f1.contains(q) <==> (f0.contains(q) && !(lo <= q < hi)) || (mark <= q < lo)
}

/// A take keeps the free extents coalesced and marked free: the head
/// ends at the run, a tail starts at its end and ends where `ext` did,
/// a hole starts at the mark, which no free extent touched, and ends at
/// the run; their pages, and the old free extents' (apart from the run),
/// are in the free-page set, and nothing else is: an old free page outside
/// the run is in an old free extent other than `ext`, or in `ext`'s head
/// or tail, and a page skipped at the mark is in the hole or the head.
proof fn lemma_took_coalesced(g0: Inner, g1: Inner, t: Take)
    requires
        g0.wf(),
        g1.took(g0, t),
        t.touch_ok(g0),
        took_free(g0.free.map.view(), g1.free.map.view(), t.vr.start as int, t.vr.start + t.vr.pages, t.mark as int),
    ensures
        coalesced(g1.descs@),
        free_marked(g1.descs@, g1.free.map.view()),
        free_exact(g1.descs@, g1.free.map.view()),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let f0 = g0.free.map.view();
    let f1 = g1.free.map.view();
    let (vr, vh, vs, ve) = (t.vr, t.vh, t.vs, t.ve);
    // Which descriptors are old.
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && x != t.run && !(t.used && x == t.spare) && !(t.has_ext
        && x == t.ext) implies d0.contains_key(x) && same_but_links(d0[x].value(), d1[x].value()) by {
        assert(t.model(d0, x) is Some);
    }
    assert(t.model(d0, t.run) == Some(vr));
    assert(d1.contains_key(t.run));
    assert(same_but_links(vr, d1[t.run].value()));
    if t.used {
        assert(t.model(d0, t.spare) == Some(vs));
        assert(d1.contains_key(t.spare));
        assert(same_but_links(vs, d1[t.spare].value()));
    }
    if t.head {
        assert(t.model(d0, t.ext) == Some(vh));
        assert(d1.contains_key(t.ext));
        assert(same_but_links(vh, d1[t.ext].value()));
    }
    if t.has_ext {
        assert(desc_ok(ve, g0.next_page));
        assert(marked(f0, ve));
    }
    assert forall|x1: *mut HugeDesc, x2: *mut HugeDesc|
        #[trigger] d1.contains_key(x1) && #[trigger] d1.contains_key(x2) && d1[x1].value().state != RunState::Active as u8
            && d1[x2].value().state != RunState::Active as u8 implies d1[x1].value().start + d1[x1].value().pages
            != d1[x2].value().start by {
        let v1 = d1[x1].value();
        let v2 = d1[x2].value();
        let old1 = x1 != t.run && !(t.used && x1 == t.spare) && !(t.has_ext && x1 == t.ext);
        let old2 = x2 != t.run && !(t.used && x2 == t.spare) && !(t.has_ext && x2 == t.ext);
        if old1 {
            assert(desc_ok(d0[x1].value(), g0.next_page));
            if t.has_ext {
                assert(apart(d0[x1].value(), ve));
            }
        }
        if old2 {
            assert(desc_ok(d0[x2].value(), g0.next_page));
            if t.has_ext {
                assert(apart(d0[x2].value(), ve));
            }
        }
        if old1 && old2 {
            assert(d0.contains_key(x1) && d0.contains_key(x2));
        } else if old1 && t.has_ext && x2 == t.ext {
            assert(d0.contains_key(x1) && d0.contains_key(t.ext));
        } else if t.used && t.has_ext && x1 == t.spare && old2 {
            assert(d0.contains_key(t.ext) && d0.contains_key(x2));
        }
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && d1[x].value().state != RunState::Active as u8 implies marked(
        f1,
        d1[x].value(),
    ) by {
        let v = d1[x].value();
        if x != t.run && !(t.used && x == t.spare) && !(t.has_ext && x == t.ext) {
            assert(d0.contains_key(x));
            assert(marked(f0, d0[x].value()));
            lemma_took_apart(g0, t, x, vr);
            assert forall|q: int| v.start <= q < v.start + v.pages implies #[trigger] f1.contains(q as nat) by {
                assert(f0.contains(q as nat));
            }
        } else {
            assert forall|q: int| v.start <= q < v.start + v.pages implies #[trigger] f1.contains(q as nat) by {
                if t.has_ext && q < ve.start + ve.pages {
                    assert(f0.contains(q as nat));
                }
            }
        }
    }
    assert forall|q: nat| #[trigger] f1.contains(q) implies exists|x: *mut HugeDesc| #[trigger] in_free(d1, x, q as int) by {
        if f0.contains(q) && !(vr.start <= q < vr.start + vr.pages) {
            let x = choose|x: *mut HugeDesc| #[trigger] in_free(d0, x, q as int);
            if t.has_ext && x == t.ext {
                if q < vr.start {
                    assert(in_free(d1, t.ext, q as int));
                } else {
                    assert(in_free(d1, t.spare, q as int));
                }
            } else {
                assert(x != t.run && !(t.used && x == t.spare));
                assert(t.model(d0, x) == Some(d0[x].value()));
                assert(d1.contains_key(x));
                assert(same_but_links(d0[x].value(), d1[x].value()));
                assert(in_free(d1, x, q as int));
            }
        } else if t.has_ext {
            assert(desc_ok(ve, g0.next_page));
            assert(in_free(d1, t.ext, q as int));
        } else {
            assert(in_free(d1, t.spare, q as int));
        }
    }
}

/// A take keeps `Inner::wf`.
proof fn lemma_took_wf(g0: Inner, g1: Inner, t: Take)
    requires
        g0.wf(),
        g1.took(g0, t),
        g1.runs@@ == active_runs(g1.descs@),
        g1.pgs@@ == active_pages(g1.descs@),
        t.touch_ok(g0),
        took_free(g0.free.map.view(), g1.free.map.view(), t.vr.start as int, t.vr.start + t.vr.pages, t.mark as int),
        g1.mark_committed(),
    ensures
        g1.wf(),
{
    lemma_took_extents(g0, g1, t);
    lemma_took_elist(g0, g1, t);
    lemma_took_coalesced(g0, g1, t);
    assert forall|k: int| 0 <= k < 7 implies #[trigger] tally(g1.all@, g1.descs@, k) == tally(g0.all@, g0.descs@, k) + t.delta(k) by {
        lemma_took_tally(g0, g1, t, k);
    }
    // Once more outside the `forall`, for what it says about `all`.
    lemma_took_tally(g0, g1, t, 0);
    assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(g1.stats, k) == tally(g1.all@, g1.descs@, k) by {
        assert(stat(g1.stats, k) == stat(g0.stats, k) + t.delta(k));
        assert(stat(g0.stats, k) == tally(g0.all@, g0.descs@, k));
    }
    assert(g1.table.spec_count() == tally(g1.all@, g1.descs@, 6));
    // The pages sum to the mark.
    assert(t.delta(3) + t.delta(4) + t.delta(5) == t.nm - t.mark);
    assert(stat(g1.stats, 3) + stat(g1.stats, 4) + stat(g1.stats, 5) == stat(g0.stats, 3) + stat(g0.stats, 4) + stat(g0.stats, 5)
        + t.delta(3) + t.delta(4) + t.delta(5));
    // Every extent has a page, so the counts are at most the pages.
    let a1 = g1.all@;
    let d1 = g1.descs@;
    assert forall|i: int| 0 <= i < a1.len() implies (#[trigger] d1[a1[i]].value()).pages >= 1 && d1[a1[i]].value().state <= 2 by {
        assert(d1.contains_key(a1[i]));
        assert(desc_ok(d1[a1[i]].value(), g1.next_page));
    }
    lemma_tally_counts(a1, d1, 0);
    lemma_tally_counts(a1, d1, 1);
    lemma_tally_counts(a1, d1, 2);
    assert(stat(g1.stats, 0) + stat(g1.stats, 1) + stat(g1.stats, 2) <= g1.next_page);
    assert forall|d: *mut HugeDesc| #[trigger] g1.descs@.contains_key(d) && g1.descs@[d].value().state == RunState::Empty as u8
        implies g1.elist@.contains(d) by {}
    // The run is the one new `Active` descriptor; the rest keep theirs.
    assert forall|d: *mut HugeDesc| #[trigger] g1.descs@.contains_key(d) implies run_ok(g1.descs@[d].value(), g1.lg@) by {
        assert(same_but_links(t.model(g0.descs@, d).unwrap(), d1[d].value()));
    }
}

impl Inner {
    /// The invariant of the huge lock: the table is well formed and every
    /// descriptor it or the `Empty` list names is live and consistent;
    /// the live descriptors (`all`) are the extents, pairwise disjoint,
    /// each under its keys in the table and nowhere else, and the counters
    /// are their tallies, whose pages sum to `next_page`; the run
    /// authorities (`runs`, `pgs`) hold the `Active` extents and their pages;
    /// no two free extents touch; `dclaims` holds every live descriptor's
    /// block claim, from the `Meta` at `mid`; and the free-page bitmap is
    /// well formed and holds exactly the free extents' pages.
    pub(crate) closed spec fn wf(&self) -> bool {
        &&& self.table.wf()
        &&& perms_ok(self.descs@)
        &&& forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> desc_ok(self.descs@[d].value(), self.next_page)
        &&& forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> run_ok(self.descs@[d].value(), self.lg@)
        &&& forall|k: usize| #[trigger] self.table.lookup(k) matches Some(d) ==> self.descs@.contains_key(d)
        &&& elist_ok(self.empty_head, self.empty_tail, self.elist@, self.descs@)
        &&& self.next_page <= self.np@ <= u32::MAX
        &&& self.stats.empty == self.elist@.len()
        &&& self.stats.empty_pages == epages(self.elist@, self.descs@)
        &&& self.stats.run_pages + self.stats.empty_pages + self.stats.purged_pages == self.next_page
        &&& self.stats.next_page == self.next_page
        // Implied by the tallies, an extent having a page or more; stated
        // for the readers that bound the counts.
        &&& self.stats.runs + self.stats.empty + self.stats.purged <= self.next_page
        &&& self.all@.no_duplicates()
        &&& forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) <==> self.all@.contains(d)
        &&& forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> keyed(self.table, self.descs@[d].value(), d)
        &&& forall|k: usize| #[trigger] self.table.lookup(k) matches Some(d) ==> is_key(self.descs@[d].value(), k as int)
        &&& forall|d1: *mut HugeDesc, d2: *mut HugeDesc|
            #[trigger] self.descs@.contains_key(d1) && #[trigger] self.descs@.contains_key(d2) && d1 != d2 ==> apart(
                self.descs@[d1].value(),
                self.descs@[d2].value(),
            )
        &&& forall|k: int| 0 <= k < 6 ==> #[trigger] stat(self.stats, k) == tally(self.all@, self.descs@, k)
        &&& self.table.spec_count() == tally(self.all@, self.descs@, 6)
        &&& forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) && self.descs@[d].value().state == RunState::Empty as u8
            ==> self.elist@.contains(d)
        &&& self.runs@@ == active_runs(self.descs@)
        &&& self.pgs@@ == active_pages(self.descs@)
        &&& coalesced(self.descs@)
        &&& forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> desc_claimed(self.dclaims@, d, self.mid@)
        &&& self.free.map.wf()
        &&& self.free.map.in_meta()
        &&& self.free.map.spec_nbits() == self.np@
        &&& self.free.own_claimed()
        &&& self.table.spec_mid() == self.mid@
        &&& self.free.l1_words <= 0x1_0000_0000
        &&& free_marked(self.descs@, self.free.map@)
        &&& free_exact(self.descs@, self.free.map@)
        &&& self.mark_committed()
    }

    /// The huge free bitmap is exact: in every value the huge lock admits,
    /// page `q`'s free bit is set iff `q` is a page of a free (`Empty` or
    /// `Purged`) extent, and no bit at or past the high-water mark is set.
    /// `Huge::audit` checks the same at run time.
    proof fn theorem_free_exact(&self, q: int)
        requires
            self.wf(),
            0 <= q,
        ensures
            self.free.map@.contains(q as nat) <==> exists|x: *mut HugeDesc| #[trigger] in_free(self.descs@, x, q),
            q >= self.next_page ==> !self.free.map@.contains(q as nat),
    {
        if exists|x: *mut HugeDesc| #[trigger] in_free(self.descs@, x, q) {
            let x = choose|x: *mut HugeDesc| #[trigger] in_free(self.descs@, x, q);
            lemma_marked(self.free.map@, self.descs@[x].value(), q);
        }
        self.lemma_free_below();
    }

    /// No page in `[from, lim)` below the mark is free when every free
    /// extent starting below `from` ends by it and no free page in the
    /// bitmap lies in `[from, lim)`: an extent holding the page would start
    /// below `from` and end by it, or start in `[from, lim)` at a free page.
    proof fn lemma_none_free(&self, from: int, lim: int, q: int)
        requires
            self.wf(),
            ends_by(self.descs@, from),
            forall|b: nat| #[trigger] self.free.map.view().contains(b) && from <= b ==> lim <= b,
            0 <= from <= q < lim,
            q < self.next_page,
        ensures
            !page_free(self.descs@, self.next_page as int, q),
    {
        if page_free(self.descs@, self.next_page as int, q) {
            let x = choose|x: *mut HugeDesc| #[trigger] in_free(self.descs@, x, q);
            let v = self.descs@[x].value();
            assert(marked(self.free.map.view(), v));
            lemma_marked(self.free.map.view(), v, v.start as int);
        }
    }

    /// A placement of `need` free pages at `q`, below the mark and in the
    /// free extent `x`, fits in `x` or runs on past the mark from `x`'s
    /// end: the page after `x` is free and below the mark only if a free
    /// extent starts there, which coalescing rules out.
    proof fn lemma_fit_extent(&self, x: *mut HugeDesc, q: int, need: int)
        requires
            self.wf(),
            in_free(self.descs@, x, q),
            forall|p: int| q <= p < q + need ==> #[trigger] page_free(self.descs@, self.next_page as int, p),
        ensures
            q + need <= self.descs@[x].value().start + self.descs@[x].value().pages
                || self.descs@[x].value().start + self.descs@[x].value().pages == self.next_page,
    {
        let v = self.descs@[x].value();
        let p = v.start + v.pages;
        assert(desc_ok(v, self.next_page));
        if p < q + need && p != self.next_page {
            assert(page_free(self.descs@, self.next_page as int, p));
            let y = choose|y: *mut HugeDesc| #[trigger] in_free(self.descs@, y, p);
            let w = self.descs@[y].value();
            assert(apart(v, w));
            self.theorem_coalesced(x, y);
        }
    }

    /// Every free bit lies below the mark: its page is in a free extent,
    /// which ends by the mark.
    proof fn lemma_free_below(&self)
        requires
            self.wf(),
        ensures
            forall|q: nat| #[trigger] self.free.map@.contains(q) ==> q < self.next_page,
    {
        assert forall|q: nat| #[trigger] self.free.map@.contains(q) implies q < self.next_page by {
            let x = choose|x: *mut HugeDesc| #[trigger] in_free(self.descs@, x, q as int);
            assert(desc_ok(self.descs@[x].value(), self.next_page));
        }
    }

    /// The conjuncts of `wf` that `Huge::take` reads directly, for a caller
    /// that hides `wf`: all but `coalesced`, `free_marked` and `free_exact`,
    /// which `take` passes on to its lemmas as `wf` itself.
    proof fn lemma_wf_for_take(&self)
        requires
            self.wf(),
        ensures
            self.table.wf(),
            perms_ok(self.descs@),
            forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> desc_ok(self.descs@[d].value(), self.next_page),
            forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> run_ok(self.descs@[d].value(), self.lg@),
            forall|k: usize| #[trigger] self.table.lookup(k) matches Some(d) ==> self.descs@.contains_key(d),
            elist_ok(self.empty_head, self.empty_tail, self.elist@, self.descs@),
            self.next_page <= self.np@ <= u32::MAX,
            self.stats.empty == self.elist@.len(),
            self.stats.empty_pages == epages(self.elist@, self.descs@),
            self.stats.run_pages + self.stats.empty_pages + self.stats.purged_pages == self.next_page,
            self.stats.next_page == self.next_page,
            self.stats.runs + self.stats.empty + self.stats.purged <= self.next_page,
            forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) <==> self.all@.contains(d),
            forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> keyed(self.table, self.descs@[d].value(), d),
            forall|k: usize| #[trigger] self.table.lookup(k) matches Some(d) ==> is_key(self.descs@[d].value(), k as int),
            forall|d1: *mut HugeDesc, d2: *mut HugeDesc|
                #[trigger] self.descs@.contains_key(d1) && #[trigger] self.descs@.contains_key(d2) && d1 != d2 ==> apart(
                    self.descs@[d1].value(),
                    self.descs@[d2].value(),
                ),
            forall|k: int| 0 <= k < 6 ==> #[trigger] stat(self.stats, k) == tally(self.all@, self.descs@, k),
            self.table.spec_count() == tally(self.all@, self.descs@, 6),
            forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) && self.descs@[d].value().state == RunState::Empty as u8
                ==> self.elist@.contains(d),
            forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> desc_claimed(self.dclaims@, d, self.mid@),
            self.free.map.wf(),
            self.free.map.in_meta(),
            self.free.map.spec_nbits() == self.np@,
            self.free.own_claimed(),
            self.table.spec_mid() == self.mid@,
            self.free.l1_words <= 0x1_0000_0000,
            self.all@.no_duplicates(),
            self.runs@@ == active_runs(self.descs@),
            self.pgs@@ == active_pages(self.descs@),
    {
    }

    /// Huge free extents are maximally coalesced: in every value `wf`
    /// admits, no two free extents (`Empty` or
    /// `Purged`) touch, so a free extent's neighbours are runs, the
    /// high-water mark or the region's start. `Huge::audit` checks the same
    /// at run time ("adjacent free extents").
    proof fn theorem_coalesced(&self, d1: *mut HugeDesc, d2: *mut HugeDesc)
        requires
            self.wf(),
            self.descs@.contains_key(d1),
            self.descs@.contains_key(d2),
            self.descs@[d1].value().state != RunState::Active as u8,
            self.descs@[d2].value().state != RunState::Active as u8,
        ensures
            self.descs@[d1].value().start + self.descs@[d1].value().pages != self.descs@[d2].value().start,
    {
    }

    /// Guard pages are never handed out, for huge runs: in every value
    /// `wf` admits, the usable pages of a live run (`Active`, the pages before
    /// its [`GUARD_PAGES`]) share no page with the guard pages of any live
    /// run, its own included: each run's guard follows its usable pages
    /// (`run_ok`) and live extents are pairwise apart.
    proof fn theorem_run_guards(&self, d1: *mut HugeDesc, d2: *mut HugeDesc)
        requires
            self.wf(),
            self.descs@.contains_key(d1),
            self.descs@.contains_key(d2),
            self.descs@[d1].value().state == RunState::Active as u8,
            self.descs@[d2].value().state == RunState::Active as u8,
        ensures
            ({
                let (v1, v2) = (self.descs@[d1].value(), self.descs@[d2].value());
                &&& v1.pages > GUARD_PAGES
                &&& v2.pages > GUARD_PAGES
                &&& crate::layout::disjoint(
                    v1.start as int,
                    v1.pages - GUARD_PAGES,
                    v2.start + v2.pages - GUARD_PAGES,
                    GUARD_PAGES as int,
                )
            }),
    {
        assert(run_ok(self.descs@[d1].value(), self.lg@));
        assert(run_ok(self.descs@[d2].value(), self.lg@));
        if d1 != d2 {
            assert(apart(self.descs@[d1].value(), self.descs@[d2].value()));
        }
    }

    /// `d` is a live descriptor: the lock's value holds its permission,
    /// which every descriptor read and write goes through.
    pub(crate) closed spec fn has_desc(&self, d: *mut HugeDesc) -> bool {
        self.descs@.contains_key(d)
    }

    /// Slots in the descriptor table.
    pub(crate) closed spec fn table_cap(&self) -> int {
        self.table.spec_cap()
    }

    /// The address of descriptor table slot `i` (`Table::slot_addr`).
    pub(crate) closed spec fn table_slot_addr(&self, i: int) -> int {
        self.table.slot_addr(i)
    }

    /// The free-page set's bitmap.
    pub(crate) closed spec fn free_map(&self) -> crate::bitmap::TwoLevelBitmap {
        self.free.map
    }

    /// The memory the huge lock's holders write through permissions is
    /// metadata: every live descriptor ([`alloc_desc`] takes each from the
    /// process's `Meta`), every slot of the descriptor table
    /// (`Table::storage`) and every word of the free-page bitmap
    /// (`Set::new`).
    pub(crate) proof fn lemma_in_meta(&self, d: *mut HugeDesc, i: int)
        requires
            self.wf(),
        ensures
            vstd::layout::size_of::<HugeDesc>() == 48,
            self.has_desc(d) ==> desc_in_meta(d),
            crate::layout::process_regions().wf() && 0 <= i < self.table_cap() ==> crate::layout::process_regions().in_meta(self.table_slot_addr(i), 16),
            self.free_map().wf(),
            self.free_map().in_meta(),
    {
        if crate::layout::process_regions().wf() && 0 <= i < self.table_cap() {
            self.table.lemma_wf_storage();
            self.table.lemma_slots_in_meta(i);
        }
    }

    /// What a table lookup needs: a well-formed table whose entries are live
    /// descriptors (true under `wf`, and while a caller has extents out of
    /// the table).
    spec fn lookups_live(&self) -> bool {
        &&& self.table.wf()
        &&& perms_ok(self.descs@)
        &&& forall|k: usize| #[trigger] self.table.lookup(k) matches Some(d) ==> self.descs@.contains_key(d)
    }

    /// The ghost page count.
    pub(crate) closed spec fn spec_np(&self) -> usize {
        self.np@
    }

    /// The ghost page shift.
    pub(crate) closed spec fn spec_lg(&self) -> u32 {
        self.lg@
    }

    /// The ghost first byte of the region.
    pub(crate) closed spec fn spec_base(&self) -> usize {
        self.base@
    }

    /// Every byte below the mark was made read-write
    /// ([`committed`](crate::vspec::unbacked::committed)): a placement
    /// that moves the mark commits the pages it adds first
    /// ([`Huge::prepare`]), and a failed one leaves the mark where it was.
    /// The fact never expires, so it does not say a byte is still
    /// read-write: a guard made `PROT_NONE` later stays committed.
    spec fn mark_committed(&self) -> bool {
        crate::vspec::unbacked::committed_range(self.base@ as int, self.next_page * pow2(self.lg@ as nat))
    }

    /// The live-object records' identity of the `Meta` the descriptors
    /// come from ([`Inner::mid`]).
    pub(crate) closed spec fn spec_mid(&self) -> vstd::resource::Loc {
        self.mid@
    }

    /// The identity of the authority of the `Active` extents
    /// ([`Inner::runs`]), fixed at creation.
    pub(crate) closed spec fn spec_rid(&self) -> vstd::resource::Loc {
        self.runs@.id()
    }

    /// The identity of the authority of the `Active` extents' pages
    /// ([`Inner::pgs`]), fixed at creation.
    pub(crate) closed spec fn spec_pid(&self) -> vstd::resource::Loc {
        self.pgs@.id()
    }

    /// The `Empty` list's entries are live, with a page or more each,
    /// no more pages than `next_page` in all.
    proof fn lemma_elist(&self)
        requires
            self.wf(),
        ensures
            forall|i: int| 0 <= i < self.elist@.len() ==> #[trigger] self.descs@.contains_key(self.elist@[i]),
            self.elist@.len() <= epages(self.elist@, self.descs@) <= self.next_page,
    {
        assert forall|i: int| 0 <= i < self.elist@.len() implies #[trigger] self.descs@.contains_key(self.elist@[i]) by {
            assert(enode(self.elist@, self.descs@, i));
        }
        assert forall|i: int| 0 <= i < self.elist@.len() implies #[trigger] self.descs@[self.elist@[i]].value().pages
            >= 1 by {
            assert(enode(self.elist@, self.descs@, i));
        }
        lemma_epages_len(self.elist@, self.descs@);
    }

    /// The table's storage and the descriptors come from one `Meta`,
    /// whose claims `dclaims` holds.
    proof fn lemma_mid(&self)
        requires
            self.wf(),
        ensures
            self.table.spec_mid() == self.mid@,
            forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> desc_claimed(self.dclaims@, d, self.mid@),
    {
    }

    /// The list head, if any, is a live descriptor.
    proof fn lemma_head(&self)
        requires
            self.wf(),
        ensures
            self.empty_head@.addr != 0 ==> self.descs@.contains_key(self.empty_head),
    {
        if self.elist@.len() > 0 {
            assert(enode(self.elist@, self.descs@, 0));
        }
    }

    /// Everything but the descriptors' links and the list is as in `o`.
    spec fn frame(&self, o: &Inner) -> bool {
        &&& self.free == o.free
        &&& self.table == o.table
        &&& self.next_page == o.next_page
        &&& self.stats == o.stats
        &&& self.np == o.np
        &&& self.lg == o.lg
        &&& self.base == o.base
        &&& self.all == o.all
        &&& self.runs == o.runs
        &&& self.pgs == o.pgs
        &&& self.dclaims == o.dclaims
        &&& self.mid == o.mid
        &&& links_only(o.descs@, self.descs@)
    }

    /// The descriptor at `d`.
    #[inline(always)]
    fn desc(&self, d: *mut HugeDesc) -> (r: &HugeDesc)
        requires
            perms_ok(self.descs@),
            self.descs@.contains_key(d),
        ensures
            *r == self.descs@[d].value(),
    {
        ptr_ref(d, Tracked(self.descs.borrow().tracked_borrow(d)))
    }

    /// The descriptor at `d`, to write.
    #[inline(always)]
    fn desc_mut(&mut self, d: *mut HugeDesc) -> (r: &mut HugeDesc)
        requires
            perms_ok(old(self).descs@),
            old(self).descs@.contains_key(d),
        ensures
            *r == old(self).descs@[d].value(),
            final(self).descs@ == old(self).descs@.insert(d, final(self).descs@[d]),
            final(self).descs@[d].ptr() == d,
            final(self).descs@[d].is_init(),
            final(self).descs@[d].value() == *final(r),
            *final(self) == (Inner { descs: final(self).descs, ..*old(self) }),
    {
        ptr_mut_ref(d, Tracked(self.descs.borrow_mut().tracked_borrow_mut(d)))
    }

    /// Enter `d`, an initialised descriptor not in the table, into the
    /// table and the counters: a run under its start page, a free extent
    /// under its first and last page so that a free finds both
    /// neighbours. The free bitmap and the `Empty` list are the caller's.
    fn add_extent(&mut self, d: *mut HugeDesc)
        requires
            old(self).table.wf(),
            old(self).table.spec_count() + weight(old(self).descs@[d].value(), 6) < old(self).table.spec_cap()
                || crate::sys::may_abort_pending(),
            perms_ok(old(self).descs@),
            old(self).descs@.contains_key(d),
            ({
                let v = old(self).descs@[d].value();
                &&& v.state <= 2
                &&& old(self).table.lookup(v.start as usize) is None
                &&& (v.state != RunState::Active as u8 && v.pages > 1) ==> old(self).table.lookup(
                    (v.start + v.pages - 1) as usize,
                ) is None
                &&& old(self).stats.can_add(state_of(v.state), v.pages as usize)
            }),
        ensures
            final(self).table.wf(),
            final(self).table.spec_count() == old(self).table.spec_count() + weight(old(self).descs@[d].value(), 6),
            final(self).table.spec_cap() == old(self).table.spec_cap(),
            forall|k: usize| #[trigger] final(self).table.lookup(k) == if is_key(old(self).descs@[d].value(), k as int) {
                Some(d)
            } else {
                old(self).table.lookup(k)
            },
            forall|k: int| 0 <= k < 6 ==> #[trigger] stat(final(self).stats, k) == stat(old(self).stats, k) + weight(
                old(self).descs@[d].value(),
                k,
            ),
            final(self).stats.next_page == old(self).stats.next_page,
            *final(self) == (Inner { table: final(self).table, stats: final(self).stats, ..*old(self) }),
            final(self).table.spec_mid() == old(self).table.spec_mid(),
    {
        // `d` is a live descriptor owned by this allocator.
        let (start, pages, state) = (self.desc(d).start(), self.desc(d).pages(), self.desc(d).state());
        self.table.insert(start, d);
        if state.is_free() && pages > 1 {
            self.table.insert(start + pages - 1, d);
        }
        self.stats.add(state, pages);
    }

    /// Undo [`add_extent`](Inner::add_extent): take `d` out of the table
    /// and the counters. `d` itself, its list links and the free bitmap
    /// are untouched, so the caller can enter it again with new bounds or
    /// [`retire`](Inner::retire) it.
    fn drop_extent(&mut self, d: *mut HugeDesc, Ghost(others): Ghost<Seq<*mut HugeDesc>>)
        requires
            old(self).table.wf(),
            perms_ok(old(self).descs@),
            old(self).descs@.contains_key(d),
            ({
                let v = old(self).descs@[d].value();
                &&& v.state <= 2
                &&& old(self).table.lookup(v.start as usize) == Some(d)
                &&& (v.state != RunState::Active as u8 && v.pages > 1) ==> old(self).table.lookup(
                    (v.start + v.pages - 1) as usize,
                ) == Some(d)
                &&& old(self).stats.can_sub(state_of(v.state), v.pages as usize)
            }),
        ensures
            final(self).table.wf(),
            final(self).table.spec_count() == old(self).table.spec_count() - weight(old(self).descs@[d].value(), 6),
            final(self).table.spec_cap() == old(self).table.spec_cap(),
            forall|k: usize| #[trigger] final(self).table.lookup(k) == if is_key(old(self).descs@[d].value(), k as int) {
                None
            } else {
                old(self).table.lookup(k)
            },
            forall|k: int| 0 <= k < 6 ==> #[trigger] stat(final(self).stats, k) == stat(old(self).stats, k) - weight(
                old(self).descs@[d].value(),
                k,
            ),
            final(self).stats.next_page == old(self).stats.next_page,
            *final(self) == (Inner { table: final(self).table, stats: final(self).stats, ..*old(self) }),
            final(self).table.spec_mid() == old(self).table.spec_mid(),
            forall|i: int|
                #![trigger others[i]]
                0 <= i < others.len() && old(self).descs@.contains_key(others[i]) && others[i] != d ==> others[i]@.addr != d@.addr,
    {
        proof {
            lemma_addrs_apart(self.descs.borrow_mut(), d, others);
        }
        // As `add_extent`.
        let (start, pages, state) = (self.desc(d).start(), self.desc(d).pages(), self.desc(d).state());
        self.table.remove(start);
        if state.is_free() && pages > 1 {
            self.table.remove(start + pages - 1);
        }
        self.stats.sub(state, pages);
    }

    /// Give a dropped descriptor back to `meta`, leaving the `Empty` list
    /// first when it is a member (every `Empty` extent is; a freed run's
    /// descriptor still reads `Active` here and is in no list), with its
    /// permission and its block's claim.
    fn retire(&mut self, meta: &Meta, d: *mut HugeDesc, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_meta()),
            perms_ok(old(self).descs@),
            elist_ok(old(self).empty_head, old(self).empty_tail, old(self).elist@, old(self).descs@),
            old(self).descs@.contains_key(d),
            old(self).descs@[d].value().state <= 2,
            old(self).all@.contains(d),
            old(self).descs@[d].value().state == RunState::Empty as u8 ==> old(self).elist@.contains(d),
            meta.live_id() == old(self).mid@,
            forall|x: *mut HugeDesc| #[trigger] old(self).descs@.contains_key(x) ==> desc_claimed(old(self).dclaims@, x, old(self).mid@),
        ensures
            final(self).descs@.dom() == old(self).descs@.dom().remove(d),
            final(self).mid == old(self).mid,
            forall|x: *mut HugeDesc| #[trigger] final(self).descs@.contains_key(x) ==> desc_claimed(final(self).dclaims@, x, final(self).mid@),
            perms_ok(final(self).descs@),
            forall|x: *mut HugeDesc| #[trigger] final(self).descs@.contains_key(x) ==> same_but_links(
                old(self).descs@[x].value(),
                final(self).descs@[x].value(),
            ),
            final(self).elist@ == if old(self).descs@[d].value().state == RunState::Empty as u8 {
                old(self).elist@.remove(old(self).elist@.index_of(d))
            } else {
                old(self).elist@
            },
            elist_ok(final(self).empty_head, final(self).empty_tail, final(self).elist@, final(self).descs@),
            final(self).all@ == old(self).all@.remove(old(self).all@.index_of(d)),
            final(self).free == old(self).free,
            final(self).table == old(self).table,
            final(self).next_page == old(self).next_page,
            final(self).stats == old(self).stats,
            final(self).np == old(self).np,
            final(self).lg == old(self).lg,
            final(self).base == old(self).base,
            final(self).runs == old(self).runs,
            final(self).pgs == old(self).pgs,
            *final(ord) == *old(ord),
    {
        let ghost d0 = self.descs@;
        let ghost l0 = self.elist@;
        proof {
            if d0[d].value().state != RunState::Empty as u8 {
                assert(!l0.contains(d)) by {
                    if l0.contains(d) {
                        assert(enode(l0, d0, l0.index_of(d)));
                    }
                }
            }
        }
        // `d` is live and about to be discarded; its claim goes with it.
        let tracked dc = self.dclaims.borrow_mut().tracked_remove(d);
        proof {
            assert(desc_claimed(old(self).dclaims@, d, old(self).mid@));
            lemma_state_byte(d0[d].value().state);
        }
        if self.desc(d).state() == RunState::Empty {
            self.unlink(d);
        }
        let ghost d1 = self.descs@;
        let ghost l1 = self.elist@;
        proof {
            // `d` is in no list now.
            assert(!l1.contains(d)) by {
                if d0[d].value().state == RunState::Empty as u8 {
                    assert(l0.no_duplicates());
                    let j = l0.index_of(d);
                    if l1.contains(d) {
                        let i = l1.index_of(d);
                        if i < j {
                            assert(l1[i] == l0[i]);
                        } else {
                            assert(l1[i] == l0[i + 1]);
                        }
                    }
                }
            }
        }
        let tracked perm = self.descs.borrow_mut().tracked_remove(d);
        proof {
            self.all@ = self.all@.remove(self.all@.index_of(d));
            assert(self.descs@.dom() =~= d0.dom().remove(d));
            assert forall|i: int| 0 <= i < l1.len() implies #[trigger] enode(l1, self.descs@, i) by {
                assert(enode(l1, d1, i));
            }
            assert forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) implies same_but_links(
                d0[x].value(),
                self.descs@[x].value(),
            ) by {
                if d0[d].value().state == RunState::Empty as u8 {
                    assert(same_but_links(d0[x].value(), d1[x].value()));
                }
            }
        }
        free_desc(meta, d, Tracked(desc_raw((perm, dc))), Tracked(ord));
    }

    /// Append to the `Empty` list; `d.dirty_at` must be the newest.
    fn link_tail(&mut self, d: *mut HugeDesc)
        requires
            perms_ok(old(self).descs@),
            elist_ok(old(self).empty_head, old(self).empty_tail, old(self).elist@, old(self).descs@),
            old(self).descs@.contains_key(d),
            old(self).descs@[d].value().state == RunState::Empty as u8,
            !old(self).elist@.contains(d),
        ensures
            final(self).frame(old(self)),
            final(self).elist@ == old(self).elist@.push(d),
            elist_ok(final(self).empty_head, final(self).empty_tail, final(self).elist@, final(self).descs@),
    {
        let ghost l = self.elist@;
        let ghost d0 = self.descs@;
        proof {
            if l.len() > 0 {
                assert(enode(l, d0, l.len() - 1));
            }
        }
        // `d` is live and not in the list.
        self.desc_mut(d).prev = self.empty_tail;
        self.desc_mut(d).next = ptr::null_mut();
        if self.empty_tail.is_null() {
            self.empty_head = d;
        } else {
            self.desc_mut(self.empty_tail).next = d;
        }
        self.empty_tail = d;
        proof {
            let nl = l.push(d);
            self.elist@ = nl;
            assert forall|i: int| 0 <= i < nl.len() implies #[trigger] enode(nl, self.descs@, i) by {
                if i < l.len() {
                    assert(enode(l, d0, i));
                    assert(nl[i] == l[i]);
                    if i + 1 < l.len() {
                        assert(l[i] != l[l.len() - 1]) by {
                            assert(l.no_duplicates());
                        }
                    }
                }
            }
            assert(nl.no_duplicates());
            assert(self.descs@.dom() =~= d0.dom());
            assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) implies same_but_links(
                d0[x].value(),
                self.descs@[x].value(),
            ) by {}
        }
    }

    /// Insert `d` right after `after`, which is in the list.
    fn link_after(&mut self, after: *mut HugeDesc, d: *mut HugeDesc)
        requires
            perms_ok(old(self).descs@),
            elist_ok(old(self).empty_head, old(self).empty_tail, old(self).elist@, old(self).descs@),
            old(self).elist@.contains(after),
            old(self).descs@.contains_key(d),
            old(self).descs@[d].value().state == RunState::Empty as u8,
            !old(self).elist@.contains(d),
        ensures
            final(self).frame(old(self)),
            final(self).elist@ == old(self).elist@.insert(old(self).elist@.index_of(after) + 1, d),
            elist_ok(final(self).empty_head, final(self).empty_tail, final(self).elist@, final(self).descs@),
    {
        let ghost l = self.elist@;
        let ghost d0 = self.descs@;
        let ghost j = l.index_of(after);
        proof {
            assert(enode(l, d0, j));
            if j + 1 < l.len() {
                assert(enode(l, d0, j + 1));
            }
        }
        // Both are live; `after` is linked, `d` is not.
        self.desc_mut(d).prev = after;
        self.desc_mut(d).next = self.desc(after).next;
        if self.desc(after).next.is_null() {
            self.empty_tail = d;
        } else {
            self.desc_mut(self.desc(after).next).prev = d;
        }
        self.desc_mut(after).next = d;
        proof {
            let nl = l.insert(j + 1, d);
            self.elist@ = nl;
            assert forall|i: int| 0 <= i < nl.len() implies #[trigger] enode(nl, self.descs@, i) by {
                if i <= j {
                    assert(nl[i] == l[i]);
                    assert(enode(l, d0, i));
                } else if i > j + 1 {
                    assert(nl[i] == l[i - 1]);
                    assert(enode(l, d0, i - 1));
                }
            }
            assert(nl.no_duplicates());
            assert(self.descs@.dom() =~= d0.dom());
            assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) implies same_but_links(
                d0[x].value(),
                self.descs@[x].value(),
            ) by {}
        }
    }

    /// Take `d` out of the `Empty` list.
    fn unlink(&mut self, d: *mut HugeDesc)
        requires
            perms_ok(old(self).descs@),
            elist_ok(old(self).empty_head, old(self).empty_tail, old(self).elist@, old(self).descs@),
            old(self).elist@.contains(d),
        ensures
            final(self).frame(old(self)),
            final(self).elist@ == old(self).elist@.remove(old(self).elist@.index_of(d)),
            elist_ok(final(self).empty_head, final(self).empty_tail, final(self).elist@, final(self).descs@),
            final(self).descs@[d].value().prev == ptr::null_mut::<HugeDesc>(),
            final(self).descs@[d].value().next == ptr::null_mut::<HugeDesc>(),
    {
        let ghost l = self.elist@;
        let ghost d0 = self.descs@;
        let ghost j = l.index_of(d);
        proof {
            assert(enode(l, d0, j));
            if j > 0 {
                assert(enode(l, d0, j - 1));
            }
            if j + 1 < l.len() {
                assert(enode(l, d0, j + 1));
            }
        }
        // `d` is live and in the list.
        if self.desc(d).prev.is_null() {
            self.empty_head = self.desc(d).next;
        } else {
            self.desc_mut(self.desc(d).prev).next = self.desc(d).next;
        }
        if self.desc(d).next.is_null() {
            self.empty_tail = self.desc(d).prev;
        } else {
            self.desc_mut(self.desc(d).next).prev = self.desc(d).prev;
        }
        self.desc_mut(d).prev = ptr::null_mut();
        self.desc_mut(d).next = ptr::null_mut();
        proof {
            let nl = l.remove(j);
            self.elist@ = nl;
            assert forall|i: int| 0 <= i < nl.len() implies #[trigger] enode(nl, self.descs@, i) by {
                if i < j {
                    assert(nl[i] == l[i]);
                    assert(enode(l, d0, i));
                } else {
                    assert(nl[i] == l[i + 1]);
                    assert(enode(l, d0, i + 1));
                }
            }
            assert(nl.no_duplicates());
            assert(self.descs@.dom() =~= d0.dom());
            assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) implies same_but_links(
                d0[x].value(),
                self.descs@[x].value(),
            ) by {}
        }
    }

    /// The free extent starting at `page`, which must be a set free bit
    /// that no free extent starting below it covers. The check that the
    /// table has a free extent starting there is proved never to fail: the
    /// bit's page is in a free extent (`free_exact`), which starts at it.
    fn extent_at(&self, page: usize) -> (r: *mut HugeDesc)
        requires
            self.wf(),
            self.free.map@.contains(page as nat),
            ends_by(self.descs@, page as int),
        ensures
            self.descs@.contains_key(r),
            self.descs@[r].value().start == page,
            self.descs@[r].value().state != RunState::Active as u8,
    {
        let ghost x = choose|x: *mut HugeDesc| #[trigger] in_free(self.descs@, x, page as int);
        proof {
            // `x` holds `page` and starts at or below it; one starting
            // below would end by it.
            assert(keyed(self.table, self.descs@[x].value(), x));
        }
        let d = self.table.get(page);
        // Table entries are live descriptors.
        if d.is_null() || self.desc(d).start() != page || !self.desc(d).state().is_free() {
            proof {
                assert(false);
            }
            sys::abort_fmt(format_args!("huge: free page {page} has no extent"));
        }
        d
    }

    /// Neighbouring free extent ending exactly at `page`, if any: a free
    /// extent of a page or more keyed in the table that ends there is it.
    fn free_before(&self, page: usize) -> (r: *mut HugeDesc)
        requires
            states_ok(self.descs@),
            self.lookups_live(),
        ensures
            r@.addr != 0 ==> {
                &&& self.descs@.contains_key(r)
                &&& self.descs@[r].value().state != RunState::Active as u8
                &&& self.descs@[r].value().start + self.descs@[r].value().pages == page
            },
            forall|x: *mut HugeDesc|
                #[trigger] self.descs@.contains_key(x) && self.descs@[x].value().state != RunState::Active as u8
                    && self.descs@[x].value().pages >= 1 && self.descs@[x].value().start + self.descs@[x].value().pages == page
                    && keyed(self.table, self.descs@[x].value(), x) ==> r == x,
    {
        if page == 0 {
            return ptr::null_mut();
        }
        let d = self.table.get(page - 1);
        // A non-null entry is in the table, so its descriptor is live.
        if !d.is_null() && self.desc(d).state().is_free() && self.desc(d).end() == page {
            d
        } else {
            ptr::null_mut()
        }
    }

    /// Neighbouring free extent starting exactly at `page`, if any: a free
    /// extent keyed in the table that starts there is it.
    fn free_after(&self, page: usize) -> (r: *mut HugeDesc)
        requires
            states_ok(self.descs@),
            self.lookups_live(),
        ensures
            r@.addr != 0 ==> {
                &&& self.descs@.contains_key(r)
                &&& self.descs@[r].value().state != RunState::Active as u8
                &&& self.descs@[r].value().start == page
            },
            forall|x: *mut HugeDesc|
                #[trigger] self.descs@.contains_key(x) && self.descs@[x].value().state != RunState::Active as u8
                    && self.descs@[x].value().start == page && keyed(self.table, self.descs@[x].value(), x) ==> r == x,
    {
        let d = self.table.get(page);
        // As `free_before`.
        if !d.is_null() && self.desc(d).state().is_free() && self.desc(d).start() == page {
            d
        } else {
            ptr::null_mut()
        }
    }
}

} // verus!

verus! {

/// Where [`Huge::find`] put a run: its first page, its length in pages
/// (guard included) and the free extent the pages come from, null for
/// fresh pages at the high-water mark. What [`Huge::take`] records.
#[derive(Clone, Copy)]
struct Placement {
    start: usize,
    pages: usize,
    ext: *mut HugeDesc,
}

impl Placement {
    /// One past the run's last page.
    #[inline]
    fn end(&self) -> (r: usize)
        requires
            self.start + self.pages <= usize::MAX,
        ensures
            r == self.start + self.pages,
    {
        self.start + self.pages
    }
}

} // verus!

verus! {

impl Huge {
    /// Create the huge allocator for `regions`, reserving its free-page
    /// bitmap and descriptor table in `meta`. `guard_install` should be the
    /// probed `MADV_GUARD_INSTALL` support and `guards` the `guards` key
    /// (false: no guard page is placed at all, and `guard_install` is
    /// ignored). `None` if the metadata region cannot hold the tables.
    /// The run authorities `new` makes are the process's where
    /// `Shared::init` stores the allocator ([`run_loc`], [`page_loc`]).
    pub fn new(
        meta: &Meta,
        regions: &Regions,
        guard_install: bool,
        guards: bool,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: Option<Huge>)
        requires
            crate::sys::may_syscall(),
            regions.wf(),
            0 < regions.huge_len >> regions.lg_page <= u32::MAX,
            regions.huge_base + regions.huge_len <= crate::meta::va_limit(),
            (1usize << regions.lg_page) >= 16,
            meta.is_process(),
            crate::meta::process_page() == (1usize << regions.lg_page),
            old(ord).allows(rank_meta()),
        ensures
            r matches Some(h) ==> h.spec_npages() == (regions.huge_len >> regions.lg_page) as int
                && h.spec_page() == (1usize << regions.lg_page) as int
                && h.spec_base() == regions.huge_base,
            r matches Some(h) ==> h.spec_mid() == meta.live_id(),
            *final(ord) == *old(ord),
    {
        let page = regions.page_size();
        let npages = regions.huge_pages();
        assert!(npages > 0 && npages <= u32::MAX as usize, "huge: region page count out of range");
        proof {
            let lg = regions.lg_page as usize;
            assert((1usize << lg) <= 0x8000_0000_0000_0000usize) by (bit_vector)
                requires
                    lg < 64,
            ;
            assert(4 <= lg) by (bit_vector)
                requires
                    lg < 64,
                    (1usize << lg) >= 16,
            ;
            vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
            vstd::bits::lemma_usize_shl_is_mul(1, lg);
            assert(page as nat == pow2(lg as nat));
            assert(crate::meta::is_pow2(page));
        }
        let free = Set::new(meta, page, npages, Tracked(ord))?;
        let table = Table::new(meta, page, Tracked(ord))?;
        let tracked descs = Map::tracked_empty();
        // The run authorities, empty: no run is live yet.
        let tracked (runs, _) = GhostSetAuth::<(int, int)>::new(vstd::set::Set::empty());
        let tracked (pgs, _) = GhostSetAuth::<int>::new(vstd::set::Set::empty());
        proof {
            assert(active_runs(descs) =~= vstd::set::Set::empty());
            assert(active_pages(descs) =~= vstd::set::Set::empty());
        }
        proof {
            let lg = regions.lg_page;
            let p = pow2(lg as nat) as int;
            vstd::bits::lemma_usize_shr_is_div(regions.huge_len, lg as usize);
            vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(regions.huge_len as int, p);
            vstd::arithmetic::div_mod::lemma_mod_pos_bound(regions.huge_len as int, p);
            assert(npages * p <= regions.huge_len) by (nonlinear_arith)
                requires
                    regions.huge_len == p * npages + (regions.huge_len as int) % p,
                    0 <= (regions.huge_len as int) % p,
            ;
        }
        Some(Huge {
            base: regions.huge_base,
            lg_page: regions.lg_page,
            npages,
            guard_install: guard_install && guards,
            guards,
            retain_max: AtomicUsize::new(DEFAULT_RETAIN_MAX),
            purge_on_free: AtomicBool::new(false),
            mixed_guards: AtomicBool::new(false),
            gate: Decayer::new(),
            inner: Lock::new(
                Inner {
                    free,
                    table,
                    next_page: 0,
                    empty_head: ptr::null_mut(),
                    empty_tail: ptr::null_mut(),
                    stats: HugeStats {
                        runs: 0,
                        run_pages: 0,
                        empty: 0,
                        empty_pages: 0,
                        purged: 0,
                        purged_pages: 0,
                        next_page: 0,
                        table_cap: 0,
                    },
                    descs: Tracked(descs),
                    dclaims: Tracked(Map::tracked_empty()),
                    mid: Ghost(meta.live_id()),
                    elist: Ghost(Seq::empty()),
                    all: Ghost(Seq::empty()),
                    np: Ghost(npages),
                    lg: Ghost(regions.lg_page),
                    base: Ghost(regions.huge_base),
                    runs: Tracked(runs),
                    pgs: Tracked(pgs),
                },
                Ghost(rank_huge()),
            ),
        })
    }

    /// The huge lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> (r: &RawLock)
        ensures
            *r == self.spec_raw_lock(),
            r.rank() == rank_huge(),
    {
        proof {
            use_type_invariant(self);
        }
        self.inner.raw()
    }

    /// The lock [`Self::raw_lock`] returns.
    pub closed spec fn spec_raw_lock(&self) -> RawLock {
        self.inner.spec_raw()
    }

    /// The page, `2^lg_page`, and at least one page fits the region.
    proof fn lemma_page(&self)
        requires
            self.inv(),
        ensures
            self.lg_page < 64,
            0 < self.npages <= u32::MAX,
            0 < pow2(self.lg_page as nat) <= self.npages * pow2(self.lg_page as nat),
            self.base + self.npages * pow2(self.lg_page as nat) <= crate::meta::va_limit(),
            16 <= pow2(self.lg_page as nat),
            (1usize << self.lg_page) == pow2(self.lg_page as nat),
    {
        let p = pow2(self.lg_page as nat);
        vstd::arithmetic::power2::lemma2_to64();
        if self.lg_page > 4 {
            vstd::arithmetic::power2::lemma_pow2_strictly_increases(4, self.lg_page as nat);
        }
        vstd::arithmetic::power2::lemma_pow2_pos(self.lg_page as nat);
        assert(p <= self.npages * p) by (nonlinear_arith)
            requires
                self.npages >= 1,
                p > 0,
        ;
        vstd::bits::lemma_usize_shl_is_mul(1, self.lg_page as usize);
    }

    /// `x` pages of `2^lg_page` fit the region when `x <= npages`.
    proof fn lemma_pages(&self, x: usize)
        requires
            self.inv(),
            x <= self.npages,
        ensures
            x * pow2(self.lg_page as nat) <= self.npages * pow2(self.lg_page as nat),
            self.base + x * pow2(self.lg_page as nat) <= crate::meta::va_limit(),
            (x << self.lg_page) == x * pow2(self.lg_page as nat),
    {
        self.lemma_page();
        let p = pow2(self.lg_page as nat);
        assert(x * p <= self.npages * p) by (nonlinear_arith)
            requires
                x <= self.npages,
                p >= 0,
        ;
        vstd::bits::lemma_usize_shl_is_mul(x, self.lg_page as usize);
    }

    /// Page `q` of pages `x..y` starts among their bytes, so it is
    /// [`committed`](crate::vspec::unbacked::committed) when they all are.
    proof fn lemma_page_in(&self, x: int, y: int, q: int)
        requires
            self.inv(),
            0 <= x <= q < y,
        ensures
            ({
                let p = pow2(self.lg_page as nat) as int;
                crate::vspec::unbacked::committed_range(self.base + x * p, (y - x) * p) ==> crate::vspec::unbacked::committed(self.base + q * p)
            }),
    {
        let p = pow2(self.lg_page as nat) as int;
        vstd::arithmetic::power2::lemma_pow2_pos(self.lg_page as nat);
        vstd::arithmetic::mul::lemma_mul_inequality(x, q, p);
        vstd::arithmetic::mul::lemma_mul_inequality(q + 1, y, p);
        assert((q + 1) * p == q * p + p) by (nonlinear_arith);
        assert(x * p + (y - x) * p == y * p) by (nonlinear_arith);
    }

    /// The bytes of pages `0..w` and of pages `lo..e`, with `lo <= w`,
    /// cover the bytes of pages `0..e`: what the mark covers once a
    /// placement commits `lo..e` and moves the mark from `w` to `e`.
    proof fn lemma_mark_grows(&self, w: int, lo: int, e: int)
        requires
            self.inv(),
            0 <= lo <= w <= e,
        ensures
            ({
                let p = pow2(self.lg_page as nat) as int;
                crate::vspec::unbacked::committed_range(self.base as int, w * p) && crate::vspec::unbacked::committed_range(
                    self.base + lo * p,
                    (e - lo) * p,
                ) ==> crate::vspec::unbacked::committed_range(self.base as int, e * p)
            }),
    {
        let p = pow2(self.lg_page as nat) as int;
        vstd::arithmetic::power2::lemma_pow2_pos(self.lg_page as nat);
        vstd::arithmetic::mul::lemma_mul_inequality(lo, w, p);
        assert(lo * p + (e - lo) * p == e * p) by (nonlinear_arith);
    }

    /// Pages in the region.
    pub closed spec fn spec_npages(&self) -> int {
        self.npages as int
    }

    /// The page size.
    pub closed spec fn spec_page(&self) -> int {
        pow2(self.lg_page as nat) as int
    }

    /// The live-object records' identity of the `Meta` this allocator's
    /// descriptors and table come from ([`Inner::spec_mid`], fixed at
    /// creation): the `meta` its callers pass.
    pub closed spec fn spec_mid(&self) -> vstd::resource::Loc {
        lock_init(self.inner).spec_mid()
    }

    /// The identity of this allocator's authority of the live runs'
    /// extents ([`Inner::spec_rid`], fixed at creation).
    pub closed spec fn spec_run_loc(&self) -> vstd::resource::Loc {
        lock_init(self.inner).spec_rid()
    }

    /// The identity of this allocator's authority of the live runs' pages
    /// ([`Inner::spec_pid`], fixed at creation).
    pub closed spec fn spec_page_loc(&self) -> vstd::resource::Loc {
        lock_init(self.inner).spec_pid()
    }

    /// The run authorities are the process's ([`run_loc`], [`page_loc`]):
    /// the run tokens it hands out and takes back are of its own
    /// authorities.
    pub open spec fn runs_process(&self) -> bool {
        &&& self.spec_run_loc() == run_loc()
        &&& self.spec_page_loc() == page_loc()
    }

    /// A value of the huge lock has the process's run authorities when the
    /// allocator does.
    proof fn lemma_run_ids(&self, g: Inner)
        requires
            self.runs_process(),
            lock_frame(lock_init(self.inner), g),
        ensures
            g.runs@.id() == run_loc(),
            g.pgs@.id() == page_loc(),
    {
    }

    /// `a` is what [`Huge::alloc_run`] returns for `size` bytes at `align`:
    /// a page start of the region, a multiple of `align`, with `size` bytes
    /// and at least a page of the region from it on.
    pub open spec fn spec_run_at(&self, a: usize, size: usize, align: usize) -> bool {
        let end = self.spec_base() + self.spec_npages() * self.spec_page();
        &&& self.spec_base() <= a
        &&& (a - self.spec_base()) % self.spec_page() == 0
        &&& a + size <= end
        &&& a + self.spec_page() <= end
        &&& (a as int) % (align as int) == 0
    }

    /// `t[0]` is the token of the run `r` [`Huge::alloc_run`] placed: of
    /// the run's first page and page count, inside the region.
    pub open spec fn run_token(&self, t: Map<int, RunToken>, r: Placed) -> bool {
        &&& t.contains_key(0)
        &&& t[0].wf()
        &&& self.spec_base() + t[0].start() * self.spec_page() == r.ptr@.addr
        &&& t[0].npages() == r.pages
        &&& t[0].start() + t[0].npages() <= self.spec_npages()
    }

    /// A run of `usable` pages (enough for `size` bytes) and its guard at
    /// the start `s` [`Huge::find`] returned is a [`Huge::spec_run_at`].
    proof fn lemma_run_at(&self, s: usize, usable: usize, size: usize, align: usize)
        requires
            self.inv(),
            crate::meta::is_pow2(align),
            usable >= 1,
            size <= usable * pow2(self.lg_page as nat),
            (self.base as int) % (pow2(self.lg_page as nat) as int) == 0,
            s + usable + GUARD_PAGES <= self.npages,
            align > pow2(self.lg_page as nat) ==> (self.base + s * pow2(self.lg_page as nat)) % (align as int) == 0,
        ensures
            self.spec_run_at((self.base + s * pow2(self.lg_page as nat)) as usize, size, align),
    {
        self.lemma_page();
        let p = pow2(self.lg_page as nat) as int;
        let a = self.base + s * p;
        let n = self.npages as int;
        assert(s * p + size <= n * p && s * p + p <= n * p) by (nonlinear_arith)
            requires
                size <= usable * p,
                s + usable + 1 <= n,
                usable >= 1,
                p > 0,
                s >= 0,
        ;
        assert(0 <= s * p) by (nonlinear_arith)
            requires s >= 0, p > 0;
        vstd::arithmetic::div_mod::lemma_mod_multiples_basic(s as int, p);
        if align <= p {
            let k = choose|k: nat| k < 64 && align as nat == #[trigger] pow2(k);
            vstd::arithmetic::power2::lemma_pow2_pos(k);
            vstd::arithmetic::div_mod::lemma_mod_self_0(align as int);
            crate::layout::lemma_pow2_divides_pow2(align, align as int, self.lg_page as nat);
            crate::layout::lemma_mod_lin3(self.base as int, s as int, p, 0, 0, p);
            crate::layout::lemma_aligned_down(a, p, align as int);
        }
    }

    /// The address of page 0.
    pub closed spec fn spec_base(&self) -> usize {
        self.base
    }

    /// The base is a page start (the huge base is `MAX_SLAB`-aligned,
    /// `Regions::separated`).
    pub open spec fn spec_base_aligned(&self) -> bool {
        (self.spec_base() as int) % self.spec_page() == 0
    }

    /// Byte `b` is one a huge purge may discard while the run-page
    /// authority holds `held`: [`free_page_byte`] over this region.
    spec fn purgeable(&self, held: vstd::set::Set<int>, b: int) -> bool {
        free_page_byte(self.base as int, self.spec_page(), self.npages as int, held, b)
    }

    /// This is the process's huge allocator: its pages are the process's
    /// huge region's (base, page and page count), its base a page start,
    /// the process's regions are well formed and separated
    /// (`heap::regions_ok`), and its run authorities are the process's
    /// ([`Huge::runs_process`]). Closed: [`Huge::lemma_purge_range`] reads it.
    pub closed spec fn is_process(&self) -> bool {
        let r = crate::layout::process_regions();
        &&& crate::heap::regions_ok()
        &&& self.spec_base() == r.huge_base
        &&& self.spec_page() == (1usize << r.lg_page) as int
        &&& self.spec_npages() == (r.huge_len >> r.lg_page) as int
        &&& self.spec_base_aligned()
        &&& self.runs_process()
    }

    /// [`Huge::is_process`] from its parts.
    pub proof fn lemma_is_process(&self)
        requires
            crate::heap::regions_ok(),
            self.spec_base() == crate::layout::process_regions().huge_base,
            self.spec_page() == (1usize << crate::layout::process_regions().lg_page) as int,
            self.spec_npages() == (crate::layout::process_regions().huge_len >> crate::layout::process_regions().lg_page) as int,
            self.spec_base_aligned(),
            self.runs_process(),
        ensures
            self.is_process(),
    {
    }

    /// The process's huge allocator has the process's run authorities.
    proof fn lemma_process_runs(&self)
        requires
            self.is_process(),
        ensures
            self.runs_process(),
    {
    }

    /// Pages `start..start + pages` of the process's huge allocator, at
    /// least one, make a range a purge may discard
    /// ([`well_formed`](crate::purge::well_formed), whole-allocator theorem
    /// "Purge batches are well formed"): page-aligned at both ends and
    /// inside the huge region.
    proof fn lemma_purge_range(&self, start: int, pages: int)
        requires
            self.is_process(),
            0 <= start,
            pages >= 1,
            start + pages <= self.spec_npages(),
        ensures
            crate::purge::well_formed(self.spec_base() + start * self.spec_page(), pages * self.spec_page()),
    {
        reveal(crate::purge::well_formed);
        let r = crate::layout::process_regions();
        let lg = r.lg_page;
        let p = self.spec_page();
        let base = self.spec_base() as int;
        assert(12 <= lg <= crate::layout::LG_MAX_SLAB);
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg as usize);
        vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
        vstd::bits::lemma_usize_shr_is_div(r.huge_len, lg as usize);
        assert(p == crate::purge::page_bytes() && p > 0);
        // The ends are page multiples.
        vstd::arithmetic::div_mod::lemma_mod_multiples_basic(start, p);
        vstd::arithmetic::div_mod::lemma_mod_multiples_basic(pages, p);
        vstd::arithmetic::div_mod::lemma_add_mod_noop(base, start * p, p);
        vstd::arithmetic::div_mod::lemma_small_mod(0, p as nat);
        assert(pages * p > 0) by (nonlinear_arith)
            requires pages >= 1, p > 0;
        // The pages lie in the region: `npages * p <= huge_len`.
        let np = self.spec_npages();
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(r.huge_len as int, p);
        vstd::arithmetic::div_mod::lemma_mod_bound(r.huge_len as int, p);
        assert(np * p <= r.huge_len) by (nonlinear_arith)
            requires
                np == r.huge_len as int / p,
                r.huge_len as int == p * (r.huge_len as int / p) + r.huge_len as int % p,
                r.huge_len as int % p >= 0,
        ;
        assert(0 <= start * p && start * p + pages * p <= np * p) by (nonlinear_arith)
            requires 0 <= start, start + pages <= np, p > 0;
        assert forall|b: int| base + start * p <= b < base + start * p + pages * p
            implies #[trigger] crate::purge::discardable(b) by {
            assert(crate::purge::huge_byte(b));
        }
    }

    /// The usable bytes of a run for `size`: `size` rounded up to a page,
    /// and at least one page.
    pub open spec fn spec_usable(&self, size: usize) -> int {
        let n = (size + self.spec_page() - 1) / self.spec_page();
        (if n == 0 { 1 } else { n }) * self.spec_page()
    }

    /// The run of `usable` usable pages from page `s` of the process's huge
    /// allocator, at `p`, is a block of the huge region in the process's
    /// terms (`heap::live::Live::from_run`'s premises, for
    /// [`Huge::check_zero`]): `p` is the address of page `s`, and the
    /// usable pages lie in the huge region.
    proof fn lemma_run_block(&self, s: usize, usable: usize, p: *mut u8)
        requires
            self.is_process(),
            usable >= 1,
            s + usable <= self.spec_npages(),
            p@.addr == self.spec_base() + s * self.spec_page(),
        ensures
            crate::heap::regions_ok(),
            crate::heap::live::page_addr(s as int) == p@.addr as int,
            crate::layout::process_regions().is_huge_block(p@.addr, crate::heap::live::run_usable(usable + GUARD_PAGES)),
    {
        let r = crate::layout::process_regions();
        let lg = r.lg_page;
        let pg = self.spec_page();
        assert(12 <= lg <= crate::layout::LG_MAX_SLAB);
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg as usize);
        vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
        vstd::bits::lemma_usize_shr_is_div(r.huge_len, lg as usize);
        assert(pg == crate::heap::live::page_size() && pg > 0);
        let (si, u) = (s as int, usable as int);
        // The run starts on a page.
        vstd::arithmetic::div_mod::lemma_mod_multiples_basic(si, pg);
        assert(usable * pg > 0) by (nonlinear_arith)
            requires usable >= 1, pg > 0;
        // The run lies in the region: `npages * pg <= huge_len`.
        let np = self.spec_npages();
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(r.huge_len as int, pg);
        vstd::arithmetic::div_mod::lemma_mod_bound(r.huge_len as int, pg);
        assert(np * pg <= r.huge_len) by (nonlinear_arith)
            requires
                np == r.huge_len as int / pg,
                r.huge_len as int == pg * (r.huge_len as int / pg) + r.huge_len as int % pg,
                r.huge_len as int % pg >= 0,
        ;
        assert(0 <= si * pg && si * pg + u * pg <= np * pg) by (nonlinear_arith)
            requires 0 <= si, si + u <= np, pg > 0;
        assert(crate::heap::live::run_usable(usable + GUARD_PAGES) == u * pg);
    }

    #[inline]
    pub fn page_size(&self) -> (r: usize)
        ensures
            r == self.spec_page(),
            r > 0,
            crate::meta::is_pow2(r),
    {
        proof {
            use_type_invariant(self);
            self.lemma_page();
            assert((1usize << self.lg_page) as nat == pow2(self.lg_page as nat));
        }
        1 << self.lg_page
    }

    /// Pages in the region.
    #[cfg(test)]
    pub fn npages(&self) -> usize {
        self.npages
    }

    /// Usable-byte threshold above which freed runs are purged at once.
    pub fn retain_max(&self) -> usize {
        self.retain_max.load(Ordering::Relaxed)
    }

    pub fn set_retain_max(&self, bytes: usize) {
        self.retain_max.store(bytes, Ordering::Relaxed);
    }

    /// Whether a freed run is purged as it is freed (`huge_dirty_decay_ms:0`).
    #[inline]
    pub fn purge_on_free(&self) -> bool {
        self.purge_on_free.load(Ordering::Relaxed)
    }

    pub fn set_purge_on_free(&self, on: bool) {
        self.purge_on_free.store(on, Ordering::Relaxed);
    }

    /// The address of page `page`, inside the region (or its end).
    #[inline]
    fn addr(&self, page: usize) -> (r: usize)
        requires
            page <= self.npages,
        ensures
            r == self.base + page * pow2(self.lg_page as nat),
            r <= crate::meta::va_limit(),
    {
        proof {
            use_type_invariant(self);
            self.lemma_pages(page);
        }
        self.base + (page << self.lg_page)
    }

    /// Discard pages `s..hi` again with `MADV_DONTNEED`, for a run `take`
    /// places in a `Purged` extent: whether they now read as zero. The
    /// extent's earlier discard does not make them zero any more: they are
    /// read-write and were handed out before, so a write through a stale
    /// pointer, or an underflow from the live run above (a freed run's
    /// guard goes with it), reaches them after it (SECURITY.md, "Zeroing
    /// allocations are zero"). Called under the huge lock, before the run
    /// is handed out, so nothing challoc does writes them in between.
    /// Fewer than [`REDISCARD_MIN`] bytes are left to the caller's
    /// `memset` (false), which is cheaper than the call; so is any range
    /// the kernel refuses (`mlock`ed pages). `take` calls it only while
    /// `purge::purged_is_zero` holds, so the pages it drops are just the
    /// ones a stale write faulted in. True says `s < hi <= npages` and
    /// that the kernel answered the `MADV_DONTNEED` of exactly pages
    /// `s..hi` with success.
    fn rediscard(&self, s: usize, hi: usize) -> (r: bool)
        requires
            crate::sys::may_syscall(),
        ensures
            r ==> s < hi <= self.npages && sys::madvise_ok(
                (self.base + s * self.spec_page()) as usize,
                ((hi - s) * self.spec_page()) as usize,
                sys::MADV_DONTNEED,
            ),
    {
        if hi <= s || hi > self.npages {
            return false;
        }
        let Some(len) = (hi - s).checked_mul(self.page_size()) else { return false };
        if len < REDISCARD_MIN {
            return false;
        }
        let addr = self.addr(s);
        // SAFETY: pages `s..hi` lie in the region (`hi <= npages`) and
        // belong to the run being placed, which nobody holds yet: their
        // contents are not needed.
        unsafe { sys::madvise(addr, len, sys::MADV_DONTNEED) }.is_ok()
    }

    /// Usable pages for a request, or `None` if it cannot be represented.
    #[inline]
    pub fn usable_pages(&self, size: usize) -> (r: Option<usize>)
        ensures
            r matches Some(n) ==> n >= 1 && size <= n * self.spec_page() && n * self.spec_page() <= usize::MAX,
            r matches Some(n) ==> n * self.spec_page() == self.spec_usable(size),
            r is None <==> size + self.spec_page() - 1 > usize::MAX,
    {
        proof {
            use_type_invariant(self);
            self.lemma_page();
        }
        let n = size.checked_add(self.page_size() - 1)? >> self.lg_page;
        proof {
            let p = pow2(self.lg_page as nat) as int;
            let y = (size + p - 1) as usize;
            vstd::bits::lemma_usize_shr_is_div(y, self.lg_page as usize);
            assert(n as int == y as int / p);
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(y as int, p);
            vstd::arithmetic::div_mod::lemma_mod_pos_bound(y as int, p);
            assert(size <= n * p && n * p <= y) by (nonlinear_arith)
                requires
                    y == size + p - 1,
                    y == p * n + (y as int) % p,
                    0 <= (y as int) % p < p,
            ;
            if n == 0 {
                assert(n * p == 0) by (nonlinear_arith)
                    requires
                        n == 0,
                ;
            }
            assert(p <= usize::MAX);
            if n == 0 {
                assert(size <= 1 * p && 1 * p <= usize::MAX);
            }
        }
        Some(n.max(1))
    }

    /// Usable bytes `alloc(size, ..)` would give: `nallocx` for huge sizes.
    #[inline]
    pub fn usable_for(&self, size: usize) -> (r: Option<usize>)
        ensures
            r matches Some(b) ==> size <= b,
            r matches Some(b) ==> b == self.spec_usable(size),
            r is None <==> size + self.spec_page() - 1 > usize::MAX,
    {
        proof {
            use_type_invariant(self);
            self.lemma_page();
        }
        let n = self.usable_pages(size);
        proof {
            if let Some(n) = n {
                vstd::bits::lemma_usize_shl_is_mul(n, self.lg_page as usize);
            }
        }
        Some(n? << self.lg_page)
    }

    /// Page `q` starts at a multiple of `align` (every page does when
    /// `align` is at most a page: the base is a page start).
    spec fn aligned_at(&self, q: int, align: usize) -> bool {
        align <= pow2(self.lg_page as nat) || (self.base + q * pow2(self.lg_page as nat)) % (align as int) == 0
    }

    /// `need` pages at `q` are a placement [`Huge::find`] could return for
    /// `align` under the huge lock's value `g`: aligned, inside the region,
    /// and every page free (a free extent's, or at or past the mark).
    spec fn fits(&self, g: &Inner, q: int, need: int, align: usize) -> bool {
        &&& 0 <= q
        &&& q + need <= self.npages
        &&& self.aligned_at(q, align)
        &&& forall|p: int| q <= p < q + need ==> #[trigger] page_free(g.descs@, g.next_page as int, p)
    }

    /// A placement that [`fits`](Huge::fits) at `s` is apart from every
    /// live run ([`runs_apart`]): its pages are free, past the mark or in
    /// a free extent, which is apart from every run.
    proof fn lemma_fits_apart(&self, g: &Inner, s: int, need: int, align: usize)
        requires
            g.wf(),
            self.fits(g, s, need, align),
            need >= 1,
        ensures
            runs_apart(*g, s, need, g.next_page as int),
    {
        g.lemma_wf_for_take();
        assert(!g.descs@.contains_key(vstd::raw_ptr::ptr_null_mut())) by {
            if g.descs@.contains_key(vstd::raw_ptr::ptr_null_mut()) {
                assert(vstd::raw_ptr::ptr_null_mut::<HugeDesc>()@.addr == 0);
            }
        }
        assert forall|d: *mut HugeDesc|
            #[trigger] g.descs@.contains_key(d) && g.descs@[d].value().state == RunState::Active as u8 implies {
                let v = g.descs@[d].value();
                &&& v.pages >= GUARD_PAGES
                &&& v.start + v.pages <= g.next_page as int
                &&& (v.start + v.pages <= s || s + need <= v.start)
            } by {
            let v = g.descs@[d].value();
            assert(desc_ok(v, g.next_page));
            assert(run_ok(v, g.lg@));
            if !(v.start + v.pages <= s || s + need <= v.start) {
                let p = if s <= v.start { v.start as int } else { s };
                assert(page_free(g.descs@, g.next_page as int, p));
                let x = choose|x: *mut HugeDesc| #[trigger] in_free(g.descs@, x, p);
                assert(apart(g.descs@[x].value(), g.descs@[d].value()));
            }
        }
    }

    /// No placement starts below `s` when none starts below `from`, every
    /// free extent starting below `from` ends by it, no free page lies in
    /// `[from, a)`, and no page in `[a, s)` is aligned: a placement in
    /// `[from, a)` would hold a page that is not free.
    proof fn lemma_below(&self, g: &Inner, from: int, a: int, s: int, need: int, align: usize)
        requires
            g.wf(),
            need >= 1,
            0 <= from <= a <= g.next_page,
            ends_by(g.descs@, from),
            forall|q: int| 0 <= q < from ==> !#[trigger] self.fits(g, q, need, align),
            forall|b: nat| #[trigger] g.free.map.view().contains(b) && from <= b ==> a <= b,
            forall|q: int| a <= q < s ==> !#[trigger] self.aligned_at(q, align),
        ensures
            forall|q: int| 0 <= q < s ==> !#[trigger] self.fits(g, q, need, align),
    {
        assert forall|q: int| 0 <= q < s implies !#[trigger] self.fits(g, q, need, align) by {
            if from <= q < a {
                g.lemma_none_free(from, a, q);
            }
        }
    }

    /// [`run_placeable`] over this region: a run for `size` bytes at
    /// `align` and its guard page have a placement in the region when it is
    /// empty ([`Huge::placeable`]).
    pub open spec fn spec_placeable(&self, size: usize, align: usize) -> bool {
        run_placeable(self.spec_base() as int, self.spec_page(), self.spec_npages(), size as int, align as int)
    }

    /// Pages a run for `size` bytes at `align` takes, its guard included, or
    /// `None` when no placement in the region can hold it: the size cannot
    /// be represented, the run is longer than the region, or `align` is
    /// larger than the region. [`Huge::alloc_run`] and [`Huge::placeable`]
    /// share these checks.
    fn run_pages(&self, size: usize, align: usize) -> (r: Option<usize>)
        ensures
            r matches Some(need) ==> need == run_need(self.spec_page(), size as int) && need <= self.npages,
            r matches Some(need) ==> need > GUARD_PAGES && size <= (need - GUARD_PAGES) * self.spec_page() && (need - GUARD_PAGES) * self.spec_page() <= usize::MAX,
            r matches Some(need) ==> (need - GUARD_PAGES) * self.spec_page() == self.spec_usable(size),
            r matches Some(_) ==> size + self.spec_page() - 1 <= usize::MAX && align <= self.npages * self.spec_page(),
            r is None ==> !self.spec_placeable(size, align),
    {
        proof {
            use_type_invariant(self);
            self.lemma_page();
            self.lemma_pages(self.npages);
        }
        let n = self.usable_pages(size)?;
        proof {
            let p = self.spec_page();
            assert(n < usize::MAX) by (nonlinear_arith)
                requires
                    n * p <= usize::MAX,
                    p >= 16,
            ;
        }
        let need = n.checked_add(GUARD_PAGES)?;
        proof {
            let p = self.spec_page();
            let n = (size + p - 1) / p;
            let m = if n == 0 { 1 } else { n };
            // `usable_pages` gave `need - 1` pages of `spec_usable(size)`
            // bytes, which is `m` pages.
            assert((need - 1) * p == m * p);
            vstd::arithmetic::mul::lemma_mul_is_commutative(m, p);
            vstd::arithmetic::div_mod::lemma_div_multiples_vanish(m, p);
            vstd::arithmetic::div_mod::lemma_div_multiples_vanish((need - 1) as int, p);
            vstd::arithmetic::mul::lemma_mul_is_commutative((need - 1) as int, p);
        }
        if need > self.npages || align > (self.npages << self.lg_page) {
            return None;
        }
        Some(need)
    }

    /// Whether [`Huge::alloc_run`] can place a run for `size` bytes at
    /// `align` in an empty region: [`Huge::run_pages`] holds, and the run
    /// fits from the first `align`-aligned page. Aligned starts repeat every
    /// `align` bytes, so if the first one leaves no room, none does.
    pub fn placeable(&self, size: usize, align: usize) -> (r: bool)
        requires
            self.spec_base_aligned(),
            crate::meta::is_pow2(align),
        ensures
            r == self.spec_placeable(size, align),
    {
        proof {
            use_type_invariant(self);
            self.lemma_page();
            let k = choose|k: nat| k < 64 && align as nat == #[trigger] pow2(k);
            vstd::bits::lemma_usize_pow2_no_overflow(k);
            vstd::bits::lemma_usize_shl_is_mul(1, k as usize);
            let ku = k as usize;
            assert((1usize << ku) > 0 && (1usize << ku) & sub(1usize << ku, 1) == 0) by (bit_vector)
                requires ku < 64;
        }
        // `run_pages(size, align).is_some_and(|need| ...)`, spelled as a
        // `match`.
        match self.run_pages(size, align) {
            Some(need) => {
                let s = self.align_page(0, align);
                proof {
                    let ghost p = self.spec_page();
                    let ghost need_i = run_need(p, size as int);
                    // A placement starts at an aligned page, so at or past
                    // the first one, `s`.
                    if self.spec_placeable(size, align) {
                        let q = choose|q: int| 0 <= q && q + need_i <= self.npages && #[trigger] page_aligned(self.base as int, p, q, align as int);
                        assert(self.aligned_at(q, align));
                    }
                    assert(page_aligned(self.base as int, p, s as int, align as int));
                }
                s + need <= self.npages
            },
            None => false,
        }
    }

    /// The huge half of `nallocx`'s zero: a placeable request
    /// ([`Huge::spec_placeable`]) has a fit ([`Huge::fits`]) under a lock
    /// value with nothing placed yet (the mark at page 0), so
    /// [`Huge::find`], whose `None` means no page fits, places it;
    /// and [`Huge::alloc_run`] ensures every run it places was placeable,
    /// so an unplaceable request fails in every state.
    proof fn theorem_placeable_fits_empty(&self, g: &Inner, size: usize, align: usize)
        requires
            g.next_page == 0,
            self.spec_placeable(size, align),
        ensures
            exists|q: int| self.fits(g, q, run_need(self.spec_page(), size as int), align),
    {
        let need = run_need(self.spec_page(), size as int);
        let q = choose|q: int| 0 <= q && q + need <= self.npages && #[trigger] page_aligned(self.base as int, self.spec_page(), q, align as int);
        assert(self.aligned_at(q, align));
        assert(self.fits(g, q, need, align));
    }

    /// First page at or after `page` whose address is a multiple of
    /// `align` (a power of two).
    #[inline]
    fn align_page(&self, page: usize, align: usize) -> (r: usize)
        requires
            (self.base as int) % (pow2(self.lg_page as nat) as int) == 0,
            page <= self.npages,
            align > 0,
            align & sub(align, 1) == 0,
            align <= self.npages * pow2(self.lg_page as nat),
        ensures
            page <= r <= 2 * self.npages,
            align > pow2(self.lg_page as nat) ==> (self.base + r * pow2(self.lg_page as nat)) % (align as int) == 0,
            // The first such page: none in `[page, r)` is aligned.
            forall|q: int| page <= q < r ==> !#[trigger] self.aligned_at(q, align),
    {
        proof {
            use_type_invariant(self);
            self.lemma_page();
        }
        if align <= self.page_size() {
            return page;
        }
        let a = self.addr(page);
        proof {
            crate::meta::lemma_align_up(a, align);
        }
        let aligned = (a + align - 1) & !(align - 1);
        proof {
            let p = pow2(self.lg_page as nat) as int;
            let d = (aligned - self.base) as usize;
            let n2 = 2 * self.npages as int;
            vstd::bits::lemma_usize_shr_is_div(d, self.lg_page as usize);
            assert(aligned == crate::meta::align_up_spec(a, align));
            assert(page * p <= d < n2 * p) by (nonlinear_arith)
                requires
                    d == aligned - self.base,
                    a == self.base + page * p,
                    a <= aligned < a + align,
                    align <= self.npages * p,
                    page <= self.npages,
                    n2 == 2 * self.npages,
                    p > 0,
            ;
            assert(page <= d as int / p) by {
                vstd::arithmetic::div_mod::lemma_div_is_ordered(p * page, d as int, p);
                vstd::arithmetic::div_mod::lemma_div_multiples_vanish(page as int, p);
            }
            assert(d as int / p <= n2) by {
                vstd::arithmetic::div_mod::lemma_div_multiples_vanish(n2, p);
                vstd::arithmetic::div_mod::lemma_div_is_ordered(d as int, p * n2, p);
            }
            let (lg32, lg) = (self.lg_page, self.lg_page as usize);
            assert(d >> lg32 == d >> lg) by (bit_vector)
                requires lg == lg32 as usize, lg32 < 64;
            // The start is `aligned` itself: `align` is a multiple of the page,
            // and so are `aligned` and the base.
            assert(self.base + (d as int / p) * p == aligned) by {
                let pu = 1usize << lg32;
                assert(pu > 0 && pu & sub(pu, 1) == 0) by (bit_vector)
                    requires pu == 1usize << lg32, lg32 < 64;
                assert(align & sub(pu, 1) == 0) by (bit_vector)
                    requires align & sub(align, 1) == 0, pu > 0, pu & sub(pu, 1) == 0, align >= pu;
                assert(align % pu == align & sub(pu, 1)) by (bit_vector)
                    requires pu > 0, pu & sub(pu, 1) == 0;
                crate::layout::lemma_aligned_down(aligned as int, align as int, p);
                crate::layout::lemma_mod_lin3(aligned as int, -1, self.base as int, 0, 0, p);
                vstd::arithmetic::div_mod::lemma_fundamental_div_mod(d as int, p);
                assert((d as int / p) * p == d) by (nonlinear_arith)
                    requires d == p * (d as int / p) + (d as int) % p, (d as int) % p == 0;
            }
            // A page in `[page, r)` starts in `[a, aligned)`, below the
            // first multiple of `align` at or past `a`.
            assert forall|q: int| page <= q < d as int / p implies !#[trigger] self.aligned_at(q, align) by {
                assert(page * p <= q * p < (d as int / p) * p) by (nonlinear_arith)
                    requires page <= q < d as int / p, p > 0;
                lemma_below_align_up(a as int, self.base + q * p, aligned as int, align as int);
            }
        }
        (aligned - self.base) >> self.lg_page
    }

}

} // verus!

impl Huge {
    /// [`Huge::alloc_run`] for a zeroing caller, as the pointer and the
    /// zero flag alone.
    #[cfg(test)]
    pub fn alloc(&self, meta: &Meta, size: usize, align: usize, arena: u16) -> (*mut u8, bool) {
        self.alloc_wanting(meta, size, align, arena, true)
    }

    /// [`Huge::alloc_run`] with its `zero` argument, as the pointer and the
    /// zero flag alone.
    #[cfg(test)]
    pub fn alloc_wanting(
        &self,
        meta: &Meta,
        size: usize,
        align: usize,
        arena: u16,
        zero: bool,
    ) -> (*mut u8, bool) {
        let r = self
            .alloc_run(
                meta,
                size,
                align,
                arena,
                zero,
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
            .0;
        (r.ptr, r.zeroed)
    }
}

verus! {

impl Huge {
    /// Allocate `size` bytes aligned to `align` (a power of two; anything
    /// up to a page is free). Returns the pointer, whether the memory is
    /// known to read as zero and the pages of the run as its descriptor
    /// records them (the guard included; what [`Huge::free_run`] will
    /// report as `pages`, so the caller's per-arena page count never
    /// drifts from what the free subtracts); [`Placed::NONE`] when the
    /// region, the metadata region or the kernel refuses. `zero` says the
    /// caller will clear the run unless it is reported zero (`calloc`,
    /// `MALLOCX_ZERO`): only then may a run reusing a `Purged` extent be
    /// discarded again to report it zero ([`Huge::rediscard`]); any other
    /// caller never pays that call.
    /// Ghost: the run's token goes to `tok`, and the second result is the
    /// token that the run's usable bytes read as zero when `zeroed`.
    // Cold: out of line in `Shared::alloc_huge`, so the ghost arguments that
    // verified code erases cannot change whether it is inlined.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub fn alloc_run(&self, meta: &Meta, size: usize, align: usize, arena: u16, zero: bool, Tracked(tok): Tracked<&mut Map<int, RunToken>>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (Placed, Tracked<Zeroed>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            crate::meta::is_pow2(align),
            self.spec_base_aligned(),
            self.is_process(),
            meta.is_process(),
            meta.live_id() == self.spec_mid(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            r.0.pages <= u32::MAX,
            r.0.ptr@.addr != 0 ==> self.spec_run_at(r.0.ptr@.addr, size, align),
            r.0.ptr@.addr != 0 && r.0.zeroed ==> r.1@.covers(r.0.ptr, self.spec_usable(size)),
            r.0.ptr@.addr != 0 ==> size <= self.spec_usable(size),
            r.0.ptr@.addr != 0 ==> size + self.spec_page() - 1 <= usize::MAX,
            r.0.ptr@.addr != 0 ==> self.run_token(*final(tok), r.0),
            r.0.ptr@.addr != 0 ==> (final(tok)[0].npages() - GUARD_PAGES) * self.spec_page() == self.spec_usable(size),
            r.0.ptr@.addr != 0 ==> self.spec_placeable(size, align),
    {
        assert!(align.is_power_of_two(), "huge: alignment not a power of two");
        self.alloc_aligned(meta, size, align, arena, zero, Tracked(tok), Tracked(ord))
    }

    /// [`Huge::alloc_run`] for an `align` already known to be a power of
    /// two.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn alloc_aligned(&self, meta: &Meta, size: usize, align: usize, arena: u16, zero: bool, Tracked(tok): Tracked<&mut Map<int, RunToken>>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (Placed, Tracked<Zeroed>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            crate::meta::is_pow2(align),
            self.spec_base_aligned(),
            self.is_process(),
            meta.is_process(),
            meta.live_id() == self.spec_mid(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            r.0.pages <= u32::MAX,
            r.0.ptr@.addr != 0 ==> self.spec_run_at(r.0.ptr@.addr, size, align),
            r.0.ptr@.addr != 0 && r.0.zeroed ==> r.1@.covers(r.0.ptr, self.spec_usable(size)),
            r.0.ptr@.addr != 0 ==> size <= self.spec_usable(size),
            r.0.ptr@.addr != 0 ==> size + self.spec_page() - 1 <= usize::MAX,
            r.0.ptr@.addr != 0 ==> self.run_token(*final(tok), r.0),
            r.0.ptr@.addr != 0 ==> (final(tok)[0].npages() - GUARD_PAGES) * self.spec_page() == self.spec_usable(size),
            r.0.ptr@.addr != 0 ==> self.spec_placeable(size, align),
    {
        proof {
            use_type_invariant(self);
            self.lemma_page();
            self.lemma_pages(self.npages);
            let k = choose|k: nat| k < 64 && align as nat == #[trigger] pow2(k);
            vstd::bits::lemma_usize_pow2_no_overflow(k);
            vstd::bits::lemma_usize_shl_is_mul(1, k as usize);
            let ku = k as usize;
            assert((1usize << ku) > 0 && (1usize << ku) & sub(1usize << ku, 1) == 0) by (bit_vector)
                requires ku < 64;
        }
        let Some(need) = self.run_pages(size, align) else { return (Placed::NONE, Tracked(Zeroed::empty(0))) };
        let usable_pages = need - GUARD_PAGES;
        let page = self.page_size();
        let tracked mut under = LockOrder::spare();
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut under));
        proof {
            self.lemma_process_runs();
            self.lemma_run_ids(g@);
        }
        let r = if let Some((s, ext)) = self.find(&g, need, align) {
            let end = s + need;
            // The lock's value at the placement: `prepare`'s commits are apart
            // from its live runs' guards. The `ensure` calls below change neither
            // its descriptors nor its mark (`prepare` gets the guard's
            // permission and `g.next_page`).
            let ghost gp = g@;
            proof {
                self.lemma_fits_apart(&gp, s as int, need as int, align);
                // `s` is a placement, so the request is placeable.
                assert(self.aligned_at(s as int, align));
                assert(page_aligned(self.base as int, self.spec_page(), s as int, align as int));
            }

            // Everything fallible happens before any state changes.
            let (run, _run_opt) = alloc_desc(meta, Tracked(&mut under));
            if run.is_null() {
                (Placed::NONE, Tracked(Zeroed::empty(0)))
            } else {
                let run_perm = Tracked(_run_opt.get().tracked_unwrap());
                let (spare, _spare_opt) = alloc_desc(meta, Tracked(&mut under));
                if spare.is_null() {
                    free_desc(meta, run, Tracked(desc_raw(run_perm.get())), Tracked(&mut under));
                    (Placed::NONE, Tracked(Zeroed::empty(0)))
                } else {
                    let spare_perm = Tracked(_spare_opt.get().tracked_unwrap());
                    proof {
                        // The table holds at most two keys per extent, a page or more
                        // each, so its count is far below `Table::ensure`'s bound.
                        assert forall|i: int| 0 <= i < g@.all@.len() implies (#[trigger] g@.descs@[g@.all@[i]].value()).pages >= 1
                            && g@.descs@[g@.all@[i]].value().state <= 2 by {
                            assert(g@.all@.contains(g@.all@[i]));
                            assert(g@.descs@.contains_key(g@.all@[i]));
                        }
                        lemma_tally_counts(g@.all@, g@.descs@, 0);
                        assert(stat(g@.stats, 0) == tally(g@.all@, g@.descs@, 0));
                        assert(stat(g@.stats, 1) == tally(g@.all@, g@.descs@, 1));
                        assert(stat(g@.stats, 2) == tally(g@.all@, g@.descs@, 2));
                    }
                    // Room for this allocation's keys (3) and one more per live run,
                    // this one included: a run's free adds at most one key net (its
                    // start key goes, a free extent's two come, the neighbours' merge
                    // away), and its own `ensure` is best effort, so with this
                    // reservation a free never finds the table full however the
                    // metadata region fares (design 14.1). Capped where no process
                    // gets.
                    let runs = g.stats.runs;
                    let extra = if g.table.keys() <= 0xFFFF_FFFF && runs <= 0xFFFF_FFFF { 4 + runs } else { 3 };
                    let ok = g.table.ensure(meta, page, extra, Tracked(&mut under)).is_ok()
                        && (end <= g.next_page || g.free.ensure(meta, page, end - 1, Tracked(&mut under)).is_ok())
                        && self.prepare(s, usable_pages, g.next_page, g.perm()).is_ok();
                    if !ok {
                        free_desc(meta, run, Tracked(desc_raw(run_perm.get())), Tracked(&mut under));
                        free_desc(meta, spare, Tracked(desc_raw(spare_perm.get())), Tracked(&mut under));
                        proof {
                            // The lock's frame: the ghost page count, shift, base and `Meta`
                            // identity are the ones `new` set.
                            assert(lock_frame(lock_init(self.inner), g@));
                        }
                        (Placed::NONE, Tracked(Zeroed::empty(0)))
                    } else {
                        proof {
                            // The `ensure` calls left the runs `prepare` was shown apart from.
                            assert(g@.descs@ == gp.descs@);
                        }
                        // `check_zero` needs the extent's state before `take` may free it.
                        let _gi: &Inner = &g;
                        let prior = if purge::check_zero() { Some(self.prior_state(s, ext, Tracked(_gi.descs.borrow()))) } else { None };
                        let (zeroed, z) = self.take(&mut g, meta, Placement { start: s, pages: need, ext }, run, spare, arena, zero, run_perm, spare_perm, Tracked(tok), Tracked(&mut under));
                        // `take` wrote the run's descriptor; the lock is still held.
                        let pages = g.desc(run).pages();
                        self.sync_oldest(&g);
                        if let (true, Some(prior)) = (zeroed, prior) {
                            self.check_zero(s, usable_pages, prior, arena, Tracked(tok));
                        }
                        proof {
                            // The lock's frame: the ghost page count, shift, base and `Meta`
                            // identity are the ones `new` set.
                            assert(lock_frame(lock_init(self.inner), g@));
                        }
                        proof {
                            self.lemma_run_at(s, usable_pages, size, align);
                        }
                        (Placed { ptr: crate::vspec::addr::addr_to_ptr(self.addr(s)), zeroed, pages }, z)
                    }
                }
            }
        } else {
            (Placed::NONE, Tracked(Zeroed::empty(0)))
        };
        g.unlock();
        r
    }

    /// Where the pages of a run at `s` come from, for `check_zero`: the
    /// free extent's state, or "fresh" for never-touched pages at or past
    /// the mark (which is also what an aligned start beyond `ext` gets).
    ///
    /// `descs` is the caller's guard's descriptor permissions, and `ext`,
    /// when not null, the free extent `find` returned.
    #[cold]
    fn prior_state(&self, s: usize, ext: *mut HugeDesc, Tracked(descs): Tracked<&Map<*mut HugeDesc, PointsTo<HugeDesc>>>) -> &'static str
        requires
            crate::sys::may_abort(),
            ext@.addr != 0 ==> {
                &&& descs.contains_key(ext)
                &&& descs[ext].ptr() == ext
                &&& descs[ext].is_init()
                &&& descs[ext].value().state != RunState::Active as u8
                &&& descs[ext].value().state <= 2
                &&& descs[ext].value().start + descs[ext].value().pages <= usize::MAX
            },
    {
        if ext.is_null() {
            return "fresh";
        }
        let e = ptr_ref(ext, Tracked(descs.tracked_borrow(ext)));
        let (st, e_end) = (e.state(), e.end());
        if s >= e_end {
            return "fresh";
        }
        match st {
            RunState::Purged => "Purged",
            RunState::Empty => "Empty",
            // `find` returns free extents only (`extent_at` checks).
            RunState::Active => unreachable!(),
        }
    }

    /// The `check_zero` debug mode: the first and
    /// last word of every usable page of a run reported zero must read
    /// zero. Aborts with the run, the pages' prior state, the arena and the
    /// purge advice. Faults every page in, so debug only. The run is the
    /// `usable_pages` pages from page `s`, inside the region; `tok[0]` is
    /// its token, which `take` just made. The reads go through a
    /// permission to the run's usable bytes, which `heap::live::Live::from_run`
    /// mints here with the token: it is used for the reads and dropped with
    /// the token given back (`Live::into_run`), so no permission to the run
    /// is held when this returns.
    #[cold]
    #[inline(never)]
    fn check_zero(
        &self,
        s: usize,
        usable_pages: usize,
        prior: &'static str,
        arena: u16,
        Tracked(tok): Tracked<&mut Map<int, RunToken>>,
    )
        requires
            crate::sys::may_abort(),
            crate::vspec::user::may_read_user(),
            s + usable_pages <= self.spec_npages(),
            self.spec_base_aligned(),
            self.is_process(),
            old(tok).contains_key(0),
            old(tok)[0].wf(),
            old(tok)[0].start() == s,
            old(tok)[0].npages() == usable_pages + GUARD_PAGES,
        ensures
            *final(tok) == *old(tok),
    {
        proof {
            use_type_invariant(self);
            self.lemma_page();
            self.lemma_pages(s);
            self.lemma_pages((s + usable_pages) as usize);
        }
        let page = self.page_size();
        let base = self.addr(s);
        let ghost p = self.spec_page();
        // The region's end, and the run's.
        let ghost hi = self.spec_base() + self.spec_npages() * p;
        let ghost end = self.spec_base() + (s + usable_pages) * p;
        // The run's pointer for its `Live` token: the cast
        // of its first page's address (the reads' pointers are casts too).
        let ghost run_ptr = vstd::raw_ptr::ptr_mut_from_data::<u8>(
            vstd::raw_ptr::PtrData::<u8> { addr: base, provenance: crate::vspec::addr::cast_provenance(), metadata: () },
        );
        let ghost t0 = tok[0];
        proof {
            broadcast use vstd::raw_ptr::axiom_ptr_mut_from_data;
            t0.lemma_wf();
            self.lemma_run_block(s, usable_pages, run_ptr);
        }
        let tracked blk = crate::heap::live::Live::from_run(tok.tracked_remove(0), run_ptr);
        proof {
            blk.lemma_mem();
            vstd::arithmetic::mul::lemma_mul_is_distributive_add_other_way(p, s as int, usable_pages as int);
        }
        proof {
            crate::meta::lemma_pow2_bits(page);
            assert(page % 8 == 0) by (bit_vector)
                requires page & sub(page, 1) == 0, page >= 16;
            let (n, u) = (self.spec_npages(), (s + usable_pages) as int);
            assert(u * p <= n * p) by (nonlinear_arith) requires u <= n, p > 0;
        }
        for i in 0..usable_pages
            invariant
                crate::sys::may_abort(),
                crate::vspec::user::may_read_user(),
                page as int == p,
                p == self.spec_page(),
                16 <= p,
                page % 8 == 0,
                base == self.spec_base() + s * p,
                self.spec_base_aligned(),
                end == self.spec_base() + (s + usable_pages) * p,
                end <= hi,
                hi == self.spec_base() + self.spec_npages() * p,
                hi <= crate::meta::va_limit(),
                blk.mem_ok(),
                blk.mem().covers(base as int, usable_pages * p),
                blk.mem().provenance() == crate::vspec::addr::cast_provenance(),
                blk.block(run_ptr),
                blk.run_opt() == Some(t0),
                end == base + usable_pages * p,
        {
            proof {
                let (b, si, ii, u) = (self.spec_base() as int, s as int, i as int, usable_pages as int);
                // Page `s + i` ends inside the run.
                vstd::arithmetic::mul::lemma_mul_inequality(si + ii + 1, si + u, p);
                vstd::arithmetic::mul::lemma_mul_is_distributive_add_other_way(p, si, ii + 1);
                vstd::arithmetic::mul::lemma_mul_is_distributive_add_other_way(p, ii, 1);
                // It starts on a word: the base is a page start, and a page
                // a multiple of 8.
                let (q, k) = (b / p, p / 8);
                vstd::arithmetic::div_mod::lemma_fundamental_div_mod(b, p);
                vstd::arithmetic::div_mod::lemma_fundamental_div_mod(p, 8);
                assert(b + si * p + ii * p == (k * q + k * si + k * ii) * 8) by (nonlinear_arith)
                    requires b == p * q, p == 8 * k;
                vstd::arithmetic::div_mod::lemma_mod_multiples_basic(k * q + k * si + k * ii, 8);
            }
            for &off in o: &[0, page - 8]
                invariant
                    crate::sys::may_abort(),
                    crate::vspec::user::may_read_user(),
                    page as int == p,
                    p == self.spec_page(),
                    16 <= p,
                    page % 8 == 0,
                    (base + i * p) % 8 == 0,
                    base >= self.spec_base(),
                    base + i * p + p <= end,
                    end <= hi,
                    hi == self.spec_base() + self.spec_npages() * p,
                    hi <= crate::meta::va_limit(),
                    base == self.spec_base() + s * p,
                    end == self.spec_base() + (s + usable_pages) * p,
                    blk.mem_ok(),
                    blk.mem().covers(base as int, usable_pages * p),
                    blk.mem().provenance() == crate::vspec::addr::cast_provenance(),
                    end == base + usable_pages * p,
                    o.seq().len() == 2,
                    *o.seq()[0] == 0,
                    *o.seq()[1] == page - 8,
            {
                proof {
                    assert(off == *o.seq()[o.index() as int]);
                }
                let a = base + i * page + off;
                proof {
                    assert forall|x: int| a <= x < a + 8 implies #[trigger] blk.mem().dom().contains(x) by {
                        assert(base <= x < base + usable_pages * p);
                    }
                }
                // SAFETY: a usable page of the run this call just took;
                // `prepare` committed it, and `blk`'s permission covers it.
                let word = unsafe { self.read_run_word(a, Tracked(blk.mem_ref())) };
                if word != 0 {
                    dirty_zero_run(base, usable_pages, s, prior, arena, a, word);
                }
            }
        }
        proof {
            // The token back, the permission dropped with it.
            let tracked t = blk.into_run(s as int);
            tok.tracked_insert(0, t);
            assert(*tok =~= *old(tok));
        }
    }

    /// The word at `a`, read once, for [`Huge::check_zero`], through the
    /// permission `m` to it (the run's, which `check_zero` mints), by the
    /// cast of `a` (`m` is of the cast's provenance). Nothing is claimed
    /// about the value.
    ///
    /// # Safety
    /// `a` is an aligned word that `m` covers. A read of user memory: it
    /// needs [`crate::vspec::user::may_read_user`].
    #[inline(always)]
    unsafe fn read_run_word(&self, a: usize, Tracked(m): Tracked<&crate::vspec::user::UserMem>) -> u64
        requires
            crate::vspec::user::may_read_user(),
            m.provenance() == crate::vspec::addr::cast_provenance(),
            forall|x: int| a <= x < a + 8 ==> #[trigger] m.dom().contains(x),
            a % 8 == 0,
    {
        let q = crate::vspec::addr::addr_to_ptr(a);
        proof {
            broadcast use {vstd::raw_ptr::axiom_ptr_mut_from_data, vstd::layout::layout_of_primitives, vstd::layout::group_align_properties, vstd::layout::align_nonzero};
            m.lemma_valid_reads(q, 8);
            crate::vspec::ptr::lemma_reads_cast::<u64>(q, 8);
            crate::vspec::ptr::axiom_plain_u64();
            // A `u64` is aligned to its size, so to its alignment.
            crate::layout::lemma_aligned_down(a as int, 8, vstd::layout::align_of::<u64>() as int);
        }
        // SAFETY: `m` makes the eight bytes at `a` valid for reads through
        // the cast, and `a` is aligned.
        unsafe { ptr::read_volatile(q as *const u64) }
    }

    /// Refresh `oldest_empty` from the list head. Returns `true` when the
    /// list went from empty to non-empty, which is when a background
    /// thread sleeping without a deadline needs waking.
    fn sync_oldest(&self, g: &Inner) -> bool
        requires
            g.wf(),
    {
        proof {
            g.lemma_head();
        }
        // The head, if any, is a live `Empty` extent.
        let new = if g.empty_head.is_null() { 0 } else { g.desc(g.empty_head).dirty_at };
        self.gate.set_oldest_empty(new)
    }

    /// Lowest-address first fit for `need` pages at `align`. Returns the
    /// start page and the free extent it comes from (`None` for fresh pages
    /// at the high-water mark). The extent may end below `s + need` only if
    /// it touches the mark.
    ///
    /// Proved lowest: the start fits ([`Huge::fits`]: aligned, inside the
    /// region, every page free) and no lower page does; `None` only when no
    /// page fits. Each free extent the scan passes over holds no aligned
    /// fit and ends below the mark, so with coalescing no fit can start in
    /// it ([`Inner::lemma_fit_extent`]); the pages between extents are not
    /// free.
    fn find(&self, g: &Inner, need: usize, align: usize) -> (r: Option<(usize, *mut HugeDesc)>)
        requires
            crate::sys::may_abort(),
            (self.base as int) % (pow2(self.lg_page as nat) as int) == 0,
            g.wf(),
            g.spec_np() == self.npages,
            g.spec_lg() == self.lg_page,
            1 <= need <= self.npages,
            align > 0,
            align & sub(align, 1) == 0,
            align <= self.npages * pow2(self.lg_page as nat),
        ensures
            r matches Some((s, e)) ==> s + need <= self.npages && if e@.addr == 0 {
                s >= g.next_page
            } else {
                let v = g.descs@[e].value();
                &&& g.descs@.contains_key(e)
                &&& v.state != RunState::Active as u8
                &&& v.start <= s
                &&& (s + need <= v.start + v.pages || v.start + v.pages == g.next_page)
            },
            r matches Some((s, _)) ==> (align > pow2(self.lg_page as nat) ==> (self.base + s * pow2(self.lg_page as nat)) % (align as int) == 0),
            // Fresh pages only when no free extent touches the mark, so a
            // hole the alignment leaves there touches no free extent.
            r matches Some((s, e)) ==> (e@.addr == 0 ==> none_ends_at(g.descs@, g.next_page as int)),
            // Lowest-address placement: `s` fits and no lower page does.
            r matches Some((s, _)) ==> self.fits(g, s as int, need as int, align),
            r matches Some((s, _)) ==> forall|q: int| 0 <= q < s ==> !#[trigger] self.fits(g, q, need as int, align),
            // Nothing fits when there is no placement.
            r is None ==> forall|q: int| !#[trigger] self.fits(g, q, need as int, align),
    {
        let mut from = 0;
        while let Some(a) = g.free.map.lowest_set_from(from)
            invariant
                crate::sys::may_abort(),
                g.wf(),
                (self.base as int) % (pow2(self.lg_page as nat) as int) == 0,
                g.spec_np() == self.npages,
                g.spec_lg() == self.lg_page,
                1 <= need <= self.npages,
                align > 0,
                align & sub(align, 1) == 0,
                align <= self.npages * pow2(self.lg_page as nat),
                from <= g.next_page,
                // The free extents touching the mark are not behind `from`.
                forall|x: *mut HugeDesc|
                    #[trigger] g.descs@.contains_key(x) && g.descs@[x].value().state != RunState::Active as u8
                        && g.descs@[x].value().start + g.descs@[x].value().pages == g.next_page ==> from <= g.descs@[x].value().start,
                // The scan has passed whole extents only, and no placement
                // starts behind it.
                ends_by(g.descs@, from as int),
                forall|q: int| 0 <= q < from ==> !#[trigger] self.fits(g, q, need as int, align),
            ensures
                // Out of free pages at or past `from`.
                forall|x: nat| #[trigger] g.free.map.view().contains(x) ==> x < from,
                from <= g.next_page,
                ends_by(g.descs@, from as int),
                forall|q: int| 0 <= q < from ==> !#[trigger] self.fits(g, q, need as int, align),
            decreases g.next_page - from,
        {
            proof {
                // No free extent starts in `[from, a)`: its first page
                // would be a free bit there.
                assert forall|x: *mut HugeDesc|
                    #[trigger] g.descs@.contains_key(x) && g.descs@[x].value().state != RunState::Active as u8
                        && g.descs@[x].value().start < a implies g.descs@[x].value().start + g.descs@[x].value().pages <= a by {
                    let v = g.descs@[x].value();
                    if v.start >= from {
                        assert(desc_ok(v, g.next_page));
                        lemma_marked(g.free.map.view(), v, v.start as int);
                    }
                }
            }
            let e = g.extent_at(a);
            proof {
                assert(desc_ok(g.descs@[e].value(), g.next_page));
            }
            // `extent_at` ensures `e` is a live free extent at `a`.
            let e_end = g.desc(e).end();
            let s = self.align_page(a, align);
            proof {
                // No placement below `s`: none behind `from`, no free page
                // in `[from, a)`, and no aligned page in `[a, s)`.
                self.lemma_below(g, from as int, a as int, s as int, need as int, align);
            }
            if s + need <= e_end {
                proof {
                    assert forall|p: int| s <= p < s + need implies #[trigger] page_free(g.descs@, g.next_page as int, p) by {
                        assert(in_free(g.descs@, e, p));
                    }
                }
                return Some((s, e));
            }
            if e_end == g.next_page {
                proof {
                    // `e` and the untouched suffix after it are free.
                    assert forall|p: int| s <= p < s + need implies #[trigger] page_free(g.descs@, g.next_page as int, p) by {
                        if p < g.next_page {
                            assert(in_free(g.descs@, e, p));
                        }
                    }
                }
                return if s + need <= self.npages { Some((s, e)) } else { None };
            }
            proof {
                // A free extent touching the mark is not `e`, and starts at
                // a free page at or past `from`, so at or past `a`: past `e`.
                assert forall|x: *mut HugeDesc|
                    #[trigger] g.descs@.contains_key(x) && g.descs@[x].value().state != RunState::Active as u8
                        && g.descs@[x].value().start + g.descs@[x].value().pages == g.next_page implies e_end <= g.descs@[x].value().start by {
                    let v = g.descs@[x].value();
                    assert(desc_ok(v, g.next_page));
                    assert(marked(g.free.map.view(), v));
                    lemma_marked(g.free.map.view(), v, v.start as int);
                    if x != e {
                        assert(apart(v, g.descs@[e].value()));
                    }
                }
                // A free extent starting below `e_end` starts below `from`
                // (so ends by it) or is `e`: one starting in `[from, a)`
                // would put a free page there.
                assert forall|x: *mut HugeDesc|
                    #[trigger] g.descs@.contains_key(x) && g.descs@[x].value().state != RunState::Active as u8
                        && g.descs@[x].value().start < e_end implies g.descs@[x].value().start + g.descs@[x].value().pages <= e_end by {
                    let v = g.descs@[x].value();
                    if v.start >= from {
                        assert(desc_ok(v, g.next_page));
                        assert(marked(g.free.map.view(), v));
                        lemma_marked(g.free.map.view(), v, v.start as int);
                        if x != e {
                            assert(apart(v, g.descs@[e].value()));
                        }
                    }
                }
                // A placement at `q` in `[s, e_end)` starts in `e`, which
                // ends below the mark, so it must end in `e`; but `s` is
                // the lowest aligned page there and `s + need` overruns
                // `e`.
                assert forall|q: int| 0 <= q < e_end implies !#[trigger] self.fits(g, q, need as int, align) by {
                    if s <= q && self.fits(g, q, need as int, align) {
                        assert(page_free(g.descs@, g.next_page as int, q));
                        let x = choose|x: *mut HugeDesc| #[trigger] in_free(g.descs@, x, q);
                        assert(in_free(g.descs@, e, q));
                        if x != e {
                            assert(apart(g.descs@[x].value(), g.descs@[e].value()));
                        }
                        g.lemma_fit_extent(e, q, need as int);
                    }
                }
            }
            from = e_end;
        }
        proof {
            // No free page is at or past `from`, where a free extent
            // touching the mark would start.
            assert forall|x: *mut HugeDesc|
                #[trigger] g.descs@.contains_key(x) && g.descs@[x].value().state != RunState::Active as u8 implies g.descs@[x].value().start
                    + g.descs@[x].value().pages != g.next_page by {
                let v = g.descs@[x].value();
                assert(desc_ok(v, g.next_page));
                assert(marked(g.free.map.view(), v));
                lemma_marked(g.free.map.view(), v, v.start as int);
            }
        }
        let s = self.align_page(g.next_page, align);
        proof {
            // No placement below `s`: no free page is left at or past
            // `from`, and none of `[next_page, s)` is aligned. From `s` on
            // every page is fresh.
            self.lemma_below(g, from as int, g.next_page as int, s as int, need as int, align);
        }
        if s + need <= self.npages { Some((s, ptr::null_mut())) } else { None }
    }

    /// Kernel side of an allocation of `usable` pages at `s`: commit and
    /// place the guard. Pages below `watermark` are committed already,
    /// apart from the `PROT_NONE` guards of live runs (fallback mode, or
    /// guard-install mode once a marker was refused) and, once
    /// [`Huge::mixed_guards`] is set, possibly a guard whose release failed.
    ///
    /// In guard-install mode every page below the mark stays committed, so
    /// one `mprotect` over `[watermark, end)` is all a run past the mark
    /// needs. That range deliberately starts at the mark, not at `s`: when
    /// alignment puts `s` above the mark, `take` records `[watermark, s)`
    /// as a `Purged` free extent (or folds it into the extent that touched
    /// the mark), and the run later placed in that hole must find its pages
    /// committed. Committing only `[s, end)` here left the hole `PROT_NONE`
    /// and the first write into it faulted; the fallback mode commits from
    /// the mark for the same reason.
    ///
    /// Every byte below `watermark` was committed (`Inner::wf`'s
    /// `mark_committed`), and on `Ok` every byte below the new mark, the
    /// greater of `watermark` and `end`, is.
    fn prepare(&self, s: usize, usable: usize, watermark: usize, Tracked(lk): Tracked<&LockPerm<Inner>>) -> (res: Result<(), Errno>)
        requires
            crate::sys::may_syscall(),
            1 <= usable,
            s + usable + GUARD_PAGES <= self.npages,
            watermark <= self.npages,
            self.inner.has_perm(*lk),
            runs_apart(*lk.value(), s as int, usable + GUARD_PAGES, watermark as int),
            crate::vspec::unbacked::committed_range(self.base as int, watermark * self.spec_page()),
        ensures
            res is Ok ==> crate::vspec::unbacked::committed_range(
                self.base as int,
                (if watermark < s + usable + GUARD_PAGES { s + usable + GUARD_PAGES } else { watermark as int }) * self.spec_page(),
            ),
    {
        // The huge lock's value, which the caller holds (`lk`).
        let ghost g = *lk.value();
        proof {
            use_type_invariant(self);
            self.lemma_pages(usable);
            self.lemma_pages(GUARD_PAGES);
            self.lemma_pages((self.npages - watermark) as usize);
            let p = pow2(self.lg_page as nat) as int;
            assert(forall|x: int, y: int| 0 <= x <= y ==> #[trigger] (x * p) <= #[trigger] (y * p)) by {
                assert forall|x: int, y: int| 0 <= x <= y implies #[trigger] (x * p) <= #[trigger] (y * p) by {
                    vstd::arithmetic::mul::lemma_mul_inequality(x, y, p);
                }
            }
        }
        let page = self.page_size();
        let guard = s + usable;
        let end = guard + GUARD_PAGES;
        proof {
            // The run's first page spares every live run's guard, and
            // every live run, for `split_guard`.
            lemma_pages_spare_guards(*self, g, vstd::raw_ptr::ptr_null_mut(), s as int, s + 1);
            lemma_page_apart_from_runs(*self, g, s as int, usable + GUARD_PAGES, watermark as int);
        }
        let r = if self.guard_install {
            if self.mixed_guards.load(Ordering::Relaxed) {
                // A `PROT_NONE` guard may lie below the mark.
                proof {
                    lemma_pages_spare_guards(*self, g, vstd::raw_ptr::ptr_null_mut(), s as int, s + usable);
                }
                commit(self.addr(s), usable * page, Ghost(*self), Tracked(lk), Ghost(vstd::raw_ptr::ptr_null_mut()))?;
                proof {
                    self.lemma_page_in(s as int, s + usable, s as int);
                }
            }
            if watermark < end {
                proof {
                    lemma_pages_spare_guards(*self, g, vstd::raw_ptr::ptr_null_mut(), watermark as int, end as int);
                }
                commit(self.addr(watermark), (end - watermark) * page, Ghost(*self), Tracked(lk), Ghost(vstd::raw_ptr::ptr_null_mut()))?;
                proof {
                    if watermark <= s {
                        self.lemma_page_in(watermark as int, end as int, s as int);
                    }
                    self.lemma_mark_grows(watermark as int, watermark as int, end as int);
                }
            }
            if guard_install(self.addr(guard), GUARD_PAGES * page).is_err() {
                // Refused (`EINVAL` on an `mlock`ed range: the marker
                // cannot go on a locked VMA): a `PROT_NONE` guard instead,
                // which works there, so the run is guarded all the same.
                // If that fails too the run is not handed out (design
                // 14.1).
                proof {
                    // A run below the mark was committed when the mark
                    // passed it (`mark_committed`).
                    if s < watermark {
                        self.lemma_page_in(0, watermark as int, s as int);
                    }
                }
                split_guard(self.addr(s), self.addr(guard), page, true, Ghost(*self), Tracked(lk))?;
                self.mixed_guards.store(true, Ordering::Relaxed);
            }
            Ok(())
        } else {
            // Fallback mode (no `MADV_GUARD_INSTALL`) or `guards:false`.
            // Below the mark every page is read-write except the guards
            // of live runs (a free makes its run's guard read-write
            // again, `free_run`; if that ever fails, `mixed_guards` is
            // set and every allocation commits its own range), so the
            // VMAs stay bounded by the live runs, and a run inside the
            // mark needs no commit. A run reaching past the mark is
            // committed from the mark when alignment starts it above (the
            // hole `take` records as a free extent must not be left a
            // `PROT_NONE` island below the mark), from `s` otherwise, and
            // through its guard page, so that under `guards:false`
            // nothing splits the VMA and otherwise the guard is split off
            // the run's own VMA ([`split_guard`]).
            let fresh = watermark < end;
            if fresh || self.mixed_guards.load(Ordering::Relaxed) {
                let lo = if watermark < s { watermark } else { s };
                proof {
                    self.lemma_pages((end - lo) as usize);
                    lemma_pages_spare_guards(*self, g, vstd::raw_ptr::ptr_null_mut(), lo as int, end as int);
                }
                commit(self.addr(lo), (end - lo) * page, Ghost(*self), Tracked(lk), Ghost(vstd::raw_ptr::ptr_null_mut()))?;
                proof {
                    self.lemma_page_in(lo as int, end as int, s as int);
                    if fresh {
                        self.lemma_mark_grows(watermark as int, lo as int, end as int);
                    }
                }
            }
            if self.guards {
                split_guard(self.addr(s), self.addr(guard), page, fresh, Ghost(*self), Tracked(lk))?;
            }
            Ok(())
        };
        // Design 7: `MADV_HUGEPAGE` on runs of a PMD or more under
        // `thp:always` (`thp.rs`; `usize::MAX` otherwise, so nothing is
        // advised). A refusal is counted, not an error: the run works
        // with small pages.
        if r.is_ok() && usable * page >= crate::thp::huge_min() {
            crate::thp::advise_huge(self.addr(s), usable * page);
        }
        r
    }

    /// Record the run `p` taken from its extent (or fresh pages), splitting
    /// the extent around it. Infallible. Returns the zero flag, with the
    /// token that the run's usable bytes read as zero.
    ///
    /// Proved to keep `Inner::wf`: the pieces (the run, the head of the
    /// extent before it, its tail after the run, or the hole alignment
    /// leaves at the mark) cover exactly what they replace, so the extents
    /// stay disjoint and inside `next_page`, each under exactly its keys,
    /// and the counters stay their tallies. `run` and `spare` come with
    /// their permissions (from [`alloc_desc`], through `Meta::alloc_raw`),
    /// so they are neither live descriptors nor the same block.
    ///
    /// The proof is large, and its solver cost moves with unrelated items
    /// elsewhere in the crate; the rlimit leaves it headroom.
    #[verifier::rlimit(1600)]
    #[allow(non_shorthand_field_patterns)]
    // The extra arguments are ghost and tracked state.
    #[allow(clippy::too_many_arguments)]
    fn take(
        &self,
        g: &mut Inner,
        meta: &Meta,
        p: Placement,
        run: *mut HugeDesc,
        spare: *mut HugeDesc,
        arena: u16,
        zero: bool,
        Tracked(run_desc): Tracked<(PointsTo<HugeDesc>, crate::meta::MetaClaim)>,
        Tracked(spare_desc): Tracked<(PointsTo<HugeDesc>, crate::meta::MetaClaim)>,
        Tracked(tok): Tracked<&mut Map<int, RunToken>>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: (bool, Tracked<Zeroed>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_meta()),
            old(g).wf(),
            old(g).spec_np() == self.npages,
            old(g).spec_lg() == self.lg_page,
            old(g).spec_base() == self.base,
            old(g).spec_rid() == run_loc(),
            old(g).spec_pid() == page_loc(),
            // `prepare` committed the pages up to the new mark.
            crate::vspec::unbacked::committed_range(
                self.base as int,
                (if old(g).next_page < p.start + p.pages { p.start + p.pages } else { old(g).next_page as int }) * self.spec_page(),
            ),
            run@.addr != 0,
            spare@.addr != 0,
            run_desc.0.ptr() == run,
            desc_claim_for(run_desc.1, run, old(g).mid@),
            spare_desc.0.ptr() == spare,
            desc_claim_for(spare_desc.1, spare, old(g).mid@),
            meta.live_id() == old(g).mid@,
            desc_in_meta(run),
            desc_in_meta(spare),
            GUARD_PAGES < p.pages,
            p.start + p.pages <= self.npages,
            (old(g).table.spec_count() + 3) * 2 <= old(g).table.spec_cap(),
            take_ok(p, old(g).descs@, old(g).next_page),
        ensures
            final(g).wf(),
            final(g).spec_np() == old(g).spec_np(),
            final(g).spec_lg() == old(g).spec_lg(),
            final(g).spec_base() == old(g).spec_base(),
            final(g).spec_mid() == old(g).spec_mid(),
            final(g).spec_rid() == old(g).spec_rid(),
            final(g).spec_pid() == old(g).spec_pid(),
            final(g).descs@.contains_key(run),
            final(g).descs@[run].value().pages == p.pages,
            final(tok).contains_key(0),
            final(tok)[0].wf(),
            final(tok)[0].start() == p.start,
            final(tok)[0].npages() == p.pages,
            r.0 ==> r.1@.addr() == self.base + p.start * self.spec_page()
                && r.1@.len() == (p.pages - GUARD_PAGES) * self.spec_page(),
            *final(ord) == *old(ord),
    {
        // The lists' pairwise distinctness and `index_of`'s choice are
        // most of the solver's work if left open; only the steps that name
        // a list position reveal them.
        hide(Seq::no_duplicates);
        hide(Seq::index_of);
        // `wf` unfolds into a quantifier per conjunct over every descriptor
        // map in scope; take reads only those `lemma_wf_for_take` states.
        hide(Inner::wf);
        let Placement { start: s, pages: need, ext } = p;
        proof {
            use_type_invariant(self);
        }
        let end = p.end();
        let old_mark = g.next_page;
        let mut spare_used = false;
        let mut zeroed = true;
        let tracked (run_perm, run_claim) = run_desc;
        let tracked (spare_perm, spare_claim) = spare_desc;
        let tracked mut spare_slot = Some(spare_perm);
        let tracked mut spare_claim_slot = Some(spare_claim);
        let ghost d0 = g.descs@;
        let ghost a0 = g.all@;
        let ghost l0 = g.elist@;
        let ghost t0 = g.table;
        let ghost st0 = g.stats;
        let ghost f0 = g.free.map.view();
        let ghost has_ext = ext@.addr != 0;
        let ghost ve = d0[ext].value();
        let ghost mut vs = ve;
        let ghost mut vh = ve;
        let ghost mut head = false;
        let ghost mut t1 = t0;
        let ghost mut t2 = t0;
        let ghost mut s1 = st0;
        let ghost mut s2 = st0;
        let ghost mut vr = ve;
        proof {
            old(g).lemma_wf_for_take();
            // The table's keys lie inside their extents, which end at the
            // mark; `ext`'s pages belong to no other extent.
            assert(keys_inside(t0, d0, old_mark as int)) by {
                reveal(keys_inside);
                assert forall|k: usize| #[trigger] t0.lookup(k) is Some implies d0.contains_key(t0.lookup(k).unwrap()) && d0[t0.lookup(
                    k,
                ).unwrap()].value().start <= k < d0[t0.lookup(k).unwrap()].value().start + d0[t0.lookup(k).unwrap()].value().pages <= old_mark by {
                    let x = t0.lookup(k).unwrap();
                    assert(is_key(d0[x].value(), k as int));
                    assert(desc_ok(d0[x].value(), old_mark));
                }
            }
            lemma_tally_nonneg(a0, d0, 6);
            if has_ext {
                assert(desc_ok(ve, old_mark));
            }
            // Every free bit is below the mark, so the run's pages past
            // `ext` (or past the mark) have none.
            old(g).lemma_free_below();
        }
        if ext.is_null() {
            if s > old_mark {
                // Alignment left a hole at the mark: a clean free extent.
                let tracked mut sp = spare_slot.tracked_take();
                init_free(spare, Tracked(&mut sp), old_mark, s - old_mark, RunState::Purged, 0);
                proof {
                    if g.descs@.contains_key(spare) {
                        sp.is_disjoint(g.descs.borrow().tracked_borrow(spare));
                    }
                    assert(!d0.contains_key(spare));
                    vs = sp.value();
                    g.descs.borrow_mut().tracked_insert(spare, sp);
                    let tracked sc = spare_claim_slot.tracked_take();
                    g.dclaims.borrow_mut().tracked_insert(spare, sc);
                    g.all@ = g.all@.push(spare);
                    reveal(keys_inside);
                    assert(t0.lookup(old_mark) is None);
                    assert(t0.lookup((s - 1) as usize) is None);
                    let v = g.descs@[spare].value();
                    assert(v.state <= 2);
                    assert(g.table.lookup(v.start as usize) is None);
                    assert(v.state != RunState::Active as u8 && v.pages > 1 ==> g.table.lookup((v.start + v.pages - 1) as usize) is None);
                    assert(g.table.spec_count() + 2 < g.table.spec_cap());
                    assert(g.stats.can_add(state_of(v.state), v.pages as usize));
                    assert(forall|i: int| 0 <= i < l0.len() ==> #[trigger] enode(l0, g.descs@, i) == enode(l0, d0, i));
                }
                g.free.map.set_range(old_mark, s - old_mark);
                g.add_extent(spare);
                spare_used = true;
                proof {
                    t2 = g.table;
                    s2 = g.stats;
                }
            }
            proof {
                assert(took_free(f0, g.free.map.view(), s as int, end as int, old_mark as int));
            }
        } else {
            // `ext` is a live free extent found under this lock.
            let (a, e_end, st, dirty_at) = (g.desc(ext).start(), g.desc(ext).end(), g.desc(ext).state(), g.desc(ext).dirty_at);
            proof {
                assert(e_end <= old_mark);
                lemma_state_byte(ve.state);
                assert(forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && x != ext ==> apart(d0[x].value(), ve));
            }
            // Pages at or past the mark are zero: no run has covered them
            // and they sit behind the last run's guard. A `Purged`
            // extent's pages are zero only once `rediscard` has discarded
            // the run's share of them again: its earlier discard does not
            // survive a stale-pointer write or an underflow from the live
            // run above. Only for a zeroing caller (`zero`; any other caller
            // never pays the call) and only while every purge so far was
            // `MADV_DONTNEED` (`purge::purged_is_zero`): then the call drops
            // just the pages such a write faulted in. After a lazy
            // `MADV_FREE` purge the pages may still be resident, and
            // dropping them would refault them, so the run is reported
            // dirty and the zeroing caller clears it. An `Empty` extent is
            // dirty.
            let usable_end = if end > GUARD_PAGES { end - GUARD_PAGES } else { s };
            let hi = if usable_end < e_end { usable_end } else { e_end };
            zeroed = s >= e_end || (zero && st == RunState::Purged && purge::purged_is_zero() && self.rediscard(s, hi));
            proof {
                assert(zeroed ==> ve.state == RunState::Purged as u8 || s >= ve.start + ve.pages);
                // `rediscard` succeeded for the run's pages in the extent.
                if zeroed && s < e_end {
                    self.lemma_pages(hi);
                }
                assert(zeroed && has_ext && s < ve.start + ve.pages ==> {
                    let h = if s + (need - GUARD_PAGES) <= ve.start + ve.pages { s + (need - GUARD_PAGES) } else { ve.start + ve.pages };
                    &&& self.base + h * self.spec_page() <= usize::MAX
                    &&& sys::madvise_ok((self.base + s * self.spec_page()) as usize, ((h - s) * self.spec_page()) as usize, sys::MADV_DONTNEED)
                });
                lemma_can_sub(*old(g), ext);
            }
            g.drop_extent(ext, Ghost(Seq::empty()));
            proof {
                t1 = g.table;
                s1 = g.stats;
            }
            proof {
                assert(stat(g.stats, 0) <= st0.runs && stat(g.stats, 1) <= st0.empty && stat(g.stats, 2) <= st0.purged);
                assert(stat(g.stats, 3) <= st0.run_pages && stat(g.stats, 4) <= st0.empty_pages && stat(g.stats, 5) <= st0.purged_pages);
                // Nothing is keyed inside `ext` or at or past the mark now.
                assert forall|k: usize| (a <= k < e_end || k >= old_mark) implies #[trigger] t1.lookup(k) is None by {
                    reveal(keys_inside);
                    if t0.lookup(k) is Some {
                        let x = t0.lookup(k).unwrap();
                        if x != ext {
                            assert(apart(d0[x].value(), ve));
                        }
                    }
                }
            }
            let taken_lo = s.max(a);
            let taken_hi = end.min(e_end);
            if taken_lo < taken_hi {
                g.free.map.clear_range(taken_lo, taken_hi - taken_lo);
            }
            proof {
                assert(took_free(f0, g.free.map.view(), s as int, end as int, s as int));
            }
            let tail = e_end.saturating_sub(end);
            if tail > 0 {
                // The tail inherits the state and age of `ext` and, when
                // `Empty`, its place in the FIFO (same `dirty_at`).
                let tracked mut sp = spare_slot.tracked_take();
                init_free(spare, Tracked(&mut sp), end, tail, st, dirty_at);
                proof {
                    if g.descs@.contains_key(spare) {
                        sp.is_disjoint(g.descs.borrow().tracked_borrow(spare));
                    }
                    assert(!d0.contains_key(spare));
                    assert(!l0.contains(spare)) by {
                        reveal(Seq::index_of);
                        if l0.contains(spare) {
                            assert(enode(l0, d0, l0.index_of(spare)));
                        }
                    }
                    vs = sp.value();
                    g.descs.borrow_mut().tracked_insert(spare, sp);
                    let tracked sc = spare_claim_slot.tracked_take();
                    g.dclaims.borrow_mut().tracked_insert(spare, sc);
                    g.all@ = g.all@.push(spare);
                    assert(forall|i: int| 0 <= i < l0.len() ==> #[trigger] enode(l0, g.descs@, i) == enode(l0, d0, i));
                    assert(t1.lookup(end) is None);
                    assert(t1.lookup((e_end - 1) as usize) is None);
                }
                let ghost ds = g.descs@;
                if st == RunState::Empty {
                    g.link_after(ext, spare);
                }
                proof {
                    assert(ds.contains_key(spare) && ds[spare].value() == vs);
                    assert(same_but_links(vs, g.descs@[spare].value()));
                    let v = g.descs@[spare].value();
                    assert(v.state <= 2);
                    assert(g.table.lookup(v.start as usize) is None);
                    assert(v.state != RunState::Active as u8 && v.pages > 1 ==> g.table.lookup((v.start + v.pages - 1) as usize) is None);
                    assert(g.table.spec_count() + 2 < g.table.spec_cap());
                    assert(g.stats.can_add(state_of(v.state), v.pages as usize));
                }
                g.add_extent(spare);
                spare_used = true;
            }
            proof {
                t2 = g.table;
                s2 = g.stats;
            }
            if s > a {
                // Head survives (and, when `s >= e_end`, absorbs the hole
                // between the old extent and the aligned start).
                if s > e_end {
                    g.free.map.set_range(e_end, s - e_end);
                }
                proof {
                    assert(took_free(f0, g.free.map.view(), s as int, end as int, old_mark as int));
                }
                let ghost dm = g.descs@;
                g.desc_mut(ext).pages = (s - a) as u32;
                proof {
                    head = true;
                    vh = HugeDesc { pages: (s - a) as u32, ..ve };
                    assert(dm.contains_key(ext));
                    assert(same_but_links(ve, dm[ext].value()));
                    assert(same_but_links(vh, g.descs@[ext].value()));
                    assert(forall|i: int| 0 <= i < g.elist@.len() ==> #[trigger] enode(g.elist@, g.descs@, i) == enode(g.elist@, dm, i));
                    assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(g.stats, k) <= stat(st0, k) + self.npages by {
                        assert(stat(s2, k) == stat(st0, k) - weight(ve, k) + if spare_used { weight(vs, k) } else { 0 });
                    }
                    assert(stat(g.stats, 1) <= stat(st0, 1) + self.npages && stat(g.stats, 2) <= stat(st0, 2) + self.npages);
                    assert(stat(g.stats, 4) <= stat(st0, 4) + self.npages && stat(g.stats, 5) <= stat(st0, 5) + self.npages);
                    assert(t2.lookup(a) is None);
                    assert(t2.lookup((s - 1) as usize) is None);
                    let v = g.descs@[ext].value();
                    assert(v.state <= 2);
                    assert(g.table.lookup(v.start as usize) is None);
                    assert(v.state != RunState::Active as u8 && v.pages > 1 ==> g.table.lookup((v.start + v.pages - 1) as usize) is None);
                    assert(g.table.spec_count() + 2 < g.table.spec_cap());
                    assert(g.stats.can_add(state_of(v.state), v.pages as usize));
                    assert(perms_ok(g.descs@));
                }
                g.add_extent(ext);
            } else {
                proof {
                    assert(a0.contains(ext));
                    assert(g.descs@.contains_key(ext));
                    assert(same_but_links(ve, g.descs@[ext].value()));
                    assert(g.all@ == a0 || g.all@ == a0.push(spare));
                    assert(g.all@.contains(ext)) by {
                        reveal(Seq::index_of);
                        if g.all@ != a0 {
                            assert(g.all@[a0.index_of(ext)] == ext);
                        }
                    }
                    if ve.state == RunState::Empty as u8 {
                        assert(l0.contains(ext));
                        assert(g.elist@.contains(ext)) by {
                            reveal(Seq::index_of);
                            if g.elist@ != l0 {
                                assert(g.elist@ == l0.insert(l0.index_of(ext) + 1, spare));
                                assert(g.elist@[l0.index_of(ext)] == ext);
                            }
                        }
                    }
                }
                g.retire(meta, ext, Tracked(ord));
            }
            proof {
                assert(took_free(f0, g.free.map.view(), s as int, end as int, old_mark as int));
            }
        }
        if end > old_mark {
            g.next_page = end;
        }
        let ghost t3 = g.table;
        let ghost s3 = g.stats;
        let ghost dr = g.descs@;
        let ghost ar = g.all@;
        proof {
            assert(stat(g.stats, 0) == stat(st0, 0) && stat(g.stats, 3) == stat(st0, 3));
            assert(t3.lookup(s) is None);
        }
        // `run` is a fresh metadata block for a `HugeDesc`.
        let tracked mut rp = run_perm;
        ptr_mut_write(
            run,
            Tracked(&mut rp),
            HugeDesc {
                start: s as u32,
                pages: need as u32,
                arena,
                state: RunState::Active as u8,
                _pad: 0,
                usable: (need - GUARD_PAGES) << self.lg_page,
                dirty_at: 0,
                prev: ptr::null_mut(),
                next: ptr::null_mut(),
            },
        );
        proof {
            if g.descs@.contains_key(run) {
                rp.is_disjoint(g.descs.borrow().tracked_borrow(run));
            }
            vr = rp.value();
            self.lemma_pages((need - GUARD_PAGES) as usize);
            assert(run_ok(vr, g.lg@));
            g.descs.borrow_mut().tracked_insert(run, rp);
            g.dclaims.borrow_mut().tracked_insert(run, run_claim);
            g.all@ = g.all@.push(run);
            assert(forall|i: int| 0 <= i < g.elist@.len() ==> #[trigger] enode(g.elist@, g.descs@, i) == enode(g.elist@, dr, i));
        }
        g.add_extent(run);
        let ghost s4 = g.stats;
        g.stats.next_page = g.next_page;
        if !spare_used {
            let tracked sp = spare_slot.tracked_unwrap();
            let tracked sc = spare_claim_slot.tracked_unwrap();
            free_desc(meta, spare, Tracked(desc_raw((sp, sc))), Tracked(ord));
        }
        proof {
            let t = Take {
                ext,
                has_ext,
                ve,
                head,
                vh,
                used: spare_used,
                spare,
                vs,
                run,
                vr,
                mark: old_mark,
                nm: g.next_page,
            };
            // The pieces replace what they cover; scoped, so the steps below
            // see only the conclusion.
            assert(g.took(*old(g), t)) by {
                assert(t.pieces_ok(*old(g)));
                assert forall|k: usize| #[trigger] g.table.lookup(k) == t.lookup(t0, k) by {
                    assert(g.table.lookup(k) == if is_key(vr, k as int) { Some(run) } else { t3.lookup(k) });
                    assert(t3.lookup(k) == if head && is_key(vh, k as int) { Some(ext) } else { t2.lookup(k) });
                    assert(t2.lookup(k) == if spare_used && is_key(vs, k as int) { Some(spare) } else { t1.lookup(k) });
                    assert(t1.lookup(k) == if has_ext && is_key(ve, k as int) { None } else { t0.lookup(k) });
                }
                assert(g.table.spec_count() == t0.spec_count() + t.delta(6));
                assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(g.stats, k) == stat(st0, k) + t.delta(k) by {
                    assert(stat(s4, k) == stat(s3, k) + weight(vr, k));
                    assert(stat(g.stats, k) == stat(s4, k));
                    assert(stat(s3, k) == stat(s2, k) + if head { weight(vh, k) } else { 0 });
                    assert(stat(s2, k) == stat(s1, k) + if spare_used { weight(vs, k) } else { 0 });
                    assert(stat(s1, k) == stat(st0, k) - if has_ext { weight(ve, k) } else { 0 });
                }
                assert forall|x: *mut HugeDesc| #[trigger] g.descs@.contains_key(x) <==> t.model(d0, x) is Some by {
                    assert(x != run ==> (dr.contains_key(x) <==> t.model(d0, x) is Some));
                }
                assert forall|x: *mut HugeDesc| #[trigger] g.descs@.contains_key(x) implies same_but_links(
                    t.model(d0, x).unwrap(),
                    g.descs@[x].value(),
                ) by {
                    if x != run {
                        assert(same_but_links(t.model(d0, x).unwrap(), dr[x].value()));
                    }
                }
                assert(g.all@ =~= t.all_after(a0)) by {
                    reveal(Seq::no_duplicates);
                    reveal(Seq::index_of);
                }
                assert(g.elist@ == t.elist_after(l0));
                assert(g.table.wf());
                assert(perms_ok(g.descs@));
                assert(g.next_page == t.nm && g.np == old(g).np && g.mid == old(g).mid);
                assert(g.free.map.wf() && g.free.map.in_meta() && g.free.map.spec_nbits() == g.np@);
                assert(elist_ok(g.empty_head, g.empty_tail, g.elist@, g.descs@));
            }
            // The run's token: its extent and pages enter the run
            // authorities, which held neither.
            lemma_took_active(*old(g), *g, t);
            let ghost gt = *g;
            let tracked ext = g.runs.borrow_mut().insert((s as int, need as int));
            let tracked pages = g.pgs.borrow_mut().insert_set(vstd::set_lib::set_int_range(s as int, s + need));
            tok.tracked_insert(0, RunToken { ext, pages });
            assert(*g == (Inner { runs: g.runs, pgs: g.pgs, ..gt }));
            assert(g.took(*old(g), t));
            assert(t.touch_ok(*old(g)));
            lemma_took_wf(*old(g), *g, t);
        }
        proof {
            assert(!has_ext ==> s >= old_mark);
            assert(has_ext ==> ve.start <= s);
            assert(has_ext ==> (s + (need - GUARD_PAGES) + GUARD_PAGES <= ve.start + ve.pages || ve.start + ve.pages == old_mark));
            assert(zeroed && has_ext ==> ve.state == RunState::Purged as u8 || s >= ve.start + ve.pages);
            assert(zeroed && has_ext && s < ve.start + ve.pages ==> {
                let h = if s + (need - GUARD_PAGES) <= ve.start + ve.pages { s + (need - GUARD_PAGES) } else { ve.start + ve.pages };
                &&& self.base + h * self.spec_page() <= usize::MAX
                &&& sys::madvise_ok((self.base + s * self.spec_page()) as usize, ((h - s) * self.spec_page()) as usize, sys::MADV_DONTNEED)
            });
            self.lemma_page();
        }
        let tracked z = zero_run(self.base as int, self.spec_page(), s as int, (need - GUARD_PAGES) as int, old_mark as int, has_ext, ve, zeroed);
        (zeroed, Tracked(z))
    }
}

} // verus!

impl Huge {
    /// [`Huge::free_run`] returning only the usable bytes released.
    #[cfg(test)]
    pub fn free(&self, meta: &Meta, page: usize) -> usize {
        self.free_run(meta, page, Tracked::assume_new(), Tracked::assume_new())
            .usable
    }
}

impl Huge {
    /// Oldest `Empty` extent's `dirty_at`, read under the lock (the decay
    /// driver reads the copy in [`Huge::oldest_empty`] instead).
    #[cfg(test)]
    pub fn oldest_dirty_at(&self) -> Option<u64> {
        let g = self.inner.lock_t();
        if g.empty_head.is_null() {
            None
        } else {
            // SAFETY: list members are live.
            Some(unsafe { (*g.empty_head).dirty_at })
        }
    }

    /// Walk every structure and check the invariants; for tests.
    #[cfg(test)]
    fn audit(&self) {
        let g = self.inner.lock_t();
        let st = g.stats;
        assert_eq!(
            st.run_pages + st.empty_pages + st.purged_pages,
            st.next_page,
            "{st:?}"
        );
        assert_eq!(st.next_page, g.next_page);
        let mut p = 0;
        let mut counts = HugeStats::default();
        let mut prev_free = false;
        let mut keys = 0;
        while p < g.next_page {
            let d = g.table.get(p);
            assert!(!d.is_null(), "page {p} has no extent");
            // SAFETY: a non-null table entry is a live extent descriptor, and `g`
            // holds the huge lock.
            let (start, pages, state) = unsafe { ((*d).start(), (*d).pages(), (*d).state()) };
            assert_eq!(start, p, "extent at {p} starts at {start}");
            assert!(pages > 0);
            keys += 1;
            counts.add(state, pages);
            match state {
                RunState::Active => {
                    assert_eq!(
                        // SAFETY: as above.
                        unsafe { (*d).usable },
                        (pages - GUARD_PAGES) << self.lg_page
                    );
                    for q in p..p + pages {
                        assert!(!g.free.map.is_set(q), "page {q} of run {p} marked free");
                    }
                    prev_free = false;
                }
                _ => {
                    assert!(!prev_free, "adjacent free extents at {p}");
                    for q in p..p + pages {
                        assert!(
                            g.free.map.is_set(q),
                            "page {q} of free extent {p} not marked free"
                        );
                    }
                    if pages > 1 {
                        assert_eq!(
                            g.table.get(p + pages - 1),
                            d,
                            "last-page key of {p} missing"
                        );
                        keys += 1;
                    }
                    prev_free = true;
                }
            }
            p += pages;
        }
        assert_eq!(p, g.next_page);
        assert_eq!((counts.runs, counts.run_pages), (st.runs, st.run_pages));
        assert_eq!(
            (counts.empty, counts.empty_pages),
            (st.empty, st.empty_pages)
        );
        assert_eq!(
            (counts.purged, counts.purged_pages),
            (st.purged, st.purged_pages)
        );
        assert_eq!(keys, g.table.count, "table holds stale keys");
        assert_eq!(g.free.map.lowest_set_from(g.next_page), None);
        // The Empty list holds exactly the Empty extents, oldest first.
        let mut n = 0;
        let mut d = g.empty_head;
        let mut last_dirty = 0;
        let mut prev = ptr::null_mut();
        while !d.is_null() {
            // SAFETY: the Empty list links live descriptors; `g` holds the lock.
            unsafe {
                assert_eq!((*d).state(), RunState::Empty);
                assert!((*d).dirty_at >= last_dirty, "empty list out of order");
                assert_eq!((*d).prev, prev);
                last_dirty = (*d).dirty_at;
                prev = d;
                d = (*d).next;
            }
        }
        assert_eq!(prev, g.empty_tail);
        d = g.empty_head;
        while !d.is_null() {
            n += 1;
            // SAFETY: as above.
            d = unsafe { (*d).next };
        }
        assert_eq!(n, st.empty);
    }
}

verus! {

#[cold]
#[inline(never)]
fn not_a_run(p: usize) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!("invalid free of {p:#x}: not the start of a huge allocation"))
}

#[cold]
#[inline(never)]
fn already_free(p: usize) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!("double free of {p:#x}: huge allocation already freed"))
}

/// The `check_zero` abort: a huge run handed out as zero has a dirty word.
#[cold]
#[inline(never)]
fn dirty_zero_run(
    run: usize,
    pages: usize,
    page: usize,
    prior: &str,
    arena: u16,
    at: usize,
    word: u64,
) -> !
    requires
        crate::sys::may_abort(),
{
    let advice = purge::advice_name();
    sys::abort_fmt(format_args!(
        "check_zero: huge run {run:#x} ({pages} pages from page {page}, arena {arena}) was handed out as zero from {prior} pages but the word at {at:#x} reads {word:#x} (purge advice {advice})"
    ))
}

} // verus!

verus! {

/// The huge lock's invariant is `Inner::wf`, and its frame is the
/// ghost page count, page shift, region base, `Meta` identity and the
/// identities of the run authorities (`Inner::spec_rid`, `spec_pid`), which
/// `Huge::new` sets and nothing changes. Read the invariant as: the real state always
/// has a ghost witness (the descriptor permissions and their blocks'
/// claims, the list of live descriptors, the `Empty` list's order, and the
/// run authorities whose sets are the `Active` extents and their pages)
/// that makes `wf` true.
///
/// `Huge::new` builds the value (`Lock::new` requires `wf`), and both
/// are checked at every release: each holder outside the tests
/// (`alloc_aligned`, `free_run`, `usable_size`, `lookup`, `purge_empty`,
/// `purge_curve`, `empty_bytes` and `stats`) ends with
/// `LockGuard::unlock`, which requires both, on every path to a return
/// (`api::tests::guards_end_in_unlock`). A panic aborts
/// (`panic = "abort"`) outside the tests, so it never releases. The
/// test-only `oldest_dirty_at` and `audit` only read. The fork handlers
/// take and release the raw lock without touching the value, and nothing
/// reads it without a guard. What is left is the mutex contract
/// (`lock::axiom_acquired`).
///
/// `api::tests::lock_holders_match_inv_docs` checks that the functions
/// that take the guard are the holders named here.
impl crate::lock::LockSpec for Inner {
    closed spec fn lock_spec_inv(self) -> bool {
        self.wf()
    }

    closed spec fn lock_spec_frame(init: Self, v: Self) -> bool {
        &&& init.spec_np() == v.spec_np()
        &&& init.spec_lg() == v.spec_lg()
        &&& init.spec_base() == v.spec_base()
        &&& init.spec_mid() == v.spec_mid()
        &&& init.spec_rid() == v.spec_rid()
        &&& init.spec_pid() == v.spec_pid()
    }
}

/// The frame holds of every value with itself, as `LockSpec` requires of
/// a defined frame (`Lock::new` does not check it, and
/// `lock::axiom_acquired` claims it at the first acquisition).
proof fn lemma_huge_frame_reflexive(v: Inner)
    ensures
        crate::lock::lock_frame(v, v),
{
}

} // verus!

#[cfg(test)]
mod tests;

#[cfg(test)]
mod linux_tests;
