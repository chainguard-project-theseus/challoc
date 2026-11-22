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

use crate::sync::patomic::{PAtomicU32, PermissionU32};
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use vstd::cell::CellId;
use vstd::cell::pcell::{PCell, PointsTo};
#[cfg(verus_keep_ghost)]
use vstd::invariant::OpenInvariantCredit;
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
use vstd::prelude::*;
use vstd::resource::Loc;
use vstd::resource::ghost_var::{GhostVar, GhostVarAuth};

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
///
/// The word is a vstd `PAtomicU32` (whose operations are `SeqCst`), and
/// its permission lives in the lock's atomic invariant ([`RawPred`]),
/// with the lock's [`Unlocked`] token exactly while the word is
/// `UNLOCKED`. An acquisition is the operation that moves the word off
/// `UNLOCKED`, so it takes the token out; the release's swap back to
/// `UNLOCKED` puts it back. A holder's [`LockHeld`] carries the token, so
/// a hold is one the word's history gives, not one a caller claims.
#[cfg_attr(not(verus_keep_ghost), repr(transparent))]
pub struct RawLock {
    state: PAtomicU32,
    /// Ghost: the lock's rank in the lock order ([`LockOrder`]), fixed at
    /// creation. Verus-only, as is dropping `repr(transparent)` there:
    /// `repr(transparent)` rejects a `Ghost` field (its fields are
    /// private), and ordinary builds keep the one-word struct as it is.
    #[cfg(verus_keep_ghost)]
    rank: Ghost<int>,
    /// Ghost: the word's invariant ([`RawPred`]), keyed by the word's
    /// identity and the location of the lock's token. Verus-only, as
    /// `rank` is.
    #[cfg(verus_keep_ghost)]
    inv: Tracked<RawInv>,
}

/// The lock's token, one per lock: a half of a vstd ghost variable at
/// the lock's [`RawLock::token_loc`]. The word's invariant holds it while
/// the word is `UNLOCKED` ([`RawPred`]); otherwise one holder has it (in
/// its [`LockHeld`] or guard). It is at `false` in the word's invariant
/// and for a bare hold; a [`Lock`] holder keeps it at `true` while it has
/// the cell's permission. The other half ([`GhostVarAuth`]) is in a
/// [`Lock`]'s cell invariant ([`CellVal`]), which [`Lock::acquired`] and
/// [`Lock::returned`] agree the token with ([`RawLock::new_paired`] makes
/// it); a bare [`RawLock`] drops it.
pub type Unlocked = GhostVar<bool>;

/// What a lock's word invariant is about: the word's identity and the
/// location of its [`Unlocked`] token.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub type RawKey = (int, Loc);

/// What a lock's word invariant holds: the word's permission and, while
/// the word is `UNLOCKED`, the lock's token.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub type RawVal = (PermissionU32, Option<Unlocked>);

/// The word invariant of a [`RawLock`]: the permission is the word's,
/// the word is one of the three states, and it is `UNLOCKED` exactly when
/// the invariant holds the lock's token.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub struct RawPred;

impl InvariantPredicate<RawKey, RawVal> for RawPred {
    closed spec fn inv(k: RawKey, v: RawVal) -> bool {
        &&& v.0.id() == k.0
        &&& v.0.value() <= CONTENDED
        &&& (v.0.value() == UNLOCKED) == (v.1 is Some)
        &&& v.1 matches Some(t) ==> t.id() == k.1 && t@ == false
    }
}

/// A lock's word invariant.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub type RawInv = AtomicInvariant<RawKey, RawVal, RawPred>;

