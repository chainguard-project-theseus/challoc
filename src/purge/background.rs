//! The background thread (design 8.1): one thread per process that runs
//! the decay passes and the cgroup sampling on its own stack, started by
//! configuration, `mallctl("background_thread")` or, by default, lazily:
//! once the process has held enough dirty memory ([`auto_tick`]).

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};

use crate::heap::{Shared, TLS_CREATING, TLS_EXITED};
use crate::lock::{Lock, Padded};
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
    /// The thread being created has already exited: it found the request
    /// withdrawn before `start` published it, so `start` must not.
    exited: bool,
    /// The one stack, reserved by the first `start` and reused by every
    /// later one; 0 until then.
    stack: usize,
    /// The last thread created on that stack, until it has been joined.
    /// `None` in a fork child, whose copy of the stack has no thread.
    thread: Option<::libc::pthread_t>,
}

static STATE: Lock<State> = Lock::new(State {
    present: false,
    creating: false,
    exited: false,
    stack: 0,
    thread: None,
});
/// Mirror of `STATE.present` for the lock-free check on every tick.
static PRESENT: AtomicBool = AtomicBool::new(false);
/// `background_thread:true` (or, later, `mallctl("background_thread")`).
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Futex word the thread sleeps on; bumped by [`wake`]. On a line of its
/// own: [`wake`]'s `fetch_add` runs while the thread is present, and the
/// statics placed next to it (`api::STATE`) are read by every `malloc`
/// and `free`.
static SEQ: Padded<AtomicU32> = Padded(AtomicU32::new(0));

/// Lazy start (the default, `background_thread` unset): nothing is
/// requested, but [`auto_tick`] requests and starts the thread once the
/// process holds [`AUTO_BURST`] dirty bytes, or [`AUTO_FLOOR`] for
/// [`AUTO_HOLD_NS`]. Cleared by an explicit [`request`] and for good by
/// [`AUTO_TRIES`] lazy starts that failed (a sandbox that refuses
/// `clone`, a thread limit the process sits at).
static AUTO: AtomicBool = AtomicBool::new(false);
/// Lazy starts that failed so far.
static AUTO_FAILS: AtomicU32 = AtomicU32::new(0);
/// Coarse time before which [`auto_tick`] does not look again.
static AUTO_NEXT: AtomicU64 = AtomicU64::new(0);
/// Coarse time of the first look of the current stretch at or above
/// [`AUTO_FLOOR`]; 0 when the last look was below it.
static AUTO_SINCE: AtomicU64 = AtomicU64::new(0);
/// The low-water mark of the dirty bytes since the fork that made this
/// process, starting from the dirty bytes at the fork (0 in a process
/// that never forked): what a fork child inherited is not its own, and
/// only what it adds above the inherited set (as that set decays) counts
/// towards a start.
static AUTO_BASE: AtomicU64 = AtomicU64::new(0);

/// Dirty bytes that start the thread at the first look.
pub const AUTO_BURST: u64 = 64 << 20;
/// Dirty bytes that start it when they stay at least this high (at every
/// look) for [`AUTO_HOLD_NS`].
pub const AUTO_FLOOR: u64 = 4 << 20;
/// How long [`AUTO_FLOOR`] must hold.
pub const AUTO_HOLD_NS: u64 = 1_000_000_000;
/// At most one look per this much coarse time: a look sums the `Empty`
/// bytes of every class that has any (one lock hold each).
pub const AUTO_CHECK_NS: u64 = 100_000_000;
/// Failed lazy starts after which the lazy start is off for good. The
/// ones before it are retried no sooner than [`AUTO_RETRY_NS`] later: an
/// `EAGAIN` from a pids or thread limit the process sits at may pass.
pub const AUTO_TRIES: u32 = 3;
/// The least time between a failed lazy start and the next look.
pub const AUTO_RETRY_NS: u64 = 1_000_000_000;
/// Run once by the next `start` that creates a thread, after the
/// creation and before the thread is published, for tests.
#[cfg(test)]
pub static BEFORE_PUBLISH: std::sync::Mutex<Option<std::boxed::Box<dyn FnOnce() + Send>>> =
    std::sync::Mutex::new(None);
