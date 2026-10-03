//! The per-class [`SlabTable`]: the class region's descriptor array,
//! class bitmap and group owner array, the hand-out watermark and the
//! commit lock; groups go to arenas here (`take_group`) and slabs are
//! committed here (`commit_slab`).

use super::Inner;
use super::abort::{guard_free, invalid_free, invalid_pointer, never_committed, owner_mismatch};
use super::sets::Lazy;
use crate::bitmap::{AtomicBitmap, WordInv};
#[cfg(verus_only)]
use crate::bitmap::{Slots, fresh_mark, fresh_marks, init_mark, init_marks, words_wf};
#[cfg(verus_only)]
use crate::bitmap::{has_bit, subset};
use crate::layout::{CLASSES, Class, Regions};
#[cfg(verus_only)]
use crate::layout::{NCLASSES, class_table, process_regions};
use crate::lock::{Lock, LockOrder, PAD_ALIGN, Padded, RawLock};
#[cfg(verus_only)]
use crate::lock::{rank_meta, rank_table};
use crate::meta::Meta;
use crate::purge;
use crate::slab::{DESC_ALIGN, SlabDesc, desc_stride};
use crate::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};
use crate::sys::{self, Errno, MADV_GUARD_INSTALL, PROT_NONE, PROT_READ, PROT_WRITE};
use crate::vspec::addr::addr_to_ptr;
#[cfg(verus_only)]
use crate::vspec::addr::cast_provenance;
#[cfg(verus_only)]
use crate::vspec::lock::{lock_frame, lock_init, lock_inv};
#[cfg(verus_only)]
use crate::vspec::word::{WordPerm, word_addr};
#[cfg(verus_only)]
use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;
use vstd::raw_ptr::{PointsTo, ptr_ref};
#[cfg(verus_only)]
use vstd::raw_ptr::{PointsToRaw, PtrData, ptr_mut_from_data};
use vstd::resource::Loc;
use vstd::resource::set::{GhostPersistentSubset, GhostSetAuth, GhostSubset};
#[cfg(verus_only)]
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
    /// `new` ([`axiom_table_lock_frame`]), for [`TableInner::inv`].
    #[allow(dead_code)]
    pub(super) page: Ghost<usize>,
    #[allow(dead_code)]
    pub(super) group: Ghost<usize>,
    #[allow(dead_code)]
    pub(super) nslabs: Ghost<usize>,
    /// Ghost: the class's bits per slab and the bitmap markers' authority
    /// (`bitmap.rs`, "Slot tokens"), fixed at `new`, and the init markers
    /// of every bit of the slabs not yet handed out, which `take_group`
    /// takes one slab at a time (for `AtomicBitmap::init`, or as they are
    /// for a guard slab).
    #[allow(dead_code)]
    pub(super) sb: Ghost<usize>,
    #[allow(dead_code)]
    pub(super) mloc: Ghost<Loc>,
    #[allow(dead_code)]
    pub(super) fresh: Tracked<GhostSubset<int>>,
}

/// The facts [`Lazy::ensure`] needs of a table committed in `page` units,
/// which it keeps.
pub(super) open spec fn lazy_ok(l: Lazy, page: usize) -> bool {
    &&& l.spec_len() <= 0x4000_0000_0000_0000
    &&& l.spec_committed() <= l.spec_len() + page
    &&& l.spec_base() + l.spec_len() + page <= usize::MAX
}

impl TableInner {
    /// `self` is `o` but for the init markers.
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
    }

    /// The guard count stays below the slabs handed out, which stay below
    /// the largest slab count a region can have and are whole hand-out
    /// groups until the region's end; the three tables are committed in
    /// page units.
    pub(super) open spec fn inv(self) -> bool {
        &&& self.guards <= self.next_slab
        &&& self.next_slab <= usize::MAX >> 12
        &&& self.group@ >= 1
        &&& self.next_slab < self.nslabs@ ==> self.next_slab % self.group@ == 0
        &&& 1 <= self.page@ <= 0x8000_0000_0000_0000
        &&& lazy_ok(self.descs, self.page@)
        &&& lazy_ok(self.bits, self.page@)
        &&& lazy_ok(self.owners, self.page@)
        &&& self.fresh@.id() == self.mloc@
        &&& init_marks(self.next_slab * self.sb@, self.nslabs@ * self.sb@).subset_of(self.fresh@@)
    }
}

/// Assumption (`lock.rs`): the commit lock's value keeps
/// [`TableInner::inv`] while the lock is free. `new` builds it with zero
/// counts and uncommitted tables; `take_group` (verified; the handed-out
/// path asserts it, and a `?` return after a partial commit keeps it
/// through `Lazy::ensure`'s postcondition)
/// moves `next_slab` to the end of the group it hands out, which is a
/// group boundary or the region's end, at most `nslabs`, below
/// `usize::MAX >> 12` (`lemma_nslabs_bound`), and commits the tables
/// through `Lazy::ensure`, which keeps [`lazy_ok`]; `commit_slab` adds
/// one guard per guard slab it commits, which keeps the count at most
/// `next_slab` only because every slab below `next_slab` is committed
/// once, by the arena its group went to (`next_fresh_slab`): assumed,
/// not proved. The init markers it holds cover the slabs from
/// `next_slab` on: `take_group` takes the markers of the group it hands
/// out, below the `next_slab` it publishes (for `AtomicBitmap::init`, or
/// for the arena to keep for a guard slab), and nothing else moves them. `guards` only reads; the test
/// helper `corrupt_owner_word` (unverified) only commits through
/// `Lazy::ensure`.
pub(super) broadcast axiom fn axiom_table_lock_inv(v: TableInner)
    ensures
        #[trigger] lock_inv(v) == v.inv(),
;

/// The commit lock's ghost geometry and marker authority never change:
/// nothing writes them.
pub(super) broadcast axiom fn axiom_table_lock_frame(init: TableInner, v: TableInner)
    ensures
        #[trigger] lock_frame(init, v) == (v.page == init.page && v.group == init.group && v.nslabs == init.nslabs
            && v.sb == init.sb && v.mloc == init.mloc),
;

/// The descriptor of `slab` in `t`'s descriptor array: what
/// [`SlabTable::desc`] returns, the one its permission
/// ([`SlabTable::perms_ok`]) is to. Names a descriptor so that a contract
/// can say which slab a `&SlabDesc` belongs to; nothing about its
/// contents.
pub closed spec fn desc_at(t: SlabTable, slab: usize) -> SlabDesc {
    t.desc_perms@[slab as int].value()
}

/// The owner word of hand-out group `group` in `t`'s owner array: what
/// [`SlabTable::owner_word`] returns, the one its permission is to. Names
/// the word, as [`desc_at`] names a descriptor; nothing about its value.
pub(super) closed spec fn owner_word_at(t: SlabTable, group: usize) -> AtomicU16 {
    t.owner_perms@[group as int].value()
}

/// The arena that owns slab `slab` of class `c`'s region: the index
/// [`SlabTable::take_group`] stores in the owner word of the slab's
/// hand-out group as it hands the group out, under the commit lock and
/// before the watermark passes the group. Fixed for the life of the
/// process from then on (arena isolation, `verus/THEOREMS.md`): the word
/// is written once, by that store ([`store_owner`]), and every load of a
/// handed-out group's word returns it ([`load_owner`]). Uninterpreted:
/// those two trusted wrappers are all that state it. Keyed by the class
/// alone, which rests on there being one table per class per process
/// (the premise [`axiom_slot_loc`] names; tests that build several
/// tables, or corrupt an owner word, are outside it).
pub uninterp spec fn slab_owner(c: int, slab: int) -> u16;

/// The arena that owns the slab of class `c`'s region holding address
/// `p` ([`slab_owner`] of [`Regions::slab_index`]): for a slot, the arena
/// it was claimed from and the only one it is released into.
pub open spec fn slot_owner(c: int, p: usize) -> u16 {
    slab_owner(c, process_regions().slab_index(c, p))
}

/// `w.load(Relaxed)` of the owner word of handed-out slab `slab`'s group:
/// the arena that owns the slab. Trusted, the read half of the
/// [`slab_owner`] premise: the caller has seen the watermark past `slab`
/// (the safety condition of [`SlabTable::owner_at`], its one caller), so
/// the group was handed out, its word was stored once before the
/// watermark moved ([`store_owner`]) and nothing writes it afterwards.
/// Verus does not check that condition (`owner_at` requires only `slab <
/// nslabs`): its callers, `owner` (after the watermark test),
/// `check_located` (after `check_live`) and `owner_unchecked` (after
/// `is_handed_out`), are what meet it. Sequential consistency, as for
/// every atomic here (`verus/WEAKMEM.md`).
#[verifier::external_body]
#[inline(always)]
fn load_owner(w: &AtomicU16, Ghost(t): Ghost<SlabTable>, Ghost(slab): Ghost<usize>) -> (r: u16)
    requires
        slab < t.spec_nslabs(),
        *w == owner_word_at(t, slab >> t.lg_group),
    ensures
        r == slab_owner(t.spec_class() as int, slab as int),
    opens_invariants none
{
    w.load(Ordering::Relaxed)
}

