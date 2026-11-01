//! Statistics (design 13.4): the per-class, per-arena and
//! process-wide figures behind `stats.*`, `malloc_stats_print` and
//! `experimental.utilization`, and the `epoch` that refreshes them.
//!
//! Nothing here is counted on the fast paths. The inputs are counters the
//! slow paths already maintain: each `ClassState` keeps its slab counts
//! under the class lock and `claimed - released` (see `class/`) gives
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
//! so the arena figures of one collection sum to its totals.
//!
//! The identities `resident >= active >= allocated` and
//! `mapped == resident + retained` hold for every snapshot by construction:
//! each bin's `curregs` is clamped to the capacity of the slabs that can
//! hold live slots (`ClassState::bin_stats`, which counts every clamp in
//! the bins table's `clamped` column), a huge run's allocated and active
//! bytes are the same number, and the totals are sums of the same
//! per-arena values. Verus proves them for every stored `ArenaStats` and,
//! with up to 256 arenas, every stored `Snapshot`. Coherence is by
//! construction: an `epoch` holds the snapshot lock across its whole
//! collection, so concurrent epochs publish one whole collection at a time
//! (`concurrent_epochs_leave_arenas_summing_to_the_totals`).

use crate::arena::Arena;
#[cfg(verus_keep_ghost)]
use crate::arena::MAX_ARENAS;
use crate::class::ClassState;
use crate::heap::Shared;
use crate::huge::{GUARD_PAGES, HugeStats};
#[cfg(verus_keep_ghost)]
use crate::layout::class_table;
use crate::layout::{NBINS, NCLASSES};
use crate::lock::LockOrder;
#[cfg(verus_keep_ghost)]
use crate::lock::{rank_arena_stats, rank_class, rank_stats};
use crate::meta::MetaStats;
use crate::vspec::ctl::Change;
use vstd::prelude::*;

pub mod ledger;

verus! {

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
    /// Times `curregs` was clamped to the bin's capacity
    /// (`ClassState::bin_stats`): transient is normal, growing while
    /// nothing is freed is a bookkeeping bug.
    pub clamped: u64,
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
        clamped: 0,
    };

    /// Bytes allocated: `curregs * size`.
    // Only the tests call it: the verified sums add the fields themselves.
    #[allow(dead_code)]
    #[inline]
    pub fn allocated(&self) -> (r: usize)
        requires
            self.curregs * self.size <= usize::MAX,
        ensures
            r == self.curregs * self.size,
    {
        self.curregs * self.size
    }

    /// Active bytes: `curslabs * slab_size`.
    // Only the tests call it: the verified sums add the fields themselves.
    #[allow(dead_code)]
    #[inline]
    pub fn active(&self) -> (r: usize)
        requires
            self.curslabs * self.slab_size <= usize::MAX,
        ensures
            r == self.curslabs * self.slab_size,
    {
        self.curslabs * self.slab_size
    }

    /// Dirty bytes: `Empty` slabs.
    // Only the tests call it: the verified sums add the fields themselves.
    #[allow(dead_code)]
    #[inline]
    pub fn dirty(&self) -> (r: usize)
        requires
            self.empty_slabs * self.slab_size <= usize::MAX,
        ensures
            r == self.empty_slabs * self.slab_size,
    {
        self.empty_slabs * self.slab_size
    }

    /// Retained bytes: `Purged` slabs.
    // Only the tests call it: the verified sums add the fields themselves.
    #[allow(dead_code)]
    #[inline]
    pub fn retained(&self) -> (r: usize)
        requires
            self.purged_slabs * self.slab_size <= usize::MAX,
        ensures
            r == self.purged_slabs * self.slab_size,
    {
        self.purged_slabs * self.slab_size
    }

    /// Every sum the merge of `o` into `self` forms fits its type.
    pub open spec fn can_add(self, o: BinStats) -> bool {
        &&& self.curregs + o.curregs <= usize::MAX
        &&& self.curslabs + o.curslabs <= usize::MAX
        &&& self.nonfull_slabs + o.nonfull_slabs <= usize::MAX
        &&& self.empty_slabs + o.empty_slabs <= usize::MAX
        &&& self.purged_slabs + o.purged_slabs <= usize::MAX
        &&& self.refilled + o.refilled <= u64::MAX
        &&& self.compacted + o.compacted <= u64::MAX
        &&& self.compact_on + o.compact_on <= u64::MAX
        &&& self.compact_off + o.compact_off <= u64::MAX
        &&& self.gate_refill + o.gate_refill <= u64::MAX
        &&& self.clamped + o.clamped <= u64::MAX
    }

    /// `o` merged into `self`: the geometry from `o` where `self` has none,
    /// every count summed.
    pub open spec fn merged(self, o: BinStats) -> BinStats {
        let g = if self.size == 0 { o } else { self };
        BinStats {
            size: g.size,
            nregs: g.nregs,
            slab_size: g.slab_size,
            curregs: (self.curregs + o.curregs) as usize,
            curslabs: (self.curslabs + o.curslabs) as usize,
            nonfull_slabs: (self.nonfull_slabs + o.nonfull_slabs) as usize,
            empty_slabs: (self.empty_slabs + o.empty_slabs) as usize,
            purged_slabs: (self.purged_slabs + o.purged_slabs) as usize,
            refilled: (self.refilled + o.refilled) as u64,
            compacted: (self.compacted + o.compacted) as u64,
            compact_on: (self.compact_on + o.compact_on) as u64,
            compact_off: (self.compact_off + o.compact_off) as u64,
            gate_refill: (self.gate_refill + o.gate_refill) as u64,
            clamped: (self.clamped + o.clamped) as u64,
        }
    }

    /// The bin's figures fit its slabs ([`collect_bin`]): its slots out
    /// fill at most its live-capable slabs (`ClassState::bin_stats`'s
    /// clamp), and all its slabs lie in its class region, at most `2^38`
    /// bytes (`layout::regions::LG_REGION_PREFERRED`).
    pub open spec fn fits(self) -> bool {
        &&& self.curregs * self.size <= self.curslabs * self.slab_size
        &&& (self.curslabs + self.empty_slabs + self.purged_slabs) * self.slab_size <= 0x40_0000_0000
    }
}

/// The byte figures of a bin that [`BinStats::fits`]: `allocated` within
/// `active`, and `active`, dirty and retained together within the region.
pub proof fn lemma_bin_fits(b: BinStats)
    requires
        b.fits(),
    ensures
        0 <= b.curregs * b.size <= b.curslabs * b.slab_size,
        0 <= b.empty_slabs * b.slab_size,
        0 <= b.purged_slabs * b.slab_size,
        b.curslabs * b.slab_size + b.empty_slabs * b.slab_size + b.purged_slabs * b.slab_size <= 0x40_0000_0000,
{
    let (x, z, u, e, p, s) = (
        b.curregs as int,
        b.size as int,
        b.curslabs as int,
        b.empty_slabs as int,
        b.purged_slabs as int,
        b.slab_size as int,
    );
    assert(0 <= x * z && 0 <= e * s && 0 <= p * s && u * s + e * s + p * s == (u + e + p) * s) by (nonlinear_arith)
        requires
            x >= 0,
            z >= 0,
            u >= 0,
            e >= 0,
            p >= 0,
            s >= 0,
    ;
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
    /// `stats.arenas.<i>.pmuzzy`: always 0 (design 8).
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
    pub const fn empty(index: u16) -> (r: ArenaStats)
        ensures
            r.index == index,
            r.small_allocated == 0,
            r.large_allocated == 0,
            r.large_runs == 0,
            r.small_active == 0 && r.small_dirty == 0 && r.small_retained == 0,
            r.large_active == 0 && r.large_dirty == 0 && r.large_retained == 0,
            r.huge_dirty == 0 && r.huge_retained == 0 && r.metadata == 0,
            r.resident == 0 && r.retained == 0 && r.mapped == 0,
            r.identities(),
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] r.bins[c] == BinStats::EMPTY,
    {
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
    // Only the tests call it: the report sums the fields with `fig_add`.
    #[allow(dead_code)]
    #[inline]
    pub fn allocated(&self) -> (r: usize)
        requires
            self.small_allocated + self.large_allocated <= usize::MAX,
        ensures
            r == self.small_allocated + self.large_allocated,
    {
        self.small_allocated + self.large_allocated
    }

    /// `active` for this arena: small plus large.
    // Only the tests call it: the verified sums add the fields themselves.
    #[allow(dead_code)]
    #[inline]
    pub fn active(&self) -> (r: usize)
        requires
            self.small_active + self.large_active <= usize::MAX,
        ensures
            r == self.small_active + self.large_active,
    {
        self.small_active + self.large_active
    }

    /// The accounting identities of one arena's figures (module doc), as
    /// [`collect_arena_with`] forms them: small and large `allocated`
    /// within their `active`, `resident` and `retained` the sums of their
    /// parts and `mapped` theirs. Every part is a natural, so they give
    /// `resident >= active + metadata`, `active >= allocated` and
    /// `mapped == resident + retained`. Every stored `ArenaStats`
    /// satisfies them (the stats lock's invariant,
    /// `vspec::arena::axiom_arena_stats_lock_inv`).
    pub open spec fn identities(self) -> bool {
        &&& self.small_allocated <= self.small_active
        &&& self.large_allocated <= self.large_active
        &&& self.resident == self.small_active + self.large_active + self.small_dirty + self.large_dirty
            + self.huge_dirty + self.metadata
        &&& self.retained == self.small_retained + self.large_retained + self.huge_retained
        &&& self.mapped == self.resident + self.retained
    }
}

