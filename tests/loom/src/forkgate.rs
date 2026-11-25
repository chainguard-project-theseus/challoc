//! The init/fork gate (`src/api/mod.rs`, WEAKMEM P8), a copy: `INIT_GATE`
//! and `GATE_FORKER` are statics, which loom cannot construct. Each step
//! names the code it mirrors:
//!
//! - [`Gate::init_gate_enter`] is `api::init_gate_enter`: an `AcqRel`
//!   compare-exchange (`Acquire` on failure) from `GATE_IDLE` to the
//!   thread's `pthread_self`; on failure it returns on `GATE_DONE`, its
//!   own ID, or `GATE_FORKING` with its own ID in the forker slot
//!   (`Relaxed` load), and spins otherwise;
//! - [`Gate::init_gate_done`] is `api::init_gate_done`: an `AcqRel`
//!   compare-exchange from the thread's ID to `GATE_DONE`;
//! - [`Gate::fork_gate_enter`] is `api::fork_gate_enter`: an `Acquire`
//!   load of the gate, return on `GATE_DONE` or its own ID; a `Relaxed`
//!   load of the slot, return on its own ID; with the gate `GATE_IDLE`
//!   and the slot 0, claim the slot (`Relaxed` compare-exchange from 0)
//!   and close the gate (`AcqRel` compare-exchange, `Acquire` on failure,
//!   `GATE_IDLE` to `GATE_FORKING`), giving the slot back (`Relaxed`
//!   store of 0) when the close fails; spin otherwise;
//! - [`Gate::fork_gate_exit`] is `api::fork_gate_exit`: with the slot
//!   its own and the gate `GATE_FORKING` (`Relaxed` loads), store
//!   `GATE_DONE` if challoc is ready (`shared_if_ready`, here an
//!   `Acquire` load of `ready`) else `GATE_IDLE` (`Release`), then the
//!   slot's 0 (`Relaxed`).
//!
//! An init is `init_gate_enter`, the publication (here a `Release` store
//! of `ready`, which stands for P4's `READY`), then `init_gate_done`.
//! `pthread_self` is a distinct number per modelled thread, never 0, 1 or
//! 2 (`sys::pthread_self`).
//!
//! Checked (#949): while one thread's `fork` holds the gate closed or
//! one thread's init is under way, no other thread's `fork_gate_enter`
//! or `init_gate_enter` returns, nor closes the gate (each return and
//! each close asserts that the model's records of the closing thread and
//! of the initialising thread are 0 or the caller); the closer's own
//! init inside its window passes `init_gate_enter` (it finds its own ID
//! in the slot); init runs once; the run ends with the gate `GATE_DONE`
//! and the slot 0; and every thread returns (a thread left spinning
//! fails the model at loom's branch limit: loom's `spin_loop` yields).
//! Controls show the checks have teeth: one per behaviour #949 removed,
//! [`loom_forkgate_slot_store_fails`] (a slot claimed by a plain store
//! lets two forks that both read an idle gate overwrite the closer's ID,
//! and the closer's own init hangs) and
//! [`loom_forkgate_passing_a_closed_gate_fails`] (a fork that goes on at
//! another thread's `GATE_FORKING`); and two for the give-back after a
//! failed close, which only a fork racing an init runs,
//! [`loom_forkgate_kept_slot_fails`] (the slot ends held) and
//! [`loom_forkgate_kept_slot_passes_an_init_fails`] (the fork returns at
//! its own slot while the init is under way).
//!
//! Left out: the rest of the prepare, parent and child handlers (the
//! locks and the holder record, `lock::FORK_HOLDER`, which only a holder
//! of every lock writes); a `fork` from a signal handler (design 9,
//! residuals); `_Fork` itself (the child, whose one thread is the
//! closer's copy, runs the same `fork_gate_exit`).

use crate::{leak, model};
use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use loom::thread;

const GATE_IDLE: usize = 0;
const GATE_DONE: usize = 1;
const GATE_FORKING: usize = 2;

#[derive(Clone, Copy)]
struct Variant {
    /// Claim the slot with a plain store after reading it 0 (before #949)
    /// rather than the compare-exchange.
    claim_by_store: bool,
    /// `fork_gate_enter` also returns on another thread's `GATE_FORKING`
    /// (before #949).
    pass_forking: bool,
    /// `fork_gate_enter` keeps the slot after a failed close (drops the
    /// give-back store).
    skip_giveback: bool,
}

const SHIPPED: Variant = Variant {
    claim_by_store: false,
    pass_forking: false,
    skip_giveback: false,
};

