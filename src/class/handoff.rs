//! The fresh half of slab selection: the next slab of the arena's
//! current group, an idle group adopted from another arena (design
//! 13.3), or the next fresh group of the region.

use super::abort::{idle_group_in_use, owner_mismatch, slab_count_underflow};
use super::{ClassState, GROUP_HANDOFF, Inner};
use crate::meta::Meta;
use crate::purge;
use crate::slab::State;
#[cfg(test)]
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;

/// Groups of one donor's `empty` and `purged` sets a refill inspects for
/// an idle one before moving on to the next donor: a donor with many
/// partly idle groups (a medium class under churn) is not scanned to the
/// end.
const MAX_ADOPT_TRIES: usize = 8;

/// Tests: groups that changed hands.
#[cfg(test)]
pub(crate) static HANDOFFS: AtomicUsize = AtomicUsize::new(0);

/// What [`ClassState::next_fresh_slab`] yielded: a slab committed this
/// instant, or a group adopted from another arena whose idle slabs went
/// into the sets.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Fresh {
    Slab(usize),
    Adopted,
}

impl ClassState {
    /// The slab a refill uses when no partial slab has room (design
    /// section 5): the lowest `Empty` slab, else the lowest `Purged` one,
    /// else the next slab of this arena's current group, else a slab of
    /// an idle group taken over from another arena
    /// ([`adopt_idle_group`]), else the next fresh group of the class
    /// region ([`SlabTable::take_group`], which commits this arena's set
    /// words for the whole group first). Guard slabs inside a group are
    /// committed as guards and skipped. The commit lock nests inside the
    /// class lock. `None` when the region, the metadata region or a
    /// commit is exhausted.
    ///
    /// [`adopt_idle_group`]: ClassState::adopt_idle_group
    pub(super) fn next_slab(&self, g: &mut Inner, meta: &Meta, wake: &mut bool) -> Option<usize> {
        loop {
            // Spelled out, not `empty_take` and `purged_take`: this runs
            // inlined in `claim_with` (through `plan_locked`), whose
            // machine code the helpers reorder (`scripts/codegen-diff.sh`);
            // the per-allocation path stays as measured.
            if let Some(s) = g.empty.map.lowest_set() {
                g.empty.map.clear(s);
                g.stats.empty = g.stats.empty.checked_sub(1).unwrap_or_else(|| slab_count_underflow("empty"));
                return Some(s);
            }
            if let Some(s) = g.purged.map.lowest_set() {
                g.purged.map.clear(s);
                g.stats.purged = g.stats.purged.checked_sub(1).unwrap_or_else(|| slab_count_underflow("purged"));
                return Some(s);
            }
            match self.next_fresh_slab(g, meta, wake)? {
                Fresh::Slab(s) => return Some(s),
                // The adopted group's idle slabs are in the sets now.
                Fresh::Adopted => {}
            }
        }
    }

    /// The fresh half of [`next_slab`], for an arena whose sets are empty
    /// (or, the prefault pass, one that wants a slab beyond them): the
    /// next slab of this arena's current group, committed, else an idle
    /// group taken over from another arena ([`Fresh::Adopted`]: its
    /// `Empty` and `Purged` slabs are in this arena's sets and its
    /// uncommitted rest is the current group; nothing is committed), else
    /// the next fresh group of the class region, committed. Guard slabs
    /// are committed as guards and skipped. `None` when the region, the
    /// metadata region or a commit is exhausted.
    ///
    /// [`next_slab`]: ClassState::next_slab
    pub(super) fn next_fresh_slab(&self, g: &mut Inner, meta: &Meta, wake: &mut bool) -> Option<Fresh> {
        let table = self.table();
        let page = 1usize << table.lg_page;
        loop {
            if g.group_next >= g.group_end {
                if self.adopt_idle_group(g, meta, wake) {
                    return Some(Fresh::Adopted);
                }
                let (start, end) = table.take_group(meta, self.arena, |s| {
                    g.partial.ensure(meta, page, s)?;
                    g.empty.ensure(meta, page, s)?;
                    g.purged.ensure(meta, page, s)
                })?;
                g.group_next = start;
                g.group_end = end;
            }
            let s = g.group_next;
            let usable = table.commit_slab(s)?;
            g.group_next = s + 1;
            if usable {
                g.stats.committed += 1;
                return Some(Fresh::Slab(s));
            }
        }
    }

