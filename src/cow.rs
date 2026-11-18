//! Fork epochs (design 5, "Fork epochs"; 13.7): `challoc_cow_begin`
//! and `challoc_cow_end` around the life of a fork child.
//!
//! A child made by `fork(2)` shares every page of the parent until one of
//! them writes it, and a write of the parent to a shared page copies it.
//! A snapshotting server (Valkey's and Redis's `BGSAVE`, an AOF rewrite, a
//! full resynchronisation) keeps serving writes while its child runs, and
//! each new value is an allocation: the refill and the cache serve it from
//! the lowest free slot, a slot on a page the child shares, so a 64-byte
//! value copies a 4 KiB page. An epoch moves those allocations to pages
//! the child never had:
//!
//! - **Floors.** [`begin`] records, for every `(arena, class)`, the
//!   arena's commit cursor (`ClassState::cow_mark`). Every slab below it
//!   may share pages with the child, and none at or above it does. While
//!   the floors are in force a refill takes slots only at or above the
//!   floor, or from a `Purged` slab (`ClassState::plan`, `next_slab`).
//! - **Deflected frees.** A free of a slot below the floor goes to the
//!   flush-only `FOREIGN` cache instead of the cache `malloc` pops (the
//!   `free` fast path declines it under [`crate::heap::FREE_EPOCH`]; the
//!   sized fast path declines every free during an epoch), so the freed
//!   slot goes back to its slab's bitmap and is not handed out again
//!   while the floor holds.
//! - **Caches.** The caller's caches are flushed at [`begin`]; every other
//!   heap flushes its caches at its next slow path ([`flush_after`]).
//! - **Cap.** The bytes refills reserve under the floors are counted
//!   ([`note_claimed`]); past the cap (the slab bytes allocated when the
//!   epoch began: past it an epoch costs more than copying every page
//!   would) the floors stop holding, so a child that never exits cannot
//!   grow the heap without bound.
//! - **Drain.** [`end`] turns each floor into a drain floor
//!   (`ClassState::cow_settle`): the utilisation queries and the defrag
//!   hint tell a defragmenter to move a block of a slab at or above it
//!   while a slab below it has a free slot ([`drain_move`]), so the blocks
//!   written during the epoch move back into the holes it left.
//!
//! Epochs nest: only the outermost [`begin`] and [`end`] act. Off with
//! `cow_epoch:false`.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::class::ClassState;
use crate::heap::Shared;
use crate::layout::NCLASSES;
use crate::lock::LockOrder;
#[cfg(verus_keep_ghost)]
use crate::lock::rank_class;
use vstd::prelude::*;

