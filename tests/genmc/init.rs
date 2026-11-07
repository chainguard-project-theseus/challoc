// GenMC port of tests/loom/src/init.rs (P4, publishing `Shared`): copies
// of `api::state_load`, `state_claim`, `state_publish`, `shared`,
// `shared_if_ready` and `init_slow` (src/api/mod.rs), as in the loom
// model. These are `api/mod.rs`'s own functions over `STATE` and
// `INIT_TID`, not `vspec` wrappers, so the harness is a copy through
// and through, tied to the code by its pins.
//
// - `init_slow`: `compare_exchange(UNINIT, INITIALISING, Acquire,
//   Acquire)`; the winner stores its tid in `INIT_TID` `Relaxed`, runs
//   `init` and stores `READY` `Release`; a loser that sees `READY`
//   returns, one whose tid is `INIT_TID` aborts (re-entry), any other
//   spins on an `Acquire` load of `STATE` until `READY`;
// - `shared`: an `Acquire` load of `STATE`, `shared_ref` on `READY`,
//   `init_slow` otherwise; `shared_if_ready`, the `free` path's: the
//   load, and `shared_ref` on `READY`;
// - `init`: a plain write of the published value (`SHARED` and the rest
//   `init` writes are plain memory); `shared_ref` is a plain read of it.
//
// Checked: `init` runs once; every caller that returns reads the value
// `init` built, with no race (a reader not ordered after `init`'s write
// is a reported non-atomic race); a re-entrant call from `init` aborts
// and never waits for itself, and no other thread takes the abort; and,
// in runs with `+live`, every caller returns (a waiter left spinning on
// `STATE`'s last value is a reported liveness violation). The loom
// model's checks for the `STATE` wrappers' contracts are kept (every
// access to `STATE` reads one of its three values and never goes back
// in its modification order from the thread's last value: `state_load`,
// and `init_slow` for the exchange; with `READBACK`, the initialiser's
// load right after its `READY` store, a load the model adds, reads
// `READY`), though under GenMC's models, which have coherence as an
// axiom, and with nothing writing `STATE` but these three values once
// each, they cannot fail. Unlike loom, GenMC can run two waiters spinning
// at once (three callers): the wait is `await_until!`, one pass of its
// load, which blocks the waiter when it reads anything but `READY`.
//
// The seam ledger cites this template's pass cases (verus/SEAMS.md,
// "Status") for `api::state_load`, `api::state_claim`,
// `api::state_publish` and `api::init_slow` (`src/api/mod.rs`), whose
// lines the harness copies (pinned by tests/loom/tests/pins.rs). It is
// evidence only: a copy runs, not the body, and no case checks a seam's
// contract.
//
// Left out, as in the loom model: `gettid` (a distinct number per
// thread), what `init` does besides its write, the `Relaxed`
// configuration statics `init` stores, and the fork gate
// (`init_gate_enter`, `init_gate_done`), which orders init against a
// fork's prepare handler and nothing of `STATE` here.

use core::cell::UnsafeCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

const UNINIT: u32 = 0;
const INITIALISING: u32 = 1;
const READY: u32 = 2;

/// The value `init` builds.
const BUILT: u64 = 0x5eed;
/// `init_slow`'s re-entry abort (`sys::abort`), as a value.
const ABORT: u64 = 1;

struct Statics {
    /// `api::STATE`.
    state: AtomicU32,
    /// `api::INIT_TID`.
    init_tid: AtomicU32,
    /// `api::SHARED` (and the rest `init` writes).
    shared: UnsafeCell<u64>,
    /// How many times `init` ran (model bookkeeping).
    inits: AtomicU32,
    /// What the re-entrant call returned (model bookkeeping).
    reentry: AtomicU32,
}
// SAFETY: `shared` is written by `init` and read after `READY`, which is
// what is checked (GenMC reports a race if not).
unsafe impl Sync for Statics {}

/// A static, as `api::STATE` and `api::SHARED` are.
static ST: Statics = Statics {
    state: AtomicU32::new(UNINIT),
    init_tid: AtomicU32::new(0),
    shared: UnsafeCell::new(0),
    inits: AtomicU32::new(0),
    reentry: AtomicU32::new(0),
};

impl Statics {
    /// `api::shared_ref`: the plain read of the published value, volatile
    /// so that rustc at `-Copt-level=1` does not hoist it above the
    /// `READY` test (it is still a non-atomic access to GenMC).
    fn shared_ref(&self) -> u64 {
        // SAFETY: `READY` observed by an `Acquire` load on this thread, or
        // published by it (GenMC reports a race if not).
        unsafe { core::ptr::read_volatile(self.shared.get()) }
    }

