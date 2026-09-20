//! [`Shared`], the process-wide allocator state every heap works against,
//! and the checks it performs for the fast and slow paths; the thread-exit
//! key and the OS shims (`os`), and the sized fast path's [`GuardTest`].

use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arena::{Arena, Arenas, Tcaches};
use crate::class::SlabTable;
use crate::config::Config;
use crate::huge::{GUARD_PAGES, Huge};
use crate::layout::{CLASSES, Kind, NCLASSES, Regions};
use crate::lock::Lock;
use crate::meta::Meta;

#[cfg(target_os = "linux")]
use super::heap_destructor;
use super::{Heap, PUSH_PAC, PUSH_PERCPU, PUSH_SCAN, PUSH_ZERO, Percpu, invalid_free};

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
    /// low `class_base` entries): after `Regions` it sat on its own line,
    /// and larson's working set turns every extra line into an L2 hit
    /// (`bench/results/2026-09-16-aarch64-arenas-lines.md`).
    pub(super) multi_arena: AtomicBool,
    /// The opt-in hardening a push applies, as [`PUSH_SCAN`]
    /// (`double_free_scan`, design 6.3 step 3: every push compares the
    /// slot against the top four cache entries and aborts on a match) and
    /// [`PUSH_ZERO`] (`zero_on_free`: the slot's class-size bytes are
    /// cleared before the push, and a huge run's usable bytes before its
    /// release), [`PUSH_PAC`] (signed cache and heap pointers where the
    /// CPU has pointer authentication; read by the pop too) and
    /// [`PUSH_PERCPU`] (the per-CPU mode). Written once by `init`; kept on
    /// the line every `free` reads, and zero in the default configuration
    /// on a CPU without PAC, so the fast path tests one byte and takes one
    /// predicted branch.
    pub(super) push_flags: u8,
    pub regions: Regions,
    /// The per-CPU mode's table and state (design 15; [`PUSH_PERCPU`]).
    /// After `regions` so the thread paths' first line is as it was; the
    /// per-CPU paths read `base`, `n` and the flag byte, two lines.
    pub percpu: Percpu,
    /// The sized fast path's guard test per class: a dense array (8 bytes
    /// per class, seven lines for all 52) so `sized_slot` never touches
    /// the 384-byte class table line (it reads neither the watermark nor
    /// the descriptor; both checks are the flush's).
    pub(super) guard_test: [GuardTest; NCLASSES],
    tables: [SlabTable; NCLASSES],
    pub arena0: Arena,
    pub meta: Meta,
    pub huge: Huge,
    /// Cache GC period (`tcache_gc_interval_ms`).
    pub gc_interval_ns: u64,
    /// The cgroup policy's outcome (design 11, `policy.rs`): the band,
    /// the keep ratio, the pending full purge. Next to `gc_interval_ns`
    /// so the periodic tick's `immediate` load hits the line it reads
    /// anyway; written at most once per 10 ms by the sampler.
    pub policy: crate::policy::Policy,
    /// The parsed `malloc_conf` (design section 12) as `init` applied it
    /// (`narenas` resolved from the CPU count, `thp` forced to `never`
    /// under a small cgroup limit). Read back by `opt.*` and the report,
    /// and on the slow paths for the cache ladder (`cache_cap`) and the
    /// compaction mask (`compact_mask`).
    pub config: Config,
    pub(super) key: ThreadKey,
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

impl GuardTest {
    pub(super) fn of(table: &SlabTable) -> GuardTest {
        let lg_spp = table.slabs_per_page().trailing_zeros();
        GuardTest { add: table.slabs_per_page() as u32, mask: table.guard_mask().wrapping_shl(lg_spp) }
    }

    /// Whether `slab` is a guard slab of the class this pair describes.
    #[inline(always)]
    pub(super) fn is_guard(self, slab: usize) -> bool {
        (slab as u32).wrapping_add(self.add) & self.mask == 0
    }
}

const _: () = assert!(
    core::mem::offset_of!(Shared, multi_arena) / 128
        == (core::mem::offset_of!(Shared, regions) + core::mem::offset_of!(Regions, superblock)) / 128
);
const _: () = assert!(
    core::mem::offset_of!(Shared, multi_arena) / 128
        == (core::mem::offset_of!(Shared, regions) + core::mem::offset_of!(Regions, lg_stride)) / 128
);

