//! Class bitmap words as atomics with a ghost permission, for the slot
//! tokens (`bitmap.rs`, "Slot tokens").
//!
//! vstd's permission atomics (`vstd::atomic`, and `atomic_ghost` over
//! them) compile every operation as `SeqCst`; a claim's `fetch_or` is
//! `Acquire`, a release's `fetch_and` `Release`, and `AtomicBitmap::init`'s
//! writes `Relaxed`. These wrappers follow vstd's `PAtomicU64` pattern over
//! a plain `core` `AtomicU64` instead: a [`WordPerm`] names the word's
//! address and value, each operation is an `#[verifier::atomic]` function
//! (so it can run inside an `open_atomic_invariant!` block) that passes the
//! caller's ordering through unchanged, and `#[inline(always)]` leaves the
//! machine code of the bare `core` call. The bitmap's loads stay plain
//! `core` loads: vstd specifies their result as any value, which transfers
//! no tokens.
//!
//! Trusted, all of it:
//! - each wrapped `core` operation is one atomic operation on the word (a
//!   read-modify-write or a store), as `core` documents;
//! - the proof model is sequentially consistent: the specifications ignore
//!   `ordering`, so nothing here proves that `Relaxed`, `Acquire` and
//!   `Release` are strong enough;
//! - [`word_addr`] is the word's address, and a permission is for the word
//!   at its [`WordPerm::addr`] (vstd's `PAtomicU64::id` is the same kind of
//!   uninterpreted identity).
//!
//! A permission's value is its word's: every write of a class bitmap word
//! goes through these wrappers (`AtomicBitmap::init`, `claim_word` and
//! `release`), each inside the word's invariant (`bitmap::WordPred`).

use core::sync::atomic::{AtomicU64, Ordering};
use vstd::prelude::*;

verus! {

/// The address of an atomic word: the identity a [`WordPerm`] is for.
pub uninterp spec fn word_addr(w: &AtomicU64) -> usize;

/// Ghost permission to the word at [`addr`](WordPerm::addr): its current
/// [`value`](WordPerm::value). It is not `Copy`.
#[verifier::external_body]
pub tracked struct WordPerm {
    _no_copy: (),
}

impl WordPerm {
    /// The word this permission is for.
    pub uninterp spec fn addr(&self) -> usize;

    /// The word's value.
    pub uninterp spec fn value(&self) -> u64;
}

/// `w.fetch_or(v, ordering)`.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn fetch_or(w: &AtomicU64, v: u64, ordering: Ordering, Tracked(perm): Tracked<&mut WordPerm>) -> (prev: u64)
    requires
        old(perm).addr() == word_addr(w),
    ensures
        final(perm).addr() == old(perm).addr(),
        prev == old(perm).value(),
        final(perm).value() == prev | v,
    opens_invariants none
    no_unwind
{
    w.fetch_or(v, ordering)
}

/// `w.store(v, ordering)`, which panics for an `Acquire` or `AcqRel`
/// ordering.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn store(w: &AtomicU64, v: u64, ordering: Ordering, Tracked(perm): Tracked<&mut WordPerm>)
    requires
        old(perm).addr() == word_addr(w),
        !(ordering is Acquire),
        !(ordering is AcqRel),
    ensures
        final(perm).addr() == old(perm).addr(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    w.store(v, ordering)
}

/// `w.fetch_and(v, ordering)`.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn fetch_and(w: &AtomicU64, v: u64, ordering: Ordering, Tracked(perm): Tracked<&mut WordPerm>) -> (prev: u64)
    requires
        old(perm).addr() == word_addr(w),
    ensures
        final(perm).addr() == old(perm).addr(),
        prev == old(perm).value(),
        final(perm).value() == prev & v,
    opens_invariants none
    no_unwind
{
    w.fetch_and(v, ordering)
}

} // verus!
