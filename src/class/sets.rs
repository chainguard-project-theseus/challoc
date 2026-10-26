//! The slab sets: a lazily committed table in the metadata region
//! ([`Lazy`]), a [`TwoLevelBitmap`] over one ([`Set`]) and one arena's
//! interleaved table of every class's three sets ([`SetTable`]).

use crate::bitmap::TwoLevelBitmap;
use crate::layout::{CLASSES, LG_MAX_SLAB, LG_MIN_SLAB, NCLASSES, Regions};
#[cfg(verus_keep_ghost)]
use crate::layout::{class_table, process_regions};
use crate::lock::{Lock, LockOrder, RawLock};
#[cfg(verus_keep_ghost)]
use crate::lock::{rank_meta, rank_sets};
use crate::meta::Meta;
use crate::sys::Errno;
#[cfg(verus_keep_ghost)]
use crate::vspec::addr::cast_provenance;
use crate::vspec::zero::ZeroRaw;
use vstd::prelude::*;
use vstd::raw_ptr::PointsTo;
use vstd::resource::map::GhostSubmap;
#[cfg(verus_keep_ghost)]
use vstd::set_lib::set_int_range;

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
    /// Ghost: the claim to the table's pages not committed yet, which
    /// [`Lazy::ensure`] gives `Meta::commit` as it commits them, and the
    /// side of the metadata region the table was reserved on
    /// ([`Meta::reserve_table`]).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    claim: Tracked<GhostSubmap<int, bool>>,
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    side: Ghost<bool>,
}

impl Lazy {
    /// The table lies below the metadata limit, so the rounding in
    /// [`Lazy::ensure`] does not wrap: what `reserve` proves of the
    /// reservation, and nothing moves `base` or `len` afterwards.
    #[verifier::type_invariant]
    spec fn inv(self) -> bool {
        self.base + self.len <= crate::meta::va_limit()
    }

    /// What [`Lazy::ensure`]'s commits need of a table of some bytes: its
    /// base and its commit watermark are on pages of the process
    /// ([`process_page`](crate::meta::process_page)), and while the
    /// watermark is below the table's end rounded up to pages, the table
    /// holds the claim to the pages from the watermark to there, of the
    /// process's `Meta` ([`process_pages_id`](crate::meta::process_pages_id)),
    /// all on the table's side. [`Lazy::reserve`] and `EMPTY` make tables
    /// that keep it, and `ensure` keeps it; each holder of a table keeps
    /// it with the table: in its own invariant ([`SetTable`]'s lock
    /// invariant, `axiom_set_tables_lock_inv`; the class tables' through
    /// `class::table::lazy_ok`), and `Set::ensure` requires it
    /// (`Set::ensure_ok`). Not a type invariant: `ensure` lends the claim
    /// to `Meta::commit` while the watermark is still the old one.
    pub(crate) closed spec fn claimed(self) -> bool {
        let p = crate::meta::process_page();
        let end = crate::meta::align_up_spec(self.len, p);
        self.len > 0 ==> {
            &&& p > 0
            &&& p & sub(p, 1) == 0
            &&& self.base % p == 0
            &&& self.committed % p == 0
            &&& self.committed < end ==> {
                &&& self.claim@.id() == crate::meta::process_pages_id()
                &&& self.claim@@.dom() == set_int_range(self.base + self.committed, self.base + end)
                &&& forall|a: int| #[trigger] self.claim@@.dom().contains(a) ==> self.claim@@[a] == self.side@
            }
        }
    }

    /// A table of no bytes: `ensure` is a no-op on it.
    pub(crate) exec const EMPTY: Lazy
        ensures
            Self::EMPTY.spec_base() == 0,
            Self::EMPTY.spec_len() == 0,
            Self::EMPTY.claimed(),
    {
        Lazy { base: 0, len: 0, committed: 0, claim: Tracked(GhostSubmap::dummy()), side: Ghost(false) }
    }

    /// The table's first byte.
    pub(crate) closed spec fn spec_base(&self) -> usize {
        self.base
    }

    /// The table's first byte.
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

