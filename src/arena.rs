//! Arenas and explicit caches (design.md section 13.3) and the routing of
//! `MALLOCX_*` flags (13.2) onto heaps, caches and the direct paths.
//!
//! An [`Arena`] is one [`ClassState`] per size class over the shared class
//! regions (every arena's states for class `c` point at the same
//! `SlabTable`, so slabs of different arenas interleave in the region and
//! each descriptor records its owner) plus its own decay period. Arena 0 is
//! built inside `Shared` at init; the [`Arenas`] registry hands out the
//! others. Indices below `narenas` (the `malloc_conf` key, default 1) are
//! *automatic*: new heaps are spread over them round robin, and each is
//! materialised the first time something touches it. Indices from
//! `narenas` up are created explicitly by `arenas.create`. Arenas are
//! never destroyed (`arena.<i>.destroy` purges and retires one); they live
//! in the metadata region.
//!
//! Memory never migrates between arenas: a slot is claimed from and
//! released to the state of the arena that committed its slab, and a
//! thread's heap keeps one cache per `(arena, class)` it has touched
//! (`heap.rs`). Once a second arena exists `Shared::multi_arena` is set and
//! the `free` fast path reads the descriptor's arena to pick the cache; with
//! one arena that load is skipped, since every slab and every heap is arena
//! 0.
//!
//! An explicit cache (`tcache.create`) is a [`Heap`] owned by no thread:
//! same caches, same refill and flush, no TLS. It is not thread safe, as in
//! jemalloc. `MALLOCX_TCACHE_NONE` bypasses every cache and claims or
//! releases straight against the slab bitmaps (the defragmenter relies on
//! this to land allocations in the fullest slabs).

use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};

use crate::class::{ClassState, SlabTable};
use crate::heap::{Heap, Shared};
use crate::layout::{NCLASSES, Regions};
use crate::lock::Lock;
use crate::meta::Meta;

/// Arena indices are the 12-bit `MALLOCX_ARENA` field; 4096 itself is
/// `MALLCTL_ARENAS_ALL`.
pub const MAX_ARENAS: usize = 4096;
/// Explicit caches; ids are the 12-bit `MALLOCX_TCACHE` field minus 2.
pub const MAX_TCACHES: usize = 1024;
/// `MALLCTL_ARENAS_ALL`.
pub const ARENAS_ALL: u32 = 4096;

/// `MALLOCX_ZERO`.
pub const MALLOCX_ZERO: u32 = 0x40;
/// `MALLOCX_TCACHE_NONE` = `MALLOCX_TCACHE(-1)`.
pub const MALLOCX_TCACHE_NONE: u32 = 1 << 8;
const LG_ALIGN_MASK: u32 = 0x3f;
const TCACHE_SHIFT: u32 = 8;
const TCACHE_MASK: u32 = 0xfff;
const ARENA_SHIFT: u32 = 20;
const ARENA_MASK: u32 = 0xfff;

/// `MALLOCX_LG_ALIGN(la)`.
pub const fn mallocx_lg_align(la: u32) -> u32 {
    la & LG_ALIGN_MASK
}

/// `MALLOCX_TCACHE(t)` for an explicit cache id.
pub const fn mallocx_tcache(t: u16) -> u32 {
    ((t as u32 + 2) & TCACHE_MASK) << TCACHE_SHIFT
}

/// `MALLOCX_ARENA(a)`.
pub const fn mallocx_arena(a: u16) -> u32 {
    ((a as u32 + 1) & ARENA_MASK) << ARENA_SHIFT
}

/// Which cache a `MALLOCX` call goes through.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tcache {
    /// The calling thread's heap.
    Default,
    /// No cache: straight to the slab bitmaps (`MALLOCX_TCACHE_NONE`).
    None,
    /// An explicit cache from `tcache.create`.
    Explicit(u16),
}

/// Decoded `MALLOCX_*` flags.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Flags {
    /// `MALLOCX_LG_ALIGN`; 0 means the natural alignment.
    pub lg_align: u32,
    /// `MALLOCX_ZERO`.
    pub zero: bool,
    pub tcache: Tcache,
    /// `MALLOCX_ARENA`; `None` means the caller's (or the cache's) arena.
    pub arena: Option<u16>,
}

