//! The background thread (design 8.1): one thread per process that runs
//! the decay passes and the cgroup sampling on its own stack, started by
//! configuration or `mallctl("background_thread")`.

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use core::sync::atomic::AtomicI32;

use crate::heap::{Shared, TLS_CREATING, TLS_EXITED};
use crate::lock::Lock;
use crate::sys::{self, Errno, PollFd, Timespec};

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
    /// The one stack, reserved by the first `start` and reused by every
    /// later one; 0 until then.
    stack: usize,
    /// The last thread created on that stack, until it has been joined.
    /// `None` in a fork child, whose copy of the stack has no thread.
    thread: Option<::libc::pthread_t>,
}

static STATE: Lock<State> = Lock::new(State { present: false, creating: false, stack: 0, thread: None });
/// Mirror of `STATE.present` for the lock-free check on every tick.
static PRESENT: AtomicBool = AtomicBool::new(false);
/// `background_thread:true` (or, later, `mallctl("background_thread")`).
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Futex word the thread sleeps on; bumped by [`wake`].
static SEQ: AtomicU32 = AtomicU32::new(0);
/// The PSI trigger descriptor (design 11) while the thread is armed,
/// else -1. Owned by the thread: only it (and a fork child, which is
/// single threaded) opens or closes it.
static PSI_FD: AtomicI32 = AtomicI32::new(-1);
/// The eventfd [`kick`] writes to while the thread sleeps in `ppoll`
/// rather than on the futex. Created once, never closed in the process
/// that created it (a kicker that loaded the number must not find it
/// reused by another file); a fork child closes its copy.
static WAKE_FD: AtomicI32 = AtomicI32::new(-1);
/// `PSI_FD` is valid and the thread sleeps in `ppoll`: kicks must also
/// write the eventfd.
static PSI_ARMED: AtomicBool = AtomicBool::new(false);

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
    if PSI_ARMED.load(Ordering::Acquire) {
        let fd = WAKE_FD.load(Ordering::Relaxed);
        if fd >= 0 {
            // Level-triggered until the thread drains it, so a kick that
            // lands before the `ppoll` is not lost. Raw syscall: `errno`
            // stays as the caller left it.
            let _ = sys::write(fd, &1u64.to_ne_bytes());
        }
    }
}

/// Arm the PSI trigger (design 11) and make sure the wake eventfd
/// exists; on any failure the cgroup record says why and the thread
/// sleeps on the futex as before.
fn arm(shared: &Shared) {
    let Some(psi) = shared.cgroup.arm_pressure() else { return };
    if WAKE_FD.load(Ordering::Relaxed) < 0 {
        match sys::eventfd(0, sys::EFD_CLOEXEC | sys::EFD_NONBLOCK) {
            Ok(fd) => WAKE_FD.store(fd, Ordering::Relaxed),
            Err(e) => {
                let _ = sys::close(psi);
                shared.cgroup.set_psi_unavailable(e.0);
                return;
            }
        }
    }
    PSI_FD.store(psi, Ordering::Relaxed);
    PSI_ARMED.store(true, Ordering::Release);
}

/// Drop the trigger: on thread exit (`errno` 0, "no thread"), or when
/// the poll reported the cgroup gone from under it.
fn disarm(shared: &Shared, errno: i32) {
    PSI_ARMED.store(false, Ordering::Release);
    let fd = PSI_FD.swap(-1, Ordering::Relaxed);
    if fd >= 0 {
        let _ = sys::close(fd);
    }
    shared.cgroup.set_psi_unavailable(errno);
}

/// Sleep in `ppoll` on the trigger (`POLLPRI`) and the wake eventfd
/// until `timeout`; account a fired trigger, drain a kick, and disarm
/// if the trigger's file went bad.
fn wait_poll(shared: &Shared, psi: i32, timeout: Option<&Timespec>) {
    let wake = WAKE_FD.load(Ordering::Relaxed);
    let mut fds = [
        PollFd { fd: psi, events: sys::POLLPRI, revents: 0 },
        PollFd { fd: wake, events: sys::POLLIN, revents: 0 },
    ];
    if let Ok(n) = sys::ppoll(&mut fds, timeout) {
        if n == 0 {
            return;
        }
        if fds[1].revents & sys::POLLIN != 0 {
            let mut b = [0u8; 8];
            let _ = sys::read(wake, &mut b);
        }
        if fds[0].revents & sys::POLLPRI != 0 {
            // A stall: sample outside the window and evaluate the
            // policy on it; the loop's decay pass follows at once.
            shared.cgroup.note_pressure_event(sys::now_coarse_ns());
            crate::policy::evaluate(shared);
        }
        if fds[0].revents & (sys::POLLERR | sys::POLLHUP | sys::POLLNVAL) != 0 {
            disarm(shared, Errno::ENOENT.0);
        }
    }
}

