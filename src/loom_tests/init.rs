//! P4: publishing `Shared` (`api/mod.rs`), a copy (`STATE`, `INIT_TID` and
//! `SHARED` are statics built in `const` context, which loom cannot
//! construct). Each step names the code it mirrors:
//!
//! - [`Statics::init_slow`] is `api::init_slow`: the
//!   `compare_exchange(UNINIT, INITIALISING, Acquire, Acquire)`; the
//!   winner stores its tid in `INIT_TID` `Relaxed`, runs `init` and
//!   stores `READY` `Release`; a loser that sees `READY` returns, one
//!   whose tid is `INIT_TID` aborts (re-entry), any other spins on an
//!   `Acquire` load of `STATE` with `spin_loop` until `READY`;
//! - [`Statics::shared`] is `api::shared`: an `Acquire` load of `STATE`,
//!   `shared_ref` on `READY`, `init_slow` otherwise;
//!   [`Statics::shared_if_ready`] is `api::shared_if_ready`, the `free`
//!   path's;
//! - `init` is [`Statics::init`]: plain writes of the published value
//!   (`SHARED`, `FEATURES` and the page geometry are
//!   plain memory), here one loom `UnsafeCell`; `shared_ref` is a plain
//!   read of it.
//!
//! Checked: `init` runs once; every caller that returns reads the value
//! `init` built, with no race (a reader not ordered after `init`'s writes
//! is a reported causality violation); every caller returns (a waiter
//! left spinning is a reported deadlock: loom's `spin_loop` yields, and a
//! thread that only yields while no other can run fails the model); a
//! re-entrant call from `init` aborts and never waits for itself; no other
//! thread takes the re-entry abort.
//!
//! Left out: `gettid`, here a distinct number per modelled thread (the
//! kernel's tids of live threads are distinct and never 0); what `init`
//! does besides its writes (it is verified, and it does not touch
//! `STATE`); the `Relaxed` configuration statics `init` also stores
//! (`thp`, `purge`, `SEALED`), whose readers claim nothing of their
//! values beyond what every store satisfies.

use super::model;
use crate::sync::atomic::{AtomicU32, Ordering};
use loom::cell::UnsafeCell;
use loom::thread;
use std::boxed::Box;
use std::vec::Vec;

const UNINIT: u32 = 0;
const INITIALISING: u32 = 1;
const READY: u32 = 2;

/// The value `init` builds.
const BUILT: u64 = 0x5eed;

/// `init_slow`'s outcome, with `sys::abort` as a value so the model can
/// count it.
#[derive(Debug, PartialEq)]
enum Out {
    Shared(u64),
    Abort,
}

#[derive(Clone, Copy)]
struct Variant {
    /// The ordering of `init_slow`'s `READY` store (`Release` as shipped).
    publish: Ordering,
    /// `init` itself calls `shared()` (an allocation during
    /// initialisation).
    reenter: bool,
}

const SHIPPED: Variant = Variant {
    publish: Ordering::Release,
    reenter: false,
};

struct Statics {
    /// `api::STATE`.
    state: AtomicU32,
    /// `api::INIT_TID`.
    init_tid: AtomicU32,
    /// `api::SHARED` (and the rest `init` writes).
    shared: UnsafeCell<u64>,
    /// How many times `init` ran (model bookkeeping, not in the source).
    inits: AtomicU32,
    /// What the re-entrant call returned (model bookkeeping).
    reentry: AtomicU32,
}

// SAFETY: `shared` is written by `init` and read after `READY`, which is
// what is checked.
unsafe impl Sync for Statics {}

impl Statics {
    fn new() -> Statics {
        Statics {
            state: AtomicU32::new(UNINIT),
            init_tid: AtomicU32::new(0),
            shared: UnsafeCell::new(0),
            inits: AtomicU32::new(0),
            reentry: AtomicU32::new(0),
        }
    }

    /// `api::shared_ref`: the plain read of the published value.
    fn shared_ref(&self) -> u64 {
        // SAFETY: `READY` observed by an `Acquire` load on this thread, or
        // published by it (loom reports a race if not).
        self.shared.with(|p| unsafe { *p })
    }

