//! Controls for the loom the models run under: loom 0.7.2 with
//! `verus/loom/loom-0.7.2-dpor.patch`, which `verus/loom/run.sh` applies.
//!
//! Stock loom 0.7.2 races an atomic store or read-modify-write only
//! against the atomic's last access. When that access is the storing
//! thread's own load, an earlier load by another thread is never raced,
//! so the store never runs ahead of it. Each test below needs exactly
//! that order to find its race; stock loom passes both after one
//! execution, so these fail (do not panic) unless the patch is in.

use super::model;
use crate::sync::atomic::{AtomicU32, Ordering};
use loom::cell::UnsafeCell;
use loom::thread;
use std::boxed::Box;

struct Pub {
    flag: AtomicU32,
    data: UnsafeCell<u64>,
}

// SAFETY: the race on `data` is what the tests look for.
unsafe impl Sync for Pub {}

/// `T` loads the flag, writes the data and sets the flag with `set`
/// (`Relaxed`, so nothing is published); the main thread loads the flag
/// first and reads the data if it saw it set. The race needs `T`'s store
/// to run before the main thread's load.
fn unpublished(set: fn(&AtomicU32)) {
    model(move || {
        let p: &'static Pub = Box::leak(Box::new(Pub {
            flag: AtomicU32::new(0),
            data: UnsafeCell::new(0),
        }));
        let t = thread::spawn(move || {
            let _ = p.flag.load(Ordering::Relaxed);
            // SAFETY: the race is the point.
            p.data.with_mut(|d| unsafe { *d = 1 });
            set(&p.flag);
        });
        if p.flag.load(Ordering::Acquire) == 1 {
            // SAFETY: the race is the point.
            let _ = p.data.with(|d| unsafe { *d });
        }
        t.join().unwrap();
    });
}

/// A store run ahead of another thread's earlier load.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_dpor_store_after_foreign_load() {
    unpublished(|f| f.store(1, Ordering::Relaxed));
}

/// A read-modify-write run ahead of another thread's earlier load.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_dpor_rmw_after_foreign_load() {
    unpublished(|f| {
        f.fetch_add(1, Ordering::Relaxed);
    });
}
