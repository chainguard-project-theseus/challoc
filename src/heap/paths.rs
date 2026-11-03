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
#[cfg(any(verus_keep_ghost, test))]
use crate::sys::pac;
use crate::sys::tls::TlsSlot;
use crate::vspec::errno::ThreadErrno;
use crate::vspec::zero::Zeroed;

use super::thread::heap_from_slot;
#[cfg(verus_keep_ghost)]
use super::zero::zero_on_free;
#[cfg(verus_keep_ghost)]
use super::{
    CachePerms, PUSH_ZERO, TLS_EXITED, cell_ok, free_target, hand_out, hint_ok, lemma_handed_ok,
    lemma_hint_ok, lemma_plain_stored, pac_mode, regions_ok, slot_ok, slot_ptr, slot_start,
    slot_token,
};
use super::{FOREIGN, FREE_ROUTE, Heap, Live, PUSH_PAC, Shared, existing_heap, heap_for_thread};
#[cfg(verus_keep_ghost)]
use crate::layout::{class_table, process_regions};
#[cfg(verus_keep_ghost)]
use crate::lock::axiom_entry;

verus! {

impl Heap {
    /// `malloc` of a non-huge class in the home arena: the cache pop, then
    /// the slow path. `blk` gets the block's token.
    #[inline(always)]
    pub fn alloc(
        &mut self,
        shared: &Shared,
        class: usize,
        Tracked(blk): Tracked<&mut Live>,
        errno: Tracked<&mut ThreadErrno>,
        Tracked(cell): Tracked<&mut crate::sys::tls::TlsCell>,
    ) -> (r: *mut u8)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
        ensures
            final(cell).kept(*old(cell)),
            final(self).keeps(*old(self)),
            r@.addr != 0 ==> process_regions().is_slot(class as int, r@.addr),
            r@.addr != 0 ==> final(blk).slot_block(r, class as int),
    {
        proof {
            shared.lemma_wf();
        }
        match self.alloc_fast(class, shared.push_flags(), Tracked(blk)) {
            Some(p) => p,
            None => self.alloc_slow(shared, class, Tracked(blk), errno, Tracked(cell)).0,
        }
    }

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
    /// (the same move as the push's decline under `zero_on_free` in
    /// [`Heap::push_cache_flags`]). With the cached slots holding their tokens,
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

    /// Fast path of `calloc`: the flag is true when challoc has written
    /// nothing to the slot since its slab was `Fresh` or `Purged`. Callers
    /// do not trust it for the bytes they return (a heap bug can have
    /// written the slot) and clear the slot anyway. `blk` gets the
    /// block's token.
    #[inline(always)]
    pub fn alloc_zeroed(
        &mut self,
        shared: &Shared,
        class: usize,
        Tracked(blk): Tracked<&mut Live>,
        errno: Tracked<&mut ThreadErrno>,
        Tracked(cell): Tracked<&mut crate::sys::tls::TlsCell>,
    ) -> (r: (*mut u8, bool))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
        ensures
            final(cell).kept(*old(cell)),
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
            None => self.alloc_slow(shared, class, Tracked(blk), errno, Tracked(cell)),
        }
    }

    /// Fast path of `free`. Aborts unless `p` is an allocated slab slot or
    /// a live huge run. A slot of another arena goes onto this heap's
    /// [`FOREIGN`] cache for the class (design 6.3 step 4). `blk` is the
    /// caller's token for `p` ([`Live::claims`]).
    #[inline(always)]
    pub fn free(&mut self, shared: &Shared, p: *mut u8, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && blk.block(p)),
            crate::sys::may_syscall(),
            shared.wf(),
            old(self).wf(),
            blk.claims(p),
        ensures
            final(errno).val() == old(errno).val(),
            final(self).wf(),
            free_target(p),
            blk.block(p),
    {
        proof {
            shared.lemma_wf();
            shared.lemma_regions_ok();
        }
        match shared.classify_free(p, Tracked(&blk)) {
            Ok((class, arena)) => {
                proof {
                    blk.lemma_claimed_block(p);
                    blk.lemma_claims_slot(p, class as int);
                }
                // The freed slot's token, with the permission to its bytes.
                if arena == self.arena {
                    self.push(shared, class, p, Tracked(blk), Tracked(errno))
                } else {
                    self.free_other(shared, class, p, Tracked(blk), Tracked(errno))
                }
            }
            Err(page) => {
                proof {
                    blk.lemma_run_block(p, page);
                }
                self.free_huge(shared, page, Tracked(blk))
            },
        }
    }

    /// Huge allocation on this thread in its home arena: no cache, one
    /// lock, counted in the thread's statistics. `blk` gets the run's token.
    #[cold]
    #[inline(never)]
    pub fn alloc_huge(&mut self, shared: &Shared, size: usize, align: usize, zero: bool, Tracked(blk): Tracked<&mut Live>) -> (r: (*mut u8, bool, Tracked<Zeroed>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
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
            r.0@.addr != 0 ==> crate::vspec::zero::huge_placeable(size, align),
    {
        let arena = self.arena;
        self.alloc_huge_in(shared, size, align, arena, zero, Tracked(blk))
    }

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
        Tracked(cell): Tracked<&mut crate::sys::tls::TlsCell>,
    ) -> (r: (*mut u8, bool))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
            arena != FOREIGN,
        ensures
            final(cell).kept(*old(cell)),
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
            return self.alloc_other(shared, arena, class, Tracked(blk), Tracked(errno), Tracked(cell));
        }
        let cache = self.cache_ptr(class);
        proof {
            self.lemma_routes_home(class as int);
        }
        // As in `alloc`. SAFETY: `cache` is this heap's own header.
        let len = unsafe { self.hdr_ref(cache) }.len;
        if len != 0 {
            // SAFETY: a real cache with `len` entries.
            return unsafe { self.pop(shared, cache, class, len, shared.pac(), Tracked(blk), Tracked(errno), Tracked(cell)) };
        }
        // Not `alloc_slow`: an explicit arena is kept even for a shared
        // class (design 13.3).
        self.alloc_slow_in(shared, arena, class, Tracked(blk), Tracked(errno), Tracked(cell))
    }

    /// Huge allocation on this thread in `arena` (which must exist).
    pub fn alloc_huge_in(
        &mut self,
        shared: &Shared,
        size: usize,
        align: usize,
        arena: u16,
        zero: bool,
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
        let (p, zero, z) = shared.alloc_huge(size, align, arena, zero, Tracked(blk), Tracked(&mut axiom_entry()));
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
            crate::sys::may_abort() || crate::sys::valid_aborts(),
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
        Tracked(cell): Tracked<&mut crate::sys::tls::TlsCell>,
    ) -> (r: (*mut u8, bool))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(cell).kept(*old(cell)),
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
            return self.alloc_slow_in(shared, 0, class, Tracked(blk), Tracked(errno), Tracked(cell));
        }
        // A non-empty home cache means `pop_fast` declined the scheduled
        // pop (design 6.4) rather than found the cache empty: count the
        // pop it declined and serve it here, out of the fast path's body.
        let cache = self.cache_ptr(class);
        proof {
            self.lemma_routes_home(class as int);
        }
        // `double_free_scan`: `pop_fast` declined every pop, scheduled or
        // not; a cold path of its own serves it, so this one keeps its
        // size with the key off.
        if shared.double_free_scan() {
            return self.alloc_slow_state(shared, class, Tracked(blk), Tracked(errno), Tracked(cell));
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
            return unsafe { self.pop_compact(cache, class, len, Tracked(blk), Tracked(errno), Tracked(cell)) };
        }
        proof {
            CachePerms::lemma_update(old_perms, self.perms@, cache);
            CachePerms::lemma_tracks_hdr(old_perms, self.perms@, cache);
            CachePerms::lemma_tokened_hdr(old_perms, self.perms@, cache);
            CachePerms::lemma_claimed_hdr(old_perms, self.perms@, cache);
            CachePerms::lemma_zero_hdr(old_perms, self.perms@, cache);
        }
        self.alloc_slow_in(shared, self.arena, class, Tracked(blk), Tracked(errno), Tracked(cell))
    }

    /// [`Heap::alloc_slow`] under `double_free_scan`, where `pop_fast`
    /// declines every pop: the ordinary pop of the home cache (which
    /// schedules its own compacting pops and moves the slot's state to
    /// `LIVE`), or a refill when the cache is empty.
    #[cold]
    #[inline(never)]
    fn alloc_slow_state(
        &mut self,
        shared: &Shared,
        class: usize,
        Tracked(blk): Tracked<&mut Live>,
        Tracked(errno): Tracked<&mut ThreadErrno>,
        Tracked(cell): Tracked<&mut crate::sys::tls::TlsCell>,
    ) -> (r: (*mut u8, bool))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(cell).kept(*old(cell)),
            final(self).keeps(*old(self)),
            r.0@.addr != 0 ==> process_regions().is_slot(class as int, r.0@.addr),
            r.0@.addr != 0 && r.1 ==> final(self).handed_ok(r.0, class as int),
            r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
    {
        let cache = self.cache_ptr(class);
        proof {
            self.lemma_routes_home(class as int);
        }
        // SAFETY: this thread's own header, unused or with `len` entries.
        let len = unsafe { self.hdr_ref(cache) }.len;
        if len != 0 {
            // SAFETY: a real cache with `len` entries.
            return unsafe { self.pop(shared, cache, class, len, shared.pac(), Tracked(blk), Tracked(errno), Tracked(cell)) };
        }
        self.alloc_slow_in(shared, self.arena, class, Tracked(blk), Tracked(errno), Tracked(cell))
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
        Tracked(cell): Tracked<&mut crate::sys::tls::TlsCell>,
    ) -> (r: (*mut u8, bool))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
            arena != FOREIGN || arena == old(self).home(),
        ensures
            final(cell).kept(*old(cell)),
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
        self.tick::<true>(shared, Tracked(&mut ord), Tracked(&mut *errno), Tracked(Some(&mut *cell)));
        let Some((a, w)) = shared.arena(arena, Tracked(&mut ord)) else {
            if arena != self.arena {
                // An index `MALLOCX_ARENA` named that does not exist, or
                // an automatic arena not built yet named in the fork
                // window, where no arena is built (design 9).
                return (ptr::null_mut(), false);
            }
            // The home arena could not be built (the metadata region is
            // full: a resource failure, design 14.1). Serve this block
            // uncached from arena 0; the next slow path tries the build
            // again, so the thread is back on its own arena once there is
            // room.
            let (p, zero, t) = shared.alloc_direct(&shared.arena0, shared.arena0_published(), class, Tracked(blk), Tracked(&mut ord));
            if !p.is_null() {
                self.count_direct_alloc(class);
            }
            let ghost put = p@.addr != 0 && zero;
            self.put_handed(t, Ghost(put), Ghost(p), Ghost(class as int));
            return (p, zero);
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
            self.pop(shared, cache, class, len, shared.pac(), Tracked(blk), Tracked(errno), Tracked(cell))
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
            final(errno).val() == old(errno).val(),
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
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            process_regions().is_slot(class as int, p@.addr),
            blk.slot_block(p, class as int),
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
        ensures
            final(errno).val() == old(errno).val(),
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
        // Under `double_free_scan` a heap never moves on a free: the move's
        // flush and re-test would run before the state check below, and a
        // refill on another thread between them could make a double free
        // look like a free of a live slot.
        if arena == FOREIGN
            && self.rehome
            && !shared.double_free_scan()
            && self.rehome_on_free(shared, class, p, Tracked(&mut blk), Tracked(&mut ord), Tracked(&mut *errno))
        {
            return;
        }
        // `double_free_scan`: every cached free arrives here (the fast
        // pushes decline), so this is the one check of the slot's state
        // for the plain, foreign and sized routes: `LIVE` becomes
        // `CACHED`, anything else aborts. Before anything else touches
        // the slot or the cache.
        if shared.double_free_scan() {
            super::slotstate::free(shared, class, p, super::slotstate::CACHED);
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
        // to its bytes is dropped with it.
        proof {
            blk.lemma_mem();
        }
        let tracked tok = blk.take_slot(class as int);
        if cache.is_null() {
            self.tick::<false>(shared, Tracked(&mut ord), Tracked(errno), Tracked(None));
            // No cache for this class here: straight to the slab, through
            // the arena its slab belongs to (the state goes on to `FREE`).
            if shared.double_free_scan() {
                super::slotstate::release(shared, class, p);
            }
            shared.free_direct(shared.arena_of_slot(class, p), class, p, Tracked(tok), Tracked(&mut ord));
            self.count_direct_free(class);
            return;
        }
        // SAFETY: `cache` is this heap's real cache for `(arena, class)`.
        unsafe {
            self.tick::<false>(shared, Tracked(&mut ord), Tracked(errno), Tracked(None));
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
            let tracked mut tk = Map::<int, GhostSingleton<int>>::tracked_empty();
            proof {
                tk.tracked_insert(0, tok);
            }
            if !Self::push_cache(cache, p, shared.pac(), Tracked(self.perms.borrow_mut()), Tracked(&mut tk)) {
                unreachable!("heap: cache still full after a flush");
            }
        }
    }
}

