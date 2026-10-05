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
use crate::lock::LockOrder;
#[cfg(verus_only)]
use crate::lock::rank_class;
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
///
/// The verified half of that invariant: the maker of a batch fixes `free`,
/// the bytes it may discard ([`Batch::new`]), and [`Batch::push`]
/// takes only ranges of such bytes, so every byte a flush hands to
/// `madvise` is one ([`discard`]). Each maker proves what its `free`
/// means (whole-allocator theorem "Purge never discards live data",
/// `verus/THEOREMS.md`): for the huge allocator, pages no live run's
/// token holds (`RunToken::theorem_purge_apart`); for a class, the slabs
/// the arena's class lock records as idle (`class::decay::idle_byte`).
pub struct Batch {
    ranges: [(usize, usize); MAX_BATCH],
    n: usize,
    /// Ghost: the bytes the batch may discard.
    #[allow(dead_code)]
    free: Ghost<spec_fn(int) -> bool>,
}

/// Every byte of `[addr, addr + len)` is one `free` admits.
pub open spec fn frees(free: spec_fn(int) -> bool, addr: int, len: int) -> bool {
    forall|b: int| addr <= b < addr + len ==> #[trigger] free(b)
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

/// Every byte of units (slabs, pages) `first..first + k` of a range at
/// `base` of units of `sz` bytes lies in one of them: its index
/// `(b - base) / sz`.
pub proof fn lemma_unit_of(base: int, sz: int, first: int, k: int, b: int)
    requires
        sz > 0,
        0 <= first,
        base + first * sz <= b < base + (first + k) * sz,
    ensures
        first <= (b - base) / sz < first + k,
{
    let d = b - base;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(d, sz);
    let q = d / sz;
    let r = d % sz;
    assert(0 <= r < sz) by {
        vstd::arithmetic::div_mod::lemma_mod_bound(d, sz);
    }
    assert(first <= q) by {
        if q < first {
            assert(q * sz + sz <= first * sz) by (nonlinear_arith)
                requires q + 1 <= first, sz > 0;
            assert(sz * q == q * sz) by (nonlinear_arith);
        }
    }
    assert(q < first + k) by {
        if q >= first + k {
            assert((first + k) * sz <= q * sz) by (nonlinear_arith)
                requires first + k <= q, sz > 0;
            assert(sz * q == q * sz) by (nonlinear_arith);
        }
    }
}

impl Batch {
    /// At most [`MAX_BATCH`] ranges, none of which ends past the address
    /// space, whose lengths sum to at most `usize::MAX`, each of bytes
    /// `free` admits.
    pub closed spec fn wf(&self) -> bool {
        &&& self.n <= MAX_BATCH
        &&& forall|i: int| #![auto] 0 <= i < self.n ==> self.ranges[i].0 + self.ranges[i].1 <= usize::MAX
        &&& self.total() <= usize::MAX
        &&& forall|i: int| #![auto] 0 <= i < self.n
            ==> frees(self.free@, self.ranges[i].0 as int, self.ranges[i].1 as int)
    }

    /// The bytes the batch may discard, fixed by its maker
    /// ([`Batch::new`]).
    pub closed spec fn free(&self) -> spec_fn(int) -> bool {
        self.free@
    }

    /// Total length of the ranges held.
    pub closed spec fn total(&self) -> int {
        lens(self.ranges@, self.n as int)
    }

    /// Ranges held.
    pub closed spec fn count(&self) -> nat {
        self.n as nat
    }

    /// An empty batch that may discard the bytes `free` admits.
    pub const fn new(Ghost(free): Ghost<spec_fn(int) -> bool>) -> (b: Batch)
        ensures
            b.wf(),
            b.total() == 0,
            b.count() == 0,
            b.free() == free,
    {
        Batch { ranges: [(0, 0); MAX_BATCH], n: 0, free: Ghost(free) }
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

    /// Add `[addr, addr + len)`, bytes `free` admits, flushing first if
    /// the batch is full.
    pub fn push(&mut self, addr: usize, len: usize)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(self).wf(),
            addr + len <= usize::MAX,
            old(self).total() + len <= usize::MAX,
            frees(old(self).free(), addr as int, len as int),
        ensures
            final(self).wf(),
            final(self).total() <= old(self).total() + len,
            final(self).free() == old(self).free(),
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
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(self).wf(),
        ensures
            final(self).wf(),
            final(self).count() == 0,
            final(self).total() == 0,
            final(self).free() == old(self).free(),
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
            discard(&self.ranges[..self.n], advice, Ghost(self.free@));
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
    /// Lock-free unless a walk is due. The result is [`spec_decay`] of
    /// the values the call's loads see (`verus/THEOREMS.md`, "Decay
    /// deadline soundness").
    ///
    /// [`spec_decay`]: Decayer::spec_decay
    pub fn decay(&self, now: u64, decay_ns: u64, walk: impl FnOnce(u64, &Curve)) -> (r: Option<u64>)
        requires
            forall|t: u64, c: &Curve| walk.requires((t, c)),
        ensures
            r matches Some(t) ==> t >= now,
            exists|oldest: u64, next: u64, after: u64|
                r == #[trigger] Decayer::spec_decay(now, decay_ns, oldest, next, after),
    {
        let oldest = self.oldest_empty();
        if oldest == 0 {
            proof {
                assert(Decayer::spec_decay(now, decay_ns, oldest, 0, 0) == None::<u64>);
            }
            return None;
        }
        let curve = Curve::new(decay_ns);
        let step = curve.step_ns();
        let first = oldest.saturating_add(step);
        if now < first {
            proof {
                assert(Decayer::spec_decay(now, decay_ns, oldest, 0, 0) == Some(first));
            }
            return Some(first);
        }
        // A deadline further away than one step was set under a longer
        // period since shortened: walk now.
        let next = self.next_walk.load(Ordering::Relaxed);
        if now < next && next - now <= step {
            proof {
                assert(Decayer::spec_decay(now, decay_ns, oldest, next, 0) == Some(next));
            }
            return Some(next);
        }
        walk(now, &curve);
        let ghost seen = next;
        let next = now.saturating_add(step);
        self.next_walk.store(next, Ordering::Relaxed);
        let after = self.oldest_empty();
        proof {
            assert(Decayer::spec_decay(now, decay_ns, oldest, seen, after) == if after == 0 {
                None
            } else {
                Some(next)
            });
        }
        if after == 0 {
            None
        } else {
            Some(next)
        }
    }

    /// `a + b` saturated at `u64::MAX` (`u64::saturating_add`).
    pub open spec fn sat_add(a: u64, b: u64) -> u64 {
        if a + b > u64::MAX { u64::MAX } else { (a + b) as u64 }
    }

    /// One step of the curve for `decay_ns` ([`Curve::step_ns`]).
    pub open spec fn spec_step(decay_ns: u64) -> u64 {
        decay_ns / STEPS as u64
    }

    /// `decay(now, decay_ns, _)` runs the walk: something is `Empty` on
    /// entry (`oldest`, the first `oldest_empty` load), its first step is
    /// over, and the stored deadline (`next`, the `next_walk` load) is
    /// not within one step after `now`: it has passed, or it was set
    /// under a longer period.
    pub open spec fn spec_walks(now: u64, decay_ns: u64, oldest: u64, next: u64) -> bool {
        let step = Decayer::spec_step(decay_ns);
        &&& oldest != 0
        &&& now >= Decayer::sat_add(oldest, step)
        &&& !(now < next && next - now <= step)
    }

    /// What `decay(now, decay_ns, _)` returns when its loads see `oldest`
    /// (`oldest_empty` on entry), `next` (`next_walk`) and `after`
    /// (`oldest_empty` after the walk, as the walk republished it or a
    /// concurrent writer left it). A load the call does not make does not
    /// matter.
    pub closed spec fn spec_decay(now: u64, decay_ns: u64, oldest: u64, next: u64, after: u64) -> Option<u64> {
        let step = Decayer::spec_step(decay_ns);
        if oldest == 0 {
            None
        } else if now < Decayer::sat_add(oldest, step) {
            Some(Decayer::sat_add(oldest, step))
        } else if now < next && next - now <= step {
            Some(next)
        } else if after == 0 {
            None
        } else {
            Some(Decayer::sat_add(now, step))
        }
    }

    /// Decay deadline soundness: whatever its loads see,
    /// [`decay`](Decayer::decay) returns `None` exactly when nothing is
    /// `Empty` on entry or the walk ran and the `oldest_empty` load after
    /// it reads 0; a deadline is no earlier than `now` and at most one
    /// step of the period it was called with past `max(oldest, now)`.
    /// When the oldest `Empty` member is not stamped after `now`, the
    /// deadline is at most one step of the current period past `now`: a
    /// `next_walk` more than one step away (set under a longer period
    /// since shortened: `arena.<i>.dirty_decay_ms`, a pressure band) is
    /// never returned. A deadline equal to `now` comes only from a zero
    /// step or saturation, and only after a walk.
    pub proof fn theorem_decay_deadline(now: u64, decay_ns: u64, oldest: u64, next: u64, after: u64)
        ensures
            ({
                let r = Decayer::spec_decay(now, decay_ns, oldest, next, after);
                let step = Decayer::spec_step(decay_ns);
                &&& r is None <==> oldest == 0 || (Decayer::spec_walks(now, decay_ns, oldest, next)
                    && after == 0)
                &&& r matches Some(t) ==> now <= t && t <= (if oldest <= now { now } else { oldest })
                    + step
                &&& r matches Some(t) ==> (oldest <= now ==> t <= now + step)
                &&& r matches Some(t) ==> (t == now ==> Decayer::spec_walks(now, decay_ns, oldest, next)
                    && (step == 0 || now == u64::MAX))
            }),
    {
    }

    /// Decay deadline soundness under pressure (with theorem 18): at one
    /// limit and one configured period `base`, the bound on `decay`'s
    /// deadline at usage `c2` is no later than at a lower usage `c1`,
    /// whatever the loads see. `p1` and `p2` are the effective periods
    /// for `c1` and `c2` (theorem 18's `spec_effective_decay_ns`).
    pub proof fn theorem_decay_deadline_under_pressure(
        base: u64,
        limit: u64,
        c1: u64,
        c2: u64,
        p1: u64,
        p2: u64,
        now: u64,
        oldest: u64,
        next: u64,
        after: u64,
    )
        requires
            c1 <= c2,
            p1 == crate::policy::spec_effective_decay_ns(base, crate::policy::spec_band(limit, c1)),
            p2 == crate::policy::spec_effective_decay_ns(base, crate::policy::spec_band(limit, c2)),
        ensures
            Decayer::spec_step(p2) <= Decayer::spec_step(p1),
            Decayer::spec_decay(now, p2, oldest, next, after) matches Some(t) ==> t <= (if oldest
                <= now { now } else { oldest }) + Decayer::spec_step(p1),
    {
        crate::policy::theorem_pressure_never_lengthens_decay(base, limit, c1, c2);
        vstd::arithmetic::div_mod::lemma_div_is_ordered(p2 as int, p1 as int, STEPS as int);
        Decayer::theorem_decay_deadline(now, p2, oldest, next, after);
    }
}

/// Arena indices to visit: [`crate::arena::Arenas::count`], which never
/// exceeds the table (`create` stops at [`MAX_ARENAS`]; `count` is
/// verified to ensure it, so the clamp is redundant and stays only to
/// keep the machine code).
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
pub fn purge_all(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>) -> usize
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    let _explicit = Explicit::enter();
    let mut n: usize = 0;
    let count = arena_count(shared);
    for i in 0..count
        invariant
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            ord.allows(rank_class(0, 0)),
            *ord == *old(ord),
            count <= crate::arena::MAX_ARENAS,
            n <= i * 0x8_0000_0000_0000,
    {
        if let Some(arena) = shared.arenas.get_existing(i as u16) {
            // At most `NCLASSES * 2^45` slabs an arena.
            n += arena.purge_all(Tracked(ord));
        }
    }
    n + shared.huge.purge_empty(u64::MAX, Tracked(ord))
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
pub fn decay(shared: &Shared, now: u64, Tracked(ord): Tracked<&mut LockOrder>) -> Option<u64>
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    // Design 11: a `memory.events` `high` rise owes one full purge, run
    // before the walks so they see the survivors (none, normally).
    let policy = &shared.policy;
    if policy.take_full_purge() {
        purge_all(shared, Tracked(ord));
    }
    let keep16 = policy.keep16();
    let mut next: Option<u64> = None;
    let count = arena_count(shared);
    for i in 0..count
        invariant
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            ord.allows(rank_class(0, 0)),
            *ord == *old(ord),
            count <= crate::arena::MAX_ARENAS,
    {
        if let Some(arena) = shared.arenas.get_existing(i as u16) {
            // Each arena's own period, scaled by the policy's band.
            next = earliest(next, arena.decay_with(now, policy.effective_decay_ns(arena.decay_ns()), keep16, Tracked(ord)));
        }
    }
    // The huge allocator is shared by every arena: its own period when
    // `huge_dirty_decay_ms` is set, else arena 0's (design 7).
    let decay_ns = policy.effective_decay_ns(shared.huge_decay_ns());
    if decay_ns != NEVER {
        next = earliest(next, shared.huge.decay_with(now, decay_ns, keep16, Tracked(ord)));
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
pub fn purge_arena(shared: &Shared, index: u16, Tracked(ord): Tracked<&mut LockOrder>) -> Option<usize>
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    let _explicit = Explicit::enter();
    let arena = shared.arenas.get_existing(index)?;
    let mut n = arena.purge_all(Tracked(ord));
    if index == 0 {
        n += shared.huge.purge_empty(u64::MAX, Tracked(ord));
    }
    Some(n)
}

/// Periodic-work hook (design 6.4): start the background thread if one was
/// requested and does not exist, otherwise run decay here unless the
/// thread is doing it.
pub fn on_tick(shared: &Shared, now: u64, Tracked(ord): Tracked<&mut LockOrder>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    sample(shared, now, Tracked(ord));
    if background::present() {
        return;
    }
    if background::requested() && background::start(shared, Tracked(ord)) {
        return;
    }
    let _ = decay(shared, now, Tracked(ord));
}

/// Design 11: `memory.current` and `memory.events`, at most once per 10 ms
/// of the coarse clock, and the policy evaluated on the sample that was
/// taken; nothing at all when the cgroup is untracked or the window has
/// not passed. Slow paths only (the periodic tick, the background thread).
fn sample(shared: &Shared, now: u64, Tracked(ord): Tracked<&mut LockOrder>)
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    if shared.cgroup.sample(now) {
        crate::policy::evaluate(shared, Tracked(ord));
    }
}

} // verus!

