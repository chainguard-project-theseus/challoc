//! Decay and purge (design.md section 8) and the background thread (8.1).
//!
//! - [`Batch`], the discard primitive. Callers collect up to [`MAX_BATCH`]
//!   ranges, merging adjacent ones, and flush them with a loop of
//!   `madvise`. The advice is decided at each flush ([`advice`], design 8
//!   and 11): `MADV_FREE` under the cgroup policy's headroom band
//!   ([`set_headroom`]), `MADV_DONTNEED` everywhere else and always for an
//!   explicit purge ([`purge_all`], [`purge_arena`]). The flush reports
//!   each range to the caller as discarded or not, and the caller changes
//!   state only for the discarded ones: a range `madvise` refuses (`EINVAL`
//!   on `mlock`ed pages) stays `Empty`, dirty and never assumed zero, and
//!   is counted in [`failed_discards`] instead of [`discarded_bytes`]. The
//!   next explicit purge retries it; a decay walk that hit one backs its
//!   owner off for [`DISCARD_RETRY_NS`] ([`Decayer::back_off`]). Any
//!   other errno (`ENOMEM`: a reservation of ours is gone; `EPERM`: a
//!   sealed range) is a heap inconsistency and aborts.
//! - The decay curve ([`Curve`], `purge/curve.rs`) and its driver
//!   ([`decay`]): jemalloc's decay shape, a smootherstep from one to zero
//!   over the `dirty_decay_ms` window. Each `(arena, class)` and the huge
//!   allocator purge their oldest `Empty` slabs (extents) until what is left
//!   is within the curve's allowance, at most once per step; [`decay`]
//!   visits them all and reports the earliest next deadline. Each class
//!   keeps the `dirty_at` of its oldest `Empty` slab and its next deadline
//!   in atomics, so the visit takes no lock unless a walk is due.
//!   [`on_tick`] is the slow path's hook (design 6.4).
//! - The background thread ([`background`], `purge/background.rs`): created
//!   on the first periodic tick after it was requested, or by default on
//!   the first one that finds enough dirty memory waiting (the lazy start,
//!   [`background::auto_tick`]), never from init, on
//!   a stack from the metadata region with a guard page below it and every
//!   signal blocked. It loops over `decay` and sleeps on a futex until the
//!   next deadline; a class whose first slab goes `Empty` wakes it. While it
//!   exists the slow path does no purging of its own. `pthread_create`
//!   allocates, so the creating thread parks its TLS slot on the
//!   `TLS_CREATING` sentinel and that allocation is served straight from the
//!   slabs (design 6.5).
//!
//! `MADV_FREE`d memory keeps its contents until the kernel reclaims it, so
//! [`purged_is_zero`] is true until the first lazy batch of the process and
//! false from then on: `calloc` clears `Purged` memory it reuses after that,
//! and `Fresh` memory stays known zero.
//!
//! The curve's tests sit in `purge/curve.rs`, the other pure tests and the
//! `serial` lock the class tests share in `purge/tests.rs`, and the ones that
//! need a kernel in `purge/linux_tests.rs`.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::arena::Arena;
use crate::heap::Shared;
pub use crate::policy::Advice;
use crate::sys;

mod curve;
#[cfg(test)]
mod linux_tests;
#[cfg(test)]
pub(crate) mod tests;

pub use curve::{Curve, NEVER, STEPS, decay_ns_from_ms};

/// Ranges per batch (design section 8).
pub const MAX_BATCH: usize = 64;

/// How long a decay walk whose discard failed holds its owner's next walk
/// off ([`Decayer::back_off`]): the memory is most likely locked, and a
/// zero or short period would otherwise retry it at every tick.
pub const DISCARD_RETRY_NS: u64 = 1_000_000_000;

