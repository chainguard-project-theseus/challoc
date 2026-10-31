//! The allocation and free paths of a heap: the inlined fast paths the
//! entry points in `api/` are built from, the cold slow paths behind them,
//! and the crate-facing functions that find the calling thread's heap.

use core::ptr;
use vstd::map::Map;
use vstd::prelude::verus;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, int};
use vstd::raw_ptr::ptr_mut_ref;
use vstd::resource::set::GhostSingleton;

use crate::layout::{CLASSES, NCLASSES};
use crate::sys::pac;
use crate::sys::tls::TlsSlot;
use crate::vspec::addr::addr_to_ptr;
use crate::vspec::errno::ThreadErrno;
use crate::vspec::zero::Zeroed;

use super::thread::heap_from_slot;
use super::{
    CacheHeader, FOREIGN, FREE_ROUTE, Heap, Live, PUSH_PAC, Shared, existing_heap, heap_for_thread,
};
#[cfg(verus_keep_ghost)]
use super::{
    CachePerms, PUSH_ZERO, cell_ok, free_target, hand_out, hint_ok, lemma_handed_ok, lemma_hint_ok,
    lemma_plain_stored, pac_mode, regions_ok, slot_ptr, slot_start, slot_token,
};
#[cfg(verus_keep_ghost)]
use crate::layout::{class_table, process_regions};
#[cfg(verus_keep_ghost)]
use crate::lock::axiom_entry;

impl Heap {
    /// `malloc` of a non-huge class in the home arena: the cache pop, then
    /// the slow path.
    #[inline(always)]
    pub fn alloc(&mut self, shared: &Shared, class: usize, _blk: Tracked<&mut Live>) -> *mut u8 {
        match self.alloc_fast(class, shared.push_flags(), Tracked::assume_new()) {
            Some(p) => p,
            None => {
                self.alloc_slow(shared, class, Tracked::assume_new(), Tracked::assume_new())
                    .0
            }
        }
    }
}

verus! {

impl Heap {
    /// The cache pop alone: `None` when the cache is empty (or still the
    /// sentinel) and the caller must take a slow path. `flags` is
    /// `Shared::push_flags` (only [`PUSH_PAC`] matters to a pop). `blk`
    /// gets the popped slot's token.
    #[inline(always)]
    // The `match` stands for `Option::map`, which Verus does not specify.
    #[allow(clippy::manual_map)]
    pub fn alloc_fast(
        &mut self,
        class: usize,
        flags: u8,
        Tracked(blk): Tracked<&mut Live>,
    ) -> (r: Option<*mut u8>)
        requires
            class < NCLASSES,
            old(self).wf(),
            (flags & PUSH_PAC != 0) ==> pac::pac_available(),
            (flags & PUSH_PAC != 0) ==> crate::sys::may_abort_corrupt(),
        ensures
            final(self).keeps(*old(self)),
            r matches Some(p) ==> ((flags & PUSH_PAC != 0) == pac_mode() ==> process_regions().is_slot(class as int, p@.addr)),
            r matches Some(p) ==> ((flags & PUSH_PAC != 0) == pac_mode() ==> final(blk).slot_block(p, class as int)),
        opens_invariants none
    {
        let tracked mut out = Map::<int, GhostSingleton<int>>::tracked_empty();
        let r = match self.pop_fast(class, flags, Tracked(&mut out)) {
            Some((p, _)) => Some(p),
            None => None,
        };
        proof {
            if let Some(p) = r {
                if (flags & PUSH_PAC != 0) == pac_mode() {
                    hand_out(&mut out, blk, class as int, p);
                }
            }
        }
        r
    }

    /// [`Heap::alloc_fast`] with the zero flag: the one body of the home
    /// cache's "load `len`, zero means slow, else pop", shared by `malloc`
    /// (`alloc_fast`) and `calloc` ([`Heap::alloc_zeroed`]).
    ///
    /// The scheduled pop (design 6.4, every `compact_mask + 1` pops) is
    /// declined here (`None`, `allocs` not yet counted) and served by
    /// [`Heap::alloc_slow`], the one place every caller's `None` lands:
    /// `pop_compact` would be the one call inside the inlined pop, and a
    /// call in the body costs `malloc` a frame it needs for nothing else
    /// (the same move as the scan's `cached_twice` in
    /// [`Heap::push_cache`]). With the cached slots holding their tokens,
    /// `out[0]` is the popped slot's.
    #[inline(always)]
    pub(super) fn pop_fast(
        &mut self,
        class: usize,
        flags: u8,
        Tracked(out): Tracked<&mut Map<int, GhostSingleton<int>>>,
    ) -> (r: Option<(*mut u8, bool)>)
        requires
            class < NCLASSES,
            old(self).wf(),
            (flags & PUSH_PAC != 0) ==> pac::pac_available(),
            (flags & PUSH_PAC != 0) ==> crate::sys::may_abort_corrupt(),
        ensures
            final(self).keeps(*old(self)),
            r matches Some((p, _)) ==> ((flags & PUSH_PAC != 0) == pac_mode() ==> process_regions().is_slot(class as int, p@.addr)),
            old(self).tokened() ==> final(self).tokened(),
            old(self).tokened() ==> (r matches Some((p, _)) ==> slot_token(*final(out), class as int, (flags & PUSH_PAC != 0) == pac_mode(), p)),
            r matches Some((p, true)) ==> ((flags & PUSH_PAC != 0) == pac_mode() ==> final(self).handed_ok(p, class as int)),
        opens_invariants none
    {
        broadcast use lemma_handed_ok;
        let cache = self.cache_ptr(class);
        let ghost old_perms = self.perms@;
        proof {
            assert(cell_ok(cache, old_perms.hdrs[cache], old_perms.slots));
        }
        // This thread's own header, unused (`len == 0`) or with a stack of
        // `len` entries.
        let h = ptr_mut_ref(cache, Tracked(self.perms.borrow_mut().hdrs.tracked_borrow_mut(cache)));
        let len = h.len;
        if len == 0 {
            proof {
                CachePerms::lemma_update(old_perms, self.perms@, cache);
                CachePerms::lemma_tracks_hdr(old_perms, self.perms@, cache);
                if old_perms.tokened() {
                    CachePerms::lemma_tokened_hdr(old_perms, self.perms@, cache);
                    CachePerms::lemma_claimed_hdr(old_perms, self.perms@, cache);
                }
                CachePerms::lemma_zero_hdr(old_perms, self.perms@, cache);
            }
            return None;
        }
        let allocs = h.allocs.wrapping_add(1);
        if ((allocs as u32) & h.compact_mask) == 0 {
            proof {
                CachePerms::lemma_update(old_perms, self.perms@, cache);
                CachePerms::lemma_tracks_hdr(old_perms, self.perms@, cache);
                if old_perms.tokened() {
                    CachePerms::lemma_tokened_hdr(old_perms, self.perms@, cache);
                    CachePerms::lemma_claimed_hdr(old_perms, self.perms@, cache);
                }
                CachePerms::lemma_zero_hdr(old_perms, self.perms@, cache);
            }
            return None;
        }
        h.allocs = allocs;
        proof {
            CachePerms::lemma_update(old_perms, self.perms@, cache);
            CachePerms::lemma_tracks_hdr(old_perms, self.perms@, cache);
            if old_perms.tokened() {
                CachePerms::lemma_tokened_hdr(old_perms, self.perms@, cache);
                CachePerms::lemma_claimed_hdr(old_perms, self.perms@, cache);
            }
            CachePerms::lemma_zero_hdr(old_perms, self.perms@, cache);
        }
        // SAFETY: `len` is the non-empty stack's length.
        Some(unsafe { Self::pop_top(cache, len, flags & PUSH_PAC != 0, Tracked(self.perms.borrow_mut()), Tracked(out)) })
    }
}

} // verus!

