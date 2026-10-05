//! Slab descriptors (design.md section 4).
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

use crate::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::layout::{Class, NCLASSES, build_classes, classes_for_page};
use crate::vspec::addr::addr_to_ptr;
#[cfg(verus_only)]
use crate::vspec::addr::cast_provenance;
use vstd::prelude::*;
use vstd::raw_ptr::{PointsTo, ptr_ref};
#[cfg(verus_only)]
use vstd::raw_ptr::{PtrData, ptr_mut_from_data};

verus! {

/// Size of the descriptor in bytes.
pub const HEADER: usize = 16;
/// Descriptor arrays are cache-line aligned, and a line of descriptors
/// never spans two arenas' slab groups (see [`desc_stride`]).
pub const DESC_ALIGN: usize = 64;

#[cold]
#[inline(never)]
fn bad_state_byte(v: u8) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!("slab: state byte {v:#x} is not a slab state"))
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
            crate::sys::may_abort(),
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

/// The address of a descriptor: the identity a [`DescPerm`] is for (as
/// `vspec::word::word_addr` is a bitmap word's). `class::table`'s
/// `axiom_init_descs` ties it to the descriptor's address when it mints
/// the permissions.
pub uninterp spec fn desc_id(d: &SlabDesc) -> usize;

/// Ghost permission to the lock-protected fields of the descriptor at
/// [`id`](DescPerm::id): the `state`, `dirty_at` and `arena` values it
/// holds. Not `Copy`, and nothing builds one but the trusted
/// `class::table::axiom_init_descs` (the fields are private to this
/// module, which has no constructor), once per descriptor of a class
/// table. The class table's commit lock holds the permissions of the
/// slabs not yet handed out, and `take_group` moves a group's to the
/// taking arena's class lock value (`class::Inner`), which keeps them
/// for good: so the holder of an arena's class lock holds the
/// permissions of the slabs the arena owns, and the accessors below that
/// take one see the value the last write through one stored.
///
/// The type is plain ghost data; what is trusted is the accessors that
/// take one, each the one `core` atomic access its body makes (two
/// stores for `set_state`, five for `init`), as `core` documents, and
/// that every write of those three fields goes through them (`SlabDesc`
/// keeps the fields private; the one writer without a permission,
/// `set_arena`, is test-only). The proof model is sequentially
/// consistent; the `Relaxed` orderings are right because every access
/// through a permission is made under the lock that holds it.
///
/// Its fields are private, so only this module could build one: no code
/// here may, as `table::axiom_init_descs` is the only source of
/// permissions and a second one for a descriptor would let two holders
/// write it.
// Ghost: read only by the specifications.
#[allow(dead_code)]
pub tracked struct DescPerm {
    ghost id: usize,
    ghost state: u8,
    ghost dirty: u64,
    ghost arena: u16,
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
}

/// `m` holds a permission for `d` under key `k`.
pub open spec fn perm_for(m: Map<nat, DescPerm>, k: nat, d: &SlabDesc) -> bool {
    m.contains_key(k) && m[k].id() == desc_id(d)
}