/// The entry points' `free` fast path, `Some(class)` when it pushed `p`:
/// the heap comes as the thread's TLS slot value (`tls_heap_slot`, which
/// proved the heap exists and so that this thread observed `READY`), and
/// both it and the `free_flags` byte are produced only after the bit
/// test. Nothing in the slot check needs them, and on x86_64 a byte
/// loaded up front stays live across the bit arithmetic and costs `free`
/// a callee-saved register (`push`/`pop` of `rbx`). `blk` is the caller's
/// token for `p` ([`Live::claims`]): the push takes its slot token, and a
/// `None` leaves it as it was, for the cold path.
///
/// # Safety
/// `(t, tp)` from [`tls_heap_slot`](super::thread::tls_heap_slot) on this
/// thread, with its token `tls` (the preconditions), which lends the heap
/// ([`heap_from_slot`]).
#[inline(always)]
// The `let ... else` stands for `?`, as the other paths spell it for Verus.
#[allow(clippy::question_mark)]
pub unsafe fn free_fast_slot(
    shared: &Shared,
    t: usize,
    tp: usize,
    p: *mut u8,
    Tracked(blk): Tracked<&mut Live>,
    Tracked(tls): Tracked<&mut TlsSlot>,
) -> (r: Option<usize>)
    requires
        crate::sys::may_abort_corrupt(),
        shared.wf(),
        old(blk).claims(p),
        slot_ok(old(tls).cell, old(tls).loan),
        t == old(tls).value(),
        t > TLS_EXITED,
        tp == old(tls).tp(),
    ensures
        r matches Some(c) ==> c < NCLASSES && process_regions().is_slot(c as int, p@.addr),
        zero_on_free(shared) ==> r is None,
        r is Some ==> free_target(p),
        r is None ==> final(blk).claims(p) && final(blk).len() == old(blk).len(),
        r is None ==> *final(blk) == *old(blk) || old(blk).block(p),
        r matches Some(c) ==> old(blk).slot_block(p, c as int),
        final(tls).lent(*old(tls)),
        r is Some ==> *final(blk) == old(blk).without_slot(),
    opens_invariants [crate::bitmap::word_ns(), crate::class::committed_ns(), crate::class::owner_ns()]
{
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
            Tracked(blk),
            Tracked(tls),
        )
    }
}

