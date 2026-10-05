//! Process-wide allocator state and its lazy initialisation (design.md
//! section 10), plus the fork handlers (section 9). The entry points in the
//! submodules reach the allocator through [`shared`].
//!
//! Init is triggered by the first call to any entry point and serialised
//! with an atomic state machine `Uninit -> Initialising -> Ready`. A second
//! thread arriving during `Initialising` spins; the initialising thread
//! re-entering the allocator (something in init allocated) aborts with a
//! message, because serving it would need the state that is being built.
//! ld.so may call `malloc` before constructors run, so nothing here relies
//! on `.init_array`.
//!
//! Init never allocates. It probes the kernel, reserves the regions, builds
//! the [`Shared`] in place inside a static, creates the thread-exit key
//! (inside `Shared::init`) and registers the fork handlers. glibc's
//! `pthread_key_create` and `pthread_atfork` do not allocate for the first
//! keys and handlers, which is all we need.
//!
//! The fork prepare handler takes every allocator lock in nesting order
//! (design section 9; the list is [`fork_prepare`]'s body): the explicit-cache
//! table lock, the arena creation lock, every arena's class locks and
//! slab-set lock, the slab table locks, the huge lock, the metadata lock,
//! the process and per-arena statistics locks, then the background
//! thread's state lock. That order matches the allocator's own nesting (the
//! explicit-cache table lock is held across heap creation and destruction,
//! which take the arena creation lock, class locks and the metadata lock;
//! class, slab table and huge code take the metadata lock while holding
//! their own; nothing takes a class or huge lock while holding metadata;
//! the statistics locks and the background state lock are leaves, only
//! ever held alone), so a thread that is mid-operation when another thread
//! forks can always finish and release. Parent and child release in
//! reverse. The child also marks the background thread absent. Other
//! threads' heaps are left alone in the child, as in every other allocator.
//!
//! The prepare handler also blocks every signal on the forking thread and the
//! parent and child handlers restore the mask after their unlocks. glibc's
//! `_Fork` blocks signals only around the `clone` itself, so a handler that
//! runs between our prepare and our parent handler (glibc restores the mask
//! before the parent handlers run) would otherwise see every allocator lock
//! held by its own thread: a `fork` from that handler deadlocks in prepare,
//! and glibc's `tst-mallocfork` provokes exactly that when the child's
//! `SIGALRM` overtakes the parent's post-fork release (seen under Rosetta).