/// The most an arena's `mapped` can be: `2^58` for arena 0, which also
/// holds the huge free extents (`u32::MAX` pages at most) and the
/// metadata (below `meta::va_limit`, `2^57`), and `2^55` for any other
/// (its class regions, at most `2^38` bytes each, and the huge runs of
/// its `large` word, below `2^32` pages of at most `MAX_SLAB` bytes).
pub open spec fn arena_mapped_max(index: u16) -> int {
    if index == 0 {
        0x400_0000_0000_0000
    } else {
        0x80_0000_0000_0000
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
    pub const EMPTY: Totals = Totals {
        allocated: 0,
        active: 0,
        metadata: 0,
        resident: 0,
        retained: 0,
        mapped: 0,
    };

    /// The accounting identities of the totals (module doc):
    /// `resident >= active + metadata`, `active >= allocated` and
    /// `mapped == resident + retained`. They hold for the sum of any
    /// figures that satisfy [`ArenaStats::identities`] when that sum
    /// fits ([`collect_into`]).
    pub open spec fn identities(self) -> bool {
        &&& self.allocated <= self.active
        &&& self.active + self.metadata <= self.resident
        &&& self.mapped == self.resident + self.retained
    }

    /// The accounting identities every snapshot satisfies:
    /// `resident >= active >= allocated` and `mapped == resident + retained`.
    #[cfg(test)]
    pub fn identities_hold(&self) -> (r: bool)
        requires
            self.resident + self.retained <= usize::MAX,
        ensures
            r == (self.resident >= self.active && self.active >= self.allocated && self.mapped == self.resident
                + self.retained),
    {
        self.resident >= self.active && self.active >= self.allocated && self.mapped == self.resident + self.retained
    }

    /// Every sum [`Totals::add_arena`] forms fits its type.
    pub open spec fn can_add_arena(self, a: ArenaStats) -> bool {
        &&& a.small_allocated + a.large_allocated <= usize::MAX
        &&& a.small_active + a.large_active <= usize::MAX
        &&& self.allocated + a.small_allocated + a.large_allocated <= usize::MAX
        &&& self.active + a.small_active + a.large_active <= usize::MAX
        &&& self.metadata + a.metadata <= usize::MAX
        &&& self.resident + a.resident <= usize::MAX
        &&& self.retained + a.retained <= usize::MAX
        &&& self.mapped + a.mapped <= usize::MAX
    }

    /// Add arena `a`'s figures. The sums are exact when they fit
    /// ([`fig_add`]).
    fn add_arena(&mut self, a: &ArenaStats)
        ensures
            old(self).can_add_arena(*a) ==> {
                &&& final(self).allocated == old(self).allocated + a.small_allocated + a.large_allocated
                &&& final(self).active == old(self).active + a.small_active + a.large_active
                &&& final(self).metadata == old(self).metadata + a.metadata
                &&& final(self).resident == old(self).resident + a.resident
                &&& final(self).retained == old(self).retained + a.retained
                &&& final(self).mapped == old(self).mapped + a.mapped
            },
            // `allocated` alone, whether or not the other sums fit.
            old(self).allocated + a.small_allocated + a.large_allocated <= usize::MAX ==> final(self).allocated
                == old(self).allocated + a.small_allocated + a.large_allocated,
    {
        self.allocated = fig_add(self.allocated, fig_add(a.small_allocated, a.large_allocated));
        self.active = fig_add(self.active, fig_add(a.small_active, a.large_active));
        self.metadata = fig_add(self.metadata, a.metadata);
        self.resident = fig_add(self.resident, a.resident);
        self.retained = fig_add(self.retained, a.retained);
        self.mapped = fig_add(self.mapped, a.mapped);
    }
}

/// `a + b` for a statistics figure, as the build computes it: a panic on
/// overflow where overflow checks are on (the tests), wrapping where they
/// are off (the shipped library). The figures are counters the slow paths
/// keep in atomics and lock-protected words that no verified invariant
/// bounds, so their sums cannot be proved to fit; the sum is exact
/// whenever it does. Trusted: the spec is `+`'s.
#[verifier::external_body]
#[inline(always)]
#[track_caller]
pub(crate) fn fig_add(a: usize, b: usize) -> (r: usize)
    ensures
        a + b <= usize::MAX ==> r == a + b,
{
    a + b
}

/// `a * b` for a statistics figure; see [`fig_add`].
#[verifier::external_body]
#[inline(always)]
#[track_caller]
pub(crate) fn fig_mul(a: usize, b: usize) -> (r: usize)
    ensures
        a * b <= usize::MAX ==> r == a * b,
{
    a * b
}

/// `a + b` for a statistics counter; see [`fig_add`].
#[verifier::external_body]
#[inline(always)]
#[track_caller]
pub(crate) fn fig_add64(a: u64, b: u64) -> (r: u64)
    ensures
        a + b <= u64::MAX ==> r == a + b,
{
    a + b
}

/// The usable pages of an arena's `runs` live huge runs in `pages` pages,
/// guards included. Every run carries its guard: the pair comes from one
/// atomic word (`Arena::large`), and the pages a run adds are the pages
/// its free subtracts (`Huge::alloc_run` reads them from the descriptor),
/// so a shortfall is a corrupt counter, not a race, and aborts. The guard
/// product cannot overflow (`runs` comes from the high half of that
/// word); `wrapping_mul` is the shipped build's `*`.
#[inline(always)]
fn huge_usable_pages(index: u16, runs: usize, pages: usize) -> (r: usize)
    requires
        crate::sys::may_abort_corrupt() || runs * GUARD_PAGES <= pages,
    ensures
        runs * GUARD_PAGES <= usize::MAX ==> runs * GUARD_PAGES <= pages && r == pages - runs * GUARD_PAGES,
{
    let guards = runs.wrapping_mul(GUARD_PAGES);
    proof {
        if runs * GUARD_PAGES <= usize::MAX {
            vstd::arithmetic::div_mod::lemma_small_mod((runs * GUARD_PAGES) as nat, 0x1_0000_0000_0000_0000nat);
        }
    }
    if pages < guards {
        huge_pages_short(index, runs, pages);
    }
    pages - guards
}

/// The abort of [`huge_usable_pages`].
#[cold]
#[inline(never)]
fn huge_pages_short(index: u16, runs: usize, pages: usize) -> !
    requires
        crate::sys::may_abort_corrupt(),
{
    crate::sys::abort_integrity(format_args!("stats: arena {}: {runs} huge runs in {pages} pages", index))
}

/// What an [`epoch`] stores in `Shared`: the totals plus the raw huge and
/// metadata counters for `malloc_stats_print`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Number of epochs so far; 0 before the first.
    pub epoch: u64,
    /// Arenas the scan found built at the epoch, each counted once: below
    /// `arenas.narenas` while an automatic arena is not built yet.
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
        huge: HugeStats {
            runs: 0,
            run_pages: 0,
            empty: 0,
            empty_pages: 0,
            purged: 0,
            purged_pages: 0,
            next_page: 0,
            table_cap: 0,
        },
        meta: MetaStats {
            len: 0,
            committed: 0,
            allocated: 0,
            bumped: 0,
            reserved: 0,
            writable: 0,
            eager: 0,
        },
    };

    /// What every stored snapshot satisfies (the `shared.stats` lock's
    /// invariant, [`axiom_snapshot_lock_inv`]): with up to
    /// 256 arenas the totals' identities hold. Each arena's `mapped` is
    /// at most [`arena_mapped_max`], so 256 of them sum below `2^64` and
    /// no figure of the totals wraps; past that the identities hold when
    /// the true sums fit (always, for the address space they describe,
    /// but not proved).
    pub open spec fn inv(self) -> bool {
        self.narenas <= 256 ==> self.totals.identities()
    }
}

/// The snapshot an `epoch` stores satisfies [`Snapshot::inv`]. Trusted as
/// every `lock_inv` axiom is: the `shared.stats` lock's writers are
/// `Shared::init` (`Snapshot::EMPTY`, all zero) and [`collect_into`],
/// which asserts it; the fork handlers take only the raw lock.
pub broadcast axiom fn axiom_snapshot_lock_inv(v: Snapshot)
    ensures
        #[trigger] crate::lock::lock_inv(v) == v.inv(),
;

} // verus!

