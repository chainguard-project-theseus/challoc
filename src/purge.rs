//! Decay and purge (design.md section 8) and the background thread (8.1).
//!
//! Four things live here; the curve and the prefault pass each have a file
//! under `purge/`, the rest is this file:
//!
//! - [`Batch`], the discard primitive (this file, with the advice statics,
//!   `os::discard`, the decay driver and [`Decayer`]). Callers collect up
//!   to [`MAX_BATCH`] ranges and flush them with one `process_madvise`
//!   (through `PIDFD_SELF` or a pidfd of this process, whichever the probe
//!   in `sys::features` found) or, when neither works, a loop of `madvise`.
//!   The advice is
//!   decided at each flush ([`advice`], the two-advice purge of design 8
//!   and 11): `MADV_FREE` under the cgroup policy's headroom band (under
//!   70% of a known limit and no eager return of the excess over the cap
//!   in progress, `policy::Policy::headroom`, mirrored here by
//!   [`set_headroom`]), `MADV_DONTNEED` everywhere else, and always for
//!   an explicit purge ([`purge_all`], [`purge_arena`]). A lazy batch goes
//!   through `process_madvise` only where the init probe found the kernel
//!   takes `MADV_FREE` there, else through the loop. A range that fails to
//!   discard aborts: the heap's idea of what is resident would otherwise be
//!   wrong.
//! - The decay curve (`purge/curve.rs`) and driver (this file). [`Curve`]
//!   is jemalloc's decay shape:
//!   `dirty_decay_ms` is the window in which a page goes from dirty to
//!   purged, divided into [`STEPS`] steps; the share of a cohort that may
//!   still be dirty at step `s` of its age is [`WEIGHTS`]`[s]`, a
//!   smootherstep from one to zero, so about half of what went `Empty` at
//!   once is purged by mid-window and all of it by the end. Each
//!   `(arena, class)` and the huge allocator purge their oldest `Empty`
//!   slabs (extents) until what is left is within the curve's allowance,
//!   at most once per step (the huge allocator on arena 0's period, or
//!   its own under `huge_dirty_decay_ms`, design 7). [`decay`] visits every class of every arena
//!   and the huge allocator and reports the earliest next deadline (the
//!   end of the current step of whoever has something `Empty`). Each class
//!   keeps the `dirty_at` of its oldest `Empty` slab and its next deadline
//!   in atomics so the visit takes no lock unless a walk is due.
//!   [`on_tick`] is the hook the slow path calls every `PERIODIC_EVERY`
//!   slow paths (design 6.4); [`purge_all`] is `malloc_trim(0)` and
//!   `arena.<i>.purge`.
//! - The background thread ([`background`], this file). Created on the
//!   first periodic tick after it was requested (never from init), on a
//!   stack carved from the metadata region
//!   with a `PROT_NONE` guard page below it, with every signal blocked. It
//!   loops over `decay` and sleeps on a futex until the next deadline; a
//!   class whose first slab goes `Empty` bumps the futex word so a thread
//!   sleeping with no deadline wakes up. While it exists the slow path does
//!   no purging of its own. After `fork` the child marks it absent and does
//!   not recreate it until asked again.
//!
//! - The prefault pass ([`prefault`], `purge/prefault.rs`, behind the
//!   `prefault` key and the background thread): after each decay pass the
//!   thread visits every `(arena, class)` whose `refilled` counter moved
//!   since its last visit
//!   and, where the arena has fewer idle (`Empty` or `Purged`) slabs of
//!   that class than the class refilled since the last pass, in slabs (at
//!   least one, at most [`prefault::MAX_AHEAD`]), commits the missing
//!   slabs through the refill's own selection
//!   (`ClassState::prefault_ahead`), populates each with one
//!   `madvise(MADV_POPULATE_WRITE)` (5.14+, probed at init; else a touch
//!   per page) while it is still unpublished, and publishes it as
//!   `Purged`, which the refill takes first; nothing at all unless the
//!   cgroup policy shows headroom (`Policy::prefault_allowed`). The commit
//!   path wakes the thread once per fresh slab it puts into service, so
//!   the pass runs right after the refill that consumed the previous one.
//!
//! `pthread_create` allocates (glibc's `allocate_dtv` calls `calloc`), so
//! the creating thread parks its TLS slot on the `TLS_CREATING` sentinel for
//! the duration and that allocation is served straight from the slabs, the
//! same path `pthread_setspecific` takes during heap creation (design 6.5).
//!
//! Memory discarded with `MADV_FREE` keeps its contents until the kernel
//! reclaims it, so a `Purged` slab or extent that went out lazily is not
//! known to be zero. `class/` and `huge/` read one process-wide answer,
//! [`purged_is_zero`], under their locks when they hand a `Purged` member
//! out, and it is sticky: true until the first lazy batch of the process,
//! false from then on, so `calloc` clears `Purged` memory it reuses after
//! that (a `memset` of the slot or run, on pages that are mostly still
//! resident) and `Fresh` memory stays known zero. Tracking the advice per
//! slab would need a state bit in the descriptors and a read of it in the
//! claim paths; the sticky flag is the smallest form and its cost is
//! bounded to `calloc` on `Purged` reuse.
//!
//! The curve's tests sit in `purge/curve.rs`; `purge/tests.rs` holds the
//! rest of the host tests and the `serial` lock the class tests share
//! (`purge::tests::serial`), `purge/linux_tests.rs` the two that need a
//! kernel.

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

