//! Huge allocations (design.md section 7).
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
//! one. Otherwise the guard page is left (or put back) `PROT_NONE`, which
//! splits the VMA but costs nothing else, and every allocation re-commits
//! its usable range so stale `PROT_NONE` guards inside a reused extent are
//! made writable again.
//!
//! Alignment above a page is met in `find`: the start of each free extent
//! (and finally the high-water mark) is rounded up to the alignment and the
//! extent is taken when the whole run still fits below its end, or when
//! the extent touches the mark and can grow; `take` leaves the pages
//! before the aligned start and any left after the run as free extents.
//! Runs whose usable size exceeds `retain_max` (default 64 MiB) are
//! `MADV_DONTNEED`ed the moment they are freed (every run under
//! `huge_dirty_decay_ms:0`, [`Huge::set_purge_on_free`]); smaller ones
//! become `Empty` and wait for the decay walk ([`Huge::decay`], on arena
//! 0's period or the huge tier's own `huge_dirty_decay_ms`, design 7) or a
//! full purge ([`Huge::purge_empty`]). `Empty` extents sit in a FIFO list ordered by
//! `dirty_at`, so purging is a pop from the head.
//!
//! Everything is under one futex lock: huge operations are rare compared to
//! slab operations and each already costs at least one syscall.
//!
//! Submodules: [`table`] is the open-addressing descriptor table
//! (`Table`), `tests` the allocator's tests and `linux_tests` the ones
//! that need the process's own regions.

use crate::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::alloc::Layout;
use core::ptr;

use crate::class::Set;
use crate::layout::Regions;
use crate::lock::{Lock, RawLock};
use crate::meta::Meta;
use crate::purge::{self, Batch, Curve, Decayer};
use crate::sys::{self, Errno};
#[cfg(verus_only)]
use crate::vspec::huge::{axiom_huge_lock_frame, axiom_huge_lock_inv};
#[cfg(verus_only)]
use crate::vspec::lock::lock_init;
#[cfg(verus_only)]
use crate::vspec::slice::array_iter_rest;
use crate::vspec::zero::Zeroed;
#[cfg(verus_only)]
use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;
#[cfg(verus_only)]
use vstd::raw_ptr::PointsToRaw;
use vstd::raw_ptr::{PointsTo, ptr_mut_ref, ptr_mut_write, ptr_ref};
use vstd::resource::set::{GhostSetAuth, GhostSingleton, GhostSubset};

mod table;

use table::Table;

