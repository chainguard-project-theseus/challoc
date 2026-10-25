//! The fresh half of slab selection: the next slab of the arena's
//! current group, or the next fresh group of the region.

use super::abort::slab_count_underflow;
use super::{ClassState, Inner};
use crate::meta::Meta;

impl ClassState {
    /// The slab a refill uses when no partial slab has room (design
    /// section 5): the lowest `Empty` slab, else the lowest `Purged` one,
    /// else the next slab of this arena's current group, else the next
    /// fresh group of the class region ([`SlabTable::take_group`], which
    /// commits this arena's set words for the whole group first). Guard
    /// slabs inside a group are committed as guards and skipped. The
    /// commit lock nests inside the class lock. `None` when the region,
    /// the metadata region or a commit is exhausted.
    pub(super) fn next_slab(&self, g: &mut Inner, meta: &Meta) -> Option<usize> {
        // Spelled out, not `empty_take` and `purged_take`: this runs
        // inlined in `claim_with` (through `plan`), whose machine
        // code the helpers reorder (`scripts/codegen-diff.sh`); the
        // per-allocation path stays as measured.
        if let Some(s) = g.empty.map.lowest_set() {
            g.empty.map.clear(s);
            g.stats.empty = g
                .stats
                .empty
                .checked_sub(1)
                .unwrap_or_else(|| slab_count_underflow("empty"));
            return Some(s);
        }
        if let Some(s) = g.purged.map.lowest_set() {
            g.purged.map.clear(s);
            g.stats.purged = g
                .stats
                .purged
                .checked_sub(1)
                .unwrap_or_else(|| slab_count_underflow("purged"));
            return Some(s);
        }
        self.next_fresh_slab(g, meta)
    }

    /// The fresh half of [`next_slab`], for an arena whose sets are empty:
    /// the next slab of this arena's current group, committed, else the next
    /// fresh group of the class region, committed. Slabs are committed a
    /// run at a time, up to the next guard slab ([`SlabTable::commit_run`]):
    /// a slab that starts no run was committed with the one that started
    /// it. Guard slabs are committed as guards and skipped. `None` when the
    /// region, the metadata region or a commit is exhausted. Out of line:
    /// it runs once per slab, not once per refill, and inlined it would
    /// double the size of `claim_with`.
    ///
    /// [`next_slab`]: ClassState::next_slab
    #[inline(never)]
    pub(super) fn next_fresh_slab(&self, g: &mut Inner, meta: &Meta) -> Option<usize> {
        let table = self.table();
        let page = 1usize << table.lg_page;
        loop {
            if g.group_next >= g.group_end {
                let (start, end) = table.take_group(meta, self.arena, g, page)?;
                g.group_next = start;
                g.group_end = end;
            }
            let s = g.group_next;
            let usable = if table.starts_run(s) {
                table.commit_run(s, g.group_end)?
            } else {
                true
            };
            g.group_next = s + 1;
            if usable {
                g.stats.committed += 1;
                return Some(s);
            }
        }
    }
}