/// One entry point exported twice, under its plain name and its `je_`
/// alias (design 13.2: what a program built against a
/// `--with-jemalloc-prefix=je_` jemalloc calls), as two thin `extern "C"`
/// wrappers with the same body: stable Rust has no symbol aliases. Used in
/// the `exports` modules, which are compiled out of the test binary (see
/// `libc.rs`); `#[unsafe(no_mangle)]` exports each function under its own
/// name, so the alias is spelled out in full at the definition.
#[cfg(not(test))]
macro_rules! export_pair {
    ($($(#[$m:meta])* fn $name:ident / $je:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)? $body:block)*) => {
        $(
            $(#[$m])*
            #[unsafe(no_mangle)]
            pub extern "C" fn $name($($arg: $ty),*) $(-> $ret)? $body
            $(#[$m])*
            #[unsafe(no_mangle)]
            pub extern "C" fn $je($($arg: $ty),*) $(-> $ret)? $body
        )*
    };
}

/// [`export_pair!`] inside `verus!`, for pairs whose bodies Verus checks
/// (`libc.rs`, `jemalloc.rs`). The same two functions in the build.
#[cfg(not(test))]
macro_rules! verified_export_pair {
    ($($(#[$m:meta])* fn $name:ident / $je:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)? $body:block)*) => {
        ::vstd::prelude::verus! {
        $(
            $(#[$m])*
            #[unsafe(no_mangle)]
            pub extern "C" fn $name($($arg: $ty),*) $(-> $ret)? {
                proof {
                    crate::sys::grant_syscalls();
                    crate::sys::grant_aborts();
                }
                $body
            }
            $(#[$m])*
            #[unsafe(no_mangle)]
            pub extern "C" fn $je($($arg: $ty),*) $(-> $ret)? {
                proof {
                    crate::sys::grant_syscalls();
                    crate::sys::grant_aborts();
                }
                $body
            }
        )*
        } // verus!
    };
}

// The C++ and libc exports are compiled out of the test binary, which
// leaves the helpers only they call unused there.
#[cfg_attr(test, allow(dead_code))]
pub mod cxx;
pub mod glibc;
pub mod jemalloc;
#[cfg_attr(test, allow(dead_code))]
pub mod libc;
pub mod mallctl;
pub mod print;

#[cfg(test)]
mod c_programs;

use crate::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;

use crate::arena::Arena;
use crate::config;
use crate::heap::Shared;
use crate::layout::{self, NCLASSES, Regions};
use crate::lock::LockHeld;
#[cfg(verus_only)]
use crate::lock::{RawLock, axiom_entry, rank_arena_stats, rank_class, rank_table};
use crate::purge;
use crate::sys;
use crate::sys::features::{FeatureLine, Features};
#[cfg(verus_only)]
use crate::sys::{SigMask, all_blocked, effective};
use crate::vspec::once;
use crate::vspec::word::WordPerm;
#[cfg(verus_only)]
use crate::vspec::word::word_addr;
#[cfg(verus_only)]
use vstd::invariant::AtomicInvariant;
use vstd::invariant::InvariantPredicate;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Map, Tracked, int, verus};
use vstd::raw_ptr::PointsTo;

verus! {

/// The crate version as a NUL-terminated static string (`"0.1.0"`; the
/// soname's major, design 13.6, follows its major); with `challoc_features`
/// and the size-returning entry points of design 13.7 (`libc.rs`) the whole
/// challoc-specific surface, declared in `<challoc/challoc.h>`. A program
/// or test that wants to know whether challoc is the loaded allocator looks
/// this symbol up with `dlsym`; it reads and writes no allocator state and
/// does not initialise anything.
/// `mallctl("version")` stays jemalloc-shaped (`5.3.0-0-gchalloc`) for
/// parsers of that string.
#[unsafe(no_mangle)]
pub extern "C" fn challoc_version() -> *const core::ffi::c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const core::ffi::c_char
}

pub const UNINIT: u32 = 0;
pub const INITIALISING: u32 = 1;
pub const READY: u32 = 2;

exec static STATE: AtomicU32 = AtomicU32::new(UNINIT);
/// Thread id of the initialising thread while the state is `INITIALISING`,
/// so a re-entrant call from it can be told apart from a concurrent one.
exec static INIT_TID: AtomicU32 = AtomicU32::new(0);

/// Storage for a value built in place at init and immutable afterwards.
/// Verus does not model the contents of the `UnsafeCell`; verified code
/// reads and writes a static's storage through a permission to it, at
/// the address its projection names ([`shared_storage_ptr`],
/// [`features_ptr`], [`line_ptr`]).
#[verifier::reject_recursive_types(T)]
struct Global<T>(UnsafeCell<MaybeUninit<T>>);

impl<T> Global<T> {
    const fn new() -> Self {
        Global(UnsafeCell::new(MaybeUninit::uninit()))
    }
}

} // verus!

// SAFETY: written exactly once, by the initialising thread before it
// publishes `READY` with a release store; every reader loads the state with
// acquire first and then only takes shared references.
unsafe impl<T> Sync for Global<T> {}

verus! {

exec static SHARED: Global<Shared> = Global::new();
exec static FEATURES: Global<Features> = Global::new();
/// The feature line for `challoc_features()` and `malloc_stats_print`.
exec static FEATURE_LINE: Global<FeatureLine> = Global::new();

} // verus!

verus! {

/// Signal mask of the forking thread, saved by `fork_prepare` and restored by
/// `fork_release`. One slot suffices because it is only written and read
/// while the writer holds every allocator lock: a second thread in
/// `fork_prepare` (glibc serialises fork handlers with its atfork lock, but
/// the unit tests call them directly) waits on the first class lock before
/// it can store, and the releaser reads before its first unlock. A handler
/// cannot nest a fork inside the window because signals are blocked for its
/// duration. An atomic so that verified code can read and write it: the
/// accesses are relaxed (a plain load and store), ordered by the locks.
/// The release reads it through the permission its hold carries
/// ([`ForkHeld::has_slot`]), so the value read is the mask the hold
/// records.
exec static FORK_SIGMASK: AtomicU64 = AtomicU64::new(0);

/// The identity of [`FORK_SIGMASK`] for its permission
/// ([`ForkHeld::has_slot`]); [`fork_slot`] names the static.
pub uninterp spec fn fork_slot_addr() -> usize;

/// [`FORK_SIGMASK`] as a reference. Trusted: Verus cannot name a static
/// in a specification, so its identity ([`fork_slot_addr`]) is stated
/// here. Nothing is claimed of its content.
#[verifier::external_body]
#[inline(always)]
fn fork_slot() -> (r: &'static AtomicU64)
    ensures
        word_addr(r) == fork_slot_addr(),
{
    &FORK_SIGMASK
}

/// The permissions to the statics `init` writes once, frozen when it is
/// done: the published `Shared` (the value `Shared::init` built, which
/// is well formed), the probed features, the feature line, and the
/// class table after [`layout::adopt_page_geometry`]. `STATE`'s
/// invariant holds them from `READY` on ([`StatePred`]), so a thread
/// that loads `READY` gets them ([`load_state`]); being `&'static`, they
/// are shared by every reader, and nothing writes the statics through
/// them again (the locks and atomics inside `Shared` change through
/// `&Shared`, but nothing [`Shared::wf`] reads does).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct Published {
    pub tracked shared: &'static PointsTo<Shared>,
    pub tracked features: &'static PointsTo<Features>,
    pub tracked line: &'static PointsTo<FeatureLine>,
    pub tracked classes: &'static PointsTo<[layout::Class; NCLASSES]>,
}

impl Published {
    /// Each permission is to its static and initialised; the `Shared` is
    /// the one built at its storage, well formed; the class table is
    /// well formed ([`layout::table_ok`]).
    pub open spec fn ok(&self) -> bool {
        &&& self.shared.ptr() == Shared::storage()
        &&& self.shared.is_init()
        &&& self.shared.value() == Shared::built_at(Shared::storage())
        &&& self.shared.value().wf()
        &&& self.features.ptr() == features_storage()
        &&& self.features.is_init()
        &&& self.line.ptr() == line_storage()
        &&& self.line.is_init()
        &&& self.classes.ptr() == layout::classes_storage()
        &&& self.classes.is_init()
        &&& layout::table_ok(self.classes.value())
    }
}

/// The published permissions a thread has seen, if any: what the
/// functions that load or write `STATE` hand back.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct Seen {
    pub tracked published: Option<&'static Published>,
}

impl Seen {
    /// The thread has seen the published permissions.
    pub open spec fn ok(&self) -> bool {
        self.published matches Some(p) && p.ok()
    }
}

/// Ghost permission to `STATE`: its current [`value`](StatePerm::value).
/// It is not `Copy`, and the one permission is in `STATE`'s invariant
/// ([`axiom_state`]).
#[verifier::external_body]
pub tracked struct StatePerm {
    _no_copy: (),
}

impl StatePerm {
    /// `STATE`'s value.
    pub uninterp spec fn value(&self) -> u32;
}

/// What `STATE`'s invariant holds: its permission, the process's
/// one-shot tokens until the winning exchange takes them, and the
/// published permissions from `READY` on.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct StateVal {
    pub tracked perm: StatePerm,
    pub tracked steps: Option<Map<once::Step, once::Once>>,
    pub tracked published: Option<&'static Published>,
}

/// `STATE`'s invariant: the state is `UNINIT`, `INITIALISING` or
/// `READY`; while it is `UNINIT` the invariant holds the tokens of every
/// step ([`once::all_steps`]), and from `READY` on the published
/// permissions ([`Published::ok`]). `STATE` moves from `UNINIT` to
/// `INITIALISING` only at [`claim_init`]'s exchange, which takes the
/// tokens, and to `READY` only at [`init_slow`]'s store, after `init`
/// built and froze what it publishes; nothing writes it otherwise, so it
/// never goes back.
// Ghost: named only by proof code.
#[allow(dead_code)]
struct StatePred;

impl InvariantPredicate<(), StateVal> for StatePred {
    open spec fn inv(k: (), v: StateVal) -> bool {
        &&& v.perm.value() == UNINIT || v.perm.value() == INITIALISING || v.perm.value() == READY
        &&& v.perm.value() == UNINIT ==> (v.steps matches Some(m) && once::all_steps(m))
        &&& v.perm.value() == READY ==> (v.published matches Some(p) && p.ok())
    }
}

/// The namespace of `STATE`'s invariant.
spec fn state_ns() -> int {
    0x7374_6174
}

/// `STATE`'s invariant. Trusted: the process starts with `STATE` at
/// `UNINIT` (the static's initialiser) and with the tokens of every step
/// of building the allocator, none taken ([`once`]); every write of
/// `STATE` is [`state_claim`]'s or [`state_publish`]'s, inside this
/// invariant (`verus/loom/run.sh` checks that `STATE` has those two
/// writes), and verified code learns its value only from them and
/// [`state_load`] (a test-only load elsewhere carries no ghost state).
/// The axiom stands for one invariant, the same object at every call.
/// Verus does not know that two calls return the same object, which
/// only weakens what it can prove: what it proves holds of the one
/// invariant, whose namespace is fixed so that no two opens nest.
axiom fn axiom_state() -> (tracked r: &'static AtomicInvariant<(), StateVal, StatePred>)
    ensures
        r.namespace() == state_ns(),
;

/// `STATE.load(Acquire)`: the permission's value. Trusted: a `core`
/// atomic load, one atomic operation on `STATE` (`verus/TODO.md`, Tier 4,
/// for the ordering).
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
fn state_load(Tracked(perm): Tracked<&StatePerm>) -> (r: u32)
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    STATE.load(Ordering::Acquire)
}

/// `STATE`'s exchange from `UNINIT` to `INITIALISING`: `Ok(UNINIT)`
/// when it was `UNINIT` (and is now `INITIALISING`), `Err` of the value
/// it has otherwise, left unchanged. Trusted: a `core` compare-exchange,
/// one atomic read-modify-write on `STATE`.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
fn state_claim(Tracked(perm): Tracked<&mut StatePerm>) -> (r: Result<u32, u32>)
    ensures
        old(perm).value() == UNINIT ==> r == Ok::<u32, u32>(UNINIT) && final(perm).value() == INITIALISING,
        old(perm).value() != UNINIT ==> r == Err::<u32, u32>(old(perm).value()) && final(perm).value() == old(perm).value(),
    opens_invariants none
    no_unwind
{
    STATE.compare_exchange(UNINIT, INITIALISING, Ordering::Acquire, Ordering::Acquire)
}

/// `STATE`'s `READY` store, `Release`. Trusted: a `core` atomic store, one
/// atomic operation on `STATE`.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
fn state_publish(Tracked(perm): Tracked<&mut StatePerm>)
    ensures
        final(perm).value() == READY,
    opens_invariants none
    no_unwind
{
    STATE.store(READY, Ordering::Release);
}

/// An acquire load of `STATE`, with the published permissions when it
/// reads `READY`.
#[inline(always)]
fn load_state(Tracked(seen): Tracked<&mut Seen>) -> (s: u32)
    ensures
        s == READY ==> final(seen).ok(),
    opens_invariants [ state_ns() ]
{
    let s;
    vstd::open_atomic_invariant!(axiom_state() => v => {
        s = state_load(Tracked(&v.perm));
        proof {
            if s == READY {
                seen.published = v.published;
            }
        }
    });
    s
}

/// `SHARED` as a reference, through `published`, the permissions frozen
/// at `READY` ([`load_state`] hands them to a thread that loads it): the
/// value `Shared::init` built at the static's storage
/// ([`Shared::built_at`]`(`[`Shared::storage`]`)`), well formed
/// ([`Published::ok`]).
///
/// # Safety
/// The value must have been initialised and `READY` observed with an
/// acquire load on this thread (what `published` stands for).
#[inline(always)]
unsafe fn shared_ref(Tracked(published): Tracked<&'static Published>) -> (r: &'static Shared)
    requires
        published.ok(),
    ensures
        r.wf(),
        *r == Shared::built_at(Shared::storage()),
    opens_invariants none
{
    vstd::raw_ptr::ptr_ref(shared_storage_ptr(), Tracked(published.shared))
}

/// The process's allocator state, initialising it on first use.
#[inline(always)]
pub fn shared() -> (r: &'static Shared)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r.wf(),
{
    let tracked mut seen = Seen { published: None };
    if load_state(Tracked(&mut seen)) == READY {
        // SAFETY: `READY` was published after init with a release store.
        return unsafe { shared_ref(Tracked(seen.published.tracked_unwrap())) };
    }
    init_slow()
}

/// The allocator state if it is ready, `None` before init has completed.
/// `free` uses this: a pointer arriving before any allocation cannot be
/// ours and must not trigger init.
#[inline(always)]
pub fn shared_if_ready() -> (r: Option<&'static Shared>)
    ensures
        r matches Some(s) ==> s.wf(),
{
    let tracked mut seen = Seen { published: None };
    if load_state(Tracked(&mut seen)) == READY {
        // SAFETY: as in `shared`.
        Some(unsafe { shared_ref(Tracked(seen.published.tracked_unwrap())) })
    } else {
        None
    }
}

/// The allocator state's address, with no readiness check and no
/// reference formed: what the fast paths hand `heap::existing_heap`, which
/// reads one byte through it only after the thread's TLS slot has proved
/// init complete. Trusted: a raw-pointer projection of a static.
#[verifier::external_body]
#[inline(always)]
pub fn shared_ptr() -> *const Shared opens_invariants none {
    (&raw const SHARED.0).cast::<Shared>()
}

/// The allocator state without the readiness check.
///
/// # Safety
/// The calling thread must itself have observed `READY` earlier (through
/// [`shared`] or [`shared_if_ready`]). The fast paths use the thread's heap
/// pointer as that witness: a heap is only created under `shared()`.
///
/// Trusted: that the caller observed `READY`, so that the value is the
/// published one, well formed (what [`shared_ref`] proves from the
/// permissions frozen at `READY`). The witness the fast paths have, the
/// heap pointer in their TLS slot, carries no ghost state, so the
/// permissions do not reach here.
#[verifier::external_body]
#[inline(always)]
pub unsafe fn shared_unchecked() -> (r: &'static Shared)
    ensures
        r.wf(),
    opens_invariants none
{
    // SAFETY: guaranteed by the caller.
    unsafe { &*shared_storage_ptr() }
}

/// The published feature line, without the readiness check.
///
/// # Safety
/// The calling thread must itself have observed `READY` earlier (its
/// `shared()` returned).
///
/// Trusted: that the caller observed `READY`, so that the line is
/// initialised (what the permissions frozen at `READY` stand for:
/// [`Published::ok`]). Its callers learn it from `shared()`, whose result
/// carries no ghost state, so the permissions do not reach here.
#[verifier::external_body]
#[inline(always)]
unsafe fn published_line() -> &'static FeatureLine
    opens_invariants none
{
    // SAFETY: guaranteed by the caller.
    unsafe { &*line_ptr() }
}

} // verus!

/// The probed kernel features. Initialises the allocator if needed.
#[cfg(test)]
pub fn features() -> &'static Features {
    let _ = shared();
    // SAFETY: `shared()` returned, so `READY` has been observed.
    unsafe { &*features_ptr() }
}

verus! {

/// The feature line (`Features`'s `Display`, built at init). Initialises
/// the allocator if needed.
pub fn feature_line() -> &'static str
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    let _ = shared();
    // SAFETY: `shared()` returned, so `READY` has been observed.
    unsafe { published_line() }.as_str()
}

/// `challoc_features()`: the feature report as one NUL-terminated line of
/// `key=value` pairs (design.md sections 16 and 17): the kernel probes
/// (`page_size`, `guard_install`, `mseal`, `anon_name`, ...) then `pac`.
/// The string is static, built once at init without allocating, and
/// identical to the `Kernel and CPU features:` line of
/// `malloc_stats_print`. Initialises the allocator if needed.
#[unsafe(no_mangle)]
pub extern "C" fn challoc_features() -> *const core::ffi::c_char {
    proof {
        crate::sys::grant_syscalls();
        crate::sys::grant_aborts();
    }
    let _ = shared();
    // SAFETY: `shared()` returned, so `READY` has been observed.
    unsafe { published_line() }.as_ptr()
}

/// Theorem "Read-only APIs never abort" (`verus/THEOREMS.md`): the
/// read-only entry points make these calls, in a context where only
/// [`sys::integrity_aborts`] holds, not [`sys::may_abort`]. Each call
/// verifies, so none of these functions requires `may_abort`, and every
/// abort but an integrity check does: they abort at most at an integrity
/// check, for no argument. `get_defrag_hint`, `malloc_usable_size(NULL)`
/// and the utilization queries are called whole; `nallocx`, `mallinfo`,
/// `mallinfo2`, `challoc_features` and `mallctl` through the calls their
/// bodies make after `shared()`, whose init may abort (it returns at once
/// once init has completed, which `shared` stands for here). The
/// `mallctl` call takes its token `cc` as its export wrapper mints it
/// (`vspec::cbuf::CCall`); `mallctlbymib`'s witness, with its own call's
/// token, is `mallctl::theorem_mallctlbymib_reads_never_abort` (no
/// verified function holds two tokens).
///
/// Verification only (`cfg(verus_only)`): the build has no such function.
#[cfg(verus_only)]
fn theorem_read_only_never_aborts(
    shared: &Shared,
    size: usize,
    flags: core::ffi::c_int,
    p: *const u8,
    verbose: bool,
    io: &mallctl::Io,
    name: *const core::ffi::c_char,
    mib: &[usize],
    Tracked(cc): Tracked<crate::vspec::cbuf::CCall>,
)
    requires
        crate::sys::may_syscall(),
        crate::sys::integrity_aborts(),
        shared.wf(),
        cc.fresh(crate::vspec::cbuf::CEntry::Mallctl {
            name: name as *const u8,
            oldp: io.oldp as *mut u8,
            oldlenp: io.oldlenp as *const usize,
            newp: io.newp as *mut u8,
            newlen: io.newlen,
        }),
{
    proof {
        shared.lemma_wf();
    }
    // `get_defrag_hint`, whole.
    let _ = jemalloc::get_defrag_hint(p);
    // `malloc_usable_size(NULL)`, whole.
    let _ = libc::malloc_usable_size(core::ptr::null_mut());
    // `nallocx` after its `shared()`.
    let f = crate::heap::mallocx::Flags::decode(flags as u32);
    let _ = jemalloc::usable_for(shared, size, &f).unwrap_or(0);
    // `mallinfo2` (and `mallinfo`, which adds clamps) after its
    // `shared()`: the calls its body makes.
    let tracked mut sd = Ghost((Seq::<crate::stats::ArenaStats>::empty(), Seq::<int>::empty()));
    // Premise (lock order), as at `mallinfo2`'s call site: an entry
    // point holds no allocator lock.
    let tracked mut ord = crate::lock::axiom_entry();
    let all = crate::stats::collect_all(shared, Tracked(&mut sd), Tracked(&mut ord));
    let _ = shared.regions.page_size();
    let _ = crate::stats::fig_add(all.small_allocated, all.large_allocated);
    let _ = crate::stats::fig_mul(all.pdirty, all.large_runs);
    // `challoc_features` after its `shared()`.
    // SAFETY: `shared` exists, so `READY` has been observed.
    let _ = unsafe { published_line() }.as_ptr();
    // `experimental.utilization.query` and `batch_query`, whole, and the
    // rest of `mallctl`.
    let _ = mallctl::utilization(shared, p as *const core::ffi::c_void, verbose);
    mallctl::theorem_mallctl_reads_never_abort(shared, name, io, mib, Tracked(cc));
}

} // verus!

/// Whether init has completed (for the tests; the entry points use
/// [`shared_if_ready`]).
#[cfg(test)]
pub fn is_ready() -> bool {
    STATE.load(Ordering::Acquire) == READY
}

verus! {

/// `STATE`'s compare-exchange from `UNINIT` to `INITIALISING`, which
/// [`init_slow`] runs: the winner gets the process's one-shot tokens
/// ([`once::all_steps`]), which `STATE`'s invariant holds while it is
/// `UNINIT` ([`StatePred`]); nothing stores `UNINIT` or `INITIALISING`
/// elsewhere, so the exchange succeeds once per process and one caller
/// gets the tokens. A caller that finds `READY` gets the published
/// permissions.
#[inline(always)]
fn claim_init(Tracked(seen): Tracked<&mut Seen>) -> (r: (Result<u32, u32>, Tracked<Map<once::Step, once::Once>>))
    ensures
        r.0 is Ok ==> once::all_steps(r.1@),
        r.0 == Err::<u32, u32>(READY) ==> final(seen).ok(),
{
    let tracked mut steps = Map::tracked_empty();
    let won;
    vstd::open_atomic_invariant!(axiom_state() => v => {
        won = state_claim(Tracked(&mut v.perm));
        proof {
            if won is Ok {
                steps = v.steps.tracked_take();
            }
            if won == Err::<u32, u32>(READY) {
                seen.published = v.published;
            }
        }
    });
    (won, Tracked(steps))
}

/// Publish `READY` with the permissions `init` froze ([`StatePred`]).
#[inline(always)]
fn publish_ready(Tracked(published): Tracked<&'static Published>)
    requires
        published.ok(),
    opens_invariants [ state_ns() ]
{
    vstd::open_atomic_invariant!(axiom_state() => v => {
        state_publish(Tracked(&mut v.perm));
        proof {
            v.published = Some(published);
        }
    });
}

/// The first caller runs `init` and publishes `READY`; a concurrent one
/// waits for it (termination is not claimed: the wait ends when the other
/// thread's `init` does); the initialising thread re-entering aborts.
#[cold]
#[inline(never)]
#[verifier::exec_allows_no_decreases_clause]
fn init_slow() -> (r: &'static Shared)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r.wf(),
{
    let tracked mut seen = Seen { published: None };
    let (won, Tracked(steps)) = claim_init(Tracked(&mut seen));
    match won {
        Ok(_) => {
            INIT_TID.store(sys::gettid() as u32, Ordering::Relaxed);
            init(Tracked(steps), Tracked(&mut seen));
            publish_ready(Tracked(seen.published.tracked_unwrap()));
        }
        Err(READY) => {}
        Err(_) => {
            // The `INIT_TID` store happens before any allocation the init
            // thread could make, so a matching tid here is re-entry.
            if INIT_TID.load(Ordering::Relaxed) == sys::gettid() as u32 {
                sys::abort("allocation during initialisation (re-entrant init)");
            }
            while load_state(Tracked(&mut seen)) != READY {
                core::hint::spin_loop();
            }
        }
    }
    // SAFETY: `READY` observed (or published by this thread).
    unsafe { shared_ref(Tracked(seen.published.tracked_unwrap())) }
}

/// Never-committed ranges of the reservations sealed at init (design 10
/// step 5); 0 when the kernel has no `mseal` or seccomp filters it.
exec static SEALED: AtomicU32 = AtomicU32::new(0);

/// How many ranges init sealed with `mseal` (for `malloc_stats_print` and
/// tests). Initialises the allocator if needed.
pub fn sealed_ranges() -> u32
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    let _ = shared();
    SEALED.load(Ordering::Relaxed)
}

/// The storage of [`SHARED`], the process's [`Shared::storage`]. Trusted:
/// a raw-pointer projection of a static.
#[verifier::external_body]
#[inline(always)]
fn shared_storage_ptr() -> (p: *mut Shared)
    ensures
        p == Shared::storage(),
    opens_invariants none
{
    // The cell's own pointer, not `as_mut_ptr` through a `&mut`:
    // `init` keeps pointers derived from it, and writes through the
    // readers' `&Shared` would invalidate ones derived from a `&mut`.
    SHARED.0.get().cast::<Shared>()
}

/// The address of [`FEATURES`]' storage.
pub uninterp spec fn features_storage() -> *mut Features;

/// The address of [`FEATURE_LINE`]'s storage.
pub uninterp spec fn line_storage() -> *mut FeatureLine;

/// The storage of [`FEATURES`]. Trusted: a raw-pointer projection of a
/// static.
#[verifier::external_body]
#[inline(always)]
fn features_ptr() -> (p: *mut Features)
    ensures
        p == features_storage(),
    opens_invariants none
{
    FEATURES.0.get().cast::<Features>()
}

/// The storage of [`FEATURE_LINE`]. Trusted: a raw-pointer projection of
/// a static.
#[verifier::external_body]
#[inline(always)]
fn line_ptr() -> (p: *mut FeatureLine)
    ensures
        p == line_storage(),
    opens_invariants none
{
    FEATURE_LINE.0.get().cast::<FeatureLine>()
}

/// The permissions to the storage of the statics `init` writes itself,
/// for `once`, the process's token of this step: [`FEATURES`] and
/// [`FEATURE_LINE`], uninitialised, and the class table
/// ([`layout::CLASSES`]), holding its initial value,
/// [`layout::BASE_CLASSES`] (`ClassTable::new`), whose entries are well
/// formed and sized ([`layout::table_ok`]). Trusted: the statics'
/// storage and initial values, which nothing but `init` writes
/// ([`publish_features`], [`layout::adopt_page_geometry`]), and that
/// `BASE_CLASSES` meets `table_ok`, which is `build_classes`'s verified
/// postcondition but which a spec cannot read off the `exec const`; the
/// token is used up, so the permissions are handed out once.
axiom fn axiom_statics(tracked once: once::Once) -> (tracked r: (
    PointsTo<Features>,
    PointsTo<FeatureLine>,
    PointsTo<[layout::Class; NCLASSES]>,
))
    requires
        once.step() == once::Step::Statics,
    ensures
        r.0.ptr() == features_storage(),
        r.0.is_uninit(),
        r.1.ptr() == line_storage(),
        r.1.is_uninit(),
        r.2.ptr() == layout::classes_storage(),
        r.2.is_init(),
        layout::table_ok(r.2.value()),
;

/// The published feature report: `FEATURE_LINE` from `features`, then
/// `FEATURES`, through the permissions to their storage.
///
/// # Safety
/// Called once, by `init`, before `READY` is published.
#[inline(always)]
unsafe fn publish_features(
    features: Features,
    Tracked(line): Tracked<&mut PointsTo<FeatureLine>>,
    Tracked(feats): Tracked<&mut PointsTo<Features>>,
)
    requires
        old(line).ptr() == line_storage(),
        old(feats).ptr() == features_storage(),
    ensures
        final(line).ptr() == line_storage(),
        final(line).is_init(),
        final(feats).ptr() == features_storage(),
        final(feats).is_init(),
{
    vstd::raw_ptr::ptr_mut_write(line_ptr(), Tracked(line), FeatureLine::build(&features));
    vstd::raw_ptr::ptr_mut_write(features_ptr(), Tracked(feats), features);
}

/// `pthread_atfork` with the three fork handlers: 0, or the error number.
/// Trusted: passes function pointers, which Verus does not support.
/// Nothing is claimed of the result.
#[verifier::external_body]
#[inline(always)]
fn atfork_handlers() -> i32
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: the three handlers are plain `extern "C"` functions that only
    // touch allocator locks.
    unsafe { ::libc::pthread_atfork(Some(fork_prepare), Some(fork_parent), Some(fork_child)) }
}

/// Register the fork handlers, aborting if glibc refuses.
#[inline(always)]
fn register_fork_handlers()
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    broadcast use crate::vspec::fmt::lemma_errno_fmt_req;
    let rc = atfork_handlers();
    if rc != 0 {
        sys::abort_fmt(format_args!("pthread_atfork failed: {}", sys::Errno(rc)));
    }
}

/// Design section 10 steps 1 to 8 (step 7, the cgroup discovery, happens
/// inside `Shared::init`). Configuration (step 2) is read before the
/// regions are reserved so that nothing it may abort on has been mapped
/// yet; it reads only the weak symbols, one file and `environ`. The
/// `Shared` it builds satisfies [`Shared::wf`] (`Shared::init`'s
/// contract, from `Regions::reserve_owning` and `config::load`), and the
/// metadata allocator owns the metadata region's address space, which
/// `reserve_owning` hands over; `init_slow` runs it once, before it
/// publishes `READY`, with the one-shot tokens its winning exchange got
/// ([`claim_init`]): one for the statics it writes here, the class table
/// and the feature statics ([`axiom_statics`]), and the rest for
/// `Shared::init`, which uses them up. It hands back the permissions to
/// what it wrote, frozen ([`Published`]).
fn init(Tracked(steps): Tracked<Map<once::Step, once::Once>>, Tracked(seen): Tracked<&mut Seen>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        once::all_steps(steps),
    ensures
        final(seen).ok(),
{
    let ghost steps0 = steps;
    let tracked mut steps = steps;
    let tracked statics_once = once::take(&mut steps, once::Step::Statics);
    proof {
        assert forall|s: once::Step| s != once::Step::Statics implies #[trigger] once::holds(steps, s) by {
            assert(once::holds(steps0, s));
        }
    }
    let tracked (mut feats, mut line, mut classes) = axiom_statics(statics_once);
    let features = Features::probe();
    // The slab geometry of the classes whose table slab is below the page
    // (design 3, page-sized slabs): written once here, before any other
    // thread can see the allocator, never again.
    // SAFETY: `init` runs once, under `INITIALISING`, before `READY`.
    unsafe { layout::adopt_page_geometry(features.page_size.trailing_zeros(), Tracked(&mut classes)) };
    let config = config::load();
    proof {
        assert(0x1_0000usize <= (1usize << 33)) by (bit_vector);
    }
    crate::thp::configure(config.thp, &features);
    purge::configure_advice(config.lazy_band);
    purge::set_check_zero(config.check_zero);
    purge::background::request(config.background_thread);
    let (regions, space) = Regions::reserve_owning(features.page_size, features.anon_name);
    if features.mseal {
        SEALED.store(regions.seal(), Ordering::Relaxed);
    }
    let tracked mut built = crate::heap::Built { perm: None };
    // SAFETY: the statics are written once here, before `READY` is
    // published, and `regions` are live reservations this process owns.
    unsafe {
        let ok = Shared::init(
            shared_storage_ptr(),
            regions,
            space,
            config,
            features.guard_install,
            features.pac,
            Tracked(steps),
            Tracked(&mut built),
        );
        if !ok {
            sys::abort("metadata region too small for the allocator tables");
        }
        let tracked shared_perm = built.perm.tracked_unwrap();
        // Design 11: `Shared::init` forced the stored `thp` to `never`
        // under a cgroup limit below 1 GiB; the advice follows.
        if vstd::raw_ptr::ptr_ref(shared_storage_ptr(), Tracked(shared_perm)).policy.thp_forced() {
            crate::thp::force_never();
        }
        publish_features(features, Tracked(&mut line), Tracked(&mut feats));
        proof {
            let tracked p = Published {
                shared: shared_perm,
                features: vstd::modes::tracked_static_ref(feats),
                line: vstd::modes::tracked_static_ref(line),
                classes: vstd::modes::tracked_static_ref(classes),
            };
            seen.published = Some(vstd::modes::tracked_static_ref(p));
        }
    }
    register_fork_handlers();
}

/// A lock the fork handlers take, named by its place in the allocator:
/// the list in [`fork_prepare`]'s body (design section 9). The test
/// `fork_lock_list_matches_the_source_tree` checks the test list
/// `FORK_LOCKS` against every `Lock` in the source tree; that
/// `FORK_LOCKS` and this enum name the same locks is by inspection.
/// Arena locks carry the arena's index (as `Arenas::get_existing` takes
/// it), class and table locks the class. The background thread's state
/// lock is separate, in its own token (see [`fork_holds`]).
// Ghost: named only by proof code.
#[allow(dead_code)]
pub ghost enum ForkLock {
    /// The explicit-cache table lock.
    Tcaches,
    /// The arena creation lock.
    Arenas,
    /// Arena `i`'s class `c` lock.
    Class(int, int),
    /// Arena `i`'s slab-set lock.
    Sets(int),
    /// Class `c`'s slab table lock.
    Table(int),
    /// The huge lock.
    Huge,
    /// The metadata lock.
    Meta,
    /// The process statistics lock.
    Stats,
    /// Arena `i`'s statistics lock.
    ArenaStats(int),
}

/// An arena lock (class or slab set) of an arena in `a`.
pub open spec fn arena_part(k: ForkLock, a: Set<int>) -> bool {
    match k {
        ForkLock::Class(i, c) => a.contains(i) && 0 <= c < NCLASSES,
        ForkLock::Sets(i) => a.contains(i),
        _ => false,
    }
}

/// A slab table lock.
pub open spec fn table_part(k: ForkLock) -> bool {
    match k {
        ForkLock::Table(c) => 0 <= c < NCLASSES,
        _ => false,
    }
}

/// An arena statistics lock of an arena in `a`.
pub open spec fn stats_part(k: ForkLock, a: Set<int>) -> bool {
    match k {
        ForkLock::ArenaStats(i) => a.contains(i),
        _ => false,
    }
}

/// The fork lock list for the arenas in `a` (the arenas `fork_prepare`
/// found),
/// the state lock aside: the explicit-cache table and arena creation
/// locks, every class and slab-set lock of every arena, every slab table
/// lock, the huge, metadata and process statistics locks, and every
/// arena's statistics lock.
pub open spec fn listed(k: ForkLock, a: Set<int>) -> bool {
    ||| k == ForkLock::Tcaches
    ||| k == ForkLock::Arenas
    ||| arena_part(k, a)
    ||| table_part(k)
    ||| k == ForkLock::Huge
    ||| k == ForkLock::Meta
    ||| k == ForkLock::Stats
    ||| stats_part(k, a)
}

/// The lock `k` names in `s`, with arena `i` the value `arenas[i]`: the
/// value of the lock the accessor path through `s` reaches.
pub open spec fn lock_at(s: Shared, arenas: Map<int, Arena>, k: ForkLock) -> RawLock {
    match k {
        ForkLock::Tcaches => s.spec_fork_locks().0,
        ForkLock::Arenas => s.spec_fork_locks().1,
        ForkLock::Class(i, c) => arenas[i].spec_class_state(c).spec_raw_lock(),
        ForkLock::Sets(i) => arenas[i].spec_sets().spec_raw_lock(),
        ForkLock::Table(c) => s.spec_tables()[c].spec_raw_lock(),
        ForkLock::Huge => s.spec_fork_locks().2,
        ForkLock::Meta => s.spec_fork_locks().3,
        ForkLock::Stats => s.spec_fork_locks().4,
        ForkLock::ArenaStats(i) => arenas[i].spec_stats_lock(),
    }
}

/// The hold of the fork lock list (theorem "fork safety", THEOREMS.md):
/// the arenas `fork_prepare` found, `n` the arena count it read, and one
/// [`LockHeld`] per listed lock, each the hold of the lock its key names,
/// and the content of [`FORK_SIGMASK`] its holder stored (`saved`),
/// with, in the release's hold, the slot's permission (`slot`).
/// Built, by convention, only by `fork_prepare` as it takes the locks
/// and by [`axiom_fork_resumed`] for the release: the fields are private
/// to `api` (its submodules can see them too), and [`arena_held`] and
/// [`count_held`] trust that the `n` and `arenas` of the bundle they are
/// handed are what `fork_prepare` read.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct ForkHeld {
    ghost n: int,
    ghost arenas: Map<int, Arena>,
    tracked toks: Map<ForkLock, LockHeld>,
    ghost saved: Option<u64>,
    tracked slot: Option<WordPerm>,
}

impl ForkHeld {
    /// The arena count `fork_prepare` read.
    pub closed spec fn n(self) -> int {
        self.n
    }

    /// The arenas `fork_prepare` found, by index.
    pub closed spec fn arenas(self) -> Map<int, Arena> {
        self.arenas
    }

    /// The keys held.
    pub closed spec fn keys(self) -> Set<ForkLock> {
        self.toks.dom()
    }

    /// The lock held under key `k`.
    pub closed spec fn lock(self, k: ForkLock) -> RawLock {
        self.toks[k].held()
    }

    /// The content of [`FORK_SIGMASK`] as this hold's holder last wrote
    /// it ([`save_sigmask`]), `None` before. Part of the hold because the
    /// slot is written and read only under every allocator lock.
    pub closed spec fn saved(self) -> Option<u64> {
        self.saved
    }

    /// The hold has the permission to [`FORK_SIGMASK`], and its value is
    /// the content the hold records: the release's hold, as
    /// [`axiom_fork_resumed`] hands it over (`fork_prepare` stores without
    /// one, under every lock).
    pub closed spec fn has_slot(self) -> bool {
        self.slot matches Some(p) && p.addr() == fork_slot_addr() && self.saved == Some(p.value())
    }

    /// The hold has no permission to [`FORK_SIGMASK`]: the prepare's hold,
    /// the only one [`save_sigmask`] writes under, so the slot is never
    /// written while its permission exists.
    pub closed spec fn no_slot(self) -> bool {
        self.slot is None
    }

    /// This hold after [`save_sigmask`] stored `m` in the slot.
    pub closed spec fn with_saved(self, m: u64) -> ForkHeld {
        ForkHeld { saved: Some(m), ..self }
    }

    /// Storing in the slot changes neither the locks held nor the arenas.
    proof fn lemma_with_saved(self, s: Shared, m: u64)
        ensures
            self.wf(s) ==> self.with_saved(m).wf(s),
            self.all() ==> self.with_saved(m).all(),
            self.with_saved(m).saved() == Some(m),
    {
        let t = self.with_saved(m);
        assert(t.keys() == self.keys());
        assert(t.arenas() == self.arenas());
        assert(t.n() == self.n());
        assert forall|k: ForkLock| #[trigger] t.lock(k) == self.lock(k) by {}
    }

    /// Add the hold `t` under key `k`.
    proof fn put(tracked &mut self, k: ForkLock, tracked t: LockHeld)
        ensures
            final(self).keys() == old(self).keys().insert(k),
            final(self).lock(k) == t.held(),
            forall|j: ForkLock| j != k ==> #[trigger] final(self).lock(j) == old(self).lock(j),
            final(self).n() == old(self).n(),
            final(self).arenas() == old(self).arenas(),
            final(self).saved() == old(self).saved(),
            final(self).no_slot() == old(self).no_slot(),
    {
        self.toks.tracked_insert(k, t);
    }

    /// Take the hold under key `k` out, to release it.
    proof fn take(tracked &mut self, k: ForkLock) -> (tracked t: LockHeld)
        requires
            old(self).keys().contains(k),
        ensures
            t.held() == old(self).lock(k),
            final(self).keys() == old(self).keys().remove(k),
            forall|j: ForkLock| j != k ==> #[trigger] final(self).lock(j) == old(self).lock(j),
            final(self).n() == old(self).n(),
            final(self).arenas() == old(self).arenas(),
            final(self).saved() == old(self).saved(),
            final(self).no_slot() == old(self).no_slot(),
    {
        self.toks.tracked_remove(k)
    }

    /// `n` is a count `fork_prepare` read (a `u32`), each token is the hold
    /// of the lock its key names in `s`, and the arenas are indices below
    /// `n`.
    pub(crate) open spec fn wf(self, s: Shared) -> bool {
        &&& 0 <= self.n() <= u32::MAX
        &&& forall|i: int| #[trigger] self.arenas().contains_key(i) ==> 0 <= i < self.n()
        &&& forall|k: ForkLock|
            #![trigger self.keys().contains(k)]
            #![trigger self.lock(k)]
            self.keys().contains(k) ==> self.lock(k) == lock_at(s, self.arenas(), k)
    }

    /// The keys held are exactly the fork lock list for the arenas found.
    pub open spec fn all(self) -> bool {
        forall|k: ForkLock| #[trigger] self.keys().contains(k) <==> listed(k, self.arenas().dom())
    }
}

/// Fork safety's hold: `held` holds every lock of the fork lock list of
/// `s` ([`listed`], for the arenas it found) and nothing else, and
/// `state` the background thread's state lock. `fork_prepare` ends with
/// it, and the release starts from it.
pub(crate) open spec fn fork_holds(s: Shared, held: ForkHeld, state: LockHeld) -> bool {
    &&& held.wf(s)
    &&& held.all()
    &&& state.held() == purge::background::state_lock()
}

/// Premise (the fork handler handoff): the parent and child handlers
/// run on the thread whose prepare handler returned holding
/// [`fork_holds`] (glibc runs them on the forking thread, after
/// `_Fork`; in the child that thread is the copy of the forking one),
/// and nothing released a lock in between: signals are blocked from the
/// prepare handler's first step, and the atfork handlers registered
/// before ours (whose prepare runs after ours and whose parent and
/// child handlers run before ours) do not take an allocator lock. Ghost
/// state does not cross the C calls, so the hold is handed over here:
/// the state lock's token, then [`axiom_fork_resumed`] for the rest, at
/// the `&Shared` the release reads (the same object: `shared` returns
/// the one static). Each handler calls them once, which nothing
/// enforces: a second call would hand out a second token for a held
/// lock, so no lemma may claim that two tokens of one lock are apart,
/// and two tokens would also let `Lock::get_unlocked` hand out two
/// `&mut` to one value, which Verus would take as independent.
axiom fn axiom_fork_resumed_state() -> (tracked state: LockHeld)
    ensures
        state.held() == purge::background::state_lock(),
;

/// Premise: see [`axiom_fork_resumed_state`]; the rest of the hold, at
/// the `Shared` the release reads, with the arena count and the arenas
/// `fork_prepare` read and the mask it saved in [`FORK_SIGMASK`] (`n`,
/// `arenas` and `saved` are those reads and that store, which the
/// ensures does not name: the premise is that they are; the slot's
/// content is a mask the kernel held, and only a holder of every lock
/// writes it, see [`save_sigmask`]), and the permission to the slot
/// whose value is that content ([`ForkHeld::has_slot`]): the one
/// trusted link between the slot and the hold, through which
/// [`saved_sigmask`] reads (no other permission to the slot exists).
/// `state` must be
/// the state lock's token: a fact about a token the caller already has is
/// a requirement, never a conclusion (ensured, it let a token of any other
/// lock prove `false`).
axiom fn axiom_fork_resumed(s: &Shared, tracked state: &LockHeld) -> (tracked held: ForkHeld)
    requires
        state.held() == purge::background::state_lock(),
    ensures
        fork_holds(*s, held, *state),
        held.has_slot(),
        held.saved() matches Some(m) && effective(m) == m,
;

/// The signal half of the fork hold: `sig` (the calling thread's mask)
/// has every blockable signal blocked, and the slot holds `orig`
/// (`held.saved()`), a mask the kernel can hold. `fork_prepare` ends with
/// it, `orig` being the mask in force at its entry, and the release
/// starts from it (theorem "Fork handlers restore the signal mask",
/// `verus/THEOREMS.md`).
pub(crate) open spec fn fork_sig_holds(sig: SigMask, held: ForkHeld, orig: u64) -> bool {
    &&& sig.wf()
    &&& sig.mask() == all_blocked()
    &&& held.saved() == Some(orig)
    &&& effective(orig) == orig
}

/// Premise (the fork handler handoff, its signal half; see
/// [`axiom_fork_resumed_state`]): the parent and child handlers run on
/// the thread whose prepare handler returned holding [`fork_sig_holds`]
/// (in the child, its copy, which inherits the mask), whose mask has
/// stayed as that handler left it, every blockable signal blocked:
/// glibc's `_Fork` restores the mask it found, and the atfork handlers
/// registered before ours (whose prepare runs after ours and whose parent
/// and child handlers run before ours) leave the mask as they found it.
/// The slot's content is [`axiom_fork_resumed`]'s. Each handler calls
/// this once, which nothing enforces.
axiom fn axiom_fork_resumed_sig() -> (tracked sig: SigMask)
    ensures
        sig.mask() == all_blocked(),
;

/// `s.arenas.get_existing(i as u16)` while `held` holds the creation
/// lock: the arena `fork_prepare` found at `i`. Trusted (the premise of
/// a frozen arena table): `Arenas::create` and `Arenas::build` write the
/// count and the table only under the creation lock, which this thread
/// has held since `fork_prepare` read them, so a read now returns what
/// that read did (an arena, once published, is never freed or moved).
/// The requirements are ghost; `held` must be `fork_prepare`'s bundle
/// or the resumed one (see [`ForkHeld`]).
#[verifier::external_body]
#[inline(always)]
fn arena_held<'a>(s: &'a Shared, i: usize, Tracked(held): Tracked<&ForkHeld>) -> (r: Option<&'a Arena>)
    requires
        held.keys().contains(ForkLock::Arenas),
        held.lock(ForkLock::Arenas) == s.spec_fork_locks().1,
        i < held.n(),
    ensures
        r is Some <==> held.arenas().contains_key(i as int),
        r matches Some(a) ==> *a == held.arenas()[i as int],
{
    s.arenas.get_existing(i as u16)
}

/// `s.arenas.count()` while `held` holds the creation lock: the count
/// `fork_prepare` read. Trusted, as [`arena_held`]. The bundle's `n` is
/// a `u32` (required, and part of [`ForkHeld::wf`]): the result is one.
#[verifier::external_body]
#[inline(always)]
fn count_held(s: &Shared, Tracked(held): Tracked<&ForkHeld>) -> (r: u32)
    requires
        0 <= held.n() <= u32::MAX,
        held.keys().contains(ForkLock::Arenas),
        held.lock(ForkLock::Arenas) == s.spec_fork_locks().1,
    ensures
        r as int == held.n(),
{
    s.arenas.count()
}

/// Block signals, then take every allocator lock so the child inherits a
/// consistent heap and no signal handler on this thread can re-enter the
/// allocator (or fork again) while the locks are held.
///
/// The order is the nesting order (design section 9): the explicit-cache
/// table lock is held across heap creation and destruction, which take the
/// arena creation lock, class locks and the metadata lock; the statistics
/// locks are leaves (`stats.rs` takes each with nothing else held) and
/// the background thread's state lock comes last. The test
/// `fork_lock_list_matches_the_source_tree` derives the list of locks
/// from the source tree, so a new `Lock` fails it until it is added here.
///
/// Fork safety, first half: it returns holding [`fork_holds`], one token
/// per lock of the fork lock list (asserted at the end). It also ends
/// with [`fork_sig_holds`]: every blockable signal blocked since before
/// its first acquisition (each loop's invariant), and the mask in force
/// at its entry saved in the slot (theorem "Fork handlers restore the
/// signal mask").
///
/// Lock order (theorem "Lock order is acyclic", `verus/THEOREMS.md`):
/// every acquisition ranks above every lock taken before it
/// ([`crate::lock::LockOrder`]), except each arena's slab-set lock. At
/// run time that lock is taken under a class lock and the class's slab
/// table lock (`SlabTable::take_group`), so it ranks above the table
/// locks, which are taken here after it. It is a gated lock
/// ([`crate::lock::gated`]): taken here with all of its arena's class
/// locks held, no other thread can hold it or wait for it.
extern "C" fn fork_prepare() {
    proof {
        crate::sys::grant_syscalls();
        crate::sys::grant_aborts();
    }
    // Premise: `sig` is this thread's mask (`sys::axiom_thread_mask`).
    let tracked mut sig = sys::axiom_thread_mask();
    let ghost orig = sig.mask();
    let mask = sys::block_all_signals(Tracked(&mut sig));
    let s = shared();
    // Premise (lock order): the thread that calls `fork` holds no
    // allocator lock (no allocator code forks, and the prepare handlers
    // that run before ours have returned).
    let tracked mut ord = axiom_entry();
    let Tracked(t) = s.tcaches_lock().lock_held(Tracked(&mut ord));
    let tracked mut toks: Map<ForkLock, LockHeld> = Map::tracked_empty();
    proof {
        toks.tracked_insert(ForkLock::Tcaches, t);
    }
    let Tracked(t) = s.arenas_lock().lock_held(Tracked(&mut ord));
    proof {
        toks.tracked_insert(ForkLock::Arenas, t);
    }
    let ghost mut arenas: Map<int, Arena> = Map::empty();
    // Each `for a in s.arenas.iter()` is spelled as the scan `Arenas::iter`
    // makes, through `Arenas::get_existing` (Verus does not specify the
    // iterator), and `a.classes[c]` as `Arena::class`, as in
    // `fork_release`.
    let n = s.arenas.count() as usize;
    for i in 0..n
        invariant
            crate::sys::may_syscall(),
            sig.wf(),
            sig.mask() == all_blocked(),
            n <= crate::arena::MAX_ARENAS,
            forall|j: int| #[trigger] arenas.contains_key(j) ==> 0 <= j < i,
            forall|j: int| #[trigger] arenas.contains_key(j) ==> arenas[j].spec_index() as int == j,
            forall|k: ForkLock| #[trigger] toks.contains_key(k) <==> (k == ForkLock::Tcaches || k
                == ForkLock::Arenas || arena_part(k, arenas.dom())),
            forall|k: ForkLock| #[trigger]
                toks.contains_key(k) ==> toks[k].held() == lock_at(*s, arenas, k),
            ord.root(),
            ord.top() < rank_class(i as int, 0),
    {
        if let Some(a) = s.arenas.get_existing(i as u16) {
            let ghost before = arenas;
            proof {
                arenas = arenas.insert(i as int, *a);
            }
            for c in 0..NCLASSES
                invariant
                    crate::sys::may_syscall(),
                    sig.wf(),
                    sig.mask() == all_blocked(),
                    arenas == before.insert(i as int, *a),
                    a.spec_index() as int == i,
                    i < crate::arena::MAX_ARENAS,
                    forall|j: int| #[trigger] before.contains_key(j) ==> 0 <= j < i,
                    forall|k: ForkLock| #[trigger] toks.contains_key(k) <==> (k == ForkLock::Tcaches
                        || k == ForkLock::Arenas || arena_part(k, before.dom()) || (
                    k matches ForkLock::Class(j, d) && j == i && 0 <= d < c)),
                    forall|k: ForkLock| #[trigger]
                        toks.contains_key(k) ==> toks[k].held() == lock_at(*s, arenas, k),
                    forall|d: int| 0 <= d < c ==> #[trigger] toks[ForkLock::Class(i as int, d)].held().rank() == rank_class(i as int, d),
                    ord.root(),
                    ord.top() < rank_class(i as int, c as int),
            {
                let Tracked(t) = a.class(c).raw_lock().lock_held(Tracked(&mut ord));
                proof {
                    toks.tracked_insert(ForkLock::Class(i as int, c as int), t);
                }
            }
            // The slab-set lock is gated by this arena's class locks, all
            // held now: no order is checked, and `ord` stays at the last
            // class lock (`lock::gated`).
            let ghost key = |c: int| ForkLock::Class(i as int, c);
            proof {
                assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] toks.contains_key(key(c))
                    && toks[key(c)].held().rank() == rank_class(i as int, c) by {
                    assert(toks.contains_key(key(c)));
                }
            }
            let Tracked(t) = a.sets().raw_lock().lock_held_gated::<ForkLock>(Tracked(&toks), Ghost(key));
            proof {
                toks.tracked_insert(ForkLock::Sets(i as int), t);
                assert forall|k: ForkLock| #[trigger] toks.contains_key(k) <==> (k
                    == ForkLock::Tcaches || k == ForkLock::Arenas || arena_part(k, arenas.dom())) by {
                    if let ForkLock::Class(j, d) = k {
                        if j == i as int {
                            assert(arena_part(k, arenas.dom()) <==> 0 <= d < NCLASSES);
                        }
                    }
                }
            }
        }
    }
    let tracked mut held = ForkHeld { n: n as int, arenas, toks, saved: None, slot: None };
    for c in 0..NCLASSES
        invariant
            crate::sys::may_syscall(),
            sig.wf(),
            sig.mask() == all_blocked(),
            held.wf(*s),
            held.no_slot(),
            held.n() <= crate::arena::MAX_ARENAS,
            forall|j: int| #[trigger] held.arenas().contains_key(j) ==> held.arenas()[j].spec_index() as int == j,
            forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || arena_part(k, held.arenas().dom())
                        || (k matches ForkLock::Table(d) && 0 <= d < c)
            ),
            ord.root(),
            ord.top() < rank_table(c as int),
            s.wf(),
    {
        let Tracked(t) = s.tables()[c].raw_lock().lock_held(Tracked(&mut ord));
        proof {
            held.put(ForkLock::Table(c as int), t);
        }
    }
    let Tracked(t) = s.huge_lock().lock_held(Tracked(&mut ord));
    proof {
        held.put(ForkLock::Huge, t);
    }
    let Tracked(t) = s.meta_lock().lock_held(Tracked(&mut ord));
    proof {
        held.put(ForkLock::Meta, t);
    }
    let Tracked(t) = s.stats_lock().lock_held(Tracked(&mut ord));
    proof {
        held.put(ForkLock::Stats, t);
    }
    let n = count_held(s, Tracked(&held)) as usize;
    for i in 0..n
        invariant
            crate::sys::may_syscall(),
            sig.wf(),
            sig.mask() == all_blocked(),
            n == held.n(),
            held.wf(*s),
            held.no_slot(),
            n <= crate::arena::MAX_ARENAS,
            forall|j: int| #[trigger] held.arenas().contains_key(j) ==> held.arenas()[j].spec_index() as int == j,
            forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || arena_part(k, held.arenas().dom())
                        || table_part(k) || k == ForkLock::Huge || k == ForkLock::Meta || k
                        == ForkLock::Stats || (k matches ForkLock::ArenaStats(j) && j < i
                        && held.arenas().contains_key(j))
            ),
            ord.root(),
            ord.top() < rank_arena_stats(i as int),
    {
        if let Some(a) = arena_held(s, i, Tracked(&held)) {
            let Tracked(t) = a.stats_raw_lock().lock_held(Tracked(&mut ord));
            proof {
                held.put(ForkLock::ArenaStats(i as int), t);
            }
        }
    }
    let Tracked(state) = purge::background::fork_lock(Tracked(&mut ord));
    // Every allocator lock is held, so no other forking thread can touch
    // the slot until `fork_release` has read it (see `FORK_SIGMASK`).
    let ghost before = held;
    save_sigmask(mask, Ghost(*s), Tracked(&mut held), Tracked(&state));
    proof {
        before.lemma_with_saved(*s, mask);
        assert(fork_holds(*s, held, state));
        assert(fork_sig_holds(sig, held, orig));
    }
}

