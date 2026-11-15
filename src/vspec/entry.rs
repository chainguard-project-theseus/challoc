//! Atomic pointers as vstd permissioned atomics, for the table entries of
//! the arena registry (`arena.rs`, `Arenas::table`) and of the explicit
//! caches (`heap/tcaches.rs`, `Tcaches::table`): each table's invariant
//! (`arena::NarenasPred`, `heap::tcaches::TablePred`) holds each entry's
//! permission, so a load made inside it reads the pointer the invariant
//! states facts about.
//!
//! Each entry is a vstd `PAtomicPtr`, and its [`EntryPerm`] holds vstd's
//! `PermissionPtr`: the entry's identity and value. Each operation is an
//! `#[verifier::atomic]` function over the one vstd operation (so it can
//! run inside an `open_atomic_invariant!` block), and `#[inline(always)]`
//! leaves the machine code of that call. vstd's operations are `SeqCst`.
//! The trusted part is vstd's: each `PAtomicPtr` operation is one atomic
//! operation on the pointer, under a sequentially consistent model,
//! `PAtomicPtr::new` mints the one permission to the atomic it makes, and
//! atomics have distinct identities.
//!
//! A permission's value is its entry's: every access to a table entry
//! goes through these wrappers, inside its table's invariant once the
//! table exists (`Arenas::put_arena0`, `Arenas::build`, the registry's
//! loads, `Tcaches::create`, `get`, `destroy`, `len`), and `Arenas::init`
//! and `Tcaches::init` make each entry with [`new`] before the table is
//! published.

use crate::sync::patomic::{PAtomicPtr, PermissionPtr};
use vstd::prelude::*;

verus! {

/// The identity of an atomic pointer: what an [`EntryPerm`] is for.
pub open spec fn entry_id<T>(a: &PAtomicPtr<T>) -> int {
    a.id()
}

/// Ghost permission to the atomic pointer with identity
/// [`id`](EntryPerm::id): its current [`value`](EntryPerm::value). vstd's
/// `PermissionPtr`, which is not `Send` (it names a raw pointer type), in
/// a value a table's invariant can hold.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct EntryPerm<T> {
    tracked p: PermissionPtr<T>,
}

impl<T> EntryPerm<T> {
    /// The atomic this permission is for.
    pub closed spec fn id(&self) -> int {
        self.p.id()
    }

    /// The atomic's value.
    pub closed spec fn value(&self) -> *mut T {
        self.p.value()
    }
}

/// `a.store(v, SeqCst)`.
#[inline(always)]
#[verifier::atomic]
pub fn store<T>(a: &PAtomicPtr<T>, v: *mut T, Tracked(perm): Tracked<&mut EntryPerm<T>>)
    requires
        old(perm).id() == entry_id(a),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    a.store(Tracked(&mut perm.p), v)
}

/// `a.load(SeqCst)`: the permission's value.
#[inline(always)]
#[verifier::atomic]
pub fn load<T>(a: &PAtomicPtr<T>, Tracked(perm): Tracked<&EntryPerm<T>>) -> (r: *mut T)
    requires
        perm.id() == entry_id(a),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    a.load(Tracked(&perm.p))
}

/// `a.swap(v, SeqCst)`: the permission's value before, which becomes
/// `v`.
#[inline(always)]
#[verifier::atomic]
pub fn swap<T>(a: &PAtomicPtr<T>, v: *mut T, Tracked(perm): Tracked<&mut EntryPerm<T>>) -> (r: *mut T)
    requires
        old(perm).id() == entry_id(a),
    ensures
        r == old(perm).value(),
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    a.swap(Tracked(&mut perm.p), v)
}

/// `PAtomicPtr::new(v)`, with the one permission to it, which only vstd's
/// `new` mints. It must not appear in a `const` item, each use of which
/// would be a new atomic with the same permission. Not under `cfg(loom)`,
/// whose atomics are not built in `const`.
#[cfg(not(loom))]
#[inline(always)]
pub const fn new<T>(v: *mut T) -> (r: (PAtomicPtr<T>, Tracked<EntryPerm<T>>))
    ensures
        r.1@.id() == entry_id(&r.0),
        r.1@.value() == v,
{
    let (a, Tracked(p)) = PAtomicPtr::new(v);
    (a, Tracked(EntryPerm { p }))
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
