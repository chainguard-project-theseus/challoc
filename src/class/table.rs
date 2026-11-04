//! The per-class [`SlabTable`]: the class region's descriptor array,
//! class bitmap and group owner array, the hand-out watermark and the
//! commit lock; groups go to arenas here (`take_group`) and slabs are
//! committed here (`commit_run`, `commit_slab`).

use super::Inner;
use super::abort::{guard_free, invalid_free, invalid_pointer, never_committed, owner_mismatch};
#[cfg(verus_keep_ghost)]
use super::owned_rec;
use super::sets::Lazy;
use crate::arena::Published;
#[cfg(verus_keep_ghost)]
use crate::arena::process_pub_loc;
use crate::bitmap::{AtomicBitmap, WordInv};
#[cfg(verus_keep_ghost)]
use crate::bitmap::{
    Slots, fresh_mark, fresh_marks, has_bit, init_mark, init_marks, subset, words_wf,
};
use crate::layout::{CLASSES, Class, Regions};
#[cfg(verus_keep_ghost)]
use crate::layout::{NCLASSES, class_table, process_regions};
use crate::lock::{Lock, LockOrder, PAD_ALIGN, Padded, RawLock};
#[cfg(verus_keep_ghost)]
use crate::lock::{lock_init, rank_meta, rank_table};
use crate::meta::Meta;
use crate::purge;
use crate::slab::{DESC_ALIGN, DescPerm, HEADER, SlabDesc, desc_stride};
#[cfg(verus_keep_ghost)]
use crate::slab::{desc_id, stride_of};
use crate::sys::{self, Errno, MADV_GUARD_INSTALL, PROT_NONE, PROT_READ, PROT_WRITE};
use crate::vspec::addr::addr_to_ptr;
#[cfg(verus_keep_ghost)]
use crate::vspec::addr::cast_provenance;
use crate::vspec::atom::{
    SeenVal, UsizePerm, load_u16_at, load_usize, load_usize_seen, new_usize, store_usize,
    store_usize_at,
};
#[cfg(verus_keep_ghost)]
use crate::vspec::atom::{u16_value, usize_id, usize_id_at};
use crate::vspec::check::check;
use crate::vspec::once::Once;
#[cfg(verus_keep_ghost)]
use crate::vspec::once::Step;
#[cfg(verus_keep_ghost)]
use crate::vspec::word::{WordPerm, word_addr};
#[cfg(verus_keep_ghost)]
use crate::vspec::zero::ZeroRaw;
use core::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};
#[cfg(verus_keep_ghost)]
use vstd::arithmetic::power2::pow2;
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
use vstd::prelude::*;
use vstd::raw_ptr::{PointsTo, ptr_ref};
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::{PtrData, ptr_mut_from_data};
use vstd::resource::Loc;
use vstd::resource::ghost_var::{GhostVar, GhostVarAuth};
use vstd::resource::set::{GhostPersistentSubset, GhostSetAuth, GhostSingleton, GhostSubset};
#[cfg(verus_keep_ghost)]
use vstd::std_specs::bits::u64_trailing_zeros;

verus! {

pub(crate) struct TableInner {
    pub(super) descs: Lazy,
    /// The class bitmap (see [`SlabTable::bits`]).
    pub(super) bits: Lazy,
    /// The group owner array (see [`SlabTable::owner`]).
    pub(super) owners: Lazy,
    /// Next slab number not yet handed to any arena (`high_water + 1` in
    /// the design, at group granularity).
    pub(super) next_slab: usize,
    /// Guard slabs committed (never handed out). A statistic: the count
    /// wraps (`wrapping_add`, the same add as `+= 1` in the release
    /// build, which checks no overflow).
    pub(super) guards: usize,
    /// Ghost: the table's page, hand-out group and slab count, fixed at
    /// `new` (the lock's frame, `TableInner`'s `LockSpec` impl), for
    /// [`TableInner::inv`].
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) page: Ghost<usize>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) group: Ghost<usize>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) nslabs: Ghost<usize>,
    /// Ghost: the class's bits per slab and the bitmap markers' authority
    /// (`bitmap.rs`, "Slot tokens"), fixed at `new`, and the init markers
    /// of every bit of the slabs not yet handed out, with the owner-store
    /// markers ([`owner_marks`]) of those slabs, which `take_group` takes
    /// one group at a time (for `AtomicBitmap::init`, or as they are for a
    /// guard slab, and for `store_owner`).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) sb: Ghost<usize>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) mloc: Ghost<Loc>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) fresh: Tracked<GhostSubset<int>>,
    /// Ghost: the permissions ([`DescPerm`]) of the descriptors of the
    /// slabs not yet handed out, each as `new` made it (zero), which
    /// `take_group` moves to the taking arena's class lock value; and
    /// where the descriptor array starts and its stride, fixed at `new`,
    /// which the permissions are for.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) dperms: Tracked<Map<int, DescPerm>>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) dbase: Ghost<int>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) dstride: Ghost<int>,
    /// Ghost: the location of the owner words' witness markers
    /// ([`OwnerPred`]), fixed at `new`, and those markers, persistent, of
    /// every slab below `next_slab`: the groups handed out, whose owner
    /// words were stored. `take_group` adds each group's as it stores its
    /// word, and copies the set into the watermark's invariant with the
    /// watermark it stores ([`WatermarkPred`]).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) sloc: Ghost<Loc>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) seen: Tracked<GhostPersistentSubset<int>>,
    /// Ghost: `next_slab`, as one half of a ghost variable whose other
    /// half the watermark's invariant holds with the watermark
    /// ([`WatermarkPred`]), and the variable's location, fixed at `new`.
    /// `take_group` updates both halves together when it stores the
    /// watermark, so the watermark is `next_slab` whenever the lock is
    /// free.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) wloc: Ghost<Loc>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) wm: Tracked<GhostVarAuth<usize>>,
    /// A copy of the watermark the `free` fast path reads from its
    /// per-class line (`heap::ClassLine`), stored right after the
    /// watermark itself, so it is never ahead of it; null until
    /// [`SlabTable::mirror_watermark`].
    pub(super) mirror: *const AtomicUsize,
    /// Ghost: the copy's invariant once `mirror_watermark` has set the
    /// pointer ([`TableInner::inv`]): the one over the atomic at `mirror`,
    /// the slab count and a ghost variable ([`CommittedPred`]).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) mirror_inv: Tracked<Option<&'static CommittedInv>>,
    /// Ghost: the other half of the copy's ghost variable, `next_slab`
    /// while the lock is free. `new_mirror` makes both halves,
    /// `mirror_watermark` sets them to `next_slab` and keeps this one, and
    /// `take_group` updates them together when it stores the copy.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) mauth: Tracked<Option<GhostVarAuth<usize>>>,
}

/// The facts [`Lazy::ensure`] needs of a table committed in `page` units
/// (its claim to its uncommitted pages among them, [`Lazy::claimed`]),
/// which it keeps.
pub(super) open spec fn lazy_ok(l: Lazy, page: usize) -> bool {
    &&& l.spec_len() <= 0x4000_0000_0000_0000
    &&& l.spec_committed() <= l.spec_len() + page
    &&& l.spec_base() + l.spec_len() + page <= usize::MAX
    &&& l.claimed()
}

/// `p` is the permission of the descriptor at `addr` as the reservation
/// holds it before any write: all zero (`Fresh`, stamp 0, arena 0).
pub(super) open spec fn desc_fresh(p: DescPerm, addr: int) -> bool {
    &&& p.id() == addr
    &&& p.state() == 0
    &&& p.dirty() == 0
    &&& p.arena() == 0
}

/// `m` holds slab `s`'s descriptor permission as `new` made it, for a
/// descriptor array at `base` with stride `stride`.
pub(super) open spec fn dfresh(m: Map<int, DescPerm>, base: int, stride: int, s: int) -> bool {
    &&& m.contains_key(s)
    &&& desc_fresh(m[s], base + s * stride)
}

/// `m` holds the permission of slab `x`'s descriptor in `t` as
/// [`SlabTable::take_group`] hands it to `arena`: `Fresh`, stamp 0, and,
/// unless `x` is a guard slab (whose descriptor stays zero), `arena`'s.
pub(super) open spec fn desc_handed(m: Map<nat, DescPerm>, t: SlabTable, x: nat, arena: u16) -> bool {
    &&& m.contains_key(x)
    &&& m[x].id() == desc_id(&desc_at(t, x as usize))
    &&& m[x].state() == 0
    &&& m[x].dirty() == 0
    &&& !t.spec_is_guard(x as usize) ==> m[x].arena() == arena
}

impl TableInner {
    /// `self` is `o` but for the init markers and the descriptor
    /// permissions.
    pub(super) open spec fn same_but_fresh(self, o: TableInner) -> bool {
        &&& self.descs == o.descs
        &&& self.bits == o.bits
        &&& self.owners == o.owners
        &&& self.next_slab == o.next_slab
        &&& self.guards == o.guards
        &&& self.page == o.page
        &&& self.group == o.group
        &&& self.nslabs == o.nslabs
        &&& self.sb == o.sb
        &&& self.mloc == o.mloc
        &&& self.dbase == o.dbase
        &&& self.dstride == o.dstride
        &&& self.mirror == o.mirror
        &&& self.mirror_inv == o.mirror_inv
        &&& self.mauth == o.mauth
        &&& self.sloc == o.sloc
        &&& self.seen == o.seen
        &&& self.wloc == o.wloc
        &&& self.wm == o.wm
    }

    /// The slabs handed out stay below the largest slab count a region
    /// can have and are whole hand-out groups until the region's end (the
    /// guard count is a statistic, with no bound here); the three tables are committed in
    /// the process's page ([`lazy_ok`], what `Lazy::ensure` takes). The
    /// commit lock holds the init markers of every bit and the owner-store
    /// markers of every slab from the watermark on, and those slabs'
    /// descriptor permissions as `new` made them. Once
    /// [`SlabTable::mirror_watermark`] has set the watermark copy's
    /// pointer, it holds the copy's invariant (`take_group` stores into
    /// it), and its half of the copy's ghost variable is `next_slab`. Its
    /// persistent owner-word witness markers cover every slab below
    /// `next_slab`, and its half of the watermark's ghost variable is
    /// `next_slab`.
    pub(super) open spec fn inv(self) -> bool {
        &&& self.next_slab <= usize::MAX >> 12
        &&& self.group@ >= 1
        &&& self.next_slab < self.nslabs@ ==> self.next_slab % self.group@ == 0
        &&& 1 <= self.page@ <= 0x8000_0000_0000_0000
        &&& self.page@ == crate::meta::process_page()
        &&& lazy_ok(self.descs, self.page@)
        &&& lazy_ok(self.bits, self.page@)
        &&& lazy_ok(self.owners, self.page@)
        &&& self.fresh@.id() == self.mloc@
        &&& init_marks(self.next_slab * self.sb@, self.nslabs@ * self.sb@).subset_of(self.fresh@@)
        &&& owner_marks(self.next_slab as int, self.nslabs@ as int).subset_of(self.fresh@@)
        &&& forall|s: int| self.next_slab <= s < self.nslabs@ ==> #[trigger] dfresh(self.dperms@, self.dbase@, self.dstride@, s)
        &&& self.mirror@.addr != 0 ==> (self.mirror_inv@ matches Some(i) && self.mauth@ matches Some(a)
            && i.constant() == (usize_id_at(self.mirror), self.nslabs@, a.id()) && i.namespace() == committed_ns()
            && a@ == self.next_slab)
        &&& self.seen@.id() == self.sloc@
        &&& owner_marks(0, self.next_slab as int).subset_of(self.seen@@)
        &&& self.wm@.id() == self.wloc@
        &&& self.wm@@ == self.next_slab
    }
}

/// Offset of class `class`'s descriptor array, bitmap and owner array
/// inside their reservations.
///
/// The per-class tables are reserved back to back from the top of the
/// metadata region, and each is a power of two of at least 1 MiB, so
/// without this every table would start on a 1 MiB boundary and slab `k`'s
/// entry of every class would map to the same L1 set (16 KiB per way) and
/// the same L2 set (1 MiB per way). The `free` fast path loads one bitmap
/// word (and one owner word) per call, so that is a 52-way conflict on the
/// hottest lines it has. Three lines per class, and `gcd(3, 128) == 1`, so
/// all 52 classes land in distinct sets of both caches; a multiple of
/// [`DESC_ALIGN`] keeps the descriptors aligned.
#[inline]
pub const fn desc_stagger(class: usize) -> (r: usize)
    requires
        class < NCLASSES,
    ensures
        r == class * 3 * PAD_ALIGN,
{
    class * 3 * PAD_ALIGN
}

/// What every arena's [`ClassState`](super::ClassState) for one class
/// shares (design 13.3: arenas are sets of class states over the *same*
/// class regions): the region's geometry, the descriptor array, the class
/// bitmap, the group owner array and the hand-out watermark. Slab groups
/// go in address order to whichever arena needs one next, so arenas
/// interleave in the region at group granularity; `free` reads the owner
/// array to route a slot back to a cache of its arena.
///
/// Layout: the fields before `inner` are written once by `new` (or, for
/// `committed_slabs`, only when a group is handed out) and read by every
/// `free`, while `inner` holds the commit lock. `repr(C)` keeps the
/// read-mostly fields first and [`Padded`] starts `inner` on its own line.
/// The fields the `free` fast path reads (`bits_base`, `owners_base`,
/// `committed_slabs`, `slab_bits`, `lg_group`) sit in the first 32 bytes:
/// one L1 line even on a core with 64-byte lines. The slot geometry is not
/// copied here on purpose; `free` reads it from the dense
/// `Regions::class_base` and `CLASSES` arrays (see `Shared::checked_slot`).
///
/// The stride of `[SlabTable; NCLASSES]` (384 bytes) is not a multiple of
/// 512 for the reason given on [`ClassState`](super::ClassState).
#[repr(C, align(128))]
pub struct SlabTable {
    /// Start of the class bitmap: bit `slab * slab_bits + slot` is the
    /// allocation bit of `slot` in `slab` (design 4).
    pub(super) bits_base: usize,
    /// Start of the owner array: one `AtomicU16` per group of
    /// `1 << lg_group` slabs, the arena the group was handed to.
    pub(super) owners_base: usize,
    /// Slabs handed to an arena so far (the hand-out watermark; a multiple
    /// of the group size except at the end of the region). Their
    /// descriptors, bitmap words and owner words exist, readable without
    /// the lock so the `free` check can reject pointers into never-handed
    /// slabs cleanly; a slab below it may still be uncommitted (its arena
    /// has not reached it yet), in which case its bits are clear and its
    /// descriptor is `Fresh`.
    pub(super) committed_slabs: AtomicUsize,
    /// `Class::slab_bits`, kept here so the fast path reads it with the
    /// table line.
    pub(super) slab_bits: u16,
    pub(super) class: u16,
    /// `log2(group)`.
    pub(super) lg_group: u8,
    /// `log2(slabs_per_page)`: the guard test on the sized fast path is a
    /// shift, an add and a mask (`is_guard_slab`).
    pub(super) lg_slabs_per_page: u8,
    pub(super) _pad: [u8; 2],
    /// `guard_interval - 1`: every `guard_interval`-th page group is a
    /// guard group and the interval is a power of two, so `(group + 1) &
    /// guard_mask == 0` is the test. With guards off the mask is all ones
    /// and no page group a region can have matches (`u32::MAX` would).
    pub(super) guard_mask: u32,
    pub(super) lg_page: u32,
    /// `desc_stride(class)`.
    pub(super) desc_stride: u32,
    /// `max(1, page / slab_size)`, a power of two. Slabs are committed,
    /// guarded and purged in page-sized groups, so on a kernel whose page is
    /// larger than this class's slab a group of `slabs_per_page` consecutive
    /// slabs is committed when its first slab is, is one guard region when
    /// it is a guard group, and is purged only when every slab in it is
    /// idle and owned by the purging arena. No supported kernel has such a
    /// class (`layout::adopt_page_geometry`
    /// raises a class's slab to the page at init, up to 64 KiB: a group of
    /// `Empty` sub-page slabs was never all idle under Valkey and held
    /// 79.5 MiB after its delete phase at 64 KiB pages), so this is 1
    /// everywhere but in the tests that fake a page above the slab, which
    /// keep the path working for a page above 64 KiB.
    pub(super) slabs_per_page: u32,
    /// Slabs handed to an arena at a time: `max(slabs_per_line,
    /// slabs_per_page)`, a power of two, so a group is whole bitmap lines
    /// and whole page groups.
    pub(super) group: u32,
    /// Start of this class's region.
    pub(super) base: usize,
    /// Number of slabs in the region.
    pub(super) nslabs: usize,
    /// Start of the descriptor array.
    pub(super) desc_base: usize,
    pub(crate) inner: Padded<Lock<TableInner>>,
    pub(super) _tail: [u8; TABLE_TAIL],
    /// Ghost (no bytes): the class bitmap's slot-token authority
    /// (`bitmap.rs`, "Slot tokens"), and the invariant of each bitmap word
    /// ([`WordInv`]), which holds the word's permission and the tokens of
    /// its clear slots, frozen (`new`).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) slot_auth: Tracked<GhostSetAuth<int>>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) word_invs: Tracked<&'static Map<int, WordInv>>,
    /// Ghost (no bytes): a permission for each bitmap word, made by `new`
    /// and frozen ([`SlabTable::perms_ok`]), which
    /// [`SlabTable::bits_unchecked`] borrows the words through.
    pub(super) word_perms: Tracked<&'static Map<int, PointsTo<AtomicU64>>>,
    /// Ghost (no bytes): the authority of the bitmap words' markers
    /// (`bitmap.rs`, "Slot tokens"), which record which slabs `init` has
    /// run on.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) mark_auth: Tracked<GhostSetAuth<int>>,
    /// Ghost (no bytes): the permissions to every slab's descriptor,
    /// made by `new` from the descriptor array's reservation
    /// ([`SlabTable::perms_ok`]).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) desc_perms: Tracked<Map<int, PointsTo<SlabDesc>>>,
    /// Ghost (no bytes): the invariant of every hand-out group's owner
    /// word ([`OwnerPred`]), which holds the word's permission, frozen
    /// (`new`).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) owner_invs: Tracked<&'static Map<int, OwnerInv>>,
    /// Ghost (no bytes): the watermark's invariant ([`WatermarkPred`]),
    /// which holds its permission: it is at most `nslabs`, and the owner
    /// words of the groups below it are stored.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) committed_inv: Tracked<WatermarkInv>,
}

/// A page group of a class table starts and ends on a page boundary: the
/// region's `base` and the group's bytes (`spp` slabs of `2^lg_slab`) are
/// multiples of the page `2^lg_page`. What a class purge's ranges need.
/// Opaque, so the type invariant carries it as one fact.
#[verifier::opaque]
pub(super) open spec fn groups_paged(base: usize, spp: u32, lg_slab: u8, lg_page: u32) -> bool {
    let page = (1usize << lg_page) as int;
    &&& page > 0
    &&& (base as int) % page == 0
    &&& (spp as int * (1usize << lg_slab) as int) % page == 0
}

/// A guard mask is `2^k - 1` for some `k` (all ones with guards off):
/// its set bits are the lowest ones.
pub open spec fn mask_shape(m: u32) -> bool {
    m & (m >> 1u32) == m >> 1u32
}

/// The marker of the owner store of slab `x` (`store_owner`): negative,
/// so apart from every bit's fresh and init markers (`bitmap.rs`). At the
/// location of a table's owner-word witness markers, slab `x`'s witness
/// marker.
pub open spec fn owner_mark(x: int) -> int {
    -1 - x
}

/// The owner-store markers of slabs `lo..hi`.
pub open spec fn owner_marks(lo: int, hi: int) -> Set<int> {
    Set::<int>::range(-hi, -lo)
}

/// Namespace of the watermark invariants ([`CommittedPred`],
/// [`WatermarkPred`]).
pub open spec fn committed_ns() -> int {
    0x6877_6d6b
}

/// A watermark copy's invariant (`heap::ClassLine`), keyed by the
/// atomic's identity (`vspec::atom::usize_id`), the table's slab count and
/// the location of a ghost variable whose half here is the copy's value
/// (the commit lock holds the other half, equal to `next_slab`:
/// [`TableInner::inv`]): the copy is at most the slab count. Its writers,
/// [`SlabTable::mirror_watermark`] and `take_group`, store `next_slab`
/// inside it and update the variable to it. The table's own watermark has
/// [`WatermarkPred`].
pub struct CommittedPred;

/// A watermark copy's permission and its half of the copy's ghost
/// variable ([`CommittedPred`]).
pub type MirrorVal = (UsizePerm, GhostVar<usize>);

impl InvariantPredicate<(int, usize, Loc), MirrorVal> for CommittedPred {
    open spec fn inv(k: (int, usize, Loc), v: MirrorVal) -> bool {
        &&& v.0.id() == k.0
        &&& v.0.value() <= k.1
        &&& v.1.id() == k.2
        &&& v.1@ == v.0.value()
    }
}

/// The invariant of one watermark copy (`heap::ClassLine`).
pub type CommittedInv = AtomicInvariant<(int, usize, Loc), MirrorVal, CommittedPred>;

/// A relaxed load of the watermark copy `a` (`heap::ClassLine`), through
/// its permission, inside its invariant `inv`: at most the slab count the
/// invariant names (its table's). The relaxed load is enough for the
/// reason [`SlabTable::desc_if_committed`] gives.
// `inv` is read only by the proof (ghost).
#[allow(unused_variables)]
#[inline(always)]
pub(crate) fn load_mirror(a: &AtomicUsize, inv: Tracked<&'static CommittedInv>) -> (r: usize)
    requires
        inv@.constant().0 == usize_id(a),
        inv@.namespace() == committed_ns(),
    ensures
        r <= inv@.constant().1,
    opens_invariants [committed_ns()]
{
    let tracked inv = inv.get();
    let r;
    vstd::open_atomic_invariant!(inv => v => {
        r = load_usize(a, Ordering::Relaxed, Tracked(&v.0));
    });
    r
}

/// A table's watermark invariant: [`CommittedPred`]'s, with the location
/// of the table's owner-word witness markers added to its key and a
/// persistent set of them, which has every slab's below the watermark, to
/// its value; its ghost variable's half here is the watermark's value
/// (the commit lock holds the other half, equal to `next_slab`:
/// [`TableInner::inv`]). `take_group`, its one writer, stores the end of
/// the group it hands out, after that group's owner word, and updates the
/// variable to it. A load returns a copy of the set
/// (`vspec::atom::load_usize_seen`).
pub struct WatermarkPred;

impl InvariantPredicate<(int, usize, Loc, Loc), SeenVal> for WatermarkPred {
    open spec fn inv(k: (int, usize, Loc, Loc), v: SeenVal) -> bool {
        &&& v.0.id() == k.0
        &&& v.0.value() <= k.1
        &&& v.1.id() == k.2
        &&& owner_marks(0, v.0.value() as int).subset_of(v.1@)
        &&& v.2.id() == k.3
        &&& v.2@ == v.0.value()
    }
}

/// The invariant of a table's watermark.
pub type WatermarkInv = AtomicInvariant<(int, usize, Loc, Loc), SeenVal, WatermarkPred>;

/// The pointer to the owner word at `addr`, of the provenance an address
/// cast picks up ([`perm_at`]): what [`SlabTable::owner_word`] returns for
/// the word of a hand-out group, the one its permission is to.
pub(super) open spec fn owner_ptr_at(addr: int) -> *mut AtomicU16 {
    ptr_mut_from_data::<AtomicU16>(PtrData { addr: addr as usize, provenance: cast_provenance(), metadata: () })
}

/// Namespace of the owner words' invariants ([`OwnerPred`]).
pub open spec fn owner_ns() -> int {
    0x6f77_6e72
}

/// What the invariant of one hand-out group's owner word is about: the
/// word's pointer, the class, the group's slabs `lo..hi` and the location
/// of the table's witness markers.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost struct OwnerKey {
    pub ptr: *mut AtomicU16,
    pub class: int,
    pub lo: int,
    pub hi: int,
    pub sloc: Loc,
}

/// What an owner word's invariant holds: the word's permission; the
/// witness markers of the group's slabs ([`owner_marks`] at the key's
/// location), exclusive while the word is not stored and persistent once
/// it is; and, once it is, the taker's record ([`Taker`]).
pub type OwnerVal = (PointsTo<AtomicU16>, GhostSubset<int>, GhostPersistentSubset<int>, Option<Taker>);

/// Who took a group, as its owner word's invariant keeps it: the
/// registry's persistent record of the taking state's arena
/// ([`Published`], which the taker holds: `take_group`'s `pubw`), and the
/// group's slabs in the taking state's owned-slab record, persistent.
pub type Taker = (&'static Published, GhostPersistentSubset<int>);

/// An owner word's invariant: the permission is to the key's word, and
/// either the exclusive markers hold the group's witness markers (the
/// word is not stored yet: `new` makes every group's so), or the
/// persistent ones do and the word holds the group's owner, which took
/// the group ([`owner_stored`]). `take_group`'s store (`store_owner`)
/// is the one write, inside it: it persists the markers as it stores,
/// keeps the persistent ones for the watermark's invariant
/// ([`WatermarkPred`]), and records the taker. A reader that holds a
/// persistent marker of a slab of the group rules the first case out (the
/// exclusive markers are disjoint from every persistent one), so it reads
/// the owner.
pub struct OwnerPred;

impl InvariantPredicate<OwnerKey, OwnerVal> for OwnerPred {
    open spec fn inv(k: OwnerKey, v: OwnerVal) -> bool {
        &&& v.0.ptr() == k.ptr
        &&& v.0.is_init()
        &&& v.1.id() == k.sloc
        &&& v.2.id() == k.sloc
        &&& (owner_marks(k.lo, k.hi).subset_of(v.1@) || owner_stored(k, u16_value(v.0.value()), v.2@, v.3))
    }
}

/// The owner word of the group of `k` holds `value`, the owner of each of
/// its slabs, `seen` has the group's witness markers, and the group was
/// taken by the published state of class `k.class` and arena `value`,
/// which has the group's slabs in its owned-slab record ([`taken_by`]).
pub open spec fn owner_stored(k: OwnerKey, value: u16, seen: Set<int>, taker: Option<Taker>) -> bool {
    &&& owner_marks(k.lo, k.hi).subset_of(seen)
    &&& forall|x: int| k.lo <= x < k.hi ==> #[trigger] slab_owner(k.class, x) == value
    &&& taker matches Some(t) && taken_by(k, value, t)
}

/// `t` is the record of a taker of the group of `k` for arena `value`:
/// the process registry's record of arena `value`, and the group's slabs
/// in its class `k.class` state's owned-slab record.
pub open spec fn taken_by(k: OwnerKey, value: u16, t: Taker) -> bool {
    &&& t.0.id() == process_pub_loc()
    &&& t.0.key() == value as int
    &&& t.1.id() == t.0.value()[k.class]
    &&& forall|x: int| k.lo <= x < k.hi ==> #[trigger] t.1@.contains(x)
}

/// The invariant of one owner word.
pub type OwnerInv = AtomicInvariant<OwnerKey, OwnerVal, OwnerPred>;

/// The arena that owns slab `slab` of class `c`'s region: the index
/// [`SlabTable::take_group`] stores in the owner word of the slab's
/// hand-out group as it hands the group out, under the commit lock and
/// before the watermark passes the group, and fixed for the life of the
/// process from then on. Uninterpreted: an owner word's invariant
/// ([`OwnerPred`]) names it once the word is stored; the take_group proof
/// gives the one store (`store_owner`) its contract, which uses up the
/// group's owner-store markers ([`owner_marks`], minted once per slab by
/// the class's table at [`mark_loc`]). Keyed by the class alone, which
/// rests on there being one table per class per process (the premise
/// [`axiom_slot_loc`] names; tests that build several tables, or corrupt
/// an owner word, are outside it).
pub uninterp spec fn slab_owner(c: int, slab: int) -> u16;

/// The arena that owns the slab of class `c`'s region holding address
/// `p` ([`slab_owner`] of `Regions::slab_index`): for a slot, the arena
/// it was claimed from and the only one it is released into.
pub open spec fn slot_owner(c: int, p: usize) -> u16 {
    slab_owner(c, process_regions().slab_index(c, p))
}

/// The authority of class `c`'s markers: the one its table made
/// ([`axiom_slot_loc`]). Every slab's owner-store marker is minted once,
/// by that table's `new`, so a store that uses it up can run at most once
/// per slab.
pub uninterp spec fn mark_loc(c: int) -> Loc;

/// The authority of class `c`'s slot tokens (`bitmap.rs`, "Slot tokens"):
/// the one its table made ([`axiom_slot_loc`]).
pub uninterp spec fn slot_loc(c: int) -> Loc;

/// `held` is the token of slot `slot` of slab `slab` of class `c`, as a
/// live slot's token holds it.
pub open spec fn holds_slot(held: Option<GhostSingleton<int>>, c: int, slab: int, slot: int) -> bool {
    &&& held matches Some(t)
    &&& t.id() == slot_loc(c)
    &&& t@ == process_regions().slot_at(c, slab, slot)
}

/// `p` is an initialised permission to the `T` at `addr`, through a
/// pointer of the provenance an address cast picks up
/// ([`addr_to_ptr`](crate::vspec::addr::addr_to_ptr)).
pub open spec fn perm_at<T>(p: PointsTo<T>, addr: int) -> bool {
    &&& p.ptr() == ptr_mut_from_data::<T>(PtrData { addr: addr as usize, provenance: cast_provenance(), metadata: () })
    &&& p.is_init()
}

/// `m` holds the permission of each of the `n` words of a class bitmap
/// at `base`: initialised, at its address, to the word that address
/// names ([`word_addr`]). Opaque (`SlabTable::bits_unchecked` reads it),
/// so that the type invariant's check in `SlabTable::new` does not
/// instantiate it.
#[verifier::opaque]
spec fn words_perms_ok(m: Map<int, PointsTo<AtomicU64>>, base: int, n: int) -> bool {
    forall|j: int| #![trigger m.contains_key(j)] #![trigger m[j]] 0 <= j < n
        ==> m.contains_key(j) && perm_at(m[j], base + 8 * j) && word_addr(&m[j].value()) == base + 8 * j
}

// The sizes [`axiom_zero_desc`], [`axiom_zero_owner`] and
// [`axiom_fresh_words`] make a value of (Verus checks them against the
// compiler's).
global layout SlabDesc is size == 16, align == 16;
global layout AtomicU16 is size == 2, align == 2;
global layout AtomicU64 is size == 8, align == 8;

/// The descriptor of `slab` in `t`'s descriptor array: the one its
/// permission ([`SlabTable::perms_ok`]) is to. Names a descriptor so
/// that a contract can say which slab a `&SlabDesc` belongs to; nothing
/// about its contents.
pub closed spec fn desc_at(t: SlabTable, slab: usize) -> SlabDesc {
    t.desc_perms@[slab as int].value()
}

/// Owner-store markers are not bit markers.
proof fn lemma_owner_marks_apart(lo: int, hi: int, a: int, b: int, m: int)
    requires
        0 <= lo,
        0 <= a,
        owner_marks(lo, hi).contains(m),
    ensures
        !init_marks(a, b).contains(m),
        !fresh_marks(a, b).contains(m),
{
    broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
}

/// `p` holds the fresh markers of `slab`'s bits in `t`'s bitmap: `init`
/// has run on the slab (`AtomicBitmap::inited_by`).
pub open spec fn slab_marked(t: SlabTable, slab: int, p: Set<int>) -> bool {
    fresh_marks(slab * t.spec_slots().sb, slab * t.spec_slots().sb + t.spec_slots().sb).subset_of(p)
}

/// More markers keep a slab marked.
pub proof fn lemma_slab_marked_mono(t: SlabTable, slab: int, a: Set<int>, b: Set<int>)
    requires
        slab_marked(t, slab, a),
        a.subset_of(b),
    ensures
        slab_marked(t, slab, b),
{
    let f = fresh_marks(slab * t.spec_slots().sb, slab * t.spec_slots().sb + t.spec_slots().sb);
    assert forall|y: int| #[trigger] f.contains(y) implies b.contains(y) by {
        assert(a.contains(y));
    }
}

