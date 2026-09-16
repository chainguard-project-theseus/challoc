//! Decay and purge (design.md section 8) and the background thread (8.1).
//!
//! Three things live here:
//!
//! - [`Batch`], the discard primitive. Callers collect up to [`MAX_BATCH`]
//!   ranges and flush them with one `process_madvise` (through `PIDFD_SELF`
//!   or a pidfd of this process, whichever the probe in `sys::features`
//!   found) or, when neither works, a loop of `madvise`. The advice is
//!   `MADV_DONTNEED`, or `MADV_FREE` under `lazy_free`. A range that fails
//!   to discard aborts: the heap's idea of what is resident would otherwise
//!   be wrong.
//! - The decay driver: [`decay`] visits every class and the huge allocator,
//!   purges what has been `Empty` for `dirty_decay_ms`, and reports the
//!   earliest next deadline. Each class keeps the `dirty_at` of its oldest
//!   `Empty` slab in an atomic so the visit takes no lock unless a purge is
//!   due. [`on_tick`] is the hook the slow path calls every
//!   `PERIODIC_EVERY` slow paths (design 6.4); [`purge_all`] is
//!   `malloc_trim(0)` and `arena.<i>.purge`.
//! - The background thread. Created on the first periodic tick after it was
//!   requested (never from init), on a stack carved from the metadata region
//!   with a `PROT_NONE` guard page below it, with every signal blocked. It
//!   loops over `decay` and sleeps on a futex until the next deadline; a
//!   class whose first slab goes `Empty` bumps the futex word so a thread
//!   sleeping with no deadline wakes up. While it exists the slow path does
//!   no purging of its own. After `fork` the child marks it absent and does
//!   not recreate it until asked again.
//!
//! `pthread_create` allocates (glibc's `allocate_dtv` calls `calloc`), so
//! the creating thread parks its TLS slot on the `TLS_CREATING` sentinel for
//! the duration and that allocation is served straight from the slabs, the
//! same path `pthread_setspecific` takes during heap creation (design 6.5).
//!
//! The `lazy_free` choice is process-wide and read by `class.rs` and
//! `huge.rs` through [`purged_is_zero`]: memory discarded with `MADV_FREE`
//! keeps its contents until the kernel reclaims it, so `Purged` slabs and
//! extents are no longer known to be zero and `calloc` clears them.

use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use crate::heap::Shared;
use crate::sys::features::ProcessMadvise;

/// Ranges per `process_madvise` (design section 8).
pub const MAX_BATCH: usize = 64;

/// `decay_ns` value meaning "never purge" (`dirty_decay_ms:-1`).
pub const NEVER: u64 = u64::MAX;

/// `dirty_decay_ms` to a nanosecond period; negative means [`NEVER`].
pub const fn decay_ns_from_ms(ms: i64) -> u64 {
    if ms < 0 {
        NEVER
    } else {
        let ns = (ms as u64).saturating_mul(1_000_000);
        if ns == NEVER { NEVER - 1 } else { ns }
    }
}

/// `BACKEND` value for "loop over `madvise`".
const LOOP: i32 = i32::MIN;
/// The kernel's `PIDFD_SELF` sentinel (Linux 6.14). Mirrored here so the
/// host build of this module type-checks; the Linux build checks it.
const PIDFD_SELF: i32 = -10000;
#[cfg(target_os = "linux")]
const _: () = assert!(PIDFD_SELF == crate::sys::PIDFD_SELF);
/// `process_madvise` target: [`LOOP`], `PIDFD_SELF`, or a pidfd.
static BACKEND: AtomicI32 = AtomicI32::new(LOOP);
/// `lazy_free`: discard with `MADV_FREE` instead of `MADV_DONTNEED`.
static LAZY: AtomicBool = AtomicBool::new(false);

/// Choose the discard backend and advice. Called once at init, before
/// anything can be purged, and again in the fork child.
pub fn configure(pm: ProcessMadvise, lazy_free: bool) {
    let backend = match pm {
        ProcessMadvise::Unavailable => LOOP,
        ProcessMadvise::PidfdSelf => PIDFD_SELF,
        ProcessMadvise::Pidfd(fd) => fd,
    };
    BACKEND.store(backend, Ordering::Relaxed);
    LAZY.store(lazy_free, Ordering::Relaxed);
}

/// Whether memory in the `Purged` state is known to read as zero. False
/// under `lazy_free`, where `MADV_FREE` leaves the old contents in place
/// until the kernel gets around to reclaiming the pages.
#[inline]
pub fn purged_is_zero() -> bool {
    !LAZY.load(Ordering::Relaxed)
}