/// `lazy_band` (default true): `MADV_FREE` under the headroom band.
static LAZY_BAND: AtomicBool = AtomicBool::new(true);
/// The policy's last evaluation put usage under 70% of a known limit with
/// no eager return of the excess over the cap in progress
/// (`policy::evaluate` mirrors `Policy::headroom` here).
static HEADROOM: AtomicBool = AtomicBool::new(false);
/// The policy's last evaluation found an eager return of the excess over
/// the cap in progress (`Policy::eager_excess`; the reason line only).
static EAGER_EXCESS: AtomicBool = AtomicBool::new(false);
/// Explicit purges in flight ([`purge_all`], [`purge_arena`]): eager.
static EXPLICIT: AtomicU32 = AtomicU32::new(0);
/// A lazy batch has been issued in this process: `Purged` memory is no
/// longer known to be zero (the module doc). Sticky.
static LAZY_EVER: AtomicBool = AtomicBool::new(false);
/// Batches that discarded anything, and bytes discarded, with each advice
/// (the report, `stats.cgroup.*`).
static LAZY_BATCHES: AtomicU64 = AtomicU64::new(0);
static EAGER_BATCHES: AtomicU64 = AtomicU64::new(0);
static LAZY_BYTES: AtomicU64 = AtomicU64::new(0);
static EAGER_BYTES: AtomicU64 = AtomicU64::new(0);
/// Pieces and bytes whose discard failed (the report).
static FAILED_PIECES: AtomicU64 = AtomicU64::new(0);
static FAILED_BYTES: AtomicU64 = AtomicU64::new(0);
/// `check_zero`: the debug mode that reads every slot or page handed out
/// as "known zero" and aborts on a dirty word.
static CHECK_ZERO: AtomicBool = AtomicBool::new(false);

/// The `lazy_band` key of the two-advice purge. Called once at init,
/// before anything can be purged, and by the tests.
pub fn configure_advice(lazy_band: bool) {
    LAZY_BAND.store(lazy_band, Ordering::Relaxed);
}

/// The policy's mirror: `low`, usage is under 70% of a known limit
/// (design 11's `Low` band) and no eager return of the excess over the cap
/// is in progress (`Policy::headroom`); `eager_excess`, such a return is
/// in progress (the reason line). Written where the policy is evaluated,
/// read at each flush.
pub fn set_headroom(low: bool, eager_excess: bool) {
    HEADROOM.store(low, Ordering::Relaxed);
    EAGER_EXCESS.store(eager_excess, Ordering::Relaxed);
}

/// The advice the next batch flush would issue (the module doc).
#[inline]
pub fn advice() -> Advice {
    crate::policy::advice(
        HEADROOM.load(Ordering::Relaxed),
        LAZY_BAND.load(Ordering::Relaxed),
        EXPLICIT.load(Ordering::Relaxed) != 0,
    )
}

/// Why [`advice`] is what it is, for the report:
/// `under 70% of the limit`, `explicit purge`, `lazy_band:false`,
/// `returning the excess over the cap` (an eager return in progress,
/// `Policy::eager_excess`), or `no headroom known` (no limit, or 70% and
/// above).
#[cold]
pub fn advice_reason() -> &'static str {
    if EXPLICIT.load(Ordering::Relaxed) != 0 {
        "explicit purge"
    } else if !LAZY_BAND.load(Ordering::Relaxed) {
        "lazy_band:false"
    } else if HEADROOM.load(Ordering::Relaxed) {
        "under 70% of the limit"
    } else if EAGER_EXCESS.load(Ordering::Relaxed) {
        "returning the excess over the cap"
    } else {
        "no headroom known"
    }
}

/// `(lazy batches, eager batches)` that discarded anything so far.
pub fn batches() -> (u64, u64) {
    (
        LAZY_BATCHES.load(Ordering::Relaxed),
        EAGER_BATCHES.load(Ordering::Relaxed),
    )
}

/// `(lazy bytes, eager bytes)` discarded so far.
pub fn discarded_bytes() -> (u64, u64) {
    (
        LAZY_BYTES.load(Ordering::Relaxed),
        EAGER_BYTES.load(Ordering::Relaxed),
    )
}

/// `(pieces, bytes)` whose discard failed so far: the ranges as pushed (a
/// page group, an extent) that stayed dirty (the module doc), the middle
/// pieces of a refused merged range left without a call included
/// ([`Batch`]). A piece retried and refused again counts again.
pub fn failed_discards() -> (u64, u64) {
    (
        FAILED_PIECES.load(Ordering::Relaxed),
        FAILED_BYTES.load(Ordering::Relaxed),
    )
}