/// Save the signal mask for `fork_release`, recorded in the hold
/// (`held.saved()`). Its requirement is the [`FORK_SIGMASK`] protocol:
/// the caller holds every allocator lock (ghost), so the hold that
/// records the content is the one exclusive hold of the slot; and the
/// hold has no permission to the slot ([`ForkHeld::no_slot`]), so no
/// permission goes stale.
#[inline(always)]
fn save_sigmask(
    mask: u64,
    Ghost(s): Ghost<Shared>,
    Tracked(held): Tracked<&mut ForkHeld>,
    Tracked(state): Tracked<&LockHeld>,
)
    requires
        fork_holds(s, *old(held), *state),
        old(held).no_slot(),
    ensures
        *final(held) == old(held).with_saved(mask),
{
    fork_slot().store(mask, Ordering::Relaxed);
    proof {
        held.saved = Some(mask);
    }
}

/// The signal mask `fork_prepare` saved, the content the hold records:
/// read through the hold's permission to [`FORK_SIGMASK`]
/// ([`ForkHeld::has_slot`]), which only a holder of every allocator lock
/// has (required, ghost). The value is still the one this thread's
/// prepare handler saved: [`save_sigmask`] requires that hold too, so a
/// second thread's `fork_prepare` (glibc serialises the handlers, the
/// unit tests do not) stores only after it has taken every lock, so
/// after this release's unlocks.
#[inline(always)]
fn saved_sigmask(
    Ghost(s): Ghost<Shared>,
    Tracked(held): Tracked<&ForkHeld>,
    Tracked(state): Tracked<&LockHeld>,
) -> (r: u64)
    requires
        fork_holds(s, *held, *state),
        held.has_slot(),
    ensures
        held.saved() == Some(r),
{
    let tracked slot = held.slot.tracked_borrow();
    crate::vspec::word::load(fork_slot(), Ordering::Relaxed, Tracked(slot))
}

