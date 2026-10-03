//! The heap-inconsistency aborts of the class protocol: `#[cold]`,
//! never inlined, so the fast paths keep only a call.

use super::MAX_EMPTY_PASSES;
use crate::slab::State;
use crate::sys;
use vstd::prelude::*;

verus! {

#[cold]
#[inline(never)]
pub(super) fn never_committed(class: usize, slab: usize) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!("invalid free: class {class} slab {slab} was never allocated"))
}

#[cold]
#[inline(never)]
pub(super) fn owner_mismatch(class: usize, group: usize, slab: usize, word: u16, desc: u16) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!(
        "heap inconsistency: class {class} group {group}: owner word says arena {word}, slab {slab}'s descriptor says arena {desc}"
    ))
}

/// A `Fresh` or `Purged` slab selected by a refill had a bit set before
/// its slots were claimed under the lock (`claim_prefix`): `live` no longer
/// bounds the set bits, so the bitmap or the descriptor is corrupt.
#[cold]
#[inline(never)]
pub(super) fn zero_slab_had_bits_set(class: usize, slab: usize, arena: u16, state: State, live: usize, word: usize, got: u64, expect: u64) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!(
        "class {class} slab {slab} (arena {arena}) is {state:?} with live {live} but word {word} of its bitmap had bits set before the refill's claim: claimed {got:#x}, expected {expect:#x}"
    ))
}

/// A slot start inside a guard slab (`is_guard_slab`): never allocated
/// from, so any free of it is invalid.
#[cold]
#[inline(never)]
pub(super) fn guard_free(p: usize, class: usize, slab: usize) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!("invalid free of {p:#x} (class {class} slab {slab}): guard slab"))
}

/// A release into a `Fresh` slab: handed to an arena but not committed
/// (at or above the arena's commit cursor), or committed and never
/// claimed. No slot of it was ever handed out.
#[cold]
#[inline(never)]
pub(super) fn release_into_fresh_slab(p: usize, class: usize, slab: usize) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!(
        "invalid free of {p:#x} (class {class} slab {slab}): the slab has never been allocated from"
    ))
}

/// The `free` check's abort (design section 4): the slot's bit is clear,
/// which is an invalid free or a stale double free.
#[cold]
#[inline(never)]
pub(super) fn invalid_free(p: usize, class: usize, slab: usize, slot: usize) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!(
        "invalid or double free of {p:#x} (class {class} slab {slab} slot {slot}): slot is not allocated"
    ))
}

#[cold]
#[inline(never)]
pub(super) fn wrong_arena(p: usize, class: usize, owner: u16, arena: u16) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!(
        "slot {p:#x} (class {class}) belongs to arena {owner} but was released through arena {arena}"
    ))
}

#[cold]
#[inline(never)]
pub(super) fn reservation_unmet(class: usize, slab: usize, want: usize, got: usize, live: usize, set: usize, nslots: usize) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!(
        "class {class}: slab {slab} yielded {got} of a reservation of {want} slots after {MAX_EMPTY_PASSES} full scans (live {live}, allocated {set}, nslots {nslots}): live count is corrupt"
    ))
}

#[cold]
#[inline(never)]
pub(super) fn invalid_pointer(p: usize, class: usize, why: &str) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!("invalid free of {p:#x} (class {class}): {why}"))
}

#[cold]
#[inline(never)]
pub(super) fn double_free(p: usize, class: usize, slab: usize, slot: usize) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!("double free of {p:#x} (class {class} slab {slab} slot {slot})"))
}

/// A slab count of `ClassStats` went below zero: the sets and the counts
/// disagree, which is a heap inconsistency.
#[cold]
#[inline(never)]
pub(super) fn slab_count_underflow(which: &str) -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!("class: {which} slab count underflow"))
}

/// `take`: a slab's `live` count is above its slot count, which would
/// reserve a wrapped count. Unlike the others inlined: the abort stands
/// for the `assert!` `take` made in place.
#[inline(always)]
pub(super) fn live_above_slots() -> !
    requires
        crate::sys::may_abort(),
{
    sys::abort_fmt(format_args!("class: slab live count above its slot count"))
}

} // verus!
