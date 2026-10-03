//! Decay and purge (design.md section 8) and the background thread (8.1).
//!
//! - [`Batch`], the discard primitive. Callers collect up to [`MAX_BATCH`]
//!   ranges, merging adjacent ones, and flush them with a loop of
//!   `madvise`. The advice is decided at each flush ([`advice`], design 8
//!   and 11): `MADV_FREE` under the cgroup policy's headroom band
//!   ([`set_headroom`]), `MADV_DONTNEED` everywhere else and always for an
//!   explicit purge ([`purge_all`], [`purge_arena`]). A range that fails to
//!   discard aborts: the heap's idea of what is resident would otherwise be
//!   wrong.
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
//!   on the first periodic tick after it was requested, never from init, on
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
//! `serial` lock the class tests share in `purge/tests.rs`, and the one that
//! needs a kernel in `purge/linux_tests.rs`.

use crate::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::heap::Shared;
pub use crate::policy::Advice;
use crate::sys;

mod curve;
#[cfg(test)]
mod linux_tests;
#[cfg(test)]
pub(crate) mod tests;

pub use curve::{Curve, NEVER, STEPS, decay_ns_from_ms};
use vstd::prelude::*;

verus! {

/// Ranges per batch (design section 8).
pub const MAX_BATCH: usize = 64;

/// `lazy_band` (default true): `MADV_FREE` under the headroom band.
exec static LAZY_BAND: AtomicBool = AtomicBool::new(true);
/// The policy's last evaluation put usage under 70% of a known limit with
/// no eager return of the excess over the cap in progress
/// (`policy::evaluate` mirrors `Policy::headroom` here).
exec static HEADROOM: AtomicBool = AtomicBool::new(false);
/// The policy's last evaluation found an eager return of the excess over
/// the cap in progress (`Policy::eager_excess`; the reason line only).
exec static EAGER_EXCESS: AtomicBool = AtomicBool::new(false);
/// Explicit purges in flight ([`purge_all`], [`purge_arena`]): eager.
exec static EXPLICIT: AtomicU32 = AtomicU32::new(0);
/// A lazy batch has been issued in this process: `Purged` memory is no
/// longer known to be zero (the module doc). Sticky.
exec static LAZY_EVER: AtomicBool = AtomicBool::new(false);
/// Batches and bytes issued with each advice (the report, `stats.cgroup.*`).
exec static LAZY_BATCHES: AtomicU64 = AtomicU64::new(0);
exec static EAGER_BATCHES: AtomicU64 = AtomicU64::new(0);
exec static LAZY_BYTES: AtomicU64 = AtomicU64::new(0);
exec static EAGER_BYTES: AtomicU64 = AtomicU64::new(0);
/// `check_zero`: the debug mode that reads every slot or page handed out
/// as "known zero" and aborts on a dirty word.
exec static CHECK_ZERO: AtomicBool = AtomicBool::new(false);

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

/// `(lazy batches, eager batches)` flushed so far.
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

/// An explicit purge in progress: every flush until the guard drops is
/// eager (the caller asked for RSS to drop), whatever the band.
struct Explicit;

impl Explicit {
    fn enter() -> Explicit {
        EXPLICIT.fetch_add(1, Ordering::Relaxed);
        Explicit
    }
}

// Trusted: Verus wants a destructor that opens no invariant and cannot
// unwind; the body is one atomic subtract.
impl Drop for Explicit {
    #[verifier::external_body]
    fn drop(&mut self)
        opens_invariants none
        no_unwind
    {
        EXPLICIT.fetch_sub(1, Ordering::Relaxed);
    }
}

} // verus!