    /// Take over an idle group of this class from another arena (design
    /// 13.3: an arena owns its groups until it lends an idle one). Called
    /// with this arena's class lock held, its sets empty and its current
    /// group used up. Visits every other registered arena state: locks it,
    /// looks for an idle group in its `empty` and `purged` sets
    /// ([`next_idle_group`]) and, finding one, moves it
    /// ([`transfer_group`]). Returns `true` when a group changed hands:
    /// its `Empty` and `Purged` slabs are now in this arena's sets and its
    /// uncommitted slabs, if it was the donor's current group, are this
    /// arena's current group. Only the fresh-commit path pays for this;
    /// the slab transitions themselves keep no extra state.
    ///
    /// Lock order. This arena's class lock is held and every donor is
    /// only `try_lock`ed, never waited for: a busy donor is skipped and
    /// the fresh group is the fallback, so no thread ever blocks while
    /// holding a class lock and there is no order to keep (the fork
    /// handlers' order, class locks by arena index then the commit locks
    /// then the set tables, is untouched: `take_group`'s nesting is the
    /// only blocking chain). Blocking on higher-index donors was tried
    /// first: on `xmalloc`, where every donor is a producer whose lock
    /// also serves its consumers' flushes, the takers joined the convoy.
    /// The donor's lock pins the states of its slabs and the group's
    /// ownership, so what the scan finds is what the transfer moves.
    ///
    /// [`next_idle_group`]: ClassState::next_idle_group
    /// [`transfer_group`]: ClassState::transfer_group
    fn adopt_idle_group(&self, g: &mut Inner, meta: &Meta, wake: &mut bool) -> bool {
        if !GROUP_HANDOFF.load(Ordering::Relaxed) {
            return false;
        }
        let table = self.table();
        let mut p = table.states.load(Ordering::Acquire);
        while !p.is_null() {
            // SAFETY: registered states are never freed (`register_state`).
            let donor: &ClassState = unsafe { &*p };
            p = donor.next.load(Ordering::Relaxed);
            if donor.arena == self.arena {
                continue;
            }
            debug_assert!(core::ptr::eq(donor.table, self.table));
            let Some(mut dg) = donor.inner.try_lock() else {
                continue;
            };
            let mut budget = MAX_ADOPT_TRIES;
            if let Some(group) = self.next_idle_group(&dg, 0, &mut budget) {
                return self.transfer_group(g, &mut dg, donor, meta, group, wake);
            }
        }
        false
    }

