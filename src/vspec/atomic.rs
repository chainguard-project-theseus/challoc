//! `core` atomic loads that open no invariant, for the `free` fast path
//! (`verus/THEOREMS.md`, theorem 9).
//!
//! vstd specifies the `core` atomic operations with Verus's default
//! invariant mask, `opens_invariants any`, so a function that declares a
//! narrower mask cannot call one. The fast paths declare `none` (the `free`
//! bit test: the bitmap words' namespace), which is how the proof shows
//! they take no lock: [`RawLock::lock`](crate::lock::RawLock::lock) opens
//! `any`. This wrapper is the `multi_arena` load `Shared::checked_slot`
//! makes (its owner-word load is `class::table`'s `load_owner`), the bare
//! `core` call under `#[inline(always)]` (the machine code is the load's).
//!
//! Trusted: a `core` atomic load opens no Verus invariant (it is one
//! instruction, and no ghost state is involved). Nothing is claimed about
//! the value, as vstd claims nothing.

use crate::sync::atomic::{AtomicBool, Ordering};
use vstd::prelude::*;

verus! {

/// `a.load(order)`.
#[inline(always)]
#[verifier::external_body]
pub fn load_bool(a: &AtomicBool, order: Ordering) -> bool
    opens_invariants none
{
    a.load(order)
}

} // verus!