    /// Reserve a table of `len` bytes aligned to `align` (and, with
    /// metadata THP on, to the folio), with the permission to its bytes,
    /// its length rounded up to a whole word, which read as zero and which
    /// [`Meta::reserve_table`] hands out once. The table keeps the claim
    /// to its pages ([`Lazy::claimed`]) that `reserve_table` hands out
    /// with it, cut to the table rounded up to pages.
    pub(crate) fn reserve(
        meta: &Meta,
        len: usize,
        align: usize,
        Tracked(raw): Tracked<&mut ZeroRaw>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: Option<Lazy>)
        requires
            crate::sys::may_syscall(),
            crate::meta::is_pow2(align),
            old(ord).allows(rank_meta()),
            meta.is_process(),
        ensures
            r matches Some(l) ==> l.spec_base() > 0 && l.spec_base() % align == 0,
            r matches Some(l) ==> l.spec_base() + (len + 7) / 8 * 8 <= usize::MAX,
            r matches Some(l) ==> l.spec_len() == len && l.spec_committed() == 0 && l.spec_base() + len <= crate::meta::va_limit(),
            r matches Some(l) ==> final(raw).is_range(l.spec_base() as int, (len + 7) / 8 * 8) && final(raw).provenance() == cast_provenance(),
            r matches Some(l) ==> l.claimed(),
            *final(ord) == *old(ord),
    {
        proof {
            broadcast use lemma_shl_pow2;
            use_type_invariant(meta);
        }
        // With metadata THP on (`thp.rs`) the table starts on a folio
        // boundary, so the rounded commits of `ensure` cover whole aligned
        // folios and the kernel can map them as such.
        let folio = crate::thp::meta_folio_size();
        let tracked mut claim = GhostSubmap::<int, bool>::dummy();
        let (base, Tracked(z)) = meta.reserve_table(len, align.max(folio), Tracked(&mut claim));
        let base = base?;
        proof {
            // The larger of two powers of two is a multiple of the smaller.
            crate::meta::lemma_pow2_bits(align);
            if folio > align {
                let k = choose|k: u64| k < 64 && folio == #[trigger] (1usize << k);
                let a = align;
                assert(folio % a == 0) by (bit_vector)
                    requires a & sub(a, 1) == 0, a > 0, k < 64, folio == 1usize << k, folio > a;
                let (q, r) = (base as int / folio as int, folio as int / a as int);
                vstd::arithmetic::div_mod::lemma_fundamental_div_mod(base as int, folio as int);
                vstd::arithmetic::div_mod::lemma_fundamental_div_mod(folio as int, a as int);
                assert(base as int == (q * r) * a) by (nonlinear_arith)
                    requires base as int == folio * q, folio as int == a * r;
                vstd::arithmetic::div_mod::lemma_mod_multiples_basic(q * r, a as int);
            }
            *raw = z;
        }
        // The claim to the table's pages, cut out of `claim`, which may
        // hold more.
        let ghost end = crate::meta::align_up_spec(len, crate::meta::process_page());
        let ghost side = claim@[base as int];
        let ghost c0 = claim@;
        let tracked mine = claim.split(set_int_range(base as int, base + end));
        proof {
            vstd::arithmetic::div_mod::lemma_small_mod(0, meta.page() as nat);
            assert forall|a: int| #[trigger] mine@.dom().contains(a) implies mine@[a] == side by {
                assert(c0.dom().contains(a));
                assert(c0[a] == mine@[a]);
            }
        }
        Some(Lazy { base, len, committed: 0, claim: Tracked(mine), side: Ghost(side) })
    }

    /// Make the first `upto` bytes committed. The table's end rounded up
    /// to `page` bounds what is ever committed, so `committed <= len +
    /// page` holds after every call with the same `page` (and the
    /// watermark only grows, so a call never breaks the bound). `page` is
    /// the process's and `meta` its allocator, whose claim to the pages
    /// past the watermark the table holds ([`Lazy::claimed`]); a commit
    /// gives `meta` the claim to the pages it commits.
    pub(crate) fn ensure(&mut self, meta: &Meta, page: usize, upto: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Result<(), Errno>)
        requires
            crate::sys::may_syscall(),
            1 <= page <= 0x8000_0000_0000_0000,
            upto <= 0x4000_0000_0000_0000,
            old(ord).allows(rank_meta()),
            meta.is_process(),
            page == crate::meta::process_page(),
            old(self).claimed(),
        ensures
            final(self).spec_base() == old(self).spec_base(),
            final(self).spec_len() == old(self).spec_len(),
            old(self).spec_committed() <= old(self).spec_len() + page ==> final(self).spec_committed() <= final(self).spec_len() + page,
            final(self).claimed(),
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(&*self);
        }
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
            proof {
                self.lemma_commit_range(page, upto, unit, want);
            }
            // The claim is lent to the commit, which takes the committed
            // pages' part of it, and comes back before the error return.
            let ghost c0 = self.claim@@;
            let tracked mut claim = self.claim.borrow_mut().take();
            let r = meta.commit(self.base + self.committed, want - self.committed, Ghost(self.side@), Tracked(&mut claim));
            proof {
                self.claim = Tracked(claim);
            }
            r?;
            proof {
                let end = crate::meta::align_up_spec(self.len, page);
                assert(self.claim@@.dom() =~= set_int_range(self.base + want, self.base + end));
                assert forall|a: int| #[trigger] self.claim@@.dom().contains(a) implies self.claim@@[a] == self.side@ by {
                    assert(c0.dom().contains(a));
                }
            }
            self.committed = want;
        }
        Ok(())
    }

    /// What [`Lazy::ensure`]'s commit of `[base + committed, base + want)`
    /// needs, for `want` the smaller of `upto` rounded up to `unit` (a
    /// power of two of at least the page, or the page) and the table's end
    /// rounded up to the page: the range is whole pages of the process's
    /// `Meta`, and the table's claim holds it.
    proof fn lemma_commit_range(&self, page: usize, upto: usize, unit: usize, want: usize)
        requires
            self.inv(),
            self.claimed(),
            page == crate::meta::process_page(),
            1 <= page <= 0x8000_0000_0000_0000,
            upto <= 0x4000_0000_0000_0000,
            unit == page || (unit > page && exists|k: u64| k < 64 && unit == #[trigger] (1usize << k)),
            unit <= 0x8000_0000_0000_0000,
            want == if ((upto + unit - 1) as usize) & !((unit - 1) as usize) <= ((self.len + page - 1) as usize) & !((page - 1) as usize) {
                ((upto + unit - 1) as usize) & !((unit - 1) as usize)
            } else {
                ((self.len + page - 1) as usize) & !((page - 1) as usize)
            },
            want > self.committed,
        ensures
            self.len > 0,
            (self.base + self.committed) % (page as int) == 0,
            (want - self.committed) % (page as int) == 0,
            want % page == 0,
            self.base + want <= usize::MAX,
            self.committed < crate::meta::align_up_spec(self.len, page),
            want <= crate::meta::align_up_spec(self.len, page),
            set_int_range(self.base + self.committed, (self.base + self.committed) + (want - self.committed)) <= self.claim@@.dom(),
    {
        let end = crate::meta::align_up_spec(self.len, page);
        if self.len == 0 {
            // An empty table's end rounds to 0: nothing to commit.
            assert(((0 + page - 1) as usize) & !((page - 1) as usize) == 0usize) by (bit_vector)
                requires page >= 1;
            assert(false);
        }

        let (p, u) = (page, unit);
        if unit > page {
            let k = choose|k: u64| k < 64 && unit == #[trigger] (1usize << k);
            assert(u & sub(u, 1) == 0) by (bit_vector)
                requires k < 64, u == 1usize << k;
        }
        crate::meta::lemma_align_bits(upto, unit, (upto + (unit - 1)) as usize);
        crate::meta::lemma_align_bits(self.len, page, (self.len + (page - 1)) as usize);
        crate::meta::lemma_pow2_divides(unit, page);
        let a = ((upto + unit - 1) as usize) & !((unit - 1) as usize);
        crate::meta::lemma_mod_trans(a as int, unit as int, page as int);
        assert(want % page == 0);
        vstd::arithmetic::div_mod::lemma_add_mod_noop(self.base as int, self.committed as int, page as int);
        vstd::arithmetic::div_mod::lemma_sub_mod_noop(want as int, self.committed as int, page as int);
        vstd::arithmetic::div_mod::lemma_small_mod(0, page as nat);
        assert(set_int_range(self.base + self.committed, self.base + want) <= self.claim@@.dom()) by {
            assert forall|x: int| set_int_range(self.base + self.committed, self.base + want).contains(x) implies self.claim@@.dom().contains(x) by {}
        }
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
    /// (ghost) the frozen permission to the table: nothing writes or frees
    /// it after `Arena::init`.
    Shared(*const SetTable, u8, Tracked<&'static PointsTo<SetTable>>),
}

impl Set {
    /// The set is a view of group `g` of the frozen table at `p`, whose
    /// slab count is `n`.
    pub(crate) open spec fn shared_over(&self, n: nat) -> bool {
        &&& self.store matches Store::Shared(p, g, perm)
        &&& perm@.ptr() == p
        &&& perm@.is_init()
        &&& (g as int) < SET_GROUPS
        &&& perm@.value().nslabs[g as int] == n
    }

    /// What [`Set::ensure`] needs to commit up to `slab` with `page`: for
    /// an own table, its claim to its uncommitted pages
    /// ([`Lazy::claimed`]), the process's page (`Lazy::ensure`), at most
    /// `2^63` bytes, and `slab` and the level-1 word count at most `2^32`
    /// (the commit end stays below `2^62`); for a view of a shared table,
    /// `slab` below its group's slab count.
    pub(crate) open spec fn ensure_ok(&self, page: usize, slab: usize) -> bool {
        match self.store {
            Store::Own(l) => l.claimed() && 1 <= page <= 0x8000_0000_0000_0000 && self.l1_words <= 0x1_0000_0000 && slab <= 0x1_0000_0000
                && page == crate::meta::process_page(),
            Store::Shared(p, g, perm) => perm@.ptr() == p && perm@.is_init() && (g as int) < SET_GROUPS && slab < perm@.value().nslabs[g as int],
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
/// from `base`, inside the address space, on a word boundary.
pub(crate) open spec fn reserved(groups: [Group; SET_GROUPS], nslabs: [usize; SET_GROUPS], c: int) -> bool {
    let (grp, n) = (groups[group_spec(c)], nslabs[group_spec(c)]);
    &&& grp.stride == 3 * grp.count
    &&& grp.count <= NCLASSES
    &&& grp.base + 8 * (65 * (((n + 63) / 64 + 63) / 64)) * grp.stride <= usize::MAX
    &&& grp.base % 8 == 0
}

/// Rows of a chunked view over `n` slab numbers
/// ([`TwoLevelBitmap::chunked_rows`]).
pub(crate) open spec fn rows_of(n: int) -> int {
    65 * (((n + 63) / 64 + 63) / 64)
}

/// The bytes of slot `slot` of a group table of `rows` rows of `stride`
/// words at `base`: the slot's word in every row.
pub(crate) open spec fn slot_bytes(base: int, rows: int, stride: int, slot: int) -> vstd::set::Set<int> {
    vstd::set::Set::range(base, base + 8 * (rows * stride)).filter(|a: int| ((a - base) / 8) % stride == slot)
}

/// Level-1 words of a set over `n` slab numbers
/// ([`TwoLevelBitmap::l1_words`]).
pub(crate) open spec fn l1_of(n: int) -> int {
    ((n + 63) / 64 + 63) / 64
}

/// Slot `slot` of group `g`'s table starts a chunked view that
/// [`TwoLevelBitmap::from_raw_chunked`] can build: its first word is not
/// at address 0, and the words it spans lie inside the allocation of the
/// table (`vspec::ptr::alloc_len` of the table's pointers, which
/// `addr_to_ptr` makes with [`cast_provenance`]). [`SetTable::new`] states
/// it for every slot while it holds the permission to the whole table.
pub(crate) open spec fn slot_alloc_ok(groups: [Group; SET_GROUPS], nslabs: [usize; SET_GROUPS], g: int, slot: int) -> bool {
    let a = groups[g].base + 8 * slot;
    &&& 0 < a
    &&& 8 * crate::bitmap::chunked_span(l1_of(nslabs[g] as int), groups[g].stride as int) <= crate::vspec::ptr::alloc_len(cast_provenance(), a as usize)
}

/// Every slot of a table of `65 * l1` rows of `stride` words at `base`
/// from `lo` on lies in the allocation of `raw`, the permission to the
/// table ([`slot_alloc_ok`], by `vspec::ptr::axiom_raw_valid`).
proof fn lemma_slots_alloc(tracked raw: &ZeroRaw, base: int, l1: int, stride: int, lo: int)
    requires
        0 < base,
        l1 >= 1,
        stride >= 1,
        0 <= lo <= stride,
        base + 8 * (65 * l1 * stride) <= usize::MAX,
        raw.is_range(base, 8 * (65 * l1 * stride)),
        raw.provenance() == cast_provenance(),
    ensures
        forall|slot: int| lo <= slot < stride ==> 8 * crate::bitmap::chunked_span(l1, stride)
            <= #[trigger] crate::vspec::ptr::alloc_len(cast_provenance(), (base + 8 * slot) as usize),
    decreases stride - lo,
{
    broadcast use {vstd::raw_ptr::group_raw_ptr_axioms, vstd::set_lib::range_set_properties};
    if lo >= stride {
        return;
    }
    let a = base + 8 * lo;
    let span = crate::bitmap::chunked_span(l1, stride);
    assert(8 * lo + 8 * span <= 8 * (65 * l1 * stride)) by (nonlinear_arith)
        requires span == (65 * l1 - 1) * stride + 1, 0 <= lo < stride, l1 >= 1;
    let p = vstd::raw_ptr::ptr_mut_from_data::<u8>(vstd::raw_ptr::PtrData::<u8> { addr: a as usize, provenance: cast_provenance(), metadata: () });
    crate::vspec::ptr::axiom_raw_valid(raw.raw(), p, 8 * span);
    lemma_slots_alloc(raw, base, l1, stride, lo + 1);
}

/// The slot of set `kind` of class `c` in its group (`Set::in_table`).
pub(crate) open spec fn slot_spec(pos: [u8; NCLASSES], groups: [Group; SET_GROUPS], c: int, kind: int) -> int {
    kind * groups[group_spec(c)].count + pos[c]
}

/// The bytes of set `kind` of class `c`: its slot's words in its group's
/// table.
pub(crate) open spec fn set_bytes(pos: [u8; NCLASSES], groups: [Group; SET_GROUPS], nslabs: [usize; SET_GROUPS], c: int, kind: int) -> vstd::set::Set<int> {
    let g = group_spec(c);
    slot_bytes(groups[g].base as int, rows_of(nslabs[g] as int), groups[g].stride as int, slot_spec(pos, groups, c, kind))
}

/// A slot's bytes are the words of the chunked view at the slot's first
/// word (`TwoLevelBitmap::from_raw_chunked`'s permission).
proof fn lemma_slot_bytes(base: int, rows: int, stride: int, slot: int)
    requires
        0 <= slot < stride,
        0 <= rows,
    ensures
        slot_bytes(base, rows, stride, slot) == crate::bitmap::chunked_bytes(base + 8 * slot, rows, stride),
{
    broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
    let (s, c) = (slot_bytes(base, rows, stride, slot), crate::bitmap::chunked_bytes(base + 8 * slot, rows, stride));
    assert forall|a: int| #[trigger] s.contains(a) implies c.contains(a) by {
        let k = (a - base) / 8;
        let r = k / stride;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(a - base, 8);
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(k, stride);
        assert(0 <= k < rows * stride);
        assert(0 <= r < rows) by (nonlinear_arith)
            requires k == stride * r + slot, 0 <= k < rows * stride, stride >= 1, 0 <= slot < stride;
        assert(r * stride == stride * r) by (nonlinear_arith);
        assert(crate::bitmap::row_addr(base + 8 * slot, r, stride) <= a < crate::bitmap::row_addr(base + 8 * slot, r, stride) + 8);
        assert(a < base + 8 * slot + 8 * (rows * stride));
    }
    assert forall|a: int| #[trigger] c.contains(a) implies s.contains(a) by {
        let r = choose|r: int| 0 <= r < rows && #[trigger] crate::bitmap::row_addr(base + 8 * slot, r, stride) <= a < crate::bitmap::row_addr(base + 8 * slot, r, stride) + 8;
        let k = r * stride + slot;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(a - base, 8, k, a - base - 8 * k);
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(k, stride, r, slot);
        assert((r + 1) * stride <= rows * stride) by (nonlinear_arith)
            requires r + 1 <= rows, stride >= 0;
        assert((r + 1) * stride == r * stride + stride) by (nonlinear_arith);
    }
    assert(s =~= c);
}

/// Different slots of a group (set `k1` at position `p1`, `k2` at `p2`,
/// positions below the group's count `n`) are different words.
proof fn lemma_slots_apart(p1: int, p2: int, k1: int, k2: int, n: int)
    requires
        0 <= p1 < n,
        0 <= p2 < n,
        0 <= k1 < SETS_PER_CLASS,
        0 <= k2 < SETS_PER_CLASS,
        k1 != k2 || p1 != p2,
    ensures
        k1 * n + p1 != k2 * n + p2,
        0 <= k1 * n + p1 < 3 * n,
{
    assert(0 <= k1 * n + p1 < 3 * n) by (nonlinear_arith)
        requires 0 <= p1 < n, 0 <= k1 < 3;
    if k1 < k2 {
        assert(k1 * n + p1 < k2 * n + p2) by (nonlinear_arith)
            requires k1 < k2, 0 <= p1 < n, 0 <= p2;
    } else if k2 < k1 {
        assert(k2 * n + p2 < k1 * n + p1) by (nonlinear_arith)
            requires k2 < k1, 0 <= p2 < n, 0 <= p1;
    }
}

/// Classes' positions in one group are distinct (`SetTable::new`'s first
/// pass hands them out in turn).
pub(crate) open spec fn pos_apart(pos: [u8; NCLASSES], c: int) -> bool {
    forall|d1: int, d2: int|
        0 <= d1 < c && 0 <= d2 < c && d1 != d2 && #[trigger] group_spec(d1) == #[trigger] group_spec(d2) ==> pos[d1] != pos[d2]
}

/// The permission to the bytes of group `g`'s table, as reserved, and
/// the table's slots ([`slot_alloc_ok`]).
spec fn group_raw(raws: Map<int, ZeroRaw>, groups: [Group; SET_GROUPS], nslabs: [usize; SET_GROUPS], g: int) -> bool {
    &&& raws.contains_key(g)
    &&& raws[g].is_range(groups[g].base as int, 8 * (rows_of(nslabs[g] as int) * groups[g].stride))
    &&& raws[g].provenance() == cast_provenance()
    &&& forall|slot: int| 0 <= slot < groups[g].stride ==> #[trigger] slot_alloc_ok(groups, nslabs, g, slot)
}

/// Set `k` of class `d` can be split off `raws`, its group's remaining
/// bytes.
spec fn splittable(raws: Map<int, ZeroRaw>, pos: [u8; NCLASSES], groups: [Group; SET_GROUPS], nslabs: [usize; SET_GROUPS], d: int, k: int) -> bool {
    &&& raws.contains_key(group_spec(d))
    &&& set_bytes(pos, groups, nslabs, d, k).subset_of(raws[group_spec(d)].dom())
    &&& raws[group_spec(d)].provenance() == cast_provenance()
}

/// The sets of classes `c..NCLASSES`, split from their groups'
/// permissions `raws`: each class's three slots, keyed `(class, kind)`.
/// Distinct classes of one group have distinct positions, so no two
/// slots meet.
proof fn split_sets(
    tracked raws: &mut Map<int, ZeroRaw>,
    pos: [u8; NCLASSES],
    groups: [Group; SET_GROUPS],
    nslabs: [usize; SET_GROUPS],
    c: int,
) -> (tracked r: Map<(int, int), ZeroRaw>)
    requires
        0 <= c <= NCLASSES,
        pos_apart(pos, NCLASSES as int),
        forall|d: int| 0 <= d < NCLASSES ==> 0 <= #[trigger] group_spec(d) < SET_GROUPS && (pos[d] as int) < groups[group_spec(d)].count,
        forall|d: int, k: int| c <= d < NCLASSES && 0 <= k < SETS_PER_CLASS ==> #[trigger] splittable(*old(raws), pos, groups, nslabs, d, k),
    ensures
        forall|d: int, k: int| #![trigger r.contains_key((d, k))] #![trigger r[(d, k)]] c <= d < NCLASSES && 0 <= k < SETS_PER_CLASS ==> {
            &&& r.contains_key((d, k))
            &&& r[(d, k)].dom() == set_bytes(pos, groups, nslabs, d, k)
            &&& r[(d, k)].provenance() == cast_provenance()
        },
    decreases NCLASSES - c,
{
    broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
    if c >= NCLASSES {
        return Map::tracked_empty();
    }
    let g = group_spec(c);
    let n = groups[g].count as int;
    assert(0 <= g < SET_GROUPS && (pos[c] as int) < n);
    let (b0, b1, b2) = (set_bytes(pos, groups, nslabs, c, 0), set_bytes(pos, groups, nslabs, c, 1), set_bytes(pos, groups, nslabs, c, 2));
    let ghost before = *raws;
    assert(splittable(before, pos, groups, nslabs, c, 0) && splittable(before, pos, groups, nslabs, c, 1) && splittable(before, pos, groups, nslabs, c, 2));
    let tracked mut raw = raws.tracked_remove(g);
    let ghost raw0 = raw;
    // Set `kind` of class `c`, split off the group's remaining bytes: the
    // three slots are different words.
    lemma_slots_apart(pos[c] as int, pos[c] as int, 0, 1, n);
    lemma_slots_apart(pos[c] as int, pos[c] as int, 0, 2, n);
    lemma_slots_apart(pos[c] as int, pos[c] as int, 1, 2, n);
    let tracked (s0, rest) = raw.split(b0);
    raw = rest;
    assert(b1.subset_of(raw.dom())) by {
        assert forall|a: int| #[trigger] b1.contains(a) implies raw.dom().contains(a) by {
            assert(!b0.contains(a));
        }
    }
    let tracked (s1, rest) = raw.split(b1);
    raw = rest;
    assert(b2.subset_of(raw.dom())) by {
        assert forall|a: int| #[trigger] b2.contains(a) implies raw.dom().contains(a) by {
            assert(!b0.contains(a) && !b1.contains(a));
        }
    }
    let tracked (s2, rest) = raw.split(b2);
    raw = rest;
    assert(raw.dom() == raw0.dom().difference(b0).difference(b1).difference(b2));
    raws.tracked_insert(g, raw);
    assert forall|d: int, k: int| c + 1 <= d < NCLASSES && 0 <= k < SETS_PER_CLASS implies #[trigger] splittable(*raws, pos, groups, nslabs, d, k) by {
        let bd = set_bytes(pos, groups, nslabs, d, k);
        assert(splittable(before, pos, groups, nslabs, d, k));
        if group_spec(d) == g {
            assert(pos[d] != pos[c]);
            assert(0 <= group_spec(d) < SET_GROUPS && (pos[d] as int) < n);
            lemma_slots_apart(pos[d] as int, pos[c] as int, k, 0, n);
            lemma_slots_apart(pos[d] as int, pos[c] as int, k, 1, n);
            lemma_slots_apart(pos[d] as int, pos[c] as int, k, 2, n);
            assert forall|a: int| #[trigger] bd.contains(a) implies raws[g].dom().contains(a) by {
                assert(raw0.dom().contains(a));
                assert(!b0.contains(a) && !b1.contains(a) && !b2.contains(a));
            }
        } else {
            assert(raws[group_spec(d)] == before[group_spec(d)]);
        }
    }
    let tracked mut r = split_sets(raws, pos, groups, nslabs, c + 1);
    let ghost r0 = r;
    r.tracked_insert((c, 0), s0);
    r.tracked_insert((c, 1), s1);
    r.tracked_insert((c, 2), s2);
    assert forall|d: int, k: int| #![trigger r.contains_key((d, k))] #![trigger r[(d, k)]] c <= d < NCLASSES && 0 <= k < SETS_PER_CLASS implies {
        &&& r.contains_key((d, k))
        &&& r[(d, k)].dom() == set_bytes(pos, groups, nslabs, d, k)
        &&& r[(d, k)].provenance() == cast_provenance()
    } by {
        if d != c {
            assert(r0.contains_key((d, k)));
            assert(r[(d, k)] == r0[(d, k)]);
        } else if k == 0 {
            assert(r[(d, k)] == s0);
        } else if k == 1 {
            assert(r[(d, k)] == s1);
        } else {
            assert(r[(d, k)] == s2);
        }
    }
    r
}

