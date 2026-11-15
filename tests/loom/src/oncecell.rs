//! The two once-cells, copies: the feature line (`src/api/mod.rs`,
//! `STATE.line`: `LINE_UNBUILT` to `LINE_BUILDING` to `LINE_READY`;
//! `STATE`, `FEATURES` and `FEATURE_LINE` are statics built in
//! `const` context, which loom cannot construct) and the deferred cgroup
//! record (`src/sys/cgroup.rs`, `Deferred`, `PENDING` to `RUNNING` to
//! `DONE`, through `vspec/byte.rs`'s wrappers, which run here as they
//! are, over the `cfg(loom)` stand-in of vstd's `PAtomicU8`, `SeqCst` on
//! loom's `AtomicU8`: `crate::vspec`, `crate::sync::patomic`). Each step
//! names the code it mirrors:
//!
//! - [`Line::line_load`], [`Line::line_compare_exchange`] and
//!   [`Line::line_store`] are the line's `vspec::byte` calls on
//!   `STATE.line` (`load`, `compare_exchange` and `store`, vstd's
//!   `SeqCst` operations), here bare loom calls on the copy's state
//!   byte;
//! - [`Line::line_ready`] is `api::line_ready`: a load (`load_line`),
//!   `LINE_READY` returns; else the exchange from `LINE_UNBUILT` to
//!   `LINE_BUILDING` (`claim_line`), whose winner builds (`build_line`:
//!   completes the features, writes the line) and stores `LINE_READY`
//!   (`publish_line`); a loser spins on a load with `spin_loop` until
//!   `LINE_READY`; every caller then reads the line (`published_line`);
//! - [`Line::fork_child`] is `api::feature_line_fork_child`: the
//!   exchange from `LINE_BUILDING` back to `LINE_UNBUILT`;
//! - [`Deferred::get`] is `Deferred::get` (`load_phase`, a
//!   `vspec::byte::load`, then `cell_ref` on `DONE`);
//!   [`Deferred::discover`] is `Deferred::discover`: the exchange from
//!   `PENDING` to `RUNNING` (`claim`, `vspec::byte::compare_exchange`),
//!   whose winner builds the record in the cell (`cell_discover`), calls
//!   `found` and stores `DONE` (`publish`, `vspec::byte::store`); a loser
//!   that waits spins on `load_phase` until `DONE`; [`Deferred::published`] is
//!   `Deferred::published`, the cell read through the permission a
//!   waiting `discover` hands back (`Shared::cgroup_now`), with no second
//!   load; [`Deferred::fork_child`] is
//!   `Deferred::fork_child`: the exchange from `RUNNING` back to
//!   `PENDING`.
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
//! update (a `Relaxed` store elsewhere); `Deferred::due`'s load (through
//! `load_phase`), whose result only decides whether to try the exchange;
//! the fork itself (the reset models run in one thread, as the child has
//! one).

