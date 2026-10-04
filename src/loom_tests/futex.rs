//! The futex under loom, which has none (`lock.rs` uses these in place of
//! `sys::futex_wait` and `sys::futex_wake`): a loom mutex and condition
//! variable. The kernel compares the word with `val` and queues the
//! waiter under the futex hash-bucket lock, which the waker takes too, so
//! a wake that follows the store the waiter's compare missed cannot pass
//! it by; the mutex gives the same order here. A wake reaches one waiter
//! (`notify_one`), as `FUTEX_WAKE` with `n == 1` does at both call sites:
//! waking more would hide a lost wake-up. One condition variable serves
//! one word, so a model may use one futex word (checked): with two, a
//! wake could reach a waiter on the other word.

use crate::sync::atomic::{AtomicU32, Ordering};
use loom::sync::{Condvar, Mutex, MutexGuard};

loom::lazy_static! {
    /// The bucket, holding the address of the model's futex word.
    static ref BUCKET: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());
}

/// Lock the bucket for `state`, the model's one futex word.
fn bucket(state: &AtomicU32) -> (MutexGuard<'static, usize>, &'static Condvar) {
    let (m, cv) = &*BUCKET;
    let mut g = m.lock().unwrap();
    let addr = core::ptr::from_ref(state) as usize;
    if *g == 0 {
        *g = addr;
    }
    assert_eq!(*g, addr, "the futex stand-in models one futex word");
    (g, cv)
}

/// `FUTEX_WAIT`: sleep while `*state == val`. The compare is `Relaxed`,
/// as the kernel's read is no C11 access. The mutex does add an edge: a
/// woken waiter re-takes it in `cv.wait`, so everything the waker did
/// before its `wake` happens before the waiter's return. The callers do
/// not rely on that (each re-checks the word with its own acquiring
/// access after a wait), but on a path through a wake the models cannot
/// show they do not: an ordering missing from that re-check would be
/// supplied here. Linux's futex orders a wake before the woken thread's
/// return in practice (the hash-bucket lock and the scheduler's wake-up
/// barriers) but documents no memory ordering.
pub(crate) fn wait(state: &AtomicU32, val: u32) {
    let (g, cv) = bucket(state);
    if state.load(Ordering::Relaxed) == val {
        drop(cv.wait(g).unwrap());
    }
}

/// `FUTEX_WAKE` of one waiter.
pub(crate) fn wake(state: &AtomicU32) {
    let (_g, cv) = bucket(state);
    cv.notify_one();
}
