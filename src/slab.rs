//! Slab descriptors (design 4).
//!
//! Every slab of every class has one [`SlabDesc`] in the metadata region:
//! a fixed 16-byte header holding the slab's state, its arena, its live
//! count and its decay stamp. Descriptors for class `c` form a dense array
//! indexed by slab number with a per-class stride ([`desc_stride`]). The
//! allocation bits are not here: they live in the class's slot bitmap
//! (`class/`, `SlabTable::bits`), one contiguous array per class that the
//! `free` check reads instead of a descriptor line.
//!
//! The header is `repr(C)` so that the offsets are fixed. Descriptors live
//! in memory that is never moved or freed, so accessors hand out references
//! with the caller's lifetime and no `Drop` is involved.
//!
//! `live` counts set bitmap bits plus in-flight claim reservations (see
//! `class/`); it is what drives the `Active -> Empty` transition.
//! `dirty_at` is the coarse monotonic time the slab became `Empty`, or 0 in
//! any other state.
//!
//! A class whose stride leaves room (32 or 64 bytes, [`desc_stride`]) has
//! a [`SlabPages`] in the 16 bytes after the header: which of the slab's
//! pages are closed, and when the slab last had a closable one
//! (docs/proposals/slab-pages.md, 3.2).

use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::layout::{Class, NCLASSES, build_classes, classes_for_page};
use crate::vspec::addr::addr_to_ptr;
#[cfg(verus_keep_ghost)]
use crate::vspec::addr::cast_provenance;
use vstd::prelude::*;
use vstd::raw_ptr::{PointsTo, ptr_ref};
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::{PtrData, ptr_mut_from_data};

verus! {

/// Size of the descriptor in bytes.
pub const HEADER: usize = 16;
/// Descriptor arrays are cache-line aligned, and a line of descriptors
/// never spans two arenas' slab groups (see [`desc_stride`]).
pub const DESC_ALIGN: usize = 64;

/// Abort on a state byte no [`State`] has. A pending check
/// (`sys::may_abort_pending`): the class lock's permissions record a valid
/// state for an owned slab, but no verified invariant bounds the byte of
/// `SlabDesc::state` or of a slab the reader does not own.
#[cold]
#[inline(never)]
fn bad_state_byte(v: u8) -> !
    requires
        crate::sys::may_abort_pending(),
{
    crate::sys::abort_pending(format_args!("slab: state byte {v:#x} is not a slab state"))
}

/// Lifecycle of a committed slab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Committed (or assigned to an arena's group and not yet committed),
    /// zero from the kernel, no slot ever claimed.
    Fresh = 0,
    /// At least one slot claimed since the slab was last `Fresh`, `Empty`
    /// or `Purged`.
    Active = 1,
    /// `live` reached zero; the pages are dirty and still resident.
    Empty = 2,
    /// `MADV_DONTNEED` applied; the pages read zero and are not resident.
    Purged = 3,
}

impl State {
    /// Decode a state byte. Aborts on a value no state has (4..=255): a
    /// corrupted byte must not read as `Purged`, which `take` treats as
    /// zero memory for `calloc`.
    #[inline]
    pub fn from_u8(v: u8) -> (r: State)
        requires
            crate::sys::may_abort_pending(),
        ensures
            state_byte(r) == v,
    {
        match State::try_from_u8(v) {
            Some(s) => s,
            None => bad_state_byte(v),
        }
    }

    /// [`from_u8`](State::from_u8) without the abort.
    #[inline]
    pub const fn try_from_u8(v: u8) -> (r: Option<State>)
        ensures
            r matches Some(s) ==> state_byte(s) == v,
    {
        match v {
            0 => Some(State::Fresh),
            1 => Some(State::Active),
            2 => Some(State::Empty),
            3 => Some(State::Purged),
            _ => None,
        }
    }

    /// Memory of a slab in this state is known to read as zero.
    #[inline]
    pub const fn is_zero(self) -> (r: bool)
        ensures
            r == (self == State::Fresh || self == State::Purged),
    {
        matches!(self, State::Fresh | State::Purged)
    }
}

