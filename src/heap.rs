//! Thread heaps and class caches: the fast and slow allocation paths
//! (design.md section 6).
//!
//! Every thread that allocates gets a [`Heap`] in the metadata region,
//! reached through the one TLS slot in `tls.rs`. A heap holds one
//! [`CacheHeader`] per size class inline: a LIFO stack of slot pointers that
//! are marked in their slab bitmaps but not live in the program. The fast
//! paths only touch the heap's header line for the class, the stack top and
//! (for `free`) one word of the class's slot bitmap plus, once a second
//! arena exists, one word of the group owner array; never the slab
//! descriptor, and no atomic read-modify-write, no lock.
//!
//! Layout. The headers are a
//! dense `[CacheHeader; NCLASSES]` inside the heap, one 64-byte line each,
//! so `malloc` reaches `len` at `heap + 128 + c * 64` without first loading
//! a cache pointer, and a thread's 52 headers share one page instead of 52.
//! Each header keeps a pointer to its stack of `cap` slot pointers, on the
//! same line as `len`; the stack is its own metadata object, allocated on
//! the class's first slow path, so a thread that only uses five classes
//! pays for five stacks. The alternative, one contiguous slot
//! block per heap, was built and measured
//! (`bench/results/2026-09-18-*-gce-dense-headers.md`): a stack inside a
//! 128 KiB metadata object costs xmalloc-test's freeing threads three to
//! four times the L1D fills of a stack in an 8 KiB object (the same lines,
//! the same page geometry), whatever the offset inside the object, so the
//! stacks stay separate objects.
//!
//! - `alloc`: pop the top of the class's cache; if it is empty, *refill* by
//!   claiming up to `cap / 2` slots from the class's lowest partial slab
//!   (lowest on top, so allocations walk up the slab). Every
//!   `2^tcache_compact_shift` pops are *scheduled pops* (below).
//! - `free`: classify the pointer, check its bitmap bit is set (invalid and
//!   stale double frees abort here), push it; if the cache is full, *flush*
//!   the `cap / 2` highest-address entries back to their slabs in one
//!   sorted batch and keep the lowest, in ascending order.
//!
//! Compaction (design 6.4). A LIFO cache hands
//! every allocation the slot that was just freed, so under a balanced
//! alloc/free churn (Valkey's DEL:SET steady state) the cache never
//! reaches either boundary, no refill and no flush ever run, and the slab
//! layout stays frozen at whatever occupancy the delete phase left; the
//! flush's selection policy cannot change that (`bench/probes/churn.c`).
//! To break it, one pop in
//! `2^tcache_compact_shift` is a scheduled pop that is served from a
//! *run* of low addresses kept at the bottom of the stack,
//! `slots[..run_end]`, descending (lowest at `run_end - 1`), instead of
//! the top: it returns the run's lowest slot and moves the top entry into
//! its place. A refill's batch is such a run; a rebuild claims a block of
//! `cap / 4` slots from the class's lowest partial slab and swaps it into
//! the bottom of the stack (no sort). The displaced frees sit above the run
//! until the next flush returns them, highest addresses first, so the live
//! set migrates downward at `1 / 2^k` of the allocation rate and the high
//! slabs empty and are purged. The schedule is gated: a cache *probes* the
//! class every 4096 pops and only *compacts* while the class has holes
//! worth `COMPACT_MIN_SLABS` slabs and a `COMPACT_MIN_RATIO`th of its
//! capacity (`ClassState::holes`) that ordinary refills are not consuming
//! (`COMPACT_MAX_REFILL_SLABS`), so a dense or a steadily refilled working
//! set pays one counter test per `malloc` and nothing else. The run stays intact because `run_end <=
//! low_water <= len`: a pop that dips below it clips both, a push never
//! writes below `len`.
//!
//! Every header starts with `len == cap == 0`, so the fast paths need no
//! null check: an empty cache is the slow path, a full one too, and the slow
//! path gives the header its capacity and stack on first use
//! (`ensure_cache`). A heap that cannot be created (metadata
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
use crate::huge::{GUARD_PAGES, Huge};
use crate::layout::{CLASSES, Kind, NCLASSES, Regions};
use crate::lock::Lock;
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

/// Default cache capacity for a class: [`MAX_CAP`] (1024) slots for classes
/// up to 128 bytes, halved for every doubling of the class size, never
/// below [`MIN_CAP`].
pub const fn default_cap(class: usize) -> u32 {
    crate::config::ladder(class, MAX_CAP, 1)
}

/// One thread's cache header for one size class: exactly one cache line,
/// inline in the heap ([`Heap::caches`]) or in an [`ArenaCaches`] node,
/// pointing at its stack of `cap` slot pointers inside the owner's slot
/// block. The fields the fast paths read (`len`, `cap`, `zeroed`, `slots`,
/// `allocs`, `compact_mask`, `low_water`) come first; a header whose `cap`
/// is zero has never been used (`slots` is null and never dereferenced,
/// because `len == cap == 0` sends both fast paths to the slow path).
///
/// The byte counters `thread.allocated` and `thread.deallocated` are not
/// kept per operation. Every pop bumps `allocs` on the header line the pop
/// writes anyway, and the number of pushes follows from the stack
/// arithmetic `len = (frees + refilled) - (allocs + flushed)`, with
/// `moved = refilled - flushed` maintained on the slow paths; see
/// [`Heap::stats`].
#[repr(C, align(64))]
pub struct CacheHeader {
    len: u32,
    cap: u32,
    /// Smallest `len` since the last GC (design 6.4).
    low_water: u32,
    /// `slots[..zeroed]` are known to read as zero.
    zeroed: u32,
    /// The stack: a metadata object of `cap` slot pointers
    /// ([`CacheHeader::slots_layout`]; null while `cap == 0`).
    slots: *mut *mut u8,
    /// Pops served from this cache since it was created.
    allocs: u64,
    /// A pop whose `allocs` count has no bit of this mask set is a
    /// scheduled pop: [`PROBE_MASK`], the configured compaction mask, or
    /// `u32::MAX` when compaction is off.
    compact_mask: u32,
    /// `slots[..run_end]` is the run of low addresses, descending (the
    /// lowest at `run_end - 1`); `run_end <= low_water`.
    run_end: u32,
    /// Slots moved in by refills minus slots moved out by flushes
    /// (wrapping; the true value is `len - frees + allocs`).
    moved: u64,
    /// The class's refill counter (low bits) at the previous probe: a class
    /// that ordinary refills are draining is not compacted.
    probe_refilled: u32,
    /// The arena this cache belongs to (a rebuild's claim needs it).
    arena: u16,
    /// [`MODE_OFF`], [`MODE_PROBE`] or [`MODE_COMPACT`].
    mode: u16,
}

/// Cache modes: off (`tcache_compact_shift:0`), probing the class for
/// fragmentation every [`PROBE_MASK`] + 1 pops, compacting.
pub const MODE_OFF: u16 = 0;
pub const MODE_PROBE: u16 = 1;
pub const MODE_COMPACT: u16 = 2;
/// Pops between two fragmentation probes of a class (minus one).
pub const PROBE_MASK: u32 = (1 << 12) - 1;
/// A class is worth compacting when its slabs hold at least this many
/// slabs' worth of holes (free slots not in any cache) and at least a
/// [`COMPACT_MIN_RATIO`]th of their capacity. Two or three slabs of holes
/// are what a producer/consumer pair keeps in flight between the
/// consumer's flushes and the producer's refills (xmalloc-test), which is
/// not fragmentation.
pub const COMPACT_MIN_SLABS: usize = 4;
/// See [`COMPACT_MIN_SLABS`]: holes must be at least capacity / 32.
pub const COMPACT_MIN_RATIO: usize = 32;
/// A class whose cache refills claimed this many slabs' worth of slots
/// between two probes of one cache (4096 pops apart) is being drained by
/// ordinary refills, which take the lowest partial slab anyway: its holes
/// are transient (xmalloc-test's producers run 0..8 slabs of holes through
/// a 150..450-slab class at thousands of refilled slots per window) and it
/// is not compacted. Only refills count (`ClassState::refilled`), not
/// compaction's own rebuild claims and not direct `MALLOCX_TCACHE_NONE`
/// claims: Valkey's active defragmenter allocates that way at hundreds of
/// slots per second, which at one slab per window kept the frozen residue
/// of the 2026-09-17 soak from ever being compacted. The residue itself
/// sees a refill only when a class's random-size churn walks a cache to
/// empty, a few per second.
pub const COMPACT_MAX_REFILL_SLABS: usize = 2;
// Both slab counts are in 64 KiB slabs (`Class::compact_slots`), so the
// thresholds mean the same bytes for the 4 KiB-slab classes as for the rest.

/// Size of a [`CacheHeader`]: one line, so a class's header never shares
/// a line with another class's.
pub const CACHE_HEADER: usize = core::mem::size_of::<CacheHeader>();
const _: () = assert!(CACHE_HEADER == 64);
const _: () = assert!(core::mem::offset_of!(CacheHeader, slots) < 64 - 8);

impl CacheHeader {
    /// A header that has never been used: both fast paths fall through to
    /// the slow path, which gives it a capacity and a stack.
    pub const EMPTY: CacheHeader = CacheHeader {
        len: 0,
        cap: 0,
        low_water: 0,
        zeroed: 0,
        slots: ptr::null_mut(),
        allocs: 0,
        compact_mask: u32::MAX,
        run_end: 0,
        moved: 0,
        probe_refilled: 0,
        arena: 0,
        mode: MODE_OFF,
    };