impl Heap {
    /// Fast path of `calloc`: the flag is true when the memory is known to
    /// read as zero, so the caller can skip its `memset`.
    #[inline(always)]
    pub fn alloc_zeroed(
        &mut self,
        shared: &Shared,
        class: usize,
        _blk: Tracked<&mut Live>,
    ) -> (*mut u8, bool) {
        match self.pop_fast(class, shared.push_flags(), Tracked::assume_new()) {
            Some(r) => r,
            None => self.alloc_slow(shared, class, Tracked::assume_new(), Tracked::assume_new()),
        }
    }

    /// Fast path of `free`. Aborts unless `p` is an allocated slab slot or
    /// a live huge run. A slot of another arena goes onto this heap's
    /// [`FOREIGN`] cache for the class (design 6.3 step 4).
    #[inline(always)]
    pub fn free(&mut self, shared: &Shared, p: *mut u8, _blk: Tracked<Live>) {
        match shared.classify_free(p, Tracked::assume_new()) {
            Ok((class, arena)) => {
                if arena == self.arena {
                    self.push(
                        shared,
                        class,
                        p,
                        Tracked::assume_new(),
                        Tracked::assume_new(),
                    )
                } else {
                    self.free_other(
                        shared,
                        class,
                        p,
                        Tracked::assume_new(),
                        Tracked::assume_new(),
                    )
                }
            }
            Err(page) => self.free_huge(shared, page, Tracked::assume_new()),
        }
    }

    /// Huge allocation on this thread in its home arena: no cache, one
    /// lock, counted in the thread's statistics.
    #[cold]
    #[inline(never)]
    pub fn alloc_huge(
        &mut self,
        shared: &Shared,
        size: usize,
        align: usize,
        _blk: Tracked<&mut Live>,
    ) -> (*mut u8, bool, Tracked<Zeroed>) {
        let arena = self.arena;
        self.alloc_huge_in(shared, size, align, arena, Tracked::assume_new())
    }
}

