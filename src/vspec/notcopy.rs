//! Compile-time checks that every `tracked` struct or enum under `src`
//! is neither `Copy` nor `Clone`: the ghost tokens, and the ghost records
//! and bundles of permissions verified code passes by value. Verus
//! (0.2026.10.18) verifies a `tracked` struct that derives both
//! (`verus/SEAMS.md`, "Status"), and verified code can copy
//! a value of a `Copy` type; ruling out `Clone` too, which `Copy`
//! requires, also rules out an explicit duplicate. Without either, code
//! that holds one token cannot make a second, the premise of facts such
//! as "`api::axiom_statics` makes the only [`LinePerm`]" and of each
//! axiom or lemma that uses up a token it takes by value.
//!
//! Each check is a top-level `const _: () = assert!(..)` under a `//`
//! comment that names its type. `verus/seams.sh --check` fails when a
//! `tracked` struct or enum under `src` has none here, and a seam's
//! status may cite one (`const assert: <label> in src/vspec/notcopy.rs`).
//! Review covers what that scan cannot see: a tracked type a macro
//! writes, and a plain struct of ghost permissions carried in
//! `Tracked<..>`, such as `huge::table::Own`.
//! `<T as NotCopy<_>>::HOLDS` names one item when `T` is not `Copy` and
//! two when it is: then the `_` has two solutions, which rustc rejects as
//! ambiguous (E0283). So the build fails, at each quantum and on each
//! target, and under Verus too, when a type gains either impl; the
//! asserted value is always true. `NotClone` is the same for `Clone`. A
//! generic type is checked at a `Copy` argument, so a derived impl,
//! which asks the same of the type argument, applies too, and one with a
//! lifetime at `'static`. Building with
//! `RUSTFLAGS='--cfg challoc_notcopy_control'` adds the controls at the
//! end, a `Copy` type and a `Clone` one that the checks must reject
//! (`verus/notcopy-control.sh`).

use super::atom::UsizePerm;
use super::byte::BytePerm;
use super::cbuf::{CCall, COut};
use super::count::CountPerm;
use super::entry::EntryPerm;
use super::errno::ThreadErrno;
use super::once::Once;
use super::unbacked::Unbacked;
use super::usable::Stored;
use super::user::UserMem;
use super::word::WordPerm;
use super::zero::{ZeroRaw, Zeroed};
use crate::api::{ForkHeld, LineCell, LinePerm, LineSeen, LineVal, Published, Seen};
use crate::api::{StatePerm, StateVal};
use crate::arena::{ArenaParts, ArenasParts, Held, Homed, RegVal, SparePerm};
use crate::bitmap::FullSeen;
use crate::class::SlotToks;
use crate::heap::{Built, CachePerms, FreshHeap, HeapPerm, HeapUse, Live, NodeHead};
use crate::heap::{SharedParts, TableVal, TableWrite, TcachesParts};
use crate::huge::RunToken;
use crate::layout::Reserved;
use crate::lock::{LockHeld, LockOrder};
use crate::meta::{Carve, MetaClaim};
use crate::purge::PurgedZero;
use crate::purge::background::StateHold;
use crate::slab::{DescPerm, StateTicket};
use crate::sys::SigMask;
use crate::sys::cgroup::{CellClaim, CellSeen};
use crate::sys::tls::{HeapLoan, TlsCell, TlsSlot};

/// `<T as NotCopy<_>>::HOLDS` compiles when `T` is not `Copy`.
pub(crate) trait NotCopy<A> {
    const HOLDS: bool = true;
}

impl<T: ?Sized> NotCopy<()> for T {}

/// The second impl, for a `Copy` type only.
pub(crate) enum IfCopy {}

impl<T: Copy> NotCopy<IfCopy> for T {}

/// `<T as NotClone<_>>::HOLDS` compiles when `T` is not `Clone`.
pub(crate) trait NotClone<A> {
    const HOLDS: bool = true;
}

impl<T: ?Sized> NotClone<()> for T {}

/// The second impl, for a `Clone` type only.
pub(crate) enum IfClone {}

impl<T: Clone> NotClone<IfClone> for T {}

// `LinePerm` is neither Copy nor Clone: `LINE_STATE`'s one permission.
const _: () = assert!(<LinePerm as NotCopy<_>>::HOLDS && <LinePerm as NotClone<_>>::HOLDS);

