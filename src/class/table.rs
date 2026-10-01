//! The per-class [`SlabTable`]: the class region's descriptor array,
//! class bitmap and group owner array, the hand-out watermark and the
//! commit lock; groups go to arenas here (`take_group`) and slabs are
//! committed here (`commit_slab`).

use super::Inner;
use super::abort::{guard_free, invalid_free, invalid_pointer, never_committed, owner_mismatch};
use super::sets::Lazy;
use crate::bitmap::{AtomicBitmap, WordInv};
#[cfg(verus_only)]
use crate::bitmap::{Slots, words_wf};
#[cfg(verus_only)]
use crate::bitmap::{has_bit, subset};
use crate::layout::{CLASSES, Class, Regions};
#[cfg(verus_only)]
use crate::layout::{NCLASSES, class_table, process_regions};
use crate::lock::{Lock, PAD_ALIGN, Padded, RawLock};
use crate::meta::Meta;
use crate::purge;
use crate::slab::{DESC_ALIGN, SlabDesc, desc_stride};
use crate::sys::{self, Errno, MADV_GUARD_INSTALL, PROT_NONE, PROT_READ, PROT_WRITE};
#[cfg(verus_only)]
use crate::vspec::lock::{lock_frame, lock_init, lock_inv};
#[cfg(verus_only)]
use crate::vspec::word::WordPerm;
use core::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};
#[cfg(verus_only)]
use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;
#[cfg(verus_only)]
use vstd::resource::Loc;
use vstd::resource::set::GhostSetAuth;
#[cfg(verus_only)]
use vstd::resource::set::GhostSubset;
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
}

/// The facts [`Lazy::ensure`] needs of a table committed in `page` units,
/// which it keeps.
pub(super) open spec fn lazy_ok(l: Lazy, page: usize) -> bool {
    &&& l.spec_len() <= 0x4000_0000_0000_0000
    &&& l.spec_committed() <= l.spec_len() + page
    &&& l.spec_base() + l.spec_len() + page <= usize::MAX
}

