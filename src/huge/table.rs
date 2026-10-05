//! The huge tier's descriptor table: page number to [`HugeDesc`] in an
//! open-addressing hash table with linear probing and backward-shift
//! deletion, grown by doubling. Storage comes from the metadata region;
//! every access is under the huge lock.
//!
//! Verus checks the table (`verus/README.md`) against [`Table::wf`]: every
//! slot is read and written through its permission, the count is the
//! number of occupied slots and stays below the capacity (so a probe always
//! ends), keys are unique, and every entry is reachable from its ideal
//! slot without crossing an empty one. `get`, `insert`, `remove` and
//! `ensure` are proved to act on the map of keys to descriptors that the
//! slots hold ([`Table::lookup`]).

// The expansion of Verus' `global` in ordinary builds is a braced block.
#![cfg_attr(not(verus_keep_ghost), allow(unused_braces))]

use core::alloc::Layout;
use core::ptr;

use crate::lock::LockOrder;
#[cfg(verus_only)]
use crate::lock::rank_meta;
use crate::meta::{self, Meta};
#[cfg(verus_only)]
use crate::vspec::layout::{layout_align, layout_size};
#[cfg(verus_only)]
use crate::vspec::zero::Zeroed;
#[cfg(verus_only)]
use vstd::layout::size_of;
use vstd::prelude::*;
#[cfg(verus_only)]
use vstd::raw_ptr::PointsToRaw;
use vstd::raw_ptr::{PointsTo, ptr_mut_write, ptr_ref};

use super::HugeDesc;

