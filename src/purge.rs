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
//!   [`set_headroom`]) or under the `lazy_free` override, `MADV_DONTNEED`
//!   everywhere else, and always for an explicit purge ([`purge_all`],
//!   [`purge_arena`]). A lazy batch goes
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
//!   at most once per step. [`decay`] visits every class of every arena
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
//! known to be zero. `class.rs` and `huge.rs` read one process-wide answer,
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
use crate::sys::features::ProcessMadvise;

mod curve;
pub mod prefault;
#[cfg(test)]
pub(crate) mod tests;
#[cfg(all(test, target_os = "linux"))]
mod linux_tests;

pub use curve::{decay_ns_from_ms, Curve, NEVER, STEPS, WEIGHTS, WEIGHT_ONE};

/// Ranges per `process_madvise` (design section 8).
pub const MAX_BATCH: usize = 64;

/// `BACKEND` value for "loop over `madvise`".
const LOOP: i32 = i32::MIN;
/// The kernel's `PIDFD_SELF` sentinel (Linux 6.15). Mirrored here so the
/// host build of this module type-checks; the Linux build checks it.
const PIDFD_SELF: i32 = -10000;
#[cfg(target_os = "linux")]
const _: () = assert!(PIDFD_SELF == crate::sys::PIDFD_SELF);
/// `process_madvise` target: [`LOOP`], `PIDFD_SELF`, or a pidfd.
static BACKEND: AtomicI32 = AtomicI32::new(LOOP);
/// The backend takes `MADV_FREE` (the init probe, `sys::features`); a lazy
/// batch through a backend that does not goes through the loop.
static BACKEND_LAZY: AtomicBool = AtomicBool::new(false);
/// `lazy_free`: the override, discard with `MADV_FREE` everywhere.
static LAZY: AtomicBool = AtomicBool::new(false);
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

/// Choose the discard backend and the `lazy_free` override. Called once at
/// init, before anything can be purged, and by the tests. `lazy_free`
/// makes `Purged` memory unknown-zero from the start (the module doc).
pub fn configure(pm: ProcessMadvise, lazy_free: bool) {
    let backend = match pm {
        ProcessMadvise::Unavailable => LOOP,
        ProcessMadvise::PidfdSelf => PIDFD_SELF,
        ProcessMadvise::Pidfd(fd) => fd,
    };
    BACKEND.store(backend, Ordering::Relaxed);
    LAZY.store(lazy_free, Ordering::Relaxed);
    if lazy_free {
        LAZY_EVER.store(true, Ordering::Relaxed);
    }
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
    let band = if HEADROOM.load(Ordering::Relaxed) { crate::policy::Band::Low } else { crate::policy::Band::Unlimited };
    crate::policy::advice(
        band,
        LAZY.load(Ordering::Relaxed),
        LAZY_BAND.load(Ordering::Relaxed),
        EXPLICIT.load(Ordering::Relaxed) != 0,
    )
}