/// The byte a descriptor's `state` field holds for `s` (`s as u8`).
pub open spec fn state_byte(s: State) -> u8 {
    match s {
        State::Fresh => 0,
        State::Active => 1,
        State::Empty => 2,
        State::Purged => 3,
    }
}

/// Descriptor header.
#[repr(C, align(16))]
pub struct SlabDesc {
    state: AtomicU8,
    /// `state + 1`, written with every state change. A descriptor whose
    /// `shadow != state + 1` has been written outside this API; the
    /// `check_zero` debug mode tests it.
    shadow: AtomicU8,
    arena: AtomicU16,
    live: AtomicU32,
    dirty_at: AtomicU64,
}

/// Ghost permission to one descriptor (`id`, its address): the `state`,
/// `dirty_at` and `arena` values it holds, the fields the class locks
/// protect (`init` and `set_state` also write `shadow`, and `init`
/// `live`, which it does not track), and the `closed` mask and `stamp`
/// of the descriptor's [`SlabPages`], at `id + 16`, where its class has
/// one (zero, and never written, where it has not). The slab-table proofs give it to the
/// holder of the lock that protects them. Not `Copy`, and its fields are
/// private to this module, which has no constructor: the one source is
/// the trusted `class::table::axiom_zero_desc`, once per descriptor of a
/// class table (it uses up the descriptor's zero bytes), but for the unit
/// tests' `Tracked::assume_new()`. `init`,
/// `set_state` and `set_dirty_at` require it to be their descriptor's.
// Ghost: for the specifications of the descriptor accessors.
#[allow(dead_code)]
pub tracked struct DescPerm {
    ghost id: usize,
    ghost state: u8,
    ghost dirty: u64,
    ghost arena: u16,
    ghost closed: u16,
    ghost stamp: u64,
}

impl DescPerm {
    /// The descriptor this permission is for ([`desc_id`]).
    pub closed spec fn id(&self) -> usize {
        self.id
    }

    /// The descriptor's `state` byte.
    pub closed spec fn state(&self) -> u8 {
        self.state
    }

    /// The descriptor's `dirty_at` stamp.
    pub closed spec fn dirty(&self) -> u64 {
        self.dirty
    }

    /// The descriptor's arena.
    pub closed spec fn arena(&self) -> u16 {
        self.arena
    }

    /// The slab's closed pages ([`SlabPages`]).
    pub closed spec fn closed(&self) -> u16 {
        self.closed
    }

    /// The slab's closable-page stamp ([`SlabPages`]).
    pub closed spec fn stamp(&self) -> u64 {
        self.stamp
    }

    /// Everything but the `state` byte and the `dirty_at` stamp is as
    /// in `o`: what a write of those through the permission keeps.
    pub open spec fn keeps_pages(&self, o: DescPerm) -> bool {
        &&& self.id() == o.id()
        &&& self.arena() == o.arena()
        &&& self.closed() == o.closed()
        &&& self.stamp() == o.stamp()
    }

    /// A ticket of the state this permission records now, for a claim
    /// that changes the state before it can use the fact (the zero-slot
    /// axioms of `class::ClassState::take`).
    pub proof fn ticket(tracked &self) -> (tracked r: StateTicket)
        ensures
            r.id() == self.id(),
            r.state() == self.state(),
    {
        StateTicket { id: self.id, state: self.state }
    }
}

/// Ghost: the state byte a descriptor's permission ([`DescPerm`]) recorded
/// when [`DescPerm::ticket`] made this, the one way to get one (its fields
/// are private to this module). Not `Copy`.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct StateTicket {
    ghost id: usize,
    ghost state: u8,
}

impl StateTicket {
    /// The descriptor the permission was for ([`desc_id`]).
    pub closed spec fn id(&self) -> usize {
        self.id
    }

    /// The state byte the permission recorded.
    pub closed spec fn state(&self) -> u8 {
        self.state
    }
}

/// The address of a descriptor: the identity a [`DescPerm`] is for (as
/// `vspec::word::word_addr` is a bitmap word's). `class::table`'s
/// `axiom_zero_desc` ties it to the descriptor's address when it mints
/// the permissions.
pub uninterp spec fn desc_id(d: &SlabDesc) -> usize;