/// The namespace of every lock's word invariant ("lock"). Only this
/// module opens one, and never one inside another.
pub open spec fn raw_ns() -> int {
    0x6c6f_636b
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
        let (l, _auth) = Self::new_paired(Ghost(rank));
        l
    }

    /// [`RawLock::new`], with the other half of the lock's token: the
    /// [`GhostVarAuth`] at the token's location, at `false`, which a
    /// [`Lock`]'s cell invariant keeps ([`CellPred`]). A bare `RawLock`
    /// drops it.
    pub const fn new_paired(Ghost(rank): Ghost<int>) -> (r: (RawLock, Tracked<GhostVarAuth<bool>>))
        requires
            rank <= rank_state(),
        ensures
            r.0.rank() == rank,
            r.1@.id() == r.0.token_loc(),
            r.1@@ == false,
    {
        // The word with its one permission, and the lock's token, both
        // into the word's invariant, at `UNLOCKED`.
        let (state, Tracked(perm)) = PAtomicU32::new(UNLOCKED);
        let tracked (auth, tok) = GhostVarAuth::<bool>::new(false);
        let tracked inv = AtomicInvariant::<RawKey, RawVal, RawPred>::new(
            (state.id(), tok.id()),
            (perm, Some(tok)),
            raw_ns(),
        );
        (
            RawLock {
                state,
                #[cfg(verus_keep_ghost)]
                rank: Ghost(rank),
                #[cfg(verus_keep_ghost)]
                inv: Tracked(inv),
            },
            Tracked(auth),
        )
    }

    /// The word's invariant is about this lock's word, in [`raw_ns`].
    #[verifier::type_invariant]
    closed spec fn wf(&self) -> bool {
        &&& self.inv@.constant().0 == self.state.id()
        &&& self.inv@.namespace() == raw_ns()
    }

    /// The lock's rank in the lock order.
    pub closed spec fn rank(self) -> int {
        self.rank@
    }

    /// The location of the lock's [`Unlocked`] token.
    pub closed spec fn token_loc(self) -> Loc {
        self.inv@.constant().1
    }

    /// `t` is this lock's token.
    pub open spec fn is_token(self, t: Unlocked) -> bool {
        t.id() == self.token_loc() && t@ == false
    }

    /// The bare CAS from `UNLOCKED` to `LOCKED`; on success the lock's
    /// token, which the word's invariant held while the word was
    /// `UNLOCKED`.
    #[inline]
    fn try_acquire(&self) -> (r: (bool, Tracked<Option<Unlocked>>))
        ensures
            r.0 == r.1@ is Some,
            r.1@ matches Some(t) ==> self.is_token(t),
    {
        proof {
            use_type_invariant(self);
        }
        let res;
        let tracked mut out: Option<Unlocked> = None;
        vstd::open_atomic_invariant!(self.inv.borrow() => v => {
            let tracked (mut perm, tok) = v;
            res = self.state.compare_exchange(Tracked(&mut perm), UNLOCKED, LOCKED);
            proof {
                if res is Ok {
                    out = tok;
                    v = (perm, None);
                } else {
                    v = (perm, tok);
                }
            }
        });
        (res.is_ok(), Tracked(out))
    }

    /// A load of the word.
    #[inline(always)]
    fn load(&self) -> u32 {
        proof {
            use_type_invariant(self);
        }
        let r;
        vstd::open_atomic_invariant!(self.inv.borrow() => v => {
            let tracked (perm, tok) = v;
            r = self.state.load(Tracked(&perm));
            proof {
                v = (perm, tok);
            }
        });
        r
    }

    /// `swap(CONTENDED)`: the slow path's acquire, which acquires when it
    /// returns `UNLOCKED`, and then hands out the lock's token.
    #[inline(always)]
    fn swap_contended(&self) -> (r: (u32, Tracked<Option<Unlocked>>))
        ensures
            (r.0 == UNLOCKED) == r.1@ is Some,
            r.1@ matches Some(t) ==> self.is_token(t),
    {
        proof {
            use_type_invariant(self);
        }
        let c;
        let tracked mut out: Option<Unlocked> = None;
        vstd::open_atomic_invariant!(self.inv.borrow() => v => {
            let tracked (mut perm, tok) = v;
            c = self.state.swap(Tracked(&mut perm), CONTENDED);
            proof {
                out = tok;
                v = (perm, None);
            }
        });
        (c, Tracked(out))
    }

    /// Acquire without blocking. `true` on success.
    #[inline]
    #[cfg(test)]
    pub fn try_lock(&self) -> (r: (bool, Tracked<Option<Unlocked>>))
        ensures
            r.0 == r.1@ is Some,
            r.1@ matches Some(t) ==> self.is_token(t),
    {
        self.try_acquire()
    }

    /// [`RawLock::release`] of the token a successful [`RawLock::try_lock`]
    /// returned, for the tests.
    #[inline]
    #[cfg(test)]
    pub fn release_some(&self, Tracked(t): Tracked<Option<Unlocked>>)
        requires
            crate::sys::may_syscall(),
            t matches Some(x) && self.is_token(x),
    {
        self.release(Tracked(t.tracked_unwrap()));
    }

    /// Acquire, blocking as needed, and return the lock's token
    /// ([`Unlocked`]). Opens any invariant (stated, though it
    /// is Verus's default): a function that declares a narrower mask
    /// cannot call it. Verified callers outside this module cannot meet
    /// the `lock_rs_only` requirement (its body is closed): they acquire
    /// through [`Lock::lock`] or [`RawLock::lock_held`], which check the
    /// lock order.
    #[inline]
    pub fn lock(&self) -> (t: Tracked<Unlocked>)
        requires
            crate::sys::may_syscall(),
            lock_rs_only(),
        ensures
            self.is_token(t@),
        opens_invariants any
    {
        let Tracked(t) = self.acquire();
        let tracked tok;
        proof {
            if t is Some {
                tok = t.tracked_unwrap();
            } else {
                // The fork window served it ([`RawLock::acquire`]).
                tok = axiom_window_served(*self);
            }
        }
        Tracked(tok)
    }

    /// Acquire, blocking as needed: the lock's token, or `None` for an
    /// acquisition the fork window served ([`RawLock::lock_slow`]), which
    /// takes no token from the word.
    #[inline]
    fn acquire(&self) -> (t: Tracked<Option<Unlocked>>)
        requires
            crate::sys::may_syscall(),
        ensures
            t@ matches Some(x) ==> self.is_token(x),
    {
        let (ok, t) = self.try_acquire();
        if !ok {
            return self.lock_slow();
        }
        t
    }

    /// Termination is not claimed: the wait ends when another thread
    /// releases the lock.
    #[cold]
    #[verifier::exec_allows_no_decreases_clause]
    fn lock_slow(&self) -> (t: Tracked<Option<Unlocked>>)
        requires
            crate::sys::may_syscall(),
        ensures
            t@ matches Some(x) ==> self.is_token(x),
    {
        // The forking thread inside the fork window already holds it
        // ([`fork_holder_is_me`]): served as acquired, released as a no-op.
        if fork_holder_is_me() {
            return Tracked(None);
        }
        for _ in 0..SPINS {
            if self.load() == UNLOCKED {
                let (ok, t) = self.try_acquire();
                if ok {
                    return t;
                }
            }
            core::hint::spin_loop();
        }
        // Announce a possible waiter, then sleep while the word stays
        // CONTENDED. A spurious wake or `EINTR` just loops: `swap` re-checks.
        let (mut c, Tracked(t)) = self.swap_contended();
        let tracked mut tok = t;
        while c != UNLOCKED
            invariant
                crate::sys::may_syscall(),
                (c == UNLOCKED) == tok is Some,
                tok matches Some(t) ==> self.is_token(t),
        {
            wait(&self.state, CONTENDED);
            let (c2, Tracked(t2)) = self.swap_contended();
            c = c2;
            proof {
                tok = t2;
            }
        }
        Tracked(tok)
    }

    /// Release, handing the lock's token `t` back to the word's invariant.
    /// Must be called by the holder exactly once per acquire.
    /// Private: every release outside this file goes through
    /// [`RawLock::unlock_held`], which takes the hold's token back, or a
    /// guard's drop. A public release would let a caller release and
    /// re-take the lock with the token of a [`LockHeld`] it keeps,
    /// holding two [`LockHeld`] tokens for one lock and, through
    /// [`Lock::lock_held_perm`], two permissions of its cell.
    #[inline]
    fn release(&self, Tracked(t): Tracked<Unlocked>)
        requires
            crate::sys::may_syscall(),
            self.is_token(t),
    {
        // An acquire the fork window served ([`RawLock::lock_slow`]): the
        // lock stays with the fork handlers, and the token goes.
        if fork_holder_is_me() {
            return;
        }
        self.release_word(Tracked(t));
    }

    /// [`RawLock::release`] past its fork-window test: the swap to
    /// `UNLOCKED` that hands the token back, and the wake.
    #[inline(always)]
    fn release_word(&self, Tracked(t): Tracked<Unlocked>)
        requires
            crate::sys::may_syscall(),
            self.is_token(t),
    {
        proof {
            use_type_invariant(self);
        }
        let prev;
        vstd::open_atomic_invariant!(self.inv.borrow() => v => {
            let tracked (mut perm, _held) = v;
            prev = self.state.swap(Tracked(&mut perm), UNLOCKED);
            proof {
                v = (perm, Some(t));
            }
        });
        if prev == CONTENDED {
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
            t@.raw(),
            final(ord).root(),
            final(ord).top() == self.rank(),
            final(ord).ranks() == old(ord).ranks().insert(self.rank()),
    {
        let Tracked(tok) = self.lock();
        proof {
            ord.top = self.rank();
            ord.ranks = ord.ranks.insert(self.rank());
        }
        Tracked(LockHeld { lock: *self, tok })
    }

    /// [`RawLock::release`] by the holder, which hands its token back,
    /// once the fork window has closed ([`window_closed`]); its rank leaves
    /// the root `ord`'s raw holds.
    #[inline(always)]
    pub fn unlock_held(&self, Tracked(t): Tracked<LockHeld>, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_syscall(),
            t.held() == *self,
            t.raw(),
            window_closed(),
            old(ord).root(),
        ensures
            final(ord).root(),
            final(ord).top() == old(ord).top(),
            final(ord).ranks() == old(ord).ranks().remove(self.rank()),
    {
        proof {
            ord.ranks = ord.ranks.remove(self.rank());
        }
        self.release(Tracked(t.into_token()));
    }

    /// Whether some thread currently holds the lock. Diagnostic only.
    #[inline]
    #[cfg(test)]
    pub fn is_locked(&self) -> bool {
        self.load() != UNLOCKED
    }
}

/// Premise (the fork window, design 9): an acquisition of `l` that the
/// fork window served ([`RawLock::acquire`] returned no token: the path
/// of [`RawLock::lock_slow`] where [`fork_holder_is_me`] returned true)
/// holds `l`'s token. The prepare
/// handler took every allocator lock before it opened the window
/// ([`fork_prepared`]), so its [`LockHeld`] has the real token, and the
/// one this premise gives is a second one for the same hold: it stands for
/// that hold, used by the one thread that runs allocator code there until
/// the release, which, in the window, does not touch the word
/// ([`RawLock::release`] drops the token). Every lock the window serves is
/// one the prepare handler took: no arena (and so no lock) is built there
/// (`Arenas::create`, `Arenas::materialise`). Its one use is
/// [`RawLock::lock`]'s, for a bare raw lock; a [`Lock`]'s acquisitions
/// take [`axiom_window_cell`] instead.
///
/// Checked by `lock::tests::permission_axioms_are_called_only_at_acquisitions`.
axiom fn axiom_window_served(l: RawLock) -> (tracked t: Unlocked)
    ensures
        l.is_token(t),
;

/// Premise (the fork window, design 9), for a [`Lock`]: an acquisition of
/// `l` that the fork window served ([`RawLock::acquire`] returned no
/// token) holds `l`'s token, as a holder of the cell's permission keeps
/// it (at `true`, [`CellPred`]), and the permission of `l`'s cell, whose
/// value satisfies [`lock_inv`] and keeps [`lock_frame`] of the value the
/// lock was created with. As for [`axiom_window_served`], the prepare
/// handler holds the lock (and, for the arena creation lock and the state
/// lock, the permission: `api::ForkHeld`, `purge::background::StateHold`),
/// and the token and permission this premise gives stand for that hold,
/// used by the one thread that runs allocator code there, on the window's
/// premise that the lock's data is as its last holder left it. The
/// release in the window ([`Lock::release_cell`]) drops both without
/// touching the cell's invariant or the word. Its one use is
/// [`Lock::acquired`].
///
/// For a lock the prepare handler holds bare ([`RawLock::lock_held`] on
/// the `Lock`'s word: every fork lock but the arena creation lock and the
/// state lock), the pair stands beside the cell's invariant, not beside a
/// hold: the invariant still has the permission and the token's other
/// half at `false`, so the pair given here (the token at `true`, a second
/// `PointsTo`) contradicts the invariant's. That is the intended premise:
/// the window path opens no cell invariant of a lock it was served, so
/// nothing joins the two. [`Lock::acquired`] takes this branch instead of
/// opening it, and [`Lock::release_cell`] returns on its own
/// [`fork_holder_is_me`] test before [`Lock::returned`] opens it. Verus
/// does not tie that release test to this acquisition: what pins it is
/// the release's text (the test named below checks that the window
/// return is the first statement of `release_cell` and of
/// [`RawLock::release`]), and the window's premise that its holder stays
/// the forking thread from the acquisition to the release (the window
/// closes only in `fork_release`, after every handler's allocator code
/// has returned).
///
/// Checked by `lock::tests::permission_axioms_are_called_only_at_acquisitions`.
axiom fn axiom_window_cell<T: LockSpec>(l: Lock<T>) -> (tracked r: (Unlocked, PointsTo<T>))
    ensures
        r.0.id() == l.spec_raw().token_loc(),
        r.0@ == true,
        l.has_perm(r.1),
        lock_inv(*r.1.value()),
        lock_frame(lock_init(l), *r.1.value()),
;

/// Ghost: its owner holds the raw lock [`LockHeld::held`], and has its
/// token ([`Unlocked`]), which the word's invariant gave up when the
/// acquisition moved the word off `UNLOCKED`.
/// [`RawLock::lock_held`] makes one after its `lock` returns, and
/// [`RawLock::unlock_held`] takes it back as it unlocks. The field is
/// private to this module, so outside it only `lock_held` and
/// [`Lock::lock_held_perm`] build one; the fork handlers' handoff axioms
/// (`api::axiom_fork_resumed_state`, `api::resume::axiom_fork_resumed`)
/// assume theirs. The token is not `Copy`, so each token is handed back
/// at most once. A token names its lock by value (Verus has no addresses of
/// references): a proof matches it to the accessor path it was taken
/// through, not to an address. That the token's lock is held by this
/// thread alone follows from the word's invariant: the token is in the
/// invariant exactly while the word is `UNLOCKED`, so outside the fork
/// window ([`axiom_window_served`]) only the acquisition that moved the
/// word off `UNLOCKED` took it. A hold of a [`Lock`] that also has the
/// cell's permission ([`Lock::lock_held_perm`]) keeps the token at `true`
/// ([`CellPred`]), so only [`Lock::unlock_held_perm`] releases it; a bare
/// hold ([`RawLock::lock_held`]) keeps it at `false` ([`LockHeld::raw`]),
/// as [`RawLock::unlock_held`] requires.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct LockHeld {
    ghost lock: RawLock,
    tracked tok: Unlocked,
}

impl LockHeld {
    /// The token is the lock's.
    #[verifier::type_invariant]
    closed spec fn wf(self) -> bool {
        self.tok.id() == self.lock.token_loc()
    }

    /// The lock this token holds.
    pub closed spec fn held(self) -> RawLock {
        self.lock
    }

    /// A bare hold, with no permission of a cell taken under it: its
    /// token is as the word's invariant keeps it ([`RawLock::is_token`]).
    pub closed spec fn raw(self) -> bool {
        self.tok@ == false
    }

    /// The lock's token, for its release.
    proof fn into_token(tracked self) -> (tracked t: Unlocked)
        ensures
            t.id() == self.held().token_loc(),
            self.raw() ==> self.held().is_token(t),
    {
        use_type_invariant(&self);
        self.tok
    }
}

/// True, but only inside this module (the body is closed): the
/// requirement of the unchecked [`RawLock::lock`].
pub closed spec fn lock_rs_only() -> bool {
    true
}

/// The root token `ord` has taken the lock of [`rank_state`] (the
/// background thread's state lock, which `api::fork_prepare` takes last;
/// the test `purge::tests::state_lock_is_the_one_lock_of_its_type`
/// checks it is the one `Lock<State>`): the requirement of
/// [`fork_window_open`]. The body is closed, so outside this module only
/// the postcondition of [`Lock::lock_held_perm`] of a lock at
/// `rank_state` gives it; a root's top is raised only by that and
/// [`RawLock::lock_held`], to the rank they take. After that
/// acquisition the root's top is `rank_state`, which no lock ranks above
/// ([`RawLock::new`]), so no acquisition follows it on that token, and
/// the window opens only once every lock is taken: an acquire after the
/// window opened would be served as acquired ([`fork_holder_is_me`])
/// without the lock.
pub closed spec fn fork_prepared(ord: LockOrder) -> bool {
    ord.root() && ord.top() == rank_state()
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
/// [`axiom_c_entry`] in an export body, or from [`axiom_init_entry`],
/// [`axiom_thread_entry`] or [`axiom_fork_entry`] (each lists its uses),
/// and, for the fork release, from `api::resume::axiom_fork_resumed`).
/// The fields are private to this module.
///
/// The token [`Lock::lock`] hands out has the lifetime `'a` of the
/// guard's borrow, which also borrows the caller's token: the caller's
/// lower bound is unusable until the guard and every use of the token
/// handed out are gone. A handed-out token used after its guard is
/// dropped bounds a lock no longer held, which only forbids more.
///
/// A root also records the ranks of the raw holds its owner keeps
/// ([`LockOrder::ranks`]): [`RawLock::lock_held`] adds one,
/// [`RawLock::unlock_held`] takes it out, and a root whose set is empty
/// becomes a token at top 0 ([`LockOrder::unroot`]).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct LockOrder<'a> {
    ghost top: int,
    ghost root: bool,
    ghost ranks: Set<int>,
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

    /// For a root, the ranks of the raw holds (`LockHeld`) its owner
    /// keeps: no lock of another rank is held by the owner, since a root
    /// starts with none ([`axiom_fork_entry`]) or with the ranks of the
    /// hold it comes with (`api::resume::axiom_fork_resumed`, the fork
    /// release's root), and its guards end before it is used again. A
    /// lock's rank names it among the locks a thread can hold at once (a
    /// raw acquisition ranks above every lock its thread holds).
    pub closed spec fn ranks(self) -> Set<int> {
        self.ranks
    }

    /// A root with no raw hold left: its owner holds no allocator lock,
    /// so a token at top 0 (not a root) stands for it. The fork release
    /// ends with one (`api::fork_release`).
    pub proof fn unroot(tracked self) -> (tracked o: LockOrder<'a>)
        requires
            self.root(),
            self.ranks() == Set::<int>::empty(),
        ensures
            o.top() == 0,
            !o.root(),
    {
        LockOrder { top: 0, root: false, ranks: Set::empty(), life: PhantomData }
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
        LockOrder { top: rank_spare(), root: false, ranks: Set::empty(), life: PhantomData }
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
        LockOrder { top: self.top, root: false, ranks: Set::empty(), life: PhantomData }
    }
}

/// Premise (C-program contract, the lock order at a C call): when C calls
/// an allocator entry point, the calling thread holds no allocator lock,
/// or, in the fork window only, holds every one as the fork's own hold
/// (below). The token stands for that call, and the export passes it down
/// (`Tracked<&mut LockOrder>`) to every function on its path that may
/// take a lock, so that no lock is held where one is taken is checked,
/// not assumed, below the export. A signal handler that interrupted
/// allocator code and calls an entry point is outside the C contract (the
/// fork handlers block signals). An allocation from a `pthread_atfork`
/// handler registered before challoc's runs in the fork window with every
/// lock held by its own thread; its locks are taken as acquired
/// ([`fork_holder_is_me`]), so it waits on none of them. Allocator code
/// calls nothing that may re-enter an entry point while it holds a lock
/// (theorem 16 checks the reports' calls out; elsewhere by inspection).
///
/// Minted only in an export body (an `extern "C" fn` that is a C call
/// into challoc, an arm of `verified_export_pair!`, or a helper one arm
/// calls), at most once each: checked by
/// `api::tests::c_call_tokens_only_in_export_bodies`.
pub(crate) axiom fn axiom_c_entry<'a>() -> (tracked o: LockOrder<'a>)
    ensures
        o.top() == 0,
        !o.root(),
;

/// Premise (allocator invariant, the lock order at init): `Shared::init`,
/// its one use, holds no allocator lock. Every allocator lock is one of
/// the `Shared` that `init` builds or a static that only code reached
/// through that `Shared` takes (the background thread's state lock), and
/// `api::init` runs `init` once, before it publishes the `Shared`; so no
/// allocator lock can be held yet. The C call that reaches `init` (its
/// first use of the allocator) holds a token of its own
/// ([`axiom_c_entry`]), which `api::shared` does not take.
///
/// Checked by `lock::tests::root_lock_order_uses_are_listed`, which pins
/// its one call site.
pub(crate) axiom fn axiom_init_entry<'a>() -> (tracked o: LockOrder<'a>)
    ensures
        o.top() == 0,
        !o.root(),
