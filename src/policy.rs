//! The cgroup policy of design.md section 11: what the allocator does with
//! the figures `sys::cgroup` samples.
//!
//! The pure part ([`band`], [`effective_decay_ns`], [`retention_cap`],
//! [`keep_ratio`]) maps the effective limit (`min(memory.high,
//! memory.max)` up the hierarchy) and the last `memory.current` sample to
//! the table's rows:
//!
//! | usage                | decay period            | retention          | cache GC          |
//! |----------------------|-------------------------|--------------------|-------------------|
//! | no limit             | as configured           | as configured      | as configured     |
//! | under 70%            | as configured           | capped at 5%       | as configured     |
//! | 70% to 90%           | scaled linearly to 1 s  | capped at 5%       | as configured     |
//! | 90% and above        | 0 (purge at once)       | (everything goes)  | every periodic tick |
//! | `memory.events` `high` rose | one full purge   |                    |                   |
//!
//! *Retention* is the `Empty` memory of every class of every arena and of
//! the huge allocator: freed dirty pages not yet purged, the only memory
//! the allocator holds that costs RSS without serving anything. The cap is
//! applied as a *keep ratio* inside the curve walks of design 8: a class
//! (or the huge allocator) whose walk is due may keep the smaller of the
//! curve's allowance and its share `total * cap / retention` of the cap, so
//! the sum kept stays under the cap while the curve decides which slabs go
//! and in what order. Scaling the period scales the curve's window, never
//! its shape. `dirty_decay_ms:-1` (never) is left alone in every band.
//!
//! The policy is evaluated where a sample was taken ([`evaluate`], from
//! `purge::on_tick` and the background thread's loop),
//! so at most once per 10 ms process-wide and never on a fast path. Without
//! a limit it is two loads and a return; with one it also sums the `Empty`
//! bytes of every class that has any (one lock hold per such class). The
//! outcome is a handful of atomics on [`Policy`], on a line the periodic
//! tick already reads: `purge::decay` turns them into a per-arena effective
//! period and keep ratio, `Heap::tick` into a zero GC interval above 90%.
//!
//! The advice ([`advice`]): a purge issues `MADV_FREE` (lazy: the pages
//! stay mapped and charged until the kernel reclaims them) only where the
//! policy knows the headroom, under 70% of a known limit, and
//! `MADV_DONTNEED` (eager) everywhere else: with no limit (lazily freed
//! pages would stay in RSS and mislead anything that reads it, design 8),
//! in the scaled and immediate bands, for an explicit purge
//! (`malloc_trim(0)`, `arena.<i>.purge`, the full purge a `memory.events`
//! `high` rise owes), and while an eager return of the excess over the
//! retention cap is in progress ([`Policy::headroom`]). That last rule
//! keeps an excess first seen at 70% or above eager until it is back within
//! the cap, even after the first walks bring usage under 70%: otherwise its
//! tail would go out lazily and stay charged to the cgroup. `lazy_band:false`
//! makes every purge eager. The advice is read at each batch flush from
//! `purge`'s mirror of the band, at most one sample behind.
//!
//! THP: the `thp` key's mechanism is `thp.rs`. Design 11's "disabled under
//! small limits" is applied once, at init (`Shared::init`): under an
//! effective limit below [`THP_MIN_LIMIT`] (1 GiB) a configured
//! `thp:always` is forced to `never` in the stored configuration, and the
//! report says so ([`Policy::thp_forced`], `opt.thp` reads `never`). The
//! rule itself is `thp::forced_by_limit`.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::heap::Shared;
use crate::purge::NEVER;
use crate::sys::cgroup::UNLIMITED;

/// Fixed-point one for the band position and the keep ratio.
pub const FRAC_ONE: u32 = 1 << 16;
/// Keep ratio meaning "no cap".
pub const KEEP_ALL: u32 = FRAC_ONE;
/// Usage (parts per million of the limit) where the scaled band starts.
pub const SCALED_FROM_PPM: u64 = 700_000;
/// Usage where the immediate band starts.
pub const IMMEDIATE_FROM_PPM: u64 = 900_000;
/// The shortest period the scaled band reaches (1000 ms).
pub const FLOOR_NS: u64 = 1_000_000_000;
/// Retention cap: the limit divided by this (5%).
pub const RETENTION_DIVISOR: u64 = 20;
/// Effective limit under which `thp:always` is forced to `never`
/// (`thp::forced_by_limit`).
pub const THP_MIN_LIMIT: u64 = 1 << 30;

