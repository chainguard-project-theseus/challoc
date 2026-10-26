//! The per-class [`SlabTable`]: the class region's descriptor array,
//! class bitmap and group owner array, the hand-out watermark and the
//! commit lock; groups go to arenas here (`take_group`) and slabs are
//! committed here (`commit_run`, `commit_slab`).

use super::Inner;
use super::abort::{guard_free, invalid_free, invalid_pointer, never_committed, owner_mismatch};
use super::sets::Lazy;
use crate::arena::Published;
use crate::bitmap::AtomicBitmap;
use crate::layout::{CLASSES, Class, Regions};
#[cfg(verus_keep_ghost)]
use crate::layout::{NCLASSES, class_table, process_regions};
use crate::lock::{Lock, LockOrder, PAD_ALIGN, Padded, RawLock};
#[cfg(verus_keep_ghost)]
use crate::lock::{lock_init, lock_inv, rank_meta, rank_table};
use crate::meta::Meta;
use crate::purge;
#[cfg(verus_keep_ghost)]
use crate::slab::stride_of;
use crate::slab::{DESC_ALIGN, DescPerm, HEADER, SlabDesc, desc_stride};
use crate::sys::{self, Errno, MADV_GUARD_INSTALL, PROT_NONE, PROT_READ, PROT_WRITE};
use crate::vspec::addr::addr_to_ptr;
#[cfg(verus_keep_ghost)]
use crate::vspec::addr::cast_provenance;
use crate::vspec::atom::{
    SeenVal, UsizePerm, load_u16_at, load_usize_seen, new_usize, store_usize, store_usize_at,
};
use crate::vspec::check::check;
use crate::vspec::once::Once;
#[cfg(verus_keep_ghost)]
use crate::vspec::zero::ZeroRaw;
use core::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};
#[cfg(verus_keep_ghost)]
use vstd::arithmetic::power2::pow2;
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
use vstd::prelude::*;
use vstd::raw_ptr::PointsTo;
use vstd::resource::Loc;
use vstd::resource::set::{GhostPersistentSubset, GhostSubset};
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
    /// Guard slabs committed (never handed out).
    pub(super) guards: usize,
    /// Ghost: the table's page, hand-out group and slab count, fixed at
    /// `new`, for [`TableInner::inv`].
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) page: Ghost<usize>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) group: Ghost<usize>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    pub(super) nslabs: Ghost<usize>,
    /// A copy of the watermark the `free` fast path reads from its
    /// per-class line (`heap::ClassLine`), stored right after the
    /// watermark itself, so it is never ahead of it; null until
    /// [`SlabTable::mirror_watermark`].
    pub(super) mirror: *const AtomicUsize,
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

impl TableInner {
    /// The guard count stays below the slabs handed out, which stay below
    /// the largest slab count a region can have and are whole hand-out
    /// groups until the region's end; the three tables are committed in
    /// the process's page ([`lazy_ok`], what `Lazy::ensure` takes).
    pub(super) open spec fn inv(self) -> bool {
        &&& self.guards <= self.next_slab
        &&& self.next_slab <= usize::MAX >> 12
        &&& self.group@ >= 1
        &&& self.next_slab < self.nslabs@ ==> self.next_slab % self.group@ == 0
        &&& 1 <= self.page@ <= 0x8000_0000_0000_0000
        &&& self.page@ == crate::meta::process_page()
        &&& lazy_ok(self.descs, self.page@)
        &&& lazy_ok(self.bits, self.page@)
        &&& lazy_ok(self.owners, self.page@)
    }
}

/// Assumption (`lock.rs`): the commit lock's value keeps
/// [`TableInner::inv`] while the lock is free. `new` (verified) builds it
/// with zero counts and uncommitted tables ([`lemma_first_value`]).
///
/// Verified holders: `guards` changes nothing. `commit_run` and
/// `commit_slab` add one to `guards` for the guard slab they commit and
/// change no other field, so every conjunct but `guards <= next_slab`
/// still holds when their guard drops; that one rests on a premise their
/// proofs do not show: each slab below `next_slab` is reached once, by
/// the cursor of the one arena its group went to (`next_fresh_slab`
/// moves each arena's cursor forward through its own groups, under that
/// arena's class lock), so at most one guard is counted per guard slab
/// handed out.
///
/// Unverified holders: `take_group` moves `next_slab` to the end of the
/// group it hands out, a group boundary or the region's end, and commits
/// the tables through `Lazy::ensure`, which keeps [`lazy_ok`] when its
/// requirements hold; `mirror_watermark` changes no field this reads; the
/// test helper `corrupt_owner_word` only commits through `Lazy::ensure`.
pub(super) broadcast axiom fn axiom_table_lock_inv(v: TableInner)
    ensures
        #[trigger] lock_inv(v) == v.inv(),
