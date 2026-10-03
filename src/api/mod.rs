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

use crate::sync::atomic::{AtomicU32, Ordering};
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
use vstd::prelude::*;
use vstd::prelude::{Ghost, Map, Tracked, int, verus};

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

const UNINIT: u32 = 0;
const INITIALISING: u32 = 1;
const READY: u32 = 2;

exec static STATE: AtomicU32 = AtomicU32::new(UNINIT);
/// Thread id of the initialising thread while the state is `INITIALISING`,
/// so a re-entrant call from it can be told apart from a concurrent one.
exec static INIT_TID: AtomicU32 = AtomicU32::new(0);

/// Storage for a value built in place at init and immutable afterwards.
/// Verus does not model the contents of the `UnsafeCell`.
#[verifier::reject_recursive_types(T)]
struct Global<T>(UnsafeCell<MaybeUninit<T>>);

impl<T> Global<T> {
    const fn new() -> Self {
        Global(UnsafeCell::new(MaybeUninit::uninit()))
    }

    /// Trusted: reads the storage (the `# Safety` contract).
    ///
    /// # Safety
    /// The value must have been initialised and `READY` observed with an
    /// acquire load on this thread.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn get(&self) -> &T {
        // SAFETY: guaranteed by the caller.
        unsafe { (*self.0.get()).assume_init_ref() }
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

/// Signal mask of the forking thread, saved by `fork_prepare` and restored by
/// `fork_release`. One slot suffices because it is only written and read
/// while the writer holds every allocator lock: a second thread in
/// `fork_prepare` (glibc serialises fork handlers with its atfork lock, but
/// the unit tests call them directly) waits on the first class lock before
/// it can store, and the releaser reads before its first unlock. A handler
/// cannot nest a fork inside the window because signals are blocked for its
/// duration.
static FORK_SIGMASK: Global<u64> = Global::new();

verus! {

/// `SHARED` as a reference, the one read of the static every accessor
/// below makes.
///
/// Trusted: the `UnsafeCell` read, and that the published `Shared`
/// satisfies [`Shared::wf`]. `init` (verified) builds the value with
/// `Shared::init`, whose contract gives `wf` of the value built in the
/// static's storage ([`Shared::built_at`]`(`[`Shared::storage`]`)`), and
/// aborts unless it returned `true`. What stays assumed: that this reads
/// that value, that is, `init_slow` runs `init` once, before it publishes
/// `READY` with a release store, and nothing writes what `wf` reads
/// afterwards (the regions, the push flags, the configuration, the huge
/// and metadata bounds and the slab tables are written only inside
/// `Shared::init`; the rest of the value, locks and atomics, changes, but
/// `wf` does not read it).
///
/// # Safety
/// The value must have been initialised and `READY` observed with an
/// acquire load on this thread.
#[verifier::external_body]
#[inline(always)]
unsafe fn shared_ref() -> (r: &'static Shared)
    ensures
        r.wf(),
    opens_invariants none
{
    // SAFETY: guaranteed by the caller.
    unsafe { SHARED.get() }
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
    if STATE.load(Ordering::Acquire) == READY {
        // SAFETY: `READY` was published after init with a release store.
        return unsafe { shared_ref() };
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
    if STATE.load(Ordering::Acquire) == READY {
        // SAFETY: as in `shared`.
        Some(unsafe { shared_ref() })
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
#[inline(always)]
pub unsafe fn shared_unchecked() -> (r: &'static Shared)
    ensures
        r.wf(),
    opens_invariants none
{
    // SAFETY: guaranteed by the caller.
    unsafe { shared_ref() }
}

} // verus!

/// The probed kernel features. Initialises the allocator if needed.
#[cfg(test)]
pub fn features() -> &'static Features {
    let _ = shared();
    // SAFETY: `shared()` returned, so `READY` has been observed.
    unsafe { FEATURES.get() }
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
    unsafe { FEATURE_LINE.get() }.as_str()
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
    unsafe { FEATURE_LINE.get() }.as_ptr()
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
/// once init has completed, which `shared` stands for here).
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
    mibp: *const usize,
    miblen: usize,
    mib: &[usize],
)
    requires
        crate::sys::may_syscall(),
        crate::sys::integrity_aborts(),
        shared.wf(),
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
    let _ = unsafe { FEATURE_LINE.get() }.as_ptr();
    // `experimental.utilization.query` and `batch_query`, whole, and the
    // rest of `mallctl`.
    let _ = mallctl::utilization(shared, p as *const core::ffi::c_void, verbose);
    mallctl::theorem_mallctl_reads_never_abort(shared, name, mibp, miblen, io, mib);
}

} // verus!

/// Whether init has completed (for the tests; the entry points use
/// [`shared_if_ready`]).
#[cfg(test)]
pub fn is_ready() -> bool {
    STATE.load(Ordering::Acquire) == READY
}

verus! {

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
    match STATE.compare_exchange(UNINIT, INITIALISING, Ordering::Acquire, Ordering::Acquire) {
        Ok(_) => {
            INIT_TID.store(sys::gettid() as u32, Ordering::Relaxed);
            init();
            STATE.store(READY, Ordering::Release);
        }
        Err(READY) => {}
        Err(_) => {
            // The `INIT_TID` store happens before any allocation the init
            // thread could make, so a matching tid here is re-entry.
            if INIT_TID.load(Ordering::Relaxed) == sys::gettid() as u32 {
                sys::abort("allocation during initialisation (re-entrant init)");
            }
            while STATE.load(Ordering::Acquire) != READY {
                core::hint::spin_loop();
            }
        }
    }
    // SAFETY: `READY` observed (or published by this thread).
    unsafe { shared_ref() }
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
{
    // The cell's own pointer, not `as_mut_ptr` through a `&mut`:
    // `init` keeps pointers derived from it, and writes through the
    // readers' `&Shared` would invalidate ones derived from a `&mut`.
    SHARED.0.get().cast::<Shared>()
}

/// The published feature report: `FEATURE_LINE` from `features`, then
/// `FEATURES`. Trusted: writes of the `UnsafeCell`s behind the statics.
///
/// # Safety
/// Called once, by `init`, before `READY` is published.
#[verifier::external_body]
#[inline(always)]
unsafe fn publish_features(features: Features) {
    // SAFETY: written once here, before `READY` is published.
    unsafe {
        (*FEATURE_LINE.0.get()).write(FeatureLine::build(&features));
        (*FEATURES.0.get()).write(features);
    }
}

/// Register the fork handlers, aborting if glibc refuses. Trusted: passes
/// function pointers, which Verus does not support, and formats the abort.
#[verifier::external_body]
#[inline(always)]
fn register_fork_handlers()
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    // SAFETY: the three handlers are plain `extern "C"` functions that only
    // touch allocator locks.
    let rc =
        unsafe { ::libc::pthread_atfork(Some(fork_prepare), Some(fork_parent), Some(fork_child)) };
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
/// publishes `READY`.
fn init()
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    let features = Features::probe();
    // The slab geometry of the classes whose table slab is below the page
    // (design 3, page-sized slabs): written once here, before any other
    // thread can see the allocator, never again.
    // SAFETY: `init` runs once, under `INITIALISING`, before `READY`.
    unsafe { layout::adopt_page_geometry(features.page_size.trailing_zeros()) };
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
        );
        if !ok {
            sys::abort("metadata region too small for the allocator tables");
        }
        // Design 11: `Shared::init` forced the stored `thp` to `never`
        // under a cgroup limit below 1 GiB; the advice follows.
        if SHARED.get().policy.thp_forced() {
            crate::thp::force_never();
        }
        publish_features(features);
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
/// [`LockHeld`] per listed lock, each the hold of the lock its key names.
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

    /// Add the hold `t` under key `k`.
    proof fn put(tracked &mut self, k: ForkLock, tracked t: LockHeld)
        ensures
            final(self).keys() == old(self).keys().insert(k),
            final(self).lock(k) == t.held(),
            forall|j: ForkLock| j != k ==> #[trigger] final(self).lock(j) == old(self).lock(j),
            final(self).n() == old(self).n(),
            final(self).arenas() == old(self).arenas(),
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
    {
        self.toks.tracked_remove(k)
    }

    /// Each token is the hold of the lock its key names in `s`, and the
    /// arenas are indices below `n`.
    pub(crate) open spec fn wf(self, s: Shared) -> bool {
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
/// `fork_prepare` read (`n` and `arenas` are those reads, which the
/// ensures does not name: the premise is that they are). `state` must be
/// the state lock's token: a fact about a token the caller already has is
/// a requirement, never a conclusion (ensured, it let a token of any other
/// lock prove `false`).
axiom fn axiom_fork_resumed(s: &Shared, tracked state: &LockHeld) -> (tracked held: ForkHeld)
    requires
        state.held() == purge::background::state_lock(),
    ensures
        fork_holds(*s, held, *state),
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
/// `fork_prepare` read. Trusted, as [`arena_held`].
#[verifier::external_body]
#[inline(always)]
fn count_held(s: &Shared, Tracked(held): Tracked<&ForkHeld>) -> (r: u32)
    requires
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
/// per lock of the fork lock list (asserted at the end).
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
    let mask = sys::block_all_signals();
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
    let tracked mut held = ForkHeld { n: n as int, arenas, toks };
    for c in 0..NCLASSES
        invariant
            crate::sys::may_syscall(),
            held.wf(*s),
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
            n == held.n(),
            held.wf(*s),
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
    proof {
        assert(fork_holds(*s, held, state));
    }
    // SAFETY: every allocator lock is held, so no other forking thread can
    // touch the slot until `fork_release` has read it (see `FORK_SIGMASK`).
    unsafe { save_sigmask(mask) };
}

/// Save the signal mask for `fork_release`. Trusted: a write of the
/// `UnsafeCell` behind [`FORK_SIGMASK`].
///
/// # Safety
/// The caller holds every allocator lock (see [`FORK_SIGMASK`]).
#[verifier::external_body]
#[inline(always)]
unsafe fn save_sigmask(mask: u64) {
    // SAFETY: guaranteed by the caller.
    unsafe { (*FORK_SIGMASK.0.get()).write(mask) };
}

/// The signal mask `fork_prepare` saved. Trusted: a read of the
/// `UnsafeCell` behind [`FORK_SIGMASK`].
///
/// # Safety
/// Written by the `fork_prepare` that paired with this release, and still
/// the caller's because every lock is held.
#[verifier::external_body]
#[inline(always)]
unsafe fn saved_sigmask() -> u64 {
    // SAFETY: guaranteed by the caller.
    unsafe { (*FORK_SIGMASK.0.get()).assume_init() }
}

/// Release the locks taken by `fork_prepare`, in reverse order, then let
/// signals through again.
///
/// Fork safety, second half: it starts from the hold `fork_prepare`
/// ended with ([`fork_holds`]; `state` is the state lock's token) and
/// releases each listed lock once, by handing its token back to
/// `RawLock::unlock_held`; it ends holding none (asserted at the end).
fn fork_release(Tracked(state): Tracked<LockHeld>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        state.held() == purge::background::state_lock(),
{
    let s = shared();
    let tracked mut held = axiom_fork_resumed(s, &state);
    // SAFETY: written by the `fork_prepare` that paired with this release,
    // and still ours because every lock is held until the unlocks below.
    let mask = unsafe { saved_sigmask() };
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
    sys::set_signal_mask(mask);
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

    /// The two handler tests drive the process-wide locks directly and the
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
        s.huge.purge_empty(u64::MAX, Tracked::assume_new());
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

    /// The handlers block every signal while the locks are held and restore
    /// the caller's mask (whatever it was) afterwards, on both fork sides.
    #[test]
    fn fork_handlers_block_signals_while_locked() {
        fn current_mask() -> u64 {
            let mut set: ::libc::sigset_t = unsafe { core::mem::zeroed() };
            let rc =
                unsafe { ::libc::pthread_sigmask(::libc::SIG_BLOCK, core::ptr::null(), &mut set) };
            assert_eq!(rc, 0);
            // The kernel mask is the first 8 bytes of glibc's 128-byte set.
            unsafe { core::ptr::read_unaligned(&set as *const _ as *const u64) }
        }
        fn blockable(mask: u64) -> u64 {
            const KILL_STOP: u64 = (1 << (::libc::SIGKILL - 1)) | (1 << (::libc::SIGSTOP - 1));
            mask & !KILL_STOP
        }
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

        fork_prepare();
        assert_eq!(
            blockable(current_mask()),
            all,
            "prepare did not block every signal"
        );
        fork_parent();
        assert_eq!(
            current_mask(),
            start,
            "parent handler did not restore the mask"
        );

        fork_prepare();
        assert_eq!(blockable(current_mask()), all);
        fork_child();
        assert_eq!(
            current_mask(),
            start,
            "child handler did not restore the mask"
        );

        unsafe { ::libc::pthread_sigmask(::libc::SIG_SETMASK, &before, core::ptr::null_mut()) };
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
    /// its definition, the grant and `Stderr`'s type invariant.
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
    /// the theorem's witnesses; the integrity checks, the calls of
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
            ("stats.rs", "fn arena_at("),
            ("arena.rs", "fn alloc_sets("),
            ("class/table.rs", "pub fn new("),
            ("bitmap.rs", "pub fn allocated("),
        ];
        // The trusted bodies that require only `may_abort_corrupt`.
        let corrupt_only = [
            ("sys/abort.rs", "pub fn abort_integrity("),
            ("vspec/check.rs", "pub fn check("),
            ("policy.rs", "pub fn empty_bytes("),
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