struct Gate {
    /// `api::INIT_GATE`.
    gate: AtomicUsize,
    /// `api::GATE_FORKER`.
    forker: AtomicUsize,
    /// `STATE`'s `READY` (P4), as `shared_if_ready` reads it.
    ready: AtomicBool,
    /// The thread whose fork holds the gate closed, or 0 (model
    /// bookkeeping, `SeqCst`).
    closer: AtomicUsize,
    /// The thread whose init is past `init_gate_enter` and not yet at
    /// `init_gate_done`, or 0 (model bookkeeping, `SeqCst`).
    initing: AtomicUsize,
    /// How many times init ran (model bookkeeping).
    inits: AtomicU32,
}

impl Gate {
    fn new() -> Gate {
        Gate {
            gate: AtomicUsize::new(GATE_IDLE),
            forker: AtomicUsize::new(0),
            ready: AtomicBool::new(false),
            closer: AtomicUsize::new(0),
            initing: AtomicUsize::new(0),
            inits: AtomicU32::new(0),
        }
    }

    /// No other thread's fork holds the gate closed, and no other
    /// thread's init is under way (model check).
    fn check_open_to(&self, me: usize, what: &str) {
        let c = self.closer.load(Ordering::SeqCst);
        assert!(
            c == 0 || c == me,
            "{what} of thread {me} returned while thread {c}'s fork holds the gate"
        );
        let i = self.initing.load(Ordering::SeqCst);
        assert!(
            i == 0 || i == me,
            "{what} of thread {me} returned while thread {i}'s init holds the gate"
        );
    }

    /// The end state: the gate done and the forker slot given back (model
    /// check).
    fn check_end(&self) {
        assert_eq!(
            self.inits.load(Ordering::Relaxed),
            1,
            "init ran other than once"
        );
        assert_eq!(
            self.gate.load(Ordering::Relaxed),
            GATE_DONE,
            "the gate did not end done"
        );
        let f = self.forker.load(Ordering::Relaxed);
        assert_eq!(f, 0, "the forker slot ended held by thread {f}");
    }

    /// `api::init_gate_enter`.
    fn init_gate_enter(&self, me: usize) {
        loop {
            let g =
                match self
                    .gate
                    .compare_exchange(GATE_IDLE, me, Ordering::AcqRel, Ordering::Acquire)
                {
                    Ok(_) => return,
                    Err(g) => g,
                };
            if g == GATE_DONE
                || g == me
                || (g == GATE_FORKING && self.forker.load(Ordering::Relaxed) == me)
            {
                return;
            }
            loom::hint::spin_loop();
        }
    }

    /// `api::init_gate_done`.
    fn init_gate_done(&self, me: usize) {
        let _ = self
            .gate
            .compare_exchange(me, GATE_DONE, Ordering::AcqRel, Ordering::Relaxed);
    }

    /// An init: the gate, the publication, `init_gate_done`.
    fn init(&self, me: usize) {
        self.init_gate_enter(me);
        self.check_open_to(me, "init_gate_enter");
        self.initing.store(me, Ordering::SeqCst);
        if !self.ready.load(Ordering::Acquire) {
            self.inits.fetch_add(1, Ordering::Relaxed);
            self.ready.store(true, Ordering::Release);
        }
        self.initing.store(0, Ordering::SeqCst);
        self.init_gate_done(me);
    }

