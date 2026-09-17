//! Statistics (design.md section 13.4): the per-class, per-arena and
//! process-wide figures behind `stats.*`, `malloc_stats_print` and
//! `experimental.utilization`, and the `epoch` that refreshes them.
//!
//! Nothing here is counted on the fast paths. The inputs are counters the
//! slow paths already maintain: each `ClassState` keeps its slab counts
//! under the class lock and `claimed - released` (see `class.rs`) gives
//! `curregs`, the slots out of the bitmaps; the huge allocator keeps its
//! extent counts under its lock and each `Arena` counts the runs allocated
//! through it; the metadata allocator counts its committed bytes. An
//! [`epoch`] reads all of them once, stores an [`ArenaStats`] in every
//! arena and a [`Snapshot`] in `Shared`, and every `stats.*` read returns
//! those stored values until the next epoch, as in jemalloc. The `collect_*`
//! functions compute the same figures without storing them, for
//! `malloc_stats_print` and tests.
//!
//! Definitions, from design 13.4:
//!
//! - `allocated`: the sum over classes of `curregs * size` (slots in
//!   thread caches count, as in jemalloc) plus the usable bytes of live
//!   huge runs.
//! - `active`: bytes of committed slabs that are neither `Empty` nor
//!   `Purged` plus the usable bytes of live huge runs.
//! - `resident`: `active` plus the bytes of `Empty` slabs and extents plus
//!   committed metadata.
//! - `retained`: bytes of `Purged` slabs and extents.
//! - `mapped`: `resident + retained`.
//!
//! Guard slabs and guard pages hold no memory and are counted nowhere. The
//! huge allocator's free extents (`Empty`, `Purged`) and the metadata region
//! belong to no arena and are attributed to arena 0, as `arena.0.purge` is;
//! so the arena figures sum to the totals.
//!
//! The identities `resident >= active >= allocated` and
//! `mapped == resident + retained` hold for every snapshot by construction:
//! each bin's `curregs` is clamped to the capacity of the slabs that can
//! hold live slots (`ClassState::bin_stats`), a huge run's allocated and
//! active bytes are the same number, and the totals are sums of the same
//! per-arena values.

use crate::arena::Arena;
use crate::class::ClassState;
use crate::heap::Shared;
use crate::huge::{GUARD_PAGES, HugeStats};
use crate::layout::{NBINS, NCLASSES};
use crate::meta::MetaStats;

/// One `(arena, class)` pair (`stats.arenas.<i>.bins.<j>.*` and the
/// `arenas.bin.<j>.*` geometry).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BinStats {
    /// Class size in bytes.
    pub size: usize,
    /// Slots per slab.
    pub nregs: usize,
    /// Slab size in bytes.
    pub slab_size: usize,
    /// Slots out of the bitmaps: in a cache or in use.
    pub curregs: usize,
    /// Slabs that are neither `Empty` nor `Purged`.
    pub curslabs: usize,
    /// Slabs in the partial set.
    pub nonfull_slabs: usize,
    /// `Empty` slabs (dirty, resident).
    pub empty_slabs: usize,
    /// `Purged` slabs (retained).
    pub purged_slabs: usize,
    /// Slots claimed by cache refills (`ClassState::refilled`).
    pub refilled: u64,
    /// Slots claimed by compaction rebuilds (`ClassState::compacted`).
    pub compacted: u64,
    /// Caches that entered compact mode for this bin.
    pub compact_on: u64,
    /// Caches that left compact mode for this bin.
    pub compact_off: u64,
    /// Probes the refill-rate gate declined.
    pub gate_refill: u64,
}

impl BinStats {
    pub const EMPTY: BinStats = BinStats {
        size: 0,
        nregs: 0,
        slab_size: 0,
        curregs: 0,
        curslabs: 0,
        nonfull_slabs: 0,
        empty_slabs: 0,
        purged_slabs: 0,
        refilled: 0,
        compacted: 0,
        compact_on: 0,
        compact_off: 0,
        gate_refill: 0,
    };

    /// Bytes allocated: `curregs * size`.
    #[inline]
    pub fn allocated(&self) -> usize {
        self.curregs * self.size
    }

    /// Active bytes: `curslabs * slab_size`.
    #[inline]
    pub fn active(&self) -> usize {
        self.curslabs * self.slab_size
    }

