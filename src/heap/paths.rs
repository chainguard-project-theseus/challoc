//! The allocation and free paths of a heap: the inlined fast paths the
//! entry points in `api/` are built from, the cold slow paths behind them,
//! and the crate-facing functions that find the calling thread's heap.

use core::ptr;

use crate::layout::{CLASSES, NCLASSES};
use crate::sys::pac;

use super::{FOREIGN, Heap, PUSH_PAC, Shared, existing_heap, heap_for_thread};

impl Heap {
    /// `malloc` of a non-huge class in the home arena: the cache pop, then
    /// the slow path.
    #[inline(always)]
    pub fn alloc(&mut self, shared: &Shared, class: usize) -> *mut u8 {
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
    /// `pop_compact` would be the one call inside the inlined pop, and a
    /// call in the body costs `malloc` a frame it needs for nothing else
    /// (the same move as the scan's `cached_twice` in
    /// [`Heap::push_cache`]).
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
    /// read as zero, so the caller can skip its `memset`.
    #[inline(always)]
    pub fn alloc_zeroed(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
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

    /// Fast path of `free`. Aborts unless `p` is an allocated slab slot or
    /// a live huge run. A slot of another arena goes onto this heap's
    /// [`FOREIGN`] cache for the class (design 6.3 step 4).
    #[inline(always)]
    pub fn free(&mut self, shared: &Shared, p: *mut u8) {
        match shared.classify_free(p) {
            Ok((class, arena)) => {
                if arena == self.arena {
                    self.push(shared, class, p)
                } else {
                    self.free_other(shared, class, p)
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
    pub fn alloc_huge_in(
        &mut self,
        shared: &Shared,
        size: usize,
        align: usize,
        arena: u16,
    ) -> (*mut u8, bool) {
        let (p, zero) = shared.alloc_huge(size, align, arena);
        if !p.is_null() {
            self.count_uncached_alloc(shared.huge.usable_for(size).unwrap_or(0));
        }
        (p, zero)
    }

    /// Free of a huge run on this thread.
    #[cold]
    #[inline(never)]
    pub(super) fn free_huge(&mut self, shared: &Shared, page: usize) {
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
    pub(super) fn count_uncached_alloc(&mut self, bytes: usize) {
        self.uncached.allocated += bytes as u64;
    }

    /// `bytes` were freed on this thread without passing through a cache:
    /// count them in `thread.deallocated`.
    #[inline]
    pub(super) fn count_uncached_free(&mut self, bytes: usize) {
        self.uncached.deallocated += bytes as u64;
    }

    /// A slot of `class` was claimed directly (`MALLOCX_TCACHE_NONE`) on
    /// this thread: count it.
    #[inline(always)]
    pub fn count_direct_alloc(&mut self, class: usize) {
        self.count_uncached_alloc(class_size(class));
    }

    /// A slot of `class` was released directly on this thread: count it.
    #[inline(always)]
    pub fn count_direct_free(&mut self, class: usize) {
        self.count_uncached_free(class_size(class));
    }

    /// Refill the home cache and pop, allocating the cache first if this
    /// is the class's first use. Serves directly from the slab when there
    /// is no cache to be had.
    #[cold]
    #[inline(never)]
    pub(super) fn alloc_slow(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        // The default path's one routing decision: a shared class (above
        // `tcache_max`, uncached everywhere) comes from arena 0 whatever
        // the home arena (`Config::shared_class`); explicit `MALLOCX_ARENA`
        // allocations (`alloc_in`) keep theirs.
        if shared.config.shared_class(class) {
            return self.alloc_slow_in(shared, 0, class);
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

    /// The slow allocation path for any arena.
    pub(super) fn alloc_slow_in(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
    ) -> (*mut u8, bool) {
        self.tick(shared);
        let Some(a) = shared.arena(arena) else {
            return (ptr::null_mut(), false);
        };
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
    pub(super) fn free_slow(&mut self, shared: &Shared, class: usize, p: *mut u8) {
        let arena = self.arena;
        self.free_slow_in(shared, arena, class, p)
    }

    /// The slow free path for any arena. With `zero_on_free` every cached
    /// free arrives here (the fast push declines, see
    /// [`Heap::push_cache_flags`]) and the slot is cleared first: the
    /// class-size bytes, from the class the caller verified (the bit test
    /// on the plain path; the size hint on the sized path, as the push
    /// trusts it, so a stale pointer of the right shape is cleared as it
    /// is cached, exactly as it would be handed out; design 6.3).
    pub(super) fn free_slow_in(&mut self, shared: &Shared, arena: u16, class: usize, p: *mut u8) {
        if shared.zero_on_free() {
            // SAFETY: `p` is a slot start of `class` (the caller's check).
            unsafe { zero_slot(class, p) };
        }
        // `arena` is the home arena, or `FOREIGN` for another arena's
        // slot (`free_other`); a real arena is materialised first, since
        // `ensure_cache` reads its class state for the header (a heap
        // whose first allocation is huge, or whose home moved, may not
        // have built it).
        let cache = if arena == FOREIGN || shared.arena(arena).is_some() {
            self.ensure_cache(shared, arena, class)
        } else {
            ptr::null_mut()
        };
        if cache.is_null() {
            self.tick(shared);
            // No cache for this class here: straight to the slab, through
            // the arena its slab belongs to.
            shared.free_direct(shared.arena_of_slot(class, p), class, p);
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
                let e = if shared.pac() {
                    pac::sign(p as usize, slots as usize) as *mut u8
                } else {
                    p
                };
                if Self::cached_recently(slots, (*cache).len, e) {
                    cached_twice(p);
                }
            }
            self.tick(shared);
            if (*cache).len == (*cache).cap {
                // A home cache keeps its lowest half (design 6.4); the
                // FOREIGN cache, popped by nothing, releases everything.
                let n = if arena == FOREIGN {
                    (*cache).cap
                } else {
                    ((*cache).cap / 2).max(1)
                };
                self.flush(shared, arena, cache, class, n as usize);
            }
            // The scan ran above; a flush only removes entries.
            if !Self::push_cache(cache, p, false, shared.pac()) {
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
    if h.push_fast(class, p, flags) {
        Some(class)
    } else {
        None
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
pub fn free_sized_fast(
    shared: &Shared,
    h: &mut Heap,
    p: *mut u8,
    hint: Option<usize>,
    flags: u8,
) -> bool {
    match shared.sized_slot(p, hint) {
        Some(class) => h.push_fast(class, p, flags),
        None => false,
    }
}

/// `malloc` of a non-huge class on the calling thread.
#[inline(always)]
pub fn alloc(shared: &Shared, class: usize) -> *mut u8 {
    match heap_for_thread(shared) {
        Some(h) => h.alloc(shared, class),
        None => shared.alloc_direct(&shared.arena0, class).0,
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
pub fn alloc_declined(shared: &Shared, class: usize) -> *mut u8 {
    match heap_for_thread(shared) {
        Some(h) => h.alloc_slow(shared, class).0,
        None => shared.alloc_direct(&shared.arena0, class).0,
    }
}

/// `calloc` of a non-huge class on the calling thread; the flag says the
/// memory is known zero.
#[inline(always)]
pub fn alloc_zeroed(shared: &Shared, class: usize) -> (*mut u8, bool) {
    match heap_for_thread(shared) {
        Some(h) => h.alloc_zeroed(shared, class),
        None => shared.alloc_direct(&shared.arena0, class),
    }
}

/// `free` of a non-null pointer on the calling thread. Aborts unless `p`
/// is an allocated slab slot or a live huge run.
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
pub(super) fn invalid_free(p: usize) -> ! {
    panic!("invalid free of {p:#x}: not an allocation")
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
pub(super) unsafe fn zero_slot(class: usize, p: *mut u8) {
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
pub(super) fn cached_twice(p: *mut u8) -> ! {
    panic!(
        "double free of {:#x}: the slot is already in the thread cache",
        p as usize
    )
}

/// `CLASSES[class].size()` without the bounds check: every `class` on the
/// fast paths came from `class_of` or a slab descriptor, so it is below
/// `NCLASSES`.
#[inline(always)]
pub(super) fn class_size(class: usize) -> usize {
    debug_assert!(class < NCLASSES);
    // SAFETY: as above.
    unsafe { CLASSES.get_unchecked(class) }.size()
}
