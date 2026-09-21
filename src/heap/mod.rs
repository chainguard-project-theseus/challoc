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
//! Files (design.md section 19):
//!
//! ```text
//! heap/mod.rs          CacheHeader, ArenaCaches, Heap: create/destroy, the thread-cache
//!                      fast and slow paths (pop, push, refill, flush), compaction, cache
//!                      GC and the periodic tick, thread statistics; the crate-facing
//!                      alloc/alloc_zeroed/free/free_direct and the free_fast and
//!                      free_sized_fast paths the entry points inline
//! heap/shared.rs       Shared: process-wide state and init, the free checks
//!                      (checked_slot, sized_slot, classify_free, release_routed), huge
//!                      routing, GuardTest, the thread-exit key and the OS shims (os)
//! heap/percpu.rs       the per-CPU mode (design 15): Percpu, PercpuState, CpuCache, the
//!                      per-CPU impl Heap blocks (percpu_alloc/percpu_free, the
//!                      scheduled pop and run, refill, flush, GC, drain)
//! heap/mallocx.rs      MALLOCX_* flag decoding (Flags, Tcache) and the routing of
//!                      mallocx/dallocx/sdallocx onto heaps, explicit caches and the
//!                      direct paths (design 13.2); arena.rs re-exports it for api/
//! heap/thread.rs       thread lifecycle (design 6.5): TLS_CREATING/TLS_EXITED,
//!                      heap_for_thread, existing_heap, tls_heap_slot, fast_flags,
//!                      heap_from_slot, the exit destructor
//! heap/flush.rs        the flush's grouping pass: group_by_slab, flag_sort, the
//!                      straddling double-free abort
//! heap/tests.rs        host tests and the builders (shared, shared_with, TestHeap, live,
//!                      xorshift, ...) that stats.rs and api/ctests.rs reuse as
//!                      heap::tests::*
//! heap/linux_tests.rs  tests that need real reservations, TLS and threads
//! ```
//!
//! Layout. The headers are a
//! dense `[CacheHeader; NCLASSES]` inside the heap, one 64-byte line each,
//! so `malloc` reaches `len` at `heap + 128 + c * 64` without first loading
//! a cache pointer, and a thread's 52 headers share one page instead of 52.
//! Each header keeps a pointer to its stack of `cap` slot pointers, on the
//! same line as `len`; the stack is its own metadata object, allocated on
//! the class's first slow path, so a thread that only uses five classes
//! pays for five stacks. The stacks are not carved from one contiguous
//! slot block per heap: a stack inside a 128 KiB metadata object costs
//! xmalloc-test's freeing threads three to four times the L1D fills of a
//! stack in an 8 KiB object (the same lines, the same page geometry),
//! whatever the offset inside the object
//! (`bench/results/2026-09-18-*-gce-dense-headers.md`).
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
//! Signed pointers (`sys::pac`). On an arm64 host
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


mod flush;
pub mod mallocx;
mod percpu;
mod shared;
mod thread;

#[cfg(test)]
pub(crate) mod tests;
#[cfg(all(test, target_os = "linux"))]
mod linux_tests;

pub use percpu::{CPU_CACHE_SIZE, CpuCache, Percpu, PercpuState};
pub use shared::{GuardTest, Shared};
#[cfg(test)]
use shared::guard_interval_for;
#[cfg(target_os = "linux")]
pub use thread::{existing_heap, heap_destructor, heap_for_thread, tls_heap_slot};
pub use thread::{TLS_CREATING, TLS_EXITED, fast_flags, heap_from_slot};

use core::alloc::Layout;
use core::ptr;

use crate::arena::Arena;
use crate::config::Config;
use crate::layout::{CLASSES, NCLASSES};
#[cfg(target_os = "linux")]
use crate::rseq;
use crate::sys::pac;

use flush::{group_by_slab, straddling_double_free};
use shared::os;


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
/// Slow-path entries between two periodic-work checks (design 6.4).
pub const PERIODIC_EVERY: u32 = 64;

