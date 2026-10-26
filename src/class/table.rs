//! The per-class [`SlabTable`]: the class region's descriptor array,
//! class bitmap and group owner array, the hand-out watermark and the
//! commit lock; groups go to arenas here (`take_group`) and slabs are
//! committed here (`commit_run`, `commit_slab`).

use super::Inner;
use super::abort::{guard_free, invalid_free, invalid_pointer, never_committed, owner_mismatch};
use super::sets::Lazy;
use crate::bitmap::AtomicBitmap;
use crate::layout::{CLASSES, Class, Regions};
use crate::lock::{Lock, PAD_ALIGN, Padded, RawLock};
use crate::meta::Meta;
use crate::purge;
use crate::slab::{DESC_ALIGN, SlabDesc, desc_stride};
use crate::sys::{self, Errno, MADV_GUARD_INSTALL, PROT_NONE, PROT_READ, PROT_WRITE};
use core::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};
use vstd::prelude::{Ghost, Tracked};

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
    /// A copy of the watermark the `free` fast path reads from its
    /// per-class line (`heap::ClassLine`), stored right after the
    /// watermark itself, so it is never ahead of it; null until
    /// [`SlabTable::mirror_watermark`].
    pub(super) mirror: *const AtomicUsize,
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
pub const fn desc_stagger(class: usize) -> usize {
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

/// Bytes a run holds at most ([`SlabTable::commit_run`]): the default
/// guard interval of 64 KiB slabs, so a run is as long with guards on or
/// off, and a class of large slabs commits one at a time.
pub(super) const RUN_BYTES: usize = 512 << 10;

// SAFETY (all three): `addr..addr + len` is inside a class region this
// allocator reserved (the callers pass slab or page-group ranges below
// the region's end), so changing its protection or installing a guard
// affects only pages nothing else in the process maps.
fn commit(addr: usize, len: usize) -> Result<(), Errno> {
    unsafe { sys::mprotect(addr, len, PROT_READ | PROT_WRITE) }
}

fn decommit(addr: usize, len: usize) -> Result<(), Errno> {
    unsafe { sys::mprotect(addr, len, PROT_NONE) }
}

fn guard(addr: usize, len: usize) -> Result<(), Errno> {
    unsafe { sys::madvise(addr, len, MADV_GUARD_INSTALL) }
}

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

/// `(*w).store(arena, Relaxed)` of the owner word of the group
/// [`SlabTable::take_group`] hands out, the only store to an owner word
/// (the test helper `corrupt_owner_word` aside).
///
/// # Safety
/// `w` is the owner word of the group at the watermark, committed.
#[inline(always)]
unsafe fn store_owner(w: *const AtomicU16, arena: u16) {
    // SAFETY: guaranteed by the caller.
    unsafe { (*w).store(arena, Ordering::Relaxed) }
}

/// The hand-out group's geometry, as [`SlabTable::new`] asserts it:
/// `group` is a power of two that is whole page groups. Inlined and
/// `track_caller`, so the caller keeps the code, the message and the
/// location of the `assert!` it had in place.
#[inline(always)]
#[track_caller]
fn group_geometry(group: usize, slabs_per_page: usize) {
    assert!(group.is_power_of_two() && group.is_multiple_of(slabs_per_page));
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
/// boundary. Inlined and `track_caller`, as [`group_geometry`].
#[inline(always)]
#[track_caller]
fn watermark_aligned(s: usize, lg_group: u8) {
    debug_assert_eq!(
        s & ((1 << lg_group) - 1),
        0,
        "take_group: the watermark is group-aligned"
    );
}

/// [`SlabTable::install_guard`]'s abort: the guard slab of `class` at `s`
/// could be neither guarded nor re-protected. Inlined and `track_caller`, as
/// [`group_geometry`], so the panic names the caller's line.
#[inline(always)]
#[track_caller]
fn guard_unprotectable(class: &u16, s: &usize) -> ! {
    panic!("class {}: cannot re-protect guard slab {}", *class, *s)
}

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
    pub fn new(
        meta: &Meta,
        regions: &Regions,
        class: usize,
        guard_interval: u32,
    ) -> Option<SlabTable> {
        assert!(class < CLASSES.len(), "class: index out of range");
        assert!(
            guard_interval != 1,
            "class: a guard interval of 1 leaves no slabs to allocate"
        );
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
        assert!(slabs_per_page.is_power_of_two() && nslabs.is_multiple_of(slabs_per_page));
        let group = cls.slabs_per_line().max(slabs_per_page);
        group_geometry(group, slabs_per_page);
        let stride = desc_stride(class);
        let descs = Lazy::reserve(meta, stagger + nslabs * stride, align)?;
        let bits = Lazy::reserve(
            meta,
            stagger + (nslabs * cls.slab_bits()).div_ceil(8),
            align,
        )?;
        let owners = Lazy::reserve(meta, stagger + nslabs.div_ceil(group) * 2, align)?;
        let bits_base = bits.start() + stagger;
        let owners_base = owners.start() + stagger;
        let desc_base = descs.start() + stagger;
        let inner = Padded(Lock::new(
            TableInner {
                descs,
                bits,
                owners,
                next_slab: 0,
                guards: 0,
                mirror: core::ptr::null(),
            },
            Ghost::assume_new(),
        ));
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
        })
    }

    #[inline]
    pub fn class(&self) -> &'static Class {
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
    pub fn is_guard_slab(&self, slab: usize) -> bool {
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

    /// Keep `*mirror` equal to the hand-out watermark from now on: it is
    /// set to the current value here and stored after the watermark at
    /// every hand-out, under the commit lock, so a reader of the copy never
    /// sees a value the watermark has not held (`heap::ClassLine`).
    ///
    /// # Safety
    /// `mirror` must stay valid for as long as this table is used.
    pub unsafe fn mirror_watermark(&self, mirror: *const AtomicUsize) {
        let mut g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
        let wm = self.watermark(Ordering::Relaxed);
        // SAFETY: guaranteed by the caller.
        unsafe { (*mirror).store(wm, Ordering::Release) };
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

    /// Guard slabs committed so far.
    pub fn guards(&self) -> usize {
        self.inner
            .lock(Tracked::assume_new(), Tracked::assume_new())
            .guards
    }

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

    #[inline]
    pub(super) fn slab_addr(&self, slab: usize) -> usize {
        self.base + (slab << self.class().lg_slab)
    }

    #[inline]
    pub(super) fn slot_ptr(&self, slab: usize, slot: usize) -> *mut u8 {
        (self.slab_addr(slab) + slot * self.class().size()) as *mut u8
    }

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
    /// slab, in `sets_page` units, before anything changes; if that fails
    /// nothing has happened. Returns the group's slab range, `None` when
    /// the region or the metadata region is exhausted. The slabs
    /// themselves are committed by the arena as its cursor reaches them, a
    /// run at a time ([`commit_run`]).
    ///
    /// [`commit_run`]: SlabTable::commit_run
    // Verus needs the explicit reborrow and a loop with an invariant.
    #[allow(clippy::explicit_auto_deref, clippy::while_let_loop)]
    pub(super) fn take_group(
        &self,
        meta: &Meta,
        arena: u16,
        sets: &mut Inner,
        sets_page: usize,
    ) -> Option<(usize, usize)> {
        let cls = self.class();
        let page = 1usize << self.lg_page;
        let class = self.class as usize;
        let stagger = desc_stagger(class);
        let mut g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
        let s = g.next_slab;
        if s >= self.nslabs {
            return None;
        }
        watermark_aligned(s, self.lg_group);
        let (_, end) = self.group_range(s >> self.lg_group);
        if purge::check_zero() && s != self.committed_slabs.load(Ordering::Relaxed) {
            panic!(
                "check_zero: class {class}: take_group at next_slab {s} but the watermark is {}",
                self.committed_slabs.load(Ordering::Relaxed)
            );
        }
        g.descs
            .ensure(meta, page, stagger + end * desc_stride(class))
            .ok()?;
        g.bits
            .ensure(meta, page, stagger + (end * cls.slab_bits()).div_ceil(8))
            .ok()?;
        g.owners
            .ensure(meta, page, stagger + ((end - 1) >> self.lg_group) * 2 + 2)
            .ok()?;
        sets.partial.ensure(meta, sets_page, end - 1).ok()?;
        sets.empty.ensure(meta, sets_page, end - 1).ok()?;
        sets.purged.ensure(meta, sets_page, end - 1).ok()?;
        // The owner word and the descriptors' arena field record the same
        // fact (design section 4) and are written together only here, so
        // a group about to be handed out must show them agreeing: both
        // zero for never-touched memory. A disagreement is a wild write
        // into one of the two arrays and aborts before the group is used.
        // SAFETY: the owner page for the group was committed above.
        let owner_word = unsafe { self.owner_word(s >> self.lg_group) };
        // SAFETY: as above.
        let previous = unsafe { (*owner_word).load(Ordering::Relaxed) };
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
            let recorded = unsafe { SlabDesc::at(self.desc_base, class, slab).arena() };
            if recorded != previous {
                owner_mismatch(class, s >> self.lg_group, slab, previous, recorded);
            }
        }
        let gi = &mut *g;
        let mut slabs = s..end;
        loop {
            let slab = match slabs.next() {
                Some(slab) => slab,
                None => break,
            };
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
                        panic!(
                            "check_zero: class {class}: take_group re-hands slab {slab} (group {s}..{end}, arena {arena}): descriptor at {:#x} has state {st} shadow {sh} live {} arena {} dirty_at {}",
                            super::verify::desc_ptr_addr(d),
                            d.live(),
                            d.arena(),
                            d.dirty_at()
                        );
                    }
                }
                SlabDesc::at(self.desc_base, class, slab).init(arena);
                let _ = self.bits_unchecked(slab).init(
                    cls.nslots(),
                    Ghost::assume_new(),
                    Tracked::assume_new(),
                );
            }
        }
        // SAFETY: the owner page for the group was committed above.
        unsafe { store_owner(owner_word, arena) };
        gi.next_slab = end;
        self.committed_slabs.store(end, Ordering::Release);
        if !gi.mirror.is_null() {
            // SAFETY: `mirror_watermark`'s caller keeps the copy alive as
            // long as the table.
            unsafe { (*gi.mirror).store(end, Ordering::Release) };
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

    /// The owner word of hand-out group `group`.
    ///
    /// # Safety
    /// `group` is a group of the region; the word is committed only once
    /// the group is handed out, or under the commit lock in
    /// [`take_group`](SlabTable::take_group).
    #[inline]
    pub(super) unsafe fn owner_word(&self, group: usize) -> *const AtomicU16 {
        (self.owners_base + group * 2) as *const AtomicU16
    }

    /// The slab range of `group`: `group << lg_group` and the `group`
    /// slabs from there, clipped to the class's slab count (the last
    /// group of a region may be short).
    #[inline]
    pub(super) fn group_range(&self, group: usize) -> (usize, usize) {
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
    pub(super) fn starts_run(&self, s: usize) -> bool {
        !self.runs()
            || s & ((1 << self.lg_group) - 1) == 0
            || self.is_guard_slab(s)
            || (s & (self.run_slabs() - 1) == 0 && !self.is_guard_slab(s - 1))
    }

    /// The most slabs a run holds: [`RUN_BYTES`] of them, at least one (a
    /// power of two, as both are).
    #[inline]
    fn run_slabs(&self) -> usize {
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
    pub(super) fn runs(&self) -> bool {
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
    /// [`starts_run`]: SlabTable::starts_run
    /// [`run_slabs`]: SlabTable::run_slabs
    pub(super) fn commit_run(&self, s: usize, limit: usize) -> Option<bool> {
        if !self.runs() {
            return self.commit_slab(s);
        }
        let cls = self.class();
        let guard = self.is_guard_slab(s);
        let end = self.run_end(s, limit);
        commit(self.slab_addr(s), (end - s) << cls.lg_slab).ok()?;
        if guard {
            let page = 1usize << self.lg_page;
            self.install_guard(s, self.slab_addr(s), page, cls.slab_size());
            self.inner
                .lock(Tracked::assume_new(), Tracked::assume_new())
                .guards += 1;
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
    pub(super) fn run_end(&self, s: usize, limit: usize) -> usize {
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
    /// slab stays uncommitted and may be retried).
    // Refill path: left as written so its codegen does not move.
    #[allow(clippy::manual_is_multiple_of)]
    pub(super) fn commit_slab(&self, s: usize) -> Option<bool> {
        let cls = self.class();
        let page = 1usize << self.lg_page;
        let mut g = self
            .inner
            .lock(Tracked::assume_new(), Tracked::assume_new());
        let addr = self.slab_addr(s);
        // A slab smaller than a page is committed with its whole page
        // group when the group's first slab comes up; a group is whole
        // page groups and the arena walks it in order, so the later slabs
        // of the page group find it committed.
        let spp = self.slabs_per_page as usize;
        let len = cls.slab_size().max(page);
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
    fn commit_page_group(&self, addr: usize, len: usize, mode: crate::thp::Mode) -> Option<()> {
        if mode == crate::thp::Mode::Chunks {
            let cls = self.class();
            let pmd = crate::thp::pmd_size();
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
