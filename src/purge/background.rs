//! The background thread (design 8.1): one thread per process that runs
//! the decay passes and the cgroup sampling on its own stack, started by
//! configuration, `mallctl("background_thread")` or, by default, lazily:
//! once the process has held enough dirty memory ([`auto_tick`]).

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};

use crate::heap::{Shared, TLS_CREATING, TLS_EXITED};
use crate::lock::{Lock, LockHeld, LockOrder, Padded};
#[cfg(verus_keep_ghost)]
use crate::lock::{RawLock, axiom_entry, lock_inv, rank_class, rank_meta, rank_state};
use crate::sys::tls::TlsCell;
use crate::sys::{self, Timespec};
use crate::vspec::addr::addr_to_ptr;
use crate::vspec::cbuf::CCall;
use crate::vspec::ctl::Change;
use crate::vspec::errno::{ThreadErrno, get_errno, set_errno};
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, verus};
#[cfg(verus_keep_ghost)]
use vstd::resource::map::GhostSubmap;
#[cfg(verus_keep_ghost)]
use vstd::set_lib::set_int_range;

verus! {

/// Stack size including the guard page below it.
pub const STACK_SIZE: usize = 256 * 1024;
/// `prctl(PR_SET_NAME)`.
const PR_SET_NAME: i32 = 15;
/// Shortest futex timeout; the coarse clock ticks every few ms anyway.
const MIN_SLEEP_NS: u64 = 1_000_000;

/// Bookkeeping behind the state lock (the lock `fork_prepare` takes
/// last, design 9).
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

/// Assumption: every holder of the state lock leaves the stack's end in
/// range and no thread published that has exited, so the lock's
/// invariant is `stack + STACK_SIZE <= usize::MAX` (0 before the first
/// start; after it, a reservation of `STACK_SIZE` bytes, which
/// `Meta::reserve` places below `va_limit`) and `exited ==> !present`
/// (a thread that found the request withdrawn and exited before its
/// `start` published it stays unpublished). The verified holders keep
/// it: `start` asserts it after writing `stack` and `present` (which it
/// sets only to `thread.is_some() && !exited`, and clears `exited` only
/// while nothing is present), `main` asserts it after its exit's writes
/// (`present` false, `exited` true), and `fork_child` (the fork child's
/// writer through `Lock::get_unlocked`) after clearing both; `request`,
/// `start_requested` and `auto_tick` write nothing under it. Verus
/// checks nothing when a guard is released, so this is the claim. The
/// test-only readers (`join_exited`, `stack`, `thread`) never write
/// `stack`, `present` or `exited`, and no other code touches the value.
///
/// `api::tests::lock_holders_match_inv_docs` checks that the functions
/// that take the guard are the holders named here.
broadcast axiom fn axiom_state_lock_inv(v: State)
    ensures
        #[trigger] crate::lock::assumed_inv(v) == (v.stack + STACK_SIZE <= usize::MAX && (v.exited ==> !v.present)),
;

exec static STATE: Lock<State>
    ensures
        STATE.spec_raw() == state_lock(),
        STATE.rank() == rank_state(),
{
    proof {
        axiom_state_lock_inv(State { present: false, creating: false, exited: false, stack: 0, thread: None });
    }
    let l = Lock::new(State {
        present: false,
        creating: false,
        exited: false,
        stack: 0,
        thread: None,
    }, Ghost(rank_state()));
    proof {
        axiom_state_lock(l);
    }
    l
}
/// Mirror of `STATE.present` for the lock-free check on every tick.
exec static PRESENT: AtomicBool = AtomicBool::new(false);
/// `background_thread:true` (or, later, `mallctl("background_thread")`).
exec static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Futex word the thread sleeps on; bumped by [`wake`]. On a line of its
/// own: [`wake`]'s `fetch_add` runs while the thread is present, and the
/// statics placed next to it (`api::STATE`) are read by every `malloc`
/// and `free`.
exec static SEQ: Padded<AtomicU32> = Padded(AtomicU32::new(0));

/// Lazy start (the default, `background_thread` unset): nothing is
/// requested, but [`auto_tick`] requests and starts the thread once the
/// process holds [`AUTO_BURST`] dirty bytes, or [`AUTO_FLOOR`] for
/// [`AUTO_HOLD_NS`]. Cleared by an explicit [`request`] and for good by
/// [`AUTO_TRIES`] lazy starts that failed (a sandbox that refuses
/// `clone`, a thread limit the process sits at).
exec static AUTO: AtomicBool = AtomicBool::new(false);
/// Lazy starts that failed so far.
exec static AUTO_FAILS: AtomicU32 = AtomicU32::new(0);
/// Coarse time before which [`auto_tick`] does not look again.
exec static AUTO_NEXT: AtomicU64 = AtomicU64::new(0);
/// Coarse time of the first look of the current stretch at or above
/// [`AUTO_FLOOR`]; 0 when the last look was below it.
exec static AUTO_SINCE: AtomicU64 = AtomicU64::new(0);
/// The low-water mark of the dirty bytes since the fork that made this
/// process, starting from the dirty bytes at the fork (0 in a process
/// that never forked): what a fork child inherited is not its own, and
/// only what it adds above the inherited set (as that set decays) counts
/// towards a start.
exec static AUTO_BASE: AtomicU64 = AtomicU64::new(0);

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

} // verus!

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