    /// Dirty bytes: `Empty` slabs.
    #[inline]
    pub fn dirty(&self) -> usize {
        self.empty_slabs * self.slab_size
    }

    /// Retained bytes: `Purged` slabs.
    #[inline]
    pub fn retained(&self) -> usize {
        self.purged_slabs * self.slab_size
    }
}

/// One arena (`stats.arenas.<i>.*`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ArenaStats {
    pub index: u16,
    /// `stats.arenas.<i>.small.allocated`: sum of `curregs * size` over the
    /// bins (classes up to `layout::BIN_MAX`).
    pub small_allocated: usize,
    /// Bytes of bin slabs neither `Empty` nor `Purged`.
    pub small_active: usize,
    /// Bytes of `Empty` bin slabs.
    pub small_dirty: usize,
    /// Bytes of `Purged` bin slabs.
    pub small_retained: usize,
    /// `stats.arenas.<i>.large.allocated`: `curregs * size` of the classes
    /// above `BIN_MAX` plus the usable bytes of live huge runs allocated
    /// through this arena.
    pub large_allocated: usize,
    /// Active bytes of the classes above `BIN_MAX` (slabs neither `Empty`
    /// nor `Purged`) plus the usable bytes of live huge runs.
    pub large_active: usize,
    /// Bytes of `Empty` slabs of the classes above `BIN_MAX`.
    pub large_dirty: usize,
    /// Bytes of `Purged` slabs of the classes above `BIN_MAX`.
    pub large_retained: usize,
    /// Live huge runs allocated through this arena.
    pub large_runs: usize,
    /// Bytes of `Empty` huge extents; arena 0 only.
    pub huge_dirty: usize,
    /// Bytes of `Purged` huge extents; arena 0 only.
    pub huge_retained: usize,
    /// Committed metadata bytes; arena 0 only.
    pub metadata: usize,
    /// `stats.arenas.<i>.pactive`: active pages.
    pub pactive: usize,
    /// `stats.arenas.<i>.pdirty`: dirty pages (slabs and, for arena 0,
    /// huge extents).
    pub pdirty: usize,
    /// `stats.arenas.<i>.pmuzzy`: always 0 (design section 8).
    pub pmuzzy: usize,
    /// `active + dirty + metadata`.
    pub resident: usize,
    /// `Purged` bytes.
    pub retained: usize,
    /// `resident + retained`.
    pub mapped: usize,
    pub bins: [BinStats; NCLASSES],
}

impl ArenaStats {
    /// All zero, for `index`.
    pub const fn empty(index: u16) -> ArenaStats {
        ArenaStats {
            index,
            small_allocated: 0,
            small_active: 0,
            small_dirty: 0,
            small_retained: 0,
            large_allocated: 0,
            large_active: 0,
            large_dirty: 0,
            large_retained: 0,
            large_runs: 0,
            huge_dirty: 0,
            huge_retained: 0,
            metadata: 0,
            pactive: 0,
            pdirty: 0,
            pmuzzy: 0,
            resident: 0,
            retained: 0,
            mapped: 0,
            bins: [BinStats::EMPTY; NCLASSES],
        }
    }

    /// `allocated` for this arena: small plus large.
    #[inline]
    pub fn allocated(&self) -> usize {
        self.small_allocated + self.large_allocated
    }

    /// `active` for this arena: small plus large.
    #[inline]
    pub fn active(&self) -> usize {
        self.small_active + self.large_active
    }
}

/// Process-wide totals (`stats.allocated` and friends).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub allocated: usize,
    pub active: usize,
    pub metadata: usize,
    pub resident: usize,
    pub retained: usize,
    pub mapped: usize,
}

impl Totals {
    pub const EMPTY: Totals = Totals { allocated: 0, active: 0, metadata: 0, resident: 0, retained: 0, mapped: 0 };

    /// The accounting identities every snapshot satisfies:
    /// `resident >= active >= allocated` and `mapped == resident + retained`.
    pub fn identities_hold(&self) -> bool {
        self.resident >= self.active && self.active >= self.allocated && self.mapped == self.resident + self.retained
    }

    fn add_arena(&mut self, a: &ArenaStats) {
        self.allocated += a.allocated();
        self.active += a.active();
        self.metadata += a.metadata;
        self.resident += a.resident;
        self.retained += a.retained;
        self.mapped += a.mapped;
    }
}

