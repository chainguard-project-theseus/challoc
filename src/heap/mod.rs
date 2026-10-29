//! Thread heaps: the per-thread caches in front of the slab bitmaps
//! (design.md section 6).
//!
//! Every thread that allocates gets a [`Heap`] in the metadata region,
//! reached through the one TLS slot in `sys/tls.rs`. A heap holds one
//! [`CacheHeader`] per size class inline, one 64-byte line each at
//! `heap + 128 + 64 * class`, so `malloc` reaches the header without
//! loading a pointer first. A header points at its stack of `cap` slot
//! pointers: slots marked allocated in their slab bitmaps but not live in
//! the program. The stack is a metadata object of its own, allocated on
//! the class's first slow path.
//!
//! - `malloc` pops the top of the class's stack. An empty stack is
//!   *refilled* with up to `cap / 2` slots claimed from the class's lowest
//!   partial slab, lowest address on top.
//! - `free` classifies the pointer, checks that its bitmap bit is set
//!   (invalid and stale double frees abort there) and pushes it. A full
//!   stack is *flushed*: its `cap / 2` highest addresses go back to their
//!   slabs in one batch grouped by slab.
//!
//! Neither fast path touches a slab descriptor, takes a lock or does an
//! atomic read-modify-write. Every header starts with `len == cap == 0`,
//! so an unused cache is simply both empty and full and the slow path
//! gives it a capacity and a stack. A thread without a heap (metadata
//! exhausted, inside heap creation, or past its exit destructor) is served
//! straight from the slab bitmaps; huge requests bypass the caches.
//!
//! On top of that: a compacting cache serves one pop in `2^COMPACT_SHIFT`
//! from a run of low addresses so a fragmented class drains its high slabs
//! (design 6.4); a heap keeps separate caches for other arenas and one
//! flush-only cache for frees of other arenas' slots (design 6.2, 6.3);
//! with pointer authentication every stack entry is stored signed with
//! the stack's base address as the modifier (`sys::pac`); and a
//! `pthread_key` destructor flushes and frees the heap at thread exit
//! (design 6.5).

mod cache;
mod compact;
mod flush;
mod fresh;
pub(crate) mod live;
pub mod mallocx;
mod others;
mod paths;
mod shared;
mod tcaches;
mod thread;
mod zero;

#[cfg(test)]
mod linux_tests;
#[cfg(test)]
pub(crate) mod tests;

pub use cache::CacheHeader;
#[cfg(test)]
use compact::{
    COMPACT_MASK, COMPACT_MAX_REFILL_SLABS, COMPACT_MIN_SLABS, COMPACT_SHIFT, MODE_COMPACT,
    PROBE_MASK,
};
pub use compact::{MODE_OFF, MODE_PROBE};
pub use others::ArenaCaches;
#[cfg(test)]
pub use paths::free_fast;
pub use paths::{
    alloc, alloc_declined, alloc_zeroed, free, free_direct, free_fast_slot, free_sized_fast,
};
#[cfg(verus_keep_ghost)]
pub use shared::Built;
pub use shared::Shared;
#[cfg(verus_keep_ghost)]
pub use shared::shared_storage;
#[cfg(test)]
use shared::{FAIL_SETSPECIFIC, guard_interval_for};
pub use thread::{TLS_CREATING, TLS_EXITED, fast_flags};
pub use thread::{existing_heap, heap_destructor, heap_for_thread, tls_heap_slot};

use core::alloc::Layout;
use core::ptr;

use crate::layout::{CLASSES, NCLASSES};
#[cfg(verus_keep_ghost)]
use crate::layout::{class_table, process_regions};

use crate::arena::Homed;
use crate::lock::LockOrder;
use crate::vspec::zero::Zeroed;
use fresh::FreshHeap;
#[cfg(verus_keep_ghost)]
pub(crate) use live::Live;
use others::{node_block_layout, node_caches_detached, node_set_arena};
use paths::invalid_free;
#[cfg(test)]
use shared::ncpus;
use shared::{now_ns, tid};
use vstd::map::Map;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, verus};
use vstd::raw_ptr::PointsTo;
use vstd::resource::set::GhostSingleton;

verus! {

/// Largest default cache capacity (classes up to 128 bytes). 1024 rather
/// than jemalloc's 200: every flush or refill writes the descriptor lines of
/// the slabs it touches and invalidates them in every other core's cache,
/// and with one arena those slabs are shared, so the `free` check's bitmap
/// load misses in proportion to the flush rate. Doubling the capacity from
/// 256 to 512 and again to 1024 took larson from 225 to 243 to 252 M ops/s
/// and xmalloc-test from 201 to 390 to 590 M frees/s at 10 threads for
/// 8 MiB of larson RSS.
pub const MAX_CAP: u32 = 1024;
/// Smallest cache capacity.
pub const MIN_CAP: u32 = 8;

} // verus!

/// Slow-path entries between two periodic-work checks (design 6.4).
pub const PERIODIC_EVERY: u32 = 64;
/// Slow-path entries ([`Heap::slow_ticks`]) before which a new thread heap
/// may still move its home onto the arena of a slot it frees
/// (`Heap::try_rehome`, design 6.5). A thread that takes over a dead
/// thread's data (larson's successors) frees it from its first operation
/// on, and moved within 2..3 slow-path entries on average (C4A); a thread
/// that frees other threads' data only after building its own (mstress:
/// about 140 entries at the first such free) never qualifies, so it keeps
/// its own data at home. Below the limit a heap has refilled at most a
/// few caches, so the move's flush is small and abandons nothing.
pub const REHOME_TICKS: u32 = 8;

/// Default cache capacity for a class: [`MAX_CAP`] (1024) slots for classes
/// up to 128 bytes, halved for every doubling of the class size, never
/// below [`MIN_CAP`].
#[cfg(test)]
pub const fn default_cap(class: usize) -> u32 {
    crate::config::ladder(class, MAX_CAP, 1)
}

/// The `others` key of the one flush-only cache per class that plain
/// frees of another arena's slots go onto ([`ArenaCaches`]); never an
/// arena index (`arena::MAX_ARENAS` is 4096).
pub const FOREIGN: u16 = u16::MAX;