impl Flags {
    /// Decode jemalloc's layout: bits 0-5 `lg_align`, bit 6 `ZERO`, bits
    /// 8-19 tcache (0 default, 1 none, `t + 2` explicit `t`), bits 20-31
    /// arena (0 default, `a + 1` arena `a`).
    pub const fn decode(flags: u32) -> Flags {
        let t = (flags >> TCACHE_SHIFT) & TCACHE_MASK;
        let a = (flags >> ARENA_SHIFT) & ARENA_MASK;
        Flags {
            lg_align: flags & LG_ALIGN_MASK,
            zero: flags & MALLOCX_ZERO != 0,
            tcache: match t {
                0 => Tcache::Default,
                1 => Tcache::None,
                _ => Tcache::Explicit((t - 2) as u16),
            },
            arena: if a == 0 { None } else { Some((a - 1) as u16) },
        }
    }

    /// The requested alignment in bytes (1 when none).
    pub const fn align(&self) -> usize {
        1usize << self.lg_align
    }
}

/// One arena (design 13.3): a class state per size class and a decay
/// period. Built in place because the states borrow the tables.
#[repr(C)]
pub struct Arena {
    index: u16,
    /// Decay period (`arena.<i>.dirty_decay_ms`) in nanoseconds;
    /// `purge::NEVER` for `-1`.
    decay_ns: AtomicU64,
    pub classes: [ClassState; NCLASSES],
}

impl Arena {
    /// Initialise `this` in place as arena `index` over `tables`. Returns
    /// `false` if the metadata region cannot hold the slab sets, leaving
    /// `this` unusable.
    ///
    /// # Safety
    /// `this` must point to writable storage for an `Arena` that nothing
    /// else reads until this returns `true`; `tables` must outlive it.
    pub unsafe fn init(
        this: *mut Arena,
        meta: &Meta,
        regions: &Regions,
        tables: &[SlabTable; NCLASSES],
        index: u16,
        decay_ns: u64,
    ) -> bool {
        // SAFETY: the caller guarantees `this` is writable and unaliased.
        unsafe {
            ptr::write(&raw mut (*this).index, index);
            ptr::write(&raw mut (*this).decay_ns, AtomicU64::new(decay_ns));
            for c in 0..NCLASSES {
                match ClassState::new(meta, regions, &tables[c], index) {
                    Some(cs) => ptr::write(&raw mut (*this).classes[c], cs),
                    None => return false,
                }
            }
        }
        true
    }

    #[inline]
    pub fn index(&self) -> u16 {
        self.index
    }

