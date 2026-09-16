//! Thread heaps and class caches: the fast and slow allocation paths
//! (design.md section 6).
//!
//! Every thread that allocates gets a [`Heap`] in the metadata region,
//! reached through the one TLS slot in `tls.rs`. A heap holds one
//! [`ClassCache`] pointer per size class: a LIFO stack of slot pointers that
//! are marked in their slab bitmaps but not live in the program. The fast
//! paths only touch the heap, the cache and (for `free`) one word of the
//! slab descriptor: no atomic read-modify-write, no lock.
//!
//! - `alloc`: pop the top of the class's cache; if it is empty, *refill* by
//!   claiming up to `cap / 2` slots from the class's lowest partial slab.
//! - `free`: classify the pointer, check its bitmap bit is set (invalid and
//!   stale double frees abort here), push it; if the cache is full, *flush*
//!   the oldest `cap / 2` entries back to their slabs in one sorted batch.
//!
//! Until a thread has a heap its cache pointers all point at one shared
//! empty sentinel whose `len` and `cap` are zero, so the fast paths need no
//! null check: an empty cache is the slow path, and the slow path allocates
//! the real cache on first use. A heap that cannot be created (metadata
//! region exhausted, or the thread is inside heap creation or has already
//! run its exit destructor) is served without a cache, straight from the
//! slab bitmaps.
//!
//! Huge requests (above the largest medium class) bypass the caches: they
//! go straight to the [`Huge`] allocator under its lock, and `free` of a
//! pointer in the huge region is routed there by `Regions::classify`.
//!
//! Zeroed memory is tracked per cache as a prefix: `slots[..zeroed]` came
//! from a `Fresh` or `Purged` slab in the last refill and nothing has been
//! pushed below that index since, so `calloc` can skip its `memset` when it
//! pops from inside the prefix.
//!
//! Thread exit (design 6.5): [`Shared::init`] creates a `pthread_key` whose
//! destructor flushes every cache and returns the heap to the metadata
//! allocator. `pthread_setspecific` is called once per heap, while the TLS
//! slot holds the [`TLS_CREATING`] sentinel; an allocation made in that
//! window is served from the slab bitmaps directly. After the destructor the
//! slot holds [`TLS_EXITED`] and the thread is served the same way, so late
//! destructors of other keys never create a second heap.

use core::alloc::Layout;
#[cfg(target_os = "linux")]
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arena::{Arena, Arenas, Tcaches};
use crate::class::SlabTable;
use crate::config::Config;
use crate::huge::Huge;
use crate::layout::{CLASSES, Kind, NCLASSES, Regions};
use crate::meta::Meta;

/// Largest default cache capacity (classes up to 128 bytes). 1024 rather
/// than jemalloc's 200: every flush or refill writes the descriptor lines of
/// the slabs it touches and invalidates them in every other core's cache,
/// and with one arena those slabs are shared, so the `free` check's bitmap
/// load misses in proportion to the flush rate. Doubling the capacity from
/// 256 to 512 and again to 1024 took larson from 225 to 243 to 252 M ops/s
/// and xmalloc-test from 201 to 390 to 590 M frees/s at 10 threads for
/// 8 MiB of larson RSS (`bench/results/2026-09-16-aarch64-fast-path.md`).
pub const MAX_CAP: u32 = 1024;
/// Smallest cache capacity.
pub const MIN_CAP: u32 = 8;
/// Cache GC period when nothing overrides it (`tcache_gc_interval_ms`).
pub const DEFAULT_GC_INTERVAL_NS: u64 = 10_000_000_000;
/// Slow-path entries between two periodic-work checks (design 6.4).
pub const PERIODIC_EVERY: u32 = 64;

/// TLS slot value while this thread's heap is being created and
/// `pthread_setspecific` may re-enter the allocator.
pub const TLS_CREATING: usize = 1;
/// TLS slot value after this thread's heap destructor has run.
pub const TLS_EXITED: usize = 2;

/// Default cache capacity for a class: 256 slots for classes up to 128
/// bytes, halved for every doubling of the class size, never below 8.
pub const fn default_cap(class: usize) -> u32 {
    crate::config::cap_for(class, MAX_CAP, 1)
}

/// One thread's cache for one size class: a [`CACHE_HEADER`]-byte header
/// followed by `cap` slot pointers. Allocated from the metadata region with
/// [`ClassCache::layout`]; the header is `repr(C)` so the trailing array
/// sits at a fixed offset.
///
/// The byte counters `thread.allocated` and `thread.deallocated` are not
/// kept per operation. Every pop bumps `allocs` on the header line the pop
/// writes anyway, and the number of pushes follows from the stack
/// arithmetic `len = (frees + refilled) - (allocs + flushed)`, with
/// `moved = refilled - flushed` maintained on the slow paths; see
/// [`Heap::stats`].
#[repr(C)]
pub struct ClassCache {
    len: u32,
    cap: u32,
    /// Smallest `len` since the last GC (design 6.4).
    low_water: u32,
    /// `slots[..zeroed]` are known to read as zero.
    zeroed: u32,
    /// Pops served from this cache since it was created.
    allocs: u64,
    /// Slots moved in by refills minus slots moved out by flushes
    /// (wrapping; the true value is `len - frees + allocs`).
    moved: u64,
    slots: [*mut u8; 0],
}

/// Size of the [`ClassCache`] header, i.e. the offset of the slot array.
pub const CACHE_HEADER: usize = core::mem::size_of::<ClassCache>();
const _: () = assert!(CACHE_HEADER == 32);
const _: () = assert!(core::mem::offset_of!(ClassCache, slots) == CACHE_HEADER);

// SAFETY: the only shared instance is `EMPTY_CACHE`, which is never written
// (every write is guarded by `len < cap` or `len > 0`, both false for it);
// real caches are owned by one thread through its heap.
unsafe impl Sync for ClassCache {}

/// The sentinel every heap starts with: empty and full at the same time, so
/// both fast paths take their slow path and allocate a real cache.
static EMPTY_CACHE: ClassCache =
    ClassCache { len: 0, cap: 0, low_water: 0, zeroed: 0, allocs: 0, moved: 0, slots: [] };

#[inline(always)]
fn empty_cache() -> *mut ClassCache {
    &EMPTY_CACHE as *const ClassCache as *mut ClassCache
}

impl ClassCache {
    /// Metadata layout of a cache with `cap` slots.
    pub const fn layout(cap: u32) -> Layout {
        match Layout::from_size_align(CACHE_HEADER + 8 * cap as usize, 16) {
            Ok(l) => l,
            Err(_) => unreachable!(),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    pub fn cap(&self) -> usize {
        self.cap as usize
    }

    #[inline]
    pub fn low_water(&self) -> usize {
        self.low_water as usize
    }

    /// Number of entries at the bottom of the stack known to be zero.
    #[inline]
    pub fn zeroed(&self) -> usize {
        self.zeroed as usize
    }

    #[inline]
    fn slots(&mut self) -> *mut *mut u8 {
        self.slots.as_mut_ptr()
    }

    /// Cached pointers, oldest first.
    #[inline]
    fn entries(&mut self) -> &mut [*mut u8] {
        // SAFETY: `len <= cap` pointers follow the header.
        unsafe { core::slice::from_raw_parts_mut(self.slots(), self.len as usize) }
    }
}

/// Per-thread byte counters (`thread.allocated`, `thread.deallocated`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadStats {
    pub allocated: u64,
    pub deallocated: u64,
}

impl ThreadStats {
    /// Add `n` operations of `size` bytes each.
    #[inline]
    fn add(&mut self, size: u64, allocs: u64, frees: u64) {
        self.allocated = self.allocated.wrapping_add(size.wrapping_mul(allocs));
        self.deallocated = self.deallocated.wrapping_add(size.wrapping_mul(frees));
    }
}

/// A heap's caches for one arena other than its home arena (design 6.2,
/// `caches[arena]`), allocated from the metadata region the first time the
/// heap touches that arena and kept on a singly linked list. Almost every
/// heap has none; Valkey's main thread has one (the Lua arena).
#[repr(C)]
pub struct ArenaCaches {
    arena: u16,
    next: *mut ArenaCaches,
    caches: [*mut ClassCache; NCLASSES],
}

/// One thread's allocator state (design 6.2). Lives in the metadata region;
/// reached through the TLS slot. Also the shape of an explicit cache
/// (`tcache.create`), which is a heap owned by no thread.
pub struct Heap {
    shared: *const Shared,
    tid: u32,
    /// Home arena: where plain `malloc` allocates from, and the arena whose
    /// caches are `caches` (design 6.2 `heap.arena`).
    arena: u16,
    slow_ticks: u32,
    last_gc: u64,
    rng: u64,
    /// Bytes that never went through a cache: huge runs and slots served
    /// directly from a slab because no cache could be allocated. The
    /// cached classes are derived from their headers in [`Heap::stats`].
    uncached: ThreadStats,
    /// Caches of the home arena.
    caches: [*mut ClassCache; NCLASSES],
    /// Caches of every other arena this heap has touched.
    others: *mut ArenaCaches,
}

/// Process-wide allocator state that every heap works against: the regions,
/// one [`SlabTable`] per class, arena 0, the metadata allocator, the huge
/// allocator, the arena and explicit-cache registries and the thread-exit
/// key. Built in place by [`Shared::init`] because the class states borrow
/// `meta`, `regions` and `tables`.
///
/// `repr(C)` fixes the order: `regions` and `multi_arena`, read by every
/// `malloc` and `free`, come first, then the line-aligned tables (read by
/// every `free`) and arena 0's class states, so the locks inside `meta` and
/// `huge` (taken on heap creation and huge allocations) never share a cache
/// line with them.
#[repr(C)]
pub struct Shared {
    pub regions: Regions,
    /// Set once a second arena exists. Until then every slab and every
    /// heap is arena 0 and the `free` fast path skips the descriptor's
    /// arena load. Relaxed: a slot of arena `a > 0` can only be freed after
    /// it was allocated, which happened after `a` was built and this flag
    /// stored, and the program itself orders that allocation before the
    /// free; so any thread freeing such a slot already sees the flag.
    multi_arena: AtomicBool,
    tables: [SlabTable; NCLASSES],
    pub arena0: Arena,
    pub meta: Meta,
    pub huge: Huge,
    /// Cache GC period (`tcache_gc_interval_ms`).
    pub gc_interval_ns: u64,
    /// The parsed `malloc_conf` (design section 12). Keys that already have
    /// a knob are applied by `init`; the rest are read by later phases.
    pub config: Config,
    key: ThreadKey,
    pub arenas: Arenas,
    pub tcaches: Tcaches,
}

#[cfg(target_os = "linux")]
mod os {
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

    pub fn now_ns() -> u64 {
        crate::sys::now_coarse_ns()
    }