/// Waits the thread has started with no timeout, for tests.
#[cfg(test)]
pub static UNTIMED_WAITS: AtomicU32 = AtomicU32::new(0);
/// Times the thread has exited because it was no longer requested, for
/// tests.
#[cfg(test)]
pub static EXITS: AtomicU32 = AtomicU32::new(0);

#[inline]
pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

#[inline]
pub fn requested() -> bool {
    REQUESTED.load(Ordering::Relaxed)
}

/// The `background_thread` key at init: `Some(true)` requests the thread
/// (the first periodic tick creates it), `Some(false)` never starts one,
/// `None` arms the lazy start. Stores only: no syscall at init.
pub fn configure(setting: Option<bool>) {
    AUTO.store(setting.is_none(), Ordering::Relaxed);
    REQUESTED.store(setting == Some(true), Ordering::Relaxed);
}

/// Whether the lazy start is armed.
#[inline]
pub fn auto() -> bool {
    AUTO.load(Ordering::Relaxed)
}

/// Ask for (or, with `false`, stop asking for) a background thread. The
/// next periodic tick creates it; a running thread that finds the
/// request withdrawn exits at its next wake-up. An explicit answer
/// either way disarms the lazy start.
pub fn request(on: bool) {
    {
        // Under the state lock, so a failing lazy start (`start_requested`)
        // cannot withdraw this request after it was made.
        let _g = STATE.lock();
        AUTO.store(false, Ordering::Relaxed);
        REQUESTED.store(on, Ordering::Relaxed);
    }
    if !on {
        // Unconditional: a thread the bookkeeping has already forgotten
        // (the parent side of a fork handler test) must still notice.
        kick();
    }
}

/// Bump the futex word and wake the thread if it is waiting. Cheap
/// enough for the rare "first `Empty` slab of a class" transition.
pub fn wake() {
    // Store buffering with `start`: the caller stored what the thread
    // must see (a first `oldest_empty`, a decay period) and this loads
    // `PRESENT`, while `start` stores `PRESENT` and the thread, after
    // `start`'s kick, loads what the caller stored. Without a `SeqCst`
    // fence on each side both loads may read the old value: no kick
    // here, and the thread asleep on what it last saw (with no deadline,
    // for a first `Empty`). `start` has the other fence; `tests/loom`
    // checks the pair.
    fence(Ordering::SeqCst);
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
        g.exited = false;
        (g.stack, g.thread.take())
    };
    // SAFETY: `__errno_location` returns this thread's `errno`.
    let errno = unsafe { ::libc::__errno_location() };
    let saved_errno = unsafe { *errno };
    let (stack, thread) = create(shared, stack, previous);
    // SAFETY: `errno` is this thread's `errno` slot (above), valid for
    // the life of the thread.
    unsafe { *errno = saved_errno };
    #[cfg(test)]
    if thread.is_some() {
        let hook = BEFORE_PUBLISH.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
    }
    let present = {
        let mut g = STATE.lock();
        g.creating = false;
        g.stack = stack;
        g.thread = thread;
        g.present = thread.is_some() && !g.exited;
        PRESENT.store(g.present, Ordering::Relaxed);
        g.present
    };
    // A class's first `Empty` slab may have arrived after the thread's
    // last scan, which left it asleep with no deadline, and its `wake`
    // did nothing while the thread was not yet present: make it look
    // again.
    if present {
        // The other side of `wake`'s fence: the `PRESENT` store, then
        // the kick, after which the thread loads what `wake`'s caller
        // stored.
        fence(Ordering::SeqCst);
        kick();
    }
    present
}