    /// Metadata layout of a stack of `cap` slots.
    pub const fn slots_layout(cap: u32) -> Layout {
        match Layout::from_size_align(8 * cap as usize, 16) {
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

    /// The run of low addresses, `slots[..run_end]`, descending (for tests
    /// and diagnostics).
    pub fn run(&self) -> &[*mut u8] {
        debug_assert!(self.run_end <= self.low_water && self.low_water <= self.len);
        if self.run_end == 0 {
            return &[];
        }
        // SAFETY: `run_end <= len <= cap` pointers exist at `slots`.
        unsafe { core::slice::from_raw_parts(self.slots, self.run_end as usize) }
    }

    /// Cached pointers, bottom of the stack first (for tests and
    /// diagnostics).
    pub fn cached(&self) -> &[*mut u8] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `len <= cap` pointers exist at `slots`.
        unsafe { core::slice::from_raw_parts(self.slots, self.len as usize) }
    }

    /// Pops served from this cache since it was created.
    #[inline]
    pub fn allocs(&self) -> u64 {
        self.allocs
    }

    /// Whether the cache is currently compacting its class.
    #[inline]
    pub fn compacting(&self) -> bool {
        self.mode == MODE_COMPACT
    }

    #[inline(always)]
    fn slots(&mut self) -> *mut *mut u8 {
        self.slots
    }

    /// Enter `mode` and derive the scheduled-pop mask from it, so the two
    /// fields never disagree: [`PROBE_MASK`] while probing, the configured
    /// `tcache_compact_shift` mask while compacting, `u32::MAX` when off
    /// (one pop in 2^32 then reaches `pop_compact`, which pops the top).
    fn set_mode(&mut self, mode: u16, config: &Config) {
        self.mode = mode;
        self.compact_mask = match mode {
            MODE_PROBE => PROBE_MASK,
            MODE_COMPACT => config.compact_mask(),
            _ => u32::MAX,
        };
    }

    /// Cached pointers, bottom of the stack first (the top is popped next).
    /// Only for a cache with a stack (`cap != 0`).
    #[inline]
    fn entries(&mut self) -> &mut [*mut u8] {
        debug_assert!(self.cap != 0);
        // SAFETY: `len <= cap` pointers exist at `slots`.
        unsafe { core::slice::from_raw_parts_mut(self.slots, self.len as usize) }
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
/// heap touches that arena and kept on a singly linked list, with the same
/// shape as the home caches (headers inline). Almost every heap has none;
/// Valkey's main thread has one (the Lua arena).
#[repr(C)]
pub struct ArenaCaches {
    arena: u16,
    /// Cache lines between the metadata block this node lives in and the
    /// node itself (`NODE_STAGGER_LINES`); `Heap::destroy` frees the block.
    pad: u8,
    next: *mut ArenaCaches,
    caches: [CacheHeader; NCLASSES],
}

/// A heap's `n`-th `others` node is placed `n % NODE_STAGGER_LINES` cache
/// lines into its metadata block, so that the node heads a cross-arena
/// `free` walks (and the stack tops it pushes to) do not share L1 sets:
/// blocks of one metadata bin are a fixed stride apart (12 KiB here), which
/// is a multiple of the L1D way size on both reference CPUs (4 KiB on the
/// Xeon, 16 KiB / 4 ways on Neoverse V2), and 15 aliasing node heads cost
/// xmalloc-test -w 16 12..25% of its throughput when the 40 KiB class's
/// foreign stack stopped being allocated between them
/// (`bench/results/2026-09-18-aarch64-gce-xmalloc-isolation.md`).
const NODE_STAGGER_LINES: usize = 16;

/// Layout of the block an `others` node is carved from.
fn node_block_layout() -> Layout {
    Layout::from_size_align(core::mem::size_of::<ArenaCaches>() + 64 * (NODE_STAGGER_LINES - 1), 64).expect("node block layout")
}

/// One thread's allocator state (design 6.2). Lives in the metadata region;
/// reached through the TLS slot. Also the shape of an explicit cache
/// (`tcache.create`), which is a heap owned by no thread. `repr(C)` so the
/// header array sits at a fixed offset the fast paths can add to the TLS
/// pointer.
#[repr(C)]
pub struct Heap {
    shared: *const Shared,
    tid: u32,
    /// Home arena: where plain `malloc` allocates from, and the arena whose
    /// caches are `caches` (design 6.2 `heap.arena`).
    arena: u16,
    slow_ticks: u32,
    last_gc: u64,
    /// Bytes that never went through a cache: huge runs and slots served
    /// directly from a slab because no cache could be allocated. The
    /// cached classes are derived from their headers in [`Heap::stats`].
    uncached: ThreadStats,
    /// Caches of every other arena this heap has touched.
    others: *mut ArenaCaches,
    /// `thread.allocated` / `thread.deallocated` as of the last `mallctl`
    /// read of any `thread.*allocated*` key; `thread.allocatedp` and
    /// `thread.deallocatedp` point into it (see [`Heap::materialise_stats`]).
    snapshot: ThreadStats,
    /// `thread.tcache.enabled`: while false, `ensure_cache` creates no
    /// cache and the thread is served straight from the slab bitmaps.
    tcache_enabled: bool,
    /// Counted in `Arenas::bound` for its home arena: thread heaps are,
    /// explicit caches (`create_detached`) are not.
    bound: bool,
    /// Caches of the home arena, one line per class; last, so the small
    /// fields above share two lines and `caches[c]` is `heap + 128 + 64 * c`.
    caches: [CacheHeader; NCLASSES],
}

const _: () = assert!(core::mem::offset_of!(Heap, caches) == 128);

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
    multi_arena: AtomicBool,
    /// `double_free_scan` (design 6.3 step 3): every push compares the slot
    /// against the top four cache entries and aborts on a match. Written
    /// once by `init`; kept on the line every `free` reads.
    double_free_scan: bool,
    pub regions: Regions,
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
    /// Process-wide statistics as of the last `epoch` (`stats.rs`).
    pub stats: Lock<crate::stats::Snapshot>,
}

// The flag and the `Regions` fields every `free` reads share one 128-byte
// line (`Regions` is `repr(C)`, so `superblock` and `lg_stride` lead it).
const _: () = assert!(
    core::mem::offset_of!(Shared, multi_arena) / 128
        == (core::mem::offset_of!(Shared, regions) + core::mem::offset_of!(Regions, superblock)) / 128
);
const _: () = assert!(
    core::mem::offset_of!(Shared, multi_arena) / 128
        == (core::mem::offset_of!(Shared, regions) + core::mem::offset_of!(Regions, lg_stride)) / 128
);

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

    pub fn ncpus() -> u32 {
        crate::sys::ncpus()
    }

    pub fn now_ns() -> u64 {
        crate::sys::now_coarse_ns()
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

    /// Host tests: one CPU, so the default `narenas` resolves to 4.
    pub fn ncpus() -> u32 {
        1
    }

    static TICK: AtomicU64 = AtomicU64::new(0);

    pub fn now_ns() -> u64 {
        TICK.fetch_add(1, Ordering::Relaxed) + 1
    }
}

use os::ThreadKey;

/// Guard slabs are placed once per `interval` slabs of 64 KiB, so a class
/// with smaller slabs (`layout::LG_MIN_SLAB`) gets one guard per
/// `interval * (64 KiB / slab)` slabs (in page groups when the page is
/// larger than the slab, see `SlabTable::is_guard_slab`): the same one
/// guard page per `interval * 64 KiB` of address space and the same
/// `madvise` count per byte committed for every class. 0 stays 0.
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
    pub unsafe fn init(this: *mut Shared, regions: Regions, config: Config, guard_install: bool) -> bool {
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
            ptr::write(&raw mut (*this).double_free_scan, config.double_free_scan);
            let meta = &*meta;
            let regions = &*(&raw const (*this).regions);
            let tables = &raw mut (*this).tables;
            for c in 0..NCLASSES {
                match SlabTable::new(meta, regions, c, guard_interval_for(c, regions.page_size(), guard_interval)) {
                    Some(t) => ptr::write(&raw mut (*tables)[c], t),
                    None => return false,
                }
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

    /// Whether pushes compare against the top four cache entries
    /// (`double_free_scan`).
    #[inline(always)]
    pub fn double_free_scan(&self) -> bool {
        self.double_free_scan
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
                let arena = self.table(class).check_located(slab as usize, slot as usize, p as usize);
                Ok((class, arena))
            }
            Kind::Huge { page } => Err(page),
            Kind::Invalid => invalid_free(p as usize),
        }
    }

    /// The sized free's check (`sdallocx`, `free_sized`; design 6.3): `p`
    /// must be a slot start in a handed-out, non-guard slab of the class
    /// the size hint names, `hint` (`class_for(size, align)`, `None` for a
    /// huge size). Arithmetic on the pointer and two dense lines
    /// (`class_base`, `CLASSES`, the class table): the slab descriptor is
    /// not loaded, so the bit test that makes plain `free` exact is left to
    /// the flush. The guard test is arithmetic on the table line the
    /// watermark load already brings in; whether
    /// the slab is below its arena's commit cursor is not knowable without
    /// the arena (the cursor sits under the class lock), so a slot start in
    /// a handed-out but uncommitted slab is caught at the flush, where the
    /// descriptor says `Fresh` (`release_owned`). `None` for anything
    /// else; the slow path repeats the checks with aborts
    /// ([`check_size_hint`], then [`classify_free`]). The arena is not read
    /// either (it is on the descriptor line): the home cache takes the
    /// slot and the flush routes it ([`release_routed`]).
    ///
    /// [`check_size_hint`]: Shared::check_size_hint
    /// [`classify_free`]: Shared::classify_free
    /// [`release_routed`]: Shared::release_routed
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[inline(always)]
    fn sized_slot(&self, p: *mut u8, hint: Option<usize>) -> Option<usize> {
        let (class, slab, _slot) = self.regions.slab_slot(p as usize)?;
        if hint != Some(class) {
            return None;
        }
        // SAFETY: `slab_slot` only returns `class < NCLASSES`.
        let table = unsafe { self.tables.get_unchecked(class) };
        if !table.is_handed_out(slab) || table.is_guard_slab(slab) {
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
    fn arena_of_slot(&self, class: usize, p: *mut u8) -> u16 {
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
    /// (design section 4, plan task 4f): the class bitmap word (one bit per
    /// slot, so a 550 MiB heap of 1 KiB values is 70 KB of bitmap and stays
    /// in L2) and, with several arenas, the group owner word (one `u16` per
    /// line-sized group of slabs). Neither touches the slab descriptor,
    /// which used to be one 64-byte line per slab and a cache miss per free
    /// on a large heap, and both are lines only the owning arena's refills
    /// and flushes write, because slabs are handed to arenas in groups
    /// that own whole lines.
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
    fn checked_slot(&self, p: *mut u8, h: &Heap) -> Option<usize> {
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

    /// Free the huge run starting at `page`. Aborts unless it is a live
    /// run start. Returns the usable bytes released.
    pub fn free_huge(&self, page: usize) -> usize {
        let run = self.huge.free_run(&self.meta, page);
        self.owner(run.arena).note_huge_free(run.pages);
        run.usable
    }
}

impl Heap {
    /// Create a heap in the metadata region for the calling thread, homed
    /// on the least-loaded automatic arena and counted there
    /// (`Arenas::assign`; `destroy` releases the binding). Null if the
    /// region is exhausted. Does not touch TLS or the thread key.
    pub fn create(shared: &Shared) -> *mut Heap {
        let arena = shared.arenas.assign();
        let h = Self::create_detached(shared, arena);
        if h.is_null() {
            shared.arenas.unbind(arena);
        } else {
            // SAFETY: fresh, exclusively owned.
            unsafe { (*h).bound = true };
        }
        h
    }

    /// Create a heap homed on `arena` (an explicit cache, or a thread heap
    /// before it is registered). Null if the region is exhausted.
    pub fn create_detached(shared: &Shared, arena: u16) -> *mut Heap {
        let h = shared.meta.alloc(Layout::new::<Heap>()) as *mut Heap;
        if h.is_null() {
            return h;
        }
        // SAFETY: `h` is a fresh, exclusively owned metadata block of the
        // right size and alignment. Field by field: a whole `Heap`
        // temporary (3.4 KiB since the headers moved inline) deepened the
        // stack of every thread's first allocation by one page, which
        // larson's thread churn turned into +1.7 MiB of RSS per 437
        // threads (thread stacks stay mapped; measured on the arm64 VM,
        // `bench/results/2026-09-18-aarch64-gce-dense-headers.md`).
        unsafe {
            ptr::write(&raw mut (*h).shared, shared);
            ptr::write(&raw mut (*h).tid, os::tid());
            ptr::write(&raw mut (*h).arena, arena);
            ptr::write(&raw mut (*h).slow_ticks, 0);
            ptr::write(&raw mut (*h).last_gc, os::now_ns());
            ptr::write(&raw mut (*h).uncached, ThreadStats::default());
            ptr::write(&raw mut (*h).others, ptr::null_mut());
            ptr::write(&raw mut (*h).snapshot, ThreadStats::default());
            ptr::write(&raw mut (*h).tcache_enabled, true);
            ptr::write(&raw mut (*h).bound, false);
            let caches = (&raw mut (*h).caches).cast::<CacheHeader>();
            for c in 0..NCLASSES {
                ptr::write(caches.add(c), CacheHeader::EMPTY);
            }
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
                let block = (o as *mut u8).sub(64 * (*o).pad as usize);
                shared.meta.free(block, node_block_layout());
                o = next;
            }
        }
        heap.others = ptr::null_mut();
        if heap.bound {
            shared.arenas.unbind(heap.arena);
        }
        shared.meta.free(h as *mut u8, Layout::new::<Heap>());
    }

    /// Reset every (flushed) header of one array to [`CacheHeader::EMPTY`]
    /// and free its stack.
    fn free_caches(shared: &Shared, caches: &mut [CacheHeader; NCLASSES]) {
        for cache in caches.iter_mut() {
            if cache.cap != 0 {
                assert_eq!(cache.len, 0, "heap: freeing a cache that was not flushed");
                shared.meta.free(cache.slots as *mut u8, CacheHeader::slots_layout(cache.cap));
            }
            *cache = CacheHeader::EMPTY;
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
            // empty and the old home's must go back first.
            self.drop_home_caches(shared);
        } else {
            // SAFETY: `node` is a live block owned by this heap.
            unsafe {
                core::mem::swap(&mut (*node).caches, &mut self.caches);
                (*node).arena = old;
            }
        }
        self.arena = arena;
        if self.bound {
            shared.arenas.unbind(old);
            shared.arenas.bind(arena);
        }
    }

    /// Flush the home caches, fold their byte counters into `uncached` and
    /// free them: the home array is `EMPTY` afterwards and `stats()` is
    /// unchanged. For a `thread.arena` change that could get no `others`
    /// node for the old home.
    fn drop_home_caches(&mut self, shared: &Shared) {
        let home = self.arena;
        self.flush_arena(shared, home, ptr::null_mut());
        Self::fold_counters(&mut self.uncached, &self.caches);
        Self::free_caches(shared, &mut self.caches);
    }

    /// Add the byte counters a cache array stands for (`allocs` pops and
    /// `len + allocs - moved` pushes per cache, each worth the class size)
    /// to `into`. [`Heap::stats`] derives the thread counters this way on
    /// every read; a cache that is about to be freed is folded into
    /// `uncached` first so that `thread.allocated` never goes backwards.
    fn fold_counters(into: &mut ThreadStats, caches: &[CacheHeader; NCLASSES]) {
        for (class, c) in caches.iter().enumerate() {
            if c.cap == 0 {
                continue;
            }
            let frees = (c.len as u64).wrapping_add(c.allocs).wrapping_sub(c.moved);
            into.add(CLASSES[class].size() as u64, c.allocs, frees);
        }
    }

    /// Byte counters for this thread: the uncached part plus what every
    /// cache of every arena stands for ([`Heap::fold_counters`]).
    pub fn stats(&self) -> ThreadStats {
        let mut s = self.uncached;
        Self::fold_counters(&mut s, &self.caches);
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                Self::fold_counters(&mut s, &(*o).caches);
                o = (*o).next;
            }
        }
        s
    }

    /// [`Heap::stats`] materialised into the heap for `thread.allocatedp`
    /// and `thread.deallocatedp` (design 13.4). The counters are derived
    /// on every read, so the pointers those keys hand out refer to this
    /// copy, which is refreshed by every read of any of the four
    /// `thread.*allocated*` keys and stale in between.
    pub fn materialise_stats(&mut self) -> &ThreadStats {
        self.snapshot = self.stats();
        &self.snapshot
    }

    /// `thread.tcache.enabled`.
    #[inline]
    pub fn tcache_enabled(&self) -> bool {
        self.tcache_enabled
    }

    /// `thread.tcache.enabled` write. Turning caches off folds the derived
    /// counters into `uncached` (so `thread.allocated` keeps counting),
    /// flushes and frees every cache of every arena, and makes
    /// `ensure_cache` refuse to create new ones; turning them on lets the
    /// next slow path create caches again.
    pub fn set_tcache_enabled(&mut self, shared: &Shared, on: bool) {
        self.tcache_enabled = on;
        if on {
            return;
        }
        self.flush_all(shared);
        Self::fold_counters(&mut self.uncached, &self.caches);
        Self::free_caches(shared, &mut self.caches);
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                Self::fold_counters(&mut self.uncached, &(*o).caches);
                Self::free_caches(shared, &mut (*o).caches);
                o = (*o).next;
            }
        }
    }