;

/// The commit lock's first value, as [`SlabTable::new`] builds it (no
/// slab handed out, no guard, the three tables reserved and uncommitted),
/// keeps the lock's invariant: what `Lock::new` requires of it.
proof fn lemma_first_value(descs: Lazy, bits: Lazy, owners: Lazy, page: usize, group: usize)
    requires
        1 <= page <= 0x8000_0000_0000_0000,
        page == crate::meta::process_page(),
        group >= 1,
        lazy_ok(descs, page),
        lazy_ok(bits, page),
        lazy_ok(owners, page),
    ensures
        forall|v: TableInner|
            v.descs == descs && v.bits == bits && v.owners == owners && v.next_slab == 0 && v.guards == 0
                && v.page@ == page && v.group@ == group ==> #[trigger] lock_inv(v),
{
    broadcast use axiom_table_lock_inv;
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
    /// allocation bit of `slot` in `slab` (design section 4).
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
/// atomic's identity (`vspec::atom::usize_id`) and the table's slab
/// count: the copy is at most the slab count. Its writers,
/// [`SlabTable::mirror_watermark`] and `take_group`, store watermark
/// values, at most `nslabs`, inside it. The table's own watermark has
/// [`WatermarkPred`].
pub struct CommittedPred;

impl InvariantPredicate<(int, usize), UsizePerm> for CommittedPred {
    open spec fn inv(k: (int, usize), perm: UsizePerm) -> bool {
        &&& perm.id() == k.0
        &&& perm.value() <= k.1
    }
}

/// The invariant of one watermark copy (`heap::ClassLine`).
pub type CommittedInv = AtomicInvariant<(int, usize), UsizePerm, CommittedPred>;

/// A table's watermark invariant: [`CommittedPred`]'s, keyed also by the
/// location of the table's owner-word witness markers, with a persistent
/// set of them that has every slab's below the watermark. `take_group`,
/// its one writer, stores the end of the group it hands out, after that
/// group's owner word. A load returns a copy of the set
/// (`vspec::atom::load_usize_seen`).
pub struct WatermarkPred;

impl InvariantPredicate<(int, usize, Loc), SeenVal> for WatermarkPred {
    open spec fn inv(k: (int, usize, Loc), v: SeenVal) -> bool {
        &&& v.0.id() == k.0
        &&& v.0.value() <= k.1
        &&& v.1.id() == k.2
        &&& owner_marks(0, v.0.value() as int).subset_of(v.1@)
    }
}

/// The invariant of a table's watermark.
pub type WatermarkInv = AtomicInvariant<(int, usize, Loc), SeenVal, WatermarkPred>;

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

// SAFETY (all three): `addr..addr + len` is inside a class region this
// allocator reserved (the callers pass slab or page-group ranges below
// the region's end), so changing its protection or installing a guard
// affects only pages nothing else in the process maps.
verus! {

fn commit(addr: usize, len: usize) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
{
    unsafe { sys::mprotect(addr, len, PROT_READ | PROT_WRITE) }
}

fn decommit(addr: usize, len: usize) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
{
    unsafe { sys::mprotect(addr, len, PROT_NONE) }
}

fn guard(addr: usize, len: usize) -> Result<(), Errno>
    requires
        crate::sys::may_syscall(),
{
    unsafe { sys::madvise(addr, len, MADV_GUARD_INSTALL) }
}

} // verus!

/// `(*w).load(Relaxed)` of the owner word of a handed-out group: the
/// arena that owns the group's slabs.
///
/// # Safety
/// `w` is the owner word of a group below the watermark: committed and
/// written before the watermark moved.
#[inline(always)]
unsafe fn load_owner(w: *const AtomicU16) -> u16 {
    // SAFETY: guaranteed by the caller.
    unsafe { (*w).load(Ordering::Relaxed) }
}

/// `(*w).store(arena, Relaxed)` of the owner word of hand-out group
/// `group` of `t`, the group [`SlabTable::take_group`] hands out, through
/// its permission `perm`, using up `marks`, the owner-store markers of the
/// group's slabs (all four ghost, unused until the take_group proof takes
/// them): the only store to an owner word (the test helper
/// `corrupt_owner_word` aside).
///
/// # Safety
/// `w` is the owner word of the group at the watermark, committed.
#[allow(unused_variables)]
#[inline(always)]
unsafe fn store_owner(
    w: *const AtomicU16,
    arena: u16,
    t: Ghost<SlabTable>,
    group: Ghost<usize>,
    perm: Tracked<&mut PointsTo<AtomicU16>>,
    marks: Tracked<GhostSubset<int>>,
) {
    // SAFETY: `perm` is to the owner word at `w`, committed.
    unsafe { (*w).store(arena, Ordering::Relaxed) }
}

/// `index` itself, for [`SlabTable::is_set_unchecked`]'s word index. On
/// aarch64 it goes through an empty `asm!` (the next item): left alone,
/// LLVM rewrites `(bit >> 6) << 3` as `(bit >> 3) & !7`, a shift and a
/// mask ahead of the load on `free`'s dependent chain, where `bit >> 6`
/// and a scaled `ldr` is one instruction. x86_64 already scales the index
/// in the load, and this is the identity.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn word_index(index: usize) -> usize {
    index
}

