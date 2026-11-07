//! Slab pages (docs/proposals/slab-pages.md, 3.4): the walk that closes
//! the pages of a class's partial slabs that hold no live or cached slot,
//! and gives them back to the kernel.
//!
//! The walk runs under the class lock. It visits up to [`WALK_SLABS`]
//! partial slabs from a cursor, finds each slab's closable pages (every
//! slot overlapping the page clear, read with plain loads: a hint), and
//! stamps the slab when it first has one (`SlabPages::stamp`, cleared when
//! it has none). The decay curve then picks how many of the closable pages
//! close, oldest stamp first, as it picks `Empty` slabs to purge
//! (`purge_curve`); an explicit purge closes every one. Closing a page
//! ([`ClassState::close_page`]) is the proposal's four steps: the mask bit,
//! a read-modify-write of each word holding a slot of the page, the
//! discard, and the closed-slot count. A page closes only while `live +
//! closed <= nslots` holds after it, the bound `take`'s reservation rests
//! on: a reserved, unclaimed slot is clear too.

#[cfg(verus_keep_ghost)]
use super::decay::walk_ok;
#[cfg(verus_keep_ghost)]
use super::table::desc_at;
use super::{ClassState, Inner};
use crate::bitmap::AtomicBitmap;
use crate::layout::pages::OpenView;
use crate::lock::LockOrder;
#[cfg(verus_keep_ghost)]
use crate::lock::rank_class;
use crate::purge::{Curve, STEPS};
use crate::slab::SlabPages;
#[cfg(verus_keep_ghost)]
use crate::slab::{desc_id, pages_id};
use core::sync::atomic::Ordering;
use vstd::prelude::*;

verus! {

/// Partial slabs one walk visits: a bounded number per step, a few
/// thousand word loads (docs/proposals/slab-pages.md, 3.4).
pub const WALK_SLABS: usize = 64;

/// The shortest interval between two defragmenter hints that arm a class's
/// walk (docs/proposals/slab-pages.md, 7.1): 100 ms.
pub const HINT_NS: u64 = 100_000_000;

/// The bytes the discard of page `[a, a + len)` frees: the page's, while
/// bit `p` of the closed mask the lock holder wrote is set (as
/// `decay::idle_byte` names an `Empty` slab's).
pub open spec fn closed_page(closed: u16, p: u16, a: int, len: int) -> spec_fn(int) -> bool {
    |b: int| (closed >> p) & 1u16 == 1u16 && a <= b < a + len
}

/// A partial slab the walk found closable pages on, and the curve step
/// of its stamp.
/// Packed: the walk's frame stays within design 13.8's budget.
#[derive(Clone, Copy)]
struct Found {
    slab: u32,
    pages: u16,
    step: u8,
}

/// The cut-off of the curve's choice: every closable page in a step older
/// than `cut` closes, and all but the first `keep` of step `cut`, which
/// together are at least `need` (`decay::cut_step`'s rule over the
/// closable pages' counts).
fn cut_of(counts: &[u16; STEPS], need: usize) -> (r: (usize, usize))
    ensures
        r.0 < STEPS,
{
    let mut need = need;
    let mut s = STEPS;
    while s > 0
        invariant
            s <= STEPS,
        decreases s,
    {
        s -= 1;
        let c = counts[s] as usize;
        if c >= need {
            return (s, c - need);
        }
        need -= c;
    }
    (0, 0)
}

/// The pages a mask names.
pub(super) fn page_count(m: u16) -> (r: usize)
    ensures
        r <= 16,
{
    let mut n: usize = 0;
    let mut p: u16 = 0;
    while p < 16
        invariant
            p <= 16,
            n <= p,
        decreases 16 - p,
    {
        if (m >> p) & 1 == 1 {
            n += 1;
        }
        p += 1;
    }
    n
}

/// A change of the lock value that leaves every field the invariant reads
/// alone but the descriptor permissions, whose agreement it keeps
/// (`descs`), and the slab-pages fields, which it does not read.
pub(super) proof fn lemma_walk_frame(cs: &ClassState, g0: Inner, g1: Inner)
    requires
        walk_ok(cs, g0),
        g1.descs(),
        g1.partial == g0.partial,
        g1.empty == g0.empty,
        g1.purged == g0.purged,
        g1.stats == g0.stats,
        g1.group_next == g0.group_next,
        g1.group_end == g0.group_end,
        g1.states == g0.states,
        g1.committed == g0.committed,
        g1.dirty == g0.dirty,
        g1.arena == g0.arena,
        g1.table == g0.table,
        g1.inited == g0.inited,
        g1.seen == g0.seen,
        g1.oauth == g0.oauth,
        g1.zeros == g0.zeros,
    ensures
        walk_ok(cs, g1),
{
    assert forall|x: nat| #[trigger] g1.slab_ok(x) by {
        assert(g0.slab_ok(x));
    }
}