/// Create the thread if it does not exist. `true` if one exists on
/// return; `false` if creation failed or is in progress on another
/// thread (the caller then purges in the foreground for this tick).
///
/// `errno` is preserved: this runs from `free`'s slow path (design
/// section 1 promises `free` leaves `errno` alone) and glibc's
/// `pthread_create` / `pthread_join` may set it on their way.
pub fn start(shared: &Shared) -> bool {
    let (stack, previous) = {
        let mut g = STATE.lock();
        if g.present {
            return true;
        }
        if g.creating {
            return false;
        }
        g.creating = true;
        (g.stack, g.thread.take())
    };
    // SAFETY: `__errno_location` returns this thread's `errno`.
    let errno = unsafe { ::libc::__errno_location() };
    let saved_errno = unsafe { *errno };
    let (stack, thread) = create(shared, stack, previous);
    // SAFETY: `errno` is this thread's `errno` slot (above), valid for
    // the life of the thread.
    unsafe { *errno = saved_errno };
    let mut g = STATE.lock();
    g.creating = false;
    g.stack = stack;
    g.thread = thread;
    g.present = thread.is_some();
    PRESENT.store(g.present, Ordering::Relaxed);
    g.present
}

/// Create the thread on `stack` (reserving the stack first if there is
/// none yet), after joining `previous`. Returns the stack (kept even
/// when creation fails, for the next attempt) and the new thread.
fn create(shared: &Shared, stack: usize, previous: Option<::libc::pthread_t>) -> (usize, Option<::libc::pthread_t>) {
    let page = shared.regions.page_size();
    // The TLS slot is parked on `TLS_CREATING` for the pthread calls:
    // `pthread_create` allocates the new thread's DTV with `calloc`
    // and `pthread_join` frees an exited thread's key data, neither of
    // which may re-enter this thread's heap mid-operation.
    let saved = crate::tls::get();
    let park = saved as usize > TLS_EXITED;
    if park {
        crate::tls::set(TLS_CREATING as *mut c_void);
    }
    // The previous thread exited at a wake-up (`request(false)`) but
    // glibc's exit path still runs on the stack until the kernel has
    // reaped it; the join returns only then, so nothing runs on the
    // stack when it is handed to the next thread. A fork child never
    // has a `previous`: its copy of the stack carries no thread.
    if let Some(t) = previous {
        // SAFETY: a joinable thread this module created and never joined.
        let rc = unsafe { ::libc::pthread_join(t, core::ptr::null_mut()) };
        if rc != 0 {
            sys::abort_fmt(format_args!("pthread_join of the background thread failed: {}", sys::Errno(rc)));
        }
    }
    // Reserved once, then committed above the lowest page, which stays
    // `PROT_NONE` as the guard. Never freed: the thread never exits
    // while its stack could be in use, and a restart reuses the same
    // reservation after the join above.
    let stack = if stack != 0 {
        Some(stack)
    } else {
        shared.meta.reserve(STACK_SIZE, page).filter(|&s| shared.meta.commit(s + page, STACK_SIZE - page).is_ok())
    };
    let thread = match stack {
        Some(stack) => spawn(shared, stack + page, STACK_SIZE - page),
        None => None,
    };
    if park {
        crate::tls::set(saved);
    }
    (stack.unwrap_or(0), thread)
}

