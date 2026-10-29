//! The allocation and free paths of a heap: the inlined fast paths the
//! entry points in `api/` are built from, the cold slow paths behind them,
//! and the crate-facing functions that find the calling thread's heap.

use core::ptr;
use vstd::map::Map;
use vstd::prelude::{Ghost, Tracked, int, verus};
use vstd::raw_ptr::ptr_mut_ref;
use vstd::resource::set::GhostSingleton;

use crate::layout::{CLASSES, NCLASSES};
use crate::sys::pac;
use crate::sys::tls::TlsSlot;
use crate::vspec::addr::addr_to_ptr;
use crate::vspec::zero::Zeroed;

use super::thread::heap_from_slot;
use super::{
    CacheHeader, FOREIGN, FREE_ROUTE, Heap, Live, PUSH_PAC, Shared, existing_heap, heap_for_thread,
};

impl Heap {
    /// `malloc` of a non-huge class in the home arena: the cache pop, then
    /// the slow path.
    #[inline(always)]
    pub fn alloc(&mut self, shared: &Shared, class: usize, _blk: Tracked<&mut Live>) -> *mut u8 {
        match self.alloc_fast(class, shared.push_flags(), Tracked::assume_new()) {
            Some(p) => p,
            None => self.alloc_slow(shared, class, Tracked::assume_new()).0,
        }
    }

    /// The cache pop alone: `None` when the cache is empty (or still the
    /// sentinel) and the caller must take a slow path. `flags` is
    /// `Shared::push_flags` (only [`PUSH_PAC`] matters to a pop).
    #[inline(always)]
    // The `match` stands for `Option::map`, which Verus does not specify.
    #[allow(clippy::manual_map)]
    pub fn alloc_fast(
        &mut self,
        class: usize,
        flags: u8,
        _blk: Tracked<&mut Live>,
    ) -> Option<*mut u8> {
        match self.pop_fast(class, flags, Tracked::assume_new()) {
            Some((p, _)) => Some(p),
            None => None,
        }
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
    pub(super) fn pop_fast(
        &mut self,
        class: usize,
        flags: u8,
        _out: Tracked<&mut Map<int, GhostSingleton<int>>>,
    ) -> Option<(*mut u8, bool)> {
        let cache = self.cache_ptr(class);
        // This thread's own header, unused (`len == 0`) or with a stack of
        // `len` entries.
        let h = ptr_mut_ref(cache, Tracked::assume_new());
        let len = h.len;
        if len == 0 {
            return None;
        }
        let allocs = h.allocs.wrapping_add(1);
        if ((allocs as u32) & h.compact_mask) == 0 {
            return None;
        }
        h.allocs = allocs;
        // SAFETY: `len` is the non-empty stack's length.
        Some(unsafe {
            Self::pop_top(
                cache,
                len,
                flags & PUSH_PAC != 0,
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
        })
    }

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
            None => self.alloc_slow(shared, class, Tracked::assume_new()),
        }
    }

    /// Allocation of `class` from `arena` through this heap's cache for
    /// that arena (`MALLOCX_ARENA`, or the home arena of an explicit
    /// cache). Null when `arena` does not exist or is exhausted.
    pub fn alloc_in(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
        _blk: Tracked<&mut Live>,
    ) -> (*mut u8, bool) {
        if arena != self.arena {
            return self.alloc_other(shared, arena, class);
        }
        let cache = self.cache_ptr(class);
        // As in `alloc`. SAFETY: `cache` is this heap's own header.
        let len = unsafe { self.hdr_ref(cache) }.len;
        if len != 0 {
            // SAFETY: a real cache with `len` entries.
            return unsafe { self.pop(cache, class, len, shared.pac(), Tracked::assume_new()) };
        }
        // Not `alloc_slow`: an explicit arena is kept even for a shared
        // class (design 13.3).
        self.alloc_slow_in(shared, arena, class, Tracked::assume_new())
    }