/// `w.store(arena, Relaxed)` of the owner word of hand-out group `group`:
/// from here on [`slab_owner`] of every slab of the group is `arena`.
/// Trusted, the write half of the [`slab_owner`] premise: called only by
/// [`SlabTable::take_group`], under the commit lock, for the group at the
/// watermark, past every fallible step, after which it moves the
/// watermark past the group; so each group's word is stored once, before
/// any slab of it is handed out. Verus does not check the "once" (a
/// second call with another arena would contradict the first): the
/// wrapper is private and has that one call site.
#[verifier::external_body]
#[inline(always)]
fn store_owner(w: &AtomicU16, arena: u16, Ghost(t): Ghost<SlabTable>, Ghost(group): Ghost<usize>)
    requires
        *w == owner_word_at(t, group),
    ensures
        forall|x: int| t.spec_group_range(group).0 <= x < t.spec_group_range(group).1
            ==> #[trigger] slab_owner(t.spec_class() as int, x) == arena,
{
    w.store(arena, Ordering::Relaxed)
}

/// `p` is an initialised permission to the `T` at `addr`, through a
/// pointer of the provenance an address cast picks up
/// ([`addr_to_ptr`](crate::vspec::addr::addr_to_ptr)).
pub open spec fn perm_at<T>(p: PointsTo<T>, addr: int) -> bool {
    &&& p.ptr() == ptr_mut_from_data::<T>(PtrData { addr: addr as usize, provenance: cast_provenance(), metadata: () })
    &&& p.is_init()
}

// The layouts the permissions below are carved with (Verus checks them
// against the compiler's).
global layout SlabDesc is size == 16, align == 16;
global layout AtomicU64 is size == 8, align == 8;
global layout AtomicU16 is size == 2, align == 2;

/// The layouts above, for the permissions `SlabTable::new` carves.
proof fn lemma_layouts()
    ensures
        vstd::layout::size_of::<SlabDesc>() == 16,
        vstd::layout::align_of::<SlabDesc>() == 16,
        vstd::layout::size_of::<AtomicU64>() == 8,
        vstd::layout::align_of::<AtomicU64>() == 8,
        vstd::layout::size_of::<AtomicU16>() == 2,
        vstd::layout::align_of::<AtomicU16>() == 2,
{
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
    /// Ghost (no bytes): the class bitmap's slot-token authority
    /// (`bitmap.rs`, "Slot tokens"), and the invariant of each bitmap word
    /// ([`WordInv`]), which holds the word's permission and the tokens of
    /// its clear slots.
    pub(super) slot_auth: Tracked<GhostSetAuth<int>>,
    pub(super) word_invs: Tracked<Map<int, WordInv>>,
    /// Ghost (no bytes): the authority of the bitmap words' markers
    /// (`bitmap.rs`, "Slot tokens"), which record which slabs `init` has
    /// run on.
    pub(super) mark_auth: Tracked<GhostSetAuth<int>>,
    /// Ghost (no bytes): the permissions to every slab's descriptor, every
    /// class bitmap word and every hand-out group's owner word, carved by
    /// `new` from the three reservations ([`SlabTable::perms_ok`]); the
    /// accessors read through them.
    pub(super) desc_perms: Tracked<Map<int, PointsTo<SlabDesc>>>,
    pub(super) word_perms: Tracked<Map<int, PointsTo<AtomicU64>>>,
    pub(super) owner_perms: Tracked<Map<int, PointsTo<AtomicU16>>>,
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
const _: () = assert!(core::mem::size_of::<SlabTable>() % PAD_ALIGN == 0);
const _: () = assert!(core::mem::size_of::<SlabTable>() % 512 != 0);

// SAFETY: `Lazy` holds an address into the metadata region that is only
// ever used under the lock.
unsafe impl Send for TableInner {}

verus! {

// SAFETY (all three): `addr..addr + len` is inside a class region this
// allocator reserved (the callers pass slab or page-group ranges below
// the region's end), so changing its protection or installing a guard
// affects only pages nothing else in the process maps.
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

verus! {

impl SlabTable {
    /// Create the table for `class`. Reserves the descriptor array, the
    /// class bitmap and the owner array in `meta`; returns `None` if the
    /// metadata region cannot hold them. `guard_interval` of `N > 0` makes
    /// every `N`-th page group a guard group; `N` must be a power of two
    /// (`config.rs` rounds a configured value up, `heap::guard_interval_for`
    /// scales by a power of two), so that the sized fast path's test is a
    /// mask; callers must pass 0 when `MADV_GUARD_INSTALL` is unavailable,
    /// since a `PROT_NONE` slab would split the region into one VMA per
    /// guard (design section 5).
    ///
    /// The asserts on the arguments are preconditions; the caller also
    /// guarantees well-formed, separated regions (which give a page no
    /// larger than a class region and a class region that ends at or below
    /// `2^57`, inside a user-space mapping, `Regions::reserve`) and that
    /// they are the process's ([`process_regions`](crate::layout::process_regions)),
    /// which is what the type invariant needs of `regions`.
    #[verifier::spinoff_prover]
    pub fn new(meta: &Meta, regions: &Regions, class: usize, guard_interval: u32, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<SlabTable>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            regions.wf(),
            regions.separated(),
            *regions == process_regions(),
            class < NCLASSES,
            guard_interval != 1,
            guard_interval == 0 || exists|k: nat| k < 32 && guard_interval as nat == #[trigger] pow2(k),
            old(ord).allows(rank_meta()),
        ensures
            r matches Some(t) ==> {
                &&& t.spec_class() == class
                &&& t.spec_nslabs() == (1usize << regions.lg_region) >> class_table()[class as int].lg_slab
                &&& meta.is_process() ==> t.meta_ok()
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
            assert(regions.class_base[c] as int % (crate::layout::MAX_SLAB as int) == 0);
            assert((1usize << 57usize) == 0x200_0000_0000_0000usize) by (bit_vector);
            assert(regions.class_base[class as int] + (1usize << regions.lg_region) <= 1usize << 57);
            assert(regions.lg_page <= regions.lg_region);
        }
        assert!(class < CLASSES.table().len(), "class: index out of range");
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
            assert(crate::layout::LG_MIN_SLAB == 12 && crate::layout::LG_MAX_SLAB == 22 && crate::layout::LG_REGION_PREFERRED == 38);
            let l = cls.lg_slab;
            assert((1usize << l) == (1usize << (l as u32))) by (bit_vector);
            lemma_new_geometry(regions.lg_page, l as u32, regions.lg_region, page, 1usize << l, nslabs, slabs_per_page);
        }
        assert!(slabs_per_page.is_power_of_two() && nslabs % slabs_per_page == 0);
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
        // The descriptor permissions need whole, aligned headers per stride
        // (`slab::strides_of` makes the stride 16, 32 or 64; the table it
        // fills is not visible to the verifier).
        crate::vspec::check::check(stride >= 16 && stride % 16 == 0, "class: descriptor stride is not whole headers");
        let (descs, _draw) = Lazy::reserve_raw(meta, stagger + nslabs * stride, align, Tracked(ord));
        let descs = descs?;
        let (bits, _braw) = Lazy::reserve_raw(meta, stagger + (nslabs * cls.slab_bits()).div_ceil(8), align, Tracked(ord));
        let bits = bits?;
        let (owners, _oraw) = Lazy::reserve_raw(meta, stagger + nslabs.div_ceil(group) * 2, align, Tracked(ord));
        let owners = owners?;
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
        proof {
            axiom_slot_loc(class as int, slot_auth.id(), geo.glg, geo.gmask);
        }
        let ghost nwords = (geo.nbits + 63) / 64;
        // The markers: the fresh ones of every bit for the word
        // invariants, the init ones for the commit lock.
        let tracked (mark_auth, mut marks) = GhostSetAuth::<int>::new(
            fresh_marks(0, 64 * nwords).union(init_marks(0, 64 * nwords)));
        let tracked imarks = marks.split(init_marks(0, 64 * nwords));
        let ghost slots = Slots {
            loc: slot_auth.id(),
            mloc: mark_auth.id(),
            sb: geo.sb,
            nslots: geo.nslots,
            nbits: geo.nbits,
            base: geo.base,
            slab_size: geo.slab_size,
            size: geo.size,
            glg: geo.glg,
            gmask: geo.gmask,
        };
        let ghost bits_base = (bits.spec_base() + stagger) as usize;
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
        }
        let tracked perms = axiom_fresh_words(bits_base, nwords as nat);
        let tracked word_invs = crate::bitmap::fresh_word_invs(perms, all, marks, slots, bits_base, nwords as nat);
        // The permissions to every descriptor, bitmap word and owner word,
        // carved from the three reservations' (`SlabTable::perms_ok`).
        proof {
            assert(stride == (stride as u32) as usize);
        }
        let tracked (dmap, wmap, omap) = table_perms(
            _draw.get(), _braw.get(), _oraw.get(), descs.spec_base() as int, bits.spec_base() as int, owners.spec_base() as int,
            align, stagger as int, stride as int, nslabs as int, cls.slab_bits as int, group as int, class as int);
        let bits_base = bits.start() + stagger;
        let owners_base = owners.start() + stagger;
        let desc_base = descs.start() + stagger;
        let inner = Padded(Lock::new(TableInner {
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
            fresh: Tracked(imarks),
        }, Ghost(rank_table(class as int))));
        proof {
            assert(lock_init(inner.0).page@ == 1usize << regions.lg_page);
            assert(lock_init(inner.0).group@ == group);
            assert(lock_init(inner.0).nslabs@ == nslabs);
            assert(lock_init(inner.0).sb@ == cls.slab_bits as usize);
            assert(lock_init(inner.0).mloc@ == mark_auth.id());
            assert(inner.0.rank() == rank_table(class as int));
            assert(words_wf(word_invs, slots, bits_base, nwords));
            assert(slots.loc == slot_loc(class as int));
            assert(crate::layout::guard_geom(class as int) == (geo.glg, geo.gmask));
            assert(regions.class_base[class as int] == process_regions().class_base[class as int]);
            assert(nslabs as int == process_regions().spec_nslabs(class as int));
        }
        Some(SlabTable {
            bits_base,
            owners_base,
            committed_slabs: AtomicUsize::new(0),
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
            word_invs: Tracked(word_invs),
            mark_auth: Tracked(mark_auth),
            desc_perms: Tracked(dmap),
            word_perms: Tracked(wmap),
            owner_perms: Tracked(omap),
        })
    }
}

/// `assert!` of [`SlabTable::new`]'s hand-out group, `max(slabs_per_line,
/// slabs_per_page)`: a power of two that is whole page groups. Inlined
/// and `track_caller`, so the caller keeps the code, the message and the
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
        crate::meta::is_pow2(group),
        group % slabs_per_page == 0,
{
    proof {
        let k = choose|k: nat| k < 64 && group as nat == #[trigger] pow2(k);
        crate::meta::lemma_pow2_bits(slabs_per_page);
        vstd::arithmetic::div_mod::lemma_mod_self_0(slabs_per_page as int);
        crate::layout::lemma_pow2_divides_pow2(slabs_per_page, slabs_per_page as int, k);
    }
    assert!(group.is_power_of_two() && group % slabs_per_page == 0);
}


/// `1 << k` has `k` trailing zeros.
pub proof fn lemma_tz_shl(x: usize, k: u64)
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
{
    assert(nslabs == 1usize << sub(lr, ls) && nslabs <= 0x400_0000) by (bit_vector)
        requires nslabs == (1usize << lr) >> ls, 12 <= ls <= lr, lr <= 38;
    if lp >= ls {
        let k: u32 = sub(lp, ls);
        let m: u32 = sub(lr, ls);
        assert(page / ss == 1usize << k) by (bit_vector)
            requires page == 1usize << lp, ss == 1usize << ls, k == sub(lp, ls), ls <= lp, lp <= 38;
        assert((1usize << k) >= 1 && (1usize << m) % (1usize << k) == 0 && (1usize << k) <= (1usize << m)) by (bit_vector)
            requires k <= m, m <= 38;
        crate::meta::lemma_is_pow2_shl(k as usize);
        assert((1usize << k) == (1usize << (k as usize))) by (bit_vector);
    } else {
        assert(page / ss == 0) by (bit_vector)
            requires page == 1usize << lp, ss == 1usize << ls, lp < ls, ls <= 22;
        crate::meta::lemma_is_pow2_one();
        assert((1usize << sub(lr, ls)) >= 1) by (bit_vector) requires sub(lr, ls) <= 38;
    }
}

} // verus!