impl ClassState {
    /// Write slab `slab`'s closed pages and stamp under this arena's class
    /// lock (`g`), through the slab's descriptor permission `g` holds, as
    /// `set_dirty_locked` writes the stamp.
    // `g` and `slab` are read only by the specification (ghost), but for
    // the descriptor permission they hold.
    #[allow(unused_variables)]
    pub(super) fn set_pages_locked(&self, g: &mut Inner, pg: &SlabPages, slab: usize, closed: u16, stamp: u64)
        requires
            walk_ok(self, *old(g)),
            old(g).owns(slab as nat),
            pages_id(pg) == desc_id(&desc_at(self.spec_table(), slab)) + 16,
        ensures
            walk_ok(self, *final(g)),
            final(g).states == old(g).states,
            final(g).partial == old(g).partial,
            final(g).dperms@.contains_key(slab as nat),
            final(g).dperms@[slab as nat].closed() == closed,
            final(g).closed_slots == old(g).closed_slots,
            final(g).pages_cursor == old(g).pages_cursor,
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
            final(g).seen == old(g).seen,
            final(g).oauth == old(g).oauth,
            final(g).zeros == old(g).zeros,
    {
        let ghost g0 = *g;
        let ghost k = slab as nat;
        proof {
            g.lemma_desc(k);
        }
        let tracked mut p = g.dperms.borrow_mut().tracked_remove(k);
        pg.set_pages(closed, stamp, Tracked(&mut p));
        proof {
            g.dperms.borrow_mut().tracked_insert(k, p);
            super::lemma_descs_update(g0, *g, k);
            lemma_walk_frame(self, g0, *g);
        }
    }

    /// [`set_pages_locked`](ClassState::set_pages_locked) for a lock value
    /// whose invariant is broken at `slab` (`take`, between the hand-out of
    /// a `Fresh` slab and its claim): only the descriptor permissions'
    /// agreement is kept, as `set_dirty_locked` keeps it.
    // `g` and `slab` are read only by the specification (ghost), but for
    // the descriptor permission they hold.
    #[allow(unused_variables)]
    pub(super) fn set_pages_raw(&self, g: &mut Inner, pg: &SlabPages, slab: usize, closed: u16, stamp: u64)
        requires
            old(g).descs(),
            old(g).table@ == self.spec_table(),
            old(g).owns(slab as nat),
            pages_id(pg) == desc_id(&desc_at(self.spec_table(), slab)) + 16,
        ensures
            final(g).descs(),
            final(g).states == old(g).states,
            final(g).dirty == old(g).dirty,
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
            final(g).seen == old(g).seen,
            final(g).oauth == old(g).oauth,
            final(g).zeros == old(g).zeros,
            final(g).closed_slots == old(g).closed_slots,
            final(g).closed_pages == old(g).closed_pages,
            final(g).pages_cursor == old(g).pages_cursor,
    {
        let ghost g0 = *g;
        let ghost k = slab as nat;
        proof {
            g.lemma_desc(k);
        }
        let tracked mut p = g.dperms.borrow_mut().tracked_remove(k);
        pg.set_pages(closed, stamp, Tracked(&mut p));
        proof {
            g.dperms.borrow_mut().tracked_insert(k, p);
            super::lemma_descs_update(g0, *g, k);
        }
    }