/// `p` holds the init markers of guard slab `slab`'s bits in `t`'s
/// bitmap: `take_group` took them out of the commit lock's value
/// ([`TableInner::inv`]) when it handed the slab's group out, as it hands
/// every other slab's to `init` (which never runs on a guard slab).
pub open spec fn guard_marked(t: SlabTable, slab: int, p: Set<int>) -> bool {
    init_marks(slab * t.spec_slots().sb, slab * t.spec_slots().sb + t.spec_slots().sb).subset_of(p)
}

/// More markers keep a guard slab marked.
pub proof fn lemma_guard_marked_mono(t: SlabTable, slab: int, a: Set<int>, b: Set<int>)
    requires
        guard_marked(t, slab, a),
        a.subset_of(b),
    ensures
        guard_marked(t, slab, b),
{
    let f = init_marks(slab * t.spec_slots().sb, slab * t.spec_slots().sb + t.spec_slots().sb);
    assert forall|y: int| #[trigger] f.contains(y) implies b.contains(y) by {
        assert(a.contains(y));
    }
}

/// The view `SlabTable::bits` gives of a slab of a class with `slab_bits`
/// bits per slab: whole words, or one run of bits inside a word.
pub open spec fn bits_shape<'a>(bm: AtomicBitmap<'a>, slab_bits: usize) -> bool {
    &&& bm.spec_nbits() == slab_bits
    &&& bm.spec_words() == words_of(slab_bits)
    &&& bm.slab_mask() == slab_mask_of(slab_bits)
    &&& slab_bits >= 64 ==> slab_bits % 64 == 0
}

/// The ghost side of the view `SlabTable::bits` gives of `slab`: slot 0 is
/// class bitmap bit `slab * slab_bits`, and the view uses the table's slot
/// tokens and word invariants.
pub open spec fn bits_ghost<'a>(bm: AtomicBitmap<'a>, t: SlabTable, slab: usize) -> bool {
    &&& bm.spec_first() == slab * t.spec_slots().sb
    &&& bm.spec_slots() == t.spec_slots()
    &&& bm.spec_invs() == t.spec_word_invs()
}

/// Bitmap words of a slab of `slab_bits` bits.
pub open spec fn words_of(slab_bits: usize) -> nat {
    if slab_bits >= 64 { (slab_bits / 64) as nat } else { 1 }
}

/// The bits of a word a slab of `slab_bits` bits owns, in slab coordinates.
pub open spec fn slab_mask_of(slab_bits: usize) -> u64 {
    if slab_bits >= 64 { !0u64 } else { sub(1u64 << (slab_bits as u64), 1u64) }
}

/// A set bit `b` of a mask inside word `word` of a slab's view is a bit
/// index below `slab_bits`.
pub proof fn lemma_bit_in_slab(x: u64, mask: u64, b: u64, slab_bits: usize, word: usize)
    requires
        subset(x, mask),
        subset(mask, slab_mask_of(slab_bits)),
        b < 64,
        has_bit(x, b),
        word < words_of(slab_bits),
        slab_bits >= 64 ==> slab_bits % 64 == 0,
    ensures
        word * 64 + b < slab_bits,
{
    if slab_bits >= 64 {
        assert(word * 64 + 64 <= slab_bits) by (nonlinear_arith)
            requires word < slab_bits / 64, slab_bits % 64 == 0;
    } else {
        let sb = slab_bits as u64;
        assert(b < sb) by (bit_vector)
            requires x & !mask == 0u64, mask & !sub(1u64 << sb, 1u64) == 0u64, (x >> b) & 1u64 == 1u64, b < 64, sb < 64;
    }
}


/// The hand-out group's shift is below 32 (`group`, `1 << lg_group`, is
/// a `u32`) and the region has at most `2^26` slabs.
pub(super) proof fn lemma_lg_group(t: SlabTable)
    requires
        t.inv(),
    ensures
        t.lg_group < 32,
        t.spec_nslabs() <= 0x400_0000,
        t.group as usize == 1usize << t.lg_group,
{
    broadcast use lemma_spec_nslabs;
    let (gr, lg) = (t.group, t.lg_group);
    assert(gr as usize == 1usize << lg && lg < 64 ==> lg < 32) by (bit_vector);
}

/// A region of `n <= m >> lg` slabs of at least 4 KiB has at most
/// `usize::MAX >> 12` of them.
proof fn lemma_nslabs_bound(n: usize, m: usize, lg: usize)
    requires
        n <= m >> lg,
        12 <= lg <= 22,
    ensures
        n <= usize::MAX >> 12,
{
    assert(n <= usize::MAX >> 12) by (bit_vector)
        requires n <= m >> lg, 12 <= lg <= 22;
}

} // verus!

/// Padding that keeps the stride of `[SlabTable; NCLASSES]` at 384 bytes,
/// three lines, off a multiple of 512 (see
/// [`ClassState`](super::ClassState)), whatever the commit lock's state
/// takes of the second and third.
const TABLE_TAIL: usize = 2 * PAD_ALIGN - core::mem::size_of::<Padded<Lock<TableInner>>>();

const _: () = assert!(core::mem::offset_of!(SlabTable, inner) == PAD_ALIGN);
const _: () = assert!(core::mem::size_of::<SlabTable>() == 3 * PAD_ALIGN);
const _: () = assert!(core::mem::offset_of!(SlabTable, lg_group) < 32);
const _: () = assert!(core::mem::size_of::<SlabTable>().is_multiple_of(PAD_ALIGN));
const _: () = assert!(!core::mem::size_of::<SlabTable>().is_multiple_of(512));

// SAFETY: `Lazy` holds an address into the metadata region that is only
// ever used under the lock.
unsafe impl Send for TableInner {}

verus! {

/// Bytes a run holds at most ([`SlabTable::commit_run`]): the default
/// guard interval of 64 KiB slabs, so a run is as long with guards on or
/// off, and a class of large slabs commits one at a time.
pub(super) const RUN_BYTES: usize = 512 << 10;

} // verus!

// In the three wrappers below, `addr..addr + len` is inside a class
// region this allocator reserved (the callers pass slab or page-group
// ranges below the region's end, which `commit` requires), so changing
// its protection or installing a guard affects only pages nothing else
// in the process maps.
verus! {

/// `[addr, addr + len)` lies in class `c`'s region of the process's
/// regions: from its base to the end of its last slab.
pub open spec fn commit_inside(c: int, addr: int, len: int) -> bool {
    let r = process_regions();
    &&& r.class_base[c] <= addr
    &&& addr + len <= r.slab_span(c, r.spec_nslabs(c)).0
}

/// A commit of `[addr, addr + len)` in class `c`'s region spares the guard
/// slabs of `c` ([`guard_slab`](crate::layout::guard_slab), the slabs
/// `heap::theorem_guard_pages` keeps out of every block): the range
/// shares no byte with a guard slab, unless the slab starts in the
/// range's first page and an `MADV_GUARD_INSTALL` of that page succeeded
/// (`sys::madvise_ok`). That marker is the slab's guard
/// (`SlabTable::install_guard`), which the commit's `mprotect` leaves in
/// place; the rest of such a slab is committed with the range.
pub open spec fn commit_spares_guards(c: int, addr: int, len: int) -> bool {
    let r = process_regions();
    let page = 1usize << r.lg_page;
    forall|g: int|
        0 <= g < r.spec_nslabs(c) && #[trigger] crate::layout::guard_slab(c, g) ==> {
            ||| crate::layout::disjoint(addr, len, r.slab_span(c, g).0, r.slab_span(c, g).1)
            ||| (sys::madvise_ok(addr as usize, page, MADV_GUARD_INSTALL) && addr <= r.slab_span(c, g).0 < addr + page)
        }
}

/// Make `[addr, addr + len)` of class `c`'s region read-write. Every
/// caller proves the range inside the region and, unless `chunk`, apart
/// from the guards (`commit_spares_guards`). `chunk` marks every commit
/// of a class whose `thp` mode is `Mode::Chunks` (the PMD chunk and its
/// plain fallback in `SlabTable::commit_page_group`): that mode places no
/// guards and commits its guard slabs read-write
/// ([`SlabTable::commit_slab`], design 17). It is ghost, set from the mode
/// the caller read; no specification ties it to the class's mode.
fn commit(addr: usize, len: usize, Ghost(c): Ghost<int>, Ghost(chunk): Ghost<bool>) -> (r: Result<(), Errno>)
    requires
        crate::sys::may_syscall(),
        commit_inside(c, addr as int, len as int),
        !chunk ==> commit_spares_guards(c, addr as int, len as int),
    ensures
        commit_inside(c, addr as int, len as int),
        !chunk ==> commit_spares_guards(c, addr as int, len as int),
{
    // SAFETY: a range of a reserved class region (above).
    unsafe { sys::mprotect(addr, len, PROT_READ | PROT_WRITE) }
}

/// Commits keep the class guard slabs inaccessible: every call of
/// [`commit`] outside a `Mode::Chunks` class (`chunk` false) makes
/// read-write only bytes of class `c`'s region, and none of any guard
/// slab of `c`, except a guard slab starting in the range's first page
/// for which an `MADV_GUARD_INSTALL` of that page succeeded
/// (`sys::madvise_ok`; in the code, the call just before on the same
/// path), a marker the `mprotect` keeps. `commit` ensures what its
/// precondition asks of each caller ([`SlabTable::commit_run`],
/// [`SlabTable::commit_slab`] through `commit_page_group`), so Verus
/// rejects any caller that has not proved it. The class region is apart
/// from every other class region and the huge region
/// (`Regions::separated`), so such a commit touches no other class's
/// region (nor its guard slabs), the huge region (nor any huge run's guard
/// page) or the metadata region.
///
/// Trusted: `mprotect` keeps a guard marker (`MADV_GUARD_INSTALL`), and
/// nothing removes a class guard's marker. Not claimed: commits of a
/// `Mode::Chunks` class, which places no guards and commits its guard
/// slabs read-write; that a class's `thp` mode is fixed before its first
/// commit (`thp::class_mode`), so such a class never placed a guard, is
/// read from the code.
proof fn theorem_slab_commit_avoids_guards(addr: usize, len: usize, c: int, chunk: bool, r: Result<(), Errno>, g: int)
    requires
        crate::heap::regions_ok(),
        0 <= c < NCLASSES,
        call_ensures(commit, (addr, len, Ghost(c), Ghost(chunk)), r),
        !chunk,
        0 <= g < process_regions().spec_nslabs(c),
        crate::layout::guard_slab(c, g),
    ensures
        process_regions().class_base[c] <= addr,
        addr + len <= process_regions().slab_span(c, process_regions().spec_nslabs(c)).0,
        ({
            let (r, page) = (process_regions(), 1usize << process_regions().lg_page);
            ||| crate::layout::disjoint(addr as int, len as int, r.slab_span(c, g).0, r.slab_span(c, g).1)
            ||| (sys::madvise_ok(addr, page, MADV_GUARD_INSTALL) && addr <= r.slab_span(c, g).0 < addr + page)
        }),
        ({
            let (r, region) = (process_regions(), (1usize << process_regions().lg_region) as int);
            &&& forall|c2: int| 0 <= c2 < NCLASSES && c2 != c ==> crate::layout::disjoint(addr as int, len as int, #[trigger] r.class_base[c2] as int, region)
            &&& crate::layout::disjoint(addr as int, len as int, r.huge_base as int, r.huge_len as int)
            &&& crate::layout::disjoint(addr as int, len as int, r.meta_base as int, r.meta_len as int)
        }),
{
    let r = process_regions();
    let (stride, region) = ((1usize << r.lg_stride) as int, (1usize << r.lg_region) as int);
    lemma_slabs_fit_region(r, c);
    assert(r.superblock + c * stride <= r.class_base[c] && r.class_base[c] + region <= r.superblock + (c + 1) * stride);
    assert forall|c2: int| 0 <= c2 < NCLASSES && c2 != c implies crate::layout::disjoint(addr as int, len as int, #[trigger] r.class_base[c2] as int, region) by {
        assert(r.superblock + c2 * stride <= r.class_base[c2] && r.class_base[c2] + region <= r.superblock + (c2 + 1) * stride);
        if c2 < c {
            vstd::arithmetic::mul::lemma_mul_inequality(c2 + 1, c, stride);
        } else {
            vstd::arithmetic::mul::lemma_mul_inequality(c + 1, c2, stride);
        }
    }
    vstd::arithmetic::mul::lemma_mul_inequality(c + 1, NCLASSES as int, stride);
}

/// Class `c`'s slabs end inside its region: `spec_nslabs` whole slabs fit
/// in `2^lg_region` bytes.
proof fn lemma_slabs_fit_region(r: Regions, c: int)
    requires
        r.wf(),
        0 <= c < NCLASSES,
    ensures
        r.slab_span(c, r.spec_nslabs(c)).0 <= r.class_base[c] + (1usize << r.lg_region),
{
    crate::vspec::class::axiom_class_table_wf(c);
    let (lg, region) = (class_table()[c].lg_slab as usize, 1usize << r.lg_region);
    vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
    vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(region >> lg, lg, region);
    vstd::bits::lemma_usize_shl_is_mul(1, lg);
    assert(region >> class_table()[c].lg_slab == region >> lg);
}

fn decommit(addr: usize, len: usize) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: a range of a reserved class region (above).
    unsafe { sys::mprotect(addr, len, PROT_NONE) }
}

fn guard(addr: usize, len: usize) -> (r: Result<(), Errno>)
    requires
        crate::sys::may_syscall(),
    ensures
        r is Ok ==> sys::madvise_ok(addr, len, MADV_GUARD_INSTALL),
{
    // SAFETY: a range of a reserved class region (above).
    unsafe { sys::madvise(addr, len, MADV_GUARD_INSTALL) }
}

/// `seen` is a witness that slab `slab` of `t` was handed out: its
/// persistent witness marker ([`owner_mark`]), which a load of the
/// watermark past `slab` returns ([`SlabTable::committed_seen`]: the
/// watermark's invariant holds the markers of every slab below it,
/// [`WatermarkPred`]).
pub open spec fn seen_owner(t: SlabTable, slab: usize, seen: Option<&'static GhostPersistentSubset<int>>) -> bool {
    &&& seen matches Some(w)
    &&& w.id() == t.spec_sloc()
    &&& w@.contains(owner_mark(slab as int))
}

/// `(*w).load(Relaxed)` of the owner word of handed-out slab `slab`'s
/// group in `t`: the arena that owns the slab, when the caller has a
/// witness that the group was handed out (`seen`, [`seen_owner`]). The
/// load is through the word's permission, inside its invariant
/// ([`OwnerPred`]), which the witness shows to be past the store
/// ([`store_owner`]): there the word holds the group's owner. Without one
/// the value is any the word held. Sequential consistency, as for every
/// atomic here (`vspec::atom`).
///
/// # Safety
/// `w` is the owner word of `slab`'s group in `t`, committed and
/// written before the watermark moved past the group.
// `t`, `slab` and `seen` are read only by the proof (ghost).
#[allow(unused_variables)]
#[inline(always)]
unsafe fn load_owner(t: &SlabTable, w: *const AtomicU16, Ghost(slab): Ghost<usize>, seen: Tracked<Option<&'static GhostPersistentSubset<int>>>) -> (r: u16)
    requires
        slab < t.spec_nslabs(),
        w == owner_ptr_at(t.owner_addr((slab >> t.lg_group) as int)),
    ensures
        seen_owner(*t, slab, seen@) ==> r == slab_owner(t.spec_class() as int, slab as int),
    opens_invariants [owner_ns()]
{
    proof {
        use_type_invariant(t);
        t.lemma_group_in_range(slab);
        t.lemma_slab_in_group(slab);
    }
    let ghost g = (slab >> t.lg_group) as int;
    let ghost (lo, hi) = (t.spec_group_range(g as usize).0 as int, t.spec_group_range(g as usize).1 as int);
    proof {
        assert(t.owner_invs@.contains_key(g));
        broadcast use vstd::set_lib::range_set_properties;
        assert(owner_marks(lo, hi).contains(owner_mark(slab as int)));
    }
    let r;
    vstd::open_atomic_invariant!(t.owner_invs.borrow().tracked_borrow(g) => v => {
        // SAFETY: the invariant's permission is to the word at `w`.
        r = unsafe { load_u16_at(w, Ordering::Relaxed, Tracked(&v.0)) };
        proof {
            let k = t.owner_invs@[g].constant();
            assert(k.lo == lo && k.hi == hi && k.class == t.spec_class() as int);
            if seen_owner(*t, slab, seen@) {
                // Not the unstored case: its markers are disjoint from the
                // persistent `seen`, which has slab `slab`'s.
                let tracked sw = seen.get().tracked_unwrap();
                if owner_marks(lo, hi).subset_of(v.1@) {
                    v.1.disjoint_persistent(sw);
                    assert(v.1@.contains(owner_mark(slab as int)));
                }
                assert(owner_stored(k, r, v.2@, v.3));
                assert(slab_owner(k.class, slab as int) == r);
            }
        }
    });
    r
}

/// `(*w).store(arena, Relaxed)` of the owner word of hand-out group
/// `group` of `t`, through its permission `perm`: the permission holds
/// `arena` ([`u16_value`], as `vspec::atom::load_u16_at` reads it back),
/// and from here on [`slab_owner`] of every slab of the group is `arena`.
/// Trusted, the write half of the [`slab_owner`] premise: called only by
/// [`SlabTable::take_group`], under the commit lock, for the group at the
/// watermark, past every fallible step, after which it moves the
/// watermark past the group; so each group's word is stored once, before
/// any slab of it is handed out. A second store of a slab's owner with
/// another arena would contradict the first, so the store uses up
/// `marks`, the owner-store markers of the group's slabs ([`owner_mark`]),
/// which the class's table minted once each ([`mark_loc`]) and its commit
/// lock holds for the slabs from the watermark on ([`TableInner::inv`]).
/// That last holds because every holder outside the tests ends with
/// `LockGuard::unlock`, which requires the invariant
/// (`TableInner`'s `LockSpec` impl). It runs inside the word's invariant
/// ([`OwnerPred`]), which holds `perm`; every store to an owner word is
/// this one (the test helper `corrupt_owner_word` aside), so the value
/// `perm` holds is the word's.
///
/// # Safety
/// `w` is the owner word of the group at the watermark, committed.
// `t`, `group`, `perm` and `marks` are read only by the specification
// (ghost).
#[allow(unused_variables)]
#[verifier::external_body]
#[verifier::atomic]
#[inline(always)]
unsafe fn store_owner(
    w: *const AtomicU16,
    arena: u16,
    Ghost(t): Ghost<SlabTable>,
    Ghost(group): Ghost<usize>,
    Tracked(perm): Tracked<&mut PointsTo<AtomicU16>>,
    Tracked(marks): Tracked<GhostSubset<int>>,
)
    requires
        group < t.spec_ngroups(),
        w == owner_ptr_at(t.owner_addr(group as int)),
        old(perm).ptr() == w,
        old(perm).is_init(),
        marks.id() == mark_loc(t.spec_class() as int),
        owner_marks(t.spec_group_range(group).0 as int, t.spec_group_range(group).1 as int).subset_of(marks@),
    ensures
        final(perm).ptr() == old(perm).ptr(),
        final(perm).is_init(),
        u16_value(final(perm).value()) == arena,
        forall|x: int| t.spec_group_range(group).0 <= x < t.spec_group_range(group).1
            ==> #[trigger] slab_owner(t.spec_class() as int, x) == arena,
    opens_invariants none
    no_unwind
{
    // SAFETY: `perm` is to the owner word at `w`, committed.
    unsafe { (*w).store(arena, Ordering::Relaxed) }
}

/// `index` itself, for the bitmap word index of [`SlabTable::is_set_unchecked`]
/// and `heap::ClassLine::set_slab`. On
/// aarch64 it goes through an empty `asm!` (the next item): left alone,
/// LLVM rewrites `(bit >> 6) << 3` as `(bit >> 3) & !7`, a shift and a
/// mask ahead of the load on `free`'s dependent chain, where `bit >> 6`
/// and a scaled `ldr` is one instruction. x86_64 already scales the index
/// in the load, and this is the identity.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub(crate) fn word_index(index: usize) -> (r: usize)
    ensures
        r == index,
    opens_invariants none
{
    index
}

/// [`word_index`] on aarch64: `index` through the empty `asm!`.
///
/// Trusted: the empty template leaves the register `index` is in, for
/// input and output, unchanged.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[verifier::external_body]
pub(crate) fn word_index(index: usize) -> (r: usize)
    ensures
        r == index,
    opens_invariants none
{
    let mut index = index;
    // SAFETY: an empty template; `index` comes out unchanged.
    unsafe {
        core::arch::asm!("/* {0} */", inout(reg) index, options(pure, nomem, nostack, preserves_flags));
    }
    index
}

/// [`SlabTable::take_group`]'s check that the watermark is a group
/// boundary, which the precondition (proved from [`TableInner::inv`])
/// says it is. Inlined and `track_caller`, as [`group_geometry`]. A
/// `debug_assert!` of the equality: Verus does not support
/// `debug_assert_eq!` (its failure path is `core::panicking::assert_failed`).
#[inline(always)]
#[track_caller]
fn watermark_aligned(s: usize, lg_group: u8)
    requires
        lg_group < 64,
        s & sub(1usize << lg_group, 1) == 0,
{
    proof {
        assert(1usize << lg_group >= 1) by (bit_vector)
            requires lg_group < 64;
    }
    debug_assert!(
        s & ((1 << lg_group) - 1) == 0,
        "take_group: the watermark is group-aligned"
    );
}

} // verus!

