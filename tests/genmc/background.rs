// GenMC port of tests/loom/src/background.rs (P3, the background
// thread's wake-up): copies of `Decayer::note_first_empty` and
// `Decayer::due` (src/purge.rs), and of `background::wake`, `kick`,
// `start` from the thread's creation on and `main`'s loop
// (src/purge/background.rs), as in the loom model. The futex is
// prelude.rs's stand-in. Three threads:
//
// - the free (`F`): a class's first slab goes `Empty`; `note_first_empty`
//   stores `oldest_empty` `Relaxed`, then `note_empty` calls `wake`, whose
//   `fence(SeqCst)` comes before its `PRESENT` load (`Relaxed`), and
//   which kicks only if `PRESENT` is set;
// - the starter (`S`): `start` creates the thread, stores `PRESENT`
//   `Relaxed`, then `fence(SeqCst)` and a kick (`SEQ.fetch_add(SeqCst)`,
//   then a futex wake);
// - the thread (`B`): `main` loads `SEQ` `SeqCst`, then `decay` loads
//   `oldest_empty` `Relaxed` (`due`), which finds no deadline when it is
//   0, and the thread waits on `SEQ` with no timeout.
//
// Checked (runs with `+live`): `B` returns, having seen the `Empty`
// slab. A `B` asleep on `SEQ`'s last value is a lost wake-up, which
// GenMC reports as a liveness violation. That is the store-buffering
// shape (`F` stores `oldest_empty` and loads `PRESENT`; `S` stores
// `PRESENT` and, through the kick, `B` loads `oldest_empty`), which the
// two fences close under RC11, IMM and ARMv8 alike; the controls drop
// one fence or both.
//
// The seam ledger cites this template's pass cases (verus/SEAMS.md,
// "Status") for `background::main` (`src/purge/background.rs`), whose
// loop the harness copies (pinned by tests/loom/tests/pins.rs). It is
// evidence only: a copy runs, not the body, over prelude.rs's futex
// stand-in, and no case checks the seam's contract.
//
// Left out, as in the loom model: the class lock `F` releases before
// `wake` and the state lock `S` stores `PRESENT` under (each is taken by
// one side only, so they close nothing), the deadline when `oldest_empty`
// is set (the thread returns there instead), and a later first `Empty`
// with `start` long past (the same pair of fences).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::thread;

/// `background::PRESENT`.
static PRESENT: AtomicBool = AtomicBool::new(false);
/// `background::SEQ`.
static SEQ: AtomicU32 = AtomicU32::new(0);
/// The class's `Decayer::oldest_empty`.
static OLDEST_EMPTY: AtomicU64 = AtomicU64::new(0);

/// `background::kick`.
fn kick() {
    SEQ.fetch_add(1, Ordering::SeqCst);
    futex_wake(&SEQ);
}

/// `background::wake`, with `present()` inlined.
fn wake() {
    if FENCE_FREE {
        fence(Ordering::SeqCst);
    }
    if PRESENT.load(Ordering::Relaxed) {
        kick();
    }
}

/// `Decayer::note_first_empty(now)`, with `now` 1.
fn note_first_empty() -> bool {
    if OLDEST_EMPTY.load(Ordering::Relaxed) != 0 {
        return false;
    }
    OLDEST_EMPTY.store(1, Ordering::Relaxed);
    true
}

/// The free: the class's first `Empty` slab, then `purge::note_empty`.
fn free() {
    if note_first_empty() {
        wake();
    }
}

/// `background::main`'s loop, ending at the first pass that sees the
/// `Empty` slab (where the real thread computes a deadline).
fn background_main() {
    let mut spurious = SPURIOUS;
    loop {
        let seq = SEQ.load(Ordering::SeqCst);
        // `decay`'s `Decayer::due`: no `Empty` slab, no deadline.
        let oldest = OLDEST_EMPTY.load(Ordering::Relaxed);
        if oldest != 0 {
            return;
        }
        futex_wait(&SEQ, seq, &mut spurious);
    }
}

/// `background::start` from the thread's creation on.
fn start() {
    create();
    PRESENT.store(true, Ordering::Relaxed);
    if FENCE_START {
        fence(Ordering::SeqCst);
    }
    kick();
}

fn harness() {
    let b = thread::spawn(created(background_main));
    let s = thread::spawn(start);
    free();
    s.join().unwrap();
    b.join().unwrap();
}