verus! {

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
/// `purge_curve`, `Huge::free_run`, `purge_empty` and `purge_curve`) push
/// under the class or huge lock that keeps the ranges free and flush
/// before releasing it; the batch never outlives that hold.
pub struct Batch {
    ranges: [(usize, usize); MAX_BATCH],
    n: usize,
}

/// Total length of the first `k` of `ranges`.
pub open spec fn lens(ranges: Seq<(usize, usize)>, k: int) -> int
    decreases k,
{
    if k <= 0 { 0 } else { lens(ranges, k - 1) + ranges[k - 1].1 }
}

/// `lens` of a prefix depends only on that prefix, and never falls as
/// the prefix grows.
proof fn lemma_lens(a: Seq<(usize, usize)>, b: Seq<(usize, usize)>, k: int)
    requires
        0 <= k <= a.len(),
        k <= b.len(),
        forall|i: int| 0 <= i < k ==> a[i] == b[i],
    ensures
        lens(a, k) == lens(b, k),
        0 <= lens(a, k),
        forall|j: int| 0 <= j <= k ==> lens(a, j) <= lens(a, k),
    decreases k,
{
    if k > 0 {
        lemma_lens(a, b, k - 1);
    }
}

impl Batch {
    /// At most [`MAX_BATCH`] ranges, none of which ends past the address
    /// space, whose lengths sum to at most `usize::MAX`.
    pub closed spec fn wf(&self) -> bool {
        &&& self.n <= MAX_BATCH
        &&& forall|i: int| #![auto] 0 <= i < self.n ==> self.ranges[i].0 + self.ranges[i].1 <= usize::MAX
        &&& self.total() <= usize::MAX
    }

    /// Total length of the ranges held.
    pub closed spec fn total(&self) -> int {
        lens(self.ranges@, self.n as int)
    }

    /// Ranges held.
    pub closed spec fn count(&self) -> nat {
        self.n as nat
    }

    pub const fn new() -> (b: Batch)
        ensures
            b.wf(),
            b.total() == 0,
    {
        Batch { ranges: [(0, 0); MAX_BATCH], n: 0 }
    }

    #[inline]
    #[cfg(test)]
    pub fn len(&self) -> (n: usize)
        ensures
            n == self.count(),
    {
        self.n
    }

    #[inline]
    #[cfg(test)]
    pub fn is_empty(&self) -> (e: bool)
        ensures
            e == (self.count() == 0),
    {
        self.n == 0
    }

    #[inline]
    pub fn is_full(&self) -> (f: bool)
        ensures
            f == (self.count() == MAX_BATCH),
    {
        self.n == MAX_BATCH
    }

    /// Add `[addr, addr + len)` (free, per the type's invariant), flushing
    /// first if the batch is full.
    pub fn push(&mut self, addr: usize, len: usize)
        requires
            crate::sys::may_syscall(),
            old(self).wf(),
            addr + len <= usize::MAX,
            old(self).total() + len <= usize::MAX,
        ensures
            final(self).wf(),
            final(self).total() <= old(self).total() + len,
    {
        proof {
            lemma_lens(self.ranges@, self.ranges@, self.n as int);
        }
        // Adjacent ranges pushed in address order (a run of empty 4 KiB
        // slabs) go out as one.
        if self.n != 0 {
            let (last, last_len) = self.ranges[self.n - 1];
            if last + last_len == addr {
                let ghost before = self.ranges@;
                proof {
                    lemma_lens(before, before, self.n as int);
                    lemma_lens(before, before, self.n - 1);
                }
                self.ranges[self.n - 1].1 = last_len + len;
                proof {
                    lemma_lens(before, self.ranges@, self.n - 1);
                }
                return;
            }
        }
        if self.is_full() {
            self.flush();
        }
        let ghost before = self.ranges@;
        let ghost t0 = self.total();
        self.ranges[self.n] = (addr, len);
        proof {
            lemma_lens(before, self.ranges@, self.n as int);
        }
        self.n += 1;
        proof {
            assert(lens(self.ranges@, self.n as int) == lens(self.ranges@, self.n - 1) + len);
            assert(t0 <= old(self).total());
        }
    }

    /// Discard every range with `madvise`, then empty the batch. The advice is
    /// [`advice`] at this moment, counted per batch and per byte; a lazy
    /// flush makes `Purged` memory unknown-zero for the rest of the process
    /// (the module doc), which is published before the discard so a claim
    /// of one of these ranges after the caller's lock drops sees it. Aborts
    /// if a range cannot be discarded. The ranges are free (the type's
    /// invariant), which is what makes the discard sound.
    pub fn flush(&mut self)
        requires
            crate::sys::may_syscall(),
            old(self).wf(),
        ensures
            final(self).wf(),
            final(self).count() == 0,
            final(self).total() == 0,
    {
        if self.n != 0 {
            let advice = advice();
            let ranges = &self.ranges[..self.n];
            proof {
                lemma_lens(ranges@, self.ranges@, self.n as int);
            }
            let bytes = total_bytes(ranges);
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
            discard(&self.ranges[..self.n], advice);
            self.n = 0;
        }
    }
}

/// The sum of the lengths of `ranges`. Trusted: `Iterator::sum`, which vstd
/// does not specify; kept as the iterator chain so the free path of a huge
/// run that purges at once compiles as it always has.
#[verifier::external_body]
#[inline(always)]
fn total_bytes(ranges: &[(usize, usize)]) -> (bytes: u64)
    requires
        lens(ranges@, ranges.len() as int) <= u64::MAX,
{
    ranges.iter().map(|&(_, len)| len as u64).sum()
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
        Decayer {
            oldest_empty: AtomicU64::new(0),
            next_walk: AtomicU64::new(0),
        }
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
    pub fn decay(&self, now: u64, decay_ns: u64, walk: impl FnOnce(u64, &Curve)) -> (r: Option<u64>)
        requires
            forall|t: u64, c: &Curve| walk.requires((t, c)),
        ensures
            r matches Some(t) ==> t >= now,
    {
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
        if self.oldest_empty() == 0 {
            None
        } else {
            Some(next)
        }
    }
}

/// Arena indices to visit: [`crate::arena::Arenas::count`], which never
/// exceeds the table (`create` stops at [`MAX_ARENAS`]); the clamp states
/// that for the verifier.
///
/// [`MAX_ARENAS`]: crate::arena::MAX_ARENAS
#[inline]
fn arena_count(shared: &Shared) -> (n: usize)
    ensures
        n <= crate::arena::MAX_ARENAS,
{
    (shared.arenas.count() as usize).min(crate::arena::MAX_ARENAS)
}

/// Purge every `Empty` slab of every arena and every huge extent
/// regardless of age, eagerly (`malloc_trim(0)`, the `high`-rise full
/// purge: the memory is wanted back now). Returns how many were purged.
pub fn purge_all(shared: &Shared) -> usize
    requires
        crate::sys::may_syscall(),
{
    let _explicit = Explicit::enter();
    let mut n: usize = 0;
    let count = arena_count(shared);
    for i in 0..count
        invariant
            crate::sys::may_syscall(),
            count <= crate::arena::MAX_ARENAS,
            n <= i * 0x8_0000_0000_0000,
    {
        if let Some(arena) = shared.arenas.get_existing(i as u16) {
            // At most `NCLASSES * 2^45` slabs an arena.
            n += arena.purge_all();
        }
    }
    n + shared.huge.purge_empty(u64::MAX)
}

/// A class or the huge allocator went from no `Empty` slab to one: wake a
/// background thread that may be sleeping without a deadline.
#[inline]
pub fn note_empty()
    requires
        crate::sys::may_syscall(),
{
    background::wake();
}

/// A decay period changed (`arena.<i>.dirty_decay_ms`, `malloc_conf` at
/// run time): wake a background thread whose deadline was computed with
/// the old period, so a shorter period takes effect now rather than at
/// the next wake-up.
pub fn note_decay_changed()
    requires
        crate::sys::may_syscall(),
{
    background::wake();
}

/// Run every decay walk that is due at `now` (coarse monotonic
/// nanoseconds) and return the earliest future deadline, `None` when
/// nothing is `Empty` or decay is off. Lock-free for a class with nothing
/// due.
pub fn decay(shared: &Shared, now: u64) -> Option<u64>
    requires
        crate::sys::may_syscall(),
{
    // Design 11: a `memory.events` `high` rise owes one full purge, run
    // before the walks so they see the survivors (none, normally).
    let policy = &shared.policy;
    if policy.take_full_purge() {
        purge_all(shared);
    }
    let keep16 = policy.keep16();
    let mut next: Option<u64> = None;
    let count = arena_count(shared);
    for i in 0..count
        invariant
            crate::sys::may_syscall(),
            count <= crate::arena::MAX_ARENAS,
    {
        if let Some(arena) = shared.arenas.get_existing(i as u16) {
            // Each arena's own period, scaled by the policy's band.
            next = earliest(next, arena.decay_with(now, policy.effective_decay_ns(arena.decay_ns()), keep16));
        }
    }
    // The huge allocator is shared by every arena: its own period when
    // `huge_dirty_decay_ms` is set, else arena 0's (design 7).
    let decay_ns = policy.effective_decay_ns(shared.huge_decay_ns());
    if decay_ns != NEVER {
        next = earliest(next, shared.huge.decay_with(now, decay_ns, keep16));
    }
    next
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

/// `arena.<i>.purge`: purge every `Empty` slab of one arena regardless of
/// age (plus, for arena 0, the huge extents, which belong to no single
/// arena's sets), eagerly. `None` if the arena does not exist.
pub fn purge_arena(shared: &Shared, index: u16) -> Option<usize>
    requires
        crate::sys::may_syscall(),
{
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
pub fn on_tick(shared: &Shared, now: u64)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
{
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
fn sample(shared: &Shared, now: u64)
    requires
        crate::sys::may_syscall(),
{
    if shared.cgroup.sample(now) {
        crate::policy::evaluate(shared);
    }
}

} // verus!

verus! {

/// Issue `advice` on `ranges`, which are free (the [`Batch`]
/// invariant; this is the one place the batch's ranges reach the
/// kernel). A failed range aborts. Needs at most [`MAX_BATCH`] ranges.
// Not isolated: the loop body sees the `broadcast use` of the function.
#[verifier::loop_isolation(false)]
fn discard(ranges: &[(usize, usize)], advice: Advice)
    requires
        crate::sys::may_syscall(),
        ranges.len() <= MAX_BATCH,
{
    broadcast use crate::vspec::fmt::lemma_errno_fmt_req;

    let advice = advice.madv();
    for &(addr, len) in ranges {
        // SAFETY: the ranges are page-aligned parts of our own mappings
        // that the caller holds free (the `Batch` invariant).
        if let Err(e) = unsafe { sys::madvise(addr, len, advice) } {
            sys::abort_fmt(format_args!("purge: madvise({advice}) of {addr:#x}+{len:#x} failed: {e}"));
        }
    }
}

} // verus!

/// The background thread (design 8.1).
pub mod background;