/// Default cache capacity for a class: [`MAX_CAP`] (1024) slots for classes
/// up to 128 bytes, halved for every doubling of the class size, never
/// below [`MIN_CAP`].
pub const fn default_cap(class: usize) -> u32 {
    crate::config::ladder(class, MAX_CAP, 1)
}

/// One thread's cache header for one size class: exactly one cache line,
/// inline in the heap ([`Heap::caches`]) or in an [`ArenaCaches`] node,
/// pointing at its stack of `cap` slot pointers, a metadata object of its
/// own (module doc, Layout). The fields the fast paths read (`len`, `cap`, `zeroed`, `slots`,
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
/// `Shared::push_flags` bit: pointer authentication is on (`sys::pac`).
/// Read by the pop as well as the push: every slot
/// pointer in a cache's stack is stored signed (`pacda`, modifier the
/// stack's base address) and authenticated on the way out (`autda`), and
/// the heap pointer in TLS is signed with the thread pointer. Set by `init`
/// when the kernel exposes `HWCAP_PACA`/`PACG` and `pac_pointers` is not
/// `false`; never set on x86_64.
pub const PUSH_PAC: u8 = 4;
// The per-CPU mode (design 15) has no bit here: a heap in the mode keeps
// every home cache header `EMPTY` (`len == cap == 0`), which both fast
// paths already treat as "slow path", and the slow paths route on
// `Heap::percpu`; the entry points carry nothing for the mode.
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
/// slots per second, which at one slab per window would keep the frozen
/// residue of the Valkey soak from ever being compacted. The residue
/// itself sees a refill only when a class's random-size churn walks a
/// cache to empty, a few per second.
pub const COMPACT_MAX_REFILL_SLABS: usize = 2;
// Both slab counts are in 64 KiB slabs (`Class::compact_slots`), so the
// thresholds mean the same bytes for the 4 KiB-slab classes as for the rest.