/// `m` holds a permission for `d` under key `k`.
pub open spec fn perm_for(m: Map<nat, DescPerm>, k: nat, d: &SlabDesc) -> bool {
    m.contains_key(k) && m[k].id() == desc_id(d)
}

const fn strides_of(classes: &[Class; NCLASSES]) -> (out: [u32; NCLASSES])
    requires
        forall|i: int| 0 <= i < NCLASSES ==> #[trigger] classes[i].wf(),
{
    let mut out = [0u32; NCLASSES];
    let mut c = 0;
    while c < NCLASSES
        invariant
            0 <= c <= NCLASSES,
            forall|i: int| 0 <= i < NCLASSES ==> #[trigger] classes[i].wf(),
        decreases NCLASSES - c,
    {
        proof {
            assert(classes[c as int].wf());
        }
        // Slabs go to arenas in groups of `slabs_per_line` (class/), so
        // a stride of at least `DESC_ALIGN / slabs_per_line` keeps every
        // descriptor line inside one group: refills and flushes of two
        // arenas never write the same line.
        let per_line = classes[c].slabs_per_line();
        let s = DESC_ALIGN / per_line;
        out[c] = (if s > HEADER { s } else { HEADER }) as u32;
        c += 1;
    }
    out
}

const fn build_strides() -> [u32; NCLASSES] {
    strides_of(&build_classes())
}

/// The stride is compile-time although the slab geometry of the 512..1024
/// classes is fixed at init (`layout::adopt_page_geometry`): on every page
/// size a class can take (4 KiB to 64 KiB) those classes have at least four
/// slabs per bitmap line, so their stride is the header either way. This
/// checks it for every geometry the table can adopt.
const fn strides_match_every_page() -> bool {
    let base = build_strides();
    let mut lg_page = crate::layout::LG_MIN_SLAB;
    while lg_page <= crate::layout::LG_SMALL_SLAB
        invariant
            lg_page <= crate::layout::LG_SMALL_SLAB + 1,
        decreases crate::layout::LG_SMALL_SLAB + 1 - lg_page,
    {
        let s = strides_of(&classes_for_page(lg_page));
        let mut c = 0;
        while c < NCLASSES
            invariant
                0 <= c <= NCLASSES,
            decreases NCLASSES - c,
        {
            if s[c] != base[c] {
                return false;
            }
            c += 1;
        }
        lg_page += 1;
    }
    true
}

/// Per-class descriptor stride in bytes: 64 for classes whose slabs are
/// handed out singly, 32 for pairs, 16 (the header) from groups of four.
pub exec static DESC_STRIDE: [u32; NCLASSES] = build_strides();

/// The stride [`desc_stride`] reads for class `c`.
pub uninterp spec fn stride_of(c: int) -> usize;

/// Descriptor stride for `class` in bytes. Trusted: `DESC_STRIDE` is an
/// immutable static, so every read of an entry returns the same value,
/// which [`stride_of`] names (Verus does not read `exec static`s).
#[inline]
#[verifier::external_body]
pub fn desc_stride(class: usize) -> (r: usize)
    requires
        class < NCLASSES,
    ensures
        r <= u32::MAX,
        r == stride_of(class as int),
{
    DESC_STRIDE[class] as usize
}

impl SlabDesc {
    /// Descriptor of slab `slab` in the array for `class` starting at
    /// `base`, read through `perm`, the permission to it; the result is
    /// the descriptor `perm` holds.
    ///
    /// # Safety
    /// `base` must be the start of a descriptor array for `class` with at
    /// least `slab + 1` committed entries.
    #[inline]
    // The debug check mirrors the `requires` clause, `%` and all.
    #[allow(clippy::manual_is_multiple_of)]
    pub unsafe fn at(
        base: usize,
        class: usize,
        slab: usize,
        Tracked(perm): Tracked<&PointsTo<SlabDesc>>,
    ) -> (r: &SlabDesc)
        requires
            class < NCLASSES,
            base % DESC_ALIGN == 0,
            base + slab * stride_of(class as int) <= usize::MAX,
            perm.ptr() == ptr_mut_from_data::<SlabDesc>(
                PtrData { addr: (base + slab * stride_of(class as int)) as usize, provenance: cast_provenance(), metadata: () },
            ),
            perm.is_init(),
        ensures
            *r == perm.value(),
    {
        debug_assert!(base % DESC_ALIGN == 0);
        // The caller guarantees the entry is inside a committed array;
        // descriptors are plain atomics valid for any bit pattern. Valid to
        // read is not the same as meaningful: zero bytes decode as `Fresh`
        // with arena 0 and `live` 0, a state byte above 3 aborts in
        // `from_u8`, and garbage in 1..=3 reads as a plausible state, which
        // is why `take_group` cross-checks the arena against the owner word
        // and `check_zero` exists.
        ptr_ref(addr_to_ptr(base + slab * desc_stride(class)) as *const SlabDesc, Tracked(perm))
    }