impl SetTable {
    /// The commit geometry [`SetTable::ensure`] relies on: the process's
    /// page, which [`Lazy::ensure`] takes, groups of at most three sets per class and
    /// at most `2^26` slabs, and a commit lock ranked as a slab-set lock.
    /// `new` proves it, and nothing writes the fields afterwards.
    #[verifier::type_invariant]
    spec fn inv(self) -> bool {
        &&& 1 <= self.page <= 0x8000_0000_0000_0000
        &&& self.page == crate::meta::process_page()
        &&& forall|g: int| 0 <= g < SET_GROUPS ==> #[trigger] self.groups[g].stride <= 3 * NCLASSES
        &&& forall|g: int| 0 <= g < SET_GROUPS ==> #[trigger] self.nslabs[g] <= 0x400_0000
        &&& rank_sets(0) <= self.tables.rank() < rank_sets(0x1_0000)
    }

    /// The commit lock's rank: arena `i`'s slab-set lock,
    /// `rank_sets(i)`.
    pub closed spec fn rank(&self) -> int {
        self.tables.rank()
    }

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

    /// `m` holds set `kind` of class `c`: the permission to its slot's
    /// bytes ([`set_bytes`]), which read as zero and which
    /// [`Set::in_table`] takes, and the slot starts a view inside its
    /// group's table ([`slot_alloc_ok`]).
    pub(crate) open spec fn holds_set(&self, m: Map<(int, int), ZeroRaw>, c: int, kind: int) -> bool {
        &&& m.contains_key((c, kind))
        &&& m[(c, kind)].dom() == set_bytes(self.pos, self.groups, self.nslabs, c, kind)
        &&& m[(c, kind)].provenance() == cast_provenance()
        &&& slot_alloc_ok(self.groups, self.nslabs, group_spec(c), slot_spec(self.pos, self.groups, c, kind))
    }
}

