//! [`Shared`], the process-wide allocator state every heap works against,
//! and the checks it performs for the fast and slow paths; the thread-exit
//! key and the thread id, clock and seed a heap reads, and the per-class
//! [`ClassLine`] the `free` fast paths read, with the sized path's
//! check.

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};

#[cfg(verus_keep_ghost)]
use crate::arena::published;
use crate::arena::{Arena, Arenas, Held, elem_at, field_at, fits};
#[cfg(verus_keep_ghost)]
use crate::arena::{
    elems_at, elems_init, elems_uninit, field_fits, field_ptr, split_array, storage_ok, take_field,
};
use crate::class::SlabTable;
#[cfg(verus_keep_ghost)]
use crate::class::{aset, distinct, ptr_addrs, slot_loc};
use crate::config::Config;
use crate::huge::{Huge, RunState};
#[cfg(verus_keep_ghost)]
use crate::layout::class_table;
use crate::layout::{CLASSES, Kind, NCLASSES, Regions};
use crate::lock::{Lock, LockOrder, RawLock};
#[cfg(verus_keep_ghost)]
use crate::lock::{
    axiom_entry, rank_arenas, rank_class, rank_huge, rank_meta, rank_stats, rank_table,
    rank_tcaches,
};
use crate::meta::Meta;
use crate::policy::Policy;
use crate::stats::Snapshot;
#[cfg(verus_keep_ghost)]
use crate::stats::axiom_snapshot_lock_inv;
use crate::sys::cgroup::{CellSeen, Cgroup, Deferred};
#[cfg(verus_keep_ghost)]
use crate::vspec::atom::usize_id;
use crate::vspec::atom::{UsizePerm, usize_ptr};
#[cfg(verus_keep_ghost)]
use crate::vspec::once;
use crate::vspec::once::{Once, Step};
#[cfg(verus_keep_ghost)]
use crate::vspec::ptr::{axiom_ptr_add_addr, axiom_raw_valid, in_alloc, ptr_add};
use crate::vspec::unbacked::Unbacked;
use vstd::prelude::{Ghost, int};

use super::heap_destructor;
use super::live::Live;
use super::tcaches::Tcaches;
use super::{FREE_ROUTE, Heap, PUSH_PAC, PUSH_SCAN, PUSH_ZERO, invalid_free};
use crate::arena::Published;
use crate::vspec::flags::RouteByte;
use crate::vspec::zero::Zeroed;
#[cfg(verus_keep_ghost)]
use vstd::layout::size_of;
use vstd::map::Map;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, verus};
use vstd::raw_ptr::{PointsTo, ptr_mut_write, ptr_ref};
use vstd::resource::set::{GhostSingleton, GhostSubset};

verus! {

/// The cache GC period (design 6.4): 10 s.
pub const GC_INTERVAL_NS: u64 = 10_000_000_000;

/// Process-wide allocator state that every heap works against: the regions,
/// one [`SlabTable`] per class, arena 0, the metadata allocator, the huge
/// allocator, the arena and explicit-cache registries and the thread-exit
/// key. Built in place by [`Shared::init`] because the class states borrow
/// `meta`, `regions` and `tables`.
///
/// `repr(C)` fixes the order: the per-class lines (read by every `free`),
/// then `free_flags` and `regions`, read by every `malloc` and `free`,
/// then the line-aligned tables and arena 0's class states, so the locks
/// inside `meta` and `huge` (taken on heap creation and huge allocations)
/// never share a cache line with them.
#[repr(C)]
pub struct Shared {
    /// What the `free` and sized `free` fast paths read per class, one
    /// line each ([`ClassLine`]). At offset 0: the fast paths hold a
    /// class's line as a reference, and LLVM builds its address as
    /// `Shared + class * 64` plus the array's offset, one more `add`
    /// whenever that offset is not zero.
    pub(crate) lines: [ClassLine; NCLASSES],
    /// The one byte the `free` fast path loads for its dispatch:
    /// [`push_flags`](Shared::push_flags) plus [`FREE_ROUTE`], set once a
    /// second arena exists. Until then every slab and every heap is arena
    /// 0 and the `free` fast path skips the owner-word load; with
    /// `FREE_ROUTE` in the byte, the default configuration's test against
    /// zero sends `free` to the arena test, so a single-arena `free` pays
    /// no separate load and branch for it. Relaxed: a slot of arena `a > 0`
    /// can only be freed after it was allocated, which happened after `a`
    /// was built and this bit stored, and the program itself orders that
    /// allocation before the free; so any thread freeing such a slot
    /// already sees the bit. The sized path and `malloc` read
    /// `push_flags`, which never changes after init.
    ///
    /// Right before `regions`, so it shares a cache line with the
    /// `Regions` fields that `set_slot` reads on every `free`
    /// (`superblock`, `lg_stride`): after `Regions` it would sit on its own
    /// line, and larson's working set turns every extra line into an L2
    /// hit.
    pub(crate) free_flags: RouteByte,
    /// The opt-in hardening a push applies, as [`PUSH_SCAN`]
    /// (`double_free_scan`, design 6.3 step 3: every push compares the
    /// slot against the top four cache entries and aborts on a match) and
    /// [`PUSH_ZERO`] (`zero_on_free`: the slot's class-size bytes are
    /// cleared before the push, and a huge run's usable bytes before its
    /// release), [`PUSH_PAC`] (`pac_pointers`: signed cache and heap
    /// pointers where the CPU has pointer authentication; read by the pop
    /// too). Written once by `init`; kept on the line every `free` reads,
    /// and zero in the default configuration on every host (all three
    /// options default to off), so the fast path tests one byte and takes
    /// one predicted branch.
    pub(crate) push_flags: u8,
    /// `!(regions.region_len() - 1)`: the bits of an offset from a class
    /// base that put it past the class region, read by the sized check
    /// ([`ClassLine::sized_ok`]) on the line `push_flags` is on.
    pub(crate) region_hi: usize,
    pub regions: Regions,
    pub(crate) tables: [SlabTable; NCLASSES],
    pub arena0: Arena,
    pub meta: Meta,
    pub huge: Huge,
    /// Cache GC period, [`GC_INTERVAL_NS`] (the tests shorten it).
    pub gc_interval_ns: u64,
    /// The cgroup policy's outcome (design 11, `policy.rs`): the band,
    /// the keep ratio, the pending full purge. Next to `gc_interval_ns`
    /// so the periodic tick's `immediate` load hits the line it reads
    /// anyway; written at most once per 10 ms by the sampler.
    pub policy: crate::policy::Policy,
    /// The parsed `malloc_conf` (design section 12) as `init` applied it
    /// (`narenas` resolved from the CPU count, `thp` forced to `never`
    /// under a small cgroup limit). Read back by `opt.*` and the report,
    /// and on the slow paths for the cache ladder (`cache_cap`).
    pub config: Config,
    pub(crate) key: ThreadKey,
    pub arenas: Arenas,
    pub tcaches: Tcaches,
    /// Process-wide statistics as of the last `epoch` (`stats.rs`).
    pub stats: Lock<crate::stats::Snapshot>,
    /// The memory cgroup as last sampled (design 11; `sys::cgroup`),
    /// discovered after init unless init needs it (`Deferred`): read it
    /// through [`Shared::cgroup`] or [`Shared::cgroup_now`]. Cold: written
    /// by discovery and by the rate-limited sampler on slow paths, read by
    /// `stats.cgroup.*` and the reports. Last, well away from the lines the
    /// fast paths read.
    pub(crate) cgroup: crate::sys::cgroup::Deferred,
}

/// The address of the process's `Shared` storage (`api`'s `SHARED`).
/// Uninterpreted.
pub uninterp spec fn shared_storage() -> *mut Shared;

/// The permission to the whole `Shared` [`Shared::init`] built, frozen
/// ([`axiom_shared_built`]), once it has: what `init` hands back to its
/// caller, which publishes it (`api::Published`).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct Built {
    pub tracked perm: Option<&'static PointsTo<Shared>>,
}

impl Built {
    /// The permission is to the `Shared` at `p`, initialised, and holds
    /// the value built there ([`Shared::built_at`]).
    pub open spec fn at(&self, p: *mut Shared) -> bool {
        self.perm matches Some(r) && r.ptr() == p && r.is_init() && r.value() == shared_at(p)
    }
}

/// The `Shared` built in place at `p`: [`Shared::init`] writes each field
/// once, before anything reads it, and afterwards nothing writes a field
/// but through its atomics, locks and cells, so the value is one value.
/// Uninterpreted; [`axiom_shared_regions`] and [`axiom_shared_meta`] fix
/// the fields `built` names from the permissions `init` wrote them
/// through before the build is done, and [`axiom_shared_built`] the whole
/// value.
pub uninterp spec fn shared_at(p: *mut Shared) -> Shared;

/// Assumption: one `Shared` per process. The process's regions
/// ([`process_regions`](crate::layout::process_regions)) are those of
/// the `Shared` in its storage, which `api::init` builds once, before it
/// publishes `READY`. `process_regions` is uninterpreted and no other
/// axiom relates it to a value, so this defines it. The unit tests build
/// further `Shared`s elsewhere; the assumption is about the shipped
/// library.
axiom fn axiom_process_shared()
    ensures
        crate::layout::process_regions() == shared_at(shared_storage()).regions,
;

impl Shared {
    /// [`shared_storage`], for modules outside `heap`.
    pub open spec fn storage() -> *mut Shared {
        shared_storage()
    }

    /// [`shared_at`], for modules outside `heap`.
    pub open spec fn built_at(p: *mut Shared) -> Shared {
        shared_at(p)
    }

    /// What `Shared::init` establishes of the value it builds, and nothing
    /// changes afterwards (`regions`, `meta` and `stats` are written
    /// once, before the `Shared` is published): the address-space layout is well formed and
    /// separated and is the process's ([`crate::layout::process_regions`]),
    /// and `meta` allocates from the metadata region and is the process's
    /// (its claims to reserved pages are
    /// [`process_pages_id`](crate::meta::process_pages_id)'s, on the
    /// regions' page), and the statistics lock is at its rank
    /// ([`rank_stats`](crate::lock::rank_stats)).
    pub closed spec fn built(&self) -> bool {
        &&& self.regions.wf()
        &&& self.regions == crate::layout::process_regions()
        &&& self.regions.separated()
        &&& self.meta.spec_base() == self.regions.meta_base
        &&& self.meta.spec_len() == self.regions.meta_len
        &&& self.meta.pages_id() == crate::meta::process_pages_id()
        &&& self.meta.page() == (1usize << self.regions.lg_page)
        &&& self.stats.rank() == crate::lock::rank_stats()
    }

    /// The value is as `Shared::init` built it ([`Shared::built`]), and the
    /// allocator is published ([`crate::api::ready`]): a `&Shared` is handed
    /// out only from `READY` on. Assumed by every verified function that
    /// takes a `&Shared`.
    pub closed spec fn wf(&self) -> bool {
        &&& self.built()
        &&& crate::api::ready()
    }

    /// [`Shared::wf`] unfolded, for the rest of the crate: the process's
    /// well-formed, separated regions and its metadata allocator.
    pub(crate) proof fn lemma_wf(&self)
        requires
            self.wf(),
        ensures
            self.regions.wf(),
            self.regions == crate::layout::process_regions(),
            self.regions.separated(),
            self.meta.is_process(),
    {
    }

    /// A built `Shared` ([`Shared::built`]) is well formed once the
    /// allocator is published.
    pub proof fn lemma_built_wf(&self)
        requires
            self.built(),
            crate::api::ready(),
        ensures
            self.wf(),
    {
    }

    /// A well-formed `Shared` was built ([`Shared::built`]) and the
    /// allocator is published.
    pub proof fn lemma_wf_ready(&self)
        requires
            self.wf(),
        ensures
            self.built(),
            crate::api::ready(),
    {
    }

    /// The process statistics lock's rank in the lock order (`lock.rs`):
    /// `init` creates the lock at that rank.
    pub(crate) proof fn lemma_ranks(&self)
        requires
            self.wf(),
        ensures
            self.stats.rank() == crate::lock::rank_stats(),
    {
    }

    /// A second arena is being built (`Arenas::build`).
    pub fn set_multi_arena(&self) {
        proof {
            assert(8u8 & !8u8 == 0) by (bit_vector);
        }
        crate::vspec::flags::fetch_or(&self.free_flags, FREE_ROUTE, Ordering::Relaxed);
    }

    /// The decay period new arenas start with (`dirty_decay_ms`).
    pub fn default_decay_ns(&self) -> u64 {
        crate::purge::decay_ns_from_ms(self.config.dirty_decay_ms)
    }
}

/// Everything the `free` fast path reads for one class, in one 64-byte
/// line: the slot geometry (`Regions::class_base[c]` and `CLASSES[c]`'s
/// slot-start test), the bitmap and owner array of [`SlabTable`] and a
/// copy of its hand-out watermark. `[ClassLine; NCLASSES]` has a
/// power-of-two stride equal to the heap's cache headers', so the class
/// index scaled once addresses both, and every field is one load at an
/// immediate offset from it; before, `free` indexed three arrays of
/// different strides (`class_base`, the 32-byte `CLASSES`, the 384-byte
/// tables, one `lea`/`shl` or `madd` each) and read three lines. The 52
/// lines are consecutive, so they fill distinct L1 sets.
///
/// The copy of the watermark is stored after the table's own, under its
/// commit lock ([`SlabTable::mirror_watermark`]), so it is never ahead of
/// it: a slab below the copy has its bitmap words and owner word, and the
/// relaxed load is enough for the reason [`SlabTable::desc_if_committed`]
/// gives. A copy that lags the table only sends a `free` into a slab
/// just handed out to the slow path, which repeats the check against the
/// table itself.
#[repr(C, align(64))]
pub struct ClassLine {
    class_base: usize,
    bits_base: usize,
    committed: AtomicUsize,
    owners_base: usize,
    slab_bits: u16,
    lg_slab: u8,
    lg_group: u8,
    /// Right before `inv`, so that `free` loads it and `inv`'s low half
    /// with one `ldp` on aarch64.
    slab_mask: u32,
    /// The inverse modulo `2^64` of the odd part of the class size, the
    /// multiplier of the sized check ([`ClassLine::sized_ok`]); its low
    /// half is [`Class::inv_odd`], the multiplier of the `free` path's
    /// 32-bit slot-start test.
    ///
    /// [`Class::inv_odd`]: crate::layout::Class::inv_odd
    inv: u64,
    /// Slots per slab, as a `u64` so that the sized check compares its
    /// 64-bit quotient against the field in memory; the `free` path reads
    /// the low half.
    nslots: u64,
    /// The last eight bytes are the sized check's guard word
    /// ([`ClassLine::guard_word`]), one load: `guard_mask` is
    /// [`SlabTable::guard_mask`] over page groups of `2^lg_guard` bytes,
    /// `tz` the slot-start test's rotate.
    guard_mask: u32,
    tz: u8,
    /// `log2` of a page group in bytes: `lg_slab` plus
    /// `log2(slabs_per_page)`.
    lg_guard: u8,
    /// Zero: initialised, so that the guard word's load reads no padding.
    _pad: [u8; 2],
}

} // verus!

const _: () = assert!(core::mem::size_of::<ClassLine>() == crate::layout::CLASS_STRIDE);
const _: () = assert!(core::mem::offset_of!(Shared, lines) == 0);

