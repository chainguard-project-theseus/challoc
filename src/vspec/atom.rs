//! `usize` atomics with a ghost permission, for the atomic invariants
//! that bound a value challoc only ever stores in range: a class table's
//! hand-out watermark (`class/table.rs`, `committed_slabs`).
//!
//! A permission names the atomic's identity and value, as `word.rs`'s
//! do a word's; [`new_usize`] is `#[inline(always)]`, so it leaves the
//! machine code of the bare `AtomicUsize::new`. The loads and stores
//! through the permission land with the code that uses them.
//!
//! Trusted:
//! - [`usize_id`] is the atomic's identity, and a permission is for the
//!   atomic with its `id`: a ghost identity carried by the atomic's value,
//!   kept when it is moved into place (`SlabTable::new`'s struct literal),
//!   not its address;
//! - atomics made by different calls have different identities: assumed,
//!   as no code makes two atomics Verus can prove equal (it knows nothing
//!   of `AtomicUsize::new`'s result; [`new_usize`] is not `const`, so no
//!   `const` item can copy one);
//! - one permission per atomic: [`UsizePerm`]'s fields are private to this
//!   module and [`new_usize`] is the only code that builds one, once, with
//!   the atomic it makes (`new_usize` itself is verified).

use core::sync::atomic::AtomicUsize;
use vstd::prelude::*;

verus! {

/// The identity of an atomic `usize`: what a [`UsizePerm`] is for.
pub uninterp spec fn usize_id(a: &AtomicUsize) -> int;

/// Ghost permission to the atomic `usize` with identity `id`: its current
/// `value`. It is not `Copy`, and its fields are private to this module,
/// which builds one only in [`new_usize`].
// Ghost: built here, for the specifications of the watermark.
#[allow(dead_code)]
pub tracked struct UsizePerm {
    ghost id: int,
    ghost value: usize,
}

/// `AtomicUsize::new(v)`, with the one permission to it: the only place a
/// [`UsizePerm`] is minted, one per atomic, with the atomic it is for.
#[inline(always)]
pub fn new_usize(v: usize) -> (r: (AtomicUsize, Tracked<UsizePerm>)) {
    let a = AtomicUsize::new(v);
    (a, Tracked(UsizePerm { id: usize_id(&a), value: v }))
}

} // verus!
