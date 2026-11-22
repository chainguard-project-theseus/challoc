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
//!   written during the epoch move back into the holes it left. A pair
//!   drains while that work is left ([`stat`]'s figure 8, which a server
//!   reads to force its defragmenter on): holes below, blocks above in
//!   more than one slab, and the queries still asking for moves.
//!
//! Epochs nest: only the outermost [`begin`] and [`end`] act, one at a
//! time (a [`begin`] while another thread's is under way is refused,
//! [`enter`]). Off with `cow_epoch:false`.

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
/// Open epochs (nesting depth), plus [`BUSY`] while the outermost
/// [`begin`] or [`end`] does its work: every change of the depth is a
/// compare-and-exchange, and none is made while [`BUSY`] is set, so the
/// outermost transitions run one at a time and a nested one never
/// returns before the floors it relies on are set.
exec static DEPTH: AtomicU32 = AtomicU32::new(0);
/// [`DEPTH`]'s bit for an outermost transition under way.
const BUSY: u32 = 0x8000_0000;
/// The most epochs open at once: [`begin`] refuses one more (`EBUSY`).
const MAX_DEPTH: u32 = 0x7fff_ffff;
/// How many compare-and-exchanges [`begin`] and [`end`] try while other
/// threads move the depth before they give up (`EBUSY`, `EAGAIN`).
const MAX_TRIES: u32 = 1 << 16;
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
/// Some `(arena, class)` may still have a drain floor, until this
/// coarse monotonic time (ns; 0 for none): [`DRAIN_WINDOW_NS`] after the
/// last [`end`].
exec static DRAIN_UNTIL: AtomicU64 = AtomicU64::new(0);
/// The drain's progress as the readers of figure 8 saw it ([`stalled`]):
/// [`seen_word`] of the time [`DRAIN_MOVES`] last changed between two
/// reads and of its count then, or 0 before the first read since [`end`]
/// (which stores 0). One word, so a reader never pairs one reader's count
/// with another's time, and the stall clock starts at the first read, not
/// at [`end`]: a program that turns its defragmenter on late still gets
/// its drain.
exec static DRAIN_SEEN: AtomicU64 = AtomicU64::new(0);
/// [`DRAIN_SEEN`]'s split: the time above, the count's low 32 bits below.
const SEEN_SPLIT: u64 = 0x1_0000_0000;
/// The utilisation queries a defragmenter made while the drain window was
/// open, since [`DRAIN_SEEN`] last recorded progress ([`asked`]): the
/// coarse second of the first such query and of the last, each plus one,
/// as `first * 2^32 + last`; 0 for none. One word, for the reason
/// [`DRAIN_SEEN`] is one. A stall needs these to span [`DRAIN_STALL_S`]
/// ([`stalled`]), so only a defragmenter that kept looking for that long
/// and found nothing to move ends a drain: a reader of figure 8 that runs
/// no defragmenter (`challoc_cow_stats`), or a defragmenter held up
/// between two reads, ends none.
exec static DRAIN_ASKS: AtomicU64 = AtomicU64::new(0);

/// How long after an epoch its drain may run: a block above the floor
/// that no defragmenter moves by then (it is not the defragmenter's to
/// move, or none runs) stops counting, so a defragmenter does not run
/// forever for it. The bound for a program that never reads figure 8;
/// one that does sees the drain end with its work ([`draining_pairs`]).
pub const DRAIN_WINDOW_NS: u64 = 300_000_000_000;

/// How long a defragmenter's queries may go on without asking for a
/// drain move, while a program reads figure 8, before every pair's drain
/// ends ([`draining_pairs`]): the defragmenter, which runs flat out while
/// a pair drains, has passed over the heap and found nothing left that it
/// can move. On Redis and Valkey the moves come continuously until the
/// work is done (bench/cow/RESULTS.md, "Ending the drain with its work"),
/// so this only ends a drain left with blocks, in more than one slab,
/// that no defragmenter moves (Redis's allocations of other threads'
/// arenas in tests/unit/fork-drain.tcl). Seconds of the coarse
/// monotonic clock that the queries span ([`DRAIN_ASKS`]), counted from
/// the first read after [`end`].
pub const DRAIN_STALL_S: u64 = 10;