/// [`start`] for an allocation's tick that found the thread requested.
/// A lazy request (the lazy start still armed) whose start failed, and
/// is not merely in progress on another thread, withdraws itself: the
/// process keeps purging in the foreground, as with
/// `background_thread:false`, and the lazy start looks again no sooner
/// than [`AUTO_RETRY_NS`] later, up to [`AUTO_TRIES`] failures in all,
/// after which it is off for good. An explicit request stays, and the
/// next tick tries again.
pub fn start_requested(shared: &Shared) -> bool {
    if start(shared) {
        return true;
    }
    let g = STATE.lock();
    if !g.creating && !g.present && AUTO.load(Ordering::Relaxed) {
        REQUESTED.store(false, Ordering::Relaxed);
        let fails = AUTO_FAILS.load(Ordering::Relaxed) + 1;
        AUTO_FAILS.store(fails, Ordering::Relaxed);
        if fails >= AUTO_TRIES {
            AUTO.store(false, Ordering::Relaxed);
        } else {
            AUTO_NEXT.store(
                sys::now_coarse_ns().saturating_add(AUTO_RETRY_NS),
                Ordering::Relaxed,
            );
        }
    }
    false
}

/// The lazy start's look, from a periodic tick that ran decay in the
/// foreground and left dirty memory waiting (`purge::on_tick`): at most
/// once per [`AUTO_CHECK_NS`], sum the dirty bytes and, when [`auto_due`]
/// says so, request the thread, and start it if the tick is an
/// allocation's (`alloc`; a `free`'s tick leaves the start to the next
/// allocation's tick, which finds the request).
/// Out of line: the periodic tick inlines `on_tick`, and this look is
/// rare next to it.
#[cold]
#[inline(never)]
pub fn auto_tick(shared: &Shared, now: u64, alloc: bool) {
    if !auto() {
        return;
    }
    #[cfg(test)]
    if AUTO_ONLY.load(Ordering::Relaxed) != shared as *const Shared as usize {
        return;
    }
    let next = AUTO_NEXT.load(Ordering::Relaxed);
    if now < next
        || AUTO_NEXT
            .compare_exchange(
                next,
                now.saturating_add(AUTO_CHECK_NS),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_err()
    {
        return;
    }
    let dirty = crate::policy::empty_bytes(shared);
    let base = AUTO_BASE.load(Ordering::Relaxed).min(dirty);
    AUTO_BASE.store(base, Ordering::Relaxed);
    if !auto_due(dirty - base, now, &AUTO_SINCE) {
        return;
    }
    {
        // Under the state lock: an explicit `request` in between wins.
        let _g = STATE.lock();
        if !AUTO.load(Ordering::Relaxed) {
            return;
        }
        REQUESTED.store(true, Ordering::Relaxed);
    }
    if alloc {
        start_requested(shared);
    }
}

/// Whether `own` dirty bytes at `now` start the thread: at once from
/// [`AUTO_BURST`]; from [`AUTO_FLOOR`] once every look for
/// [`AUTO_HOLD_NS`] has seen at least that much. Updates the stretch,
/// `since` (the start of the current one, 0 for none).
pub(crate) fn auto_due(own: u64, now: u64, since: &AtomicU64) -> bool {
    if own >= AUTO_BURST {
        return true;
    }
    if own < AUTO_FLOOR {
        since.store(0, Ordering::Relaxed);
        return false;
    }
    let start = since.load(Ordering::Relaxed);
    if start == 0 {
        since.store(now.max(1), Ordering::Relaxed);
        return false;
    }
    now.saturating_sub(start) >= AUTO_HOLD_NS
}

/// The one allocator whose ticks the lazy start looks at, for tests:
/// `AUTO` is process-wide and the unit tests tick many fixtures at once.
#[cfg(test)]
static AUTO_ONLY: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Arm the lazy start for `shared` alone (or, with `None`, disarm it),
/// with a fresh stretch and no baseline, for tests.
#[cfg(test)]
pub fn set_auto(shared: Option<&Shared>) {
    AUTO_ONLY.store(
        shared.map_or(0, |s| s as *const Shared as usize),
        Ordering::Relaxed,
    );
    AUTO.store(shared.is_some(), Ordering::Relaxed);
    AUTO_NEXT.store(0, Ordering::Relaxed);
    AUTO_SINCE.store(0, Ordering::Relaxed);
    AUTO_BASE.store(0, Ordering::Relaxed);
    AUTO_FAILS.store(0, Ordering::Relaxed);
}

/// Create the thread on `stack` (reserving the stack first if there is
/// none yet), after joining `previous`. Returns the stack (kept even
/// when creation fails, for the next attempt) and the new thread.
fn create(
    shared: &Shared,
    stack: usize,
    previous: Option<::libc::pthread_t>,
) -> (usize, Option<::libc::pthread_t>) {
    let page = shared.regions.page_size();
    // The TLS slot is parked on `TLS_CREATING` for the pthread calls:
    // `pthread_create` allocates the new thread's DTV with `calloc`
    // and `pthread_join` frees an exited thread's key data, neither of
    // which may re-enter this thread's heap mid-operation.
    let saved = crate::sys::tls::get();
    let park = saved as usize > TLS_EXITED;
    if park {
        crate::sys::tls::set(TLS_CREATING as *mut c_void);
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
            sys::abort_fmt(format_args!(
                "pthread_join of the background thread failed: {}",
                sys::Errno(rc)
            ));
        }
    }
    // Reserved once, then committed above the lowest page, which stays
    // `PROT_NONE` as the guard. Never freed: the thread never exits
    // while its stack could be in use, and a restart reuses the same
    // reservation after the join above.
    let stack = if stack != 0 {
        Some(stack)
    } else {
        shared
            .meta
            .reserve(STACK_SIZE, page)
            .filter(|&s| shared.meta.commit(s + page, STACK_SIZE - page).is_ok())
    };
    let thread = match stack {
        Some(stack) => spawn(shared, stack + page, STACK_SIZE - page),
        None => None,
    };
    if park {
        crate::sys::tls::set(saved);
    }
    (stack.unwrap_or(0), thread)
}