verus! {

impl ArenaStats {
    /// Add every figure of `o` to `self` (the `MALLCTL_ARENAS_ALL` merge
    /// of `stats.arenas.4096.*`). The bin geometry (`size`, `nregs`,
    /// `slab_size`) is the same for every arena and is taken from `o`
    /// where `self` has none yet.
    pub fn add(&mut self, o: &ArenaStats)
        ensures
            final(self).index == old(self).index,
            old(self).can_add(*o) ==> old(self).is_merge(*o, *final(self)),
            // The byte figures, whether or not the counts fit.
            old(self).bytes_fit(*o) ==> old(self).bytes_merged(*o, *final(self)),
            // Each sum the exact `allocated` needs, whether or not the
            // others fit.
            old(self).small_allocated + o.small_allocated <= usize::MAX ==> final(self).small_allocated
                == old(self).small_allocated + o.small_allocated,
            old(self).large_allocated + o.large_allocated <= usize::MAX ==> final(self).large_allocated
                == old(self).large_allocated + o.large_allocated,
            old(self).large_runs + o.large_runs <= usize::MAX ==> final(self).large_runs == old(self).large_runs
                + o.large_runs,
            forall|c: int|
                0 <= c < NCLASSES && old(self).bins[c].curregs + o.bins[c].curregs <= usize::MAX
                    ==> #[trigger] final(self).bins[c].curregs == old(self).bins[c].curregs + o.bins[c].curregs,
    {
        self.small_allocated = fig_add(self.small_allocated, o.small_allocated);
        self.small_active = fig_add(self.small_active, o.small_active);
        self.small_dirty = fig_add(self.small_dirty, o.small_dirty);
        self.small_retained = fig_add(self.small_retained, o.small_retained);
        self.large_allocated = fig_add(self.large_allocated, o.large_allocated);
        self.large_active = fig_add(self.large_active, o.large_active);
        self.large_dirty = fig_add(self.large_dirty, o.large_dirty);
        self.large_retained = fig_add(self.large_retained, o.large_retained);
        self.large_runs = fig_add(self.large_runs, o.large_runs);
        self.huge_dirty = fig_add(self.huge_dirty, o.huge_dirty);
        self.huge_retained = fig_add(self.huge_retained, o.huge_retained);
        self.metadata = fig_add(self.metadata, o.metadata);
        self.pactive = fig_add(self.pactive, o.pactive);
        self.pdirty = fig_add(self.pdirty, o.pdirty);
        self.pmuzzy = fig_add(self.pmuzzy, o.pmuzzy);
        self.resident = fig_add(self.resident, o.resident);
        self.retained = fig_add(self.retained, o.retained);
        self.mapped = fig_add(self.mapped, o.mapped);
        for c in 0..NCLASSES
            invariant
                self.index == old(self).index,
                old(self).can_add(*o) ==> old(self).scalars_merged(*o, *self),
                old(self).bytes_fit(*o) ==> old(self).bytes_merged(*o, *self),
                old(self).can_add(*o) ==> forall|j: int| 0 <= j < c ==> #[trigger] self.bins[j] == old(self).bins[j].merged(o.bins[j]),
                forall|j: int| c <= j < NCLASSES ==> #[trigger] self.bins[j] == old(self).bins[j],
                old(self).small_allocated + o.small_allocated <= usize::MAX ==> self.small_allocated
                    == old(self).small_allocated + o.small_allocated,
                old(self).large_allocated + o.large_allocated <= usize::MAX ==> self.large_allocated
                    == old(self).large_allocated + o.large_allocated,
                old(self).large_runs + o.large_runs <= usize::MAX ==> self.large_runs == old(self).large_runs
                    + o.large_runs,
                forall|j: int|
                    0 <= j < c && old(self).bins[j].curregs + o.bins[j].curregs <= usize::MAX
                        ==> #[trigger] self.bins[j].curregs == old(self).bins[j].curregs + o.bins[j].curregs,
        {
            let (b, ob) = (&mut self.bins[c], &o.bins[c]);
            if b.size == 0 {
                b.size = ob.size;
                b.nregs = ob.nregs;
                b.slab_size = ob.slab_size;
            }
            b.curregs = fig_add(b.curregs, ob.curregs);
            b.curslabs = fig_add(b.curslabs, ob.curslabs);
            b.nonfull_slabs = fig_add(b.nonfull_slabs, ob.nonfull_slabs);
            b.empty_slabs = fig_add(b.empty_slabs, ob.empty_slabs);
            b.purged_slabs = fig_add(b.purged_slabs, ob.purged_slabs);
            b.refilled = fig_add64(b.refilled, ob.refilled);
            b.compacted = fig_add64(b.compacted, ob.compacted);
            b.compact_on = fig_add64(b.compact_on, ob.compact_on);
            b.compact_off = fig_add64(b.compact_off, ob.compact_off);
            b.gate_refill = fig_add64(b.gate_refill, ob.gate_refill);
            b.clamped = fig_add64(b.clamped, ob.clamped);
            proof {
                if old(self).can_add(*o) {
                    assert(old(self).bins[c as int].can_add(o.bins[c as int]));
                }
            }
        }
    }

    /// Every byte figure's sum [`ArenaStats::add`] forms fits a `usize`.
    pub open spec fn bytes_fit(self, o: ArenaStats) -> bool {
        &&& self.small_allocated + o.small_allocated <= usize::MAX
        &&& self.small_active + o.small_active <= usize::MAX
        &&& self.small_dirty + o.small_dirty <= usize::MAX
        &&& self.small_retained + o.small_retained <= usize::MAX
        &&& self.large_allocated + o.large_allocated <= usize::MAX
        &&& self.large_active + o.large_active <= usize::MAX
        &&& self.large_dirty + o.large_dirty <= usize::MAX
        &&& self.large_retained + o.large_retained <= usize::MAX
        &&& self.huge_dirty + o.huge_dirty <= usize::MAX
        &&& self.huge_retained + o.huge_retained <= usize::MAX
        &&& self.metadata + o.metadata <= usize::MAX
        &&& self.resident + o.resident <= usize::MAX
        &&& self.retained + o.retained <= usize::MAX
        &&& self.mapped + o.mapped <= usize::MAX
    }

    /// `r` has every byte figure of `self` and `o` summed.
    pub open spec fn bytes_merged(self, o: ArenaStats, r: ArenaStats) -> bool {
        &&& r.small_allocated == self.small_allocated + o.small_allocated
        &&& r.small_active == self.small_active + o.small_active
        &&& r.small_dirty == self.small_dirty + o.small_dirty
        &&& r.small_retained == self.small_retained + o.small_retained
        &&& r.large_allocated == self.large_allocated + o.large_allocated
        &&& r.large_active == self.large_active + o.large_active
        &&& r.large_dirty == self.large_dirty + o.large_dirty
        &&& r.large_retained == self.large_retained + o.large_retained
        &&& r.huge_dirty == self.huge_dirty + o.huge_dirty
        &&& r.huge_retained == self.huge_retained + o.huge_retained
        &&& r.metadata == self.metadata + o.metadata
        &&& r.resident == self.resident + o.resident
        &&& r.retained == self.retained + o.retained
        &&& r.mapped == self.mapped + o.mapped
    }

    /// Merging figures that satisfy their identities, when the `mapped`
    /// sum fits: every byte figure's sum fits, and the merge satisfies
    /// the identities too.
    pub proof fn lemma_merge_identities(self, o: ArenaStats, r: ArenaStats)
        requires
            self.identities(),
            o.identities(),
            self.mapped + o.mapped <= usize::MAX,
        ensures
            self.bytes_fit(o),
            self.bytes_merged(o, r) ==> r.identities(),
    {
    }

    /// Every sum [`ArenaStats::add`] forms fits its type.
    pub open spec fn can_add(self, o: ArenaStats) -> bool {
        &&& self.small_allocated + o.small_allocated <= usize::MAX
        &&& self.small_active + o.small_active <= usize::MAX
        &&& self.small_dirty + o.small_dirty <= usize::MAX
        &&& self.small_retained + o.small_retained <= usize::MAX
        &&& self.large_allocated + o.large_allocated <= usize::MAX
        &&& self.large_active + o.large_active <= usize::MAX
        &&& self.large_dirty + o.large_dirty <= usize::MAX
        &&& self.large_retained + o.large_retained <= usize::MAX
        &&& self.large_runs + o.large_runs <= usize::MAX
        &&& self.huge_dirty + o.huge_dirty <= usize::MAX
        &&& self.huge_retained + o.huge_retained <= usize::MAX
        &&& self.metadata + o.metadata <= usize::MAX
        &&& self.pactive + o.pactive <= usize::MAX
        &&& self.pdirty + o.pdirty <= usize::MAX
        &&& self.pmuzzy + o.pmuzzy <= usize::MAX
        &&& self.resident + o.resident <= usize::MAX
        &&& self.retained + o.retained <= usize::MAX
        &&& self.mapped + o.mapped <= usize::MAX
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.bins[c].can_add(o.bins[c])
    }

    /// `r` is `self` with every figure of `o` added, bins aside.
    pub open spec fn scalars_merged(self, o: ArenaStats, r: ArenaStats) -> bool {
        &&& r.index == self.index
        &&& r.small_allocated == self.small_allocated + o.small_allocated
        &&& r.small_active == self.small_active + o.small_active
        &&& r.small_dirty == self.small_dirty + o.small_dirty
        &&& r.small_retained == self.small_retained + o.small_retained
        &&& r.large_allocated == self.large_allocated + o.large_allocated
        &&& r.large_active == self.large_active + o.large_active
        &&& r.large_dirty == self.large_dirty + o.large_dirty
        &&& r.large_retained == self.large_retained + o.large_retained
        &&& r.large_runs == self.large_runs + o.large_runs
        &&& r.huge_dirty == self.huge_dirty + o.huge_dirty
        &&& r.huge_retained == self.huge_retained + o.huge_retained
        &&& r.metadata == self.metadata + o.metadata
        &&& r.pactive == self.pactive + o.pactive
        &&& r.pdirty == self.pdirty + o.pdirty
        &&& r.pmuzzy == self.pmuzzy + o.pmuzzy
        &&& r.resident == self.resident + o.resident
        &&& r.retained == self.retained + o.retained
        &&& r.mapped == self.mapped + o.mapped
    }

    /// `r` is `o` merged into `self` ([`ArenaStats::add`]).
    pub open spec fn is_merge(self, o: ArenaStats, r: ArenaStats) -> bool {
        &&& self.scalars_merged(o, r)
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] r.bins[c] == self.bins[c].merged(o.bins[c])
    }
}

} // verus!

