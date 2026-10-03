//! The allocation and free paths of a heap: the inlined fast paths the
//! entry points in `api/` are built from, the cold slow paths behind them,
//! and the crate-facing functions that find the calling thread's heap.

use core::ptr;

use crate::layout::{CLASSES, NCLASSES};
use crate::sys::pac;
use crate::vspec::addr::addr_to_ptr;
use crate::vspec::zero::Zeroed;

#[cfg(verus_only)]
use super::{
    CachePerms, PUSH_ZERO, cell_ok, free_target, hand_out, hint_ok, lemma_handed_ok, lemma_hint_ok,
    pac_mode, regions_ok, slot_token,
};
use super::{FOREIGN, Heap, Live, PUSH_PAC, Shared, existing_heap, heap_for_thread};
#[cfg(verus_only)]
use crate::class::slot_loc;
#[cfg(verus_only)]
use crate::layout::{class_table, process_regions};
use vstd::map::Map;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, int, verus};
use vstd::raw_ptr::ptr_mut_ref;
use vstd::resource::set::GhostSingleton;

verus! {

impl Heap {
    /// `malloc` of a non-huge class in the home arena: the cache pop, then
    /// the slow path. `blk` gets the block's token.
    #[inline(always)]
    pub fn alloc(&mut self, shared: &Shared, class: usize, Tracked(blk): Tracked<&mut Live>) -> (r: *mut u8)
        requires
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
            r@.addr != 0 ==> process_regions().is_slot(class as int, r@.addr),
            r@.addr != 0 ==> final(blk).slot_block(r, class as int),
    {
        proof {
            shared.lemma_wf();
        }
        match self.alloc_fast(class, shared.push_flags(), Tracked(blk)) {
            Some(p) => p,
            None => self.alloc_slow(shared, class, Tracked(blk)).0,
        }
    }

    /// The cache pop alone: `None` when the cache is empty (or still the
    /// sentinel) and the caller must take a slow path. `flags` is
    /// `Shared::push_flags` (only [`PUSH_PAC`] matters to a pop). `blk`
    /// gets the popped slot's token.
    #[inline(always)]
    pub fn alloc_fast(&mut self, class: usize, flags: u8, Tracked(blk): Tracked<&mut Live>) -> (r: Option<*mut u8>)
        requires
            class < NCLASSES,
            old(self).wf(),
            (flags & PUSH_PAC != 0) ==> pac::pac_available(),
        ensures
            final(self).keeps(*old(self)),
            r matches Some(p) ==> ((flags & PUSH_PAC != 0) == pac_mode() ==> process_regions().is_slot(class as int, p@.addr)),
            r matches Some(p) ==> ((flags & PUSH_PAC != 0) == pac_mode() ==> final(blk).slot_block(p, class as int)),
    {
        let tracked mut out = Map::<int, GhostSingleton<int>>::tracked_empty();
        let r = self.pop_fast(class, flags, Tracked(&mut out)).map(|r: (*mut u8, bool)| -> (p: *mut u8) ensures p == r.0 { r.0 });
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
    /// [`Heap::push_cache`]). With the cached slots
    /// holding their tokens, `out[0]` is the popped slot's.
    #[inline(always)]
    pub(super) fn pop_fast(&mut self, class: usize, flags: u8, Tracked(out): Tracked<&mut Map<int, GhostSingleton<int>>>) -> (r: Option<(*mut u8, bool)>)
        requires
            class < NCLASSES,
            old(self).wf(),
            (flags & PUSH_PAC != 0) ==> pac::pac_available(),
        ensures
            final(self).keeps(*old(self)),
            r matches Some((p, _)) ==> ((flags & PUSH_PAC != 0) == pac_mode() ==> process_regions().is_slot(class as int, p@.addr)),
            old(self).tokened() ==> final(self).tokened(),
            old(self).tokened() ==> (r matches Some((p, _)) ==> slot_token(*final(out), class as int, (flags & PUSH_PAC != 0) == pac_mode(), p)),
            r matches Some((p, true)) ==> ((flags & PUSH_PAC != 0) == pac_mode() ==> final(self).handed_ok(p, class as int)),
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
            }
            CachePerms::lemma_zero_hdr(old_perms, self.perms@, cache);
        }
        // SAFETY: `len` is the non-empty stack's length.
        Some(unsafe { Self::pop_top(cache, len, flags & PUSH_PAC != 0, Tracked(self.perms.borrow_mut()), Tracked(out)) })
    }

    /// Fast path of `calloc`: the flag is true when the memory is known to
    /// read as zero, so the caller can skip its `memset`. `blk` gets the
    /// block's token.
    #[inline(always)]
    pub fn alloc_zeroed(&mut self, shared: &Shared, class: usize, Tracked(blk): Tracked<&mut Live>) -> (r: (*mut u8, bool))
        requires
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
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
        let tracked mut out = Map::<int, GhostSingleton<int>>::tracked_empty();
        match self.pop_fast(class, shared.push_flags(), Tracked(&mut out)) {
            Some(r) => {
                proof {
                    hand_out(&mut out, blk, class as int, r.0);
                }
                r
            },
            None => self.alloc_slow(shared, class, Tracked(blk)),
        }
    }

    /// Allocation of `class` from `arena` through this heap's cache for
    /// that arena (`MALLOCX_ARENA`, or the home arena of an explicit
    /// cache). Null when `arena` does not exist or is exhausted. `blk`
    /// gets the block's token.
    pub fn alloc_in(&mut self, shared: &Shared, arena: u16, class: usize, Tracked(blk): Tracked<&mut Live>) -> (r: (*mut u8, bool))
        requires
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
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
            return self.alloc_other(shared, arena, class, Tracked(blk));
        }
        let cache = self.cache_ptr(class);
        // As in `alloc`.
        let len = self.hdr_ref(cache).len;
        if len != 0 {
            // SAFETY: a real cache with `len` entries.
            return unsafe { self.pop(cache, class, len, shared.pac(), Tracked(blk)) };
        }
        // Not `alloc_slow`: an explicit arena is kept even for a shared
        // class (design 13.3).
        self.alloc_slow_in(shared, arena, class, Tracked(blk))
    }

    /// Fast path of `free`. Aborts unless `p` is an allocated slab slot or
    /// a live huge run. A slot of another arena goes onto this heap's
    /// [`FOREIGN`] cache for the class (design 6.3 step 4). `blk` is the
    /// caller's token for `p` ([`Live::claims`]).
    #[inline(always)]
    pub fn free(&mut self, shared: &Shared, p: *mut u8, Tracked(blk): Tracked<Live>)
        requires
            shared.wf(),
            old(self).wf(),
            blk.claims(p),
        ensures
            final(self).keeps(*old(self)),
            free_target(p),
            blk.block(p),
    {
        proof {
            shared.lemma_wf();
            shared.lemma_regions_ok();
        }
        match shared.classify_free(p) {
            Ok((class, arena)) => {
                proof {
                    blk.lemma_claimed_block(p);
                }
                // The freed slot's token, from the block's.
                let tracked mut blk = blk;
                let tracked tok = blk.claim_slot(p, class as int);
                if arena == self.arena {
                    self.push(shared, class, p, Tracked(tok))
                } else {
                    self.free_other(shared, class, p, Tracked(tok))
                }
            }
            Err(page) => {
                proof {
                    blk.lemma_claimed_block(p);
                }
                let tracked t = blk.take_run(p, page);
                self.free_huge(shared, page, Tracked(t))
            },
        }
    }

    /// Huge allocation on this thread in its home arena: no cache, one
    /// lock, counted in the thread's statistics.
    #[cold]
    #[inline(never)]
    pub fn alloc_huge(&mut self, shared: &Shared, size: usize, align: usize, Tracked(blk): Tracked<&mut Live>) -> (r: (*mut u8, bool, Tracked<Zeroed>))
        requires
            old(self).wf(),
            shared.wf(),
            crate::meta::is_pow2(align),
        ensures
            final(self).keeps(*old(self)),
            r.0@.addr != 0 ==> crate::layout::huge_block(r.0@.addr, size, align),
            r.0@.addr != 0 ==> size <= shared.huge_usable(size),
            r.0@.addr != 0 ==> shared.huge_ok(size),
            r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, shared.huge_usable(size)),
            r.0@.addr != 0 ==> final(blk).block(r.0) && final(blk).len() == shared.huge_usable(size),
    {
        let arena = self.arena;
        self.alloc_huge_in(shared, size, align, arena, Tracked(blk))
    }

    /// Huge allocation on this thread in `arena` (which must exist).
    pub fn alloc_huge_in(&mut self, shared: &Shared, size: usize, align: usize, arena: u16, Tracked(blk): Tracked<&mut Live>) -> (r: (*mut u8, bool, Tracked<Zeroed>))
        requires
            old(self).wf(),
            shared.wf(),
            crate::meta::is_pow2(align),
        ensures
            final(self).keeps(*old(self)),
            r.0@.addr != 0 ==> crate::layout::huge_block(r.0@.addr, size, align),
            r.0@.addr != 0 ==> size <= shared.huge_usable(size),
            r.0@.addr != 0 ==> shared.huge_ok(size),
            r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, shared.huge_usable(size)),
            r.0@.addr != 0 ==> final(blk).block(r.0) && final(blk).len() == shared.huge_usable(size),
    {
        let (p, zero, z) = shared.alloc_huge(size, align, arena, Tracked(blk));
        if !p.is_null() {
            self.count_uncached_alloc(shared.huge.usable_for(size).unwrap_or(0));
        }
        (p, zero, z)
    }

    /// Free of a huge run on this thread.
    #[cold]
    #[inline(never)]
    pub(super) fn free_huge(&mut self, shared: &Shared, page: usize, Tracked(tok): Tracked<crate::huge::RunToken>)
        requires
            old(self).wf(),
            shared.wf(),
            page <= shared.huge_pages(),
            tok.wf(),
            tok.start() == page,
        ensures
            final(self).keeps(*old(self)),
    {
        if shared.zero_on_free() {
            shared.zero_huge(page);
        }
        let n = shared.free_huge(page, Tracked(tok));
        self.count_uncached_free(n);
    }

    /// `bytes` were allocated on this thread without passing through a
    /// cache (a huge run, or a slot served straight from a slab): count
    /// them in `thread.allocated` (wrapping, as every counter here). The
    /// one way `uncached` grows.
    #[inline]
    pub(super) fn count_uncached_alloc(&mut self, bytes: usize)
        requires
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
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
    {
        self.count_uncached_free(class_size(class));
    }

    /// Refill the home cache and pop, allocating the cache first if this
    /// is the class's first use. Serves directly from the slab when there
    /// is no cache to be had.
    #[cold]
    #[inline(never)]
    pub(super) fn alloc_slow(&mut self, shared: &Shared, class: usize, Tracked(blk): Tracked<&mut Live>) -> (r: (*mut u8, bool))
        requires
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
            return self.alloc_slow_in(shared, 0, class, Tracked(blk));
        }
        // A non-empty home cache means `pop_fast` declined the scheduled
        // pop (design 6.4) rather than found the cache empty: count the
        // pop it declined and serve it here, out of the fast path's body.
        let cache = self.cache_ptr(class);
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
                CachePerms::lemma_zero_hdr(old_perms, self.perms@, cache);
            }
            // SAFETY: `cache` is this heap's home cache for `class`, with
            // `len` entries.
            return unsafe { self.pop_compact(cache, class, len, Tracked(blk)) };
        }
        proof {
            CachePerms::lemma_update(old_perms, self.perms@, cache);
            CachePerms::lemma_tracks_hdr(old_perms, self.perms@, cache);
            CachePerms::lemma_tokened_hdr(old_perms, self.perms@, cache);
            CachePerms::lemma_zero_hdr(old_perms, self.perms@, cache);
        }
        self.alloc_slow_in(shared, self.arena, class, Tracked(blk))
    }

    /// The slow allocation path for any arena.
    pub(super) fn alloc_slow_in(&mut self, shared: &Shared, arena: u16, class: usize, Tracked(blk): Tracked<&mut Live>) -> (r: (*mut u8, bool))
        requires
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
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
        self.tick(shared);
        let Some(a) = shared.arena(arena) else { return (ptr::null_mut(), false) };
        let cache = self.ensure_cache(shared, arena, class);
        if cache.is_null() {
            let (p, zero, t) = shared.alloc_direct(a, class, Tracked(blk));
            if !p.is_null() {
                self.count_direct_alloc(class);
            }
            let ghost put = p@.addr != 0 && zero;
            self.put_handed(t, Ghost(put), Ghost(p), Ghost(class as int));
            return (p, zero);
        }
        // SAFETY: `cache` is this heap's real cache for `(arena, class)`.
        unsafe {
            let len = self.refill(shared, a, cache, class);
            if len == 0 {
                return (ptr::null_mut(), false);
            }
            self.pop(cache, class, len, shared.pac(), Tracked(blk))
        }
    }

    /// Flush the highest-address half of the home cache and push,
    /// allocating the cache first if this is the class's first use.
    /// Releases directly to the slab when there is no cache to be had.
    #[cold]
    #[inline(never)]
    pub(super) fn free_slow(&mut self, shared: &Shared, class: usize, p: *mut u8, Tracked(tok): Tracked<GhostSingleton<int>>)
        requires
            process_regions().is_slot(class as int, p@.addr),
            tok.id() == slot_loc(class as int),
            tok@ == p@.addr as int,
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
    {
        let arena = self.arena;
        self.free_slow_in(shared, arena, class, p, Tracked(tok))
    }

    /// The slow free path for any arena. With `zero_on_free` every cached
    /// free arrives here (the fast push declines, see
    /// [`Heap::push_cache_flags`]) and the slot is cleared first: the
    /// class-size bytes, from the class the caller verified (the bit test
    /// on the plain path; the size hint on the sized path, as the push
    /// trusts it, so a stale pointer of the right shape is cleared as it
    /// is cached, exactly as it would be handed out; design 6.3).
    pub(super) fn free_slow_in(&mut self, shared: &Shared, arena: u16, class: usize, p: *mut u8, Tracked(tok): Tracked<GhostSingleton<int>>)
        requires
            process_regions().is_slot(class as int, p@.addr),
            tok.id() == slot_loc(class as int),
            tok@ == p@.addr as int,
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
    {
        if shared.zero_on_free() {
            // SAFETY: `p` is a slot start of `class` (the caller's check).
            unsafe { zero_slot(class, p) };
        }
        // `arena` is the home arena, or `FOREIGN` for another arena's
        // slot (`free_other`); a real arena is materialised first, since
        // `ensure_cache` reads its class state for the header (a heap
        // whose first allocation is huge, or whose home moved, may not
        // have built it).
        let cache = if arena == FOREIGN || shared.arena(arena).is_some() { self.ensure_cache(shared, arena, class) } else { ptr::null_mut() };
        if cache.is_null() {
            self.tick(shared);
            // No cache for this class here: straight to the slab, through
            // the arena its slab belongs to.
            shared.free_direct(shared.arena_of_slot(class, p), class, p, Tracked(tok));
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
                let slots = self.hdr_ref(cache).slots;
                let e = if shared.pac() { addr_to_ptr(pac::sign(p as usize, slots as usize)) } else { p };
                if Self::cached_recently(slots, self.hdr_ref(cache).len, e, Tracked(&self.perms.borrow().slots)) {
                    cached_twice(p);
                }
            }
            self.tick(shared);
            if self.hdr_ref(cache).len == self.hdr_ref(cache).cap {
                // A home cache keeps its lowest half (design 6.4); the
                // FOREIGN cache, popped by nothing, releases everything.
                let n = if arena == FOREIGN { self.hdr_ref(cache).cap } else { (self.hdr_ref(cache).cap / 2).max(1) };
                self.flush(shared, arena, cache, class, n as usize);
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

/// The `free` fast path on an existing heap: check the slot and push it
/// onto the home cache. `false` whenever anything is off the fast path (not
/// a verified slab slot, another arena's slab, full cache); the caller then
/// goes through a cold [`free`], which repeats the checks and aborts when
/// they fail. Contains no call, so the entry point that inlines it needs no
/// frame. `blk` is the caller's token for `p` ([`Live::claims`]): the push
/// takes its slot token, and a `false` leaves it as it was, for the cold
/// path.
#[inline(always)]
pub fn free_fast(shared: &Shared, h: &mut Heap, p: *mut u8, flags: u8, Tracked(blk): Tracked<&mut Live>) -> (r: bool)
    requires
        shared.wf(),
        (flags & PUSH_PAC != 0) == pac_mode(),
        (flags & PUSH_PAC != 0) ==> pac::pac_available(),
        old(h).wf(),
        old(blk).claims(p),
    ensures
        final(h).keeps(*old(h)),
        flags & PUSH_ZERO != 0 ==> !r,
        r ==> free_target(p),
        !r ==> *final(blk) == *old(blk),
        r ==> old(blk).block(p),
{
    match shared.checked_slot(p, h) {
        Some(class) => {
            let tracked mut tk = Map::<int, GhostSingleton<int>>::tracked_empty();
            let ghost orig = *blk;
            proof {
                shared.lemma_regions_ok();
                lend_slot(blk, &mut tk, class as int, p);
            }
            let r = h.push_fast(class, p, flags, Tracked(&mut tk));
            proof {
                if !r {
                    blk.put_slot(tk.tracked_remove(0), orig);
                }
            }
            r
        },
        None => false,
    }
}

/// [`free_fast`] that also names the class it pushed, for
/// `challoc_free_usable` (design 13.7): `Some(class)` when `p` was
/// verified as a slot of `class` and pushed, `None` for anything off the
/// fast path (the caller then takes its cold exact path). The same loads
/// as `free_fast`, no call.
#[inline(always)]
pub fn free_fast_class(shared: &Shared, h: &mut Heap, p: *mut u8, flags: u8, Tracked(blk): Tracked<&mut Live>) -> (r: Option<usize>)
    requires
        shared.wf(),
        (flags & PUSH_PAC != 0) == pac_mode(),
        (flags & PUSH_PAC != 0) ==> pac::pac_available(),
        old(h).wf(),
        old(blk).claims(p),
    ensures
        final(h).keeps(*old(h)),
        r matches Some(c) ==> c < NCLASSES && process_regions().is_slot(c as int, p@.addr),
        flags & PUSH_ZERO != 0 ==> r is None,
        r is Some ==> free_target(p),
        r is None ==> *final(blk) == *old(blk),
        r matches Some(c) ==> old(blk).slot_block(p, c as int),
{
    let class = shared.checked_slot(p, h)?;
    let tracked mut tk = Map::<int, GhostSingleton<int>>::tracked_empty();
    let ghost orig = *blk;
    proof {
        shared.lemma_regions_ok();
        lend_slot(blk, &mut tk, class as int, p);
    }
    let r = h.push_fast(class, p, flags, Tracked(&mut tk));
    proof {
        if !r {
            blk.put_slot(tk.tracked_remove(0), orig);
        }
    }
    if r { Some(class) } else { None }
}

/// The sized free's fast path on an existing heap (`sdallocx`,
/// `free_sized`): verify the size hint's class against the pointer
/// arithmetically and push onto the home cache without loading the slab
/// descriptor (design 6.3). `false` for anything off the fast path (not a
/// slot of a committed slab, a hint naming another class or a huge size, a
/// full cache); the caller then goes through [`Shared::check_size_hint`]
/// and the exact [`free`]. Contains no call. `blk` as for [`free_fast`].
#[inline(always)]
pub fn free_sized_fast(shared: &Shared, h: &mut Heap, p: *mut u8, hint: Option<usize>, flags: u8, Tracked(blk): Tracked<&mut Live>) -> (r: bool)
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
        !r ==> *final(blk) == *old(blk),
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
            }
            let r = h.push_fast(class, p, flags, Tracked(&mut tk));
            proof {
                if !r {
                    blk.put_slot(tk.tracked_remove(0), orig);
                }
            }
            r
        },
        None => false,
    }
}