impl TableInner {
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
/// not proved. `guards` only reads; the test
/// helper `corrupt_owner_word` (unverified) only commits through
/// `Lazy::ensure`.
pub(super) broadcast axiom fn axiom_table_lock_inv(v: TableInner)
    ensures
        #[trigger] lock_inv(v) == v.inv(),
;

/// The commit lock's ghost geometry never changes: nothing writes it.
pub(super) broadcast axiom fn axiom_table_lock_frame(init: TableInner, v: TableInner)
    ensures
        #[trigger] lock_frame(init, v) == (v.page == init.page && v.group == init.group && v.nslabs == init.nslabs),
;

/// The descriptor of `slab` in `t`'s descriptor array: what
/// [`SlabTable::desc`] returns. Names a descriptor so that a contract can
/// say which slab a `&SlabDesc` belongs to; nothing about its contents.
pub uninterp spec fn desc_at(t: SlabTable, slab: usize) -> SlabDesc;

/// The owner word of hand-out group `group` in `t`'s owner array: what
/// [`SlabTable::owner_word`] returns. Names the word, as [`desc_at`] names
/// a descriptor; nothing about its value.
pub(super) uninterp spec fn owner_word_at(t: SlabTable, group: usize) -> AtomicU16;

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
// Trusted: system calls (and `sys`' constants, outside `verus!`).
#[verifier::external_body]
fn commit(addr: usize, len: usize) -> Result<(), Errno> {
    unsafe { sys::mprotect(addr, len, PROT_READ | PROT_WRITE) }
}

#[verifier::external_body]
fn decommit(addr: usize, len: usize) -> Result<(), Errno> {
    unsafe { sys::mprotect(addr, len, PROT_NONE) }
}

#[verifier::external_body]
fn guard(addr: usize, len: usize) -> Result<(), Errno> {
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
    pub fn new(meta: &Meta, regions: &Regions, class: usize, guard_interval: u32) -> (r: Option<SlabTable>)
        requires
            regions.wf(),
            regions.separated(),
            *regions == process_regions(),
            class < NCLASSES,
            guard_interval != 1,
            guard_interval == 0 || exists|k: nat| k < 32 && guard_interval as nat == #[trigger] pow2(k),
        ensures
            r matches Some(t) ==> {
                &&& t.spec_class() == class
                &&& t.spec_nslabs() == (1usize << regions.lg_region) >> class_table()[class as int].lg_slab
                &&& meta.is_process() ==> t.meta_ok()
            },
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
        let descs = Lazy::reserve(meta, stagger + nslabs * stride, align)?;
        let bits = Lazy::reserve(meta, stagger + (nslabs * cls.slab_bits()).div_ceil(8), align)?;
        let owners = Lazy::reserve(meta, stagger + nslabs.div_ceil(group) * 2, align)?;
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
            sb: cls.slab_bits as int,
            nslots: cls.nslots as int,
            nbits: nslabs * cls.slab_bits,
            base: regions.class_base[class as int] as int,
            slab_size: (1usize << cls.lg_slab) as int,
            size: cls.size as int,
        };
        let tracked (slot_auth, all) = GhostSetAuth::<int>::new(crate::bitmap::slot_set(geo));
        proof {
            axiom_slot_loc(class as int, slot_auth.id());
        }
        let ghost slots = Slots {
            loc: slot_auth.id(),
            sb: geo.sb,
            nslots: geo.nslots,
            nbits: geo.nbits,
            base: geo.base,
            slab_size: geo.slab_size,
            size: geo.size,
        };
        let ghost nwords = (geo.nbits + 63) / 64;
        let ghost bits_base = (bits.spec_base() + stagger) as usize;
        proof {
            broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter, Set::lemma_map_contains};
            assert forall|b: int| 0 <= b < 64 * nwords && slots.real(b) implies all@.contains(#[trigger] slots.addr(b)) by {
                assert(Set::<int>::range(0, geo.nbits).filter(|b: int| geo.real(b)).contains(b));
                assert(geo.addr(b) == slots.addr(b));
            }
        }
        let tracked perms = axiom_fresh_words(bits_base, nwords as nat);
        let tracked word_invs = crate::bitmap::fresh_word_invs(perms, all, slots, bits_base, nwords as nat);
        Some(SlabTable {
            bits_base: bits.start() + stagger,
            owners_base: owners.start() + stagger,
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
            desc_base: descs.start() + stagger,
            inner: Padded(Lock::new(TableInner {
                descs,
                bits,
                owners,
                next_slab: 0,
                guards: 0,
                page: Ghost(page),
                group: Ghost(group),
                nslabs: Ghost(nslabs),
            })),
            _tail: [0; TABLE_TAIL],
            slot_auth: Tracked(slot_auth),
            word_invs: Tracked(word_invs),
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
/// private to this module and `SlabTable::new` is its one call.
axiom fn axiom_slot_loc(c: int, loc: Loc)
    ensures
        slot_loc(c) == loc;

/// Trusted: the permissions of the `n` words of the table at `base` that
/// `Meta::reserve_table` has just returned, each holding zero: the table
/// has never been written (`meta.rs`: carved once and never freed), and
/// nothing but the bitmap views reaches it. A second permission for one
/// word would let a proof write the word outside its invariant, so it is
/// private to this module and `SlabTable::new` is its one call, for its
/// own bitmap.
axiom fn axiom_fresh_words(base: usize, n: nat) -> (tracked r: Map<int, WordPerm>)
    ensures
        forall|j: int| 0 <= j < n ==> #[trigger] r.contains_key(j) && r[j].addr() == base + 8 * j && r[j].value() == 0;

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
    ///
    /// [`process_regions`]: crate::layout::process_regions
    #[verifier::type_invariant]
    pub(super) open spec fn inv(self) -> bool {
        &&& self.class < NCLASSES
        &&& class_table()[self.class as int].wf()
        &&& self.slab_bits == class_table()[self.class as int].slab_bits
        &&& self.slab_bits >= 64 ==> self.slab_bits % 64 == 0
        &&& self.lg_slabs_per_page < 64
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
        &&& self.base == process_regions().class_base[self.class as int]
        &&& self.nslabs as int == process_regions().spec_nslabs(self.class as int)
        &&& words_wf(self.word_invs@, self.spec_slots(), self.bits_base, self.spec_nwords())
        &&& self.spec_slots().loc == slot_loc(self.class as int)
    }

    /// The descriptor array, the class bitmap and the owner array lie in
    /// the metadata region (whole-allocator theorem 4): [`new`] reserves
    /// each from `meta` ([`Lazy::reserve`]) with room for every slab's
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
            sb: self.slab_bits as int,
            nslots: class_table()[self.class as int].nslots as int,
            nbits: self.nslabs * self.slab_bits,
            base: self.base as int,
            slab_size: (1usize << class_table()[self.class as int].lg_slab) as int,
            size: class_table()[self.class as int].size as int,
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
        let pg = (slab >> self.lg_slabs_per_page) as u32;
        (if pg == u32::MAX { 0u32 } else { (pg + 1) as u32 }) & self.guard_mask == 0
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
    pub fn guard_mask(&self) -> u32 {
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
        let mut g = self.inner.lock();
        g.owners
            .ensure(meta, page, stagger + group * 2 + 2)
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
        self.committed_slabs.load(Ordering::Acquire)
    }

    /// Guard slabs committed so far.
    pub fn guards(&self) -> usize {
        self.inner.lock().guards
    }

    /// Descriptor of a handed-out slab. Aborts for a slab that was never
    /// handed out rather than faulting on its `PROT_NONE` descriptor page.
    #[inline]
    pub fn desc(&self, slab: usize) -> (r: &SlabDesc)
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
        if slab >= self.committed_slabs.load(Ordering::Relaxed) {
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
        slab < self.committed_slabs.load(Ordering::Relaxed)
    }

    /// Descriptor of `slab` without a watermark check.
    ///
    /// # Safety
    /// `slab` must be below a value `committed_slabs` has held: descriptors
    /// up to that count are committed and initialised (or zero, for guard
    /// slabs) before the watermark moves.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn desc_unchecked(&self, slab: usize) -> (r: &SlabDesc)
        ensures
            *r == desc_at(*self, slab),
    {
        // SAFETY: guaranteed by the caller.
        unsafe { &*((self.desc_base + slab * self.desc_stride as usize) as *const SlabDesc) }
    }

    /// The slot bits of a handed-out slab (design section 4). Aborts for a
    /// slab that was never handed out.
    #[inline]
    pub fn bits(&self, slab: usize) -> (r: AtomicBitmap<'_>)
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

    /// Trusted: the view's words are words `first / 64 ..` of the class
    /// bitmap at `bits_base` (so `AtomicBitmap`'s type invariant ties them
    /// to the table's word invariants), with `first = slab * slab_bits`;
    /// the address arithmetic below is what says so, for a slab below the
    /// watermark, which never passes `nslabs` (`take_group`).
    ///
    /// # Safety
    /// As [`desc_unchecked`](SlabTable::desc_unchecked).
    #[inline]
    #[verifier::external_body]
    unsafe fn bits_unchecked(&self, slab: usize) -> (r: AtomicBitmap<'_>)
        ensures
            bits_shape(r, class_table()[self.spec_class() as int].slab_bits as usize),
            bits_ghost(r, *self, slab),
    {
        let nbits = self.slab_bits as usize;
        let first = slab * nbits;
        let word = (self.bits_base + (first / 64) * 8) as *const AtomicU64;
        if nbits >= 64 {
            debug_assert!(first % 64 == 0);
            // SAFETY: the reservation holds `nslabs * slab_bits` bits and
            // the group's words were committed before the watermark moved.
            AtomicBitmap::from_words(unsafe { core::slice::from_raw_parts(word, nbits / 64) }, Ghost::assume_new(), Ghost::assume_new(), Tracked::assume_new())
        } else {
            // SAFETY: as above, one word.
            AtomicBitmap::sub_word(unsafe { &*word }, (first % 64) as u32, nbits, Ghost::assume_new(), Ghost::assume_new(), Tracked::assume_new())
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
    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn is_set_unchecked(&self, slab: usize, slot: usize) -> bool {
        let bit = slab * self.slab_bits as usize + slot;
        let word = (self.bits_base + (bit / 64) * 8) as *const AtomicU64;
        // SAFETY: the caller guarantees the word is inside the handed-out
        // part of the bitmap.
        (unsafe { (*word).load(Ordering::Relaxed) } >> (bit % 64)) & 1 == 1
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
    /// [`is_handed_out`]: SlabTable::is_handed_out
    #[verifier::external_body]
    #[inline(always)]
    pub unsafe fn owner_unchecked(&self, slab: usize) -> u16 {
        let word = (self.owners_base + (slab >> self.lg_group) * 2) as *const AtomicU16;
        // SAFETY: the owner word of a handed-out group was committed and
        // written before the watermark moved.
        unsafe { (*word).load(Ordering::Relaxed) }
    }

    /// The arena that owns `slab`. Aborts for a slab that was never handed
    /// out.
    #[inline]
    pub fn owner(&self, slab: usize) -> u16 {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        // SAFETY: `slab` is below the acquire-loaded watermark.
        unsafe { self.owner_unchecked(slab) }
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
    /// a slot start of the class region; a bit index up to `slab_bits`
    /// reaches here from a claim, whose padding bits above `nslots` are
    /// never claimable (`class::axiom_no_padding_won` names the assumption).
    #[inline]
    pub(super) fn slot_ptr(&self, slab: usize, slot: usize) -> (r: *mut u8)
        requires
            slab < self.spec_nslabs(),
            slot < class_table()[self.spec_class() as int].slab_bits,
        ensures
            r@.addr == process_regions().slot_at(self.spec_class() as int, slab as int, slot as int),
            slot < class_table()[self.spec_class() as int].nslots
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
        }
        crate::vspec::addr::addr_to_ptr(a + slot * cls.size())
    }

    /// Slab and slot of a pointer that must be a slot start in this class's
    /// region. Aborts otherwise.
    #[inline]
    pub fn locate(&self, p: *mut u8) -> (r: (usize, usize))
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
    pub fn check_located(&self, slab: usize, slot: usize, p: usize) -> u16
        requires
            slot < class_table()[self.spec_class() as int].nslots,
    {
        self.check_live(slab, slot, p);
        // SAFETY: `check_live` returned, so `slab` is below the watermark.
        unsafe { self.owner_unchecked(slab) }
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
            slot < class_table()[self.spec_class() as int].nslots,
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
    fn not_live(&self, slab: usize, slot: usize, p: usize) -> ! {
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
    pub(super) fn commit_slab(&self, s: usize) -> (r: Option<bool>)
        requires
            s < self.spec_nslabs(),
        ensures
            r matches Some(usable) ==> usable == !self.spec_is_guard(s),
    {
        broadcast use axiom_table_lock_inv;
        proof {
            use_type_invariant(self);
        }
        let cls = self.class();
        let page = 1usize << self.lg_page;
        let mut g = self.inner.lock();
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
    fn install_guard(&self, s: usize, addr: usize, page: usize, len: usize) {
        if guard(addr, page).is_ok() {
            return;
        }
        if decommit(addr, len).is_err() {
            guard_unprotectable(&self.class, &s);
        }
    }

}

/// [`SlabTable::install_guard`]'s abort: the guard slab of `class` at `s`
/// could be neither guarded nor re-protected. Inlined and
/// `track_caller`, so the caller keeps the code and the location its
/// `panic!` had in place; the arguments are the references `panic!`
/// formats.
/// Trusted: `panic!` formats its message; the `-> !` is the whole
/// specification, and honest.
#[inline(always)]
#[track_caller]
#[verifier::external_body]
fn guard_unprotectable(class: &u16, s: &usize) -> ! {
    panic!("class {}: cannot re-protect guard slab {}", *class, *s)
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
    /// [`ClassState::next_fresh_slab`](super::ClassState::next_fresh_slab) adds its slabs to the ghost; that no
    /// other arena's ghost holds it rests on the same bridge's assumption
    /// for their lock values.
    ///
    /// [`commit_slab`]: SlabTable::commit_slab
    /// [`next_slab_locked`]: SlabTable::next_slab_locked
    // Verus needs the explicit reborrow and a loop with an invariant.
    #[allow(clippy::explicit_auto_deref, clippy::while_let_loop)]
    pub(super) fn take_group(&self, meta: &Meta, arena: u16, sets: &mut Inner, sets_page: usize) -> (r: Option<(usize, usize)>)
        requires
            old(sets).table@ == *self,
            old(sets).arena@ == arena,
        ensures
            final(sets).same_sets(*old(sets)),
            r matches Some((s, e)) ==> s < e <= self.spec_nslabs()
                && self.spec_group_range(s >> self.lg_group) == (s, e)
                && (forall|x: nat| #[trigger] old(sets).owns(x) ==> x < s)
                && old(sets).group_end <= s,
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
        let mut g = self.inner.lock();
        let s = self.next_slab_locked(&*g, Ghost(*sets));
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
            #[cfg(verus_only)]
            watermark_mismatch(&class, &s, &self.committed_slabs.load(Ordering::Relaxed));
            #[cfg(not(verus_only))]
            panic!("check_zero: class {class}: take_group at next_slab {s} but the watermark is {}", self.committed_slabs.load(Ordering::Relaxed));
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
        g.descs.ensure(meta, page, stagger + end * desc_stride(class)).ok()?;
        g.bits.ensure(meta, page, stagger + (end * cls.slab_bits()).div_ceil(8)).ok()?;
        g.owners.ensure(meta, page, stagger + ((end - 1) >> self.lg_group) * 2 + 2).ok()?;
        sets.ensure_sets(meta, sets_page, end - 1).ok()?;
        // The owner word and the descriptors' arena field record the same
        // fact (design section 4) and are written together only here, so
        // a group about to be handed out must show them agreeing: both
        // zero for never-touched memory. A disagreement is a wild write
        // into one of the two arrays and aborts before the group is used.
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
        let mut slabs = s..end;
        loop
            invariant
                slabs.start <= slabs.end == end,
                end <= self.spec_nslabs(),
                self.lg_group < 64,
                g@ == g0,
                *cls == class_table()[self.spec_class() as int],
                cls.wf(),
            decreases slabs.end - slabs.start,
        {
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
                            let d = SlabDesc::at(self.desc_base, class, slab);
                            let (st, sh) = d.state_bytes();
                            if sh != 0 || d.live() != 0 || st != 0 {
                                #[cfg(verus_only)]
                                slab_not_zero(&class, &slab, &s, &end, &arena, d, &st, &sh);
                                #[cfg(not(verus_only))]
                                panic!(
                                    "check_zero: class {class}: take_group re-hands slab {slab} (group {s}..{end}, arena {arena}): descriptor at {:#x} has state {st} shadow {sh} live {} arena {} dirty_at {}",
                                    d as *const SlabDesc as usize,
                                    d.live(),
                                    d.arena(),
                                    d.dirty_at()
                                );
                            }
                        }
                        SlabDesc::at(self.desc_base, class, slab).init(arena);
                        self.bits_unchecked(slab).init(cls.nslots());
                    }
                }
            }
        }
        owner_word.store(arena, Ordering::Relaxed);
        g.next_slab = end;
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

    /// Bridge (trusted): under the commit lock (`t`, its value), with a
    /// class lock over this table held by the caller (`v`, its value),
    /// every slab `v`'s ghost owns is below the next slab to hand out, and
    /// so is `v`'s current group's end. The field read it compiles to is
    /// `next_slab`'s. Assumes that ownership exists only below the
    /// watermark, a fact about every arena of the table at once that the
    /// verified writers keep but no single lock invariant can state:
    /// `next_slab` only grows (`take_group` moves it to the end of the
    /// group it hands out), a group's slabs join an arena's ghost only in
    /// `next_fresh_slab`, right after `take_group` handed them out, with
    /// that group as its current group, and nothing else adds owned slabs
    /// or current groups (`ClassState::new` starts with nothing owned and
    /// an empty group).
    /// Any other write of a ghost `states` key would break it.
    #[allow(unused_variables)]
    #[inline(always)]
    #[verifier::external_body]
    fn next_slab_locked(&self, t: &TableInner, Ghost(v): Ghost<Inner>) -> (r: usize)
        requires
            v.table@ == *self,
        ensures
            r == t.next_slab,
            forall|x: nat| #[trigger] v.owns(x) ==> x < r,
            v.group_end <= r,
    {
        t.next_slab
    }

    /// The owner word of `group`.
    ///
    /// # Safety
    /// The group must have been handed out (its owner page is committed).
    /// Trusted: an address turned into a reference.
    #[inline]
    #[verifier::external_body]
    pub(super) unsafe fn owner_word(&self, group: usize) -> (r: &AtomicU16)
        ensures
            *r == owner_word_at(*self, group),
    {
        // SAFETY: the caller guarantees the page is committed.
        unsafe { &*((self.owners_base + group * 2) as *const AtomicU16) }
    }
}

/// `take_group`'s debug check that the watermark is group-aligned, which
/// the precondition (proved from [`TableInner::inv`]) says it is.
/// Inlined and `track_caller`, as [`guard_unprotectable`].
/// Trusted: Verus does not support `debug_assert_eq!` (its failure path
/// is `core::panicking::assert_failed`).
#[inline(always)]
#[track_caller]
#[verifier::external_body]
fn watermark_aligned(s: usize, lg_group: u8)
    requires
        lg_group < 64,
        s & sub(1usize << lg_group, 1) == 0,
{
    debug_assert_eq!(s & ((1 << lg_group) - 1), 0, "take_group: the watermark is group-aligned");
}

/// `take_group`'s `check_zero` abort: the watermark the table publishes
/// disagrees with the next slab under the commit lock.
///
/// Verification only (`verus_only`): Verus sees this call where the build
/// keeps the `panic!` in place, because every out-of-line or inlined helper
/// tried changed the code of the refill path `take_group` is inlined into
/// (`next_fresh_slab`) on both architectures. Trusted: the `-> !` is the
/// whole specification, and the `panic!` it stands for never returns; the
/// two copies of the message must be kept in step.
#[cfg(verus_only)]
#[verifier::external_body]
fn watermark_mismatch(class: &usize, s: &usize, watermark: &usize) -> ! {
    panic!("check_zero: class {}: take_group at next_slab {} but the watermark is {}", *class, *s, *watermark)
}

/// `take_group`'s `check_zero` abort: a slab it is about to hand out has a
/// descriptor that is not all zero. Verification only, as
/// [`watermark_mismatch`].
#[cfg(verus_only)]
#[verifier::external_body]
fn slab_not_zero(class: &usize, slab: &usize, s: &usize, end: &usize, arena: &u16, d: &SlabDesc, st: &u8, sh: &u8) -> ! {
    panic!(
        "check_zero: class {}: take_group re-hands slab {} (group {}..{}, arena {}): descriptor at {:#x} has state {} shadow {} live {} arena {} dirty_at {}",
        *class,
        *slab,
        *s,
        *end,
        *arena,
        d as *const SlabDesc as usize,
        *st,
        *sh,
        d.live(),
        d.arena(),
        d.dirty_at()
    );
}

} // verus!
