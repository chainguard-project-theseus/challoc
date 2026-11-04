//! Specifications for `huge/mod.rs`: the invariant of the huge lock and
//! what every value it holds keeps from the first.

use vstd::prelude::*;

verus! {

/// Assumption: every holder of the huge lock leaves `huge::Inner::wf`
/// true when it unlocks, so the lock's invariant is `Inner::wf`. Read it
/// as: the real state always has a ghost witness (the descriptor
/// permissions and their blocks' claims, the list of live descriptors,
/// the `Empty` list's order, and the run authorities whose sets are the
/// `Active` extents and their pages) that makes `wf` true. Each holder
/// outside the tests (`Huge::alloc_aligned`, `free_run`, `usable_size`,
/// `lookup`, `purge_empty`, `purge_curve`, `empty_bytes` and `stats`)
/// ends with `LockGuard::unlock`, which requires `wf` (this axiom) and
/// the frame ([`axiom_huge_lock_frame`]), on every path to a return
/// (`api::tests::guards_end_in_unlock`); a guard dropped without it
/// would release unchecked. A panic aborts (`panic = "abort"`) outside
/// the tests, so it never releases.
///
/// `Huge::new` builds the value (`Lock::new` requires `wf`). The
/// test-only `oldest_dirty_at` and `audit` only read. The fork handlers
/// take and release the raw lock without touching the value, and nothing
/// calls `get_unlocked` on it. What is left is the claim that the lock
/// protects `wf`.
///
/// `api::tests::lock_holders_match_inv_docs` checks that the functions
/// that take the guard are the holders named here.
pub(crate) broadcast axiom fn axiom_huge_lock_inv(v: crate::huge::Inner)
    ensures
        #[trigger] crate::lock::assumed_inv(v) == v.wf(),
;

/// Assumption: what every value the huge lock holds keeps from the one
/// `Huge::new` created it with is exactly the ghost page count, page
/// shift and `Meta` identity. They are ghost fields set only in
/// `Huge::new`. Each holder outside the tests checks it at its
/// `LockGuard::unlock`, as for [`axiom_huge_lock_inv`].
pub(crate) broadcast axiom fn axiom_huge_lock_frame(a: crate::huge::Inner, b: crate::huge::Inner)
    ensures
        #[trigger] crate::lock::assumed_frame(a, b) == (a.spec_np() == b.spec_np() && a.spec_lg() == b.spec_lg() && a.spec_mid() == b.spec_mid()),
;

} // verus!