/// Release the locks taken by `fork_prepare`, in reverse order, then let
/// signals through again.
///
/// Fork safety, second half: it starts from the hold `fork_prepare`
/// ended with ([`fork_holds`]; `state` is the state lock's token) and
/// releases each listed lock once, by handing its token back to
/// `RawLock::unlock_held`; it ends holding none (asserted at the end).
///
/// The signal mask (theorem "Fork handlers restore the signal mask"): it
/// starts from [`fork_sig_holds`], reads the slot under every lock, keeps
/// every blockable signal blocked through its last unlock, and ends with
/// the thread's mask the slot's content (asserted at the end).
fn fork_release(Tracked(state): Tracked<LockHeld>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        state.held() == purge::background::state_lock(),
{
    let s = shared();
    let tracked mut held = axiom_fork_resumed(s, &state);
    let tracked mut sig = axiom_fork_resumed_sig();
    let ghost saved = held.saved()->Some_0;
    proof {
        crate::sys::lemma_effective_idempotent(!0u64);
        assert(fork_sig_holds(sig, held, saved));
    }
    // Written by the `fork_prepare` that paired with this release, and
    // still ours because every lock is held until the unlocks below.
    let mask = saved_sigmask(Ghost(*s), Tracked(&held), Tracked(&state));
    purge::background::fork_unlock(Tracked(state));
    // Arenas cannot appear while the creation lock is held, so this is the
    // same set `fork_prepare` locked (the premise `arena_held` and
    // `count_held` state), in reverse. Each `for i in
    // (0..n).rev()` is spelled as a countdown: Verus does not specify
    // `Rev`.
    // The class states are read through `Arena::class`, the verified
    // accessor of `a.classes[c]`.
    let n = count_held(s, Tracked(&held)) as usize;
    let mut i = n;
    while i > 0
        invariant
            crate::sys::may_syscall(),
            sig.wf(),
            sig.mask() == all_blocked(),
            i <= n == held.n(),
            held.wf(*s),
            forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || arena_part(k, held.arenas().dom())
                        || table_part(k) || k == ForkLock::Huge || k == ForkLock::Meta || k
                        == ForkLock::Stats || (k matches ForkLock::ArenaStats(j) && j < i
                        && held.arenas().contains_key(j))
            ),
        decreases i,
    {
        i -= 1;
        if let Some(a) = arena_held(s, i, Tracked(&held)) {
            let tracked t = held.take(ForkLock::ArenaStats(i as int));
            a.stats_raw_lock().unlock_held(Tracked(t));
        }
    }
    let tracked t = held.take(ForkLock::Stats);
    s.stats_lock().unlock_held(Tracked(t));
    let tracked t = held.take(ForkLock::Meta);
    s.meta_lock().unlock_held(Tracked(t));
    let tracked t = held.take(ForkLock::Huge);
    s.huge_lock().unlock_held(Tracked(t));
    let mut c = NCLASSES;
    while c > 0
        invariant
            crate::sys::may_syscall(),
            sig.wf(),
            sig.mask() == all_blocked(),
            c <= NCLASSES,
            n == held.n(),
            held.wf(*s),
            forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || arena_part(k, held.arenas().dom())
                        || (k matches ForkLock::Table(d) && 0 <= d < c)
            ),
        decreases c,
    {
        c -= 1;
        let tracked t = held.take(ForkLock::Table(c as int));
        s.tables()[c].raw_lock().unlock_held(Tracked(t));
    }
    let mut i = n;
    while i > 0
        invariant
            crate::sys::may_syscall(),
            sig.wf(),
            sig.mask() == all_blocked(),
            i <= n == held.n(),
            held.wf(*s),
            forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || arena_part(
                        k,
                        held.arenas().dom().filter(|j: int| j < i),
                    )
            ),
        decreases i,
    {
        i -= 1;
        if let Some(a) = arena_held(s, i, Tracked(&held)) {
            let tracked t = held.take(ForkLock::Sets(i as int));
            a.sets().raw_lock().unlock_held(Tracked(t));
            let mut c = NCLASSES;
            while c > 0
                invariant
                    crate::sys::may_syscall(),
                    sig.wf(),
                    sig.mask() == all_blocked(),
                    c <= NCLASSES,
                    i < n == held.n(),
                    held.arenas().contains_key(i as int),
                    *a == held.arenas()[i as int],
                    held.wf(*s),
                    forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                            k == ForkLock::Tcaches || k == ForkLock::Arenas || arena_part(
                                k,
                                held.arenas().dom().filter(|j: int| j < i),
                            ) || (k matches ForkLock::Class(j, d) && j == i && 0 <= d < c)
                    ),
                decreases c,
            {
                c -= 1;
                let tracked t = held.take(ForkLock::Class(i as int, c as int));
                a.class(c).raw_lock().unlock_held(Tracked(t));
            }
        }
    }
    let tracked t = held.take(ForkLock::Arenas);
    s.arenas_lock().unlock_held(Tracked(t));
    let tracked t = held.take(ForkLock::Tcaches);
    s.tcaches_lock().unlock_held(Tracked(t));
    proof {
        assert(held.keys() =~= Set::empty());
    }
    sys::set_signal_mask(mask, Tracked(&mut sig));
    proof {
        assert(sig.mask() == saved);
    }
}

