//! Purge and the decay walk over the `empty` set (design section 8).

use super::abort::slab_count_underflow;
use super::{ClassState, Inner};
use crate::lock::LockOrder;
use crate::purge::{Batch, Curve, Left, Pieces, STEPS, Settle};
use crate::slab::State;
use vstd::prelude::*;

/// The older of two `dirty_at` stamps where 0 means "none yet": the fold
/// of the purge walks over the `Empty` slabs they keep, feeding the gate's `oldest_empty` (0 when no slab is `Empty`).
/// An `Empty` slab's stamp is never 0 (`on_live_zero` writes
/// `now.max(1)`), so 0 is unambiguous.
pub(super) fn min_nonzero(cur: u64, t: u64) -> u64 {
    debug_assert!(t != 0, "min_nonzero: an Empty slab's dirty_at is never 0");
    if cur == 0 || t < cur { t } else { cur }
}

verus! {

/// What a purge walk's flushes settled: slabs made `Purged`, the oldest
/// `dirty_at` among the survivors (`min_nonzero`), and whether a discard
/// failed. It settles the walk's batch ([`Settle`], through
/// [`ClassState::settle`]).
struct Tally {
    purged: usize,
    oldest: u64,
    failed: bool,
    /// Ghost: the bytes reported discarded ([`Settle::bytes`]).
    #[allow(dead_code)]
    bytes: Ghost<int>,
}

impl Tally {
    /// Nothing settled yet.
    fn new() -> (t: Tally)
        ensures
            t.bytes@ == 0,
    {
        Tally { purged: 0, oldest: 0, failed: false, bytes: Ghost(0) }
    }
}

// Sealed: no invariant over the class lock value, and `settle` trusted;
// `ClassState::settle` and the walks are outside `verus!`.
impl Settle for Tally {
    type Owner = ClassState;

    type Ctx = Inner;

    type Conf = ();

    closed spec fn conf(&self) -> () { () }

    closed spec fn inv(&self, o: &ClassState, c: Inner, pend: Pieces) -> bool {
        true
    }

    closed spec fn bytes(&self) -> int {
        self.bytes@
    }

    // Trusted: `ClassState::settle` is outside `verus!`; the ghost byte
    // count is the only state its `ensures` constrain.
    #[verifier::external_body]
    #[inline(always)]
    fn settle(
        &mut self,
        o: &ClassState,
        c: &mut Inner,
        addr: usize,
        len: usize,
        done: bool,
        _piece: Ghost<int>,
        _pend: Ghost<Pieces>,
    ) {
        o.settle(c, addr, len, done, self);
    }
}

} // verus!

/// The cut-off step of a decay walk that must purge `need` of the slabs
/// counted in `counts` (the age histogram, older steps at higher
/// indices): the oldest step such that it and the older ones hold at
/// least `need`, and how many of that step may stay.
fn cut_step(counts: &[u32; STEPS], need: usize) -> (usize, usize) {
    let mut cut = STEPS;
    let mut acc = 0usize;
    while acc < need {
        cut -= 1;
        acc += counts[cut] as usize;
    }
    (cut, acc - need)
}

/// A visitor of [`ClassState::walk_empty`]: the state of a walk (a
/// histogram, a count of slabs purged and their batch) as a struct, so
/// the walk's writes to it are visible to Verus, which does not model a
/// closure's writes to its captures.
pub(super) trait EmptyVisitor {
    /// Visit `slab`, stamped `dirty_at`, under the class lock (`g`).
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64);
}

