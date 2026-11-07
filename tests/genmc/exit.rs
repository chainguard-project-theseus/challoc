// GenMC port of tests/loom/src/exit.rs (P3, the background thread's
// exit): copies of `background::requested`, `request(false)`, `kick` and
// `main`'s loop (src/purge/background.rs), as in the loom model. The
// futex is prelude.rs's stand-in.
//
// - the thread (`B`) is `main`'s loop: load `SEQ` `SeqCst`, return if
//   `requested()` (`REQUESTED`, `Relaxed`) is false, otherwise wait on
//   `SEQ` with the loaded value (the pass, `sample` and `decay`, touches
//   neither word; the model takes its worst case, no deadline);
// - the withdrawer (`W`) is `request(false)`: store `REQUESTED`
//   `Relaxed`, then `kick()` (`SEQ.fetch_add(1, SeqCst)` and a futex
//   wake);
// - a concurrent waker (`K`, with `WAKER`) is `wake` with `present()`
//   true: another kick, an RMW of `SEQ` between `W`'s and `B`'s accesses.
//
// Checked (runs with `+live`): `B` returns in every execution; a `B`
// asleep on `SEQ`'s last value is a reported liveness violation. The
// controls are loom's (`WITHDRAW` 1: no kick, which leaves a `B` that
// already passed its `REQUESTED` check asleep, and a `Relaxed` `SEQ`
// load in `B`, which can read the kick's value with the old `REQUESTED`
// and wait on that value with no kick to come) and a kick before the
// `REQUESTED` store (`WITHDRAW` 2), which fails the same way.
//
// The no-kick control fails only because prelude.rs's `futex_wait`
// passes the word through `black_box`. Without it, `SEQ`, a private
// static nothing writes in that case, is constant to rustc at
// `-Copt-level=1`: the IR has no `SEQ` at all, `B`'s wait is
// `await_until!` with no load (`__VERIFIER_spin_end(false)`), and GenMC
// explores 1 execution, reports no error and counts no blocked one
// (also with `--disable-load-annotation`), so the run passes.
//
// The seam ledger cites this template's pass cases (verus/SEAMS.md,
// "Status") for `background::main` (`src/purge/background.rs`), whose
// loop the harness copies (pinned by tests/loom/tests/pins.rs). It is
// evidence only: a copy runs, not the body, over prelude.rs's futex
// stand-in, and no case checks the seam's contract.
//
// Left out, as in the loom model: the state lock `W` stores `REQUESTED`
// under and the one `B` takes to clear `present` on its way out (after
// its decision), and a `request(true)` after the withdrawal.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;

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
    if WITHDRAW == 2 {
        kick();
    }
    REQUESTED.store(false, Ordering::Relaxed);
    if WITHDRAW == 0 {
        kick();
    }
}

/// `background::main`'s loop.
fn background_main() {
    let mut spurious = SPURIOUS;
    loop {
        let seq = SEQ.load(SEQ_LOAD);
        if !REQUESTED.load(Ordering::Relaxed) {
            return;
        }
        futex_wait(&SEQ, seq, &mut spurious);
    }
}

fn harness() {
    let b = thread::spawn(background_main);
    let k = if WAKER {
        Some(thread::spawn(kick))
    } else {
        None
    };
    withdraw();
    if let Some(k) = k {
        k.join().unwrap();
    }
    b.join().unwrap();
}