/// The `len` bytes of a table at `base` whose permission `raw` holds lie
/// in the allocation of an address-cast pointer to them (`Set::new`,
/// `vspec::ptr::axiom_raw_valid`).
proof fn lemma_table_alloc(tracked raw: &ZeroRaw, base: int, len: int)
    requires
        0 < len,
        0 <= base,
        base + len <= usize::MAX,
        raw.is_range(base, len),
        raw.provenance() == cast_provenance(),
    ensures
        crate::vspec::ptr::alloc_len(cast_provenance(), base as usize) >= len,
{
    broadcast use vstd::set_lib::range_set_properties;
    let p = vstd::raw_ptr::ptr_mut_from_data::<u8>(vstd::raw_ptr::PtrData::<u8> { addr: base as usize, provenance: cast_provenance(), metadata: () });
    crate::vspec::ptr::axiom_raw_valid(raw.raw(), p, len);
}

impl Set {
    /// A set over `0..nslabs` on its own table, which holds its claim to
    /// its uncommitted pages ([`Lazy::claimed`]).
    pub(crate) fn new(meta: &Meta, page: usize, nslabs: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<Set>)
        requires
            crate::sys::may_syscall(),
            crate::meta::is_pow2(page),
            page >= 8,
            page <= 0x8000_0000_0000_0000,
            0 < nslabs <= 0x1_0000_0000,
            old(ord).allows(rank_meta()),
            meta.is_process(),
            page == crate::meta::process_page(),
        ensures
            r matches Some(s) ==> s.map.wf() && s.map.spec_nbits() == nslabs && s.map@ == vstd::set::Set::<nat>::empty(),
            r matches Some(s) ==> s.store is Own && s.l1_words <= 0x1_0000_0000,
            r matches Some(s) ==> (s.store matches Store::Own(l) && l.claimed()),
            *final(ord) == *old(ord),
    {
        let l0_words = nslabs.div_ceil(64);
        let l1_words = l0_words.div_ceil(64);
        let tracked mut words = ZeroRaw::empty(cast_provenance());
        let mut table = Lazy::reserve(meta, 8 * TwoLevelBitmap::words_needed(nslabs), page, Tracked(&mut words), Tracked(ord))?;
        proof {
            // The table starts on a page, a power of two of at least 8, so
            // it is aligned for `u64` (`from_raw`'s requires).
            let (b, pg) = (table.base, page);
            crate::meta::lemma_pow2_bits(pg);
            assert(b % pg == 0);
            assert(b & sub(pg, 1) == 0) by (bit_vector)
                requires
                    pg & sub(pg, 1) == 0,
                    pg > 0,
                    b % pg == 0,
            ;
            assert(b % 8 == 0) by (bit_vector)
                requires
                    b & sub(pg, 1) == 0,
                    pg & sub(pg, 1) == 0,
                    pg >= 8,
            ;
            // The whole table lies in its allocation (`from_raw`'s
            // requires), the permission's.
            broadcast use vstd::raw_ptr::group_raw_ptr_axioms;
            let len = 8 * ((nslabs + 63) / 64 + ((nslabs + 63) / 64 + 63) / 64);
            assert((len + 7) / 8 * 8 == len);
            lemma_table_alloc(&words, b as int, len);
        }
        // Level 1 is small and touched by every operation: commit it now.
        table.ensure(meta, page, 8 * l1_words, Tracked(ord)).ok()?;
        // SAFETY: the words come from a fresh, zero, never-freed reservation.
        let map = unsafe { TwoLevelBitmap::from_raw(nslabs, crate::vspec::addr::addr_to_ptr(table.base) as *mut u64, Tracked(words)) };
        proof {
            map.lemma_fresh(nslabs);
        }
        Some(Set { map, store: Store::Own(table), l1_words })
    }

