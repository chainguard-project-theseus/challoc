//! Demand-window retention (design.md section 8, "demand floor"): a
//! per-pool record of recent demand peaks that lets a decay walk keep
//! the `Empty` memory that demand has come back for, and the huge tier
//! keep a freed run above `huge_retain_max` when the tier's demand
//! recurs at the height the run brings it to: the tier's live pages just
//! before the free, this run included, are within an eighth of the
//! tier's recurring level (`Huge::free_run`). The test is on the tier's
//! total, not on the run's own size.
//!
//! A pool is one `(arena, class)` (units: slabs) or the huge tier
//! (units: pages, guards included). Its [`Demand`] is a ring of
//! [`EPOCHS`] epochs over the window `hpa_peak_demand_window_ms`, each
//! holding the two largest *committed* peaks seen in it, plus the
//! running maximum since the last commit. A peak is committed when
//! demand falls more than an eighth below the running maximum; from then
//! on the tracker follows the trough, and only a rise of more than an
//! eighth above it starts a new peak. So small dips never commit and a
//! gradual fall commits once, however many samples it spans. [`Demand::level`]
//! is the second-largest of the committed peaks in the window and the
//! running maximum: the level demand has reached at least twice in the
//! window. A one-off peak therefore never raises it, and a recurring one
//! keeps it up until the window passes over its last occurrence.
//!
//! The walks keep [`allowance`]`(curve, level - active, ...)`: the curve's
//! allowance or the demand floor, whichever is larger, then the cgroup
//! policy's retention cap, which always wins.
//!
//! Single writer: every method that writes is called under the owner's
//! lock (the class lock, the huge lock). The fields are relaxed atomics
//! only so that the trackers are `Sync`; there are no read-modify-writes.

use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};

use crate::arena::MAX_ARENAS;
use crate::layout::NCLASSES;

/// Epochs in the window.
pub const EPOCHS: usize = 8;
/// A sample below `run - run >> HYST_SHIFT` commits the running peak.
const HYST_SHIFT: u32 = 3;
/// The longest a walk that found nothing to purge defers the next one
/// ([`Window::hold`]).
pub const HOLD_NS: u64 = 1_000_000_000;
/// Largest `hpa_peak_demand_window_ms`: one day.
pub const MAX_WINDOW_MS: i64 = 86_400_000;

/// The window as the walks use it: its epoch length. `None` (no window)
/// is the key at 0, today's decay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    epoch_ns: u64,
}

impl Window {
    /// `hpa_peak_demand_window_ms` as a window; 0 is none. Values under
    /// [`EPOCHS`] ms round up to it, so an epoch is never 0 ns.
    pub const fn from_ms(ms: u64) -> Option<Window> {
        if ms == 0 {
            return None;
        }
        let ms = if ms < EPOCHS as u64 { EPOCHS as u64 } else { ms };
        Some(Window { epoch_ns: ms.saturating_mul(1_000_000) / EPOCHS as u64 })
    }

    /// A window from its epoch length (0 is none), as the huge tier
    /// stores it.
    #[inline]
    pub const fn from_epoch_ns(epoch_ns: u64) -> Option<Window> {
        if epoch_ns == 0 { None } else { Some(Window { epoch_ns }) }
    }

    #[inline]
    pub const fn epoch_ns(self) -> u64 {
        self.epoch_ns
    }

    /// The epoch `now` falls in.
    #[inline]
    pub fn epoch(self, now: u64) -> u64 {
        now / self.epoch_ns
    }

    /// The deadline of a walk that found everything within the demand
    /// floor: a second on, or the next epoch boundary (where the level may
    /// fall) if that is sooner.
    pub fn hold(self, now: u64) -> u64 {
        let boundary = (self.epoch(now) + 1).saturating_mul(self.epoch_ns);
        now.saturating_add(HOLD_NS).min(boundary)
    }
}

/// One pool's demand record (the module doc).
pub struct Demand {
    /// Absolute epoch number of the newest slot.
    epoch: AtomicU64,
    /// Per epoch slot (`epoch % EPOCHS`): the two largest committed peaks,
    /// `[0] >= [1]`.
    top: [[AtomicU32; 2]; EPOCHS],
    /// Running maximum since the last commit, or while `falling` the
    /// trough since it.
    run: AtomicU32,
    /// A peak was committed and demand has not risen more than an eighth
    /// above the trough since.
    falling: AtomicBool,
}