/// `pthread_create` on the committed `[base, base + len)`, joinable so
/// a restart can wait for its exit. Signals are blocked so the new
/// thread inherits an empty mask and never runs an application handler.
fn spawn(shared: &Shared, base: usize, len: usize) -> Option<::libc::pthread_t> {
    // SAFETY: plain pthread attribute calls on a local; the stack is
    // ours and committed.
    unsafe {
        let mut attr: ::libc::pthread_attr_t = core::mem::zeroed();
        if ::libc::pthread_attr_init(&mut attr) != 0 {
            return None;
        }
        let mut thread = None;
        if ::libc::pthread_attr_setstack(&mut attr, base as *mut c_void, len) == 0 {
            let mask = sys::block_all_signals();
            let mut tid: ::libc::pthread_t = 0;
            let rc = ::libc::pthread_create(&mut tid, &attr, main, shared as *const Shared as *mut c_void);
            sys::set_signal_mask(mask);
            if rc == 0 {
                thread = Some(tid);
            }
        }
        ::libc::pthread_attr_destroy(&mut attr);
        thread
    }
}

/// Join the recorded thread if it has exited (`request(false)` and
/// its wake-up have happened) and forget it, so the stack is known to
/// be idle without waiting for the next `start`. `false` if there is
/// no thread to join or it is still present. Used by the tests that
/// drive `fork_child` in-process: dropping the record of a thread that
/// exists in *this* process would leave it unjoined for good.
pub fn join_exited() -> bool {
    let t = {
        let mut g = STATE.lock();
        if g.present || g.creating {
            return false;
        }
        g.thread.take()
    };
    match t {
        Some(t) => {
            // SAFETY: a joinable thread this module created and never joined.
            assert_eq!(unsafe { ::libc::pthread_join(t, core::ptr::null_mut()) }, 0);
            true
        }
        None => false,
    }
}

/// The stack reservation (0 before the first start), for tests.
pub fn stack() -> usize {
    STATE.lock().stack
}

/// The last thread created and not yet joined, for tests.
pub fn thread() -> Option<::libc::pthread_t> {
    STATE.lock().thread
}

extern "C" fn main(arg: *mut c_void) -> *mut c_void {
    // SAFETY: `start` passed the process-wide `Shared`, which lives for
    // the rest of the process; the name is NUL-terminated.
    let shared = unsafe { &*(arg as *const Shared) };
    let _ = unsafe { sys::prctl(PR_SET_NAME, b"challoc-bg\0".as_ptr() as usize, 0, 0, 0) };
    // Design 11: the trigger is armed only by this thread, once it
    // exists, and only if the kernel takes the write.
    arm(shared);
    loop {
        // Read the sequence before scanning, so a bump that lands
        // between the scan and the wait makes the wait return at once.
        let seq = SEQ.load(Ordering::SeqCst);
        if !requested() {
            disarm(shared, 0);
            let mut g = STATE.lock();
            g.present = false;
            PRESENT.store(false, Ordering::Relaxed);
            return core::ptr::null_mut();
        }
        let now = sys::now_coarse_ns();
        super::sample(shared, now);
        let deadline = super::decay(shared, now);
        let timeout = deadline.map(|deadline| {
            let ns = deadline.saturating_sub(now).max(MIN_SLEEP_NS);
            Timespec { sec: (ns / 1_000_000_000) as i64, nsec: (ns % 1_000_000_000) as i64 }
        });
        let psi = PSI_FD.load(Ordering::Relaxed);
        if psi >= 0 {
            // A kick that landed since the `SEQ` load above left the
            // eventfd readable, so this returns at once in that case.
            wait_poll(shared, psi, timeout.as_ref());
        } else {
            // SAFETY: `SEQ` is a live, aligned `u32`; `timeout` outlives
            // the call. Every outcome (wake, EAGAIN, ETIMEDOUT, EINTR)
            // just means "look again".
            let _ = unsafe { sys::futex_wait(SEQ.as_ptr(), seq, timeout.as_ref()) };
        }
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
    // The parent's thread does not exist here and its `pthread_t`
    // must never be joined (the wait would be on a thread id the
    // kernel never clears in this process); the stack is kept and is
    // free to reuse, nothing runs on the child's copy of it.
    st.thread = None;
    PRESENT.store(false, Ordering::Relaxed);
    REQUESTED.store(false, Ordering::Relaxed);
    // The child's copies of the parent thread's descriptors: closing
    // them here is safe (one thread, nothing can be mid-`kick`), and
    // necessary, since a kick through the shared eventfd would wake the
    // parent's thread.
    PSI_ARMED.store(false, Ordering::Relaxed);
    for fd in [&PSI_FD, &WAKE_FD] {
        let fd = fd.swap(-1, Ordering::Relaxed);
        if fd >= 0 {
            let _ = sys::close(fd);
        }
    }
}