verus! {

/// The permissions [`SlabTable::new`] carves from its three reservations
/// (`draw`, `braw`, `oraw`, from `Lazy::reserve_raw`, at `d`, `b` and `o`,
/// aligned to `align`, each starting with `stagger` bytes): to the
/// descriptors of `nslabs` slabs every `stride` bytes, to the class bitmap
/// words that hold `nslabs * sb` bits, and to the owner words of the
/// hand-out groups of `group` slabs; initialised (`axiom_init_descs` and
/// its siblings). [`SlabTable::perms_ok`]'s three maps.
#[verifier::spinoff_prover]
proof fn table_perms(
    tracked draw: PointsToRaw,
    tracked braw: PointsToRaw,
    tracked oraw: PointsToRaw,
    d: int,
    b: int,
    o: int,
    align: usize,
    stagger: int,
    stride: int,
    nslabs: int,
    sb: int,
    group: int,
    class: int,
) -> (tracked r: (Map<int, PointsTo<SlabDesc>>, Map<int, PointsTo<AtomicU64>>, Map<int, PointsTo<AtomicU16>>))
    requires
        crate::meta::is_pow2(align),
        align >= 64,
        0 <= d,
        0 <= b,
        0 <= o,
        d % (align as int) == 0,
        b % (align as int) == 0,
        o % (align as int) == 0,
        0 <= class,
        stagger == class * 3 * PAD_ALIGN,
        16 <= stride <= u32::MAX,
        stride % 16 == 0,
        1 <= nslabs,
        0 <= sb,
        1 <= group,
        d + stagger + nslabs * stride <= crate::meta::va_limit(),
        b + stagger + (nslabs * sb + 7) / 8 <= crate::meta::va_limit(),
        o + stagger + (nslabs + group - 1) / group * 2 <= crate::meta::va_limit(),
        draw.is_range(d, (stagger + nslabs * stride + 7) / 8 * 8),
        braw.is_range(b, (stagger + (nslabs * sb + 7) / 8 + 7) / 8 * 8),
        oraw.is_range(o, (stagger + (nslabs + group - 1) / group * 2 + 7) / 8 * 8),
        draw.provenance() == cast_provenance(),
        braw.provenance() == cast_provenance(),
        oraw.provenance() == cast_provenance(),
    ensures
        forall|s: int| 0 <= s < nslabs ==> #[trigger] r.0.contains_key(s) && perm_at(r.0[s], d + stagger + s * stride),
        forall|j: int| 0 <= j < (nslabs * sb + 63) / 64 ==> #[trigger] r.1.contains_key(j)
            && perm_at(r.1[j], b + stagger + 8 * j)
            && word_addr(&r.1[j].value()) == b + stagger + 8 * j,
        forall|g: int| 0 <= g < (nslabs + group - 1) / group ==> #[trigger] r.2.contains_key(g)
            && perm_at(r.2[g], o + stagger + 2 * g),
{
    let ngroups = (nslabs + group - 1) / group;
    let nwords = (nslabs * sb + 63) / 64;
    let a = align;
    assert(a >= 64 && a & sub(a, 1) == 0 ==> a % 64 == 0) by (bit_vector);
    crate::meta::lemma_pow2_bits(a);
    crate::meta::lemma_mod_trans(d, align as int, 64);
    crate::meta::lemma_mod_trans(b, align as int, 64);
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
    assert(nslabs * stride >= 0 && nslabs * sb >= 0) by (nonlinear_arith)
        requires stride >= 0, nslabs >= 0, sb >= 0;
    lemma_round_up8(stagger + nslabs * stride);
    lemma_round_up8(stagger + ngroups * 2);
    lemma_round_words(stagger, nslabs * sb);
    lemma_layouts();
    let tracked dmap = carve_descs(draw, d + stagger, stride, nslabs);
    let tracked wmap = carve_words(braw, b + stagger, nwords);
    let tracked omap = carve_owners(oraw, o + stagger, ngroups);
    (dmap, wmap, omap)
}

/// [`table_perms`]' descriptor permissions: `n` of them, every `stride`
/// bytes from `base`, carved from `raw` and initialised.
proof fn carve_descs(tracked raw: PointsToRaw, base: int, stride: int, n: int) -> (tracked r: Map<int, PointsTo<SlabDesc>>)
    requires
        0 <= n,
        16 <= stride,
        base % 16 == 0,
        stride % 16 == 0,
        0 <= base,
        base + n * stride <= usize::MAX,
        raw.contains_range(base, n * stride),
        raw.provenance() == cast_provenance(),
    ensures
        forall|s: int| 0 <= s < n ==> #[trigger] r.contains_key(s) && perm_at(r[s], base + s * stride),
{
    lemma_layouts();
    let tracked carved = carve::<SlabDesc>(raw, base, stride, n);
    let tracked r = axiom_init_descs(carved, n);
    assert forall|s: int| 0 <= s < n implies #[trigger] r.contains_key(s) && perm_at(r[s], base + s * stride) by {
        assert(carved.contains_key(0 + s));
    }
    r
}