verus! {

/// What [`ClassLine::guard_word`] loads: `guard_mask | tz << 32 |
/// lg_guard << 40`, the padding above zero.
pub(super) open spec fn guard_word_of(guard_mask: u32, tz: u8, lg_guard: u8) -> u64 {
    (guard_mask as u64) | ((tz as u64) << 32u64) | ((lg_guard as u64) << 40u64)
}

/// What [`ClassLine::is_guard`] computes.
pub(super) open spec fn is_guard_spec(rel: usize, gw: u64) -> bool {
    (vstd::wrapping::usize_specs::wrapping_shr(rel, (gw >> 40u64) as u32) as u32).wrapping_add(1) & (gw as u32) == 0
}

impl ClassLine {
    /// The line of class `class`, with the permission of its watermark copy
    /// ([`SlabTable::new_mirror`]), for [`SlabTable::mirror_watermark`].
    fn of(regions: &Regions, table: &SlabTable, class: usize) -> (r: (ClassLine, Tracked<UsizePerm>))
        requires
            class < NCLASSES,
        ensures
            r.1@.id() == usize_id(&r.0.committed),
    {
        broadcast use crate::vspec::class::axiom_class_table_wf;

        let cls = &CLASSES.table()[class];
        let (committed, perm) = table.new_mirror();
        proof {
            assert(cls.wf());
            vstd::std_specs::bits::axiom_u64_trailing_zeros(table.spec_slabs_per_page() as u64);
        }
        let line = ClassLine {
            class_base: regions.class_base[class],
            bits_base: table.bits_base(),
            committed,
            owners_base: table.owners_base(),
            inv: inverse_of_odd_part_u64(cls.size()),
            nslots: cls.nslots as u64,
            slab_mask: cls.slab_mask,
            guard_mask: table.guard_mask(),
            slab_bits: cls.slab_bits,
            tz: cls.tz,
            lg_slab: cls.lg_slab,
            lg_group: table.lg_group(),
            lg_guard: cls.lg_slab + table.slabs_per_page().trailing_zeros() as u8,
            _pad: [0; 2],
        };
        (line, perm)
    }

    /// The slot-start test ([`Class::slot_of_offset`]) on `rel`, the
    /// offset from the region base: the slot number, or `None` for an
    /// address that is not a slot start of its slab.
    ///
    /// [`Class::slot_of_offset`]: crate::layout::Class::slot_of_offset
    #[inline(always)]
    fn slot_start(&self, rel: usize) -> (r: Option<u32>)
        ensures
            ({
                let q = crate::vspec::core_int::rotr32(((rel as u32) & self.slab_mask).wrapping_mul(self.inv as u32), self.tz as u32);
                r == if q < self.nslots as u32 { Some(q) } else { None }
            }),
        opens_invariants none
    {
        let slot = ((rel as u32) & self.slab_mask)
            .wrapping_mul(self.inv as u32)
            .rotate_right(self.tz as u32);
        if slot < self.nslots as u32 {
            Some(slot)
        } else {
            None
        }
    }

    /// The sized free's check on this class's line: whether `p`, at `rel`
    /// from the region base, is inside the region and a slot start in a
    /// slab that is not a guard slab. `region_hi` is
    /// [`Shared::region_hi`], the offset bits at and above the region
    /// length.
    ///
    /// The region bound and the slot-start test are one 64-bit test: `x`
    /// keeps the in-slab offset `rel & slab_mask` plus the bits of `rel`
    /// past the region, and `q = (x * inv).rotate_right(tz)` is below
    /// `nslots` exactly when `x` is a multiple of the size and `x / size <
    /// nslots` (the exact-division test of [`Class::slot_of_offset`], on 64
    /// bits). An offset inside the region leaves `x` the in-slab offset, so
    /// that is the slot-start test; an offset past it leaves `x >= 2^32`,
    /// more than `nslots * size` (a slab is at most `MAX_SLAB`), so `q` is
    /// at least `nslots` whether or not `x` is a multiple. The guard test
    /// is [`ClassLine::is_guard`] on the guard word. The whole check
    /// accepts exactly what the reference form does
    /// ([`Regions::slot_in_class`] and [`SlabTable::is_guard_slab`];
    /// `sized_slot_agrees_with_the_reference`). The ensures state the
    /// computation; that it accepts only slot starts outside guard slabs
    /// is not proved here.
    ///
    /// The shape is about loads. Each one waits on the size (the class
    /// indexes the line) or the pointer, and in a program whose frees take
    /// both from a cache miss (a Valkey `SET` reads the old value's pointer
    /// and size from the key's entry) they all wait in the load queue and
    /// the scheduler until the miss returns. On Zen 4 (Genoa) that
    /// capacity, more than the instruction count, sets how far the core
    /// runs ahead to the next miss: `valkey1` is 8% faster with 13 such
    /// loads in `sdallocx` than with 16 (#46). So the bound has no load of
    /// its own, and the guard mask and both shift counts are one load.
    ///
    /// [`Class::slot_of_offset`]: crate::layout::Class::slot_of_offset
    #[inline(always)]
    fn sized_ok(&self, rel: usize, region_hi: usize) -> (r: bool)
        requires
            self._pad[0] == 0 && self._pad[1] == 0,
        ensures
            ({
                let gw = guard_word_of(self.guard_mask, self.tz, self.lg_guard);
                let x = (rel & (self.slab_mask as usize | region_hi)) as u64;
                let q = crate::vspec::core_int::rotr64(x.wrapping_mul(self.inv), (gw >> 32u64) as u32);
                r == (q < self.nslots && !is_guard_spec(rel, gw))
            }),
        opens_invariants none
    {
        let gw = self.guard_word();
        let x = (rel & (self.slab_mask as usize | region_hi)) as u64;
        // `rotate_right` takes its count modulo 64: `tz`, the low byte.
        let q = x.wrapping_mul(self.inv).rotate_right((gw >> 32) as u32);
        q < self.nslots && !Self::is_guard(rel, gw)
    }

    /// `guard_mask | tz << 32 | lg_guard << 40` (the padding above is
    /// zero), from one 8-byte load.
    ///
    /// Trusted: the load reads four fields through one `u64` (the layout
    /// below, little-endian on x86_64 and aarch64), which Verus has no
    /// permission for, and on x86_64 it goes through an empty `asm!`
    /// (the template of `api::shared_base`'s and `class::table::word_index`'s,
    /// there on aarch64) that leaves the value unchanged.
    #[inline(always)]
    #[verifier::external_body]
    fn guard_word(&self) -> (r: u64)
        requires
            self._pad[0] == 0 && self._pad[1] == 0,
        ensures
            r == guard_word_of(self.guard_mask, self.tz, self.lg_guard),
        opens_invariants none
    {
        // SAFETY: the eight initialised bytes from `guard_mask` to the end
        // of `self`, 8-aligned (asserted below).
        #[allow(unused_mut)]
        let mut gw = unsafe {
            ptr::read(
                (self as *const ClassLine as *const u8)
                    .add(core::mem::offset_of!(ClassLine, guard_mask))
                    as *const u64,
            )
        };
        // SAFETY: an empty template; `gw` comes out unchanged. Without it
        // LLVM splits the load back into a byte load per shift count.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("/* {0} */", inout(reg) gw, options(pure, nomem, nostack, preserves_flags))
        };
        gw
    }

    /// [`SlabTable::is_guard_slab`] on the offset `rel` from the region
    /// base, from a guard word: `rel >> lg_guard` is the page group
    /// (`slab >> lg_slabs_per_page`), a guard when `(group + 1) &
    /// guard_mask == 0`. With guards off the mask is all ones and only
    /// group `u32::MAX` would match, which is past every region; an offset
    /// past the region is rejected by the bound in [`ClassLine::sized_ok`]
    /// whatever this says. `guard_test_agrees_with_the_tables` checks it
    /// against the table for every class.
    #[inline(always)]
    fn is_guard(rel: usize, gw: u64) -> (r: bool)
        ensures
            r == is_guard_spec(rel, gw),
        opens_invariants none
    {
        // `wrapping_shr` takes its count modulo 64: `lg_guard`.
        (rel.wrapping_shr((gw >> 40) as u32) as u32).wrapping_add(1) & (gw as u32) == 0
    }
}

} // verus!

impl ClassLine {
    /// Tests: the line's fields as `(class_base, bits_base, watermark copy,
    /// owners_base, lg_group)`.
    #[cfg(test)]
    pub(super) fn parts(&self) -> (usize, usize, usize, usize, u8) {
        (
            self.class_base,
            self.bits_base,
            self.committed.load(Ordering::Relaxed),
            self.owners_base,
            self.lg_group,
        )
    }

    /// Tests: the sized check's 64-bit multiplier.
    #[cfg(test)]
    pub(super) fn inv(&self) -> u64 {
        self.inv
    }

    /// Tests: [`ClassLine::is_guard`] on this line.
    #[cfg(test)]
    pub(super) fn is_guard_at(&self, rel: usize) -> bool {
        Self::is_guard(rel, self.guard_word())
    }

    /// The `free` check on this class's line: `Some(slab)` when `p` is a
    /// slot start in a slab below the watermark whose bit is set, `None`
    /// otherwise. The slot-start test is [`Class::slot_of_offset`]; the
    /// watermark compare is also the region bound (an address past the
    /// region, in the gap after it or below its base yields a slab number
    /// of at least `nslabs`, which no watermark reaches).
    ///
    /// [`Class::slot_of_offset`]: crate::layout::Class::slot_of_offset
    #[inline(always)]
    fn set_slab(&self, p: usize) -> Option<usize> {
        // On aarch64 every field is read up front, before the first
        // branch: read after it, LLVM rebuilt the line's address (one more
        // `add`), and up front the two compares fold into a `ccmp`. On
        // x86_64 that order costs callee-saved registers (`push` and `pop`
        // of `rbx`, and `r14`), so there each is read just before its use.
        #[cfg(target_arch = "aarch64")]
        let (lg_slab, slab_bits, bits_base, committed) = self.bit_fields();
        let rel = p.wrapping_sub(self.class_base);
        let slot = self.slot_start(rel)?;
        #[cfg(not(target_arch = "aarch64"))]
        let lg_slab = self.lg_slab;
        let slab = rel >> lg_slab;
        #[cfg(not(target_arch = "aarch64"))]
        let committed = self.committed.load(Ordering::Relaxed);
        if slab >= committed {
            return None;
        }
        #[cfg(not(target_arch = "aarch64"))]
        let (slab_bits, bits_base) = (self.slab_bits, self.bits_base);
        let bit = slab * slab_bits as usize + slot as usize;
        #[allow(unused_mut)]
        let mut index = bit / 64;
        // As in `SlabTable::is_set_unchecked`: on aarch64 `bit >> 6` and a
        // scaled `ldr`, not LLVM's `(bit >> 3) & !7`.
        #[cfg(target_arch = "aarch64")]
        // SAFETY: an empty template; `index` comes out unchanged.
        unsafe {
            core::arch::asm!("/* {0} */", inout(reg) index, options(pure, nomem, nostack, preserves_flags))
        };
        let word = (bits_base + index * 8) as *const AtomicU64;
        // SAFETY: `slab` is below a value the watermark held and `slot`
        // below `nslots`, so the word is in the committed bitmap.
        if (unsafe { (*word).load(Ordering::Relaxed) } >> (bit % 64)) & 1 == 0 {
            return None;
        }
        Some(slab)
    }

    /// What [`set_slab`](ClassLine::set_slab) reads past the slot-start
    /// test: `lg_slab`, `slab_bits`, `bits_base` and the watermark copy.
    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn bit_fields(&self) -> (u8, u16, usize, usize) {
        (
            self.lg_slab,
            self.slab_bits,
            self.bits_base,
            self.committed.load(Ordering::Relaxed),
        )
    }

    /// The arena that owns `slab`'s group.
    ///
    /// # Safety
    /// `slab` is one [`set_slab`](ClassLine::set_slab) returned.
    #[inline(always)]
    unsafe fn owner(&self, slab: usize) -> u16 {
        let word = (self.owners_base + (slab >> self.lg_group) * 2) as *const AtomicU16;
        // SAFETY: as `SlabTable::owner_at`.
        unsafe { (*word).load(Ordering::Relaxed) }
    }
}

// The route bit `vspec::flags::fetch_or` may set is `FREE_ROUTE`.
const _: () = assert!(crate::vspec::flags::ROUTE == FREE_ROUTE);
const _: () = assert!(
    core::mem::offset_of!(Shared, free_flags) / 128
        == (core::mem::offset_of!(Shared, regions) + core::mem::offset_of!(Regions, superblock))
            / 128
);
const _: () = assert!(
    core::mem::offset_of!(Shared, free_flags) / 128
        == (core::mem::offset_of!(Shared, regions) + core::mem::offset_of!(Regions, lg_stride))
            / 128
);

/// Makes the next [`ThreadKey::set`] on the thread with this kernel id
/// fail as `pthread_setspecific` would, for tests.
#[cfg(test)]
pub static FAIL_SETSPECIFIC: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

verus! {

/// The `pthread_key` whose destructor tears down a thread's heap.
pub struct ThreadKey {
    key: libc::pthread_key_t,
    valid: bool,
}

impl ThreadKey {
    /// A new key whose destructor is `heap_destructor`; `valid` says
    /// whether `pthread_key_create` gave one. Trusted: passes a function
    /// pointer, which Verus does not support.
    #[verifier::external_body]
    pub fn create() -> ThreadKey
        requires
            crate::sys::may_syscall(),
    {
        let mut key: libc::pthread_key_t = 0;
        // SAFETY: `key` is a valid out-pointer; the destructor is a plain
        // `extern "C"` function that only touches allocator state.
        let rc = unsafe { libc::pthread_key_create(&mut key, Some(heap_destructor)) };
        ThreadKey {
            key,
            valid: rc == 0,
        }
    }
}

} // verus!

impl ThreadKey {
    #[cfg(test)]
    pub fn valid(&self) -> bool {
        self.valid
    }

    #[cfg(test)]
    pub fn key(&self) -> Option<libc::pthread_key_t> {
        if self.valid { Some(self.key) } else { None }
    }

    /// Register `heap` for destruction when the calling thread exits.
    /// `false` if `pthread_setspecific` fails, which for a live key
    /// happens only past glibc's first 32 keys, when it cannot allocate
    /// the second-level block; `true` without registering when the key
    /// does not exist.
    pub fn set(&self, heap: *mut Heap) -> bool {
        if !self.valid {
            return true;
        }
        #[cfg(test)]
        if FAIL_SETSPECIFIC
            .compare_exchange(tid(), 0, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return false;
        }
        // SAFETY: the key was created by `create`; glibc stores the
        // pointer without dereferencing it.
        unsafe { libc::pthread_setspecific(self.key, heap as *const c_void) == 0 }
    }
}

pub(super) fn tid() -> u32 {
    crate::sys::gettid() as u32
}

verus! {

pub(super) fn ncpus() -> u32
    requires
        crate::sys::may_syscall(),
{
    crate::sys::ncpus()
}

} // verus!

pub(super) fn now_ns() -> u64 {
    crate::sys::now_coarse_ns()
}

/// Eight random bytes from `getrandom(2)` for `slot_randomize`; on a
/// kernel that refuses (not seen on any supported one) the clock, so
/// the heap is still seeded with something that varies.
pub(super) fn seed() -> u64 {
    let mut b = [0u8; 8];
    match crate::sys::getrandom_exact(&mut b) {
        // `u64::from_ne_bytes` on the little-endian targets (x86_64,
        // aarch64), spelled out.
        Ok(()) => {
            b[0] as u64
                | (b[1] as u64) << 8
                | (b[2] as u64) << 16
                | (b[3] as u64) << 24
                | (b[4] as u64) << 32
                | (b[5] as u64) << 40
                | (b[6] as u64) << 48
                | (b[7] as u64) << 56
        }
        Err(_) => crate::sys::now_coarse_ns(),
    }
}

