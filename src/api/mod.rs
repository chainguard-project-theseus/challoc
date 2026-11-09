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
pub(crate) mod c_programs;
#[cfg(test)]
mod restated;

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::arena::Arena;
use crate::config;
use crate::heap::Shared;
use crate::layout::{self, NCLASSES, Regions};
use crate::lock::{Lock, LockHeld, LockOrder};
#[cfg(verus_keep_ghost)]
use crate::lock::{RawLock, rank_arena_stats, rank_class, rank_sets, rank_state, rank_table};
use crate::purge;
use crate::purge::background::{State, StateHold};
use crate::sys;
use crate::sys::features::{FeatureLine, Features};
#[cfg(verus_keep_ghost)]
use crate::sys::{SigMask, all_blocked, effective};
use crate::vspec::byte::BytePerm;
use crate::vspec::count::CountPerm;
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

/// The two init-once words and the invariant that governs them: `word`,
/// the allocator's state (`UNINIT`, `INITIALISING`, `READY`), and `line`,
/// the feature line's (`LINE_UNBUILT`, `LINE_BUILDING`, `LINE_READY`;
/// [`FEATURE_LINE`]). `inv` is `word`'s invariant ([`StatePred`]), which
/// also holds `line`'s permission until `init` moves it into the line's
/// invariant ([`LinePred`]). `inv` is a ghost field: the static is the
/// two atomics, laid out in this order (`repr(C)`), so `word` stays at
/// `STATE`'s address.
#[repr(C)]
struct InitWords {
    word: AtomicU32,
    line: AtomicU8,
    inv: Tracked<AtomicInvariant<(), StateVal, StatePred>>,
}

impl InitWords {
    /// The atomics are the ones the process-start token names
    /// ([`axiom_start`]), and the invariant is in [`state_ns`].
    closed spec fn wf(&self) -> bool {
        &&& crate::vspec::count::count_id(&self.word) == state_word_id()
        &&& crate::vspec::byte::byte_id(&self.line) == line_word_id()
        &&& self.inv@.namespace() == state_ns()
    }
}

/// The init-once words, built in `const` context: each atomic with its one
/// permission ([`crate::vspec::count::new`], [`crate::vspec::byte::new`]),
/// and `STATE`'s invariant over them, at `UNINIT`, with the process-start
/// token ([`axiom_start`]). The initialiser runs once, at compile time, so
/// the token is taken once.
exec static STATE: InitWords
    ensures
        STATE.wf(),
{
    let (word, Tracked(perm)) = crate::vspec::count::new(UNINIT);
    let (line, Tracked(line_perm)) = crate::vspec::byte::new(LINE_UNBUILT);
    let tracked s = axiom_start(&word, &line);
    let tracked inv = AtomicInvariant::new(
        (),
        StateVal {
            perm,
            line: Some(line_perm),
            steps: Some(s.steps),
            statics: Some(s.statics),
            published: None,
            auth: s.auth,
            half: Some(s.half),
            mark: None,
        },
        state_ns(),
    );
    InitWords { word, line, inv: Tracked(inv) }
}

} // verus!

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
/// statics it covers (`STATE`'s invariant holds them from the process's
/// start, [`axiom_start`], until the winning exchange), so no thread that can state `ready` holds
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
/// does hold linear permissions (the line word's, and the two statics'
/// while the line is unbuilt), and a second `LineInv` in [`line_ns`]
/// would duplicate them; none can exist, since a `LineInv` needs a
/// permission to the line's word ([`line_word_id`]), whose only one `STATE`'s initialiser mints (`vspec::byte::new`) and [`claim_init`] hands out once, and `init` puts
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

/// The identity of `STATE`'s word ([`InitWords`]): what its permission
/// is for. Uninterpreted: one atomic, fixed for the process
/// ([`axiom_start`]).
pub uninterp spec fn state_word_id() -> int;

/// The identity of the feature line's word ([`InitWords`]).
pub uninterp spec fn line_word_id() -> int;

/// What the process starts with, besides the two words and their
/// permissions: the tokens of every step of building the allocator,
/// none taken ([`once`]); both halves of the ghost variable at
/// [`ready_mark`], at `false`; and the permissions to the storage of the
/// statics `init` writes itself ([`Statics::fresh`]).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct Start {
    pub tracked steps: Map<once::Step, once::Once>,
    pub tracked auth: GhostVarAuth<bool>,
    pub tracked half: GhostVar<bool>,
    pub tracked statics: Statics,
}

/// The permissions to the storage of the statics `init` writes itself:
/// [`FEATURES`], [`FEATURE_LINE`] and the class table
/// ([`layout::CLASSES`]).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct Statics {
    pub tracked features: PointsTo<Features>,
    pub tracked line: PointsTo<FeatureLine>,
    pub tracked classes: PointsTo<[layout::Class; NCLASSES]>,
}

impl Statics {
    /// As the process starts: `FEATURES` and `FEATURE_LINE`
    /// uninitialised, the class table initialised, its entries well
    /// formed and sized ([`layout::table_ok`]).
    pub open spec fn fresh(&self) -> bool {
        &&& self.features.ptr() == features_storage()
        &&& self.features.is_uninit()
        &&& self.line.ptr() == line_storage()
        &&& self.line.is_uninit()
        &&& self.classes.ptr() == layout::classes_storage()
        &&& self.classes.is_init()
        &&& layout::table_ok(self.classes.value())
    }
}

