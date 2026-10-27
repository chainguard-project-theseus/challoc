//! [`Shared`], the process-wide allocator state every heap works against,
//! and the checks it performs for the fast and slow paths; the thread-exit
//! key and the thread id, clock and seed a heap reads, and the per-class
//! [`ClassLine`] the `free` fast paths read, with the sized path's
//! check.

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU64, AtomicUsize, Ordering};

use crate::arena::{Arena, Arenas, Held, elem_at, field_at, fits};
#[cfg(verus_keep_ghost)]
use crate::arena::{
    elems_at, elems_init, elems_uninit, field_fits, field_ptr, split_array, storage_ok, take_field,
};
use crate::class::SlabTable;
use crate::config::Config;
use crate::huge::{Huge, RunState};
#[cfg(verus_keep_ghost)]
use crate::layout::class_table;
use crate::layout::{CLASSES, Kind, NCLASSES, Regions};
use crate::lock::Lock;
#[cfg(verus_keep_ghost)]
use crate::lock::{axiom_entry, rank_meta, rank_stats, rank_table};
use crate::meta::Meta;
use crate::policy::Policy;
use crate::stats::Snapshot;
#[cfg(verus_keep_ghost)]
use crate::stats::axiom_snapshot_lock_inv;
use crate::sys::cgroup::{Cgroup, Deferred};
#[cfg(verus_keep_ghost)]
use crate::vspec::atom::usize_id;
use crate::vspec::atom::{UsizePerm, usize_ptr};
#[cfg(verus_keep_ghost)]
use crate::vspec::once;
use crate::vspec::once::{Once, Step};
#[cfg(verus_keep_ghost)]
use crate::vspec::ptr::{axiom_ptr_add_addr, axiom_raw_valid, in_alloc, ptr_add};
use crate::vspec::unbacked::Unbacked;
use vstd::prelude::Ghost;

use super::heap_destructor;
use super::tcaches::Tcaches;
use super::{FREE_ROUTE, Heap, PUSH_PAC, PUSH_SCAN, PUSH_ZERO, invalid_free};
#[cfg(verus_keep_ghost)]
use vstd::layout::size_of;
use vstd::map::Map;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, verus};
use vstd::raw_ptr::{PointsTo, ptr_mut_write, ptr_ref};

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
    pub(crate) free_flags: AtomicU8,
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

/// The permission to the whole `Shared` [`Shared::init`] built, frozen, once
/// it has: what `init` hands back to its caller, which publishes it.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct Built {
    pub tracked perm: Option<&'static PointsTo<Shared>>,
}

