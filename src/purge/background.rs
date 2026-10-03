//! The background thread (design 8.1): one thread per process that runs
//! the decay passes and the cgroup sampling on its own stack, started by
//! configuration or `mallctl("background_thread")`.

use crate::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::ffi::c_void;

use crate::heap::{Shared, TLS_CREATING, TLS_EXITED};
#[cfg(verus_only)]
use crate::lock::RawLock;
use crate::lock::{Lock, LockHeld, LockOrder};
#[cfg(verus_only)]
use crate::lock::{axiom_entry, rank_meta, rank_state};
use crate::sys::{self, Timespec};
use crate::vspec::addr::addr_to_ptr;
#[cfg(verus_only)]
use crate::vspec::lock::lock_inv;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, verus};

verus! {

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

/// Assumption: every holder of the state lock leaves the stack's end
/// in range, so the lock's invariant is `stack + STACK_SIZE <= usize::MAX`
/// (0 before the first start; after it, a reservation of `STACK_SIZE`
/// bytes, which `Meta::reserve` places below `va_limit`). The verified
/// holders keep it: `start` asserts it after writing `stack`, and `main`
/// and `fork_child` (the fork child's writer through
/// `Lock::get_unlocked`) keep it by not writing `stack`. Verus checks
/// nothing when a guard is released, so this is the claim. The test-only
/// readers (`join_exited`, `stack`, `thread`) never write `stack`, and no
/// other code touches the value.
broadcast axiom fn axiom_state_lock_inv(v: State)
    ensures
        #[trigger] lock_inv(v) == (v.stack + STACK_SIZE <= usize::MAX),
;

exec static STATE: Lock<State> = {
    proof {
        axiom_state_lock_inv(State { present: false, creating: false, stack: 0, thread: None });
    }
    Lock::new(State {
        present: false,
        creating: false,
        stack: 0,
        thread: None,
    }, Ghost(rank_state()))
};
/// Mirror of `STATE.present` for the lock-free check on every tick.
exec static PRESENT: AtomicBool = AtomicBool::new(false);
/// `background_thread:true` (or, later, `mallctl("background_thread")`).
exec static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Futex word the thread sleeps on; bumped by [`wake`].
exec static SEQ: AtomicU32 = AtomicU32::new(0);

} // verus!

/// Run once by the next `start` that creates a thread, after the
/// creation and before the thread is published, for tests.
#[cfg(test)]
pub static BEFORE_PUBLISH: std::sync::Mutex<Option<std::boxed::Box<dyn FnOnce() + Send>>> =
    std::sync::Mutex::new(None);
/// Waits the thread has started with no timeout, for tests.
#[cfg(test)]
pub static UNTIMED_WAITS: AtomicU32 = AtomicU32::new(0);