    /// Set `kind` (0..`SETS_PER_CLASS`) of `class` in the table at `tp`,
    /// over the class's slab numbers: a well-formed empty set over the
    /// group's slab count, through `words`, the permission to its slot's
    /// bytes, which [`SetTable::new`] handed out once
    /// ([`SetTable::holds_set`]). `tperm` is the frozen permission to the
    /// table, which outlives the set and which the set keeps.
    pub(crate) fn in_table(
        tp: *const SetTable,
        Tracked(tperm): Tracked<&'static PointsTo<SetTable>>,
        class: usize,
        kind: usize,
        Tracked(words): Tracked<ZeroRaw>,
    ) -> (r: Set)
        requires
            class < NCLASSES,
            kind < SETS_PER_CLASS,
            tperm.ptr() == tp,
            tperm.is_init(),
            tperm.value().class_ok(process_regions(), class as int),
            words.dom() == set_bytes(tperm.value().pos, tperm.value().groups, tperm.value().nslabs, class as int, kind as int),
            words.provenance() == cast_provenance(),
            slot_alloc_ok(
                tperm.value().groups,
                tperm.value().nslabs,
                group_spec(class as int),
                slot_spec(tperm.value().pos, tperm.value().groups, class as int, kind as int),
            ),
        ensures
            r.map.wf(),
            r.map.spec_nbits() == tperm.value().nslabs[group_spec(class as int)],
            r.map@ == vstd::set::Set::<nat>::empty(),
            r.shared_over(r.map.spec_nbits()),
    {
        let table = vstd::raw_ptr::ptr_ref(tp, Tracked(tperm));
        debug_assert!(kind < SETS_PER_CLASS);
        let g = SetTable::group_of(class);
        let group = &table.groups[g];
        proof {
            let (n, s, c) = (table.nslabs[g as int] as int, group.stride as int, group.count as int);
            let l1 = ((n + 63) / 64 + 63) / 64;
            assert(1 <= l1 <= 0x4001);
            assert(1 <= c && s == 3 * c);
            assert(kind * c + table.pos[class as int] < 3 * c) by (nonlinear_arith)
                requires kind < 3, table.pos[class as int] < c;
            assert(s <= 8 * (65 * l1) * s) by (nonlinear_arith) requires l1 >= 1, s >= 0;
            // The view spans fewer than `isize::MAX` bytes
            // (`from_raw_chunked`'s requires).
            assert(c <= NCLASSES);
            assert(65 * l1 * s * 8 <= isize::MAX) by (nonlinear_arith)
                requires 1 <= l1 <= 0x4001, s == 3 * c, 0 <= c <= NCLASSES, NCLASSES <= 64;
        }
        let slot = kind * group.count + table.pos[class] as usize;
        let nslabs = table.nslabs[g];
        proof {
            let (n, s) = (nslabs as int, group.stride as int);
            let l1 = ((n + 63) / 64 + 63) / 64;
            assert(65 * l1 * s * 8 == 8 * (65 * l1) * s) by (nonlinear_arith);
            assert(rows_of(n) * s == 65 * l1 * s);
            lemma_slot_bytes(group.base as int, rows_of(n), s, slot as int);
            // The view's first word and the words it spans
            // (`from_raw_chunked`'s requires).
            broadcast use vstd::raw_ptr::group_raw_ptr_axioms;
            assert(8 * slot + 8 * crate::bitmap::chunked_span(l1, s) <= 8 * (65 * l1) * s) by (nonlinear_arith)
                requires 0 <= slot < s, l1 >= 1, crate::bitmap::chunked_span(l1, s) == (65 * l1 - 1) * s + 1;
        }
        // SAFETY: the slot's words are `base + 8 * (slot + r * stride)`,
        // disjoint from every other slot's, zero, never freed; the rows a
        // slab's bit needs are committed by `ensure` before the class
        // touches it (`take_group`).
        let map = unsafe {
            TwoLevelBitmap::from_raw_chunked(
                nslabs,
                crate::vspec::addr::addr_to_ptr(group.base + 8 * slot) as *mut u64,
                group.stride,
                Tracked(words),
            )
        };
        proof {
            map.lemma_fresh(nslabs);
        }
        Set { map, store: Store::Shared(tp, g as u8, Tracked(tperm)), l1_words: TwoLevelBitmap::l1_words(nslabs) }
    }