/// What an [`epoch`] stores in `Shared`: the totals plus the raw huge and
/// metadata counters for `malloc_stats_print`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Number of epochs so far; 0 before the first.
    pub epoch: u64,
    /// Arenas that existed at the epoch (`arenas.narenas` at the time).
    pub narenas: u32,
    pub totals: Totals,
    pub huge: HugeStats,
    pub meta: MetaStats,
}

impl Snapshot {
    pub const EMPTY: Snapshot = Snapshot {
        epoch: 0,
        narenas: 0,
        totals: Totals::EMPTY,
        huge: HugeStats { runs: 0, run_pages: 0, empty: 0, empty_pages: 0, purged: 0, purged_pages: 0, next_page: 0, table_cap: 0 },
        meta: MetaStats { len: 0, committed: 0, allocated: 0, bumped: 0, reserved: 0 },
    };
}

impl ArenaStats {
    /// Add every figure of `o` to `self` (the `MALLCTL_ARENAS_ALL` merge
    /// of `stats.arenas.4096.*`). The bin geometry (`size`, `nregs`,
    /// `slab_size`) is the same for every arena and is taken from `o`
    /// where `self` has none yet.
    pub fn add(&mut self, o: &ArenaStats) {
        self.small_allocated += o.small_allocated;
        self.small_active += o.small_active;
        self.small_dirty += o.small_dirty;
        self.small_retained += o.small_retained;
        self.large_allocated += o.large_allocated;
        self.large_active += o.large_active;
        self.large_dirty += o.large_dirty;
        self.large_retained += o.large_retained;
        self.large_runs += o.large_runs;
        self.huge_dirty += o.huge_dirty;
        self.huge_retained += o.huge_retained;
        self.metadata += o.metadata;
        self.pactive += o.pactive;
        self.pdirty += o.pdirty;
        self.pmuzzy += o.pmuzzy;
        self.resident += o.resident;
        self.retained += o.retained;
        self.mapped += o.mapped;
        for (b, ob) in self.bins.iter_mut().zip(o.bins.iter()) {
            if b.size == 0 {
                b.size = ob.size;
                b.nregs = ob.nregs;
                b.slab_size = ob.slab_size;
            }
            b.curregs += ob.curregs;
            b.curslabs += ob.curslabs;
            b.nonfull_slabs += ob.nonfull_slabs;
            b.empty_slabs += ob.empty_slabs;
            b.purged_slabs += ob.purged_slabs;
            b.refilled += ob.refilled;
            b.compacted += ob.compacted;
            b.compact_on += ob.compact_on;
            b.compact_off += ob.compact_off;
            b.gate_refill += ob.gate_refill;
        }
    }
}

/// The current figures of one class state (one lock hold).
pub fn collect_bin(cs: &ClassState) -> BinStats {
    let (s, curregs) = cs.bin_stats();
    let cls = cs.class();
    let (compacted, compact_on, compact_off, gate_refill) = cs.compaction_stats();
    BinStats {
        size: cls.size(),
        nregs: cls.nslots(),
        slab_size: cls.slab_size(),
        curregs,
        curslabs: s.committed - s.empty - s.purged,
        nonfull_slabs: s.nonfull,
        empty_slabs: s.empty,
        purged_slabs: s.purged,
        refilled: cs.refilled(),
        compacted,
        compact_on,
        compact_off,
        gate_refill,
    }
}

/// The current figures of `arena`. `huge` and `meta` are the process-wide
/// counters the caller read (they are folded into arena 0 only).
fn collect_arena_with(shared: &Shared, arena: &Arena, huge: &HugeStats, meta: &MetaStats) -> ArenaStats {
    let page = shared.regions.page_size();
    let mut a = ArenaStats::empty(arena.index());
    for (c, bin) in a.bins.iter_mut().enumerate() {
        *bin = collect_bin(arena.class(c));
        if c < NBINS {
            a.small_allocated += bin.allocated();
            a.small_active += bin.active();
            a.small_dirty += bin.dirty();
            a.small_retained += bin.retained();
        } else {
            // Slab-allocated, but reported as large (see `layout::BIN_MAX`).
            a.large_allocated += bin.allocated();
            a.large_active += bin.active();
            a.large_dirty += bin.dirty();
            a.large_retained += bin.retained();
        }
    }
    let (runs, pages) = arena.large();
    let huge_usable = pages.saturating_sub(runs * GUARD_PAGES) * page;
    a.large_runs = runs;
    a.large_allocated += huge_usable;
    a.large_active += huge_usable;
    if arena.index() == 0 {
        a.huge_dirty = huge.empty_pages * page;
        a.huge_retained = huge.purged_pages * page;
        a.metadata = meta.committed;
    }
    let dirty = a.small_dirty + a.large_dirty + a.huge_dirty;
    a.pactive = a.active() / page;
    a.pdirty = dirty / page;
    a.pmuzzy = 0;
    a.resident = a.active() + dirty + a.metadata;
    a.retained = a.small_retained + a.large_retained + a.huge_retained;
    a.mapped = a.resident + a.retained;
    a
}