    #[inline]
    pub fn slow_ticks(&self) -> u32 {
        self.slow_ticks
    }

    /// The home arena's cache for `class`, or `None` while it has never
    /// been used.
    pub fn cache(&self, class: usize) -> Option<&CacheHeader> {
        let c = &self.caches[class];
        if c.cap == 0 { None } else { Some(c) }
    }

    /// The cache for `class` in `arena`, home or not, or `None` while it
    /// has never been used (or the arena was never touched).
    pub fn cache_in(&self, arena: u16, class: usize) -> Option<&CacheHeader> {
        if arena == self.arena {
            return self.cache(class);
        }
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                if (*o).arena == arena {
                    let c = &(*o).caches[class];
                    return if c.cap == 0 { None } else { Some(c) };
                }
                o = (*o).next;
            }
        }
        None
    }

    /// The home header for `class`: `heap + 128 + 64 * class`, no load.
    #[inline(always)]
    fn cache_ptr(&mut self, class: usize) -> *mut CacheHeader {
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of` only yields indices below `NCLASSES`.
        unsafe { (&raw mut self.caches).cast::<CacheHeader>().add(class) }
    }

    /// The header for `class` in an `others` node.
    ///
    /// # Safety
    /// `node` is a live node owned by this heap.
    #[inline]
    unsafe fn node_cache_ptr(node: *mut ArenaCaches, class: usize) -> *mut CacheHeader {
        debug_assert!(class < NCLASSES);
        // SAFETY: guaranteed by the caller; `class < NCLASSES`.
        unsafe { (&raw mut (*node).caches).cast::<CacheHeader>().add(class) }
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
        // SAFETY: this thread's own header, unused (`len == 0`) or with a
        // stack of `len` entries.
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
        if arena != self.arena {
            return self.alloc_other(shared, arena, class);
        }
        let cache = self.cache_ptr(class);
        // SAFETY: as in `alloc`.
        unsafe {
            let len = (*cache).len;
            if len != 0 {
                return self.pop(cache, class, len);
            }
        }
        // Not `alloc_slow`: an explicit arena is kept even for a shared
        // class (design 13.3).
        self.alloc_slow_in(shared, arena, class)
    }

    /// Pop the top entry of a non-empty cache. Touches only the header
    /// line and the slot: `allocs` is the only counter, `low_water` is
    /// written only when it actually drops (and the run clipped with it),
    /// and the class size is never looked up. Every `compact_mask + 1`
    /// pops go to [`Heap::pop_compact`] instead.
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `class` and `len` is its
    /// current non-zero length. `len` is only a copy: the scheduled pop
    /// runs `tick`, whose GC may flush this cache, and reloads it.
    #[inline(always)]
    unsafe fn pop(&mut self, cache: *mut CacheHeader, class: usize, len: u32) -> (*mut u8, bool) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let allocs = (*cache).allocs.wrapping_add(1);
            (*cache).allocs = allocs;
            if ((allocs as u32) & (*cache).compact_mask) == 0 {
                return self.pop_compact(cache, class, len);
            }
            Self::pop_top(cache, len)
        }
    }

    /// The ordinary pop of the top entry, `allocs` already counted.
    ///
    /// # Safety
    /// As for [`Heap::pop`].
    #[inline(always)]
    unsafe fn pop_top(cache: *mut CacheHeader, len: u32) -> (*mut u8, bool) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let len = len - 1;
            (*cache).len = len;
            if len < (*cache).low_water {
                (*cache).low_water = len;
                if len < (*cache).run_end {
                    (*cache).run_end = len;
                }
            }
            let p = *(*cache).slots().add(len as usize);
            let zero = len < (*cache).zeroed;
            (p, zero)
        }
    }

    /// The scheduled pop (design 6.4). Probing: run the periodic work, ask
    /// the class whether it is fragmented and being left alone by ordinary
    /// refills and, if not, pop the top as usual. Compacting: serve
    /// the lowest entry of the run, `slots[run_end - 1]`, moving the top
    /// entry into its place, so this allocation lands in the lowest slab
    /// with room instead of reusing the slot that was just freed; an
    /// exhausted run is rebuilt first ([`Heap::rebuild_run`]), which also
    /// re-checks the class and drops back to probing when it is compact.
    ///
    /// # Safety
    /// As for [`Heap::pop`]; `allocs` has already been counted. The
    /// probing branch calls `tick`, after which `len` is stale (the GC may
    /// have flushed the cache), so it reloads `len` and refills an emptied
    /// cache before popping.
    #[cold]
    #[inline(never)]
    unsafe fn pop_compact(&mut self, cache: *mut CacheHeader, class: usize, len: u32) -> (*mut u8, bool) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            match (*cache).mode {
                MODE_COMPACT => {}
                MODE_PROBE => {
                    let shared: &Shared = &*self.shared;
                    // The probe is also a heartbeat for the periodic work: a
                    // balanced churn on a dense class takes no other slow
                    // path, and without it no GC or decay would ever run on
                    // a thread without a background thread.
                    self.tick(shared);
                    let mut len = (*cache).len;
                    if len == 0 {
                        // The GC emptied the cache.
                        let Some(a) = shared.arena((*cache).arena) else { return (ptr::null_mut(), false) };
                        len = self.refill(shared, a, cache, class);
                        if len == 0 {
                            return (ptr::null_mut(), false);
                        }
                    }
                    let cs = shared.owner((*cache).arena).class(class);
                    let refilled = cs.refilled() as u32;
                    let delta = refilled.wrapping_sub((*cache).probe_refilled) as usize;
                    (*cache).probe_refilled = refilled;
                    if delta >= COMPACT_MAX_REFILL_SLABS * CLASSES[class].compact_slots() {
                        if Self::fragmented(shared, (*cache).arena, class) {
                            cs.note_gate_refill();
                        }
                        return Self::pop_top(cache, len);
                    }
                    if !Self::fragmented(shared, (*cache).arena, class) {
                        return Self::pop_top(cache, len);
                    }
                    cs.note_compact_mode(true);
                    (*cache).set_mode(MODE_COMPACT, &shared.config);
                    return self.pop_compact_run(cache, class, len);
                }
                _ => return Self::pop_top(cache, len),
            }
            self.pop_compact_run(cache, class, len)
        }
    }

    /// The compacting half of [`Heap::pop_compact`]. A cache that is not
    /// (or, after its rebuild, no longer) in compact mode pops the top.
    ///
    /// # Safety
    /// As for [`Heap::pop`].
    unsafe fn pop_compact_run(&mut self, cache: *mut CacheHeader, class: usize, len: u32) -> (*mut u8, bool) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            match (*cache).mode {
                MODE_COMPACT => {}
                _ => return Self::pop_top(cache, len),
            }
            let mut len = len;
            if (*cache).run_end == 0 {
                self.rebuild_run(cache, class);
                len = (*cache).len;
                if (*cache).run_end == 0 {
                    if len == 0 {
                        // The rebuild's tick ran the cache GC, which
                        // flushes every entry of a cache with `low_water
                        // == len < 4`, and the rebuild then claimed
                        // nothing (the class is no longer fragmented, or
                        // the claim came back empty). Refill as the probe
                        // branch does; popping here would wrap `len - 1`.
                        let shared: &Shared = &*self.shared;
                        let Some(a) = shared.arena((*cache).arena) else { return (ptr::null_mut(), false) };
                        len = self.refill(shared, a, cache, class);
                        if len == 0 {
                            return (ptr::null_mut(), false);
                        }
                    }
                    return Self::pop_top(cache, len);
                }
            }
            // The run invariant `run_end <= low_water <= len` is what keeps
            // this pop from serving a stale pointer out of `slots[run_end -
            // 1]`; a violation is a heap inconsistency and aborts in release.
            assert!(
                (*cache).run_end <= (*cache).low_water && (*cache).low_water <= len,
                "heap: cache run invariant violated (run_end > low_water or low_water > len)"
            );
            let top = len - 1;
            let at = (*cache).run_end - 1;
            let slots = (*cache).slots();
            let p = *slots.add(at as usize);
            *slots.add(at as usize) = *slots.add(top as usize);
            (*cache).len = top;
            (*cache).run_end = at;
            if top < (*cache).low_water {
                (*cache).low_water = top;
            }
            let zero = at < (*cache).zeroed;
            if zero && top >= (*cache).zeroed {
                (*cache).zeroed = at;
            }
            (p, zero)
        }
    }

    /// Whether `class` in `arena` is worth compacting (module doc).
    fn fragmented(shared: &Shared, arena: u16, class: usize) -> bool {
        let cs = shared.owner(arena).class(class);
        let (holes, capacity) = cs.holes();
        holes >= COMPACT_MIN_SLABS * CLASSES[class].compact_slots() && holes * COMPACT_MIN_RATIO >= capacity
    }

    /// Rebuild the run for a scheduled pop that found it exhausted: run the
    /// periodic work (under a balanced churn this is the only slow path a
    /// class ever takes), drop back to probing if the class is no longer
    /// fragmented, otherwise flush the cache down to `cap / 2` if it has no
    /// room for the block, claim a block of `cap / 4` slots from the class's
    /// lowest partial slab and swap it into the bottom of the stack,
    /// lowest address on top of the block. No sort: the block is claimed in
    /// address order and the entries it displaces are LIFO entries whose
    /// order does not matter.
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `class`, in compact mode.
    #[cold]
    #[inline(never)]
    unsafe fn rebuild_run(&mut self, cache: *mut CacheHeader, class: usize) {
        // SAFETY: a heap never outlives the `Shared` it was created on; the
        // unbounded lifetime lets `self` be borrowed mutably below.
        let shared: &Shared = unsafe { &*self.shared };
        self.tick(shared);
        // SAFETY: guaranteed by the caller; `len <= cap` entries exist and
        // `cap - len` slots of storage follow them.
        unsafe {
            let arena = (*cache).arena;
            if !Self::fragmented(shared, arena, class) {
                shared.owner(arena).class(class).note_compact_mode(false);
                (*cache).set_mode(MODE_PROBE, &shared.config);
                return;
            }
            let cap = (*cache).cap as usize;
            let half = (cap / 2).max(1);
            let block = (cap / 4).max(1);
            let len = (*cache).len as usize;
            if len + block > cap && len > half {
                self.flush(shared, arena, cache, class, len - half);
            }
            let len = (*cache).len as usize;
            let want = block.min(cap - len);
            if want == 0 {
                return;
            }
            let slots = (*cache).slots();
            let out = core::slice::from_raw_parts_mut(slots.add(len), want);
            let claim = shared.owner(arena).class(class).claim(&shared.meta, out);
            let n = claim.n;
            if n == 0 {
                return;
            }
            shared.owner(arena).class(class).note_compact(n);
            out[..n].reverse();
            let all = core::slice::from_raw_parts_mut(slots, len + n);
            if len >= n {
                // [old..][block] -> [block][old[n..]][old[..n]]
                let (bottom, rest) = all.split_at_mut(n);
                bottom.swap_with_slice(&mut rest[len - n..]);
            } else {
                // The block is bigger than what was there: rotate it down.
                all.rotate_left(len);
            }
            let new_len = (len + n) as u32;
            (*cache).len = new_len;
            (*cache).moved = (*cache).moved.wrapping_add(n as u64);
            (*cache).run_end = n as u32;
            (*cache).low_water = (*cache).low_water.max(n as u32);
            (*cache).zeroed = if claim.zeroed { n as u32 } else { 0 };
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
            self.count_uncached_alloc(shared.huge.usable_for(size).unwrap_or(0));
        }
        (p, zero)
    }

    /// Free of a huge run on this thread.
    #[cold]
    #[inline(never)]
    fn free_huge(&mut self, shared: &Shared, page: usize) {
        let n = shared.free_huge(page);
        self.count_uncached_free(n);
    }

    /// `bytes` were allocated on this thread without passing through a
    /// cache (a huge run, or a slot served straight from a slab): count
    /// them in `thread.allocated`. The one way `uncached` grows.
    #[inline]
    pub fn count_uncached_alloc(&mut self, bytes: usize) {
        self.uncached.allocated += bytes as u64;
    }

    /// `bytes` were freed on this thread without passing through a cache:
    /// count them in `thread.deallocated`.
    #[inline]
    pub fn count_uncached_free(&mut self, bytes: usize) {
        self.uncached.deallocated += bytes as u64;
    }

    /// A slot of `class` was claimed directly (`MALLOCX_TCACHE_NONE`) on
    /// this thread: count it.
    pub fn count_direct_alloc(&mut self, class: usize) {
        self.count_uncached_alloc(CLASSES[class].size());
    }

    /// A slot of `class` was released directly on this thread: count it.
    pub fn count_direct_free(&mut self, class: usize) {
        self.count_uncached_free(CLASSES[class].size());
    }

    /// Push a checked slot of `class` (home arena) onto its cache.
    #[inline(always)]
    fn push(&mut self, shared: &Shared, class: usize, p: *mut u8) {
        if !self.push_fast(class, p, shared.double_free_scan()) {
            self.free_slow(shared, class, p);
        }
    }

    /// The cache push alone: `false` when the cache is full (or still the
    /// sentinel) and the caller must take a slow path. No counter moves:
    /// the push count is implied by `len`, `allocs` and `moved`. `zeroed`
    /// is written only when the pushed slot lands inside the zero prefix,
    /// which is rare (the prefix is usually already zero). `scan` is
    /// `Shared::double_free_scan`.
    #[inline(always)]
    pub fn push_fast(&mut self, class: usize, p: *mut u8, scan: bool) -> bool {
        let cache = self.cache_ptr(class);
        // SAFETY: this thread's own header, unused (`len == cap == 0`, so
        // nothing is written) or with a stack of `cap` slots.
        unsafe { Self::push_cache(cache, p, scan) }
    }

    /// Push onto `cache` unless it is full. With `scan`, compare `p`
    /// against the top four entries first (design 6.3 step 3, plan Phase
    /// 1.5 task 9c): a match is a double free whose first copy is still
    /// cached, which the bit test cannot see, and it aborts before the
    /// second copy is stored. Four unconditional compares from the line
    /// the push writes to; a cache holding fewer than four takes the loop.
    ///
    /// # Safety
    /// `cache` is a header owned by this heap, unused or with a stack.
    #[inline(always)]
    unsafe fn push_cache(cache: *mut CacheHeader, p: *mut u8, scan: bool) -> bool {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let len = (*cache).len;
            if len == (*cache).cap {
                return false;
            }
            if scan && len != 0 {
                let top = (*cache).slots().add(len as usize);
                let hit = if len >= 4 {
                    (*top.sub(1) == p) | (*top.sub(2) == p) | (*top.sub(3) == p) | (*top.sub(4) == p)
                } else {
                    (1..=len as usize).any(|i| *top.sub(i) == p)
                };
                if hit {
                    cached_twice(p);
                }
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
        // The default path's one routing decision: a shared class (above
        // `tcache_max`, uncached everywhere) comes from arena 0 whatever
        // the home arena (`Config::shared_class`); explicit `MALLOCX_ARENA`
        // allocations (`alloc_in`) keep theirs.
        let arena = if shared.config.shared_class(class) { 0 } else { self.arena };
        self.alloc_slow_in(shared, arena, class)
    }

    /// Allocation of `class` from another arena than the home one: through
    /// this heap's cache for that arena, which is looked up (or created)
    /// on the `others` list.
    #[cold]
    #[inline(never)]
    fn alloc_other(&mut self, shared: &Shared, arena: u16, class: usize) -> (*mut u8, bool) {
        // A class this heap never caches for `arena` (`Config::cap_for`):
        // straight to the slow path, which serves it from the bitmaps
        // without creating an `others` node.
        if shared.config.cap_for(self.arena, arena, class) == 0 {
            return self.alloc_slow_in(shared, arena, class);
        }
        let node = self.other_caches(shared, arena, true);
        if !node.is_null() {
            // SAFETY: `node` is a live block owned by this heap.
            let cache = unsafe { Self::node_cache_ptr(node, class) };
            // SAFETY: unused (`len == 0`) or this heap's cache.
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
                self.count_direct_alloc(class);
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

    /// Flush the highest-address half of the home cache and push,
    /// allocating the cache first if this is the class's first use. Releases directly to
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
        // A class this heap never caches for `arena` (`Config::cap_for`):
        // straight to the owner's bitmap without creating a node for it.
        if shared.config.cap_for(self.arena, arena, class) == 0 {
            return self.free_slow_in(shared, arena, class, p);
        }
        let node = self.other_caches(shared, arena, true);
        if !node.is_null() {
            // SAFETY: `node` is a live block owned by this heap; the cache
            // is unused (`len == cap == 0`) or ours.
            if unsafe { Self::push_cache(Self::node_cache_ptr(node, class), p, shared.double_free_scan()) } {
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
            self.count_direct_free(class);
            return;
        }
        // SAFETY: `cache` is this heap's real cache for `(arena, class)`.
        unsafe {
            if (*cache).len == (*cache).cap {
                let half = ((*cache).cap / 2).max(1);
                self.flush(shared, arena, cache, class, half as usize);
            }
            if !Self::push_cache(cache, p, shared.double_free_scan()) {
                unreachable!("heap: cache still full after a flush");
            }
        }
    }

    /// The `others` node for `arena`, created (if `create`) on first use.
    /// Null when there is none (and none could be allocated). Never called
    /// with the home arena: creating a node for it would split the home
    /// caches in two, which the creation branch aborts on (the lookup walk
    /// is xmalloc's per-free path and cannot find a node that was never
    /// created).
    fn other_caches(&mut self, shared: &Shared, arena: u16, create: bool) -> *mut ArenaCaches {
        let mut o = self.others;
        let mut n = 0usize;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                if (*o).arena == arena {
                    return o;
                }
                o = (*o).next;
            }
            n += 1;
        }
        if !create {
            return ptr::null_mut();
        }
        assert_ne!(arena, self.arena, "heap: others node requested for the home arena");
        let block = shared.meta.alloc(node_block_layout());
        if block.is_null() {
            return ptr::null_mut();
        }
        let pad = n % NODE_STAGGER_LINES;
        // SAFETY: the block has room for the node at any of the
        // `NODE_STAGGER_LINES` line offsets and is exclusively ours. Field
        // by field: a whole `ArenaCaches` temporary would put 3 KiB on the
        // stack of every call (this function sits on the cross-arena free
        // path).
        let node = unsafe { block.add(64 * pad) } as *mut ArenaCaches;
        unsafe {
            ptr::write(&raw mut (*node).arena, arena);
            ptr::write(&raw mut (*node).pad, pad as u8);
            ptr::write(&raw mut (*node).next, self.others);
            for c in 0..NCLASSES {
                ptr::write(Self::node_cache_ptr(node, c), CacheHeader::EMPTY);
            }
        }
        self.others = node;
        node
    }

    /// The header for `(arena, class)`, creating the `others` node if
    /// needed. Null if no node could be allocated.
    fn cache_slot(&mut self, shared: &Shared, arena: u16, class: usize) -> *mut CacheHeader {
        if arena == self.arena {
            return self.cache_ptr(class);
        }
        let node = self.other_caches(shared, arena, true);
        if node.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `node` is a live block owned by this heap.
        unsafe { Self::node_cache_ptr(node, class) }
    }

    /// The usable cache for `(arena, class)`, giving the header its
    /// capacity and stack on first use. Null if the metadata region is
    /// exhausted or caches are off (`tcache:false`).
    fn ensure_cache(&mut self, shared: &Shared, arena: u16, class: usize) -> *mut CacheHeader {
        // The capacity first: a class that is never cached for this arena
        // must not create an `others` node.
        let cap = shared.config.cap_for(self.arena, arena, class);
        if cap == 0 || !self.tcache_enabled {
            return ptr::null_mut();
        }
        let cache = self.cache_slot(shared, arena, class);
        if cache.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `cache` points into this heap's own header array.
        if unsafe { (*cache).cap } != 0 {
            return cache;
        }
        let slots = shared.meta.alloc(CacheHeader::slots_layout(cap));
        if slots.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `cache` is this heap's own header (still `EMPTY`) and
        // `slots` has room for `cap` pointers, exclusively ours.
        unsafe {
            ptr::write(
                cache,
                CacheHeader {
                    len: 0,
                    cap,
                    low_water: 0,
                    zeroed: 0,
                    slots: slots as *mut *mut u8,
                    allocs: 0,
                    compact_mask: u32::MAX,
                    run_end: 0,
                    moved: 0,
                    probe_refilled: shared.owner(arena).class(class).refilled() as u32,
                    arena,
                    mode: MODE_OFF,
                },
            );
            let mode = if shared.config.tcache_compact_shift == 0 { MODE_OFF } else { MODE_PROBE };
            (*cache).set_mode(mode, &shared.config);
        }
        cache
    }

    /// Claim up to `cap / 2` slots of `class` in `arena` into an empty
    /// cache, lowest address on top so allocations stay address-ordered;
    /// the batch is the cache's run. Returns the new length.
    ///
    /// `low_water` restarts at the batch length rather than zero: the run
    /// needs `run_end <= low_water`, and a GC that then finds the batch
    /// untouched releases three quarters of it, which is what the GC is
    /// for (design 6.4).
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `(arena, class)` and is empty:
    /// callers arrive from a fast path that fell through on `len == 0`,
    /// from `ensure_cache` on a fresh header, or after re-reading `len`
    /// following a `tick`. The `assert_eq!` below is the abort on a heap
    /// inconsistency, not the contract.
    unsafe fn refill(&mut self, shared: &Shared, arena: &Arena, cache: *mut CacheHeader, class: usize) -> u32 {
        // SAFETY: the cache is empty and has `cap` slots of storage.
        unsafe {
            assert_eq!((*cache).len, 0, "heap: refill of a cache that is not empty");
            let want = ((*cache).cap / 2).max(1) as usize;
            let out = core::slice::from_raw_parts_mut((*cache).slots(), want);
            let claim = arena.class(class).claim(&shared.meta, out);
            arena.class(class).note_refill(claim.n);
            out[..claim.n].reverse();
            (*cache).len = claim.n as u32;
            (*cache).moved = (*cache).moved.wrapping_add(claim.n as u64);
            (*cache).low_water = claim.n as u32;
            (*cache).run_end = claim.n as u32;
            (*cache).zeroed = if claim.zeroed { claim.n as u32 } else { 0 };
            claim.n as u32
        }
    }

    /// Release the `n` highest-address entries of a cache to their slabs
    /// (in `arena`, which owns them) in one address-sorted batch and keep
    /// the rest in ascending order, highest on top. Selecting by address
    /// sends the holes to the high slabs, which is what lets them drain
    /// (module doc). The kept entries are deliberately left ascending
    /// rather than reversed into a run: a descending run under the
    /// ascending pushes of a producer/consumer pattern is a pathological
    /// pivot sequence for the next flush's sort (measured: xmalloc-test
    /// -41% on x86_64 with the reverse, -1.5% without; and -76% on arm64
    /// with an ascending refill batch under descending pushes), so the run
    /// is dropped and the next scheduled pop rebuilds it. The zero prefix
    /// is given up: the kept entries are reordered, so nothing is known
    /// about what lies at the bottom.
    ///
    /// The sort puts the two copies of a cached double free next to each
    /// other. `release_owned` aborts on a duplicate inside the released
    /// batch; a pair kept together stays cached and consistent (both
    /// copies in the cache, the bit set) until a flush releases it; the
    /// pair that straddles the split would have one copy released, its
    /// bit cleared, and the other still cached to be handed out over a
    /// slot a refill can claim again, so the
    /// split point gets one compare. Not a pass
    /// over the kept half: `rest_len` compares per flush cost xmalloc-test
    /// 4.9% on aarch64 and 7.3% on x86_64
    /// (`bench/results/2026-09-18-*-gce-detection-gaps.md`).
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `(arena, class)` and `n <= len`.
    unsafe fn flush(&mut self, shared: &Shared, arena: u16, cache: *mut CacheHeader, class: usize, n: usize) {
        if n == 0 {
            return;
        }
        // SAFETY: `n <= len <= cap` entries exist.
        unsafe {
            let all = (*cache).entries();
            all.sort_unstable();
            let rest_len = all.len() - n;
            if rest_len > 0 && *all.get_unchecked(rest_len - 1) == *all.get_unchecked(rest_len) {
                straddling_double_free(*all.get_unchecked(rest_len), class);
            }
            let (_keep, high) = all.split_at_mut(rest_len);
            shared.release_routed(arena, class, high);
            (*cache).len = rest_len as u32;
            (*cache).moved = (*cache).moved.wrapping_sub(n as u64);
            (*cache).zeroed = 0;
            (*cache).run_end = 0;
            (*cache).low_water = (*cache).low_water.min(rest_len as u32);
        }
    }

    /// The header for `class` in the home array (`node` null) or in `node`.
    ///
    /// # Safety
    /// `node` is null or a live node owned by this heap.
    #[inline]
    unsafe fn header_in(&mut self, node: *mut ArenaCaches, class: usize) -> *mut CacheHeader {
        // SAFETY: guaranteed by the caller.
        if node.is_null() { self.cache_ptr(class) } else { unsafe { Self::node_cache_ptr(node, class) } }
    }

    /// Flush every cache of one array (the home array when `node` is null,
    /// else the node's, whose arena is `arena`) completely.
    fn flush_arena(&mut self, shared: &Shared, arena: u16, node: *mut ArenaCaches) {
        for class in 0..NCLASSES {
            // SAFETY: `node` is null or a live node owned by this heap; an
            // unused header has `len == 0` and is skipped.
            unsafe {
                let cache = self.header_in(node, class);
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
        self.flush_arena(shared, home, ptr::null_mut());
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                let arena = (*o).arena;
                self.flush_arena(shared, arena, o);
                o = (*o).next;
            }
        }
    }

    /// Cache GC (design 6.4): for every cache, release three quarters of
    /// the entries that went unused since the last GC, then restart the
    /// low-water tracking from the current length.
    pub fn gc(&mut self, shared: &Shared) {
        let home = self.arena;
        self.gc_arena(shared, home, ptr::null_mut());
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: as in `flush_all`.
            unsafe {
                let arena = (*o).arena;
                self.gc_arena(shared, arena, o);
                o = (*o).next;
            }
        }
    }

    fn gc_arena(&mut self, shared: &Shared, arena: u16, node: *mut ArenaCaches) {
        for class in 0..NCLASSES {
            // SAFETY: `node` is null or a live node owned by this heap; an
            // unused header (`cap == 0`) is skipped.
            unsafe {
                let cache = self.header_in(node, class);
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
    /// `purge::on_tick`). Nothing else: thread statistics are merged by
    /// `epoch` (`stats.rs`), not here.
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

/// Destructor of the thread-exit key: mark the thread as exited, then flush
/// and free its heap. The sentinel goes first so that anything allocating
/// on this thread from here on (a signal handler landing in the middle of
/// `Heap::destroy`, another key's destructor afterwards) is served without
/// a heap instead of reaching the one being freed through `existing_heap`
///
/// # Safety
/// `p` is the heap pointer registered with `pthread_setspecific` by
/// `create_heap_for_thread`, called on the thread that owns it; after this
/// returns the heap is freed and the TLS slot holds `TLS_EXITED`, so it
/// must not be reached through TLS again (the sentinel is what stops
/// `existing_heap` from handing it out).
#[cfg(target_os = "linux")]
pub unsafe extern "C" fn heap_destructor(p: *mut c_void) {
    let h = p as *mut Heap;
    crate::tls::set(TLS_EXITED as *mut c_void);
    // SAFETY: the pointer was registered by `heap_for_thread` for this thread.
    unsafe {
        let shared = &*(*h).shared;
        Heap::destroy(h, shared);
    }
}

/// The calling thread's heap, created on first use. `None` when the thread
/// must be served without a cache: heap creation is in progress on it
/// (`pthread_setspecific` re-entered the allocator), its destructor has run,
/// or the metadata region is exhausted.
///
/// The `&mut Heap` is exclusive by construction, not by the type system:
/// only the owning thread reaches its heap through TLS, and every entry
/// point takes the reference once and passes it down, never holding two
/// (a `Shared` method called with one live must not call back into
/// `heap_for_thread` or [`existing_heap`]). The reference is not really
/// `'static` either: the thread-exit destructor frees the heap, so it must
/// not be stored beyond the call.
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
/// otherwise head every dependent-load chain). Same aliasing and lifetime
/// rule as [`heap_for_thread`]: one live reference per thread, never
/// stored.
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
    match shared.checked_slot(p, h) {
        Some(class) => h.push_fast(class, p, shared.double_free_scan()),
        None => false,
    }
}

/// The sized free's fast path on an existing heap (`sdallocx`,
/// `free_sized`): verify the size hint's class against the pointer
/// arithmetically and push onto the home cache without loading the slab
/// descriptor (design 6.3). `false` for anything off the fast path (not a
/// slot of a committed slab, a hint naming another class or a huge size, a
/// full cache); the caller then goes through [`Shared::check_size_hint`]
/// and the exact [`free`]. Contains no call.
#[inline(always)]
pub fn free_sized_fast(shared: &Shared, h: &mut Heap, p: *mut u8, hint: Option<usize>) -> bool {
    match shared.sized_slot(p, hint) {
        Some(class) => h.push_fast(class, p, shared.double_free_scan()),
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
                h.count_uncached_free(n);
            }
        }
    }
}

