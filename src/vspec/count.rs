//! `u32` counters as atomics with a ghost permission, for the arena
//! registry's bound counts (`arena.rs`, `Arenas::bound`; `verus/THEOREMS.md`,
//! "Balanced arena assignment") and its arena count (`Arenas::count`),
//! and for the policy's position (`policy.rs`, `Policy::position`), a
//! `u32` value an atomic invariant bounds (`verus/TODO.md`, "Coverage
//! ceiling", ATOM).
//!
//! The same pattern as `word.rs` for the class bitmap words: a
//! [`CountPerm`] names the counter's identity and value, each operation is
//! an `#[verifier::atomic]` function (so it can run inside an
//! `open_atomic_invariant!` block) that passes the caller's ordering
//! through unchanged, and `#[inline(always)]` leaves the machine code of
//! the bare `core` call.
//!
//! Trusted, all of it:
//! - each wrapped `core` operation is one atomic operation on the counter
//!   (a load, a store, a swap, a strong compare-and-swap or a wrapping
//!   read-modify-write), as `core` documents;
//! - the proof model is sequentially consistent: the specifications ignore
//!   `ordering`, so nothing here proves that `Relaxed` is strong enough
//!   (`verus/TODO.md`, Tier 4);
//! - [`count_id`] is the counter's identity, and a permission is for the
//!   counter with its [`CountPerm::id`]. The identity is a ghost one, as
//!   vstd's `PAtomicU32::id`: a function of the atomic's value, so fixed
//!   when `AtomicU32::new` makes the atomic and kept when the atomic is
//!   moved into place (`Arenas::init`'s `ptr::write`, `Policy::new`'s
//!   struct literal), not its address;
//! - atomics made by different calls have different identities: assumed,
//!   as no code makes two atomics Verus can prove equal (it knows nothing
//!   of `AtomicU32::new`'s result); a `const` item holding [`new`]'s or
//!   `Policy::new`'s result would, each use of it copying one atomic and
//!   its permission (`verus/loom/run.sh` checks by name that none does);
//! - one permission per counter: [`CountPerm`]'s fields are private to
//!   this module and [`new`] is the only code that builds one, once, with
//!   the atomic it makes (`verus/loom/run.sh` checks it by name; `new`
//!   itself is verified);
//! - a permission's value is its counter's: every access to a bound count
//!   goes through these wrappers (`Arenas::assign`, `bind`,
//!   `try_bind_idle`, `unbind`, `bound`), each inside the counter's
//!   invariant (`arena::BoundPred`),
//!   and so does every access to the arena count (`Arenas::count`,
//!   `Arenas::create`) inside its invariant (`arena::NarenasPred`), and to
//!   the position (`Policy::position`, `Policy::apply`) inside its
//!   (`policy::FracPred`); `Arenas::init` writes each counter of the
//!   registry before the registry is published (`verus/loom/run.sh`
//!   checks the accesses by name).
//!
//! `Arenas::init` keeps the permissions of the bound counts and the arena
//! count inside its invariants, and `Policy::new` that of the position
//! inside its invariant.

use crate::sync::atomic::{AtomicU32, Ordering};
use vstd::prelude::*;