/// A pair whose live blocks above its drain floor sit in fewer slabs
/// than this has nothing worth moving: moving them would give back at
/// most one slab ([`draining_pairs`]).
pub const DRAIN_MIN_SLABS: usize = 2;

/// The smallest cap: an epoch begun over a nearly empty heap still has
/// room for the writes of a short child.
pub const MIN_CAP: u64 = 64 << 20;

/// Results of [`begin`] and [`end`], as `challoc_cow_begin` returns them.
pub const ENOTSUP: i32 = 95;
pub const EINVAL: i32 = 22;
pub const EBUSY: i32 = 16;
pub const EAGAIN: i32 = 11;

/// Whether the floors hold.
#[inline(always)]
pub fn in_force() -> bool {
    IN_FORCE.load(Ordering::Relaxed)
}

/// Whether an epoch is open (its floors may have stopped holding), or an
/// outermost [`begin`] or [`end`] is under way.
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

/// Whether some `(arena, class)` may still drain: an epoch ended less
/// than [`DRAIN_WINDOW_NS`] ago. One load when none did; the clock is read
/// only within a window, and the first read past it closes the window.
#[inline(always)]
pub fn draining() -> bool
    requires
        crate::sys::may_syscall(),
{
    let until = DRAIN_UNTIL.load(Ordering::Relaxed);
    if until == 0 {
        return false;
    }
    if crate::sys::now_coarse_ns() < until {
        true
    } else {
        // A begin and end in between stored a later deadline: keep it.
        let _ = DRAIN_UNTIL.compare_exchange(until, 0, Ordering::Relaxed, Ordering::Relaxed);
        false
    }
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

/// [`end`]'s walk: every floor goes, and becomes a drain floor where a
/// refill went above it (`ClassState::cow_settle`).
fn each_class_settle(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>)
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    let n = (shared.arenas.count() as usize).min(crate::arena::MAX_ARENAS);
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
                a.class(c).cow_settle(Tracked(&mut *ord));
                c += 1;
            }
        }
        i += 1;
    }
}

/// What [`enter`] and [`leave`] did to [`DEPTH`].
pub enum Enter {
    /// The depth went from 0 to 1 (from 1 to 0), and [`BUSY`] is set: the
    /// caller does the outermost transition's work, then clears it.
    Outermost,
    /// The depth went up (down) by one, at least one epoch staying open.
    Nested,
    /// The depth is as it was; [`begin`] or [`end`] returns this.
    Refused(i32),
}

/// [`begin`]'s change of [`DEPTH`]: one more open epoch, unless another
/// thread's outermost [`begin`] or [`end`] is under way ([`BUSY`]), when
/// it changes nothing and refuses with `EBUSY`: the epoch that thread is
/// opening has no floors yet, or the one it is closing has none left, so
/// a nested epoch counted now would return with no floors in force. A
/// child forked while another thread was in a transition starts with the
/// depth at 0 ([`fork_child`]).
fn enter() -> Enter {
    let mut tries: u32 = 0;
    while tries < MAX_TRIES
        decreases MAX_TRIES - tries,
    {
        tries += 1;
        let d = DEPTH.load(Ordering::Acquire);
        if d & BUSY != 0 || d >= MAX_DEPTH {
            return Enter::Refused(EBUSY);
        }
        if d == 0 {
            if DEPTH.compare_exchange(0, 1 | BUSY, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                return Enter::Outermost;
            }
        } else if DEPTH.compare_exchange(d, d + 1, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            return Enter::Nested;
        }
    }
    Enter::Refused(EBUSY)
}

