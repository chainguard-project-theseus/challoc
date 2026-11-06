//! Atomic pointers with a ghost permission, for the table entries of the
//! arena registry (`arena.rs`, `Arenas::table`) and of the explicit caches
//! (`heap/tcaches.rs`, `Tcaches::table`): each table's invariant
//! (`arena::NarenasPred`, `heap::tcaches::TablePred`) holds each entry's
//! permission, so a load made inside it reads the pointer the invariant
//! states facts about.
//!
//! The same pattern as `count.rs` for the arena count: an [`EntryPerm`]
//! names the atomic's identity and value, each operation is an
//! `#[verifier::atomic]` function (so it can run inside an
//! `open_atomic_invariant!` block) that passes the caller's ordering
//! through unchanged, and `#[inline(always)]` leaves the machine code of
//! the bare `core` call.
//!
//! Trusted, all of it:
//! - each wrapped `core` operation is one atomic operation on the pointer
//!   (a load, a store, a swap), as `core` documents;
//! - the proof model is sequentially consistent: the specifications ignore
//!   `ordering`, so nothing here proves that the orderings are strong
//!   enough;
//! - [`entry_id`] is the atomic's identity, and a permission is for the
//!   atomic with its [`EntryPerm::id`]. The identity is a ghost one, as
//!   vstd's `PAtomicPtr::id`: a function of the atomic's value, so fixed
//!   when `AtomicPtr::new` makes the atomic and kept when the atomic is
//!   moved into place (`Arenas::init`'s and `Tcaches::init`'s writes), not
//!   its address;
//! - atomics made by different calls have different identities: assumed,
//!   as no code makes two atomics Verus can prove equal (it knows nothing
//!   of `AtomicPtr::new`'s result); a `const` item holding [`new`]'s
//!   result would, each use of it copying one atomic and its permission;
//! - one permission per atomic: [`EntryPerm`]'s fields are private to this
//!   module and [`new`] is the only code that builds one, once, with the
//!   atomic it makes (`new` itself is verified);
//! - a permission's value is its atomic's: every write of a table entry
//!   goes through [`store`] or [`swap`] (`Arenas::put_arena0`,
//!   `Arenas::build`, `Tcaches::create`, `Tcaches::destroy`), inside its
//!   table's invariant once it exists, and `Arenas::init` and
//!   `Tcaches::init` make each entry with [`new`] before the table is
//!   published. Reads outside the invariant (`Arenas::assign`'s built
//!   test, the test-only `Arenas::iter`, and `Tcaches::create`'s search
//!   and `Tcaches::len`) are plain `core` loads, whose result Verus takes
//!   as any pointer.

use crate::sync::atomic::{AtomicPtr, Ordering};
use vstd::prelude::*;

verus! {

/// The identity of an atomic pointer: what an [`EntryPerm`] is for.
pub uninterp spec fn entry_id<T>(a: &AtomicPtr<T>) -> int;

/// Ghost permission to the atomic pointer with identity
/// [`id`](EntryPerm::id): its current [`value`](EntryPerm::value). It is
/// not `Copy`, and its fields are private to this module, which builds one
/// only in [`new`].
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct EntryPerm<T> {
    ghost id: int,
    ghost value: *mut T,
}

impl<T> EntryPerm<T> {
    /// The atomic this permission is for.
    pub closed spec fn id(&self) -> int {
        self.id
    }

    /// The atomic's value.
    pub closed spec fn value(&self) -> *mut T {
        self.value
    }
}

/// `a.store(v, ordering)` (it panics for an `Acquire` or `AcqRel`
/// ordering).
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn store<T>(a: &AtomicPtr<T>, v: *mut T, ordering: Ordering, Tracked(perm): Tracked<&mut EntryPerm<T>>)
    requires
        old(perm).id() == entry_id(a),
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

/// `a.load(ordering)`: the permission's value (it panics for a `Release`
/// or `AcqRel` ordering).
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn load<T>(a: &AtomicPtr<T>, ordering: Ordering, Tracked(perm): Tracked<&EntryPerm<T>>) -> (r: *mut T)
    requires
        perm.id() == entry_id(a),
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    a.load(ordering)
}

/// `a.swap(v, ordering)`: the permission's value before, which becomes
/// `v`.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn swap<T>(a: &AtomicPtr<T>, v: *mut T, ordering: Ordering, Tracked(perm): Tracked<&mut EntryPerm<T>>) -> (r: *mut T)
    requires
        old(perm).id() == entry_id(a),
    ensures
        r == old(perm).value(),
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    a.swap(v, ordering)
}

/// `AtomicPtr::new(v)`, with the one permission to it: the only place an
/// [`EntryPerm`] is minted, one per atomic, with the atomic it is for.
/// Not under `cfg(loom)`, whose `AtomicPtr::new` is not `const`.
#[cfg(not(loom))]
#[inline(always)]
pub const fn new<T>(v: *mut T) -> (r: (AtomicPtr<T>, Tracked<EntryPerm<T>>))
    ensures
        r.1@.id() == entry_id(&r.0),
        r.1@.value() == v,
{
    let a = AtomicPtr::new(v);
    (a, Tracked(EntryPerm { id: entry_id(&a), value: v }))
}

} // verus!

// SAFETY: a ghost permission: it lives only in a `Tracked` (a table's
// invariant, `arena::NarenasPred` or `heap::tcaches::TablePred`), which
// has no bytes at run time, so no pointer moves between threads with it.
// Over two lines: `verus/seams.sh` reads a one-line `impl ... {}` as an
// `impl` whose block opens at the next brace.
#[rustfmt::skip]
unsafe impl<T> Send for EntryPerm<T> {
}