/// The `Shared` built in place at `p`: [`Shared::init`] writes each field
/// once, before anything reads it, and afterwards nothing writes a field
/// but through its atomics, locks and cells, so the value is one value.
/// Uninterpreted; [`axiom_shared_regions`], [`axiom_shared_meta`] and
/// [`axiom_shared_stats`] fix the fields `built` names from the
/// permissions `init` wrote them through.
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
        self.free_flags.fetch_or(FREE_ROUTE, Ordering::Relaxed);
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

    /// The slot-start test ([`Class::slot_of_offset`]) on `rel`, the
    /// offset from the region base: the slot number, or `None` for an
    /// address that is not a slot start of its slab.
    ///
    /// [`Class::slot_of_offset`]: crate::layout::Class::slot_of_offset
    #[inline(always)]
    fn slot_start(&self, rel: usize) -> Option<u32> {
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
    /// `sized_slot_agrees_with_the_reference`).
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
    fn sized_ok(&self, rel: usize, region_hi: usize) -> bool {
        let gw = self.guard_word();
        let x = (rel & (self.slab_mask as usize | region_hi)) as u64;
        // `rotate_right` takes its count modulo 64: `tz`, the low byte.
        let q = x.wrapping_mul(self.inv).rotate_right((gw >> 32) as u32);
        q < self.nslots && !Self::is_guard(rel, gw)
    }

    /// `guard_mask | tz << 32 | lg_guard << 40` (the padding above is
    /// zero), from one 8-byte load.
    #[inline(always)]
    fn guard_word(&self) -> u64 {
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
    fn is_guard(rel: usize, gw: u64) -> bool {
        // `wrapping_shr` takes its count modulo 64: `lg_guard`.
        (rel.wrapping_shr((gw >> 40) as u32) as u32).wrapping_add(1) & (gw as u32) == 0
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
        field_fits::<Shared, AtomicU8>(S_FREE_FLAGS, S_PUSH_FLAGS as int),
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
        && fits::<Shared, AtomicU8>(S_FREE_FLAGS, S_PUSH_FLAGS)
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
    tracked free_flags: PointsTo<AtomicU8>,
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
        &&& self.free_flags.ptr() == field_ptr::<Shared, AtomicU8>(p, S_FREE_FLAGS)
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

}

/// The class line permissions of the `Shared` at `p`, element by element.
spec fn lines_at(m: Map<int, PointsTo<ClassLine>>, p: *mut Shared) -> bool {
    elems_at(m, field_ptr::<Shared, ClassLine>(p, S_LINES), 0, NCLASSES as int)
}

/// The table permissions of the `Shared` at `p`, element by element.
spec fn tables_at(m: Map<int, PointsTo<SlabTable>>, p: *mut Shared) -> bool {
    elems_at(m, field_ptr::<Shared, SlabTable>(p, S_TABLES), 0, NCLASSES as int)
}

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

/// The statistics lock of the `Shared` at `p` is what the permission to
/// its `stats` field holds, once that permission is frozen
/// (`vstd::modes::tracked_static_ref`, in `init`, after
/// [`Shared::put_tail`] writes the field), as [`axiom_shared_regions`]
/// states of the regions: no verified code can write the field again (the
/// snapshot changes behind the lock), so the value is the built one.
/// Trusted, as [`axiom_shared_regions`] is.
axiom fn axiom_shared_stats(tracked ps: &'static PointsTo<StatsLock>, p: *mut Shared)
    requires
        ps.ptr() == field_ptr::<Shared, Lock<Snapshot>>(p, S_STATS),
        ps.is_init(),
    ensures
        shared_at(p).stats == ps.value(),
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
        let tracked (free_flags, raw) = take_field::<Shared, AtomicU8>(raw, p, S_FREE_FLAGS, S_PUSH_FLAGS as int);
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
    /// `meta` and `regions`; the written metadata allocator, its field's
    /// permission taken out of `pm` and frozen: nothing writes the field
    /// again, and the `Shared` built at `this` holds `meta`
    /// ([`axiom_shared_meta`]).
    #[inline(always)]
    unsafe fn put_head<'a>(
        this: *mut Shared,
        meta: Meta,
        regions: Regions,
        Tracked(pm): Tracked<&'a mut Held<Meta>>,
        Tracked(pr): Tracked<&mut PointsTo<Regions>>,
    ) -> (r: &'a Meta)
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
            *r == meta,
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
            ptr_ref(meta_ptr, Tracked(m))
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
        Tracked(pa): Tracked<&mut PointsTo<AtomicU8>>,
        Tracked(pr): Tracked<&'a PointsTo<Regions>>,
    ) -> (r: &'a Regions)
        requires
            shared_ok(this),
            old(pf).ptr() == field_ptr::<Shared, u8>(this, S_PUSH_FLAGS),
            old(ph).ptr() == field_ptr::<Shared, usize>(this, S_REGION_HI),
            old(pa).ptr() == field_ptr::<Shared, AtomicU8>(this, S_FREE_FLAGS),
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
            ptr_mut_write(field_at(this, S_FREE_FLAGS), Tracked(pa), AtomicU8::new(flags));
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
        Tracked(_built): Tracked<&mut Built>,
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
            let meta = Shared::put_head(
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
                // Every step but `Shared`'s still has its token.
                assert forall|s: Step| s != Step::Shared implies #[trigger] once::holds(steps, s) by {
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
            let arena0 = Shared::arena0_ptr(this);
            proof {
                axiom_shared_layout();
                axiom_ptr_add_addr(this as *mut u8, S_ARENA0);
            }
            if !Arena::init(arena0, meta, regions, tables, Tracked(table_perms), 0, decay_ns, Tracked(&mut parts.arena0), Tracked(&mut ord)) {
                return false;
            }
            // Arena 0's permission, frozen: the arena registry's table
            // keeps it for entry 0.
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
            // Frozen: nothing writes the field again (the snapshot changes
            // behind the lock).
            let tracked stats_perm = vstd::modes::tracked_static_ref(parts.stats);
            proof {
                axiom_shared_stats(stats_perm, this);
                assert(shared_at(this).regions == regions0);
            }
        }
        true
    }
}

} // verus!

