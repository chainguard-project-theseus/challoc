//! Specifications for `huge/mod.rs`: the lock invariant of its state.

use vstd::prelude::*;

#[cfg(verus_only)]
use crate::vspec::lock::{lock_frame, lock_inv};

verus! {

/// Assumption: every holder of the huge lock leaves `huge::Inner::wf`
/// true when it unlocks, so the lock's invariant is `Inner::wf`. Read it
/// as: the real state always has a ghost witness (descriptor permissions
/// whose values are the descriptors' real contents, the list of live
/// descriptors, and the `Empty` list's order) that makes `wf` true. Verus
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

/// Assumption: the huge state's ghost page count and page shift never
/// change after `Huge::new` sets them. Verified code never assigns them
/// (they are ghost fields, set only in `Huge::new`) and unverified code
/// cannot.
pub(crate) broadcast axiom fn axiom_huge_lock_frame(a: crate::huge::Inner, b: crate::huge::Inner)
    ensures
        #[trigger] lock_frame(a, b) ==> a.spec_np() == b.spec_np() && a.spec_lg() == b.spec_lg(),
;

/// Assumption: the C contract of `free`, `realloc` and the sized frees,
/// that only a block's owner frees or reallocates it. A run
/// [`Huge::alloc_run`] returned at `p` for `size` bytes ([`allocated`])
/// keeps the descriptor `Huge::take` wrote for it, with the usable bytes
/// of its committed pages ([`Huge::spec_usable`]`(size)`, at least
/// `size`), until its owner frees it: nothing else writes an `Active`
/// descriptor. So while the caller that allocated the block at `p` still
/// holds it, the usable size [`Huge::usable_size`] reads for its page
/// (`r`, a [`Huge::spec_run_usable`]) covers the request. The premise
/// that the block has not been freed since is the caller's, as for
/// `vspec::addr::axiom_fresh_block`; the one call site is
/// `libc::calloc_usable_zeroed`, right after its `alloc_huge`.
///
/// [`Huge::alloc_run`]: crate::huge::Huge::alloc_run
/// [`allocated`]: crate::huge::allocated
/// [`Huge::spec_usable`]: crate::huge::Huge::spec_usable
/// [`Huge::usable_size`]: crate::huge::Huge::usable_size
/// [`Huge::spec_run_usable`]: crate::huge::Huge::spec_run_usable
#[verifier::external_body]
pub proof fn axiom_owned_run(shared: &crate::heap::Shared, p: usize, size: usize, r: usize)
    requires
        crate::huge::allocated(p, size),
        shared.huge_run_usable(p, r),
    ensures
        size <= r,
{
}

} // verus!
