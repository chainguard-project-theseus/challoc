//! The slab sets: a lazily committed table in the metadata region
//! ([`Lazy`]), a [`TwoLevelBitmap`] over one ([`Set`]) and one arena's
//! interleaved table of every class's three sets ([`SetTable`]).

use crate::bitmap::TwoLevelBitmap;
use crate::layout::{CLASSES, LG_MAX_SLAB, LG_MIN_SLAB, NCLASSES, Regions};
#[cfg(verus_only)]
use crate::layout::{class_table, process_regions};
use crate::lock::{Lock, RawLock};
use crate::meta::Meta;
use crate::sys::Errno;
use vstd::prelude::*;

verus! {

/// `1 << k` is `pow2(k)`.
broadcast proof fn lemma_shl_pow2(k: u64)
    requires
        k < 64,
    ensures
        #[trigger] (1usize << k) as nat == vstd::arithmetic::power2::pow2(k as nat),
{
    vstd::bits::lemma_usize_pow2_no_overflow(k as nat);
    vstd::bits::lemma_usize_shl_is_mul(1, k as usize);
    assert((1usize << k) == (1usize << (k as usize))) by (bit_vector);
}

/// A never-freed table in the metadata region, committed page by page as
/// the high-water mark grows (a table of at most `meta::EAGER_MAX` bytes
/// is carved writable from the object side and the commits are accounting
/// only; `meta.rs`, the module doc). Shared with `huge/`.
pub(crate) struct Lazy {
    pub(crate) base: usize,
    len: usize,
    committed: usize,
}

impl Lazy {
    /// A table of no bytes: `ensure` is a no-op on it.
    pub(crate) exec const EMPTY: Lazy = Lazy { base: 0, len: 0, committed: 0 };

    /// The table's first byte.
    pub(crate) closed spec fn spec_base(&self) -> usize {
        self.base
    }

    /// [`Lazy::spec_base`], for modules that cannot see its closed body.
    #[inline(always)]
    pub(crate) fn start(&self) -> (r: usize)
        ensures
            r == self.spec_base(),
    {
        self.base
    }

    /// The table's length in bytes.
    pub(crate) closed spec fn spec_len(&self) -> usize {
        self.len
    }

    /// Bytes committed so far.
    pub(crate) closed spec fn spec_committed(&self) -> usize {
        self.committed
    }

    pub(crate) fn reserve(meta: &Meta, len: usize, align: usize) -> (r: Option<Lazy>)
        requires
            crate::meta::is_pow2(align),
        ensures
            r matches Some(l) ==> l.spec_base() + l.spec_len() <= crate::meta::va_limit() && l.spec_len() == len && l.spec_committed() == 0,
            r matches Some(l) ==> meta.holds(l.spec_base() as int, len as int),
    {
        proof {
            broadcast use lemma_shl_pow2;
        }
        // With metadata THP on (`thp.rs`) the table starts on a folio
        // boundary, so the rounded commits of `ensure` cover whole aligned
        // folios and the kernel can map them as such.
        let base = meta.reserve_table(len, align.max(crate::thp::meta_folio_size()))?;
        Some(Lazy {
            base,
            len,
            committed: 0,
        })
    }
}

/// A slab-number (or huge-page) set: a [`TwoLevelBitmap`] over a lazily
/// committed table, either its own ([`Set::new`], `huge/`) or one view
/// of an arena's [`SetTable`] ([`Set::in_table`]). Shared with `huge/`.
pub(crate) struct Set {
    pub(crate) map: TwoLevelBitmap,
    pub(crate) store: Store,
    pub(crate) l1_words: usize,
}

pub(crate) enum Store {
    Own(Lazy),
    /// The arena's table, which outlives the set (it is a field of the
    /// same `Arena`, or leaked by the tests), and the set's group in it.
    Shared(*const SetTable, u8),
}

/// Slab sets per class ([`ClassState`]: partial, empty, purged).
pub const SETS_PER_CLASS: usize = 3;
/// Set table groups: one per slab size (`Class::lg_slab`), since classes
/// with the same slab size have the same slab count.
pub const SET_GROUPS: usize = (LG_MAX_SLAB - LG_MIN_SLAB + 1) as usize;

/// One arena's slab sets for every class: one interleaved, lazily
/// committed table per slab size, holding the `SETS_PER_CLASS` sets of
/// every class of that slab size as chunked [`TwoLevelBitmap`] views (row
/// `r` of the set at `slot = kind * count + pos` is at `base + 8 * (r *
/// stride + slot)`, `stride = SETS_PER_CLASS * count`). Rows are committed
/// as a prefix while slab numbers grow, and a small heap touches only row 0
/// (level-1 word 0) and row 1 (level-0 word 0) of the groups it uses, one
/// page per (arena, slab size). One page-aligned table per set would cost
/// one or two pages per (arena, class), which at 16 arenas puts a small
/// multi-threaded heap's RSS well above jemalloc's.
pub struct SetTable {
    pub(crate) groups: [Group; SET_GROUPS],
    /// Slab count of each group's classes.
    pub(crate) nslabs: [usize; SET_GROUPS],
    /// Position of each class within its group.
    pub(crate) pos: [u8; NCLASSES],
    /// Commit watermarks; taken by the fork handlers after the arena's
    /// class locks, which is the order `take_group` uses.
    pub(crate) tables: Lock<[Lazy; SET_GROUPS]>,
    pub(crate) page: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct Group {
    pub(crate) base: usize,
    pub(crate) stride: usize,
    pub(crate) count: usize,
}

impl SetTable {
    /// Group of a class: its slab size.
    #[inline]
    pub fn group_of(class: usize) -> (r: usize)
        requires
            class < NCLASSES,
        ensures
            r < SET_GROUPS,
            r == class_table()[class as int].lg_slab - LG_MIN_SLAB,
    {
        proof {
            broadcast use crate::vspec::class::axiom_class_table_wf;
            assert(class_table()[class as int].wf());
        }
        (CLASSES.table()[class].lg_slab as u32 - LG_MIN_SLAB) as usize
    }
}

impl SetTable {
    /// The commit lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> &RawLock {
        self.tables.raw()
    }
}

impl Lazy {
    /// Make the first `upto` bytes committed. The table's end rounded up
    /// to `page` bounds what is ever committed, so `committed <= len +
    /// page` holds after every call with the same `page`.
    pub(crate) fn ensure(&mut self, meta: &Meta, page: usize, upto: usize) -> (r: Result<(), Errno>)
        requires
            1 <= page <= 0x8000_0000_0000_0000,
            upto <= 0x4000_0000_0000_0000,
            old(self).spec_len() <= 0x4000_0000_0000_0000,
            old(self).spec_committed() <= old(self).spec_len() + page,
            old(self).spec_base() + old(self).spec_len() + page <= usize::MAX,
        ensures
            final(self).spec_base() == old(self).spec_base(),
            final(self).spec_len() == old(self).spec_len(),
            final(self).spec_committed() <= final(self).spec_len() + page,
    {
        // With metadata THP on (`thp.rs`) the commit is rounded to the
        // folio so the first touch of a table page maps one folio; the
        // table's own end is still the limit.
        let unit = crate::thp::meta_folio_size().max(page);
        proof {
            let (a, b, m, n) = ((upto + unit - 1) as usize, (self.len + page - 1) as usize, sub(unit, 1), sub(page, 1));
            assert(unit <= 0x8000_0000_0000_0000usize) by {
                if unit != page {
                    let k = choose|k: u64| k < 64 && unit == #[trigger] (1usize << k);
                    assert(k < 64 ==> (1usize << k) <= 0x8000_0000_0000_0000usize) by (bit_vector);
                }
            }
            assert(b & !n <= b) by (bit_vector);
            assert(a & !m <= a) by (bit_vector);
        }
        let want = ((upto + unit - 1) & !(unit - 1)).min((self.len + page - 1) & !(page - 1));
        if want > self.committed {
            meta.commit(self.base + self.committed, want - self.committed)?;
            self.committed = want;
        }
        Ok(())
    }
}

} // verus!