verus! {

impl Heap {
    /// Allocation of `class` from `arena` through this heap's cache for
    /// that arena (`MALLOCX_ARENA`, or the home arena of an explicit
    /// cache). Null when `arena` does not exist or is exhausted. `blk`
    /// gets the block's token. `arena` is never [`FOREIGN`] (cache
    /// routing: `MALLOCX_ARENA` names arenas below 4095), so the pop is
    /// from the home cache or the `others` node keyed `arena`.
    pub fn alloc_in(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
        Tracked(blk): Tracked<&mut Live>,
        Tracked(errno): Tracked<&mut ThreadErrno>,
    ) -> (r: (*mut u8, bool))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
            arena != FOREIGN,
        ensures
            final(self).keeps(*old(self)),
            r.0@.addr != 0 ==> process_regions().is_slot(class as int, r.0@.addr),
            r.0@.addr != 0 && r.1 ==> final(self).handed_ok(r.0, class as int),
            r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
    {
        broadcast use lemma_handed_ok;
        proof {
            shared.lemma_wf();
        }
        if arena != self.arena {
            return self.alloc_other(shared, arena, class, Tracked(blk), Tracked(errno));
        }
        let cache = self.cache_ptr(class);
        proof {
            self.lemma_routes_home(class as int);
        }
        // As in `alloc`. SAFETY: `cache` is this heap's own header.
        let len = unsafe { self.hdr_ref(cache) }.len;
        if len != 0 {
            // SAFETY: a real cache with `len` entries.
            return unsafe { self.pop(cache, class, len, shared.pac(), Tracked(blk), Tracked(errno)) };
        }
        // Not `alloc_slow`: an explicit arena is kept even for a shared
        // class (design 13.3).
        self.alloc_slow_in(shared, arena, class, Tracked(blk), Tracked(errno))
    }

    /// Huge allocation on this thread in `arena` (which must exist).
    pub fn alloc_huge_in(
        &mut self,
        shared: &Shared,
        size: usize,
        align: usize,
        arena: u16,
        Tracked(blk): Tracked<&mut Live>,
    ) -> (r: (*mut u8, bool, Tracked<Zeroed>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(self).wf(),
            shared.wf(),
            crate::meta::is_pow2(align),
        ensures
            final(self).keeps(*old(self)),
            final(self).caches_same(*old(self)),
            r.0@.addr != 0 ==> crate::layout::huge_block(r.0@.addr, size, align),
            r.0@.addr != 0 ==> size <= shared.huge_usable(size),
            r.0@.addr != 0 ==> shared.huge_ok(size),
            r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, shared.huge_usable(size)),
            r.0@.addr != 0 ==> final(blk).block(r.0) && final(blk).len() == shared.huge_usable(size),
            r.0@.addr != 0 ==> crate::vspec::zero::huge_placeable(size, align),
    {
        // Premise (lock order): huge allocations come from the entry points
        // and the slow paths, which hold no lock.
        let (p, zero, z) = shared.alloc_huge(size, align, arena, Tracked(blk), Tracked(&mut axiom_entry()));
        if !p.is_null() {
            self.count_uncached_alloc(shared.huge.usable_for(size).unwrap_or(0));
        }
        (p, zero, z)
    }

    /// Free of a huge run on this thread.
    #[cold]
    #[inline(never)]
    pub(super) fn free_huge(&mut self, shared: &Shared, page: usize, Tracked(blk): Tracked<Live>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(self).wf(),
            shared.wf(),
            page <= shared.huge_pages(),
            blk.run_block(page as int),
        ensures
            final(self).keeps(*old(self)),
            final(self).caches_same(*old(self)),
    {
        let tracked mut blk = blk;
        if shared.zero_on_free() {
            // The token for the cleared run (`theorem_zero_on_free_huge`).
            let Tracked(z) = shared.zero_huge(page, Tracked(&mut blk));
            proof {
                super::zero::theorem_zero_on_free_huge(&z, blk, page as int);
            }
        }
        // The run's token; the permission to its bytes is dropped with it.
        let tracked tok = blk.into_run(page as int);
        // Premise (lock order): a free of a huge run comes from an entry
        // point, which holds no lock.
        let n = shared.free_huge(page, Tracked(tok), Tracked(&mut axiom_entry()));
        self.count_uncached_free(n);
    }

    /// `bytes` were allocated on this thread without passing through a
    /// cache (a huge run, or a slot served straight from a slab): count
    /// them in `thread.allocated` (wrapping). The one way `uncached` grows.
    #[inline]
    pub(super) fn count_uncached_alloc(&mut self, bytes: usize)
        requires
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
            final(self).caches_same(*old(self)),
    {
        self.uncached.allocated = self.uncached.allocated.wrapping_add(bytes as u64);
    }

    /// `bytes` were freed on this thread without passing through a cache:
    /// count them in `thread.deallocated` (wrapping).
    #[inline]
    pub(super) fn count_uncached_free(&mut self, bytes: usize)
        requires
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
            final(self).caches_same(*old(self)),
    {
        self.uncached.deallocated = self.uncached.deallocated.wrapping_add(bytes as u64);
    }

    /// A slot of `class` was claimed directly (`MALLOCX_TCACHE_NONE`) on
    /// this thread: count it.
    #[inline(always)]
    pub fn count_direct_alloc(&mut self, class: usize)
        requires
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
            final(self).caches_same(*old(self)),
    {
        self.count_uncached_alloc(class_size(class));
    }

    /// A slot of `class` was released directly on this thread: count it.
    #[inline(always)]
    pub fn count_direct_free(&mut self, class: usize)
        requires
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
            final(self).caches_same(*old(self)),
    {
        self.count_uncached_free(class_size(class));
    }

    /// Refill the home cache and pop, allocating the cache first if this
    /// is the class's first use. Serves directly from the slab when there
    /// is no cache to be had.
    #[cold]
    #[inline(never)]
    pub(super) fn alloc_slow(
        &mut self,
        shared: &Shared,
        class: usize,
        Tracked(blk): Tracked<&mut Live>,
        Tracked(errno): Tracked<&mut ThreadErrno>,
    ) -> (r: (*mut u8, bool))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
            r.0@.addr != 0 ==> process_regions().is_slot(class as int, r.0@.addr),
            r.0@.addr != 0 && r.1 ==> final(self).handed_ok(r.0, class as int),
            r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
    {
        // The default path's one routing decision: a shared class (above
        // `tcache_max`, uncached everywhere) comes from arena 0 whatever
        // the home arena (`Config::shared_class`); explicit `MALLOCX_ARENA`
        // allocations (`alloc_in`) keep theirs.
        if shared.config.shared_class(class) {
            return self.alloc_slow_in(shared, 0, class, Tracked(blk), Tracked(errno));
        }
        // A non-empty home cache means `pop_fast` declined the scheduled
        // pop (design 6.4) rather than found the cache empty: count the
        // pop it declined and serve it here, out of the fast path's body.
        let cache = self.cache_ptr(class);
        proof {
            self.lemma_routes_home(class as int);
        }
        let ghost old_perms = self.perms@;
        proof {
            assert(cell_ok(cache, old_perms.hdrs[cache], old_perms.slots));
        }
        // This thread's own header, unused or with `len` entries.
        let h = ptr_mut_ref(cache, Tracked(self.perms.borrow_mut().hdrs.tracked_borrow_mut(cache)));
        let len = h.len;
        if len != 0 {
            let allocs = h.allocs.wrapping_add(1);
            debug_scheduled(allocs, h.compact_mask);
            h.allocs = allocs;
            proof {
                CachePerms::lemma_update(old_perms, self.perms@, cache);
                CachePerms::lemma_tracks_hdr(old_perms, self.perms@, cache);
                CachePerms::lemma_tokened_hdr(old_perms, self.perms@, cache);
                CachePerms::lemma_claimed_hdr(old_perms, self.perms@, cache);
                CachePerms::lemma_zero_hdr(old_perms, self.perms@, cache);
                Heap::lemma_poppable_same(*old(self), *self, class as int, cache);
            }
            // SAFETY: `cache` is this heap's home cache for `class`, with
            // `len` entries.
            return unsafe { self.pop_compact(cache, class, len, Tracked(blk), Tracked(errno)) };
        }
        proof {
            CachePerms::lemma_update(old_perms, self.perms@, cache);
            CachePerms::lemma_tracks_hdr(old_perms, self.perms@, cache);
            CachePerms::lemma_tokened_hdr(old_perms, self.perms@, cache);
            CachePerms::lemma_claimed_hdr(old_perms, self.perms@, cache);
            CachePerms::lemma_zero_hdr(old_perms, self.perms@, cache);
        }
        self.alloc_slow_in(shared, self.arena, class, Tracked(blk), Tracked(errno))
    }

    /// The slow allocation path for any arena: the home arena or an arena
    /// that is not [`FOREIGN`] (cache routing; the cache it pops is the one
    /// [`Heap::ensure_cache`] routes `arena` to).
    pub(super) fn alloc_slow_in(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
        Tracked(blk): Tracked<&mut Live>,
        Tracked(errno): Tracked<&mut ThreadErrno>,
    ) -> (r: (*mut u8, bool))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
            arena != FOREIGN || arena == old(self).home(),
        ensures
            final(self).keeps(*old(self)),
            r.0@.addr != 0 ==> process_regions().is_slot(class as int, r.0@.addr),
            r.0@.addr != 0 && r.1 ==> final(self).handed_ok(r.0, class as int),
            r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
    {
        broadcast use lemma_handed_ok;
        proof {
            shared.lemma_wf();
        }
        // Premise (lock order): the slow allocation path is entered from a
        // fast path or an entry point, none of which holds a lock.
        let tracked mut ord = axiom_entry();
        self.tick::<true>(shared, Tracked(&mut ord), Tracked(&mut *errno));
        let Some((a, w)) = shared.arena(arena, Tracked(&mut ord)) else {
            return (ptr::null_mut(), false);
        };
        let cache = self.ensure_cache(shared, arena, class, Tracked(&mut ord));
        let ghost s1 = *self;
        if cache.is_null() {
            let (p, zero, t) = shared.alloc_direct(a, w, class, Tracked(blk), Tracked(&mut ord));
            if !p.is_null() {
                self.count_direct_alloc(class);
            }
            let ghost put = p@.addr != 0 && zero;
            self.put_handed(t, Ghost(put), Ghost(p), Ghost(class as int));
            return (p, zero);
        }
        // SAFETY: `cache` is this heap's real cache for `(arena, class)`.
        unsafe {
            let len = self.refill(shared, a, w, cache, class, Tracked(&mut ord));
            proof {
                Heap::lemma_keeps_routes(s1, *self, arena, class as int, cache);
                self.lemma_poppable(arena, class as int, cache);
            }
            if len == 0 {
                return (ptr::null_mut(), false);
            }
            self.pop(cache, class, len, shared.pac(), Tracked(blk), Tracked(errno))
        }
    }

    /// Flush the highest-address half of the home cache and push,
    /// allocating the cache first if this is the class's first use.
    /// Releases directly to the slab when there is no cache to be had.
    #[cold]
    #[inline(never)]
    pub(super) fn free_slow(
        &mut self,
        shared: &Shared,
        class: usize,
        p: *mut u8,
        Tracked(blk): Tracked<Live>,
        Tracked(errno): Tracked<&mut ThreadErrno>,
    )
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            process_regions().is_slot(class as int, p@.addr),
            blk.slot_block(p, class as int),
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
        decreases (if old(self).spec_rehome() { 1int } else { 0int }), 2int,
    {
        proof {
            // A heap that may move is counted, so homed on an arena below
            // `MAX_ARENAS`, never `FOREIGN` (`homed_ok`): the home push's
            // slow path keeps the home.
            if self.rehome {
                assert(self.homed@ is Some);
                assert(self.arena != FOREIGN);
            }
        }
        let arena = self.arena;
        self.free_slow_in(shared, arena, class, p, Tracked(blk), Tracked(errno))
    }

    /// `double_free_scan`'s slow-path compare: abort by name when `p` is
    /// among the top four entries of `cache` (signed as the push signs it).
    /// When no entry stands for `p` (the second disjunct of the
    /// precondition, which [`Heap::lemma_not_cached`] gives a free that
    /// holds `p`'s slot token) the abort is unreachable.
    ///
    /// # Safety
    /// `cache` is one of this heap's real caches.
    #[inline(always)]
    pub(super) unsafe fn scan_or_abort(
        &self,
        shared: &Shared,
        cache: *mut CacheHeader,
        p: *mut u8,
    )
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && process_regions().is_slot(self.hdr_class(cache), p@.addr) && forall|i: int| 0 <= i < self.hdr(cache).len ==> #[trigger] self.cached_addr(cache, i) != p@.addr as int),
            shared.wf(),
            self.wf(),
            self.owns(cache),
    {
        // SAFETY: guaranteed by the caller.
        let slots = unsafe { self.hdr_ref(cache) }.slots;
        let e = if shared.pac() {
            addr_to_ptr(pac::sign(p as usize, slots as usize))
        } else {
            p
        };
        // SAFETY: guaranteed by the caller.
        if unsafe {
            Self::cached_recently(slots, self.hdr_ref(cache).len, e, Tracked(&self.perms.borrow().slots))
        } {
            proof {
                // The hit's entry stands for `p` (`e` is `p` as stored).
                shared.lemma_regions_ok();
                let i = choose|i: int| 0 <= i < self.hdr(cache).len && self.perms@.slots[#[trigger] slot_ptr(slots, i)].value()@.addr == e@.addr;
                assert(self.perms@.ent(cache, i) == slot_ptr(slots, i));
                if process_regions().is_slot(self.hdr_class(cache), p@.addr) {
                    lemma_plain_stored(e, p, self.hdr_class(cache), slots);
                    assert(self.cached_addr(cache, i) == p@.addr as int);
                }
            }
            cached_twice(p);
        }
    }

    /// The slow free path for any arena. With `zero_on_free` every cached
    /// free arrives here (the fast push declines, see
    /// [`Heap::push_cache_flags`]) and the slot is cleared first: the
    /// class-size bytes, from the class the caller verified (the bit test
    /// on the plain path; the size hint on the sized path, as the push
    /// trusts it, so a stale pointer of the right shape is cleared as it
    /// is cached, exactly as it would be handed out; design 6.3).
    pub(super) fn free_slow_in(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
        p: *mut u8,
        Tracked(blk): Tracked<Live>,
        Tracked(errno): Tracked<&mut ThreadErrno>,
    )
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && (arena != FOREIGN || !old(self).rehome)),
            crate::sys::may_syscall(),
            process_regions().is_slot(class as int, p@.addr),
            blk.slot_block(p, class as int),
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
        ensures
            final(self).wf(),
            // Only a young heap's cross-arena free can move the home
            // (`rehome_on_free`).
            arena != FOREIGN || !old(self).spec_rehome() ==> final(self).keeps(*old(self)),
        decreases (if old(self).spec_rehome() { 1int } else { 0int }), 1int,
    {
        let tracked mut blk = blk;
        // Premise (lock order): the slow free path is entered from a fast
        // path or an entry point, none of which holds a lock.
        let tracked mut ord = axiom_entry();
        if arena == FOREIGN
            && self.rehome
            && self.rehome_on_free(shared, class, p, Tracked(&mut blk), Tracked(&mut ord), Tracked(&mut *errno))
        {
            return;
        }
        if shared.zero_on_free() {
            // SAFETY: `p` is a slot start of `class` (the caller's check),
            // and `blk` holds the permission to its bytes.
            let Tracked(z) = unsafe { zero_slot(class, p, Tracked(&mut blk)) };
            proof {
                // `z` covers the slot's whole usable size; the slot is
                // given up below, with its token.
                super::zero::theorem_zero_on_free(&z, p, class as int);
            }
        }
        proof {
            old(self).lemma_wf_regions();
        }
        // `arena` is the home arena, or `FOREIGN` for another arena's
        // slot (`free_other`); a real arena is materialised first, since
        // `ensure_cache` reads its class state for the header (a heap
        // whose first allocation is huge, or whose home moved, may not
        // have built it).
        let cache = if arena == FOREIGN || shared.arena(arena, Tracked(&mut ord)).is_some() {
            self.ensure_cache(shared, arena, class, Tracked(&mut ord))
        } else {
            ptr::null_mut()
        };
        // The slot's token, for the cache or the bitmap; the permission
        // to its bytes is dropped with it. No entry of the cache stands
        // for `p` while `blk` holds it (the scan below).
        proof {
            if cache@.addr != 0 {
                self.lemma_not_cached(&mut blk, cache, class as int, self.hdr(cache).len as int);
            }
            blk.lemma_mem();
        }
        let tracked tok = blk.take_slot(class as int);
        if cache.is_null() {
            self.tick::<false>(shared, Tracked(&mut ord), Tracked(errno));
            // No cache for this class here: straight to the slab, through
            // the arena its slab belongs to.
            shared.free_direct(shared.arena_of_slot(class, p), class, p, Tracked(tok), Tracked(&mut ord));
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
                self.scan_or_abort(shared, cache, p);
            }
            self.tick::<false>(shared, Tracked(&mut ord), Tracked(errno));
            if self.hdr_ref(cache).len == self.hdr_ref(cache).cap {
                // A home cache keeps its lowest half (design 6.4); the
                // FOREIGN cache, popped by nothing, releases everything.
                let n = if arena == FOREIGN {
                    self.hdr_ref(cache).cap
                } else {
                    (self.hdr_ref(cache).cap / 2).max(1)
                };
                self.flush(shared, arena, cache, class, n as usize, Tracked(&mut ord));
                proof {
                    // Cache routing: a full `FOREIGN` cache is released whole.
                    if arena == FOREIGN {
                        assert(self.cached_len(cache) == 0);
                    }
                }
            }
            // The scan ran above; a flush only removes entries.
            let tracked mut tk = Map::<int, GhostSingleton<int>>::tracked_empty();
            proof {
                tk.tracked_insert(0, tok);
            }
            if !Self::push_cache(cache, p, false, shared.pac(), Tracked(self.perms.borrow_mut()), Tracked(&mut tk)) {
                unreachable!("heap: cache still full after a flush");
            }
        }
    }
}

} // verus!

/// The entry points' form of [`free_fast_class`]: the heap comes as the
/// thread's TLS slot value (`tls_heap_slot`, which proved the heap exists
/// and so that this thread observed `READY`), and both it and the
/// `free_flags` byte are produced only after the bit test. Nothing in the
/// slot check needs them, and on x86_64 a byte loaded up front stays live
/// across the bit arithmetic and costs `free` a callee-saved register
/// (`push`/`pop` of `rbx`).
///
/// # Safety
/// `(t, tp)` from [`tls_heap_slot`](super::thread::tls_heap_slot) on this
/// thread.
#[inline(always)]
// The `let ... else` stands for `?`, as the other paths spell it for Verus.
#[allow(clippy::question_mark)]
pub unsafe fn free_fast_slot(
    shared: &Shared,
    t: usize,
    tp: usize,
    p: *mut u8,
    _blk: Tracked<&mut Live>,
    _tls: Tracked<&mut TlsSlot>,
) -> Option<usize> {
    let Some((class, slab)) = shared.set_slot(p) else {
        return None;
    };
    let flags = shared.free_flags();
    // SAFETY: guaranteed by the caller.
    unsafe {
        free_fast_tail(
            shared,
            class,
            slab,
            p,
            flags,
            t,
            tp,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
    }
}

/// After the bit test: the heap from the slot value `t`, authenticated
/// against `tp` when `flags` carries [`PUSH_PAC`], the owner-word test
/// with [`FREE_ROUTE`], the push.
///
/// # Safety
/// `(t, tp)` from [`tls_heap_slot`](super::thread::tls_heap_slot) on this
/// thread, and `flags` is `Shared::free_flags`.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn free_fast_tail(
    shared: &Shared,
    class: usize,
    slab: usize,
    p: *mut u8,
    flags: u8,
    t: usize,
    tp: usize,
    _blk: Tracked<&mut Live>,
    _tls: Tracked<&mut TlsSlot>,
) -> Option<usize> {
    // The two default configurations (one arena, several arenas) each get
    // one compare and the plain push; the opt-in hardening takes the
    // general dispatch. SAFETY (all three): guaranteed by the caller;
    // `flags` carries the process's `PUSH_PAC`, and the two default
    // configurations have it clear, so their slot value is the heap itself
    // and they skip the `PUSH_PAC` test of `heap_from_slot`.
    let pushed = match flags {
        0 => unsafe { heap_from_slot(t, tp, false, Tracked::assume_new()) }.push_fast(
            class,
            p,
            0,
            Tracked::assume_new(),
        ),
        FREE_ROUTE => {
            let h = unsafe { heap_from_slot(t, tp, false, Tracked::assume_new()) };
            shared.home_owns(class, slab, h) && h.push_fast(class, p, 0, Tracked::assume_new())
        }
        _ => {
            let h = unsafe { heap_from_slot(t, tp, flags & PUSH_PAC != 0, Tracked::assume_new()) };
            (flags & FREE_ROUTE == 0 || shared.home_owns(class, slab, h))
                && h.push_fast(class, p, flags & !FREE_ROUTE, Tracked::assume_new())
        }
    };
    if pushed { Some(class) } else { None }
}

verus! {

/// The sized free's fast path on an existing heap (`sdallocx`,
/// `free_sized`): verify the size hint's class against the pointer
/// arithmetically and push onto the home cache without loading the slab
/// descriptor (design 6.3). `false` for anything off the fast path (not a
/// slot of a committed slab, a hint naming another class or a huge size, a
/// full cache); the caller then goes through [`Shared::check_size_hint`]
/// and the exact [`free`]. Contains no call. `blk` holds the token for
/// `p`: a push takes its slot token, and a declined push gives `blk` back
/// the token with a fresh permission to the slot, its bytes unknown.
#[inline(always)]
pub fn free_sized_fast(
    shared: &Shared,
    h: &mut Heap,
    p: *mut u8,
    hint: Option<usize>,
    flags: u8,
    Tracked(blk): Tracked<&mut Live>,
) -> (r: bool)
    requires
        shared.wf(),
        (flags & PUSH_PAC != 0) == pac_mode(),
        (flags & PUSH_PAC != 0) ==> pac::pac_available(),
        hint matches Some(c) ==> c < NCLASSES,
        old(h).wf(),
        old(blk).claims(p),
    ensures
        final(h).keeps(*old(h)),
        flags & PUSH_ZERO != 0 ==> !r,
        r ==> free_target(p),
        r ==> hint_ok(p, hint),
        r ==> slot_start(p),
        !r ==> final(blk).claims(p) && final(blk).len() == old(blk).len(),
        !r ==> *final(blk) == *old(blk) || old(blk).block(p),
        r ==> old(blk).block(p),
{
    match shared.sized_slot(p, hint) {
        Some(class) => {
            let tracked mut tk = Map::<int, GhostSingleton<int>>::tracked_empty();
            let ghost orig = *blk;
            proof {
                shared.lemma_regions_ok();
                lemma_hint_ok(p, class as int);
                lend_slot(blk, &mut tk, class as int, p);
                assert(process_regions().is_slot(class as int, p@.addr));
            }
            let r = h.push_fast(class, p, flags, Tracked(&mut tk));
            proof {
                if !r {
                    blk.put_slot(tk.tracked_remove(0), orig, p, class as int);
                }
            }
            r
        },
        None => false,
    }
}

/// The freed slot `p` of class `c`'s token, from the caller's token for
/// `p`, into `tk[0]` for a cache push ([`slot_token`]); the permission to
/// the slot's bytes is dropped with it ([`Live::take_slot`]), and a push
/// that declines puts the token back with a fresh one ([`Live::put_slot`]).
proof fn lend_slot(tracked blk: &mut Live, tracked tk: &mut Map<int, GhostSingleton<int>>, c: int, p: *mut u8)
    requires
        regions_ok(),
        old(blk).claims(p),
        process_regions().is_slot(c, p@.addr),
        *old(tk) == Map::<int, GhostSingleton<int>>::empty(),
    ensures
        slot_token(*final(tk), c, true, p),
        *final(blk) == old(blk).without_slot(),
        final(tk)[0] == old(blk).slot_tok(),
        old(blk).slot_block(p, c),
{
    old(blk).lemma_claims_slot(p, c);
    old(blk).lemma_claimed_block(p);
    blk.lemma_mem();
    let tracked t = blk.take_slot(c);
    tk.tracked_insert(0, t);
}

} // verus!

/// `malloc` of a non-huge class on the calling thread.
#[inline(always)]
pub fn alloc(shared: &Shared, class: usize, _blk: Tracked<&mut Live>) -> *mut u8 {
    match heap_for_thread(shared) {
        Some(h) => h.alloc(shared, class, Tracked::assume_new()),
        None => {
            shared
                .alloc_direct(
                    &shared.arena0,
                    shared.arena0_published(),
                    class,
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                )
                .0
        }
    }
}

/// `malloc` of a non-huge class whose fast pop declined (`Heap::alloc_fast`
/// returned `None`: an empty cache, or the scheduled pop of design 6.4 that
/// `pop_fast` leaves to the slow path): straight to [`Heap::alloc_slow`],
/// without the retry of the pop that [`alloc`] makes first, so a scheduled
/// pop costs the entry point one tail call and the dispatch in
/// `alloc_slow`, not a second pass through its own fast path (measured:
/// through `alloc` the mixed `valkey1` row paid 4 instructions per
/// operation for the one pop in eight the compacting mode schedules).
#[inline(always)]
pub fn alloc_declined(shared: &Shared, class: usize, _blk: Tracked<&mut Live>) -> *mut u8 {
    match heap_for_thread(shared) {
        Some(h) => {
            h.alloc_slow(shared, class, Tracked::assume_new(), Tracked::assume_new())
                .0
        }
        None => {
            shared
                .alloc_direct(
                    &shared.arena0,
                    shared.arena0_published(),
                    class,
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                )
                .0
        }
    }
}

/// `calloc` of a non-huge class on the calling thread; the flag says the
/// memory is known zero.
#[inline(always)]
pub fn alloc_zeroed(
    shared: &Shared,
    class: usize,
    _blk: Tracked<&mut Live>,
) -> (*mut u8, bool, Tracked<Zeroed>) {
    match heap_for_thread(shared) {
        Some(h) => {
            let (p, zero) = h.alloc_zeroed(shared, class, Tracked::assume_new());
            let z = h.take_handed(
                Ghost::assume_new(),
                Ghost::assume_new(),
                Ghost::assume_new(),
            );
            (p, zero, z)
        }
        None => shared.alloc_direct(
            &shared.arena0,
            shared.arena0_published(),
            class,
            Tracked::assume_new(),
            Tracked::assume_new(),
        ),
    }
}

/// `free` of a non-null pointer on the calling thread. Aborts unless `p`
/// is an allocated slab slot or a live huge run.
#[inline(always)]
pub fn free(shared: &Shared, p: *mut u8, _blk: Tracked<Live>) {
    match heap_for_thread(shared) {
        Some(h) => h.free(shared, p, Tracked::assume_new()),
        None => free_direct(shared, p, Tracked::assume_new()),
    }
}

/// `free` of a non-null pointer straight to its slab bitmap or the huge
/// allocator, through no cache (`MALLOCX_TCACHE_NONE`, or a thread without
/// a heap). Counted in the thread's statistics when it has a heap.
pub fn free_direct(shared: &Shared, p: *mut u8, _blk: Tracked<Live>) {
    match shared.classify_free(p, Tracked::assume_new()) {
        Ok((class, arena)) => {
            if shared.zero_on_free() {
                // SAFETY: `classify_free` verified `p` as an allocated slot
                // of `class`, and `blk` holds the permission to its bytes.
                let _ = unsafe { zero_slot(class, p, Tracked::assume_new()) };
            }
            shared.free_direct(
                arena,
                class,
                p,
                Tracked::assume_new(),
                Tracked::assume_new(),
            );
            if let Some(h) = existing_heap(shared_ptr(shared)) {
                h.count_direct_free(class);
            }
        }
        Err(page) => {
            if shared.zero_on_free() {
                let _ = shared.zero_huge(page, Tracked::assume_new());
            }
            let n = shared.free_huge(page, Tracked::assume_new(), Tracked::assume_new());
            if let Some(h) = existing_heap(shared_ptr(shared)) {
                h.count_uncached_free(n);
            }
        }
    }
}

verus! {

/// `shared` as the raw pointer [`existing_heap`] takes and `init_fields`
/// stores in a new heap: the process's `Shared`, at its storage. Trusted:
/// a reference-to-pointer conversion, whose address Verus does not model;
/// a well-formed `Shared` is the one at [`Shared::storage`], the only
/// place one is built (`Shared::init` requires it; one `Shared` per
/// process; the unit tests build others, and the claim is about the
/// shipped library).
#[inline(always)]
#[verifier::external_body]
pub(crate) fn shared_ptr(shared: &Shared) -> (r: *const Shared)
    requires
        shared.wf(),
    ensures
        r == Shared::storage() as *const Shared,
    opens_invariants none
{
    ptr::from_ref(shared)
}

#[cold]
#[inline(never)]
pub(super) fn invalid_free(p: usize) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!("invalid free of {p:#x}: not an allocation"))
}

