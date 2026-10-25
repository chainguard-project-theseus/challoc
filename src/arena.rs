//! Arenas (design.md section 13.3). Explicit caches are
//! `heap/tcaches.rs`; the decoding of `MALLOCX_*` flags and their routing
//! onto heaps, caches and the direct paths (13.2) is `heap/mallocx.rs`.
//!
//! An [`Arena`] is one [`ClassState`] per size class over the shared class
//! regions (every arena's states for class `c` point at the same
//! `SlabTable`, so slabs of different arenas interleave in the region and
//! each descriptor records its owner) plus its own decay period. Arena 0 is
//! built inside `Shared` at init; the [`Arenas`] registry hands out the
//! others. Indices below `narenas` (the `malloc_conf` key; the default
//! resolves to `min(4 * ncpus, 64)`, `Config::effective_narenas`) are
//! *automatic*: a new thread heap is bound to the one with the fewest
//! bound heaps ([`Arenas::assign`], jemalloc's `arena_choose_hard`), the
//! binding is dropped when the heap is destroyed, and each arena is
//! materialised the first time something touches it. So a program with
//! `t` threads alive at once uses `min(t, narenas)` arenas, a
//! single-threaded program never leaves arena 0, and thread churn reuses
//! the arena an exited thread left instead of walking on to a fresh one
//! (round robin materialised every arena under churn and left a partial
//! slab per class in each: larson1 67 MiB against 13.6).
//! Indices from
//! `narenas` up are created explicitly by `arenas.create`. Arenas are
//! never destroyed (`arena.<i>.destroy` purges and retires one); they live
//! in the metadata region.
//!
//! Memory never migrates between arenas: a slot is claimed from and
//! released to the state of the arena that committed its slab, and a
//! thread's heap keeps one cache per `(arena, class)` it has touched
//! (`heap/`). Once a second arena exists `Shared::multi_arena` is set and
//! the `free` fast path reads the slab group's word in the class's owner
//! array (`SlabTable`, never the descriptor) to pick the cache; with one
//! arena that load is skipped, since every slab and every heap is arena 0.

use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};

use crate::class::{ClassState, SetTable, SlabTable};
use crate::heap::Shared;
use crate::layout::{NCLASSES, Regions};
use crate::lock::Lock;
use crate::meta::Meta;
use crate::stats::ArenaStats;

/// Arena indices are the 12-bit `MALLOCX_ARENA` field; 4096 itself is
/// `MALLCTL_ARENAS_ALL`.
pub const MAX_ARENAS: usize = 4096;
/// `MALLCTL_ARENAS_ALL`.
pub const ARENAS_ALL: u32 = 4096;

/// One arena (design 13.3): a class state per size class and a decay
/// period. Built in place because the states borrow the tables.
#[repr(C)]
pub struct Arena {
    index: u16,
    /// Decay period (`arena.<i>.dirty_decay_ms`) in nanoseconds;
    /// `purge::NEVER` for `-1`.
    decay_ns: AtomicU64,
    /// The slab sets of every class, one interleaved table per slab size
    /// (`class/`), a separate metadata object so that `Arena` keeps its
    /// size and the offsets of everything allocated after it.
    sets: *const SetTable,
    pub classes: [ClassState; NCLASSES],
    /// Live huge runs allocated through this arena and their pages (guard
    /// included), kept by `Shared::alloc_huge` / `free_huge` since the huge
    /// allocator itself is shared by every arena (`stats.arenas.<i>.large`).
    /// One word, runs in the high half and pages in the low
    /// ([`Arena::LARGE_RUN`]), so a reader's `(runs, pages)` is always a
    /// pair one run's add or subtract produced together and `pages >=
    /// runs * GUARD_PAGES` holds for every snapshot (`stats.rs` subtracts
    /// the guards exactly).
    large: AtomicU64,
    /// This arena's statistics as of the last `epoch` (`stats.rs`).
    stats: Lock<ArenaStats>,
    /// When the last exit purge ran ([`Arena::take_exit_purge`]), 0 before
    /// the first.
    last_exit_purge: AtomicU64,
}

