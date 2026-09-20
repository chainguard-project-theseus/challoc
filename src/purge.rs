//! Decay and purge (design.md section 8) and the background thread (8.1).
//!
//! Three things live here:
//!
//! - [`Batch`], the discard primitive. Callers collect up to [`MAX_BATCH`]
//!   ranges and flush them with one `process_madvise` (through `PIDFD_SELF`
//!   or a pidfd of this process, whichever the probe in `sys::features`
//!   found) or, when neither works, a loop of `madvise`. The advice is
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
//! - The decay curve and driver. [`Curve`] is jemalloc's decay shape:
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
//! - The background thread. Created on the first periodic tick after it was
//!   requested (never from init), on a stack carved from the metadata region
//!   with a `PROT_NONE` guard page below it, with every signal blocked. It
//!   loops over `decay` and sleeps on a futex until the next deadline; a
//!   class whose first slab goes `Empty` bumps the futex word so a thread
//!   sleeping with no deadline wakes up. While it exists the slow path does
//!   no purging of its own. After `fork` the child marks it absent and does
//!   not recreate it until asked again.
//!
//! - The prefault pass ([`prefault`], plan Phase 2.5 task 11, behind the
//!   `prefault` key and the background thread): after each decay pass the
//!   thread visits every `(arena, class)` whose `refilled` counter moved
//!   since its last visit and, where the arena has no `Empty` or `Purged`
//!   slab of that class (so the next refill would commit a fresh slab),
//!   commits that slab through the refill's own selection
//!   (`ClassState::prefault_next`), populates it with one
//!   `madvise(MADV_POPULATE_WRITE)` (5.14+, probed at init; else a touch
//!   per page) while it is still unpublished, and publishes it as
//!   `Purged`, which the refill takes first; one slab per class per pass,
//!   nothing at all unless the cgroup policy shows headroom
//!   (`Policy::prefault_allowed`). The commit path wakes the thread once
//!   per fresh slab it puts into service, so the pass runs right after the
//!   refill that consumed the previous one.
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
//! bounded to `calloc` on `Purged` reuse (plan Phase 2.5 task 10).

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

use crate::heap::Shared;
pub use crate::policy::Advice;
use crate::sys::features::ProcessMadvise;

/// Ranges per `process_madvise` (design section 8).
pub const MAX_BATCH: usize = 64;

/// `decay_ns` value meaning "never purge" (`dirty_decay_ms:-1`).
pub const NEVER: u64 = u64::MAX;

/// `dirty_decay_ms` to a nanosecond period; negative means [`NEVER`].
pub const fn decay_ns_from_ms(ms: i64) -> u64 {
    if ms < 0 {
        NEVER
    } else {
        let ns = (ms as u64).saturating_mul(1_000_000);
        if ns == NEVER { NEVER - 1 } else { ns }
    }
}

/// Steps of the decay curve over one window (jemalloc's
/// `SMOOTHSTEP_NSTEPS`).
pub const STEPS: usize = 200;
/// Fixed-point one of the curve weights (jemalloc's `SMOOTHSTEP_BFP`).
pub const WEIGHT_ONE: u64 = 1 << 24;

/// `WEIGHTS[s]`: the share of a cohort of dirty memory that may stay dirty
/// while its age is in step `s` of the window, as
/// `WEIGHT_ONE * (1 - smootherstep((s + 1) / STEPS))` (jemalloc's
/// `h_steps`, variant "smoother": `6x^5 - 15x^4 + 10x^3`). Non-increasing,
/// from just under one to zero at `STEPS - 1`, positive before that.
pub static WEIGHTS: [u32; STEPS] = build_weights();

const fn build_weights() -> [u32; STEPS] {
    let mut w = [0u32; STEPS];
    let n = STEPS as i128;
    let n5 = n * n * n * n * n;
    let mut s = 0;
    while s < STEPS {
        let t = (s + 1) as i128;
        // smootherstep(t / n), scaled by n^5.
        let num = 6 * t * t * t * t * t - 15 * t * t * t * t * n + 10 * t * t * t * n * n;
        w[s] = (((n5 - num) * WEIGHT_ONE as i128 + n5 / 2) / n5) as u32;
        s += 1;
    }
    w
}

