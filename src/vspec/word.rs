//! Class bitmap words as atomics with a ghost permission, for the slot
//! tokens (`bitmap.rs`, "Slot tokens"; `verus/TODO.md`, "Uniqueness: slot
//! tokens").
//!
//! vstd's permission atomics (`vstd::atomic`, and `atomic_ghost` over
//! them) compile every operation as `SeqCst`; a claim's `fetch_or` is
//! `Acquire`, a release's `fetch_and` `Release`, and `AtomicBitmap::init`'s
//! writes `Relaxed`. These wrappers follow vstd's
//! `PAtomicU64` pattern over a plain `core` `AtomicU64` instead: a
//! [`WordPerm`] names the word's address and value, each operation is an
//! `#[verifier::atomic]` function (so it can run inside an
//! `open_atomic_invariant!` block) that passes the caller's ordering
//! through unchanged, and `#[inline(always)]` leaves the machine code of
//! the bare `core` call. The bitmap's loads stay plain `core` loads: vstd
//! specifies their result as any value, which transfers no tokens. The
//! one exception is [`load`], for the `free` path's bit test, whose result
//! is the word's value (a set bit is not in a guard slab).
//!
//! Trusted, all of it:
//! - each wrapped `core` operation is one atomic operation on the word (a
//!   load, a read-modify-write, or a store), as `core` documents;
//! - the proof model is sequentially consistent: the specifications ignore
//!   `ordering`, so nothing here proves that `Relaxed`, `Acquire` and
//!   `Release` are strong enough (`verus/TODO.md`, Tier 4);
//! - [`word_addr`] is the word's address, and a permission is for the word
//!   at its [`WordPerm::addr`] (vstd's `PAtomicU64::id` is the same kind of
//!   uninterpreted identity). The permissions are minted once per class
//!   bitmap, by `SlabTable::new` (`table::axiom_fresh_words`), and for the
//!   fork signal-mask slot (`api::FORK_SIGMASK`) by each fork release's
//!   handoff (`api::axiom_fork_resumed`), at the slot's content.
//!
//! A permission's value is its word's: every write of a class bitmap word
//! goes through these wrappers (`AtomicBitmap::init`, `claim_word` and
//! `release`), each inside the word's invariant (`bitmap::WordPred`); the
//! fork slot is written (a plain store, under every allocator lock, by a
//! hold without its permission: `api::save_sigmask` requires `no_slot`),
//! and its permission lives only in the release's hold of every lock.

use crate::sync::atomic::{AtomicU64, Ordering};
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

/// `w.load(ordering)`: the permission's value (it panics for a `Release`
/// or `AcqRel` ordering). The bitmap's other loads
/// stay plain `core` loads; this one is for `SlabTable::is_set_unchecked`,
/// whose set bit proves the slab is not a guard slab (`bitmap::guards_ok`).
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
pub fn load(w: &AtomicU64, ordering: Ordering, Tracked(perm): Tracked<&WordPerm>) -> (r: u64)
    requires
        perm.addr() == word_addr(w),
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    w.load(ordering)
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