// `StatePerm` is neither Copy nor Clone: `STATE`'s one permission.
const _: () = assert!(<StatePerm as NotCopy<_>>::HOLDS && <StatePerm as NotClone<_>>::HOLDS);

// `HeapPerm` is neither Copy nor Clone: a thread heap's permission.
const _: () = assert!(<HeapPerm as NotCopy<_>>::HOLDS && <HeapPerm as NotClone<_>>::HOLDS);

// `Reserved` is neither Copy nor Clone: one live reservation.
const _: () = assert!(<Reserved as NotCopy<_>>::HOLDS && <Reserved as NotClone<_>>::HOLDS);

// `WordPerm` is neither Copy nor Clone: a bitmap word's permission.
const _: () = assert!(<WordPerm as NotCopy<_>>::HOLDS && <WordPerm as NotClone<_>>::HOLDS);

// `Published` is neither Copy nor Clone: the published permissions.
const _: () = assert!(<Published as NotCopy<_>>::HOLDS && <Published as NotClone<_>>::HOLDS);

// `Seen` is neither Copy nor Clone: the published permissions a thread saw.
const _: () = assert!(<Seen as NotCopy<_>>::HOLDS && <Seen as NotClone<_>>::HOLDS);

// `LineCell` is neither Copy nor Clone: the feature line's statics.
const _: () = assert!(<LineCell as NotCopy<_>>::HOLDS && <LineCell as NotClone<_>>::HOLDS);

// `LineVal` is neither Copy nor Clone: what the line's invariant holds.
const _: () = assert!(<LineVal as NotCopy<_>>::HOLDS && <LineVal as NotClone<_>>::HOLDS);

// `LineSeen` is neither Copy nor Clone: the line's permissions a thread saw.
const _: () = assert!(<LineSeen as NotCopy<_>>::HOLDS && <LineSeen as NotClone<_>>::HOLDS);

// `StateVal` is neither Copy nor Clone: what `STATE`'s invariant holds.
const _: () = assert!(<StateVal as NotCopy<_>>::HOLDS && <StateVal as NotClone<_>>::HOLDS);

// `ForkHeld` is neither Copy nor Clone: the hold of the fork lock list.
const _: () = assert!(<ForkHeld as NotCopy<_>>::HOLDS && <ForkHeld as NotClone<_>>::HOLDS);

// `Held` is neither Copy nor Clone: storage under construction.
const _: () = assert!(<Held<()> as NotCopy<_>>::HOLDS && <Held<()> as NotClone<_>>::HOLDS);

// `ArenaParts` is neither Copy nor Clone: an arena's field permissions.
const _: () = assert!(<ArenaParts as NotCopy<_>>::HOLDS && <ArenaParts as NotClone<_>>::HOLDS);

// `RegVal` is neither Copy nor Clone: what the registry's invariant holds.
const _: () = assert!(<RegVal as NotCopy<_>>::HOLDS && <RegVal as NotClone<_>>::HOLDS);

// `SparePerm` is neither Copy nor Clone: the permission to `spare`.
const _: () = assert!(<SparePerm as NotCopy<_>>::HOLDS && <SparePerm as NotClone<_>>::HOLDS);

// `Homed` is neither Copy nor Clone: a thread heap's arena ticket.
const _: () = assert!(<Homed as NotCopy<_>>::HOLDS && <Homed as NotClone<_>>::HOLDS);

// `ArenasParts` is neither Copy nor Clone: an `Arenas`' field permissions.
const _: () = assert!(<ArenasParts as NotCopy<_>>::HOLDS && <ArenasParts as NotClone<_>>::HOLDS);

// `FullSeen` is neither Copy nor Clone: the record of one read.
const _: () = assert!(<FullSeen as NotCopy<_>>::HOLDS && <FullSeen as NotClone<_>>::HOLDS);

// `SlotToks` is neither Copy nor Clone: a batch's slot tokens.
const _: () = assert!(<SlotToks as NotCopy<_>>::HOLDS && <SlotToks as NotClone<_>>::HOLDS);

// `CachePerms` is neither Copy nor Clone: a heap's cache memory.
const _: () = assert!(<CachePerms as NotCopy<_>>::HOLDS && <CachePerms as NotClone<_>>::HOLDS);

