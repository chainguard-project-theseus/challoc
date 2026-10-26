//! The slab sets: a lazily committed table in the metadata region
//! ([`Lazy`]), a [`TwoLevelBitmap`] over one ([`Set`]) and one arena's
//! interleaved table of every class's three sets ([`SetTable`]).

use crate::bitmap::TwoLevelBitmap;
use crate::layout::{CLASSES, LG_MAX_SLAB, LG_MIN_SLAB, NCLASSES, Regions};
use crate::lock::{Lock, LockOrder, RawLock};
use crate::meta::Meta;
use crate::sys::Errno;
use crate::vspec::zero::ZeroRaw;
use vstd::prelude::{Ghost, Map, Tracked, int};
use vstd::raw_ptr::PointsTo;

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
    pub(crate) const EMPTY: Lazy = Lazy {
        base: 0,
        len: 0,
        committed: 0,
    };

    /// The table's first byte.
    #[inline(always)]
    pub(crate) fn start(&self) -> usize {
        self.base
    }

    pub(crate) fn reserve(
        meta: &Meta,
        len: usize,
        align: usize,
        _raw: Tracked<&mut Option<ZeroRaw>>,
        _ord: Tracked<&mut LockOrder>,
    ) -> Option<Lazy> {
        // With metadata THP on (`thp.rs`) the table starts on a folio
        // boundary, so the rounded commits of `ensure` cover whole aligned
        // folios and the kernel can map them as such.
        // The claim to the pages and the zero permission are taken on
        // trust until the table carries them.
        let base = meta
            .reserve_table(
                len,
                align.max(crate::thp::meta_folio_size()),
                Tracked::assume_new(),
            )
            .0?;
        Some(Lazy {
            base,
            len,
            committed: 0,
        })
    }

    /// Make the first `upto` bytes committed.
    pub(crate) fn ensure(
        &mut self,
        meta: &Meta,
        page: usize,
        upto: usize,
        _ord: Tracked<&mut LockOrder>,
    ) -> Result<(), Errno> {
        // With metadata THP on (`thp.rs`) the commit is rounded to the
        // folio so the first touch of a table page maps one folio; the
        // table's own end is still the limit.
        let unit = crate::thp::meta_folio_size().max(page);
        let want = ((upto + unit - 1) & !(unit - 1)).min((self.len + page - 1) & !(page - 1));
        if want > self.committed {
            meta.commit(
                self.base + self.committed,
                want - self.committed,
                Ghost::assume_new(),
                Tracked::assume_new(),
            )?;
            self.committed = want;
        }
        Ok(())
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
    /// same `Arena`, or leaked by the tests), the set's group in it, and
    /// (ghost) the frozen permission to the table.
    Shared(*const SetTable, u8, Tracked<&'static PointsTo<SetTable>>),
}

impl Set {
    pub(crate) fn new(
        meta: &Meta,
        page: usize,
        nslabs: usize,
        _ord: Tracked<&mut LockOrder>,
    ) -> Option<Set> {
        let l0_words = nslabs.div_ceil(64);
        let l1_words = l0_words.div_ceil(64);
        let mut table = Lazy::reserve(
            meta,
            8 * TwoLevelBitmap::words_needed(nslabs),
            page,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )?;
        // Level 1 is small and touched by every operation: commit it now.
        table
            .ensure(meta, page, 8 * l1_words, Tracked::assume_new())
            .ok()?;
        // SAFETY: the words come from a fresh, zero, never-freed reservation.
        // Their ghost permission, as zero bytes, is trusted on the same
        // grounds.
        let map = unsafe {
            TwoLevelBitmap::from_raw(
                nslabs,
                crate::vspec::addr::addr_to_ptr(table.base) as *mut u64,
                Tracked::assume_new(),
            )
        };
        Some(Set {
            map,
            store: Store::Own(table),
            l1_words,
        })
    }

    /// Set `kind` (0..`SETS_PER_CLASS`) of `class` in the table at `tp`,
    /// over the class's slab numbers. The table must outlive the set.
    /// Ghost: `perm`, the frozen permission to the table, which the set
    /// keeps, and the permission to the slot's words.
    pub(crate) fn in_table(
        tp: *const SetTable,
        perm: Tracked<&'static PointsTo<SetTable>>,
        class: usize,
        kind: usize,
        _w: Tracked<ZeroRaw>,
    ) -> Set {
        // SAFETY: the table outlives the set (the caller's contract).
        let table = unsafe { &*tp };
        debug_assert!(kind < SETS_PER_CLASS);
        let g = SetTable::group_of(class);
        let group = &table.groups[g];
        let slot = kind * group.count + table.pos[class] as usize;
        let nslabs = table.nslabs[g];
        // SAFETY: the slot's words are `base + 8 * (slot + r * stride)`,
        // disjoint from every other slot's, zero, never freed; the rows a
        // slab's bit needs are committed by `ensure` before the class
        // touches it (`take_group`). Their ghost permission, as zero bytes,
        // is trusted on the same grounds.
        let map = unsafe {
            TwoLevelBitmap::from_raw_chunked(
                nslabs,
                crate::vspec::addr::addr_to_ptr(group.base + 8 * slot) as *mut u64,
                group.stride,
                Tracked::assume_new(),
            )
        };
        Set {
            map,
            store: Store::Shared(tp, g as u8, perm),
            l1_words: TwoLevelBitmap::l1_words(nslabs),
        }
    }

    /// Commit the level-0 words that hold every bit up to and including
    /// `slab`.
    pub(crate) fn ensure(
        &mut self,
        meta: &Meta,
        page: usize,
        slab: usize,
        _ord: Tracked<&mut LockOrder>,
    ) -> Result<(), Errno> {
        match &mut self.store {
            Store::Own(table) => table.ensure(
                meta,
                page,
                8 * (self.l1_words + slab / 64 + 1),
                Tracked::assume_new(),
            ),
            // SAFETY: the table outlives the set (`Store::Shared`).
            Store::Shared(table, g, _) => {
                unsafe { &**table }.ensure(meta, *g as usize, slab, Tracked::assume_new())
            }
        }
    }
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
    /// class locks and the slab table locks, which is the order
    /// `take_group` uses.
    pub(crate) tables: Lock<[Lazy; SET_GROUPS]>,
    pub(crate) page: usize,
}

// SAFETY: a `SetTable` is shared by the arena's class states; its groups
// and positions are immutable after `new` and the commit watermarks are
// behind `tables`' lock.
unsafe impl Sync for SetTable {}
unsafe impl Send for SetTable {}

#[derive(Clone, Copy)]
pub(crate) struct Group {
    pub(crate) base: usize,
    pub(crate) stride: usize,
    pub(crate) count: usize,
}

impl SetTable {
    /// Group of a class: its slab size.
    #[inline]
    pub fn group_of(class: usize) -> usize {
        (CLASSES.table()[class].lg_slab as u32 - LG_MIN_SLAB) as usize
    }

    /// Reserve the tables (address space only; rows are committed by
    /// `ensure`). `None` if the metadata region cannot hold them.
    // Indexed loop, spelled out for Verus like the rest of this function.
    #[allow(clippy::needless_range_loop)]
    pub(crate) fn new(
        meta: &Meta,
        regions: &Regions,
        _a: Ghost<u16>,
        _s: Tracked<&mut Map<(int, int), ZeroRaw>>,
        _o: Tracked<&mut LockOrder>,
    ) -> Option<SetTable> {
        let page = regions.page_size();
        let mut groups = [Group {
            base: 0,
            stride: 0,
            count: 0,
        }; SET_GROUPS];
        let mut nslabs = [0usize; SET_GROUPS];
        let mut pos = [0u8; NCLASSES];
        for c in 0..NCLASSES {
            let g = Self::group_of(c);
            pos[c] = groups[g].count as u8;
            let mut grp = groups[g];
            grp.count += 1;
            groups[g] = grp;
            nslabs[g] = regions.nslabs(c);
        }
        // One `Lazy::EMPTY` per group, spelled out for Verus; the type
        // `Lock::new` takes checks the count against `SET_GROUPS`.
        let mut tables = [
            Lazy::EMPTY,
            Lazy::EMPTY,
            Lazy::EMPTY,
            Lazy::EMPTY,
            Lazy::EMPTY,
            Lazy::EMPTY,
            Lazy::EMPTY,
            Lazy::EMPTY,
            Lazy::EMPTY,
            Lazy::EMPTY,
            Lazy::EMPTY,
        ];
        for g in 0..SET_GROUPS {
            if groups[g].count != 0 {
                let mut grp = groups[g];
                grp.stride = SETS_PER_CLASS * grp.count;
                let rows = TwoLevelBitmap::chunked_rows(nslabs[g]);
                let t = Lazy::reserve(
                    meta,
                    8 * rows * grp.stride,
                    page,
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                )?;
                grp.base = t.base;
                tables[g] = t;
                groups[g] = grp;
            }
        }
        Some(SetTable {
            groups,
            nslabs,
            pos,
            tables: Lock::new(tables, Ghost::assume_new()),
            page,
        })
    }

    /// Commit the rows of group `g` up to the level-0 word of `slab` (and
    /// so its chunk's level-1 word and every row before).
    fn ensure(
        &self,
        meta: &Meta,
        g: usize,
        slab: usize,
        _ord: Tracked<&mut LockOrder>,
    ) -> Result<(), Errno> {
        let row = TwoLevelBitmap::chunked_l0_row(slab / 64);
        self.tables
            .lock(Tracked::assume_new(), Tracked::assume_new())[g]
            .ensure(
                meta,
                self.page,
                8 * (row + 1) * self.groups[g].stride,
                Tracked::assume_new(),
            )
    }

    /// Tests: committed byte ranges of the groups.
    #[cfg(test)]
    pub fn committed_ranges(&self) -> [(usize, usize); SET_GROUPS] {
        let t = self.tables.lock_t();
        let mut out = [(0usize, 0usize); SET_GROUPS];
        for g in 0..SET_GROUPS {
            out[g] = (t[g].base, t[g].committed);
        }
        out
    }

    /// The commit lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> &RawLock {
        self.tables.raw()
    }
}