// SAFETY: `sets` points at a never-freed metadata object whose only
// mutable state is behind its own lock (`SetTable::tables`); everything
// else in `Arena` is atomics, locks and `ClassState`s, which are `Sync`
// and `Send`. Sharing or moving an `Arena` between threads therefore
// hands out nothing but that pointer to a process-lifetime, locked table.
unsafe impl Sync for Arena {}
unsafe impl Send for Arena {}

/// Makes the next [`Arena::init`] over the metadata allocator at this
/// address fail before it allocates anything, as an exhausted region
/// would, for tests.
#[cfg(test)]
pub static FAIL_INIT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// Field and element pointers for the in-place builds (`Arena::init`,
// `Arenas::init`). A field's pointer is the struct's pointer advanced by
// the field's byte offset (`offset_of!`, which compiles to the same
// address as `&raw mut (*p).f`), an element's the array's advanced by its
// index.

/// `p`'s address plus `off` bytes as a `*mut T`, with `p`'s provenance:
/// the pointer `&raw mut (*p).f` gives for a field `f` of type `T` at
/// byte offset `off`.
///
/// # Safety
/// `off` is inside the allocation `p` points into. A caller that writes
/// through the result also needs a `T` at `off` to lie inside that
/// allocation, aligned for `T`: the `fits` assertions below check it for
/// every field offset this file uses.
#[inline(always)]
pub(crate) unsafe fn field_at<S, T>(p: *mut S, off: usize) -> *mut T {
    // SAFETY: `off` bytes from `p` stay inside its allocation.
    unsafe { (p as *mut u8).add(off) as *mut T }
}

/// Element `i` of the `n` from `base`: `base.add(i)`, the address of the
/// array index projection.
///
/// # Safety
/// The `n` elements are inside one allocation and `i < n`.
#[allow(unused_variables)]
#[inline(always)]
pub(crate) unsafe fn elem_at<T>(base: *mut T, i: usize, n: usize) -> *mut T {
    // SAFETY: element `i < n` is inside the allocation.
    unsafe { base.add(i) }
}

// Byte offsets of `Arena`'s fields.
const A_INDEX: usize = core::mem::offset_of!(Arena, index);
const A_DECAY_NS: usize = core::mem::offset_of!(Arena, decay_ns);
const A_SETS: usize = core::mem::offset_of!(Arena, sets);
const A_CLASSES: usize = core::mem::offset_of!(Arena, classes);
const A_LARGE: usize = core::mem::offset_of!(Arena, large);
const A_STATS: usize = core::mem::offset_of!(Arena, stats);
const A_LAST_EXIT_PURGE: usize = core::mem::offset_of!(Arena, last_exit_purge);

/// A field of type `T` at offset `off` of an `S` is aligned for `T`, `S`'s
/// alignment is a multiple of `T`'s, and the field ends by `next` (the
/// next field's offset, or the size of `S`). An overflow in the `const`
/// assertions that evaluate it is a compile error.
pub(crate) const fn fits<S, T>(off: usize, next: usize) -> bool {
    off % core::mem::align_of::<T>() == 0
        && core::mem::align_of::<S>() % core::mem::align_of::<T>() == 0
        && off + core::mem::size_of::<T>() <= next
}

// `Arena`'s layout (`repr(C)`): each field at its offset, aligned, and
// ending by the next one's offset, the last by the size.
const _: () = assert!(
    fits::<Arena, u16>(A_INDEX, A_DECAY_NS)
        && fits::<Arena, AtomicU64>(A_DECAY_NS, A_SETS)
        && fits::<Arena, *const SetTable>(A_SETS, A_CLASSES)
        && fits::<Arena, [ClassState; NCLASSES]>(A_CLASSES, A_LARGE)
        && NCLASSES as u128 * core::mem::size_of::<ClassState>() as u128 <= isize::MAX as u128
        && fits::<Arena, AtomicU64>(A_LARGE, A_STATS)
        && fits::<Arena, Lock<ArenaStats>>(A_STATS, A_LAST_EXIT_PURGE)
        && fits::<Arena, AtomicU64>(A_LAST_EXIT_PURGE, core::mem::size_of::<Arena>())
        && core::mem::size_of::<Arena>() <= isize::MAX as usize
);

