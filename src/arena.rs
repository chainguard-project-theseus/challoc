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
#[cfg(verus_only)]
use crate::layout::process_regions;
use crate::layout::{NCLASSES, Regions};
use crate::lock::Lock;
use crate::meta::Meta;
use crate::stats::ArenaStats;
use crate::vspec::addr::addr_to_ptr;
#[cfg(verus_only)]
use crate::vspec::arena::axiom_arena_stats_lock_inv;
use vstd::prelude::*;
#[cfg(verus_only)]
use vstd::raw_ptr::PointsToRaw;
use vstd::raw_ptr::{PointsTo, ptr_mut_write, ptr_ref};

verus! {

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
    /// Ghost: the permission to the table at `sets`.
    #[allow(dead_code)]
    sets_perm: Tracked<PointsTo<SetTable>>,
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

} // verus!

// SAFETY: `sets` points at a never-freed metadata object whose only
// mutable state is behind its own lock (`SetTable::tables`); everything
// else in `Arena` is atomics, locks and `ClassState`s, which are `Sync`
// and `Send`. Sharing or moving an `Arena` between threads therefore
// hands out nothing but that pointer to a process-lifetime, locked table.
unsafe impl Sync for Arena {}
unsafe impl Send for Arena {}

verus! {

/// The slab set table's permission of a block of `raw` at `p`, when there
/// is a block (as `huge::desc_perm` for descriptors).
proof fn table_perm(tracked raw: PointsToRaw, p: *mut SetTable) -> (tracked r: Option<PointsTo<SetTable>>)
    ensures
        p@.addr != 0 && raw.is_range(p@.addr as int, vstd::layout::size_of::<SetTable>() as int)
            && raw.provenance() == p@.provenance && p@.addr as int % vstd::layout::align_of::<SetTable>() as int == 0
            ==> r is Some && r->0.ptr() == p && r->0.is_uninit(),
{
    if p@.addr != 0 && raw.is_range(p@.addr as int, vstd::layout::size_of::<SetTable>() as int)
        && raw.provenance() == p@.provenance && p@.addr as int % vstd::layout::align_of::<SetTable>() as int == 0 {
        let tracked perm = raw.into_typed::<SetTable>(p@.addr);
        assert(perm.ptr()@ == p@);
        Some(perm)
    } else {
        None
    }
}

/// The in-place writes of [`Arena::init`], each trusted: Verus has no
/// raw-pointer field projection. Each writes the fields it names and
/// nothing else; `this` must be the storage `init` was handed.
impl Arena {
    /// `index` and `decay_ns`.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_head(this: *mut Arena, index: u16, decay_ns: u64) {
        // SAFETY: `init`'s caller guarantees `this` is writable and unaliased.
        unsafe {
            ptr::write(&raw mut (*this).index, index);
            ptr::write(&raw mut (*this).decay_ns, AtomicU64::new(decay_ns));
        }
    }

    /// Storage for the slab set table from `meta`, null when it is full,
    /// with the permission to it. The table fits an object (the check is
    /// on constants, so the build folds it away).
    #[inline(always)]
    fn alloc_sets(meta: &Meta) -> (r: (*mut SetTable, Tracked<Option<PointsTo<SetTable>>>))
        ensures
            r.0@.addr != 0 ==> r.1@ is Some && r.1@->0.ptr() == r.0 && r.1@->0.is_uninit(),
    {
        let layout = core::alloc::Layout::new::<SetTable>();
        crate::vspec::check::check(
            layout.size() <= crate::meta::MAX_BINNED && layout.align() <= crate::meta::MAX_ALIGN,
            "arena: slab set table larger than an object",
        );
        let (p, Tracked(raw)) = meta.alloc_raw(layout);
        let p = p as *mut SetTable;
        let tracked perm = table_perm(raw, p);
        (p, Tracked(perm))
    }

    /// `sets_ptr` into the field and its permission `perm` into
    /// `sets_perm`; the table, where it lives.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_sets<'a>(this: *mut Arena, sets_ptr: *mut SetTable, perm: Tracked<PointsTo<SetTable>>) -> (r: &'a SetTable)
        requires
            perm@.ptr() == sets_ptr,
            perm@.is_init(),
        ensures
            *r == perm@.value(),
    {
        // SAFETY: `sets_ptr` is the metadata object the table was just
        // written to; `this` as in `put_head`.
        unsafe {
            ptr::write(&raw mut (*this).sets, sets_ptr as *const SetTable);
            ptr::write(&raw mut (*this).sets_perm, perm);
            &*sets_ptr
        }
    }

    /// `cs` into `classes[c]`. The precondition is the type invariant's
    /// conjunct for `c`: [`Arena::init`] proves it for every state it
    /// writes, and writes every class before it returns `true`.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_class(this: *mut Arena, c: usize, cs: ClassState)
        requires
            c < NCLASSES,
            cs.spec_class() == c,
    {
        // SAFETY: as in `put_head`.
        unsafe {
            ptr::write(&raw mut (*this).classes[c], cs);
        }
    }

    /// `large`, `stats` and `last_exit_purge`.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn put_tail(this: *mut Arena, index: u16) {
        // SAFETY: as in `put_head`.
        unsafe {
            ptr::write(&raw mut (*this).large, AtomicU64::new(0));
            ptr::write(&raw mut (*this).stats, Lock::new(ArenaStats::empty(index)));
            ptr::write(&raw mut (*this).last_exit_purge, AtomicU64::new(0));
        }
    }

    /// Initialise `this` in place as arena `index` over `tables`. Returns
    /// `false` if the metadata region cannot hold the slab sets, leaving
    /// `this` unusable. Verified apart from the writes above: every
    /// `classes[c]` is written, with a state of class `c` (the type
    /// invariant), before `true` is returned.
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
    ) -> bool
        requires
            regions.wf(),
            *regions == process_regions(),
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] tables[c].spec_class() == c,
    {
        // SAFETY: the caller guarantees `this` is writable and unaliased.
        unsafe {
            Arena::put_head(this, index, decay_ns);
            let (sets_ptr, Tracked(perm)) = Arena::alloc_sets(meta);
            if sets_ptr.is_null() {
                return false;
            }
            let sets = match SetTable::new(meta, regions) {
                Some(sets) => {
                    let tracked mut perm = perm.tracked_unwrap();
                    ptr_mut_write(sets_ptr, Tracked(&mut perm), sets);
                    Arena::put_sets(this, sets_ptr, Tracked(perm))
                },
                None => return false,
            };
            for c in 0..NCLASSES
                invariant
                    sets.over(process_regions()),
                    forall|c: int| 0 <= c < NCLASSES ==> #[trigger] tables[c].spec_class() == c,
            {
                match ClassState::new(meta, regions, &tables[c], index, sets) {
                    Some(cs) => Arena::put_class(this, c, cs),
                    None => return false,
                }
            }
            Arena::put_tail(this, index);
        }
        true
    }

    /// The arena's slab set table (fork handlers, tests).
    #[inline]
    pub fn sets(&self) -> &SetTable {
        proof {
            use_type_invariant(self);
        }
        // Written by `init` to a live, never-freed metadata object.
        ptr_ref(self.sets, Tracked(self.sets_perm.borrow()))
    }
}

} // verus!