// SAFETY: a `SetTable` is shared by the arena's class states; its groups
// and positions are immutable after `new` and the commit watermarks are
// behind `tables`' lock.
unsafe impl Sync for SetTable {}
unsafe impl Send for SetTable {}

verus! {

/// The set table group of class `c` ([`SetTable::group_of`]): its slab
/// size's index.
pub(crate) open spec fn group_spec(c: int) -> int {
    class_table()[c].lg_slab - LG_MIN_SLAB
}

/// Class `c`'s part of the arrays [`SetTable::new`] fills in its first
/// pass: its group is a group, its position lies inside the group's
/// count, and the group's slab count is the class's
/// (`Regions::spec_nslabs`), at least one and at most `2^26`.
pub(crate) open spec fn placed(pos: [u8; NCLASSES], groups: [Group; SET_GROUPS], nslabs: [usize; SET_GROUPS], regions: Regions, c: int) -> bool {
    let g = group_spec(c);
    &&& 0 <= g < SET_GROUPS
    &&& (pos[c] as int) < groups[g].count
    &&& nslabs[g] as int == regions.spec_nslabs(c)
    &&& 1 <= nslabs[g] <= 0x400_0000
}

/// Class `c`'s group after the second pass: its stride is three sets per
/// class and its rows ([`TwoLevelBitmap::chunked_rows`]) were reserved
/// from `base`, below the metadata limit.
pub(crate) open spec fn reserved(groups: [Group; SET_GROUPS], nslabs: [usize; SET_GROUPS], c: int) -> bool {
    let (grp, n) = (groups[group_spec(c)], nslabs[group_spec(c)]);
    &&& grp.stride == 3 * grp.count
    &&& grp.base + 8 * (65 * (((n + 63) / 64 + 63) / 64)) * grp.stride <= crate::meta::va_limit()
}

impl SetTable {
    /// What [`SetTable::new`] builds over `regions` for class `c`
    /// ([`placed`] and [`reserved`]).
    pub(crate) open spec fn class_ok(&self, regions: Regions, c: int) -> bool {
        &&& placed(self.pos, self.groups, self.nslabs, regions, c)
        &&& reserved(self.groups, self.nslabs, c)
    }

    /// [`SetTable::class_ok`] for every class.
    pub(crate) open spec fn over(&self, regions: Regions) -> bool {
        forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.class_ok(regions, c)
    }
}

/// `table` as the pointer [`Store::Shared`] keeps. Trusted: Verus has no
/// reference-to-pointer cast; nothing is claimed about the pointer.
#[verifier::external_body]
#[inline(always)]
fn set_table_ptr(table: &SetTable) -> *const SetTable {
    table
}

impl Set {
    /// Set `kind` (0..`SETS_PER_CLASS`) of `class` in `table`, over the
    /// class's slab numbers: a well-formed empty set over the group's slab
    /// count. `table` must outlive the set.
    pub(crate) fn in_table(table: &SetTable, class: usize, kind: usize) -> (r: Set)
        requires
            class < NCLASSES,
            kind < SETS_PER_CLASS,
            table.class_ok(process_regions(), class as int),
        ensures
            r.map.wf(),
            r.map.spec_nbits() == table.nslabs[group_spec(class as int)],
            r.map@ == vstd::set::Set::<nat>::empty(),
    {
        debug_assert!(kind < SETS_PER_CLASS);
        let g = SetTable::group_of(class);
        let group = &table.groups[g];
        proof {
            let (n, s, c, b) = (table.nslabs[g as int] as int, group.stride as int, group.count as int, group.base as int);
            let l1 = ((n + 63) / 64 + 63) / 64;
            assert(1 <= l1 <= 0x4001);
            assert(1 <= c && s == 3 * c);
            assert(kind * c + table.pos[class as int] < 3 * c) by (nonlinear_arith)
                requires kind < 3, table.pos[class as int] < c;
            assert(s <= 8 * (65 * l1) * s) by (nonlinear_arith) requires l1 >= 1, s >= 0;
        }
        let slot = kind * group.count + table.pos[class] as usize;
        let nslabs = table.nslabs[g];
        proof {
            let (n, s) = (nslabs as int, group.stride as int);
            let l1 = ((n + 63) / 64 + 63) / 64;
            assert(65 * l1 * s * 8 == 8 * (65 * l1) * s) by (nonlinear_arith);
        }
        // SAFETY: the slot's words are `base + 8 * (slot + r * stride)`,
        // disjoint from every other slot's, zero, never freed; the rows a
        // slab's bit needs are committed by `ensure` before the class
        // touches it (`take_group`).
        let map = unsafe {
            TwoLevelBitmap::from_raw_chunked(nslabs, crate::vspec::addr::addr_to_ptr(group.base + 8 * slot) as *mut u64, group.stride)
        };
        proof {
            map.lemma_fresh(nslabs);
        }
        Set { map, store: Store::Shared(set_table_ptr(table), g as u8), l1_words: TwoLevelBitmap::l1_words(nslabs) }
    }
}

} // verus!