    pub fn seed() -> u64 {
        let mut b = [0u8; 8];
        let _ = crate::sys::getrandom_exact(&mut b);
        u64::from_ne_bytes(b) | 1
    }
}

/// Off Linux there is no TLS accessor and no thread-exit key; host tests
/// drive heaps by hand.
#[cfg(not(target_os = "linux"))]
mod os {
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

    static TICK: AtomicU64 = AtomicU64::new(0);

    pub fn now_ns() -> u64 {
        TICK.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn seed() -> u64 {
        0x9e37_79b9_7f4a_7c15
    }
}

use os::ThreadKey;

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
    /// number of automatic arenas; `guard_install` says whether the kernel
    /// has `MADV_GUARD_INSTALL`, without which no guard is placed at all
    /// (the `PROT_NONE` fallback would split every class region into one
    /// VMA per guard, see `class.rs`).
    pub unsafe fn init(this: *mut Shared, regions: Regions, config: Config, guard_install: bool) -> bool {
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
            let regions = &*(&raw const (*this).regions);
            let tables = &raw mut (*this).tables;
            for c in 0..NCLASSES {
                match SlabTable::new(meta, regions, c, guard_interval) {
                    Some(t) => ptr::write(&raw mut (*tables)[c], t),
                    None => return false,
                }
            }
            let arena0 = &raw mut (*this).arena0;
            if !Arena::init(arena0, meta, regions, &*tables, 0, decay_ns) {
                return false;
            }
            match Huge::new(meta, regions, guard_install) {
                Some(h) => ptr::write(&raw mut (*this).huge, h),
                None => return false,
            }
            ptr::write(&raw mut (*this).gc_interval_ns, config.gc_interval_ns());
            Arenas::init(&raw mut (*this).arenas, arena0, config.narenas);
            Tcaches::init(&raw mut (*this).tcaches);
            ptr::write(&raw mut (*this).config, config);
            ptr::write(&raw mut (*this).key, ThreadKey::create());
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
    fn owner(&self, index: u16) -> &Arena {
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

    /// A second arena is being built (`Arenas::build`).
    pub fn set_multi_arena(&self) {
        self.multi_arena.store(true, Ordering::Relaxed);
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
                let t = self.table(class);
                let desc = t.desc(slab as usize);
                desc.check(t.class(), slot as usize, p as usize, class, slab as usize);
                Ok((class, desc.arena()))
            }
            Kind::Huge { page } => Err(page),
            Kind::Invalid => invalid_free(p as usize),
        }
    }

    /// The `free` fast path's check: `Some(class)` when `p` is a slot start
    /// in a committed slab whose bit is set and whose arena is `home` (the
    /// arena load is skipped while only arena 0 exists), `None` for
    /// anything else. It never aborts; [`classify_free`] on the slow path
    /// repeats the checks and does. Every table read is bounds-free: the
    /// class index comes from `slab_slot`, the descriptor stride from the
    /// table, the bitmap word from a slot already known to be below
    /// `nslots`.
    ///
    /// [`classify_free`]: Shared::classify_free
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[inline(always)]
    fn checked_slot(&self, p: *mut u8, home: u16) -> Option<usize> {
        let (class, slab, slot) = self.regions.slab_slot(p as usize)?;
        let desc = self.table(class).desc_if_committed(slab)?;
        // SAFETY: `slab_slot` validated `slot < nslots`.
        if !unsafe { desc.is_allocated_unchecked(slot) } {
            return None;
        }
        if self.multi_arena() && desc.arena() != home {
            return None;
        }
        Some(class)
    }

    /// Allocate a huge run of `size` bytes aligned to `align` in `arena`
    /// (which must exist). Null on exhaustion; the flag says the memory is
    /// known zero.
    pub fn alloc_huge(&self, size: usize, align: usize, arena: u16) -> (*mut u8, bool) {
        self.huge.alloc(&self.meta, size, align, arena)
    }

    /// Free the huge run starting at `page`. Aborts unless it is a live
    /// run start. Returns the usable bytes released.
    pub fn free_huge(&self, page: usize) -> usize {
        self.huge.free(&self.meta, page)
    }
}

impl Heap {
    /// Create a heap in the metadata region for the calling thread, homed
    /// on the next automatic arena. Null if the region is exhausted. Does
    /// not touch TLS or the thread key.
    pub fn create(shared: &Shared) -> *mut Heap {
        Self::create_detached(shared, shared.arenas.assign())
    }

    /// Create a heap homed on `arena` (an explicit cache, or a thread heap
    /// before it is registered). Null if the region is exhausted.
    pub fn create_detached(shared: &Shared, arena: u16) -> *mut Heap {
        let h = shared.meta.alloc(Layout::new::<Heap>()) as *mut Heap;
        if h.is_null() {
            return h;
        }
        // SAFETY: `h` is a fresh, exclusively owned metadata block of the
        // right size and alignment.
        unsafe {
            ptr::write(
                h,
                Heap {
                    shared,
                    tid: os::tid(),
                    arena,
                    slow_ticks: 0,
                    last_gc: os::now_ns(),
                    rng: os::seed(),
                    uncached: ThreadStats::default(),
                    caches: [empty_cache(); NCLASSES],
                    others: ptr::null_mut(),
                },
            );
        }
        h
    }