impl Shared {
    /// The memory cgroup record, `None` until discovery has run
    /// (`sys::cgroup::Deferred`). For the samplers and the policy.
    #[inline]
    pub fn cgroup(&self) -> Option<&crate::sys::cgroup::Cgroup> {
        self.cgroup.get()
    }

    /// The memory cgroup record, discovering it first if that has not
    /// happened: what a reader of the figures (`stats.cgroup.*`, the
    /// report) uses.
    pub fn cgroup_now(&self) -> &crate::sys::cgroup::Cgroup {
        self.discover_cgroup(true);
        self.cgroup
            .get()
            .unwrap_or_else(|| crate::sys::abort("cgroup: discovery did not publish"))
    }

    /// Discover the cgroup if the deferral has run out at `now` (the
    /// periodic tick, the background thread): `true` when this call did,
    /// and the policy has been evaluated on the first figures.
    pub fn discover_cgroup_if_due(&self, now: u64) -> bool {
        self.cgroup.due(now) && self.discover_cgroup(false)
    }

    /// Discover the cgroup in `fork`'s prepare handler, so the child
    /// inherits the record; without waiting for a discovery another
    /// thread (or this one, under a signal handler) has under way, which
    /// the child then starts over (`Deferred::fork_child`).
    pub fn discover_cgroup_for_fork(&self) {
        self.discover_cgroup(false);
    }

    /// Run the deferred discovery (`Deferred::discover`): `true` when this
    /// call ran it. The `memory.events` `high` count found is the policy's
    /// baseline, set before the record is published (as at init: a
    /// container that already had `high` events does not start with a
    /// full purge, whichever thread samples first); then the band from
    /// the first sample.
    fn discover_cgroup(&self, wait: bool) -> bool {
        let ran = self.cgroup.discover(wait, |cg| {
            self.policy.rebase_high_rises(cg.rises().0);
        });
        if ran {
            crate::policy::evaluate(self, Tracked::assume_new());
        }
        ran
    }

    /// The child of a `fork`: see `sys::cgroup::Deferred::fork_child`.
    pub fn cgroup_fork_child(&self) {
        self.cgroup.fork_child();
    }

    /// The per-class slab tables (shared by every arena).
    #[inline(always)]
    pub fn tables(&self) -> &[SlabTable; NCLASSES] {
        &self.tables
    }