verus! {

/// The process's regions are well formed and separated
/// ([`regions_ok`](crate::heap::regions_ok)), as the `regions`
/// preconditions of [`SlabTable::new`] give. Opaque
/// ([`SlabTable::lemma_regions_ok`] reads it), so the invariant's users do
/// not pay for it.
#[verifier::opaque]
pub(super) open spec fn regions_held() -> bool {
    crate::heap::regions_ok()
}

impl SlabTable {
    /// The geometry `new` sets up and never changes: the class, its slab
    /// bits, the power-of-two page and hand-out groups (a page group no
    /// larger than a hand-out group, both tiling the region, page groups
    /// on page boundaries), a region of `nslabs` slabs from `base` that
    /// ends inside the address space and is class `class`'s region of the
    /// process's regions ([`process_regions`]), a guard mask of the shape
    /// `2^k - 1` ([`mask_shape`]), the three arrays inside the address
    /// space, the commit lock's rank and fixed ghost geometry, and the
    /// process's regions well formed and separated ([`regions_held`]).
    /// `new` proves it from its preconditions, and nothing writes these
    /// fields afterwards (`committed_slabs` and `inner`'s current value are
    /// not read here).
    #[verifier::type_invariant]
    pub(super) open spec fn inv(self) -> bool {
        &&& self.class < NCLASSES
        &&& class_table()[self.class as int].wf()
        &&& self.slab_bits == class_table()[self.class as int].slab_bits
        &&& self.slab_bits >= 64 ==> self.slab_bits % 64 == 0
        &&& self.lg_slabs_per_page <= 26
        &&& self.lg_group < 64
        &&& self.lg_page < 64
        &&& self.slabs_per_page >= 1
        &&& self.page_groups_ok()
        &&& self.nslabs <= (usize::MAX - self.base) as usize >> class_table()[self.class as int].lg_slab
        &&& self.base < 1usize << 57
        &&& self.nslabs <= ((1usize << 57) - self.base) as usize >> class_table()[self.class as int].lg_slab
        &&& self.nslabs <= 0x400_0000
        &&& self.group as usize == 1usize << self.lg_group
        &&& self.slabs_per_page <= self.group
        &&& self.base == process_regions().class_base[self.class as int]
        &&& crate::layout::base_aligned(self.base as int, self.class as int, process_regions().lg_page)
        &&& self.nslabs as int == process_regions().spec_nslabs(self.class as int)
        &&& self.lg_page == process_regions().lg_page
        &&& groups_paged(self.base, self.slabs_per_page, class_table()[self.class as int].lg_slab, self.lg_page)
        &&& mask_shape(self.guard_mask)
        &&& self.desc_stride as usize == stride_of(self.class as int)
        &&& self.desc_base % DESC_ALIGN == 0
        &&& self.desc_base + self.nslabs * (self.desc_stride as int) <= usize::MAX
        &&& self.bits_base + (self.nslabs * (self.slab_bits as int) + 7) / 8 <= usize::MAX
        &&& self.owners_base + (self.nslabs + self.group - 1) / (self.group as int) * 2 <= usize::MAX
        &&& self.inner.0.rank() == rank_table(self.class as int)
        &&& lock_init(self.inner.0).page@ == 1usize << self.lg_page
        &&& lock_init(self.inner.0).group@ == self.group as usize
        &&& lock_init(self.inner.0).nslabs@ == self.nslabs
        &&& lock_init(self.inner.0).sb@ == self.slab_bits as usize
        &&& lock_init(self.inner.0).mloc@ == self.mark_auth@.id()
        &&& lock_init(self.inner.0).dbase@ == self.desc_base as int
        &&& lock_init(self.inner.0).dstride@ == self.desc_stride as int
        &&& words_wf(*self.word_invs@, self.spec_slots(), self.bits_base, self.spec_nwords())
        &&& self.bits_base + 8 * self.spec_nwords() <= usize::MAX
        &&& self.spec_slots().loc == slot_loc(self.class as int)
        &&& self.mark_auth@.id() == mark_loc(self.class as int)
        &&& crate::layout::guard_geom(self.class as int) == (self.lg_slabs_per_page, self.guard_mask)
        &&& self.perms_ok()
        &&& self.committed_inv@.constant() == (usize_id(&self.committed_slabs), self.nslabs, self.spec_sloc(), lock_init(self.inner.0).wloc@)
        &&& self.committed_inv@.namespace() == committed_ns()
        &&& regions_held()
    }

    /// The location of the owner words' witness markers ([`OwnerPred`]):
    /// the commit lock's, fixed at `new`.
    pub closed spec fn spec_sloc(&self) -> Loc {
        lock_init(self.inner.0).sloc@
    }

    /// Hand-out groups of the region (owner words).
    pub(super) closed spec fn spec_ngroups(&self) -> int {
        (self.nslabs + self.group - 1) / (self.group as int)
    }

    /// The permissions `new` made from the descriptor and owner
    /// reservations: to descriptor `s` at `desc_base + s * desc_stride`
    /// (the descriptor [`desc_id`] names there), and to owner word `g` at
    /// `owners_base + 2 * g` (held by the word's invariant,
    /// [`owner_inv_at`]), for every slab and hand-out group of the region;
    /// and, made from the bitmap's reservation, to bitmap word `j` at
    /// `bits_base + 8 * j`, the word whose identity ([`word_addr`]) is that
    /// address, for every word of the bitmap.
    pub(super) closed spec fn perms_ok(&self) -> bool {
        &&& forall|s: int| 0 <= s < self.nslabs
            ==> desc_id(&(#[trigger] self.desc_perms@[s]).value()) == self.desc_base + s * (self.desc_stride as int)
        &&& forall|s: int| #![trigger self.desc_perms@.contains_key(s)] #![trigger self.desc_perms@[s]] 0 <= s < self.nslabs ==> self.desc_perms@.contains_key(s)
            && perm_at(self.desc_perms@[s], self.desc_base + s * (self.desc_stride as int))
        &&& forall|g: int| #![trigger self.owner_invs@.contains_key(g)] #![trigger self.owner_invs@[g]] 0 <= g < self.spec_ngroups() ==> self.owner_invs@.contains_key(g)
            && owner_inv_at(self.owner_invs@[g], self.owners_base as int, self.class as int, self.lg_group, self.group, self.nslabs, self.spec_sloc(), g)
        &&& words_perms_ok(*self.word_perms@, self.bits_base as int, self.spec_nwords())
    }

    /// The class bitmap's slot tokens (`bitmap.rs`, "Slot tokens"): bits of
    /// `slab_bits` per slab, `nslots` of them slots, `nslabs` slabs.
    pub closed spec fn spec_slots(&self) -> Slots {
        Slots {
            loc: self.slot_auth@.id(),
            mloc: self.mark_auth@.id(),
            sloc: self.spec_sloc(),
            sb: self.slab_bits as int,
            nslots: class_table()[self.class as int].nslots as int,
            nbits: self.nslabs * self.slab_bits,
            base: self.base as int,
            slab_size: (1usize << class_table()[self.class as int].lg_slab) as int,
            size: class_table()[self.class as int].size as int,
            glg: self.lg_slabs_per_page,
            gmask: self.guard_mask,
        }
    }

    /// Words of the class bitmap.
    pub closed spec fn spec_nwords(&self) -> int {
        (self.nslabs * self.slab_bits + 63) / 64
    }

    /// The invariants of the class bitmap's words.
    pub closed spec fn spec_word_invs(&self) -> Map<int, WordInv> {
        *self.word_invs@
    }

    /// The descriptor array, the class bitmap and the owner array lie in
    /// the metadata region: [`new`] reserves each from `meta`
    /// ([`Lazy::reserve`]) with room for every slab's descriptor, every
    /// slab's `slab_bits` bits and every hand-out group's owner word.
    ///
    /// [`new`]: SlabTable::new
    pub closed spec fn meta_ok(&self) -> bool {
        let r = process_regions();
        &&& self.group >= 1
        &&& r.in_meta(self.desc_base as int, self.nslabs * (self.desc_stride as int))
        &&& r.in_meta(self.bits_base as int, (self.nslabs * (self.slab_bits as int) + 7) / 8)
        &&& r.in_meta(self.owners_base as int, ((self.nslabs + self.group - 1) / (self.group as int)) * 2)
    }

    /// Where slab `slab`'s descriptor starts.
    pub closed spec fn desc_addr(&self, slab: int) -> int {
        self.desc_base + slab * (self.desc_stride as int)
    }

    /// A descriptor's bytes (`desc_stride(class)`).
    pub closed spec fn spec_desc_stride(&self) -> int {
        self.desc_stride as int
    }

    /// Where hand-out group `group`'s owner word starts.
    pub closed spec fn owner_addr(&self, group: int) -> int {
        self.owners_base + 2 * group
    }

    /// Slabs per hand-out group.
    pub closed spec fn spec_group(&self) -> int {
        self.group as int
    }

    /// Where the bitmap word holding bit `bit` of the class bitmap starts.
    pub closed spec fn bit_word_addr(&self, bit: int) -> int {
        self.bits_base + 8 * (bit / 64)
    }

    /// Bits in the class bitmap: `slab_bits` per slab.
    pub closed spec fn spec_nbits(&self) -> int {
        self.nslabs * (self.slab_bits as int)
    }

    /// The slab range of hand-out group `group` ([`group_range`]): `group`
    /// slabs from `group << lg_group`, clipped to the region.
    ///
    /// [`group_range`]: SlabTable::group_range
    pub(super) open spec fn spec_group_range(self, group: usize) -> (usize, usize) {
        group_range_at(self.lg_group, self.group, self.nslabs, group)
    }

    /// What [`SlabDesc::at`] needs of slab `slab`'s descriptor and its
    /// permission, which is to [`desc_at`], the descriptor [`desc_id`]
    /// names at its address.
    pub(super) proof fn lemma_desc_at(&self, slab: usize)
        requires
            self.inv(),
            slab < self.spec_nslabs(),
        ensures
            (self.class as usize) < NCLASSES,
            self.desc_base % DESC_ALIGN == 0,
            self.desc_base + slab * stride_of(self.class as int) <= usize::MAX,
            self.desc_perms@.contains_key(slab as int),
            self.desc_perms@[slab as int].ptr() == ptr_mut_from_data::<SlabDesc>(PtrData {
                addr: (self.desc_base + slab * stride_of(self.class as int)) as usize,
                provenance: cast_provenance(),
                metadata: (),
            }),
            self.desc_perms@[slab as int].is_init(),
            self.desc_perms@[slab as int].value() == desc_at(*self, slab),
            desc_id(&desc_at(*self, slab)) == self.desc_base + slab * stride_of(self.class as int),
            self.desc_stride as usize == stride_of(self.class as int),
    {
        let d = self.desc_stride as int;
        assert(slab * d <= self.nslabs * d) by (nonlinear_arith)
            requires slab < self.nslabs, d >= 0;
        assert(self.desc_perms@.contains_key(slab as int));
    }

    /// The descriptor of a slab of the region, the bytes
    /// [`desc_unchecked`](SlabTable::desc_unchecked) points at
    /// (`desc_base + slab * desc_stride`), is metadata.
    pub proof fn lemma_desc_in_meta(&self, slab: int)
        requires
            self.meta_ok(),
            0 <= slab < self.spec_nslabs(),
        ensures
            process_regions().in_meta(self.desc_addr(slab), self.spec_desc_stride()),
            self.spec_desc_stride() >= 0,
    {
        let d = self.desc_stride as int;
        assert(slab * d + d <= self.nslabs * d) by (nonlinear_arith)
            requires
                slab < self.nslabs,
                d >= 0,
        ;
        assert(0 <= slab * d) by (nonlinear_arith)
            requires
                slab >= 0,
                d >= 0,
        ;
    }

    /// The owner word of a hand-out group of the region, the two bytes
    /// [`owner_word`](SlabTable::owner_word) and
    /// [`owner_at`](SlabTable::owner_at) point at
    /// (`owners_base + 2 * group`), is metadata.
    pub proof fn lemma_owner_in_meta(&self, group: int)
        requires
            self.meta_ok(),
            0 <= group,
            group * self.spec_group() < self.spec_nslabs(),
        ensures
            process_regions().in_meta(self.owner_addr(group), 2),
    {
        let (n, g) = (self.nslabs as int, self.group as int);
        assert(group + 1 <= (n + g - 1) / g) by {
            vstd::arithmetic::div_mod::lemma_div_is_ordered(group * g + g, n + g - 1, g);
            vstd::arithmetic::div_mod::lemma_div_multiples_vanish(group + 1, g);
            assert(group * g + g == (group + 1) * g) by (nonlinear_arith);
        }
    }

    /// The bitmap word holding bit `bit` of the class bitmap, the eight
    /// bytes [`is_set_unchecked`](SlabTable::is_set_unchecked) and
    /// [`bits_unchecked`](SlabTable::bits_unchecked) point at
    /// (`bits_base + 8 * (bit / 64)`), is metadata when the bitmap's bits
    /// fill whole words ([`meta_ok`](SlabTable::meta_ok) covers the bits'
    /// bytes, not the last word's padding).
    pub proof fn lemma_bit_word_in_meta(&self, bit: int)
        requires
            self.meta_ok(),
            0 <= bit < self.spec_nbits(),
            self.spec_nbits() % 64 == 0,
        ensures
            process_regions().in_meta(self.bit_word_addr(bit), 8),
    {
        let nb = self.nslabs * (self.slab_bits as int);
        assert(8 * (bit / 64) + 8 <= (nb + 7) / 8) by {
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(nb, 64);
            vstd::arithmetic::div_mod::lemma_div_is_ordered(bit, nb - 1, 64);
            assert((nb - 1) / 64 == nb / 64 - 1) by {
                vstd::arithmetic::div_mod::lemma_fundamental_div_mod(nb - 1, 64);
                vstd::arithmetic::div_mod::lemma_mod_bound(nb - 1, 64);
            }
            assert((nb + 7) / 8 == 8 * (nb / 64)) by {
                vstd::arithmetic::div_mod::lemma_fundamental_div_mod(nb + 7, 8);
            }
        }
    }

    /// The slot tokens' geometry, spelled out: the class's slab bits, slots,
    /// slot size and slab size, the region's base and slab count.
    pub(super) proof fn lemma_slots_wf(&self)
        requires
            self.inv(),
        ensures
            self.spec_slots().wf(),
            self.spec_slots().sb == class_table()[self.spec_class() as int].slab_bits as int,
            self.spec_slots().nslots == class_table()[self.spec_class() as int].nslots as int,
            self.spec_slots().size == class_table()[self.spec_class() as int].size as int,
            self.spec_slots().slab_size == (1usize << class_table()[self.spec_class() as int].lg_slab) as int,
            self.spec_slots().base == process_regions().class_base[self.spec_class() as int] as int,
            self.spec_slots().nbits == self.spec_nslabs() * self.spec_slots().sb,
            self.spec_slots().loc == slot_loc(self.spec_class() as int),
    {
    }

    /// The table's guard slabs are the process's guard slabs of its class
    /// ([`guard_slab`](crate::layout::guard_slab), which
    /// `Regions::is_slot` excludes) and its slot tokens' ([`Slots::guard`]).
    pub(super) proof fn lemma_guard_slab(&self, slab: int)
        requires
            self.inv(),
        ensures
            self.spec_is_guard(slab as usize) == crate::layout::guard_slab(self.spec_class() as int, slab),
            self.spec_is_guard(slab as usize) == self.spec_slots().guard(slab),
    {
    }

    /// The hand-out group of `slab`, `slab >> lg_group`, is one of the
    /// region's, with its owner word below the end of the address space.
    proof fn lemma_group_in_range(&self, slab: usize)
        requires
            self.inv(),
            slab < self.nslabs,
        ensures
            (slab >> self.lg_group) < self.spec_ngroups(),
            self.owners_base + 2 * (slab >> self.lg_group) + 2 <= usize::MAX,
    {
        let lg = self.lg_group as usize;
        let l8 = self.lg_group;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg);
        vstd::bits::lemma_usize_shr_is_div(slab, lg);
        assert((slab >> l8) == (slab >> lg) && (1usize << l8) == (1usize << lg)) by (bit_vector)
            requires lg == l8 as usize;
        let (n, g) = (self.nslabs as int, self.group as int);
        assert(g > 0);
        vstd::arithmetic::div_mod::lemma_div_is_ordered(slab as int, n - 1, g);
        vstd::arithmetic::div_mod::lemma_div_plus_one(n - 1, g);
        assert(g + (n - 1) == n + g - 1);
    }

    /// No markers of the class bitmap's authority: where a class lock
    /// value's record of its slabs' markers starts.
    pub proof fn empty_marks(tracked &self) -> (tracked r: GhostPersistentSubset<int>)
        ensures
            r.id() == self.spec_slots().mloc,
            r@ == Set::<int>::empty(),
    {
        self.mark_auth.borrow().empty().persist()
    }

    /// The process's regions are well formed and separated
    /// ([`regions_ok`](crate::heap::regions_ok)): `new` requires it.
    pub(super) proof fn lemma_regions_ok(&self)
        requires
            self.inv(),
        ensures
            crate::heap::regions_ok(),
    {
        reveal(regions_held);
    }

    /// The table covers its class's region of the process's regions,
    /// whose base is aligned to its slab and the page (`base_aligned`).
    pub(super) proof fn lemma_region(&self)
        requires
            self.inv(),
        ensures
            crate::layout::base_aligned(process_regions().class_base[self.spec_class() as int] as int, self.spec_class() as int, process_regions().lg_page),
            self.spec_nslabs() as int == process_regions().spec_nslabs(self.spec_class() as int),
    {
    }

    /// An empty token of the class bitmap's authority: where a claim's
    /// tokens start.
    pub proof fn empty_tokens(tracked &self) -> (tracked r: GhostSubset<int>)
        ensures
            r.id() == self.spec_slots().loc,
            r@ == Set::<int>::empty(),
    {
        self.slot_auth.borrow().empty()
    }

    /// The slot tokens' geometry is the class's (`Class::wf`), and slot
    /// `slot` of `slab` is bit `slab * slab_bits + slot`, at the slot's
    /// address in the process's regions, and a slot exactly when `slot <
    /// nslots`.
    pub(super) proof fn lemma_slot_bit(&self, slab: int, slot: int)
        requires
            self.inv(),
            0 <= slab < self.spec_nslabs(),
            0 <= slot < class_table()[self.spec_class() as int].slab_bits,
        ensures
            self.spec_slots().wf(),
            self.spec_slots().addr(slab * self.spec_slots().sb + slot)
                == process_regions().slot_at(self.spec_class() as int, slab, slot),
            self.spec_slots().real(slab * self.spec_slots().sb + slot)
                == (slot < class_table()[self.spec_class() as int].nslots),
            0 <= slab * self.spec_slots().sb + slot < self.spec_slots().nbits,
    {
        let s = self.spec_slots();
        let (n, sbi) = (self.nslabs as int, self.slab_bits as int);
        assert(s.nbits == n * sbi);
        assert(0 <= slab * sbi && slab * sbi + sbi <= n * sbi) by (nonlinear_arith)
            requires slab < n, 0 <= slab, sbi >= 0;
        crate::bitmap::lemma_slab_bit(s, slab, slot);
    }

    /// Page groups are `1 << lg_slabs_per_page` slabs and tile the
    /// region. Opaque, so its bit operations stay out of proofs that do
    /// not need them; revealed by `new`.
    #[verifier::opaque]
    pub(super) open spec fn page_groups_ok(self) -> bool {
        &&& self.slabs_per_page as usize == 1usize << self.lg_slabs_per_page
        &&& self.nslabs & sub(self.slabs_per_page as usize, 1) == 0
    }

    /// The slab count, for other modules' contracts.
    pub closed spec fn spec_nslabs(&self) -> usize {
        self.nslabs
    }

    /// The class, for other modules' contracts.
    pub closed spec fn spec_class(&self) -> usize {
        self.class as usize
    }

    /// The start of the class bitmap, for other modules' contracts.
    pub closed spec fn spec_bits_base(&self) -> usize {
        self.bits_base
    }

    /// The start of the group owner array, for other modules' contracts.
    pub closed spec fn spec_owners_base(&self) -> usize {
        self.owners_base
    }

    /// `log2` of the hand-out group, for other modules' contracts.
    pub closed spec fn spec_lg_group(&self) -> u8 {
        self.lg_group
    }

    /// Slabs per page group, for other modules' contracts.
    pub closed spec fn spec_slabs_per_page(&self) -> usize {
        self.slabs_per_page as usize
    }

    /// Whether `slab` is a guard slab ([`is_guard_slab`]).
    ///
    /// [`is_guard_slab`]: SlabTable::is_guard_slab
    pub closed spec fn spec_is_guard(&self, slab: usize) -> bool {
        crate::layout::guard_of(self.lg_slabs_per_page, self.guard_mask, slab)
    }

    /// The slabs a run holds at most ([`run_slabs`](SlabTable::run_slabs)).
    pub closed spec fn spec_run_slabs(&self) -> usize {
        let r = RUN_BYTES >> class_table()[self.class as int].lg_slab;
        if r < 1 { 1 } else { r }
    }

    /// Create the table for `class`. Reserves the descriptor array, the
    /// class bitmap and the owner array in `meta`; returns `None` if the
    /// metadata region cannot hold them. `guard_interval` of `N > 0` makes
    /// every `N`-th page group a guard group; `N` must be a power of two
    /// (`config.rs` rounds a configured value up, `heap::guard_interval_for`
    /// scales by a power of two), so that the sized fast path's test is a
    /// mask; callers must pass 0 when `MADV_GUARD_INSTALL` is unavailable,
    /// since a `PROT_NONE` slab would split the region into one VMA per
    /// guard (design 5). `ord` is the caller's lock-order token,
    /// which the reservations take for the metadata lock. `slot_once` and
    /// `words_once` are the one-shot tokens (ghost, `vspec::once`) of the
    /// steps that fix the class's slot-token authority ([`axiom_slot_loc`])
    /// and its bitmap words' permissions ([`axiom_fresh_words`]). The table
    /// keeps the permissions of its descriptors and the invariants of its
    /// bitmap words, owner words and watermark ([`SlabTable::perms_ok`]),
    /// and its commit lock the markers and descriptor permissions of every
    /// slab ([`TableInner::inv`]).
    ///
    /// The asserts on the arguments are preconditions; the caller also
    /// guarantees well-formed, separated regions (which give a page no
    /// larger than a class region and a class region that ends at or below
    /// `2^57`, inside a user-space mapping, `Regions::reserve`) and that
    /// they and `meta` are the process's
    /// ([`process_regions`](crate::layout::process_regions),
    /// [`Meta::is_process`]), which is what the type invariant and the
    /// tables' claims ([`lazy_ok`]) need.
    #[verifier::spinoff_prover]
    pub fn new(
        meta: &Meta,
        regions: &Regions,
        class: usize,
        guard_interval: u32,
        Tracked(slot_once): Tracked<Once>,
        Tracked(words_once): Tracked<Once>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: Option<SlabTable>)
        requires
            crate::sys::may_abort_corrupt(),
            crate::sys::may_syscall(),
            regions.wf(),
            regions.separated(),
            *regions == process_regions(),
            meta.is_process(),
            class < NCLASSES,
            guard_interval != 1,
            guard_interval == 0 || exists|k: nat| k < 32 && guard_interval as nat == #[trigger] pow2(k),
            old(ord).allows(rank_meta()),
            slot_once.step() == Step::SlotLoc(class as int),
            words_once.step() == Step::FreshWords(class as int),
        ensures
            r matches Some(t) ==> {
                &&& t.spec_class() == class
                &&& t.spec_nslabs() == (1usize << regions.lg_region) >> class_table()[class as int].lg_slab
                &&& meta.is_process() ==> t.meta_ok()
            },
            *final(ord) == *old(ord),
    {
        proof {
            // The page and the region end that the geometry needs, from the
            // separation of the class regions.
            let stride = (1usize << regions.lg_stride) as int;
            let c = class as int;
            assert(regions.superblock + (c + 1) * stride <= regions.superblock + NCLASSES * stride) by (nonlinear_arith)
                requires
                    c < NCLASSES,
                    stride >= 0;
            assert(crate::layout::base_aligned(regions.class_base[c] as int, c, regions.lg_page));
            assert((1usize << 57usize) == 0x200_0000_0000_0000usize) by (bit_vector);
            assert(regions.class_base[class as int] + (1usize << regions.lg_region) <= 1usize << 57);
            assert(regions.lg_page <= regions.lg_region);
        }
        assert!(class < CLASSES.len(), "class: index out of range");
        assert!(guard_interval != 1, "class: a guard interval of 1 leaves no slabs to allocate");
        assert!(
            guard_interval == 0 || guard_interval.is_power_of_two(),
            "class: guard interval {guard_interval} is not a power of two"
        );
        let cls = &CLASSES.table()[class];
        let page = regions.page_size();
        let nslabs = regions.nslabs(class);
        let stagger = desc_stagger(class);
        let align = DESC_ALIGN.max(page);
        let slabs_per_page = (page / cls.slab_size()).max(1);
        proof {
            assert(cls.wf());
            assert(crate::layout::LG_MIN_SLAB == 12 && crate::layout::LG_MAX_SLAB == 22 && regions.lg_region <= 38);
            let l = cls.lg_slab;
            assert((1usize << l) == (1usize << (l as u32))) by (bit_vector);
            lemma_new_geometry(regions.lg_page, l as u32, regions.lg_region, page, 1usize << l, nslabs, slabs_per_page);
        }
        assert!(slabs_per_page.is_power_of_two() && nslabs.is_multiple_of(slabs_per_page));
        let group = cls.slabs_per_line().max(slabs_per_page);
        group_geometry(group, slabs_per_page);
        proof {
            crate::meta::lemma_pow2_bits(group);
            assert(NCLASSES <= 64 && PAD_ALIGN == 128);
            let lp = regions.lg_page;
            assert((1usize << lp) == (1usize << (lp as usize)) && (1usize << 6usize) == 64) by (bit_vector)
                requires lp < 64;
            crate::meta::lemma_is_pow2_shl(lp as usize);
            crate::meta::lemma_is_pow2_shl(6);
            assert(crate::meta::is_pow2(align));
            assert forall|d: usize| d <= 0xffff_ffff implies #[trigger] (nslabs as int * d as int) <= 0x400_0000 * 0xffff_ffff by {
                assert(nslabs as int * d as int <= 0x400_0000 * 0xffff_ffff) by (nonlinear_arith)
                    requires nslabs <= 0x400_0000, d <= 0xffff_ffff;
            }
        }
        let stride = desc_stride(class);
        let tracked mut draw = ZeroRaw::empty(cast_provenance());
        let tracked mut braw = ZeroRaw::empty(cast_provenance());
        let tracked mut oraw = ZeroRaw::empty(cast_provenance());
        // The descriptor permissions need whole, aligned headers per stride
        // (`slab::strides_of` makes the stride 16, 32 or 64; the table it
        // fills is not visible to the verifier).
        check(stride >= HEADER && stride.is_multiple_of(HEADER), "class: descriptor stride is not whole headers");
        let descs = Lazy::reserve(meta, stagger + nslabs * stride, align, Tracked(&mut draw), Tracked(ord))?;
        let bits = Lazy::reserve(
            meta,
            stagger + crate::vspec::div_ceil(nslabs * cls.slab_bits(), 8),
            align,
            Tracked(&mut braw),
            Tracked(ord),
        )?;
        let owners = Lazy::reserve(meta, stagger + crate::vspec::div_ceil(nslabs, group) * 2, align, Tracked(&mut oraw), Tracked(ord))?;
        let bits_base = bits.start() + stagger;
        let owners_base = owners.start() + stagger;
        let desc_base = descs.start() + stagger;
        proof {
            broadcast use vstd::std_specs::bits::axiom_u64_trailing_zeros;
            assert(u64_trailing_zeros(slabs_per_page as u64) < 64);
            assert(u64_trailing_zeros(group as u64) < 64);
            let (b, lr, ls) = (regions.class_base[class as int], regions.lg_region, cls.lg_slab);
            assert(b < 1usize << 57 && nslabs <= (usize::MAX - b) as usize >> ls && nslabs <= ((1usize << 57) - b) as usize >> ls) by (bit_vector)
                requires b + (1usize << lr) <= 1usize << 57, nslabs == (1usize << lr) >> ls, lr <= 38, ls <= 22;
            let k = choose|k: nat| k < 64 && group as nat == #[trigger] pow2(k);
            vstd::bits::lemma_usize_pow2_no_overflow(k);
            vstd::bits::lemma_usize_shl_is_mul(1, k as usize);
            let (ku, k64) = (k as usize, k as u64);
            assert(group == 1usize << ku);
            assert(group == 1usize << k64) by (bit_vector) requires group == 1usize << ku, ku as u64 == k64, k64 < 64;
            lemma_tz_shl(group, k64);
            let lg = u64_trailing_zeros(group as u64) as u8;
            assert(group == 1usize << lg) by (bit_vector) requires group == 1usize << k64, lg as u64 == k64;
            // The page group: `1 << lg_slabs_per_page`, whole in the region.
            let j = choose|j: nat| j < 64 && slabs_per_page as nat == #[trigger] pow2(j);
            vstd::bits::lemma_usize_pow2_no_overflow(j);
            vstd::bits::lemma_usize_shl_is_mul(1, j as usize);
            let (ju, j64) = (j as usize, j as u64);
            assert(slabs_per_page == 1usize << ju);
            assert(slabs_per_page == 1usize << j64) by (bit_vector)
                requires slabs_per_page == 1usize << ju, ju as u64 == j64, j64 < 64;
            lemma_tz_shl(slabs_per_page, j64);
            let lgs = u64_trailing_zeros(slabs_per_page as u64) as u8;
            assert(slabs_per_page == 1usize << lgs) by (bit_vector)
                requires slabs_per_page == 1usize << j64, lgs as u64 == j64;
            assert(lgs <= 26) by (bit_vector)
                requires slabs_per_page == 1usize << j64, lgs as u64 == j64, j64 < 64, slabs_per_page <= 0x400_0000;
            assert(nslabs & sub(slabs_per_page, 1) == 0) by (bit_vector)
                requires nslabs % slabs_per_page == 0, slabs_per_page == 1usize << j64, j64 < 64;
            let lp = regions.lg_page;
            assert((1usize << lp) <= 0x8000_0000_0000_0000usize) by (bit_vector) requires lp < 64;
            reveal(SlabTable::page_groups_ok);
            // The three tables' claims and bounds (`lazy_ok`), and where
            // the arrays end.
            use_type_invariant(meta);
            assert(stagger <= 63 * 384);
            assert(nslabs * stride <= 0x400_0000 * 0xffff_ffff);
            assert(desc_base % DESC_ALIGN == 0) by {
                crate::meta::lemma_pow2_bits(align);
                assert((64usize & sub(64usize, 1)) == 0usize) by (bit_vector);
                crate::meta::lemma_pow2_divides(align, 64);
                crate::meta::lemma_mod_trans(descs.spec_base() as int, align as int, 64);
                vstd::arithmetic::div_mod::lemma_mod_multiples_basic(class as int * 6, 64);
                vstd::arithmetic::div_mod::lemma_add_mod_noop(descs.spec_base() as int, stagger as int, 64);
            }
            let ng = (nslabs + group - 1) / (group as int);
            assert(ng <= nslabs) by {
                assert(nslabs + group - 1 <= nslabs * group) by (nonlinear_arith)
                    requires group >= 1, nslabs >= 1;
                vstd::arithmetic::div_mod::lemma_div_is_ordered(nslabs + group - 1, nslabs * group, group as int);
                vstd::arithmetic::div_mod::lemma_div_multiples_vanish(nslabs as int, group as int);
                assert((nslabs * group) / (group as int) == nslabs) by { vstd::arithmetic::mul::lemma_mul_is_commutative(nslabs as int, group as int); }
                assert((nslabs + group - 1) / (group as int) <= (nslabs * group) / (group as int));
            }
            lemma_mask_shape(guard_interval);
            assert(lazy_ok(descs, page));
            assert(lazy_ok(bits, page));
            assert(lazy_ok(owners, page));
            lemma_groups_paged(regions.class_base[class as int], slabs_per_page, cls.lg_slab, regions.lg_page, page);
        }
        proof {
            reveal(SlabTable::page_groups_ok);
        }
        // The slot tokens: an authority over the region's slots, and the
        // invariant of each bitmap word, which starts zero with the tokens
        // of all its slots.
        let ghost geo = Slots {
            loc: vstd::pervasive::arbitrary(),
            mloc: vstd::pervasive::arbitrary(),
            sloc: vstd::pervasive::arbitrary(),
            sb: cls.slab_bits as int,
            nslots: cls.nslots as int,
            nbits: nslabs * cls.slab_bits,
            base: regions.class_base[class as int] as int,
            slab_size: (1usize << cls.lg_slab) as int,
            size: cls.size as int,
            glg: u64_trailing_zeros(slabs_per_page as u64) as u8,
            gmask: guard_interval.wrapping_sub(1),
        };
        let tracked (slot_auth, all) = GhostSetAuth::<int>::new(crate::bitmap::slot_set(geo));
        let ghost nwords = (geo.nbits + 63) / 64;
        // The markers: the fresh ones of every bit for the word
        // invariants, the init ones and every slab's owner-store one for
        // the commit lock.
        let tracked (mark_auth, mut marks) = GhostSetAuth::<int>::new(
            fresh_marks(0, 64 * nwords).union(init_marks(0, 64 * nwords)).union(owner_marks(0, nslabs as int)));
        let tracked imarks = marks.split(init_marks(0, 64 * nwords).union(owner_marks(0, nslabs as int)));
        proof {
            axiom_slot_loc(class as int, slot_auth.id(), mark_auth.id(), geo.glg, geo.gmask, slot_once);
        }
        // The owner words' witness markers: exclusive in each word's
        // invariant until its store (`OwnerPred`), with the word's
        // permission; and the empty persistent sets of them the commit lock,
        // the watermark and the bitmap words start from.
        let tracked (_sauth, smarks) = GhostSetAuth::<int>::new(owner_marks(0, nslabs as int));
        let ghost sloc = smarks.id();
        let tracked seen0 = smarks.empty().persist();
        let tracked seen1 = seen0.duplicate();
        let ghost slots = Slots {
            loc: slot_auth.id(),
            mloc: mark_auth.id(),
            sloc,
            sb: geo.sb,
            nslots: geo.nslots,
            nbits: geo.nbits,
            base: geo.base,
            slab_size: geo.slab_size,
            size: geo.size,
            glg: geo.glg,
            gmask: geo.gmask,
        };
        proof {
            broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter, Set::lemma_map_contains};
            assert forall|b: int| 0 <= b < 64 * nwords && slots.real(b) implies all@.contains(#[trigger] slots.addr(b)) by {
                assert(Set::<int>::range(0, geo.nbits).filter(|b: int| geo.real(b)).contains(b));
                assert(geo.addr(b) == slots.addr(b));
            }
            assert forall|m: int| #[trigger] fresh_marks(0, 64 * nwords).contains(m) implies marks@.contains(m) by {
                crate::bitmap::lemma_marks_elems(0, 64 * nwords, m);
                crate::bitmap::lemma_marks(0, 64 * nwords, m / 2);
            }
            assert(nslabs * cls.slab_bits <= 64 * nwords);
            crate::bitmap::lemma_marks_sub(0, nslabs * cls.slab_bits, 0, 64 * nwords);
            // The words end inside the bitmap's reservation, below `2^57`.
            let (lb, ln, nb) = (bits.spec_base() as int, bits.spec_len() as int, geo.nbits);
            assert(bits_base + 8 * nwords <= usize::MAX) by (nonlinear_arith)
                requires
                    bits_base as int == lb + stagger,
                    lb + ln <= 0x200_0000_0000_0000,
                    ln >= stagger + nb / 8,
                    nwords == (nb + 63) / 64,
                    0 <= nb,
                    0 <= lb,
                    0 <= stagger,
            ;
            // The words are inside the bitmap's reservation, rounded up to
            // whole words, whose zero permission `braw` is.
            assert(stagger % 8 == 0) by {
                vstd::arithmetic::div_mod::lemma_mod_multiples_basic(class as int * 48, 8);
            }
            lemma_round_words(stagger as int, nb);
            assert(Set::<int>::range(bits_base as int, bits_base + 8 * nwords).subset_of(braw.dom()));
        }
        let tracked (perms, wperms) = fresh_words(braw, bits_base, bits.spec_base(), align, nwords as nat, class as int, words_once);
        let tracked word_invs = crate::bitmap::fresh_word_invs(perms, all, marks, &seen0, slots, bits_base, nwords as nat);
        // The permissions to every descriptor and owner word, made from
        // the two reservations (`SlabTable::perms_ok`).
        proof {
            assert(stride == (stride as u32) as usize);
        }
        let tracked (dmap, omap, dps) = table_perms(
            draw, oraw, descs.spec_base() as int, owners.spec_base() as int,
            align, stagger as int, stride as int, nslabs as int, group as int, class as int);
        // The ghost variable that ties the watermark to `next_slab`: both
        // start at 0, one half for the commit lock, one for the watermark.
        let tracked (wauth, wvar) = GhostVarAuth::<usize>::new(0);
        let ghost wloc = wauth.id();
        let ghost lgg = u64_trailing_zeros(group as u64) as u8;
        let ghost ng = (nslabs + group - 1) / (group as int);
        proof {
            assert((group as u32) as usize == 1usize << lgg && lgg < 64);
            let (n, gr) = (nslabs as int, group as int);
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(n + gr - 1, gr);
            vstd::arithmetic::mul::lemma_mul_is_commutative(ng, gr);
            vstd::arithmetic::div_mod::lemma_mod_bound(n + gr - 1, gr);
            assert(ng == (n + gr - 1) / gr);
            assert(ng * gr + (n + gr - 1) % gr == n + gr - 1);
            assert(0 <= ng);
            assert(ng == 0 || (ng - 1) * gr < n) by (nonlinear_arith)
                requires ng * gr + (n + gr - 1) % gr == n + gr - 1, 0 <= (n + gr - 1) % gr, gr >= 1, ng >= 0;
            assert(ng * gr >= n) by (nonlinear_arith)
                requires ng * gr + (n + gr - 1) % gr == n + gr - 1, (n + gr - 1) % gr < gr;
        }
        let tracked oinvs = fresh_owner_invs(omap, smarks, owners_base as int, class as int, lgg, group as u32, nslabs, ng as nat);
        // The commit lock's invariant (`TableInner::inv`) of the value
        // below, and the descriptors' identities (`perms_ok`).
        proof {
            assert(imarks.id() == mark_auth.id());
            assert(init_marks(0 * (cls.slab_bits as usize), nslabs * (cls.slab_bits as usize)).subset_of(imarks@));
            assert(owner_marks(0, nslabs as int).subset_of(imarks@));
            assert(1 <= page <= 0x8000_0000_0000_0000);
            assert(group >= 1);
            assert(0usize < nslabs ==> 0usize % group == 0);
            // Every descriptor's permission, zero, for the commit lock.
            assert forall|s: int| 0 <= s < nslabs implies #[trigger] dfresh(dps, desc_base as int, stride as int, s) by {
                assert(dps.contains_key(s));
            }
            assert(1 <= page <= 0x8000_0000_0000_0000);
            assert(group >= 1);
            assert(0usize < nslabs ==> 0usize % group == 0);
            // Each descriptor is the one at its address (`perms_ok`).
            assert forall|s: int| 0 <= s < nslabs
                implies desc_id(&(#[trigger] dmap[s]).value()) == desc_base + s * ((stride as u32) as int) by {
                assert(dmap.contains_key(s));
            }
        }
        let (committed_slabs, Tracked(cperm)) = new_usize(0);
        let inner = Padded(Lock::new(
            TableInner {
                descs,
                bits,
                owners,
                next_slab: 0,
                guards: 0,
                page: Ghost(page),
                group: Ghost(group),
                nslabs: Ghost(nslabs),
                sb: Ghost(cls.slab_bits as usize),
                mloc: Ghost(mark_auth.id()),
                dbase: Ghost(desc_base as int),
                dstride: Ghost(stride as int),
                mirror: core::ptr::null(),
                sloc: Ghost(sloc),
                fresh: Tracked(imarks),
                dperms: Tracked(dps),
                seen: Tracked(seen0),
                wloc: Ghost(wloc),
                wm: Tracked(wauth),
                mirror_inv: Tracked(None),
                mauth: Tracked(None),
            },
            Ghost(rank_table(class as int)),
        ));
        proof {
            assert(lock_init(inner.0).sloc@ == sloc);
            assert(lock_init(inner.0).page@ == 1usize << regions.lg_page);
            assert(lock_init(inner.0).group@ == group);
            assert(lock_init(inner.0).nslabs@ == nslabs);
            assert(lock_init(inner.0).sb@ == cls.slab_bits as usize);
            assert(lock_init(inner.0).mloc@ == mark_auth.id());
            assert(lock_init(inner.0).dbase@ == desc_base as int);
            assert(lock_init(inner.0).dstride@ == stride as int);
            assert(inner.0.rank() == rank_table(class as int));
            assert(crate::bitmap::words_wf(word_invs, slots, bits_base, nwords));
            assert(slots.loc == slot_loc(class as int));
            assert(mark_auth.id() == mark_loc(class as int));
            assert(crate::layout::guard_geom(class as int) == (geo.glg, geo.gmask));
        }
        // The watermark, with its invariant (0, at most any slab count).
        let tracked committed_inv = WatermarkInv::new((usize_id(&committed_slabs), nslabs, sloc, wloc), (cperm, seen1, wvar), committed_ns());
        proof {
            reveal(regions_held);
            assert(lock_init(inner.0).wloc@ == wloc);
            assert(committed_inv.constant() == (usize_id(&committed_slabs), nslabs, sloc, wloc));
            assert(committed_inv.namespace() == committed_ns());
        }
        Some(SlabTable {
            bits_base,
            owners_base,
            committed_slabs,
            slab_bits: cls.slab_bits() as u16,
            class: class as u16,
            lg_group: group.trailing_zeros() as u8,
            lg_slabs_per_page: slabs_per_page.trailing_zeros() as u8,
            _pad: [0; 2],
            guard_mask: guard_interval.wrapping_sub(1),
            lg_page: regions.lg_page,
            desc_stride: stride as u32,
            slabs_per_page: slabs_per_page as u32,
            group: group as u32,
            base: regions.class_base[class],
            nslabs,
            desc_base,
            inner,
            _tail: [0; TABLE_TAIL],
            slot_auth: Tracked(slot_auth),
            word_invs: Tracked(vstd::modes::tracked_static_ref(word_invs)),
            word_perms: Tracked(vstd::modes::tracked_static_ref(wperms)),
            mark_auth: Tracked(mark_auth),
            desc_perms: Tracked(dmap),
            owner_invs: Tracked(vstd::modes::tracked_static_ref(oinvs)),
            committed_inv: Tracked(committed_inv),
        })
    }
}

