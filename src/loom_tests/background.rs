//! P3: the background thread's wake-up, a copy (its atomics are statics
//! built in `const` context, which loom cannot construct). Each step
//! names the code it mirrors.
//!
//! Three threads:
//!
//! - the free (`F`): a class's first slab goes `Empty`:
//!   `Decayer::note_first_empty` (`purge.rs`) stores `oldest_empty`
//!   `Relaxed`, then `note_empty` calls `background::wake`, which kicks
//!   only if `present()` (`PRESENT`, `Relaxed`);
//! - the starter (`S`): `background::start` creates the thread, then
//!   stores `PRESENT` `Relaxed`; since chainguard-dev/challoc PR #72 it
//!   then calls `kick()` (`SEQ.fetch_add(SeqCst)`, futex wake);
//! - the thread (`B`): `background::main` loads `SEQ` `SeqCst`, runs
//!   `decay`, which reads `oldest_empty` `Relaxed` (`Decayer::decay`) and
//!   returns no deadline when it is 0, then `futex_wait`s on `SEQ` with no
//!   timeout.
//!
//! The property: `B` sees the `Empty` slab (`oldest_empty != 0`) in some
//! pass. A `B` asleep with no deadline after `F` and `S` are done is the
//! lost wake-up; loom reports it as a deadlock, since the main thread
//! joins `B`. Nothing else wakes `B` in the model; in the process it is
//! the next class's first `Empty` slab, a decay-period change or
//! `request(false)`.
//!
//! Left out: the class lock `F` releases before `wake` (`class/mod.rs`,
//! `note_empty` after the guard's drop) and the state lock `S` stores
//! `PRESENT` under. A `Release` swap does not stop a later load from
//! passing an earlier store, so they close nothing in C++20; on x86_64
//! the release is an `xchg`, a full barrier, which is why the
//! store-buffering lost wake-up needs aarch64 (or a compiler that moves
//! the accesses) to happen on hardware.

use super::futex::{wait, wake};
use super::model;
use crate::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use loom::thread;
use std::boxed::Box;

#[derive(Clone, Copy)]
struct Variant {
    /// PR #72: `start` calls `kick()` after it stores `PRESENT`.
    kick_after_present: bool,
    /// A `fence(SeqCst)` in `wake`, between the caller's `oldest_empty`
    /// store and the `PRESENT` load.
    fence_free: bool,
    /// A `fence(SeqCst)` in `start`, between the `PRESENT` store and the
    /// kick.
    fence_start: bool,
    /// The ordering of the four store-buffering accesses: `F`'s
    /// `oldest_empty` store and `PRESENT` load, `S`'s `PRESENT` store and
    /// `B`'s `oldest_empty` load (`Relaxed` as shipped).
    order: Ordering,
}

/// As shipped, with PR #72's kick.
const SHIPPED: Variant = Variant {
    kick_after_present: true,
    fence_free: false,
    fence_start: false,
    order: Ordering::Relaxed,
};

#[derive(Default)]
struct Statics {
    /// `background::PRESENT`.
    present: AtomicBool,
    /// `background::SEQ`.
    seq: AtomicU32,
    /// The class's `Decayer::oldest_empty`.
    oldest_empty: AtomicU64,
}

impl Statics {
    /// `background::kick`.
    fn kick(&self) {
        self.seq.fetch_add(1, Ordering::SeqCst);
        wake(&self.seq);
    }

    /// The free: `note_first_empty(now)`, then `note_empty` (`wake`).
    fn free(&self, v: Variant) {
        if self.oldest_empty.load(Ordering::Relaxed) == 0 {
            self.oldest_empty.store(1, v.order);
            if v.fence_free {
                fence(Ordering::SeqCst);
            }
            if self.present.load(v.order) {
                self.kick();
            }
        }
    }

    /// `background::main`'s loop, ending at the first pass that sees the
    /// `Empty` slab (where the real thread computes a deadline).
    fn thread(&self, v: Variant) {
        loop {
            let seq = self.seq.load(Ordering::SeqCst);
            if self.oldest_empty.load(v.order) != 0 {
                return;
            }
            wait(&self.seq, seq);
        }
    }

