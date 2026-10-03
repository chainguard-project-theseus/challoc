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
//! The spin between two polls of the word is `core::hint::spin_loop`
//! (`pause` on x86_64, `isb` on aarch64; loom's under `cfg(loom)`, through
//! `crate::sync::hint`); after [`SPINS`] rounds the waiter falls through
//! to the futex (design 5).
//!
//! Nothing on the `malloc`/`free` fast path takes a lock.

use crate::sync::atomic::{AtomicU32, Ordering};
#[cfg(not(loom))]
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use vstd::prelude::*;

verus! {

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
const CONTENDED: u32 = 2;

/// Spins before the first sleep. Slow-path critical sections are short
/// (a few dozen instructions), so a holder is usually gone within this.
/// One round is one poll of the word and one `pause` (or `isb`); the
/// phase is 5..15 us.
#[cfg(not(loom))]
const SPINS: u32 = 100;
/// Under loom each spin round is a branch of every explored execution;
/// two keep the spin-then-sleep path reachable at a small cost.
#[cfg(loom)]
const SPINS: u32 = 2;

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
    #[cfg(not(loom))]
    pub const fn new() -> RawLock {
        RawLock { state: AtomicU32::new(UNLOCKED) }
    }

    /// The bare CAS from `UNLOCKED` to `LOCKED`.
    #[inline]
    fn try_acquire(&self) -> bool {
        self.state
            .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// Acquire without blocking. `true` on success.
    #[inline]
    #[cfg(test)]
    pub fn try_lock(&self) -> bool {
        self.try_acquire()
    }

    /// Acquire, blocking as needed.
    #[inline]
    pub fn lock(&self) {
        if !self.try_acquire() {
            self.lock_slow();
        }
    }

    /// Termination is not claimed: the wait ends when another thread
    /// releases the lock.
    #[cold]
    #[verifier::exec_allows_no_decreases_clause]
    fn lock_slow(&self) {
        for _ in 0..SPINS {
            if self.state.load(Ordering::Relaxed) == UNLOCKED && self.try_acquire() {
                return;
            }
            crate::sync::hint::spin_loop();
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

    /// [`RawLock::lock`], returning the hold as a [`LockHeld`] token, for
    /// code that holds several raw locks with no guard (the fork
    /// handlers).
    #[inline(always)]
    pub fn lock_held(&self) -> (t: Tracked<LockHeld>)
        ensures
            t@.held() == *self,
    {
        self.lock();
        Tracked(LockHeld { lock: *self })
    }

    /// [`RawLock::unlock`] by the holder, which hands its token back.
    #[inline(always)]
    pub fn unlock_held(&self, Tracked(t): Tracked<LockHeld>)
        requires
            t.held() == *self,
    {
        self.unlock();
    }

    /// Whether some thread currently holds the lock. Diagnostic only.
    #[inline]
    #[cfg(test)]
    pub fn is_locked(&self) -> bool {
        self.state.load(Ordering::Relaxed) != UNLOCKED
    }
}

/// Ghost: its owner holds the raw lock [`LockHeld::held`].
/// [`RawLock::lock_held`] makes one after its `lock` returns, and
/// [`RawLock::unlock_held`] takes it back as it unlocks. The field is
/// private to this module, so outside it only `lock_held` builds one;
/// the other source is the fork handlers' handoff axioms
/// (`api::axiom_fork_resumed_state`, `api::axiom_fork_resumed`), which
/// restate across the C calls the tokens `fork_prepare` ended with. The
/// token is not `Copy`, so each token is handed back at most once. A
/// token names its lock by value (Verus has no addresses of
/// references): a proof matches it to the accessor path it was taken
/// through, not to an address. That `lock` returns only once this thread
/// holds the lock, and that no other thread does until `unlock`, is the
/// futex mutex's contract, which Verus does not check (the atomics
/// return any value): the token records the calls; the mutex makes them
/// a hold.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct LockHeld {
    ghost lock: RawLock,
}

impl LockHeld {
    /// The lock this token holds.
    pub closed spec fn held(self) -> RawLock {
        self.lock
    }
}

// The futex calls; neither returns anything the callers use. Under loom,
// the stand-ins in `loom_tests::futex`.
#[cfg(not(loom))]
#[inline]
fn wait(state: &AtomicU32, val: u32) {
    // SAFETY: `state` is a live, aligned `u32` for the duration of the call.
    // Every error (EAGAIN: already changed; EINTR: signal) means "re-check".
    let _ = unsafe { crate::sys::futex_wait(state.as_ptr(), val, None) };
}

#[cfg(not(loom))]
#[inline]
fn wake(state: &AtomicU32) {
    // SAFETY: `state` is a live, aligned `u32`.
    let _ = unsafe { crate::sys::futex_wake(state.as_ptr(), 1) };
}

} // verus!

#[cfg(loom)]
use crate::loom_tests::futex::{wait, wake};

#[cfg(loom)]
impl RawLock {
    /// loom's atomics have no `const` constructor.
    pub fn new() -> RawLock {
        RawLock {
            state: AtomicU32::new(UNLOCKED),
        }
    }
}

#[cfg(not(loom))]
verus! {

/// A [`RawLock`] guarding a value.
#[verifier::reject_recursive_types(T)]
pub struct Lock<T> {
    raw: RawLock,
    value: UnsafeCell<T>,
    /// Ghost: the value the lock was created with ([`lock_init`]).
    #[allow(dead_code)]
    init: Ghost<T>,
}

/// The invariant the value behind every `Lock<T>` keeps while unlocked.
///
/// Verified code that takes a lock relies on it: [`Lock::new`] requires it
/// of the initial value, and [`Lock::lock`] hands the value out with it.
/// Nothing checks that a guard is dropped with the invariant restored, so
/// this is an assumption about every holder of the lock: a verified holder
/// ends with the value satisfying it, through the postcondition of the
/// last call that changes the value or an explicit `assert` before the
/// guard goes out of scope; an unverified one is trusted. A verified
/// holder must not move a tracked permission out of the value and keep it
/// past the guard: the next `lock` would hand out the invariant's
/// permission again. `lock_inv` is uninterpreted; the owner of each locked
/// type states what it means for that type, and lists its unverified
/// holders (`vspec/meta.rs`, `vspec/huge.rs`, `class/mod.rs`). The
/// assumption enters at [`axiom_acquired`], and at [`Lock::get_unlocked`]
/// for a holder of the raw lock.
pub uninterp spec fn lock_inv<T>(v: T) -> bool;

/// The value a lock was created with.
pub closed spec fn lock_init<T>(l: Lock<T>) -> T {
    l.init@
}

/// What every value a lock holds keeps from the value it was created
/// with (its ghost constants, say); uninterpreted like [`lock_inv`], and
/// stated per type by the type's owner.
pub uninterp spec fn lock_frame<T>(init: T, v: T) -> bool;

/// The lock invariant's assumption where it enters for a guard (the other
/// entry is [`Lock::get_unlocked`], for a holder of the raw lock): the
/// guard of a lock this thread has just acquired holds a value that
/// satisfies [`lock_inv`] (every earlier holder restored it) and keeps
/// [`lock_frame`] of the value the lock was created with. Private, and
/// used only by [`Lock::lock`] and [`Lock::try_lock`] right after the
/// acquire, on the guard they just built: of a guard whose value has
/// since been changed it would be false, and nothing but that discipline
/// stops such a call.
axiom fn axiom_acquired<T>(l: Lock<T>, g: LockGuard<'_, T>)
    requires
        *g.lock == l,
    ensures
        lock_inv(g@),
        lock_frame(lock_init(l), g@),
;

} // verus!

verus! {

/// Alignment that keeps a lock (or any hot atomic) off the cache line of
/// read-mostly data next to it. 128 bytes covers Apple silicon's 128-byte
/// lines and Intel's adjacent-line prefetcher pairs; x86_64 and most aarch64
/// servers have 64-byte lines, which 128 also satisfies.
pub const PAD_ALIGN: usize = 128;

/// `T` on its own [`PAD_ALIGN`]-byte line: nothing placed before or after a
/// `Padded<T>` in a struct or array shares a cache line with it.
#[repr(align(128))]
pub struct Padded<T>(pub T);

impl<T> Deref for Padded<T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> (r: &T)
        ensures *r == self.0,
    {
        &self.0
    }
}

impl<T> DerefMut for Padded<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> (r: &mut T)
        ensures *r == old(self).0, final(self).0 == *final(r),
    {
        &mut self.0
    }
}

} // verus!