    /// Flush every cache, free them and the heap itself back to the
    /// metadata allocator. `h` is dangling afterwards.
    ///
    /// # Safety
    /// `h` must come from [`Heap::create`] on `shared` and no longer be in
    /// use by any thread.
    pub unsafe fn destroy(h: *mut Heap, shared: &Shared) {
        // SAFETY: the caller gives us exclusive ownership of the heap.
        let heap = unsafe { &mut *h };
        heap.flush_all(shared);
        Self::free_caches(shared, &mut heap.caches);
        let mut o = heap.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                let next = (*o).next;
                Self::free_caches(shared, &mut (*o).caches);
                shared.meta.free(o as *mut u8, Layout::new::<ArenaCaches>());
                o = next;
            }
        }
        heap.others = ptr::null_mut();
        shared.meta.free(h as *mut u8, Layout::new::<Heap>());
    }

    /// Free every real (flushed) cache of one array and reset it to the
    /// sentinel.
    fn free_caches(shared: &Shared, caches: &mut [*mut ClassCache; NCLASSES]) {
        for slot in caches.iter_mut() {
            let cache = *slot;
            // SAFETY: real caches were allocated with `ClassCache::layout(cap)`.
            unsafe {
                if (*cache).cap != 0 {
                    debug_assert_eq!((*cache).len, 0);
                    shared.meta.free(cache as *mut u8, ClassCache::layout((*cache).cap));
                }
            }
            *slot = empty_cache();
        }
    }

    #[inline]
    pub fn shared(&self) -> &Shared {
        // SAFETY: a heap never outlives the `Shared` it was created on.
        unsafe { &*self.shared }
    }

    #[inline]
    pub fn tid(&self) -> u32 {
        self.tid
    }

    /// Home arena (design 6.2 `heap.arena`; `thread.arena`).
    #[inline]
    pub fn arena(&self) -> u16 {
        self.arena
    }

    /// Change the home arena (`thread.arena`). The caches of the old home
    /// move to the `others` list and the new home's, if any, take their
    /// place, so nothing is flushed. `arena` must exist.
    pub fn set_arena(&mut self, shared: &Shared, arena: u16) {
        if arena == self.arena {
            return;
        }
        let old = self.arena;
        let node = self.other_caches(shared, arena, true);
        if node.is_null() {
            // No metadata for a new node: the caches of the new home start
            // from the sentinel and the old home's must go back first.
            self.flush_arena(shared, old, &mut self.caches.clone());
            let mut caches = core::mem::replace(&mut self.caches, [empty_cache(); NCLASSES]);
            Self::free_caches(shared, &mut caches);
        } else {
            // SAFETY: `node` is a live block owned by this heap.
            unsafe {
                core::mem::swap(&mut (*node).caches, &mut self.caches);
                (*node).arena = old;
            }
        }
        self.arena = arena;
    }

    /// Byte counters for this thread: the uncached part plus, for every
    /// cache, `allocs` pops and `len + allocs - moved` pushes, each worth
    /// the class size.
    pub fn stats(&self) -> ThreadStats {
        let mut s = self.uncached;
        let mut add = |caches: &[*mut ClassCache; NCLASSES]| {
            for (class, &cache) in caches.iter().enumerate() {
                // SAFETY: sentinel (`cap == 0`, skipped) or this heap's cache.
                let c = unsafe { &*cache };
                if c.cap == 0 {
                    continue;
                }
                let frees = (c.len as u64).wrapping_add(c.allocs).wrapping_sub(c.moved);
                s.add(CLASSES[class].size() as u64, c.allocs, frees);
            }
        };
        add(&self.caches);
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                add(&(*o).caches);
                o = (*o).next;
            }
        }
        s
    }

    #[inline]
    pub fn slow_ticks(&self) -> u32 {
        self.slow_ticks
    }

    /// Next value of the heap's xorshift generator (for `slot_randomize`).
    #[inline]
    pub fn next_random(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    /// The home arena's cache for `class`, or `None` while it is still the
    /// sentinel.
    pub fn cache(&self, class: usize) -> Option<&ClassCache> {
        // SAFETY: cache pointers are the sentinel or a live metadata block.
        let c = unsafe { &*self.caches[class] };
        if c.cap == 0 { None } else { Some(c) }
    }

    /// The cache for `class` in `arena`, home or not, or `None` while it is
    /// the sentinel (or the arena was never touched).
    pub fn cache_in(&self, arena: u16, class: usize) -> Option<&ClassCache> {
        if arena == self.arena {
            return self.cache(class);
        }
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                if (*o).arena == arena {
                    let c = &*(*o).caches[class];
                    return if c.cap == 0 { None } else { Some(c) };
                }
                o = (*o).next;
            }
        }
        None
    }

    #[inline(always)]
    fn cache_ptr(&self, class: usize) -> *mut ClassCache {
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of` only yields indices below `NCLASSES`.
        unsafe { *self.caches.get_unchecked(class) }
    }

    /// Fast path of `malloc` for a non-huge class in the home arena.
    #[inline(always)]
    pub fn alloc(&mut self, shared: &Shared, class: usize) -> *mut u8 {
        match self.alloc_fast(class) {
            Some(p) => p,
            None => self.alloc_slow(shared, class).0,
        }
    }

    /// The cache pop alone: `None` when the cache is empty (or still the
    /// sentinel) and the caller must take a slow path.
    #[inline(always)]
    pub fn alloc_fast(&mut self, class: usize) -> Option<*mut u8> {
        let cache = self.cache_ptr(class);
        // SAFETY: the pointer is the sentinel (read-only, `len == 0`) or
        // this thread's own cache.
        unsafe {
            let len = (*cache).len;
            if len == 0 {
                return None;
            }
            Some(self.pop(cache, class, len).0)
        }
    }

    /// Fast path of `calloc`: the flag is true when the memory is known to
    /// read as zero, so the caller can skip its `memset`.
    #[inline(always)]
    pub fn alloc_zeroed(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        let cache = self.cache_ptr(class);
        // SAFETY: as in `alloc`.
        unsafe {
            let len = (*cache).len;
            if len == 0 {
                return self.alloc_slow(shared, class);
            }
            self.pop(cache, class, len)
        }
    }

    /// Allocation of `class` from `arena` through this heap's cache for
    /// that arena (`MALLOCX_ARENA`, or the home arena of an explicit
    /// cache). Null when `arena` does not exist or is exhausted.
    pub fn alloc_in(&mut self, shared: &Shared, arena: u16, class: usize) -> (*mut u8, bool) {
        if arena == self.arena {
            return self.alloc_zeroed(shared, class);
        }
        self.alloc_other(shared, arena, class)
    }

    /// Pop the top entry of a non-empty cache. Touches only the header
    /// line and the slot: `allocs` is the only counter, `low_water` is
    /// written only when it actually drops (it can only do so after a GC
    /// reset it to `len`), and the class size is never looked up.
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `class` and `len` is its
    /// current non-zero length.
    #[inline(always)]
    unsafe fn pop(&mut self, cache: *mut ClassCache, _class: usize, len: u32) -> (*mut u8, bool) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let len = len - 1;
            (*cache).len = len;
            if len < (*cache).low_water {
                (*cache).low_water = len;
            }
            (*cache).allocs = (*cache).allocs.wrapping_add(1);
            let p = *(*cache).slots().add(len as usize);
            let zero = len < (*cache).zeroed;
            (p, zero)
        }
    }

    /// Fast path of `free`. Aborts unless `p` is an allocated slab slot or
    /// a live huge run. A slot of another arena goes to this heap's cache
    /// for that arena (design 6.3 step 4).
    #[inline(always)]
    pub fn free(&mut self, shared: &Shared, p: *mut u8) {
        match shared.classify_free(p) {
            Ok((class, arena)) => {
                if arena == self.arena {
                    self.push(shared, class, p)
                } else {
                    self.free_other(shared, arena, class, p)
                }
            }
            Err(page) => self.free_huge(shared, page),
        }
    }

    /// Huge allocation on this thread in its home arena: no cache, one
    /// lock, counted in the thread's statistics.
    #[cold]
    #[inline(never)]
    pub fn alloc_huge(&mut self, shared: &Shared, size: usize, align: usize) -> (*mut u8, bool) {
        let arena = self.arena;
        self.alloc_huge_in(shared, size, align, arena)
    }

    /// Huge allocation on this thread in `arena` (which must exist).
    pub fn alloc_huge_in(&mut self, shared: &Shared, size: usize, align: usize, arena: u16) -> (*mut u8, bool) {
        let (p, zero) = shared.alloc_huge(size, align, arena);
        if !p.is_null() {
            self.uncached.allocated += shared.huge.usable_for(size).unwrap_or(0) as u64;
        }
        (p, zero)
    }

    /// Free of a huge run on this thread.
    #[cold]
    #[inline(never)]
    fn free_huge(&mut self, shared: &Shared, page: usize) {
        self.uncached.deallocated += shared.free_huge(page) as u64;
    }

    /// A slot of `class` was claimed directly (`MALLOCX_TCACHE_NONE`) on
    /// this thread: count it.
    pub fn count_direct_alloc(&mut self, class: usize) {
        self.uncached.allocated += CLASSES[class].size() as u64;
    }

    /// A slot of `class` was released directly on this thread: count it.
    pub fn count_direct_free(&mut self, class: usize) {
        self.uncached.deallocated += CLASSES[class].size() as u64;
    }

    /// Push a checked slot of `class` (home arena) onto its cache.
    #[inline(always)]
    fn push(&mut self, shared: &Shared, class: usize, p: *mut u8) {
        if !self.push_fast(class, p) {
            self.free_slow(shared, class, p);
        }
    }

    /// The cache push alone: `false` when the cache is full (or still the
    /// sentinel) and the caller must take a slow path. No counter moves:
    /// the push count is implied by `len`, `allocs` and `moved`. `zeroed`
    /// is written only when the pushed slot lands inside the zero prefix,
    /// which is rare (the prefix is usually already zero).
    #[inline(always)]
    pub fn push_fast(&mut self, class: usize, p: *mut u8) -> bool {
        let cache = self.cache_ptr(class);
        // SAFETY: the pointer is the sentinel (`len == cap == 0`, so it is
        // never written) or this thread's own cache with `len < cap` slots.
        unsafe { Self::push_cache(cache, p) }
    }

    /// Push onto `cache` unless it is full.
    ///
    /// # Safety
    /// `cache` is the sentinel or a real cache owned by this heap.
    #[inline(always)]
    unsafe fn push_cache(cache: *mut ClassCache, p: *mut u8) -> bool {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let len = (*cache).len;
            if len == (*cache).cap {
                return false;
            }
            *(*cache).slots().add(len as usize) = p;
            (*cache).len = len + 1;
            if len < (*cache).zeroed {
                (*cache).zeroed = len;
            }
        }
        true
    }

    /// Refill the home cache and pop, allocating the cache first if this
    /// is the class's first use. Serves directly from the slab when there
    /// is no cache to be had.
    #[cold]
    #[inline(never)]
    fn alloc_slow(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        let arena = self.arena;
        self.alloc_slow_in(shared, arena, class)
    }

    /// Allocation of `class` from another arena than the home one: through
    /// this heap's cache for that arena, which is looked up (or created)
    /// on the `others` list.
    #[cold]
    #[inline(never)]
    fn alloc_other(&mut self, shared: &Shared, arena: u16, class: usize) -> (*mut u8, bool) {
        let node = self.other_caches(shared, arena, true);
        if !node.is_null() {
            // SAFETY: `node` is a live block owned by this heap.
            let cache = unsafe { (*node).caches[class] };
            // SAFETY: sentinel (`len == 0`) or this heap's cache.
            let len = unsafe { (*cache).len };
            if len != 0 {
                // SAFETY: a real cache with `len` entries.
                return unsafe { self.pop(cache, class, len) };
            }
        }
        self.alloc_slow_in(shared, arena, class)
    }

    /// The slow allocation path for any arena.
    fn alloc_slow_in(&mut self, shared: &Shared, arena: u16, class: usize) -> (*mut u8, bool) {
        self.tick(shared);
        let Some(a) = shared.arena(arena) else { return (ptr::null_mut(), false) };
        let cache = self.ensure_cache(shared, arena, class);
        if cache.is_null() {
            let (p, zero) = shared.alloc_direct(a, class);
            if !p.is_null() {
                self.uncached.allocated += CLASSES[class].size() as u64;
            }
            return (p, zero);
        }
        // SAFETY: `cache` is this heap's real cache for `(arena, class)`.
        unsafe {
            let len = self.refill(shared, a, cache, class);
            if len == 0 {
                return (ptr::null_mut(), false);
            }
            self.pop(cache, class, len)
        }
    }

    /// Flush the oldest half of the home cache and push, allocating the
    /// cache first if this is the class's first use. Releases directly to
    /// the slab when there is no cache to be had.
    #[cold]
    #[inline(never)]
    fn free_slow(&mut self, shared: &Shared, class: usize, p: *mut u8) {
        let arena = self.arena;
        self.free_slow_in(shared, arena, class, p)
    }

    /// Free of a slot whose slab belongs to another arena than the home
    /// one: onto this heap's cache for that arena.
    #[cold]
    #[inline(never)]
    fn free_other(&mut self, shared: &Shared, arena: u16, class: usize, p: *mut u8) {
        let node = self.other_caches(shared, arena, true);
        if !node.is_null() {
            // SAFETY: `node` is a live block owned by this heap; the cache
            // is the sentinel or ours.
            if unsafe { Self::push_cache((*node).caches[class], p) } {
                return;
            }
        }
        self.free_slow_in(shared, arena, class, p)
    }

    /// The slow free path for any arena.
    fn free_slow_in(&mut self, shared: &Shared, arena: u16, class: usize, p: *mut u8) {
        self.tick(shared);
        let cache = self.ensure_cache(shared, arena, class);
        if cache.is_null() {
            shared.free_direct(arena, class, p);
            self.uncached.deallocated += CLASSES[class].size() as u64;
            return;
        }
        // SAFETY: `cache` is this heap's real cache for `(arena, class)`.
        unsafe {
            if (*cache).len == (*cache).cap {
                let half = ((*cache).cap / 2).max(1);
                self.flush(shared, arena, cache, class, half as usize);
            }
            if !Self::push_cache(cache, p) {
                unreachable!("heap: cache still full after a flush");
            }
        }
    }

    /// The `others` node for `arena`, created (if `create`) on first use.
    /// Null when there is none (and none could be allocated). Never called
    /// with the home arena.
    fn other_caches(&mut self, shared: &Shared, arena: u16, create: bool) -> *mut ArenaCaches {
        debug_assert_ne!(arena, self.arena);
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                if (*o).arena == arena {
                    return o;
                }
                o = (*o).next;
            }
        }
        if !create {
            return ptr::null_mut();
        }
        let node = shared.meta.alloc(Layout::new::<ArenaCaches>()) as *mut ArenaCaches;
        if node.is_null() {
            return node;
        }
        // SAFETY: a fresh block of the right layout, exclusively ours.
        unsafe {
            ptr::write(node, ArenaCaches { arena, next: self.others, caches: [empty_cache(); NCLASSES] });
        }
        self.others = node;
        node
    }

    /// The slot holding the cache pointer for `(arena, class)`, creating
    /// the `others` node if needed. Null if no node could be allocated.
    fn cache_slot(&mut self, shared: &Shared, arena: u16, class: usize) -> *mut *mut ClassCache {
        if arena == self.arena {
            return &mut self.caches[class];
        }
        let node = self.other_caches(shared, arena, true);
        if node.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `node` is a live block owned by this heap.
        unsafe { &mut (*node).caches[class] }
    }

    /// The real cache for `(arena, class)`, allocating it on first use.
    /// Null if the metadata region is exhausted or caches are off
    /// (`tcache:false`).
    fn ensure_cache(&mut self, shared: &Shared, arena: u16, class: usize) -> *mut ClassCache {
        let slot = self.cache_slot(shared, arena, class);
        if slot.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `slot` points into this heap's own cache array; sentinel
        // or real cache, both readable.
        let cache = unsafe { *slot };
        if unsafe { (*cache).cap } != 0 {
            return cache;
        }
        let cap = shared.config.cache_cap(class);
        if cap == 0 {
            return ptr::null_mut();
        }
        let cache = shared.meta.alloc(ClassCache::layout(cap)) as *mut ClassCache;
        if cache.is_null() {
            return cache;
        }
        // SAFETY: a fresh block of `layout(cap)` bytes, exclusively ours;
        // `slot` is still valid (nothing touched the arrays since).
        unsafe {
            ptr::write(cache, ClassCache { len: 0, cap, low_water: 0, zeroed: 0, allocs: 0, moved: 0, slots: [] });
            *slot = cache;
        }
        cache
    }

    /// Claim up to `cap / 2` slots of `class` in `arena` into an empty
    /// cache, lowest address on top so allocations stay address-ordered.
    /// Returns the new length.
    ///
    /// # Safety
    /// `cache` is this heap's real, empty cache for `(arena, class)`.
    unsafe fn refill(&mut self, shared: &Shared, arena: &Arena, cache: *mut ClassCache, class: usize) -> u32 {
        // SAFETY: the cache is empty and has `cap` slots of storage.
        unsafe {
            debug_assert_eq!((*cache).len, 0);
            let want = ((*cache).cap / 2).max(1) as usize;
            let out = core::slice::from_raw_parts_mut((*cache).slots(), want);
            let claim = arena.class(class).claim(&shared.meta, out);
            out[..claim.n].reverse();
            (*cache).len = claim.n as u32;
            (*cache).moved = (*cache).moved.wrapping_add(claim.n as u64);
            (*cache).low_water = 0;
            (*cache).zeroed = if claim.zeroed { claim.n as u32 } else { 0 };
            claim.n as u32
        }
    }

    /// Release the oldest `n` entries of a cache to their slabs (in
    /// `arena`, which owns them) in one address-sorted batch and close the
    /// gap.
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `(arena, class)` and `n <= len`.
    unsafe fn flush(&mut self, shared: &Shared, arena: u16, cache: *mut ClassCache, class: usize, n: usize) {
        if n == 0 {
            return;
        }
        // SAFETY: `n <= len <= cap` entries exist.
        unsafe {
            let all = (*cache).entries();
            let (old, rest) = all.split_at_mut(n);
            old.sort_unstable();
            shared.owner(arena).class(class).release(old);
            let rest_len = rest.len();
            ptr::copy((*cache).slots().add(n), (*cache).slots(), rest_len);
            (*cache).len = rest_len as u32;
            (*cache).moved = (*cache).moved.wrapping_sub(n as u64);
            (*cache).zeroed = (*cache).zeroed.saturating_sub(n as u32);
            (*cache).low_water = (*cache).low_water.min(rest_len as u32);
        }
    }

    /// Flush every cache of one arena's array completely.
    fn flush_arena(&mut self, shared: &Shared, arena: u16, caches: &[*mut ClassCache; NCLASSES]) {
        for (class, &cache) in caches.iter().enumerate() {
            // SAFETY: sentinel (`len == 0`, nothing to do) or real cache.
            unsafe {
                let len = (*cache).len as usize;
                if len != 0 {
                    self.flush(shared, arena, cache, class, len);
                }
            }
        }
    }

    /// Return every cached slot to its slab. Caches stay allocated.
    pub fn flush_all(&mut self, shared: &Shared) {
        let home = self.arena;
        let caches = self.caches;
        self.flush_arena(shared, home, &caches);
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap; the array is copied out so `flush` can borrow
            // `self`.
            unsafe {
                let (arena, caches) = ((*o).arena, (*o).caches);
                self.flush_arena(shared, arena, &caches);
                o = (*o).next;
            }
        }
    }

    /// Cache GC (design 6.4): for every cache, release three quarters of
    /// the entries that went unused since the last GC, then restart the
    /// low-water tracking from the current length.
    pub fn gc(&mut self, shared: &Shared) {
        let home = self.arena;
        let caches = self.caches;
        self.gc_arena(shared, home, &caches);
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: as in `flush_all`.
            unsafe {
                let (arena, caches) = ((*o).arena, (*o).caches);
                self.gc_arena(shared, arena, &caches);
                o = (*o).next;
            }
        }
    }

    fn gc_arena(&mut self, shared: &Shared, arena: u16, caches: &[*mut ClassCache; NCLASSES]) {
        for (class, &cache) in caches.iter().enumerate() {
            // SAFETY: sentinel (`cap == 0`, read-only, skipped) or real cache.
            unsafe {
                if (*cache).cap == 0 {
                    continue;
                }
                let lw = (*cache).low_water as usize;
                if lw > 0 {
                    self.flush(shared, arena, cache, class, lw - lw / 4);
                }
                (*cache).low_water = (*cache).len;
            }
        }
    }

    /// Count a slow-path entry and run periodic work every
    /// [`PERIODIC_EVERY`] of them: cache GC when the interval has elapsed,
    /// then decay (or starting the background thread that will do it,
    /// `purge::on_tick`). Stats merging and cgroup sampling join in later
    /// tasks.
    fn tick(&mut self, shared: &Shared) {
        self.slow_ticks = self.slow_ticks.wrapping_add(1);
        if self.slow_ticks % PERIODIC_EVERY == 0 {
            let now = os::now_ns();
            if now.wrapping_sub(self.last_gc) >= shared.gc_interval_ns {
                self.last_gc = now;
                self.gc(shared);
            }
            crate::purge::on_tick(shared, now);
        }
    }
}

/// Destructor of the thread-exit key: flush and free the exiting thread's
/// heap, then mark the thread as exited so a later allocation from another
/// key's destructor is served without a heap.
///
/// # Safety
/// `p` is the heap pointer registered with `pthread_setspecific` by
/// `heap_for_thread`, called on the thread that owns it.
#[cfg(target_os = "linux")]
pub unsafe extern "C" fn heap_destructor(p: *mut c_void) {
    let h = p as *mut Heap;
    // SAFETY: the pointer was registered by `heap_for_thread` for this thread.
    unsafe {
        let shared = &*(*h).shared;
        Heap::destroy(h, shared);
    }
    crate::tls::set(TLS_EXITED as *mut c_void);
}

/// The calling thread's heap, created on first use. `None` when the thread
/// must be served without a cache: heap creation is in progress on it
/// (`pthread_setspecific` re-entered the allocator), its destructor has run,
/// or the metadata region is exhausted.
#[cfg(target_os = "linux")]
#[inline(always)]
pub fn heap_for_thread(shared: &Shared) -> Option<&mut Heap> {
    let t = crate::tls::get() as usize;
    if t > TLS_EXITED {
        // SAFETY: anything above the sentinels is this thread's heap pointer.
        return Some(unsafe { &mut *(t as *mut Heap) });
    }
    create_heap_for_thread(shared, t)
}

#[cfg(target_os = "linux")]
#[cold]
#[inline(never)]
fn create_heap_for_thread(shared: &Shared, slot: usize) -> Option<&mut Heap> {
    if slot != 0 {
        return None;
    }
    let h = Heap::create(shared);
    if h.is_null() {
        return None;
    }
    crate::tls::set(TLS_CREATING as *mut c_void);
    shared.key.set(h);
    crate::tls::set(h as *mut c_void);
    // SAFETY: freshly created and owned by this thread.
    Some(unsafe { &mut *h })
}

/// The calling thread's heap if it already exists, with no attempt to
/// create one. The fast paths use this so that everything unusual is one
/// tail call away. `Some` also proves the allocator is initialised on this
/// thread: heaps are only created by [`heap_for_thread`] under `shared()`,
/// so the entry points skip the init-state load (an acquire that would
/// otherwise head every dependent-load chain).
#[cfg(target_os = "linux")]
#[inline(always)]
pub fn existing_heap() -> Option<&'static mut Heap> {
    let t = crate::tls::get() as usize;
    if t > TLS_EXITED {
        // SAFETY: anything above the sentinels is this thread's heap pointer.
        Some(unsafe { &mut *(t as *mut Heap) })
    } else {
        None
    }
}

/// The `free` fast path on an existing heap: check the slot and push it
/// onto the home cache. `false` whenever anything is off the fast path (not
/// a verified slab slot, another arena's slab, full cache); the caller then
/// goes through a cold [`free`], which repeats the checks and aborts when
/// they fail. Contains no call, so the entry point that inlines it needs no
/// frame.
#[inline(always)]
pub fn free_fast(shared: &Shared, h: &mut Heap, p: *mut u8) -> bool {
    match shared.checked_slot(p, h.arena) {
        Some(class) => h.push_fast(class, p),
        None => false,
    }
}

/// `malloc` of a non-huge class on the calling thread.
#[cfg(target_os = "linux")]
#[inline(always)]
pub fn alloc(shared: &Shared, class: usize) -> *mut u8 {
    match heap_for_thread(shared) {
        Some(h) => h.alloc(shared, class),
        None => shared.alloc_direct(&shared.arena0, class).0,
    }
}

/// `calloc` of a non-huge class on the calling thread; the flag says the
/// memory is known zero.
#[cfg(target_os = "linux")]
#[inline(always)]
pub fn alloc_zeroed(shared: &Shared, class: usize) -> (*mut u8, bool) {
    match heap_for_thread(shared) {
        Some(h) => h.alloc_zeroed(shared, class),
        None => shared.alloc_direct(&shared.arena0, class),
    }
}

/// `free` of a non-null pointer on the calling thread. Aborts unless `p`
/// is an allocated slab slot or a live huge run.
#[cfg(target_os = "linux")]
#[inline(always)]
pub fn free(shared: &Shared, p: *mut u8) {
    match heap_for_thread(shared) {
        Some(h) => h.free(shared, p),
        None => free_direct(shared, p),
    }
}

/// `free` of a non-null pointer straight to its slab bitmap or the huge
/// allocator, through no cache (`MALLOCX_TCACHE_NONE`, or a thread without
/// a heap). Counted in the thread's statistics when it has a heap.
#[cfg(target_os = "linux")]
pub fn free_direct(shared: &Shared, p: *mut u8) {
    match shared.classify_free(p) {
        Ok((class, arena)) => {
            shared.free_direct(arena, class, p);
            if let Some(h) = existing_heap() {
                h.count_direct_free(class);
            }
        }
        Err(page) => {
            let n = shared.free_huge(page);
            if let Some(h) = existing_heap() {
                h.uncached.deallocated += n as u64;
            }
        }
    }
}

#[cold]
#[inline(never)]
fn invalid_free(p: usize) -> ! {
    panic!("invalid free of {p:#x}: not an allocation")
}

#[cold]
#[inline(never)]
fn no_such_arena(index: u16) -> ! {
    panic!("arena {index} does not exist")
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::ClassStats;
    use crate::layout::{LG_MAX_SLAB, MAX_SLAB, Mapping, class_of};
    use crate::slab::State;
    use std::boxed::Box;
    use std::collections::HashSet;
    use std::mem::MaybeUninit;
    use std::vec::Vec;

    const PAGE: usize = 4096;
    /// Default capacity of a cache for a class up to 128 bytes, and the
    /// refill/flush batch that goes with it.
    const CAP: usize = MAX_CAP as usize;
    const HALF: usize = CAP / 2;
    const LG_REGION: u32 = LG_MAX_SLAB;
    const LG_STRIDE: u32 = LG_REGION + 2;
    const META_LEN: usize = 16 << 20;

    fn align_up(x: usize, a: usize) -> usize {
        (x + a - 1) & !(a - 1)
    }

    /// 64 MiB huge region; on Linux `shared()` reserves it for real.
    const HUGE_LEN: usize = 64 << 20;

    fn regions_at(superblock: usize, meta: (usize, usize), huge_base: usize) -> Regions {
        let sb_len = (NCLASSES << LG_STRIDE) + MAX_SLAB;
        let huge = (huge_base, HUGE_LEN);
        Regions::from_parts(
            superblock,
            LG_REGION,
            LG_STRIDE,
            &[0usize; NCLASSES],
            huge,
            meta,
            PAGE.trailing_zeros(),
            [
                Mapping { addr: superblock, len: sb_len },
                Mapping { addr: huge.0, len: huge.1 },
                Mapping { addr: meta.0, len: meta.1 },
            ],
        )
    }

    /// A `Shared` over a synthetic 4 MiB-per-class layout. Leaked on
    /// purpose: on Linux the test thread's own heap is torn down by the
    /// key destructor after the test body has returned. On Linux the
    /// regions are real `PROT_NONE` reservations so slot memory can be
    /// touched; elsewhere the metadata is a `Vec` and slot addresses are
    /// never dereferenced.
    fn shared() -> &'static Shared {
        shared_with(Config::DEFAULT)
    }

    fn shared_with(config: Config) -> &'static Shared {
        #[cfg(target_os = "linux")]
        let (sb, mb, hb) = {
            use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
            let reserve = |len: usize| -> usize {
                unsafe {
                    crate::sys::mmap(0, len, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0)
                }
                .expect("reserve")
            };
            (reserve((NCLASSES << LG_STRIDE) + MAX_SLAB), reserve(META_LEN), reserve(HUGE_LEN))
        };
        #[cfg(not(target_os = "linux"))]
        let (sb, mb, hb) = {
            let mut v: Vec<u8> = Vec::new();
            v.resize(META_LEN + PAGE, 0);
            let buf: &'static mut [u8] = Box::leak(v.into_boxed_slice());
            (0x1000_0000_0000usize, buf.as_ptr() as usize, 0x2000_0000_1000usize)
        };
        let regions = regions_at(align_up(sb, MAX_SLAB), (align_up(mb, PAGE), META_LEN), hb);
        let b: &'static mut MaybeUninit<Shared> = Box::leak(Box::new(MaybeUninit::uninit()));
        assert!(unsafe { Shared::init(b.as_mut_ptr(), regions, config, false) });
        unsafe { b.assume_init_ref() }
    }

    fn class_with(size: usize) -> usize {
        CLASSES.iter().position(|c| c.size() == size).unwrap()
    }

    struct TestHeap {
        h: *mut Heap,
        s: &'static Shared,
    }

    impl TestHeap {
        fn new(s: &'static Shared) -> TestHeap {
            let h = Heap::create(s);
            assert!(!h.is_null());
            TestHeap { h, s }
        }

        fn heap(&self) -> &mut Heap {
            unsafe { &mut *self.h }
        }

        fn alloc(&self, c: usize) -> *mut u8 {
            let p = self.heap().alloc(self.s, c);
            assert!(!p.is_null());
            p
        }

        fn alloc_zeroed(&self, c: usize) -> (*mut u8, bool) {
            let (p, z) = self.heap().alloc_zeroed(self.s, c);
            assert!(!p.is_null());
            (p, z)
        }

        fn free(&self, p: *mut u8) {
            self.heap().free(self.s, p)
        }

        fn alloc_huge(&self, size: usize, align: usize) -> (*mut u8, bool) {
            self.heap().alloc_huge(self.s, size, align)
        }

        fn stats(&self) -> ThreadStats {
            self.heap().stats()
        }

        fn gc(&self) {
            self.heap().gc(self.s)
        }

        fn flush_all(&self) {
            self.heap().flush_all(self.s)
        }

        fn ticks(&self) -> u32 {
            self.heap().slow_ticks()
        }

        fn cache(&self, c: usize) -> &ClassCache {
            self.heap().cache(c).expect("cache exists")
        }

        fn len(&self, c: usize) -> usize {
            self.cache(c).len()
        }

        fn destroy(self) {
            unsafe { Heap::destroy(self.h, self.s) }
        }
    }

    /// Sum of `live` over every committed slab of a class.
    fn live(s: &Shared, c: usize) -> usize {
        let cs = &s.arena0.classes[c];
        (0..cs.committed_slabs()).filter(|&i| !cs.is_guard_slab(i)).map(|i| cs.desc(i).live()).sum()
    }

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn default_caps_follow_the_design_table() {
        for (c, cls) in CLASSES.iter().enumerate() {
            let cap = default_cap(c);
            let expect = match cls.size() {
                0..=128 => 1024,
                129..=256 => 512,
                257..=512 => 256,
                513..=1024 => 128,
                1025..=2048 => 64,
                2049..=4096 => 32,
                4097..=8192 => 16,
                _ => 8,
            };
            assert_eq!(cap, expect, "class {c} size {}", cls.size());
            assert!(cap >= MIN_CAP && cap <= MAX_CAP);
            // A cache may hold more than one slab's worth (a 64 KiB slab of
            // 128-byte slots has 512); refills and flushes span slabs.
            let l = ClassCache::layout(cap);
            assert_eq!(l.size(), CACHE_HEADER + 8 * cap as usize);
            assert_eq!(l.align(), 16);
        }
        assert_eq!(core::mem::size_of::<ClassCache>(), CACHE_HEADER);
        assert!(core::mem::size_of::<Heap>() <= 512, "heap is {} bytes", core::mem::size_of::<Heap>());
    }

    #[test]
    fn heap_starts_with_the_sentinel_and_allocates_caches_lazily() {
        let s = shared();
        let before = s.meta.stats().allocated;
        let t = TestHeap::new(s);
        for c in 0..NCLASSES {
            assert!(t.heap().cache(c).is_none());
        }
        assert_eq!(t.heap().stats(), ThreadStats::default());
        assert_eq!(t.heap().arena(), 0);
        let c = class_with(64);
        let p = t.alloc(c);
        let cache = t.cache(c);
        assert_eq!(cache.cap(), CAP);
        // A refill claims half the capacity and the pop takes the lowest.
        assert_eq!(cache.len(), HALF - 1);
        assert_eq!(p as usize, s.regions.slab_addr(c, 0));
        assert!(t.heap().cache(c + 1).is_none());
        assert_eq!(t.heap().stats().allocated, 64);
        assert_eq!(t.heap().slow_ticks(), 1);
        // Only the heap and one cache came from the metadata region.
        let meta_used = s.meta.stats().allocated - before;
        assert!(meta_used >= core::mem::size_of::<Heap>() + CACHE_HEADER + 8 * CAP);
        assert!(meta_used <= 512 + 12288);
        t.free(p);
        t.destroy();
        assert_eq!(s.meta.stats().allocated, before);
        assert_eq!(live(s, c), 0);
    }

    #[test]
    fn allocations_are_address_ordered_and_lifo_after_free() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(16);
        let size = 16;
        let base = s.regions.slab_addr(c, 0);
        let n = 2 * HALF + 44;
        let mut ps = Vec::new();
        for i in 0..n {
            let p = t.alloc(c);
            assert_eq!(p as usize, base + i * size, "allocation {i}");
            ps.push(p);
        }
        // Three refills of HALF (the third partially consumed).
        assert_eq!(t.heap().slow_ticks(), 3);
        assert_eq!(t.cache(c).len(), 3 * HALF - n);
        assert_eq!(live(s, c), 3 * HALF);
        // Frees push, the most recent free is reused first.
        t.free(ps[10]);
        t.free(ps[20]);
        assert_eq!(t.alloc(c), ps[20]);
        assert_eq!(t.alloc(c), ps[10]);
        assert_eq!(t.heap().stats(), ThreadStats { allocated: (n as u64 + 2) * 16, deallocated: 2 * 16 });
        t.destroy();
        assert_eq!(live(s, c), n);
    }

    #[test]
    fn full_cache_flushes_the_oldest_half_sorted_and_slabs_empty() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let cs = &s.arena0.classes[c];
        let n = 3000;
        let ps: Vec<*mut u8> = (0..n).map(|_| t.alloc(c)).collect();
        assert_eq!(live(s, c), align_up(n, HALF));
        // Free in a scrambled order so flushes see unsorted batches.
        let mut order: Vec<usize> = (0..n).collect();
        let mut st = 0x1234_5678u64;
        for i in (1..n).rev() {
            let j = (xorshift(&mut st) % (i as u64 + 1)) as usize;
            order.swap(i, j);
        }
        let mut flushes = 0;
        for (k, &i) in order.iter().enumerate() {
            let before = t.cache(c).len();
            t.free(ps[i]);
            let after = t.cache(c).len();
            if after != before + 1 {
                // A flush released the oldest HALF and pushed one.
                assert_eq!(before, CAP);
                assert_eq!(after, HALF + 1);
                flushes += 1;
            }
            assert!(t.cache(c).len() <= CAP, "free {k}");
        }
        assert!(flushes >= 2, "{flushes} flushes");
        // Nothing is live in the program, so `live` is exactly what is cached
        // (the never-handed-out tail of the last refill was flushed too).
        assert_eq!(live(s, c), t.cache(c).len());
        // Everything back: the cache empties, slabs go Empty.
        t.flush_all();
        assert_eq!(t.cache(c).len(), 0);
        assert_eq!(live(s, c), 0);
        for i in 0..cs.committed_slabs() {
            assert_eq!(cs.desc(i).state(), State::Empty, "slab {i}");
        }
        let st = cs.stats();
        assert_eq!(st.empty, cs.committed_slabs());
        assert_eq!(st.nonfull, 0);
        // Allocating again reuses the lowest slab, now Empty -> Active.
        let p = t.alloc(c);
        assert_eq!(p as usize, s.regions.slab_addr(c, 0));
        assert_eq!(cs.desc(0).state(), State::Active);
        t.free(p);
        t.destroy();
        assert_eq!(cs.stats(), ClassStats { committed: cs.committed_slabs(), empty: cs.committed_slabs(), ..Default::default() });
    }

    #[test]
    fn zeroed_prefix_tracks_fresh_memory() {
        let s = shared();
        let t = TestHeap::new(s);
        // 4096 slots per slab, so refills after the first come from the
        // same, by then Active, slab.
        let c = class_with(16);
        // First refill comes from a Fresh slab: the whole batch is zero.
        // Pops do not shrink the prefix (the popped index is above `len`).
        let (p1, z1) = t.alloc_zeroed(c);
        assert!(z1);
        assert_eq!(t.len(c), HALF - 1);
        assert_eq!(t.cache(c).zeroed(), HALF);
        let (p2, z2) = t.alloc_zeroed(c);
        assert!(z2);
        // A free pushes dirty memory on top: the prefix ends below it.
        // Popping it is not zero, the entry below it still is.
        t.free(p2);
        assert_eq!(t.cache(c).zeroed(), HALF - 2);
        let (p3, z3) = t.alloc_zeroed(c);
        assert_eq!(p3, p2);
        assert!(!z3);
        let (_, z4) = t.alloc_zeroed(c);
        assert!(z4);
        // Pushing below the prefix cuts it there.
        for _ in 0..50 {
            t.alloc(c);
        }
        t.free(p1);
        assert_eq!(t.cache(c).zeroed(), t.len(c) - 1);
        // Drain the cache: the next refill is from an Active slab, so the
        // conservative answer is "not zero" for the whole batch.
        while t.len(c) > 0 {
            t.alloc(c);
        }
        let (_, z5) = t.alloc_zeroed(c);
        assert!(!z5);
        assert_eq!(t.cache(c).zeroed(), 0);
        t.destroy();
    }

    #[test]
    fn flush_drops_the_zeroed_prefix_with_the_oldest_entries() {
        let s = shared();
        let t = TestHeap::new(s);
        let u = TestHeap::new(s);
        let c = class_with(32);
        // Zero prefix of HALF - 28 with 28 dirty pushes above it.
        let ps: Vec<*mut u8> = (0..28).map(|_| t.alloc(c)).collect();
        assert_eq!(t.len(c), HALF - 28);
        for &p in &ps {
            t.free(p);
        }
        assert_eq!(t.len(c), HALF);
        assert_eq!(t.cache(c).zeroed(), HALF - 28);
        // Fill the rest of the cache with pointers another heap allocated
        // (dirty by definition), then overflow it by one.
        let qs: Vec<*mut u8> = (0..=HALF).map(|_| u.alloc(c)).collect();
        for &q in &qs[..HALF] {
            t.free(q);
        }
        assert_eq!(t.len(c), CAP);
        assert_eq!(t.cache(c).zeroed(), HALF - 28);
        let live_before = live(s, c);
        t.free(qs[HALF]);
        // The oldest HALF went back to the slab, taking the whole prefix.
        assert_eq!(t.len(c), HALF + 1);
        assert_eq!(t.cache(c).zeroed(), 0);
        assert_eq!(live(s, c), live_before - HALF);
        t.destroy();
        u.destroy();
        assert_eq!(live(s, c), 0);
    }

    #[test]
    fn gc_releases_unused_entries_by_low_water() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(48);
        // A refill resets low water to zero, so the first GC after a refill
        // flushes nothing and just restarts tracking at the current length.
        t.alloc(c);
        assert_eq!(t.len(c), HALF - 1);
        assert_eq!(t.cache(c).low_water(), 0);
        t.gc();
        assert_eq!(t.len(c), HALF - 1);
        assert_eq!(t.cache(c).low_water(), HALF - 1);
        // Pops lower it; pushes do not raise it.
        for _ in 0..27 {
            t.alloc(c);
        }
        let len = HALF - 28;
        assert_eq!(t.cache(c).low_water(), len);
        let extra: Vec<*mut u8> = (0..10).map(|_| t.alloc(c)).collect();
        let lw = len - 10;
        assert_eq!(t.cache(c).low_water(), lw);
        for &p in &extra {
            t.free(p);
        }
        assert_eq!(t.len(c), len);
        assert_eq!(t.cache(c).low_water(), lw);
        // GC releases three quarters of the low water.
        let live_before = live(s, c);
        t.gc();
        let flushed = lw - lw / 4;
        assert_eq!(t.len(c), len - flushed);
        assert_eq!(live(s, c), live_before - flushed);
        assert_eq!(t.cache(c).low_water(), len - flushed);
        // Untouched since, so the next GC shrinks it again.
        let len = t.len(c);
        t.gc();
        assert_eq!(t.len(c), len - (len - len / 4));
        // A cache that was drained and refilled is left alone.
        while t.len(c) > 0 {
            t.alloc(c);
        }
        t.alloc(c);
        assert_eq!(t.cache(c).low_water(), 0);
        let len = t.len(c);
        t.gc();
        assert_eq!(t.len(c), len);
        assert_eq!(t.cache(c).low_water(), len);
        t.destroy();
    }

    #[test]
    fn periodic_work_runs_gc_on_the_interval() {
        // Every slow path is a tick and every 64th tick checks the clock.
        // With a zero interval that means GC on ticks 64, 128, ...; with the
        // default 10 s it means no GC in a test.
        let c = class_with(16);
        let d = class_with(48);
        for interval in [0u64, DEFAULT_GC_INTERVAL_NS] {
            let s = shared();
            // SAFETY: nothing else references `s` yet; the field is plain data.
            unsafe { (*(s as *const Shared as *mut Shared)).gc_interval_ns = interval };
            let t = TestHeap::new(s);
            // Prime class d: one refill, HALF - 1 cached, low water 0.
            t.alloc(d);
            assert_eq!(t.ticks(), 1);
            // Drive the tick counter with class c refills.
            let mut ps = Vec::new();
            while t.ticks() < 64 {
                ps.push(t.alloc(c));
            }
            assert_eq!(t.ticks(), 64);
            assert_eq!(t.len(d), HALF - 1);
            // GC 1 flushed nothing (low water 0) but restarted tracking.
            let expect_lw = if interval == 0 { HALF - 1 } else { 0 };
            assert_eq!(t.cache(d).low_water(), expect_lw);
            for _ in 0..27 {
                t.alloc(d);
            }
            let lw = HALF - 28;
            assert_eq!(t.len(d), lw);
            while t.ticks() < 128 {
                ps.push(t.alloc(c));
            }
            // GC 2 released three quarters of d's low water if it ran.
            let live_d = live(s, d);
            let flushed = lw - lw / 4;
            if interval == 0 {
                assert_eq!(t.len(d), lw - flushed);
                assert_eq!(t.cache(d).low_water(), lw - flushed);
                assert_eq!(live_d, HALF - flushed);
            } else {
                assert_eq!(t.len(d), lw);
                assert_eq!(t.cache(d).low_water(), 0);
                assert_eq!(live_d, HALF);
            }
            t.destroy();
            assert_eq!(live(s, d), 28);
            assert_eq!(live(s, c), ps.len());
        }
    }

    #[test]
    fn direct_paths_bypass_the_cache() {
        let s = shared();
        let c = class_with(256);
        let (p, zero) = s.alloc_direct(&s.arena0, c);
        assert!(!p.is_null() && zero);
        assert_eq!(p as usize, s.regions.slab_addr(c, 0));
        assert_eq!(live(s, c), 1);
        let (q, zero) = s.alloc_direct(&s.arena0, c);
        assert_eq!(q as usize, p as usize + 256);
        assert!(!zero);
        s.free_direct(0, c, p);
        assert_eq!(live(s, c), 1);
        s.free_direct(0, c, q);
        assert_eq!(live(s, c), 0);
        assert_eq!(s.arena0.classes[c].desc(0).state(), State::Empty);
    }

    #[test]
    fn cross_thread_frees_land_in_the_freeing_heap() {
        let s = shared();
        let a = TestHeap::new(s);
        let b = TestHeap::new(s);
        let c = class_with(64);
        let ps: Vec<*mut u8> = (0..200).map(|_| a.alloc(c)).collect();
        for &p in &ps {
            b.free(p);
        }
        // 200 pushes fit in B's cache of CAP; A did one refill of HALF.
        assert_eq!(b.cache(c).len(), 200);
        assert_eq!(a.cache(c).len(), HALF - 200);
        assert_eq!(b.heap().stats(), ThreadStats { allocated: 0, deallocated: 200 * 64 });
        // B reuses what it freed, lowest address first after its flushes.
        let q = b.alloc(c);
        assert_eq!(q, ps[199]);
        b.free(q);
        a.destroy();
        b.destroy();
        assert_eq!(live(s, c), 0);
    }

    #[test]
    fn randomised_model_check() {
        let s = shared();
        let heaps: Vec<TestHeap> = (0..3).map(|_| TestHeap::new(s)).collect();
        let classes = [class_with(16), class_with(48), class_with(64), class_with(256), class_with(4096)];
        let mut model: HashSet<usize> = HashSet::new();
        let mut order: Vec<(usize, usize)> = Vec::new();
        let mut st = 0xdead_beef_cafe_f00du64;
        for step in 0..60_000 {
            let r = xorshift(&mut st);
            let t = &heaps[(r >> 8) as usize % heaps.len()];
            if model.len() < 2000 && (r & 3 != 0 || model.is_empty()) {
                let ci = (r >> 16) as usize % classes.len();
                let c = classes[ci];
                let p = t.alloc(c) as usize;
                assert!(model.insert(p), "step {step}: duplicate live pointer {p:#x}");
                assert_eq!(class_of(CLASSES[c].size()), Some(c));
                assert!(matches!(s.regions.classify(p), Kind::Slab { class, .. } if class as usize == c));
                order.push((p, c));
            } else {
                let i = (r >> 20) as usize % order.len();
                let (p, _) = order.swap_remove(i);
                assert!(model.remove(&p));
                t.free(p as *mut u8);
            }
            if step % 5000 == 0 {
                // Invariant: live == program-live + cached, per class.
                for &c in &classes {
                    let prog = order.iter().filter(|&&(_, cc)| cc == c).count();
                    let cached: usize = heaps.iter().map(|h| h.heap().cache(c).map_or(0, |k| k.len())).sum();
                    assert_eq!(live(s, c), prog + cached, "step {step} class {c}");
                }
            }
        }
        for t in heaps {
            t.destroy();
        }
        for &c in &classes {
            let prog = order.iter().filter(|&&(_, cc)| cc == c).count();
            assert_eq!(live(s, c), prog);
        }
    }

    #[test]
    #[should_panic(expected = "not an allocation")]
    fn free_of_a_pointer_outside_every_region_aborts() {
        let s = shared();
        let t = TestHeap::new(s);
        let x = 0u64;
        t.free(&x as *const u64 as *mut u8);
    }

    #[test]
    #[should_panic(expected = "not an allocation")]
    fn free_of_a_misaligned_pointer_aborts() {
        let s = shared();
        let t = TestHeap::new(s);
        let p = t.alloc(class_with(64));
        t.free(unsafe { p.add(8) });
    }

    #[test]
    #[should_panic(expected = "invalid or double free")]
    fn free_of_a_never_allocated_slot_aborts() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let p = t.alloc(c);
        // A slot of slab 0 above the first refill's HALF is committed but
        // was never claimed (the 64-byte slab has 1024 slots).
        t.free(unsafe { p.add((HALF + 200) * 64) });
    }

    #[test]
    #[should_panic(expected = "was never allocated")]
    fn free_into_an_uncommitted_slab_aborts() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let _ = t.alloc(c);
        t.free(s.regions.slab_addr(c, 5) as *mut u8);
    }

    #[test]
    #[should_panic(expected = "invalid or double free")]
    fn stale_double_free_aborts_at_the_check() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let p = t.alloc(c);
        t.free(p);
        t.flush_all();
        t.free(p);
    }

    #[test]
    #[should_panic(expected = "double free of")]
    fn cached_double_free_aborts_at_the_flush() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let p = t.alloc(c);
        t.free(p);
        // The bit is still set while `p` is cached, so the check passes
        // and the duplicate is caught when the batch is released.
        t.free(p);
        t.flush_all();
    }

    #[test]
    #[should_panic(expected = "not the start of a huge allocation")]
    fn free_of_an_unallocated_huge_pointer_aborts() {
        let s = shared();
        let t = TestHeap::new(s);
        t.free(s.regions.huge_base as *mut u8);
    }

    #[test]
    fn huge_runs_go_through_the_heap_without_caches() {
        let s = shared();
        let t = TestHeap::new(s);
        let size = 300 * 1024;
        let (p, zero) = t.alloc_huge(size, 16);
        assert!(!p.is_null() && zero);
        assert_eq!(s.regions.classify(p as usize), Kind::Huge { page: 0 });
        assert_eq!(s.huge.usable_size(0), s.huge.usable_for(size).unwrap());
        assert_eq!(t.stats().allocated, s.huge.usable_for(size).unwrap() as u64);
        let (q, _) = t.alloc_huge(size, 1 << 20);
        assert_eq!(q as usize % (1 << 20), 0);
        // Frees route by region: a huge free never touches a cache.
        t.free(p);
        assert_eq!(t.stats().deallocated, s.huge.usable_for(size).unwrap() as u64);
        assert!((0..NCLASSES).all(|c| t.heap().cache(c).is_none()));
        assert_eq!(s.huge.stats().runs, 1);
        // The heap-less path frees huge runs too.
        s.free_huge(match s.regions.classify(q as usize) {
            Kind::Huge { page } => page,
            other => panic!("{other:?}"),
        });
        assert_eq!(s.huge.stats().runs, 0);
        assert!(s.alloc_huge(usize::MAX, 1, 0).0.is_null());
    }
    /// The registry: arena 0 is `arena0`, automatic arenas are 0 alone by
    /// default, explicit ones come from `create` in order and are built in
    /// the metadata region with their own class states over the shared
    /// tables; unknown indices are `None`.
    #[test]
    fn arena_registry_creates_and_looks_up() {
        let s = shared();
        assert_eq!(s.arenas.count(), 1);
        assert_eq!(s.arenas.auto(), 1);
        assert!(!s.multi_arena());
        assert!(core::ptr::eq(s.arena(0).unwrap(), &s.arena0));
        assert!(s.arena(1).is_none());
        assert!(s.arena(4095).is_none());
        assert!(s.arenas.get_existing(1).is_none());
        for _ in 0..8 {
            assert_eq!(s.arenas.assign(), 0);
        }
        let a1 = s.arenas.create(s).unwrap();
        let a2 = s.arenas.create(s).unwrap();
        assert_eq!((a1, a2), (1, 2));
        assert_eq!(s.arenas.count(), 3);
        assert!(s.multi_arena());
        let a = s.arena(1).unwrap();
        assert_eq!(a.index(), 1);
        assert!(s.meta.contains(a as *const Arena as usize));
        assert!(core::ptr::eq(a.class(3).table(), s.table(3)));
        assert_eq!(a.class(3).arena(), 1);
        assert_eq!(a.decay_ns(), s.decay_ns());
        assert!(s.arena(3).is_none());
        let seen: Vec<u16> = s.arenas.iter().map(|a| a.index()).collect();
        assert_eq!(seen, [0, 1, 2]);
        // Still assigning 0: created arenas are explicit, not automatic.
        assert_eq!(s.arenas.assign(), 0);
        a.set_decay_ms(-1);
        assert_eq!(a.decay_ns(), crate::purge::NEVER);
        assert_ne!(s.decay_ns(), crate::purge::NEVER);
        s.set_decay_ms(5);
        assert_eq!(a.decay_ns(), 5_000_000);
        assert_eq!(s.decay_ns(), 5_000_000);
    }

    /// `narenas:3` gives three automatic arenas assigned round robin to new
    /// heaps and built on first touch.
    #[test]
    fn automatic_arenas_are_assigned_round_robin_and_built_lazily() {
        let s = shared_with(Config { narenas: 3, ..Config::DEFAULT });
        assert_eq!(s.arenas.count(), 3);
        assert_eq!(s.arenas.auto(), 3);
        assert!(s.arenas.get_existing(1).is_none());
        let homes: Vec<u16> = (0..6).map(|_| TestHeap::new(s).heap().arena()).collect();
        assert_eq!(homes, [1, 2, 0, 1, 2, 0]);
        assert!(s.arenas.get_existing(1).is_none(), "assignment alone must not build");
        assert!(s.arenas.create(s).is_some_and(|i| i == 3));
        let t = TestHeap::new(s);
        let home = t.heap().arena();
        assert_eq!(home, 1);
        let c = class_with(64);
        let p = t.alloc(c);
        assert!(s.arenas.get_existing(1).is_some());
        assert!(s.multi_arena());
        assert_eq!(s.arena0.classes[c].desc(0).arena(), 1);
        assert_eq!(s.classify_free(p), Ok((c, 1)));
        t.heap().free(s, p);
        assert_eq!(t.heap().cache(c).unwrap().len(), HALF);
    }

    /// A heap keeps one cache per (arena, class): allocations from another
    /// arena come from and go back to that arena's slabs through a cache on
    /// the `others` list, plain `free` routes by the slab's owner, and the
    /// fast path refuses another arena's slot.
    #[test]
    fn caches_are_keyed_by_arena() {
        let s = shared();
        let a1 = s.arenas.create(s).unwrap();
        let t = TestHeap::new(s);
        let h = t.heap();
        assert_eq!(h.arena(), 0);
        let c = class_with(64);
        let p0 = t.alloc(c);
        let (p1, zero) = h.alloc_in(s, a1, c);
        assert!(!p1.is_null() && zero);
        assert_eq!(s.classify_free(p0), Ok((c, 0)));
        assert_eq!(s.classify_free(p1), Ok((c, a1)));
        assert_ne!(s.arena0.classes[c].locate(p0).0, s.arena0.classes[c].locate(p1).0);
        assert_eq!(h.cache(c).unwrap().len(), HALF - 1);
        assert_eq!(h.cache_in(a1, c).unwrap().len(), HALF - 1);
        assert_eq!(s.arena(a1).unwrap().classes[c].stats().committed, 1);
        assert_eq!(s.arena0.classes[c].stats().committed, 1);
        // Non-existent arena: null, nothing touched.
        assert!(h.alloc_in(s, 77, c).0.is_null());
        // The fast path takes only the home arena's slots.
        assert!(free_fast(s, h, p0));
        assert!(!free_fast(s, h, p1));
        assert_eq!(h.cache(c).unwrap().len(), HALF);
        h.free(s, p1);
        assert_eq!(h.cache_in(a1, c).unwrap().len(), HALF);
        // The next allocation from arena 1 is the LIFO top of its cache.
        assert_eq!(h.alloc_in(s, a1, c).0, p1);
        h.free(s, p1);
        // Byte counters cover both arenas.
        let st = h.stats();
        assert_eq!(st.allocated, 3 * 64);
        assert_eq!(st.deallocated, 3 * 64);
        // Flushing returns everything to the right owner.
        h.flush_all(s);
        assert_eq!(h.cache(c).unwrap().len(), 0);
        assert_eq!(h.cache_in(a1, c).unwrap().len(), 0);
        assert_eq!(s.arena0.classes[c].stats().empty, 1);
        assert_eq!(s.arena(a1).unwrap().classes[c].stats().empty, 1);
        // Direct paths against an explicit arena.
        let a = s.arena(a1).unwrap();
        let (d, _) = s.alloc_direct(a, c);
        assert_eq!(s.classify_free(d), Ok((c, a1)));
        assert_eq!(a.classes[c].desc(s.arena0.classes[c].locate(d).0).live(), 1);
        s.free_direct(a1, c, d);
        assert_eq!(a.classes[c].stats().empty, 1);
        // Destroying the heap frees the other-arena caches too.
        let before = s.meta.stats().allocated;
        unsafe { Heap::destroy(t.h, s) };
        assert!(s.meta.stats().allocated < before);
        core::mem::forget(t);
    }

    /// `thread.arena`: the home arena changes without flushing; the old
    /// home's caches move to the list and back.
    #[test]
    fn set_arena_swaps_cache_arrays() {
        let s = shared();
        let a1 = s.arenas.create(s).unwrap();
        let t = TestHeap::new(s);
        let h = t.heap();
        let c = class_with(64);
        let p0 = t.alloc(c);
        let (p1, _) = h.alloc_in(s, a1, c);
        h.set_arena(s, a1);
        assert_eq!(h.arena(), a1);
        assert_eq!(h.cache(c).unwrap().len(), HALF - 1);
        assert_eq!(h.cache_in(0, c).unwrap().len(), HALF - 1);
        // Plain allocation now comes from arena 1's cache.
        let q = t.alloc(c);
        assert_eq!(s.classify_free(q), Ok((c, a1)));
        assert!(free_fast(s, h, p1));
        assert!(!free_fast(s, h, p0));
        h.free(s, p0);
        h.free(s, q);
        assert_eq!(h.cache(c).unwrap().len(), HALF);
        assert_eq!(h.cache_in(0, c).unwrap().len(), HALF);
        h.set_arena(s, 0);
        assert_eq!(h.cache(c).unwrap().len(), HALF);
        assert_eq!(h.cache_in(a1, c).unwrap().len(), HALF);
        h.flush_all(s);
        assert_eq!(s.arena0.classes[c].stats().empty, 1);
        assert_eq!(s.arena(a1).unwrap().classes[c].stats().empty, 1);
    }

    /// Explicit caches are heaps owned by no thread with their own ids.
    #[test]
    fn explicit_caches_create_flush_destroy() {
        let s = shared();
        assert!(s.tcaches.is_empty());
        let t0 = s.tcaches.create(s, 0).unwrap();
        let t1 = s.tcaches.create(s, 0).unwrap();
        assert_eq!((t0, t1), (0, 1));
        assert_eq!(s.tcaches.len(), 2);
        assert!(s.tcaches.get(2).is_none());
        assert!(s.tcaches.get(u16::MAX).is_none());
        let c = class_with(64);
        let h = s.tcaches.get(t0).unwrap();
        let (p, _) = h.alloc_in(s, 0, c);
        assert!(!p.is_null());
        assert_eq!(h.cache(c).unwrap().len(), HALF - 1);
        h.free(s, p);
        assert_eq!(h.cache(c).unwrap().len(), HALF);
        assert!(s.tcaches.flush(s, t0));
        assert_eq!(s.tcaches.get(t0).unwrap().cache(c).unwrap().len(), 0);
        assert!(!s.tcaches.flush(s, 9));
        assert!(s.tcaches.destroy(s, t0));
        assert!(!s.tcaches.destroy(s, t0));
        assert!(s.tcaches.get(t0).is_none());
        assert_eq!(s.tcaches.len(), 1);
        // The slot is reused.
        assert_eq!(s.tcaches.create(s, 0), Some(t0));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use crate::layout::class_of;
    use crate::tls;
    use std::boxed::Box;
    use std::mem::MaybeUninit;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::vec::Vec;

    const PAGE: usize = 4096;
    const LG_REGION: u32 = crate::layout::LG_MAX_SLAB;
    const LG_STRIDE: u32 = LG_REGION + 2;
    const META_LEN: usize = 16 << 20;

    fn shared() -> &'static Shared {
        use crate::layout::{MAX_SLAB, Mapping};
        use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
        let reserve = |len: usize| -> usize {
            unsafe { crate::sys::mmap(0, len, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0) }
                .expect("reserve")
        };
        let sb_len = (NCLASSES << LG_STRIDE) + MAX_SLAB;
        let sb = reserve(sb_len);
        let mb = reserve(META_LEN);
        let superblock = (sb + MAX_SLAB - 1) & !(MAX_SLAB - 1);
        let huge = (reserve(64 << 20), 64usize << 20);
        let regions = Regions::from_parts(
            superblock,
            LG_REGION,
            LG_STRIDE,
            &[0usize; NCLASSES],
            huge,
            (mb, META_LEN),
            PAGE.trailing_zeros(),
            [
                Mapping { addr: superblock, len: sb_len },
                Mapping { addr: huge.0, len: huge.1 },
                Mapping { addr: mb, len: META_LEN },
            ],
        );
        let b: &'static mut MaybeUninit<Shared> = Box::leak(Box::new(MaybeUninit::uninit()));
        assert!(unsafe { Shared::init(b.as_mut_ptr(), regions, Config::DEFAULT, false) });
        unsafe { b.assume_init_ref() }
    }

    fn live(s: &Shared, c: usize) -> usize {
        let cs = &s.arena0.classes[c];
        (0..cs.committed_slabs()).filter(|&i| !cs.is_guard_slab(i)).map(|i| cs.desc(i).live()).sum()
    }

    fn fill(p: *mut u8, n: usize, v: u8) {
        unsafe { core::ptr::write_bytes(p, v, n) }
    }

    fn check(p: *mut u8, n: usize, v: u8) {
        let s = unsafe { core::slice::from_raw_parts(p, n) };
        assert!(s.iter().all(|&b| b == v), "memory at {:#x} is not all {v:#x}", p as usize);
    }

    fn is_zero(p: *mut u8, n: usize) -> bool {
        unsafe { core::slice::from_raw_parts(p, n) }.iter().all(|&b| b == 0)
    }

    #[test]
    fn heap_is_created_on_first_use_through_tls() {
        let s = shared();
        assert!(s.has_thread_key());
        std::thread::spawn(move || {
            assert!(tls::get().is_null());
            let c = class_of(100).unwrap();
            let p = alloc(s, c);
            assert!(!p.is_null());
            let h = tls::get() as usize;
            assert!(h > TLS_EXITED);
            let heap = unsafe { &*(h as *mut Heap) };
            assert_eq!(heap.tid(), crate::sys::gettid() as u32);
            assert!(core::ptr::eq(heap.shared(), s));
            assert_eq!(heap.stats().allocated, CLASSES[c].size() as u64);
            // Same heap on the next call.
            let q = alloc(s, c);
            assert_eq!(tls::get() as usize, h);
            assert_ne!(p, q);
            fill(p, CLASSES[c].size(), 0xa5);
            fill(q, CLASSES[c].size(), 0x5a);
            check(p, CLASSES[c].size(), 0xa5);
            free(s, p);
            free(s, q);
            assert_eq!(unsafe { &*(h as *mut Heap) }.stats().deallocated, 2 * CLASSES[c].size() as u64);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn calloc_flag_is_honest_about_memory() {
        let s = shared();
        std::thread::spawn(move || {
            let c = class_of(200).unwrap();
            let size = CLASSES[c].size();
            let (p, zero) = alloc_zeroed(s, c);
            assert!(zero && is_zero(p, size));
            fill(p, size, 0xff);
            free(s, p);
            let (q, zero) = alloc_zeroed(s, c);
            assert_eq!(p, q);
            assert!(!zero && !is_zero(q, size));
            // Everything the flag calls zero really is. The flag is
            // conservative: only a refill batch that came entirely from a
            // `Fresh` or `Purged` slab is flagged, so here that is the rest
            // of the very first batch (the first slab's later batches come
            // from an `Active` slab, and the batch that crosses into the
            // next fresh slab is mixed).
            let mut zeros = 0;
            for _ in 0..3000 {
                let (r, zero) = alloc_zeroed(s, c);
                assert!(!r.is_null());
                if zero {
                    assert!(is_zero(r, size), "{:#x} flagged zero but is not", r as usize);
                    zeros += 1;
                }
                fill(r, size, 0x11);
            }
            let refill = default_cap(c) as usize / 2;
            assert!(zeros >= refill - 1 && zeros < 3000, "{zeros} zero-flagged of 3000");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn thread_exit_flushes_and_frees_the_heap() {
        let s = shared();
        let c = class_of(64).unwrap();
        let meta_before = s.meta.stats().allocated;
        let leaked = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for t in 0..16u8 {
            let leaked = leaked.clone();
            handles.push(std::thread::spawn(move || {
                let mut ps = Vec::new();
                for i in 0..500 {
                    let p = alloc(s, c);
                    assert!(!p.is_null());
                    fill(p, 64, t);
                    ps.push(p);
                    if i % 3 == 0 {
                        free(s, ps.swap_remove(i / 3 % ps.len()));
                    }
                }
                for &p in &ps {
                    check(p, 64, t);
                }
                // Exit with `ps` live and a non-empty cache; the live ones
                // stay allocated, the cached ones must be released.
                leaked.fetch_add(ps.len(), Ordering::Relaxed);
                assert!(tls::get() as usize > TLS_EXITED);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(live(s, c), leaked.load(Ordering::Relaxed));
        // Heaps and caches went back to the metadata allocator.
        assert_eq!(s.meta.stats().allocated, meta_before);
        assert_eq!(s.arena0.classes[c].desc(0).state(), crate::slab::State::Active);
    }

    #[test]
    fn creating_and_exited_sentinels_are_served_directly() {
        let s = shared();
        std::thread::spawn(move || {
            let c = class_of(32).unwrap();
            let meta_before = s.meta.stats().allocated;
            // Inside heap creation: no heap, served from the slab bitmap.
            tls::set(TLS_CREATING as *mut c_void);
            let p = alloc(s, c);
            assert!(!p.is_null());
            assert_eq!(tls::get() as usize, TLS_CREATING);
            assert_eq!(s.meta.stats().allocated, meta_before);
            assert_eq!(live(s, c), 1);
            free(s, p);
            assert_eq!(live(s, c), 0);
            // Real heap.
            tls::set(core::ptr::null_mut());
            let p = alloc(s, c);
            let h = tls::get();
            assert!(h as usize > TLS_EXITED);
            assert!(s.meta.stats().allocated > meta_before);
            free(s, p);
            // Run the destructor by hand (glibc clears the key's value
            // before calling it, so do the same to avoid a second run at
            // thread exit): heap gone, slot says exited, and later calls go
            // direct without creating another heap.
            let key = s.thread_key().expect("key");
            assert_eq!(unsafe { libc::pthread_getspecific(key) }, h);
            assert_eq!(unsafe { libc::pthread_setspecific(key, core::ptr::null()) }, 0);
            unsafe { heap_destructor(h) };
            assert_eq!(tls::get() as usize, TLS_EXITED);
            assert_eq!(s.meta.stats().allocated, meta_before);
            assert_eq!(live(s, c), 0);
            // The slab went `Empty` when everything was released, so its
            // memory is dirty and the flag must say so.
            let (q, zero) = alloc_zeroed(s, c);
            assert!(!q.is_null());
            assert!(!zero || is_zero(q, 32));
            assert_eq!(tls::get() as usize, TLS_EXITED);
            assert_eq!(s.meta.stats().allocated, meta_before);
            free(s, q);
            assert_eq!(live(s, c), 0);
            // The key holds no value now, so thread exit runs no destructor.
        })
        .join()
        .unwrap();
    }

    #[test]
    fn producer_consumer_across_threads_with_patterns() {
        let s = shared();
        let c = class_of(96).unwrap();
        let size = CLASSES[c].size();
        let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(64);
        let producer = std::thread::spawn(move || {
            for i in 0..20_000usize {
                let p = alloc(s, c);
                assert!(!p.is_null());
                fill(p, size, (i & 0xff) as u8);
                tx.send(p as usize).unwrap();
            }
        });
        let consumer = std::thread::spawn(move || {
            let mut n = 0usize;
            while let Ok(p) = rx.recv() {
                check(p as *mut u8, size, (n & 0xff) as u8);
                free(s, p as *mut u8);
                n += 1;
            }
            n
        });
        producer.join().unwrap();
        assert_eq!(consumer.join().unwrap(), 20_000);
        assert_eq!(live(s, c), 0);
    }

    #[test]
    fn eight_threads_hammer_disjoint_pointers() {
        let s = shared();
        let classes: Vec<usize> = [16usize, 64, 320, 2048].iter().map(|&n| class_of(n).unwrap()).collect();
        let mut handles = Vec::new();
        for t in 0..8u64 {
            let classes = classes.clone();
            handles.push(std::thread::spawn(move || {
                let mut st = 0x1234_5678_9abc_def0u64 ^ (t + 1);
                let mut held: Vec<(*mut u8, usize)> = Vec::new();
                for _ in 0..50_000 {
                    st ^= st << 13;
                    st ^= st >> 7;
                    st ^= st << 17;
                    if held.len() < 400 && (st & 3 != 0 || held.is_empty()) {
                        let c = classes[(st >> 8) as usize % classes.len()];
                        let p = alloc(s, c);
                        assert!(!p.is_null());
                        let size = CLASSES[c].size();
                        // Fresh from the kernel or zeroed by whoever freed it.
                        check(p, size, 0);
                        fill(p, size, t as u8 + 1);
                        held.push((p, size));
                    } else {
                        let i = (st >> 12) as usize % held.len();
                        let (p, size) = held.swap_remove(i);
                        check(p, size, t as u8 + 1);
                        fill(p, size, 0);
                        free(s, p);
                    }
                }
                for (p, size) in held {
                    check(p, size, t as u8 + 1);
                    fill(p, size, 0);
                    free(s, p);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        for &c in &classes {
            assert_eq!(live(s, c), 0, "class {c}");
        }
    }
}