/// [`table_perms`]' class bitmap word permissions: `n` words from `base`,
/// carved from `raw` and initialised.
proof fn carve_words(tracked raw: PointsToRaw, base: int, n: int) -> (tracked r: Map<int, PointsTo<AtomicU64>>)
    requires
        0 <= n,
        base % 8 == 0,
        0 <= base,
        base + n * 8 <= usize::MAX,
        raw.contains_range(base, n * 8),
        raw.provenance() == cast_provenance(),
    ensures
        forall|j: int| 0 <= j < n ==> #[trigger] r.contains_key(j)
            && perm_at(r[j], base + 8 * j)
            && word_addr(&r[j].value()) == base + 8 * j,
{
    lemma_layouts();
    let tracked carved = carve::<AtomicU64>(raw, base, 8, n);
    let tracked r = axiom_init_words(carved, n);
    assert forall|j: int| 0 <= j < n implies #[trigger] r.contains_key(j)
        && perm_at(r[j], base + 8 * j)
        && word_addr(&r[j].value()) == base + 8 * j by {
        assert(carved.contains_key(0 + j));
        assert(j * 8 == 8 * j && base + 8 * j <= usize::MAX) by (nonlinear_arith)
            requires 0 <= j < n, base + n * 8 <= usize::MAX;
        assert(carved[0 + j].ptr()@.addr == base + 8 * j);
    }
    r
}

/// [`table_perms`]' owner word permissions: `n` words from `base`, carved
/// from `raw` and initialised.
proof fn carve_owners(tracked raw: PointsToRaw, base: int, n: int) -> (tracked r: Map<int, PointsTo<AtomicU16>>)
    requires
        0 <= n,
        base % 2 == 0,
        0 <= base,
        base + n * 2 <= usize::MAX,
        raw.contains_range(base, n * 2),
        raw.provenance() == cast_provenance(),
    ensures
        forall|g: int| 0 <= g < n ==> #[trigger] r.contains_key(g) && perm_at(r[g], base + 2 * g),
{
    lemma_layouts();
    let tracked carved = carve::<AtomicU16>(raw, base, 2, n);
    let tracked r = axiom_init_owners(carved, n);
    assert forall|g: int| 0 <= g < n implies #[trigger] r.contains_key(g) && perm_at(r[g], base + 2 * g) by {
        assert(carved.contains_key(0 + g));
    }
    r
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
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(nb + 7, 8);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(c + 7, 8);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(nb + 63, 64);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(st, 8);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(st + c + 7, 8);
}

/// `m >> lg` slabs of `1 << lg` bytes fit in `m` bytes: slab `s` below
/// that starts at `s << lg` and ends at or below `m`.
proof fn lemma_slab_fits(s: usize, n: usize, m: usize, lg: usize)
    requires
        s < n,
        n <= m >> lg,
        lg < 64,
    ensures
        (s << lg) + (1usize << lg) <= m,
        (s << lg) >> lg == s,
{
    assert((s << lg) + (1usize << lg) <= m && (s << lg) >> lg == s) by (bit_vector)
        requires s < n, n <= m >> lg, lg < 64;
}

/// The view `SlabTable::bits` gives of a slab of a class with `slab_bits`
/// bits per slab: whole words, or one run of bits inside a word.
pub open spec fn bits_shape<'a>(bm: AtomicBitmap<'a>, slab_bits: usize) -> bool {
    &&& bm.spec_nbits() == slab_bits
    &&& bm.spec_words() == words_of(slab_bits)
    &&& bm.slab_mask() == slab_mask_of(slab_bits)
    &&& slab_bits >= 64 ==> slab_bits % 64 == 0
}

/// The authority of class `c`'s slot tokens (`bitmap.rs`, "Slot tokens"):
/// the one its table made.
pub uninterp spec fn slot_loc(c: int) -> Loc;

/// Trusted: `SlabTable::new` runs once per class in a process
/// (`Shared::init`, its one caller, builds the class tables of the one
/// `Shared`, as `layout::process_regions` assumes), so the authority it
/// makes is the class's. It names the authority for the thread caches,
/// whose tokens come from every arena's refills of the class. Two calls
/// for one class with two authorities would prove `false`, so it is
/// private to this module and `SlabTable::new` is its one call. For the
/// same reason the table's guard geometry (`glg`, its `lg_slabs_per_page`,
/// and `gmask`, its `guard_mask`) is the class's
/// ([`guard_geom`](crate::layout::guard_geom)): the guard slabs that
/// `Regions::is_slot` excludes are this table's. A second call for one
/// class with another guard interval would prove `false` too: `init`
/// builds each table once and aborts on failure, never retrying.
axiom fn axiom_slot_loc(c: int, loc: Loc, glg: u8, gmask: u32)
    ensures
        slot_loc(c) == loc,
        crate::layout::guard_geom(c) == (glg, gmask);

/// Trusted: the permissions of the `n` words of the table at `base` that
/// `Meta::reserve_table` has just returned, each holding zero: the table
/// has never been written (`meta.rs`: carved once and never freed), and
/// nothing but the bitmap views reaches it. A second permission for one
/// word would let a proof write the word outside its invariant, so it is
/// private to this module and `SlabTable::new` is its one call, for its
/// own bitmap. The `PointsTo<AtomicU64>`s of [`axiom_init_words`] are
/// a second permission to the same words, held by `SlabTable` and only
/// ever borrowed shared (`ptr_ref`), never written through.
axiom fn axiom_fresh_words(base: usize, n: nat) -> (tracked r: Map<int, WordPerm>)
    ensures
        forall|j: int| 0 <= j < n ==> #[trigger] r.contains_key(j) && r[j].addr() == base + 8 * j && r[j].value() == 0;

/// `n` permissions of `perms` from key `j0` on are to the uninitialised
/// `T`s at `base`, `base + stride`, ..., of `perms`' provenance `prov`.
spec fn carved<T>(perms: Map<int, PointsTo<T>>, j0: int, base: int, stride: int, n: int, prov: vstd::raw_ptr::Provenance) -> bool {
    forall|i: int| 0 <= i < n ==> {
        &&& #[trigger] perms.contains_key(j0 + i)
        &&& perms[j0 + i].ptr() == ptr_mut_from_data::<T>(PtrData { addr: (base + i * stride) as usize, provenance: prov, metadata: () })
        &&& perms[j0 + i].is_uninit()
    }
}

/// The permissions of the `n` values of `T` at `base`, `base + stride`,
/// ..., keys `0..n`, carved from `raw`, the permission to their bytes;
/// uninitialised, as `PointsToRaw::into_typed` makes them.
proof fn carve<T>(tracked raw: PointsToRaw, base: int, stride: int, n: int) -> (tracked r: Map<int, PointsTo<T>>)
    requires
        0 <= n,
        0 < vstd::layout::size_of::<T>() <= stride,
        base % (vstd::layout::align_of::<T>() as int) == 0,
        stride % (vstd::layout::align_of::<T>() as int) == 0,
        0 <= base,
        base + n * stride <= usize::MAX,
        raw.contains_range(base, n * stride),
    ensures
        carved(r, 0, base, stride, n, raw.provenance()),
    decreases n,
{
    if n == 0 {
        Map::tracked_empty()
    } else {
        let m = n - 1;
        let sz = vstd::layout::size_of::<T>() as int;
        let al = vstd::layout::align_of::<T>() as int;
        let a = base + m * stride;
        assert(m * stride + stride == n * stride && 0 <= m * stride) by (nonlinear_arith)
            requires m == n - 1, 0 < n, stride > 0;
        assert(a % al == 0) by {
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(base, al);
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(stride, al);
            let (qb, qs) = (base / al, stride / al);
            assert(a == (qb + m * qs) * al) by (nonlinear_arith)
                requires base == al * qb, stride == al * qs, a == base + m * stride;
            vstd::arithmetic::div_mod::lemma_mod_multiples_basic(qb + m * qs, al);
        }
        let tracked (last, rest) = raw.split(Set::range(a, a + sz));
        let tracked mut r = carve::<T>(rest, base, stride, m);
        let ghost before = r;
        assert(last.dom() =~= Set::range(a, a + sz));
        let tracked p = last.into_typed::<T>(a as usize);
        r.tracked_insert(m, p);
        assert forall|i: int| 0 <= i < n implies {
            &&& #[trigger] r.contains_key(0 + i)
            &&& r[0 + i].ptr() == ptr_mut_from_data::<T>(PtrData { addr: (base + i * stride) as usize, provenance: raw.provenance(), metadata: () })
            &&& r[0 + i].is_uninit()
        } by {
            if i < m {
                assert(before.contains_key(0 + i));
            }
        }
        r
    }
}