/// An explicit purge in progress: every flush until the guard drops is
/// eager (the caller asked for RSS to drop), whatever the band.
struct Explicit;

impl Explicit {
    fn enter() -> Explicit {
        EXPLICIT.fetch_add(1, Ordering::Relaxed);
        Explicit
    }
}

impl Drop for Explicit {
    fn drop(&mut self) {
        EXPLICIT.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Turn the `check_zero` debug mode on or off (init, from the config key;
/// the tests, directly).
pub fn set_check_zero(on: bool) {
    CHECK_ZERO.store(on, Ordering::Relaxed);
}

/// Whether the `check_zero` debug mode is on: `ClassState::claim` and
/// `Huge::alloc` then read the first and last word of every slot or page
/// they report as zero and abort on a non-zero one. Off by default; the
/// callers test it only on the paths that already hold a class or huge
/// lock, so the release fast paths never see it.
#[inline]
pub fn check_zero() -> bool {
    CHECK_ZERO.load(Ordering::Relaxed)
}

/// The advice in force for diagnostics, e.g. `MADV_DONTNEED`; it reads
/// `MADV_DONTNEED (some lazy batches issued)` once any lazy batch went
/// out, since that is what makes `Purged` memory unknown-zero.
#[cold]
pub fn advice_name() -> &'static str {
    match (advice(), purged_is_zero()) {
        (Advice::Eager, false) => "MADV_DONTNEED (some lazy batches issued)",
        (a, _) => a.madv_name(),
    }
}

/// Whether memory in the `Purged` state is known to read as zero: until
/// the first `MADV_FREE` batch of the process (the module doc), which
/// leaves the old contents in place until the kernel gets around to
/// reclaiming the pages; false from then on, for good.
#[inline]
pub fn purged_is_zero() -> bool {
    !LAZY_EVER.load(Ordering::Relaxed)
}

/// Up to [`MAX_BATCH`] page ranges to discard together.
///
/// Invariant the callers uphold (the reason [`Batch::push`] and
/// [`Batch::flush`] are safe functions over an `madvise` that drops page
/// contents): every range pushed is a page-aligned part of one of this
/// process's own reservations whose pages are *free* (an `Empty` slab, a
/// freed huge run: reachable through no live pointer) and stay so until
/// the flush returns. The callers (`ClassState::purge_empty` and
/// `purge_curve`, `Huge::free_run` through [`discard_one`], `purge_empty`
/// and `purge_curve`) push under the class or huge lock that keeps the
/// ranges free and flush before releasing it; the batch never outlives
/// that hold.
///
/// The memory stays in its free state (`Empty`) until the flush reports
/// it discarded: `push` and `flush` take the caller's `settle(addr, len,
/// discarded)`, which the flush (a full batch flushes inside `push`)
/// calls for every range it issued, and the caller makes the memory
/// `Purged` only when `discarded` is true. Only ranges pushed with the
/// same length merge (a walk's page groups); a merged range the kernel
/// refuses is issued again piece by piece, as pushed, and each piece is
/// reported on its own, so a locked page keeps only its own piece dirty.
/// A piece `madvise` refused part of is reported not discarded, and the
/// bytes a refused call did discard are not counted.
///
/// The re-issue starts with the first and the last piece; when both are
/// refused too, the middle ones are reported refused without a call.
/// Under `mlockall` every range is refused, and a merged range of `n`
/// pieces then costs 3 calls instead of `n + 1` (64 MiB of 4 KiB groups
/// would be ~16k `EINVAL`s per walk, under the class lock). The price is
/// a range locked at both ends and not in the middle: its unlocked middle
/// stays dirty, which is always safe (only not returned yet), until an
/// end is unlocked or reused, or a later batch merges the run another
/// way. One locked page in an otherwise unlocked run, the common case,
/// still keeps only its own piece dirty.
pub struct Batch {
    /// `(addr, len, piece)`: `len` is a multiple of `piece`, the length
    /// of each range merged into it.
    ranges: [(usize, usize, usize); MAX_BATCH],
    n: usize,
}

impl Batch {
    pub const fn new() -> Batch {
        Batch {
            ranges: [(0, 0, 0); MAX_BATCH],
            n: 0,
        }
    }

    #[inline]
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.n
    }

    #[inline]
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.n == MAX_BATCH
    }