// `FreshHeap` is neither Copy nor Clone: a new heap's permission.
const _: () = assert!(<FreshHeap as NotCopy<_>>::HOLDS && <FreshHeap as NotClone<_>>::HOLDS);

// `Live` is neither Copy nor Clone: a live block's token.
const _: () = assert!(<Live as NotCopy<_>>::HOLDS && <Live as NotClone<_>>::HOLDS);

// `NodeHead` is neither Copy nor Clone: an `others` node's permissions.
const _: () = assert!(<NodeHead as NotCopy<_>>::HOLDS && <NodeHead as NotClone<_>>::HOLDS);

// `Built` is neither Copy nor Clone: the built `Shared`'s permission.
const _: () = assert!(<Built as NotCopy<_>>::HOLDS && <Built as NotClone<_>>::HOLDS);

// `SharedParts` is neither Copy nor Clone: a `Shared`'s field permissions.
const _: () = assert!(<SharedParts as NotCopy<_>>::HOLDS && <SharedParts as NotClone<_>>::HOLDS);

// `TableVal` is neither Copy nor Clone: what the table's invariant holds.
const _: () = assert!(<TableVal as NotCopy<_>>::HOLDS && <TableVal as NotClone<_>>::HOLDS);

// `TcachesParts` is neither Copy nor Clone: a `Tcaches`' field permissions.
const _: () = assert!(<TcachesParts as NotCopy<_>>::HOLDS && <TcachesParts as NotClone<_>>::HOLDS);

// `HeapUse` is neither Copy nor Clone: the right to one heap reference.
const _: () = assert!(<HeapUse as NotCopy<_>>::HOLDS && <HeapUse as NotClone<_>>::HOLDS);

// `TableWrite` is neither Copy nor Clone: the record of one table write.
const _: () = assert!(<TableWrite as NotCopy<_>>::HOLDS && <TableWrite as NotClone<_>>::HOLDS);

// `RunToken` is neither Copy nor Clone: a live huge run's token.
const _: () = assert!(<RunToken as NotCopy<_>>::HOLDS && <RunToken as NotClone<_>>::HOLDS);

// `LockHeld` is neither Copy nor Clone: one hold of a raw lock.
const _: () = assert!(<LockHeld as NotCopy<_>>::HOLDS && <LockHeld as NotClone<_>>::HOLDS);

// `LockOrder` is neither Copy nor Clone: a lock-order bound.
const _: () = assert!(
    <LockOrder<'static> as NotCopy<_>>::HOLDS && <LockOrder<'static> as NotClone<_>>::HOLDS
);

// `MetaClaim` is neither Copy nor Clone: an object's claim.
const _: () = assert!(<MetaClaim as NotCopy<_>>::HOLDS && <MetaClaim as NotClone<_>>::HOLDS);

// `Carve` is neither Copy nor Clone: what a table's carve hands back.
const _: () = assert!(<Carve as NotCopy<_>>::HOLDS && <Carve as NotClone<_>>::HOLDS);

// `PurgedZero` is neither Copy nor Clone: the record of one read.
const _: () = assert!(<PurgedZero as NotCopy<_>>::HOLDS && <PurgedZero as NotClone<_>>::HOLDS);

// `StateHold` is neither Copy nor Clone: the fork handlers' hold.
const _: () = assert!(<StateHold as NotCopy<_>>::HOLDS && <StateHold as NotClone<_>>::HOLDS);

// `DescPerm` is neither Copy nor Clone: one descriptor's permission.
const _: () = assert!(<DescPerm as NotCopy<_>>::HOLDS && <DescPerm as NotClone<_>>::HOLDS);

// `StateTicket` is neither Copy nor Clone: a descriptor's state record.
const _: () = assert!(<StateTicket as NotCopy<_>>::HOLDS && <StateTicket as NotClone<_>>::HOLDS);

// `SigMask` is neither Copy nor Clone: the thread's signal-mask token.
const _: () = assert!(<SigMask as NotCopy<_>>::HOLDS && <SigMask as NotClone<_>>::HOLDS);

// `CellSeen` is neither Copy nor Clone: the cell's permission a thread saw.
const _: () = assert!(<CellSeen as NotCopy<_>>::HOLDS && <CellSeen as NotClone<_>>::HOLDS);