/// The decay curve of one period (design section 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Curve {
    decay_ns: u64,
}

impl Curve {
    pub const fn new(decay_ns: u64) -> Curve {
        Curve { decay_ns }
    }

    /// Length of one step; 0 for a zero period, where everything is past
    /// the window at once.
    #[inline]
    pub const fn step_ns(&self) -> u64 {
        self.decay_ns / STEPS as u64
    }

    /// The step something dirty since `dirty_at` is in at `now`:
    /// `STEPS - 1` (weight zero) for anything in or past the last step.
    pub fn step(&self, now: u64, dirty_at: u64) -> usize {
        let age = now.saturating_sub(dirty_at);
        if age >= self.decay_ns {
            return STEPS - 1;
        }
        // `u64` only: the 128-bit division helper on aarch64 drags
        // `rust_eh_personality` into the `no_std` library. An age too large
        // to scale is under a period whose step is at least `u64::MAX /
        // STEPS`, so dividing by the step loses nothing that matters.
        let s = if age > u64::MAX / STEPS as u64 { age / self.step_ns() } else { age * STEPS as u64 / self.decay_ns };
        (s as usize).min(STEPS - 1)
    }

    /// Weight of `step`, in units of [`WEIGHT_ONE`].
    #[inline]
    pub fn weight(step: usize) -> u64 {
        WEIGHTS[step] as u64
    }

    /// Units (slabs, pages) that may stay dirty given `weighted`, the sum
    /// of `units * weight(step)` over everything dirty: the curve's value
    /// rounded up, so a lone slab is not purged before its cohort's weight
    /// reaches zero (jemalloc rounds its page count down; here the unit is
    /// a slab of 16 pages, and a purge-and-refault of it for one unit of
    /// rounding is not worth it).
    #[inline]
    pub fn allowance(weighted: u64) -> usize {
        weighted.div_ceil(WEIGHT_ONE) as usize
    }
}

/// `BACKEND` value for "loop over `madvise`".
const LOOP: i32 = i32::MIN;
/// The kernel's `PIDFD_SELF` sentinel (Linux 6.14). Mirrored here so the
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

/// The two-advice purge's settings (plan Phase 2.5 task 10): whether the
/// backend takes `MADV_FREE` (the probe) and the `lazy_band` key. Called
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