verus! {

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

/// `Shared::free_flags` bit, never in `push_flags`: more than one arena
/// exists, so plain `free` compares the slot's owner word with the heap's
/// home arena before pushing.
pub const FREE_ROUTE: u8 = 8;

/// The process's regions are well formed and separated: a heap exists
/// only once the `Shared` is published, and [`Regions::from_parts`],
/// which builds its regions (`Regions::reserve_owning` calls it), ensures
/// both.
///
/// [`Regions::from_parts`]: crate::layout::Regions::from_parts
pub open spec fn regions_ok() -> bool {
    process_regions().wf() && process_regions().separated()
}

/// What an allocation of `size` bytes at `align` (a power of two) returns
/// (pointer authentication on or off): null, or a block of
/// the process's class or huge regions with at least `size` usable bytes
/// ([`Regions::user_block`](crate::layout::Regions::user_block)) whose
/// address is a multiple of `align`.
pub open spec fn fits(p: *mut u8, size: int, align: int) -> bool {
    p@.addr != 0 ==> {
        &&& regions_ok()
        &&& process_regions().user_block(p@.addr, size)
        &&& (p@.addr as int) % align == 0
    }
}

/// Region separation for an allocation's result: a non-null block that
/// [`fits`] lies in a class region or the huge region, whole (its `size`
/// bytes, at least one), never in a gap between class regions, and
/// neither its start nor any of its bytes is in the metadata region.
pub proof fn theorem_region_separation(p: *mut u8, size: int, align: int)
    requires
        fits(p, size, align),
        p@.addr != 0,
        size >= 0,
    ensures
        ({
            let r = process_regions();
            &&& (exists|c: int|
                0 <= c < NCLASSES && #[trigger] r.class_base[c] <= p@.addr && p@.addr + (if size < 1 { 1 } else { size }) <= r.class_base[c] + (1usize << r.lg_region))
                || (r.huge_base <= p@.addr && p@.addr + (if size < 1 { 1 } else { size }) <= r.huge_base + r.huge_len)
            &&& !(r.meta_base <= p@.addr && p@.addr < r.meta_base + r.meta_len)
            &&& (p@.addr + size <= r.meta_base || r.meta_base + r.meta_len <= p@.addr)
        }),
{
    process_regions().lemma_user_block_separated(p@.addr, size);
}

/// Guard pages are never handed out: no byte of a non-null block that
/// [`fits`] lies in a guard slab of any class region
/// ([`guard_slab`](crate::layout::guard_slab)), the slabs `SlabTable`
/// keeps for its guard pages (`MADV_GUARD_INSTALL` on the page group's
/// first page, or the whole slab or page group left `PROT_NONE`). A slab
/// block is a slot start ([`Regions::is_slot`]), which excludes them.
///
/// [`Regions::is_slot`]: crate::layout::Regions::is_slot
pub proof fn theorem_guard_pages(p: *mut u8, size: int, align: int, c: int, slab: int)
    requires
        fits(p, size, align),
        p@.addr != 0,
        size >= 0,
        0 <= c < NCLASSES,
        0 <= slab < process_regions().spec_nslabs(c),
        crate::layout::guard_slab(c, slab),
    ensures
        crate::layout::disjoint(
            p@.addr as int,
            if size < 1 { 1 } else { size },
            process_regions().slab_span(c, slab).0,
            process_regions().slab_span(c, slab).1,
        ),
{
    process_regions().lemma_user_block_not_guard(p@.addr, size, c, slab);
}

/// The free side of [`theorem_guard_pages`]: a [`free_target`] is no
/// address in a guard slab. A slot start is never in a guard slab
/// ([`Regions::is_slot`]), and a huge page start lies in the huge region,
/// apart from every class region.
///
/// [`Regions::is_slot`]: crate::layout::Regions::is_slot
pub proof fn theorem_guard_free(p: *mut u8, c: int, slab: int)
    requires
        regions_ok(),
        free_target(p),
        0 <= c < NCLASSES,
        0 <= slab < process_regions().spec_nslabs(c),
        crate::layout::guard_slab(c, slab),
    ensures
        crate::layout::disjoint(p@.addr as int, 1, process_regions().slab_span(c, slab).0, process_regions().slab_span(c, slab).1),
{
    let r = process_regions();
    if slot_start(p) {
        let c2 = choose|c2: int| #[trigger] r.is_slot(c2, p@.addr);
        assert(r.is_slot(c2, p@.addr) && 0 <= class_table()[c2].size);
    }
    assert(r.user_block(p@.addr, 0));
    r.lemma_user_block_not_guard(p@.addr, 0, c, slab);
}

/// `p` is a slot start of some class in the process's regions: what every
/// cache push and bitmap release acts on.
pub open spec fn slot_start(p: *mut u8) -> bool {
    exists|c: int| #[trigger] process_regions().is_slot(c, p@.addr)
}

/// Where a free may act: a slot start of some class, or a page start in
/// the huge region, of the process's regions. Any other pointer (outside
/// every class region and the huge region, in a gap, or inside a slot or
/// a page) is an invalid free. A slot start is never in a guard slab
/// ([`Regions::is_slot`](crate::layout::Regions::is_slot)), so a pointer
/// into one is not a free target ([`theorem_guard_free`]).
///
/// Not stated: that the block is live. A slot start whose bit is clear
/// aborts on the exact paths (`SlabTable::check_located` and
/// `check_live` test the bit at run time); the sized fast path
/// (`sdallocx`, `free_sized`) pushes a slot start of the hinted class
/// without the bit test, and the flush catches a stale one. A page start
/// that is not a live run start aborts in `Huge::free_run`. That a slot
/// start the program passes was allocated to it and not freed since is
/// the C caller's contract.
pub open spec fn free_target(p: *mut u8) -> bool {
    ||| slot_start(p)
    ||| process_regions().is_huge_block(p@.addr, 1)
}

/// A sized free's size hint names `p`'s class: if `p` is a slot start of
/// class `c`, `hint` is `Some(c)`.
/// `hint` is the class the size (and alignment) the program passed name,
/// `None` for a size no class holds or an alignment that is not a power
/// of two, so a slot freed with the size of another class, or with a huge
/// size, fails it. The sized frees test it of a slot before they push or
/// release it (`Shared::sized_slot` on the fast path, else
/// `Shared::check_size_hint`, which aborts); a pointer that is no slot
/// start passes it, and the exact free behind it judges the pointer.
pub open spec fn hint_ok(p: *mut u8, hint: Option<usize>) -> bool {
    forall|c: int| #[trigger] process_regions().is_slot(c, p@.addr) ==> hint == Some(c as usize)
}

/// The huge half of a sized free's check ([`hint_ok`] is the slot half):
/// for `p` a page start of the huge region and no slot start, with `blk`
/// the caller's token for it (the run's, [`Live::claims`]), `size`
/// rounded up to whole pages ([`huge_usable`]) is the run's usable size,
/// the run's `nallocx` for a size no class serves. Also the small size of
/// a run an alignment made huge, and any size `realloc` kept the run for.
/// `Shared::check_size_hint` aborts on a size that fails it.
///
/// [`huge_usable`]: crate::vspec::zero::huge_usable
pub open spec fn huge_hint_ok(blk: Live, p: *mut u8, size: usize) -> bool {
    blk.claims(p) && !slot_start(p) && process_regions().is_huge_block(p@.addr, 1) ==> {
        &&& size + live::page_size() - 1 <= usize::MAX
        &&& crate::vspec::zero::huge_usable(size as int) == blk.len()
    }
}

/// A misaligned pointer is no free target: every slot start and every
/// huge page start is a multiple of the quantum, so a [`free_target`] is
/// one too.
pub proof fn theorem_free_target_aligned(p: *mut u8)
    requires
        regions_ok(),
        free_target(p),
    ensures
        (p@.addr as int) % (crate::QUANTUM as int) == 0,
{
    if slot_start(p) {
        let c = choose|c: int| #[trigger] process_regions().is_slot(c, p@.addr);
        crate::vspec::class::axiom_class_table_wf(c);
        lemma_slot_fits_quantum(p, c, 0);
    } else {
        lemma_quantum_pow2();
        crate::meta::lemma_is_pow2_one();
        lemma_huge_fits(p, 0, 1, crate::QUANTUM);
    }
}

/// No block at address 0: with the process's regions well formed and
/// separated, no slot start, no huge page start, no block an allocation
/// of `size` bytes may return
/// ([`Regions::user_block`](crate::layout::Regions::user_block)) and no
/// free target is null. So a NULL result is never a block, and NULL is
/// never a [`free_target`].
pub proof fn theorem_no_block_at_zero(p: *mut u8, c: int, size: int)
    requires
        regions_ok(),
    ensures
        process_regions().is_slot(c, p@.addr) ==> p@.addr != 0,
        process_regions().is_huge_block(p@.addr, 1) ==> p@.addr != 0,
        process_regions().user_block(p@.addr, size) ==> p@.addr != 0,
        free_target(p) ==> p@.addr != 0,
{
    let r = process_regions();
    r.lemma_block_nonzero(c, p@.addr, 1, size);
    if slot_start(p) {
        let c2 = choose|c2: int| #[trigger] r.is_slot(c2, p@.addr);
        r.lemma_block_nonzero(c2, p@.addr, 1, size);
    }
}

/// A slot of class `c` serves `size` bytes at `align` when the class is at
/// least `size` bytes and its size a multiple of `align`.
pub proof fn lemma_slot_fits(p: *mut u8, c: int, size: int, align: usize)
    requires
        regions_ok(),
        process_regions().is_slot(c, p@.addr),
        size <= class_table()[c].size,
        crate::meta::is_pow2(align),
        (class_table()[c].size as int) % (align as int) == 0,
    ensures
        process_regions().user_block(p@.addr, size),
        (p@.addr as int) % (align as int) == 0,
{
    process_regions().lemma_slot_aligned(c, p@.addr, align);
}

/// A slot of class `c` serves `malloc(size)` when the class is at least
/// `size` bytes: aligned to the quantum, as every class size is a multiple
/// of it.
pub proof fn lemma_slot_fits_quantum(p: *mut u8, c: int, size: int)
    requires
        regions_ok(),
        process_regions().is_slot(c, p@.addr),
        size <= class_table()[c].size,
    ensures
        process_regions().user_block(p@.addr, size),
        (p@.addr as int) % (crate::QUANTUM as int) == 0,
{
    crate::vspec::class::axiom_class_table_wf(c);
    crate::layout::lemma_class_size_quantum(c);
    lemma_quantum_pow2();
    lemma_slot_fits(p, c, size, crate::QUANTUM);
}

/// The quantum is a power of two, at most 4 KiB.
pub proof fn lemma_quantum_pow2()
    ensures
        crate::meta::is_pow2(crate::QUANTUM),
        crate::QUANTUM <= 0x1000,
{
    vstd::arithmetic::power2::lemma2_to64();
    if crate::QUANTUM == 16 {
        assert(crate::QUANTUM as nat == vstd::arithmetic::power2::pow2(4));
    } else {
        assert(crate::QUANTUM as nat == vstd::arithmetic::power2::pow2(3));
    }
}

/// A huge run placed for `size` bytes at `a1` serves `size` bytes at any
/// power of two `a2` that divides `a1` or a page (4 KiB or more).
pub proof fn lemma_huge_fits(p: *mut u8, size: usize, a1: usize, a2: usize)
    requires
        regions_ok(),
        crate::layout::huge_block(p@.addr, size, a1),
        crate::meta::is_pow2(a1),
        crate::meta::is_pow2(a2),
        a2 <= a1 || a2 <= 0x1000,
    ensures
        process_regions().user_block(p@.addr, size as int),
        (p@.addr as int) % (a2 as int) == 0,
{
    let r = process_regions();
    let k1 = choose|k: nat| k < 64 && a1 as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
    let k2 = choose|k: nat| k < 64 && a2 as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
    vstd::arithmetic::power2::lemma_pow2_pos(k1);
    vstd::arithmetic::power2::lemma_pow2_pos(k2);
    vstd::arithmetic::div_mod::lemma_mod_self_0(a2 as int);
    if a2 <= a1 {
        crate::layout::lemma_pow2_divides_pow2(a2, a2 as int, k1);
        crate::layout::lemma_aligned_down(p@.addr as int, a1 as int, a2 as int);
    } else {
        // The page is at least 4 KiB and divides `MAX_SLAB`, which the
        // huge base is a multiple of.
        let lg = r.lg_page;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg as usize);
        let page = 1usize << lg;
        vstd::arithmetic::power2::lemma2_to64();
        vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
        if lg > 12 {
            vstd::arithmetic::power2::lemma_pow2_strictly_increases(12, lg as nat);
        }
        crate::layout::lemma_pow2_divides_pow2(a2, a2 as int, lg as nat);
        if lg < 22 {
            vstd::arithmetic::power2::lemma_pow2_strictly_increases(lg as nat, 22);
        }
        crate::meta::lemma_is_pow2_shl(lg as usize);
        vstd::arithmetic::div_mod::lemma_mod_self_0(page as int);
        crate::layout::lemma_pow2_divides_pow2(page, page as int, 22);
        assert(crate::layout::MAX_SLAB == 0x40_0000) by (bit_vector);
        crate::layout::lemma_aligned_down(r.huge_base as int, crate::layout::MAX_SLAB as int, page as int);
        let d = p@.addr - r.huge_base;
        crate::layout::lemma_mod_lin3(r.huge_base as int, 1, d, 0, 0, page as int);
        crate::layout::lemma_aligned_down(p@.addr as int, page as int, a2 as int);
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

/// Ghost: the permissions for the cache memory of one heap (its home
/// headers, the headers of its `others` nodes and their stacks), keyed by
/// address. `cls` and `owner` are ghost bookkeeping for what the caches
/// hold: the class each header caches, and for each stack entry the
/// header and index it is. `toks` holds the slot tokens of the cached
/// slots, by stack entry; `zero` the zero tokens of the headers' zero
/// prefixes; `handed` the token of the slot the last pop from inside a
/// zero prefix handed out; `released` the `(class, address)` pairs this
/// heap's flushes handed back; `blocks` the metadata claim of each stack,
/// by its base.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct CachePerms {
    pub tracked hdrs: Map<*mut CacheHeader, PointsTo<CacheHeader>>,
    pub tracked slots: Map<*mut *mut u8, PointsTo<*mut u8>>,
    pub ghost cls: Map<*mut CacheHeader, vstd::prelude::int>,
    pub ghost owner: Map<*mut *mut u8, (*mut CacheHeader, vstd::prelude::int)>,
    pub tracked toks: Map<*mut *mut u8, GhostSingleton<vstd::prelude::int>>,
    pub tracked zero: Map<*mut *mut u8, Zeroed>,
    pub tracked handed: Option<Zeroed>,
    pub ghost released: vstd::seq::Seq<(vstd::prelude::int, vstd::prelude::int)>,
    pub tracked blocks: Map<*mut *mut u8, crate::meta::MetaClaim>,
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
    /// `MALLOCX_TCACHE_NONE`). The thread caches are derived from their
    /// headers in [`Heap::stats`].
    uncached: ThreadStats,
    /// Caches of every other arena this heap has allocated from
    /// explicitly, and the [`FOREIGN`] node ([`ArenaCaches`]).
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
    /// `slot_randomize`'s xorshift state: 0 until the heap's first
    /// randomised claim seeds it from `getrandom`, so the
    /// default configuration makes no syscall per heap creation.
    rng: u64,
    /// This heap may still move its home onto the arena of a slot it
    /// frees ([`Heap::try_rehome`]): set for a new thread heap; cleared
    /// for an explicit cache, by a `thread.arena` write, by the one move,
    /// and once [`REHOME_TICKS`] slow-path entries have passed.
    rehome: bool,
    /// The arena of the last cross-arena free [`Heap::try_rehome`] looked
    /// at: the move needs two in a row into the same arena.
    rehome_from: u16,
    /// Caches of the home arena, one line per class; last, so the small
    /// fields above share two lines and `caches[c]` is `heap + 128 + 64 * c`.
    caches: [CacheHeader; NCLASSES],
    /// Ghost ([`CachePerms`], no bytes): the permissions for `caches`, the
    /// `others` nodes' headers and their stacks.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    perms: Tracked<CachePerms>,
    /// Ghost (no bytes): the `others` list, head first, as the nodes'
    /// `next` fields chain it.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    list: Ghost<vstd::seq::Seq<*mut ArenaCaches>>,
    /// Ghost (no bytes): the `others` nodes' keys, `keys[i]` the `arena`
    /// field of `list[i]`.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    keys: Ghost<vstd::seq::Seq<u16>>,
    /// Ghost (no bytes): the home headers, one per class, where creation
    /// wrote them.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    home_hdrs: Ghost<vstd::seq::Seq<*mut CacheHeader>>,
    /// Ghost (no bytes): the heap's ticket for its home arena while it is
    /// counted there.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    homed: Tracked<Option<Homed>>,
    /// Ghost (no bytes): what the heap keeps of each `others` node's block
    /// ([`others::NodeHead`]): the permissions to the `arena`, `pad` and
    /// `next` fields `other_caches` writes when it links the node in, the
    /// rest of the block, and its claim.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    heads: Tracked<Map<*mut ArenaCaches, others::NodeHead>>,
}

} // verus!

const _: () = assert!(core::mem::offset_of!(Heap, caches) == 128);

/// `slots[start..start + len]` as a slice, for the slice operations of
/// the refill, the flush and the run rebuild (claims, selections, sorts,
/// reversals).
///
/// # Safety
/// The entries are this heap's stack storage and nothing else reads or
/// writes them while the slice lives.
#[inline(always)]
#[allow(
    clippy::needless_lifetimes,
    reason = "the permission and the slice share the lifetime in the Verus signature"
)]
unsafe fn stack_mut<'a>(
    slots: *mut *mut u8,
    start: usize,
    len: usize,
    _perms: Tracked<&'a mut Map<*mut *mut u8, PointsTo<*mut u8>>>,
) -> &'a mut [*mut u8] {
    // SAFETY: guaranteed by the caller.
    unsafe { core::slice::from_raw_parts_mut(slots.add(start), len) }
}