    /// `background::start` from the thread's creation on.
    fn start(&'static self, v: Variant) -> thread::JoinHandle<()> {
        let b = thread::spawn(move || self.thread(v));
        self.present.store(true, v.order);
        if v.kick_after_present {
            if v.fence_start {
                fence(Ordering::SeqCst);
            }
            self.kick();
        }
        b
    }
}

fn run(v: Variant) {
    model(move || {
        // Leaked, not an `Arc`: the lost wake-up ends the execution with
        // `B` parked, and dropping a loom object while that panic unwinds
        // aborts the test binary.
        let st: &'static Statics = Box::leak(Box::new(Statics::default()));
        let starter = thread::spawn(move || st.start(v));
        st.free(v);
        starter.join().unwrap().join().unwrap();
    });
}

/// The startup race before PR #72 (`start` does not kick):
/// `B`'s first pass reads `oldest_empty == 0`, the free's `wake` reads
/// `PRESENT == false`, and `B` sleeps for good. Reachable under SC.
#[test]
#[should_panic(expected = "deadlock")]
fn loom_background_startup_race() {
    run(Variant {
        kick_after_present: false,
        ..SHIPPED
    });
}

/// PR #72's kick alone, with the `Relaxed` accesses as shipped: the
/// store-buffering shape. `F` stores `oldest_empty` and loads `PRESENT`
/// (`false`), `S` stores `PRESENT` and kicks, and `B`, woken by that
/// kick, may still load the old `oldest_empty` (0): no edge orders `F`'s
/// store before `B`'s load. `B` sleeps for good. Not reachable under SC.
#[test]
#[should_panic(expected = "deadlock")]
fn loom_background_kick_store_buffering() {
    run(SHIPPED);
}

/// The fix: the kick with a `SeqCst` fence on each side of the
/// store-buffering pair. Under C++20 (P0668) the fences are totally
/// ordered. If `S`'s came first, `F`'s `PRESENT` load, after its own
/// fence, sees `true` and `F` kicks. If `F`'s came first, `B` sees `F`'s
/// store: a `B` load reading 0 would be coherence-ordered before that
/// store, while `S`'s fence happens before the load (`S` fences, kicks,
/// and `B` synchronises with the kick through `SEQ`), which would put
/// `S`'s fence before `F`'s. loom's `SeqCst` fence joins every fenced
/// thread's view into one clock, stronger than C++20, so this pass rests
/// on the argument; the failures below are behaviours C++20 allows too.
#[test]
fn loom_background_kick_with_fences() {
    run(Variant {
        fence_free: true,
        fence_start: true,
        ..SHIPPED
    });
}

/// One fence is not enough: `F`'s alone.
#[test]
#[should_panic(expected = "deadlock")]
fn loom_background_kick_with_free_fence_only() {
    run(Variant {
        fence_free: true,
        ..SHIPPED
    });
}

/// One fence is not enough: `S`'s alone.
#[test]
#[should_panic(expected = "deadlock")]
fn loom_background_kick_with_start_fence_only() {
    run(Variant {
        fence_start: true,
        ..SHIPPED
    });
}

/// The other fix: the four accesses `SeqCst`, no fences. C++20 forbids
/// the lost wake-up here: in the single total order of `SeqCst`
/// operations, `F`'s store precedes `F`'s `PRESENT` load, which (reading
/// `false`) precedes `S`'s `PRESENT` store, then the kick, `B`'s `SEQ`
/// load and `B`'s `oldest_empty` load, so that load cannot read the value
/// before `F`'s store. loom regards `SeqCst` accesses as `AcqRel` (its
/// README, "Unsupported features"; tokio-rs/loom#180) and reports the
/// deadlock: a false alarm, kept to show which form loom can check.
#[test]
#[ignore = "loom models SeqCst accesses as AcqRel and reports a deadlock C++20 forbids"]
fn loom_background_kick_with_seqcst_accesses() {
    run(Variant {
        order: Ordering::SeqCst,
        ..SHIPPED
    });
}
