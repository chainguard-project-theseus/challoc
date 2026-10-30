//! `RawLock` (`src/lock.rs`), a copy: the futex mutex in the shape of
//! Drepper's "mutex, take 3", over loom's atomics, with the futex stand-in
//! `futex` for `lock.rs`'s `wait` and `wake`. Each step names the code it
//! mirrors:
//!
//! - [`RawLock::try_acquire`] is `RawLock::try_acquire`: the CAS from
//!   `UNLOCKED` to `LOCKED`, `Acquire` on success, `Relaxed` on failure;
//! - [`RawLock::lock`] is `RawLock::lock` and `lock_slow`: the CAS, then
//!   [`SPINS`] rounds of a `Relaxed` poll and the CAS (with loom's
//!   `spin_loop`, which yields, for `core::hint::spin_loop`), then
//!   `swap(CONTENDED, Acquire)` and, while it did not return `UNLOCKED`, a
//!   futex wait on `CONTENDED` and the swap again;
//! - [`RawLock::unlock`] is `RawLock::release`: `swap(UNLOCKED, Release)`,
//!   and a futex wake of one waiter if it returned `CONTENDED`.
//!
//! Checked: mutual exclusion and the acquire/release edge (the counter is
//! a loom `UnsafeCell`, so two holders at once, or a holder that does not
//! see the previous one's write, is a reported race), and no lost wake-up
//! (a waiter left asleep with the others done is a reported deadlock).
//!
//! Left out: the ghost rank and the `LockHeld` tokens, which are not
//! compiled; `try_lock`, a bare `try_acquire`.

use crate::futex;
use crate::{leak, model, model_bounded};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU32, Ordering};
use loom::thread;

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
const CONTENDED: u32 = 2;

/// Spins before the first sleep: 2 here, 100 in `lock.rs`. Each round is
/// one more poll the model has to interleave; two rounds keep both the
/// spin's acquire and the fall-through to the futex in every model.
const SPINS: u32 = 2;

/// `lock.rs`'s `RawLock`.
pub(crate) struct RawLock {
    state: AtomicU32,
}

impl RawLock {
    /// `RawLock::new`: a free lock.
    pub(crate) fn new() -> RawLock {
        RawLock {
            state: AtomicU32::new(UNLOCKED),
        }
    }

    /// `RawLock::try_acquire`.
    fn try_acquire(&self) -> bool {
        self.state
            .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// `RawLock::lock`, then `lock_slow`.
    pub(crate) fn lock(&self) {
        if self.try_acquire() {
            return;
        }
        for _ in 0..SPINS {
            if self.state.load(Ordering::Relaxed) == UNLOCKED && self.try_acquire() {
                return;
            }
            loom::hint::spin_loop();
        }
        let mut c = self.state.swap(CONTENDED, Ordering::Acquire);
        while c != UNLOCKED {
            futex::wait(&self.state, CONTENDED);
            c = self.state.swap(CONTENDED, Ordering::Acquire);
        }
    }

    /// `RawLock::release`.
    pub(crate) fn unlock(&self) {
        if self.state.swap(UNLOCKED, Ordering::Release) == CONTENDED {
            futex::wake(&self.state);
        }
    }
}

/// The two lock operations, for the copy and the control below.
trait Mutex: Sync + 'static {
    fn new() -> Self;
    fn lock(&self);
    fn unlock(&self);
}

impl Mutex for RawLock {
    fn new() -> Self {
        RawLock::new()
    }
    fn lock(&self) {
        RawLock::lock(self)
    }
    fn unlock(&self) {
        RawLock::unlock(self)
    }
}

struct Counter<L> {
    lock: L,
    n: UnsafeCell<u32>,
}

// SAFETY: `n` is touched only under `lock`, which is what is checked.
unsafe impl<L: Sync> Sync for Counter<L> {}

impl<L: Mutex> Counter<L> {
    fn bump(&self) {
        self.lock.lock();
        // SAFETY: under the lock (loom reports a race if not).
        self.n.with_mut(|p| unsafe { *p += 1 });
        self.lock.unlock();
    }
}

/// `threads` lockers bump the counter once each.
fn contend<L: Mutex>(threads: u32) -> impl Fn() + Sync + Send + 'static {
    move || {
        let c = leak(Counter {
            lock: L::new(),
            n: UnsafeCell::new(0),
        });
        let hs: Vec<_> = (1..threads)
            .map(|_| thread::spawn(move || c.bump()))
            .collect();
        c.bump();
        for h in hs {
            h.join().unwrap();
        }
        // SAFETY: every other thread has been joined.
        assert_eq!(c.n.with(|p| unsafe { *p }), threads);
    }
}

/// Two lockers: the CAS fast path against the spin and the sleep.
#[test]
fn loom_lock_two_threads() {
    model(contend::<RawLock>(2));
}

/// Three lockers: the third arrives while the second sleeps on
/// `CONTENDED`, so a release must wake a sleeper behind another waiter.
/// Bounded at 5 preemptions: too large to exhaust.
#[test]
fn loom_lock_three_threads() {
    model_bounded(5, contend::<RawLock>(3));
}

/// A control: `RawLock`'s protocol with one mutation, a woken waiter
/// that retakes the lock by CAS to `LOCKED` instead of
/// `swap(CONTENDED)`. That drops the mark of a waiter still asleep, so
/// the next release does not wake it.
struct DropsTheMark(AtomicU32);

impl Mutex for DropsTheMark {
    fn new() -> Self {
        DropsTheMark(AtomicU32::new(UNLOCKED))
    }
    fn lock(&self) {
        let try_acquire = || {
            self.0
                .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        };
        if try_acquire() {
            return;
        }
        let mut c = self.0.swap(CONTENDED, Ordering::Acquire);
        while c != UNLOCKED {
            futex::wait(&self.0, CONTENDED);
            c = if try_acquire() {
                UNLOCKED
            } else {
                self.0.swap(CONTENDED, Ordering::Acquire)
            };
        }
    }
    fn unlock(&self) {
        if self.0.swap(UNLOCKED, Ordering::Release) == CONTENDED {
            futex::wake(&self.0);
        }
    }
}

/// The lost-wake-up check has teeth: the mutant leaves a waiter asleep.
#[test]
#[should_panic(expected = "deadlock")]
fn loom_lock_dropped_mark_loses_a_wake_up() {
    model_bounded(5, contend::<DropsTheMark>(3));
}

/// The stand-in sleeps for real: a wait on a word nobody wakes is a
/// deadlock.
#[test]
#[should_panic(expected = "deadlock")]
fn loom_futex_stand_in_sleeps() {
    model(|| {
        let w = leak(AtomicU32::new(7));
        futex::wait(w, 7);
    });
}