/// `assert_eq!(*len, 0, ..)`, the abort of [`Heap::free_caches`] and
/// [`Heap::free_caches_in`] on a cache that still holds slots: `*len == 0`
/// after it returns.
#[inline(always)]
#[track_caller]
fn assert_flushed(len: &u32) {
    assert_eq!(*len, 0, "heap: freeing a cache that was not flushed");
}

/// `assert_eq!(*len, 0, ..)`, [`Heap::refill`]'s abort on a heap
/// inconsistency: `*len == 0` after it returns.
#[inline(always)]
#[track_caller]
fn assert_refill_empty(len: &u32) {
    assert_eq!(*len, 0, "heap: refill of a cache that is not empty");
}

/// `assert_eq!(*len, 0, ..)`, [`CacheHeader::rekey`]'s abort on a cache
/// that still holds slots: `*len == 0` after it returns.
#[inline(always)]
#[track_caller]
fn assert_rekey_flushed(len: &u32) {
    assert_eq!(*len, 0, "heap: re-keying a cache that was not flushed");
}

/// `(*h).bound = true` for a heap [`Heap::create`] has just made: a field
/// store through the raw heap pointer.
///
/// # Safety
/// `h` is a fresh heap, exclusively owned.
#[inline(always)]
unsafe fn mark_bound(h: *mut Heap, _s: Tracked<&mut FreshHeap>) {
    // SAFETY: guaranteed by the caller.
    unsafe { (*h).bound = true };
}