;

/// Premise (platform fact, the lock order at a thread's start or exit):
/// code that the C library runs on a thread outside any call into
/// challoc holds no allocator lock. Its uses:
/// - the background thread's `main` (`purge::background`), its thread's
///   start routine, which `pthread_create` runs on a new thread;
/// - `heap::thread::heap_destructor`, the thread-exit key's destructor,
///   which glibc runs as the thread exits (after its start routine
///   returns, or from `pthread_exit`). challoc calls neither
///   `pthread_exit` nor a cancellation point while it holds an
///   allocator lock, so no exit starts inside a critical section of
///   challoc's.
///
/// Checked by `lock::tests::root_lock_order_uses_are_listed`, which pins
/// the functions that call it to the uses listed above.
pub(crate) axiom fn axiom_thread_entry<'a>() -> (tracked o: LockOrder<'a>)
    ensures
        o.top() == 0,
        !o.root(),
;

/// Premise (the fork prepare handler's entry): the calling thread holds no
/// allocator lock, as for [`axiom_c_entry`], and keeps this token for the
/// whole handler, which takes every lock with [`RawLock::lock_held`] and
/// releases none of them before it returns: a root, with no raw hold yet.
/// Its one use is `api::fork_prepare` (the fork handlers block signals
/// before their first lock, so nothing re-enters the allocator under it),
/// which `api::tests::fork_hold_is_resumed_once` pins, counting every
/// call in every file under `src/`.
///
/// A `fork` from a signal handler that interrupted this thread inside
/// the allocator, with a lock held, breaks the premise: `fork_prepare`
/// would then wait forever on that lock. `fork` runs the atfork handlers
/// and is not async-signal-safe in glibc or in POSIX.1-2024, which list
/// `_Fork` instead, so that call is outside the C contract, as the
/// allocating signal handler of [`axiom_c_entry`] is.
pub(crate) axiom fn axiom_fork_entry<'a>() -> (tracked o: LockOrder<'a>)
    ensures
        o.top() == 0,
        o.root(),
        o.ranks() == Set::<int>::empty(),
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
/// handler's last step, with every lock taken on its root token `ord`
/// ([`fork_prepared`]).
pub(crate) fn fork_window_open(Tracked(ord): Tracked<&LockOrder>)
    requires
        crate::sys::may_syscall(),
        fork_prepared(*ord),
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
/// child handler would, and do it themselves. The capability
/// [`crate::sys::cgroup::in_fork_child`] stands for exactly this state
/// (the child's single thread, before any other runs), which the window's
/// record read back shows ([`axiom_child_window`]).
pub(crate) fn in_child_fork_window() -> (r: bool)
    requires
        crate::sys::may_syscall(),
    ensures
        r ==> crate::sys::cgroup::in_fork_child(),
{
    let h = FORK_HOLDER.load(Ordering::Relaxed);
    if h == 0 {
        return false;
    }
    let me = crate::sys::pthread_self();
    if h != me {
        return false;
    }
    let pid = FORK_PID.load(Ordering::Relaxed);
    let now = crate::sys::getpid() as u32;
    proof {
        if pid != now {
            axiom_child_window(h, me, pid, now);
        }
    }
    pid != now
}

