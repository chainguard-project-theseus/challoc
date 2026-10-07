//! Specifications for `meta.rs`: the syscalls it makes and the invariant
//! of its lock.

use vstd::prelude::*;

#[cfg(verus_only)]
use crate::vspec::lock::{lock_frame, lock_inv};

verus! {


/// Assumption: every holder of the metadata lock leaves `Inner::wf`
/// true when it unlocks, so the lock's invariant is `Inner::wf`. Read it
/// as: the real state always has a ghost witness (free lists, the
/// permissions to free blocks and to the committed bytes above the bump
/// pointer, the uncommitted address space, and the records of live
/// objects and of unclaimed reserved bytes) that makes `wf` true. Verus
/// does not check what a holder does before it drops a guard; each
/// verified holder ends with the value satisfying `wf`, either as the
/// postcondition of the last `Inner` method it calls or by an explicit
/// `assert(g@.wf())`.
///
/// The writers of the locked value: `Meta::owning` builds it (verified:
/// `Lock::new` requires `wf`, and its caller must hand it the region's
/// `Unbacked` token, which only `Regions::reserve_owning` makes, through
/// `api::init` and `Shared::init`; the test-only `Meta::new` assumes
/// one); `Meta::alloc_raw` (and `alloc`),
/// `alloc_zeroed`, `free_claimed`, `commit_claimed`, `reserve`,
/// `reserve_committed_raw` (and `reserve_committed`, including its
/// rollback), `reserve_table` and `stats` are verified to keep it.
/// `Meta::free` takes its caller's claim to the object (a `MetaClaim`,
/// which `alloc_raw` and `alloc_zeroed` hand out) and passes it to
/// `free_claimed`; `Meta::commit` takes its caller's claim to the pages
/// (which `reserve` and `reserve_table` hand out, and each lazily
/// committed table keeps, `class::sets::Lazy::claimed`) and passes it to
/// `commit_claimed`, which takes the committed range's part. The
/// only writers not verified are the test-only
/// `set_eager_for_test` and `eager_allowed`, which touch only `eager`,
/// which `wf` does not mention. The fork handlers take and
/// release the raw lock without touching the value, and nothing calls
/// `get_unlocked` on it. What is left is the claim that the lock
/// protects `wf`.
pub(crate) broadcast axiom fn axiom_meta_lock_inv(v: crate::meta::Inner)
    ensures
        #[trigger] lock_inv(v) == v.wf(),
;

/// Assumption: the page size, the region's bounds and the identities of
/// the ghost records (live objects, unclaimed reserved bytes) never change
/// after `Meta::owning` sets them. Verified code never assigns `page`, `base`
/// or `len`, and only inserts and removes records (every `Inner` method
/// that does keeps `live_id` and `pages_id`), and nothing replaces the
/// value (`Inner::new` is called only by `Meta::owning`); unverified code
/// cannot reach the ghost fields and writes none of the three (the
/// test-only `set_eager_for_test` writes `eager`).
pub(crate) broadcast axiom fn axiom_meta_lock_frame(a: crate::meta::Inner, b: crate::meta::Inner)
    ensures
        #[trigger] lock_frame(a, b) ==> a.live_id() == b.live_id() && a.pages_id() == b.pages_id()
            && a.spec_page() == b.spec_page() && a.spec_base() == b.spec_base() && a.spec_len() == b.spec_len(),
;

} // verus!