use crate::sync::patomic::PAtomicU8;
use crate::vspec::byte;
use crate::{leak, model};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use loom::thread;
use vstd::prelude::Tracked;

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
    /// `api::STATE.line`.
    state: AtomicU8,
    /// `api::FEATURES` (the THP table the build completes).
    features: UnsafeCell<u64>,
    /// `api::FEATURE_LINE`.
    line: UnsafeCell<u64>,
    /// The ordering of the `LINE_READY` store (`SeqCst` as shipped).
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

    /// `vspec::byte::load` on `api::STATE.line`.
    fn line_load(&self) -> u8 {
        self.state.load(Ordering::SeqCst)
    }

    /// `vspec::byte::compare_exchange` on `api::STATE.line`.
    fn line_compare_exchange(&self, current: u8, new: u8) -> Result<u8, u8> {
        self.state
            .compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst)
    }

    /// `vspec::byte::store` on `api::STATE.line`, at `ordering` (`SeqCst`
    /// as shipped; the control weakens it).
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
        // SAFETY: `LINE_READY` observed by a `SeqCst` load on this
        // thread, or published by it (loom reports a race if not).
        self.line.with(|p| unsafe { *p })
    }

    /// `api::line_ready`, then `published_line`.
    fn line_ready(&self) -> u64 {
        if self.line_load() == LINE_READY {
            return self.published_line();
        }
        let won = self.line_compare_exchange(LINE_UNBUILT, LINE_BUILDING);
        if won.is_ok() {
            self.build_line();
            self.line_store(LINE_READY, self.publish);
            return self.published_line();
        }
        while self.line_load() != LINE_READY {
            loom::hint::spin_loop();
        }
        self.published_line()
    }

    /// `api::feature_line_fork_child`.
    fn fork_child(&self) {
        let _ = self.line_compare_exchange(LINE_BUILDING, LINE_UNBUILT);
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
        if l.line_load() == LINE_READY {
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
///
/// Covers copies of `api::load_line`, `api::claim_line` and
/// `api::publish_line`.
#[test]
fn loom_oncecell_line_two_requests() {
    run_line(Ordering::SeqCst);
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
///
/// Covers copies of `api::load_line`, `api::claim_line`,
/// `api::publish_line` and `api::feature_line_fork_child`.
#[test]
fn loom_oncecell_line_fork_child_rebuilds() {
    model(|| {
        let l = leak(Line::new(Ordering::SeqCst));
        l.state.store(LINE_BUILDING, Ordering::Relaxed);
        l.fork_child();
        assert_eq!(
            l.line_load(),
            LINE_UNBUILT,
            "the reset left the build pending"
        );
        assert_eq!(
            l.line_ready(),
            BUILT,
            "the child's request did not build the line"
        );
        l.fork_child();
        assert_eq!(l.line_load(), LINE_READY, "the reset undid a ready line");
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
    phase: PAtomicU8,
    /// `Deferred::cell`.
    cell: UnsafeCell<u64>,
    /// The control: the winner stores `DONE` before it writes the cell
    /// (`false` as shipped).
    early: bool,
    /// How many discoveries ran (model bookkeeping, not in the source).
    discoveries: AtomicU32,
}

// SAFETY: the cell is written by the exchange's winner and read after
// `DONE`, which is what is checked.
unsafe impl Sync for Deferred {}

impl Deferred {
    /// `Deferred::later`.
    fn later(early: bool) -> Deferred {
        Deferred {
            phase: PAtomicU8::new(PHASE_PENDING).0,
            cell: UnsafeCell::new(0),
            early,
            discoveries: AtomicU32::new(0),
        }
    }

    /// `Deferred::get`: `load_phase`, then `cell_ref` on `DONE`.
    fn get(&self) -> Option<u64> {
        if byte::load(&self.phase, Tracked::assume_new()) == PHASE_DONE {
            // SAFETY: `DONE` observed by a `SeqCst` load on this thread
            // (loom reports a race if not).
            Some(self.cell.with(|p| unsafe { *p }))
        } else {
            None
        }
    }

    /// `Deferred::discover`, `found` left out.
    fn discover(&self, wait: bool) -> bool {
        match byte::compare_exchange(
            &self.phase,
            PHASE_PENDING,
            PHASE_RUNNING,
            Tracked::assume_new(),
        ) {
            Ok(_) => {
                self.discoveries.fetch_add(1, Ordering::Relaxed);
                if self.early {
                    byte::store(&self.phase, PHASE_DONE, Tracked::assume_new());
                }
                // SAFETY: only the exchange's winner writes, before `DONE`
                // (loom reports a race if not).
                self.cell.with_mut(|p| unsafe { *p = RECORD });
                if !self.early {
                    byte::store(&self.phase, PHASE_DONE, Tracked::assume_new());
                }
                true
            }
            Err(_) => {
                while wait && byte::load(&self.phase, Tracked::assume_new()) != PHASE_DONE {
                    loom::hint::spin_loop();
                }
                false
            }
        }
    }

    /// `Deferred::published`: after a waiting `discover`, the cell read
    /// with no second load of the phase (`Shared::cgroup_now`).
    fn published(&self) -> u64 {
        // SAFETY: `DONE` observed by this thread's `SeqCst` load in
        // `discover`, or written by it (loom reports a race if not).
        self.cell.with(|p| unsafe { *p })
    }

    /// `Deferred::fork_child`.
    fn fork_child(&self) {
        let _ = byte::compare_exchange(
            &self.phase,
            PHASE_RUNNING,
            PHASE_PENDING,
            Tracked::assume_new(),
        );
    }
}

/// A tick discovers without waiting while a reader of the figures
/// discovers and waits, and the main thread samples (`get`).
fn run_deferred(early: bool) {
    model(move || {
        let d = leak(Deferred::later(early));
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
///
/// Runs `vspec::byte::load`, `vspec::byte::store` and
/// `vspec::byte::compare_exchange`.
#[test]
fn loom_oncecell_deferred_discover_races_get() {
    run_deferred(false);
}

/// The publication check has teeth: a `DONE` store before the
/// discovery's write lets a reader see `DONE` without that write.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_oncecell_deferred_early_publish_is_a_race() {
    run_deferred(true);
}

/// The child of a fork whose other thread was discovering (the phase left
/// at `RUNNING`): the reset makes the child's waiting discovery run
/// rather than spin for ever; a reset of a published record leaves it.
///
/// Runs `vspec::byte::load`, `vspec::byte::store` and
/// `vspec::byte::compare_exchange`.
#[test]
fn loom_oncecell_deferred_fork_child_restarts() {
    model(|| {
        let d = leak(Deferred::later(false));
        byte::store(&d.phase, PHASE_RUNNING, Tracked::assume_new());
        d.fork_child();
        assert_eq!(d.get(), None);
        assert!(d.discover(true), "the child's discovery did not run");
        d.fork_child();
        assert_eq!(d.get(), Some(RECORD), "the reset undid a published record");
        assert!(!d.discover(true));
        assert_eq!(d.discoveries.load(Ordering::Relaxed), 1);
    });
}
