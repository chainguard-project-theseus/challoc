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
use core::sync::atomic::{AtomicU32, Ordering};
use vstd::cell::pcell::{PCell, PointsTo};
use vstd::prelude::*;

verus! {

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
const CONTENDED: u32 = 2;

/// Spins before the first sleep. Slow-path critical sections are short
/// (a few dozen instructions), so a holder is usually gone within this.
/// One round is one poll of the word and one `pause` (or `isb`); the
/// phase is 5..15 us.
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
        // A rank below a spare token's, which [`RawLock::new`] requires.
        Self::new(Ghost(1))
    }
}

impl RawLock {
    /// A free lock of rank `rank` (ghost).
    pub const fn new(Ghost(rank): Ghost<int>) -> (l: RawLock)
        requires
            rank < rank_spare(),
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
    {
        self.lock();
        proof {
            ord.top = self.rank();
        }
        Tracked(LockHeld { lock: *self })
    }

    /// [`RawLock::release`] by the holder, which hands its token back.
    #[inline(always)]
    pub fn unlock_held(&self, Tracked(t): Tracked<LockHeld>)
        requires
            crate::sys::may_syscall(),
            t.held() == *self,
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
/// private to this module, so outside it only `lock_held` builds one. The
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

/// True, but only inside this module (the body is closed): the
/// requirement of the unchecked [`RawLock::lock`].
pub closed spec fn lock_rs_only() -> bool {
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
/// (the exceptions are `meta::Meta`'s methods but `Meta::stats`,
/// [`axiom_meta_order`], and `huge::Huge::usable_size`,
/// `heap::shared::Shared::check_size_hint` and `heap::shared::Shared::init`,
/// [`axiom_entry`]). The fields are private to
/// this module.
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
/// - `Shared::init`, which runs before the `Shared` is published, so no
///   allocator lock exists yet;
/// - the background thread's `main` (`purge::background`), since a new
///   thread holds no allocator lock;
/// - the report writers of `api::print` (under `malloc_stats_print`,
///   `malloc_info` and `malloc_stats`), one token per call to
///   `stats::collect`, `stats::collect_arena`, `stats::collect_all`,
///   `Shared::cgroup_now`, `ClassState::is_partial` and
///   `policy::empty_bytes`: the report's C caller holds no allocator lock,
///   the report takes none itself, and each of those calls releases every
///   lock it takes before it returns (none returns a guard), so none is
///   held at the next call.
///
/// No allocator entry point is re-entered while a lock is held: allocator
/// code calls nothing that may allocate with a lock held, and a signal
/// handler that allocates is outside the C contract (the fork handlers
/// block signals).
pub(crate) axiom fn axiom_entry<'a>() -> (tracked o: LockOrder<'a>)
    ensures
        o.top() == 0,
        !o.root(),
;

/// Premise: the calling thread holds only allocator locks ranked below
/// the class locks. Stands in for a token a closure cannot carry: its use
/// is `ClassState::decay_with`'s walk, which `Decayer::decay` calls with
/// no lock taken since `decay_with` was called with a token that allows
/// the class locks.
pub(crate) axiom fn axiom_below_class<'a>() -> (tracked o: LockOrder<'a>)
    ensures
        o.allows(rank_class(0, 0)),
        !o.root(),
;

/// Premise: the calling thread holds only allocator locks ranked below
/// the huge lock. Stands in for a token a closure cannot carry: its use
/// is `Huge::decay_with`'s walk, which `Decayer::decay` calls with no
/// lock taken since `decay_with` was called with a token that allows
/// the huge lock.
pub(crate) axiom fn axiom_below_huge<'a>() -> (tracked o: LockOrder<'a>)
    ensures
        o.allows(rank_huge()),
        !o.root(),
;

/// Premise: the calling thread holds only allocator locks ranked below the
/// metadata lock ([`rank_meta`]). Stands in for the token `meta::Meta`'s
/// methods would take from their callers: only `Meta::stats` takes one so
/// far, and the others would change about 20 signatures. Its uses are
/// the acquisitions in the other `Meta` methods.
pub axiom fn axiom_meta_order<'a>() -> (tracked o: LockOrder<'a>)
    ensures
        o.allows(rank_meta()),
        !o.root(),
;

// The futex calls; neither returns anything the callers use. Trusted
// until the `sys` futex wrappers they call have specifications.