use crate::heap::Shared;
pub use crate::policy::Advice;
use crate::sys::PIDFD_SELF;
use crate::sys::features::ProcessMadvise;

mod curve;
#[cfg(test)]
pub(crate) mod tests;
#[cfg(all(test, target_os = "linux"))]
mod linux_tests;

pub use curve::{decay_ns_from_ms, Curve, NEVER, STEPS, WEIGHTS, WEIGHT_ONE};

/// Ranges per `process_madvise` (design section 8).
pub const MAX_BATCH: usize = 64;

/// `BACKEND` value for "loop over `madvise`".
const LOOP: i32 = i32::MIN;
/// `process_madvise` target: [`LOOP`], `PIDFD_SELF`, or a pidfd.
static BACKEND: AtomicI32 = AtomicI32::new(LOOP);
/// The backend takes `MADV_FREE` (the init probe, `sys::features`); a lazy
/// batch through a backend that does not goes through the loop.
static BACKEND_LAZY: AtomicBool = AtomicBool::new(false);
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
/// Batches and bytes issued with each advice (the report, `stats.cgroup.*`).
static LAZY_BATCHES: AtomicU64 = AtomicU64::new(0);
static EAGER_BATCHES: AtomicU64 = AtomicU64::new(0);
static LAZY_BYTES: AtomicU64 = AtomicU64::new(0);
static EAGER_BYTES: AtomicU64 = AtomicU64::new(0);
/// `check_zero`: the debug mode that reads every slot or page handed out
/// as "known zero" and aborts on a dirty word.
static CHECK_ZERO: AtomicBool = AtomicBool::new(false);

/// Choose the discard backend. Called once at init, before anything can
/// be purged, and by the tests.
pub fn configure(pm: ProcessMadvise) {
    let backend = match pm {
        ProcessMadvise::Unavailable => LOOP,
        ProcessMadvise::PidfdSelf => PIDFD_SELF,
        ProcessMadvise::Pidfd(fd) => fd,
    };
    BACKEND.store(backend, Ordering::Relaxed);
}