    /// Fast path of `free`. Aborts unless `p` is an allocated slab slot or
    /// a live huge run. A slot of another arena goes onto this heap's
    /// [`FOREIGN`] cache for the class (design 6.3 step 4).
    #[inline(always)]
    pub fn free(&mut self, shared: &Shared, p: *mut u8, _blk: Tracked<Live>) {
        match shared.classify_free(p, Tracked::assume_new()) {
            Ok((class, arena)) => {
                if arena == self.arena {
                    self.push(shared, class, p, Tracked::assume_new())
                } else {
                    self.free_other(shared, class, p)
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

    /// Huge allocation on this thread in `arena` (which must exist).
    pub fn alloc_huge_in(
        &mut self,
        shared: &Shared,
        size: usize,
        align: usize,
        arena: u16,
        _blk: Tracked<&mut Live>,
    ) -> (*mut u8, bool, Tracked<Zeroed>) {
        let (p, zero, z) = shared.alloc_huge(
            size,
            align,
            arena,
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        if !p.is_null() {
            self.count_uncached_alloc(shared.huge.usable_for(size).unwrap_or(0));
        }
        (p, zero, z)
    }

    /// Free of a huge run on this thread.
    #[cold]
    #[inline(never)]
    pub(super) fn free_huge(&mut self, shared: &Shared, page: usize, _blk: Tracked<Live>) {
        if shared.zero_on_free() {
            let _ = shared.zero_huge(page, Tracked::assume_new());
        }
        let n = shared.free_huge(page, Tracked::assume_new(), Tracked::assume_new());
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
    pub(super) fn alloc_slow(
        &mut self,
        shared: &Shared,
        class: usize,
        _blk: Tracked<&mut Live>,
    ) -> (*mut u8, bool) {
        // The default path's one routing decision: a shared class (above
        // `tcache_max`, uncached everywhere) comes from arena 0 whatever
        // the home arena (`Config::shared_class`); explicit `MALLOCX_ARENA`
        // allocations (`alloc_in`) keep theirs.
        if shared.config.shared_class(class) {
            return self.alloc_slow_in(shared, 0, class, Tracked::assume_new());
        }
        // A non-empty home cache means `pop_fast` declined the scheduled
        // pop (design 6.4) rather than found the cache empty: count the
        // pop it declined and serve it here, out of the fast path's body.
        let cache = self.cache_ptr(class);
        // This thread's own header, unused or with `len` entries.
        let h = ptr_mut_ref(cache, Tracked::assume_new());
        let len = h.len;
        if len != 0 {
            let allocs = h.allocs.wrapping_add(1);
            debug_scheduled(allocs, h.compact_mask);
            h.allocs = allocs;
            // SAFETY: `cache` is this heap's home cache for `class`, with
            // `len` entries.
            return unsafe { self.pop_compact(cache, class, len) };
        }
        self.alloc_slow_in(shared, self.arena, class, Tracked::assume_new())
    }

    /// The slow allocation path for any arena.
    pub(super) fn alloc_slow_in(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
        _blk: Tracked<&mut Live>,
    ) -> (*mut u8, bool) {
        self.tick::<true>(shared);
        let Some((a, w)) = shared.arena(arena, Tracked::assume_new()) else {
            return (ptr::null_mut(), false);
        };
        let cache = self.ensure_cache(shared, arena, class, Tracked::assume_new());
        if cache.is_null() {
            let (p, zero, t) =
                shared.alloc_direct(a, w, class, Tracked::assume_new(), Tracked::assume_new());
            if !p.is_null() {
                self.count_direct_alloc(class);
            }
            self.put_handed(
                t,
                Ghost::assume_new(),
                Ghost::assume_new(),
                Ghost::assume_new(),
            );
            return (p, zero);
        }
        // SAFETY: `cache` is this heap's real cache for `(arena, class)`.
        unsafe {
            let len = self.refill(shared, a, w, cache, class, Tracked::assume_new());
            if len == 0 {
                return (ptr::null_mut(), false);
            }
            self.pop(cache, class, len, shared.pac(), Tracked::assume_new())
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
        _blk: Tracked<Live>,
    ) {
        let arena = self.arena;
        self.free_slow_in(shared, arena, class, p, Tracked::assume_new())
    }

    /// `double_free_scan`'s slow-path compare: abort by name when `p` is
    /// among the top four entries of `cache` (signed as the push signs it).
    ///
    /// # Safety
    /// `cache` is one of this heap's real caches.
    #[inline(always)]
    pub(super) unsafe fn scan_or_abort(
        &self,
        shared: &Shared,
        cache: *mut CacheHeader,
        p: *mut u8,
    ) {
        // SAFETY: guaranteed by the caller.
        let slots = unsafe { self.hdr_ref(cache) }.slots;
        let e = if shared.pac() {
            addr_to_ptr(pac::sign(p as usize, slots as usize))
        } else {
            p
        };
        // SAFETY: guaranteed by the caller.
        if unsafe {
            Self::cached_recently(slots, self.hdr_ref(cache).len, e, Tracked::assume_new())
        } {
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
        _blk: Tracked<Live>,
    ) {
        if arena == FOREIGN && self.rehome && self.rehome_on_free(shared, class, p) {
            return;
        }
        if shared.zero_on_free() {
            // SAFETY: `p` is a slot start of `class` (the caller's check),
            // and `blk` holds the permission to its bytes.
            unsafe { zero_slot(class, p) };
        }
        // `arena` is the home arena, or `FOREIGN` for another arena's
        // slot (`free_other`); a real arena is materialised first, since
        // `ensure_cache` reads its class state for the header (a heap
        // whose first allocation is huge, or whose home moved, may not
        // have built it).
        let cache = if arena == FOREIGN || shared.arena(arena, Tracked::assume_new()).is_some() {
            self.ensure_cache(shared, arena, class, Tracked::assume_new())
        } else {
            ptr::null_mut()
        };
        if cache.is_null() {
            self.tick::<false>(shared);
            // No cache for this class here: straight to the slab, through
            // the arena its slab belongs to.
            shared.free_direct(
                shared.arena_of_slot(class, p),
                class,
                p,
                Tracked::assume_new(),
                Tracked::assume_new(),
            );
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
            self.tick::<false>(shared);
            if self.hdr_ref(cache).len == self.hdr_ref(cache).cap {
                // A home cache keeps its lowest half (design 6.4); the
                // FOREIGN cache, popped by nothing, releases everything.
                let n = if arena == FOREIGN {
                    self.hdr_ref(cache).cap
                } else {
                    (self.hdr_ref(cache).cap / 2).max(1)
                };
                self.flush(
                    shared,
                    arena,
                    cache,
                    class,
                    n as usize,
                    Tracked::assume_new(),
                );
            }
            // The scan ran above; a flush only removes entries.
            if !Self::push_cache(
                cache,
                p,
                false,
                shared.pac(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            ) {
                unreachable!("heap: cache still full after a flush");
            }
        }
    }
}

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
    _blk: Tracked<&mut Live>,
) -> bool {
    match shared.sized_slot(p, hint) {
        Some(class) => h.push_fast(class, p, flags, Tracked::assume_new()),
        None => false,
    }
}

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
        Some(h) => h.alloc_slow(shared, class, Tracked::assume_new()).0,
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
                unsafe { zero_slot(class, p) };
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

#[cold]
#[inline(never)]
pub(super) fn invalid_free(p: usize) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!("invalid free of {p:#x}: not an allocation"))
}

} // verus!

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
    crate::sys::abort_fmt(format_args!(
        "double free of {:#x}: the slot is already in the thread cache",
        p as usize
    ))
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

/// The scheduled pop's check, in debug builds: `allocs` is a multiple of
/// the compaction period.
#[inline(always)]
fn debug_scheduled(allocs: u64, mask: u32) {
    debug_assert!(((allocs as u32) & mask) == 0);
}

/// `shared` as the raw pointer the slot readers take.
#[inline(always)]
pub(crate) fn shared_ptr(shared: &Shared) -> *const Shared {
    ptr::from_ref(shared)
}

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