#[verifier::external_body]
#[inline]
fn wait(state: &AtomicU32, val: u32)
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: `state` is a live, aligned `u32` for the duration of the call.
    // Every error (EAGAIN: already changed; EINTR: signal) means "re-check".
    let _ = unsafe { crate::sys::futex_wait(state.as_ptr(), val, None) };
}

#[verifier::external_body]
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

/// The invariant the value behind every `Lock<T>` keeps while unlocked.
///
/// Verified code that takes a lock relies on it: [`Lock::new`] requires it
/// of the initial value, and [`Lock::lock`] hands the value out with it.
/// Nothing checks that a guard is dropped with the invariant restored, so
/// this is an assumption about every holder of the lock: a verified holder
/// ends with the value satisfying it, through the postcondition of the
/// last call that changes the value or an explicit `assert` before the
/// guard goes out of scope, unless the owner of the type names a premise
/// that holder rests on instead; an unverified one is trusted. A verified
/// holder must not move a tracked permission out of the value and keep it
/// past the guard: the next `lock` would hand out the invariant's
/// permission again. `lock_inv` is uninterpreted; the owner of each locked
/// type states what it means for that type, and lists its unverified
/// holders and the verified ones that rest on a premise. The assumption enters at [`axiom_acquired`], and at
/// [`axiom_held`] for a holder of the raw lock.
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
/// entry is [`axiom_held`], for a holder of the raw lock): a thread that
/// has just acquired the raw lock of `l` takes the permission of `l`'s
/// cell out of the raw lock's invariant, and the value in the cell
/// satisfies [`lock_inv`] (every earlier holder restored it) and keeps
/// [`lock_frame`] of the value the lock was created with. Private, and
/// used only by [`Lock::lock`] and [`Lock::try_lock`] right after the
/// acquire, for the guard they build, which keeps the permission in a
/// field private to this module until its drop releases the lock: a call
/// while an earlier permission is alive would give two permissions to
/// one cell, and nothing but that discipline stops such a call.
axiom fn axiom_acquired<T>(l: Lock<T>) -> (tracked p: PointsTo<T>)
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
axiom fn axiom_held<'a, T>(l: Lock<T>, tracked held: &'a mut LockHeld) -> (tracked p: &'a mut PointsTo<T>)
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
#[verifier::external]
unsafe impl<T: Send> Send for Lock<T> {}

impl<T> Lock<T> {
    /// A lock of rank `rank` (ghost) guarding `value`.
    pub const fn new(value: T, Ghost(rank): Ghost<int>) -> (l: Lock<T>)
        requires
            lock_inv(value),
            rank < rank_spare(),
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
        requires
            crate::sys::may_syscall(),
            old(ord).allows(self.rank()),
        ensures
            lock_inv(g@),
            lock_frame(lock_init(*self), g@),
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
    pub fn lock_t(&self) -> LockGuard<'_, T> {
        self.lock(Tracked::assume_new(), Tracked::assume_new())
    }

    /// Acquire without blocking.
    #[inline]
    #[cfg(test)]
    pub fn try_lock(&self) -> (r: Option<LockGuard<'_, T>>)
        ensures
            r matches Some(g) ==> lock_inv(g@) && lock_frame(lock_init(*self), g@),
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

impl<T> Lock<T> {
    /// [`RawLock::lock_held`] for a holder that also keeps the value:
    /// acquire, and return the hold's token with the permission of the
    /// cell, which the holder keeps until the release (the fork handlers,
    /// for the arena creation lock).
    #[inline(always)]
    pub(crate) fn lock_held_perm(
        &self,
        _ord: Tracked<&mut LockOrder>,
    ) -> Tracked<(LockHeld, PointsTo<T>)> {
        self.raw.lock();
        Tracked::assume_new()
    }

    /// The value, read through the permission `perm` of the cell, which a
    /// holder keeps ([`Lock::lock_held_perm`]).
    #[inline(always)]
    pub(crate) fn value_held<'a>(&'a self, _perm: Tracked<&'a PointsTo<T>>) -> &'a T {
        self.value.borrow(Tracked::assume_new())
    }

    /// [`RawLock::unlock_held`] for a [`Lock::lock_held_perm`] holder:
    /// release, handing back the token and the permission of the cell.
    #[inline(always)]
    pub(crate) fn unlock_held_perm(&self, _t: Tracked<LockHeld>, _perm: Tracked<PointsTo<T>>) {
        self.raw.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

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
        assert_eq!(a, ["lock", "try_lock"]);
        assert_eq!(h, ["get_unlocked"]);
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