    /// Opening on demand (docs/proposals/slab-pages.md, 3.3): the closed
    /// mask of a slab whose first `n` slots are its only claimed ones: the
    /// pages past the one slot `n - 1` ends in. Every slot below `n` stays
    /// open; a slot straddling into the first closed page is closed.
    pub(super) fn first_claim_mask(v: OpenView, n: usize) -> u16
        requires
            v.paged(),
    {
        if n == 0 || n >= v.nslots {
            return 0;
        }
        proof {
            assert(1 <= n * v.size <= 0x10000 * 0x10000) by (nonlinear_arith)
                requires 1 <= n <= 0x10000, 1 <= v.size <= 0x10000;
        }
        let last = (n * v.size - 1) / v.pg;
        if last + 1 >= v.pages {
            return 0;
        }
        let (np, nl) = (v.pages as u32, (last + 1) as u32);
        proof {
            assert((1u32 << np) >= 1u32 && (1u32 << np) - 1u32 <= 0xffffu32) by (bit_vector)
                requires np <= 16;
            assert((1u32 << nl) >= 1u32 && (1u32 << nl) - 1u32 <= 0xffffu32) by (bit_vector)
                requires nl <= 16;
        }
        let all = ((1u32 << np) - 1) as u16;
        let open = ((1u32 << nl) - 1) as u16;
        all & !open
    }

    /// Slab `slab`'s closed pages and stamp, read under this arena's class
    /// lock (`g`) through the permission `g` holds.
    // `g` and `slab` are read only by the specification (ghost).
    #[allow(unused_variables)]
    pub(super) fn pages_locked(g: &Inner, pg: &SlabPages, slab: usize) -> (u16, u64) {
        pg.pages_held(Ghost(slab as nat), Tracked(g.dperms.borrow()))
    }

    /// The open pages of view `v` (a slab's geometry and closed mask) whose
    /// every slot is clear in `bm`, the slab's bits: plain loads, so only
    /// a hint, which [`close_page`](ClassState::close_page)'s
    /// read-modify-writes check.
    fn closable(bm: &AtomicBitmap, v: OpenView) -> u16
        requires
            v.paged(),
    {
        let words = bm.words();
        let mut out = 0u16;
        let mut p: usize = 0;
        while p < v.pages
            invariant
                v.paged(),
                p <= v.pages,
                words == bm.spec_words(),
            decreases v.pages - p,
        {
            if (v.closed >> (p as u16)) & 1 == 0 {
                let (first, end) = v.page_slots_of(p);
                let mut free = first < end;
                let mut w = first / 64;
                while free && w < words && w < 1024 && w * 64 < end
                    invariant
                        v.paged(),
                        p < v.pages,
                        words == bm.spec_words(),
                    decreases words - w,
                {
                    if bm.load(w) & v.page_word(p, w) != 0 {
                        free = false;
                    }
                    w += 1;
                }
                if free {
                    out |= 1u16 << (p as u16);
                }
            }
            p += 1;
        }
        out
    }

