//! Specifications for `huge/mod.rs`: the lock invariant of its state.

use vstd::prelude::*;

#[cfg(verus_only)]
use crate::vspec::lock::{lock_frame, lock_inv};

verus! {

/// Assumption: every holder of the huge lock leaves `huge::Inner::wf`
/// true when it unlocks, so the lock's invariant is `Inner::wf`. Read it
/// as: the real state always has a ghost witness (descriptor permissions
/// whose values are the descriptors' real contents, the list of live
/// descriptors, the `Empty` list's order, and the run authorities whose
/// sets are the `Active` extents and their pages) that makes `wf` true. Verus
/// does not check what a holder does before it drops a guard; each
/// verified holder ends with the value satisfying `wf`, as the
/// postcondition of the last call that writes it (`take` in `alloc_run`)
/// or of a lemma (`lemma_free_end` in `free_run`), or by an explicit
/// `assert(g@.wf())`, or only reads.
///
/// The writers of the locked value: `Huge::new` builds it (verified:
/// `Lock::new` requires `wf`, but `Huge::new`'s own preconditions,
/// well-formed regions below 2^57 with 1 to 2^32 - 1 huge pages, are its
/// unverified caller's); `alloc_run`, `take`, `free_run`, `purge_empty`,
/// `purge_extent` and `purge_curve` are verified to keep it; `find`,
/// `lookup`, `usable_size`, `empty_bytes` and `stats` only read, as do the
/// test-only `oldest_dirty_at` and `audit`. What those proofs trust
/// (among it, that the descriptor blocks `alloc_desc` hands out are fresh,
/// and that `Set::ensure` on the free-page set leaves the bitmap view
/// unchanged) is trusted where it is stated, not here. The fork handlers take
/// and release the raw lock without touching the value, and nothing calls
/// `get_unlocked` on it. What is left is the claim that the lock protects
/// `wf`.
pub(crate) broadcast axiom fn axiom_huge_lock_inv(v: crate::huge::Inner)
    ensures
        #[trigger] lock_inv(v) == v.wf(),
;

/// Assumption: the huge state's ghost page count, page shift and `Meta`
/// identity never change after `Huge::new` sets them. Verified code never
/// assigns them (they are ghost fields, set only in `Huge::new`) and
/// unverified code cannot.
pub(crate) broadcast axiom fn axiom_huge_lock_frame(a: crate::huge::Inner, b: crate::huge::Inner)
    ensures
        #[trigger] lock_frame(a, b) ==> a.spec_np() == b.spec_np() && a.spec_lg() == b.spec_lg() && a.spec_mid() == b.spec_mid(),
;

} // verus!