verus! {

/// Guard pages after every run.
pub const GUARD_PAGES: usize = 1;

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
    /// zero memory.
    #[inline]
    fn from_u8(v: u8) -> (r: RunState)
        ensures
            v <= 2 && r == state_of(v),
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

/// Descriptor of one extent (design section 7's `HugeDesc`). Allocated from
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

// Checked by the compiler; `PointsTo::is_disjoint` needs the size, and
// `PointsToRaw::into_typed` (`desc_perm`) the alignment.
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

    /// Aborts unless the state byte is valid.
    #[inline]
    pub fn state(&self) -> (r: RunState)
        ensures
            self.spec_state() <= 2 && r == state_of(self.spec_state()),
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

    /// Uncount one extent of `pages` in `state`.
    fn sub(&mut self, state: RunState, pages: usize)
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
            state == RunState::Active ==> old(self).runs >= 1 && old(self).run_pages >= pages,
            state == RunState::Empty ==> old(self).empty >= 1 && old(self).empty_pages >= pages,
            state == RunState::Purged ==> old(self).purged >= 1 && old(self).purged_pages >= pages,
    {
        // A count going below zero means the extents and the counters
        // disagree; the release profile does not check overflow, so the
        // subtractions are explicit.
        let sub = |x: usize, y: usize| -> (r: usize)
            ensures
                x >= y && r == x - y,
            { x.checked_sub(y).unwrap_or_else(|| -> (r: usize) ensures false { huge_count_underflow(state) }) };
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

#[cold]
#[inline(never)]
pub(crate) fn bad_run_state_byte(v: u8) -> ! {
    sys::abort_fmt(format_args!("huge: run state byte {v:#x} is not a run state"))
}

#[cold]
#[inline(never)]
pub(crate) fn huge_count_underflow(state: RunState) -> ! {
    sys::abort_fmt(format_args!("huge: {state:?} extent count underflow"))
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
    /// Ghost: the authority of the `Active` descriptors' extents
    /// ([`run_loc`]); each run's holder has its [`RunToken`].
    #[allow(dead_code)]
    runs: Tracked<GhostSetAuth<(int, int)>>,
    /// Ghost: the authority of the `Active` descriptors' pages
    /// ([`page_loc`]).
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

/// The 48 bytes of the descriptor at `d` are metadata
/// ([`Regions::in_meta`]).
pub open spec fn desc_in_meta(d: *mut HugeDesc) -> bool {
    crate::layout::process_regions().in_meta(d@.addr as int, vstd::layout::size_of::<HugeDesc>() as int)
}

/// The authority of the live runs' extents ([`RunToken`]): one per
/// process, the huge allocator's ([`Huge::new`], `axiom_run_locs`).
pub uninterp spec fn run_loc() -> vstd::resource::Loc;

/// The authority of the live runs' pages ([`RunToken`]).
pub uninterp spec fn page_loc() -> vstd::resource::Loc;

/// Trusted: [`Huge::new`] runs once per process, for the process's
/// `Shared` (`Shared::init`), so the run authorities it makes are the
/// process's ([`run_loc`], [`page_loc`]), as `class::table::axiom_slot_loc`
/// for the slot authorities. One call, in `Huge::new`.
axiom fn axiom_run_locs(r: vstd::resource::Loc, p: vstd::resource::Loc)
    ensures
        run_loc() == r,
        page_loc() == p,
;

/// Ghost token of a live huge run: its extent `(start, npages)`, the run's
/// first page and its page count with the guard, as a singleton of the
/// extents' authority ([`run_loc`]), and its pages as a subset of the
/// pages' authority ([`page_loc`]). [`Huge::alloc_run`] hands one out with
/// every run and [`Huge::free_run`] takes it back; the huge lock's
/// invariant ([`Inner::wf`]) holds the authorities, whose sets are the
/// `Active` descriptors' extents and pages. Two tokens' pages are
/// disjoint ([`RunToken::lemma_apart`]), whoever holds them.
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

    /// Purge never discards live data, for huge runs (whole-allocator
    /// theorem "Purge never discards live data", `verus/THEOREMS.md`): a
    /// byte [`free_page_byte`] admits for the set of the page authority
    /// `auth` is in no page, guard included, of the run whose token this
    /// is, whoever holds it. Every huge purge's [`Batch`] (`Huge::purge_empty`,
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

/// Byte `b` lies in one of the `np` pages of `pg` bytes of the region at
/// `base`, and that page is not in `held`, the pages the run authority
/// holds ([`page_loc`]): a page of no live run. What a huge purge's
/// [`Batch`] may discard (whole-allocator theorem "Purge never discards
/// live data", `verus/THEOREMS.md`; [`RunToken::theorem_purge_apart`]).
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

/// Every page of every free extent is in the free-page set `f` (what
/// [`Huge::find`] needs to see every free extent).
spec fn free_marked(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, f: vstd::set::Set<nat>) -> bool {
    forall|d: *mut HugeDesc| #[trigger] descs.contains_key(d) && descs[d].value().state != RunState::Active as u8 ==> marked(
        f,
        descs[d].value(),
    )
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

/// Page `p` lies in `x`, a free extent of `descs`.
spec fn in_free(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, x: *mut HugeDesc, p: int) -> bool {
    &&& descs.contains_key(x)
    &&& descs[x].value().state != RunState::Active as u8
    &&& descs[x].value().start <= p < descs[x].value().start + descs[x].value().pages
}

/// Page `p` can hold part of a new run: it is at or past the mark
/// `next_page` (never handed out) or a page of a free extent.
spec fn page_free(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, next_page: int, p: int) -> bool {
    p >= next_page || exists|x: *mut HugeDesc| #[trigger] in_free(descs, x, p)
}

/// Every free extent starting below `from` ends at or before it.
spec fn ends_by(descs: Map<*mut HugeDesc, PointsTo<HugeDesc>>, from: int) -> bool {
    forall|x: *mut HugeDesc| #[trigger] descs.contains_key(x) && descs[x].value().state != RunState::Active as u8
        && descs[x].value().start < from ==> descs[x].value().start + descs[x].value().pages <= from
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

impl Inner {
    /// The invariant of the huge lock: the table is well formed and every
    /// descriptor it or the `Empty` list names is live and consistent;
    /// the live descriptors (`all`) are the extents, pairwise disjoint,
    /// each under its keys in the table and nowhere else, and the counters
    /// are their tallies, whose pages sum to `next_page`; the run
    /// authorities ([`RunToken`]) hold the `Active` extents and their pages;
    /// no two free extents touch, and every page of a free extent is in
    /// `free`.
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
        // Implied by the tallies (`lemma_tally_counts`); stated for the
        // readers that bound the counts without the lemma.
        &&& self.stats.runs + self.stats.empty + self.stats.purged <= self.next_page
        &&& self.free.map.wf()
        &&& self.free.map.in_meta()
        &&& self.free.map.spec_nbits() == self.np@
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
        &&& self.runs@.id() == run_loc()
        &&& self.pgs@.id() == page_loc()
        &&& self.runs@@ == active_runs(self.descs@)
        &&& self.pgs@@ == active_pages(self.descs@)
        &&& coalesced(self.descs@)
        &&& free_marked(self.descs@, self.free.map.view())
    }

    /// Huge free extents are maximally coalesced (`verus/THEOREMS.md`): in
    /// every value the huge lock admits, no two free extents (`Empty` or
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

    /// Guard pages are never handed out, for huge runs (whole-allocator
    /// theorem 7, `verus/THEOREMS.md`): in every value the huge lock
    /// admits, the usable pages of a live run (`Active`, the pages before
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
    /// metadata (whole-allocator theorem 4): every live descriptor
    /// ([`alloc_desc`] takes each from the process's `Meta`), every slot
    /// of the descriptor table (`Table::storage`) and every word of the
    /// free-page bitmap (`Set::new`).
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

    /// Everything but the descriptors' links and the list is as in `o`.
    spec fn frame(&self, o: &Inner) -> bool {
        &&& self.free == o.free
        &&& self.table == o.table
        &&& self.next_page == o.next_page
        &&& self.stats == o.stats
        &&& self.np == o.np
        &&& self.lg == o.lg
        &&& self.all == o.all
        &&& self.runs == o.runs
        &&& self.pgs == o.pgs
        &&& links_only(o.descs@, self.descs@)
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
    /// [`retire`](Inner::retire) it. Ghost: the live descriptors in `others`
    /// other than `d` are at other addresses than `d`, which the caller's
    /// pointer comparisons need.
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

    /// Neighbouring free extent ending exactly at `page`, if any: a free
    /// extent of a page or more keyed in the table that ends there is it.
    fn free_before(&self, page: usize) -> (r: *mut HugeDesc)
        requires
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
}

/// The huge allocator: one per process, shared by every arena (design
/// section 7); the arena an allocation was made through is recorded per
/// run for the per-arena statistics.
pub struct Huge {
    base: usize,
    lg_page: u32,
    npages: usize,
    guard_install: bool,
    retain_max: AtomicUsize,
    /// `huge_dirty_decay_ms:0` (design 7): a freed run is purged as it is
    /// freed, whatever its size, instead of waiting in the `Empty` list
    /// for a decay pass; set once at init (`Shared::init`).
    purge_on_free: AtomicBool,
    /// The decay step gate (`purge::Decayer`): `dirty_at` of the head of
    /// the `Empty` list (its oldest member), kept in step with the list
    /// under the lock by `sync_oldest` and read without it by the decay
    /// driver, and the next walk's deadline.
    gate: Decayer,
    inner: Lock<Inner>,
}

impl Huge {
    /// The region's geometry: `npages` pages of `2^lg_page` bytes from
    /// `base`, below the end of the user address space.
    #[verifier::type_invariant]
    spec fn inv(&self) -> bool {
        &&& self.lg_page < 64
        &&& 4 <= self.lg_page
        &&& 0 < self.npages <= u32::MAX
        &&& self.base + self.npages * pow2(self.lg_page as nat) <= crate::meta::va_limit()
        &&& lock_init(self.inner).spec_np() == self.npages
        &&& lock_init(self.inner).spec_lg() == self.lg_page
    }
}

} // verus!

// SAFETY: the raw pointers address the metadata region, which is process
// global; the `Lock` serialises every access.
unsafe impl Send for Inner {}

verus! {

/// `PROT_NONE`, `PROT_READ | PROT_WRITE` and the guard advice of
/// `sys` (checked against them below).
const PROT_NONE: i32 = 0;
const PROT_RW: i32 = 3;
const MADV_GUARD_INSTALL: i32 = 102;
const MADV_GUARD_REMOVE: i32 = 103;

fn commit(addr: usize, len: usize) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
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

fn guard_install(addr: usize, len: usize) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: as `protect_none`.
    unsafe { sys::madvise(addr, len, MADV_GUARD_INSTALL) }
}

fn guard_remove(addr: usize, len: usize) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: the run is being freed; the guard protects nothing now.
    unsafe { sys::madvise(addr, len, MADV_GUARD_REMOVE) }
}

fn now_ns() -> u64
    requires
        crate::sys::may_syscall(),
{
    sys::now_coarse_ns()
}

} // verus!

const _: () = assert!(
    PROT_NONE == sys::PROT_NONE
        && PROT_RW == sys::PROT_READ | sys::PROT_WRITE
        && MADV_GUARD_INSTALL == sys::MADV_GUARD_INSTALL
        && MADV_GUARD_REMOVE == sys::MADV_GUARD_REMOVE
);

verus! {

/// A block for a descriptor from `meta`, with the permission to it
/// (`Meta::alloc_raw`'s, typed); null, with none, when the metadata region
/// is exhausted. The block is `meta`'s, so metadata when `meta` is the
/// process's.
#[inline(always)]
fn alloc_desc(meta: &Meta) -> (r: (*mut HugeDesc, Tracked<Option<PointsTo<HugeDesc>>>))
    requires
        crate::sys::may_syscall(),
    ensures
        r.0@.addr != 0 ==> r.1@ is Some && r.1@->0.ptr() == r.0 && r.1@->0.is_uninit(),
        r.0@.addr != 0 ==> meta.holds(r.0@.addr as int, vstd::layout::size_of::<HugeDesc>() as int),
        r.0@.addr != 0 && meta.is_process() ==> desc_in_meta(r.0),
{
    proof {
        crate::meta::lemma_consts();
    }
    let (p, Tracked(raw)) = meta.alloc_raw(Layout::new::<HugeDesc>());
    let d = p as *mut HugeDesc;
    let tracked perm = desc_perm(raw, d);
    (d, Tracked(perm))
}

/// The descriptor permission of a block of `raw` at `d`, when there is a
/// block.
proof fn desc_perm(tracked raw: PointsToRaw, d: *mut HugeDesc) -> (tracked r: Option<PointsTo<HugeDesc>>)
    ensures
        d@.addr != 0 && raw.is_range(d@.addr as int, vstd::layout::size_of::<HugeDesc>() as int)
            && raw.provenance() == d@.provenance && d@.addr as int % vstd::layout::align_of::<HugeDesc>() as int == 0
            ==> r is Some && r->0.ptr() == d && r->0.is_uninit(),
{
    if d@.addr != 0 && raw.is_range(d@.addr as int, vstd::layout::size_of::<HugeDesc>() as int)
        && raw.provenance() == d@.provenance && d@.addr as int % vstd::layout::align_of::<HugeDesc>() as int == 0 {
        let tracked perm = raw.into_typed::<HugeDesc>(d@.addr);
        assert(perm.ptr()@ == d@);
        Some(perm)
    } else {
        None
    }
}

/// Give the block `d` from [`alloc_desc`] back to `meta`, with its
/// permission, which is dropped: `Meta::free` takes the block without
/// one, and its caller contract (the block is no longer used) is trusted
/// there.
#[inline(always)]
fn free_desc(meta: &Meta, d: *mut HugeDesc, _perm: Tracked<PointsTo<HugeDesc>>)
    requires
        crate::sys::may_syscall(),
        d@.addr != 0,
        _perm@.ptr() == d,
{
    meta.free(d as *mut u8, Layout::new::<HugeDesc>());
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

/// The zero token of a run `Huge::take` placed at page `s` with `usable`
/// pages before its guard, over a region at `base` with pages of `pg`
/// bytes whose high-water mark was `mark`: empty unless `zeroed`, which
/// `take` sets for a run from fresh pages (no extent, or a start at or
/// past the extent it touches, which then ends at the mark) or from a
/// `Purged` extent while `purge::purged_is_zero` read true under the lock
/// (`ve` the extent). Pages at or past the mark are fresh
/// (`vspec::sys::axiom_fresh_anon_zero`: only `take` raises the mark, and
/// no run has covered them); the pages of a `Purged` extent were
/// discarded with `MADV_DONTNEED` (`vspec::sys::axiom_discarded_zero`),
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
    ensures
        zeroed ==> z.addr() == base + s * pg && z.len() == usable * pg,
{
    if !zeroed {
        Zeroed::empty(0)
    } else if !has_ext || s >= ve.start + ve.pages {
        // At or past the mark.
        assert(s >= mark);
        crate::vspec::sys::axiom_fresh_anon_zero(base + s * pg, usable * pg)
    } else if s + usable <= ve.start + ve.pages {
        // Inside the `Purged` extent.
        assert(ve.state == RunState::Purged as u8);
        crate::vspec::sys::axiom_discarded_zero(base + s * pg, usable * pg)
    } else {
        // The `Purged` extent up to the mark, then fresh pages.
        let e = ve.start + ve.pages;
        assert(ve.state == RunState::Purged as u8 && e == mark);
        let tracked lo = crate::vspec::sys::axiom_discarded_zero(base + s * pg, (e - s) * pg);
        let tracked hi = crate::vspec::sys::axiom_fresh_anon_zero(base + e * pg, (s + usable - e) * pg);
        assert(base + s * pg + (e - s) * pg == base + e * pg) by (nonlinear_arith);
        assert((e - s) * pg + (s + usable - e) * pg == usable * pg) by (nonlinear_arith);
        assert((e - s) * pg >= 0 && (s + usable - e) * pg >= 0) by (nonlinear_arith)
            requires s < e, e < s + usable, pg > 0;
        lo.join(hi)
    }
}

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
    spec fn touch_ok(self, g0: Inner) -> bool {
        &&& self.head ==> self.vh.start + self.vh.pages == self.vr.start
        &&& self.has_ext && self.used ==> self.vs.start == self.vr.start + self.vr.pages && self.vs.start + self.vs.pages
            == self.ve.start + self.ve.pages
        &&& !self.has_ext && self.used ==> self.vs.start == self.mark && self.vs.start + self.vs.pages == self.vr.start
        &&& !self.has_ext ==> none_ends_at(g0.descs@, self.mark as int)
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
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) <==> t.model(g0.descs@, x) is Some
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) ==> same_but_links(
            t.model(g0.descs@, x).unwrap(),
            self.descs@[x].value(),
        )
        &&& self.all@ == t.all_after(g0.all@)
        &&& forall|k: int| 0 <= k < 6 ==> #[trigger] stat(self.stats, k) == stat(g0.stats, k) + t.delta(k)
        &&& self.next_page == t.nm
        &&& self.np == g0.np
        &&& self.lg == g0.lg
        &&& self.free.map.wf()
        &&& self.free.map.in_meta()
        &&& self.free.map.spec_nbits() == self.np@
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
/// crate; the rlimit leaves it headroom.
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
/// The explicit rlimit: this proof is sensitive to crate-wide context and
/// went past the default once unrelated ghost state was added elsewhere.
#[verifier::rlimit(40)]
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
    assert forall|i: int| 0 <= i < l0.len() implies #[trigger] d0.contains_key(l0[i]) && d0[l0[i]].value().state == RunState::Empty as u8 by {
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

/// The free-page set after a take, against `f0` before: what was free
/// outside the run `[lo, hi)` still is, and so is the stretch `[mark, s)`
/// alignment skipped at the mark (empty unless the run starts past it).
spec fn took_free(f0: vstd::set::Set<nat>, f1: vstd::set::Set<nat>, lo: int, hi: int, mark: int) -> bool {
    forall|q: nat| (f0.contains(q) && !(lo <= q < hi)) || (mark <= q < lo) ==> #[trigger] f1.contains(q)
}

/// A take keeps the free extents coalesced and marked free: the head
/// ends at the run, a tail starts at its end and ends where `ext` did,
/// a hole starts at the mark, which no free extent touched, and ends at
/// the run; their pages, and the old free extents' (apart from the run),
/// are in the free-page set.
proof fn lemma_took_coalesced(g0: Inner, g1: Inner, t: Take)
    requires
        g0.wf(),
        g1.took(g0, t),
        t.touch_ok(g0),
        took_free(g0.free.map.view(), g1.free.map.view(), t.vr.start as int, t.vr.start + t.vr.pages, t.mark as int),
    ensures
        coalesced(g1.descs@),
        free_marked(g1.descs@, g1.free.map.view()),
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
}

/// A take keeps `Inner::wf`.
proof fn lemma_took_wf(g0: Inner, g1: Inner, t: Take)
    requires
        g0.wf(),
        g1.took(g0, t),
        g1.runs@.id() == run_loc(),
        g1.pgs@.id() == page_loc(),
        g1.runs@@ == active_runs(g1.descs@),
        g1.pgs@@ == active_pages(g1.descs@),
        t.touch_ok(g0),
        took_free(g0.free.map.view(), g1.free.map.view(), t.vr.start as int, t.vr.start + t.vr.pages, t.mark as int),
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
    /// Give a dropped descriptor back to `meta`, leaving the `Empty` list
    /// first when it is a member (every `Empty` extent is; a freed run's
    /// descriptor still reads `Active` here and is in no list).
    fn retire(&mut self, meta: &Meta, d: *mut HugeDesc)
        requires
            crate::sys::may_syscall(),
            perms_ok(old(self).descs@),
            elist_ok(old(self).empty_head, old(self).empty_tail, old(self).elist@, old(self).descs@),
            old(self).descs@.contains_key(d),
            old(self).all@.contains(d),
            old(self).descs@[d].value().state == RunState::Empty as u8 ==> old(self).elist@.contains(d),
        ensures
            final(self).descs@.dom() == old(self).descs@.dom().remove(d),
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
            final(self).runs == old(self).runs,
            final(self).pgs == old(self).pgs,
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
        // `d` is live and about to be discarded.
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
        free_desc(meta, d, Tracked(perm));
    }

    /// The free extent starting at `page`, which must be a set free bit.
    /// Aborts unless the table has a free extent starting there.
    fn extent_at(&self, page: usize) -> (r: *mut HugeDesc)
        requires
            self.wf(),
        ensures
            self.descs@.contains_key(r),
            self.descs@[r].value().start == page,
            self.descs@[r].value().state != RunState::Active as u8,
    {
        let d = self.table.get(page);
        // Table entries are live descriptors.
        if d.is_null() || self.desc(d).start() != page || !self.desc(d).state().is_free() {
            sys::abort_fmt(format_args!("huge: free page {page} has no extent"));
        }
        d
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

impl Huge {
    /// Create the huge allocator for `regions`, reserving its free-page
    /// bitmap and descriptor table in `meta`. `guard_install` should be the
    /// probed `MADV_GUARD_INSTALL` support. `None` if the metadata region
    /// cannot hold the tables.
    pub fn new(meta: &Meta, regions: &Regions, guard_install: bool) -> (r: Option<Huge>)
        requires
            crate::sys::may_syscall(),
            regions.wf(),
            0 < regions.huge_len >> regions.lg_page <= u32::MAX,
            regions.huge_base + regions.huge_len <= crate::meta::va_limit(),
            (1usize << regions.lg_page) >= crate::meta::MIN_ALIGN,
            meta.is_process(),
        ensures
            r matches Some(h) ==> h.spec_npages() == (regions.huge_len >> regions.lg_page) as int
                && h.spec_page() == (1usize << regions.lg_page) as int
                && h.spec_base() == regions.huge_base,
    {
        broadcast use axiom_huge_lock_inv;

        let page = regions.page_size();
        let npages = regions.huge_pages();
        assert!(npages > 0 && npages <= u32::MAX as usize, "huge: region page count out of range");
        proof {
            let lg = regions.lg_page as usize;
            assert((1usize << lg) <= 0x8000_0000_0000_0000usize) by (bit_vector)
                requires lg < 64;
            assert(4 <= lg) by (bit_vector)
                requires lg < 64, (1usize << lg) >= 16;
            vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
            vstd::bits::lemma_usize_shl_is_mul(1, lg);
            assert(page as nat == pow2(lg as nat));
            assert(crate::meta::is_pow2(page));
        }
        let free = Set::new(meta, page, npages)?;
        let table = Table::new(meta, page)?;
        let tracked descs = Map::tracked_empty();
        // The run authorities, empty: no run is live yet.
        let tracked (runs, _) = GhostSetAuth::<(int, int)>::new(vstd::set::Set::empty());
        let tracked (pgs, _) = GhostSetAuth::<int>::new(vstd::set::Set::empty());
        proof {
            axiom_run_locs(runs.id(), pgs.id());
            lemma_active(descs);
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
            guard_install,
            retain_max: AtomicUsize::new(DEFAULT_RETAIN_MAX),
            purge_on_free: AtomicBool::new(false),
            gate: Decayer::new(),
            inner: Lock::new(Inner {
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
                elist: Ghost(Seq::empty()),
                all: Ghost(Seq::empty()),
                np: Ghost(npages),
                lg: Ghost(regions.lg_page),
                runs: Tracked(runs),
                pgs: Tracked(pgs),
            }),
        })
    }

    /// The huge lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> (r: &RawLock)
        ensures
            *r == self.spec_raw_lock(),
    {
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

    /// Pages in the region.
    pub closed spec fn spec_npages(&self) -> int {
        self.npages as int
    }

    /// The address of page 0.
    pub closed spec fn spec_base(&self) -> usize {
        self.base
    }

    /// The base is a page start: `Shared::wf` gives it (the huge base is
    /// `MAX_SLAB`-aligned, `Regions::separated`), and the alignment of the
    /// runs [`Huge::alloc_run`] places rests on it.
    pub open spec fn spec_base_aligned(&self) -> bool {
        (self.spec_base() as int) % self.spec_page() == 0
    }

    /// The usable bytes of a run for `size`: `size` rounded up to a page,
    /// and at least one page.
    pub open spec fn spec_usable(&self, size: usize) -> int {
        let n = (size + self.spec_page() - 1) / self.spec_page();
        (if n == 0 { 1 } else { n }) * self.spec_page()
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

    /// The page size.
    pub closed spec fn spec_page(&self) -> int {
        pow2(self.lg_page as nat) as int
    }

    /// Byte `b` is one a huge purge may discard while the run-page
    /// authority holds `held`: [`free_page_byte`] over this region.
    spec fn purgeable(&self, held: vstd::set::Set<int>, b: int) -> bool {
        free_page_byte(self.base as int, self.spec_page(), self.npages as int, held, b)
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

    /// Kernel side of an allocation of `usable` pages at `s`: commit and
    /// place the guard. Pages below `watermark` are committed already
    /// (apart from `PROT_NONE` guards in fallback mode).
    ///
    /// In guard-install mode every page below the mark stays committed, so
    /// one `mprotect` over `[watermark, end)` is all a run past the mark
    /// needs. That range deliberately starts at the mark, not at `s`: when
    /// alignment puts `s` above the mark, `take` records `[watermark, s)`
    /// as a `Purged` free extent (or folds it into the extent that touched
    /// the mark), and the run later placed in that hole must find its pages
    /// committed. Committing only `[s, end)` here left the hole `PROT_NONE`
    /// and the first write into it faulted; the fallback mode never had
    /// the problem because it commits `[s, s + usable)` on every call.
    fn prepare(&self, s: usize, usable: usize, watermark: usize) -> Result<(), Errno>
        requires
            crate::sys::may_syscall(),
            s + usable + GUARD_PAGES <= self.npages,
            watermark <= self.npages,
    {
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
        let r = if self.guard_install {
            if watermark < end {
                commit(self.addr(watermark), (end - watermark) * page)?;
            }
            guard_install(self.addr(guard), GUARD_PAGES * page)
        } else {
            commit(self.addr(s), usable * page)?;
            if guard < watermark {
                protect_none(self.addr(guard), GUARD_PAGES * page)?;
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

    /// `dirty_at` of the oldest `Empty` extent as the decay driver sees it
    /// (0 when none).
    #[cfg(test)]
    pub fn oldest_empty(&self) -> u64 {
        self.gate.oldest_empty()
    }

    /// Bytes in `Empty` extents (one lock hold), for the cgroup policy's
    /// retention figure: at most the region.
    pub fn empty_bytes(&self) -> (r: usize)
        requires
            crate::sys::may_syscall(),
        ensures
            r <= crate::meta::va_limit(),
    {
        broadcast use {axiom_huge_lock_inv, axiom_huge_lock_frame};

        proof {
            use_type_invariant(self);
        }
        let g = self.inner.lock();
        proof {
            assert(g@.stats.empty_pages <= g@.next_page <= self.npages);
            self.lemma_pages(g@.stats.empty_pages as usize);
        }
        g.stats.empty_pages << self.lg_page
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

    /// `r` is the usable size of a run starting at `page`: whole pages, at
    /// least one, with the run's guard after them inside the region.
    pub open spec fn spec_run_usable(&self, page: usize, r: usize) -> bool {
        &&& r >= self.spec_page()
        &&& (r as int) % self.spec_page() == 0
        &&& page * self.spec_page() + r + GUARD_PAGES * self.spec_page() <= self.spec_npages() * self.spec_page()
    }

    /// Usable bytes of the live run starting at `page`. Aborts otherwise.
    /// The result is the bytes of the run's usable pages, as its descriptor
    /// counts them (`Huge::take` writes `usable` from the pages it commits):
    /// the usable pages of the run token `tok` holds, if it is this run's.
    pub fn usable_size(&self, page: usize, Tracked(tok): Tracked<&Option<RunToken>>) -> (r: usize)
        requires
            crate::sys::may_syscall(),
            page <= self.spec_npages(),
        ensures
            page < self.spec_npages(),
            self.spec_run_usable(page, r),
            tok is Some && tok.unwrap().wf() && tok.unwrap().start() == page ==> r == (tok.unwrap().npages() - GUARD_PAGES) * self.spec_page(),
    {
        broadcast use {axiom_huge_lock_inv, axiom_huge_lock_frame};

        proof {
            use_type_invariant(self);
            self.lemma_page();
        }
        let g = self.inner.lock();
        let d = g.table.get(page);
        // Table entries are live descriptors.
        if d.is_null() || g.desc(d).start() != page {
            not_a_run(self.addr(page));
        }
        if g.desc(d).state() != RunState::Active {
            already_free(self.addr(page));
        }
        let _gi: &Inner = &g;
        proof {
            // A live descriptor's pages lie below the mark.
            assert(g@.descs@.contains_key(d));
            let v = g@.descs@[d].value();
            assert(desc_ok(v, g@.next_page));
            assert(v.start == page && g@.next_page <= g@.np@);
            // An `Active` descriptor's usable pages and guard are its pages.
            assert(run_ok(v, g@.lg@));
            // A token of this run has its extent ([`lemma_token_extent`]).
            if tok is Some && tok.unwrap().wf() && tok.unwrap().start() == page {
                let tracked t = tok.tracked_borrow();
                t.ext.agree(_gi.runs.borrow());
                lemma_token_extent(g@, d, page as int, t.npages());
            }
            let p = self.spec_page();
            assert(v.usable >= p && (v.usable as int) % p == 0) by {
                vstd::arithmetic::div_mod::lemma_mod_multiples_basic(v.pages - GUARD_PAGES, p);
                assert((v.pages - GUARD_PAGES) * p >= 1 * p) by (nonlinear_arith)
                    requires
                        v.pages - GUARD_PAGES >= 1,
                        p > 0,
                ;
            }
            assert(page * p + v.usable + GUARD_PAGES * p <= self.npages * p) by (nonlinear_arith)
                requires
                    v.usable == (v.pages - GUARD_PAGES) * p,
                    page + v.pages <= self.npages,
                    p > 0,
            ;
        }
        g.desc(d).usable
    }

    /// Descriptor of the extent starting or ending at `page`, if any.
    pub fn lookup(&self, page: usize) -> Option<RunInfo>
        requires
            crate::sys::may_syscall(),
    {
        broadcast use axiom_huge_lock_inv;

        let g = self.inner.lock();
        let d = g.table.get(page);
        if d.is_null() {
            return None;
        }
        // Table entries are live descriptors.
        Some(RunInfo {
            start: g.desc(d).start(),
            pages: g.desc(d).pages(),
            usable: g.desc(d).usable,
            arena: g.desc(d).arena,
            state: g.desc(d).state(),
            dirty_at: g.desc(d).dirty_at,
        })
    }

    /// The extent counters, from one lock hold. The free extents' pages
    /// are among the region's, at most `u32::MAX`.
    pub fn stats(&self) -> (r: HugeStats)
        requires
            crate::sys::may_syscall(),
        ensures
            r.empty_pages + r.purged_pages <= u32::MAX,
    {
        broadcast use axiom_huge_lock_inv;

        let g = self.inner.lock();
        proof {
            g@.table.lemma_wf_storage();
        }
        HugeStats { table_cap: g.table.cap(), ..g.stats }
    }

    /// Purge every `Empty` extent whose `dirty_at` is at or before `cutoff`
    /// (`u64::MAX` for all): `MADV_DONTNEED`, then `Purged`. Returns the
    /// number purged, at most the region's pages.
    pub fn purge_empty(&self, cutoff: u64) -> (r: usize)
        requires
            crate::sys::may_syscall(),
        ensures
            r <= u32::MAX,
    {
        broadcast use axiom_huge_lock_inv, axiom_huge_lock_frame;

        let mut g = self.inner.lock();
        proof {
            use_type_invariant(self);
            self.lemma_page();
            g@.lemma_elist();
            let p = self.spec_page();
            assert(epages(g@.elist@, g@.descs@) * p <= self.npages * p) by (nonlinear_arith)
                requires
                    epages(g@.elist@, g@.descs@) <= self.npages,
                    p > 0,
            ;
        }
        let mut n = 0;
        // The batch may discard pages of no live run: the run authority
        // does not change while the lock is held here.
        let ghost held = g@.pgs@@;
        let mut batch = Batch::new(Ghost(|b: int| self.purgeable(held, b)));
        loop
            invariant
                crate::sys::may_syscall(),
                g@.wf(),
                g@.spec_np() == self.npages,
                g@.spec_lg() == self.lg_page,
                g@.pgs@@ == held,
                forall|b: int| #[trigger] (batch.free())(b) == self.purgeable(held, b),
                batch.wf(),
                batch.total() + epages(g@.elist@, g@.descs@) * self.spec_page() <= self.npages * self.spec_page(),
                n as int + g@.elist@.len() <= g@.next_page,
            decreases g@.elist@.len(),
        {
            let d = g.empty_head;
            if d.is_null() {
                break;
            }
            proof {
                g@.lemma_head();
                g@.lemma_elist();
                assert(enode(g@.elist@, g@.descs@, 0));
                lemma_epages_step(g@.elist@, g@.descs@, 0);
                assert(g@.elist@.subrange(0, g@.elist@.len() as int) =~= g@.elist@);
                lemma_epages_nonneg(g@.elist@.subrange(1, g@.elist@.len() as int), g@.descs@);
                use_type_invariant(self);
                self.lemma_page();
                let p = self.spec_page();
                let e = epages(g@.elist@, g@.descs@);
                let pg = g@.descs@[d].value().pages as int;
                assert(pg * p <= e * p) by (nonlinear_arith)
                    requires
                        pg <= e,
                        p > 0,
                ;
            }
            // List members are live `Empty` extents.
            if g.desc(d).dirty_at > cutoff {
                break;
            }
            let ghost l0 = g@.elist@;
            let ghost d0 = g@.descs@;
            let ghost t0 = batch.total();
            proof {
                assert(l0.index_of(d) == 0) by {
                    assert(l0.no_duplicates());
                }
            }
            self.purge_extent(&mut g, d, &mut batch);
            proof {
                let l1 = g@.elist@;
                lemma_epages_remove(l0, d0, 0);
                assert forall|i: int| 0 <= i < l1.len() implies #[trigger] g@.descs@[l1[i]].value().pages
                    == d0[l1[i]].value().pages by {
                    assert(l1[i] == l0[i + 1]);
                    assert(enode(l0, d0, i + 1));
                }
                lemma_epages_frame(l1, d0, g@.descs@);
                let p = self.spec_page();
                let pg = d0[d].value().pages as int;
                let e0 = epages(l0, d0);
                assert(t0 + pg * p + (e0 - pg) * p == t0 + e0 * p) by (nonlinear_arith);
            }
            n += 1;
        }
        batch.flush();
        self.sync_oldest(&g);
        proof {
            assert(g@.wf());
        }
        n
    }

    /// Purge `d`, a member of the `Empty` list: `MADV_DONTNEED` through
    /// `batch`, then `Purged`. Discarded in batches before the lock is
    /// released, so no allocation can take the extent between the state
    /// change and the discard. Returns its pages.
    ///
    /// The explicit rlimit: the proof ran over the default on aarch64
    /// once `perms_ok` placed descriptors in the metadata region.
    #[verifier::rlimit(40)]
    fn purge_extent(&self, g: &mut Inner, d: *mut HugeDesc, batch: &mut Batch) -> (r: usize)
        requires
            crate::sys::may_syscall(),
            old(g).wf(),
            old(g).spec_np() == self.npages,
            old(g).spec_lg() == self.lg_page,
            old(g).elist@.contains(d),
            old(batch).wf(),
            old(batch).total() + old(g).descs@[d].value().pages * self.spec_page() <= self.npages * self.spec_page(),
            forall|b: int| self.purgeable(old(g).pgs@@, b) ==> #[trigger] (old(batch).free())(b),
        ensures
            final(g).wf(),
            final(g).spec_np() == old(g).spec_np(),
            final(g).spec_lg() == old(g).spec_lg(),
            final(g).next_page == old(g).next_page,
            final(g).pgs == old(g).pgs,
            final(batch).wf(),
            final(batch).free() == old(batch).free(),
            r == old(g).descs@[d].value().pages,
            final(batch).total() <= old(batch).total() + r * self.spec_page(),
            final(g).elist@ == old(g).elist@.remove(old(g).elist@.index_of(d)),
            forall|x: *mut HugeDesc| #[trigger] old(g).descs@.contains_key(x) ==> final(g).descs@[x].value().pages
                == old(g).descs@[x].value().pages,
    {
        let ghost j = g.elist@.index_of(d);
        let ghost d0 = g.descs@;
        let ghost l0 = g.elist@;
        proof {
            use_type_invariant(self);
            self.lemma_page();
            g.lemma_elist();
            assert(enode(l0, d0, j));
            let v = d0[d].value();
            let p = self.spec_page();
            self.lemma_pages((v.start + v.pages) as usize);
            self.lemma_pages(v.start as usize);
            assert((v.start + v.pages) * p == v.start * p + v.pages * p) by (nonlinear_arith);
        }
        // List members are live `Empty` extents.
        let (start, pages) = (g.desc(d).start(), g.desc(d).pages());
        // The extent's pages are no live run's: the page authority holds
        // the `Active` extents' pages, and those are apart from `d`.
        proof {
            let v = d0[d].value();
            let p = self.spec_page();
            let base = self.base as int;
            lemma_run_auths(*g);
            assert(desc_ok(v, g.next_page));
            lemma_active(d0);
            assert forall|b: int| base + v.start * p <= b < base + v.start * p + v.pages * p
                implies #[trigger] (batch.free())(b) by {
                crate::purge::lemma_unit_of(base, p, v.start as int, v.pages as int, b);
                let k = (b - base) / p;
                if g.pgs@@.contains(k) {
                    let x = choose|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && run_page(d0[x].value(), k);
                    assert(x != d);
                    assert(apart(v, d0[x].value()));
                }
                assert(self.purgeable(g.pgs@@, b));
            }
        }
        batch.push(self.addr(start), pages * self.page_size());
        g.unlink(d);
        let ghost d1 = g.descs@;
        g.desc_mut(d).state = RunState::Purged as u8;
        g.desc_mut(d).dirty_at = 0;
        g.stats.sub(RunState::Empty, pages);
        g.stats.add(RunState::Purged, pages);
        proof {
            let l1 = g.elist@;
            assert(!l1.contains(d)) by {
                assert(l0.no_duplicates());
                if l1.contains(d) {
                    let i = l1.index_of(d);
                    if i < j {
                        assert(l1[i] == l0[i]);
                    } else {
                        assert(l1[i] == l0[i + 1]);
                    }
                }
            }
            assert forall|i: int| 0 <= i < l1.len() implies #[trigger] enode(l1, g.descs@, i) by {
                assert(enode(l1, d1, i));
                assert(l1[i] != d);
            }
            assert(g.descs@.dom() =~= d0.dom());
            lemma_epages_remove(l0, d0, j);
            assert forall|i: int| 0 <= i < l1.len() implies #[trigger] g.descs@[l1[i]].value().pages
                == d0[l1[i]].value().pages by {
                assert(same_but_links(d0[l1[i]].value(), d1[l1[i]].value())) by {
                    assert(enode(l1, d1, i));
                }
            }
            lemma_epages_frame(l1, d0, g.descs@);
            assert forall|x: *mut HugeDesc| #[trigger] g.descs@.contains_key(x) implies desc_ok(
                g.descs@[x].value(),
                g.next_page,
            ) by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) implies g.descs@[x].value().pages
                == d0[x].value().pages by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            // The extents are where they were; `d` changed state only.
            let d2 = g.descs@;
            // The run authorities are untouched: `d` was `Empty`, and the
            // `Active` extents are as they were.
            assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && d0[x].value().state == RunState::Active as u8 implies {
                &&& d2.contains_key(x)
                &&& d2[x].value().state == RunState::Active as u8
                &&& d2[x].value().start == d0[x].value().start
                &&& d2[x].value().pages == d0[x].value().pages
            } by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            assert forall|x: *mut HugeDesc| #[trigger] d2.contains_key(x) && d2[x].value().state == RunState::Active as u8 implies {
                &&& d0.contains_key(x)
                &&& d0[x].value().state == RunState::Active as u8
            } by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            lemma_active_frame(d0, d2);
            assert forall|x: *mut HugeDesc| #[trigger] d2.contains_key(x) implies keyed(g.table, d2[x].value(), x) by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            assert forall|k: usize| #[trigger] g.table.lookup(k) is Some implies is_key(
                d2[g.table.lookup(k).unwrap()].value(),
                k as int,
            ) by {
                let x = g.table.lookup(k).unwrap();
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            assert forall|x1: *mut HugeDesc, x2: *mut HugeDesc|
                #[trigger] d2.contains_key(x1) && #[trigger] d2.contains_key(x2) && x1 != x2 implies apart(
                    d2[x1].value(),
                    d2[x2].value(),
                ) by {
                assert(same_but_links(d0[x1].value(), d1[x1].value()));
                assert(same_but_links(d0[x2].value(), d1[x2].value()));
            }
            // Free extents stay free (`d` turns from `Empty` to `Purged`),
            // where they were.
            assert forall|x1: *mut HugeDesc, x2: *mut HugeDesc|
                #[trigger] d2.contains_key(x1) && #[trigger] d2.contains_key(x2) && d2[x1].value().state != RunState::Active as u8
                    && d2[x2].value().state != RunState::Active as u8 implies d2[x1].value().start + d2[x1].value().pages
                    != d2[x2].value().start by {
                assert(same_but_links(d0[x1].value(), d1[x1].value()));
                assert(same_but_links(d0[x2].value(), d1[x2].value()));
                assert(d0.contains_key(x1) && d0.contains_key(x2));
            }
            assert forall|x: *mut HugeDesc| #[trigger] d2.contains_key(x) && d2[x].value().state != RunState::Active as u8
                implies marked(g.free.map.view(), d2[x].value()) by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
                assert(d0.contains_key(x));
            }
            assert forall|x: *mut HugeDesc| #[trigger] d2.contains_key(x) && d2[x].value().state == RunState::Empty as u8
                implies l1.contains(x) by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
                assert(l0.contains(x));
                let i = l0.index_of(x);
                assert(i != j);
                if i < j {
                    assert(l1[i] == x);
                } else {
                    assert(l1[i - 1] == x);
                }
            }
            let all = g.all@;
            assert(all.contains(d));
            assert forall|k: int| 0 <= k < 7 implies #[trigger] tally(all, d2, k) == tally(all, d0, k) - weight(d0[d].value(), k)
                + weight(d2[d].value(), k) by {
                assert forall|i: int| 0 <= i < all.len() implies #[trigger] weight(d1[all[i]].value(), k) == weight(
                    d0[all[i]].value(),
                    k,
                ) by {
                    assert(d0.contains_key(all[i]));
                    assert(same_but_links(d0[all[i]].value(), d1[all[i]].value()));
                }
                assert(same_but_links(d0[d].value(), d1[d].value()));
                lemma_tally_frame(all, d0, d1, k);
                lemma_tally_update(all, d1, d2, d, k);
            }
            assert(tally(all, d2, 6) == tally(all, d0, 6));
            assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(g.stats, k) == tally(all, d2, k) by {
                assert(tally(all, d2, k) == tally(all, d0, k) - weight(d0[d].value(), k) + weight(d2[d].value(), k));
                assert(stat(old(g).stats, k) == tally(all, d0, k));
            }
        }
        pages
    }

    /// Decay walk (design section 8), as `ClassState::purge_curve` in
    /// pages: the curve's allowance at `now` is the sum over the `Empty`
    /// extents of their pages times the weight of their age, rounded up,
    /// and the excess over it is purged oldest extent first, whole extents
    /// only and never below the allowance (an extent larger than the
    /// remaining excess is skipped for younger ones that fit), so a lone
    /// extent lasts until its weight is zero, the end of the window, as
    /// before. Returns the number of extents purged.
    fn purge_curve(&self, now: u64, curve: &Curve, keep16: u32) -> usize
        requires
            crate::sys::may_syscall(),
    {
        broadcast use axiom_huge_lock_inv, axiom_huge_lock_frame;

        let mut g = self.inner.lock();
        proof {
            use_type_invariant(self);
            self.lemma_page();
            g@.lemma_elist();
        }
        let ghost l = g@.elist@;
        let ghost len = l.len() as int;
        let ghost mut k: int = 0;
        let mut total = 0usize;
        let mut weighted = 0u64;
        let mut d = g.empty_head;
        proof {
            assert(l.subrange(0, len) =~= l);
            g@.lemma_head();
        }
        while !d.is_null()
            invariant
                g@.wf(),
                l == g@.elist@,
                len == l.len(),
                0 <= k <= len,
                d == (if k < len { l[k] } else { ptr::null_mut::<HugeDesc>() }),
                total + epages(l.subrange(k, len), g@.descs@) == epages(l, g@.descs@),
                epages(l, g@.descs@) <= g@.next_page <= u32::MAX,
                forall|i: int| 0 <= i < len ==> #[trigger] g@.descs@.contains_key(l[i]),
                weighted <= total * 0xFFFF_FFFFu64,
            decreases len - k,
        {
            proof {
                if k >= len {
                    assert(d@.addr == 0);
                }
                assert(enode(l, g@.descs@, k));
                lemma_epages_step(l, g@.descs@, k);
                lemma_epages_nonneg(l.subrange(k + 1, len), g@.descs@);
            }
            // List members are live `Empty` extents.
            let (pages, dirty_at, next) = (g.desc(d).pages(), g.desc(d).dirty_at, g.desc(d).next);
            total += pages;
            proof {
                assert forall|w: u64| w <= 0xFFFF_FFFFu64 implies #[trigger] (pages as u64 * w) <= pages * 0xFFFF_FFFFu64 by {
                    assert(pages as u64 * w <= pages * 0xFFFF_FFFFu64) by (nonlinear_arith)
                        requires
                            w <= 0xFFFF_FFFFu64,
                    ;
                }
                assert(total * 0xFFFF_FFFFu64 <= 0xFFFF_FFFFu64 * 0xFFFF_FFFFu64) by (nonlinear_arith)
                    requires
                        total <= 0xFFFF_FFFFu64,
                ;
                assert(total * 0xFFFF_FFFFu64 == (total - pages) * 0xFFFF_FFFFu64 + pages * 0xFFFF_FFFFu64) by (nonlinear_arith);
            }
            weighted += pages as u64 * Curve::weight(curve.step(now, dirty_at));
            d = next;
            proof {
                k = k + 1;
                if k < len {
                    let kk = k;
                    assert(l.subrange(kk, len) =~= l.subrange(kk, len));
                }
            }
        }
        // The cgroup policy's retention cap (design 11), as in the class
        // walk: keep at most this walk's share of the cap.
        let mut allow = Curve::allowance(weighted);
        proof {
            crate::policy::frac_one_value();
        }
        if keep16 < crate::policy::KEEP_ALL {
            allow = allow.min(crate::policy::mul16(total as u64, keep16) as usize);
        }
        let mut excess = total.saturating_sub(allow);
        // Under the cap an extent larger than the excess goes whole: free
        // neighbours coalesce, so what a program frees in pieces is often
        // one extent, and skipping it would keep the retention over the
        // cap for good (measured: 50 freed 8 MiB runs, one 400 MiB
        // extent, nothing purged in 3 s). The curve alone keeps the rule.
        let capped = keep16 < crate::policy::KEEP_ALL;
        let mut n = 0;
        if excess > 0 {
            // As in `purge_empty`: pages of no live run.
            let ghost held = g@.pgs@@;
            let mut batch = Batch::new(Ghost(|b: int| self.purgeable(held, b)));
            let mut d = g.empty_head;
            let ghost mut k: int = 0;
            proof {
                g@.lemma_head();
                g@.lemma_elist();
                assert(g@.elist@.subrange(0, g@.elist@.len() as int) =~= g@.elist@);
                let p = self.spec_page();
                assert(epages(g@.elist@, g@.descs@) * p <= self.npages * p) by (nonlinear_arith)
                    requires
                        epages(g@.elist@, g@.descs@) <= self.npages,
                        p > 0,
                ;
            }
            while !d.is_null() && excess > 0
                invariant
                    crate::sys::may_syscall(),
                    g@.wf(),
                    g@.spec_np() == self.npages,
                    g@.spec_lg() == self.lg_page,
                    g@.pgs@@ == held,
                    forall|b: int| #[trigger] (batch.free())(b) == self.purgeable(held, b),
                    batch.wf(),
                    0 <= k <= g@.elist@.len(),
                    d == (if k < g@.elist@.len() { g@.elist@[k] } else { ptr::null_mut::<HugeDesc>() }),
                    batch.total() + epages(g@.elist@.subrange(k, g@.elist@.len() as int), g@.descs@) * self.spec_page()
                        <= self.npages * self.spec_page(),
                    n as int + g@.elist@.len() <= g@.next_page,
                decreases g@.elist@.len() - k,
            {
                let ghost l1 = g@.elist@;
                let ghost n1 = l1.len() as int;
                proof {
                    if k >= n1 {
                        assert(d@.addr == 0);
                    }
                    g@.lemma_elist();
                    assert(enode(l1, g@.descs@, k));
                    lemma_epages_step(l1, g@.descs@, k);
                    lemma_epages_nonneg(l1.subrange(k + 1, n1), g@.descs@);
                    use_type_invariant(self);
                    self.lemma_page();
                    let p = self.spec_page();
                    let pg = g@.descs@[d].value().pages as int;
                    let rest = epages(l1.subrange(k + 1, n1), g@.descs@);
                    assert(pg * p + rest * p == (pg + rest) * p) by (nonlinear_arith);
                    assert(rest * p >= 0) by (nonlinear_arith)
                        requires
                            rest >= 0,
                            p > 0,
                    ;
                    assert(l1.index_of(d) == k) by {
                        assert(l1.no_duplicates());
                    }
                }
                // As above; `next` is read before `d` leaves the list.
                let (pages, next) = (g.desc(d).pages(), g.desc(d).next);
                if pages <= excess || capped {
                    let ghost dd = g@.descs@;
                    excess = excess.saturating_sub(self.purge_extent(&mut g, d, &mut batch));
                    n += 1;
                    proof {
                        let l2 = g@.elist@;
                        assert(l2.subrange(k, l2.len() as int) =~= l1.subrange(k + 1, n1));
                        assert forall|i: int| 0 <= i < l2.subrange(k, l2.len() as int).len() implies #[trigger] g@.descs@[l2.subrange(
                            k,
                            l2.len() as int,
                        )[i]].value().pages == dd[l2.subrange(k, l2.len() as int)[i]].value().pages by {
                            g@.lemma_elist();
                            assert(l2.subrange(k, l2.len() as int)[i] == l2[k + i]);
                            assert(l2[k + i] == l1[k + 1 + i]);
                            assert(enode(l1, dd, k + 1 + i));
                        }
                        lemma_epages_frame(l2.subrange(k, l2.len() as int), dd, g@.descs@);
                        if k < l2.len() {
                            assert(enode(l1, dd, k + 1));
                        }
                    }
                } else {
                    proof {
                        k = k + 1;
                    }
                }
                d = next;
            }
            batch.flush();
        }
        self.sync_oldest(&g);
        proof {
            assert(g@.wf());
        }
        n
    }

    /// Decay step (design section 8), as `ClassState::decay`: the gate
    /// runs `purge_curve` over the list at most once per step of the
    /// curve and returns the next deadline, `None` when nothing is
    /// `Empty`. Lock-free unless a walk is due.
    pub fn decay(&self, now: u64, decay_ns: u64) -> Option<u64>
        requires
            crate::sys::may_syscall(),
    {
        self.decay_with(now, decay_ns, crate::policy::KEEP_ALL)
    }

    /// [`decay`](Huge::decay) with the cgroup policy's keep ratio (design
    /// 11), as `ClassState::decay_with`.
    pub fn decay_with(&self, now: u64, decay_ns: u64, keep16: u32) -> Option<u64>
        requires
            crate::sys::may_syscall(),
    {
        self.gate.decay(now, decay_ns, |now, curve| {
            self.purge_curve(now, curve, keep16);
        })
    }

}

} // verus!

