//! The background thread's wake-up (`src/purge/background.rs`,
//! `src/purge.rs`), a copy.
//!
//! Three threads:
//!
//! - the free (`F`): a class's first slab goes `Empty`.
//!   `Decayer::note_first_empty` stores `oldest_empty` `Relaxed`, then
//!   `note_empty` calls `background::wake`, which kicks the thread only
//!   if `present()` (`PRESENT`, `Relaxed`);
//! - the starter (`S`): `background::start` creates the thread, stores
//!   `PRESENT` `Relaxed`, then kicks it (`kick`: `SEQ.fetch_add(SeqCst)`
//!   and a futex wake);
//! - the thread (`B`): `background::main` loads `SEQ` `SeqCst`, runs
//!   `decay`, which loads `oldest_empty` `Relaxed` (`Decayer::decay`) and
//!   computes no deadline when it is 0, then waits on `SEQ` with no
//!   timeout.
//!
//! The property: `B` sees the `Empty` slab (`oldest_empty != 0`) in some
//! pass. A `B` asleep without a deadline once `F` and `S` are done is a
//! lost wake-up, and the class's `Empty` slabs stay resident until
//! something else wakes it (another class's first `Empty`, a decay
//! period change). loom reports it as a deadlock, since the main thread
//! joins `B`.
//!
//! One transition is modelled, a class's first `Empty` with the thread
//! just started. A later one (a walk stored 0, then a free stores a time)
//! is the same pair of fences with `start` long past, and the huge
//! allocator's `swap` is the same `Relaxed` store to the model.
//!
//! `F` stores `oldest_empty` and loads `PRESENT`; `S` stores `PRESENT`
//! and, through the kick, `B` loads `oldest_empty`. That is the
//! store-buffering shape: without a `SeqCst` fence on each side, both
//! loads may read the old values (`F` sees `PRESENT == false` and does
//! not kick; `B`, woken by `S`'s kick, sees `oldest_empty == 0`).
//!
//! Left out: the class lock `F` releases before `wake` and the state lock
//! `S` stores `PRESENT` under. Each is taken by one side only, and a
//! `Release` unlock does not keep a later load behind an earlier store,
//! so they close nothing in C11. On x86_64 the unlock is an `xchg`, a
//! full barrier, so the lost wake-up needs aarch64 (or a compiler that
//! moves the accesses) to happen on hardware.

use crate::futex::{wait, wake};
use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use loom::thread;

#[derive(Clone, Copy)]
struct Variant {
    /// `wake`'s `fence(SeqCst)`, between the caller's `oldest_empty`
    /// store and the `PRESENT` load.
    fence_free: bool,
    /// `start`'s `fence(SeqCst)`, between the `PRESENT` store and the
    /// kick.
    fence_start: bool,
}

/// The code as it is.
const TREE: Variant = Variant {
    fence_free: true,
    fence_start: true,
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

    /// The free: `Decayer::note_first_empty(now)`, then `note_empty`
    /// (`background::wake`).
    fn free(&self, v: Variant) {
        if self.oldest_empty.load(Ordering::Relaxed) != 0 {
            return;
        }
        self.oldest_empty.store(1, Ordering::Relaxed);
        if v.fence_free {
            fence(Ordering::SeqCst);
        }
        if self.present.load(Ordering::Relaxed) {
            self.kick();
        }
    }

    /// `background::main`'s loop, ending at the first pass that sees the
    /// `Empty` slab (where the real thread computes a deadline).
    fn thread(&self) {
        loop {
            let seq = self.seq.load(Ordering::SeqCst);
            if self.oldest_empty.load(Ordering::Relaxed) != 0 {
                return;
            }
            wait(&self.seq, seq);
        }
    }

    /// `background::start` from the thread's creation on.
    fn start(&'static self, v: Variant) -> thread::JoinHandle<()> {
        let b = thread::spawn(move || self.thread());
        self.present.store(true, Ordering::Relaxed);
        if v.fence_start {
            fence(Ordering::SeqCst);
        }
        self.kick();
        b
    }
}

fn run(v: Variant) {
    loom::model(move || {
        // Leaked, not an `Arc`: a lost wake-up ends the execution with
        // `B` parked, and dropping a loom object while that panic unwinds
        // aborts the test binary.
        let st: &'static Statics = Box::leak(Box::default());
        let starter = thread::spawn(move || st.start(v));
        st.free(v);
        starter.join().unwrap().join().unwrap();
    });
}

/// The wake-up as the code has it: `B` always sees the `Empty` slab.
#[test]
fn loom_background_wake() {
    run(TREE);
}

/// Without the fences, the store-buffering outcome is reachable: `F`
/// reads `PRESENT == false`, and `B`, woken by `S`'s kick, reads
/// `oldest_empty == 0` and sleeps for good.
#[test]
#[should_panic(expected = "deadlock")]
fn loom_background_wake_without_fences() {
    run(Variant {
        fence_free: false,
        fence_start: false,
    });
}

/// `F`'s fence alone is not enough.
#[test]
#[should_panic(expected = "deadlock")]
fn loom_background_wake_with_free_fence_only() {
    run(Variant {
        fence_free: true,
        fence_start: false,
    });
}

/// `S`'s fence alone is not enough.
#[test]
#[should_panic(expected = "deadlock")]
fn loom_background_wake_with_start_fence_only() {
    run(Variant {
        fence_free: false,
        fence_start: true,
    });
}