verus! {

impl Set {
    /// A set over `0..nslabs` on its own table.
    pub(crate) fn new(meta: &Meta, page: usize, nslabs: usize) -> (r: Option<Set>)
        requires
            crate::meta::is_pow2(page),
            page <= 0x8000_0000_0000_0000,
            nslabs <= 0x1_0000_0000,
        ensures
            r matches Some(s) ==> s.map.wf() && s.map.spec_nbits() == nslabs,
            r matches Some(s) ==> (meta.is_process() ==> s.map.in_meta()),
    {
        proof {
            let k = choose|k: nat| k < 64 && page as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
            vstd::arithmetic::power2::lemma_pow2_pos(k);
        }
        let l0_words = nslabs.div_ceil(64);
        let l1_words = l0_words.div_ceil(64);
        let mut table = Lazy::reserve(meta, 8 * TwoLevelBitmap::words_needed(nslabs), page)?;
        // Level 1 is small and touched by every operation: commit it now.
        table.ensure(meta, page, 8 * l1_words).ok()?;
        // SAFETY: the words come from a fresh, zero, never-freed reservation.
        let map = unsafe { TwoLevelBitmap::from_raw(nslabs, crate::vspec::addr::addr_to_ptr(table.base) as *mut u64) };
        proof {
            map.lemma_fresh(nslabs);
        }
        Some(Set { map, store: Store::Own(table), l1_words })
    }
}

} // verus!