/// The process-start token, taken once, by [`STATE`]'s initialiser.
/// Trusted: `word` and `line` are [`STATE`]'s two atomics, whose
/// identities this fixes ([`state_word_id`], [`line_word_id`]); the
/// process has the token of every step of building the allocator, none
/// taken ([`once`]), and both halves of the ghost variable at
/// [`ready_mark`], at `false`; and the statics' storage and initial
/// values: nothing but `init` ([`publish_features`],
/// [`layout::adopt_page_geometry`]) and, after `READY`, the feature
/// line's build ([`build_line`]: `FEATURES`' THP table and
/// `FEATURE_LINE`, through the permissions the line's invariant hands
/// it) writes them, and the class table's storage holds `BASE_CLASSES`'
/// value, which meets `table_ok` (the `exec const`'s verified
/// `ensures`). Verified code reads [`layout::CLASSES`] only through the
/// class table's permission, which `init` freezes after
/// [`layout::adopt_page_geometry`] (`layout::frozen_table`), so the table
/// it reads is the one [`layout::axiom_class_table`] fixes. A `static`'s
/// initialiser runs once, at compile time; the test
/// `access_init_once_words_written_by_their_protocol`
/// (tests/loom/tests/access.rs) checks that it is the one call.
axiom fn axiom_start(word: &AtomicU32, line: &AtomicU8) -> (tracked s: Start)
    ensures
        crate::vspec::count::count_id(word) == state_word_id(),
        crate::vspec::byte::byte_id(line) == line_word_id(),
        once::all_steps(s.steps),
        s.auth.id() == ready_mark(),
        !s.auth@,
        s.half.id() == ready_mark(),
        s.statics.fresh(),
;

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

/// What the line's invariant holds: the line word's permission, the
/// statics' permissions while the line is unbuilt, and those
/// permissions, frozen, from `LINE_READY` on.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct LineVal {
    pub tracked perm: BytePerm,
    pub tracked cell: Option<LineCell>,
    pub tracked frozen: Option<&'static LineCell>,
}

/// The feature line's invariant, the once-cell protocol of
/// the line's word: the state is `LINE_UNBUILT`, `LINE_BUILDING` or
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
        &&& v.perm.id() == line_word_id()
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

/// What `STATE`'s invariant holds: its word's permission, the feature
/// line's word's permission, the process's one-shot tokens and the
/// statics' permissions until the winning exchange takes them, the
/// published permissions from `READY` on, and the ghost variable at
/// [`ready_mark`]: its authority, its second half until the winning
/// exchange takes it too, and that half frozen ([`ReadyMark`]) from
/// `READY` on.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct StateVal {
    pub tracked perm: CountPerm,
    pub tracked line: Option<BytePerm>,
    pub tracked steps: Option<Map<once::Step, once::Once>>,
    pub tracked statics: Option<Statics>,
    pub tracked published: Option<&'static Published>,
    pub tracked auth: GhostVarAuth<bool>,
    pub tracked half: Option<GhostVar<bool>>,
    pub tracked mark: Option<&'static ReadyMark>,
}

/// `STATE`'s invariant: the permission is to `STATE`'s word, which is
/// `UNINIT`, `INITIALISING` or `READY`; while it is `UNINIT` the
/// invariant holds the feature line's word's permission at
/// `LINE_UNBUILT`, the tokens of every step ([`once::all_steps`]), the
/// statics' permissions ([`Statics::fresh`]) and the ghost variable's
/// second half (and only then: the half is in the invariant exactly at
/// `UNINIT`, so the initialiser, which puts it there, builds the
/// invariant at `UNINIT`), and from `READY` on the published permissions
/// ([`Published::ok`]) and the [`ReadyMark`]. The ghost variable at
/// [`ready_mark`] is `true` exactly at `READY`, so `READY` is final: a
/// holder of the mark, which is frozen at `true`, finds `STATE` at
/// `READY` ([`load_state_marked`]). `STATE` moves from `UNINIT` to
/// `INITIALISING` only at [`claim_init`]'s exchange, which takes the
/// tokens, the permissions and the half, and to `READY` only at
/// [`publish_ready`]'s store, after `init` built and froze what it
/// publishes, which sets the variable and freezes the half; nothing
/// writes it otherwise, so it never goes back.
// Ghost: named only by proof code.
#[allow(dead_code)]
struct StatePred;

impl InvariantPredicate<(), StateVal> for StatePred {
    open spec fn inv(k: (), v: StateVal) -> bool {
        &&& v.perm.id() == state_word_id()
        &&& v.perm.value() == UNINIT || v.perm.value() == INITIALISING || v.perm.value() == READY
        &&& v.perm.value() == UNINIT ==> (v.line matches Some(l) && l.id() == line_word_id() && l.value() == LINE_UNBUILT)
        &&& v.perm.value() == UNINIT ==> (v.steps matches Some(m) && once::all_steps(m))
        &&& v.perm.value() == UNINIT ==> (v.statics matches Some(s) && s.fresh())
        &&& v.perm.value() == READY ==> (v.published matches Some(p) && p.ok())
        &&& v.auth.id() == ready_mark()
        &&& v.auth@ <==> v.perm.value() == READY
        &&& v.perm.value() == UNINIT ==> (v.half matches Some(h) && h.id() == ready_mark())
        &&& v.perm.value() != UNINIT ==> v.half is None
        &&& v.perm.value() == READY ==> (v.mark matches Some(m) && marks_ready(*m))
    }
}

/// The namespace of `STATE`'s invariant.
spec fn state_ns() -> int {
    0x7374_6174
}

