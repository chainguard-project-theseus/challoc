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
//! The spin between two polls of the word is a [`SpinHint`]: `pause` or
//! `isb` (`core::hint::spin_loop`) everywhere, and, when the CPU probe
//! (`sys::cpu::Cpu::probe`, the one thing it acts on) says so, x86_64's
//! `umonitor`/`umwait` on the lock word or aarch64's `ldaxr`/`wfe` on it,
//! which park the waiter until the holder's store instead of re-polling
//! (design 5). A hint changes nothing about the protocol: every hint polls
//! the same word and falls through to the same futex after [`SPINS`] rounds.
//!
//! Off Linux (host unit tests) the sleep degrades to a yield so the modules
//! built on it can still be tested with `cargo test --features std`.
//!
//! Nothing on the `malloc`/`free` fast path takes a lock.

use crate::slice;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
const CONTENDED: u32 = 2;

/// Spins before the first sleep. Slow-path critical sections are short
/// (a few dozen instructions), so a holder is usually gone within this.
/// One round is one poll of the word and one [`SpinHint`] wait; with
/// `pause` the phase is 5..15 us, with `umwait` or `wfe` a round ends at
/// the holder's store (or the hint's own deadline), so the phase is bounded
/// by lock traffic rather than by a fixed number of `pause`s.
const SPINS: u32 = 100;

/// How a contended locker waits between two polls of the word during the
/// [`SPINS`] phase (design 5). Chosen once at init from the CPU probe
/// (`sys::cpu::Cpu::probe` calls [`set_spin_hint`]); until then, and on a
/// CPU without the instruction, [`SpinHint::Baseline`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SpinHint {
    /// `core::hint::spin_loop`: `pause` on x86_64, `isb` on aarch64.
    Baseline = 0,
    /// x86_64 WAITPKG (`cpuid` 7.0 `ECX[5]`): `umonitor` the lock word,
    /// then `umwait` in C0.1 until a store to its line or a short TSC
    /// deadline, whichever first. The sibling hyperthread gets the core's
    /// resources meanwhile, where `pause` only throttles the issue. GCE's
    /// hypervisor hides the bit on every CPU of the matrix (Sapphire and
    /// Emerald Rapids have it), so on GCE this reads absent and the lock
    /// runs [`SpinHint::Baseline`]; the fallback is what the C4 measures.
    Umwait = 1,
    /// aarch64, `spin_hint:wfe` only (never chosen by `auto`: on the
    /// Altra and the Axion it hands a contended lock over later than a
    /// `pause` re-poll and costs xmalloc-test 3% and a quarter of its RSS,
    /// `bench/results/2026-09-19-lane-c-spin-and-geometry.md`), under the
    /// timer event stream (`HWCAP_EVTSTRM`, the key checks it): `ldaxr`
    /// arms the exclusive monitor on the lock word and `wfe` sleeps until
    /// the holder's store clears it (Arm ARM B2.9, "the clearing of the
    /// global monitor generates a WFE wake-up event"; the pattern of
    /// Linux's `__cmpwait_case`), an interrupt, `sev`, or the event
    /// stream's next tick (10 kHz on Linux), which bounds a wait that no
    /// store ends. The load and the `wfe` leave no window: a store between
    /// them sets the event register and the `wfe` returns at once.
    Wfe = 2,
}

impl SpinHint {
    /// The hint this build can use in place of `self`: the instruction's own
    /// architecture, else [`SpinHint::Baseline`].
    pub const fn available(self) -> SpinHint {
        match self {
            SpinHint::Umwait if cfg!(target_arch = "x86_64") => SpinHint::Umwait,
            SpinHint::Wfe if cfg!(target_arch = "aarch64") => SpinHint::Wfe,
            _ => SpinHint::Baseline,
        }
    }

    fn from_u8(v: u8) -> SpinHint {
        match v {
            1 => SpinHint::Umwait,
            2 => SpinHint::Wfe,
            _ => SpinHint::Baseline,
        }
    }

    /// The key in the report and the results files.
    pub const fn name(self) -> &'static str {
        match self {
            SpinHint::Baseline => "baseline",
            SpinHint::Umwait => "umwait",
            SpinHint::Wfe => "wfe",
        }
    }
}

/// The hint every contended locker reads on entering the slow path.
static SPIN_HINT: AtomicU8 = AtomicU8::new(SpinHint::Baseline as u8);
/// Set once the configuration chose (`spin_hint:<value>`, design 12): the
/// probe's later suggestions (a second probe at init, a test's) are ignored.
static SPIN_FORCED: AtomicBool = AtomicBool::new(false);