/// `(*h).rehome = true` for a thread heap [`Heap::create`] has just made
/// and marked bound ([`mark_bound`]): it may still move its home
/// ([`Heap::try_rehome`]).
///
/// # Safety
/// `h` is a fresh heap, exclusively owned.
#[inline(always)]
unsafe fn mark_rehome(h: *mut Heap, _s: Tracked<&mut FreshHeap>) {
    // SAFETY: guaranteed by the caller.
    unsafe { (*h).rehome = true };
}

/// [`Arenas::assign`](crate::arena::Arenas::assign) on the process's
/// arenas, with the new heap's ticket for the arena it picked.
#[inline(always)]
fn assign_home(shared: &Shared, _ord: Tracked<&mut LockOrder>) -> (u16, Tracked<Homed>) {
    let (arena, t, _) = shared.arenas.assign(Tracked::assume_new());
    (arena, t)
}

impl Heap {
    /// Create a heap in the metadata region for the calling thread, homed
    /// on the least-loaded automatic arena and counted there
    /// (`Arenas::assign`; `destroy` releases the binding). Null if the
    /// region is exhausted. Does not touch TLS or the thread key.
    pub fn create(shared: &Shared) -> *mut Heap {
        let (arena, ticket) = assign_home(shared, Tracked::assume_new());
        let (h, _, _) = Self::create_detached(shared, arena, Tracked::assume_new());
        if h.is_null() {
            shared.arenas.unbind(arena, ticket);
        } else {
            // SAFETY: fresh, exclusively owned.
            unsafe { mark_bound(h, Tracked::assume_new()) };
            // SAFETY: as above.
            unsafe { mark_rehome(h, Tracked::assume_new()) };
        }
        h
    }