// An array of `N` `T`s is `N` `T`s back to back, aligned as one, at
// `Arena`'s class states.
const _: () = assert!(
    core::mem::size_of::<[ClassState; NCLASSES]>() == NCLASSES * core::mem::size_of::<ClassState>()
        && core::mem::align_of::<[ClassState; NCLASSES]>() == core::mem::align_of::<ClassState>()
);

/// The in-place writes of [`Arena::init`], each to the field it names.
impl Arena {
    /// `index` and `decay_ns`.
    ///
    /// # Safety
    /// `this` is `init`'s storage.
    #[inline(always)]
    unsafe fn put_head(this: *mut Arena, index: u16, decay_ns: u64) {
        // SAFETY: the fields of `init`'s storage.
        unsafe {
            ptr::write(field_at(this, A_INDEX), index);
            ptr::write(field_at(this, A_DECAY_NS), AtomicU64::new(decay_ns));
        }
    }

    /// Storage for the slab set table from `meta`, null when it is full.
    #[inline(always)]
    fn alloc_sets(meta: &Meta) -> *mut SetTable {
        meta.alloc(core::alloc::Layout::new::<SetTable>()) as *mut SetTable
    }

    /// `sets_ptr` into the field.
    ///
    /// # Safety
    /// `this` is `init`'s storage.
    #[inline(always)]
    unsafe fn put_sets(this: *mut Arena, sets_ptr: *mut SetTable) {
        // SAFETY: the field of `init`'s storage.
        unsafe {
            ptr::write(field_at(this, A_SETS), sets_ptr as *const SetTable);
        }
    }

    /// `cs` into `classes[c]`.
    ///
    /// # Safety
    /// `this` is `init`'s storage and `c < NCLASSES`.
    #[inline(always)]
    unsafe fn put_class(this: *mut Arena, c: usize, cs: ClassState) {
        // SAFETY: element `c` of the field.
        unsafe {
            let classes: *mut ClassState = field_at(this, A_CLASSES);
            ptr::write(elem_at(classes, c, NCLASSES), cs);
        }
    }

    /// `large`, `stats` and `last_exit_purge`.
    ///
    /// # Safety
    /// `this` is `init`'s storage.
    #[inline(always)]
    unsafe fn put_tail(this: *mut Arena, index: u16) {
        // SAFETY: the fields of `init`'s storage.
        unsafe {
            ptr::write(field_at(this, A_LARGE), AtomicU64::new(0));
            ptr::write(field_at(this, A_STATS), Lock::new(ArenaStats::empty(index)));
            ptr::write(field_at(this, A_LAST_EXIT_PURGE), AtomicU64::new(0));
        }
    }