    /// The lowest idle group of the arena whose class lock `dg` is held,
    /// at or above slab `from`: a group whose every committed non-guard
    /// slab is in `dg`'s `empty` or `purged` set (its committed slabs are
    /// those below the arena's cursor when it is the arena's current
    /// group, all of them otherwise). Walks both sets in address order,
    /// one group per step, and gives up after `budget` groups.
    pub(super) fn next_idle_group(&self, dg: &Inner, mut from: usize, budget: &mut usize) -> Option<usize> {
        let table = self.table();
        while from < table.nslabs && *budget > 0 {
            let s = match (dg.empty.map.lowest_set_from(from), dg.purged.map.lowest_set_from(from)) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => return None,
            };
            let group = s >> table.lg_group;
            let start = group << table.lg_group;
            let end = (start + table.group as usize).min(table.nslabs);
            let cursor = if dg.group_end == end { dg.group_next } else { end };
            if (start..cursor).all(|x| table.is_guard_slab(x) || dg.empty.map.is_set(x) || dg.purged.map.is_set(x)) {
                return Some(group);
            }
            *budget -= 1;
            from = end;
        }
        None
    }

    /// The handoff itself, with this arena's class lock (`g`) and the
    /// donor's (`dg`) held. `false` only when this arena's set words for
    /// the group cannot be committed (done first: the verification reads
    /// them, and on Linux an uncommitted metadata page faults), with
    /// nothing changed. Aborts when
    /// the descriptors disagree with the donor's sets (an `Active` slab, a
    /// live slot, a slab of another arena, a partial bit): a heap
    /// inconsistency. The owner word and the descriptors' arena field are
    /// rewritten together, as `take_group` writes them, so its cross-check
    /// still holds for every group.
    fn transfer_group(&self, g: &mut Inner, dg: &mut Inner, donor: &ClassState, meta: &Meta, group: usize, wake: &mut bool) -> bool {
        let table = self.table();
        let class = self.class as usize;
        let page = 1usize << table.lg_page;
        let start = group << table.lg_group;
        let end = (start + table.group as usize).min(table.nslabs);
        // SAFETY: the group holds a slab of the donor's sets, so it was
        // handed out and its owner page is committed.
        let owner_word = unsafe { table.owner_word(group) };
        let word = owner_word.load(Ordering::Relaxed);
        if word != donor.arena {
            owner_mismatch(class, group, start, word, donor.arena);
        }
        debug_assert!(g.group_next >= g.group_end);
        // This arena's set words for the group, before anything is read
        // from them (they may be beyond this arena's committed rows) or
        // changed: a failure here changes nothing.
        if g.partial.ensure(meta, page, end - 1).is_err()
            || g.empty.ensure(meta, page, end - 1).is_err()
            || g.purged.ensure(meta, page, end - 1).is_err()
        {
            return false;
        }
        // Slabs below the donor's cursor are committed; the rest of its
        // current group is not and becomes this arena's current group.
        let cursor = if dg.group_end == end { dg.group_next } else { end };
        for s in start..end {
            if table.is_guard_slab(s) {
                continue;
            }
            if purge::check_zero() {
                self.check_shadow(s, "adopt");
            }
            let d = self.desc(s);
            if d.arena() != donor.arena {
                owner_mismatch(class, group, s, donor.arena, d.arena());
            }
            let state = d.state();
            let in_sets = match state {
                State::Empty => dg.empty.map.is_set(s),
                State::Purged => dg.purged.map.is_set(s),
                State::Fresh => s >= cursor && !dg.empty.map.is_set(s) && !dg.purged.map.is_set(s),
                State::Active => false,
            };
            if !in_sets || d.live() != 0 || dg.partial.map.is_set(s) || g.partial.map.is_set(s) || g.empty.map.is_set(s) || g.purged.map.is_set(s) {
                idle_group_in_use(class, group, s, donor.arena, self.arena, state, d.live(), in_sets, dg.partial.map.is_set(s));
            }
        }
        let mut oldest = 0u64;
        for s in start..end {
            if table.is_guard_slab(s) {
                continue;
            }
            let d = self.desc(s);
            match d.state() {
                State::Empty => {
                    dg.empty.map.clear(s);
                    dg.stats.empty = dg.stats.empty.checked_sub(1).unwrap_or_else(|| slab_count_underflow("empty"));
                    g.empty.map.set(s);
                    g.stats.empty += 1;
                    let t = d.dirty_at();
                    if oldest == 0 || t < oldest {
                        oldest = t;
                    }
                }
                State::Purged => {
                    dg.purged.map.clear(s);
                    dg.stats.purged = dg.stats.purged.checked_sub(1).unwrap_or_else(|| slab_count_underflow("purged"));
                    g.purged.map.set(s);
                    g.stats.purged += 1;
                }
                State::Fresh | State::Active => {}
            }
            if s < cursor {
                dg.stats.committed = dg.stats.committed.checked_sub(1).unwrap_or_else(|| slab_count_underflow("committed"));
                g.stats.committed += 1;
            }
            d.set_arena(self.arena);
        }
        owner_word.store(self.arena, Ordering::Relaxed);
        #[cfg(test)]
        HANDOFFS.fetch_add(1, Ordering::Relaxed);
        if dg.group_end == end {
            g.group_next = dg.group_next;
            g.group_end = end;
            dg.group_next = end;
            dg.group_end = end;
        }
        // This arena's oldest `Empty` slab may now be one of the moved
        // ones. The donor's gate is left as it is: at worst it is too
        // early, and its next walk recomputes it (see `purge::Decayer`).
        if oldest != 0 {
            let cur = self.gate.oldest_empty();
            if (cur == 0 || oldest < cur) && self.gate.set_oldest_empty(oldest) {
                *wake = true;
            }
        }
        true
    }

    /// Tests: this arena's idle groups, the ones a handoff would offer.
    #[cfg(test)]
    pub(crate) fn idle_groups(&self) -> std::vec::Vec<usize> {
        let g = self.inner.lock();
        let mut out = std::vec::Vec::new();
        let mut from = 0;
        let mut budget = usize::MAX;
        while let Some(group) = self.next_idle_group(&g, from, &mut budget) {
            out.push(group);
            from = (group + 1) << self.table().lg_group;
        }
        out
    }
}
