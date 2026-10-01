//! The fresh half of slab selection: the next slab of the arena's
//! current group, or the next fresh group of the region.

use super::abort::slab_count_underflow;
use super::{ClassState, Inner};
use crate::meta::Meta;
use vstd::prelude::*;

#[cfg(verus_only)]
use super::table::{
    guard_marked, lemma_guard_marked_mono, lemma_slab_marked_mono, lemma_spec_class,
    lemma_spec_nslabs, slab_marked,
};
#[cfg(verus_only)]
use super::{SlabTable, lemma_same_sets_inv};
#[cfg(verus_only)]
use crate::slab::State;

verus! {

broadcast use {lemma_spec_nslabs, lemma_spec_class};

impl ClassState {
    /// The slab a refill uses when no partial slab has room (design
    /// section 5): the lowest `Empty` slab, else the lowest `Purged` one,
    /// else the next slab of this arena's current group, else the next
    /// fresh group of the class region ([`SlabTable::take_group`], which
    /// commits this arena's set words for the whole group first). Guard
    /// slabs inside a group are committed as guards and skipped. The
    /// commit lock nests inside the class lock. `None` when the region,
    /// the metadata region or a commit is exhausted.
    pub(super) fn next_slab(&self, g: &mut Inner, meta: &Meta) -> (r: Option<usize>)
        requires
            old(g).inv(),
            old(g).nbits() == self.spec_nslabs(),
            old(g).arena@ == self.arena,
            old(g).table@ == self.spec_table(),
        ensures
            final(g).nbits() == old(g).nbits(),
            final(g).arena == old(g).arena,
            final(g).table == old(g).table,
            r is None ==> final(g).inv(),
            r matches Some(s) ==> s < self.spec_nslabs() && final(g).ready(s as nat),
    {
        // Spelled out, not `empty_take` and `purged_take`: this runs
        // inlined in `claim_with` (through `plan`), whose machine
        // code the helpers reorder (`scripts/codegen-diff.sh`); the
        // per-allocation path stays as measured.
        if let Some(s) = g.empty.map.lowest_set() {
            let ghost g0 = *g;
            proof {
                assert(g0.slab_ok(s as nat));
            }
            g.empty.map.clear(s);
            g.stats.empty = g
                .stats
                .empty
                .checked_sub(1)
                .unwrap_or_else(|| slab_count_underflow("empty"));
            proof {
                assert forall|x: nat| x != s implies #[trigger] g.slab_ok(x) by {
                    assert(g0.slab_ok(x));
                }
            }
            return Some(s);
        }
        if let Some(s) = g.purged.map.lowest_set() {
            let ghost g0 = *g;
            proof {
                assert(g0.slab_ok(s as nat));
            }
            g.purged.map.clear(s);
            g.stats.purged = g
                .stats
                .purged
                .checked_sub(1)
                .unwrap_or_else(|| slab_count_underflow("purged"));
            proof {
                assert forall|x: nat| x != s implies #[trigger] g.slab_ok(x) by {
                    assert(g0.slab_ok(x));
                }
            }
            return Some(s);
        }
        let r = self.next_fresh_slab(g, meta);
        proof {
            if let Some(s) = r {
                assert(g.slab_ok(s as nat));
            }
        }
        r
    }

    /// The fresh half of [`next_slab`], for an arena whose sets are empty:
    /// the next slab of this arena's current group, committed, else the next
    /// fresh group of the class region, committed. Guard slabs are
    /// committed as guards and skipped. `None` when the region, the
    /// metadata region or a commit is exhausted. Out of line: it runs once
    /// per slab, not once per refill, and inlined it would double the size
    /// of `claim_with`.
    ///
    /// A fresh group's slabs join this arena's ghost `Fresh` with stamp 0,
    /// as `SlabDesc::init` writes them, and as its current group: no arena's
    /// ghost holds them, since ownership exists only below the watermark
    /// `take_group` moved past them (`SlabTable::next_slab_locked`, proved
    /// for this arena's value; see `owner_locked` for the others). Slabs
    /// already owned keep their ghost state and stamp. The loop ends
    /// because every group taken ends beyond the current group's end and
    /// every other round consumes a slab of the current group.
    ///
    /// [`next_slab`]: ClassState::next_slab
    #[inline(never)]
    pub(super) fn next_fresh_slab(&self, g: &mut Inner, meta: &Meta) -> (r: Option<usize>)
        requires
            old(g).inv(),
            old(g).nbits() == self.spec_nslabs(),
            old(g).arena@ == self.arena,
            old(g).table@ == self.spec_table(),
        ensures
            final(g).inv(),
            final(g).nbits() == old(g).nbits(),
            r matches Some(s) ==> s < self.spec_nslabs()
                && final(g).owns(s as nat) && final(g).st(s as nat) == State::Fresh
                && final(g).committed@.contains(s as nat)
                && !(final(g).group_next <= s < final(g).group_end),
            forall|s: nat| #[trigger] old(g).owns(s) ==> final(g).owns(s) && final(g).st(s) == old(g).st(s)
                && final(g).dirty@[s] == old(g).dirty@[s],
            forall|s: nat| #[trigger] old(g).committed@.contains(s) ==> final(g).committed@.contains(s),
            final(g).arena == old(g).arena,
            final(g).table == old(g).table,
    {
        let table = self.table();
        proof {
            use_type_invariant(self);
            use_type_invariant(table);
            lemma_lg_group(*table);
        }
        let page = 1usize << table.lg_page;
        loop
            invariant
                g.inv(),
                g.nbits() == self.spec_nslabs(),
                g.nbits() == old(g).nbits(),
                g.arena == old(g).arena,
                g.table == old(g).table,
                g.arena@ == self.arena,
                g.table@ == self.spec_table(),
                *table == self.spec_table(),
                table.inv(),
                table.lg_group < 32,
                self.spec_nslabs() <= 0x400_0000,
                forall|s: nat| #[trigger] old(g).owns(s) ==> g.owns(s) && g.st(s) == old(g).st(s)
                    && g.dirty@[s] == old(g).dirty@[s],
                forall|s: nat| #[trigger] old(g).committed@.contains(s) ==> g.committed@.contains(s),
            decreases self.spec_nslabs() - g.group_end, g.group_end - g.group_next,
        {
            broadcast use lemma_same_sets_inv;
            if g.group_next >= g.group_end {
                let ghost ga = *g;
                // The fresh markers of the group's slabs join the ghost
                // with them.
                let tracked mut im = g.inited.borrow().duplicate();
                let (start, end) = table.take_group(meta, self.arena, g, page, Tracked(&mut im))?;
                let ghost g0 = *g;
                proof {
                    assert(g0.same_sets(ga));
                    assert(ga.inv());
                    g.inited.borrow_mut().combine(im);
                    assert forall|x: nat| #[trigger] g0.owns(x) implies x < start by {
                        assert(ga.owns(x));
                    }
                    let range = vstd::set::Set::<nat>::range(start as nat, end as nat);
                    g.states@ = g.states@.union_prefer_right(Map::new(range, |x: nat| State::Fresh));
                    g.dirty@ = Map::new(g0.dirty@.dom() + g0.states@.dom() + range,
                        |x: nat| if start <= x < end { 0u64 } else { g0.dirty@[x] });
                }
                g.group_next = start;
                g.group_end = end;
                proof {
                    broadcast use vstd::set_lib::range_set_properties;
                    assert forall|x: nat| #[trigger] g.owns(x) == (g0.owns(x) || start <= x < end) by {}
                    assert forall|x: nat| start <= x < end implies #[trigger] g.st(x) == State::Fresh by {}
                    assert forall|x: nat| #[trigger] g0.owns(x) implies g.st(x) == g0.st(x) && g.dirty@[x] == g0.dirty@[x] by {}
                    assert forall|x: int| start <= x < end && !table.spec_is_guard(x as usize)
                        implies #[trigger] slab_marked(g0.table@, x, g.inited@@) by {
                        lemma_slab_marked_mono(g0.table@, x, im@, g.inited@@);
                    }
                    assert forall|x: int| start <= x < end && table.spec_is_guard(x as usize)
                        implies #[trigger] guard_marked(g0.table@, x, g.inited@@) by {
                        lemma_guard_marked_mono(g0.table@, x, im@, g.inited@@);
                    }
                    lemma_fresh_group(g0, *g, start, end);
                }
            }
            let s = g.group_next;
            let usable = table.commit_slab(s)?;
            let ghost g1 = *g;
            g.group_next = s + 1;
            proof {
                let t = self.spec_table();
                let (lg, gs) = (t.lg_group, s >> t.lg_group);
                assert(g1.slab_ok(s as nat));
                assert(g1.group_end == t.spec_group_range(gs).1);
                if s + 1 < g1.group_end {
                    lemma_group_shift(s, lg);
                    lemma_shr_in((s + 1) as usize, gs, lg);
                }
                assert forall|x: nat| #[trigger] g.slab_ok(x) by {
                    assert(g1.slab_ok(x));
                }
            }
            if usable {
                proof {
                    let r = vstd::set::Set::<nat>::range(0, g.nbits()).remove(s as nat);
                    assert(g.committed@.subset_of(r));
                    vstd::set_lib::lemma_len_subset(g.committed@, r);
                }
                let ghost g2 = *g;
                g.stats.committed += 1;
                proof {
                    g.committed@ = g.committed@.insert(s as nat);
                    assert forall|x: nat| #[trigger] g.slab_ok(x) by {
                        assert(g2.slab_ok(x));
                    }
                }
                return Some(s);
            }
        }
    }
}