/// [`word_index`] on aarch64: `index` through the empty `asm!`.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn word_index(index: usize) -> usize {
    let mut index = index;
    // SAFETY: an empty template; `index` comes out unchanged.
    unsafe {
        core::arch::asm!("/* {0} */", inout(reg) index, options(pure, nomem, nostack, preserves_flags));
    }
    index
}

/// [`SlabTable::take_group`]'s check that the watermark is a group
/// boundary. Inlined and `track_caller`, as [`group_geometry`]. A
/// `debug_assert!` of the equality: Verus does not support
/// `debug_assert_eq!` (its failure path is `core::panicking::assert_failed`).
#[inline(always)]
#[track_caller]
fn watermark_aligned(s: usize, lg_group: u8) {
    debug_assert!(
        s & ((1 << lg_group) - 1) == 0,
        "take_group: the watermark is group-aligned"
    );
}

verus! {

/// [`SlabTable::install_guard`]'s abort: the guard slab of `class` at `s`
/// could be neither guarded nor re-protected.
#[inline(always)]
fn guard_unprotectable(class: &u16, s: &usize) -> !
    requires
        crate::sys::may_abort_kernel(),
{
    sys::abort_kernel(format_args!("class {}: cannot re-protect guard slab {}", *class, *s))
}

} // verus!

verus! {

impl SlabTable {
    /// The geometry `new` sets up and never changes: the class, its slab
    /// bits, the power-of-two page and hand-out groups (a page group no
    /// larger than a hand-out group, both tiling the region, page groups
    /// on page boundaries), a region of `nslabs` slabs from `base` that
    /// ends inside the address space and is class `class`'s region of the
    /// process's regions ([`process_regions`]), a guard mask of the shape
    /// `2^k - 1` ([`mask_shape`]), the three arrays inside the address
    /// space, and the commit lock's rank and fixed ghost geometry. `new`
    /// proves it from its preconditions, and nothing writes these fields
    /// afterwards (`committed_slabs` and `inner`'s current value are not
    /// read here).
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
    /// guard (design section 5). `ord` is the caller's lock-order token,
    /// which the reservations take for the metadata lock. `_slot_once` and
    /// `_words_once` are the one-shot tokens (ghost, `vspec::once`) of the
    /// steps that fix the class's slot-token authority and its bitmap
    /// words' permissions, unused until the slab-table proofs take them.
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
        _slot_once: Tracked<Once>,
        _words_once: Tracked<Once>,
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
        ensures
            r matches Some(t) ==> {
                &&& t.spec_class() == class
                &&& t.spec_nslabs() == (1usize << regions.lg_region) >> class_table()[class as int].lg_slab
            },
            *final(ord) == *old(ord),
    {
        broadcast use axiom_table_lock_inv;
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
            stagger + (nslabs * cls.slab_bits()).div_ceil(8),
            align,
            Tracked(&mut braw),
            Tracked(ord),
        )?;
        let owners = Lazy::reserve(meta, stagger + nslabs.div_ceil(group) * 2, align, Tracked(&mut oraw), Tracked(ord))?;
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
                assert((nslabs * group) / (group as int) == nslabs);
                assert((nslabs + group - 1) / (group as int) <= (nslabs * group) / (group as int));
            }
            lemma_mask_shape(guard_interval);
            assert(lazy_ok(descs, page));
            assert(lazy_ok(bits, page));
            assert(lazy_ok(owners, page));
            lemma_groups_paged(regions.class_base[class as int], slabs_per_page, cls.lg_slab, regions.lg_page, page);
        }
        proof {
            lemma_first_value(descs, bits, owners, page, group);
        }
        let (committed_slabs, _cperm) = new_usize(0);
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
                mirror: core::ptr::null(),
            },
            Ghost(rank_table(class as int)),
        ));
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
proof fn lemma_tz_shl(x: usize, k: u64)
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
    pub fn raw_lock(&self) -> &RawLock {
        self.inner.raw()
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
    pub fn guard_mask(&self) -> u32 {
        self.guard_mask
    }
}

} // verus!