    /// Close page `p` of slab `slab` (docs/proposals/slab-pages.md, 3.4),
    /// whose view `v` is its geometry and closed mask and `stamp` its
    /// stamp, under this arena's class lock (`g`). Returns whether it
    /// closed: it does not when the page's slots would leave a claim's
    /// reservation short, when a claim got one of them first, or when the
    /// kernel refused the discard; the page is open again then. Returns the
    /// slots it hid (0 when it did not close it).
    ///
    /// 1. The mask bit (`SlabPages::set_pages`, `Release`): a claim that
    ///    reads the mask from now on takes nothing on the page.
    /// 2. `fetch_or(0)` (`AcqRel`) of each word holding a slot of the
    ///    page (`AtomicBitmap::probe`): a claim whose `fetch_or` came
    ///    first left its bit set, and the page reopens; one that comes
    ///    after reads from the walk's and sees step 1 at its re-check.
    /// 3. The discard (`purge::discard_one`, the band's advice).
    /// 4. `closed_slots` up by the slots that were open and are closed
    ///    now: a straddling slot whose other page is closed already does
    ///    not count twice.
    fn close_page(&self, g: &mut Inner, slab: usize, pg: &SlabPages, v: OpenView, p: usize, stamp: u64) -> (r: usize)
        requires
            self.is_process(),
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            walk_ok(self, *old(g)),
            old(g).owns(slab as nat),
            slab < self.spec_nslabs(),
            !self.spec_table().spec_is_guard(slab),
            pages_id(pg) == desc_id(&desc_at(self.spec_table(), slab)) + 16,
            v.paged(),
            p < v.pages,
        ensures
            walk_ok(self, *final(g)),
            final(g).states == old(g).states,
            final(g).partial == old(g).partial,
    {
        let t = self.table();
        let bm = t.bits(slab);
        let live = self.desc(slab).live();
        let new = v.closed | (1u16 << (p as u16));
        let n_old = v.closed_slots();
        let n_new = v.with_closed(new).closed_slots();
        // The reservation bound `take` rests on: `live + closed <= nslots`.
        if live > v.nslots - n_new {
            return 0;
        }
        // Step 1.
        self.set_pages_locked(g, pg, slab, new, stamp);
        // Step 2.
        let (first, end) = v.page_slots_of(p);
        let words = bm.words();
        let mut w = first / 64;
        let mut busy = false;
        while !busy && w < words && w < 1024 && w * 64 < end
            invariant
                v.paged(),
                p < v.pages,
                words == bm.spec_words(),
            decreases words - w,
        {
            if bm.probe(w) & v.page_word(p, w) != 0 {
                busy = true;
            }
            w += 1;
        }
        if busy {
            self.set_pages_locked(g, pg, slab, v.closed, stamp);
            return 0;
        }
        // Step 3.
        let Some((a, len)) = t.page_range(slab, p) else {
            self.set_pages_locked(g, pg, slab, v.closed, stamp);
            return 0;
        };
        proof {
            let (c, pu) = (v.closed, p as u16);
            assert(((c | (1u16 << pu)) >> pu) & 1u16 == 1u16) by (bit_vector)
                requires pu < 16;
            assert(crate::purge::frees(closed_page(new, pu, a as int, len as int), a as int, len as int));
        }
        if !crate::purge::discard_one(a, len, Ghost(closed_page(new, p as u16, a as int, len as int))) {
            // Refused (`EINVAL`, `mlock`ed pages): the page reopens and keeps
            // its age, as a refused `Empty` slab stays `Empty`.
            self.set_pages_locked(g, pg, slab, v.closed, stamp);
            return 0;
        }
        // Step 4.
        let ghost g0 = *g;
        let hid = n_new.saturating_sub(n_old);
        g.closed_slots = g.closed_slots.saturating_add(hid);
        g.closed_pages = g.closed_pages.saturating_add(1);
        proof {
            lemma_walk_frame(self, g0, *g);
        }
        hid
    }

    /// Open the lowest closed pages of slab `slab` until its open free
    /// slots cover `need`, or every page is open (a refill's step 2,
    /// docs/proposals/slab-pages.md, 3.3), under this arena's class lock
    /// (`g`); `closed_slots` goes down by the slots that open. The bits
    /// need no change: a closed page's are clear. The pages were
    /// discarded, so the claim touches them again; the `Release` store of
    /// the mask (`SlabPages::set_pages`) orders the claim after it.
    pub(super) fn reopen_pages(&self, g: &mut Inner, slab: usize, need: usize)
        requires
            crate::sys::may_abort_pending(),
            walk_ok(self, *old(g)),
            old(g).owns(slab as nat),
        ensures
            walk_ok(self, *final(g)),
            final(g).states == old(g).states,
            final(g).partial == old(g).partial,
            final(g).empty == old(g).empty,
            final(g).purged == old(g).purged,
            final(g).committed == old(g).committed,
            final(g).group_next == old(g).group_next,
            final(g).group_end == old(g).group_end,
    {
        let t = self.table();
        let v0 = t.page_view();
        if !v0.is_paged() {
            return;
        }
        let Some(pg) = t.pages(slab) else {
            return;
        };
        let (closed, stamp) = Self::pages_locked(&*g, pg, slab);
        if closed == 0 {
            return;
        }
        let live = self.desc(slab).live();
        let mut c = closed;
        while c != 0 && v0.nslots - v0.with_closed(c).closed_slots() < live.saturating_add(need)
            invariant
                v0.paged(),
            decreases c,
        {
            // The lowest closed page opens: `c & (c - 1)` clears the lowest
            // set bit.
            proof {
                assert(c & sub(c, 1) < c) by (bit_vector)
                    requires c != 0u16;
            }
            c &= c - 1;
        }
        let n_old = v0.with_closed(closed).closed_slots();
        let n_new = v0.with_closed(c).closed_slots();
        let ghost g0 = *g;
        g.closed_slots = g.closed_slots.saturating_sub(n_old.saturating_sub(n_new));
        g.closed_pages = g.closed_pages.saturating_sub(page_count(closed & !c));
        proof {
            lemma_walk_frame(self, g0, *g);
        }
        self.set_pages_locked(g, pg, slab, c, stamp);
    }

