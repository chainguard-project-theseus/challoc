//! [`Shared`], the process-wide allocator state every heap works against,
//! and the checks it performs for the fast and slow paths; the thread-exit
//! key and the thread id, clock and seed a heap reads, and the sized fast
//! path's [`GuardTest`].

use crate::sync::atomic::{AtomicBool, Ordering};
use core::ffi::c_void;
use core::ptr;

use crate::arena::{Arena, Arenas, Held, elem_at, field_at, fits};
#[cfg(verus_only)]
use crate::arena::{
    elems_at, elems_init, elems_uninit, field_fits, field_ptr, split_array, storage_ok, take_field,
};
use crate::class::SlabTable;
use crate::config::Config;
use crate::huge::Huge;
#[cfg(verus_only)]
use crate::layout::class_table;
use crate::layout::{CLASSES, Kind, NCLASSES, Regions};
use crate::lock::RawLock;
use crate::lock::{Lock, LockOrder};
#[cfg(verus_only)]
use crate::lock::{
    axiom_entry, rank_arenas, rank_class, rank_huge, rank_meta, rank_stats, rank_tcaches,
};
use crate::meta::Meta;
use crate::policy::Policy;
use crate::stats::Snapshot;
#[cfg(verus_only)]
use crate::stats::axiom_snapshot_lock_inv;
use crate::sys::cgroup::Cgroup;
#[cfg(verus_only)]
use crate::vspec::once;
use crate::vspec::once::{Once, Step};
#[cfg(verus_only)]
use crate::vspec::ptr::{axiom_ptr_add_addr, axiom_raw_valid, in_alloc, ptr_add};
use crate::vspec::unbacked::Unbacked;
use crate::vspec::zero::Zeroed;

use super::heap_destructor;
use super::tcaches::Tcaches;
#[cfg(verus_only)]
use super::thread::reentry_benign;
use super::{Heap, PUSH_PAC, PUSH_SCAN, PUSH_ZERO, invalid_free};
#[cfg(verus_only)]
use crate::class::slot_loc;
#[cfg(verus_only)]
use crate::class::{aset, distinct, ptr_addrs};
use crate::sys::tls::TlsSlot;
#[cfg(verus_only)]
use vstd::layout::size_of;
use vstd::prelude::verus;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Map, Tracked, int};
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
/// `repr(C)` fixes the order: `multi_arena` and `regions`, read by every
/// `malloc` and `free`, come first, then the line-aligned tables (read by
/// every `free`) and arena 0's class states, so the locks inside `meta` and
/// `huge` (taken on heap creation and huge allocations) never share a cache
/// line with them.
#[repr(C)]
pub struct Shared {
    /// Set once a second arena exists. Until then every slab and every
    /// heap is arena 0 and the `free` fast path skips the descriptor's
    /// arena load. Relaxed: a slot of arena `a > 0` can only be freed after
    /// it was allocated, which happened after `a` was built and this flag
    /// stored, and the program itself orders that allocation before the
    /// free; so any thread freeing such a slot already sees the flag.
    ///
    /// First, so it shares a cache line with the `Regions` fields that
    /// `slab_slot` reads on every `free` (`superblock`, `lg_stride`, the
    /// low `class_base` entries): after `Regions` it would sit on its own
    /// line, and larson's working set turns every extra line into an L2
    /// hit.
    pub(crate) multi_arena: AtomicBool,
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
    pub regions: Regions,
    /// The sized fast path's guard test per class: a dense array (8 bytes
    /// per class, seven lines for all 52) so `sized_slot` never touches
    /// the 384-byte class table line (it reads neither the watermark nor
    /// the descriptor; both checks are the flush's).
    pub(crate) guard_test: [GuardTest; NCLASSES],
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
    /// The memory cgroup as last sampled (design 11; `sys::cgroup`). Cold:
    /// written by init and by the rate-limited sampler on slow paths, read
    /// by `stats.cgroup.*` and the reports. Last, well away from the lines
    /// the fast paths read.
    pub cgroup: crate::sys::cgroup::Cgroup,
}

/// [`SlabTable::is_guard_slab`] folded into one add and one mask on the
/// slab number, for the sized fast path: `(slab + add) & mask == 0` with
/// `add = slabs_per_page` and `mask = (interval - 1) << lg_slabs_per_page`
/// says `((slab >> lg_spp) + 1) & (interval - 1) == 0`, the page-group
/// test, since `slab + spp` is `((slab >> lg_spp) + 1) << lg_spp` plus the
/// low bits the shifted mask ignores. With guards off the mask is
/// `u32::MAX << lg_spp` and `slab + spp` is never below `spp`, so nothing
/// matches. `Shared::init` fills one per class from its table;
/// `guard_test_agrees_with_the_tables` checks the two forms against each
/// other over every class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct GuardTest {
    add: u32,
    mask: u32,
}

} // verus!

verus! {

impl GuardTest {
    /// `self` is the test of a table whose slabs per page is `1 << lg` (at
    /// most a region's `2^26` slabs) and
    /// whose guard mask is `mask` ([`GuardTest::of`]).
    pub(super) closed spec fn tests(self, lg: u8, mask: u32) -> bool {
        &&& lg <= 26
        &&& self.add == 1u32 << lg
        &&& self.mask == mask << lg
    }

    /// What [`GuardTest::is_guard`] computes.
    pub(super) closed spec fn spec_is_guard(self, slab: usize) -> bool {
        (slab as u32).wrapping_add(self.add) & self.mask == 0
    }

    /// Whether `slab` is a guard slab of the class this pair describes.
    #[inline(always)]
    pub(super) fn is_guard(self, slab: usize) -> (r: bool)
        ensures
            r == self.spec_is_guard(slab),
        opens_invariants none
    {
        (slab as u32).wrapping_add(self.add) & self.mask == 0
    }

    /// The folded test agrees with the table's ([`guard_of`]) on every
    /// slab of a region (at most `2^26`), whose page groups are at most
    /// the region (`lg <= 26`): `slab + (1 << lg)` neither wraps nor
    /// reaches the mask bits the shift drops.
    ///
    /// [`guard_of`]: crate::layout::guard_of
    pub(super) proof fn lemma_is_guard(self, lg: u8, mask: u32, slab: usize)
        requires
            self.tests(lg, mask),
            slab < 0x400_0000,
        ensures
            self.spec_is_guard(slab) == crate::layout::guard_of(lg, mask, slab),
    {
        reveal(crate::layout::guard_of);
        let (add, m) = (self.add, self.mask);
        let s = slab as u32;
        let pg = (slab >> lg) as u32;
        assert(s < 0x400_0000u32 && pg == s >> lg && pg < 0x400_0000u32) by (bit_vector)
            requires slab < 0x400_0000, s == slab as u32, pg == (slab >> lg) as u32;
        assert(add <= 0x400_0000u32) by (bit_vector)
            requires add == 1u32 << lg, lg <= 26;
        let sum = (s + add) as u32;
        let pg1 = (pg + 1) as u32;
        assert((sum & m == 0) == (pg1 & mask == 0)) by (bit_vector)
            requires
                add == 1u32 << lg,
                m == mask << lg,
                lg <= 26,
                s < 0x400_0000u32,
                sum == s + add,
                pg == s >> lg,
                pg1 == pg + 1,
        ;
    }

    pub(super) fn of(table: &SlabTable) -> (r: GuardTest)
        ensures
            r.tests(crate::layout::guard_geom(table.spec_class() as int).0, crate::layout::guard_geom(table.spec_class() as int).1),
    {
        let lg_spp = table.slabs_per_page().trailing_zeros();
        proof {
            let lg = crate::layout::guard_geom(table.spec_class() as int).0;
            let spp = table.spec_slabs_per_page();
            assert(spp == 1usize << (lg as u64)) by (bit_vector)
                requires spp == 1usize << lg;
            crate::class::lemma_tz_shl(spp, lg as u64);
            assert(spp as u32 == 1u32 << lg) by (bit_vector)
                requires spp == 1usize << lg, lg <= 26;
        }
        GuardTest {
            add: table.slabs_per_page() as u32,
            mask: table.guard_mask().wrapping_shl(lg_spp),
        }
    }
}

} // verus!

const _: () = assert!(
    core::mem::offset_of!(Shared, multi_arena) / 128
        == (core::mem::offset_of!(Shared, regions) + core::mem::offset_of!(Regions, superblock))
            / 128
);
const _: () = assert!(
    core::mem::offset_of!(Shared, multi_arena) / 128
        == (core::mem::offset_of!(Shared, regions) + core::mem::offset_of!(Regions, lg_stride))
            / 128
);

verus! {

/// The `pthread_key` whose destructor tears down a thread's heap.
pub(crate) struct ThreadKey {
    key: libc::pthread_key_t,
    valid: bool,
}

} // verus!