const fn strides_of(classes: &[Class; NCLASSES]) -> (out: [u32; NCLASSES])
    requires forall|i: int| 0 <= i < NCLASSES ==> #[trigger] classes[i].wf(),
{
    let mut out = [0u32; NCLASSES];
    let mut c = 0;
    while c < NCLASSES
        invariant 0 <= c <= NCLASSES, forall|i: int| 0 <= i < NCLASSES ==> #[trigger] classes[i].wf(),
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
        invariant lg_page <= crate::layout::LG_SMALL_SLAB + 1,
        decreases crate::layout::LG_SMALL_SLAB + 1 - lg_page,
    {
        let s = strides_of(&classes_for_page(lg_page));
        let mut c = 0;
        while c < NCLASSES
            invariant 0 <= c <= NCLASSES,
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
    requires class < NCLASSES,
    ensures r <= u32::MAX, r == stride_of(class as int),
{
    DESC_STRIDE[class] as usize
}

impl SlabDesc {
    /// Descriptor of slab `slab` in the array for `class` starting at
    /// `base`, through its permission `perm`.
    ///
    /// # Safety
    /// `base` must be the start of a descriptor array for `class` with at
    /// least `slab + 1` committed entries.
    #[inline]
    pub unsafe fn at<'a>(base: usize, class: usize, slab: usize, Tracked(perm): Tracked<&'a PointsTo<SlabDesc>>) -> (r: &'a SlabDesc)
        requires
            class < NCLASSES,
            base % DESC_ALIGN == 0,
            base + slab * stride_of(class as int) <= usize::MAX,
            perm.ptr() == ptr_mut_from_data::<SlabDesc>(PtrData { addr: (base + slab * stride_of(class as int)) as usize, provenance: cast_provenance(), metadata: () }),
            perm.is_init(),
        ensures
            *r == perm.value(),
    {
        debug_assert!(base % DESC_ALIGN == 0);
        // SAFETY: the caller guarantees the entry is inside a committed array;
        // descriptors are plain atomics valid for any bit pattern. Valid to
        // read is not the same as meaningful: zero bytes decode as `Fresh`
        // with arena 0 and `live` 0, a state byte above 3 aborts in
        // `from_u8`, and garbage in 1..=3 reads as a plausible state, which
        // is why `take_group` cross-checks the arena against the owner word
        // and `check_zero` exists.
        ptr_ref(addr_to_ptr(base + slab * desc_stride(class)) as *const SlabDesc, Tracked(perm))
    }

    /// Initialise the descriptor of a slab handed to `arena`: `Fresh`, no
    /// live slots. Trusted ([`DescPerm`]).
    #[verifier::external_body]
    pub fn init(&self, arena: u16, Tracked(p): Tracked<&mut DescPerm>)
        requires
            old(p).id() == desc_id(self),
        ensures
            final(p).id() == old(p).id(),
            final(p).state() == state_byte(State::Fresh),
            final(p).dirty() == 0,
            final(p).arena() == arena,
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
            crate::sys::may_abort(),
    {
        State::from_u8(self.state.load(Ordering::Relaxed))
    }

    /// [`state`](SlabDesc::state) read by a holder of `m`, the permissions
    /// a class lock value holds: the state the permission under `k`
    /// records when that is this descriptor's.
    #[inline(always)]
    pub fn state_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: State)
        requires
            crate::sys::may_abort(),
        ensures
            perm_for(*m, k, self) ==> state_byte(r) == m[k].state(),
    {
        State::from_u8(self.state_byte_held(Ghost(k), Tracked(m)))
    }

    /// The `state` byte, as [`state_held`](SlabDesc::state_held).
    /// Trusted ([`DescPerm`]).
    #[inline(always)]
    #[verifier::external_body]
    fn state_byte_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: u8)
        ensures
            perm_for(*m, k, self) ==> r == m[k].state(),
    {
        self.state.load(Ordering::Relaxed)
    }

    /// [`dirty_at`](SlabDesc::dirty_at), as
    /// [`state_held`](SlabDesc::state_held). Trusted ([`DescPerm`]).
    #[inline(always)]
    #[verifier::external_body]
    pub fn dirty_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: u64)
        ensures
            perm_for(*m, k, self) ==> r == m[k].dirty(),
    {
        self.dirty_at.load(Ordering::Relaxed)
    }

    /// [`arena`](SlabDesc::arena), as [`state_held`](SlabDesc::state_held).
    /// Trusted ([`DescPerm`]).
    #[inline(always)]
    #[verifier::external_body]
    pub fn arena_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: u16)
        ensures
            perm_for(*m, k, self) ==> r == m[k].arena(),
    {
        self.arena.load(Ordering::Relaxed)
    }

    /// Only ever called with the class lock held, which holds `p`.
    /// Trusted ([`DescPerm`]).
    #[inline]
    #[verifier::external_body]
    pub fn set_state(&self, s: State, Tracked(p): Tracked<&mut DescPerm>)
        requires
            old(p).id() == desc_id(self),
        ensures
            final(p).id() == old(p).id(),
            final(p).state() == state_byte(s),
            final(p).dirty() == old(p).dirty(),
            final(p).arena() == old(p).arena(),
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
    #[inline]
    #[verifier::external_body]
    pub fn sub_live(&self, n: usize) -> usize
        requires
            crate::sys::may_abort(),
    {
        let prev = self.live.fetch_sub(n as u32, Ordering::AcqRel) as usize;
        assert!(prev >= n, "slab: live count underflow ({prev} - {n})");
        prev
    }

    #[inline]
    pub fn dirty_at(&self) -> u64 {
        self.dirty_at.load(Ordering::Relaxed)
    }

    /// Only ever called with the class lock held, which holds `p`.
    /// Trusted ([`DescPerm`]).
    #[inline]
    #[verifier::external_body]
    pub fn set_dirty_at(&self, ns: u64, Tracked(p): Tracked<&mut DescPerm>)
        requires
            old(p).id() == desc_id(self),
        ensures
            final(p).id() == old(p).id(),
            final(p).state() == old(p).state(),
            final(p).dirty() == ns,
            final(p).arena() == old(p).arena(),
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

} // verus!

const _: () = assert!(core::mem::size_of::<SlabDesc>() == HEADER);
const _: () = assert!(
    strides_match_every_page(),
    "a page-sized slab geometry changes a descriptor stride"
);

/// Kani harness for `desc_stride` (`verus/seams/slab-desc_stride.md`).
#[cfg(kani)]
mod kani_desc_stride {
    use super::{DESC_STRIDE, desc_stride};
    use crate::layout::NCLASSES;

    /// S199 `desc_stride`, `requires class < NCLASSES`, `ensures r <=
    /// u32::MAX, r == stride_of(class as int)`: for every `class <
    /// NCLASSES` the call returns without a panic, and what it returns
    /// is `DESC_STRIDE[class]` widened (so at most `u32::MAX`), the same
    /// on every call: a function of `class` alone, which `stride_of`
    /// names.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::BASE_CLASSES;
    use crate::layout::CLASSES;
    use std::vec;
    use std::vec::Vec;

    fn align_up(x: usize, a: usize) -> usize {
        (x + a - 1) & !(a - 1)
    }

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
                s >= HEADER && s <= DESC_ALIGN && DESC_ALIGN % s == 0,
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