/// The CPU probe's choice (`sys::cpu::spin_hint`), installed unless the
/// configuration chose one. Returns the hint in use afterwards.
pub fn suggest_spin_hint(hint: SpinHint) -> SpinHint {
    if !SPIN_FORCED.load(Ordering::Acquire) {
        SPIN_HINT.store(hint.available() as u8, Ordering::Relaxed);
    }
    spin_hint()
}

/// The configuration's choice (`spin_hint:baseline|umwait|wfe`): installed
/// as far as this build can run it ([`SpinHint::available`]) and kept
/// against later suggestions. The caller checks the CPU has the
/// instruction (`umwait` faults without WAITPKG). Returns the hint in use.
pub fn set_spin_hint(hint: SpinHint) -> SpinHint {
    SPIN_HINT.store(hint.available() as u8, Ordering::Relaxed);
    SPIN_FORCED.store(true, Ordering::Release);
    spin_hint()
}

/// Held by the tests that install a hint or assert the process-wide one
/// (`config::linux_tests::spin_hint_key_installs_and_umwait_needs_waitpkg`,
/// `sys::cpu::linux_tests::probe_matches_proc_cpuinfo`), so they do not
/// observe each other; the lock tests force their hint per call instead.
#[cfg(all(test, target_os = "linux"))]
pub(crate) static SPIN_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The spin hint in use.
pub fn spin_hint() -> SpinHint {
    SpinHint::from_u8(SPIN_HINT.load(Ordering::Relaxed))
}