verus! {

/// The floors hold: refills honour them (`ClassState::cow_floor`).
exec static IN_FORCE: AtomicBool = AtomicBool::new(false);
/// Open epochs (nesting depth).
exec static DEPTH: AtomicU32 = AtomicU32::new(0);
/// A heap whose last cache GC is older than this (coarse monotonic
/// nanoseconds) flushes every cache at its next slow path; 0 for none.
exec static FLUSH_AFTER: AtomicU64 = AtomicU64::new(0);
/// Bytes reserved by refills under the floors of the current (or last)
/// epoch, and the cap past which the floors stop holding.
exec static CLAIMED: AtomicU64 = AtomicU64::new(0);
exec static CAP: AtomicU64 = AtomicU64::new(0);
/// Epochs begun, and those whose floors the cap ended early.
exec static EPOCHS: AtomicU64 = AtomicU64::new(0);
exec static EXPIRED: AtomicU64 = AtomicU64::new(0);
/// Frees kept out of the caches because their slot was below a floor.
exec static DEFLECTED: AtomicU64 = AtomicU64::new(0);
/// Blocks the utilisation queries told a defragmenter to move back below
/// a drain floor.
exec static DRAIN_MOVES: AtomicU64 = AtomicU64::new(0);
/// Some `(arena, class)` may still have a drain floor.
exec static DRAINING: AtomicBool = AtomicBool::new(false);

/// The smallest cap: an epoch begun over a nearly empty heap still has
/// room for the writes of a short child.
pub const MIN_CAP: u64 = 64 << 20;

/// Results of [`begin`] and [`end`], as `challoc_cow_begin` returns them.
pub const ENOTSUP: i32 = 95;
pub const EINVAL: i32 = 22;

/// Whether the floors hold.
#[inline(always)]
pub fn in_force() -> bool {
    IN_FORCE.load(Ordering::Relaxed)
}

/// Whether an epoch is open (its floors may have stopped holding).
#[inline(always)]
pub fn open() -> bool {
    DEPTH.load(Ordering::Relaxed) != 0
}

/// The cache-flush stamp of the current epoch ([`FLUSH_AFTER`]), 0 for
/// none: a heap whose last cache GC is older flushes every cache.
#[inline(always)]
pub fn flush_after() -> u64 {
    FLUSH_AFTER.load(Ordering::Relaxed)
}

/// Whether some `(arena, class)` may still drain.
#[inline(always)]
pub fn draining() -> bool {
    DRAINING.load(Ordering::Relaxed)
}

/// `bytes` were reserved by a refill under a floor: past the cap, the
/// floors stop holding.
pub fn note_claimed(bytes: u64) {
    let before = CLAIMED.fetch_add(bytes, Ordering::Relaxed);
    if before <= CAP.load(Ordering::Relaxed) && before.saturating_add(bytes) > CAP.load(Ordering::Relaxed) && IN_FORCE.load(Ordering::Relaxed) {
        IN_FORCE.store(false, Ordering::Relaxed);
        EXPIRED.fetch_add(1, Ordering::Relaxed);
    }
}

/// A free kept out of the caches by a floor.
#[inline]
pub fn note_deflected() {
    DEFLECTED.fetch_add(1, Ordering::Relaxed);
}

/// [`begin`]'s walk: every class state of every built arena below the
/// registry's count, in order, gets its floor (`ClassState::cow_mark`).
/// Returns the slab bytes out of the bitmaps (in caches or in use) it
/// counted on the way, the epoch's cap.
fn each_class_mark(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>) -> (cap: u64)
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    let n = (shared.arenas.count() as usize).min(crate::arena::MAX_ARENAS);
    let mut bytes: u64 = 0;
    let mut i: usize = 0;
    while i < n
        invariant
            crate::sys::may_syscall(),
            ord.allows(rank_class(0, 0)),
            *ord == *old(ord),
            n <= crate::arena::MAX_ARENAS,
        decreases n - i,
    {
        if let Some(a) = shared.arenas.get_existing(i as u16) {
            let mut c: usize = 0;
            while c < NCLASSES
                invariant
                    crate::sys::may_syscall(),
                    ord.allows(rank_class(0, 0)),
                    *ord == *old(ord),
                decreases NCLASSES - c,
            {
                let cs: &ClassState = a.class(c);
                let (_st, curregs) = cs.bin_stats(Tracked(&mut *ord));
                bytes = bytes.saturating_add((curregs as u64).saturating_mul(cs.class().size() as u64));
                cs.cow_mark(Tracked(&mut *ord));
                c += 1;
            }
        }
        i += 1;
    }
    bytes
}

/// [`end`]'s walk: every floor becomes a drain floor.
fn each_class_settle(shared: &Shared) {
    let n = (shared.arenas.count() as usize).min(crate::arena::MAX_ARENAS);
    let mut i: usize = 0;
    while i < n
        invariant
            n <= crate::arena::MAX_ARENAS,
        decreases n - i,
    {
        if let Some(a) = shared.arenas.get_existing(i as u16) {
            let mut c: usize = 0;
            while c < NCLASSES
                decreases NCLASSES - c,
            {
                a.class(c).cow_settle();
                c += 1;
            }
        }
        i += 1;
    }
}

