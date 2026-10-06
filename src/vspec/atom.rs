//! `usize` atomics with a ghost permission, for the atomic invariants
//! that bound a value challoc only ever stores in range (`verus/TODO.md`,
//! "Coverage ceiling", ATOM): a class table's hand-out watermark
//! (`class/table.rs`, `committed_slabs`) and its copy in the class's line
//! (`heap::ClassLine`, stored through a pointer). The `u32` values (`policy.rs`'s
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
//!   (a load or a store), as `core` documents;
//! - the proof model is sequentially consistent: the specifications ignore
//!   `ordering`, so nothing here proves that `Relaxed`, `Acquire` and
//!   `Release` are strong enough (`verus/TODO.md`, Tier 4); the invariants
//!   built on these only bound values, and every store writes a value in
//!   bounds, so a load returns an in-bounds value under any ordering;
//! - [`usize_id`] is the atomic's identity, and a permission is for the
//!   atomic with its `id`: a ghost identity carried by the atomic's value,
//!   kept when it is moved into place (`SlabTable::new`'s struct literal),
//!   not its address, as `count::count_id`;
//! - atomics made by different calls have different identities: assumed,
//!   as no code makes two atomics Verus can prove equal (it knows nothing
//!   of `AtomicUsize::new`'s result; [`new_usize`] is not `const`, so no
//!   `const` item can copy one);
//! - one permission per atomic: [`UsizePerm`]'s fields are private to this
//!   module and [`new_usize`] is the only code that builds one, once, with
//!   the atomic it makes (`verus/loom/run.sh` checks it by name;
//!   `new_usize` itself is verified);
//! - a permission's value is its atomic's: every store to an atomic that
//!   has one goes through these wrappers, inside the atomic's invariant
//!   (`SlabTable::take_group`; the class lines' watermark copies are stored
//!   by `take_group` and `SlabTable::mirror_watermark`, through `store_usize_at`;
//!   `verus/loom/run.sh` checks it by name);
//! - [`usize_id_at`] names the atomic a pointer from [`usize_ptr`] was
//!   made from, for as long as it stays in place: the class lines' copies
//!   live in the `Shared`, which never moves (`Shared::init`'s contract).

use crate::sync::atomic::{AtomicUsize, Ordering};
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
use vstd::prelude::*;

verus! {

/// The identity of an atomic `usize`: what a [`UsizePerm`] is for.
pub uninterp spec fn usize_id(a: &AtomicUsize) -> int;

/// Ghost permission to the atomic `usize` with identity
/// [`id`](UsizePerm::id): its current [`value`](UsizePerm::value). It is
/// not `Copy`, and its fields are private to this module, which builds one
/// only in [`new_usize`].
pub tracked struct UsizePerm {
    ghost id: int,
    ghost value: usize,
}

impl UsizePerm {
    /// The atomic this permission is for.
    pub closed spec fn id(&self) -> int {
        self.id
    }

    /// The atomic's value.
    pub closed spec fn value(&self) -> usize {
        self.value
    }
}

/// `AtomicUsize::new(v)`, with the one permission to it: the only place a
/// [`UsizePerm`] is minted, one per atomic, with the atomic it is for.
#[inline(always)]
pub fn new_usize(v: usize) -> (r: (AtomicUsize, Tracked<UsizePerm>))
    ensures
        r.1@.id() == usize_id(&r.0),
        r.1@.value() == v,
{
    let a = AtomicUsize::new(v);
    (a, Tracked(UsizePerm { id: usize_id(&a), value: v }))
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

/// The identity ([`usize_id`]) of the atomic `usize` at `p`: what a
/// pointer [`usize_ptr`] made from a reference to it points at.
pub uninterp spec fn usize_id_at(p: *const AtomicUsize) -> int;

/// `a as *const AtomicUsize`: a pointer to the atomic, which names it
/// ([`usize_id_at`]). Trusted: Verus has no reference-to-pointer cast.
/// The pointer names `a` for as long as `a` stays where it is, which is
/// the caller's to keep (`SlabTable::mirror_watermark`'s safety contract:
/// the class line it points into is in a `Shared` that never moves).
#[inline(always)]
#[verifier::external_body]
pub fn usize_ptr(a: &AtomicUsize) -> (r: *const AtomicUsize)
    ensures
        usize_id_at(r) == usize_id(a),
    opens_invariants none
    no_unwind
{
    a
}

/// `(*p).store(v, ordering)` through a pointer from [`usize_ptr`], with
/// the permission of the atomic it names (it panics for an `Acquire` or
/// `AcqRel` ordering).
///
/// # Safety
/// `p` points to a live `AtomicUsize`: the one it was made from, still
/// in place.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub unsafe fn store_usize_at(p: *const AtomicUsize, v: usize, ordering: Ordering, Tracked(perm): Tracked<&mut UsizePerm>)
    requires
        old(perm).id() == usize_id_at(p),
        !(ordering is Acquire),
        !(ordering is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    // SAFETY: guaranteed by the caller.
    unsafe { (*p).store(v, ordering) }
}

} // verus!