    /// Initialise the descriptor of a slab handed to `arena`: `Fresh`, no
    /// live slots, through `p`, the descriptor's permission (ghost).
    /// Trusted: the five relaxed stores (`core`'s) write the fields the
    /// permission records as it says (`Fresh`, stamp 0, `arena`).
    // `p` is read only by the specification (ghost).
    #[allow(unused_variables)]
    #[verifier::external_body]
    pub fn init(&self, arena: u16, Tracked(p): Tracked<&mut DescPerm>)
        requires
            old(p).id() == desc_id(self),
        ensures
            final(p).id() == old(p).id(),
            final(p).state() == state_byte(State::Fresh),
            final(p).dirty() == 0,
            final(p).arena() == arena,
            final(p).closed() == old(p).closed(),
            final(p).stamp() == old(p).stamp(),
    {
        self.state.store(State::Fresh as u8, Ordering::Relaxed);
        self.shadow.store(State::Fresh as u8 + 1, Ordering::Relaxed);
        self.arena.store(arena, Ordering::Relaxed);
        self.live.store(0, Ordering::Relaxed);
        self.dirty_at.store(0, Ordering::Relaxed);
    }

    #[inline]
    pub fn state(&self) -> State
        requires
            crate::sys::may_abort_pending(),
    {
        State::from_u8(self.state.load(Ordering::Relaxed))
    }

