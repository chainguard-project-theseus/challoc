//! The futex under loom, which has none: a loom mutex and condition
//! variable. The kernel compares the word with `val` and queues the
//! waiter under the futex hash-bucket lock, which the waker takes too, so
//! a wake that follows the store the waiter's compare missed cannot pass
//! it by; the mutex gives the same order here. A wake reaches one waiter
//! (`notify_one`), as `FUTEX_WAKE` with `n == 1` does. One condition
//! variable serves one word, so a model may use one futex word (checked).

use loom::sync::atomic::{AtomicU32, Ordering};
use loom::sync::{Condvar, Mutex, MutexGuard};

loom::lazy_static! {
    /// The bucket, holding the address of the model's futex word.
    static ref BUCKET: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());
}

/// Lock the bucket for `word`, the model's one futex word.
fn bucket(word: &AtomicU32) -> (MutexGuard<'static, usize>, &'static Condvar) {
    let (m, cv) = &*BUCKET;
    let mut g = m.lock().unwrap();
    let addr = core::ptr::from_ref(word) as usize;
    if *g == 0 {
        *g = addr;
    }
    assert_eq!(*g, addr, "the futex stand-in models one futex word");
    (g, cv)
}

/// `FUTEX_WAIT` with no timeout: sleep while `*word == val`. The compare
/// is `Relaxed`, as the kernel's read is no C11 access.
pub(crate) fn wait(word: &AtomicU32, val: u32) {
    let (g, cv) = bucket(word);
    if word.load(Ordering::Relaxed) == val {
        drop(cv.wait(g).unwrap());
    }
}

/// `FUTEX_WAKE` of one waiter.
pub(crate) fn wake(word: &AtomicU32) {
    let (_g, cv) = bucket(word);
    cv.notify_one();
}
