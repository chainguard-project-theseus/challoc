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
//! on `.init_array` for init; the library's constructor only registers the
//! fork handlers early ([`REGISTER_EARLY`], design 9).
//!
//! Init never allocates. It probes the kernel, reserves the regions, builds
//! the [`Shared`] in place inside a static, creates the thread-exit key
//! (inside `Shared::init`) and registers the fork handlers. glibc's
//! `pthread_key_create` and `pthread_atfork` do not allocate for the first
//! keys and handlers, which is all we need.
//!
//! The fork prepare handler takes every allocator lock in nesting order
//! (design 9; the list is [`fork_prepare`]'s body): the explicit-cache
//! table lock, the arena creation lock, the process statistics lock, every
//! arena's class locks, the slab table locks, every arena's slab-set lock,
//! the huge lock, the metadata lock, the per-arena statistics locks, then
//! the background thread's state lock. That order matches the allocator's
//! own nesting (the explicit-cache table lock is held across heap creation
//! and destruction, which take the arena creation lock, class locks and the
//! metadata lock; `epoch` holds the process statistics lock across its
//! scan, which takes the class, slab table, huge, metadata and per-arena
//! statistics locks; `take_group` holds a class lock and its slab table
//! lock while it takes the arena's slab-set lock; class, slab table,
//! slab-set and huge code take the metadata lock while holding their own;
//! nothing takes a class or huge lock while holding metadata; the
//! per-arena statistics locks and the background state lock are leaves),
//! so a thread that is mid-operation when another thread
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
/// The bodies are inside `verus!`: Verus checks them, after granting
/// each export the syscall and abort capabilities of a C entry point. A
/// pair that returns a value names its result (`-> (r: T)`), and a pair
/// may give both exports the same `ensures [..]` clauses, which state the
/// Rust entry point's postcondition at the C boundary. The body is taken
/// as tokens rather than a Rust block, so that it may hold Verus syntax
/// (`proof { .. }`).
#[cfg(not(test))]
macro_rules! verified_export_pair {
    ($($(#[$m:meta])* fn $name:ident / $je:ident($($arg:ident: $ty:ty),*) $(-> ($r:ident: $ret:ty))? $(ensures [$($ens:tt)*])? { $($body:tt)* })*) => {
        ::vstd::prelude::verus! {
        $(
            $(#[$m])*
            #[unsafe(no_mangle)]
            pub extern "C" fn $name($($arg: $ty),*) $(-> ($r: $ret))?
                $(ensures $($ens)*)?
            {
                proof {
                    crate::sys::grant_syscalls();
                    crate::sys::grant_aborts();
                }
                $($body)*
            }
            $(#[$m])*
            #[unsafe(no_mangle)]
            pub extern "C" fn $je($($arg: $ty),*) $(-> ($r: $ret))?
                $(ensures $($ens)*)?
            {
                proof {
                    crate::sys::grant_syscalls();
                    crate::sys::grant_aborts();
                }
                $($body)*
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
#[cfg(test)]
mod restated;

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::arena::Arena;
use crate::config;
use crate::heap::Shared;
use crate::layout::{self, NCLASSES, Regions};
use crate::lock::LockHeld;
#[cfg(verus_keep_ghost)]
use crate::lock::{RawLock, axiom_entry, rank_arena_stats, rank_class, rank_sets, rank_table};
use crate::purge;
use crate::sys;
use crate::sys::features::{FeatureLine, Features};
#[cfg(verus_keep_ghost)]
use crate::sys::{SigMask, all_blocked, effective};
use crate::vspec::once;
use crate::vspec::word::WordPerm;
#[cfg(verus_keep_ghost)]
use crate::vspec::word::word_addr;
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Map, Tracked, int, verus};
use vstd::raw_ptr::PointsTo;
#[cfg(verus_keep_ghost)]
use vstd::resource::Loc;
use vstd::resource::ghost_var::{GhostVar, GhostVarAuth};

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

} // verus!

static STATE: AtomicU32 = AtomicU32::new(UNINIT);

/// The library's constructor registers the fork handlers, without
/// initialising the allocator: glibc runs prepare handlers in the reverse
/// order of registration, so every handler registered later (by the
/// program's constructors or code, or a library initialised after
/// challoc) runs before challoc's prepare takes the allocator's locks,
/// and may allocate, or wait on threads that allocate (a thread pool's
/// shutdown), as under glibc's malloc. The handlers do nothing until
/// challoc is initialised (`fork_prepare`), so loading the library costs
/// one `pthread_atfork`. Out of the tests, which do not run the library
/// as the process's allocator, and of Verus's build, which has no `#[used]`
/// statics.
#[cfg(all(not(test), not(verus_keep_ghost)))]
#[used]
#[unsafe(link_section = ".init_array")]
static REGISTER_EARLY: extern "C" fn() = register_early;

#[cfg(all(not(test), not(verus_keep_ghost)))]
extern "C" fn register_early() {
    let _ = atfork_handlers();
}

verus! {

/// The fork handlers are registered (`atfork_handlers`): by the library's
/// constructor, or at init when that did not run first.
exec static ATFORK_REGISTERED: AtomicBool = AtomicBool::new(false);

/// `fork_prepare` took the allocator's locks for the `fork` under way:
/// false when challoc was not initialised then, so the parent and child
/// handlers release nothing.
exec static FORK_LOCKED: AtomicBool = AtomicBool::new(false);

/// The gate between init and `fork` (design 9): a `fork` never copies an
/// init under way on another thread, whose child would wait for an
/// initialising thread it does not have. `GATE_IDLE` until a thread
/// starts init or a `fork` closes it; while an init is under way, the
/// initialising thread's `pthread_self` (`sys::pthread_self`: never 0, 1
/// or 2, and kept by the forking thread in a `fork` child), which only
/// that thread's claim follows, until it publishes `READY`; then
/// `GATE_DONE` for good. `GATE_FORKING` from the prepare handler of a
/// `fork` that found it idle (challoc not initialised, none under way)
/// until that fork's parent and child handlers, during which no init
/// starts but the forking thread's own (a handler registered before
/// challoc's that allocates). Recording the initialiser in the gate word
/// itself, by the exchange that opens the init, lets a `fork` from a
/// signal handler that interrupted that thread's own init tell so at any
/// point of it.
exec static INIT_GATE: AtomicUsize = AtomicUsize::new(GATE_IDLE);
const GATE_IDLE: usize = 0;
const GATE_DONE: usize = 1;
const GATE_FORKING: usize = 2;
/// The forking thread while the gate is `GATE_FORKING`, as
/// `pthread_self` (`sys::pthread_self`), which the forking thread keeps in
/// the child, where its kernel thread ID is new: a child handler
/// registered before challoc's runs before `fork_gate_exit` and may
/// initialise.
exec static GATE_FORKER: AtomicUsize = AtomicUsize::new(0);
/// This `fork`'s prepare handler closed the gate.
exec static FORK_GATED: AtomicBool = AtomicBool::new(false);

/// Thread id of the initialising thread while the state is `INITIALISING`,
/// so a re-entrant call from it can be told apart from a concurrent one.
exec static INIT_TID: AtomicU32 = AtomicU32::new(0);

/// The permissions to the statics `init` writes once, frozen when it is
/// done: the published `Shared` (the value `Shared::init` built, which
/// is well formed) and the class table after
/// [`layout::adopt_page_geometry`] (not the probed features, which the
/// feature line's first request completes, nor the line, built then:
/// [`feature_line_ready`]); and the feature line's invariant
/// ([`LinePred`]), which `init` makes with the permissions to those two
/// statics. `STATE`'s
/// invariant holds them from `READY` on ([`StatePred`]), so a thread
/// that loads `READY` gets them ([`load_state`]); being `&'static`, they
/// are shared by every reader, and nothing writes the statics through
/// them again (the locks and atomics inside `Shared` change through
/// `&Shared`, but nothing [`Shared::wf`] reads does), except through the
/// line's invariant, which hands the permissions to [`FEATURES`] and
/// [`FEATURE_LINE`] to the line's one builder ([`claim_line`]).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct Published {
    pub tracked shared: &'static PointsTo<Shared>,
    pub tracked classes: &'static PointsTo<[layout::Class; NCLASSES]>,
    pub tracked line: &'static LineInv,
}

impl Published {
    /// Each permission is to its static and initialised; the `Shared` is
    /// the one built at its storage, as `Shared::init` built it
    /// ([`Shared::built`]); the class table is well formed
    /// ([`layout::table_ok`]) and is [`layout::class_table`]; the line's
    /// invariant is in [`line_ns`].
    pub open spec fn ok(&self) -> bool {
        &&& self.shared.ptr() == Shared::storage()
        &&& self.shared.is_init()
        &&& self.shared.value() == Shared::built_at(Shared::storage())
        &&& self.shared.value().built()
        &&& self.classes.ptr() == layout::classes_storage()
        &&& self.classes.is_init()
        &&& layout::table_ok(self.classes.value())
        &&& self.classes.value() == layout::class_table()
        &&& self.line.namespace() == line_ns()
    }
}

/// The allocator is published: `init` has frozen the permissions
/// [`Published`] holds. A fact rather than a token, so that it travels in
/// contracts at no cost: [`Shared::wf`] includes it (a `&Shared` is handed
/// out only from `READY` on). Whoever has it gets the frozen permissions
/// back from [`axiom_published`]. Uninterpreted. Verified code learns it
/// from [`axiom_ready`], of a thread that holds a `Published` (the
/// publication itself, or a load of `READY`, [`load_state`]). Being one
/// timeless fact, it says nothing of `STATE`'s value at a given point:
/// that `STATE` is `READY` and stays so takes the token
/// [`ReadyMark`], which no fact produces.
pub uninterp spec fn ready() -> bool;

/// Trusted: a thread that holds the frozen permissions ([`Published`],
/// which `init` builds when it freezes them and `STATE`'s invariant holds
/// from `READY` on, [`load_state`]) may state [`ready`]. A `Published`
/// exists only once `init` has used up the linear permissions to the
/// statics it covers (`api::axiom_statics` hands them out once, for the
/// process's one-shot token), so no thread that can state `ready` holds
/// one of those outside the line's invariant ([`Published::line`]).
pub axiom fn axiom_ready(tracked p: &'static Published)
    requires
        p.ok(),
    ensures
        ready(),
;

/// Trusted: the frozen permissions, for a thread that knows the allocator
/// is published ([`ready`]): a `&'static` reference to the `Published`
/// `init` froze, the only one there is. Another reference to it is
/// harmless: its permissions to `SHARED` and the class table only give
/// shared references to statics nothing writes again ([`Published`]),
/// and no linear permission to those statics exists once `ready` can be
/// stated ([`axiom_ready`]). Its line invariant ([`Published::line`])
/// does hold linear permissions (`LINE_STATE`'s, and the two statics'
/// while the line is unbuilt), and a second `LineInv` in [`line_ns`]
/// would duplicate them; none can exist, since a `LineInv` needs a
/// [`LinePerm`], [`axiom_statics`] mints the only one, and `init` puts
/// it in the invariant it publishes, so every reference this returns is
/// to that one invariant. That the fact reaches a thread only after it,
/// or a thread it happens after, saw `READY` is what [`ready`]'s producers
/// ensure (see there), carried along in contracts. It hands back the
/// statics' permissions only, not the [`ReadyMark`], so a proof that
/// assumes `ready` where it may not hold yet learns nothing from it
/// about `STATE`.
pub axiom fn axiom_published() -> (tracked r: &'static Published)
    requires
        ready(),
    ensures
        r.ok(),
;

/// The published permissions a thread has seen, if any, and the
/// [`ReadyMark`] with them: what the functions that load or write
/// `STATE` hand back.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct Seen {
    pub tracked published: Option<&'static Published>,
    pub tracked mark: Option<&'static ReadyMark>,
}

impl Seen {
    /// The thread has seen the published permissions.
    pub open spec fn ok(&self) -> bool {
        self.published matches Some(p) && p.ok()
    }

    /// The thread holds the [`ReadyMark`].
    pub open spec fn marked(&self) -> bool {
        self.mark matches Some(m) && marks_ready(*m)
    }
}

/// The token that `STATE` is `READY` and stays so: the half of the ghost
/// variable at [`ready_mark`] that [`publish_ready`] sets to `true` and
/// freezes at the `READY` store ([`StatePred`]). Only a thread that saw
/// `READY` gets it ([`load_state`], [`claim_init`], [`publish_ready`]),
/// and a holder finds `STATE` at `READY` ([`load_state_marked`]).
pub type ReadyMark = GhostVar<bool>;

/// `m` is the [`ReadyMark`]: the half at [`ready_mark`], at `true`.
pub open spec fn marks_ready(m: ReadyMark) -> bool {
    m.id() == ready_mark() && m@
}

/// The location of the ghost variable that `STATE`'s invariant ties to
/// `READY` ([`StatePred`]). Uninterpreted: one location, fixed for the
/// process.
pub uninterp spec fn ready_mark() -> Loc;

/// Ghost permission to `LINE_STATE`: its current
/// [`value`](LinePerm::value). It is not `Copy`.
#[verifier::external_body]
pub tracked struct LinePerm {
    _no_copy: (),
}

impl LinePerm {
    /// `LINE_STATE`'s value.
    pub uninterp spec fn value(&self) -> u8;
}

/// The permissions to the two statics the feature line's build writes:
/// [`FEATURES`], whose THP size table it completes, and
/// [`FEATURE_LINE`].
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct LineCell {
    pub tracked features: PointsTo<Features>,
    pub tracked line: PointsTo<FeatureLine>,
}

impl LineCell {
    /// The permissions are to the two statics, and the features are
    /// initialised (`init` published them before `READY`): what the build
    /// starts from.
    pub open spec fn unbuilt(&self) -> bool {
        &&& self.features.ptr() == features_storage()
        &&& self.features.is_init()
        &&& self.line.ptr() == line_storage()
    }

    /// ... and the line is built: what readers see.
    pub open spec fn built(&self) -> bool {
        &&& self.unbuilt()
        &&& self.line.is_init()
    }
}

/// What the line's invariant holds: `LINE_STATE`'s permission, the
/// statics' permissions while the line is unbuilt, and those
/// permissions, frozen, from `LINE_READY` on.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct LineVal {
    pub tracked perm: LinePerm,
    pub tracked cell: Option<LineCell>,
    pub tracked frozen: Option<&'static LineCell>,
}

/// The feature line's invariant, the once-cell protocol of
/// `LINE_STATE`: the state is `LINE_UNBUILT`, `LINE_BUILDING` or
/// `LINE_READY`; while it is `LINE_UNBUILT` the invariant holds the
/// permissions to [`FEATURES`] and [`FEATURE_LINE`] ([`LineCell::unbuilt`]),
/// and from `LINE_READY` on those permissions frozen, the line built
/// ([`LineCell::built`]). `init` makes it at `LINE_UNBUILT`, with the
/// features published. The exchange from `LINE_UNBUILT` to
/// `LINE_BUILDING` ([`claim_line`]) hands the permissions to its winner,
/// the only writer of the two statics, and the invariant holds none
/// while the line is `LINE_BUILDING`; the `LINE_READY` store
/// ([`publish_line`]) puts them back frozen, after the build, for every
/// reader ([`load_line`]).
// Ghost: named only by proof code.
#[allow(dead_code)]
pub struct LinePred;

impl InvariantPredicate<(), LineVal> for LinePred {
    open spec fn inv(k: (), v: LineVal) -> bool {
        &&& v.perm.value() == LINE_UNBUILT || v.perm.value() == LINE_BUILDING || v.perm.value() == LINE_READY
        &&& v.perm.value() == LINE_UNBUILT ==> (v.cell matches Some(c) && c.unbuilt())
        &&& v.perm.value() == LINE_BUILDING ==> v.cell is None
        &&& v.perm.value() == LINE_READY ==> (v.frozen matches Some(c) && c.built())
    }
}

/// The namespace of the line's invariant.
pub open spec fn line_ns() -> int {
    0x6c69_6e65
}

/// The feature line's invariant.
pub type LineInv = AtomicInvariant<(), LineVal, LinePred>;

/// The line's frozen permissions a thread has seen, if any: what
/// [`load_line`] hands back when it reads `LINE_READY`.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct LineSeen {
    pub tracked frozen: Option<&'static LineCell>,
}

/// Ghost permission to `STATE`: its current [`value`](StatePerm::value).
/// It is not `Copy`.
#[verifier::external_body]
pub tracked struct StatePerm {
    _no_copy: (),
}

impl StatePerm {
    /// `STATE`'s value.
    pub uninterp spec fn value(&self) -> u32;
}

/// What `STATE`'s invariant holds: its permission, the process's
/// one-shot tokens until the winning exchange takes them, the published
/// permissions from `READY` on, and the ghost variable at [`ready_mark`]:
/// its authority, its second half until the winning exchange takes it
/// too, and that half frozen ([`ReadyMark`]) from `READY` on.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct StateVal {
    pub tracked perm: StatePerm,
    pub tracked steps: Option<Map<once::Step, once::Once>>,
    pub tracked published: Option<&'static Published>,
    pub tracked auth: GhostVarAuth<bool>,
    pub tracked half: Option<GhostVar<bool>>,
    pub tracked mark: Option<&'static ReadyMark>,
}

/// `STATE`'s invariant: the state is `UNINIT`, `INITIALISING` or
/// `READY`; while it is `UNINIT` the invariant holds the tokens of every
/// step ([`once::all_steps`]) and the ghost variable's second half, and
/// from `READY` on the published permissions ([`Published::ok`]) and the
/// [`ReadyMark`]. The ghost variable at [`ready_mark`] is `true` exactly
/// at `READY`, so `READY` is final: a holder of the mark, which is
/// frozen at `true`, finds `STATE` at `READY` ([`load_state_marked`]).
/// `STATE` moves from `UNINIT` to `INITIALISING` only at [`claim_init`]'s
/// exchange, which takes the tokens and the half, and to `READY` only at
/// [`publish_ready`]'s store, after `init` built and froze what it
/// publishes, which sets the variable and freezes the half; nothing
/// writes it otherwise, so it never goes back.
// Ghost: named only by proof code.
#[allow(dead_code)]
struct StatePred;

impl InvariantPredicate<(), StateVal> for StatePred {
    open spec fn inv(k: (), v: StateVal) -> bool {
        &&& v.perm.value() == UNINIT || v.perm.value() == INITIALISING || v.perm.value() == READY
        &&& v.perm.value() == UNINIT ==> (v.steps matches Some(m) && once::all_steps(m))
        &&& v.perm.value() == READY ==> (v.published matches Some(p) && p.ok())
        &&& v.auth.id() == ready_mark()
        &&& v.auth@ <==> v.perm.value() == READY
        &&& v.perm.value() == UNINIT ==> (v.half matches Some(h) && h.id() == ready_mark())
        &&& v.perm.value() == READY ==> (v.mark matches Some(m) && marks_ready(*m))
    }
}

/// The namespace of `STATE`'s invariant.
spec fn state_ns() -> int {
    0x7374_6174
}

/// `STATE`'s invariant. Trusted: the process starts with `STATE` at
/// `UNINIT` (the static's initialiser), with the tokens of every step
/// of building the allocator, none taken ([`once`]), and with both
/// halves of the ghost variable at [`ready_mark`], at `false`; every
/// write of `STATE` is [`state_claim`]'s or [`state_publish`]'s, inside
/// this invariant, and verified code learns its value only from them and
/// [`state_load`] (a test-only load elsewhere carries no ghost state).
/// The axiom stands for one invariant, the same object at every call.
/// Verus does not know that two calls return the same object, which
/// only weakens what it can prove: what it proves holds of the one
/// invariant, whose namespace is fixed so that no two opens nest.
axiom fn axiom_state() -> (tracked r: &'static AtomicInvariant<(), StateVal, StatePred>)
    ensures
        r.namespace() == state_ns(),
;

/// The address of [`FEATURES`]' storage.
pub uninterp spec fn features_storage() -> *mut Features;

/// The address of [`FEATURE_LINE`]'s storage.
pub uninterp spec fn line_storage() -> *mut FeatureLine;

} // verus!

/// Storage for a value built in place once: at init and immutable
/// afterwards (`SHARED`), or completed and built at the feature line's
/// first request ([`FEATURES`], [`FEATURE_LINE`], [`build_line`]). Read
/// and written through its storage's pointer ([`shared_storage_ptr`],
/// [`features_ptr`], [`line_ptr`]).
struct Global<T>(UnsafeCell<MaybeUninit<T>>);

// SAFETY: written exactly once, by the initialising thread before it
// publishes `READY` with a release store; every reader loads the state with
// acquire first and then only takes shared references.
unsafe impl<T> Sync for Global<T> {}

impl<T> Global<T> {
    const fn new() -> Self {
        Global(UnsafeCell::new(MaybeUninit::uninit()))
    }
}

static SHARED: Global<Shared> = Global::new();
static FEATURES: Global<Features> = Global::new();
/// The feature line for `challoc_features()` and `malloc_stats_print`,
/// built at its first request ([`feature_line_ready`]), not at init.
static FEATURE_LINE: Global<FeatureLine> = Global::new();
/// [`FEATURE_LINE`]'s state: `LINE_UNBUILT`, `LINE_BUILDING`, `LINE_READY`
/// (its invariant: [`LinePred`]).
static LINE_STATE: AtomicU8 = AtomicU8::new(LINE_UNBUILT);

verus! {

pub const LINE_UNBUILT: u8 = 0;
pub const LINE_BUILDING: u8 = 1;
pub const LINE_READY: u8 = 2;

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
/// records. Its identity is carried by the holds rather than stated of
/// the static (Verus cannot name a static in a specification): the
/// prepare's hold records the word it stored to
/// ([`ForkHeld::slot_addr`], bookkeeping: nothing reads that hold
/// after the store), and the release's hold is handed over for the word
/// [`fork_slot`] returns to the release ([`resume::resumed`]); that the
/// two are one word is the handoff's premise.
exec static FORK_SIGMASK: AtomicU64 = AtomicU64::new(0);

/// [`FORK_SIGMASK`] as a reference. Nothing is claimed of it: the holds
/// carry its identity (see [`FORK_SIGMASK`]).
#[inline(always)]
fn fork_slot() -> &'static AtomicU64 {
    &FORK_SIGMASK
}

} // verus!

verus! {

/// `STATE.load(Acquire)`: the permission's value. Trusted: a `core`
/// atomic load, one atomic operation on `STATE`.
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

/// An acquire load of `STATE`, with the published permissions and the
/// [`ReadyMark`] when it reads `READY`.
#[inline(always)]
fn load_state(Tracked(seen): Tracked<&mut Seen>) -> (s: u32)
    ensures
        s == READY ==> final(seen).ok() && final(seen).marked(),
    opens_invariants [ state_ns() ]
{
    let s;
    vstd::open_atomic_invariant!(axiom_state() => v => {
        s = state_load(Tracked(&v.perm));
        proof {
            if s == READY {
                seen.published = v.published;
                seen.mark = v.mark;
            }
        }
    });
    s
}

/// [`load_state`] for a holder of the [`ReadyMark`], which reads `READY`:
/// the invariant's authority agrees with the mark, which is `true`.
#[inline(always)]
fn load_state_marked(Tracked(seen): Tracked<&mut Seen>, Tracked(mark): Tracked<&'static ReadyMark>) -> (s: u32)
    requires
        marks_ready(*mark),
    ensures
        s == READY,
        final(seen).ok(),
    opens_invariants [ state_ns() ]
{
    let s;
    vstd::open_atomic_invariant!(axiom_state() => v => {
        s = state_load(Tracked(&v.perm));
        proof {
            v.auth.agree(mark);
            seen.published = v.published;
        }
    });
    s
}

/// `LINE_STATE.load(ordering)`: the permission's value (it panics for a
/// `Release` or `AcqRel` ordering). Trusted: a `core` atomic load, one
/// atomic operation on `LINE_STATE`.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
fn line_load(ordering: Ordering, Tracked(perm): Tracked<&LinePerm>) -> (r: u8)
    requires
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
{
    LINE_STATE.load(ordering)
}

/// `LINE_STATE.compare_exchange(current, new, success, failure)`: `Ok`
/// with the previous value, `current`, when it held it and now holds
/// `new`; otherwise `Err` with the value it holds, unchanged (the strong
/// form never fails spuriously). It panics for a `Release` or `AcqRel`
/// failure ordering. Trusted: a `core` compare-exchange, one atomic
/// read-modify-write on `LINE_STATE`.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
fn line_compare_exchange(
    current: u8,
    new: u8,
    success: Ordering,
    failure: Ordering,
    Tracked(perm): Tracked<&mut LinePerm>,
) -> (r: Result<u8, u8>)
    requires
        !(failure is Release),
        !(failure is AcqRel),
    ensures
        r matches Ok(prev) ==> prev == current && old(perm).value() == current && final(perm).value() == new,
        r matches Err(prev) ==> prev == old(perm).value() && prev != current && final(perm).value() == prev,
    opens_invariants none
    no_unwind
{
    LINE_STATE.compare_exchange(current, new, success, failure)
}

/// `LINE_STATE.store(v, ordering)` (it panics for an `Acquire` or
/// `AcqRel` ordering). Trusted: a `core` atomic store, one atomic
/// operation on `LINE_STATE`.
#[inline(always)]
#[verifier::external_body]
#[verifier::atomic]
fn line_store(v: u8, ordering: Ordering, Tracked(perm): Tracked<&mut LinePerm>)
    requires
        !(ordering is Acquire),
        !(ordering is AcqRel),
    ensures
        final(perm).value() == v,
    opens_invariants none
    no_unwind
{
    LINE_STATE.store(v, ordering)
}

/// An acquire load of `LINE_STATE`, with the line's frozen permissions
/// when it reads `LINE_READY` ([`LinePred`]).
#[inline(always)]
fn load_line(Tracked(inv): Tracked<&'static LineInv>, Tracked(seen): Tracked<&mut LineSeen>) -> (s: u8)
    requires
        inv.namespace() == line_ns(),
    ensures
        s == LINE_READY ==> (final(seen).frozen matches Some(c) && c.built()),
    opens_invariants [ line_ns() ]
{
    let s;
    vstd::open_atomic_invariant!(inv => v => {
        s = line_load(Ordering::Acquire, Tracked(&v.perm));
        proof {
            if s == LINE_READY {
                seen.frozen = v.frozen;
            }
        }
    });
    s
}

/// `LINE_STATE`'s exchange from `LINE_UNBUILT` to `LINE_BUILDING`: the
/// winner gets the permissions to the two statics, which the invariant
/// holds while the line is unbuilt ([`LinePred`]).
#[inline(always)]
fn claim_line(Tracked(inv): Tracked<&'static LineInv>) -> (r: (Result<u8, u8>, Tracked<Option<LineCell>>))
    requires
        inv.namespace() == line_ns(),
    ensures
        r.0 is Ok ==> (r.1@ matches Some(c) && c.unbuilt()),
    opens_invariants [ line_ns() ]
{
    let tracked mut cell = None;
    let won;
    vstd::open_atomic_invariant!(inv => v => {
        won = line_compare_exchange(LINE_UNBUILT, LINE_BUILDING, Ordering::Acquire, Ordering::Acquire, Tracked(&mut v.perm));
        proof {
            if won is Ok {
                cell = Some(v.cell.tracked_take());
            }
        }
    });
    (won, Tracked(cell))
}

/// The `LINE_READY` store, `Release`, with the built line's permissions
/// frozen into the invariant for every reader ([`LinePred`]); the frozen
/// permissions.
#[inline(always)]
fn publish_line(Tracked(inv): Tracked<&'static LineInv>, Tracked(cell): Tracked<LineCell>) -> (r: Tracked<&'static LineCell>)
    requires
        inv.namespace() == line_ns(),
        cell.built(),
    ensures
        r@.built(),
    opens_invariants [ line_ns() ]
{
    let tracked frozen = vstd::modes::tracked_static_ref(cell);
    vstd::open_atomic_invariant!(inv => v => {
        line_store(LINE_READY, Ordering::Release, Tracked(&mut v.perm));
        proof {
            v.frozen = Some(frozen);
        }
    });
    Tracked(frozen)
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
    let r = vstd::raw_ptr::ptr_ref(shared_storage_ptr(), Tracked(published.shared));
    proof {
        axiom_ready(published);
        r.lemma_built_wf();
    }
    r
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
    let tracked mut seen = Seen { published: None, mark: None };
    if load_state(Tracked(&mut seen)) == READY {
        // SAFETY: `READY` was published after init with a release store.
        return unsafe { shared_ref(Tracked(seen.published.tracked_unwrap())) };
    }
    init_slow().0
}

/// The allocator state if it is ready, `None` before init has completed.
/// `free` uses this: a pointer arriving before any allocation cannot be
/// ours and must not trigger init.
#[inline(always)]
pub fn shared_if_ready() -> (r: Option<&'static Shared>)
    ensures
        r matches Some(s) ==> s.wf(),
{
    let tracked mut seen = Seen { published: None, mark: None };
    if load_state(Tracked(&mut seen)) == READY {
        // SAFETY: as in `shared`.
        Some(unsafe { shared_ref(Tracked(seen.published.tracked_unwrap())) })
    } else {
        None
    }
}

/// [`shared`], with the [`ReadyMark`] of the `READY` it loaded or
/// published, for `mallctl` and `mallctlbymib`, whose readers then take
/// [`shared_marked`]'s path, which does not abort.
#[inline(always)]
pub fn shared_published() -> (r: (&'static Shared, Tracked<&'static ReadyMark>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r.0.wf(),
        marks_ready(*r.1@),
{
    let tracked mut seen = Seen { published: None, mark: None };
    if load_state(Tracked(&mut seen)) == READY {
        // SAFETY: as in `shared`.
        return (unsafe { shared_ref(Tracked(seen.published.tracked_unwrap())) }, Tracked(seen.mark.tracked_unwrap()));
    }
    init_slow()
}

/// [`shared`] for a holder of the [`ReadyMark`]: its load reads `READY`
/// ([`load_state_marked`]), so it never reaches init and does not abort.
#[inline(always)]
pub fn shared_marked(Tracked(mark): Tracked<&'static ReadyMark>) -> (r: &'static Shared)
    requires
        marks_ready(*mark),
    ensures
        r.wf(),
{
    let tracked mut seen = Seen { published: None, mark: None };
    if load_state_marked(Tracked(&mut seen), Tracked(mark)) == READY {
        // SAFETY: as in `shared`.
        return unsafe { shared_ref(Tracked(seen.published.tracked_unwrap())) };
    }
    init_slow().0
}

/// The allocator state's address, with no readiness check and no
/// reference formed: what the fast paths hand `heap::existing_heap`, which
/// reads one byte through it only after the thread's TLS slot has proved
/// init complete. Trusted: a raw-pointer projection of a static, the
/// pointer [`Shared::storage`] names, as `shared_storage_ptr`'s: the same
/// cell of the same static (`UnsafeCell` is `repr(transparent)`), so the
/// same address and provenance.
#[verifier::external_body]
#[inline(always)]
pub fn shared_ptr() -> (r: *const Shared)
    ensures
        r == Shared::storage() as *const Shared,
    opens_invariants none
{
    (&raw const SHARED.0).cast::<Shared>()
}

/// [`shared_ptr`] for the free paths. On aarch64 the address goes through
/// an empty `asm!`, so LLVM keeps it in one register and every field the
/// free path reads (the push flags, the region geometry, the class
/// tables, the arena flag) is one `ldr` at an immediate offset from it.
/// Without that each field is its own `adrp` (and `add`) of `SHARED +
/// offset`: LLVM folds the offset into the relocation, so the page
/// addresses of two fields never CSE, and `free` paid five of them.
/// x86_64 addresses statics `rip`-relative at no cost, so there the
/// address stays a constant: `shared_storage_ptr`'s. Only the static's
/// cell is borrowed (`UnsafeCell::get`): no reference to the allocator
/// state is formed, as in [`shared_ptr`].
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn shared_base() -> (r: *const Shared)
    ensures
        r == Shared::storage() as *const Shared,
    opens_invariants none
{
    shared_storage_ptr() as *const Shared
}

/// [`shared_base`] on aarch64: [`shared_ptr`] through the empty `asm!`.
///
/// Trusted: a raw-pointer projection of a static, the pointer
/// [`Shared::storage`] names, as [`shared_ptr`] and `shared_storage_ptr`:
/// the same cell of the same static (`UnsafeCell` is `repr(transparent)`),
/// so the same address and provenance; the template is empty, so the
/// register the operand is in for input and output comes out unchanged.
#[cfg(target_arch = "aarch64")]
#[verifier::external_body]
#[inline(always)]
// The empty `asm!` is an optimization barrier on the pointer; it reads no
// memory, so `nomem` is right.
#[allow(clippy::pointers_in_nomem_asm_block)]
pub fn shared_base() -> (r: *const Shared)
    ensures
        r == Shared::storage() as *const Shared,
    opens_invariants none
{
    let mut p = shared_ptr();
    // SAFETY: an empty template; the pointer is unchanged.
    unsafe {
        core::arch::asm!("/* {0} */", inout(reg) p, options(pure, nomem, nostack, preserves_flags));
    }
    p
}

/// The allocator state at `p` (outside the tests, a [`shared_base`]
/// result), without the readiness check: `&*p` for the free paths,
/// through the permission frozen at `READY` ([`axiom_published`]).
///
/// # Safety
/// `p` points to a live, initialised `Shared` this thread can read: the
/// process's, once the calling thread has observed `READY`, or run after a
/// thread that did (through [`shared`] or [`shared_if_ready`]); or, in the
/// unit tests, a `Shared` of their own (`testutil`), which they never free.
/// A verified caller states the first case in the `requires` (the storage
/// address and [`ready`]); the tests are outside the proof.
#[inline(always)]
#[allow(clippy::let_and_return, reason = "named for the proof")]
pub unsafe fn shared_deref(p: *const Shared) -> (r: &'static Shared)
    requires
        p == Shared::storage() as *const Shared,
        ready(),
    ensures
        r.wf(),
    opens_invariants none
{
    let tracked published = axiom_published();
    let r = vstd::raw_ptr::ptr_ref(p, Tracked(published.shared));
    proof {
        r.lemma_built_wf();
    }
    r
}

/// The feature line, through `line`, its permissions frozen at
/// `LINE_READY` ([`feature_line_ready`] hands them to a thread whose
/// request returned): the line [`build_line`] wrote before the release
/// store of `LINE_READY`.
#[inline(always)]
fn published_line(Tracked(line): Tracked<&'static LineCell>) -> &'static FeatureLine
    requires
        line.built(),
    opens_invariants none
{
    vstd::raw_ptr::ptr_ref(line_ptr(), Tracked(&line.line))
}

/// Complete the probed features' THP size table if init skipped it and
/// build the feature line into [`FEATURE_LINE`] (`feature_line_ready`'s
/// build), through `line`, the permissions to the two statics that
/// `LINE_STATE`'s exchange from `LINE_UNBUILT` handed its winner
/// ([`claim_line`]): the only writer of the two statics, while readers
/// wait for `LINE_READY`.
#[inline(always)]
fn build_line(Tracked(line): Tracked<&mut LineCell>)
    requires
        crate::sys::may_syscall(),
        old(line).unbuilt(),
    ensures
        final(line).built(),
{
    let f = vstd::raw_ptr::ptr_mut_ref(features_ptr(), Tracked(&mut line.features));
    if !f.thp_sizes {
        f.thp = crate::sys::thp_host::probe(f.page_size);
        f.thp_sizes = true;
    }
    vstd::raw_ptr::ptr_mut_write(line_ptr(), Tracked(&mut line.line), FeatureLine::build(f));
}

} // verus!

/// The probed kernel features, the THP size table complete. Initialises
/// the allocator if needed.
#[cfg(test)]
pub fn features() -> &'static Features {
    feature_line_ready();
    // SAFETY: `feature_line_ready` returned, so `READY` and `LINE_READY`
    // have been observed.
    unsafe { &*features_ptr() }
}

verus! {

/// Build the feature line on its first request (once, whichever thread
/// asks first; the others wait): init skips the per-size THP files under
/// `thp:never`, which nothing but the line's `mthp` key reads, so they
/// are read here instead, and a process that never asks for the line
/// never reads them. Initialises the allocator if needed. Does not
/// allocate. Like `malloc`, not async-signal-safe: a signal handler that
/// asks for the line while its own thread is building it waits forever.
/// Hands back the line's permissions, frozen at `LINE_READY`
/// ([`LinePred`]). `challoc_features`'s restatement in
/// `theorem_read_only_never_aborts` includes its calls (checked with
/// `challoc_features`'s).
fn feature_line_ready() -> (r: Tracked<&'static LineCell>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r@.built(),
{
    // Read only by the proof: its `wf` gives `ready()`.
    let _shared = shared();
    let tracked published;
    proof {
        _shared.lemma_wf_ready();
        published = axiom_published();
    }
    line_ready(Tracked(published))
}

/// [`feature_line_ready`] after its `shared()`, whose published
/// permissions (`published`) have the line's invariant: build the line,
/// or wait for the thread building it (termination is not claimed: the
/// wait ends when the other thread's build does). Hands back the line's
/// permissions, frozen at `LINE_READY`.
#[inline(always)]
#[verifier::exec_allows_no_decreases_clause]
fn line_ready(Tracked(published): Tracked<&'static Published>) -> (r: Tracked<&'static LineCell>)
    requires
        crate::sys::may_syscall(),
        published.ok(),
    ensures
        r@.built(),
{
    let tracked inv = published.line;
    loop
        invariant
            crate::sys::may_syscall(),
            published.ok(),
            inv == published.line,
    {
        let tracked mut seen = LineSeen { frozen: None };
        if load_line(Tracked(inv), Tracked(&mut seen)) == LINE_READY {
            return Tracked(seen.frozen.tracked_unwrap());
        }
        let (won, Tracked(cell)) = claim_line(Tracked(inv));
        if won.is_ok() {
            // This thread won the exchange, so it holds the permissions to
            // the two statics, and readers wait for `LINE_READY`.
            let tracked mut cell = cell.tracked_unwrap();
            build_line(Tracked(&mut cell));
            return publish_line(Tracked(inv), Tracked(cell));
        }
        if let Some(r) = wait_line(Tracked(published)) {
            return r;
        }
    }
}

/// Wait for the thread building the line: its permissions, frozen at
/// `LINE_READY`; or `None` when the caller is a fork child's handler in
/// the fork window (`lock::in_child_fork_window`), where that thread is
/// not in the child and the wait would never end: the line is then reset
/// as the child handler would ([`feature_line_fork_child`]), for the
/// caller to build.
#[verifier::exec_allows_no_decreases_clause]
fn wait_line(Tracked(published): Tracked<&'static Published>) -> (r: Option<Tracked<&'static LineCell>>)
    requires
        crate::sys::may_syscall(),
        published.ok(),
    ensures
        r matches Some(t) ==> t@.built(),
{
    let tracked inv = published.line;
    loop
        invariant
            crate::sys::may_syscall(),
            published.ok(),
            inv == published.line,
    {
        let tracked mut seen = LineSeen { frozen: None };
        if load_line(Tracked(inv), Tracked(&mut seen)) == LINE_READY {
            return Some(Tracked(seen.frozen.tracked_unwrap()));
        }
        if crate::lock::in_child_fork_window() {
            feature_line_fork_child(Tracked(published));
            return None;
        }
        core::hint::spin_loop();
    }
}

/// The feature line (`Features`'s `Display`, built at its first request).
/// Initialises the allocator if needed.
pub fn feature_line() -> &'static str
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    let line = feature_line_ready();
    published_line(line).as_str()
}

/// `challoc_features()`: the feature report as one NUL-terminated line of
/// `key=value` pairs (design 16 and 17): the kernel probes
/// (`page_size`, `guard_install`, `mseal`, `anon_name`, ...) then `pac`.
/// The string is static, built once at its first request without
/// allocating, and identical to the `Kernel and CPU features:` line of
/// `malloc_stats_print`. Initialises the allocator if needed.
/// `theorem_read_only_never_aborts` restates its calls after `shared()`,
/// `feature_line_ready`'s included;
/// `api::restated::read_only_restatements_match_their_bodies` checks
/// that the two make the same calls.
#[unsafe(no_mangle)]
pub extern "C" fn challoc_features() -> *const core::ffi::c_char {
    proof {
        crate::sys::grant_syscalls();
        crate::sys::grant_aborts();
    }
    let line = feature_line_ready();
    published_line(line).as_ptr()
}

/// The read-only entry points make these calls, in a context where only
/// [`sys::integrity_aborts`] holds, not [`sys::may_abort`]. Each call
/// verifies, so none of these functions requires `may_abort`, and every
/// abort but an integrity check does: they abort at most at an integrity
/// check, for no argument. `get_defrag_hint` and
/// `malloc_usable_size(NULL)` are called whole; `nallocx` and
/// `challoc_features` through the calls their bodies make after
/// `shared()`, whose init may abort (it returns at once once init has
/// completed, which `shared` stands for here). `mallinfo2` (verified
/// under `may_abort`, for its `shared()`) through the calls of its body
/// that carry a contract (`stats::collect_all` and the page size); its
/// figure arithmetic (`stats::fig_add`, `stats::fig_mul`) wraps in the
/// shipped library and panics on overflow only with overflow checks on.
/// [`mallctl::utilization`], the reading behind
/// `experimental.utilization`, is called whole; the read-only keys of
/// `mallctl` and `mallctlbymib` other than `thread.*`'s are covered by
/// `mallctl::theorem_mallctl_reads_never_abort` and
/// `mallctl::theorem_mallctlbymib_reads_never_abort`. The calls listed for
/// `nallocx`, `challoc_features` and `mallinfo2` restate those bodies,
/// and Verus checks only the callees called here; the test
/// `api::restated::read_only_restatements_match_their_bodies` fails when
/// a body and its restatement differ in the names or order of their exec
/// calls (`shared()` and `mallinfo2`'s arithmetic left out). It
/// does not compare arguments or control flow.
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such function.
#[cfg(verus_keep_ghost)]
fn theorem_read_only_never_aborts(shared: &Shared, size: usize, flags: core::ffi::c_int, p: *const u8, verbose: bool)
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
    // `mallinfo2` after its `shared()`: its calls that carry a contract.
    let first = crate::stats::Scratch::new(&shared.meta);
    let _ = glibc::zero_info();
    if let Some(mut sc) = first {
        let tracked mut sd = Ghost((Seq::<crate::stats::ArenaStats>::empty(), Seq::<int>::empty()));
        // Premise (lock order), as at an entry point: it holds no
        // allocator lock.
        let tracked mut ord = axiom_entry();
        let pair = sc.record();
        crate::stats::collect_all(shared, &mut pair.a, &mut pair.b, Tracked(&mut sd), Tracked(&mut ord));
        let all = &sc.get().a;
        let page = shared.regions.page_size();
        let _ = crate::stats::fig_mul(all.pdirty, page);
        sc.free(&shared.meta);
    }
    // `challoc_features` after its `shared()`: the line's build on the
    // first request, or the wait for it.
    let tracked published;
    proof {
        shared.lemma_wf_ready();
        published = axiom_published();
    }
    let line = line_ready(Tracked(published));
    let _ = published_line(line).as_ptr();
    // `mallctl::utilization`, whole.
    let _ = mallctl::utilization(shared, p as *const core::ffi::c_void, verbose);
}

} // verus!

/// Whether init has completed (for the tests; the entry points use
/// [`shared_if_ready`]).
#[cfg(test)]
pub fn is_ready() -> bool {
    STATE.load(Ordering::Acquire) == READY
}

verus! {

/// What the winner of [`claim_init`]'s exchange takes from `STATE`'s
/// invariant: the one-shot tokens and the ghost variable's second half.
type ClaimTokens = (Map<once::Step, once::Once>, Option<GhostVar<bool>>);

/// `STATE`'s compare-exchange from `UNINIT` to `INITIALISING`, which
/// [`init_slow`] runs: the winner gets the process's one-shot tokens
/// ([`once::all_steps`]) and the second half of the ghost variable at
/// [`ready_mark`], which `STATE`'s invariant holds while it is `UNINIT`
/// ([`StatePred`]); nothing stores `UNINIT` or `INITIALISING`
/// elsewhere, so the exchange succeeds once per process and one caller
/// gets them. A caller that finds `READY` gets the published
/// permissions and the [`ReadyMark`].
#[inline(always)]
fn claim_init(Tracked(seen): Tracked<&mut Seen>) -> (r: (Result<u32, u32>, Tracked<ClaimTokens>))
    ensures
        r.0 is Ok ==> once::all_steps(r.1@.0),
        r.0 is Ok ==> (r.1@.1 matches Some(h) && h.id() == ready_mark()),
        r.0 == Err::<u32, u32>(READY) ==> final(seen).ok() && final(seen).marked(),
{
    let tracked mut steps = Map::tracked_empty();
    let tracked mut half = None;
    let won;
    vstd::open_atomic_invariant!(axiom_state() => v => {
        won = state_claim(Tracked(&mut v.perm));
        proof {
            if won is Ok {
                steps = v.steps.tracked_take();
                half = Some(v.half.tracked_take());
            }
            if won == Err::<u32, u32>(READY) {
                seen.published = v.published;
                seen.mark = v.mark;
            }
        }
    });
    (won, Tracked((steps, half)))
}

/// Publish `READY` with the permissions `init` froze ([`StatePred`]):
/// at the store, set the ghost variable at [`ready_mark`] to `true` with
/// `half`, its second half that [`claim_init`] handed over, and freeze
/// that half into the [`ReadyMark`], which comes back.
#[inline(always)]
fn publish_ready(Tracked(published): Tracked<&'static Published>, Tracked(half): Tracked<GhostVar<bool>>) -> (r: Tracked<&'static ReadyMark>)
    requires
        published.ok(),
        half.id() == ready_mark(),
    ensures
        marks_ready(*r@),
    opens_invariants [ state_ns() ]
{
    let tracked mut half = half;
    let tracked mut out = None;
    vstd::open_atomic_invariant!(axiom_state() => v => {
        state_publish(Tracked(&mut v.perm));
        proof {
            v.auth.update(&mut half, true);
            let tracked m = vstd::modes::tracked_static_ref(half);
            v.published = Some(published);
            v.mark = Some(m);
            out = Some(m);
        }
    });
    Tracked(out.tracked_unwrap())
}

/// The first caller runs `init` and publishes `READY`; a concurrent one
/// waits for it (termination is not claimed: the wait ends when the other
/// thread's `init` does); the initialising thread re-entering aborts. With
/// the [`ReadyMark`] of the `READY` it published or loaded.
#[cold]
#[inline(never)]
#[verifier::exec_allows_no_decreases_clause]
fn init_slow() -> (r: (&'static Shared, Tracked<&'static ReadyMark>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r.0.wf(),
        marks_ready(*r.1@),
{
    let tracked mut seen = Seen { published: None, mark: None };
    init_gate_enter();
    let (won, Tracked(parts)) = claim_init(Tracked(&mut seen));
    let tracked (steps, half) = parts;
    match won {
        Ok(_) => {
            INIT_TID.store(sys::gettid() as u32, Ordering::Relaxed);
            init(Tracked(steps), Tracked(&mut seen));
            let Tracked(m) = publish_ready(Tracked(seen.published.tracked_unwrap()), Tracked(half.tracked_unwrap()));
            init_gate_done();
            proof {
                seen.mark = Some(m);
            }
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
    (unsafe { shared_ref(Tracked(seen.published.tracked_unwrap())) }, Tracked(seen.mark.tracked_unwrap()))
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
/// [`FEATURE_LINE`], uninitialised, the class table
/// ([`layout::CLASSES`]), initialised, whose entries are well formed and
/// sized ([`layout::table_ok`]: its initial value is
/// [`layout::BASE_CLASSES`], `ClassTable::new`), and `LINE_STATE`'s
/// permission, at `LINE_UNBUILT`. Trusted: the statics'
/// storage and initial values, which nothing but `init`
/// ([`publish_features`], [`layout::adopt_page_geometry`]) and, after
/// `READY`, the feature line's build ([`build_line`]: `FEATURES`' THP
/// table and `FEATURE_LINE`, through the permissions the line's
/// invariant hands it) write, and that
/// `BASE_CLASSES` meets `table_ok`, which is `build_classes`'s verified
/// postcondition but which a spec cannot read off the `exec const`;
/// `LINE_STATE`'s initial value (the static's initialiser) and that
/// every write of it is [`line_compare_exchange`]'s or [`line_store`]'s,
/// inside the line's invariant, which `init` makes with this
/// permission ([`LinePred`]; the fork child's reset from `LINE_BUILDING`
/// to `LINE_UNBUILT`, [`feature_line_fork_child`], puts back the
/// permissions [`axiom_line_orphaned`] hands it); and that verified code
/// learns its value only from them and [`line_load`]; nothing writes it
/// before `init` makes that invariant (the line's requests first return
/// from `shared`, and the fork handlers are registered at `init`'s end).
/// Nothing reads [`layout::CLASSES`] before `init`'s
/// [`layout::adopt_page_geometry`] (`init`'s first statement), so the
/// table `layout::ClassTable::table` reads is the one
/// [`layout::axiom_class_table`] fixes. The token is used up, so the
/// permissions are handed out once.
axiom fn axiom_statics(tracked once: once::Once) -> (tracked r: (
    PointsTo<Features>,
    PointsTo<FeatureLine>,
    PointsTo<[layout::Class; NCLASSES]>,
    LinePerm,
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
        r.3.value() == LINE_UNBUILT,
;

/// The published features: `FEATURES`, through the permission to its
/// storage (the feature line waits for its first request,
/// [`feature_line_ready`]).
///
/// # Safety
/// Called once, by `init`, before `READY` is published.
#[inline(always)]
unsafe fn publish_features(features: Features, Tracked(feats): Tracked<&mut PointsTo<Features>>)
    requires
        old(feats).ptr() == features_storage(),
    ensures
        final(feats).ptr() == features_storage(),
        final(feats).is_init(),
{
    vstd::raw_ptr::ptr_mut_write(features_ptr(), Tracked(feats), features);
}

/// `pthread_atfork` with the three fork handlers, once per process (the
/// library's constructor or init, whichever comes first; a failure lets
/// the next try again): 0, or the error number. Nothing is claimed of the
/// result.
#[inline(never)]
fn atfork_handlers() -> i32
    requires
        crate::sys::may_syscall(),
{
    if ATFORK_REGISTERED.swap(true, Ordering::AcqRel) {
        return 0;
    }
    let rc = pthread_atfork_handlers();
    if rc != 0 {
        ATFORK_REGISTERED.store(false, Ordering::Release);
    }
    rc
}

/// The `pthread_atfork` call itself. Trusted: passes function pointers,
/// which Verus does not support.
#[verifier::external_body]
#[inline(always)]
fn pthread_atfork_handlers() -> i32 {
    // SAFETY: the three handlers are plain `extern "C"` functions that only
    // touch allocator locks.
    unsafe { ::libc::pthread_atfork(Some(fork_prepare), Some(fork_parent), Some(fork_child)) }
}

/// Record whether `fork_prepare` took the locks (`FORK_LOCKED`): a
/// relaxed store; the parent and child handlers of the same `fork` run on
/// the thread that ran the prepare handler (the premise is
/// [`axiom_fork_resumed_state`]'s, which is called only when it is set).
#[inline(always)]
fn set_fork_locked(locked: bool) {
    FORK_LOCKED.store(locked, Ordering::Relaxed);
}

/// Whether `fork_prepare` took the locks for this `fork` (`FORK_LOCKED`),
/// as [`set_fork_locked`].
#[inline(always)]
fn fork_was_locked() -> bool {
    FORK_LOCKED.load(Ordering::Relaxed)
}

/// Before init's claim: open an init under way as this thread's
/// (`GATE_IDLE` to its `sys::pthread_self`), or go on to the claim when challoc
/// is initialised (`GATE_DONE`), when this thread already holds the gate
/// (a re-entrant call: the claim finds `INITIALISING` and aborts), or when
/// a `fork` holds it and this is the forking thread; wait while another
/// thread's init or another thread's `fork` holds it. So the claim's
/// winner is the gate's holder (or the forking thread inside its fork):
/// atomics outside `STATE`'s protocol, which this only orders; nothing is
/// claimed (termination neither: the wait ends when the other thread's
/// init or the fork's parent handler does).
#[inline(never)]
#[verifier::exec_allows_no_decreases_clause]
fn init_gate_enter() {
    let me = crate::sys::pthread_self();
    loop {
        let g = match INIT_GATE.compare_exchange(GATE_IDLE, me, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(g) => g,
        };
        if g == GATE_DONE || g == me || (g == GATE_FORKING && GATE_FORKER.load(Ordering::Relaxed) == me) {
            return;
        }
        core::hint::spin_loop();
    }
}

/// Init published `READY`: the gate stays open for good (`GATE_DONE`),
/// unless the forking thread initialised inside its own `fork`
/// (`GATE_FORKING`), whose parent and child handlers then open it.
#[inline(always)]
fn init_gate_done() {
    let me = crate::sys::pthread_self();
    let _ = INIT_GATE.compare_exchange(me, GATE_DONE, Ordering::AcqRel, Ordering::Relaxed);
}

/// The prepare handler, before it looks at `STATE`: with challoc
/// initialised (`GATE_DONE`) nothing; with an init under way on another
/// thread, wait until it publishes, so the child gets the finished state;
/// with this thread's own init under way (a `fork` from a signal handler
/// that interrupted it), nothing (design 9, residuals); with none
/// (`GATE_IDLE`), close the gate (`GATE_FORKING`) so that none starts
/// until the parent and child handlers (`fork_gate_exit`). Initialises
/// nothing. The wait ends when the other thread's init does (not claimed:
/// the forking thread holds nothing init takes, since the fork handlers
/// are registered already, so init's `pthread_atfork` call returns without
/// glibc's atfork lock).
#[inline(never)]
#[verifier::exec_allows_no_decreases_clause]
fn fork_gate_enter() {
    let me = crate::sys::pthread_self();
    loop {
        let g = INIT_GATE.load(Ordering::Acquire);
        if g == GATE_IDLE {
            GATE_FORKER.store(me, Ordering::Relaxed);
            if INIT_GATE.compare_exchange(GATE_IDLE, GATE_FORKING, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                FORK_GATED.store(true, Ordering::Relaxed);
                return;
            }
        } else if g == GATE_DONE || g == GATE_FORKING || g == me {
            return;
        } else {
            core::hint::spin_loop();
        }
    }
}

/// The parent and child handlers: open the gate this `fork`'s prepare
/// closed, to `GATE_DONE` if the forking thread initialised meanwhile,
/// else `GATE_IDLE`.
#[inline(never)]
fn fork_gate_exit() {
    if FORK_GATED.swap(false, Ordering::Relaxed) {
        let open = if shared_if_ready().is_some() { GATE_DONE } else { GATE_IDLE };
        GATE_FORKER.store(0, Ordering::Relaxed);
        INIT_GATE.store(open, Ordering::Release);
    }
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

/// Design section 10 steps 1 to 8 (step 7, the cgroup discovery, is set
/// up inside `Shared::init`: run there under `thp:always`, else deferred,
/// `sys::cgroup::Deferred`). Configuration (step 2) is read before the
/// regions are reserved so that nothing it may abort on has been mapped
/// yet; it reads only the weak symbols, one file and `environ`. The
/// `Shared` it builds satisfies [`Shared::wf`] (`Shared::init`'s
/// contract, from `Regions::reserve_owning` and `config::load`), and the
/// metadata allocator owns the metadata region's address space, which
/// `reserve_owning` hands over; `init_slow` runs it once, before it
/// publishes `READY`, with the one-shot tokens its winning exchange got
/// ([`claim_init`]): one for the statics it writes here, the class table
/// and the features ([`axiom_statics`]; the feature line's permission and
/// `LINE_STATE`'s go into the line's invariant with the features', the
/// line waits for its first request, [`feature_line_ready`]),
/// and the rest for `Shared::init`, which uses them up. It hands back the
/// permissions to the `Shared` and the class table, frozen, and the
/// line's invariant
/// ([`Published`]).
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
    let tracked (mut feats, line, mut classes, line_state) = axiom_statics(statics_once);
    // The slab geometry of the classes whose table slab is below the page
    // (design 3, page-sized slabs): written once here, before any other
    // thread can see the allocator, never again.
    // SAFETY: `init` runs once, under `INITIALISING`, before `READY`.
    unsafe { layout::adopt_page_geometry(crate::sys::features::page_size().trailing_zeros(), Tracked(&mut classes)) };
    let config = config::load();
    // The per-size THP files only matter to `thp:auto` and `thp:always`;
    // under `never` they wait for the feature line (`feature_line_ready`).
    let features = Features::probe(config.thp != config::Thp::Never);
    proof {
        assert(0x1_0000usize <= (1usize << 33)) by (bit_vector);
    }
    crate::thp::configure(config.thp, &features);
    purge::configure_advice(config.lazy_band);
    purge::set_check_zero(config.check_zero);
    // The unit-test process shares this allocator between tests that
    // assert on the thread's presence; the lazy start is tested on its own
    // (`purge::tests`, the C programs). `config.background_thread.or(Some(false))`
    // is spelled as a `match`: vstd does not specify `Option::or`.
    purge::background::configure(if cfg!(test) {
        match config.background_thread {
            Some(b) => Some(b),
            None => Some(false),
        }
    } else {
        config.background_thread
    });
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
            sys::abort("cannot set up the allocator tables: metadata region too small, or the kernel refused their commit (RLIMIT_DATA, vm.max_map_count)");
        }
        let tracked shared_perm = built.perm.tracked_unwrap();
        // Design 11: `Shared::init` forced the stored `thp` to `never`
        // under a cgroup limit below 1 GiB; the advice follows.
        if vstd::raw_ptr::ptr_ref(shared_storage_ptr(), Tracked(shared_perm)).policy.thp_forced() {
            crate::thp::force_never();
        }
        publish_features(features, Tracked(&mut feats));
        proof {
            // The line's invariant, at `LINE_UNBUILT` with the published
            // features and the line's storage, for its first request.
            let tracked cell = LineCell { features: feats, line };
            let tracked line_inv = LineInv::new((), LineVal { perm: line_state, cell: Some(cell), frozen: None }, line_ns());
            let tracked classes = vstd::modes::tracked_static_ref(classes);
            layout::axiom_class_table(classes);
            let tracked p = Published {
                shared: shared_perm,
                classes,
                line: vstd::modes::tracked_static_ref(line_inv),
            };
            seen.published = Some(vstd::modes::tracked_static_ref(p));
        }
    }
    register_fork_handlers();
}

} // verus!

verus! {

/// A lock the fork handlers take, named by its place in the allocator:
/// the list in [`fork_prepare`]'s body (design 9). The test
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

/// A class lock of an arena in `a`.
pub open spec fn class_part(k: ForkLock, a: Set<int>) -> bool {
    match k {
        ForkLock::Class(i, c) => a.contains(i) && 0 <= c < NCLASSES,
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
/// found), the state lock aside: the explicit-cache table and arena
/// creation locks, every class and slab-set lock of every arena, every
/// slab table lock, the huge, metadata and process statistics locks, and
/// every arena's statistics lock.
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

/// The hold of the fork lock list:
/// the arenas `fork_prepare` found, `n` the arena count it read, and one
/// [`LockHeld`] per listed lock, each the hold of the lock its key names,
/// the mask its holder records as stored in [`FORK_SIGMASK`] (`saved`)
/// and the address of that word (`slot_addr`), with, in the
/// release's hold, the permission to that word (`slot`).
/// It also keeps the permission of the creation lock's cell (`created`,
/// from `Arenas::lock_held`), whose value's half of the registry's
/// snapshot is `n` and `arenas` ([`ForkHeld::wf`]), so the count and the
/// table cannot change while it is kept ([`arena_held`], [`count_held`]).
/// Built, by convention, only by `fork_prepare` as it takes the locks
/// and by [`resume::axiom_fork_resumed`] for the release: the fields are
/// private to `api` (its submodules can see them too).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct ForkHeld {
    ghost n: int,
    ghost arenas: Map<int, Arena>,
    tracked toks: Map<ForkLock, LockHeld>,
    ghost saved: Option<u64>,
    ghost slot_addr: usize,
    tracked slot: Option<WordPerm>,
    tracked created: crate::arena::CreatedPerm,
}

impl ForkHeld {
    /// The permission of the creation lock's cell the hold keeps.
    pub closed spec fn created(self) -> crate::arena::CreatedPerm {
        self.created
    }

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

    /// The content of [`FORK_SIGMASK`] as this hold's holder records it
    /// ([`save_sigmask`] records its argument), `None` before. Part of the
    /// hold because the slot is written and read only under every lock.
    pub closed spec fn saved(self) -> Option<u64> {
        self.saved
    }

    /// The address of the word this hold's holder records as storing its
    /// [`saved`](ForkHeld::saved) content ([`save_sigmask`]: the word
    /// of [`FORK_SIGMASK`]); meaningful once that is `Some`.
    pub closed spec fn slot_addr(self) -> usize {
        self.slot_addr
    }

    /// The hold has the permission to the word it records
    /// ([`slot_addr`](ForkHeld::slot_addr)), and its value is the content
    /// the hold records: the release's hold, as [`resume::axiom_fork_resumed`]
    /// hands it over (`fork_prepare` stores without one, under every
    /// lock).
    pub closed spec fn has_slot(self) -> bool {
        self.slot matches Some(p) && p.addr() == self.slot_addr && self.saved == Some(p.value())
    }

    /// The hold has no permission to [`FORK_SIGMASK`]: the prepare's hold,
    /// the only one [`save_sigmask`] writes under, so the slot is never
    /// written while its permission exists.
    pub closed spec fn no_slot(self) -> bool {
        self.slot is None
    }

    /// This hold after [`save_sigmask`] stored `m` in the word at `a`.
    pub closed spec fn with_saved(self, m: u64, a: usize) -> ForkHeld {
        ForkHeld { saved: Some(m), slot_addr: a, ..self }
    }

    /// Storing in the slot changes neither the locks held nor the arenas.
    proof fn lemma_with_saved(self, s: Shared, m: u64, a: usize)
        ensures
            self.wf(s) ==> self.with_saved(m, a).wf(s),
            self.all() ==> self.with_saved(m, a).all(),
            self.with_saved(m, a).saved() == Some(m),
    {
        let t = self.with_saved(m, a);
        assert(t.keys() == self.keys());
        assert(t.arenas() == self.arenas());
        assert(t.n() == self.n());
        assert(t.created() == self.created());
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
            final(self).created() == old(self).created(),
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
            final(self).created() == old(self).created(),
    {
        self.toks.tracked_remove(k)
    }

    /// `n` is a count `fork_prepare` read (a `u32`), each token is the hold
    /// of the lock its key names in `s`, and the arenas are indices below
    /// `n`; the creation lock's cell permission is `s`'s registry's, and
    /// its value's half of the snapshot is `n` and the arenas
    /// (`Arenas::frozen`).
    pub(crate) open spec fn wf(self, s: Shared) -> bool {
        &&& 0 <= self.n() <= u32::MAX
        &&& forall|i: int| #[trigger] self.arenas().contains_key(i) ==> 0 <= i < self.n()
        &&& s.arenas.frozen(self.created())
        &&& self.created().value().snap().0 as int == self.n()
        &&& self.created().value().snap().1 == self.arenas()
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
/// and nothing released a lock in between (the handlers call this only
/// when `FORK_LOCKED` says this fork's prepare took the locks: glibc
/// serialises one fork's handlers under its atfork lock and runs them on
/// the forking thread, so a `true` read there is this fork's; a stale
/// `true` would mint a hold that does not exist): signals are blocked from the
/// prepare handler's first step, and the atfork handlers registered
/// before ours (whose prepare runs after ours and whose parent and
/// child handlers run before ours) release none: an allocation of theirs
/// takes the held locks as acquired and its releases do nothing
/// (`lock::fork_holder_is_me`, whose premise is that the forking thread
/// has no allocator critical section open in the window). Ghost
/// state does not cross the C calls, so the hold is handed over here:
/// the state lock's token, then [`resume::axiom_fork_resumed`] for the rest, at
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

/// The signal half of the fork hold: `sig` (the calling thread's mask)
/// has every blockable signal blocked, and the hold records `orig` as
/// saved (`held.saved()`), a mask the kernel can hold. `fork_prepare` ends with
/// it, `orig` being the mask in force at its entry, and the release
/// starts from it.
pub(crate) open spec fn fork_sig_holds(sig: SigMask, held: ForkHeld, orig: u64) -> bool {
    &&& sig.wf()
    &&& sig.mask() == all_blocked()
    &&& held.saved() == Some(orig)
    &&& effective(orig) == orig
    &&& orig & !crate::sys::BLOCKABLE_SIGNALS == 0
}

/// Premise (the fork handler handoff, its signal half; see
/// [`axiom_fork_resumed_state`]): the parent and child handlers run on
/// the thread whose prepare handler returned holding [`fork_sig_holds`]
/// (in the child, its copy, which inherits the mask), whose mask has
/// stayed as that handler left it, every blockable signal blocked:
/// glibc's `_Fork` restores the mask it found, and the atfork handlers
/// registered before ours (whose prepare runs after ours and whose parent
/// and child handlers run before ours) leave the mask as they found it.
/// The slot's content is [`resume::axiom_fork_resumed`]'s. Each handler calls
/// this once, which nothing enforces.
axiom fn axiom_fork_resumed_sig() -> (tracked sig: SigMask)
    ensures
        sig.mask() == all_blocked(),
;

/// `s.arenas.get_existing(i as u16)` while `held` holds the creation
/// lock: the arena `fork_prepare` found at `i`. `held` keeps the
/// permission of the creation lock's cell, whose value's half of the
/// registry's snapshot is the count and the arenas `fork_prepare` read
/// ([`ForkHeld::wf`]); `Arenas::create` and `Arenas::build` change the
/// count and the table only with that half, so a read now returns what
/// that read did (`Arenas::get_frozen`). The hold must also have the
/// creation lock's token (required), so a scan runs only while the lock
/// is held, before the release hands the token and the cell permission
/// back together (`Arenas::unlock_held`).
#[inline(always)]
fn arena_held<'a>(s: &'a Shared, i: usize, Tracked(held): Tracked<&ForkHeld>) -> (r: Option<&'a Arena>)
    requires
        held.wf(*s),
        held.keys().contains(ForkLock::Arenas),
        held.lock(ForkLock::Arenas) == s.spec_fork_locks().1,
        i < held.n(),
    ensures
        r is Some <==> held.arenas().contains_key(i as int),
        r matches Some(a) ==> *a == held.arenas()[i as int],
{
    proof {
        s.arenas.lemma_frozen(held.created);
    }
    s.arenas.get_frozen(i as u16, Tracked(&held.created))
}

/// `s.arenas.count()` while `held` holds the creation lock: the count
/// `fork_prepare` read, as [`arena_held`] (`Arenas::count_frozen`).
#[inline(always)]
fn count_held(s: &Shared, Tracked(held): Tracked<&ForkHeld>) -> (r: u32)
    requires
        held.wf(*s),
        held.keys().contains(ForkLock::Arenas),
        held.lock(ForkLock::Arenas) == s.spec_fork_locks().1,
    ensures
        r as int == held.n(),
{
    s.arenas.count_frozen(Tracked(&held.created))
}

/// Block signals, then take every allocator lock so the child inherits a
/// consistent heap and no signal handler on this thread can re-enter the
/// allocator (or fork again) while the locks are held.
///
/// The order is the nesting order (design 9): the explicit-cache
/// table lock is held across heap creation and destruction, which take the
/// arena creation lock, class locks and the metadata lock; the process
/// statistics lock is held across `epoch`'s scan, which takes the class,
/// slab table, huge, metadata and per-arena statistics locks
/// (`stats.rs`); `take_group` holds a class lock and its slab table lock
/// while it takes the arena's slab-set lock, so every slab table lock
/// comes before every slab-set lock; the per-arena statistics locks are
/// leaves and the background thread's state lock comes last. The tests
/// `fork_lock_list_matches_the_source_tree` (the list of locks, from the
/// source tree) and `fork_handlers_take_and_release_every_lock` (that the
/// handlers take each one) fail until a new `Lock` is added here.
///
/// Fork safety, first half: it returns holding [`fork_holds`], one token
/// per lock of the fork lock list (asserted at the end). It also ends
/// with [`fork_sig_holds`]: every blockable signal blocked since before
/// its first acquisition (each loop's invariant), and the mask in force
/// at its entry recorded in the hold as saved (`held.saved()`). That the
/// slot holds that mask rests on the code, not the proof: the store in
/// [`save_sigmask`] is unchecked, and the release's link back to it is
/// the premise of [`resume::axiom_fork_resumed`].
///
/// Lock order: every acquisition ranks above every lock taken before it
/// ([`crate::lock::LockOrder`]).
extern "C" fn fork_prepare() {
    proof {
        crate::sys::grant_syscalls();
        crate::sys::grant_aborts();
    }
    // An init under way on another thread finishes first; with none, none
    // starts until the parent and child handlers (the gate).
    fork_gate_enter();
    // Registered by the library's constructor, before init: until challoc
    // is initialised there is nothing to lock (nor to initialise for a
    // `fork`).
    if shared_if_ready().is_none() {
        set_fork_locked(false);
        return;
    }
    set_fork_locked(true);
    // Premise (lock order): the thread that calls `fork` holds no
    // allocator lock (no allocator code forks; a `fork` from a signal
    // handler that interrupted allocator code holding a lock is outside
    // the C contract; and the prepare handlers that run before ours have
    // returned).
    let tracked mut ord = crate::lock::axiom_fork_entry();
    crate::stats::pool_fork_prepare();
    // A deferred cgroup discovery (design 11) runs now, before the locks,
    // whatever the deferral (in the record's cell, with little stack of
    // its own): a process that forks (a PostgreSQL postmaster, a prefork
    // server's workers) reads the files once and its children inherit the
    // record, where each child would otherwise read them again on its own
    // first tick.
    // It does not wait for a discovery another thread has under way (nor
    // for one this thread's own interrupted code is running, when `fork`
    // is called from a signal handler): the child then starts that one
    // over (`fork_child`), and the parent's finishes as usual.
    shared().discover_cgroup_for_fork(Tracked(&mut ord));
    // Premise: `sig` is this thread's mask (`sys::axiom_thread_mask`).
    let tracked mut sig = sys::axiom_thread_mask();
    let ghost orig = sig.mask();
    let mask = sys::block_all_signals(Tracked(&mut sig));
    let s = shared();
    let Tracked(t) = s.tcaches_lock().lock_held(Tracked(&mut ord));
    let tracked mut toks: Map<ForkLock, LockHeld> = Map::tracked_empty();
    proof {
        toks.tracked_insert(ForkLock::Tcaches, t);
    }
    // With the permission of the creation lock's cell, which the hold
    // keeps: the count and the table stay as read below until the release.
    let Tracked(tcp) = s.arenas.lock_held(Tracked(&mut ord));
    let tracked (t, cp) = tcp;
    proof {
        s.lemma_arenas_lock();
        toks.tracked_insert(ForkLock::Arenas, t);
        s.arenas.lemma_frozen(cp);
    }
    let Tracked(t) = s.stats_lock().lock_held(Tracked(&mut ord));
    proof {
        toks.tracked_insert(ForkLock::Stats, t);
    }
    let ghost mut arenas: Map<int, Arena> = Map::empty();
    // Each `for a in s.arenas.iter()` is spelled as the scan `Arenas::iter`
    // makes, through `Arenas::get_frozen` (`get_existing` with the
    // creation lock's half; Verus does not specify the iterator), and
    // `a.classes[c]` as `Arena::class`, as in `fork_release`.
    let n = s.arenas.count_frozen(Tracked(&cp)) as usize;
    for i in 0..n
        invariant
            crate::sys::may_syscall(),
            sig.wf(),
            sig.mask() == all_blocked(),
            n <= crate::arena::MAX_ARENAS,
            s.arenas.frozen(cp),
            n == cp.value().snap().0,
            forall|j: int| #[trigger] arenas.contains_key(j) ==> arenas[j] == cp.value().snap().1[j],
            forall|j: int| #[trigger] arenas.contains_key(j) ==> cp.value().snap().1.contains_key(j),
            forall|j: int| 0 <= j < i && #[trigger] cp.value().snap().1.contains_key(j) ==> arenas.contains_key(j),
            forall|j: int| #[trigger] arenas.contains_key(j) ==> 0 <= j < i,
            forall|j: int| #[trigger] arenas.contains_key(j) ==> arenas[j].spec_index() as int == j,
            forall|k: ForkLock| #[trigger] toks.contains_key(k) <==> (k == ForkLock::Tcaches || k
                == ForkLock::Arenas || k == ForkLock::Stats || class_part(k, arenas.dom())),
            forall|k: ForkLock| #[trigger]
                toks.contains_key(k) ==> toks[k].held() == lock_at(*s, arenas, k),
            ord.root(),
            ord.top() < rank_class(i as int, 0),
    {
        if let Some(a) = s.arenas.get_frozen(i as u16, Tracked(&cp)) {
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
                    s.arenas.frozen(cp),
                    n == cp.value().snap().0,
                    cp.value().snap().1.contains_key(i as int),
                    *a == cp.value().snap().1[i as int],
                    forall|j: int| #[trigger] before.contains_key(j) ==> before[j] == cp.value().snap().1[j],
                    forall|j: int| #[trigger] before.contains_key(j) ==> cp.value().snap().1.contains_key(j),
                    forall|j: int| 0 <= j < i && #[trigger] cp.value().snap().1.contains_key(j) ==> before.contains_key(j),
                    forall|j: int| #[trigger] before.contains_key(j) ==> 0 <= j < i,
                    forall|k: ForkLock| #[trigger] toks.contains_key(k) <==> (k == ForkLock::Tcaches
                        || k == ForkLock::Arenas || k == ForkLock::Stats || class_part(k, before.dom()) || (
                    k matches ForkLock::Class(j, d) && j == i && 0 <= d < c)),
                    forall|k: ForkLock| #[trigger]
                        toks.contains_key(k) ==> toks[k].held() == lock_at(*s, arenas, k),
                    ord.root(),
                    ord.top() < rank_class(i as int, c as int),
            {
                let Tracked(t) = a.class(c).raw_lock().lock_held(Tracked(&mut ord));
                proof {
                    toks.tracked_insert(ForkLock::Class(i as int, c as int), t);
                }
            }
            proof {
                assert forall|k: ForkLock| #[trigger] toks.contains_key(k) <==> (k
                    == ForkLock::Tcaches || k == ForkLock::Arenas || k == ForkLock::Stats
                    || class_part(k, arenas.dom())) by {
                    if let ForkLock::Class(j, d) = k {
                        if j == i as int {
                            assert(class_part(k, arenas.dom()) <==> 0 <= d < NCLASSES);
                        }
                    }
                }
            }
        }
    }
    proof {
        s.arenas.lemma_frozen(cp);
        assert(arenas =~= cp.value().snap().1);
    }
    let tracked mut held = ForkHeld { n: n as int, arenas, toks, saved: None, slot_addr: 0, slot: None, created: cp };
    for c in 0..NCLASSES
        invariant
            crate::sys::may_syscall(),
            sig.wf(),
            sig.mask() == all_blocked(),
            held.wf(*s),
            held.no_slot(),
            n == held.n(),
            held.n() <= crate::arena::MAX_ARENAS,
            forall|j: int| #[trigger] held.arenas().contains_key(j) ==> held.arenas()[j].spec_index() as int == j,
            forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || k == ForkLock::Stats || class_part(k, held.arenas().dom())
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
    // `take_group` takes an arena's slab-set lock under a class lock and
    // that class's slab table lock, so the slab-set locks come after
    // every slab table lock.
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
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || k == ForkLock::Stats || class_part(k, held.arenas().dom())
                        || table_part(k) || (k matches ForkLock::Sets(j) && j < i
                        && held.arenas().contains_key(j))
            ),
            ord.root(),
            ord.top() < rank_sets(i as int),
            s.wf(),
    {
        if let Some(a) = arena_held(s, i, Tracked(&held)) {
            let Tracked(t) = a.sets().raw_lock().lock_held(Tracked(&mut ord));
            proof {
                held.put(ForkLock::Sets(i as int), t);
            }
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
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || k == ForkLock::Stats || arena_part(k, held.arenas().dom())
                        || table_part(k) || k == ForkLock::Huge || k == ForkLock::Meta || (k matches ForkLock::ArenaStats(j) && j < i
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
    // The fork window: the atfork handlers registered before ours run on
    // this thread with every lock held (their prepare after this, their
    // parent and child handlers before our release), and their allocations
    // take the held locks as acquired (`lock::fork_holder_is_me`). It
    // opens after the last acquisition (`lock::fork_prepared`, which only
    // the state lock's `lock_held` gives).
    crate::lock::fork_window_open();
    proof {
        before.lemma_with_saved(*s, mask, held.slot_addr());
        assert(fork_holds(*s, held, state));
        assert(fork_sig_holds(sig, held, orig));
    }
}

/// Save the signal mask for `fork_release`, recorded in the hold
/// (`held.saved()`). Its requirement is the [`FORK_SIGMASK`] protocol:
/// the caller holds every allocator lock (ghost), so the hold that
/// records the content is the one exclusive hold of the slot; and the
/// hold has no permission to the slot ([`ForkHeld::no_slot`]), so no
/// permission goes stale. The store is unchecked: it goes through no
/// permission, and `saved` records the argument `mask`, not the word
/// written, so nothing the proof checks ties the two (a store of any
/// other value would verify).
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
        *final(held) == old(held).with_saved(mask, final(held).slot_addr()),
{
    let slot = fork_slot();
    slot.store(mask, Ordering::Relaxed);
    proof {
        held.saved = Some(mask);
        held.slot_addr = word_addr(slot);
    }
}

/// The signal mask `fork_prepare` saved, the content the hold records:
/// read from `w` ([`FORK_SIGMASK`], the word the hold records) through
/// the hold's permission to it
/// ([`ForkHeld::has_slot`]), which only a holder of every allocator lock
/// has (required, ghost). The value is still the one this thread's
/// prepare handler saved: [`save_sigmask`] requires that hold too, so a
/// second thread's `fork_prepare` (glibc serialises the handlers, the
/// unit tests do not) stores only after it has taken every lock, so
/// after this release's unlocks.
#[inline(always)]
fn saved_sigmask(
    w: &AtomicU64,
    Ghost(s): Ghost<Shared>,
    Tracked(held): Tracked<&ForkHeld>,
    Tracked(state): Tracked<&LockHeld>,
) -> (r: u64)
    requires
        fork_holds(s, *held, *state),
        held.has_slot(),
        held.slot_addr() == word_addr(w),
    ensures
        held.saved() == Some(r),
{
    let tracked slot = held.slot.tracked_borrow();
    crate::vspec::word::load(w, Ordering::Relaxed, Tracked(slot))
}

/// Release the locks taken by `fork_prepare`, in reverse order, then let
/// signals through again.
///
/// Fork safety, second half: it starts from the hold `fork_prepare`
/// ended with ([`fork_holds`]; `state` is the state lock's token) and
/// releases each listed lock once, by handing its token back to
/// `RawLock::unlock_held`; it ends holding none (asserted at the end).
///
/// The signal mask: it
/// starts from [`fork_sig_holds`], reads the slot under every lock, keeps
/// every blockable signal blocked through its last unlock, and ends with
/// the thread's mask the slot's content (asserted at the end).
fn fork_release(Tracked(state): Tracked<LockHeld>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        state.held() == purge::background::state_lock(),
{
    // The window ends before the first release, which must then unlock
    // (`lock::window_closed`, which every token release requires).
    crate::lock::fork_window_close();
    let s = shared();
    let (slot, Tracked(mut held)) = resume::resumed(s, Tracked(&state));
    let tracked mut sig = axiom_fork_resumed_sig();
    let ghost saved = held.saved()->Some_0;
    proof {
        crate::sys::lemma_effective_idempotent(crate::sys::BLOCKABLE_SIGNALS);
        assert(fork_sig_holds(sig, held, saved));
    }
    // Written by the `fork_prepare` that paired with this release, and
    // still ours because every lock is held until the unlocks below.
    let mask = saved_sigmask(slot, Ghost(*s), Tracked(&held), Tracked(&state));
    purge::background::fork_unlock(Tracked(state));
    // Arenas cannot appear while the creation lock is held, so this is the
    // same set `fork_prepare` locked (what `arena_held` and `count_held`
    // prove from the hold's cell permission), in reverse. Each `for i in
    // (0..n).rev()` is spelled as a countdown: Verus does not specify
    // `Rev`.
    // The class states are read through `Arena::class`, the verified
    // accessor of `a.classes[c]`.
    let n = count_held(s, Tracked(&held)) as usize;
    let mut i = n;
    while i > 0
        invariant
            crate::sys::may_syscall(),
            crate::lock::window_closed(),
            sig.wf(),
            sig.mask() == all_blocked(),
            i <= n == held.n(),
            held.wf(*s),
            forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || k == ForkLock::Stats || arena_part(k, held.arenas().dom())
                        || table_part(k) || k == ForkLock::Huge || k == ForkLock::Meta || (k matches ForkLock::ArenaStats(j) && j < i
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
    let tracked t = held.take(ForkLock::Meta);
    s.meta_lock().unlock_held(Tracked(t));
    let tracked t = held.take(ForkLock::Huge);
    s.huge_lock().unlock_held(Tracked(t));
    let mut i = n;
    while i > 0
        invariant
            crate::sys::may_syscall(),
            crate::lock::window_closed(),
            sig.wf(),
            sig.mask() == all_blocked(),
            i <= n == held.n(),
            held.wf(*s),
            forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || k == ForkLock::Stats || class_part(k, held.arenas().dom())
                        || table_part(k) || (k matches ForkLock::Sets(j) && j < i
                        && held.arenas().contains_key(j))
            ),
        decreases i,
    {
        i -= 1;
        if let Some(a) = arena_held(s, i, Tracked(&held)) {
            let tracked t = held.take(ForkLock::Sets(i as int));
            a.sets().raw_lock().unlock_held(Tracked(t));
        }
    }
    let mut c = NCLASSES;
    while c > 0
        invariant
            crate::sys::may_syscall(),
            crate::lock::window_closed(),
            sig.wf(),
            sig.mask() == all_blocked(),
            c <= NCLASSES,
            n == held.n(),
            held.wf(*s),
            forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || k == ForkLock::Stats || class_part(k, held.arenas().dom())
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
            crate::lock::window_closed(),
            sig.wf(),
            sig.mask() == all_blocked(),
            i <= n == held.n(),
            held.wf(*s),
            forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                    k == ForkLock::Tcaches || k == ForkLock::Arenas || k == ForkLock::Stats || class_part(
                        k,
                        held.arenas().dom().filter(|j: int| j < i),
                    )
            ),
        decreases i,
    {
        i -= 1;
        if let Some(a) = arena_held(s, i, Tracked(&held)) {
            let mut c = NCLASSES;
            while c > 0
                invariant
                    crate::sys::may_syscall(),
                    crate::lock::window_closed(),
                    sig.wf(),
                    sig.mask() == all_blocked(),
                    c <= NCLASSES,
                    i < n == held.n(),
                    held.arenas().contains_key(i as int),
                    *a == held.arenas()[i as int],
                    held.wf(*s),
                    forall|k: ForkLock| #[trigger] held.keys().contains(k) <==> (
                            k == ForkLock::Tcaches || k == ForkLock::Arenas || k == ForkLock::Stats || class_part(
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
    let tracked t = held.take(ForkLock::Stats);
    s.stats_lock().unlock_held(Tracked(t));
    let tracked t = held.take(ForkLock::Arenas);
    let tracked tt = held.take(ForkLock::Tcaches);
    proof {
        assert(held.keys() =~= Set::empty());
        s.lemma_arenas_lock();
    }
    // The creation lock's cell permission goes back with its token: the
    // hold has no lock left.
    let tracked cp = held.created;
    s.arenas.unlock_held(Tracked(t), Tracked(cp));
    s.tcaches_lock().unlock_held(Tracked(tt));
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
    fork_gate_exit();
    if !fork_was_locked() {
        return;
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
    fork_gate_exit();
    if !fork_was_locked() {
        return;
    }
    let tracked mut state = axiom_fork_resumed_state();
    purge::background::fork_child(Tracked(&mut state));
    let s = shared();
    proof {
        // The child's fork handler is where `in_fork_child` holds: only the
        // forking thread runs in the child.
        reveal(crate::sys::cgroup::in_fork_child);
    }
    s.cgroup_fork_child();
    crate::stats::pool_fork_child();
    let tracked published;
    proof {
        s.lemma_wf_ready();
        published = axiom_published();
    }
    feature_line_fork_child(Tracked(published));
    fork_release(Tracked(state));
    // The dirty bytes this child inherited, the lazy start's baseline: read
    // after the release (the sum takes the class and huge locks), while
    // the child is still single-threaded, so nothing has moved them. Only
    // under the lazy start: nothing else reads it.
    if purge::background::auto() {
        // Premise (lock order): the release above handed back every lock
        // this thread held.
        let tracked mut ord = axiom_entry();
        purge::background::set_fork_base(crate::policy::empty_bytes(shared(), Tracked(&mut ord)));
    }
}

/// The child of a `fork` has only the forking thread: a feature line
/// another thread was building is not coming, so the child's first
/// request builds it again rather than waiting forever. Called by the
/// child's fork handler (`fork_child`) and, in the child's fork window
/// before it, by [`wait_line`] (`lock::in_child_fork_window`), where only
/// the forking thread runs too. The reset puts
/// back the permissions to the two statics, which that thread held
/// ([`axiom_line_orphaned`]). (If the forking thread itself was building
/// it, from a signal handler that forked, it finishes when the handler
/// returns; a second build in between writes the same bytes when the
/// THP files read the same both times, and can race with it if the child
/// has started another thread: the residue that axiom states.)
fn feature_line_fork_child(Tracked(published): Tracked<&'static Published>)
    requires
        published.ok(),
        crate::sys::cgroup::in_fork_child(),
{
    let tracked inv = published.line;
    vstd::open_atomic_invariant!(inv => v => {
        let ghost before = v.perm;
        let _reset = line_compare_exchange(LINE_BUILDING, LINE_UNBUILT, Ordering::Relaxed, Ordering::Relaxed, Tracked(&mut v.perm));
        proof {
            if _reset is Ok {
                v.cell = Some(axiom_line_orphaned(before, &v.perm, &v.cell));
            }
        }
    });
}

/// Premise (the fork child's orphaned line): in the child of a `fork`,
/// a line whose state the child's reset found `LINE_BUILDING` (`before`,
/// `LINE_STATE`'s permission just before the reset, and `perm`, just
/// after it, at `LINE_UNBUILT`) was being built by a thread of the
/// parent, which holds the permissions to [`FEATURES`] and
/// [`FEATURE_LINE`] between its winning exchange and its `LINE_READY`
/// store ([`claim_line`], [`publish_line`]), and they satisfy
/// [`LineCell::unbuilt`] throughout (the build writes the features
/// through `ptr_mut_ref`, which keeps them initialised). The child has
/// only the forking thread, so when the builder was another thread, no
/// thread of the child holds those permissions, and the reset takes
/// them. Called only by [`feature_line_fork_child`], right after its
/// successful reset, which only the child's fork handler runs
/// ([`fork_child`]; the test `fork_child_resets_are_child_only` checks
/// the call sites).
///
/// The residue this does not cover: when the builder was the forking
/// thread itself (a signal handler that forked while its thread was
/// building the line, api/mod.rs's `line_ready`), the permissions exist
/// twice in the child, the reset's and the interrupted build's. The
/// child's own request may then build the line and publish
/// `LINE_READY`, and the interrupted build, resumed when the handler
/// returns, completes the features' THP table and writes the line a
/// second time and stores `LINE_READY` again: a line a reader of the
/// first `LINE_READY` may hold a reference into is overwritten, with the
/// same bytes when the THP files read the same both times. The two
/// builds run one after the other when both are the forking thread's;
/// if the child has started another thread by then (the handler
/// allocates and starts the lazy background thread, say) and that thread
/// requests the line, they can run concurrently, a data race on the two
/// statics. And the child's copy of `FEATURES` may hold a half-done
/// update of the THP table (`thp` written, `thp_sizes` not, or the
/// reverse: plain stores the compiler may reorder), so the child's build
/// may skip the probe and print a mixed table.
///
/// The requirements pin the call to the state right after that reset: the
/// caller is the child's fork handler (`sys::cgroup::in_fork_child`), and
/// the invariant's slot for the permissions is empty (`cell`, which
/// [`LinePred`] keeps empty at `LINE_BUILDING`), so the permissions it
/// hands out are the only ones the invariant can hold.
axiom fn axiom_line_orphaned(before: LinePerm, tracked perm: &LinePerm, tracked cell: &Option<LineCell>) -> (tracked c: LineCell)
    requires
        crate::sys::cgroup::in_fork_child(),
        cell is None,
        before.value() == LINE_BUILDING,
        perm.value() == LINE_UNBUILT,
    ensures
        c.unbuilt(),
;

} // verus!

/// The release's handoff of the fork hold, in a module of its own: its
/// axiom is private here, so the only code that can mint the permission
/// to the signal-mask slot is [`resumed`](resume::resumed), which mints
/// it for [`fork_slot`]'s word and no other.
mod resume {
    #[cfg(verus_keep_ghost)]
    use super::fork_holds;
    use super::{ForkHeld, fork_slot};
    use crate::heap::Shared;
    use crate::lock::LockHeld;
    #[cfg(verus_keep_ghost)]
    use crate::{purge, sys::effective, vspec::word::word_addr};
    use core::sync::atomic::AtomicU64;
    #[cfg(verus_keep_ghost)]
    use vstd::prelude::*;
    use vstd::prelude::{Tracked, verus};

    verus! {

    /// Premise (see `api::axiom_fork_resumed_state`): the rest of the
    /// hold, with the permission to `slot`, which must be the word the
    /// prepare stored its mask to: [`resumed`] passes [`fork_slot`]'s
    /// `&FORK_SIGMASK`, as the prepare's store did, and a static has one
    /// address (the same premise as for `s`, the `Shared` the release
    /// reads); passed any other word, it would mint a second permission
    /// to it, which is why it is private to this module. The hold has the
    /// arena count and the arenas `fork_prepare` read and the mask it
    /// saved in `FORK_SIGMASK` (`n`, `arenas` and `saved` are those reads
    /// and that store, which the ensures does not name: the premise is
    /// that they are; the slot's content is a mask the kernel held, and
    /// only a holder of every lock writes it, see `save_sigmask`), and the
    /// permission to the slot whose value is that content
    /// ([`ForkHeld::has_slot`]): the one trusted link between the slot and
    /// the hold, through which `saved_sigmask` reads (no other permission
    /// to the slot exists). `save_sigmask`'s store is unchecked, so that
    /// the word holds the `saved` the prepare's hold recorded is part of
    /// this premise too. `state` must be the state lock's token: a fact
    /// about a token the caller already has is a requirement, never a
    /// conclusion (ensured, a token of any other lock would prove
    /// `false`). The hold also has the permission of the creation lock's
    /// cell that `fork_prepare` took (`Arenas::lock_held`, through
    /// [`ForkHeld::wf`]): restated, as the tokens are, and still the only
    /// one, since the prepare's hold kept it and no one wrote the value.
    /// That permission is exclusive (vstd's `PointsTo::is_exclusive`), so a
    /// second call would prove `false`, not just restate a token: the one
    /// caller is [`resumed`], called once per handler (the test
    /// `fork_hold_is_resumed_once` checks there is one call site).
    axiom fn axiom_fork_resumed(s: &Shared, slot: &AtomicU64, tracked state: &LockHeld) -> (tracked held: ForkHeld)
        requires
            state.held() == purge::background::state_lock(),
        ensures
            fork_holds(*s, held, *state),
            held.has_slot(),
            held.slot_addr() == word_addr(slot),
            held.saved() matches Some(m) && effective(m) == m && m & !crate::sys::BLOCKABLE_SIGNALS == 0,
    ;

    /// The slot, [`fork_slot`], and the release's hold of every lock, with
    /// the permission to that slot: [`axiom_fork_resumed`] for the word
    /// [`fork_slot`] returns. Each handler calls it once, which nothing
    /// enforces (see `api::axiom_fork_resumed_state`).
    // `s` is only named by the specification and the ghost call.
    #[allow(unused_variables)]
    #[inline(always)]
    pub(super) fn resumed(s: &Shared, Tracked(state): Tracked<&LockHeld>) -> (r: (&'static AtomicU64, Tracked<ForkHeld>))
        requires
            state.held() == purge::background::state_lock(),
        ensures
            fork_holds(*s, r.1@, *state),
            r.1@.has_slot(),
            r.1@.slot_addr() == word_addr(r.0),
            r.1@.saved() matches Some(m) && effective(m) == m && m & !crate::sys::BLOCKABLE_SIGNALS == 0,
    {
        let slot = fork_slot();
        (slot, Tracked(axiom_fork_resumed(s, slot, state)))
    }

    } // verus!
}

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

    /// `challoc_features()` is the `Display` of a full probe,
    /// NUL-terminated, static, and the same bytes on every call: under
    /// `thp:never` (the default, so the tests' configuration), where init
    /// skips the per-size THP files, the line built at the first request
    /// still lists every enabled mTHP size, as a fresh full probe does.
    #[test]
    fn challoc_features_is_the_probe_line() {
        let p = challoc_features();
        assert!(!p.is_null());
        assert_eq!(p, challoc_features());
        // SAFETY: `challoc_features` returns a static NUL-terminated line.
        let s = unsafe { core::ffi::CStr::from_ptr(p) }.to_str().unwrap();
        let full = crate::sys::features::Features::probe(true);
        assert_eq!(s, std::format!("{full}"), "the line is the full probe's");
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

    /// A `fork` discovers a deferred cgroup in its prepare handler, so the
    /// child inherits the record (it reads no files of its own) and the
    /// parent has it too.
    #[test]
    fn a_forked_child_inherits_the_discovered_cgroup() {
        // Init registers the fork handlers.
        let _ = shared();
        // SAFETY: the test binary is multithreaded; the child uses only challoc's
        // state, which its fork handlers keep consistent, and leaves through
        // `_exit`.
        let pid = unsafe { ::libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let code = if shared().cgroup().is_some() { 0 } else { 1 };
            // SAFETY: `_exit` has no preconditions; the child must not unwind.
            unsafe { ::libc::_exit(code) };
        }
        let mut status = 0;
        // SAFETY: `pid` is this process's child; `status` is a local.
        assert_eq!(unsafe { ::libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(::libc::WIFEXITED(status), "child status {status:#x}");
        assert_eq!(::libc::WEXITSTATUS(status), 0, "the child has no record");
        assert!(shared().cgroup().is_some(), "nor the parent");
    }

    /// A feature line another thread was building when the process forked
    /// is built again by the child, not waited for: the child (one thread
    /// here, so the state can be set without racing other tests) marks the
    /// line as being built, forks, and its own child must get the line
    /// within the alarm.
    #[test]
    fn a_forked_child_does_not_wait_for_a_line_being_built() {
        let expect = feature_line().len();
        // SAFETY: as in `a_forked_child_inherits_the_discovered_cgroup`: each
        // child uses only challoc's state and leaves through `_exit`.
        let pid = unsafe { ::libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            LINE_STATE.store(LINE_BUILDING, Ordering::Release);
            // SAFETY: as above.
            let gc = unsafe { ::libc::fork() };
            if gc == 0 {
                // SAFETY: `alarm` has no preconditions.
                unsafe { ::libc::alarm(10) };
                let p = challoc_features();
                // SAFETY: `challoc_features` returns a static NUL-terminated line.
                let n = unsafe { core::ffi::CStr::from_ptr(p) }.to_bytes().len();
                // SAFETY: `_exit` has no preconditions.
                unsafe { ::libc::_exit(if n == expect { 0 } else { 2 }) };
            }
            let mut status = 0;
            let code = if gc > 0
                // SAFETY: `gc` is this process's child; `status` is a local.
                && unsafe { ::libc::waitpid(gc, &mut status, 0) } == gc
                && ::libc::WIFEXITED(status)
            {
                ::libc::WEXITSTATUS(status)
            } else {
                3
            };
            // SAFETY: `_exit` has no preconditions.
            unsafe { ::libc::_exit(code) };
        }
        let mut status = 0;
        // SAFETY: `pid` is this process's child; `status` is a local.
        assert_eq!(unsafe { ::libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(::libc::WIFEXITED(status), "child status {status:#x}");
        assert_eq!(
            ::libc::WEXITSTATUS(status),
            0,
            "the grandchild spun on the line (3: killed by the alarm) or got another one (2)"
        );
    }

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
                    if let Some((name, ty)) = rest.split_once(':')
                        && is_lock(ty)
                    {
                        out.push(std::format!("{rel}:static.{}", name.trim()));
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
                            s.arena(a, vstd::prelude::Tracked::assume_new())
                                .unwrap()
                                .0
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
                                s.arena(a, vstd::prelude::Tracked::assume_new())
                                    .unwrap()
                                    .0
                                    .classes[c]
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
                            s.arena(a, vstd::prelude::Tracked::assume_new())
                                .unwrap()
                                .0
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

    /// `resume::axiom_fork_resumed` restates the fork hold, whose creation
    /// lock cell permission is exclusive: a second call would prove
    /// `false`. Its one call is in `resume::resumed`, and `resumed`'s one
    /// call is in `fork_release`, which each handler calls once.
    #[test]
    fn fork_hold_is_resumed_once() {
        // Spelled in parts, so that this test's own lines match nothing.
        let src = include_str!("mod.rs");
        let calls = |name: &str| {
            src.lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .filter(|l| l.contains(name) && !l.contains("fn "))
                .count()
        };
        let axiom = std::format!("{}{}(", "axiom_fork_", "resumed");
        let resumed = std::format!("{}{}(", "resume::", "resumed");
        let release = std::format!("{}{}(", "fork_", "release");
        assert_eq!(calls(&axiom), 1, "calls of the resume axiom");
        assert_eq!(calls(&resumed), 1, "calls of resume::resumed");
        assert_eq!(calls(&release), 2, "calls of fork_release");
        // The other once-per-handler premises: the state lock's token, in
        // the parent and the child handler, and the mask's, in the release;
        // the prepare's lock-order root and the thread's mask, in the
        // prepare.
        let state = std::format!("{}{}(", "axiom_fork_resumed_", "state");
        let sig = std::format!("{}{}(", "axiom_fork_resumed_", "sig");
        let entry = std::format!("{}{}(", "axiom_fork_", "entry");
        let mask = std::format!("{}{}(", "axiom_thread_", "mask");
        assert_eq!(calls(&state), 2, "calls of the state token's premise");
        assert_eq!(calls(&sig), 1, "calls of the mask token's premise");
        let lock = include_str!("../lock.rs");
        let sys = include_str!("../sys/mod.rs");
        let calls_in = |text: &str, name: &str| {
            text.lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .filter(|l| l.contains(name) && !l.contains("fn "))
                .count()
        };
        assert_eq!(
            calls(&entry) + calls_in(lock, &entry),
            1,
            "calls of the prepare's lock-order premise"
        );
        assert_eq!(
            calls(&mask) + calls_in(sys, &mask),
            1,
            "calls of the thread mask premise"
        );
    }

    /// The once-cells' fork-child resets, which take the permissions a
    /// thread of the parent held (`axiom_line_orphaned`,
    /// `sys::cgroup::axiom_cell_orphaned`), run only in the child's fork
    /// handler: across every non-test source file, each axiom has one
    /// call, in its reset, each reset one caller, `fork_child` (through
    /// `Shared::cgroup_fork_child` for the cgroup record), and
    /// `fork_child` is named only where `pthread_atfork` registers it.
    #[test]
    fn fork_child_resets_are_child_only() {
        use std::string::ToString;
        /// Every `.rs` file under `dir` but the loom and Kani trees, as
        /// (path, text before its first `#[cfg(test)]` test module).
        fn sources(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            entries.sort();
            for path in entries {
                let name = path.to_str().unwrap().to_string();
                if path.is_dir() {
                    if !name.ends_with("loom_tests") && !name.ends_with("kani") {
                        sources(&path, out);
                    }
                } else if name.ends_with(".rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let cut = ["tests", "linux_tests", "pure_tests"]
                        .iter()
                        .filter_map(|m| text.find(&std::format!("\n#[cfg(test)]\nmod {m} {{")))
                        .min()
                        .unwrap_or(text.len());
                    out.push((name, text[..cut].to_string()));
                }
            }
        }
        let mut files = Vec::new();
        sources(
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
            &mut files,
        );
        // Calls: lines naming `name` that are neither comments nor the
        // item's own declaration.
        let calls = |name: &str| -> Vec<String> {
            files
                .iter()
                .flat_map(|(f, src)| {
                    src.lines()
                        .filter(|l| !l.trim_start().starts_with("//"))
                        .filter(|l| l.contains(name) && !l.contains("fn "))
                        .map(move |l| std::format!("{f}: {}", l.trim()))
                })
                .collect()
        };
        let api = include_str!("mod.rs");
        let child = api
            .split("extern \"C\" fn fork_child() {")
            .nth(1)
            .and_then(|s| s.split("\n}\n").next())
            .unwrap_or("");
        let one = |name: &str, file: &str| {
            let found = calls(name);
            assert_eq!(found.len(), 1, "calls of {name}: {found:?}");
            assert!(
                found[0].contains(file),
                "{name} called outside {file}: {found:?}"
            );
        };
        one("axiom_line_orphaned(", "src/api/mod.rs");
        one("axiom_cell_orphaned(", "src/sys/cgroup.rs");
        // The child handler's resets, and each wait's early one for a fork
        // child's handler in the window (`lock::in_child_fork_window`).
        let two = |name: &str, files: [&str; 2]| {
            let mut found = calls(name);
            found.sort();
            assert_eq!(found.len(), 2, "calls of {name}: {found:?}");
            for (f, file) in found.iter().zip(files) {
                assert!(
                    f.contains(file),
                    "{name} called outside {files:?}: {found:?}"
                );
            }
        };
        two(
            "feature_line_fork_child(",
            ["src/api/mod.rs", "src/api/mod.rs"],
        );
        one("cgroup_fork_child(", "src/api/mod.rs");
        two(".fork_child()", ["src/heap/shared.rs", "src/sys/cgroup.rs"]);
        assert!(
            child.contains("feature_line_fork_child(")
                && child.contains("cgroup_fork_child(")
                && child.contains("reveal(crate::sys::cgroup::in_fork_child)"),
            "the resets and the fork-child capability are fork_child's"
        );
        let handler = calls("(fork_child)");
        assert!(
            handler.len() == 1 && handler[0].contains("pthread_atfork"),
            "fork_child is named outside its registration: {handler:?}"
        );
        assert!(files.len() > 50, "the source tree was not found");
    }

    /// The set of locks the handlers cover is exactly the set of locks in
    /// the crate (design 9).
    #[test]
    fn fork_lock_list_matches_the_source_tree() {
        let found = lock_inventory();
        let mut want: Vec<String> = FORK_LOCKS
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        want.sort();
        assert_eq!(
            found, want,
            "locks in src/ differ from the fork handlers' list: add every new lock to \
             fork_prepare/fork_release in nesting order, to FORK_LOCKS and to assert_locks"
        );
    }

    /// The handlers take every lock and release every lock, on both fork
    /// sides. The checks run in a fork child: in this process another test
    /// can take a heap lock (a huge allocation, a refill) the moment the
    /// handlers release it, so "every lock free" is only exact where no
    /// other thread runs. The real `fork` runs the handlers through
    /// `pthread_atfork`, so the child starts from `fork_child`'s release.
    #[test]
    fn fork_handlers_take_and_release_every_lock() {
        let _serial = crate::testutil::global_state();
        let s = shared();
        // A second arena so the handlers have more than arena 0 to cover.
        let extra = s
            .arenas
            .create(
                s,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            )
            .unwrap();
        let arenas = [0, extra];
        crate::testutil::in_fork_child("fork handlers", 60_000, || {
            assert_locks(s, &arenas, false);
            assert!(!purge::background::present());
            assert!(!purge::background::requested());
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
        });
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
        let _serial = crate::testutil::global_state();
        let s = shared();
        assert!(!tasks().iter().any(|n| n == "challoc-bg"));
        purge::background::request(
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert!(
            purge::background::start(
                s,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new()
            ),
            "background thread did not start"
        );
        assert!(purge::background::present());
        assert!(
            purge::background::start(
                s,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new()
            ),
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
        s.set_decay_ms(1, Tracked::assume_new(), Tracked::assume_new());
        let (p, _, _) = s.alloc_huge(
            8 << 20,
            4096,
            0,
            false,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!p.is_null());
        // SAFETY: `p` is a live 8 MiB huge allocation.
        unsafe { core::ptr::write_bytes(p, 1, 8 << 20) };
        let page = (p as usize - s.regions.huge_base) >> s.regions.lg_page;
        s.free_huge(
            page,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
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
        s.set_decay_ms(
            (old_ns / 1_000_000) as i64,
            Tracked::assume_new(),
            Tracked::assume_new(),
        );

        // A real fork: the child handler forgets the thread and the
        // request in the child, and leaves this process's thread alone.
        // The child checks with atomics and the state lock only and
        // answers through its exit status.
        assert!(purge::background::thread().is_some(), "no thread record");
        // SAFETY: the test binary is multithreaded; the child uses only atomics
        // and challoc's state lock, which its fork handlers keep consistent,
        // and leaves through `_exit`.
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
            // SAFETY: `_exit` has no preconditions; the child must not unwind.
            unsafe { ::libc::_exit(code) };
        }
        let mut status = 0;
        // SAFETY: `pid` is this process's child; `status` is a local.
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
        purge::background::request(
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
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

    /// The lazy start's rule (design 8.1): the burst threshold starts at
    /// the first look; the floor only once every look for the hold has
    /// seen it; a look under the floor ends the stretch.
    #[test]
    fn lazy_start_rule() {
        use core::sync::atomic::AtomicU64;
        use purge::background::{AUTO_BURST, AUTO_FLOOR, AUTO_HOLD_NS, auto_due};
        let since = AtomicU64::new(0);
        let t = 5_000_000_000u64;
        assert!(auto_due(AUTO_BURST, t, &since), "burst at the first look");
        assert!(!auto_due(AUTO_FLOOR - 1, t, &since));
        assert!(
            !auto_due(AUTO_FLOOR - 1, t + 10 * AUTO_HOLD_NS, &since),
            "under the floor never"
        );
        assert!(
            !auto_due(AUTO_FLOOR, t, &since),
            "the floor starts a stretch"
        );
        assert!(!auto_due(AUTO_BURST - 1, t + AUTO_HOLD_NS / 2, &since));
        assert!(
            auto_due(AUTO_FLOOR, t + AUTO_HOLD_NS, &since),
            "held for the hold"
        );
        // A dip under the floor restarts the stretch.
        since.store(0, core::sync::atomic::Ordering::Relaxed);
        assert!(!auto_due(AUTO_FLOOR, t, &since));
        assert!(!auto_due(AUTO_FLOOR / 2, t + AUTO_HOLD_NS / 2, &since));
        assert!(!auto_due(AUTO_FLOOR, t + AUTO_HOLD_NS, &since));
        assert!(!auto_due(
            AUTO_FLOOR,
            t + AUTO_HOLD_NS + AUTO_HOLD_NS / 2,
            &since
        ));
        assert!(auto_due(AUTO_FLOOR, t + 2 * AUTO_HOLD_NS, &since));
    }

    /// The lazy start end to end on one allocator, on injected tick
    /// times: nothing dirty starts nothing; 8 MiB dirty (over the floor,
    /// under the burst threshold) starts nothing at the first look or
    /// half a hold later; once the hold has passed a free's tick only
    /// requests the thread and the next allocation's tick starts it;
    /// the start leaves the lazy start armed; an explicit `request`
    /// disarms it.
    #[test]
    // The constant check pins the test size between the floor and the burst.
    #[allow(clippy::assertions_on_constants)]
    fn lazy_start_waits_for_dirty_memory() {
        use purge::background::{AUTO_BURST, AUTO_FLOOR, AUTO_HOLD_NS};
        let _serial = crate::testutil::global_state();
        let s = crate::testutil::shared();
        assert!(!purge::background::present());
        // A window long enough that nothing decays between the looks.
        let old_ns = s.decay_ns();
        s.set_decay_ms(1_000_000, Tracked::assume_new(), Tracked::assume_new());
        assert!((8u64 << 20) >= AUTO_FLOOR && (8u64 << 20) < AUTO_BURST);
        purge::background::set_auto(Some(s));
        let t0 = crate::sys::now_coarse_ns();
        purge::on_tick(
            s,
            t0,
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        purge::on_tick(
            s,
            t0 + 2 * AUTO_HOLD_NS,
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let clean = purge::background::present();
        let (p, _, _) = s.alloc_huge(
            8 << 20,
            4096,
            0,
            false,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!p.is_null());
        // SAFETY: `p` is a live 8 MiB huge allocation.
        unsafe { core::ptr::write_bytes(p, 1, 8 << 20) };
        s.free_huge(
            (p as usize - s.regions.huge_base) >> s.regions.lg_page,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        let t1 = t0 + 3 * AUTO_HOLD_NS;
        purge::on_tick(
            s,
            t1,
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let first = purge::background::present();
        purge::on_tick(
            s,
            t1 + AUTO_HOLD_NS / 2,
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let half = purge::background::present();
        // A free's tick past the hold requests the thread but never
        // creates it (glibc frees under locks `pthread_create` takes);
        // the next allocation's tick does.
        purge::on_tick(
            s,
            t1 + AUTO_HOLD_NS + 1,
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let freed = purge::background::present();
        let pending = purge::background::requested();
        purge::on_tick(
            s,
            t1 + AUTO_HOLD_NS + 2,
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let held = purge::background::present();
        let armed = purge::background::auto();
        purge::background::request(
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let disarmed = !purge::background::auto();
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
        purge::background::set_auto(None);
        s.huge
            .purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new());
        s.set_decay_ms(
            (old_ns / 1_000_000) as i64,
            Tracked::assume_new(),
            Tracked::assume_new(),
        );

        assert!(!clean, "nothing dirty started the thread");
        assert!(!first, "8 MiB started the thread at the first look");
        assert!(!half, "8 MiB started the thread before the hold");
        assert!(!freed, "a free's tick created the thread");
        assert!(pending, "a free's tick past the hold did not request it");
        assert!(held, "8 MiB held past the hold did not start the thread");
        assert!(armed, "a successful start disarmed the lazy start");
        assert!(disarmed, "request(false) left the lazy start armed");
        assert!(stopped, "background thread did not stop");
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
        let _serial = crate::testutil::global_state();
        // A private allocator: the process-wide one is shared with the
        // tests that do not take `global_state`, whose huge frees could
        // leave a run `Empty` before the hook's.
        let s = crate::testutil::shared();
        assert!(!purge::background::present());
        let old_ns = s.decay_ns();
        s.set_decay_ms(1, Tracked::assume_new(), Tracked::assume_new());
        let (p, _, _) = s.alloc_huge(
            8 << 20,
            4096,
            0,
            false,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!p.is_null());
        // SAFETY: `p` is a live 8 MiB huge allocation.
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
                    vstd::prelude::Tracked::assume_new(),
                );
                let first = none_before && s.huge.oldest_empty() != 0;
                hook_seen.store(if first { 3 } else { 2 }, Ordering::SeqCst);
            }));
        purge::background::request(
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let started = purge::background::start(
            s,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
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
                vstd::prelude::Tracked::assume_new(),
            );
        }
        s.set_decay_ms(
            (old_ns / 1_000_000) as i64,
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        purge::background::request(
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
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

    #[test]
    fn background_thread_that_exits_before_its_start_publishes_is_not_present() {
        use core::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let _serial = crate::testutil::global_state();
        let s = crate::testutil::shared();
        assert!(!purge::background::present());
        // The request is withdrawn while `start` is between creating the
        // thread and publishing it, and the new thread sees that and exits
        // first: `start` must not then publish a thread that is gone.
        let exited = Arc::new(AtomicBool::new(false));
        let exited_seen = Arc::clone(&exited);
        let exits = purge::background::EXITS.load(Ordering::SeqCst);
        *purge::background::BEFORE_PUBLISH.lock().unwrap() =
            Some(std::boxed::Box::new(move || {
                // No panics here: unwinding out of `start` would leave the
                // state marked as creating for every later test.
                purge::background::request(
                    false,
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                );
                for _ in 0..1000 {
                    if purge::background::EXITS.load(Ordering::SeqCst) != exits {
                        exited_seen.store(true, Ordering::SeqCst);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }));
        purge::background::request(
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        purge::background::start(
            s,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        purge::background::BEFORE_PUBLISH.lock().unwrap().take();
        let present_after_exit = purge::background::present();
        // Asked again, the thread must come back: a stale `present` would
        // make `start` return at once with no thread running.
        purge::background::request(
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let restarted = purge::background::start(
            s,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let exits_before_stop = purge::background::EXITS.load(Ordering::SeqCst);
        purge::background::request(
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let mut stopped = false;
        for _ in 0..400 {
            if purge::background::EXITS.load(Ordering::SeqCst) != exits_before_stop
                && !purge::background::present()
            {
                stopped = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if stopped {
            purge::background::join_exited();
        }
        assert!(
            exited.load(Ordering::SeqCst),
            "the new thread did not exit in the hook"
        );
        assert!(
            !present_after_exit,
            "start published a thread that had already exited"
        );
        assert!(restarted, "background thread did not restart");
        assert!(stopped, "restarted background thread did not stop");
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
        let _serial = crate::testutil::global_state();
        let s = shared();
        assert!(!purge::background::present());
        purge::background::request(
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert!(
            purge::background::start(
                s,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new()
            ),
            "background thread did not start"
        );
        let stack = purge::background::stack();
        assert_ne!(stack, 0);
        for cycle in 0..8 {
            purge::background::request(
                false,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            );
            wait_absent();
            purge::background::request(
                true,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            );
            assert!(
                purge::background::start(
                    s,
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                    Tracked::assume_new()
                ),
                "restart {cycle} failed"
            );
            assert!(purge::background::present());
            assert_eq!(
                purge::background::stack(),
                stack,
                "restart {cycle} took a new stack"
            );
        }
        purge::background::request(
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        wait_absent();
        assert!(purge::background::join_exited());
        assert!(purge::background::thread().is_none());
    }

    /// The calling thread's signal mask, as the kernel's 64-bit word.
    fn current_mask() -> u64 {
        // SAFETY: an all-zero `sigset_t` is a valid (empty) set.
        let mut set: ::libc::sigset_t = unsafe { core::mem::zeroed() };
        let rc =
            // SAFETY: a null new set only reads the mask into `set`, a local.
            unsafe { ::libc::pthread_sigmask(::libc::SIG_BLOCK, core::ptr::null(), &mut set) };
        assert_eq!(rc, 0);
        // The kernel mask is the first 8 bytes of glibc's 128-byte set.
        // SAFETY: `sigset_t` is 128 bytes, so its first 8 are readable.
        unsafe { core::ptr::read_unaligned(&set as *const _ as *const u64) }
    }

    /// glibc's SIGCANCEL (32) and SIGSETXID (33), never blocked.
    const GLIBC_INTERNAL: u64 = (1 << 31) | (1 << 32);

    /// `mask` without `SIGKILL` and `SIGSTOP`, which the kernel never blocks,
    /// and glibc's internal two, which the handlers leave unblocked.
    fn blockable(mask: u64) -> u64 {
        const KILL_STOP: u64 = (1 << (::libc::SIGKILL - 1)) | (1 << (::libc::SIGSTOP - 1));
        mask & !KILL_STOP & !GLIBC_INTERNAL
    }

    /// The handlers block every signal but glibc's internal two while the
    /// locks are held and restore the caller's mask (whatever it was)
    /// afterwards, on both fork sides.
    #[test]
    fn fork_handlers_block_signals_while_locked() {
        let _serial = crate::testutil::global_state();
        let all = blockable(!0);
        // Start from a distinctive mask so "restored" is not "left blocked".
        // SAFETY: an all-zero `sigset_t` is a valid (empty) set.
        let mut want: ::libc::sigset_t = unsafe { core::mem::zeroed() };
        // SAFETY: as above.
        let mut before: ::libc::sigset_t = unsafe { core::mem::zeroed() };
        // SAFETY: `want` and `before` are local sets.
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
        assert_eq!(
            current_mask() & GLIBC_INTERNAL,
            0,
            "prepare blocked SIGCANCEL or SIGSETXID"
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

    /// Two threads with different masks run the prepare and parent
    /// handlers concurrently (glibc serialises them; the shared
    /// `FORK_SIGMASK` slot must not need it): each gets its own mask back,
    /// because a prepare saves only once it holds every lock, and the
    /// release reads before its first unlock.
    #[test]
    fn concurrent_fork_handlers_restore_each_threads_mask() {
        let _serial = crate::testutil::global_state();
        let all = blockable(!0);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = [::libc::SIGUSR1, ::libc::SIGUSR2]
            .into_iter()
            .map(|signo| {
                let barrier = barrier.clone();
                thread::spawn(move || {
                    // SAFETY: an all-zero `sigset_t` is a valid (empty) set.
                    let mut want: ::libc::sigset_t = unsafe { core::mem::zeroed() };
                    // SAFETY: `want` is a local set; the new mask is this
                    // test thread's own.
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

    /// The C export modules the export-boundary checks below cover.
    const EXPORT_FILES: [&str; 3] = ["api/libc.rs", "api/cxx.rs", "api/jemalloc.rs"];

    /// Whether `t` (a trimmed line) is the first line of a C export: an
    /// `extern "C"` function item, or one `fn name / je_name(..)` entry of
    /// `verified_export_pair!`, whose one body serves both names.
    fn export_head(t: &str) -> bool {
        let pair = t.starts_with("fn ") && t.split('(').next().is_some_and(|h| h.contains(" / "));
        !t.starts_with("//") && (pair || (t.contains("extern \"C") && t.contains(" fn ")))
    }

    /// The exports in `text` (a source file's `mod exports`), as each
    /// one's first line and its body's statements: trimmed, with comments
    /// and blank lines dropped, a call spread over several lines joined
    /// into one, and the body's closing brace left out.
    fn export_bodies(text: &str) -> Vec<(String, Vec<String>)> {
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let t = lines[i];
            if !export_head(t) {
                i += 1;
                continue;
            }
            let head = String::from(t);
            // The body opens at the end of the first line, the head's own
            // included, that ends in ` {` (`{` alone, or `] {` after the
            // pair's `ensures [..]`).
            while lines[i] != "{" && !lines[i].ends_with(" {") {
                i += 1;
            }
            i += 1;
            let mut depth = 1i32;
            let mut body: Vec<String> = Vec::new();
            let mut open_call: Option<String> = None;
            while depth > 0 {
                let l = lines[i];
                i += 1;
                depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
                if depth == 0 || l.is_empty() || l.starts_with("//") {
                    continue;
                }
                let joined = match open_call.take() {
                    Some(prev) => prev + l,
                    None => String::from(l),
                };
                if joined.matches('(').count() > joined.matches(')').count() {
                    open_call = Some(joined);
                } else {
                    body.push(joined);
                }
            }
            out.push((head, body));
        }
        out
    }

    /// The lending of an export's `errno` token, spelled in parts so that
    /// this module's own lines are not lend sites.
    fn lend_line() -> String {
        std::format!(
            "let Tracked(mut errno) = crate::vspec::errno::{}();",
            "lend"
        )
    }

    /// The `failed` conditions an export may pass to
    /// `axiom_errno_at_return` (`vspec::errno`'s doc), one per kind of
    /// export: a null result, except a `realloc` (or `reallocarray`) of a
    /// block to size 0, and `posix_memalign`'s `ENOMEM`.
    const FAILED: [&str; 4] = [
        "r@.addr == 0",
        "r@.addr == 0 && !(p@.addr != 0 && size == 0)",
        "r@.addr == 0 && !(p@.addr != 0 && n * m == 0)",
        "r == ENOMEM",
    ];

    /// The [`FAILED`] condition of the export whose head is `head`: the
    /// `posix_memalign` pair's, `reallocarray`'s, the `realloc` forms'
    /// (the pair and `challoc_realloc_usable`), or a null result.
    fn failed_for(head: &str) -> &'static str {
        if head.contains("fn posix_memalign") {
            FAILED[3]
        } else if head.contains("fn reallocarray") {
            FAILED[2]
        } else if head.contains("realloc") {
            FAILED[1]
        } else {
            FAILED[0]
        }
    }

    /// The statement that hands a token back, with condition `failed`.
    fn axiom_line(axiom: &str, failed: &str) -> String {
        std::format!("crate::vspec::errno::{axiom}(errno, {failed});")
    }

    /// The out-pointer tokens an export body may mint (`axiom_c_out`,
    /// directly or through `memptr_out`), each with the statement that
    /// hands it back (`axiom_out_at_return`), spelled in parts so that this
    /// module's own lines are neither mint nor hand-back sites.
    fn out_tokens() -> [(String, String); 2] {
        let back = std::format!("{}{}", "axiom_out_", "at_return");
        [
            (
                std::format!(
                    "let Tracked(mut oout) = {}(out as *mut *mut u8);",
                    "memptr_out"
                ),
                std::format!("{back}(oout, out as *mut *mut u8);"),
            ),
            (
                std::format!("let tracked mut uout = {}(usable, true);", "axiom_c_out"),
                std::format!("{back}(uout, usable);"),
            ),
        ]
    }

    /// Checks one export's body against the one shape an export may have,
    /// and returns whether it hands its `errno` token back
    /// (`axiom_errno_at_return`, named in `axiom`) and whether it hands an
    /// out-pointer token back ([`out_tokens`]). The body sets up its call
    /// (the root grants, the token it lends, the C caller's out-pointer
    /// token), then calls its Rust entry point once, through `super::`,
    /// and returns what the call returned: as the body's tail, or bound to
    /// `r`, followed by the axiom on its `errno` token and the hand-back of
    /// the out-pointer token it minted, then `r`.
    fn check_export_body(at: &str, body: &[String], axiom: &str) -> (bool, bool) {
        // The root grants spelled in parts, so that these lines are not
        // grant sites (`sys::tests::grant_sites`).
        let grant = |g: &str| std::format!("crate::sys::grant_{g}();");
        let outs = out_tokens();
        let setup = [
            String::from("proof {"),
            grant("syscalls"),
            grant("aborts"),
            String::from("}"),
            lend_line(),
            outs[0].0.clone(),
            outs[1].0.clone(),
            String::from("let tracked mut left = Ghost(crate::purge::Left::none());"),
        ];
        let call = body
            .iter()
            .position(|s| s.starts_with("super::") || s.starts_with("let r = super::"))
            .unwrap_or_else(|| panic!("{at}: no `super::` call that is the result or binds `r`"));
        for s in &body[..call] {
            assert!(setup.contains(s), "{at}: `{s}` before the call");
        }
        let lends = body[..call].iter().filter(|s| **s == lend_line()).count();
        assert!(
            lends <= 1,
            "{at}: got = {lends} lent tokens, want = at most 1"
        );
        let minted: Vec<&String> = outs
            .iter()
            .filter(|(mint, _)| body[..call].contains(mint))
            .map(|(_, back)| back)
            .collect();
        assert!(
            minted.len() <= 1
                && outs
                    .iter()
                    .all(|(mint, _)| body[..call].iter().filter(|s| *s == mint).count() <= 1),
            "{at}: more than 1 out-pointer token minted, want = at most 1"
        );
        let rest: Vec<&str> = body[call + 1..].iter().map(String::as_str).collect();
        let s = &body[call];
        if s.starts_with("let r = ") {
            // One statement: the only `;` ends it.
            assert_eq!(s.find(';'), Some(s.len() - 1), "{at}: `{s}`");
            let back = axiom_line(axiom, failed_for(at));
            let mut want = Vec::from(["proof {", back.as_str()]);
            want.extend(minted.iter().map(|b| b.as_str()));
            want.extend(["}", "r"]);
            assert_eq!(
                rest, want,
                "{at}: after `{s}`: got = {rest:?}, want = the axiom on the token with this export's `failed` condition, the hand-back of its out-pointer token, then `r`"
            );
            assert_eq!(lends, 1, "{at}: hands back a token it did not lend");
            (true, !minted.is_empty())
        } else {
            assert!(!s.contains(';'), "{at}: the call `{s}` is not the result");
            assert!(
                rest.is_empty(),
                "{at}: got = {rest:?} after the call, want = nothing"
            );
            assert!(
                minted.is_empty(),
                "{at}: mints an out-pointer token it does not hand back"
            );
            (false, false)
        }
    }

    /// Every C export of `api::libc`, `api::cxx` and `api::jemalloc`
    /// returns its Rust entry point's result unchanged
    /// (`check_export_body`), so a nullable export cannot drop a block and
    /// return null even where its `ensures` cannot tell the two apart (the
    /// entry point states nothing about `errno`: `operator new`'s
    /// `nothrow_t` forms, `mallocx`, `rallocx`), and cannot write or read
    /// its `errno` token around the call. And the `errno` premises of
    /// `vspec::errno::axiom_errno_at_return` hold: the axiom is named only
    /// in those exports' bodies, right after the call, on the token the
    /// body lent, with that export's `failed` condition from [`FAILED`];
    /// tokens are lent only by exports, one each; out-pointer tokens are
    /// minted only by exports, at most one each, and go back through
    /// `axiom_out_at_return`, named only there, right after the call; and no code names an `exports` module,
    /// so nothing verified calls an export.
    #[test]
    fn exports_return_their_callees_result() {
        // Spelled in parts, so that this test's own lines match nothing.
        let axiom = std::format!("{}{}", "axiom_errno_", "at_return");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut handed_back = 0;
        let mut outs_back = 0;
        for file in EXPORT_FILES {
            let text = std::fs::read_to_string(src.join(file)).unwrap();
            let start = text
                .find("\nmod exports {")
                .unwrap_or_else(|| panic!("{file}: no `mod exports`"));
            let end = text[start..]
                .find("\n#[cfg(test)]")
                .map_or(text.len(), |e| start + e);
            let region = &text[start..end];
            let exports = export_bodies(region);
            // Each `extern "C"` item carries one symbol attribute, so the
            // parser found every export the module has.
            let attrs = region
                .lines()
                .map(str::trim)
                .filter(|t| {
                    t.starts_with("#[unsafe(no_mangle)]") || t.starts_with("#[unsafe(export_name")
                })
                .count();
            let items = exports
                .iter()
                .filter(|(h, _)| h.contains("extern \"C"))
                .count();
            assert_eq!(items, attrs, "{file}: extern \"C\" items parsed");
            assert!(!exports.is_empty(), "{file}: no exports found");
            for (head, body) in &exports {
                let at = std::format!("{file}: `{head}`");
                let (errno_back, out_back) = check_export_body(&at, body, &axiom);
                handed_back += usize::from(errno_back);
                outs_back += usize::from(out_back);
            }
        }
        // 13 bodies in `api::libc` (each pair's body counted once): the
        // exports whose entry point states `errno`.
        assert_eq!(handed_back, 13, "exports that hand their token back");
        let sites: Vec<_> = crate::sys::tests::grant_sites(&axiom)
            .into_iter()
            .filter(|(_, at)| !at.contains("axiom fn "))
            .collect();
        for (sig, at) in &sites {
            assert!(
                EXPORT_FILES
                    .iter()
                    .any(|f| at.starts_with(&std::format!("{f}:")))
                    && export_head(sig)
                    && at.ends_with(&std::format!("`{}`", axiom_line(&axiom, failed_for(sig)))),
                "{at}: the errno axiom in `{sig}`"
            );
        }
        assert_eq!(sites.len(), handed_back, "errno axiom calls");
        // 4 bodies in `api::libc` (the `posix_memalign` pair counted once,
        // and the three allocating `challoc_*_usable` exports) hand an
        // out-pointer token back, each only there, right after the call.
        assert_eq!(outs_back, 4, "exports that hand an out-pointer token back");
        let outs = out_tokens();
        let out_axiom = std::format!("{}{}", "axiom_out_", "at_return");
        let out_sites: Vec<_> = crate::sys::tests::grant_sites(&out_axiom)
            .into_iter()
            .filter(|(_, at)| !at.contains("axiom fn "))
            .collect();
        for (sig, at) in &out_sites {
            assert!(
                at.starts_with("api/libc.rs:")
                    && export_head(sig)
                    && outs
                        .iter()
                        .any(|(_, back)| at.ends_with(&std::format!("`{back}`"))),
                "{at}: the out-pointer axiom in `{sig}`"
            );
        }
        assert_eq!(out_sites.len(), outs_back, "out-pointer axiom calls");
        // Out-pointer tokens are minted only by those exports: `axiom_c_out`
        // directly in the `_usable` exports and through `memptr_out` in the
        // `posix_memalign` pair, so each export's `c_out_entry` and `c_out`
        // name its one call.
        let mint = std::format!("{}{}", "axiom_c_", "out(");
        let memptr = std::format!("{}{}", "memptr_", "out(");
        let mints: Vec<_> = crate::sys::tests::grant_sites(&mint)
            .into_iter()
            .filter(|(_, at)| !at.contains("axiom fn "))
            .collect();
        for (sig, at) in &mints {
            assert!(
                at.starts_with("api/libc.rs:")
                    && ((export_head(sig) && at.ends_with(&std::format!("`{}`", outs[1].0)))
                        || (sig.starts_with(&std::format!("fn {memptr}"))
                            && at
                                .ends_with(&std::format!("`let tracked o = {mint}out, false);`")))),
                "{at}: an out-pointer token minted in `{sig}`"
            );
        }
        assert_eq!(mints.len(), 4, "out-pointer token mints");
        let memptrs: Vec<_> = crate::sys::tests::grant_sites(&memptr)
            .into_iter()
            .filter(|(sig, _)| !sig.starts_with(&std::format!("fn {memptr}")))
            .collect();
        for (sig, at) in &memptrs {
            assert!(
                at.starts_with("api/libc.rs:")
                    && export_head(sig)
                    && at.ends_with(&std::format!("`{}`", outs[0].0)),
                "{at}: a `posix_memalign` token minted in `{sig}`"
            );
        }
        assert_eq!(memptrs.len(), 1, "`memptr_out` calls");
        let lend = lend_line();
        for (sig, at) in crate::sys::tests::grant_sites(&std::format!("{}{}", "errno::", "lend()"))
        {
            assert!(
                export_head(&sig) && at.ends_with(&std::format!("`{lend}`")),
                "{at}: an errno token lent in `{sig}`"
            );
        }
        // No code names an `exports` module (a path through one, or a
        // `use` of one), so no verified code calls an export and no proof
        // sees two returns (`vspec::errno::c_errno`).
        let mut files = Vec::new();
        crate::sys::tests::walk(&src, &mut files);
        let name = std::format!("{}{}", "expo", "rts");
        let path = std::format!("{name}::");
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap();
            for (i, line) in text.lines().enumerate() {
                let t = line.trim();
                let names = t.contains(path.as_str())
                    || ((t.starts_with("use ") || t.starts_with("pub use "))
                        && t.contains(name.as_str()));
                assert!(
                    t.starts_with("//") || !names,
                    "{}:{}: `{t}`",
                    f.display(),
                    i + 1
                );
            }
        }
    }
}

/// Kani checks of the static-cell pointers `shared_ptr`,
/// `shared_storage_ptr`, `features_ptr` and `line_ptr`
/// (`verus/ledger/src/api/mod.rs.md`; `cfg(kani)` only, see
/// `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_statics {
    use super::{
        FEATURE_LINE, FEATURES, FeatureLine, Features, SHARED, Shared, features_ptr, line_ptr,
        shared_ptr, shared_storage_ptr,
    };
    use core::mem::{align_of, size_of};

    /// The storage of a `Global<T>` at `p`, as the permissions
    /// `axiom_statics` and `Shared::init` hand out for it need it:
    /// non-null, aligned for `T`, and its first and last bytes writable
    /// through `p` (Kani fails a write outside the object `p` is derived
    /// from), so the `size_of::<T>()` bytes from `p` are the cell.
    fn is_cell<T>(p: *mut T, cell: *const u8) {
        assert!(!p.is_null());
        assert!(p as usize % align_of::<T>() == 0);
        assert!(p as usize == cell as usize);
        let b = p.cast::<u8>();
        // SAFETY: the static's `MaybeUninit<T>` storage, written only here
        // (no other code runs in a harness).
        unsafe {
            b.write(0);
            b.add(size_of::<T>() - 1).write(0);
        }
    }

    /// `shared_storage_ptr`, `ensures p == Shared::storage()`, and
    /// `shared_ptr`, `ensures r == Shared::storage() as *const Shared`,
    /// with `Shared::storage` uninterpreted: every call of either returns
    /// one pointer, the same for both (both name `Shared::storage()`).
    /// Beyond the contracts, it is `SHARED`'s storage (`is_cell`).
    #[kani::proof]
    fn shared_ptrs_agree() {
        let p = shared_storage_ptr();
        assert!(shared_storage_ptr() == p);
        assert!(shared_ptr() == p as *const Shared);
        assert!(shared_ptr() == shared_ptr());
        is_cell(p, (&raw const SHARED).cast());
    }

    /// `features_ptr`, `ensures p == features_storage()`, and `line_ptr`,
    /// `ensures p == line_storage()`, both uninterpreted: every call
    /// returns the same pointer. Beyond the contracts, each is its
    /// static's storage (`is_cell`).
    #[kani::proof]
    fn feature_ptrs_fixed() {
        let f = features_ptr();
        assert!(features_ptr() == f);
        is_cell::<Features>(f, (&raw const FEATURES).cast());
        let l = line_ptr();
        assert!(line_ptr() == l);
        is_cell::<FeatureLine>(l, (&raw const FEATURE_LINE).cast());
    }
}