/// The current figures of `arena`, not stored.
pub fn collect_arena(shared: &Shared, arena: &Arena) -> ArenaStats {
    collect_arena_with(shared, arena, &shared.huge.stats(), &shared.meta.stats())
}

/// Read every counter once; store the result per arena and in `Shared`
/// when `store` is set. Returns the snapshot (with the new epoch number
/// when stored, the current one otherwise).
fn collect_into(shared: &Shared, store: bool) -> Snapshot {
    let huge = shared.huge.stats();
    let meta = shared.meta.stats();
    let mut totals = Totals::EMPTY;
    let mut narenas = 0;
    for arena in shared.arenas.iter() {
        let a = collect_arena_with(shared, arena, &huge, &meta);
        totals.add_arena(&a);
        narenas += 1;
        if store {
            arena.store_stats(&a);
        }
    }
    let mut g = shared.stats.lock();
    if store {
        g.epoch += 1;
        g.narenas = narenas;
        g.totals = totals;
        g.huge = huge;
        g.meta = meta;
        *g
    } else {
        Snapshot { epoch: g.epoch, narenas, totals, huge, meta }
    }
}

/// `epoch`: refresh every arena's stored [`ArenaStats`] and the process
/// [`Snapshot`]. Returns the new epoch number.
pub fn epoch(shared: &Shared) -> u64 {
    collect_into(shared, true).epoch
}

/// The current totals without storing them (for `malloc_stats_print`).
pub fn collect(shared: &Shared) -> Snapshot {
    collect_into(shared, false)
}

/// The snapshot stored by the last [`epoch`] (all zero before the first).
pub fn snapshot(shared: &Shared) -> Snapshot {
    *shared.stats.lock()
}

/// The [`ArenaStats`] stored by the last [`epoch`] for arena `index`, or
/// `None` if the arena does not exist. An arena created since the last
/// epoch reports zeros.
pub fn arena_stats(shared: &Shared, index: u16) -> Option<ArenaStats> {
    Some(shared.arenas.get_existing(index)?.stats())
}

/// `stats.arenas.4096.*` (`MALLCTL_ARENAS_ALL`): every existing arena's
/// stored figures summed. Its `index` is 4096.
pub fn arena_stats_all(shared: &Shared) -> ArenaStats {
    let mut all = ArenaStats::empty(crate::arena::ARENAS_ALL as u16);
    for arena in shared.arenas.iter() {
        all.add(&arena.stats());
    }
    all
}

