//! Specifications for `arena.rs`: `AtomicPtr::new`, and the invariant of
//! its statistics lock.

use vstd::prelude::*;

use crate::lock::lock_inv;

verus! {

/// `AtomicPtr::new`, which vstd does not specify.
pub assume_specification<T>[ core::sync::atomic::Atomic::<*mut T>::new ](v: *mut T) -> core::sync::atomic::Atomic<*mut T>;

/// Assumption (`lock.rs`): the value of an arena's statistics lock keeps
/// the accounting identities (`ArenaStats::identities`) while the lock is
/// free. Its writers: `Arena::init` creates the lock with
/// `ArenaStats::empty` (verified: `Lock::new` requires the invariant and
/// `empty` ensures the identities), and `Arena::store_stats` stores a
/// value it requires them of (verified). `Arena::stats` only reads. The
/// caller Verus does not check is `stats::collect_into`, which stores
/// what `collect_arena_with` computes; its sums are exact only when no
/// figure wraps. The fork handlers take and release the raw lock without
/// touching the value, and nothing calls `get_unlocked` on it. What is
/// left is the claim that the lock protects the identities.
pub broadcast axiom fn axiom_arena_stats_lock_inv(v: crate::stats::ArenaStats)
    ensures
        #[trigger] lock_inv(v) == v.identities(),
;

} // verus!