verus! {

impl Huge {
    /// Lowest-address first fit for `need` pages at `align`. Returns the
    /// start page and the free extent it comes from (`None` for fresh pages
    /// at the high-water mark). The extent may end below `s + need` only if
    /// it touches the mark.
    ///
    /// Proved lowest (`verus/THEOREMS.md`, "Lowest-address huge
    /// placement"): the start fits ([`Huge::fits`]: aligned, inside the
    /// region, every page free) and no lower page does; `None` only when no
    /// page fits. Each free extent the scan passes over holds no aligned
    /// fit and ends below the mark, so with coalescing no fit can start in
    /// it ([`Inner::lemma_fit_extent`]); the pages between extents are not
    /// free.
    fn find(&self, g: &Inner, need: usize, align: usize) -> (r: Option<(usize, *mut HugeDesc)>)
        requires
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
            let e = g.extent_at(a);
            proof {
                assert(desc_ok(g.descs@[e].value(), g.next_page));
            }
            // `extent_at` checked `e` is live.
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

    /// Record the run `p` taken from its extent (or fresh pages), splitting
    /// the extent around it. Infallible. Returns the zero flag.
    ///
    /// Proved to keep `Inner::wf`: the pieces (the run, the head of the
    /// extent before it, its tail after the run, or the hole alignment
    /// leaves at the mark) cover exactly what they replace, so the extents
    /// stay disjoint and inside `next_page`, each under exactly its keys,
    /// and the counters stay their tallies. `run` and `spare` come with
    /// their permissions ([`alloc_desc`], where the trust that they are
    /// fresh blocks sits), so they are neither live descriptors nor the
    /// same block.
    ///
    /// The proof is large, and its solver cost moves with unrelated items
    /// elsewhere in the crate; the rlimit leaves it headroom.
    #[verifier::rlimit(800)]
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
        Tracked(run_perm): Tracked<PointsTo<HugeDesc>>,
        Tracked(spare_perm): Tracked<PointsTo<HugeDesc>>,
        Tracked(tok): Tracked<&mut Map<int, RunToken>>,
    ) -> (r: (bool, Tracked<Zeroed>))
        requires
            crate::sys::may_syscall(),
            old(g).wf(),
            old(g).spec_np() == self.npages,
            old(g).spec_lg() == self.lg_page,
            run@.addr != 0,
            spare@.addr != 0,
            run_perm.ptr() == run,
            spare_perm.ptr() == spare,
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
            final(g).descs@.contains_key(run),
            final(g).descs@[run].value().pages == p.pages,
            final(tok).contains_key(0),
            final(tok)[0].wf(),
            final(tok)[0].start() == p.start,
            final(tok)[0].npages() == p.pages,
            r.0 ==> r.1@.addr() == self.base + p.start * self.spec_page()
                && r.1@.len() == (p.pages - GUARD_PAGES) * self.spec_page(),
    {
        let Placement { start: s, pages: need, ext } = p;
        proof {
            use_type_invariant(self);
        }
        let end = p.end();
        let old_mark = g.next_page;
        let mut spare_used = false;
        let mut zeroed = true;
        let tracked mut spare_slot = Some(spare_perm);
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
            // The table's keys lie inside their extents, which end at the
            // mark; `ext`'s pages belong to no other extent.
            assert forall|k: usize| #[trigger] t0.lookup(k) is Some implies d0.contains_key(t0.lookup(k).unwrap()) && d0[t0.lookup(
                k,
            ).unwrap()].value().start <= k < d0[t0.lookup(k).unwrap()].value().start + d0[t0.lookup(k).unwrap()].value().pages <= old_mark by {
                let x = t0.lookup(k).unwrap();
                assert(is_key(d0[x].value(), k as int));
                assert(desc_ok(d0[x].value(), old_mark));
            }
            lemma_tally_nonneg(a0, d0, 6);
            if has_ext {
                assert(desc_ok(ve, old_mark));
            }
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
                    g.all@ = g.all@.push(spare);
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
            // `Purged` pages are zero unless a lazy purge discarded them
            // with `MADV_FREE`; pages at or past the mark are untouched.
            zeroed = (st == RunState::Purged && purge::purged_is_zero()) || s >= e_end;
            proof {
                assert(zeroed ==> ve.state == RunState::Purged as u8 || s >= ve.start + ve.pages);
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
                        if l0.contains(spare) {
                            assert(enode(l0, d0, l0.index_of(spare)));
                        }
                    }
                    vs = sp.value();
                    g.descs.borrow_mut().tracked_insert(spare, sp);
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
                        if g.all@ != a0 {
                            assert(g.all@[a0.index_of(ext)] == ext);
                        }
                    }
                    if ve.state == RunState::Empty as u8 {
                        assert(l0.contains(ext));
                        assert(g.elist@.contains(ext)) by {
                            if g.elist@ != l0 {
                                assert(g.elist@ == l0.insert(l0.index_of(ext) + 1, spare));
                                assert(g.elist@[l0.index_of(ext)] == ext);
                            }
                        }
                    }
                }
                g.retire(meta, ext);
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
            g.all@ = g.all@.push(run);
            assert(forall|i: int| 0 <= i < g.elist@.len() ==> #[trigger] enode(g.elist@, g.descs@, i) == enode(g.elist@, dr, i));
        }
        g.add_extent(run);
        let ghost s4 = g.stats;
        g.stats.next_page = g.next_page;
        if !spare_used {
            let tracked sp = spare_slot.tracked_unwrap();
            free_desc(meta, spare, Tracked(sp));
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
            assert(g.all@ =~= t.all_after(a0));
            assert(g.elist@ == t.elist_after(l0));
            assert(g.table.wf());
            assert(perms_ok(g.descs@));
            assert(g.next_page == t.nm && g.np == old(g).np);
            assert(g.free.map.wf() && g.free.map.spec_nbits() == g.np@);
            assert(elist_ok(g.empty_head, g.empty_tail, g.elist@, g.descs@));
            assert(g.took(*old(g), t));
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
            self.lemma_page();
        }
        let tracked z = zero_run(self.base as int, self.spec_page(), s as int, (need - GUARD_PAGES) as int, old_mark as int, has_ext, ve, zeroed);
        (zeroed, Tracked(z))
    }
}

} // verus!