#[cfg(target_os = "linux")]
pub(super) mod os {
    use super::Heap;
    use core::ffi::c_void;

    /// The `pthread_key` whose destructor tears down a thread's heap.
    pub struct ThreadKey {
        key: libc::pthread_key_t,
        valid: bool,
    }

    impl ThreadKey {
        pub fn create() -> ThreadKey {
            let mut key: libc::pthread_key_t = 0;
            // SAFETY: `key` is a valid out-pointer; the destructor is a plain
            // `extern "C"` function that only touches allocator state.
            let rc = unsafe { libc::pthread_key_create(&mut key, Some(super::heap_destructor)) };
            ThreadKey { key, valid: rc == 0 }
        }

        pub fn valid(&self) -> bool {
            self.valid
        }

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

    pub fn tid() -> u32 {
        crate::sys::gettid() as u32
    }

    pub fn ncpus() -> u32 {
        crate::sys::ncpus()
    }

    pub fn now_ns() -> u64 {
        crate::sys::now_coarse_ns()
    }

    /// Eight random bytes from `getrandom(2)` for `slot_randomize`; on a
    /// kernel that refuses (not seen on any supported one) the clock, so
    /// the heap is still seeded with something that varies.
    pub fn seed() -> u64 {
        let mut b = [0u8; 8];
        match crate::sys::getrandom_exact(&mut b) {
            Ok(()) => u64::from_ne_bytes(b),
            Err(_) => crate::sys::now_coarse_ns(),
        }
    }
}

/// Off Linux there is no TLS accessor and no thread-exit key; host tests
/// drive heaps by hand.
#[cfg(not(target_os = "linux"))]
pub(super) mod os {
    use super::Heap;
    use core::sync::atomic::{AtomicU64, Ordering};

    pub struct ThreadKey;

    impl ThreadKey {
        pub fn create() -> ThreadKey {
            ThreadKey
        }

        pub fn valid(&self) -> bool {
            false
        }

        #[allow(dead_code)]
        pub fn set(&self, _heap: *mut Heap) {}
    }

    pub fn tid() -> u32 {
        0
    }

    /// Host tests: one CPU, so the default `narenas` resolves to 4.
    pub fn ncpus() -> u32 {
        1
    }

    static TICK: AtomicU64 = AtomicU64::new(0);