/// `zero_on_free`: clear the class-size bytes of slot `p`. Called from the
/// slow paths only (`Heap::free_slow_in`, `free_direct`), so the default
/// `free` carries nothing of it but the flag test; a `memset` of the class
/// size, so a 1 KiB value costs a 1 KiB store (and the slow-path call) per
/// free, which is why the key is off by default. Returns the token for
/// the cleared bytes, for a caller to hold until it gives the slot up
/// ([`theorem_zero_on_free`](super::zero::theorem_zero_on_free)).
///
/// # Safety
/// `p` is a slot start of `class` in a committed slab, and `blk` holds the
/// permission to its bytes.
#[cold]
#[inline(never)]
pub(super) unsafe fn zero_slot(
    class: usize,
    p: *mut u8,
    Tracked(blk): Tracked<&mut Live>,
) -> (z: Tracked<Zeroed>)
    requires
        class < NCLASSES,
        regions_ok(),
        process_regions().is_slot(class as int, p@.addr),
        old(blk).slot_block(p, class as int),
    ensures
        z@.covers(p, class_table()[class as int].size as int),
        final(blk).slot_block(p, class as int),
{
    let ghost orig = *blk;
    proof {
        blk.lemma_mem();
        blk.lemma_slot_len(p, class as int);
        process_regions().lemma_block_nonzero(class as int, p@.addr, 0, 0);
    }
    let tracked mut mem = blk.take_mem();
    // SAFETY: guaranteed by the caller; `mem` makes the slot's `size`
    // bytes writable.
    let z = unsafe { crate::vspec::user::zero_user(p, CLASSES.table()[class].size(), Tracked(&mut mem)) };
    proof {
        blk.put_mem(mem, orig);
    }
    z
}

