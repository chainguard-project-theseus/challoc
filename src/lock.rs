//! Mutual exclusion for the slow paths.
//!
//! Design.md section 9 names the class locks, the huge lock, the metadata
//! lock and the background thread's state lock. They are all this type: a
//! futex mutex in the shape of Drepper's "mutex, take 3". One `u32` is
//! `UNLOCKED`, `LOCKED` (no waiter) or `CONTENDED` (a waiter may sleep). The
//! uncontended acquire is one CAS and the uncontended release one swap;
//! contended lockers spin briefly, then sleep in `futex(FUTEX_WAIT)`, and a
//! release only issues `FUTEX_WAKE` when the state says someone may be
//! sleeping.
//!
//! Off Linux (host unit tests) the sleep degrades to a yield so the modules
//! built on it can still be tested with `cargo test --features std`.
//!
//! Nothing on the `malloc`/`free` fast path takes a lock.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
const CONTENDED: u32 = 2;

/// Spins before the first sleep. Slow-path critical sections are short
/// (a few dozen instructions), so a holder is usually gone within this.
const SPINS: u32 = 100;

/// The bare mutex word. Use [`Lock`] when there is data to protect; use this
/// directly only where the data is elsewhere (fork handlers, state machines).
#[repr(transparent)]
pub struct RawLock {
    state: AtomicU32,
}

impl Default for RawLock {
    fn default() -> Self {
        Self::new()
    }
}

impl RawLock {
    pub const fn new() -> RawLock {
        RawLock { state: AtomicU32::new(UNLOCKED) }
    }

    /// Acquire without blocking. `true` on success.
    #[inline]
    pub fn try_lock(&self) -> bool {
        self.state
            .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// Acquire, blocking as needed.
    #[inline]
    pub fn lock(&self) {
        if !self.try_lock() {
            self.lock_slow();
        }
    }

    #[cold]
    fn lock_slow(&self) {
        for _ in 0..SPINS {
            if self.state.load(Ordering::Relaxed) == UNLOCKED && self.try_lock() {
                return;
            }
            core::hint::spin_loop();
        }
        // Announce a possible waiter, then sleep while the word stays
        // CONTENDED. A spurious wake or `EINTR` just loops: `swap` re-checks.
        let mut c = self.state.swap(CONTENDED, Ordering::Acquire);
        while c != UNLOCKED {
            wait(&self.state, CONTENDED);
            c = self.state.swap(CONTENDED, Ordering::Acquire);
        }
    }

    /// Release. Must be called by the holder exactly once per acquire.
    #[inline]
    pub fn unlock(&self) {
        if self.state.swap(UNLOCKED, Ordering::Release) == CONTENDED {
            wake(&self.state);
        }
    }

    /// Whether some thread currently holds the lock. Diagnostic only.
    #[inline]
    pub fn is_locked(&self) -> bool {
        self.state.load(Ordering::Relaxed) != UNLOCKED
    }
}

#[cfg(target_os = "linux")]
#[inline]
fn wait(state: &AtomicU32, val: u32) {
    // SAFETY: `state` is a live, aligned `u32` for the duration of the call.
    // Every error (EAGAIN: already changed; EINTR: signal) means "re-check".
    let _ = unsafe { crate::sys::futex_wait(state.as_ptr(), val, None) };
}

#[cfg(target_os = "linux")]
#[inline]
fn wake(state: &AtomicU32) {
    // SAFETY: `state` is a live, aligned `u32`.
    let _ = unsafe { crate::sys::futex_wake(state.as_ptr(), 1) };
}

#[cfg(not(target_os = "linux"))]
#[inline]
fn wait(_state: &AtomicU32, _val: u32) {
    #[cfg(feature = "std")]
    std::thread::yield_now();
    core::hint::spin_loop();
}

#[cfg(not(target_os = "linux"))]
#[inline]
fn wake(_state: &AtomicU32) {}

/// A [`RawLock`] guarding a value.
pub struct Lock<T> {
    raw: RawLock,
    value: UnsafeCell<T>,
}

/// Alignment that keeps a lock (or any hot atomic) off the cache line of
/// read-mostly data next to it. 128 bytes covers Apple silicon's 128-byte
/// lines and Intel's adjacent-line prefetcher pairs; x86_64 and most aarch64
/// servers have 64-byte lines, which 128 also satisfies.
pub const PAD_ALIGN: usize = 128;

/// `T` on its own [`PAD_ALIGN`]-byte line: nothing placed before or after a
/// `Padded<T>` in a struct or array shares a cache line with it.
#[repr(align(128))]
pub struct Padded<T>(pub T);

const _: () = assert!(core::mem::align_of::<Padded<u8>>() == PAD_ALIGN);

impl<T> Deref for Padded<T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Padded<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

// SAFETY: access to `value` is serialised by `raw`, so sharing the lock
// across threads is sound whenever the value could be moved between them.
unsafe impl<T: Send> Sync for Lock<T> {}
unsafe impl<T: Send> Send for Lock<T> {}

impl<T> Lock<T> {
    pub const fn new(value: T) -> Lock<T> {
        Lock { raw: RawLock::new(), value: UnsafeCell::new(value) }
    }

