//! The two once-cells, copies: the feature line (`src/api/mod.rs`,
//! `LINE_STATE`: `LINE_UNBUILT` to `LINE_BUILDING` to `LINE_READY`;
//! `LINE_STATE`, `FEATURES` and `FEATURE_LINE` are statics built in
//! `const` context, which loom cannot construct) and the deferred cgroup
//! record (`src/sys/cgroup.rs`, `Deferred`, `PENDING` to `RUNNING` to
//! `DONE`, through `vspec/byte.rs`'s wrappers, each the bare `core`
//! operation with the caller's ordering). Each step names the code it
//! mirrors:
//!
//! - [`Line::line_load`], [`Line::line_compare_exchange`] and
//!   [`Line::line_store`] are `api::line_load`, `api::line_compare_exchange`
//!   and `api::line_store` (the bare `core` calls on `LINE_STATE`);
//! - [`Line::line_ready`] is `api::line_ready`: an `Acquire` load
//!   (`load_line`), `LINE_READY` returns; else the exchange from
//!   `LINE_UNBUILT` to `LINE_BUILDING`, `Acquire` both ways
//!   (`claim_line`), whose winner builds (`build_line`: completes the
//!   features, writes the line) and stores `LINE_READY` `Release`
//!   (`publish_line`); a loser spins on an `Acquire` load with
//!   `spin_loop` until `LINE_READY`; every caller then reads the line
//!   (`published_line`);
//! - [`Line::fork_child`] is `api::feature_line_fork_child`: the
//!   `Relaxed` exchange from `LINE_BUILDING` back to `LINE_UNBUILT`;
//! - [`Deferred::get`] is `Deferred::get` (`load_phase`, an `Acquire`
//!   `vspec::byte::load`, then `cell_ref` on `DONE`);
//!   [`Deferred::discover`] is `Deferred::discover`: the exchange from
//!   `PENDING` to `RUNNING`, `Acquire` on success and `Relaxed` on
//!   failure (`claim`, `vspec::byte::compare_exchange`), whose winner
//!   builds the record in the cell (`cell_discover`), calls `found` and
//!   stores `DONE` `Release` (`publish`, `vspec::byte::store`); a loser
//!   that waits spins on `load_phase` until `DONE`; [`Deferred::published`] is
//!   `Deferred::published`, the cell read through the permission a
//!   waiting `discover` hands back (`Shared::cgroup_now`), with no second
//!   load; [`Deferred::fork_child`] is
//!   `Deferred::fork_child`: the `Relaxed` exchange from `RUNNING` back
//!   to `PENDING`.
//!
//! Checked: the build (the discovery) runs once; every caller that reads
//! the line (the record) reads the value the winner wrote, with no race
//! (a read not ordered after the write is a reported causality
//! violation); every waiter returns (a waiter left spinning fails the model
//! at loom's branch limit); a child's reset makes the next request build
//! (discover) again rather than wait, and leaves `LINE_READY` (`DONE`) alone.
//!
//! Left out: a fork child's handler in the fork window
//! (`lock::in_child_fork_window`), whose wait resets the work and builds
//! (discovers) itself rather than spin: the child has one thread there,
//! so that step is the child reset below followed by a request.
//!
//! Also left out: what the build and the discovery compute (the probe, the
//! formatting, the files; verified, and they touch neither state byte),
//! here one plain write of a fixed value per cell; `found`'s policy
//! update (a `Relaxed` store elsewhere); `Deferred::due`'s `Relaxed`
//! load, whose result only decides whether to try the exchange; the
//! fork itself (the reset models run in one thread, as the child has
//! one).

use crate::{leak, model};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use loom::thread;

const LINE_UNBUILT: u8 = 0;
const LINE_BUILDING: u8 = 1;
const LINE_READY: u8 = 2;

const PHASE_DONE: u8 = 0;
const PHASE_PENDING: u8 = 1;
const PHASE_RUNNING: u8 = 2;

/// The completed features `build_line` writes.
const FEATURES: u64 = 0xfea7;
/// The line `build_line` writes.
const BUILT: u64 = 0x11e5;
/// The record discovery writes.
const RECORD: u64 = 0xc6;

/// The feature line's statics.
struct Line {
    /// `api::LINE_STATE`.
    state: AtomicU8,
    /// `api::FEATURES` (the THP table the build completes).
    features: UnsafeCell<u64>,
    /// `api::FEATURE_LINE`.
    line: UnsafeCell<u64>,
    /// The ordering of the `LINE_READY` store (`Release` as shipped).
    publish: Ordering,
    /// How many builds ran (model bookkeeping, not in the source).
    builds: AtomicU32,
}