#[inline]
fn ld(a: &AtomicU32) -> u32 {
    a.load(Ordering::Relaxed)
}

#[inline]
fn st(a: &AtomicU32, v: u32) {
    a.store(v, Ordering::Relaxed)
}

impl Demand {
    pub const fn new() -> Demand {
        Demand {
            epoch: AtomicU64::new(0),
            top: [const { [AtomicU32::new(0), AtomicU32::new(0)] }; EPOCHS],
            run: AtomicU32::new(0),
            falling: AtomicBool::new(false),
        }
    }

    /// Move the ring to epoch `e`: the slots of the epochs passed over
    /// are cleared (all of them after `EPOCHS` or more). An `e` at or
    /// before the newest slot does nothing (the clock is monotonic; a
    /// test may go back).
    fn advance(&self, e: u64) {
        let cur = self.epoch.load(Ordering::Relaxed);
        if e <= cur {
            return;
        }
        let k = (e - cur).min(EPOCHS as u64);
        for i in 1..=k {
            let s = (cur.wrapping_add(i) % EPOCHS as u64) as usize;
            st(&self.top[s][0], 0);
            st(&self.top[s][1], 0);
        }
        self.epoch.store(e, Ordering::Relaxed);
    }

    /// Record demand `d` at epoch `e`: a new running maximum, or a fall of
    /// more than an eighth below it, which commits the maximum into the
    /// epoch's top two. After a commit the tracker follows the trough
    /// until demand rises more than an eighth above it, which starts the
    /// next peak.
    pub fn sample(&self, e: u64, d: usize) {
        self.advance(e);
        let d = d.min(u32::MAX as usize) as u32;
        let run = ld(&self.run);
        if self.falling.load(Ordering::Relaxed) {
            if d < run {
                st(&self.run, d);
            } else if d as u64 > run as u64 + (run >> HYST_SHIFT) as u64 {
                self.falling.store(false, Ordering::Relaxed);
                st(&self.run, d);
            }
            return;
        }
        if d >= run {
            st(&self.run, d);
        } else if d < run - (run >> HYST_SHIFT) {
            let s = (self.epoch.load(Ordering::Relaxed) % EPOCHS as u64) as usize;
            let (t0, t1) = (ld(&self.top[s][0]), ld(&self.top[s][1]));
            if run > t0 {
                st(&self.top[s][1], t0);
                st(&self.top[s][0], run);
            } else if run > t1 {
                st(&self.top[s][1], run);
            }
            st(&self.run, d);
            self.falling.store(true, Ordering::Relaxed);
        }
    }

    /// The recurring level at epoch `e`: the second-largest of the
    /// committed peaks in the window and the running maximum (or the
    /// trough, after a commit).
    pub fn level(&self, e: u64) -> usize {
        self.advance(e);
        let (mut m1, mut m2) = (ld(&self.run), 0u32);
        for slot in &self.top {
            for v in slot {
                let v = ld(v);
                if v > m1 {
                    m2 = m1;
                    m1 = v;
                } else if v > m2 {
                    m2 = v;
                }
            }
        }
        m2 as usize
    }
}

impl Default for Demand {
    fn default() -> Demand {
        Demand::new()
    }
}

/// The trackers of one arena, one per class, carved from the metadata
/// region the first time the decay driver visits the arena with a window
/// on (`purge::arena_demand`) and never freed.
pub struct ArenaDemand {
    pub classes: [Demand; NCLASSES],
}

impl ArenaDemand {
    pub const fn new() -> ArenaDemand {
        ArenaDemand { classes: [const { Demand::new() }; NCLASSES] }
    }
}

/// `Shared::demand`: one tracker set per arena index, carved together
/// with the first set and never freed. A slot is null until its arena is
/// first visited, or [`CARVE_FAILED`] when the metadata region could not
/// hold its trackers (that arena then decays without a floor).
pub struct DemandTable {
    pub arenas: [AtomicPtr<ArenaDemand>; MAX_ARENAS],
}

