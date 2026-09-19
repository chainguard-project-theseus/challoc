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
//!   batch grouped by slab (a selection and a counting pass, no sort) and
//!   keep the lowest.
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
//! Signed pointers (plan Phase 2.5 task 7, `sys::pac`). On an arm64 host
//! whose kernel exposes pointer authentication (`HWCAP_PACA`/`PACG`;
//! `Shared::pac()`, the [`PUSH_PAC`] bit of the byte every `free` already
//! reads), every pointer stored in a cache's stack is signed with `pacda`,
//! the stack's base address as the modifier, and authenticated with
//! `autda` when it leaves (pop, scheduled pop, flush); the heap pointer in
//! the TLS slot is signed with the thread pointer. Entries move within a
//! stack signed (the modifier is the stack, not the position), a flush
//! orders them by their address bits (the PAC field masked off, no
//! authentication) and authenticates only the half it releases, and the
//! sentinels in the TLS slot are never signed (the compare against them
//! comes first). A corrupted entry faults
//! at the `autda` (FEAT_FPAC) or comes out non-canonical and faults on
//! first use, so a write into the metadata region cannot make `malloc`
//! return a chosen address. The stack base as the modifier survives
//! `set_arena`'s swap of whole header arrays (the stacks stay put) and
//! costs nothing: the pop and the push load it anyway.
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
use crate::rseq;
use crate::sys::pac;

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
    /// Consecutive flushes whose slot array was not already in address
    /// order (wrapping); zeroed by a flush that found it sorted. Drives the
    /// kept half's sort in [`Heap::flush`].
    unsorted_flushes: u8,
}

/// Cache modes: off (`tcache_compact_shift:0`), probing the class for
/// fragmentation every [`PROBE_MASK`] + 1 pops, compacting.
pub const MODE_OFF: u16 = 0;
pub const MODE_PROBE: u16 = 1;
pub const MODE_COMPACT: u16 = 2;

/// `Shared::push_flags` bit: `double_free_scan` (compare the pushed slot
/// against the top four cache entries).
pub const PUSH_SCAN: u8 = 1;
/// `Shared::push_flags` bit: `zero_on_free` (clear the block before the
/// push or the huge release).
pub const PUSH_ZERO: u8 = 2;
/// `Shared::push_flags` bit: pointer authentication is on (`sys::pac`,
/// plan Phase 2.5 task 7). Read by the pop as well as the push: every slot
/// pointer in a cache's stack is stored signed (`pacda`, modifier the
/// stack's base address) and authenticated on the way out (`autda`), and
/// the heap pointer in TLS is signed with the thread pointer. Set by `init`
/// when the kernel exposes `HWCAP_PACA`/`PACG` and `pac_pointers` is not
/// `false`; never set on x86_64.
pub const PUSH_PAC: u8 = 4;
/// `Shared::push_flags` bit: the per-CPU mode is on (design 15, plan Phase
/// 2.5 task 8; [`Shared::percpu`]). Both fast paths test it right after
/// the byte is loaded and, when it is set, take the out-of-line per-CPU
/// path ([`Heap::pop_percpu_mode`], [`percpu_free_fast`]) instead of the
/// thread cache; the thread paths themselves are untouched, and a heap
/// whose [`Heap::percpu`] is false (an explicit cache, a thread homed on
/// an explicit arena, `thread.tcache.enabled:false`) takes the thread
/// path from inside the per-CPU one.
pub const PUSH_PERCPU: u8 = 8;
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

/// Field offsets of [`CacheHeader`] for the rseq critical sections
/// (`rseq.rs`), which address the fields from `asm!`.
pub const OFF_LEN: usize = core::mem::offset_of!(CacheHeader, len);
pub const OFF_CAP: usize = core::mem::offset_of!(CacheHeader, cap);
pub const OFF_LOW_WATER: usize = core::mem::offset_of!(CacheHeader, low_water);
pub const OFF_ZEROED: usize = core::mem::offset_of!(CacheHeader, zeroed);
pub const OFF_SLOTS: usize = core::mem::offset_of!(CacheHeader, slots);
pub const OFF_ALLOCS: usize = core::mem::offset_of!(CacheHeader, allocs);
pub const OFF_COMPACT_MASK: usize = core::mem::offset_of!(CacheHeader, compact_mask);

/// The caches of one concurrency id in the per-CPU mode (design 15): the
/// same header array as a heap's home caches, one line per class, so the
/// per-CPU paths and the thread paths share [`CacheHeader`] and its stack
/// layout. The table is `possible_cpus` of these, contiguous, reserved
/// zeroed from the metadata region at init and touched only as ids are
/// used: a zeroed header is `len == cap == 0`, which both fast paths treat
/// as "slow path", and the first slow path on it installs the stack
/// ([`Heap::percpu_ensure`]). Memory is bounded by the number of possible
/// CPUs, never by the number of threads.
#[repr(C, align(64))]
pub struct CpuCache {
    pub caches: [CacheHeader; NCLASSES],
}

/// Distance between two ids' tables ([`rseq::STRIDE`]).
pub const CPU_CACHE_SIZE: usize = core::mem::size_of::<CpuCache>();
const _: () = assert!(CPU_CACHE_SIZE == NCLASSES * CACHE_HEADER);

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
        unsorted_flushes: 0,
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
    /// and diagnostics), as stored: signed when `Shared::pac()`
    /// ([`CacheHeader::plain`]).
    pub fn run(&self) -> &[*mut u8] {
        debug_assert!(self.run_end <= self.low_water && self.low_water <= self.len);
        if self.run_end == 0 {
            return &[];
        }
        // SAFETY: `run_end <= len <= cap` pointers exist at `slots`.
        unsafe { core::slice::from_raw_parts(self.slots, self.run_end as usize) }
    }

    /// Cached pointers, bottom of the stack first (for tests and
    /// diagnostics), as stored: signed when `Shared::pac()`
    /// ([`CacheHeader::plain`]).
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

    /// Sign every entry of `entries` (plain pointers) in place with the
    /// stack base `slots` as the modifier ([`PUSH_PAC`]).
    #[inline]
    fn sign_all(slots: *mut *mut u8, entries: &mut [*mut u8]) {
        let m = slots as usize;
        for e in entries {
            *e = pac::sign(*e as usize, m) as *mut u8;
        }
    }

    /// Authenticate every entry of `entries` (signed) in place.
    #[inline]
    fn auth_all(slots: *mut *mut u8, entries: &mut [*mut u8]) {
        let m = slots as usize;
        for e in entries {
            *e = pac::auth(*e as usize, m) as *mut u8;
        }
    }

    /// The plain pointer a stored entry names: authenticated when `pac`
    /// (`Shared::pac()`), the entry itself otherwise. For diagnostics and
    /// tests reading [`CacheHeader::cached`] or [`CacheHeader::run`], which
    /// return the entries as stored.
    pub fn plain(&self, e: *mut u8, pac: bool) -> *mut u8 {
        if pac { pac::auth(e as usize, self.slots as usize) as *mut u8 } else { e }
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
    /// Bytes counted per operation rather than derived from a cache
    /// header: huge runs, slots served directly from a slab (no cache, or
    /// `MALLOCX_TCACHE_NONE`) and, in the per-CPU mode, every pop from and
    /// push onto a CPU cache (a cache shared by the threads of a CPU
    /// cannot be folded into one thread's counters). The thread caches
    /// are derived from their headers in [`Heap::stats`].
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
    /// This heap's plain paths use the per-CPU caches (design 15): the
    /// mode is on ([`Shared::percpu`]), this is a thread heap (`bound`),
    /// its caches are enabled and its home arena is an automatic one
    /// (`Arenas::auto`; a thread moved to an explicit arena by
    /// `thread.arena` allocates from that arena through its own caches, as
    /// under jemalloc's `percpu_arena`). Recomputed by [`Heap::set_arena`]
    /// and [`Heap::set_tcache_enabled`]. Read only behind [`PUSH_PERCPU`].
    percpu: bool,
    /// The owning thread's rseq area (`rseq::area` at creation; the heap is
    /// created on its thread), read by the per-CPU paths; null when the
    /// mode is off.
    rseq_area: *mut u8,
    /// `slot_randomize`'s xorshift state (plan Phase 2 task 3): 0 until the
    /// heap's first randomised claim seeds it from `getrandom`, so the
    /// default configuration makes no syscall per heap creation.
    rng: u64,
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
    /// The opt-in hardening a push applies, as [`PUSH_SCAN`]
    /// (`double_free_scan`, design 6.3 step 3: every push compares the
    /// slot against the top four cache entries and aborts on a match) and
    /// [`PUSH_ZERO`] (`zero_on_free`: the slot's class-size bytes are
    /// cleared before the push, and a huge run's usable bytes before its
    /// release), and [`PUSH_PAC`] (signed cache and heap pointers where
    /// the CPU has pointer authentication, plan Phase 2.5 task 7; read by
    /// the pop too). Written once by `init`; kept on the line every `free`
    /// reads, and zero in the default configuration on a CPU without PAC,
    /// so the fast path tests one byte and takes one predicted branch
    /// (plan Phase 2 task 3).
    push_flags: u8,
    pub regions: Regions,
    /// The per-CPU mode's table and state (design 15; [`PUSH_PERCPU`]).
    /// After `regions` so the thread paths' first line is as it was; the
    /// per-CPU paths read `base`, `n` and the flag byte, two lines.
    pub percpu: Percpu,
    /// The sized fast path's guard test per class (plan Phase 2.5 task
    /// 4c): a dense array (8 bytes per class, seven lines for all 52) so
    /// `sized_slot` never touches the 384-byte class table line, whose
    /// only other use on that path was the watermark it no longer reads.
    guard_test: [GuardTest; NCLASSES],
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
    /// The parsed `malloc_conf` (design section 12). Keys that already have
    /// a knob are applied by `init`; the rest are read by later phases.
    pub config: Config,
    key: ThreadKey,
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
    fn of(table: &SlabTable) -> GuardTest {
        let lg_spp = table.slabs_per_page().trailing_zeros();
        GuardTest { add: table.slabs_per_page() as u32, mask: table.guard_mask().wrapping_shl(lg_spp) }
    }

    /// Whether `slab` is a guard slab of the class this pair describes.
    #[inline(always)]
    fn is_guard(self, slab: usize) -> bool {
        (slab as u32).wrapping_add(self.add) & self.mask == 0
    }
}

/// The per-CPU mode (design 15, plan Phase 2.5 task 8): the table of
/// [`CpuCache`]s indexed by the running thread's rseq `mm_cid`, or why
/// there is none. Built by [`Shared::init`] from the configuration
/// (`percpu_arena`), the rseq probe (`rseq::probe`) and the possible-CPU
/// count; read-only afterwards.
#[repr(C)]
pub struct Percpu {
    /// The table: `n` contiguous [`CpuCache`]s; null when the mode is off.
    base: *mut u8,
    /// `__rseq_offset`: thread pointer to the thread's rseq area.
    offset: isize,
    /// Table entries: the kernel's `nr_cpu_ids`, the bound on `mm_cid`.
    n: u32,
    state: PercpuState,
    /// What glibc registered, for the report.
    probe: rseq::Probe,
    /// `percpu_arena:percpu` was asked for (the report notes a fallback).
    forced: bool,
}

/// Why the per-CPU mode is on or off ([`Percpu::state`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PercpuState {
    /// Caches keyed by `mm_cid`.
    On = 0,
    /// `percpu_arena:off`.
    OffKey = 1,
    /// `percpu_arena:auto`, the key's default: thread caches until the
    /// default is decided with the numbers in front of the human (plan
    /// Phase 2.5 task 8, PROGRESS.md); `percpu` is the opt-in.
    OffAuto = 6,
    /// The rseq probe failed (`rseq::Probe::verdict` says how).
    OffProbe = 2,
    /// `/sys/devices/system/cpu/possible` could not be read.
    OffCpus = 3,
    /// `double_free_scan:true`: the scan of the top entries is not written
    /// into the critical sections, so the two keys exclude each other and
    /// the hardening wins.
    OffScan = 4,
    /// The metadata region could not hold the table.
    OffMemory = 5,
}

impl PercpuState {
    pub const fn name(self) -> &'static str {
        match self {
            PercpuState::On => "on",
            PercpuState::OffKey => "off",
            PercpuState::OffAuto => "off (auto: opt in with percpu_arena:percpu)",
            PercpuState::OffProbe => "off (rseq)",
            PercpuState::OffCpus => "off (no cpu count)",
            PercpuState::OffScan => "off (double_free_scan)",
            PercpuState::OffMemory => "off (no metadata)",
        }
    }
}

// SAFETY: `base` names a process-lifetime metadata reservation whose
// headers are only ever mutated inside rseq critical sections (one running
// thread per id) or read racily with volatile loads for diagnostics; the
// other fields are written once by `init`.
unsafe impl Sync for Percpu {}
unsafe impl Send for Percpu {}

impl Percpu {
    const OFF: Percpu =
        Percpu { base: ptr::null_mut(), offset: 0, n: 0, state: PercpuState::OffKey, probe: rseq::Probe::NONE, forced: false };

    /// Decide the mode and reserve the table. `allowed` is the caller's
    /// say (init passes true; the unit tests' `Shared` builders false, so
    /// their thread-cache assertions hold on a host with `mm_cid`): the
    /// mode is on when it is allowed, the key is `percpu` and the probe
    /// passes (`PercpuArena::enabled`), `double_free_scan` is off, the
    /// possible-CPU count is readable and the metadata region has room for
    /// `n * CPU_CACHE_SIZE` bytes (reserved zeroed, touched as ids are
    /// used). `auto`, the key's default, keeps thread caches
    /// ([`PercpuState::OffAuto`]): the mode measured 8..24% behind thread
    /// caches on `quick`'s rows and worse on the Valkey model on both
    /// reference VMs (PROGRESS.md, `bench/results/2026-09-19-*-task8-*`),
    /// so it is not on by default; the plan leaves the default to the
    /// human, and the key's default lives in `config.rs`, another lane's
    /// file, so this is where `auto` is read as "not yet". Off Linux there
    /// is no rseq and the mode is off.
    fn build(meta: &Meta, config: &Config, allowed: bool) -> Percpu {
        use crate::config::PercpuArena;
        let probe = rseq::probe();
        let forced = config.percpu_arena.forced();
        let off = |state: PercpuState| Percpu { state, probe, forced, ..Percpu::OFF };
        match config.percpu_arena {
            PercpuArena::Off => return off(PercpuState::OffKey),
            PercpuArena::Auto => return off(PercpuState::OffAuto),
            PercpuArena::Percpu => {}
        }
        if !allowed || !config.percpu_arena.enabled(probe.mm_cid_usable()) {
            return off(PercpuState::OffProbe);
        }
        if config.double_free_scan {
            return off(PercpuState::OffScan);
        }
        #[cfg(target_os = "linux")]
        {
            let Some(n) = rseq::possible_cpus() else { return off(PercpuState::OffCpus) };
            let n = n.clamp(1, crate::arena::MAX_ARENAS as u32);
            let Some(base) = meta.reserve_committed(n as usize * CPU_CACHE_SIZE, 64) else {
                return off(PercpuState::OffMemory);
            };
            Percpu { base: base as *mut u8, offset: probe.offset, n, state: PercpuState::On, probe, forced }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = meta;
            off(PercpuState::OffProbe)
        }
    }