/// Some lazy batch has been issued (the sticky flag behind
/// [`purged_is_zero`]), for the report.
pub fn lazy_ever() -> bool {
    LAZY_EVER.load(Ordering::Relaxed)
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

impl Default for Decayer {
    fn default() -> Decayer {
        Decayer::new()
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
pub fn sample(shared: &Shared, now: u64) {
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
        /// later one (review 2026-09-18 item 14); 0 until then.
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

    /// Whether the thread holds a PSI trigger (for tests and the report).
    pub fn psi_armed() -> bool {
        PSI_ARMED.load(Ordering::Relaxed)
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
    /// `pthread_create` / `pthread_join` may set it on their way (review
    /// 2026-09-18 item 13).
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
            // Plan Phase 2.5 task 11: after the walks, so a class whose
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

/// Prefaulted fresh slabs (plan Phase 2.5 task 11; the module doc). The
/// pass runs on the background thread; the commit path calls
/// [`note_commit`]; init calls [`configure`].
pub mod prefault {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use crate::heap::Shared;
    use crate::layout::NCLASSES;

    /// `prefault:true` with the background thread (`Config::prefault_active`).
    static ENABLED: AtomicBool = AtomicBool::new(false);
    /// The kernel takes `MADV_POPULATE_WRITE` (else the touch fallback).
    static POPULATE: AtomicBool = AtomicBool::new(false);
    /// A pass is running: its own commits wake nobody.
    static IN_PASS: AtomicBool = AtomicBool::new(false);
    /// Passes run, slabs prefaulted, bytes populated, passes skipped for
    /// want of headroom, `MADV_POPULATE_WRITE` calls that failed and fell
    /// back to the touch.
    static PASSES: AtomicU64 = AtomicU64::new(0);
    static SLABS: AtomicU64 = AtomicU64::new(0);
    static BYTES: AtomicU64 = AtomicU64::new(0);
    static SKIPPED: AtomicU64 = AtomicU64::new(0);
    static FALLBACKS: AtomicU64 = AtomicU64::new(0);
    /// Wakes the commit path issued.
    static WAKES: AtomicU64 = AtomicU64::new(0);
    /// Wakes the claim path issued (`ClassState::plan`: a refill took the
    /// arena's last idle slab of a class).
    static TAKEN: AtomicU64 = AtomicU64::new(0);
    /// Passes that found at least one class refilling (and so re-armed
    /// the poll).
    static ACTIVE: AtomicU64 = AtomicU64::new(0);

    /// The background thread's sleep while classes keep refilling: at
    /// 100 k Valkey SETs of a few hundred bytes per second a hot class
    /// takes a 64 KiB slab every millisecond or so, and a pass is a few
    /// microseconds, so a millisecond keeps the next slab ready without
    /// the thread spinning; a quiet pass stops the poll.
    pub const POLL_NS: u64 = 1_000_000;

    /// The most slabs a pass keeps ready per `(arena, class)`: the
    /// class's refills since the last pass in slabs, at least one and at
    /// most this many (`ClassState::prefault_ahead`; the plan's "one slab
    /// per class" is the floor, the burst rate raises it). Eight 64 KiB
    /// slabs is half a megabyte resident ahead of a class taking several
    /// slabs per millisecond, and nothing more for a class that does not.
    pub const MAX_AHEAD: u64 = 8;

    /// Init: the key's outcome and the probe's.
    pub fn configure(active: bool, populate_write: bool) {
        POPULATE.store(populate_write, Ordering::Relaxed);
        ENABLED.store(active, Ordering::Release);
    }

    /// Whether the pass runs at all.
    #[inline]
    pub fn enabled() -> bool {
        ENABLED.load(Ordering::Relaxed)
    }

    /// `MADV_POPULATE_WRITE` is the populate in use (else the touch).
    pub fn uses_populate_write() -> bool {
        POPULATE.load(Ordering::Relaxed)
    }

    /// The counters: passes, slabs, bytes, skipped passes, fallbacks, wakes.
    pub fn stats() -> Stats {
        Stats {
            passes: PASSES.load(Ordering::Relaxed),
            slabs: SLABS.load(Ordering::Relaxed),
            bytes: BYTES.load(Ordering::Relaxed),
            skipped: SKIPPED.load(Ordering::Relaxed),
            fallbacks: FALLBACKS.load(Ordering::Relaxed),
            wakes: WAKES.load(Ordering::Relaxed),
            active: ACTIVE.load(Ordering::Relaxed),
            taken: TAKEN.load(Ordering::Relaxed),
        }
    }

    /// The counters, for the report and `stats.prefault.*`.
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
    pub struct Stats {
        pub passes: u64,
        pub slabs: u64,
        pub bytes: u64,
        pub skipped: u64,
        pub fallbacks: u64,
        pub wakes: u64,
        pub active: u64,
        pub taken: u64,
    }

    /// A fresh slab went into service (`SlabTable::commit_slab`): wake the
    /// thread for a pass, unless the pass itself is committing. Off: one
    /// relaxed load.
    #[inline]
    pub fn note_commit() {
        if enabled() && !IN_PASS.load(Ordering::Relaxed) {
            WAKES.fetch_add(1, Ordering::Relaxed);
            super::background::wake();
        }
    }

    /// A refill took the arena's last idle slab of a class
    /// (`ClassState::plan`, the claim side; plan Phase 2.5 task 11):
    /// wake the thread so the class's next slab is committed now rather
    /// than at the next poll or the next fresh commit. The caller tests
    /// [`enabled`] first.
    #[inline]
    pub fn note_taken() {
        if enabled() {
            TAKEN.fetch_add(1, Ordering::Relaxed);
            super::background::wake();
        }
    }

    /// One pass over every `(arena, class)`: the classes that refilled
    /// since the last pass get their next fresh slab committed and
    /// populated (`ClassState::prefault_next`), one each, when the cgroup
    /// policy shows headroom. Returns whether any class had refilled (the
    /// thread then polls, `POLL_NS`). Background thread only.
    pub fn pass(shared: &Shared) -> bool {
        if !enabled() {
            return false;
        }
        if !shared.policy.prefault_allowed() {
            SKIPPED.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        PASSES.fetch_add(1, Ordering::Relaxed);
        IN_PASS.store(true, Ordering::Relaxed);
        let page = shared.regions.page_size();
        let mut due = false;
        for arena in shared.arenas.iter() {
            for class in 0..NCLASSES {
                let cs = arena.class(class);
                let delta = cs.prefault_delta();
                if delta == 0 {
                    continue;
                }
                due = true;
                // The class's refills since the last pass, in slabs: the
                // pipeline to keep ready (`MAX_AHEAD`).
                let ahead = delta.div_ceil(cs.class().nslots() as u64).clamp(1, MAX_AHEAD) as usize;
                let n = cs.prefault_ahead(&shared.meta, ahead, |addr, len| populate(addr, len, page));
                SLABS.fetch_add(n as u64, Ordering::Relaxed);
            }
        }
        IN_PASS.store(false, Ordering::Relaxed);
        if due {
            ACTIVE.fetch_add(1, Ordering::Relaxed);
        }
        due
    }

    /// Fault `[addr, addr + len)` in writable: `MADV_POPULATE_WRITE` where
    /// the probe found it, else one write of zero per page (the slab is
    /// fresh, unpublished and zero, so the write changes nothing and is
    /// this thread's alone). Counted.
    pub fn populate(addr: usize, len: usize, page: usize) {
        BYTES.fetch_add(len as u64, Ordering::Relaxed);
        if POPULATE.load(Ordering::Relaxed) && os::populate_write(addr, len) {
            return;
        }
        if POPULATE.load(Ordering::Relaxed) {
            FALLBACKS.fetch_add(1, Ordering::Relaxed);
        }
        let mut off = 0;
        while off < len {
            // SAFETY: a committed, read-write slab this thread holds the
            // class lock for and nothing else references yet; a zero over
            // a zero.
            unsafe { ((addr + off) as *mut u8).write_volatile(0) };
            off += page;
        }
    }

    #[cfg(target_os = "linux")]
    mod os {
        pub fn populate_write(addr: usize, len: usize) -> bool {
            // SAFETY: a committed range of a class region this process
            // owns; populating changes residency, never contents.
            unsafe { crate::sys::madvise(addr, len, crate::sys::MADV_POPULATE_WRITE) }.is_ok()
        }
    }

    #[cfg(not(target_os = "linux"))]
    mod os {
        pub fn populate_write(_addr: usize, _len: usize) -> bool {
            false
        }
    }

    #[cfg(test)]
    pub(crate) fn reset_for_test(active: bool, populate_write: bool) {
        configure(active, populate_write);
        for c in [&PASSES, &SLABS, &BYTES, &SKIPPED, &FALLBACKS, &WAKES, &ACTIVE, &TAKEN] {
            c.store(0, Ordering::Relaxed);
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The curve is jemalloc's: just under one at the start, zero only at
    /// the last step, half at mid-window, non-increasing, and symmetric
    /// about the middle.
    #[test]
    fn weights_are_a_smootherstep_from_one_to_zero() {
        // 1 - smootherstep(1 / 200) = 1 - 1.24e-6 (jemalloc: 0x14 of 2^24).
        assert_eq!(WEIGHTS[0] as u64, WEIGHT_ONE - 21);
        assert_eq!(WEIGHTS[STEPS - 2], 21);
        assert_eq!(WEIGHTS[STEPS - 1], 0);
        // 1 - smootherstep(100 / 200) = 0.5 exactly.
        assert_eq!(WEIGHTS[STEPS / 2 - 1] as u64, WEIGHT_ONE / 2);
        for s in 1..STEPS {
            assert!(WEIGHTS[s] <= WEIGHTS[s - 1], "step {s}");
        }
        // smootherstep(1 - x) = 1 - smootherstep(x).
        for s in 0..STEPS - 1 {
            let mirror = STEPS - 2 - s;
            let sum = WEIGHTS[s] as u64 + WEIGHTS[mirror] as u64;
            assert!(sum.abs_diff(WEIGHT_ONE) <= 1, "steps {s} and {mirror}: {sum}");
        }
        // A quarter of the way in, 1 - smootherstep(0.25) = 0.896...
        let q = WEIGHTS[STEPS / 4 - 1] as f64 / WEIGHT_ONE as f64;
        assert!((q - 0.896484375).abs() < 0.001, "{q}");
    }

    #[test]
    fn steps_and_allowance() {
        let c = Curve::new(200_000);
        assert_eq!(c.step_ns(), 1000);
        assert_eq!(c.step(5000, 5000), 0);
        assert_eq!(c.step(5999, 5000), 0);
        assert_eq!(c.step(6000, 5000), 1);
        assert_eq!(c.step(5000 + 100_000, 5000), 100);
        assert_eq!(c.step(5000 + 199_000, 5000), STEPS - 1);
        assert_eq!(c.step(u64::MAX, 5000), STEPS - 1);
        // `now` before `dirty_at` (a stale clock read) is age zero.
        assert_eq!(c.step(4000, 5000), 0);
        // A zero period puts everything in the last step.
        assert_eq!(Curve::new(0).step(5000, 5000), STEPS - 1);
        assert_eq!(Curve::new(0).step_ns(), 0);
        // Ages beyond the scaling range, under a period longer still.
        let huge = Curve::new(u64::MAX - 1);
        assert_eq!(huge.step(u64::MAX / 2, 0), STEPS / 2);
        assert_eq!(huge.step(u64::MAX - 1, 0), STEPS - 1);
        assert_eq!(huge.step(1000, 0), 0);
        // A period shorter than the step count still ends.
        assert_eq!(Curve::new(7).step(5007, 5000), STEPS - 1);
        assert_eq!(Curve::new(7).step(5000, 5000), 0);
        // The allowance rounds up.
        assert_eq!(Curve::allowance(0), 0);
        assert_eq!(Curve::allowance(1), 1);
        assert_eq!(Curve::allowance(WEIGHT_ONE), 1);
        assert_eq!(Curve::allowance(WEIGHT_ONE + 1), 2);
        assert_eq!(Curve::allowance(100 * Curve::weight(STEPS / 2 - 1)), 50);
        assert_eq!(Curve::allowance(100 * Curve::weight(STEPS - 1)), 0);
        // Half of a cohort of 100 at mid-window, all of it at the end, and
        // none of it in the first step.
        assert_eq!(Curve::allowance(100 * Curve::weight(0)), 100);
    }

    #[test]
    fn decay_period_from_ms() {
        assert_eq!(decay_ns_from_ms(-1), NEVER);
        assert_eq!(decay_ns_from_ms(-1000), NEVER);
        assert_eq!(decay_ns_from_ms(0), 0);
        assert_eq!(decay_ns_from_ms(10_000), 10_000_000_000);
        assert_eq!(decay_ns_from_ms(crate::config::MAX_INTERVAL_MS), 1_000_000_000_000_000_000);
        assert_ne!(decay_ns_from_ms(i64::MAX), NEVER);
    }

    /// Pages the bookkeeping test may point a batch at: a real mapping on
    /// Linux (the flush really discards), made-up addresses elsewhere.
    #[cfg(target_os = "linux")]
    fn scratch(pages: usize, page: usize) -> usize {
        use crate::sys::{self, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};
        unsafe { sys::mmap(0, pages * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) }.unwrap()
    }

    #[cfg(not(target_os = "linux"))]
    fn scratch(_pages: usize, _page: usize) -> usize {
        0x1000_0000
    }

    /// The page the batch tests work in: the kernel's on Linux (the flush
    /// really discards, so the ranges must be page-aligned there), 4 KiB
    /// elsewhere.
    #[cfg(target_os = "linux")]
    fn page_size() -> usize {
        crate::sys::features::page_size()
    }

    #[cfg(not(target_os = "linux"))]
    fn page_size() -> usize {
        4096
    }

    #[test]
    fn batch_merges_adjacent_ranges() {
        let _serial = serial();
        let page = page_size();
        let base = scratch(8, page);
        let mut b = Batch::new();
        b.push(base, page);
        b.push(base + page, page);
        b.push(base + 2 * page, 2 * page);
        assert_eq!(b.len(), 1);
        assert_eq!(b.ranges[0], (base, 4 * page));
        // A gap starts a new range; so does a range that is not the next one.
        b.push(base + 5 * page, page);
        assert_eq!(b.len(), 2);
        b.push(base + 4 * page, page);
        assert_eq!(b.len(), 3);
        b.flush();
        assert!(b.is_empty());
    }

    #[test]
    fn batch_fills_and_flushes() {
        let _serial = serial();
        let (_, eager0) = batches();
        let page = page_size();
        let base = scratch(2 * MAX_BATCH + 4, page);
        let mut b = Batch::new();
        assert!(b.is_empty());
        // Every other page, so nothing merges.
        for i in 0..MAX_BATCH {
            assert!(!b.is_full());
            b.push(base + 2 * page * i, page);
        }
        assert!(b.is_full());
        assert_eq!(b.len(), MAX_BATCH);
        // The 65th push flushes the first 64 first.
        b.push(base + 2 * page * MAX_BATCH, 2 * page);
        assert_eq!(b.len(), 1);
        b.flush();
        assert!(b.is_empty());
        b.flush();
        assert!(b.is_empty());
        // At least the two eager batches of this test were counted (the
        // default advice; other tests flush their own batches concurrently
        // in this process), none lazy.
        assert!(batches().1 >= eager0 + 2);
        assert!(purged_is_zero());
    }

    /// The default (no `lazy_free`, no limit) discards with `MADV_DONTNEED`,
    /// so purged memory is zero. `lazy_free:true` is exercised end to end by
    /// `tests/c/decay.c` and the headroom band by `tests/c/cgroup_lazy.c`;
    /// issuing a lazy batch here would make the flag sticky for the class
    /// and huge tests that read it.
    #[test]
    fn purged_memory_is_zero_by_default() {
        assert!(purged_is_zero());
        assert_eq!(advice(), Advice::Eager);
        assert_eq!(advice_reason(), "no headroom known");
    }

    /// The page size and the `MADV_POPULATE_WRITE` probe of the
    /// environment, as `api::init` reads them: the kernel's on Linux (the
    /// 5.10 and 64 KiB-page matrix cells found a 4 KiB constant and a
    /// forced populate word), 4 KiB and "unusable" in the synthetic host
    /// environment, where `os::populate_write` always fails.
    #[cfg(target_os = "linux")]
    fn prefault_env() -> (usize, bool) {
        let page = crate::sys::features::page_size();
        (page, crate::sys::features::probe_populate_write(page))
    }

    #[cfg(not(target_os = "linux"))]
    fn prefault_env() -> (usize, bool) {
        (4096, false)
    }

    /// The prefault pass's plumbing (plan Phase 2.5 task 11), host side,
    /// at the kernel's page size and in the populate direction the real
    /// probe reports: `populate` counts the bytes; configured as the probe
    /// says it never falls back (the advice works, or the touch is the
    /// populate); told the advice works where the probe says it does not
    /// (before 5.14, off Linux always), one fallback, the bytes still
    /// counted and the touch done, so a zero buffer stays zero either
    /// way; `note_commit` counts a wake only while the pass is enabled;
    /// `configure` is what `enabled` and the populate word report.
    #[test]
    fn prefault_populate_and_wakes_are_counted() {
        let _serial = serial();
        let (page, usable) = prefault_env();
        std::println!(
            "prefault test: page_size={page} populate_write={} (probed)",
            if usable { "yes" } else { "no" }
        );
        let buf = std::vec![0u8; 4 * page + page];
        let addr = (buf.as_ptr() as usize + page - 1) & !(page - 1);
        prefault::reset_for_test(false, false);
        assert!(!prefault::enabled() && !prefault::uses_populate_write());
        prefault::note_commit();
        assert_eq!(prefault::stats().wakes, 0, "off: no wake");
        prefault::populate(addr, 4 * page, page);
        let s = prefault::stats();
        assert_eq!((s.bytes, s.fallbacks), (4 * page as u64, 0), "the touch is the populate, not a fallback");
        assert!(buf.iter().all(|&b| b == 0));
        // The probe's direction, as `api::init` configures the pass.
        prefault::reset_for_test(true, usable);
        assert!(prefault::enabled() && prefault::uses_populate_write() == usable);
        prefault::note_commit();
        prefault::note_commit();
        // At least this test's two wakes (other tests' fresh commits wake
        // too while the pass is on here).
        assert!(prefault::stats().wakes >= 2);
        prefault::populate(addr, 2 * page, page);
        let s = prefault::stats();
        assert_eq!((s.bytes, s.fallbacks), (2 * page as u64, 0), "the probe's direction never falls back");
        assert!(buf.iter().all(|&b| b == 0));
        // The fallback contract: the populate word set where the kernel
        // lacks the advice (or the address is not page-aligned), the
        // `madvise` fails, one fallback is counted, the bytes are counted
        // and the touch populates; where the probe says yes this is the
        // probe's direction again.
        prefault::reset_for_test(true, true);
        prefault::populate(addr, page, page);
        let s = prefault::stats();
        let expected = if usable { 0 } else { 1 };
        assert_eq!((s.bytes, s.fallbacks), (page as u64, expected), "forced populate_write against the probe");
        assert!(buf.iter().all(|&b| b == 0));
        std::println!(
            "prefault test: probed direction fallbacks=0, forced populate_write fallbacks={} (expected {expected})",
            s.fallbacks
        );
        prefault::reset_for_test(false, false);
    }

    /// The advice's process-wide wiring (plan Phase 2.5 task 10): the
    /// policy's headroom mirror, the `lazy_band` key and an explicit purge
    /// in flight, read without any batch being flushed. Serialised with
    /// [`ADVICE_TEST`] against the batch tests, which flush.
    #[test]
    fn advice_follows_headroom_band_key_and_explicit_purges() {
        let _serial = serial();
        assert_eq!(advice(), Advice::Eager);
        set_headroom(true, false);
        assert_eq!(advice(), Advice::Lazy);
        assert_eq!(advice_reason(), "under 70% of the limit");
        {
            let _e = Explicit::enter();
            assert_eq!(advice(), Advice::Eager);
            assert_eq!(advice_reason(), "explicit purge");
            {
                let _e2 = Explicit::enter();
                assert_eq!(advice(), Advice::Eager);
            }
            assert_eq!(advice(), Advice::Eager);
        }
        assert_eq!(advice(), Advice::Lazy);
        configure_advice(false, false);
        assert_eq!(advice(), Advice::Eager);
        assert_eq!(advice_reason(), "lazy_band:false");
        assert!(!lazy_uses_process_madvise());
        configure_advice(false, true);
        assert_eq!(advice(), Advice::Lazy);
        set_headroom(false, true);
        assert_eq!(advice(), Advice::Eager);
        assert_eq!(advice_reason(), "returning the excess over the cap");
        set_headroom(false, false);
        assert_eq!(advice(), Advice::Eager);
        assert_eq!(advice_reason(), "no headroom known");
        // Nothing was flushed: the sticky flag is untouched.
        assert!(purged_is_zero());
        assert_eq!(batches(), (0, EAGER_BATCHES.load(Ordering::Relaxed)));
    }

    /// The batch tests flush (on Linux, for real); the advice test flips
    /// the headroom mirror; one at a time.
    pub(super) static ADVICE_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take [`ADVICE_TEST`]; a panic in one holder must not fail the rest.
    pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
        ADVICE_TEST.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use crate::sys::{self, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};
    use crate::sys::features::Features;

    /// Both tests below switch the process-wide backend; take turns.
    static BACKEND_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Resident pages of `[addr, addr + len)` via `mincore`.
    fn resident(addr: usize, len: usize, page: usize) -> usize {
        let n = len / page;
        let mut vec = std::vec![0u8; n];
        let rc = unsafe { ::libc::mincore(addr as *mut _, len, vec.as_mut_ptr()) };
        assert_eq!(rc, 0);
        vec.iter().filter(|&&b| b & 1 != 0).count()
    }

    /// A batch through whatever backend the kernel offers (and, when that
    /// is `process_madvise`, again through the loop) discards every range
    /// and leaves the pages zero.
    #[test]
    fn batch_discards_through_the_probed_backend_and_the_loop() {
        let _serial = BACKEND_TEST.lock().unwrap();
        let _advice = super::tests::serial();
        let page = crate::sys::features::page_size();
        let mut f = Features::probe();
        let backends = [f.process_madvise, ProcessMadvise::Unavailable];
        for (i, backend) in backends.iter().enumerate() {
            configure(*backend, false);
            assert_eq!(uses_process_madvise(), *backend != ProcessMadvise::Unavailable);
            let len = 8 * page;
            let addr = unsafe { sys::mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) }.unwrap();
            for p in 0..8 {
                unsafe { *((addr + p * page) as *mut u8) = 0xA5 };
            }
            assert_eq!(resident(addr, len, page), 8);
            let mut b = Batch::new();
            // Two ranges with a hole between them; the hole stays resident.
            b.push(addr, 3 * page);
            b.push(addr + 4 * page, 4 * page);
            b.flush();
            assert_eq!(resident(addr, len, page), 1, "backend {i}: {backend:?}");
            assert_eq!(unsafe { *((addr + 3 * page) as *const u8) }, 0xA5);
            for p in [0usize, 1, 2, 4, 5, 6, 7] {
                assert_eq!(unsafe { *((addr + p * page) as *const u8) }, 0, "page {p} not zero ({backend:?})");
            }
            unsafe { sys::munmap(addr, len) }.unwrap();
        }
        configure(ProcessMadvise::Unavailable, false);
        f.release();
    }

    /// The fork child drops an inherited pidfd and keeps `PIDFD_SELF`.
    #[test]
    fn fork_child_downgrades_a_pidfd_backend() {
        let _serial = BACKEND_TEST.lock().unwrap();
        configure(ProcessMadvise::PidfdSelf, false);
        assert_eq!(fork_child(), ProcessMadvise::PidfdSelf);
        assert!(uses_process_madvise());
        let fd = sys::open(b"/dev/null\0", sys::O_RDONLY).unwrap();
        configure(ProcessMadvise::Pidfd(fd), false);
        assert_eq!(fork_child(), ProcessMadvise::Unavailable);
        assert!(!uses_process_madvise());
        // The fd was closed.
        assert!(sys::close(fd).is_err());
        configure(ProcessMadvise::Unavailable, false);
        assert_eq!(fork_child(), ProcessMadvise::Unavailable);
    }
}
