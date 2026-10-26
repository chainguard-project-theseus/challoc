//! `usize` atomics with a ghost permission, for the atomic invariants
//! that bound a value challoc only ever stores in range: a class table's
//! hand-out watermark (`class/table.rs`, `committed_slabs`) and its copy
//! in the class's line (`heap::ClassLine`, stored through a pointer); and
//! the load of an atomic `u16` through its `PointsTo` (a class table's
//! owner words).
//!
//! A permission names the atomic's identity and value, as `word.rs`'s
//! do a word's; the stores and the `u16` load are `#[verifier::atomic]`
//! functions (so they can run inside an `open_atomic_invariant!` block),
//! the watermark load ([`load_usize_seen`]) opens the invariant that holds
//! the permission itself, each passes the caller's ordering through
//! unchanged, and `#[inline(always)]` leaves the machine code of the bare
//! `core` call.
//!
//! Trusted:
//! - each wrapped `core` operation is one atomic operation on the value
//!   (a load or a store), as `core` documents;
//! - the proof model is sequentially consistent: the specifications ignore
//!   `ordering`, so nothing here proves that `Relaxed`, `Acquire` and
//!   `Release` are strong enough. The invariants built on these bound values,
//!   and every store writes a value in bounds, so a load returns an
//!   in-bounds value under any ordering; the one that also hands out a
//!   persistent set ([`load_usize_seen`]: a class table's watermark, with
//!   the owner words stored below it) is sound only for a load that
//!   happens after the store that published the set, so it requires an
//!   `Acquire` load, which pairs with `take_group`'s `Release` store;
//! - [`usize_id`] is the atomic's identity, and a permission is for the
//!   atomic with its `id`: a ghost identity carried by the atomic's value,
//!   kept when it is moved into place (`SlabTable::new`'s struct literal),
//!   not its address;
//! - atomics made by different calls have different identities: assumed,
//!   as no code makes two atomics Verus can prove equal (it knows nothing
//!   of `AtomicUsize::new`'s result; [`new_usize`] is not `const`, so no
//!   `const` item can copy one);
//! - one permission per atomic: [`UsizePerm`]'s fields are private to this
//!   module and [`new_usize`] is the only code that builds one, once, with
//!   the atomic it makes (`new_usize` itself is verified);
//! - a permission's value is its atomic's: every store to an atomic that
//!   has one goes through these wrappers (`SlabTable::take_group`'s
//!   stores of the watermark and of its copy in the class line, and
//!   `SlabTable::mirror_watermark`'s of the copy, through
//!   [`store_usize_at`]); vstd's own specification of
//!   `AtomicUsize::store` would let verified code store without the
//!   permission, and none does. The class lines' copies are made with
//!   `AtomicUsize::new` today, so their permissions and invariants are
//!   premises of `mirror_watermark`'s caller (`Shared::init`, unverified);
//! - [`usize_id_at`] names the atomic a pointer points at, for as long as
//!   it stays in place: the class lines' copies live in the `Shared`,
//!   which never moves (`Shared::init`'s contract).

use core::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
use vstd::prelude::*;
use vstd::raw_ptr::PointsTo;
use vstd::resource::set::GhostPersistentSubset;