#[cold]
#[inline(never)]
fn invalid_free(p: usize) -> ! {
    panic!("invalid free of {p:#x}: not an allocation")
}

/// `double_free_scan`: the slot being pushed is one of the top four cache
/// entries, so it was freed a moment ago and never handed out since.
#[cold]
#[inline(never)]
fn cached_twice(p: *mut u8) -> ! {
    panic!("double free of {:#x}: the slot is already in the thread cache", p as usize)
}

/// Two copies of one slot in a cache at flush time, one on each side of
/// the split (`Heap::flush`): the flush would have released one and kept
/// the other.
#[cold]
#[inline(never)]
fn straddling_double_free(p: *mut u8, class: usize) -> ! {
    panic!("double free of {:#x} (class {class}): both copies cached, one at the flush boundary", p as usize)
}

#[cold]
#[inline(never)]
fn no_such_arena(index: u16) -> ! {
    panic!("arena {index} does not exist")
}
#[cfg(test)]
pub(crate) mod tests {
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
    /// One automatic arena, so every heap of a test shares arena 0 (the
    /// default would spread concurrently live heaps over `4 * ncpus`).
    pub(crate) fn shared() -> &'static Shared {
        shared_with(Config { narenas: 1, ..Config::DEFAULT })
    }

    pub(crate) fn shared_with(config: Config) -> &'static Shared {
        shared_with_guards(config, false)
    }

    /// `huge_retain_max` from `malloc_conf` reaches the huge allocator.
    #[test]
    fn huge_retain_max_key_reaches_the_huge_allocator() {
        assert_eq!(shared().huge.retain_max(), crate::huge::DEFAULT_RETAIN_MAX);
        let s = shared_with(Config { huge_retain_max: 3 << 20, ..Config::DEFAULT });
        assert_eq!(s.huge.retain_max(), 3 << 20);
        let s = shared_with(Config { huge_retain_max: 0, ..Config::DEFAULT });
        assert_eq!(s.huge.retain_max(), 0);
    }

    /// `shared()` with guard slabs at the default interval (`guard_install`
    /// as on a kernel with guard regions; a real reservation is needed, so
    /// Linux only).
    #[cfg(target_os = "linux")]
    pub(crate) fn shared_guarded() -> &'static Shared {
        shared_with_guards(Config { narenas: 1, ..Config::DEFAULT }, true)
    }

    fn shared_with_guards(config: Config, guard_install: bool) -> &'static Shared {
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
        assert!(unsafe { Shared::init(b.as_mut_ptr(), regions, config, guard_install) });
        unsafe { b.assume_init_ref() }
    }

    pub(crate) fn class_with(size: usize) -> usize {
        CLASSES.iter().position(|c| c.size() == size).unwrap()
    }

    pub(crate) struct TestHeap {
        h: *mut Heap,
        s: &'static Shared,
    }

    impl TestHeap {
        pub(crate) fn new(s: &'static Shared) -> TestHeap {
            let h = Heap::create(s);
            assert!(!h.is_null());
            TestHeap { h, s }
        }

        pub(crate) fn heap(&self) -> &mut Heap {
            unsafe { &mut *self.h }
        }

        pub(crate) fn alloc(&self, c: usize) -> *mut u8 {
            let p = self.heap().alloc(self.s, c);
            assert!(!p.is_null());
            p
        }

        fn alloc_zeroed(&self, c: usize) -> (*mut u8, bool) {
            let (p, z) = self.heap().alloc_zeroed(self.s, c);
            assert!(!p.is_null());
            (p, z)
        }

        pub(crate) fn free(&self, p: *mut u8) {
            self.heap().free(self.s, p)
        }

        pub(crate) fn alloc_huge(&self, size: usize, align: usize) -> (*mut u8, bool) {
            self.heap().alloc_huge(self.s, size, align)
        }

        fn stats(&self) -> ThreadStats {
            self.heap().stats()
        }

        fn gc(&self) {
            self.heap().gc(self.s)
        }

        pub(crate) fn flush_all(&self) {
            self.heap().flush_all(self.s)
        }

        fn ticks(&self) -> u32 {
            self.heap().slow_ticks()
        }

        fn cache(&self, c: usize) -> &CacheHeader {
            self.heap().cache(c).expect("cache exists")
        }

        fn len(&self, c: usize) -> usize {
            self.cache(c).len()
        }

        pub(crate) fn destroy(self) {
            unsafe { Heap::destroy(self.h, self.s) }
        }
    }

    /// Sum of `live` over every committed slab of a class.
    pub(crate) fn live(s: &Shared, c: usize) -> usize {
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
            let l = CacheHeader::slots_layout(cap);
            assert_eq!(l.size(), 8 * cap as usize);
            assert_eq!(l.align(), 16);
        }
        // One line per header, headers dense in the heap, the small fields
        // on the two lines before them (review P3).
        assert_eq!(core::mem::size_of::<CacheHeader>(), 64);
        assert_eq!(core::mem::align_of::<CacheHeader>(), 64);
        assert_eq!(core::mem::offset_of!(Heap, caches), 128);
        assert_eq!(core::mem::size_of::<Heap>(), 128 + 64 * NCLASSES, "heap is {} bytes", core::mem::size_of::<Heap>());
        assert_eq!(core::mem::size_of::<ArenaCaches>(), 64 + 64 * NCLASSES);
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
        // Only the heap and one stack came from the metadata region (both
        // rounded up to a metadata bin).
        let meta_used = s.meta.stats().allocated - before;
        assert!(meta_used >= core::mem::size_of::<Heap>() + 8 * CAP);
        assert!(meta_used <= 4096 + 8 * CAP, "{meta_used}");
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
    fn full_cache_flushes_the_highest_half_sorted_and_slabs_empty() {
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
                // A flush released the highest HALF and pushed one.
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

    /// Plan Phase 1.5 task 9a. Fill the class-64 cache with a double free
    /// and exactly the right number of other slots, so that after the
    /// flush's sort the pair sits at `lower` and `lower + 1`; the push that
    /// overflows the cache triggers the flush. Returns the flush's panic
    /// message, or the message of a later flush of the whole cache when
    /// the first one let the pair through.
    fn cached_pair_flush_message(lower_of: impl Fn(usize) -> usize) -> std::string::String {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let cap = default_cap(c) as usize;
        let mut all: Vec<*mut u8> = (0..4 * cap).map(|_| t.alloc(c)).collect();
        t.flush_all();
        all.sort_unstable();
        let mid = 2 * cap;
        let rest = cap - cap / 2;
        let lower = lower_of(rest);
        let higher = cap - 2 - lower;
        let p = all[mid];
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            t.free(p);
            t.free(p);
            for i in 1..=lower {
                t.free(all[mid - i]);
            }
            for i in 1..=higher {
                t.free(all[mid + i]);
            }
            assert_eq!(t.cache(c).len(), cap);
            t.free(all[0]);
            // A pair kept together is released, and caught, by the next
            // flush of the whole cache.
            t.flush_all();
        }));
        let e = r.expect_err("the flushes let a cached double free through");
        let msg = e
            .downcast_ref::<std::string::String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|m| std::string::String::from(*m)))
            .expect("a message");
        assert!(msg.contains(&std::format!("double free of {:#x} (class {c}", p as usize)), "{msg}");
        msg
    }

    /// Plan Phase 1.5 task 9c: with `double_free_scan` a second free of a
    /// slot that is one of the top four cache entries aborts at the push;
    /// deeper than that it is left to the flush as before.
    #[test]
    fn double_free_scan_compares_the_top_four_entries() {
        let s = shared_with(Config { narenas: 1, double_free_scan: true, ..Config::DEFAULT });
        assert!(s.double_free_scan());
        let t = TestHeap::new(s);
        let c = class_with(64);
        for between in 0..=4usize {
            let p: Vec<*mut u8> = (0..6).map(|_| t.alloc(c)).collect();
            let before = t.cache(c).len();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                t.free(p[0]);
                for &q in &p[1..=between] {
                    t.free(q);
                }
                t.free(p[0]);
            }));
            let msg = |e: Box<dyn std::any::Any + Send>| {
                e.downcast_ref::<std::string::String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|m| std::string::String::from(*m)))
                    .expect("a message")
            };
            if between < 4 {
                let m = msg(r.expect_err("the push let a double free through"));
                assert!(m.contains(&std::format!("double free of {:#x}: the slot is already in the thread cache", p[0] as usize)), "{between}: {m}");
                // The abort came before the second copy was stored.
                assert_eq!(t.cache(c).len(), before + between + 1);
                for &q in &p[between + 1..] {
                    t.free(q);
                }
                t.flush_all();
            } else {
                r.expect("the fifth entry down is not compared");
                // Both copies are cached; the flush catches them.
                let m = msg(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.flush_all())).expect_err("flush"));
                assert!(m.contains("double free of"), "{m}");
            }
        }
    }

    #[test]
    fn double_free_scan_off_by_default_leaves_the_push_alone() {
        let s = shared();
        assert_eq!(s.double_free_scan(), Config::DEFAULT.double_free_scan);
    }

    #[test]
    fn flush_aborts_on_a_cached_pair_straddling_the_split() {
        let msg = cached_pair_flush_message(|rest| rest - 1);
        assert!(msg.contains("both copies cached, one at the flush boundary"), "{msg}");
    }

    /// A pair that the split keeps together stays cached (both copies, the
    /// bit set) and is caught by the release that flushes it, not by the
    /// boundary compare.
    #[test]
    fn a_cached_pair_kept_by_the_split_is_caught_when_released() {
        let msg = cached_pair_flush_message(|rest| rest / 2);
        assert!(msg.contains("double free of") && !msg.contains("boundary"), "{msg}");
    }

    /// Plan Phase 1.5 task 9b: the sized fast path's check rejects a slot
    /// start inside a guard slab (arithmetic on the table line), so the
    /// exact slow path aborts instead of the guard memory being cached.
    #[cfg(target_os = "linux")]
    #[test]
    fn sized_slot_rejects_guard_slabs() {
        let s = shared_guarded();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let cs = s.arena0.class(c);
        let n = CLASSES[c].nslots();
        let interval = guard_interval_for(c, s.regions.page_size(), Config::DEFAULT.guard_slab_interval) as usize;
        let all: Vec<*mut u8> = (0..(interval + 2) * n).map(|_| t.alloc(c)).collect();
        let committed = cs.committed_slabs();
        let guard = (0..committed).find(|&i| cs.is_guard_slab(i)).expect("a guard slab was passed");
        let body = s.regions.slab_addr(c, guard) + s.regions.page_size().min(CLASSES[c].slab_size() / 2);
        assert_eq!(s.sized_slot(all[0], Some(c)), Some(c));
        assert_eq!(s.sized_slot(body as *mut u8, Some(c)), None, "guard slab {guard}");
        assert_eq!(s.sized_slot(s.regions.slab_addr(c, committed) as *mut u8, Some(c)), None, "not handed out");
        assert_eq!(s.sized_slot(all[0], Some(c + 1)), None, "wrong hint");
        for p in all {
            t.free(p);
        }
        t.destroy();
    }

    #[test]
    fn flush_releases_the_highest_addresses_and_keeps_the_lowest_ascending() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        // Slots spread over several slabs, freed in a scrambled order so
        // the cache holds an unsorted mix of low and high addresses.
        let n = 4 * CAP;
        let ps: Vec<*mut u8> = (0..n).map(|_| t.alloc(c)).collect();
        let mut order: Vec<usize> = (0..n).collect();
        let mut st = 0x9e37_79b9u64;
        for i in (1..n).rev() {
            let j = (xorshift(&mut st) % (i as u64 + 1)) as usize;
            order.swap(i, j);
        }
        let mut cached: Vec<*mut u8> = order[..CAP].iter().map(|&i| ps[i]).collect();
        for &q in &cached {
            t.free(q);
        }
        assert_eq!(t.len(c), CAP);
        let live_before = live(s, c);
        // Overflow by one: the HALF highest addresses go back to their
        // slabs, the HALF lowest stay in ascending order (highest of them
        // on top) under the new push.
        let extra = ps[order[CAP]];
        t.free(extra);
        assert_eq!(t.len(c), HALF + 1);
        assert_eq!(live(s, c), live_before - HALF);
        cached.sort_unstable();
        assert_eq!(t.alloc(c), extra);
        for (k, &want) in cached[..HALF].iter().rev().enumerate() {
            assert_eq!(t.alloc(c), want, "pop {k}");
        }
        assert_eq!(t.len(c), 0);
        // The released half is free in the slabs: allocating it again
        // is the only way to get those addresses back, and a refill takes
        // the lowest partial slab first, which is where they now are
        // (everything below them is live in the program).
        let got: Vec<*mut u8> = (0..HALF).map(|_| t.alloc(c)).collect();
        let mut got_sorted = got.clone();
        got_sorted.sort_unstable();
        assert_eq!(got_sorted, cached[HALF..].to_vec());
        t.destroy();
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
    fn flush_drops_the_zeroed_prefix() {
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
        // HALF entries went back to the slab and the rest were reordered:
        // the prefix is gone.
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
        // A refill starts low water at the batch length (the run needs
        // `run_end <= low_water`); the pop that follows lowers it.
        t.alloc(c);
        assert_eq!(t.len(c), HALF - 1);
        assert_eq!(t.cache(c).low_water(), HALF - 1);
        assert_eq!(t.cache(c).run().len(), HALF - 1);
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
        // A cache that was drained and refilled and then left untouched
        // gives three quarters of the batch back at the next GC.
        while t.len(c) > 0 {
            t.alloc(c);
        }
        t.alloc(c);
        assert_eq!(t.cache(c).low_water(), HALF - 1);
        let len = t.len(c);
        t.gc();
        assert_eq!(t.len(c), len - (len - len / 4));
        assert_eq!(t.cache(c).low_water(), t.len(c));
        assert_eq!(t.cache(c).run().len(), 0);
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
            // Prime class d: one refill, HALF - 1 cached, low water HALF - 1.
            t.alloc(d);
            assert_eq!(t.ticks(), 1);
            // Drive the tick counter with class c refills.
            let mut ps = Vec::new();
            while t.ticks() < 64 {
                ps.push(t.alloc(c));
            }
            assert_eq!(t.ticks(), 64);
            // GC 1 released three quarters of d's untouched batch if it ran.
            let lw1 = HALF - 1;
            let flushed1 = if interval == 0 { lw1 - lw1 / 4 } else { 0 };
            assert_eq!(t.len(d), lw1 - flushed1);
            assert_eq!(t.cache(d).low_water(), lw1 - flushed1);
            for _ in 0..27 {
                t.alloc(d);
            }
            let lw = lw1 - flushed1 - 27;
            assert_eq!(t.len(d), lw);
            while t.ticks() < 128 {
                ps.push(t.alloc(c));
            }
            // GC 2 released three quarters of d's low water if it ran.
            let live_d = live(s, d);
            let flushed = if interval == 0 { lw - lw / 4 } else { 0 };
            assert_eq!(t.len(d), lw - flushed);
            assert_eq!(t.cache(d).low_water(), lw - flushed);
            assert_eq!(live_d, HALF - flushed1 - flushed);
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

    /// Fill `slabs` slabs of `c` on `t`, free a random `frac` of the blocks
    /// and return the survivors: the post-delete residue of the churn
    /// probe, with the holes in the slabs (the cache is flushed).
    /// Slots the churn models fill: 40 slabs of the 64-slot 1 KiB class the
    /// models were written for (`bench/probes/churn.c`), 640 of today's
    /// 4-slot slabs.
    const MODEL_SLOTS: usize = 2560;

    fn residue(t: &TestHeap, c: usize, slabs: usize, frac_per_mille: u64, seed: u64) -> Vec<*mut u8> {
        let nslots = CLASSES[c].nslots();
        let mut st = seed;
        let mut keep = Vec::new();
        for p in (0..slabs * nslots).map(|_| t.alloc(c)).collect::<Vec<_>>() {
            if xorshift(&mut st) % 1000 < frac_per_mille { t.free(p) } else { keep.push(p) }
        }
        t.flush_all();
        keep
    }

    /// Slabs of `c` with at least one live slot.
    fn occupied(s: &Shared, c: usize) -> usize {
        let cs = &s.arena0.classes[c];
        (0..cs.committed_slabs()).filter(|&i| !cs.is_guard_slab(i) && cs.desc(i).live() > 0).count()
    }

    /// Pop one slot of `c` and check the rule against the cache state
    /// before the pop: while compacting, a scheduled pop returns the run's
    /// lowest entry (its last) or, when the run was exhausted, the lowest
    /// slot of the block a rebuild claimed; otherwise every pop returns the
    /// top of the stack. Nothing is checked when the cache was empty (a
    /// refill happened). The run is descending afterwards.
    fn pop_checked(t: &TestHeap, c: usize) -> *mut u8 {
        let (before, run, allocs, compacting) = match t.heap().cache(c) {
            Some(k) => (k.cached().to_vec(), k.run().to_vec(), k.allocs(), k.compacting()),
            None => (Vec::new(), Vec::new(), 0, false),
        };
        let scheduled = compacting && (allocs + 1) % (1 << Config::DEFAULT.tcache_compact_shift) == 0;
        let probe = !compacting && (allocs + 1) % (PROBE_MASK as u64 + 1) == 0;
        let p = t.alloc(c);
        if !before.is_empty() && !probe {
            if scheduled {
                if let Some(&low) = run.last() {
                    assert_eq!(p, low, "scheduled pop {} is not the run's lowest entry", allocs + 1);
                } else {
                    // A rebuild: the block came from the lowest partial slab
                    // and its lowest slot was served.
                    assert!(t.cache(c).run().iter().all(|&q| q > p), "rebuild pop {} is not the block minimum", allocs + 1);
                }
            } else if t.cache(c).compacting() || !compacting {
                assert_eq!(p, *before.last().unwrap(), "pop {} is not the top", allocs + 1);
            }
        }
        check_run(t, c);
        p
    }

    /// The run invariant: descending, distinct, a prefix of the cache no
    /// longer than the low-water mark.
    fn check_run(t: &TestHeap, c: usize) {
        let k = t.cache(c);
        let run = k.run();
        assert!(run.windows(2).all(|w| w[0] > w[1]), "run not descending: {run:?}");
        assert!(run.len() <= k.low_water() && k.low_water() <= k.len());
        assert_eq!(run, &k.cached()[..run.len()]);
    }

    /// The compaction wrap (review 2026-09-18, section 1.4): a scheduled
    /// pop finds a compact-mode cache holding one entry with `low_water ==
    /// len == 1`, an exhausted run and the GC due. `rebuild_run`'s tick
    /// runs the GC, which flushes the whole cache (`lw - lw / 4 == lw`
    /// below 4); the class is dense by then, so the rebuild drops the cache
    /// back to probing and claims nothing. `pop_compact_run` must then
    /// refill (or give up) rather than pop the empty cache, where `len - 1`
    /// wrapped and read `slots[0xFFFF_FFFF]`. Every step but the schedule
    /// (`allocs`, `slow_ticks`, `last_gc`) goes through the public
    /// operations.
    #[test]
    fn a_scheduled_pop_whose_gc_empties_the_cache_refills_instead_of_wrapping() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(224);
        let mut live_ps = residue(&t, c, 30, 560, 0x0c0f_fee5);
        assert!(Heap::fragmented(s, 0, c));
        // Refill once, then probe until the cache switches to compacting
        // (the first probe only records the class's refill count).
        live_ps.push(t.alloc(c));
        for _ in 0..4 {
            if t.cache(c).compacting() {
                break;
            }
            let k = t.heap().cache_ptr(c);
            unsafe { (*k).allocs = (*k).compact_mask as u64 };
            live_ps.push(t.alloc(c));
        }
        assert!(t.cache(c).compacting());
        // One entry, `low_water == len == 1`, no run, still compacting.
        t.flush_all();
        assert!(t.cache(c).compacting());
        let p0 = live_ps.pop().unwrap();
        t.free(p0);
        t.gc();
        assert_eq!(t.len(c), 1);
        assert_eq!(t.cache(c).low_water(), 1);
        assert!(t.cache(c).run().is_empty());
        // Make the class dense: every other live slot goes back to its slab
        // through another heap, so `p0`'s slab is the only one that counts
        // and a single slab is never fragmented.
        let b = TestHeap::new(s);
        for &p in &live_ps {
            b.free(p);
        }
        b.destroy();
        assert!(!Heap::fragmented(s, 0, c));
        assert_eq!(live(s, c), 1);
        // The next pop is scheduled, its tick is the periodic one and the
        // GC interval has elapsed (on the host `now_ns` is a counter, so the
        // interval is subtracted rather than assumed).
        let h = t.heap();
        h.slow_ticks = PERIODIC_EVERY - 1;
        h.last_gc = os::now_ns().wrapping_sub(s.gc_interval_ns);
        let k = h.cache_ptr(c);
        unsafe { (*k).allocs = (*k).compact_mask as u64 };
        let ticks = t.ticks();
        let p = t.alloc(c);
        assert_eq!(t.ticks(), ticks + 1);
        assert!(!p.is_null());
        let (cls, arena) = s.classify_free(p).ok().expect("a slot of the class");
        assert_eq!((cls, arena), (c, 0));
        // The GC flushed `p0`, the rebuild dropped back to probing, and the
        // refill served the pop: half a cache minus the one popped.
        assert!(!t.cache(c).compacting());
        assert_eq!(t.len(c), t.cache(c).cap() / 2 - 1);
        assert_eq!(t.cache(c).low_water(), t.len(c));
        check_run(&t, c);
        t.free(p);
        t.destroy();
        assert_eq!(live(s, c), 0);
    }

    #[test]
    fn a_dense_class_is_probed_but_never_compacted() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        // A larson-like churn over a dense class: allocations and frees
        // balance, the holes stay in the cache, the class has none.
        let mut ps: Vec<*mut u8> = (0..2000).map(|_| pop_checked(&t, c)).collect();
        let ticks = t.ticks();
        let mut st = 0xabcd_ef01u64;
        for _ in 0..40_000 {
            let i = (xorshift(&mut st) % ps.len() as u64) as usize;
            t.free(ps[i]);
            ps[i] = pop_checked(&t, c);
        }
        let pops = t.cache(c).allocs();
        assert!(pops > 4 * (PROBE_MASK as u64 + 1), "{pops} pops");
        assert!(!t.cache(c).compacting());
        // No refill, no flush, no rebuild: the only slow paths were the
        // probes' heartbeats, one per 4096 pops.
        assert_eq!(t.ticks() as u64, ticks as u64 + pops / (PROBE_MASK as u64 + 1) - 2000 / (PROBE_MASK as u64 + 1));
        assert!(!Heap::fragmented(s, 0, c));
        for p in ps {
            t.free(p);
        }
        t.destroy();
    }

    #[test]
    fn scheduled_pops_take_the_run_minimum_and_rebuild_from_the_lowest_slab() {
        let s = shared();
        let t = TestHeap::new(s);
        // 292 slots per slab, cap 512, block 128.
        let c = class_with(224);
        let cs = &s.arena0.classes[c];
        let mut live_ps = residue(&t, c, 30, 560, 0x5eed_1234);
        assert!(Heap::fragmented(s, 0, c));
        assert_eq!(t.len(c), 0);
        // The first probe switches the cache to compacting; from then on
        // every eighth pop is served from the run.
        let mut st = 0x1357_9bdfu64;
        let mut switched_at = None;
        let mut rebuilds = 0;
        for step in 0..30_000 {
            if xorshift(&mut st) & 1 == 0 && live_ps.len() > 2000 {
                let i = (xorshift(&mut st) % live_ps.len() as u64) as usize;
                t.free(live_ps.swap_remove(i));
                check_run(&t, c);
            } else {
                let run_before = t.heap().cache(c).map_or(0, |k| k.run().len());
                let ticks = t.ticks();
                live_ps.push(pop_checked(&t, c));
                if run_before == 0 && t.ticks() > ticks && t.cache(c).compacting() {
                    rebuilds += 1;
                    // The block is the run, claimed lowest partial slab
                    // first: no slab below the highest it touched still has
                    // room (the lowest partial slab left is at or above it).
                    let run = t.cache(c).run();
                    let top_slab = run.iter().map(|&q| cs.locate(q).0).max().unwrap();
                    // (`plan` clears a filled slab's partial bit lazily, so
                    // the lowest partial slab may be a full one.)
                    let nslots = CLASSES[c].nslots();
                    assert!(
                        cs.lowest_partial().is_none_or(|l| l >= top_slab || cs.desc(l).live() == nslots),
                        "block from slab {top_slab} above the lowest partial slab {:?}",
                        cs.lowest_partial()
                    );
                }
            }
            if switched_at.is_none() && t.cache(c).compacting() {
                switched_at = Some(step);
            }
            if step % 1009 == 0 {
                assert_eq!(live(s, c), live_ps.len() + t.len(c), "step {step}");
            }
        }
        assert!(switched_at.is_some(), "never switched to compacting");
        assert!(rebuilds > 0, "no rebuild observed");
        assert_eq!(live(s, c), live_ps.len() + t.len(c));
        for p in live_ps {
            t.free(p);
        }
        t.destroy();
        assert_eq!(live(s, c), 0);
    }

    #[test]
    fn churn_model_empties_the_high_slabs_and_then_stops_compacting() {
        // The model of `bench/probes/churn.c`: fill `MODEL_SLOTS` slots of
        // the 1 KiB class (cap 128), free a random 56%, then run a
        // balanced free-one/allocate-one churn. Without scheduled pops
        // nothing moves (every allocation gets back the slot just freed);
        // with them the live set migrates into the lowest slabs, the high
        // slabs empty, and once the class is dense the cache goes back to
        // probing.
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(1024);
        let nslots = CLASSES[c].nslots();
        let mut keep = residue(&t, c, MODEL_SLOTS / nslots, 560, 0x9e37_79b9_7f4a_7c15);
        let before = occupied(s, c);
        // A random 56% deletion empties a 64-slot slab never and a 4-slot
        // slab about one time in ten.
        assert!(before * 20 >= 17 * (MODEL_SLOTS / nslots), "{before} occupied slabs after the delete");
        assert!(Heap::fragmented(s, 0, c));
        let ideal = keep.len().div_ceil(nslots);
        let mut st = 0x2468_ace0u64;
        let mut was_compacting = false;
        // With shift 8 one pop in 256 migrates a slot; the live set of about
        // 1100 slots in the high half needs a few hundred thousand steps.
        for _ in 0..400_000 {
            let i = (xorshift(&mut st) % keep.len() as u64) as usize;
            t.free(keep[i]);
            keep[i] = t.alloc(c);
            was_compacting |= t.cache(c).compacting();
        }
        assert!(was_compacting);
        let after = occupied(s, c);
        // Live data plus the holes the gate leaves (under COMPACT_MIN_SLABS
        // 64 KiB slabs' worth, i.e. 64 of the 4 KiB slabs) plus what the
        // cache holds (cap = 128 slots, 32 slabs) plus one partial slab.
        let bound = ideal + COMPACT_MIN_SLABS * CLASSES[c].compact_slots() / nslots + default_cap(c) as usize / nslots + 1;
        assert!(after <= bound, "{after} occupied slabs after the churn, ideal {ideal}, bound {bound}, before {before}");
        assert!(!Heap::fragmented(s, 0, c));
        assert!(!t.cache(c).compacting(), "still compacting a dense class");
        assert_eq!(live(s, c), keep.len() + t.len(c));
        for p in keep {
            t.free(p);
        }
        t.destroy();
        assert_eq!(live(s, c), 0);
    }

    #[test]
    fn direct_claims_do_not_block_compaction_but_cache_refills_do() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(1024);
        let cs = &s.arena0.classes[c];
        let nslots = CLASSES[c].nslots();
        let mut keep = residue(&t, c, MODEL_SLOTS / CLASSES[c].nslots(), 560, 0x0bad_cafe);
        assert!(Heap::fragmented(s, 0, c));
        // A balanced churn with a defragmenter-like stream of direct
        // (MALLOCX_TCACHE_NONE) claims at 3 slabs' worth per probe window
        // (over the gate, were it counting `claimed`; light enough not to
        // fill the residue's holes by itself): `claimed` races ahead,
        // `refilled` does not, compaction engages and slabs empty.
        let mut st = 0x1111_2222u64;
        let mut direct: *mut u8 = ptr::null_mut();
        let before_claimed = cs.claimed();
        let before = occupied(s, c);
        let mut was_compacting = false;
        let windows = 72;
        for step in 0..windows * (PROBE_MASK as usize + 1) {
            let i = (xorshift(&mut st) % keep.len() as u64) as usize;
            t.free(keep[i]);
            keep[i] = t.alloc(c);
            was_compacting |= t.cache(c).compacting();
            if step % 32 == 0 {
                // Like the defragmenter: a direct copy, the old one freed.
                if !direct.is_null() {
                    s.free_direct(0, c, direct);
                }
                let (p, _) = s.alloc_direct(&s.arena0, c);
                assert!(!p.is_null());
                direct = p;
            }
        }
        assert!(cs.claimed() - before_claimed >= windows as u64 * COMPACT_MAX_REFILL_SLABS as u64 * CLASSES[c].compact_slots() as u64);
        assert!(was_compacting, "direct claims blocked compaction");
        // The churn model's bound: live data plus the holes the gate
        // leaves plus what the cache holds plus one partial slab.
        let bound = keep.len().div_ceil(nslots) + COMPACT_MIN_SLABS * CLASSES[c].compact_slots() / nslots + default_cap(c) as usize / nslots + 1;
        assert!(occupied(s, c) <= bound, "little compacted: {} of {before} slabs still occupied, bound {bound}", occupied(s, c));
        s.free_direct(0, c, direct);
        for p in keep {
            t.free(p);
        }
        t.destroy();
        // A residue under a growing allocation stream (the 224-byte class,
        // 292 slots per slab, so two probe windows fit the synthetic
        // region): every 256 pops refill 256 slots, 4096 per window, far
        // over the gate; the class is being drained the ordinary way and
        // is left alone.
        let c = class_with(224);
        let cs = &s.arena0.classes[c];
        let t = TestHeap::new(s);
        let mut keep = residue(&t, c, 30, 560, 0x3333_4444);
        assert!(Heap::fragmented(s, 0, c));
        let before_refilled = cs.refilled();
        for _ in 0..2 * (PROBE_MASK as usize + 1) + 64 {
            keep.push(t.alloc(c));
        }
        assert!(cs.refilled() - before_refilled >= 2 * 4096 - 256);
        assert!(!t.cache(c).compacting(), "a refilling class was compacted");
        for p in keep {
            t.free(p);
        }
        t.destroy();
        assert_eq!(live(s, c), 0);
    }

    #[test]
    fn random_walk_churn_model_compacts_too() {
        // Valkey's delete-phase pattern: frees and allocations of one class
        // arrive in random order (memtier DEL:SET over random keys), so the
        // live count and the cache length random-walk instead of the strict
        // alternation of `churn_model_...`. The bound is the same.
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(1024);
        let nslots = CLASSES[c].nslots();
        let mut keep = residue(&t, c, MODEL_SLOTS / CLASSES[c].nslots(), 560, 0x5151_a1a1_7777_0f0f);
        let target = keep.len();
        let before = occupied(s, c);
        let mut st = 0x1357_9bdfu64;
        let mut was_compacting = false;
        for _ in 0..400_000 {
            let r = xorshift(&mut st);
            let go_free = if keep.len() > target + 100 { true } else if keep.len() + 100 < target { false } else { r & 1 == 0 };
            if go_free {
                let i = ((r >> 8) % keep.len() as u64) as usize;
                t.free(keep.swap_remove(i));
            } else {
                keep.push(t.alloc(c));
            }
            was_compacting |= t.cache(c).compacting();
        }
        assert!(was_compacting);
        let after = occupied(s, c);
        let bound = keep.len().div_ceil(nslots) + COMPACT_MIN_SLABS * CLASSES[c].compact_slots() / nslots + default_cap(c) as usize / nslots + 1;
        assert!(after <= bound, "{after} occupied slabs after the random walk, bound {bound}, before {before}");
        for p in keep {
            t.free(p);
        }
        t.destroy();
    }

    #[test]
    fn compaction_off_keeps_the_lifo_reuse_in_place() {
        let s = shared_with(Config { tcache_compact_shift: 0, ..Config::DEFAULT });
        let t = TestHeap::new(s);
        let c = class_with(1024);
        let keep = residue(&t, c, MODEL_SLOTS / CLASSES[c].nslots(), 560, 0x1234_5678);
        let before = occupied(s, c);
        let mut st = 0x8765_4321u64;
        for _ in 0..50_000 {
            let i = (xorshift(&mut st) % keep.len() as u64) as usize;
            t.free(keep[i]);
            let p = t.alloc(c);
            assert_eq!(p, keep[i], "LIFO reuse in place");
        }
        assert_eq!(occupied(s, c), before);
        assert!(!t.cache(c).compacting());
        for p in keep {
            t.free(p);
        }
        t.destroy();
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
        let c = class_with(16);
        let p = t.alloc(c);
        // A slot of slab 0 above the first refill's HALF is committed but
        // was never claimed (the 16-byte slab has 1024 slots).
        t.free(unsafe { p.add((HALF + 200) * 16) });
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

    /// `narenas:3`: a new heap is bound to the automatic arena with the
    /// fewest bound heaps (ties: a built one first, then the lowest index),
    /// `destroy` releases the binding, and arenas are built on first touch.
    #[test]
    fn automatic_arenas_are_assigned_least_loaded_and_built_lazily() {
        let s = shared_with(Config { narenas: 3, ..Config::DEFAULT });
        assert_eq!(s.arenas.count(), 3);
        assert_eq!(s.arenas.auto(), 3);
        assert!(s.arenas.get_existing(1).is_none());
        // Six live heaps: two per arena, lowest index first.
        let mut heaps: Vec<TestHeap> = (0..6).map(|_| TestHeap::new(s)).collect();
        let homes: Vec<u16> = heaps.iter().map(|t| t.heap().arena()).collect();
        assert_eq!(homes, [0, 1, 2, 0, 1, 2]);
        assert_eq!((0..3).map(|i| s.arenas.bound(i)).collect::<Vec<_>>(), [2, 2, 2]);
        assert!(s.arenas.get_existing(1).is_none(), "assignment alone must not build");
        assert!(!s.multi_arena());
        // Destroying a heap frees its binding; the next heap takes the
        // vacated arena.
        heaps.remove(1).destroy();
        assert_eq!(s.arenas.bound(1), 1);
        let t = TestHeap::new(s);
        assert_eq!(t.heap().arena(), 1);
        assert_eq!(s.arenas.bound(1), 2);
        // Explicit arenas and explicit caches take no part in it.
        assert!(s.arenas.create(s).is_some_and(|i| i == 3));
        assert_eq!(s.arenas.bound(3), 0);
        let before = s.arenas.bound(0);
        let tc = s.tcaches.create(s, 0).unwrap();
        assert_eq!(s.arenas.bound(0), before);
        assert!(s.tcaches.destroy(s, tc));
        assert_eq!(s.arenas.bound(0), before);
        // First touch builds arena 1.
        let c = class_with(64);
        let p = t.alloc(c);
        assert!(s.arenas.get_existing(1).is_some());
        assert!(s.multi_arena());
        assert_eq!(s.arena0.classes[c].desc(0).arena(), 1);
        assert_eq!(s.classify_free(p), Ok((c, 1)));
        t.heap().free(s, p);
        assert_eq!(t.heap().cache(c).unwrap().len(), HALF);
        // `thread.arena` moves the binding with the heap.
        t.heap().set_arena(s, 3);
        assert_eq!((s.arenas.bound(1), s.arenas.bound(3)), (1, 1));
        t.destroy();
        assert_eq!((s.arenas.bound(1), s.arenas.bound(3)), (1, 0));
        for h in heaps {
            h.destroy();
        }
        assert_eq!((0..4).map(|i| s.arenas.bound(i)).collect::<Vec<_>>(), [0, 0, 0, 0]);
    }

    /// Thread churn under a multi-arena default: a lone heap stays on
    /// arena 0 and never sets `multi_arena` (a single-threaded program
    /// never pays the `free` routing); a second heap that comes and goes
    /// lands on arena 1 every time, so only the arenas that were ever
    /// alive at once get built; a built idle arena beats an unbuilt one.
    #[test]
    fn heap_churn_reuses_the_vacated_arena() {
        let s = shared_with(Config { narenas: 8, ..Config::DEFAULT });
        let main = TestHeap::new(s);
        assert_eq!(main.heap().arena(), 0);
        let c = class_with(64);
        let p = main.alloc(c);
        assert!(!s.multi_arena(), "one heap: arena 0 alone");
        for _ in 0..20 {
            let w = TestHeap::new(s);
            assert_eq!(w.heap().arena(), 1);
            let q = w.alloc(c);
            w.free(q);
            w.destroy();
        }
        assert!(s.multi_arena());
        assert!(s.arenas.get_existing(1).is_some());
        assert!((2..8).all(|i| s.arenas.get_existing(i).is_none()), "only arenas 0 and 1 were ever alive at once");
        assert_eq!((s.arenas.bound(0), s.arenas.bound(1)), (1, 0));
        // Two workers alive at once: the second takes a third arena.
        let w1 = TestHeap::new(s);
        let w2 = TestHeap::new(s);
        assert_eq!((w1.heap().arena(), w2.heap().arena()), (1, 2));
        w1.destroy();
        // Arena 1 (built, idle) is taken before arena 2 would be shared.
        let w3 = TestHeap::new(s);
        assert_eq!(w3.heap().arena(), 1);
        w2.destroy();
        // Arenas 2..8 are all idle and unbuilt: lowest index.
        let w4 = TestHeap::new(s);
        assert_eq!(w4.heap().arena(), 2);
        w3.destroy();
        w4.destroy();
        main.free(p);
        main.destroy();
        assert!((0..8).all(|i| s.arenas.bound(i) == 0));
    }

    /// `narenas:0` (the default) is resolved at init from the CPU count
    /// and stored, so `opt.narenas` and `arenas.auto` agree on it.
    #[test]
    fn default_narenas_is_resolved_at_init() {
        let s = shared_with(Config::DEFAULT);
        let expect = Config::DEFAULT.effective_narenas(os::ncpus());
        assert!((4..=crate::config::MAX_AUTO_ARENAS).contains(&expect));
        assert_eq!(s.config.narenas, expect);
        assert_eq!(s.arenas.auto(), expect);
        assert_eq!(s.arenas.count(), expect);
        let s1 = shared_with(Config { narenas: 1, ..Config::DEFAULT });
        assert_eq!((s1.config.narenas, s1.arenas.auto()), (1, 1));
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
        // One refill spans `HALF / nslots` slabs (one 1024-slot slab at 64 bytes).
        let per_refill = HALF.div_ceil(CLASSES[c].nslots());
        assert_eq!(s.arena(a1).unwrap().classes[c].stats().committed, per_refill);
        assert_eq!(s.arena0.classes[c].stats().committed, per_refill);
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
        assert_eq!(s.arena0.classes[c].stats().empty, per_refill);
        assert_eq!(s.arena(a1).unwrap().classes[c].stats().empty, per_refill);
        // Direct paths against an explicit arena.
        let a = s.arena(a1).unwrap();
        let (d, _) = s.alloc_direct(a, c);
        assert_eq!(s.classify_free(d), Ok((c, a1)));
        assert_eq!(a.classes[c].desc(s.arena0.classes[c].locate(d).0).live(), 1);
        s.free_direct(a1, c, d);
        assert_eq!(a.classes[c].stats().empty, per_refill);
        // Destroying the heap frees the other-arena caches too.
        let before = s.meta.stats().allocated;
        unsafe { Heap::destroy(t.h, s) };
        assert!(s.meta.stats().allocated < before);
        core::mem::forget(t);
    }

    /// A shared class (above `tcache_max`) comes from arena 0 on the
    /// default path whatever the home arena, but `MALLOCX_ARENA(a)`
    /// keeps `a`, home or not (design 13.3), and a heap creates no
    /// `others` node for a class it never caches there (`Config::cap_for`).
    #[test]
    fn explicit_arena_keeps_a_shared_class_in_its_arena() {
        let s = shared();
        let a1 = s.arenas.create(s).unwrap();
        let t = TestHeap::new(s);
        let h = t.heap();
        let c = class_with(64 * 1024);
        assert!(s.config.shared_class(c));
        assert_eq!(s.config.cap_for(0, 0, c), 0);
        assert_eq!(s.config.cap_for(0, a1, c), 0);
        h.set_arena(s, a1);
        // Default path: arena 0.
        let (d, _) = h.alloc_zeroed(s, c);
        assert_eq!(s.classify_free(d), Ok((c, 0)));
        // The home arena named explicitly: stays home.
        let (p, _) = h.alloc_in(s, a1, c);
        assert_eq!(s.classify_free(p), Ok((c, a1)));
        assert_eq!(s.arena(a1).unwrap().classes[c].stats().committed, 1);
        // A foreign arena named explicitly, through a heap that caches
        // nothing of that class there: no node, no cache, right arena.
        let before = s.meta.stats().allocated;
        let (q, _) = h.alloc_in(s, 0, c);
        assert_eq!(s.classify_free(q), Ok((c, 0)));
        assert_eq!(s.meta.stats().allocated, before, "no others node for an uncached class");
        assert!(h.cache_in(0, c).is_none());
        assert!(h.cache(c).is_none());
        let st = h.stats();
        assert_eq!(st.allocated, 3 * 64 * 1024);
        h.free(s, d);
        h.free(s, p);
        h.free(s, q);
        assert_eq!(s.meta.stats().allocated, before);
        assert_eq!(h.stats().deallocated, 3 * 64 * 1024);
        assert_eq!(s.arena(a1).unwrap().classes[c].stats().empty, 1);
        assert_eq!(s.arena0.classes[c].stats().empty, 1);
    }

    /// `thread.arena` with no metadata for an `others` node drops the old
    /// home's caches (`drop_home_caches`); their counters are folded into
    /// `uncached` first, so `thread.allocated` and `thread.deallocated`
    /// do not move and keep counting afterwards.
    #[test]
    fn dropping_the_home_caches_keeps_the_thread_counters() {
        let s = shared();
        let t = TestHeap::new(s);
        let h = t.heap();
        let c = class_with(64);
        let p = t.alloc(c);
        let q = t.alloc(c);
        h.free(s, p);
        let before = h.stats();
        assert_eq!((before.allocated, before.deallocated), (128, 64));
        let meta_before = s.meta.stats().allocated;
        h.drop_home_caches(s);
        assert!(h.cache(c).is_none());
        assert!(s.meta.stats().allocated < meta_before, "the stack was freed");
        let after = h.stats();
        assert_eq!((after.allocated, after.deallocated), (before.allocated, before.deallocated));
        h.free(s, q);
        let again = h.stats();
        assert_eq!((again.allocated, again.deallocated), (128, 128));
        assert!(h.cache(c).is_some(), "the slow path created a new cache");
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
        let per_refill = HALF.div_ceil(CLASSES[c].nslots());
        assert_eq!(s.arena0.classes[c].stats().empty, per_refill);
        assert_eq!(s.arena(a1).unwrap().classes[c].stats().empty, per_refill);
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

    // Phase 1.5 task 8: the four cache invariants that were `debug_assert!`
    // abort in release. Each test builds the violating state by hand.

    #[test]
    #[should_panic(expected = "cache run invariant violated")]
    fn a_scheduled_pop_aborts_when_the_run_reaches_past_the_length() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let _p = t.alloc(c);
        let h = t.heap();
        let len = h.cache(c).unwrap().len() as u32;
        assert!(len > 1);
        let k = h.cache_ptr(c);
        unsafe {
            (*k).mode = MODE_COMPACT;
            (*k).run_end = len + 1;
            h.pop_compact_run(k, c, len);
        }
    }

    #[test]
    #[should_panic(expected = "refill of a cache that is not empty")]
    fn a_refill_of_a_cache_that_is_not_empty_aborts() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let _p = t.alloc(c);
        let h = t.heap();
        assert!(h.cache(c).unwrap().len() > 0);
        let a = s.arena(0).unwrap();
        let k = h.cache_ptr(c);
        unsafe { h.refill(s, a, k, c) };
    }

    #[test]
    #[should_panic(expected = "freeing a cache that was not flushed")]
    fn freeing_caches_that_were_not_flushed_aborts() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let _p = t.alloc(c);
        let h = t.heap();
        assert!(h.cache(c).unwrap().len() > 0);
        Heap::free_caches(s, &mut h.caches);
    }

    #[test]
    #[should_panic(expected = "others node requested for the home arena")]
    fn an_others_node_for_the_home_arena_aborts() {
        let s = shared();
        let t = TestHeap::new(s);
        let h = t.heap();
        let home = h.arena;
        assert!(h.other_caches(s, home, false).is_null());
        h.other_caches(s, home, true);
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
        // One automatic arena: the tests below reason about `arena0`.
        let config = Config { narenas: 1, ..Config::DEFAULT };
        assert!(unsafe { Shared::init(b.as_mut_ptr(), regions, config, false) });
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