/// Start an epoch (the outermost [`begin`] acts; nested ones count): the
/// floors at every arena's commit cursor, the cap, the cache-flush stamp
/// and the `free` dispatch bit. The caller has flushed its own caches and
/// purged the `Empty` slabs (`api::cow::begin`). `ENOTSUP` with
/// `cow_epoch:false`.
pub fn begin(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>) -> (r: i32)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    if !shared.cow_enabled() {
        return ENOTSUP;
    }
    if DEPTH.fetch_add(1, Ordering::Relaxed) != 0 {
        return 0;
    }
    EPOCHS.fetch_add(1, Ordering::Relaxed);
    let live = each_class_mark(shared, Tracked(&mut *ord));
    CLAIMED.store(0, Ordering::Relaxed);
    CAP.store(if live > MIN_CAP { live } else { MIN_CAP }, Ordering::Relaxed);
    // One past now: a heap whose last GC ran in this coarse tick flushes
    // too (`Heap::tick`).
    FLUSH_AFTER.store(crate::sys::now_coarse_ns().saturating_add(1), Ordering::Relaxed);
    IN_FORCE.store(true, Ordering::Relaxed);
    shared.set_epoch(true);
    0
}

/// End the epoch (the outermost [`end`] acts): the floors go, each
/// becoming a drain floor, and the `free` dispatch bit is cleared.
/// `EINVAL` with no epoch open.
pub fn end(shared: &Shared) -> (r: i32)
    requires
        shared.wf(),
{
    let d = DEPTH.load(Ordering::Relaxed);
    if d == 0 {
        return EINVAL;
    }
    DEPTH.store(d - 1, Ordering::Relaxed);
    if d > 1 {
        return 0;
    }
    IN_FORCE.store(false, Ordering::Relaxed);
    FLUSH_AFTER.store(0, Ordering::Relaxed);
    shared.set_epoch(false);
    each_class_settle(shared);
    DRAINING.store(true, Ordering::Relaxed);
    0
}

/// Whether a defragmenter should move a block of slab `slab` of `cs`
/// back below the pair's drain floor: the slab is at or above it, and a
/// slab below it has a free slot (the slab a direct claim takes from
/// next, `lowest_partial`). The first query that finds none ends the
/// pair's drain. `ord` is the caller's lock-order token (ghost) for the
/// class lock `lowest_partial` takes.
pub fn drain_move(cs: &ClassState, slab: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: bool)
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    let f = cs.drain_floor();
    if f == 0 || slab < f {
        return false;
    }
    match cs.lowest_partial(Tracked(ord)) {
        Some(s) if s < f => {
            DRAIN_MOVES.fetch_add(1, Ordering::Relaxed);
            true
        },
        _ => {
            cs.drain_done();
            false
        },
    }
}

/// The figures `challoc_cow_stat(which)` reports (`<challoc/challoc.h>`'s
/// `CHALLOC_COW_*`): 0 epochs begun, 1 epochs open (depth), 2 floors in
/// force, 3 epochs the cap ended early, 4 bytes reserved under the floors
/// of the current or last epoch, 5 its cap, 6 deflected frees, 7 drain
/// moves suggested, 8 `(arena, class)` pairs still draining. 0 for any
/// other `which`.
pub fn stat(shared: &Shared, which: u32) -> u64 {
    match which {
        0 => EPOCHS.load(Ordering::Relaxed),
        1 => DEPTH.load(Ordering::Relaxed) as u64,
        2 => IN_FORCE.load(Ordering::Relaxed) as u64,
        3 => EXPIRED.load(Ordering::Relaxed),
        4 => CLAIMED.load(Ordering::Relaxed),
        5 => CAP.load(Ordering::Relaxed),
        6 => DEFLECTED.load(Ordering::Relaxed),
        7 => DRAIN_MOVES.load(Ordering::Relaxed),
        8 => draining_pairs(shared),
        _ => 0,
    }
}

/// `(arena, class)` pairs whose drain floor is set.
fn draining_pairs(shared: &Shared) -> u64 {
    let n = (shared.arenas.count() as usize).min(crate::arena::MAX_ARENAS);
    let mut k: u64 = 0;
    let mut i: usize = 0;
    while i < n
        invariant
            n <= crate::arena::MAX_ARENAS,
        decreases n - i,
    {
        if let Some(a) = shared.arenas.get_existing(i as u16) {
            let mut c: usize = 0;
            while c < NCLASSES
                decreases NCLASSES - c,
            {
                if a.class(c).drain_floor() != 0 {
                    k = k.saturating_add(1);
                }
                c += 1;
            }
        }
        i += 1;
    }
    k
}

} // verus!
