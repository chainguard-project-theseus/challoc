//! `u32` counters as atomics with a ghost permission, for the arena
//! registry's bound counts (`arena.rs`, `Arenas::bound`; `verus/THEOREMS.md`,
//! "Balanced arena assignment").
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
//!   (a load or a wrapping read-modify-write), as `core` documents;
//! - the proof model is sequentially consistent: the specifications ignore
//!   `ordering`, so nothing here proves that `Relaxed` is strong enough
//!   (`verus/TODO.md`, Tier 4);
//! - [`count_id`] is the counter's identity, and a permission is for the
//!   counter with its [`CountPerm::id`]. The permissions are minted once per
//!   registry, by `Arenas::init`, inside its invariants.
//!
//! A permission's value is its counter's: every access to a bound count
//! goes through these wrappers (`Arenas::assign`, `bind`, `unbind`,
//! `bound`), each inside the counter's invariant (`arena::BoundPred`),
//! and `Arenas::init` writes it before the registry is published.

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
/// its current [`value`](CountPerm::value). It is not `Copy`.
#[verifier::external_body]
pub tracked struct CountPerm {
    _no_copy: (),
}

impl CountPerm {
    /// The counter this permission is for.
    pub uninterp spec fn id(&self) -> int;

    /// The counter's value.
    pub uninterp spec fn value(&self) -> u32;
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

/// `c.load(ordering)`: the permission's value.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn load(c: &AtomicU32, ordering: Ordering, Tracked(perm): Tracked<&CountPerm>) -> (r: u32)
    requires
        perm.id() == count_id(c),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    c.load(ordering)
}

} // verus!