/// The row of the table usage falls in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    /// No effective limit (or nothing tracked): everything as configured.
    Unlimited,
    /// Under 70%: defaults plus the retention cap.
    Low,
    /// 70% to 90%: the position within the band in [`FRAC_ONE`] units
    /// (0 at 70%, `FRAC_ONE` just under 90%), plus the cap.
    Scaled(u32),
    /// 90% and above: purge at once, GC every tick.
    Immediate,
}

/// `num / den` in [`FRAC_ONE`] units for `num <= den`, `den > 0`; `u64`
/// arithmetic only (a 128-bit division would pull the unwinding helpers
/// into the `no_std` library, see `purge::Curve::step`).
pub fn frac16(mut num: u64, mut den: u64) -> u32 {
    debug_assert!(den > 0 && num <= den);
    while den > 1 << 47 {
        num >>= 1;
        den >>= 1;
    }
    ((num << 16) / den).min(FRAC_ONE as u64) as u32
}

/// `x * f / FRAC_ONE` without overflow for any `x`.
pub fn mul16(x: u64, f: u32) -> u64 {
    let f = f as u64;
    (x >> 16) * f + ((x & 0xffff) * f >> 16)
}

/// Usage as parts per million of the limit; saturates at 1e6 when
/// `current >= limit`. `limit > 0` and not [`UNLIMITED`].
pub fn usage_ppm(limit: u64, current: u64) -> u64 {
    if current >= limit {
        return 1_000_000;
    }
    let (mut c, mut l) = (current, limit);
    while l > 1 << 43 {
        c >>= 1;
        l >>= 1;
    }
    if l == 0 {
        return 1_000_000;
    }
    c * 1_000_000 / l
}

/// The row for `limit` (the effective `min(high, max)`) and `current`.
pub fn band(limit: u64, current: u64) -> Band {
    if limit == UNLIMITED || limit == 0 {
        return Band::Unlimited;
    }
    let ppm = usage_ppm(limit, current);
    if ppm < SCALED_FROM_PPM {
        Band::Low
    } else if ppm >= IMMEDIATE_FROM_PPM {
        Band::Immediate
    } else {
        Band::Scaled(frac16(
            ppm - SCALED_FROM_PPM,
            IMMEDIATE_FROM_PPM - SCALED_FROM_PPM,
        ))
    }
}

/// The advice a purge batch is issued with (design 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advice {
    /// `MADV_DONTNEED`: the pages leave RSS and the cgroup's charge now;
    /// the next touch faults in a zero page.
    Eager,
    /// `MADV_FREE`: the pages stay mapped and charged until the kernel
    /// reclaims them; a touch before that keeps the old contents and costs
    /// no fault.
    Lazy,
}

impl Advice {
    /// The advice as the reports and `stats.cgroup.purge_advice` name it.
    pub const fn name(self) -> &'static str {
        match self {
            Advice::Eager => "eager",
            Advice::Lazy => "lazy",
        }
    }

    /// The `madvise` advice a batch is issued with.
    pub const fn madv(self) -> i32 {
        match self {
            Advice::Eager => crate::sys::MADV_DONTNEED,
            Advice::Lazy => crate::sys::MADV_FREE,
        }
    }

    /// [`Advice::madv`] by name, for the reports.
    pub const fn madv_name(self) -> &'static str {
        match self {
            Advice::Eager => "MADV_DONTNEED",
            Advice::Lazy => "MADV_FREE",
        }
    }
}

/// The advice for a purge (the module doc): an explicit purge is eager,
/// then `headroom` (the
/// `Low` band, under 70% of a known limit, with no eager return of the
/// excess in progress: [`Policy::headroom`]) is lazy when `lazy_band`
/// allows it, and everything else, no limit included, is eager.
pub fn advice(headroom: bool, lazy_band: bool, explicit: bool) -> Advice {
    if explicit {
        Advice::Eager
    } else if lazy_band && headroom {
        Advice::Lazy
    } else {
        Advice::Eager
    }
}

