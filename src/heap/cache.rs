//! The cache header and its stack: the pop and push primitives every
//! path shares, and the first-use setup that gives a header its capacity
//! and stack ([`Heap::ensure_cache`]).

use core::alloc::Layout;
use core::ptr;

use crate::layout::NCLASSES;
use crate::sys::pac;

use super::{FOREIGN, Heap, MIN_CAP, PUSH_PAC, PUSH_SCAN, PUSH_ZERO, Shared};
use super::{MODE_OFF, MODE_PROBE};

/// One thread's cache header for one size class: exactly one cache line,
/// inline in the heap ([`Heap::caches`]) or in an [`ArenaCaches`](super::ArenaCaches) node,
/// pointing at its stack of `cap` slot pointers, a metadata object of its
/// own. The fields the fast paths read (`len`, `cap`, `zeroed`, `slots`,
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
    pub(super) len: u32,
    pub(super) cap: u32,
    /// Smallest `len` since the last GC (design 6.4).
    pub(super) low_water: u32,
    /// `slots[..zeroed]` are known to read as zero. `zeroed <= low_water`:
    /// a pop that takes `len` below `low_water` lowers `zeroed` with it
    /// ([`Heap::pop_top`]), so a push, which stores at `len >= zeroed`,
    /// never touches it.
    pub(super) zeroed: u32,
    /// The stack: a metadata object of `cap` slot pointers
    /// ([`CacheHeader::slots_layout`]; null while `cap == 0`).
    pub(super) slots: *mut *mut u8,
    /// Pops served from this cache since it was created.
    pub(super) allocs: u64,
    /// A pop whose `allocs` count has no bit of this mask set is a
    /// scheduled pop: [`PROBE_MASK`](super::PROBE_MASK),
    /// [`COMPACT_MASK`](super::COMPACT_MASK), or `u32::MAX` when
    /// compaction is off.
    pub(super) compact_mask: u32,
    /// `slots[..run_end]` is the run of low addresses, descending (the
    /// lowest at `run_end - 1`); `run_end <= low_water`.
    pub(super) run_end: u32,
    /// Slots moved in by refills minus slots moved out by flushes
    /// (wrapping; the true value is `len - frees + allocs`).
    pub(super) moved: u64,
    /// The class's refill counter (low bits) at the previous probe: a class
    /// that ordinary refills are draining is not compacted.
    pub(super) probe_refilled: u32,
    /// The arena this cache belongs to (a rebuild's claim needs it).
    pub(super) arena: u16,
    /// [`MODE_OFF`], [`MODE_PROBE`] or [`MODE_COMPACT`](super::MODE_COMPACT).
    pub(super) mode: u16,
    /// Consecutive flushes whose slot array was not already in address
    /// order (wrapping); zeroed by a flush that found it sorted. Drives the
    /// kept half's sort in [`Heap::flush`].
    pub(super) unsorted_flushes: u8,
}

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
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    #[cfg(test)]
    pub fn cap(&self) -> usize {
        self.cap as usize
    }

    #[inline]
    #[cfg(test)]
    pub fn low_water(&self) -> usize {
        self.low_water as usize
    }

    /// Number of entries at the bottom of the stack known to be zero.
    #[inline]
    #[cfg(test)]
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

    #[inline(always)]
    pub(super) fn slots(&mut self) -> *mut *mut u8 {
        self.slots
    }

    /// Sign every entry of `entries` (plain pointers) in place with the
    /// stack base `slots` as the modifier ([`PUSH_PAC`]).
    #[inline]
    pub(super) fn sign_all(slots: *mut *mut u8, entries: &mut [*mut u8]) {
        let m = slots as usize;
        for e in entries {
            *e = pac::sign(*e as usize, m) as *mut u8;
        }
    }

    /// Authenticate every entry of `entries` (signed) in place.
    #[inline]
    pub(super) fn auth_all(slots: *mut *mut u8, entries: &mut [*mut u8]) {
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
        if pac {
            pac::auth(e as usize, self.slots as usize) as *mut u8
        } else {
            e
        }
    }

    /// Cached pointers, bottom of the stack first (the top is popped next).
    /// Only for a cache with a stack (`cap != 0`).
    #[inline]
    pub(super) fn entries(&mut self) -> &mut [*mut u8] {
        debug_assert!(self.cap != 0);
        // SAFETY: `len <= cap` pointers exist at `slots`.
        unsafe { core::slice::from_raw_parts_mut(self.slots, self.len as usize) }
    }
}