    /// Commit the level-0 words that hold every bit up to and including
    /// `slab`, which the set's table admits with `page`
    /// ([`Set::ensure_ok`]). Only an own store changes; the bitmap view
    /// is the same.
    ///
    /// Lock order: an own store's commit takes the metadata lock; a shared
    /// store's takes the slab-set lock of the table's arena, then the
    /// metadata lock (`SetTable::ensure`). Every slab-set lock ranks at
    /// least `rank_sets(0)`.
    // `perm` is read only by the ghost argument of `ptr_ref`.
    #[allow(unused_variables)]
    pub(crate) fn ensure(&mut self, meta: &Meta, page: usize, slab: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Result<(), Errno>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_meta()),
            old(self).store is Shared ==> old(ord).allows(rank_sets(0)),
            old(self).ensure_ok(page, slab),
            meta.is_process(),
        ensures
            final(self).map == old(self).map,
            final(self).l1_words == old(self).l1_words,
            (final(self).store is Own) == (old(self).store is Own),
            final(self).store matches Store::Own(l) ==> l.claimed(),
            old(self).store is Shared ==> final(self).store == old(self).store,
            *final(ord) == *old(ord),
    {
        match &mut self.store {
            Store::Own(table) => table.ensure(meta, page, 8 * (self.l1_words + slab / 64 + 1), Tracked(ord)),
            // Through the frozen permission to the table, which outlives
            // the set.
            Store::Shared(table, g, perm) => vstd::raw_ptr::ptr_ref(*table, Tracked(*perm.borrow())).ensure(meta, *g as usize, slab, Tracked(ord)),
        }
    }
}