impl Huge {
    /// Pages in the region.
    #[cfg(test)]
    pub fn npages(&self) -> usize {
        self.npages
    }

    /// [`Huge::alloc_run`] as the pointer and the zero flag alone.
    #[cfg(test)]
    pub fn alloc(&self, meta: &Meta, size: usize, align: usize, arena: u16) -> (*mut u8, bool) {
        let r = self
            .alloc_run(meta, size, align, arena, Tracked::assume_new())
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
    /// region, the metadata region or the kernel refuses.
    #[inline]
    pub fn alloc_run(&self, meta: &Meta, size: usize, align: usize, arena: u16, Tracked(tok): Tracked<&mut Map<int, RunToken>>) -> (r: (Placed, Tracked<Zeroed>))
        requires
            crate::sys::may_syscall(),
            crate::meta::is_pow2(align),
            self.spec_base_aligned(),
            meta.is_process(),
        ensures
            r.0.pages <= u32::MAX,
            r.0.ptr@.addr != 0 ==> self.spec_run_at(r.0.ptr@.addr, size, align),
            r.0.ptr@.addr != 0 && r.0.zeroed ==> r.1@.covers(r.0.ptr, self.spec_usable(size)),
            r.0.ptr@.addr != 0 ==> size <= self.spec_usable(size),
            r.0.ptr@.addr != 0 ==> size + self.spec_page() - 1 <= usize::MAX,
            r.0.ptr@.addr != 0 ==> self.run_token(*final(tok), r.0),
            r.0.ptr@.addr != 0 ==> (final(tok)[0].npages() - GUARD_PAGES) * self.spec_page() == self.spec_usable(size),
    {
        assert!(align.is_power_of_two(), "huge: alignment not a power of two");
        self.alloc_aligned(meta, size, align, arena, Tracked(tok))
    }

    /// [`Huge::alloc_run`] for an `align` already known to be a power of
    /// two.
    #[inline(always)]
    fn alloc_aligned(&self, meta: &Meta, size: usize, align: usize, arena: u16, Tracked(tok): Tracked<&mut Map<int, RunToken>>) -> (r: (Placed, Tracked<Zeroed>))
        requires
            crate::sys::may_syscall(),
            crate::meta::is_pow2(align),
            self.spec_base_aligned(),
            meta.is_process(),
        ensures
            r.0.pages <= u32::MAX,
            r.0.ptr@.addr != 0 ==> self.spec_run_at(r.0.ptr@.addr, size, align),
            r.0.ptr@.addr != 0 && r.0.zeroed ==> r.1@.covers(r.0.ptr, self.spec_usable(size)),
            r.0.ptr@.addr != 0 ==> size <= self.spec_usable(size),
            r.0.ptr@.addr != 0 ==> size + self.spec_page() - 1 <= usize::MAX,
            r.0.ptr@.addr != 0 ==> self.run_token(*final(tok), r.0),
            r.0.ptr@.addr != 0 ==> (final(tok)[0].npages() - GUARD_PAGES) * self.spec_page() == self.spec_usable(size),
    {
        broadcast use {axiom_huge_lock_inv, axiom_huge_lock_frame};
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
        let Some(usable_pages) = self.usable_pages(size) else { return (Placed::NONE, Tracked(Zeroed::empty(0))) };
        let Some(need) = usable_pages.checked_add(GUARD_PAGES) else { return (Placed::NONE, Tracked(Zeroed::empty(0))) };
        if need > self.npages || align > (self.npages << self.lg_page) {
            return (Placed::NONE, Tracked(Zeroed::empty(0)));
        }
        let page = self.page_size();
        let mut g = self.inner.lock();
        let Some((s, ext)) = self.find(&g, need, align) else { return (Placed::NONE, Tracked(Zeroed::empty(0))) };
        let end = s + need;

        // Everything fallible happens before any state changes.
        let (run, _run_opt) = alloc_desc(meta);
        if run.is_null() {
            return (Placed::NONE, Tracked(Zeroed::empty(0)));
        }
        let run_perm = Tracked(_run_opt.get().tracked_unwrap());
        let (spare, _spare_opt) = alloc_desc(meta);
        if spare.is_null() {
            free_desc(meta, run, run_perm);
            return (Placed::NONE, Tracked(Zeroed::empty(0)));
        }
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
        let ok = g.table.ensure(meta, page, 3).is_ok()
            && (end <= g.next_page || g.free.ensure(meta, page, end - 1).is_ok())
            && self.prepare(s, usable_pages, g.next_page).is_ok();
        if !ok {
            free_desc(meta, run, run_perm);
            free_desc(meta, spare, spare_perm);
            return (Placed::NONE, Tracked(Zeroed::empty(0)));
        }

        // `check_zero` needs the extent's state before `take` may free it.
        let _gi: &Inner = &g;
        let prior = if purge::check_zero() { Some(self.prior_state(s, ext, Tracked(_gi.descs.borrow()))) } else { None };
        let (zeroed, z) = self.take(&mut g, meta, Placement { start: s, pages: need, ext }, run, spare, arena, run_perm, spare_perm, Tracked(tok));
        // `take` wrote the run's descriptor; the lock is still held.
        let pages = g.desc(run).pages();
        self.sync_oldest(&g);
        if let (true, Some(prior)) = (zeroed, prior) {
            self.check_zero(s, usable_pages, prior, arena);
        }
        proof {
            self.lemma_run_at(s, usable_pages, size, align);
        }
        (Placed { ptr: crate::vspec::addr::addr_to_ptr(self.addr(s)), zeroed, pages }, z)
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
            ext@.addr != 0 ==> {
                &&& descs.contains_key(ext)
                &&& descs[ext].ptr() == ext
                &&& descs[ext].is_init()
                &&& descs[ext].value().state != RunState::Active as u8
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
    /// purge advice. Faults every page in, so debug only.
    ///
    /// Trusted: volatile reads of the run's memory.
    #[cold]
    #[inline(never)]
    #[verifier::external_body]
    fn check_zero(&self, s: usize, usable_pages: usize, prior: &'static str, arena: u16) {
        let page = self.page_size();
        let base = self.addr(s);
        for i in 0..usable_pages {
            for off in [0, page - 8] {
                let a = base + i * page + off;
                // SAFETY: a usable page of the run this call just took;
                // `prepare` committed it.
                let word = unsafe { ptr::read_volatile(a as *const u64) };
                if word != 0 {
                    dirty_zero_run(base, usable_pages, s, prior, arena, a, word);
                }
            }
        }
    }
}

} // verus!

impl Huge {
    /// [`Huge::free_run`] returning only the usable bytes released.
    #[cfg(test)]
    pub fn free(&self, meta: &Meta, page: usize) -> usize {
        self.free_run(meta, page, Tracked::assume_new()).usable
    }

    /// Oldest `Empty` extent's `dirty_at`, read under the lock (the decay
    /// driver reads the copy in [`Huge::oldest_empty`] instead).
    #[cfg(test)]
    pub fn oldest_dirty_at(&self) -> Option<u64> {
        let g = self.inner.lock();
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
        let g = self.inner.lock();
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
            let (start, pages, state) = unsafe { ((*d).start(), (*d).pages(), (*d).state()) };
            assert_eq!(start, p, "extent at {p} starts at {start}");
            assert!(pages > 0);
            keys += 1;
            counts.add(state, pages);
            match state {
                RunState::Active => {
                    assert_eq!(
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
            d = unsafe { (*d).next };
        }
        assert_eq!(n, st.empty);
    }
}

verus! {

/// What [`Huge::free_run`] merges into one free extent: the freed run `d`
/// (`vd`, still `Active`) and its free neighbours `pred`, which ends where
/// the run starts, and `succ`, which starts where it ends, when present.
/// `surv` holds the merged extent afterwards, as `vm`; the others retire.
#[allow(dead_code)]
ghost struct Merge {
    d: *mut HugeDesc,
    vd: HugeDesc,
    pred: *mut HugeDesc,
    has_pred: bool,
    vp: HugeDesc,
    succ: *mut HugeDesc,
    has_succ: bool,
    vs: HugeDesc,
    surv: *mut HugeDesc,
    vm: HugeDesc,
}

impl Merge {
    /// After `i` steps of the coalescing loop, `k` is a key of a neighbour
    /// it has taken out of the table.
    spec fn nb_key(self, i: int, k: int) -> bool {
        (i >= 1 && self.has_pred && is_key(self.vp, k)) || (i >= 2 && self.has_succ && is_key(self.vs, k))
    }

    /// What those neighbours added to tally `j`.
    spec fn nb_w(self, i: int, j: int) -> int {
        (if i >= 1 && self.has_pred { weight(self.vp, j) } else { 0 }) + (if i >= 2 && self.has_succ {
            weight(self.vs, j)
        } else {
            0
        })
    }

    /// One of them is `Empty`.
    spec fn nb_empty(self, i: int) -> bool {
        (i >= 1 && self.has_pred && self.vp.state == RunState::Empty as u8) || (i >= 2 && self.has_succ && self.vs.state
            == RunState::Empty as u8)
    }

    /// The merged extent's first page so far.
    spec fn nb_lo(self, i: int) -> int {
        if i >= 1 && self.has_pred { self.vp.start as int } else { self.vd.start as int }
    }

    /// One past its last page so far.
    spec fn nb_hi(self, i: int) -> int {
        if i >= 2 && self.has_succ { self.vs.start + self.vs.pages } else { self.vd.start + self.vd.pages }
    }

    /// After `i` steps of the retiring loop over `[d, pred, succ]`, `x` is
    /// one it has retired: present and not the survivor.
    spec fn gone(self, i: int, x: *mut HugeDesc) -> bool {
        ||| (i >= 1 && x == self.d && self.d != self.surv)
        ||| (i >= 2 && self.has_pred && x == self.pred && self.pred != self.surv)
        ||| (i >= 3 && self.has_succ && x == self.succ && self.succ != self.surv)
    }

    /// What those added to tally `j`.
    spec fn gone_w(self, i: int, j: int) -> int {
        (if i >= 1 && self.d != self.surv { weight(self.vd, j) } else { 0 }) + (if i >= 2 && self.has_pred && self.pred
            != self.surv {
            weight(self.vp, j)
        } else {
            0
        }) + (if i >= 3 && self.has_succ && self.succ != self.surv { weight(self.vs, j) } else { 0 })
    }

    /// `x` is one of the merged extents.
    spec fn part(self, x: *mut HugeDesc) -> bool {
        x == self.d || (self.has_pred && x == self.pred) || (self.has_succ && x == self.succ)
    }

    /// What the merged extents added to tally `j`.
    spec fn parts(self, j: int) -> int {
        weight(self.vd, j) + self.nb_w(2, j)
    }

    /// The descriptor at `x` afterwards, up to its list links.
    spec fn model(self, d0: Map<*mut HugeDesc, PointsTo<HugeDesc>>, x: *mut HugeDesc) -> Option<HugeDesc> {
        if x == self.surv {
            Some(self.vm)
        } else if self.part(x) {
            None
        } else if d0.contains_key(x) {
            Some(d0[x].value())
        } else {
            None
        }
    }

    /// The table's entry for `k` afterwards.
    spec fn lookup(self, t0: Table, k: usize) -> Option<*mut HugeDesc> {
        if is_key(self.vm, k as int) {
            Some(self.surv)
        } else if is_key(self.vd, k as int) || self.nb_key(2, k as int) {
            None
        } else {
            t0.lookup(k)
        }
    }

    /// The merged extents, against the state `g0` they come from.
    spec fn parts_ok(self, g0: Inner) -> bool {
        &&& g0.descs@.contains_key(self.d)
        &&& self.vd == g0.descs@[self.d].value()
        &&& self.vd.state == RunState::Active as u8
        &&& self.has_pred == (self.pred@.addr != 0)
        &&& self.has_pred ==> g0.descs@.contains_key(self.pred) && self.vp == g0.descs@[self.pred].value() && self.vp.state
            != RunState::Active as u8 && self.vp.start + self.vp.pages == self.vd.start
        &&& self.has_succ == (self.succ@.addr != 0)
        &&& self.has_succ ==> g0.descs@.contains_key(self.succ) && self.vs == g0.descs@[self.succ].value() && self.vs.state
            != RunState::Active as u8 && self.vs.start == self.vd.start + self.vd.pages
    }

    /// The survivor is one of the merged extents and holds all of them,
    /// free.
    spec fn surv_ok(self) -> bool {
        &&& self.part(self.surv)
        &&& self.vm.start == self.nb_lo(2)
        &&& self.vm.pages == self.nb_hi(2) - self.nb_lo(2)
        &&& self.vm.state == RunState::Empty as u8 || self.vm.state == RunState::Purged as u8
    }

    /// The neighbours are every free extent touching the run: without
    /// `pred`, no free extent ends where the run starts; without `succ`,
    /// none starts where it ends.
    spec fn nbrs_ok(self, g0: Inner) -> bool {
        &&& !self.has_pred ==> none_ends_at(g0.descs@, self.vd.start as int)
        &&& !self.has_succ ==> none_starts_at(g0.descs@, self.vd.start + self.vd.pages)
    }
}

impl Inner {
    /// `self` is `g0` with the extents of `m` merged into `m.surv`.
    spec fn freed(&self, g0: Inner, m: Merge) -> bool {
        &&& m.parts_ok(g0)
        &&& m.surv_ok()
        &&& self.table.wf()
        &&& self.table.spec_count() == g0.table.spec_count() - m.parts(6) + weight(m.vm, 6)
        &&& forall|k: usize| #[trigger] self.table.lookup(k) == m.lookup(g0.table, k)
        &&& perms_ok(self.descs@)
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) <==> m.model(g0.descs@, x) is Some
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) ==> same_but_links(
            m.model(g0.descs@, x).unwrap(),
            self.descs@[x].value(),
        )
        &&& self.all@.no_duplicates()
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) <==> self.all@.contains(x)
        &&& forall|j: int| 0 <= j < 7 ==> #[trigger] tally(self.all@, self.descs@, j) == tally(g0.all@, g0.descs@, j) - m.parts(j)
            + weight(m.vm, j)
        &&& forall|j: int| 0 <= j < 6 ==> #[trigger] stat(self.stats, j) == stat(g0.stats, j) - m.parts(j) + weight(m.vm, j)
        &&& self.next_page == g0.next_page
        &&& self.np == g0.np
        &&& self.lg == g0.lg
        &&& self.free.map.wf()
        &&& self.free.map.in_meta()
        &&& self.free.map.spec_nbits() == self.np@
        &&& elist_ok(self.empty_head, self.empty_tail, self.elist@, self.descs@)
        &&& self.stats.empty == self.elist@.len()
        &&& self.stats.empty_pages == epages(self.elist@, self.descs@)
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) && self.descs@[x].value().state == RunState::Empty as u8
            ==> self.elist@.contains(x)
    }
}

/// A key of an extent is one of its pages.
proof fn lemma_key_inside(v: HugeDesc, k: int)
    requires
        v.pages >= 1,
        is_key(v, k),
    ensures
        v.start <= k < v.start + v.pages,
{
}

/// A state's weights: one extent, and its pages, in one state.
proof fn lemma_weights(v: HugeDesc)
    requires
        v.state <= 2,
    ensures
        weight(v, 0) + weight(v, 1) + weight(v, 2) == 1,
        weight(v, 3) + weight(v, 4) + weight(v, 5) == v.pages,
{
}

/// An old extent outside the merge shares no page with the merged one.
proof fn lemma_freed_apart(g0: Inner, m: Merge, y: *mut HugeDesc)
    requires
        g0.wf(),
        m.parts_ok(g0),
        m.surv_ok(),
        g0.descs@.contains_key(y),
        !m.part(y),
    ensures
        apart(g0.descs@[y].value(), m.vm),
        desc_ok(g0.descs@[y].value(), g0.next_page),
{
    let d0 = g0.descs@;
    assert(apart(d0[y].value(), m.vd));
    if m.has_pred {
        assert(apart(d0[y].value(), m.vp));
    }
    if m.has_succ {
        assert(apart(d0[y].value(), m.vs));
    }
    assert(desc_ok(d0[y].value(), g0.next_page));
}

/// The extents after a free: inside the mark, disjoint, each under exactly
/// its keys.
proof fn lemma_freed_extents(g0: Inner, g1: Inner, m: Merge)
    requires
        g0.wf(),
        g1.freed(g0, m),
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
    assert(desc_ok(m.vd, g0.next_page));
    if m.has_pred {
        assert(desc_ok(m.vp, g0.next_page));
    }
    if m.has_succ {
        assert(desc_ok(m.vs, g0.next_page));
    }
    assert(m.model(d0, m.surv) == Some(m.vm));
    assert(d1.contains_key(m.surv));
    // Which descriptors are old.
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && x != m.surv implies d0.contains_key(x) && !m.part(x)
        && same_but_links(d0[x].value(), d1[x].value()) by {
        assert(m.model(d0, x) is Some);
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) implies desc_ok(d1[x].value(), g1.next_page) by {
        if x != m.surv {
            lemma_freed_apart(g0, m, x);
        }
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) implies keyed(g1.table, d1[x].value(), x) by {
        let v = d1[x].value();
        let k1 = v.start as usize;
        let k2 = (v.start + v.pages - 1) as usize;
        assert(g1.table.lookup(k1) == m.lookup(t0, k1));
        assert(g1.table.lookup(k2) == m.lookup(t0, k2));
        if x != m.surv {
            lemma_freed_apart(g0, m, x);
            assert(apart(d0[x].value(), m.vd));
            if m.has_pred {
                assert(apart(d0[x].value(), m.vp));
            }
            if m.has_succ {
                assert(apart(d0[x].value(), m.vs));
            }
            assert(keyed(t0, d0[x].value(), x));
        }
    }
    assert forall|k: usize| #[trigger] g1.table.lookup(k) is Some implies d1.contains_key(g1.table.lookup(k).unwrap()) && is_key(
        d1[g1.table.lookup(k).unwrap()].value(),
        k as int,
    ) by {
        assert(g1.table.lookup(k) == m.lookup(t0, k));
        let y = g1.table.lookup(k).unwrap();
        if !is_key(m.vm, k as int) {
            assert(t0.lookup(k) == Some(y));
            assert(d0.contains_key(y) && is_key(d0[y].value(), k as int));
            assert(!m.part(y));
            assert(y != m.surv);
            assert(m.model(d0, y) == Some(d0[y].value()));
            assert(d1.contains_key(y));
        }
    }
    assert forall|x1: *mut HugeDesc, x2: *mut HugeDesc|
        #[trigger] d1.contains_key(x1) && #[trigger] d1.contains_key(x2) && x1 != x2 implies apart(d1[x1].value(), d1[x2].value()) by {
        if x1 != m.surv && x2 != m.surv {
            assert(apart(d0[x1].value(), d0[x2].value()));
        } else if x1 != m.surv {
            lemma_freed_apart(g0, m, x1);
        } else {
            lemma_freed_apart(g0, m, x2);
        }
    }
}