impl ClassState {
    /// Walk this arena's empty set in address order under the class lock
    /// (`g`): `v.visit(self, g, slab, dirty_at)` for every slab in it,
    /// after the check that the slab is what the set says (`Empty`, `live
    /// == 0`; anything else is a heap inconsistency and aborts). The
    /// visitor may take slabs out of the set (a purge's flush settles the
    /// page groups behind the walk); one ahead of the walk that it took
    /// out would not be visited.
    pub(super) fn walk_empty<V: EmptyVisitor>(&self, g: &mut Inner, v: &mut V) {
        let mut next = g.empty.map.lowest_set();
        while let Some(slab) = next {
            let desc = self.desc(slab);
            assert!(
                desc.state() == State::Empty && desc.live() == 0,
                "class {}: slab {slab} in the empty set is not empty",
                self.class
            );
            v.visit(self, g, slab, desc.dirty_at());
            next = g.empty.map.lowest_set_from(slab + 1);
        }
    }

    /// Purge every `Empty` slab whose `dirty_at` is at or before `cutoff`
    /// (`u64::MAX` for all of them) and make it `Purged`. The discards go
    /// out in batches of [`purge::MAX_BATCH`] under the class lock, so no
    /// claim can hand out a slab between the discard and its state change;
    /// a slab whose discard fails stays `Empty` (the next purge retries
    /// it). Recomputes `oldest_empty` from the survivors. Returns the
    /// number purged.
    pub fn purge_empty(
        &self,
        cutoff: u64,
        _left: Tracked<&mut Ghost<Left>>,
        _ord: Tracked<&mut LockOrder>,
    ) -> usize {
        let spp = self.table().slabs_per_page();
        let mut g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
        let mut v = PurgeOlder {
            cutoff,
            p: Purger::new(spp),
        };
        self.walk_empty(&mut g, &mut v);
        v.p.batch.flush(&mut v.p.t, self, &mut g);
        self.gate.set_oldest_empty(v.p.t.oldest);
        v.p.t.purged
    }

    /// Push the whole page group of `slab` (one slab when the slab is at
    /// least a page) to `batch`; once it is discarded ([`settle`]) every
    /// `Empty` slab in it becomes `Purged`, a younger one included, since
    /// nothing in the group is in use. Under the class lock; the caller
    /// checked the group is idle. Returns the slab after the group.
    ///
    /// [`settle`]: ClassState::settle
    fn push_group(
        &self,
        g: &mut Inner,
        slab: usize,
        spp: usize,
        batch: &mut Batch,
        t: &mut Tally,
    ) -> usize {
        let first = slab & !(spp - 1);
        batch.push(
            self.slab_addr(first),
            self.class().slab_size() * spp,
            t,
            self,
            g,
        );
        first + spp
    }

    /// The flush's report on `[addr, addr + len)`, page groups pushed by
    /// [`push_group`] (under the class lock since): discarded, every
    /// `Empty` slab in it becomes `Purged`; not, they stay `Empty` and
    /// count among the survivors for `oldest_empty`.
    ///
    /// [`push_group`]: ClassState::push_group
    // Verus needs a loop with an invariant.
    #[allow(clippy::while_let_loop)]
    fn settle(&self, g: &mut Inner, addr: usize, len: usize, done: bool, t: &mut Tally) {
        let slab_size = self.class().slab_size();
        let first = (addr - self.slab_addr(0)) / slab_size;
        t.failed |= !done;
        let mut slabs = first..first + len / slab_size;
        loop {
            let s = match slabs.next() {
                Some(s) => s,
                None => break,
            };
            let d = self.desc(s);
            if d.state() != State::Empty {
                continue;
            }
            if !done {
                t.oldest = min_nonzero(t.oldest, d.dirty_at());
                continue;
            }
            self.set_state_locked(g, d, s, State::Purged);
            self.set_dirty_locked(g, d, s, 0);
            // Spelled out, not `empty_take` and `purged_add`: paired that
            // way the two stores reorder, and the release build of
            // unrelated code then changes shape (how it addresses two
            // merged statics; `scripts/codegen-diff.sh`). The seam's
            // invariant holds here by inspection: bit and count move
            // together for both sets.
            g.empty.map.clear(s);
            g.purged.map.set(s);
            g.stats.empty = g
                .stats
                .empty
                .checked_sub(1)
                .unwrap_or_else(|| -> usize { slab_count_underflow("empty") });
            g.stats.purged += 1;
            t.purged += 1;
        }
    }