/// `double_free_scan`: the slot being pushed is one of the top four cache
/// entries, so it was freed a moment ago and never handed out since.
/// Called from the slow path only ([`Heap::free_slow_in`]): the fast push
/// declines instead of calling, so the entry points that inline it keep no
/// frame for this call.
#[cold]
#[inline(never)]
pub(super) fn cached_twice(p: *mut u8) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!(
        "double free of {:#x}: the slot is already in the thread cache",
        p as usize
    ))
}

/// `CLASSES[class].size()` without the bounds check: every `class` on the
/// fast paths came from `class_of` or a slab descriptor, so it is below
/// `NCLASSES`.
#[inline(always)]
pub(super) fn class_size(class: usize) -> (r: usize)
    requires
        class < NCLASSES,
    ensures
        r == crate::layout::class_table()[class as int].size,
{
    debug_assert!(class < NCLASSES);
    // SAFETY: `class < NCLASSES`, the precondition.
    unsafe { crate::vspec::slice::array_get_unchecked(CLASSES.table(), class) }.size()
}

/// The scheduled pop's check, in debug builds: `allocs` is a multiple of
/// the compaction period.
///
/// Trusted: the check is a protocol between the fast pop and the entry
/// points that call the slow path after it, which no contract here
/// states; nothing is claimed. A failed check panics (in debug builds),
/// so it requires `may_abort`.
#[inline(always)]
#[verifier::external_body]
fn debug_scheduled(allocs: u64, mask: u32)
    requires
        crate::sys::may_abort(),
{
    debug_assert!(((allocs as u32) & mask) == 0);
}

} // verus!