    /// Initialise `this` in place as arena `index` over `tables`. Returns
    /// `false` if the metadata region cannot hold the slab sets, leaving
    /// `this` unusable until a later `init` over the same storage
    /// succeeds.
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
            Arena::put_head(this, index, decay_ns);
            #[cfg(test)]
            if FAIL_INIT
                .compare_exchange(
                    meta as *const Meta as usize,
                    0,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return false;
            }
            let sets_ptr = Arena::alloc_sets(meta);
            if sets_ptr.is_null() {
                return false;
            }
            match SetTable::new(meta, regions) {
                Some(sets) => {
                    ptr::write(sets_ptr, sets);
                    Arena::put_sets(this, sets_ptr);
                }
                None => {
                    meta.free(sets_ptr as *mut u8, core::alloc::Layout::new::<SetTable>());
                    return false;
                }
            }
            let sets: &SetTable = &*sets_ptr;
            for c in 0..NCLASSES {
                match ClassState::new(meta, regions, &tables[c], index, sets) {
                    Some(cs) => {
                        Arena::put_class(this, c, cs);
                    }
                    None => {
                        return false;
                    }
                }
            }
            Arena::put_tail(this, index);
        }
        true
    }

    /// One run in the `large` word; the low 32 bits count pages (a run's
    /// `pages` is a `u32` in its descriptor, and the region holds fewer
    /// than 2^32 pages in all).
    const LARGE_RUN: u64 = 1 << 32;

    /// The `large` word for one run of `pages` (guard included).
    fn large_word(pages: usize) -> u64 {
        assert!(pages < (1 << 32), "arena: huge run of {pages} pages");
        Self::LARGE_RUN | pages as u64
    }

    /// A huge run of `pages` (guard included) was allocated through this
    /// arena.
    pub fn note_huge_alloc(&self, pages: usize) {
        self.large
            .fetch_add(Self::large_word(pages), Ordering::Relaxed);
    }

    /// A huge run of `pages` (guard included) of this arena was freed.
    pub fn note_huge_free(&self, pages: usize) {
        self.large
            .fetch_sub(Self::large_word(pages), Ordering::Relaxed);
    }

    /// Live huge runs of this arena and their pages, guards included, as
    /// one snapshot (see the field).
    pub fn large(&self) -> (usize, usize) {
        let w = self.large.load(Ordering::Relaxed);
        ((w >> 32) as usize, (w & (Self::LARGE_RUN - 1)) as usize)
    }

    /// The statistics stored by the last `epoch`.
    pub fn stats(&self) -> ArenaStats {
        *self.stats.lock()
    }

    /// Store the statistics of an `epoch`.
    pub fn store_stats(&self, s: &ArenaStats) {
        *self.stats.lock() = *s;
    }

    /// The statistics lock, for the fork handlers (a leaf: no lock is
    /// taken while it is held; `epoch` takes it under the process
    /// statistics lock).
    pub fn stats_raw_lock(&self) -> &crate::lock::RawLock {
        self.stats.raw()
    }

    #[inline]
    /// The arena's slab set table (fork handlers, tests).
    pub fn sets(&self) -> &SetTable {
        // SAFETY: written by `init` to a live, never-freed metadata object.
        unsafe { &*self.sets }
    }

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
        self.decay_ns
            .store(crate::purge::decay_ns_from_ms(ms), Ordering::Relaxed);
        crate::purge::note_decay_changed();
    }

    /// Decay step over every class (see `ClassState::decay_with`) with
    /// the period the cgroup policy gives this arena and its keep ratio
    /// for the retention cap (design 11): the earliest next deadline,
    /// `None` when nothing is `Empty`; `NEVER` does nothing.
    pub fn decay_with(&self, now: u64, decay_ns: u64, keep16: u32) -> Option<u64> {
        if decay_ns == crate::purge::NEVER {
            return None;
        }
        let mut next: Option<u64> = None;
        for cs in self.classes.iter() {
            if let Some(d) = cs.decay_with(now, decay_ns, keep16) {
                next = Some(next.map_or(d, |n| n.min(d)));
            }
        }
        next
    }

    /// `Empty` bytes over every class (one lock hold per class that has
    /// any), for the cgroup policy's retention figure.
    pub fn empty_bytes(&self) -> usize {
        let mut sum = 0usize;
        let mut c = 0;
        while c < NCLASSES {
            let cs = &self.classes[c];
            if cs.oldest_empty() != 0 {
                sum += cs.empty_bytes();
            }
            c += 1;
        }
        sum
    }

    /// The exit purge's gate (design 13.3): the last thread heap homed on
    /// this arena is leaving at `now`; `true` when its `Empty` slabs
    /// should be purged now, which is at most once per decay period of
    /// the arena and never with decay off (`-1`). Thread churn (a pool
    /// that retires and recreates its threads, larson, mstress) would
    /// otherwise purge and refault the same slabs at every exit; a
    /// process whose workers leave once (the Ruby soak's end) purges
    /// once. Called under no lock; two exits racing on one arena within
    /// a period both see the old stamp and both purge, which is harmless.
    pub fn take_exit_purge(&self, now: u64) -> bool {
        let period = self.decay_ns();
        if period == crate::purge::NEVER {
            return false;
        }
        let last = self.last_exit_purge.load(Ordering::Relaxed);
        if last != 0 && now.wrapping_sub(last) < period {
            return false;
        }
        self.last_exit_purge.store(now.max(1), Ordering::Relaxed);
        true
    }

    /// Purge every `Empty` slab of every class whatever its age. Returns
    /// how many were purged.
    pub fn purge_all(&self) -> usize {
        let mut sum = 0usize;
        let mut c = 0;
        while c < NCLASSES {
            let cs = &self.classes[c];
            sum += cs.purge_empty(u64::MAX);
            c += 1;
        }
        sum
    }
}