    pub fn now_ns() -> u64 {
        TICK.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Host tests: a fixed seed, so a randomised test is repeatable.
    pub fn seed() -> u64 {
        0xC4A1_10C5_EED0_0001
    }
}

use os::ThreadKey;

/// Guard slabs are placed once per `interval` slabs of 64 KiB, so a class
/// with smaller slabs (`layout::LG_MIN_SLAB`) gets one guard per
/// `interval * (64 KiB / slab)` slabs (in page groups when the page is
/// larger than the slab, see `SlabTable::is_guard_slab`): the same one
/// guard page per `interval * 64 KiB` of address space and the same
/// `madvise` count per byte committed for every class. 0 stays 0. The
/// ratio is a power of two (slab sizes and pages are), so a power-of-two
/// `interval` (what `config.rs` guarantees) stays one, as the mask test
/// needs.
pub(crate) fn guard_interval_for(class: usize, page: usize, interval: u32) -> u32 {
    let unit = (1usize << crate::layout::LG_GUARD_UNIT).max(page);
    let group = CLASSES[class].slab_size().max(page);
    if group >= unit { interval } else { interval.saturating_mul((unit / group) as u32) }
}

impl Shared {
    /// Initialise `this` in place for already reserved `regions`: the
    /// metadata allocator over the metadata reservation, one slab table
    /// per class with `guard_interval` (0 when the kernel has no guard
    /// regions), arena 0, the huge allocator (`guard_install` is the probed
    /// `MADV_GUARD_INSTALL` support), the registries and the thread-exit
    /// key. Returns `false` if the metadata region cannot hold the tables,
    /// leaving `this` unusable.
    ///
    /// # Safety
    /// `this` must point to writable storage for a `Shared` that nothing
    /// else reads until this returns `true`, and the regions must be
    /// reserved (on Linux, `PROT_NONE` mappings that this process owns).
    /// The `Shared` must never move afterwards: arena 0's class states
    /// point at the tables and the registry points at arena 0.
    ///
    /// `config` supplies the guard slab interval, the huge-run guards, the
    /// cache GC period, the cache capacities, the decay period and the
    /// number of automatic arenas (`narenas:0`, the default, is resolved
    /// here from the CPU count and stored, so `opt.narenas` reports the
    /// number in use); `guard_install` says whether the kernel
    /// has `MADV_GUARD_INSTALL`, without which no guard is placed at all
    /// (the `PROT_NONE` fallback would split every class region into one
    /// VMA per guard, see `class.rs`).
    pub unsafe fn init(
        this: *mut Shared,
        regions: Regions,
        config: Config,
        guard_install: bool,
        pac: bool,
        percpu: bool,
    ) -> bool {
        let mut config = config;
        config.narenas = config.effective_narenas(os::ncpus());
        let guard_interval = if guard_install { config.effective_guard_slab_interval() } else { 0 };
        let guard_install = guard_install && config.guards;
        let decay_ns = crate::purge::decay_ns_from_ms(config.dirty_decay_ms);
        // SAFETY: the caller guarantees `this` is writable and unaliased.
        unsafe {
            let meta = &raw mut (*this).meta;
            ptr::write(meta, Meta::new(regions.meta_base, regions.meta_len, regions.page_size()));
            ptr::write(&raw mut (*this).regions, regions);
            ptr::write(&raw mut (*this).multi_arena, AtomicBool::new(false));
            let meta = &*meta;
            let percpu = Percpu::build(meta, &config, percpu);
            ptr::write(
                &raw mut (*this).push_flags,
                (if config.double_free_scan { PUSH_SCAN } else { 0 })
                    | (if config.zero_on_free { PUSH_ZERO } else { 0 })
                    | (if pac && config.pac_pointers { PUSH_PAC } else { 0 })
                    | (if percpu.on() { PUSH_PERCPU } else { 0 }),
            );
            ptr::write(&raw mut (*this).percpu, percpu);
            let regions = &*(&raw const (*this).regions);
            let tables = &raw mut (*this).tables;
            for c in 0..NCLASSES {
                match SlabTable::new(meta, regions, c, guard_interval_for(c, regions.page_size(), guard_interval)) {
                    Some(t) => ptr::write(&raw mut (*tables)[c], t),
                    None => return false,
                }
            }
            let guard_test = &raw mut (*this).guard_test;
            for c in 0..NCLASSES {
                ptr::write(&raw mut (*guard_test)[c], GuardTest::of(&(*tables)[c]));
            }
            let arena0 = &raw mut (*this).arena0;
            if !Arena::init(arena0, meta, regions, &*tables, 0, decay_ns) {
                return false;
            }
            match Huge::new(meta, regions, guard_install) {
                Some(h) => {
                    h.set_retain_max(config.huge_retain_max);
                    ptr::write(&raw mut (*this).huge, h)
                }
                None => return false,
            }
            ptr::write(&raw mut (*this).gc_interval_ns, config.gc_interval_ns());
            Arenas::init(&raw mut (*this).arenas, arena0, config.narenas);
            Tcaches::init(&raw mut (*this).tcaches);
            ptr::write(&raw mut (*this).cgroup, crate::sys::cgroup::Cgroup::discover(config.cgroup));
            // Design 11: under an effective limit below 1 GiB a configured
            // `thp:always` is forced to `never` in the stored configuration
            // (`auto` is left alone; the report says so); the policy's
            // `high` baseline is the count at init.
            let cgroup = &*(&raw const (*this).cgroup);
            let thp_forced = config.thp == crate::config::Thp::Always
                && cgroup.limit().min(cgroup.high()) < crate::policy::THP_MIN_LIMIT;
            if thp_forced {
                config.thp = crate::config::Thp::Never;
            }
            ptr::write(&raw mut (*this).policy, crate::policy::Policy::new(cgroup.rises().0, thp_forced));
            // Applied once on the discovery sample, so the band and the cap
            // are published before the first 10 ms window closes (nothing
            // is `Empty` yet: retention 0).
            let limit = cgroup.limit().min(cgroup.high());
            (*this).policy.apply(crate::policy::band(limit, cgroup.current()), limit, 0, cgroup.rises().0);
            ptr::write(&raw mut (*this).config, config);
            ptr::write(&raw mut (*this).key, ThreadKey::create());
            ptr::write(&raw mut (*this).stats, Lock::new(crate::stats::Snapshot::EMPTY));
        }
        true
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
        self.arenas.get(self, index)
    }

