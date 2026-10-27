//! Specifications for `arena.rs`: the invariant of its statistics lock.

use vstd::prelude::*;

use crate::lock::lock_inv;

verus! {

/// Every `ArenaStats` an arena's statistics lock may hold satisfies the
/// lock's invariant: the lock keeps no invariant of its value, which is
/// opaque (`stats::ExArenaStats`). Trusted as every `lock_inv` axiom is;
/// it says nothing of the figures, so it is true of any value.
pub broadcast axiom fn axiom_arena_stats_lock_inv(v: crate::stats::ArenaStats)
    ensures
        #[trigger] lock_inv(v),
;

} // verus!