/// The hand-out group's geometry, as [`SlabTable::new`] asserts it:
/// `group` is a power of two that is whole page groups. Inlined and
/// `track_caller`, so the caller keeps the code, the message and the
/// location of the `assert!` it had in place. Both are powers of two
/// (`Class::slabs_per_line`, and the `slabs_per_page` check before it), so
/// the check never fails.
#[inline(always)]
#[track_caller]
fn group_geometry(group: usize, slabs_per_page: usize)
    requires
        crate::meta::is_pow2(group),
        crate::meta::is_pow2(slabs_per_page),
        slabs_per_page <= group,
    ensures
        group % slabs_per_page == 0,
{
    proof {
        let k = choose|k: nat| k < 64 && group as nat == #[trigger] pow2(k);
        crate::meta::lemma_pow2_bits(slabs_per_page);
        vstd::arithmetic::div_mod::lemma_mod_self_0(slabs_per_page as int);
        crate::layout::lemma_pow2_divides_pow2(slabs_per_page, slabs_per_page as int, k);
    }
    assert!(group.is_power_of_two() && group.is_multiple_of(slabs_per_page));
}

/// `1 << k` has `k` trailing zeros.
pub(crate) proof fn lemma_tz_shl(x: usize, k: u64)
    requires
        k < 64,
        x == 1usize << k,
    ensures
        u64_trailing_zeros(x as u64) == k,
{
    broadcast use vstd::std_specs::bits::axiom_u64_trailing_zeros;
    let y = x as u64;
    let t = u64_trailing_zeros(y) as u64;
    assert(y == 1u64 << k) by (bit_vector)
        requires x == 1usize << k, y == x as u64, k < 64;
    assert(y != 0) by (bit_vector)
        requires y == 1u64 << k, k < 64;
    if t < k {
        assert((y >> t) & 1u64 == 0u64) by (bit_vector)
            requires y == 1u64 << k, t < k, k < 64;
    } else if t > k {
        assert((y >> k) & 1u64 == 1u64) by (bit_vector)
            requires y == 1u64 << k, k < 64;
    }
}

/// The page groups [`SlabTable::new`] computes: `max(page / slab, 1)`
/// slabs, a power of two dividing the region's slab count, which is at
/// most `2^26` (a region is at most `2^LG_REGION_PREFERRED` bytes of slabs
/// of at least `2^12`).
proof fn lemma_new_geometry(lp: u32, ls: u32, lr: u32, page: usize, ss: usize, nslabs: usize, spp: usize)
    requires
        page == 1usize << lp,
        ss == 1usize << ls,
        nslabs == (1usize << lr) >> ls,
        spp == (if page / ss > 1 { page / ss } else { 1 }),
        12 <= ls <= 22,
        ls <= lr,
        lp <= lr <= 38,
    ensures
        crate::meta::is_pow2(spp),
        nslabs % spp == 0,
        1 <= spp <= nslabs <= 0x400_0000,
        page > 0,
        (spp as int * ss as int) % (page as int) == 0,
{
    assert(nslabs == 1usize << sub(lr, ls) && nslabs <= 0x400_0000) by (bit_vector)
        requires nslabs == (1usize << lr) >> ls, 12 <= ls <= lr, lr <= 38;
    let (lpu, lsu) = (lp as usize, ls as usize);
    assert(page == 1usize << lpu && ss == 1usize << lsu) by (bit_vector)
        requires page == 1usize << lp, ss == 1usize << ls, lpu == lp as usize, lsu == ls as usize;
    vstd::arithmetic::power2::lemma2_to64();
    vstd::arithmetic::power2::lemma_pow2_strictly_increases(38, 64);
    if lp < 38 {
        vstd::arithmetic::power2::lemma_pow2_strictly_increases(lp as nat, 38);
    }
    vstd::arithmetic::power2::lemma_pow2_strictly_increases(ls as nat, 38);
    vstd::bits::lemma_usize_shl_is_mul(1, lpu);
    vstd::bits::lemma_usize_shl_is_mul(1, lsu);
    vstd::arithmetic::power2::lemma_pow2_pos(lp as nat);
    if lp >= ls {
        let k: u32 = sub(lp, ls);
        let m: u32 = sub(lr, ls);
        assert(page / ss == 1usize << k) by (bit_vector)
            requires page == 1usize << lp, ss == 1usize << ls, k == sub(lp, ls), ls <= lp, lp <= 38;
        assert((1usize << k) >= 1 && (1usize << m) % (1usize << k) == 0 && (1usize << k) <= (1usize << m)) by (bit_vector)
            requires k <= m, m <= 38;
        crate::meta::lemma_is_pow2_shl(k as usize);
        assert((1usize << k) == (1usize << (k as usize))) by (bit_vector);
        // `spp == 2^k` and `2^k * 2^ls == 2^lp`.
        assert(spp == 1usize << k);
        if k < 38 {
            vstd::arithmetic::power2::lemma_pow2_strictly_increases(k as nat, 38);
        }
        vstd::bits::lemma_usize_shl_is_mul(1, k as usize);
        vstd::arithmetic::power2::lemma_pow2_adds(k as nat, ls as nat);
        assert(spp as int * ss as int == page as int);
        vstd::arithmetic::div_mod::lemma_mod_self_0(page as int);
    } else {
        assert(page / ss == 0) by (bit_vector)
            requires page == 1usize << lp, ss == 1usize << ls, lp < ls, ls <= 22;
        crate::meta::lemma_is_pow2_one();
        assert((1usize << sub(lr, ls)) >= 1) by (bit_vector) requires sub(lr, ls) <= 38;
        // `ss == 2^(ls - lp) * page`.
        let d: u32 = sub(ls, lp);
        vstd::arithmetic::power2::lemma_pow2_adds(d as nat, lp as nat);
        vstd::arithmetic::div_mod::lemma_mod_multiples_basic(pow2(d as nat) as int, pow2(lp as nat) as int);
    }
}

/// [`groups_paged`] of a table [`SlabTable::new`] builds: a
/// page-aligned base, a page of at most `MAX_SLAB`, and page groups
/// whose bytes are a page multiple ([`lemma_new_geometry`]).
proof fn lemma_groups_paged(base: usize, spp: usize, ls: u8, lp: u32, page: usize)
    requires
        page == 1usize << lp,
        12 <= lp <= crate::layout::LG_MAX_SLAB,
        (base as int) % (page as int) == 0,
        spp <= 0x400_0000,
        (spp as int * (1usize << ls) as int) % (page as int) == 0,
    ensures
        groups_paged(base, spp as u32, ls, lp),
{
    reveal(groups_paged);
    vstd::bits::lemma_usize_pow2_no_overflow(lp as nat);
    vstd::bits::lemma_usize_shl_is_mul(1, lp as usize);
    vstd::arithmetic::power2::lemma2_to64();
    if lp < 22 {
        vstd::arithmetic::power2::lemma_pow2_strictly_increases(lp as nat, 22);
    }
    assert(crate::layout::MAX_SLAB == 0x40_0000) by (bit_vector);
    crate::meta::lemma_is_pow2_shl(lp as usize);
    vstd::arithmetic::div_mod::lemma_mod_self_0(page as int);
    crate::layout::lemma_pow2_divides_pow2(page, page as int, 22);
    assert((spp as u32) as int == spp as int);
}

/// `guard_interval - 1` of a guard interval [`SlabTable::new`] accepts
/// (0 or a power of two below `2^32`) has [`mask_shape`].
proof fn lemma_mask_shape(gi: u32)
    requires
        gi == 0 || exists|k: nat| k < 32 && gi as nat == #[trigger] pow2(k),
    ensures
        mask_shape(gi.wrapping_sub(1)),
{
    if gi == 0 {
        assert(gi.wrapping_sub(1) == 0xFFFF_FFFFu32);
        assert(mask_shape(0xFFFF_FFFFu32)) by (compute);
    } else {
        let k = choose|k: nat| k < 32 && gi as nat == #[trigger] pow2(k);
        let ku = k as u32;
        vstd::bits::lemma_u32_shl_is_mul(1, ku);
        vstd::bits::lemma_u32_pow2_no_overflow(k);
        assert(gi == 1u32 << ku);
        let m = gi.wrapping_sub(1);
        assert(m == sub(gi, 1));
        assert(gi == 1u32 << ku && ku < 32 && m == sub(gi, 1) ==> m & (m >> 1u32) == m >> 1u32) by (bit_vector);
    }
}

/// The guard test with one slab per page group ([`SlabTable::is_guard_slab`]),
/// for a slab number of a region: with guards off no slab is a guard
/// slab; otherwise slab `x` is one iff its low bits under the mask are
/// all set, so the guard slabs are every `guard_mask + 1`-th slab.
proof fn lemma_guard_test(t: SlabTable, x: usize)
    requires
        t.inv(),
        t.spec_slabs_per_page() == 1,
        x <= 0x400_0000,
    ensures
        t.guard_mask == u32::MAX ==> !t.spec_is_guard(x),
        t.guard_mask != u32::MAX ==> (t.spec_is_guard(x) == (x & (t.guard_mask as usize) == t.guard_mask as usize)),
{
    reveal(crate::layout::guard_of);
    reveal(SlabTable::page_groups_ok);
    let (spp, lg) = (t.slabs_per_page as usize, t.lg_slabs_per_page);
    assert(spp == 1usize << lg && lg <= 26 && spp == 1 ==> lg == 0) by (bit_vector);
    let m = t.guard_mask;
    let mu = m as usize;
    let pg = (x >> lg) as u32;
    assert(pg as usize == x && pg < 0x400_0001u32) by (bit_vector)
        requires pg == (x >> lg) as u32, lg == 0, x <= 0x400_0000;
    if m != 0xFFFF_FFFFu32 {
        assert((add(pg, 1u32) & m == 0) == (pg & m == m)) by (bit_vector)
            requires m & (m >> 1u32) == m >> 1u32, pg < 0x400_0001u32, m != 0xFFFF_FFFFu32;
    }
    assert(add(pg, 1u32) & 0xFFFF_FFFFu32 != 0) by (bit_vector)
        requires pg < 0x400_0001u32;
    assert((pg & m == m) == (x & mu == mu)) by (bit_vector)
        requires pg as usize == x, mu == m as usize;
    assert(pg != u32::MAX);
}

/// `n` slabs of `t`'s class, `n` at most the region's: `n << lg_slab` is
/// their bytes, `n` times the slab, and from the base they end inside the
/// address space.
proof fn lemma_slab_bytes(t: SlabTable, n: usize)
    requires
        t.inv(),
        n <= t.nslabs,
    ensures
        (n << class_table()[t.class as int].lg_slab) as int == n * ((1usize << class_table()[t.class as int].lg_slab) as int),
        n * ((1usize << class_table()[t.class as int].lg_slab) as int) <= t.nslabs * ((1usize << class_table()[t.class as int].lg_slab) as int),
        t.base + t.nslabs * ((1usize << class_table()[t.class as int].lg_slab) as int) <= usize::MAX,
{
    let lg = class_table()[t.class as int].lg_slab as usize;
    let m = (usize::MAX - t.base) as usize;
    assert(class_table()[t.class as int].wf());
    vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
    vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(t.nslabs, lg, m);
    let p = pow2(lg as nat) as int;
    assert(n * p <= t.nslabs * p) by (nonlinear_arith)
        requires n <= t.nslabs, p >= 0;
    vstd::bits::lemma_usize_shl_is_mul(n, lg);
    vstd::bits::lemma_usize_shl_is_mul(1, lg);
}

/// The commit contract ([`commit_inside`], [`commit_spares_guards`]) for
/// the `len` bytes from slab `a` of `t`'s region, `len` at most the bytes
/// of slabs `a..b`, given it for the guard slabs of `a..b`: each is apart
/// from the range or starts in its first page, which an
/// `MADV_GUARD_INSTALL` marked. The slabs below `a` end by the range's
/// start and those from `b` start past its end.
proof fn lemma_commit_slabs(t: SlabTable, a: usize, b: usize, len: int)
    requires
        t.inv(),
        a < b <= t.nslabs,
        0 <= len <= (b - a) * ((1usize << class_table()[t.class as int].lg_slab) as int),
        forall|g: int|
            a <= g < b && #[trigger] t.spec_is_guard(g as usize) ==> {
                let (slab, page) = ((1usize << class_table()[t.class as int].lg_slab) as int, 1usize << t.lg_page);
                ||| crate::layout::disjoint(t.base + a * slab, len, t.base + g * slab, slab)
                ||| (sys::madvise_ok((t.base + a * slab) as usize, page, MADV_GUARD_INSTALL) && t.base + g * slab < t.base + a * slab + page)
            },
    ensures
        ({
            let addr = t.base + a * ((1usize << class_table()[t.class as int].lg_slab) as int);
            &&& commit_inside(t.class as int, addr, len)
            &&& commit_spares_guards(t.class as int, addr, len)
        }),
{
    let c = t.class as int;
    let slab = (1usize << class_table()[c].lg_slab) as int;
    let r = process_regions();
    lemma_slab_bytes(t, b);
    assert(slab > 0) by {
        assert(class_table()[c].wf());
        let lg = class_table()[c].lg_slab;
        assert((1usize << lg) > 0) by (bit_vector)
            requires lg <= 22;
    }
    assert(a * slab + (b - a) * slab == b * slab) by (nonlinear_arith);
    vstd::arithmetic::mul::lemma_mul_inequality(b as int, t.nslabs as int, slab);
    assert(r.spec_nslabs(c) == t.nslabs);
    assert forall|g: int| 0 <= g < r.spec_nslabs(c) && #[trigger] crate::layout::guard_slab(c, g) implies {
        let page = 1usize << r.lg_page;
        ||| crate::layout::disjoint(t.base + a * slab, len, r.slab_span(c, g).0, r.slab_span(c, g).1)
        ||| (sys::madvise_ok((t.base + a * slab) as usize, page, MADV_GUARD_INSTALL) && t.base + a * slab <= r.slab_span(c, g).0 < t.base + a * slab + page)
    } by {
        assert(t.spec_is_guard(g as usize));
        if g < a {
            vstd::arithmetic::mul::lemma_mul_inequality(g + 1, a as int, slab);
            assert((g + 1) * slab == g * slab + slab) by (nonlinear_arith);
        } else if g >= b {
            vstd::arithmetic::mul::lemma_mul_inequality(b as int, g, slab);
        } else {
            vstd::arithmetic::mul::lemma_mul_inequality(a as int, g, slab);
        }
    }
}

/// [`SlabTable::commit_run`]'s commit of slabs `a..b`, none of whose
/// slabs past `a` is a guard slab: in the region, and apart from the
/// guards when `a` is no guard slab either or `MADV_GUARD_INSTALL` marked
/// its first page. `(b - a) << lg_slab` is its length.
proof fn lemma_run_commit(t: SlabTable, a: usize, b: usize, marked: bool)
    requires
        t.inv(),
        a < b <= t.nslabs,
        forall|g: usize| a < g < b ==> !#[trigger] t.spec_is_guard(g),
        marked ==> sys::madvise_ok(
            (t.base + a * ((1usize << class_table()[t.class as int].lg_slab) as int)) as usize,
            1usize << t.lg_page,
            MADV_GUARD_INSTALL,
        ),
        !marked ==> !t.spec_is_guard(a),
    ensures
        ({
            let slab = (1usize << class_table()[t.class as int].lg_slab) as int;
            &&& (((b - a) as usize) << class_table()[t.class as int].lg_slab) as int == (b - a) * slab
            &&& commit_inside(t.class as int, t.base + a * slab, (b - a) * slab)
            &&& commit_spares_guards(t.class as int, t.base + a * slab, (b - a) * slab)
        }),
{
    lemma_slab_bytes(t, (b - a) as usize);
    let lp = t.lg_page;
    assert((1usize << lp) > 0) by (bit_vector)
        requires lp < 64;
    assert forall|g: int| a <= g < b && #[trigger] t.spec_is_guard(g as usize) implies g == a && marked by {
        if g > a {
            assert(!t.spec_is_guard(g as usize));
        }
    }
    lemma_commit_slabs(t, a, b, (b - a) * ((1usize << class_table()[t.class as int].lg_slab) as int));
}

/// [`SlabTable::commit_slab`]'s commit of the page group slab `s` starts,
/// `len` bytes: its slab, or the page when the slab is smaller. It lies
/// in the region, inside slabs `s..s + slabs_per_page`, and spares the
/// guards when `s` is no guard slab or `MADV_GUARD_INSTALL` marked the
/// group's first page: a page group's slabs are all guard slabs or none.
proof fn lemma_page_group_commit(t: SlabTable, s: usize, len: usize, addr: usize)
    requires
        t.inv(),
        addr == t.base + s * ((1usize << class_table()[t.class as int].lg_slab) as int),
        s < t.nslabs,
        s % (t.slabs_per_page as usize) == 0,
        len == (if (1usize << class_table()[t.class as int].lg_slab) >= (1usize << t.lg_page) {
            1usize << class_table()[t.class as int].lg_slab
        } else {
            1usize << t.lg_page
        }),
    ensures
        ({
            &&& commit_inside(t.class as int, addr as int, len as int)
            &&& addr + len <= t.base + (t.nslabs << class_table()[t.class as int].lg_slab)
            &&& (!t.spec_is_guard(s) || sys::madvise_ok(addr, 1usize << t.lg_page, MADV_GUARD_INSTALL))
                ==> commit_spares_guards(t.class as int, addr as int, len as int)
        }),
{
    reveal(crate::layout::guard_of);
    reveal(groups_paged);
    reveal(SlabTable::page_groups_ok);
    let c = t.class as int;
    let lgs = class_table()[c].lg_slab;
    let slab = (1usize << lgs) as int;
    let page = 1usize << t.lg_page;
    let (spp, lg, n) = (t.slabs_per_page as usize, t.lg_slabs_per_page, t.nslabs);
    assert(class_table()[c].wf());
    assert(spp == 1usize << lg && lg <= 26 && s % spp == 0 && n & sub(spp, 1) == 0 && s < n && n <= 0x400_0000
        ==> s + spp <= n) by (bit_vector);
    let b = (s + spp) as usize;
    // The group's bytes, `spp` slabs, are a whole number of pages (at
    // least one) and at least a slab.
    assert(groups_paged(t.base, t.slabs_per_page, lgs, t.lg_page));
    assert(page > 0 && slab > 0) by {
        let lp = t.lg_page;
        assert((1usize << lp) > 0 && (1usize << lgs) > 0) by (bit_vector)
            requires lp < 64, lgs <= 22;
    }
    assert(spp * slab >= page) by {
        let (x, pg) = (t.slabs_per_page as int * (1usize << lgs) as int, (1usize << t.lg_page) as int);
        assert(x % pg == 0);
        assert(x > 0) by (nonlinear_arith)
            requires x == t.slabs_per_page as int * slab, t.slabs_per_page >= 1, slab > 0;
        vstd::arithmetic::div_mod::lemma_mod_is_zero(x as nat, pg as nat);
        assert(x == spp * slab);
    }
    assert(spp * slab >= slab) by (nonlinear_arith)
        requires spp >= 1, slab > 0;
    assert((b - s) * slab == spp * slab);
    lemma_slab_bytes(t, n);
    assert(s * slab + spp * slab == b * slab) by (nonlinear_arith)
        requires b == s + spp;
    vstd::arithmetic::mul::lemma_mul_inequality(b as int, n as int, slab);
    assert(process_regions().spec_nslabs(c) == n);
    if !t.spec_is_guard(s) || sys::madvise_ok(addr, page, MADV_GUARD_INSTALL) {
        assert forall|g: int| s <= g < b && #[trigger] t.spec_is_guard(g as usize) implies {
            ||| crate::layout::disjoint(addr as int, len as int, t.base + g * slab, slab)
            ||| (sys::madvise_ok(addr, page, MADV_GUARD_INSTALL) && t.base + g * slab < addr as int + page as int)
        } by {
            let gu = g as usize;
            assert(spp == 1usize << lg && lg <= 26 && s % spp == 0 && s <= gu && gu < s + spp && s + spp <= 0x400_0000
                ==> gu >> lg == s >> lg) by (bit_vector);
            assert(t.spec_is_guard(s));
            assert(g * slab == s * slab + (g - s) * slab) by (nonlinear_arith);
            if g > s {
                vstd::arithmetic::mul::lemma_mul_inequality(1, g - s, slab);
            }
        }
        assert((t.base + s * slab) as usize == addr);
        lemma_commit_slabs(t, s, b, len as int);
    }
}

/// [`SlabTable::run_slabs`] is a power of two from 1 to 128.
proof fn lemma_run_slabs(t: SlabTable)
    requires
        t.inv(),
    ensures
        1 <= t.spec_run_slabs() <= 0x80,
        t.spec_run_slabs() & sub(t.spec_run_slabs(), 1) == 0,
{
    let lg = class_table()[t.class as int].lg_slab;
    assert(class_table()[t.class as int].wf());
    assert((512usize << 10usize) == 0x8_0000usize) by (bit_vector);
    assert(12 <= lg <= 22 ==> (0x8_0000usize >> lg) <= 0x80
        && ((0x8_0000usize >> lg) == 0 || (0x8_0000usize >> lg) & sub(0x8_0000usize >> lg, 1) == 0)) by (bit_vector);
    assert(1usize & sub(1usize, 1) == 0) by (bit_vector);
}

/// `spec_nslabs` is the field, for the rest of `class/`.
pub(super) broadcast proof fn lemma_spec_nslabs(t: SlabTable)
    ensures
        #[trigger] t.spec_nslabs() == t.nslabs,
{
}

/// `spec_class` is the field, for the rest of `class/`.
pub(super) broadcast proof fn lemma_spec_class(t: SlabTable)
    ensures
        #[trigger] t.spec_class() == t.class as usize,
{
}

/// `spec_slabs_per_page` is the field, for the rest of `class/`.
pub(super) broadcast proof fn lemma_spec_slabs_per_page(t: SlabTable)
    ensures
        #[trigger] t.spec_slabs_per_page() == t.slabs_per_page as usize,
{
}

} // verus!