    /// Slab table for `class`, without the bounds check the fast paths do
    /// not need (`class` comes from `class_of`).
    #[inline(always)]
    pub fn table(&self, class: usize) -> &SlabTable {
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of` only yields indices below `NCLASSES`.
        unsafe { self.tables.get_unchecked(class) }
    }

    /// Arena `index` if it exists (automatic arenas are built on first
    /// touch; see `Arenas::get`).
    #[inline]
    pub fn arena(&self, index: u16) -> Option<&Arena> {
        if index == 0 {
            return Some(&self.arena0);
        }
        match self.arenas.get(self, index, Tracked::assume_new()) {
            Some((a, _)) => Some(a),
            None => None,
        }
    }

    /// Arena `index`, which must exist: the index came from a slab or run
    /// descriptor, an owner word or a cache header, so it was built before
    /// that memory was handed out. Never builds one: an index no arena was
    /// built for is a corrupt word (design 6.3 step 4), and aborts here
    /// instead of quietly materialising an automatic arena to release the
    /// slot through.
    #[inline]
    pub(super) fn owner(&self, index: u16) -> &Arena {
        if index == 0 {
            return &self.arena0;
        }
        match self.arenas.get_existing(index) {
            Some(a) => a,
            None => no_such_arena(index),
        }
    }

    /// Whether more than one arena exists (see the field).
    #[cfg(test)]
    pub fn multi_arena(&self) -> bool {
        self.free_flags() & FREE_ROUTE != 0
    }

    /// The `free` fast path's dispatch byte (see the field): `push_flags`,
    /// plus [`FREE_ROUTE`] once a second arena exists.
    #[inline(always)]
    pub fn free_flags(&self) -> u8 {
        self.free_flags.load(Ordering::Relaxed)
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

    /// Change the decay period of every existing arena; `-1` turns purging
    /// off. Takes effect at the next periodic tick, or at once for a
    /// background thread (each arena setter wakes it).
    /// `arena.<i>.dirty_decay_ms` goes through `Arena::set_decay_ms` instead.
    pub fn set_decay_ms(&self, ms: i64) {
        // `for a in self.arenas.iter()`, spelled out: every built arena,
        // in index order.
        let n = self.arenas.count() as usize;
        for i in 0..n {
            if let Some(a) = self.arenas.get_existing(i as u16) {
                a.set_decay_ms(ms);
            }
        }
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

    /// Claim one slot of `class` in `arena` straight from the slab bitmaps,
    /// bypassing every cache. Null when the region is exhausted. The flag
    /// says the memory is known zero.
    pub fn alloc_direct(&self, arena: &Arena, class: usize) -> (*mut u8, bool) {
        let mut out = [ptr::null_mut()];
        let c = arena.class(class).claim(&self.meta, &mut out);
        if c.n == 0 {
            (ptr::null_mut(), false)
        } else {
            (out[0], c.zeroed)
        }
    }

    /// Release one slot straight to its slab bitmap through its owning
    /// arena. Aborts unless `p` is an allocated slot of `class` in `arena`.
    pub fn free_direct(&self, arena: u16, class: usize, p: *mut u8) {
        self.owner(arena).class(class).release_with(
            &[p],
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
    }

    /// Classify `p` for `free`: `Ok((class, arena))` for an allocated slab
    /// slot (the bitmap check happens here), `Err(page)` for a page-aligned
    /// pointer into the huge region (the huge allocator checks it). Aborts
    /// on anything else.
    #[inline(always)]
    pub fn classify_free(&self, p: *mut u8) -> Result<(usize, u16), usize> {
        match self.regions.classify(p as usize) {
            Kind::Slab { class, slab, slot } => {
                let class = class as usize;
                let arena = self.table(class).check_located(
                    slab as usize,
                    slot as usize,
                    p as usize,
                    Tracked::assume_new(),
                );
                Ok((class, arena))
            }
            Kind::Huge { page } => Err(page),
            Kind::Invalid => invalid_free(p as usize),
        }
    }

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

    /// The sized free's abort: a slab pointer whose class is not the one
    /// the size hint names (`sdallocx`'s contract is `nallocx(size, flags)
    /// == sallocx(p)`), or the start of an active huge run whose page count
    /// is not the one `size` rounds to (any size whose page count is the
    /// run's passes, which also lets `free_aligned_sized` name the small
    /// size of a run an alignment made huge). Pointers that are not
    /// allocations pass here and are judged by the exact free that follows.
    #[cold]
    #[inline(never)]
    pub fn check_size_hint(&self, p: *mut u8, size: usize, hint: Option<usize>) {
        match self.regions.classify(p as usize) {
            Kind::Slab { class, .. } => {
                if hint != Some(class as usize) {
                    hint_mismatch(p, size, class);
                }
            }
            Kind::Huge { page } => {
                let Some(run) =
                    self.huge
                        .lookup(page, Tracked::assume_new(), Tracked::assume_new())
                else {
                    return;
                };
                if run.start == page
                    && run.state == RunState::Active
                    && self.huge.usable_for(size) != Some(run.usable)
                {
                    huge_hint_mismatch(p, size, run.usable);
                }
            }
            _ => {}
        }
    }

    /// Owning arena of the handed-out slab holding the slot `p` of `class`.
    /// Aborts for anything that is not a slot start in such a slab.
    pub(super) fn arena_of_slot(&self, class: usize, p: *mut u8) -> u16 {
        let t = self.table(class);
        let (slab, _) = t.locate(p);
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
    /// [`release_owned`]: crate::class::ClassState::release_owned
    pub fn release_routed(&self, arena: u16, class: usize, ptrs: &[*mut u8]) {
        let mut done = 0;
        let mut a = arena;
        loop {
            done += self.owner(a).class(class).release_owned(
                &ptrs[done..],
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            );
            if done == ptrs.len() {
                return;
            }
            let next = self.arena_of_slot(class, ptrs[done]);
            if next == a {
                // `release_owned` stopped at a slab it says is not ours while
                // the descriptor says it is: the descriptor changed under us.
                panic!("heap: slab of class {class} changed arena {a} during a flush");
            }
            a = next;
        }
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
    /// (`heap::free_fast_class`) makes the same two tests from
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

    /// [`checked_slot`] without the arena test: the bit test alone, so a
    /// test can pin down what [`checked_slot`]'s two halves each decide.
    ///
    /// [`checked_slot`]: Shared::checked_slot
    #[cfg(test)]
    pub(super) fn checked_slot_any(&self, p: *mut u8) -> Option<usize> {
        self.set_slot(p).map(|(class, _)| class)
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

    /// Allocate a huge run of `size` bytes aligned to `align` in `arena`:
    /// an automatic arena not built yet (a thread whose first allocation
    /// is huge) is built here, an index that does not exist aborts. Null
    /// on exhaustion; the flag says the memory is known zero.
    pub fn alloc_huge(&self, size: usize, align: usize, arena: u16) -> (*mut u8, bool) {
        let Some(a) = self.arena(arena) else {
            no_such_arena(arena)
        };
        let r = self
            .huge
            .alloc_run(
                &self.meta,
                size,
                align,
                arena,
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
            .0;
        if !r.ptr.is_null() {
            // The pages the run's descriptor records, which is what
            // `free_huge` will subtract.
            a.note_huge_alloc(r.pages);
        }
        (r.ptr, r.zeroed)
    }

    /// `zero_on_free` for a huge run: clear the usable bytes of the live
    /// run starting at `page` (aborts unless it is one) before its release.
    /// Out of line: it takes the huge lock for the size and then writes
    /// the whole run.
    #[cold]
    #[inline(never)]
    pub fn zero_huge(&self, page: usize) {
        let n = self.huge.usable_size(page, Tracked::assume_new());
        // SAFETY: a live run's usable bytes are committed, writable memory
        // this allocator handed out and the caller is freeing.
        unsafe { ptr::write_bytes(self.regions.huge_addr(page) as *mut u8, 0, n) };
    }

    /// Free the huge run starting at `page`. Aborts unless it is a live
    /// run start. Returns the usable bytes released.
    pub fn free_huge(&self, page: usize) -> usize {
        let run = self.huge.free_run(
            &self.meta,
            page,
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        self.owner(run.arena).note_huge_free(run.pages);
        run.usable
    }
}

#[cold]
#[inline(never)]
fn no_such_arena(index: u16) -> ! {
    panic!("arena {index} does not exist")
}

/// [`Shared::check_size_hint`]'s abort on a slab slot whose class is not
/// the hint's.
#[cold]
#[inline(never)]
#[track_caller]
fn hint_mismatch(p: *mut u8, size: usize, class: u32) -> ! {
    panic!(
        "sized free of {:#x}: size hint {} does not match the allocation (class {}, {} bytes)",
        p as usize,
        size,
        class,
        CLASSES.table()[class as usize].size()
    )
}

/// [`Shared::check_size_hint`]'s abort on a huge run whose usable size is
/// not the hint's.
#[cold]
#[inline(never)]
#[track_caller]
fn huge_hint_mismatch(p: *mut u8, size: usize, usable: usize) -> ! {
    panic!(
        "sized free of {:#x}: size hint {} does not match the allocation (huge run, {} bytes)",
        p as usize, size, usable
    )
}

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