verus! {

/// The current figures of one class state (one lock hold).
pub fn collect_bin(cs: &ClassState, Tracked(ord): Tracked<&mut LockOrder>) -> (r: BinStats)
    requires
        crate::sys::may_syscall(),
        crate::layout::process_regions().wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        r.size == class_table()[cs.spec_class() as int].size,
        r.nregs == class_table()[cs.spec_class() as int].nslots,
        r.empty_slabs + r.purged_slabs + r.curslabs <= cs.spec_nslabs(),
        r.fits(),
        *final(ord) == *old(ord),
{
    let (s, curregs) = cs.bin_stats(Tracked(ord));
    let cls = cs.class();
    cs.nslabs_are_the_region_s();
    proof {
        // The slabs, in bytes, within the class region.
        let (lr, ls) = (crate::layout::process_regions().lg_region, cls.lg_slab);
        let (lru, lsu) = (lr as usize, ls as usize);
        assert(crate::layout::LG_REGION_PREFERRED == 38 && crate::layout::LG_MAX_SLAB == 22);
        assert(lsu <= lru <= 38);
        assert((1usize << lsu) == (1usize << ls)) by (bit_vector)
            requires
                lsu == ls as usize,
        ;
        assert((1usize << lru) == (1usize << lr)) by (bit_vector)
            requires
                lru == lr as usize,
        ;
        assert(((1usize << lru) >> lsu) * (1usize << lsu) == (1usize << lru) && (1usize << lru) <= 0x40_0000_0000usize)
            by (bit_vector)
            requires
                lsu <= lru <= 38,
        ;
        let n = cs.spec_nslabs() as int;
        let m = s.committed as int;
        let sl = (1usize << lsu) as int;
        assert(m * sl <= n * sl) by (nonlinear_arith)
            requires
                0 <= m <= n,
                sl >= 0,
        ;
        // The clamp: `curregs * size <= curslabs * nslots * size`.
        let (x, u, k, z) = (curregs as int, (s.committed - s.empty - s.purged) as int, cls.nslots as int, cls.size as int);
        assert(x * z <= u * sl) by (nonlinear_arith)
            requires
                0 <= x <= u * k,
                k * z <= sl,
                u >= 0,
                z >= 0,
        ;
    }
    let (compacted, compact_on, compact_off, gate_refill) = cs.compaction_stats();
    BinStats {
        size: cls.size(),
        nregs: cls.nslots(),
        slab_size: cls.slab_size(),
        curregs,
        curslabs: s.curslabs(),
        nonfull_slabs: s.nonfull,
        empty_slabs: s.empty,
        purged_slabs: s.purged,
        refilled: cs.refilled(),
        compacted,
        compact_on,
        compact_off,
        gate_refill,
        clamped: cs.clamped(),
    }
}

/// Arena `i` of the registry scan over every index below the count: `None`
/// for an index not built yet. `i` is below `arenas.narenas`, which never
/// exceeds `MAX_ARENAS` (`Arenas::count`'s invariant).
pub(crate) fn arena_at(shared: &Shared, i: usize) -> (r: Option<&Arena>)
    requires
        i < MAX_ARENAS,
    ensures
        r matches Some(a) ==> a.spec_index() as int == i,
{
    shared.arenas.get_existing(i as u16)
}

/// The current figures of `arena`. `huge` and `meta` are the process-wide
/// counters the caller read (they are folded into arena 0 only).
///
/// `allocated` is exact ([`arena_exact`]): the bins' `curregs * size` plus
/// the usable bytes of the huge runs in the pages its load of the `large`
/// word returned, which `pg` gets, whenever that true sum fits. The
/// figures satisfy the accounting identities ([`ArenaStats::identities`])
/// and `mapped` is at most [`arena_mapped_max`]: every sum and product
/// fits, so none wraps.
fn collect_arena_with(
    shared: &Shared,
    arena: &Arena,
    huge: &HugeStats,
    meta: &MetaStats,
    Tracked(pg): Tracked<&mut Ghost<int>>,
    Tracked(ord): Tracked<&mut LockOrder>,
) -> (r: ArenaStats)
    requires
        crate::sys::may_abort_corrupt(),
        crate::sys::may_syscall(),
        shared.wf(),
        huge.empty_pages + huge.purged_pages <= u32::MAX,
        meta.identities(),
        meta.len <= crate::meta::va_limit(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        r.pmuzzy == 0,
        forall|c: int| 0 <= c < NCLASSES ==> #[trigger] r.bins[c].size == class_table()[c].size,
        arena_exact(r, final(pg)@),
        r.index == arena.spec_index(),
        r.identities(),
        r.mapped <= arena_mapped_max(r.index),
        *final(ord) == *old(ord),
{
    proof { shared.lemma_wf(); }
    let page = shared.regions.page_size();
    proof {
        let lg = shared.regions.lg_page;
        assert(page <= 0x40_0000usize) by (bit_vector)
            requires
                page == 1usize << lg,
                lg <= 22,
        ;
        assert(NCLASSES <= 64);
    }
    let mut a = ArenaStats::empty(arena.index());
    // The true small and large (bins only) sums so far.
    let ghost mut st: int = 0;
    let ghost mut lt: int = 0;
    proof {
        lemma_class_bytes_frame(curregs_of(a), curregs_of(a), 0, 0);
    }
    for c in 0..NCLASSES
        invariant
            crate::sys::may_syscall(),
            ord.allows(rank_class(0, 0)),
            *ord == *old(ord),
            shared.wf(),
            crate::layout::process_regions().wf(),
            page as int == page_bytes(),
            1 <= page <= 0x40_0000,
            forall|j: int| 0 <= j < c ==> #[trigger] a.bins[j].size == class_table()[j].size,
            st >= 0,
            lt >= 0,
            st + lt == class_bytes(curregs_of(a), 0, c as int),
            class_bytes(curregs_of(a), 0, c as int) <= usize::MAX ==> a.small_allocated == st && a.large_allocated == lt,
            // Every figure so far is exact: allocated within active, and
            // the slab bytes within one class region per class.
            a.index == arena.spec_index(),
            a.huge_dirty == 0 && a.huge_retained == 0 && a.metadata == 0,
            a.small_allocated <= a.small_active,
            a.large_allocated <= a.large_active,
            a.small_active + a.small_dirty + a.small_retained + a.large_active + a.large_dirty + a.large_retained
                <= c * 0x40_0000_0000,
    {
        let ghost a0 = a;
        a.bins[c] = collect_bin(arena.class_at(c), Tracked(ord));
        let ghost t = a.bins[c as int].curregs * a.bins[c as int].size;
        proof {
            lemma_class_bytes_frame(curregs_of(a0), curregs_of(a), 0, c as int);
            let (x, z) = (a.bins[c as int].curregs as int, a.bins[c as int].size as int);
            assert(x * z >= 0) by (nonlinear_arith)
                requires
                    x >= 0,
                    z >= 0,
            ;
            assert(class_bytes(curregs_of(a), 0, c + 1) == class_bytes(curregs_of(a), 0, c as int) + t);
            lemma_bin_fits(a.bins[c as int]);
        }
        let bin = &a.bins[c];
        if c < NBINS {
            let (al, ac, di, re) = (
                fig_mul(bin.curregs, bin.size),
                fig_mul(bin.curslabs, bin.slab_size),
                fig_mul(bin.empty_slabs, bin.slab_size),
                fig_mul(bin.purged_slabs, bin.slab_size),
            );
            a.small_allocated = fig_add(a.small_allocated, al);
            a.small_active = fig_add(a.small_active, ac);
            a.small_dirty = fig_add(a.small_dirty, di);
            a.small_retained = fig_add(a.small_retained, re);
            proof {
                st = st + t;
            }
        } else {
            // Slab-allocated, but reported as large (see `layout::BIN_MAX`).
            let (al, ac, di, re) = (
                fig_mul(bin.curregs, bin.size),
                fig_mul(bin.curslabs, bin.slab_size),
                fig_mul(bin.empty_slabs, bin.slab_size),
                fig_mul(bin.purged_slabs, bin.slab_size),
            );
            a.large_allocated = fig_add(a.large_allocated, al);
            a.large_active = fig_add(a.large_active, ac);
            a.large_dirty = fig_add(a.large_dirty, di);
            a.large_retained = fig_add(a.large_retained, re);
            proof {
                lt = lt + t;
            }
        }
    }
    let (runs, pages) = arena.large();
    let usable_pages = huge_usable_pages(arena.index(), runs, pages);
    proof {
        let (u, p) = (usable_pages as int, page as int);
        assert(u * p <= 0x40_0000_0000_0000) by (nonlinear_arith)
            requires
                0 <= u < 0x1_0000_0000,
                1 <= p <= 0x40_0000,
        ;
    }
    let huge_usable = fig_mul(usable_pages, page);
    a.large_runs = runs;
    a.large_allocated = fig_add(a.large_allocated, huge_usable);
    a.large_active = fig_add(a.large_active, huge_usable);
    proof {
        lemma_huge_bytes_nonneg(runs as int, pages as int);
        assert(huge_bytes(runs as int, pages as int) == (pages - runs * GUARD_PAGES) * page);
        let n = curregs_of(a);
        assert(true_allocated(n, runs as int, pages as int) == st + lt + huge_bytes(runs as int, pages as int));
        assert(arena_exact(a, pages as int));
    }
    let ghost a1 = a;
    if arena.index() == 0 {
        proof {
            let (e, q, p) = (huge.empty_pages as int, huge.purged_pages as int, page as int);
            assert(0 <= e * p && 0 <= q * p && e * p + q * p <= 0x40_0000_0000_0000) by (nonlinear_arith)
                requires
                    e >= 0,
                    q >= 0,
                    e + q <= u32::MAX,
                    1 <= p <= 0x40_0000,
            ;
        }
        a.huge_dirty = fig_mul(huge.empty_pages, page);
        a.huge_retained = fig_mul(huge.purged_pages, page);
        a.metadata = meta.committed;
    }
    let dirty = fig_add(fig_add(a.small_dirty, a.large_dirty), a.huge_dirty);
    let active = fig_add(a.small_active, a.large_active);
    a.pactive = active / page;
    a.pdirty = dirty / page;
    a.pmuzzy = 0;
    a.resident = fig_add(fig_add(active, dirty), a.metadata);
    a.retained = fig_add(fig_add(a.small_retained, a.large_retained), a.huge_retained);
    a.mapped = fig_add(a.resident, a.retained);
    proof {
        assert(curregs_of(a) =~= curregs_of(a1));
        assert(arena_exact(a, pages as int));
        *pg = Ghost(pages as int);
    }
    a
}

/// The current figures of `arena`, not stored.
pub fn collect_arena(shared: &Shared, arena: &Arena, Tracked(ord): Tracked<&mut LockOrder>) -> (r: ArenaStats)
    requires
        crate::sys::may_abort_corrupt(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        forall|c: int| 0 <= c < NCLASSES ==> #[trigger] r.bins[c].size == class_table()[c].size,
        r.identities(),
        *final(ord) == *old(ord),
{
    let tracked mut pg = Ghost(0int);
    collect_arena_with(shared, arena, &shared.huge.stats(Tracked(ord)), &shared.meta.stats(Tracked(ord)), Tracked(&mut pg), Tracked(ord))
}

/// The arenas' figures `reads`' `mapped` summed.
pub open spec fn sum_mapped(reads: Seq<ArenaStats>) -> int
    decreases reads.len(),
{
    if reads.len() == 0 {
        0
    } else {
        sum_mapped(reads.drop_last()) + reads.last().mapped
    }
}

/// Adding arena figures `a` that satisfy their identities to totals `t`
/// that satisfy theirs, when the `mapped` sum fits: every sum
/// [`Totals::add_arena`] forms fits, and the result satisfies the
/// identities too.
pub proof fn lemma_totals_add(t: Totals, a: ArenaStats)
    requires
        t.identities(),
        a.identities(),
        t.mapped + a.mapped <= usize::MAX,
    ensures
        t.can_add_arena(a),
        ({
            let u = Totals {
                allocated: (t.allocated + a.small_allocated + a.large_allocated) as usize,
                active: (t.active + a.small_active + a.large_active) as usize,
                metadata: (t.metadata + a.metadata) as usize,
                resident: (t.resident + a.resident) as usize,
                retained: (t.retained + a.retained) as usize,
                mapped: (t.mapped + a.mapped) as usize,
            };
            u.identities()
        }),
{
}

/// Read every counter once; store the result per arena and in `Shared`
/// when `store` is set. Returns the snapshot (with the new epoch number
/// when stored, the current one otherwise). `rd` gets the figures of each
/// arena it found, in index order, and the pages each one's `large` word
/// load returned; `totals.allocated` is their true sum when that fits
/// ([`totals_exact`]). Each arena's figures satisfy their identities, and
/// the totals theirs whenever the arenas' `mapped` sum fits, which it
/// does for up to 256 arenas ([`Snapshot::inv`]); the snapshot stored
/// is the one returned.
///
/// The process statistics lock is held across the whole collection, so
/// one collection is published at a time: concurrent `epoch`s never leave
/// arenas and totals from different collections, and a reader of the
/// snapshot never sees the arenas of a collection whose totals are not
/// stored yet. It nests outside the class, slab table, huge, metadata and
/// per-arena statistics locks the collection takes (design 9).
fn collect_into(shared: &Shared, store: bool, Tracked(rd): Tracked<&mut Ghost<(Seq<ArenaStats>, Seq<int>)>>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Snapshot)
    requires
        crate::sys::may_abort_corrupt(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_stats()),
    ensures
        totals_exact(final(rd)@.0, final(rd)@.1, r.totals),
        r.narenas == final(rd)@.0.len(),
        ids_ascending(final(rd)@.0),
        forall|j: int| 0 <= j < final(rd)@.0.len() ==> ((#[trigger] final(rd)@.0[j]).index as usize) < MAX_ARENAS,
        forall|j: int| 0 <= j < final(rd)@.0.len() ==> #[trigger] final(rd)@.0[j].identities(),
        sum_mapped(final(rd)@.0) <= usize::MAX ==> r.totals.mapped == sum_mapped(final(rd)@.0)
            && r.totals.identities(),
        r.inv(),
        *final(ord) == *old(ord),
{
    broadcast use axiom_snapshot_lock_inv;

    proof {
        shared.lemma_ranks();
    }
    let tracked mut under = LockOrder::spare();
    let mut g = shared.stats.lock(Tracked(ord), Tracked(&mut under));
    let huge = shared.huge.stats(Tracked(&mut under));
    let meta = shared.meta.stats(Tracked(&mut under));
    let mut totals = Totals::EMPTY;
    let mut narenas: u32 = 0;
    let n = shared.arenas.count();
    let ghost mut reads = Seq::<ArenaStats>::empty();
    let ghost mut pages = Seq::<int>::empty();
    proof {
        lemma_reads_empty();
        assert(sum_mapped(reads) == 0);
    }
    for i in 0..n as usize
        invariant
            n as usize <= MAX_ARENAS,
            crate::sys::may_abort_corrupt(),
            crate::sys::may_syscall(),
            shared.wf(),
            huge.empty_pages + huge.purged_pages <= u32::MAX,
            meta.identities(),
            meta.len <= crate::meta::va_limit(),
            narenas <= i <= n,
            totals_exact(reads, pages, totals),
            narenas == reads.len(),
            ids_ascending(reads),
            forall|j: int| 0 <= j < reads.len() ==> ((#[trigger] reads[j]).index as usize) < i,
            forall|j: int| 0 <= j < reads.len() ==> #[trigger] reads[j].identities(),
            // Arena 0, the only one above `2^55`, comes first.
            sum_mapped(reads) <= 0x400_0000_0000_0000 + reads.len() * 0x80_0000_0000_0000,
            sum_mapped(reads) <= usize::MAX ==> totals.mapped == sum_mapped(reads) && totals.identities(),
            under.top() == rank_stats(),
    {
        if let Some(arena) = arena_at(shared, i) {
            let tracked mut pg = Ghost(0int);
            let a = collect_arena_with(shared, arena, &huge, &meta, Tracked(&mut pg), Tracked(&mut under));
            let ghost p = pg@;
            proof {
                lemma_reads_step(reads, pages, a, p);
                assert(reads.push(a).drop_last() =~= reads);
                if sum_mapped(reads.push(a)) <= usize::MAX {
                    lemma_totals_add(totals, a);
                }
            }
            totals.add_arena(&a);
            proof {
                reads = reads.push(a);
                pages = pages.push(p);
            }
            narenas += 1;
            if store {
                arena.store_stats(&a, Tracked(&mut under));
            }
        }
    }
    proof {
        assert(totals_exact(reads, pages, totals));
        *rd = Ghost((reads, pages));
        if narenas <= 256 {
            assert(sum_mapped(reads) <= usize::MAX);
        }
    }
    if store {
        g.epoch = fig_add64(g.epoch, 1);
        g.narenas = narenas;
        g.totals = totals;
        g.huge = huge;
        g.meta = meta;
        proof {
            assert(totals_exact(reads, pages, g@.totals));
            assert(crate::lock::lock_inv(g@));
        }
        *g
    } else {
        Snapshot {
            epoch: g.epoch,
            narenas,
            totals,
            huge,
            meta,
        }
    }
}

/// `epoch`: refresh every arena's stored [`ArenaStats`] and the process
/// [`Snapshot`]. Returns the new epoch number. The caller's record `fx`
/// gets [`Change::Refreshed`].
pub fn epoch(shared: &Shared, Tracked(fx): Tracked<&mut Ghost<Change>>, Tracked(ord): Tracked<&mut LockOrder>) -> u64
    requires
        crate::sys::may_abort_corrupt(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_stats()),
    ensures
        *final(ord) == *old(ord),
        final(fx)@ == Change::Refreshed,
{
    proof {
        *fx = Ghost(Change::Refreshed);
    }
    let tracked mut rd = Ghost((Seq::<ArenaStats>::empty(), Seq::<int>::empty()));
    collect_into(shared, true, Tracked(&mut rd), Tracked(ord)).epoch
}

/// The current totals without storing them (for `malloc_stats_print`).
pub fn collect(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Snapshot)
    requires
        crate::sys::may_abort_corrupt(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_stats()),
    ensures
        r.inv(),
        *final(ord) == *old(ord),
{
    let tracked mut rd = Ghost((Seq::<ArenaStats>::empty(), Seq::<int>::empty()));
    collect_into(shared, false, Tracked(&mut rd), Tracked(ord))
}

/// The snapshot stored by the last [`epoch`] (all zero before the first).
pub fn snapshot(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Snapshot)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_stats()),
    ensures
        r.inv(),
        *final(ord) == *old(ord),
{
    broadcast use axiom_snapshot_lock_inv;

    proof {
        shared.lemma_ranks();
    }
    *shared.stats.lock(Tracked(ord), Tracked(&mut LockOrder::spare()))
}

/// The [`ArenaStats`] stored by the last [`epoch`] for arena `index`, or
/// `None` if the arena does not exist. An arena created since the last
/// epoch reports zeros.
pub fn arena_stats(shared: &Shared, index: u16, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<ArenaStats>)
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_arena_stats(0)),
    ensures
        r matches Some(a) ==> a.identities(),
        *final(ord) == *old(ord),
{
    Some(shared.arenas.get_existing(index)?.stats(Tracked(ord)))
}

/// `stats.arenas.4096.*` (`MALLCTL_ARENAS_ALL`): every existing arena's
/// stored figures summed, all from one `epoch` (the process statistics
/// lock keeps one from storing midway). Its `index` is 4096.
pub fn arena_stats_all(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>) -> (r: ArenaStats)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_stats()),
    ensures
        r.index == crate::arena::ARENAS_ALL as u16,
        *final(ord) == *old(ord),
{
    proof {
        shared.lemma_ranks();
    }
    let tracked mut under = LockOrder::spare();
    let _published = shared.stats.lock(Tracked(ord), Tracked(&mut under));
    let mut all = ArenaStats::empty(crate::arena::ARENAS_ALL as u16);
    let n = shared.arenas.count();
    for i in 0..n as usize
        invariant
            n as usize <= MAX_ARENAS,
            crate::sys::may_syscall(),
            all.index == crate::arena::ARENAS_ALL as u16,
            under.top() == rank_stats(),
    {
        if let Some(arena) = arena_at(shared, i) {
            all.add(&arena.stats(Tracked(&mut under)));
        }
    }
    all
}

/// Every existing arena's *current* figures summed, not stored (for
/// `mallinfo` and `malloc_stats`, which glibc defines as live). Its
/// `allocated`, `curregs` and `large_runs` are the true sums of the
/// figures it read of each arena it found, which `rd` gets in index order
/// with the pages of each `large` word load ([`summed`]). Each arena's
/// figures satisfy their identities, and the sum does whenever the
/// arenas' `mapped` sum fits, which it does for up to 256 arenas.
pub fn collect_all(shared: &Shared, Tracked(rd): Tracked<&mut Ghost<(Seq<ArenaStats>, Seq<int>)>>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: ArenaStats)
    requires
        crate::sys::may_abort_corrupt(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        r.index == crate::arena::ARENAS_ALL as u16,
        summed(final(rd)@.0, final(rd)@.1, r),
        ids_ascending(final(rd)@.0),
        forall|j: int| 0 <= j < final(rd)@.0.len() ==> ((#[trigger] final(rd)@.0[j]).index as usize) < MAX_ARENAS,
        forall|j: int| 0 <= j < final(rd)@.0.len() ==> #[trigger] final(rd)@.0[j].identities(),
        sum_mapped(final(rd)@.0) <= usize::MAX ==> r.mapped == sum_mapped(final(rd)@.0) && r.identities(),
        final(rd)@.0.len() <= 256 ==> r.identities(),
        *final(ord) == *old(ord),
{
    let huge = shared.huge.stats(Tracked(ord));
    let meta = shared.meta.stats(Tracked(ord));
    let mut all = ArenaStats::empty(crate::arena::ARENAS_ALL as u16);
    let n = shared.arenas.count();
    let ghost mut reads = Seq::<ArenaStats>::empty();
    let ghost mut pages = Seq::<int>::empty();
    proof {
        lemma_reads_empty();
        assert(sum_mapped(reads) == 0);
    }
    for i in 0..n as usize
        invariant
            n as usize <= MAX_ARENAS,
            crate::sys::may_abort_corrupt(),
            crate::sys::may_syscall(),
            shared.wf(),
            huge.empty_pages + huge.purged_pages <= u32::MAX,
            meta.identities(),
            meta.len <= crate::meta::va_limit(),
            all.index == crate::arena::ARENAS_ALL as u16,
            summed(reads, pages, all),
            reads.len() <= i,
            ids_ascending(reads),
            forall|j: int| 0 <= j < reads.len() ==> ((#[trigger] reads[j]).index as usize) < i,
            forall|j: int| 0 <= j < reads.len() ==> #[trigger] reads[j].identities(),
            // Arena 0, the only one above `2^55`, comes first.
            sum_mapped(reads) <= 0x400_0000_0000_0000 + reads.len() * 0x80_0000_0000_0000,
            sum_mapped(reads) <= usize::MAX ==> all.mapped == sum_mapped(reads) && all.identities(),
            ord.allows(rank_class(0, 0)),
            *ord == *old(ord),
    {
        if let Some(arena) = arena_at(shared, i) {
            let tracked mut pg = Ghost(0int);
            let a = collect_arena_with(shared, arena, &huge, &meta, Tracked(&mut pg), Tracked(ord));
            let ghost p = pg@;
            let ghost all0 = all;
            proof {
                lemma_reads_step(reads, pages, a, p);
                assert(reads.push(a).drop_last() =~= reads);
            }
            all.add(&a);
            proof {
                if sum_mapped(reads.push(a)) <= usize::MAX {
                    all0.lemma_merge_identities(a, all);
                }
                let (r1, p1) = (reads.push(a), pages.push(p));
                if reads_allocated(r1, p1) <= usize::MAX {
                    assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] all.bins[c].curregs == sum_curregs(r1, c) by {
                        assert(sum_curregs(r1, c) <= usize::MAX);
                    }
                }
                reads = r1;
                pages = p1;
            }
        }
    }
    proof {
        *rd = Ghost((reads, pages));
    }
    all
}

} // verus!

verus! {

// Exact stats. The figures above as true sums: what `collect_arena_with`,
// `collect_into` and `collect_all` report for `allocated`, as a function
// of the counters they read.

/// `n[c]` slots of each class `c` in `lo..hi`, in bytes: the sum of
/// `n[c] * size(c)`, in `int`.
pub open spec fn class_bytes(n: Seq<int>, lo: int, hi: int) -> int
    decreases hi - lo,
{
    if hi <= lo {
        0
    } else {
        class_bytes(n, lo, hi - 1) + n[hi - 1] * class_table()[hi - 1].size as int
    }
}

/// The `curregs` of `a`'s bins, by class.
pub open spec fn curregs_of(a: ArenaStats) -> Seq<int> {
    Seq::new(NCLASSES as nat, |c: int| a.bins[c].curregs as int)
}

/// The process's page, in bytes.
pub open spec fn page_bytes() -> int {
    (1usize << crate::layout::process_regions().lg_page) as int
}

/// The usable bytes of `runs` huge runs of `pages` pages in all, each with
/// its guard: the pages before the guards, at the process's page size.
pub open spec fn huge_bytes(runs: int, pages: int) -> int {
    (pages - runs * GUARD_PAGES) * page_bytes()
}

/// The true `allocated` of slots `n` (by class) and `runs` huge runs of
/// `pages` pages: [`class_bytes`] plus [`huge_bytes`].
pub open spec fn true_allocated(n: Seq<int>, runs: int, pages: int) -> int {
    class_bytes(n, 0, NCLASSES as int) + huge_bytes(runs, pages)
}

/// `a`'s `allocated` (`small_allocated + large_allocated`) is the true sum
/// of its bins and of its `large_runs` huge runs, in `pages` pages, when
/// that sum fits a `usize`.
pub open spec fn arena_exact(a: ArenaStats, pages: int) -> bool {
    &&& a.large_runs * GUARD_PAGES <= pages
    &&& true_allocated(curregs_of(a), a.large_runs as int, pages) <= usize::MAX ==> a.small_allocated + a.large_allocated
        == true_allocated(curregs_of(a), a.large_runs as int, pages)
}

/// The `curregs` of class `c` summed over the arenas' figures `reads`.
pub open spec fn sum_curregs(reads: Seq<ArenaStats>, c: int) -> int
    decreases reads.len(),
{
    if reads.len() == 0 {
        0
    } else {
        sum_curregs(reads.drop_last(), c) + reads.last().bins[c].curregs
    }
}

/// [`sum_curregs`] for every class.
pub open spec fn all_curregs(reads: Seq<ArenaStats>) -> Seq<int> {
    Seq::new(NCLASSES as nat, |c: int| sum_curregs(reads, c))
}

/// The huge runs of the arenas' figures `reads`, summed.
pub open spec fn sum_runs(reads: Seq<ArenaStats>) -> int
    decreases reads.len(),
{
    if reads.len() == 0 {
        0
    } else {
        sum_runs(reads.drop_last()) + reads.last().large_runs
    }
}

/// `xs` summed.
pub open spec fn sum_ints(xs: Seq<int>) -> int
    decreases xs.len(),
{
    if xs.len() == 0 {
        0
    } else {
        sum_ints(xs.drop_last()) + xs.last()
    }
}

/// The true `allocated` of the arenas' figures `reads`, whose huge runs
/// hold `pages[i]` pages.
pub open spec fn reads_allocated(reads: Seq<ArenaStats>, pages: Seq<int>) -> int {
    true_allocated(all_curregs(reads), sum_runs(reads), sum_ints(pages))
}

/// The figures `reads` come from arenas of ascending registry IDs
/// (`ArenaStats::index`): a scan in index order reads each arena once,
/// and keys it by its own ID, not by its position in the scan (with
/// automatic arenas not built yet, IDs skip: `0, 2` for two automatic
/// arenas, only arena 0 built, and explicit arena 2).
pub open spec fn ids_ascending(reads: Seq<ArenaStats>) -> bool {
    forall|i: int, j: int| 0 <= i < j < reads.len() ==> #[trigger] reads[i].index < #[trigger] reads[j].index
}

/// `reads` (with huge pages `pages`) are per-arena figures as a scan over
/// the arenas reads them: each exact ([`arena_exact`]), every bin sized as
/// its class.
pub open spec fn reads_ok(reads: Seq<ArenaStats>, pages: Seq<int>) -> bool {
    &&& reads.len() == pages.len()
    &&& forall|i: int| 0 <= i < reads.len() ==> #[trigger] arena_exact(reads[i], pages[i])
    &&& forall|i: int, c: int| 0 <= i < reads.len() && 0 <= c < NCLASSES ==> #[trigger] reads[i].bins[c].size
        == class_table()[c].size
}

/// `r` is the sum of the per-arena figures `reads` ([`ArenaStats::add`]):
/// whenever their true `allocated` fits, `r`'s `allocated` and per-class
/// `curregs` are the true sums; its `large_runs` is when that sum fits.
pub open spec fn summed(reads: Seq<ArenaStats>, pages: Seq<int>, r: ArenaStats) -> bool {
    &&& reads_ok(reads, pages)
    &&& reads_allocated(reads, pages) <= usize::MAX ==> {
        &&& r.small_allocated + r.large_allocated == reads_allocated(reads, pages)
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] r.bins[c].curregs == sum_curregs(reads, c)
    }
    &&& sum_runs(reads) <= usize::MAX ==> r.large_runs == sum_runs(reads)
}

/// `t` is the total of the per-arena figures `reads`
/// ([`Totals::add_arena`]): its `allocated` is their true sum when that
/// fits.
pub open spec fn totals_exact(reads: Seq<ArenaStats>, pages: Seq<int>, t: Totals) -> bool {
    &&& reads_ok(reads, pages)
    &&& reads_allocated(reads, pages) <= usize::MAX ==> t.allocated == reads_allocated(reads, pages)
}

/// [`class_bytes`] of naturals is a natural.
pub proof fn lemma_class_bytes_nonneg(n: Seq<int>, lo: int, hi: int)
    requires
        forall|c: int| lo <= c < hi ==> n[c] >= 0,
    ensures
        class_bytes(n, lo, hi) >= 0,
    decreases hi - lo,
{
    if hi > lo {
        lemma_class_bytes_nonneg(n, lo, hi - 1);
        let z = class_table()[hi - 1].size as int;
        let x = n[hi - 1];
        assert(x * z >= 0) by (nonlinear_arith)
            requires
                x >= 0,
                z >= 0,
        ;
    }
}

/// [`class_bytes`] reads `n` on its range only.
pub proof fn lemma_class_bytes_frame(n: Seq<int>, m: Seq<int>, lo: int, hi: int)
    requires
        forall|c: int| lo <= c < hi ==> n[c] == m[c],
    ensures
        class_bytes(n, lo, hi) == class_bytes(m, lo, hi),
    decreases hi - lo,
{
    if hi > lo {
        lemma_class_bytes_frame(n, m, lo, hi - 1);
    }
}

/// [`class_bytes`] is linear.
pub proof fn lemma_class_bytes_add(n: Seq<int>, m: Seq<int>, s: Seq<int>, lo: int, hi: int)
    requires
        forall|c: int| lo <= c < hi ==> s[c] == n[c] + m[c],
    ensures
        class_bytes(s, lo, hi) == class_bytes(n, lo, hi) + class_bytes(m, lo, hi),
    decreases hi - lo,
{
    if hi > lo {
        lemma_class_bytes_add(n, m, s, lo, hi - 1);
        let z = class_table()[hi - 1].size as int;
        let (x, y, w) = (n[hi - 1], m[hi - 1], s[hi - 1]);
        assert(w * z == x * z + y * z) by (nonlinear_arith)
            requires
                w == x + y,
        ;
    }
}

/// One class's slots are at most the bytes of all the slots: a class is a
/// byte or more.
pub proof fn lemma_class_bytes_bound(n: Seq<int>, lo: int, hi: int, c: int)
    requires
        0 <= lo <= c < hi <= NCLASSES,
        forall|j: int| lo <= j < hi ==> n[j] >= 0,
    ensures
        n[c] <= class_bytes(n, lo, hi),
    decreases hi - lo,
{
    broadcast use crate::vspec::class::axiom_class_table_wf;

    lemma_class_bytes_nonneg(n, lo, hi - 1);
    let z = class_table()[hi - 1].size as int;
    let x = n[hi - 1];
    assert(class_table()[hi - 1].wf());
    assert(x * z >= x) by (nonlinear_arith)
        requires
            x >= 0,
            z >= 1,
    ;
    if c < hi - 1 {
        lemma_class_bytes_bound(n, lo, hi - 1, c);
    }
}

/// [`huge_bytes`] is linear.
pub proof fn lemma_huge_bytes_add(r1: int, p1: int, r2: int, p2: int)
    ensures
        huge_bytes(r1 + r2, p1 + p2) == huge_bytes(r1, p1) + huge_bytes(r2, p2),
{
    let pg = page_bytes();
    assert((p1 + p2 - (r1 + r2) * GUARD_PAGES) * pg == (p1 - r1 * GUARD_PAGES) * pg + (p2 - r2 * GUARD_PAGES) * pg)
        by (nonlinear_arith);
}

/// [`huge_bytes`] of runs within their pages is a natural.
pub proof fn lemma_huge_bytes_nonneg(runs: int, pages: int)
    requires
        runs * GUARD_PAGES <= pages,
    ensures
        huge_bytes(runs, pages) >= 0,
{
    let pg = page_bytes();
    assert(pg >= 0);
    assert((pages - runs * GUARD_PAGES) * pg >= 0) by (nonlinear_arith)
        requires
            runs * GUARD_PAGES <= pages,
            pg >= 0,
    ;
}

/// Adding arena figures `a` (huge pages `p`) to `reads` adds its
/// `curregs` per class, its runs, its pages and its true `allocated`.
pub proof fn lemma_reads_push(reads: Seq<ArenaStats>, pages: Seq<int>, a: ArenaStats, p: int)
    ensures
        forall|c: int| #[trigger] sum_curregs(reads.push(a), c) == sum_curregs(reads, c) + a.bins[c].curregs,
        sum_runs(reads.push(a)) == sum_runs(reads) + a.large_runs,
        sum_ints(pages.push(p)) == sum_ints(pages) + p,
        reads_allocated(reads.push(a), pages.push(p)) == reads_allocated(reads, pages) + true_allocated(
            curregs_of(a),
            a.large_runs as int,
            p,
        ),
{
    assert(reads.push(a).drop_last() =~= reads);
    assert(pages.push(p).drop_last() =~= pages);
    let n = all_curregs(reads);
    let m = curregs_of(a);
    let s = all_curregs(reads.push(a));
    lemma_class_bytes_add(n, m, s, 0, NCLASSES as int);
    lemma_huge_bytes_add(sum_runs(reads), sum_ints(pages), a.large_runs as int, p);
}

/// The sums over no arenas are zero.
pub proof fn lemma_reads_empty()
    ensures
        forall|c: int| #[trigger] sum_curregs(Seq::<ArenaStats>::empty(), c) == 0,
        sum_runs(Seq::<ArenaStats>::empty()) == 0,
        sum_ints(Seq::<int>::empty()) == 0,
        reads_allocated(Seq::<ArenaStats>::empty(), Seq::<int>::empty()) == 0,
{
    let n = all_curregs(Seq::<ArenaStats>::empty());
    assert forall|c: int| 0 <= c < NCLASSES implies n[c] == 0 by {}
    lemma_class_bytes_zero(n, 0, NCLASSES as int);
    let pg = page_bytes();
    assert((0 - 0 * GUARD_PAGES) * pg == 0) by (nonlinear_arith);
    assert(huge_bytes(0, 0) == 0);
}

/// [`class_bytes`] of no slots is zero.
pub proof fn lemma_class_bytes_zero(n: Seq<int>, lo: int, hi: int)
    requires
        forall|c: int| lo <= c < hi ==> n[c] == 0,
    ensures
        class_bytes(n, lo, hi) == 0,
    decreases hi - lo,
{
    if hi > lo {
        lemma_class_bytes_zero(n, lo, hi - 1);
    }
}

/// The sums of naturals over the arenas are naturals; the runs' guards
/// are within their pages.
pub proof fn lemma_reads_nonneg(reads: Seq<ArenaStats>, pages: Seq<int>)
    requires
        reads_ok(reads, pages),
    ensures
        forall|c: int| #[trigger] sum_curregs(reads, c) >= 0,
        sum_runs(reads) >= 0,
        sum_runs(reads) * GUARD_PAGES <= sum_ints(pages),
        class_bytes(all_curregs(reads), 0, NCLASSES as int) >= 0,
        huge_bytes(sum_runs(reads), sum_ints(pages)) >= 0,
        reads_allocated(reads, pages) >= 0,
    decreases reads.len(),
{
    if reads.len() == 0 {
        lemma_reads_empty();
    } else {
        let (r0, p0) = (reads.drop_last(), pages.drop_last());
        assert(reads_ok(r0, p0)) by {
            assert forall|i: int| 0 <= i < r0.len() implies #[trigger] arena_exact(r0[i], p0[i]) by {
                assert(arena_exact(reads[i], pages[i]));
            }
            assert forall|i: int, c: int| 0 <= i < r0.len() && 0 <= c < NCLASSES implies #[trigger] r0[i].bins[c].size
                == class_table()[c].size by {
                assert(reads[i].bins[c].size == class_table()[c].size);
            }
        }
        lemma_reads_nonneg(r0, p0);
        assert(reads =~= r0.push(reads.last()));
        assert(pages =~= p0.push(pages.last()));
        lemma_reads_push(r0, p0, reads.last(), pages.last());
        assert(arena_exact(reads[reads.len() - 1], pages[reads.len() - 1]));
    }
    lemma_class_bytes_nonneg(all_curregs(reads), 0, NCLASSES as int);
    lemma_huge_bytes_nonneg(sum_runs(reads), sum_ints(pages));
}

/// One more arena's figures `a`, exact with huge pages `p`, in a scan:
/// the sums grow by its own, which are naturals, and a per-class sum fits
/// whenever the true `allocated` does.
pub proof fn lemma_reads_step(reads: Seq<ArenaStats>, pages: Seq<int>, a: ArenaStats, p: int)
    requires
        reads_ok(reads, pages),
        arena_exact(a, p),
        forall|c: int| 0 <= c < NCLASSES ==> #[trigger] a.bins[c].size == class_table()[c].size,
    ensures
        reads_ok(reads.push(a), pages.push(p)),
        reads_allocated(reads.push(a), pages.push(p)) == reads_allocated(reads, pages) + true_allocated(
            curregs_of(a),
            a.large_runs as int,
            p,
        ),
        reads_allocated(reads, pages) >= 0,
        true_allocated(curregs_of(a), a.large_runs as int, p) >= 0,
        forall|c: int| #[trigger] sum_curregs(reads.push(a), c) == sum_curregs(reads, c) + a.bins[c].curregs,
        sum_runs(reads.push(a)) == sum_runs(reads) + a.large_runs,
        forall|c: int|
            0 <= c < NCLASSES && reads_allocated(reads.push(a), pages.push(p)) <= usize::MAX ==> #[trigger] sum_curregs(
                reads.push(a),
                c,
            ) <= usize::MAX,
{
    let (r1, p1) = (reads.push(a), pages.push(p));
    lemma_reads_nonneg(reads, pages);
    lemma_reads_push(reads, pages, a, p);
    assert(reads_ok(r1, p1)) by {
        assert forall|i: int| 0 <= i < r1.len() implies #[trigger] arena_exact(r1[i], p1[i]) by {
            if i < reads.len() {
                assert(arena_exact(reads[i], pages[i]));
            }
        }
        assert forall|i: int, c: int| 0 <= i < r1.len() && 0 <= c < NCLASSES implies #[trigger] r1[i].bins[c].size
            == class_table()[c].size by {
            if i < reads.len() {
                assert(reads[i].bins[c].size == class_table()[c].size);
            }
        }
    }
    lemma_reads_nonneg(r1, p1);
    let m = curregs_of(a);
    assert forall|c: int| 0 <= c < NCLASSES implies m[c] >= 0 by {}
    lemma_class_bytes_nonneg(m, 0, NCLASSES as int);
    lemma_huge_bytes_nonneg(a.large_runs as int, p);
    assert forall|c: int|
        0 <= c < NCLASSES && reads_allocated(r1, p1) <= usize::MAX implies #[trigger] sum_curregs(r1, c) <= usize::MAX by {
        let n = all_curregs(r1);
        assert forall|j: int| 0 <= j < NCLASSES implies n[j] >= 0 by {}
        lemma_class_bytes_bound(n, 0, NCLASSES as int, c);
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::tests::{TestHeap, class_with, live};
    use crate::layout::CLASSES;
    use crate::testutil::{page_size, shared};
    use std::vec::Vec;

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
        assert_eq!(snapshot(s, Tracked::assume_new()), Snapshot::EMPTY);
        assert_eq!(epoch(s, Tracked::assume_new(), Tracked::assume_new()), 1);
        let first = snapshot(s, Tracked::assume_new());
        assert_eq!(first.epoch, 1);
        assert_eq!(first.narenas, 1);
        assert_eq!(first.totals.allocated, 0);
        assert_eq!(first.totals.active, 0);
        assert!(
            first.totals.metadata > 0,
            "the slab tables and sets are committed metadata"
        );
        assert_eq!(first.totals.resident, first.totals.metadata);
        assert_eq!(first.totals.retained, 0);
        assert_eq!(first.totals.mapped, first.totals.resident);
        assert!(first.totals.identities_hold());
        assert_eq!(first.meta, s.meta.stats(Tracked::assume_new()));

        let t = TestHeap::new(s);
        let c16 = class_with(16);
        let c256 = class_with(256);
        let mut ps: Vec<*mut u8> = (0..100).map(|_| t.alloc(c16)).collect();
        ps.extend((0..10).map(|_| t.alloc(c256)));
        // A class above BIN_MAX: slab-allocated, reported as large.
        let cmed = class_with(32 * 1024);
        assert!((NBINS..NCLASSES).contains(&cmed));
        let pm = t.alloc(cmed);
        let med_slab = CLASSES[cmed].slab_size();
        let huge_size = 300 * 1024;
        let (h, _) = t.alloc_huge(huge_size, 16);
        assert!(!h.is_null());
        let usable = s.huge.usable_for(huge_size).unwrap();
        let run_pages = s.huge.usable_pages(huge_size).unwrap() + GUARD_PAGES;
        assert_eq!(
            s.arena0.large(),
            (1, run_pages),
            "the descriptor's pages, guard included"
        );

        // Frozen until the next epoch; `collect` sees the live figures.
        assert_eq!(snapshot(s, Tracked::assume_new()), first);
        assert_eq!(
            arena_stats(s, 0, Tracked::assume_new())
                .unwrap()
                .small_allocated,
            0
        );
        assert!(collect(s, Tracked::assume_new()).totals.allocated > 0);
        assert_eq!(collect(s, Tracked::assume_new()).epoch, 1);

        assert_eq!(epoch(s, Tracked::assume_new(), Tracked::assume_new()), 2);
        let a = arena_stats(s, 0, Tracked::assume_new()).unwrap();
        let tot = snapshot(s, Tracked::assume_new()).totals;
        assert_eq!(a.index, 0);
        let b16 = a.bins[c16];
        assert_eq!(
            (b16.size, b16.nregs, b16.slab_size),
            (16, CLASSES[c16].nslots(), CLASSES[c16].slab_size())
        );
        assert_eq!(b16.curregs, live(s, c16), "curregs is the sum of live");
        assert!(
            b16.curregs >= 100,
            "the refill batch counts: {}",
            b16.curregs
        );
        assert_eq!(
            (
                b16.curslabs,
                b16.nonfull_slabs,
                b16.empty_slabs,
                b16.purged_slabs
            ),
            (1, 1, 0, 0)
        );
        let b256 = a.bins[c256];
        assert_eq!(b256.curregs, live(s, c256));
        // The 256-byte refill (256 slots) fits in one 256-slot slab; keep
        // the count derived so the test survives a geometry change.
        let slabs256 = b256.curslabs;
        assert!(
            slabs256 >= 1 && (slabs256 - 1) * CLASSES[c256].nslots() < b256.curregs,
            "{slabs256} slabs"
        );
        assert_eq!(a.small_allocated, live(s, c16) * 16 + live(s, c256) * 256);
        assert_eq!(
            a.small_active,
            CLASSES[c16].slab_size() + slabs256 * CLASSES[c256].slab_size()
        );
        assert_eq!((a.small_dirty, a.small_retained), (0, 0));
        let bmed = a.bins[cmed];
        assert_eq!((bmed.curregs, bmed.curslabs), (live(s, cmed), 1));
        assert!(bmed.curregs >= 1);
        assert_eq!(a.large_runs, 1);
        assert_eq!(usable, (run_pages - GUARD_PAGES) * page_size());
        assert_eq!(
            a.large_allocated,
            usable + live(s, cmed) * CLASSES[cmed].size(),
            "classes above BIN_MAX are large"
        );
        assert_eq!(a.large_active, usable + med_slab);
        assert_eq!(
            (
                a.large_dirty,
                a.large_retained,
                a.huge_dirty,
                a.huge_retained
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(a.pactive, (a.small_active + a.large_active) / page_size());
        assert_eq!((a.pdirty, a.pmuzzy), (0, 0));
        assert_eq!(a.resident, a.small_active + a.large_active + a.metadata);
        assert_eq!(a.mapped, a.resident);
        assert_eq!(tot.allocated, a.small_allocated + a.large_allocated);
        assert_eq!(tot.active, a.small_active + a.large_active);
        assert_eq!(tot.metadata, s.meta.stats(Tracked::assume_new()).committed);
        assert_eq!(tot.resident, tot.active + tot.metadata);
        assert_eq!(tot.retained, 0);
        assert!(tot.identities_hold(), "{tot:?}");
        assert_eq!(
            collect(s, Tracked::assume_new()).totals,
            tot,
            "nothing moved: live and stored agree"
        );

        // Free everything and flush: the slabs and the run go Empty.
        for &p in &ps {
            t.free(p);
        }
        t.free(h);
        t.free(pm);
        t.flush_all();
        assert_eq!(epoch(s, Tracked::assume_new(), Tracked::assume_new()), 3);
        let a = arena_stats(s, 0, Tracked::assume_new()).unwrap();
        let tot = snapshot(s, Tracked::assume_new()).totals;
        assert_eq!((tot.allocated, tot.active), (0, 0));
        let b16 = a.bins[c16];
        assert_eq!(
            (
                b16.curregs,
                b16.curslabs,
                b16.nonfull_slabs,
                b16.empty_slabs,
                b16.purged_slabs
            ),
            (0, 0, 0, 1, 0)
        );
        assert_eq!(a.bins[c256].empty_slabs, slabs256);
        assert_eq!(
            a.small_dirty,
            CLASSES[c16].slab_size() + slabs256 * CLASSES[c256].slab_size()
        );
        assert_eq!((a.large_runs, a.large_allocated, a.large_active), (0, 0, 0));
        assert_eq!(a.large_dirty, med_slab, "the medium slab went Empty");
        assert_eq!(a.huge_dirty, run_pages * page_size());
        assert_eq!(
            a.pdirty,
            (a.small_dirty + a.large_dirty + a.huge_dirty) / page_size()
        );
        assert_eq!(
            tot.resident,
            tot.metadata + a.small_dirty + a.large_dirty + a.huge_dirty
        );
        assert_eq!(tot.retained, 0);
        assert_eq!(tot.mapped, tot.resident);
        assert!(tot.identities_hold(), "{tot:?}");
        let dirty = a.small_dirty + a.large_dirty + a.huge_dirty;

        // Purge: dirty becomes retained; resident falls back to metadata.
        assert!(crate::purge::purge_all(s, Tracked::assume_new(), Tracked::assume_new()) > 0);
        assert_eq!(epoch(s, Tracked::assume_new(), Tracked::assume_new()), 4);
        let a = arena_stats(s, 0, Tracked::assume_new()).unwrap();
        let tot = snapshot(s, Tracked::assume_new()).totals;
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
                    let bin = collect_bin(cs, Tracked::assume_new());
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
            assert_eq!(collect_bin(cs, Tracked::assume_new()).curslabs, 0);
        }
        let tot = collect(s, Tracked::assume_new()).totals;
        assert_eq!(tot.allocated, 0);
        assert!(tot.identities_hold(), "{tot:?}");
    }

    /// Arena IDs survive enumeration holes: with two automatic arenas of
    /// which only arena 0 is built, `create` hands out arena 2, and the
    /// scan finds arenas 0 and 2, each keyed by its registry ID; the hole
    /// at 1 is skipped, and the totals are the two arenas' figures.
    #[test]
    fn collection_keys_arenas_by_registry_id_across_a_hole() {
        let s = crate::testutil::TestShared::with(crate::config::Config {
            narenas: 2,
            ..crate::config::Config::DEFAULT
        })
        .build();
        assert_eq!(s.arenas.count(), 2);
        assert!(s.arenas.get_existing(1).is_none());
        assert_eq!(
            s.arenas.create(
                s,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            Some(2)
        );
        assert_eq!(s.arenas.count(), 3);
        assert!(s.arenas.get_existing(1).is_none());
        let c32 = class_with(32);
        let a2 = s.arenas.get_existing(2).expect("arena 2");
        let (p0, _, _) = s.alloc_direct(
            s.arenas.get_existing(0).unwrap(),
            Tracked::assume_new(),
            c32,
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let (p2, _, _) = s.alloc_direct(
            a2,
            Tracked::assume_new(),
            c32,
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        let (q2, _, _) = s.alloc_direct(
            a2,
            Tracked::assume_new(),
            c32,
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert!(!p0.is_null() && !p2.is_null() && !q2.is_null());

        let live = collect(s, Tracked::assume_new());
        epoch(s, Tracked::assume_new(), Tracked::assume_new());
        let snap = snapshot(s, Tracked::assume_new());
        assert_eq!((live.narenas, snap.narenas), (2, 2));
        assert!(arena_stats(s, 1, Tracked::assume_new()).is_none());
        let s0 = arena_stats(s, 0, Tracked::assume_new()).unwrap();
        let s2 = arena_stats(s, 2, Tracked::assume_new()).unwrap();
        assert_eq!((s0.index, s2.index), (0, 2));
        assert_eq!((s0.bins[c32].curregs, s2.bins[c32].curregs), (1, 2));
        let size = CLASSES[c32].size();
        assert_eq!(s2.allocated(), 2 * size);
        assert_eq!(snap.totals.allocated, 3 * size);
        assert_eq!(live.totals.allocated, 3 * size);
        // The scan never builds the hole.
        assert!(s.arenas.get_existing(1).is_none());
    }

    /// With a second arena the per-arena figures split by owner: slabs and
    /// runs count where they were allocated, while the huge free extents
    /// and the metadata go to arena 0. The arenas sum to the totals.
    #[test]
    fn arenas_split_by_owner_and_sum_to_the_totals() {
        let s = shared();
        let t = TestHeap::new(s);
        let a1 = s
            .arenas
            .create(
                s,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            )
            .expect("arena 1");
        assert_eq!(a1, 1);
        let c32 = class_with(32);
        let p0 = t.alloc(c32);
        let (p1, _) = t.heap().alloc_in(
            s,
            a1,
            c32,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert!(!p1.is_null());
        let size = 400 * 1024;
        let (h1, _, _) = t
            .heap()
            .alloc_huge_in(s, size, 16, a1, Tracked::assume_new());
        assert!(!h1.is_null());
        let usable = s.huge.usable_for(size).unwrap();

        epoch(s, Tracked::assume_new(), Tracked::assume_new());
        let snap = snapshot(s, Tracked::assume_new());
        assert_eq!(snap.narenas, 2);
        let s0 = arena_stats(s, 0, Tracked::assume_new()).unwrap();
        let s1 = arena_stats(s, 1, Tracked::assume_new()).unwrap();
        assert!(arena_stats(s, 2, Tracked::assume_new()).is_none());
        assert_eq!((s0.index, s1.index), (0, 1));
        assert_eq!(s0.bins[c32].curslabs, 1);
        assert_eq!(s1.bins[c32].curslabs, 1);
        assert_eq!(
            s1.bins[c32].curregs,
            s.arena(1, Tracked::assume_new())
                .unwrap()
                .0
                .class(c32)
                .curregs()
        );
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
        epoch(s, Tracked::assume_new(), Tracked::assume_new());
        let s0 = arena_stats(s, 0, Tracked::assume_new()).unwrap();
        let s1 = arena_stats(s, 1, Tracked::assume_new()).unwrap();
        assert_eq!((s1.large_runs, s1.large_allocated), (0, 0));
        assert_eq!(
            s0.huge_dirty,
            (s.huge.usable_pages(size).unwrap() + GUARD_PAGES) * page_size()
        );
        assert_eq!(s1.huge_dirty, 0);
        assert_eq!(s1.bins[c32].empty_slabs, 1);
        assert_eq!(s0.bins[c32].empty_slabs, 1);
        let tot = snapshot(s, Tracked::assume_new()).totals;
        assert_eq!(tot.resident, s0.resident + s1.resident);
        assert!(tot.identities_hold(), "{tot:?}");
        t.destroy();
    }

    /// Two `epoch`s at once each publish one whole collection: the stored
    /// arenas sum to the stored totals after every round, whichever writer
    /// stored last. Arena 0's bins and huge runs move between collections,
    /// so two collections differ and a mix of them shows.
    #[test]
    fn concurrent_epochs_leave_arenas_summing_to_the_totals() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicBool, Ordering};
        let s = shared();
        // More arenas widen the window between the first arena's store
        // and the totals' store.
        for _ in 0..15 {
            s.arenas
                .create(
                    s,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                )
                .expect("arena");
        }
        let stop = AtomicBool::new(false);
        // Stops the traffic thread even if a round panics, so a failure
        // fails the test instead of hanging it.
        struct Stop<'a>(&'a AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let mut bad = None;
        std::thread::scope(|sc| {
            let _stop = Stop(&stop);
            sc.spawn(|| {
                let t = TestHeap::new(s);
                let c = class_with(64);
                while !stop.load(Ordering::Relaxed) {
                    let (h, _) = t.alloc_huge(300 * 1024, 16);
                    let ps: Vec<*mut u8> = (0..64).map(|_| t.alloc(c)).collect();
                    for p in ps {
                        t.free(p);
                    }
                    t.free(h);
                    t.flush_all();
                }
                t.destroy();
            });
            for round in 0..2000 {
                let go = Barrier::new(2);
                std::thread::scope(|w| {
                    for _ in 0..2 {
                        w.spawn(|| {
                            go.wait();
                            epoch(s, Tracked::assume_new(), Tracked::assume_new());
                        });
                    }
                });
                let snap = snapshot(s, Tracked::assume_new());
                let mut sum = Totals::EMPTY;
                for a in s.arenas.iter() {
                    sum.add_arena(&a.stats(Tracked::assume_new()));
                }
                if sum != snap.totals {
                    bad = Some((round, sum, snap.totals));
                    break;
                }
            }
        });
        assert_eq!(bad, None, "(round, arenas summed, stored totals)");
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
        let ok = Totals {
            allocated: 1,
            active: 2,
            metadata: 1,
            resident: 3,
            retained: 4,
            mapped: 7,
        };
        assert!(ok.identities_hold());
        assert!(!Totals { mapped: 8, ..ok }.identities_hold());
        assert!(!Totals { allocated: 3, ..ok }.identities_hold());
        assert!(
            !Totals {
                resident: 1,
                mapped: 5,
                ..ok
            }
            .identities_hold()
        );
        assert_eq!(ArenaStats::empty(7).index, 7);
        assert_eq!(ArenaStats::empty(7).bins[0], BinStats::EMPTY);
    }
}