verus! {

/// The permissions [`SlabTable::new`] makes from its descriptor and owner
/// reservations (`draw` and `oraw`, from `Lazy::reserve`, which read as
/// zero, at `d` and `o`, aligned to `align`, each starting with `stagger`
/// bytes): to the descriptors of `nslabs` slabs every `stride` bytes, with
/// their [`DescPerm`]s, and to the owner words of the hand-out groups of
/// `group` slabs ([`axiom_zero_desc`], [`axiom_zero_owner`]).
/// [`SlabTable::perms_ok`]'s maps, and the descriptors' permissions.
#[verifier::spinoff_prover]
proof fn table_perms(
    tracked draw: ZeroRaw,
    tracked oraw: ZeroRaw,
    d: int,
    o: int,
    align: usize,
    stagger: int,
    stride: int,
    nslabs: int,
    group: int,
    class: int,
) -> (tracked r: (Map<int, PointsTo<SlabDesc>>, Map<int, PointsTo<AtomicU16>>, Map<int, DescPerm>))
    requires
        crate::meta::is_pow2(align),
        align >= 64,
        0 < d,
        0 < o,
        d % (align as int) == 0,
        o % (align as int) == 0,
        0 <= class,
        stagger == class * 3 * PAD_ALIGN,
        16 <= stride <= u32::MAX,
        stride % 16 == 0,
        1 <= nslabs,
        1 <= group,
        d + stagger + nslabs * stride <= crate::meta::va_limit(),
        o + stagger + (nslabs + group - 1) / group * 2 <= crate::meta::va_limit(),
        draw.is_range(d, (stagger + nslabs * stride + 7) / 8 * 8),
        oraw.is_range(o, (stagger + (nslabs + group - 1) / group * 2 + 7) / 8 * 8),
        draw.provenance() == cast_provenance(),
        oraw.provenance() == cast_provenance(),
    ensures
        forall|s: int| #![trigger r.0.contains_key(s)] #![trigger r.0[s]] 0 <= s < nslabs ==> r.0.contains_key(s)
            && perm_at(r.0[s], d + stagger + s * stride)
            && desc_id(&r.0[s].value()) == d + stagger + s * stride,
        forall|s: int| #![trigger r.2.contains_key(s)] #![trigger r.2[s]] 0 <= s < nslabs ==> r.2.contains_key(s)
            && desc_fresh(r.2[s], d + stagger + s * stride),
        forall|g: int| #![trigger r.1.contains_key(g)] #![trigger r.1[g]] 0 <= g < (nslabs + group - 1) / group ==> r.1.contains_key(g)
            && perm_at(r.1[g], o + stagger + 2 * g),
{
    let ngroups = (nslabs + group - 1) / group;
    let a = align;
    assert(a >= 64 && a & sub(a, 1) == 0 ==> a % 64 == 0) by (bit_vector);
    crate::meta::lemma_pow2_bits(a);
    crate::meta::lemma_mod_trans(d, align as int, 64);
    crate::meta::lemma_mod_trans(o, align as int, 64);
    assert(stagger % 64 == 0) by {
        vstd::arithmetic::div_mod::lemma_mod_multiples_basic(class * 6, 64);
    }
    assert(0 <= ngroups <= nslabs) by {
        assert(nslabs + group - 1 <= nslabs * group) by (nonlinear_arith)
            requires group >= 1, nslabs >= 1;
        vstd::arithmetic::div_mod::lemma_div_is_ordered(nslabs + group - 1, nslabs * group, group);
        vstd::arithmetic::div_mod::lemma_div_multiples_vanish(nslabs, group);
    }
    assert(nslabs * stride >= 0) by (nonlinear_arith)
        requires stride >= 0, nslabs >= 0;
    lemma_round_up8(stagger + nslabs * stride);
    lemma_round_up8(stagger + ngroups * 2);
    let tracked dz = carve_zero(draw, d + stagger, stride, 16, nslabs);
    let tracked (dmap, dps) = zero_descs(dz, d + stagger, stride, nslabs);
    let tracked oz = carve_zero(oraw, o + stagger, 2, 2, ngroups);
    let tracked omap = zero_owners(oz, o + stagger, ngroups);
    (dmap, omap, dps)
}

/// The `n` pieces of `z` of `size` bytes at `base`, `base + stride`, ...,
/// keys `0..n`, each reading as zero (`ZeroRaw::split`).
proof fn carve_zero(tracked z: ZeroRaw, base: int, stride: int, size: int, n: int) -> (tracked r: Map<int, ZeroRaw>)
    requires
        0 <= n,
        0 < size <= stride,
        0 <= base,
        Set::<int>::range(base, base + n * stride).subset_of(z.dom()),
    ensures
        forall|i: int| #![trigger r.contains_key(i)] #![trigger r[i]] 0 <= i < n ==> r.contains_key(i)
            && r[i].is_range(base + i * stride, size) && r[i].provenance() == z.provenance(),
    decreases n,
{
    broadcast use vstd::set_lib::range_set_properties;
    if n == 0 {
        Map::tracked_empty()
    } else {
        let m = n - 1;
        let a = base + m * stride;
        assert(m * stride + stride == n * stride && 0 <= m * stride) by (nonlinear_arith)
            requires m == n - 1, 0 < n, stride > 0;
        assert(Set::<int>::range(a, a + size).subset_of(z.dom()));
        let tracked (piece, rest) = z.split(Set::<int>::range(a, a + size));
        assert(Set::<int>::range(base, base + m * stride).subset_of(rest.dom()));
        let tracked mut r = carve_zero(rest, base, stride, size, m);
        let ghost before = r;
        r.tracked_insert(m, piece);
        assert forall|i: int| #![trigger r.contains_key(i)] #![trigger r[i]] 0 <= i < n implies r.contains_key(i)
            && r[i].is_range(base + i * stride, size) && r[i].provenance() == z.provenance() by {
            if i < m {
                assert(before.contains_key(i));
            }
        }
        r
    }
}

/// [`table_perms`]' descriptors: the `n` zero pieces of `z` at `base`,
/// `base + stride`, ..., each made a descriptor with its permission
/// ([`axiom_zero_desc`]).
proof fn zero_descs(tracked z: Map<int, ZeroRaw>, base: int, stride: int, n: int) -> (tracked r: (Map<int, PointsTo<SlabDesc>>, Map<int, DescPerm>))
    requires
        0 <= n,
        16 <= stride,
        base % 16 == 0,
        stride % 16 == 0,
        0 < base,
        base + n * stride <= usize::MAX,
        forall|i: int| #![trigger z.contains_key(i)] #![trigger z[i]] 0 <= i < n ==> z.contains_key(i)
            && z[i].is_range(base + i * stride, 16) && z[i].provenance() == cast_provenance(),
    ensures
        forall|s: int| #![trigger r.0.contains_key(s)] #![trigger r.0[s]] 0 <= s < n ==> r.0.contains_key(s)
            && perm_at(r.0[s], base + s * stride) && desc_id(&r.0[s].value()) == base + s * stride,
        forall|s: int| #![trigger r.1.contains_key(s)] #![trigger r.1[s]] 0 <= s < n ==> r.1.contains_key(s)
            && desc_fresh(r.1[s], base + s * stride),
    decreases n,
{
    if n == 0 {
        (Map::tracked_empty(), Map::tracked_empty())
    } else {
        let m = n - 1;
        let a = base + m * stride;
        assert(0 <= m * stride && m * stride + stride == n * stride) by (nonlinear_arith)
            requires m == n - 1, 0 < n, stride > 0;
        assert(a % 16 == 0) by {
            vstd::arithmetic::div_mod::lemma_mod_multiples_basic(m * (stride / 16), 16);
            assert(m * stride == m * (stride / 16) * 16) by (nonlinear_arith)
                requires stride == (stride / 16) * 16;
            vstd::arithmetic::div_mod::lemma_add_mod_noop(base, m * stride, 16);
        }
        let tracked mut z = z;
        assert(z.contains_key(m));
        let tracked piece = z.tracked_remove(m);
        let tracked (p, dp) = axiom_zero_desc(piece, a);
        let tracked (mut descs, mut dps) = zero_descs(z, base, stride, m);
        let ghost (d0, p0) = (descs, dps);
        descs.tracked_insert(m, p);
        dps.tracked_insert(m, dp);
        assert forall|s: int| #![trigger descs.contains_key(s)] #![trigger descs[s]] 0 <= s < n implies descs.contains_key(s)
            && perm_at(descs[s], base + s * stride) && desc_id(&descs[s].value()) == base + s * stride by {
            if s < m {
                assert(d0.contains_key(s));
            }
        }
        assert forall|s: int| #![trigger dps.contains_key(s)] #![trigger dps[s]] 0 <= s < n implies dps.contains_key(s)
            && desc_fresh(dps[s], base + s * stride) by {
            if s < m {
                assert(p0.contains_key(s));
            }
        }
        (descs, dps)
    }
}

/// [`table_perms`]' owner words: the `n` zero pieces of `z` at `base`,
/// `base + 2`, ..., each made an owner word ([`axiom_zero_owner`]).
proof fn zero_owners(tracked z: Map<int, ZeroRaw>, base: int, n: int) -> (tracked r: Map<int, PointsTo<AtomicU16>>)
    requires
        0 <= n,
        base % 2 == 0,
        0 < base,
        base + n * 2 <= usize::MAX,
        forall|i: int| #![trigger z.contains_key(i)] #![trigger z[i]] 0 <= i < n ==> z.contains_key(i)
            && z[i].is_range(base + i * 2, 2) && z[i].provenance() == cast_provenance(),
    ensures
        forall|g: int| #![trigger r.contains_key(g)] #![trigger r[g]] 0 <= g < n ==> r.contains_key(g) && perm_at(r[g], base + 2 * g),
    decreases n,
{
    if n == 0 {
        Map::tracked_empty()
    } else {
        let m = n - 1;
        let tracked mut z = z;
        assert(z.contains_key(m));
        let tracked piece = z.tracked_remove(m);
        let tracked w = axiom_zero_owner(piece, base + m * 2);
        let tracked mut r = zero_owners(z, base, m);
        let ghost r0 = r;
        r.tracked_insert(m, w);
        assert forall|g: int| #![trigger r.contains_key(g)] #![trigger r[g]] 0 <= g < n implies r.contains_key(g) && perm_at(r[g], base + 2 * g) by {
            if g < m {
                assert(r0.contains_key(g));
            }
        }
        r
    }
}

/// Trusted: the 16 bytes at `addr`, which read as zero (`z`, a piece of
/// the descriptor array `Meta::reserve_table` returned), are a
/// `SlabDesc`, initialised: plain atomics, valid for every bit pattern
/// (`PointsToRaw::into_typed` with the value the bytes hold; vstd's raw
/// permissions do not track byte values). Also the one [`DescPerm`] of
/// that descriptor: the descriptor at `addr` is the one [`desc_id`] names
/// there, and the permission records its zero fields, which nothing has
/// written yet. One permission per descriptor: `z` is used up, and a
/// `ZeroRaw` for bytes is minted only where the metadata allocator carves
/// them, once (`ZeroRaw::axiom_new`).
axiom fn axiom_zero_desc(tracked z: ZeroRaw, addr: int) -> (tracked r: (PointsTo<SlabDesc>, DescPerm))
    requires
        z.is_range(addr, 16),
        z.provenance() == cast_provenance(),
        0 < addr,
        addr % 16 == 0,
        addr + 16 <= usize::MAX,
    ensures
        perm_at(r.0, addr),
        desc_id(&r.0.value()) == addr,
        desc_fresh(r.1, addr),
;

/// Trusted: the two bytes at `addr`, which read as zero (`z`, a piece of
/// the owner array `Meta::reserve_table` returned), are an `AtomicU16`,
/// initialised (valid for every bit pattern), as [`axiom_zero_desc`].
axiom fn axiom_zero_owner(tracked z: ZeroRaw, addr: int) -> (tracked r: PointsTo<AtomicU16>)
    requires
        z.is_range(addr, 2),
        z.provenance() == cast_provenance(),
        0 < addr,
        addr % 2 == 0,
        addr + 2 <= usize::MAX,
    ensures
        perm_at(r, addr),
;

/// Trusted: `SlabTable::new` runs once per class in a process, so the
/// authority it makes is the class's ([`slot_loc`]), and so is its marker
/// authority (`mloc`, [`mark_loc`]). Two calls for one class with two
/// authorities would prove `false`, so each uses up `once`, the token of
/// class `c`'s step (`vspec::once`). Premise, not checked: one token per
/// step per process; `Shared::init` (unverified) passes `SlabTable::new`
/// a token it takes on trust (`Tracked::assume_new()`), and runs once per
/// process. For the same reason the table's guard geometry (`glg`, its
/// `lg_slabs_per_page`, and `gmask`, its `guard_mask`) is the class's
/// ([`guard_geom`](crate::layout::guard_geom)): the guard slabs that
/// `Regions::is_slot` excludes are this table's.
// `once` is read only by the requires (ghost).
#[allow(unused_variables)]
axiom fn axiom_slot_loc(c: int, loc: Loc, mloc: Loc, glg: u8, gmask: u32, tracked once: Once)
    requires
        once.step() == Step::SlotLoc(c),
    ensures
        slot_loc(c) == loc,
        mark_loc(c) == mloc,
        crate::layout::guard_geom(c) == (glg, gmask);

/// Trusted: the permissions of the `n` words of the class bitmap at
/// `base`, each holding zero: `z`, the bitmap's reservation from
/// `Meta::reserve_table`, reads as zero over them. Eight zero bytes are
/// the word 0 in either byte order; vstd's raw permissions do not track
/// byte values. One permission per word: `z` is used up, and so is
/// `once`, the token of class `c`'s step (`vspec::once`; its premises as
/// for [`axiom_slot_loc`]). The words end in the address space
/// (required): a permission's `addr` is a `usize`. With each word's
/// value permission (`WordPerm`), the permission to borrow the word
/// itself (`r.1`, as [`axiom_zero_owner`] makes an owner word's: eight
/// aligned zero bytes are an `AtomicU64`, initialised, valid for every
/// bit pattern), at the address the value permission names
/// ([`word_addr`]).
// `z` and `once` are read only by the requires (ghost).
#[allow(unused_variables)]
axiom fn axiom_fresh_words(tracked z: ZeroRaw, base: usize, n: nat, c: int, tracked once: Once)
    -> (tracked r: (Map<int, WordPerm>, Map<int, PointsTo<AtomicU64>>))
    requires
        Set::<int>::range(base as int, base + 8 * n).subset_of(z.dom()),
        z.provenance() == cast_provenance(),
        0 < base,
        base % 8 == 0,
        base + 8 * n <= usize::MAX,
        once.step() == Step::FreshWords(c),
    ensures
        forall|j: int| #![trigger r.0.contains_key(j)] #![trigger r.0[j]] 0 <= j < n ==> r.0.contains_key(j) && r.0[j].addr() == base + 8 * j && r.0[j].value() == 0,
        words_perms_ok(r.1, base as int, n as int);

/// [`axiom_fresh_words`] for the class bitmap at `base`, a whole number of
/// words into its reservation at `start`, aligned to `align`: the words
/// are aligned. Its own function, so that `SlabTable::new`'s
/// proof does not carry the alignment facts.
proof fn fresh_words(tracked z: ZeroRaw, base: usize, start: usize, align: usize, n: nat, c: int, tracked once: Once)
    -> (tracked r: (Map<int, WordPerm>, Map<int, PointsTo<AtomicU64>>))
    requires
        Set::<int>::range(base as int, base + 8 * n).subset_of(z.dom()),
        z.provenance() == cast_provenance(),
        crate::meta::is_pow2(align),
        align >= 64,
        start % align == 0,
        0 < start <= base,
        (base - start) % 8 == 0,
        base + 8 * n <= usize::MAX,
        once.step() == Step::FreshWords(c),
    ensures
        forall|j: int| #![trigger r.0.contains_key(j)] #![trigger r.0[j]] 0 <= j < n ==> r.0.contains_key(j) && r.0[j].addr() == base + 8 * j && r.0[j].value() == 0,
        words_perms_ok(r.1, base as int, n as int),
{
    crate::meta::lemma_pow2_bits(align);
    assert(start % align == 0 && align & sub(align, 1) == 0 && align >= 64 ==> start % 8 == 0) by (bit_vector);
    assert(base % 8 == 0) by {
        vstd::arithmetic::div_mod::lemma_add_mod_noop(start as int, base - start, 8);
    }
    axiom_fresh_words(z, base, n, c, once)
}

/// The class bitmap's words fit in its reservation rounded up to a whole
/// word: `st` bytes of stagger (a whole number of words), then the bytes
/// of `nb` bits.
proof fn lemma_round_words(st: int, nb: int)
    requires
        st >= 0,
        st % 8 == 0,
        nb >= 0,
    ensures
        st + 8 * ((nb + 63) / 64) <= (st + (nb + 7) / 8 + 7) / 8 * 8,
{
    let c = (nb + 7) / 8;
    let w = (nb + 63) / 64;
    let d = (c + 7) / 8;
    let s = st / 8;
    let e = (st + c + 7) / 8;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(nb + 7, 8);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(c + 7, 8);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(nb + 63, 64);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(st, 8);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(st + c + 7, 8);
    assert(nb <= 8 * c <= nb + 7);
    assert(nb <= 64 * w <= nb + 63);
    assert(c <= 8 * d <= c + 7);
    assert(st == 8 * s);
    assert(st + c <= 8 * e <= st + c + 7);
    assert(w <= d);
    assert(e == s + d);
}

/// The slabs of hand-out group `g` of a table with `2^lg` slabs per group
/// and `nslabs` slabs: from `g << lg`, `group` of them, clipped to the
/// region ([`SlabTable::group_range`]).
pub(super) open spec fn group_range_at(lg: u8, group: u32, nslabs: usize, g: usize) -> (usize, usize) {
    let start = g << lg;
    (start, if start + group as usize <= nslabs { (start + group as usize) as usize } else { nslabs })
}

/// [`group_range_at`] in numbers, for a group that starts inside the
/// region: it starts at `g * group` and ends `group` slabs later or at the
/// region's end.
proof fn lemma_range_at(lg: u8, group: u32, nslabs: usize, g: usize)
    requires
        lg < 64,
        group as usize == 1usize << lg,
        (g as int) * (group as int) < nslabs,
    ensures
        group_range_at(lg, group, nslabs, g).0 == (g as int) * (group as int),
        group_range_at(lg, group, nslabs, g).1 == if (g as int) * (group as int) + group <= nslabs {
            (g as int) * (group as int) + group
        } else {
            nslabs as int
        },
{
    let lgu = lg as usize;
    vstd::bits::lemma_usize_pow2_no_overflow(lgu as nat);
    vstd::bits::lemma_usize_shl_is_mul(1, lgu);
    assert((1usize << lg) == (1usize << lgu) && (g << lg) == (g << lgu)) by (bit_vector)
        requires lgu == lg as usize;
    vstd::bits::lemma_usize_shl_is_mul(g, lgu);
}

/// `i` is the invariant [`OwnerPred`] of the owner word of hand-out group
/// `g` of class `class`'s table, whose owner array starts at `base`, with
/// `2^lg` slabs per group, `nslabs` slabs and its witness markers at
/// `sloc`.
pub(super) open spec fn owner_inv_at(i: OwnerInv, base: int, class: int, lg: u8, group: u32, nslabs: usize, sloc: Loc, g: int) -> bool {
    &&& i.constant() == OwnerKey {
        ptr: owner_ptr_at(base + 2 * g),
        class,
        lo: group_range_at(lg, group, nslabs, g as usize).0 as int,
        hi: group_range_at(lg, group, nslabs, g as usize).1 as int,
        sloc,
    }
    &&& i.namespace() == owner_ns()
}

/// The invariants ([`OwnerPred`]) of the owner words of hand-out groups
/// `0..n`, as `new` makes them: not stored, each with its word's
/// permission, taken from `perms`, and the witness markers of its slabs,
/// split off `marks`.
proof fn fresh_owner_invs(
    tracked perms: Map<int, PointsTo<AtomicU16>>,
    tracked marks: GhostSubset<int>,
    base: int,
    class: int,
    lg: u8,
    group: u32,
    nslabs: usize,
    n: nat,
) -> (tracked r: Map<int, OwnerInv>)
    requires
        forall|g: int| #![trigger perms.contains_key(g)] #![trigger perms[g]] 0 <= g < n ==> perms.contains_key(g) && perm_at(perms[g], base + 2 * g),
        lg < 64,
        group as usize == 1usize << lg,
        n == 0 || (n - 1) * (group as int) < nslabs,
        owner_marks(0, if n * (group as int) <= nslabs { n * (group as int) } else { nslabs as int }).subset_of(marks@),
    ensures
        forall|g: int| #![trigger r.contains_key(g)] #![trigger r[g]] 0 <= g < n ==> r.contains_key(g)
            && owner_inv_at(r[g], base, class, lg, group, nslabs, marks.id(), g),
    decreases n,
{
    broadcast use vstd::set_lib::range_set_properties;
    if n == 0 {
        Map::tracked_empty()
    } else {
        let g = n - 1;
        let gr = group as int;
        assert((1usize << lg) >= 1) by (bit_vector)
            requires lg < 64;
        assert(0 <= g * gr) by (nonlinear_arith)
            requires 0 <= g, 0 <= gr;
        assert(n * gr == g * gr + gr) by (nonlinear_arith)
            requires g == n - 1;
        assert(g <= g * gr) by (nonlinear_arith)
            requires 0 <= g, 1 <= gr;
        assert((g as usize) as int == g);
        lemma_range_at(lg, group, nslabs, g as usize);
        let lo = group_range_at(lg, group, nslabs, g as usize).0 as int;
        let hi = group_range_at(lg, group, nslabs, g as usize).1 as int;
        let tracked mut perms = perms;
        let tracked mut marks = marks;
        let ghost m0 = marks@;
        assert(owner_marks(lo, hi).subset_of(marks@));
        let tracked m = marks.split(owner_marks(lo, hi));
        assert(owner_marks(0, lo).subset_of(marks@)) by {
            assert forall|x: int| #[trigger] owner_marks(0, lo).contains(x) implies marks@.contains(x) by {
                assert(m0.contains(x));
                assert(!owner_marks(lo, hi).contains(x));
            }
        }
        let tracked e = m.empty().persist();
        assert(perms.contains_key(g));
        let tracked p = perms.tracked_remove(g);
        let k = OwnerKey { ptr: owner_ptr_at(base + 2 * g), class, lo, hi, sloc: marks.id() };
        let tracked inv = OwnerInv::new(k, (p, m, e, None), owner_ns());
        if g > 0 {
            assert((g - 1) * gr < nslabs) by (nonlinear_arith)
                requires g * gr < nslabs, gr >= 0, g >= 1;
        }
        assert(lo == g * gr && g * gr <= nslabs);
        assert((if (g as nat) * gr <= nslabs { (g as nat) * gr } else { nslabs as int }) == lo);
        let tracked mut r = fresh_owner_invs(perms, marks, base, class, lg, group, nslabs, g as nat);
        let ghost r0 = r;
        r.tracked_insert(g, inv);
        assert forall|j: int| #![trigger r.contains_key(j)] #![trigger r[j]] 0 <= j < n implies r.contains_key(j) && owner_inv_at(r[j], base, class, lg, group, nslabs, marks.id(), j) by {
            if j < g {
                assert(r0.contains_key(j));
            }
        }
        r
    }
}

/// A length rounded up to a whole word covers it.
proof fn lemma_round_up8(y: int)
    requires
        y >= 0,
    ensures
        y <= (y + 7) / 8 * 8,
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(y + 7, 8);
}

} // verus!

verus! {

impl SlabTable {
    #[inline]
    pub fn class(&self) -> (r: &'static Class)
        ensures
            *r == class_table()[self.spec_class() as int],
            r.wf(),
    {
        proof {
            use_type_invariant(self);
        }
        &CLASSES.table()[self.class as usize]
    }

    #[inline]
    #[cfg(test)]
    pub fn nslabs(&self) -> usize {
        self.nslabs
    }

    /// The commit lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> (r: &RawLock)
        ensures
            *r == self.spec_raw_lock(),
            r.rank() == rank_table(self.spec_class() as int),
    {
        proof {
            use_type_invariant(self);
        }
        self.inner.raw()
    }

    /// The lock [`Self::raw_lock`] returns.
    pub closed spec fn spec_raw_lock(&self) -> RawLock {
        self.inner.0.spec_raw()
    }

    /// Whether `slab` is reserved as a guard slab. Guards are counted in
    /// page groups (see [`slabs_per_page`]): every `guard_interval`-th group
    /// is a guard group, and with one slab per page (the usual case) that is
    /// every `guard_interval`-th slab. Arithmetic on the table line (a
    /// shift, an add and a mask: the interval is a power of two; a
    /// runtime `%` would be a 32-bit `divl` twice per Valkey SET on the
    /// `sdallocx` path, the largest single term of the sized path's
    /// instruction window on small-L3 hosts), so
    /// the sized fast path can reject a slot start in a guard slab without
    /// a memory access: a guard slab's body past its first page is
    /// read-write memory that was never handed out. With guards off
    /// (`guard_mask == u32::MAX`) only page group `u32::MAX` would match,
    /// which no region has (at most 2^22 slabs), so there is no separate
    /// "off" branch.
    ///
    /// [`slabs_per_page`]: SlabTable::slabs_per_page
    #[inline(always)]
    pub fn is_guard_slab(&self, slab: usize) -> (r: bool)
        ensures
            r == self.spec_is_guard(slab),
    {
        proof {
            use_type_invariant(self);
            reveal(crate::layout::guard_of);
        }
        let page_group = (slab >> self.lg_slabs_per_page) as u32;
        page_group.wrapping_add(1) & self.guard_mask == 0
    }

    /// Tests: the guard interval in page groups (0 when guards are off).
    #[cfg(test)]
    pub fn guard_interval(&self) -> u32 {
        self.guard_mask.wrapping_add(1)
    }

    /// `guard_interval - 1` (all ones when guards are off): the mask of
    /// [`is_guard_slab`], for the dense copy the sized fast path reads
    /// (the guard word of `heap::ClassLine`).
    ///
    /// [`is_guard_slab`]: SlabTable::is_guard_slab
    #[inline]
    pub fn guard_mask(&self) -> (r: u32)
        ensures
            r == crate::layout::guard_geom(self.spec_class() as int).1,
    {
        proof {
            use_type_invariant(self);
        }
        self.guard_mask
    }
}

} // verus!

verus! {

impl SlabTable {
    /// A watermark copy for this table's class line (`heap::ClassLine`):
    /// a new atomic at 0 and its invariant ([`CommittedPred`]), keyed by
    /// the atomic's identity, this table's slab count and the location of
    /// a ghost variable at 0, whose other half (`auth`) goes to
    /// [`mirror_watermark`](SlabTable::mirror_watermark), which keeps the
    /// copy up. The invariant is frozen, so the line holds it as long as
    /// the table does.
    pub(crate) fn new_mirror(&self) -> (r: (AtomicUsize, Tracked<&'static CommittedInv>, Tracked<GhostVarAuth<usize>>))
        ensures
            r.1@.constant() == (usize_id(&r.0), self.spec_nslabs(), r.2@.id()),
            r.1@.namespace() == committed_ns(),
    {
        let (a, Tracked(perm)) = new_usize(0);
        let tracked (auth, var) = GhostVarAuth::<usize>::new(0);
        let tracked inv = CommittedInv::new((usize_id(&a), self.spec_nslabs(), auth.id()), (perm, var), committed_ns());
        (a, Tracked(vstd::modes::tracked_static_ref(inv)), Tracked(auth))
    }

    /// Keep `*mirror` equal to the hand-out watermark from now on: it is
    /// set to the current value here and stored after the watermark at
    /// every hand-out, under the commit lock, so a reader of the copy never
    /// sees a value the watermark has not held (`heap::ClassLine`).
    /// `inv` and `auth` are the copy's invariant and the other half of its
    /// ghost variable ([`SlabTable::new_mirror`]). This stores
    /// `next_slab`, which the watermark equals while the lock is held
    /// ([`WatermarkPred`]), inside `inv`, sets both halves of the variable
    /// to it, and keeps `inv` and `auth` in the commit lock's value;
    /// [`take_group`] updates both halves when it stores the copy, so the
    /// copy is `next_slab` whenever the lock is free. `ord` is the caller's
    /// lock-order token for the commit lock.
    ///
    /// # Safety
    /// `mirror` must stay valid for as long as this table is used.
    ///
    /// [`take_group`]: SlabTable::take_group
    pub unsafe fn mirror_watermark(
        &self,
        mirror: *const AtomicUsize,
        Tracked(inv): Tracked<&'static CommittedInv>,
        Tracked(auth): Tracked<GhostVarAuth<usize>>,
        Tracked(ord): Tracked<&mut LockOrder>,
    )
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_table(0)),
            mirror@.addr != 0,
            inv.constant() == (usize_id_at(mirror), self.spec_nslabs(), auth.id()),
            inv.namespace() == committed_ns(),
        ensures
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let tracked mut under = LockOrder::spare();
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut under));
        let gi = &mut *g;
        // While the lock is held the watermark is `next_slab` (its
        // invariant holds the other half of the ghost variable that ties
        // the two), so at most the slab count.
        let wm = gi.next_slab;
        vstd::open_atomic_invariant!(self.committed_inv.borrow() => v => {
            proof {
                gi.wm.borrow().agree(&v.2);
            }
        });
        let tracked mut auth = auth;
        vstd::open_atomic_invariant!(inv => v => {
            // SAFETY: guaranteed by the caller.
            unsafe { store_usize_at(mirror, wm, Ordering::Release, Tracked(&mut v.0)) };
            proof {
                // The copy becomes `next_slab` with both halves of its
                // ghost variable.
                auth.update(&mut v.1, wm);
            }
        });
        gi.mirror = mirror;
        proof {
            gi.mirror_inv = Tracked(Some(inv));
            gi.mauth = Tracked(Some(auth));
            assert(gi.inv());
        }
        g.unlock();
    }
}

} // verus!

verus! {

impl SlabTable {
    /// Slabs per page group; 1 unless the page is larger than the slab. A
    /// power of two that divides the slab count, so page groups tile the
    /// region.
    #[inline]
    pub fn slabs_per_page(&self) -> (r: usize)
        ensures
            r >= 1,
            r == self.spec_slabs_per_page(),
            r & sub(r, 1) == 0,
            self.spec_nslabs() & sub(r, 1) == 0,
            r == 1usize << crate::layout::guard_geom(self.spec_class() as int).0,
            crate::layout::guard_geom(self.spec_class() as int).0 <= 26,
    {
        proof {
            use_type_invariant(self);
            reveal(SlabTable::page_groups_ok);
            let (p, l) = (self.slabs_per_page as usize, self.lg_slabs_per_page);
            assert(p == 1usize << l && l < 64 ==> p & sub(p, 1) == 0) by (bit_vector);
        }
        self.slabs_per_page as usize
    }

    /// Start of the class bitmap (for `heap::ClassLine`), and the facts
    /// the line keeps of the table: its slab count is its class region's,
    /// at most `2^26`.
    #[inline]
    pub fn bits_base(&self) -> (r: usize)
        ensures
            r == self.spec_bits_base(),
            self.spec_nslabs() as int == process_regions().spec_nslabs(self.spec_class() as int),
            self.spec_nslabs() <= 0x400_0000,
    {
        proof {
            use_type_invariant(self);
        }
        self.bits_base
    }

    /// Start of the group owner array (for `heap::ClassLine`).
    #[inline]
    pub fn owners_base(&self) -> (r: usize)
        ensures
            r == self.spec_owners_base(),
    {
        self.owners_base
    }

    /// `log2` of the hand-out group (for `heap::ClassLine`).
    #[inline]
    pub fn lg_group(&self) -> (r: u8)
        ensures
            r == self.spec_lg_group(),
    {
        self.lg_group
    }
}

} // verus!

impl SlabTable {
    /// Tests: slabs handed to an arena at a time (see the field).
    #[cfg(test)]
    pub fn group(&self) -> usize {
        self.group as usize
    }