verus! {

impl Arena {
    /// `classes[c]` is the state of class `c`. `init` builds the value in
    /// place, so no struct expression checks this: `init` proves that it
    /// writes every `classes[c]` with a state of class `c` (the
    /// precondition of `Arena::put_class`, from `ClassState::new`'s
    /// postcondition over `tables[c]`, whose class is `c` by `init`'s
    /// precondition) before it returns `true`; its callers, `Shared::init`
    /// and `Arenas::build`, meet those preconditions. `sets_perm` is the
    /// initialised table at `sets` (the precondition of `Arena::put_sets`).
    /// Trusted: the in-place writers write what they are handed and
    /// nothing else.
    #[verifier::type_invariant]
    spec fn inv(self) -> bool {
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.classes[c].spec_class() == c
        &&& self.sets_perm@.ptr() == self.sets
        &&& self.sets_perm@.is_init()
    }

    /// `Empty` bytes over every class (one lock hold per class that has
    /// any), for the cgroup policy's retention figure.
    pub fn empty_bytes(&self) -> (r: usize)
        ensures
            r <= NCLASSES * 0x200_0000_0000_0000,
    {
        let mut sum = 0usize;
        let mut c = 0;
        while c < NCLASSES
            invariant
                c <= NCLASSES,
                sum <= c * 0x200_0000_0000_0000,
            decreases NCLASSES - c,
        {
            let cs = &self.classes[c];
            if cs.oldest_empty() != 0 {
                sum += cs.empty_bytes();
            }
            c += 1;
        }
        sum
    }

    /// Purge every `Empty` slab of every class whatever its age. Returns
    /// how many were purged.
    pub fn purge_all(&self) -> (r: usize)
        ensures
            r <= NCLASSES * 0x2000_0000_0000,
    {
        let mut sum = 0usize;
        let mut c = 0;
        while c < NCLASSES
            invariant
                c <= NCLASSES,
                sum <= c * 0x2000_0000_0000,
            decreases NCLASSES - c,
        {
            let cs = &self.classes[c];
            cs.nslabs_fit();
            sum += cs.purge_empty(u64::MAX);
            c += 1;
        }
        sum
    }
}

} // verus!