/// A free makes the run's descriptor free (merged into the survivor, or
/// retired): the run authorities lose its extent and its pages, which no
/// other `Active` descriptor has.
proof fn lemma_freed_active(g0: Inner, g1: Inner, m: Merge)
    requires
        g0.wf(),
        g1.freed(g0, m),
    ensures
        active_runs(g1.descs@) == active_runs(g0.descs@).remove((m.vd.start as int, m.vd.pages as int)),
        active_pages(g1.descs@) == active_pages(g0.descs@).difference(
            vstd::set_lib::set_int_range(m.vd.start as int, m.vd.start + m.vd.pages),
        ),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    lemma_active(d0);
    lemma_active(d1);
    let ed = (m.vd.start as int, m.vd.pages as int);
    let range = vstd::set_lib::set_int_range(m.vd.start as int, m.vd.start + m.vd.pages);
    assert(desc_ok(m.vd, g0.next_page));
    // An `Active` descriptor afterwards is an old one other than the run.
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && d1[x].value().state == RunState::Active as u8 implies {
        &&& d0.contains_key(x)
        &&& x != m.d
        &&& same_but_links(d0[x].value(), d1[x].value())
        &&& apart(d0[x].value(), m.vd)
    } by {
        assert(same_but_links(m.model(d0, x).unwrap(), d1[x].value()));
    }
    // An old `Active` descriptor other than the run stays.
    assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && d0[x].value().state == RunState::Active as u8 && x != m.d implies {
        &&& d1.contains_key(x)
        &&& same_but_links(d0[x].value(), d1[x].value())
        &&& apart(d0[x].value(), m.vd)
    } by {
        assert(!m.part(x));
        assert(x != m.surv);
        assert(m.model(d0, x) == Some(d0[x].value()));
        assert(d1.contains_key(x) <==> m.model(d0, x) is Some);
        assert(d1.contains_key(x));
        assert(same_but_links(m.model(d0, x).unwrap(), d1[x].value()));
    }
    assert forall|e: (int, int)| active_runs(d1).contains(e) <==> active_runs(d0).remove(ed).contains(e) by {
        if active_runs(d1).contains(e) {
            let x = choose|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && run_at(d1[x].value(), e);
            assert(d0.contains_key(x) && run_at(d0[x].value(), e));
        }
        if active_runs(d0).contains(e) && e != ed {
            let x = choose|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && run_at(d0[x].value(), e);
            assert(x != m.d);
            assert(d1.contains_key(x) && run_at(d1[x].value(), e));
        }
    }
    assert forall|k: int| active_pages(d1).contains(k) <==> active_pages(d0).difference(range).contains(k) by {
        if active_pages(d1).contains(k) {
            let x = choose|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && run_page(d1[x].value(), k);
            assert(d0.contains_key(x) && run_page(d0[x].value(), k));
        }
        if active_pages(d0).contains(k) && !range.contains(k) {
            let x = choose|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && run_page(d0[x].value(), k);
            assert(x != m.d);
            assert(d1.contains_key(x) && run_page(d1[x].value(), k));
        }
    }
    assert(active_runs(d1) =~= active_runs(d0).remove(ed));
    assert(active_pages(d1) =~= active_pages(d0).difference(range));
}