/// Premise (`fork(2)`, `getpid(2)`, glibc's `pthread_self`; design 9):
/// the fork window's record, read back by the thread that opened it,
/// shows that thread in the child of its `fork`. `h` and `pid` are what
/// the caller read from [`FORK_HOLDER`] and then [`FORK_PID`], `me` its
/// `pthread_self` and `now` its `getpid` (as a `u32`), read after them;
/// the one use is [`in_child_fork_window`]. The contract does not
/// enforce that the arguments are those reads: it requires facts of
/// values the caller picks. The one call site does, and
/// `lock::tests::fork_record_uses_are_listed` pins it and its reads.
///
/// The record's writers are [`fork_window_open`], which stores the
/// opener's `getpid` and then its `pthread_self`, and
/// [`fork_window_close`], which stores 0
/// (`lock::tests::fork_record_uses_are_listed` pins them, this use and
/// its reads). So `h == me` (not 0) says the caller opened the window
/// and has not closed it, and the `pid` it then read is the one it stored
/// (both stores are its own, so its relaxed loads see them). The platform
/// facts: `pthread_self` tells the live threads of a process apart, and
/// in glibc the forking thread keeps its value in the child of a `fork`
/// (the thread descriptor is copied with the memory); the child starts
/// with a copy of the parent's memory, these atomics included; and in
/// one PID namespace the child's `getpid` is not the parent's. So
/// `pid != now` says the caller is the forking thread in a child of the
/// fork whose window it opened, before the child handler closed the
/// window. A child in a new PID namespace may read its parent's
/// `getpid` (both 1): then the caller gets `false`, which claims nothing.
///
/// Not covered (design 9, residuals): a process `_Fork`ed, `vfork`ed or
/// cloned inside the window, which runs no child handler, inherits the
/// record as its own, and so passes for the child of the fork (a `vfork`
/// child shares the memory of the parent's live threads).
axiom fn axiom_child_window(h: usize, me: usize, pid: u32, now: u32)
    requires
        h != 0,
        h == me,
        pid != now,
    ensures
        crate::sys::cgroup::in_fork_child(),
