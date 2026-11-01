//! The `free` fast path's dispatch byte (`heap::Shared::free_flags`) as
//! an atomic whose bits other than [`ROUTE`] never change once it is
//! made.
//!
//! [`RouteByte`] wraps the `core` `AtomicU8`, `#[repr(transparent)]`, in
//! a field private to this module, so the only operations on it are the
//! wrappers below: [`new`] makes it (`Shared::put_flags`, from
//! `push_flags`), [`fetch_or`] of bits within [`ROUTE`] is the only write
//! after that (`Shared::set_multi_arena`), and the `free` fast path reads
//! it with [`load`] (`Shared::free_flags`). So every value the byte holds
//! is the made one, with or without [`ROUTE`]: an invariant of the
//! writers, which no ghost permission needs to carry (the reader is the
//! `free` fast path, which holds no ghost state for the byte). [`fixed`]
//! names the bits it keeps; `Shared::built` ties them to `push_flags`
//! (`Shared::lemma_route_flags` reads them back). Each
//! wrapper is `#[inline(always)]` and passes the caller's ordering through
//! unchanged, which leaves the machine code of the bare `core` call.
//!
//! Trusted, all of it:
//! - each wrapped `core` operation is one atomic operation on the byte (a
//!   load or a read-modify-write), and `AtomicU8::new(v)` makes a byte
//!   holding `v`, as `core` documents;
//! - [`fixed`] is a function of the value, fixed when [`new`] makes it,
//!   kept when it is moved into place (`put_flags`' `ptr::write`), and not
//!   changed by an operation through `&RouteByte`, which is what the
//!   writers' invariant needs: every write of the byte after [`new`] is
//!   [`fetch_or`] of bits within [`ROUTE`] (the field is private, the type
//!   has no `Clone`, `Copy` or `Default`, and [`new`] is its only
//!   constructor);
//! - the proof model is sequentially consistent: the specifications ignore
//!   `ordering`. That every load reads [`new`]'s write or a later
//!   [`fetch_or`] (and not the zeroed storage before `init`) is C11
//!   coherence once the reader happens after `init`, the premise of every
//!   holder of a published `&Shared`.

use core::sync::atomic::{AtomicU8, Ordering};
use vstd::prelude::*;

verus! {

/// The one bit [`fetch_or`] may set: `heap::FREE_ROUTE` (`heap/shared.rs`
/// asserts the two equal at compile time).
pub const ROUTE: u8 = 8;

/// An atomic byte whose bits other than [`ROUTE`] are [`fixed`]: only
/// [`new`] makes one, and only [`fetch_or`] within [`ROUTE`] writes it.
#[repr(transparent)]
pub struct RouteByte {
    b: AtomicU8,
}

/// The byte's bits other than [`ROUTE`]: the same in every value it
/// holds.
pub uninterp spec fn fixed(b: &RouteByte) -> u8;

/// `AtomicU8::new(v)`: a byte whose bits other than [`ROUTE`] are `v`'s
/// from here on.
#[inline(always)]
#[verifier::external_body]
pub fn new(v: u8) -> (r: RouteByte)
    ensures
        fixed(&r) == v & !ROUTE,
    opens_invariants none
    no_unwind
{
    RouteByte { b: AtomicU8::new(v) }
}

/// `b.load(ordering)` (it panics for a `Release` or `AcqRel` ordering):
/// the made value, with or without [`ROUTE`].
#[inline(always)]
#[verifier::external_body]
pub fn load(b: &RouteByte, ordering: Ordering) -> (r: u8)
    requires
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r & !ROUTE == fixed(b),
    opens_invariants none
    no_unwind
{
    b.b.load(ordering)
}

/// `b.fetch_or(v, ordering)`, for bits `v` within [`ROUTE`]: the previous
/// value, which is the made value with or without [`ROUTE`].
#[inline(always)]
#[verifier::external_body]
pub fn fetch_or(b: &RouteByte, v: u8, ordering: Ordering) -> (prev: u8)
    requires
        v & !ROUTE == 0,
    ensures
        prev & !ROUTE == fixed(b),
    opens_invariants none
    no_unwind
{
    b.b.fetch_or(v, ordering)
}

} // verus!