verus! {

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
}

verus! {

impl ThreadKey {
    /// Register `heap` for destruction when the calling thread exits.
    /// `pthread_setspecific` may re-enter the allocator on this thread, so
    /// the slot (`tls`) must hold a sentinel ([`reentry_benign`]); the
    /// call leaves it as it was.
    pub fn set(&self, heap: *mut Heap, Tracked(tls): Tracked<&mut TlsSlot>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            reentry_benign(old(tls).value()),
        ensures
            *final(tls) == *old(tls),
    {
        if !self.valid {
            return;
        }
        // SAFETY: the key was created by `create`; glibc stores the
        // pointer without dereferencing it.
        if unsafe { pthread_setspecific(self.key, heap as *const c_void, Tracked(tls)) } != 0 {
            crate::sys::abort_fmt(format_args!("heap: pthread_setspecific failed"));
        }
    }
}

/// `pthread_setspecific(3)`: stores `value` for the calling thread under
/// `key` without dereferencing it; 0 on success, an error number
/// otherwise (nothing is claimed about it). It may allocate (glibc
/// `calloc`s a second-level block of 32 keys the first time a thread
/// sets a non-null value for a key in it), re-entering the allocator on
/// this thread, so it may reach the kernel (`may_syscall`, theorem 8).
/// Each nested entry stands for a `heap::thread` core
/// holding `tls`, lent to it ([`TlsSlot`]): at a [`reentry_benign`]
/// value the cores return `None` and leave the token as they found it
/// ([`heap_for_thread_in`](super::thread::heap_for_thread_in),
/// [`existing_heap_in`](super::thread::existing_heap_in)), and the
/// background thread's `create` writes nothing, so the call leaves `tls`
/// unchanged. Trusted: an FFI call, and the premise that every nested
/// entry reaches the slot through those.
///
/// # Safety
/// `key` was created by `pthread_key_create`.
#[verifier::external_body]
#[inline(always)]
unsafe fn pthread_setspecific(key: libc::pthread_key_t, value: *const c_void, Tracked(tls): Tracked<&mut TlsSlot>) -> (r: i32)
    requires
        crate::sys::may_syscall(),
        reentry_benign(old(tls).value()),
    ensures
        *final(tls) == *old(tls),
{
    // SAFETY: guaranteed by the caller.
    unsafe { libc::pthread_setspecific(key, value) }
}

pub(super) fn now_ns() -> u64
    requires
        crate::sys::may_syscall(),
{
    crate::sys::now_coarse_ns()
}

pub(super) fn tid() -> u32
    requires
        crate::sys::may_syscall(),
{
    crate::sys::gettid() as u32
}

pub(super) fn ncpus() -> u32
    requires
        crate::sys::may_syscall(),
{
    crate::sys::ncpus()
}

/// Eight random bytes from `getrandom(2)` for `slot_randomize`; on a
/// kernel that refuses (not seen on any supported one) the clock, so
/// the heap is still seeded with something that varies. Claims nothing
/// about the value.
pub(super) fn seed() -> u64
    requires
        crate::sys::may_syscall(),
{
    let mut b = [0u8; 8];
    match crate::sys::getrandom_exact(&mut b) {
        // The bytes as a little-endian word, which is
        // `u64::from_ne_bytes(b)` on x86_64 and aarch64 (any word would
        // do), spelled out: Verus cannot state `from_ne_bytes`'s
        // signature (its array length is `size_of::<Self>()`). The
        // compiler folds this into the same 8-byte load.
        Ok(()) => {
            b[0] as u64 | (b[1] as u64) << 8 | (b[2] as u64) << 16 | (b[3] as u64) << 24 | (b[4] as u64) << 32
                | (b[5] as u64) << 40 | (b[6] as u64) << 48 | (b[7] as u64) << 56
        },
        Err(_) => crate::sys::now_coarse_ns(),
    }
}

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

/// The address of the process's `Shared` storage (`api`'s `SHARED`).
pub uninterp spec fn shared_storage() -> *mut Shared;

/// The `Shared` built in place at `p`: [`Shared::init`] writes each field
/// once, before anything reads it, and nothing writes the fields
/// [`Shared::wf`] reads afterwards, so the value is one value.
/// Uninterpreted; [`axiom_shared_regions`] and [`axiom_shared_built`]
/// fix the fields `wf` names from the permissions `init` wrote them through.
pub uninterp spec fn shared_at(p: *mut Shared) -> Shared;

/// Assumption: one `Shared` per process. The process's regions and its
/// pointer-signing mode ([`process_regions`], [`super::pac_mode`]) are
/// those of the `Shared` in its storage, which `api::init` builds once,
/// before it publishes `READY`. Both are uninterpreted and no other axiom
/// relates them to a value, so this defines them; trusted accessors
/// (`fast_flags`, the slot readers) state their results in terms of them.
/// The unit tests build further `Shared`s elsewhere; the assumption is
/// about the shipped library.
///
/// [`process_regions`]: crate::layout::process_regions
pub(crate) axiom fn axiom_process_shared()
    ensures
        crate::layout::process_regions() == shared_at(shared_storage()).regions,
        super::pac_mode() == (shared_at(shared_storage()).push_flags & PUSH_PAC != 0),
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
}

// Byte offsets of `Shared`'s fields: `offset_of!`, which Verus cannot
// evaluate, so their values are opaque to it; `axiom_shared_layout`
// states what verified code relies on, and the build checks it.
#[verifier::external_body]
const S_MULTI_ARENA: usize = core::mem::offset_of!(Shared, multi_arena);
#[verifier::external_body]
const S_PUSH_FLAGS: usize = core::mem::offset_of!(Shared, push_flags);
#[verifier::external_body]
const S_REGIONS: usize = core::mem::offset_of!(Shared, regions);
#[verifier::external_body]
const S_GUARD_TEST: usize = core::mem::offset_of!(Shared, guard_test);
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
        field_fits::<Shared, AtomicBool>(S_MULTI_ARENA, S_PUSH_FLAGS as int),
        field_fits::<Shared, u8>(S_PUSH_FLAGS, S_REGIONS as int),
        field_fits::<Shared, Regions>(S_REGIONS, S_GUARD_TEST as int),
        field_fits::<Shared, [GuardTest; NCLASSES]>(S_GUARD_TEST, S_TABLES as int),
        field_fits::<Shared, [SlabTable; NCLASSES]>(S_TABLES, S_ARENA0 as int),
        NCLASSES * size_of::<GuardTest>() <= isize::MAX,
        NCLASSES * size_of::<SlabTable>() <= isize::MAX,
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
        field_fits::<Shared, Cgroup>(S_CGROUP, size_of::<Shared>() as int),
        size_of::<Shared>() <= isize::MAX,
;

} // verus!

// `axiom_shared_layout`, checked.
const _: () = assert!(
    fits::<Shared, AtomicBool>(S_MULTI_ARENA, S_PUSH_FLAGS)
        && fits::<Shared, u8>(S_PUSH_FLAGS, S_REGIONS)
        && fits::<Shared, Regions>(S_REGIONS, S_GUARD_TEST)
        && fits::<Shared, [GuardTest; NCLASSES]>(S_GUARD_TEST, S_TABLES)
        && fits::<Shared, [SlabTable; NCLASSES]>(S_TABLES, S_ARENA0)
        && NCLASSES as u128 * core::mem::size_of::<GuardTest>() as u128 <= isize::MAX as u128
        && NCLASSES as u128 * core::mem::size_of::<SlabTable>() as u128 <= isize::MAX as u128
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
        && fits::<Shared, Cgroup>(S_CGROUP, core::mem::size_of::<Shared>())
        && core::mem::size_of::<Shared>() <= isize::MAX as usize
);

// `arena::Arena::axiom_array_layout` at `split`'s two instances, checked.
const _: () = assert!(
    core::mem::size_of::<[GuardTest; NCLASSES]>() == NCLASSES * core::mem::size_of::<GuardTest>()
        && core::mem::align_of::<[GuardTest; NCLASSES]>() == core::mem::align_of::<GuardTest>()
        && core::mem::size_of::<[SlabTable; NCLASSES]>()
            == NCLASSES * core::mem::size_of::<SlabTable>()
        && core::mem::align_of::<[SlabTable; NCLASSES]>() == core::mem::align_of::<SlabTable>()
);

