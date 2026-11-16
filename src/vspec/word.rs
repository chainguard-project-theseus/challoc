//! Class bitmap words as vstd permissioned atomics, for the slot tokens
//! (`bitmap.rs`, "Slot tokens"), and the read of the fork handlers'
//! signal-mask slot (`api::FORK_SIGMASK`).
//!
//! Each bitmap word is a vstd `PAtomicU64`, and its [`WordPerm`] is vstd's
//! `PermissionU64`: the word's identity and value. Each operation is an
//! `#[verifier::atomic]` function over the one vstd operation (so it can
//! run inside an `open_atomic_invariant!` block), and `#[inline(always)]`
//! leaves the machine code of that call. vstd's operations are `SeqCst`.
//! The trusted part is vstd's: each `PAtomicU64` operation is one atomic
//! operation on the word, under a sequentially consistent model,
//! `PAtomicU64::new` mints the one permission to the atomic it makes, and
//! atomics have distinct identities. The class bitmaps are raw metadata
//! memory no `new` made: `class::table::axiom_fresh_words` hands out one
//! permission per word, with the identity [`word_at`] names for its
//! address, and the permission to borrow the word there as a `PAtomicU64`;
//! both are trusted there.
//!
//! A permission's value is its word's: every access to a class bitmap
//! word goes through these wrappers (`AtomicBitmap::init`, `claim_word`,
//! `release`, `probe` and `load_word`, and `SlabTable::bit_word_held`),
//! each inside the word's invariant (`bitmap::WordPred`).
//!
//! `api::FORK_SIGMASK` is a `core` `AtomicU64`, written by
//! `api::save_sigmask`'s raw store, through no permission, so it cannot be
//! a `PAtomicU64`. Its permission is a vstd `PermissionU64` that
//! `api::resume::axiom_fork_resumed` mints for the slot's identity
//! ([`fork_slot_id`]); [`load_fork_slot`] reads the slot through it.
//! Trusted: that wrapper's body is one `core` load, the slot's value is
//! its permission's (the axiom's premise), and [`fork_slot_id`] is the
//! slot's identity.

use crate::sync::atomic::{AtomicU64, Ordering};
use crate::sync::patomic::{PAtomicU64, PermissionU64};
use vstd::prelude::*;

verus! {

/// Ghost permission to a bitmap word: vstd's `PermissionU64`, whose
/// `id()` is the word's [`word_id`] and whose `value()` is the word's
/// value.
pub type WordPerm = PermissionU64;

/// The identity of an atomic word: what a [`WordPerm`] is for.
pub open spec fn word_id(w: &PAtomicU64) -> int {
    w.id()
}

/// The identity of the `PAtomicU64` at address `addr` of a class bitmap
/// (`class::table::axiom_fresh_words`). Premise, not checked: distinct
/// addresses have distinct identities, and none is the identity of an
/// atomic `PAtomicU64::new` made or the fork slot's ([`fork_slot_id`]).
pub uninterp spec fn word_at(addr: int) -> int;

/// `w.fetch_or(v, SeqCst)`.
#[inline(always)]
#[verifier::atomic]
pub fn fetch_or(w: &PAtomicU64, v: u64, Tracked(perm): Tracked<&mut WordPerm>) -> (prev: u64)
    requires
        old(perm).id() == word_id(w),
    ensures
        final(perm).id() == old(perm).id(),
        prev == old(perm).value(),
        final(perm).value() == prev | v,
    opens_invariants none
    no_unwind
{
    w.fetch_or(Tracked(perm), v)
}

/// `w.store(v, SeqCst)`.
#[inline(always)]
#[verifier::atomic]
pub fn store(w: &PAtomicU64, v: u64, Tracked(perm): Tracked<&mut WordPerm>)
    requires
        old(perm).id() == word_id(w),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    w.store(Tracked(perm), v)
}

/// `w.load(SeqCst)`: the permission's value.
#[inline(always)]
#[verifier::atomic]
pub fn load(w: &PAtomicU64, Tracked(perm): Tracked<&WordPerm>) -> (r: u64)
    requires
        perm.id() == word_id(w),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    w.load(Tracked(perm))
}

/// `w.fetch_and(v, SeqCst)`.
#[inline(always)]
#[verifier::atomic]
pub fn fetch_and(w: &PAtomicU64, v: u64, Tracked(perm): Tracked<&mut WordPerm>) -> (prev: u64)
    requires
        old(perm).id() == word_id(w),
    ensures
        final(perm).id() == old(perm).id(),
        prev == old(perm).value(),
        final(perm).value() == prev & v,
    opens_invariants none
    no_unwind
{
    w.fetch_and(Tracked(perm), v)
}

/// The identity of the fork handlers' signal-mask slot, `w`: what the
/// permission `api::resume::axiom_fork_resumed` mints is for. Premise,
/// not checked: it is not the identity of an atomic `PAtomicU64::new`
/// made or of a class bitmap word ([`word_at`]).
pub uninterp spec fn fork_slot_id(w: &AtomicU64) -> int;

/// `w.load(Relaxed)` of the fork slot: the permission's value. The slot's
/// accesses are ordered by the allocator's locks (`api::FORK_SIGMASK`).
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn load_fork_slot(w: &AtomicU64, Tracked(perm): Tracked<&PermissionU64>) -> (r: u64)
    requires
        perm.id() == fork_slot_id(w),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    w.load(Ordering::Relaxed)
}

} // verus!