verus! {

/// The identity of an atomic `usize`: what a [`UsizePerm`] is for.
pub uninterp spec fn usize_id(a: &AtomicUsize) -> int;

/// Ghost permission to the atomic `usize` with identity
/// [`id`](UsizePerm::id): its current [`value`](UsizePerm::value). It is
/// not `Copy`, and its fields are private to this module, which builds one
/// only in [`new_usize`].
// Ghost: built here, for the specifications of the watermark.
#[allow(dead_code)]
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
pub fn new_usize(v: usize) -> (r: (AtomicUsize, Tracked<UsizePerm>)) {
    let a = AtomicUsize::new(v);
    (a, Tracked(UsizePerm { id: usize_id(&a), value: v }))
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

/// The identity ([`usize_id`]) of the atomic `usize` at `p`.
pub uninterp spec fn usize_id_at(p: *const AtomicUsize) -> int;

/// `(*p).store(v, ordering)` through a pointer to an atomic, with the
/// permission of the atomic it names (it panics for an `Acquire` or
/// `AcqRel` ordering).
///
/// # Safety
/// `p` points to a live `AtomicUsize`, still in place.
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

/// What the invariant of a [`load_usize_seen`] atomic holds: the atomic's
/// permission and a persistent set, the record of what a reader that
/// loaded the value may rely on (for a class table's watermark, the
/// owner words stored below it: `class::table::WatermarkPred`).
pub type SeenVal = (UsizePerm, GhostPersistentSubset<int>);

/// `a.load(ordering)` inside `inv`, an invariant that holds `a`'s
/// permission with a persistent set ([`SeenVal`]): the permission's value,
/// so a value the predicate admits, and a copy of the set the invariant
/// held with it, frozen. The ordering is `Acquire`: the set records what
/// the writer did before its `Release` store of the value (the owner words
/// `take_group` stores before the watermark), which a weaker load need not
/// observe.
///
/// It is what opening `inv` around a load through the permission, then
/// duplicating the set (`GhostPersistentSubset::duplicate`) and freezing
/// the copy (`vstd::modes::tracked_static_ref`) would prove: the set is
/// persistent, so a copy taken with the load stays true after it. It takes
/// the invariant rather than the permission, as a load outside an
/// `open_atomic_invariant!` block.
// The invariant is only named by the specification.
#[allow(unused_variables)]
#[inline(always)]
#[verifier::external_body]
pub fn load_usize_seen<K, P: InvariantPredicate<K, SeenVal>>(
    a: &AtomicUsize,
    ordering: Ordering,
    inv: &Tracked<AtomicInvariant<K, SeenVal, P>>,
) -> (r: (usize, Tracked<&'static GhostPersistentSubset<int>>))
    requires
        forall|v: SeenVal| #[trigger] P::inv(inv@.constant(), v) ==> v.0.id() == usize_id(a),
        ordering is Acquire,
    ensures
        exists|v: SeenVal| #[trigger] P::inv(inv@.constant(), v) && v.0.value() == r.0
            && r.1@.id() == v.1.id() && r.1@@ == v.1@,
    opens_invariants [inv@.namespace()]
    no_unwind
{
    (a.load(ordering), Tracked::assume_new())
}

/// The value of an atomic `u16` as its permission holds it: what a load
/// through the permission returns ([`load_u16_at`]). Uninterpreted: the
/// trusted wrappers that read or write a `u16` atomic through its
/// `PointsTo` are all that state it (the class tables' owner words: the
/// load here, the one store `class::table::store_owner`).
pub uninterp spec fn u16_value(a: AtomicU16) -> u16;

/// `(*p).load(ordering)` through `perm`, the permission to the atomic at
/// `p`: the value the permission holds ([`u16_value`]). It panics for a
/// `Release` or `AcqRel` ordering. Trusted, as the wrappers above: one
/// atomic load (`core`'s), and every store to an atomic read here goes
/// through a store that updates the permission (the owner words':
/// `class::table::store_owner`, trusted itself; the test helper
/// `SlabTable::corrupt_owner_word` aside, and vstd's own specification of
/// `AtomicU16::store`, which would let verified code store through a
/// borrowed permission, unused for these words), so the value the
/// permission holds is the atomic's.
///
/// # Safety
/// `p` points to the live atomic `perm` is to.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub(crate) unsafe fn load_u16_at(p: *const AtomicU16, ordering: Ordering, Tracked(perm): Tracked<&PointsTo<AtomicU16>>) -> (r: u16)
    requires
        perm.ptr() == p,
        perm.is_init(),
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r == u16_value(perm.value()),
    opens_invariants none
    no_unwind
{
    // SAFETY: guaranteed by the caller.
    unsafe { (*p).load(ordering) }
}

} // verus!