/// After the bit test: the heap from the slot value `t`, authenticated
/// against `tp` when `flags` carries [`PUSH_PAC`], the owner-word test
/// with [`FREE_ROUTE`], the push. `blk` as for [`free_fast_slot`].
///
/// # Safety
/// `(t, tp)` from [`tls_heap_slot`](super::thread::tls_heap_slot) on this
/// thread, with its token `tls`, and `flags` is `Shared::free_flags`
/// (the preconditions).
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
    Tracked(blk): Tracked<&mut Live>,
    Tracked(tls): Tracked<&mut TlsSlot>,
) -> (r: Option<usize>)
    requires
        crate::sys::may_abort_corrupt(),
        shared.wf(),
        class < NCLASSES,
        process_regions().is_slot(class as int, p@.addr),
        slab < shared.tables[class as int].spec_nslabs(),
        flags & !FREE_ROUTE == shared.route_fixed(),
        old(blk).claims(p),
        slot_ok(old(tls).cell, old(tls).loan),
        t == old(tls).value(),
        t > TLS_EXITED,
        tp == old(tls).tp(),
    ensures
        r matches Some(c) ==> c == class,
        flags & PUSH_ZERO != 0 ==> r is None,
        zero_on_free(shared) ==> r is None,
        r is Some ==> free_target(p),
        r is None ==> final(blk).claims(p) && final(blk).len() == old(blk).len(),
        r is None ==> *final(blk) == *old(blk) || old(blk).block(p),
        r matches Some(c) ==> old(blk).slot_block(p, c as int),
        r is Some ==> *final(blk) == old(blk).without_slot(),
        final(tls).lent(*old(tls)),
    opens_invariants [crate::class::owner_ns()]
{
    let tracked mut tk = Map::<int, GhostSingleton<int>>::tracked_empty();
    let ghost orig = *blk;
    proof {
        shared.lemma_regions_ok();
        lend_slot(blk, &mut tk, class as int, p);
        lemma_free_route_bits(flags);
        shared.lemma_route_flags(flags);
    }
    // The two default configurations (one arena, several arenas) each get
    // one compare and the plain push; the opt-in hardening takes the
    // general dispatch. `flags` carries the process's `PUSH_PAC`, clear in
    // both default configurations.
    let pushed = match flags {
        // SAFETY: `(t, tp)` is this thread's slot (the caller's contract),
        // and `flags == 0` means `PUSH_PAC` is clear.
        0 => unsafe { heap_from_slot(t, tp, false, Tracked(&tls.cell), Tracked(&mut tls.loan)) }.push_fast(
            class,
            p,
            0,
            Tracked(&mut tk),
        ),
        FREE_ROUTE => {
            // SAFETY: as above; `PUSH_PAC` is clear, so the slot value is the heap.
            let h = unsafe { heap_from_slot(t, tp, false, Tracked(&tls.cell), Tracked(&mut tls.loan)) };
            shared.home_owns(class, slab, h) && h.push_fast(class, p, 0, Tracked(&mut tk))
        }
        _ => {
            // SAFETY: as above; the `PUSH_PAC` bit passed is the process's.
            let h = unsafe { heap_from_slot(t, tp, flags & PUSH_PAC != 0, Tracked(&tls.cell), Tracked(&mut tls.loan)) };
            (flags & FREE_ROUTE == 0 || shared.home_owns(class, slab, h))
                && h.push_fast(class, p, flags & !FREE_ROUTE, Tracked(&mut tk))
        }
    };
    proof {
        if !pushed {
            blk.put_slot(tk.tracked_remove(0), orig, p, class as int);
        }
    }
    if pushed { Some(class) } else { None }
}