/// The two-advice purge's settings: whether the backend takes `MADV_FREE`
/// (the probe) and the `lazy_band` key. Called
/// once at init after [`configure`].
pub fn configure_advice(backend_lazy: bool, lazy_band: bool) {
    BACKEND_LAZY.store(backend_lazy, Ordering::Relaxed);
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

/// Whether a lazy batch goes through `process_madvise` (the probe found
/// the backend takes `MADV_FREE`); false with the loop backend too.
pub fn lazy_uses_process_madvise() -> bool {
    uses_process_madvise() && BACKEND_LAZY.load(Ordering::Relaxed)
}

/// `(lazy batches, eager batches)` flushed so far.
pub fn batches() -> (u64, u64) {
    (LAZY_BATCHES.load(Ordering::Relaxed), EAGER_BATCHES.load(Ordering::Relaxed))
}

/// `(lazy bytes, eager bytes)` discarded so far.
pub fn discarded_bytes() -> (u64, u64) {
    (LAZY_BYTES.load(Ordering::Relaxed), EAGER_BYTES.load(Ordering::Relaxed))
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
/// the tests, directly). Separate from [`configure`] so a re-probe of the
/// backend never resets it.
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

/// The discard backend and the advice in force for diagnostics, e.g.
/// `process_madvise(PIDFD_SELF) MADV_DONTNEED`; the advice reads
/// `MADV_FREE (some lazy batches issued)` once any lazy batch went out,
/// since that is what makes `Purged` memory unknown-zero.
#[cold]
pub fn backend_name() -> (&'static str, &'static str) {
    let backend = match BACKEND.load(Ordering::Relaxed) {
        LOOP => "madvise loop",
        PIDFD_SELF => "process_madvise(PIDFD_SELF)",
        _ => "process_madvise(pidfd)",
    };
    let advice = match (advice(), purged_is_zero()) {
        (Advice::Eager, false) => "MADV_DONTNEED (some lazy batches issued)",
        (a, _) => a.madv_name(),
    };
    (backend, advice)
}

/// Whether memory in the `Purged` state is known to read as zero: until
/// the first `MADV_FREE` batch of the process (the module doc), which
/// leaves the old contents in place until the kernel gets around to
/// reclaiming the pages; false from then on, for good.
#[inline]
pub fn purged_is_zero() -> bool {
    !LAZY_EVER.load(Ordering::Relaxed)
}

/// Whether batches go through `process_madvise` (false: `madvise` loop).
pub fn uses_process_madvise() -> bool {
    BACKEND.load(Ordering::Relaxed) != LOOP
}

/// Up to [`MAX_BATCH`] page ranges to discard together.
///
/// Invariant the callers uphold (the reason [`Batch::push`] and
/// [`Batch::flush`] are safe functions over an `madvise` that drops page
/// contents): every range pushed is a page-aligned part of one of this
/// process's own reservations whose pages are *free* (an `Empty` slab, a
/// freed huge run: reachable through no live pointer) and stay so until
/// the flush returns. The callers (`ClassState::purge_empty` and
/// `purge_curve`, `Huge::free_run`, `purge_empty` and `purge_curve`) push
/// under the class or huge lock that keeps the ranges free and flush
/// before releasing it; the batch never outlives that hold.
pub struct Batch {
    ranges: [(usize, usize); MAX_BATCH],
    n: usize,
}

impl Batch {
    pub const fn new() -> Batch {
        Batch { ranges: [(0, 0); MAX_BATCH], n: 0 }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.n
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.n == MAX_BATCH
    }

    /// Add `[addr, addr + len)` (free, per the type's invariant), flushing
    /// first if the batch is full.
    pub fn push(&mut self, addr: usize, len: usize) {
        // Adjacent ranges pushed in address order (a run of empty 4 KiB
        // slabs) go out as one.
        if self.n != 0 {
            let (last, last_len) = self.ranges[self.n - 1];
            if last + last_len == addr {
                self.ranges[self.n - 1].1 = last_len + len;
                return;
            }
        }
        if self.is_full() {
            self.flush();
        }
        self.ranges[self.n] = (addr, len);
        self.n += 1;
    }

    /// Discard every range with one syscall when the kernel allows it and a
    /// loop of `madvise` otherwise, then empty the batch. The advice is
    /// [`advice`] at this moment, counted per batch and per byte; a lazy
    /// flush makes `Purged` memory unknown-zero for the rest of the process
    /// (the module doc), which is published before the discard so a claim
    /// of one of these ranges after the caller's lock drops sees it. Aborts
    /// if a range cannot be discarded. The ranges are free (the type's
    /// invariant), which is what makes the discard sound.
    pub fn flush(&mut self) {
        if self.n != 0 {
            let advice = advice();
            let bytes: u64 = self.ranges[..self.n].iter().map(|&(_, len)| len as u64).sum();
            match advice {
                Advice::Lazy => {
                    LAZY_EVER.store(true, Ordering::Relaxed);
                    LAZY_BATCHES.fetch_add(1, Ordering::Relaxed);
                    LAZY_BYTES.fetch_add(bytes, Ordering::Relaxed);
                }
                Advice::Eager => {
                    EAGER_BATCHES.fetch_add(1, Ordering::Relaxed);
                    EAGER_BYTES.fetch_add(bytes, Ordering::Relaxed);
                }
            }
            os::discard(&self.ranges[..self.n], advice);
            self.n = 0;
        }
    }
}

/// The step gate of design section 8, one per `ClassState` and one in
/// the huge allocator: `dirty_at` of the oldest `Empty` member (0 when
/// none is known), written under the owner's lock and read without it by
/// the decay driver, and the deadline of the owner's next walk.
/// [`Decayer::decay`] runs the owner's walk at most once per step of the
/// curve; the walks themselves (`ClassState::purge_curve`,
/// `Huge::purge_curve`) differ and stay with their owners.
pub struct Decayer {
    /// `dirty_at` of the oldest `Empty` member, 0 when none. It may lag
    /// behind a claim that reused the oldest member, in which case it is
    /// too early rather than too late; the next walk recomputes it.
    oldest_empty: AtomicU64,
    /// When the next walk is due; 0 before the first.
    next_walk: AtomicU64,
}

impl Decayer {
    pub const fn new() -> Decayer {
        Decayer { oldest_empty: AtomicU64::new(0), next_walk: AtomicU64::new(0) }
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
    /// republishes the oldest `dirty_at`) at most once per step. Returns
    /// the owner's next deadline (the end of the oldest member's first
    /// step, or of the current step), `None` when nothing is `Empty`.
    /// Lock-free unless a walk is due.
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
        // A deadline further away than one step was set under a longer
        // period since shortened: walk now.
        let next = self.next_walk.load(Ordering::Relaxed);
        if now < next && next - now <= step {
            return Some(next);
        }
        walk(now, &curve);
        let next = now.saturating_add(step);
        self.next_walk.store(next, Ordering::Relaxed);
        if self.oldest_empty() == 0 { None } else { Some(next) }
    }
}

/// Run every decay walk that is due at `now` (coarse monotonic
/// nanoseconds) and return the earliest future deadline, `None` when
/// nothing is `Empty` or decay is off. Lock-free for a class with nothing
/// due.
pub fn decay(shared: &Shared, now: u64) -> Option<u64> {
    // Design 11: a `memory.events` `high` rise owes one full purge, run
    // before the walks so they see the survivors (none, normally).
    let policy = &shared.policy;
    if policy.take_full_purge() {
        purge_all(shared);
    }
    let keep16 = policy.keep16();
    let mut next: Option<u64> = None;
    let mut fold = |d: Option<u64>| {
        if let Some(d) = d {
            next = Some(next.map_or(d, |n| n.min(d)));
        }
    };
    for arena in shared.arenas.iter() {
        // Each arena's own period, scaled by the policy's band.
        fold(arena.decay_with(now, policy.effective_decay_ns(arena.decay_ns()), keep16));
    }
    // The huge allocator is shared by every arena: its own period when
    // `huge_dirty_decay_ms` is set, else arena 0's (design 7).
    let decay_ns = policy.effective_decay_ns(shared.huge_decay_ns());
    if decay_ns != NEVER {
        fold(shared.huge.decay_with(now, decay_ns, keep16));
    }
    next
}

/// Purge every `Empty` slab of every arena and every huge extent
/// regardless of age, eagerly (`malloc_trim(0)`, the `high`-rise full
/// purge: the memory is wanted back now). Returns how many were purged.
pub fn purge_all(shared: &Shared) -> usize {
    let _explicit = Explicit::enter();
    let mut n = 0;
    for arena in shared.arenas.iter() {
        n += arena.purge_all();
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
/// thread is doing it.
pub fn on_tick(shared: &Shared, now: u64) {
    sample(shared, now);
    if background::present() {
        return;
    }
    if background::requested() && background::start(shared) {
        return;
    }
    let _ = decay(shared, now);
}

/// Design 11: `memory.current` and `memory.events`, at most once per 10 ms
/// of the coarse clock, and the policy evaluated on the sample that was
/// taken; nothing at all when the cgroup is untracked or the window has
/// not passed. Slow paths only (the periodic tick, the background thread).
fn sample(shared: &Shared, now: u64) {
    if shared.cgroup.sample(now) {
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

#[cfg(target_os = "linux")]
mod os {
    use super::{Advice, BACKEND, BACKEND_LAZY, LOOP, MAX_BATCH};
    use crate::sys::{self, IoVec};
    use core::sync::atomic::Ordering;

    /// Issue `advice` on `ranges`, which are free (the [`Batch`]
    /// invariant; this is the one place the batch's ranges reach the
    /// kernel).
    ///
    /// [`Batch`]: super::Batch
    pub fn discard(ranges: &[(usize, usize)], advice: Advice) {
        let lazy = advice == Advice::Lazy;
        let advice = advice.madv();
        let backend = BACKEND.load(Ordering::Relaxed);
        // A lazy batch takes the batched path only where the probe found
        // the kernel accepts `MADV_FREE` through `process_madvise`.
        if backend != LOOP && (!lazy || BACKEND_LAZY.load(Ordering::Relaxed)) {
            let mut iov = [IoVec { base: core::ptr::null(), len: 0 }; MAX_BATCH];
            let mut total = 0usize;
            for (i, &(addr, len)) in ranges.iter().enumerate() {
                iov[i] = IoVec { base: addr as *const u8, len };
                total += len;
            }
            // SAFETY: the ranges are page-aligned parts of our own mappings
            // that the caller holds free (the `Batch` invariant); `iov`
            // outlives the call.
            let done = unsafe { sys::process_madvise(backend, iov.as_ptr(), ranges.len(), advice, 0) };
            if done == Ok(total) {
                return;
            }
            // A short or failed batch (a range the kernel refused, a pidfd
            // that went stale) falls back to the loop: both advices are
            // idempotent, so redoing the whole batch is harmless.
        }
        for &(addr, len) in ranges {
            // SAFETY: as above.
            if let Err(e) = unsafe { sys::madvise(addr, len, advice) } {
                panic!("purge: madvise({advice}) of {addr:#x}+{len:#x} failed: {e}");
            }
        }
    }
}

/// Off Linux nothing is mapped; host tests only exercise the bookkeeping.
#[cfg(not(target_os = "linux"))]
mod os {
    pub fn discard(_ranges: &[(usize, usize)], _advice: super::Advice) {}
}

/// The background thread (design 8.1); the host build gets the stub.
#[cfg(target_os = "linux")]
#[path = "purge/background.rs"]
pub mod background;
#[cfg(not(target_os = "linux"))]
#[path = "purge/background_host.rs"]
pub mod background;
/// The prefault pass (design 8.2), the background thread's second job.
pub mod prefault;

/// The fork child's view of the discard backend: a pidfd inherited from the
/// parent names the parent, so it is closed and purges fall back to the
/// `madvise` loop (`PIDFD_SELF` needs no fd and stays). Returns the
/// backend the child ended up with, for the feature record.
pub fn fork_child() -> ProcessMadvise {
    let b = BACKEND.load(Ordering::Relaxed);
    if b >= 0 {
        #[cfg(target_os = "linux")]
        let _ = crate::sys::close(b);
        BACKEND.store(LOOP, Ordering::Relaxed);
        return ProcessMadvise::Unavailable;
    }
    if b == LOOP { ProcessMadvise::Unavailable } else { ProcessMadvise::PidfdSelf }
}
