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
//! (`pause` on x86_64, `isb` on aarch64); after [`SPINS`] rounds the waiter
//! falls through to the futex (design 5).
//!
//! Nothing on the `malloc`/`free` fast path takes a lock.

use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use vstd::cell::pcell::{PCell, PointsTo};
use vstd::prelude::*;

verus! {

/// The thread that holds every allocator lock for a `fork`
/// (`pthread_self`, the same for the forking thread in the child), from
/// the end of `api::fork_prepare` until the release in `api::fork_parent`
/// or `api::fork_child`; 0 otherwise. See [`fork_holder_is_me`].
exec static FORK_HOLDER: AtomicUsize = AtomicUsize::new(0);

/// The process ID when the fork window opened: the parent's. The forking
/// thread sees another in the child ([`in_child_fork_window`]).
exec static FORK_PID: AtomicU32 = AtomicU32::new(0);

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
const CONTENDED: u32 = 2;

/// Spins before the first sleep. Slow-path critical sections are short
/// (a few dozen instructions), so a holder is usually gone within this.
/// One round is one poll of the word and one `pause` (or `isb`), so the
/// phase's length follows that instruction's latency on the host CPU.
const SPINS: u32 = 100;

/// The bare mutex word. Use [`Lock`] when there is data to protect; use this
/// directly only where the data is elsewhere (fork handlers, state machines).
#[cfg_attr(not(verus_keep_ghost), repr(transparent))]
pub struct RawLock {
    state: AtomicU32,
    /// Ghost: the lock's rank in the lock order ([`LockOrder`]), fixed at
    /// creation. Verus-only, as is dropping `repr(transparent)` there:
    /// `repr(transparent)` rejects a `Ghost` field (its fields are
    /// private), and ordinary builds keep the one-word struct as it is.
    #[cfg(verus_keep_ghost)]
    rank: Ghost<int>,
}

impl Default for RawLock {
    fn default() -> Self {
        // A rank at most the state lock's, which [`RawLock::new`] requires.
        Self::new(Ghost(1))
    }
}

impl RawLock {
    /// A free lock of rank `rank` (ghost), at most the state lock's
    /// ([`rank_state`]), so none ranks above the lock `api::fork_prepare`
    /// takes last ([`fork_prepared`]).
    pub const fn new(Ghost(rank): Ghost<int>) -> (l: RawLock)
        requires
            rank <= rank_state(),
        ensures
            l.rank() == rank,
    {
        RawLock {
            state: AtomicU32::new(UNLOCKED),
            #[cfg(verus_keep_ghost)]
            rank: Ghost(rank),
        }
    }

    /// The lock's rank in the lock order.
    pub closed spec fn rank(self) -> int {
        self.rank@
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

    /// Acquire, blocking as needed. Opens any invariant (stated, though it
    /// is Verus's default): a function that declares a narrower mask
    /// cannot call it. Verified callers outside this module cannot meet
    /// the `lock_rs_only` requirement (its body is closed): they acquire
    /// through [`Lock::lock`] or [`RawLock::lock_held`], which check the
    /// lock order.
    #[inline]
    pub fn lock(&self)
        requires
            crate::sys::may_syscall(),
            lock_rs_only(),
        opens_invariants any
    {
        if !self.try_acquire() {
            self.lock_slow();
        }
    }

    /// Termination is not claimed: the wait ends when another thread
    /// releases the lock.
    #[cold]
    #[verifier::exec_allows_no_decreases_clause]
    fn lock_slow(&self)
        requires
            crate::sys::may_syscall(),
    {
        // The forking thread inside the fork window already holds it
        // ([`fork_holder_is_me`]): served as acquired, released as a no-op.
        if fork_holder_is_me() {
            return;
        }
        for _ in 0..SPINS {
            if self.state.load(Ordering::Relaxed) == UNLOCKED && self.try_acquire() {
                return;
            }
            core::hint::spin_loop();
        }
        // Announce a possible waiter, then sleep while the word stays
        // CONTENDED. A spurious wake or `EINTR` just loops: `swap` re-checks.
        let mut c = self.state.swap(CONTENDED, Ordering::Acquire);
        while c != UNLOCKED
            invariant
                crate::sys::may_syscall(),
        {
            wait(&self.state, CONTENDED);
            c = self.state.swap(CONTENDED, Ordering::Acquire);
        }
    }

    /// Release. Must be called by the holder exactly once per acquire.
    /// Private: every release outside this file goes through
    /// [`RawLock::unlock_held`], which takes the hold's token back, or a
    /// guard's drop. A public release without a token would let a caller
    /// release and re-take the lock, holding two [`LockHeld`] tokens for
    /// one lock and so two [`Lock::get_unlocked`] references.
    #[inline]
    fn release(&self)
        requires
            crate::sys::may_syscall(),
    {
        // An acquire the fork window served ([`RawLock::lock_slow`]): the
        // lock stays with the fork handlers.
        if fork_holder_is_me() {
            return;
        }
        if self.state.swap(UNLOCKED, Ordering::Release) == CONTENDED {
            wake(&self.state);
        }
    }

    /// [`RawLock::lock`], returning the hold as a [`LockHeld`] token, for
    /// code that holds several raw locks with no guard (the fork
    /// handlers).
    ///
    /// Lock order: the lock ranks above every lock `ord` bounds, and `ord`
    /// is raised to it. The hold outlives any guard, so `ord` must be a
    /// root ([`LockOrder::root`]), which no guard's scope bounds.
    #[inline(always)]
    pub fn lock_held(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (t: Tracked<LockHeld>)
        requires
            crate::sys::may_syscall(),
            old(ord).root(),
            old(ord).allows(self.rank()),
        ensures
            t@.held() == *self,
            final(ord).root(),
            final(ord).top() == self.rank(),
            self.rank() == rank_state() ==> fork_prepared(),
    {
        self.lock();
        proof {
            ord.top = self.rank();
        }
        Tracked(LockHeld { lock: *self })
    }

    /// [`RawLock::release`] by the holder, which hands its token back,
    /// once the fork window has closed ([`window_closed`]).
    #[inline(always)]
    pub fn unlock_held(&self, Tracked(t): Tracked<LockHeld>)
        requires
            crate::sys::may_syscall(),
            t.held() == *self,
            window_closed(),
    {
        self.release();
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
/// private to this module, so outside it only `lock_held` and
/// [`Lock::lock_held_perm`] build one; the fork handlers' handoff axioms
/// (`api::axiom_fork_resumed_state`, `api::resume::axiom_fork_resumed`)
/// assume theirs. The token is not `Copy`, so each token is handed back
/// at most once. A token names its lock by value (Verus has no addresses of
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

/// True, but only inside this module (the body is closed): the
/// requirement of the unchecked [`RawLock::lock`].
pub closed spec fn lock_rs_only() -> bool {
    true
}

/// True, but only inside this module (the body is closed), and ensured
/// only by [`RawLock::lock_held`] of a lock at [`rank_state`] (the
/// background thread's state lock, which `api::fork_prepare` takes last;
/// the test `purge::tests::state_lock_axiom_names_the_one_state_lock`
/// checks it is the one lock of that rank): the requirement of
/// [`fork_window_open`]. After that acquisition the root's top is
/// `rank_state`, which no lock ranks above ([`RawLock::new`]), so no
/// `lock_held` follows it on that token, and the window opens only once every lock is
/// taken: an acquire after the window opened would be served as acquired
/// ([`fork_holder_is_me`]) without the lock.
pub closed spec fn fork_prepared() -> bool {
    true
}

/// True, but only inside this module (the body is closed), and ensured
/// only by [`fork_window_close`]: the requirement of the token releases
/// ([`RawLock::unlock_held`], [`Lock::unlock_held_perm`]). While the window
/// is open [`RawLock::release`] does nothing for the forking thread, so a
/// fork handler's release before the window closed would leave its lock
/// held.
pub closed spec fn window_closed() -> bool {
    true
}

/// The explicit-cache table lock (`heap::Tcaches`), the outermost:
/// `tcache.create` and `tcache.destroy` hold it across the arena creation,
/// class and metadata locks.
pub open spec fn rank_tcaches() -> int {
    0x100_0000
}

/// The arena creation lock (`arena::Arenas`'s), below the class locks:
/// a build takes the class, slab-set and metadata locks under it.
pub open spec fn rank_arenas() -> int {
    0x200_0000
}

/// The process statistics lock (`heap::Shared::stats`): held across
/// `epoch`'s scan, which takes the class, slab table, huge, metadata and
/// arena statistics locks.
pub open spec fn rank_stats() -> int {
    0x280_0000
}

/// The class lock of class `c` (below `0x100`) of arena `i`
/// (`class::ClassState`), below the slab table locks.
pub open spec fn rank_class(i: int, c: int) -> int {
    0x300_0000 + i * 0x100 + c
}

/// The slab table lock of class `c` (`class::SlabTable`'s commit lock),
/// below the slab-set locks.
pub open spec fn rank_table(c: int) -> int {
    0x400_0000 + c
}

/// The slab-set lock of arena `i` (`class::SetTable`), below the
/// metadata lock.
pub open spec fn rank_sets(i: int) -> int {
    0x500_0000 + i
}

/// The huge lock (`huge::Huge`), below the metadata lock.
pub open spec fn rank_huge() -> int {
    0x600_0000
}

/// The metadata lock (`meta::Meta`).
pub open spec fn rank_meta() -> int {
    0x700_0000
}

/// The statistics lock of arena `i` (`arena::Arena`'s), above the
/// metadata lock: a leaf, under which no lock is taken.
pub open spec fn rank_arena_stats(i: int) -> int {
    0x900_0000 + i
}

/// The background thread's state lock (`purge::background`), above the
/// statistics locks: `fork_prepare` takes it last.
pub open spec fn rank_state() -> int {
    0xA00_0000
}

/// Above every lock's rank ([`RawLock::new`] requires it): a
/// [`LockOrder::spare`] allows nothing.
pub open spec fn rank_spare() -> int {
    0xB00_0000
}

/// Ghost: a bound on the ranks of the allocator locks its owner holds
/// ([`LockOrder::top`]). Every allocator lock has a rank, fixed when it
/// is created ([`RawLock::new`]). A blocking acquisition requires a token
/// whose top is below the lock's rank, so no set of threads can each wait
/// for a lock the next one holds: around such a cycle the ranks would
/// climb back to where they started. [`Lock::lock`] borrows the caller's
/// token for the guard's lifetime and hands out another, at the lock's
/// rank, for the code under the guard; [`RawLock::lock_held`] raises a
/// root in place. Every function that may take a lock takes its caller's
/// (`Tracked<&mut LockOrder>`), so the bound follows the call chain
/// (the exceptions are the entry points, whose token comes from
/// [`axiom_entry`] or [`axiom_fork_entry`] (each lists its uses)).
/// The fields are private to this module.
///
/// The token [`Lock::lock`] hands out has the lifetime `'a` of the
/// guard's borrow, which also borrows the caller's token: the caller's
/// lower bound is unusable until the guard and every use of the token
/// handed out are gone. A handed-out token used after its guard is
/// dropped bounds a lock no longer held, which only forbids more.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct LockOrder<'a> {
    ghost top: int,
    ghost root: bool,
    ghost life: PhantomData<&'a ()>,
}

impl<'a> LockOrder<'a> {
    /// Every allocator lock this token's owner holds ranks at most this
    /// (0: none).
    pub closed spec fn top(self) -> int {
        self.top
    }

    /// A root: kept for a whole entry point, not handed out under a guard
    /// ([`RawLock::lock_held`] needs one).
    pub closed spec fn root(self) -> bool {
        self.root
    }

    /// The owner may block on a lock of rank `r`.
    pub open spec fn allows(self, r: int) -> bool {
        self.top() < r
    }

    /// A token that allows nothing, to receive the one [`Lock::lock`]
    /// hands out.
    pub proof fn spare() -> (tracked o: LockOrder<'a>)
        ensures
            o.top() == rank_spare(),
            !o.root(),
    {
        LockOrder { top: rank_spare(), root: false, life: PhantomData }
    }

    /// A token for the code that runs while this one is lent: the same
    /// bound, not a root, and only for as long as the borrow of this one
    /// lasts (`'b`), so this one is unusable until the lent token and
    /// every guard taken with it are gone. For a token kept in a field of
    /// a type with an invariant, which Verus does not lend to an exec
    /// call as `&mut` (the report sink of `api::print`).
    pub proof fn lend<'b>(tracked &'b mut self) -> (tracked o: LockOrder<'b>)
        ensures
            o.top() == old(self).top(),
            !o.root(),
            *final(self) == *old(self),
    {
        LockOrder { top: self.top, root: false, life: PhantomData }
    }
}

/// Premise (an entry point): the calling thread holds no allocator lock.
/// Stands in for the token an entry point would hold for its whole call.
/// Each use is reached from a C entry point (or a thread's start) through
/// code that holds no allocator lock at that point. Its uses:
/// - `Huge::usable_size`, whose callers (`api::libc`'s `usable_size`, its
///   realloc and `free_usable` paths, and `Shared::zero_huge`) hold no
///   allocator lock;
/// - `Shared::check_size_hint`'s `Huge::lookup` on the sized-free slow
///   path, whose callers (`api::libc`'s `free_sized_slow` and
///   `api::jemalloc`'s `sdallocx_routed`) are reached from the C entry
///   points through code that takes no lock;
/// - `Heap::alloc_huge_in`'s `Shared::alloc_huge` and `Heap::free_huge`'s
///   `Shared::free_huge`, reached from the entry points and the slow
///   paths, which hold no allocator lock;
/// - `Shared::init`, which runs before the `Shared` is published, so no
///   allocator lock exists yet;
/// - the background thread's `main` (`purge::background`), since a new
///   thread holds no allocator lock;
/// - the report entry points of `api::print` (`malloc_stats_print`,
///   `malloc_info` and `malloc_stats`), one token per report: their C
///   caller holds no allocator lock, and `api::shared`, called first,
///   releases every lock it takes before it returns. `stats::Scratch::new`
///   takes the token before the sink is made; then the report's sink
///   holds the token and lends it to each call that may take a lock
///   (`stats::collect`, `stats::collect_arena`, `stats::collect_all`,
///   `Shared::cgroup_now`, `ClassState::is_partial`, `stats::Scratch::free`
///   and `policy::empty_bytes`) through `LockOrder::lend`, whose token
///   borrows the sink and is not a root, and the sink's flush (`emit`)
///   requires top 0;
/// - the readers of `api::mallctl` (under `mallctl` and `mallctlbymib`,
///   `batch_query` and `experimental.utilization.query` among them), one
///   token per reader or per `utilization` and `owner_of` call, and one
///   in `arena_figure` for its `stats::Scratch`'s `new` and `free` (which
///   take the metadata lock): the entry point holds no allocator lock,
///   `ctl` and the readers take none themselves, and each call the token
///   is passed to releases every lock it takes before it returns;
/// - `api::libc`'s `alloc_huge` without a thread heap, reached only from the
///   C entry points, and the entry point `malloc_trim`: their callers hold
///   no allocator lock, and `malloc_trim`'s
///   `Heap::flush_all` and `purge::purge_all` leave the token unchanged;
/// - `heap::alloc`, `heap::alloc_declined` and `heap::alloc_zeroed` without
///   a thread heap, `heap::free_direct`, and `heap::mallocx`'s
///   `alloc_direct`, `alloc_huge` and `no_arena` (`heap::mallocx::alloc`'s
///   check of an explicit `MALLOCX_ARENA`): reached from the C entry points
///   (`malloc`, `free` and the `mallocx` family) through code that holds
///   no allocator lock;
/// - `api::jemalloc`'s `get_defrag_hint`, one token for its
///   `ClassState::lowest_partial` and `ClassState::bin_stats` calls: a C
///   entry point, which holds no allocator lock, and each call releases the
///   class lock before it returns;
/// - `api::glibc::mallinfo2`'s `stats::Scratch` and `stats::collect_all`
///   calls: its callers (the C exports `mallinfo2` and `mallinfo`, the
///   latter through `api::glibc::mallinfo`, and the tests) hold no
///   allocator lock, and `api::shared`, called first, releases its locks
///   before it returns;
/// - the verification-only `api::theorem_read_only_never_aborts`, for its
///   `stats::Scratch` and `stats::collect_all` calls, which restate
///   `mallinfo2`'s;
/// - `api::fork_child`'s `policy::empty_bytes` call: it runs after
///   `fork_release` has handed back every lock token, the child is
///   single-threaded, and `empty_bytes` releases every lock it takes
///   before it returns;
/// - the scheduled pops: `Heap::pop_compact`'s probe (one token for its
///   `tick`, `Shared::arena`, `refill` and `probe_verdict` calls),
///   `Heap::pop_compact_run`'s `Shared::arena` and `refill` calls, and
///   `Heap::rebuild_run`, which `pop_compact_run` calls. They run from
///   `Heap::pop`, which `alloc_in`, `alloc_other` and `alloc_slow_in`
///   (after `refill`, whose `final(ord) == old(ord)` shows it released its
///   locks) call, and from `alloc_slow`, which calls `Heap::pop_compact`
///   directly for the scheduled pop `pop_fast` declined, none of them
///   holding an allocator lock at the call;
/// - `heap::thread::create_heap_for_thread`, reached from a thread's first
///   allocation on its slow path, with no lock held;
/// - `heap::thread::heap_destructor`, a TLS destructor that runs at thread
///   exit, outside any allocator call;
/// - the slow paths `Heap::alloc_slow_in` and `Heap::free_slow_in`, each
///   with one token for all its calls that take one (`tick`, `Shared::arena`,
///   `ensure_cache`, the refill or flush, the direct claim or release, and
///   `Heap::rehome_on_free`'s move), entered from a fast path or an
///   entry point, which hold no allocator lock, or (`free_slow_in`)
///   from the push after `Heap::rehome_on_free`'s move, whose
///   `Heap::try_rehome` releases every lock it takes before it returns.
///
/// No allocator entry point is re-entered while a lock is held: allocator
/// code calls nothing that may allocate with a lock held, and a signal
/// handler that allocates is outside the C contract (the fork handlers
/// block signals).
///
/// Checked by `lock::tests::entry_lock_order_uses_are_listed`, which pins
/// the functions that call it to the uses listed above: a new use is
/// listed here, with why no allocator lock is held at it, and added to
/// that test's list in the same change.
pub(crate) axiom fn axiom_entry<'a>() -> (tracked o: LockOrder<'a>)
    ensures
        o.top() == 0,
        !o.root(),
;

/// Premise (the fork prepare handler's entry): the calling thread holds no
/// allocator lock, as for [`axiom_entry`], and keeps this token for the
/// whole handler, which takes every lock with [`RawLock::lock_held`] and
/// releases none of them before it returns: a root. Its one use is
/// `api::fork_prepare` (the fork handlers block signals before their
/// first lock, so nothing re-enters the allocator under it).
///
/// A `fork` from a signal handler that interrupted this thread inside
/// the allocator, with a lock held, breaks the premise: `fork_prepare`
/// would then wait forever on that lock. `fork` runs the atfork handlers
/// and is not async-signal-safe in glibc or in POSIX.1-2024, which list
/// `_Fork` instead, so that call is outside the C contract, as the
/// allocating handler of [`axiom_entry`] is.
pub(crate) axiom fn axiom_fork_entry<'a>() -> (tracked o: LockOrder<'a>)
    ensures
        o.top() == 0,
        o.root(),
;

/// Whether the calling thread holds every allocator lock for a `fork`:
/// between the end of `api::fork_prepare` and the release in
/// `api::fork_parent` or `api::fork_child`. glibc runs the prepare handlers
/// registered before challoc's after it, and the parent and child handlers
/// registered before challoc's before its own, all on the forking thread
/// with every allocator lock held: an allocation from one of them takes
/// its locks as acquired and its releases do nothing, so it does not wait
/// on its own thread. Premise (trusted; `verus/ledger/src/lock.rs.md`,
/// design 9): the forking thread has no allocator critical section open
/// in that window (the prepare handler took every lock with every signal
/// blocked, and holds the data as the last holder left it), so the
/// allocation sees each lock's data as a fresh acquirer would, and only
/// that thread runs allocator code until the release. One relaxed load
/// when no fork is under way.
#[inline(always)]
fn fork_holder_is_me() -> bool {
    let h = FORK_HOLDER.load(Ordering::Relaxed);
    h != 0 && fork_holder_slow(h)
}

/// Whether the calling thread is inside the fork window
/// ([`fork_holder_is_me`]): what a step whose fork handling assumes it
/// cannot happen there (starting the background thread) checks to refuse.
#[inline(always)]
pub(crate) fn in_fork_window() -> bool {
    fork_holder_is_me()
}

/// `h` is the calling thread.
#[cold]
#[inline(never)]
fn fork_holder_slow(h: usize) -> bool {
    h == crate::sys::pthread_self()
}

/// Record the calling thread as the fork window's holder; the prepare
/// handler's last step, with every lock taken ([`fork_prepared`]).
pub(crate) fn fork_window_open()
    requires
        crate::sys::may_syscall(),
        fork_prepared(),
{
    FORK_PID.store(crate::sys::getpid() as u32, Ordering::Relaxed);
    FORK_HOLDER.store(crate::sys::pthread_self(), Ordering::Relaxed);
}

/// Whether the caller is the forking thread in the child of a `fork`,
/// inside the window: a child handler registered before challoc's, which
/// runs before challoc's own resets the work other parent threads had
/// under way (the feature line's build, the cgroup discovery). A wait for
/// that work there would never end (the threads are not in the child,
/// and every signal is blocked), so the waits reset it early, as the
/// child handler would, and do it themselves. Trusted: the capability
/// [`crate::sys::cgroup::in_fork_child`] stands for exactly this state
/// (the child's single thread, before any other runs).
#[verifier::external_body]
pub(crate) fn in_child_fork_window() -> (r: bool)
    requires
        crate::sys::may_syscall(),
    ensures
        r ==> crate::sys::cgroup::in_fork_child(),
{
    fork_holder_is_me() && FORK_PID.load(Ordering::Relaxed) != crate::sys::getpid() as u32
}

/// End the fork window; the parent and child handlers' step before their
/// release, which it allows ([`window_closed`]).
pub(crate) fn fork_window_close()
    ensures
        window_closed(),
{
    FORK_HOLDER.store(0, Ordering::Relaxed);
}

// The futex calls; neither returns anything the callers use.

#[inline]
fn wait(state: &AtomicU32, val: u32)
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: `state` is a live, aligned `u32` for the duration of the call.
    // Every error (EAGAIN: already changed; EINTR: signal) means "re-check".
    let _ = unsafe { crate::sys::futex_wait(state.as_ptr(), val, None) };
}

#[inline]
fn wake(state: &AtomicU32)
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: `state` is a live, aligned `u32`.
    let _ = unsafe { crate::sys::futex_wake(state.as_ptr(), 1) };
}

/// A [`RawLock`] guarding a value.
///
/// The value is a vstd [`PCell`]: reading or writing it takes the cell's
/// [`PointsTo`] permission (ghost). The raw lock's invariant holds that
/// permission while the lock is free: [`Lock::new`] hands it over, an
/// acquisition takes it out ([`axiom_acquired`]) and the guard's drop,
/// the release, puts it back; a holder of the raw lock with no guard
/// borrows it for as long as it borrows its token ([`axiom_held`]).
/// That a free lock's invariant owns the permission, and that the futex
/// mutex hands it to one holder at a time, is the mutex's contract,
/// which Verus does not check (the atomics return any value); the two
/// axioms state it. A `PCell` is a `ManuallyDrop<UnsafeCell<T>>`: the
/// layout and the code of the `UnsafeCell` it replaces, except that
/// dropping a `Lock` does not drop the value (no lock is dropped outside
/// the tests).
#[verifier::reject_recursive_types(T)]
pub struct Lock<T> {
    raw: RawLock,
    value: PCell<T>,
    /// Ghost: the value the lock was created with ([`lock_init`]).
    #[allow(dead_code)]
    init: Ghost<T>,
}

/// The invariant and frame of a locked type, stated by the type's owner:
/// [`lock_inv`] is `lock_spec_inv`, [`lock_frame`] is `lock_spec_frame`.
/// An owner may define them in its impl only when every holder of its
/// lock outside the tests ends with [`LockGuard::unlock`], which checks
/// both at each release, and `api::tests::UNLOCKED_LOCKS` lists the lock
/// with those holders (`api::tests::guards_end_in_unlock`). Otherwise
/// the impl keeps them uninterpreted ([`assumed_inv`], [`assumed_frame`])
/// and the owner states them by an axiom that names the holders. A frame
/// of `true` may be defined either way: no release can break it. A frame
/// an impl defines must hold of every value with itself: [`Lock::new`]
/// does not check it of the initial value, and [`axiom_acquired`] claims
/// it at the first acquisition. The two method names are defined nowhere
/// else in the crate (several locked types have an inherent `inv`), so
/// `lock::tests::lock_axioms_state_assumed_specs` finds every use of
/// them by name, in method, path or generic form.
pub trait LockSpec: Sized {
    spec fn lock_spec_inv(self) -> bool;

    spec fn lock_spec_frame(init: Self, v: Self) -> bool;
}

/// The invariant the value behind every `Lock<T>` keeps while unlocked.
///
/// Verified code that takes a lock relies on it: [`Lock::new`] requires it
/// of the initial value, and [`Lock::lock`] hands the value out with it.
/// A holder that ends with [`LockGuard::unlock`] is checked, since the
/// unlock requires it, and so is a [`Lock::lock_held_perm`] holder, whose
/// release ([`Lock::unlock_held_perm`]) requires it of the value it hands
/// back. Nothing checks a guard that drops instead, so for
/// those this is an assumption about the holder: a verified holder
/// ends with the value satisfying it, through the postcondition of the
/// last call that changes the value or an explicit `assert` before the
/// guard goes out of scope, unless the owner of the type names a premise
/// that holder rests on instead; an unverified one is trusted. A verified
/// holder must not move a tracked permission out of the value and keep it
/// past the guard: the next `lock` would hand out the invariant's
/// permission again. The owner of each locked type states what it means
/// for that type ([`LockSpec`]), and, where an axiom states it, lists its
/// unverified holders and the verified ones that rest on a premise. The
/// assumption enters at [`axiom_acquired`], and at [`axiom_held`] for a
/// holder of the raw lock.
pub open spec fn lock_inv<T: LockSpec>(v: T) -> bool {
    <T as LockSpec>::lock_spec_inv(v)
}

/// [`LockSpec::lock_spec_inv`] for a type whose owner states it by an
/// axiom. The axiom states this function, never [`lock_inv`] or the
/// trait method, so it constrains only a type whose impl delegates here:
/// a leftover axiom cannot contradict a real impl added later
/// (`lock::tests::lock_axioms_state_assumed_specs`).
pub uninterp spec fn assumed_inv<T>(v: T) -> bool;

/// The value a lock was created with.
pub closed spec fn lock_init<T>(l: Lock<T>) -> T {
    l.init@
}

/// What every value a lock holds keeps from the value it was created
/// with (its ghost constants, say), stated per type by the type's owner
/// ([`LockSpec`]), like [`lock_inv`].
pub open spec fn lock_frame<T: LockSpec>(init: T, v: T) -> bool {
    <T as LockSpec>::lock_spec_frame(init, v)
}

/// [`LockSpec::lock_spec_frame`] for a type whose owner states it by an
/// axiom, stated as for [`assumed_inv`].
pub uninterp spec fn assumed_frame<T>(init: T, v: T) -> bool;

/// The lock invariant's assumption where it enters for a guard (the other
/// entry is [`axiom_held`], for a holder of the raw lock): a thread that
/// has just acquired the raw lock of `l` takes the permission of `l`'s
/// cell out of the raw lock's invariant, and the value in the cell
/// satisfies [`lock_inv`] (every earlier holder restored it) and keeps
/// [`lock_frame`] of the value the lock was created with. Private, and
/// used only right after an acquire: by [`Lock::lock`] and
/// [`Lock::try_lock`], for the guard they build, which keeps the
/// permission in a field private to this module until its drop releases
/// the lock, and by [`Lock::lock_held_perm`], whose holder hands the
/// permission back with the token to [`Lock::unlock_held_perm`]. A call
/// while an earlier permission is alive would give two permissions to
/// one cell, and nothing but that discipline stops such a call. In the
/// fork window ([`fork_holder_is_me`]) an acquire the window serves is
/// also followed by this call, for a cell whose permission the fork
/// handlers' hold (`api::ForkHeld`) already has: the second permission
/// stands for that hold, used by the one thread that runs allocator code
/// there until the release, on the window's premise that the hold's data
/// is as the last holder left it. Every lock the window serves is one the
/// prepare handler took: no arena (and so no lock) is built there
/// (`Arenas::create`, `Arenas::materialise`).
///
/// Checked by `lock::tests::permission_axioms_are_called_only_at_acquisitions`.
axiom fn axiom_acquired<T: LockSpec>(l: Lock<T>) -> (tracked p: PointsTo<T>)
    ensures
        p.id() == l.value.id(),
        lock_inv(*p.value()),
        lock_frame(lock_init(l), *p.value()),
;

/// The lock invariant's assumption where it enters for a holder of the
/// raw lock ([`Lock::get_unlocked`]): the holder's [`LockHeld`] token
/// lends the permission of `l`'s cell for as long as the token stays
/// borrowed, and the value satisfies [`lock_inv`] and keeps
/// [`lock_frame`], as at [`axiom_acquired`]. Private, and used only by
/// `get_unlocked`, which hands out the value, never the permission: a
/// permission moved out of the borrow (`tracked_swap`) would outlive it,
/// and the next call would give a second one to the same cell.
///
/// Checked by `lock::tests::permission_axioms_are_called_only_at_acquisitions`.
axiom fn axiom_held<'a, T: LockSpec>(l: Lock<T>, tracked held: &'a mut LockHeld) -> (tracked p: &'a mut PointsTo<T>)
    requires
        old(held).held() == l.raw,
    ensures
        *final(held) == *old(held),
        p.id() == l.value.id(),
        lock_inv(*p.value()),
        lock_frame(lock_init(l), *p.value()),
;

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

verus! {

// SAFETY: access to `value` is serialised by `raw`, so sharing the lock
// across threads is sound whenever the value could be moved between them.
// Inside `verus!`, as external items: rustc numbers the impls outside a
// macro before those a macro expands to, and the number is part of the
// symbol of `RawLock::lock_slow`, which sets its place in `.text`.
#[verifier::external]
unsafe impl<T: Send> Sync for Lock<T> {}
// SAFETY: a `Lock` owns its value, so moving it moves only a `T: Send`.
#[verifier::external]
unsafe impl<T: Send> Send for Lock<T> {}

impl<T> Lock<T> {
    /// The value's address inside the lock at `p`, for building a large
    /// value in place (`Arena::put_stats`). Trusted: the address of the
    /// cell, whose value is its only data (`kani_lock` checks it for locks
    /// at four `T`s).
    #[verifier::external_body]
    #[inline(always)]
    pub(crate) fn value_ptr(p: *mut Lock<T>) -> *mut T {
        // SAFETY: a field projection; nothing is read or written.
        unsafe { core::ptr::addr_of_mut!((*p).value) as *mut T }
    }

    /// A lock of rank `rank` (ghost) guarding `value`, at most the state
    /// lock's ([`RawLock::new`]).
    pub const fn new(value: T, Ghost(rank): Ghost<int>) -> (l: Lock<T>)
        where T: LockSpec
        requires
            lock_inv(value),
            rank <= rank_state(),
        ensures
            lock_init(l) == value,
            l.rank() == rank,
    {
        // The permission goes to the free lock's invariant.
        let (cell, _perm) = PCell::new(value);
        Lock { raw: RawLock::new(Ghost(rank)), value: cell, init: Ghost(value) }
    }

    /// The lock's rank in the lock order ([`LockOrder`]).
    pub open spec fn rank(&self) -> int {
        self.spec_raw().rank()
    }

    /// Acquire and return a guard that releases on drop. Opens any
    /// invariant, as [`RawLock::lock`] does.
    ///
    /// Lock order: the lock ranks above every lock `ord` bounds. `ord` is
    /// borrowed for the guard's lifetime `'a`, and `under` receives the
    /// bound for the code under the guard, at the lock's rank, with the
    /// same lifetime: while the guard or `under` is in use, `ord` is not.
    #[inline]
    pub fn lock<'a>(&'a self, Tracked(ord): Tracked<&'a mut LockOrder<'_>>, Tracked(under): Tracked<&mut LockOrder<'a>>) -> (g: LockGuard<'a, T>)
        where T: LockSpec
        requires
            crate::sys::may_syscall(),
            old(ord).allows(self.rank()),
        ensures
            lock_inv(g@),
            lock_frame(lock_init(*self), g@),
            g.spec_lock() == *self,
            *final(ord) == *old(ord),
            final(under).top() == self.rank(),
            !final(under).root(),
        opens_invariants any
    {
        self.raw.lock();
        let tracked perm = axiom_acquired(*self);
        let g = LockGuard { lock: self, perm: Tracked(perm) };
        proof {
            *under = LockOrder { top: self.rank(), root: false, life: PhantomData };
        }
        g
    }

    /// [`Lock::lock`] for the tests, which keep no lock order tokens.
    #[cfg(test)]
    #[verifier::external]
    pub fn lock_t(&self) -> LockGuard<'_, T> where T: LockSpec {
        self.lock(Tracked::assume_new(), Tracked::assume_new())
    }

    /// Acquire without blocking.
    #[inline]
    #[cfg(test)]
    pub fn try_lock(&self) -> (r: Option<LockGuard<'_, T>>)
        where T: LockSpec
        ensures
            r matches Some(g) ==> lock_inv(g@) && lock_frame(lock_init(*self), g@) && g.spec_lock() == *self,
    {
        if self.raw.try_lock() {
            let tracked perm = axiom_acquired(*self);
            Some(LockGuard { lock: self, perm: Tracked(perm) })
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
    /// The value is the cell's, read through the permission the hold's
    /// token lends ([`axiom_held`]): as at [`axiom_acquired`] for
    /// [`Lock::lock`], the value is one a holder left, so it satisfies
    /// [`lock_inv`] and keeps [`lock_frame`]. The caller is a holder like
    /// any other and must leave the invariant true; a verified caller ends
    /// with an explicit `assert(lock_inv(*r))`. The caller shows its hold
    /// with the raw lock's [`LockHeld`] token (ghost), borrowed mutably for
    /// as long as the reference lives: two references to the value at once
    /// would be two values to Verus, each with the invariant's ghost state.
    #[inline]
    // Exclusivity is the caller's promise (`# Safety`).
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get_unlocked<'a>(&'a self, Tracked(held): Tracked<&'a mut LockHeld>) -> (r: &'a mut T)
        where T: LockSpec
        requires
            old(held).held() == self.spec_raw(),
        ensures
            *final(held) == *old(held),
            lock_inv(*r),
            lock_frame(lock_init(*self), *r),
    {
        let tracked perm = axiom_held(*self, held);
        self.value.borrow_mut(Tracked(perm))
    }

    /// [`RawLock::lock_held`] for a holder that also keeps the value:
    /// acquire, and return the hold's token with the permission of the
    /// cell, taken out of the free lock's invariant as [`Lock::lock`]
    /// takes it for its guard ([`axiom_acquired`]). The holder reads the
    /// value through it ([`Lock::value_held`]), and nothing can write the
    /// value while it keeps it: the fork handlers keep the creation
    /// lock's (`arena::Created`) from `fork_prepare` to the release, which
    /// hands the permission back with the token ([`Lock::unlock_held_perm`]).
    /// The caller must not take a guard of this lock (or call
    /// [`Lock::get_unlocked`]) while it keeps the permission, nor release
    /// the lock through `RawLock::unlock_held` instead: either would leave
    /// a second permission to the cell possible (the lock order already
    /// refuses a guard's scope here, `ord` must be a root). Crate-private,
    /// for the arena registry's creation lock (`Arenas::lock_held`).
    ///
    /// Lock order: as [`RawLock::lock_held`].
    #[inline(always)]
    pub(crate) fn lock_held_perm(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Tracked<(LockHeld, PointsTo<T>)>)
        where T: LockSpec
        requires
            crate::sys::may_syscall(),
            old(ord).root(),
            old(ord).allows(self.rank()),
        ensures
            r@.0.held() == self.spec_raw(),
            self.has_perm(r@.1),
            lock_inv(*r@.1.value()),
            lock_frame(lock_init(*self), *r@.1.value()),
            final(ord).root(),
            final(ord).top() == self.rank(),
    {
        self.raw.lock();
        proof {
            ord.top = self.rank();
        }
        Tracked((LockHeld { lock: self.raw }, axiom_acquired(*self)))
    }

    /// The value, read through the permission `perm` of the cell, which a
    /// holder keeps ([`Lock::lock_held_perm`]).
    #[inline(always)]
    pub(crate) fn value_held<'a>(&'a self, perm: Tracked<&'a PointsTo<T>>) -> (r: &'a T)
        requires
            self.has_perm(*perm@),
        ensures
            *r == *perm@.value(),
    {
        self.value.borrow(perm)
    }

    /// `p` is the permission of this lock's cell.
    pub closed spec fn has_perm(&self, p: PointsTo<T>) -> bool {
        p.id() == self.value.id()
    }

    /// [`RawLock::unlock_held`] for a [`Lock::lock_held_perm`] holder:
    /// release, handing back the token and the permission of the cell,
    /// which the free lock's invariant holds again, once the fork window
    /// has closed ([`window_closed`]). Like [`LockGuard::unlock`], it
    /// requires the value it hands back to keep [`lock_inv`] and
    /// [`lock_frame`], so the next acquisition's claim of them
    /// ([`axiom_acquired`]) is checked here.
    #[inline(always)]
    pub(crate) fn unlock_held_perm(&self, Tracked(t): Tracked<LockHeld>, Tracked(perm): Tracked<PointsTo<T>>)
        where T: LockSpec
        requires
            crate::sys::may_syscall(),
            t.held() == self.spec_raw(),
            self.has_perm(perm),
            lock_inv(*perm.value()),
            lock_frame(lock_init(*self), *perm.value()),
            window_closed(),
    {
        self.raw.release();
    }
}

/// Exclusive access to a [`Lock`]'s value; unlocks on drop. Holds the
/// permission of the lock's cell, taken at the acquisition
/// ([`axiom_acquired`]), until the drop releases the lock.
#[verifier::reject_recursive_types(T)]
pub struct LockGuard<'a, T> {
    lock: &'a Lock<T>,
    /// Ghost: the permission of the lock's cell.
    #[allow(dead_code)]
    perm: Tracked<PointsTo<T>>,
}

