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
pub mod mallocx;
mod others;
mod paths;
mod shared;
mod tcaches;
mod thread;

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
pub use shared::Shared;
#[cfg(test)]
use shared::{GuardTest, guard_interval_for};
pub use thread::{TLS_CREATING, TLS_EXITED, fast_flags};
pub use thread::{existing_heap, heap_destructor, heap_for_thread, tls_heap_slot};

use core::alloc::Layout;
use core::ptr;

use crate::layout::{CLASSES, NCLASSES};

use others::node_block_layout;
use paths::invalid_free;
#[cfg(test)]
use shared::ncpus;
use shared::{now_ns, tid};

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
/// Slow-path entries between two periodic-work checks (design 6.4).
pub const PERIODIC_EVERY: u32 = 64;

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
/// Automatic arenas whose exited thread heaps are parked for the next
/// thread homed there (`Heap::park`): the default `narenas` is at most 64.
pub const PARK_SLOTS: usize = 64;
/// How long a parked heap waits for the next thread on its arena before
/// it is destroyed (`Shared::expire_parked`): 100 ms. Its cached slots
/// keep their slot bits set while it waits, so a stale `free` of one is
/// not caught by the bit test until then (design 6.5); a program that
/// replaces threads faster than this keeps the takeover.
pub const PARK_TTL_NS: u64 = 100_000_000;
/// `Shared::free_flags` bit, never in `push_flags`: more than one arena
/// exists, so plain `free` compares the slot's owner word with the heap's
/// home arena before pushing.
pub const FREE_ROUTE: u8 = 8;

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
    /// Caches of the home arena, one line per class; last, so the small
    /// fields above share two lines and `caches[c]` is `heap + 128 + 64 * c`.
    caches: [CacheHeader; NCLASSES],
}

const _: () = assert!(core::mem::offset_of!(Heap, caches) == 128);

impl Heap {
    /// Create a heap in the metadata region for the calling thread, homed
    /// on the least-loaded automatic arena and counted there
    /// (`Arenas::assign`; `destroy` releases the binding). The heap an
    /// exited thread parked on that arena ([`Heap::park`]) is taken over
    /// with its caches when there is one. Null if the region is exhausted.
    /// Does not touch TLS or the thread key.
    pub fn create(shared: &Shared) -> *mut Heap {
        let arena = shared.arenas.assign();
        let parked = shared.take_parked(arena);
        if !parked.is_null() {
            // SAFETY: a parked heap belongs to no thread; `take_parked`
            // handed it to this one alone.
            unsafe {
                if !Shared::parked_too_long(parked, now_ns()) {
                    (*parked).adopt();
                    return parked;
                }
                Heap::destroy(parked, shared);
            }
        }
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
        heap.drop_others(shared);
        heap.release_binding(shared, true);
        shared.meta.free(h as *mut u8, Layout::new::<Heap>());
    }

    /// Thread exit (design 6.5): park the heap ([`Heap::park`]) only while
    /// the background thread runs, the one thing that expires a parked heap
    /// after [`PARK_TTL_NS`] whatever the program does; otherwise flush and
    /// free it ([`Heap::destroy`]), so a stale `free` of a slot it had
    /// cached is caught by the bit test at once. A park that races with the
    /// background thread stopping is undone here (the stop drains the slots
    /// it sees, `background::main`): the `SeqCst` park and recheck against
    /// the `SeqCst` clear and drain mean one side sees the other.
    ///
    /// # Safety
    /// As for [`Heap::destroy`].
    pub unsafe fn retire(h: *mut Heap, shared: &Shared) {
        use crate::purge::background;
        // SAFETY: the caller gives us exclusive ownership of the heap.
        let (arena, was_bound) = unsafe { ((*h).arena, (*h).bound) };
        if !background::present_fenced() {
            // SAFETY: as above.
            unsafe { Heap::destroy(h, shared) };
            return;
        }
        // SAFETY: as above.
        if !unsafe { Heap::park(h, shared) } {
            // A refused park may have released the binding already, with
            // no exit purge (`release_binding(.., false)`): do it here.
            // SAFETY: on `false` the heap is still ours.
            let released = was_bound && !unsafe { (*h).bound };
            // SAFETY: as above.
            unsafe { Heap::destroy(h, shared) };
            if released {
                Self::exit_purge(shared, arena);
            }
            return;
        }
        if background::present_fenced() {
            return;
        }
        // The background thread stopped while this heap was parking, and
        // its drain may have missed it: take back whatever the slot holds.
        let g = shared.take_parked(arena);
        if !g.is_null() {
            // SAFETY: the swap took it out of the slot; ours alone.
            unsafe { Heap::destroy(g, shared) };
            Self::exit_purge(shared, arena);
        }
    }

