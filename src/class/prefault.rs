//! The prefault pass's side of the class (`purge::prefault`): commit
//! and populate the slab the next refill would commit, ahead of it.

use super::handoff::Fresh;
use super::{ClassState, Inner};
use crate::meta::Meta;
use crate::purge;
use crate::slab::State;
use core::sync::atomic::Ordering;

impl ClassState {
    /// The prefault pass's question: did a refill
    /// take slots of this class from this arena since the pass last
    /// asked? Advances the snapshot. Background thread only.
    pub fn prefault_due(&self) -> bool {
        self.prefault_delta() != 0
    }

    /// The slots refills took from this arena's slabs of the class since
    /// the pass last asked (0: not due), the pass's measure of the class's
    /// burst rate ([`prefault_ahead`]). Advances
    /// the snapshot. Background thread only.
    ///
    /// [`prefault_ahead`]: ClassState::prefault_ahead
    pub fn prefault_delta(&self) -> u64 {
        let now = self.refilled();
        let seen = self.prefault_seen.load(Ordering::Relaxed);
        if now == seen {
            return 0;
        }
        self.prefault_seen.store(now, Ordering::Relaxed);
        now.wrapping_sub(seen)
    }

    /// Slabs prefaulted for this arena and class so far.
    pub fn prefaulted(&self) -> u64 {
        self.prefaulted.load(Ordering::Relaxed)
    }

    /// Commit the slab the next refill of this class would commit, ahead
    /// of it, and hand it to `populate` while it is still this thread's
    /// alone (the prefault pass's commit-path half; design 8).
    /// Under the class lock: when the arena has an `Empty` or `Purged`
    /// slab the next refill takes that, so nothing is done; otherwise
    /// [`next_slab`] runs exactly the refill's own selection (the current
    /// group's next slab, else an idle group adopted from another arena,
    /// else a fresh group from the region) and the slab it yields is
    /// populated by `populate(addr, len)` (`MADV_POPULATE_WRITE`, or a
    /// touch per page, `purge::prefault`) with the lock still held, so no
    /// refill can be writing into it meanwhile, then published as
    /// `Purged` in the `purged` set: a `Purged` slab is what `next_slab`
    /// takes first after `Empty` ones, its memory reads zero (the kernel's
    /// fresh pages, which the populate leaves as they are), so `take`
    /// still reports it zero for `calloc` where `purged_is_zero` holds,
    /// and the decay walks leave the `purged` set alone, so the pages
    /// stay resident until the refill. When the adoption gave the arena an
    /// `Empty` or `Purged` slab instead, that slab goes back where it came
    /// from: the next refill takes it and nothing needs prefaulting. One
    /// slab per call. Returns the slab prefaulted, `None` when nothing
    /// was.
    ///
    /// [`next_slab`]: ClassState::next_slab
    pub fn prefault_next(&self, meta: &Meta, populate: impl FnOnce(usize, usize)) -> Option<usize> {
        let mut wake = false;
        let result = {
            let mut g = self.inner.lock();
            if g.empty.map.lowest_set().is_some() || g.purged.map.lowest_set().is_some() {
                None
            } else {
                self.prefault_one(&mut g, meta, &mut wake, populate).unwrap_or(None)
            }
        };
        if wake {
            purge::note_empty();
        }
        result
    }

    /// [`prefault_next`] sized to the class's burst rate (the prefault
    /// pass's claim-side half): commit and populate fresh slabs, as
    /// `prefault_next` does one, until the arena holds `ahead` idle
    /// (`Empty` or `Purged`) slabs of the class or the region is
    /// exhausted; `ahead` of 1 is `prefault_next`. A refill that needs
    /// more than one slab per pass interval (`refilled` moved by more
    /// than a slab's slots since the last pass) then finds the next ones
    /// ready too, where one slab per pass gives it every other one. The
    /// idle count is the sets' own (`stats.empty + stats.purged`), so a
    /// slab the adoption yielded counts and is not prefaulted over.
    /// Returns the number of slabs prefaulted.
    ///
    /// [`prefault_next`]: ClassState::prefault_next
    pub fn prefault_ahead(&self, meta: &Meta, ahead: usize, mut populate: impl FnMut(usize, usize)) -> usize {
        let mut wake = false;
        let mut n = 0;
        // One slab per lock hold: the populate runs under the class lock
        // (the slab is unpublished), and a refill of the class on another
        // thread waits for it, so the hold is bounded by one slab's
        // populate rather than `ahead` of them; the idle count is re-read
        // under the lock each time, so refills between the holds are seen.
        loop {
            let mut g = self.inner.lock();
            if g.stats.empty + g.stats.purged >= ahead {
                break;
            }
            match self.prefault_one(&mut g, meta, &mut wake, &mut populate) {
                Ok(Some(_)) => n += 1,
                Ok(None) => {}
                Err(()) => break,
            }
        }
        if wake {
            purge::note_empty();
        }
        n
    }

    /// One step of [`prefault_next`] under the lock: the refill's own
    /// [`next_fresh_slab`] (the sets are the caller's business: a refill
    /// takes them first, and a slab the pass took out of them would only
    /// go back), populated and published `Purged` (`Ok(Some(slab))`);
    /// `Ok(None)` when the adoption gave the arena idle slabs instead
    /// (they are in the sets untouched, the idle count grew); `Err` when
    /// the region, the metadata or a commit is exhausted.
    ///
    /// [`prefault_next`]: ClassState::prefault_next
    /// [`next_fresh_slab`]: ClassState::next_fresh_slab
    fn prefault_one(&self, g: &mut Inner, meta: &Meta, wake: &mut bool, populate: impl FnOnce(usize, usize)) -> Result<Option<usize>, ()> {
        match self.next_fresh_slab(g, meta, wake) {
            None => Err(()),
            // The adopted group's idle slabs are in the sets, untouched:
            // the refill takes them and the idle count grew.
            Some(Fresh::Adopted) => Ok(None),
            Some(Fresh::Slab(s)) => {
                let d = self.desc(s);
                // A slab committed this instant is `Fresh`; anything else
                // is a descriptor written outside the protocol.
                assert!(d.state() == State::Fresh, "class {}: prefault: freshly committed slab {s} reads {:?}", self.class, d.state());
                let cls = self.class();
                populate(self.table().slab_addr(s), cls.slab_size());
                d.set_state(State::Purged);
                d.set_dirty_at(0);
                g.purged_add(s);
                self.prefaulted.fetch_add(1, Ordering::Relaxed);
                Ok(Some(s))
            }
        }
    }
}