verus! {

impl Set {
    /// Commit the level-0 words that hold every bit up to and including
    /// `slab`. Only the store changes; the bitmap view is the same.
    ///
    /// Trusted: `Lazy::ensure`'s preconditions are facts about the table
    /// `new` or `in_table` sized, which no invariant of `Set` records, and
    /// the shared store is followed through a raw pointer.
    #[verifier::external_body]
    pub(crate) fn ensure(&mut self, meta: &Meta, page: usize, slab: usize) -> (r: Result<(), Errno>)
        ensures
            final(self).map == old(self).map,
            final(self).l1_words == old(self).l1_words,
    {
        match &mut self.store {
            Store::Own(table) => table.ensure(meta, page, 8 * (self.l1_words + slab / 64 + 1)),
            // SAFETY: the table outlives the set (`Store::Shared`).
            Store::Shared(table, g) => unsafe { &**table }.ensure(meta, *g as usize, slab),
        }
    }
}

} // verus!

verus! {

/// Assumption (`lock.rs`): the commit watermarks behind
/// [`SetTable::tables`] keep no invariant verified code relies on: their
/// one holder, `SetTable::ensure`, is outside `verus!`, and no verified
/// code reads them.
pub broadcast axiom fn axiom_set_tables_lock_inv(v: [Lazy; SET_GROUPS])
    ensures
        #[trigger] crate::lock::lock_inv(v),
;

impl SetTable {
    /// Reserve the tables (address space only; rows are committed by
    /// `ensure`). `None` if the metadata region cannot hold them.
    pub(crate) fn new(meta: &Meta, regions: &Regions) -> (r: Option<SetTable>)
        requires
            regions.wf(),
        ensures
            r matches Some(t) ==> t.over(*regions),
    {
        broadcast use axiom_set_tables_lock_inv;
        let page = regions.page_size();
        proof {
            crate::meta::lemma_is_pow2_shl(regions.lg_page as usize);
            let lg = regions.lg_page;
            assert((1usize << lg) == (1usize << (lg as usize))) by (bit_vector) requires lg < 64;
        }
        let mut groups = [Group { base: 0, stride: 0, count: 0 }; SET_GROUPS];
        let mut nslabs = [0usize; SET_GROUPS];
        let mut pos = [0u8; NCLASSES];
        for c in 0..NCLASSES
            invariant
                regions.wf(),
                forall|g: int| 0 <= g < SET_GROUPS ==> #[trigger] groups[g].count <= c,
                forall|g: int| 0 <= g < SET_GROUPS ==> #[trigger] nslabs[g] <= 0x400_0000,
                forall|d: int| 0 <= d < c ==> #[trigger] placed(pos, groups, nslabs, *regions, d),
        {
            let g = Self::group_of(c);
            proof {
                crate::vspec::class::axiom_class_table_wf(c as int);
                let (lr, ls) = (regions.lg_region, class_table()[c as int].lg_slab);
                assert(1 <= (1usize << lr) >> ls <= 0x400_0000) by (bit_vector)
                    requires 22 <= lr <= 38, 12 <= ls <= 22;
            }
            let ghost (pos0, groups0, nslabs0) = (pos, groups, nslabs);
            pos[c] = groups[g].count as u8;
            let mut grp = groups[g];
            grp.count += 1;
            groups[g] = grp;
            nslabs[g] = regions.nslabs(c);
            proof {
                assert forall|d: int| 0 <= d < c + 1 implies #[trigger] placed(pos, groups, nslabs, *regions, d) by {
                    if d < c {
                        assert(placed(pos0, groups0, nslabs0, *regions, d));
                        if group_spec(d) == g {
                            assert(class_table()[d].lg_slab == class_table()[c as int].lg_slab);
                        }
                    }
                }
            }
        }
        // `[Lazy::EMPTY; SET_GROUPS]` spelled out: Verus has no array fill of
        // a type that is not `Copy`. A change of `SET_GROUPS` fails to compile
        // here.
        proof {
            assert forall|d: int| 0 <= d < NCLASSES && group_spec(d) < 0 implies #[trigger] reserved(groups, nslabs, d) by {
                assert(placed(pos, groups, nslabs, *regions, d));
            }
        }
        let mut tables = [Lazy::EMPTY, Lazy::EMPTY, Lazy::EMPTY, Lazy::EMPTY, Lazy::EMPTY, Lazy::EMPTY, Lazy::EMPTY, Lazy::EMPTY, Lazy::EMPTY, Lazy::EMPTY, Lazy::EMPTY];
        for g in 0..SET_GROUPS
            invariant
                forall|h: int| 0 <= h < SET_GROUPS ==> #[trigger] groups[h].count <= NCLASSES,
                forall|h: int| 0 <= h < SET_GROUPS ==> #[trigger] nslabs[h] <= 0x400_0000,
                forall|d: int| 0 <= d < NCLASSES ==> #[trigger] placed(pos, groups, nslabs, *regions, d),
                forall|d: int| 0 <= d < NCLASSES && group_spec(d) < g ==> #[trigger] reserved(groups, nslabs, d),
                crate::meta::is_pow2(page),
        {
            if groups[g].count != 0 {
                let ghost groups0 = groups;
                let mut grp = groups[g];
                grp.stride = SETS_PER_CLASS * grp.count;
                let rows = TwoLevelBitmap::chunked_rows(nslabs[g]);
                let ghost n = nslabs[g as int] as int;
                proof {
                    assert(((n + 63) / 64 + 63) / 64 <= 0x4001) by (nonlinear_arith) requires 0 <= n <= 0x400_0000;
                    assert(rows <= 65 * 0x4001);
                    assert(grp.stride <= 3 * NCLASSES);
                    let r8 = 8 * rows;
                    assert(r8 * grp.stride <= 8 * (65 * 0x4001) * (3 * NCLASSES)) by (nonlinear_arith)
                        requires r8 <= 8 * 65 * 0x4001, grp.stride <= 3 * NCLASSES, grp.stride >= 0;
                }
                let t = Lazy::reserve(meta, 8 * rows * grp.stride, page)?;
                grp.base = t.base;
                tables[g] = t;
                groups[g] = grp;
                proof {
                    assert(8 * rows * grp.stride == 8 * (65 * (((n + 63) / 64 + 63) / 64)) * grp.stride) by (nonlinear_arith)
                        requires rows == 65 * (((n + 63) / 64 + 63) / 64);
                    assert forall|d: int| 0 <= d < NCLASSES implies #[trigger] placed(pos, groups, nslabs, *regions, d) by {
                        assert(placed(pos, groups0, nslabs, *regions, d));
                    }
                    assert forall|d: int| 0 <= d < NCLASSES && group_spec(d) < g + 1 implies #[trigger] reserved(groups, nslabs, d) by {
                        assert(placed(pos, groups0, nslabs, *regions, d));
                        if group_spec(d) < g {
                            assert(reserved(groups0, nslabs, d));
                        }
                    }
                }
            } else {
                proof {
                    // No class is in an empty group.
                    assert forall|d: int| 0 <= d < NCLASSES && group_spec(d) < g + 1 implies #[trigger] reserved(groups, nslabs, d) by {
                        assert(placed(pos, groups, nslabs, *regions, d));
                    }
                }
            }
        }
        proof {
            assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] placed(pos, groups, nslabs, *regions, c)
                && reserved(groups, nslabs, c) by {
                assert(placed(pos, groups, nslabs, *regions, c));
            }
        }
        Some(SetTable {
            groups,
            nslabs,
            pos,
            tables: Lock::new(tables),
            page,
        })
    }
}

} // verus!

impl SetTable {
    /// Commit the rows of group `g` up to the level-0 word of `slab` (and
    /// so its chunk's level-1 word and every row before).
    fn ensure(&self, meta: &Meta, g: usize, slab: usize) -> Result<(), Errno> {
        let row = TwoLevelBitmap::chunked_l0_row(slab / 64);
        self.tables.lock()[g].ensure(meta, self.page, 8 * (row + 1) * self.groups[g].stride)
    }

    /// Tests: committed byte ranges of the groups.
    #[cfg(test)]
    pub fn committed_ranges(&self) -> [(usize, usize); SET_GROUPS] {
        let t = self.tables.lock();
        let mut out = [(0usize, 0usize); SET_GROUPS];
        for g in 0..SET_GROUPS {
            out[g] = (t[g].base, t[g].committed);
        }
        out
    }
}
