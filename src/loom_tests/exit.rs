//! The background thread's exit (`purge/background.rs`), a copy (its
//! atomics are statics built in `const` context, which loom cannot
//! construct). Each step names the code it mirrors:
//!
//! - the thread (`B`) is `background::main`'s loop: load `SEQ` `SeqCst`,
//!   return if `requested()` (`REQUESTED`, `Relaxed`) is false, otherwise
//!   run a pass and `futex_wait` on `SEQ` with the loaded value. The pass
//!   (`sample`, `decay`) touches neither word; the model takes its worst
//!   case for this property, no deadline, so `B` sleeps with no timeout;
//! - the withdrawer (`W`) is `background::request(false)`: store
//!   `REQUESTED` `Relaxed`, then `kick()` (`SEQ.fetch_add(1, SeqCst)`,
//!   then a futex wake of one waiter);
//! - a concurrent waker (`K`, optional) is `background::wake` with
//!   `present()` true: another `kick()`, an RMW of `SEQ` between `W`'s and
//!   `B`'s accesses.
//!
//! Checked: `B` returns in every execution (a `B` asleep after `W` and
//! `K` are done is a reported deadlock, since the main thread joins it).
//!
//! Left out: the state lock `B` takes to clear `present` on its way out
//! (after the decision, so it does not bear on it), and `request(true)`
//! after the withdrawal (the property is about a withdrawal that is the
//! last request).

use super::futex::{wait, wake};
use super::model;
use crate::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use loom::thread;
use std::boxed::Box;

#[derive(Clone, Copy)]
struct Variant {
    /// `request(false)` kicks (as shipped).
    withdraw_kicks: bool,
    /// The ordering of `B`'s `SEQ` load (`SeqCst` as shipped).
    seq_load: Ordering,
    /// Run `K` too.
    waker: bool,
}

const SHIPPED: Variant = Variant {
    withdraw_kicks: true,
    seq_load: Ordering::SeqCst,
    waker: false,
};

struct Statics {
    /// `background::REQUESTED`.
    requested: AtomicBool,
    /// `background::SEQ`.
    seq: AtomicU32,
}

impl Statics {
    /// `background::kick`.
    fn kick(&self) {
        self.seq.fetch_add(1, Ordering::SeqCst);
        wake(&self.seq);
    }

    /// `background::request(false)`.
    fn withdraw(&self, v: Variant) {
        self.requested.store(false, Ordering::Relaxed);
        if v.withdraw_kicks {
            self.kick();
        }
    }

    /// `background::main`'s loop.
    fn thread(&self, v: Variant) {
        loop {
            let seq = self.seq.load(v.seq_load);
            if !self.requested.load(Ordering::Relaxed) {
                return;
            }
            wait(&self.seq, seq);
        }
    }
}

fn run(v: Variant) {
    model(move || {
        // Leaked, not an `Arc`: a deadlock ends the execution with `B`
        // parked, and dropping a loom object while that panic unwinds
        // aborts the test binary.
        let st: &'static Statics = Box::leak(Box::new(Statics {
            requested: AtomicBool::new(true),
            seq: AtomicU32::new(0),
        }));
        let b = thread::spawn(move || st.thread(v));
        let k = v.waker.then(|| thread::spawn(move || st.kick()));
        st.withdraw(v);
        if let Some(k) = k {
            k.join().unwrap();
        }
        b.join().unwrap();
    });
}

/// `B` exits after `request(false)`, whether its `SEQ` load comes before
/// or after the withdrawal's kick.
#[test]
fn loom_exit_after_withdrawal() {
    run(SHIPPED);
}

/// The same with another kick on `SEQ` in between: `B` reading that
/// kick's value still synchronises with the withdrawal (the release
/// sequence of `W`'s `fetch_add` continues through `K`'s RMW).
#[test]
fn loom_exit_after_withdrawal_with_waker() {
    run(Variant {
        waker: true,
        ..SHIPPED
    });
}

/// The check has teeth: a withdrawal that does not kick leaves a `B`
/// that already passed its `REQUESTED` check asleep.
#[test]
#[should_panic(expected = "deadlock")]
fn loom_exit_without_kick_is_lost() {
    run(Variant {
        withdraw_kicks: false,
        ..SHIPPED
    });
}

/// The `SeqCst` (acquire) `SEQ` load is needed: a `Relaxed` one may read
/// the kick's value yet the old `REQUESTED`, then wait on the new value
/// with no kick to come.
#[test]
#[should_panic(expected = "deadlock")]
fn loom_exit_relaxed_seq_load_is_lost() {
    run(Variant {
        seq_load: Ordering::Relaxed,
        ..SHIPPED
    });
}
