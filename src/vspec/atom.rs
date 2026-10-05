//! `usize` atomics with a ghost permission, for the atomic invariants
//! that bound a value challoc only ever stores in range (`verus/TODO.md`,
//! "Coverage ceiling", ATOM): a class table's hand-out watermark
//! (`class/table.rs`, `committed_slabs`). The `u32` values (`policy.rs`'s
//! position, `arena.rs`'s arena count) use `count.rs`'s
//! [`CountPerm`](super::count::CountPerm).
//!
//! The same pattern as `word.rs` and `count.rs`: a permission names the
//! atomic's identity and value, the store is an `#[verifier::atomic]`
//! function (so it can run inside an `open_atomic_invariant!` block), the
//! load ([`load_usize_in`]) opens the invariant that holds the permission
//! itself, each passes the caller's ordering through unchanged, and
//! `#[inline(always)]` leaves the machine code of the bare `core` call.
//!
//! Trusted, all of it:
//! - each wrapped `core` operation is one atomic operation on the value
//!   (a load or a store), as `core` documents, and [`new_usize`] is
//!   `AtomicUsize::new`, which makes a new atomic;
//! - the proof model is sequentially consistent: the specifications ignore
//!   `ordering`, so nothing here proves that `Relaxed`, `Acquire` and
//!   `Release` are strong enough (`verus/TODO.md`, Tier 4); the invariants
//!   built on these only bound values, and every store writes a value in
//!   bounds, so a load returns an in-bounds value under any ordering;
//! - [`usize_id`] is the atomic's identity, and a permission is for the
//!   atomic with its `id` (as `count::count_id`). A permission is minted
//!   only by [`new_usize`], with its atomic.
//!
//! A permission's value is its atomic's: every store to an atomic that
//! has one goes through these wrappers, inside the atomic's invariant
//! (`SlabTable::take_group`).

use crate::sync::atomic::{AtomicUsize, Ordering};
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
use vstd::prelude::*;

verus! {

/// The identity of an atomic `usize`: what a [`UsizePerm`] is for.
pub uninterp spec fn usize_id(a: &AtomicUsize) -> int;

/// Ghost permission to the atomic `usize` with identity
/// [`id`](UsizePerm::id): its current [`value`](UsizePerm::value). It is
/// not `Copy`.
#[verifier::external_body]
pub tracked struct UsizePerm {
    _no_copy: (),
}

impl UsizePerm {
    /// The atomic this permission is for.
    pub uninterp spec fn id(&self) -> int;

    /// The atomic's value.
    pub uninterp spec fn value(&self) -> usize;
}

/// `AtomicUsize::new(v)`, with the one permission to it.
#[inline(always)]
#[verifier::external_body]
pub fn new_usize(v: usize) -> (r: (AtomicUsize, Tracked<UsizePerm>))
    ensures
        r.1@.id() == usize_id(&r.0),
        r.1@.value() == v,
{
    (AtomicUsize::new(v), Tracked::assume_new())
}

/// `a.load(ordering)` inside `inv`, an invariant that holds `a`'s
/// permission (every permission its predicate admits is for the atomic
/// its key names first): the permission's value, so a value the predicate
/// admits. It panics for a `Release` or `AcqRel` ordering.
///
/// It is what opening `inv` around a load through the permission would
/// prove; it takes the invariant rather than the permission because a
/// `Tracked` argument leaves a closure in the caller's exec code, which
/// changed the machine code of the `free` flush path (`Heap::flush`,
/// `group_by_slab`) that inlines the caller, `SlabTable::watermark`.
// The invariant is only named by the specification.
#[allow(unused_variables)]
#[inline(always)]
#[verifier::external_body]
pub fn load_usize_in<P: InvariantPredicate<(int, usize), UsizePerm>>(
    a: &AtomicUsize,
    ordering: Ordering,
    inv: &Tracked<AtomicInvariant<(int, usize), UsizePerm, P>>,
) -> (r: usize)
    requires
        inv@.constant().0 == usize_id(a),
        forall|p: UsizePerm| #[trigger] P::inv(inv@.constant(), p) ==> p.id() == inv@.constant().0,
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        exists|p: UsizePerm| #[trigger] P::inv(inv@.constant(), p) && p.value() == r,
    opens_invariants [inv@.namespace()]
    no_unwind
{
    a.load(ordering)
}

/// `a.store(v, ordering)` (it panics for an `Acquire` or `AcqRel`
/// ordering).
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn store_usize(a: &AtomicUsize, v: usize, ordering: Ordering, Tracked(perm): Tracked<&mut UsizePerm>)
    requires
        old(perm).id() == usize_id(a),
        !(ordering is Acquire),
        !(ordering is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    a.store(v, ordering)
}

} // verus!