/// The hand-out group's shift is below 32 (`group`, `1 << lg_group`, is
/// a `u32`) and the region has at most `2^26` slabs.
proof fn lemma_lg_group(t: SlabTable)
    requires
        t.inv(),
    ensures
        t.lg_group < 32,
        t.spec_nslabs() <= 0x400_0000,
        t.group as usize == 1usize << t.lg_group,
{
    broadcast use lemma_spec_nslabs;
    let (gr, lg) = (t.group, t.lg_group);
    assert(gr as usize == 1usize << lg && lg < 64 ==> lg < 32) by (bit_vector);
}

/// The first slab of `s`'s hand-out group is at most `s`, and `s` is
/// within a group of it.
proof fn lemma_group_shift(s: usize, lg: u8)
    requires
        lg < 32,
        s <= 0x400_0000,
    ensures
        (s >> lg) << lg <= s,
        s < ((s >> lg) << lg) + (1usize << lg),
        s >> lg <= 0x400_0000,
{
    assert((s >> lg) << lg <= s && s < ((s >> lg) << lg) + (1usize << lg) && s >> lg <= 0x400_0000) by (bit_vector)
        requires lg < 32, s <= 0x400_0000;
}

/// A slab within a group of `g << lg` is in hand-out group `g`.
proof fn lemma_shr_in(x: usize, g: usize, lg: u8)
    requires
        lg < 32,
        g <= 0x400_0000,
        g << lg <= x,
        x < (g << lg) + (1usize << lg),
    ensures
        x >> lg == g,
{
    assert(x >> lg == g) by (bit_vector)
        requires lg < 32, g <= 0x400_0000, g << lg <= x, x < (g << lg) + (1usize << lg);
}