/// The free-page set after a free, against `f0` before: what was free
/// still is, and so is every page of the freed run `v`.
spec fn freed_free(f0: vstd::set::Set<nat>, f1: vstd::set::Set<nat>, v: HugeDesc) -> bool {
    forall|q: nat| f0.contains(q) || (v.start <= q < v.start + v.pages) ==> #[trigger] f1.contains(q)
}

/// A free keeps the free extents coalesced and marked free: the merged
/// extent starts at `pred`'s start, which no other free extent touched,
/// or at the run's, which none did when `free_before` found none, and
/// ends likewise; its pages are `pred`'s, `succ`'s (free before) and the
/// run's (added to the set).
proof fn lemma_freed_coalesced(g0: Inner, g1: Inner, m: Merge)
    requires
        g0.wf(),
        g1.freed(g0, m),
        m.nbrs_ok(g0),
        freed_free(g0.free.map.view(), g1.free.map.view(), m.vd),
    ensures
        coalesced(g1.descs@),
        free_marked(g1.descs@, g1.free.map.view()),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let f0 = g0.free.map.view();
    let f1 = g1.free.map.view();
    lemma_parts_distinct(g0, m);
    assert(m.model(d0, m.surv) == Some(m.vm));
    assert(d1.contains_key(m.surv));
    assert(same_but_links(m.vm, d1[m.surv].value()));
    // Which descriptors are old.
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && x != m.surv implies d0.contains_key(x) && !m.part(x)
        && same_but_links(d0[x].value(), d1[x].value()) by {
        assert(m.model(d0, x) is Some);
    }
    if m.has_pred {
        assert(marked(f0, m.vp));
    }
    if m.has_succ {
        assert(marked(f0, m.vs));
    }
    assert forall|x1: *mut HugeDesc, x2: *mut HugeDesc|
        #[trigger] d1.contains_key(x1) && #[trigger] d1.contains_key(x2) && d1[x1].value().state != RunState::Active as u8
            && d1[x2].value().state != RunState::Active as u8 implies d1[x1].value().start + d1[x1].value().pages
            != d1[x2].value().start by {
        if x1 != m.surv && x2 != m.surv {
            assert(d0.contains_key(x1) && d0.contains_key(x2));
        } else if x1 != m.surv {
            if m.has_pred {
                assert(d0.contains_key(x1) && d0.contains_key(m.pred));
            } else {
                assert(d0.contains_key(x1));
            }
        } else if x2 != m.surv {
            if m.has_succ {
                assert(d0.contains_key(m.succ) && d0.contains_key(x2));
            } else {
                assert(d0.contains_key(x2));
            }
        }
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && d1[x].value().state != RunState::Active as u8 implies marked(
        f1,
        d1[x].value(),
    ) by {
        let v = d1[x].value();
        if x != m.surv {
            assert(d0.contains_key(x));
            assert(marked(f0, d0[x].value()));
            assert forall|q: int| v.start <= q < v.start + v.pages implies #[trigger] f1.contains(q as nat) by {
                assert(f0.contains(q as nat));
            }
        } else {
            assert forall|q: int| v.start <= q < v.start + v.pages implies #[trigger] f1.contains(q as nat) by {
                if m.has_pred && q < m.vd.start {
                    assert(f0.contains(q as nat));
                }
                if m.has_succ && q >= m.vd.start + m.vd.pages {
                    assert(f0.contains(q as nat));
                }
            }
        }
    }
}

/// A free keeps `Inner::wf`.
proof fn lemma_freed_wf(g0: Inner, g1: Inner, m: Merge)
    requires
        g0.wf(),
        g1.freed(g0, m),
        g1.runs@.id() == run_loc(),
        g1.pgs@.id() == page_loc(),
        g1.runs@@ == active_runs(g1.descs@),
        g1.pgs@@ == active_pages(g1.descs@),
        m.nbrs_ok(g0),
        freed_free(g0.free.map.view(), g1.free.map.view(), m.vd),
    ensures
        g1.wf(),
{
    lemma_freed_extents(g0, g1, m);
    lemma_freed_coalesced(g0, g1, m);
    assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(g1.stats, k) == tally(g1.all@, g1.descs@, k) by {
        assert(stat(g0.stats, k) == tally(g0.all@, g0.descs@, k));
        assert(tally(g1.all@, g1.descs@, k) == tally(g0.all@, g0.descs@, k) - m.parts(k) + weight(m.vm, k));
    }
    assert(g1.table.spec_count() == tally(g1.all@, g1.descs@, 6)) by {
        assert(tally(g1.all@, g1.descs@, 6) == tally(g0.all@, g0.descs@, 6) - m.parts(6) + weight(m.vm, 6));
    }
    // The pages sum to the mark: the merged extent has the parts' pages.
    assert(desc_ok(m.vd, g0.next_page));
    lemma_weights(m.vd);
    if m.has_pred {
        assert(desc_ok(m.vp, g0.next_page));
        lemma_weights(m.vp);
    }
    if m.has_succ {
        assert(desc_ok(m.vs, g0.next_page));
        lemma_weights(m.vs);
    }
    lemma_weights(m.vm);
    assert(m.parts(3) + m.parts(4) + m.parts(5) == weight(m.vm, 3) + weight(m.vm, 4) + weight(m.vm, 5));
    assert(stat(g1.stats, 3) + stat(g1.stats, 4) + stat(g1.stats, 5) == stat(g0.stats, 3) + stat(g0.stats, 4) + stat(g0.stats, 5));
    // Every extent has a page, so the counts are at most the pages.
    let a1 = g1.all@;
    let d1 = g1.descs@;
    assert forall|i: int| 0 <= i < a1.len() implies (#[trigger] d1[a1[i]].value()).pages >= 1 && d1[a1[i]].value().state <= 2 by {
        assert(a1.contains(a1[i]));
        assert(d1.contains_key(a1[i]));
        assert(desc_ok(d1[a1[i]].value(), g1.next_page));
    }
    lemma_tally_counts(a1, d1, 0);
    lemma_tally_counts(a1, d1, 1);
    lemma_tally_counts(a1, d1, 2);
    assert(stat(g1.stats, 0) + stat(g1.stats, 1) + stat(g1.stats, 2) <= g1.next_page);
}

/// The merged extents are three different descriptors, the neighbours
/// free and on either side of the run.
proof fn lemma_parts_distinct(g0: Inner, m: Merge)
    requires
        g0.wf(),
        m.parts_ok(g0),
    ensures
        m.has_pred ==> m.pred != m.d && m.vp.pages >= 1,
        m.has_succ ==> m.succ != m.d && m.vs.pages >= 1,
        m.has_pred && m.has_succ ==> m.pred != m.succ,
        m.vd.pages >= 1,
{
    assert(desc_ok(m.vd, g0.next_page));
    if m.has_pred {
        assert(desc_ok(m.vp, g0.next_page));
    }
    if m.has_succ {
        assert(desc_ok(m.vs, g0.next_page));
    }
}

/// Step `i` of the retiring loop may retire `x`, the `i`th of
/// `[d, pred, succ]`, when it is present and not the survivor.
proof fn lemma_retire_ok(g0: Inner, g: Inner, m: Merge, i: int, x: *mut HugeDesc)
    requires
        g0.wf(),
        m.parts_ok(g0),
        0 <= i < 3,
        x == seq![m.d, m.pred, m.succ][i],
        x@.addr != 0,
        x != m.surv,
        forall|y: *mut HugeDesc| #[trigger] g.descs@.contains_key(y) <==> g0.descs@.contains_key(y) && !m.gone(i, y),
        forall|y: *mut HugeDesc| #[trigger] g.descs@.contains_key(y) ==> same_but_links(g0.descs@[y].value(), g.descs@[y].value()),
        forall|y: *mut HugeDesc| #[trigger] g.all@.contains(y) <==> g.descs@.contains_key(y),
        forall|y: *mut HugeDesc| #[trigger] g.elist@.contains(y) <==> g0.elist@.contains(y) && !m.gone(i, y),
    ensures
        g.descs@.contains_key(x),
        g.all@.contains(x),
        g.descs@[x].value().state == RunState::Empty as u8 ==> g.elist@.contains(x),
        forall|y: *mut HugeDesc| #[trigger] m.gone(i + 1, y) <==> m.gone(i, y) || y == x,
        forall|j: int| #[trigger] m.gone_w(i + 1, j) == m.gone_w(i, j) + weight(g0.descs@[x].value(), j),
{
    lemma_parts_distinct(g0, m);
    assert(seq![m.d, m.pred, m.succ][0] == m.d);
    assert(seq![m.d, m.pred, m.succ][1] == m.pred);
    assert(seq![m.d, m.pred, m.succ][2] == m.succ);
    assert(i == 1 ==> m.has_pred);
    assert(i == 2 ==> m.has_succ);
    assert(g0.descs@.contains_key(x));
    assert(!m.gone(i, x));
    let v = g0.descs@[x].value();
    assert(g.descs@.contains_key(x));
    assert(same_but_links(v, g.descs@[x].value()));
    if v.state == RunState::Empty as u8 {
        assert(g0.elist@.contains(x));
    }
}

/// Step `i` of the retiring loop retired `x` (`gr` before, `g1` after):
/// its invariant holds for step `i + 1`.
proof fn lemma_retired(g0: Inner, gr: Inner, g1: Inner, m: Merge, i: int, x: *mut HugeDesc)
    requires
        g0.wf(),
        gr.descs@.contains_key(x),
        gr.all@.contains(x),
        gr.descs@[x].value().state == RunState::Empty as u8 ==> gr.elist@.contains(x),
        forall|y: *mut HugeDesc| #[trigger] m.gone(i + 1, y) <==> m.gone(i, y) || y == x,
        forall|j: int| #[trigger] m.gone_w(i + 1, j) == m.gone_w(i, j) + weight(g0.descs@[x].value(), j),
        // The loop invariant at step `i`.
        forall|y: *mut HugeDesc| #[trigger] gr.descs@.contains_key(y) <==> g0.descs@.contains_key(y) && !m.gone(i, y),
        forall|y: *mut HugeDesc| #[trigger] gr.descs@.contains_key(y) ==> same_but_links(g0.descs@[y].value(), gr.descs@[y].value()),
        gr.all@.no_duplicates(),
        forall|y: *mut HugeDesc| #[trigger] gr.all@.contains(y) <==> gr.descs@.contains_key(y),
        forall|j: int| 0 <= j < 7 ==> #[trigger] tally(gr.all@, g0.descs@, j) == tally(g0.all@, g0.descs@, j) - m.gone_w(i, j),
        gr.elist@.no_duplicates(),
        forall|y: *mut HugeDesc| #[trigger] gr.elist@.contains(y) <==> g0.elist@.contains(y) && !m.gone(i, y),
        gr.elist@.len() == g0.elist@.len() - m.gone_w(i, 1),
        epages(gr.elist@, g0.descs@) == epages(g0.elist@, g0.descs@) - m.gone_w(i, 4),
        // What `retire` did.
        g1.descs@.dom() == gr.descs@.dom().remove(x),
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) ==> same_but_links(gr.descs@[y].value(), g1.descs@[y].value()),
        g1.elist@ == if gr.descs@[x].value().state == RunState::Empty as u8 {
            gr.elist@.remove(gr.elist@.index_of(x))
        } else {
            gr.elist@
        },
        g1.all@ == gr.all@.remove(gr.all@.index_of(x)),
    ensures
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) <==> g0.descs@.contains_key(y) && !m.gone(i + 1, y),
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) ==> same_but_links(g0.descs@[y].value(), g1.descs@[y].value()),
        g1.all@.no_duplicates(),
        forall|y: *mut HugeDesc| #[trigger] g1.all@.contains(y) <==> g1.descs@.contains_key(y),
        forall|j: int| 0 <= j < 7 ==> #[trigger] tally(g1.all@, g0.descs@, j) == tally(g0.all@, g0.descs@, j) - m.gone_w(i + 1, j),
        forall|y: *mut HugeDesc| #[trigger] g1.elist@.contains(y) <==> g0.elist@.contains(y) && !m.gone(i + 1, y),
        g1.elist@.len() == g0.elist@.len() - m.gone_w(i + 1, 1),
        epages(g1.elist@, g0.descs@) == epages(g0.elist@, g0.descs@) - m.gone_w(i + 1, 4),
{
    let d0 = g0.descs@;
    let v = d0[x].value();
    assert(same_but_links(v, gr.descs@[x].value()));
    assert forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) <==> d0.contains_key(y) && !m.gone(i + 1, y) by {
        assert(g1.descs@.contains_key(y) <==> g1.descs@.dom().contains(y));
    }
    lemma_drop(gr.all@, x);
    let ja = gr.all@.index_of(x);
    assert forall|j: int| 0 <= j < 7 implies #[trigger] tally(g1.all@, d0, j) == tally(g0.all@, d0, j) - m.gone_w(i + 1, j) by {
        lemma_tally_remove(gr.all@, d0, ja, j);
    }
    if v.state == RunState::Empty as u8 {
        lemma_drop(gr.elist@, x);
        lemma_epages_remove(gr.elist@, d0, gr.elist@.index_of(x));
    } else {
        assert(!g0.elist@.contains(x)) by {
            if g0.elist@.contains(x) {
                assert(enode(g0.elist@, d0, g0.elist@.index_of(x)));
            }
        }
    }
    assert(m.gone_w(i + 1, 1) == m.gone_w(i, 1) + weight(v, 1));
    assert(m.gone_w(i + 1, 4) == m.gone_w(i, 4) + weight(v, 4));
}

/// No extent outside the merge has a key inside the merged extent.
proof fn lemma_merged_room(g0: Inner, m: Merge, key: usize)
    requires
        g0.wf(),
        m.parts_ok(g0),
        m.nb_lo(2) <= key < m.nb_hi(2),
    ensures
        g0.table.lookup(key) is Some ==> is_key(m.vd, key as int) || m.nb_key(2, key as int),
{
    lemma_parts_distinct(g0, m);
    if g0.table.lookup(key) is Some {
        let y = g0.table.lookup(key).unwrap();
        let v = g0.descs@[y].value();
        assert(g0.descs@.contains_key(y) && is_key(v, key as int));
        assert(desc_ok(v, g0.next_page));
        if y != m.d {
            assert(apart(v, m.vd));
        }
        if m.has_pred && y != m.pred {
            assert(apart(v, m.vp));
        }
        if m.has_succ && y != m.succ {
            assert(apart(v, m.vs));
        }
    }
}