impl<'a, T> LockGuard<'a, T> {
    /// The permission is the cell's.
    #[verifier::type_invariant]
    closed spec fn wf(&self) -> bool {
        self.perm@.id() == self.lock.value.id()
    }

    /// The lock this guard holds.
    pub closed spec fn spec_lock(&self) -> Lock<T> {
        *self.lock
    }

    /// The permission of the lock's cell, lent for as long as the guard
    /// is borrowed: a ghost argument for a callee that must know a value
    /// is the one this guard holds ([`Lock::has_perm`]). Read-only, like
    /// [`Deref`]: nothing writes the value through it.
    #[inline(always)]
    pub fn perm(&self) -> (p: Tracked<&PointsTo<T>>)
        ensures
            self.spec_lock().has_perm(*p@),
            *p@.value() == self@,
    {
        proof {
            use_type_invariant(self);
        }
        Tracked(self.perm.borrow())
    }

    /// Release the lock, handing the value back with the lock's invariant
    /// and frame restored ([`lock_inv`], [`lock_frame`]): the explicit
    /// unlock a verified holder ends with, which checks what the free
    /// lock's invariant takes back at the next [`axiom_acquired`]. The
    /// release is the guard's drop, at the end of the body.
    #[inline(always)]
    pub fn unlock(self)
        where T: LockSpec
        requires
            crate::sys::may_syscall(),
            lock_inv(self@),
            lock_frame(lock_init(self.spec_lock()), self@),
    {
    }
}