    /// A defragmenter asked about a slot of this class
    /// (`get_defrag_hint`): at most once per [`HINT_NS`], arm the next
    /// slab-pages walk to run at once and close every page closable for a
    /// curve step, and wake the background thread (docs/proposals/
    /// slab-pages.md, 7.1). Lock-free: the hint runs once per pointer a
    /// defragmenter scans.
    pub fn note_defrag_hint(&self)
        requires
            crate::sys::may_syscall(),
    {
        let now = crate::sys::now_coarse_ns();
        if now >= self.hint_due.load(Ordering::Relaxed) {
            self.hint_due.store(now.saturating_add(HINT_NS), Ordering::Relaxed);
            self.hint_armed.store(1, Ordering::Relaxed);
            self.pages_due.store(0, Ordering::Relaxed);
            crate::purge::note_pages();
        }
    }

    /// The slab-pages walk (docs/proposals/slab-pages.md, 3.4 and 7.1),
    /// once its gate (`pages_due`) is due at `now`: the closable pages of
    /// up to [`WALK_SLABS`] partial slabs from the cursor, closed along the
    /// decay curve of period `decay_ns` (oldest stamp first; within the
    /// cut-off step the lowest pages stay), or, for an explicit purge
    /// (`all`), every closable page of every partial slab at once. Returns
    /// the next deadline while closable pages are left (a curve step on),
    /// none otherwise; the walk's own gate stays a step ahead while the
    /// class has partial slabs.
    pub fn close_pages(&self, now: u64, decay_ns: u64, all: bool, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<u64>)
        requires
            self.is_process(),
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let t = self.table();
        let v0 = t.page_view();
        if !v0.is_paged() {
            return None;
        }
        let due = self.pages_due.load(Ordering::Relaxed);
        if !all && now < due {
            return Some(due);
        }
        let curve = Curve::new(decay_ns);
        // A defragmenter's hint: every page closable for a curve step (its
        // stamp's step at least 1) closes, beside the curve's choice.
        let hinted = self.hint_armed.swap(0, Ordering::Relaxed) != 0;
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        let mut found = [Found { slab: 0, pages: 0, step: 0 }; WALK_SLABS];
        let mut nf: usize = 0;
        let mut counts = [0u16; STEPS];
        let mut total: usize = 0;
        let mut weighted: u64 = 0;
        let mut visited: usize = 0;
        let mut found_slots: usize = 0;
        let mut closed_now: usize = 0;
        let limit = if all { usize::MAX } else { WALK_SLABS };
        let start = if all { 0 } else { g.pages_cursor };
        proof {
            broadcast use super::table::lemma_spec_nslabs;
            assert(self.spec_nslabs() <= 0x400_0000);
        }
        let mut next = g.partial.map.lowest_set_from(start);
        if next.is_none() && start != 0 {
            next = g.partial.map.lowest_set();
        }
        // Pass 1: each slab's closable pages, its stamp, and the curve's
        // histogram of them (an explicit purge closes them at once).
        while let Some(s) = next
            invariant
                crate::sys::may_abort() || crate::sys::valid_aborts(),
                crate::sys::may_syscall(),
                self.is_process(),
                walk_ok(self, g@),
                g.spec_lock() == self.inner.0,
                *t == self.spec_table(),
                v0.paged(),
                v0.closed == 0,
                v0.nslots == crate::layout::class_table()[self.spec_class() as int].nslots,
                nf <= visited,
                nf <= WALK_SLABS,
                self.spec_nslabs() <= 0x400_0000,
                limit == if all { usize::MAX } else { WALK_SLABS },
                !all ==> visited <= WALK_SLABS,
                total <= nf * 16,
                weighted <= total * 0xffff_ffff,
                forall|k: int| 0 <= k < STEPS ==> #[trigger] counts[k] <= total,
                forall|j: int| 0 <= j < nf ==> #[trigger] g@.partial.map@.contains(found[j].slab as nat)
                    && found[j].slab < self.spec_nslabs() && found[j].step < STEPS,
                next matches Some(x) ==> x < self.spec_nslabs() && g@.partial.map@.contains(x as nat),
            decreases
                match next {
                    Some(x) => self.spec_nslabs() - x,
                    None => 0,
                },
        {
            if visited >= limit {
                break;
            }
            let ghost gp = g@.partial;
            let ghost fh = found;
            let ghost nfh = nf;
            visited += 1;
            proof {
                assert(g@.slab_ok(s as nat));
            }
            if let Some(pg) = t.pages(s) {
                proof {
                    assert(pages_id(pg) == desc_id(&desc_at(self.spec_table(), s)) + 16);
                }
                let (closed, stamp) = Self::pages_locked(&g, pg, s);
                let v = v0.with_closed(closed);
                let cand = Self::closable(&t.bits(s), v);
                // The slots those pages hold: free pages, not fragmentation
                // (`ClassStats::closable_slots`).
                if cand != 0 {
                    let k = v0.with_closed(closed | cand).closed_slots().saturating_sub(v.closed_slots());
                    found_slots = found_slots.saturating_add(k);
                }
                let st = if cand == 0 { 0 } else if stamp == 0 { if now == 0 { 1 } else { now } } else { stamp };
                if st != stamp {
                    self.set_pages_locked(&mut g, pg, s, closed, st);
                }
                if cand != 0 {
                    if all {
                        let ghost part = g@.partial;
                        let mut p: usize = 0;
                        while p < v0.pages
                            invariant
                                crate::sys::may_abort() || crate::sys::valid_aborts(),
                                crate::sys::may_syscall(),
                                self.is_process(),
                                walk_ok(self, g@),
                                g.spec_lock() == self.inner.0,
                                *t == self.spec_table(),
                                nf <= WALK_SLABS,
                                v0.paged(),
                                s < self.spec_nslabs(),
                                g@.partial.map@.contains(s as nat),
                                !self.spec_table().spec_is_guard(s),
                                pages_id(pg) == desc_id(&desc_at(self.spec_table(), s)) + 16,
                                g@.partial == part,
                            decreases v0.pages - p,
                        {
                            if (cand >> (p as u16)) & 1 == 1 {
                                proof {
                                    assert(g@.slab_ok(s as nat));
                                }
                                let (c, sp) = Self::pages_locked(&g, pg, s);
                                let hid = self.close_page(&mut g, s, pg, v0.with_closed(c), p, sp);
                                closed_now = closed_now.saturating_add(hid);
                            }
                            p += 1;
                        }
                    } else {
                        let n = page_count(cand);
                        let step = curve.step(now, st);
                        let wt = Curve::weight(step);
                        proof {
                            assert(n as u64 * wt <= n * 0xffff_ffff) by (nonlinear_arith)
                                requires n <= 16, wt <= 0xffff_ffff;
                            assert(weighted + n * 0xffff_ffff <= (total + n) * 0xffff_ffff) by (nonlinear_arith)
                                requires weighted <= total * 0xffff_ffff;
                            assert(nf < 64);
                            assert((total + n) * 0xffff_ffff <= 2000 * 0xffff_ffff) by (nonlinear_arith)
                                requires total <= nf * 16, nf < 64, n <= 16;
                        }
                        counts[step] += n as u16;
                        total += n;
                        weighted += n as u64 * wt;
                        let ghost f0 = found;
                        found[nf] = Found { slab: s as u32, pages: cand, step: step as u8 };
                        proof {
                            assert forall|j: int| 0 <= j < nf + 1 implies #[trigger] g@.partial.map@.contains(found[j].slab as nat)
                                && found[j].slab < self.spec_nslabs() && found[j].step < STEPS by {
                                if j < nf {
                                    assert(found[j] == f0[j]);
                                    assert(f0[j] == fh[j]);
                                    assert(g@.partial == gp);
                                    assert(gp.map@.contains(fh[j].slab as nat));
                                } else {
                                    assert(found[j].slab == s as u32 && found[j].step == step as u8);
                                    assert(g@.partial.map@.contains(s as nat));
                                    assert(s < 0x400_0000);
                                    assert((s as u32) as usize == s);
                                    assert(step < 200);
                                    assert((step as u8) as usize == step);
                                }
                            }
                        }
                        nf += 1;
                    }
                }
            }
            proof {
                assert(g@.partial == gp);
                assert forall|j: int| 0 <= j < nf implies #[trigger] g@.partial.map@.contains(found[j].slab as nat)
                    && found[j].slab < self.spec_nslabs() && found[j].step < STEPS by {
                    if j < nfh {
                        assert(found[j] == fh[j]);
                        assert(gp.map@.contains(fh[j].slab as nat));
                    }
                }
            }
            next = g.partial.map.lowest_set_from(s + 1);
        }
        let ghost gc = g@;
        g.pages_cursor = next.unwrap_or(0);
        proof {
            lemma_walk_frame(self, gc, g@);
        }
        // Pass 2: close every closable page in a step older than the cut,
        // and all but the lowest `keep` of the cut's step.
        let allow = if hinted { 0 } else { Curve::allowance(weighted) };
        let mut left = total > 0;
        if total > allow {
            // Hinted: every closable page in a step from 1 on closes.
            let (cut, mut keep) = if hinted { (1, 0) } else { cut_of(&counts, total - allow) };
            left = false;
            let mut i: usize = 0;
            while i < nf
                invariant
                    crate::sys::may_abort() || crate::sys::valid_aborts(),
                    crate::sys::may_syscall(),
                    self.is_process(),
                    walk_ok(self, g@),
                    g.spec_lock() == self.inner.0,
                    *t == self.spec_table(),
                    v0.paged(),
                    nf <= WALK_SLABS,
                    i <= nf,
                    forall|j: int| 0 <= j < nf ==> #[trigger] g@.partial.map@.contains(found[j].slab as nat)
                        && found[j].slab < self.spec_nslabs() && found[j].step < STEPS,
                decreases nf - i,
            {
                let f = found[i];
                let (fs, fstep) = (f.slab as usize, f.step as usize);
                let ghost part = g@.partial;
                proof {
                    assert(g@.partial.map@.contains(found[i as int].slab as nat));
                    assert(g@.slab_ok(fs as nat));
                }
                if let Some(pg) = t.pages(fs) {
                    let mut p: usize = 0;
                    while p < v0.pages
                        invariant
                            crate::sys::may_abort() || crate::sys::valid_aborts(),
                            crate::sys::may_syscall(),
                            self.is_process(),
                            walk_ok(self, g@),
                            g.spec_lock() == self.inner.0,
                            *t == self.spec_table(),
                            nf <= WALK_SLABS,
                            i < nf,
                            v0.paged(),
                            fs < self.spec_nslabs(),
                            g@.partial.map@.contains(fs as nat),
                            !self.spec_table().spec_is_guard(fs),
                            pages_id(pg) == desc_id(&desc_at(self.spec_table(), fs)) + 16,
                            g@.partial == part,
                        decreases v0.pages - p,
                    {
                        if (f.pages >> (p as u16)) & 1 == 1 {
                            let close = fstep > cut || (fstep == cut && {
                                if keep > 0 {
                                    keep -= 1;
                                    false
                                } else {
                                    true
                                }
                            });
                            if close {
                                proof {
                                    assert(g@.slab_ok(fs as nat));
                                }
                                let (c, sp) = Self::pages_locked(&g, pg, fs);
                                let hid = self.close_page(&mut g, fs, pg, v0.with_closed(c), p, sp);
                                if hid == 0 {
                                    left = true;
                                }
                                closed_now = closed_now.saturating_add(hid);
                            } else {
                                left = true;
                            }
                        }
                        p += 1;
                    }
                }
                proof {
                    assert forall|j: int| 0 <= j < nf implies #[trigger] g@.partial.map@.contains(found[j].slab as nat)
                        && found[j].slab < self.spec_nslabs() && found[j].step < STEPS by {
                        assert(g@.partial == part);
                    }
                }
                i += 1;
            }
        }
        let any = visited > 0;
        // The closable slots of this pass over the partial set, less the
        // ones this walk closed; published once the cursor has been round
        // the set, the walks between accumulate.
        let ghost gk = g@;
        g.closable_cycle = g.closable_cycle.saturating_add(found_slots.saturating_sub(closed_now));
        if next.is_none() || all {
            g.closable_slots = g.closable_cycle;
            g.closable_cycle = 0;
        }
        proof {
            lemma_walk_frame(self, gk, g@);
        }
        g.unlock();
        // The class keeps its walk a curve step ahead while it has partial
        // slabs (docs/proposals/slab-pages.md, 3.4), so that a page that
        // becomes closable is stamped within a step; the driver is told
        // the deadline only while closable pages are left, so that partial
        // slabs alone neither keep a background thread awake nor start one.
        let step = curve.step_ns();
        let next_due = if left || any {
            now.saturating_add(if step == 0 { 1 } else { step })
        } else {
            0
        };
        self.pages_due.store(next_due, Ordering::Relaxed);
        if left {
            Some(next_due)
        } else {
            None
        }
    }
}

} // verus!