;

/// End the fork window; the parent and child handlers' step before their
/// release, which it allows ([`window_closed`]).
pub(crate) fn fork_window_close()
    ensures
        window_closed(),
{
    FORK_HOLDER.store(0, Ordering::Relaxed);
}

// The futex calls; neither returns anything the callers use. The futex
// word is the `u32` of the `PAtomicU32`, at its address: vstd's
// `PAtomicU32` is one `AtomicU32` field, and the assertion after this
// block checks it is a `u32`'s size and alignment, so the field is at
// offset 0.

#[inline]
fn wait(state: &PAtomicU32, val: u32)
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: `state` is a live, aligned `u32` for the duration of the call.
    // Every error (EAGAIN: already changed; EINTR: signal) means "re-check".
    let _ = unsafe { crate::sys::futex_wait(core::ptr::from_ref(state) as *const u32, val, None) };
}

#[inline]
fn wake(state: &PAtomicU32)
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: `state` is a live, aligned `u32`.
    let _ = unsafe { crate::sys::futex_wake(core::ptr::from_ref(state) as *const u32, 1) };
}

/// A [`RawLock`] guarding a value.
///
/// The value is a vstd [`PCell`]: reading or writing it takes the cell's
/// [`PointsTo`] permission (ghost). The lock's cell invariant
/// ([`CellPred`]) holds that permission while no holder has it, with the
/// other half of the raw lock's token ([`RawLock::new_paired`]):
/// [`Lock::new`] hands both over. An acquisition takes the raw lock's
/// token out of the word's invariant ([`RawPred`]), agrees it with the
/// half here (both `false`: the permission is in), takes the permission
/// and sets both halves to `true` ([`Lock::acquired`]). The release
/// ([`Lock::release_cell`], from the guard's drop or
/// [`Lock::unlock_held_perm`]) puts the permission back, with the value's
/// [`lock_inv`] and [`lock_frame`], sets both halves to `false` and hands
/// the token back to the word. So outside the fork window
/// ([`axiom_window_cell`]) a holder's permission is the one the word's
/// history gave. A `PCell` is a
/// `ManuallyDrop<UnsafeCell<T>>`: the
/// layout and the code of the `UnsafeCell` it replaces, except that
/// dropping a `Lock` does not drop the value (no lock is dropped outside
/// the tests). The cell invariant is a ghost field (zero-sized).
#[verifier::reject_recursive_types(T)]
pub struct Lock<T> {
    raw: RawLock,
    value: PCell<T>,
    /// Ghost: the value the lock was created with ([`lock_init`]).
    #[allow(dead_code)]
    init: Ghost<T>,
    /// Ghost: the cell's invariant ([`CellPred`]).
    #[allow(dead_code)]
    cell: Tracked<CellInv<T>>,
}