/// The bits of a `free_flags` byte the dispatch of [`free_fast_tail`]
/// passes on: the byte without [`FREE_ROUTE`] keeps its [`PUSH_PAC`] and
/// [`PUSH_ZERO`] bits, and `0` and `FREE_ROUTE` have neither.
proof fn lemma_free_route_bits(flags: u8)
    ensures
        (flags & !FREE_ROUTE) & PUSH_PAC == flags & PUSH_PAC,
        (flags & !FREE_ROUTE) & PUSH_ZERO == flags & PUSH_ZERO,
        flags == 0 || flags == FREE_ROUTE ==> flags & PUSH_PAC == 0 && flags & PUSH_ZERO == 0,
        0u8 & PUSH_PAC == 0,
{
    assert((flags & !8u8) & 4u8 == flags & 4u8) by (bit_vector);
    assert((flags & !8u8) & 2u8 == flags & 2u8) by (bit_vector);
    assert(flags == 0u8 || flags == 8u8 ==> flags & 4u8 == 0u8 && flags & 2u8 == 0u8) by (bit_vector);
    assert(0u8 & 4u8 == 0u8) by (bit_vector);
}

/// The sized free's fast path on an existing heap (`sdallocx`,
/// `free_sized`): verify the size hint's class against the pointer
/// arithmetically and push onto the home cache without loading the slab
/// descriptor (design 6.3). `false` for anything off the fast path (not a
/// slot start of the hinted class's region outside a guard slab, a hint
/// naming another class or a huge size, a full cache, every sized free with
/// `exact_sized_free`); the caller then goes through
/// [`Shared::check_size_hint`] and the exact [`free`]. With
/// `exact_sized_free` it declines every pointer and leaves `h` and `blk` as
/// they were. A slot start in a slab no arena has been handed, or not
/// committed, is pushed like any other (`Shared::sized_slot`). Contains
/// no call. `blk` holds the token for `p`: a push takes its slot token,
/// and a declined push gives `blk` back the token with a fresh permission
/// to the slot, its bytes unknown.
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
        shared.exact_sized_free() ==> !r,
        shared.exact_sized_free() ==> *final(h) == *old(h) && *final(blk) == *old(blk),
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