/// [`end`]'s change of [`DEPTH`]: one fewer open epoch. `EINVAL`, with
/// the depth as it was, when no epoch is open or another thread's
/// outermost transition is under way ([`BUSY`]): an [`end`] matched to a
/// [`begin`] that returned 0 never sees one, since the depth counts that
/// epoch, and so is neither 0 before the transition nor 1 before the end
/// of the last epoch. `EAGAIN`, with the depth as it was, when other
/// threads moved the depth under each of `max_tries` compare-and-exchanges:
/// the end did not count, and its caller calls it again.
fn leave(max_tries: u32) -> Enter {
    let mut tries: u32 = 0;
    while tries < max_tries
        decreases max_tries - tries,
    {
        tries += 1;
        let d = DEPTH.load(Ordering::Acquire);
        if d & BUSY != 0 || d == 0 {
            return Enter::Refused(EINVAL);
        }
        if d == 1 {
            if DEPTH.compare_exchange(1, BUSY, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                return Enter::Outermost;
            }
        } else if DEPTH.compare_exchange(d, d - 1, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            return Enter::Nested;
        }
    }
    Enter::Refused(EAGAIN)
}

/// Start an epoch (the outermost [`begin`] acts; nested ones count): the
/// floors at every arena's commit cursor, the cap, the cache-flush stamp
/// and the `free` dispatch bit. The caller has flushed its own caches and
/// purged the `Empty` slabs (`api::cow::begin`). `ENOTSUP` with
/// `cow_epoch:false`; `EBUSY`, opening nothing, while another thread's
/// outermost [`begin`] or [`end`] is under way ([`enter`]).
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
    match enter() {
        Enter::Nested => return 0,
        Enter::Refused(e) => return e,
        Enter::Outermost => {},
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
    // The transition is done: nested begins and ends may count again.
    DEPTH.store(1, Ordering::Release);
    0
}

/// End the epoch (the outermost [`end`] acts): the floors go, each
/// becoming a drain floor where blocks of the epoch went above it, and the
/// `free` dispatch bit is cleared. `EINVAL` with no epoch open (or while
/// another thread's outermost transition is under way, [`leave`]);
/// `EAGAIN`, counting nothing, when other threads kept moving the depth
/// ([`MAX_TRIES`]): the caller ends again.
pub fn end(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>) -> (r: i32)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    end_within(shared, MAX_TRIES, Tracked(ord))
}

/// [`end`], giving up with `EAGAIN` after `max_tries` failed
/// compare-and-exchanges (0 in a test: the exhausted exit, deterministic).
pub fn end_within(shared: &Shared, max_tries: u32, Tracked(ord): Tracked<&mut LockOrder>) -> (r: i32)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    match leave(max_tries) {
        Enter::Nested => return 0,
        Enter::Refused(e) => return e,
        Enter::Outermost => {},
    }
    IN_FORCE.store(false, Ordering::Relaxed);
    FLUSH_AFTER.store(0, Ordering::Relaxed);
    shared.set_epoch(false);
    each_class_settle(shared, Tracked(&mut *ord));
    // The stall clock starts at the first read of figure 8 ([`stalled`]).
    DRAIN_SEEN.store(0, Ordering::Relaxed);
    DRAIN_UNTIL.store(crate::sys::now_coarse_ns().saturating_add(DRAIN_WINDOW_NS), Ordering::Relaxed);
    // The transition is done: the next begin may open an epoch.
    DEPTH.store(0, Ordering::Release);
    0
}

/// The child of a `fork(2)` (`api::fork_child`, while only the forking
/// thread runs): the epochs it inherited are the parent's, open for its own
/// life, so the child starts with none. Without this the child would keep
/// every free on the slow path and hold the floors until the cap, and a
/// child forked while another thread's outermost [`begin`] or [`end`] was
/// under way would inherit [`BUSY`], which no thread of it ever clears, so
/// compaction and young-heap rehome would stay off and its own epochs
/// would be refused for good. The floors stay stored but stop holding with
/// [`IN_FORCE`] clear ([`ClassState::cow_floor`]), and the child's next
/// [`begin`] marks every pair again.
pub fn fork_child(shared: &Shared) {
    if DEPTH.load(Ordering::Relaxed) == 0 {
        return;
    }
    IN_FORCE.store(false, Ordering::Relaxed);
    FLUSH_AFTER.store(0, Ordering::Relaxed);
    shared.set_epoch(false);
    DEPTH.store(0, Ordering::Release);
}

