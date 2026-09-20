//! Purge and the decay walk over the `empty` set (design section 8).

use super::abort::slab_count_underflow;
use super::{ClassState, Inner};
use crate::purge::{Batch, Curve, STEPS};
use crate::slab::State;

impl ClassState {
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
        self.gate.decay(now, decay_ns, |now, curve| {
            self.purge_curve(now, curve, keep16);
        })
    }

    /// Bytes of this arena's `Empty` slabs of the class (one lock hold).
    pub fn empty_bytes(&self) -> usize {
        self.inner.lock().stats.empty * self.class().slab_size()
    }
}