    /// Add `[addr, addr + len)` (free, per the type's invariant), flushing
    /// first, with `settle`, if the batch is full.
    pub fn push(&mut self, addr: usize, len: usize, settle: impl FnMut(usize, usize, bool)) {
        // Adjacent ranges of one length pushed in address order (a run of
        // empty 4 KiB slabs) go out as one.
        if self.n != 0 {
            let (last, last_len, piece) = self.ranges[self.n - 1];
            if last + last_len == addr && len == piece {
                self.ranges[self.n - 1].1 = last_len + len;
                return;
            }
        }
        if self.is_full() {
            self.flush(settle);
        }
        self.ranges[self.n] = (addr, len, len);
        self.n += 1;
    }

    /// Discard every range with `madvise`, report each to `settle(addr,
    /// len, discarded)` (a refused merged range piece by piece, the type's
    /// doc), then empty the batch. The advice is [`advice`] at this moment;
    /// a batch that discarded anything is counted, and so are the bytes
    /// discarded ([`discarded_bytes`]; a refused piece counts in
    /// [`failed_discards`]). A lazy flush makes `Purged` memory
    /// unknown-zero for the rest of the process (the module doc), which is
    /// published before the discard so a claim of one of these ranges
    /// after the caller's lock drops sees it. The ranges are free (the
    /// type's invariant), which is what makes the discard sound.
    pub fn flush(&mut self, mut settle: impl FnMut(usize, usize, bool)) {
        if self.n != 0 {
            let advice = advice();
            if let Advice::Lazy = advice {
                LAZY_EVER.store(true, Ordering::Relaxed);
            }
            let mut bytes = 0u64;
            // The ranges by index and each range's pieces by address
            // (`for` over a slice and a `step_by` range, spelled out:
            // Verus does not specify those iterators).
            let mut i = 0;
            while i < self.n {
                let (addr, len, piece) = self.ranges[i];
                if discard(addr, len, advice) {
                    bytes += len as u64;
                    settle(addr, len, true);
                    i += 1;
                    continue;
                }
                // A range of one piece is not issued twice; when its first
                // and last pieces are refused as well, neither are the
                // middle ones (the type's doc).
                let last = addr + len - piece;
                let first_done = if piece < len {
                    discard(addr, piece, advice)
                } else {
                    false
                };
                let last_done = if piece < len {
                    discard(last, piece, advice)
                } else {
                    false
                };
                let middle = first_done || last_done;
                let mut at = addr;
                while at < addr + len {
                    let done = if at == addr {
                        first_done
                    } else if at == last {
                        last_done
                    } else if middle {
                        discard(at, piece, advice)
                    } else {
                        false
                    };
                    if done {
                        bytes += piece as u64;
                        settle(at, piece, true);
                    } else {
                        FAILED_PIECES.fetch_add(1, Ordering::Relaxed);
                        FAILED_BYTES.fetch_add(piece as u64, Ordering::Relaxed);
                        settle(at, piece, false);
                    }
                    at += piece;
                }
                i += 1;
            }
            if bytes != 0 {
                let (batches, total) = match advice {
                    Advice::Lazy => (&LAZY_BATCHES, &LAZY_BYTES),
                    Advice::Eager => (&EAGER_BATCHES, &EAGER_BYTES),
                };
                batches.fetch_add(1, Ordering::Relaxed);
                total.fetch_add(bytes, Ordering::Relaxed);
            }
            self.n = 0;
        }
    }
}

/// Discard `[addr, addr + len)` alone, free per the [`Batch`] invariant,
/// as a batch of one: whether it was discarded (`Huge::free_run`'s purge
/// at free time).
pub fn discard_one(addr: usize, len: usize) -> bool {
    let mut discarded = false;
    let mut batch = Batch::new();
    batch.push(addr, len, |_, _, _| {});
    batch.flush(|_, _, d| discarded = d);
    discarded
}