verus! {

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
pub fn request(on: bool)
    requires
        crate::sys::may_syscall(),
{
    REQUESTED.store(on, Ordering::Relaxed);
    if !on {
        // Unconditional: a thread the bookkeeping has already forgotten
        // (the parent side of a fork handler test) must still notice.
        kick();
    }
}

/// Bump the futex word and wake the thread if it is waiting. Cheap
/// enough for the rare "first `Empty` slab of a class" transition.
pub fn wake()
    requires
        crate::sys::may_syscall(),
{
    if present() {
        kick();
    }
}

fn kick()
    requires
        crate::sys::may_syscall(),
{
    SEQ.fetch_add(1, Ordering::SeqCst);
    // SAFETY: `SEQ` is a live, aligned `u32`.
    let _ = unsafe { sys::futex_wake(SEQ.as_ptr(), 1) };
}

/// This thread's `errno` slot. Trusted: `__errno_location`, a libc call;
/// nothing is claimed about the address.
#[verifier::external_body]
#[inline(always)]
fn errno_location() -> *mut i32 {
    // SAFETY: `__errno_location` returns this thread's `errno`.
    unsafe { ::libc::__errno_location() }
}

/// The `errno` at `errno`. Trusted: a raw read; nothing is claimed about
/// the value.
///
/// # Safety
/// `errno` is [`errno_location`]'s result on this thread.
#[verifier::external_body]
#[inline(always)]
unsafe fn errno_read(errno: *mut i32) -> i32 {
    // SAFETY: guaranteed by the caller.
    unsafe { *errno }
}

/// Store `v` at `errno`. Trusted: a raw write.
///
/// # Safety
/// `errno` is [`errno_location`]'s result on this thread.
#[verifier::external_body]
#[inline(always)]
unsafe fn errno_write(errno: *mut i32, v: i32) {
    // SAFETY: guaranteed by the caller.
    unsafe { *errno = v };
}

/// Create the thread if it does not exist. `true` if one exists on
/// return; `false` if creation failed or is in progress on another
/// thread (the caller then purges in the foreground for this tick).
///
/// `errno` is preserved: this runs from `free`'s slow path (design
/// section 1 promises `free` leaves `errno` alone) and glibc's
/// `pthread_create` / `pthread_join` may set it on their way.
pub fn start(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>) -> bool
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_meta()),
    ensures
        *final(ord) == *old(ord),
{
    broadcast use axiom_state_lock_inv;

    let (stack, previous) = {
        let mut g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
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
    let errno = errno_location();
    let saved_errno = unsafe { errno_read(errno) };
    let (stack, thread) = create(shared, stack, previous, Tracked(ord));
    // SAFETY: `errno` is this thread's `errno` slot (above), valid for
    // the life of the thread.
    unsafe { errno_write(errno, saved_errno) };
    #[cfg(test)]
    if thread.is_some() {
        let hook = BEFORE_PUBLISH.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
    }
    let present = {
        let mut g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        g.creating = false;
        g.stack = stack;
        g.thread = thread;
        g.present = thread.is_some();
        PRESENT.store(g.present, Ordering::Relaxed);
        assert(lock_inv(g@));
        g.present
    };
    // A class's first `Empty` slab may have arrived after the thread's
    // last scan, which left it asleep with no deadline, and its `wake`
    // did nothing while the thread was not yet present: make it look
    // again.
    if present {
        kick();
    }
    present
}

/// `pthread_join(t, NULL)`: 0 or an error number. Trusted: a raw libc
/// call; nothing is claimed about the result.
///
/// # Safety
/// `t` is a joinable thread this module created and never joined.
#[verifier::external_body]
#[inline(always)]
unsafe fn pthread_join(t: ::libc::pthread_t) -> i32
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: guaranteed by the caller.
    unsafe { ::libc::pthread_join(t, core::ptr::null_mut()) }
}