/// What a lock's cell invariant is about: the cell's identity, the
/// location of the raw lock's token, and the value the lock was created
/// with ([`lock_init`]).
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub type CellKey<T> = (CellId, Loc, T);

/// What a lock's cell invariant holds: the other half of the raw lock's
/// token and, while no holder has it, the cell's permission.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub type CellVal<T> = (GhostVarAuth<bool>, Option<PointsTo<T>>);

/// The cell invariant of a [`Lock`]: the half is the token's, it is
/// `false` exactly when the permission is in, and the permission is the
/// cell's, with a value that satisfies [`lock_inv`] and keeps
/// [`lock_frame`] of the value the lock was created with.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub struct CellPred;

impl<T: LockSpec> InvariantPredicate<CellKey<T>, CellVal<T>> for CellPred {
    closed spec fn inv(k: CellKey<T>, v: CellVal<T>) -> bool {
        &&& v.0.id() == k.1
        &&& (v.0@ == false) == (v.1 is Some)
        &&& v.1 matches Some(p) ==> {
            &&& p.id() == k.0
            &&& lock_inv(*p.value())
            &&& lock_frame(k.2, *p.value())
        }
    }
}

/// A lock's cell invariant.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub type CellInv<T> = AtomicInvariant<CellKey<T>, CellVal<T>, CellPred>;

/// The namespace of every lock's cell invariant ("cell"). Only this
/// module opens one, never inside another, and never inside a word's
/// invariant ([`raw_ns`]).
pub open spec fn cell_ns() -> int {
    0x6365_6c6c
}

/// The invariant and frame of a locked type, stated by the type's owner:
/// [`lock_inv`] is `lock_spec_inv`, [`lock_frame`] is `lock_spec_frame`.
/// An owner may define them in its impl only when every holder of its
/// lock outside the tests ends with [`LockGuard::unlock`], which checks
/// both at each release, and `api::tests::UNLOCKED_LOCKS` lists the lock
/// with those holders (`api::tests::guards_end_in_unlock`;
/// `api::tests::defined_lock_specs_are_unlocked_locks` checks that it
/// lists every lock of a type whose impl defines either, a frame of
/// `true` aside). Otherwise
/// the impl keeps them uninterpreted ([`assumed_inv`], [`assumed_frame`])
/// and the owner states them by an axiom that names the holders. A frame
/// of `true` may be defined either way: no release can break it.
/// [`Lock::new`] requires the frame of the initial value with itself,
/// which the cell's invariant ([`CellPred`]) holds until the first
/// acquisition. The two method
/// names are defined nowhere else in the crate (several locked types have
/// an inherent `inv`), so
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
/// assumption enters at a dropped guard's release (`LockGuard::drop`,
/// trusted, calls [`Lock::release_cell`], which requires it, without the
/// check), and in the fork window at [`axiom_window_cell`].
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