/// Trusted: the descriptors at the permissions `perms` carved for keys
/// `0..n` hold values. A `SlabDesc` is plain atomics, valid for every bit
/// pattern, and the reservation's bytes are initialised (anonymous memory,
/// zero, once committed: the premise of `sets::axiom_table_bytes` that
/// only committed pages are read). Private; `SlabTable::new` is its one
/// call, on the descriptor array it reserved.
axiom fn axiom_init_descs(tracked perms: Map<int, PointsTo<SlabDesc>>, n: int) -> (tracked r: Map<int, PointsTo<SlabDesc>>)
    ensures
        forall|i: int| 0 <= i < n && #[trigger] perms.contains_key(i) && perms[i].is_uninit()
            ==> r.contains_key(i) && r[i].ptr() == perms[i].ptr() && r[i].is_init(),
;

/// Trusted: the owner words at the permissions `perms` carved for keys
/// `0..n` hold values, as [`axiom_init_descs`] (an `AtomicU16` is valid
/// for every bit pattern). Private; `SlabTable::new` is its one call, on
/// the owner array it reserved.
axiom fn axiom_init_owners(tracked perms: Map<int, PointsTo<AtomicU16>>, n: int) -> (tracked r: Map<int, PointsTo<AtomicU16>>)
    ensures
        forall|i: int| 0 <= i < n && #[trigger] perms.contains_key(i) && perms[i].is_uninit()
            ==> r.contains_key(i) && r[i].ptr() == perms[i].ptr() && r[i].is_init(),
;

/// Trusted: the class bitmap words at the permissions `perms` carved for
/// keys `0..n` hold values, as [`axiom_init_descs`], and each is the word
/// at its permission's address, the identity [`word_addr`] names (the
/// identity the word permissions of [`axiom_fresh_words`] are for).
/// Private; `SlabTable::new` is its one call, on the class bitmap it
/// reserved.
axiom fn axiom_init_words(tracked perms: Map<int, PointsTo<AtomicU64>>, n: int) -> (tracked r: Map<int, PointsTo<AtomicU64>>)
    ensures
        forall|i: int| 0 <= i < n && #[trigger] perms.contains_key(i) && perms[i].is_uninit()
            ==> r.contains_key(i) && r[i].ptr() == perms[i].ptr() && r[i].is_init()
                && word_addr(&r[i].value()) == perms[i].ptr()@.addr,
;

