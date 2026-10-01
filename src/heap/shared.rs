//! [`Shared`], the process-wide allocator state every heap works against,
//! and the checks it performs for the fast and slow paths; the thread-exit
//! key and the thread id, clock and seed a heap reads, and the sized fast
//! path's [`GuardTest`].

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arena::{Arena, Arenas};
use crate::class::SlabTable;
use crate::config::Config;
use crate::huge::Huge;
#[cfg(verus_only)]
use crate::layout::class_table;
use crate::layout::{CLASSES, Kind, NCLASSES, Regions};
use crate::lock::Lock;
use crate::meta::Meta;
use crate::vspec::unbacked::Unbacked;
use crate::vspec::zero::Zeroed;

use super::heap_destructor;
use super::tcaches::Tcaches;
use super::{Heap, PUSH_PAC, PUSH_SCAN, PUSH_ZERO, invalid_free};
#[cfg(verus_only)]
use crate::class::slot_loc;
#[cfg(verus_only)]
use crate::class::{aset, distinct, ptr_addrs};
use vstd::prelude::verus;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, int};
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
    /// Whether `slab` is a guard slab of the class this pair describes.
    #[inline(always)]
    pub(super) fn is_guard(self, slab: usize) -> bool {
        (slab as u32).wrapping_add(self.add) & self.mask == 0
    }

    pub(super) fn of(table: &SlabTable) -> GuardTest {
        let lg_spp = table.slabs_per_page().trailing_zeros();
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

impl ThreadKey {
    pub fn create() -> ThreadKey {
        let mut key: libc::pthread_key_t = 0;
        // SAFETY: `key` is a valid out-pointer; the destructor is a plain
        // `extern "C"` function that only touches allocator state.
        let rc = unsafe { libc::pthread_key_create(&mut key, Some(heap_destructor)) };
        ThreadKey {
            key,
            valid: rc == 0,
        }
    }

    #[cfg(test)]
    pub fn valid(&self) -> bool {
        self.valid
    }

    #[cfg(test)]
    pub fn key(&self) -> Option<libc::pthread_key_t> {
        if self.valid { Some(self.key) } else { None }
    }

    /// Register `heap` for destruction when the calling thread exits.
    pub fn set(&self, heap: *mut Heap) {
        if !self.valid {
            return;
        }
        // SAFETY: the key was created by `create`; glibc stores the
        // pointer without dereferencing it.
        if unsafe { libc::pthread_setspecific(self.key, heap as *const c_void) } != 0 {
            panic!("heap: pthread_setspecific failed");
        }
    }
}

verus! {

pub(super) fn now_ns() -> u64 {
    crate::sys::now_coarse_ns()
}

pub(super) fn tid() -> u32 {
    crate::sys::gettid() as u32
}

pub(super) fn ncpus() -> u32 {
    crate::sys::ncpus()
}

} // verus!

/// Eight random bytes from `getrandom(2)` for `slot_randomize`; on a
/// kernel that refuses (not seen on any supported one) the clock, so
/// the heap is still seeded with something that varies.
pub(super) fn seed() -> u64 {
    let mut b = [0u8; 8];
    match crate::sys::getrandom_exact(&mut b) {
        Ok(()) => u64::from_ne_bytes(b),
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

/// The address of the process's `Shared` storage (`api`'s `SHARED`).
pub uninterp spec fn shared_storage() -> *mut Shared;

/// The `Shared` built in place at `p`: [`Shared::init`] writes each field
/// once, before anything reads it, and nothing writes the fields
/// [`Shared::wf`] reads afterwards, so the value is one value.
/// Uninterpreted; the in-place writers below fix the fields `wf` names.
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

/// Ghost record of an in-place build of the `Shared` at [`Build::at`]: the
/// fields [`Shared::wf`] names that have been written ([`F_REGIONS`] for
/// `regions` and `meta`, and the rest; table `c` is `F_TABLE + c`), so
/// each is written once.
/// Trusted: only [`Shared::begin`] makes one, and it must be called once
/// per storage (its one caller, `Shared::init`, builds each storage once);
/// two tokens for one storage could write a field twice.
#[verifier::external_body]
pub tracked struct Build {
    _p: (),
}

pub spec const F_REGIONS: int = 0;
pub spec const F_FLAGS: int = 1;
pub spec const F_HUGE: int = 2;
pub spec const F_CONFIG: int = 3;
pub spec const F_TABLE: int = 100;

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

impl Build {
    pub uninterp spec fn at(&self) -> *mut Shared;

    pub uninterp spec fn done(&self) -> vstd::set::Set<int>;

    /// Every table is written.
    pub open spec fn tables_done(&self) -> bool {
        forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.done().contains(F_TABLE + c)
    }
}

/// The in-place writes and reads of [`Shared::init`], each trusted: Verus
/// has no raw-pointer field projection. Each writes or reads the fields it
/// names and nothing else, in `init`'s order, and `this` must be the
/// storage `init` was handed; a writer's postcondition is the field of
/// [`shared_at`] it fixes.
impl Shared {
    /// A build of fresh storage at `this`: nothing written yet.
    #[allow(unused_variables)]
    #[verifier::external_body]
    #[inline(always)]
    fn begin(this: *mut Shared) -> (b: Tracked<Build>)
        ensures
            b@.at() == this,
            b@.done() == vstd::set::Set::<int>::empty(),
    {
        Tracked::assume_new()
    }

    /// `meta`, `regions` and `multi_arena` (false); the written metadata
    /// allocator.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_head<'a>(this: *mut Shared, meta: Meta, regions: Regions, Tracked(b): Tracked<&mut Build>) -> (r: &'a Meta)
        requires
            old(b).at() == this,
            !old(b).done().contains(F_REGIONS),
        ensures
            final(b).at() == this,
            final(b).done() == old(b).done().insert(F_REGIONS),
            shared_at(this).regions == regions,
            shared_at(this).meta == meta,
            *r == meta,
    {
        // SAFETY: `init`'s caller guarantees `this` is writable and
        // unaliased; the fields are never moved.
        unsafe {
            let meta_ptr = &raw mut (*this).meta;
            ptr::write(meta_ptr, meta);
            ptr::write(&raw mut (*this).regions, regions);
            ptr::write(&raw mut (*this).multi_arena, AtomicBool::new(false));
            &*meta_ptr
        }
    }

    /// `push_flags`; the written regions.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_flags<'a>(this: *mut Shared, flags: u8, Tracked(b): Tracked<&mut Build>) -> (r: &'a Regions)
        requires
            old(b).at() == this,
            old(b).done().contains(F_REGIONS),
            !old(b).done().contains(F_FLAGS),
        ensures
            final(b).at() == this,
            final(b).done() == old(b).done().insert(F_FLAGS),
            shared_at(this).push_flags == flags,
            *r == shared_at(this).regions,
    {
        // SAFETY: as in `put_head`; `regions` was written by `put_head`.
        unsafe {
            ptr::write(&raw mut (*this).push_flags, flags);
            &(*this).regions
        }
    }

    /// `tables[c]`.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_table(this: *mut Shared, c: usize, t: SlabTable, Tracked(b): Tracked<&mut Build>)
        requires
            old(b).at() == this,
            c < NCLASSES,
            !old(b).done().contains(F_TABLE + c),
        ensures
            final(b).at() == this,
            final(b).done() == old(b).done().insert(F_TABLE + c),
            shared_at(this).tables[c as int] == t,
    {
        // SAFETY: as in `put_head`.
        unsafe { ptr::write(&raw mut (*this).tables[c], t) }
    }

    /// Every table, written by [`Shared::put_table`].
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn tables_of<'a>(this: *mut Shared, Tracked(b): Tracked<&Build>) -> (r: &'a [SlabTable; NCLASSES])
        requires
            b.at() == this,
            b.tables_done(),
        ensures
            *r == shared_at(this).tables,
    {
        // SAFETY: written by `put_table`; never moved.
        unsafe { &(*this).tables }
    }

    /// `guard_test[c]`.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_guard_test(this: *mut Shared, c: usize, g: GuardTest) {
        // SAFETY: as in `put_head`.
        unsafe { ptr::write(&raw mut (*this).guard_test[c], g) }
    }

    /// The storage of arena 0.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn arena0_ptr(this: *mut Shared) -> *mut Arena {
        // SAFETY: a projection inside `init`'s storage; nothing is read.
        unsafe { &raw mut (*this).arena0 }
    }

    /// `huge`.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_huge(this: *mut Shared, h: Huge, Tracked(b): Tracked<&mut Build>)
        requires
            old(b).at() == this,
            !old(b).done().contains(F_HUGE),
        ensures
            final(b).at() == this,
            final(b).done() == old(b).done().insert(F_HUGE),
            shared_at(this).huge == h,
    {
        // SAFETY: as in `put_head`.
        unsafe { ptr::write(&raw mut (*this).huge, h) }
    }

    /// `gc_interval_ns` and the two registries (`Arenas::init` with
    /// `arena0`, `Tcaches::init`).
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_registries(this: *mut Shared, gc_interval_ns: u64, arena0: *mut Arena, narenas: u32) {
        // SAFETY: as in `put_head`; `arena0` is inside the same `Shared`.
        unsafe {
            ptr::write(&raw mut (*this).gc_interval_ns, gc_interval_ns);
            Arenas::init(&raw mut (*this).arenas, arena0, narenas);
            Tcaches::init(&raw mut (*this).tcaches);
        }
    }

    /// `cgroup`; the written record.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_cgroup<'a>(this: *mut Shared, cgroup: crate::sys::cgroup::Cgroup) -> &'a crate::sys::cgroup::Cgroup {
        // SAFETY: as in `put_head`; never moved.
        unsafe {
            ptr::write(&raw mut (*this).cgroup, cgroup);
            &(*this).cgroup
        }
    }

    /// `policy`; the written policy.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_policy<'a>(this: *mut Shared, policy: crate::policy::Policy) -> &'a crate::policy::Policy {
        // SAFETY: as in `put_head`; never moved.
        unsafe {
            ptr::write(&raw mut (*this).policy, policy);
            &(*this).policy
        }
    }

    /// `config`, then the thread-exit key (`ThreadKey::create`) and the
    /// empty statistics.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_tail(this: *mut Shared, config: Config, Tracked(b): Tracked<&mut Build>)
        requires
            old(b).at() == this,
            !old(b).done().contains(F_CONFIG),
        ensures
            final(b).at() == this,
            final(b).done() == old(b).done().insert(F_CONFIG),
            shared_at(this).config == config,
    {
        // SAFETY: as in `put_head`.
        unsafe {
            ptr::write(&raw mut (*this).config, config);
            ptr::write(&raw mut (*this).key, ThreadKey::create());
            ptr::write(
                &raw mut (*this).stats,
                Lock::new(crate::stats::Snapshot::EMPTY),
            );
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
    /// [`Shared::wf`]; verified apart from the in-place writes above.
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
    pub unsafe fn init(
        this: *mut Shared,
        regions: Regions,
        space: Tracked<Unbacked>,
        config: Config,
        guard_install: bool,
        pac: bool,
    ) -> (r: bool)
        requires
            this == shared_storage(),
            regions.wf(),
            regions.separated(),
            regions.ready(),
            space@.addr() == regions.meta_base,
            space@.len() == regions.meta_len,
            config.wf(),
        ensures
            r ==> shared_at(this).wf(),
    {
        let mut config = config;
        config.narenas = config.effective_narenas(ncpus());
        let guard_interval = if guard_install {
            config.effective_guard_slab_interval()
        } else {
            0
        };
        let guard_install = guard_install && config.guards;
        let decay_ns = crate::purge::decay_ns_from_ms(config.dirty_decay_ms);
        let Tracked(mut b) = Shared::begin(this);
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
        // SAFETY: the caller guarantees `this` is writable and unaliased.
        unsafe {
            let meta = Shared::put_head(
                this,
                Meta::owning(regions.meta_base, regions.meta_len, regions.page_size(), space),
                regions,
                Tracked(&mut b),
            );
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
                Tracked(&mut b),
            );
            for c in 0..NCLASSES
                invariant
                    b.at() == this,
                    *regions == shared_at(this).regions,
                    regions.wf(),
                    regions.separated(),
                    regions.ready(),
                    *regions == crate::layout::process_regions(),
                    guard_interval == 0 || (2 <= guard_interval <= 0x40_0000 && crate::meta::is_pow2(
                        guard_interval as usize,
                    )),
                    b.done().contains(F_REGIONS),
                    b.done().contains(F_FLAGS),
                    !b.done().contains(F_HUGE),
                    !b.done().contains(F_CONFIG),
                    forall|d: int| 0 <= d < NCLASSES ==> (#[trigger] b.done().contains(F_TABLE + d)) == (d < c),
                    forall|d: int| 0 <= d < c ==> (#[trigger] shared_at(this).tables[d]).spec_class() == d,
                    meta.is_process(),
                    forall|d: int| 0 <= d < c ==> (#[trigger] shared_at(this).tables[d]).meta_ok(),
            {
                match SlabTable::new(
                    meta,
                    regions,
                    c,
                    guard_interval_for(c, regions.page_size(), guard_interval),
                ) {
                    Some(t) => Shared::put_table(this, c, t, Tracked(&mut b)),
                    None => return false,
                }
            }
            let tables = Shared::tables_of(this, Tracked(&b));
            for c in 0..NCLASSES {
                Shared::put_guard_test(this, c, GuardTest::of(&tables[c]));
            }
            let arena0 = Shared::arena0_ptr(this);
            if !Arena::init(arena0, meta, regions, tables, 0, decay_ns) {
                return false;
            }
            match Huge::new(meta, regions, guard_install) {
                Some(h) => {
                    h.set_retain_max(config.huge_retain_max);
                    // `huge_dirty_decay_ms:0`: a freed run is purged at
                    // once rather than at the next decay pass (design 7).
                    h.set_purge_on_free(config.huge_dirty_decay_ms == Some(0));
                    Shared::put_huge(this, h, Tracked(&mut b))
                }
                None => return false,
            }
            Shared::put_registries(this, GC_INTERVAL_NS, arena0, config.narenas);
            let cgroup = Shared::put_cgroup(this, crate::sys::cgroup::Cgroup::discover(config.cgroup));
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
            Shared::put_tail(this, config, Tracked(&mut b));
        }
        proof {
            let s = shared_at(this);
            assert(s.huge_pages() == s.regions.huge_len >> s.regions.lg_page);
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
    /// `tables[c]` is class `c`'s table; `huge` covers exactly the huge
    /// region's pages, starting at the huge base with the regions' page;
    /// the [`PUSH_PAC`] bit is [`super::pac_mode`]; `meta` allocates from
    /// the metadata region, which holds every class table's descriptors,
    /// bitmap and owner words.
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
        &&& self.config.wf()
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.tables[c].spec_class() == c
        &&& self.huge.spec_npages() == self.huge_pages()
        &&& self.meta.spec_base() == self.regions.meta_base
        &&& self.meta.spec_len() == self.regions.meta_len
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.tables[c].meta_ok()
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

    /// The usable bytes of a huge run for `size` ([`Huge::spec_usable`]).
    pub closed spec fn huge_usable(&self, size: usize) -> int {
        self.huge.spec_usable(size)
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
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.tables[c].spec_class() == c,
            self.huge.spec_npages() == self.huge_pages(),
            self.config.wf(),
            self.huge_pages() == self.regions.huge_len >> self.regions.lg_page,
            self.meta.is_process(),
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.tables[c].meta_ok(),
    {
    }

    /// The per-class slab tables (shared by every arena).
    #[inline(always)]
    pub fn tables(&self) -> (r: &[SlabTable; NCLASSES])
        ensures
            self.wf() ==> forall|c: int| 0 <= c < NCLASSES ==> #[trigger] r[c].spec_class() == c,
    {
        &self.tables
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
    pub fn multi_arena(&self) -> bool {
        self.multi_arena.load(Ordering::Relaxed)
    }

    /// The flag byte both fast paths load: [`PUSH_SCAN`], [`PUSH_ZERO`]
    /// and [`PUSH_PAC`]; 0 in the default configuration on a CPU without
    /// pointer authentication.
    #[inline(always)]
    pub fn push_flags(&self) -> (r: u8)
        ensures
            r == self.flags(),
            self.wf() ==> ((r & PUSH_PAC != 0) == super::pac_mode()),
    {
        self.push_flags
    }

    /// Whether cache and heap pointers are signed ([`PUSH_PAC`]).
    #[inline(always)]
    pub fn pac(&self) -> (r: bool)
        ensures
            r == (self.flags() & PUSH_PAC != 0),
            self.wf() ==> r == super::pac_mode(),
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
    pub fn arena(&self, index: u16) -> Option<&Arena>
        requires
            self.wf(),
    {
        if index == 0 {
            return Some(&self.arena0);
        }
        self.arenas.get(self, index)
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
    /// says the memory is known zero.
    pub fn alloc_direct(&self, arena: &Arena, class: usize) -> (r: (*mut u8, bool, Tracked<Zeroed>))
        requires
            class < NCLASSES,
        ensures
            r.0@.addr != 0 ==> crate::layout::process_regions().is_slot(class as int, r.0@.addr),
            r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, class_table()[class as int].size as int),
    {
        let mut out = [ptr::null_mut()];
        let (c, Tracked(mut z)) = arena.class(class).claim_zero(&self.meta, &mut out);
        if c.n == 0 {
            (ptr::null_mut(), false, Tracked(Zeroed::empty(0)))
        } else {
            let tracked t = if c.zeroed { z.tracked_remove(0) } else { Zeroed::empty(0) };
            (out[0], c.zeroed, Tracked(t))
        }
    }

    /// Release one slot straight to its slab bitmap through its owning
    /// arena. Aborts unless `p` is an allocated slot of `class` in `arena`.
    /// `tok` is the slot's token; it goes back to the slot's bitmap word.
    pub fn free_direct(&self, arena: u16, class: usize, p: *mut u8, Tracked(tok): Tracked<GhostSingleton<int>>)
        requires
            class < NCLASSES,
            crate::layout::process_regions().is_slot(class as int, p@.addr),
            tok.id() == slot_loc(class as int),
            tok@ == p@.addr as int,
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
        self.owner(arena).class(class).release_with(&[p], Tracked(bag));
    }

    /// Classify `p` for `free`: `Ok((class, arena))` for an allocated slab
    /// slot (the bitmap check happens here), `Err(page)` for a page-aligned
    /// pointer into the huge region (the huge allocator checks it). Aborts
    /// on anything else.
    #[inline(always)]
    pub fn classify_free(&self, p: *mut u8) -> (r: Result<(usize, u16), usize>)
        requires
            self.wf(),
        ensures
            r matches Ok((c, _)) ==> c < NCLASSES && crate::layout::process_regions().is_slot(c as int, p@.addr),
            r matches Err(page) ==> page <= self.huge_pages(),
            // Exact: a page start of the huge region, `page` its index.
            r matches Err(page) ==> crate::layout::process_regions().is_huge_block(p@.addr, 1)
                && page == ((p@.addr - crate::layout::process_regions().huge_base) as usize) >> crate::layout::process_regions().lg_page,
    {
        match self.regions.classify(p as usize) {
            Kind::Slab { class, slab, slot } => {
                let class = class as usize;
                let arena = self.table(class).check_located(slab as usize, slot as usize, p as usize);
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
    {
        let class = hint?;
        let (slab, _slot) = self.regions.slot_in_class(class, p as usize)?;
        // SAFETY: `class_of`/`class_for` only produce `class < NCLASSES`
        // (`slot_in_class` relies on the same).
        if unsafe { self.guard_test.get_unchecked(class) }.is_guard(slab) {
            return None;
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
    {
        let (class, table, slab) = self.set_slot(p)?;
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
    pub fn alloc_huge(&self, size: usize, align: usize, arena: u16) -> (r: (*mut u8, bool, Tracked<Zeroed>))
        requires
            self.wf(),
            crate::meta::is_pow2(align),
        ensures
            r.0@.addr != 0 ==> crate::layout::huge_block(r.0@.addr, size, align),
            r.0@.addr != 0 ==> size <= self.huge_usable(size),
            r.0@.addr != 0 ==> self.huge_ok(size),
            r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, self.huge_usable(size)),
    {
        proof {
            self.lemma_huge_aligned();
        }
        let Some(a) = self.arena(arena) else { no_such_arena(arena) };
        let (r, z) = self.huge.alloc_run(&self.meta, size, align, arena);
        proof {
            if r.ptr@.addr != 0 {
                self.lemma_run_is_block(r.ptr@.addr, size, align);
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
    /// the whole run.
    #[cold]
    #[inline(never)]
    pub fn zero_huge(&self, page: usize)
        requires
            self.wf(),
            page <= self.huge_pages(),
    {
        let n = self.huge.usable_size(page);
        proof {
            broadcast use vstd::layout::align_of_u8;
            crate::vspec::addr::axiom_huge_writable(self.regions, page, n);
        }
        // SAFETY: a live run's usable bytes are committed, writable memory
        // this allocator handed out and the caller is freeing.
        unsafe { ptr::write_bytes(crate::vspec::addr::addr_to_ptr(self.regions.huge_addr(page)), 0, n) };
    }

    /// Free the huge run starting at `page`. Aborts unless it is a live
    /// run start. Returns the usable bytes released.
    pub fn free_huge(&self, page: usize) -> usize
        requires
            self.wf(),
            page <= self.huge_pages(),
    {
        let run = self.huge.free_run(&self.meta, page);
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
            self.wf(),
    {
        proof {
            self.lemma_wf();
        }
        if let Kind::Slab { class, .. } = self.regions.classify(p as usize) {
            if hint != Some(class as usize) {
                hint_mismatch(p, size, class);
            }
        }
    }

    /// Owning arena of the handed-out slab holding the slot `p` of `class`.
    /// Aborts for anything that is not a slot start in such a slab.
    pub(super) fn arena_of_slot(&self, class: usize, p: *mut u8) -> u16
        requires
            class < NCLASSES,
    {
        let t = self.table(class);
        let (slab, _) = t.locate(p);
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
    {
        let (class, slab, slot) = self.regions.slab_slot(p as usize)?;
        // SAFETY: `slab_slot` only returns `class < NCLASSES`.
        let table = unsafe { self.tables.get_unchecked(class) };
        if !table.is_handed_out(slab) {
            return None;
        }
        // SAFETY: `slab` is below the watermark, `slab_slot` validated
        // `slot < nslots`.
        if !unsafe { table.is_set_unchecked(slab, slot) } {
            return None;
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
    /// belong back with the bitmap words each run releases.
    ///
    /// The loop ends because each pass releases at least one pointer or
    /// moves to the arena the next pointer's owner word names, which
    /// `release_owned` then owns; that rests on the owner words of slabs
    /// with allocated slots not changing, which Verus does not see
    /// (`exec_allows_no_decreases_clause`).
    ///
    /// [`release_owned`]: crate::class::ClassState::release_owned
    #[verifier::exec_allows_no_decreases_clause]
    pub fn release_routed(&self, arena: u16, class: usize, ptrs: &[*mut u8], Tracked(bag): Tracked<GhostSubset<int>>)
        requires
            class < NCLASSES,
            bag.id() == slot_loc(class as int),
            aset(ptr_addrs(ptrs@), 0, ptrs@.len() as int) <= bag@,
            distinct(ptr_addrs(ptrs@), 0, ptrs@.len() as int),
    {
        broadcast use crate::class::lemma_aset;
        let tracked mut bag = bag;
        let ghost pa = ptr_addrs(ptrs@);
        let mut done = 0;
        let mut a = arena;
        loop
            invariant
                class < NCLASSES,
                done <= ptrs@.len(),
                pa == ptr_addrs(ptrs@),
                bag.id() == slot_loc(class as int),
                aset(pa, done as int, ptrs@.len() as int) <= bag@,
                distinct(pa, 0, ptrs@.len() as int),
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
            done += self.owner(a).class(class).release_owned(&ptrs[done..], Tracked(&mut bag));
            proof {
                let r = done - d0;
                crate::class::lemma_aset_split(pa, d0, done as int, ptrs@.len() as int);
                crate::class::lemma_aset_split(pa, 0, d0, ptrs@.len() as int);
                crate::class::lemma_aset_shift(pa, ra, d0, 0, r);
                assert(aset(pa, done as int, ptrs@.len() as int) <= bag@);
            }
            if done == ptrs.len() {
                return;
            }
            let next = self.arena_of_slot(class, ptrs[done]);
            if next == a {
                // `release_owned` stopped at a slab it says is not ours while
                // the descriptor says it is: the descriptor changed under us.
                #[cfg(verus_only)]
                arena_changed(class, a);
                #[cfg(not(verus_only))]
                panic!("heap: slab of class {class} changed arena {a} during a flush");
            }
            a = next;
        }
    }
}

} // verus!

impl Shared {
    /// Change the decay period of every existing arena; `-1` turns purging
    /// off. Takes effect at the next periodic tick, or at once for a
    /// background thread (each arena setter wakes it).
    /// `arena.<i>.dirty_decay_ms` goes through `Arena::set_decay_ms` instead.
    pub fn set_decay_ms(&self, ms: i64) {
        for a in self.arenas.iter() {
            a.set_decay_ms(ms);
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

/// Trusted: aborts.
#[cold]
#[inline(never)]
#[verifier::external_body]
fn no_such_arena(index: u16) -> ! {
    panic!("arena {index} does not exist")
}

/// [`Shared::check_size_hint`]'s abort: the size hint names a class other
/// than slot `p`'s `class`. Trusted: aborts.
#[cold]
#[inline(never)]
#[verifier::external_body]
fn hint_mismatch(p: *mut u8, size: usize, class: u32) -> ! {
    panic!(
        "sized free of {:#x}: size hint {} does not match the allocation (class {}, {} bytes)",
        p as usize,
        size,
        class,
        CLASSES[class as usize].size()
    );
}

} // verus!

verus! {

/// What Verus sees for [`Shared::release_routed`]'s `panic!` on a slab
/// whose arena changed during a flush; the build keeps the `panic!`
/// inline, so `release_routed` keeps its code. Trusted: the `-> !` is the
/// whole specification, and the `panic!` it stands for never returns; the
/// two copies of the message must be kept in step.
#[cfg(verus_only)]
#[verifier::external_body]
fn arena_changed(class: usize, a: u16) -> ! {
    panic!("heap: slab of class {class} changed arena {a} during a flush")
}

} // verus!