impl<'a, T> View for LockGuard<'a, T> {
    type V = T;

    /// The guarded value: the cell's, through the guard's permission.
    closed spec fn view(&self) -> T {
        *self.perm@.value()
    }
}

impl<T> Deref for LockGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> (r: &T)
        ensures
            *r == self@,
    {
        proof {
            use_type_invariant(self);
        }
        cell_ref(&self.lock.value, &self.perm)
    }
}

impl<T> DerefMut for LockGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> (r: &mut T)
        ensures
            *r == old(self)@,
            *final(r) == final(self)@,
            final(self).spec_lock() == old(self).spec_lock(),
    {
        proof {
            use_type_invariant(&*self);
        }
        cell_mut(&self.lock.value, &mut self.perm)
    }
}

// The guard's two reads of its cell: [`PCell::borrow`] and
// [`PCell::borrow_mut`] with the permission taken from the guard's
// field. `c.borrow(Tracked(t.borrow()))` and
// `c.borrow_mut(Tracked(t.borrow_mut()))` verify as they are, but every
// `Tracked(..)` expression in exec code erases to
// `Tracked::assume_new_fallback(|| unreachable!())`, and that closure,
// instantiated in `deref` and `deref_mut` for every guarded type, changed
// the machine code of `Shared::free_huge` (an extra range check of the
// run state byte) and `ClassState::claim_with` (block order) on x86_64.
// These two pass `Tracked::assume_new()` instead. Trusted: each contract
// is the vstd one of the call it makes, restated for a permission held by
// reference (`&Tracked<PointsTo<T>>`, `&mut Tracked<PointsTo<T>>`) rather
// than passed as `Tracked<&PointsTo<T>>`.

