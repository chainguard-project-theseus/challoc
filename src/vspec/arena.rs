//! Specifications for `arena.rs`: `AtomicPtr::new`, `load` and `store`, and the
//! invariant of its statistics lock.

use vstd::prelude::*;

verus! {

/// `AtomicPtr::load`, which vstd does not specify: any pointer. It panics
/// for a `Release` or `AcqRel` ordering.
pub assume_specification<T>[ core::sync::atomic::Atomic::<*mut T>::load ](
    a: &core::sync::atomic::Atomic<*mut T>,
    order: core::sync::atomic::Ordering,
) -> *mut T
    requires
        !(order is Release),
        !(order is AcqRel),
;

/// `AtomicPtr::store`, which vstd does not specify. It panics for an
/// `Acquire` or `AcqRel` ordering.
pub assume_specification<T>[ core::sync::atomic::Atomic::<*mut T>::store ](
    a: &core::sync::atomic::Atomic<*mut T>,
    val: *mut T,
    order: core::sync::atomic::Ordering,
)
    requires
        !(order is Acquire),
        !(order is AcqRel),
;

/// `AtomicPtr::new`, which vstd does not specify.
pub assume_specification<T>[ core::sync::atomic::Atomic::<*mut T>::new ](v: *mut T) -> core::sync::atomic::Atomic<*mut T>;

/// `AtomicPtr::swap`, which vstd does not specify: any previous pointer.
pub assume_specification<T>[ core::sync::atomic::Atomic::<*mut T>::swap ](
    a: &core::sync::atomic::Atomic<*mut T>,
    val: *mut T,
    order: core::sync::atomic::Ordering,
) -> *mut T;

/// Assumption (`lock.rs`): the value of an arena's statistics lock keeps
/// the accounting identities (`ArenaStats::identities`) while the lock is
/// free. Its writers: `Arena::init` creates the lock with
/// `ArenaStats::empty` (verified: `Lock::new` requires the invariant and
/// `empty` ensures the identities), and `Arena::store_stats` stores a
/// value it requires them of (verified). `Arena::stats_into` and
/// `Arena::add_stats_to` only read. Each of the three ends in
/// `LockGuard::unlock`, which requires this and the frame
/// (`ArenaStats`'s `LockSpec` impl: true of any two values), on every
/// path to a return (`api::tests::guards_end_in_unlock`).
/// `Arena::store_stats`'s one caller, `stats::collect_into`, stores what
/// `collect_arena_with` computes, whose ensures give the identities. The fork
/// handlers take and release the raw lock without touching the value,
/// and nothing calls `get_unlocked` on it. What is left is the claim
/// that the lock protects the identities.
///
/// `api::tests::lock_holders_match_inv_docs` checks that the functions
/// that take the guard are the holders named here.
pub broadcast axiom fn axiom_arena_stats_lock_inv(v: crate::stats::ArenaStats)
    ensures
        #[trigger] crate::lock::assumed_inv(v) == v.identities(),
;

} // verus!
