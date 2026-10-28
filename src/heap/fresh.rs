//! A heap [`Heap::create`](super::Heap::create) has just made, before it
//! is registered: the token for its plain fields.

use super::{Heap, Shared};
use vstd::prelude::verus;

verus! {

/// Ghost: what [`Heap::create_detached`](super::Heap::create_detached)
/// stored in a fresh heap's plain fields, and the permission to them:
/// the heap, the permission to its head (`head`), the home arena, whether
/// [`mark_bound`](super::mark_bound) set its `bound` flag, and its
/// `Shared`.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub(super) tracked struct FreshHeap {
    ghost heap: *mut Heap,
    tracked head: vstd::raw_ptr::PointsToRaw,
    ghost arena: vstd::prelude::int,
    ghost bound: bool,
    ghost shared: *const Shared,
}

} // verus!