impl Heap {
    /// The home header for `class`: `heap + 128 + 64 * class`, no load.
    #[inline(always)]
    pub(super) fn cache_ptr(&mut self, class: usize) -> *mut CacheHeader {
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of` only yields indices below `NCLASSES`.
        unsafe { (&raw mut self.caches).cast::<CacheHeader>().add(class) }
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
    pub(super) unsafe fn pop(
        &mut self,
        cache: *mut CacheHeader,
        class: usize,
        len: u32,
        pac: bool,
    ) -> (*mut u8, bool) {
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
    /// The entry is known zero only if it is below `zeroed`, which is at
    /// most `low_water`, so the zero test and the prefix's shrink live in
    /// the branch that lowers `low_water`.
    ///
    /// # Safety
    /// As for [`Heap::pop`].
    #[inline(always)]
    pub(super) unsafe fn pop_top(cache: *mut CacheHeader, len: u32, pac: bool) -> (*mut u8, bool) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let len = len - 1;
            (*cache).len = len;
            let mut zero = false;
            if len < (*cache).low_water {
                (*cache).low_water = len;
                if len < (*cache).run_end {
                    (*cache).run_end = len;
                }
                if len < (*cache).zeroed {
                    (*cache).zeroed = len;
                    zero = true;
                }
            }
            let slots = (*cache).slots();
            let p = *slots.add(len as usize);
            let p = if pac {
                pac::auth(p as usize, slots as usize) as *mut u8
            } else {
                p
            };
            (p, zero)
        }
    }

    /// Push a checked slot of `class` (home arena) onto its cache.
    #[inline(always)]
    pub(super) fn push(&mut self, shared: &Shared, class: usize, p: *mut u8) {
        if !self.push_fast(class, p, shared.push_flags()) {
            self.free_slow(shared, class, p);
        }
    }

    /// The cache push alone: `false` when the cache is full (or still the
    /// sentinel) and the caller must take a slow path. No counter moves:
    /// the push count is implied by `len`, `allocs` and `moved`, and
    /// `zeroed` is never above the pushed position (see
    /// [`CacheHeader::zeroed`]). `flags` is `Shared::push_flags`.
    #[inline(always)]
    pub fn push_fast(&mut self, class: usize, p: *mut u8, flags: u8) -> bool {
        let cache = self.cache_ptr(class);
        // SAFETY: this thread's own header, unused (`len == cap == 0`, so
        // nothing is written) or with a stack of `cap` slots.
        unsafe { Self::push_cache_flags(cache, p, flags) }
    }

    /// [`Heap::push_cache`] behind the opt-in hardening: with `flags` zero
    /// (the default on every host: `pac_pointers` defaults to false, so
    /// even a PAC host's byte is 0) it is the plain push after one byte
    /// test. On aarch64 the signed-only push (`pac_pointers:true` on a
    /// host with PAC, `flags == PUSH_PAC`) is tested first, one compare
    /// ahead of the default's; off aarch64 `PUSH_PAC` is never set and
    /// that compare is compiled out, so the zero test is the whole
    /// dispatch. With
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
    pub(super) unsafe fn push_cache_flags(cache: *mut CacheHeader, p: *mut u8, flags: u8) -> bool {
        // One byte compare each for the two common configurations: signed
        // pointers alone (opt-in `pac_pointers:true` on an arm64 host with
        // PAC; never set elsewhere, so the compare is compiled out) and no
        // hardening (the default on every host, PAC or not).
        if cfg!(target_arch = "aarch64") && flags == PUSH_PAC {
            // SAFETY: guaranteed by the caller.
            return unsafe { Self::push_cache(cache, p, false, true) };
        }
        if flags == 0 {
            // SAFETY: guaranteed by the caller.
            return unsafe { Self::push_cache(cache, p, false, false) };
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
    pub(super) unsafe fn cached_recently(slots: *mut *mut u8, len: u32, e: *mut u8) -> bool {
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
    /// an abort from here: `cached_twice` would be the one call inside the
    /// inlined push, and a call in the body costs every `free` and
    /// `sdallocx` a frame (7 instructions per call on x86_64, the
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
    pub(super) unsafe fn push_cache(
        cache: *mut CacheHeader,
        p: *mut u8,
        scan: bool,
        pac: bool,
    ) -> bool {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let len = (*cache).len;
            if len == (*cache).cap {
                return false;
            }
            let slots = (*cache).slots();
            let e = if pac {
                pac::sign(p as usize, slots as usize) as *mut u8
            } else {
                p
            };
            if scan && Self::cached_recently(slots, len, e) {
                return false;
            }
            *slots.add(len as usize) = e;
            (*cache).len = len + 1;
        }
        true
    }

    /// The usable cache for `(arena, class)`, giving the header its
    /// capacity and stack on first use. Null if the metadata region is
    /// exhausted or caches are off (`tcache:false`).
    pub(super) fn ensure_cache(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
    ) -> *mut CacheHeader {
        // The capacity first: a class that is never cached for this arena
        // must not create an `others` node. This is the one place the
        // placement rule is consulted on the allocation and free paths.
        let cap = shared.config.cap_for(self.arena, arena, class);
        if cap == 0 || !self.tcache_enabled {
            return ptr::null_mut();
        }
        // The FOREIGN cache is never popped, so its capacity only sets
        // the release batch: half the ladder, released whole when full
        // (`free_slow_in`), is the same batch as a home cache's half
        // flush for a residue that averages a quarter of the ladder
        // instead of three quarters (design 6.3 step 4).
        let cap = if arena == FOREIGN {
            (cap / 2).max(MIN_CAP)
        } else {
            cap
        };
        // The arena before the node: `arena` came from an owner word on
        // the free path, and a word naming an arena that was never built
        // aborts here (`Shared::owner`) before a node is created for it.
        let refilled = if arena == FOREIGN {
            0
        } else {
            shared.owner(arena).class(class).refilled() as u32
        };
        let cache = self.cache_slot(shared, arena, class);
        if cache.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `cache` points into this heap's own header array.
        if unsafe { (*cache).cap } != 0 {
            return cache;
        }
        let mut slots = self.packed_stack(shared, arena, class, cap);
        if slots.is_null() {
            slots = shared.meta.alloc(CacheHeader::slots_layout(cap));
            if slots.is_null() {
                return ptr::null_mut();
            }
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
            // The `FOREIGN` cache is never popped, so it never probes.
            let mode = if arena == FOREIGN {
                MODE_OFF
            } else {
                MODE_PROBE
            };
            (*cache).set_mode(mode);
        }
        cache
    }

    /// A home stack of `cap` slots for `class` packed in the heap's own
    /// block ([`PACK_MAX`](super::PACK_MAX)), claimed; null when `(arena, class)` is not a
    /// packed home cache or its stack is still held by an `others` node
    /// (a `thread.arena` change swapped the arrays), and the caller takes
    /// a metadata object instead.
    fn packed_stack(&mut self, shared: &Shared, arena: u16, class: usize, cap: u32) -> *mut u8 {
        if arena != self.arena || self.pack_used & (1 << class) != 0 {
            return ptr::null_mut();
        }
        let Some(off) = Self::pack_offset(&shared.config, class) else {
            return ptr::null_mut();
        };
        debug_assert!(off + 8 * cap as usize <= self.pack_bytes as usize);
        self.pack_used |= 1 << class;
        // SAFETY: the heap's block has `pack_bytes` bytes from
        // `PACK_START` and `[off, off + 8 * cap)` is inside them, claimed by `class`
        // alone.
        unsafe { (self as *mut Heap as *mut u8).add(super::PACK_START + off) }
    }
}
