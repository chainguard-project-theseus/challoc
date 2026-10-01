//! Purge and the decay walk over the `empty` set (design section 8).

use super::abort::{empty_count_mismatch, slab_count_underflow};
use super::{ClassState, Inner};
use crate::purge::{Batch, Curve, DemandGate, STEPS};
use crate::slab::State;

/// The older of two `dirty_at` stamps where 0 means "none yet": the fold
/// of the purge walks over the `Empty` slabs they keep, feeding the gate's `oldest_empty` (0 when no slab is `Empty`).
/// An `Empty` slab's stamp is never 0 (`on_live_zero` writes
/// `now.max(1)`), so 0 is unambiguous.
pub(super) fn min_nonzero(cur: u64, t: u64) -> u64 {
    debug_assert!(t != 0, "min_nonzero: an Empty slab's dirty_at is never 0");
    if cur == 0 || t < cur { t } else { cur }
}

impl ClassState {
    /// Walk this arena's empty set in address order under the class lock
    /// (`g`): `f(g, slab, dirty_at)` for every slab in it, after the
    /// check that the slab is what the set says (`Empty`, `live == 0`;
    /// anything else is a heap inconsistency and aborts). `f` may take
    /// slabs out of the set, including ones ahead of the walk (a purge
    /// takes the whole page group), and the walk does not visit those.
    pub(super) fn walk_empty(&self, g: &mut Inner, mut f: impl FnMut(&mut Inner, usize, u64)) {
        let mut next = g.empty.map.lowest_set();
        while let Some(slab) = next {
            let desc = self.desc(slab);
            assert!(
                desc.state() == State::Empty && desc.live() == 0,
                "class {}: slab {slab} in the empty set is not empty",
                self.class
            );
            f(g, slab, desc.dirty_at());
            next = g.empty.map.lowest_set_from(slab + 1);
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
        self.walk_empty(&mut g, |g, slab, dirty_at| {
            if dirty_at <= cutoff && (spp == 1 || self.page_group_is_idle(slab, spp)) {
                n += self.purge_group(g, slab, spp, &mut batch);
            } else {
                oldest = min_nonzero(oldest, dirty_at);
            }
        });
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
            // Spelled out, not `empty_take` and `purged_add`: paired that
            // way the two stores reorder, and the release build of
            // unrelated code then changes shape (how it addresses two
            // merged statics; `scripts/codegen-diff.sh`). The seam's
            // invariant holds here by inspection: bit and count move
            // together for both sets.
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
    /// Recomputes `oldest_empty` from the survivors.
    ///
    /// With a demand tracker (`purge::demand`) the walk samples the
    /// class's demand, its slabs in use plus those that went `Empty` since
    /// the last walk and then the slabs in use, and keeps at least the
    /// recurring level less the
    /// slabs in use (`purge::allowance`; the retention cap still wins).
    /// Returns the number purged and, when that floor alone keeps every
    /// `Empty` slab, the hold deadline for the gate.
    fn purge_curve(&self, now: u64, curve: &Curve, keep16: u32, demand: Option<DemandGate<'_>>) -> (usize, Option<u64>) {
        let spp = self.table().slabs_per_page();
        let since = self.gate.last_walk();
        let mut g = self.inner.lock();
        // First pass: the age histogram of the empty set and its allowance.
        let mut counts = [0u32; STEPS];
        let mut total = 0usize;
        let mut fresh = 0usize;
        let mut weighted = 0u64;
        let mut oldest = 0u64;
        self.walk_empty(&mut g, |_, _, dirty_at| {
            let step = curve.step(now, dirty_at);
            counts[step] += 1;
            total += 1;
            // At or after: the stamps and the walk times are the coarse
            // clock, so a slab that went `Empty` in the tick of the last
            // walk but after it has its stamp; one that went `Empty` in
            // that tick before it is counted twice, which errs high by at
            // most a tick of frees.
            fresh += (dirty_at >= since) as usize;
            weighted += Curve::weight(step);
            oldest = min_nonzero(oldest, dirty_at);
        });
        if total != g.stats.empty {
            empty_count_mismatch(self.class, self.arena, total, g.stats.empty);
        }
        // The demand floor, then the cgroup policy's retention cap (design
        // 11): this walk's share of the process-wide cap, on top of both.
        let mut keep_demand = 0;
        let mut hold = None;
        if let Some(dm) = demand {
            // Since the last walk demand reached at least the slabs in
            // use plus those that went `Empty` meanwhile, and is now the
            // slabs in use: both samples, so a fall between two walks
            // commits its peak even when the walks are a hold apart.
            let active = g.stats.curslabs();
            let e = dm.window.epoch(now);
            dm.tracker.sample(e, active + fresh);
            dm.tracker.sample(e, active);
            if dm.apply {
                keep_demand = dm.tracker.level(e).saturating_sub(active);
            }
            if total > 0 && keep_demand >= total {
                hold = Some(dm.window.hold(now));
            }
        }
        let allow = crate::purge::allowance(Curve::allowance(weighted), keep_demand, total, keep16);
        if total <= allow {
            self.gate.set_oldest_empty(oldest);
            return (0, hold);
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
        self.walk_empty(&mut g, |g, slab, dirty_at| {
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
                n += self.purge_group(g, slab, spp, &mut batch);
            } else {
                oldest = min_nonzero(oldest, dirty_at);
            }
        });
        batch.flush();
        self.gate.set_oldest_empty(oldest);
        (n, None)
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
        self.decay_demand(now, decay_ns, keep16, None)
    }

    /// [`decay_with`](ClassState::decay_with) with the class's demand
    /// tracker (`purge::demand`): the walk keeps the demand floor too,
    /// and holds the next walk while the floor covers everything.
    pub fn decay_demand(&self, now: u64, decay_ns: u64, keep16: u32, demand: Option<DemandGate<'_>>) -> Option<u64> {
        self.gate.decay(now, decay_ns, keep16, |now, curve| self.purge_curve(now, curve, keep16, demand).1)
    }

    /// Bytes of this arena's `Empty` slabs of the class (one lock hold).
    pub fn empty_bytes(&self) -> usize {
        self.inner.lock().stats.empty * self.class().slab_size()
    }
}