    /// Decay walk (design section 8): purge the oldest `Empty` slabs until
    /// the number left is within the curve's allowance at `now`, the sum
    /// over the `Empty` slabs of the weight of their age, rounded up. Slabs
    /// are purged oldest step first; within the cut-off step the highest
    /// addresses go, so the lowest, which a claim takes first, stay. The
    /// discards go out in batches under the class lock, as `purge_empty`;
    /// a failed one leaves its slabs `Empty` and backs the gate off
    /// (`Decayer::back_off`). Recomputes `oldest_empty` from the
    /// survivors. Returns the number purged.
    fn purge_curve(
        &self,
        now: u64,
        curve: &Curve,
        keep16: u32,
        _ord: Tracked<&mut LockOrder>,
    ) -> usize {
        let spp = self.table().slabs_per_page();
        let mut g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
        // First pass: the age histogram of the empty set and its allowance.
        let mut h = Histogram {
            now,
            curve,
            counts: [0u32; STEPS],
            total: 0,
            weighted: 0,
            oldest: 0,
        };
        self.walk_empty(&mut g, &mut h);
        // The cgroup policy's retention cap (design 11): this walk's share
        // of the process-wide cap, on top of the curve.
        let mut allow = Curve::allowance(h.weighted);
        if keep16 < crate::policy::KEEP_ALL {
            allow = allow.min(crate::policy::mul16(h.total as u64, keep16) as usize);
        }
        if h.total <= allow {
            self.gate.set_oldest_empty(h.oldest);
            return 0;
        }
        // The cut-off step: every slab in an older step goes and all but
        // the first `keep` of this one.
        let need = h.total - allow;
        let (cut, keep) = cut_step(&h.counts, need);
        // Second pass: purge.
        let mut v = PurgeCurve {
            now,
            curve,
            cut,
            keep,
            p: Purger::new(spp),
        };
        self.walk_empty(&mut g, &mut v);
        v.p.batch.flush(&mut v.p.t, self, &mut g);
        if v.p.t.failed {
            self.gate.back_off(now);
        }
        self.gate.set_oldest_empty(v.p.t.oldest);
        v.p.t.purged
    }

    /// Whether the page group of `slab` (`spp` slabs, `spp > 1`) can be
    /// discarded: every slab in it is committed, owned by this arena, and
    /// `Fresh`, `Empty` or `Purged`. Called under this arena's class lock,
    /// which is what pins those states for this arena's slabs; a slab of
    /// another arena, or one not yet handed out by `next_fresh_slab`, could
    /// be claimed under a lock this thread does not hold, so it vetoes the
    /// purge.
    ///
    /// The owner is read from the descriptor here where the release path
    /// reads the owner word: they agree because a page group never
    /// straddles a hand-out group (`SlabTable::new` asserts `group %
    /// slabs_per_page == 0`), so every slab here is under one owner word,
    /// and `transfer_group` rewrites that word and every descriptor's
    /// arena in one hold of both class locks.
    // Verus needs a loop with an invariant.
    #[allow(clippy::while_let_loop)]
    fn page_group_is_idle(&self, slab: usize, spp: usize) -> bool {
        let first = slab & !(spp - 1);
        let committed = self.table().committed_slabs();
        let mut slabs = first..first + spp;
        loop {
            let s = match slabs.next() {
                Some(s) => s,
                None => break,
            };
            if s >= committed {
                return false;
            }
            let d = self.desc(s);
            if !(d.arena() == self.arena
                && matches!(d.state(), State::Fresh | State::Empty | State::Purged))
            {
                return false;
            }
        }
        true
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
    #[cfg(test)]
    pub fn decay(&self, now: u64, decay_ns: u64) -> Option<u64> {
        self.decay_with(
            now,
            decay_ns,
            crate::policy::KEEP_ALL,
            Tracked::assume_new(),
        )
    }

    /// [`decay`](ClassState::decay) with the cgroup policy's keep ratio
    /// (design 11): the walk keeps at most `keep16 / 65536` of its `Empty`
    /// slabs on top of the curve's allowance.
    pub fn decay_with(
        &self,
        now: u64,
        decay_ns: u64,
        keep16: u32,
        _ord: Tracked<&mut LockOrder>,
    ) -> Option<u64> {
        self.gate.decay(now, decay_ns, |now, curve| {
            self.purge_curve(now, curve, keep16, Tracked::assume_new());
        })
    }

    /// Bytes of this arena's `Empty` slabs of the class (one lock hold).
    pub fn empty_bytes(&self, _ord: Tracked<&mut LockOrder>) -> usize {
        self.inner
            .lock(Tracked::assume_new(), Tracked::assume_new())
            .stats
            .empty
            * self.class().slab_size()
    }
}

/// The purge half of both purge walks: the page groups pushed, their
/// discard batch and the tally.
struct Purger {
    /// Slabs per page group.
    spp: usize,
    /// Slabs below `pushed` are in a page group already in the batch.
    pushed: usize,
    batch: Batch,
    t: Tally,
}

impl Purger {
    #[inline(always)]
    fn new(spp: usize) -> Purger {
        Purger {
            spp,
            pushed: 0,
            batch: Batch::new(Ghost::assume_new()),
            t: Tally::new(),
        }
    }