verus! {

/// A guard slab interval [`SlabTable::new`] accepts: 0, or a power of two
/// from 2 (1 leaves no slabs to allocate).
pub open spec fn table_interval_ok(i: u32) -> bool {
    i == 0 || (i != 1 && exists|k: nat| k < 32 && i as nat == #[trigger] vstd::arithmetic::power2::pow2(k))
}

/// Guard slabs are placed once per `interval` slabs of 64 KiB, so a class
/// with smaller slabs (`layout::LG_MIN_SLAB`) gets one guard per
/// `interval * (64 KiB / slab)` slabs (in page groups when the page is
/// larger than the slab, see `SlabTable::is_guard_slab`): the same one
/// guard page per `interval * 64 KiB` of address space and the same
/// `madvise` count per byte committed for every class. 0 stays 0. The
/// ratio is a power of two (slab sizes and pages are), so a power-of-two
/// `interval` (what `config.rs` guarantees, `Config::wf`) stays one, as
/// the mask test needs, and a 4 KiB to 4 MiB page and an interval of at
/// most `2^22` keep it at most `2^26`, clear of the saturation.
pub(crate) fn guard_interval_for(class: usize, page: usize, interval: u32) -> (r: u32)
    requires
        class < NCLASSES,
    ensures
        (exists|lg: u32| 12 <= lg <= 22 && page == 1usize << lg) && (interval == 0 || (2 <= interval <= 0x40_0000
            && crate::meta::is_pow2(interval as usize))) ==> table_interval_ok(r),
{
    broadcast use crate::vspec::class::axiom_class_table_wf;

    let unit = (1usize << crate::layout::LG_GUARD_UNIT).max(page);
    let group = CLASSES.table()[class].slab_size().max(page);
    proof {
        assert((1usize << crate::layout::LG_GUARD_UNIT) == 0x1_0000usize) by (compute);
    }
    if group >= unit {
        proof {
            lemma_guard_interval_pow2(interval);
        }
        interval
    } else {
        let r = interval.saturating_mul((unit / group) as u32);
        proof {
            lemma_guard_interval_scaled(class, page, interval, unit, group, r);
        }
        r
    }
}

/// A configured guard interval ([`Config::wf`]) is a power of two from 2
/// to `2^22`: `2^k` with `1 <= k <= 22`.
proof fn lemma_guard_interval_pow2(interval: u32)
    ensures
        2 <= interval <= 0x40_0000 && crate::meta::is_pow2(interval as usize) ==> exists|k: nat|
            1 <= k <= 22 && interval as nat == #[trigger] vstd::arithmetic::power2::pow2(k),
        2 <= interval <= 0x40_0000 && crate::meta::is_pow2(interval as usize) ==> table_interval_ok(interval),
{
    if 2 <= interval <= 0x40_0000 && crate::meta::is_pow2(interval as usize) {
        vstd::arithmetic::power2::lemma2_to64();
        let k = choose|k: nat| k < 64 && interval as usize as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
        if k >= 23 {
            vstd::arithmetic::power2::lemma_pow2_strictly_increases(22, k);
        }
        assert(1 <= k <= 22);
    }
}

/// [`guard_interval_for`]'s scaled branch keeps a power of two below
/// `2^32`: `unit / group` is 2 to 16 and the interval at most `2^22`.
proof fn lemma_guard_interval_scaled(class: usize, page: usize, interval: u32, unit: usize, group: usize, r: u32)
    requires
        class < NCLASSES,
        unit >= 0x1_0000,
        unit >= page,
        unit == 0x1_0000 || unit == page,
        group >= (1usize << class_table()[class as int].lg_slab),
        group >= page,
        group == (1usize << class_table()[class as int].lg_slab) || group == page,
        group < unit,
        r as int == vstd::math::min(interval as int * ((unit / group) as u32) as int, u32::MAX as int),
    ensures
        (exists|lg: u32| 12 <= lg <= 22 && page == 1usize << lg) && (interval == 0 || (2 <= interval <= 0x40_0000
            && crate::meta::is_pow2(interval as usize))) ==> table_interval_ok(r),
{
    broadcast use crate::vspec::class::axiom_class_table_wf;

    if (exists|lg: u32| 12 <= lg <= 22 && page == 1usize << lg) && interval != 0 && 2 <= interval <= 0x40_0000
        && crate::meta::is_pow2(interval as usize) {
        let lg = choose|lg: u32| 12 <= lg <= 22 && page == 1usize << lg;
        let ls = class_table()[class as int].lg_slab;
        assert(class_table()[class as int].wf());
        let slab = 1usize << ls;
        lemma_guard_interval_pow2(interval);
        let k = choose|k: nat| 1 <= k <= 22 && interval as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
        assert(unit == 0x1_0000 && (group == 0x1000 || group == 0x2000 || group == 0x4000 || group == 0x8000))
            by (bit_vector)
            requires
                page == 1usize << lg, 12 <= lg <= 22, 12 <= ls <= 22, slab == 1usize << ls,
                unit >= 0x1_0000, unit >= page, unit == 0x1_0000 || unit == page,
                group >= slab, group >= page, group == slab || group == page,
                group < unit;
        let q = unit / group;
        let j: nat = if group == 0x1000 { 4 } else if group == 0x2000 { 3 } else if group == 0x4000 { 2 } else { 1 };
        vstd::arithmetic::power2::lemma2_to64();
        if group == 0x1000 {
            assert(q == 16);
        } else if group == 0x2000 {
            assert(q == 8);
        } else if group == 0x4000 {
            assert(q == 4);
        } else {
            assert(q == 2);
        }
        assert(q as nat == vstd::arithmetic::power2::pow2(j));
        vstd::arithmetic::power2::lemma_pow2_adds(k, j);
        vstd::arithmetic::power2::lemma_pow2_strictly_increases(k + j, 32);
        assert(interval as int * q as int == vstd::arithmetic::power2::pow2(k + j)) by (nonlinear_arith)
            requires
                interval as nat == vstd::arithmetic::power2::pow2(k),
                q as nat == vstd::arithmetic::power2::pow2(j),
                vstd::arithmetic::power2::pow2(k) * vstd::arithmetic::power2::pow2(j) == vstd::arithmetic::power2::pow2(k + j);
        assert(r as nat == vstd::arithmetic::power2::pow2(k + j));
    }
}

} // verus!

verus! {

// Byte offsets of `Shared`'s fields: `offset_of!`, which Verus cannot
// evaluate, so their values are opaque to it; `axiom_shared_layout`
// states what verified code relies on, and the build checks it.
#[verifier::external_body]
const S_LINES: usize = core::mem::offset_of!(Shared, lines);
#[verifier::external_body]
const S_FREE_FLAGS: usize = core::mem::offset_of!(Shared, free_flags);
#[verifier::external_body]
const S_PUSH_FLAGS: usize = core::mem::offset_of!(Shared, push_flags);
#[verifier::external_body]
const S_REGION_HI: usize = core::mem::offset_of!(Shared, region_hi);
#[verifier::external_body]
const S_REGIONS: usize = core::mem::offset_of!(Shared, regions);
#[verifier::external_body]
const S_TABLES: usize = core::mem::offset_of!(Shared, tables);
#[verifier::external_body]
const S_ARENA0: usize = core::mem::offset_of!(Shared, arena0);
#[verifier::external_body]
const S_META: usize = core::mem::offset_of!(Shared, meta);
#[verifier::external_body]
const S_HUGE: usize = core::mem::offset_of!(Shared, huge);
#[verifier::external_body]
const S_GC_INTERVAL_NS: usize = core::mem::offset_of!(Shared, gc_interval_ns);
#[verifier::external_body]
const S_POLICY: usize = core::mem::offset_of!(Shared, policy);
#[verifier::external_body]
const S_CONFIG: usize = core::mem::offset_of!(Shared, config);
#[verifier::external_body]
const S_KEY: usize = core::mem::offset_of!(Shared, key);
#[verifier::external_body]
const S_ARENAS: usize = core::mem::offset_of!(Shared, arenas);
#[verifier::external_body]
const S_TCACHES: usize = core::mem::offset_of!(Shared, tcaches);
#[verifier::external_body]
const S_STATS: usize = core::mem::offset_of!(Shared, stats);
#[verifier::external_body]
const S_CGROUP: usize = core::mem::offset_of!(Shared, cgroup);

/// `Shared`'s layout (`repr(C)`): each field at its offset, aligned, and
/// ending by the next one's offset, the last by the size. Trusted, and
/// checked by the build: the `const` assertion after this block
/// evaluates the same conditions with `arena::fits`.
axiom fn axiom_shared_layout()
    ensures
        field_fits::<Shared, [ClassLine; NCLASSES]>(S_LINES, S_FREE_FLAGS as int),
        field_fits::<Shared, RouteByte>(S_FREE_FLAGS, S_PUSH_FLAGS as int),
        field_fits::<Shared, u8>(S_PUSH_FLAGS, S_REGION_HI as int),
        field_fits::<Shared, usize>(S_REGION_HI, S_REGIONS as int),
        field_fits::<Shared, Regions>(S_REGIONS, S_TABLES as int),
        field_fits::<Shared, [SlabTable; NCLASSES]>(S_TABLES, S_ARENA0 as int),
        NCLASSES * size_of::<ClassLine>() <= isize::MAX,
        NCLASSES * size_of::<SlabTable>() <= isize::MAX,
        0 < size_of::<ClassLine>(),
        0 < size_of::<SlabTable>(),
        field_fits::<Shared, Arena>(S_ARENA0, S_META as int),
        field_fits::<Shared, Meta>(S_META, S_HUGE as int),
        field_fits::<Shared, Huge>(S_HUGE, S_GC_INTERVAL_NS as int),
        field_fits::<Shared, u64>(S_GC_INTERVAL_NS, S_POLICY as int),
        field_fits::<Shared, Policy>(S_POLICY, S_CONFIG as int),
        field_fits::<Shared, Config>(S_CONFIG, S_KEY as int),
        field_fits::<Shared, ThreadKey>(S_KEY, S_ARENAS as int),
        field_fits::<Shared, Arenas>(S_ARENAS, S_TCACHES as int),
        field_fits::<Shared, Tcaches>(S_TCACHES, S_STATS as int),
        // Over several lines, as in `arena::axiom_arena_layout`.
        field_fits::<
            Shared,
            Lock<Snapshot>,
        >(S_STATS, S_CGROUP as int),
        field_fits::<Shared, Deferred>(S_CGROUP, size_of::<Shared>() as int),
        size_of::<Shared>() <= isize::MAX,
;

} // verus!

// `axiom_shared_layout`, checked.
const _: () = assert!(
    fits::<Shared, [ClassLine; NCLASSES]>(S_LINES, S_FREE_FLAGS)
        && fits::<Shared, RouteByte>(S_FREE_FLAGS, S_PUSH_FLAGS)
        && fits::<Shared, u8>(S_PUSH_FLAGS, S_REGION_HI)
        && fits::<Shared, usize>(S_REGION_HI, S_REGIONS)
        && fits::<Shared, Regions>(S_REGIONS, S_TABLES)
        && fits::<Shared, [SlabTable; NCLASSES]>(S_TABLES, S_ARENA0)
        && NCLASSES as u128 * core::mem::size_of::<ClassLine>() as u128 <= isize::MAX as u128
        && NCLASSES as u128 * core::mem::size_of::<SlabTable>() as u128 <= isize::MAX as u128
        && core::mem::size_of::<ClassLine>() > 0
        && core::mem::size_of::<SlabTable>() > 0
        && fits::<Shared, Arena>(S_ARENA0, S_META)
        && fits::<Shared, Meta>(S_META, S_HUGE)
        && fits::<Shared, Huge>(S_HUGE, S_GC_INTERVAL_NS)
        && fits::<Shared, u64>(S_GC_INTERVAL_NS, S_POLICY)
        && fits::<Shared, Policy>(S_POLICY, S_CONFIG)
        && fits::<Shared, Config>(S_CONFIG, S_KEY)
        && fits::<Shared, ThreadKey>(S_KEY, S_ARENAS)
        && fits::<Shared, Arenas>(S_ARENAS, S_TCACHES)
        && fits::<Shared, Tcaches>(S_TCACHES, S_STATS)
        && fits::<Shared, Lock<Snapshot>>(S_STATS, S_CGROUP)
        && fits::<Shared, Deferred>(S_CGROUP, core::mem::size_of::<Shared>())
        && core::mem::size_of::<Shared>() <= isize::MAX as usize
);

// `arena::Arena::axiom_array_layout` at `split`'s two instances, checked.
const _: () = assert!(
    core::mem::size_of::<[ClassLine; NCLASSES]>() == NCLASSES * core::mem::size_of::<ClassLine>()
        && core::mem::align_of::<[ClassLine; NCLASSES]>() == core::mem::align_of::<ClassLine>()
        && core::mem::size_of::<[SlabTable; NCLASSES]>()
            == NCLASSES * core::mem::size_of::<SlabTable>()
        && core::mem::align_of::<[SlabTable; NCLASSES]>() == core::mem::align_of::<SlabTable>()
);

verus! {

/// The type of [`Shared::stats`]: named, so that the permission parameter
/// of [`Shared::put_tail`] does not read as a lock field to
/// `api::tests::lock_inventory`.
type StatsLock = Lock<Snapshot>;

/// The permissions to the fields of a `Shared` under construction but
/// its regions (frozen once written: [`axiom_shared_regions`]) and its
/// class lines and tables, which are maps of their own, element by
/// element ([`Shared::split`]). `arena0`, `meta` and `arenas` are [`Held`]
/// because `Arena::init`, `Shared::put_head` and `Arenas::init` take
/// theirs out (the first and last hand them back).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
tracked struct SharedParts {
    tracked free_flags: PointsTo<RouteByte>,
    tracked push_flags: PointsTo<u8>,
    tracked region_hi: PointsTo<usize>,
    tracked arena0: Held<Arena>,
    tracked meta: Held<Meta>,
    tracked huge: PointsTo<Huge>,
    tracked gc_interval_ns: PointsTo<u64>,
    tracked policy: PointsTo<Policy>,
    tracked config: PointsTo<Config>,
    tracked key: PointsTo<ThreadKey>,
    tracked arenas: Held<Arenas>,
    tracked tcaches: PointsTo<Tcaches>,
    tracked stats: PointsTo<StatsLock>,
    tracked cgroup: PointsTo<Deferred>,
}

/// The `Shared` at `p` is [`storage_ok`], and the arrays inside it are
/// inside its allocation.
spec fn shared_ok(p: *mut Shared) -> bool {
    &&& storage_ok(p)
    &&& in_alloc(field_ptr::<Shared, ClassLine>(p, S_LINES), NCLASSES as int * size_of::<ClassLine>())
    &&& NCLASSES * size_of::<ClassLine>() <= isize::MAX
    &&& in_alloc(field_ptr::<Shared, SlabTable>(p, S_TABLES), NCLASSES as int * size_of::<SlabTable>())
    &&& NCLASSES * size_of::<SlabTable>() <= isize::MAX
    &&& field_ptr::<Shared, SlabTable>(p, S_TABLES)@.addr + NCLASSES * size_of::<SlabTable>() <= usize::MAX
}

impl SharedParts {
    /// Each permission is to its field of the `Shared` at `p`
    /// ([`shared_ok`]); `arena0` and `arenas` when they are held.
    spec fn at(&self, p: *mut Shared) -> bool {
        &&& shared_ok(p)
        &&& self.free_flags.ptr() == field_ptr::<Shared, RouteByte>(p, S_FREE_FLAGS)
        &&& self.push_flags.ptr() == field_ptr::<Shared, u8>(p, S_PUSH_FLAGS)
        &&& self.region_hi.ptr() == field_ptr::<Shared, usize>(p, S_REGION_HI)
        &&& (self.arena0.perm matches Some(a) ==> a.ptr() == field_ptr::<Shared, Arena>(p, S_ARENA0))
        &&& (self.meta.perm matches Some(m) ==> m.ptr() == field_ptr::<Shared, Meta>(p, S_META))
        &&& self.huge.ptr() == field_ptr::<Shared, Huge>(p, S_HUGE)
        &&& self.gc_interval_ns.ptr() == field_ptr::<Shared, u64>(p, S_GC_INTERVAL_NS)
        &&& self.policy.ptr() == field_ptr::<Shared, Policy>(p, S_POLICY)
        &&& self.config.ptr() == field_ptr::<Shared, Config>(p, S_CONFIG)
        &&& self.key.ptr() == field_ptr::<Shared, ThreadKey>(p, S_KEY)
        &&& (self.arenas.perm matches Some(a) ==> a.ptr() == field_ptr::<Shared, Arenas>(p, S_ARENAS))
        &&& self.tcaches.ptr() == field_ptr::<Shared, Tcaches>(p, S_TCACHES)
        &&& self.stats.ptr() == field_ptr::<Shared, Lock<Snapshot>>(p, S_STATS)
        &&& self.cgroup.ptr() == field_ptr::<Shared, Deferred>(p, S_CGROUP)
    }

    /// Every field is written (arena 0 and the metadata allocator too,
    /// whose permissions `init` freezes, arena 0's for the arena registry
    /// and the allocator's for [`axiom_shared_meta`], so not among these).
    spec fn written(&self) -> bool {
        &&& self.free_flags.is_init()
        &&& self.push_flags.is_init()
        &&& self.region_hi.is_init()
        &&& self.huge.is_init()
        &&& self.gc_interval_ns.is_init()
        &&& self.policy.is_init()
        &&& self.config.is_init()
        &&& self.key.is_init()
        &&& self.arenas.perm matches Some(a) && a.is_init()
        &&& self.tcaches.is_init()
        &&& self.stats.is_init()
        &&& self.cgroup.is_init()
    }
}

/// The class line permissions of the `Shared` at `p`, element by element.
spec fn lines_at(m: Map<int, PointsTo<ClassLine>>, p: *mut Shared) -> bool {
    elems_at(m, field_ptr::<Shared, ClassLine>(p, S_LINES), 0, NCLASSES as int)
}

/// The table permissions of the `Shared` at `p`, element by element.
spec fn tables_at(m: Map<int, PointsTo<SlabTable>>, p: *mut Shared) -> bool {
    elems_at(m, field_ptr::<Shared, SlabTable>(p, S_TABLES), 0, NCLASSES as int)
}

/// The written fields of the `Shared` at `p` (`Shared::split`'s parts and
/// class lines, every one initialised, and the tables), joined into the
/// permission to the whole value, the value [`shared_at`] names, frozen
/// (`&'static`): readers share it, and nothing can write through it. Its
/// statistics lock is the one the parts' permission holds. Trusted, as
/// `arena::axiom_arena_assemble` is: vstd has no way to join typed
/// permissions, and the permissions cover every field at its offset. The
/// permissions are used up, so nothing verified writes the fields
/// afterwards; that nothing unverified does is `shared_at`'s own
/// assumption. The tables', the class lines' and arena 0's permissions
/// are frozen already (`vstd::modes::tracked_static_ref`, once `init` has
/// written them, as the regions' is; arena 0's for the arena registry's
/// table, `Arenas::init`), so they are only read. So are the regions' and
/// the metadata allocator's (`regions` and `meta`), frozen once written
/// ([`Shared::put_head`]); [`axiom_shared_regions`] and
/// [`axiom_shared_meta`] state those two fields.
axiom fn axiom_shared_built(
    tracked parts: SharedParts,
    tracked regions: &'static PointsTo<Regions>,
    tracked meta: &'static PointsTo<Meta>,
    tracked lines: &'static Map<int, PointsTo<ClassLine>>,
    tracked tables: &'static Map<int, PointsTo<SlabTable>>,
    tracked arena0: &'static PointsTo<Arena>,
    p: *mut Shared,
) -> (tracked r: &'static PointsTo<Shared>)
    requires
        parts.at(p),
        parts.written(),
        parts.meta.perm is None,
        regions.ptr() == field_ptr::<Shared, Regions>(p, S_REGIONS),
        regions.is_init(),
        meta.ptr() == field_ptr::<Shared, Meta>(p, S_META),
        meta.is_init(),
        lines_at(*lines, p),
        elems_init(*lines, 0, NCLASSES as int),
        tables_at(*tables, p),
        elems_init(*tables, 0, NCLASSES as int),
        arena0.ptr() == field_ptr::<Shared, Arena>(p, S_ARENA0),
        arena0.is_init(),
    ensures
        shared_at(p).stats == parts.stats.value(),
        r.ptr() == p,
        r.is_init(),
        r.value() == shared_at(p),
;

/// The regions of the `Shared` at `p` are what the permission to its
/// `regions` field holds, once that permission is frozen
/// (`vstd::modes::tracked_static_ref`, right after [`Shared::put_head`]
/// writes the field): no verified code can write the field again, so the
/// value is the built one. Trusted, as `arena::axiom_arena_assemble` is:
/// vstd has no way to relate a field's permission to the whole value.
/// `init` needs it before the build is done (the process's regions,
/// [`axiom_process_shared`], are the arena's and the tables').
axiom fn axiom_shared_regions(tracked pr: &'static PointsTo<Regions>, p: *mut Shared)
    requires
        pr.ptr() == field_ptr::<Shared, Regions>(p, S_REGIONS),
        pr.is_init(),
    ensures
        shared_at(p).regions == pr.value(),
;

/// The metadata allocator of the `Shared` at `p` is what the permission
/// to its `meta` field holds, once that permission is frozen
/// (`vstd::modes::tracked_static_ref`, in [`Shared::put_head`], right
/// after it writes the field), as [`axiom_shared_regions`] states of the
/// regions: no verified code can write the field again, so the value is
/// the built one. Trusted, as [`axiom_shared_regions`] is. `init` needs the
/// allocator's identities
/// ([`process_pages_id`](crate::meta::process_pages_id)) before the
/// build is done, for the lazily committed tables it reserves. Called
/// only by `put_head`, on the permission it has just written and frozen.
axiom fn axiom_shared_meta(tracked pm: &'static PointsTo<Meta>, p: *mut Shared)
    requires
        pm.ptr() == field_ptr::<Shared, Meta>(p, S_META),
        pm.is_init(),
    ensures
        shared_at(p).meta == pm.value(),
;

impl Shared {
    /// The permission to `init`'s storage, uninitialised, for `once`, the
    /// process's token of this step. Trusted: `init`'s caller hands over
    /// writable storage for a `Shared` that nothing else uses (its safety
    /// contract). The token is used up, so the permission is handed out
    /// once: `init`'s caller, `api::init` (unverified), runs it once per
    /// process, after `api::init_slow`'s winning `claim_init`, with the
    /// process's tokens (the unit tests, unverified callers outside this
    /// contract, build a fresh leaked box each time): two permissions for
    /// one storage would prove `false` (`PointsTo::is_disjoint`), and could
    /// freeze two values of a field (`axiom_shared_regions`).
    /// The storage is a Rust object, so it does not wrap the address
    /// space. Required to be the process's storage ([`shared_storage`],
    /// as `init` requires): without it a call at a null or misaligned
    /// pointer proves `false` (`PointsTo::is_nonnull`, `is_aligned`).
    #[allow(unused_variables)]
    #[verifier::external_body]
    #[inline(always)]
    fn begin(this: *mut Shared, Tracked(once): Tracked<Once>) -> (r: Tracked<PointsTo<Shared>>)
        requires
            this == shared_storage(),
            once.step() == Step::Shared,
        ensures
            r@.ptr() == this,
            r@.is_uninit(),
            this@.addr + size_of::<Shared>() <= usize::MAX,
    {
        Tracked::assume_new()
    }

    /// The permissions to the fields of the uninitialised `Shared` `perm`
    /// is to: [`take_field`] for each field at the offsets
    /// [`axiom_shared_layout`] gives, and the class lines and the tables
    /// split by [`split_array`]; the regions' permission last.
    proof fn split(tracked perm: PointsTo<Shared>) -> (tracked r: (
        SharedParts,
        Map<int, PointsTo<ClassLine>>,
        Map<int, PointsTo<SlabTable>>,
        PointsTo<Regions>,
    ))
        requires
            perm.is_uninit(),
            perm.ptr()@.addr + size_of::<Shared>() <= usize::MAX,
        ensures
            r.0.at(perm.ptr()),
            r.0.free_flags.is_uninit(),
            r.0.push_flags.is_uninit(),
            r.0.region_hi.is_uninit(),
            r.3.ptr() == field_ptr::<Shared, Regions>(perm.ptr(), S_REGIONS),
            r.3.is_uninit(),
            r.0.arena0.perm matches Some(a) && a.is_uninit(),
            r.0.meta.perm matches Some(m) && m.is_uninit(),
            r.0.huge.is_uninit(),
            r.0.gc_interval_ns.is_uninit(),
            r.0.policy.is_uninit(),
            r.0.config.is_uninit(),
            r.0.key.is_uninit(),
            r.0.arenas.perm matches Some(a) && a.is_uninit(),
            r.0.tcaches.is_uninit(),
            r.0.stats.is_uninit(),
            r.0.cgroup.is_uninit(),
            lines_at(r.1, perm.ptr()),
            elems_uninit(r.1, 0, NCLASSES as int),
            tables_at(r.2, perm.ptr()),
            elems_uninit(r.2, 0, NCLASSES as int),
    {
        broadcast use vstd::raw_ptr::group_raw_ptr_axioms;

        axiom_shared_layout();
        let p = perm.ptr();
        perm.is_nonnull();
        perm.is_aligned();
        let tracked raw = perm.into_raw();
        axiom_raw_valid(&raw, p as *mut u8, size_of::<Shared>() as int);
        let tracked (lines, raw) = take_field::<Shared, [ClassLine; NCLASSES]>(raw, p, S_LINES, S_FREE_FLAGS as int);
        let tracked (free_flags, raw) = take_field::<Shared, RouteByte>(raw, p, S_FREE_FLAGS, S_PUSH_FLAGS as int);
        let tracked (push_flags, raw) = take_field::<Shared, u8>(raw, p, S_PUSH_FLAGS, S_REGION_HI as int);
        let tracked (region_hi, raw) = take_field::<Shared, usize>(raw, p, S_REGION_HI, S_REGIONS as int);
        let tracked (regions, raw) = take_field::<Shared, Regions>(raw, p, S_REGIONS, S_TABLES as int);
        let tracked (tables, raw) = take_field::<Shared, [SlabTable; NCLASSES]>(raw, p, S_TABLES, S_ARENA0 as int);
        let tracked (arena0, raw) = take_field::<Shared, Arena>(raw, p, S_ARENA0, S_META as int);
        let tracked (meta, raw) = take_field::<Shared, Meta>(raw, p, S_META, S_HUGE as int);
        let tracked (huge, raw) = take_field::<Shared, Huge>(raw, p, S_HUGE, S_GC_INTERVAL_NS as int);
        let tracked (gc_interval_ns, raw) = take_field::<Shared, u64>(raw, p, S_GC_INTERVAL_NS, S_POLICY as int);
        let tracked (policy, raw) = take_field::<Shared, Policy>(raw, p, S_POLICY, S_CONFIG as int);
        let tracked (config, raw) = take_field::<Shared, Config>(raw, p, S_CONFIG, S_KEY as int);
        let tracked (key, raw) = take_field::<Shared, ThreadKey>(raw, p, S_KEY, S_ARENAS as int);
        let tracked (arenas, raw) = take_field::<Shared, Arenas>(raw, p, S_ARENAS, S_TCACHES as int);
        let tracked (tcaches, raw) = take_field::<Shared, Tcaches>(raw, p, S_TCACHES, S_STATS as int);
        let tracked (stats, raw) = take_field::<Shared, Lock<Snapshot>>(raw, p, S_STATS, S_CGROUP as int);
        let tracked (cgroup, _raw) = take_field::<Shared, Deferred>(raw, p, S_CGROUP, size_of::<Shared>() as int);
        crate::arena::axiom_array_layout::<ClassLine, NCLASSES>();
        crate::arena::axiom_array_layout::<SlabTable, NCLASSES>();
        assert(0 < NCLASSES * size_of::<ClassLine>()) by (nonlinear_arith)
            requires
                0 < NCLASSES,
                0 < size_of::<ClassLine>(),
        ;
        assert(0 < NCLASSES * size_of::<SlabTable>()) by (nonlinear_arith)
            requires
                0 < NCLASSES,
                0 < size_of::<SlabTable>(),
        ;
        let tracked lines = split_array(lines);
        let tracked tables = split_array(tables);
        crate::arena::lemma_field_cast::<Shared, [ClassLine; NCLASSES], ClassLine>(p, S_LINES);
        crate::arena::lemma_field_cast::<Shared, [SlabTable; NCLASSES], SlabTable>(p, S_TABLES);
        (
            SharedParts {
                free_flags,
                push_flags,
                region_hi,
                arena0: Held { perm: Some(arena0) },
                meta: Held { perm: Some(meta) },
                huge,
                gc_interval_ns,
                policy,
                config,
                key,
                arenas: Held { perm: Some(arenas) },
                tcaches,
                stats,
                cgroup,
            },
            lines,
            tables,
            regions,
        )
    }
}

/// The in-place writes and reads of [`Shared::init`], each through the
/// permission to the field it writes or reads (`SharedParts`), in
/// `init`'s order; `this` is the storage `init` was handed.
impl Shared {
    /// `meta` and `regions`; the written metadata allocator and its field's
    /// permission, taken out of `pm` and frozen: nothing writes the field
    /// again, and the `Shared` built at `this` holds `meta`
    /// ([`axiom_shared_meta`]).
    #[inline(always)]
    unsafe fn put_head<'a>(
        this: *mut Shared,
        meta: Meta,
        regions: Regions,
        Tracked(pm): Tracked<&'a mut Held<Meta>>,
        Tracked(pr): Tracked<&mut PointsTo<Regions>>,
    ) -> (r: (&'a Meta, Tracked<&'static PointsTo<Meta>>))
        requires
            shared_ok(this),
            old(pm).perm matches Some(m) && m.ptr() == field_ptr::<Shared, Meta>(this, S_META),
            old(pr).ptr() == field_ptr::<Shared, Regions>(this, S_REGIONS),
        ensures
            final(pm).perm is None,
            shared_at(this).meta == meta,
            final(pr).ptr() == old(pr).ptr(),
            final(pr).is_init(),
            final(pr).value() == regions,
            *r.0 == meta,
            r.1@.ptr() == field_ptr::<Shared, Meta>(this, S_META),
            r.1@.is_init(),
            r.1@.value() == meta,
    {
        proof {
            axiom_shared_layout();
        }
        let tracked mut m = pm.perm.tracked_take();
        // SAFETY: the fields of `init`'s storage, through their
        // permissions; never moved.
        unsafe {
            let meta_ptr = field_at(this, S_META);
            ptr_mut_write(meta_ptr, Tracked(&mut m), meta);
            ptr_mut_write(field_at(this, S_REGIONS), Tracked(pr), regions);
            let tracked m = vstd::modes::tracked_static_ref(m);
            proof {
                axiom_shared_meta(m, this);
            }
            (ptr_ref(meta_ptr, Tracked(m)), Tracked(m))
        }
    }

    /// `push_flags`, `region_hi` and `free_flags` (the same byte as
    /// `push_flags`: no [`FREE_ROUTE`] yet); the written regions.
    #[inline(always)]
    unsafe fn put_flags<'a>(
        this: *mut Shared,
        flags: u8,
        region_hi: usize,
        Tracked(pf): Tracked<&mut PointsTo<u8>>,
        Tracked(ph): Tracked<&mut PointsTo<usize>>,
        Tracked(pa): Tracked<&mut PointsTo<RouteByte>>,
        Tracked(pr): Tracked<&'a PointsTo<Regions>>,
    ) -> (r: &'a Regions)
        requires
            shared_ok(this),
            old(pf).ptr() == field_ptr::<Shared, u8>(this, S_PUSH_FLAGS),
            old(ph).ptr() == field_ptr::<Shared, usize>(this, S_REGION_HI),
            old(pa).ptr() == field_ptr::<Shared, RouteByte>(this, S_FREE_FLAGS),
            pr.ptr() == field_ptr::<Shared, Regions>(this, S_REGIONS),
            pr.is_init(),
        ensures
            final(pf).ptr() == old(pf).ptr(),
            final(pf).is_init(),
            final(pf).value() == flags,
            final(ph).ptr() == old(ph).ptr(),
            final(ph).is_init(),
            final(ph).value() == region_hi,
            final(pa).ptr() == old(pa).ptr(),
            final(pa).is_init(),
            *r == pr.value(),
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: as in `put_head`; `regions` was written by `put_head`.
        unsafe {
            ptr_mut_write(field_at(this, S_PUSH_FLAGS), Tracked(pf), flags);
            ptr_mut_write(field_at(this, S_REGION_HI), Tracked(ph), region_hi);
            ptr_mut_write(field_at(this, S_FREE_FLAGS), Tracked(pa), crate::vspec::flags::new(flags));
            ptr_ref(field_at(this, S_REGIONS), Tracked(pr))
        }
    }

    /// `tables[c]`, through the permission `pt` to it.
    #[inline(always)]
    unsafe fn put_table(this: *mut Shared, c: usize, t: SlabTable, Tracked(pt): Tracked<&mut PointsTo<SlabTable>>)
        requires
            shared_ok(this),
            c < NCLASSES,
            old(pt).ptr() == ptr_add(field_ptr::<Shared, SlabTable>(this, S_TABLES), c),
        ensures
            final(pt).ptr() == old(pt).ptr(),
            final(pt).is_init(),
            final(pt).value() == t,
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: element `c` of the field, through its permission.
        unsafe {
            let tables: *mut SlabTable = field_at(this, S_TABLES);
            ptr_mut_write(elem_at(tables, c, NCLASSES), Tracked(pt), t)
        }
    }

    /// The storage of the tables: their field, as a pointer to its first
    /// element.
    #[inline(always)]
    unsafe fn tables_ptr(this: *mut Shared) -> (r: *mut SlabTable)
        requires
            shared_ok(this),
        ensures
            r == field_ptr::<Shared, SlabTable>(this, S_TABLES),
            in_alloc(r, NCLASSES as int * size_of::<SlabTable>()),
            NCLASSES * size_of::<SlabTable>() <= isize::MAX,
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: a projection inside `init`'s storage; nothing is read.
        unsafe { field_at(this, S_TABLES) }
    }

    /// `lines[c]`, through the permission `pl` to it.
    #[inline(always)]
    unsafe fn put_line(this: *mut Shared, c: usize, l: ClassLine, Tracked(pl): Tracked<&mut PointsTo<ClassLine>>)
        requires
            shared_ok(this),
            c < NCLASSES,
            old(pl).ptr() == ptr_add(field_ptr::<Shared, ClassLine>(this, S_LINES), c),
        ensures
            final(pl).ptr() == old(pl).ptr(),
            final(pl).is_init(),
            final(pl).value() == l,
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: element `c` of the field, through its permission.
        unsafe {
            let lines: *mut ClassLine = field_at(this, S_LINES);
            ptr_mut_write(elem_at(lines, c, NCLASSES), Tracked(pl), l)
        }
    }

    /// The storage of the class lines: their field, as a pointer to its
    /// first element.
    #[inline(always)]
    unsafe fn lines_ptr(this: *mut Shared) -> (r: *mut ClassLine)
        requires
            shared_ok(this),
        ensures
            r == field_ptr::<Shared, ClassLine>(this, S_LINES),
            in_alloc(r, NCLASSES as int * size_of::<ClassLine>()),
            NCLASSES * size_of::<ClassLine>() <= isize::MAX,
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: a projection inside `init`'s storage; nothing is read.
        unsafe { field_at(this, S_LINES) }
    }

    /// The storage of arena 0.
    #[inline(always)]
    unsafe fn arena0_ptr(this: *mut Shared) -> (r: *mut Arena)
        requires
            shared_ok(this),
        ensures
            r == field_ptr::<Shared, Arena>(this, S_ARENA0),
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: a projection inside `init`'s storage; nothing is read.
        unsafe { field_at(this, S_ARENA0) }
    }

    /// `huge`.
    #[inline(always)]
    unsafe fn put_huge(this: *mut Shared, h: Huge, Tracked(ph): Tracked<&mut PointsTo<Huge>>)
        requires
            shared_ok(this),
            old(ph).ptr() == field_ptr::<Shared, Huge>(this, S_HUGE),
        ensures
            final(ph).ptr() == old(ph).ptr(),
            final(ph).is_init(),
            final(ph).value() == h,
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: as in `put_head`.
        unsafe { ptr_mut_write(field_at(this, S_HUGE), Tracked(ph), h) }
    }

    /// `gc_interval_ns` and the two registries (`Arenas::init` with
    /// `arena0` and its frozen permission `a0`, `Tcaches::init`).
    #[inline(always)]
    // The permissions, one per field written, are ghost.
    #[allow(clippy::too_many_arguments)]
    unsafe fn put_registries(
        this: *mut Shared,
        gc_interval_ns: u64,
        arena0: *mut Arena,
        a0: Tracked<&'static PointsTo<Arena>>,
        narenas: u32,
        Tracked(pg): Tracked<&mut PointsTo<u64>>,
        Tracked(pa): Tracked<&mut Held<Arenas>>,
        Tracked(pt): Tracked<&mut PointsTo<Tcaches>>,
    )
        requires
            shared_ok(this),
            old(pg).ptr() == field_ptr::<Shared, u64>(this, S_GC_INTERVAL_NS),
            old(pa).perm matches Some(a) && a.ptr() == field_ptr::<Shared, Arenas>(this, S_ARENAS) && a.is_uninit(),
            a0@.ptr() == arena0,
            a0@.is_init(),
            a0@.value().spec_index() == 0,
            old(pt).ptr() == field_ptr::<Shared, Tcaches>(this, S_TCACHES),
            old(pt).is_uninit(),
        ensures
            final(pg).ptr() == old(pg).ptr(),
            final(pg).is_init(),
            final(pa).perm matches Some(a) && a.ptr() == field_ptr::<Shared, Arenas>(this, S_ARENAS) && a.is_init() && a.value().wf()
                && a.value().spec_arena0() == a0@.value(),
            final(pt).ptr() == old(pt).ptr(),
            final(pt).is_init(),
    {
        proof {
            axiom_shared_layout();
            // `Arenas::init` builds in place: its storage ends below the
            // top of the address space.
            axiom_ptr_add_addr(this as *mut u8, S_ARENAS);
        }
        // SAFETY: as in `put_head`; `arena0` is inside the same `Shared`.
        unsafe {
            ptr_mut_write(field_at(this, S_GC_INTERVAL_NS), Tracked(pg), gc_interval_ns);
            Arenas::init(field_at(this, S_ARENAS), arena0, narenas, a0, Tracked(pa));
            Tcaches::init(field_at(this, S_TCACHES), Tracked(pt));
        }
    }

    /// `cgroup`; the written record.
    #[inline(always)]
    unsafe fn put_cgroup(this: *mut Shared, cgroup: Deferred, Tracked(pc): Tracked<&mut PointsTo<Deferred>>) -> (r: &Deferred)
        requires
            shared_ok(this),
            old(pc).ptr() == field_ptr::<Shared, Deferred>(this, S_CGROUP),
        ensures
            final(pc).ptr() == old(pc).ptr(),
            final(pc).is_init(),
            *r == cgroup,
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: as in `put_head`; never moved.
        unsafe {
            let cgroup_ptr = field_at(this, S_CGROUP);
            ptr_mut_write(cgroup_ptr, Tracked(&mut *pc), cgroup);
            ptr_ref(cgroup_ptr, Tracked(&*pc))
        }
    }

    /// `policy`; the written policy.
    #[inline(always)]
    unsafe fn put_policy(this: *mut Shared, policy: Policy, Tracked(pp): Tracked<&mut PointsTo<Policy>>) -> (r: &Policy)
        requires
            shared_ok(this),
            old(pp).ptr() == field_ptr::<Shared, Policy>(this, S_POLICY),
        ensures
            final(pp).ptr() == old(pp).ptr(),
            final(pp).is_init(),
            *r == policy,
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: as in `put_head`; never moved.
        unsafe {
            let policy_ptr = field_at(this, S_POLICY);
            ptr_mut_write(policy_ptr, Tracked(&mut *pp), policy);
            ptr_ref(policy_ptr, Tracked(&*pp))
        }
    }

    /// `config`, then the thread-exit key (`ThreadKey::create`) and the
    /// empty statistics.
    #[inline(always)]
    unsafe fn put_tail(
        this: *mut Shared,
        config: Config,
        Tracked(pc): Tracked<&mut PointsTo<Config>>,
        Tracked(pk): Tracked<&mut PointsTo<ThreadKey>>,
        Tracked(ps): Tracked<&mut PointsTo<StatsLock>>,
    )
        requires
            crate::sys::may_syscall(),
            shared_ok(this),
            old(pc).ptr() == field_ptr::<Shared, Config>(this, S_CONFIG),
            old(pk).ptr() == field_ptr::<Shared, ThreadKey>(this, S_KEY),
            old(ps).ptr() == field_ptr::<Shared, Lock<Snapshot>>(this, S_STATS),
        ensures
            final(pc).ptr() == old(pc).ptr(),
            final(pc).is_init(),
            final(pc).value() == config,
            final(pk).ptr() == old(pk).ptr(),
            final(pk).is_init(),
            final(ps).ptr() == old(ps).ptr(),
            final(ps).is_init(),
            final(ps).value().rank() == rank_stats(),
    {
        proof {
            axiom_shared_layout();
            axiom_snapshot_lock_inv(Snapshot::EMPTY);
        }
        // SAFETY: as in `put_head`.
        unsafe {
            ptr_mut_write(field_at(this, S_CONFIG), Tracked(pc), config);
            ptr_mut_write(field_at(this, S_KEY), Tracked(pk), ThreadKey::create());
            ptr_mut_write(field_at(this, S_STATS), Tracked(ps), Lock::new(Snapshot::EMPTY, Ghost(rank_stats())));
        }
    }

    /// Initialise `this` in place for already reserved `regions`: the
    /// metadata allocator over the metadata reservation (`space`, its
    /// uncommitted address space), one slab table per class with
    /// `guard_interval` (0 when the kernel has no guard regions), arena 0,
    /// the huge allocator (`guard_install` is the probed
    /// `MADV_GUARD_INSTALL` support), the registries and the thread-exit
    /// key. Returns `false` if the metadata region cannot hold the tables,
    /// leaving `this` unusable. On `true` the built value satisfies
    /// [`Shared::built`]. Every field is written through the permission to it,
    /// carved from the storage's ([`Shared::begin`], [`Shared::split`]).
    ///
    /// # Safety
    /// `this` must point to writable storage for a `Shared` that nothing
    /// else reads until this returns `true`, and the regions must be
    /// reserved (`PROT_NONE` mappings that this process owns).
    /// The `Shared` must never move afterwards: arena 0's class states
    /// point at the tables and the registry points at arena 0. The
    /// contract takes `this` to be the process's storage
    /// ([`shared_storage`]); the unit tests build others.
    ///
    /// `config` supplies the guard slab interval, the huge-run guards, the
    /// cache GC period, the cache capacities, the decay period and the
    /// number of automatic arenas (`narenas:0`, the default, is resolved
    /// here from the CPU count and stored, so `opt.narenas` reports the
    /// number in use); `guard_install` says whether the kernel
    /// has `MADV_GUARD_INSTALL`, without which no guard is placed at all
    /// (the `PROT_NONE` fallback would split every class region into one
    /// VMA per guard, see `class/`).
    // The last two arguments are ghost.
    #[allow(clippy::too_many_arguments)]
    #[verifier::loop_isolation(false)]
    pub unsafe fn init(
        this: *mut Shared,
        regions: Regions,
        space: Tracked<Unbacked>,
        config: Config,
        guard_install: bool,
        pac: bool,
        Tracked(steps): Tracked<Map<Step, Once>>,
        Tracked(built): Tracked<&mut Built>,
    ) -> (r: bool)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            this == shared_storage(),
            regions.wf(),
            regions.separated(),
            regions.ready(),
            space@.addr() == regions.meta_base,
            space@.len() == regions.meta_len,
            config.wf(),
            once::shared_steps(steps),
        ensures
            r ==> shared_at(this).built(),
            r ==> final(built).at(this),
    {
        // Premise (lock order): no allocator lock exists before this
        // `Shared` is built (`api::init` runs `init` once, before the
        // `Shared` is published), so this thread holds none.
        let tracked mut ord = axiom_entry();
        let mut config = config;
        config.narenas = config.effective_narenas(ncpus());
        let guard_interval = if guard_install {
            config.effective_guard_slab_interval()
        } else {
            0
        };
        let guard_install = guard_install && config.guards;
        let decay_ns = crate::purge::decay_ns_from_ms(config.dirty_decay_ms);
        let ghost steps0 = steps;
        let tracked mut steps = steps;
        let tracked shared_once = once::take(&mut steps, Step::Shared);
        let Tracked(perm) = Shared::begin(this, Tracked(shared_once));
        let tracked (mut parts, mut line_perms, mut table_perms, mut regions_perm) = Shared::split(perm);
        proof {
            assert(shared_ok(this));
            crate::arena::lemma_field_cast::<Shared, [SlabTable; NCLASSES], SlabTable>(this, S_TABLES);
        }
        let ghost regions0 = regions;
        proof {
            assert(crate::config::MAX_GUARD_SLAB_INTERVAL == 0x40_0000) by (compute);
            axiom_process_shared();
            crate::meta::lemma_process_pages_id();
            let lg = regions.lg_page;
            assert((1usize << lg) == (1usize << (lg as usize))) by (bit_vector)
                requires
                    lg < 64,
            ;
            crate::meta::lemma_is_pow2_shl(lg as usize);
        }
        // SAFETY: the caller guarantees `this` is writable and unaliased;
        // every write goes through the permission to its field.
        unsafe {
            let (meta, Tracked(meta_perm)) = Shared::put_head(
                this,
                Meta::owning(regions.meta_base, regions.meta_len, regions.page_size(), space),
                regions,
                Tracked(&mut parts.meta),
                Tracked(&mut regions_perm),
            );
            // Frozen: nothing writes the field again.
            let tracked regions_perm = vstd::modes::tracked_static_ref(regions_perm);
            proof {
                axiom_shared_regions(regions_perm, this);
            }
            proof {
                let lr = regions.lg_region;
                assert(1usize << lr >= 1) by (bit_vector)
                    requires lr < 64;
            }
            let regions = Shared::put_flags(
                this,
                (if config.double_free_scan {
                    PUSH_SCAN
                } else {
                    0
                }) | (if config.zero_on_free { PUSH_ZERO } else { 0 })
                    | (if pac && config.pac_pointers {
                        PUSH_PAC
                    } else {
                        0
                    }),
                !(regions.region_len() - 1),
                Tracked(&mut parts.push_flags),
                Tracked(&mut parts.region_hi),
                Tracked(&mut parts.free_flags),
                Tracked(regions_perm),
            );
            proof {
                // Every step but `Shared`'s and `Statics`' still has its
                // token.
                assert forall|s: Step| s != Step::Shared && s != Step::Statics implies #[trigger] once::holds(steps, s) by {
                    assert(once::holds(steps0, s));
                }
            }
            for c in 0..NCLASSES
                invariant
                    crate::sys::may_abort(),
                    tables_at(table_perms, this),
                    elems_init(table_perms, 0, c as int),
                    elems_uninit(table_perms, c as int, NCLASSES as int),
                    forall|d: int| 0 <= d < c ==> (#[trigger] table_perms[d]).value().spec_class() == d,
                    ord.allows(rank_meta()),
                    ord.allows(rank_table(0)),
                    forall|d: int| c <= d < NCLASSES ==> #[trigger] once::holds(steps, Step::SlotLoc(d)),
                    forall|d: int| c <= d < NCLASSES ==> #[trigger] once::holds(steps, Step::FreshWords(d)),
                    once::holds(steps, Step::Huge),
            {
                let tracked slot_once = once::take(&mut steps, Step::SlotLoc(c as int));
                let tracked words_once = once::take(&mut steps, Step::FreshWords(c as int));
                match SlabTable::new(
                    meta,
                    regions,
                    c,
                    guard_interval_for(c, regions.page_size(), guard_interval),
                    Tracked(slot_once),
                    Tracked(words_once),
                    Tracked(&mut ord),
                ) {
                    Some(t) => {
                        let ghost before = table_perms;
                        proof {
                            assert(table_perms.contains_key(c as int));
                        }
                        let tracked pt = table_perms.tracked_borrow_mut(c as int);
                        Shared::put_table(this, c, t, Tracked(pt));
                        proof {
                            assert forall|d: int| #![trigger table_perms.contains_key(d)] #![trigger table_perms[d]] 0 <= d < NCLASSES implies table_perms.contains_key(d)
                                && table_perms[d].ptr() == ptr_add(field_ptr::<Shared, SlabTable>(this, S_TABLES), d as usize) by {
                                assert(before.contains_key(d));
                            }
                            assert forall|d: int| 0 <= d < NCLASSES implies (d < c + 1 ==> (#[trigger] table_perms[d]).is_init()
                                && table_perms[d].value().spec_class() == d)
                                && (c + 1 <= d ==> table_perms[d].is_uninit()) by {
                                if d != c {
                                    assert(before[d] == table_perms[d]);
                                }
                            }
                        }
                    },
                    None => return false,
                }
            }
            // Frozen: nothing writes the tables again (what changes in one
            // is behind its locks and atomics). The class states of every
            // arena keep these permissions.
            let tracked table_perms = vstd::modes::tracked_static_ref(table_perms);
            let tables = Shared::tables_ptr(this);
            proof {
                assert(tables_at(*table_perms, this));
                assert forall|d: int| 0 <= d < NCLASSES implies #[trigger] table_perms.contains_key(d) by {}
                assert forall|d: int| 0 <= d < NCLASSES implies (#[trigger] table_perms[d]).ptr() == ptr_add(tables, d as usize) by {
                    assert(table_perms.contains_key(d));
                }
                assert forall|d: int| 0 <= d < NCLASSES implies (#[trigger] table_perms[d]).is_init() by {}
                assert forall|d: int| 0 <= d < NCLASSES implies (#[trigger] table_perms[d]).value().spec_class() == d by {}
                assert(in_alloc(tables, NCLASSES as int * size_of::<SlabTable>()));
            }
            // Each class's line, from its table and the regions, and the
            // table storing its watermark into the line's copy from now on.
            let lines = Shared::lines_ptr(this);
            for c in 0..NCLASSES
                invariant
                    crate::sys::may_syscall(),
                    lines_at(line_perms, this),
                    elems_init(line_perms, 0, c as int),
                    crate::class::tables_ok(*table_perms, tables),
                    lines == field_ptr::<Shared, ClassLine>(this, S_LINES),
                    in_alloc(lines, NCLASSES as int * size_of::<ClassLine>()),
                    ord.allows(rank_table(0)),
            {
                let ghost before = line_perms;
                proof {
                    assert(line_perms.contains_key(c as int));
                    assert(table_perms.contains_key(c as int));
                }
                let table = ptr_ref(elem_at(tables, c, NCLASSES), Tracked(table_perms.tracked_borrow(c as int)));
                let (line, perm) = ClassLine::of(regions, table, c);
                let tracked pl = line_perms.tracked_borrow_mut(c as int);
                Shared::put_line(this, c, line, Tracked(pl));
                proof {
                    assert forall|d: int| #![trigger line_perms.contains_key(d)] #![trigger line_perms[d]] 0 <= d < NCLASSES implies line_perms.contains_key(d)
                        && line_perms[d].ptr() == ptr_add(field_ptr::<Shared, ClassLine>(this, S_LINES), d as usize) by {
                        assert(before.contains_key(d));
                    }
                    assert forall|d: int| 0 <= d < c + 1 implies (#[trigger] line_perms[d]).is_init() by {
                        if d != c {
                            assert(before[d] == line_perms[d]);
                        }
                    }
                }
                let line = ptr_ref(elem_at(lines, c, NCLASSES), Tracked(line_perms.tracked_borrow(c as int)));
                // The `Shared` never moves (this function's contract), and
                // `perm` is the copy's, moved into place with it.
                table.mirror_watermark(usize_ptr(&line.committed), perm, Tracked(&mut ord));
            }
            // Frozen: the lines' fields are written once, here, and what
            // changes in one (the watermark copy) is an atomic.
            let tracked line_perms = vstd::modes::tracked_static_ref(line_perms);
            let arena0 = Shared::arena0_ptr(this);
            proof {
                axiom_shared_layout();
                axiom_ptr_add_addr(this as *mut u8, S_ARENA0);
            }
            if !Arena::init(arena0, meta, regions, tables, Tracked(table_perms), 0, decay_ns, Tracked(&mut parts.arena0), Tracked(&mut ord)) {
                return false;
            }
            // Arena 0's permission, frozen: the arena registry's table
            // keeps it for entry 0, and `axiom_shared_built` takes it.
            let tracked a0 = vstd::modes::tracked_static_ref(parts.arena0.perm.tracked_take());
            let tracked huge_once = once::take(&mut steps, Step::Huge);
            match Huge::new(meta, regions, guard_install, Tracked(huge_once), Tracked(&mut ord)) {
                Some(h) => {
                    h.set_retain_max(config.huge_retain_max);
                    // `huge_dirty_decay_ms:0`: a freed run is purged at
                    // once rather than at the next decay pass (design 7).
                    h.set_purge_on_free(config.huge_dirty_decay_ms == Some(0));
                    Shared::put_huge(this, h, Tracked(&mut parts.huge))
                }
                None => return false,
            }
            Shared::put_registries(
                this,
                GC_INTERVAL_NS,
                arena0,
                Tracked(a0),
                config.narenas,
                Tracked(&mut parts.gc_interval_ns),
                Tracked(&mut parts.arenas),
                Tracked(&mut parts.tcaches),
            );
            // Design 11: discovery waits for the first tick `DEFER_NS` into
            // the process's life (or the first read of the figures), so a
            // short-lived process never reads the files; `thp:always` needs
            // the limit now, and `cgroup:false` reads nothing either way.
            let cgroup = Shared::put_cgroup(
                this,
                if config.cgroup && config.thp == crate::config::Thp::Always {
                    Deferred::now(Cgroup::discover(true))
                } else if config.cgroup {
                    Deferred::later(crate::sys::now_coarse_ns())
                } else {
                    Deferred::now(Cgroup::discover(false))
                },
                Tracked(&mut parts.cgroup),
            );
            // Design 11: under an effective limit below 1 GiB a configured
            // `thp:always` is forced to `never` in the stored configuration
            // (`auto` is left alone; the report says so); the policy's
            // `high` baseline is the count at init. A deferred discovery
            // leaves no limit, no rises and the `Unlimited` band for now.
            let cgroup = cgroup.get();
            let limit = match cgroup {
                Some(c) => c.limit().min(c.high()),
                None => crate::sys::cgroup::UNLIMITED,
            };
            let rises = match cgroup {
                Some(c) => c.rises().0,
                None => 0,
            };
            let band = match cgroup {
                Some(c) => crate::policy::cgroup_band(c),
                None => crate::policy::Band::Unlimited,
            };
            let thp_forced = crate::thp::forced_by_limit(config.thp, limit);
            if thp_forced {
                config.thp = crate::config::Thp::Never;
            }
            let policy = Shared::put_policy(
                this,
                crate::policy::Policy::new(rises, thp_forced),
                Tracked(&mut parts.policy),
            );
            // Applied once on the discovery sample, so the band and the cap
            // are published before the first 10 ms window closes (nothing
            // is `Empty` yet: retention 0).
            policy.apply(band, limit, 0, rises);
            Shared::put_tail(
                this,
                config,
                Tracked(&mut parts.config),
                Tracked(&mut parts.key),
                Tracked(&mut parts.stats),
            );
            proof {
                built.perm = Some(axiom_shared_built(parts, regions_perm, meta_perm, line_perms, table_perms, a0, this));
                assert(shared_at(this).regions == regions0);
            }
        }
        true
    }
}

} // verus!

verus! {

impl Shared {
    /// Pages in the huge region: the bound on a huge page
    /// [`Shared::classify_free`] returns.
    pub closed spec fn huge_pages(&self) -> usize {
        self.regions.huge_len >> self.regions.lg_page
    }

    /// [`Shared::huge_pages`] unfolded.
    pub(crate) proof fn lemma_huge_pages(&self)
        ensures
            self.huge_pages() == self.regions.huge_len >> self.regions.lg_page,
    {
    }

    /// The process's regions are this `Shared`'s, well formed and separated.
    pub proof fn lemma_regions_ok(&self)
        requires
            self.wf(),
        ensures
            super::regions_ok(),
    {
    }

    /// The huge allocator's base is a page start: the huge base is
    /// `MAX_SLAB`-aligned and the page is at most `MAX_SLAB`
    /// (`Regions::separated`).
    pub(crate) proof fn lemma_huge_aligned(&self)
        requires
            self.wf(),
        ensures
            self.huge.spec_base_aligned(),
    {
        self.lemma_wf_slow();
        let lg = self.regions.lg_page;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg as usize);
        let page = 1usize << lg;
        vstd::arithmetic::power2::lemma2_to64();
        if lg < 22 {
            vstd::arithmetic::power2::lemma_pow2_strictly_increases(lg as nat, 22);
        }
        assert(crate::layout::MAX_SLAB == 0x40_0000) by (bit_vector);
        crate::meta::lemma_is_pow2_shl(lg as usize);
        vstd::arithmetic::div_mod::lemma_mod_self_0(page as int);
        crate::layout::lemma_pow2_divides_pow2(page, page as int, 22);
        crate::layout::lemma_aligned_down(self.regions.huge_base as int, crate::layout::MAX_SLAB as int, page as int);
    }

    /// The huge allocator is the process's ([`Huge::is_process`]): what
    /// its purges need to place their ranges in the huge region
    /// (whole-allocator theorem "Purge batches are well formed").
    pub(crate) proof fn lemma_huge_process(&self)
        requires
            self.wf(),
        ensures
            self.huge.is_process(),
    {
        self.lemma_wf_slow();
        self.lemma_huge_aligned();
        self.huge.lemma_is_process();
    }

    /// A run [`Huge::alloc_run`] placed for `size` bytes at `align` is a
    /// [`huge_block`](crate::layout::huge_block).
    proof fn lemma_run_is_block(&self, a: usize, size: usize, align: usize)
        requires
            self.wf(),
            self.huge.spec_run_at(a, size, align),
        ensures
            crate::layout::huge_block(a, size, align),
    {
        self.lemma_wf_slow();
        let (len, lg32) = (self.regions.huge_len, self.regions.lg_page);
        let lg = lg32 as usize;
        let np = len >> lg32;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg);
        assert(np == len >> lg) by (bit_vector)
            requires
                np == len >> lg32,
                lg == lg32 as usize,
        ;
        vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(np, lg, len);
    }

    /// The run token [`Huge::alloc_run`] returned for the run at `p`
    /// ([`Huge::run_token`]) is a live block's in the process's terms: `p`
    /// is the address of its first page, and its usable pages lie in the
    /// huge region.
    proof fn lemma_run_token(&self, t: crate::huge::RunToken, p: *mut u8)
        requires
            self.wf(),
            t.wf(),
            self.huge.spec_base() + t.start() * self.huge.spec_page() == p@.addr,
            t.start() + t.npages() <= self.huge.spec_npages(),
        ensures
            super::live::page_addr(t.start()) == p@.addr as int,
            crate::layout::process_regions().is_huge_block(p@.addr, super::live::run_usable(t.npages())),
    {
        self.lemma_wf_slow();
        let (len, lg32) = (self.regions.huge_len, self.regions.lg_page);
        let lg = lg32 as usize;
        let np = len >> lg32;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg);
        assert(np == len >> lg) by (bit_vector)
            requires
                np == len >> lg32,
                lg == lg32 as usize,
        ;
        vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(np, lg, len);
        t.lemma_wf();
        let pg = self.huge.spec_page();
        let (s, n) = (t.start(), t.npages());
        assert(pg > 0);
        assert((s + n) * pg <= np * pg) by (nonlinear_arith)
            requires
                s + n <= np,
                pg > 0,
        ;
        assert(s * pg + (n - crate::huge::GUARD_PAGES) * pg + pg == (s + n) * pg) by (nonlinear_arith)
            requires
                crate::huge::GUARD_PAGES == 1,
        ;
        assert((n - crate::huge::GUARD_PAGES) * pg > 0) by (nonlinear_arith)
            requires
                n > crate::huge::GUARD_PAGES,
                pg > 0,
        ;
        vstd::arithmetic::div_mod::lemma_mod_multiples_basic(s, pg);
    }

    /// The usable bytes of a huge run for `size` ([`Huge::spec_usable`]).
    pub closed spec fn huge_usable(&self, size: usize) -> int {
        self.huge.spec_usable(size)
    }

    /// `z` starts at the address of page `page` and covers a usable size
    /// [`Huge::spec_run_usable`] admits for a run there: whole pages, at
    /// least one, with the guard after them inside the region. What
    /// [`Shared::zero_huge`] clears ([`Huge::usable_size`]'s result).
    pub closed spec fn huge_run_zero(&self, z: Zeroed, page: usize) -> bool {
        &&& page < self.huge_pages()
        &&& z.addr() == self.regions.huge_base + (page << self.regions.lg_page)
        &&& 0 <= z.len() <= usize::MAX
        &&& self.huge.spec_run_usable(page, z.len() as usize)
    }

    /// A huge run for `size` can be described: `size` rounded up to a page
    /// does not overflow.
    pub closed spec fn huge_ok(&self, size: usize) -> bool {
        size + self.huge.spec_page() - 1 <= usize::MAX
    }

    /// [`Shared::huge_usable`] unfolded.
    pub(crate) proof fn lemma_huge_usable(&self, size: usize)
        ensures
            self.huge_usable(size) == self.huge.spec_usable(size),
            self.huge_ok(size) == (size + self.huge.spec_page() - 1 <= usize::MAX),
    {
    }

    /// The huge allocator's [`Huge::spec_placeable`] is the process's
    /// [`huge_placeable`](crate::vspec::zero::huge_placeable).
    pub(crate) proof fn lemma_huge_placeable(&self, size: usize, align: usize)
        requires
            self.wf(),
        ensures
            self.huge.spec_placeable(size, align) == crate::vspec::zero::huge_placeable(size, align),
    {
        self.lemma_wf_slow();
        self.lemma_wf();
        self.lemma_huge_pages();
    }

    /// [`Shared::wf`] unfolded, the facts of `built` the slow paths use
    /// beyond [`Shared::lemma_wf`]'s: the huge allocator over the huge
    /// region, the class tables' classes, the configuration, and the arena
    /// registry with arena 0. Trusted until `built` carries them and
    /// `Shared::init` proves them.
    #[verifier::external_body]
    pub(crate) proof fn lemma_wf_slow(&self)
        requires
            self.wf(),
        ensures
            self.huge.spec_base() == self.regions.huge_base,
            self.huge.spec_page() == (1usize << self.regions.lg_page) as int,
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.tables[c].spec_class() == c,
            self.huge.spec_npages() == self.huge_pages(),
            self.config.wf(),
            self.region_hi == !(((1usize << self.regions.lg_region) - 1) as usize),
            self.huge.spec_mid() == self.meta.live_id(),
            self.arena0.spec_index() == 0,
            self.arenas.wf(),
            self.arenas.is_process(),
            self.arenas.spec_arena0() == self.arena0,
    {
    }

    /// The memory cgroup record, `None` until discovery has run
    /// (`sys::cgroup::Deferred`). For the samplers and the policy.
    #[inline]
    pub fn cgroup(&self) -> Option<&Cgroup> {
        self.cgroup.get()
    }

    /// The memory cgroup record, discovering it first if that has not
    /// happened: what a reader of the figures (`stats.cgroup.*`, the
    /// report) uses. A waiting discovery returns with the record published
    /// and hands back the cell's frozen permission (`Deferred::discover`),
    /// through which the record is read, so it never aborts.
    pub fn cgroup_now(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: &Cgroup)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        let tracked mut seen = CellSeen { frozen: None };
        self.discover_cgroup(true, Tracked(ord), Tracked(&mut seen));
        self.cgroup.published(Tracked(seen))
    }

    /// Discover the cgroup if the deferral has run out at `now` (the
    /// periodic tick, the background thread): `true` when this call did,
    /// and the policy has been evaluated on the first figures.
    pub fn discover_cgroup_if_due(&self, now: u64, Tracked(ord): Tracked<&mut LockOrder>) -> bool
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        let tracked mut seen = CellSeen { frozen: None };
        self.cgroup.due(now) && self.discover_cgroup(false, Tracked(ord), Tracked(&mut seen))
    }

    /// Discover the cgroup in `fork`'s prepare handler, so the child
    /// inherits the record; without waiting for a discovery another
    /// thread (or this one, under a signal handler) has under way, which
    /// the child then starts over (`Deferred::fork_child`).
    pub fn discover_cgroup_for_fork(&self, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        let tracked mut seen = CellSeen { frozen: None };
        self.discover_cgroup(false, Tracked(ord), Tracked(&mut seen));
    }

    /// Run the deferred discovery (`Deferred::discover`): `true` when this
    /// call ran it. The `memory.events` `high` count found is the policy's
    /// baseline, set before the record is published (as at init: a
    /// container that already had `high` events does not start with a
    /// full purge, whichever thread samples first); then the band from
    /// the first sample. When `wait`, `seen` gets the cell's frozen
    /// permission (the record is published).
    fn discover_cgroup(&self, wait: bool, Tracked(ord): Tracked<&mut LockOrder>, Tracked(seen): Tracked<&mut CellSeen>) -> bool
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
            wait ==> self.cgroup.seen_done(*final(seen)),
    {
        let ran = self.cgroup.discover(
            wait,
            |cg: &Cgroup| {
                self.policy.rebase_high_rises(cg.rises().0);
            },
            Tracked(seen),
        );
        if ran {
            crate::policy::evaluate(self, Tracked(ord));
        }
        ran
    }

    /// The per-class slab tables (shared by every arena).
    #[inline(always)]
    pub fn tables(&self) -> (r: &[SlabTable; NCLASSES])
        ensures
            self.wf() ==> forall|c: int| 0 <= c < NCLASSES ==> #[trigger] r[c].spec_class() == c,
            *r == self.spec_tables(),
    {
        proof {
            if self.wf() {
                self.lemma_wf_slow();
            }
        }
        &self.tables
    }

    /// The slab tables [`Shared::tables`] returns.
    pub closed spec fn spec_tables(&self) -> [SlabTable; NCLASSES] {
        self.tables
    }

    /// The fork handlers' locks that live in `Shared` itself, by value
    /// (`api::lock_at`): the explicit-cache table lock, the arena
    /// creation lock, the huge, metadata and process statistics locks.
    pub closed spec fn spec_fork_locks(&self) -> (RawLock, RawLock, RawLock, RawLock, RawLock) {
        (
            self.tcaches.spec_raw_lock(),
            self.arenas.spec_raw_lock(),
            self.huge.spec_raw_lock(),
            self.meta.spec_raw_lock(),
            self.stats.spec_raw(),
        )
    }

    /// The arena creation lock of [`Shared::spec_fork_locks`] is the
    /// registry's (`Arenas::lock_held` takes it for the fork handlers).
    pub(crate) proof fn lemma_arenas_lock(&self)
        ensures
            self.spec_fork_locks().1 == self.arenas.spec_raw_lock(),
    {
    }

    /// The explicit-cache table lock, for the fork handlers.
    #[inline(always)]
    pub fn tcaches_lock(&self) -> (r: &RawLock)
        ensures
            *r == self.spec_fork_locks().0,
            r.rank() == rank_tcaches(),
    {
        self.tcaches.raw_lock()
    }

    /// The huge lock, for the fork handlers.
    #[inline(always)]
    pub fn huge_lock(&self) -> (r: &RawLock)
        ensures
            *r == self.spec_fork_locks().2,
            r.rank() == rank_huge(),
    {
        self.huge.raw_lock()
    }

    /// The metadata lock, for the fork handlers.
    #[inline(always)]
    pub fn meta_lock(&self) -> (r: &RawLock)
        ensures
            *r == self.spec_fork_locks().3,
            r.rank() == rank_meta(),
    {
        self.meta.raw_lock()
    }

    /// The process statistics lock, for the fork handlers. Its rank is
    /// `init`'s, so it is stated of a well-formed `Shared` only
    /// ([`Shared::lemma_ranks`]).
    #[inline(always)]
    pub fn stats_lock(&self) -> (r: &RawLock)
        ensures
            *r == self.spec_fork_locks().4,
            self.wf() ==> r.rank() == rank_stats(),
    {
        self.stats.raw()
    }

    /// Slab table for `class`, without the bounds check the fast paths do
    /// not need (`class` comes from `class_of`).
    #[inline(always)]
    pub fn table(&self, class: usize) -> (r: &SlabTable)
        requires
            class < NCLASSES,
        ensures
            self.wf() ==> r.spec_class() == class,
    {
        proof {
            if self.wf() {
                self.lemma_wf_slow();
            }
        }
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of` only yields indices below `NCLASSES`.
        unsafe { self.tables.get_unchecked(class) }
    }

    /// The cache GC period the periodic tick uses: `gc_interval_ns`,
    /// or 0 (GC every tick) while the cgroup policy is in its immediate
    /// band (design 11).
    #[inline]
    pub fn gc_interval(&self) -> u64 {
        if self.policy.immediate() {
            0
        } else {
            self.gc_interval_ns
        }
    }

    /// Arena `index` if it exists (automatic arenas are built on first
    /// touch; see `Arenas::get`).
    #[inline]
    pub fn arena(&self, index: u16, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<(&Arena, Tracked<&'static Published>)>)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_arenas()),
            self.wf(),
        ensures
            *final(ord) == *old(ord),
            r matches Some((a, w)) ==> a.spec_index() == index && published(*w@, *a),
    {
        proof {
            if self.wf() {
                self.lemma_wf_slow();
            }
        }
        if index == 0 {
            return Some((&self.arena0, self.arena0_published()));
        }
        self.arenas.get(self, index, Tracked(ord))
    }

    /// Arena 0's record in the registry ([`Published`],
    /// `Arenas::published0`), which names `arena0` (`Shared::wf`). No
    /// code: the registry's invariant is opened for the proof alone.
    pub(crate) fn arena0_published(&self) -> (r: Tracked<&'static Published>)
        ensures
            self.wf() ==> published(*r@, self.arena0),
    {
        proof {
            if self.wf() {
                self.lemma_wf_slow();
            }
        }
        self.arenas.published0()
    }

    /// Arena `index`, which must exist: the index came from a slab or run
    /// descriptor, an owner word or a cache header, so it was built before
    /// that memory was handed out. Never builds one: an index no arena was
    /// built for is a corrupt word (design 6.3 step 4), and aborts here
    /// instead of quietly materialising an automatic arena to release the
    /// slot through.
    #[inline]
    pub(super) fn owner(&self, index: u16) -> (r: (&Arena, Tracked<&'static Published>))
        requires
            crate::sys::may_abort_pending(),
        ensures
            self.wf() ==> r.0.spec_index() == index && published(*r.1@, *r.0),
    {
        proof {
            if self.wf() {
                self.lemma_wf_slow();
            }
        }
        if index == 0 {
            return (&self.arena0, self.arena0_published());
        }
        match self.arenas.get_existing_published(index) {
            Some(a) => a,
            None => no_such_arena(index),
        }
    }

    /// Arena 0's decay period in nanoseconds (`purge::NEVER` when purging
    /// is off), which the huge allocator shares unless
    /// `huge_dirty_decay_ms` gives it its own ([`Shared::huge_decay_ns`]).
    #[inline]
    pub fn decay_ns(&self) -> u64 {
        self.arena0.decay_ns()
    }

    /// The huge allocator's decay period in nanoseconds (design 7): its
    /// own when `huge_dirty_decay_ms` is set (fixed at init; `-1` is
    /// `purge::NEVER`), else arena 0's live period ([`Shared::decay_ns`],
    /// which `arena.0.dirty_decay_ms` moves). The cgroup policy scales
    /// whichever it is (`purge::decay`).
    pub fn huge_decay_ns(&self) -> u64 {
        match self.config.huge_dirty_decay_ms {
            Some(ms) => crate::purge::decay_ns_from_ms(ms),
            None => self.decay_ns(),
        }
    }

    /// Claim one slot of `class` in `arena` straight from the slab bitmaps,
    /// bypassing every cache. Null when the region is exhausted. The flag
    /// says the memory is known zero; `blk` gets the slot's token.
    pub fn alloc_direct(&self, arena: &Arena, pubw: Tracked<&'static Published>, class: usize, Tracked(blk): Tracked<&mut Live>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (*mut u8, bool, Tracked<Zeroed>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            published(*pubw@, *arena),
            class < NCLASSES,
            self.wf(),
        ensures
            *final(ord) == *old(ord),
            r.0@.addr != 0 ==> crate::layout::process_regions().is_slot(class as int, r.0@.addr),
            r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, class_table()[class as int].size as int),
            r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
    {
        let mut out = [ptr::null_mut()];
        let cs = arena.class(class);
        cs.slot_tokens_are_the_class_s();
        let tracked mut toks = crate::class::SlotToks::empty();
        let (c, Tracked(mut z)) = cs.claim_tokens(&self.meta, &mut out, Tracked(&mut toks), pubw, Tracked(ord));
        if c.n == 0 {
            (ptr::null_mut(), false, Tracked(Zeroed::empty(0)))
        } else {
            proof {
                assert(crate::class::claim_zero_ok(c, out@, z, class as int));
                if c.zeroed {
                    assert(z.contains_key(0));
                    assert(z[0].covers(out@[0], class_table()[class as int].size as int));
                }
            }
            let tracked t = if c.zeroed { z.tracked_remove(0) } else { Zeroed::empty(0) };
            proof {
                toks.lemma_owns_keyed(cs.spec_table(), out@, c.n as int);
                assert(toks.toks.contains_key(0));
                assert(out@.subrange(0, c.n as int)[0] == out@[0]);
                let tracked s = toks.toks.tracked_remove(0);
                self.lemma_regions_ok();
                blk.set(Live::from_slot(s, class as int, out[0]));
            }
            (out[0], c.zeroed, Tracked(t))
        }
    }

    /// Release one slot straight to its slab bitmap through its owning
    /// arena. Aborts unless `p` is an allocated slot of `class` in `arena`.
    /// `tok` is the slot's token; it goes back to the slot's bitmap word.
    pub fn free_direct(&self, arena: u16, class: usize, p: *mut u8, Tracked(tok): Tracked<GhostSingleton<int>>, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && self.wf() && crate::class::slot_owner(class as int, p@.addr) == arena),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            self.wf(),
            class < NCLASSES,
            crate::layout::process_regions().is_slot(class as int, p@.addr),
            tok.id() == slot_loc(class as int),
            tok@ == p@.addr as int,
        ensures
            *final(ord) == *old(ord),
            // Arena isolation: it returns only if `arena` owns the slot's
            // slab, the arena it released the slot into.
            self.wf() ==> crate::class::slot_owner(class as int, p@.addr) == arena,
    {
        broadcast use crate::class::lemma_aset;
        let tracked bag = tok.subset();
        proof {
            let a = ptr_addrs(seq![p]);
            assert(a[0] == p@.addr as int);
            crate::class::lemma_aset_front(a, 0, 1);
            crate::class::lemma_aset_step(a, 0, 0, 1);
            assert(aset(a, 0, 1) =~= set![p@.addr as int]);
            // A batch of one live slot of `arena`.
            if self.wf() {
                self.lemma_regions_ok();
            }
            assert(seq![p][0] == p);
        }
        let (oa, ow) = self.owner(arena);
        oa.class(class).release_with(&[p], Tracked(bag), ow, Tracked(ord));
    }

    /// Classify `p` for `free`: `Ok((class, arena))` for an allocated slab
    /// slot (the bitmap check happens here), `Err(page)` for a page-aligned
    /// pointer into the huge region (the huge allocator checks it). Aborts
    /// on anything else. `blk` is the caller's token for `p`: with the
    /// token of the live block there, neither `invalid_free` nor
    /// `check_located`'s bit test can fire; its watermark compare is not
    /// yet proved not to fire.
    #[inline(always)]
    pub fn classify_free(&self, p: *mut u8, Tracked(blk): Tracked<&Live>) -> (r: Result<(usize, u16), usize>)
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && blk.block(p)),
            self.wf(),
        ensures
            r matches Ok((c, _)) ==> c < NCLASSES && crate::layout::process_regions().is_slot(c as int, p@.addr),
            r matches Err(page) ==> page <= self.huge_pages(),
            // Exact: a page start of the huge region, `page` its index.
            r matches Err(page) ==> crate::layout::process_regions().is_huge_block(p@.addr, 1)
                && page == ((p@.addr - crate::layout::process_regions().huge_base) as usize) >> crate::layout::process_regions().lg_page,
    {
        proof {
            if self.wf() {
                self.lemma_wf_slow();
            }
        }
        proof {
            self.lemma_wf();
            self.lemma_regions_ok();
        }
        match self.regions.classify(p as usize) {
            Kind::Slab { class, slab, slot } => {
                let class = class as usize;
                proof {
                    if blk.block(p) {
                        // A live block at a slot address is the live slot,
                        // whose token `blk` holds.
                        blk.lemma_block_slot_addr(p, class as int);
                    }
                }
                let arena = self.table(class).check_located(slab as usize, slot as usize, p as usize, Tracked(blk.slot_ref()));
                proof {
                    // `check_located` returned: not a guard slab.
                    self.regions.lemma_is_slot(class as int, slab as int, slot as int, p as usize);
                }
                Ok((class, arena))
            }
            Kind::Huge { page } => Err(page),
            Kind::Invalid => {
                proof {
                    if blk.block(p) {
                        // A slot start or a page start of the huge region,
                        // which `classify` never calls `Invalid`.
                        blk.lemma_block_target(p);
                        if crate::heap::slot_start(p) {
                            let c = choose|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr);
                            assert(self.regions.is_slot_addr(c, p@.addr));
                        }
                    }
                    assert(!blk.block(p));
                }
                invalid_free(p as usize)
            },
        }
    }

    /// Allocate a huge run of `size` bytes aligned to `align` in `arena`:
    /// an automatic arena not built yet (a thread whose first allocation
    /// is huge) is built here, an index that does not exist aborts. Null
    /// on exhaustion; the flag says the memory is known zero.
    pub fn alloc_huge(&self, size: usize, align: usize, arena: u16, Tracked(blk): Tracked<&mut Live>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (*mut u8, bool, Tracked<Zeroed>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_arenas()),
            self.wf(),
            crate::meta::is_pow2(align),
        ensures
            *final(ord) == *old(ord),
            r.0@.addr != 0 ==> crate::layout::huge_block(r.0@.addr, size, align),
            r.0@.addr != 0 ==> size <= self.huge_usable(size),
            r.0@.addr != 0 ==> self.huge_ok(size),
            r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, self.huge_usable(size)),
            r.0@.addr != 0 ==> final(blk).block(r.0) && final(blk).len() == self.huge_usable(size),
            r.0@.addr != 0 ==> crate::vspec::zero::huge_placeable(size, align),
    {
        proof {
            self.lemma_wf_slow();
        }
        proof {
            self.lemma_huge_aligned();
            self.lemma_huge_process();
        }
        let Some((a, _)) = self.arena(arena, Tracked(ord)) else { no_such_arena(arena) };
        let tracked mut tok = vstd::map::Map::<int, crate::huge::RunToken>::tracked_empty();
        let (r, z) = self.huge.alloc_run(&self.meta, size, align, arena, Tracked(&mut tok), Tracked(ord));
        proof {
            self.lemma_huge_placeable(size, align);
            if r.ptr@.addr != 0 {
                self.lemma_run_is_block(r.ptr@.addr, size, align);
                let tracked t = tok.tracked_remove(0);
                self.lemma_run_token(t, r.ptr);
                // Exact stats: the pages `note_huge_alloc` adds are the token's.
                assert(t.npages() == r.pages);
                self.lemma_regions_ok();
                blk.set(Live::from_run(t, r.ptr));
            }
        }
        if !r.ptr.is_null() {
            // The pages the run's descriptor records, which is what
            // `free_huge` will subtract.
            a.note_huge_alloc(r.pages);
        }
        (r.ptr, r.zeroed, z)
    }

    /// `zero_on_free` for a huge run: clear the usable bytes of the live
    /// run starting at `page` (aborts unless it is one) before its release.
    /// Out of line: it takes the huge lock for the size and then writes
    /// the whole run. `blk` is the run's token, whose permission to the
    /// run's usable bytes the clear takes (through the cast of the page's
    /// address, `vspec::user::axiom_cast`). Returns the token for the
    /// cleared bytes.
    #[cold]
    #[inline(never)]
    pub fn zero_huge(&self, page: usize, Tracked(blk): Tracked<&mut Live>) -> (z: Tracked<Zeroed>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            self.wf(),
            page <= self.huge_pages(),
            old(blk).run_block(page as int),
        ensures
            self.huge_run_zero(z@, page),
            final(blk).run_block(page as int),
            z@.addr() == final(blk).addr() as int,
            z@.len() == final(blk).len(),
    {
        proof {
            self.lemma_wf_slow();
        }
        let n = self.huge.usable_size(page, Tracked(blk.run_ref()));
        proof {
            broadcast use vstd::layout::align_of_u8;
            // The run's usable bytes end inside the huge region.
            self.lemma_regions_ok();
            let (len, lg32) = (self.regions.huge_len, self.regions.lg_page);
            let lg = lg32 as usize;
            let (np, one) = (len >> lg32, 1usize << lg32);
            assert(np == len >> lg && one == 1usize << lg) by (bit_vector)
                requires
                    np == len >> lg32,
                    one == 1usize << lg32,
                    lg == lg32 as usize,
                    lg32 < 64,
            ;
            vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
            vstd::bits::lemma_usize_shl_is_mul(1, lg);
            vstd::bits::lemma_usize_shr_is_div(len, lg);
            let pg = one as int;
            vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(len as int, pg);
            vstd::arithmetic::div_mod::lemma_mod_bound(len as int, pg);
            assert(page * pg <= np * pg) by (nonlinear_arith)
                requires
                    page < np,
                    pg > 0,
            ;
            assert(page * vstd::arithmetic::power2::pow2(lg as nat) <= usize::MAX) by {
                assert(pg == vstd::arithmetic::power2::pow2(lg as nat));
                assert(np as int == len as int / pg);
                assert(np * pg == pg * np) by (nonlinear_arith);
            }
            vstd::bits::lemma_usize_shl_is_mul(page, lg);
            assert(page << lg32 == page << lg) by (bit_vector)
                requires
                    lg == lg32 as usize,
            ;
            // The run's usable bytes are the permission's: they start at
            // the page's address and are the token's usable pages.
            self.lemma_wf();
            assert(self.regions.huge_base + (page << lg32) == super::live::page_addr(page as int));
            assert(n == super::live::run_usable(blk.run_opt().unwrap().npages()));
            blk.lemma_mem();
            // The page's address is a block of the huge region, so not null.
            let a = (self.regions.huge_base + (page << lg32)) as usize;
            assert((page + 1) * pg <= np * pg) by (nonlinear_arith)
                requires
                    page < np,
                    pg > 0,
            ;
            vstd::arithmetic::div_mod::lemma_mod_multiples_basic(page as int, pg);
            assert(self.regions.is_huge_block(a, 1));
            self.regions.lemma_block_nonzero(0, a, 1, 0);
        }
        // The permission, through the cast of the page's address.
        let ghost orig = *blk;
        let tracked mut m = blk.take_mem();
        proof {
            crate::vspec::user::axiom_cast(&mut m);
        }
        // SAFETY: `m` makes the run's `n` usable bytes writable.
        let z = unsafe { crate::vspec::user::zero_user(crate::vspec::addr::addr_to_ptr(self.regions.huge_addr(page)), n, Tracked(&mut m)) };
        proof {
            blk.put_mem(m, orig);
        }
        z
    }

    /// Free the huge run starting at `page`. Aborts unless it is a live
    /// run start. Returns the usable bytes released.
    pub fn free_huge(&self, page: usize, Tracked(tok): Tracked<crate::huge::RunToken>, Tracked(ord): Tracked<&mut LockOrder>) -> usize
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_huge()),
            self.wf(),
            page <= self.huge_pages(),
            tok.wf(),
            tok.start() == page,
        ensures
            *final(ord) == *old(ord),
    {
        proof {
            self.lemma_wf_slow();
        }
        let ghost np = tok.npages();
        proof {
            self.lemma_huge_process();
        }
        let run = self.huge.free_run(&self.meta, page, Tracked(tok), Tracked(ord));
        // Exact stats: the pages `note_huge_free` subtracts are the token's.
        proof {
            assert(run.pages == np);
        }
        self.owner(run.arena).0.note_huge_free(run.pages);
        run.usable
    }

    /// The sized free's abort: a slab pointer whose class is not the one
    /// the size hint names (`sdallocx`'s contract is `nallocx(size, flags)
    /// == sallocx(p)`), or the start of an active huge run whose page count
    /// is not the one `size` rounds to (any size whose page count is the
    /// run's passes, which also lets `free_aligned_sized` name the small
    /// size of a run an alignment made huge). Pointers that are not
    /// allocations pass here and are judged by the exact free that follows.
    /// `blk` is the caller's token for `p` ([`Live::claims`]): for a
    /// run, it names the run whose page count `size` must round to
    /// ([`crate::heap::huge_hint_ok`]).
    #[cold]
    #[inline(never)]
    pub fn check_size_hint(&self, p: *mut u8, size: usize, hint: Option<usize>, Tracked(blk): Tracked<&Live>)
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && blk.block(p) && crate::heap::hint_ok(p, hint) && crate::heap::huge_hint_ok(*blk, p, size)),
            crate::sys::may_syscall(),
            self.wf(),
        ensures
            crate::heap::hint_ok(p, hint),
            crate::heap::huge_hint_ok(*blk, p, size),
    {
        proof {
            self.lemma_wf_slow();
        }
        proof {
            self.lemma_wf();
            self.lemma_regions_ok();
        }
        match self.regions.classify(p as usize) {
            Kind::Slab { class, .. } => {
                proof {
                    if blk.block(p) {
                        // The live slot of `class`, which the hint names
                        // (`hint_ok`).
                        blk.lemma_block_slot_addr(p, class as int);
                    }
                }
                if hint != Some(class as usize) {
                    hint_mismatch(p, size, class);
                }
                proof {
                    // A slot address is no huge page start.
                    crate::vspec::zero::lemma_slot_not_huge(class as int, p@.addr);
                }
            },
            Kind::Huge { page } => {
                proof {
                    if blk.block(p) {
                        // The live run's token, at a page start no slot
                        // starts at.
                        blk.lemma_claims(p);
                        if crate::heap::slot_start(p) {
                            let c = choose|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr);
                            crate::vspec::zero::lemma_slot_not_huge(c, p@.addr);
                        }
                    }
                }
                let ghost claimed = blk.claims(p) && !crate::heap::slot_start(p);
                proof {
                    if claimed {
                        blk.lemma_run_block(p, page);
                    }
                }
                // Premise (lock order): called from the sized frees' slow
                // paths, which hold no allocator lock.
                let tracked mut ord = axiom_entry();
                let Some(run) = self.huge.lookup(page, Tracked(blk.run_ref()), Tracked(&mut ord)) else {
                    return;
                };
                proof {
                    // The size a valid sized free passes rounds up to the
                    // run's usable bytes (`huge_hint_ok`).
                    self.lemma_huge_usable(size);
                }
                if run.start == page && run.state == RunState::Active && self.huge.usable_for(size) != Some(run.usable) {
                    huge_hint_mismatch(p, size, run.usable);
                }
                proof {
                    // The run's usable bytes are the token's, and `size`
                    // rounds up to them.
                    self.lemma_huge_usable(size);
                }
            },
            _ => {
                proof {
                    // Every huge page start is classified `Huge`.
                    assert(!crate::layout::process_regions().is_huge_block(p@.addr, 1));
                }
            },
        }
    }

    /// Owning arena of the handed-out slab holding the slot `p` of `class`.
    /// Aborts for anything that is not a slot start in such a slab.
    pub(super) fn arena_of_slot(&self, class: usize, p: *mut u8) -> (r: u16)
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && self.wf() && crate::layout::process_regions().is_slot_addr(class as int, p@.addr)),
            class < NCLASSES,
        ensures
            self.wf() ==> r == crate::class::slot_owner(class as int, p@.addr),
    {
        proof {
            if self.wf() {
                self.lemma_regions_ok();
            }
        }
        let t = self.table(class);
        let (slab, _slot) = t.locate(p);
        proof {
            if self.wf() {
                self.regions.lemma_slab_index(class as int, slab as int, _slot as int, p as usize);
            }
        }
        t.owner(slab).0
    }

    /// Release a cache's sorted batch of `class` slots, starting with
    /// `arena`. A home cache's batch is normally one [`release_owned`]
    /// call with no extra work per pointer; the sized path's home cache
    /// and the one foreign cache per class (`heap::FOREIGN`, design 6.3
    /// step 4) hold slots of any arena, and each run of slabs of another
    /// arena is released through that arena (one owner-word load per
    /// switch, a word `release_owned` reads anyway).
    ///
    /// `bag` holds the batch's slot tokens (distinct addresses), which
    /// belong back with the bitmap words each run releases. Every one of
    /// them goes to a [`release_owned`] call, which takes it out of `bag`:
    /// when this returns, `bag` has lost exactly the batch's tokens.
    ///
    /// Arena isolation: each run goes to the class state of arena `a`
    /// (`Shared::owner(a)`, whose index is `a`), and [`release_owned`]
    /// releases only slots whose slab `a` owns ([`slot_owner`]), stopping
    /// at the first it does not; the next pass moves to the arena that
    /// slot's owner word names.
    ///
    /// The loop ends because each pass releases at least one pointer or
    /// moves to the arena the next pointer's owner word names, which
    /// `release_owned` then owns: owners are fixed once a slab is handed
    /// out ([`slot_owner`]), so the measure (twice the pointers left, plus
    /// one while the next pointer is not `a`'s) drops every pass.
    ///
    /// [`release_owned`]: crate::class::ClassState::release_owned
    /// [`slot_owner`]: crate::class::slot_owner
    pub fn release_routed(&self, arena: u16, class: usize, ptrs: &[*mut u8], Tracked(bag): Tracked<&mut GhostSubset<int>>, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && crate::class::live_slots(class as int, ptrs@)),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            self.wf(),
            class < NCLASSES,
            old(bag).id() == slot_loc(class as int),
            aset(ptr_addrs(ptrs@), 0, ptrs@.len() as int) <= old(bag)@,
            distinct(ptr_addrs(ptrs@), 0, ptrs@.len() as int),
        ensures
            final(bag).id() == old(bag).id(),
            final(bag)@ == old(bag)@ - aset(ptr_addrs(ptrs@), 0, ptrs@.len() as int),
            *final(ord) == *old(ord),
    {
        broadcast use crate::class::lemma_aset;
        let ghost pa = ptr_addrs(ptrs@);
        let mut done = 0;
        let mut a = arena;
        proof {
            assert(aset(pa, 0, 0) =~= Set::empty());
            assert(bag@ =~= old(bag)@ - aset(pa, 0, 0));
        }
        loop
            invariant
                crate::sys::may_abort() || (crate::sys::valid_aborts() && crate::class::live_slots(class as int, ptrs@)),
                crate::sys::may_syscall(),
                self.wf(),
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
                class < NCLASSES,
                done <= ptrs@.len(),
                pa == ptr_addrs(ptrs@),
                bag.id() == slot_loc(class as int),
                bag.id() == old(bag).id(),
                bag@ == old(bag)@ - aset(pa, 0, done as int),
                aset(pa, done as int, ptrs@.len() as int) <= bag@,
                distinct(pa, 0, ptrs@.len() as int),
            decreases
                2 * (ptrs@.len() - done) + if done < ptrs@.len()
                    && crate::class::slot_owner(class as int, ptrs@[done as int]@.addr) != a { 1int } else { 0int },
        {
            let ghost d0 = done as int;
            let ghost b0 = bag@;
            let ghost rest = ptrs@.subrange(d0, ptrs@.len() as int);
            let ghost ra = ptr_addrs(rest);
            proof {
                assert(ptrs@.len() == ptrs.len());
                assert forall|i: int| 0 <= i < rest.len() implies ra[i] == pa[d0 + i] by {
                    assert(rest[i] == ptrs@[d0 + i]);
                }
                crate::class::lemma_aset_shift(pa, ra, d0, 0, rest.len() as int);
                crate::class::lemma_aset_split(pa, 0, d0, ptrs@.len() as int);
                if crate::class::live_slots(class as int, ptrs@) {
                    assert forall|i: int| 0 <= i < rest.len() implies #[trigger] crate::layout::process_regions().is_slot(class as int, rest[i]@.addr) by {
                        assert(rest[i] == ptrs@[d0 + i]);
                    }
                }
            }
            let (oa, ow) = self.owner(a);
            done += oa.class(class).release_owned(&ptrs[done..], Tracked(&mut *bag), ow, Tracked(ord));
            proof {
                let r = done - d0;
                crate::class::lemma_aset_split(pa, d0, done as int, ptrs@.len() as int);
                crate::class::lemma_aset_split(pa, 0, d0, ptrs@.len() as int);
                crate::class::lemma_aset_split(pa, 0, d0, done as int);
                crate::class::lemma_aset_shift(pa, ra, d0, 0, r);
                assert(aset(pa, done as int, ptrs@.len() as int) <= bag@);
                if done < ptrs@.len() {
                    // The first pointer left is not `a`'s.
                    assert(rest[r] == ptrs@[done as int]);
                }
                assert(bag@ =~= old(bag)@ - aset(pa, 0, done as int));
            }
            if done == ptrs.len() {
                return;
            }
            proof {
                if crate::class::live_slots(class as int, ptrs@) {
                    assert(crate::layout::process_regions().is_slot(class as int, ptrs@[done as int]@.addr));
                }
            }
            let next = self.arena_of_slot(class, ptrs[done]);
            if next == a {
                // `release_owned` stopped at a slab whose owner word did not
                // name `a`, and a second read names it: the word changed
                // under us. Unreachable while owner words are fixed once
                // handed out (`slot_owner`), as the proof checks.
                proof {
                    assert(false);
                }
                crate::sys::abort_fmt(format_args!("heap: slab of class {class} changed arena {a} during a flush"));
            }
            a = next;
        }
    }

    /// Change the decay period of every existing arena; `-1` turns purging
    /// off. Takes effect at the next periodic tick, or at once for a
    /// background thread (each arena setter wakes it).
    /// `arena.<i>.dirty_decay_ms` goes through `Arena::set_decay_ms` instead.
    pub fn set_decay_ms(&self, ms: i64)
        requires
            crate::sys::may_syscall(),
    {
        // The arenas `Arenas::iter` walks: every built index below the
        // count, in order.
        let n = self.arenas.count() as usize;
        for i in 0..n
            invariant
                crate::sys::may_syscall(),
        {
            if let Some(a) = self.arenas.get_existing(i as u16) {
                a.set_decay_ms(ms);
            }
        }
    }
}

} // verus!