    /// Tests: write `arena` into the owner word of `group` (committing its
    /// page first), a wild write into the owner array for `take_group`'s
    /// cross-check to find.
    #[cfg(test)]
    pub(crate) fn corrupt_owner_word(&self, meta: &Meta, group: usize, arena: u16) {
        let page = 1usize << self.lg_page;
        let stagger = desc_stagger(self.class as usize);
        let mut g = self.inner.lock_t();
        g.owners
            .ensure(meta, page, stagger + group * 2 + 2, Tracked::assume_new())
            .expect("owner page");
        drop(g);
        // SAFETY: the word's page was just committed.
        unsafe {
            (*((self.owners_base + group * 2) as *const AtomicU16)).store(arena, Ordering::Relaxed)
        };
    }
}

verus! {

impl SlabTable {
    /// Number of slabs (including guard slabs) handed to an arena: their
    /// descriptors, bits and owner words exist (see the field). An
    /// `Acquire` load of the watermark, read inside its invariant
    /// ([`WatermarkPred`]).
    #[inline]
    pub fn committed_slabs(&self) -> (r: usize)
        ensures
            r <= self.spec_nslabs(),
    {
        proof {
            use_type_invariant(self);
        }
        load_usize_seen(&self.committed_slabs, Ordering::Acquire, &self.committed_inv).0
    }

    /// `committed_slabs.load(order)`: the hand-out watermark, as the
    /// queries and the `free` fast path load it (`Relaxed`; see
    /// [`desc_if_committed`]), through its permission, inside its
    /// invariant ([`WatermarkPred`]): at most the slab count. It takes only
    /// the value, not the invariant's set of witness markers, which only an
    /// `Acquire` load may hand out ([`committed_seen`]).
    ///
    /// [`committed_seen`]: SlabTable::committed_seen
    /// [`desc_if_committed`]: SlabTable::desc_if_committed
    #[inline(always)]
    fn watermark(&self, order: Ordering) -> (r: usize)
        requires
            // `core`'s load panics for these two.
            !(order is Release),
            !(order is AcqRel),
        ensures
            r <= self.spec_nslabs(),
        opens_invariants [committed_ns()]
    {
        proof {
            use_type_invariant(self);
        }
        let r;
        vstd::open_atomic_invariant!(self.committed_inv.borrow() => v => {
            r = load_usize(&self.committed_slabs, order, Tracked(&v.0));
        });
        r
    }

    /// Guard slabs committed so far. `ord` is the caller's lock-order
    /// token for the commit lock.
    pub fn guards(&self, Tracked(ord): Tracked<&mut LockOrder>) -> usize
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_table(0)),
        ensures
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        let r = g.guards;
        g.unlock();
        r
    }
}

impl SlabTable {
    /// Descriptor of `slab` if it has been handed out, `None` otherwise:
    /// no abort, for queries.
    ///
    /// The watermark is loaded `Relaxed`. For a pointer that really is
    /// an allocated slot the program itself ordered the free after the
    /// allocation (the same thread, or a hand-off through its own
    /// synchronisation), and the allocation was claimed under the class lock
    /// after the group was handed out and initialised, so that chain of
    /// happens-before already makes the watermark, the descriptor, the bits
    /// and the owner word visible here; the acquire added nothing. For any
    /// other pointer the only stale values this load can observe are a
    /// smaller watermark (`None`, and the slow path repeats the check with
    /// acquire) or the tables' initial zero words (a clear bit); it can
    /// never see a watermark above what has been handed out, so the pages
    /// are always mapped. [`is_set_unchecked`] and [`owner_at`]
    /// rely on the same argument.
    ///
    /// [`is_set_unchecked`]: SlabTable::is_set_unchecked
    /// [`owner_at`]: SlabTable::owner_at
    #[inline(always)]
    pub fn desc_if_committed(&self, slab: usize) -> Option<&SlabDesc> {
        if slab >= self.watermark(Ordering::Relaxed) {
            return None;
        }
        // SAFETY: `slab` is below a value the watermark actually held.
        Some(unsafe { self.desc_unchecked(slab) })
    }
}

impl SlabTable {
    /// Descriptor of a handed-out slab. Aborts for a slab that was never
    /// handed out rather than faulting on its `PROT_NONE` descriptor page.
    #[inline]
    pub fn desc(&self, slab: usize) -> (r: &SlabDesc)
        requires
            crate::sys::may_abort_pending(),
        ensures
            *r == desc_at(*self, slab),
    {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        // SAFETY: `slab` is below the acquire-loaded watermark.
        unsafe { self.desc_unchecked(slab) }
    }

    /// Descriptor of `slab` without a watermark check, through its
    /// permission ([`SlabTable::perms_ok`]).
    ///
    /// # Safety
    /// `slab` must be below a value `committed_slabs` has held: descriptors
    /// up to that count are committed and initialised (or zero, for guard
    /// slabs) before the watermark moves.
    #[inline(always)]
    unsafe fn desc_unchecked(&self, slab: usize) -> (r: &SlabDesc)
        requires
            slab < self.spec_nslabs(),
        ensures
            *r == desc_at(*self, slab),
    {
        proof {
            use_type_invariant(self);
            let d = self.desc_stride as int;
            assert(slab * d <= self.nslabs * d) by (nonlinear_arith)
                requires slab < self.nslabs, d >= 0;
            assert(self.desc_perms@.contains_key(slab as int));
        }
        let p = addr_to_ptr(self.desc_base + slab * self.desc_stride as usize) as *const SlabDesc;
        ptr_ref(p, Tracked(self.desc_perms.borrow().tracked_borrow(slab as int)))
    }

    /// The slot bits of a handed-out slab (design 4). Aborts for a
    /// slab that was never handed out.
    #[inline]
    pub fn bits(&self, slab: usize) -> (r: AtomicBitmap<'_>)
        requires
            crate::sys::may_abort_pending(),
        ensures
            bits_shape(r, class_table()[self.spec_class() as int].slab_bits as usize),
            bits_ghost(r, *self, slab),
    {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        // SAFETY: `slab` is below the acquire-loaded watermark.
        unsafe { self.bits_unchecked(slab) }
    }
}

impl SlabTable {
    /// Whether `slab` has been handed out, by a relaxed load of the
    /// watermark (see [`desc_if_committed`] for why that is enough on the
    /// `free` fast path). A handed-out slab is one of the region's: the
    /// watermark is at most the slab count (its invariant,
    /// [`WatermarkPred`]).
    ///
    /// [`desc_if_committed`]: SlabTable::desc_if_committed
    #[inline(always)]
    pub fn is_handed_out(&self, slab: usize) -> (r: bool)
        ensures
            r ==> slab < self.spec_nslabs(),
        opens_invariants [committed_ns()]
    {
        slab < self.watermark(Ordering::Relaxed)
    }

    /// The `free` check's bit (design 4): whether `slot` of `slab`
    /// is marked allocated (or cached). One relaxed load of the bitmap
    /// word, through its permission, no bounds checks. A set bit is not a
    /// guard slab's, and with the slot's token (`held`, [`holds_slot`]: a
    /// live slot's) the bit is set.
    ///
    /// # Safety
    /// `slab` must have been handed out ([`is_handed_out`]): its bitmap
    /// word is committed. The precondition states only the bounds (`slab`
    /// below the slab count, `slot` below the class's `nslots`).
    ///
    /// [`is_handed_out`]: SlabTable::is_handed_out
    #[inline(always)]
    pub unsafe fn is_set_unchecked(&self, slab: usize, slot: usize, Tracked(held): Tracked<&Option<GhostSingleton<int>>>) -> (r: bool)
        requires
            slab < self.spec_nslabs(),
            slot < class_table()[self.spec_class() as int].nslots,
        ensures
            r ==> !crate::layout::guard_slab(self.spec_class() as int, slab as int),
            holds_slot(*held, self.spec_class() as int, slab as int, slot as int) ==> r,
        opens_invariants [crate::bitmap::word_ns()]
    {
        proof {
            use_type_invariant(self);
            self.lemma_bits_fit(slab);
            self.lemma_slots_wf();
        }
        let bit = slab * self.slab_bits as usize + slot;
        let index = word_index(bit / 64);
        // SAFETY: the caller guarantees the word is inside the handed-out
        // part of the bitmap.
        let x = unsafe { self.bit_word_held(self.bits_base, index, Ghost(slab), Ghost(slot), Ghost(bit), Tracked(held)) };
        proof {
            let k = (bit % 64) as u64;
            assert((x >> k) & 1u64 != 0u64 ==> (x >> k) & 1u64 == 1u64) by (bit_vector);
        }
        (x >> (bit % 64)) & 1 == 1
    }

    /// A relaxed load of word `index` of the class bitmap that starts at
    /// `bits_base` (this table's), through its permission, inside its
    /// invariant: the word [`is_set_unchecked`] tests, which holds bit
    /// `bit`, slot `slot` of `slab`. A guard slab's bit is clear in every
    /// value the invariant admits (`bitmap::guards_ok`), and with the
    /// slot's token (`held`, [`holds_slot`]) the bit is set: the word's
    /// invariant holds the tokens of exactly its clear bits
    /// (`bitmap::WordPred`), and tokens are unique.
    ///
    /// # Safety
    /// The word is inside the handed-out part of the bitmap (committed).
    ///
    /// [`is_set_unchecked`]: SlabTable::is_set_unchecked
    // `slab`, `slot`, `bit` and `held` are read only by the proof (ghost).
    #[allow(unused_variables)]
    #[inline(always)]
    unsafe fn bit_word_held(&self, bits_base: usize, index: usize, Ghost(slab): Ghost<usize>, Ghost(slot): Ghost<usize>, Ghost(bit): Ghost<usize>, Tracked(held): Tracked<&Option<GhostSingleton<int>>>) -> (x: u64)
        requires
            bits_base == self.bits_base,
            slab < self.spec_nslabs(),
            slot < class_table()[self.spec_class() as int].nslots,
            bit == slab * class_table()[self.spec_class() as int].slab_bits + slot,
            index == bit / 64,
        ensures
            (x >> (bit % 64)) & 1 != 0 ==> !crate::layout::guard_slab(self.spec_class() as int, slab as int),
            holds_slot(*held, self.spec_class() as int, slab as int, slot as int) ==> (x >> (bit % 64)) & 1 != 0,
        opens_invariants [crate::bitmap::word_ns()]
    {
        proof {
            use_type_invariant(self);
            self.lemma_bits_fit(slab);
            self.lemma_slots_wf();
            reveal(words_perms_ok);
        }
        let ghost j = bit as int / 64;
        proof {
            assert(0 <= bit < self.nslabs * (self.slab_bits as int));
            assert(j < self.spec_nwords());
            assert(self.word_perms@.contains_key(j));
            assert(self.word_invs@.contains_key(j));
        }
        let word = addr_to_ptr(bits_base + index * 8) as *const AtomicU64;
        let w = ptr_ref(word, Tracked((*self.word_perms.borrow()).tracked_borrow(j)));
        let tracked inv = (*self.word_invs.borrow()).tracked_borrow(j);
        let x;
        vstd::open_atomic_invariant!(inv => pair => {
            let tracked (perm, mut g, u, d, sn) = pair;
            x = crate::vspec::word::load(w, Ordering::Relaxed, Tracked(&perm));
            proof {
                self.lemma_guard_slab(slab as int);
                let s = self.spec_slots();
                let (v, k) = (x, (bit - 64 * j) as u64);
                assert((bit - 64 * j) as u64 == (bit % 64) as u64);
                assert((v >> k) & 1u64 != 1u64 ==> (v >> k) & 1u64 == 0u64) by (bit_vector);
                if crate::layout::guard_slab(self.spec_class() as int, slab as int) {
                    crate::bitmap::lemma_slab_bit(s, slab as int, slot as int);
                    crate::bitmap::lemma_guard_clear(inv.constant(), (perm, g, u, d, sn), bit as int);
                }
                if holds_slot(*held, self.spec_class() as int, slab as int, slot as int) {
                    // The slot's token is not in the word, which holds the
                    // tokens of its clear bits: the bit is set.
                    let tracked t = held.tracked_borrow();
                    g.disjoint_singleton(t);
                    self.lemma_slot_bit(slab as int, slot as int);
                    broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter, Set::lemma_map_contains};
                    if !crate::bitmap::has_bit(v, k) {
                        assert(crate::bitmap::free_bits(s, j, v).contains(bit as int));
                        assert(g@.contains(s.addr(bit as int)));
                    }
                }
                pair = (perm, g, u, d, sn);
            }
        });
        x
    }