/// `pthread_create` on the committed `[base, base + len)`, joinable so
/// a restart can wait for its exit. Signals are blocked
/// ([`sys::block_all_signals`]) so the new thread inherits them blocked
/// and never runs an application handler.
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
            let rc = ::libc::pthread_create(
                &mut tid,
                &attr,
                main,
                shared as *const Shared as *mut c_void,
            );
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
#[cfg(test)]
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
#[cfg(test)]
pub fn stack() -> usize {
    STATE.lock().stack
}

/// The last thread created and not yet joined, for tests.
#[cfg(test)]
pub fn thread() -> Option<::libc::pthread_t> {
    STATE.lock().thread
}

extern "C" fn main(arg: *mut c_void) -> *mut c_void {
    // SAFETY: `start` passed a `Shared` that lives for the rest of the
    // process (the process-wide one, or a leaked test fixture); the name
    // is NUL-terminated.
    let shared = unsafe { &*(arg as *const Shared) };
    let _ = unsafe { sys::prctl(PR_SET_NAME, b"challoc-bg\0".as_ptr() as usize, 0, 0, 0) };
    detach_from_starter();
    loop {
        // Read the sequence before scanning, so a bump that lands
        // between the scan and the wait makes the wait return at once.
        let seq = SEQ.load(Ordering::SeqCst);
        if !requested() {
            let mut g = STATE.lock();
            g.present = false;
            g.exited = true;
            PRESENT.store(false, Ordering::Relaxed);
            #[cfg(test)]
            EXITS.fetch_add(1, Ordering::SeqCst);
            return core::ptr::null_mut();
        }
        let now = sys::now_coarse_ns();
        super::sample(shared, now);
        let deadline = super::decay(shared, now);
        let timeout = deadline.map(|deadline| {
            let ns = deadline.saturating_sub(now).max(MIN_SLEEP_NS);
            Timespec {
                sec: (ns / 1_000_000_000) as i64,
                nsec: (ns % 1_000_000_000) as i64,
            }
        });
        #[cfg(test)]
        if timeout.is_none() {
            UNTIMED_WAITS.fetch_add(1, Ordering::SeqCst);
        }
        // SAFETY: `SEQ` is a live, aligned `u32`; `timeout` outlives
        // the call. Every outcome (wake, EAGAIN, ETIMEDOUT, EINTR) just
        // means "look again".
        let _ = unsafe { sys::futex_wait(SEQ.as_ptr(), seq, timeout.as_ref()) };
    }
}

