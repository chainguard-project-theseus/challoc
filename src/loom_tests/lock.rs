//! P2: `RawLock` (`lock.rs`), its real code over loom's atomics, with the
//! futex stand-in `loom_tests::futex` and two spin rounds before a sleep.
//!
//! Checked: mutual exclusion and the acquire/release edge (the counter is
//! a loom `UnsafeCell`, so two holders at once, or a holder that does not
//! see the previous one's write, is a reported race), and no lost wake-up
//! (a waiter left asleep with the others done is a reported deadlock).

use super::futex;
use super::{model, model_bounded};
use crate::lock::RawLock;
use crate::sync::atomic::{AtomicU32, Ordering};
use loom::cell::UnsafeCell;
use loom::thread;
use std::boxed::Box;

/// The two lock operations, for the real lock and the control below.
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
        // Leaked, not an `Arc`: a deadlock ends the execution with
        // threads parked, and dropping a loom object while that panic
        // unwinds aborts the test binary.
        let c: &'static Counter<L> = Box::leak(Box::new(Counter {
            lock: L::new(),
            n: UnsafeCell::new(0),
        }));
        let hs: std::vec::Vec<_> = (1..threads)
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
/// Bounded at 5 preemptions (`loom_tests`).
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
        DropsTheMark(AtomicU32::new(0))
    }
    fn lock(&self) {
        let try_acquire = || {
            self.0
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        };
        if try_acquire() {
            return;
        }
        let mut c = self.0.swap(2, Ordering::Acquire);
        while c != 0 {
            futex::wait(&self.0, 2);
            c = if try_acquire() {
                0
            } else {
                self.0.swap(2, Ordering::Acquire)
            };
        }
    }
    fn unlock(&self) {
        if self.0.swap(0, Ordering::Release) == 2 {
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
        let w: &'static AtomicU32 = Box::leak(Box::new(AtomicU32::new(7)));
        futex::wait(w, 7);
    });
}