verus! {

/// Issue `advice` on `ranges`, which are free (the [`Batch`]
/// invariant; this is the one place the batch's ranges reach the
/// kernel): every byte of each is one the batch's `free` admits. A
/// failed range aborts. Needs at most [`MAX_BATCH`] ranges.
// Not isolated: the loop body sees the `broadcast use` of the function.
#[verifier::loop_isolation(false)]
fn discard(ranges: &[(usize, usize)], advice: Advice, Ghost(free): Ghost<spec_fn(int) -> bool>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        ranges.len() <= MAX_BATCH,
        forall|i: int| #![auto] 0 <= i < ranges.len() ==> frees(free, ranges[i].0 as int, ranges[i].1 as int),
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

/// Tests: a batch with no ghost predicate (the tests hold no ghost state).
#[cfg(test)]
impl Batch {
    pub fn new_t() -> Batch {
        Batch::new(Ghost::assume_new())
    }
}

/// The background thread (design 8.1).
pub mod background;

/// Kani check of `total_bytes` (record under `verus/seams/`).
#[cfg(kani)]
mod kani_total {
    use super::total_bytes;

    /// The most ranges checked.
    const MAX: usize = 4;

    /// S184 `total_bytes`, `requires lens(ranges, len) <= u64::MAX`, no
    /// `ensures`: under it the call returns (the sum, which cannot
    /// overflow), for every list of up to `MAX` ranges.
    #[kani::proof]
    #[kani::unwind(6)]
    fn total_bytes_returns() {
        let r: [(usize, usize); MAX] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= MAX);
        let mut lens: u128 = 0;
        for &(_, len) in &r[..n] {
            lens += len as u128;
        }
        kani::assume(lens <= u64::MAX as u128);
        assert_eq!(total_bytes(&r[..n]) as u128, lens);
    }
}

/// Kani check of `Explicit`'s `drop` (record under `verus/seams/`).
#[cfg(kani)]
mod kani_explicit {
    use super::{EXPLICIT, Explicit};
    use core::sync::atomic::Ordering::Relaxed;

    /// S183 `Explicit::drop`: no `requires`, no `ensures`, `no_unwind`:
    /// it returns without a panic for every count `EXPLICIT` holds (one
    /// less afterwards, wrapping at zero as an atomic subtraction does),
    /// both when a guard from `Explicit::enter` drops and when the count
    /// was changed in between.
    #[kani::proof]
    fn explicit_drop_returns() {
        let c: u32 = kani::any();
        EXPLICIT.store(c, Relaxed);
        let g = Explicit::enter();
        assert_eq!(EXPLICIT.load(Relaxed), c.wrapping_add(1));
        let d: u32 = kani::any();
        EXPLICIT.store(d, Relaxed);
        drop(g);
        assert_eq!(EXPLICIT.load(Relaxed), d.wrapping_sub(1));
    }
}