verus! {

/// The identity of an atomic counter: what a [`CountPerm`] is for.
pub uninterp spec fn count_id(c: &AtomicU32) -> int;

/// `x` reduced to a `u32` the way the hardware wraps it.
pub open spec fn wrap32(x: int) -> u32 {
    (x % 0x1_0000_0000) as u32
}

/// Ghost permission to the counter with identity [`id`](CountPerm::id):
/// its current [`value`](CountPerm::value). It is not `Copy`, and its
/// fields are private to this module, which builds one only in [`new`].
pub tracked struct CountPerm {
    ghost id: int,
    ghost value: u32,
}

impl CountPerm {
    /// The counter this permission is for.
    pub closed spec fn id(&self) -> int {
        self.id
    }

    /// The counter's value.
    pub closed spec fn value(&self) -> u32 {
        self.value
    }
}

/// `c.fetch_add(v, ordering)`, wrapping.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn fetch_add(c: &AtomicU32, v: u32, ordering: Ordering, Tracked(perm): Tracked<&mut CountPerm>) -> (prev: u32)
    requires
        old(perm).id() == count_id(c),
    ensures
        final(perm).id() == old(perm).id(),
        prev == old(perm).value(),
        final(perm).value() == wrap32(prev + v),
    opens_invariants none
    no_unwind
{
    c.fetch_add(v, ordering)
}

/// `c.fetch_sub(v, ordering)`, wrapping.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn fetch_sub(c: &AtomicU32, v: u32, ordering: Ordering, Tracked(perm): Tracked<&mut CountPerm>) -> (prev: u32)
    requires
        old(perm).id() == count_id(c),
    ensures
        final(perm).id() == old(perm).id(),
        prev == old(perm).value(),
        final(perm).value() == wrap32(prev - v),
    opens_invariants none
    no_unwind
{
    c.fetch_sub(v, ordering)
}

/// `c.store(v, ordering)` (it panics for an `Acquire` or `AcqRel`
/// ordering).
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn store(c: &AtomicU32, v: u32, ordering: Ordering, Tracked(perm): Tracked<&mut CountPerm>)
    requires
        old(perm).id() == count_id(c),
        !(ordering is Acquire),
        !(ordering is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    c.store(v, ordering)
}

/// `c.load(ordering)`: the permission's value (it panics for a `Release`
/// or `AcqRel` ordering).
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn load(c: &AtomicU32, ordering: Ordering, Tracked(perm): Tracked<&CountPerm>) -> (r: u32)
    requires
        perm.id() == count_id(c),
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    c.load(ordering)
}

/// `AtomicU32::new(v)`, with the one permission to it: the only place a
/// [`CountPerm`] is minted, one per counter, with the counter it is for.
/// It is `const` for `Policy::new`, which runs at run time: neither may
/// appear in a `const` item, each use of which would be a new atomic with
/// the same permission. Not under `cfg(loom)`, whose `AtomicU32::new` is
/// not `const` (the loom models make their counters directly).
#[cfg(not(loom))]
#[inline(always)]
pub const fn new(v: u32) -> (r: (AtomicU32, Tracked<CountPerm>))
    ensures
        r.1@.id() == count_id(&r.0),
        r.1@.value() == v,
{
    let c = AtomicU32::new(v);
    (c, Tracked(CountPerm { id: count_id(&c), value: v }))
}

/// `c.swap(v, ordering)`: the previous value.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn swap(c: &AtomicU32, v: u32, ordering: Ordering, Tracked(perm): Tracked<&mut CountPerm>) -> (prev: u32)
    requires
        old(perm).id() == count_id(c),
    ensures
        final(perm).id() == old(perm).id(),
        prev == old(perm).value(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    c.swap(v, ordering)
}

/// `c.compare_exchange(current, new, success, failure)`: `Ok` with the
/// previous value, `current`, when the counter held it and now holds
/// `new`; otherwise `Err` with the value it holds, unchanged (the strong
/// form never fails spuriously). It panics for a `Release` or `AcqRel`
/// failure ordering.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn compare_exchange(
    c: &AtomicU32,
    current: u32,
    new: u32,
    success: Ordering,
    failure: Ordering,
    Tracked(perm): Tracked<&mut CountPerm>,
) -> (r: Result<u32, u32>)
    requires
        old(perm).id() == count_id(c),
        !(failure is Release),
        !(failure is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        r matches Ok(prev) ==> prev == current && old(perm).value() == current && final(perm).value() == new,
        r matches Err(prev) ==> prev == old(perm).value() && prev != current && final(perm).value() == prev,
    opens_invariants none
    no_unwind
{
    c.compare_exchange(current, new, success, failure)
}

} // verus!