    /// [`state`](SlabDesc::state) read by a holder of `m`, the permissions
    /// a class lock value holds: the state the permission under `k`
    /// records when that is this descriptor's.
    #[inline(always)]
    pub fn state_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: State)
        requires
            crate::sys::may_abort_pending(),
        ensures
            perm_for(*m, k, self) ==> state_byte(r) == m[k].state(),
    {
        State::from_u8(self.state_byte_held(Ghost(k), Tracked(m)))
    }

    /// The `state` byte, as [`state_held`](SlabDesc::state_held): the
    /// state the permission under `k` records when that is this
    /// descriptor's. Trusted ([`DescPerm`]): every write of the byte
    /// (`init`, `set_state`) takes this descriptor's permission and
    /// records the value written in it; only the unit tests pass a
    /// permission they made up (`Tracked::assume_new()`).
    // `k` and `m` are read only by the specification (ghost).
    #[allow(unused_variables)]
    #[inline(always)]
    #[verifier::external_body]
    fn state_byte_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: u8)
        ensures
            perm_for(*m, k, self) ==> r == m[k].state(),
    {
        self.state.load(Ordering::Relaxed)
    }

    /// [`dirty_at`](SlabDesc::dirty_at) read by a holder of `m`, as
    /// [`state_held`](SlabDesc::state_held): the stamp the permission
    /// under `k` records when that is this descriptor's. Trusted
    /// ([`DescPerm`]): every write of the stamp (`init`, `set_dirty_at`)
    /// takes this descriptor's permission and records the value written
    /// in it; only the unit tests pass a permission they made up
    /// (`Tracked::assume_new()`).
    // `k` and `m` are read only by the specification (ghost).
    #[allow(unused_variables)]
    #[inline(always)]
    #[verifier::external_body]
    pub fn dirty_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: u64)
        ensures
            perm_for(*m, k, self) ==> r == m[k].dirty(),
    {
        self.dirty_at.load(Ordering::Relaxed)
    }

    /// Only ever called with the class lock held, which holds `p`, the
    /// descriptor's permission (ghost), which records the new state.
    /// Trusted ([`DescPerm`]): the two relaxed stores write the byte the
    /// permission records, and its shadow.
    // `p` is read only by the specification (ghost).
    #[allow(unused_variables)]
    #[inline]
    #[verifier::external_body]
    pub fn set_state(&self, s: State, Tracked(p): Tracked<&mut DescPerm>)
        requires
            old(p).id() == desc_id(self),
        ensures
            final(p).keeps_pages(*old(p)),
            final(p).state() == state_byte(s),
            final(p).dirty() == old(p).dirty(),
    {
        self.state.store(s as u8, Ordering::Relaxed);
        self.shadow.store(s as u8 + 1, Ordering::Relaxed);
    }

    /// The raw state byte and its shadow (`state + 1` when consistent).
    #[inline]
    pub fn state_bytes(&self) -> (u8, u8) {
        (
            self.state.load(Ordering::Relaxed),
            self.shadow.load(Ordering::Relaxed),
        )
    }

    #[inline]
    pub fn arena(&self) -> u16 {
        self.arena.load(Ordering::Relaxed)
    }

    /// [`arena`](SlabDesc::arena) read by a holder of `m`, the permissions
    /// a class lock value holds: the arena the permission under `k`
    /// records when that is this descriptor's. Trusted ([`DescPerm`]):
    /// every write of the arena (`init`) takes this descriptor's
    /// permission and records the value written in it; only the unit tests
    /// write it otherwise (`set_arena`, a corrupt descriptor) or pass a
    /// permission they made up (`Tracked::assume_new()`).
    // `k` and `m` are read only by the specification (ghost).
    #[allow(unused_variables)]
    #[inline(always)]
    #[verifier::external_body]
    pub fn arena_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: u16)
        ensures
            perm_for(*m, k, self) ==> r == m[k].arena(),
    {
        self.arena.load(Ordering::Relaxed)
    }

    /// Tests: overwrite the slab's arena (a corrupt descriptor).
    #[cfg(test)]
    pub fn set_arena(&self, arena: u16) {
        self.arena.store(arena, Ordering::Relaxed);
    }

    /// Acquire: a releaser clears bits and then subtracts, so a reader that
    /// sees the smaller count also sees the cleared bits. `class/` relies
    /// on that to treat `nslots - live` as a lower bound on claimable slots.
    #[inline]
    pub fn live(&self) -> (r: usize)
        ensures
            r <= u32::MAX,
    {
        self.live.load(Ordering::Acquire) as usize
    }

    /// Add `n` to `live`, returning the previous value.
    #[inline]
    pub fn add_live(&self, n: usize) -> usize {
        self.live.fetch_add(n as u32, Ordering::AcqRel) as usize
    }

    /// Subtract `n` from `live`, returning the previous value. Aborts if
    /// that would go negative: more slots released than were ever claimed.
    /// A pending check (`sys::may_abort_pending`): no verified invariant
    /// bounds `n` by the count.
    #[inline]
    pub fn sub_live(&self, n: usize) -> (r: usize)
        requires
            crate::sys::may_abort_pending(),
        ensures
            r >= n,
    {
        let prev = self.live.fetch_sub(n as u32, Ordering::AcqRel) as usize;
        if prev < n {
            crate::sys::abort_pending(format_args!("slab: live count underflow ({prev} - {n})"));
        }
        prev
    }

    #[inline]
    pub fn dirty_at(&self) -> u64 {
        self.dirty_at.load(Ordering::Relaxed)
    }

    /// Only ever called with the class lock held, which holds `p`, as
    /// [`set_state`](SlabDesc::set_state). Trusted ([`DescPerm`]): the
    /// relaxed store writes the stamp the permission records.
    // `p` is read only by the specification (ghost).
    #[allow(unused_variables)]
    #[inline]
    #[verifier::external_body]
    pub fn set_dirty_at(&self, ns: u64, Tracked(p): Tracked<&mut DescPerm>)
        requires
            old(p).id() == desc_id(self),
        ensures
            final(p).keeps_pages(*old(p)),
            final(p).state() == old(p).state(),
            final(p).dirty() == ns,
    {
        self.dirty_at.store(ns, Ordering::Relaxed);
    }

    /// Free slots according to `live` (design 13.4 `nfree`). Transiently
    /// under-reports while a claim reservation is in flight.
    #[inline]
    pub fn nfree(&self, class: &crate::layout::Class) -> (r: usize)
        ensures
            r <= class.nslots,
    {
        class.nslots().saturating_sub(self.live())
    }
}