/// The sentinel of a slot (or of `Shared::demand`) whose carve failed.
pub const CARVE_FAILED: *mut ArenaDemand = 1 as *mut ArenaDemand;

/// What a class walk needs to apply the demand floor: the class's
/// tracker, the window, and whether the cgroup policy lets the floor
/// retain anything (`Policy::demand_ok`; the tracker samples either way).
#[derive(Clone, Copy)]
pub struct DemandGate<'a> {
    pub tracker: &'a Demand,
    pub window: Window,
    pub apply: bool,
}

/// Units a walk may keep: the curve's allowance `curve` or the demand
/// floor `keep_demand`, whichever is larger, then at most `keep16 /
/// 65536` of the `total` units when the cgroup policy caps retention
/// (`keep16 < KEEP_ALL`): the cap always wins. With `keep_demand` 0 this
/// is the curve's allowance under the cap, as before the floor existed.
#[inline]
pub fn allowance(curve: usize, keep_demand: usize, total: usize, keep16: u32) -> usize {
    let mut allow = curve.max(keep_demand);
    if keep16 < crate::policy::KEEP_ALL {
        allow = allow.min(crate::policy::mul16(total as u64, keep16) as usize);
    }
    allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{KEEP_ALL, mul16};

    #[test]
    fn level_is_the_second_peak_of_the_window() {
        let d = Demand::new();
        assert_eq!(d.level(10), 0, "a new tracker");
        // One peak, still running: nothing has recurred.
        d.sample(10, 100);
        assert_eq!(d.level(10), 0);
        // It falls: the committed peak and the running level 40.
        d.sample(10, 40);
        assert_eq!(d.level(10), 40, "one peak: the running level");
        // Falling on to 0 is the same fall: nothing more is committed.
        d.sample(10, 0);
        assert_eq!(d.level(10), 0);
        // A second, smaller peak: the level is the smaller.
        d.sample(11, 80);
        d.sample(11, 0);
        assert_eq!(d.level(11), 80);
        // A third between them: the second-largest.
        d.sample(12, 90);
        d.sample(12, 0);
        assert_eq!(d.level(12), 90);
        // Two commits in one epoch are both kept, the third smaller one
        // drops out of that slot only.
        let d = Demand::new();
        for p in [50, 70, 60] {
            d.sample(3, p);
            d.sample(3, 0);
        }
        assert_eq!(d.level(3), 60);
        // The running maximum counts as an occurrence.
        d.sample(3, 65);
        assert_eq!(d.level(3), 65);
        d.sample(3, 75);
        assert_eq!(d.level(3), 70);
    }

    #[test]
    fn hysteresis_commits_a_gradual_fall_once() {
        let d = Demand::new();
        d.sample(1, 800);
        // Dips of up to an eighth never commit.
        for v in [700, 790, 701, 800, 760] {
            d.sample(1, v);
        }
        assert_eq!(d.level(1), 0, "no commit yet");
        // A gradual fall from 800 to 0, one sample per step of 10 (as a
        // pool freed over many walks): one commit of 800 when it passes
        // 700 and nothing after it, so a one-off peak leaves no level.
        let d = Demand::new();
        d.sample(1, 800);
        let mut v = 800;
        while v > 0 {
            v -= 10;
            d.sample(1, v);
        }
        let s = 1 % EPOCHS;
        assert_eq!(ld(&d.top[s][0]), 800);
        assert_eq!(ld(&d.top[s][1]), 0);
        assert_eq!(d.level(1), 0);
        // A rise past an eighth over the trough starts a peak, and its
        // fall commits the second one.
        d.sample(1, 400);
        d.sample(1, 380);
        d.sample(1, 420);
        for v in [360, 300, 200, 0] {
            d.sample(1, v);
        }
        assert_eq!(d.level(1), 420);
        // A rise within an eighth of the trough is the same fall: 670
        // over a trough of 600 starts no peak, so 500 commits nothing.
        let d = Demand::new();
        for v in [800, 600, 670, 500] {
            d.sample(1, v);
        }
        assert_eq!(d.level(1), 500, "800 committed, the trough running");
    }

    #[test]
    fn peaks_expire_with_their_epochs() {
        let d = Demand::new();
        let base = 1_000;
        d.sample(base, 100);
        d.sample(base, 0);
        d.sample(base + 2, 50);
        d.sample(base + 2, 0);
        assert_eq!(d.level(base + 2), 50);
        // Within the window both stay.
        assert_eq!(d.level(base + EPOCHS as u64 - 1), 50);
        // The oldest slot goes first: 100 expires, 50 is alone.
        assert_eq!(d.level(base + EPOCHS as u64), 0);
        d.sample(base + EPOCHS as u64, 40);
        assert_eq!(d.level(base + EPOCHS as u64), 40, "50 committed, 40 running");
        // Then 50 expires too.
        assert_eq!(d.level(base + 2 + EPOCHS as u64), 0);
        // A backwards epoch changes nothing.
        let d = Demand::new();
        d.sample(500, 30);
        d.sample(500, 0);
        d.sample(500, 20);
        assert_eq!(d.level(400), 20);
        d.sample(400, 0);
        assert_eq!(d.level(500), 20, "the commit went to the current slot");
        // A delta of 2^40 epochs clears the ring.
        d.sample(500 + (1 << 40), 0);
        assert_eq!(d.level(500 + (1 << 40)), 0);
        assert!(d.top.iter().flatten().all(|v| ld(v) == 0));
        // Saturation: a sample beyond u32 is u32::MAX.
        let d = Demand::new();
        d.sample(1, usize::MAX);
        d.sample(1, 0);
        d.sample(1, usize::MAX);
        assert_eq!(d.level(1), u32::MAX as usize);
    }

    #[test]
    fn window_epochs_and_holds() {
        assert_eq!(Window::from_ms(0), None);
        let w = Window::from_ms(120_000).unwrap();
        assert_eq!(w.epoch_ns(), 15_000_000_000);
        assert_eq!(w.epoch(15_000_000_000 * 3 + 1), 3);
        // A hold is a second on, or the next boundary if sooner.
        assert_eq!(w.hold(0), HOLD_NS);
        assert_eq!(w.hold(15_000_000_000 - 10), 15_000_000_000);
        // Tiny windows round up to one millisecond per epoch.
        assert_eq!(Window::from_ms(1).unwrap().epoch_ns(), 1_000_000);
        assert_eq!(Window::from_ms(EPOCHS as u64).unwrap().epoch_ns(), 1_000_000);
        assert_eq!(Window::from_epoch_ns(0), None);
        assert_eq!(Window::from_epoch_ns(w.epoch_ns()), Some(w));
        // The longest window still has a sane epoch and hold at the end
        // of the clock.
        let w = Window::from_ms(MAX_WINDOW_MS as u64).unwrap();
        assert_eq!(w.epoch_ns(), 10_800_000_000_000);
        assert!(w.hold(u64::MAX - 1) >= u64::MAX - 1);
    }

    /// The cap wins over the floor; a zero floor is the old allowance bit
    /// for bit; the floor raises the curve and never lowers it.
    #[test]
    fn allowance_takes_the_floor_and_the_cap_wins() {
        let old = |curve: usize, total: usize, keep16: u32| {
            let mut allow = curve;
            if keep16 < KEEP_ALL {
                allow = allow.min(mul16(total as u64, keep16) as usize);
            }
            allow
        };
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for _ in 0..100_000 {
            let total = (next() % 100_000) as usize;
            let curve = (next() as usize) % (total + 1);
            let keep16 = match next() % 4 {
                0 => KEEP_ALL,
                1 => 0,
                _ => (next() % KEEP_ALL as u64) as u32,
            };
            let floor = (next() as usize) % (2 * total + 1);
            assert_eq!(allowance(curve, 0, total, keep16), old(curve, total, keep16));
            let a = allowance(curve, floor, total, keep16);
            assert!(a >= allowance(curve, 0, total, keep16));
            if keep16 < KEEP_ALL {
                assert!(a <= mul16(total as u64, keep16) as usize, "the cap wins");
            } else {
                assert_eq!(a, curve.max(floor));
            }
        }
        assert_eq!(allowance(10, 50, 100, KEEP_ALL), 50);
        assert_eq!(allowance(10, 50, 100, KEEP_ALL / 4), 25);
        assert_eq!(allowance(10, 5, 100, KEEP_ALL / 4), 10);
    }
}
