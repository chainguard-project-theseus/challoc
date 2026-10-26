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
//! (`Table`), `tests` the allocator's tests and `linux_tests` the ones
//! that need the process's own regions.

use core::alloc::Layout;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::class::Set;
use crate::layout::Regions;
use crate::lock::{Lock, LockOrder, RawLock};
#[cfg(verus_keep_ghost)]
use crate::lock::{axiom_below_huge, axiom_entry, lock_init, rank_huge, rank_meta};
use crate::meta::Meta;
use crate::purge::{self, Batch, Curve, Decayer, Settle};
use crate::sys::{self, Errno};
#[cfg(verus_keep_ghost)]
use crate::vspec::huge::{axiom_huge_lock_frame, axiom_huge_lock_inv};
use crate::vspec::once::Once;
#[cfg(verus_keep_ghost)]
use crate::vspec::once::Step;

mod table;

use table::Table;
#[cfg(verus_keep_ghost)]
use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;
use vstd::raw_ptr::{PointsTo, PointsToRaw, ptr_mut_ref, ptr_ref};
use vstd::resource::set::GhostSetAuth;

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
    panic!("huge: {state:?} extent count underflow")
}

} // verus!

verus! {

/// What a purge walk's flushes settled: extents made `Purged` and whether
/// a discard failed. It settles the walk's batch ([`Settle`], through
/// [`Huge::settle`]).
// Ghost fields that only the proofs read.
#[allow(dead_code)]
struct Tally {
    purged: usize,
    failed: bool,
    /// Ghost: the run-page authority's set when the walk started; no
    /// purge changes it.
    held: Ghost<vstd::set::Set<int>>,
    /// Ghost: the `Empty` extents the walk visited.
    vis: Ghost<vstd::set::Set<*mut HugeDesc>>,
    /// Ghost: the members of the `Empty` list left to visit.
    rest: Ghost<Seq<*mut HugeDesc>>,
    /// Ghost: the walk pushes every extent it visits (a full purge).
    full: Ghost<bool>,
    /// Ghost: the length of the `Empty` list when the walk started.
    e0: Ghost<int>,
    /// Ghost: the bytes reported discarded ([`Settle::bytes`]).
    bytes: Ghost<int>,
    /// Ghost: the extents of ranges reported not discarded.
    refused: Ghost<vstd::set::Set<*mut HugeDesc>>,
}

impl Settle for Tally {
    type Owner = Huge;

    type Ctx = Inner;

    /// `(held, vis, rest, full, e0)`.
    type Conf = (vstd::set::Set<int>, vstd::set::Set<*mut HugeDesc>, Seq<*mut HugeDesc>, bool, int);

    /// The walk's invariant over the lock value `c`, the pieces `pend`
    /// pushed and not settled: the lock value is the allocator's, its page
    /// authority's set the one the walk started with; each piece pending
    /// is a visited `Empty` extent's range ([`has_piece`]); the walk has
    /// visited the list up to `rest` ([`walked`]); the extents purged and the list
    /// sum to the list at the start; an extent was purged iff a byte was
    /// reported discarded; and a full walk has every visited member of the
    /// list pending or refused.
    closed spec fn inv(&self, o: &Huge, c: Inner, pend: spec_fn((int, int)) -> bool) -> bool {
        &&& o.is_process()
        &&& c.wf()
        &&& c.spec_np() == o.npages
        &&& c.spec_lg() == o.lg_page
        &&& c.pgs@@ == self.held@
        &&& forall|x: (int, int)| #[trigger] pend(x) ==> has_piece(o, c, self.vis@, x)
        &&& walked(c.elist@, self.rest@, self.vis@)
        &&& self.purged + c.elist@.len() == self.e0@
        &&& self.e0@ <= u32::MAX
        &&& self.bytes@ >= 0
        &&& (self.purged > 0) == (self.bytes@ > 0)
        &&& self.full@ ==> forall|i: int|
            0 <= i < c.elist@.len() - self.rest@.len() ==> #[trigger] pend(piece_of(o, c.descs@[c.elist@[i]].value()))
                || self.refused@.contains(c.elist@[i])
    }

    closed spec fn bytes(&self) -> int {
        self.bytes@
    }

    closed spec fn conf(&self) -> (vstd::set::Set<int>, vstd::set::Set<*mut HugeDesc>, Seq<*mut HugeDesc>, bool, int) {
        (self.held@, self.vis@, self.rest@, self.full@, self.e0@)
    }

    #[inline(always)]
    fn settle(
        &mut self,
        o: &Huge,
        c: &mut Inner,
        addr: usize,
        len: usize,
        done: bool,
        piece: Ghost<int>,
        pend: Ghost<spec_fn((int, int)) -> bool>,
    ) {
        o.settle(c, addr, len, done, self, piece, pend);
    }
}

impl Tally {
    /// Nothing visited of the list `l`, of `e0` members; the page
    /// authority's set `held`, `full` for a walk that pushes every extent
    /// it visits.
    fn new(
        Ghost(held): Ghost<vstd::set::Set<int>>,
        Ghost(l): Ghost<Seq<*mut HugeDesc>>,
        Ghost(full): Ghost<bool>,
        Ghost(e0): Ghost<int>,
    ) -> (t: Tally)
        ensures
            t.purged == 0,
            t.held@ == held,
            t.vis@ == vstd::set::Set::<*mut HugeDesc>::empty(),
            t.rest@ == l,
            t.full@ == full,
            t.e0@ == e0,
            t.bytes@ == 0,
            t.refused@ == vstd::set::Set::<*mut HugeDesc>::empty(),
    {
        Tally {
            purged: 0,
            failed: false,
            held: Ghost(held),
            vis: Ghost(vstd::set::Set::empty()),
            rest: Ghost(l),
            full: Ghost(full),
            e0: Ghost(e0),
            bytes: Ghost(0),
            refused: Ghost(vstd::set::Set::empty()),
        }
    }

    /// The first member left is a live `Empty` extent, at its place in
    /// the list.
    proof fn lemma_rest_live(&self, h: &Huge, g: Inner, pend: spec_fn((int, int)) -> bool)
        requires
            self.inv(h, g, pend),
        ensures
            self.rest@.len() > 0 ==> {
                let d = self.rest@[0];
                &&& g.descs@.contains_key(d)
                &&& d@.addr != 0
                &&& g.elist@.contains(d)
                &&& g.elist@[g.elist@.len() - self.rest@.len()] == d
                &&& g.descs@[d].value().state == RunState::Empty as u8
                &&& !self.vis@.contains(d)
                &&& g.descs@[d].value().next == (if self.rest@.len() > 1 {
                    self.rest@[1]
                } else {
                    ptr::null_mut::<HugeDesc>()
                })
            },
            self.purged <= u32::MAX,
    {
        if self.rest@.len() > 0 {
            let l = g.elist@;
            let k = l.len() - self.rest@.len();
            assert(l[k] == l.subrange(k, l.len() as int)[0]);
            if self.rest@.len() > 1 {
                assert(l[k + 1] == l.subrange(k, l.len() as int)[1]);
            }
            assert(enode(l, g.descs@, k));
            assert(!self.vis@.contains(self.rest@[0]));
            assert(perms_ok(g.descs@));
        }
    }

    /// The count purged is at most `u32::MAX`.
    proof fn lemma_counts(&self, h: &Huge, g: Inner)
        requires
            self.inv(h, g, purge::none()),
        ensures
            self.purged <= u32::MAX,
    {
    }
}

/// The range a huge purge pushes for extent `v` of `h`: its pages.
spec fn piece_of(h: &Huge, v: HugeDesc) -> (int, int) {
    (h.spec_base() as int + v.start * h.spec_page(), v.pages * h.spec_page())
}

/// `x` is the range of an `Empty` extent of `g` that a walk visited (in
/// `vis`): what a huge purge's batch holds pending. Opaque, so a
/// settler's invariant carries it per piece as one fact.
#[verifier::opaque]
spec fn has_piece(h: &Huge, g: Inner, vis: vstd::set::Set<*mut HugeDesc>, x: (int, int)) -> bool {
    exists|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && g.descs@[d].value().state == RunState::Empty as u8 && vis.contains(d)
            && piece_of(h, g.descs@[d].value()) == x
}

/// A walk of the `Empty` list `l` that has visited (`vis`) its members up
/// to `rest`, the members left: `l` ends with `rest`, every member before
/// is visited, and none of `rest` is.
spec fn walked(l: Seq<*mut HugeDesc>, rest: Seq<*mut HugeDesc>, vis: vstd::set::Set<*mut HugeDesc>) -> bool {
    &&& rest.len() <= l.len()
    &&& l.subrange(l.len() - rest.len(), l.len() as int) == rest
    &&& forall|i: int| 0 <= i < l.len() - rest.len() ==> vis.contains(#[trigger] l[i])
    &&& forall|j: int| 0 <= j < rest.len() ==> !vis.contains(#[trigger] rest[j])
}

/// A visited member's removal (a settled extent's unlink) keeps the walk.
proof fn lemma_walked_remove(l: Seq<*mut HugeDesc>, rest: Seq<*mut HugeDesc>, vis: vstd::set::Set<*mut HugeDesc>, i: int)
    requires
        walked(l, rest, vis),
        0 <= i < l.len(),
        vis.contains(l[i]),
    ensures
        walked(l.remove(i), rest, vis),
        i < l.len() - rest.len(),
{
    let k = l.len() - rest.len();
    if i >= k {
        assert(l[i] == l.subrange(k, l.len() as int)[i - k]);
    }
    let m = l.remove(i);
    assert(m.subrange(m.len() - rest.len(), m.len() as int) =~= rest) by {
        assert forall|j: int| 0 <= j < rest.len() implies m.subrange(m.len() - rest.len(), m.len() as int)[j] == rest[j] by {
            assert(m[k - 1 + j] == l[k + j]);
            assert(l[k + j] == l.subrange(k, l.len() as int)[j]);
        }
    }
    assert forall|x: int| 0 <= x < m.len() - rest.len() implies vis.contains(#[trigger] m[x]) by {
        if x < i {
            assert(m[x] == l[x]);
        } else {
            assert(m[x] == l[x + 1]);
        }
    }
}

/// Visiting the first member left keeps the walk.
proof fn lemma_walked_visit(l: Seq<*mut HugeDesc>, rest: Seq<*mut HugeDesc>, vis: vstd::set::Set<*mut HugeDesc>)
    requires
        walked(l, rest, vis),
        rest.len() > 0,
        l.no_duplicates(),
    ensures
        walked(l, rest.drop_first(), vis.insert(rest[0])),
        l[l.len() - rest.len()] == rest[0],
{
    let k = l.len() - rest.len();
    let r1 = rest.drop_first();
    assert(l[k] == l.subrange(k, l.len() as int)[0]);
    assert(l.subrange(l.len() - r1.len(), l.len() as int) =~= r1) by {
        assert forall|j: int| 0 <= j < r1.len() implies l.subrange(l.len() - r1.len(), l.len() as int)[j] == r1[j] by {
            assert(l[k + 1 + j] == l.subrange(k, l.len() as int)[j + 1]);
        }
    }
    assert forall|x: int| 0 <= x < l.len() - r1.len() implies vis.insert(rest[0]).contains(#[trigger] l[x]) by {
        if x < k {
            assert(vis.contains(l[x]));
        }
    }
    assert forall|j: int| 0 <= j < r1.len() implies !vis.insert(rest[0]).contains(#[trigger] r1[j]) by {
        assert(r1[j] == rest[j + 1]);
        assert(l[k + 1 + j] == l.subrange(k, l.len() as int)[j + 1]);
        assert(l[k] != l[k + 1 + j]);
    }
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
    /// Ghost: the authority of the `Active` descriptors' extents
    /// ([`run_loc`]).
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
/// process, the huge allocator's.
pub uninterp spec fn run_loc() -> vstd::resource::Loc;

/// The authority of the live runs' pages.
pub uninterp spec fn page_loc() -> vstd::resource::Loc;

/// Byte `b` lies in one of the `np` pages of `pg` bytes of the region at
/// `base`, and that page is not in `held`, the pages the run authority
/// holds ([`page_loc`]): a page of no live run. What a huge purge's
/// [`Batch`] may discard.
spec fn free_page_byte(base: int, pg: int, np: int, held: vstd::set::Set<int>, b: int) -> bool {
    &&& base <= b
    &&& (b - base) / pg < np
    &&& !held.contains((b - base) / pg)
}

/// Trusted: outside tests, [`Huge::new`] runs once per process, for the
/// process's `Shared` (`Shared::init`), so the run authorities it makes
/// are the process's ([`run_loc`], [`page_loc`]), as
/// `class::table::axiom_slot_loc` for the slot authorities. Two calls
/// with two pairs of authorities would prove `false`, so a call uses up
/// `once`, the process's one token of this step (`vspec::once`), handed
/// to `Huge::new`.
// `once` is read only by the requires (ghost).
#[allow(unused_variables)]
axiom fn axiom_run_locs(r: vstd::resource::Loc, p: vstd::resource::Loc, tracked once: Once)
    requires
        once.step() == Step::Huge,
    ensures
        run_loc() == r,
        page_loc() == p,
;

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
    /// `base`, below the end of the user address space; the lock's first
    /// value has the same page count and shift, and the lock its rank.
    #[verifier::type_invariant]
    spec fn inv(&self) -> bool {
        &&& self.lg_page < 64
        &&& 4 <= self.lg_page
        &&& 0 < self.npages <= u32::MAX
        &&& self.base + self.npages * pow2(self.lg_page as nat) <= crate::meta::va_limit()
        &&& lock_init(self.inner).spec_np() == self.npages
        &&& lock_init(self.inner).spec_lg() == self.lg_page
        &&& self.inner.rank() == rank_huge()
    }
}

} // verus!

/// `PROT_NONE`, `PROT_READ | PROT_WRITE` and the guard advice of
/// `sys` (checked against them below).
const PROT_NONE: i32 = 0;
const PROT_RW: i32 = 3;
const MADV_GUARD_INSTALL: i32 = 102;
const MADV_GUARD_REMOVE: i32 = 103;

const _: () = assert!(
    PROT_NONE == sys::PROT_NONE
        && PROT_RW == sys::PROT_READ | sys::PROT_WRITE
        && MADV_GUARD_INSTALL == sys::MADV_GUARD_INSTALL
        && MADV_GUARD_REMOVE == sys::MADV_GUARD_REMOVE
);

fn commit(addr: usize, len: usize) -> Result<(), Errno> {
    // SAFETY: inside our own reservation; adding permissions to pages
    // nobody holds cannot break anything.
    unsafe { sys::mprotect(addr, len, PROT_RW) }
}

fn protect_none(addr: usize, len: usize) -> Result<(), Errno> {
    // SAFETY: the page is a guard that no caller may touch.
    unsafe { sys::mprotect(addr, len, PROT_NONE) }
}

fn guard_install(addr: usize, len: usize) -> Result<(), Errno> {
    // SAFETY: as `protect_none`.
    unsafe { sys::madvise(addr, len, MADV_GUARD_INSTALL) }
}

fn guard_remove(addr: usize, len: usize) -> Result<(), Errno> {
    // SAFETY: the run is being freed; the guard protects nothing now.
    unsafe { sys::madvise(addr, len, MADV_GUARD_REMOVE) }
}

fn now_ns() -> u64 {
    sys::now_coarse_ns()
}

/// A block for a descriptor from `meta`; null when the metadata region is
/// exhausted. [`free_desc`] gives it back.
#[inline(always)]
fn alloc_desc(meta: &Meta) -> *mut HugeDesc {
    meta.alloc(Layout::new::<HugeDesc>()) as *mut HugeDesc
}

verus! {

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
fn free_desc(meta: &Meta, d: *mut HugeDesc, blk: Tracked<(PointsToRaw, crate::meta::MetaClaim)>)
    requires
        crate::sys::may_syscall(),
        d@.addr != 0,
        blk@.0.is_range(d@.addr as int, vstd::layout::size_of::<HugeDesc>() as int),
        blk@.0.provenance() == d@.provenance,
        desc_claim_for(blk@.1, d, meta.live_id()),
{
    proof {
        assert((d as *mut u8)@.addr == d@.addr && (d as *mut u8)@.provenance == d@.provenance);
    }
    meta.free(d as *mut u8, Layout::new::<HugeDesc>(), blk);
}

impl Inner {
    /// The invariant of the huge lock: the table is well formed and every
    /// descriptor it or the `Empty` list names is live and consistent;
    /// the live descriptors (`all`) are the extents, pairwise disjoint,
    /// each under its keys in the table and nowhere else, and the counters
    /// are their tallies, whose pages sum to `next_page`; the run
    /// authorities (`runs`, `pgs`) hold the `Active` extents and their pages;
    /// no two free extents touch; `dclaims` holds every live descriptor's
    /// block claim, from the `Meta` at `mid`; and the free-page bitmap holds
    /// exactly the free extents' pages.
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
        &&& self.runs@.id() == run_loc()
        &&& self.pgs@.id() == page_loc()
        &&& self.runs@@ == active_runs(self.descs@)
        &&& self.pgs@@ == active_pages(self.descs@)
        &&& coalesced(self.descs@)
        &&& forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> desc_claimed(self.dclaims@, d, self.mid@)
        &&& free_marked(self.descs@, self.free.map@)
        &&& free_exact(self.descs@, self.free.map@)
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
            old(self).table.spec_count() + weight(old(self).descs@[d].value(), 6) < old(self).table.spec_cap(),
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
    /// [`retire`](Inner::retire) it.
    fn drop_extent(&mut self, d: *mut HugeDesc)
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
    {
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
    fn retire(&mut self, meta: &Meta, d: *mut HugeDesc)
        requires
            crate::sys::may_syscall(),
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
        free_desc(meta, d, Tracked(desc_raw((perm, dc))));
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
    fn end(&self) -> usize {
        self.start + self.pages
    }
}

verus! {

impl Huge {
    /// Create the huge allocator for `regions`, reserving its free-page
    /// bitmap and descriptor table in `meta`. `guard_install` should be the
    /// probed `MADV_GUARD_INSTALL` support. `None` if the metadata region
    /// cannot hold the tables.
    pub fn new(
        meta: &Meta,
        regions: &Regions,
        guard_install: bool,
        Tracked(once): Tracked<Once>,
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
            once.step() == Step::Huge,
        ensures
            r matches Some(h) ==> h.spec_npages() == (regions.huge_len >> regions.lg_page) as int
                && h.spec_page() == (1usize << regions.lg_page) as int,
            *final(ord) == *old(ord),
    {
        broadcast use axiom_huge_lock_inv;

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
        let table = Table::new(meta, page)?;
        let tracked descs = Map::tracked_empty();
        // The run authorities, empty: no run is live yet.
        let tracked (runs, _) = GhostSetAuth::<(int, int)>::new(vstd::set::Set::empty());
        let tracked (pgs, _) = GhostSetAuth::<int>::new(vstd::set::Set::empty());
        proof {
            axiom_run_locs(runs.id(), pgs.id(), once);
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
                    runs: Tracked(runs),
                    pgs: Tracked(pgs),
                },
                Ghost(rank_huge()),
            ),
        })
    }

    /// The huge lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> &RawLock {
        self.inner.raw()
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

    /// The page size.
    pub closed spec fn spec_page(&self) -> int {
        pow2(self.lg_page as nat) as int
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
    /// and the process's regions are well formed and separated
    /// (`heap::regions_ok`). Closed: [`Huge::lemma_purge_range`] reads it.
    pub closed spec fn is_process(&self) -> bool {
        let r = crate::layout::process_regions();
        &&& crate::heap::regions_ok()
        &&& self.spec_base() == r.huge_base
        &&& self.spec_page() == (1usize << r.lg_page) as int
        &&& self.spec_npages() == (r.huge_len >> r.lg_page) as int
        &&& self.spec_base_aligned()
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
}

} // verus!

impl Huge {
    /// Pages a run for `size` bytes at `align` takes, its guard included, or
    /// `None` when no placement in the region can hold it: the size cannot
    /// be represented, the run is longer than the region, or `align` is
    /// larger than the region. [`Huge::alloc_run`] and [`Huge::placeable`]
    /// share these checks.
    fn run_pages(&self, size: usize, align: usize) -> Option<usize> {
        let n = self.usable_pages(size)?;
        let need = n.checked_add(GUARD_PAGES)?;
        if need > self.npages || align > (self.npages << self.lg_page) {
            return None;
        }
        Some(need)
    }

    /// Whether [`Huge::alloc_run`] can place a run for `size` bytes at
    /// `align` in an empty region: [`Huge::run_pages`] holds, and the run
    /// fits from the first `align`-aligned page. Aligned starts repeat every
    /// `align` bytes, so if the first one leaves no room, none does.
    pub fn placeable(&self, size: usize, align: usize) -> bool {
        // `run_pages(size, align).is_some_and(|need| ...)`, spelled as a
        // `match`.
        match self.run_pages(size, align) {
            Some(need) => {
                let s = self.align_page(0, align);
                s + need <= self.npages
            }
            None => false,
        }
    }

    /// First page at or after `page` whose address is a multiple of
    /// `align` (a power of two).
    #[inline]
    fn align_page(&self, page: usize, align: usize) -> usize {
        if align <= self.page_size() {
            return page;
        }
        let a = self.addr(page);
        let aligned = (a + align - 1) & !(align - 1);
        (aligned - self.base) >> self.lg_page
    }

    /// [`Huge::alloc_run`] as the pointer and the zero flag alone.
    #[cfg(test)]
    pub fn alloc(&self, meta: &Meta, size: usize, align: usize, arena: u16) -> (*mut u8, bool) {
        let r = self.alloc_run(meta, size, align, arena);
        (r.ptr, r.zeroed)
    }

    /// Allocate `size` bytes aligned to `align` (a power of two; anything
    /// up to a page is free). Returns the pointer, whether the memory is
    /// known to read as zero and the pages of the run as its descriptor
    /// records them (the guard included; what [`Huge::free_run`] will
    /// report as `pages`, so the caller's per-arena page count never
    /// drifts from what the free subtracts); [`Placed::NONE`] when the
    /// region, the metadata region or the kernel refuses.
    // Cold: out of line in `Shared::alloc_huge`, so the ghost arguments that
    // verified code erases cannot change whether it is inlined.
    #[inline(never)]
    pub fn alloc_run(&self, meta: &Meta, size: usize, align: usize, arena: u16) -> Placed {
        assert!(
            align.is_power_of_two(),
            "huge: alignment not a power of two"
        );
        self.alloc_aligned(meta, size, align, arena)
    }

    /// [`Huge::alloc_run`] for an `align` already known to be a power of
    /// two.
    #[inline(always)]
    fn alloc_aligned(&self, meta: &Meta, size: usize, align: usize, arena: u16) -> Placed {
        let Some(need) = self.run_pages(size, align) else {
            return Placed::NONE;
        };
        let usable_pages = need - GUARD_PAGES;
        let page = self.page_size();
        let mut g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
        let Some((s, ext)) = self.find(&g, need, align) else {
            return Placed::NONE;
        };
        let end = s + need;

        // Everything fallible happens before any state changes.
        let run = alloc_desc(meta);
        if run.is_null() {
            return Placed::NONE;
        }
        let spare = alloc_desc(meta);
        if spare.is_null() {
            free_desc(meta, run, Tracked::assume_new());
            return Placed::NONE;
        }
        let ok = g.table.ensure(meta, page, 3).is_ok()
            && (end <= g.next_page
                || g.free
                    .ensure(meta, page, end - 1, Tracked::assume_new())
                    .is_ok())
            && self.prepare(s, usable_pages, g.next_page).is_ok();
        if !ok {
            free_desc(meta, run, Tracked::assume_new());
            free_desc(meta, spare, Tracked::assume_new());
            return Placed::NONE;
        }

        // `check_zero` needs the extent's state before `take` may free it.
        let prior = if purge::check_zero() {
            Some(self.prior_state(s, ext))
        } else {
            None
        };
        let zeroed = self.take(
            &mut g,
            meta,
            Placement {
                start: s,
                pages: need,
                ext,
            },
            run,
            spare,
            arena,
        );
        // `take` wrote the run's descriptor; the lock is still held.
        let pages = g.desc(run).pages();
        self.sync_oldest(&g);
        if let (true, Some(prior)) = (zeroed, prior) {
            self.check_zero(s, usable_pages, prior, arena);
        }
        Placed {
            ptr: crate::vspec::addr::addr_to_ptr(self.addr(s)),
            zeroed,
            pages,
        }
    }

    /// Where the pages of a run at `s` come from, for `check_zero`: the
    /// free extent's state, or "fresh" for never-touched pages at or past
    /// the mark (which is also what an aligned start beyond `ext` gets).
    #[cold]
    fn prior_state(&self, s: usize, ext: *mut HugeDesc) -> &'static str {
        if ext.is_null() {
            return "fresh";
        }
        // SAFETY: `ext` is a live free extent found under the lock the
        // caller holds.
        let (st, e_end) = unsafe { ((*ext).state(), (*ext).end()) };
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
    #[cold]
    #[inline(never)]
    fn check_zero(&self, s: usize, usable_pages: usize, prior: &'static str, arena: u16) {
        let page = self.page_size();
        let base = self.addr(s);
        for i in 0..usable_pages {
            for &off in &[0, page - 8] {
                let a = base + i * page + off;
                // SAFETY: a usable page of the run this call just took;
                // `prepare` committed it.
                let word = unsafe { self.read_run_word(a) };
                if word != 0 {
                    dirty_zero_run(base, usable_pages, s, prior, arena, a, word);
                }
            }
        }
    }

    /// The word at `a`, read once, for [`Huge::check_zero`]. Nothing is
    /// claimed about the value.
    ///
    /// # Safety
    /// `a` is an aligned word of a committed usable page of a run.
    #[inline(always)]
    unsafe fn read_run_word(&self, a: usize) -> u64 {
        let q = crate::vspec::addr::addr_to_ptr(a);
        // SAFETY: the caller's.
        unsafe { ptr::read_volatile(q as *const u64) }
    }
}

verus! {

impl Huge {
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
}

} // verus!

impl Huge {
    /// Lowest-address first fit for `need` pages at `align`. Returns the
    /// start page and the free extent it comes from (`None` for fresh pages
    /// at the high-water mark). The extent may end below `s + need` only if
    /// it touches the mark.
    fn find(&self, g: &Inner, need: usize, align: usize) -> Option<(usize, *mut HugeDesc)> {
        let mut from = 0;
        while let Some(a) = g.free.map.lowest_set_from(from) {
            let e = g.extent_at(a);
            // `extent_at` checked `e` is live.
            let e_end = g.desc(e).end();
            let s = self.align_page(a, align);
            if s + need <= e_end {
                return Some((s, e));
            }
            if e_end == g.next_page {
                return if s + need <= self.npages {
                    Some((s, e))
                } else {
                    None
                };
            }
            from = e_end;
        }
        let s = self.align_page(g.next_page, align);
        if s + need <= self.npages {
            Some((s, ptr::null_mut()))
        } else {
            None
        }
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
    fn prepare(&self, s: usize, usable: usize, watermark: usize) -> Result<(), Errno> {
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

    /// Record the run `p` taken from its extent (or fresh pages), splitting
    /// the extent around it. Infallible. Returns the zero flag.
    fn take(
        &self,
        g: &mut Inner,
        meta: &Meta,
        p: Placement,
        run: *mut HugeDesc,
        spare: *mut HugeDesc,
        arena: u16,
    ) -> bool {
        let Placement {
            start: s,
            pages: need,
            ext,
        } = p;
        let end = p.end();
        let old_mark = g.next_page;
        let mut spare_used = false;
        let mut zeroed = true;
        if ext.is_null() {
            if s > old_mark {
                // Alignment left a hole at the mark: a clean free extent.
                init_free(spare, old_mark, s - old_mark, RunState::Purged, 0);
                g.free.map.set_range(old_mark, s - old_mark);
                g.add_extent(spare);
                spare_used = true;
            }
        } else {
            // `ext` is a live free extent found under this lock.
            let (a, e_end, st, dirty_at) = (
                g.desc(ext).start(),
                g.desc(ext).end(),
                g.desc(ext).state(),
                g.desc(ext).dirty_at,
            );
            // `Purged` pages are zero unless a lazy purge discarded them
            // with `MADV_FREE`; pages at or past the mark are untouched.
            zeroed = (st == RunState::Purged && purge::purged_is_zero()) || s >= e_end;
            g.drop_extent(ext);
            let taken_lo = s.max(a);
            let taken_hi = end.min(e_end);
            if taken_lo < taken_hi {
                g.free.map.clear_range(taken_lo, taken_hi - taken_lo);
            }
            let tail = e_end.saturating_sub(end);
            if tail > 0 {
                // The tail inherits the state and age of `ext` and, when
                // `Empty`, its place in the FIFO (same `dirty_at`).
                init_free(spare, end, tail, st, dirty_at);
                if st == RunState::Empty {
                    g.link_after(ext, spare);
                }
                g.add_extent(spare);
                spare_used = true;
            }
            if s > a {
                // Head survives (and, when `s >= e_end`, absorbs the hole
                // between the old extent and the aligned start).
                if s > e_end {
                    g.free.map.set_range(e_end, s - e_end);
                }
                g.desc_mut(ext).pages = (s - a) as u32;
                g.add_extent(ext);
            } else {
                g.retire(meta, ext);
            }
        }
        if end > old_mark {
            g.next_page = end;
        }
        // SAFETY: `run` is a fresh metadata block for a `HugeDesc`.
        unsafe {
            ptr::write(
                run,
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
        }
        g.add_extent(run);
        g.stats.next_page = g.next_page;
        if !spare_used {
            free_desc(meta, spare, Tracked::assume_new());
        }
        zeroed
    }

    /// [`Huge::free_run`] returning only the usable bytes released.
    #[cfg(test)]
    pub fn free(&self, meta: &Meta, page: usize) -> usize {
        self.free_run(meta, page).usable
    }

    /// Free the run starting at `page` (from `Regions::classify`). Aborts
    /// unless it is the start of a live run. Returns the run as it was
    /// (`start`, `pages` with the guard, `usable`, `arena`), for the
    /// caller's per-arena counters.
    // The coalescing loops are spelled out (see below), not `while let`.
    #[allow(clippy::while_let_loop)]
    pub fn free_run(&self, meta: &Meta, page: usize) -> RunInfo {
        let page_size = self.page_size();
        let mut g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
        let d = g.table.get(page);
        if d.is_null() {
            not_a_run(self.addr(page));
        }
        // Table entries are live descriptors.
        let (start, n, state, usable, arena) = (
            g.desc(d).start(),
            g.desc(d).pages(),
            g.desc(d).state(),
            g.desc(d).usable,
            g.desc(d).arena(),
        );
        if state != RunState::Active {
            already_free(self.addr(page));
        }
        if start != page {
            not_a_run(self.addr(page));
        }
        let addr = self.addr(page);
        if self.guard_install
            && let Err(e) = guard_remove(
                addr + (n - GUARD_PAGES) * page_size,
                GUARD_PAGES * page_size,
            )
        {
            panic!("huge: madvise(MADV_GUARD_REMOVE) of {addr:#x} failed: {e}");
        }
        let now = now_ns().max(1);
        // Purged at once: above `huge_retain_max`, or under a zero huge
        // decay period (`huge_dirty_decay_ms:0`). A run the discard fails
        // on is kept `Empty` for decay or the next explicit purge.
        let cap = self.retain_max();
        let purge = usable > cap || self.purge_on_free();
        let st = if purge && purge::discard_one(addr, n * page_size, Ghost::assume_new()) {
            RunState::Purged
        } else {
            RunState::Empty
        };
        // Best effort: a fuller table still works, only slower.
        let _ = g.table.ensure(meta, page_size, 2);
        g.drop_extent(d);

        // Coalesce with free neighbours into one extent `[lo, hi)`. The
        // survivor is the `Empty` participant with the oldest `dirty_at`
        // (it keeps its place in the FIFO list), else `d`; the other
        // descriptors go back to `meta`. A neighbour whose discards are
        // refused (locked pages) absorbs the run all the same, which then
        // stays dirty with it until the unlock or a reuse (design 8).
        let pred = g.free_before(page);
        let succ = g.free_after(page + n);
        let mut lo = page;
        let mut hi = page + n;
        let mut merged = st;
        let mut survivor = d;
        // `for nb in [pred, succ]`, spelled out: Verus rejects `continue`
        // in a `for` loop, and the spelled-out loop keeps the machine code
        // of the `for`.
        let mut iter = [pred, succ].into_iter();
        loop {
            let nb = match iter.next() {
                Some(nb) => nb,
                None => {
                    break;
                }
            };
            if nb.is_null() {
                continue;
            }
            // Neighbours are live free extents.
            let (ns, ne, nst, ndirty) = (
                g.desc(nb).start(),
                g.desc(nb).end(),
                g.desc(nb).state(),
                g.desc(nb).dirty_at,
            );
            g.drop_extent(nb);
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
        // `for x in [d, pred, succ]`, spelled out as above.
        let mut iter = [d, pred, succ].into_iter();
        loop {
            let x = match iter.next() {
                Some(x) => x,
                None => {
                    break;
                }
            };
            if !x.is_null() && x != survivor {
                g.retire(meta, x);
            }
        }
        // `survivor` is live: `d` (still the run's descriptor) or an
        // `Empty` neighbour that keeps its age and list position.
        g.desc_mut(survivor).start = lo as u32;
        g.desc_mut(survivor).pages = (hi - lo) as u32;
        g.desc_mut(survivor).state = merged as u8;
        g.desc_mut(survivor).usable = 0;
        if merged == RunState::Purged {
            g.desc_mut(survivor).dirty_at = 0;
        } else if survivor == d {
            g.desc_mut(survivor).dirty_at = now;
            g.link_tail(d);
        }
        g.free.map.set_range(page, n);
        g.add_extent(survivor);
        let first = self.sync_oldest(&g);
        drop(g);
        if first {
            purge::note_empty();
        }
        RunInfo {
            start,
            pages: n,
            usable,
            arena,
            state: RunState::Active,
            dirty_at: 0,
        }
    }
}

verus! {

impl Huge {
    /// `r` is the usable size of a run starting at `page`: whole pages, at
    /// least one, with the run's guard after them inside the region.
    pub open spec fn spec_run_usable(&self, page: usize, r: usize) -> bool {
        &&& r >= self.spec_page()
        &&& (r as int) % self.spec_page() == 0
        &&& page * self.spec_page() + r + GUARD_PAGES * self.spec_page() <= self.spec_npages() * self.spec_page()
    }

    /// Usable bytes of the live run starting at `page`. Aborts otherwise.
    /// The result is the bytes of the run's usable pages, as its descriptor
    /// counts them (`Huge::take` writes `usable` from the pages it commits).
    pub fn usable_size(&self, page: usize) -> (r: usize)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            page <= self.spec_npages(),
        ensures
            page < self.spec_npages(),
            self.spec_run_usable(page, r),
    {
        broadcast use {axiom_huge_lock_inv, axiom_huge_lock_frame};

        proof {
            use_type_invariant(self);
            self.lemma_page();
        }
        // Premise (lock order): the callers hold no allocator lock.
        let tracked mut ord = axiom_entry();
        let g = self.inner.lock(Tracked(&mut ord), Tracked(&mut LockOrder::spare()));
        let d = g.table.get(page);
        // Table entries are live descriptors.
        if d.is_null() || g.desc(d).start() != page {
            not_a_run(self.addr(page));
        }
        if g.desc(d).state() != RunState::Active {
            already_free(self.addr(page));
        }
        proof {
            // A live descriptor's pages lie below the mark.
            assert(g@.descs@.contains_key(d));
            let v = g@.descs@[d].value();
            assert(desc_ok(v, g@.next_page));
            assert(v.start == page && g@.next_page <= g@.np@);
            lemma_state_byte(v.state);
            assert(g@.spec_lg() == self.lg_page);
            // An `Active` descriptor's usable pages and guard are its pages.
            assert(run_ok(v, g@.lg@));
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
    pub fn lookup(&self, page: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<RunInfo>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
    {
        broadcast use {axiom_huge_lock_inv, axiom_huge_lock_frame};

        proof {
            use_type_invariant(self);
        }
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
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
}

} // verus!

verus! {

impl Huge {
    /// Purge every `Empty` extent whose `dirty_at` is at or before `cutoff`
    /// (`u64::MAX` for all): `MADV_DONTNEED`, then `Purged`; an extent
    /// whose discard fails stays `Empty` in its place (the next purge
    /// retries it). Returns the number purged, at most `u32::MAX`.
    pub fn purge_empty(&self, cutoff: u64, Tracked(ord): Tracked<&mut LockOrder>) -> (r: usize)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            self.is_process(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            r <= u32::MAX,
    {
        broadcast use axiom_huge_lock_inv, axiom_huge_lock_frame;

        proof {
            use_type_invariant(self);
        }
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        proof {
            use_type_invariant(self);
            self.lemma_page();
            g@.lemma_elist();
        }
        // The batch may discard pages of no live run: the run authority
        // does not change while the lock is held here.
        let ghost held = g@.pgs@@;
        let ghost full = cutoff == u64::MAX;
        let mut t = Tally::new(Ghost(held), Ghost(g@.elist@), Ghost(full), Ghost(g@.elist@.len() as int));
        let mut batch = Batch::new(Ghost(|b: int| self.purgeable(held, b)));
        let mut d = g.empty_head;
        proof {
            reveal(has_piece);
            assert(g@.elist@.subrange(0, g@.elist@.len() as int) =~= g@.elist@);
            g@.lemma_head();
            assert(t.inv(self, g@, batch.pieces()));
            t.lemma_rest_live(self, g@, batch.pieces());
        }
        while !d.is_null()
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                self.is_process(),
                batch.wf(),
                forall|b: int| #[trigger] (batch.free())(b) == self.purgeable(held, b),
                batch.sent() == t.bytes@,
                t.inv(self, g@, batch.pieces()),
                t.held@ == held,
                t.full@ == (cutoff == u64::MAX),
                d == (if t.rest@.len() > 0 { t.rest@[0] } else { ptr::null_mut::<HugeDesc>() }),
                t.rest@.len() > 0 ==> d@.addr != 0,
            ensures
                cutoff == u64::MAX ==> t.rest@.len() == 0,
            decreases t.rest@.len(),
        {
            proof {
                if t.rest@.len() == 0 {
                    assert(d@.addr == 0);
                }
                t.lemma_rest_live(self, g@, batch.pieces());
            }
            // SAFETY: list members are live `Empty` extents; `next` is
            // read before a flush can unlink `d`.
            let (dirty_at, next) = (g.desc(d).dirty_at, g.desc(d).next);
            if dirty_at > cutoff {
                break;
            }
            self.push_extent(&mut g, d, &mut batch, &mut t);
            proof {
                t.lemma_rest_live(self, g@, batch.pieces());
            }
            d = next;
        }
        batch.flush(&mut t, self, &mut g);
        self.sync_oldest(&g);
        proof {
            t.lemma_counts(self, g@);
        }
        t.purged
    }

    /// Push `d`, the first member of the `Empty` list a walk has left
    /// (`t.rest`), to `batch`; it stays there, `Empty`, until the flush
    /// reports it discarded ([`settle`]). The batch is flushed before the
    /// lock is released, so no allocation can take the extent between the
    /// discard and the state change. Returns its pages.
    ///
    /// [`settle`]: Huge::settle
    fn push_extent(&self, g: &mut Inner, d: *mut HugeDesc, batch: &mut Batch, t: &mut Tally) -> (r: usize)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(t).inv(self, *old(g), old(batch).pieces()),
            old(batch).wf(),
            forall|b: int| #[trigger] (old(batch).free())(b) == self.purgeable(old(t).held@, b),
            old(t).rest@.len() > 0,
            d == old(t).rest@[0],
        ensures
            final(t).inv(self, *final(g), final(batch).pieces()),
            final(batch).wf(),
            final(batch).free() == old(batch).free(),
            final(batch).sent() - old(batch).sent() == final(t).bytes() - old(t).bytes(),
            final(t).held == old(t).held,
            final(t).vis@ == old(t).vis@.insert(d),
            final(t).rest@ == old(t).rest@.drop_first(),
            final(t).full == old(t).full,
            r == old(g).descs@[d].value().pages,
    {
        let ghost d0 = g.descs@;
        let ghost l0 = g.elist@;
        let ghost t0 = *t;
        proof {
            use_type_invariant(self);
            self.lemma_page();
            t.lemma_rest_live(self, *g, batch.pieces());
            let v = d0[d].value();
            let p = self.spec_page();
            assert(desc_ok(v, g.next_page));
            self.lemma_pages((v.start + v.pages) as usize);
            self.lemma_pages(v.start as usize);
            assert((v.start + v.pages) * p == v.start * p + v.pages * p) by (nonlinear_arith);
        }
        // SAFETY: list members are live `Empty` extents.
        let (start, pages) = (g.desc(d).start(), g.desc(d).pages());
        let ghost new = piece_of(self, d0[d].value());
        // The extent's pages are no live run's: the page authority holds
        // the `Active` extents' pages, and those are apart from `d`.
        proof {
            let v = d0[d].value();
            let p = self.spec_page();
            let base = self.base as int;
            lemma_run_auths(*g);
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
            // Page-aligned extent pages inside the huge region.
            self.lemma_purge_range(v.start as int, v.pages as int);
            // Apart from the extents pending, each another visited one.
            reveal(has_piece);
            assert forall|x: (int, int)| #[trigger] (batch.pieces())(x) implies x.0 + x.1 <= new.0 || new.0 + new.1
                <= x.0 by {
                let y = choose|y: *mut HugeDesc|
                    #[trigger] g.descs@.contains_key(y) && g.descs@[y].value().state == RunState::Empty as u8
                        && t0.vis@.contains(y) && piece_of(self, g.descs@[y].value()) == x;
                assert(y != d);
                let w = d0[y].value();
                assert(apart(v, w));
                if v.start + v.pages <= w.start {
                    assert(v.start * p + v.pages * p <= w.start * p) by (nonlinear_arith)
                        requires v.start + v.pages <= w.start, p > 0;
                } else {
                    assert(w.start * p + w.pages * p <= v.start * p) by (nonlinear_arith)
                        requires w.start + w.pages <= v.start, p > 0;
                }
            }
            lemma_walked_visit(l0, t0.rest@, t0.vis@);
            assert(l0.no_duplicates());
        }
        t.vis = Ghost(t.vis@.insert(d));
        t.rest = Ghost(t.rest@.drop_first());
        proof {
            reveal(has_piece);
            let pend = purge::either(batch.pieces(), purge::one(new));
            assert forall|x: (int, int)| #[trigger] pend(x) implies has_piece(self, *g, t.vis@, x) by {
                if x == new {
                    assert(g.descs@.contains_key(d));
                } else {
                    let y = choose|y: *mut HugeDesc|
                        #[trigger] g.descs@.contains_key(y) && g.descs@[y].value().state == RunState::Empty as u8
                            && t0.vis@.contains(y) && piece_of(self, g.descs@[y].value()) == x;
                    assert(t.vis@.contains(y));
                }
            }
            if t.full@ {
                let l = g.elist@;
                let k = l.len() - t0.rest@.len();
                assert forall|i: int| 0 <= i < l.len() - t.rest@.len() implies #[trigger] pend(
                    piece_of(self, g.descs@[l[i]].value()),
                ) || t.refused@.contains(l[i]) by {
                    if i == k {
                        assert(l[i] == d);
                    }
                }
            }
            assert(t.inv(self, *g, pend));
        }
        batch.push(self.addr(start), pages * self.page_size(), t, self, g);
        pages
    }

    /// Make `d`, a member of the `Empty` list, `Purged`: out of the list,
    /// with no age, its pages counted as purged. Returns its pages.
    #[inline(always)]
    fn purge_desc(&self, g: &mut Inner, d: *mut HugeDesc) -> (r: usize)
        requires
            crate::sys::may_abort(),
            old(g).wf(),
            old(g).elist@.contains(d),
        ensures
            final(g).wf(),
            final(g).spec_np() == old(g).spec_np(),
            final(g).spec_lg() == old(g).spec_lg(),
            final(g).next_page == old(g).next_page,
            final(g).pgs == old(g).pgs,
            r == old(g).descs@[d].value().pages,
            final(g).elist@ == old(g).elist@.remove(old(g).elist@.index_of(d)),
            final(g).descs@.dom() == old(g).descs@.dom(),
            forall|x: *mut HugeDesc| #[trigger] old(g).descs@.contains_key(x) ==> final(g).descs@[x].value().pages
                == old(g).descs@[x].value().pages && final(g).descs@[x].value().start == old(g).descs@[x].value().start
                && (x != d ==> final(g).descs@[x].value().state == old(g).descs@[x].value().state),
            final(g).descs@[d].value().state == RunState::Purged as u8,
    {
        let ghost j = g.elist@.index_of(d);
        let ghost d0 = g.descs@;
        let ghost l0 = g.elist@;
        proof {
            g.lemma_elist();
            assert(enode(l0, d0, j));
        }
        // SAFETY: list members are live `Empty` extents.
        let pages = g.desc(d).pages();
        g.unlink(d);
        let ghost d1 = g.descs@;
        g.desc_mut(d).state = RunState::Purged as u8;
        g.desc_mut(d).dirty_at = 0;
        proof {
            // `d` is on the list, so the list's count and pages hold it.
            lemma_epages_remove(l0, d0, j);
            lemma_epages_nonneg(l0.remove(j), d0);
            assert(g.stats == old(g).stats);
        }
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
            assert forall|q: nat| #[trigger] g.free.map.view().contains(q) implies exists|x: *mut HugeDesc| #[trigger] in_free(
                d2,
                x,
                q as int,
            ) by {
                let x = choose|x: *mut HugeDesc| #[trigger] in_free(d0, x, q as int);
                assert(same_but_links(d0[x].value(), d1[x].value()));
                assert(in_free(d2, x, q as int));
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
        proof {
            assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) implies g.descs@[x].value().pages
                == d0[x].value().pages && g.descs@[x].value().start == d0[x].value().start && (x != d
                ==> g.descs@[x].value().state == d0[x].value().state) by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
        }
        pages
    }

    /// The flush's report on `[addr, addr + len)`, extents pushed by
    /// [`push_extent`] (under the huge lock since, so still in the `Empty`
    /// list): discarded, each leaves the list and becomes `Purged`; not,
    /// they stay as they are. Free extents never touch, so a range is one
    /// extent: the check that it starts an `Empty` extent never fails.
    ///
    /// [`push_extent`]: Huge::push_extent
    #[allow(clippy::too_many_arguments)]
    fn settle(
        &self,
        g: &mut Inner,
        addr: usize,
        len: usize,
        done: bool,
        t: &mut Tally,
        Ghost(piece): Ghost<int>,
        Ghost(pend): Ghost<spec_fn((int, int)) -> bool>,
    )
        requires
            crate::sys::may_abort(),
            old(t).inv(self, *old(g), pend),
            0 < piece <= len,
            len as int % piece == 0,
            addr + len <= usize::MAX,
            forall|x: (int, int)| #[trigger] purge::in_run(addr as int, len as int, piece, x) ==> pend(x),
        ensures
            final(t).inv(self, *final(g), purge::minus(pend, addr as int, len as int, piece)),
            final(t).conf() == old(t).conf(),
            final(t).bytes() == old(t).bytes() + if done {
                len as int
            } else {
                0
            },
    {
        let ghost g0 = *g;
        let ghost t0 = *t;
        let ghost p = self.spec_page();
        let ghost base = self.base as int;
        proof {
            use_type_invariant(self);
            self.lemma_page();
            reveal(has_piece);
            vstd::arithmetic::div_mod::lemma_small_mod(0, piece as nat);
            assert(purge::in_run(addr as int, len as int, piece, (addr as int, piece)));
            assert(pend((addr as int, piece)));
        }
        // The extent pushed for the range's first piece.
        let ghost dd = choose|y: *mut HugeDesc|
            #[trigger] g0.descs@.contains_key(y) && g0.descs@[y].value().state == RunState::Empty as u8
                && t0.vis@.contains(y) && piece_of(self, g0.descs@[y].value()) == (addr as int, piece);
        let ghost v = g0.descs@[dd].value();
        proof {
            assert(desc_ok(v, g0.next_page));
            // One piece: a second would be another `Empty` extent starting
            // where this one ends, and free extents never touch.
            if piece < len {
                vstd::arithmetic::div_mod::lemma_mod_self_0(piece);
                assert(purge::in_run(addr as int, len as int, piece, (addr + piece, piece)));
                assert(pend((addr + piece, piece)));
                let y = choose|y: *mut HugeDesc|
                    #[trigger] g0.descs@.contains_key(y) && g0.descs@[y].value().state == RunState::Empty as u8
                        && t0.vis@.contains(y) && piece_of(self, g0.descs@[y].value()) == (addr + piece, piece);
                let w = g0.descs@[y].value();
                assert(w.start * p == v.start * p + v.pages * p);
                assert(w.start == v.start + v.pages) by (nonlinear_arith)
                    requires w.start * p == v.start * p + v.pages * p, p > 0;
                g0.theorem_coalesced(dd, y);
            }
            assert(len == piece);
            assert(g0.elist@.contains(dd));
            assert(keyed(g0.table, v, dd));
        }
        if !done {
            t.failed = true;
            proof {
                t.refused@ = t.refused@.insert(dd);
                let rest = purge::minus(pend, addr as int, len as int, piece);
                assert forall|x: (int, int)| #[trigger] rest(x) implies has_piece(self, *g, t.vis@, x) by {
                    assert(pend(x));
                }
                if t.full@ {
                    let l = g.elist@;
                    assert forall|i: int| 0 <= i < l.len() - t.rest@.len() implies #[trigger] rest(
                        piece_of(self, g.descs@[l[i]].value()),
                    ) || t.refused@.contains(l[i]) by {
                        assert(pend(piece_of(self, g.descs@[l[i]].value())) || t0.refused@.contains(l[i]));
                        if l[i] != dd && purge::in_run(addr as int, len as int, piece, piece_of(self, g.descs@[l[i]].value())) {
                            purge::lemma_run_one(addr as int, len as int, piece_of(self, g.descs@[l[i]].value()));
                            assert(enode(l, g.descs@, i));
                            let w = g.descs@[l[i]].value();
                            assert(w.start * p == v.start * p);
                            assert(w.start == v.start) by (nonlinear_arith)
                                requires w.start * p == v.start * p, p > 0;
                            assert(apart(v, w));
                        }
                    }
                }
                assert(t.inv(self, *g, rest));
            }
            return;
        }
        let mut page = (addr - self.base) >> self.lg_page;
        proof {
            vstd::bits::lemma_usize_shr_is_div((addr - self.base) as usize, self.lg_page as usize);
            vstd::bits::lemma_usize_shr_is_div(len, self.lg_page as usize);
            vstd::arithmetic::div_mod::lemma_div_multiples_vanish(v.start as int, p);
            vstd::arithmetic::div_mod::lemma_div_multiples_vanish(v.pages as int, p);
            assert(p * v.start == v.start * p && p * v.pages == v.pages * p) by (nonlinear_arith);
            assert(page == v.start);
            assert(len >> self.lg_page == v.pages);
            assert(v.start + v.pages <= g0.next_page <= u32::MAX);
        }
        let end = page + (len >> self.lg_page);
        let ghost mut once = false;
        while page < end
            invariant
                crate::sys::may_abort(),
                end == v.start + v.pages,
                v == g0.descs@[dd].value(),
                v.pages >= 1,
                g0.wf(),
                g0.descs@.contains_key(dd),
                g0.elist@.contains(dd),
                keyed(g0.table, v, dd),
                v.state == RunState::Empty as u8,
                t0.purged + g0.elist@.len() == t0.e0@,
                t0.e0@ <= u32::MAX,
                !once ==> page == v.start && *g == g0 && *t == t0,
                once ==> page == end,
                once ==> {
                    &&& g.wf()
                    &&& g.spec_np() == g0.spec_np()
                    &&& g.spec_lg() == g0.spec_lg()
                    &&& g.pgs == g0.pgs
                    &&& g.elist@ == g0.elist@.remove(g0.elist@.index_of(dd))
                    &&& g.descs@.dom() == g0.descs@.dom()
                    &&& forall|x: *mut HugeDesc| #[trigger] g0.descs@.contains_key(x) ==> g.descs@[x].value().pages
                        == g0.descs@[x].value().pages && g.descs@[x].value().start == g0.descs@[x].value().start
                        && (x != dd ==> g.descs@[x].value().state == g0.descs@[x].value().state)
                    &&& g.descs@[dd].value().state == RunState::Purged as u8
                    &&& t.purged == t0.purged + 1
                    &&& t.conf() == t0.conf()
                    &&& t.bytes == t0.bytes
                    &&& t.refused == t0.refused
                },
            decreases end - page,
        {
            let d = g.table.get(page);
            // SAFETY: table entries are live descriptors.
            if d.is_null() || g.desc(d).start() != page || g.desc(d).state() != RunState::Empty {
                proof {
                    assert(false);
                }
                sys::abort_fmt(format_args!("huge: purged range {addr:#x}+{len:#x} is not Empty extents"));
            }
            proof {
                assert(d == dd);
                assert(t.purged + g.elist@.len() == t.e0@);
                g.lemma_elist();
            }
            let pages = self.purge_desc(g, d);
            t.purged += 1;
            page += pages;
            proof {
                once = true;
            }
        }
        t.bytes = Ghost(t.bytes@ + len);
        proof {
            assert(once);
            let l0 = g0.elist@;
            let i0 = l0.index_of(dd);
            lemma_walked_remove(l0, t0.rest@, t0.vis@, i0);
            let rest = purge::minus(pend, addr as int, len as int, piece);
            assert forall|x: (int, int)| #[trigger] rest(x) implies has_piece(self, *g, t.vis@, x) by {
                assert(pend(x));
                let y = choose|y: *mut HugeDesc|
                    #[trigger] g0.descs@.contains_key(y) && g0.descs@[y].value().state == RunState::Empty as u8
                        && t0.vis@.contains(y) && piece_of(self, g0.descs@[y].value()) == x;
                purge::lemma_run_one(addr as int, len as int, x);
                assert(y != dd);
                assert(g.descs@.contains_key(y));
            }
            if t.full@ {
                let l = g.elist@;
                assert forall|i: int| 0 <= i < l.len() - t.rest@.len() implies #[trigger] rest(
                    piece_of(self, g.descs@[l[i]].value()),
                ) || t.refused@.contains(l[i]) by {
                    let i1 = if i < i0 { i } else { i + 1 };
                    assert(l[i] == l0[i1]);
                    assert(pend(piece_of(self, g0.descs@[l0[i1]].value())) || t0.refused@.contains(l0[i1]));
                    assert(l0.no_duplicates());
                    assert(l[i] != dd);
                    assert(enode(l0, g0.descs@, i1));
                    let w = g0.descs@[l[i]].value();
                    purge::lemma_run_one(addr as int, len as int, piece_of(self, w));
                    if piece_of(self, w) == (addr as int, len as int) {
                        assert(w.start * p == v.start * p);
                        assert(w.start == v.start) by (nonlinear_arith)
                            requires w.start * p == v.start * p, p > 0;
                        assert(apart(v, w));
                    }
                }
            }
            assert(t.inv(self, *g, rest));
        }
    }

    /// Decay walk (design section 8), as `ClassState::purge_curve` in
    /// pages: the curve's allowance at `now` is the sum over the `Empty`
    /// extents of their pages times the weight of their age, rounded up,
    /// and the excess over it is purged oldest extent first, whole extents
    /// only and never below the allowance (an extent larger than the
    /// remaining excess is skipped for younger ones that fit), so a lone
    /// extent lasts until its weight is zero, the end of the window, as
    /// before. A failed discard leaves its extent `Empty` and backs the
    /// gate off (`Decayer::back_off`). Returns the number of extents
    /// purged.
    fn purge_curve(&self, now: u64, curve: &Curve, keep16: u32, Tracked(ord): Tracked<&mut LockOrder>) -> usize
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            self.is_process(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
    {
        broadcast use axiom_huge_lock_inv, axiom_huge_lock_frame;

        proof {
            use_type_invariant(self);
        }
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
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
        // As in `purge_empty`: pages of no live run.
        let ghost held = g@.pgs@@;
        let mut t = Tally::new(Ghost(held), Ghost(g@.elist@), Ghost(false), Ghost(g@.elist@.len() as int));
        if excess > 0 {
            let mut batch = Batch::new(Ghost(|b: int| self.purgeable(held, b)));
            let mut d = g.empty_head;
            proof {
                reveal(has_piece);
                g@.lemma_head();
                g@.lemma_elist();
                assert(g@.elist@.subrange(0, g@.elist@.len() as int) =~= g@.elist@);
                assert(t.inv(self, g@, batch.pieces()));
            }
            while !d.is_null() && excess > 0
                invariant
                    crate::sys::may_abort(),
                    crate::sys::may_syscall(),
                    self.is_process(),
                    batch.wf(),
                    forall|b: int| #[trigger] (batch.free())(b) == self.purgeable(held, b),
                    t.inv(self, g@, batch.pieces()),
                    t.held@ == held,
                    !t.full@,
                    d == (if t.rest@.len() > 0 { t.rest@[0] } else { ptr::null_mut::<HugeDesc>() }),
                decreases t.rest@.len(),
            {
                proof {
                    if t.rest@.len() == 0 {
                        assert(d@.addr == 0);
                    }
                    t.lemma_rest_live(self, g@, batch.pieces());
                }
                // SAFETY: as above; `next` is read before a flush can
                // unlink `d`.
                let (pages, next) = (g.desc(d).pages(), g.desc(d).next);
                if pages <= excess || capped {
                    let pushed = self.push_extent(&mut g, d, &mut batch, &mut t);
                    excess = excess.saturating_sub(pushed);
                } else {
                    proof {
                        reveal(has_piece);
                        lemma_walked_visit(g@.elist@, t.rest@, t.vis@);
                        let pend = batch.pieces();
                        let vis0 = t.vis@;
                        t.vis@ = t.vis@.insert(d);
                        t.rest@ = t.rest@.drop_first();
                        assert forall|x: (int, int)| #[trigger] pend(x) implies has_piece(self, g@, t.vis@, x) by {
                            assert(has_piece(self, g@, vis0, x));
                            let y = choose|y: *mut HugeDesc|
                                #[trigger] g@.descs@.contains_key(y) && g@.descs@[y].value().state == RunState::Empty as u8
                                    && vis0.contains(y) && piece_of(self, g@.descs@[y].value()) == x;
                            assert(t.vis@.contains(y));
                        }
                        assert(t.inv(self, g@, pend));
                    }
                }
                proof {
                    t.lemma_rest_live(self, g@, batch.pieces());
                }
                d = next;
            }
            batch.flush(&mut t, self, &mut g);
        }
        if t.failed {
            self.gate.back_off(now);
        }
        self.sync_oldest(&g);
        t.purged
    }

    /// [`decay_with`](Huge::decay_with) keeping everything the curve
    /// allows.
    #[cfg(test)]
    pub fn decay(&self, now: u64, decay_ns: u64, Tracked(ord): Tracked<&mut LockOrder>) -> Option<u64>
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            self.is_process(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
    {
        self.decay_with(now, decay_ns, crate::policy::KEEP_ALL, Tracked(ord))
    }

    /// Decay step (design section 8), as `ClassState::decay_with`: the
    /// gate runs `purge_curve` over the list at most once per step of the
    /// curve, with the cgroup policy's keep ratio (design 11), and returns
    /// the next deadline, `None` when nothing is `Empty`. Lock-free unless
    /// a walk is due.
    pub fn decay_with(&self, now: u64, decay_ns: u64, keep16: u32, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<u64>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            self.is_process(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            r matches Some(t) ==> t >= now,
            exists|oldest: u64, retry: u64, next: u64, after: u64, retry2: u64|
                r == #[trigger] crate::purge::Decayer::spec_decay(now, decay_ns, oldest, retry, next, after, retry2),
    {
        self.gate.decay(now, decay_ns, |now, curve| {
            // Premise (lock order): the closure cannot carry the caller's
            // token; `ord`, which `decay_with` requires, bounds the same
            // locks (this thread's, unchanged since the call).
            let tracked mut ord = axiom_below_huge();
            self.purge_curve(now, curve, keep16, Tracked(&mut ord));
        })
    }

    /// `dirty_at` of the oldest `Empty` extent as the decay driver sees it
    /// (0 when none).
    #[cfg(test)]
    pub fn oldest_empty(&self) -> u64 {
        self.gate.oldest_empty()
    }

    /// Bytes in `Empty` extents (one lock hold), for the cgroup policy's
    /// retention figure: at most the region.
    pub fn empty_bytes(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: usize)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            r <= crate::meta::va_limit(),
    {
        broadcast use {axiom_huge_lock_inv, axiom_huge_lock_frame};

        proof {
            use_type_invariant(self);
        }
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        proof {
            assert(g@.stats.empty_pages <= g@.next_page <= self.npages);
            self.lemma_pages(g@.stats.empty_pages as usize);
        }
        g.stats.empty_pages << self.lg_page
    }

    /// The extent counters, from one lock hold. The free extents' pages
    /// are among the region's, at most `u32::MAX`.
    pub fn stats(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: HugeStats)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_huge()),
        ensures
            r.empty_pages + r.purged_pages <= u32::MAX,
            *final(ord) == *old(ord),
    {
        broadcast use {axiom_huge_lock_inv, axiom_huge_lock_frame};

        proof {
            use_type_invariant(self);
        }
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        proof {
            g@.table.lemma_wf_storage();
        }
        HugeStats { table_cap: g.table.cap(), ..g.stats }
    }
}

} // verus!

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

/// Initialise a metadata block as a free extent.
fn init_free(d: *mut HugeDesc, start: usize, pages: usize, state: RunState, dirty_at: u64) {
    debug_assert!(state.is_free());
    // SAFETY: `d` is a fresh metadata block for a `HugeDesc`.
    unsafe {
        ptr::write(
            d,
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
        );
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

} // verus!

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
    panic!(
        "check_zero: huge run {run:#x} ({pages} pages from page {page}, arena {arena}) was handed out as zero from {prior} pages but the word at {at:#x} reads {word:#x} (purge advice {advice})"
    )
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod linux_tests;
