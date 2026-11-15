//! `u8` state bytes as vstd permissioned atomics, for the phase of the
//! deferred cgroup record (`sys::cgroup::Deferred`, `PENDING` to
//! `RUNNING` to `DONE`), whose invariant hands the cell's permission to
//! the exchange's winner and freezes it at the `DONE` store
//! (`sys::cgroup::PhasePred`), for `double_free_scan`'s slot-state
//! bytes (`heap::slotstate::step`), and for the feature line's state
//! (`api/mod.rs`, `STATE.line`, `LINE_UNBUILT` to `LINE_BUILDING` to
//! `LINE_READY`), whose invariant does the same for the line's statics
//! (`api::LinePred`).
//!
//! Each byte is a vstd `PAtomicU8`, and its [`BytePerm`] is vstd's
//! `PermissionU8`: the byte's identity and value. Each operation is an
//! `#[verifier::atomic]` function over the one vstd operation (so it can
//! run inside an `open_atomic_invariant!` block), and `#[inline(always)]`
//! leaves the machine code of that call. vstd's operations are `SeqCst`.
//! The trusted part is vstd's: each `PAtomicU8` operation is one atomic
//! operation on the byte, under a sequentially consistent model,
//! `PAtomicU8::new` mints the one permission to the atomic it makes, and
//! atomics have distinct identities. The slot-state bytes are raw mapped
//! memory no `new` made: `api::axiom_start` hands out one permission per
//! address, once (`api::Statics::slots`), and `heap::slotstate::byte_at`
//! names the `PAtomicU8` at an address; both are trusted there.
//!
//! A permission's value is its byte's: every access to a deferred
//! record's phase goes through these wrappers, inside the phase's
//! invariant (`sys::cgroup::PhasePred`: `Deferred::fork_child`,
//! `load_phase`, `claim`, `publish`; `Deferred::get`, `due` and a waiting
//! `Deferred::discover` load through `load_phase`); every access to a
//! slot-state byte is [`compare_exchange`] in `heap::slotstate::step`,
//! inside the slot-state invariant (`heap::slotstate::Slots`); and every
//! access to `STATE.line` is one of these wrappers (`api::load_line`,
//! `claim_line`, `publish_line`, `feature_line_fork_child`), inside the
//! line's invariant (`api::LinePred`), which `init` builds with the
//! permission `STATE`'s initialiser got from [`new`] (`STATE`'s invariant
//! holds it until then).

use crate::sync::patomic::{PAtomicU8, PermissionU8};
use vstd::prelude::*;

verus! {

/// Ghost permission to a byte: vstd's `PermissionU8`, whose `id()` is the
/// byte's [`byte_id`] and whose `value()` is the byte's value.
pub type BytePerm = PermissionU8;

/// The identity of an atomic byte: what a [`BytePerm`] is for.
pub open spec fn byte_id(b: &PAtomicU8) -> int {
    b.id()
}

/// `PAtomicU8::new(v)`, with the one permission to it, which only vstd's
/// `new` mints. It is `const` for `Deferred::later`, which runs at run
/// time: neither may appear in a `const` item, each use of which would be
/// a new atomic with the same permission. Not under `cfg(loom)`, whose
/// atomics are not built in `const`.
#[cfg(not(loom))]
#[inline(always)]
pub const fn new(v: u8) -> (r: (PAtomicU8, Tracked<BytePerm>))
    ensures
        r.1@.id() == byte_id(&r.0),
        r.1@.value() == v,
{
    PAtomicU8::new(v)
}

/// `b.load(SeqCst)`: the permission's value.
#[inline(always)]
#[verifier::atomic]
pub fn load(b: &PAtomicU8, Tracked(perm): Tracked<&BytePerm>) -> (r: u8)
    requires
        perm.id() == byte_id(b),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    b.load(Tracked(perm))
}

/// `b.store(v, SeqCst)`.
#[inline(always)]
#[verifier::atomic]
pub fn store(b: &PAtomicU8, v: u8, Tracked(perm): Tracked<&mut BytePerm>)
    requires
        old(perm).id() == byte_id(b),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    b.store(Tracked(perm), v)
}

/// `b.compare_exchange(current, new, SeqCst, SeqCst)`: `Ok` with the
/// previous value, `current`, when the byte held it and now holds `new`;
/// otherwise `Err` with the value it holds, unchanged (the strong form
/// never fails spuriously).
#[inline(always)]
#[verifier::atomic]
pub fn compare_exchange(
    b: &PAtomicU8,
    current: u8,
    new: u8,
    Tracked(perm): Tracked<&mut BytePerm>,
) -> (r: Result<u8, u8>)
    requires
        old(perm).id() == byte_id(b),
    ensures
        final(perm).id() == old(perm).id(),
        r matches Ok(prev) ==> prev == current && old(perm).value() == current && final(perm).value() == new,
        r matches Err(prev) ==> prev == old(perm).value() && prev != current && final(perm).value() == prev,
    opens_invariants none
    no_unwind
{
    b.compare_exchange(Tracked(perm), current, new)
}

} // verus!