    /// Create a heap homed on `arena` (an explicit cache, or a thread heap
    /// before it is registered). Null if the region is exhausted. With
    /// the heap, the permissions to its caches and the token for its plain
    /// fields (ghost).
    fn create_detached(
        shared: &Shared,
        arena: u16,
        _ord: Tracked<&mut LockOrder>,
    ) -> (*mut Heap, Tracked<CachePerms>, Tracked<Option<FreshHeap>>) {
        let block = shared.meta.alloc(Layout::new::<Heap>());
        let h = block as *mut Heap;
        if h.is_null() {
            return (h, Tracked::assume_new(), Tracked::assume_new());
        }
        // SAFETY: `h` is a fresh, exclusively owned metadata block of the
        // right size and alignment. Field by field: a whole `Heap`
        // temporary (3.4 KiB with the headers inline) would deepen the
        // stack of every thread's first allocation by one page, which
        // larson's thread churn turns into +1.7 MiB of RSS per 437
        // threads (thread stacks stay mapped; measured on arm64).
        unsafe {
            ptr::write(&raw mut (*h).shared, shared);
            ptr::write(&raw mut (*h).tid, tid());
            ptr::write(&raw mut (*h).arena, arena);
            ptr::write(&raw mut (*h).slow_ticks, 0);
            ptr::write(&raw mut (*h).last_gc, now_ns());
            ptr::write(&raw mut (*h).uncached, ThreadStats::default());
            ptr::write(&raw mut (*h).others, ptr::null_mut());
            ptr::write(&raw mut (*h).snapshot, ThreadStats::default());
            ptr::write(&raw mut (*h).tcache_enabled, true);
            ptr::write(&raw mut (*h).bound, false);
            ptr::write(&raw mut (*h).rng, 0);
            ptr::write(&raw mut (*h).rehome, false);
            ptr::write(&raw mut (*h).rehome_from, FOREIGN);
        }
        // SAFETY: as above; `caches` is 128 bytes in (the `const`
        // assertion after `Heap`).
        let caches = unsafe { block.add(128) } as *mut CacheHeader;
        for c in 0..NCLASSES {
            // SAFETY: `c < NCLASSES`, inside the block.
            let cache = unsafe { caches.add(c) };
            // SAFETY: as above.
            unsafe { ptr::write(cache, CacheHeader::EMPTY) };
        }
        (h, Tracked::assume_new(), Tracked::assume_new())
    }