/// Whether batches go through `process_madvise` (false: `madvise` loop).
pub fn uses_process_madvise() -> bool {
    BACKEND.load(Ordering::Relaxed) != LOOP
}

/// Up to [`MAX_BATCH`] page ranges to discard together. Callers push while
/// holding the lock that keeps the ranges free and flush before releasing
/// it; the batch never outlives that hold.
pub struct Batch {
    ranges: [(usize, usize); MAX_BATCH],
    n: usize,
}

impl Default for Batch {
    fn default() -> Self {
        Batch::new()
    }
}

impl Batch {
    pub const fn new() -> Batch {
        Batch { ranges: [(0, 0); MAX_BATCH], n: 0 }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.n
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.n == MAX_BATCH
    }

    /// Add `[addr, addr + len)`, flushing first if the batch is full.
    pub fn push(&mut self, addr: usize, len: usize) {
        if self.is_full() {
            self.flush();
        }
        self.ranges[self.n] = (addr, len);
        self.n += 1;
    }

    /// Discard every range with one syscall when the kernel allows it and a
    /// loop of `madvise` otherwise, then empty the batch. Aborts if a range
    /// cannot be discarded.
    pub fn flush(&mut self) {
        if self.n != 0 {
            os::discard(&self.ranges[..self.n]);
            self.n = 0;
        }
    }
}

/// Run every purge that is due at `now` (coarse monotonic nanoseconds) and
/// return the earliest future deadline, `None` when nothing is `Empty` or
/// decay is off. Lock-free for a class with nothing due.
pub fn decay(shared: &Shared, now: u64) -> Option<u64> {
    let decay_ns = shared.decay_ns();
    if decay_ns == NEVER {
        return None;
    }
    let mut next: Option<u64> = None;
    let mut fold = |d: Option<u64>| {
        if let Some(d) = d {
            next = Some(next.map_or(d, |n| n.min(d)));
        }
    };
    for cs in shared.classes.iter() {
        fold(cs.decay(now, decay_ns));
    }
    fold(shared.huge.decay(now, decay_ns));
    next
}

/// Purge every `Empty` slab and huge extent regardless of age. Returns how
/// many were purged.
pub fn purge_all(shared: &Shared) -> usize {
    let mut n = 0;
    for cs in shared.classes.iter() {
        n += cs.purge_empty(u64::MAX);
    }
    n + shared.huge.purge_empty(u64::MAX)
}

/// Periodic-work hook (design 6.4): start the background thread if one was
/// requested and does not exist, otherwise run decay here unless the
/// thread is doing it.
pub fn on_tick(shared: &Shared, now: u64) {
    if background::present() {
        return;
    }
    if background::requested() && background::start(shared) {
        return;
    }
    let _ = decay(shared, now);
}

/// A class or the huge allocator went from no `Empty` slab to one: wake a
/// background thread that may be sleeping without a deadline.
#[inline]
pub fn note_empty() {
    background::wake();
}

#[cfg(target_os = "linux")]
mod os {
    use super::{BACKEND, LAZY, LOOP, MAX_BATCH};
    use crate::sys::{self, IoVec, MADV_DONTNEED, MADV_FREE};
    use core::sync::atomic::Ordering;

    pub fn discard(ranges: &[(usize, usize)]) {
        let advice = if LAZY.load(Ordering::Relaxed) { MADV_FREE } else { MADV_DONTNEED };
        let backend = BACKEND.load(Ordering::Relaxed);
        if backend != LOOP {
            let mut iov = [IoVec { base: core::ptr::null(), len: 0 }; MAX_BATCH];
            let mut total = 0usize;
            for (i, &(addr, len)) in ranges.iter().enumerate() {
                iov[i] = IoVec { base: addr as *const u8, len };
                total += len;
            }
            // SAFETY: the ranges are page-aligned parts of our own mappings
            // that the caller holds free; `iov` outlives the call.
            let done = unsafe { sys::process_madvise(backend, iov.as_ptr(), ranges.len(), advice, 0) };
            if done == Ok(total) {
                return;
            }
            // A short or failed batch (a range the kernel refused, a pidfd
            // that went stale) falls back to the loop: both advices are
            // idempotent, so redoing the whole batch is harmless.
        }
        for &(addr, len) in ranges {
            // SAFETY: as above.
            if let Err(e) = unsafe { sys::madvise(addr, len, advice) } {
                panic!("purge: madvise({advice}) of {addr:#x}+{len:#x} failed: {e}");
            }
        }
    }
}