/// The ghost side of a fresh group: `g1` is `g0` (every current-group
/// slab used up) with the slabs `start..end`, which `g0` does not own,
/// owned and `Fresh`, and with `start..end` as its current group; the
/// owned slabs keep their states and stamps.
proof fn lemma_fresh_group(g0: Inner, g1: Inner, start: usize, end: usize)
    requires
        g0.inv(),
        g0.group_end <= g0.group_next,
        start < end <= g0.nbits(),
        forall|x: nat| #[trigger] g0.owns(x) ==> x < start,
        g0.table@.spec_group_range(start >> g0.table@.lg_group) == (start, end),
        forall|x: nat| #[trigger] g1.owns(x) == (g0.owns(x) || start <= x < end),
        forall|x: nat| start <= x < end ==> #[trigger] g1.st(x) == State::Fresh,
        forall|x: nat| #[trigger] g0.owns(x) ==> g1.st(x) == g0.st(x) && g1.dirty@[x] == g0.dirty@[x],
        g1.group_next == start,
        g1.group_end == end,
        g1.partial == g0.partial,
        g1.empty == g0.empty,
        g1.purged == g0.purged,
        g1.stats == g0.stats,
        g1.committed == g0.committed,
        g1.table == g0.table,
        g1.inited@.id() == g0.inited@.id(),
        g0.inited@@.subset_of(g1.inited@@),
        forall|x: int| start <= x < end && !g0.table@.spec_is_guard(x as usize)
            ==> #[trigger] slab_marked(g0.table@, x, g1.inited@@),
        forall|x: int| start <= x < end && g0.table@.spec_is_guard(x as usize)
            ==> #[trigger] guard_marked(g0.table@, x, g1.inited@@),
    ensures
        g1.inv(),
{
    assert(g1.nbits() == g0.nbits());
    assert forall|x: nat| #[trigger] g1.states@.contains_key(x) implies x < g1.nbits() by {
        assert(g1.owns(x));
        if !(start <= x < end) {
            assert(g0.owns(x));
        }
    }
    assert forall|x: nat| #[trigger] g1.states@.contains_key(x) && !g1.table@.spec_is_guard(x as usize)
        implies slab_marked(g1.table@, x as int, g1.inited@@) by {
        assert(g1.owns(x));
        if !(start <= x < end) {
            assert(g0.owns(x));
            lemma_slab_marked_mono(g0.table@, x as int, g0.inited@@, g1.inited@@);
        }
    }
    assert forall|x: nat| #[trigger] g1.states@.contains_key(x) && g1.table@.spec_is_guard(x as usize)
        implies guard_marked(g1.table@, x as int, g1.inited@@) by {
        assert(g1.owns(x));
        if !(start <= x < end) {
            assert(g0.owns(x));
            lemma_guard_marked_mono(g0.table@, x as int, g0.inited@@, g1.inited@@);
        }
    }
    assert(g1.owns((end - 1) as nat));
    assert(g1.base());
    assert forall|x: nat| #[trigger] g1.slab_ok(x) by {
        assert(g0.slab_ok(x));
        if start <= x < end {
            assert(!g0.owns(x));
        }
    }
}

} // verus!