/// Test only: the floors hold, or stop holding, with no epoch begun, for
/// a test of an allocator of its own (`huge::grow_tests`), in a fork
/// child.
#[cfg(test)]
pub fn test_set_in_force(on: bool) {
    IN_FORCE.store(on, Ordering::Relaxed);
}

/// Test only: an outermost transition under way in another thread
/// ([`BUSY`]), as a fork child inherits it.
#[cfg(test)]
pub fn test_set_busy() {
    DEPTH.fetch_or(BUSY, Ordering::Relaxed);
}

/// Test only: the drain's last progress ([`DRAIN_SEEN`]) `secs` earlier,
/// as if no drain move had been asked for in that long; nothing before
/// the first read since [`end`], when there is no progress to age.
#[cfg(test)]
pub fn test_age_drain_progress(secs: u64) {
    let w = DRAIN_SEEN.load(Ordering::Relaxed);
    if w != 0 {
        let t = (w / SEEN_SPLIT).saturating_sub(secs).max(1);
        DRAIN_SEEN.store(t * SEEN_SPLIT + w % SEEN_SPLIT, Ordering::Relaxed);
    }
}

/// Test only: the first query since the drain's last progress
/// ([`DRAIN_ASKS`]) `first` seconds earlier and the last `last` seconds
/// earlier, as if a defragmenter had been asking over that span; nothing
/// before the first query.
#[cfg(test)]
pub fn test_age_drain_asks(first: u64, last: u64) {
    let w = DRAIN_ASKS.load(Ordering::Relaxed);
    if w != 0 {
        let f = (w / SEEN_SPLIT).saturating_sub(first).max(1);
        let l = (w % SEEN_SPLIT).saturating_sub(last).max(1);
        DRAIN_ASKS.store(f * SEEN_SPLIT + l, Ordering::Relaxed);
    }
}

/// Record a utilisation query made while the drain window is open
/// ([`DRAIN_ASKS`]): the first since the last progress sets the first
/// second, and each sets the last. Stores only when the second changed,
/// and one compare-and-exchange try (a query that loses it was in the
/// same second as the one that won, or the progress just reset it).
fn asked()
    requires
        crate::sys::may_syscall(),
{
    let s = (crate::sys::now_coarse_ns() / 1_000_000_000) % 0x8000_0000 + 1;
    let w = DRAIN_ASKS.load(Ordering::Relaxed);
    let first = if w == 0 { s } else { w / SEEN_SPLIT };
    if w != 0 && w % SEEN_SPLIT == s {
        return;
    }
    proof {
        assert(first * 0x1_0000_0000 + s <= u64::MAX) by (nonlinear_arith)
            requires
                first <= 0xffff_ffff,
                s <= 0x8000_0000,
        ;
    }
    let _ = DRAIN_ASKS.compare_exchange(w, first * SEEN_SPLIT + s, Ordering::Relaxed, Ordering::Relaxed);
}

/// The drain's rule (design 5, "Fork epochs"): a block of slab `slab`
/// moves back below the pair's drain floor `floor` when there is one, the
/// slab is at or above it, and the lowest partial slab `low` (where a
/// direct claim puts the moved block) lies below it.
pub open spec fn drain_ok(slab: int, floor: int, low: Option<usize>) -> bool {
    &&& floor != 0
    &&& floor <= slab
    &&& low matches Some(s) && (s as int) < floor
}