/// The `free` fast path on an existing heap: check the slot and push it
/// onto the home cache. `false` whenever anything is off the fast path (not
/// a verified slab slot, another arena's slab, full cache); the caller then
/// goes through a cold [`free`], which repeats the checks and aborts when
/// they fail. Contains no call, so the entry point that inlines it needs no
/// frame.
///
/// `flags` is `Shared::free_flags`: zero (the default configuration with
/// one arena) is the bit test and the plain push; [`FREE_ROUTE`] adds the
/// owner-word test, and the opt-in hardening bits select the push as for
/// the other paths.
#[cfg(test)]
pub fn free_fast(shared: &Shared, h: &mut Heap, p: *mut u8, flags: u8) -> bool {
    free_fast_class(shared, h, p, flags).is_some()
}

/// [`free_fast`] that also names the class it pushed, for
/// `challoc_free_usable` (design 13.7): `Some(class)` when `p` was
/// verified as a slot of `class` and pushed, `None` for anything off the
/// fast path (the caller then takes its cold exact path). The same loads
/// as `free_fast`, no call: [`free_fast_slot`]'s, with the heap and the
/// byte given.
#[cfg(test)]
pub fn free_fast_class(shared: &Shared, h: &mut Heap, p: *mut u8, flags: u8) -> Option<usize> {
    let (class, slab) = shared.set_slot(p)?;
    // `h` as the slot value `heap_from_slot` turns back into it: signed
    // with the thread pointer when `flags` carries `PUSH_PAC`.
    let (_, tp) = crate::sys::tls::get_tp(Tracked::assume_new());
    let t = if flags & PUSH_PAC != 0 {
        pac::sign(h as *mut Heap as usize, tp)
    } else {
        h as *mut Heap as usize
    };
    // SAFETY: `(t, tp)` stand for this thread's slot holding `h`.
    unsafe {
        free_fast_tail(
            shared,
            class,
            slab,
            p,
            flags,
            t,
            tp,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
    }
}

verus! {

/// The `Shared` a heap was created on, with a lifetime free of the heap's
/// borrow (so the heap can be borrowed mutably while it is used): the
/// heap's `shared` field, read as `api::shared_deref` reads it. For every
/// heap the allocator creates that is the process's `Shared` at its
/// storage ([`Heap::wf`]); the unit tests also build heaps over a `Shared`
/// of their own (`testutil`), which they never free, outside the proof.
///
/// # Safety
/// `p` is the `shared` field of a live heap, so it points to a live,
/// initialised `Shared`; a verified caller states the storage address and
/// `api::ready` in the `requires`.
#[inline(always)]
pub(super) unsafe fn shared_of<'a>(p: *const Shared) -> (r: &'a Shared)
    requires
        p == Shared::storage() as *const Shared,
        crate::api::ready(),
    ensures
        r.wf(),
{
    // SAFETY: guaranteed by the caller.
    unsafe { crate::api::shared_deref(p) }
}

} // verus!

/// Kani checks of `debug_scheduled` (`verus/ledger/src/heap/paths.rs.md`;
/// `cfg(kani)` only, see `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_aborts {
    use super::debug_scheduled;
    use crate::kani_proofs::must_not_return;

    /// `debug_scheduled`, `requires may_abort()`, no `ensures`: with debug
    /// assertions on (Kani's build, and the tests'), when `allocs as u32`
    /// has a bit of `mask` the call never returns.
    #[kani::proof]
    #[kani::should_panic]
    fn debug_scheduled_returns_only_if_clear() {
        let allocs: u64 = kani::any();
        let mask: u32 = kani::any();
        debug_scheduled(allocs, mask);
        if (allocs as u32) & mask != 0 {
            must_not_return();
        }
    }

    /// The other half: it returns whenever `allocs as u32` has no bit of
    /// `mask`.
    #[kani::proof]
    fn debug_scheduled_returns_if_clear() {
        let allocs: u64 = kani::any();
        let mask: u32 = kani::any();
        kani::assume((allocs as u32) & mask == 0);
        debug_scheduled(allocs, mask);
    }
}