/// The arena registry. Index 0 is `Shared::arena0`; the rest live in the
/// metadata region. `count` is the number of indices in use: the automatic
/// ones from init plus every `create` since, published with a release
/// store once the arena is usable (automatic arenas may still be
/// unmaterialised, see [`Arenas::get`]).
///
/// `repr(C)`: the fields lie in declaration order, so the layout
/// assertion below holds on every compiler.
#[repr(C)]
pub struct Arenas {
    table: [AtomicPtr<Arena>; MAX_ARENAS],
    /// Thread heaps currently homed on each arena (`assign`/`bind` add,
    /// `unbind` subtracts); explicit caches are not counted.
    bound: [AtomicU32; MAX_ARENAS],
    /// Creation lock; the value is `count` for the writer.
    lock: Lock<u32>,
    /// The storage of a build whose `Arena::init` failed, for the next
    /// build to reuse: a reservation is never freed, so a retry under
    /// metadata pressure would otherwise strand one per attempt. Null
    /// when there is none; only touched under `lock`.
    spare: AtomicPtr<Arena>,
    count: AtomicU32,
    /// Indices below this are automatic (`narenas`).
    auto: u32,
}

// Byte offsets of `Arenas`'s fields.
const AR_TABLE: usize = core::mem::offset_of!(Arenas, table);
const AR_BOUND: usize = core::mem::offset_of!(Arenas, bound);
const AR_LOCK: usize = core::mem::offset_of!(Arenas, lock);
const AR_COUNT: usize = core::mem::offset_of!(Arenas, count);
const AR_AUTO: usize = core::mem::offset_of!(Arenas, auto);
const AR_SPARE: usize = core::mem::offset_of!(Arenas, spare);

// `Arenas`'s layout: each field at its offset, aligned, and ending by the
// next one's offset, the last by the size. `Arenas` is `repr(C)`, which
// fixes the fields in declaration order (the table, the bound counts,
// `lock`, `spare`, `count`, `auto`), so this holds on every compiler.
const _: () = assert!(
    fits::<Arenas, [AtomicPtr<Arena>; MAX_ARENAS]>(AR_TABLE, AR_BOUND)
        && MAX_ARENAS as u128 * core::mem::size_of::<AtomicPtr<Arena>>() as u128
            <= isize::MAX as u128
        && fits::<Arenas, [AtomicU32; MAX_ARENAS]>(AR_BOUND, AR_LOCK)
        && MAX_ARENAS as u128 * core::mem::size_of::<AtomicU32>() as u128 <= isize::MAX as u128
        && fits::<Arenas, Lock<u32>>(AR_LOCK, AR_SPARE)
        && fits::<Arenas, AtomicPtr<Arena>>(AR_SPARE, AR_COUNT)
        && fits::<Arenas, AtomicU32>(AR_COUNT, AR_AUTO)
        && fits::<Arenas, u32>(AR_AUTO, core::mem::size_of::<Arenas>())
        && core::mem::size_of::<Arenas>() <= isize::MAX as usize
);

