//! `u32` counters as vstd permissioned atomics, for the arena registry's
//! bound counts (`arena.rs`, `Arenas::bound`) and its arena count
//! (`Arenas::count`), for the cgroup policy's position in the scaled band
//! (`policy.rs`, `Policy::position`), and for the allocator's init state
//! (`api/mod.rs`, `STATE.word`).
//!
//! Each counter is a vstd `PAtomicU32`, and its [`CountPerm`] is vstd's
//! `PermissionU32`: the counter's identity and value. Each operation is
//! an `#[verifier::atomic]` function over the one vstd operation (so it
//! can run inside an `open_atomic_invariant!` block), and
//! `#[inline(always)]` leaves the machine code of that call. vstd's
//! operations are `SeqCst`. The trusted part is vstd's: each
//! `PAtomicU32` operation is one atomic operation on the counter, under a
//! sequentially consistent model, `PAtomicU32::new` mints the one
//! permission to the atomic it makes, and atomics have distinct
//! identities.
//!
//! A permission's value is its counter's: every access to a bound count
//! or the arena count goes through these wrappers (`Arenas::assign`,
//! `bind`, `try_bind_idle`, `unbind`, `bound`, `count`, `create`), each
//! inside the counter's invariant once the registry is built
//! (`arena::BoundPred`, `arena::NarenasPred`), and `Arenas::init` writes
//! each counter before the registry is published. The policy's position
//! goes through [`load`] and [`swap`] only, inside its invariant
//! (`policy::FracPred`), which `Policy::new` builds with the permission
//! [`new`] returns. `STATE.word` goes through [`load`],
//! [`compare_exchange`] and [`store`] only (`api::load_state`,
//! `load_state_marked`, `claim_init`, `publish_ready`), inside `STATE`'s
//! invariant (`api::StatePred`), which `STATE`'s initialiser builds with
//! the permission [`new`] returns.

use crate::sync::patomic::{PAtomicU32, PermissionU32};
use vstd::prelude::*;

verus! {

/// Ghost permission to a counter: vstd's `PermissionU32`, whose `id()` is
/// the counter's [`count_id`] and whose `value()` is the counter's value.
pub type CountPerm = PermissionU32;

/// The identity of an atomic counter: what a [`CountPerm`] is for.
pub open spec fn count_id(c: &PAtomicU32) -> int {
    c.id()
}

/// `x` reduced to a `u32` the way the hardware wraps it.
pub open spec fn wrap32(x: int) -> u32 {
    (x % 0x1_0000_0000) as u32
}

/// `c.fetch_add(v, SeqCst)`, wrapping.
#[inline(always)]
#[verifier::atomic]
pub fn fetch_add(c: &PAtomicU32, v: u32, Tracked(perm): Tracked<&mut CountPerm>) -> (prev: u32)
    requires
        old(perm).id() == count_id(c),
    ensures
        final(perm).id() == old(perm).id(),
        prev == old(perm).value(),
        final(perm).value() == wrap32(prev + v),
    opens_invariants none
    no_unwind
{
    c.fetch_add_wrapping(Tracked(perm), v)
}

/// `c.fetch_sub(v, SeqCst)`, wrapping.
#[inline(always)]
#[verifier::atomic]
pub fn fetch_sub(c: &PAtomicU32, v: u32, Tracked(perm): Tracked<&mut CountPerm>) -> (prev: u32)
    requires
        old(perm).id() == count_id(c),
    ensures
        final(perm).id() == old(perm).id(),
        prev == old(perm).value(),
        final(perm).value() == wrap32(prev - v),
    opens_invariants none
    no_unwind
{
    c.fetch_sub_wrapping(Tracked(perm), v)
}

/// `c.store(v, SeqCst)`.
#[inline(always)]
#[verifier::atomic]
pub fn store(c: &PAtomicU32, v: u32, Tracked(perm): Tracked<&mut CountPerm>)
    requires
        old(perm).id() == count_id(c),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    c.store(Tracked(perm), v)
}

/// `c.load(SeqCst)`: the permission's value.
#[inline(always)]
#[verifier::atomic]
pub fn load(c: &PAtomicU32, Tracked(perm): Tracked<&CountPerm>) -> (r: u32)
    requires
        perm.id() == count_id(c),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    c.load(Tracked(perm))
}

/// `c.swap(v, SeqCst)`: the previous value.
#[inline(always)]
#[verifier::atomic]
pub fn swap(c: &PAtomicU32, v: u32, Tracked(perm): Tracked<&mut CountPerm>) -> (prev: u32)
    requires
        old(perm).id() == count_id(c),
    ensures
        final(perm).id() == old(perm).id(),
        prev == old(perm).value(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    c.swap(Tracked(perm), v)
}

/// `PAtomicU32::new(v)`, with the one permission to it, which only vstd's
/// `new` mints. It must not appear in a `const` item, each use of which
/// would be a new atomic with the same permission. Not under `cfg(loom)`,
/// whose atomics are not built in `const`.
#[cfg(not(loom))]
#[inline(always)]
pub const fn new(v: u32) -> (r: (PAtomicU32, Tracked<CountPerm>))
    ensures
        r.1@.id() == count_id(&r.0),
        r.1@.value() == v,
{
    PAtomicU32::new(v)
}

/// `c.compare_exchange(current, new, SeqCst, SeqCst)`: `Ok` with the
/// previous value, `current`, when the counter held it and now holds
/// `new`; otherwise `Err` with the value it holds, unchanged (the strong
/// form never fails spuriously).
#[inline(always)]
#[verifier::atomic]
pub fn compare_exchange(
    c: &PAtomicU32,
    current: u32,
    new: u32,
    Tracked(perm): Tracked<&mut CountPerm>,
) -> (r: Result<u32, u32>)
    requires
        old(perm).id() == count_id(c),
    ensures
        final(perm).id() == old(perm).id(),
        r matches Ok(prev) ==> prev == current && old(perm).value() == current && final(perm).value() == new,
        r matches Err(prev) ==> prev == old(perm).value() && prev != current && final(perm).value() == prev,
    opens_invariants none
    no_unwind
{
    c.compare_exchange(Tracked(perm), current, new)
}

} // verus!