/// The decay period a band gives a configured period `base` (nanoseconds,
/// [`NEVER`] for never). `NEVER` stays `NEVER`; in the scaled band a
/// period above [`FLOOR_NS`] shrinks linearly to the floor at 90%, a
/// shorter one is kept; the immediate band gives 0.
pub fn effective_decay_ns(base: u64, band: Band) -> u64 {
    if base == NEVER {
        return NEVER;
    }
    match band {
        Band::Unlimited | Band::Low => base,
        Band::Scaled(f) => {
            if base <= FLOOR_NS {
                base
            } else {
                base - mul16(base - FLOOR_NS, f)
            }
        }
        Band::Immediate => 0,
    }
}

/// Bytes of `Empty` memory the process may retain under `limit`:
/// [`UNLIMITED`] without a limit, else 5% of it.
pub fn retention_cap(limit: u64) -> u64 {
    if limit == UNLIMITED {
        UNLIMITED
    } else {
        limit / RETENTION_DIVISOR
    }
}

/// The share of its `Empty` memory a walk may keep so that `retention`
/// (the process-wide `Empty` bytes) comes down to `cap`: [`KEEP_ALL`] when
/// within the cap or uncapped, else `cap / retention`.
pub fn keep_ratio(cap: u64, retention: u64) -> u32 {
    if cap == UNLIMITED || retention <= cap {
        KEEP_ALL
    } else if cap == 0 {
        0
    } else {
        frac16(cap, retention)
    }
}

/// The policy's outcome as last evaluated, read by the decay driver and
/// the periodic tick. All atomics: written by whichever thread took the
/// sample, read by every slow path that runs periodic work.
pub struct Policy {
    /// Position in the scaled band ([`FRAC_ONE`] units); 0 outside it.
    frac16: AtomicU32,
    /// In the immediate band.
    immediate: AtomicBool,
    /// Keep ratio for the walks ([`KEEP_ALL`] when uncapped).
    keep16: AtomicU32,
    /// The retention cap in bytes ([`UNLIMITED`] without a limit).
    cap: AtomicU64,
    /// `Empty` bytes at the last evaluation (0 without a limit).
    empty_bytes: AtomicU64,
    /// `high_rises` of the cgroup record as last seen; a higher figure at
    /// the next evaluation requests one full purge.
    high_rises_seen: AtomicU64,
    /// A full purge is owed to the next decay pass.
    full_purge_pending: AtomicBool,
    /// Full purges the policy has run.
    full_purges: AtomicU64,
    /// A limit was seen at the last evaluation (so a limit that goes away
    /// resets the outcome once).
    limited: AtomicBool,
    /// An eager return of the excess over the cap is in progress: the
    /// retention went over the cap while usage was at 70% or above, and
    /// has not come back within it since. Keeps the advice eager through
    /// the `Low` band until then (the module doc).
    eager_excess: AtomicBool,
    /// `thp` was forced to `never` at init by a limit under
    /// [`THP_MIN_LIMIT`] (`thp::forced_by_limit`, decided by
    /// `Shared::init`).
    thp_forced: bool,
}

impl Policy {
    /// Defaults; `high_rises` is the cgroup record's count at init, so a
    /// container whose `memory.events` already showed `high` events does
    /// not start with a full purge.
    pub const fn new(high_rises: u64, thp_forced: bool) -> Policy {
        Policy {
            frac16: AtomicU32::new(0),
            immediate: AtomicBool::new(false),
            keep16: AtomicU32::new(KEEP_ALL),
            cap: AtomicU64::new(UNLIMITED),
            empty_bytes: AtomicU64::new(0),
            high_rises_seen: AtomicU64::new(high_rises),
            full_purge_pending: AtomicBool::new(false),
            full_purges: AtomicU64::new(0),
            limited: AtomicBool::new(false),
            eager_excess: AtomicBool::new(false),
            thp_forced,
        }
    }

    /// The band as last evaluated (`Unlimited` when no limit was seen).
    pub fn band(&self) -> Band {
        if self.immediate.load(Ordering::Relaxed) {
            Band::Immediate
        } else if !self.limited.load(Ordering::Relaxed) {
            Band::Unlimited
        } else {
            match self.frac16.load(Ordering::Relaxed) {
                0 => Band::Low,
                f => Band::Scaled(f),
            }
        }
    }