verus! {

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
/// either way disarms the lazy start. The caller's record `fx` gets the
/// setting, read first if `cc`'s read was made ([`Change::Set`]).
pub fn request(on: bool, Tracked(cc): Tracked<&CCall>, Tracked(fx): Tracked<&mut Ghost<Change>>, Tracked(ord): Tracked<&mut LockOrder>)
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_state()),
    ensures
        *final(ord) == *old(ord),
        final(fx)@ == (Change::Set { read_first: cc.written() }),
{
    proof {
        *fx = Ghost(Change::Set { read_first: cc.written() });
    }
    {
        // Under the state lock, so a failing lazy start (`start_requested`)
        // cannot withdraw this request after it was made.
        let _g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
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
pub fn wake()
    requires
        crate::sys::may_syscall(),
{
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

fn kick()
    requires
        crate::sys::may_syscall(),
{
    SEQ.fetch_add(1, Ordering::SeqCst);
    // SAFETY: `SEQ` is a live, aligned `u32`.
    let _ = unsafe { sys::futex_wake(SEQ.as_ptr(), 1) };
}

/// Create the thread if it does not exist. `true` if one exists on
/// return; `false` if creation failed or is in progress on another
/// thread (the caller then purges in the foreground for this tick).
///
/// `errno` (the calling thread's, through its [`ThreadErrno`] token) is
/// preserved: this runs from an allocation's tick and from
/// `mallctl("background_thread")`, and glibc's `pthread_create` /
/// `pthread_join` may set it on their way.
/// `cell` is the calling thread's slot cell, which [`create`] parks the
/// slot through.
pub fn start(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>, Tracked(errno): Tracked<&mut ThreadErrno>, Tracked(cell): Tracked<&mut TlsCell>) -> bool
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_meta()),
    ensures
        final(cell).kept(*old(cell)),
        final(errno).val() == old(errno).val(),
        *final(ord) == *old(ord),
{
    broadcast use axiom_state_lock_inv;

    // Not inside the fork window (an atfork handler registered before
    // challoc's, on the forking thread with every lock held): the fork
    // handlers' bookkeeping of the thread (`fork_child` forgets it,
    // keeping its stack for a thread that cannot exist yet) assumes none
    // starts there.
    if crate::lock::in_fork_window() {
        return false;
    }
    let (stack, previous) = {
        let mut g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        if g.present {
            return true;
        }
        if g.creating {
            return false;
        }
        g.creating = true;
        g.exited = false;
        assert(lock_inv(g@));
        (g.stack, g.thread.take())
    };
    let saved_errno = get_errno(Tracked(&*errno));
    let (stack, thread) = create(shared, stack, previous, Tracked(ord), Tracked(cell));
    set_errno(saved_errno, Tracked(&mut *errno));
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
        g.present = thread.is_some() && !g.exited;
        PRESENT.store(g.present, Ordering::Relaxed);
        assert(lock_inv(g@));
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
pub fn start_requested(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>, Tracked(errno): Tracked<&mut ThreadErrno>, Tracked(cell): Tracked<&mut TlsCell>) -> bool
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_meta()),
    ensures
        final(cell).kept(*old(cell)),
        final(errno).val() == old(errno).val(),
        *final(ord) == *old(ord),
{
    // In the fork window `start` refuses; that is not a failed start, and
    // the lazy request stays for a tick after the fork.
    if crate::lock::in_fork_window() {
        return false;
    }
    if start(shared, Tracked(ord), Tracked(&mut *errno), Tracked(&mut *cell)) {
        return true;
    }
    let g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
    if !g.creating && !g.present && AUTO.load(Ordering::Relaxed) {
        REQUESTED.store(false, Ordering::Relaxed);
        // `+ 1` as the release build computes it (overflow checks are off
        // there): the count only ever reaches `AUTO_TRIES`, but it is an
        // atomic, whose loads Verus does not bound.
        let fails = AUTO_FAILS.load(Ordering::Relaxed).wrapping_add(1);
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
pub fn auto_tick(shared: &Shared, now: u64, alloc: bool, Tracked(ord): Tracked<&mut LockOrder>, Tracked(errno): Tracked<&mut ThreadErrno>, Tracked(cell): Tracked<Option<&mut TlsCell>>)
    requires
        alloc ==> cell is Some,
        crate::sys::may_abort() || (crate::sys::valid_aborts() && !alloc),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        cell matches Some(c) ==> final(c).kept(*c),
        final(errno).val() == old(errno).val(),
        *final(ord) == *old(ord),
{
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
    let dirty = crate::policy::empty_bytes(shared, Tracked(ord));
    let base = AUTO_BASE.load(Ordering::Relaxed).min(dirty);
    AUTO_BASE.store(base, Ordering::Relaxed);
    if !auto_due(dirty - base, now, &AUTO_SINCE) {
        return;
    }
    {
        // Under the state lock: an explicit `request` in between wins.
        let _g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        if !AUTO.load(Ordering::Relaxed) {
            return;
        }
        REQUESTED.store(true, Ordering::Relaxed);
    }
    if alloc {
        start_requested(shared, Tracked(ord), Tracked(&mut *errno), Tracked(cell.tracked_unwrap()));
    }
}

/// Whether `own` dirty bytes at `now` start the thread: at once from
/// [`AUTO_BURST`]; from [`AUTO_FLOOR`] once every look for
/// [`AUTO_HOLD_NS`] has seen at least that much. Updates the stretch,
/// `since` (the start of the current one, 0 for none). Whatever `since`
/// holds, a burst starts the thread and less than the floor does not.
pub(crate) fn auto_due(own: u64, now: u64, since: &AtomicU64) -> (r: bool)
    ensures
        own >= AUTO_BURST ==> r,
        own < AUTO_FLOOR ==> !r,
{
    if own >= AUTO_BURST {
        proof {
            assert(AUTO_FLOOR < AUTO_BURST) by (compute);
        }
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

} // verus!

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

verus! {

/// `pthread_join(t, NULL)`: 0 or an error number. Trusted: a raw libc
/// call; nothing is claimed about the result.
///
/// The join is a cancellation point, one of the two challoc reaches
/// (every other system call is a raw `syscall`, and `pthread_create`,
/// the attribute calls, `pthread_sigmask` and `pthread_setspecific` are
/// not cancellation points; the other is `malloc_info`'s `fwrite`,
/// `api::print::write_file`, design 8.1). It runs with cancellation disabled, so a
/// deferred `pthread_cancel` of the calling thread is acted on at the
/// program's next cancellation point instead of unwinding out of
/// [`start`] with its `creating` flag set (design 8.1, 14.1).
/// `cell` is the calling thread's slot cell: the join frees the exited
/// thread's key data, which may re-enter the allocator on this thread, so
/// the slot must be empty or hold a sentinel ([`crate::heap::reentry_benign`]),
/// as for `pthread_setspecific`. At a sentinel a nested entry leaves the
/// cell alone; at an empty slot a nested free's slow path (`heap::free`,
/// `heap_for_thread`) may create this thread's heap and fill the slot.
/// That path (`create_heap_for_thread`) parks the slot on `TLS_CREATING`
/// only while it registers the heap and leaves it empty or holding the
/// heap, never a sentinel, and so does a nested `create`, which puts back
/// what it parks ([`TlsCell::kept`]).
///
/// # Safety
/// `t` is a joinable thread this module created and never joined.
#[verifier::external_body]
#[inline(always)]
unsafe fn pthread_join(t: ::libc::pthread_t, Tracked(cell): Tracked<&mut TlsCell>) -> i32
    requires
        crate::sys::may_syscall(),
        old(cell).value() == 0 || crate::heap::reentry_benign(old(cell).value()),
    ensures
        final(cell).kept(*old(cell)),
{
    // glibc's, which the `libc` crate does not bind; the same on both
    // targets.
    const PTHREAD_CANCEL_DISABLE: i32 = 1;
    unsafe extern "C" {
        fn pthread_setcancelstate(state: i32, old: *mut i32) -> i32;
    }
    // SAFETY: guaranteed by the caller; `old` is a live int the
    // cancel-state calls write.
    unsafe {
        let mut old = 0;
        pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &mut old);
        let rc = ::libc::pthread_join(t, core::ptr::null_mut());
        pthread_setcancelstate(old, &mut old);
        rc
    }
}

/// Create the thread on `stack` (reserving the stack first if there is
/// none yet), after joining `previous`. Returns the stack (kept even
/// when creation fails, for the next attempt) and the new thread.
/// A slot holding a heap is parked on [`TLS_CREATING`] through the
/// caller's slot cell `cell` across the pthread calls, which require an
/// empty slot or a sentinel, and put back after: the cell is as it was
/// ([`TlsCell::kept`]), unless the slot was empty, when a nested free in
/// `pthread_join` or a nested allocation in `pthread_create` may create
/// the thread's heap. A heap the join created is parked the same way
/// for `pthread_create` and put back after it, so an empty slot ends
/// empty or holding a heap, never `TLS_CREATING`.
// The `match` stands for `Option::filter`, which vstd does not specify.
#[allow(clippy::manual_filter)]
fn create(
    shared: &Shared,
    stack: usize,
    previous: Option<::libc::pthread_t>,
    Tracked(ord): Tracked<&mut LockOrder>,
    Tracked(cell): Tracked<&mut TlsCell>,
) -> (r: (usize, Option<::libc::pthread_t>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        stack + STACK_SIZE <= usize::MAX,
        old(ord).allows(rank_meta()),
    ensures
        final(cell).kept(*old(cell)),
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
        // stack, as the code does: the regions' page is at most 64 KiB
        // (`Regions::separated`).
        let lg = shared.regions.lg_page;
        assert((1usize << lg) <= 0x4_0000usize) by (bit_vector)
            requires
                lg <= 16,
        ;
        assert(STACK_SIZE == 0x4_0000usize);
    }
    // The TLS slot is parked on `TLS_CREATING` for the pthread calls:
    // `pthread_create` allocates the new thread's DTV with `calloc`
    // and `pthread_join` frees an exited thread's key data, neither of
    // which may re-enter this thread's heap mid-operation.
    let ghost cell0 = *cell;
    let mut saved = crate::sys::tls::get(Tracked(&*cell));
    let mut park = saved as usize > TLS_EXITED;
    if park {
        crate::sys::tls::set(addr_to_ptr(TLS_CREATING) as *mut c_void, Tracked(&mut *cell));
    }
    // The previous thread exited at a wake-up (`request(false)`); the join
    // returns at the kernel's exit-time tid clear (CLONE_CHILD_CLEARTID),
    // after the thread has left its stack, so nothing runs on the stack when
    // it is handed to the next thread. /proc may list the exiting thread a
    // little longer. A fork child never has a `previous`: no thread on it.
    if let Some(t) = previous {
        // SAFETY: a joinable thread this module created and never joined.
        let rc = unsafe { pthread_join(t, Tracked(&mut *cell)) };
        if rc != 0 {
            sys::abort_fmt(format_args!(
                "pthread_join of the background thread failed: {}",
                sys::Errno(rc)
            ));
        }
        // At an empty slot the join's nested free may have created this
        // thread's heap: park it too, for `pthread_create`.
        if !park {
            saved = crate::sys::tls::get(Tracked(&*cell));
            park = saved as usize > TLS_EXITED;
            if park {
                crate::sys::tls::set(addr_to_ptr(TLS_CREATING) as *mut c_void, Tracked(&mut *cell));
            }
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
        // vstd does not specify `Option::filter`. The commit takes the
        // reservation's claim to the pages above the guard.
        let tracked mut claim = GhostSubmap::<int, bool>::dummy();
        match shared.meta.reserve(STACK_SIZE, page, Tracked(&mut claim), Tracked(ord)) {
            Some(s) => {
                proof {
                    let lg = shared.regions.lg_page;
                    assert(sub(0x4_0000usize, 1usize << lg) % (1usize << lg) == 0 && 0 < (1usize << lg) < 0x4_0000usize)
                        by (bit_vector)
                        requires
                            lg <= 16,
                    ;
                    vstd::arithmetic::div_mod::lemma_add_mod_noop(s as int, page as int, page as int);
                    vstd::arithmetic::div_mod::lemma_mod_self_0(page as int);
                    vstd::arithmetic::div_mod::lemma_small_mod(0, page as nat);
                    assert(set_int_range(s + page, (s + page) + (STACK_SIZE - page)) <= claim@.dom()) by {
                        assert forall|a: int| set_int_range(s + page, (s + page) + (STACK_SIZE - page)).contains(a)
                            implies claim@.dom().contains(a) by {
                            assert(set_int_range(s as int, s + STACK_SIZE).contains(a));
                        }
                    }
                }
                if shared
                    .meta
                    .commit(
                        s + page,
                        STACK_SIZE - page,
                        Ghost(true),
                        Tracked(&mut claim),
                        Tracked(ord),
                    )
                    .is_ok()
                {
                    Some(s)
                } else {
                    None
                }
            }
            None => None,
        }
    };
    let thread = match stack {
        Some(stack) => spawn(shared, stack + page, STACK_SIZE - page, Tracked(&mut *cell)),
        None => None,
    };
    if park {
        crate::sys::tls::set(saved, Tracked(&mut *cell));
    }
    proof {
        if cell0.value() != 0 {
            TlsCell::lemma_same(*cell, cell0);
        }
    }
    (stack.unwrap_or(0), thread)
}

/// `pthread_create` on the committed `[base, base + len)`, joinable so
/// a restart can wait for its exit. Signals are blocked
/// ([`sys::block_all_signals`]) so the new thread inherits them blocked
/// and never runs an application handler.
///
/// Trusted: raw pthread calls, and the thread's entry is a function
/// pointer. `shared` is well formed: the thread's [`shared_arg`] ensures
/// it of the pointer passed here.
/// `cell` is the calling thread's slot cell: `pthread_create` allocates
/// the new thread's DTV, which may re-enter the allocator on this thread,
/// so the slot must be empty or hold a sentinel, as for [`pthread_join`].
/// A nested entry leaves a sentinel in the slot as it found it and may
/// fill an empty slot with a heap, never a sentinel, as for the join
/// ([`TlsCell::kept`]).
#[verifier::external_body]
fn spawn(shared: &Shared, base: usize, len: usize, Tracked(cell): Tracked<&mut TlsCell>) -> Option<::libc::pthread_t>
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(cell).value() == 0 || crate::heap::reentry_benign(old(cell).value()),
    ensures
        final(cell).kept(*old(cell)),
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
            // Unverified: the signal-mask token is a placeholder until
            // `spawn` is verified.
            let mask = sys::block_all_signals(Tracked::assume_new());
            let mut tid: ::libc::pthread_t = 0;
            let rc = ::libc::pthread_create(
                &mut tid,
                &attr,
                main,
                shared as *const Shared as *mut c_void,
            );
            sys::set_signal_mask(mask, Tracked::assume_new());
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
/// given, which its contract requires to be well formed. `Shared::wf`
/// includes `api::ready()`, so this also asserts that the allocator is
/// published, for the new thread: `start`'s caller had it, and
/// `pthread_create` orders the thread after it. The deref itself would
/// verify through `api::axiom_published`; what is trusted is that `arg`
/// is that `Shared`, at its storage, which no contract can say of the
/// argument the C library passes.
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
    detach_from_starter();
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
            let ghost before = g@;
            g.present = false;
            g.exited = true;
            proof {
                axiom_state_lock_inv(before);
                axiom_state_lock_inv(g@);
            }
            assert(lock_inv(g@));
            PRESENT.store(false, Ordering::Relaxed);
            #[cfg(test)]
            EXITS.fetch_add(1, Ordering::SeqCst);
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

/// The thread inherits the scheduling of whichever thread created it, and
/// under the lazy start that is any allocating thread: a worker pinned to
/// one CPU, niced or real-time. Run on the new thread, this puts it on
/// `SCHED_OTHER` with the CPU affinity and nice value of the process's
/// main thread (the thread whose id is the pid) instead. Best effort:
/// what the kernel refuses (raising priority past `RLIMIT_NICE`, leaving
/// `SCHED_IDLE`, a main thread that has exited) stays as inherited.
///
/// Raw syscalls on the calling thread (pid argument 0) and reads of the
/// main thread's settings, all into locals; nothing is claimed.
fn detach_from_starter()
    requires
        crate::sys::may_syscall(),
{
    let _ = sys::sched_set_other();
    let pid = sys::getpid();
    // Zeroed: glibc's wrapper zeroes the bytes the kernel does not store.
    let mut set: sys::CpuSet = [0u64; 16];
    if sys::sched_getaffinity(pid, &mut set).is_ok() {
        let _ = sys::sched_setaffinity(0, &set);
    }
    // A failed getpriority is an `Err`, never a nice value.
    if let Ok(nice) = sys::getpriority(pid) {
        let _ = sys::setpriority(0, nice);
    }
}

/// The state lock, as [`fork_lock`] takes it: [`STATE`]'s raw lock.
/// Uninterpreted (Verus does not read statics in specifications);
/// `STATE`'s postcondition pins it ([`axiom_state_lock`]).
pub uninterp spec fn state_lock() -> RawLock;

/// Assumption: [`state_lock`] is the raw lock of [`STATE`]. Called once,
/// in `STATE`'s initialiser, for the lock it builds: a static is one
/// object, initialised once (at compile time), so every read of it
/// returns that lock. Private: in this module `STATE` is the only
/// `Lock<State>` (`State` is private too) and the only lock of rank
/// [`rank_state`], which the requirement pins (`purge::tests::
/// state_lock_axiom_names_the_one_state_lock` checks the three by name).
axiom fn axiom_state_lock(l: Lock<State>)
    requires
        l.rank() == rank_state(),
    ensures
        l.spec_raw() == state_lock(),
;

/// `&STATE`: its raw lock is [`state_lock`] and its rank the one `STATE`
/// is created with (the static's postcondition).
#[inline(always)]
fn state() -> (r: &'static Lock<State>)
    ensures
        r.spec_raw() == state_lock(),
        r.rank() == rank_state(),
{
    &STATE
}

/// Take the state lock (fork prepare, last: after the arena statistics
/// locks); the token is the hold, and `ord` (a root) is raised to its
/// rank. The last acquisition, after which the fork window may open
/// (`lock::fork_prepared`).
pub fn fork_lock(Tracked(ord): Tracked<&mut LockOrder>) -> (t: Tracked<LockHeld>)
    requires
        crate::sys::may_syscall(),
        old(ord).root(),
        old(ord).allows(rank_state()),
    ensures
        t@.held() == state_lock(),
        final(ord).root(),
        final(ord).top() == rank_state(),
        crate::lock::fork_prepared(),
{
    state().raw().lock_held(Tracked(ord))
}

/// Release the state lock (fork parent and child, before the others),
/// handing back the token of the hold, once the fork window has closed
/// (`lock::window_closed`).
pub fn fork_unlock(Tracked(t): Tracked<LockHeld>)
    requires
        crate::sys::may_syscall(),
        t.held() == state_lock(),
        crate::lock::window_closed(),
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
/// get one until it asks again or, under the lazy start, until it holds
/// enough dirty memory of its own: the dirty bytes at the fork are the
/// baseline ([`set_fork_base`], after the locks are released). Called
/// with the state lock held by this thread (`fork_prepare` took it;
/// `held` is the hold). Leaves the state's lock invariant true, so the
/// value the child's next locker reads is consistent.
pub fn fork_child(Tracked(held): Tracked<&mut LockHeld>)
    requires
        old(held).held() == state_lock(),
    ensures
        *final(held) == *old(held),
{
    // SAFETY: the forking thread holds the lock; the child is single
    // threaded.
    let st = unsafe { state().get_unlocked(Tracked(held)) };
    let ghost before = *st;
    st.present = false;
    st.creating = false;
    st.exited = false;
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
    AUTO_NEXT.store(0, Ordering::Relaxed);
    AUTO_SINCE.store(0, Ordering::Relaxed);
}

/// The lazy start's baseline in a fork child: `inherited`, the dirty
/// bytes at the fork, from which [`auto_tick`] keeps the low-water mark.
pub fn set_fork_base(inherited: u64) {
    AUTO_BASE.store(inherited, Ordering::Relaxed);
}

} // verus!

verus! {

/// The state lock's invariant, which [`axiom_state_lock_inv`] states; its
/// frame is uninterpreted.
impl crate::lock::LockSpec for State {
    open spec fn lock_spec_inv(self) -> bool {
        crate::lock::assumed_inv(self)
    }

    open spec fn lock_spec_frame(init: Self, v: Self) -> bool {
        crate::lock::assumed_frame(init, v)
    }
}

} // verus!