/// The end of a free, from three states: `gr` after the retiring loop,
/// whose table and counters are those before the free without the merged
/// extents; `gl` after the survivor's rewrite as `vm` (and, when it is
/// the run's descriptor turned `Empty`, its append to the `Empty` list);
/// `g1` after its entry into the table, the counters and the free-page
/// set. `g1` is `g0` with the extents of `m` merged.
proof fn lemma_free_end(g0: Inner, gr: Inner, gl: Inner, g1: Inner, m: Merge)
    requires
        g0.wf(),
        m.parts_ok(g0),
        m.surv_ok(),
        m.nbrs_ok(g0),
        freed_free(g0.free.map.view(), g1.free.map.view(), m.vd),
        m.surv != m.d ==> g0.descs@[m.surv].value().state == RunState::Empty as u8 && m.vm.state == RunState::Empty as u8,
        // `gr`: the merged extents are out of the table and the counters,
        // and the ones other than the survivor are retired.
        gr.table.wf(),
        forall|k: usize| #[trigger] gr.table.lookup(k) == if is_key(m.vd, k as int) || m.nb_key(2, k as int) {
            None
        } else {
            g0.table.lookup(k)
        },
        gr.table.spec_count() == g0.table.spec_count() - m.parts(6),
        forall|j: int| 0 <= j < 6 ==> #[trigger] stat(gr.stats, j) == stat(g0.stats, j) - m.parts(j),
        forall|x: *mut HugeDesc| #[trigger] gr.descs@.contains_key(x) <==> g0.descs@.contains_key(x) && !m.gone(3, x),
        forall|x: *mut HugeDesc| #[trigger] gr.descs@.contains_key(x) ==> same_but_links(g0.descs@[x].value(), gr.descs@[x].value()),
        gr.all@.no_duplicates(),
        forall|x: *mut HugeDesc| #[trigger] gr.all@.contains(x) <==> gr.descs@.contains_key(x),
        forall|j: int| 0 <= j < 7 ==> #[trigger] tally(gr.all@, g0.descs@, j) == tally(g0.all@, g0.descs@, j) - m.gone_w(3, j),
        gr.elist@.no_duplicates(),
        forall|x: *mut HugeDesc| #[trigger] gr.elist@.contains(x) <==> g0.elist@.contains(x) && !m.gone(3, x),
        gr.elist@.len() == g0.elist@.len() - m.gone_w(3, 1),
        epages(gr.elist@, g0.descs@) == epages(g0.elist@, g0.descs@) - m.gone_w(3, 4),
        gr.next_page == g0.next_page,
        gr.np == g0.np,
        gr.lg == g0.lg,
        // `gl`: the survivor rewritten.
        gl.table == gr.table,
        gl.stats == gr.stats,
        gl.all == gr.all,
        gl.next_page == gr.next_page,
        gl.np == gr.np,
        gl.lg == gr.lg,
        perms_ok(gl.descs@),
        gl.descs@.dom() == gr.descs@.dom(),
        forall|x: *mut HugeDesc| #[trigger] gl.descs@.contains_key(x) && x != m.surv ==> same_but_links(
            gr.descs@[x].value(),
            gl.descs@[x].value(),
        ),
        gl.descs@[m.surv].value() == m.vm,
        gl.elist@ == if m.surv == m.d && m.vm.state == RunState::Empty as u8 {
            gr.elist@.push(m.d)
        } else {
            gr.elist@
        },
        elist_ok(gl.empty_head, gl.empty_tail, gl.elist@, gl.descs@),
        // `g1`: the survivor entered.
        g1 == (Inner { table: g1.table, stats: g1.stats, free: g1.free, ..gl }),
        g1.table.wf(),
        g1.table.spec_count() == gl.table.spec_count() + weight(m.vm, 6),
        forall|k: usize| #[trigger] g1.table.lookup(k) == if is_key(m.vm, k as int) {
            Some(m.surv)
        } else {
            gl.table.lookup(k)
        },
        forall|j: int| 0 <= j < 6 ==> #[trigger] stat(g1.stats, j) == stat(gl.stats, j) + weight(m.vm, j),
        g1.free.map.wf(),
        g1.free.map.in_meta(),
        g1.free.map.spec_nbits() == g1.np@,
        // The run authorities without the run's token.
        g1.runs@.id() == run_loc(),
        g1.pgs@.id() == page_loc(),
        g1.runs@@ == active_runs(g0.descs@).remove((m.vd.start as int, m.vd.pages as int)),
        g1.pgs@@ == active_pages(g0.descs@).difference(vstd::set_lib::set_int_range(m.vd.start as int, m.vd.start + m.vd.pages)),
    ensures
        g1.wf(),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let l0 = g0.elist@;
    let lr = gr.elist@;
    let ar = gr.all@;
    let s = m.surv;
    let v0 = d0[s].value();
    lemma_parts_distinct(g0, m);
    assert(d0.contains_key(s) && !m.gone(3, s));
    assert(gr.descs@.contains_key(s));
    // The survivor's old weight is what the retired ones leave out.
    assert forall|j: int| #[trigger] m.gone_w(3, j) == m.parts(j) - weight(v0, j) by {}
    // The descriptors afterwards.
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) <==> m.model(d0, x) is Some by {
        assert(d1.contains_key(x) <==> gr.descs@.dom().contains(x));
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) implies same_but_links(m.model(d0, x).unwrap(), d1[x].value()) by {
        if x != s {
            assert(same_but_links(d0[x].value(), gr.descs@[x].value()));
        }
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) <==> g1.all@.contains(x) by {
        assert(d1.contains_key(x) <==> gr.descs@.dom().contains(x));
    }
    // The tallies: the survivor's weight changed, the others' did not.
    assert forall|j: int| 0 <= j < 7 implies #[trigger] tally(g1.all@, d1, j) == tally(g0.all@, d0, j) - m.parts(j) + weight(
        m.vm,
        j,
    ) by {
        assert forall|i: int| 0 <= i < ar.len() && ar[i] != s implies #[trigger] weight(d1[ar[i]].value(), j) == weight(
            d0[ar[i]].value(),
            j,
        ) by {
            assert(ar.contains(ar[i]));
            assert(same_but_links(d0[ar[i]].value(), gr.descs@[ar[i]].value()));
        }
        lemma_tally_update(ar, d0, d1, s, j);
    }
    // The table.
    assert forall|k: usize| #[trigger] g1.table.lookup(k) == m.lookup(g0.table, k) by {}
    // The `Empty` list: its length and pages are the counters, and it has
    // every `Empty` extent.
    assert(stat(g1.stats, 1) == stat(g0.stats, 1) - m.parts(1) + weight(m.vm, 1));
    assert(stat(g1.stats, 4) == stat(g0.stats, 4) - m.parts(4) + weight(m.vm, 4));
    assert forall|i: int| 0 <= i < lr.len() && lr[i] != s implies #[trigger] d1[lr[i]].value().pages == d0[lr[i]].value().pages by {
        assert(lr.contains(lr[i]));
        assert(l0.contains(lr[i]));
        assert(enode(l0, d0, l0.index_of(lr[i])));
        assert(gr.descs@.contains_key(lr[i]));
        assert(d1.contains_key(lr[i]));
        assert(same_but_links(d0[lr[i]].value(), gr.descs@[lr[i]].value()));
        assert(same_but_links(gr.descs@[lr[i]].value(), d1[lr[i]].value()));
    }
    if s == m.d {
        assert(!lr.contains(s)) by {
            if l0.contains(s) {
                assert(enode(l0, d0, l0.index_of(s)));
            }
        }
        lemma_epages_frame(lr, d0, d1);
        if m.vm.state == RunState::Empty as u8 {
            lemma_epages_insert(lr, d1, lr.len() as int, s);
            assert(lr.insert(lr.len() as int, s) =~= lr.push(s));
        }
    } else {
        assert(l0.contains(s));
        assert(lr.contains(s));
        lemma_epages_update(lr, d0, d1, s);
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && d1[x].value().state == RunState::Empty as u8 implies g1.elist@.contains(
        x,
    ) by {
        if x != s {
            assert(same_but_links(d0[x].value(), gr.descs@[x].value()));
            assert(l0.contains(x));
            assert(lr.contains(x));
        } else if s == m.d {
            assert(g1.elist@ == lr.push(s));
            assert(g1.elist@[lr.len() as int] == s);
        } else {
            assert(l0.contains(s));
            assert(lr.contains(s));
        }
        if g1.elist@ != lr && lr.contains(x) {
            assert(g1.elist@[lr.index_of(x)] == x);
        }
    }
    assert(g1.freed(g0, m));
    lemma_freed_active(g0, g1, m);
    lemma_freed_wf(g0, g1, m);
}

/// The run authorities of a well-formed state (`wf`, for proofs that hide
/// it).
proof fn lemma_run_auths(g0: Inner)
    requires
        g0.wf(),
    ensures
        g0.runs@.id() == run_loc(),
        g0.pgs@.id() == page_loc(),
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

/// Exact huge stats (`verus/THEOREMS.md`, "Exact stats"): in every value
/// the huge lock admits, `stats.runs` is the number of live runs (the
/// extents the run authority holds, one per live [`RunToken`]) and
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

/// A live run is counted: its token's extent and pages are among those
/// [`theorem_huge_stats`] counts.
proof fn theorem_run_counted(tracked g: &Inner, tracked t: &RunToken)
    requires
        g.wf(),
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

/// With the run `d` out of the table `t`, every free extent is still
/// under its keys there, and has a page or more.
proof fn lemma_free_keyed(g0: Inner, t: Table, d: *mut HugeDesc)
    requires
        g0.wf(),
        g0.descs@.contains_key(d),
        g0.descs@[d].value().state == RunState::Active as u8,
        forall|k: usize| #[trigger] t.lookup(k) == if is_key(g0.descs@[d].value(), k as int) {
            None
        } else {
            g0.table.lookup(k)
        },
    ensures
        forall|x: *mut HugeDesc| #[trigger] g0.descs@.contains_key(x) && g0.descs@[x].value().state != RunState::Active as u8 ==> keyed(
            t,
            g0.descs@[x].value(),
            x,
        ) && g0.descs@[x].value().pages >= 1,
{
    let d0 = g0.descs@;
    let vd = d0[d].value();
    assert(desc_ok(vd, g0.next_page));
    assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && d0[x].value().state != RunState::Active as u8 implies keyed(
        t,
        d0[x].value(),
        x,
    ) && d0[x].value().pages >= 1 by {
        let v = d0[x].value();
        assert(desc_ok(v, g0.next_page));
        assert(apart(v, vd));
        assert(keyed(g0.table, v, x));
        let k2 = (v.start + v.pages - 1) as usize;
        assert(t.lookup(v.start as usize) == g0.table.lookup(v.start as usize));
        assert(t.lookup(k2) == g0.table.lookup(k2));
    }
}

/// What [`Huge::free_run`] reads from `wf` before it changes anything.
proof fn lemma_free_start(g0: Inner, page: usize)
    requires
        g0.wf(),
    ensures
        g0.lookups_live(),
        g0.next_page <= g0.np@ <= u32::MAX,
        g0.table.lookup(page) matches Some(d) ==> is_key(g0.descs@[d].value(), page as int),
        forall|x: *mut HugeDesc| #[trigger] g0.descs@.contains_key(x) ==> desc_ok(g0.descs@[x].value(), g0.next_page) && keyed(
            g0.table,
            g0.descs@[x].value(),
            x,
        ),
        g0.table.spec_count() <= 2 * g0.next_page,
        g0.stats.run_pages + g0.stats.empty_pages + g0.stats.purged_pages == g0.next_page,
        g0.stats.runs + g0.stats.empty + g0.stats.purged <= g0.next_page,
        elist_ok(g0.empty_head, g0.empty_tail, g0.elist@, g0.descs@),
        g0.all@.no_duplicates(),
        forall|x: *mut HugeDesc| #[trigger] g0.all@.contains(x) <==> g0.descs@.contains_key(x),
        g0.free.map.wf(),
        g0.free.map.spec_nbits() == g0.np@,
{
    let d0 = g0.descs@;
    assert forall|i: int| 0 <= i < g0.all@.len() implies (#[trigger] d0[g0.all@[i]].value()).pages >= 1 && d0[g0.all@[i]].value().state
        <= 2 by {
        assert(g0.all@.contains(g0.all@[i]));
        assert(d0.contains_key(g0.all@[i]));
        assert(desc_ok(d0[g0.all@[i]].value(), g0.next_page));
    }
    lemma_tally_counts(g0.all@, d0, 0);
    assert(stat(g0.stats, 0) == tally(g0.all@, d0, 0));
    assert(stat(g0.stats, 1) == tally(g0.all@, d0, 1));
    assert(stat(g0.stats, 2) == tally(g0.all@, d0, 2));
}

/// Removing `y` from a list without duplicates removes exactly `y`.
proof fn lemma_drop(l: Seq<*mut HugeDesc>, y: *mut HugeDesc)
    requires
        l.no_duplicates(),
        l.contains(y),
    ensures
        l.remove(l.index_of(y)).no_duplicates(),
        l.remove(l.index_of(y)).len() == l.len() - 1,
        forall|x: *mut HugeDesc| #[trigger] l.remove(l.index_of(y)).contains(x) <==> l.contains(x) && x != y,
{
    let j = l.index_of(y);
    let r = l.remove(j);
    assert forall|x: *mut HugeDesc| #[trigger] r.contains(x) <==> l.contains(x) && x != y by {
        if r.contains(x) {
            let i = r.index_of(x);
            let i0 = if i < j { i } else { i + 1 };
            assert(l[i0] == x);
            assert(x != y) by {
                assert(l.no_duplicates());
            }
        }
        if l.contains(x) && x != y {
            let i0 = l.index_of(x);
            assert(i0 != j);
            let i = if i0 < j { i0 } else { i0 - 1 };
            assert(r[i] == x);
        }
    }
    assert forall|i1: int, i2: int| 0 <= i1 < r.len() && 0 <= i2 < r.len() && i1 != i2 implies r[i1] != r[i2] by {
        let j1 = if i1 < j { i1 } else { i1 + 1 };
        let j2 = if i2 < j { i2 } else { i2 + 1 };
        assert(r[i1] == l[j1] && r[i2] == l[j2]);
    }
}