/// Whether a defragmenter should move a block of slab `slab` of `cs`
/// back below the pair's drain floor ([`drain_ok`] over the floor and the
/// lowest partial slab it read, returned as ghosts: the slab a direct
/// claim takes from next, `lowest_partial`). The first query that finds
/// no partial slab below the floor ends the pair's drain. The callers
/// ask only while [`draining`], and every query, of any pair, counts as
/// a defragmenter looking ([`asked`]). `ord` is the caller's lock-order
/// token (ghost) for the class lock `lowest_partial` takes.
pub fn drain_move(cs: &ClassState, slab: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (bool, Ghost<usize>, Ghost<Option<usize>>))
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
        r.0 ==> drain_ok(slab as int, r.1@ as int, r.2@),
{
    asked();
    let f = cs.drain_floor();
    if f == 0 || slab < f || !draining() {
        return (false, Ghost(f), Ghost(None));
    }
    let low = cs.lowest_partial(Tracked(ord));
    match low {
        Some(s) if s < f => {
            DRAIN_MOVES.fetch_add(1, Ordering::Relaxed);
            (true, Ghost(f), Ghost(low))
        },
        _ => {
            cs.drain_done();
            (false, Ghost(f), Ghost(low))
        },
    }
}

/// The figures `challoc_cow_stat(which)` reports (`<challoc/challoc.h>`'s
/// `CHALLOC_COW_*`): 0 epochs begun, 1 epochs open (depth), 2 floors in
/// force, 3 epochs the cap ended early, 4 bytes reserved under the floors
/// of the current or last epoch, 5 its cap, 6 deflected frees, 7 drain
/// moves suggested, 8 `(arena, class)` pairs still draining. 0 for any
/// other `which`. `ord` is the caller's lock-order token (ghost) for the
/// class locks figure 8 takes.
pub fn stat(shared: &Shared, which: u32, Tracked(ord): Tracked<&mut LockOrder>) -> u64
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    match which {
        8 => draining_pairs(shared, Tracked(ord)),
        _ => stat_unlocked(which),
    }
}

/// [`stat`]'s figures 0 to 7, which take no lock; 0 for any other.
pub fn stat_unlocked(which: u32) -> u64 {
    match which {
        0 => EPOCHS.load(Ordering::Relaxed),
        1 => (DEPTH.load(Ordering::Relaxed) & !BUSY) as u64,
        2 => IN_FORCE.load(Ordering::Relaxed) as u64,
        3 => EXPIRED.load(Ordering::Relaxed),
        4 => CLAIMED.load(Ordering::Relaxed),
        5 => CAP.load(Ordering::Relaxed),
        6 => DEFLECTED.load(Ordering::Relaxed),
        7 => DRAIN_MOVES.load(Ordering::Relaxed),
        _ => 0,
    }
}