// An array of `N` `T`s is `N` `T`s back to back, aligned as one, at
// `Arenas`'s two arrays.
const _: () = assert!(
    core::mem::size_of::<[AtomicPtr<Arena>; MAX_ARENAS]>()
        == MAX_ARENAS * core::mem::size_of::<AtomicPtr<Arena>>()
        && core::mem::align_of::<[AtomicPtr<Arena>; MAX_ARENAS]>()
            == core::mem::align_of::<AtomicPtr<Arena>>()
        && core::mem::size_of::<[AtomicU32; MAX_ARENAS]>()
            == MAX_ARENAS * core::mem::size_of::<AtomicU32>()
        && core::mem::align_of::<[AtomicU32; MAX_ARENAS]>() == core::mem::align_of::<AtomicU32>()
);

/// `a < b` on `(u32, bool)`: lexicographic, `false < true`, as `core`
/// documents tuple and `bool` order.
#[inline(always)]
fn key_less(a: (u32, bool), b: (u32, bool)) -> bool {
    a < b
}

/// The in-place writes of [`Arenas::init`], each to the field it names.
impl Arenas {
    /// `lock`, `count` and `auto`, from the automatic count `auto`.
    ///
    /// # Safety
    /// `this` is `init`'s storage.
    #[inline(always)]
    unsafe fn put_head(this: *mut Arenas, auto: u32) {
        // SAFETY: the fields of `init`'s storage.
        unsafe {
            ptr::write(field_at(this, AR_LOCK), Lock::new(auto));
            ptr::write(field_at(this, AR_COUNT), AtomicU32::new(auto));
            ptr::write(field_at(this, AR_AUTO), auto);
        }
    }

    /// Entry `i` of `table`, null, and of `bound`, a count of 0.
    ///
    /// # Safety
    /// `this` is `init`'s storage and `i < MAX_ARENAS`.
    #[inline(always)]
    unsafe fn put_entry(this: *mut Arenas, i: usize) {
        // SAFETY: element `i` of each array field.
        unsafe {
            let table: *mut AtomicPtr<Arena> = field_at(this, AR_TABLE);
            let bound: *mut AtomicU32 = field_at(this, AR_BOUND);
            ptr::write(
                elem_at(table, i, MAX_ARENAS),
                AtomicPtr::new(ptr::null_mut()),
            );
            ptr::write(elem_at(bound, i, MAX_ARENAS), AtomicU32::new(0));
        }
    }

    /// `arena0` into entry 0 of `table`.
    ///
    /// # Safety
    /// `this` is `init`'s storage, its entry 0 written.
    #[inline(always)]
    unsafe fn put_arena0(this: *mut Arenas, arena0: *mut Arena) {
        // SAFETY: element 0 of the field, written by `put_entry`.
        unsafe {
            let table: *mut AtomicPtr<Arena> = field_at(this, AR_TABLE);
            (*elem_at(table, 0, MAX_ARENAS)).store(arena0, Ordering::Release);
        }
    }

    /// `spare`, null.
    ///
    /// # Safety
    /// `this` is `init`'s storage.
    #[inline(always)]
    unsafe fn put_spare(this: *mut Arenas) {
        // SAFETY: the field of `init`'s storage.
        unsafe {
            ptr::write(
                field_at(this, AR_SPARE),
                AtomicPtr::<Arena>::new(ptr::null_mut()),
            );
        }
    }
}