impl SlabTable {
    /// Keep `*mirror` equal to the hand-out watermark from now on: it is
    /// set to the current value here and stored after the watermark at
    /// every hand-out, under the commit lock, so a reader of the copy never
    /// sees a value the watermark has not held (`heap::ClassLine`).
    ///
    /// `_inv` (ghost) is the copy's invariant, which bounds it by the slab
    /// count ([`CommittedPred`]), and `_ord` the caller's lock-order token,
    /// both unused until the take_group proof takes them. The watermark is
    /// read, `Acquire`, inside its own invariant ([`WatermarkPred`],
    /// `vspec::atom::load_usize_seen`), which bounds it the same way.
    ///
    /// # Safety
    /// `mirror` must stay valid for as long as this table is used.
    pub unsafe fn mirror_watermark(
        &self,
        mirror: *const AtomicUsize,
        _inv: Tracked<&'static CommittedInv>,
        _ord: Tracked<&mut LockOrder>,
    ) {
        let mut g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
        let (wm, _) = load_usize_seen(
            &self.committed_slabs,
            Ordering::Acquire,
            &Tracked::<WatermarkInv>::assume_new(),
        );
        // SAFETY: guaranteed by the caller.
        unsafe { store_usize_at(mirror, wm, Ordering::Release, Tracked::assume_new()) };
        let gi = &mut *g;
        gi.mirror = mirror;
    }

    /// Start of the class bitmap (for `heap::ClassLine`).
    #[inline]
    pub fn bits_base(&self) -> usize {
        self.bits_base
    }

    /// Start of the group owner array (for `heap::ClassLine`).
    #[inline]
    pub fn owners_base(&self) -> usize {
        self.owners_base
    }

    /// `log2` of the hand-out group (for `heap::ClassLine`).
    #[inline]
    pub fn lg_group(&self) -> u8 {
        self.lg_group
    }

    /// Slabs per page group; 1 unless the page is larger than the slab.
    #[inline]
    pub fn slabs_per_page(&self) -> usize {
        self.slabs_per_page as usize
    }

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

    /// Number of slabs (including guard slabs) handed to an arena: their
    /// descriptors, bits and owner words exist (see the field).
    #[inline]
    pub fn committed_slabs(&self) -> usize {
        self.watermark(Ordering::Acquire)
    }

    /// `committed_slabs.load(order)`: the hand-out watermark, the one load
    /// of it outside [`take_group`]'s check, which reads it under the
    /// commit lock.
    ///
    /// [`take_group`]: SlabTable::take_group
    #[inline(always)]
    fn watermark(&self, order: Ordering) -> usize {
        self.committed_slabs.load(order)
    }
}

verus! {

impl SlabTable {
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
        self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare())).guards
    }
}

} // verus!

impl SlabTable {
    /// Descriptor of a handed-out slab. Aborts for a slab that was never
    /// handed out rather than faulting on its `PROT_NONE` descriptor page.
    #[inline]
    pub fn desc(&self, slab: usize) -> &SlabDesc {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        // SAFETY: `slab` is below the acquire-loaded watermark.
        unsafe { self.desc_unchecked(slab) }
    }

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

    /// Whether `slab` has been handed out, by a relaxed load of the
    /// watermark (see [`desc_if_committed`] for why that is enough on the
    /// `free` fast path).
    ///
    /// [`desc_if_committed`]: SlabTable::desc_if_committed
    #[inline(always)]
    pub fn is_handed_out(&self, slab: usize) -> bool {
        slab < self.watermark(Ordering::Relaxed)
    }

    /// Descriptor of `slab` without a watermark check.
    ///
    /// # Safety
    /// `slab` must be below a value `committed_slabs` has held: descriptors
    /// up to that count are committed and initialised (or zero, for guard
    /// slabs) before the watermark moves.
    #[inline(always)]
    unsafe fn desc_unchecked(&self, slab: usize) -> &SlabDesc {
        // SAFETY: guaranteed by the caller.
        unsafe { &*((self.desc_base + slab * self.desc_stride as usize) as *const SlabDesc) }
    }

