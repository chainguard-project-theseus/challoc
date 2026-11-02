//! Specifications for `meta.rs`: the invariant of the metadata lock and
//! what every value it holds keeps from the first.

use vstd::prelude::*;

use crate::lock::{lock_frame, lock_inv};

verus! {

/// Assumption: every holder of the metadata lock leaves `Inner::wf` true
/// when it unlocks, so the lock's invariant is `Inner::wf`. Read it as:
/// the real state always has a ghost witness (free lists, the permissions
/// to free blocks and to the committed bytes above the bump pointer, the
/// uncommitted address space, and the records of live objects and of
/// unclaimed reserved bytes) that makes `wf` true. Each holder outside
/// the tests (`Meta::alloc`, `alloc_raw`, `alloc_zeroed`, `free`,
/// `reserve`, `reserve_table`, `commit`, `reserve_committed_raw` and
/// `stats`) ends with `LockGuard::unlock`, which requires `wf` (this
/// axiom) and the frame ([`axiom_meta_lock_frame`]), on every path to a
/// return (`meta::tests::meta_guards_end_in_unlock`); a guard dropped
/// without it would release unchecked. A panic aborts (`panic = "abort"`)
/// outside the tests, so it never releases.
///
/// Holders not verified: the test-only `set_eager_for_test` and
/// `eager_allowed`, which touch only `eager`, which `wf` does not
/// mention, and `base` and `top`, which only read. The fork handlers take
/// and release the raw lock without touching the value, and nothing
/// calls `get_unlocked` on it. What is
/// left is the claim that the lock protects `wf`.
pub(crate) broadcast axiom fn axiom_meta_lock_inv(v: crate::meta::Inner)
    ensures
        #[trigger] lock_inv(v) == v.wf(),
;

/// Assumption: what every value the metadata lock holds keeps from the
/// one `Meta::owning` created it with is exactly the page size, the
/// region's bounds and the identities of the ghost records (live objects,
/// unclaimed reserved bytes). Each holder outside the tests checks it at
/// its `LockGuard::unlock`, as for [`axiom_meta_lock_inv`]. Nothing
/// replaces the value (`Inner::new` is called only by `Meta::owning`),
/// and the test-only `set_eager_for_test` writes only `eager`.
pub(crate) broadcast axiom fn axiom_meta_lock_frame(a: crate::meta::Inner, b: crate::meta::Inner)
    ensures
        #[trigger] lock_frame(a, b) == (a.live_id() == b.live_id() && a.pages_id() == b.pages_id()
            && a.spec_page() == b.spec_page() && a.spec_base() == b.spec_base() && a.spec_len() == b.spec_len()),
;

} // verus!