verus! {

impl Arena {
    /// One run in the `large` word; the low 32 bits count pages (a run's
    /// `pages` is a `u32` in its descriptor, and the region holds fewer
    /// than 2^32 pages in all).
    const LARGE_RUN: u64 = 1 << 32;

    /// The `large` word for one run of `pages` (guard included).
    fn large_word(pages: usize) -> u64
        requires
            pages < 0x1_0000_0000,
    {
        proof {
            assert((1usize << 32usize) == 0x1_0000_0000usize) by (bit_vector);
        }
        assert!(pages < (1 << 32), "arena: huge run of {pages} pages");
        Self::LARGE_RUN | pages as u64
    }

    /// A huge run of `pages` (guard included) was allocated through this
    /// arena.
    pub fn note_huge_alloc(&self, pages: usize)
        requires
            pages < 0x1_0000_0000,
    {
        self.large.fetch_add(Self::large_word(pages), Ordering::Relaxed);
    }

    /// A huge run of `pages` (guard included) of this arena was freed.
    pub fn note_huge_free(&self, pages: usize)
        requires
            pages < 0x1_0000_0000,
    {
        self.large.fetch_sub(Self::large_word(pages), Ordering::Relaxed);
    }

    /// Live huge runs of this arena and their pages, guards included, as
    /// one snapshot (see the field).
    pub fn large(&self) -> (usize, usize) {
        proof {
            assert(Self::LARGE_RUN == 0x1_0000_0000) by (compute_only);
        }
        let w = self.large.load(Ordering::Relaxed);
        ((w >> 32) as usize, (w & (Self::LARGE_RUN - 1)) as usize)
    }

    /// The statistics stored by the last `epoch`.
    pub fn stats(&self) -> ArenaStats {
        *self.stats.lock()
    }

    /// Store the statistics of an `epoch`.
    pub fn store_stats(&self, s: &ArenaStats) {
        broadcast use axiom_arena_stats_lock_inv;

        *self.stats.lock() = *s;
    }

    /// The statistics lock, for the fork handlers (a leaf: never held
    /// while another lock is taken).
    pub fn stats_raw_lock(&self) -> &crate::lock::RawLock {
        self.stats.raw()
    }

    pub fn index(&self) -> u16 {
        self.index
    }

    /// Class state for `class`, without the bounds check the fast paths do
    /// not need (`class` comes from `class_of`).
    #[inline(always)]
    pub fn class(&self, class: usize) -> (r: &ClassState)
        requires
            class < NCLASSES,
        ensures
            r.spec_class() == class,
    {
        proof {
            use_type_invariant(self);
        }
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

    /// Decay step over every class (see `ClassState::decay`): the earliest
    /// next deadline, `None` when nothing is `Empty` or decay is off.
    pub fn decay(&self, now: u64) -> Option<u64> {
        self.decay_with(now, self.decay_ns(), crate::policy::KEEP_ALL)
    }

    /// [`decay`](Arena::decay) with the period the cgroup policy gives
    /// this arena and its keep ratio for the retention cap (design 11);
    /// `NEVER` does nothing.
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
}

} // verus!

verus! {

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
    /// Thread heaps currently homed on each arena (`assign`/`bind` add,
    /// `unbind` subtracts); explicit caches are not counted.
    bound: [AtomicU32; MAX_ARENAS],
    table: [AtomicPtr<Arena>; MAX_ARENAS],
}

} // verus!

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
            let table = &raw mut (*this).table;
            let bound = &raw mut (*this).bound;
            for i in 0..MAX_ARENAS {
                ptr::write(&raw mut (*table)[i], AtomicPtr::new(ptr::null_mut()));
                ptr::write(&raw mut (*bound)[i], AtomicU32::new(0));
            }
            (*table)[0].store(arena0, Ordering::Release);
        }
    }

    /// Every arena that exists, in index order (`Shared::set_decay_ms`,
    /// `policy::empty_bytes`).
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