// `CellClaim` is neither Copy nor Clone: the cell's claimed permission.
const _: () = assert!(<CellClaim as NotCopy<_>>::HOLDS && <CellClaim as NotClone<_>>::HOLDS);

// `TlsSlot` is neither Copy nor Clone: a thread's heap slot.
const _: () = assert!(<TlsSlot as NotCopy<_>>::HOLDS && <TlsSlot as NotClone<_>>::HOLDS);

// `TlsCell` is neither Copy nor Clone: a slot's cell.
const _: () = assert!(<TlsCell as NotCopy<_>>::HOLDS && <TlsCell as NotClone<_>>::HOLDS);

// `HeapLoan` is neither Copy nor Clone: a slot's heap loan.
const _: () = assert!(<HeapLoan as NotCopy<_>>::HOLDS && <HeapLoan as NotClone<_>>::HOLDS);

// `UsizePerm` is neither Copy nor Clone: an atomic `usize`'s permission.
const _: () = assert!(<UsizePerm as NotCopy<_>>::HOLDS && <UsizePerm as NotClone<_>>::HOLDS);

// `BytePerm` is neither Copy nor Clone: an atomic byte's permission.
const _: () = assert!(<BytePerm as NotCopy<_>>::HOLDS && <BytePerm as NotClone<_>>::HOLDS);

// `CountPerm` is neither Copy nor Clone: a counter's permission.
const _: () = assert!(<CountPerm as NotCopy<_>>::HOLDS && <CountPerm as NotClone<_>>::HOLDS);

// `EntryPerm` is neither Copy nor Clone: an atomic pointer's permission.
const _: () = assert!(
    <EntryPerm<(u8, u8)> as NotCopy<_>>::HOLDS && <EntryPerm<(u8, u8)> as NotClone<_>>::HOLDS
);

// `CCall` is neither Copy nor Clone: one call from C.
const _: () = assert!(<CCall as NotCopy<_>>::HOLDS && <CCall as NotClone<_>>::HOLDS);

// `COut` is neither Copy nor Clone: a C out-pointer's permission.
const _: () = assert!(<COut<()> as NotCopy<_>>::HOLDS && <COut<()> as NotClone<_>>::HOLDS);

// `Once` is neither Copy nor Clone: the right to take a step once.
const _: () = assert!(<Once as NotCopy<_>>::HOLDS && <Once as NotClone<_>>::HOLDS);

// `ThreadErrno` is neither Copy nor Clone: the calling thread's `errno`.
const _: () = assert!(<ThreadErrno as NotCopy<_>>::HOLDS && <ThreadErrno as NotClone<_>>::HOLDS);

// `Stored` is neither Copy nor Clone: an entry point's last store.
const _: () = assert!(<Stored as NotCopy<_>>::HOLDS && <Stored as NotClone<_>>::HOLDS);

// `UserMem` is neither Copy nor Clone: a permission to user memory.
const _: () = assert!(<UserMem as NotCopy<_>>::HOLDS && <UserMem as NotClone<_>>::HOLDS);

// `Unbacked` is neither Copy nor Clone: reserved, uncommitted bytes.
const _: () = assert!(<Unbacked as NotCopy<_>>::HOLDS && <Unbacked as NotClone<_>>::HOLDS);

// `Zeroed` is neither Copy nor Clone: bytes that read as zero.
const _: () = assert!(<Zeroed as NotCopy<_>>::HOLDS && <Zeroed as NotClone<_>>::HOLDS);

// `ZeroRaw` is neither Copy nor Clone: bytes that read as zero, with access.
const _: () = assert!(<ZeroRaw as NotCopy<_>>::HOLDS && <ZeroRaw as NotClone<_>>::HOLDS);

// The controls: a `Copy` type, which both checks must reject, and a
// `Clone` one, which `NotClone` must reject. Each `assert!` here is an
// error under the cfg.
#[cfg(challoc_notcopy_control)]
mod control {
    use super::{NotClone, NotCopy};

    #[derive(Clone, Copy)]
    struct CopyControl;

    #[derive(Clone)]
    struct CloneControl;

    const _: () = assert!(<CopyControl as NotCopy<_>>::HOLDS);
    const _: () = assert!(<CopyControl as NotClone<_>>::HOLDS);
    const _: () = assert!(<CloneControl as NotClone<_>>::HOLDS);
}