    /// `api::init`: build the value, here one plain write.
    fn init(&self, tid: u32, v: Variant) {
        self.inits.fetch_add(1, Ordering::Relaxed);
        // SAFETY: only the `compare_exchange` winner runs `init`, before
        // `READY` (loom reports a race if not).
        self.shared.with_mut(|p| unsafe { *p = BUILT });
        if v.reenter {
            let r = match self.shared(tid, v) {
                Out::Abort => 1,
                Out::Shared(_) => 2,
            };
            self.reentry.store(r, Ordering::Relaxed);
        }
    }

    /// `api::init_slow`.
    fn init_slow(&self, tid: u32, v: Variant) -> Out {
        match self.state.compare_exchange(
            UNINIT,
            INITIALISING,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.init_tid.store(tid, Ordering::Relaxed);
                self.init(tid, v);
                self.state.store(READY, v.publish);
            }
            Err(READY) => {}
            Err(_) => {
                if self.init_tid.load(Ordering::Relaxed) == tid {
                    return Out::Abort;
                }
                while self.state.load(Ordering::Acquire) != READY {
                    crate::sync::hint::spin_loop();
                }
            }
        }
        Out::Shared(self.shared_ref())
    }

    /// `api::shared`.
    fn shared(&self, tid: u32, v: Variant) -> Out {
        if self.state.load(Ordering::Acquire) == READY {
            return Out::Shared(self.shared_ref());
        }
        self.init_slow(tid, v)
    }

    /// `api::shared_if_ready`.
    fn shared_if_ready(&self) -> Option<u64> {
        if self.state.load(Ordering::Acquire) == READY {
            Some(self.shared_ref())
        } else {
            None
        }
    }
}

/// Leaked, not an `Arc`: a failed model ends the execution with threads
/// parked, and dropping a loom object while that panic unwinds aborts
/// the test binary.
fn leak<T>(v: T) -> &'static T {
    Box::leak(Box::new(v))
}

/// `callers` threads (tids 1..=callers) call `shared()` while the main
/// thread calls `shared_if_ready()` (a `free`). One spinning waiter at a
/// time is what loom can explore: two waiters yielding to each other
/// while the initialiser is not scheduled is a path loom cannot cut
/// short (it exceeds any branch bound).
fn run(callers: u32, v: Variant) {
    model(move || {
        let st = leak(Statics::new());
        let hs: Vec<_> = (1..=callers)
            .map(|tid| thread::spawn(move || st.shared(tid, v)))
            .collect();
        if let Some(x) = st.shared_if_ready() {
            assert_eq!(x, BUILT, "a ready reader saw another value");
        }
        for h in hs {
            assert_eq!(
                h.join().unwrap(),
                Out::Shared(BUILT),
                "a caller did not get the built value"
            );
        }
        assert_eq!(
            st.inits.load(Ordering::Relaxed),
            1,
            "init ran more than once"
        );
        if v.reenter {
            assert_eq!(
                st.reentry.load(Ordering::Relaxed),
                1,
                "re-entry did not abort"
            );
        }
    });
}

/// One caller initialises while a free races with it: `shared_if_ready`
/// returns `None` or the built value.
#[test]
fn loom_init_free_races_init() {
    run(1, SHIPPED);
}

/// Two callers race to initialise, with a free: one runs `init`, the
/// other returns the value it built (on `READY`, or after spinning on
/// `INITIALISING`), and its `Relaxed` `INIT_TID` load never matches its
/// own tid.
#[test]
fn loom_init_two_callers() {
    run(2, SHIPPED);
}

/// An allocation during `init` takes the re-entry abort, never the wait
/// (which would spin for ever on its own `INITIALISING`), while another
/// caller waits for the outer `init`.
#[test]
fn loom_init_reentry_aborts() {
    run(
        2,
        Variant {
            reenter: true,
            ..SHIPPED
        },
    );
}

/// The publication check has teeth: a `Relaxed` `READY` store lets a
/// reader see `READY` without `init`'s write.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_init_relaxed_publish_is_a_race() {
    run(
        1,
        Variant {
            publish: Ordering::Relaxed,
            ..SHIPPED
        },
    );
}

/// The termination check has teeth: a waiter on an initialiser that never
/// publishes `READY` fails the model.
#[test]
#[should_panic(expected = "maximum number of branches")]
fn loom_init_unpublished_wait_fails() {
    model(|| {
        let st = leak(Statics::new());
        st.state.store(INITIALISING, Ordering::Relaxed);
        st.init_tid.store(1, Ordering::Relaxed);
        let _ = st.init_slow(2, SHIPPED);
    });
}