    /// Class state for `class`, without the bounds check the fast paths do
    /// not need (`class` comes from `class_of`).
    #[inline(always)]
    pub fn class(&self, class: usize) -> &ClassState {
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of` only yields indices below `NCLASSES`.
        unsafe { self.classes.get_unchecked(class) }
    }

    /// The decay period in nanoseconds (`purge::NEVER` when purging is off).
    #[inline]
    pub fn decay_ns(&self) -> u64 {
        self.decay_ns.load(Ordering::Relaxed)
    }

    /// Change the decay period; `-1` turns purging off. Takes effect at the
    /// next periodic tick, or at once for a background thread, which is
    /// woken to recompute its deadline.
    pub fn set_decay_ms(&self, ms: i64) {
        self.decay_ns.store(crate::purge::decay_ns_from_ms(ms), Ordering::Relaxed);
        crate::purge::note_decay_changed();
    }

    /// Decay step over every class (see `ClassState::decay`): the earliest
    /// next deadline, `None` when nothing is `Empty` or decay is off.
    pub fn decay(&self, now: u64) -> Option<u64> {
        let decay_ns = self.decay_ns();
        if decay_ns == crate::purge::NEVER {
            return None;
        }
        let mut next: Option<u64> = None;
        for cs in self.classes.iter() {
            if let Some(d) = cs.decay(now, decay_ns) {
                next = Some(next.map_or(d, |n| n.min(d)));
            }
        }
        next
    }

    /// Purge every `Empty` slab of every class whatever its age. Returns
    /// how many were purged.
    pub fn purge_all(&self) -> usize {
        self.classes.iter().map(|cs| cs.purge_empty(u64::MAX)).sum()
    }
}

/// The arena registry. Index 0 is `Shared::arena0`; the rest live in the
/// metadata region. `count` is the number of indices in use: the automatic
/// ones from init plus every `create` since, published with a release
/// store once the arena is usable (automatic arenas may still be
/// unmaterialised, see [`Arenas::get`]).
pub struct Arenas {
    /// Creation lock; the value is `count` for the writer.
    lock: Lock<u32>,
    count: AtomicU32,
    /// Indices below this are automatic (`narenas`).
    auto: u32,
    /// Round robin over the automatic arenas for new heaps.
    next_auto: AtomicU32,
    table: [AtomicPtr<Arena>; MAX_ARENAS],
}

impl Arenas {
    /// Initialise `this` in place with `arena0` at index 0 and `narenas`
    /// automatic indices (clamped to `1..=MAX_ARENAS`).
    ///
    /// # Safety
    /// `this` must point to writable storage that nothing else reads until
    /// this returns; `arena0` must outlive the registry.
    pub unsafe fn init(this: *mut Arenas, arena0: *mut Arena, narenas: u32) {
        let auto = narenas.clamp(1, MAX_ARENAS as u32);
        // SAFETY: the caller guarantees `this` is writable and unaliased.
        unsafe {
            ptr::write(&raw mut (*this).lock, Lock::new(auto));
            ptr::write(&raw mut (*this).count, AtomicU32::new(auto));
            ptr::write(&raw mut (*this).auto, auto);
            ptr::write(&raw mut (*this).next_auto, AtomicU32::new(1));
            let table = &raw mut (*this).table;
            for i in 0..MAX_ARENAS {
                ptr::write(&raw mut (*table)[i], AtomicPtr::new(ptr::null_mut()));
            }
            (*table)[0].store(arena0, Ordering::Release);
        }
    }

    /// Number of arena indices in use (`arenas.narenas`).
    #[inline]
    pub fn count(&self) -> u32 {
        self.count.load(Ordering::Acquire)
    }

    /// Number of automatic arenas (`opt.narenas` as applied).
    #[inline]
    pub fn auto(&self) -> u32 {
        self.auto
    }

    /// Pick the home arena for a new heap: 0 with one automatic arena,
    /// otherwise the next automatic index round robin. Does not materialise
    /// it; the heap's first refill does.
    pub fn assign(&self) -> u16 {
        if self.auto == 1 {
            return 0;
        }
        (self.next_auto.fetch_add(1, Ordering::Relaxed) % self.auto) as u16
    }

    /// Arena `index` if it exists. An automatic index whose arena has not
    /// been built yet is built here (cold); an explicit index that
    /// `create` has not handed out, or one beyond the table, is `None`.
    #[inline]
    pub fn get<'a>(&'a self, shared: &'a Shared, index: u16) -> Option<&'a Arena> {
        let i = index as usize;
        if i >= MAX_ARENAS {
            return None;
        }
        let p = self.table[i].load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: arenas are never freed; the pointer was published
            // after `Arena::init` with a release store.
            return Some(unsafe { &*p });
        }
        self.materialise(shared, index)
    }

    /// Arena `index` if it has been built, without building it.
    pub fn get_existing(&self, index: u16) -> Option<&Arena> {
        let i = index as usize;
        if i >= MAX_ARENAS {
            return None;
        }
        let p = self.table[i].load(Ordering::Acquire);
        // SAFETY: as in `get`.
        if p.is_null() { None } else { Some(unsafe { &*p }) }
    }

    #[cold]
    #[inline(never)]
    fn materialise<'a>(&'a self, shared: &'a Shared, index: u16) -> Option<&'a Arena> {
        if index as u32 >= self.auto {
            return None;
        }
        let _g = self.lock.lock();
        let p = self.table[index as usize].load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: as in `get`.
            return Some(unsafe { &*p });
        }
        self.build(shared, index)
    }

    /// Build arena `index` in the metadata region and publish it. Called
    /// with the creation lock held and the slot known to be empty.
    fn build<'a>(&'a self, shared: &'a Shared, index: u16) -> Option<&'a Arena> {
        let layout = core::alloc::Layout::new::<Arena>();
        // Never freed, so a reservation (page aligned, which covers the
        // 128-byte class state alignment) rather than an object.
        let p = shared.meta.reserve_committed(layout.size(), layout.align().max(shared.regions.page_size()))? as *mut Arena;
        // SAFETY: fresh, zeroed, exclusively owned storage of the right
        // size and alignment; the tables live in `Shared` for the process.
        let ok = unsafe {
            Arena::init(p, &shared.meta, &shared.regions, shared.tables(), index, shared.default_decay_ns())
        };
        if !ok {
            // The reservation is lost; the metadata region is exhausted
            // anyway.
            return None;
        }
        shared.set_multi_arena();
        self.table[index as usize].store(p, Ordering::Release);
        // SAFETY: just built and published.
        Some(unsafe { &*p })
    }

    /// `arenas.create`: a new explicit arena. `None` when the table or the
    /// metadata region is full.
    pub fn create(&self, shared: &Shared) -> Option<u16> {
        let mut g = self.lock.lock();
        let index = *g;
        if index as usize >= MAX_ARENAS {
            return None;
        }
        self.build(shared, index as u16)?;
        *g = index + 1;
        self.count.store(index + 1, Ordering::Release);
        Some(index as u16)
    }

    /// The creation lock, for fork handlers (taken before every class lock).
    pub fn raw_lock(&self) -> &crate::lock::RawLock {
        self.lock.raw()
    }

    /// Every arena that exists, in index order (for decay, purge, stats
    /// and the fork handlers).
    pub fn iter<'a>(&'a self) -> impl Iterator<Item = &'a Arena> + 'a {
        let n = self.count() as usize;
        (0..n).filter_map(move |i| {
            let p = self.table[i].load(Ordering::Acquire);
            // SAFETY: as in `get`.
            if p.is_null() { None } else { Some(unsafe { &*p }) }
        })
    }
}

/// Explicit caches (`tcache.create` / `tcache.flush` / `tcache.destroy`):
/// heaps owned by no thread, looked up by id.
pub struct Tcaches {
    lock: Lock<()>,
    table: [AtomicPtr<Heap>; MAX_TCACHES],
}

impl Tcaches {
    /// Initialise `this` in place, empty.
    ///
    /// # Safety
    /// `this` must point to writable storage that nothing else reads until
    /// this returns.
    pub unsafe fn init(this: *mut Tcaches) {
        // SAFETY: the caller guarantees `this` is writable and unaliased.
        unsafe {
            ptr::write(&raw mut (*this).lock, Lock::new(()));
            let table = &raw mut (*this).table;
            for i in 0..MAX_TCACHES {
                ptr::write(&raw mut (*table)[i], AtomicPtr::new(ptr::null_mut()));
            }
        }
    }

    /// `tcache.create`: a new explicit cache, home arena `arena`. `None`
    /// when the table or the metadata region is full.
    pub fn create(&self, shared: &Shared, arena: u16) -> Option<u16> {
        let _g = self.lock.lock();
        let slot = self.table.iter().position(|p| p.load(Ordering::Relaxed).is_null())?;
        let h = Heap::create_detached(shared, arena);
        if h.is_null() {
            return None;
        }
        self.table[slot].store(h, Ordering::Release);
        Some(slot as u16)
    }

    /// The explicit cache `id`, if it exists. Not thread safe: two threads
    /// using one explicit cache at once is the caller's bug, as in
    /// jemalloc.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub fn get(&self, id: u16) -> Option<&mut Heap> {
        let i = id as usize;
        if i >= MAX_TCACHES {
            return None;
        }
        let p = self.table[i].load(Ordering::Acquire);
        // SAFETY: published after `Heap::create_detached`; only `destroy`
        // frees it, and using a destroyed id is the caller's bug.
        if p.is_null() { None } else { Some(unsafe { &mut *p }) }
    }

    /// `tcache.flush`: return every cached slot of `id` to its slabs.
    /// `false` if there is no such cache.
    pub fn flush(&self, shared: &Shared, id: u16) -> bool {
        match self.get(id) {
            Some(h) => {
                h.flush_all(shared);
                true
            }
            None => false,
        }
    }

    /// `tcache.destroy`: flush and free `id`. `false` if there is no such
    /// cache.
    pub fn destroy(&self, shared: &Shared, id: u16) -> bool {
        let _g = self.lock.lock();
        let i = id as usize;
        if i >= MAX_TCACHES {
            return false;
        }
        let p = self.table[i].swap(ptr::null_mut(), Ordering::AcqRel);
        if p.is_null() {
            return false;
        }
        // SAFETY: `p` came from `create_detached` and is now unreachable
        // through the table.
        unsafe { Heap::destroy(p, shared) };
        true
    }

    /// Number of live explicit caches (for tests and stats).
    pub fn len(&self) -> usize {
        self.table.iter().filter(|p| !p.load(Ordering::Relaxed).is_null()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(target_os = "linux")]
#[cold]
#[inline(never)]
fn invalid_tcache(id: u16) -> ! {
    panic!("MALLOCX_TCACHE({id}): no such explicit cache")
}

/// Routing of a `MALLOCX` allocation of a non-huge class on the calling
/// thread. `None` from the arena lookup (an index `arenas.create` never
/// handed out) gives null. The flag says the memory is known zero.
#[cfg(target_os = "linux")]
pub fn alloc(shared: &Shared, class: usize, arena: Option<u16>, tcache: Tcache) -> (*mut u8, bool) {
    match tcache {
        Tcache::Default => match crate::heap::heap_for_thread(shared) {
            Some(h) => match arena {
                Some(a) => h.alloc_in(shared, a, class),
                None => h.alloc_zeroed(shared, class),
            },
            None => alloc_direct(shared, class, arena.unwrap_or(0)),
        },
        Tcache::Explicit(t) => {
            let Some(h) = shared.tcaches.get(t) else { invalid_tcache(t) };
            let a = arena.unwrap_or(h.arena());
            h.alloc_in(shared, a, class)
        }
        Tcache::None => {
            let a = arena.or_else(|| crate::heap::existing_heap().map(|h| h.arena())).unwrap_or(0);
            let (p, zero) = alloc_direct(shared, class, a);
            if !p.is_null() {
                if let Some(h) = crate::heap::existing_heap() {
                    h.count_direct_alloc(class);
                }
            }
            (p, zero)
        }
    }
}

/// One slot of `class` straight from `arena`'s slab bitmaps.
#[cfg(target_os = "linux")]
fn alloc_direct(shared: &Shared, class: usize, arena: u16) -> (*mut u8, bool) {
    match shared.arena(arena) {
        Some(a) => shared.alloc_direct(a, class),
        None => (ptr::null_mut(), false),
    }
}

/// Routing of a `MALLOCX` huge allocation on the calling thread (caches
/// play no part; the arena is recorded in the run's descriptor).
#[cfg(target_os = "linux")]
pub fn alloc_huge(shared: &Shared, size: usize, align: usize, arena: Option<u16>, tcache: Tcache) -> (*mut u8, bool) {
    let a = match (arena, tcache) {
        (Some(a), _) => a,
        (None, Tcache::Explicit(t)) => match shared.tcaches.get(t) {
            Some(h) => h.arena(),
            None => invalid_tcache(t),
        },
        (None, _) => crate::heap::existing_heap().map_or(0, |h| h.arena()),
    };
    if shared.arena(a).is_none() {
        return (ptr::null_mut(), false);
    }
    match crate::heap::heap_for_thread(shared) {
        Some(h) => h.alloc_huge_in(shared, size, align, a),
        None => shared.alloc_huge(size, align, a),
    }
}

/// Routing of a `MALLOCX` free (`dallocx`, `sdallocx`) on the calling
/// thread. Aborts unless `p` is a live allocation.
#[cfg(target_os = "linux")]
pub fn free(shared: &Shared, p: *mut u8, tcache: Tcache) {
    match tcache {
        Tcache::Default => crate::heap::free(shared, p),
        Tcache::Explicit(t) => match shared.tcaches.get(t) {
            Some(h) => h.free(shared, p),
            None => invalid_tcache(t),
        },
        Tcache::None => crate::heap::free_direct(shared, p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_decoding_matches_jemalloc() {
        let f = Flags::decode(0);
        assert_eq!(f, Flags { lg_align: 0, zero: false, tcache: Tcache::Default, arena: None });
        assert_eq!(f.align(), 1);
        let f = Flags::decode(mallocx_lg_align(12) | MALLOCX_ZERO);
        assert_eq!(f.lg_align, 12);
        assert_eq!(f.align(), 4096);
        assert!(f.zero);
        assert_eq!(Flags::decode(MALLOCX_TCACHE_NONE).tcache, Tcache::None);
        assert_eq!(MALLOCX_TCACHE_NONE, 0x100);
        assert_eq!(Flags::decode(mallocx_tcache(0)).tcache, Tcache::Explicit(0));
        assert_eq!(Flags::decode(mallocx_tcache(7)).tcache, Tcache::Explicit(7));
        assert_eq!(mallocx_tcache(7), 9 << 8);
        assert_eq!(Flags::decode(mallocx_arena(0)).arena, Some(0));
        assert_eq!(Flags::decode(mallocx_arena(3)).arena, Some(3));
        assert_eq!(mallocx_arena(3), 4 << 20);
        assert_eq!(Flags::decode(mallocx_arena(4094)).arena, Some(4094));
        // Every field at once, as Valkey's Lua arena code combines them.
        let f = Flags::decode(mallocx_arena(1) | MALLOCX_TCACHE_NONE | mallocx_lg_align(4) | MALLOCX_ZERO);
        assert_eq!(f, Flags { lg_align: 4, zero: true, tcache: Tcache::None, arena: Some(1) });
        // Bit 7 is unused and ignored.
        assert_eq!(Flags::decode(0x80), Flags::decode(0));
    }
}