/// The step gate of design section 8, one per `ClassState` and one in
/// the huge allocator: `dirty_at` of the oldest `Empty` member (0 when
/// none is known), written under the owner's lock and read without it by
/// the decay driver, and the deadline of the owner's next walk.
/// [`Decayer::decay`] runs the owner's walk at most once per step of the
/// curve, and not before the back-off a failed discard set has passed;
/// the walks themselves (`ClassState::purge_curve`, `Huge::purge_curve`)
/// differ and stay with their owners.
pub struct Decayer {
    /// `dirty_at` of the oldest `Empty` member, 0 when none. It may lag
    /// behind a claim that reused the oldest member, in which case it is
    /// too early rather than too late; the next walk recomputes it.
    oldest_empty: AtomicU64,
    /// When the next walk is due; 0 before the first.
    next_walk: AtomicU64,
    /// No walk before this ([`Decayer::back_off`]); 0 until a walk's
    /// discard fails.
    retry_at: AtomicU64,
}

impl Decayer {
    pub const fn new() -> Decayer {
        Decayer {
            oldest_empty: AtomicU64::new(0),
            next_walk: AtomicU64::new(0),
            retry_at: AtomicU64::new(0),
        }
    }

    /// A walk at `now` could not discard some of what it chose (the
    /// module doc): hold the owner's walks off for [`DISCARD_RETRY_NS`],
    /// whatever the period, so memory `madvise` keeps refusing is retried
    /// once a second rather than at every tick. Called by the walk, under
    /// the owner's lock.
    pub fn back_off(&self, now: u64) {
        self.retry_at
            .store(now.saturating_add(DISCARD_RETRY_NS), Ordering::Relaxed);
    }

    /// `dirty_at` of the oldest `Empty` member as the driver sees it (0
    /// when none).
    #[inline]
    pub fn oldest_empty(&self) -> u64 {
        self.oldest_empty.load(Ordering::Relaxed)
    }

    /// Publish the oldest `dirty_at` (0 for none) after a walk or a list
    /// change. Returns `true` when the owner went from no `Empty` member
    /// to one, which is when a background thread sleeping without a
    /// deadline needs waking ([`note_empty`]).
    pub fn set_oldest_empty(&self, dirty_at: u64) -> bool {
        let old = self.oldest_empty.swap(dirty_at, Ordering::Relaxed);
        old == 0 && dirty_at != 0
    }

    /// A member went `Empty` at `now`: record it as the oldest if none is
    /// known. Returns `true` when it is the first (see
    /// [`set_oldest_empty`]).
    ///
    /// [`set_oldest_empty`]: Decayer::set_oldest_empty
    pub fn note_first_empty(&self, now: u64) -> bool {
        if self.oldest_empty.load(Ordering::Relaxed) != 0 {
            return false;
        }
        self.oldest_empty.store(now, Ordering::Relaxed);
        true
    }

    /// Decay step: once the oldest `Empty` member is a step of the curve
    /// old, run `walk(now, curve)` (which purges along the curve and
    /// republishes the oldest `dirty_at`) at most once per step and not
    /// during a back-off. Returns the owner's next deadline (the end of the
    /// oldest member's first step, of the back-off, or of the current
    /// step), `None` when nothing is `Empty`. Lock-free unless a walk is
    /// due.
    pub fn decay(&self, now: u64, decay_ns: u64, walk: impl FnOnce(u64, &Curve)) -> Option<u64> {
        let oldest = self.oldest_empty();
        if oldest == 0 {
            return None;
        }
        let curve = Curve::new(decay_ns);
        let step = curve.step_ns();
        let first = oldest.saturating_add(step);
        if now < first {
            return Some(first);
        }
        let retry = self.retry_at.load(Ordering::Relaxed);
        if now < retry {
            return Some(retry);
        }
        // A deadline further away than one step was set under a longer
        // period since shortened: walk now.
        let next = self.next_walk.load(Ordering::Relaxed);
        if now < next && next - now <= step {
            return Some(next);
        }
        walk(now, &curve);
        let next = now.saturating_add(step);
        self.next_walk.store(next, Ordering::Relaxed);
        if self.oldest_empty() == 0 {
            None
        } else {
            Some(next.max(self.retry_at.load(Ordering::Relaxed)))
        }
    }
}