    /// Whether the caches are keyed by `mm_cid`.
    #[inline(always)]
    pub fn on(&self) -> bool {
        self.state == PercpuState::On
    }

    pub fn state(&self) -> PercpuState {
        self.state
    }

    /// The rseq probe the decision was made on.
    pub fn probe(&self) -> rseq::Probe {
        self.probe
    }

    /// `percpu_arena:percpu` was configured.
    pub fn forced(&self) -> bool {
        self.forced
    }

    /// Table entries (possible CPUs); 0 when off.
    pub fn ids(&self) -> u32 {
        self.n
    }

    /// The critical sections' view of the table for `class`: the id-0
    /// header of that class and the id bound.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[inline(always)]
    fn table(&self, class: usize) -> rseq::Table {
        debug_assert!(self.on() && class < NCLASSES);
        // SAFETY: `class < NCLASSES` and the table has `n >= 1` entries.
        rseq::Table { base: unsafe { self.base.add(class * CACHE_HEADER) }, n: self.n }
    }

    /// The header of `(id, class)` for diagnostics and statistics (the
    /// fields are read racily with volatile loads; the owner may be
    /// writing them from a critical section on another CPU).
    pub fn header(&self, id: u32, class: usize) -> Option<*const CacheHeader> {
        if !self.on() || id >= self.n || class >= NCLASSES {
            return None;
        }
        // SAFETY: inside the table.
        Some(unsafe { self.base.add(id as usize * CPU_CACHE_SIZE + class * CACHE_HEADER) } as *const CacheHeader)
    }

    /// Installed headers and cached slots over the whole table (a racy
    /// snapshot for `stats.percpu.*` and the report).
    pub fn snapshot(&self) -> (usize, usize) {
        let (mut headers, mut slots) = (0usize, 0usize);
        for id in 0..self.n {
            for class in 0..NCLASSES {
                let h = self.header(id, class).unwrap();
                // SAFETY: a live header; racy reads of two `u32`s.
                let (cap, len) = unsafe {
                    (ptr::read_volatile(&raw const (*h).cap), ptr::read_volatile(&raw const (*h).len))
                };
                if cap != 0 {
                    headers += 1;
                    slots += len as usize;
                }
            }
        }
        (headers, slots)
    }
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
            // Design 11: under an effective limit below 1 GiB `thp` is
            // forced to `never` in the stored configuration (the report
            // says so); the policy's `high` baseline is the count at init.
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

    /// The opt-in hardening every push applies ([`PUSH_SCAN`],
    /// [`PUSH_ZERO`]); 0 by default.
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
    /// compare, plan Phase 2.5 task 4c), and the guard test is one
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
    fn sized_slot(&self, p: *mut u8, hint: Option<usize>) -> Option<usize> {
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

    /// [`checked_slot`] without the arena test, for the per-CPU push: a
    /// CPU cache takes verified slots of every arena and its flush routes
    /// them by the owner array ([`release_routed`]), as the sized path's
    /// cache already does.
    ///
    /// [`checked_slot`]: Shared::checked_slot
    /// [`release_routed`]: Shared::release_routed
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[inline(always)]
    fn checked_slot_any(&self, p: *mut u8) -> Option<usize> {
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
            // SAFETY: fresh, exclusively owned; created on the owning
            // thread, whose rseq area this is.
            unsafe {
                (*h).bound = true;
                (*h).rseq_area = Self::rseq_area_for(shared);
                (*h).percpu = Self::percpu_for(shared, true, true, arena);
            }
        }
        h
    }

    /// Whether a heap with these properties takes the per-CPU paths
    /// ([`Heap::percpu`]).
    fn percpu_for(shared: &Shared, bound: bool, tcache_enabled: bool, arena: u16) -> bool {
        shared.percpu_on() && bound && tcache_enabled && (arena as u32) < shared.arenas.auto()
    }

    /// The calling thread's rseq area when the mode is on, else null.
    fn rseq_area_for(shared: &Shared) -> *mut u8 {
        #[cfg(target_os = "linux")]
        {
            if shared.percpu_on() { rseq::area(shared.percpu.offset) } else { ptr::null_mut() }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = shared;
            ptr::null_mut()
        }
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
            ptr::write(&raw mut (*h).percpu, false);
            ptr::write(&raw mut (*h).rseq_area, ptr::null_mut());
            ptr::write(&raw mut (*h).rng, 0);
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
        heap.flush_thread_caches(shared);
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
        self.percpu = Self::percpu_for(shared, self.bound, self.tcache_enabled, arena);
    }

    /// Whether this heap's plain paths use the per-CPU caches (see the
    /// field).
    #[inline]
    pub fn percpu(&self) -> bool {
        self.percpu
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
        self.percpu = Self::percpu_for(shared, self.bound, on, self.arena);
        if on {
            return;
        }
        self.flush_thread_caches(shared);
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
        let flags = shared.push_flags();
        let fast = if flags & PUSH_PERCPU != 0 {
            self.pop_percpu_mode(class, flags).map(|r| r.0)
        } else {
            self.alloc_fast(class, flags)
        };
        match fast {
            Some(p) => p,
            None => self.alloc_slow(shared, class).0,
        }
    }

    /// The cache pop alone: `None` when the cache is empty (or still the
    /// sentinel) and the caller must take a slow path. `flags` is
    /// `Shared::push_flags` (only [`PUSH_PAC`] matters to a pop).
    #[inline(always)]
    pub fn alloc_fast(&mut self, class: usize, flags: u8) -> Option<*mut u8> {
        let cache = self.cache_ptr(class);
        // SAFETY: this thread's own header, unused (`len == 0`) or with a
        // stack of `len` entries.
        unsafe {
            let len = (*cache).len;
            if len == 0 {
                return None;
            }
            Some(self.pop(cache, class, len, flags & PUSH_PAC != 0).0)
        }
    }

    /// Fast path of `calloc`: the flag is true when the memory is known to
    /// read as zero, so the caller can skip its `memset`.
    #[inline(always)]
    pub fn alloc_zeroed(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        let flags = shared.push_flags();
        if flags & PUSH_PERCPU != 0 {
            return match self.pop_percpu_mode(class, flags) {
                Some(r) => r,
                None => self.alloc_slow(shared, class),
            };
        }
        let cache = self.cache_ptr(class);
        // SAFETY: as in `alloc`.
        unsafe {
            let len = (*cache).len;
            if len == 0 {
                return self.alloc_slow(shared, class);
            }
            self.pop(cache, class, len, shared.pac())
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
                return self.pop(cache, class, len, shared.pac());
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
    /// runs `tick`, whose GC may flush this cache, and reloads it. `pac`
    /// is `Shared::pac()`: whether the entry is stored signed.
    #[inline(always)]
    unsafe fn pop(&mut self, cache: *mut CacheHeader, class: usize, len: u32, pac: bool) -> (*mut u8, bool) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let allocs = (*cache).allocs.wrapping_add(1);
            (*cache).allocs = allocs;
            if ((allocs as u32) & (*cache).compact_mask) == 0 {
                return self.pop_compact(cache, class, len);
            }
            Self::pop_top(cache, len, pac)
        }
    }