verus! {

/// The arena a non-null registry entry points at. Trusted: Verus has no
/// raw-pointer dereference. Entries are published by `build` with a
/// release store after `Arena::init`, and arenas are never freed.
///
/// # Safety
/// `p` is a non-null entry loaded from `Arenas::table`.
#[verifier::external_body]
#[inline(always)]
unsafe fn arena_at<'a>(p: *mut Arena) -> &'a Arena {
    // SAFETY: guaranteed by the caller.
    unsafe { &*p }
}

/// An arena's bound count went below zero: a heap was unbound twice, or
/// never bound. Trusted: `panic!` formats its message.
#[cold]
#[inline(never)]
#[verifier::external_body]
fn unbind_unbound(index: u16) -> ! {
    panic!("arena {index}: unbinding a heap that was never bound")
}

impl Arenas {
    /// A thread heap homed on `index` is destroyed or moves away.
    pub fn unbind(&self, index: u16)
        requires
            (index as usize) < MAX_ARENAS,
    {
        let prev = self.bound[index as usize].fetch_sub(1, Ordering::Relaxed);
        if prev == 0 {
            unbind_unbound(index);
        }
    }

    /// Arena `index` if it exists. An automatic index whose arena has not
    /// been built yet is built here (cold); an explicit index that
    /// `create` has not handed out, or one beyond the table, is `None`.
    #[inline]
    pub fn get<'a>(&'a self, shared: &'a Shared, index: u16) -> Option<&'a Arena>
        requires
            shared.wf(),
    {
        let i = index as usize;
        if i >= MAX_ARENAS {
            return None;
        }
        let p = self.table[i].load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: arenas are never freed; the pointer was published
            // after `Arena::init` with a release store.
            return Some(unsafe { arena_at(p) });
        }
        self.materialise(shared, index)
    }

    #[cold]
    #[inline(never)]
    fn materialise<'a>(&'a self, shared: &'a Shared, index: u16) -> Option<&'a Arena>
        requires
            (index as usize) < MAX_ARENAS,
            shared.wf(),
    {
        if index as u32 >= self.auto {
            return None;
        }
        let _g = self.lock.lock();
        let p = self.table[index as usize].load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: as in `get`.
            return Some(unsafe { arena_at(p) });
        }
        self.build(shared, index)
    }

    /// Build arena `index` in the metadata region and publish it. Called
    /// with the creation lock held and the slot known to be empty.
    /// `shared` is the published `Shared`, whose `wf` gives
    /// `Arena::init`'s preconditions (the process's well-formed regions and
    /// the tables in class order).
    fn build<'a>(&'a self, shared: &'a Shared, index: u16) -> Option<&'a Arena>
        requires
            (index as usize) < MAX_ARENAS,
            shared.wf(),
    {
        let layout = core::alloc::Layout::new::<Arena>();
        proof {
            shared.lemma_wf();
            let lg = shared.regions.lg_page;
            assert((1usize << lg) == (1usize << (lg as usize))) by (bit_vector)
                requires
                    lg < 64,
            ;
            crate::meta::lemma_is_pow2_shl(lg as usize);
        }
        // Never freed, so a reservation (page aligned, which covers the
        // 128-byte class state alignment) rather than an object.
        let p = addr_to_ptr(shared.meta.reserve_committed(
            layout.size(),
            layout.align().max(shared.regions.page_size()),
        )?) as *mut Arena;
        // SAFETY: fresh, zeroed, exclusively owned storage of the right
        // size and alignment; the tables live in `Shared` for the process.
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
            // The reservation is lost; the metadata region is exhausted
            // anyway.
            return None;
        }
        shared.set_multi_arena();
        self.table[index as usize].store(p, Ordering::Release);
        // SAFETY: just built and published.
        Some(unsafe { arena_at(p) })
    }

    /// `arenas.create`: a new explicit arena. `None` when the table or the
    /// metadata region is full.
    pub fn create(&self, shared: &Shared) -> Option<u16>
        requires
            shared.wf(),
    {
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

    /// Arena `index` if it has been built, without building it.
    pub fn get_existing(&self, index: u16) -> Option<&Arena> {
        let i = index as usize;
        if i >= MAX_ARENAS {
            return None;
        }
        let p = self.table[i].load(Ordering::Acquire);
        // SAFETY: a non-null entry of the table.
        if p.is_null() { None } else { Some(unsafe { arena_at(p) }) }
    }

    /// Automatic arenas, as `init` clamped them.
    pub closed spec fn spec_auto(&self) -> int {
        self.auto as int
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
    pub fn assign(&self) -> (r: u16)
        requires
            self.spec_auto() <= MAX_ARENAS,
        ensures
            r < MAX_ARENAS,
    {
        if self.auto == 1 {
            self.bound[0].fetch_add(1, Ordering::Relaxed);
            return 0;
        }
        let _g = self.lock.lock();
        let mut best = 0usize;
        // (bound heaps, not yet built): the smallest wins.
        let mut best_key = (u32::MAX, true);
        for i in 0..self.auto as usize
            invariant
                self.auto <= MAX_ARENAS,
                best < MAX_ARENAS,
        {
            let key = (self.bound[i].load(Ordering::Relaxed), self.table[i].load(Ordering::Relaxed).is_null());
            if key < best_key {
                best = i;
                best_key = key;
                if key == (0, false) {
                    break;
                }
            }
        }
        self.bound[best].fetch_add(1, Ordering::Relaxed);
        best as u16
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

    /// Count a thread heap as homed on `index` (`thread.arena` moving a
    /// heap to an arena `assign` did not pick).
    pub fn bind(&self, index: u16)
        requires
            index < MAX_ARENAS,
    {
        self.bound[index as usize].fetch_add(1, Ordering::Relaxed);
    }

    /// Thread heaps currently homed on `index`.
    pub fn bound(&self, index: u16) -> u32
        requires
            index < MAX_ARENAS,
    {
        self.bound[index as usize].load(Ordering::Relaxed)
    }

    /// The creation lock, for fork handlers (taken before every class lock).
    pub fn raw_lock(&self) -> &crate::lock::RawLock {
        self.lock.raw()
    }
}

} // verus!