const _: () = assert!(core::mem::align_of::<Padded<u8>>() == PAD_ALIGN);

// SAFETY: access to `value` is serialised by `raw`, so sharing the lock
// across threads is sound whenever the value could be moved between them.
#[cfg(not(loom))]
unsafe impl<T: Send> Sync for Lock<T> {}
#[cfg(not(loom))]
unsafe impl<T: Send> Send for Lock<T> {}

#[cfg(not(loom))]
verus! {

impl<T> Lock<T> {
    pub const fn new(value: T) -> (l: Lock<T>)
        requires
            lock_inv(value),
        ensures
            lock_init(l) == value,
    {
        Lock { raw: RawLock::new(), value: UnsafeCell::new(value), init: Ghost(value) }
    }

    /// Acquire and return a guard that releases on drop.
    #[inline]
    pub fn lock(&self) -> (g: LockGuard<'_, T>)
        ensures
            lock_inv(g@),
            lock_frame(lock_init(*self), g@),
    {
        self.raw.lock();
        let g = LockGuard { lock: self };
        proof {
            axiom_acquired(*self, g);
        }
        g
    }

    /// Acquire without blocking.
    #[inline]
    #[cfg(test)]
    pub fn try_lock(&self) -> (r: Option<LockGuard<'_, T>>)
        ensures
            r matches Some(g) ==> lock_inv(g@) && lock_frame(lock_init(*self), g@),
    {
        if self.raw.try_lock() {
            let g = LockGuard { lock: self };
            proof {
                axiom_acquired(*self, g);
            }
            Some(g)
        } else {
            None
        }
    }

    /// The underlying word, for code that must hold several locks across a
    /// region with no guard to keep (the fork handlers).
    #[inline]
    pub fn raw(&self) -> (r: &RawLock)
        ensures
            *r == self.spec_raw(),
    {
        &self.raw
    }

    /// The raw lock [`Lock::raw`] returns.
    pub closed spec fn spec_raw(&self) -> RawLock {
        self.raw
    }

    /// Access the value without locking.
    ///
    /// # Safety
    /// The caller must hold the raw lock, taken through [`Lock::raw`],
    /// with no guard alive, so no other thread can touch the value. In a
    /// fork child that holds when the forking thread took the raw lock in
    /// the prepare handler; a fork child alone is not enough, since the
    /// parent's holder may have been mid-update.
    ///
    /// Trusted, as [`axiom_acquired`] is for [`Lock::lock`]: under that
    /// contract the value is one a holder left, so it satisfies
    /// [`lock_inv`] and keeps [`lock_frame`]. The caller is a holder like
    /// any other and must leave the invariant true; a verified caller ends
    /// with an explicit `assert(lock_inv(*r))`. The caller shows its hold
    /// with the raw lock's [`LockHeld`] token (ghost).
    #[inline]
    #[verifier::external_body]
    pub unsafe fn get_unlocked(&self, Tracked(held): Tracked<&LockHeld>) -> (r: &mut T)
        requires
            held.held() == self.spec_raw(),
        ensures
            lock_inv(*r),
            lock_frame(lock_init(*self), *r),
    {
        // SAFETY: exclusivity is the caller's promise.
        unsafe { &mut *self.value.get() }
    }
}

/// Exclusive access to a [`Lock`]'s value; unlocks on drop.
#[verifier::reject_recursive_types(T)]
pub struct LockGuard<'a, T> {
    lock: &'a Lock<T>,
}

impl<'a, T> View for LockGuard<'a, T> {
    type V = T;

    /// The guarded value.
    uninterp spec fn view(&self) -> T;
}

// The value is behind the `UnsafeCell`, which Verus does not model, so
// the two accessors are trusted to return the guarded value.
impl<T> Deref for LockGuard<'_, T> {
    type Target = T;
    #[inline]
    #[verifier::external_body]
    fn deref(&self) -> (r: &T)
        ensures
            *r == self@,
    {
        // SAFETY: the guard exists only while the raw lock is held.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for LockGuard<'_, T> {
    #[inline]
    #[verifier::external_body]
    fn deref_mut(&mut self) -> (r: &mut T)
        ensures
            *r == old(self)@,
            *final(r) == final(self)@,
    {
        // SAFETY: the guard exists only while the raw lock is held, and
        // `&mut self` makes this the only reference derived from it.
        unsafe { &mut *self.lock.value.get() }
    }
}

// Trusted: Verus wants a destructor that opens no invariant and cannot
// unwind, and `RawLock::unlock` is not marked either way. The body is the
// release, and claims nothing; its `no_unwind` rests on the
// `panic = "abort"` profiles (test builds unwind).
impl<T> Drop for LockGuard<'_, T> {
    #[inline]
    #[verifier::external_body]
    fn drop(&mut self)
        opens_invariants none
        no_unwind
    {
        self.lock.raw.unlock();
    }
}

} // verus!