/// Run every decay walk that is due at `now` (coarse monotonic
/// nanoseconds) and return the earliest future deadline, `None` when
/// nothing is `Empty` or decay is off. Lock-free for a class with nothing
/// due.
///
/// Never inlined, so `Heap::tick` (through `on_tick`) and
/// `background::main` keep a call here rather than a copy of the loop.
#[inline(never)]
pub fn decay(shared: &Shared, now: u64) -> Option<u64> {
    // Design 11: a `memory.events` `high` rise owes one full purge, run
    // before the walks so they see the survivors (none, normally).
    let policy = &shared.policy;
    if policy.take_full_purge() {
        purge_all(shared);
    }
    let keep16 = policy.keep16();
    let mut next: Option<u64> = None;
    let count = arena_count(shared);
    for i in 0..count {
        if let Some(arena) = shared.arenas.get_existing(i as u16) {
            next = earliest(next, decay_one(shared, arena, now, keep16));
        }
    }
    next = earliest(next, decay_huge(shared, now, keep16));
    next
}

/// `arena.<i>.decay`: the decay walks of one arena (plus, for arena 0,
/// the huge extents, which belong to no single arena's sets) that are due
/// at `now`, on the periods and keep ratio [`decay`] gives them. `None`
/// if the arena does not exist.
pub fn decay_arena(shared: &Shared, index: u16, now: u64) -> Option<()> {
    let arena = shared.arenas.get_existing(index)?;
    let keep16 = shared.policy.keep16();
    decay_one(shared, arena, now, keep16);
    if index == 0 {
        decay_huge(shared, now, keep16);
    }
    Some(())
}

/// One arena's walks on its own period, scaled by the policy's band.
fn decay_one(shared: &Shared, arena: &Arena, now: u64, keep16: u32) -> Option<u64> {
    arena.decay_with(
        now,
        shared.policy.effective_decay_ns(arena.decay_ns()),
        keep16,
    )
}

/// The huge allocator's walk. It is shared by every arena: its own period
/// when `huge_dirty_decay_ms` is set, else arena 0's (design 7), scaled by
/// the policy's band.
fn decay_huge(shared: &Shared, now: u64, keep16: u32) -> Option<u64> {
    let decay_ns = shared.policy.effective_decay_ns(shared.huge_decay_ns());
    if decay_ns == NEVER {
        return None;
    }
    shared.huge.decay_with(now, decay_ns, keep16)
}

/// The earlier of two optional deadlines; either one when the other is
/// `None`.
#[inline(always)]
fn earliest(next: Option<u64>, d: Option<u64>) -> Option<u64> {
    match d {
        Some(d) => Some(next.map_or(d, |n| n.min(d))),
        None => next,
    }
}

/// Arena indices to visit: [`crate::arena::Arenas::count`], which never
/// exceeds the table (`create` stops at [`MAX_ARENAS`]; the clamp is
/// redundant and keeps the bound visible to the verifier).
///
/// [`MAX_ARENAS`]: crate::arena::MAX_ARENAS
#[inline]
fn arena_count(shared: &Shared) -> usize {
    (shared.arenas.count() as usize).min(crate::arena::MAX_ARENAS)
}

/// Purge every `Empty` slab of every arena and every huge extent
/// regardless of age, eagerly (`malloc_trim(0)`, the `high`-rise full
/// purge: the memory is wanted back now). Returns how many were purged.
pub fn purge_all(shared: &Shared) -> usize {
    let _explicit = Explicit::enter();
    let mut n: usize = 0;
    let count = arena_count(shared);
    for i in 0..count {
        if let Some(arena) = shared.arenas.get_existing(i as u16) {
            n += arena.purge_all();
        }
    }
    n + shared.huge.purge_empty(u64::MAX)
}