/// Why [`advice`] is what it is, for the report: `lazy_free`,
/// `under 70% of the limit`, `explicit purge`, `lazy_band:false`,
/// `returning the excess over the cap` (an eager return in progress,
/// `Policy::eager_excess`), or `no headroom known` (no limit, or 70% and
/// above).
#[cold]
pub fn advice_reason() -> &'static str {
    if LAZY.load(Ordering::Relaxed) {
        "lazy_free"
    } else if EXPLICIT.load(Ordering::Relaxed) != 0 {
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
        (Advice::Lazy, _) => "MADV_FREE",
        (Advice::Eager, true) => "MADV_DONTNEED",
        (Advice::Eager, false) => "MADV_DONTNEED (some lazy batches issued)",
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

/// Up to [`MAX_BATCH`] page ranges to discard together. Callers push while
/// holding the lock that keeps the ranges free and flush before releasing
/// it; the batch never outlives that hold.
pub struct Batch {
    ranges: [(usize, usize); MAX_BATCH],
    n: usize,
}

impl Default for Batch {
    fn default() -> Self {
        Batch::new()
    }
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

    /// Add `[addr, addr + len)`, flushing first if the batch is full.
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
    /// if a range cannot be discarded.
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
    // The huge allocator is shared by every arena and follows arena 0.
    let decay_ns = policy.effective_decay_ns(shared.decay_ns());
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
    use crate::sys::{self, IoVec, MADV_DONTNEED, MADV_FREE};
    use core::sync::atomic::Ordering;

    pub fn discard(ranges: &[(usize, usize)], advice: Advice) {
        let lazy = advice == Advice::Lazy;
        let advice = if lazy { MADV_FREE } else { MADV_DONTNEED };
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
            // that the caller holds free; `iov` outlives the call.
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

/// The background thread (design 8.1).
#[cfg(target_os = "linux")]
pub mod background {
    use core::ffi::c_void;
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use core::sync::atomic::AtomicI32;

    use crate::heap::{Shared, TLS_CREATING, TLS_EXITED};
    use crate::lock::Lock;
    use crate::sys::{self, Errno, PollFd, Timespec};

    /// Stack size including the guard page below it.
    pub const STACK_SIZE: usize = 256 * 1024;
    /// `prctl(PR_SET_NAME)`.
    const PR_SET_NAME: i32 = 15;
    /// Shortest futex timeout; the coarse clock ticks every few ms anyway.
    const MIN_SLEEP_NS: u64 = 1_000_000;

    /// Bookkeeping behind the state lock (the lock `fork_prepare` takes
    /// last, design section 9).
    struct State {
        present: bool,
        creating: bool,
        /// The one stack, reserved by the first `start` and reused by every
        /// later one; 0 until then.
        stack: usize,
        /// The last thread created on that stack, until it has been joined.
        /// `None` in a fork child, whose copy of the stack has no thread.
        thread: Option<::libc::pthread_t>,
    }

    static STATE: Lock<State> = Lock::new(State { present: false, creating: false, stack: 0, thread: None });
    /// Mirror of `STATE.present` for the lock-free check on every tick.
    static PRESENT: AtomicBool = AtomicBool::new(false);
    /// `background_thread:true` (or, later, `mallctl("background_thread")`).
    static REQUESTED: AtomicBool = AtomicBool::new(false);
    /// Futex word the thread sleeps on; bumped by [`wake`].
    static SEQ: AtomicU32 = AtomicU32::new(0);
    /// The PSI trigger descriptor (design 11) while the thread is armed,
    /// else -1. Owned by the thread: only it (and a fork child, which is
    /// single threaded) opens or closes it.
    static PSI_FD: AtomicI32 = AtomicI32::new(-1);
    /// The eventfd [`kick`] writes to while the thread sleeps in `ppoll`
    /// rather than on the futex. Created once, never closed in the process
    /// that created it (a kicker that loaded the number must not find it
    /// reused by another file); a fork child closes its copy.
    static WAKE_FD: AtomicI32 = AtomicI32::new(-1);
    /// `PSI_FD` is valid and the thread sleeps in `ppoll`: kicks must also
    /// write the eventfd.
    static PSI_ARMED: AtomicBool = AtomicBool::new(false);

    #[inline]
    pub fn present() -> bool {
        PRESENT.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn requested() -> bool {
        REQUESTED.load(Ordering::Relaxed)
    }

    /// Ask for (or, with `false`, stop asking for) a background thread. The
    /// next periodic tick creates it; a running thread that finds the
    /// request withdrawn exits at its next wake-up.
    pub fn request(on: bool) {
        REQUESTED.store(on, Ordering::Relaxed);
        if !on {
            // Unconditional: a thread the bookkeeping has already forgotten
            // (the parent side of a fork handler test) must still notice.
            kick();
        }
    }

    /// Bump the futex word and wake the thread if it is waiting. Cheap
    /// enough for the rare "first `Empty` slab of a class" transition.
    pub fn wake() {
        if present() {
            kick();
        }
    }

    fn kick() {
        SEQ.fetch_add(1, Ordering::SeqCst);
        // SAFETY: `SEQ` is a live, aligned `u32`.
        let _ = unsafe { sys::futex_wake(SEQ.as_ptr(), 1) };
        if PSI_ARMED.load(Ordering::Acquire) {
            let fd = WAKE_FD.load(Ordering::Relaxed);
            if fd >= 0 {
                // Level-triggered until the thread drains it, so a kick that
                // lands before the `ppoll` is not lost. Raw syscall: `errno`
                // stays as the caller left it.
                let _ = sys::write(fd, &1u64.to_ne_bytes());
            }
        }
    }

    /// Arm the PSI trigger (design 11) and make sure the wake eventfd
    /// exists; on any failure the cgroup record says why and the thread
    /// sleeps on the futex as before.
    fn arm(shared: &Shared) {
        let Some(psi) = shared.cgroup.arm_pressure() else { return };
        if WAKE_FD.load(Ordering::Relaxed) < 0 {
            match sys::eventfd(0, sys::EFD_CLOEXEC | sys::EFD_NONBLOCK) {
                Ok(fd) => WAKE_FD.store(fd, Ordering::Relaxed),
                Err(e) => {
                    let _ = sys::close(psi);
                    shared.cgroup.set_psi_unavailable(e.0);
                    return;
                }
            }
        }
        PSI_FD.store(psi, Ordering::Relaxed);
        PSI_ARMED.store(true, Ordering::Release);
    }

    /// Drop the trigger: on thread exit (`errno` 0, "no thread"), or when
    /// the poll reported the cgroup gone from under it.
    fn disarm(shared: &Shared, errno: i32) {
        PSI_ARMED.store(false, Ordering::Release);
        let fd = PSI_FD.swap(-1, Ordering::Relaxed);
        if fd >= 0 {
            let _ = sys::close(fd);
        }
        shared.cgroup.set_psi_unavailable(errno);
    }

    /// Sleep in `ppoll` on the trigger (`POLLPRI`) and the wake eventfd
    /// until `timeout`; account a fired trigger, drain a kick, and disarm
    /// if the trigger's file went bad.
    fn wait_poll(shared: &Shared, psi: i32, timeout: Option<&Timespec>) {
        let wake = WAKE_FD.load(Ordering::Relaxed);
        let mut fds = [
            PollFd { fd: psi, events: sys::POLLPRI, revents: 0 },
            PollFd { fd: wake, events: sys::POLLIN, revents: 0 },
        ];
        if let Ok(n) = sys::ppoll(&mut fds, timeout) {
            if n == 0 {
                return;
            }
            if fds[1].revents & sys::POLLIN != 0 {
                let mut b = [0u8; 8];
                let _ = sys::read(wake, &mut b);
            }
            if fds[0].revents & sys::POLLPRI != 0 {
                // A stall: sample outside the window and evaluate the
                // policy on it; the loop's decay pass follows at once.
                shared.cgroup.note_pressure_event(sys::now_coarse_ns());
                crate::policy::evaluate(shared);
            }
            if fds[0].revents & (sys::POLLERR | sys::POLLHUP | sys::POLLNVAL) != 0 {
                disarm(shared, Errno::ENOENT.0);
            }
        }
    }

    /// Create the thread if it does not exist. `true` if one exists on
    /// return; `false` if creation failed or is in progress on another
    /// thread (the caller then purges in the foreground for this tick).
    ///
    /// `errno` is preserved: this runs from `free`'s slow path (design
    /// section 1 promises `free` leaves `errno` alone) and glibc's
    /// `pthread_create` / `pthread_join` may set it on their way.
    pub fn start(shared: &Shared) -> bool {
        let (stack, previous) = {
            let mut g = STATE.lock();
            if g.present {
                return true;
            }
            if g.creating {
                return false;
            }
            g.creating = true;
            (g.stack, g.thread.take())
        };
        // SAFETY: `__errno_location` returns this thread's `errno`.
        let errno = unsafe { ::libc::__errno_location() };
        let saved_errno = unsafe { *errno };
        let (stack, thread) = create(shared, stack, previous);
        // SAFETY: `errno` is this thread's `errno` slot (above), valid for
        // the life of the thread.
        unsafe { *errno = saved_errno };
        let mut g = STATE.lock();
        g.creating = false;
        g.stack = stack;
        g.thread = thread;
        g.present = thread.is_some();
        PRESENT.store(g.present, Ordering::Relaxed);
        g.present
    }

    /// Create the thread on `stack` (reserving the stack first if there is
    /// none yet), after joining `previous`. Returns the stack (kept even
    /// when creation fails, for the next attempt) and the new thread.
    fn create(shared: &Shared, stack: usize, previous: Option<::libc::pthread_t>) -> (usize, Option<::libc::pthread_t>) {
        let page = shared.regions.page_size();
        // The TLS slot is parked on `TLS_CREATING` for the pthread calls:
        // `pthread_create` allocates the new thread's DTV with `calloc`
        // and `pthread_join` frees an exited thread's key data, neither of
        // which may re-enter this thread's heap mid-operation.
        let saved = crate::tls::get();
        let park = saved as usize > TLS_EXITED;
        if park {
            crate::tls::set(TLS_CREATING as *mut c_void);
        }
        // The previous thread exited at a wake-up (`request(false)`) but
        // glibc's exit path still runs on the stack until the kernel has
        // reaped it; the join returns only then, so nothing runs on the
        // stack when it is handed to the next thread. A fork child never
        // has a `previous`: its copy of the stack carries no thread.
        if let Some(t) = previous {
            // SAFETY: a joinable thread this module created and never joined.
            let rc = unsafe { ::libc::pthread_join(t, core::ptr::null_mut()) };
            if rc != 0 {
                sys::abort_fmt(format_args!("pthread_join of the background thread failed: {}", sys::Errno(rc)));
            }
        }
        // Reserved once, then committed above the lowest page, which stays
        // `PROT_NONE` as the guard. Never freed: the thread never exits
        // while its stack could be in use, and a restart reuses the same
        // reservation after the join above.
        let stack = if stack != 0 {
            Some(stack)
        } else {
            shared.meta.reserve(STACK_SIZE, page).filter(|&s| shared.meta.commit(s + page, STACK_SIZE - page).is_ok())
        };
        let thread = match stack {
            Some(stack) => spawn(shared, stack + page, STACK_SIZE - page),
            None => None,
        };
        if park {
            crate::tls::set(saved);
        }
        (stack.unwrap_or(0), thread)
    }

    /// `pthread_create` on the committed `[base, base + len)`, joinable so
    /// a restart can wait for its exit. Signals are blocked so the new
    /// thread inherits an empty mask and never runs an application handler.
    fn spawn(shared: &Shared, base: usize, len: usize) -> Option<::libc::pthread_t> {
        // SAFETY: plain pthread attribute calls on a local; the stack is
        // ours and committed.
        unsafe {
            let mut attr: ::libc::pthread_attr_t = core::mem::zeroed();
            if ::libc::pthread_attr_init(&mut attr) != 0 {
                return None;
            }
            let mut thread = None;
            if ::libc::pthread_attr_setstack(&mut attr, base as *mut c_void, len) == 0 {
                let mask = sys::block_all_signals();
                let mut tid: ::libc::pthread_t = 0;
                let rc = ::libc::pthread_create(&mut tid, &attr, main, shared as *const Shared as *mut c_void);
                sys::set_signal_mask(mask);
                if rc == 0 {
                    thread = Some(tid);
                }
            }
            ::libc::pthread_attr_destroy(&mut attr);
            thread
        }
    }

    /// Join the recorded thread if it has exited (`request(false)` and
    /// its wake-up have happened) and forget it, so the stack is known to
    /// be idle without waiting for the next `start`. `false` if there is
    /// no thread to join or it is still present. Used by the tests that
    /// drive `fork_child` in-process: dropping the record of a thread that
    /// exists in *this* process would leave it unjoined for good.
    pub fn join_exited() -> bool {
        let t = {
            let mut g = STATE.lock();
            if g.present || g.creating {
                return false;
            }
            g.thread.take()
        };
        match t {
            Some(t) => {
                // SAFETY: a joinable thread this module created and never joined.
                assert_eq!(unsafe { ::libc::pthread_join(t, core::ptr::null_mut()) }, 0);
                true
            }
            None => false,
        }
    }

    /// The stack reservation (0 before the first start), for tests.
    pub fn stack() -> usize {
        STATE.lock().stack
    }

    /// The last thread created and not yet joined, for tests.
    pub fn thread() -> Option<::libc::pthread_t> {
        STATE.lock().thread
    }

    extern "C" fn main(arg: *mut c_void) -> *mut c_void {
        // SAFETY: `start` passed the process-wide `Shared`, which lives for
        // the rest of the process; the name is NUL-terminated.
        let shared = unsafe { &*(arg as *const Shared) };
        let _ = unsafe { sys::prctl(PR_SET_NAME, b"challoc-bg\0".as_ptr() as usize, 0, 0, 0) };
        // Design 11: the trigger is armed only by this thread, once it
        // exists, and only if the kernel takes the write.
        arm(shared);
        loop {
            // Read the sequence before scanning, so a bump that lands
            // between the scan and the wait makes the wait return at once.
            let seq = SEQ.load(Ordering::SeqCst);
            if !requested() {
                disarm(shared, 0);
                let mut g = STATE.lock();
                g.present = false;
                PRESENT.store(false, Ordering::Relaxed);
                return core::ptr::null_mut();
            }
            let now = sys::now_coarse_ns();
            super::sample(shared, now);
            let mut deadline = super::decay(shared, now);
            // After the walks, so a class whose
            // `Empty` slabs were just purged is not prefaulted on top of
            // them (they veto the pass while they exist). While a pass
            // finds classes refilling, the thread polls (`POLL_NS`) so the
            // next slab is ready before the refill that needs it; a quiet
            // pass leaves the sleep to the decay deadline and the wakes.
            if super::prefault::pass(shared) {
                let poll = now.saturating_add(super::prefault::POLL_NS);
                deadline = Some(deadline.map_or(poll, |d| d.min(poll)));
            }
            let timeout = deadline.map(|deadline| {
                let ns = deadline.saturating_sub(now).max(MIN_SLEEP_NS);
                Timespec { sec: (ns / 1_000_000_000) as i64, nsec: (ns % 1_000_000_000) as i64 }
            });
            let psi = PSI_FD.load(Ordering::Relaxed);
            if psi >= 0 {
                // A kick that landed since the `SEQ` load above left the
                // eventfd readable, so this returns at once in that case.
                wait_poll(shared, psi, timeout.as_ref());
            } else {
                // SAFETY: `SEQ` is a live, aligned `u32`; `timeout` outlives
                // the call. Every outcome (wake, EAGAIN, ETIMEDOUT, EINTR)
                // just means "look again".
                let _ = unsafe { sys::futex_wait(SEQ.as_ptr(), seq, timeout.as_ref()) };
            }
        }
    }

    /// Take the state lock (fork prepare, after the metadata lock).
    pub fn fork_lock() {
        STATE.raw().lock();
    }

    /// Release the state lock (fork parent and child, before the others).
    pub fn fork_unlock() {
        STATE.raw().unlock();
    }

    /// Whether the state lock is held (for the fork handler tests).
    pub fn is_locked() -> bool {
        STATE.raw().is_locked()
    }

    /// The child has no background thread and, per design 8.1, does not
    /// get one until it asks again. Called with the state lock held by
    /// this thread (`fork_prepare` took it).
    pub fn fork_child() {
        // SAFETY: the forking thread holds the lock; the child is single
        // threaded.
        let st = unsafe { STATE.get_unlocked() };
        st.present = false;
        st.creating = false;
        // The parent's thread does not exist here and its `pthread_t`
        // must never be joined (the wait would be on a thread id the
        // kernel never clears in this process); the stack is kept and is
        // free to reuse, nothing runs on the child's copy of it.
        st.thread = None;
        PRESENT.store(false, Ordering::Relaxed);
        REQUESTED.store(false, Ordering::Relaxed);
        // The child's copies of the parent thread's descriptors: closing
        // them here is safe (one thread, nothing can be mid-`kick`), and
        // necessary, since a kick through the shared eventfd would wake the
        // parent's thread.
        PSI_ARMED.store(false, Ordering::Relaxed);
        for fd in [&PSI_FD, &WAKE_FD] {
            let fd = fd.swap(-1, Ordering::Relaxed);
            if fd >= 0 {
                let _ = sys::close(fd);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub mod background {
    use crate::heap::Shared;

    pub fn present() -> bool {
        false
    }

    pub fn requested() -> bool {
        false
    }

    pub fn request(_on: bool) {}

    pub fn wake() {}

    pub fn start(_shared: &Shared) -> bool {
        false
    }
}

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