// The futex word is the `PAtomicU32` itself (`wait`, `wake`): a `u32`'s
// size and alignment, so its one field is at offset 0.
const _: () =
    assert!(core::mem::size_of::<PAtomicU32>() == 4 && core::mem::align_of::<PAtomicU32>() == 4);

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
    /// The value's address inside the lock at `p`, which the permission
    /// `perm` names, for building a large value in place
    /// (`Arena::put_stats`). Trusted: the address of the cell, whose value
    /// is its only data (`kani_lock` checks it for locks at four `T`s).
    #[verifier::external_body]
    #[inline(always)]
    pub(crate) fn value_ptr(p: *mut Lock<T>, Tracked(perm): Tracked<&vstd::raw_ptr::PointsTo<Lock<T>>>) -> *mut T
        requires
            perm.ptr() == p,
    {
        // SAFETY: a field projection inside the lock `perm` names; nothing
        // is read or written.
        unsafe { core::ptr::addr_of_mut!((*p).value) as *mut T }
    }

    /// A lock of rank `rank` (ghost) guarding `value`, at most the state
    /// lock's ([`RawLock::new`]). `value` satisfies [`lock_inv`] and keeps
    /// [`lock_frame`] of itself, which the cell invariant ([`CellPred`])
    /// holds of it until the first acquisition.
    pub const fn new(value: T, Ghost(rank): Ghost<int>) -> (l: Lock<T>)
        where T: LockSpec
        requires
            lock_inv(value),
            lock_frame(value, value),
            rank <= rank_state(),
        ensures
            lock_init(l) == value,
            l.rank() == rank,
    {
        // The permission and the token's other half go to the cell's
        // invariant.
        let (cell, Tracked(perm)) = PCell::new(value);
        let (raw, Tracked(auth)) = RawLock::new_paired(Ghost(rank));
        let tracked inv = AtomicInvariant::<CellKey<T>, CellVal<T>, CellPred>::new(
            (cell.id(), raw.token_loc(), value),
            (auth, Some(perm)),
            cell_ns(),
        );
        Lock { raw, value: cell, init: Ghost(value), cell: Tracked(inv) }
    }

    /// The cell's invariant is about this lock's cell, its raw lock's
    /// token and its initial value, in [`cell_ns`].
    #[verifier::type_invariant]
    closed spec fn wf(&self) -> bool {
        &&& self.cell@.constant() == (self.value.id(), self.raw.token_loc(), self.init@)
        &&& self.cell@.namespace() == cell_ns()
    }

    /// The acquisition's half of the cell's hand-over: from the token `t`
    /// [`RawLock::acquire`] returned, the token at `true` and the cell's
    /// permission, which the cell's invariant gave up (the token at
    /// `false` agrees with the invariant's half, so the permission was
    /// in); for an acquisition the fork window served (`None`),
    /// [`axiom_window_cell`]'s.
    proof fn acquired(tracked &self, tracked t: Option<Unlocked>, tracked credit: OpenInvariantCredit) -> (tracked r: (Unlocked, PointsTo<T>))
        where T: LockSpec
        requires
            t matches Some(x) ==> self.raw.is_token(x),
        ensures
            r.0.id() == self.raw.token_loc(),
            r.0@ == true,
            self.has_perm(r.1),
            lock_inv(*r.1.value()),
            lock_frame(lock_init(*self), *r.1.value()),
        opens_invariants any
    {
        use_type_invariant(self);
        if t is Some {
            let tracked mut tok = t.tracked_unwrap();
            let tracked mut out: Option<PointsTo<T>> = None;
            vstd::open_atomic_invariant_in_proof!(credit => self.cell.borrow() => v => {
                let tracked (mut auth, p) = v;
                auth.agree(&tok);
                auth.update(&mut tok, true);
                out = p;
                v = (auth, None);
            });
            (tok, out.tracked_unwrap())
        } else {
            axiom_window_cell(*self)
        }
    }

    /// The release's half: the permission `perm` back into the cell's
    /// invariant, with its value's [`lock_inv`] and [`lock_frame`], and
    /// the token `t` (the hold's) back at `false`, for the word
    /// ([`RawLock::release_word`]). The token is at `true` (a holder of
    /// the permission's): at `false` the invariant would hold a second
    /// permission of the cell, which this one would replace.
    proof fn returned(tracked &self, tracked t: Unlocked, tracked perm: PointsTo<T>, tracked credit: OpenInvariantCredit) -> (tracked r: Unlocked)
        where T: LockSpec
        requires
            t.id() == self.raw.token_loc(),
            self.has_perm(perm),
            lock_inv(*perm.value()),
            lock_frame(lock_init(*self), *perm.value()),
        ensures
            self.raw.is_token(r),
        opens_invariants any
    {
        use_type_invariant(self);
        let tracked mut tok = t;
        let tracked mut perm = perm;
        vstd::open_atomic_invariant_in_proof!(credit => self.cell.borrow() => v => {
            let tracked (mut auth, p) = v;
            // `p` is `None`: a second permission of the cell would prove
            // `false` (vstd's `PointsTo::is_exclusive`); the proof does
            // not need it, since the invariant takes `perm` either way.
            let tracked _p = p;
            auth.update(&mut tok, false);
            v = (auth, Some(perm));
        });
        tok
    }

    /// The release of a holder of the cell's permission: outside the fork
    /// window, the permission and the token back ([`Lock::returned`]),
    /// then the word ([`RawLock::release_word`]); inside it (the
    /// acquisition was one the window served, [`axiom_window_cell`]),
    /// nothing, as [`RawLock::release`]. The guard's drop calls it with
    /// the guard's ghost fields.
    #[inline(always)]
    fn release_cell(&self, Tracked(t): Tracked<Unlocked>, Tracked(perm): Tracked<PointsTo<T>>)
        where T: LockSpec
        requires
            crate::sys::may_syscall(),
            t.id() == self.raw.token_loc(),
            self.has_perm(perm),
            lock_inv(*perm.value()),
            lock_frame(lock_init(*self), *perm.value()),
    {
        if fork_holder_is_me() {
            return;
        }
        let Tracked(credit) = vstd::invariant::create_open_invariant_credit();
        let tracked tok = self.returned(t, perm, credit);
        self.raw.release_word(Tracked(tok));
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
        let Tracked(t) = self.raw.acquire();
        let Tracked(credit) = vstd::invariant::create_open_invariant_credit();
        let tracked (tok, perm) = self.acquired(t, credit);
        let g = LockGuard { lock: self, perm: Tracked(perm), tok: Tracked(tok) };
        proof {
            *under = LockOrder { top: self.rank(), root: false, ranks: Set::empty(), life: PhantomData };
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
        let (ok, Tracked(t)) = self.raw.try_lock();
        if ok {
            let Tracked(credit) = vstd::invariant::create_open_invariant_credit();
            let tracked (tok, perm) = self.acquired(t, credit);
            Some(LockGuard { lock: self, perm: Tracked(perm), tok: Tracked(tok) })
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

    /// [`RawLock::lock_held`] for a holder that also keeps the value:
    /// acquire, and return the hold's token with the permission of the
    /// cell, taken out of the cell's invariant as [`Lock::lock`]
    /// takes it for its guard ([`Lock::acquired`]). The holder reads the
    /// value through it ([`Lock::value_held`]) and writes it through it
    /// ([`Lock::value_held_mut`]), and nothing else can touch the value while it
    /// keeps it: the fork handlers keep the creation lock's
    /// (`arena::Created`) and the background state lock's
    /// (`purge::background::StateHold`) from `fork_prepare` to the release,
    /// which hands the permission back with the token
    /// ([`Lock::unlock_held_perm`]). The caller must not take a guard of
    /// this lock while it keeps the permission, nor release the lock
    /// through `RawLock::unlock_held` instead: either would leave a second
    /// permission to the cell possible (the lock order already refuses a
    /// guard's scope here, `ord` must be a root). Crate-private, for the
    /// arena registry's creation lock (`Arenas::lock_held`) and the state
    /// lock (`purge::background::fork_lock`). Taking the lock of
    /// [`rank_state`] gives [`fork_prepared`] of `ord`, which nothing else
    /// gives.
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
            final(ord).ranks() == old(ord).ranks().insert(self.rank()),
            self.rank() == rank_state() ==> fork_prepared(*final(ord)),
    {
        let Tracked(t) = self.raw.acquire();
        let Tracked(credit) = vstd::invariant::create_open_invariant_credit();
        let tracked (tok, perm) = self.acquired(t, credit);
        proof {
            ord.top = self.rank();
            ord.ranks = ord.ranks.insert(self.rank());
        }
        Tracked((LockHeld { lock: self.raw, tok }, perm))
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

    /// The value, borrowed for writing through the permission `perm` of
    /// the cell, which a holder keeps ([`Lock::lock_held_perm`]): the write
    /// counterpart of [`Lock::value_held`]. The permission's final value is
    /// the reference's, as [`LockGuard`]'s `deref_mut` gives a guard's.
    #[inline(always)]
    pub(crate) fn value_held_mut<'a>(&'a self, Tracked(perm): Tracked<&'a mut PointsTo<T>>) -> (r: &'a mut T)
        requires
            self.has_perm(*old(perm)),
        ensures
            *r == *old(perm).value(),
            *final(r) == *final(perm).value(),
            self.has_perm(*final(perm)),
    {
        self.value.borrow_mut(Tracked(perm))
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
    /// [`lock_frame`], which the cell's invariant holds for the next
    /// acquisition ([`Lock::release_cell`]).
    #[inline(always)]
    pub(crate) fn unlock_held_perm(&self, Tracked(t): Tracked<LockHeld>, Tracked(perm): Tracked<PointsTo<T>>, Tracked(ord): Tracked<&mut LockOrder>)
        where T: LockSpec
        requires
            crate::sys::may_syscall(),
            t.held() == self.spec_raw(),
            self.has_perm(perm),
            lock_inv(*perm.value()),
            lock_frame(lock_init(*self), *perm.value()),
            window_closed(),
            old(ord).root(),
        ensures
            final(ord).root(),
            final(ord).top() == old(ord).top(),
            final(ord).ranks() == old(ord).ranks().remove(t.held().rank()),
    {
        proof {
            ord.ranks = ord.ranks.remove(t.held().rank());
        }
        self.release_cell(Tracked(t.into_token()), Tracked(perm));
    }
}

/// Exclusive access to a [`Lock`]'s value; unlocks on drop. Holds the
/// permission of the lock's cell, taken at the acquisition
/// ([`Lock::acquired`]), and the lock's token, until the drop releases
/// the lock ([`Lock::release_cell`]).
#[verifier::reject_recursive_types(T)]
pub struct LockGuard<'a, T: LockSpec> {
    lock: &'a Lock<T>,
    /// Ghost: the permission of the lock's cell.
    #[allow(dead_code)]
    perm: Tracked<PointsTo<T>>,
    /// Ghost: the lock's token ([`Unlocked`], at `true` while the
    /// permission is out of the cell's invariant), which the drop's
    /// release hands back.
    #[allow(dead_code)]
    tok: Tracked<Unlocked>,
}

impl<'a, T: LockSpec> LockGuard<'a, T> {
    /// The permission is the cell's, and the token the lock's.
    #[verifier::type_invariant]
    closed spec fn wf(&self) -> bool {
        &&& self.perm@.id() == self.lock.value.id()
        &&& self.tok@.id() == self.lock.raw.token_loc()
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
    /// unlock a verified holder ends with, which checks what the cell's
    /// invariant takes back for the next acquisition. The release is the
    /// guard's drop, at the end of the body ([`Lock::release_cell`]).
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

impl<'a, T: LockSpec> View for LockGuard<'a, T> {
    type V = T;

    /// The guarded value: the cell's, through the guard's permission.
    closed spec fn view(&self) -> T {
        *self.perm@.value()
    }
}

impl<T: LockSpec> Deref for LockGuard<'_, T> {
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

impl<T: LockSpec> DerefMut for LockGuard<'_, T> {
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
// field. Each contract is the vstd one of the call it makes, carried to
// a permission held by reference (`&Tracked<PointsTo<T>>`,
// `&mut Tracked<PointsTo<T>>`) rather than passed as
// `Tracked<&PointsTo<T>>`. Outside Verus `Tracked(..)` erases to
// `Tracked::assume_new_fallback(|| unreachable!())`, which leaves `t`
// unused in an ordinary build.

/// [`PCell::borrow`] with the permission `t`.
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
    c.borrow(Tracked(t.borrow()))
}

/// [`PCell::borrow_mut`] with the permission `t`.
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
    c.borrow_mut(Tracked(t.borrow_mut()))
}

// Trusted: Verus wants a destructor that opens no invariant and cannot
// unwind, and `Lock::release_cell` opens the cell's and the word's
// invariants and is not marked `no_unwind`. The body is the release of the
// guard's token and permission (`Lock::release_cell`), and claims nothing.
// It reads the two ghost fields out of the guard (`Tracked` is zero-sized,
// so the reads copy nothing; the guard is gone after the drop, so each is
// handed over once). The requirement it does not check: the token and
// permission are the lock's (the guard's type invariant gives it), and the
// value keeps `lock_inv` and `lock_frame` (a guard that ends with
// `LockGuard::unlock` checks it there; for a guard that drops instead it
// is the assumption `lock_inv` describes). Its `no_unwind` rests on the
// `panic = "abort"` profiles (test builds unwind).
impl<T: LockSpec> Drop for LockGuard<'_, T> {
    #[inline]
    #[verifier::external_body]
    fn drop(&mut self)
        opens_invariants none
        no_unwind
    {
        // SAFETY: both fields are `Tracked`, zero-sized, so the reads copy
        // no bytes. Each is read once, here, and handed to `release_cell`.
        // The originals left in `*self` are not handed over again: the
        // guard is gone once `drop` returns, and the drop glue that runs on
        // its fields then does nothing for them, because `Tracked` has no
        // `Drop` in ordinary builds (its ghost value exists only under
        // Verus). Moving them out needs a raw read because `drop` has only
        // `&mut self`, and the safe ways to take a field (`mem::replace`,
        // `mem::take`) need a second `Tracked` value, which exec code can
        // only make through vstd's assumed constructor, an assumption this
        // crate does not use.
        let (tok, perm) = unsafe { (core::ptr::read(&self.tok), core::ptr::read(&self.perm)) };
        self.lock.release_cell(tok, perm);
    }
}

} // verus!

#[cfg(test)]
mod tests;

/// Kani harness for [`Lock::value_ptr`] (`cfg(kani)` only; the value
/// field is private, so it sits here).
#[cfg(kani)]
mod kani_lock {
    use super::*;
    use core::mem::{MaybeUninit, align_of, offset_of, size_of};

    /// `Lock::value_ptr`, `requires perm.ptr() == p`, no `ensures`: on
    /// the pointer to a lock of `T` that a permission names, here the
    /// storage of one (uninitialised: the call reads nothing, and the
    /// permission is a made-up one, `Tracked::assume_new()`, which the body
    /// does not use), at `T` a byte, a word, a padded pair and
    /// `ArenaStats` (`Arena::put_stats`' instance), it returns, and
    /// (beyond the contract) the pointer is the value field's: its `T`
    /// fills the cell, lies inside the lock and apart from the lock word,
    /// is aligned for `T` and is writable.
    #[kani::proof]
    fn value_ptr_is_the_value_field() {
        fn on<T>() {
            let mut l = MaybeUninit::<Lock<T>>::uninit();
            let p = l.as_mut_ptr();
            let r = Lock::value_ptr(p, Tracked::assume_new());
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