/// Off Linux nothing is mapped; host tests only exercise the bookkeeping.
#[cfg(not(target_os = "linux"))]
mod os {
    pub fn discard(_ranges: &[(usize, usize)]) {}
}

/// The background thread (design 8.1).
#[cfg(target_os = "linux")]
pub mod background {
    use core::ffi::c_void;
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use crate::heap::{Shared, TLS_CREATING, TLS_EXITED};
    use crate::lock::Lock;
    use crate::sys::{self, Timespec};

    /// Stack size including the guard page below it.
    pub const STACK_SIZE: usize = 256 * 1024;
    /// `prctl(PR_SET_NAME)`.
    const PR_SET_NAME: i32 = 15;
    /// Shortest futex timeout; the coarse clock ticks every few ms anyway.
    const MIN_SLEEP_NS: u64 = 1_000_000;

    /// Bookkeeping behind the state lock (the lock `fork_prepare` takes
    /// last, design section 9).
    struct State {
        present: bool,
        creating: bool,
    }

    static STATE: Lock<State> = Lock::new(State { present: false, creating: false });
    /// Mirror of `STATE.present` for the lock-free check on every tick.
    static PRESENT: AtomicBool = AtomicBool::new(false);
    /// `background_thread:true` (or, later, `mallctl("background_thread")`).
    static REQUESTED: AtomicBool = AtomicBool::new(false);
    /// Futex word the thread sleeps on; bumped by [`wake`].
    static SEQ: AtomicU32 = AtomicU32::new(0);

    #[inline]
    pub fn present() -> bool {
        PRESENT.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn requested() -> bool {
        REQUESTED.load(Ordering::Relaxed)
    }

    /// Ask for (or, with `false`, stop asking for) a background thread. The
    /// next periodic tick creates it; a running thread that finds the
    /// request withdrawn exits at its next wake-up.
    pub fn request(on: bool) {
        REQUESTED.store(on, Ordering::Relaxed);
        if !on {
            // Unconditional: a thread the bookkeeping has already forgotten
            // (the parent side of a fork handler test) must still notice.
            kick();
        }
    }

    /// Bump the futex word and wake the thread if it is waiting. Cheap
    /// enough for the rare "first `Empty` slab of a class" transition.
    pub fn wake() {
        if present() {
            kick();
        }
    }

    fn kick() {
        SEQ.fetch_add(1, Ordering::SeqCst);
        // SAFETY: `SEQ` is a live, aligned `u32`.
        let _ = unsafe { sys::futex_wake(SEQ.as_ptr(), 1) };
    }

    /// Create the thread if it does not exist. `true` if one exists on
    /// return; `false` if creation failed or is in progress on another
    /// thread (the caller then purges in the foreground for this tick).
    pub fn start(shared: &Shared) -> bool {
        {
            let mut g = STATE.lock();
            if g.present {
                return true;
            }
            if g.creating {
                return false;
            }
            g.creating = true;
        }
        let ok = create(shared);
        let mut g = STATE.lock();
        g.creating = false;
        g.present = ok;
        PRESENT.store(ok, Ordering::Relaxed);
        ok
    }

    fn create(shared: &Shared) -> bool {
        let page = shared.regions.page_size();
        // Reserved, then committed above the lowest page, which stays
        // `PROT_NONE` as the guard. Never freed: the thread never exits
        // while its stack could be in use (it only stops at a wake-up).
        let Some(stack) = shared.meta.reserve(STACK_SIZE, page) else { return false };
        if shared.meta.commit(stack + page, STACK_SIZE - page).is_err() {
            return false;
        }
        // SAFETY: plain pthread attribute calls on a local; the stack is
        // ours and committed. Signals are blocked so the new thread
        // inherits an empty mask and never runs an application handler.
        // The TLS slot is parked on `TLS_CREATING` because
        // `pthread_create` allocates the new thread's DTV with `calloc`,
        // which must not re-enter this thread's heap mid-operation.
        unsafe {
            let mut attr: ::libc::pthread_attr_t = core::mem::zeroed();
            if ::libc::pthread_attr_init(&mut attr) != 0 {
                return false;
            }
            let mut ok = ::libc::pthread_attr_setstack(&mut attr, (stack + page) as *mut c_void, STACK_SIZE - page) == 0
                && ::libc::pthread_attr_setdetachstate(&mut attr, ::libc::PTHREAD_CREATE_DETACHED) == 0;
            if ok {
                let saved = crate::tls::get();
                let park = saved as usize > TLS_EXITED;
                if park {
                    crate::tls::set(TLS_CREATING as *mut c_void);
                }
                let mask = sys::block_all_signals();
                let mut tid: ::libc::pthread_t = 0;
                let rc = ::libc::pthread_create(&mut tid, &attr, main, shared as *const Shared as *mut c_void);
                sys::set_signal_mask(mask);
                if park {
                    crate::tls::set(saved);
                }
                ok = rc == 0;
            }
            ::libc::pthread_attr_destroy(&mut attr);
            ok
        }
    }

    extern "C" fn main(arg: *mut c_void) -> *mut c_void {
        // SAFETY: `start` passed the process-wide `Shared`, which lives for
        // the rest of the process; the name is NUL-terminated.
        let shared = unsafe { &*(arg as *const Shared) };
        let _ = unsafe { sys::prctl(PR_SET_NAME, b"challoc-bg\0".as_ptr() as usize, 0, 0, 0) };
        loop {
            // Read the sequence before scanning, so a bump that lands
            // between the scan and the wait makes the wait return at once.
            let seq = SEQ.load(Ordering::SeqCst);
            if !requested() {
                let mut g = STATE.lock();
                g.present = false;
                PRESENT.store(false, Ordering::Relaxed);
                return core::ptr::null_mut();
            }
            let now = sys::now_coarse_ns();
            let timeout = super::decay(shared, now).map(|deadline| {
                let ns = deadline.saturating_sub(now).max(MIN_SLEEP_NS);
                Timespec { sec: (ns / 1_000_000_000) as i64, nsec: (ns % 1_000_000_000) as i64 }
            });
            // SAFETY: `SEQ` is a live, aligned `u32`; `timeout` outlives
            // the call. Every outcome (wake, EAGAIN, ETIMEDOUT, EINTR)
            // just means "look again".
            let _ = unsafe { sys::futex_wait(SEQ.as_ptr(), seq, timeout.as_ref()) };
        }
    }

    /// Take the state lock (fork prepare, after the metadata lock).
    pub fn fork_lock() {
        STATE.raw().lock();
    }

    /// Release the state lock (fork parent and child, before the others).
    pub fn fork_unlock() {
        STATE.raw().unlock();
    }

    /// Whether the state lock is held (for the fork handler tests).
    pub fn is_locked() -> bool {
        STATE.raw().is_locked()
    }

    /// The child has no background thread and, per design 8.1, does not
    /// get one until it asks again. Called with the state lock held by
    /// this thread (`fork_prepare` took it).
    pub fn fork_child() {
        // SAFETY: the forking thread holds the lock; the child is single
        // threaded.
        let st = unsafe { STATE.get_unlocked() };
        st.present = false;
        st.creating = false;
        PRESENT.store(false, Ordering::Relaxed);
        REQUESTED.store(false, Ordering::Relaxed);
    }
}

#[cfg(not(target_os = "linux"))]
pub mod background {
    use crate::heap::Shared;