    /// `api::fork_gate_enter`; returns whether this call closed the gate
    /// (model bookkeeping).
    fn fork_gate_enter(&self, me: usize, v: Variant) -> bool {
        loop {
            let g = self.gate.load(Ordering::Acquire);
            if g == GATE_DONE || g == me || (v.pass_forking && g == GATE_FORKING) {
                return false;
            }
            let f = self.forker.load(Ordering::Relaxed);
            if f == me {
                return false;
            }
            let claimed = g == GATE_IDLE
                && f == 0
                && if v.claim_by_store {
                    self.forker.store(me, Ordering::Relaxed);
                    true
                } else {
                    self.forker
                        .compare_exchange(0, me, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                };
            if claimed {
                if self
                    .gate
                    .compare_exchange(GATE_IDLE, GATE_FORKING, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return true;
                }
                if !v.skip_giveback {
                    self.forker.store(0, Ordering::Relaxed);
                }
            } else {
                loom::hint::spin_loop();
            }
        }
    }

    /// `api::fork_gate_exit`.
    fn fork_gate_exit(&self, me: usize) {
        if self.forker.load(Ordering::Relaxed) == me
            && self.gate.load(Ordering::Relaxed) == GATE_FORKING
        {
            let open = if self.ready.load(Ordering::Acquire) {
                GATE_DONE
            } else {
                GATE_IDLE
            };
            self.closer.store(0, Ordering::SeqCst);
            self.gate.store(open, Ordering::Release);
            self.forker.store(0, Ordering::Relaxed);
        }
    }

    /// A `fork`'s gate steps: enter, the window (where the forking thread
    /// may initialise, as a handler registered before challoc's that
    /// allocates does), exit.
    fn fork(&self, me: usize, init_inside: bool, v: Variant) {
        if self.fork_gate_enter(me, v) {
            let c = self.closer.swap(me, Ordering::SeqCst);
            assert_eq!(c, 0, "thread {me} closed the gate thread {c}'s fork holds");
            let i = self.initing.load(Ordering::SeqCst);
            assert_eq!(
                i, 0,
                "thread {me} closed the gate while thread {i}'s init holds it"
            );
        } else {
            self.check_open_to(me, "fork_gate_enter");
        }
        if init_inside {
            self.init(me);
        }
        self.fork_gate_exit(me);
    }
}

/// Two threads fork at once before challoc is initialised; the first
/// (and, with `both_init`, the second) initialises inside its window.
fn two_forks(both_init: bool, v: Variant) {
    model(move || {
        let g = leak(Gate::new());
        let b = thread::spawn(move || g.fork(32, both_init, v));
        g.fork(16, true, v);
        b.join().unwrap();
        g.check_end();
    });
}

/// A fork (thread 16, no allocation in its window) races an init on
/// thread 32; `end` runs the end-state check (a control turns it off to
/// show the exclusion check alone).
fn fork_races_init(v: Variant, end: bool) {
    model(move || {
        let g = leak(Gate::new());
        let t = thread::spawn(move || g.init(32));
        g.fork(16, false, v);
        t.join().unwrap();
        if end {
            g.check_end();
        }
    });
}

/// Two forks race to close the idle gate, the closer initialising inside
/// its window: one closes it, the other waits until the closer's exit
/// and goes on at `GATE_DONE`; the closer's init finds its own ID in the
/// slot.
///
/// Covers copies of `api::init_gate_enter`, `api::init_gate_done`,
/// `api::fork_gate_enter` and `api::fork_gate_exit`.
#[test]
fn loom_forkgate_two_forks() {
    two_forks(false, SHIPPED);
}

/// As [`loom_forkgate_two_forks`], with both forks' windows allocating:
/// whichever closes initialises, the other finds challoc ready.
///
/// Covers copies of `api::init_gate_enter`, `api::init_gate_done`,
/// `api::fork_gate_enter` and `api::fork_gate_exit`.
#[test]
fn loom_forkgate_two_forks_both_allocate() {
    two_forks(true, SHIPPED);
}

/// A fork races an init on another thread: the init waits while the
/// fork holds the gate, or the fork waits for the init; never both past
/// the gate at once. This is the case that runs the give-back after a
/// failed close (the fork claims the slot, the init takes the idle gate
/// first); the slot ends given back.
///
/// Covers copies of `api::init_gate_enter`, `api::init_gate_done`,
/// `api::fork_gate_enter` and `api::fork_gate_exit`.
#[test]
fn loom_forkgate_fork_races_init() {
    fork_races_init(SHIPPED, true);
}

/// The check has teeth: with the slot claimed by a plain store, two forks
/// that both read an idle gate and an empty slot both store, the loser
/// gives the slot back (or leaves its own ID) after its close fails, and
/// the closer's own init no longer finds its ID and spins until the
/// closer's exit, which cannot come first (the hang P8 recorded before
/// #949).
#[test]
#[should_panic(expected = "maximum number of branches")]
fn loom_forkgate_slot_store_fails() {
    two_forks(
        false,
        Variant {
            claim_by_store: true,
            ..SHIPPED
        },
    );
}

/// The exclusion check has teeth: a `fork_gate_enter` that goes on at
/// another thread's `GATE_FORKING`, as before #949, returns while that
/// fork holds the gate (the case `tests/c/fork_race.c` reproduces).
#[test]
#[should_panic(expected = "'s fork holds the gate")]
fn loom_forkgate_passing_a_closed_gate_fails() {
    two_forks(
        false,
        Variant {
            pass_forking: true,
            ..SHIPPED
        },
    );
}

/// The give-back has teeth (the end-state check): a fork that keeps the
/// slot after its close lost to another thread's init leaves the slot
/// held for good (`fork_gate_exit` clears it only at `GATE_FORKING`).
#[test]
#[should_panic(expected = "the forker slot ended held by thread 16")]
fn loom_forkgate_kept_slot_fails() {
    fork_races_init(
        Variant {
            skip_giveback: true,
            ..SHIPPED
        },
        true,
    );
}

/// The give-back has teeth (the exclusion check alone): the fork that
/// kept the slot returns at its own slot on its next pass while the
/// other thread's init is under way, so `_Fork` would copy a half-built
/// init.
#[test]
#[should_panic(
    expected = "fork_gate_enter of thread 16 returned while thread 32's init holds the gate"
)]
fn loom_forkgate_kept_slot_passes_an_init_fails() {
    fork_races_init(
        Variant {
            skip_giveback: true,
            ..SHIPPED
        },
        false,
    );
}