/// `STATE`'s invariant, and its word, with the facts `STATE`'s
/// initialiser ensures ([`InitWords::wf`]).
#[inline(always)]
fn state() -> (r: (&'static AtomicU32, Tracked<&'static AtomicInvariant<(), StateVal, StatePred>>))
    ensures
        crate::vspec::count::count_id(r.0) == state_word_id(),
        r.1@.namespace() == state_ns(),
    opens_invariants none
{
    let s = &STATE;
    (&s.word, Tracked(s.inv.borrow()))
}

/// The feature line's word, with its identity.
#[inline(always)]
fn line_word() -> (r: &'static AtomicU8)
    ensures
        crate::vspec::byte::byte_id(r) == line_word_id(),
    opens_invariants none
{
    &STATE.line
}

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

/// An acquire load of `STATE`, with the published permissions and the
/// [`ReadyMark`] when it reads `READY`.
#[inline(always)]
fn load_state(Tracked(seen): Tracked<&mut Seen>) -> (s: u32)
    ensures
        s == READY ==> final(seen).ok() && final(seen).marked(),
    opens_invariants [ state_ns() ]
{
    let (word, Tracked(inv)) = state();
    let s;
    vstd::open_atomic_invariant!(inv => v => {
        s = crate::vspec::count::load(word, Ordering::Acquire, Tracked(&v.perm));
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
    let (word, Tracked(inv)) = state();
    let s;
    vstd::open_atomic_invariant!(inv => v => {
        s = crate::vspec::count::load(word, Ordering::Acquire, Tracked(&v.perm));
        proof {
            v.auth.agree(mark);
            seen.published = v.published;
        }
    });
    s
}

/// An acquire load of the feature line's word, with the line's frozen
/// permissions when it reads `LINE_READY` ([`LinePred`]).
#[inline(always)]
fn load_line(Tracked(inv): Tracked<&'static LineInv>, Tracked(seen): Tracked<&mut LineSeen>) -> (s: u8)
    requires
        inv.namespace() == line_ns(),
    ensures
        s == LINE_READY ==> (final(seen).frozen matches Some(c) && c.built()),
    opens_invariants [ line_ns() ]
{
    let w = line_word();
    let s;
    vstd::open_atomic_invariant!(inv => v => {
        s = crate::vspec::byte::load(w, Ordering::Acquire, Tracked(&v.perm));
        proof {
            if s == LINE_READY {
                seen.frozen = v.frozen;
            }
        }
    });
    s
}

/// The feature line's exchange from `LINE_UNBUILT` to `LINE_BUILDING`:
/// the winner gets the permissions to the two statics, which the
/// invariant holds while the line is unbuilt ([`LinePred`]).
#[inline(always)]
fn claim_line(Tracked(inv): Tracked<&'static LineInv>) -> (r: (Result<u8, u8>, Tracked<Option<LineCell>>))
    requires
        inv.namespace() == line_ns(),
    ensures
        r.0 is Ok ==> (r.1@ matches Some(c) && c.unbuilt()),
    opens_invariants [ line_ns() ]
{
    let w = line_word();
    let tracked mut cell = None;
    let won;
    vstd::open_atomic_invariant!(inv => v => {
        won = crate::vspec::byte::compare_exchange(w, LINE_UNBUILT, LINE_BUILDING, Ordering::Acquire, Ordering::Acquire, Tracked(&mut v.perm));
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
    let w = line_word();
    let tracked frozen = vstd::modes::tracked_static_ref(cell);
    vstd::open_atomic_invariant!(inv => v => {
        crate::vspec::byte::store(w, LINE_READY, Ordering::Release, Tracked(&mut v.perm));
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

/// The process's allocator state, initialising it on first use: the one
/// value `Shared::init` built at the static's storage
/// ([`Shared::built_at`]`(`[`Shared::storage`]`)`), whichever path returns it.
#[inline(always)]
pub fn shared() -> (r: &'static Shared)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r.wf(),
        *r == Shared::built_at(Shared::storage()),
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
        r matches Some(s) ==> *s == Shared::built_at(Shared::storage()),
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
        *r.0 == Shared::built_at(Shared::storage()),
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
        *r == Shared::built_at(Shared::storage()),
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
/// the line word's exchange from `LINE_UNBUILT` handed its winner
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
/// check, for no argument. `malloc_usable_size(NULL)` is called whole
/// (`get_defrag_hint` is not read only: it runs the caller's cache GC,
/// whose release may abort, design 13.5); `nallocx` and
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
fn theorem_read_only_never_aborts(shared: &Shared, size: usize, flags: core::ffi::c_int, p: *const u8, verbose: bool, Tracked(ord): Tracked<&mut LockOrder>)
    requires
        crate::sys::may_syscall(),
        crate::sys::integrity_aborts(),
        shared.wf(),
        // As at an entry point: no allocator lock held.
        old(ord).top() == 0,
{
    proof {
        shared.lemma_wf();
    }
    // `malloc_usable_size(NULL)`, whole.
    let _ = libc::malloc_usable_size(core::ptr::null_mut(), Tracked(&mut *ord));
    // `nallocx` after its `shared()`.
    let f = crate::heap::mallocx::Flags::decode(flags as u32);
    let _ = jemalloc::usable_for(shared, size, &f).unwrap_or(0);
    // `mallinfo2` after its `shared()`: its calls that carry a contract.
    let first = crate::stats::Scratch::new(&shared.meta, Tracked(&mut *ord));
    let _ = glibc::zero_info();
    if let Some(mut sc) = first {
        let tracked mut sd = Ghost((Seq::<crate::stats::ArenaStats>::empty(), Seq::<int>::empty()));
        let pair = sc.record();
        crate::stats::collect_all(shared, &mut pair.a, &mut pair.b, Tracked(&mut sd), Tracked(&mut *ord));
        let all = &sc.get().a;
        let page = shared.regions.page_size();
        let _ = crate::stats::fig_mul(all.pdirty, page);
        sc.free(&shared.meta, Tracked(&mut *ord));
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
    let _ = mallctl::utilization(shared, p as *const core::ffi::c_void, verbose, Tracked(ord));
}

/// `get_defrag_hint` aborts, for every pointer and before init, at most
/// where a valid program's free can (a resource failure or a pending
/// free-path check, [`sys::valid_aborts`], both in its cache GC's release,
/// which does not read the pointer) or at an integrity check
/// ([`sys::integrity_aborts`]): called with only those capabilities, not
/// [`sys::may_abort`], it verifies (design 13.5).
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such function.
#[cfg(verus_keep_ghost)]
fn theorem_defrag_hint_aborts_only_in_its_gc(p: *const u8, Tracked(ord): Tracked<&mut LockOrder>)
    requires
        crate::sys::may_syscall(),
        crate::sys::valid_aborts(),
        crate::sys::integrity_aborts(),
        old(ord).top() == 0,
{
    let _ = jemalloc::get_defrag_hint(p, Tracked(ord));
}

} // verus!

/// Whether init has completed (for the tests; the entry points use
/// [`shared_if_ready`]).
#[cfg(test)]
pub fn is_ready() -> bool {
    STATE.word.load(Ordering::Acquire) == READY
}

unsafe extern "C" {
    /// glibc's `pthread_atfork(3)`, with the `libc` crate's signature and
    /// the same symbol (premise C8 of `verus/kani/premises.md`). Declared
    /// in this crate rather than taken from the `libc` crate so that
    /// `kani_atfork` can stub it: Kani 0.68 stubs a foreign function only
    /// where this crate declares it, and silently ignores a stub on
    /// `libc::pthread_atfork`.
    fn pthread_atfork(
        prepare: Option<unsafe extern "C" fn()>,
        parent: Option<unsafe extern "C" fn()>,
        child: Option<unsafe extern "C" fn()>,
    ) -> core::ffi::c_int;
}

verus! {

/// What the winner of [`claim_init`]'s exchange takes from `STATE`'s
/// invariant: the one-shot tokens, the ghost variable's second half, and
/// the feature line's word's permission with the statics' permissions.
type ClaimTokens = (Map<once::Step, once::Once>, Option<GhostVar<bool>>, Option<(BytePerm, Statics)>);

/// `STATE`'s compare-exchange from `UNINIT` to `INITIALISING`, which
/// [`init_slow`] runs: the winner gets the process's one-shot tokens
/// ([`once::all_steps`]), the second half of the ghost variable at
/// [`ready_mark`], the feature line's word's permission and the statics'
/// permissions, which `STATE`'s invariant holds while it is `UNINIT`
/// ([`StatePred`]); nothing stores `UNINIT` or `INITIALISING`
/// elsewhere, so the exchange succeeds once per process and one caller
/// gets them. A caller that finds `READY` gets the published
/// permissions and the [`ReadyMark`].
#[inline(always)]
fn claim_init(Tracked(seen): Tracked<&mut Seen>) -> (r: (Result<u32, u32>, Tracked<ClaimTokens>))
    ensures
        r.0 is Ok ==> once::all_steps(r.1@.0),
        r.0 is Ok ==> (r.1@.1 matches Some(h) && h.id() == ready_mark()),
        r.0 is Ok ==> (r.1@.2 matches Some(o) && o.0.id() == line_word_id() && o.0.value() == LINE_UNBUILT && o.1.fresh()),
        r.0 == Err::<u32, u32>(READY) ==> final(seen).ok() && final(seen).marked(),
{
    let (word, Tracked(inv)) = state();
    let tracked mut steps = Map::tracked_empty();
    let tracked mut half = None;
    let tracked mut owned = None;
    let won;
    vstd::open_atomic_invariant!(inv => v => {
        won = crate::vspec::count::compare_exchange(word, UNINIT, INITIALISING, Ordering::Acquire, Ordering::Acquire, Tracked(&mut v.perm));
        proof {
            if won is Ok {
                steps = v.steps.tracked_take();
                half = Some(v.half.tracked_take());
                owned = Some((v.line.tracked_take(), v.statics.tracked_take()));
            }
            if won == Err::<u32, u32>(READY) {
                seen.published = v.published;
                seen.mark = v.mark;
            }
        }
    });
    (won, Tracked((steps, half, owned)))
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
    let (word, Tracked(inv)) = state();
    let tracked mut half = half;
    let tracked mut out = None;
    vstd::open_atomic_invariant!(inv => v => {
        crate::vspec::count::store(word, READY, Ordering::Release, Tracked(&mut v.perm));
        proof {
            v.auth.update(&mut half, true);
            let tracked m = vstd::modes::tracked_static_ref(half);
            v.published = Some(published);
            v.mark = Some(m);
            // The invariant holds the half only at `UNINIT`, and the word
            // is `READY` now.
            v.half = None;
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
        *r.0 == Shared::built_at(Shared::storage()),
        marks_ready(*r.1@),
{
    let tracked mut seen = Seen { published: None, mark: None };
    init_gate_enter();
    let (won, Tracked(parts)) = claim_init(Tracked(&mut seen));
    let tracked (steps, half, owned) = parts;
    match won {
        Ok(_) => {
            INIT_TID.store(sys::gettid() as u32, Ordering::Relaxed);
            init(Tracked(steps), Tracked(owned.tracked_unwrap()), Tracked(&mut seen));
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

/// The `pthread_atfork` call itself, which may wait on glibc's atfork
/// lock (a futex). Trusted: passes function pointers, which Verus does
/// not support.
#[verifier::external_body]
#[inline(always)]
fn pthread_atfork_handlers() -> i32
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: the three handlers are plain `extern "C"` functions that only
    // touch allocator locks.
    unsafe { pthread_atfork(Some(fork_prepare), Some(fork_parent), Some(fork_child)) }
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
/// publishes `READY`, with what its winning exchange got
/// ([`claim_init`]): the statics' permissions ([`Statics`]) and the
/// feature line's word's permission, for the class table and the features
/// it writes here (the feature line's permission and its word's go into
/// the line's invariant with the features', the line waits for its first
/// request, [`feature_line_ready`]), and the one-shot tokens for
/// `Shared::init`, which uses them up. It hands back the
/// permissions to the `Shared` and the class table, frozen, and the
/// line's invariant
/// ([`Published`]).
fn init(
    Tracked(steps): Tracked<Map<once::Step, once::Once>>,
    Tracked(owned): Tracked<(BytePerm, Statics)>,
    Tracked(seen): Tracked<&mut Seen>,
)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        once::all_steps(steps),
        owned.0.id() == line_word_id(),
        owned.0.value() == LINE_UNBUILT,
        owned.1.fresh(),
    ensures
        final(seen).ok(),
{
    let tracked (line_state, statics) = owned;
    let tracked Statics { features: mut feats, line, mut classes } = statics;
    // The slab geometry of the classes whose table slab is below the page
    // (design 3, page-sized slabs): written once here, before any other
    // thread can see the allocator, never again.
    // SAFETY: `init` runs once, under `INITIALISING`, before `READY`.
    unsafe { layout::adopt_page_geometry(crate::sys::features::page_size().trailing_zeros(), Tracked(&mut classes)) };
    // Frozen now: every later reader, here and from `READY` on, reads the
    // table through this one shared permission.
    let tracked classes = vstd::modes::tracked_static_ref(classes);
    proof {
        layout::axiom_class_table(classes);
    }
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
    let (regions, space) = Regions::reserve_owning(features.page_size, features.anon_name, Tracked(classes));
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
            Tracked(classes),
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
/// table cannot change while it is kept ([`arena_held`], [`count_held`],
/// which lend the creation lock's token with it). Built, by convention,
/// only by `fork_prepare` as it takes the locks and by
/// [`resume::axiom_fork_resumed`] for the release: the fields are private
/// to `api` (its submodules can see them too).
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

    /// The ranks of the locks the hold holds.
    pub(crate) open spec fn held_ranks(self) -> Set<int> {
        self.keys().map(|k: ForkLock| self.lock(k).rank())
    }

    /// A set of ranks within the hold's stays within it once the lock of
    /// key `k` is taken out ([`ForkHeld::take`]) and its rank removed.
    proof fn lemma_take_ranks(h0: ForkHeld, h1: ForkHeld, k: ForkLock, rs: Set<int>)
        requires
            rs.subset_of(h0.held_ranks()),
            h0.keys().contains(k),
            h1.keys() == h0.keys().remove(k),
            forall|j: ForkLock| j != k ==> #[trigger] h1.lock(j) == h0.lock(j),
        ensures
            rs.remove(h0.lock(k).rank()).subset_of(h1.held_ranks()),
    {
        assert forall|x: int| rs.remove(h0.lock(k).rank()).contains(x) implies #[trigger] h1.held_ranks().contains(x) by {
            assert(h0.held_ranks().contains(x));
            let j = choose|j: ForkLock| h0.keys().contains(j) && h0.lock(j).rank() == x;
            assert(j != k);
            assert(h1.keys().contains(j));
            assert(h1.lock(j) == h0.lock(j));
        }
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
/// `state` the background thread's state lock, with its cell's
/// permission ([`StateHold`]). `fork_prepare` ends with it, and the
/// release starts from it.
pub(crate) open spec fn fork_holds(s: Shared, held: ForkHeld, state: StateHold) -> bool {
    &&& held.wf(s)
    &&& held.all()
    &&& state.wf()
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
/// the state lock's hold, then [`resume::axiom_fork_resumed`] for the rest, at
/// the `&Shared` the release reads, which must be the process's one
/// value (`Shared::built_at(Shared::storage())`, what `shared` ensures).
/// The state lock's hold has the permission of its cell that
/// `fork_prepare` took (`purge::background::fork_lock`, through
/// [`StateHold::wf`]): restated, as the tokens are, and still the only
/// one, since the prepare's hold kept it. Its value keeps the state's
/// lock invariant, restated too: it is the value prepare's
/// `Lock::lock_held_perm` read, which that call ensures of it, and
/// nothing wrote it since (only the hold can, through
/// `purge::background::fork_child`, which runs after this call, and the
/// window's holders of the state lock write nothing under it: `start`
/// and `start_requested` refuse there, and `request` and `auto_tick`
/// store only atomics; each of them ends in a `LockGuard::unlock`,
/// which checks the invariant). Each handler calls them once, which
/// nothing enforces: that permission is exclusive (vstd's
/// `PointsTo::is_exclusive`), so a second call would give two
/// permissions of one cell, a contradiction that is derivable where
/// `Lock::has_perm` unfolds, in `lock.rs` (it is closed here and in
/// `purge::background`).
axiom fn axiom_fork_resumed_state() -> (tracked state: StateHold)
    ensures
        state.wf(),
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
/// creation lock's token (required), which it lends to `get_frozen`, so
/// a scan runs only while the lock is held, before the release hands the
/// token and the cell permission back together (`Arenas::unlock_held`).
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
        s.lemma_arenas_lock();
    }
    s.arenas.get_frozen(i as u16, Tracked(held.toks.tracked_borrow(ForkLock::Arenas)), Tracked(&held.created))
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
    proof {
        s.lemma_arenas_lock();
    }
    s.arenas.count_frozen(Tracked(held.toks.tracked_borrow(ForkLock::Arenas)), Tracked(&held.created))
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
    let n = s.arenas.count_frozen(Tracked(toks.tracked_borrow(ForkLock::Arenas)), Tracked(&cp)) as usize;
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
        proof {
            s.lemma_arenas_lock();
            assert(toks.contains_key(ForkLock::Arenas));
            assert(toks[ForkLock::Arenas].held() == s.arenas.spec_raw_lock());
        }
        if let Some(a) = s.arenas.get_frozen(i as u16, Tracked(toks.tracked_borrow(ForkLock::Arenas)), Tracked(&cp)) {
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
    // opens after the last acquisition (`lock::fork_prepared` of `ord`,
    // which only the state lock's acquisition on this root gives).
    crate::lock::fork_window_open(Tracked(&ord));
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
    Tracked(state): Tracked<&StateHold>,
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
    Tracked(state): Tracked<&StateHold>,
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
/// ended with ([`fork_holds`]; `state` is the state lock's hold) and
/// releases each listed lock once, by handing its token back to
/// `RawLock::unlock_held`; it ends holding none (asserted at the end).
///
/// The signal mask: it
/// starts from [`fork_sig_holds`], reads the slot under every lock, keeps
/// every blockable signal blocked through its last unlock, and ends with
/// the thread's mask the slot's content (asserted at the end).
///
/// The lock order: the release's root token (the hold's, handed over with
/// it by [`resume::axiom_fork_resumed`]) gives back the rank of each lock
/// it unlocks; with every lock of the hold unlocked it has none left, and
/// becomes the token at top 0 this returns ([`LockOrder::unroot`]): the
/// thread holds no allocator lock afterwards.
///
/// `l` is the state lock, the one `purge::background::state` returns,
/// which the release unlocks first.
fn fork_release(l: &Lock<State>, Tracked(state): Tracked<StateHold>) -> (o: Tracked<LockOrder<'static>>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        state.wf(),
        *l == purge::background::state_static(),
        l.rank() == rank_state(),
    ensures
        o@.top() == 0,
        !o@.root(),
{
    // The window ends before the first release, which must then unlock
    // (`lock::window_closed`, which every token release requires).
    crate::lock::fork_window_close();
    let s = shared();
    let (slot, Tracked(mut held), Tracked(mut root)) = resume::resumed(s, Tracked(&state));
    let tracked mut sig = axiom_fork_resumed_sig();
    let ghost saved = held.saved()->Some_0;
    proof {
        crate::sys::lemma_effective_idempotent(crate::sys::BLOCKABLE_SIGNALS);
        assert(fork_sig_holds(sig, held, saved));
    }
    // Written by the `fork_prepare` that paired with this release, and
    // still ours because every lock is held until the unlocks below.
    let mask = saved_sigmask(slot, Ghost(*s), Tracked(&held), Tracked(&state));
    purge::background::fork_unlock(l, Tracked(state), Tracked(&mut root));
    proof {
        assert(root.ranks().subset_of(held.held_ranks()));
    }
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
            root.root(),
            root.ranks().subset_of(held.held_ranks()),
        decreases i,
    {
        i -= 1;
        if let Some(a) = arena_held(s, i, Tracked(&held)) {
            let ghost h0 = held;
            let tracked t = held.take(ForkLock::ArenaStats(i as int));
            proof {
                ForkHeld::lemma_take_ranks(h0, held, ForkLock::ArenaStats(i as int), root.ranks());
            }
            a.stats_raw_lock().unlock_held(Tracked(t), Tracked(&mut root));
        }
    }
    let ghost h0 = held;
    let tracked t = held.take(ForkLock::Meta);
    proof {
        ForkHeld::lemma_take_ranks(h0, held, ForkLock::Meta, root.ranks());
    }
    s.meta_lock().unlock_held(Tracked(t), Tracked(&mut root));
    let ghost h0 = held;
    let tracked t = held.take(ForkLock::Huge);
    proof {
        ForkHeld::lemma_take_ranks(h0, held, ForkLock::Huge, root.ranks());
    }
    s.huge_lock().unlock_held(Tracked(t), Tracked(&mut root));
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
            root.root(),
            root.ranks().subset_of(held.held_ranks()),
        decreases i,
    {
        i -= 1;
        if let Some(a) = arena_held(s, i, Tracked(&held)) {
            let ghost h0 = held;
            let tracked t = held.take(ForkLock::Sets(i as int));
            proof {
                ForkHeld::lemma_take_ranks(h0, held, ForkLock::Sets(i as int), root.ranks());
            }
            a.sets().raw_lock().unlock_held(Tracked(t), Tracked(&mut root));
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
            root.root(),
            root.ranks().subset_of(held.held_ranks()),
        decreases c,
    {
        c -= 1;
        let ghost h0 = held;
        let tracked t = held.take(ForkLock::Table(c as int));
        proof {
            ForkHeld::lemma_take_ranks(h0, held, ForkLock::Table(c as int), root.ranks());
        }
        s.tables()[c].raw_lock().unlock_held(Tracked(t), Tracked(&mut root));
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
            root.root(),
            root.ranks().subset_of(held.held_ranks()),
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
                    root.root(),
                    root.ranks().subset_of(held.held_ranks()),
                decreases c,
            {
                c -= 1;
                let ghost h0 = held;
                let tracked t = held.take(ForkLock::Class(i as int, c as int));
                proof {
                    ForkHeld::lemma_take_ranks(h0, held, ForkLock::Class(i as int, c as int), root.ranks());
                }
                a.class(c).raw_lock().unlock_held(Tracked(t), Tracked(&mut root));
            }
        }
    }
    let ghost h0 = held;
    let tracked t = held.take(ForkLock::Stats);
    proof {
        ForkHeld::lemma_take_ranks(h0, held, ForkLock::Stats, root.ranks());
    }
    s.stats_lock().unlock_held(Tracked(t), Tracked(&mut root));
    let ghost h0 = held;
    let tracked t = held.take(ForkLock::Arenas);
    let ghost h1 = held;
    let tracked tt = held.take(ForkLock::Tcaches);
    proof {
        ForkHeld::lemma_take_ranks(h0, h1, ForkLock::Arenas, root.ranks());
        ForkHeld::lemma_take_ranks(h1, held, ForkLock::Tcaches, root.ranks().remove(h0.lock(ForkLock::Arenas).rank()));
        assert(held.keys() =~= Set::empty());
        assert(held.held_ranks() =~= Set::<int>::empty());
        s.lemma_arenas_lock();
    }
    // The creation lock's cell permission goes back with its token: the
    // hold has no lock left.
    let tracked cp = held.created;
    s.arenas.unlock_held(Tracked(t), Tracked(cp), Tracked(&mut root));
    s.tcaches_lock().unlock_held(Tracked(tt), Tracked(&mut root));
    sys::set_signal_mask(mask, Tracked(&mut sig));
    proof {
        assert(sig.mask() == saved);
        assert(root.ranks() =~= Set::<int>::empty());
    }
    Tracked(root.unroot())
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
    let l = purge::background::state();
    let tracked state = axiom_fork_resumed_state();
    fork_release(l, Tracked(state));
}

/// The child has one thread; the locks were held by the forking thread, so
/// releasing them is exactly right. Before that, with everything still
/// locked, the background thread is marked absent (design 8.1).
extern "C" fn fork_child() {
    proof {
        crate::sys::grant_syscalls();
        crate::sys::grant_aborts();
    }
    #[cfg(challoc_tls_check)]
    crate::heap::tlscheck::on_fork_child();
    fork_gate_exit();
    if !fork_was_locked() {
        return;
    }
    let l = purge::background::state();
    let tracked mut state = axiom_fork_resumed_state();
    purge::background::fork_child(l, Tracked(&mut state));
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
    let Tracked(mut ord) = fork_release(l, Tracked(state));
    // The dirty bytes this child inherited, the lazy start's baseline: read
    // after the release (the sum takes the class and huge locks), while
    // the child is still single-threaded, so nothing has moved them. Only
    // under the lazy start: nothing else reads it.
    if purge::background::auto() {
        // The release above handed back every lock this thread held, and
        // returned the token that says so.
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
    let w = line_word();
    let tracked inv = published.line;
    vstd::open_atomic_invariant!(inv => v => {
        let ghost before = v.perm;
        let _reset = crate::vspec::byte::compare_exchange(w, LINE_BUILDING, LINE_UNBUILT, Ordering::Relaxed, Ordering::Relaxed, Tracked(&mut v.perm));
        proof {
            if _reset is Ok {
                v.cell = Some(axiom_line_orphaned(before, &v.perm, &v.cell));
            }
        }
    });
}

/// Premise (the fork child's orphaned line): in the child of a `fork`,
/// a line whose state the child's reset found `LINE_BUILDING` (`before`,
/// the line word's permission just before the reset, and `perm`, just
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
axiom fn axiom_line_orphaned(before: BytePerm, tracked perm: &BytePerm, tracked cell: &Option<LineCell>) -> (tracked c: LineCell)
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
    use crate::lock::LockOrder;
    #[cfg(verus_keep_ghost)]
    use crate::lock::rank_state;
    use crate::purge::background::StateHold;
    #[cfg(verus_keep_ghost)]
    use crate::{sys::effective, vspec::word::word_addr};
    use core::sync::atomic::AtomicU64;
    #[cfg(verus_keep_ghost)]
    use vstd::prelude::*;
    use vstd::prelude::{Tracked, verus};

    verus! {

    /// Premise (see `api::axiom_fork_resumed_state`): the rest of the
    /// hold, with the permission to `slot`, which must be the word the
    /// prepare stored its mask to: [`resumed`] passes [`fork_slot`]'s
    /// `&FORK_SIGMASK`, as the prepare's store did, and a static has one
    /// address; passed any other word, it would mint a second permission
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
    /// this premise too. `state` must be the state lock's hold
    /// ([`StateHold::wf`]): a fact about a hold the caller already has is a
    /// requirement, never a conclusion (ensured, a hold of any other lock
    /// would prove `false`). The hold also has the permission of the
    /// creation lock's cell that `fork_prepare` took (`Arenas::lock_held`,
    /// through [`ForkHeld::wf`]): restated, as the tokens are, and still
    /// the only one, since the prepare's hold kept it and no one wrote the
    /// value.
    /// That permission is exclusive (vstd's `PointsTo::is_exclusive`), so a
    /// second call would prove `false`, not just restate a token: the one
    /// caller is [`resumed`], called once per handler (the test
    /// `fork_hold_is_resumed_once` checks there is one call site).
    ///
    /// With the hold comes the root lock-order token `fork_prepare` held
    /// it with (`lock::axiom_fork_entry`'s, raised by each acquisition):
    /// a root whose raw holds are among the hold's locks and the state
    /// lock (`fork_prepare`'s thread held no allocator lock before it, and
    /// the window's allocations release none). Unlike the hold, which
    /// `fork_prepare` asserts (`fork_holds`) before the C call that drops
    /// it, this rank clause is assumed: each acquisition adds its rank to
    /// the root, but no invariant of the prepare's loops carries the
    /// root's ranks to that C call.
    axiom fn axiom_fork_resumed(s: &Shared, slot: &AtomicU64, tracked state: &StateHold) -> (tracked r: (ForkHeld, LockOrder<'static>))
        requires
            *s == Shared::built_at(Shared::storage()),
            state.wf(),
        ensures
            fork_holds(*s, r.0, *state),
            r.0.has_slot(),
            r.0.slot_addr() == word_addr(slot),
            r.0.saved() matches Some(m) && effective(m) == m && m & !crate::sys::BLOCKABLE_SIGNALS == 0,
            r.1.root(),
            r.1.ranks().subset_of(r.0.held_ranks().insert(rank_state())),
    ;

    /// The slot, [`fork_slot`], and the release's hold of every lock, with
    /// the permission to that slot, and the hold's root token:
    /// [`axiom_fork_resumed`] for the word [`fork_slot`] returns. Each
    /// handler calls it once, which nothing enforces (see
    /// `api::axiom_fork_resumed_state`).
    // `s` is only named by the specification and the ghost call.
    #[allow(unused_variables)]
    #[inline(always)]
    pub(super) fn resumed(s: &Shared, Tracked(state): Tracked<&StateHold>) -> (r: (&'static AtomicU64, Tracked<ForkHeld>, Tracked<LockOrder<'static>>))
        requires
            *s == Shared::built_at(Shared::storage()),
            state.wf(),
        ensures
            fork_holds(*s, r.1@, *state),
            r.1@.has_slot(),
            r.1@.slot_addr() == word_addr(r.0),
            r.1@.saved() matches Some(m) && effective(m) == m && m & !crate::sys::BLOCKABLE_SIGNALS == 0,
            r.2@.root(),
            r.2@.ranks().subset_of(r.1@.held_ranks().insert(rank_state())),
    {
        let slot = fork_slot();
        let tracked (held, root) = axiom_fork_resumed(s, slot, state);
        (slot, Tracked(held), Tracked(root))
    }

    } // verus!
}

#[cfg(test)]
mod tests;

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
    /// `axiom_start` and `Shared::init` hand out for it need it:
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

/// Kani check of `pthread_atfork_handlers` (`verus/ledger/src/api/mod.rs.md`;
/// `cfg(kani)` only, see `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_atfork {
    use super::{fork_child, fork_parent, fork_prepare, pthread_atfork_handlers};
    use core::ffi::c_int;

    /// What the stand-in for `pthread_atfork` returns.
    static mut RC: c_int = 0;
    /// How many times it was called, and the three handlers it was passed
    /// (0 for none), in its argument order.
    static mut CALLS: usize = 0;
    static mut PASSED: [usize; 3] = [0; 3];

    /// The address of a handler, 0 for none.
    fn addr(h: Option<unsafe extern "C" fn()>) -> usize {
        h.map_or(0, |f| f as usize)
    }

    /// A stand-in for glibc's `pthread_atfork` (premise C8): it records
    /// its call and the handlers, and returns `RC`.
    #[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
    unsafe extern "C" fn atfork_model(
        prepare: Option<unsafe extern "C" fn()>,
        parent: Option<unsafe extern "C" fn()>,
        child: Option<unsafe extern "C" fn()>,
    ) -> c_int {
        // SAFETY: the harness's own statics; no other code runs in a
        // harness.
        unsafe {
            CALLS += 1;
            PASSED = [addr(prepare), addr(parent), addr(child)];
            RC
        }
    }

    /// `pthread_atfork_handlers`: no `ensures`; it returns, and (beyond
    /// the contract) calls `pthread_atfork` once with `fork_prepare`,
    /// `fork_parent` and `fork_child` in that order, and returns its
    /// result unchanged, for any result.
    #[kani::proof]
    #[kani::stub(super::pthread_atfork, atfork_model)]
    fn atfork_registers_the_handlers() {
        let rc: c_int = kani::any();
        kani::cover!(rc == 0);
        kani::cover!(rc != 0);
        // SAFETY: as in `atfork_model`.
        unsafe { RC = rc };
        let want = [
            fork_prepare as *const () as usize,
            fork_parent as *const () as usize,
            fork_child as *const () as usize,
        ];
        assert!(want[0] != want[1] && want[1] != want[2] && want[0] != want[2]);
        assert_eq!(pthread_atfork_handlers(), rc);
        // SAFETY: as in `atfork_model`.
        let (calls, passed) = unsafe { (CALLS, PASSED) };
        assert_eq!(calls, 1);
        assert_eq!(passed, want);
    }
}

#[cfg(test)]
mod heap_at_tests;
