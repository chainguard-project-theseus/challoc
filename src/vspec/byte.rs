//! `u8` state bytes as atomics with a ghost permission, for the phase of
//! the deferred cgroup record (`sys::cgroup::Deferred`, `PENDING` to
//! `RUNNING` to `DONE`), whose invariant hands the cell's permission to
//! the exchange's winner and freezes it at the `DONE` store
//! (`sys::cgroup::PhasePred`), and for `double_free_scan`'s slot-state
//! bytes (`heap::slotstate::step`).
//!
//! A [`BytePerm`] names the byte's identity and value, each operation is
//! an `#[verifier::atomic]` function (so it can run inside an
//! `open_atomic_invariant!` block) that passes the caller's ordering
//! through unchanged, and `#[inline(always)]` leaves the machine code of
//! the bare `core` call. The phase's other load (`Deferred::due`'s
//! `Relaxed` one) stays a plain `core` load: vstd specifies its result
//! as any value, which transfers nothing; a waiting `Deferred::discover`
//! loads through `Deferred::load_phase`, as `Deferred::get` does.
//!
//! Trusted, all of it:
//! - each wrapped `core` operation is one atomic operation on the byte (a
//!   load, a store or a strong compare-and-swap), as `core` documents;
//! - the proof model is sequentially consistent: the specifications ignore
//!   `ordering`, so nothing here proves that `Acquire`, `Release` and
//!   `Relaxed` are strong enough;
//! - [`byte_id`] is the byte's identity, and a permission is for the byte
//!   with its [`BytePerm::id`]. The identity is a ghost one, as vstd's
//!   `PAtomicU8::id`: a function of the atomic's value, so fixed when
//!   `AtomicU8::new` makes the atomic and kept when the atomic is moved
//!   into place (`Deferred::now` and `Deferred::later`'s struct literals,
//!   then `Shared::put_cgroup`'s `ptr::write`), not its address;
//! - atomics made by different calls have different identities: assumed,
//!   as no code makes two atomics Verus can prove equal (it knows nothing
//!   of `AtomicU8::new`'s result); a `const` item holding [`new`]'s or
//!   `Deferred::later`'s result would, each use of it copying one atomic
//!   and its permission, and none does;
//! - one permission per byte: [`BytePerm`]'s fields are private to this
//!   module and [`new`] is the only code here that builds one, once, with
//!   the atomic it makes (`new` itself is verified). The slot-state bytes
//!   are raw mapped memory no `new` made: `heap::slotstate::axiom_slot_byte`
//!   mints a permission to one for each step on it, called only there
//!   (its own premise, that the value is the byte's);
//! - a permission's value is its byte's: every write of a deferred
//!   record's phase goes through these wrappers (`Deferred::discover`,
//!   `Deferred::fork_child`), each inside the phase's invariant
//!   (`sys::cgroup::PhasePred`), as do `Deferred::get`'s and a waiting
//!   `Deferred::discover`'s loads (`Deferred::load_phase`); every access
//!   to a slot-state byte is one of these wrappers in
//!   `heap::slotstate::step`.

use core::sync::atomic::{AtomicU8, Ordering};
use vstd::prelude::*;

verus! {

/// The identity of an atomic byte: what a [`BytePerm`] is for.
pub uninterp spec fn byte_id(b: &AtomicU8) -> int;

/// Ghost permission to the byte with identity [`id`](BytePerm::id): its
/// current [`value`](BytePerm::value). It is not `Copy`, and its fields
/// are private to this module, which builds one only in [`new`].
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct BytePerm {
    ghost id: int,
    ghost value: u8,
}

impl BytePerm {
    /// The byte this permission is for.
    pub closed spec fn id(&self) -> int {
        self.id
    }

    /// The byte's value.
    pub closed spec fn value(&self) -> u8 {
        self.value
    }
}

/// `AtomicU8::new(v)`, with the one permission to it: the only place in
/// this module a [`BytePerm`] is minted, one per byte, with the byte it is
/// for. It is
/// `const` for `Deferred::later`, which runs at run time: neither may
/// appear in a `const` item, each use of which would be a new atomic with
/// the same permission.
#[inline(always)]
pub const fn new(v: u8) -> (r: (AtomicU8, Tracked<BytePerm>))
    ensures
        r.1@.id() == byte_id(&r.0),
        r.1@.value() == v,
{
    let b = AtomicU8::new(v);
    (b, Tracked(BytePerm { id: byte_id(&b), value: v }))
}

/// `b.load(ordering)`: the permission's value (it panics for a `Release`
/// or `AcqRel` ordering).
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn load(b: &AtomicU8, ordering: Ordering, Tracked(perm): Tracked<&BytePerm>) -> (r: u8)
    requires
        perm.id() == byte_id(b),
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    b.load(ordering)
}

/// `b.store(v, ordering)` (it panics for an `Acquire` or `AcqRel`
/// ordering).
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn store(b: &AtomicU8, v: u8, ordering: Ordering, Tracked(perm): Tracked<&mut BytePerm>)
    requires
        old(perm).id() == byte_id(b),
        !(ordering is Acquire),
        !(ordering is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    b.store(v, ordering)
}

/// `b.compare_exchange(current, new, success, failure)`: `Ok` with the
/// previous value, `current`, when the byte held it and now holds `new`;
/// otherwise `Err` with the value it holds, unchanged (the strong form
/// never fails spuriously). It panics for a `Release` or `AcqRel` failure
/// ordering.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn compare_exchange(
    b: &AtomicU8,
    current: u8,
    new: u8,
    success: Ordering,
    failure: Ordering,
    Tracked(perm): Tracked<&mut BytePerm>,
) -> (r: Result<u8, u8>)
    requires
        old(perm).id() == byte_id(b),
        !(failure is Release),
        !(failure is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        r matches Ok(prev) ==> prev == current && old(perm).value() == current && final(perm).value() == new,
        r matches Err(prev) ==> prev == old(perm).value() && prev != current && final(perm).value() == prev,
    opens_invariants none
    no_unwind
{
    b.compare_exchange(current, new, success, failure)
}

} // verus!