    /// `api::state_load`: an `Acquire` load of `STATE`. `seen` is the
    /// thread's last value of `STATE`, read or written (model
    /// bookkeeping): the load reads one of the three values, never one
    /// before `seen` in the modification order, and becomes `seen`.
    fn state_load(&self, seen: &mut u32) -> u32 {
        let s = self.state.load(Ordering::Acquire);
        gassert!(s <= READY, "STATE read an unknown value");
        gassert!(s >= *seen, "STATE went back");
        *seen = s;
        s
    }

    /// `api::init`: build the value, here one plain write.
    fn init(&self, tid: u32, seen: &mut u32) {
        self.inits.fetch_add(1, Ordering::Relaxed);
        // SAFETY: only the exchange's winner runs `init`, before `READY`
        // (GenMC reports a race if not).
        unsafe { *self.shared.get() = BUILT };
        if REENTER {
            let r = match self.shared(tid, seen) {
                ABORT => 1,
                _ => 2,
            };
            self.reentry.store(r, Ordering::Relaxed);
        }
    }

    /// `api::init_slow`.
    fn init_slow(&self, tid: u32, seen: &mut u32) -> u64 {
        let won =
            self.state
                .compare_exchange(UNINIT, INITIALISING, Ordering::Acquire, Ordering::Acquire);
        // The exchange read `UNINIT` (and wrote `INITIALISING`) or the
        // value it returns, never one before `seen`.
        let s = match won {
            Ok(s) => s,
            Err(s) => s,
        };
        gassert!(s <= READY && s >= *seen, "the exchange went back");
        *seen = if won.is_ok() { INITIALISING } else { s };
        match won {
            Ok(_) => {
                self.init_tid.store(tid, Ordering::Relaxed);
                self.init(tid, seen);
                self.state.store(READY, PUBLISH);
                *seen = READY;
                if READBACK {
                    // `state_publish`'s `final(perm).value() == READY`: the
                    // publisher's next load reads it (a load the model adds).
                    gassert!(self.state_load(seen) == READY, "the publisher read back");
                }
            }
            Err(READY) => {}
            Err(_) => {
                if self.init_tid.load(Ordering::Relaxed) == tid {
                    return ABORT;
                }
                // `while load_state(..) != READY { spin_loop() }`.
                await_until!(self.state_load(seen) == READY);
            }
        }
        self.shared_ref()
    }

    /// `api::shared`.
    fn shared(&self, tid: u32, seen: &mut u32) -> u64 {
        if self.state_load(seen) == READY {
            return self.shared_ref();
        }
        self.init_slow(tid, seen)
    }

    /// `api::shared_if_ready`.
    fn shared_if_ready(&self) -> Option<u64> {
        let mut seen = UNINIT;
        if self.state_load(&mut seen) == READY {
            Some(self.shared_ref())
        } else {
            None
        }
    }
}

fn caller(tid: u32) -> u64 {
    let mut seen = UNINIT;
    ST.shared(tid, &mut seen)
}

/// `CALLERS` threads (tids 1 to 3) call `shared()` while the main thread
/// calls `shared_if_ready()` (a `free`). With `UNPUBLISHED`, loom's
/// `loom_init_unpublished_wait_fails`: a waiter on an initialiser that
/// never publishes `READY`.
fn harness() {
    if UNPUBLISHED {
        ST.state.store(INITIALISING, Ordering::Relaxed);
        ST.init_tid.store(1, Ordering::Relaxed);
        let mut seen = UNINIT;
        let _ = ST.init_slow(2, &mut seen);
        return;
    }
    let a = thread::spawn(|| caller(1));
    let b = if CALLERS > 1 {
        Some(thread::spawn(|| caller(2)))
    } else {
        None
    };
    let c = if CALLERS > 2 {
        Some(thread::spawn(|| caller(3)))
    } else {
        None
    };
    if let Some(x) = ST.shared_if_ready() {
        gassert!(x == BUILT, "a ready reader saw another value");
    }
    gassert!(
        a.join().unwrap() == BUILT,
        "a caller did not get the built value"
    );
    if let Some(b) = b {
        gassert!(
            b.join().unwrap() == BUILT,
            "a caller did not get the built value"
        );
    }
    if let Some(c) = c {
        gassert!(
            c.join().unwrap() == BUILT,
            "a caller did not get the built value"
        );
    }
    gassert!(
        ST.inits.load(Ordering::Relaxed) == 1,
        "init ran more than once"
    );
    if REENTER {
        gassert!(
            ST.reentry.load(Ordering::Relaxed) == 1,
            "re-entry did not abort"
        );
    }
}