    /// The owner-word witness marker of `slab` covers the view of the
    /// slab's bits ([`SlabTable::bits`]): each of its bits is the slab's
    /// (`bitmap::owner_seen`).
    pub(super) proof fn lemma_seen_view<'a>(&self, bm: AtomicBitmap<'a>, slab: usize, w: GhostPersistentSubset<int>)
        requires
            self.inv(),
            slab < self.spec_nslabs(),
            bits_shape(bm, class_table()[self.spec_class() as int].slab_bits as usize),
            bits_ghost(bm, *self, slab),
            w.id() == self.spec_sloc(),
            w@.contains(owner_mark(slab as int)),
        ensures
            bm.seen_by(w),
    {
        self.lemma_slots_wf();
        let sb = self.spec_slots().sb;
        assert forall|b: int| bm.spec_first() <= b < bm.spec_first() + bm.spec_nbits()
            implies #[trigger] crate::bitmap::owner_seen(bm.spec_slots(), w@, b) by {
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(b, sb, slab as int, b - slab * sb);
        }
    }

    /// The witness that slot `slot` of `slab` was handed out, for a reader
    /// that holds the slot's token (`held`, [`holds_slot`]): the persistent
    /// owner-word witness markers of the bit's word's invariant, which has
    /// the marker of the slab of every set slot bit
    /// (`bitmap::owners_ok`), frozen. The token is not in the word, which
    /// holds the tokens of its clear bits, so the bit is set. No code: the
    /// invariant is opened for the proof alone.
    proof fn lemma_held_seen(
        tracked &self,
        tracked held: &Option<GhostSingleton<int>>,
        tracked credit: vstd::invariant::OpenInvariantCredit,
        slab: usize,
        slot: usize,
    ) -> (tracked r: Option<&'static GhostPersistentSubset<int>>)
        requires
            self.inv(),
            slab < self.spec_nslabs(),
            slot < class_table()[self.spec_class() as int].nslots,
        ensures
            holds_slot(*held, self.spec_class() as int, slab as int, slot as int) ==> seen_owner(*self, slab, r),
        opens_invariants [crate::bitmap::word_ns()]
    {
        self.lemma_bits_fit(slab);
        self.lemma_slots_wf();
        let s = self.spec_slots();
        let bit = slab * s.sb + slot;
        let j = bit / 64;
        assert(0 <= bit < self.nslabs * (self.slab_bits as int));
        assert(j < self.spec_nwords());
        assert(self.word_invs@.contains_key(j));
        let tracked inv = (*self.word_invs.borrow()).tracked_borrow(j);
        let tracked mut r = None;
        vstd::open_atomic_invariant_in_proof!(credit => inv => pair => {
            let tracked (perm, mut g, u, d, sn) = pair;
            if holds_slot(*held, self.spec_class() as int, slab as int, slot as int) {
                let tracked t = held.tracked_borrow();
                g.disjoint_singleton(t);
                self.lemma_slot_bit(slab as int, slot as int);
                crate::bitmap::lemma_slab_bit(s, slab as int, slot as int);
                broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter, Set::lemma_map_contains};
                let (v, k) = (perm.value(), (bit - 64 * j) as u64);
                if !crate::bitmap::has_bit(v, k) {
                    assert(crate::bitmap::free_bits(s, j, v).contains(bit));
                    assert(g@.contains(s.addr(bit)));
                }
                assert(crate::bitmap::owner_seen(s, sn@, bit));
                r = Some(vstd::modes::tracked_static_ref(sn.duplicate()));
            }
            pair = (perm, g, u, d, sn);
        });
        r
    }

    /// The arena that owns `slab`'s group, without a watermark check:
    /// one relaxed load of its owner word, through its permission, inside
    /// its invariant ([`load_owner`]). With `seen`, the witness that the
    /// group was handed out (slab `slab`'s persistent witness marker,
    /// which a load of the watermark past `slab` returns:
    /// [`committed_seen`]), it is the slab's owner ([`slab_owner`]).
    ///
    /// # Safety
    /// `slab` must have been handed out ([`is_handed_out`]): the owner
    /// word of a handed-out group was committed and written before the
    /// watermark moved (the premise of `Unbacked::into_lazy`).
    ///
    /// [`is_handed_out`]: SlabTable::is_handed_out
    /// [`committed_seen`]: SlabTable::committed_seen
    #[inline(always)]
    unsafe fn owner_at(&self, slab: usize, seen: Tracked<Option<&'static GhostPersistentSubset<int>>>) -> (r: u16)
        requires
            slab < self.spec_nslabs(),
        ensures
            seen_owner(*self, slab, seen@) ==> r == slab_owner(self.spec_class() as int, slab as int),
        opens_invariants [owner_ns()]
    {
        proof {
            use_type_invariant(self);
            self.lemma_group_in_range(slab);
        }
        // `owners_base` before the shift: in this order the `free` fast
        // path's owner test schedules its loads as the plain code does.
        let word = addr_to_ptr(self.owners_base + (slab >> self.lg_group) * 2) as *const AtomicU16;
        // SAFETY: the owner word of a handed-out group was committed and
        // written before the watermark moved.
        unsafe { load_owner(self, word, Ghost(slab), seen) }
    }

    /// The watermark, an `Acquire` load, as [`committed_slabs`] loads it,
    /// with the witness the watermark's invariant holds with it: the
    /// persistent witness markers of every slab below it, whose owner
    /// words are stored ([`WatermarkPred`], [`load_owner`]).
    ///
    /// [`committed_slabs`]: SlabTable::committed_slabs
    #[inline(always)]
    fn committed_seen(&self) -> (r: (usize, Tracked<&'static GhostPersistentSubset<int>>))
        ensures
            r.0 <= self.spec_nslabs(),
            r.1@.id() == self.spec_sloc(),
            owner_marks(0, r.0 as int).subset_of(r.1@@),
        opens_invariants [committed_ns()]
    {
        proof {
            use_type_invariant(self);
        }
        load_usize_seen(&self.committed_slabs, Ordering::Acquire, &self.committed_inv)
    }

    /// The arena that owns `slab` ([`slab_owner`]), with the witness that
    /// the group was handed out that the watermark's load returned, for
    /// [`SlabTable::owned_by`]. Aborts for a slab that was never handed
    /// out.
    #[inline]
    pub fn owner(&self, slab: usize) -> (r: (u16, Tracked<&'static GhostPersistentSubset<int>>))
        requires
            crate::sys::may_abort_pending(),
        ensures
            slab < self.spec_nslabs(),
            r.0 == slab_owner(self.spec_class() as int, slab as int),
            r.1@.id() == self.spec_sloc(),
            r.1@@.contains(owner_mark(slab as int)),
    {
        let (committed, seen) = self.committed_seen();
        if slab >= committed {
            never_committed(self.class as usize, slab);
        }
        proof {
            broadcast use vstd::set_lib::range_set_properties;
            assert(owner_marks(0, committed as int).contains(owner_mark(slab as int)));
        }
        let tracked w = Some(seen.get());
        // SAFETY: `slab` is below the acquire-loaded watermark.
        (unsafe { self.owner_at(slab, Tracked(w)) }, seen)
    }

    /// What the owner word of handed-out slab `slab`'s group records of
    /// its taker ([`OwnerPred`]), for a reader holding the registry's
    /// record `pubw` of an arena: if the slab's owner is that arena, the
    /// taker was that arena's class state (the two records of one index
    /// agree, `GhostPersistentPointsTo::intersection_agrees`), so `g`, a
    /// class lock value whose owned-slab record is that state's, has the
    /// slab in it. `seen` is the witness that the group was handed out
    /// ([`SlabTable::owner`] returns it). No code: the invariant is opened
    /// for the proof alone.
    // `g`, `seen` and `pubw` are read only by the proof (ghost).
    #[allow(unused_variables)]
    #[inline(always)]
    pub(super) fn owned_by(&self, slab: usize, seen: Tracked<&GhostPersistentSubset<int>>, g: &Inner, pubw: Tracked<&'static Published>)
        requires
            slab < self.spec_nslabs(),
            seen@.id() == self.spec_sloc(),
            seen@@.contains(owner_mark(slab as int)),
            pubw@.id() == process_pub_loc(),
            g.oauth@.id() == pubw@.value()[self.spec_class() as int],
        ensures
            slab_owner(self.spec_class() as int, slab as int) as int == pubw@.key() ==> g.oauth@@.contains(slab as int),
        opens_invariants [owner_ns()]
    {
        proof {
            use_type_invariant(self);
            self.lemma_group_in_range(slab);
            self.lemma_slab_in_group(slab);
        }
        let ghost gr = (slab >> self.lg_group) as int;
        let ghost (lo, hi) = (self.spec_group_range(gr as usize).0 as int, self.spec_group_range(gr as usize).1 as int);
        proof {
            assert(self.owner_invs@.contains_key(gr));
            broadcast use vstd::set_lib::range_set_properties;
            assert(owner_marks(lo, hi).contains(owner_mark(slab as int)));
        }
        vstd::open_atomic_invariant!(self.owner_invs.borrow().tracked_borrow(gr) => v => {
            proof {
                let k = self.owner_invs@[gr].constant();
                assert(k.lo == lo && k.hi == hi && k.class == self.spec_class() as int);
                // Stored, as in `load_owner`.
                if owner_marks(lo, hi).subset_of(v.1@) {
                    v.1.disjoint_persistent(seen.get());
                    assert(v.1@.contains(owner_mark(slab as int)));
                }
                let value = u16_value(v.0.value());
                assert(owner_stored(k, value, v.2@, v.3));
                assert(slab_owner(k.class, slab as int) == value);
                let tracked t = v.3.tracked_borrow();
                assert(taken_by(k, value, *t));
                if value as int == pubw@.key() {
                    let tracked mut w = t.0.duplicate();
                    w.intersection_agrees(pubw.get());
                    t.1.agree(g.oauth.borrow());
                    assert(t.1@.contains(slab as int));
                }
            }
        });
    }

    /// Slab `slab` is one of its hand-out group's, `slab >> lg_group`
    /// ([`spec_group_range`](SlabTable::spec_group_range)).
    proof fn lemma_slab_in_group(&self, slab: usize)
        requires
            self.inv(),
            slab < self.nslabs,
        ensures
            self.spec_group_range(slab >> self.lg_group).0 <= slab < self.spec_group_range(slab >> self.lg_group).1,
    {
        let (lg, gr, n) = (self.lg_group, self.group as usize, self.nslabs);
        let g = slab >> lg;
        assert((g << lg) <= slab && slab < (g << lg) + gr) by (bit_vector)
            requires lg < 64, gr == 1usize << lg, g == slab >> lg, slab < 0x400_0000;
    }

    /// Whether `slot` of `slab` is marked allocated (or cached); `false`
    /// for a slab that was never handed out. Never aborts.
    #[inline]
    pub fn is_allocated(&self, slab: usize, slot: usize) -> bool
        requires
            slot < class_table()[self.spec_class() as int].nslots,
    {
        debug_assert!(slot < self.class().nslots());
        let tracked none = None;
        // SAFETY: the watermark check, then a slot below `nslots`.
        self.is_handed_out(slab) && unsafe { self.is_set_unchecked(slab, slot, Tracked(&none)) }
    }
}

impl SlabTable {
    #[inline]
    pub(super) fn slab_addr(&self, slab: usize) -> (r: usize)
        requires
            slab < self.spec_nslabs(),
        ensures
            r == self.base + slab * ((1usize << class_table()[self.spec_class() as int].lg_slab) as int),
            r + (1usize << class_table()[self.spec_class() as int].lg_slab)
                <= self.base + (self.spec_nslabs() << class_table()[self.spec_class() as int].lg_slab),
            r + (1usize << class_table()[self.spec_class() as int].lg_slab) <= 1usize << 57,
    {
        proof {
            use_type_invariant(self);
            let lg = class_table()[self.class as int].lg_slab as usize;
            let (m, m57) = ((usize::MAX - self.base) as usize, ((1usize << 57) - self.base) as usize);
            vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
            vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(self.nslabs, lg, m);
            vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(self.nslabs, lg, m57);
            let p = pow2(lg as nat) as int;
            assert(slab * p + p <= self.nslabs * p) by (nonlinear_arith)
                requires slab < self.nslabs, p >= 0;
            vstd::bits::lemma_usize_shl_is_mul(self.nslabs, lg);
            vstd::bits::lemma_usize_shl_is_mul(slab, lg);
            vstd::bits::lemma_usize_shl_is_mul(1, lg);
        }
        self.base + (slab << self.class().lg_slab)
    }

    /// The start of slot `slot` of slab `slab`: `slot_at` of the
    /// process's regions, a pointer of the address-cast provenance.
    #[inline]
    pub(super) fn slot_ptr(&self, slab: usize, slot: usize) -> (r: *mut u8)
        requires
            slab < self.spec_nslabs(),
            slot < class_table()[self.spec_class() as int].slab_bits,
        ensures
            r@.addr == process_regions().slot_at(self.spec_class() as int, slab as int, slot as int),
            r@.provenance == cast_provenance(),
    {
        proof {
            use_type_invariant(self);
            let cls = class_table()[self.class as int];
            assert(slot * (cls.size as int) <= 0x2000 * 0x4_0000) by (nonlinear_arith)
                requires
                    slot < cls.slab_bits,
                    cls.slab_bits <= 0x2000,
                    cls.size <= 0x4_0000,
            ;
            assert((1usize << 57u32) == 0x200_0000_0000_0000usize) by (bit_vector);
        }
        addr_to_ptr(self.slab_addr(slab) + slot * self.class().size())
    }
}

impl SlabTable {
    /// Slab and slot of a pointer that must be a slot start in this class's
    /// region. Aborts otherwise: never for a slot start (a guard slab's
    /// too).
    #[inline]
    pub fn locate(&self, p: *mut u8) -> (r: (usize, usize))
        requires
            crate::sys::may_abort() || (crate::heap::regions_ok() && process_regions().is_slot_addr(self.spec_class() as int, p@.addr)),
        ensures
            r.0 < self.spec_nslabs(),
            r.1 < class_table()[self.spec_class() as int].nslots,
            process_regions().slot_at(self.spec_class() as int, r.0 as int, r.1 as int) == p@.addr,
    {
        proof {
            use_type_invariant(self);
        }
        let cls = self.class();
        let rel = (p as usize).wrapping_sub(self.base);
        proof {
            if crate::heap::regions_ok() && process_regions().is_slot_addr(self.spec_class() as int, p@.addr) {
                // A slot start passes both checks below
                // (`Regions::lemma_slot_addr_found`): its offset is inside
                // the region, `nslabs` slabs, and a whole number of slots
                // into its slab.
                let c = self.spec_class() as int;
                process_regions().lemma_slot_addr_found(c, p as usize);
                let lg = cls.lg_slab;
                let lr = process_regions().lg_region;
                let (n, rl) = (self.nslabs, 1usize << lr);
                assert(rel < rl);
                assert(22 <= lr < 64);
                assert(n == rl >> lg);
                assert(n << lg == rl) by (bit_vector)
                    requires
                        n == rl >> lg,
                        rl == 1usize << lr,
                        lg <= 22,
                        22 <= lr < 64,
                ;
            }
        }
        if rel >= self.nslabs << cls.lg_slab {
            invalid_pointer(p as usize, self.class as usize, "outside the class region");
        }
        proof {
            let lg = cls.lg_slab;
            let n = self.nslabs;
            let m = (usize::MAX - self.base) as usize;
            assert(rel < n << lg && n <= m >> lg && lg <= 22 ==> rel >> lg < n) by (bit_vector);
            assert(rel & sub(1usize << lg, 1) < 0x40_0000) by (bit_vector)
                requires lg <= 22;
            assert(crate::layout::MAX_SLAB == 0x40_0000) by (bit_vector);
            let mk = cls.slab_mask as usize;
            assert(mk == ((1usize << lg) - 1) as usize);
            assert((rel & mk) < (1usize << lg)) by (bit_vector)
                requires lg <= 22, mk == ((1usize << lg) - 1) as usize;
        }
        let (slot, valid) = cls.slot_of_offset(rel & cls.slab_mask());
        if !valid {
            invalid_pointer(p as usize, self.class as usize, "not a slot start");
        }
        proof {
            // `p` is the slot's start: the offset splits into the slab's
            // start and a whole number of slots.
            let lg = cls.lg_slab;
            let lgu = lg as usize;
            let slab = rel >> lg;
            let mask = sub(1usize << lg, 1);
            let (n, b, a) = (self.nslabs, self.base, p as usize);
            let m = (usize::MAX - b) as usize;
            assert(rel == ((rel >> lg) << lg) + (rel & mask) && slab <= usize::MAX >> lgu) by (bit_vector)
                requires lg <= 22, lgu == lg as usize, slab == rel >> lg, mask == sub(1usize << lg, 1);
            assert(rel < n << lg && n <= m >> lg && lg <= 22 ==> rel <= m) by (bit_vector);
            vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
            vstd::bits::lemma_usize_shl_is_mul(1, lgu);
            vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(slab, lgu, usize::MAX);
            vstd::bits::lemma_usize_shl_is_mul(slab, lgu);
            assert((slab << lg) == (slab << lgu) && (1usize << lg) == (1usize << lgu)) by (bit_vector)
                requires lgu == lg as usize;
            assert(a as int == b + rel);
        }
        (rel >> cls.lg_slab, slot)
    }

    /// The `free` check of a pointer already located (`Regions::classify`):
    /// aborts unless `slab` has been handed out and `slot`'s bit is set,
    /// with the pointer and its location; returns the owning arena. `held`
    /// is the caller's slot token, if any: with the slot's (a live slot's,
    /// [`holds_slot`]) the bit test cannot fail, and the arena is the
    /// slab's owner. The owner word is read after a `Relaxed` watermark
    /// load, which hands out no witness that the group was handed out
    /// (only an `Acquire` load does, `vspec::atom::load_usize_seen`); the
    /// witness comes from the slot's bitmap word instead, whose invariant
    /// has it for every set slot bit ([`SlabTable::lemma_held_seen`]).
    /// Without the token the value returned is not specified: the release
    /// it routes to reads the owner again, with the witness
    /// ([`owner`](SlabTable::owner)).
    // `credit` is read only by the proof (ghost).
    #[allow(unused_variables)]
    #[inline]
    pub fn check_located(&self, slab: usize, slot: usize, p: usize, held: Tracked<&Option<GhostSingleton<int>>>) -> (r: u16)
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && holds_slot(*held@, self.spec_class() as int, slab as int, slot as int)),
            slot < class_table()[self.spec_class() as int].nslots,
        ensures
            slab < self.spec_nslabs(),
            !crate::layout::guard_slab(self.spec_class() as int, slab as int),
            holds_slot(*held@, self.spec_class() as int, slab as int, slot as int)
                ==> r == slab_owner(self.spec_class() as int, slab as int),
    {
        self.check_live(slab, slot, p, held);
        let credit = vstd::invariant::create_open_invariant_credit();
        proof {
            use_type_invariant(self);
        }
        let tracked seen = self.lemma_held_seen(held.get(), credit.get(), slab, slot);
        // SAFETY: `check_live` returned, so `slab` is below the watermark.
        unsafe { self.owner_at(slab, Tracked(seen)) }
    }

    /// The bit test of [`check_located`], without the owner: the exact
    /// check of `malloc_usable_size`, `realloc` and `rallocx` on a slot
    /// `Regions::classify` located. The loads that
    /// depend on the pointer are the `free` fast path's
    /// (`Shared::checked_slot`): the watermark, `bits_base` and
    /// `slab_bits` from this table line, then the class bitmap word; the
    /// slab and slot come from the caller's dense-array arithmetic and are
    /// not derived again from `base` and `CLASSES[class]` here (that would
    /// be a second dependent chain in front of the word load). The
    /// watermark is loaded `Relaxed` for the reason [`desc_if_committed`]
    /// gives: for a live slot the program's own ordering makes the group's
    /// words visible, and for anything else every observable value aborts.
    /// Aborts by name for a slab never handed out, a guard slab, or a
    /// clear bit; the guard test (`is_guard_slab`: a shift, an add and a
    /// mask) runs only once the bit
    /// is clear, which it always is in a guard slab (`take_group` never
    /// sets one), so the messages are unchanged and the hot path is the
    /// watermark compare and the bit test. With the slot's token
    /// (`held`) the bit is set, so neither of the last two can fire; the
    /// watermark compare is a pending check then (not proved unreachable
    /// yet).
    ///
    /// [`check_located`]: SlabTable::check_located
    /// [`desc_if_committed`]: SlabTable::desc_if_committed
    #[inline(always)]
    pub fn check_live(&self, slab: usize, slot: usize, p: usize, Tracked(held): Tracked<&Option<GhostSingleton<int>>>)
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && holds_slot(*held, self.spec_class() as int, slab as int, slot as int)),
            slot < class_table()[self.spec_class() as int].nslots,
        ensures
            slab < self.spec_nslabs(),
            // A guard slab's bits are clear: it aborts by name.
            !crate::layout::guard_slab(self.spec_class() as int, slab as int),
    {
        if !self.is_handed_out(slab) {
            never_committed(self.class as usize, slab);
        }
        debug_assert!(slot < self.class().nslots());
        // SAFETY: below a value the watermark held, slot below `nslots`.
        if !unsafe { self.is_set_unchecked(slab, slot, Tracked(held)) } {
            proof {
                // A live slot's bit is set: the caller holds no token of it.
                assert(!holds_slot(*held, self.spec_class() as int, slab as int, slot as int));
            }
            self.not_live(slab, slot, p);
        }
    }

    /// The abort of [`check_live`] for a clear bit: a guard slab by name,
    /// otherwise a slot that is not allocated.
    ///
    /// [`check_live`]: SlabTable::check_live
    #[cold]
    #[inline(never)]
    fn not_live(&self, slab: usize, slot: usize, p: usize) -> !
        requires
            crate::sys::may_abort(),
    {
        if self.is_guard_slab(slab) {
            guard_free(p, self.class as usize, slab);
        }
        invalid_free(p, self.class as usize, slab, slot)
    }
}

impl SlabTable {
    /// Hand the next group of slabs to `arena` (design 4):
    /// commit their descriptors, bitmap words and owner word, initialise
    /// the descriptors (`Fresh`, owned by `arena`; guard slabs stay zero)
    /// and clear their bits, then publish the watermark. The set words of
    /// `sets`, the value behind `arena`'s class lock (which the caller
    /// holds), are committed under the commit lock up to the group's last
    /// slab, in `sets_page` units (the caller's page, the table's), before
    /// anything changes ([`Inner::ensure_sets`]); if that
    /// fails nothing has happened. Returns the group's slab range, `None`
    /// when the region or the metadata region is exhausted. The slabs
    /// themselves are committed by the arena as its cursor reaches them, a
    /// run at a time ([`commit_run`]).
    ///
    /// The group is above every slab `sets` owns and its current group's
    /// end ([`next_slab_locked`]), which is how
    /// [`ClassState::next_fresh_slab`](super::ClassState::next_fresh_slab) adds its slabs to the ghost; the
    /// same marker argument holds for every other arena's lock value, but
    /// no proof here sees them. `init` takes each non-guard slab's init
    /// markers from the commit lock's value and returns its fresh markers,
    /// and a guard slab's init markers are taken as they are; both join
    /// `inited` for `next_fresh_slab` to keep with the slabs. So do the
    /// group's descriptor permissions, out of the commit lock's value into
    /// `dout`: each written by `init` (`Fresh`, `arena`'s), or as it is for
    /// a guard slab (zero).
    ///
    /// [`commit_run`]: SlabTable::commit_run
    /// [`next_slab_locked`]: SlabTable::next_slab_locked
    // Verus needs the explicit reborrow and a loop with an invariant.
    // `pubw` is read only by the proof (ghost).
    #[allow(clippy::explicit_auto_deref, clippy::while_let_loop, clippy::too_many_arguments, unused_variables)]
    pub(super) fn take_group(
        &self,
        meta: &Meta,
        arena: u16,
        sets: &mut Inner,
        sets_page: usize,
        Tracked(inited): Tracked<&mut GhostPersistentSubset<int>>,
        Tracked(dout): Tracked<&mut Map<nat, DescPerm>>,
        pubw: Tracked<&'static Published>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: Option<(usize, usize)>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_table(0)),
            meta.is_process(),
            old(sets).table@ == *self,
            old(sets).arena@ == arena,
            old(sets).base(),
            old(sets).nbits() == self.spec_nslabs(),
            old(inited).id() == self.spec_slots().mloc,
            old(dout).dom().is_empty(),
            // The taker is the published state of this class and arena
            // (`ClassState::published_by`), whose owned-slab record `sets`
            // has.
            pubw@.id() == process_pub_loc(),
            pubw@.key() == arena as int,
            old(sets).oauth@.id() == pubw@.value()[self.spec_class() as int],
        ensures
            final(sets).same_sets_but_rec(*old(sets)),
            final(sets).oauth@.id() == old(sets).oauth@.id(),
            r is None ==> final(sets).same_sets(*old(sets)),
            r matches Some((s, e)) ==> final(sets).oauth@@ == old(sets).oauth@@.union(Set::<int>::range(s as int, e as int)),
            // The group's owner-word witness markers join the value's.
            r matches Some((s, e)) ==> final(sets).seen@.id() == self.spec_sloc()
                && owner_marks(s as int, e as int).subset_of(final(sets).seen@@)
                && (old(sets).seen@.id() == self.spec_sloc() ==> old(sets).seen@@.subset_of(final(sets).seen@@)),
            *final(ord) == *old(ord),
            final(inited).id() == old(inited).id(),
            old(inited)@.subset_of(final(inited)@),
            r matches Some((s, e)) ==> s < e <= self.spec_nslabs()
                && self.spec_group_range(s >> self.lg_group) == (s, e)
                && (forall|x: nat| #[trigger] old(sets).owns(x) ==> x < s)
                && old(sets).group_end <= s
                && (forall|x: int| s <= x < e ==> #[trigger] slab_owner(self.spec_class() as int, x) == arena)
                && (forall|x: int| s <= x < e && !self.spec_is_guard(x as usize) ==> #[trigger] slab_marked(*self, x, final(inited)@))
                && (forall|x: int| s <= x < e && self.spec_is_guard(x as usize) ==> #[trigger] guard_marked(*self, x, final(inited)@))
                && (forall|x: nat| #[trigger] final(dout).contains_key(x) ==> s <= x < e)
                && (forall|x: nat| s <= x < e ==> #[trigger] desc_handed(*final(dout), *self, x, arena)),
    {
        proof {
            use_type_invariant(self);
            lemma_nslabs_bound(self.nslabs, (usize::MAX - self.base) as usize, class_table()[self.class as int].lg_slab as usize);
        }
        let cls = self.class();
        let page = 1usize << self.lg_page;
        let class = self.class as usize;
        let stagger = desc_stagger(class);
        let tracked mut under = LockOrder::spare();
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut under));
        let s = self.next_slab_locked(&*g, &*sets);
        if s >= self.nslabs {
            g.unlock();
            return None;
        }
        proof {
            let (n, lg) = (self.nslabs, self.lg_group);
            assert(s % g@.group@ == 0);
            assert(s < n && lg < 64 ==> s >> lg <= n >> lg) by (bit_vector);
            assert(s % (1usize << lg) == 0 && lg < 64 ==> s & sub(1usize << lg, 1) == 0 && (s >> lg) << lg == s) by (bit_vector);
        }
        watermark_aligned(s, self.lg_group);
        let (_, end) = self.group_range(s >> self.lg_group);
        if purge::check_zero() && s != self.committed_slabs.load(Ordering::Relaxed) {
            crate::sys::abort_fmt(format_args!("check_zero: class {class}: take_group at next_slab {s} but the watermark is {}", self.committed_slabs.load(Ordering::Relaxed)));
        }
        proof {
            assert(NCLASSES <= 64 && PAD_ALIGN == 128);
            assert(crate::layout::MAX_SLAB_WORDS * 64 <= 0x1_0000);
            assert(end <= 0x400_0000);
            assert(s < end);
            let (e, lg) = (end, self.lg_group);
            assert(e >= 1 ==> sub(e, 1) >> lg <= sub(e, 1)) by (bit_vector);
            assert forall|d: usize| d <= 0xffff_ffff implies #[trigger] (end as int * d as int) <= 0x400_0000 * 0xffff_ffff by {
                assert(end as int * d as int <= 0x400_0000 * 0xffff_ffff) by (nonlinear_arith)
                    requires end <= 0x400_0000, d <= 0xffff_ffff;
            }
        }
        if g.descs.ensure(meta, page, stagger + end * desc_stride(class), Tracked(&mut under)).is_err()
            || g.bits.ensure(meta, page, stagger + crate::vspec::div_ceil(end * cls.slab_bits(), 8), Tracked(&mut under)).is_err()
            || g.owners.ensure(meta, page, stagger + ((end - 1) >> self.lg_group) * 2 + 2, Tracked(&mut under)).is_err()
            || sets.ensure_sets(meta, sets_page, end - 1, Tracked(&mut under)).is_err()
        {
            g.unlock();
            return None;
        }
        // The owner word and the descriptors' arena field record the same
        // fact (design section 4) and are written together only here, so
        // a group about to be handed out must show them agreeing: both
        // zero for never-touched memory. A disagreement is a wild write
        // into one of the two arrays and aborts before the group is used.
        proof {
            self.lemma_group_in_range(s);
        }
        // SAFETY: the owner page for the group was committed above.
        let owner_word = unsafe { self.owner_word(s >> self.lg_group) };
        let ghost og = (s >> self.lg_group) as int;
        proof {
            use_type_invariant(self);
            assert(self.owner_invs@.contains_key(og));
        }
        let previous;
        vstd::open_atomic_invariant!(self.owner_invs.borrow().tracked_borrow(og) => v => {
            // SAFETY: the owner page for the group was committed above; the
            // invariant's permission is to the word.
            previous = unsafe { load_u16_at(owner_word, Ordering::Relaxed, Tracked(&v.0)) };
        });
        let ghost g0 = g@;
        // `for slab in s..end`, spelled out (both loops): Verus rejects
        // `continue` in a `for` loop. The `continue` keeps the plain loop's
        // code; in the second loop only with the body in the `Some` arm, as
        // the `for` desugaring has it (a `let slab = match ..` before the
        // body changed the code of `next_fresh_slab`, which inlines this).
        let mut slabs = s..end;
        loop
            invariant
                crate::sys::may_abort(),
                slabs.start <= slabs.end == end,
                end <= self.spec_nslabs(),
                self.lg_group < 64,
                class == self.class as usize,
                g@ == g0,
            decreases slabs.end - slabs.start,
        {
            let slab = match slabs.next() {
                Some(slab) => slab,
                None => break,
            };
            if self.is_guard_slab(slab) {
                continue;
            }
            proof {
                use_type_invariant(self);
                self.lemma_desc_at(slab);
            }
            // SAFETY: the descriptor page for `slab` was committed above.
            let recorded = unsafe { SlabDesc::at(self.desc_base, class, slab, Tracked(self.desc_perms.borrow().tracked_borrow(slab as int))).arena() };
            if recorded != previous {
                owner_mismatch(class, s >> self.lg_group, slab, previous, recorded);
            }
        }
        // The init markers of each slab `init` runs on come out of the
        // value's (the `&mut` is the guard's, as for every write below).
        let gi = &mut *g;
        let ghost sl = self.spec_slots();
        proof {
            self.lemma_slots_wf();
            assert(s * sl.sb <= sl.nbits) by (nonlinear_arith)
                requires s < self.nslabs, sl.nbits == self.nslabs * sl.sb, sl.sb > 0;
        }
        let mut slabs = s..end;
        loop
            invariant
                crate::sys::may_abort(),
                slabs.start <= slabs.end == end,
                s <= slabs.start,
                end <= self.spec_nslabs(),
                self.lg_group < 64,
                class == self.class as usize,
                gi.same_but_fresh(g0),
                self.inv(),
                sl == self.spec_slots(),
                sl.wf(),
                sl.sb == cls.slab_bits,
                sl.nslots == cls.nslots,
                sl.nbits == self.spec_nslabs() * sl.sb,
                gi.fresh@.id() == sl.mloc,
                init_marks(slabs.start * sl.sb, self.spec_nslabs() * sl.sb).subset_of(gi.fresh@@),
                gi.dbase@ == self.desc_base as int,
                gi.dstride@ == self.desc_stride as int,
                forall|x: int| slabs.start <= x < self.spec_nslabs() ==> #[trigger] dfresh(gi.dperms@, gi.dbase@, gi.dstride@, x),
                forall|x: nat| #[trigger] dout.contains_key(x) ==> s <= x < slabs.start,
                forall|x: nat| s <= x < slabs.start ==> #[trigger] desc_handed(*dout, *self, x, arena),
                owner_marks(s as int, self.spec_nslabs() as int).subset_of(gi.fresh@@),
                inited.id() == sl.mloc,
                old(inited)@.subset_of(inited@),
                forall|x: int| s <= x < slabs.start && !self.spec_is_guard(x as usize)
                    ==> #[trigger] slab_marked(*self, x, inited@),
                forall|x: int| s <= x < slabs.start && self.spec_is_guard(x as usize)
                    ==> #[trigger] guard_marked(*self, x, inited@),
                *cls == class_table()[self.spec_class() as int],
                cls.wf(),
            ensures
                gi.same_but_fresh(g0),
                gi.fresh@.id() == sl.mloc,
                init_marks(end * sl.sb, self.spec_nslabs() * sl.sb).subset_of(gi.fresh@@),
                forall|x: int| end <= x < self.spec_nslabs() ==> #[trigger] dfresh(gi.dperms@, gi.dbase@, gi.dstride@, x),
                forall|x: nat| #[trigger] dout.contains_key(x) ==> s <= x < end,
                forall|x: nat| s <= x < end ==> #[trigger] desc_handed(*dout, *self, x, arena),
                owner_marks(s as int, self.spec_nslabs() as int).subset_of(gi.fresh@@),
                inited.id() == sl.mloc,
                old(inited)@.subset_of(inited@),
                forall|x: int| s <= x < end && !self.spec_is_guard(x as usize)
                    ==> #[trigger] slab_marked(*self, x, inited@),
                forall|x: int| s <= x < end && self.spec_is_guard(x as usize)
                    ==> #[trigger] guard_marked(*self, x, inited@),
            decreases slabs.end - slabs.start,
        {
            proof {
                assert(slabs.start * sl.sb <= sl.nbits) by (nonlinear_arith)
                    requires slabs.start <= self.spec_nslabs(), sl.nbits == self.spec_nslabs() * sl.sb, sl.sb > 0;
                assert((slabs.start + 1) * sl.sb == slabs.start * sl.sb + sl.sb) by (nonlinear_arith);
                crate::bitmap::lemma_marks_sub((slabs.start + 1) * sl.sb, self.spec_nslabs() * sl.sb,
                    slabs.start * sl.sb, self.spec_nslabs() * sl.sb);
            }
            match slabs.next() {
                None => break,
                Some(slab) => {
                    if self.is_guard_slab(slab) {
                        // A guard slab's init markers leave the value too,
                        // for the arena to keep (`next_slab_locked`).
                        proof {
                            let f = slab * sl.sb;
                            assert(f + sl.sb <= sl.nbits) by (nonlinear_arith)
                                requires slab < self.spec_nslabs(), sl.nbits == self.spec_nslabs() * sl.sb, f == slab * sl.sb, sl.sb > 0;
                            crate::bitmap::lemma_marks_sub(f, f + sl.sb, f, self.spec_nslabs() * sl.sb);
                            crate::bitmap::lemma_marks_split(f, f + sl.sb, self.spec_nslabs() * sl.sb);
                            let tracked gm = gi.fresh.borrow_mut().split(init_marks(f, f + sl.sb));
                            assert forall|m: int| #[trigger] owner_marks(s as int, self.spec_nslabs() as int).contains(m) implies gi.fresh@@.contains(m) by {
                                lemma_owner_marks_apart(s as int, self.spec_nslabs() as int, f, f + sl.sb, m);
                            }
                            let before = inited@;
                            inited.combine(gm.persist());
                            assert forall|x: int| s <= x < slabs.start && !self.spec_is_guard(x as usize)
                                implies #[trigger] slab_marked(*self, x, inited@) by {
                                lemma_slab_marked_mono(*self, x, before, inited@);
                            }
                            assert forall|x: int| s <= x < slabs.start && self.spec_is_guard(x as usize)
                                implies #[trigger] guard_marked(*self, x, inited@) by {
                                if x != slab {
                                    lemma_guard_marked_mono(*self, x, before, inited@);
                                }
                            }
                        }
                        // Its descriptor's permission goes as it is: a
                        // guard slab's descriptor stays zero.
                        proof {
                            use_type_invariant(self);
                            self.lemma_desc_at(slab);
                            let (m0, d0) = (gi.dperms@, *dout);
                            assert(dfresh(m0, gi.dbase@, gi.dstride@, slab as int));
                            let tracked p = gi.dperms.borrow_mut().tracked_remove(slab as int);
                            dout.tracked_insert(slab as nat, p);
                            assert forall|x: int| slabs.start <= x < self.spec_nslabs()
                                implies #[trigger] dfresh(gi.dperms@, gi.dbase@, gi.dstride@, x) by {
                                assert(dfresh(m0, gi.dbase@, gi.dstride@, x));
                            }
                            assert forall|x: nat| s <= x < slabs.start implies #[trigger] desc_handed(*dout, *self, x, arena) by {
                                if x != slab {
                                    assert(desc_handed(d0, *self, x, arena));
                                }
                            }
                        }
                        continue;
                    }
                    // SAFETY: the descriptor and bitmap pages for `slab` were
                    // committed above.
                    unsafe {
                        proof {
                            use_type_invariant(self);
                            self.lemma_desc_at(slab);
                            assert(dfresh(gi.dperms@, gi.dbase@, gi.dstride@, slab as int));
                        }
                        if purge::check_zero() {
                            let d = SlabDesc::at(self.desc_base, class, slab, Tracked(self.desc_perms.borrow().tracked_borrow(slab as int)));
                            let (st, sh) = d.state_bytes();
                            if sh != 0 || d.live() != 0 || st != 0 {
                                crate::sys::abort_fmt(format_args!(
                                    "check_zero: class {class}: take_group re-hands slab {slab} (group {s}..{end}, arena {arena}): descriptor at {:#x} has state {st} shadow {sh} live {} arena {} dirty_at {}",
                                    super::verify::desc_ptr_addr(d),
                                    d.live(),
                                    d.arena(),
                                    d.dirty_at()
                                ));
                            }
                        }
                        // The descriptor's permission goes with it, written
                        // `Fresh` and `arena`'s.
                        let ghost (m0, d0) = (gi.dperms@, *dout);
                        let tracked mut dp = gi.dperms.borrow_mut().tracked_remove(slab as int);
                        SlabDesc::at(self.desc_base, class, slab, Tracked(self.desc_perms.borrow().tracked_borrow(slab as int))).init(arena, Tracked(&mut dp));
                        proof {
                            dout.tracked_insert(slab as nat, dp);
                            assert forall|x: int| slabs.start <= x < self.spec_nslabs()
                                implies #[trigger] dfresh(gi.dperms@, gi.dbase@, gi.dstride@, x) by {
                                assert(dfresh(m0, gi.dbase@, gi.dstride@, x));
                            }
                            assert forall|x: nat| s <= x < slabs.start implies #[trigger] desc_handed(*dout, *self, x, arena) by {
                                if x != slab {
                                    assert(desc_handed(d0, *self, x, arena));
                                }
                            }
                        }
                        let ghost f = slab * sl.sb;
                        proof {
                            assert(f + sl.sb <= sl.nbits) by (nonlinear_arith)
                                requires slab < self.spec_nslabs(), sl.nbits == self.spec_nslabs() * sl.sb, f == slab * sl.sb, sl.sb > 0;
                            crate::bitmap::lemma_marks_sub(f, f + sl.sb, f, self.spec_nslabs() * sl.sb);
                            crate::bitmap::lemma_marks_split(f, f + sl.sb, self.spec_nslabs() * sl.sb);
                            self.lemma_guard_slab(slab as int);
                        }
                        let tracked fs = gi.fresh.borrow_mut().split(init_marks(f, f + sl.sb));
                        proof {
                            assert forall|m: int| #[trigger] owner_marks(s as int, self.spec_nslabs() as int).contains(m) implies gi.fresh@@.contains(m) by {
                                lemma_owner_marks_apart(s as int, self.spec_nslabs() as int, f, f + sl.sb, m);
                            }
                        }
                        let Tracked(m) = self.bits_unchecked(slab).init(cls.nslots(), Ghost(slab as int), Tracked(fs));
                        proof {
                            let before = inited@;
                            inited.combine(m);
                            assert forall|x: int| s <= x < slabs.start && !self.spec_is_guard(x as usize)
                                implies #[trigger] slab_marked(*self, x, inited@) by {
                                let fx = fresh_marks(x * sl.sb, x * sl.sb + sl.sb);
                                if x != slab {
                                    assert(slab_marked(*self, x, before));
                                    assert forall|y: int| #[trigger] fx.contains(y) implies inited@.contains(y) by {
                                        assert(before.contains(y));
                                    }
                                } else {
                                    assert forall|y: int| #[trigger] fx.contains(y) implies inited@.contains(y) by {
                                        assert(m@.contains(y));
                                    }
                                }
                            }
                            assert forall|x: int| s <= x < slabs.start && self.spec_is_guard(x as usize)
                                implies #[trigger] guard_marked(*self, x, inited@) by {
                                lemma_guard_marked_mono(*self, x, before, inited@);
                            }
                        }
                    }
                }
            }
        }
        // The group's owner-store markers go to the store, which uses them
        // up: each slab's owner is stored once.
        proof {
            broadcast use vstd::set_lib::range_set_properties;
            assert(owner_marks(s as int, end as int).subset_of(gi.fresh@@));
        }
        let tracked om = gi.fresh.borrow_mut().split(owner_marks(s as int, end as int));
        // The store, inside the word's invariant: the group's witness
        // markers become persistent with it, and the commit lock's copy of
        // the persistent ones takes them.
        proof {
            use_type_invariant(&*self);
            self.lemma_group_in_range(s);
            assert(self.spec_group_range(s >> self.lg_group) == (s, end));
        }
        let tracked mut stored = GhostPersistentSubset::<int>::dummy();
        vstd::open_atomic_invariant!(self.owner_invs.borrow().tracked_borrow(og) => v => {
            proof {
                if owner_marks(s as int, end as int).subset_of(v.1@) {
                    stored = v.1.take().persist();
                    v.2 = stored.duplicate();
                } else {
                    stored = v.2.duplicate();
                }
            }
            // SAFETY: the owner page for the group was committed above.
            unsafe { store_owner(owner_word, arena, Ghost(*self), Ghost(s >> self.lg_group), Tracked(&mut v.0), Tracked(om)) };
            proof {
                // The taker: its arena's record and the group's slabs in
                // its owned-slab record, which had none of them (it owns
                // only slabs below `s`).
                assert(sets.oauth@@.disjoint(Set::<int>::range(s as int, end as int))) by {
                    broadcast use vstd::set_lib::range_set_properties;
                    assert(owned_rec(old(sets).oauth@@, old(sets).states@));
                    assert forall|x: int| #[trigger] sets.oauth@@.contains(x) implies !Set::<int>::range(s as int, end as int).contains(x) by {
                        assert(old(sets).oauth@@.contains(x));
                        assert(old(sets).owns(x as nat));
                    }
                }
                let tracked f = sets.oauth.borrow_mut().insert_set(Set::<int>::range(s as int, end as int));
                v.3 = Some((pubw.get(), f.persist()));
            }
        });
        proof {
            broadcast use vstd::set_lib::range_set_properties;
            // The class lock value's witness markers take the group's too:
            // its slabs join the value's ghost (`Inner::base`).
            if sets.seen@.id() == stored.id() {
                sets.seen.borrow_mut().combine(stored.duplicate());
            } else {
                sets.seen = Tracked(stored.duplicate());
            }
            gi.seen.borrow_mut().combine(stored);
            assert(owner_marks(0, end as int) =~= owner_marks(0, s as int).union(owner_marks(s as int, end as int)));
        }
        gi.next_slab = end;
        proof {
            assert(owner_marks(end as int, self.spec_nslabs() as int).subset_of(gi.fresh@@)) by {
                broadcast use vstd::set_lib::range_set_properties;
            }
            let gr = gi.group@;
            vstd::arithmetic::div_mod::lemma_mod_adds(s as int, gr as int, gr as int);
            vstd::arithmetic::div_mod::lemma_mod_self_0(gr as int);
        }
        proof {
            use_type_invariant(&*self);
        }
        // `double_free_scan`'s state bytes for the group, before the
        // watermark that lets a free read them (nothing without the key).
        crate::heap::slotstate::commit(class, self.base, s, end);
        // The watermark becomes `end`, the new `next_slab`, with both
        // halves of the ghost variable that ties the two.
        vstd::open_atomic_invariant!(self.committed_inv.borrow() => v => {
            store_usize(&self.committed_slabs, end, Ordering::Release, Tracked(&mut v.0));
            proof {
                v.1 = gi.seen.borrow().duplicate();
                gi.wm.borrow_mut().update(&mut v.2, end);
            }
        });
        if !gi.mirror.is_null() {
            let tracked mi = gi.mirror_inv.borrow().tracked_unwrap();
            vstd::open_atomic_invariant!(mi => v => {
                // SAFETY: `mirror_watermark`'s caller keeps the copy alive
                // as long as the table.
                unsafe { store_usize_at(gi.mirror, end, Ordering::Release, Tracked(&mut v.0)) };
                proof {
                    // The copy becomes `end` with both halves of its
                    // ghost variable.
                    let tracked mut a = gi.mauth.borrow_mut().tracked_take();
                    a.update(&mut v.1, end);
                    gi.mauth = Tracked(Some(a));
                }
            });
        }
        proof {
            assert(gi.inv());
        }
        // `thp:auto` (design 7, 17; `thp.rs`): the group's range, still
        // reserved `PROT_NONE`, is advised once here, so the slabs the
        // arena commits from it fault in as folios of the enabled size.
        // Off, and under `always` (whose PMD chunks `commit_slab` advises),
        // this is a load and a compare.
        if crate::thp::class_mode(cls.slab_size()) == crate::thp::Mode::Folios {
            crate::thp::advise_class(self.slab_addr(s), (end - s) << cls.lg_slab, true);
        }
        proof {
            let gr = g@.group@;
            vstd::arithmetic::div_mod::lemma_mod_adds(s as int, gr as int, gr as int);
            vstd::arithmetic::div_mod::lemma_mod_self_0(gr as int);
            assert(g@.inv());
        }
        g.unlock();
        Some((s, end))
    }

    /// The next slab to hand out, read under the commit lock (`t`, its
    /// value) with a class lock over this table held by the caller (`v`,
    /// its value): every slab `v`'s ghost owns is below it, and so is
    /// `v`'s current group's end (the slab before it is owned). The proof
    /// is by markers ([`lemma_marked_below`]): an owned slab's markers are
    /// held by `v` (a guard slab's init markers, the fresh markers `init`
    /// returned for any other), while the commit lock's value holds the
    /// init markers of every slab from the watermark on, and a slab there
    /// would have its markers in two places at once.
    ///
    /// [`lemma_marked_below`]: SlabTable::lemma_marked_below
    #[allow(unused_variables)]
    #[inline(always)]
    fn next_slab_locked(&self, t: &TableInner, v: &Inner) -> (r: usize)
        requires
            v.table@ == *self,
            v.base(),
            v.nbits() == self.spec_nslabs(),
            t.inv(),
            t.nslabs@ == self.spec_nslabs(),
            t.sb@ == self.spec_slots().sb,
            t.mloc@ == self.spec_slots().mloc,
        ensures
            r == t.next_slab,
            forall|x: nat| #[trigger] v.owns(x) ==> x < r,
            v.group_end <= r,
    {
        let credit = vstd::invariant::create_open_invariant_credit();
        proof {
            use_type_invariant(self);
            if exists|x: nat| v.owns(x) && x >= t.next_slab {
                let x = choose|x: nat| v.owns(x) && x >= t.next_slab;
                self.lemma_marked_below(t.fresh.borrow(), v.inited.borrow(), credit.get(), x as int, t.next_slab as int);
            }
            if v.group_end > 0 {
                assert(v.owns((v.group_end - 1) as nat));
            }
        }
        t.next_slab
    }

    /// No slab at or above `lo` has its markers held by `inited` while
    /// `fresh` holds the init markers of every bit from slab `lo` on: a
    /// guard slab's init markers would be in `fresh` and `inited` at once,
    /// and any other slab's first bit has, in its word's invariant
    /// ([`marks_ok`](crate::bitmap::marks_ok)), either its fresh marker,
    /// which `inited` holds, or its init marker, which `fresh` holds.
    proof fn lemma_marked_below(
        tracked &self,
        tracked fresh: &GhostSubset<int>,
        tracked inited: &GhostPersistentSubset<int>,
        tracked credit: vstd::invariant::OpenInvariantCredit,
        x: int,
        lo: int,
    )
        requires
            self.inv(),
            fresh.id() == self.spec_slots().mloc,
            inited.id() == self.spec_slots().mloc,
            0 <= lo <= x < self.spec_nslabs(),
            init_marks(lo * self.spec_slots().sb, self.spec_nslabs() * self.spec_slots().sb).subset_of(fresh@),
            self.spec_is_guard(x as usize) ==> guard_marked(*self, x, inited@),
            !self.spec_is_guard(x as usize) ==> slab_marked(*self, x, inited@),
        ensures
            false,
        opens_invariants any
    {
        broadcast use crate::bitmap::lemma_marks;
        self.lemma_slots_wf();
        let sl = self.spec_slots();
        let b = x * sl.sb;
        assert(lo * sl.sb <= b && b + sl.sb <= sl.nbits) by (nonlinear_arith)
            requires lo <= x < self.nslabs, b == x * sl.sb, sl.nbits == self.nslabs * sl.sb, sl.sb > 0;
        assert(fresh@.contains(init_mark(b)));
        if self.spec_is_guard(x as usize) {
            assert(inited@.contains(init_mark(b)));
            let tracked mut c = inited.duplicate();
            c.disjoint(fresh);
        } else {
            assert(inited@.contains(fresh_mark(b)));
            let j = b / 64;
            assert(0 <= j < self.spec_nwords());
            assert(self.word_invs@.contains_key(j));
            let tracked inv = (*self.word_invs.borrow()).tracked_borrow(j);
            assert(inv.constant().slots == sl);
            vstd::open_atomic_invariant_in_proof!(credit => inv => pair => {
                let tracked (perm, toks, mut u, mut d, sn) = pair;
                assert(64 * j <= b < 64 * j + 64);
                assert(u.id() == sl.mloc && d.id() == sl.mloc);
                assert(crate::bitmap::marks_ok(sl, j, perm.value(), u@, d@));
                assert(u@.contains(fresh_mark(b)) || d@.contains(init_mark(b)));
                if u@.contains(fresh_mark(b)) {
                    u.disjoint_persistent(inited);
                } else {
                    assert(d@.contains(init_mark(b)));
                    d.disjoint(fresh);
                }
                pair = (perm, toks, u, d, sn);
            });
        }
    }

    /// The owner word of `group`, the one its invariant's permission is to
    /// ([`SlabTable::perms_ok`]).
    ///
    /// # Safety
    /// The group must have been handed out, or be about to be under the
    /// commit lock, before the word is read (its owner page is committed:
    /// the premise of `Unbacked::into_lazy`).
    #[inline]
    pub(super) unsafe fn owner_word(&self, group: usize) -> (r: *const AtomicU16)
        requires
            group < self.spec_ngroups(),
        ensures
            r == owner_ptr_at(self.owner_addr(group as int)),
    {
        proof {
            use_type_invariant(self);
        }
        addr_to_ptr(self.owners_base + group * 2) as *const AtomicU16
    }

    /// The view of `slab`'s bits: words `first / 64 ..` of the class
    /// bitmap at `bits_base`, with `first = slab * slab_bits`, which the
    /// table's word invariants are for, borrowed through the words'
    /// permissions ([`SlabTable::perms_ok`]), which make them the words
    /// those invariants are keyed by.
    ///
    /// # Safety
    /// As [`desc_unchecked`](SlabTable::desc_unchecked).
    #[inline]
    // Hot bitmap lookup: left as written.
    #[allow(clippy::manual_is_multiple_of)]
    unsafe fn bits_unchecked(&self, slab: usize) -> (r: AtomicBitmap<'_>)
        requires
            slab < self.spec_nslabs(),
        ensures
            bits_shape(r, class_table()[self.spec_class() as int].slab_bits as usize),
            bits_ghost(r, *self, slab),
    {
        proof {
            use_type_invariant(self);
            self.lemma_slots_wf();
            self.lemma_bits_fit(slab);
            reveal(words_perms_ok);
        }
        let nbits = self.slab_bits as usize;
        let first = slab * nbits;
        let ghost j0 = first as int / 64;
        let ghost sl = self.spec_slots();
        proof {
            assert(self.word_perms@.contains_key(j0));
        }
        let word = addr_to_ptr(self.bits_base + (first / 64) * 8) as *const AtomicU64;
        if nbits >= 64 {
            let ghost k = nbits / 64;
            proof {
                let (n, sb) = (self.nslabs as int, nbits as int);
                assert(first % 64 == 0) by {
                    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(slab as int * (sb / 64), 64);
                    assert(first == slab as int * (sb / 64) * 64) by (nonlinear_arith)
                        requires first == slab * sb, sb == (sb / 64) * 64;
                }
                assert(j0 + k <= self.spec_nwords()) by {
                    assert(first + sb <= n * sb) by (nonlinear_arith)
                        requires slab < n, first == slab * sb, sb >= 0;
                }
                assert(vstd::layout::size_of::<AtomicU64>() == 8);
                assert(word@.addr + k * 8 <= usize::MAX);
                assert forall|i: int| #![trigger self.word_perms@.contains_key(j0 + i)] #![trigger self.word_perms@[j0 + i]] 0 <= i < k implies self.word_perms@.contains_key(j0 + i)
                    && self.word_perms@[j0 + i].is_init()
                    && self.word_perms@[j0 + i].ptr() == ptr_mut_from_data::<AtomicU64>(
                        PtrData { addr: (word@.addr + 8 * i) as usize, provenance: word@.provenance, metadata: () }) by {
                    assert(self.word_perms@.contains_key(j0 + i));
                }
            }
            debug_assert!(first % 64 == 0);
            // The group's words were committed before the watermark moved
            // (the caller's premise). `word_perms` is passed on as it is:
            // a `Tracked(..)` built here would be one more distinct
            // `Tracked` type under `Heap::flush`.
            // SAFETY: the `nbits / 64` words are consecutive words of the
            // class bitmap, one reservation from `bits_base`, so one
            // allocation.
            let words = unsafe { crate::vspec::slice::slice_from_perms(word, nbits / 64, Ghost(j0), self.word_perms) };
            proof {
                assert forall|i: int| 0 <= i < words@.len() implies #[trigger] crate::bitmap::word_ok(words@[i], first / 64 + i, sl, *self.word_invs@) by {
                    assert(self.word_perms@.contains_key(j0 + i));
                    assert(self.word_invs@.contains_key(j0 + i));
                }
            }
            AtomicBitmap::from_words(words, Ghost(first as int), Ghost(self.spec_slots()), self.word_invs)
        } else {
            // As above, one word.
            let w = ptr_ref(word, Tracked((*self.word_perms.borrow()).tracked_borrow(j0)));
            proof {
                assert(self.word_invs@.contains_key(j0));
                // A sub-word slab's bits do not cross a word: `slab_bits` is
                // a power of two below 64 (`Class::wf`), which `first`, a
                // multiple of it, keeps.
                let (s, f) = (nbits, first);
                assert(crate::layout::LINE_BITS >= 64);
                crate::meta::lemma_pow2_bits(s);
                vstd::arithmetic::div_mod::lemma_mod_multiples_basic(slab as int, s as int);
                assert(f % 64 + s <= 64) by (bit_vector)
                    requires f % s == 0, s & sub(s, 1) == 0, 0 < s, s < 64;
            }
            AtomicBitmap::sub_word(w, (first % 64) as u32, nbits, Ghost(first as int), Ghost(self.spec_slots()), self.word_invs)
        }
    }

    /// Slab `slab`'s bits are inside the class bitmap's bits, at most
    /// `2^42` of them, and every bit of the bitmap is in one of its words.
    proof fn lemma_bits_fit(&self, slab: usize)
        requires
            self.inv(),
            slab < self.nslabs,
        ensures
            1 <= self.slab_bits,
            0 <= slab * (self.slab_bits as int),
            slab * (self.slab_bits as int) + self.slab_bits <= self.nslabs * (self.slab_bits as int),
            self.nslabs * (self.slab_bits as int) <= 0x400_0000_0000,
            forall|b: int| 0 <= b < self.nslabs * (self.slab_bits as int) ==> #[trigger] (b / 64) < self.spec_nwords(),
    {
        let (n, sb) = (self.nslabs as int, self.slab_bits as int);
        assert(crate::layout::MAX_SLAB_WORDS * 64 <= 0x1_0000);
        assert(0 <= slab * sb && slab * sb + sb <= n * sb) by (nonlinear_arith)
            requires slab < n, sb >= 0;
        assert(n * sb <= 0x400_0000 * 0x1_0000) by (nonlinear_arith)
            requires 0 <= n <= 0x400_0000, 0 <= sb <= 0x1_0000;
        assert forall|b: int| 0 <= b < n * sb implies #[trigger] (b / 64) < self.spec_nwords() by {
            vstd::arithmetic::div_mod::lemma_div_is_ordered(b, n * sb - 1, 64);
            vstd::arithmetic::div_mod::lemma_div_plus_one(n * sb - 1, 64);
        }
    }
}

} // verus!