    /// The ordinary pop of the top entry, `allocs` already counted. With
    /// `pac` the entry is authenticated against the stack's base address
    /// (`autda`; [`CacheHeader::plain`]): a corrupted entry faults here on
    /// FEAT_FPAC, or comes out non-canonical and faults on its first use.
    ///
    /// # Safety
    /// As for [`Heap::pop`].
    #[inline(always)]
    unsafe fn pop_top(cache: *mut CacheHeader, len: u32, pac: bool) -> (*mut u8, bool) {
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
            let slots = (*cache).slots();
            let p = *slots.add(len as usize);
            let p = if pac { pac::auth(p as usize, slots as usize) as *mut u8 } else { p };
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
            let pac = (*self.shared).pac();
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
                        return Self::pop_top(cache, len, pac);
                    }
                    if !Self::fragmented(shared, (*cache).arena, class) {
                        return Self::pop_top(cache, len, pac);
                    }
                    cs.note_compact_mode(true);
                    (*cache).set_mode(MODE_COMPACT, &shared.config);
                    return self.pop_compact_run(cache, class, len);
                }
                _ => return Self::pop_top(cache, len, pac),
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
            let pac = (*self.shared).pac();
            match (*cache).mode {
                MODE_COMPACT => {}
                _ => return Self::pop_top(cache, len, pac),
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
                    return Self::pop_top(cache, len, pac);
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
            // Entries move within the stack signed (the modifier is the
            // stack's base, not the entry's position); only the one leaving
            // is authenticated.
            let p = if pac { pac::auth(p as usize, slots as usize) as *mut u8 } else { p };
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
            let claim = self.claim(shared, shared.owner(arena).class(class), out);
            let n = claim.n;
            if n == 0 {
                return;
            }
            shared.owner(arena).class(class).note_compact(n);
            out[..n].reverse();
            if shared.pac() {
                CacheHeader::sign_all(slots, &mut out[..n]);
            }
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
                if self.percpu {
                    self.percpu_free_slow(shared, class, p)
                } else if arena == self.arena {
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
        if shared.zero_on_free() {
            shared.zero_huge(page);
        }
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

    /// A slot of `class` was claimed directly (`MALLOCX_TCACHE_NONE`) or
    /// popped from a per-CPU cache on this thread: count it.
    #[inline]
    pub fn count_direct_alloc(&mut self, class: usize) {
        self.count_uncached_alloc(CLASSES[class].size());
    }

    /// A slot of `class` was released directly or pushed onto a per-CPU
    /// cache on this thread: count it.
    #[inline]
    pub fn count_direct_free(&mut self, class: usize) {
        self.count_uncached_free(CLASSES[class].size());
    }

    /// Push a checked slot of `class` (home arena) onto its cache.
    #[inline(always)]
    fn push(&mut self, shared: &Shared, class: usize, p: *mut u8) {
        if !self.push_fast(class, p, shared.push_flags()) {
            self.free_slow(shared, class, p);
        }
    }

    /// The cache push alone: `false` when the cache is full (or still the
    /// sentinel) and the caller must take a slow path. No counter moves:
    /// the push count is implied by `len`, `allocs` and `moved`. `zeroed`
    /// is written only when the pushed slot lands inside the zero prefix,
    /// which is rare (the prefix is usually already zero). `flags` is
    /// `Shared::push_flags`.
    #[inline(always)]
    pub fn push_fast(&mut self, class: usize, p: *mut u8, flags: u8) -> bool {
        let cache = self.cache_ptr(class);
        // SAFETY: this thread's own header, unused (`len == cap == 0`, so
        // nothing is written) or with a stack of `cap` slots.
        unsafe { Self::push_cache_flags(cache, p, flags) }
    }

    /// [`Heap::push_cache`] behind the opt-in hardening: with `flags` zero
    /// (the default) it is the plain push after one byte test. With
    /// `zero_on_free` it declines (`false`), so the caller takes the cold
    /// slow path it already has for a full cache, and that path
    /// ([`Heap::free_slow_in`]) clears the slot and pushes it: the clear
    /// must not be a call from here, since a call inside the inlined fast
    /// path costs every default `free` a callee-saved register spill in
    /// its prologue (measured: larson1 +1.9% on aarch64 with the `memset`
    /// call inlined here, the `sdallocx` frame 0x20 -> 0x50 with `x19`
    /// saved). With `double_free_scan` alone it is the scanning push.
    ///
    /// # Safety
    /// As for [`Heap::push_cache`].
    #[inline(always)]
    unsafe fn push_cache_flags(cache: *mut CacheHeader, p: *mut u8, flags: u8) -> bool {
        // The two default configurations first, one byte compare each: no
        // hardening (x86_64, a CPU without PAC) and signed pointers alone
        // (an arm64 host with PAC).
        if flags == 0 {
            // SAFETY: guaranteed by the caller.
            return unsafe { Self::push_cache(cache, p, false, false) };
        }
        if flags == PUSH_PAC {
            // SAFETY: guaranteed by the caller.
            return unsafe { Self::push_cache(cache, p, false, true) };
        }
        if flags & PUSH_ZERO != 0 {
            return false;
        }
        // SAFETY: guaranteed by the caller.
        unsafe { Self::push_cache(cache, p, flags & PUSH_SCAN != 0, flags & PUSH_PAC != 0) }
    }

    /// Push onto `cache` unless it is full. With `scan`, compare `p`
    /// against the top four entries first (design 6.3 step 3, plan Phase
    /// 1.5 task 9c): a match is a double free whose first copy is still
    /// cached, which the bit test cannot see, and it aborts before the
    /// second copy is stored. Four unconditional compares from the line
    /// the push writes to; a cache holding fewer than four takes the loop.
    ///
    /// With `pac` the entry is stored signed with the stack's base address
    /// as the modifier (`pacda`, [`PUSH_PAC`]); the scan compares signed
    /// against signed, which is the same test (one key, one modifier, so
    /// equal addresses give equal words).
    ///
    /// # Safety
    /// `cache` is a header owned by this heap, unused or with a stack.
    #[inline(always)]
    unsafe fn push_cache(cache: *mut CacheHeader, p: *mut u8, scan: bool, pac: bool) -> bool {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let len = (*cache).len;
            if len == (*cache).cap {
                return false;
            }
            let slots = (*cache).slots();
            let e = if pac { pac::sign(p as usize, slots as usize) as *mut u8 } else { p };
            if scan && len != 0 {
                let top = slots.add(len as usize);
                let hit = if len >= 4 {
                    (*top.sub(1) == e) | (*top.sub(2) == e) | (*top.sub(3) == e) | (*top.sub(4) == e)
                } else {
                    (1..=len as usize).any(|i| *top.sub(i) == e)
                };
                if hit {
                    cached_twice(p);
                }
            }
            *slots.add(len as usize) = e;
            (*cache).len = len + 1;
            if len < (*cache).zeroed {
                (*cache).zeroed = len;
            }
        }
        true
    }

    /// The batch claim behind a refill or a run rebuild: `cs.claim`, or
    /// with `slot_randomize` `cs.claim_random` with this heap's xorshift
    /// (the scan starts at a random bitmap word and the batch is shuffled;
    /// design section 4). The key is read from the configuration here, on
    /// the slow path, so the fast paths carry nothing for it.
    #[inline]
    fn claim(&mut self, shared: &Shared, cs: &crate::class::ClassState, out: &mut [*mut u8]) -> crate::class::Claim {
        if shared.config.slot_randomize {
            let rng = self.rng();
            cs.claim_random(&shared.meta, out, rng)
        } else {
            cs.claim(&shared.meta, out)
        }
    }

    /// This heap's xorshift state for `slot_randomize`, seeded on first use
    /// from `getrandom` (mixed with the heap's address and thread id so two
    /// heaps seeded in the same instant differ even if the kernel returned
    /// the same bytes, and never zero, which xorshift cannot leave).
    #[cold]
    #[inline(never)]
    fn rng(&mut self) -> &mut u64 {
        if self.rng == 0 {
            let mut seed = os::seed();
            seed ^= (self as *const Heap as u64).rotate_left(17) ^ ((self.tid as u64) << 32);
            if seed == 0 {
                seed = 0x9E37_79B9_7F4A_7C15;
            }
            self.rng = seed;
        }
        &mut self.rng
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
        if shared.config.shared_class(class) {
            return self.alloc_slow_in(shared, 0, class);
        }
        if self.percpu {
            return self.percpu_alloc_slow(shared, class);
        }
        self.alloc_slow_in(shared, self.arena, class)
    }

    /// Allocation of `class` from another arena than the home one: through
    /// this heap's cache for that arena if the `others` list has one with
    /// entries, else the slow path, whose `ensure_cache` creates the node
    /// and the cache only for a class this heap caches there
    /// (`Config::cap_for`) and serves the rest from the bitmaps.
    #[cold]
    #[inline(never)]
    fn alloc_other(&mut self, shared: &Shared, arena: u16, class: usize) -> (*mut u8, bool) {
        let node = self.other_caches(shared, arena, false);
        if !node.is_null() {
            // SAFETY: `node` is a live block owned by this heap.
            let cache = unsafe { Self::node_cache_ptr(node, class) };
            // SAFETY: unused (`len == 0`) or this heap's cache.
            let len = unsafe { (*cache).len };
            if len != 0 {
                // SAFETY: a real cache with `len` entries.
                return unsafe { self.pop(cache, class, len, shared.pac()) };
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
            self.pop(cache, class, len, shared.pac())
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
    /// one: onto this heap's cache for that arena if the `others` list has
    /// one with room (xmalloc-test's consumers take this path on every
    /// free: a list walk and a push, no policy test), else the slow path,
    /// whose `ensure_cache` creates the node and the cache only for a
    /// class this heap caches there (`Config::cap_for`) and releases the
    /// rest straight to the owner's bitmap.
    #[cold]
    #[inline(never)]
    fn free_other(&mut self, shared: &Shared, arena: u16, class: usize, p: *mut u8) {
        let node = self.other_caches(shared, arena, false);
        if !node.is_null() {
            // SAFETY: `node` is a live block owned by this heap; the cache
            // is unused (`len == cap == 0`) or ours.
            if unsafe { Self::push_cache_flags(Self::node_cache_ptr(node, class), p, shared.push_flags()) } {
                return;
            }
        }
        self.free_slow_in(shared, arena, class, p)
    }

    /// The slow free path for any arena. With `zero_on_free` every cached
    /// free arrives here (the fast push declines, see
    /// [`Heap::push_cache_flags`]) and the slot is cleared first: the
    /// class-size bytes, from the class the caller verified (the bit test
    /// on the plain path; the size hint on the sized path, as the push
    /// trusts it, so a stale pointer of the right shape is cleared as it
    /// is cached, exactly as it would be handed out; design 6.3).
    fn free_slow_in(&mut self, shared: &Shared, arena: u16, class: usize, p: *mut u8) {
        if shared.zero_on_free() {
            // SAFETY: `p` is a slot start of `class` (the caller's check).
            unsafe { zero_slot(class, p) };
        }
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
            if !Self::push_cache(cache, p, shared.double_free_scan(), shared.pac()) {
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
        // must not create an `others` node. This is the one place the
        // placement rule is consulted on the allocation and free paths.
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
                    unsorted_flushes: 0,
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
            let claim = self.claim(shared, arena.class(class), out);
            arena.class(class).note_refill(claim.n);
            out[..claim.n].reverse();
            if shared.pac() {
                CacheHeader::sign_all((*cache).slots(), &mut out[..claim.n]);
            }
            (*cache).len = claim.n as u32;
            (*cache).moved = (*cache).moved.wrapping_add(claim.n as u64);
            (*cache).low_water = claim.n as u32;
            (*cache).run_end = claim.n as u32;
            (*cache).zeroed = if claim.zeroed { claim.n as u32 } else { 0 };
            claim.n as u32
        }
    }

    /// Release the `n` highest-address entries of a cache to their slabs
    /// (in `arena`, which owns them) in one batch grouped by slab and keep
    /// the rest. Selecting by address sends the holes to the high slabs,
    /// which is what lets them drain (module doc). Until Phase 2.5 task 1b
    /// this was one `sort_unstable` of the whole slot array, which under
    /// tiny-object churn was 8..9% of glibc-simple and 5% of the Python
    /// json row on both VMs, the largest single term wherever challoc
    /// trailed mimalloc or tcmalloc by more than 10%
    /// (`bench/results/2026-09-19-*-gce-flush-sort.md`). Now:
    ///
    /// - A slot array already in address order (a cache pushed in the order
    ///   a producer's refill batch was handed out, xmalloc-test's consumer
    ///   side, where the old sort took ipnsort's O(n) path at about 1.25k
    ///   cycles per flush) is split directly: `is_sorted` is one pass that
    ///   stops at the first inversion, the released half is sorted and so
    ///   grouped, and the kept half stays sorted for the next flush.
    /// - Otherwise `select_nth_unstable` on the address (O(len), in place)
    ///   puts the `n` highest at the top, and [`group_by_slab`] groups them
    ///   by slab (a counting pass over the slab numbers, O(n); nothing when
    ///   they are one slab); `release_owned` needs nothing more.
    /// - The kept half is left as the selection leaves it, except on the
    ///   first two consecutive unsorted flushes of a cache and every 32nd
    ///   after that (`unsorted_flushes`), when it is sorted: a cache pushed
    ///   in order that met one unsorted flush (its first fill, a producer
    ///   moving to a lower slab) gets back onto the sorted path, where a
    ///   scrambled kept half would have kept it on the selection forever
    ///   (xmalloc-test +16% that way; the old sort re-established the order
    ///   at every flush), while a cache pushed in arbitrary order pays two
    ///   sorts of half the array and then one in 32 (the unconditional
    ///   kept sort cost glibc-simple and Python their whole gain). The run
    ///   is dropped in every case and rebuilt by the scheduled pop (design
    ///   6.4); the zero prefix is given up.
    ///
    /// Candidates rejected on the way, all in the results file: a selection
    /// followed by a sort of the released half (the selection scrambles it,
    /// so that is a full sort: xmalloc +51%); an in-place bucket pass over
    /// the whole array by address (about 10 cycles per entry, the old sort's
    /// cost on random input and ten times its cost on sorted input: xmalloc
    /// +49%); the selection alone (xmalloc +21%, no self-healing).
    ///
    /// Double frees. `release_owned` aborts on a duplicate inside the
    /// released batch (a slab's slots are contiguous after the bucket pass
    /// and land on one scratch bitmap, so the second copy hits a set bit; a
    /// duplicate in two groups of one slab hits a clear bit in the class
    /// bitmap). A pair kept together stays cached and consistent (both
    /// copies in the cache, the bit set) until a flush releases it. The
    /// pair that straddles the split would have one copy released, its bit
    /// cleared, and the other still cached to be handed out over a slot a
    /// refill can claim again: after the split every kept entry is at most
    /// `kept_max = slots[rest_len - 1]` and every released entry at least
    /// that, so a straddling pair is exactly a released entry equal to
    /// `kept_max`, and the lowest released address (found by the bucket
    /// pass's first sweep, or `slots[rest_len]` on the sorted path) is
    /// compared with it. One compare, the same strength as the sorted
    /// form's neighbour compare at the split; a pass over the kept half was
    /// measured at 4.9% / 7.3% of xmalloc-test and is not done
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
            let pac = shared.pac();
            let slots = (*cache).slots();
            // With signed entries the selection, the sortedness test and
            // the kept sort order by the address bits alone (`!pac::mask()`
            // masks the PAC field, one `and` per compare); only the
            // released half is authenticated, once, before the release,
            // and the kept half stays as it is. Authenticating the whole
            // stack and re-signing the kept half was measured first:
            // xmalloc-test -15.8% on the C4A (the consumer's flushes are
            // the sorted path, 1.25k cycles each, and the two passes
            // doubled them; `bench/results/2026-09-19-lane-a-pac-pointers.md`).
            // The keyed forms only when signed: `sort_unstable_by_key` and
            // friends take a comparator closure and lose the primitive
            // pointer sort's specialisations, which costs every flush even
            // when the key is the identity.
            let addr_mask = !pac::mask();
            let key = |e: &*mut u8| (*e as usize) & addr_mask;
            let all = (*cache).entries();
            let rest_len = all.len() - n;
            let kept_max;
            let sorted = rest_len > 0 && if pac { all.is_sorted_by_key(key) } else { all.is_sorted() };
            if sorted {
                // Already in address order: the split is direct, the
                // released half is sorted and so grouped, and the kept half
                // stays sorted for the next flush.
                kept_max = *all.get_unchecked(rest_len - 1);
                (*cache).unsorted_flushes = 0;
            } else {
                kept_max = if rest_len > 0 {
                    // `all[..rest_len - 1] <= all[rest_len - 1] <= all[rest_len..]`.
                    if pac {
                        all.select_nth_unstable_by_key(rest_len - 1, key);
                    } else {
                        all.select_nth_unstable(rest_len - 1);
                    }
                    *all.get_unchecked(rest_len - 1)
                } else {
                    ptr::null_mut()
                };
                let k = (*cache).unsorted_flushes;
                (*cache).unsorted_flushes = k.wrapping_add(1);
                if rest_len > 1 && (k < 2 || k % 32 == 0) {
                    if pac {
                        all.get_unchecked_mut(..rest_len).sort_unstable_by_key(key);
                    } else {
                        all.get_unchecked_mut(..rest_len).sort_unstable();
                    }
                }
            }
            let high = all.get_unchecked_mut(rest_len..);
            if pac {
                // The release needs plain pointers. A corrupted entry
                // faults here (FEAT_FPAC) or in `release_owned`.
                CacheHeader::auth_all(slots, high);
            }
            let low = if sorted { *high.get_unchecked(0) } else { group_by_slab(shared, class, high) };
            let kept_max_plain = if pac && rest_len > 0 { pac::auth(kept_max as usize, slots as usize) as *mut u8 } else { kept_max };
            if low == kept_max_plain {
                straddling_double_free(low, class);
            }
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

    /// Return every cached slot to its slab: this heap's caches and, in the
    /// per-CPU mode, the caches of the CPU the thread is running on.
    /// Caches stay allocated.
    pub fn flush_all(&mut self, shared: &Shared) {
        if self.percpu {
            self.percpu_drain(shared);
        }
        self.flush_thread_caches(shared);
    }

    /// [`Heap::flush_all`] for this heap's own caches alone (thread exit,
    /// `thread.tcache.enabled:false`: the CPU caches are not the thread's).
    pub fn flush_thread_caches(&mut self, shared: &Shared) {
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
        if self.percpu {
            self.percpu_gc(shared);
        }
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
            if now.wrapping_sub(self.last_gc) >= shared.gc_interval() {
                self.last_gc = now;
                self.gc(shared);
            }
            crate::purge::on_tick(shared, now);
        }
    }
}

/// The per-CPU paths (design 15, plan Phase 2.5 task 8). A process in the
/// mode ([`Shared::percpu_on`], the [`PUSH_PERCPU`] bit) keys its plain
/// `malloc`/`free` caches by the running thread's rseq `mm_cid` instead of
/// by thread: the table of [`CpuCache`]s in [`Percpu`], one header per
/// (concurrency id, class), every header operation a critical section of
/// `rseq.rs` that reads the id, so no lock and no atomic read-modify-write
/// is added to the fast paths and two threads never mutate one header at
/// the same time (an id is held by one running thread; a thread preempted
/// inside a section is moved to the abort handler and retries). The thread
/// heap stays: it is the init witness the fast paths already load, it
/// keeps the thread's byte counters (every per-CPU pop and push is counted
/// there, `thread.allocated` stays exact), its home caches serve explicit
/// `MALLOCX_ARENA` traffic and huge runs as before, and a heap off the mode
/// ([`Heap::percpu`] false) takes the thread paths.
///
/// Arenas are per concurrency id: the header of `(id, class)` is bound to
/// arena `id % narenas` ([`Percpu::arena_of`]) at its first use and its
/// refills claim there, so the slabs a CPU touches are its own arena's;
/// a slot freed on another CPU is pushed onto that CPU's cache whatever
/// its arena (the flush routes it back through the owner array,
/// [`Shared::release_routed`], as the sized path's cache already does) and
/// the `free` fast path skips the owner word. What a slow path touches
/// besides the header goes through a thread-private buffer: a refill
/// claims its batch onto the stack and pushes it in one section
/// ([`rseq::push_batch`]); a flush takes the whole stack out
/// ([`rseq::take_all`]), selects the highest-address half as the thread
/// flush does, releases it and pushes the kept half back; if the thread
/// migrated in between, the batch simply lands on the new CPU's cache and
/// what does not fit is released to its slabs. Capacities follow the
/// thread ladder, clamped to [`MAX_CAP`] (the buffers are stack arrays of
/// that size). Compaction (design 6.4) keeps its gate per header (probe
/// every 4096 pops, compact while the class is fragmented and refills are
/// not draining it) but not its run: a scheduled pop in compact mode
/// claims one slot straight from the arena's lowest partial slab
/// ([`Heap::percpu_scheduled`]), which lands the allocation where the run
/// would have, without a two-word update no single commit store can make.
/// The cache GC and `thread.tcache.flush` act on the caches of the CPU the
/// thread is running on; an idle CPU's caches are drained the next time a
/// thread runs there and ticks, which is the residue the mode bounds by
/// cores. Thread exit leaves the CPU caches alone (they are not the
/// thread's), and fork needs nothing (no lock, and every section's
/// pre-commit stores leave a consistent header).
#[cfg(target_os = "linux")]
impl Heap {
    /// The per-CPU cache capacity for `class`: the thread ladder
    /// (`Config::cache_cap`) clamped to [`MAX_CAP`]; 0 means uncached.
    #[inline]
    fn percpu_cap(shared: &Shared, class: usize) -> u32 {
        shared.config.cache_cap(class).min(MAX_CAP)
    }

    /// The pop of a heap in a per-CPU process: the [`PUSH_PERCPU`] branch
    /// of [`Heap::alloc_fast`] and [`Heap::alloc_zeroed`], out of line so
    /// the thread pop keeps its shape. A heap off the mode pops its own
    /// cache here. `None` sends the caller to the slow path.
    #[inline(never)]
    pub(crate) fn pop_percpu_mode(&mut self, class: usize, flags: u8) -> Option<(*mut u8, bool)> {
        let pac = flags & PUSH_PAC != 0;
        if !self.percpu {
            let cache = self.cache_ptr(class);
            // SAFETY: this thread's own header, unused or with a stack.
            unsafe {
                let len = (*cache).len;
                if len == 0 {
                    return None;
                }
                return Some(self.pop(cache, class, len, pac));
            }
        }
        // SAFETY: a heap never outlives its `Shared`.
        let shared: &Shared = unsafe { &*self.shared };
        // SAFETY: the owning thread registered this area; the table is live.
        match unsafe { rseq::pop(self.rseq_area, shared.percpu.table(class), pac) } {
            rseq::Pop::Popped(p, zero) => {
                self.count_direct_alloc(class);
                Some((p, zero))
            }
            rseq::Pop::Empty => None,
            rseq::Pop::Scheduled => self.percpu_scheduled(shared, class),
            rseq::Pop::OutOfRange => cid_out_of_range(),
        }
    }

    /// A pop with the schedule already consumed: `Some` or empty.
    #[inline(never)]
    fn percpu_pop_plain(&mut self, shared: &Shared, class: usize) -> Option<(*mut u8, bool)> {
        // SAFETY: as in `pop_percpu_mode`.
        match unsafe { rseq::pop(self.rseq_area, shared.percpu.table(class), shared.pac()) } {
            rseq::Pop::Popped(p, zero) => {
                self.count_direct_alloc(class);
                Some((p, zero))
            }
            rseq::Pop::Empty | rseq::Pop::Scheduled => None,
            rseq::Pop::OutOfRange => cid_out_of_range(),
        }
    }

    /// The scheduled pop of a per-CPU cache (design 6.4 without the run):
    /// in probe mode run the periodic work and ask the class whether it is
    /// fragmented and left alone by refills, entering compact mode if so;
    /// in compact mode claim one slot from the arena's lowest partial slab
    /// (re-checking the class every [`PROBE_MASK`] + 1 scheduled pops and
    /// dropping back to probing once it is dense); otherwise pop the top.
    /// The header's mode fields are hints written with volatile stores
    /// (another thread may own the id by the time they land; a stale mode
    /// changes only which pops are scheduled, never a stack).
    #[cold]
    #[inline(never)]
    fn percpu_scheduled(&mut self, shared: &Shared, class: usize) -> Option<(*mut u8, bool)> {
        // SAFETY: this thread's area.
        let cid = unsafe { rseq::mm_cid(self.rseq_area) };
        let Some(h) = shared.percpu.header(cid, class) else { cid_out_of_range() };
        let h = h as *mut CacheHeader;
        // SAFETY: a live header of the table; racy volatile reads.
        let (mode, arena) = unsafe { (ptr::read_volatile(&raw const (*h).mode), ptr::read_volatile(&raw const (*h).arena)) };
        let cs = shared.owner(arena).class(class);
        match mode {
            MODE_COMPACT => {
                // SAFETY: as above; `run_end` is unused by per-CPU caches
                // and counts scheduled pops here.
                let k = unsafe { ptr::read_volatile(&raw const (*h).run_end) }.wrapping_add(1);
                unsafe { ptr::write_volatile(&raw mut (*h).run_end, k) };
                if k & PROBE_MASK == 0 {
                    self.tick(shared);
                    if !Self::fragmented(shared, arena, class) {
                        cs.note_compact_mode(false);
                        // SAFETY: as above.
                        unsafe {
                            ptr::write_volatile(&raw mut (*h).mode, MODE_PROBE);
                            ptr::write_volatile(&raw mut (*h).compact_mask, PROBE_MASK);
                        }
                        return self.percpu_pop_plain(shared, class);
                    }
                }
            }
            MODE_PROBE => {
                self.tick(shared);
                let refilled = cs.refilled() as u32;
                // SAFETY: as above.
                let prev = unsafe { ptr::read_volatile(&raw const (*h).probe_refilled) };
                unsafe { ptr::write_volatile(&raw mut (*h).probe_refilled, refilled) };
                let delta = refilled.wrapping_sub(prev) as usize;
                if delta >= COMPACT_MAX_REFILL_SLABS * CLASSES[class].compact_slots() {
                    if Self::fragmented(shared, arena, class) {
                        cs.note_gate_refill();
                    }
                    return self.percpu_pop_plain(shared, class);
                }
                if !Self::fragmented(shared, arena, class) {
                    return self.percpu_pop_plain(shared, class);
                }
                cs.note_compact_mode(true);
                // SAFETY: as above.
                unsafe {
                    ptr::write_volatile(&raw mut (*h).mode, MODE_COMPACT);
                    ptr::write_volatile(&raw mut (*h).compact_mask, shared.config.compact_mask());
                    ptr::write_volatile(&raw mut (*h).run_end, 0);
                }
            }
            _ => return self.percpu_pop_plain(shared, class),
        }
        let mut out = [ptr::null_mut()];
        let claim = self.claim(shared, cs, &mut out);
        if claim.n == 0 {
            return self.percpu_pop_plain(shared, class);
        }
        cs.note_compact(1);
        self.count_direct_alloc(class);
        Some((out[0], claim.zeroed))
    }

    /// One slot of `class` straight from the home arena's bitmaps, counted
    /// (the per-CPU paths' fallback for an uncached class or an exhausted
    /// metadata region).
    fn percpu_alloc_direct(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        let Some(a) = shared.arena(self.arena) else { return (ptr::null_mut(), false) };
        let (p, zero) = shared.alloc_direct(a, class);
        if !p.is_null() {
            self.count_direct_alloc(class);
        }
        (p, zero)
    }

    /// Refill the running CPU's cache for `class` and pop (the per-CPU
    /// [`Heap::alloc_slow`]): claim up to `cap / 2` slots from the id's
    /// arena into a stack buffer, hand the lowest to the caller and push
    /// the rest in one section, lowest on top; what the section could not
    /// take (the thread moved to a CPU whose cache is full) goes back to
    /// its slabs. Retries from the top after a scheduled pop that served
    /// nothing.
    #[cold]
    #[inline(never)]
    fn percpu_alloc_slow(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        self.tick(shared);
        let cap = Self::percpu_cap(shared, class);
        if cap == 0 {
            return self.percpu_alloc_direct(shared, class);
        }
        let pac = shared.pac();
        let area = self.rseq_area;
        let t = shared.percpu.table(class);
        loop {
            // SAFETY: this thread's area; the table is live.
            match unsafe { rseq::pop(area, t, pac) } {
                rseq::Pop::Popped(p, zero) => {
                    self.count_direct_alloc(class);
                    return (p, zero);
                }
                rseq::Pop::Scheduled => {
                    if let Some(r) = self.percpu_scheduled(shared, class) {
                        return r;
                    }
                    continue;
                }
                rseq::Pop::OutOfRange => cid_out_of_range(),
                rseq::Pop::Empty => {}
            }
            // SAFETY: this thread's area.
            let cid = unsafe { rseq::mm_cid(area) };
            if !self.percpu_ensure(shared, class, cap, cid) {
                return self.percpu_alloc_direct(shared, class);
            }
            let arena = Percpu::arena_of(shared, cid);
            let Some(a) = shared.arena(arena) else { return (ptr::null_mut(), false) };
            let want = ((cap / 2).max(1) as usize).min(MAX_CAP as usize / 2);
            let mut buf = [ptr::null_mut::<u8>(); MAX_CAP as usize / 2];
            let out = &mut buf[..want];
            let claim = self.claim(shared, a.class(class), out);
            a.class(class).note_refill(claim.n);
            if claim.n == 0 {
                return (ptr::null_mut(), false);
            }
            let p = out[0];
            let rest = &mut out[1..claim.n];
            rest.reverse();
            // SAFETY: this thread's area; `rest` are plain slot pointers.
            let pushed = match unsafe { rseq::push_batch(area, t, rest, claim.zeroed, pac) } {
                Ok(k) => k,
                Err(()) => cid_out_of_range(),
            };
            if pushed < rest.len() {
                // A descending batch keeps each slab's slots contiguous,
                // which is all `release_owned` needs.
                shared.release_routed(arena, class, &rest[pushed..]);
            }
            self.count_direct_alloc(class);
            return (p, claim.zeroed);
        }
    }

    /// Give the header `(cid, class)` its stack if it has none: the
    /// idempotent fields with plain stores (every installer writes the
    /// same values, so a racing installer's stores are harmless), then
    /// `slots` and `cap` in one section ([`rseq::install`]), which refuses
    /// to publish a header this thread did not prepare (it migrated:
    /// re-read the id and try again). `false` when no stack could be
    /// allocated (the caller serves the request directly).
    #[inline(never)]
    fn percpu_ensure(&mut self, shared: &Shared, class: usize, cap: u32, cid: u32) -> bool {
        let mut cid = cid;
        loop {
            let Some(h) = shared.percpu.header(cid, class) else { cid_out_of_range() };
            let h = h as *mut CacheHeader;
            // SAFETY: a live header; racy read of `cap`.
            if unsafe { ptr::read_volatile(&raw const (*h).cap) } != 0 {
                return true;
            }
            let slots = shared.meta.alloc(CacheHeader::slots_layout(cap));
            if slots.is_null() {
                return false;
            }
            let arena = Percpu::arena_of(shared, cid);
            let mode = if shared.config.tcache_compact_shift == 0 { MODE_OFF } else { MODE_PROBE };
            let mask = if mode == MODE_PROBE { PROBE_MASK } else { u32::MAX };
            // SAFETY: a live header not yet published (`cap == 0`); these
            // stores are identical from every installer of this header.
            unsafe {
                ptr::write_volatile(&raw mut (*h).arena, arena);
                ptr::write_volatile(&raw mut (*h).mode, mode);
                ptr::write_volatile(&raw mut (*h).compact_mask, mask);
                ptr::write_volatile(&raw mut (*h).probe_refilled, shared.owner(arena).class(class).refilled() as u32);
            }
            // SAFETY: this thread's area; `slots` holds `cap` pointers.
            match unsafe { rseq::install(self.rseq_area, shared.percpu.table(class), h as *const u8, slots as *mut *mut u8, cap) } {
                rseq::Install::Installed => return true,
                rseq::Install::Exists => {
                    shared.meta.free(slots, CacheHeader::slots_layout(cap));
                    return true;
                }
                rseq::Install::Moved => {
                    shared.meta.free(slots, CacheHeader::slots_layout(cap));
                    // SAFETY: this thread's area.
                    cid = unsafe { rseq::mm_cid(self.rseq_area) };
                }
                rseq::Install::OutOfRange => cid_out_of_range(),
            }
        }
    }

    /// Push a verified slot onto the running CPU's cache, flushing half of
    /// it first when it is full (the per-CPU [`Heap::free`] for a full or
    /// uninstalled cache and for `zero_on_free`, which clears here).
    #[cold]
    #[inline(never)]
    fn percpu_free_slow(&mut self, shared: &Shared, class: usize, p: *mut u8) {
        if shared.zero_on_free() {
            // SAFETY: `p` is a slot start of `class` (the caller's check).
            unsafe { zero_slot(class, p) };
        }
        self.tick(shared);
        let cap = Self::percpu_cap(shared, class);
        if cap == 0 {
            shared.free_direct(shared.arena_of_slot(class, p), class, p);
            self.count_direct_free(class);
            return;
        }
        let pac = shared.pac();
        let area = self.rseq_area;
        let t = shared.percpu.table(class);
        loop {
            // SAFETY: this thread's area; `p` is a verified slot of `class`.
            match unsafe { rseq::push(area, t, p, pac) } {
                rseq::Push::Pushed => {
                    self.count_direct_free(class);
                    return;
                }
                rseq::Push::OutOfRange => cid_out_of_range(),
                rseq::Push::Full => {}
            }
            // SAFETY: this thread's area.
            let cid = unsafe { rseq::mm_cid(area) };
            if !self.percpu_ensure(shared, class, cap, cid) {
                shared.free_direct(shared.arena_of_slot(class, p), class, p);
                self.count_direct_free(class);
                return;
            }
            self.percpu_flush(shared, class, cap, |len| len / 2);
        }
    }

    /// Flush the running CPU's cache for `class`: take every entry out in
    /// one section (into an 8 KiB stack buffer that lives in this frame
    /// alone, `inline(never)`: inlined into `gc` and on into `tick` it
    /// deepened every slow path's frame and larson's thread stacks by
    /// pages), release the `n(len)` highest addresses to their slabs
    /// (grouped by slab, the straddling double-free compare of
    /// [`Heap::flush`] included) and push the rest back in one section,
    /// lowest on top when the stack was in address order. Entries the
    /// push-back cannot place (the thread migrated to a CPU whose cache is
    /// full) are released too.
    #[inline(never)]
    fn percpu_flush(&mut self, shared: &Shared, class: usize, cap: u32, n: impl Fn(usize) -> usize) {
        let pac = shared.pac();
        let area = self.rseq_area;
        let t = shared.percpu.table(class);
        let mut buf = [ptr::null_mut::<u8>(); MAX_CAP as usize];
        let out = &mut buf[..(cap as usize).min(MAX_CAP as usize)];
        // SAFETY: this thread's area; `out` holds the cache's capacity.
        let len = match unsafe { rseq::take_all(area, t, out, pac) } {
            Ok(len) => len,
            Err(()) => cid_out_of_range(),
        };
        if len == 0 {
            return;
        }
        let all = &mut out[..len];
        let n = n(len).min(len);
        let rest_len = len - n;
        // SAFETY: this thread's area.
        let arena = Percpu::arena_of(shared, unsafe { rseq::mm_cid(area) });
        if n > 0 {
            let sorted = rest_len > 0 && all.is_sorted();
            if !sorted && rest_len > 0 {
                all.select_nth_unstable(rest_len - 1);
            }
            let (kept, high) = all.split_at_mut(rest_len);
            let low = if sorted { high[0] } else { group_by_slab(shared, class, high) };
            if rest_len > 0 && low == kept[rest_len - 1] {
                straddling_double_free(low, class);
            }
            shared.release_routed(arena, class, high);
            if sorted {
                kept.reverse();
            }
        }
        if rest_len == 0 {
            return;
        }
        let kept = &mut all[..rest_len];
        // SAFETY: this thread's area; plain slot pointers.
        let pushed = match unsafe { rseq::push_batch(area, t, kept, false, pac) } {
            Ok(k) => k,
            Err(()) => cid_out_of_range(),
        };
        if pushed < kept.len() {
            let rem = &mut kept[pushed..];
            group_by_slab(shared, class, rem);
            shared.release_routed(arena, class, rem);
        }
    }

    /// Return every entry of the running CPU's caches to the slabs
    /// (`thread.tcache.flush`, `malloc_trim`, the purges' caller flush).
    #[inline(never)]
    fn percpu_drain(&mut self, shared: &Shared) {
        for class in 0..NCLASSES {
            let cap = Self::percpu_cap(shared, class);
            if cap == 0 || !self.percpu_current_has(shared, class) {
                continue;
            }
            self.percpu_flush(shared, class, cap, |len| len);
        }
    }

    /// The running CPU's caches' GC (design 6.4): release three quarters of
    /// each class's `low_water`, the entries unused since the last GC; the
    /// push-back restarts `low_water` at the kept length.
    #[inline(never)]
    fn percpu_gc(&mut self, shared: &Shared) {
        // SAFETY: this thread's area.
        let cid = unsafe { rseq::mm_cid(self.rseq_area) };
        for class in 0..NCLASSES {
            let cap = Self::percpu_cap(shared, class);
            let Some(h) = shared.percpu.header(cid, class) else { cid_out_of_range() };
            // SAFETY: a live header; racy hints.
            let (hcap, lw) = unsafe { (ptr::read_volatile(&raw const (*h).cap), ptr::read_volatile(&raw const (*h).low_water)) };
            if cap == 0 || hcap == 0 || lw == 0 {
                continue;
            }
            let lw = lw as usize;
            self.percpu_flush(shared, class, cap, move |len| (lw - lw / 4).min(len));
        }
    }

    /// Whether the running CPU's header for `class` is installed and holds
    /// anything (a racy hint that saves a drain the empty section).
    fn percpu_current_has(&self, shared: &Shared, class: usize) -> bool {
        // SAFETY: this thread's area.
        let cid = unsafe { rseq::mm_cid(self.rseq_area) };
        let Some(h) = shared.percpu.header(cid, class) else { cid_out_of_range() };
        // SAFETY: a live header; racy reads.
        unsafe { ptr::read_volatile(&raw const (*h).cap) != 0 && ptr::read_volatile(&raw const (*h).len) != 0 }
    }
}

/// Off Linux the mode never turns on; the entry points still compile.
#[cfg(not(target_os = "linux"))]
impl Heap {
    pub(crate) fn pop_percpu_mode(&mut self, _class: usize, _flags: u8) -> Option<(*mut u8, bool)> {
        None
    }

    fn percpu_alloc_slow(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        let arena = self.arena;
        self.alloc_slow_in(shared, arena, class)
    }

    fn percpu_free_slow(&mut self, shared: &Shared, class: usize, p: *mut u8) {
        let arena = self.arena;
        self.free_slow_in(shared, arena, class, p)
    }

    fn percpu_drain(&mut self, _shared: &Shared) {}

    fn percpu_gc(&mut self, _shared: &Shared) {}
}

impl Percpu {
    /// The arena bound to concurrency id `cid`: `cid % narenas` over the
    /// automatic arenas, so with the default `narenas` (`4 * ncpus`, at
    /// most 64) every id has its own arena up to 64 CPUs.
    #[inline]
    pub fn arena_of(shared: &Shared, cid: u32) -> u16 {
        (cid % shared.arenas.auto().max(1)) as u16
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
    let (t, tp) = crate::tls::get_tp();
    let t = t as usize;
    if t > TLS_EXITED {
        // Anything above the sentinels is this thread's heap pointer,
        // signed with the thread pointer when `PUSH_PAC` is set (the
        // sentinels are never signed, so the compare comes first; a signed
        // pointer keeps its low 48 bits and stays above them).
        // SAFETY: `t` is this thread's slot value above the sentinels.
        return Some(unsafe { heap_from_slot(t, tp, shared.pac()) });
    }
    create_heap_for_thread(shared, t)
}

/// The slot value for heap `h` on the calling thread: `h` signed with the
/// thread pointer when `shared.pac()`, else `h` itself.
#[cfg(target_os = "linux")]
fn tls_value_for(shared: &Shared, h: *mut Heap) -> *mut c_void {
    if shared.pac() {
        let (_, tp) = crate::tls::get_tp();
        pac::sign(h as usize, tp) as *mut c_void
    } else {
        h as *mut c_void
    }
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
    crate::tls::set(tls_value_for(shared, h));
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
///
/// `shared` is the process's `Shared` as a raw pointer (`api::shared_ptr`,
/// or a `&Shared`), read only after the slot proved a heap exists: its
/// `push_flags` byte says whether the slot value is signed
/// ([`PUSH_PAC`]), and a thread that created a heap did so under
/// `shared()`, after init wrote that byte.
#[cfg(target_os = "linux")]
#[inline(always)]
pub fn existing_heap(shared: *const Shared) -> Option<&'static mut Heap> {
    let (t, tp) = tls_heap_slot()?;
    // SAFETY: a real slot value proves this thread observed `READY`, so
    // `push_flags` was written before.
    let flags = unsafe { fast_flags(shared) };
    // SAFETY: `t` is this thread's slot value above the sentinels.
    Some(unsafe { heap_from_slot(t, tp, flags & PUSH_PAC != 0) })
}

/// The calling thread's TLS slot value and thread pointer when the slot
/// holds a heap pointer (plain or signed), `None` on null or a sentinel.
/// (Branching once here into two copies of the whole fast path was tried:
/// LLVM merged the copies' tails back together and gave `malloc` a stack
/// frame it did not have, so the entry points take [`existing_heap`] and
/// the `autda`s sit behind their own predicted `tbz`s.)
#[cfg(target_os = "linux")]
#[inline(always)]
pub fn tls_heap_slot() -> Option<(usize, usize)> {
    let (t, tp) = crate::tls::get_tp();
    let t = t as usize;
    if t > TLS_EXITED { Some((t, tp)) } else { None }
}

/// `Shared::push_flags` through a raw pointer (`api::shared_ptr`).
///
/// # Safety
/// The caller must have observed `READY` (a heap pointer in its TLS slot
/// is the witness: heaps are created under `shared()`, after init wrote
/// the byte).
#[inline(always)]
pub unsafe fn fast_flags(shared: *const Shared) -> u8 {
    // SAFETY: guaranteed by the caller.
    unsafe { ptr::read(&raw const (*shared).push_flags) }
}

/// The heap a TLS slot value names: authenticated against the thread
/// pointer when `pac` ([`PUSH_PAC`]), the value itself otherwise.
///
/// # Safety
/// `(t, tp)` from [`tls_heap_slot`] on this thread, and `pac` is the
/// process's `PUSH_PAC`.
#[inline(always)]
pub unsafe fn heap_from_slot(t: usize, tp: usize, pac: bool) -> &'static mut Heap {
    let h = if pac { pac::auth(t, tp) } else { t };
    // SAFETY: guaranteed by the caller; the hint spares the null test the
    // compiler would otherwise add for the `Option` niche, `t` being above
    // the sentinels and `autda` never producing zero from it.
    unsafe {
        core::hint::assert_unchecked(h != 0);
        &mut *(h as *mut Heap)
    }
}

/// The `free` fast path on an existing heap: check the slot and push it
/// onto the home cache. `false` whenever anything is off the fast path (not
/// a verified slab slot, another arena's slab, full cache); the caller then
/// goes through a cold [`free`], which repeats the checks and aborts when
/// they fail. Contains no call, so the entry point that inlines it needs no
/// frame.
#[inline(always)]
pub fn free_fast(shared: &Shared, h: &mut Heap, p: *mut u8, flags: u8) -> bool {
    match shared.checked_slot(p, h) {
        Some(class) => h.push_fast(class, p, flags),
        None => false,
    }
}

/// The `free` and sized-free fast paths of a process in the per-CPU mode
/// (design 15). The entry points (`api::libc`) test [`PUSH_PERCPU`] on the
/// flag byte they load anyway and tail-call their per-CPU wrapper, which
/// calls this, so the thread paths keep their shape (no frame, as the
/// disassembly of `malloc`/`free` in `bench/results/2026-09-19-*-task8-*`
/// records; a call inside the inlined fast path gave both a frame). A heap
/// off the mode ([`Heap::percpu`] false) takes the thread path here;
/// otherwise the slot is verified (the bit test without the arena test,
/// [`Shared::checked_slot_any`], or the sized path's arithmetic with the
/// hint), pushed onto the running CPU's cache in one critical section and
/// counted in the thread's byte counters. `false` for anything off the
/// fast path (a full or uninstalled cache, a pointer the checks decline,
/// `zero_on_free`, which clears on the slow path before the push).
#[cfg(target_os = "linux")]
#[inline(never)]
pub fn percpu_free_fast(shared: &Shared, h: &mut Heap, p: *mut u8, flags: u8, hint: Option<usize>, sized: bool) -> bool {
    if !h.percpu {
        let class = if sized { shared.sized_slot(p, hint) } else { shared.checked_slot(p, h) };
        return match class {
            Some(class) => h.push_fast(class, p, flags),
            None => false,
        };
    }
    let class = if sized { shared.sized_slot(p, hint) } else { shared.checked_slot_any(p) };
    let Some(class) = class else { return false };
    if flags & PUSH_ZERO != 0 {
        return false;
    }
    // SAFETY: the heap's thread registered this area; the table is live;
    // `p` is a verified slot start of `class`.
    match unsafe { rseq::push(h.rseq_area, shared.percpu.table(class), p, flags & PUSH_PAC != 0) } {
        rseq::Push::Pushed => {
            h.count_direct_free(class);
            true
        }
        rseq::Push::Full => false,
        rseq::Push::OutOfRange => cid_out_of_range(),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn percpu_free_fast(_: &Shared, _: &mut Heap, _: *mut u8, _: u8, _: Option<usize>, _: bool) -> bool {
    false
}

/// The sized free's fast path on an existing heap (`sdallocx`,
/// `free_sized`): verify the size hint's class against the pointer
/// arithmetically and push onto the home cache without loading the slab
/// descriptor (design 6.3). `false` for anything off the fast path (not a
/// slot of a committed slab, a hint naming another class or a huge size, a
/// full cache); the caller then goes through [`Shared::check_size_hint`]
/// and the exact [`free`]. Contains no call.
#[inline(always)]
pub fn free_sized_fast(shared: &Shared, h: &mut Heap, p: *mut u8, hint: Option<usize>, flags: u8) -> bool {
    match shared.sized_slot(p, hint) {
        Some(class) => h.push_fast(class, p, flags),
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
            if shared.zero_on_free() {
                // SAFETY: `classify_free` verified `p` as an allocated slot
                // of `class`.
                unsafe { zero_slot(class, p) };
            }
            shared.free_direct(arena, class, p);
            if let Some(h) = existing_heap(shared) {
                h.count_direct_free(class);
            }
        }
        Err(page) => {
            if shared.zero_on_free() {
                shared.zero_huge(page);
            }
            let n = shared.free_huge(page);
            if let Some(h) = existing_heap(shared) {
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

/// `zero_on_free`: clear the class-size bytes of slot `p` (plan Phase 2
/// task 3). Called from the slow paths only (`Heap::free_slow_in`,
/// `free_direct`), so the default `free` carries nothing of it but the
/// flag test; a `memset` of the class size, so a 1 KiB value costs a 1 KiB
/// store (and the slow-path call) per free, which is why the key is off by
/// default (review 2026-09-18 section 5.1; priced in
/// `bench/results/2026-09-18-*-gce-tunables.md`).
///
/// # Safety
/// `p` is a slot start of `class` in a committed slab.
#[cold]
#[inline(never)]
unsafe fn zero_slot(class: usize, p: *mut u8) {
    // SAFETY: guaranteed by the caller; the slot is `size` writable bytes.
    unsafe { ptr::write_bytes(p, 0, CLASSES[class].size()) };
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
/// Buckets of [`group_by_slab`]'s counting pass: a batch whose slab
/// numbers span fewer than this many slabs is grouped in one in-place
/// pass; a batch spread wider is sorted instead.
const GROUP_BUCKETS: usize = 256;

/// Group a flush's released batch by slab, in place, and return its lowest
/// address. `release_owned` needs a slab's slots contiguous (one owner
/// check, one descriptor update and one bitmap pass per slab) and nothing
/// else, so the pass is a counting sort on the slab number, `(p - base) >>
/// lg_slab` (design 3's geometry): one sweep finds the lowest and highest
/// slab number (and the batch's minimum address); a batch from one slab is
/// done; one spanning fewer than [`GROUP_BUCKETS`] slabs is permuted by
/// [`flag_sort`] on `slab - lowest`; anything wider, a cache holding slots
/// of more than 256 slabs' span, falls back to `sort_unstable` on the
/// address. Every path is O(n) but the last, which is the old sort on
/// half the entries.
fn group_by_slab(shared: &Shared, class: usize, high: &mut [*mut u8]) -> *mut u8 {
    let base = shared.regions.class_base[class];
    let lg = CLASSES[class].lg_slab as u32;
    let key = |p: *mut u8| (p as usize).wrapping_sub(base) >> lg;
    let mut low = high[0];
    let (mut kmin, mut kmax) = (usize::MAX, 0usize);
    for &p in high.iter() {
        let k = key(p);
        kmin = kmin.min(k);
        kmax = kmax.max(k);
        if p < low {
            low = p;
        }
    }
    if kmin == kmax {
        return low;
    }
    let span = kmax - kmin + 1;
    if span <= GROUP_BUCKETS {
        flag_sort(high, span, |p| key(p) - kmin);
    } else {
        high.sort_unstable();
    }
    low
}

/// American flag sort: permute `v` in place so that entries with equal
/// `key` (below `nb`, at most [`GROUP_BUCKETS`]) are contiguous, buckets
/// in ascending key order. One counting sweep, a prefix sum, and one
/// cycle-following sweep that moves every entry once: O(len + nb), no
/// scratch beyond the bucket cursors.
fn flag_sort(v: &mut [*mut u8], nb: usize, key: impl Fn(*mut u8) -> usize) {
    debug_assert!(nb <= GROUP_BUCKETS && v.len() <= u16::MAX as usize);
    let mut next = [0u16; GROUP_BUCKETS];
    let mut end = [0u16; GROUP_BUCKETS];
    for &p in v.iter() {
        end[key(p)] += 1;
    }
    let mut acc = 0u16;
    for b in 0..nb {
        next[b] = acc;
        acc += end[b];
        end[b] = acc;
    }
    for b in 0..nb {
        while next[b] < end[b] {
            let i = next[b] as usize;
            let mut p = v[i];
            let mut kb = key(p);
            while kb != b {
                let j = next[kb] as usize;
                next[kb] += 1;
                core::mem::swap(&mut p, &mut v[j]);
                kb = key(p);
            }
            v[i] = p;
            next[b] += 1;
        }
    }
}

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

/// The kernel wrote an `mm_cid` at or above the possible-CPU count the
/// table was sized for (`rseq::possible_cpus`): the sizing rule is wrong
/// for this kernel, and the heap must not continue on a guess.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[cold]
#[inline(never)]
fn cid_out_of_range() -> ! {
    panic!("rseq mm_cid at or above the possible-CPU count: the per-CPU table is too small")
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

    /// The page size the synthetic layout is built with: the kernel's on
    /// Linux (the reservations are real), 4 KiB on the host.
    #[cfg(target_os = "linux")]
    pub(crate) fn page_size() -> usize {
        crate::sys::features::page_size()
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn page_size() -> usize {
        4096
    }

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
            page_size().trailing_zeros(),
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
    /// `pac_pointers` on: the signed path runs in every heap test on a
    /// host with pointer authentication (plan Phase 2.5 task 7), and is
    /// the plain path elsewhere.
    pub(crate) fn shared() -> &'static Shared {
        shared_with(Config { narenas: 1, pac_pointers: true, ..Config::DEFAULT })
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
        shared_with_guards(Config { narenas: 1, pac_pointers: true, ..Config::DEFAULT }, true)
    }

    fn shared_with_guards(config: Config, guard_install: bool) -> &'static Shared {
        shared_with_guards_mode(config, guard_install, false)
    }

    /// A `Shared` with the per-CPU mode allowed (on where the host's rseq
    /// has `mm_cid`; the tests check `percpu.on()` and skip otherwise).
    #[cfg(target_os = "linux")]
    pub(crate) fn shared_percpu() -> &'static Shared {
        shared_with_guards_mode(
            Config { narenas: 4, percpu_arena: crate::config::PercpuArena::Percpu, ..Config::DEFAULT },
            true,
            true,
        )
    }

    pub(crate) fn shared_with_guards_mode(config: Config, guard_install: bool, percpu: bool) -> &'static Shared {
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
            v.resize(META_LEN + page_size(), 0);
            let buf: &'static mut [u8] = Box::leak(v.into_boxed_slice());
            (0x1000_0000_0000usize, buf.as_ptr() as usize, 0x2000_0000_1000usize)
        };
        let regions = regions_at(
            align_up(sb, MAX_SLAB),
            (align_up(mb, page_size()), META_LEN),
            hb,
        );
        let b: &'static mut MaybeUninit<Shared> = Box::leak(Box::new(MaybeUninit::uninit()));
        assert!(unsafe { Shared::init(b.as_mut_ptr(), regions, config, guard_install, pac::available(), percpu) });
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

        pub(crate) fn alloc_zeroed(&self, c: usize) -> (*mut u8, bool) {
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

        pub(crate) fn stats(&self) -> ThreadStats {
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

        /// The entries of `c`'s cache as plain pointers (authenticated
        /// where the stack is signed), bottom first.
        fn cached_plain(&self, c: usize) -> Vec<*mut u8> {
            let k = self.cache(c);
            k.cached().iter().map(|&e| k.plain(e, self.s.pac())).collect()
        }

        /// The run of `c`'s cache as plain pointers.
        fn run_plain(&self, c: usize) -> Vec<*mut u8> {
            let k = self.cache(c);
            k.run().iter().map(|&e| k.plain(e, self.s.pac())).collect()
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

    pub(crate) fn xorshift(state: &mut u64) -> u64 {
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

    /// The dense [`GuardTest`] the sized fast path reads agrees with
    /// [`SlabTable::is_guard_slab`] for every class over the first slabs of
    /// the region and around its end, with guards on and off.
    #[cfg(target_os = "linux")]
    #[test]
    fn guard_test_agrees_with_the_tables() {
        for s in [shared_guarded(), shared()] {
            for c in 0..NCLASSES {
                let t = s.table(c);
                let g = s.guard_test[c];
                assert_eq!(g, GuardTest::of(t));
                let n = t.nslabs();
                for slab in (0..1024.min(n)).chain(n.saturating_sub(64)..n) {
                    assert_eq!(g.is_guard(slab), t.is_guard_slab(slab), "class {c} slab {slab}");
                }
            }
        }
    }

    /// Plan Phase 1.5 task 9b: the sized fast path's check rejects a slot
    /// start inside a guard slab (arithmetic on a dense per-class pair), so
    /// the exact slow path aborts instead of the guard memory being cached.
    /// Since Phase 2.5 task 4c it does not read the watermark: a slot start
    /// in a slab past it passes here and aborts at the flush (the next
    /// test).
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
        assert_eq!(s.sized_slot(s.regions.slab_addr(c, committed) as *mut u8, Some(c)), Some(c), "deferred to the flush");
        assert_eq!(s.sized_slot(all[0], Some(c + 1)), None, "wrong hint");
        assert_eq!(s.sized_slot(all[0], None), None, "huge hint");
        assert_eq!(s.sized_slot((all[0] as usize + 8) as *mut u8, Some(c)), None, "mid-slot");
        assert_eq!(s.sized_slot((s.regions.class_base[c] - 64) as *mut u8, Some(c)), None, "below the region");
        assert_eq!(s.sized_slot((s.regions.class_base[c] + s.regions.region_len()) as *mut u8, Some(c)), None, "past the region");
        for p in all {
            t.free(p);
        }
        t.destroy();
    }

    /// The watermark check the sized fast path gave up (plan Phase 2.5
    /// task 4c) is the flush's: a sized free of a slot start in a slab no
    /// arena has been handed is cached and aborts by name when the cache
    /// flushes it.
    #[cfg(target_os = "linux")]
    #[test]
    #[should_panic(expected = "was never allocated")]
    fn sized_free_past_the_watermark_aborts_at_the_flush() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let p = t.alloc(c);
        let committed = s.arena0.class(c).committed_slabs();
        let beyond = s.regions.slab_addr(c, committed + 1) as *mut u8;
        assert!(free_sized_fast(s, t.heap(), beyond, Some(c), s.push_flags()), "the fast path took it");
        t.free(p);
        t.flush_all();
    }

    #[test]
    fn flush_releases_the_highest_addresses_and_keeps_the_lowest() {
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
        // slabs, the HALF lowest stay (in whatever order the selection
        // left them; the run is dropped, design 6.4) under the new push.
        let extra = ps[order[CAP]];
        t.free(extra);
        assert_eq!(t.len(c), HALF + 1);
        assert_eq!(live(s, c), live_before - HALF);
        cached.sort_unstable();
        assert_eq!(t.alloc(c), extra);
        let mut kept: Vec<*mut u8> = (0..HALF).map(|_| t.alloc(c)).collect();
        kept.sort_unstable();
        assert_eq!(kept, cached[..HALF].to_vec(), "the kept half is the lowest addresses, content intact");
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

    /// Plan Phase 2.5 task 1b: the flush groups its released half by slab
    /// with a counting pass instead of sorting. Fake slot addresses (never
    /// dereferenced) from five slabs inside a 256-slab window, scrambled,
    /// come out slab-contiguous in ascending slab order as a permutation
    /// of the input, and the return value is the lowest address.
    #[test]
    fn group_by_slab_groups_a_scrambled_batch() {
        let s = shared();
        let c = class_with(64);
        let cls = &CLASSES[c];
        let slabs = [3usize, 5, 7, 9, 200];
        let mut v: Vec<*mut u8> = Vec::new();
        for (i, &sl) in slabs.iter().enumerate() {
            for k in 0..(5 + 7 * i) {
                v.push((s.regions.slab_addr(c, sl) + ((k * 37) % cls.nslots()) * cls.size()) as *mut u8);
            }
        }
        let mut st = 0x1234_5678_9abc_def1u64;
        for i in (1..v.len()).rev() {
            let j = (xorshift(&mut st) % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
        let mut expect = v.clone();
        expect.sort_unstable();
        let low = group_by_slab(s, c, &mut v);
        assert_eq!(low, expect[0]);
        let slab_of = |p: *mut u8| (p as usize - s.regions.class_base[c]) >> cls.lg_slab;
        let mut seen: Vec<usize> = Vec::new();
        for &p in &v {
            let sl = slab_of(p);
            if seen.last() != Some(&sl) {
                assert!(!seen.contains(&sl), "slab {sl} appears in two groups");
                seen.push(sl);
            }
        }
        assert_eq!(seen, slabs.to_vec(), "one contiguous group per slab, ascending");
        let mut got = v.clone();
        got.sort_unstable();
        assert_eq!(got, expect, "a permutation of the input");
    }

    /// A batch from one slab needs no grouping and is left as it is; a
    /// batch spanning more than 256 slabs takes the sort fallback and comes
    /// out fully sorted. Both return the lowest address.
    #[test]
    fn group_by_slab_single_slab_and_wide_span() {
        let s = shared();
        let c = class_with(64);
        let cls = &CLASSES[c];
        let at = |sl: usize, k: usize| (s.regions.slab_addr(c, sl) + k * cls.size()) as *mut u8;
        let mut one = std::vec![at(4, 9), at(4, 2), at(4, 700), at(4, 0), at(4, 33)];
        let before = one.clone();
        assert_eq!(group_by_slab(s, c, &mut one), at(4, 0));
        assert_eq!(one, before, "one slab: untouched");
        let mut wide = std::vec![at(1000, 3), at(0, 8), at(300, 1), at(0, 2), at(1000, 1), at(300, 5)];
        let mut expect = wide.clone();
        expect.sort_unstable();
        assert_eq!(group_by_slab(s, c, &mut wide), at(0, 2));
        assert_eq!(wide, expect, "wider than GROUP_BUCKETS slabs: sorted");
    }

    /// The in-place counting sort behind the bucket pass: 1000 entries
    /// with random 8-bit keys come out in non-decreasing key order as a
    /// permutation of the input.
    #[test]
    fn flag_sort_permutes_into_ascending_buckets() {
        let mut st = 0xdead_beef_cafe_f00du64;
        let mut v: Vec<*mut u8> = (0..1000).map(|_| (xorshift(&mut st) % 1_000_000) as *mut u8).collect();
        let mut expect = v.clone();
        let key = |p: *mut u8| (p as usize) & 0xff;
        flag_sort(&mut v, 256, key);
        assert!(v.windows(2).all(|w| key(w[0]) <= key(w[1])), "buckets ascend");
        v.sort_unstable();
        expect.sort_unstable();
        assert_eq!(v, expect);
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
            Some(k) => (t.cached_plain(c), t.run_plain(c), k.allocs(), k.compacting()),
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
                    assert!(t.run_plain(c).iter().all(|&q| q > p), "rebuild pop {} is not the block minimum", allocs + 1);
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
        let run = t.run_plain(c);
        assert!(run.windows(2).all(|w| w[0] > w[1]), "run not descending: {run:?}");
        assert!(run.len() <= k.low_water() && k.low_water() <= k.len());
        assert_eq!(run, &t.cached_plain(c)[..run.len()]);
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
                    let run = t.run_plain(c);
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
    /// Design 11: the periodic tick's GC interval is the configured one
    /// until the policy's immediate band, where it is 0; the effective
    /// decay period follows the band for arena 0 and the huge allocator;
    /// `-1` stays never. The host has no cgroup, so the policy is driven
    /// through `apply` here.
    #[test]
    fn cgroup_policy_drives_gc_interval_and_decay_period() {
        use crate::policy::{Band, KEEP_ALL, band, effective_decay_ns};
        let s = shared();
        // At init the outcome is the discovery sample's: no limit on the
        // host and under Docker's default profile, a band under
        // `--memory=512m`.
        let at_init = band(s.cgroup.limit().min(s.cgroup.high()), s.cgroup.current());
        assert_eq!(s.policy.band(), at_init);
        assert_eq!(s.gc_interval(), if s.policy.immediate() { 0 } else { s.gc_interval_ns });
        assert_eq!(s.policy.effective_decay_ns(s.decay_ns()), effective_decay_ns(s.decay_ns(), at_init));
        assert_eq!(crate::policy::empty_bytes(s), 0);
        let l = 512u64 << 20;
        s.policy.apply(band(l, l * 95 / 100), l, 0, 0);
        assert_eq!(s.gc_interval(), 0);
        assert_eq!(s.policy.effective_decay_ns(s.decay_ns()), 0);
        assert_eq!(s.policy.effective_decay_ns(crate::purge::NEVER), crate::purge::NEVER);
        s.policy.apply(band(l, l * 8 / 10), l, 0, 0);
        assert_eq!(s.gc_interval(), s.gc_interval_ns);
        let eff = s.policy.effective_decay_ns(s.decay_ns());
        assert!(eff < s.decay_ns() && eff > 1_000_000_000, "{eff}");
        // A decay pass with a pending full purge consumes it.
        s.policy.apply(band(l, l * 8 / 10), l, 0, 1);
        assert!(s.policy.full_purge_pending());
        let _ = crate::purge::decay(s, os::now_ns());
        assert!(!s.policy.full_purge_pending());
        assert_eq!(s.policy.full_purges(), 1);
        s.policy.apply(Band::Unlimited, crate::sys::cgroup::UNLIMITED, 0, 1);
        assert_eq!(s.policy.keep16(), KEEP_ALL);
        assert_eq!(s.gc_interval(), s.gc_interval_ns);
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
        assert!(free_fast(s, h, p0, s.push_flags()));
        assert!(!free_fast(s, h, p1, s.push_flags()));
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
        assert!(free_fast(s, h, p1, s.push_flags()));
        assert!(!free_fast(s, h, p0, s.push_flags()));
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


    // Phase 2 task 3: the opt-in `zero_on_free` and `slot_randomize` keys.

    /// The two push flags are one byte on `Shared`'s first line; the
    /// default configuration leaves it zero.
    #[test]
    fn push_flags_encode_zero_on_free_and_double_free_scan() {
        // `PUSH_PAC` follows the host (plan Phase 2.5 task 7); the two
        // opt-in bits are what this test is about.
        let pac = if pac::available() { PUSH_PAC } else { 0 };
        assert_eq!(shared().push_flags(), pac, "the test builder asks for pac_pointers");
        assert!(!shared().zero_on_free());
        let s = shared_with(Config { narenas: 1, zero_on_free: true, ..Config::DEFAULT });
        assert_eq!(s.push_flags(), PUSH_ZERO, "pac_pointers is off by default");
        assert!(s.zero_on_free() && !s.double_free_scan());
        let s = shared_with(Config { narenas: 1, zero_on_free: true, double_free_scan: true, pac_pointers: true, ..Config::DEFAULT });
        assert_eq!(s.push_flags(), PUSH_ZERO | PUSH_SCAN | pac);
        assert!(s.zero_on_free() && s.double_free_scan());
        assert_eq!(core::mem::offset_of!(Shared, push_flags) / 128, core::mem::offset_of!(Shared, multi_arena) / 128);
    }

    /// Slot memory is real on Linux only (`shared_with_guards`).
    #[cfg(target_os = "linux")]
    fn bytes_of(p: *mut u8, n: usize) -> Vec<u8> {
        (0..n).map(|i| unsafe { core::ptr::read_volatile(p.add(i)) }).collect()
    }

    /// With the key a freed slot reads as zero the moment `free` returns,
    /// through the plain path (cached), the sized path and the direct
    /// release; without it the slot keeps its bytes (the default path does
    /// not clear).
    #[cfg(target_os = "linux")]
    #[test]
    fn zero_on_free_clears_the_slot_on_the_plain_sized_and_direct_paths() {
        let c = class_with(64);
        let size = CLASSES[c].size();
        let dirty = |t: &TestHeap| {
            let p = t.alloc(c);
            unsafe { ptr::write_bytes(p, 0xA5, size) };
            p
        };
        // Default: the bytes survive the free (LIFO reuse hands them back).
        let s = shared();
        let t = TestHeap::new(s);
        let p = dirty(&t);
        t.free(p);
        assert!(bytes_of(p, size).iter().all(|&b| b == 0xA5), "the default path cleared a slot");
        assert_eq!(t.alloc(c), p);
        t.free(p);
        t.flush_all();
        t.destroy();

        let s = shared_with(Config { narenas: 1, zero_on_free: true, ..Config::DEFAULT });
        let t = TestHeap::new(s);
        // Plain free: cleared, then the same slot comes back zero without
        // `calloc`'s memset.
        let p = dirty(&t);
        t.free(p);
        assert!(bytes_of(p, size).iter().all(|&b| b == 0), "plain free left bytes");
        assert_eq!(t.alloc(c), p);
        assert!(bytes_of(p, size).iter().all(|&b| b == 0));
        // Sized path (`sdallocx`): the size hint's class, no bit test. With
        // the key the fast push declines and `free_sized_slow` takes the
        // exact route, which clears the slot in `free_slow_in`.
        unsafe { ptr::write_bytes(p, 0xA5, size) };
        assert!(!free_sized_fast(s, t.heap(), p, Some(c), s.push_flags()), "the fast push accepted a slot under zero_on_free");
        assert!(bytes_of(p, size).iter().all(|&b| b == 0xA5), "the declined push touched the slot");
        s.check_size_hint(p, size, Some(c));
        t.free(p);
        assert!(bytes_of(p, size).iter().all(|&b| b == 0), "sized free left bytes");
        assert_eq!(t.alloc(c), p);
        // A full cache: the slot goes through `free_slow` after the clear.
        let cap = t.cache(c).cap() as usize;
        let ps: Vec<*mut u8> = (0..cap).map(|_| dirty(&t)).collect();
        for &q in &ps {
            t.free(q);
        }
        for &q in &ps {
            assert!(bytes_of(q, size).iter().all(|&b| b == 0), "a flushed or cached slot kept bytes");
        }
        // Direct release (`MALLOCX_TCACHE_NONE`), with no cache involved.
        unsafe { ptr::write_bytes(p, 0xA5, size) };
        super::free_direct(s, p);
        assert!(bytes_of(p, size).iter().all(|&b| b == 0), "direct free left bytes");
        t.flush_all();
        t.destroy();
    }

    /// A huge run's usable bytes are cleared before the release, on the
    /// thread path and the direct path; the run is retained (`Empty`) so
    /// the memory is still there to read.
    #[cfg(target_os = "linux")]
    #[test]
    fn zero_on_free_clears_a_huge_run() {
        let s = shared_with(Config { narenas: 1, zero_on_free: true, ..Config::DEFAULT });
        let t = TestHeap::new(s);
        let n = 1 << 20;
        for direct in [false, true] {
            let (p, _) = t.alloc_huge(n, page_size());
            assert!(!p.is_null());
            let usable = s.huge.usable_for(n).unwrap();
            assert!(usable >= n);
            unsafe { ptr::write_bytes(p, 0xA5, usable) };
            if direct { super::free_direct(s, p) } else { t.free(p) }
            let b = bytes_of(p, usable);
            assert!(b.iter().all(|&x| x == 0), "huge free (direct {direct}) left bytes: first non-zero at {:?}", b.iter().position(|&x| x != 0));
        }
        // Default: untouched.
        let s = shared();
        let t2 = TestHeap::new(s);
        let (p, _) = t2.alloc_huge(n, page_size());
        unsafe { ptr::write_bytes(p, 0xA5, n) };
        t2.free(p);
        assert!(bytes_of(p, n).iter().all(|&x| x == 0xA5), "the default huge free cleared the run");
        t2.destroy();
        t.destroy();
    }

    /// Heap creation makes no `getrandom` call: the state stays 0 until
    /// the first randomised claim, and never under the default.
    #[test]
    fn slot_randomize_seeds_the_heap_lazily() {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        assert_eq!(t.heap().rng, 0);
        let p = t.alloc(c);
        assert_eq!(t.heap().rng, 0, "the default refill touched the rng");
        t.free(p);
        t.flush_all();
        t.destroy();
        let s = shared_with(Config { narenas: 1, slot_randomize: true, ..Config::DEFAULT });
        let t = TestHeap::new(s);
        assert_eq!(t.heap().rng, 0, "heap creation seeded the rng");
        let p = t.alloc(c);
        let seed = t.heap().rng;
        assert_ne!(seed, 0, "the first randomised refill did not seed");
        let q = t.alloc(c);
        assert_eq!(t.heap().rng, seed, "a pop advanced the rng");
        t.free(p);
        t.free(q);
        t.flush_all();
        t.destroy();
    }

    /// Under the key a refill's batch is the same slot set an ordered
    /// claim would take from the slab (`plan` and the prefix claim are
    /// untouched, so `Claim::zeroed` is too) in a non-address order; the
    /// second batch from the now `Active` slab (the bit loop from a random
    /// word) is the rest of the slab, shuffled as well. The default hands
    /// the slots out by ascending address.
    #[test]
    fn slot_randomize_shuffles_the_batch_and_keeps_the_slot_set() {
        let c = class_with(64);
        let size = CLASSES[c].size();
        let nslots = CLASSES[c].nslots();
        assert_eq!(nslots, 1024);
        let batch = HALF;
        let contiguous = |ps: &[*mut u8]| {
            let mut v: Vec<usize> = ps.iter().map(|&p| p as usize).collect();
            v.sort_unstable();
            v.windows(2).all(|w| w[1] - w[0] == size)
        };
        let ascending = |ps: &[*mut u8]| ps.windows(2).all(|w| w[0] < w[1]);

        let s = shared();
        let t = TestHeap::new(s);
        let first: Vec<*mut u8> = (0..batch).map(|_| t.alloc(c)).collect();
        assert!(ascending(&first), "the default order is not ascending");
        assert!(contiguous(&first));
        for &p in &first {
            t.free(p);
        }
        t.flush_all();
        t.destroy();

        let s = shared_with(Config { narenas: 1, slot_randomize: true, ..Config::DEFAULT });
        let t = TestHeap::new(s);
        let cs = &s.arena0.classes[c];
        let (first_z, zeroed): (Vec<*mut u8>, Vec<bool>) = (0..batch).map(|_| t.heap().alloc_zeroed(s, c)).unzip();
        assert!(zeroed.iter().all(|&z| z), "the first batch from a Fresh slab is zero whatever its order");
        assert!(!ascending(&first_z), "randomised batch is address-ordered");
        assert!(!first_z.windows(2).all(|w| w[0] > w[1]), "randomised batch is reverse-ordered");
        assert!(contiguous(&first_z), "the first batch is not the slab's first {batch} slots");
        let slab = cs.locate(first_z[0]).0;
        assert!(first_z.iter().all(|&p| cs.locate(p).0 == slab));
        let second: Vec<*mut u8> = (0..batch).map(|_| t.alloc(c)).collect();
        assert!(!ascending(&second) && contiguous(&second), "the second batch (bit loop) is ordered or not one run of slots");
        assert!(second.iter().all(|&p| cs.locate(p).0 == slab));
        let mut all: Vec<usize> = first_z.iter().chain(&second).map(|&p| p as usize).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), nslots, "the two batches are not the whole slab");
        assert_eq!(live(s, c), nslots);
        for p in first_z.into_iter().chain(second) {
            t.free(p);
        }
        t.flush_all();
        t.destroy();
        assert_eq!(live(s, c), 0);
    }

    /// The churn model with randomisation on: the cache still switches to
    /// compacting and rebuilds its run (the run is a shuffled block, so the
    /// scheduled pop serves a random slot of the lowest slab rather than
    /// its lowest, the documented trade), every slot handed out is a valid,
    /// distinct slot of the class and the live accounting holds.
    #[test]
    fn slot_randomize_keeps_the_compacting_run_valid() {
        let s = shared_with(Config { narenas: 1, slot_randomize: true, ..Config::DEFAULT });
        let t = TestHeap::new(s);
        let c = class_with(1024);
        let cs = &s.arena0.classes[c];
        let nslots = CLASSES[c].nslots();
        let mut keep = residue(&t, c, MODEL_SLOTS / nslots, 560, 0x9e37_79b9_7f4a_7c15);
        assert!(Heap::fragmented(s, 0, c));
        let mut held: HashSet<usize> = keep.iter().map(|&p| p as usize).collect();
        let mut st = 0x2468_ace0u64;
        let mut was_compacting = false;
        let mut rebuilt = false;
        for _ in 0..150_000 {
            let i = (xorshift(&mut st) % keep.len() as u64) as usize;
            assert!(held.remove(&(keep[i] as usize)));
            t.free(keep[i]);
            let run_before = t.heap().cache(c).map_or(0, |k| k.run().len());
            let p = t.alloc(c);
            let k = t.cache(c);
            was_compacting |= k.compacting();
            rebuilt |= k.compacting() && run_before == 0 && !k.run().is_empty();
            // A valid slot: the class's arithmetic locates it (aborts
            // otherwise) and nobody else holds it.
            let (slab, slot) = cs.locate(p);
            assert!(slab < cs.committed_slabs() && slot < nslots);
            assert!(held.insert(p as usize), "slot handed out twice");
            assert!(k.run().len() <= k.low_water() && k.low_water() <= k.len());
            keep[i] = p;
        }
        assert!(was_compacting, "never compacting");
        assert!(rebuilt, "no rebuild observed");
        assert_eq!(live(s, c), keep.len() + t.len(c));
        for p in keep {
            t.free(p);
        }
        t.destroy();
        assert_eq!(live(s, c), 0);
    }

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

    use super::tests::{TestHeap, page_size, shared_percpu, shared_with_guards_mode, xorshift};
    use std::collections::HashSet;
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
            page_size().trailing_zeros(),
            [
                Mapping { addr: superblock, len: sb_len },
                Mapping { addr: huge.0, len: huge.1 },
                Mapping { addr: mb, len: META_LEN },
            ],
        );
        let b: &'static mut MaybeUninit<Shared> = Box::leak(Box::new(MaybeUninit::uninit()));
        // One automatic arena: the tests below reason about `arena0`.
        let config = Config { narenas: 1, pac_pointers: true, ..Config::DEFAULT };
        assert!(unsafe { Shared::init(b.as_mut_ptr(), regions, config, false, pac::available(), false) });
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
            let raw = tls::get() as usize;
            assert!(raw > TLS_EXITED);
            // The slot holds the heap pointer, signed with the thread
            // pointer when PAC is on (`existing_heap` authenticates it).
            let hp = existing_heap(s).expect("heap") as *mut Heap;
            let h = hp as usize;
            if s.pac() {
                assert_eq!(pac::strip(raw), h);
                assert_eq!(pac::auth(raw, tls::get_tp().1), h);
            } else {
                assert_eq!(raw, h);
            }
            let heap = unsafe { &*hp };
            assert_eq!(heap.tid(), crate::sys::gettid() as u32);
            assert!(core::ptr::eq(heap.shared(), s));
            assert_eq!(heap.stats().allocated, CLASSES[c].size() as u64);
            // Same heap on the next call.
            let q = alloc(s, c);
            assert_eq!(tls::get() as usize, raw);
            assert_ne!(p, q);
            fill(p, CLASSES[c].size(), 0xa5);
            fill(q, CLASSES[c].size(), 0x5a);
            check(p, CLASSES[c].size(), 0xa5);
            free(s, p);
            free(s, q);
            assert_eq!(unsafe { &*hp }.stats().deallocated, 2 * CLASSES[c].size() as u64);
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
            let raw = tls::get();
            assert!(raw as usize > TLS_EXITED);
            // The key holds the plain pointer, the slot the signed one.
            let h = existing_heap(s).unwrap() as *mut Heap as *mut c_void;
            if s.pac() {
                assert_eq!(pac::strip(raw as usize), h as usize);
            } else {
                assert_eq!(raw, h);
            }
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

    /// Plan Phase 2.5 task 7: `PUSH_PAC` follows the probe and the key,
    /// and the key is off by default (the test builders turn it on).
    #[test]
    fn pac_flag_follows_the_probe_and_the_key() {
        let s = shared();
        assert_eq!(s.pac(), pac::available(), "pac_pointers:true is the probe's answer");
        assert_eq!(s.push_flags() & PUSH_PAC != 0, s.pac());
        assert!(!Config::DEFAULT.pac_pointers, "opt-in");
        let off = crate::heap::tests::shared_with(Config { narenas: 1, ..Config::DEFAULT });
        assert!(!off.pac(), "the default configuration signs nothing");
        assert_eq!(off.push_flags(), 0);
        #[cfg(target_arch = "x86_64")]
        assert!(!s.pac());
        std::eprintln!("pac: available={} on={} fpac={}", pac::available(), s.pac(), pac::fpac());
    }

    /// Plan Phase 2.5 task 7. With pointer authentication on, every entry
    /// of a cache's stack is stored signed (strips to the slot it names,
    /// authenticates against the stack's base) and pops return the plain
    /// pointers; the TLS slot holds the heap pointer signed with the thread
    /// pointer. Skipped where the CPU has no PAC.
    #[test]
    fn cache_entries_and_the_tls_heap_pointer_are_signed() {
        let s = shared();
        if !s.pac() {
            std::eprintln!("pac: off on this host, skipping");
            return;
        }
        std::thread::spawn(move || {
            let c = class_of(64).unwrap();
            let ps: Vec<*mut u8> = (0..8).map(|_| alloc(s, c)).collect();
            for &p in &ps {
                free(s, p);
            }
            let hp = existing_heap(s).unwrap() as *mut Heap;
            let k = unsafe { &*hp }.cache(c).unwrap();
            let stored = k.cached();
            assert!(stored.len() >= 8);
            let plain: Vec<*mut u8> = stored.iter().map(|&e| k.plain(e, true)).collect();
            assert_eq!(&plain[plain.len() - 8..], &ps[..], "the last eight pushes, bottom first");
            for (&e, &p) in stored.iter().zip(&plain) {
                assert_eq!(pac::strip(e as usize), p as usize, "entry strips to its slot");
                assert!(p as usize >> 48 == 0, "plain pointers are canonical");
            }
            // Every entry carries a PAC; with a 7-bit PAC the chance that
            // all eight are zero is 2^-56.
            assert!(stored[stored.len() - 8..].iter().zip(&plain[plain.len() - 8..]).any(|(&e, &p)| e != p));
            // The TLS slot: signed heap pointer, thread pointer as modifier.
            let (raw, tp) = tls::get_tp();
            assert_eq!(pac::strip(raw as usize), hp as usize);
            assert_eq!(pac::auth(raw as usize, tp), hp as usize);
            // Pops authenticate and hand the plain slots back, LIFO.
            for &p in ps.iter().rev() {
                assert_eq!(alloc(s, c), p);
            }
            for &p in &ps {
                free(s, p);
            }
        })
        .join()
        .unwrap();
    }

    /// Plan Phase 2.5 task 7, the acceptance test: a corrupted cache entry
    /// (an address bit flipped by a write into the metadata region) never
    /// comes back from `malloc` as a usable pointer, and a corrupted heap
    /// pointer in the TLS slot never reaches a heap. In a forked child:
    /// on FEAT_FPAC the `autda` faults (`SIGILL`); on the original PAuth
    /// the pointer comes out non-canonical and its first dereference
    /// faults (`SIGSEGV`). The flipped bit is chosen so the forged
    /// address's PAC differs (a 7-bit PAC matches by chance one time in
    /// 128). Skipped where the CPU has no PAC.
    #[test]
    fn corrupted_cache_entry_or_heap_pointer_faults() {
        let s = shared();
        if !s.pac() {
            std::eprintln!("pac: off on this host, skipping");
            return;
        }
        std::thread::spawn(move || {
            let c = class_of(48).unwrap();
            let p = alloc(s, c);
            free(s, p);
            let hp = existing_heap(s).unwrap() as *mut Heap;
            let cache = unsafe { &mut *hp }.cache_ptr(c);
            let (slots, len) = unsafe { ((*cache).slots, (*cache).len as usize) };
            assert!(len >= 1);
            let top = unsafe { *slots.add(len - 1) } as usize;
            assert_eq!(pac::auth(top, slots as usize), p as usize);
            let (raw, tp) = tls::get_tp();
            let raw = raw as usize;
            // A bit whose flip changes the PAC of both pointers.
            let bit = (12..40)
                .map(|b| 1usize << b)
                .find(|&b| {
                    pac::sign((p as usize) ^ b, slots as usize) != top ^ b && pac::sign((hp as usize) ^ b, tp) != raw ^ b
                })
                .expect("a bit that changes both PACs");
            for target in ["entry", "tls"] {
                let pid = unsafe { libc::fork() };
                assert!(pid >= 0);
                if pid == 0 {
                    unsafe {
                        if target == "entry" {
                            let e = slots.add(len - 1);
                            *e = ((*e as usize) ^ bit) as *mut u8;
                        } else {
                            tls::set((raw ^ bit) as *mut c_void);
                        }
                        let q = alloc(s, c);
                        core::ptr::write_volatile(q, 1);
                        libc::_exit(3);
                    }
                }
                let mut status = 0;
                assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
                assert!(libc::WIFSIGNALED(status), "{target}: the child survived a corrupted pointer: status {status:#x}");
                let sig = libc::WTERMSIG(status);
                if pac::fpac() {
                    assert_eq!(sig, libc::SIGILL, "{target}: FEAT_FPAC faults at the autda");
                } else {
                    assert_eq!(sig, libc::SIGSEGV, "{target}: the non-canonical pointer faults on use");
                }
                std::eprintln!("pac: corrupted {target} -> signal {sig} (fpac={})", pac::fpac());
            }
            // The parent's cache is intact.
            assert_eq!(alloc(s, c), p);
            free(s, p);
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

    /// `live` over the class's whole table, whichever arenas own the slabs
    /// (the per-CPU mode binds ids to arenas, so several are in play).
    #[cfg(target_os = "linux")]
    fn live_all(s: &Shared, c: usize) -> usize {
        let t = s.table(c);
        (0..t.committed_slabs()).filter(|&i| !t.is_guard_slab(i)).map(|i| t.desc(i).live()).sum()
    }

    /// Plan Phase 2.5 task 8: the mode follows the probe and the key, both
    /// ways. On a host whose glibc registered `mm_cid` (the reference
    /// VMs) the allowed `Shared` is on; on one without (the Mac's Docker
    /// VM, `__rseq_size` 20) it is off with the probe's verdict; the
    /// default test builders never turn it on, `percpu_arena:off` never
    /// does, and `double_free_scan` excludes it.
    #[cfg(target_os = "linux")]
    #[test]
    fn percpu_mode_follows_the_probe_and_the_key() {
        use crate::config::PercpuArena;
        let probe = rseq::probe();
        let s = shared_percpu();
        std::eprintln!(
            "percpu: probe {} (size {}, offset {}), state {}, ids {}",
            probe.verdict(),
            probe.size,
            probe.offset,
            s.percpu.state().name(),
            s.percpu.ids()
        );
        assert_eq!(s.percpu_on(), probe.mm_cid_usable() && rseq::possible_cpus().is_some());
        assert_eq!(s.percpu_on(), s.percpu.state() == PercpuState::On);
        assert_eq!(s.percpu_on(), s.push_flags() & PUSH_PERCPU != 0);
        assert!(s.percpu.forced(), "the builder asks for percpu");
        if s.percpu_on() {
            assert_eq!(s.percpu.ids(), rseq::possible_cpus().unwrap());
        } else {
            assert_eq!(s.percpu.state(), PercpuState::OffProbe);
        }
        assert!(!shared().percpu_on(), "the default builders keep thread caches");
        assert_eq!(shared().percpu.state(), PercpuState::OffAuto, "auto is the opt-in's default");
        let auto = shared_with_guards_mode(Config { narenas: 1, ..Config::DEFAULT }, false, true);
        assert!(!auto.percpu_on(), "auto keeps thread caches pending the default decision");
        assert_eq!(auto.percpu.state(), PercpuState::OffAuto);
        assert!(!auto.percpu.forced());
        let off = shared_with_guards_mode(Config { percpu_arena: PercpuArena::Off, ..Config::DEFAULT }, false, true);
        assert!(!off.percpu_on());
        assert_eq!(off.percpu.state(), PercpuState::OffKey);
        let denied =
            shared_with_guards_mode(Config { percpu_arena: PercpuArena::Percpu, ..Config::DEFAULT }, false, false);
        assert!(!denied.percpu_on(), "not allowed by the builder");
        assert_eq!(denied.percpu.state(), PercpuState::OffProbe);
        let scan = shared_with_guards_mode(
            Config { percpu_arena: PercpuArena::Percpu, double_free_scan: true, ..Config::DEFAULT },
            false,
            true,
        );
        assert!(!scan.percpu_on());
        if probe.mm_cid_usable() {
            assert_eq!(scan.percpu.state(), PercpuState::OffScan);
        }
    }

    /// With the mode on, a thread heap's plain allocations go through the
    /// running CPU's caches, not the heap's own: the refill installs the
    /// id's header, the thread's byte counters still move by every pop and
    /// push, `calloc`'s zero prefix survives the refill, and every slot
    /// out of the bitmaps is either live in the program or in a CPU cache.
    #[cfg(target_os = "linux")]
    #[test]
    fn percpu_caches_serve_plain_allocations() {
        let s = shared_percpu();
        if !s.percpu_on() {
            std::eprintln!("percpu: off on this host ({}), skipping", s.percpu.probe().verdict());
            return;
        }
        std::thread::spawn(move || {
            let t = TestHeap::new(s);
            assert!(t.heap().percpu(), "a thread heap on an automatic arena takes the per-CPU paths");
            let c = class_of(64).unwrap();
            let before = t.stats();
            let (p0, zero) = t.alloc_zeroed(c);
            assert!(zero, "a fresh slab's batch is a zero prefix");
            let ps: Vec<*mut u8> = (0..7).map(|_| t.alloc(c)).collect();
            assert!(t.heap().cache(c).is_none(), "the thread's own cache stays untouched");
            let (headers, cached) = s.percpu.snapshot();
            assert!(headers >= 1);
            let half = (Config::DEFAULT.cache_cap(c) / 2) as usize;
            assert_eq!(cached + 8, live_all(s, c), "cached + live in the program = slots out of the bitmaps");
            assert!(live_all(s, c) <= 2 * half, "at most two refills happened (one per CPU the thread ran on)");
            let mid = t.stats();
            assert_eq!(mid.allocated - before.allocated, 8 * 64, "thread.allocated counts per-CPU pops");
            for &p in ps.iter().chain([&p0]) {
                unsafe { *p = 7 };
                t.free(p);
            }
            let after = t.stats();
            assert_eq!(after.deallocated - mid.deallocated, 8 * 64, "thread.deallocated counts per-CPU pushes");
            let (_, cached) = s.percpu.snapshot();
            assert_eq!(cached, live_all(s, c));
            // A slot freed and popped again is not zero.
            let (q, zero) = t.alloc_zeroed(c);
            assert!(!zero || unsafe { *q } == 0, "the zero flag never lies");
            t.free(q);
            t.destroy();
        })
        .join()
        .unwrap();
    }

    /// The per-CPU flush: more frees than a cache holds release the
    /// highest-address half to the slabs and keep the lowest, the total
    /// stays conserved, and `flush_all` drains the running CPU's caches.
    #[cfg(target_os = "linux")]
    #[test]
    fn percpu_flush_releases_the_high_half_and_drain_empties() {
        let s = shared_percpu();
        if !s.percpu_on() {
            return;
        }
        std::thread::spawn(move || {
            let t = TestHeap::new(s);
            let c = class_of(64).unwrap();
            let cap = Config::DEFAULT.cache_cap(c) as usize;
            let n = 2 * cap + 100;
            let mut ps: Vec<*mut u8> = (0..n).map(|_| t.alloc(c)).collect();
            let mut set: HashSet<usize> = ps.iter().map(|&p| p as usize).collect();
            assert_eq!(set.len(), n, "distinct slots");
            for &p in &ps {
                t.free(p);
            }
            let (_, cached) = s.percpu.snapshot();
            assert_eq!(cached, live_all(s, c), "every slot is cached or released");
            assert!(cached <= cap * s.percpu.ids() as usize);
            // Allocate again: the cache hands back what it kept, lowest first
            // after a sorted flush, and the rest come from the slabs' low
            // addresses; nothing is handed out twice.
            ps.clear();
            set.clear();
            for _ in 0..n {
                let p = t.alloc(c);
                assert!(set.insert(p as usize), "slot handed out twice");
                ps.push(p);
            }
            for &p in &ps {
                t.free(p);
            }
            t.flush_all();
            // The running CPU's caches are empty now; another CPU's may
            // hold what the thread pushed while it ran there.
            let cid = unsafe { rseq::mm_cid(rseq::area(s.percpu.probe().offset)) };
            let h = s.percpu.header(cid, c).unwrap();
            assert_eq!(unsafe { ptr::read_volatile(&raw const (*h).len) }, 0);
            let (_, cached) = s.percpu.snapshot();
            assert_eq!(cached, live_all(s, c));
            t.destroy();
        })
        .join()
        .unwrap();
    }

    /// Many threads, more than CPUs, allocate and free at random through
    /// the per-CPU caches (migrations and aborted sections included): no
    /// slot is live in two threads at once, and at the end every slot out
    /// of the bitmaps is in a cache.
    #[cfg(target_os = "linux")]
    #[test]
    fn percpu_threads_conserve_slots_under_migration() {
        let s = shared_percpu();
        if !s.percpu_on() {
            return;
        }
        let c = class_of(64).unwrap();
        // More threads than possible CPUs: on a 6.19+ kernel that is what
        // leaves the per-task id mode (a thread otherwise keeps its own id
        // across migrations, and no header is ever shared), and on every
        // kernel it is what makes the abort path run.
        let nthreads = 2 * (rseq::possible_cpus().unwrap_or(1) as usize).max(2);
        let aborts0 = rseq::aborts();
        let handles: Vec<_> = (0..nthreads)
            .map(|i| {
                std::thread::spawn(move || {
                    let t = TestHeap::new(s);
                    let tag = (i as u64 + 1) * 0x0101_0101_0101_0101;
                    let mut mine: Vec<*mut u8> = Vec::new();
                    let mut rng = 0x9E37_79B9_7F4A_7C15u64 ^ (i as u64 + 1);
                    for _ in 0..20_000 {
                        let r = xorshift(&mut rng);
                        if mine.len() < 300 && (r & 1 == 0 || mine.is_empty()) {
                            let p = t.alloc(c);
                            unsafe { (p as *mut u64).write(tag) };
                            mine.push(p);
                        } else {
                            let k = (r as usize >> 1) % mine.len();
                            let p = mine.swap_remove(k);
                            assert_eq!(unsafe { (p as *mut u64).read() }, tag, "slot {p:?} was handed to another thread");
                            t.free(p);
                        }
                    }
                    for p in mine {
                        assert_eq!(unsafe { (p as *mut u64).read() }, tag);
                        t.free(p);
                    }
                    t.destroy();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let (headers, cached) = s.percpu.snapshot();
        assert_eq!(cached, live_all(s, c), "nothing leaked and nothing is counted twice");
        let aborts = rseq::aborts() - aborts0;
        // (A section is aborted only when a preemption or a signal lands
        // inside its few instructions; in this short run that is a
        // handful at most, and `percpu_sections_abort_and_retry_under_signals`
        // is the test that forces it.)
        std::eprintln!(
            "percpu: {nthreads} threads, {headers} headers installed, {cached} slots cached at the end, {aborts} sections aborted and retried"
        );
    }

    /// The abort path, forced: a worker pops and pushes through the CPU
    /// caches while another thread signals it as fast as it can (a signal
    /// delivered inside a section moves the thread to the abort handler
    /// before the handler frame is built, on every kernel with rseq), so
    /// sections are aborted and retried by the hundreds; every block the
    /// worker frees still carries its tag and the slots are conserved.
    #[cfg(target_os = "linux")]
    #[test]
    fn percpu_sections_abort_and_retry_under_signals() {
        use std::os::unix::thread::JoinHandleExt;
        let s = shared_percpu();
        if !s.percpu_on() {
            return;
        }
        extern "C" fn noop(_: libc::c_int) {}
        // SAFETY: installing a no-op handler for SIGUSR1, which nothing
        // else in the test binary uses.
        unsafe {
            let mut sa: libc::sigaction = core::mem::zeroed();
            sa.sa_sigaction = noop as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            assert_eq!(libc::sigaction(libc::SIGUSR1, &sa, ptr::null_mut()), 0);
        }
        let c = class_of(64).unwrap();
        let aborts0 = rseq::aborts();
        let stop = Arc::new(AtomicUsize::new(0));
        let worker = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let t = TestHeap::new(s);
                let tag = 0xA5A5_0000_0000_0001u64;
                let mut mine: Vec<*mut u8> = Vec::new();
                let mut rng = 0x1234_5678_9ABC_DEF1u64;
                let mut ops = 0u64;
                while stop.load(Ordering::Relaxed) == 0 {
                    for _ in 0..256 {
                        let r = xorshift(&mut rng);
                        if mine.len() < 200 && (r & 1 == 0 || mine.is_empty()) {
                            let p = t.alloc(c);
                            unsafe { (p as *mut u64).write(tag) };
                            mine.push(p);
                        } else {
                            let k = (r as usize >> 1) % mine.len();
                            let p = mine.swap_remove(k);
                            assert_eq!(unsafe { (p as *mut u64).read() }, tag);
                            t.free(p);
                        }
                        ops += 1;
                    }
                }
                for p in mine {
                    t.free(p);
                }
                t.destroy();
                ops
            })
        };
        let target = worker.as_pthread_t();
        let t0 = std::time::Instant::now();
        let mut signals = 0u64;
        while t0.elapsed() < std::time::Duration::from_millis(400) {
            // SAFETY: the worker is alive until `stop` is set below.
            unsafe { libc::pthread_kill(target, libc::SIGUSR1) };
            signals += 1;
            for _ in 0..200 {
                core::hint::spin_loop();
            }
        }
        stop.store(1, Ordering::Relaxed);
        let ops = worker.join().unwrap();
        let aborts = rseq::aborts() - aborts0;
        std::eprintln!("percpu: {ops} operations under {signals} signals, {aborts} sections aborted and retried");
        assert!(aborts > 0, "no section was aborted under {signals} signals");
        let (_, cached) = s.percpu.snapshot();
        assert_eq!(cached, live_all(s, c), "slots conserved across the retries");
    }

    /// Heaps off the mode inside a per-CPU process take the thread paths:
    /// an explicit cache, a thread with `thread.tcache.enabled:false`
    /// (served directly), and a thread moved to an explicit arena (its own
    /// caches on that arena).
    #[cfg(target_os = "linux")]
    #[test]
    fn percpu_heaps_off_the_mode_take_the_thread_paths() {
        let s = shared_percpu();
        if !s.percpu_on() {
            return;
        }
        std::thread::spawn(move || {
            let c = class_of(64).unwrap();
            // Explicit cache.
            let e = Heap::create_detached(s, 0);
            let eh = unsafe { &mut *e };
            assert!(!eh.percpu());
            let p = eh.alloc(s, c);
            assert!(eh.cache(c).is_some(), "an explicit cache uses its own stacks");
            eh.free(s, p);
            unsafe { Heap::destroy(e, s) };
            // Thread caches off: direct.
            let t = TestHeap::new(s);
            assert!(t.heap().percpu());
            t.heap().set_tcache_enabled(s, false);
            assert!(!t.heap().percpu());
            let live0 = live_all(s, c);
            let (_, cached0) = s.percpu.snapshot();
            let p = t.alloc(c);
            assert_eq!(live_all(s, c), live0 + 1, "claimed straight from a slab");
            assert_eq!(s.percpu.snapshot().1, cached0, "no CPU cache touched");
            t.free(p);
            assert_eq!(live_all(s, c), live0);
            t.heap().set_tcache_enabled(s, true);
            assert!(t.heap().percpu());
            // An explicit arena: the thread's own caches on it.
            let a = s.arenas.create(s).expect("arenas.create");
            t.heap().set_arena(s, a);
            assert!(!t.heap().percpu());
            let p = t.alloc(c);
            assert_eq!(s.classify_free(p), Ok((c, a)));
            assert!(t.heap().cache(c).is_some());
            t.free(p);
            assert!(t.heap().cache(c).unwrap().len() >= 1);
            t.destroy();
        })
        .join()
        .unwrap();
    }
}
