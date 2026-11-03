//! Specifications for `huge/mod.rs`: the invariant of the huge lock and
//! what every value it holds keeps from the first.

use vstd::prelude::*;

use crate::lock::{lock_frame, lock_inv};

verus! {

/// Assumption: every holder of the huge lock leaves `huge::Inner::wf`
/// true when it unlocks, so the lock's invariant is `Inner::wf`. Read it
/// as: the real state always has a ghost witness (the descriptor
/// permissions and their blocks' claims, the list of live descriptors,
/// the `Empty` list's order, and the run authorities whose sets are the
/// `Active` extents and their pages) that makes `wf` true. Verus does not
/// check what a holder does before it drops a guard; each verified holder
/// ends with the value satisfying `wf` or only reads.
///
/// The writers of the locked value: `Huge::new` builds it (verified:
/// `Lock::new` requires `wf`); `lookup`, `usable_size`, `empty_bytes` and
/// `stats` are verified and only read. `alloc_run` (through
/// `alloc_aligned`), `free_run`, `purge_empty` and the decay walk
/// (`purge_curve`) are verified and write it, though nothing checks that
/// they restore `wf` at the guard's drop; the test-only `oldest_dirty_at`
/// and `audit` only read. The fork handlers take
/// and release the raw lock without touching the value, and nothing calls
/// `get_unlocked` on it. What is left is the claim that the lock protects
/// `wf`.
///
/// `api::tests::lock_holders_match_inv_docs` checks that the functions
/// that take the guard are the holders named here.
pub(crate) broadcast axiom fn axiom_huge_lock_inv(v: crate::huge::Inner)
    ensures
        #[trigger] lock_inv(v) == v.wf(),
;

/// Assumption: what every value the huge lock holds keeps from the one
/// `Huge::new` created it with is exactly the ghost page count, page
/// shift and `Meta` identity. They are ghost fields set only in
/// `Huge::new`. The verified writers assert it of the value they leave
/// (`alloc_aligned` on its paths after the first write, `purge_empty`,
/// `purge_curve`, `free_run`).
pub(crate) broadcast axiom fn axiom_huge_lock_frame(a: crate::huge::Inner, b: crate::huge::Inner)
    ensures
        #[trigger] lock_frame(a, b) == (a.spec_np() == b.spec_np() && a.spec_lg() == b.spec_lg() && a.spec_mid() == b.spec_mid()),
;

} // verus!