/// The parent keeps its threads; the forking thread releases what its
/// prepare handler took.
extern "C" fn fork_parent() {
    proof {
        crate::sys::grant_syscalls();
        crate::sys::grant_aborts();
    }
    let tracked state = axiom_fork_resumed_state();
    fork_release(Tracked(state));
}

/// The child has one thread; the locks were held by the forking thread, so
/// releasing them is exactly right. Before that, with everything still
/// locked, the background thread is marked absent (design 8.1).
extern "C" fn fork_child() {
    proof {
        crate::sys::grant_syscalls();
        crate::sys::grant_aborts();
    }
    let tracked mut state = axiom_fork_resumed_state();
    purge::background::fork_child(Tracked(&mut state));
    fork_release(Tracked(state));
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::String;
    use std::thread;
    use std::vec::Vec;

    #[test]
    fn init_is_idempotent_and_concurrent() {
        let handles: Vec<_> = (0..16)
            .map(|_| thread::spawn(|| shared() as *const Shared as usize))
            .collect();
        let first = shared() as *const Shared as usize;
        for h in handles {
            assert_eq!(h.join().unwrap(), first);
        }
        assert!(is_ready());
        assert!(shared_if_ready().is_some());
        assert!(shared().has_thread_key());
        let f = features();
        assert_eq!(f.page_size, shared().regions.page_size());
    }

    /// `challoc_features()` is the `Display` of the probe, NUL-terminated,
    /// static, and the same bytes on every call.
    #[test]
    fn challoc_features_is_the_probe_line() {
        let p = challoc_features();
        assert!(!p.is_null());
        assert_eq!(p, challoc_features());
        let s = unsafe { core::ffi::CStr::from_ptr(p) }.to_str().unwrap();
        assert_eq!(s, std::format!("{}", features()));
        assert_eq!(s, feature_line());
        assert!(s.starts_with("page_size="), "{s}");
        assert!(s.ends_with(" pac=yes") || s.ends_with(" pac=no"), "{s}");
        assert!(!s.contains('\n'));
        for kv in s.split(' ') {
            let (k, v) = kv
                .split_once('=')
                .unwrap_or_else(|| panic!("`{kv}` in {s}"));
            assert!(!k.is_empty() && !v.is_empty(), "`{kv}` in {s}");
        }
    }

    /// The handler tests drive the process-wide locks directly and the
    /// harness runs tests in parallel, so they take turns.
    static FORK_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Every `Lock<..>` (or bare `RawLock`) field and static in `src/`, as
    /// `file:Owner.field` (`file:static.NAME` for statics), read from the
    /// source tree so the fork handlers cannot miss a lock added later.
    /// `lock.rs` defines the types and is skipped; so are the modules
    /// declared under `#[cfg(test)]` and each file's `mod tests`.
    fn lock_inventory() -> Vec<String> {
        use std::string::ToString;
        /// Every `.rs` file under `dir` as (path relative to `src/`, text).
        fn sources(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, String)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            entries.sort();
            for path in entries {
                let name = path.file_name().unwrap().to_str().unwrap().to_string();
                let rel = if rel.is_empty() {
                    name
                } else {
                    std::format!("{rel}/{name}")
                };
                if path.is_dir() {
                    sources(&path, &rel, out);
                } else if rel.ends_with(".rs") {
                    out.push((rel, std::fs::read_to_string(&path).unwrap()));
                }
            }
        }
        /// The module paths (relative to `src/`, without `.rs`) that `rel`
        /// declares as `#[cfg(test)] mod name;`.
        fn test_modules(rel: &str, text: &str, out: &mut Vec<String>) {
            let stem = rel.strip_suffix(".rs").unwrap();
            let dir = match stem.rsplit_once('/') {
                Some((d, "mod")) => d.to_string(),
                None if stem == "lib" || stem == "mod" => String::new(),
                _ => stem.to_string(),
            };
            let mut cfg_test = false;
            for line in text.lines() {
                let t = line.trim();
                let decl = t
                    .strip_prefix("pub(crate) ")
                    .or_else(|| t.strip_prefix("pub "))
                    .unwrap_or(t);
                if let (true, Some(name)) = (
                    cfg_test,
                    decl.strip_prefix("mod ").and_then(|m| m.strip_suffix(';')),
                ) {
                    out.push(if dir.is_empty() {
                        name.to_string()
                    } else {
                        std::format!("{dir}/{name}")
                    });
                }
                cfg_test = t == "#[cfg(test)]";
            }
        }
        let mut files = Vec::new();
        sources(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            "",
            &mut files,
        );
        let mut test_only = Vec::new();
        for (rel, text) in &files {
            test_modules(rel, text, &mut test_only);
        }
        let mut out = Vec::new();
        for (rel, text) in &files {
            let stem = rel.strip_suffix(".rs").unwrap();
            if rel == "lock.rs"
                || test_only
                    .iter()
                    .any(|m| stem == m || rel.starts_with(&std::format!("{m}/")))
            {
                continue;
            }
            let mut owner = String::new();
            for line in text.lines() {
                let t = line.trim();
                if t == "mod tests {" {
                    break;
                }
                if t.starts_with("//")
                    || t.starts_with("use ")
                    || t.contains(" fn ")
                    || t.starts_with("fn ")
                {
                    continue;
                }
                let decl = t
                    .strip_prefix("pub(crate) ")
                    .or_else(|| t.strip_prefix("pub "))
                    .unwrap_or(t);
                if let Some(rest) = decl.strip_prefix("struct ") {
                    owner = rest
                        .split(['<', '{', '(', ' ', ';'])
                        .next()
                        .unwrap()
                        .to_string();
                    continue;
                }
                let is_lock = |ty: &str| ty.contains("Lock<") || ty.contains("RawLock");
                // `exec static` is how `verus!` spells a static.
                if let Some(rest) = decl
                    .strip_prefix("static ")
                    .or_else(|| decl.strip_prefix("exec static "))
                {
                    if let Some((name, ty)) = rest.split_once(':') {
                        if is_lock(ty) {
                            out.push(std::format!("{rel}:static.{}", name.trim()));
                        }
                    }
                    continue;
                }
                if let Some((name, ty)) = decl.split_once(':') {
                    let name = name.trim();
                    if !name.is_empty()
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                        && is_lock(ty)
                        && !ty.contains("->")
                    {
                        out.push(std::format!("{rel}:{owner}.{name}"));
                    }
                }
            }
        }
        out.sort();
        out
    }

    /// The locks `fork_prepare` takes, keyed as in [`lock_inventory`].
    /// Adding a lock to the crate means adding it here *and* to the
    /// handlers; `assert_locks` refuses a key it has no assertion for.
    const FORK_LOCKS: &[&str] = &[
        "arena.rs:Arena.stats",
        "arena.rs:Arenas.lock",
        "heap/tcaches.rs:Tcaches.lock",
        "class/mod.rs:ClassState.inner",
        "class/sets.rs:SetTable.tables",
        "class/table.rs:SlabTable.inner",
        "heap/shared.rs:Shared.stats",
        "huge/mod.rs:Huge.inner",
        "meta.rs:Meta.inner",
        "purge/background.rs:static.STATE",
    ];

    /// Assert that every lock in [`FORK_LOCKS`] is held (`locked`) or free
    /// (`!locked`), for the arenas in `arenas`.
    fn assert_locks(s: &Shared, arenas: &[u16], locked: bool) {
        let state = |held: bool, what: &str| {
            assert_eq!(
                held,
                locked,
                "{what} {}",
                if locked { "not locked" } else { "still locked" }
            );
        };
        for key in FORK_LOCKS {
            match *key {
                "arena.rs:Arena.stats" => {
                    for &a in arenas {
                        state(
                            s.arena(a, Tracked::assume_new())
                                .unwrap()
                                .stats_raw_lock()
                                .is_locked(),
                            &std::format!("arena {a} stats"),
                        );
                    }
                }
                "arena.rs:Arenas.lock" => state(s.arenas.raw_lock().is_locked(), "arena registry"),
                "heap/tcaches.rs:Tcaches.lock" => {
                    state(s.tcaches.raw_lock().is_locked(), "tcache table")
                }
                "class/mod.rs:ClassState.inner" => {
                    for &a in arenas {
                        for c in 0..NCLASSES {
                            state(
                                s.arena(a, Tracked::assume_new()).unwrap().classes[c]
                                    .raw_lock()
                                    .is_locked(),
                                &std::format!("arena {a} class {c}"),
                            );
                        }
                    }
                }
                "class/sets.rs:SetTable.tables" => {
                    for &a in arenas {
                        state(
                            s.arena(a, Tracked::assume_new())
                                .unwrap()
                                .sets()
                                .raw_lock()
                                .is_locked(),
                            &std::format!("arena {a} sets"),
                        );
                    }
                }
                "class/table.rs:SlabTable.inner" => {
                    for c in 0..NCLASSES {
                        state(
                            s.tables()[c].raw_lock().is_locked(),
                            &std::format!("table {c}"),
                        );
                    }
                }
                "heap/shared.rs:Shared.stats" => state(s.stats.raw().is_locked(), "process stats"),
                "huge/mod.rs:Huge.inner" => state(s.huge.raw_lock().is_locked(), "huge"),
                "meta.rs:Meta.inner" => state(s.meta.raw_lock().is_locked(), "meta"),
                "purge/background.rs:static.STATE" => {
                    state(purge::background::is_locked(), "background state")
                }
                other => panic!("no fork-handler assertion for lock {other}"),
            }
        }
    }

    /// The set of locks the handlers cover is exactly the set of locks in
    /// the crate (design 9).
    #[test]
    fn fork_lock_list_matches_the_source_tree() {
        let found = lock_inventory();
        let mut want: Vec<String> = FORK_LOCKS
            .iter()
            .map(|k| std::string::ToString::to_string(k))
            .collect();
        want.sort();
        assert_eq!(
            found, want,
            "locks in src/ differ from the fork handlers' list: add every new lock to \
             fork_prepare/fork_release in nesting order, to FORK_LOCKS and to assert_locks"
        );
    }

    #[test]
    fn fork_handlers_take_and_release_every_lock() {
        let _serial = FORK_TEST.lock().unwrap();
        let s = shared();
        // A second arena so the handlers have more than arena 0 to cover.
        let extra = s.arenas.create(s, Tracked::assume_new()).unwrap();
        let arenas = [0, extra];
        fork_prepare();
        assert_locks(s, &arenas, true);
        fork_parent();
        assert_locks(s, &arenas, false);
        fork_prepare();
        assert_locks(s, &arenas, true);
        fork_child();
        assert_locks(s, &arenas, false);
        assert!(!purge::background::present());
        assert!(!purge::background::requested());
    }

    /// The background thread starts on request, is visible under
    /// `/proc/self/task` with its name, purges without any slow-path
    /// tick, is gone from the bookkeeping in a fork child while the
    /// parent's keeps running, and stops when the request is withdrawn.
    #[test]
    fn background_thread_starts_purges_and_is_absent_after_fork() {
        use std::string::ToString;
        fn tasks() -> std::vec::Vec<std::string::String> {
            let mut names = std::vec::Vec::new();
            for e in std::fs::read_dir("/proc/self/task").unwrap() {
                let p = e.unwrap().path().join("comm");
                names.push(
                    std::fs::read_to_string(p)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                );
            }
            names
        }
        let _serial = FORK_TEST.lock().unwrap();
        let s = shared();
        assert!(!tasks().iter().any(|n| n == "challoc-bg"));
        purge::background::request(true);
        assert!(
            purge::background::start(s, Tracked::assume_new()),
            "background thread did not start"
        );
        assert!(purge::background::present());
        assert!(
            purge::background::start(s, Tracked::assume_new()),
            "second start is idempotent"
        );
        // The name is set by the thread itself; give it a moment.
        let mut seen = false;
        for _ in 0..200 {
            if tasks().iter().any(|n| n == "challoc-bg") {
                seen = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(seen, "no task named challoc-bg: {:?}", tasks());

        // A 1 ms decay: free a huge run, wait, and the thread purges it
        // with nobody else touching the allocator.
        let old_ns = s.decay_ns();
        s.set_decay_ms(1);
        let (p, _, _) = s.alloc_huge(
            8 << 20,
            4096,
            0,
            vstd::prelude::Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert!(!p.is_null());
        unsafe { core::ptr::write_bytes(p, 1, 8 << 20) };
        let page = (p as usize - s.regions.huge_base) >> s.regions.lg_page;
        s.free_huge(
            page,
            vstd::prelude::Tracked::assume_new(),
            Tracked::assume_new(),
        );
        // No check that the run is still `Empty` here: with a 1 ms decay
        // the thread can purge it before this thread runs again (seen on
        // a loaded Docker VM); the loop below is the assertion.
        let mut purged = false;
        for _ in 0..400 {
            if s.huge.oldest_empty() == 0 && s.huge.stats(Tracked::assume_new()).empty == 0 {
                purged = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            purged,
            "background thread did not purge the freed run: {:?}",
            s.huge.stats(Tracked::assume_new())
        );
        s.set_decay_ms((old_ns / 1_000_000) as i64);

        // A real fork: the child handler forgets the thread and the
        // request in the child, and leaves this process's thread alone.
        // The child checks with atomics and the state lock only and
        // answers through its exit status.
        assert!(purge::background::thread().is_some(), "no thread record");
        let pid = unsafe { ::libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let bad = [
                purge::background::present(),
                purge::background::requested(),
                purge::background::is_locked(),
                purge::background::thread().is_some(),
            ];
            let code = bad
                .iter()
                .enumerate()
                .fold(0, |m, (i, &b)| m | ((b as i32) << i));
            unsafe { ::libc::_exit(code) };
        }
        let mut status = 0;
        assert_eq!(unsafe { ::libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(
            ::libc::WIFEXITED(status),
            "fork child did not exit: status {status:#x}"
        );
        assert_eq!(
            ::libc::WEXITSTATUS(status),
            0,
            "fork child still sees (bit 0) present, (1) requested, (2) the state lock held, (3) a thread record"
        );
        assert!(
            purge::background::present() && purge::background::requested(),
            "the fork stopped the parent's thread"
        );

        // Withdraw the request: the kick wakes the thread and it exits
        // there; the exit is asynchronous, so wait for it, bounded, and
        // join it.
        purge::background::request(false);
        let mut stopped = false;
        for _ in 0..400 {
            if !purge::background::present() {
                stopped = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            stopped,
            "background thread did not stop at the kick: {:?}",
            tasks()
        );
        assert!(purge::background::join_exited(), "no exited thread to join");
        assert!(purge::background::thread().is_none());
        // Joined, the task leaves /proc/self/task as the kernel reaps it.
        let mut gone = false;
        for _ in 0..400 {
            if !tasks().iter().any(|n| n == "challoc-bg") {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            gone,
            "background thread still running after the join: {:?}",
            tasks()
        );
    }

    /// A first `Empty` that lands while `start` is publishing the thread
    /// is purged: the thread has already scanned and gone to sleep with
    /// no deadline, and the free's `wake` does nothing because the
    /// thread is not yet present, so `start` must kick it once it is.
    /// Run alone, it fails without that kick; in the full suite another
    /// test's first `Empty` can wake the thread instead and hide it.
    #[test]
    fn background_thread_purges_an_empty_from_its_startup() {
        use core::sync::atomic::{AtomicU8, Ordering};
        use std::sync::Arc;
        let _serial = FORK_TEST.lock().unwrap();
        // A private allocator: the process-wide one is shared with the
        // tests that do not take `FORK_TEST`, whose huge frees could
        // leave a run `Empty` before the hook's.
        let s = crate::testutil::shared();
        assert!(!purge::background::present());
        let old_ns = s.decay_ns();
        s.set_decay_ms(1);
        let (p, _, _) = s.alloc_huge(
            8 << 20,
            4096,
            0,
            vstd::prelude::Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert!(!p.is_null());
        unsafe { core::ptr::write_bytes(p, 1, 8 << 20) };
        let page = (p as usize - s.regions.huge_base) >> s.regions.lg_page;
        // Nothing else `Empty` in the huge allocator, so the free in the
        // hook is its first.
        s.huge
            .purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new());
        // 0: the hook did not run; 1: the thread never slept without a
        // deadline; 2: the free was not the first `Empty`; 3: ready.
        let hook_state = Arc::new(AtomicU8::new(0));
        let hook_seen = Arc::clone(&hook_state);
        let waits = purge::background::UNTIMED_WAITS.load(Ordering::SeqCst);
        *purge::background::BEFORE_PUBLISH.lock().unwrap() =
            Some(std::boxed::Box::new(move || {
                // No panics here: unwinding out of `start` would leave the
                // state marked as creating for every later test.
                let mut asleep = false;
                for _ in 0..1000 {
                    if purge::background::UNTIMED_WAITS.load(Ordering::SeqCst) != waits {
                        asleep = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                if !asleep {
                    hook_seen.store(1, Ordering::SeqCst);
                    return;
                }
                let none_before = s.huge.oldest_empty() == 0;
                s.free_huge(
                    page,
                    vstd::prelude::Tracked::assume_new(),
                    Tracked::assume_new(),
                );
                let first = none_before && s.huge.oldest_empty() != 0;
                hook_seen.store(if first { 3 } else { 2 }, Ordering::SeqCst);
            }));
        purge::background::request(true);
        let started = purge::background::start(s, Tracked::assume_new());
        let mut purged = false;
        if started && hook_state.load(Ordering::SeqCst) == 3 {
            for _ in 0..400 {
                if s.huge.oldest_empty() == 0 && s.huge.stats(Tracked::assume_new()).empty == 0 {
                    purged = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        // Clean up before asserting, so a failure here leaves no thread
        // or hook behind for the other background tests.
        purge::background::BEFORE_PUBLISH.lock().unwrap().take();
        if hook_state.load(Ordering::SeqCst) < 2 {
            s.free_huge(
                page,
                vstd::prelude::Tracked::assume_new(),
                Tracked::assume_new(),
            );
        }
        s.set_decay_ms((old_ns / 1_000_000) as i64);
        purge::background::request(false);
        for _ in 0..400 {
            if !purge::background::present() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let stopped = !purge::background::present();
        if stopped {
            purge::background::join_exited();
        }
        assert!(started, "background thread did not start");
        assert_eq!(
            hook_state.load(Ordering::SeqCst),
            3,
            "hook: 0 did not run, 1 the thread never slept without a deadline, \
             2 the free was not the first Empty"
        );
        assert!(
            purged,
            "background thread slept through a first Empty from its startup: {:?}",
            s.huge.stats(Tracked::assume_new())
        );
        assert!(stopped, "background thread did not stop");
    }

    /// One stack for the life of the process (design 8.1): a
    /// stop/start cycle joins the exited thread and hands the same
    /// reservation to the next one, so toggling `background_thread` does
    /// not grow the metadata region.
    #[test]
    fn background_stack_is_reused_across_restarts() {
        fn wait_absent() {
            for _ in 0..400 {
                if !purge::background::present() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("background thread did not stop");
        }
        let _serial = FORK_TEST.lock().unwrap();
        let s = shared();
        assert!(!purge::background::present());
        purge::background::request(true);
        assert!(
            purge::background::start(s, Tracked::assume_new()),
            "background thread did not start"
        );
        let stack = purge::background::stack();
        assert_ne!(stack, 0);
        for cycle in 0..8 {
            purge::background::request(false);
            wait_absent();
            purge::background::request(true);
            assert!(
                purge::background::start(s, Tracked::assume_new()),
                "restart {cycle} failed"
            );
            assert!(purge::background::present());
            assert_eq!(
                purge::background::stack(),
                stack,
                "restart {cycle} took a new stack"
            );
        }
        purge::background::request(false);
        wait_absent();
        assert!(purge::background::join_exited());
        assert!(purge::background::thread().is_none());
    }

    /// The calling thread's kernel signal mask.
    fn current_mask() -> u64 {
        let mut set: ::libc::sigset_t = unsafe { core::mem::zeroed() };
        let rc = unsafe { ::libc::pthread_sigmask(::libc::SIG_BLOCK, core::ptr::null(), &mut set) };
        assert_eq!(rc, 0);
        // The kernel mask is the first 8 bytes of glibc's 128-byte set.
        unsafe { core::ptr::read_unaligned(&set as *const _ as *const u64) }
    }

    /// `mask` without `SIGKILL` and `SIGSTOP`, which the kernel never blocks.
    fn blockable(mask: u64) -> u64 {
        const KILL_STOP: u64 = (1 << (::libc::SIGKILL - 1)) | (1 << (::libc::SIGSTOP - 1));
        mask & !KILL_STOP
    }

    /// The handlers block every signal while the locks are held and restore
    /// the caller's mask (whatever it was) afterwards, on both fork sides.
    #[test]
    fn fork_handlers_block_signals_while_locked() {
        let _serial = FORK_TEST.lock().unwrap();
        let all = blockable(!0);
        // Start from a distinctive mask so "restored" is not "left blocked".
        let mut want: ::libc::sigset_t = unsafe { core::mem::zeroed() };
        let mut before: ::libc::sigset_t = unsafe { core::mem::zeroed() };
        unsafe {
            ::libc::sigemptyset(&mut want);
            ::libc::sigaddset(&mut want, ::libc::SIGUSR2);
            ::libc::sigaddset(&mut want, ::libc::SIGWINCH);
            assert_eq!(
                ::libc::pthread_sigmask(::libc::SIG_SETMASK, &want, &mut before),
                0
            );
        }
        let start = current_mask();
        assert_eq!(
            start,
            (1 << (::libc::SIGUSR2 - 1)) | (1 << (::libc::SIGWINCH - 1))
        );

        // Each mask is read under the locks and asserted after the release:
        // a panic with every lock held would hang the other tests.
        fork_prepare();
        let during = blockable(current_mask());
        fork_parent();
        assert_eq!(during, all, "prepare did not block every signal");
        assert_eq!(
            current_mask(),
            start,
            "parent handler did not restore the mask"
        );

        fork_prepare();
        let during = blockable(current_mask());
        fork_child();
        assert_eq!(during, all);
        assert_eq!(
            current_mask(),
            start,
            "child handler did not restore the mask"
        );

        unsafe { ::libc::pthread_sigmask(::libc::SIG_SETMASK, &before, core::ptr::null_mut()) };
    }

    /// Two threads with different masks run the prepare and parent
    /// handlers concurrently (glibc serialises them; the shared
    /// `FORK_SIGMASK` slot must not need it): each gets its own mask back,
    /// because a prepare saves only once it holds every lock, and the
    /// release reads before its first unlock.
    #[test]
    fn concurrent_fork_handlers_restore_each_threads_mask() {
        let _serial = FORK_TEST.lock().unwrap();
        let all = blockable(!0);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = [::libc::SIGUSR1, ::libc::SIGUSR2]
            .into_iter()
            .map(|signo| {
                let barrier = barrier.clone();
                thread::spawn(move || {
                    let mut want: ::libc::sigset_t = unsafe { core::mem::zeroed() };
                    let rc = unsafe {
                        ::libc::sigemptyset(&mut want);
                        ::libc::sigaddset(&mut want, signo);
                        ::libc::pthread_sigmask(::libc::SIG_SETMASK, &want, core::ptr::null_mut())
                    };
                    let start = current_mask();
                    // Past the barrier before any assert, so a failure here
                    // cannot leave the other thread waiting.
                    barrier.wait();
                    assert_eq!(rc, 0);
                    assert_eq!(start, 1 << (signo - 1));
                    for round in 0..200 {
                        fork_prepare();
                        // Asserted after the release: a panic with every
                        // lock held would hang the other thread.
                        let during = blockable(current_mask());
                        fork_parent();
                        assert_eq!(
                            during, all,
                            "round {round}: prepare left a signal unblocked"
                        );
                        assert_eq!(
                            current_mask(),
                            start,
                            "round {round}: another thread's mask came back"
                        );
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
    }

    /// The `.rs` files under `dir`, in path order.
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    /// Whether `t` starts a `fn` item: qualifiers, then `fn `.
    fn fn_item(t: &str) -> bool {
        let mut t = t;
        loop {
            let rest = [
                "pub ",
                "const ",
                "unsafe ",
                "extern \"C\" ",
                "proof ",
                "open ",
                "closed ",
            ]
            .iter()
            .find_map(|q| t.strip_prefix(q))
            .or_else(|| {
                t.strip_prefix("pub(")
                    .and_then(|r| r.split_once(") ").map(|(_, r)| r))
            });
            match rest {
                Some(r) => t = r,
                None => return t.starts_with("fn "),
            }
        }
    }

    /// Theorem 8's premises on its capability (`sys::may_syscall`), read
    /// from the source: `grant_syscalls` is called only in the body of an
    /// `extern "C"` function or of one that never returns (a call belongs
    /// to the nearest `fn` item line above it), and `may_syscall` appears
    /// only as a `requires` or loop invariant clause of its own, apart from
    /// its definition, the grant and the type invariants of `Stderr` and
    /// the report's `Sink`.
    #[test]
    fn syscall_grants_only_at_roots() {
        // Spelled in parts, so that this test's own lines match nothing.
        let grant = std::format!("{}{}", "grant_", "syscalls");
        let cap = std::format!("{}{}", "may_", "syscall");
        let clause = std::format!("crate::sys::{cap}(),");
        let allowed = [
            (
                "sys/mod.rs",
                std::format!("pub open spec fn {cap}() -> bool {{"),
            ),
            ("sys/mod.rs", std::format!("pub proof fn {grant}()")),
            ("sys/mod.rs", std::format!("{cap}(),")),
            ("sys/mod.rs", std::format!("reveal({cap});")),
            (
                "sys/abort.rs",
                std::format!("self.len <= 512 && crate::sys::{cap}()"),
            ),
            (
                "api/print.rs",
                std::format!("self.len < 1024 && crate::sys::{cap}()"),
            ),
        ];
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&src, &mut files);
        let mut grants = 0;
        for path in &files {
            let rel = path.strip_prefix(&src).unwrap().to_str().unwrap();
            let text = std::fs::read_to_string(path).unwrap();
            let (mut sig, mut kw) = ("", "");
            for (i, line) in text.lines().enumerate() {
                let t = line.trim();
                if t.starts_with("//") {
                    continue;
                }
                if fn_item(t) {
                    sig = t;
                }
                if let Some(k) = [
                    "requires",
                    "ensures",
                    "invariant",
                    "invariant_except_break",
                    "decreases",
                ]
                .iter()
                .find(|k| t == **k)
                {
                    kw = k;
                }
                let at = || std::format!("{rel}:{}: `{t}`", i + 1);
                if allowed.iter().any(|(f, a)| *f == rel && t == a.as_str()) {
                    continue;
                }
                if t.contains(grant.as_str()) {
                    grants += 1;
                    assert!(
                        sig.contains("extern \"C\" fn ") || sig.contains(") -> !"),
                        "{}: a syscall grant in `{sig}`",
                        at()
                    );
                } else if t.contains(cap.as_str()) {
                    assert!(
                        t == clause && kw != "ensures" && kw != "decreases",
                        "{}: `{cap}` used other than as a requires or invariant clause",
                        at()
                    );
                }
            }
        }
        assert!(grants > 0);
    }

    /// The premises of theorem "Read-only APIs never abort" on its
    /// capabilities, read from the source as for theorem 8:
    /// `grant_aborts` is called only in the body of an `extern "C"`
    /// function (never in one that does not return: aborting is what the
    /// capability allows); `may_abort` and `may_abort_corrupt` appear only
    /// as `requires` or loop invariant clauses that start with them;
    /// `integrity_aborts` only in its definition and in the `requires` of
    /// the theorem's witness; the integrity checks, the calls of
    /// `abort_integrity` and of `vspec::check::check` (which need only
    /// `may_abort_corrupt`), only in the functions THEOREMS.md lists. And
    /// every `external_body` function that does not return, or whose body
    /// can panic or abort in a release build (`assert!`, `panic!`, an
    /// `unwrap`, a call of an abort), requires one of the two
    /// capabilities; `may_abort_corrupt` alone only the integrity checks'
    /// trusted bodies THEOREMS.md lists. A trusted body that aborts only
    /// through a verified function it calls is not caught by the last
    /// check (the list in THEOREMS.md is read from the code).
    #[test]
    fn abort_grants_only_at_roots() {
        // Spelled in parts, so that this test's own lines match nothing.
        let grant = std::format!("{}{}", "grant_", "aborts");
        let cap = std::format!("{}{}", "may_", "abort");
        let integrity = std::format!("{}{}", "integrity_", "aborts");
        let sink = std::format!("{}{}(", "abort_", "integrity");
        let checks = std::format!("{}{}(", "chec", "k");
        let allowed = [
            std::format!("pub open spec fn {cap}() -> bool {{"),
            std::format!("pub proof fn {grant}()"),
            std::format!("{cap}(),"),
            std::format!("reveal({cap});"),
            std::format!("pub open spec fn {integrity}() -> bool {{"),
            std::format!("pub open spec fn {cap}_corrupt() -> bool {{"),
            std::format!("{cap}() || {integrity}()"),
        ];
        // The integrity checks: (file, function) of each call.
        let sites = [
            ("stats.rs", "fn huge_pages_short("),
            ("arena.rs", "fn alloc_sets("),
            ("class/table.rs", "pub fn new("),
            ("bitmap.rs", "pub fn allocated("),
            ("api/print.rs", "fn flush("),
        ];
        // The trusted bodies that require only `may_abort_corrupt`.
        let corrupt_only = [
            ("sys/abort.rs", "pub fn abort_integrity("),
            ("vspec/check.rs", "pub fn check("),
        ];
        // Trusted bodies whose panic is their `requires`, so cannot fail.
        let unfailing = [("heap/others.rs", "fn assert_not_home(")];
        let aborts = [
            "abort(",
            "abort_fmt(",
            "assert!(",
            "assert_eq!(",
            "assert_ne!(",
            "panic!(",
            "unreachable!(",
            ".unwrap()",
            ".expect(",
            "asm!(\"ud2\"",
        ];
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&src, &mut files);
        let (mut grants, mut integrity_checks, mut trusted) = (0, 0, 0);
        for path in &files {
            let rel = path.strip_prefix(&src).unwrap().to_str().unwrap();
            // The Kani harnesses (`kani/` and the `#[cfg(kani)]` modules)
            // are not built here or by Verus: skipped.
            if rel.contains("tests")
                || rel.starts_with("model/")
                || rel.starts_with("kani/")
                || rel == "testutil.rs"
            {
                continue;
            }
            let text = std::fs::read_to_string(path).unwrap();
            let lines: Vec<&str> = text.lines().map(str::trim).collect();
            let lines = match lines.iter().position(|t| *t == "mod tests {") {
                Some(end) => &lines[..end],
                None => &lines[..],
            };
            let (mut sig, mut kw) = ("", "");
            // Brace depth inside a `#[cfg(kani)]` item (one ends at its
            // closing brace, or at `;` if it has no body), and whether its
            // body has opened.
            let (mut kani, mut kani_open) = (None::<i32>, false);
            for (i, &t) in lines.iter().enumerate() {
                if t == "#[cfg(kani)]" {
                    (kani, kani_open) = (Some(0), false);
                    continue;
                }
                if let Some(d) = kani {
                    let d = d + t.matches('{').count() as i32 - t.matches('}').count() as i32;
                    kani_open |= t.contains('{');
                    let done = if kani_open { d <= 0 } else { t.ends_with(';') };
                    kani = if done { None } else { Some(d) };
                    continue;
                }
                if t.starts_with("//") {
                    continue;
                }
                if fn_item(t) {
                    (sig, kw) = (t, "");
                }
                if t == "{" || t.ends_with(" {") {
                    kw = "";
                }
                if let Some(k) = [
                    "requires",
                    "ensures",
                    "invariant",
                    "invariant_except_break",
                    "decreases",
                ]
                .iter()
                .find(|k| t == **k)
                {
                    kw = k;
                }
                let at = || std::format!("{rel}:{}: `{t}`", i + 1);
                let clause = kw == "requires" || kw.starts_with("invariant");
                if rel == "sys/mod.rs" && allowed.iter().any(|a| t == a.as_str()) {
                    continue;
                }
                if t.contains(grant.as_str()) {
                    grants += 1;
                    assert!(
                        sig.contains("extern \"C\" fn "),
                        "{}: an abort grant in `{sig}`",
                        at()
                    );
                } else if t.contains(integrity.as_str()) {
                    assert!(
                        t.ends_with(&std::format!("::{integrity}(),"))
                            && kw == "requires"
                            && sig.contains("fn theorem_"),
                        "{}: `{integrity}` outside a witness's requires",
                        at()
                    );
                } else if t.contains(cap.as_str()) {
                    assert!(
                        t.starts_with(&std::format!("crate::sys::{cap}"))
                            && t.ends_with(',')
                            && !t.contains("|| true")
                            && clause,
                        "{}: `{cap}` used other than as a requires or invariant clause",
                        at()
                    );
                }
                let call = t.contains(sink.as_str())
                    || t.match_indices(checks.as_str()).any(|(j, _)| {
                        // `check(` as a whole name, not `x.check(` or
                        // `check_zero(`.
                        j == 0 || matches!(t.as_bytes()[j - 1], b' ' | b':' | b'(' | b'!')
                    });
                if call && !t.starts_with("pub fn ") {
                    integrity_checks += 1;
                    assert!(
                        sites.iter().any(|(f, s)| *f == rel && sig.contains(s)),
                        "{}: an integrity check in `{sig}`, not listed",
                        at()
                    );
                }
                // A trusted function: its header (to the body's `{`) and
                // body (to the matching `}`).
                if t != "#[verifier::external_body]" {
                    continue;
                }
                let Some(f) = (i..lines.len().min(i + 8)).find(|&j| fn_item(lines[j])) else {
                    continue;
                };
                let Some(b) =
                    (f..lines.len()).find(|&j| lines[j] == "{" || lines[j].ends_with('{'))
                else {
                    continue;
                };
                let mut depth = 0i32;
                let mut end = b;
                for (j, l) in lines.iter().enumerate().skip(b) {
                    depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
                    end = j;
                    if depth <= 0 {
                        break;
                    }
                }
                let header = lines[f..b].join(" ");
                let body = lines[b..=end].join(" ").replace("debug_assert", "");
                let ends = header.contains(") -> !") || body.contains(") -> !");
                if !ends && !aborts.iter().any(|a| body.contains(a)) {
                    continue;
                }
                trusted += 1;
                let full = std::format!("crate::sys::{cap}()");
                let at = std::format!("{rel}:{}: `{}`", f + 1, lines[f]);
                let listed =
                    |l: &[(&str, &str)]| l.iter().any(|(p, s)| *p == rel && lines[f].contains(s));
                assert!(
                    header.contains(full.as_str())
                        || (header.contains(&std::format!("crate::sys::{cap}_corrupt()"))
                            && listed(&corrupt_only))
                        || listed(&unfailing),
                    "{at}: a trusted body that can abort, without `{cap}`"
                );
            }
        }
        assert!(grants > 0 && integrity_checks > 0 && trusted > 0);
    }
}