/// Every existing arena's *current* figures summed, not stored (for
/// `mallinfo` and `malloc_stats`, which glibc defines as live).
pub fn collect_all(shared: &Shared) -> ArenaStats {
    let huge = shared.huge.stats();
    let meta = shared.meta.stats();
    let mut all = ArenaStats::empty(crate::arena::ARENAS_ALL as u16);
    for arena in shared.arenas.iter() {
        all.add(&collect_arena_with(shared, arena, &huge, &meta));
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::tests::{TestHeap, class_with, live, shared};
    use crate::layout::CLASSES;
    use std::vec::Vec;

    const PAGE: usize = 4096;

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    /// Before the first epoch everything is zero; after it the totals are
    /// the metadata alone. Allocations do not show until the next epoch.
    /// Through allocate, free + flush and purge the identities hold and
    /// every figure is what the slab and run states say.
    #[test]
    fn identities_hold_through_the_lifecycle() {
        let s = shared();
        assert_eq!(snapshot(s), Snapshot::EMPTY);
        assert_eq!(epoch(s), 1);
        let first = snapshot(s);
        assert_eq!(first.epoch, 1);
        assert_eq!(first.narenas, 1);
        assert_eq!(first.totals.allocated, 0);
        assert_eq!(first.totals.active, 0);
        assert!(first.totals.metadata > 0, "the slab tables and sets are committed metadata");
        assert_eq!(first.totals.resident, first.totals.metadata);
        assert_eq!(first.totals.retained, 0);
        assert_eq!(first.totals.mapped, first.totals.resident);
        assert!(first.totals.identities_hold());
        assert_eq!(first.meta, s.meta.stats());

        let t = TestHeap::new(s);
        let c16 = class_with(16);
        let c256 = class_with(256);
        let mut ps: Vec<*mut u8> = (0..100).map(|_| t.alloc(c16)).collect();
        ps.extend((0..10).map(|_| t.alloc(c256)));
        // A class above BIN_MAX: slab-allocated, reported as large.
        let cmed = class_with(32 * 1024);
        assert!(cmed >= NBINS && cmed < NCLASSES);
        let pm = t.alloc(cmed);
        let med_slab = CLASSES[cmed].slab_size();
        let huge_size = 300 * 1024;
        let (h, _) = t.alloc_huge(huge_size, 16);
        assert!(!h.is_null());
        let usable = s.huge.usable_for(huge_size).unwrap();
        let run_pages = s.huge.usable_pages(huge_size).unwrap() + GUARD_PAGES;

        // Frozen until the next epoch; `collect` sees the live figures.
        assert_eq!(snapshot(s), first);
        assert_eq!(arena_stats(s, 0).unwrap().small_allocated, 0);
        assert!(collect(s).totals.allocated > 0);
        assert_eq!(collect(s).epoch, 1);

        assert_eq!(epoch(s), 2);
        let a = arena_stats(s, 0).unwrap();
        let tot = snapshot(s).totals;
        assert_eq!(a.index, 0);
        let b16 = a.bins[c16];
        assert_eq!((b16.size, b16.nregs, b16.slab_size), (16, CLASSES[c16].nslots(), CLASSES[c16].slab_size()));
        assert_eq!(b16.curregs, live(s, c16), "curregs is the sum of live");
        assert!(b16.curregs >= 100, "the refill batch counts: {}", b16.curregs);
        assert_eq!((b16.curslabs, b16.nonfull_slabs, b16.empty_slabs, b16.purged_slabs), (1, 1, 0, 0));
        let b256 = a.bins[c256];
        assert_eq!(b256.curregs, live(s, c256));
        // The 256-byte refill (256 slots) fits in one 256-slot slab; keep
        // the count derived so the test survives a geometry change.
        let slabs256 = b256.curslabs;
        assert!(slabs256 >= 1 && (slabs256 - 1) * CLASSES[c256].nslots() < b256.curregs, "{slabs256} slabs");
        assert_eq!(a.small_allocated, live(s, c16) * 16 + live(s, c256) * 256);
        assert_eq!(a.small_active, CLASSES[c16].slab_size() + slabs256 * CLASSES[c256].slab_size());
        assert_eq!((a.small_dirty, a.small_retained), (0, 0));
        let bmed = a.bins[cmed];
        assert_eq!((bmed.curregs, bmed.curslabs), (live(s, cmed), 1));
        assert!(bmed.curregs >= 1);
        assert_eq!(a.large_runs, 1);
        assert_eq!(usable, (run_pages - GUARD_PAGES) * PAGE);
        assert_eq!(a.large_allocated, usable + live(s, cmed) * CLASSES[cmed].size(), "classes above BIN_MAX are large");
        assert_eq!(a.large_active, usable + med_slab);
        assert_eq!((a.large_dirty, a.large_retained, a.huge_dirty, a.huge_retained), (0, 0, 0, 0));
        assert_eq!(a.pactive, (a.small_active + a.large_active) / PAGE);
        assert_eq!((a.pdirty, a.pmuzzy), (0, 0));
        assert_eq!(a.resident, a.small_active + a.large_active + a.metadata);
        assert_eq!(a.mapped, a.resident);
        assert_eq!(tot.allocated, a.small_allocated + a.large_allocated);
        assert_eq!(tot.active, a.small_active + a.large_active);
        assert_eq!(tot.metadata, s.meta.stats().committed);
        assert_eq!(tot.resident, tot.active + tot.metadata);
        assert_eq!(tot.retained, 0);
        assert!(tot.identities_hold(), "{tot:?}");
        assert_eq!(collect(s).totals, tot, "nothing moved: live and stored agree");

        // Free everything and flush: the slabs and the run go Empty.
        for &p in &ps {
            t.free(p);
        }
        t.free(h);
        t.free(pm);
        t.flush_all();
        assert_eq!(epoch(s), 3);
        let a = arena_stats(s, 0).unwrap();
        let tot = snapshot(s).totals;
        assert_eq!((tot.allocated, tot.active), (0, 0));
        let b16 = a.bins[c16];
        assert_eq!((b16.curregs, b16.curslabs, b16.nonfull_slabs, b16.empty_slabs, b16.purged_slabs), (0, 0, 0, 1, 0));
        assert_eq!(a.bins[c256].empty_slabs, slabs256);
        assert_eq!(a.small_dirty, CLASSES[c16].slab_size() + slabs256 * CLASSES[c256].slab_size());
        assert_eq!((a.large_runs, a.large_allocated, a.large_active), (0, 0, 0));
        assert_eq!(a.large_dirty, med_slab, "the medium slab went Empty");
        assert_eq!(a.huge_dirty, run_pages * PAGE);
        assert_eq!(a.pdirty, (a.small_dirty + a.large_dirty + a.huge_dirty) / PAGE);
        assert_eq!(tot.resident, tot.metadata + a.small_dirty + a.large_dirty + a.huge_dirty);
        assert_eq!(tot.retained, 0);
        assert_eq!(tot.mapped, tot.resident);
        assert!(tot.identities_hold(), "{tot:?}");
        let dirty = a.small_dirty + a.large_dirty + a.huge_dirty;

        // Purge: dirty becomes retained; resident falls back to metadata.
        assert!(crate::purge::purge_all(s) > 0);
        assert_eq!(epoch(s), 4);
        let a = arena_stats(s, 0).unwrap();
        let tot = snapshot(s).totals;
        assert_eq!((a.small_dirty, a.large_dirty, a.huge_dirty), (0, 0, 0));
        assert_eq!(a.small_retained + a.large_retained + a.huge_retained, dirty);
        assert_eq!(a.bins[c16].purged_slabs, 1);
        assert_eq!(tot.retained, dirty);
        assert_eq!(tot.resident, tot.metadata);
        assert_eq!(tot.mapped, tot.resident + tot.retained);
        assert!(tot.identities_hold(), "{tot:?}");
        t.destroy();
    }

    /// `curregs` from the claimed/released counters equals the sum of
    /// `live` over the slabs after any single-threaded sequence of
    /// allocations, frees, flushes and GC.
    #[test]
    fn curregs_matches_live_under_random_traffic() {
        let s = shared();
        let t = TestHeap::new(s);
        let classes = [class_with(16), class_with(64), class_with(1024)];
        let mut held: Vec<Vec<*mut u8>> = classes.iter().map(|_| Vec::new()).collect();
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        for step in 0..20_000 {
            let r = xorshift(&mut rng);
            let k = (r % 3) as usize;
            let c = classes[k];
            if (r >> 8) % 5 < 3 || held[k].is_empty() {
                held[k].push(t.alloc(c));
            } else {
                let i = ((r >> 16) as usize) % held[k].len();
                let p = held[k].swap_remove(i);
                t.free(p);
            }
            if step % 977 == 0 {
                t.flush_all();
            }
            if step % 251 == 0 {
                for (k, &c) in classes.iter().enumerate() {
                    let cs = s.arena0.class(c);
                    assert_eq!(cs.curregs(), live(s, c), "class {c} at step {step}");
                    assert!(cs.curregs() >= held[k].len(), "the cache's slots count too");
                    let bin = collect_bin(cs);
                    assert_eq!(bin.curregs, cs.curregs());
                    assert!(bin.curregs <= bin.curslabs * bin.nregs);
                }
            }
        }
        for v in &held {
            for &p in v {
                t.free(p);
            }
        }
        t.destroy();
        for &c in &classes {
            let cs = s.arena0.class(c);
            assert_eq!(cs.curregs(), 0);
            assert_eq!(live(s, c), 0);
            assert_eq!(collect_bin(cs).curslabs, 0);
        }
        let tot = collect(s).totals;
        assert_eq!(tot.allocated, 0);
        assert!(tot.identities_hold(), "{tot:?}");
    }

    /// With a second arena the per-arena figures split by owner: slabs and
    /// runs count where they were allocated, while the huge free extents
    /// and the metadata go to arena 0. The arenas sum to the totals.
    #[test]
    fn arenas_split_by_owner_and_sum_to_the_totals() {
        let s = shared();
        let t = TestHeap::new(s);
        let a1 = s.arenas.create(s).expect("arena 1");
        assert_eq!(a1, 1);
        let c32 = class_with(32);
        let p0 = t.alloc(c32);
        let (p1, _) = t.heap().alloc_in(s, a1, c32);
        assert!(!p1.is_null());
        let size = 400 * 1024;
        let (h1, _) = t.heap().alloc_huge_in(s, size, 16, a1);
        assert!(!h1.is_null());
        let usable = s.huge.usable_for(size).unwrap();

        epoch(s);
        let snap = snapshot(s);
        assert_eq!(snap.narenas, 2);
        let s0 = arena_stats(s, 0).unwrap();
        let s1 = arena_stats(s, 1).unwrap();
        assert!(arena_stats(s, 2).is_none());
        assert_eq!((s0.index, s1.index), (0, 1));
        assert_eq!(s0.bins[c32].curslabs, 1);
        assert_eq!(s1.bins[c32].curslabs, 1);
        assert_eq!(s1.bins[c32].curregs, s.arena(1).unwrap().class(c32).curregs());
        assert_eq!((s0.large_runs, s0.large_allocated), (0, 0));
        assert_eq!((s1.large_runs, s1.large_allocated), (1, usable));
        assert_eq!((s1.huge_dirty, s1.huge_retained, s1.metadata), (0, 0, 0));
        assert_eq!(s0.metadata, snap.meta.committed);
        let sum = |f: fn(&ArenaStats) -> usize| f(&s0) + f(&s1);
        assert_eq!(snap.totals.allocated, sum(|a| a.allocated()));
        assert_eq!(snap.totals.active, sum(|a| a.active()));
        assert_eq!(snap.totals.resident, sum(|a| a.resident));
        assert_eq!(snap.totals.retained, sum(|a| a.retained));
        assert_eq!(snap.totals.mapped, sum(|a| a.mapped));
        assert!(snap.totals.identities_hold());

        // Freeing arena 1's run leaves an Empty extent that arena 0 reports.
        t.free(h1);
        t.free(p1);
        t.free(p0);
        t.flush_all();
        epoch(s);
        let s0 = arena_stats(s, 0).unwrap();
        let s1 = arena_stats(s, 1).unwrap();
        assert_eq!((s1.large_runs, s1.large_allocated), (0, 0));
        assert_eq!(s0.huge_dirty, (s.huge.usable_pages(size).unwrap() + GUARD_PAGES) * PAGE);
        assert_eq!(s1.huge_dirty, 0);
        assert_eq!(s1.bins[c32].empty_slabs, 1);
        assert_eq!(s0.bins[c32].empty_slabs, 1);
        let tot = snapshot(s).totals;
        assert_eq!(tot.resident, s0.resident + s1.resident);
        assert!(tot.identities_hold(), "{tot:?}");
        t.destroy();
    }

    /// `BinStats` and `Totals` helpers.
    #[test]
    fn helpers_and_identity_check() {
        let b = BinStats {
            size: 48,
            nregs: 1365,
            slab_size: 65536,
            curregs: 10,
            curslabs: 2,
            nonfull_slabs: 1,
            empty_slabs: 3,
            purged_slabs: 4,
            ..BinStats::EMPTY
        };
        assert_eq!(b.allocated(), 480);
        assert_eq!(b.active(), 2 * 65536);
        assert_eq!(b.dirty(), 3 * 65536);
        assert_eq!(b.retained(), 4 * 65536);
        let ok = Totals { allocated: 1, active: 2, metadata: 1, resident: 3, retained: 4, mapped: 7 };
        assert!(ok.identities_hold());
        assert!(!Totals { mapped: 8, ..ok }.identities_hold());
        assert!(!Totals { allocated: 3, ..ok }.identities_hold());
        assert!(!Totals { resident: 1, mapped: 5, ..ok }.identities_hold());
        assert_eq!(ArenaStats::empty(7).index, 7);
        assert_eq!(ArenaStats::empty(7).bins[0], BinStats::EMPTY);
    }
}