    /// Flush every cache, free them and the heap itself back to the
    /// metadata allocator. `h` is dangling afterwards.
    ///
    /// # Safety
    /// `h` must come from [`Heap::create`] on `shared` and no longer be in
    /// use by any thread.
    pub unsafe fn destroy(h: *mut Heap, shared: &Shared, _ord: Tracked<&mut LockOrder>) {
        // SAFETY: the caller gives us exclusive ownership of the heap.
        let heap = unsafe { tcaches::heap_at(h) };
        // Unbound before the flush: the arena is idle from here on, so a
        // thread this one just started (larson's successor, which frees
        // what this one allocated) can be homed on it, or move there
        // ([`Heap::try_rehome`]), while the flush below runs. The heap is
        // not counted from then on: `bound` and `rehome` are cleared
        // (nothing reads them again but this, which keeps the `bound` it
        // read for the exit purge).
        let bound = heap.bound;
        if bound {
            shared
                .arenas
                .unbind(heap.arena, vstd::prelude::Tracked::assume_new());
            heap.bound = false;
            heap.rehome = false;
        }
        heap.flush_all(shared, Tracked::assume_new());
        // SAFETY: the home array (null).
        unsafe { heap.free_caches_in(shared, ptr::null_mut(), Tracked::assume_new()) };
        // `for o in Self::nodes(heap.others)`, spelled out over
        // `node_next`: as the iterator does, the node's `next` is read
        // before the node is freed. Each node leaves the list as it goes
        // (`others` is its successor).
        let mut o = heap.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap; the node is freed below, after its `next`.
            let next = unsafe { heap.node_next(o, Ghost::assume_new()) };
            // SAFETY: `o` is a live node of this heap's `others` list.
            unsafe { heap.free_caches_in(shared, o, Tracked::assume_new()) };
            // SAFETY: as above.
            let pad = unsafe { heap.node_pad(o, Ghost::assume_new()) };
            // SAFETY: the node lies `pad` lines into its block.
            let block = unsafe { (o as *mut u8).sub(64 * pad as usize) };
            shared.meta.free(
                block,
                node_block_layout(),
                vstd::prelude::Tracked::assume_new(),
            );
            heap.others = next;
            o = next;
        }
        heap.others = ptr::null_mut();
        if bound {
            let arena = heap.arena;
            // jemalloc's rule (`tcache_destroy`): the last thread homed on
            // an arena leaves, so nothing will refill its `Empty` slabs
            // soon; purge them now instead of letting them sit for the
            // decay window. The caches just flushed above are most of
            // them. Not with a background thread, which decays on its own
            // schedule (as jemalloc), and at most once per decay period
            // per arena (`Arena::take_exit_purge`: thread churn would
            // purge and refault the same slabs at every exit); and not
            // when another heap has been homed there since the unbind
            // above. Design 13.3.
            if shared.arenas.bound(arena) == 0
                && !crate::purge::background::present()
                && let Some(a) = shared.arenas.get_existing(arena)
                && a.take_exit_purge(now_ns())
            {
                crate::purge::purge_arena(
                    shared,
                    arena,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                );
            }
        }
        shared.meta.free(
            h as *mut u8,
            Layout::new::<Heap>(),
            vstd::prelude::Tracked::assume_new(),
        );
    }

    /// Reset every (flushed) header of one array to [`CacheHeader::EMPTY`]
    /// and free its stack, through a plain reference to the array (the
    /// tests' teardown of a bare cache array). The heap's own arrays go
    /// through [`Heap::free_caches_in`].
    #[cfg(test)]
    fn free_caches(
        shared: &Shared,
        caches: &mut [CacheHeader; NCLASSES],
        _stacks: Tracked<
            Map<vstd::prelude::int, (vstd::raw_ptr::PointsToRaw, crate::meta::MetaClaim)>,
        >,
    ) {
        for cache in caches.iter_mut() {
            if cache.cap != 0 {
                assert_flushed(&cache.len);
                shared.meta.free(
                    cache.slots as *mut u8,
                    CacheHeader::slots_layout(cache.cap),
                    vstd::prelude::Tracked::assume_new(),
                );
            }
            *cache = CacheHeader::EMPTY;
        }
    }

    /// [`Heap::free_caches`] header by header, for the home array (`node`
    /// null) or `node`'s: reset every (flushed) header to
    /// [`CacheHeader::EMPTY`] and free its stack.
    ///
    /// Unsafe for its caller's precondition; a later proof PR can make it
    /// a safe `fn` again once a Verus `requires` carries that precondition.
    ///
    /// # Safety
    /// `node` is null or a live node owned by this heap.
    unsafe fn free_caches_in(
        &mut self,
        shared: &Shared,
        node: *mut ArenaCaches,
        _ord: Tracked<&mut LockOrder>,
    ) {
        for class in 0..NCLASSES {
            // SAFETY: `node` is null or a live node owned by this heap.
            let cache = unsafe { self.header_in(node, class) };
            // SAFETY: `header_in` returned one of this heap's headers.
            let h = unsafe { self.hdr_mut(cache) };
            if h.cap != 0 {
                assert_flushed(&h.len);
                shared.meta.free(
                    h.slots as *mut u8,
                    CacheHeader::slots_layout(h.cap),
                    vstd::prelude::Tracked::assume_new(),
                );
            }
            *h = CacheHeader::EMPTY;
        }
    }

    /// Trade the values of owned headers `a` and `b` of one class
    /// (`thread.arena`'s exchange of the home caches with a node's,
    /// [`Heap::set_arena`]); each stack goes with its header.
    ///
    /// Unsafe for its caller's precondition; a later proof PR can make it
    /// a safe `fn` again once a Verus `requires` carries that precondition.
    ///
    /// # Safety
    /// `a` and `b` are distinct headers this heap owns.
    unsafe fn swap_hdrs(&mut self, a: *mut CacheHeader, b: *mut CacheHeader) {
        // SAFETY: guaranteed by the caller.
        unsafe { ptr::swap(a, b) };
    }

    /// `&*cache` for an owned header (a `(*cache).field` load).
    ///
    /// Unsafe for its caller's precondition; a later proof PR can make it
    /// a safe `fn` again once a Verus `requires` carries that precondition.
    ///
    /// # Safety
    /// `cache` is a header this heap owns (a home header or one of a live
    /// `others` node's), and nothing writes it while the reference lives.
    #[inline(always)]
    unsafe fn hdr_ref(&self, cache: *mut CacheHeader) -> &CacheHeader {
        // SAFETY: guaranteed by the caller.
        unsafe { &*cache }
    }

    /// `&mut *cache` for an owned header (`(*cache).field` stores).
    ///
    /// Unsafe for its caller's precondition; a later proof PR can make it
    /// a safe `fn` again once a Verus `requires` carries that precondition.
    ///
    /// # Safety
    /// `cache` is a header this heap owns (a home header or one of a live
    /// `others` node's), and nothing else reads or writes it while the
    /// reference lives.
    #[inline(always)]
    unsafe fn hdr_mut(&mut self, cache: *mut CacheHeader) -> &mut CacheHeader {
        // SAFETY: guaranteed by the caller.
        unsafe { &mut *cache }
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
    pub fn set_arena(&mut self, shared: &Shared, arena: u16, _ord: Tracked<&mut LockOrder>) {
        // An explicit home is kept, even one equal to the current home:
        // no late move away from it ([`Heap::try_rehome`]).
        self.rehome = false;
        if arena == self.arena {
            return;
        }
        let old = self.arena;
        let node = self.other_caches(shared, arena, true);
        if node.is_null() {
            // No metadata for a new node: the caches of the new home start
            // empty and the old home's must go back first.
            self.drop_home_caches(shared, Tracked::assume_new());
        } else {
            // The node's headers and the home headers trade places, and
            // the node becomes the cache of `old`, the home arena being
            // left.
            for c in 0..NCLASSES {
                let a = self.cache_ptr(c);
                // SAFETY: `node` is a live block owned by this heap.
                let b = unsafe { Self::node_cache_ptr(node, c) };
                // SAFETY: a home header and a header of the node, both
                // this heap's, so distinct.
                unsafe { self.swap_hdrs(a, b) };
            }
            // SAFETY: as above.
            unsafe { node_set_arena(node, old, Tracked::assume_new()) };
        }
        self.arena = arena;
        if self.bound {
            shared
                .arenas
                .unbind(old, vstd::prelude::Tracked::assume_new());
            let _ticket = shared.arenas.bind(arena);
        }
    }

    /// The `FOREIGN` slow path of a young heap's cross-arena `free`
    /// ([`Heap::rehome`] set): the first free of each class, while the
    /// class has no `FOREIGN` cache yet, and the flush of a full one. The
    /// heap moves onto the slot's arena when [`Heap::try_rehome`] allows
    /// it, and the slot then takes the home push; otherwise (false) the
    /// caller goes on to the ordinary `FOREIGN` path. Here rather than in
    /// [`Heap::free_other`] so that the cross-arena fast push (every free
    /// of xmalloc-test's consumers) carries no test for it.
    ///
    /// The move flushes the `FOREIGN` caches, and with them a first copy
    /// of `p` if this free is a double free: before the move the
    /// `double_free_scan` compare runs against the `FOREIGN` cache (the
    /// one `free_slow_in` would have run), and after it the exact bit test
    /// runs again on `p`, so a slot the flush just released aborts as
    /// `invalid or double free` instead of being pushed into a cache that
    /// `malloc` pops (which would hand it out twice).
    #[cold]
    #[inline(never)]
    fn rehome_on_free(&mut self, shared: &Shared, class: usize, p: *mut u8) -> bool {
        let to = shared.arena_of_slot(class, p);
        if shared.double_free_scan() {
            let foreign = self.other_caches(shared, FOREIGN, false);
            if !foreign.is_null() {
                // SAFETY: `foreign` is this heap's live `FOREIGN` node; a
                // header with `cap != 0` is a real cache.
                unsafe {
                    let cache = Self::node_cache_ptr(foreign, class);
                    if self.hdr_ref(cache).cap != 0 {
                        Self::scan_or_abort(shared, cache, p);
                    }
                }
            }
        }
        if !self.try_rehome(shared, to) {
            return false;
        }
        // The flush released every cached slot: test `p`'s bit again.
        match shared.classify_free(p, Tracked::assume_new()) {
            Ok((c, a)) => debug_assert!(c == class && a == to),
            Err(_) => invalid_free(p as usize),
        }
        self.push(shared, class, p);
        true
    }

    /// Move the home of a young thread heap onto `to`, the arena owning a
    /// slot it is freeing, if no heap is homed there (design 6.5). Called
    /// from the cross-arena `free` ([`Heap::rehome_on_free`]) while
    /// [`Heap::rehome`] is set, which this clears once the heap has made
    /// [`REHOME_TICKS`] slow-path entries; true when the heap moved.
    ///
    /// Why: a thread that takes over a dead thread's data was homed when
    /// it started, while that thread still held its arena, so it was put
    /// on another one, and every `free` of the inherited data took the
    /// foreign cache while its own allocations came from fresh refills of
    /// its own arena (larson: each successor alternated between two
    /// arenas, `narenas:1` was 31% faster at 10 rounds per thread on the
    /// C4A). Most successors now create their heap after the exiting
    /// thread's unbind ([`Heap::destroy`] unbinds before its flush) and
    /// are homed on its arena directly; one started earlier moves there
    /// here once that arena is idle, and the inherited slots feed its
    /// home caches again.
    ///
    /// The move flushes the home caches and the `FOREIGN` caches through
    /// the ordinary flush (every cached slot goes back to its slab, its bit
    /// cleared, so a stale free of one aborts as before; nothing stays
    /// cached across the move), keeps the stacks and re-keys the home
    /// headers to `to`. Only an automatic arena, only one with no heap
    /// homed on it (`Arenas::try_bind_idle`, one compare-and-swap, so two
    /// heaps never both take it), never one this heap already keeps an
    /// `others` node for, and at most once per heap.
    ///
    /// Only while the heap is young by its slow-path count: a thread that
    /// has built data of its own at home and then frees other threads'
    /// data (mstress's workers take transfer slots from every arena) would
    /// strand its own data on the arena it leaves and send every later
    /// free of it to the `FOREIGN` cache. Moving on the first free from
    /// any idle arena cost mstress 6% (700 of its 775 workers moved, after
    /// about 140 slow-path entries each); under the limit 46 still moved
    /// (+2.3%). Hence the second condition: two cross-arena frees in a
    /// row seen here went to the same arena (an inherited data set comes
    /// from one arena; mstress's transfer slots from all of them).
    fn try_rehome(&mut self, shared: &Shared, to: u16) -> bool {
        debug_assert!(self.bound);
        if self.slow_ticks >= REHOME_TICKS {
            self.rehome = false;
            return false;
        }
        if to != self.rehome_from {
            self.rehome_from = to;
            return false;
        }
        if to as u32 >= shared.arenas.auto()
            || to == self.arena
            || shared.arenas.bound(to) != 0
            || !self.other_caches(shared, to, false).is_null()
            || !shared.arenas.try_bind_idle(to).0
        {
            return false;
        }
        let old = self.arena;
        self.flush_arena(shared, old, ptr::null_mut(), Tracked::assume_new());
        let foreign = self.other_caches(shared, FOREIGN, false);
        if !foreign.is_null() {
            self.flush_arena(shared, FOREIGN, foreign, Tracked::assume_new());
        }
        let a = shared.owner(to);
        for class in 0..NCLASSES {
            let c = self.cache_ptr(class);
            // SAFETY: `cache_ptr` is this heap's home header for `class`.
            if unsafe { self.hdr_ref(c) }.cap != 0 {
                let refilled = a.0.class(class).refilled() as u32;
                // SAFETY: as above.
                let h = unsafe { self.hdr_mut(c) };
                h.rekey(to, refilled);
            }
        }
        self.arena = to;
        self.rehome = false;
        shared
            .arenas
            .unbind(old, vstd::prelude::Tracked::assume_new());
        true
    }

    /// Flush the home caches, fold their byte counters into `uncached` and
    /// free them: the home array is `EMPTY` afterwards and `stats()` is
    /// unchanged. For a `thread.arena` change that could get no `others`
    /// node for the old home.
    fn drop_home_caches(&mut self, shared: &Shared, _ord: Tracked<&mut LockOrder>) {
        let home = self.arena;
        self.flush_arena(shared, home, ptr::null_mut(), Tracked::assume_new());
        Self::fold_counters(&mut self.uncached, &self.caches);
        // SAFETY: the home array (null).
        unsafe { self.free_caches_in(shared, ptr::null_mut(), Tracked::assume_new()) };
    }

    /// Add the byte counters a cache array stands for (`allocs` pops and
    /// `len + allocs - moved` pushes per cache, each worth the class size)
    /// to `into`. [`Heap::stats`] derives the thread counters this way on
    /// every read; a cache that is about to be freed is folded into
    /// `uncached` first so that `thread.allocated` never goes backwards.
    #[allow(
        clippy::while_let_loop,
        reason = "the desugared `for`, which Verus needs for `continue`"
    )]
    fn fold_counters(into: &mut ThreadStats, caches: &[CacheHeader; NCLASSES]) {
        // `for (class, c) in caches.iter().enumerate()`, spelled out: Verus
        // has no `Enumerate` and rejects `continue` in a `for` loop. The
        // count is taken and bumped as `Enumerate::next` does.
        let mut iter = caches.iter();
        let mut count: usize = 0;
        loop {
            let c = match iter.next() {
                Some(c) => c,
                None => break,
            };
            let class = count;
            count += 1;
            if c.cap == 0 {
                continue;
            }
            let frees = (c.len as u64).wrapping_add(c.allocs).wrapping_sub(c.moved);
            into.add(CLASSES.table()[class].size() as u64, c.allocs, frees);
        }
    }

    /// Byte counters for this thread: the uncached part plus what every
    /// cache of every arena stands for ([`Heap::fold_counters`]).
    pub fn stats(&self) -> ThreadStats {
        let mut s = self.uncached;
        Self::fold_counters(&mut s, &self.caches);
        // `for o in Self::nodes(self.others)`, spelled out: Verus has no
        // `from_fn`.
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            let next = unsafe { self.node_next(o, Ghost::assume_new()) };
            // SAFETY: as above.
            unsafe { Self::fold_counters(&mut s, self.node_caches(o, Ghost::assume_new())) };
            o = next;
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
    pub fn set_tcache_enabled(&mut self, shared: &Shared, on: bool, _ord: Tracked<&mut LockOrder>) {
        self.tcache_enabled = on;
        if on {
            return;
        }
        self.flush_all(shared, Tracked::assume_new());
        Self::fold_counters(&mut self.uncached, &self.caches);
        // SAFETY: the home array (null).
        unsafe { self.free_caches_in(shared, ptr::null_mut(), Tracked::assume_new()) };
        // The walk of `flush_all`. Folding and freeing a node's headers
        // leaves the list as it is.
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            let next = unsafe { self.node_next(o, Ghost::assume_new()) };
            // SAFETY: as above; the fold only reads the headers.
            unsafe {
                Self::fold_counters(
                    &mut self.uncached,
                    node_caches_detached(
                        o,
                        Tracked::assume_new(),
                        Ghost::assume_new(),
                        Ghost::assume_new(),
                    ),
                )
            };
            // SAFETY: `o` is a live node of this heap's `others` list.
            unsafe { self.free_caches_in(shared, o, Tracked::assume_new()) };
            o = next;
        }
    }

    /// Slow-path entries so far (the periodic tick's counter).
    #[cfg(test)]
    pub fn slow_ticks(&self) -> u32 {
        self.slow_ticks
    }

    /// The home arena's cache for `class`, or `None` while it has never
    /// been used.
    #[cfg(test)]
    pub fn cache(&self, class: usize) -> Option<&CacheHeader> {
        let c = &self.caches[class];
        if c.cap == 0 { None } else { Some(c) }
    }

    /// The cache for `class` in `arena`, home or not, or `None` while it
    /// has never been used (or the arena was never touched).
    #[cfg(test)]
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

    /// Flush every cache of one array (the home array when `node` is null,
    /// else the node's, whose arena is `arena`) completely.
    fn flush_arena(
        &mut self,
        shared: &Shared,
        arena: u16,
        node: *mut ArenaCaches,
        _ord: Tracked<&mut LockOrder>,
    ) {
        for class in 0..NCLASSES {
            // SAFETY: `node` is null or a live node owned by this heap; an
            // unused header has `len == 0` and is skipped.
            unsafe {
                let cache = self.header_in(node, class);
                let len = self.hdr_ref(cache).len as usize;
                if len != 0 {
                    self.flush(shared, arena, cache, class, len, Tracked::assume_new());
                }
            }
        }
    }

    /// Return every cached slot of this heap to its slab. Caches stay
    /// allocated.
    pub fn flush_all(&mut self, shared: &Shared, _ord: Tracked<&mut LockOrder>) {
        let home = self.arena;
        self.flush_arena(shared, home, ptr::null_mut(), Tracked::assume_new());
        // `for o in Self::nodes(self.others)`, spelled out: Verus has no
        // `from_fn`. As the iterator does, the node's `next` is read before
        // its turn.
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            let next = unsafe { self.node_next(o, Ghost::assume_new()) };
            // SAFETY: as above.
            let arena = unsafe { self.node_arena(o, Ghost::assume_new()) };
            self.flush_arena(shared, arena, o, Tracked::assume_new());
            o = next;
        }
    }

    /// Cache GC (design 6.4): for every cache, release three quarters of
    /// the entries that went unused since the last GC, then restart the
    /// low-water tracking from the current length.
    pub fn gc(&mut self, shared: &Shared, _ord: Tracked<&mut LockOrder>) {
        let home = self.arena;
        self.gc_arena(shared, home, ptr::null_mut(), Tracked::assume_new());
        // The walk of `flush_all`.
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: as in `flush_all`.
            let next = unsafe { self.node_next(o, Ghost::assume_new()) };
            // SAFETY: as above.
            let arena = unsafe { self.node_arena(o, Ghost::assume_new()) };
            self.gc_arena(shared, arena, o, Tracked::assume_new());
            o = next;
        }
    }

    /// One GC step over one array (the home array when `node` is null,
    /// else the node's, whose arena is `arena`).
    #[allow(
        clippy::while_let_loop,
        reason = "the desugared `for`, which Verus needs for `continue`"
    )]
    fn gc_arena(
        &mut self,
        shared: &Shared,
        arena: u16,
        node: *mut ArenaCaches,
        _ord: Tracked<&mut LockOrder>,
    ) {
        // `for class in 0..NCLASSES`, spelled out: Verus rejects `continue`
        // in a `for` loop.
        let mut iter = 0..NCLASSES;
        loop {
            let class = match iter.next() {
                Some(c) => c,
                None => break,
            };
            // SAFETY: `node` is null or a live node owned by this heap; an
            // unused header (`cap == 0`) is skipped.
            unsafe {
                let cache = self.header_in(node, class);
                if self.hdr_ref(cache).cap == 0 {
                    continue;
                }
                let lw = self.hdr_ref(cache).low_water as usize;
                if arena == FOREIGN {
                    // Never popped: everything it holds went unused.
                    let len = self.hdr_ref(cache).len as usize;
                    if len > 0 {
                        self.flush(shared, arena, cache, class, len, Tracked::assume_new());
                    }
                } else if lw > 0 {
                    self.flush(
                        shared,
                        arena,
                        cache,
                        class,
                        lw - lw / 4,
                        Tracked::assume_new(),
                    );
                }
                let h = self.hdr_mut(cache);
                h.low_water = h.len;
            }
        }
    }

    /// Count a slow-path entry and run periodic work every
    /// [`PERIODIC_EVERY`] of them: cache GC when the interval has elapsed,
    /// then decay (or starting the background thread that will do it,
    /// `purge::on_tick`). Nothing else: thread statistics are merged by
    /// `epoch` (`stats.rs`), not here. `ALLOC` is whether the caller is
    /// an allocation: only an allocation's tick may create the thread
    /// (design 8.1), because glibc calls `free` with locks held that
    /// `pthread_create` and `pthread_join` take again (the stack cache
    /// lock, from `pthread_join`'s TLS teardown).
    // Hot slow-path tick: left as written so its codegen does not move.
    #[allow(clippy::manual_is_multiple_of)]
    fn tick<const ALLOC: bool>(&mut self, shared: &Shared) {
        self.slow_ticks = self.slow_ticks.wrapping_add(1);
        if self.slow_ticks % PERIODIC_EVERY == 0 {
            let now = now_ns();
            if now.wrapping_sub(self.last_gc) >= shared.gc_interval() {
                self.last_gc = now;
                self.gc(shared, Tracked::assume_new());
            }
            crate::purge::on_tick(shared, now, ALLOC, vstd::prelude::Tracked::assume_new());
        }
    }
}