/// The page state of a slab (docs/proposals/slab-pages.md, 3.2), in the
/// 16 bytes after its descriptor's header, for a class whose stride is
/// 32 or 64 bytes. Zero, as the kernel hands the metadata pages out, is
/// every page open and no stamp, which is what a `Fresh` slab needs, so
/// `SlabDesc::init` does not write it.
#[repr(C, align(16))]
pub struct SlabPages {
    /// Bit `p` set: page `p` of the slab is closed, its slots unavailable
    /// to a claim. The complement of the proposal's `open` mask, so that
    /// zero is all open. Written under the class lock, `Release`; read by
    /// claims without it, `Acquire`.
    closed: AtomicU16,
    /// When the decay walk last found one of the slab's open pages
    /// closable (coarse monotonic nanoseconds), or 0.
    stamp: AtomicU64,
}

/// The address of a [`SlabPages`]: the identity a [`DescPerm`] at
/// `pages_id - 16` tracks its fields for. `class::table`'s
/// `axiom_zero_desc` ties it to the address when it mints the
/// permission to the bytes.
pub uninterp spec fn pages_id(pg: &SlabPages) -> usize;

/// `m` holds, under key `k`, the permission of the descriptor whose
/// [`SlabPages`] `pg` is.
pub open spec fn pages_for(m: Map<nat, DescPerm>, k: nat, pg: &SlabPages) -> bool {
    m.contains_key(k) && pages_id(pg) == m[k].id() + 16
}

impl SlabPages {
    /// The closed pages, without the class lock: a claim's view
    /// (docs/proposals/slab-pages.md, 3.4). `Acquire`, against
    /// [`set_pages`](SlabPages::set_pages)' `Release`, so that a claim
    /// that sees a page reopened sees everything the lock holder did
    /// before reopening it.
    #[inline]
    pub fn closed(&self) -> u16 {
        self.closed.load(Ordering::Acquire)
    }

    /// The closed pages and the stamp, read by a holder of `m`, the
    /// permissions a class lock value holds: the values the permission
    /// under `k` records when it is this slab's. Trusted ([`DescPerm`]):
    /// every write of the fields (`set_pages`) takes the descriptor's
    /// permission and records the values written in it, and before the
    /// first both are zero, as the permission (`class::table`'s
    /// `axiom_zero_desc`) records; only the unit tests pass a permission
    /// they made up (`Tracked::assume_new()`).
    // `k` and `m` are read only by the specification (ghost).
    #[allow(unused_variables)]
    #[inline(always)]
    #[verifier::external_body]
    pub fn pages_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: (u16, u64))
        ensures
            pages_for(*m, k, self) ==> r.0 == m[k].closed() && r.1 == m[k].stamp(),
    {
        (self.closed.load(Ordering::Relaxed), self.stamp.load(Ordering::Relaxed))
    }

    /// Write the closed pages and the stamp. Only ever called with the
    /// class lock held, which holds `p`, the descriptor's permission
    /// (ghost), which records the new values. Trusted ([`DescPerm`]): the
    /// two stores write the values the permission records; the mask's is
    /// `Release` for [`closed`](SlabPages::closed).
    // `p` is read only by the specification (ghost).
    #[allow(unused_variables)]
    #[inline]
    #[verifier::external_body]
    pub fn set_pages(&self, closed: u16, stamp: u64, Tracked(p): Tracked<&mut DescPerm>)
        requires
            pages_id(self) == old(p).id() + 16,
        ensures
            final(p).id() == old(p).id(),
            final(p).state() == old(p).state(),
            final(p).dirty() == old(p).dirty(),
            final(p).arena() == old(p).arena(),
            final(p).closed() == closed,
            final(p).stamp() == stamp,
    {
        self.stamp.store(stamp, Ordering::Relaxed);
        self.closed.store(closed, Ordering::Release);
    }
}

} // verus!