/// `core::slice::from_raw_parts(p, n)` over `n` class bitmap words whose
/// permissions `perms` holds from key `j0` on, at `p`, `p + 8`, ...
/// Trusted: vstd has no slice view of permissions. `n` initialised,
/// aligned, consecutive `AtomicU64`s with consecutive keys of one
/// table's map, all carved from one reservation's permission (private;
/// `bits_unchecked`, its one caller, passes `SlabTable::word_perms`),
/// are what `from_raw_parts` asks for, and the slice's words are the ones
/// the permissions are to.
#[verifier::external_body]
#[inline(always)]
fn words_ref<'a>(p: *const AtomicU64, n: usize, Ghost(j0): Ghost<int>, Tracked(perms): Tracked<&'a Map<int, PointsTo<AtomicU64>>>) -> (r: &'a [AtomicU64])
    requires
        0 < n,
        n * 8 <= isize::MAX,
        forall|i: int| 0 <= i < n ==> #[trigger] perms.contains_key(j0 + i) && perms[j0 + i].is_init()
            && perms[j0 + i].ptr() == ptr_mut_from_data::<AtomicU64>(
                PtrData { addr: (p@.addr + 8 * i) as usize, provenance: p@.provenance, metadata: () }),
    ensures
        r@.len() == n,
        forall|i: int| 0 <= i < n ==> #[trigger] r@[i] == perms[j0 + i].value(),
{
    // SAFETY: the precondition.
    unsafe { core::slice::from_raw_parts(p, n) }
}

/// The ghost side of the view `SlabTable::bits` gives of `slab`: slot 0 is
/// class bitmap bit `slab * slab_bits`, and the view uses the table's slot
/// tokens and word invariants.
pub open spec fn bits_ghost<'a>(bm: AtomicBitmap<'a>, t: SlabTable, slab: usize) -> bool {
    &&& bm.spec_first() == slab * t.spec_slots().sb
    &&& bm.spec_slots() == t.spec_slots()
    &&& bm.spec_invs() == t.spec_word_invs()
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

impl SlabTable {
    /// The geometry `new` sets up and never changes: the class, its slab
    /// bits, the power-of-two page and hand-out groups, and a region of
    /// `nslabs` slabs from `base` that ends inside the address space. `new`
    /// proves it from its preconditions; its caller `Shared::init` takes
    /// the ones about `regions` from its own preconditions (trusted at
    /// `api::reserve_regions`) and `heap::shared::axiom_process_shared`:
    /// well formed and separated (`Regions::from_parts`, whose separation gives
    /// a page no larger than a class region and a class region that ends at
    /// or below `2^57`: it is inside a user-space mapping, and no x86_64 or
    /// aarch64 user address space reaches that; 5-level paging tops out at
    /// `2^56`), and the process's regions ([`process_regions`]: `init`
    /// builds the tables over the one `Shared`'s regions), so the region
    /// is class `class`'s region of the process's regions.
    /// It also holds the permissions `new` carved for the three arrays
    /// ([`SlabTable::perms_ok`]), and its guard geometry is the class's
    /// ([`guard_geom`](crate::layout::guard_geom), `axiom_slot_loc`), with
    /// page groups of at most `2^26` slabs.
    ///
    /// [`process_regions`]: crate::layout::process_regions
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
        &&& lock_init(self.inner.0).page@ == 1usize << self.lg_page
        &&& lock_init(self.inner.0).group@ == self.group as usize
        &&& lock_init(self.inner.0).nslabs@ == self.nslabs
        &&& lock_init(self.inner.0).sb@ == self.slab_bits as usize
        &&& lock_init(self.inner.0).mloc@ == self.mark_auth@.id()
        &&& self.base == process_regions().class_base[self.class as int]
        &&& self.nslabs as int == process_regions().spec_nslabs(self.class as int)
        &&& words_wf(self.word_invs@, self.spec_slots(), self.bits_base, self.spec_nwords())
        &&& self.spec_slots().loc == slot_loc(self.class as int)
        &&& crate::layout::guard_geom(self.class as int) == (self.lg_slabs_per_page, self.guard_mask)
        &&& self.perms_ok()
        &&& self.inner.0.rank() == rank_table(self.class as int)
    }

    /// Hand-out groups of the region (owner words).
    pub(super) closed spec fn spec_ngroups(&self) -> int {
        (self.nslabs + self.group - 1) / (self.group as int)
    }

    /// The permissions `new` carved from the three reservations: to
    /// descriptor `s` at `desc_base + s * desc_stride`, to class bitmap word
    /// `j` at `bits_base + 8 * j` (the word [`word_addr`] names there), and
    /// to owner word `g` at `owners_base + 2 * g`, for every slab, word and
    /// hand-out group of the region; none of the arrays wraps.
    pub(super) closed spec fn perms_ok(&self) -> bool {
        &&& self.desc_base + self.nslabs * (self.desc_stride as int) <= usize::MAX
        &&& self.bits_base + 8 * self.spec_nwords() <= usize::MAX
        &&& self.owners_base + 2 * self.spec_ngroups() <= usize::MAX
        &&& forall|s: int| 0 <= s < self.nslabs ==> #[trigger] self.desc_perms@.contains_key(s)
            && perm_at(self.desc_perms@[s], self.desc_base + s * (self.desc_stride as int))
        &&& forall|j: int| 0 <= j < self.spec_nwords() ==> #[trigger] self.word_perms@.contains_key(j)
            && perm_at(self.word_perms@[j], self.bits_base + 8 * j)
            && word_addr(&self.word_perms@[j].value()) == self.bits_base + 8 * j
        &&& forall|g: int| 0 <= g < self.spec_ngroups() ==> #[trigger] self.owner_perms@.contains_key(g)
            && perm_at(self.owner_perms@[g], self.owners_base + 2 * g)
    }

    /// The descriptor array, the class bitmap and the owner array lie in
    /// the metadata region (whole-allocator theorem 4): [`new`] reserves
    /// each from `meta` ([`Lazy::reserve_raw`]) with room for every slab's
    /// descriptor, every slab's `slab_bits` bits and every hand-out
    /// group's owner word.
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
    /// [`owner_unchecked`](SlabTable::owner_unchecked) point at
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
    /// fill whole words (the reservation covers the bits' bytes, not the
    /// last word's padding: `Meta` rounds a table to whole pages, which is
    /// what makes the last word's padding readable when they do not).
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

    /// Slabs per page group, for other modules' contracts.
    pub closed spec fn spec_slabs_per_page(&self) -> usize {
        self.slabs_per_page as usize
    }

    /// Page groups are `1 << lg_slabs_per_page` slabs and tile the
    /// region. Opaque, so its bit operations stay out of proofs that do
    /// not need them; revealed by
    /// `new` and [`slabs_per_page`](SlabTable::slabs_per_page).
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

    /// The class bitmap's slot tokens (`bitmap.rs`, "Slot tokens"): bits of
    /// `slab_bits` per slab, `nslots` of them slots, `nslabs` slabs.
    pub closed spec fn spec_slots(&self) -> Slots {
        Slots {
            loc: self.slot_auth@.id(),
            mloc: self.mark_auth@.id(),
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
        self.word_invs@
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

    /// No markers of the class bitmap's authority: where a claim's proof
    /// of its slabs' initialisation starts.
    pub proof fn empty_marks(tracked &self) -> (tracked r: GhostPersistentSubset<int>)
        ensures
            r.id() == self.spec_slots().mloc,
            r@ == Set::<int>::empty(),
    {
        self.mark_auth.borrow().empty().persist()
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

    /// Whether `slab` is a guard slab ([`is_guard_slab`]).
    ///
    /// [`is_guard_slab`]: SlabTable::is_guard_slab
    pub closed spec fn spec_is_guard(&self, slab: usize) -> bool {
        crate::layout::guard_of(self.lg_slabs_per_page, self.guard_mask, slab)
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

    /// The slab range of hand-out group `group` ([`group_range`]): `group`
    /// slabs from `group << lg_group`, clipped to the region.
    ///
    /// [`group_range`]: SlabTable::group_range
    pub(super) open spec fn spec_group_range(self, group: usize) -> (usize, usize) {
        let start = group << self.lg_group;
        (start, if start + self.group as usize <= self.nslabs { (start + self.group as usize) as usize } else { self.nslabs })
    }

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
    pub fn nslabs(&self) -> (r: usize)
        ensures
            r == self.spec_nslabs(),
    {
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
    /// (`heap::GuardTest`).
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

    /// The watermark, loaded with `order` (inlined, so the caller's load
    /// is the bare `core` one with its ordering).
    ///
    /// Trusted: the watermark is at most the slab count. Its one atomic is
    /// written only by `new` (0) and by `take_group`, which stores the end
    /// of the group it hands out, at most `nslabs` (proved there); vstd
    /// specifies the load as returning any value, and nothing ties the
    /// atomic to those writes.
    #[inline(always)]
    #[verifier::external_body]
    fn watermark(&self, order: Ordering) -> (r: usize)
        requires
            // `core`'s load panics for these two.
            !(order is Release),
            !(order is AcqRel),
        ensures
            r <= self.spec_nslabs(),
        opens_invariants none
    {
        self.committed_slabs.load(order)
    }

    /// Number of slabs (including guard slabs) handed to an arena: their
    /// descriptors, bits and owner words exist (see the field).
    #[inline]
    pub fn committed_slabs(&self) -> (r: usize)
        ensures
            r <= self.spec_nslabs(),
    {
        self.watermark(Ordering::Acquire)
    }

    /// Guard slabs committed so far.
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

    /// Descriptor of a handed-out slab. Aborts for a slab that was never
    /// handed out rather than faulting on its `PROT_NONE` descriptor page.
    #[inline]
    pub fn desc(&self, slab: usize) -> (r: &SlabDesc)
        requires
            crate::sys::may_abort(),
        ensures
            *r == desc_at(*self, slab),
    {
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
    /// are always mapped. [`is_set_unchecked`] and [`owner_unchecked`]
    /// rely on the same argument.
    ///
    /// [`is_set_unchecked`]: SlabTable::is_set_unchecked
    /// [`owner_unchecked`]: SlabTable::owner_unchecked
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
    pub fn is_handed_out(&self, slab: usize) -> (r: bool)
        ensures
            r ==> slab < self.spec_nslabs(),
        opens_invariants none
    {
        slab < self.watermark(Ordering::Relaxed)
    }

    /// Descriptor of `slab` without a watermark check, through its
    /// permission ([`SlabTable::perms_ok`]).
    ///
    /// # Safety
    /// `slab` must be below a value `committed_slabs` has held: descriptors
    /// up to that count are committed and initialised (or zero, for guard
    /// slabs) before the watermark moves (the premise of
    /// `sets::axiom_table_bytes`).
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

    /// The slot bits of a handed-out slab (design section 4). Aborts for a
    /// slab that was never handed out.
    #[inline]
    pub fn bits(&self, slab: usize) -> (r: AtomicBitmap<'_>)
        requires
            crate::sys::may_abort(),
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

    /// The view of `slab`'s bits: words `first / 64 ..` of the class
    /// bitmap at `bits_base`, with `first = slab * slab_bits`, through their
    /// permissions ([`SlabTable::perms_ok`]), which make them the words
    /// the table's word invariants are for.
    ///
    /// # Safety
    /// As [`desc_unchecked`](SlabTable::desc_unchecked).
    #[inline]
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
                assert forall|i: int| 0 <= i < k implies #[trigger] self.word_perms@.contains_key(j0 + i)
                    && self.word_perms@[j0 + i].is_init()
                    && self.word_perms@[j0 + i].ptr() == ptr_mut_from_data::<AtomicU64>(
                        PtrData { addr: (word@.addr + 8 * i) as usize, provenance: word@.provenance, metadata: () }) by {
                    assert(self.word_perms@.contains_key(j0 + i));
                }
            }
            debug_assert!(first % 64 == 0);
            // The group's words were committed before the watermark moved
            // (the caller's premise).
            let words = words_ref(word, nbits / 64, Ghost(j0), Tracked(self.word_perms.borrow()));
            proof {
                assert forall|i: int| 0 <= i < words@.len() implies #[trigger] crate::bitmap::word_ok(words@[i], first / 64 + i, sl, self.word_invs@) by {
                    assert(self.word_perms@.contains_key(j0 + i));
                    assert(self.word_invs@.contains_key(j0 + i));
                }
            }
            AtomicBitmap::from_words(words, Ghost(first as int), Ghost(sl), Tracked(self.word_invs.borrow()))
        } else {
            // As above, one word.
            let w = ptr_ref(word, Tracked(self.word_perms.borrow().tracked_borrow(j0)));
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
            AtomicBitmap::sub_word(w, (first % 64) as u32, nbits, Ghost(first as int), Ghost(sl), Tracked(self.word_invs.borrow()))
        }
    }

    /// The `free` check's bit (design section 4): whether `slot` of `slab`
    /// is marked allocated (or cached). One relaxed load of the bitmap
    /// word, through its permission, no bounds checks.
    ///
    /// # Safety
    /// `slab` must have been handed out ([`is_handed_out`]): its bitmap
    /// word is committed. The precondition states only the bounds (`slab`
    /// below the slab count, `slot` below the class's `nslots`).
    ///
    /// [`is_handed_out`]: SlabTable::is_handed_out
    #[inline(always)]
    pub unsafe fn is_set_unchecked(&self, slab: usize, slot: usize) -> (r: bool)
        requires
            slab < self.spec_nslabs(),
            slot < class_table()[self.spec_class() as int].nslots,
        ensures
            // A guard slab's bits are never set (`bitmap::guards_ok`).
            r ==> !self.spec_is_guard(slab) && !crate::layout::guard_slab(self.spec_class() as int, slab as int),
        opens_invariants [crate::bitmap::word_ns()]
    {
        proof {
            use_type_invariant(self);
            self.lemma_bits_fit(slab);
            assert(slot < self.slab_bits);
        }
        let bit = slab * self.slab_bits as usize + slot;
        let ghost j = bit as int / 64;
        proof {
            assert(j < self.spec_nwords());
            assert(self.word_perms@.contains_key(j));
            assert(self.word_invs@.contains_key(j));
        }
        let word = addr_to_ptr(self.bits_base + (bit / 64) * 8) as *const AtomicU64;
        // The caller guarantees the word is inside the handed-out part of
        // the bitmap (committed).
        let w = ptr_ref(word, Tracked(self.word_perms.borrow().tracked_borrow(j)));
        let tracked inv = self.word_invs.borrow().tracked_borrow(j);
        let x;
        vstd::open_atomic_invariant!(inv => pair => {
            let tracked (perm, g, u, d) = pair;
            x = crate::vspec::word::load(w, Ordering::Relaxed, Tracked(&perm));
            proof {
                self.lemma_guard_slab(slab as int);
                if self.spec_is_guard(slab) {
                    let s = self.spec_slots();
                    crate::bitmap::lemma_slab_bit(s, slab as int, slot as int);
                    crate::bitmap::lemma_guard_clear(inv.constant(), (perm, g, u, d), bit as int);
                    assert((bit - 64 * j) as u64 == (bit % 64) as u64);
                }
                pair = (perm, g, u, d);
            }
        });
        (x >> (bit % 64)) & 1 == 1
    }

    /// Whether `slot` of `slab` is marked allocated (or cached); `false`
    /// for a slab that was never handed out. Never aborts.
    #[inline]
    pub fn is_allocated(&self, slab: usize, slot: usize) -> bool
        requires
            slot < class_table()[self.spec_class() as int].nslots,
    {
        debug_assert!(slot < self.class().nslots());
        // SAFETY: the watermark check, then a slot below `nslots`.
        self.is_handed_out(slab) && unsafe { self.is_set_unchecked(slab, slot) }
    }

    /// The arena that owns `slab`'s group, without a watermark check.
    ///
    /// # Safety
    /// `slab` must have been handed out ([`is_handed_out`]).
    ///
    /// The read is [`owner_at`](SlabTable::owner_at)'s; a handed-out slab
    /// is below the slab count ([`is_handed_out`]'s postcondition), which
    /// its one caller outside this file, `Shared::checked_slot`, has from
    /// `Shared::set_slot`.
    ///
    /// [`is_handed_out`]: SlabTable::is_handed_out
    #[inline(always)]
    pub unsafe fn owner_unchecked(&self, slab: usize) -> u16
        requires
            slab < self.spec_nslabs(),
        opens_invariants none
    {
        // SAFETY: the caller's.
        unsafe { self.owner_at(slab) }
    }

    /// The arena that owns `slab`'s group: one relaxed load of its owner
    /// word, through its permission ([`SlabTable::perms_ok`]).
    ///
    /// # Safety
    /// `slab` must be below a value the watermark has held: the owner word
    /// of a handed-out group was committed and written before the
    /// watermark moved (the premise of `sets::axiom_table_bytes`).
    #[inline(always)]
    unsafe fn owner_at(&self, slab: usize) -> (r: u16)
        requires
            slab < self.spec_nslabs(),
        ensures
            r == slab_owner(self.spec_class() as int, slab as int),
        opens_invariants none
    {
        proof {
            use_type_invariant(self);
            self.lemma_group_in_range(slab);
        }
        let group = slab >> self.lg_group;
        proof {
            assert(self.owner_perms@.contains_key(group as int));
        }
        let word = addr_to_ptr(self.owners_base + group * 2) as *const AtomicU16;
        load_owner(ptr_ref(word, Tracked(self.owner_perms.borrow().tracked_borrow(group as int))), Ghost(*self), Ghost(slab))
    }

    /// Slab `slab`'s `slab_bits` bits from `slab * slab_bits` lie in the
    /// class bitmap, which has at most `2^42` bits, and every bit of it is
    /// in one of its words.
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

    /// The arena that owns `slab` ([`slab_owner`]). Aborts for a slab that
    /// was never handed out.
    #[inline]
    pub fn owner(&self, slab: usize) -> (r: u16)
        requires
            crate::sys::may_abort(),
        ensures
            slab < self.spec_nslabs(),
            r == slab_owner(self.spec_class() as int, slab as int),
    {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        // SAFETY: `slab` is below the acquire-loaded watermark.
        unsafe { self.owner_at(slab) }
    }

    #[inline]
    pub(super) fn slab_addr(&self, slab: usize) -> (r: usize)
        requires
            slab < self.spec_nslabs(),
        ensures
            r + (1usize << class_table()[self.spec_class() as int].lg_slab) <= usize::MAX,
            r + (1usize << class_table()[self.spec_class() as int].lg_slab) <= 1usize << 57,
            self.base <= r,
            r as int == self.base as int + (slab as int) * ((1usize << class_table()[self.spec_class() as int].lg_slab) as int),
            r + (1usize << class_table()[self.spec_class() as int].lg_slab)
                <= self.base + (self.spec_nslabs() << class_table()[self.spec_class() as int].lg_slab),
            r == self.base + (slab << class_table()[self.spec_class() as int].lg_slab),
    {
        proof {
            use_type_invariant(self);
            let lg = class_table()[self.class as int].lg_slab as usize;
            vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
            vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(slab, lg, (usize::MAX - self.base) as usize);
            vstd::bits::lemma_usize_shl_is_mul(slab, lg);
            vstd::bits::lemma_usize_shl_is_mul(1, lg);
            lemma_slab_fits(slab, self.nslabs, (usize::MAX - self.base) as usize, lg);
            lemma_slab_fits(slab, self.nslabs, ((1usize << 57) - self.base) as usize, lg);
            let (n, l8, m) = (self.nslabs, class_table()[self.class as int].lg_slab, (usize::MAX - self.base) as usize);
            assert((slab << lg) + (1usize << lg) <= (n << l8)) by (bit_vector)
                requires slab < n, lg == l8 as usize, lg <= 22, n <= m >> lg;
            assert((1usize << l8) == (1usize << lg)) by (bit_vector)
                requires lg == l8 as usize;
        }
        self.base + (slab << self.class().lg_slab)
    }

    /// The address of slot `slot` of slab `slab`. A slot below `nslots` is
    /// a slot start of the class region; a claim passes bit indices below
    /// `slab_bits`, and proves them slots (`AtomicBitmap::claim_word`).
    #[inline]
    pub(super) fn slot_ptr(&self, slab: usize, slot: usize) -> (r: *mut u8)
        requires
            slab < self.spec_nslabs(),
            slot < class_table()[self.spec_class() as int].slab_bits,
        ensures
            r@.addr == process_regions().slot_at(self.spec_class() as int, slab as int, slot as int),
            slot < class_table()[self.spec_class() as int].nslots && !crate::layout::guard_slab(self.spec_class() as int, slab as int)
                ==> process_regions().is_slot(self.spec_class() as int, r@.addr),
            process_regions().is_bit_slot(self.spec_class() as int, r@.addr),
    {
        proof {
            use_type_invariant(self);
        }
        let a = self.slab_addr(slab);
        let cls = self.class();
        proof {
            let c = self.class as int;
            let lg = cls.lg_slab as usize;
            let s = (1usize << cls.lg_slab) as int;
            vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
            vstd::bits::lemma_usize_shl_is_mul(1, lg);
            assert(slot * (cls.size as int) <= 0x2000 * 0x4_0000) by (nonlinear_arith)
                requires
                    slot < cls.slab_bits,
                    cls.slab_bits <= 0x2000,
                    cls.size <= 0x4_0000,
            ;
            let m = (usize::MAX - self.base) as usize;
            let (n, l8) = (self.nslabs, cls.lg_slab);
            vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(n, lg, m);
            assert(n <= m >> lg) by (bit_vector)
                requires
                    n <= m >> l8,
                    lg == l8 as usize,
            ;
            assert(slab * pow2(lg as nat) <= m) by (nonlinear_arith)
                requires
                    slab < n,
                    n * pow2(lg as nat) <= m,
            ;
            vstd::bits::lemma_usize_shl_is_mul(slab, lg);
            assert((slab << l8) == (slab << lg)) by (bit_vector)
                requires lg == l8 as usize;
            assert(a as int == process_regions().slot_at(c, slab as int, 0) - 0);
            assert(process_regions().slot_at(c, slab as int, slot as int) == a + slot * (cls.size as int));
            assert((1usize << 57usize) == 0x200_0000_0000_0000usize) by (bit_vector);
            if slot < cls.nslots {
                process_regions().lemma_slab_index(c, slab as int, slot as int, (a + slot * (cls.size as int)) as usize);
            }
        }
        crate::vspec::addr::addr_to_ptr(a + slot * cls.size())
    }

    /// Slab and slot of a pointer that must be a slot start in this class's
    /// region. Aborts otherwise.
    #[inline]
    pub fn locate(&self, p: *mut u8) -> (r: (usize, usize))
        requires
            crate::sys::may_abort(),
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
    /// with the pointer and its location; returns the owning arena.
    #[inline]
    pub fn check_located(&self, slab: usize, slot: usize, p: usize) -> (r: u16)
        requires
            crate::sys::may_abort(),
            slot < class_table()[self.spec_class() as int].nslots,
        ensures
            r == slab_owner(self.spec_class() as int, slab as int),
            slab < self.spec_nslabs(),
            !crate::layout::guard_slab(self.spec_class() as int, slab as int),
    {
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
    pub fn check_live(&self, slab: usize, slot: usize, p: usize)
        requires
            crate::sys::may_abort(),
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
    fn not_live(&self, slab: usize, slot: usize, p: usize) -> !
        requires
            crate::sys::may_abort(),
    {
        if self.is_guard_slab(slab) {
            guard_free(p, self.class as usize, slab);
        }
        invalid_free(p, self.class as usize, slab, slot)
    }

    /// The slab range of `group`: `group << lg_group` and the `group`
    /// slabs from there, clipped to the class's slab count (the last
    /// group of a region may be short).
    #[inline]
    pub(super) fn group_range(&self, group: usize) -> (r: (usize, usize))
        requires
            group <= self.spec_nslabs() >> self.lg_group,
        ensures
            r.0 <= r.1 <= self.spec_nslabs(),
            r.0 == group << self.lg_group,
            r.1 == (if r.0 + self.group as usize <= self.spec_nslabs() { r.0 + self.group as usize } else { self.spec_nslabs() as int }),
            r == self.spec_group_range(group),
    {
        proof {
            use_type_invariant(self);
            lemma_nslabs_bound(self.nslabs, (usize::MAX - self.base) as usize, class_table()[self.class as int].lg_slab as usize);
            let (n, lg) = (self.nslabs, self.lg_group);
            assert(group <= n >> lg && lg < 64 ==> group << lg <= n) by (bit_vector);
        }
        let start = group << self.lg_group;
        proof {
            assert((usize::MAX >> 12) + 0xFFFF_FFFFusize <= usize::MAX) by (compute);
        }
        (start, (start + self.group as usize).min(self.nslabs))
    }

    /// Commit slab `s` of a group handed to the caller's arena: make its
    /// memory (or its whole page group, when the page is larger than the
    /// slab and `s` starts one) read-write, or install the guard when `s`
    /// is a guard slab. `Some(true)` for a slab ready to allocate from,
    /// `Some(false)` for a guard slab, `None` when `mprotect` fails (the
    /// slab stays uncommitted and may be retried).
    pub(super) fn commit_slab(&self, s: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<bool>)
        requires
            crate::sys::may_abort(),
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
        proof {
            let lp = self.lg_page;
            assert((1usize << lp) <= 0x8000_0000_0000_0000usize) by (bit_vector)
                requires lp < 64;
            assert((1usize << 57u32) == 0x200_0000_0000_0000usize) by (bit_vector);
            assert(usize::MAX >> 12 == 0xF_FFFF_FFFF_FFFFusize) by (bit_vector);
        }
        let mode = crate::thp::class_mode(cls.slab_size());
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
            crate::sys::may_abort(),
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

/// [`SlabTable::install_guard`]'s abort: the guard slab of `class` at `s`
/// could be neither guarded nor re-protected. Inlined, so the caller
/// keeps the abort it had in place.
#[inline(always)]
fn guard_unprotectable(class: &u16, s: &usize) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!("class {}: cannot re-protect guard slab {}", *class, *s))
}

} // verus!

verus! {

impl SlabTable {
    /// Hand the next group of slabs to `arena` (design section 4):
    /// commit their descriptors, bitmap words and owner word, initialise
    /// the descriptors (`Fresh`, owned by `arena`; guard slabs stay zero)
    /// and clear their bits, then publish the watermark. The set words of
    /// `sets`, the value behind `arena`'s class lock (which the caller
    /// holds), are committed under the commit lock up to the group's last
    /// slab, in `sets_page` units (the caller's page, the table's), before
    /// anything changes ([`Inner::ensure_sets`]); if that
    /// fails nothing has happened. Returns the group's slab range, `None`
    /// when the region or the metadata region is exhausted. The slabs
    /// themselves are committed one by one by [`commit_slab`].
    ///
    /// The group is above every slab `sets` owns and its current group's
    /// end ([`next_slab_locked`]), which is how
    /// [`ClassState::next_fresh_slab`](super::ClassState::next_fresh_slab) adds its slabs to the ghost; the
    /// same marker argument holds for every other arena's lock value, but
    /// no proof here sees them. `init` takes each non-guard slab's init
    /// markers from the commit lock's value and returns its fresh markers,
    /// and a guard slab's init markers are taken as they are; both join
    /// `inited` for `next_fresh_slab` to keep with the slabs.
    ///
    /// [`commit_slab`]: SlabTable::commit_slab
    /// [`next_slab_locked`]: SlabTable::next_slab_locked
    // Verus needs the explicit reborrow and a loop with an invariant.
    #[allow(clippy::explicit_auto_deref, clippy::while_let_loop)]
    pub(super) fn take_group(
        &self,
        meta: &Meta,
        arena: u16,
        sets: &mut Inner,
        sets_page: usize,
        Tracked(inited): Tracked<&mut GhostPersistentSubset<int>>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: Option<(usize, usize)>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_table(0)),
            old(sets).table@ == *self,
            old(sets).arena@ == arena,
            old(sets).base(),
            old(sets).nbits() == self.spec_nslabs(),
            old(inited).id() == self.spec_slots().mloc,
        ensures
            final(sets).same_sets(*old(sets)),
            *final(ord) == *old(ord),
            final(inited).id() == old(inited).id(),
            old(inited)@.subset_of(final(inited)@),
            r matches Some((s, e)) ==> s < e <= self.spec_nslabs()
                && self.spec_group_range(s >> self.lg_group) == (s, e)
                && (forall|x: nat| #[trigger] old(sets).owns(x) ==> x < s)
                && old(sets).group_end <= s
                && (forall|x: int| s <= x < e ==> #[trigger] slab_owner(self.spec_class() as int, x) == arena)
                && (forall|x: int| s <= x < e && !self.spec_is_guard(x as usize) ==> #[trigger] slab_marked(*self, x, final(inited)@))
                && (forall|x: int| s <= x < e && self.spec_is_guard(x as usize) ==> #[trigger] guard_marked(*self, x, final(inited)@)),
    {
        broadcast use {axiom_table_lock_inv, axiom_table_lock_frame};
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
        g.descs.ensure(meta, page, stagger + end * desc_stride(class), Tracked(&mut under)).ok()?;
        g.bits.ensure(meta, page, stagger + (end * cls.slab_bits()).div_ceil(8), Tracked(&mut under)).ok()?;
        g.owners.ensure(meta, page, stagger + ((end - 1) >> self.lg_group) * 2 + 2, Tracked(&mut under)).ok()?;
        sets.ensure_sets(meta, sets_page, end - 1, Tracked(&mut under)).ok()?;
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
        let previous = owner_word.load(Ordering::Relaxed);
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
            // SAFETY: the descriptor page for `slab` was committed above.
            let recorded = unsafe { SlabDesc::at(self.desc_base, class, slab).arena() };
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
                gi.same_but_fresh(g0),
                self.inv(),
                sl == self.spec_slots(),
                sl.wf(),
                sl.sb == cls.slab_bits,
                sl.nslots == cls.nslots,
                sl.nbits == self.spec_nslabs() * sl.sb,
                gi.fresh@.id() == sl.mloc,
                init_marks(slabs.start * sl.sb, self.spec_nslabs() * sl.sb).subset_of(gi.fresh@@),
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
                        continue;
                    }
                    // SAFETY: the descriptor and bitmap pages for `slab` were
                    // committed above.
                    unsafe {
                        if purge::check_zero() {
                            let d = SlabDesc::at(self.desc_base, class, slab);
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
                        SlabDesc::at(self.desc_base, class, slab).init(arena);
                        let ghost f = slab * sl.sb;
                        proof {
                            assert(f + sl.sb <= sl.nbits) by (nonlinear_arith)
                                requires slab < self.spec_nslabs(), sl.nbits == self.spec_nslabs() * sl.sb, f == slab * sl.sb, sl.sb > 0;
                            crate::bitmap::lemma_marks_sub(f, f + sl.sb, f, self.spec_nslabs() * sl.sb);
                            crate::bitmap::lemma_marks_split(f, f + sl.sb, self.spec_nslabs() * sl.sb);
                            self.lemma_guard_slab(slab as int);
                        }
                        let tracked fs = gi.fresh.borrow_mut().split(init_marks(f, f + sl.sb));
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
        store_owner(owner_word, arena, Ghost(*self), Ghost(s >> self.lg_group));
        gi.next_slab = end;
        proof {
            let gr = gi.group@;
            vstd::arithmetic::div_mod::lemma_mod_adds(s as int, gr as int, gr as int);
            vstd::arithmetic::div_mod::lemma_mod_self_0(gr as int);
            assert(gi.inv());
        }
        self.committed_slabs.store(end, Ordering::Release);
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
            let tracked inv = self.word_invs.borrow().tracked_borrow(j);
            assert(inv.constant().slots == sl);
            vstd::open_atomic_invariant_in_proof!(credit => inv => pair => {
                let tracked (perm, toks, mut u, mut d) = pair;
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
                pair = (perm, toks, u, d);
            });
        }
    }

    /// The owner word of `group`, through its permission
    /// ([`SlabTable::perms_ok`]).
    ///
    /// # Safety
    /// The group must have been handed out, or be about to be under the
    /// commit lock (its owner page is committed: the premise of
    /// `sets::axiom_table_bytes`).
    #[inline]
    pub(super) unsafe fn owner_word(&self, group: usize) -> (r: &AtomicU16)
        requires
            group < self.spec_ngroups(),
        ensures
            *r == owner_word_at(*self, group),
    {
        proof {
            use_type_invariant(self);
            assert(self.owner_perms@.contains_key(group as int));
        }
        let p = addr_to_ptr(self.owners_base + group * 2) as *const AtomicU16;
        ptr_ref(p, Tracked(self.owner_perms.borrow().tracked_borrow(group as int)))
    }
}

/// `take_group`'s debug check that the watermark is group-aligned, which
/// the precondition (proved from [`TableInner::inv`]) says it is.
/// Inlined and `track_caller`, so the caller keeps the code and the
/// location of the check it had in place. A `debug_assert!` of the
/// equality: Verus does not support `debug_assert_eq!` (its failure path
/// is `core::panicking::assert_failed`).
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
    debug_assert!(s & ((1 << lg_group) - 1) == 0, "take_group: the watermark is group-aligned");
}

} // verus!