/// Assumption (`lock.rs`): the commit watermarks behind
/// [`SetTable::tables`] keep each table's claim to its uncommitted pages
/// ([`Lazy::claimed`]), which `Lazy::ensure` needs and keeps; the bounds
/// `SetTable::ensure` needs are `Lazy`'s type invariant and `SetTable`'s.
/// Their one writer, `SetTable::ensure`, is verified, and keeps it by
/// `Lazy::ensure`'s postcondition (Verus does not check it as the guard
/// drops); `SetTable::new` proves it of the tables it reserves; the
/// test-only `committed_ranges` only reads.
pub(crate) broadcast axiom fn axiom_set_tables_lock_inv(v: [Lazy; SET_GROUPS])
    ensures
        #[trigger] crate::lock::lock_inv(v) == forall|g: int| 0 <= g < SET_GROUPS ==> #[trigger] v[g].claimed(),
;

impl SetTable {
    /// Reserve the tables (address space only; rows are committed by
    /// `ensure`). `None` if the metadata region cannot hold them. On
    /// success `sets` holds the permission to every class's three slots
    /// ([`SetTable::holds_set`]), each for its one [`Set::in_table`], and
    /// the commit lock is arena `arena`'s slab-set lock.
    // Indexed loop, spelled out for Verus like the rest of this function.
    #[allow(clippy::needless_range_loop)]
    pub(crate) fn new(
        meta: &Meta,
        regions: &Regions,
        Ghost(arena): Ghost<u16>,
        Tracked(sets): Tracked<&mut Map<(int, int), ZeroRaw>>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: Option<SetTable>)
        requires
            crate::sys::may_syscall(),
            regions.wf(),
            // A page of at least 4 KiB: the group tables start on a word.
            regions.separated(),
            *regions == process_regions(),
            old(ord).allows(rank_meta()),
            meta.is_process(),
        ensures
            r matches Some(t) ==> t.over(*regions) && t.rank() == rank_sets(arena as int),
            r matches Some(t) ==> forall|c: int, k: int| 0 <= c < NCLASSES && 0 <= k < SETS_PER_CLASS ==> #[trigger] t.holds_set(*final(sets), c, k),
            *final(ord) == *old(ord),
    {
        let page = regions.page_size();
        proof {
            crate::meta::lemma_is_pow2_shl(regions.lg_page as usize);
            let lg = regions.lg_page;
            assert((1usize << lg) == (1usize << (lg as usize))) by (bit_vector) requires lg < 64;
            assert(page % 8 == 0 && page >= 8) by (bit_vector) requires page == 1usize << lg, 12 <= lg < 64;
        }
        let mut groups = [Group { base: 0, stride: 0, count: 0 }; SET_GROUPS];
        let mut nslabs = [0usize; SET_GROUPS];
        let mut pos = [0u8; NCLASSES];
        for c in 0..NCLASSES
            invariant
                regions.wf(),
                forall|g: int| 0 <= g < SET_GROUPS ==> #[trigger] groups[g].count <= c,
                forall|g: int| 0 <= g < SET_GROUPS ==> #[trigger] nslabs[g] <= 0x400_0000,
                forall|g: int| 0 <= g < SET_GROUPS ==> #[trigger] groups[g].stride <= 3 * NCLASSES,
                forall|g: int| 0 <= g < SET_GROUPS && groups[g].count != 0 ==> 1 <= #[trigger] nslabs[g],
                forall|d: int| 0 <= d < c ==> #[trigger] placed(pos, groups, nslabs, *regions, d),
                pos_apart(pos, c as int),
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
                // The new position is the group's count so far, above every
                // earlier class's of the group.
                assert forall|d1: int, d2: int|
                    0 <= d1 < c + 1 && 0 <= d2 < c + 1 && d1 != d2 && #[trigger] group_spec(d1) == #[trigger] group_spec(d2) implies pos[d1] != pos[d2] by {
                    if d1 < c && d2 < c {
                        assert(pos0[d1] != pos0[d2]);
                    } else if d1 < c {
                        assert(placed(pos0, groups0, nslabs0, *regions, d1));
                    } else {
                        assert(placed(pos0, groups0, nslabs0, *regions, d2));
                    }
                }
            }
        }
        proof {
            assert forall|d: int| 0 <= d < NCLASSES && group_spec(d) < 0 implies #[trigger] reserved(groups, nslabs, d) by {
                assert(placed(pos, groups, nslabs, *regions, d));
            }
        }
        // Ghost: each reserved group's permission to its table's bytes.
        let tracked mut raws: Map<int, ZeroRaw> = Map::tracked_empty();
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
        for g in 0..SET_GROUPS
            invariant
                crate::sys::may_syscall(),
                forall|h: int| 0 <= h < SET_GROUPS ==> #[trigger] groups[h].count <= NCLASSES,
                forall|h: int| 0 <= h < SET_GROUPS ==> #[trigger] nslabs[h] <= 0x400_0000,
                forall|h: int| 0 <= h < SET_GROUPS ==> #[trigger] groups[h].stride <= 3 * NCLASSES,
                forall|h: int| 0 <= h < SET_GROUPS && groups[h].count != 0 ==> 1 <= #[trigger] nslabs[h],
                forall|d: int| 0 <= d < NCLASSES ==> #[trigger] placed(pos, groups, nslabs, *regions, d),
                forall|d: int| 0 <= d < NCLASSES && group_spec(d) < g ==> #[trigger] reserved(groups, nslabs, d),
                forall|h: int| 0 <= h < g && groups[h].count != 0 ==> #[trigger] group_raw(raws, groups, nslabs, h),
                pos_apart(pos, NCLASSES as int),
                crate::meta::is_pow2(page),
                page % 8 == 0 && page >= 8,
                ord.allows(rank_meta()),
                *ord == *old(ord),
                meta.is_process(),
                forall|h: int| 0 <= h < SET_GROUPS ==> #[trigger] tables[h].claimed(),
        {
            if groups[g].count != 0 {
                let ghost groups0 = groups;
                let ghost raws0 = raws;
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
                let tracked mut raw = ZeroRaw::empty(cast_provenance());
                let t = Lazy::reserve(meta, 8 * rows * grp.stride, page, Tracked(&mut raw), Tracked(ord))?;
                grp.base = t.base;
                tables[g] = t;
                groups[g] = grp;
                proof {
                    assert(8 * rows * grp.stride == 8 * (65 * (((n + 63) / 64 + 63) / 64)) * grp.stride) by (nonlinear_arith)
                        requires rows == 65 * (((n + 63) / 64 + 63) / 64);
                    let len = 8 * rows * grp.stride;
                    assert((len + 7) / 8 * 8 == 8 * (rows_of(n) * grp.stride)) by (nonlinear_arith)
                        requires len == 8 * rows * grp.stride, rows == rows_of(n);
                    assert(8 * (rows_of(n) * grp.stride) == 8 * (65 * l1_of(n) * grp.stride)) by (nonlinear_arith)
                        requires rows_of(n) == 65 * l1_of(n);
                    lemma_mod_8(grp.base as int, page as int);
                    lemma_slots_alloc(&raw, grp.base as int, l1_of(n), grp.stride as int, 0);
                    raws.tracked_insert(g as int, raw);
                    assert forall|d: int| 0 <= d < NCLASSES implies #[trigger] placed(pos, groups, nslabs, *regions, d) by {
                        assert(placed(pos, groups0, nslabs, *regions, d));
                    }
                    assert forall|d: int| 0 <= d < NCLASSES && group_spec(d) < g + 1 implies #[trigger] reserved(groups, nslabs, d) by {
                        assert(placed(pos, groups0, nslabs, *regions, d));
                        if group_spec(d) < g {
                            assert(reserved(groups0, nslabs, d));
                        }
                    }
                    assert forall|h: int| 0 <= h < g + 1 && groups[h].count != 0 implies #[trigger] group_raw(raws, groups, nslabs, h) by {
                        if h < g {
                            assert(group_raw(raws0, groups0, nslabs, h));
                            assert forall|slot: int| 0 <= slot < groups[h].stride implies #[trigger] slot_alloc_ok(groups, nslabs, h, slot) by {
                                assert(slot_alloc_ok(groups0, nslabs, h, slot));
                            }
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
            assert forall|c: int| #![trigger placed(pos, groups, nslabs, *regions, c)] #![trigger reserved(groups, nslabs, c)] 0 <= c < NCLASSES implies placed(pos, groups, nslabs, *regions, c)
                && reserved(groups, nslabs, c) by {
                assert(placed(pos, groups, nslabs, *regions, c));
            }
            broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
            // Every class's slots lie in its group's reserved bytes.
            assert forall|d: int, k: int| 0 <= d < NCLASSES && 0 <= k < SETS_PER_CLASS implies #[trigger] splittable(raws, pos, groups, nslabs, d, k) by {
                assert(placed(pos, groups, nslabs, *regions, d));
                assert(group_raw(raws, groups, nslabs, group_spec(d)));
            }
            assert forall|d: int| 0 <= d < NCLASSES implies 0 <= #[trigger] group_spec(d) < SET_GROUPS
                && (pos[d] as int) < groups[group_spec(d)].count by {
                assert(placed(pos, groups, nslabs, *regions, d));
            }
            // Every class's slots start a view inside its group's table.
            assert forall|d: int, k: int| 0 <= d < NCLASSES && 0 <= k < SETS_PER_CLASS implies #[trigger] slot_alloc_ok(groups, nslabs, group_spec(d), slot_spec(pos, groups, d, k)) by {
                assert(placed(pos, groups, nslabs, *regions, d));
                assert(reserved(groups, nslabs, d));
                assert(group_raw(raws, groups, nslabs, group_spec(d)));
                let (p, n) = (pos[d] as int, groups[group_spec(d)].count as int);
                assert(0 <= k * n + p < 3 * n) by (nonlinear_arith)
                    requires 0 <= p < n, 0 <= k < 3;
            }
            let tracked m = split_sets(&mut raws, pos, groups, nslabs, 0);
            *sets = m;
            crate::meta::lemma_pow2_bits(page);
            broadcast use axiom_set_tables_lock_inv;
        }
        Some(SetTable {
            groups,
            nslabs,
            pos,
            tables: Lock::new(tables, Ghost(rank_sets(arena as int))),
            page,
        })
    }
}