/// The thread inherits the scheduling of whichever thread created it, and
/// under the lazy start that is any allocating thread: a worker pinned to
/// one CPU, niced or real-time. Run on the new thread, this puts it on
/// `SCHED_OTHER` with the CPU affinity and nice value of the process's
/// main thread (the thread whose id is the pid) instead. Best effort:
/// what the kernel refuses (raising priority past `RLIMIT_NICE`, leaving
/// `SCHED_IDLE`, a main thread that has exited) stays as inherited.
fn detach_from_starter() {
    // SAFETY: scheduling calls on this thread (id 0) and reads of the
    // main thread's settings, all into locals.
    unsafe {
        let param = ::libc::sched_param { sched_priority: 0 };
        let _ = ::libc::sched_setscheduler(0, ::libc::SCHED_OTHER, &param);
        let pid = ::libc::getpid();
        let mut set: ::libc::cpu_set_t = core::mem::zeroed();
        let size = core::mem::size_of::<::libc::cpu_set_t>();
        if ::libc::sched_getaffinity(pid, size, &mut set) == 0 {
            let _ = ::libc::sched_setaffinity(0, size, &set);
        }
        // getpriority returns the nice value, -1 included, so errno tells
        // a failure apart.
        let errno = ::libc::__errno_location();
        *errno = 0;
        let nice = ::libc::getpriority(::libc::PRIO_PROCESS, pid as ::libc::id_t);
        if *errno == 0 {
            let _ = ::libc::setpriority(::libc::PRIO_PROCESS, 0, nice);
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
#[cfg(test)]
pub fn is_locked() -> bool {
    STATE.raw().is_locked()
}

/// The child has no background thread and, per design 8.1, does not
/// get one until it asks again or, under the lazy start, until it holds
/// enough dirty memory of its own: the dirty bytes at the fork are the
/// baseline ([`set_fork_base`], after the locks are released). Called
/// with the state lock held by this thread (`fork_prepare` took it).
pub fn fork_child() {
    // SAFETY: the forking thread holds the lock; the child is single
    // threaded.
    let st = unsafe { STATE.get_unlocked() };
    st.present = false;
    st.creating = false;
    st.exited = false;
    // The parent's thread does not exist here and its `pthread_t`
    // must never be joined (the wait would be on a thread id the
    // kernel never clears in this process); the stack is kept and is
    // free to reuse, nothing runs on the child's copy of it.
    st.thread = None;
    PRESENT.store(false, Ordering::Relaxed);
    REQUESTED.store(false, Ordering::Relaxed);
    AUTO_NEXT.store(0, Ordering::Relaxed);
    AUTO_SINCE.store(0, Ordering::Relaxed);
}

/// The lazy start's baseline in a fork child: `inherited`, the dirty
/// bytes at the fork, from which [`auto_tick`] keeps the low-water mark.
pub fn set_fork_base(inherited: u64) {
    AUTO_BASE.store(inherited, Ordering::Relaxed);
}