verus! {

impl SlabTable {
    /// The slab range of `group`: `group << lg_group` and the `group`
    /// slabs from there, clipped to the class's slab count (the last
    /// group of a region may be short).
    #[inline]
    pub(super) fn group_range(&self, group: usize) -> (r: (usize, usize))
        requires
            group <= self.spec_nslabs() >> self.lg_group,
        ensures
            r.0 == group << self.lg_group,
            r.0 <= r.1 <= self.spec_nslabs(),
            r == self.spec_group_range(group),
    {
        proof {
            use_type_invariant(self);
            let (n, lg) = (self.nslabs, self.lg_group);
            assert(group <= n >> lg && lg < 64 ==> group << lg <= n) by (bit_vector);
        }
        let start = group << self.lg_group;
        (start, (start + self.group as usize).min(self.nslabs))
    }

    /// Whether an arena whose cursor reaches slab `s` of its group commits
    /// a run there ([`commit_run`]): at the group's first slab, at a guard
    /// slab, and every [`run_slabs`] slabs except right after a guard slab
    /// (whose run goes on); every slab, where slabs are committed one by
    /// one. Any other slab was committed with the run before it.
    ///
    /// [`commit_run`]: SlabTable::commit_run
    /// [`run_slabs`]: SlabTable::run_slabs
    #[inline]
    pub(super) fn starts_run(&self, s: usize) -> (r: bool)
        requires
            s < self.spec_nslabs(),
        ensures
            !r ==> !self.spec_is_guard(s),
    {
        proof {
            use_type_invariant(self);
            let lg = self.lg_group;
            assert((1usize << lg) >= 1) by (bit_vector)
                requires lg < 64;
            assert(s & sub(1usize << lg, 1) != 0 ==> s > 0) by (bit_vector);
        }
        !self.runs()
            || s & ((1 << self.lg_group) - 1) == 0
            || self.is_guard_slab(s)
            || (s & (self.run_slabs() - 1) == 0 && !self.is_guard_slab(s - 1))
    }

    /// The most slabs a run holds: [`RUN_BYTES`] of them, at least one (a
    /// power of two, as both are).
    #[inline]
    fn run_slabs(&self) -> (r: usize)
        ensures
            r == self.spec_run_slabs(),
    {
        (RUN_BYTES >> self.class().lg_slab).max(1)
    }

    /// Whether this class commits runs: one slab per page group (a page
    /// above the slab is only the tests' faked pages) and not the PMD
    /// chunks of `thp:always`, which [`commit_slab`] commits slab by slab.
    /// The `thp` mode is fixed before the first allocation (`thp::configure`
    /// and `thp::force_never` run only during init); a mode that changed
    /// in the middle of a group would make [`starts_run`] skip slabs no run
    /// committed.
    ///
    /// [`commit_slab`]: SlabTable::commit_slab
    /// [`starts_run`]: SlabTable::starts_run
    #[inline]
    pub(super) fn runs(&self) -> (r: bool)
        ensures
            r ==> self.spec_slabs_per_page() == 1,
    {
        self.slabs_per_page == 1
            && crate::thp::class_mode(self.class().slab_size()) != crate::thp::Mode::Chunks
    }

    /// Commit the run that starts at slab `s` ([`starts_run`]) of a group
    /// handed to the caller's arena: slabs `s` up to the next guard slab or
    /// the next multiple of [`run_slabs`], at most to `limit` (the group's
    /// end), with one `mprotect`, `s` itself guarded (`install_guard`) when
    /// it is a guard slab. `Some(true)` when `s` is a slab to allocate
    /// from, `Some(false)` for a guard slab, `None` when the run cannot be
    /// committed. On `None` part of the run may be read-write (Linux
    /// `mprotect` can change a prefix of the range before it fails); the
    /// cursor does not move, so the retry commits the whole run again.
    ///
    /// The guard slab that ends a run stays uncommitted until the cursor
    /// reaches it, as one commit per slab leaves it: a write running off
    /// the run faults at its end, and a process that never fills the run
    /// pays for no guard. Each `mprotect` that grows the class region's
    /// read-write mapping takes the process's `mmap_lock` for writing and
    /// stalls every thread's page faults behind it (design 5). The
    /// run is the caller's (its arena's group, past its cursor, under its
    /// class lock), so the syscalls run without the commit lock, which
    /// only counts the guard.
    ///
    /// `ord` is the caller's lock-order token for the commit lock.
    ///
    /// [`starts_run`]: SlabTable::starts_run
    /// [`run_slabs`]: SlabTable::run_slabs
    pub(super) fn commit_run(&self, s: usize, limit: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<bool>)
        requires
            crate::sys::may_syscall(),
            s < limit <= self.spec_nslabs(),
            old(ord).allows(rank_table(0)),
        ensures
            r matches Some(usable) ==> usable == !self.spec_is_guard(s),
            *final(ord) == *old(ord),
    {
        if !self.runs() {
            return self.commit_slab(s, Tracked(ord));
        }
        proof {
            use_type_invariant(self);
        }
        let cls = self.class();
        let guard = self.is_guard_slab(s);
        let end = self.run_end(s, limit);
        if !guard {
            proof {
                lemma_run_commit(*self, s, end, false);
            }
            commit(self.slab_addr(s), (end - s) << cls.lg_slab, Ghost(self.class as int), Ghost(false)).ok()?;
            return Some(true);
        }
        let page = 1usize << self.lg_page;
        // The guard first, on the slab the cursor finds `PROT_NONE`: a
        // marker survives the run's commit, so the run is one `mprotect`
        // as before; without one the slab stays `PROT_NONE` and only the
        // slabs after it are committed.
        if self.install_guard(self.slab_addr(s), page, cls.slab_size())? {
            proof {
                lemma_run_commit(*self, s, end, true);
            }
            commit(self.slab_addr(s), (end - s) << cls.lg_slab, Ghost(self.class as int), Ghost(false)).ok()?;
        } else if end > s + 1 {
            proof {
                lemma_run_commit(*self, (s + 1) as usize, end, false);
            }
            commit(self.slab_addr(s + 1), (end - s - 1) << cls.lg_slab, Ghost(self.class as int), Ghost(false)).ok()?;
        }
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        g.guards = g.guards.wrapping_add(1);
        g.unlock();
        Some(false)
    }

    /// The end of the run that starts at slab `s` ([`starts_run`]), for a
    /// class that [`runs`]: the next slab that starts a run, the group's
    /// end `limit` standing for the next group's first slab.
    ///
    /// [`starts_run`]: SlabTable::starts_run
    /// [`runs`]: SlabTable::runs
    #[inline]
    pub(super) fn run_end(&self, s: usize, limit: usize) -> (r: usize)
        requires
            s < limit <= self.spec_nslabs(),
            self.spec_slabs_per_page() == 1,
        ensures
            s < r <= limit,
            forall|g: usize| s < g < r ==> !#[trigger] self.spec_is_guard(g),
    {
        proof {
            use_type_invariant(self);
            lemma_run_slabs(*self);
            lemma_guard_test(*self, s);
            let m = (self.spec_run_slabs() - 1) as usize;
            assert(s <= 0x400_0000 && m < 0x80 ==> s < (s | m) + 1 <= s + 0x80) by (bit_vector);
            let gm = self.guard_mask as usize;
            assert(s < 0x400_0000 && gm <= 0xFFFF_FFFF ==> s < add(s, 1usize) | gm && add(s, 1usize) | gm <= 0x1_0400_0000) by (bit_vector);
            assert(s & gm != gm ==> s < s | gm) by (bit_vector);
        }
        debug_assert!(s < limit && limit <= self.nslabs);
        // With one slab per page group a guard slab is every
        // `guard_mask + 1`-th slab and `x | guard_mask` the first one at or
        // above `x`.
        let guard = self.is_guard_slab(s);
        let mut every = (s | (self.run_slabs() - 1)) + 1;
        if guard && every == s + 1 {
            every += self.run_slabs();
        }
        let mut end = every.min(limit);
        if self.guard_mask != u32::MAX {
            end = end.min(if guard {
                (s + 1) | self.guard_mask as usize
            } else {
                s | self.guard_mask as usize
            });
        }
        proof {
            // No guard slab lies past `s` and before `end`: `x | guard_mask`
            // is the first one at or above `x` (`s + 1` after a guard slab).
            assert forall|g: usize| s < g < end implies !#[trigger] self.spec_is_guard(g) by {
                lemma_guard_test(*self, g);
                if self.guard_mask != u32::MAX {
                    let m = self.guard_mask;
                    let gm = m as usize;
                    let x = if guard { (s + 1) as usize } else { s };
                    assert(m & (m >> 1u32) == m >> 1u32 ==> gm & (gm >> 1) == gm >> 1) by (bit_vector)
                        requires gm == m as usize;
                    assert(gm & (gm >> 1) == gm >> 1 && x <= g && g < (x | gm) ==> g & gm != gm) by (bit_vector);
                }
            }
        }
        end
    }

    /// Commit slab `s` of a group handed to the caller's arena: make its
    /// memory (or its whole page group, when the page is larger than the
    /// slab and `s` starts one) read-write, or install the guard when `s`
    /// is a guard slab. `Some(true)` for a slab ready to allocate from,
    /// `Some(false)` for a guard slab, `None` when `mprotect` fails (the
    /// slab stays uncommitted and may be retried). `ord` is the caller's
    /// lock-order token for the commit lock.
    // The page-group test stays `s % spp == 0`, not `is_multiple_of`.
    #[allow(clippy::manual_is_multiple_of)]
    pub(super) fn commit_slab(&self, s: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<bool>)
        requires
            crate::sys::may_syscall(),
            s < self.spec_nslabs(),
            old(ord).allows(rank_table(0)),
        ensures
            r matches Some(usable) ==> usable == !self.spec_is_guard(s),
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let cls = self.class();
        let page = 1usize << self.lg_page;
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        let addr = self.slab_addr(s);
        // A slab smaller than a page is committed with its whole page
        // group when the group's first slab comes up; a group is whole
        // page groups and the arena walks it in order, so the later slabs
        // of the page group find it committed.
        let spp = self.slabs_per_page as usize;
        let len = cls.slab_size().max(page);
        let mode = crate::thp::class_mode(cls.slab_size());
        proof {
            let lp = self.lg_page;
            assert((1usize << lp) <= 0x8000_0000_0000_0000usize) by (bit_vector)
                requires lp < 64;
            assert((1usize << 57u32) == 0x200_0000_0000_0000usize) by (bit_vector);
        }
        let first_of_page_group = s % spp == 0;
        let is_guard = self.is_guard_slab(s);
        // Under `thp:always` with PMD chunks the marker is not installed:
        // a guard marker (or a `PROT_NONE` page) inside a 2 MiB page
        // splits it back into small pages the moment the arena reaches
        // the guard slab (measured: every chunk's huge page was gone by
        // the time the slab was in use), so the slab is left unused and
        // unguarded; design 17 records the trade. Otherwise the guard
        // goes on first, while the slab is still `PROT_NONE` (as in
        // `commit_run`): a marker survives the commit, and without one
        // the page group stays `PROT_NONE`.
        let committed = if !first_of_page_group {
            true
        } else if is_guard && mode != crate::thp::Mode::Chunks {
            match self.install_guard(addr, page, len) {
                Some(true) => {
                    proof {
                        lemma_page_group_commit(*self, s, len, addr);
                    }
                    self.commit_page_group(addr, len, mode).is_some()
                },
                Some(false) => true,
                None => false,
            }
        } else {
            proof {
                lemma_page_group_commit(*self, s, len, addr);
            }
            self.commit_page_group(addr, len, mode).is_some()
        };
        if !committed {
            g.unlock();
            return None;
        }
        if is_guard {
            g.guards = g.guards.wrapping_add(1);
            g.unlock();
            return Some(false);
        }
        g.unlock();
        Some(true)
    }

    /// Make the `len` bytes at `addr` (a slab, or the page group it
    /// starts) read-write. Under `thp:always` with the PMD size enabled
    /// (`Mode::Chunks`; design 7, 17; `thp.rs`) the PMD-sized chunk
    /// around them, clipped to the region, is committed and advised
    /// instead, so the first touch maps one 2 MiB page (a chunk already
    /// committed costs two no-op calls); if that fails, the plain commit
    /// is the fallback and the slab is served with small pages. `None`
    /// when the plain commit fails too.
    fn commit_page_group(&self, addr: usize, len: usize, mode: crate::thp::Mode) -> Option<()>
        requires
            crate::sys::may_syscall(),
            self.base <= addr <= self.base + (self.spec_nslabs() << class_table()[self.spec_class() as int].lg_slab),
            addr + len <= usize::MAX,
            commit_inside(self.class as int, addr as int, len as int),
            mode != crate::thp::Mode::Chunks ==> commit_spares_guards(self.class as int, addr as int, len as int),
    {
        proof {
            use_type_invariant(self);
        }
        if mode == crate::thp::Mode::Chunks {
            let cls = self.class();
            let pmd = crate::thp::pmd_size();
            proof {
                let (n, lg, b) = (self.nslabs, cls.lg_slab, self.base);
                assert(n <= (usize::MAX - b) as usize >> lg && lg <= 22 ==> b + (n << lg) <= usize::MAX) by (bit_vector);
                assert(b < 1usize << 57 && pmd & sub(pmd, 1) == 0 && pmd > 0 ==> b + pmd <= usize::MAX) by (bit_vector);
            }
            let region_end = self.base + (self.nslabs << cls.lg_slab);
            let (lo, hi) = crate::thp::chunk(addr, len, self.base, region_end, pmd);
            // Groups are one to sixteen slabs and arenas commit theirs
            // in any order, so which call reaches a chunk first is not
            // known here: every slab of the chunk re-issues the two
            // calls (no-ops in the kernel once the first has run, a
            // microsecond each per 64 KiB committed), and the counters
            // take the one that starts the chunk.
            proof {
                lemma_slab_bytes(*self, self.nslabs);
                assert(process_regions().spec_nslabs(self.class as int) == self.nslabs);
            }
            if commit(lo, hi - lo, Ghost(self.class as int), Ghost(true)).is_ok() {
                crate::thp::advise_class(lo, hi - lo, addr == lo);
                return Some(());
            }
        }
        commit(addr, len, Ghost(self.class as int), Ghost(mode == crate::thp::Mode::Chunks)).ok()
    }

    /// Guard the slab (or page group, `len` bytes) at `addr`, before
    /// its commit: a guard region on its first page, so a stray write
    /// past the previous slab faults, `Some(true)`; the marker survives
    /// the `mprotect` that commits the rest. If the kernel refuses the
    /// marker (`EINVAL` on an `mlock`ed range), the whole slab is left
    /// `PROT_NONE`, a hard guard, `Some(false)`: already so when the
    /// cursor first reaches it, made so when a failed earlier attempt
    /// left it read-write. `None` when that `mprotect` fails too (a VMA
    /// split refused at `vm.max_map_count`): a resource failure (design
    /// 14.1), so the caller hands nothing out and the cursor stays, for
    /// a retry. Never memory next to a missing guard.
    fn install_guard(&self, addr: usize, page: usize, len: usize) -> (r: Option<bool>)
        requires
            crate::sys::may_syscall(),
        ensures
            r == Some(true) ==> sys::madvise_ok(addr, page, MADV_GUARD_INSTALL),
    {
        if guard(addr, page).is_ok() {
            return Some(true);
        }
        decommit(addr, len).ok()?;
        Some(false)
    }
}

} // verus!

verus! {

impl SlabTable {
    /// The bitmap word `index` of this table's committed bitmap at
    /// `bits_base` (the class line's copy of [`SlabTable::bits_base`]):
    /// the word of `bit`, the bit of slot `slot` of slab `slab`. One
    /// relaxed load, through the word's permission, inside its invariant
    /// ([`bit_word_held`] with no slot token): a set bit is not a guard
    /// slab's.
    ///
    /// # Safety
    /// `slab` is below a value the watermark held, so the word is in the
    /// committed bitmap.
    ///
    /// [`bit_word_held`]: SlabTable::bit_word_held
    #[inline(always)]
    pub(crate) unsafe fn bit_word(
        &self,
        bits_base: usize,
        index: usize,
        Ghost(slab): Ghost<usize>,
        Ghost(slot): Ghost<usize>,
        Ghost(bit): Ghost<usize>,
    ) -> (x: u64)
        requires
            bits_base == self.spec_bits_base(),
            slab < self.spec_nslabs(),
            slot < class_table()[self.spec_class() as int].nslots,
            bit == slab * class_table()[self.spec_class() as int].slab_bits + slot,
            index == bit / 64,
        ensures
            (x >> (bit % 64)) & 1 != 0 ==> !crate::layout::guard_slab(self.spec_class() as int, slab as int),
        opens_invariants [crate::bitmap::word_ns()]
    {
        let tracked none = None;
        // SAFETY: guaranteed by the caller.
        unsafe { self.bit_word_held(bits_base, index, Ghost(slab), Ghost(slot), Ghost(bit), Tracked(&none)) }
    }

    /// The owner word of `slab`'s group, through the class line's copies
    /// of [`SlabTable::owners_base`] and the group size: one relaxed load,
    /// through the word's permission, inside its invariant
    /// ([`load_owner`]). Without a witness that the group was handed out
    /// the value is unknown: the `free` fast path compares it with the
    /// home arena and takes the slow path, which reads it again
    /// ([`check_located`]), when it differs.
    ///
    /// # Safety
    /// `slab` is below a value the watermark held: the owner word of a
    /// handed-out group was committed and written before the watermark
    /// moved.
    ///
    /// [`check_located`]: SlabTable::check_located
    #[inline(always)]
    pub(crate) unsafe fn owner_from(&self, owners_base: usize, lg_group: u8, slab: usize) -> (r: u16)
        requires
            owners_base == self.spec_owners_base(),
            lg_group == self.spec_lg_group(),
            slab < self.spec_nslabs(),
        opens_invariants [owner_ns()]
    {
        proof {
            use_type_invariant(self);
            self.lemma_group_in_range(slab);
        }
        let word = addr_to_ptr(owners_base + (slab >> lg_group) * 2) as *const AtomicU16;
        // SAFETY: guaranteed by the caller.
        unsafe { load_owner(self, word, Ghost(slab), Tracked(None)) }
    }
}

} // verus!

verus! {

/// The slab table's commit lock ([`SlabTable::inner`]): its invariant is
/// [`TableInner::inv`], and its frame is the ghost geometry (page,
/// hand-out group, slab count, bits per slab), the marker authorities,
/// the descriptor array and the location of the watermark's ghost
/// variable, which `SlabTable::new` sets and no holder writes.
///
/// `new` (verified) builds the value with zero counts, uncommitted
/// tables, every slab's markers and descriptor permission, and no
/// watermark copy (`Lock::new` requires the invariant). Each holder
/// outside the tests (`SlabTable::mirror_watermark`, `guards`,
/// `take_group`, `commit_run` and `commit_slab`) ends with
/// `LockGuard::unlock`, which requires both, on every path to a return
/// (`api::tests::guards_end_in_unlock`). A panic aborts
/// (`panic = "abort"`) outside the tests, so it never releases. The test
/// helper `corrupt_owner_word` only commits through `Lazy::ensure`. The
/// fork handlers take the raw lock without touching the value.
///
/// `api::tests::lock_holders_match_inv_docs` checks that the functions
/// that take the guard are the holders named here.
impl crate::lock::LockSpec for TableInner {
    closed spec fn lock_spec_inv(self) -> bool {
        self.inv()
    }

    closed spec fn lock_spec_frame(init: Self, v: Self) -> bool {
        v.page == init.page && v.group == init.group && v.nslabs == init.nslabs && v.sb == init.sb && v.mloc == init.mloc
            && v.dbase == init.dbase && v.dstride == init.dstride && v.sloc == init.sloc && v.wloc == init.wloc
    }
}

/// The frame holds of every value with itself, as `LockSpec` requires of
/// a defined frame (`Lock::new` does not check it, and
/// `lock::axiom_acquired` claims it at the first acquisition).
proof fn lemma_table_frame_reflexive(v: TableInner)
    ensures
        crate::lock::lock_frame(v, v),
{
}

} // verus!