/// `(arena, class)` pairs whose drain floor is set, without a lock (the
/// stats report).
pub fn drain_floors(shared: &Shared) -> u64 {
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

/// [`DRAIN_SEEN`]'s word for coarse monotonic time `now_ns` and drain-move
/// count `moves`: `(t + 1) * 2^32 + m`, with `t` the time in seconds
/// modulo 2^31 and `m` the count's low 32 bits. Never 0.
fn seen_word(now_ns: u64, moves: u64) -> (r: u64)
    ensures
        r != 0,
{
    let t = (now_ns / 1_000_000_000) % 0x8000_0000;
    let m = moves % SEEN_SPLIT;
    proof {
        assert((t + 1) * 0x1_0000_0000 + m <= u64::MAX && (t + 1) * 0x1_0000_0000 > 0) by (nonlinear_arith)
            requires
                t < 0x8000_0000,
                m < 0x1_0000_0000,
        ;
    }
    (t + 1) * SEEN_SPLIT + m
}

/// Whether the drain stopped making progress: no query asked for a drain
/// move ([`DRAIN_MOVES`]) in the last [`DRAIN_STALL_S`] seconds between
/// reads of figure 8, while a defragmenter's queries went on over that
/// many seconds ([`DRAIN_ASKS`]). The first read since [`end`], and any
/// read that finds the count changed, clears the queries and records the
/// count and the time in [`DRAIN_SEEN`] with one compare-and-exchange (a
/// reader that loses it saw progress too) and answers no. The clear comes
/// first and the record publishes it (Release; a reader loads the record
/// with Acquire), so a reader that sees the record judges by the queries
/// made since.
fn stalled() -> bool
    requires
        crate::sys::may_syscall(),
{
    let moves = DRAIN_MOVES.load(Ordering::Relaxed);
    let fresh = seen_word(crate::sys::now_coarse_ns(), moves);
    let w = DRAIN_SEEN.load(Ordering::Acquire);
    let at = w / SEEN_SPLIT;
    let now = fresh / SEEN_SPLIT;
    // `now < at`: the clock's seconds wrapped past 2^31, start again.
    if w == 0 || w % SEEN_SPLIT != moves % SEEN_SPLIT || now < at {
        DRAIN_ASKS.store(0, Ordering::Relaxed);
        let _ = DRAIN_SEEN.compare_exchange(w, fresh, Ordering::Release, Ordering::Relaxed);
        return false;
    }
    let a = DRAIN_ASKS.load(Ordering::Relaxed);
    let first = a / SEEN_SPLIT;
    let last = a % SEEN_SPLIT;
    now - at >= DRAIN_STALL_S && a != 0 && last >= first && last - first >= DRAIN_STALL_S
}

/// End every pair's drain and close the window, after a stall
/// ([`draining_pairs`]), while holding [`DEPTH`]'s [`BUSY`] bit over no
/// open epoch, so no [`end`] can set a new epoch's drain floors while
/// this clears them (a reader that judged by the last epoch's progress
/// would otherwise end the new drain before it starts). False, ending
/// nothing, when an epoch is open or a transition is under way: the next
/// read judges again. A [`begin`] in another thread meanwhile is refused
/// (`EBUSY`), as during another thread's transition.
fn end_all_drains(shared: &Shared) -> bool {
    if DEPTH.compare_exchange(0, BUSY, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return false;
    }
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
                let cs: &ClassState = a.class(c);
                if cs.drain_floor() != 0 {
                    cs.drain_done();
                }
                c += 1;
            }
        }
        i += 1;
    }
    DRAIN_UNTIL.store(0, Ordering::Relaxed);
    DEPTH.store(0, Ordering::Release);
    true
}

/// `(arena, class)` pairs still draining, with work left worth a
/// defragmenter's effort: a drain floor set, the drain window open, a
/// slab below the floor with a free slot to move a block into, live
/// blocks above it in at least [`DRAIN_MIN_SLABS`] slabs, and no stall
/// ([`stalled`]: a defragmenter's queries went on for [`DRAIN_STALL_S`]
/// without asking for a drain move). A pair that fails one of the first
/// three ends its drain here, for good (its drain floor goes), and a
/// stall ends every pair's and closes the window ([`end_all_drains`]),
/// so the figure never rises again before the next epoch: blocks in one
/// slab are at most that slab to give back, and a defragmenter that has
/// gone a stall without a move has found nothing left it can move. 0
/// once the window closed.
fn draining_pairs(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>) -> u64
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    if !draining() {
        return 0;
    }
    if stalled() && end_all_drains(shared) {
        return 0;
    }
    let n = (shared.arenas.count() as usize).min(crate::arena::MAX_ARENAS);
    let mut k: u64 = 0;
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
                let f = cs.drain_floor();
                if f != 0 {
                    let holes = match cs.lowest_partial(Tracked(&mut *ord)) {
                        Some(s) => s < f,
                        None => false,
                    };
                    if holes && cs.slabs_live_at_or_above(f, DRAIN_MIN_SLABS) >= DRAIN_MIN_SLABS {
                        k = k.saturating_add(1);
                    } else {
                        cs.drain_done();
                    }
                }
                c += 1;
            }
        }
        i += 1;
    }
    k
}

} // verus!
