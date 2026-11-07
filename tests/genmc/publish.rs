// GenMC port of tests/loom/src/publish.rs (P3, `background::start`'s
// publication against the new thread's exit): copies of `request(false)`,
// `kick`, `start` from its first state lock hold on and `main`'s loop
// and exit (src/purge/background.rs), as in the loom model. The state
// lock is genmc-std's `Mutex`, not the `RawLock` copy, and the futex is
// prelude.rs's stand-in.
//
// - the starter (`S`) is `start`: under the lock, `creating = true` and
//   `exited = false`; then the thread's creation (which spawns `B`);
//   then, under the lock again, `creating = false`,
//   `present = thread.is_some() && !exited` and
//   `PRESENT.store(present, Relaxed)`; then, if it published,
//   `fence(SeqCst)` and `kick()`;
// - the thread (`B`) is `main`'s loop: load `SEQ` `SeqCst`; if
//   `requested()` is false, take the state lock, `present = false`,
//   `exited = true`, `PRESENT.store(false, Relaxed)`, and return;
//   otherwise wait on `SEQ` with the loaded value and no timeout;
// - the withdrawer (`W`) is `request(false)`: under the state lock,
//   `REQUESTED.store(false, Relaxed)`; then `kick()`.
//
// Checked (runs with `+live`): once all three are done, `B` has returned
// (a `B` asleep on `SEQ`'s last value is a reported liveness violation)
// and neither the state nor `PRESENT` says a thread is present, so
// `start` never leaves published a thread that exited before it
// published it. One control (`CHECKS_EXITED` false: `present =
// thread.is_some()`) does, when `B` sees the withdrawal and exits
// between `S`'s two holds; the other (`SEQ_LOAD` `Relaxed`) leaves `B`
// asleep, reading the last kick's `SEQ` with the old `REQUESTED`.
//
// The seam ledger cites this template's pass cases (verus/SEAMS.md,
// "Status") for `background::main` (`src/purge/background.rs`), whose
// loop the harness copies (pinned by tests/loom/tests/pins.rs). It is
// evidence only: a copy runs, not the body, over prelude.rs's futex
// stand-in, and no case checks the seam's contract.
//
// Left out, as in the loom model: the stack, the recorded `thread` and
// the `errno` save; a creation that fails; `request`'s `AUTO` store
// (nothing here reads it); `wake`'s kicks (each one more RMW of `SEQ`,
// as in exit.rs).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering, fence};
use std::thread;

/// `background::State`'s flags.
struct State {
    present: bool,
    creating: bool,
    exited: bool,
}

/// `background::STATE`.
static STATE: Mutex<State> = Mutex::new(State {
    present: false,
    creating: false,
    exited: false,
});
/// `background::PRESENT`.
static PRESENT: AtomicBool = AtomicBool::new(false);
/// `background::REQUESTED`.
static REQUESTED: AtomicBool = AtomicBool::new(true);
/// `background::SEQ`.
static SEQ: AtomicU32 = AtomicU32::new(0);

/// `background::kick`.
fn kick() {
    SEQ.fetch_add(1, Ordering::SeqCst);
    futex_wake(&SEQ);
}

/// `background::request(false)`.
fn withdraw() {
    let g = STATE.lock().unwrap();
    REQUESTED.store(false, Ordering::Relaxed);
    drop(g);
    kick();
}

/// `background::main`'s loop.
fn background_main() {
    let mut spurious = SPURIOUS;
    loop {
        let seq = SEQ.load(SEQ_LOAD);
        if !REQUESTED.load(Ordering::Relaxed) {
            let mut g = STATE.lock().unwrap();
            g.present = false;
            g.exited = true;
            PRESENT.store(false, Ordering::Relaxed);
            drop(g);
            return;
        }
        futex_wait(&SEQ, seq, &mut spurious);
    }
}

/// `background::start`, with a creation that succeeds: whether a thread
/// is present on return. (Here it always creates one: a `start` that
/// returned before `create` would leave `B` waiting for its creation,
/// a reported liveness violation.)
fn start() -> bool {
    let mut g = STATE.lock().unwrap();
    if g.present {
        drop(g);
        return true;
    }
    if g.creating {
        drop(g);
        return false;
    }
    g.creating = true;
    g.exited = false;
    drop(g);
    create();
    let mut g = STATE.lock().unwrap();
    g.creating = false;
    g.present = !CHECKS_EXITED || !g.exited;
    PRESENT.store(g.present, Ordering::Relaxed);
    let present = g.present;
    drop(g);
    if present {
        fence(Ordering::SeqCst);
        kick();
    }
    present
}

fn harness() {
    let b = thread::spawn(created(background_main));
    let s = thread::spawn(start);
    withdraw();
    let _ = s.join().unwrap();
    b.join().unwrap();
    let g = STATE.lock().unwrap();
    gassert!(
        !g.present && !PRESENT.load(Ordering::Relaxed),
        "start published a thread that had exited"
    );
    drop(g);
}