    /// Visit `slab` (stamped `dirty_at`), at or past the page groups
    /// pushed: push its page group when `purge` holds and the group is
    /// idle, else keep it and fold its stamp into `oldest`.
    #[inline(always)]
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64, purge: bool) {
        let spp = self.spp;
        if purge && (spp == 1 || cs.page_group_is_idle(slab, spp)) {
            self.pushed = cs.push_group(g, slab, spp, &mut self.batch, &mut self.t);
        } else {
            self.t.oldest = min_nonzero(self.t.oldest, dirty_at);
        }
    }
}

/// `purge_empty`'s visitor: purge every slab stamped at or before
/// `cutoff`.
struct PurgeOlder {
    cutoff: u64,
    p: Purger,
}

impl EmptyVisitor for PurgeOlder {
    #[inline(always)]
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64) {
        if slab < self.p.pushed {
            return;
        }
        let purge = dirty_at <= self.cutoff;
        self.p.visit(cs, g, slab, dirty_at, purge);
    }
}

/// `purge_curve`'s first pass: the age histogram of the empty set (steps
/// of `curve` at `now`), its size, its weighted sum and its oldest stamp.
struct Histogram<'a> {
    now: u64,
    curve: &'a Curve,
    counts: [u32; STEPS],
    total: usize,
    weighted: u64,
    oldest: u64,
}

impl EmptyVisitor for Histogram<'_> {
    #[inline(always)]
    fn visit(&mut self, _: &ClassState, _: &mut Inner, _: usize, dirty_at: u64) {
        let step = self.curve.step(self.now, dirty_at);
        self.counts[step] += 1;
        self.total += 1;
        self.weighted += Curve::weight(step);
        self.oldest = min_nonzero(self.oldest, dirty_at);
    }
}

/// `purge_curve`'s second pass: purge every slab in a step older than
/// `cut`, and all but the first `keep` of step `cut`.
struct PurgeCurve<'a> {
    now: u64,
    curve: &'a Curve,
    cut: usize,
    keep: usize,
    p: Purger,
}

impl EmptyVisitor for PurgeCurve<'_> {
    #[inline(always)]
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64) {
        if slab < self.p.pushed {
            return;
        }
        let step = self.curve.step(self.now, dirty_at);
        let purge = step > self.cut
            || (step == self.cut && {
                if self.keep > 0 {
                    self.keep -= 1;
                    false
                } else {
                    true
                }
            });
        self.p.visit(cs, g, slab, dirty_at, purge);
    }
}