/// `b` on a boundary of `p`, a multiple of 8, is on a word boundary.
proof fn lemma_mod_8(b: int, p: int)
    requires
        p > 0,
        b % p == 0,
        p % 8 == 0,
    ensures
        b % 8 == 0,
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(b, p);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(p, 8);
    let (q, m) = (b / p, p / 8);
    assert(b == (m * q) * 8) by (nonlinear_arith)
        requires b == p * q, p == 8 * m;
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(m * q, 8);
}

impl SetTable {
    /// Commit the rows of group `g` up to the level-0 word of `slab` (and
    /// so its chunk's level-1 word and every row before).
    ///
    /// Lock order: takes this table's commit lock, then the metadata
    /// lock.
    fn ensure(&self, meta: &Meta, g: usize, slab: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Result<(), Errno>)
        requires
            crate::sys::may_syscall(),
            g < SET_GROUPS,
            slab < self.nslabs[g as int],
            old(ord).allows(rank_sets(0)),
            meta.is_process(),
        ensures
            *final(ord) == *old(ord),
    {
        proof {
            broadcast use axiom_set_tables_lock_inv;
            use_type_invariant(self);
            let w = slab / 64;
            assert(w <= 0x10_0000);
            assert(65 * (w / 64) + 1 + w % 64 <= 65 * 0x4000 + 64);
            let (r1, s) = (65 * (w / 64) + 1 + w % 64 + 1, self.groups[g as int].stride as int);
            assert(8 * r1 * s <= 8 * (65 * 0x4000 + 65) * (3 * NCLASSES)) by (nonlinear_arith)
                requires 0 <= r1 <= 65 * 0x4000 + 65, 0 <= s <= 3 * NCLASSES;
        }
        let row = TwoLevelBitmap::chunked_l0_row(slab / 64);
        let tracked mut under = LockOrder::spare();
        self.tables.lock(Tracked(ord), Tracked(&mut under))[g].ensure(meta, self.page, 8 * (row + 1) * self.groups[g].stride, Tracked(&mut under))
    }

    /// The commit lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> &RawLock {
        self.tables.raw()
    }
}

} // verus!

impl SetTable {
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
}