/// What a compaction probe decided ([`Heap::probe_verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Ordinary refills claimed [`COMPACT_MAX_REFILL_SLABS`] slabs' worth
    /// of slots since the previous probe: the class is being drained by
    /// them and is left alone.
    Refilling,
    /// Not fragmented ([`Heap::fragmented`]): keep probing.
    Dense,
    /// Fragmented and left alone by refills: enter compact mode.
    Fragmented,
}

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

    /// The run of low addresses, `slots[..run_end]`, descending, as
    /// stored: signed when `Shared::pac()` ([`CacheHeader::plain`]).
    #[cfg(test)]
    pub fn run(&self) -> &[*mut u8] {
        debug_assert!(self.run_end <= self.low_water && self.low_water <= self.len);
        if self.run_end == 0 {
            return &[];
        }
        // SAFETY: `run_end <= len <= cap` pointers exist at `slots`.
        unsafe { core::slice::from_raw_parts(self.slots, self.run_end as usize) }
    }

    /// Cached pointers, bottom of the stack first, as stored: signed when
    /// `Shared::pac()` ([`CacheHeader::plain`]).
    #[cfg(test)]
    pub fn cached(&self) -> &[*mut u8] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `len <= cap` pointers exist at `slots`.
        unsafe { core::slice::from_raw_parts(self.slots, self.len as usize) }
    }

    /// Pops served from this cache since it was created.
    #[cfg(test)]
    pub fn allocs(&self) -> u64 {
        self.allocs
    }

    /// Whether the cache is currently compacting its class.
    #[cfg(test)]
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
    /// (`Shared::pac()`), the entry itself otherwise. For tests reading
    /// [`CacheHeader::cached`] or [`CacheHeader::run`], which return the
    /// entries as stored.
    #[cfg(test)]
    pub fn plain(&self, e: *mut u8, pac: bool) -> *mut u8 {
        if pac { pac::auth(e as usize, self.slots as usize) as *mut u8 } else { e }
    }

    /// The scheduled-pop mask `mode` implies: [`PROBE_MASK`] while
    /// probing, the configured `tcache_compact_shift` mask while
    /// compacting, `u32::MAX` when off (one pop in 2^32 then reaches
    /// `pop_compact`, which pops the top). The one home of the
    /// mode-to-mask rule; [`CacheHeader::set_mode`] and
    /// [`CacheHeader::set_mode_volatile`] apply it.
    fn mask_for(mode: u16, config: &Config) -> u32 {
        match mode {
            MODE_PROBE => PROBE_MASK,
            MODE_COMPACT => config.compact_mask(),
            _ => u32::MAX,
        }
    }

    /// Enter `mode` and derive the scheduled-pop mask from it
    /// ([`CacheHeader::mask_for`]), so the two fields never disagree.
    fn set_mode(&mut self, mode: u16, config: &Config) {
        self.mode = mode;
        self.compact_mask = Self::mask_for(mode, config);
    }

    /// [`CacheHeader::set_mode`] for a header of the per-CPU table
    /// (`percpu.rs`), whose mode fields are hints written with volatile
    /// stores: another thread may own the id by the time they land, and
    /// a stale mode changes only which pops are scheduled, never a stack.
    ///
    /// # Safety
    /// `h` is a live header of the per-CPU table.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    unsafe fn set_mode_volatile(h: *mut CacheHeader, mode: u16, config: &Config) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            ptr::write_volatile(&raw mut (*h).mode, mode);
            ptr::write_volatile(&raw mut (*h).compact_mask, Self::mask_for(mode, config));
        }
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
    /// and [`Heap::set_tcache_enabled`]. Read on the slow paths only: a
    /// heap with it set never installs a home cache ([`Heap::ensure_cache`]
    /// refuses the home arena), so its home headers stay `EMPTY` and the
    /// entry points' pop and push fall through to the slow paths, which
    /// route here ([`Heap::alloc`], [`Heap::alloc_zeroed`], [`Heap::free`],
    /// [`Heap::alloc_slow`]); the fast paths carry no test for the mode.
    percpu: bool,
    /// The owning thread's rseq area (`rseq::area` at creation; the heap is
    /// created on its thread), read by the per-CPU paths; null when the
    /// mode is off.
    rseq_area: *mut u8,
    /// `slot_randomize`'s xorshift state: 0 until the heap's first
    /// randomised claim seeds it from `getrandom`, so the
    /// default configuration makes no syscall per heap creation.
    rng: u64,
    /// Caches of the home arena, one line per class; last, so the small
    /// fields above share two lines and `caches[c]` is `heap + 128 + 64 * c`.
    caches: [CacheHeader; NCLASSES],
}