    /// The slot bits of a handed-out slab (design section 4). Aborts for a
    /// slab that was never handed out.
    #[inline]
    pub fn bits(&self, slab: usize) -> AtomicBitmap<'_> {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        // SAFETY: `slab` is below the acquire-loaded watermark.
        unsafe { self.bits_unchecked(slab) }
    }

    /// # Safety
    /// As [`desc_unchecked`](SlabTable::desc_unchecked).
    #[inline]
    // Hot bitmap lookup: left as written.
    #[allow(clippy::manual_is_multiple_of)]
    unsafe fn bits_unchecked(&self, slab: usize) -> AtomicBitmap<'_> {
        let nbits = self.slab_bits as usize;
        let first = slab * nbits;
        let word = (self.bits_base + (first / 64) * 8) as *const AtomicU64;
        if nbits >= 64 {
            debug_assert!(first % 64 == 0);
            // SAFETY: the reservation holds `nslabs * slab_bits` bits and
            // the group's words were committed before the watermark moved.
            AtomicBitmap::from_words(
                unsafe { core::slice::from_raw_parts(word, nbits / 64) },
                Ghost::assume_new(),
                Ghost::assume_new(),
                Tracked::assume_new(),
            )
        } else {
            // SAFETY: as above, one word.
            AtomicBitmap::sub_word(
                unsafe { &*word },
                (first % 64) as u32,
                nbits,
                Ghost::assume_new(),
                Ghost::assume_new(),
                Tracked::assume_new(),
            )
        }
    }

    /// The `free` check's bit (design section 4): whether `slot` of `slab`
    /// is marked allocated (or cached). One relaxed load of the bitmap
    /// word, no bounds checks.
    ///
    /// # Safety
    /// `slab` must have been handed out ([`is_handed_out`]) and `slot` must
    /// be below the class's `nslots`.
    ///
    /// [`is_handed_out`]: SlabTable::is_handed_out
    #[inline(always)]
    pub unsafe fn is_set_unchecked(&self, slab: usize, slot: usize) -> bool {
        let bit = slab * self.slab_bits as usize + slot;
        let index = word_index(bit / 64);
        // SAFETY: the caller guarantees the word is inside the handed-out
        // part of the bitmap.
        let x = unsafe { self.bit_word_held(self.bits_base, index) };
        (x >> (bit % 64)) & 1 == 1
    }

    /// A relaxed load of word `index` of the class bitmap that starts at
    /// `bits_base` (this table's): the word [`is_set_unchecked`] tests.
    ///
    /// # Safety
    /// The word is inside the handed-out part of the bitmap.
    ///
    /// [`is_set_unchecked`]: SlabTable::is_set_unchecked
    #[inline(always)]
    unsafe fn bit_word_held(&self, bits_base: usize, index: usize) -> u64 {
        let word = (bits_base + index * 8) as *const AtomicU64;
        // SAFETY: guaranteed by the caller.
        unsafe { (*word).load(Ordering::Relaxed) }
    }

    /// Whether `slot` of `slab` is marked allocated (or cached); `false`
    /// for a slab that was never handed out. Never aborts.
    #[inline]
    pub fn is_allocated(&self, slab: usize, slot: usize) -> bool {
        debug_assert!(slot < self.class().nslots());
        // SAFETY: the watermark check, then a slot below `nslots`.
        self.is_handed_out(slab) && unsafe { self.is_set_unchecked(slab, slot) }
    }

    /// The arena that owns `slab`'s group, without a watermark check:
    /// one relaxed load of its owner word ([`load_owner`]).
    ///
    /// # Safety
    /// `slab` must have been handed out ([`is_handed_out`]).
    ///
    /// [`is_handed_out`]: SlabTable::is_handed_out
    #[inline(always)]
    unsafe fn owner_at(&self, slab: usize) -> u16 {
        // `owners_base` before the shift: in this order the `free` fast
        // path's owner test schedules its loads as the plain code does.
        let word = (self.owners_base + (slab >> self.lg_group) * 2) as *const AtomicU16;
        // SAFETY: the owner word of a handed-out group was committed and
        // written before the watermark moved.
        unsafe { load_owner(word) }
    }

    /// The arena that owns `slab`. Aborts for a slab that was never handed
    /// out.
    #[inline]
    pub fn owner(&self, slab: usize) -> u16 {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        // SAFETY: `slab` is below the acquire-loaded watermark.
        unsafe { self.owner_at(slab) }
    }
}