verus! {

/// `log2` of the initial descriptor table capacity.
const LG_INITIAL_TABLE: u32 = 10;

/// One hash table slot: the key it was inserted under and the descriptor.
/// A null descriptor means empty.
#[derive(Clone, Copy)]
struct Entry {
    key: usize,
    desc: *mut HugeDesc,
}

// Verus checks the size `Table::layout` assumes, and the alignment
// `entry_slots` types the storage at.
global layout Entry is size == 16, align == 8;

/// Open-addressing hash table with linear probing and backward-shift
/// deletion. Load is kept at or below one half by [`Table::ensure`].
pub(super) struct Table {
    entries: *mut Entry,
    lg_cap: u32,
    pub(super) count: usize,
    /// Storage came from `Meta::alloc` (and can be freed) rather than a
    /// reservation.
    binned: bool,
    /// Permission for each slot of the storage (ghost).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    slots: Tracked<Map<int, PointsTo<Entry>>>,
}

/// The slot holds a descriptor.
spec fn occupied(e: Entry) -> bool {
    e.desc@.addr != 0
}

/// Number of occupied slots.
spec fn occupancy(v: Seq<Entry>) -> nat
    decreases v.len(),
{
    if v.len() == 0 {
        0
    } else {
        occupancy(v.drop_last()) + if occupied(v.last()) {
            1nat
        } else {
            0nat
        }
    }
}

/// Slots in a table of `2^lg` of them.
spec fn cap_of(lg: u32) -> int {
    (1usize << lg) as int
}

/// The slot `d` steps after `h` in a table of `cap` slots, for `d < cap`.
spec fn at(h: int, d: int, cap: int) -> int {
    if h + d < cap {
        h + d
    } else {
        h + d - cap
    }
}

/// Steps from slot `a` forward to slot `b`, wrapping.
spec fn dist(a: int, b: int, cap: int) -> int {
    if a <= b {
        b - a
    } else {
        b + cap - a
    }
}

/// The slot the probe for `key` starts at ([`Table::hash`]).
spec fn ideal(key: usize, lg: u32) -> int {
    ((key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> sub(64, lg)) as int
}

/// No two occupied slots hold the same key.
spec fn keys_unique(v: Seq<Entry>) -> bool {
    forall|a: int, b: int|
        0 <= a < v.len() && 0 <= b < v.len() && occupied(#[trigger] v[a]) && occupied(#[trigger] v[b])
            && v[a].key == v[b].key ==> a == b
}

/// Every slot on the probe path from the ideal slot of the entry at `s` to
/// `s` is occupied.
spec fn reachable(v: Seq<Entry>, lg: u32, s: int) -> bool {
    forall|d: int|
        0 <= d < dist(ideal(v[s].key, lg), s, v.len() as int) ==> occupied(
            #[trigger] v[at(ideal(v[s].key, lg), d, v.len() as int)],
        )
}

/// Every entry is [`reachable`]: a probe for its key finds it before an
/// empty slot.
spec fn probes_reach(v: Seq<Entry>, lg: u32) -> bool {
    forall|s: int| 0 <= s < v.len() && occupied(#[trigger] v[s]) ==> reachable(v, lg, s)
}

/// Some occupied slot holds `key`.
spec fn holds(v: Seq<Entry>, key: usize) -> bool {
    exists|j: int| 0 <= j < v.len() && occupied(#[trigger] v[j]) && v[j].key == key
}

/// The descriptor stored under `key`, if any.
spec fn lookup(v: Seq<Entry>, key: usize) -> Option<*mut HugeDesc> {
    if holds(v, key) {
        Some(v[choose|j: int| 0 <= j < v.len() && occupied(#[trigger] v[j]) && v[j].key == key].desc)
    } else {
        None
    }
}

/// The `2^lg` entries from `p` on are metadata
/// ([`Regions::in_meta`](crate::layout::Regions::in_meta)).
spec fn entries_in_meta(p: *mut Entry, lg: u32) -> bool {
    crate::layout::process_regions().in_meta(p@.addr as int, cap_of(lg) * 16)
}

/// The permissions `slots` cover `2^lg` entries from `p` on.
spec fn covers(p: *mut Entry, lg: u32, slots: Map<int, PointsTo<Entry>>) -> bool {
    forall|i: int| 0 <= i < cap_of(lg) ==> slot_at(p, slots, i)
}

/// `slots` holds an initialized permission for entry `i` from `p`, which
/// lies inside the storage allocation (so `p.add(i)` is in bounds).
spec fn slot_at(p: *mut Entry, slots: Map<int, PointsTo<Entry>>, i: int) -> bool {
    &&& #[trigger] slots.contains_key(i)
    &&& slots[i].ptr() == crate::vspec::ptr::ptr_add(p, i as usize)
    &&& crate::vspec::ptr::in_alloc(p, i * vstd::layout::size_of::<Entry>())
    &&& slots[i].is_init()
}

/// An all-zero entry is empty, given the permission to its uninitialised
/// bytes and the token that they read as zero. Trusted: Verus does not
/// model the bytes behind a `PointsTo`; an `Entry` is a `usize` and a raw
/// pointer, whose all-zero bit patterns are `0` and the null pointer.
#[verifier::external_body]
proof fn axiom_zero_entry(tracked perm: PointsTo<Entry>, tracked z: Zeroed) -> (tracked r: PointsTo<Entry>)
    requires
        perm.is_uninit(),
        z.addr() == perm.ptr()@.addr,
        z.len() == 16,
    ensures
        r.ptr() == perm.ptr(),
        r.is_init(),
        !occupied(r.value()),
{
    unimplemented!()
}

/// Entry `i` from `p` has its permission in `slots` ([`slot_at`]) and is
/// empty.
spec fn empty_slot_at(p: *mut Entry, slots: Map<int, PointsTo<Entry>>, i: int) -> bool {
    slot_at(p, slots, i) && !occupied(slots[i].value())
}

/// The permissions of the first `n` entries from `p`, each empty, from
/// `raw`, the permission to their bytes, and `z`, the token that they read
/// as zero.
proof fn entry_slots(tracked raw: PointsToRaw, tracked z: Zeroed, p: *mut Entry, n: int) -> (tracked slots: Map<
    int,
    PointsTo<Entry>,
>)
    requires
        0 <= n,
        raw.is_range(p@.addr as int, n * 16),
        raw.provenance() == p@.provenance,
        z.addr() == p@.addr,
        z.len() == n * 16,
        p@.addr % 16 == 0,
        p@.addr != 0,
        p@.addr + n * 16 <= usize::MAX,
    ensures
        forall|i: int| 0 <= i < n ==> #[trigger] empty_slot_at(p, slots, i),
    decreases n,
{
    broadcast use vstd::raw_ptr::axiom_ptr_mut_from_data;

    if n == 0 {
        Map::tracked_empty()
    } else {
        let m = n - 1;
        let lo = p@.addr as int;
        assert(m * 16 + 16 == n * 16 && 0 <= m * 16) by (nonlinear_arith)
            requires
                m == n - 1,
                0 < n,
        ;
        // The storage's bytes are inside its allocation, so entry `m`'s
        // pointer is `m` entries past `p`.
        crate::vspec::ptr::axiom_raw_valid(&raw, p as *mut u8, n * 16);
        crate::vspec::ptr::axiom_ptr_add_addr(p, m as usize);
        let q = crate::vspec::ptr::ptr_add(p, m as usize);
        let tracked (front, last) = raw.split(Set::range(lo, lo + m * 16));
        assert(front.dom() =~= Set::range(lo, lo + m * 16));
        assert(last.dom() =~= Set::range(lo + m * 16, lo + n * 16));
        let tracked (zf, zl) = z.split(m * 16);
        let tracked mut slots = entry_slots(front, zf, p, m);
        let ghost before = slots;
        let tracked e = last.into_typed::<Entry>(q@.addr);
        assert(e.ptr()@ == q@);
        let tracked e = axiom_zero_entry(e, zl);
        slots.tracked_insert(m, e);
        assert forall|i: int| 0 <= i < n implies #[trigger] empty_slot_at(p, slots, i) by {
            if i < m {
                assert(empty_slot_at(p, before, i));
            }
        }
        slots
    }
}

/// [`entry_slots`]' result for a table of `2^lg` entries is what
/// [`Table::storage`] hands out: every slot covered, none occupied.
proof fn lemma_empty_covers(p: *mut Entry, lg: u32, slots: Map<int, PointsTo<Entry>>)
    requires
        forall|i: int| 0 <= i < cap_of(lg) ==> #[trigger] empty_slot_at(p, slots, i),
    ensures
        covers(p, lg, slots),
        forall|i: int| 0 <= i < cap_of(lg) ==> !occupied(#[trigger] slots[i].value()),
{
    assert forall|i: int| 0 <= i < cap_of(lg) implies #[trigger] slot_at(p, slots, i) by {
        assert(empty_slot_at(p, slots, i));
    }
    assert forall|i: int| 0 <= i < cap_of(lg) implies !occupied(#[trigger] slots[i].value()) by {
        assert(empty_slot_at(p, slots, i));
    }
}

/// An address aligned to a power-of-two page of at least 16 bytes is
/// aligned to 16.
proof fn lemma_page_aligned_16(a: usize, page: usize)
    requires
        meta::is_pow2(page),
        16 <= page,
        a % page == 0,
    ensures
        a % 16 == 0,
{
    let k = choose|k: nat| k < 64 && page as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
    vstd::arithmetic::power2::lemma2_to64();
    if k < 4 {
        assert(k == 0 || k == 1 || k == 2 || k == 3);
        assert(false);
    }
    vstd::arithmetic::power2::lemma_pow2_adds(4, (k - 4) as nat);
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(vstd::arithmetic::power2::pow2((k - 4) as nat) as int, 16);
    meta::lemma_mod_trans(a as int, page as int, 16);
}

proof fn lemma_occupancy_update(v: Seq<Entry>, i: int, e: Entry)
    requires
        0 <= i < v.len(),
    ensures
        occupancy(v.update(i, e)) + (if occupied(v[i]) {
            1int
        } else {
            0int
        }) == occupancy(v) + (if occupied(e) {
            1int
        } else {
            0int
        }),
    decreases v.len(),
{
    if i < v.len() - 1 {
        lemma_occupancy_update(v.drop_last(), i, e);
        assert(v.update(i, e).drop_last() =~= v.drop_last().update(i, e));
    } else {
        assert(v.update(i, e).drop_last() =~= v.drop_last());
    }
}

/// With fewer occupied slots than slots, some slot is empty.
proof fn lemma_empty_exists(v: Seq<Entry>) -> (z: int)
    requires
        occupancy(v) < v.len(),
    ensures
        0 <= z < v.len(),
        !occupied(v[z]),
    decreases v.len(),
{
    lemma_occupancy_bound(v.drop_last());
    if occupied(v.last()) {
        let z = lemma_empty_exists(v.drop_last());
        z
    } else {
        v.len() - 1
    }
}

proof fn lemma_occupancy_bound(v: Seq<Entry>)
    ensures
        occupancy(v) <= v.len(),
    decreases v.len(),
{
    if v.len() > 0 {
        lemma_occupancy_bound(v.drop_last());
    }
}

/// A table with no occupied slot.
proof fn lemma_occupancy_none(v: Seq<Entry>)
    requires
        forall|i: int| 0 <= i < v.len() ==> !occupied(#[trigger] v[i]),
    ensures
        occupancy(v) == 0,
    decreases v.len(),
{
    if v.len() > 0 {
        lemma_occupancy_none(v.drop_last());
    }
}

proof fn lemma_occupancy_take(v: Seq<Entry>, i: int)
    requires
        0 <= i < v.len(),
    ensures
        occupancy(v.take(i + 1)) == occupancy(v.take(i)) + if occupied(v[i]) {
            1nat
        } else {
            0nat
        },
{
    assert(v.take(i + 1).drop_last() =~= v.take(i));
}

/// The occupied slot `j` is what `lookup` finds for its key.
proof fn lemma_lookup_at(v: Seq<Entry>, j: int)
    requires
        keys_unique(v),
        0 <= j < v.len(),
        occupied(v[j]),
    ensures
        lookup(v, v[j].key) == Some(v[j].desc),
{
    assert(holds(v, v[j].key));
}

/// Slot arithmetic: walking `dist(h, j)` steps from `h` reaches `j`, and the
/// steps compose.
proof fn lemma_at_dist(h: int, j: int, cap: int)
    requires
        0 <= h < cap,
        0 <= j < cap,
    ensures
        0 <= dist(h, j, cap) < cap,
        at(h, dist(h, j, cap), cap) == j,
{
}

/// A table of at most `2^58` slots.
proof fn lemma_cap_bound(lg: u32)
    requires
        lg <= 58,
    ensures
        cap_of(lg) <= 0x0400_0000_0000_0000,
{
    let l = lg as usize;
    assert(l <= 58 ==> (1usize << l) <= 0x0400_0000_0000_0000usize) by (bit_vector);
}

/// `(i + 1) & (cap - 1)` is the slot after `i`.
proof fn lemma_next(i: usize, lg: u32)
    requires
        lg < 64,
        i < (1usize << lg),
    ensures
        (i + 1) as usize & sub(1usize << lg, 1) == (if i + 1 == (1usize << lg) {
            0
        } else {
            i + 1
        }),
{
    let l = lg as usize;
    assert(l < 64 && i < (1usize << l) ==> ((i + 1) as usize & sub(1usize << l, 1usize)) == (if i + 1 == (1usize
        << l) {
        0
    } else {
        i + 1
    })) by (bit_vector);
}

impl Table {
    /// Storage layout of a table of `1 << lg_cap` entries: the one source
    /// for `storage` and `release`.
    const fn layout(lg_cap: u32) -> (r: Layout)
        requires
            lg_cap <= 58,
        ensures
            layout_size(r) == 16usize << lg_cap,
            layout_align(r) == 16,
    {
        proof {
            let l = lg_cap as usize;
            assert(l <= 58 ==> (16usize << l) <= 0x7FFF_FFFF_FFFF_FFF0usize) by (bit_vector);
            assert(16usize & sub(16usize, 1usize) == 0) by (bit_vector);
            assert(size_of::<Entry>() == 16);
        }
        match Layout::from_size_align(core::mem::size_of::<Entry>() << lg_cap, 16) {
            Ok(l) => l,
            Err(_) => panic!("huge: descriptor table layout"),
        }
    }

    /// Zeroed storage for `1 << lg_cap` entries, all empty, with the
    /// permission for each: the block's permission from `Meta`
    /// (`alloc_zeroed` or `reserve_committed_raw`), cut into entries,
    /// each read as empty from the block's zero token ([`entry_slots`]).
    /// The storage is `meta`'s (`Meta::holds` for the layout's size, `16
    /// << lg_cap`); so metadata when `meta` is the process's.
    // The tracked permission map is part of the result.
    #[allow(clippy::type_complexity)]
    fn storage(meta: &Meta, page: usize, lg_cap: u32, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<(*mut Entry, bool, Tracked<Map<int, PointsTo<Entry>>>)>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_meta()),
            lg_cap <= 58,
            meta::is_pow2(page),
            16 <= page,
        ensures
            *final(ord) == *old(ord),
            r matches Some((p, _, slots)) ==> covers(p, lg_cap, slots@) && forall|i: int|
                0 <= i < cap_of(lg_cap) ==> !occupied(#[trigger] slots@[i].value()),
            r matches Some((p, _, _)) ==> meta.holds(p@.addr as int, cap_of(lg_cap) * 16),
            r matches Some((p, _, _)) ==> (meta.is_process() ==> entries_in_meta(p, lg_cap)),
    {
        proof {
            let l = lg_cap as usize;
            assert(l <= 58 ==> (16usize << l) == 16 * (1usize << l) && (16usize << l) <= 0x7FFF_FFFF_FFFF_FFF0usize)
                by (bit_vector);
            crate::meta::lemma_consts();
        }
        let layout = Table::layout(lg_cap);
        if layout.size() <= meta::MAX_BINNED {
            let (p, Tracked(raw), Tracked(z)) = meta.alloc_zeroed(layout, Tracked(ord));
            if p.is_null() {
                None
            } else {
                let e = p as *mut Entry;
                let tracked slots = entry_slots(raw, z, e, cap_of(lg_cap));
                proof {
                    lemma_empty_covers(e, lg_cap, slots);
                }
                Some((e, true, Tracked(slots)))
            }
        } else {
            let (s, Tracked(raw), Tracked(z)) = meta.reserve_committed_raw(layout.size(), page, Tracked(ord));
            let p = s?;
            let e = crate::vspec::addr::addr_to_ptr(p) as *mut Entry;
            let tracked mine = raw.split(Set::range(p as int, p + layout_size(layout))).0;
            proof {
                assert(mine.is_range(p as int, cap_of(lg_cap) * 16));
                lemma_page_aligned_16(p, page);
            }
            let tracked slots = entry_slots(mine, z, e, cap_of(lg_cap));
            proof {
                lemma_empty_covers(e, lg_cap, slots);
            }
            Some((e, false, Tracked(slots)))
        }
    }

    /// Give the storage back if it was binned. A reserved table cannot be
    /// given back; it is leaked (at most the size of the final table in
    /// total). The caller drops the table afterwards: `Meta::free`'s
    /// trusted contract is that the block is no longer used, so the slot
    /// permissions this value still holds are never used again.
    fn release(&self, meta: &Meta, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_meta()),
            self.storage_ok(),
        ensures
            *final(ord) == *old(ord),
    {
        if self.binned {
            meta.free(self.entries as *mut u8, Table::layout(self.lg_cap), Tracked(ord));
        }
    }

    /// The capacity, `2^lg_cap`.
    pub(super) closed spec fn spec_cap(self) -> int {
        cap_of(self.lg_cap)
    }

    /// The slots' contents.
    closed spec fn view(self) -> Seq<Entry> {
        Seq::new(self.spec_cap() as nat, |i: int| self.slots@[i].value())
    }

    /// The number of keys.
    pub(super) closed spec fn spec_count(self) -> int {
        self.count as int
    }

    /// The descriptor stored under `key`, if any.
    pub(super) closed spec fn lookup(self, key: usize) -> Option<*mut HugeDesc> {
        lookup(self.view(), key)
    }

    /// The storage: `2^lg_cap` slots, each with its permission, in the
    /// metadata region.
    pub(super) closed spec fn storage_ok(self) -> bool {
        &&& LG_INITIAL_TABLE <= self.lg_cap <= 58
        &&& covers(self.entries, self.lg_cap, self.slots@)
        &&& entries_in_meta(self.entries, self.lg_cap)
    }

    /// The address of slot `i`: where the permission the table reads and
    /// writes it through ([`Table::slot`], [`Table::set_slot`]) points.
    pub(super) closed spec fn slot_addr(self, i: int) -> int {
        self.slots@[i].ptr()@.addr as int
    }

    /// Every slot is 16 bytes of the metadata region.
    pub(super) proof fn lemma_slots_in_meta(self, i: int)
        requires
            self.storage_ok(),
            0 <= i < self.spec_cap(),
            crate::layout::process_regions().wf(),
        ensures
            crate::layout::process_regions().in_meta(self.slot_addr(i), 16),
    {
        assert(slot_at(self.entries, self.slots@, i));
        crate::vspec::ptr::axiom_ptr_add_addr(self.entries, i as usize);
    }

    /// The table invariant.
    pub(super) closed spec fn wf(self) -> bool {
        &&& self.storage_ok()
        &&& self.count == occupancy(self.view())
        &&& self.count < self.spec_cap()
        &&& keys_unique(self.view())
        &&& probes_reach(self.view(), self.lg_cap)
    }

    /// A well-formed table has its storage.
    pub(super) proof fn lemma_wf_storage(self)
        requires
            self.wf(),
        ensures
            self.storage_ok(),
    {
    }

    pub(super) fn new(meta: &Meta, page: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<Table>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_meta()),
            meta.is_process(),
            meta::is_pow2(page),
            16 <= page,
        ensures
            *final(ord) == *old(ord),
            r matches Some(t) ==> t.wf() && t.spec_count() == 0 && forall|k: usize| t.lookup(k) is None,
    {
        let (entries, binned, slots) = Table::storage(meta, page, LG_INITIAL_TABLE, Tracked(ord))?;
        let t = Table { entries, lg_cap: LG_INITIAL_TABLE, count: 0, binned, slots };
        proof {
            assert(cap_of(LG_INITIAL_TABLE) == 1024) by (compute_only);
            lemma_occupancy_none(t.view());
        }
        Some(t)
    }

    #[inline]
    pub(super) fn cap(&self) -> (c: usize)
        requires
            self.storage_ok(),
        ensures
            c == self.spec_cap(),
    {
        1 << self.lg_cap
    }

    #[inline]
    fn hash(&self, key: usize) -> (h: usize)
        requires
            self.storage_ok(),
        ensures
            h == ideal(key, self.lg_cap),
            h < self.spec_cap(),
    {
        proof {
            let (x, l) = ((key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15), self.lg_cap as u64);
            assert(1 <= l < 64 ==> (x >> sub(64, l)) < (1u64 << l)) by (bit_vector);
            let lu = self.lg_cap as usize;
            assert(lu < 64 ==> (1usize << lu) as int == (1u64 << (lu as u64)) as int) by (bit_vector);
        }
        ((key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - self.lg_cap)) as usize
    }

    #[inline]
    fn slot(&self, i: usize) -> (e: &Entry)
        requires
            self.storage_ok(),
            i < self.spec_cap(),
        ensures
            *e == self.view()[i as int],
    {
        debug_assert!(i < self.cap());
        proof {
            assert(slot_at(self.entries, self.slots@, i as int));
            lemma_cap_bound(self.lg_cap);
        }
        // SAFETY: `i < cap` and the storage is `cap` entries that only this
        // table (behind the huge lock) touches; the permission for slot `i`
        // stands for both.
        ptr_ref(unsafe { self.entries.add(i) }, Tracked(self.slots.borrow().tracked_borrow(i as int)))
    }

    /// Store `e` in slot `i`.
    #[inline]
    fn set_slot(&mut self, i: usize, e: Entry)
        requires
            old(self).storage_ok(),
            i < old(self).spec_cap(),
        ensures
            final(self).entries == old(self).entries,
            final(self).lg_cap == old(self).lg_cap,
            final(self).count == old(self).count,
            final(self).binned == old(self).binned,
            final(self).storage_ok(),
            final(self).view() == old(self).view().update(i as int, e),
    {
        debug_assert!(i < self.cap());
        proof {
            assert(slot_at(self.entries, self.slots@, i as int));
            lemma_cap_bound(self.lg_cap);
        }
        // SAFETY: as in `slot`.
        let p = unsafe { self.entries.add(i) };
        let tracked mut perm = self.slots.borrow_mut().tracked_remove(i as int);
        ptr_mut_write(p, Tracked(&mut perm), e);
        proof {
            self.slots.borrow_mut().tracked_insert(i as int, perm);
            assert forall|j: int| 0 <= j < self.spec_cap() implies slot_at(self.entries, self.slots@, j) by {
                assert(j != i ==> old(self).slots@[j] == self.slots@[j]);
                assert(slot_at(self.entries, old(self).slots@, j));
            }
        }
        proof {
            assert(self.view() =~= old(self).view().update(i as int, e));
        }
    }

    /// Slot index holding `key`, if present. Terminates because the table
    /// always has an empty slot (`insert` keeps `count < cap`).
    fn find(&self, key: usize) -> (r: Option<usize>)
        requires
            self.wf(),
        ensures
            r matches Some(i) ==> i < self.spec_cap() && occupied(self.view()[i as int]) && self.view()[i
                as int].key == key,
            r is None ==> !holds(self.view(), key),
    {
        let ghost cap = self.spec_cap();
        let ghost v = self.view();
        let mask = self.cap() - 1;
        let mut i = self.hash(key);
        let ghost h = i as int;
        let ghost mut s: int = 0;
        proof {
            let _ = lemma_empty_exists(v);
        }
        let ghost z = choose|z: int| 0 <= z < cap && !occupied(#[trigger] v[z]);
        proof {
            lemma_at_dist(h, z, cap);
        }
        loop
            invariant
                self.wf(),
                cap == self.spec_cap(),
                v == self.view(),
                mask == cap - 1,
                0 <= h < cap,
                h == ideal(key, self.lg_cap),
                0 <= z < cap,
                !occupied(v[z]),
                0 <= s <= dist(h, z, cap),
                i == at(h, s, cap),
                forall|d: int|
                    0 <= d < s ==> occupied(#[trigger] v[at(h, d, cap)]) && v[at(h, d, cap)].key != key,
            decreases dist(h, z, cap) - s,
        {
            let e = self.slot(i);
            if e.desc.is_null() {
                proof {
                    lemma_probe_misses(v, self.lg_cap, key, h, s);
                }
                return None;
            }
            if e.key == key {
                return Some(i);
            }
            proof {
                lemma_next(i, self.lg_cap);
                s = s + 1;
            }
            i = (i + 1) & mask;
        }
    }

    pub(super) fn get(&self, key: usize) -> (d: *mut HugeDesc)
        requires
            self.wf(),
        ensures
            self.lookup(key) matches Some(p) ==> d == p,
            self.lookup(key) is None ==> d@.addr == 0,
    {
        match self.find(key) {
            Some(i) => {
                proof {
                    lemma_lookup_at(self.view(), i as int);
                }
                self.slot(i).desc
            },
            None => ptr::null_mut(),
        }
    }

    /// Insert `key`, which must not be present. Aborts rather than fill
    /// the last slot: a probe for an absent key ends at an empty slot, so
    /// a full table would spin in `find`. `ensure` keeps production load
    /// at or below one half; only a free after a failed growth (its
    /// `ensure` is best effort) can get here.
    // The negated test is the form the proof states.
    #[allow(clippy::nonminimal_bool)]
    pub(super) fn insert(&mut self, key: usize, desc: *mut HugeDesc)
        requires
            crate::sys::may_abort(),
            old(self).wf(),
            old(self).lookup(key) is None,
            desc@.addr != 0,
        ensures
            final(self).wf(),
            final(self).spec_count() == old(self).spec_count() + 1,
            final(self).spec_cap() == old(self).spec_cap(),
            forall|k: usize|
                #[trigger] final(self).lookup(k) == if k == key {
                    Some(desc)
                } else {
                    old(self).lookup(k)
                },
    {
        if !(self.count + 1 < self.cap()) {
            crate::sys::abort_fmt(format_args!("huge: descriptor table full"));
        }
        let ghost cap = self.spec_cap();
        let ghost v = self.view();
        let mask = self.cap() - 1;
        let mut i = self.hash(key);
        let ghost h = i as int;
        let ghost mut s: int = 0;
        proof {
            let _ = lemma_empty_exists(v);
        }
        let ghost z = choose|z: int| 0 <= z < cap && !occupied(#[trigger] v[z]);
        proof {
            lemma_at_dist(h, z, cap);
        }
        loop
            invariant
                self.wf(),
                self.count + 1 < cap,
                cap == self.spec_cap(),
                v == self.view(),
                *self == *old(self),
                !holds(v, key),
                desc@.addr != 0,
                mask == cap - 1,
                0 <= h < cap,
                h == ideal(key, self.lg_cap),
                0 <= z < cap,
                !occupied(v[z]),
                0 <= s <= dist(h, z, cap),
                i == at(h, s, cap),
                forall|d: int| 0 <= d < s ==> occupied(#[trigger] v[at(h, d, cap)]),
            decreases dist(h, z, cap) - s,
        {
            let e = self.slot(i);
            if e.desc.is_null() {
                self.set_slot(i, Entry { key, desc });
                self.count += 1;
                proof {
                    lemma_insert_keeps_wf(v, self.view(), self.lg_cap, i as int, key, desc, h, s);
                }
                return;
            }
            assert!(e.key != key, "huge: key inserted twice");
            proof {
                lemma_next(i, self.lg_cap);
                s = s + 1;
            }
            i = (i + 1) & mask;
        }
    }
}

/// A probe for `key` that walked `s` occupied slots holding other keys from
/// `h` and then found an empty one: `key` is absent.
proof fn lemma_probe_misses(v: Seq<Entry>, lg: u32, key: usize, h: int, s: int)
    requires
        probes_reach(v, lg),
        v.len() > 0,
        0 <= h < v.len(),
        h == ideal(key, lg),
        0 <= s < v.len(),
        !occupied(v[at(h, s, v.len() as int)]),
        forall|d: int|
            0 <= d < s ==> occupied(#[trigger] v[at(h, d, v.len() as int)]) && v[at(h, d, v.len() as int)].key
                != key,
    ensures
        !holds(v, key),
{
    let cap = v.len() as int;
    assert forall|j: int| 0 <= j < cap && occupied(#[trigger] v[j]) implies v[j].key != key by {
        if v[j].key == key {
            lemma_at_dist(h, j, cap);
            let t = dist(h, j, cap);
            assert(reachable(v, lg, j));
            if t < s {
                assert(v[at(h, t, cap)].key != key);
            } else if t > s {
                assert(occupied(v[at(ideal(v[j].key, lg), s, cap)]));
            }
        }
    }
}

/// Filling the empty slot `i`, `s` steps along the probe path of `key`
/// from its ideal slot `h`, with every earlier slot of the path occupied,
/// keeps the invariant and adds `key` to the map.
proof fn lemma_insert_keeps_wf(
    v: Seq<Entry>,
    w: Seq<Entry>,
    lg: u32,
    i: int,
    key: usize,
    desc: *mut HugeDesc,
    h: int,
    s: int,
)
    requires
        keys_unique(v),
        probes_reach(v, lg),
        !holds(v, key),
        desc@.addr != 0,
        0 <= h < v.len(),
        h == ideal(key, lg),
        0 <= s < v.len(),
        i == at(h, s, v.len() as int),
        !occupied(v[i]),
        forall|d: int| 0 <= d < s ==> occupied(#[trigger] v[at(h, d, v.len() as int)]),
        w == v.update(i, Entry { key, desc }),
    ensures
        occupancy(w) == occupancy(v) + 1,
        keys_unique(w),
        probes_reach(w, lg),
        forall|k: usize| #[trigger] lookup(w, k) == if k == key {
            Some(desc)
        } else {
            lookup(v, k)
        },
{
    let cap = v.len() as int;
    lemma_occupancy_update(v, i, Entry { key, desc });
    assert(keys_unique(w)) by {
        assert forall|a: int, b: int|
            0 <= a < cap && 0 <= b < cap && occupied(#[trigger] w[a]) && occupied(#[trigger] w[b]) && w[a].key
                == w[b].key implies a == b by {
            if a != i && b != i {
                assert(v[a] == w[a] && v[b] == w[b]);
            } else if a == i && b != i {
                assert(occupied(v[b]) && v[b].key == key);
            } else if a != i && b == i {
                assert(occupied(v[a]) && v[a].key == key);
            }
        }
    }
    assert forall|t: int| 0 <= t < cap && occupied(#[trigger] w[t]) implies reachable(w, lg, t) by {
        if t == i {
            assert forall|d: int| 0 <= d < dist(ideal(w[t].key, lg), t, cap) implies occupied(
                #[trigger] w[at(ideal(w[t].key, lg), d, cap)],
            ) by {
                assert(occupied(v[at(h, d, cap)]));
            }
        } else {
            assert(reachable(v, lg, t));
            assert forall|d: int| 0 <= d < dist(ideal(w[t].key, lg), t, cap) implies occupied(
                #[trigger] w[at(ideal(w[t].key, lg), d, cap)],
            ) by {
                assert(occupied(v[at(ideal(v[t].key, lg), d, cap)]));
            }
        }
    }
    assert forall|k: usize| #[trigger] lookup(w, k) == if k == key {
        Some(desc)
    } else {
        lookup(v, k)
    } by {
        if k == key {
            lemma_lookup_at(w, i);
        } else if holds(v, k) {
            let j = choose|j: int| 0 <= j < v.len() && occupied(#[trigger] v[j]) && v[j].key == k;
            lemma_lookup_at(v, j);
            lemma_lookup_at(w, j);
        } else {
            assert(!holds(w, k)) by {
                if holds(w, k) {
                    let j = choose|j: int| 0 <= j < w.len() && occupied(#[trigger] w[j]) && w[j].key == k;
                    assert(j != i);
                    assert(v[j] == w[j]);
                }
            }
        }
    }
}

impl Table {
    /// Remove `key`, which must be present.
    pub(super) fn remove(&mut self, key: usize)
        requires
            old(self).wf(),
            old(self).lookup(key) is Some,
        ensures
            final(self).wf(),
            final(self).spec_count() == old(self).spec_count() - 1,
            final(self).spec_cap() == old(self).spec_cap(),
            forall|k: usize|
                #[trigger] final(self).lookup(k) == if k == key {
                    None
                } else {
                    old(self).lookup(k)
                },
    {
        let Some(mut i) = self.find(key) else {
            panic!("huge: descriptor key {key} missing from the table");
        };
        let ghost cap = self.spec_cap();
        let ghost lg = self.lg_cap;
        let ghost v0 = self.view();
        let ghost i0 = i as int;
        let mask = self.cap() - 1;
        let ghost mut t: int = 0;
        proof {
            let _ = lemma_empty_exists(v0);
        }
        let ghost z = choose|z: int| 0 <= z < cap && !occupied(#[trigger] v0[z]);
        proof {
            lemma_at_dist(i0, z, cap);
            lemma_remove_start(v0, lg, i0, key);
        }
        // Backward shift: pull later entries of the same probe chain into
        // the hole unless their ideal slot lies past it.
        let mut j = i;
        loop
            invariant_except_break
                0 <= i0 < cap,
                0 <= z < cap,
                !occupied(v0[z]),
                0 <= t < dist(i0, z, cap),
                j == at(i0, t, cap),
                0 <= i < cap,
                dist(i0, i as int, cap) <= t,
                occupied(self.view()[i as int]),
                forall|p: int| 0 <= p < cap ==> occupied(#[trigger] self.view()[p]) == occupied(v0[p]),
                shifting(self.view(), lg, i as int, j as int, v0, key),
            invariant
                self.storage_ok(),
                self.lg_cap == lg,
                self.count == old(self).count,
                cap == self.spec_cap(),
                mask == cap - 1,
                v0 == old(self).view(),
                v0.len() == cap,
                old(self).wf(),
            ensures
                self.storage_ok(),
                self.lg_cap == lg,
                self.count == old(self).count,
                0 <= i < cap,
                occupancy(vacate(self.view(), i as int)) + 1 == old(self).count,
                keys_unique(vacate(self.view(), i as int)),
                probes_reach(vacate(self.view(), i as int), lg),
                forall|k: usize|
                    #[trigger] lookup(vacate(self.view(), i as int), k) == if k == key {
                        None
                    } else {
                        lookup(v0, k)
                    },
            decreases dist(i0, z, cap) - t,
        {
            let ghost jp = j as int;
            proof {
                lemma_next(j, lg);
                lemma_at_dist(i0, i as int, cap);
            }
            j = (j + 1) & mask;
            proof {
                t = t + 1;
            }
            let e = *self.slot(j);
            if e.desc.is_null() {
                proof {
                    lemma_shift_done(self.view(), lg, i as int, jp, j as int, v0, key);
                    lemma_occupancy_same(self.view(), v0);
                    lemma_occupancy_update(self.view(), i as int, vacant());
                }
                break;
            }
            let ideal = self.hash(e.key);
            let stays = if i <= j {
                i < ideal && ideal <= j
            } else {
                i < ideal || ideal <= j
            };
            if !stays {
                let ghost v = self.view();
                self.set_slot(i, e);
                proof {
                    lemma_shift_moves(v, lg, i as int, jp, j as int, v0, key);
                }
                i = j;
            } else {
                proof {
                    lemma_shift_stays(self.view(), lg, i as int, jp, j as int, v0, key);
                }
            }
        }
        self.set_slot(i, Entry { key: 0, desc: ptr::null_mut() });
        self.count = self.count.checked_sub(1).unwrap_or_else(
            ||
                requires
                    false,
                { panic!("huge: descriptor table count underflow") },
        );
    }

    /// Make room for `extra` more keys at a load of one half or less. On
    /// failure the table is unchanged and still works, only fuller.
    pub(super) fn ensure(&mut self, meta: &Meta, page: usize, extra: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Result<(), ()>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_meta()),
            old(self).wf(),
            meta.is_process(),
            meta::is_pow2(page),
            16 <= page,
            old(self).spec_count() + extra <= 0x0100_0000_0000_0000,
        ensures
            *final(ord) == *old(ord),
            final(self).wf(),
            final(self).spec_count() == old(self).spec_count(),
            forall|k: usize| #[trigger] final(self).lookup(k) == old(self).lookup(k),
            r is Ok ==> (final(self).spec_count() + extra) * 2 <= final(self).spec_cap(),
    {
        if (self.count + extra) * 2 <= self.cap() {
            return Ok(());
        }
        let ghost n = ((self.count + extra) * 2) as usize;
        // `old` names the table being rehashed below.
        let ghost (view_in, count_in) = (self.view(), self.count);
        proof {
            lemma_grow_bound(n, self.lg_cap);
        }
        let mut lg = self.lg_cap + 1;
        while (self.count + extra) * 2 > (1usize << lg)
            invariant
                self.wf(),
                self.count + extra <= 0x0100_0000_0000_0000,
                n == (self.count + extra) * 2,
                old(self).lg_cap < lg <= 57,
                self.lg_cap == old(self).lg_cap,
            decreases 57 - lg,
        {
            proof {
                lemma_grow_bound(n, lg);
            }
            lg += 1;
        }
        let (entries, binned, slots) = Table::storage(meta, page, lg, Tracked(ord)).ok_or(())?;
        let tracked mut old_slots = Map::tracked_empty();
        proof {
            vstd::modes::tracked_swap(self.slots.borrow_mut(), &mut old_slots);
        }
        let old = Table { entries: self.entries, lg_cap: self.lg_cap, count: self.count, binned: self.binned, slots: Tracked(old_slots) };
        *self = Table { entries, lg_cap: lg, count: 0, binned, slots };
        let ghost ov = old.view();
        proof {
            assert(ov =~= view_in);
            lemma_occupancy_none(self.view());
            lemma_occupancy_bound(ov);
            assert(ov.take(0) =~= Seq::<Entry>::empty());
        }
        for i in 0..old.cap()
            invariant
                crate::sys::may_abort(),
                old.wf(),
                ov == old.view(),
                ov == view_in,
                old.count == count_in,
                self.wf(),
                (old.count + extra) * 2 <= self.spec_cap(),
                (old.count + extra) * 2 > cap_of(old.lg_cap),
                self.count == occupancy(ov.take(i as int)),
                forall|k: usize| #[trigger] self.lookup(k) == lookup(ov.take(i as int), k),
        {
            let e = *old.slot(i);
            proof {
                lemma_occupancy_take(ov, i as int);
                lemma_occupancy_prefix(ov, i as int + 1);
                lemma_unique_prefix(ov, i as int);
                lemma_unique_prefix(ov, i as int + 1);
            }
            if !e.desc.is_null() {
                proof {
                    lemma_prefix_misses(ov, i as int);
                }
                self.insert(e.key, e.desc);
            }
            proof {
                lemma_lookup_take(ov, i as int);
            }
        }
        proof {
            assert(ov.take(ov.len() as int) =~= ov);
        }
        old.release(meta, Tracked(ord));
        Ok(())
    }
}

/// The growth loop of `ensure` stops by `2^57`: `n` more keys than half of
/// `2^lg` slots means `lg` is below 57.
proof fn lemma_grow_bound(n: usize, lg: u32)
    requires
        lg < 64,
        n > (1usize << lg),
        n <= 0x0200_0000_0000_0000,
    ensures
        lg < 57,
{
    let l = lg as usize;
    assert(l < 64 && n > (1usize << l) && n <= 0x0200_0000_0000_0000usize ==> l < 57) by (bit_vector);
}

proof fn lemma_occupancy_prefix(v: Seq<Entry>, i: int)
    requires
        0 <= i <= v.len(),
    ensures
        occupancy(v.take(i)) <= occupancy(v),
    decreases v.len() - i,
{
    if i < v.len() {
        lemma_occupancy_take(v, i);
        lemma_occupancy_prefix(v, i + 1);
    } else {
        assert(v.take(i) =~= v);
    }
}

proof fn lemma_unique_prefix(v: Seq<Entry>, i: int)
    requires
        keys_unique(v),
        0 <= i <= v.len(),
    ensures
        keys_unique(v.take(i)),
{
    let w = v.take(i);
    assert forall|a: int, b: int|
        0 <= a < w.len() && 0 <= b < w.len() && occupied(#[trigger] w[a]) && occupied(#[trigger] w[b]) && w[a].key
            == w[b].key implies a == b by {
        assert(w[a] == v[a] && w[b] == v[b]);
    }
}

/// The key of the occupied slot `i` is not among the slots before it.
proof fn lemma_prefix_misses(v: Seq<Entry>, i: int)
    requires
        keys_unique(v),
        0 <= i < v.len(),
        occupied(v[i]),
    ensures
        lookup(v.take(i), v[i].key) is None,
{
    let w = v.take(i);
    if holds(w, v[i].key) {
        let j = choose|j: int| 0 <= j < w.len() && occupied(#[trigger] w[j]) && w[j].key == v[i].key;
        assert(w[j] == v[j]);
    }
}

/// Extending the prefix by slot `i` adds its entry, if any, to the map.
proof fn lemma_lookup_take(v: Seq<Entry>, i: int)
    requires
        keys_unique(v),
        0 <= i < v.len(),
    ensures
        forall|k: usize| #[trigger] lookup(v.take(i + 1), k) == if occupied(v[i]) && k == v[i].key {
            Some(v[i].desc)
        } else {
            lookup(v.take(i), k)
        },
{
    let (a, b) = (v.take(i), v.take(i + 1));
    lemma_unique_prefix(v, i);
    lemma_unique_prefix(v, i + 1);
    assert forall|k: usize| #[trigger] lookup(b, k) == if occupied(v[i]) && k == v[i].key {
        Some(v[i].desc)
    } else {
        lookup(a, k)
    } by {
        if occupied(v[i]) && k == v[i].key {
            assert(b[i] == v[i]);
            lemma_lookup_at(b, i);
        } else if holds(a, k) {
            let j = choose|j: int| 0 <= j < a.len() && occupied(#[trigger] a[j]) && a[j].key == k;
            assert(a[j] == b[j]);
            lemma_lookup_at(a, j);
            lemma_lookup_at(b, j);
        } else if holds(b, k) {
            let j = choose|j: int| 0 <= j < b.len() && occupied(#[trigger] b[j]) && b[j].key == k;
            assert(j != i);
            assert(a[j] == b[j]);
        }
    }
}

/// An empty slot.
spec fn vacant() -> Entry {
    Entry { key: 0, desc: vstd::raw_ptr::ptr_null_mut() }
}

/// `v` with slot `i` emptied.
spec fn vacate(v: Seq<Entry>, i: int) -> Seq<Entry> {
    v.update(i, vacant())
}

/// On the probe path of the entry at `s` in `w`, slot `p` is occupied, or
/// is the hole `i` and `s` lies past the scanned slots `(i, j]`.
spec fn hole_ok(w: Seq<Entry>, i: int, j: int, s: int, p: int) -> bool {
    if p == i {
        dist(i, s, w.len() as int) > dist(i, j, w.len() as int)
    } else {
        occupied(w[p])
    }
}

/// [`reachable`] with the hole at `i` allowed on the paths of entries not
/// yet scanned.
spec fn reachable_past(w: Seq<Entry>, lg: u32, i: int, j: int, s: int) -> bool {
    forall|d: int|
        0 <= d < dist(ideal(w[s].key, lg), s, w.len() as int) ==> #[trigger] hole_ok(
            w,
            i,
            j,
            s,
            at(ideal(w[s].key, lg), d, w.len() as int),
        )
}

/// The state of `remove`'s backward shift: `v` with the hole `i` emptied
/// is the table `v0` without `key`, and every entry is reachable except
/// through the hole from past the scanned slots `(i, j]`.
spec fn shifting(v: Seq<Entry>, lg: u32, i: int, j: int, v0: Seq<Entry>, key: usize) -> bool {
    let w = vacate(v, i);
    &&& keys_unique(w)
    &&& forall|k: usize| #[trigger] lookup(w, k) == if k == key {
        None
    } else {
        lookup(v0, k)
    }
    &&& forall|s: int| 0 <= s < v.len() && occupied(#[trigger] w[s]) ==> reachable_past(w, lg, i, j, s)
}

proof fn lemma_occupancy_same(a: Seq<Entry>, b: Seq<Entry>)
    requires
        a.len() == b.len(),
        forall|p: int| 0 <= p < a.len() ==> occupied(#[trigger] a[p]) == occupied(b[p]),
    ensures
        occupancy(a) == occupancy(b),
    decreases a.len(),
{
    if a.len() > 0 {
        lemma_occupancy_same(a.drop_last(), b.drop_last());
    }
}

/// Moving or removing entries keeps `lookup` for the keys that stay, when
/// `w`'s occupied slots are `v`'s, less `key`, with the same descriptors.
proof fn lemma_lookup_same(v: Seq<Entry>, w: Seq<Entry>, key: usize, k: usize)
    requires
        keys_unique(v),
        keys_unique(w),
        k != key,
        forall|a: int|
            0 <= a < v.len() && occupied(#[trigger] v[a]) && v[a].key != key ==> exists|b: int|
                0 <= b < w.len() && occupied(#[trigger] w[b]) && w[b].key == v[a].key && w[b].desc == v[a].desc,
        forall|b: int|
            0 <= b < w.len() && occupied(#[trigger] w[b]) ==> exists|a: int|
                0 <= a < v.len() && occupied(#[trigger] v[a]) && v[a].key == w[b].key && v[a].desc == w[b].desc,
    ensures
        lookup(w, k) == lookup(v, k),
{
    if holds(v, k) {
        let a = choose|a: int| 0 <= a < v.len() && occupied(#[trigger] v[a]) && v[a].key == k;
        lemma_lookup_at(v, a);
        let b = choose|b: int| 0 <= b < w.len() && occupied(#[trigger] w[b]) && w[b].key == v[a].key && w[b].desc
            == v[a].desc;
        lemma_lookup_at(w, b);
    } else if holds(w, k) {
        let b = choose|b: int| 0 <= b < w.len() && occupied(#[trigger] w[b]) && w[b].key == k;
        let a = choose|a: int| 0 <= a < v.len() && occupied(#[trigger] v[a]) && v[a].key == w[b].key && v[a].desc
            == w[b].desc;
    }
}

/// The shift starts with the hole at `key`'s slot `i0` and nothing scanned.
proof fn lemma_remove_start(v0: Seq<Entry>, lg: u32, i0: int, key: usize)
    requires
        keys_unique(v0),
        probes_reach(v0, lg),
        0 <= i0 < v0.len(),
        occupied(v0[i0]),
        v0[i0].key == key,
    ensures
        shifting(v0, lg, i0, i0, v0, key),
{
    let w = vacate(v0, i0);
    let cap = v0.len() as int;
    assert(keys_unique(w)) by {
        assert forall|a: int, b: int|
            0 <= a < cap && 0 <= b < cap && occupied(#[trigger] w[a]) && occupied(#[trigger] w[b]) && w[a].key
                == w[b].key implies a == b by {
            assert(v0[a] == w[a] && v0[b] == w[b]);
        }
    }
    assert forall|k: usize| #[trigger] lookup(w, k) == if k == key {
        None
    } else {
        lookup(v0, k)
    } by {
        if k == key {
            if holds(w, k) {
                let b = choose|b: int| 0 <= b < w.len() && occupied(#[trigger] w[b]) && w[b].key == k;
                assert(v0[b] == w[b]);
            }
        } else {
            assert forall|a: int|
                0 <= a < v0.len() && occupied(#[trigger] v0[a]) && v0[a].key != key implies exists|b: int|
                    0 <= b < w.len() && occupied(#[trigger] w[b]) && w[b].key == v0[a].key && w[b].desc
                        == v0[a].desc by {
                assert(w[a] == v0[a]);
            }
            assert forall|b: int| 0 <= b < w.len() && occupied(#[trigger] w[b]) implies exists|a: int|
                0 <= a < v0.len() && occupied(#[trigger] v0[a]) && v0[a].key == w[b].key && v0[a].desc
                    == w[b].desc by {
                assert(w[b] == v0[b]);
            }
            lemma_lookup_same(v0, w, key, k);
        }
    }
    assert forall|s: int| 0 <= s < cap && occupied(#[trigger] w[s]) implies reachable_past(w, lg, i0, i0, s) by {
        assert(w[s] == v0[s]);
        assert(reachable(v0, lg, s));
        assert forall|d: int| 0 <= d < dist(ideal(w[s].key, lg), s, cap) implies #[trigger] hole_ok(
            w,
            i0,
            i0,
            s,
            at(ideal(w[s].key, lg), d, cap),
        ) by {
            assert(occupied(v0[at(ideal(v0[s].key, lg), d, cap)]));
        }
    }
}

/// The scanned entry at `j` stays: its ideal slot lies in `(i, j]`.
proof fn lemma_shift_stays(v: Seq<Entry>, lg: u32, i: int, jp: int, j: int, v0: Seq<Entry>, key: usize)
    requires
        shifting(v, lg, i, jp, v0, key),
        v.len() >= 2,
        0 <= i < v.len(),
        0 <= jp < v.len(),
        j == at(jp, 1, v.len() as int),
        j != i,
        occupied(v[j]),
        1 <= dist(i, ideal(v[j].key, lg), v.len() as int) <= dist(i, j, v.len() as int),
    ensures
        shifting(v, lg, i, j, v0, key),
{
    let w = vacate(v, i);
    let cap = v.len() as int;
    assert forall|s: int| 0 <= s < cap && occupied(#[trigger] w[s]) implies reachable_past(w, lg, i, j, s) by {
        assert(reachable_past(w, lg, i, jp, s));
        let h = ideal(w[s].key, lg);
        assert forall|d: int| 0 <= d < dist(h, s, cap) implies #[trigger] hole_ok(w, i, j, s, at(h, d, cap)) by {
            assert(hole_ok(w, i, jp, s, at(h, d, cap)));
        }
    }
}

/// The scanned entry at `j` moves into the hole `i`, and `j` becomes the
/// hole.
proof fn lemma_shift_moves(v: Seq<Entry>, lg: u32, i: int, jp: int, j: int, v0: Seq<Entry>, key: usize)
    requires
        shifting(v, lg, i, jp, v0, key),
        v.len() >= 2,
        0 <= i < v.len(),
        0 <= jp < v.len(),
        j == at(jp, 1, v.len() as int),
        j != i,
        occupied(v[j]),
        0 <= ideal(v[j].key, lg) < v.len(),
        !(1 <= dist(i, ideal(v[j].key, lg), v.len() as int) <= dist(i, j, v.len() as int)),
    ensures
        shifting(v.update(i, v[j]), lg, j, j, v0, key),
{
    let cap = v.len() as int;
    let e = v[j];
    let w = vacate(v, i);
    let v2 = v.update(i, e);
    let w2 = vacate(v2, j);
    assert(w2 =~= w.update(i, e).update(j, vacant()));
    assert(keys_unique(w2)) by {
        assert forall|a: int, b: int|
            0 <= a < cap && 0 <= b < cap && occupied(#[trigger] w2[a]) && occupied(#[trigger] w2[b]) && w2[a].key
                == w2[b].key implies a == b by {
            let a1 = if a == i { j } else { a };
            let b1 = if b == i { j } else { b };
            assert(w[a1] == w2[a] && w[b1] == w2[b]);
        }
    }
    assert forall|k: usize| #[trigger] lookup(w2, k) == if k == key {
        None
    } else {
        lookup(v0, k)
    } by {
        if holds(w2, k) {
            let b = choose|b: int| 0 <= b < w2.len() && occupied(#[trigger] w2[b]) && w2[b].key == k;
            let b1 = if b == i { j } else { b };
            assert(w[b1] == w2[b]);
            lemma_lookup_at(w, b1);
            lemma_lookup_at(w2, b);
        } else if holds(w, k) {
            let a = choose|a: int| 0 <= a < w.len() && occupied(#[trigger] w[a]) && w[a].key == k;
            let a2 = if a == j { i } else { a };
            assert(w2[a2] == w[a]);
            assert(occupied(w2[a2]) && w2[a2].key == k);
        }
        assert(lookup(w2, k) == lookup(w, k));
        assert(lookup(w, k) == if k == key {
            None
        } else {
            lookup(v0, k)
        });
    }
    assert forall|s: int| 0 <= s < cap && occupied(#[trigger] w2[s]) implies reachable_past(w2, lg, j, j, s) by {
        let s1 = if s == i { j } else { s };
        assert(w[s1] == w2[s]);
        assert(reachable_past(w, lg, i, jp, s1));
        let h = ideal(w2[s].key, lg);
        if s == i {
            // The path from `h` to `i` is the start of the path from `h` to
            // `j`, which passes the hole.
            assert(dist(h, j, cap) == dist(h, i, cap) + dist(i, j, cap));
        }
        assert forall|d: int| 0 <= d < dist(h, s, cap) implies #[trigger] hole_ok(w2, j, j, s, at(h, d, cap)) by {
            let p = at(h, d, cap);
            assert(hole_ok(w, i, jp, s1, at(ideal(w[s1].key, lg), d, cap)));
            if p != i && p != j {
                assert(w2[p] == w[p]);
            }
        }
    }
}

/// The scan reached an empty slot `j`: with the hole emptied, every entry
/// is reachable.
proof fn lemma_shift_done(v: Seq<Entry>, lg: u32, i: int, jp: int, j: int, v0: Seq<Entry>, key: usize)
    requires
        shifting(v, lg, i, jp, v0, key),
        v.len() >= 2,
        0 <= i < v.len(),
        0 <= jp < v.len(),
        j == at(jp, 1, v.len() as int),
        j != i,
        !occupied(v[j]),
    ensures
        probes_reach(vacate(v, i), lg),
{
    let w = vacate(v, i);
    let cap = v.len() as int;
    assert forall|s: int| 0 <= s < cap && occupied(#[trigger] w[s]) implies reachable(w, lg, s) by {
        assert(reachable_past(w, lg, i, jp, s));
        let h = ideal(w[s].key, lg);
        assert forall|d: int| 0 <= d < dist(h, s, cap) implies occupied(#[trigger] w[at(h, d, cap)]) by {
            assert(hole_ok(w, i, jp, s, at(h, d, cap)));
            if at(h, d, cap) == i {
                // Past the hole the path reaches the empty `j` before `s`.
                let d1 = d + dist(i, j, cap);
                assert(at(h, d1, cap) == j);
                assert(hole_ok(w, i, jp, s, at(h, d1, cap)));
            }
        }
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;
    use crate::huge::tests::{Env, page_size};
    use std::vec::Vec;

    /// Backward-shift deletion across the wrap from the last slot to slot
    /// 0, on the table alone: keys that hash to the last slot and to slot
    /// 0 collide there, and removing any of them must pull the rest of the
    /// chain back over the wrap while an entry already in its ideal slot
    /// stays put. The descriptors are opaque non-null pointers; the table
    /// never dereferences them.
    #[test]
    fn table_removal_shifts_across_the_wrap_around() {
        let env = Env::new();
        let page = page_size();
        let mut t = Table::new(&env.meta, page, Tracked::assume_new()).expect("table");
        let last = t.cap() - 1;
        let key_with_hash =
            |t: &Table, h: usize, from: usize| (from..).find(|&k| t.hash(k) == h).unwrap();
        // Three keys hashing to the last slot, one hashing to slot 0 and
        // one hashing to slot 1.
        let a = key_with_hash(&t, last, 1);
        let b = key_with_hash(&t, last, a + 1);
        let d = key_with_hash(&t, last, b + 1);
        let c = key_with_hash(&t, 0, 1);
        let e = key_with_hash(&t, 1, 1);
        let desc = |k: usize| k as *mut HugeDesc;

        // Insertion order a, b, c, d: a takes the last slot, b wraps to 0,
        // c (ideal 0) is pushed to 1, d (ideal last) to 2.
        for k in [a, b, c, d] {
            t.insert(k, desc(k));
        }
        assert_eq!(
            [t.find(a), t.find(b), t.find(c), t.find(d)],
            [Some(last), Some(0), Some(1), Some(2)]
        );
        // Removing the head of the chain pulls b back across the wrap, c
        // into its ideal slot and d after it.
        t.remove(a);
        assert_eq!(
            [t.find(b), t.find(c), t.find(d)],
            [Some(last), Some(0), Some(1)]
        );
        assert!(t.find(a).is_none() && t.get(a).is_null());
        assert_eq!(t.count, 3);
        for k in [b, c, d] {
            assert_eq!(t.get(k), desc(k));
        }
        // Removing the entry at slot 0 with the chain continuing past it.
        t.remove(c);
        assert_eq!([t.find(b), t.find(d)], [Some(last), Some(0)]);
        // An entry already in its ideal slot is not moved into the hole
        // before it: e sits at 1, its ideal, and stays when d leaves 0.
        t.insert(e, desc(e));
        assert_eq!(t.find(e), Some(1));
        t.remove(d);
        assert_eq!(
            [t.find(b), t.find(e), t.find(d)],
            [Some(last), Some(1), None]
        );
        t.remove(b);
        assert_eq!([t.find(b), t.find(e)], [None, Some(1)]);
        assert_eq!(t.count, 1);
        // The wrap slot is reusable.
        t.insert(a, desc(a));
        assert_eq!(t.find(a), Some(last));
        assert_eq!(t.count, 2);
    }

    /// `Table::ensure` fails cleanly when the metadata region cannot hold
    /// the grown table: the old table is untouched and still answers, and
    /// keeps taking keys up to its capacity.
    #[test]
    fn table_growth_failure_leaves_the_table_working() {
        // 64 KiB of metadata: the initial 16 KiB table fits and so does
        // its first doubling (32 KiB); the second (64 KiB) does not.
        let env = Env::with_meta_len(64 * 1024);
        let page = page_size();
        let mut t = Table::new(&env.meta, page, Tracked::assume_new()).expect("table");
        assert_eq!(t.cap(), 1 << LG_INITIAL_TABLE);
        let desc = |k: usize| k as *mut HugeDesc;
        let mut keys = Vec::new();
        let mut grew = 0;
        loop {
            let cap = t.cap();
            if t.ensure(&env.meta, page, 1, Tracked::assume_new()).is_err() {
                break;
            }
            if t.cap() > cap {
                grew += 1;
            }
            let k = keys.len() + 1;
            t.insert(k, desc(k));
            keys.push(k);
            assert!(keys.len() <= 1 << 16, "the table never ran out of metadata");
        }
        assert_eq!(grew, 1, "one doubling fits, the second does not");
        assert_eq!(t.cap(), 2 << LG_INITIAL_TABLE);
        assert_eq!(t.count, keys.len());
        assert_eq!(
            keys.len() * 2,
            t.cap(),
            "ensure refused exactly at half load"
        );
        for &k in &keys {
            assert_eq!(t.get(k), desc(k), "key {k} lost in the failed growth");
        }
        // The table is fuller than `ensure` would allow, and still works
        // up to one slot short of full (a probe for an absent key ends at
        // an empty slot; `insert` refuses to take the last one, see
        // `the_table_never_fills_its_last_slot`).
        let extra = t.cap() - t.count - 1;
        for k in keys.len() + 1..=keys.len() + extra {
            assert!(t.ensure(&env.meta, page, 1, Tracked::assume_new()).is_err());
            t.insert(k, desc(k));
            assert!(t.find(k).is_some());
        }
        assert_eq!(t.count, t.cap() - 1);
        assert_eq!(t.get(0), ptr::null_mut());
        t.remove(1);
        assert_eq!(t.get(1), ptr::null_mut());
        assert_eq!(t.get(2), desc(2));
        assert_eq!(t.get(keys.len() + extra), desc(keys.len() + extra));
    }

    /// `insert` without `ensure` (as a free after a failed growth does)
    /// takes keys until exactly one slot is left; absent keys are still
    /// found absent (the probe ends at that slot), and the insert that
    /// would fill it aborts instead of leaving a table that `find` cannot
    /// terminate on.
    #[test]
    fn the_table_never_fills_its_last_slot() {
        let env = Env::new();
        let mut t = Table::new(&env.meta, page_size(), Tracked::assume_new()).expect("table");
        let cap = t.cap();
        let desc = |k: usize| k as *mut HugeDesc;
        for k in 1..cap {
            t.insert(k, desc(k));
        }
        assert_eq!(t.count, cap - 1);
        for k in [0, cap, cap + 1, usize::MAX] {
            assert!(t.find(k).is_none(), "absent key {k} found");
            assert!(t.get(k).is_null());
        }
        for k in [1, cap / 2, cap - 1] {
            assert_eq!(t.get(k), desc(k));
        }
        // Removing and re-adding at the limit works: one slot stays free.
        t.remove(cap / 2);
        assert!(t.get(cap / 2).is_null());
        t.insert(cap / 2, desc(cap / 2));
        assert_eq!(t.get(cap / 2), desc(cap / 2));
        assert_eq!(t.count, cap - 1);
    }

    #[test]
    #[should_panic(expected = "descriptor table full")]
    fn the_table_aborts_before_its_last_slot_fills() {
        let env = Env::new();
        let mut t = Table::new(&env.meta, page_size(), Tracked::assume_new()).expect("table");
        let cap = t.cap();
        for k in 1..cap {
            t.insert(k, k as *mut HugeDesc);
        }
        assert_eq!(t.count, cap - 1);
        t.insert(cap, cap as *mut HugeDesc);
    }

    #[test]
    #[should_panic(expected = "key inserted twice")]
    fn the_table_rejects_a_duplicate_key() {
        let env = Env::new();
        let mut t = Table::new(&env.meta, page_size(), Tracked::assume_new()).unwrap();
        let d = 0x10 as *mut HugeDesc;
        t.insert(7, d);
        assert_eq!(t.count, 1);
        t.insert(7, d);
    }

    #[test]
    #[should_panic(expected = "descriptor table count underflow")]
    fn the_table_aborts_when_its_count_disagrees_with_its_entries() {
        let env = Env::new();
        let mut t = Table::new(&env.meta, page_size(), Tracked::assume_new()).unwrap();
        t.insert(7, 0x10 as *mut HugeDesc);
        t.count = 0;
        t.remove(7);
    }
}

/// Kani check of `axiom_zero_entry` (record under `verus/seams/`).
#[cfg(kani)]
mod kani_table {
    use super::Entry;

    /// S166 `axiom_zero_entry`, its executable content: 16 zero bytes,
    /// aligned as an `Entry`, read as an `Entry` (a defined read: a
    /// `usize` and a raw pointer accept every bit pattern), and that
    /// entry is empty (`!occupied`: its `desc` has address 0). The
    /// entry is 16 bytes (`z.len() == 16`), the size the `global layout`
    /// line states.
    #[kani::proof]
    fn zero_entry_is_empty() {
        assert_eq!(core::mem::size_of::<Entry>(), 16);
        let z = [0u64; 2];
        assert!(core::mem::align_of::<Entry>() <= core::mem::align_of::<[u64; 2]>());
        // SAFETY: 16 initialised bytes, aligned for `Entry`, whose fields
        // accept every bit pattern.
        let e: Entry = unsafe { core::ptr::read(z.as_ptr().cast::<Entry>()) };
        assert_eq!(e.desc as usize, 0);
        assert_eq!(e.key, 0);
    }
}