    /// The purge side's "under 70% of a known limit" (the lazy advice,
    /// design 8): the `Low` band, and no eager return of the excess over
    /// the cap in progress.
    pub fn headroom(&self) -> bool {
        self.band() == Band::Low && !self.eager_excess.load(Ordering::Relaxed)
    }

    /// An eager return of the excess over the cap is in progress (for the
    /// report's reason line).
    pub fn eager_excess(&self) -> bool {
        self.eager_excess.load(Ordering::Relaxed)
    }

    /// Above 90%: the periodic tick runs the cache GC every time.
    #[inline]
    pub fn immediate(&self) -> bool {
        self.immediate.load(Ordering::Relaxed)
    }

    /// The period the walks use for an arena configured with `base`.
    #[inline]
    pub fn effective_decay_ns(&self, base: u64) -> u64 {
        if !self.limited.load(Ordering::Relaxed) {
            return base;
        }
        effective_decay_ns(base, self.band())
    }

    #[inline]
    pub fn keep16(&self) -> u32 {
        self.keep16.load(Ordering::Relaxed)
    }

    pub fn cap(&self) -> u64 {
        self.cap.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn empty_bytes(&self) -> u64 {
        self.empty_bytes.load(Ordering::Relaxed)
    }

    pub fn full_purges(&self) -> u64 {
        self.full_purges.load(Ordering::Relaxed)
    }

    pub fn full_purge_pending(&self) -> bool {
        self.full_purge_pending.load(Ordering::Relaxed)
    }

    pub fn thp_forced(&self) -> bool {
        self.thp_forced
    }

    /// Consume the pending full purge, if any, and count it.
    pub fn take_full_purge(&self) -> bool {
        if self.full_purge_pending.swap(false, Ordering::Relaxed) {
            self.full_purges.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Apply `band` and the cap for `limit` given `retention` (the
    /// process-wide `Empty` bytes) and the cgroup's `high_rises` count.
    /// Returns `true` when the decay driver should look again now (the
    /// band changed or a full purge is owed).
    pub fn apply(&self, band: Band, limit: u64, retention: u64, high_rises: u64) -> bool {
        let (frac, immediate) = match band {
            Band::Unlimited | Band::Low => (0, false),
            Band::Scaled(f) => (f, false),
            Band::Immediate => (0, true),
        };
        let limited = band != Band::Unlimited;
        let cap = if limited {
            retention_cap(limit)
        } else {
            UNLIMITED
        };
        let mut changed = self.limited.swap(limited, Ordering::Relaxed) != limited;
        changed |= self.frac16.swap(frac, Ordering::Relaxed) != frac;
        changed |= self.immediate.swap(immediate, Ordering::Relaxed) != immediate;
        self.cap.store(cap, Ordering::Relaxed);
        self.empty_bytes
            .store(if limited { retention } else { 0 }, Ordering::Relaxed);
        self.keep16.store(
            if limited {
                keep_ratio(cap, retention)
            } else {
                KEEP_ALL
            },
            Ordering::Relaxed,
        );
        // The excess over the cap is returned with the advice of the band
        // it was first seen in: eager from 70% up, and then eager until the
        // retention is back within the cap, whatever the first eager walks
        // did to usage (the module doc). Within the cap the band decides.
        if !limited || retention <= cap {
            self.eager_excess.store(false, Ordering::Relaxed);
        } else if band != Band::Low {
            self.eager_excess.store(true, Ordering::Relaxed);
        }
        let seen = self.high_rises_seen.swap(high_rises, Ordering::Relaxed);
        if high_rises > seen {
            self.full_purge_pending.store(true, Ordering::Relaxed);
            changed = true;
        }
        changed
    }
}

/// `Empty` bytes held by every class of every arena and by the huge
/// allocator: one lock hold per class that has an `Empty` slab (the
/// lock-free `oldest_empty` says which), one for the huge allocator.
pub fn empty_bytes(shared: &Shared) -> u64 {
    let mut total = 0u64;
    for arena in shared.arenas.iter() {
        total += arena.empty_bytes() as u64;
    }
    total + shared.huge.empty_bytes() as u64
}

/// Evaluate the table against the cgroup record after a sample. Without a
/// limit this is a few loads; with one it sums the `Empty` bytes and
/// publishes the outcome on `shared.policy`, waking a background thread
/// when the band changed or a full purge is owed.
pub fn evaluate(shared: &Shared) {
    let cg = &shared.cgroup;
    let p = &shared.policy;
    let limit = cg.limit().min(cg.high());
    let b = band(limit, cg.current());
    if b == Band::Unlimited {
        // A limit that went away (`docker update`) resets the outcome once;
        // otherwise nothing to do and nothing to read.
        crate::purge::set_headroom(false, false);
        if p.limited.load(Ordering::Relaxed) && p.apply(b, UNLIMITED, 0, cg.rises().0) {
            crate::purge::note_decay_changed();
        }
        return;
    }
    let retention = empty_bytes(shared);
    let changed = p.apply(b, limit, retention, cg.rises().0);
    // The purge side's mirror of "under 70% of a known limit", read at
    // each batch flush for the advice (the module doc): the band as just
    // applied, less an eager return of the excess in progress.
    crate::purge::set_headroom(p.headroom(), p.eager_excess());
    if changed {
        crate::purge::note_decay_changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;
    const GIB: u64 = 1 << 30;
    /// 500 MiB: divisible by 10 and 20, so the band edges are exact.
    const L: u64 = 500 << 20;
    const BASE: u64 = 10_000 * MS;

    #[test]
    fn frac16_and_mul16_agree_with_the_arithmetic() {
        assert_eq!(frac16(0, 10), 0);
        assert_eq!(frac16(5, 10), FRAC_ONE / 2);
        assert_eq!(frac16(10, 10), FRAC_ONE);
        assert_eq!(frac16(1, 4), FRAC_ONE / 4);
        // Large denominators are scaled down, not overflowed.
        let f = frac16(u64::MAX / 2, u64::MAX - 1);
        assert!((f as i64 - (FRAC_ONE / 2) as i64).abs() <= 1, "{f}");
        assert_eq!(frac16(u64::MAX - 1, u64::MAX - 1), FRAC_ONE);
        assert_eq!(mul16(1000, FRAC_ONE), 1000);
        assert_eq!(mul16(1000, FRAC_ONE / 2), 500);
        assert_eq!(mul16(0, FRAC_ONE), 0);
        assert_eq!(mul16(u64::MAX, FRAC_ONE), u64::MAX);
        assert_eq!(mul16(u64::MAX, FRAC_ONE / 2), u64::MAX / 2);
    }

    #[test]
    fn usage_ppm_saturates_and_scales() {
        assert_eq!(usage_ppm(L, 0), 0);
        assert_eq!(usage_ppm(L, L / 2), 500_000);
        assert_eq!(usage_ppm(L, L), 1_000_000);
        assert_eq!(usage_ppm(L, L + 1), 1_000_000);
        assert_eq!(usage_ppm(100, 70), 700_000);
        let p = usage_ppm(u64::MAX - 1, (u64::MAX - 1) / 4);
        assert!((p as i64 - 250_000).abs() <= 1, "{p}");
    }

    #[test]
    fn bands_at_the_edges() {
        assert_eq!(band(UNLIMITED, 0), Band::Unlimited);
        assert_eq!(band(UNLIMITED, u64::MAX), Band::Unlimited);
        assert_eq!(band(0, 0), Band::Unlimited);
        assert_eq!(band(L, 0), Band::Low);
        assert_eq!(band(L, L * 7 / 10 - 1), Band::Low);
        assert_eq!(band(L, L * 7 / 10), Band::Scaled(0));
        assert_eq!(band(L, L * 8 / 10), Band::Scaled(FRAC_ONE / 2));
        // The last byte below 90% is the top of the scaled band.
        match band(L, L * 9 / 10 - 1) {
            Band::Scaled(f) => assert!(f >= FRAC_ONE - 2 && f < FRAC_ONE, "{f}"),
            b => panic!("{b:?}"),
        }
        assert_eq!(band(L, L * 9 / 10), Band::Immediate);
        assert_eq!(band(L, L), Band::Immediate);
        assert_eq!(band(L, 2 * L), Band::Immediate);
        // `memory.high` below `memory.max`: the caller passes the minimum.
        assert_eq!(
            band((256 << 20).min(L), 200 << 20),
            Band::Scaled(frac16(200 * 1_000_000 / 256 - 700_000, 200_000))
        );
    }

    #[test]
    fn effective_period_follows_the_table() {
        // No limit and under 70%: the configured period.
        assert_eq!(effective_decay_ns(BASE, Band::Unlimited), BASE);
        assert_eq!(effective_decay_ns(BASE, Band::Low), BASE);
        // 70%: unchanged; 80%: halfway to 1000 ms; just under 90%: 1000 ms.
        assert_eq!(effective_decay_ns(BASE, Band::Scaled(0)), BASE);
        assert_eq!(effective_decay_ns(BASE, band(L, L * 8 / 10)), 5_500 * MS);
        assert_eq!(effective_decay_ns(BASE, Band::Scaled(FRAC_ONE)), FLOOR_NS);
        let top = effective_decay_ns(BASE, band(L, L * 9 / 10 - 1));
        assert!(top >= FLOOR_NS && top < FLOOR_NS + MS, "{top}");
        // 90% and above: 0.
        assert_eq!(effective_decay_ns(BASE, Band::Immediate), 0);
        // A period at or under the floor is not scaled up or down.
        assert_eq!(
            effective_decay_ns(FLOOR_NS, Band::Scaled(FRAC_ONE / 2)),
            FLOOR_NS
        );
        assert_eq!(
            effective_decay_ns(200 * MS, Band::Scaled(FRAC_ONE)),
            200 * MS
        );
        assert_eq!(effective_decay_ns(200 * MS, Band::Immediate), 0);
        // Never stays never in every band.
        for b in [
            Band::Unlimited,
            Band::Low,
            Band::Scaled(FRAC_ONE / 2),
            Band::Immediate,
        ] {
            assert_eq!(effective_decay_ns(NEVER, b), NEVER);
        }
        // Scaling is monotonic in usage.
        let mut last = BASE;
        for pct in 70..90 {
            let e = effective_decay_ns(BASE, band(L, L * pct / 100));
            assert!(e <= last, "{pct}%: {e} > {last}");
            last = e;
        }
    }

    #[test]
    fn retention_cap_and_keep_ratio() {
        assert_eq!(retention_cap(UNLIMITED), UNLIMITED);
        assert_eq!(retention_cap(L), 25_600 * 1024);
        assert_eq!(retention_cap(GIB), GIB / 20);
        let cap = retention_cap(L);
        // Within the cap: keep everything.
        assert_eq!(keep_ratio(cap, 0), KEEP_ALL);
        assert_eq!(keep_ratio(cap, cap), KEEP_ALL);
        assert_eq!(keep_ratio(UNLIMITED, u64::MAX), KEEP_ALL);
        // Over it: the ratio brings the sum down to the cap.
        assert_eq!(keep_ratio(cap, 2 * cap), FRAC_ONE / 2);
        assert_eq!(keep_ratio(cap, 4 * cap), FRAC_ONE / 4);
        let e = 400 << 20;
        let k = keep_ratio(cap, e);
        let kept = mul16(e, k);
        assert!(
            kept <= cap && kept + (e >> 16) + 1 >= cap,
            "kept {kept} of cap {cap}"
        );
        // Shares sum to at most the cap however the retention is split.
        let parts = [64u64 << 20, 200 << 20, 136 << 20];
        let sum: u64 = parts.iter().map(|&p| mul16(p, k)).sum();
        assert!(sum <= cap, "{sum} > {cap}");
        assert_eq!(keep_ratio(0, 1), 0);
    }

    #[test]
    fn policy_apply_publishes_the_outcome_and_the_full_purge() {
        let p = Policy::new(3, false);
        assert_eq!(p.band(), Band::Unlimited);
        assert_eq!(p.effective_decay_ns(BASE), BASE);
        assert_eq!(p.keep16(), KEEP_ALL);
        assert!(!p.immediate());
        // Under 70%, retention within the cap.
        assert!(p.apply(band(L, L / 2), L, 1 << 20, 3));
        assert_eq!(p.band(), Band::Low);
        assert_eq!(p.cap(), L / 20);
        assert_eq!(p.keep16(), KEEP_ALL);
        assert_eq!(p.empty_bytes(), 1 << 20);
        assert_eq!(p.effective_decay_ns(BASE), BASE);
        // Same band again: nothing changed.
        assert!(!p.apply(band(L, L / 2), L, 1 << 20, 3));
        // Over the cap: the keep ratio drops.
        assert!(!p.apply(band(L, L / 2), L, L / 10, 3));
        assert_eq!(p.keep16(), FRAC_ONE / 2);
        // Into the scaled band: the period shrinks.
        assert!(p.apply(band(L, L * 8 / 10), L, 0, 3));
        assert_eq!(p.effective_decay_ns(BASE), 5_500 * MS);
        assert_eq!(p.effective_decay_ns(NEVER), NEVER);
        assert!(!p.immediate());
        // Immediate: period 0, GC flag on.
        assert!(p.apply(band(L, L * 95 / 100), L, 0, 3));
        assert!(p.immediate());
        assert_eq!(p.band(), Band::Immediate);
        assert_eq!(p.effective_decay_ns(BASE), 0);
        assert_eq!(p.effective_decay_ns(NEVER), NEVER);
        // A `high` rise requests one full purge, taken once.
        assert!(!p.full_purge_pending());
        assert!(p.apply(band(L, L * 95 / 100), L, 0, 4));
        assert!(p.full_purge_pending());
        assert!(p.take_full_purge());
        assert!(!p.take_full_purge());
        assert_eq!(p.full_purges(), 1);
        // The same count again does not.
        assert!(!p.apply(band(L, L * 95 / 100), L, 0, 4));
        assert!(!p.full_purge_pending());
        // The limit goes away: back to the defaults.
        assert!(p.apply(band(UNLIMITED, 0), UNLIMITED, 0, 4));
        assert_eq!(p.band(), Band::Unlimited);
        assert_eq!(p.cap(), UNLIMITED);
        assert_eq!(p.keep16(), KEEP_ALL);
        assert_eq!(p.effective_decay_ns(BASE), BASE);
        assert!(!p.immediate());
        assert!(!p.thp_forced());
        assert!(Policy::new(0, true).thp_forced());
    }

    /// The advice at the band edges (the headroom is the `Low` band): lazy
    /// only under 70% of a known limit with `lazy_band` on and no
    /// explicit purge in play; no limit eager.
    #[test]
    fn advice_at_the_band_edges() {
        use Advice::{Eager, Lazy};
        let advice = |b: Band, lazy_band, explicit| advice(b == Band::Low, lazy_band, explicit);
        let defaults = |b| advice(b, true, false);
        // No limit: eager (the headroom is unknown).
        assert_eq!(defaults(Band::Unlimited), Eager);
        assert_eq!(defaults(band(UNLIMITED, 0)), Eager);
        assert_eq!(defaults(band(0, 0)), Eager);
        // Under 70%: lazy, up to the last byte below the band.
        assert_eq!(defaults(band(L, 0)), Lazy);
        assert_eq!(defaults(band(L, L / 2)), Lazy);
        assert_eq!(defaults(band(L, L * 7 / 10 - 1)), Lazy);
        // 70% and above: eager.
        assert_eq!(defaults(band(L, L * 7 / 10)), Eager);
        assert_eq!(defaults(Band::Scaled(0)), Eager);
        assert_eq!(defaults(band(L, L * 8 / 10)), Eager);
        assert_eq!(defaults(band(L, L * 9 / 10 - 1)), Eager);
        assert_eq!(defaults(band(L, L * 9 / 10)), Eager);
        assert_eq!(defaults(Band::Immediate), Eager);
        assert_eq!(defaults(band(L, 2 * L)), Eager);
        // `memory.high` below `memory.max`: the minimum decides.
        assert_eq!(defaults(band((256 << 20).min(L), 100 << 20)), Lazy);
        assert_eq!(defaults(band((256 << 20).min(L), 200 << 20)), Eager);
        for b in [
            Band::Unlimited,
            Band::Low,
            Band::Scaled(FRAC_ONE / 2),
            Band::Immediate,
        ] {
            // The band saving off: eager everywhere.
            assert_eq!(advice(b, false, false), Eager, "{b:?}");
            // An explicit purge is eager in every band.
            assert_eq!(advice(b, true, true), Eager, "{b:?}");
        }
    }

    /// The floor fix (the module doc): an excess over the
    /// cap first seen at 70% or above is returned eagerly until the
    /// retention is back within the cap, through the `Low` band the first
    /// eager walks bring usage into; an excess first seen under 70% is
    /// lazy; within the cap the band alone decides.
    #[test]
    fn eager_return_of_the_excess_holds_until_within_the_cap() {
        let p = Policy::new(0, false);
        let cap = retention_cap(L);
        assert!(!p.headroom(), "no limit: no headroom");
        // The churn: 400 MiB freed at 79% (the scaled band), the first
        // walk returns 100 MiB and usage drops to 59%.
        p.apply(band(L, L * 79 / 100), L, 400 << 20, 0);
        assert!(!p.headroom() && p.eager_excess());
        p.apply(band(L, L * 59 / 100), L, 300 << 20, 0);
        assert_eq!(p.band(), Band::Low);
        assert!(!p.headroom(), "the rest of the excess is still eager");
        assert!(p.eager_excess());
        p.apply(band(L, L * 2 / 100), L, cap - 1, 0);
        assert!(p.headroom(), "within the cap the Low band is lazy");
        assert!(!p.eager_excess());
        // The lazy test: 200 MiB freed at 39%, over the cap, never above
        // 70%: lazy.
        p.apply(band(L, L * 39 / 100), L, 200 << 20, 0);
        assert!(p.headroom() && !p.eager_excess());
        // Usage rising into the scaled band mid-episode turns it eager at
        // once, and it stays eager back under 70% until within the cap.
        p.apply(band(L, L * 75 / 100), L, 150 << 20, 0);
        assert!(!p.headroom() && p.eager_excess());
        p.apply(band(L, L * 50 / 100), L, 100 << 20, 0);
        assert!(!p.headroom());
        p.apply(band(L, L * 50 / 100), L, cap, 0);
        assert!(p.headroom());
        // Immediate: eager, and the flag set for the way down.
        p.apply(band(L, L * 95 / 100), L, 100 << 20, 0);
        assert!(!p.headroom() && p.eager_excess());
        // The limit goes away: nothing lazy, the flag cleared.
        p.apply(band(UNLIMITED, 0), UNLIMITED, 0, 0);
        assert!(!p.headroom() && !p.eager_excess());
        // A retention over the cap first seen in the Low band, cleared
        // when it is within the cap again, then the scaled band with a
        // small retention: within the cap, eager (the band), no episode.
        p.apply(band(L, L * 30 / 100), L, 100 << 20, 0);
        assert!(p.headroom());
        p.apply(band(L, L * 30 / 100), L, 0, 0);
        assert!(p.headroom() && !p.eager_excess());
        p.apply(band(L, L * 80 / 100), L, cap / 2, 0);
        assert!(!p.headroom() && !p.eager_excess());
        p.apply(band(L, L * 60 / 100), L, cap / 2, 0);
        assert!(p.headroom());
    }

    /// The two names of each advice are the report's words, and the
    /// `madvise` flag is the one the kernel knows by that name.
    #[test]
    fn advice_names_and_flags() {
        assert_eq!(Advice::Lazy.name(), "lazy");
        assert_eq!(Advice::Eager.name(), "eager");
        assert_eq!(Advice::Lazy.madv(), crate::sys::MADV_FREE);
        assert_eq!(Advice::Eager.madv(), crate::sys::MADV_DONTNEED);
        assert_eq!(
            (Advice::Lazy.madv_name(), Advice::Lazy.madv()),
            ("MADV_FREE", 8)
        );
        assert_eq!(
            (Advice::Eager.madv_name(), Advice::Eager.madv()),
            ("MADV_DONTNEED", 4)
        );
    }

    /// The forced-`never` rule (`thp::forced_by_limit`): `always` under 1
    /// GiB and nothing else; the test constants sit on either side.
    #[test]
    fn thp_threshold() {
        use crate::config::Thp;
        use crate::thp::forced_by_limit;
        assert!(L < THP_MIN_LIMIT);
        assert!(GIB >= THP_MIN_LIMIT);
        assert!(UNLIMITED >= THP_MIN_LIMIT);
        assert!(forced_by_limit(Thp::Always, L));
        assert!(forced_by_limit(Thp::Always, THP_MIN_LIMIT - 1));
        assert!(!forced_by_limit(Thp::Always, THP_MIN_LIMIT));
        assert!(!forced_by_limit(Thp::Always, GIB));
        assert!(!forced_by_limit(Thp::Always, UNLIMITED));
        for key in [Thp::Auto, Thp::Never] {
            assert!(!forced_by_limit(key, L), "{key:?} is never forced");
            assert!(!forced_by_limit(key, 0), "{key:?} is never forced");
        }
    }
}