verus! {

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

} // verus!

impl SlabTable {
    /// Slab and slot of a pointer that must be a slot start in this class's
    /// region. Aborts otherwise.
    #[inline]
    pub fn locate(&self, p: *mut u8) -> (usize, usize) {
        let cls = self.class();
        let rel = (p as usize).wrapping_sub(self.base);
        if rel >= self.nslabs << cls.lg_slab {
            invalid_pointer(p as usize, self.class as usize, "outside the class region");
        }
        let (slot, valid) = cls.slot_of_offset(rel & cls.slab_mask());
        if !valid {
            invalid_pointer(p as usize, self.class as usize, "not a slot start");
        }
        (rel >> cls.lg_slab, slot)
    }

    /// The `free` check of a pointer already located (`Regions::classify`):
    /// aborts unless `slab` has been handed out and `slot`'s bit is set,
    /// with the pointer and its location; returns the owning arena.
    #[inline]
    pub fn check_located(&self, slab: usize, slot: usize, p: usize) -> u16 {
        self.check_live(slab, slot, p);
        // SAFETY: `check_live` returned, so `slab` is below the watermark.
        unsafe { self.owner_at(slab) }
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
    /// watermark compare and the bit test.
    ///
    /// [`check_located`]: SlabTable::check_located
    /// [`desc_if_committed`]: SlabTable::desc_if_committed
    #[inline(always)]
    pub fn check_live(&self, slab: usize, slot: usize, p: usize) {
        if !self.is_handed_out(slab) {
            never_committed(self.class as usize, slab);
        }
        debug_assert!(slot < self.class().nslots());
        // SAFETY: below a value the watermark held, slot below `nslots`.
        if !unsafe { self.is_set_unchecked(slab, slot) } {
            self.not_live(slab, slot, p);
        }
    }

    /// The abort of [`check_live`] for a clear bit: a guard slab by name,
    /// otherwise a slot that is not allocated.
    ///
    /// [`check_live`]: SlabTable::check_live
    #[cold]
    #[inline(never)]
    fn not_live(&self, slab: usize, slot: usize, p: usize) -> ! {
        if self.is_guard_slab(slab) {
            guard_free(p, self.class as usize, slab);
        }
        invalid_free(p, self.class as usize, slab, slot)
    }

    /// Hand the next group of slabs to `arena` (design section 4):
    /// commit their descriptors, bitmap words and owner word, initialise
    /// the descriptors (`Fresh`, owned by `arena`; guard slabs stay zero)
    /// and clear their bits, then publish the watermark. The set words of
    /// `sets`, the value behind `arena`'s class lock (which the caller
    /// holds), are committed under the commit lock up to the group's last
    /// slab, in `sets_page` units, before anything changes
    /// ([`Inner::ensure_sets`]); if that fails nothing has happened.
    /// Returns the group's slab range, `None` when the region or the
    /// metadata region is exhausted. The slabs themselves are committed by
    /// the arena as its cursor reaches them, a run at a time
    /// ([`commit_run`]). `_inited` and `_dout` (ghost) receive the group's
    /// bitmap markers and descriptor permissions, `_pubw` (ghost) is the
    /// registry's record of `arena`, and `_ord` is the caller's lock-order
    /// token, all unused until the take_group proof takes them.
    ///
    /// [`commit_run`]: SlabTable::commit_run
    // Verus needs the explicit reborrow and a loop with an invariant.
    #[allow(
        clippy::explicit_auto_deref,
        clippy::while_let_loop,
        clippy::too_many_arguments
    )]
    pub(super) fn take_group(
        &self,
        meta: &Meta,
        arena: u16,
        sets: &mut Inner,
        sets_page: usize,
        _inited: Tracked<&mut GhostPersistentSubset<int>>,
        _dout: Tracked<&mut Map<nat, DescPerm>>,
        _pubw: Tracked<&'static Published>,
        _ord: Tracked<&mut LockOrder>,
    ) -> Option<(usize, usize)> {
        let cls = self.class();
        let page = 1usize << self.lg_page;
        let class = self.class as usize;
        let stagger = desc_stagger(class);
        let mut g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
        let s = self.next_slab_locked(&*g, &*sets);
        if s >= self.nslabs {
            return None;
        }
        watermark_aligned(s, self.lg_group);
        let (_, end) = self.group_range(s >> self.lg_group);
        if purge::check_zero() && s != self.committed_slabs.load(Ordering::Relaxed) {
            crate::sys::abort_fmt(format_args!(
                "check_zero: class {class}: take_group at next_slab {s} but the watermark is {}",
                self.committed_slabs.load(Ordering::Relaxed)
            ));
        }
        g.descs
            .ensure(
                meta,
                page,
                stagger + end * desc_stride(class),
                Tracked::assume_new(),
            )
            .ok()?;
        g.bits
            .ensure(
                meta,
                page,
                stagger + (end * cls.slab_bits()).div_ceil(8),
                Tracked::assume_new(),
            )
            .ok()?;
        g.owners
            .ensure(
                meta,
                page,
                stagger + ((end - 1) >> self.lg_group) * 2 + 2,
                Tracked::assume_new(),
            )
            .ok()?;
        sets.ensure_sets(meta, sets_page, end - 1, Tracked::assume_new())
            .ok()?;
        // The owner word and the descriptors' arena field record the same
        // fact (design section 4) and are written together only here, so
        // a group about to be handed out must show them agreeing: both
        // zero for never-touched memory. A disagreement is a wild write
        // into one of the two arrays and aborts before the group is used.
        // SAFETY: the owner page for the group was committed above.
        let owner_word = unsafe { self.owner_word(s >> self.lg_group) };
        // SAFETY: as above.
        let previous = unsafe { load_u16_at(owner_word, Ordering::Relaxed, Tracked::assume_new()) };
        // `for slab in s..end`, spelled out (both loops), because Verus
        // rejects `continue` in a `for` loop. The second loop keeps its body
        // in the `Some` arm, as the `for` desugaring does.
        let mut slabs = s..end;
        loop {
            let slab = match slabs.next() {
                Some(slab) => slab,
                None => break,
            };
            if self.is_guard_slab(slab) {
                continue;
            }
            // SAFETY: the descriptor page for `slab` was committed above.
            let recorded =
                unsafe { SlabDesc::at(self.desc_base, class, slab, Tracked::assume_new()).arena() };
            if recorded != previous {
                owner_mismatch(class, s >> self.lg_group, slab, previous, recorded);
            }
        }
        let gi = &mut *g;
        let mut slabs = s..end;
        loop {
            match slabs.next() {
                None => break,
                Some(slab) => {
                    if self.is_guard_slab(slab) {
                        continue;
                    }
                    // SAFETY: the descriptor and bitmap pages for `slab` were
                    // committed above.
                    unsafe {
                        if purge::check_zero() {
                            let d =
                                SlabDesc::at(self.desc_base, class, slab, Tracked::assume_new());
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
                        SlabDesc::at(self.desc_base, class, slab, Tracked::assume_new())
                            .init(arena, Tracked::assume_new());
                        let _ = self.bits_unchecked(slab).init(
                            cls.nslots(),
                            Ghost::assume_new(),
                            Tracked::assume_new(),
                        );
                    }
                }
            }
        }
        // SAFETY: the owner page for the group was committed above.
        unsafe {
            store_owner(
                owner_word,
                arena,
                Ghost::assume_new(),
                Ghost::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
        };
        gi.next_slab = end;
        store_usize(
            &self.committed_slabs,
            end,
            Ordering::Release,
            Tracked::assume_new(),
        );
        if !gi.mirror.is_null() {
            // SAFETY: `mirror_watermark`'s caller keeps the copy alive as
            // long as the table.
            unsafe { store_usize_at(gi.mirror, end, Ordering::Release, Tracked::assume_new()) };
        }
        // `thp:auto` (design 7, 17; `thp.rs`): the group's range, still
        // reserved `PROT_NONE`, is advised once here, so the slabs the
        // arena commits from it fault in as folios of the enabled size.
        // Off, and under `always` (whose PMD chunks `commit_slab` advises),
        // this is a load and a compare.
        if crate::thp::class_mode(cls.slab_size()) == crate::thp::Mode::Folios {
            crate::thp::advise_class(self.slab_addr(s), (end - s) << cls.lg_slab, true);
        }
        Some((s, end))
    }

    /// The next slab to hand out, read under the commit lock (`t`, its
    /// value) with a class lock over this table held by the caller (`v`,
    /// its value, which the take_group proof reads).
    #[allow(unused_variables)]
    #[inline(always)]
    fn next_slab_locked(&self, t: &TableInner, v: &Inner) -> usize {
        t.next_slab
    }

    /// The owner word of hand-out group `group`.
    ///
    /// # Safety
    /// `group` is a group of the region; the word is committed only once
    /// the group is handed out, or under the commit lock in
    /// [`take_group`](SlabTable::take_group).
    #[inline]
    pub(super) unsafe fn owner_word(&self, group: usize) -> *const AtomicU16 {
        addr_to_ptr(self.owners_base + group * 2) as *const AtomicU16
    }
}

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
    pub(super) fn starts_run(&self, s: usize) -> bool
        requires
            s < self.spec_nslabs(),
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
    /// stalls every thread's page faults behind it (design section 5). The
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
            crate::sys::may_abort_kernel(),
            crate::sys::may_syscall(),
            s < limit <= self.spec_nslabs(),
            old(ord).allows(rank_table(0)),
        ensures
            r matches Some(usable) ==> usable == !self.spec_is_guard(s),
            *final(ord) == *old(ord),
    {
        broadcast use axiom_table_lock_inv;
        if !self.runs() {
            return self.commit_slab(s, Tracked(ord));
        }
        proof {
            use_type_invariant(self);
        }
        let cls = self.class();
        let guard = self.is_guard_slab(s);
        let end = self.run_end(s, limit);
        commit(self.slab_addr(s), (end - s) << cls.lg_slab).ok()?;
        if guard {
            let page = 1usize << self.lg_page;
            self.install_guard(s, self.slab_addr(s), page, cls.slab_size());
            proof {
                assert(usize::MAX >> 12 == 0xF_FFFF_FFFF_FFFFusize) by (bit_vector);
            }
            // `guards <= next_slab` after the increment is the cross-arena
            // premise `axiom_table_lock_inv` names, not shown here.
            self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare())).guards += 1;
            return Some(false);
        }
        Some(true)
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
        end
    }

    /// Commit slab `s` of a group handed to the caller's arena: make its
    /// memory (or its whole page group, when the page is larger than the
    /// slab and `s` starts one) read-write, or install the guard when `s`
    /// is a guard slab. `Some(true)` for a slab ready to allocate from,
    /// `Some(false)` for a guard slab, `None` when `mprotect` fails (the
    /// slab stays uncommitted and may be retried). `ord` is the caller's
    /// lock-order token for the commit lock.
    // Refill path: left as written so its codegen does not move.
    #[allow(clippy::manual_is_multiple_of)]
    pub(super) fn commit_slab(&self, s: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<bool>)
        requires
            crate::sys::may_abort_kernel(),
            crate::sys::may_syscall(),
            s < self.spec_nslabs(),
            old(ord).allows(rank_table(0)),
        ensures
            r matches Some(usable) ==> usable == !self.spec_is_guard(s),
            *final(ord) == *old(ord),
    {
        broadcast use axiom_table_lock_inv;
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
            assert(usize::MAX >> 12 == 0xF_FFFF_FFFF_FFFFusize) by (bit_vector);
        }
        let first_of_page_group = s % spp == 0;
        if first_of_page_group {
            self.commit_page_group(addr, len, mode)?;
        }
        if self.is_guard_slab(s) {
            // Under `thp:always` with PMD chunks the marker is not
            // installed: a guard marker (or a `PROT_NONE` page) inside a
            // 2 MiB page splits it back into small pages the moment the
            // arena reaches the guard slab (measured: every chunk's huge
            // page was gone by the time the slab was in use), so the slab
            // is left unused and unguarded; design 17 records the trade.
            if first_of_page_group && mode != crate::thp::Mode::Chunks {
                self.install_guard(s, addr, page, len);
            }
            // As in `commit_run`: `guards <= next_slab` afterwards rests on
            // the cross-arena premise `axiom_table_lock_inv` names.
            g.guards += 1;
            return Some(false);
        }
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
            if commit(lo, hi - lo).is_ok() {
                crate::thp::advise_class(lo, hi - lo, addr == lo);
                return Some(());
            }
        }
        commit(addr, len).ok()
    }

    /// Guard slab `s`, just committed read-write at `addr` (so the VMA
    /// merges with its neighbours): a guard region on its first page, so
    /// a stray write past the previous slab faults. If the install
    /// fails, fall back to leaving the whole slab (or page group, `len`)
    /// `PROT_NONE`: a hard guard at the cost of a VMA split. A slab that
    /// can be neither guarded nor re-protected aborts.
    fn install_guard(&self, s: usize, addr: usize, page: usize, len: usize)
        requires
            crate::sys::may_abort_kernel(),
            crate::sys::may_syscall(),
    {
        if guard(addr, page).is_ok() {
            return;
        }
        if decommit(addr, len).is_err() {
            guard_unprotectable(&self.class, &s);
        }
    }
}

} // verus!