impl Huge {
    /// Free the run starting at `page` (from `Regions::classify`). Aborts
    /// unless it is the start of a live run. Returns the run as it was
    /// (`start`, `pages` with the guard, `usable`, `arena`), for the
    /// caller's per-arena counters.
    ///
    /// Proved to keep `Inner::wf`: the run and its free neighbours leave
    /// the table and the counters, one of them (the survivor) comes back
    /// as the merged free extent, which covers exactly their pages, and
    /// the others retire; the `Empty` list loses the retired `Empty`
    /// neighbours and gains the run's descriptor when it survives as an
    /// `Empty` extent.
    ///
    /// The body needs about a quarter of its rlimit; `wf` is hidden here
    /// (the lemmas read it), and the rlimit leaves headroom for the solver
    /// cost moving with unrelated items elsewhere in the crate.
    #[verifier::rlimit(40)]
    #[verifier::loop_isolation(false)]
    pub fn free_run(&self, meta: &Meta, page: usize, Tracked(tok): Tracked<RunToken>) -> (r: RunInfo)
        requires
            crate::sys::may_syscall(),
            meta.is_process(),
            tok.wf(),
            tok.start() == page,
        ensures
            r.pages <= u32::MAX,
            r.pages == tok.npages(),
    {
        hide(Inner::wf);
        broadcast use {axiom_huge_lock_inv, axiom_huge_lock_frame, crate::vspec::fmt::lemma_errno_fmt_req};

        proof {
            use_type_invariant(self);
            self.lemma_page();
        }
        let page_size = self.page_size();
        let mut g = self.inner.lock();
        let ghost g0 = g@;
        let ghost d0 = g0.descs@;
        let ghost t0 = g0.table;
        proof {
            lemma_free_start(g0, page);
        }
        let d = g.table.get(page);
        if d.is_null() {
            #[cfg(verus_only)]
            not_a_run_page(page);
            #[cfg(not(verus_only))]
            not_a_run(self.addr(page));
        }
        let ghost vd = d0[d].value();
        proof {
            assert(d0.contains_key(d));
            assert(is_key(vd, page as int));
            assert(desc_ok(vd, g0.next_page));
            lemma_key_inside(vd, page as int);
        }
        // Table entries are live descriptors.
        let (start, n, state, usable, arena) =
            (g.desc(d).start(), g.desc(d).pages(), g.desc(d).state(), g.desc(d).usable, g.desc(d).arena());
        if state != RunState::Active {
            already_free(self.addr(page));
        }
        if start != page {
            not_a_run(self.addr(page));
        }
        // The run's token: its extent is this run's (one `Active` extent
        // starts at `page`), and it leaves the run authorities.
        let ghost tpages = tok.pages@;
        let _gm: &mut Inner = &mut g;
        proof {
            lemma_run_auths(g0);
            tok.ext.agree(_gm.runs.borrow());
            lemma_token_extent(g0, d, tok.start(), tok.npages());
            assert(n == tok.npages());
            let tracked RunToken { ext, pages } = tok;
            _gm.runs.borrow_mut().delete_singleton(ext);
            _gm.pgs.borrow_mut().delete(pages);
        }
        let addr = self.addr(page);
        proof {
            let p = pow2(self.lg_page as nat);
            self.lemma_pages((page + n) as usize);
            assert((page + n) * p == page * p + (n - 1) * p + p) by (nonlinear_arith)
                requires
                    n >= 1,
            ;
            assert(n * p == (n - 1) * p + p) by (nonlinear_arith);
        }
        if self.guard_install {
            if let Err(e) = guard_remove(addr + (n - GUARD_PAGES) * page_size, GUARD_PAGES * page_size) {
                sys::abort_fmt(format_args!("huge: madvise(MADV_GUARD_REMOVE) of {addr:#x} failed: {e}"));
            }
        }
        let now = now_ns().max(1);
        // Purged at once: above `huge_retain_max`, or under a zero huge
        // decay period (`huge_dirty_decay_ms:0`).
        let st = if usable > self.retain_max() || self.purge_on_free() {
            // The run's pages left the run authority with its token above:
            // they are no live run's.
            let ghost held = g@.pgs@@;
            let mut batch = Batch::new(Ghost(|b: int| self.purgeable(held, b)));
            proof {
                let p = self.spec_page();
                let base = self.base as int;
                assert(page + n <= g0.next_page <= g0.np@ == self.npages);
                assert forall|b: int| addr <= b < addr + n * page_size implies #[trigger] (batch.free())(b) by {
                    crate::purge::lemma_unit_of(base, p, page as int, n as int, b);
                    assert(tpages.contains((b - base) / p));
                }
            }
            batch.push(addr, n * page_size);
            batch.flush();
            RunState::Purged
        } else {
            RunState::Empty
        };
        // Best effort: a fuller table still works, only slower.
        let _ = g.table.ensure(meta, page_size, 2);
        g.drop_extent(d, Ghost(Seq::empty()));
        let ghost gb = g@;

        // Coalesce with free neighbours into one extent `[lo, hi)`. The
        // survivor is the `Empty` participant with the oldest `dirty_at`
        // (it keeps its place in the FIFO list), else `d`; the other
        // descriptors go back to `meta`.
        let pred = g.free_before(page);
        let succ = g.free_after(page + n);
        let ghost m = Merge {
            d,
            vd,
            pred,
            has_pred: pred@.addr != 0,
            vp: d0[pred].value(),
            succ,
            has_succ: succ@.addr != 0,
            vs: d0[succ].value(),
            surv: d,
            vm: vd,
        };
        proof {
            assert(m.parts_ok(g0));
            // The neighbours are every free extent touching the run: each
            // is still under its keys once the run left the table.
            lemma_free_keyed(g0, gb.table, d);
            assert(m.nbrs_ok(g0));
            if m.has_pred {
                assert(desc_ok(m.vp, g0.next_page));
                assert(keyed(t0, m.vp, pred));
            }
            if m.has_succ {
                assert(desc_ok(m.vs, g0.next_page));
                assert(keyed(t0, m.vs, succ));
            }
        }
        let mut lo = page;
        let mut hi = page + n;
        let mut merged = st;
        let mut survivor = d;
        // `for nb in [pred, succ]`, spelled out: Verus rejects `continue`
        // in a `for` loop, and the spelled-out loop keeps the machine code
        // of the `for`.
        let mut iter = [pred, succ].into_iter();
        let ghost mut k: int = 0;
        loop
            invariant
                0 <= k <= 2,
                array_iter_rest(iter) == seq![pred, succ].skip(k),
                g@ == (Inner { table: g@.table, stats: g@.stats, ..gb }),
                g@.table.wf(),
                forall|key: usize| #[trigger] g@.table.lookup(key) == if m.nb_key(k, key as int) {
                    None
                } else {
                    gb.table.lookup(key)
                },
                g@.table.spec_count() == gb.table.spec_count() - m.nb_w(k, 6),
                forall|j: int| 0 <= j < 6 ==> #[trigger] stat(g@.stats, j) == stat(gb.stats, j) - m.nb_w(k, j),
                lo == m.nb_lo(k),
                hi == m.nb_hi(k),
                merged == (if m.nb_empty(k) { RunState::Empty } else { st }),
                survivor == d <==> !m.nb_empty(k),
                survivor != d ==> (k >= 1 && m.has_pred && survivor == pred && m.vp.state == RunState::Empty as u8) || (k
                    >= 2 && m.has_succ && survivor == succ && m.vs.state == RunState::Empty as u8),
                k >= 1 && m.has_pred ==> pred@.addr != d@.addr,
                k >= 2 && m.has_succ ==> succ@.addr != d@.addr && (m.has_pred ==> succ@.addr != pred@.addr),
            decreases 2 - k,
        {
            let nb = match iter.next() {
                Some(nb) => nb,
                None => {
                    proof {
                        assert(seq![pred, succ].skip(k).len() == 2 - k);
                    }
                    break;
                },
            };
            proof {
                assert(seq![pred, succ].skip(k)[0] == seq![pred, succ][k]);
                k = k + 1;
            }
            if nb.is_null() {
                continue;
            }
            proof {
                assert(k == 1 ==> nb == pred);
                assert(k == 2 ==> nb == succ);
                let v = d0[nb].value();
                assert(g@.table.lookup(v.start as usize) == Some(nb));
                assert(v.pages > 1 ==> g@.table.lookup((v.start + v.pages - 1) as usize) == Some(nb));
            }
            // Neighbours are live free extents.
            let (ns, ne, nst, ndirty) = (g.desc(nb).start(), g.desc(nb).end(), g.desc(nb).state(), g.desc(nb).dirty_at);
            let ghost others = seq![d, pred];
            g.drop_extent(nb, Ghost(others));
            proof {
                assert(others[0] == d);
                assert(others[1] == pred);
            }
            lo = lo.min(ns);
            hi = hi.max(ne);
            if nst == RunState::Empty {
                merged = RunState::Empty;
                // A survivor other than `d` is a live `Empty` neighbour.
                if survivor == d || ndirty < g.desc(survivor).dirty_at {
                    survivor = nb;
                }
            }
        }
        let ghost ge = g@;
        let ghost m2 = Merge { surv: survivor, ..m };
        // `for x in [d, pred, succ]`, spelled out as above.
        let mut iter = [d, pred, succ].into_iter();
        let ghost mut k2: int = 0;
        loop
            invariant
                0 <= k2 <= 3,
                array_iter_rest(iter) == seq![d, pred, succ].skip(k2),
                perms_ok(g@.descs@),
                elist_ok(g@.empty_head, g@.empty_tail, g@.elist@, g@.descs@),
                forall|x: *mut HugeDesc| #[trigger] g@.descs@.contains_key(x) <==> d0.contains_key(x) && !m2.gone(k2, x),
                forall|x: *mut HugeDesc| #[trigger] g@.descs@.contains_key(x) ==> same_but_links(d0[x].value(), g@.descs@[x].value()),
                g@.all@.no_duplicates(),
                forall|x: *mut HugeDesc| #[trigger] g@.all@.contains(x) <==> g@.descs@.contains_key(x),
                forall|j: int| 0 <= j < 7 ==> #[trigger] tally(g@.all@, d0, j) == tally(g0.all@, d0, j) - m2.gone_w(k2, j),
                forall|x: *mut HugeDesc| #[trigger] g@.elist@.contains(x) <==> g0.elist@.contains(x) && !m2.gone(k2, x),
                g@.elist@.len() == g0.elist@.len() - m2.gone_w(k2, 1),
                epages(g@.elist@, d0) == epages(g0.elist@, d0) - m2.gone_w(k2, 4),
                g@.free == ge.free,
                g@.table == ge.table,
                g@.next_page == ge.next_page,
                g@.stats == ge.stats,
                g@.np == ge.np,
                g@.lg == ge.lg,
                g@.runs == ge.runs,
                g@.pgs == ge.pgs,
            decreases 3 - k2,
        {
            let x = match iter.next() {
                Some(x) => x,
                None => {
                    proof {
                        assert(seq![d, pred, succ].skip(k2).len() == 3 - k2);
                    }
                    break;
                },
            };
            proof {
                assert(seq![d, pred, succ].skip(k2)[0] == seq![d, pred, succ][k2]);
            }
            if !x.is_null() && x != survivor {
                proof {
                    lemma_retire_ok(g0, g@, m2, k2, x);
                }
                let ghost gr = g@;
                g.retire(meta, x);
                proof {
                    lemma_retired(g0, gr, g@, m2, k2, x);
                }
            }
            proof {
                if !(x@.addr != 0 && x != survivor) {
                    lemma_parts_distinct(g0, m2);
                    assert(seq![d, pred, succ][0] == d);
                    assert(seq![d, pred, succ][1] == pred);
                    assert(seq![d, pred, succ][2] == succ);
                    assert forall|y: *mut HugeDesc| #[trigger] m2.gone(k2 + 1, y) <==> m2.gone(k2, y) by {}
                    assert forall|j: int| #[trigger] m2.gone_w(k2 + 1, j) == m2.gone_w(k2, j) by {}
                }
            }
            proof {
                k2 = k2 + 1;
            }
        }
        let ghost gr = g@;
        proof {
            lemma_parts_distinct(g0, m2);
            assert forall|x: *mut HugeDesc| #[trigger] m2.gone(3, x) <==> m2.part(x) && x != survivor by {}
            assert(gr.descs@.contains_key(survivor));
            assert(desc_ok(vd, g0.next_page));
            if m.has_pred {
                assert(desc_ok(m.vp, g0.next_page));
            }
            if m.has_succ {
                assert(desc_ok(m.vs, g0.next_page));
            }
            assert(lo < hi <= g0.next_page);
            assert(!gr.elist@.contains(d)) by {
                if g0.elist@.contains(d) {
                    assert(enode(g0.elist@, d0, g0.elist@.index_of(d)));
                }
            }
        }
        // `survivor` is live: `d` (still the run's descriptor) or an
        // `Empty` neighbour that keeps its age and list position.
        g.desc_mut(survivor).start = lo as u32;
        g.desc_mut(survivor).pages = (hi - lo) as u32;
        g.desc_mut(survivor).state = merged as u8;
        g.desc_mut(survivor).usable = 0;
        let ghost gw = g@;
        proof {
            assert(gw.descs@ =~= gr.descs@.insert(survivor, gw.descs@[survivor]));
            assert(lo < u32::MAX);
            assert(same_but_links(gw.descs@[survivor].value(), HugeDesc {
                start: lo as u32,
                pages: (hi - lo) as u32,
                state: merged as u8,
                usable: 0,
                ..gr.descs@[survivor].value()
            }));
            assert(gw.descs@[survivor].value().prev == gr.descs@[survivor].value().prev);
            assert(gw.descs@[survivor].value().next == gr.descs@[survivor].value().next);
            assert(gw.descs@[survivor].value().dirty_at == gr.descs@[survivor].value().dirty_at);
            assert forall|i: int| 0 <= i < gr.elist@.len() implies #[trigger] enode(gr.elist@, gw.descs@, i) by {
                assert(enode(gr.elist@, gr.descs@, i));
            }
        }
        if merged == RunState::Purged {
            g.desc_mut(survivor).dirty_at = 0;
            proof {
                assert forall|i: int| 0 <= i < gr.elist@.len() implies #[trigger] enode(gr.elist@, g@.descs@, i) by {
                    assert(enode(gr.elist@, gw.descs@, i));
                }
            }
        } else if survivor == d {
            g.desc_mut(survivor).dirty_at = now;
            proof {
                assert forall|i: int| 0 <= i < gr.elist@.len() implies #[trigger] enode(gr.elist@, g@.descs@, i) by {
                    assert(enode(gr.elist@, gw.descs@, i));
                }
            }
            g.link_tail(d);
        }
        let ghost gl = g@;
        proof {
            let vw = gw.descs@[survivor].value();
            let vl = gl.descs@[survivor].value();
            assert(gl.descs@.contains_key(survivor));
            assert(gl.descs@.dom() =~= gr.descs@.dom());
            assert(vl.start == vw.start && vl.pages == vw.pages && vl.state == vw.state && vl.usable == vw.usable);
            assert(lo == m2.nb_lo(2) && hi == m2.nb_hi(2));
            assert forall|key: usize| lo <= key < hi implies #[trigger] gl.table.lookup(key) is None by {
                lemma_merged_room(g0, m2, key);
            }
            let v = gl.descs@[survivor].value();
            assert(v.start == lo && v.pages == hi - lo);
            assert(stat(gl.stats, 1) <= stat(g0.stats, 1) && stat(gl.stats, 2) <= stat(g0.stats, 2));
            assert(stat(gl.stats, 4) <= stat(g0.stats, 4) && stat(gl.stats, 5) <= stat(g0.stats, 5));
        }
        g.free.map.set_range(page, n);
        g.add_extent(survivor);
        proof {
            let m3 = Merge { vm: g@.descs@[survivor].value(), ..m2 };
            assert(m3.surv_ok());
            assert forall|key: usize| #[trigger] gr.table.lookup(key) == if is_key(vd, key as int) || m3.nb_key(2, key as int) {
                None
            } else {
                t0.lookup(key)
            } by {}
            assert(g0.free.map.in_meta()) by {
                reveal(Inner::wf);
            }
            assert(gl.free.map.place() == g0.free.map.place());
            assert(gl.free == g0.free);
            assert(freed_free(g0.free.map.view(), g@.free.map.view(), vd));
            assert(m3.nbrs_ok(g0));
            lemma_free_end(g0, gr, gl, g@, m3);
        }
        let first = self.sync_oldest(&g);
        drop(g);
        if first {
            purge::note_empty();
        }
        RunInfo { start, pages: n, usable, arena, state: RunState::Active, dirty_at: 0 }
    }
}

#[cold]
#[inline(never)]
pub(crate) fn not_a_run(p: usize) -> ! {
    sys::abort_fmt(format_args!("invalid free of {p:#x}: not the start of a huge allocation"))
}

#[cold]
#[inline(never)]
pub(crate) fn already_free(p: usize) -> ! {
    sys::abort_fmt(format_args!("double free of {p:#x}: huge allocation already freed"))
}

/// [`Huge::free_run`]'s abort for a page that is not in the descriptor
/// table, where the build calls [`not_a_run`] with the page's address.
///
/// Verification only (`verus_only`): the address is computed only for the
/// message, from a page no precondition bounds, so Verus sees this call in
/// place of the build's `not_a_run(self.addr(page))`, which stays as it
/// is. Trusted: the `-> !` is the whole specification, and `not_a_run`
/// never returns.
#[cfg(verus_only)]
#[verifier::external_body]
fn not_a_run_page(page: usize) -> ! {
    panic!("invalid free of huge page {page}: not the start of a huge allocation")
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
) -> ! {
    let advice = purge::advice_name();
    sys::abort_fmt(format_args!(
        "check_zero: huge run {run:#x} ({pages} pages from page {page}, arena {arena}) was handed out as zero from {prior} pages but the word at {at:#x} reads {word:#x} (purge advice {advice})"
    ))
}

} // verus!

#[cfg(test)]
mod tests;

#[cfg(test)]
mod linux_tests;