/// The freed slot `p` of class `c`'s token, from the caller's token for
/// `p`, into `tk[0]` for a cache push ([`slot_token`]).
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
        old(blk).has_slot(),
        old(blk).slot_block(p, c),
{
    old(blk).lemma_claims_slot(p, c);
    old(blk).lemma_claimed_block(p);
    let tracked t = blk.take_slot(c);
    tk.tracked_insert(0, t);
}

/// `malloc` of a non-huge class on the calling thread. `blk` gets the
/// block's token.
#[inline(always)]
pub fn alloc(shared: &Shared, class: usize, Tracked(blk): Tracked<&mut Live>) -> (r: *mut u8)
    requires
        class < NCLASSES,
        shared.wf(),
    ensures
        r@.addr != 0 ==> process_regions().is_slot(class as int, r@.addr),
        r@.addr != 0 ==> final(blk).slot_block(r, class as int),
{
    match heap_for_thread(shared) {
        Some(h) => h.alloc(shared, class, Tracked(blk)),
        None => shared.alloc_direct(&shared.arena0, class, Tracked(blk)).0,
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
/// `blk` gets the block's token.
#[inline(always)]
pub fn alloc_declined(shared: &Shared, class: usize, Tracked(blk): Tracked<&mut Live>) -> (r: *mut u8)
    requires
        shared.wf(),
        class < NCLASSES,
    ensures
        r@.addr != 0 ==> process_regions().is_slot(class as int, r@.addr),
        r@.addr != 0 ==> final(blk).slot_block(r, class as int),
{
    match heap_for_thread(shared) {
        Some(h) => h.alloc_slow(shared, class, Tracked(blk)).0,
        None => shared.alloc_direct(&shared.arena0, class, Tracked(blk)).0,
    }
}

/// `calloc` of a non-huge class on the calling thread; the flag says the
/// memory is known zero. `blk` gets the block's token.
#[inline(always)]
pub fn alloc_zeroed(shared: &Shared, class: usize, Tracked(blk): Tracked<&mut Live>) -> (r: (*mut u8, bool, Tracked<Zeroed>))
    requires
        class < NCLASSES,
        shared.wf(),
    ensures
        r.0@.addr != 0 ==> process_regions().is_slot(class as int, r.0@.addr),
        r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, class_table()[class as int].size as int),
        r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
{
    match heap_for_thread(shared) {
        Some(h) => {
            proof {
                h.lemma_wf_regions();
            }
            let (p, zero) = h.alloc_zeroed(shared, class, Tracked(blk));
            let ghost take = p@.addr != 0 && zero;
            let z = h.take_handed(Ghost(take), Ghost(p), Ghost(class as int));
            (p, zero, z)
        },
        None => shared.alloc_direct(&shared.arena0, class, Tracked(blk)),
    }
}

/// `free` of a non-null pointer on the calling thread. Aborts unless `p`
/// is an allocated slab slot or a live huge run. `blk` is the caller's
/// token for `p` ([`Live::claims`]).
#[inline(always)]
pub fn free(shared: &Shared, p: *mut u8, Tracked(blk): Tracked<Live>)
    requires
        shared.wf(),
        blk.claims(p),
    ensures
        free_target(p),
        blk.block(p),
{
    match heap_for_thread(shared) {
        Some(h) => h.free(shared, p, Tracked(blk)),
        None => free_direct(shared, p, Tracked(blk)),
    }
}

/// `free` of a non-null pointer straight to its slab bitmap or the huge
/// allocator, through no cache (`MALLOCX_TCACHE_NONE`, or a thread without
/// a heap). Counted in the thread's statistics when it has a heap. `blk`
/// as for [`free`].
pub fn free_direct(shared: &Shared, p: *mut u8, Tracked(blk): Tracked<Live>)
    requires
        shared.wf(),
        blk.claims(p),
    ensures
        free_target(p),
        blk.block(p),
{
    proof {
        shared.lemma_wf();
        shared.lemma_regions_ok();
    }
    match shared.classify_free(p) {
        Ok((class, arena)) => {
            proof {
                blk.lemma_claimed_block(p);
            }
            if shared.zero_on_free() {
                // SAFETY: `classify_free` verified `p` as an allocated slot
                // of `class`.
                unsafe { zero_slot(class, p) };
            }
            // The freed slot's token, from the block's.
            let tracked mut blk = blk;
            let tracked tok = blk.claim_slot(p, class as int);
            shared.free_direct(arena, class, p, Tracked(tok));
            if let Some(h) = existing_heap(shared_ptr(shared)) {
                h.count_direct_free(class);
            }
        }
        Err(page) => {
            proof {
                blk.lemma_claimed_block(p);
            }
            if shared.zero_on_free() {
                shared.zero_huge(page);
            }
            let tracked t = blk.take_run(p, page);
            let n = shared.free_huge(page, Tracked(t));
            if let Some(h) = existing_heap(shared_ptr(shared)) {
                h.count_uncached_free(n);
            }
        }
    }
}

/// `shared` as the raw pointer [`existing_heap`] takes.
#[inline(always)]
pub(crate) fn shared_ptr(shared: &Shared) -> *const Shared {
    ptr::from_ref(shared)
}

#[cold]
#[inline(never)]
pub(super) fn invalid_free(p: usize) -> ! {
    crate::sys::abort_fmt(format_args!("invalid free of {p:#x}: not an allocation"))
}

/// `zero_on_free`: clear the class-size bytes of slot `p`. Called from the
/// slow paths only (`Heap::free_slow_in`, `free_direct`), so the default
/// `free` carries nothing of it but the flag test; a `memset` of the class
/// size, so a 1 KiB value costs a 1 KiB store (and the slow-path call) per
/// free, which is why the key is off by default.
///
/// # Safety
/// `p` is a slot start of `class` in a committed slab.
#[cold]
#[inline(never)]
pub(super) unsafe fn zero_slot(class: usize, p: *mut u8)
    requires
        class < NCLASSES,
        process_regions().is_slot(class as int, p@.addr),
{
    proof {
        broadcast use vstd::layout::align_of_u8;
        crate::vspec::addr::axiom_slot_writable(class, p);
    }
    // SAFETY: guaranteed by the caller; the slot is `size` writable bytes.
    unsafe { ptr::write_bytes(p, 0, CLASSES.table()[class].size()) };
}

/// `double_free_scan`: the slot being pushed is one of the top four cache
/// entries, so it was freed a moment ago and never handed out since.
/// Called from the slow path only ([`Heap::free_slow_in`]): the fast push
/// declines instead of calling, so the entry points that inline it keep no
/// frame for this call.
#[cold]
#[inline(never)]
pub(super) fn cached_twice(p: *mut u8) -> ! {
    crate::sys::abort_fmt(format_args!("double free of {:#x}: the slot is already in the thread cache", p as usize))
}

/// [`Heap::alloc_slow`]'s `debug_assert!` that a non-empty home cache
/// reached it through a scheduled pop the fast pop declined (design 6.4):
/// `allocs`, the pop's count, has no bit of `mask` set. Compiled out of
/// release builds.
///
/// Trusted: the check is a protocol between the fast pop and the entry
/// points that call the slow path after it, which no contract here states;
/// nothing is claimed.
#[inline(always)]
#[verifier::external_body]
fn debug_scheduled(allocs: u64, mask: u32) {
    debug_assert!(((allocs as u32) & mask) == 0);
}

/// The `Shared` a heap was created on, with a lifetime free of the heap's
/// borrow (so the heap can be borrowed mutably while it is used).
/// Trusted: a raw-pointer dereference; a heap never outlives its `Shared`,
/// which `init` made well formed.
#[inline(always)]
#[verifier::external_body]
pub(super) fn shared_of<'a>(p: *const Shared) -> (r: &'a Shared)
    ensures
        r.wf(),
{
    // SAFETY: as above.
    unsafe { &*p }
}

/// `CLASSES[class].size()` without the bounds check: every `class` on the
/// fast paths came from `class_of` or a slab descriptor, so it is below
/// `NCLASSES`, and the check was two instructions and a panic call.
#[inline(always)]
pub(super) fn class_size(class: usize) -> usize
    requires
        class < NCLASSES,
{
    debug_assert!(class < NCLASSES);
    // SAFETY: as above.
    unsafe { CLASSES.table().get_unchecked(class) }.size()
}

} // verus!