/// `arena.<i>.purge`: purge every `Empty` slab of one arena regardless of
/// age (plus, for arena 0, the huge extents, which belong to no single
/// arena's sets), eagerly. `None` if the arena does not exist.
pub fn purge_arena(shared: &Shared, index: u16) -> Option<usize> {
    let _explicit = Explicit::enter();
    let arena = shared.arenas.get_existing(index)?;
    let mut n = arena.purge_all();
    if index == 0 {
        n += shared.huge.purge_empty(u64::MAX);
    }
    Some(n)
}

/// Periodic-work hook (design 6.4): start the background thread if one was
/// requested and does not exist, otherwise run decay here unless the
/// thread is doing it, and let the lazy start look at what decay left.
/// `alloc` is whether the tick is an allocation's: a `free`'s tick never
/// creates (or joins) a thread, because glibc frees with its stack cache
/// lock held (`pthread_join`'s TLS teardown, the stack cache trim) and
/// `pthread_create` takes that lock again. Such a tick purges in the
/// foreground and leaves the start to the next allocation's tick.
pub fn on_tick(shared: &Shared, now: u64, alloc: bool) {
    sample(shared, now);
    if background::present() {
        return;
    }
    if alloc && background::requested() && background::start_requested(shared) {
        return;
    }
    if decay(shared, now).is_some() {
        // Dirty memory is still waiting: the lazy start's look.
        background::auto_tick(shared, now, alloc);
    }
}

/// Design 11: `memory.current` and `memory.events`, at most once per 10 ms
/// of the coarse clock, and the policy evaluated on the sample that was
/// taken; nothing at all when the cgroup is untracked or the window has
/// not passed. Slow paths only (the periodic tick, the background thread).
fn sample(shared: &Shared, now: u64) {
    if shared.discover_cgroup_if_due(now) {
        return;
    }
    // `shared.cgroup().is_some_and(|cg| cg.sample(now))`, spelled out:
    // Verus does not specify `Option::is_some_and`.
    let sampled = match shared.cgroup() {
        Some(cg) => cg.sample(now),
        None => false,
    };
    if sampled {
        crate::policy::evaluate(shared);
    }
}

/// A class or the huge allocator went from no `Empty` slab to one: wake a
/// background thread that may be sleeping without a deadline.
#[inline]
pub fn note_empty() {
    background::wake();
}

/// A decay period changed (`arena.<i>.dirty_decay_ms`, `malloc_conf` at
/// run time): wake a background thread whose deadline was computed with
/// the old period, so a shorter period takes effect now rather than at
/// the next wake-up.
pub fn note_decay_changed() {
    background::wake();
}

/// Issue `advice` on `[addr, addr + len)`, which is free (the [`Batch`]
/// invariant; this is the one place the batch's ranges reach the
/// kernel). Whether the kernel took it: on a refusal (`EINVAL` for a
/// range with `mlock`ed pages, under `mlockall` every range) the pages
/// keep their contents, or some of them do (Linux applies the advice to
/// the VMAs before the one it refuses). Every supported kernel has
/// `MADV_FREE` (design 17), so `EINVAL` is never an unknown advice. Any
/// other errno aborts: `ENOMEM` (part
/// of the range not mapped) means one of our reservations is gone,
/// `EPERM` that the program sealed our free memory, and anything else is
/// unexplained.
fn discard(addr: usize, len: usize, advice: Advice) -> bool {
    assert!(
        {
            let page = sys::features::page_size();
            addr % page == 0 && len % page == 0 && len != 0
        },
        "purge: discard of {addr:#x}+{len:#x} is not page-aligned"
    );
    #[cfg(test)]
    DISCARD_CALLS.fetch_add(1, Ordering::Relaxed);
    let advice = advice.madv();
    // SAFETY: the range is a page-aligned part of our own mappings that
    // the caller holds free (the `Batch` invariant).
    match unsafe { sys::madvise(addr, len, advice) } {
        Ok(()) => true,
        Err(sys::Errno::EINVAL) => false,
        Err(e) => panic!("purge: madvise({advice}) of {addr:#x}+{len:#x} failed: {e}"),
    }
}

/// `madvise` calls [`discard`] made (the tests' syscall counts).
#[cfg(test)]
static DISCARD_CALLS: AtomicU64 = AtomicU64::new(0);

/// The background thread (design 8.1).
pub mod background;