verus! {

impl Shared {
    /// The child of a `fork`: see `sys::cgroup::Deferred::fork_child`.
    /// Only the child's fork handler holds the capability it requires
    /// (`api::fork_child`).
    pub fn cgroup_fork_child(&self)
        requires
            crate::sys::cgroup::in_fork_child(),
    {
        self.cgroup.fork_child();
    }
}

} // verus!

impl Shared {
    /// The sized free's check (`sdallocx`, `free_sized`; design 6.3): `p`
    /// must be a slot start in a non-guard slab of the class the size hint
    /// names, `hint` (`class_for(size, align)`, `None` for a huge size).
    /// Arithmetic on the pointer and the class's [`ClassLine`]: the hint
    /// names the region, so the pointer is checked against that region
    /// alone (no superblock classification and no class compare; the
    /// reference form is [`Regions::slot_in_class`]), as one
    /// [`ClassLine::sized_ok`]: the region bound folded into the
    /// slot-start test, and the guard test. Neither the slab
    /// descriptor nor the class table line is touched: the bit test that
    /// makes plain `free` exact is left to the flush, and so is the
    /// watermark. A slot start in a slab no arena has been handed, or in
    /// a handed-out slab its arena has not committed (the cursor sits
    /// under the class lock), is caught at the flush, which aborts by name
    /// (`SlabTable::owner` on a slab past the watermark, `release_owned`
    /// on a `Fresh` one); until then the slot can be popped by `malloc`,
    /// and a program that writes to it faults on the reserved `PROT_NONE`
    /// page instead of aborting by name (the trade design 6.3 records).
    /// `None` for anything else; the slow path repeats the checks with
    /// aborts ([`check_size_hint`], then [`classify_free`]). The arena is
    /// not read either (it is on the descriptor line): the home cache
    /// takes the slot and the flush routes it ([`release_routed`]).
    ///
    /// [`check_size_hint`]: Shared::check_size_hint
    /// [`classify_free`]: Shared::classify_free
    /// [`release_routed`]: Shared::release_routed
    #[inline(always)]
    pub(super) fn sized_slot(&self, p: *mut u8, hint: Option<usize>) -> Option<usize> {
        let class = hint?;
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of`/`class_for` only produce `class < NCLASSES`.
        let line = unsafe { self.lines.get_unchecked(class) };
        let rel = (p as usize).wrapping_sub(line.class_base);
        if !line.sized_ok(rel, self.region_hi) {
            return None;
        }
        Some(class)
    }

    /// The `free` fast path's dispatch byte (see the field): `push_flags`,
    /// plus [`FREE_ROUTE`] once a second arena exists.
    #[inline(always)]
    pub fn free_flags(&self) -> u8 {
        crate::vspec::flags::load(&self.free_flags, Ordering::Relaxed)
    }

    /// The flag byte both fast paths load: [`PUSH_SCAN`], [`PUSH_ZERO`]
    /// and [`PUSH_PAC`]; 0 in the default configuration, PAC host or not.
    #[inline(always)]
    pub fn push_flags(&self) -> u8 {
        self.push_flags
    }

    /// Whether cache and heap pointers are signed ([`PUSH_PAC`]).
    #[inline(always)]
    pub fn pac(&self) -> bool {
        self.push_flags & PUSH_PAC != 0
    }

    /// Whether pushes compare against the top four cache entries
    /// (`double_free_scan`).
    #[inline(always)]
    pub fn double_free_scan(&self) -> bool {
        self.push_flags & PUSH_SCAN != 0
    }

    /// Whether a freed block is cleared before it is cached or released
    /// (`zero_on_free`).
    #[inline(always)]
    pub fn zero_on_free(&self) -> bool {
        self.push_flags & PUSH_ZERO != 0
    }

    /// The arena half of [`checked_slot`]: whether the group owner word of
    /// `slab`, a slab holding a slot whose bit [`set_slot`] found set, names
    /// `h`'s home arena. Read only with several arenas ([`FREE_ROUTE`]).
    ///
    /// [`checked_slot`]: Shared::checked_slot
    /// [`set_slot`]: Shared::set_slot
    #[inline(always)]
    pub(super) fn home_owns(&self, class: usize, slab: usize, h: &Heap) -> bool {
        // The home arena is read on the multi-arena branch only. A plain
        // load is hoisted above the flag test (the heap is known valid)
        // and then every single-arena `free` touches the heap's header
        // line, which nothing else on the fast paths reads; a volatile
        // read stays here.
        // SAFETY: `h` is a live heap and `arena` a plain field.
        let home = unsafe { ptr::read_volatile(&raw const h.arena) };
        // SAFETY: the caller's `class` and `slab` came from `set_slot`.
        unsafe { self.lines.get_unchecked(class).owner(slab) == home }
    }

    /// The bit test the two `free` fast-path checks share: `Some((class,
    /// slab))` when `p` is a slot start in a handed-out slab of `class`
    /// whose bit is set (what [`checked_slot`]'s arena test then reads),
    /// `None` for anything else; never aborts. The class index, not a
    /// reference to its [`ClassLine`]: a reference made LLVM materialise
    /// the line's address (one more `add`) ahead of loads that address it
    /// from the base and the scaled index anyway.
    ///
    /// [`checked_slot`]: Shared::checked_slot
    #[inline(always)]
    pub(super) fn set_slot(&self, p: *mut u8) -> Option<(usize, usize)> {
        let p = p as usize;
        let class = p.wrapping_sub(self.regions.superblock) >> self.regions.lg_stride;
        if class >= NCLASSES {
            return None;
        }
        // SAFETY: `class < NCLASSES` was just checked.
        let line = unsafe { self.lines.get_unchecked(class) };
        let slab = line.set_slab(p)?;
        Some((class, slab))
    }

    /// Whether the thread-exit key exists (heaps of exiting threads are
    /// reclaimed). False only if `pthread_key_create` failed at init.
    #[cfg(test)]
    pub fn has_thread_key(&self) -> bool {
        self.key.valid()
    }

    /// The thread-exit key, for diagnostics and tests.
    #[cfg(test)]
    pub fn thread_key(&self) -> Option<libc::pthread_key_t> {
        self.key.key()
    }

    /// The `free` fast path's check: `Some(class)` when `p` is a slot start
    /// in a handed-out slab whose bit is set and whose arena is `home` (the
    /// arena load is skipped while only arena 0 exists), `None` for
    /// anything else. It never aborts; [`classify_free`] on the slow path
    /// repeats the checks and does. Every read is bounds-free: the
    /// class index is checked against `NCLASSES` and every per-class field
    /// comes from its [`ClassLine`], the bitmap word from a
    /// slot already known to be below `nslots` in a slab below the
    /// watermark, whose compare is the region bound too (no separate
    /// `region_len` test). The fast path itself
    /// (`heap::free_fast_slot`) makes the same two tests from
    /// [`set_slot`] and [`home_owns`], the arena test selected by the
    /// `free_flags` byte it loads anyway; this form serves the tests.
    ///
    /// The two loads that depend on the pointer are from dense structures
    /// (design section 4): the class bitmap word (one bit per slot, so a
    /// 550 MiB heap of 1 KiB values is 70 KB of bitmap and stays in L2)
    /// and, with several arenas, the group owner word (one `u16` per
    /// line-sized group of slabs). Neither touches the slab descriptor
    /// (one 64-byte line per slab, a cache miss per free on a large heap),
    /// and both are lines only the owning arena's refills and flushes
    /// write, because slabs are handed to arenas in groups that own whole
    /// lines.
    ///
    /// The geometry comes from [`Regions::slab_slot`] (`class_base[c]`,
    /// `lg_region`, `CLASSES[c]`), not from a copy in `tables[c]`: those
    /// arrays are dense (about a dozen lines for all 52 classes, so L1
    /// resident under any workload) and `class_base[c]` is one indexed
    /// load, whereas the per-class table line is one of 52 and reached
    /// through a multiply. With the geometry on the table line the chain
    /// class -> `madd` -> table load -> slab -> bitmap word would be the
    /// whole of `free`'s critical path; here the table's `bits_base`,
    /// `slab_bits` and `committed_slabs` loads run alongside the slot
    /// arithmetic and are needed only for the word address.
    ///
    /// [`classify_free`]: Shared::classify_free
    /// [`Regions::slab_slot`]: crate::layout::Regions::slab_slot
    /// [`set_slot`]: Shared::set_slot
    /// [`home_owns`]: Shared::home_owns
    #[cfg(test)]
    pub(super) fn checked_slot(&self, p: *mut u8, h: &Heap) -> Option<usize> {
        let (class, slab) = self.set_slot(p)?;
        if self.multi_arena() && !self.home_owns(class, slab, h) {
            return None;
        }
        Some(class)
    }

    /// Whether more than one arena exists (see the field).
    #[cfg(test)]
    pub fn multi_arena(&self) -> bool {
        self.free_flags() & FREE_ROUTE != 0
    }

    /// [`checked_slot`] without the arena test: the bit test alone, so a
    /// test can pin down what [`checked_slot`]'s two halves each decide.
    ///
    /// [`checked_slot`]: Shared::checked_slot
    #[cfg(test)]
    pub(super) fn checked_slot_any(&self, p: *mut u8) -> Option<usize> {
        self.set_slot(p).map(|(class, _)| class)
    }
}

verus! {

/// An arena index no arena was built for. From an owner word or a
/// descriptor ([`Shared::owner`]) a check not yet proved not to fire
/// (`crate::sys::may_abort_pending`): the word names the arena that claimed the
/// slab, but no verified invariant says it was built. The other caller,
/// `alloc_huge` (of its caller's index), holds [`crate::sys::may_abort`].
#[cold]
#[inline(never)]
fn no_such_arena(index: u16) -> !
    requires
        crate::sys::may_abort_pending(),
{
    crate::sys::abort_pending(format_args!("arena {index} does not exist"))
}

/// [`Shared::check_size_hint`]'s abort: the size hint names a class other
/// than slot `p`'s `class`.
#[cold]
#[inline(never)]
fn hint_mismatch(p: *mut u8, size: usize, class: u32) -> !
    requires
        crate::sys::may_abort(),
        (class as usize) < NCLASSES,
{
    crate::sys::abort_fmt(format_args!(
        "sized free of {:#x}: size hint {} does not match the allocation (class {}, {} bytes)",
        p as usize,
        size,
        class,
        CLASSES.table()[class as usize].size()
    ))
}

/// [`Shared::check_size_hint`]'s abort for a huge run: `size` does not
/// round up to the run's `usable` bytes.
#[cold]
#[inline(never)]
fn huge_hint_mismatch(p: *mut u8, size: usize, usable: usize) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!(
        "sized free of {:#x}: size hint {} does not match the allocation (huge run, {} bytes)",
        p as usize,
        size,
        usable
    ))
}

} // verus!
verus! {

/// The inverse modulo `2^64` of the odd part of `size` ([`ClassLine`]'s
/// `inv`): Newton's iteration `x = x * (2 - d * x)` doubles the correct
/// low bits from the 3 that `x = d` has for odd `d`, so five steps give
/// 96.
const fn inverse_of_odd_part_u64(size: usize) -> u64
    requires
        1 <= size <= crate::layout::MEDIUM_MAX,
{
    proof {
        let w = size as u64;
        vstd::std_specs::bits::axiom_u64_trailing_zeros(w);
        let t = vstd::std_specs::bits::u64_trailing_zeros(w) as u64;
        assert(t < 64) by (bit_vector)
            requires 1 <= w, t <= 64, w == 0 <==> t == 64;
    }
    let d = (size as u64) >> size.trailing_zeros();
    let mut x = d;
    let mut i = 0;
    while i < 5
        invariant
            0 <= i <= 5,
        decreases 5 - i,
    {
        x = x.wrapping_mul(2u64.wrapping_sub(d.wrapping_mul(x)));
        i += 1;
    }
    x
}

} // verus!

// The guard word: `guard_mask`, `tz`, `lg_guard` and the zero padding are
// the line's last eight bytes, 8-aligned.
const _: () = assert!(
    core::mem::offset_of!(ClassLine, guard_mask) == 56
        && core::mem::offset_of!(ClassLine, tz) == 60
        && core::mem::offset_of!(ClassLine, lg_guard) == 61
        && core::mem::offset_of!(ClassLine, _pad) == 62
);