// SAFETY: the cells are written by the exchange's winner and read after
// `LINE_READY`, which is what is checked.
unsafe impl Sync for Line {}

impl Line {
    fn new(publish: Ordering) -> Line {
        Line {
            state: AtomicU8::new(LINE_UNBUILT),
            features: UnsafeCell::new(0),
            line: UnsafeCell::new(0),
            publish,
            builds: AtomicU32::new(0),
        }
    }

    /// `api::line_load`.
    fn line_load(&self, ordering: Ordering) -> u8 {
        self.state.load(ordering)
    }

    /// `api::line_compare_exchange`.
    fn line_compare_exchange(
        &self,
        current: u8,
        new: u8,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u8, u8> {
        self.state.compare_exchange(current, new, success, failure)
    }

    /// `api::line_store`.
    fn line_store(&self, v: u8, ordering: Ordering) {
        self.state.store(v, ordering)
    }

    /// `api::build_line`: complete the features, write the line.
    fn build_line(&self) {
        self.builds.fetch_add(1, Ordering::Relaxed);
        // SAFETY: only the exchange's winner builds, before `LINE_READY`
        // (loom reports a race if not).
        self.features.with_mut(|p| unsafe { *p = FEATURES });
        self.line.with_mut(|p| unsafe { *p = BUILT });
    }

    /// `api::published_line`: the plain read of the line.
    fn published_line(&self) -> u64 {
        // SAFETY: `LINE_READY` observed by an `Acquire` load on this
        // thread, or published by it (loom reports a race if not).
        self.line.with(|p| unsafe { *p })
    }

    /// `api::line_ready`, then `published_line`.
    fn line_ready(&self) -> u64 {
        if self.line_load(Ordering::Acquire) == LINE_READY {
            return self.published_line();
        }
        let won = self.line_compare_exchange(
            LINE_UNBUILT,
            LINE_BUILDING,
            Ordering::Acquire,
            Ordering::Acquire,
        );
        if won.is_ok() {
            self.build_line();
            self.line_store(LINE_READY, self.publish);
            return self.published_line();
        }
        while self.line_load(Ordering::Acquire) != LINE_READY {
            loom::hint::spin_loop();
        }
        self.published_line()
    }

    /// `api::feature_line_fork_child`.
    fn fork_child(&self) {
        let _ = self.line_compare_exchange(
            LINE_BUILDING,
            LINE_UNBUILT,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

/// Two threads request the line while the main thread reads it if it is
/// ready (a third request that only takes the fast path).
fn run_line(publish: Ordering) {
    model(move || {
        let l = leak(Line::new(publish));
        let hs: Vec<_> = (0..2)
            .map(|_| thread::spawn(move || l.line_ready()))
            .collect();
        if l.line_load(Ordering::Acquire) == LINE_READY {
            assert_eq!(l.published_line(), BUILT, "a ready reader saw another line");
        }
        for h in hs {
            assert_eq!(
                h.join().unwrap(),
                BUILT,
                "a request did not get the built line"
            );
        }
        assert_eq!(
            l.builds.load(Ordering::Relaxed),
            1,
            "the line was built more than once"
        );
    });
}

/// Two requests race: one builds, the other returns the line it built
/// (on `LINE_READY`, or after spinning on `LINE_BUILDING`).
#[test]
fn loom_oncecell_line_two_requests() {
    run_line(Ordering::Release);
}

/// The publication check has teeth: a `Relaxed` `LINE_READY` store lets a
/// reader see `LINE_READY` without the build's write.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_oncecell_line_relaxed_publish_is_a_race() {
    run_line(Ordering::Relaxed);
}

/// The child of a fork whose other thread was building the line (the
/// state left at `LINE_BUILDING`, the line unwritten): the reset makes
/// the child's request build it, not wait for ever; a reset of a ready
/// line leaves it ready and built once.
#[test]
fn loom_oncecell_line_fork_child_rebuilds() {
    model(|| {
        let l = leak(Line::new(Ordering::Release));
        l.state.store(LINE_BUILDING, Ordering::Relaxed);
        l.fork_child();
        assert_eq!(
            l.line_load(Ordering::Acquire),
            LINE_UNBUILT,
            "the reset left the build pending"
        );
        assert_eq!(
            l.line_ready(),
            BUILT,
            "the child's request did not build the line"
        );
        l.fork_child();
        assert_eq!(
            l.line_load(Ordering::Acquire),
            LINE_READY,
            "the reset undid a ready line"
        );
        assert_eq!(l.line_ready(), BUILT);
        assert_eq!(
            l.builds.load(Ordering::Relaxed),
            1,
            "a ready line was built again"
        );
    });
}

/// The deferred cgroup record.
struct Deferred {
    /// `Deferred::phase`.
    phase: AtomicU8,
    /// `Deferred::cell`.
    cell: UnsafeCell<u64>,
    /// The ordering of the `DONE` store (`Release` as shipped).
    publish: Ordering,
    /// How many discoveries ran (model bookkeeping, not in the source).
    discoveries: AtomicU32,
}

// SAFETY: the cell is written by the exchange's winner and read after
// `DONE`, which is what is checked.
unsafe impl Sync for Deferred {}

impl Deferred {
    /// `Deferred::later`.
    fn later(publish: Ordering) -> Deferred {
        Deferred {
            phase: AtomicU8::new(PHASE_PENDING),
            cell: UnsafeCell::new(0),
            publish,
            discoveries: AtomicU32::new(0),
        }
    }

    /// `Deferred::get`: `load_phase`, then `cell_ref` on `DONE`.
    fn get(&self) -> Option<u64> {
        if self.phase.load(Ordering::Acquire) == PHASE_DONE {
            // SAFETY: `DONE` observed by an `Acquire` load on this thread
            // (loom reports a race if not).
            Some(self.cell.with(|p| unsafe { *p }))
        } else {
            None
        }
    }

    /// `Deferred::discover`, `found` left out.
    fn discover(&self, wait: bool) -> bool {
        match self.phase.compare_exchange(
            PHASE_PENDING,
            PHASE_RUNNING,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                self.discoveries.fetch_add(1, Ordering::Relaxed);
                // SAFETY: only the exchange's winner writes, before `DONE`
                // (loom reports a race if not).
                self.cell.with_mut(|p| unsafe { *p = RECORD });
                self.phase.store(PHASE_DONE, self.publish);
                true
            }
            Err(_) => {
                while wait && self.phase.load(Ordering::Acquire) != PHASE_DONE {
                    loom::hint::spin_loop();
                }
                false
            }
        }
    }

    /// `Deferred::published`: after a waiting `discover`, the cell read
    /// with no second load of the phase (`Shared::cgroup_now`).
    fn published(&self) -> u64 {
        // SAFETY: `DONE` observed by this thread's `Acquire` load in
        // `discover`, or written by it (loom reports a race if not).
        self.cell.with(|p| unsafe { *p })
    }

    /// `Deferred::fork_child`.
    fn fork_child(&self) {
        let _ = self.phase.compare_exchange(
            PHASE_RUNNING,
            PHASE_PENDING,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

/// A tick discovers without waiting while a reader of the figures
/// discovers and waits, and the main thread samples (`get`).
fn run_deferred(publish: Ordering) {
    model(move || {
        let d = leak(Deferred::later(publish));
        let tick = thread::spawn(move || d.discover(false));
        let reader = thread::spawn(move || {
            let ran = d.discover(true);
            assert_eq!(
                d.published(),
                RECORD,
                "a waiting reader read another record"
            );
            (ran, d.get())
        });
        if let Some(r) = d.get() {
            assert_eq!(r, RECORD, "a sampler saw another record");
        }
        let ran_tick = tick.join().unwrap();
        let (ran_reader, seen) = reader.join().unwrap();
        assert!(ran_tick != ran_reader, "not exactly one discovery ran");
        assert_eq!(
            seen,
            Some(RECORD),
            "a waiting reader did not see the record"
        );
        assert_eq!(d.get(), Some(RECORD));
        assert_eq!(
            d.discoveries.load(Ordering::Relaxed),
            1,
            "discovery ran more than once"
        );
    });
}

/// A tick and a waiting reader race to discover, with a sampler: one
/// discovers, the waiting reader returns with the record published, and
/// the sampler sees nothing or the record.
#[test]
fn loom_oncecell_deferred_discover_races_get() {
    run_deferred(Ordering::Release);
}

/// The publication check has teeth: a `Relaxed` `DONE` store lets a
/// reader see `DONE` without the discovery's write.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_oncecell_deferred_relaxed_publish_is_a_race() {
    run_deferred(Ordering::Relaxed);
}

/// The child of a fork whose other thread was discovering (the phase left
/// at `RUNNING`): the reset makes the child's waiting discovery run
/// rather than spin for ever; a reset of a published record leaves it.
#[test]
fn loom_oncecell_deferred_fork_child_restarts() {
    model(|| {
        let d = leak(Deferred::later(Ordering::Release));
        d.phase.store(PHASE_RUNNING, Ordering::Relaxed);
        d.fork_child();
        assert_eq!(d.get(), None);
        assert!(d.discover(true), "the child's discovery did not run");
        d.fork_child();
        assert_eq!(d.get(), Some(RECORD), "the reset undid a published record");
        assert!(!d.discover(true));
        assert_eq!(d.discoveries.load(Ordering::Relaxed), 1);
    });
}
