//! `background::start`'s publication against the new thread's exit
//! (`src/purge/background.rs`), a copy (its state lock and
//! atomics are statics built in `const` context, which loom cannot
//! construct). Each step names the code it mirrors:
//!
//! - the starter (`S`) is `start` from its first state lock hold on:
//!   under the lock, `creating = true` and `exited = false`; then the
//!   thread's creation (`create`, which spawns `B`); then, under the lock
//!   again, `creating = false`, `present = thread.is_some() && !exited`
//!   and `PRESENT.store(present, Relaxed)`; then, if it published,
//!   `fence(SeqCst)` and `kick()`;
//! - the thread (`B`) is `main`'s loop: load `SEQ` `SeqCst`; if
//!   `requested()` (`REQUESTED`, `Relaxed`) is false, take the state
//!   lock, `present = false`, `exited = true`, `PRESENT.store(false,
//!   Relaxed)`, and return; otherwise wait on `SEQ` with the loaded value
//!   and no timeout (the pass touches none of these);
//! - the withdrawer (`W`) is `request(false)`: under the state lock,
//!   `REQUESTED.store(false, Relaxed)` (and `AUTO`'s store, which nothing
//!   here reads); then `kick()`.
//!
//! Checked: once all three are done, `B` has returned and neither the
//! state nor `PRESENT` says a thread is present, so `start` never leaves
//! published a thread that exited before it published it. The control
//! (`present = thread.is_some()`, without the `exited` test) does in some
//! execution: `B` sees the withdrawal and exits between `S`'s two holds.
//! Verus proves the lock-value side (`exited ==> !present` in
//! `axiom_state_lock_inv`, asserted at each guard's write); that `exited` is set
//! exactly when the created thread exits is read from `main`. The model
//! adds the interleavings of the three threads, which Verus does not see
//! (loom stops the control at its first failing one).
//!
//! Left out: the stack, the recorded `thread` and the `errno` save; a
//! creation that fails (nothing is published then); `wake`'s kicks
//! (each is one more RMW of `SEQ`, as in `exit.rs`).

use crate::futex::{wait, wake};
use crate::{leak, model};
use loom::sync::Mutex;
use loom::sync::atomic::{AtomicBool, AtomicU32, Ordering, fence};
use loom::thread;

#[derive(Clone, Copy)]
struct Variant {
    /// The publication tests `exited` (as shipped).
    checks_exited: bool,
}

const SHIPPED: Variant = Variant {
    checks_exited: true,
};

/// `background::State`'s flags.
struct State {
    present: bool,
    creating: bool,
    exited: bool,
}

struct Statics {
    /// `background::STATE`.
    state: Mutex<State>,
    /// `background::PRESENT`.
    present: AtomicBool,
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
    fn withdraw(&self) {
        {
            let _g = self.state.lock().unwrap();
            self.requested.store(false, Ordering::Relaxed);
        }
        self.kick();
    }

    /// `background::main`'s loop.
    fn thread(&self) {
        loop {
            let seq = self.seq.load(Ordering::SeqCst);
            if !self.requested.load(Ordering::Relaxed) {
                let mut g = self.state.lock().unwrap();
                g.present = false;
                g.exited = true;
                self.present.store(false, Ordering::Relaxed);
                return;
            }
            wait(&self.seq, seq);
        }
    }

    /// `background::start`, with a creation that succeeds; the new
    /// thread's handle, `None` when it returned before creating one.
    fn start(&'static self, v: Variant) -> Option<thread::JoinHandle<()>> {
        {
            let mut g = self.state.lock().unwrap();
            if g.present || g.creating {
                return None;
            }
            g.creating = true;
            g.exited = false;
        }
        let b = thread::spawn(move || self.thread());
        let present = {
            let mut g = self.state.lock().unwrap();
            g.creating = false;
            g.present = !v.checks_exited || !g.exited;
            self.present.store(g.present, Ordering::Relaxed);
            g.present
        };
        if present {
            fence(Ordering::SeqCst);
            self.kick();
        }
        Some(b)
    }
}

fn run(v: Variant) {
    model(move || {
        let st = leak(Statics {
            state: Mutex::new(State {
                present: false,
                creating: false,
                exited: false,
            }),
            present: AtomicBool::new(false),
            requested: AtomicBool::new(true),
            seq: AtomicU32::new(0),
        });
        let s = thread::spawn(move || st.start(v));
        st.withdraw();
        let b = s
            .join()
            .unwrap()
            .expect("nothing was present or being created");
        b.join().unwrap();
        let g = st.state.lock().unwrap();
        assert!(
            !g.present && !st.present.load(Ordering::Relaxed),
            "start published a thread that had exited"
        );
    });
}

/// After a final `request(false)` the thread has exited and is not
/// published, whether it exits before `start`'s second hold or after it.
#[test]
fn loom_publish_skips_an_exited_thread() {
    run(SHIPPED);
}

/// The check has teeth: a publication that ignores `exited` leaves an
/// exited thread published.
#[test]
#[should_panic(expected = "start published a thread that had exited")]
fn loom_publish_without_exited_check_is_stale() {
    run(Variant {
        checks_exited: false,
    });
}