/// `malloc` of a non-huge class on the calling thread. `blk` gets the
/// block's token.
#[inline(always)]
pub fn alloc(shared: &Shared, class: usize, Tracked(blk): Tracked<&mut Live>, errno: Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        class < NCLASSES,
        shared.wf(),
    ensures
        r@.addr != 0 ==> process_regions().is_slot(class as int, r@.addr),
        r@.addr != 0 ==> final(blk).slot_block(r, class as int),
{
    let tracked tls = TlsSlot::entry();
    match super::thread::heap_for_thread_in(shared, Tracked(&mut tls.cell), Tracked(&mut tls.loan)) {
        Some(h) => h.alloc(shared, class, Tracked(blk), errno, Tracked(&mut tls.cell)),
        // Premise (lock order, `lock::axiom_entry`): the C caller holds no
        // allocator lock.
        None => shared.alloc_direct(&shared.arena0, shared.arena0_published(), class, Tracked(blk), Tracked(&mut axiom_entry())).0,
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
pub fn alloc_declined(shared: &Shared, class: usize, Tracked(blk): Tracked<&mut Live>, errno: Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        class < NCLASSES,
    ensures
        r@.addr != 0 ==> process_regions().is_slot(class as int, r@.addr),
        r@.addr != 0 ==> final(blk).slot_block(r, class as int),
{
    let tracked tls = TlsSlot::entry();
    match super::thread::heap_for_thread_in(shared, Tracked(&mut tls.cell), Tracked(&mut tls.loan)) {
        Some(h) => h.alloc_slow(shared, class, Tracked(blk), errno, Tracked(&mut tls.cell)).0,
        // Premise (lock order, `lock::axiom_entry`): the C caller holds no
        // allocator lock.
        None => shared.alloc_direct(&shared.arena0, shared.arena0_published(), class, Tracked(blk), Tracked(&mut axiom_entry())).0,
    }
}

/// `calloc` of a non-huge class on the calling thread; the flag says the
/// memory is known zero.
/// `blk` gets the block's token, and a known-zero token is the one the
/// allocation that made the block (the pop, the slow path or the heapless
/// `alloc_direct`) handed out, before any write to it: fresh for its bytes,
/// which `covers` alone does not say.
#[inline(always)]
pub fn alloc_zeroed(shared: &Shared, class: usize, Tracked(blk): Tracked<&mut Live>, errno: Tracked<&mut ThreadErrno>) -> (r: (*mut u8, bool, Tracked<Zeroed>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        class < NCLASSES,
        shared.wf(),
    ensures
        r.0@.addr != 0 ==> process_regions().is_slot(class as int, r.0@.addr),
        r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, class_table()[class as int].size as int),
        r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
{
    let tracked tls = TlsSlot::entry();
    match super::thread::heap_for_thread_in(shared, Tracked(&mut tls.cell), Tracked(&mut tls.loan)) {
        Some(h) => {
            proof {
                h.lemma_wf_regions();
            }
            let (p, zero) = h.alloc_zeroed(shared, class, Tracked(blk), errno, Tracked(&mut tls.cell));
            let ghost take = p@.addr != 0 && zero;
            let z = h.take_handed(Ghost(take), Ghost(p), Ghost(class as int));
            (p, zero, z)
        },
        // Premise (lock order, `lock::axiom_entry`): the C caller holds no
        // allocator lock.
        None => shared.alloc_direct(&shared.arena0, shared.arena0_published(), class, Tracked(blk), Tracked(&mut axiom_entry())),
    }
}

/// `free` of a non-null pointer on the calling thread. Aborts unless `p`
/// is an allocated slab slot or a live huge run. `blk` is the caller's
/// token for `p` ([`Live::claims`]).
#[inline(always)]
pub fn free(shared: &Shared, p: *mut u8, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        blk.claims(p),
    ensures
        final(errno).val() == old(errno).val(),
        free_target(p),
        blk.block(p),
{
    match heap_for_thread(shared) {
        Some(h) => h.free(shared, p, Tracked(blk), Tracked(errno)),
        None => free_direct(shared, p, Tracked(blk)),
    }
}

/// `free` of a non-null pointer straight to its slab bitmap or the huge
/// allocator, through no cache (`MALLOCX_TCACHE_NONE`, or a thread without
/// a heap). Counted in the thread's statistics when it has a heap. `blk`
/// as for [`free`].
pub fn free_direct(shared: &Shared, p: *mut u8, Tracked(blk): Tracked<Live>)
    requires
        crate::sys::may_abort_corrupt(),
        crate::sys::may_abort() || (crate::sys::valid_aborts() && blk.block(p)),
        crate::sys::may_syscall(),
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
    // Premise (lock order): called from the entry points, which hold no
    // lock.
    let tracked mut ord = axiom_entry();
    match shared.classify_free(p, Tracked(&blk)) {
        Ok((class, arena)) => {
            proof {
                blk.lemma_claimed_block(p);
                blk.lemma_claims_slot(p, class as int);
            }
            // `double_free_scan`: the direct free's state check, `LIVE` to
            // `FREE` (a copy still in a cache is `CACHED` and aborts).
            if shared.double_free_scan() {
                super::slotstate::free(shared, class, p, super::slotstate::FREE);
            }
            let tracked mut blk = blk;
            if shared.zero_on_free() {
                // SAFETY: `classify_free` verified `p` as an allocated slot
                // of `class`, and `blk` holds the permission to its bytes.
                let Tracked(z) = unsafe { zero_slot(class, p, Tracked(&mut blk)) };
                proof {
                    // `z` covers the slot's whole usable size; the slot is
                    // given up below, with its token.
                    super::zero::theorem_zero_on_free(&z, p, class as int);
                }
            }
            // The freed slot's token, from the block's; the permission to
            // its bytes is dropped with it.
            proof {
                blk.lemma_mem();
            }
            let tracked tok = blk.take_slot(class as int);
            shared.free_direct(arena, class, p, Tracked(tok), Tracked(&mut ord));
            if let Some(h) = existing_heap(shared_ptr(shared)) {
                h.count_direct_free(class);
            }
        }
        Err(page) => {
            proof {
                blk.lemma_run_block(p, page);
            }
            let tracked mut blk = blk;
            if shared.zero_on_free() {
                // The token for the cleared run (`theorem_zero_on_free_huge`).
                let Tracked(z) = shared.zero_huge(page, Tracked(&mut blk));
                proof {
                    super::zero::theorem_zero_on_free_huge(&z, blk, page as int);
                }
            }
            // The run's token; the permission to its bytes is dropped with
            // it.
            let tracked t = blk.into_run(page as int);
            let n = shared.free_huge(page, Tracked(t), Tracked(&mut ord));
            if let Some(h) = existing_heap(shared_ptr(shared)) {
                h.count_uncached_free(n);
            }
        }
    }
}

/// A free of a live block, slot or huge run, through no cache
/// ([`free_direct`]: a thread with no heap, `MALLOCX_TCACHE_NONE`), with
/// the block's token, reaches none of its not-an-allocation,
/// invalid-free or double-free checks. It verifies without
/// [`crate::sys::may_abort`], with only [`crate::sys::valid_aborts`]' two
/// capabilities (a resource failure, and the pending checks) and
/// [`crate::sys::integrity_aborts`]. The premise is there because
/// [`free_direct`] counts the free in the thread's heap, found through
/// its TLS slot by [`existing_heap`], which requires `may_abort_corrupt`:
/// under PAC a corrupt slot value faults in `pac::auth`, an integrity
/// abort, as a corrupt cache entry does ([`Heap::alloc_fast`]).
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such function.
#[cfg(verus_keep_ghost)]
fn theorem_valid_free_never_aborts(shared: &Shared, p: *mut u8, Tracked(blk): Tracked<Live>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        crate::sys::integrity_aborts(),
        shared.wf(),
        blk.block(p),
{
    proof {
        shared.lemma_regions_ok();
        blk.lemma_claims(p);
    }
    free_direct(shared, p, Tracked(blk));
}

/// The free of a live huge run on a thread with a heap, as
/// [`Heap::free`] makes it (`classify_free`, then [`Heap::free_huge`]),
/// with only [`crate::sys::valid_aborts`]' two capabilities, reaches
/// none of its not-an-allocation, invalid-free or double-free checks.
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such function.
#[cfg(verus_keep_ghost)]
fn theorem_valid_run_free_never_aborts(shared: &Shared, h: &mut Heap, p: *mut u8, Tracked(blk): Tracked<Live>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        shared.wf(),
        old(h).wf(),
        blk.block(p),
        process_regions().is_huge_block(p@.addr, 1),
{
    proof {
        shared.lemma_wf();
        shared.lemma_regions_ok();
        blk.lemma_claims(p);
    }
    match shared.classify_free(p, Tracked(&blk)) {
        Ok((class, _)) => {
            proof {
                // A page start of the huge region is no slot.
                crate::vspec::zero::lemma_slot_not_huge(class as int, p@.addr);
            }
        },
        Err(page) => {
            proof {
                blk.lemma_run_block(p, page);
            }
            h.free_huge(shared, page, Tracked(blk))
        },
    }
}

/// A thread's free of a live block through its caches ([`Heap::free`]:
/// the push onto the home cache or the `FOREIGN` one, the slow path with
/// its tick (cache GC, decay and purges), the arena it may build, the
/// cache it may create, the flush and the release), with only
/// [`crate::sys::valid_aborts`]' two capabilities, reaches none of
/// its not-an-allocation, invalid-free or double-free checks. The heap is
/// any: a young heap's cross-arena free may also move its home
/// ([`Heap::rehome_on_free`]).
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such function.
#[cfg(verus_keep_ghost)]
fn theorem_valid_cache_free_never_aborts(shared: &Shared, h: &mut Heap, p: *mut u8, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        shared.wf(),
        old(h).wf(),
        blk.block(p),
{
    proof {
        shared.lemma_regions_ok();
        blk.lemma_claims(p);
    }
    h.free(shared, p, Tracked(blk), Tracked(errno));
}

/// A sized free (`free_sized`, `sdallocx`, `free_aligned_sized`) of a
/// live block whose size names it (`hint` is the size's class,
/// `class_for(size, align)`: [`hint_ok`], and for a run `size` rounds to
/// its pages: [`huge_hint_ok`]; the C contract of a sized free), as for
/// [`theorem_valid_cache_free_never_aborts`]: the hinted fast push
/// ([`free_sized_fast`]), then `free_sized_slow`'s check of the hint
/// ([`Shared::check_size_hint`]) and the exact free ([`Heap::free`]),
/// with only [`crate::sys::valid_aborts`]' two capabilities, reaches
/// none of its not-an-allocation, invalid-free or double-free checks.
/// The heap is any: the thread's, or an explicit cache's
/// (`sdallocx_routed`). The theorem composes the steps on a given heap;
/// the real slow paths reach that heap through `heap::free` (which may
/// create the thread's heap, so it requires `may_abort`) or
/// `mallocx::free`.
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such function.
#[cfg(verus_keep_ghost)]
fn theorem_valid_sized_free_never_aborts(shared: &Shared, h: &mut Heap, p: *mut u8, size: usize, hint: Option<usize>, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        shared.wf(),
        old(h).wf(),
        blk.block(p),
        hint matches Some(c) ==> c < NCLASSES,
        hint_ok(p, hint),
        super::huge_hint_ok(blk, p, size),
{
    let tracked mut blk = blk;
    proof {
        shared.lemma_regions_ok();
        blk.lemma_block_target(p);
        blk.lemma_claims(p);
    }
    if hint.is_some() {
        if free_sized_fast(shared, h, p, hint, shared.push_flags(), Tracked(&mut blk)) {
            return;
        }
    }
    proof {
        // A declined push leaves a token that claims `p` with the same
        // usable size: `p`'s block.
        blk.lemma_claimed_block(p);
    }
    shared.check_size_hint(p, size, hint, Tracked(&blk));
    h.free(shared, p, Tracked(blk), Tracked(errno));
}

/// The sized free of [`theorem_valid_sized_free_never_aborts`] through no
/// cache (a thread with no heap, `MALLOCX_TCACHE_NONE`), as for
/// [`theorem_valid_free_never_aborts`]: the check of the hint, then
/// [`free_direct`], reaches none of its not-an-allocation, invalid-free
/// or double-free checks. It assumes [`crate::sys::integrity_aborts`]
/// for the same reason: [`free_direct`] counts the free through
/// [`existing_heap`], which requires `may_abort_corrupt`.
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such function.
#[cfg(verus_keep_ghost)]
fn theorem_valid_sized_direct_free_never_aborts(shared: &Shared, p: *mut u8, size: usize, hint: Option<usize>, Tracked(blk): Tracked<Live>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        crate::sys::integrity_aborts(),
        shared.wf(),
        blk.block(p),
        hint_ok(p, hint),
        super::huge_hint_ok(blk, p, size),
{
    proof {
        shared.lemma_regions_ok();
        blk.lemma_claims(p);
    }
    shared.check_size_hint(p, size, hint, Tracked(&blk));
    free_direct(shared, p, Tracked(blk));
}

/// With `exact_sized_free` (the configuration key), a sized free
/// (`free_sized`, `free_aligned_sized`, `sdallocx` and the sized
/// `operator delete`s) runs plain `free`'s allocation check before it
/// caches or releases a slot: the hinted fast push ([`free_sized_fast`])
/// declines every pointer and leaves the heap and the caller's token as
/// they were (the assertion), so the free goes on to
/// [`Shared::check_size_hint`] and the exact free ([`Heap::free`]), whose
/// [`Shared::classify_free`] tests the slot's slab and bit
/// (`SlabTable::check_live`) first; the order and the bit test are read
/// from the code.
/// The heap is any: the thread's, or an explicit cache's
/// (`sdallocx_routed`). The theorem composes the steps on a given heap,
/// as [`theorem_valid_sized_free_never_aborts`] does; the real slow paths
/// reach that heap through `heap::free` or `mallocx::free`, whose other
/// route, [`free_direct`], has no fast push and starts with the same
/// check.
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such function.
#[cfg(verus_keep_ghost)]
fn theorem_exact_sized_free(shared: &Shared, h: &mut Heap, p: *mut u8, size: usize, hint: Option<usize>, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        shared.exact_sized_free(),
        old(h).wf(),
        hint matches Some(c) ==> c < NCLASSES,
        blk.claims(p),
    ensures
        final(errno).val() == old(errno).val(),
        free_target(p),
        hint_ok(p, hint),
        blk.block(p),
{
    let tracked mut blk = blk;
    let ghost (h0, b0) = (*h, blk);
    if hint.is_some() {
        let pushed = free_sized_fast(shared, h, p, hint, shared.push_flags(), Tracked(&mut blk));
        assert(!pushed && *h == h0 && blk == b0);
    }
    shared.check_size_hint(p, size, hint, Tracked(&blk));
    h.free(shared, p, Tracked(blk), Tracked(errno));
}

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

/// Test helper: [`free_fast`] that also names the class it pushed.
/// `Some(class)` when `p` was verified as a slot of `class` and pushed,
/// `None` for anything off the fast path. Runs the entry points' path
/// ([`free_fast_slot`]'s bit test and `free_fast_tail`) with the heap `h`
/// and the `flags` byte given instead of read from the thread slot and
/// `Shared`.
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