verus! {

/// The permissions to the fields of a `Shared` under construction but
/// its regions (frozen once written: [`axiom_shared_regions`]) and its
/// guard tests and tables, which are maps of their own, element by
/// element ([`Shared::split`]). `arena0` and `arenas` are [`Held`]
/// because `Arena::init` and `Arenas::init` take theirs out and
/// hand them back.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
tracked struct SharedParts {
    tracked multi_arena: PointsTo<AtomicBool>,
    tracked push_flags: PointsTo<u8>,
    tracked arena0: Held<Arena>,
    tracked meta: PointsTo<Meta>,
    tracked huge: PointsTo<Huge>,
    tracked gc_interval_ns: PointsTo<u64>,
    tracked policy: PointsTo<Policy>,
    tracked config: PointsTo<Config>,
    tracked key: PointsTo<ThreadKey>,
    tracked arenas: Held<Arenas>,
    tracked tcaches: PointsTo<Tcaches>,
    tracked stats: PointsTo<Lock<Snapshot>>,
    tracked cgroup: PointsTo<Cgroup>,
}

/// The `Shared` at `p` is [`storage_ok`], and the arrays inside it are
/// inside its allocation.
spec fn shared_ok(p: *mut Shared) -> bool {
    &&& storage_ok(p)
    &&& in_alloc(field_ptr::<Shared, GuardTest>(p, S_GUARD_TEST), NCLASSES as int * size_of::<GuardTest>())
    &&& NCLASSES * size_of::<GuardTest>() <= isize::MAX
    &&& in_alloc(field_ptr::<Shared, SlabTable>(p, S_TABLES), NCLASSES as int * size_of::<SlabTable>())
    &&& NCLASSES * size_of::<SlabTable>() <= isize::MAX
    &&& field_ptr::<Shared, SlabTable>(p, S_TABLES)@.addr + NCLASSES * size_of::<SlabTable>() <= usize::MAX
}

impl SharedParts {
    /// Each permission is to its field of the `Shared` at `p`
    /// ([`shared_ok`]); `arena0` and `arenas` when they are held.
    spec fn at(&self, p: *mut Shared) -> bool {
        &&& shared_ok(p)
        &&& self.multi_arena.ptr() == field_ptr::<Shared, AtomicBool>(p, S_MULTI_ARENA)
        &&& self.push_flags.ptr() == field_ptr::<Shared, u8>(p, S_PUSH_FLAGS)
        &&& (self.arena0.perm matches Some(a) ==> a.ptr() == field_ptr::<Shared, Arena>(p, S_ARENA0))
        &&& self.meta.ptr() == field_ptr::<Shared, Meta>(p, S_META)
        &&& self.huge.ptr() == field_ptr::<Shared, Huge>(p, S_HUGE)
        &&& self.gc_interval_ns.ptr() == field_ptr::<Shared, u64>(p, S_GC_INTERVAL_NS)
        &&& self.policy.ptr() == field_ptr::<Shared, Policy>(p, S_POLICY)
        &&& self.config.ptr() == field_ptr::<Shared, Config>(p, S_CONFIG)
        &&& self.key.ptr() == field_ptr::<Shared, ThreadKey>(p, S_KEY)
        &&& (self.arenas.perm matches Some(a) ==> a.ptr() == field_ptr::<Shared, Arenas>(p, S_ARENAS))
        &&& self.tcaches.ptr() == field_ptr::<Shared, Tcaches>(p, S_TCACHES)
        &&& self.stats.ptr() == field_ptr::<Shared, Lock<Snapshot>>(p, S_STATS)
        &&& self.cgroup.ptr() == field_ptr::<Shared, Cgroup>(p, S_CGROUP)
    }

    /// Every field is written.
    spec fn written(&self) -> bool {
        &&& self.multi_arena.is_init()
        &&& self.push_flags.is_init()
        &&& self.arena0.perm matches Some(a) && a.is_init()
        &&& self.meta.is_init()
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

/// The guard test permissions of the `Shared` at `p`, element by element.
spec fn guard_tests_at(m: Map<int, PointsTo<GuardTest>>, p: *mut Shared) -> bool {
    elems_at(m, field_ptr::<Shared, GuardTest>(p, S_GUARD_TEST), 0, NCLASSES as int)
}

/// The table permissions of the `Shared` at `p`, element by element.
spec fn tables_at(m: Map<int, PointsTo<SlabTable>>, p: *mut Shared) -> bool {
    elems_at(m, field_ptr::<Shared, SlabTable>(p, S_TABLES), 0, NCLASSES as int)
}

/// The written fields of the `Shared` at `p` (`Shared::split`'s parts and
/// guard tests, every one initialised, and the tables joined into their
/// array), as the value [`shared_at`] names: the fields [`Shared::wf`]
/// reads are what their permissions hold. Trusted, as
/// `arena::axiom_arena_assemble` is: vstd has no way to join typed
/// permissions, and the permissions cover every field at its offset. The
/// permissions are used up, so nothing verified writes the fields
/// afterwards; that nothing unverified does is `shared_at`'s own
/// assumption.
axiom fn axiom_shared_built(
    tracked parts: SharedParts,
    tracked guard_tests: Map<int, PointsTo<GuardTest>>,
    tracked tables: PointsTo<[SlabTable; NCLASSES]>,
    p: *mut Shared,
)
    requires
        parts.at(p),
        parts.written(),
        guard_tests_at(guard_tests, p),
        elems_init(guard_tests, 0, NCLASSES as int),
        tables.ptr() == field_ptr::<Shared, [SlabTable; NCLASSES]>(p, S_TABLES),
        tables.is_init(),
    ensures
        shared_at(p).push_flags == parts.push_flags.value(),
        shared_at(p).tables == tables.value(),
        forall|c: int| 0 <= c < NCLASSES ==> #[trigger] shared_at(p).guard_test[c] == guard_tests[c].value(),
        shared_at(p).meta == parts.meta.value(),
        shared_at(p).huge == parts.huge.value(),
        shared_at(p).config == parts.config.value(),
        parts.arena0.perm matches Some(a) && shared_at(p).arena0 == a.value(),
        shared_at(p).stats == parts.stats.value(),
        parts.arenas.perm matches Some(a) && shared_at(p).arenas == a.value(),
;

/// The regions of the `Shared` at `p` are what the permission to its
/// `regions` field holds, once that permission is frozen
/// (`vstd::modes::tracked_static_ref`, right after [`Shared::put_head`]
/// writes the field): no verified code can write the field again, so the
/// value is the built one. Trusted, as [`axiom_shared_built`] is; stated
/// apart because `init` needs it before the build is done (the process's
/// regions, [`axiom_process_shared`], are the arena's and the tables').
axiom fn axiom_shared_regions(tracked pr: &'static PointsTo<Regions>, p: *mut Shared)
    requires
        pr.ptr() == field_ptr::<Shared, Regions>(p, S_REGIONS),
        pr.is_init(),
    ensures
        shared_at(p).regions == pr.value(),
;

impl Shared {
    /// The permission to `init`'s storage, uninitialised, for `once`, the
    /// process's token of this step. Trusted: `init`'s caller hands over
    /// writable storage for a `Shared` that nothing else uses (its safety
    /// contract). The token is used up, so the permission is handed out
    /// once (`api::init_slow`'s winning exchange holds the one token,
    /// `api::claim_init`; the unit tests, unverified callers outside this
    /// contract, build a fresh leaked box each time): two permissions for
    /// one storage proved `false` (`PointsTo::is_disjoint`), and could
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
    /// [`axiom_shared_layout`] gives, and the guard tests and the tables
    /// split by [`split_array`]; the regions' permission last.
    proof fn split(tracked perm: PointsTo<Shared>) -> (tracked r: (
        SharedParts,
        Map<int, PointsTo<GuardTest>>,
        Map<int, PointsTo<SlabTable>>,
        PointsTo<Regions>,
    ))
        requires
            perm.is_uninit(),
            perm.ptr()@.addr + size_of::<Shared>() <= usize::MAX,
        ensures
            r.0.at(perm.ptr()),
            r.0.multi_arena.is_uninit(),
            r.0.push_flags.is_uninit(),
            r.3.ptr() == field_ptr::<Shared, Regions>(perm.ptr(), S_REGIONS),
            r.3.is_uninit(),
            r.0.arena0.perm matches Some(a) && a.is_uninit(),
            r.0.meta.is_uninit(),
            r.0.huge.is_uninit(),
            r.0.gc_interval_ns.is_uninit(),
            r.0.policy.is_uninit(),
            r.0.config.is_uninit(),
            r.0.key.is_uninit(),
            r.0.arenas.perm matches Some(a) && a.is_uninit(),
            r.0.tcaches.is_uninit(),
            r.0.stats.is_uninit(),
            r.0.cgroup.is_uninit(),
            guard_tests_at(r.1, perm.ptr()),
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
        let tracked (multi_arena, raw) = take_field::<Shared, AtomicBool>(raw, p, S_MULTI_ARENA, S_PUSH_FLAGS as int);
        let tracked (push_flags, raw) = take_field::<Shared, u8>(raw, p, S_PUSH_FLAGS, S_REGIONS as int);
        let tracked (regions, raw) = take_field::<Shared, Regions>(raw, p, S_REGIONS, S_GUARD_TEST as int);
        let tracked (guard_test, raw) = take_field::<Shared, [GuardTest; NCLASSES]>(
            raw,
            p,
            S_GUARD_TEST,
            S_TABLES as int,
        );
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
        let tracked (cgroup, _raw) = take_field::<Shared, Cgroup>(raw, p, S_CGROUP, size_of::<Shared>() as int);
        crate::arena::axiom_array_layout::<GuardTest, NCLASSES>();
        crate::arena::axiom_array_layout::<SlabTable, NCLASSES>();
        let tracked guard_tests = split_array(guard_test);
        let tracked tables = split_array(tables);
        crate::arena::lemma_field_cast::<Shared, [GuardTest; NCLASSES], GuardTest>(p, S_GUARD_TEST);
        crate::arena::lemma_field_cast::<Shared, [SlabTable; NCLASSES], SlabTable>(p, S_TABLES);
        (
            SharedParts {
                multi_arena,
                push_flags,
                arena0: Held { perm: Some(arena0) },
                meta,
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
            guard_tests,
            tables,
            regions,
        )
    }
}

/// [`Tcaches::init`] on the field at `p`, through the permission to it.
/// Trusted: `Tcaches::init` (`heap/tcaches.rs`) is not verified; it
/// writes every field of the registry.
///
/// # Safety
/// As `Tcaches::init`.
#[verifier::external_body]
#[inline(always)]
unsafe fn init_tcaches(p: *mut Tcaches, Tracked(perm): Tracked<&mut PointsTo<Tcaches>>)
    requires
        old(perm).ptr() == p,
        old(perm).is_uninit(),
    ensures
        final(perm).ptr() == p,
        final(perm).is_init(),
{
    // SAFETY: as the caller guarantees.
    unsafe { Tcaches::init(p) }
}

/// The in-place writes and reads of [`Shared::init`], each through the
/// permission to the field it writes or reads (`SharedParts`), in
/// `init`'s order; `this` is the storage `init` was handed.
impl Shared {
    /// `meta`, `regions` and `multi_arena` (false); the written metadata
    /// allocator.
    #[inline(always)]
    unsafe fn put_head<'a>(
        this: *mut Shared,
        meta: Meta,
        regions: Regions,
        Tracked(pm): Tracked<&'a mut PointsTo<Meta>>,
        Tracked(pr): Tracked<&mut PointsTo<Regions>>,
        Tracked(pa): Tracked<&mut PointsTo<AtomicBool>>,
    ) -> (r: &'a Meta)
        requires
            shared_ok(this),
            old(pm).ptr() == field_ptr::<Shared, Meta>(this, S_META),
            old(pr).ptr() == field_ptr::<Shared, Regions>(this, S_REGIONS),
            old(pa).ptr() == field_ptr::<Shared, AtomicBool>(this, S_MULTI_ARENA),
        ensures
            final(pm).ptr() == old(pm).ptr(),
            final(pm).is_init(),
            final(pm).value() == meta,
            final(pr).ptr() == old(pr).ptr(),
            final(pr).is_init(),
            final(pr).value() == regions,
            final(pa).ptr() == old(pa).ptr(),
            final(pa).is_init(),
            *r == meta,
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: the fields of `init`'s storage, through their
        // permissions; never moved.
        unsafe {
            let meta_ptr = field_at(this, S_META);
            ptr_mut_write(meta_ptr, Tracked(&mut *pm), meta);
            ptr_mut_write(field_at(this, S_REGIONS), Tracked(pr), regions);
            ptr_mut_write(field_at(this, S_MULTI_ARENA), Tracked(pa), AtomicBool::new(false));
            ptr_ref(meta_ptr, Tracked(&*pm))
        }
    }

    /// `push_flags`; the written regions.
    #[inline(always)]
    unsafe fn put_flags<'a>(
        this: *mut Shared,
        flags: u8,
        Tracked(pf): Tracked<&mut PointsTo<u8>>,
        Tracked(pr): Tracked<&'a PointsTo<Regions>>,
    ) -> (r: &'a Regions)
        requires
            shared_ok(this),
            old(pf).ptr() == field_ptr::<Shared, u8>(this, S_PUSH_FLAGS),
            pr.ptr() == field_ptr::<Shared, Regions>(this, S_REGIONS),
            pr.is_init(),
        ensures
            final(pf).ptr() == old(pf).ptr(),
            final(pf).is_init(),
            final(pf).value() == flags,
            *r == pr.value(),
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: as in `put_head`; `regions` was written by `put_head`.
        unsafe {
            ptr_mut_write(field_at(this, S_PUSH_FLAGS), Tracked(pf), flags);
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

    /// Every table, written by [`Shared::put_table`] and joined into the
    /// array `pt`.
    #[inline(always)]
    unsafe fn tables_of<'a>(this: *mut Shared, Tracked(pt): Tracked<&'a PointsTo<[SlabTable; NCLASSES]>>) -> (r: &'a [SlabTable; NCLASSES])
        requires
            shared_ok(this),
            pt.ptr() == field_ptr::<Shared, [SlabTable; NCLASSES]>(this, S_TABLES),
            pt.is_init(),
        ensures
            *r == pt.value(),
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: written by `put_table`; never moved.
        unsafe { ptr_ref(field_at(this, S_TABLES), Tracked(pt)) }
    }

    /// `guard_test[c]`, through the permission `pg` to it.
    #[inline(always)]
    unsafe fn put_guard_test(this: *mut Shared, c: usize, g: GuardTest, Tracked(pg): Tracked<&mut PointsTo<GuardTest>>)
        requires
            shared_ok(this),
            c < NCLASSES,
            old(pg).ptr() == ptr_add(field_ptr::<Shared, GuardTest>(this, S_GUARD_TEST), c),
        ensures
            final(pg).ptr() == old(pg).ptr(),
            final(pg).is_init(),
            final(pg).value() == g,
    {
        proof {
            axiom_shared_layout();
        }
        // SAFETY: element `c` of the field, through its permission.
        unsafe {
            let guard_test: *mut GuardTest = field_at(this, S_GUARD_TEST);
            ptr_mut_write(elem_at(guard_test, c, NCLASSES), Tracked(pg), g)
        }
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
    /// `arena0`, `Tcaches::init`).
    #[inline(always)]
    unsafe fn put_registries(
        this: *mut Shared,
        gc_interval_ns: u64,
        arena0: *mut Arena,
        narenas: u32,
        Tracked(pg): Tracked<&mut PointsTo<u64>>,
        Tracked(pa): Tracked<&mut Held<Arenas>>,
        Tracked(pt): Tracked<&mut PointsTo<Tcaches>>,
    )
        requires
            shared_ok(this),
            old(pg).ptr() == field_ptr::<Shared, u64>(this, S_GC_INTERVAL_NS),
            old(pa).perm matches Some(a) && a.ptr() == field_ptr::<Shared, Arenas>(this, S_ARENAS) && a.is_uninit(),
            old(pt).ptr() == field_ptr::<Shared, Tcaches>(this, S_TCACHES),
            old(pt).is_uninit(),
        ensures
            final(pg).ptr() == old(pg).ptr(),
            final(pg).is_init(),
            final(pa).perm matches Some(a) && a.ptr() == field_ptr::<Shared, Arenas>(this, S_ARENAS) && a.is_init() && a.value().wf(),
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
            Arenas::init(field_at(this, S_ARENAS), arena0, narenas, Tracked(pa));
            init_tcaches(field_at(this, S_TCACHES), Tracked(pt));
        }
    }

    /// `cgroup`; the written record.
    #[inline(always)]
    unsafe fn put_cgroup<'a>(this: *mut Shared, cgroup: Cgroup, Tracked(pc): Tracked<&'a mut PointsTo<Cgroup>>) -> (r: &'a Cgroup)
        requires
            shared_ok(this),
            old(pc).ptr() == field_ptr::<Shared, Cgroup>(this, S_CGROUP),
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
    unsafe fn put_policy<'a>(this: *mut Shared, policy: Policy, Tracked(pp): Tracked<&'a mut PointsTo<Policy>>) -> (r: &'a Policy)
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
        Tracked(ps): Tracked<&mut PointsTo<Lock<Snapshot>>>,
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
    /// [`Shared::wf`]. Every field is written through the permission to it,
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
    // The second loop writes `guard_test[c]` of the storage from `tables[c]`.
    #[allow(clippy::needless_range_loop)]
    #[verifier::loop_isolation(false)]
    #[verifier::rlimit(20)]
    // Own prover: the proof went past its rlimit in the module's shared
    // Z3 context once unrelated functions of this module changed.
    #[verifier::spinoff_prover]
    pub unsafe fn init(
        this: *mut Shared,
        regions: Regions,
        space: Tracked<Unbacked>,
        config: Config,
        guard_install: bool,
        pac: bool,
        Tracked(steps): Tracked<Map<Step, Once>>,
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
            pac ==> crate::sys::pac::pac_available(),
            once::all_steps(steps),
        ensures
            r ==> shared_at(this).wf(),
    {
        // Premise (lock order): no allocator lock exists before this
        // `Shared` is built (`init_slow` runs `init` once, before the
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
        let tracked (mut parts, mut guard_tests, mut table_perms, mut regions_perm) = Shared::split(perm);
        proof {
            assert(shared_ok(this));
            crate::arena::lemma_field_cast::<Shared, [SlabTable; NCLASSES], SlabTable>(this, S_TABLES);
        }
        let ghost regions0 = regions;
        proof {
            assert(crate::config::MAX_GUARD_SLAB_INTERVAL == 0x40_0000) by (compute);
            axiom_process_shared();
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
                Tracked(&mut parts.multi_arena),
            );
            // Frozen: nothing writes the field again.
            let tracked regions_perm = vstd::modes::tracked_static_ref(regions_perm);
            proof {
                axiom_shared_regions(regions_perm, this);
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
                Tracked(&mut parts.push_flags),
                Tracked(regions_perm),
            );
            proof {
                // The `PUSH_PAC` bit is set only from `pac` (the probe).
                let f = parts.push_flags.value();
                let x: u8 = if config.double_free_scan { PUSH_SCAN } else { 0 };
                let y: u8 = if config.zero_on_free { PUSH_ZERO } else { 0 };
                let z: u8 = if pac && config.pac_pointers { PUSH_PAC } else { 0 };
                assert(f == x | y | z);
                assert((x | y | z) & 4u8 != 0 ==> z == 4u8) by (bit_vector)
                    requires
                        x == 0 || x == 1,
                        y == 0 || y == 2,
                        z == 0 || z == 4,
                ;
            }
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
                    forall|d: int| 0 <= d < c ==> (#[trigger] table_perms[d]).value().meta_ok(),
                    ord.allows(rank_meta()),
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
                            assert forall|d: int| 0 <= d < NCLASSES implies #[trigger] table_perms.contains_key(d)
                                && table_perms[d].ptr() == ptr_add(field_ptr::<Shared, SlabTable>(this, S_TABLES), d as usize) by {
                                assert(before.contains_key(d));
                            }
                            assert forall|d: int| 0 <= d < NCLASSES implies (d < c + 1 ==> (#[trigger] table_perms[d]).is_init()
                                && table_perms[d].value().spec_class() == d && table_perms[d].value().meta_ok())
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
            let tracked tables_perm = crate::arena::axiom_array_join(
                field_ptr::<Shared, [SlabTable; NCLASSES]>(this, S_TABLES),
                table_perms,
            );
            let tables = Shared::tables_of(this, Tracked(&tables_perm));
            for c in 0..NCLASSES
                invariant
                    guard_tests_at(guard_tests, this),
                    elems_init(guard_tests, 0, c as int),
                    forall|d: int| 0 <= d < NCLASSES ==> (#[trigger] tables[d]).spec_class() == d,
                    forall|d: int| 0 <= d < c ==> (#[trigger] guard_tests[d]).value().tests(
                        crate::layout::guard_geom(d).0, crate::layout::guard_geom(d).1),
            {
                let ghost before = guard_tests;
                proof {
                    assert(guard_tests.contains_key(c as int));
                }
                let tracked pg = guard_tests.tracked_borrow_mut(c as int);
                Shared::put_guard_test(this, c, GuardTest::of(&tables[c]), Tracked(pg));
                proof {
                    assert forall|d: int| 0 <= d < NCLASSES implies #[trigger] guard_tests.contains_key(d)
                        && guard_tests[d].ptr() == ptr_add(field_ptr::<Shared, GuardTest>(this, S_GUARD_TEST), d as usize) by {
                        assert(before.contains_key(d));
                    }
                    assert forall|d: int| 0 <= d < c + 1 implies (#[trigger] guard_tests[d]).is_init()
                        && guard_tests[d].value().tests(crate::layout::guard_geom(d).0, crate::layout::guard_geom(d).1) by {
                        if d != c {
                            assert(before[d] == guard_tests[d]);
                        }
                    }
                }
            }
            let arena0 = Shared::arena0_ptr(this);
            proof {
                axiom_shared_layout();
                axiom_ptr_add_addr(this as *mut u8, S_ARENA0);
            }
            if !Arena::init(arena0, meta, regions, tables, 0, decay_ns, Tracked(&mut parts.arena0), Tracked(&mut ord)) {
                return false;
            }
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
                config.narenas,
                Tracked(&mut parts.gc_interval_ns),
                Tracked(&mut parts.arenas),
                Tracked(&mut parts.tcaches),
            );
            let cgroup = Shared::put_cgroup(
                this,
                crate::sys::cgroup::Cgroup::discover(config.cgroup),
                Tracked(&mut parts.cgroup),
            );
            // Design 11: under an effective limit below 1 GiB a configured
            // `thp:always` is forced to `never` in the stored configuration
            // (`auto` is left alone; the report says so); the policy's
            // `high` baseline is the count at init.
            let thp_forced =
                crate::thp::forced_by_limit(config.thp, cgroup.limit().min(cgroup.high()));
            if thp_forced {
                config.thp = crate::config::Thp::Never;
            }
            let policy = Shared::put_policy(
                this,
                crate::policy::Policy::new(cgroup.rises().0, thp_forced),
                Tracked(&mut parts.policy),
            );
            // Applied once on the discovery sample, so the band and the cap
            // are published before the first 10 ms window closes (nothing
            // is `Empty` yet: retention 0).
            let limit = cgroup.limit().min(cgroup.high());
            policy.apply(
                crate::policy::band(limit, cgroup.current()),
                limit,
                0,
                cgroup.rises().0,
            );
            Shared::put_tail(
                this,
                config,
                Tracked(&mut parts.config),
                Tracked(&mut parts.key),
                Tracked(&mut parts.stats),
            );
            proof {
                axiom_shared_built(parts, guard_tests, tables_perm, this);
                let s = shared_at(this);
                s.arenas.lemma_is_process();
                assert(s.regions == regions0);
                assert(s.huge_pages() == s.regions.huge_len >> s.regions.lg_page);
            }
        }
        true
    }
}

} // verus!

verus! {

impl Shared {
    /// What `Shared::init` establishes of the value it builds, and nothing
    /// changes afterwards (every field is written once, before the
    /// `Shared` is published): the address-space layout is well formed and
    /// separated and is the process's ([`crate::layout::process_regions`]);
    /// `tables[c]` is class `c`'s table, and `guard_test[c]` its guard test
    /// ([`GuardTest::of`], over the class's guard geometry,
    /// [`guard_geom`](crate::layout::guard_geom)); `huge` covers exactly the huge
    /// region's pages, starting at the huge base with the regions' page;
    /// the [`PUSH_PAC`] bit is [`super::pac_mode`], set only where the process
    /// may execute the PAC instructions (`sys::pac::pac_available`, from the
    /// probe `init` was given); `meta` allocates from
    /// the metadata region, which holds every class table's descriptors,
    /// bitmap and owner words; `arenas` is the registry `Arenas::init`
    /// built (`Arenas::wf`: its bound counts at their invariants), the
    /// process's (`Arenas::is_process`).
    ///
    /// Assumed by every verified function that takes a `&Shared`: the
    /// accessor that hands the published `Shared` out (`api::shared_ref`)
    /// claims it. [`Shared::init`] proves it of the value it builds (the
    /// configuration is well formed: `Config::wf`, which `config::load`
    /// ensures); what `shared_ref` adds is that the published value is that
    /// one, unchanged.
    pub closed spec fn wf(&self) -> bool {
        &&& self.regions.wf()
        &&& self.regions == crate::layout::process_regions()
        &&& self.regions.separated()
        &&& self.huge.spec_base() == self.regions.huge_base
        &&& self.huge.spec_page() == (1usize << self.regions.lg_page) as int
        &&& (self.push_flags & PUSH_PAC != 0) == super::pac_mode()
        &&& super::pac_mode() ==> crate::sys::pac::pac_available()
        &&& self.config.wf()
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.tables[c].spec_class() == c
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.guard_test[c].tests(crate::layout::guard_geom(c).0, crate::layout::guard_geom(c).1)
        &&& self.huge.spec_npages() == self.huge_pages()
        &&& self.meta.spec_base() == self.regions.meta_base
        &&& self.meta.spec_len() == self.regions.meta_len
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.tables[c].meta_ok()
        &&& self.arena0.spec_index() == 0
        &&& self.stats.rank() == rank_stats()
        &&& self.arenas.wf()
        &&& self.arenas.is_process()
    }

    /// `meta` is the process's metadata allocator ([`Shared::wf`]).
    pub(crate) proof fn lemma_meta(&self)
        requires
            self.wf(),
        ensures
            self.meta.is_process(),
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.tables[c].meta_ok(),
    {
    }

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

    /// A run [`Huge::alloc_run`] placed for `size` bytes at `align` is a
    /// [`huge_block`](crate::layout::huge_block).
    proof fn lemma_run_is_block(&self, a: usize, size: usize, align: usize)
        requires
            self.wf(),
            self.huge.spec_run_at(a, size, align),
        ensures
            crate::layout::huge_block(a, size, align),
    {
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

    /// A token that claims `p`, a page start of the huge region with page
    /// index `page`, is the live block's, and holds the token of the run
    /// at `page` whose usable bytes, in this `Shared`'s pages, are the
    /// block's.
    pub(crate) proof fn lemma_run_page(&self, p: *mut u8, page: usize, blk: super::Live)
        requires
            self.wf(),
            blk.claims(p),
            crate::layout::process_regions().is_huge_block(p@.addr, 1),
            page == ((p@.addr - crate::layout::process_regions().huge_base) as usize) >> crate::layout::process_regions().lg_page,
        ensures
            blk.block(p),
            blk.run_opt() is Some,
            blk.run_opt().unwrap().wf(),
            blk.run_opt().unwrap().start() == page,
            blk.len() == (blk.run_opt().unwrap().npages() - crate::huge::GUARD_PAGES) * self.huge.spec_page(),
    {
        self.lemma_regions_ok();
        blk.lemma_run_page(p, page);
        blk.lemma_claimed_block(p);
    }

    /// The usable bytes of a huge run for `size` ([`Huge::spec_usable`]).
    pub closed spec fn huge_usable(&self, size: usize) -> int {
        self.huge.spec_usable(size)
    }

    /// `r` is the usable size of a huge run at the page start `p`
    /// ([`Huge::spec_run_usable`] of its page), for specs outside `heap`.
    pub closed spec fn huge_run_usable(&self, p: usize, r: usize) -> bool {
        self.huge.spec_run_usable(((p - self.regions.huge_base) as usize) >> self.regions.lg_page, r)
    }

    /// [`Shared::huge_run_usable`] unfolded.
    pub(crate) proof fn lemma_huge_run_usable(&self, p: usize, r: usize)
        ensures
            self.huge_run_usable(p, r) == self.huge.spec_run_usable(((p - self.regions.huge_base) as usize) >> self.regions.lg_page, r),
    {
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

    /// The bytes [`Shared::huge_run_zero`] names are a block of the huge
    /// region: a page start with the run's usable bytes inside the region.
    pub(crate) proof fn lemma_run_zero_block(&self, z: Zeroed, page: usize)
        requires
            self.wf(),
            self.huge_run_zero(z, page),
        ensures
            self.regions.is_huge_block(z.addr() as usize, z.len()),
    {
        self.lemma_wf();
        let (len, lg32) = (self.regions.huge_len, self.regions.lg_page);
        let lg = lg32 as usize;
        let np = len >> lg32;
        let pg = 1usize << lg32;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg);
        assert(np == len >> lg && pg == 1usize << lg) by (bit_vector)
            requires
                np == len >> lg32,
                pg == 1usize << lg32,
                lg == lg32 as usize,
        ;
        vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(np, lg, len);
        assert(np * pg <= len);
        assert(page * pg <= np * pg) by (nonlinear_arith)
            requires
                page < np,
                pg > 0,
        ;
        vstd::bits::lemma_usize_shl_is_mul(page, lg);
        assert(page << lg32 == page << lg) by (bit_vector)
            requires
                lg == lg32 as usize,
        ;
        assert((page << lg32) == page * pg);
        vstd::arithmetic::div_mod::lemma_mod_multiples_basic(page as int, pg as int);
        assert(page * self.huge.spec_page() + z.len() <= np * pg);
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

    /// The flag byte [`Shared::push_flags`] returns.
    pub closed spec fn flags(&self) -> u8 {
        self.push_flags
    }

    /// [`Shared::wf`] unfolded, for the rest of the crate.
    pub(crate) proof fn lemma_wf(&self)
        requires
            self.wf(),
        ensures
            self.regions.wf(),
            self.regions == crate::layout::process_regions(),
            self.regions.separated(),
            self.huge.spec_base() == self.regions.huge_base,
            self.huge.spec_page() == (1usize << self.regions.lg_page) as int,
            (self.flags() & PUSH_PAC != 0) == super::pac_mode(),
            super::pac_mode() ==> crate::sys::pac::pac_available(),
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.tables[c].spec_class() == c,
            self.huge.spec_npages() == self.huge_pages(),
            self.config.wf(),
            self.huge_pages() == self.regions.huge_len >> self.regions.lg_page,
            self.meta.is_process(),
            self.arenas.wf(),
            self.arenas.is_process(),
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.tables[c].meta_ok(),
    {
    }

    /// The per-class slab tables (shared by every arena).
    #[inline(always)]
    pub fn tables(&self) -> (r: &[SlabTable; NCLASSES])
        ensures
            self.wf() ==> forall|c: int| 0 <= c < NCLASSES ==> #[trigger] r[c].spec_class() == c,
            *r == self.spec_tables(),
    {
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

    /// The process statistics lock's rank in the lock order (`lock.rs`).
    pub(crate) proof fn lemma_ranks(&self)
        requires
            self.wf(),
        ensures
            self.stats.rank() == rank_stats(),
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

    /// The arena creation lock, for the fork handlers.
    #[inline(always)]
    pub fn arenas_lock(&self) -> (r: &RawLock)
        ensures
            *r == self.spec_fork_locks().1,
            r.rank() == rank_arenas(),
    {
        self.arenas.raw_lock()
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

    /// The process statistics lock, for the fork handlers.
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
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of` only yields indices below `NCLASSES`.
        unsafe { self.tables.get_unchecked(class) }
    }

    /// Whether more than one arena exists (see the field).
    #[inline(always)]
    pub fn multi_arena(&self) -> bool opens_invariants none {
        crate::vspec::atomic::load_bool(&self.multi_arena, Ordering::Relaxed)
    }

    /// The flag byte both fast paths load: [`PUSH_SCAN`], [`PUSH_ZERO`]
    /// and [`PUSH_PAC`]; 0 in the default configuration on a CPU without
    /// pointer authentication.
    #[inline(always)]
    pub fn push_flags(&self) -> (r: u8)
        ensures
            r == self.flags(),
            self.wf() ==> ((r & PUSH_PAC != 0) == super::pac_mode()),
            self.wf() && (r & PUSH_PAC != 0) ==> crate::sys::pac::pac_available(),
        opens_invariants none
    {
        self.push_flags
    }

    /// Whether cache and heap pointers are signed ([`PUSH_PAC`]).
    #[inline(always)]
    pub fn pac(&self) -> (r: bool)
        ensures
            r == (self.flags() & PUSH_PAC != 0),
            self.wf() ==> r == super::pac_mode(),
            self.wf() && r ==> crate::sys::pac::pac_available(),
    {
        self.push_flags & PUSH_PAC != 0
    }

    /// Whether pushes compare against the top four cache entries
    /// (`double_free_scan`).
    #[inline(always)]
    pub fn double_free_scan(&self) -> (r: bool)
        ensures
            r == (self.flags() & PUSH_SCAN != 0),
    {
        self.push_flags & PUSH_SCAN != 0
    }

    /// Whether a freed block is cleared before it is cached or released
    /// (`zero_on_free`).
    #[inline(always)]
    pub fn zero_on_free(&self) -> (r: bool)
        ensures
            r == (self.flags() & PUSH_ZERO != 0),
    {
        self.push_flags & PUSH_ZERO != 0
    }

    /// A second arena is being built (`Arenas::build`).
    pub fn set_multi_arena(&self) {
        self.multi_arena.store(true, Ordering::Relaxed);
    }

    /// The cache GC period the periodic tick uses: `tcache_gc_interval_ms`,
    /// or 0 (GC every tick) while the cgroup policy is in its immediate
    /// band (design 11).
    #[inline]
    pub fn gc_interval(&self) -> u64 {
        if self.policy.immediate() { 0 } else { self.gc_interval_ns }
    }
    /// touch; see `Arenas::get`).
    #[inline]
    pub fn arena(&self, index: u16, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<&Arena>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_arenas()),
            self.wf(),
        ensures
            *final(ord) == *old(ord),
            r matches Some(a) ==> a.spec_index() == index,
    {
        if index == 0 {
            return Some(&self.arena0);
        }
        self.arenas.get(self, index, Tracked(ord))
    }

    /// Arena `index`, which must exist: the index came from a slab or run
    /// descriptor, an owner word or a cache header, so it was built before
    /// that memory was handed out. Never builds one: an index no arena was
    /// built for is a corrupt word (design 6.3 step 4), and aborts here
    /// instead of quietly materialising an automatic arena to release the
    /// slot through.
    #[inline]
    pub(super) fn owner(&self, index: u16) -> (r: &Arena)
        requires
            crate::sys::may_abort(),
        ensures
            self.wf() ==> r.spec_index() == index,
    {
        if index == 0 {
            return &self.arena0;
        }
        match self.arenas.get_existing(index) {
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

    /// The decay period new arenas start with (`dirty_decay_ms`).
    pub fn default_decay_ns(&self) -> u64 {
        crate::purge::decay_ns_from_ms(self.config.dirty_decay_ms)
    }

    /// Claim one slot of `class` in `arena` straight from the slab bitmaps,
    /// bypassing every cache. Null when the region is exhausted. The flag
    /// says the memory is known zero; `blk` gets the slot's token.
    pub fn alloc_direct(&self, arena: &Arena, class: usize, Tracked(blk): Tracked<&mut super::Live>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (*mut u8, bool, Tracked<Zeroed>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
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
        let (c, Tracked(mut z)) = cs.claim_tokens(&self.meta, &mut out, Tracked(&mut toks), Tracked(ord));
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
                blk.set(super::Live::from_slot(s, class as int, out[0]));
            }
            (out[0], c.zeroed, Tracked(t))
        }
    }

    /// Release one slot straight to its slab bitmap through its owning
    /// arena. Aborts unless `p` is an allocated slot of `class` in `arena`.
    /// `tok` is the slot's token; it goes back to the slot's bitmap word.
    pub fn free_direct(&self, arena: u16, class: usize, p: *mut u8, Tracked(tok): Tracked<GhostSingleton<int>>, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
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
        }
        self.owner(arena).class(class).release_with(&[p], Tracked(bag), Tracked(ord));
    }

    /// Classify `p` for `free`: `Ok((class, arena))` for an allocated slab
    /// slot (the bitmap check happens here), `Err(page)` for a page-aligned
    /// pointer into the huge region (the huge allocator checks it). Aborts
    /// on anything else.
    #[inline(always)]
    pub fn classify_free(&self, p: *mut u8) -> (r: Result<(usize, u16), usize>)
        requires
            crate::sys::may_abort(),
            self.wf(),
        ensures
            r matches Ok((c, _)) ==> c < NCLASSES && crate::layout::process_regions().is_slot(c as int, p@.addr),
            // The arena it names owns the slot's slab.
            r matches Ok((c, a)) ==> a == crate::class::slot_owner(c as int, p@.addr),
            r matches Err(page) ==> page <= self.huge_pages(),
            // Exact: a page start of the huge region, `page` its index.
            r matches Err(page) ==> crate::layout::process_regions().is_huge_block(p@.addr, 1)
                && page == ((p@.addr - crate::layout::process_regions().huge_base) as usize) >> crate::layout::process_regions().lg_page,
    {
        match self.regions.classify(p as usize) {
            Kind::Slab { class, slab, slot } => {
                let class = class as usize;
                let arena = self.table(class).check_located(slab as usize, slot as usize, p as usize);
                proof {
                    // `check_located` returned: not a guard slab.
                    self.regions.lemma_is_slot(class as int, slab as int, slot as int, p as usize);
                    // Its slab is the one `slot_owner` names.
                    self.regions.lemma_slab_index(class as int, slab as int, slot as int, p as usize);
                }
                Ok((class, arena))
            }
            Kind::Huge { page } => Err(page),
            Kind::Invalid => invalid_free(p as usize),
        }
    }

    /// The sized free's check (`sdallocx`, `free_sized`; design 6.3): `p`
    /// must be a slot start in a non-guard slab of the class the size hint
    /// names, `hint` (`class_for(size, align)`, `None` for a huge size).
    /// Arithmetic on the pointer and two dense lines: the hint names the
    /// region, so [`Regions::slot_in_class`] checks the pointer against
    /// that region alone (no superblock classification and no class
    /// compare), and the guard test is one
    /// 8-byte load from [`GuardTest`]'s dense array. Neither the slab
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
    pub(super) fn sized_slot(&self, p: *mut u8, hint: Option<usize>) -> (r: Option<usize>)
        requires
            self.wf(),
            hint matches Some(c) ==> c < NCLASSES,
        ensures
            r matches Some(c) ==> hint == Some(c) && crate::layout::process_regions().is_slot(c as int, p@.addr),
        opens_invariants none
    {
        let Some(class) = hint else { return None };
        let Some((slab, _slot)) = self.regions.slot_in_class(class, p as usize) else { return None };
        // SAFETY: `class_of`/`class_for` only produce `class < NCLASSES`
        // (`slot_in_class` relies on the same).
        if unsafe { crate::vspec::slice::array_get_unchecked(&self.guard_test, class) }.is_guard(slab) {
            return None;
        }
        proof {
            // The dense test is the table's (`Shared::wf`), so the slab is
            // not a guard slab.
            let c = class as int;
            self.regions.lemma_nslabs_bound(c);
            self.guard_test[c].lemma_is_guard(crate::layout::guard_geom(c).0, crate::layout::guard_geom(c).1, slab);
            self.regions.lemma_is_slot(c, slab as int, _slot as int, p as usize);
        }
        Some(class)
    }

    /// The `free` fast path's check: `Some(class)` when `p` is a slot start
    /// in a handed-out slab whose bit is set and whose arena is `home` (the
    /// arena load is skipped while only arena 0 exists), `None` for
    /// anything else. It never aborts; [`classify_free`] on the slow path
    /// repeats the checks and does. Every table read is bounds-free: the
    /// class index comes from `slab_slot`, the bitmap word from a slot
    /// already known to be below `nslots` in a slab below the watermark.
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
    #[inline(always)]
    pub(super) fn checked_slot(&self, p: *mut u8, h: &Heap) -> (r: Option<usize>)
        requires
            self.wf(),
        ensures
            r matches Some(c) ==> c < NCLASSES && crate::layout::process_regions().is_slot(c as int, p@.addr),
        opens_invariants [crate::bitmap::word_ns()]
    {
        let Some((class, table, slab)) = self.set_slot(p) else { return None };
        if self.multi_arena() {
            // The home arena is read on this branch only. A plain load is
            // hoisted above the flag test (the heap is known valid) and
            // then every single-arena `free` touches the heap's header
            // line, which nothing else on the fast paths reads; a volatile
            // read stays here.
            let home = h.arena_volatile();
            // SAFETY: `slab` is below the watermark.
            if unsafe { table.owner_unchecked(slab) } != home {
                return None;
            }
        }
        Some(class)
    }

    /// Allocate a huge run of `size` bytes aligned to `align` in `arena`:
    /// an automatic arena not built yet (a thread whose first allocation
    /// is huge) is built here, an index that does not exist aborts. Null
    /// on exhaustion; the flag says the memory is known zero.
    pub fn alloc_huge(&self, size: usize, align: usize, arena: u16, Tracked(blk): Tracked<&mut super::Live>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (*mut u8, bool, Tracked<Zeroed>))
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
    {
        proof {
            self.lemma_huge_aligned();
        }
        let Some(a) = self.arena(arena, Tracked(ord)) else { no_such_arena(arena) };
        let tracked mut tok = vstd::map::Map::<int, crate::huge::RunToken>::tracked_empty();
        let (r, z) = self.huge.alloc_run(&self.meta, size, align, arena, Tracked(&mut tok), Tracked(ord));
        proof {
            if r.ptr@.addr != 0 {
                self.lemma_run_is_block(r.ptr@.addr, size, align);
                let tracked t = tok.tracked_remove(0);
                self.lemma_run_token(t, r.ptr);
                // Exact stats: the pages `note_huge_alloc` adds are the token's.
                assert(t.npages() == r.pages);
                self.lemma_regions_ok();
                blk.set(super::Live::from_run(t, r.ptr));
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
    /// cleared bytes
    /// ([`theorem_zero_on_free_huge`](super::zero::theorem_zero_on_free_huge)).
    #[cold]
    #[inline(never)]
    pub fn zero_huge(&self, page: usize, Tracked(blk): Tracked<&mut super::Live>) -> (z: Tracked<Zeroed>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            self.wf(),
            page <= self.huge_pages(),
            old(blk).run_block(page as int),
        ensures
            self.huge_run_zero(z@, page),
            final(blk).run_block(page as int),
    {
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
        let ghost np = tok.npages();
        let run = self.huge.free_run(&self.meta, page, Tracked(tok), Tracked(ord));
        // Exact stats: the pages `note_huge_free` subtracts are the token's.
        proof {
            assert(run.pages == np);
        }
        self.owner(run.arena).note_huge_free(run.pages);
        run.usable
    }

    /// The sized free's abort: a slab pointer whose class is not the one
    /// the size hint names (`sdallocx`'s contract is `nallocx(size, flags)
    /// == sallocx(p)`). Huge runs and pointers that are not allocations pass
    /// here and are judged by the exact free that follows.
    #[cold]
    #[inline(never)]
    pub fn check_size_hint(&self, p: *mut u8, size: usize, hint: Option<usize>)
        requires
            crate::sys::may_abort(),
            self.wf(),
        ensures
            crate::heap::hint_ok(p, hint),
    {
        proof {
            self.lemma_wf();
            self.lemma_regions_ok();
        }
        if let Kind::Slab { class, .. } = self.regions.classify(p as usize) {
            if hint != Some(class as usize) {
                hint_mismatch(p, size, class);
            }
        }
    }

    /// Owning arena of the handed-out slab holding the slot `p` of `class`.
    /// Aborts for anything that is not a slot start in such a slab.
    pub(super) fn arena_of_slot(&self, class: usize, p: *mut u8) -> (r: u16)
        requires
            crate::sys::may_abort(),
            class < NCLASSES,
        ensures
            self.wf() ==> r == crate::class::slot_owner(class as int, p@.addr),
    {
        let t = self.table(class);
        let (slab, _slot) = t.locate(p);
        proof {
            if self.wf() {
                self.regions.lemma_slab_index(class as int, slab as int, _slot as int, p as usize);
            }
        }
        t.owner(slab)
    }

    /// The bit test the two `free` fast-path checks share: `Some((class,
    /// table, slab))` when `p` is a slot start in a handed-out slab of
    /// `class` whose bit is set (the table and slab are what
    /// [`checked_slot`]'s arena test then reads), `None` for anything
    /// else; never aborts.
    ///
    /// [`checked_slot`]: Shared::checked_slot
    #[inline(always)]
    fn set_slot(&self, p: *mut u8) -> (r: Option<(usize, &SlabTable, usize)>)
        requires
            self.wf(),
        ensures
            r matches Some((c, _, _)) ==> c < NCLASSES && crate::layout::process_regions().is_slot(c as int, p@.addr),
            r matches Some((_, t, s)) ==> s < t.spec_nslabs(),
        opens_invariants [crate::bitmap::word_ns()]
    {
        let Some((class, slab, slot)) = self.regions.slab_slot(p as usize) else { return None };
        // SAFETY: `slab_slot` only returns `class < NCLASSES`.
        let table = unsafe { crate::vspec::slice::array_get_unchecked(&self.tables, class) };
        if !table.is_handed_out(slab) {
            return None;
        }
        // SAFETY: `slab` is below the watermark, `slab_slot` validated
        // `slot < nslots`.
        if !unsafe { table.is_set_unchecked(slab, slot) } {
            return None;
        }
        proof {
            // A set bit is not in a guard slab.
            self.regions.lemma_is_slot(class as int, slab as int, slot as int, p as usize);
        }
        Some((class, table, slab))
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
    /// when this returns, `bag` has lost exactly the batch's tokens
    /// (whole-allocator theorem 10).
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
            crate::sys::may_abort(),
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
                crate::sys::may_abort(),
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
            }
            done += self.owner(a).class(class).release_owned(&ptrs[done..], Tracked(&mut *bag), Tracked(ord));
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

impl Shared {
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

    /// [`checked_slot`] without the arena test: the bit test alone, so a
    /// test can pin down what [`checked_slot`]'s two halves each decide.
    ///
    /// [`checked_slot`]: Shared::checked_slot
    #[cfg(test)]
    pub(super) fn checked_slot_any(&self, p: *mut u8) -> Option<usize> {
        self.set_slot(p).map(|(class, _, _)| class)
    }
}

verus! {

#[cold]
#[inline(never)]
fn no_such_arena(index: u16) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!("arena {index} does not exist"))
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
    ));
}

} // verus!

/// Kani checks of `axiom_shared_layout`, the offsets it names and
/// `arena::axiom_array_layout` at the two arrays `Shared` splits
/// (records under `verus/seams/`).
#[cfg(kani)]
mod kani_layout {
    use super::*;
    use crate::arena::kani_layout::array_layout;
    use core::mem::{align_of, offset_of, size_of};

    /// `field_fits::<S, T>(off, next)` with its `int` sum in `u128`.
    fn field_fits<S, T>(off: usize, next: usize) -> bool {
        off % align_of::<T>() == 0
            && align_of::<S>() % align_of::<T>() == 0
            && off as u128 + size_of::<T>() as u128 <= next as u128
    }

    /// S143..S158, S159: each `S_*` is its field's `offset_of!`, and the
    /// fields fit as `axiom_shared_layout` says.
    #[kani::proof]
    fn shared_layout() {
        assert_eq!(S_MULTI_ARENA, offset_of!(Shared, multi_arena));
        assert_eq!(S_PUSH_FLAGS, offset_of!(Shared, push_flags));
        assert_eq!(S_REGIONS, offset_of!(Shared, regions));
        assert_eq!(S_GUARD_TEST, offset_of!(Shared, guard_test));
        assert_eq!(S_TABLES, offset_of!(Shared, tables));
        assert_eq!(S_ARENA0, offset_of!(Shared, arena0));
        assert_eq!(S_META, offset_of!(Shared, meta));
        assert_eq!(S_HUGE, offset_of!(Shared, huge));
        assert_eq!(S_GC_INTERVAL_NS, offset_of!(Shared, gc_interval_ns));
        assert_eq!(S_POLICY, offset_of!(Shared, policy));
        assert_eq!(S_CONFIG, offset_of!(Shared, config));
        assert_eq!(S_KEY, offset_of!(Shared, key));
        assert_eq!(S_ARENAS, offset_of!(Shared, arenas));
        assert_eq!(S_TCACHES, offset_of!(Shared, tcaches));
        assert_eq!(S_STATS, offset_of!(Shared, stats));
        assert_eq!(S_CGROUP, offset_of!(Shared, cgroup));
        assert!(field_fits::<Shared, AtomicBool>(
            S_MULTI_ARENA,
            S_PUSH_FLAGS
        ));
        assert!(field_fits::<Shared, u8>(S_PUSH_FLAGS, S_REGIONS));
        assert!(field_fits::<Shared, Regions>(S_REGIONS, S_GUARD_TEST));
        assert!(field_fits::<Shared, [GuardTest; NCLASSES]>(
            S_GUARD_TEST,
            S_TABLES
        ));
        assert!(field_fits::<Shared, [SlabTable; NCLASSES]>(
            S_TABLES, S_ARENA0
        ));
        assert!(NCLASSES as u128 * size_of::<GuardTest>() as u128 <= isize::MAX as u128);
        assert!(NCLASSES as u128 * size_of::<SlabTable>() as u128 <= isize::MAX as u128);
        assert!(field_fits::<Shared, Arena>(S_ARENA0, S_META));
        assert!(field_fits::<Shared, Meta>(S_META, S_HUGE));
        assert!(field_fits::<Shared, Huge>(S_HUGE, S_GC_INTERVAL_NS));
        assert!(field_fits::<Shared, u64>(S_GC_INTERVAL_NS, S_POLICY));
        assert!(field_fits::<Shared, Policy>(S_POLICY, S_CONFIG));
        assert!(field_fits::<Shared, Config>(S_CONFIG, S_KEY));
        assert!(field_fits::<Shared, ThreadKey>(S_KEY, S_ARENAS));
        assert!(field_fits::<Shared, Arenas>(S_ARENAS, S_TCACHES));
        assert!(field_fits::<Shared, Tcaches>(S_TCACHES, S_STATS));
        assert!(field_fits::<Shared, Lock<Snapshot>>(S_STATS, S_CGROUP));
        assert!(field_fits::<Shared, Cgroup>(S_CGROUP, size_of::<Shared>()));
        assert!(size_of::<Shared>() <= isize::MAX as usize);
    }

    /// S49 `arena::axiom_array_layout` at the two arrays of `Shared`.
    #[kani::proof]
    fn shared_array_layout() {
        array_layout::<GuardTest, NCLASSES>();
        array_layout::<SlabTable, NCLASSES>();
    }
}