/// [`PCell::borrow`] with the permission `t`.
#[verifier::external_body]
#[inline(always)]
#[allow(unused_variables)]
fn cell_ref<'b, T>(c: &'b PCell<T>, t: &'b Tracked<PointsTo<T>>) -> (r: &'b T)
    requires
        t@.id() == c.id(),
    ensures
        *r == *t@.value(),
    opens_invariants none
    no_unwind
{
    c.borrow(Tracked::assume_new())
}

/// [`PCell::borrow_mut`] with the permission `t`.
#[verifier::external_body]
#[inline(always)]
#[allow(unused_variables)]
fn cell_mut<'b, T>(c: &'b PCell<T>, t: &'b mut Tracked<PointsTo<T>>) -> (r: &'b mut T)
    requires
        old(t)@.id() == c.id(),
    ensures
        *r == *old(t)@.value(),
        *final(r) == *final(t)@.value(),
        final(t)@.id() == c.id(),
    opens_invariants none
    no_unwind
{
    c.borrow_mut(Tracked::assume_new())
}

// Trusted: Verus wants a destructor that opens no invariant and cannot
// unwind, and `RawLock::release` is not marked either way. The body is the
// release, and claims nothing; its `no_unwind` rests on the
// `panic = "abort"` profiles (test builds unwind).
impl<T> Drop for LockGuard<'_, T> {
    #[inline]
    #[verifier::external_body]
    fn drop(&mut self)
        opens_invariants none
        no_unwind
    {
        self.lock.raw.release();
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    // The values the tests below lock; their invariant is not stated.
    impl LockSpec for u32 {}
    impl LockSpec for u64 {}
    impl<T> LockSpec for Vec<T> {}

    /// Every lock axiom outside this file states [`assumed_inv`] or
    /// [`assumed_frame`], never [`lock_inv`], [`lock_frame`] or a
    /// [`LockSpec`] method: an axiom on those could contradict a real impl
    /// added later and prove `false`, while one on the `assumed_*`
    /// functions constrains only the types whose impls delegate to them.
    /// The four names are matched as words, so a method call
    /// (`v.lock_spec_inv()`), a type path (`Inner::lock_spec_inv(v)`), a
    /// generic call (`T::lock_spec_inv(v)`) and a turbofish all count, and
    /// no function outside a `LockSpec` impl takes the method names. No lock
    /// axiom names, in its parameters, a type whose impl defines the
    /// function the axiom states (does not delegate it): such an axiom
    /// constrains nothing the type's holders use, so it is dead and its
    /// doc misleads. An axiom is an `axiom fn` item through the first line
    /// that ends in `;`, and a lock axiom one that names any of these
    /// functions. Types are compared by path, a bare name read as an item
    /// of the module whose file names it. Not seen: an `assumed_*` call
    /// spelled with a space before `(`, a `use .. as` rename, a trusted
    /// fact outside an `axiom fn` (an `external_body` proof, an `assume`),
    /// and a type named only inside `ensures`. `lock.rs`, whose axioms
    /// (`axiom_acquired`, `axiom_held`) are generic over `T`, is not
    /// scanned. What the scan reads each impl to define is what `DEFINED`
    /// lists, so a lock whose axioms were retired is one the scan sees,
    /// and a leftover axiom on it fails here.
    #[test]
    fn lock_axioms_state_assumed_specs() {
        use std::string::{String, ToString};
        // (type, defines `inv`, defines `frame`) for each impl that defines
        // either; every other impl delegates both.
        const DEFINED: &[(&str, bool, bool)] = &[
            ("crate::arena::Created", true, true),
            ("crate::class::Inner", true, true),
            ("crate::class::sets::[Lazy; SET_GROUPS]", true, true),
            ("crate::class::table::TableInner", true, true),
            ("crate::heap::tcaches::()", true, true),
            ("crate::huge::Inner", true, true),
            ("crate::meta::Inner", true, true),
            ("crate::stats::ArenaStats", true, true),
            ("crate::stats::Snapshot", true, true),
        ];
        // Spelled in parts, so that this test's own lines match nothing.
        let named = [
            std::format!("{}{}", "lock_", "inv"),
            std::format!("{}{}", "lock_", "frame"),
            std::format!("{}{}", "lock_spec_", "inv"),
            std::format!("{}{}", "lock_spec_", "frame"),
        ];
        let assumed_inv = std::format!("{}{}", "assumed_", "inv(");
        let assumed_frame = std::format!("{}{}", "assumed_", "frame(");
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        // A use of `name`, not the tail of a longer name.
        let calls =
            |t: &str, name: &str| t.match_indices(name).any(|(i, _)| !t[..i].ends_with(ident));
        // `name` as a whole word.
        let word = |t: &str, name: &str| {
            t.match_indices(name)
                .any(|(i, _)| !t[..i].ends_with(ident) && !t[i + name.len()..].starts_with(ident))
        };
        let module = |rel: &str| {
            let stem = rel.strip_suffix(".rs").unwrap();
            let stem = stem.strip_suffix("/mod").unwrap_or(stem);
            std::format!("crate::{}", stem.replace('/', "::"))
        };
        let key = |m: &str, ty: &str| {
            let ty = ty.trim();
            if ty.starts_with("crate::") {
                ty.to_string()
            } else {
                std::format!("{m}::{ty}")
            }
        };
        // Per impl: (type, where, defines `inv`, defines `frame`); per lock
        // axiom: (where, module, text).
        let mut impls: Vec<(String, String, bool, bool)> = Vec::new();
        let mut axioms: Vec<(String, String, String)> = Vec::new();
        let for_ = std::format!("{}{}", "LockSpec", " for ");
        for (rel, text) in crate::testutil::shipped_sources() {
            if rel == "lock.rs" {
                continue;
            }
            let m = module(&rel);
            let lines: Vec<&str> = text.lines().collect();
            let mut i = 0;
            while i < lines.len() {
                let t = lines[i].trim_start();
                let at = std::format!("{rel}:{}", i + 1);
                if t.starts_with("impl") && t.contains(for_.as_str()) {
                    let rest = &t[t.find(for_.as_str()).unwrap() + for_.len()..];
                    let ty = rest.split(" where").next().unwrap().trim_end();
                    let ty = ty.trim_end_matches('}').trim_end_matches('{').trim_end();
                    let indent = &lines[i][..lines[i].len() - t.len()];
                    let mut body = String::new();
                    if !t.trim_end().ends_with('}') {
                        i += 1;
                        while lines[i] != std::format!("{indent}}}") {
                            body.push_str(lines[i]);
                            body.push('\n');
                            i += 1;
                        }
                    }
                    impls.push((
                        key(&m, ty),
                        at,
                        !calls(&body, &assumed_inv),
                        !calls(&body, &assumed_frame),
                    ));
                } else if named[2..].iter().any(|n| word(t, &std::format!("fn {n}"))) {
                    // The names stay unique: no item outside a `LockSpec`
                    // impl defines them.
                    panic!("{at}: a function named as a `LockSpec` method outside an impl of it");
                } else if !t.starts_with("//") && t.contains("axiom fn ") {
                    let mut item = String::new();
                    loop {
                        item.push_str(lines[i]);
                        item.push('\n');
                        if lines[i].trim_end().ends_with(';') {
                            break;
                        }
                        i += 1;
                    }
                    if named.iter().any(|n| word(&item, n))
                        || [&assumed_inv, &assumed_frame]
                            .iter()
                            .any(|n| calls(&item, n))
                    {
                        axioms.push((at, m.clone(), item));
                    }
                }
                i += 1;
            }
        }
        assert!(!impls.is_empty(), "no `LockSpec` impl found");
        assert!(!axioms.is_empty(), "no lock axiom found");
        for (k, imp, inv, frame) in &impls {
            let want = DEFINED
                .iter()
                .find(|d| d.0 == k.as_str())
                .map_or((false, false), |d| (d.1, d.2));
            assert!(
                (*inv, *frame) == want,
                "{imp}: `{k}`'s `LockSpec` impl defines (inv, frame) {:?}; `DEFINED` says {want:?}",
                (*inv, *frame)
            );
        }
        for d in DEFINED {
            assert!(
                impls.iter().any(|(k, ..)| k == d.0),
                "`DEFINED` names `{}`, which has no `LockSpec` impl",
                d.0
            );
        }
        for (at, m, item) in &axioms {
            for n in &named {
                assert!(
                    !word(item, n),
                    "{at}: a lock axiom names `{n}`; state `assumed_inv`/`assumed_frame` instead"
                );
            }
            // The parameters' types: the first `(..)` after the name, split
            // at its top-level commas.
            let name = item.find("axiom fn ").unwrap();
            let open = name + item[name..].find('(').unwrap();
            let mut depth = 0;
            let mut params = Vec::new();
            let mut cur = String::new();
            for c in item[open + 1..].chars() {
                match c {
                    '(' | '[' | '<' => depth += 1,
                    ')' | ']' | '>' if depth > 0 => depth -= 1,
                    ')' => break,
                    ',' if depth == 0 => {
                        params.push(core::mem::take(&mut cur));
                        continue;
                    }
                    _ => {}
                }
                cur.push(c);
            }
            params.push(cur);
            for p in params.iter().filter_map(|p| p.split_once(':')) {
                let ty = key(m, p.1);
                for (k, imp, inv, frame) in &impls {
                    let dead = (*inv && calls(item, &assumed_inv))
                        || (*frame && calls(item, &assumed_frame));
                    assert!(
                        !(*k == ty && dead),
                        "{at}: a lock axiom on `{ty}` states a function its `LockSpec` impl at \
                         {imp} defines; remove the axiom"
                    );
                }
            }
        }
    }

    /// `axiom_acquired` and `axiom_held` mint a permission of a lock's
    /// cell, and two alive at once would prove `false` (vstd's
    /// `PointsTo::is_exclusive`): the first is called only where
    /// `Lock::lock` and `Lock::try_lock` build their guard, the second
    /// only in `Lock::get_unlocked`. Both are private to this file.
    #[test]
    fn permission_axioms_are_called_only_at_acquisitions() {
        // Spelled in parts, so that this test's own lines match nothing.
        // A call is the name then `(` or a turbofish `::`.
        let acquired = std::format!("{}{}", "axiom_", "acquired");
        let held = std::format!("{}{}", "axiom_", "held");
        let calls = |t: &str, name: &str| {
            t.contains(std::format!("{name}(").as_str())
                || t.contains(std::format!("{name}::").as_str())
        };
        let (mut a, mut h) = (Vec::new(), Vec::new());
        let mut func = "";
        for line in include_str!("lock.rs").lines() {
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            if let Some(i) = t.find("fn ") {
                func = t[i + 3..].split(['(', '<']).next().unwrap();
            }
            if calls(t, &acquired) && !t.starts_with("axiom fn") {
                a.push(func);
            }
            if calls(t, &held) && !t.starts_with("axiom fn") {
                h.push(func);
            }
        }
        assert_eq!(a, ["lock", "try_lock", "lock_held_perm"]);
        assert_eq!(h, ["get_unlocked"]);
    }

    /// `axiom_entry` asserts that no allocator lock is held where it is
    /// called, which only its caller's context shows: each use is pinned
    /// to the functions its doc lists, so a new one fails here until the
    /// doc argues its premise (a count of 1 where the doc says one token,
    /// otherwise none: some take one token per call they make).
    #[test]
    fn entry_lock_order_uses_are_listed() {
        use crate::testutil::{ANY, assert_only_at};
        assert_only_at(
            concat!("axiom_", "entry("),
            &[
                // `Huge::usable_size`, `Shared::check_size_hint`.
                ("huge/mod.rs:", "pub fn usable_size(", ANY),
                ("heap/shared.rs:", "pub fn check_size_hint(", ANY),
                // `Heap::alloc_huge_in`, `Heap::free_huge`.
                ("heap/paths.rs:", "pub fn alloc_huge_in(", ANY),
                ("heap/paths.rs:", "pub(super) fn free_huge(", ANY),
                // `Shared::init`, the background thread's `main`.
                ("heap/shared.rs:", "pub unsafe fn init(", ANY),
                ("purge/background.rs:", "extern \"C\" fn main(", ANY),
                // The report entry points of `api::print`, one token per report.
                ("api/print.rs:", "fn stats_print(", 1),
                ("api/print.rs:", "pub fn malloc_info(", 1),
                ("api/print.rs:", "pub fn malloc_stats(", 1),
                // The readers of `api::mallctl`.
                ("api/mallctl.rs:", "fn cgroup_empty_bytes(", ANY),
                ("api/mallctl.rs:", "fn stats_allocated(", ANY),
                ("api/mallctl.rs:", "fn stats_active(", ANY),
                ("api/mallctl.rs:", "fn stats_metadata(", ANY),
                ("api/mallctl.rs:", "fn stats_resident(", ANY),
                ("api/mallctl.rs:", "fn stats_mapped(", ANY),
                ("api/mallctl.rs:", "fn stats_retained(", ANY),
                ("api/mallctl.rs:", "pub fn utilization(", ANY),
                ("api/mallctl.rs:", "pub fn owner_of(", ANY),
                ("api/mallctl.rs:", "fn tracked(", ANY),
                ("api/mallctl.rs:", "fn arena_stats_into(", ANY),
                ("api/mallctl.rs:", "fn arena_figure(", ANY),
                ("api/mallctl.rs:", "fn ctl(", ANY),
                // `api::libc`'s `alloc_huge` and `malloc_trim`.
                ("api/libc.rs:", "fn alloc_huge(", ANY),
                ("api/libc.rs:", "pub fn malloc_trim(", ANY),
                // The heap entry points without a thread heap.
                ("heap/paths.rs:", "pub fn alloc(", ANY),
                ("heap/paths.rs:", "pub fn alloc_declined(", ANY),
                ("heap/paths.rs:", "pub fn alloc_zeroed(", ANY),
                ("heap/paths.rs:", "pub fn free_direct(", ANY),
                ("heap/mallocx.rs:", "fn alloc_direct(", ANY),
                ("heap/mallocx.rs:", "pub fn alloc_huge(", ANY),
                ("heap/mallocx.rs:", "fn no_arena(", ANY),
                // `api::jemalloc::get_defrag_hint`.
                ("api/jemalloc.rs:", "pub fn get_defrag_hint(", ANY),
                // `api::glibc::mallinfo2`'s `stats::collect_all` call.
                ("api/glibc.rs:", "pub fn mallinfo2(", 1),
                // `api::theorem_read_only_never_aborts`, `api::fork_child`.
                ("api/mod.rs:", "fn theorem_read_only_never_aborts(", ANY),
                ("api/mod.rs:", "extern \"C\" fn fork_child(", ANY),
                // The scheduled pops.
                ("heap/compact.rs:", "pub(super) unsafe fn pop_compact(", ANY),
                (
                    "heap/compact.rs:",
                    "pub(super) unsafe fn pop_compact_run(",
                    ANY,
                ),
                ("heap/compact.rs:", "pub(super) unsafe fn rebuild_run(", ANY),
                // A thread's first heap and its destructor.
                ("heap/thread.rs:", "fn create_heap_for_thread<", ANY),
                (
                    "heap/thread.rs:",
                    "pub unsafe extern \"C\" fn heap_destructor(",
                    ANY,
                ),
                // The slow paths.
                ("heap/paths.rs:", "pub(super) fn alloc_slow_in(", ANY),
                ("heap/paths.rs:", "pub(super) fn free_slow_in(", ANY),
            ],
        );
    }

    #[test]
    fn raw_lock_state_machine() {
        let l = RawLock::new(Ghost::assume_new());
        assert!(!l.is_locked());
        assert!(l.try_lock());
        assert!(l.is_locked());
        assert!(!l.try_lock());
        l.release();
        assert!(!l.is_locked());
        assert_eq!(l.state.load(Ordering::Relaxed), UNLOCKED);
        l.lock();
        assert!(!l.try_lock());
        l.release();
        assert!(!l.is_locked());
    }

    #[test]
    fn guard_releases_on_drop() {
        let l = Lock::new(5u32, Ghost::assume_new());
        {
            let mut g = l.lock_t();
            *g += 1;
            assert!(l.try_lock().is_none());
            assert!(l.raw().is_locked());
        }
        assert!(!l.raw().is_locked());
        assert_eq!(*l.lock_t(), 6);
        assert_eq!(*l.try_lock().unwrap(), 6);
    }

    /// Many threads hammer one counter: every increment must land, and the
    /// lock must end up unlocked with no waiter bit stuck.
    #[test]
    fn contended_counter_is_exact() {
        const THREADS: u64 = 8;
        const ITERS: u64 = 50_000;
        let l = Lock::new(0u64, Ghost::assume_new());
        std::thread::scope(|s| {
            for _ in 0..THREADS {
                s.spawn(|| {
                    for _ in 0..ITERS {
                        *l.lock_t() += 1;
                    }
                });
            }
        });
        assert_eq!(*l.lock_t(), THREADS * ITERS);
        assert_eq!(l.raw().state.load(Ordering::Relaxed), UNLOCKED);
    }

    /// Long holds force the futex path (spinning cannot outlast them) and
    /// every waiter must still get through.
    #[test]
    fn sleepers_are_woken() {
        let l = Lock::new(Vec::new(), Ghost::assume_new());
        std::thread::scope(|s| {
            for t in 0..4u32 {
                let l = &l;
                s.spawn(move || {
                    for i in 0..20u32 {
                        let mut g = l.lock_t();
                        g.push(t * 100 + i);
                        std::thread::sleep(std::time::Duration::from_micros(200));
                    }
                });
            }
        });
        {
            let v = l.lock_t();
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
        let l = Arc::new(RawLock::new(Ghost::assume_new()));
        l.lock();
        let waiter = {
            let l = Arc::clone(&l);
            std::thread::spawn(move || {
                l.lock();
                let acquired_at = Instant::now();
                l.release();
                acquired_at
            })
        };
        // The announce: the spin phase ends with the swap to `CONTENDED`.
        if !within_bound(|| l.state.load(Ordering::Relaxed) == CONTENDED) {
            l.release();
            panic!("the waiter did not announce itself within {BOUND:?}");
        }
        // Parked: while the lock stays held the word stays `CONTENDED`
        // and the waiter does not get through. Read, then release, then
        // assert, so a failure leaves nothing parked.
        std::thread::sleep(Duration::from_millis(20));
        let word = l.state.load(Ordering::Relaxed);
        let through = waiter.is_finished();
        let released_at = Instant::now();
        l.release();
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

/// Kani harness for [`Lock::value_ptr`] (`cfg(kani)` only; the value
/// field is private, so it sits here).
#[cfg(kani)]
mod kani_lock {
    use super::*;
    use core::mem::{MaybeUninit, align_of, offset_of, size_of};

    /// `Lock::value_ptr`, no `requires`, no `ensures`: on a pointer to a
    /// lock of `T` (here uninitialised: the call reads nothing), at `T` a
    /// byte, a word, a padded pair and `ArenaStats` (`Arena::put_stats`'
    /// instance), it returns, and (beyond the contract) the pointer is the
    /// value field's: its `T` fills the cell, lies inside the lock and
    /// apart from the lock word, is aligned for `T` and is writable.
    #[kani::proof]
    fn value_ptr_is_the_value_field() {
        fn on<T>() {
            let mut l = MaybeUninit::<Lock<T>>::uninit();
            let p = l.as_mut_ptr();
            let r = Lock::value_ptr(p);
            let off = r as usize - p as usize;
            assert_eq!(off, offset_of!(Lock<T>, value));
            assert_eq!(size_of::<PCell<T>>(), size_of::<T>());
            assert!(off + size_of::<T>() <= size_of::<Lock<T>>());
            let w = offset_of!(Lock<T>, raw);
            assert!(w + size_of::<RawLock>() <= off || off + size_of::<T>() <= w);
            assert_eq!(r as usize % align_of::<T>(), 0);
            assert!(kani::mem::can_write(r));
        }
        on::<u8>();
        on::<u64>();
        on::<(u64, u8)>();
        on::<crate::stats::ArenaStats>();
    }
}