/// An arena's bound count went below zero: a heap was unbound twice, or
/// never bound.
#[cold]
#[inline(never)]
#[track_caller]
fn unbind_unbound(index: u16) -> ! {
    panic!("arena {index}: unbinding a heap that was never bound")
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
            Arenas::put_head(this, auto);
            for i in 0..MAX_ARENAS {
                Arenas::put_entry(this, i);
            }
            Arenas::put_arena0(this, arena0);
            Arenas::put_spare(this);
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

    /// Pick the home arena for a new thread heap and bind the heap to it:
    /// the automatic arena with the fewest bound heaps, ties going to one
    /// that is already built, then to the lowest index. Under the creation
    /// lock so two threads created at once do not both take the same idle
    /// arena. Does not materialise the arena; the heap's first refill does,
    /// which is why a single-threaded program (main thread on arena 0,
    /// nothing else ever bound) never builds a second arena and never
    /// pays the `multi_arena` routing on `free`. The caller undoes the
    /// binding with [`Arenas::unbind`] when the heap is destroyed.
    pub fn assign(&self) -> u16 {
        if self.auto == 1 {
            self.count_in(0);
            return 0;
        }
        let _g = self.lock.lock();
        let mut best = 0usize;
        // (bound heaps, not yet built): the smallest wins.
        let mut best_key = (u32::MAX, true);
        for i in 0..self.auto as usize {
            let key = (
                self.load_bound(i),
                self.table[i].load(Ordering::Relaxed).is_null(),
            );
            if key_less(key, best_key) {
                best = i;
                best_key = key;
                if key == (0, false) {
                    break;
                }
            }
        }
        self.count_in(best);
        best as u16
    }

    /// `bound[i]`.
    #[inline(always)]
    fn load_bound(&self, i: usize) -> u32 {
        self.bound[i].load(Ordering::Relaxed)
    }

    /// `bound[i] += 1`.
    #[inline(always)]
    fn count_in(&self, i: usize) {
        self.bound[i].fetch_add(1, Ordering::Relaxed);
    }

    /// Count a thread heap as homed on `index` (`thread.arena` moving a
    /// heap to an arena `assign` did not pick).
    pub fn bind(&self, index: u16) {
        self.count_in(index as usize)
    }

    /// Bind a thread heap to `index` only if no heap is homed there now
    /// (one compare-and-swap of its count from 0 to 1): the late rehoming
    /// of a young heap onto the arena an exited thread left
    /// (`Heap::try_rehome`). Two heaps racing for one idle arena cannot
    /// both win, so a rehome never stacks a second heap on an arena. A
    /// plain load first: a busy arena (xmalloc's live producers) is
    /// refused without taking its counter's line exclusive.
    pub fn try_bind_idle(&self, index: u16) -> bool {
        let i = index as usize;
        if self.load_bound(i) != 0 {
            return false;
        }
        self.bind_if_idle(i)
    }

    /// `bound[i]`'s compare-and-swap from 0 to 1.
    #[inline(always)]
    fn bind_if_idle(&self, i: usize) -> bool {
        self.bound[i]
            .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    /// A thread heap homed on `index` is destroyed or moves away.
    pub fn unbind(&self, index: u16) {
        let prev = self.bound[index as usize].fetch_sub(1, Ordering::Relaxed);
        if prev == 0 {
            unbind_unbound(index);
        }
    }

    /// Thread heaps currently homed on `index`.
    pub fn bound(&self, index: u16) -> u32 {
        self.load_bound(index as usize)
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
        if p.is_null() {
            None
        } else {
            Some(unsafe { &*p })
        }
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
        let mut p = self.spare.swap(ptr::null_mut(), Ordering::Relaxed);
        if p.is_null() {
            let layout = core::alloc::Layout::new::<Arena>();
            // Never freed, so a reservation (page aligned, which covers the
            // 128-byte class state alignment) rather than an object.
            p = shared.meta.reserve_committed(
                layout.size(),
                layout.align().max(shared.regions.page_size()),
            )? as *mut Arena;
        }
        // SAFETY: exclusively owned storage of the right size and
        // alignment, fresh or left by a failed build (never published;
        // `init` writes every field); the tables live in `Shared` for the
        // process.
        let ok = unsafe {
            Arena::init(
                p,
                &shared.meta,
                &shared.regions,
                shared.tables(),
                index,
                shared.default_decay_ns(),
            )
        };
        if !ok {
            self.spare.store(p, Ordering::Relaxed);
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
            if p.is_null() {
                None
            } else {
                Some(unsafe { &*p })
            }
        })
    }
}