    /// Thread exit (design 6.5): hand the heap of an exiting thread, with
    /// its home caches as they are, to the next thread homed on the same
    /// automatic arena ([`Heap::create`]) instead of flushing them to the
    /// slabs for that thread to refill. The caches of other arenas (the
    /// `others` nodes, [`FOREIGN`] among them) are flushed and freed and
    /// the arena binding is released here, as [`Heap::destroy`] does. One
    /// heap per arena waits (`Shared::park`); `false` when the arena's
    /// slot is taken or the heap is not a bound thread heap with caches,
    /// and the caller destroys it. A heap that has waited
    /// [`PARK_TTL_NS`] is destroyed instead of taken over, by the next
    /// decay pass or the next thread on its arena
    /// (`Shared::expire_parked`), so what an exited thread left cached is
    /// back in its slabs (its slot bits clear, its slabs free to go
    /// `Empty` and decay) within that time; `malloc_trim` and the full
    /// purge destroy the heaps that wait at once (`Shared::drain_parked`).
    ///
    /// # Safety
    /// As for [`Heap::destroy`]; on `true` the heap is owned by the slot.
    pub unsafe fn park(h: *mut Heap, shared: &Shared) -> bool {
        // SAFETY: the caller gives us exclusive ownership of the heap.
        let heap = unsafe { &mut *h };
        if !heap.bound || !heap.tcache_enabled || !shared.can_park(heap.arena) {
            return false;
        }
        heap.flush_others(shared);
        heap.drop_others(shared);
        heap.release_binding(shared, false);
        // The park time; the next owner restarts its GC interval anyway.
        heap.last_gc = now_ns();
        if !shared.park(heap.arena, h) {
            return false;
        }
        // A background thread asleep without a deadline must learn the
        // parked heap's (`Shared::expire_parked`).
        crate::purge::note_empty();
        true
    }

    /// Take over a parked heap for the calling thread: its binding was
    /// released when it was parked and `Arenas::assign` has just counted
    /// the new thread on the same arena. The thread counters restart from
    /// zero over the caches as they stand (`thread.allocated` is per
    /// thread), so they are rebased rather than reset.
    fn adopt(&mut self) {
        let mut held = ThreadStats::default();
        Self::fold_counters(&mut held, &self.caches);
        self.uncached = ThreadStats {
            allocated: 0u64.wrapping_sub(held.allocated),
            deallocated: 0u64.wrapping_sub(held.deallocated),
        };
        self.snapshot = ThreadStats::default();
        self.tid = tid();
        self.last_gc = now_ns();
        // Reseed `slot_randomize` from `getrandom` at the next randomised
        // claim, as a fresh heap does: a parked heap is copied into every
        // forked child, which would otherwise all lay out alike.
        self.rng = 0;
        self.bound = true;
    }

    /// Flush the caches of every arena but the home one.
    fn flush_others(&mut self, shared: &Shared) {
        for o in Self::nodes(self.others) {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            let arena = unsafe { (*o).arena };
            self.flush_arena(shared, arena, o);
        }
    }

    /// Free the (flushed) `others` nodes and their stacks.
    fn drop_others(&mut self, shared: &Shared) {
        for o in Self::nodes(self.others) {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap; the iterator has read `next` already, so the
            // block can go.
            unsafe {
                Self::free_caches(shared, &mut (*o).caches);
                let block = (o as *mut u8).sub(64 * (*o).pad as usize);
                shared.meta.free(block, node_block_layout());
            }
        }
        self.others = ptr::null_mut();
    }

    /// Release a thread heap's arena binding (nothing for an explicit cache
    /// or a heap already released), purging the arena's `Empty` slabs when
    /// it was the last heap homed there and `purge` is set (not for a heap
    /// being parked: it keeps its home caches, so its slabs are not
    /// `Empty`; [`Shared::expire_parked`] purges when it destroys it).
    fn release_binding(&mut self, shared: &Shared, purge: bool) {
        if self.bound {
            self.bound = false;
            let arena = self.arena;
            shared.arenas.unbind(arena);
            if purge {
                Self::exit_purge(shared, arena);
            }
        }
    }

    /// jemalloc's rule (`tcache_destroy`): the last thread homed on an
    /// arena has left, so nothing will refill its `Empty` slabs soon;
    /// purge them now instead of letting them sit for the decay window.
    /// The caches just flushed are most of them. Not with a background
    /// thread, which decays on its own schedule (as jemalloc), and at most
    /// once per decay period per arena (`Arena::take_exit_purge`: thread
    /// churn would purge and refault the same slabs at every exit).
    /// Design 13.3.
    fn exit_purge(shared: &Shared, arena: u16) {
        if shared.arenas.bound(arena) == 0 && !crate::purge::background::present() {
            if let Some(a) = shared.arenas.get_existing(arena) {
                if a.take_exit_purge(now_ns()) {
                    crate::purge::purge_arena(shared, arena);
                }
            }
        }
    }

    /// Reset every (flushed) header of one array to [`CacheHeader::EMPTY`]
    /// and free its stack.
    fn free_caches(shared: &Shared, caches: &mut [CacheHeader; NCLASSES]) {
        for cache in caches.iter_mut() {
            if cache.cap != 0 {
                assert_eq!(cache.len, 0, "heap: freeing a cache that was not flushed");
                shared
                    .meta
                    .free(cache.slots as *mut u8, CacheHeader::slots_layout(cache.cap));
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
        if on {
            return;
        }
        self.flush_all(shared);
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

    /// Return every cached slot of this heap to its slab. Caches stay
    /// allocated.
    pub fn flush_all(&mut self, shared: &Shared) {
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
        let home = self.arena;
        self.gc_arena(shared, home, ptr::null_mut());
        for o in Self::nodes(self.others) {
            // SAFETY: as in `flush_all`.
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
                if arena == FOREIGN {
                    // Never popped: everything it holds went unused.
                    let len = (*cache).len as usize;
                    if len > 0 {
                        self.flush(shared, arena, cache, class, len);
                    }
                } else if lw > 0 {
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
            let now = now_ns();
            if now.wrapping_sub(self.last_gc) >= shared.gc_interval() {
                self.last_gc = now;
                self.gc(shared);
            }
            crate::purge::on_tick(shared, now);
        }
    }
}