/// Create the thread on `stack` (reserving the stack first if there is
/// none yet), after joining `previous`. Returns the stack (kept even
/// when creation fails, for the next attempt) and the new thread.
// The `match` stands for `Option::filter`, which vstd does not specify.
#[allow(clippy::manual_filter)]
fn create(
    shared: &Shared,
    stack: usize,
    previous: Option<::libc::pthread_t>,
    Tracked(ord): Tracked<&mut LockOrder>,
) -> (r: (usize, Option<::libc::pthread_t>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        stack + STACK_SIZE <= usize::MAX,
        old(ord).allows(rank_meta()),
    ensures
        r.0 + STACK_SIZE <= usize::MAX,
        *final(ord) == *old(ord),
{
    broadcast use crate::vspec::fmt::lemma_errno_fmt_req;

    proof {
        shared.lemma_wf();
    }
    let page = shared.regions.page_size();
    proof {
        // The stack arithmetic below relies on a page no larger than the
        // stack, as the code does: Linux pages are 4 to 64 KiB.
        crate::layout::axiom_process_page_at_most_64k();
        let lg = shared.regions.lg_page;
        assert((1usize << lg) <= 0x4_0000usize) by (bit_vector)
            requires lg <= 16;
        assert(STACK_SIZE == 0x4_0000usize);
    }
    // The TLS slot is parked on `TLS_CREATING` for the pthread calls:
    // `pthread_create` allocates the new thread's DTV with `calloc`
    // and `pthread_join` frees an exited thread's key data, neither of
    // which may re-enter this thread's heap mid-operation.
    let tracked mut tls = crate::sys::tls::TlsSlot::entry();
    let saved = crate::sys::tls::get(Tracked(&tls));
    let park = saved as usize > TLS_EXITED;
    if park {
        crate::sys::tls::set(addr_to_ptr(TLS_CREATING) as *mut c_void, Tracked(&mut tls));
    }
    // The previous thread exited at a wake-up (`request(false)`) but
    // glibc's exit path still runs on the stack until the kernel has
    // reaped it; the join returns only then, so nothing runs on the
    // stack when it is handed to the next thread. A fork child never
    // has a `previous`: its copy of the stack carries no thread.
    if let Some(t) = previous {
        // SAFETY: a joinable thread this module created and never joined.
        let rc = unsafe { pthread_join(t) };
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
        proof {
            crate::meta::lemma_is_pow2_shl(shared.regions.lg_page as usize);
        }
        // `.filter(|&s| shared.meta.commit(..).is_ok())`, spelled out:
        // vstd does not specify `Option::filter`.
        match shared.meta.reserve(STACK_SIZE, page, Tracked(ord)) {
            Some(s) => {
                if shared.meta.commit(s + page, STACK_SIZE - page, Tracked(ord)).is_ok() {
                    Some(s)
                } else {
                    None
                }
            }
            None => None,
        }
    };
    let thread = match stack {
        Some(stack) => spawn(shared, stack + page, STACK_SIZE - page),
        None => None,
    };
    if park {
        crate::sys::tls::set(saved, Tracked(&mut tls));
    }
    (stack.unwrap_or(0), thread)
}

/// `pthread_create` on the committed `[base, base + len)`, joinable so
/// a restart can wait for its exit. Signals are blocked so the new
/// thread inherits an empty mask and never runs an application handler.
///
/// Trusted: raw pthread calls, and the thread's entry is a function
/// pointer. `shared` is well formed: the thread's [`shared_arg`] ensures
/// it of the pointer passed here.
#[verifier::external_body]
fn spawn(shared: &Shared, base: usize, len: usize) -> Option<::libc::pthread_t>
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
{
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

} // verus!

/// Join the recorded thread if it has exited (`request(false)` and
/// its wake-up have happened) and forget it, so the stack is known to
/// be idle without waiting for the next `start`. `false` if there is
/// no thread to join or it is still present. Used by the tests that
/// drive `fork_child` in-process: dropping the record of a thread that
/// exists in *this* process would leave it unjoined for good.
#[cfg(test)]
pub fn join_exited() -> bool {
    let t = {
        let mut g = STATE.lock_t();
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
    STATE.lock_t().stack
}

/// The last thread created and not yet joined, for tests.
#[cfg(test)]
pub fn thread() -> Option<::libc::pthread_t> {
    STATE.lock_t().thread
}

verus! {

/// The `Shared` that [`spawn`] passes as the thread's argument. Trusted:
/// a raw-pointer dereference; the pointer is the `Shared` `start` was
/// given, which its contract requires to be well formed.
#[verifier::external_body]
#[inline(always)]
fn shared_arg(arg: *mut c_void) -> (r: &'static Shared)
    ensures
        r.wf(),
{
    // SAFETY: `start` passed a `Shared` that lives for the rest of the
    // process (the process-wide one, or a leaked test fixture).
    unsafe { &*(arg as *const Shared) }
}

/// The thread's name for `PR_SET_NAME`, NUL-terminated.
const THREAD_NAME: [u8; 11] = [b'c', b'h', b'a', b'l', b'l', b'o', b'c', b'-', b'b', b'g', 0];

/// The thread's body: decay and sample until the request is withdrawn.
/// Termination is not claimed: the loop ends when another thread calls
/// `request(false)`.
#[verifier::exec_allows_no_decreases_clause]
extern "C" fn main(arg: *mut c_void) -> *mut c_void {
    proof {
        crate::sys::grant_syscalls();
        crate::sys::grant_aborts();
    }
    let shared = shared_arg(arg);
    // Premise (lock order): a new thread holds no allocator lock.
    let tracked mut ord = axiom_entry();
    // SAFETY: the name is NUL-terminated.
    let _ = unsafe { sys::prctl(PR_SET_NAME, THREAD_NAME.as_ptr() as usize, 0, 0, 0) };
    loop
        invariant
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            ord.top() == 0,
    {
        // Read the sequence before scanning, so a bump that lands
        // between the scan and the wait makes the wait return at once.
        let seq = SEQ.load(Ordering::SeqCst);
        if !requested() {
            let mut g = state().lock(Tracked(&mut ord), Tracked(&mut LockOrder::spare()));
            g.present = false;
            PRESENT.store(false, Ordering::Relaxed);
            return core::ptr::null_mut();
        }
        let now = sys::now_coarse_ns();
        super::sample(shared, now, Tracked(&mut ord));
        let deadline = super::decay(shared, now, Tracked(&mut ord));
        // `deadline.map(|deadline| ..)`, spelled as a `match`.
        let timeout = match deadline {
            Some(deadline) => {
                let ns = deadline.saturating_sub(now).max(MIN_SLEEP_NS);
                Some(Timespec {
                    sec: (ns / 1_000_000_000) as i64,
                    nsec: (ns % 1_000_000_000) as i64,
                })
            }
            None => None,
        };
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

/// The state lock, as [`fork_lock`] takes it: [`STATE`]'s raw lock.
/// Uninterpreted (Verus does not read statics in specifications);
/// [`state`] pins it.
pub uninterp spec fn state_lock() -> RawLock;

/// `&STATE`. Trusted: names its raw lock [`state_lock`], which holds
/// because `STATE` is one static, and its rank, the one `STATE` is
/// created with.
#[verifier::external_body]
#[inline(always)]
fn state() -> (r: &'static Lock<State>)
    ensures
        r.spec_raw() == state_lock(),
        r.rank() == rank_state(),
{
    &STATE
}

/// Take the state lock (fork prepare, after the metadata lock); the
/// token is the hold, and `ord` (a root) is raised to its rank.
pub fn fork_lock(Tracked(ord): Tracked<&mut LockOrder>) -> (t: Tracked<LockHeld>)
    requires
        crate::sys::may_syscall(),
        old(ord).root(),
        old(ord).allows(rank_state()),
    ensures
        t@.held() == state_lock(),
        final(ord).root(),
        final(ord).top() == rank_state(),
{
    state().raw().lock_held(Tracked(ord))
}

/// Release the state lock (fork parent and child, before the others),
/// handing back the token of the hold.
pub fn fork_unlock(Tracked(t): Tracked<LockHeld>)
    requires
        crate::sys::may_syscall(),
        t.held() == state_lock(),
{
    state().raw().unlock_held(Tracked(t))
}

} // verus!

/// Whether the state lock is held (for the fork handler tests).
#[cfg(test)]
pub fn is_locked() -> bool {
    STATE.raw().is_locked()
}

verus! {

/// The child has no background thread and, per design 8.1, does not
/// get one until it asks again. Called with the state lock held by
/// this thread (`fork_prepare` took it; `held` is the hold). Leaves the
/// state's lock invariant true, so the value the child's next locker
/// reads is consistent.
pub fn fork_child(Tracked(held): Tracked<&LockHeld>)
    requires
        held.held() == state_lock(),
{
    // SAFETY: the forking thread holds the lock; the child is single
    // threaded.
    let st = unsafe { state().get_unlocked(Tracked(held)) };
    let ghost before = *st;
    st.present = false;
    st.creating = false;
    // The parent's thread does not exist here and its `pthread_t`
    // must never be joined (the wait would be on a thread id the
    // kernel never clears in this process); the stack is kept and is
    // free to reuse, nothing runs on the child's copy of it.
    st.thread = None;
    proof {
        axiom_state_lock_inv(before);
        axiom_state_lock_inv(*st);
        assert(lock_inv(*st));
    }
    PRESENT.store(false, Ordering::Relaxed);
    REQUESTED.store(false, Ordering::Relaxed);
}

} // verus!