const _: () = assert!(core::mem::offset_of!(Heap, caches) == 128);

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
        // temporary (3.4 KiB with the headers inline) would deepen the
        // stack of every thread's first allocation by one page, which
        // larson's thread churn turns into +1.7 MiB of RSS per 437
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
        for o in Self::nodes(heap.others) {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap; the iterator has read `next` already, so the
            // block can go.
            unsafe {
                Self::free_caches(shared, &mut (*o).caches);
                let block = (o as *mut u8).sub(64 * (*o).pad as usize);
                shared.meta.free(block, node_block_layout());
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

    /// The `Shared` this heap was created on.
    #[cfg(test)]
    pub fn shared(&self) -> &Shared {
        // SAFETY: a heap never outlives the `Shared` it was created on.
        unsafe { &*self.shared }
    }

    /// The owning thread's kernel thread id at creation.
    #[cfg(test)]
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
        for o in Self::nodes(self.others) {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe { Self::fold_counters(&mut s, &(*o).caches) };
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
        for o in Self::nodes(self.others) {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                Self::fold_counters(&mut self.uncached, &(*o).caches);
                Self::free_caches(shared, &mut (*o).caches);
            }
        }
    }

    /// Slow-path entries so far (the periodic tick's counter).
    #[cfg(test)]
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
        // SAFETY: `others` is a list of live metadata blocks owned by this
        // heap.
        let node = Self::nodes(self.others).find(|&o| unsafe { (*o).arena } == arena)?;
        // SAFETY: as above.
        let c = unsafe { &(*node).caches[class] };
        if c.cap == 0 { None } else { Some(c) }
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

    /// `malloc` of a non-huge class in the home arena: the cache pop, then
    /// the slow path. A heap in the per-CPU mode (design 15) goes to the
    /// running CPU's cache instead ([`Heap::percpu_alloc`]); its home
    /// caches are never installed, so the entry point's inlined pop has
    /// already fallen through by the time this runs.
    #[inline(always)]
    pub fn alloc(&mut self, shared: &Shared, class: usize) -> *mut u8 {
        if self.percpu {
            return self.percpu_alloc(shared, class).0;
        }
        match self.alloc_fast(class, shared.push_flags()) {
            Some(p) => p,
            None => self.alloc_slow(shared, class).0,
        }
    }

    /// The cache pop alone: `None` when the cache is empty (or still the
    /// sentinel) and the caller must take a slow path. `flags` is
    /// `Shared::push_flags` (only [`PUSH_PAC`] matters to a pop).
    #[inline(always)]
    pub fn alloc_fast(&mut self, class: usize, flags: u8) -> Option<*mut u8> {
        self.pop_fast(class, flags).map(|r| r.0)
    }

    /// [`Heap::alloc_fast`] with the zero flag: the one body of the home
    /// cache's "load `len`, zero means slow, else pop", shared by `malloc`
    /// (`alloc_fast`) and `calloc` ([`Heap::alloc_zeroed`]).
    ///
    /// The scheduled pop (design 6.4, every `compact_mask + 1` pops) is
    /// declined here (`None`, `allocs` not yet counted) and served by
    /// [`Heap::alloc_slow`], the one place every caller's `None` lands:
    /// `pop_compact` was the one call inside the inlined pop, and a call in
    /// the body costs `malloc` a frame it needs for nothing else (plan
    /// Phase 2.5 task 4c's budget; the same move as the scan's
    /// `cached_twice` in [`Heap::push_cache`]).
    #[inline(always)]
    pub(super) fn pop_fast(&mut self, class: usize, flags: u8) -> Option<(*mut u8, bool)> {
        let cache = self.cache_ptr(class);
        // SAFETY: this thread's own header, unused (`len == 0`) or with a
        // stack of `len` entries.
        unsafe {
            let len = (*cache).len;
            if len == 0 {
                return None;
            }
            let allocs = (*cache).allocs.wrapping_add(1);
            if ((allocs as u32) & (*cache).compact_mask) == 0 {
                return None;
            }
            (*cache).allocs = allocs;
            Some(Self::pop_top(cache, len, flags & PUSH_PAC != 0))
        }
    }

    /// Fast path of `calloc`: the flag is true when the memory is known to
    /// read as zero, so the caller can skip its `memset`. The per-CPU
    /// mode as in [`Heap::alloc`].
    #[inline(always)]
    pub fn alloc_zeroed(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        if self.percpu {
            return self.percpu_alloc(shared, class);
        }
        match self.pop_fast(class, shared.push_flags()) {
            Some(r) => r,
            None => self.alloc_slow(shared, class),
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
                    let (refilled, verdict) = Self::probe_verdict(shared, (*cache).arena, class, (*cache).probe_refilled);
                    (*cache).probe_refilled = refilled;
                    match verdict {
                        Verdict::Refilling | Verdict::Dense => return Self::pop_top(cache, len, pac),
                        Verdict::Fragmented => {}
                    }
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

    /// The compaction probe's gate (module doc), shared by the thread
    /// ([`Heap::pop_compact`]) and per-CPU (`percpu_scheduled`) scheduled
    /// pops: the class's refill counter now (the caller stores it as the
    /// header's `probe_refilled`) and the verdict, given the counter at
    /// the previous probe. Records the class's `note_gate_refill` and
    /// `note_compact_mode(true)` statistics as it decides.
    fn probe_verdict(shared: &Shared, arena: u16, class: usize, prev_refilled: u32) -> (u32, Verdict) {
        let cs = shared.owner(arena).class(class);
        let refilled = cs.refilled() as u32;
        let delta = refilled.wrapping_sub(prev_refilled) as usize;
        let verdict = if delta >= COMPACT_MAX_REFILL_SLABS * CLASSES[class].compact_slots() {
            if Self::fragmented(shared, arena, class) {
                cs.note_gate_refill();
            }
            Verdict::Refilling
        } else if !Self::fragmented(shared, arena, class) {
            Verdict::Dense
        } else {
            cs.note_compact_mode(true);
            Verdict::Fragmented
        };
        (refilled, verdict)
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
        if self.percpu {
            return self.percpu_free(shared, p);
        }
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
    fn count_uncached_alloc(&mut self, bytes: usize) {
        self.uncached.allocated += bytes as u64;
    }

    /// `bytes` were freed on this thread without passing through a cache:
    /// count them in `thread.deallocated`.
    #[inline]
    fn count_uncached_free(&mut self, bytes: usize) {
        self.uncached.deallocated += bytes as u64;
    }

    /// A slot of `class` was claimed directly (`MALLOCX_TCACHE_NONE`) or
    /// popped from a per-CPU cache on this thread: count it.
    #[inline(always)]
    pub fn count_direct_alloc(&mut self, class: usize) {
        self.count_uncached_alloc(class_size(class));
    }

    /// A slot of `class` was released directly or pushed onto a per-CPU
    /// cache on this thread: count it.
    #[inline(always)]
    pub fn count_direct_free(&mut self, class: usize) {
        self.count_uncached_free(class_size(class));
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

    /// `double_free_scan`'s compare (design 6.3 step 3): whether `e`, the
    /// entry as it would be stored (signed under `pac`), is one of the top
    /// four of the `len` entries in `slots`. Shared by the fast push, which
    /// declines on a hit, and by [`Heap::free_slow_in`], which aborts on
    /// one; no call in the body.
    ///
    /// # Safety
    /// `slots` has `len` entries.
    #[inline(always)]
    unsafe fn cached_recently(slots: *mut *mut u8, len: u32, e: *mut u8) -> bool {
        if len == 0 {
            return false;
        }
        // SAFETY: guaranteed by the caller.
        unsafe {
            let top = slots.add(len as usize);
            if len >= 4 {
                (*top.sub(1) == e) | (*top.sub(2) == e) | (*top.sub(3) == e) | (*top.sub(4) == e)
            } else {
                (1..=len as usize).any(|i| *top.sub(i) == e)
            }
        }
    }

    /// Push onto `cache` unless it is full. With `scan`, compare `p`
    /// against the top four entries first (design 6.3 step 3): a match is
    /// a double free whose first copy is still
    /// cached, which the bit test cannot see, and the push declines
    /// (`false`) without storing the second copy; the caller's slow path
    /// ([`Heap::free_slow_in`]) repeats the compare and aborts by name. Not
    /// an abort from here: `cached_twice` was the one call inside the
    /// inlined push, and a call in the body costs every `free` and
    /// `sdallocx` a frame (the sized path's instruction budget, plan
    /// Phase 2.5 task 4c: 7 instructions per call on x86_64, the
    /// `stp`/`ldp` pair on aarch64). Four unconditional compares from the
    /// line the push writes to; a cache holding fewer than four takes the
    /// loop.
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
            if scan && Self::cached_recently(slots, len, e) {
                return false;
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
            // Not reached through `alloc`/`alloc_zeroed`, which route
            // first; kept so no path can install a home cache on a heap
            // in the mode.
            return self.percpu_alloc_slow(shared, class);
        }
        // A non-empty home cache means `pop_fast` declined the scheduled
        // pop (design 6.4) rather than found the cache empty: count the
        // pop it declined and serve it here, out of the fast path's body.
        let cache = self.cache_ptr(class);
        // SAFETY: this thread's own header, unused or with `len` entries.
        unsafe {
            let len = (*cache).len;
            if len != 0 {
                let allocs = (*cache).allocs.wrapping_add(1);
                debug_assert!(((allocs as u32) & (*cache).compact_mask) == 0);
                (*cache).allocs = allocs;
                return self.pop_compact(cache, class, len);
            }
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
    /// allocating the cache first if this is the class's first use.
    /// Releases directly to the slab when there is no cache to be had.
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
        let cache = self.ensure_cache(shared, arena, class);
        if cache.is_null() {
            self.tick(shared);
            shared.free_direct(arena, class, p);
            self.count_direct_free(class);
            return;
        }
        // SAFETY: `cache` is this heap's real cache for `(arena, class)`.
        unsafe {
            // `double_free_scan`: the fast push declined on a hit among the
            // top four (or the cache was full, or `zero_on_free` sent every
            // push here); repeat the compare before anything can move an
            // entry (the tick's GC or the flush release the highest
            // addresses, the first copy possibly among them) and abort by
            // name, as the push did when the call was inline.
            if shared.double_free_scan() {
                let slots = (*cache).slots();
                let e = if shared.pac() { pac::sign(p as usize, slots as usize) as *mut u8 } else { p };
                if Self::cached_recently(slots, (*cache).len, e) {
                    cached_twice(p);
                }
            }
            self.tick(shared);
            if (*cache).len == (*cache).cap {
                let half = ((*cache).cap / 2).max(1);
                self.flush(shared, arena, cache, class, half as usize);
            }
            // The scan ran above; a flush only removes entries.
            if !Self::push_cache(cache, p, false, shared.pac()) {
                unreachable!("heap: cache still full after a flush");
            }
        }
    }

    /// The nodes of an `others` list headed by `head`, in list order, for
    /// the cold walks (statistics, flush, GC, `cache_in`, destruction).
    /// Each node's `next` is read before the node is yielded, so a caller
    /// may free the node it was handed. The lookup on the cross-arena free
    /// path ([`Heap::other_caches`]) keeps its own loop.
    fn nodes(head: *mut ArenaCaches) -> impl Iterator<Item = *mut ArenaCaches> {
        let mut o = head;
        core::iter::from_fn(move || {
            if o.is_null() {
                return None;
            }
            let cur = o;
            // SAFETY: the list is live metadata blocks owned by the heap
            // whose `others` head this was.
            o = unsafe { (*cur).next };
            Some(cur)
        })
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
    /// exhausted, caches are off (`tcache:false`), or `arena` is the home
    /// arena of a heap in the per-CPU mode: its home headers stay `EMPTY`
    /// so the entry points' inlined pop and push fall through to the slow
    /// paths, which serve the running CPU's cache (design 15); an explicit
    /// `MALLOCX_ARENA` request for such a heap's own home arena is served
    /// directly from the slab instead.
    fn ensure_cache(&mut self, shared: &Shared, arena: u16, class: usize) -> *mut CacheHeader {
        // The capacity first: a class that is never cached for this arena
        // must not create an `others` node. This is the one place the
        // placement rule is consulted on the allocation and free paths.
        let cap = shared.config.cap_for(self.arena, arena, class);
        if cap == 0 || !self.tcache_enabled || (self.percpu && arena == self.arena) {
            return ptr::null_mut();
        }
        // The arena before the node: `arena` came from an owner word on
        // the free path, and a word naming an arena that was never built
        // aborts here (`Shared::owner`) before a node is created for it.
        let refilled = shared.owner(arena).class(class).refilled() as u32;
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
                    probe_refilled: refilled,
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
    /// which is what lets them drain (module doc). One `sort_unstable` of
    /// the whole slot array per flush would be 8..9% of glibc-simple and
    /// 5% of the Python json row under tiny-object churn on both VMs, the
    /// largest single term wherever challoc trailed mimalloc or tcmalloc
    /// by more than 10% (`bench/results/2026-09-19-*-gce-flush-sort.md`),
    /// so the ordering work is the least that keeps the release grouped:
    ///
    /// - A slot array already in address order (a cache pushed in the order
    ///   a producer's refill batch was handed out, xmalloc-test's consumer
    ///   side, where even a sort's O(n) presorted path is about 1.25k
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
    ///   (xmalloc-test +16% that way; a full sort re-establishes the order
    ///   at every flush), while a cache pushed in arbitrary order pays two
    ///   sorts of half the array and then one in 32 (the unconditional
    ///   kept sort cost glibc-simple and Python their whole gain). The run
    ///   is dropped in every case and rebuilt by the scheduled pop (design
    ///   6.4); the zero prefix is given up.
    ///
    /// Candidates rejected, all in the results file: a selection followed
    /// by a sort of the released half (the selection scrambles it, so that
    /// is a full sort: xmalloc +51%); an in-place bucket pass over the
    /// whole array by address (about 10 cycles per entry, a full sort's
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
            // when the key is the identity. The mask itself only when
            // signed: `pac::mask` is an `xpacd`, UNDEFINED on a CPU without
            // the extension (`SIGILL` on Neoverse N1, the Altra), and no
            // PAC instruction may run unless the probe said yes.
            let addr_mask = if pac { !pac::mask() } else { usize::MAX };
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
        for o in Self::nodes(self.others) {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            let arena = unsafe { (*o).arena };
            self.flush_arena(shared, arena, o);
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
        for o in Self::nodes(self.others) {
            // SAFETY: as in `flush_thread_caches`.
            let arena = unsafe { (*o).arena };
            self.gc_arena(shared, arena, o);
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

/// [`free_fast`] that also names the class it pushed, for
/// `challoc_free_usable` (design 13.7): `Some(class)` when `p` was
/// verified as a slot of `class` and pushed, `None` for anything off the
/// fast path (the caller then takes its cold exact path). The same loads
/// as `free_fast`, no call.
#[inline(always)]
pub fn free_fast_class(shared: &Shared, h: &mut Heap, p: *mut u8, flags: u8) -> Option<usize> {
    let class = shared.checked_slot(p, h)?;
    if h.push_fast(class, p, flags) { Some(class) } else { None }
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

/// `zero_on_free`: clear the class-size bytes of slot `p`. Called from the
/// slow paths only (`Heap::free_slow_in`, `free_direct`), so the default
/// `free` carries nothing of it but the flag test; a `memset` of the class
/// size, so a 1 KiB value costs a 1 KiB store (and the slow-path call) per
/// free, which is why the key is off by default (priced in
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
/// Called from the slow path only ([`Heap::free_slow_in`]): the fast push
/// declines instead of calling, so the entry points that inline it keep no
/// frame for this call.
#[cold]
#[inline(never)]
fn cached_twice(p: *mut u8) -> ! {
    panic!("double free of {:#x}: the slot is already in the thread cache", p as usize)
}

/// `CLASSES[class].size()` without the bounds check: every `class` on the
/// fast paths came from `class_of` or a slab descriptor, so it is below
/// `NCLASSES`, and the check was two instructions and a panic call in the
/// per-CPU wrappers.
#[inline(always)]
fn class_size(class: usize) -> usize {
    debug_assert!(class < NCLASSES);
    // SAFETY: as above.
    unsafe { CLASSES.get_unchecked(class) }.size()
}