#[cfg(not(loom))]
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

    /// One waiter spinning while the holder sleeps past the spin phase:
    /// the waiter must reach the futex (announce `CONTENDED`), stay parked
    /// while the lock is held, and be woken by the release.
    ///
    /// Every wait is bounded by `BOUND` and fails instead of hanging, and
    /// the waiter is a detached thread, not a scoped one: `thread::scope`
    /// joins its threads on the way out even when the body panicked, so
    /// an assertion that failed while the lock was still held would leave
    /// the waiter parked on a lock nobody releases and the test thread
    /// parked in the join, with the panic message held by the harness.
    /// The announce is waited for, and the lock is released before
    /// anything is asserted about it.
    #[test]
    fn a_parked_waiter_is_released() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        /// Long enough for a thread to be scheduled and woken on a host
        /// under load; a wait that outlasts it is a failure, not a delay.
        const BOUND: Duration = Duration::from_secs(10);
        /// Poll `done` every millisecond for up to `BOUND`.
        fn within_bound(mut done: impl FnMut() -> bool) -> bool {
            let deadline = Instant::now() + BOUND;
            loop {
                if done() {
                    return true;
                }
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let l = Arc::new(RawLock::new());
        l.lock();
        let waiter = {
            let l = Arc::clone(&l);
            std::thread::spawn(move || {
                l.lock();
                let acquired_at = Instant::now();
                l.unlock();
                acquired_at
            })
        };
        // The announce: the spin phase ends with the swap to `CONTENDED`.
        if !within_bound(|| l.state.load(Ordering::Relaxed) == CONTENDED) {
            l.unlock();
            panic!("the waiter did not announce itself within {BOUND:?}");
        }
        // Parked: while the lock stays held the word stays `CONTENDED`
        // and the waiter does not get through. Read, then release, then
        // assert, so a failure leaves nothing parked.
        std::thread::sleep(Duration::from_millis(20));
        let word = l.state.load(Ordering::Relaxed);
        let through = waiter.is_finished();
        let released_at = Instant::now();
        l.unlock();
        assert_eq!(word, CONTENDED, "the word changed while the lock was held");
        assert!(!through, "the waiter got through while the lock was held");
        // Released: the wake reaches the waiter. On failure the waiter
        // stays parked (a lost wake); the test fails rather than joins.
        assert!(
            within_bound(|| waiter.is_finished()),
            "the waiter was not woken within {BOUND:?} of the release"
        );
        let acquired_at = waiter.join().unwrap();
        assert!(
            acquired_at >= released_at,
            "acquired {:?} before the release",
            released_at - acquired_at
        );
        assert_eq!(l.state.load(Ordering::Relaxed), UNLOCKED);
    }
}