    /// Arena `index`, which must exist: the index came from a slab or run
    /// descriptor, so it was built before that memory was handed out.
    #[inline]
    pub(super) fn owner(&self, index: u16) -> &Arena {
        match self.arena(index) {
            Some(a) => a,
            None => no_such_arena(index),
        }
    }

    /// Whether more than one arena exists (see the field).
    #[inline(always)]
    pub fn multi_arena(&self) -> bool {
        self.multi_arena.load(Ordering::Relaxed)
    }

    /// The flag byte both fast paths load: [`PUSH_SCAN`], [`PUSH_ZERO`],
    /// [`PUSH_PAC`] and [`PUSH_PERCPU`]; 0 in the default configuration on
    /// a CPU without pointer authentication.
    #[inline(always)]
    pub fn push_flags(&self) -> u8 {
        self.push_flags
    }

    /// Whether cache and heap pointers are signed ([`PUSH_PAC`]).
    #[inline(always)]
    pub fn pac(&self) -> bool {
        self.push_flags & PUSH_PAC != 0
    }

    /// Whether the per-CPU mode is on ([`PUSH_PERCPU`], design 15).
    #[inline(always)]
    pub fn percpu_on(&self) -> bool {
        self.push_flags & PUSH_PERCPU != 0
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

    /// The decay period new arenas start with (`dirty_decay_ms`).
    pub fn default_decay_ns(&self) -> u64 {
        crate::purge::decay_ns_from_ms(self.config.dirty_decay_ms)
    }

    /// Arena 0's decay period in nanoseconds (`purge::NEVER` when purging
    /// is off), which the huge allocator shares.
    #[inline]
    pub fn decay_ns(&self) -> u64 {
        self.arena0.decay_ns()
    }

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
    pub fn has_thread_key(&self) -> bool {
        self.key.valid()
    }

    /// The thread-exit key, for diagnostics and tests.
    #[cfg(target_os = "linux")]
    pub fn thread_key(&self) -> Option<libc::pthread_key_t> {
        self.key.key()
    }

    /// Claim one slot of `class` in `arena` straight from the slab bitmaps,
    /// bypassing every cache. Null when the region is exhausted. The flag
    /// says the memory is known zero.
    pub fn alloc_direct(&self, arena: &Arena, class: usize) -> (*mut u8, bool) {
        let mut out = [ptr::null_mut()];
        let c = arena.class(class).claim(&self.meta, &mut out);
        if c.n == 0 { (ptr::null_mut(), false) } else { (out[0], c.zeroed) }
    }

    /// Release one slot straight to its slab bitmap through its owning
    /// arena. Aborts unless `p` is an allocated slot of `class` in `arena`.
    pub fn free_direct(&self, arena: u16, class: usize, p: *mut u8) {
        self.owner(arena).class(class).release(&[p]);
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
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[inline(always)]
    pub(super) fn sized_slot(&self, p: *mut u8, hint: Option<usize>) -> Option<usize> {
        let class = hint?;
        let (slab, _slot) = self.regions.slot_in_class(class, p as usize)?;
        // SAFETY: `class_of`/`class_for` only produce `class < NCLASSES`
        // (`slot_in_class` relies on the same).
        if unsafe { self.guard_test.get_unchecked(class) }.is_guard(slab) {
            return None;
        }
        Some(class)
    }

    /// The sized free's abort: a slab pointer whose class is not the one
    /// the size hint names (`sdallocx`'s contract is `nallocx(size, flags)
    /// == sallocx(p)`). Huge runs and pointers that are not allocations pass
    /// here and are judged by the exact free that follows.
    #[cold]
    #[inline(never)]
    pub fn check_size_hint(&self, p: *mut u8, size: usize, hint: Option<usize>) {
        if let Kind::Slab { class, .. } = self.regions.classify(p as usize) {
            if hint != Some(class as usize) {
                panic!(
                    "sized free of {:#x}: size hint {} does not match the allocation (class {}, {} bytes)",
                    p as usize,
                    size,
                    class,
                    CLASSES[class as usize].size()
                );
            }
        }
    }

    /// Owning arena of the handed-out slab holding the slot `p` of `class`.
    /// Aborts for anything that is not a slot start in such a slab.
    pub(super) fn arena_of_slot(&self, class: usize, p: *mut u8) -> u16 {
        let t = self.table(class);
        let (slab, _) = t.locate(p);
        t.owner(slab)
    }

    /// Release a cache's sorted batch of `class` slots, keyed by `arena`.
    /// Plain frees push a slot onto the cache of its slab's arena, so the
    /// whole batch is normally one [`release_owned`] call with no extra
    /// work per pointer; sized frees push slots of any arena onto the home
    /// cache (their check reads neither the bit nor the owner), and each
    /// run of slabs of another arena is released through that arena (one
    /// owner-word load per switch, a word `release_owned` reads anyway).
    ///
    /// [`release_owned`]: crate::class::ClassState::release_owned
    pub fn release_routed(&self, arena: u16, class: usize, ptrs: &[*mut u8]) {
        let mut done = 0;
        let mut a = arena;
        loop {
            done += self.owner(a).class(class).release_owned(&ptrs[done..]);
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
    /// arithmetic and are needed only for the word address
    /// (`bench/results/2026-09-16-aarch64-arenas-geometry.md`).
    ///
    /// [`classify_free`]: Shared::classify_free
    /// [`Regions::slab_slot`]: crate::layout::Regions::slab_slot
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[inline(always)]
    pub(super) fn checked_slot(&self, p: *mut u8, h: &Heap) -> Option<usize> {
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
        if self.multi_arena() {
            // The home arena is read on this branch only. A plain load is
            // hoisted above the flag test (the heap is known valid) and
            // then every single-arena `free` touches the heap's header
            // line, which nothing else on the fast paths reads; a volatile
            // read stays here.
            // SAFETY: `h` is a live heap and `arena` a plain field.
            let home = unsafe { ptr::read_volatile(&raw const h.arena) };
            // SAFETY: `slab` is below the watermark.
            if unsafe { table.owner_unchecked(slab) } != home {
                return None;
            }
        }
        Some(class)
    }

    /// [`checked_slot`] without the arena test, for the per-CPU push: a
    /// CPU cache takes verified slots of every arena and its flush routes
    /// them by the owner array ([`release_routed`]), as the sized path's
    /// cache already does.
    ///
    /// [`checked_slot`]: Shared::checked_slot
    /// [`release_routed`]: Shared::release_routed
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[inline(always)]
    pub(super) fn checked_slot_any(&self, p: *mut u8) -> Option<usize> {
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
        Some(class)
    }

    /// Allocate a huge run of `size` bytes aligned to `align` in `arena`
    /// (which must exist). Null on exhaustion; the flag says the memory is
    /// known zero.
    pub fn alloc_huge(&self, size: usize, align: usize, arena: u16) -> (*mut u8, bool) {
        let r = self.huge.alloc(&self.meta, size, align, arena);
        if !r.0.is_null() {
            // The run's pages are what `Huge::alloc` committed for `size`
            // (see `usable_pages`) plus the guard.
            let pages = self.huge.usable_pages(size).unwrap_or(0) + GUARD_PAGES;
            self.owner(arena).note_huge_alloc(pages);
        }
        r
    }

    /// `zero_on_free` for a huge run: clear the usable bytes of the live
    /// run starting at `page` (aborts unless it is one) before its release.
    /// Out of line: it takes the huge lock for the size and then writes
    /// the whole run.
    #[cold]
    #[inline(never)]
    pub fn zero_huge(&self, page: usize) {
        let n = self.huge.usable_size(page);
        // SAFETY: a live run's usable bytes are committed, writable memory
        // this allocator handed out and the caller is freeing.
        unsafe { ptr::write_bytes(self.regions.huge_addr(page) as *mut u8, 0, n) };
    }

    /// Free the huge run starting at `page`. Aborts unless it is a live
    /// run start. Returns the usable bytes released.
    pub fn free_huge(&self, page: usize) -> usize {
        let run = self.huge.free_run(&self.meta, page);
        self.owner(run.arena).note_huge_free(run.pages);
        run.usable
    }
}

#[cold]
#[inline(never)]
fn no_such_arena(index: u16) -> ! {
    panic!("arena {index} does not exist")
}