    /// Acquire and return a guard that releases on drop.
    #[inline]
    pub fn lock(&self) -> LockGuard<'_, T> {
        self.raw.lock();
        LockGuard { lock: self }
    }

    /// Acquire without blocking.
    #[inline]
    pub fn try_lock(&self) -> Option<LockGuard<'_, T>> {
        if self.raw.try_lock() { Some(LockGuard { lock: self }) } else { None }
    }

    /// The underlying word, for code that must hold several locks across a
    /// region with no guard to keep (the fork handlers).
    #[inline]
    pub fn raw(&self) -> &RawLock {
        &self.raw
    }

    /// Access the value without locking.
    ///
    /// # Safety
    /// The caller must guarantee no other thread can touch the value: it
    /// holds the raw lock, or the process is single-threaded (a fork child).
    #[inline]
    pub unsafe fn get_unlocked(&self) -> &mut T {
        // SAFETY: exclusivity is the caller's promise.
        unsafe { &mut *self.value.get() }
    }
}

/// Exclusive access to a [`Lock`]'s value; unlocks on drop.
pub struct LockGuard<'a, T> {
    lock: &'a Lock<T>,
}

impl<T> Deref for LockGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: the guard exists only while the raw lock is held.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for LockGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard exists only while the raw lock is held, and
        // `&mut self` makes this the only reference derived from it.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for LockGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.lock.raw.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    #[test]
    fn raw_lock_state_machine() {
        let l = RawLock::new();
        assert!(!l.is_locked());
        assert!(l.try_lock());
        assert!(l.is_locked());
        assert!(!l.try_lock());
        l.unlock();
        assert!(!l.is_locked());
        assert_eq!(l.state.load(Ordering::Relaxed), UNLOCKED);
        l.lock();
        assert!(!l.try_lock());
        l.unlock();
        assert!(!l.is_locked());
    }

    #[test]
    fn guard_releases_on_drop() {
        let l = Lock::new(5u32);
        {
            let mut g = l.lock();
            *g += 1;
            assert!(l.try_lock().is_none());
            assert!(l.raw().is_locked());
        }
        assert!(!l.raw().is_locked());
        assert_eq!(*l.lock(), 6);
        assert_eq!(*l.try_lock().unwrap(), 6);
    }

    /// Many threads hammer one counter: every increment must land, and the
    /// lock must end up unlocked with no waiter bit stuck.
    #[test]
    fn contended_counter_is_exact() {
        const THREADS: u64 = 8;
        const ITERS: u64 = 50_000;
        let l = Lock::new(0u64);
        std::thread::scope(|s| {
            for _ in 0..THREADS {
                s.spawn(|| {
                    for _ in 0..ITERS {
                        *l.lock() += 1;
                    }
                });
            }
        });
        assert_eq!(*l.lock(), THREADS * ITERS);
        assert_eq!(l.raw().state.load(Ordering::Relaxed), UNLOCKED);
    }

    /// Long holds force the futex path (spinning cannot outlast them) and
    /// every waiter must still get through.
    #[test]
    fn sleepers_are_woken() {
        let l = Lock::new(Vec::new());
        std::thread::scope(|s| {
            for t in 0..4u32 {
                let l = &l;
                s.spawn(move || {
                    for i in 0..20u32 {
                        let mut g = l.lock();
                        g.push(t * 100 + i);
                        std::thread::sleep(std::time::Duration::from_micros(200));
                    }
                });
            }
        });
        {
            let v = l.lock();
            assert_eq!(v.len(), 80);
            for t in 0..4u32 {
                let mine: Vec<u32> = v.iter().copied().filter(|x| x / 100 == t).collect();
                assert_eq!(mine, (0..20).map(|i| t * 100 + i).collect::<Vec<_>>());
            }
        }
        assert_eq!(l.raw().state.load(Ordering::Relaxed), UNLOCKED);
    }
}