/// One wait between two polls of `state`.
#[inline]
fn spin_once(state: &AtomicU32, hint: SpinHint) {
    match hint {
        SpinHint::Baseline => core::hint::spin_loop(),
        SpinHint::Umwait => arch::umwait(state),
        SpinHint::Wfe => arch::wfe(state),
    }
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use super::UNLOCKED;
    use core::arch::asm;
    use core::sync::atomic::{AtomicU32, Ordering};

    /// TSC ticks one `umwait` may sleep: about 0.4 us at 2.5 GHz, so a
    /// deadline wake costs what a few `pause`s would have (Cascade Lake's
    /// `pause` is 140 cycles); `IA32_UMWAIT_CONTROL` may cap it lower.
    pub const UMWAIT_TICKS: u64 = 1024;

    /// Arm the monitor on the lock word's line, re-check, and sleep until
    /// a store to the line or the deadline. Arming before the check is
    /// what closes the window: a store after the check trips the armed
    /// monitor and `umwait` returns at once.
    #[inline]
    pub fn umwait(state: &AtomicU32) {
        // SAFETY: WAITPKG is present (the probe chose this hint; both
        // instructions #UD without it). `umonitor` reads nothing and arms
        // the monitor on the line of a live, aligned word; `umwait` takes
        // the TSC deadline in edx:eax, the C-state in ecx (1 = C0.1, the
        // faster wake), and sets CF, which nothing here relies on. Neither
        // touches the stack. The `umonitor` block is not marked `nomem`
        // so the compiler cannot hoist the load above it.
        unsafe {
            asm!("umonitor {p}", p = in(reg) state.as_ptr(), options(nostack, preserves_flags));
            if state.load(Ordering::Relaxed) != UNLOCKED {
                let deadline = core::arch::x86_64::_rdtsc().wrapping_add(UMWAIT_TICKS);
                asm!("umwait ecx", in("ecx") 1u32, in("eax") deadline as u32, in("edx") (deadline >> 32) as u32, options(nostack, nomem));
            }
        }
    }

    /// Never selected on x86_64 ([`super::SpinHint::available`]).
    #[inline]
    pub fn wfe(_state: &AtomicU32) {
        core::hint::spin_loop()
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use super::UNLOCKED;
    use core::arch::asm;
    use core::sync::atomic::AtomicU32;

    /// Never selected on aarch64 ([`super::SpinHint::available`]).
    #[inline]
    pub fn umwait(_state: &AtomicU32) {
        core::hint::spin_loop()
    }

    /// Load the word exclusively (arming the PE's exclusive monitor on its
    /// line) and, if it is still held, sleep until an event. The holder's
    /// release is a store to that line, which clears the monitor and
    /// generates the event; the timer event stream bounds anything else.
    #[inline]
    pub fn wfe(state: &AtomicU32) {
        let v: u32;
        // SAFETY: an exclusive load of the live, aligned lock word; it
        // writes nothing and touches no stack. `wfe` accesses no memory.
        // The load's block is not marked `readonly` so it stays ordered
        // before the `wfe` and after the caller's poll.
        unsafe {
            asm!("ldaxr {v:w}, [{p}]", v = out(reg) v, p = in(reg) state.as_ptr(), options(nostack, preserves_flags));
            if v != UNLOCKED {
                asm!("wfe", options(nostack, nomem, preserves_flags));
            }
        }
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod arch {
    use core::sync::atomic::AtomicU32;
    #[inline]
    pub fn umwait(_state: &AtomicU32) {
        core::hint::spin_loop()
    }
    #[inline]
    pub fn wfe(_state: &AtomicU32) {
        core::hint::spin_loop()
    }
}

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

    /// The bare CAS from `UNLOCKED` to `LOCKED`.
    #[inline]
    fn try_acquire(&self) -> bool {
        self.state
            .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// Acquire without blocking. `true` on success.
    #[inline]
    pub fn try_lock(&self) -> bool {
        let ok = self.try_acquire();
        if ok {
            slice::acquired();
        }
        ok
    }

    /// Acquire, blocking as needed. Once held, the holder asks the kernel
    /// to defer a preemption until the release (`slice::acquired`, the
    /// rseq time slice extension, design 17): one relaxed load when the
    /// extension is off, two byte-sized accesses to this thread's rseq
    /// area when on. The request is made after the acquire, never during
    /// the wait: a waiter spinning while the holder is extended is what
    /// the extension exists for.
    #[inline]
    pub fn lock(&self) {
        if !self.try_acquire() {
            self.lock_slow();
        }
        slice::acquired();
    }

    #[cold]
    fn lock_slow(&self) {
        self.lock_slow_with(spin_hint())
    }

    /// [`lock`](RawLock::lock) with an explicit hint (the tests exercise
    /// every hint without touching the process-wide choice).
    #[cfg(test)]
    pub(crate) fn lock_with(&self, hint: SpinHint) {
        if !self.try_acquire() {
            self.lock_slow_with(hint);
        }
        slice::acquired();
    }

    #[cold]
    fn lock_slow_with(&self, hint: SpinHint) {
        for _ in 0..SPINS {
            if self.state.load(Ordering::Relaxed) == UNLOCKED && self.try_acquire() {
                return;
            }
            spin_once(&self.state, hint);
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
    /// The extension request is withdrawn before the word is released and,
    /// if the kernel granted one during the hold, the CPU is handed back
    /// right after (`slice::released`; a `FUTEX_WAKE` issued here already
    /// ended the grant, so the two never both run). Nested holds (the fork
    /// handlers, heap creation under the cache-table lock) request per
    /// lock: the inner release withdraws the request while the outer hold
    /// goes on unextended, which costs nothing but the extension.
    #[inline]
    pub fn unlock(&self) {
        let area = slice::acquired_area();
        slice::releasing(area);
        if self.state.swap(UNLOCKED, Ordering::Release) == CONTENDED {
            wake(&self.state);
        }
        slice::released(area);
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

    /// The hints a test may force on this machine: the baseline always;
    /// `wfe` on any aarch64 (the instruction is unprivileged and the
    /// monitor wake needs no event stream); `umwait` only where the probe
    /// installed it (it #UDs without WAITPKG).
    fn testable_hints() -> Vec<SpinHint> {
        let mut v = std::vec![SpinHint::Baseline];
        if SpinHint::Wfe.available() == SpinHint::Wfe {
            v.push(SpinHint::Wfe);
        }
        if spin_hint() == SpinHint::Umwait {
            v.push(SpinHint::Umwait);
        }
        v
    }

    #[test]
    fn spin_hint_availability_follows_the_architecture() {
        assert_eq!(SpinHint::Baseline.available(), SpinHint::Baseline);
        assert_eq!(SpinHint::Umwait.available(), if cfg!(target_arch = "x86_64") { SpinHint::Umwait } else { SpinHint::Baseline });
        assert_eq!(SpinHint::Wfe.available(), if cfg!(target_arch = "aarch64") { SpinHint::Wfe } else { SpinHint::Baseline });
        for h in [SpinHint::Baseline, SpinHint::Umwait, SpinHint::Wfe] {
            assert_eq!(SpinHint::from_u8(h as u8), h);
            assert_eq!(h.available().available(), h.available());
        }
        // The process-wide hint is whatever the probe installed (nothing,
        // off Linux); it is one this build can run.
        let h = spin_hint();
        assert_eq!(h.available(), h);
    }

    /// The wake path under each hint: threads contend on short critical
    /// sections (the spin phase does the work) and on long ones (every
    /// waiter goes through the hint's spin into the futex); nothing is
    /// lost, nothing hangs, and the word ends `UNLOCKED`.
    #[test]
    fn every_hint_wakes_under_contention() {
        for hint in testable_hints() {
            const THREADS: u64 = 8;
            const ITERS: u64 = 20_000;
            let l = Lock::new(0u64);
            std::thread::scope(|s| {
                for _ in 0..THREADS {
                    s.spawn(|| {
                        for _ in 0..ITERS {
                            l.raw.lock_with(hint);
                            // SAFETY: the raw lock is held.
                            unsafe { *l.get_unlocked() += 1 };
                            l.raw.unlock();
                        }
                    });
                }
            });
            assert_eq!(*l.lock(), THREADS * ITERS, "{hint:?}");
            assert_eq!(l.raw().state.load(Ordering::Relaxed), UNLOCKED, "{hint:?}");

            let l = Lock::new(0u32);
            std::thread::scope(|s| {
                for _ in 0..4 {
                    s.spawn(|| {
                        for _ in 0..10 {
                            l.raw.lock_with(hint);
                            std::thread::sleep(std::time::Duration::from_micros(300));
                            // SAFETY: the raw lock is held.
                            unsafe { *l.get_unlocked() += 1 };
                            l.raw.unlock();
                        }
                    });
                }
            });
            assert_eq!(*l.lock(), 40, "{hint:?}");
            assert_eq!(l.raw().state.load(Ordering::Relaxed), UNLOCKED, "{hint:?}");
        }
    }

    /// One waiter parked in the hint's spin while the holder sleeps past
    /// the spin phase: the waiter must reach the futex and be woken by the
    /// release (a `wfe` that no store ends is bounded by the event stream,
    /// and here a store does end it).
    #[test]
    fn a_parked_waiter_is_released() {
        for hint in testable_hints() {
            let l = RawLock::new();
            l.lock();
            let t0 = std::time::Instant::now();
            std::thread::scope(|s| {
                let waiter = s.spawn(|| {
                    l.lock_with(hint);
                    let held = t0.elapsed();
                    l.unlock();
                    held
                });
                std::thread::sleep(std::time::Duration::from_millis(20));
                assert_eq!(l.state.load(Ordering::Relaxed), CONTENDED, "{hint:?}: the waiter did not announce itself within 20 ms");
                l.unlock();
                let held = waiter.join().unwrap();
                assert!(held >= std::time::Duration::from_millis(20), "{hint:?}: acquired before the release ({held:?})");
                assert!(held < std::time::Duration::from_millis(2000), "{hint:?}: the wake took {held:?}");
            });
            assert_eq!(l.state.load(Ordering::Relaxed), UNLOCKED);
        }
    }

    /// The lock's own cost per hint, printed for the results file (no
    /// assertion on time: hosts differ). Uncontended pairs never spin, so
    /// the hint cannot show there; the contended figure is 8 threads on
    /// one counter, the case the spin phase exists for.
    #[test]
    fn spin_hint_costs() {
        for hint in testable_hints() {
            let l = RawLock::new();
            const N: u32 = 2_000_000;
            let t0 = std::time::Instant::now();
            for _ in 0..N {
                l.lock_with(hint);
                l.unlock();
            }
            let uncontended = t0.elapsed().as_nanos() as f64 / N as f64;

            const THREADS: u64 = 8;
            const ITERS: u64 = 100_000;
            let c = Lock::new(0u64);
            let t0 = std::time::Instant::now();
            std::thread::scope(|s| {
                for _ in 0..THREADS {
                    s.spawn(|| {
                        for _ in 0..ITERS {
                            c.raw.lock_with(hint);
                            // SAFETY: the raw lock is held.
                            unsafe { *c.get_unlocked() += 1 };
                            c.raw.unlock();
                        }
                    });
                }
            });
            let contended = t0.elapsed().as_nanos() as f64 / (THREADS * ITERS) as f64;
            assert_eq!(*c.lock(), THREADS * ITERS);
            std::println!(
                "lock: spin_hint={} uncontended {uncontended:.1} ns/pair, contended {contended:.1} ns/acquire ({THREADS} threads x {ITERS}), in use: {}",
                hint.name(),
                spin_hint().name()
            );
        }
    }
}
