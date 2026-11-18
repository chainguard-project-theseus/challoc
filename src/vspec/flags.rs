//! The `free` fast path's dispatch byte (`heap::Shared::free_flags`) as
//! an atomic whose bits other than [`DYN`] ([`ROUTE`] and [`EPOCH`])
//! never change once it is made.
//!
//! [`RouteByte`] holds a vstd `PAtomicU8` and an atomic invariant over
//! its permission: the byte's bits other than [`DYN`] are the
//! invariant's constant, [`fixed`]. [`new`] makes it (`Shared::put_flags`,
//! from `push_flags`), [`fetch_or`] of bits within [`DYN`]
//! (`Shared::set_multi_arena`, `Shared::set_epoch`) and [`fetch_and`]
//! clearing bits within it (`Shared::set_epoch`) are the only writes after
//! that, and the `free` fast path reads it with [`load`]
//! (`Shared::free_flags`). Each wrapper opens the invariant around its
//! one atomic operation, so that every value the byte holds keeps
//! [`fixed`] is proved, not trusted.
//!
//! vstd's `PAtomicU8` operations are `SeqCst`; the trusted part is vstd's
//! (each `PAtomicU8` operation is one atomic operation on the byte, under
//! a sequentially consistent model).

use crate::sync::patomic::{PAtomicU8, PermissionU8};
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
use vstd::prelude::*;

verus! {

/// The bit set once a second arena exists: `heap::FREE_ROUTE`
/// (`heap/shared.rs` asserts the two equal at compile time).
pub const ROUTE: u8 = 8;

/// The bit set while a fork epoch is open: `heap::FREE_EPOCH` (asserted
/// equal, as [`ROUTE`]).
pub const EPOCH: u8 = 16;

/// The bits [`fetch_or`] may set and [`fetch_and`] may clear.
pub const DYN: u8 = 24;

/// The namespace of a [`RouteByte`]'s invariant.
pub open spec fn flags_ns() -> int {
    0x666c_6167
}

/// The invariant's predicate: the permission is the byte's, and its bits
/// other than [`DYN`] are the constant.
pub struct FixedPred;

impl InvariantPredicate<(u8, int), PermissionU8> for FixedPred {
    open spec fn inv(k: (u8, int), perm: PermissionU8) -> bool {
        perm.view().patomic == k.1 && perm.view().value & !DYN == k.0
    }
}

/// An atomic byte whose bits other than [`DYN`] are [`fixed`]: only
/// [`new`] makes one, and only [`fetch_or`] and [`fetch_and`] within
/// [`DYN`] write it.
pub struct RouteByte {
    b: PAtomicU8,
    // Ghost: read only by the proofs, so an ordinary build sees it unused.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    inv: Tracked<AtomicInvariant<(u8, int), PermissionU8, FixedPred>>,
}

impl RouteByte {
    #[verifier::type_invariant]
    spec fn wf(self) -> bool {
        &&& self.inv@.constant().1 == self.b.id()
        &&& self.inv@.namespace() == flags_ns()
    }
}

/// The byte's bits other than [`DYN`]: the same in every value it
/// holds.
pub closed spec fn fixed(b: &RouteByte) -> u8 {
    b.inv@.constant().0
}

/// A byte whose bits other than [`DYN`] are `v`'s from here on.
#[inline(always)]
pub fn new(v: u8) -> (r: RouteByte)
    ensures
        fixed(&r) == v & !DYN,
{
    let (b, Tracked(perm)) = PAtomicU8::new(v);
    let tracked inv = AtomicInvariant::new((v & !DYN, b.id()), perm, flags_ns());
    RouteByte { b, inv: Tracked(inv) }
}

/// `b.load(SeqCst)`: the made value, with or without the bits of
/// [`DYN`].
#[inline(always)]
pub fn load(b: &RouteByte) -> (r: u8)
    ensures
        r & !DYN == fixed(b),
    opens_invariants [flags_ns()]
    no_unwind
{
    proof {
        use_type_invariant(b);
    }
    let r;
    vstd::open_atomic_invariant!(b.inv.borrow() => perm => {
        r = b.b.load(Tracked(&perm));
    });
    r
}

/// `b.fetch_or(v, SeqCst)`, for bits `v` within [`DYN`]: the previous
/// value, which is the made value with or without the bits of [`DYN`].
#[inline(always)]
pub fn fetch_or(b: &RouteByte, v: u8) -> (prev: u8)
    requires
        v & !DYN == 0,
    ensures
        prev & !DYN == fixed(b),
    opens_invariants [flags_ns()]
    no_unwind
{
    proof {
        use_type_invariant(b);
    }
    let prev;
    vstd::open_atomic_invariant!(b.inv.borrow() => perm => {
        let ghost before = perm.view().value;
        prev = b.b.fetch_or(Tracked(&mut perm), v);
        let ghost after = perm.view().value;
        assert(after & !24u8 == before & !24u8) by (bit_vector)
            requires
                after == before | v,
                v & !24u8 == 0;
    });
    prev
}

/// `b.fetch_and(v, SeqCst)`, for a mask `v` that clears only bits within
/// [`DYN`]: the previous value, which is the made value with or without
/// the bits of [`DYN`].
#[inline(always)]
pub fn fetch_and(b: &RouteByte, v: u8) -> (prev: u8)
    requires
        v | DYN == 0xff,
    ensures
        prev & !DYN == fixed(b),
    opens_invariants [flags_ns()]
    no_unwind
{
    proof {
        use_type_invariant(b);
    }
    let prev;
    vstd::open_atomic_invariant!(b.inv.borrow() => perm => {
        let ghost before = perm.view().value;
        prev = b.b.fetch_and(Tracked(&mut perm), v);
        let ghost after = perm.view().value;
        assert(after & !24u8 == before & !24u8) by (bit_vector)
            requires
                after == before & v,
                v | 24u8 == 0xffu8;
    });
    prev
}

} // verus!