const _: () = assert!(core::mem::size_of::<SlabDesc>() == HEADER);
const _: () = assert!(core::mem::size_of::<SlabPages>() == HEADER);
const _: () = assert!(
    strides_match_every_page(),
    "a page-sized slab geometry changes a descriptor stride"
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::BASE_CLASSES;
    use crate::layout::CLASSES;
    use crate::meta::align_up;
    use std::vec;
    use std::vec::Vec;

    /// A descriptor array for one class backed by a `Vec`, aligned to a
    /// cache line.
    struct Descs {
        buf: Vec<u8>,
        base: usize,
        class: usize,
    }

    impl Descs {
        fn new(class: usize, n: usize) -> Descs {
            let buf = vec![0u8; n * desc_stride(class) + DESC_ALIGN];
            let base = align_up(buf.as_ptr() as usize, DESC_ALIGN);
            Descs { buf, base, class }
        }

        fn at(&self, slab: usize) -> &SlabDesc {
            assert!((slab + 1) * desc_stride(self.class) <= self.buf.len() - DESC_ALIGN);
            // SAFETY: `base` is the `DESC_ALIGN`-aligned start of `buf`, and the
            // assertion keeps entry `slab` inside it.
            unsafe { SlabDesc::at(self.base, self.class, slab, Tracked::assume_new()) }
        }
    }

    #[test]
    fn header_layout_is_fixed() {
        assert_eq!(core::mem::size_of::<SlabDesc>(), 16);
        assert_eq!(core::mem::align_of::<SlabDesc>(), 16);
        assert_eq!(core::mem::offset_of!(SlabDesc, state), 0);
        assert_eq!(core::mem::offset_of!(SlabDesc, arena), 2);
        assert_eq!(core::mem::offset_of!(SlabDesc, live), 4);
        assert_eq!(core::mem::offset_of!(SlabDesc, dirty_at), 8);
    }

    #[test]
    fn strides_keep_a_line_inside_one_group() {
        for (c, cls) in CLASSES.iter().enumerate() {
            let s = desc_stride(c);
            assert!(
                (HEADER..=DESC_ALIGN).contains(&s) && DESC_ALIGN.is_multiple_of(s),
                "class {c}: stride {s}"
            );
            // The descriptors sharing a line are consecutive slabs of one
            // line-sized group.
            assert!(DESC_ALIGN / s <= cls.slabs_per_line(), "class {c}");
            assert_eq!(
                s,
                (DESC_ALIGN / cls.slabs_per_line()).max(HEADER),
                "class {c}"
            );
        }
        // Singly handed-out slabs (the small classes) keep a line each.
        assert_eq!(desc_stride(0), DESC_ALIGN);
        // The 1024-byte class (4 slots, groups of 128) is down to the header.
        let c = CLASSES.iter().position(|c| c.size() == 1024).unwrap();
        assert_eq!(desc_stride(c), HEADER);
    }

    /// The stride table is compile-time; every geometry the class table can
    /// adopt at init (`layout::adopt_page_geometry`, pages of 4 KiB to
    /// 64 KiB) yields the same strides, because a raised class keeps at
    /// least four slabs per bitmap line.
    #[test]
    fn strides_are_the_same_for_every_page_geometry() {
        use crate::layout::{LG_MIN_SLAB, LG_SMALL_SLAB};
        assert!(strides_match_every_page());
        for lg_page in LG_MIN_SLAB..=LG_SMALL_SLAB {
            let t = classes_for_page(lg_page);
            let s = strides_of(&t);
            assert_eq!(s, DESC_STRIDE, "pages of 2^{lg_page}");
            for (c, cls) in t.iter().enumerate() {
                if cls != &BASE_CLASSES[c] {
                    assert!(cls.slabs_per_line() >= 4, "class {c} at 2^{lg_page}");
                    assert_eq!(s[c] as usize, HEADER);
                }
            }
        }
    }

    #[test]
    fn state_round_trips() {
        for s in [State::Fresh, State::Active, State::Empty, State::Purged] {
            assert_eq!(State::from_u8(s as u8), s);
        }
        assert!(State::Fresh.is_zero());
        assert!(State::Purged.is_zero());
        assert!(!State::Active.is_zero());
        assert!(!State::Empty.is_zero());
    }

    #[test]
    fn init_sets_the_header() {
        for (c, cls) in CLASSES.iter().enumerate() {
            let d = Descs::new(c, 3);
            for slab in 0..3 {
                let desc = d.at(slab);
                desc.set_state(State::Active, Tracked::assume_new());
                desc.add_live(3);
                desc.set_dirty_at(9, Tracked::assume_new());
                desc.init(7, Tracked::assume_new());
                assert_eq!(desc.state(), State::Fresh);
                assert_eq!(desc.arena(), 7);
                assert_eq!(desc.live(), 0);
                assert_eq!(desc.dirty_at(), 0);
                assert_eq!(desc.nfree(cls), cls.nslots());
            }
        }
    }

    #[test]
    fn neighbouring_descriptors_do_not_overlap() {
        for c in [0usize, 12, 15, 19] {
            let d = Descs::new(c, 4);
            for slab in 0..4 {
                d.at(slab).init(slab as u16, Tracked::assume_new());
            }
            d.at(1).add_live(5);
            d.at(1).set_state(State::Active, Tracked::assume_new());
            d.at(1)
                .set_dirty_at(0x1111_2222_3333_4444, Tracked::assume_new());
            for other in [0usize, 2, 3] {
                let desc = d.at(other);
                assert_eq!(desc.arena(), other as u16, "class {c}");
                assert_eq!(desc.state(), State::Fresh, "class {c}");
                assert_eq!(desc.live(), 0, "class {c}");
                assert_eq!(desc.dirty_at(), 0, "class {c}");
            }
        }
    }

    #[test]
    fn live_counter_and_state() {
        let cls = &CLASSES[3];
        let d = Descs::new(3, 1);
        let desc = d.at(0);
        desc.init(0, Tracked::assume_new());
        assert_eq!(desc.add_live(5), 0);
        assert_eq!(desc.add_live(2), 5);
        assert_eq!(desc.live(), 7);
        assert_eq!(desc.nfree(cls), cls.nslots() - 7);
        assert_eq!(desc.sub_live(7), 7);
        assert_eq!(desc.live(), 0);
        desc.set_state(State::Empty, Tracked::assume_new());
        desc.set_dirty_at(42, Tracked::assume_new());
        assert_eq!(desc.state(), State::Empty);
        assert_eq!(desc.dirty_at(), 42);
    }

    #[test]
    #[should_panic(expected = "live count underflow")]
    fn live_underflow_aborts() {
        let d = Descs::new(3, 1);
        let desc = d.at(0);
        desc.init(0, Tracked::assume_new());
        desc.add_live(1);
        desc.sub_live(2);
    }

    #[test]
    fn state_bytes_no_state_has_decode_to_none() {
        for v in 4..=255u8 {
            assert_eq!(State::try_from_u8(v), None, "{v}");
        }
        for v in 0..4u8 {
            assert_eq!(State::try_from_u8(v), Some(State::from_u8(v)));
        }
    }

    #[test]
    #[should_panic(expected = "is not a slab state")]
    fn a_state_byte_no_state_has_aborts() {
        let _ = State::from_u8(4);
    }
}

/// Kani harness for `desc_stride`, this file's seam that Kani can run
/// (`verus/ledger/src/slab.rs.md`).
#[cfg(kani)]
mod kani_desc_stride {
    use super::{DESC_STRIDE, desc_stride};
    use crate::layout::NCLASSES;

    /// `desc_stride`, `requires class < NCLASSES`, `ensures r <= u32::MAX,
    /// r == stride_of(class as int)`. For every `class < NCLASSES` the call
    /// returns without a panic, and it returns `DESC_STRIDE[class]` widened
    /// (its body), so at most `u32::MAX`. Two calls agree; that every call
    /// does, which `stride_of` names, rests on `DESC_STRIDE` being an
    /// immutable static, which Kani does not show.
    #[kani::proof]
    fn desc_stride_reads_the_table() {
        let class: usize = kani::any();
        kani::assume(class < NCLASSES);
        let r = desc_stride(class);
        assert!(r <= u32::MAX as usize);
        assert_eq!(r, DESC_STRIDE[class] as usize);
        assert_eq!(desc_stride(class), r);
    }
}