    pub fn present() -> bool {
        false
    }

    pub fn requested() -> bool {
        false
    }

    pub fn request(_on: bool) {}

    pub fn wake() {}

    pub fn start(_shared: &Shared) -> bool {
        false
    }
}

/// The fork child's view of the discard backend: a pidfd inherited from the
/// parent names the parent, so it is closed and purges fall back to the
/// `madvise` loop (`PIDFD_SELF` needs no fd and stays). Returns the
/// backend the child ended up with, for the feature record.
pub fn fork_child() -> ProcessMadvise {
    let b = BACKEND.load(Ordering::Relaxed);
    if b >= 0 {
        #[cfg(target_os = "linux")]
        let _ = crate::sys::close(b);
        BACKEND.store(LOOP, Ordering::Relaxed);
        return ProcessMadvise::Unavailable;
    }
    if b == LOOP { ProcessMadvise::Unavailable } else { ProcessMadvise::PidfdSelf }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_period_from_ms() {
        assert_eq!(decay_ns_from_ms(-1), NEVER);
        assert_eq!(decay_ns_from_ms(-1000), NEVER);
        assert_eq!(decay_ns_from_ms(0), 0);
        assert_eq!(decay_ns_from_ms(10_000), 10_000_000_000);
        assert_eq!(decay_ns_from_ms(crate::config::MAX_INTERVAL_MS), 1_000_000_000_000_000_000);
        assert_ne!(decay_ns_from_ms(i64::MAX), NEVER);
    }

    /// Pages the bookkeeping test may point a batch at: a real mapping on
    /// Linux (the flush really discards), made-up addresses elsewhere.
    #[cfg(target_os = "linux")]
    fn scratch(pages: usize, page: usize) -> usize {
        use crate::sys::{self, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};
        unsafe { sys::mmap(0, pages * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) }.unwrap()
    }

    #[cfg(not(target_os = "linux"))]
    fn scratch(_pages: usize, _page: usize) -> usize {
        0x1000_0000
    }

    #[test]
    fn batch_fills_and_flushes() {
        let page = 4096;
        let base = scratch(MAX_BATCH + 2, page);
        let mut b = Batch::new();
        assert!(b.is_empty());
        for i in 0..MAX_BATCH {
            assert!(!b.is_full());
            b.push(base + page * i, page);
        }
        assert!(b.is_full());
        assert_eq!(b.len(), MAX_BATCH);
        // The 65th push flushes the first 64 first.
        b.push(base + page * MAX_BATCH, 2 * page);
        assert_eq!(b.len(), 1);
        b.flush();
        assert!(b.is_empty());
        b.flush();
        assert!(b.is_empty());
    }

    /// The default (no `lazy_free`) discards with `MADV_DONTNEED`, so
    /// purged memory is zero. `lazy_free:true` is exercised end to end by
    /// `tests/c/decay.c`; toggling the process-wide flag here would race
    /// the class and huge tests that read it.
    #[test]
    fn purged_memory_is_zero_by_default() {
        assert!(purged_is_zero());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use crate::sys::{self, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};
    use crate::sys::features::Features;

    /// Both tests below switch the process-wide backend; take turns.
    static BACKEND_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Resident pages of `[addr, addr + len)` via `mincore`.
    fn resident(addr: usize, len: usize, page: usize) -> usize {
        let n = len / page;
        let mut vec = std::vec![0u8; n];
        let rc = unsafe { ::libc::mincore(addr as *mut _, len, vec.as_mut_ptr()) };
        assert_eq!(rc, 0);
        vec.iter().filter(|&&b| b & 1 != 0).count()
    }

    /// A batch through whatever backend the kernel offers (and, when that
    /// is `process_madvise`, again through the loop) discards every range
    /// and leaves the pages zero.
    #[test]
    fn batch_discards_through_the_probed_backend_and_the_loop() {
        let _serial = BACKEND_TEST.lock().unwrap();
        let page = crate::sys::features::page_size();
        let mut f = Features::probe();
        let backends = [f.process_madvise, ProcessMadvise::Unavailable];
        for (i, backend) in backends.iter().enumerate() {
            configure(*backend, false);
            assert_eq!(uses_process_madvise(), *backend != ProcessMadvise::Unavailable);
            let len = 8 * page;
            let addr = unsafe { sys::mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) }.unwrap();
            for p in 0..8 {
                unsafe { *((addr + p * page) as *mut u8) = 0xA5 };
            }
            assert_eq!(resident(addr, len, page), 8);
            let mut b = Batch::new();
            // Two ranges with a hole between them; the hole stays resident.
            b.push(addr, 3 * page);
            b.push(addr + 4 * page, 4 * page);
            b.flush();
            assert_eq!(resident(addr, len, page), 1, "backend {i}: {backend:?}");
            assert_eq!(unsafe { *((addr + 3 * page) as *const u8) }, 0xA5);
            for p in [0usize, 1, 2, 4, 5, 6, 7] {
                assert_eq!(unsafe { *((addr + p * page) as *const u8) }, 0, "page {p} not zero ({backend:?})");
            }
            unsafe { sys::munmap(addr, len) }.unwrap();
        }
        configure(ProcessMadvise::Unavailable, false);
        f.release();
    }

    /// The fork child drops an inherited pidfd and keeps `PIDFD_SELF`.
    #[test]
    fn fork_child_downgrades_a_pidfd_backend() {
        let _serial = BACKEND_TEST.lock().unwrap();
        configure(ProcessMadvise::PidfdSelf, false);
        assert_eq!(fork_child(), ProcessMadvise::PidfdSelf);
        assert!(uses_process_madvise());
        let fd = sys::open(b"/dev/null\0", sys::O_RDONLY).unwrap();
        configure(ProcessMadvise::Pidfd(fd), false);
        assert_eq!(fork_child(), ProcessMadvise::Unavailable);
        assert!(!uses_process_madvise());
        // The fd was closed.
        assert!(sys::close(fd).is_err());
        configure(ProcessMadvise::Unavailable, false);
        assert_eq!(fork_child(), ProcessMadvise::Unavailable);
    }
}
