//! The ghost model of a heap's caches: [`CachePerms`]'s lemmas over the
//! headers, stacks, slot tokens and released log, and the stack and entry
//! predicates they are stated over. Ghost code only; `heap` re-exports it.

use super::*;
use vstd::prelude::*;

verus! {

/// `out[0]` is the token of slot `p` of class `c`, when `ok` (the caller's signed-entry
/// flag agrees with [`pac_mode`]; otherwise `p` is not the slot's address).
pub open spec fn slot_token(out: Map<int, GhostSingleton<int>>, c: int, ok: bool, p: *mut u8) -> bool {
    &&& out.contains_key(0)
    &&& out[0].id() == slot_loc(c)
    &&& ok ==> out[0]@ == p@.addr as int
}

/// Hand out slot `p` of class `c`: its slot token, `out[0]`
/// ([`slot_token`]), becomes the block's [`Live`] token in `blk`.
pub proof fn hand_out(tracked out: &mut Map<int, GhostSingleton<int>>, tracked blk: &mut Live, c: int, p: *mut u8)
    requires
        regions_ok(),
        slot_token(*old(out), c, true, p),
        process_regions().is_slot(c, p@.addr),
    ensures
        final(blk).slot_block(p, c),
{
    let tracked t = out.tracked_remove(0);
    *blk = Live::from_slot(t, c, p);
}

/// A pop from `cache` that took the top entry's slot token: `new` keeps
/// the tokens of the others ([`CachePerms::tokened`]), and `out[0]` is the
/// popped slot's token, of the class's authority, for the slot `p` when
/// `ok` (the pop authenticated exactly when entries are signed).
pub(super) open spec fn popped(old: CachePerms, new: CachePerms, out: Map<int, GhostSingleton<int>>, cache: *mut CacheHeader, ok: bool, p: *mut u8) -> bool {
    &&& new.tokened()
    &&& out.contains_key(0)
    &&& out[0].id() == slot_loc(old.cls[cache])
    &&& ok ==> out[0]@ == p@.addr as int
}

/// The address cache entry `e` of the stack at `base` stands for: `e`
/// itself, or with [`pac_mode`] what `pac::auth` returns for it with the
/// stack base as the modifier.
pub open spec fn plain(e: *mut u8, base: *mut *mut u8) -> usize {
    if pac_mode() { crate::sys::pac::spec_auth(e@.addr, base@.addr) } else { e@.addr }
}

/// Cache entry `e` of the stack at `base` holds a slot start of class `c`
/// the way a push stores it: the address it stands for ([`plain`]) is a
/// slot of `c`, and `e` is that address [`stored`] (signed with
/// [`pac_mode`], so `pac::auth` accepts it).
pub open spec fn slot_entry(c: int, e: *mut u8, base: *mut *mut u8) -> bool {
    &&& process_regions().is_slot(c, plain(e, base))
    &&& e@.addr == stored(plain(e, base), base)
}

/// A slot start stored in the stack at `base` the way a push stores it
/// (signed with [`pac_mode`]) stands for itself.
pub proof fn lemma_plain_stored(e: *mut u8, p: *mut u8, c: int, base: *mut *mut u8)
    requires
        regions_ok(),
        process_regions().is_slot(c, p@.addr),
        e@.addr == stored(p@.addr, base),
    ensures
        plain(e, base) == p@.addr,
        slot_entry(c, e, base),
{
    process_regions().lemma_slot_below_user_va(c, p@.addr);
    crate::sys::pac::axiom_auth_sign(p@.addr, base@.addr);
}

/// With [`pac_mode`], an entry that holds a slot the way a push stores it
/// ([`slot_entry`]) is signed with the stack base, so `pac::auth` takes no
/// integrity abort on it.
pub proof fn lemma_entry_signed(c: int, e: *mut u8, base: *mut *mut u8)
    requires
        regions_ok(),
        pac_mode(),
        slot_entry(c, e, base),
    ensures
        crate::sys::pac::signed(e@.addr, base@.addr),
{
    process_regions().lemma_slot_below_user_va(c, plain(e, base));
    assert(e@.addr == crate::sys::pac::spec_sign(plain(e, base), base@.addr));
}

/// The header for `class` in `others` node `node` ([`Heap::node_cache_ptr`]):
/// `class` lines past the node's first, which holds `arena`, `pad` and
/// `next` (`ArenaCaches::caches` is at offset 64), with the node's
/// provenance.
pub open spec fn node_hdr(node: *mut ArenaCaches, class: int) -> *mut CacheHeader {
    vstd::raw_ptr::ptr_mut_from_data::<CacheHeader>(
        vstd::raw_ptr::PtrData { addr: (node@.addr + 64 + 64 * class) as usize, provenance: node@.provenance, metadata: () },
    )
}

/// Node `node`'s first line and its headers lie inside the block it was
/// carved from ([`crate::vspec::ptr::in_alloc`]), so
/// [`Heap::node_cache_ptr`]'s offsets are in bounds and do not wrap.
pub open spec fn node_placed(node: *mut ArenaCaches) -> bool {
    &&& crate::vspec::ptr::in_alloc(node as *mut u8, 64)
    &&& crate::vspec::ptr::in_alloc(node_hdr(node, 0), 64 * NCLASSES)
    &&& node@.addr + 64 + 64 * NCLASSES <= usize::MAX
}

/// The `others` list `new` is `old` with nodes added at the head: each
/// node keeps its place counted from the end.
pub open spec fn list_grows<T>(new: Seq<T>, old: Seq<T>) -> bool {
    &&& old.len() <= new.len()
    &&& forall|r: int| 1 <= r <= old.len() ==> new[new.len() - r] == #[trigger] old[old.len() - r]
}

/// The slots at addresses `a` of class `c`, as `(class, address)` pairs:
/// what [`CachePerms::released`] records.
pub open spec fn tagged(c: int, a: Seq<int>) -> Seq<(int, int)> {
    Seq::new(a.len(), |i: int| (c, a[i]))
}

/// The released log `new` is `old` with entries appended, and the entries
/// appended are the set `s`.
pub open spec fn log_adds(new: Seq<(int, int)>, old: Seq<(int, int)>, s: Set<(int, int)>) -> bool {
    &&& old.len() <= new.len()
    &&& new.subrange(0, old.len() as int) == old
    &&& new.subrange(old.len() as int, new.len() as int).to_set() == s
}

/// An unchanged log has nothing appended.
pub(super) proof fn lemma_log_same(a: Seq<(int, int)>)
    ensures
        log_adds(a, a, Set::empty()),
{
    assert(a.subrange(0, a.len() as int) =~= a);
    assert(a.subrange(a.len() as int, a.len() as int).to_set() =~= Set::empty());
}

/// Appending `t` adds its entries.
pub(super) proof fn lemma_log_append(a: Seq<(int, int)>, t: Seq<(int, int)>)
    ensures
        log_adds(a + t, a, t.to_set()),
{
    assert((a + t).subrange(0, a.len() as int) =~= a);
    assert((a + t).subrange(a.len() as int, (a + t).len() as int) =~= t);
}

/// Two appends add the union of what each added.
pub(super) proof fn lemma_log_trans(a: Seq<(int, int)>, b: Seq<(int, int)>, c: Seq<(int, int)>, s1: Set<(int, int)>, s2: Set<(int, int)>)
    requires
        log_adds(b, a, s1),
        log_adds(c, b, s2),
    ensures
        log_adds(c, a, s1 + s2),
{
    let x = b.subrange(a.len() as int, b.len() as int);
    let y = c.subrange(b.len() as int, c.len() as int);
    assert forall|i: int| 0 <= i < b.len() implies c[i] == b[i] by {
        assert(c.subrange(0, b.len() as int)[i] == c[i]);
    }
    assert(c.subrange(0, a.len() as int) =~= a) by {
        assert forall|i: int| 0 <= i < a.len() implies c.subrange(0, a.len() as int)[i] == a[i] by {
            assert(b.subrange(0, a.len() as int)[i] == b[i]);
        }
    }
    assert(c.subrange(a.len() as int, c.len() as int) =~= x + y);
    vstd::seq_lib::seq_to_set_distributes_over_add(x, y);
}

/// One flush's split: the addresses a cache
/// held (`a`) are those it keeps (`k`) and those it released (`r`), and
/// none is both. Opaque, so a proof that uses it carries one
/// fact rather than two set identities.
#[verifier::opaque]
pub(super) open spec fn split_ok(a: Seq<int>, k: Seq<int>, r: Seq<int>) -> bool {
    &&& aset(a, 0, a.len() as int) == aset(k, 0, k.len() as int) + aset(r, 0, r.len() as int)
    &&& aset(k, 0, k.len() as int).disjoint(aset(r, 0, r.len() as int))
}

/// A flush's sets of addresses lifted to `(class, address)` pairs: the
/// slots cached before (`a`) are those kept (`k`) and those released
/// (`r`), and none is both, so the released pairs are the cached ones the
/// cache no longer holds.
pub(super) proof fn lemma_tagged_split(c: int, a: Seq<int>, k: Seq<int>, r: Seq<int>)
    requires
        split_ok(a, k, r),
    ensures
        tagged(c, k).to_set() <= tagged(c, a).to_set(),
        tagged(c, a).to_set() - tagged(c, k).to_set() == tagged(c, r).to_set(),
{
    broadcast use crate::class::lemma_aset;
    reveal(split_ok);
    assert forall|e: (int, int)| #[trigger] tagged(c, k).to_set().contains(e) implies tagged(c, a).to_set().contains(e) by {
        let m = choose|m: int| 0 <= m < k.len() && tagged(c, k)[m] == e;
        assert(aset(k, 0, k.len() as int).contains(k[m]));
        assert(aset(a, 0, a.len() as int).contains(k[m]));
        let i = choose|i: int| 0 <= i < a.len() && a[i] == k[m];
        assert(tagged(c, a)[i] == e);
    }
    assert forall|e: (int, int)| #[trigger] tagged(c, r).to_set().contains(e) implies tagged(c, a).to_set().contains(e) && !tagged(c, k).to_set().contains(e) by {
        let j = choose|j: int| 0 <= j < r.len() && tagged(c, r)[j] == e;
        assert(aset(r, 0, r.len() as int).contains(r[j]));
        assert(aset(a, 0, a.len() as int).contains(r[j]));
        let i = choose|i: int| 0 <= i < a.len() && a[i] == r[j];
        assert(tagged(c, a)[i] == e);
        if tagged(c, k).to_set().contains(e) {
            let m = choose|m: int| 0 <= m < k.len() && tagged(c, k)[m] == e;
            assert(aset(k, 0, k.len() as int).contains(k[m]));
        }
    }
    assert forall|e: (int, int)| #[trigger] tagged(c, a).to_set().contains(e) && !tagged(c, k).to_set().contains(e) implies tagged(c, r).to_set().contains(e) by {
        let i = choose|i: int| 0 <= i < a.len() && tagged(c, a)[i] == e;
        assert(aset(a, 0, a.len() as int).contains(a[i]));
        if aset(k, 0, k.len() as int).contains(a[i]) {
            let m = choose|m: int| 0 <= m < k.len() && k[m] == a[i];
            assert(tagged(c, k)[m] == e);
        } else {
            let j = choose|j: int| 0 <= j < r.len() && r[j] == a[i];
            assert(tagged(c, r)[j] == e);
        }
    }
    assert(tagged(c, a).to_set() - tagged(c, k).to_set() =~= tagged(c, r).to_set());
}

/// Entry `i` of the stack at `base` (`base.add(i)`).
pub(super) open spec fn slot_ptr(base: *mut *mut u8, i: int) -> *mut *mut u8 {
    crate::vspec::ptr::ptr_add(base, i as usize)
}

/// Entry `i` of the stack at `base` lies inside the stack's allocation,
/// so `base.add(i)` is in bounds.
pub(super) open spec fn slot_in(base: *mut *mut u8, i: int) -> bool {
    crate::vspec::ptr::in_alloc(base, i * vstd::layout::size_of::<*mut u8>())
}

/// The stack of header `h` is owned entry by entry: `cap` entries from
/// `slots`, the first `len` of them written, each entry inside the
/// stack's allocation ([`slot_in`]) and, when `cap` is not 0, its end
/// too (`slots.add(cap)` is in bounds).
pub(super) open spec fn stack_ok(h: CacheHeader, slots: Map<*mut *mut u8, PointsTo<*mut u8>>) -> bool {
    &&& forall|i: int|
        0 <= i < h.cap ==> {
            let q = #[trigger] slot_ptr(h.slots, i);
            &&& slots.contains_key(q)
            &&& slots[q].ptr() == q
            &&& (i < h.len ==> slots[q].is_init())
        }
    &&& forall|i: int| 0 <= i < h.cap ==> #[trigger] slot_in(h.slots, i)
    &&& h.cap != 0 ==> slot_in(h.slots, h.cap as int)
}

/// The stack of header `h` comes with its
/// [`MetaClaim`](crate::meta::MetaClaim) in `blocks`, by its base: the
/// claim to a block of `8 * cap` bytes aligned to 16
/// ([`CacheHeader::slots_layout`]) from the process's `Meta`
/// ([`process_live_id`](crate::meta::process_live_id)).
pub(super) open spec fn stack_claimed(h: CacheHeader, blocks: Map<*mut *mut u8, crate::meta::MetaClaim>) -> bool {
    &&& blocks.contains_key(h.slots)
    &&& blocks[h.slots].held(crate::meta::process_live_id(), h.slots as *mut u8, 8 * h.cap, 16)
}

/// `stacks` holds, for class `c`, the permission to the bytes of `h`'s
/// stack and its claim from the `Meta` whose live-object records are `id`
/// ([`Heap::free_caches`]).
pub(super) open spec fn stack_given(stacks: Map<int, (vstd::raw_ptr::PointsToRaw, crate::meta::MetaClaim)>, c: int, h: CacheHeader, id: vstd::resource::Loc) -> bool {
    &&& stacks.contains_key(c)
    &&& stacks[c].0.is_range(h.slots@.addr as int, 8 * h.cap)
    &&& stacks[c].0.provenance() == h.slots@.provenance
    &&& stacks[c].1.held(id, h.slots as *mut u8, 8 * h.cap, 16)
}

/// Header `p` is owned through `hp`, satisfies [`CacheHeader::inv`],
/// owns its stack, and it and its stack are metadata.
pub(super) open spec fn cell_ok(p: *mut CacheHeader, hp: PointsTo<CacheHeader>, slots: Map<*mut *mut u8, PointsTo<*mut u8>>) -> bool {
    &&& hp.ptr() == p
    &&& hp.is_init()
    &&& hp.value().inv()
    &&& stack_ok(hp.value(), slots)
    &&& cell_in_meta(p, hp.value())
}

/// Header `p`, holding `h`, and once it has one its stack lie in the
/// metadata region.
pub(super) open spec fn cell_in_meta(p: *mut CacheHeader, h: CacheHeader) -> bool {
    &&& process_regions().in_meta(p@.addr as int, 64)
    &&& h.cap != 0 ==> process_regions().in_meta(h.slots@.addr as int, 8 * h.cap)
}

/// `new` keeps every stack entry `old` owns, at the same address, and
/// every written one written, and files each entry it adds under the
/// entry's own address.
pub(super) open spec fn slots_grow(old: Map<*mut *mut u8, PointsTo<*mut u8>>, new: Map<*mut *mut u8, PointsTo<*mut u8>>) -> bool {
    &&& forall|q: *mut *mut u8|
        #[trigger] old.contains_key(q) ==> {
            &&& new.contains_key(q)
            &&& new[q].ptr() == old[q].ptr()
            &&& (old[q].is_init() ==> new[q].is_init())
        }
    &&& forall|q: *mut *mut u8| #[trigger] new.contains_key(q) && !old.contains_key(q) ==> new[q].ptr() == q
}

/// A header whose stack keeps its base and capacity and loses entries
/// from the top still owns its stack.
pub(super) proof fn lemma_stack_shrink(old_h: CacheHeader, new_h: CacheHeader, slots: Map<*mut *mut u8, PointsTo<*mut u8>>)
    requires
        stack_ok(old_h, slots),
        new_h.slots == old_h.slots,
        new_h.cap == old_h.cap,
        new_h.len <= old_h.len,
    ensures
        stack_ok(new_h, slots),
{
    assert forall|i: int| 0 <= i < new_h.cap implies #[trigger] slot_in(new_h.slots, i) by {}
    assert forall|i: int| 0 <= i < new_h.cap implies {
        let q = #[trigger] slot_ptr(new_h.slots, i);
        &&& slots.contains_key(q)
        &&& slots[q].ptr() == q
        &&& (i < new_h.len ==> slots[q].is_init())
    } by {
        assert(slot_ptr(old_h.slots, i) == slot_ptr(new_h.slots, i));
    }
}

/// A stack stays owned when its entries' permissions only grow.
pub(super) proof fn lemma_stack_grow(h: CacheHeader, old_s: Map<*mut *mut u8, PointsTo<*mut u8>>, new_s: Map<*mut *mut u8, PointsTo<*mut u8>>)
    requires
        stack_ok(h, old_s),
        slots_grow(old_s, new_s),
    ensures
        stack_ok(h, new_s),
{
    assert forall|i: int| 0 <= i < h.cap implies #[trigger] slot_in(h.slots, i) by {}
    assert forall|i: int| 0 <= i < h.cap implies {
        let q = #[trigger] slot_ptr(h.slots, i);
        &&& new_s.contains_key(q)
        &&& new_s[q].ptr() == q
        &&& (i < h.len ==> new_s[q].is_init())
    } by {
        assert(old_s.contains_key(slot_ptr(h.slots, i)));
    }
}

/// A push: the entry at the old length written, the length one more.
pub(super) proof fn lemma_stack_push(
    old_h: CacheHeader,
    new_h: CacheHeader,
    old_s: Map<*mut *mut u8, PointsTo<*mut u8>>,
    new_s: Map<*mut *mut u8, PointsTo<*mut u8>>,
)
    requires
        stack_ok(old_h, old_s),
        slots_grow(old_s, new_s),
        new_h.slots == old_h.slots,
        new_h.cap == old_h.cap,
        new_h.len == old_h.len + 1,
        new_s[slot_ptr(old_h.slots, old_h.len as int)].is_init(),
    ensures
        stack_ok(new_h, new_s),
{
    assert forall|i: int| 0 <= i < new_h.cap implies #[trigger] slot_in(new_h.slots, i) by {}
    assert forall|i: int| 0 <= i < new_h.cap implies {
        let q = #[trigger] slot_ptr(new_h.slots, i);
        &&& new_s.contains_key(q)
        &&& new_s[q].ptr() == q
        &&& (i < new_h.len ==> new_s[q].is_init())
    } by {
        assert(old_s.contains_key(slot_ptr(old_h.slots, i)));
    }
}

/// A stack whose entries `start..end` were all written (a claim into a
/// slice over them, [`stack_mut`]) and whose length grew to at most `end`
/// from at least `start` still owns its stack.
pub(super) proof fn lemma_stack_fill(
    old_h: CacheHeader,
    new_h: CacheHeader,
    old_s: Map<*mut *mut u8, PointsTo<*mut u8>>,
    new_s: Map<*mut *mut u8, PointsTo<*mut u8>>,
    start: int,
    end: int,
)
    requires
        stack_ok(old_h, old_s),
        slots_grow(old_s, new_s),
        new_h.slots == old_h.slots,
        new_h.cap == old_h.cap,
        0 <= start <= old_h.len,
        new_h.len <= end,
        forall|i: int| start <= i < end ==> #[trigger] new_s[slot_ptr(old_h.slots, i)].is_init(),
    ensures
        stack_ok(new_h, new_s),
{
    assert forall|i: int| 0 <= i < new_h.cap implies #[trigger] slot_in(new_h.slots, i) by {}
    assert forall|i: int| 0 <= i < new_h.cap implies {
        let q = #[trigger] slot_ptr(new_h.slots, i);
        &&& new_s.contains_key(q)
        &&& new_s[q].ptr() == q
        &&& (i < new_h.len ==> new_s[q].is_init())
    } by {
        assert(old_s.contains_key(slot_ptr(old_h.slots, i)));
        if i < start {
            assert(old_s[slot_ptr(old_h.slots, i)].is_init());
        }
    }
}

/// What a push stores for the slot start `p` in the stack at `base`: `p`,
/// or with [`pac_mode`] `pac::sign` of it with the stack base as the
/// modifier.
pub open spec fn stored(p: usize, base: *mut *mut u8) -> usize {
    if pac_mode() { crate::sys::pac::spec_sign(p, base@.addr) } else { p }
}

/// Stack entry `q` is one of entries `k..n` of the stack at `slots`.
pub(super) open spec fn in_entries(slots: *mut *mut u8, q: *mut *mut u8, k: int, n: int) -> bool {
    exists|i: int| k <= i < n && q == slot_ptr(slots, i)
}

/// Every stack entry but entries `0..n` of the stack at `slots` keeps its
/// token.
pub(super) open spec fn toks_kept(new: Map<*mut *mut u8, GhostSingleton<int>>, old: Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, n: int) -> bool {
    forall|q: *mut *mut u8| #![trigger new.contains_key(q)] #![trigger old.contains_key(q)] !in_entries(slots, q, 0, n) ==> {
        &&& new.contains_key(q) == old.contains_key(q)
        &&& old.contains_key(q) ==> new[q] == old[q]
    }
}

/// The entries of header `h`'s stack: `cap` words from `slots`.
pub(super) open spec fn stack_set(h: CacheHeader) -> Set<*mut *mut u8> {
    vstd::set_lib::set_int_range(0, h.cap as int).map(|i: int| slot_ptr(h.slots, i))
}

/// What [`stack_set`] contains.
pub(super) proof fn lemma_stack_set(h: CacheHeader, q: *mut *mut u8)
    ensures
        #[trigger] stack_set(h).contains(q) <==> exists|i: int| 0 <= i < h.cap && slot_ptr(h.slots, i) == q,
{
    broadcast use {vstd::set_lib::range_set_properties, Set::lemma_map_contains};
    if stack_set(h).contains(q) {
        let i = choose|i: int| vstd::set_lib::set_int_range(0, h.cap as int).contains(i) && q == slot_ptr(h.slots, i);
        assert(0 <= i < h.cap);
    }
    if exists|i: int| 0 <= i < h.cap && slot_ptr(h.slots, i) == q {
        let i = choose|i: int| 0 <= i < h.cap && slot_ptr(h.slots, i) == q;
        assert(vstd::set_lib::set_int_range(0, h.cap as int).contains(i));
    }
}

/// `gone` holds the permission to entry `i` of the stack at `base`, which
/// lies inside the stack's allocation ([`join_entries`]).
pub(super) open spec fn entry_given(gone: Map<*mut *mut u8, PointsTo<*mut u8>>, base: *mut *mut u8, i: int) -> bool {
    let q = slot_ptr(base, i);
    &&& gone.contains_key(q)
    &&& gone[q].ptr() == q
    &&& slot_in(base, i)
}

/// The permission to the first `n` entries of the stack at `base` (`8 *
/// n` bytes), joined from the entries' permissions in `gone` with their
/// contents dropped.
pub(super) proof fn join_entries(tracked gone: Map<*mut *mut u8, PointsTo<*mut u8>>, base: *mut *mut u8, n: int) -> (tracked r: vstd::raw_ptr::PointsToRaw)
    requires
        0 <= n,
        base@.addr + 8 * n <= usize::MAX,
        forall|i: int| 0 <= i < n ==> #[trigger] entry_given(gone, base, i),
    ensures
        r.is_range(base@.addr as int, 8 * n),
        r.provenance() == base@.provenance,
    decreases n,
{
    broadcast use vstd::layout::layout_of_references_and_pointers_for_sized_types;

    if n == 0 {
        let tracked r = vstd::raw_ptr::PointsToRaw::empty(base@.provenance);
        assert(r.dom() =~= Set::range(base@.addr as int, base@.addr as int));
        r
    } else {
        let q = slot_ptr(base, n - 1);
        assert(entry_given(gone, base, n - 1));
        assert(((n - 1) as usize) as int == n - 1);
        assert(vstd::layout::size_of::<*mut u8>() == 8);
        assert(base@.addr + (n - 1) * 8 <= usize::MAX);
        crate::vspec::ptr::axiom_ptr_add_addr(base, (n - 1) as usize);
        let tracked mut rest = gone;
        let tracked mut e = rest.tracked_remove(q);
        assert forall|i: int| 0 <= i < n - 1 implies #[trigger] entry_given(rest, base, i) by {
            assert(entry_given(gone, base, i));
            assert((i as usize) as int == i);
            assert(base@.addr + i * 8 <= usize::MAX);
            crate::vspec::ptr::axiom_ptr_add_addr(base, i as usize);
            assert(slot_ptr(base, i)@.addr != q@.addr);
        }
        let tracked r0 = join_entries(rest, base, n - 1);
        e.leak_contents();
        let tracked er = e.into_raw();
        let tracked r = r0.join(er);
        assert(r.dom() =~= Set::range(base@.addr as int, base@.addr + 8 * n));
        r
    }
}

/// The `owner` of a stack entry after headers `a` and `b` trade values
/// ([`Heap::swap_hdrs`]): an entry of `a`'s stack is now `b`'s at the same
/// index, and the other way round.
pub(super) open spec fn swap_owner(
    owner: Map<*mut *mut u8, (*mut CacheHeader, vstd::prelude::int)>,
    a: *mut CacheHeader,
    b: *mut CacheHeader,
) -> Map<*mut *mut u8, (*mut CacheHeader, vstd::prelude::int)> {
    Map::new(
        owner.dom(),
        |q: *mut *mut u8|
            if owner[q].0 == a {
                (b, owner[q].1)
            } else if owner[q].0 == b {
                (a, owner[q].1)
            } else {
                owner[q]
            },
    )
}

/// Entries `lo..cap` of the stack at `base` are no keys of `s` when `m`
/// holds their permissions (`elems_at`) and every such key of `s` would
/// hold a permission to its own address: two permissions to one word
/// are disjoint ([`PointsTo::is_disjoint`]), which they cannot be.
pub(super) proof fn lemma_keys_fresh(
    tracked m: &mut Map<int, PointsTo<*mut u8>>,
    tracked s: &Map<*mut *mut u8, PointsTo<*mut u8>>,
    base: *mut *mut u8,
    lo: int,
    cap: int,
)
    requires
        0 <= lo <= cap,
        crate::arena::elems_at(*old(m), base, lo, cap),
        forall|i: int| lo <= i < cap && #[trigger] s.contains_key(slot_ptr(base, i)) ==> s[slot_ptr(base, i)].ptr() == slot_ptr(base, i),
    ensures
        *final(m) == *old(m),
        forall|i: int| lo <= i < cap ==> !s.contains_key(#[trigger] slot_ptr(base, i)),
    decreases cap - lo,
{
    if lo < cap {
        let q = slot_ptr(base, lo);
        if s.contains_key(q) {
            assert(vstd::layout::size_of::<*mut u8>() == 8);
            assert(m.contains_key(lo));
            let tracked mut e = m.tracked_remove(lo);
            e.is_disjoint(s.tracked_borrow(q));
            m.tracked_insert(lo, e);
            assert(false);
        }
        lemma_keys_fresh(m, s, base, lo + 1, cap);
    }
}

/// Entries `lo..n` of the stack at `base`, whose permissions `m` holds
/// (`elems_at`), join `perms` as entries of header `hdr`. None is a key
/// of `perms` yet, and their addresses are distinct, so every key
/// `perms` had keeps its permission and its owner.
pub(super) proof fn lemma_put_entries(
    tracked perms: &mut CachePerms,
    tracked m: Map<int, PointsTo<*mut u8>>,
    hdr: *mut CacheHeader,
    base: *mut *mut u8,
    lo: int,
    n: int,
)
    requires
        0 <= lo <= n,
        crate::arena::elems_at(m, base, lo, n),
        forall|i: int| lo <= i < n ==> !old(perms).slots.contains_key(#[trigger] slot_ptr(base, i)),
        forall|i: int| 0 <= i < n ==> #[trigger] slot_ptr(base, i)@.addr == base@.addr + 8 * i,
    ensures
        final(perms).hdrs == old(perms).hdrs,
        final(perms).cls == old(perms).cls,
        final(perms).toks == old(perms).toks,
        final(perms).zero == old(perms).zero,
        final(perms).handed == old(perms).handed,
        final(perms).blocks == old(perms).blocks,
        final(perms).released == old(perms).released,
        forall|q: *mut *mut u8| #[trigger] old(perms).slots.contains_key(q) ==> final(perms).slots.contains_key(q) && final(perms).slots[q] == old(perms).slots[q],
        forall|q: *mut *mut u8| #[trigger] final(perms).slots.contains_key(q) && !old(perms).slots.contains_key(q) ==> final(perms).slots[q].ptr() == q,
        forall|q: *mut *mut u8|
            old(perms).slots.contains_key(q) && #[trigger] old(perms).owner.contains_key(q) ==> final(perms).owner.contains_key(q) && final(perms).owner[q] == old(perms).owner[q],
        forall|i: int| lo <= i < n ==> {
            let q = #[trigger] slot_ptr(base, i);
            &&& final(perms).slots.contains_key(q)
            &&& final(perms).slots[q].ptr() == q
            &&& final(perms).owner.contains_key(q)
            &&& final(perms).owner[q] == (hdr, i)
        },
    decreases n - lo,
{
    if lo < n {
        let q = slot_ptr(base, lo);
        let tracked mut m = m;
        assert(m.contains_key(lo));
        let tracked e = m.tracked_remove(lo);
        let p0 = *perms;
        perms.slots.tracked_insert(q, e);
        perms.owner = perms.owner.insert(q, (hdr, lo));
        assert forall|i: int| lo + 1 <= i < n implies !perms.slots.contains_key(#[trigger] slot_ptr(base, i)) by {
            assert(slot_ptr(base, i)@.addr != q@.addr);
            assert(!p0.slots.contains_key(slot_ptr(base, i)));
        }
        assert forall|i: int| #![trigger m.contains_key(i)] #![trigger m[i]] lo + 1 <= i < n implies m.contains_key(i) && m[i].ptr() == crate::vspec::ptr::ptr_add(base, i as usize) by {}
        let p1 = *perms;
        assert forall|q2: *mut *mut u8| #[trigger] p0.slots.contains_key(q2) implies p1.slots.contains_key(q2) && p1.slots[q2] == p0.slots[q2] by {
            assert(q2 != q);
        }
        assert forall|q2: *mut *mut u8| p0.slots.contains_key(q2) && #[trigger] p0.owner.contains_key(q2) implies p1.owner.contains_key(q2) && p1.owner[q2] == p0.owner[q2] by {
            assert(q2 != q);
        }
        lemma_put_entries(perms, m, hdr, base, lo + 1, n);
        assert(p1.slots.contains_key(q) && p1.owner.contains_key(q));
        assert(perms.slots[q] == p1.slots[q] && perms.owner[q] == (hdr, lo));
        assert(p1.slots[q].ptr() == q);
        assert forall|q2: *mut *mut u8| #[trigger] perms.slots.contains_key(q2) && !p0.slots.contains_key(q2) implies perms.slots[q2].ptr() == q2 by {
            if p1.slots.contains_key(q2) {
                assert(q2 == q);
            }
        }
        assert forall|q2: *mut *mut u8| #[trigger] p0.slots.contains_key(q2) implies perms.slots.contains_key(q2) && perms.slots[q2] == p0.slots[q2] by {
            assert(p1.slots.contains_key(q2));
        }
        assert forall|q2: *mut *mut u8| p0.slots.contains_key(q2) && #[trigger] p0.owner.contains_key(q2) implies perms.owner.contains_key(q2) && perms.owner[q2] == p0.owner[q2] by {
            assert(p1.slots.contains_key(q2));
            assert(p1.owner.contains_key(q2));
        }
    }
}

impl CachePerms {
    /// Every owned header is [`cell_ok`], and every stack-entry permission
    /// is filed under its own address.
    pub(super) open spec fn wf(&self) -> bool {
        &&& forall|p: *mut CacheHeader| #[trigger] self.hdrs.contains_key(p) ==> cell_ok(p, self.hdrs[p], self.slots)
        &&& forall|q: *mut *mut u8| #[trigger] self.slots.contains_key(q) ==> self.slots[q].ptr() == q
    }

    /// `self` is well formed and owns every header `old` owned, and every
    /// header that had a stack still has the same one.
    pub(super) open spec fn keeps(&self, old: CachePerms) -> bool {
        &&& self.wf()
        &&& forall|p: *mut CacheHeader| #[trigger] old.hdrs.contains_key(p) ==> self.hdrs.contains_key(p)
        &&& forall|p: *mut CacheHeader| #[trigger] old.hdrs.contains_key(p) && old.hdr(p).cap != 0 ==> {
            &&& self.hdr(p).cap == old.hdr(p).cap
            &&& self.hdr(p).slots == old.hdr(p).slots
        }
    }

    /// Every owned header with a stack holds the stack's claim
    /// ([`stack_claimed`]).
    #[verifier::opaque]
    pub(super) open spec fn claimed(&self) -> bool {
        forall|p: *mut CacheHeader| #[trigger] self.hdrs.contains_key(p) && self.hdr(p).cap != 0 ==> stack_claimed(self.hdr(p), self.blocks)
    }

    /// Header `p` of `new` is one `old` owned, with the same stack
    /// ([`CachePerms::lemma_claimed_frame`]).
    pub(super) open spec fn same_stack(old: CachePerms, new: CachePerms, p: *mut CacheHeader) -> bool {
        &&& old.hdrs.contains_key(p)
        &&& old.hdr(p).cap == new.hdr(p).cap
        &&& old.hdr(p).slots == new.hdr(p).slots
    }

    /// A change that keeps the claims, and leaves every owned header with
    /// a stack an owned header of `old` with the same stack, keeps
    /// [`CachePerms::claimed`].
    pub(super) proof fn lemma_claimed_frame(old: CachePerms, new: CachePerms)
        requires
            old.claimed(),
            new.blocks == old.blocks,
            forall|p: *mut CacheHeader| #[trigger] new.hdrs.contains_key(p) && new.hdr(p).cap != 0 ==> CachePerms::same_stack(old, new, p),
        ensures
            new.claimed(),
    {
        reveal(CachePerms::claimed);
        assert forall|q: *mut CacheHeader| #[trigger] new.hdrs.contains_key(q) && new.hdr(q).cap != 0 implies stack_claimed(new.hdr(q), new.blocks) by {
            assert(stack_claimed(old.hdr(q), old.blocks));
        }
    }

    /// A change to header `p` alone that keeps the claims and leaves `p`
    /// with no stack or with its old one keeps [`CachePerms::claimed`].
    pub(super) proof fn lemma_claimed_hdr(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.claimed(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.blocks == old.blocks,
            new.hdr(p).cap == 0 || (new.hdr(p).cap == old.hdr(p).cap && new.hdr(p).slots == old.hdr(p).slots),
        ensures
            new.claimed(),
    {
        reveal(CachePerms::claimed);
        assert forall|q: *mut CacheHeader| #[trigger] new.hdrs.contains_key(q) && new.hdr(q).cap != 0 implies stack_claimed(new.hdr(q), new.blocks) by {
            assert(old.hdrs.contains_key(q));
            if q == p {
                assert(stack_claimed(old.hdr(p), old.blocks));
            } else {
                assert(new.hdr(q) == old.hdr(q));
            }
        }
    }

    /// A change to header `p` alone, which had no stack and now has one
    /// whose claim `blocks` holds, keeps [`CachePerms::claimed`].
    pub(super) proof fn lemma_claimed_new(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.claimed(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.blocks == old.blocks,
            stack_claimed(new.hdr(p), new.blocks),
        ensures
            new.claimed(),
    {
        reveal(CachePerms::claimed);
        assert forall|q: *mut CacheHeader| #[trigger] new.hdrs.contains_key(q) && new.hdr(q).cap != 0 implies stack_claimed(new.hdr(q), new.blocks) by {
            if q != p {
                assert(old.hdrs.contains_key(q));
                assert(new.hdr(q) == old.hdr(q));
            }
        }
    }

    /// Add `mc`, the claim of a new stack of `cap` entries at `base`,
    /// keyed by `base`. No owned stack is at `base` already: its claim
    /// would be a second record of the same object
    /// ([`MetaClaim::lemma_apart`](crate::meta::MetaClaim::lemma_apart)).
    pub(super) proof fn tracked_put_claim(tracked &mut self, base: *mut *mut u8, cap: int, tracked mc: crate::meta::MetaClaim)
        requires
            old(self).claimed(),
            mc.held(crate::meta::process_live_id(), base as *mut u8, 8 * cap, 16),
        ensures
            *final(self) == (CachePerms { blocks: final(self).blocks, ..*old(self) }),
            final(self).claimed(),
            final(self).blocks.contains_key(base),
            final(self).blocks[base].held(crate::meta::process_live_id(), base as *mut u8, 8 * cap, 16),
    {
        reveal(CachePerms::claimed);
        let tracked mut mc = mc;
        let ghost id = crate::meta::process_live_id();
        if self.blocks.contains_key(base) {
            mc.lemma_apart(self.blocks.tracked_borrow(base), id, base as *mut u8, 8 * cap, 16);
        }
        let ghost before = *self;
        self.blocks.tracked_insert(base, mc);
        assert forall|p: *mut CacheHeader| #[trigger] self.hdrs.contains_key(p) && self.hdr(p).cap != 0 implies stack_claimed(self.hdr(p), self.blocks) by {
            assert(stack_claimed(before.hdr(p), before.blocks));
            let h = before.hdr(p);
            if h.slots == base {
                assert(before.blocks[base].held(id, h.slots as *mut u8, 8 * h.cap, 16));
            }
        }
    }

    /// The `cap` entries of the fresh stack `slots` join `self`, unwritten,
    /// as entries `0..cap` of header `hdr` (`CachePerms::owner`), each
    /// with the permission to its word carved from `raw`, the permission
    /// to the block's `8 * cap` bytes that `Meta::alloc_raw` handed out
    /// with it ([`CacheHeader::slots_layout`]).
    ///
    /// No entry is a key `self` already has ([`lemma_keys_fresh`]): `wf`
    /// files every key under its own address, so such a key would hold a
    /// second permission to a word of `raw`. Every other key keeps its
    /// permission, and every owned stack's entries keep their owners,
    /// since they are keys `self` already has.
    pub(super) proof fn lemma_fresh_stack(
        tracked &mut self,
        hdr: *mut CacheHeader,
        slots: *mut *mut u8,
        cap: u32,
        tracked raw: vstd::raw_ptr::PointsToRaw,
    )
        requires
            slots@.addr != 0,
            slots@.addr as int % 8 == 0,
            slots@.addr + 8 * cap <= usize::MAX,
            crate::vspec::ptr::in_alloc(slots, 8 * cap),
            old(self).wf(),
            raw.is_range(slots@.addr as int, 8 * cap),
            raw.provenance() == slots@.provenance,
        ensures
            final(self).hdrs == old(self).hdrs,
            final(self).cls == old(self).cls,
            slots_grow(old(self).slots, final(self).slots),
            forall|q: *mut *mut u8| #[trigger] old(self).slots.contains_key(q) ==> final(self).slots[q] == old(self).slots[q],
            final(self).toks == old(self).toks,
            final(self).zero == old(self).zero,
            final(self).handed == old(self).handed,
            final(self).blocks == old(self).blocks,
            final(self).released == old(self).released,
            forall|i: int| 0 <= i < cap ==> {
                let q = #[trigger] slot_ptr(slots, i);
                &&& final(self).slots.contains_key(q)
                &&& final(self).slots[q].ptr() == q
                &&& final(self).owner.contains_key(q)
                &&& final(self).owner[q] == (hdr, i)
            },
            forall|i: int| 0 <= i < cap ==> #[trigger] slot_in(slots, i),
            forall|p: *mut CacheHeader, j: int|
                old(self).hdrs.contains_key(p) && 0 <= j < old(self).hdr(p).cap && old(self).owner.contains_key(old(self).ent(p, j)) ==> {
                    let q = #[trigger] old(self).ent(p, j);
                    &&& final(self).owner.contains_key(q)
                    &&& final(self).owner[q] == old(self).owner[q]
                },
    {
        broadcast use vstd::layout::align_properties, vstd::raw_ptr::group_raw_ptr_axioms;

        let n = cap as int;
        assert(vstd::layout::size_of::<*mut u8>() == 8);
        // The word's alignment divides its size (8), so 8-aligned entries
        // are aligned.
        crate::layout::lemma_aligned_down(slots@.addr as int, 8, vstd::layout::align_of::<*mut u8>() as int);
        // Entry `i`'s address, for every entry: the entries are distinct.
        assert forall|i: int| 0 <= i < n implies #[trigger] slot_ptr(slots, i)@.addr == slots@.addr + 8 * i && slot_in(slots, i) by {
            crate::vspec::ptr::axiom_ptr_add_addr(slots, i as usize);
        }
        assert(0 * 8 == 0int);
        let tracked mut m = crate::arena::split_elems::<*mut u8>(raw, slots, 0, cap as nat);
        lemma_keys_fresh(&mut m, &self.slots, slots, 0, n);
        let old_self = *self;
        lemma_put_entries(self, m, hdr, slots, 0, n);
        assert forall|p: *mut CacheHeader, j: int|
            old_self.hdrs.contains_key(p) && 0 <= j < old_self.hdr(p).cap implies #[trigger] old_self.slots.contains_key(old_self.ent(p, j)) by {
            assert(cell_ok(p, old_self.hdrs[p], old_self.slots));
        }
    }

    /// Take out the claim of owned header `p`'s stack, to free the stack
    /// ([`Heap::free_caches_in`]). Every other owned header with a stack
    /// keeps its claim: its stack is at another base, since the stacks'
    /// first entries have one owner each ([`CachePerms::tracks`]).
    pub(super) proof fn tracked_take_claim(tracked &mut self, p: *mut CacheHeader) -> (tracked r: crate::meta::MetaClaim)
        requires
            old(self).wf(),
            old(self).tracks(),
            old(self).claimed(),
            old(self).hdrs.contains_key(p),
            old(self).hdr(p).cap != 0,
        ensures
            *final(self) == (CachePerms { blocks: final(self).blocks, ..*old(self) }),
            r.held(crate::meta::process_live_id(), old(self).hdr(p).slots as *mut u8, 8 * old(self).hdr(p).cap, 16),
            forall|q: *mut CacheHeader|
                #[trigger] old(self).hdrs.contains_key(q) && q != p && old(self).hdr(q).cap != 0 ==> stack_claimed(old(self).hdr(q), final(self).blocks),
    {
        reveal(CachePerms::claimed);
        let ghost before = *self;
        let base = before.hdr(p).slots;
        assert(stack_claimed(before.hdr(p), before.blocks));
        let tracked r = self.blocks.tracked_remove(base);
        assert forall|q: *mut CacheHeader|
            #[trigger] before.hdrs.contains_key(q) && q != p && before.hdr(q).cap != 0 implies stack_claimed(before.hdr(q), self.blocks) by {
            assert(stack_claimed(before.hdr(q), before.blocks));
            if before.hdr(q).slots == base {
                // Entry 0 of both stacks: `owner` gives it one header.
                assert(before.owner[before.ent(q, 0)] == (q, 0));
                assert(before.owner[before.ent(p, 0)] == (p, 0));
            }
        }
        r
    }

    /// The value of owned header `p`.
    pub(super) open spec fn hdr(&self, p: *mut CacheHeader) -> CacheHeader {
        self.hdrs[p].value()
    }

    /// Entry `i` of owned header `p`'s stack.
    pub(super) open spec fn ent(&self, p: *mut CacheHeader, i: int) -> *mut *mut u8 {
        slot_ptr(self.hdr(p).slots, i)
    }

    /// Stack entry `q` of the stack at `base` holds a slot start of class
    /// `c` in the process's regions, stored as a push stores it
    /// ([`slot_entry`]): the word itself, or with [`pac_mode`] the slot
    /// start signed with the stack base as the modifier, which `pac::auth`
    /// gives back ([`plain`]).
    pub(super) open spec fn holds(&self, c: int, q: *mut *mut u8, base: *mut *mut u8) -> bool {
        slot_entry(c, self.slots[q].value(), base)
    }

    /// What the caches hold: every entry of an owned header's stack is
    /// that header's alone (`owner` maps it to the header and its index,
    /// so no two stacks, and no two indices of one stack, share an
    /// entry), and each of the first `len` entries holds a slot start of
    /// the header's class (`cls`). What pops rely on and pushes keep.
    pub(super) open spec fn tracks(&self) -> bool {
        forall|p: *mut CacheHeader, i: int|
            #![trigger self.ent(p, i)]
            self.hdrs.contains_key(p) && 0 <= i < self.hdr(p).cap ==> {
                &&& self.owner.contains_key(self.ent(p, i))
                &&& self.owner[self.ent(p, i)] == (p, i)
                &&& (i < self.hdr(p).len ==> self.holds(self.cls[p], self.ent(p, i), self.hdr(p).slots))
            }
    }


    /// Entry `i` of owned header `p` holds its slot's token, of the
    /// authority of the header's class: the token of the address the entry
    /// stands for ([`plain`]).
    pub(super) open spec fn tok_at(&self, p: *mut CacheHeader, i: int) -> bool {
        let q = self.ent(p, i);
        &&& self.toks.contains_key(q)
        &&& self.toks[q].id() == slot_loc(self.cls[p])
        &&& self.toks[q]@ == plain(self.slots[q].value(), self.hdr(p).slots) as int
    }

    /// The slot tokens of what the caches hold: each of a header's first `len` entries
    /// holds the token of its slot ([`CachePerms::tok_at`]).
    pub(super) open spec fn tokened(&self) -> bool {
        forall|p: *mut CacheHeader, i: int|
            #![trigger self.ent(p, i)]
            self.hdrs.contains_key(p) && 0 <= i < self.hdr(p).len ==> self.tok_at(p, i)
    }

    /// An entry of a cached slot holds its token.
    pub(super) proof fn lemma_tok_get(&self, p: *mut CacheHeader, i: int)
        requires
            self.tokened(),
            self.hdrs.contains_key(p),
            0 <= i < self.hdr(p).len,
        ensures
            self.tok_at(p, i),
    {
        assert(self.tok_at(p, i));
    }

    /// A change confined to header `p`, its stack entries and their tokens
    /// keeps [`CachePerms::tokened`] when `p`'s own first `len` entries hold
    /// theirs: the other headers' entries are the stack entries `owner`
    /// maps to them ([`CachePerms::tracks`]).
    pub(super) proof fn lemma_tok_update(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.wf(),
            old.tracks(),
            old.tokened(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.cls == old.cls,
            forall|p2: *mut CacheHeader, j: int|
                old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap ==> {
                    let q = #[trigger] old.ent(p2, j);
                    &&& new.slots[q] == old.slots[q]
                    &&& (old.toks.contains_key(q) ==> new.toks.contains_key(q) && new.toks[q] == old.toks[q])
                },
            forall|i: int| 0 <= i < new.hdr(p).len ==> #[trigger] new.tok_at(p, i),
        ensures
            new.tokened(),
    {
        assert forall|p2: *mut CacheHeader, j: int|
            #![trigger new.ent(p2, j)]
            new.hdrs.contains_key(p2) && 0 <= j < new.hdr(p2).len implies new.tok_at(p2, j) by {
            if p2 != p {
                assert(old.hdrs.contains_key(p2));
                assert(new.hdr(p2) == old.hdr(p2));
                assert(cell_ok(p2, old.hdrs[p2], old.slots));
                assert(old.tok_at(p2, j));
                assert(new.ent(p2, j) == old.ent(p2, j));
            } else {
                assert(new.tok_at(p, j));
            }
        }
    }

    /// No other header's entry is entry `i` of header `p`'s stack.
    pub(super) proof fn lemma_ent_apart(&self, p: *mut CacheHeader, i: int, p2: *mut CacheHeader, j: int)
        requires
            self.wf(),
            self.tracks(),
            self.hdrs.contains_key(p),
            self.hdrs.contains_key(p2),
            0 <= i < self.hdr(p).cap,
            0 <= j < self.hdr(p2).cap,
            p2 != p || j != i,
        ensures
            self.ent(p, i) != self.ent(p2, j),
    {
        assert(self.owner[self.ent(p, i)] == (p, i));
        assert(self.owner[self.ent(p2, j)] == (p2, j));
    }

    /// A refill of header `p` (its first `n` entries written, with their
    /// tokens) keeps every other header's tokens.
    pub(super) proof fn lemma_tokened_refill(old: CachePerms, new: CachePerms, p: *mut CacheHeader, n: int)
        requires
            old.wf(),
            old.tracks(),
            old.tokened(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.cls == old.cls,
            new.owner == old.owner,
            new.hdr(p).slots == old.hdr(p).slots,
            new.hdr(p).cap == old.hdr(p).cap,
            new.hdr(p).len == n,
            0 <= n <= old.hdr(p).cap,
            forall|i: int| 0 <= i < n ==> #[trigger] new.tok_at(p, i),
            toks_kept(new.toks, old.toks, old.hdr(p).slots, n),
            forall|p2: *mut CacheHeader, j: int|
                old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap
                    ==> #[trigger] new.slots[old.ent(p2, j)] == old.slots[old.ent(p2, j)],
        ensures
            new.tokened(),
    {
        assert forall|p2: *mut CacheHeader, j: int|
            old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap implies {
                let q = #[trigger] old.ent(p2, j);
                &&& new.slots[q] == old.slots[q]
                &&& (old.toks.contains_key(q) ==> new.toks.contains_key(q) && new.toks[q] == old.toks[q])
            } by {
            let q = old.ent(p2, j);
            if in_entries(old.hdr(p).slots, q, 0, n) {
                let k = choose|k: int| 0 <= k < n && q == slot_ptr(old.hdr(p).slots, k);
                old.lemma_ent_apart(p, k, p2, j);
            }
        }
        CachePerms::lemma_tok_update(old, new, p);
    }

    /// Moving tokens changes nothing [`CachePerms::keeps`] or
    /// [`CachePerms::follows`] reads.
    pub(super) proof fn lemma_toks_only(a: CachePerms, b: CachePerms, o: CachePerms)
        requires
            a.hdrs == b.hdrs,
            a.slots == b.slots,
            a.cls == b.cls,
            a.owner == b.owner,
            a.keeps(o),
            a.follows(o),
        ensures
            b.keeps(o),
            b.follows(o),
            a.zero_ok() && b.zero == a.zero ==> b.zero_ok(),
    {
        if a.zero_ok() && b.zero == a.zero {
            CachePerms::lemma_zero_same(a, b);
        }
        assert forall|p: *mut CacheHeader, i: int|
            b.hdrs.contains_key(p) && 0 <= i < b.hdr(p).cap implies {
                &&& b.owner.contains_key(#[trigger] b.ent(p, i))
                &&& b.owner[b.ent(p, i)] == (p, i)
                &&& (i < b.hdr(p).len ==> b.holds(b.cls[p], b.ent(p, i), b.hdr(p).slots))
            } by {
            assert(b.ent(p, i) == a.ent(p, i));
        }
    }

    /// A header store that keeps the stack and does not grow its length,
    /// with the entries and tokens untouched, keeps the tokens.
    pub(super) proof fn lemma_tokened_hdr(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.tokened(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.slots == old.slots,
            new.cls == old.cls,
            new.toks == old.toks,
            new.hdr(p).slots == old.hdr(p).slots,
            new.hdr(p).len <= old.hdr(p).len,
        ensures
            new.tokened(),
    {
        assert forall|p2: *mut CacheHeader, i: int|
            #![trigger new.ent(p2, i)]
            new.hdrs.contains_key(p2) && 0 <= i < new.hdr(p2).len implies new.tok_at(p2, i) by {
            assert(new.ent(p2, i) == old.ent(p2, i));
            assert(old.tok_at(p2, i));
        }
    }

    /// A pop (the length one less, nothing written) takes the top entry's
    /// token out and keeps the others'.
    pub(super) proof fn lemma_tokened_pop(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.wf(),
            old.tracks(),
            old.tokened(),
            old.hdrs.contains_key(p),
            old.hdr(p).len > 0,
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.slots == old.slots,
            new.owner == old.owner,
            new.cls == old.cls,
            new.hdr(p).slots == old.hdr(p).slots,
            new.hdr(p).cap == old.hdr(p).cap,
            new.hdr(p).len == old.hdr(p).len - 1,
            new.toks == old.toks.remove(old.ent(p, new.hdr(p).len as int)),
        ensures
            new.tokened(),
    {
        let k = new.hdr(p).len as int;
        assert(cell_ok(p, old.hdrs[p], old.slots));
        assert forall|p2: *mut CacheHeader, j: int|
            old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap implies {
                let q = #[trigger] old.ent(p2, j);
                &&& new.slots[q] == old.slots[q]
                &&& (old.toks.contains_key(q) ==> new.toks.contains_key(q) && new.toks[q] == old.toks[q])
            } by {
            old.lemma_ent_apart(p, k, p2, j);
        }
        assert forall|i: int| 0 <= i < new.hdr(p).len implies #[trigger] new.tok_at(p, i) by {
            old.lemma_tok_get(p, i);
            old.lemma_ent_apart(p, k, p, i);
            assert(new.ent(p, i) == old.ent(p, i));
        }
        CachePerms::lemma_tok_update(old, new, p);
    }

    /// A push (entry `len` written, the length one more) puts the pushed
    /// slot's token `t` at the entry and keeps the others'.
    pub(super) proof fn lemma_tokened_push(old: CachePerms, new: CachePerms, p: *mut CacheHeader, t: GhostSingleton<vstd::prelude::int>)
        requires
            old.wf(),
            old.tracks(),
            old.tokened(),
            old.hdrs.contains_key(p),
            old.hdr(p).len < old.hdr(p).cap,
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.owner == old.owner,
            new.cls == old.cls,
            new.hdr(p).slots == old.hdr(p).slots,
            new.hdr(p).cap == old.hdr(p).cap,
            new.hdr(p).len == old.hdr(p).len + 1,
            new.slots == old.slots.insert(old.ent(p, old.hdr(p).len as int), new.slots[old.ent(p, old.hdr(p).len as int)]),
            new.toks == old.toks.insert(old.ent(p, old.hdr(p).len as int), t),
            t.id() == slot_loc(old.cls[p]),
            t@ == plain(new.slots[old.ent(p, old.hdr(p).len as int)].value(), old.hdr(p).slots) as int,
        ensures
            new.tokened(),
    {
        let k = old.hdr(p).len as int;
        assert(cell_ok(p, old.hdrs[p], old.slots));
        assert forall|p2: *mut CacheHeader, j: int|
            old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap implies {
                let q = #[trigger] old.ent(p2, j);
                &&& new.slots[q] == old.slots[q]
                &&& (old.toks.contains_key(q) ==> new.toks.contains_key(q) && new.toks[q] == old.toks[q])
            } by {
            old.lemma_ent_apart(p, k, p2, j);
        }
        assert forall|i: int| 0 <= i < new.hdr(p).len implies #[trigger] new.tok_at(p, i) by {
            assert(new.ent(p, i) == old.ent(p, i));
            if i < k {
                old.lemma_tok_get(p, i);
                old.lemma_ent_apart(p, k, p, i);
            }
        }
        CachePerms::lemma_tok_update(old, new, p);
    }

    /// Owned headers `a` and `b` of one class trading values, each stack's
    /// entries owned by its new header ([`swap_owner`]), keep the
    /// permissions well formed, tracking and tokened: entries, slots and
    /// tokens stay where they are, and the class each header caches is the
    /// same.
    pub(super) proof fn lemma_swap(o: CachePerms, n: CachePerms, a: *mut CacheHeader, b: *mut CacheHeader)
        requires
            o.wf(),
            o.tracks(),
            o.zero_ok(),
            o.tokened(),
            o.hdrs.contains_key(a),
            o.hdrs.contains_key(b),
            a != b,
            o.cls[a] == o.cls[b],
            n.hdrs == o.hdrs.insert(a, n.hdrs[a]).insert(b, n.hdrs[b]),
            n.hdrs[a].ptr() == a,
            n.hdrs[a].is_init(),
            n.hdrs[a].value() == o.hdr(b),
            n.hdrs[b].ptr() == b,
            n.hdrs[b].is_init(),
            n.hdrs[b].value() == o.hdr(a),
            n.slots == o.slots,
            n.cls == o.cls,
            n.toks == o.toks,
            n.zero == o.zero,
            n.owner == swap_owner(o.owner, a, b),
        ensures
            n.wf(),
            n.tracks(),
            n.zero_ok(),
            n.tokened(),
    {
        // The header that held `p`'s value before the trade.
        let src = |p: *mut CacheHeader| if p == a { b } else if p == b { a } else { p };
        assert(cell_ok(a, o.hdrs[a], o.slots));
        assert(cell_ok(b, o.hdrs[b], o.slots));
        assert forall|p: *mut CacheHeader| #[trigger] n.hdrs.contains_key(p) implies cell_ok(p, n.hdrs[p], n.slots) by {
            if p != a && p != b {
                assert(cell_ok(p, o.hdrs[p], o.slots));
            }
        }
        assert forall|p: *mut CacheHeader, i: int|
            #![trigger n.ent(p, i)]
            n.hdrs.contains_key(p) && 0 <= i < n.hdr(p).cap implies {
                &&& n.owner.contains_key(n.ent(p, i))
                &&& n.owner[n.ent(p, i)] == (p, i)
                &&& (i < n.hdr(p).len ==> n.holds(n.cls[p], n.ent(p, i), n.hdr(p).slots))
            } by {
            let s = src(p);
            assert(n.hdr(p) == o.hdr(s));
            assert(n.ent(p, i) == o.ent(s, i));
            assert(o.owner[o.ent(s, i)] == (s, i));
        }
        assert forall|p: *mut CacheHeader, i: int|
            #![trigger n.ent(p, i)]
            n.hdrs.contains_key(p) && 0 <= i < n.hdr(p).len implies n.tok_at(p, i) by {
            let s = src(p);
            assert(n.hdr(p) == o.hdr(s));
            assert(n.ent(p, i) == o.ent(s, i));
            assert(o.tok_at(s, i));
        }
        assert forall|p: *mut CacheHeader, i: int|
            #![trigger n.ent(p, i)]
            n.hdrs.contains_key(p) && Self::in_prefix(n.hdr(p), i) implies n.zero_at(p, i) by {
            let s = src(p);
            assert(n.hdr(p) == o.hdr(s));
            assert(n.ent(p, i) == o.ent(s, i));
            o.lemma_zero_get(s, i);
        }
        n.lemma_zero_intro();
    }

    /// Uniqueness for one
    /// heap's caches: two different cached entries of one class never
    /// stand for the same slot. Each holds its slot's token
    /// ([`CachePerms::tokened`]), and two
    /// tokens of one authority are disjoint.
    pub(super) proof fn theorem_uniqueness_cached(tracked perms: &mut CachePerms, p1: *mut CacheHeader, i1: int, p2: *mut CacheHeader, i2: int)
        requires
            old(perms).wf(),
            old(perms).tracks(),
            old(perms).tokened(),
            old(perms).hdrs.contains_key(p1),
            old(perms).hdrs.contains_key(p2),
            0 <= i1 < old(perms).hdr(p1).len,
            0 <= i2 < old(perms).hdr(p2).len,
            p1 != p2 || i1 != i2,
            old(perms).cls[p1] == old(perms).cls[p2],
        ensures
            final(perms).hdrs == old(perms).hdrs,
            final(perms).slots == old(perms).slots,
            final(perms).cls == old(perms).cls,
            final(perms).owner == old(perms).owner,
            final(perms).zero == old(perms).zero,
            final(perms).handed == old(perms).handed,
            final(perms).blocks == old(perms).blocks,
            final(perms).released == old(perms).released,
            final(perms).tokened(),
            plain(old(perms).slots[old(perms).ent(p1, i1)].value(), old(perms).hdr(p1).slots)
                != plain(old(perms).slots[old(perms).ent(p2, i2)].value(), old(perms).hdr(p2).slots),
    {
        let ghost o = *perms;
        let q1 = perms.ent(p1, i1);
        let q2 = perms.ent(p2, i2);
        perms.lemma_tok_get(p1, i1);
        perms.lemma_tok_get(p2, i2);
        perms.lemma_ent_apart(p1, i1, p2, i2);
        let tracked mut t1 = perms.toks.tracked_remove(q1);
        t1.disjoint(perms.toks.tracked_borrow(q2));
        perms.toks.tracked_insert(q1, t1);
        assert forall|p: *mut CacheHeader, i: int|
            #![trigger perms.ent(p, i)]
            perms.hdrs.contains_key(p) && 0 <= i < perms.hdr(p).len implies perms.tok_at(p, i) by {
            o.lemma_tok_get(p, i);
        }
    }

    /// Uniqueness against every other holder: a slot whose
    /// token `held` holds is cached nowhere in this heap. `held` stands for
    /// whatever else may hold tokens of the class's authority: the bitmap
    /// words' invariants (the free slots), another heap's caches, a claim
    /// in flight, or a slot handed out to the program.
    pub(super) proof fn theorem_uniqueness_held(tracked perms: &CachePerms, tracked held: &mut GhostSubset<int>, p: *mut CacheHeader, i: int)
        requires
            perms.tokened(),
            perms.hdrs.contains_key(p),
            0 <= i < perms.hdr(p).len,
            old(held).id() == slot_loc(perms.cls[p]),
        ensures
            final(held).id() == old(held).id(),
            final(held)@ == old(held)@,
            !old(held)@.contains(plain(perms.slots[perms.ent(p, i)].value(), perms.hdr(p).slots) as int),
    {
        perms.lemma_tok_get(p, i);
        held.disjoint_singleton(perms.toks.tracked_borrow(perms.ent(p, i)));
    }

    /// `self` [`tracks`](CachePerms::tracks) its contents and every header
    /// `old` owned still caches the class it did.
    pub(super) open spec fn follows(&self, old: CachePerms) -> bool {
        &&& self.tracks()
        &&& forall|p: *mut CacheHeader| #[trigger] old.hdrs.contains_key(p) ==> self.cls[p] == old.cls[p]
    }

    /// Unchanged permissions follow themselves.
    pub(super) proof fn lemma_tracks_same(a: CachePerms)
        requires
            a.tracks(),
        ensures
            a.follows(a),
    {
    }

    /// A change to header `p` alone keeps the tracking of every other
    /// header, as long as their entries keep their owners and values;
    /// `p`'s own entries are the caller's to show.
    pub(super) proof fn lemma_tracks_frame(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.wf(),
            old.tracks(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.cls == old.cls,
            forall|p2: *mut CacheHeader, j: int|
                old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap ==> {
                    let q = #[trigger] old.ent(p2, j);
                    &&& new.owner.contains_key(q)
                    &&& new.owner[q] == old.owner[q]
                    &&& new.slots[q] == old.slots[q]
                },
            forall|i: int| 0 <= i < new.hdr(p).cap ==> {
                let q = #[trigger] new.ent(p, i);
                &&& new.owner.contains_key(q)
                &&& new.owner[q] == (p, i)
                &&& (i < new.hdr(p).len ==> new.holds(new.cls[p], q, new.hdr(p).slots))
            },
        ensures
            new.tracks(),
            new.follows(old),
    {
        assert forall|p2: *mut CacheHeader, j: int|
            new.hdrs.contains_key(p2) && 0 <= j < new.hdr(p2).cap implies {
                &&& new.owner.contains_key(#[trigger] new.ent(p2, j))
                &&& new.owner[new.ent(p2, j)] == (p2, j)
                &&& (j < new.hdr(p2).len ==> new.holds(new.cls[p2], new.ent(p2, j), new.hdr(p2).slots))
            } by {
            if p2 != p {
                assert(new.hdrs[p2] == old.hdrs[p2]);
                assert(new.ent(p2, j) == old.ent(p2, j));
                assert(old.owner[old.ent(p2, j)] == (p2, j));
            }
        }
    }

    /// [`CachePerms::lemma_tracks_frame`] for a header that keeps its stack
    /// and capacity (so its entries keep their owners): only the first
    /// `len` entries holding slots is the caller's to show.
    pub(super) proof fn lemma_tracks_stack(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.wf(),
            old.tracks(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.cls == old.cls,
            new.owner == old.owner,
            new.hdr(p).slots == old.hdr(p).slots,
            new.hdr(p).cap == old.hdr(p).cap,
            forall|p2: *mut CacheHeader, j: int|
                old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap
                    ==> #[trigger] new.slots[old.ent(p2, j)] == old.slots[old.ent(p2, j)],
            forall|i: int| 0 <= i < new.hdr(p).len ==> #[trigger] new.holds(new.cls[p], new.ent(p, i), new.hdr(p).slots),
        ensures
            new.tracks(),
            new.follows(old),
    {
        assert forall|i: int| 0 <= i < new.hdr(p).cap implies {
            let q = #[trigger] new.ent(p, i);
            &&& new.owner.contains_key(q)
            &&& new.owner[q] == (p, i)
            &&& (i < new.hdr(p).len ==> new.holds(new.cls[p], q, new.hdr(p).slots))
        } by {
            assert(new.ent(p, i) == old.ent(p, i));
            assert(old.owner[old.ent(p, i)] == (p, i));
        }
        assert forall|p2: *mut CacheHeader, j: int|
            old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap implies {
                let q = #[trigger] old.ent(p2, j);
                &&& new.owner.contains_key(q)
                &&& new.owner[q] == old.owner[q]
                &&& new.slots[q] == old.slots[q]
            } by {
            assert(old.owner[old.ent(p2, j)] == (p2, j));
            assert(new.slots[old.ent(p2, j)] == old.slots[old.ent(p2, j)]);
        }
        CachePerms::lemma_tracks_frame(old, new, p);
    }

    /// Header stores that keep the stack, its capacity and at most its
    /// first `len` entries, with every entry untouched, keep the tracking.
    pub(super) proof fn lemma_tracks_hdr(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.wf(),
            old.tracks(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.slots == old.slots,
            new.owner == old.owner,
            new.cls == old.cls,
            new.hdr(p).slots == old.hdr(p).slots,
            new.hdr(p).cap == old.hdr(p).cap,
            new.hdr(p).len <= old.hdr(p).len,
        ensures
            new.tracks(),
            new.follows(old),
    {
        assert forall|i: int| 0 <= i < new.hdr(p).cap implies {
            let q = #[trigger] new.ent(p, i);
            &&& new.owner.contains_key(q)
            &&& new.owner[q] == (p, i)
            &&& (i < new.hdr(p).len ==> new.holds(new.cls[p], q, new.hdr(p).slots))
        } by {
            assert(new.ent(p, i) == old.ent(p, i));
        }
        assert forall|p2: *mut CacheHeader, j: int|
            old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap implies {
                let q = #[trigger] old.ent(p2, j);
                &&& new.owner.contains_key(q)
                &&& new.owner[q] == old.owner[q]
                &&& new.slots[q] == old.slots[q]
            } by {
            assert(old.owner[old.ent(p2, j)] == (p2, j));
        }
        CachePerms::lemma_tracks_frame(old, new, p);
    }

    /// One entry of `p`'s stack, `i`, written, and the length at most the
    /// old one or one more with `i` the new entry: the tracking holds when
    /// the written entry, if below the new length, holds a slot.
    pub(super) proof fn lemma_tracks_put(old: CachePerms, new: CachePerms, p: *mut CacheHeader, i: int)
        requires
            old.wf(),
            old.tracks(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.owner == old.owner,
            new.cls == old.cls,
            new.hdr(p).slots == old.hdr(p).slots,
            new.hdr(p).cap == old.hdr(p).cap,
            0 <= i < old.hdr(p).cap,
            new.slots == old.slots.insert(old.ent(p, i), new.slots[old.ent(p, i)]),
            new.hdr(p).len <= old.hdr(p).len || (new.hdr(p).len == old.hdr(p).len + 1 && i == old.hdr(p).len),
            i < new.hdr(p).len ==> new.holds(new.cls[p], old.ent(p, i), new.hdr(p).slots),
        ensures
            new.tracks(),
            new.follows(old),
    {
        let w = old.ent(p, i);
        assert(old.owner[w] == (p, i));
        assert forall|k: int| 0 <= k < new.hdr(p).cap implies {
            let q = #[trigger] new.ent(p, k);
            &&& new.owner.contains_key(q)
            &&& new.owner[q] == (p, k)
            &&& (k < new.hdr(p).len ==> new.holds(new.cls[p], q, new.hdr(p).slots))
        } by {
            assert(new.ent(p, k) == old.ent(p, k));
            assert(old.owner[old.ent(p, k)] == (p, k));
            if k != i {
                assert(old.ent(p, k) != w);
            }
        }
        assert forall|p2: *mut CacheHeader, j: int|
            old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap implies {
                let q = #[trigger] old.ent(p2, j);
                &&& new.owner.contains_key(q)
                &&& new.owner[q] == old.owner[q]
                &&& new.slots[q] == old.slots[q]
            } by {
            assert(old.owner[old.ent(p2, j)] == (p2, j));
            assert(old.ent(p2, j) != w);
        }
        CachePerms::lemma_tracks_frame(old, new, p);
    }

    /// Entries `start..start + len` of `p`'s stack rewritten through a
    /// slice ([`stack_mut`]'s frame, from `before` to `after`, where
    /// `before` owns at least what `old` does): every other header's
    /// entries, and `p`'s outside the range, keep their values.
    pub(super) proof fn lemma_slice_frame(
        old: CachePerms,
        before: Map<*mut *mut u8, PointsTo<*mut u8>>,
        after: Map<*mut *mut u8, PointsTo<*mut u8>>,
        p: *mut CacheHeader,
        start: int,
        len: int,
    )
        requires
            old.wf(),
            old.tracks(),
            old.hdrs.contains_key(p),
            slots_grow(old.slots, before),
            0 <= start,
            start + len <= old.hdr(p).cap,
            forall|q: *mut *mut u8|
                before.contains_key(q) && (forall|i: int| start <= i < start + len ==> q != #[trigger] slot_ptr(old.hdr(p).slots, i))
                    ==> #[trigger] after[q] == before[q],
        ensures
            forall|p2: *mut CacheHeader, j: int|
                old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap
                    ==> #[trigger] after[old.ent(p2, j)] == before[old.ent(p2, j)],
            forall|k: int|
                0 <= k < old.hdr(p).cap && !(start <= k < start + len)
                    ==> #[trigger] after[old.ent(p, k)] == before[old.ent(p, k)],
    {
        assert forall|p2: *mut CacheHeader, j: int|
            old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap
                implies #[trigger] after[old.ent(p2, j)] == before[old.ent(p2, j)] by {
            let q = old.ent(p2, j);
            assert(cell_ok(p2, old.hdrs[p2], old.slots));
            assert(old.slots.contains_key(q));
            assert(old.owner[q] == (p2, j));
            assert forall|i: int| start <= i < start + len implies q != #[trigger] slot_ptr(old.hdr(p).slots, i) by {
                assert(old.owner[old.ent(p, i)] == (p, i));
            }
        }
        assert forall|k: int|
            0 <= k < old.hdr(p).cap && !(start <= k < start + len)
                implies #[trigger] after[old.ent(p, k)] == before[old.ent(p, k)] by {
            let q = old.ent(p, k);
            assert(cell_ok(p, old.hdrs[p], old.slots));
            assert(old.slots.contains_key(q));
            assert(old.owner[q] == (p, k));
            assert forall|i: int| start <= i < start + len implies q != #[trigger] slot_ptr(old.hdr(p).slots, i) by {
                assert(old.owner[old.ent(p, i)] == (p, i));
            }
        }
    }

    /// Changing one header (to a [`cell_ok`] value) and writing stack
    /// entries keeps the permissions well formed.
    pub(super) proof fn lemma_update(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.wf(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            cell_ok(p, new.hdrs[p], new.slots),
            slots_grow(old.slots, new.slots),
            old.hdr(p).cap != 0 ==> new.hdr(p).cap == old.hdr(p).cap && new.hdr(p).slots == old.hdr(p).slots,
        ensures
            new.keeps(old),
    {
        assert forall|q: *mut CacheHeader| #[trigger] new.hdrs.contains_key(q) implies cell_ok(q, new.hdrs[q], new.slots) by {
            if q != p {
                assert(old.hdrs.contains_key(q));
                assert(cell_ok(q, old.hdrs[q], old.slots));
                let h = old.hdrs[q].value();
                assert forall|i: int| 0 <= i < h.cap implies #[trigger] slot_in(h.slots, i) by {}
                assert forall|i: int| 0 <= i < h.cap implies {
                    let s = #[trigger] slot_ptr(h.slots, i);
                    &&& new.slots.contains_key(s)
                    &&& new.slots[s].ptr() == s
                    &&& (i < h.len ==> new.slots[s].is_init())
                } by {
                    assert(old.slots.contains_key(slot_ptr(h.slots, i)));
                }
            }
        }
    }

    /// Well-formed permissions keep themselves.
    pub(super) proof fn lemma_keeps_refl(a: CachePerms)
        requires
            a.wf(),
        ensures
            a.keeps(a),
    {
    }

    /// Giving up owned headers, the rest of the permissions unchanged,
    /// keeps [`CachePerms::wf`], [`CachePerms::tracks`],
    /// [`CachePerms::zero_ok`], [`CachePerms::tokened`] and
    /// [`CachePerms::claimed`] (`Heap::destroy`, as it frees a node): each
    /// states something of an owned header in terms of the parts that did
    /// not change.
    pub(super) proof fn lemma_remove_hdrs(old: CachePerms, new: CachePerms)
        requires
            old.wf(),
            old.tracks(),
            old.zero_ok(),
            old.tokened(),
            old.claimed(),
            new == (CachePerms { hdrs: new.hdrs, ..old }),
            forall|p: *mut CacheHeader| #[trigger] new.hdrs.contains_key(p) ==> old.hdrs.contains_key(p),
            forall|p: *mut CacheHeader| #[trigger] new.hdrs.contains_key(p) ==> new.hdrs[p] == old.hdrs[p],
        ensures
            new.wf(),
            new.tracks(),
            new.zero_ok(),
            new.tokened(),
            new.claimed(),
    {
        reveal(CachePerms::claimed);
        CachePerms::lemma_zero_remove(old, new);
        assert forall|q: *mut CacheHeader| #[trigger] new.hdrs.contains_key(q) implies cell_ok(q, new.hdrs[q], new.slots) by {
            assert(cell_ok(q, old.hdrs[q], old.slots));
        }
        assert forall|q: *mut CacheHeader, i: int|
            #![trigger new.ent(q, i)]
            new.hdrs.contains_key(q) && 0 <= i < new.hdr(q).cap implies {
                &&& new.owner.contains_key(new.ent(q, i))
                &&& new.owner[new.ent(q, i)] == (q, i)
                &&& (i < new.hdr(q).len ==> new.holds(new.cls[q], new.ent(q, i), new.hdr(q).slots))
            } by {
            assert(new.ent(q, i) == old.ent(q, i));
        }
        assert forall|q: *mut CacheHeader, i: int|
            #![trigger new.ent(q, i)]
            new.hdrs.contains_key(q) && 0 <= i < new.hdr(q).len implies new.tok_at(q, i) by {
            assert(new.ent(q, i) == old.ent(q, i));
            assert(old.tok_at(q, i));
        }
        assert forall|q: *mut CacheHeader| #[trigger] new.hdrs.contains_key(q) && new.hdr(q).cap != 0 implies stack_claimed(new.hdr(q), new.blocks) by {
            assert(stack_claimed(old.hdr(q), old.blocks));
        }
    }

    /// A header `old` does not own joining it unused (no stack, nothing
    /// cached), as a header of class `c`, keeps everything `old` had.
    pub(super) proof fn lemma_insert_unused(old: CachePerms, new: CachePerms, p: *mut CacheHeader, c: int)
        requires
            old.wf(),
            old.tracks(),
            old.zero_ok(),
            old.tokened(),
            !old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.cls == old.cls.insert(p, c),
            new.slots == old.slots,
            new.owner == old.owner,
            new.toks == old.toks,
            new.zero == old.zero,
            cell_ok(p, new.hdrs[p], new.slots),
            new.hdr(p).cap == 0,
        ensures
            new.keeps(old),
            new.follows(old),
            new.zero_ok(),
            new.tokened(),
    {
        assert forall|q: *mut CacheHeader| #[trigger] new.hdrs.contains_key(q) implies cell_ok(q, new.hdrs[q], new.slots) by {
            if q != p {
                assert(cell_ok(q, old.hdrs[q], old.slots));
            }
        }
        assert forall|q: *mut CacheHeader, i: int|
            #![trigger new.ent(q, i)]
            new.hdrs.contains_key(q) && 0 <= i < new.hdr(q).cap implies {
                &&& new.owner.contains_key(new.ent(q, i))
                &&& new.owner[new.ent(q, i)] == (q, i)
                &&& (i < new.hdr(q).len ==> new.holds(new.cls[q], new.ent(q, i), new.hdr(q).slots))
            } by {
            assert(q != p);
            assert(new.ent(q, i) == old.ent(q, i));
        }
        assert forall|q: *mut CacheHeader, i: int|
            #![trigger new.ent(q, i)]
            new.hdrs.contains_key(q) && 0 <= i < new.hdr(q).len implies new.tok_at(q, i) by {
            assert(q != p);
            assert(new.ent(q, i) == old.ent(q, i));
            assert(old.tok_at(q, i));
        }
        CachePerms::lemma_zero_insert(old, new, p);
    }

    /// Writing one owned stack entry keeps every stack owned.
    pub(super) proof fn lemma_write_grows(old: Map<*mut *mut u8, PointsTo<*mut u8>>, new: Map<*mut *mut u8, PointsTo<*mut u8>>, q: *mut *mut u8)
        requires
            old.contains_key(q),
            old[q].ptr() == q,
            new == old.insert(q, new[q]),
            new[q].ptr() == q,
            new[q].is_init(),
        ensures
            slots_grow(old, new),
    {
    }
}

/// The first `n` entries of `t` are entries of `s`: what a permutation of
/// `s` into `t` (a selection, a sort) leaves.
pub(super) open spec fn from_all(t: Seq<*mut u8>, s: Seq<*mut u8>, n: int) -> bool {
    forall|i: int| 0 <= i < n ==> s.contains(#[trigger] t[i])
}

/// Every entry of `s` is an entry of `s`.
pub(super) proof fn lemma_from_refl(s: Seq<*mut u8>)
    ensures
        from_all(s, s, s.len() as int),
{
    assert forall|i: int| 0 <= i < s.len() implies s.contains(#[trigger] s[i]) by {
        // A named copy: `s[i] == s[i]` alone simplifies away before the
        // witness `i` can be matched.
        let x = s[i];
        assert(s[i] == x);
    }
}

/// Entries of a prefix of `t` are entries of `t`.
pub(super) proof fn lemma_from_sub(t: Seq<*mut u8>, n: int, u: Seq<*mut u8>)
    requires
        0 <= n <= t.len(),
        from_all(u, t.subrange(0, n), n),
    ensures
        from_all(u, t, n),
{
    assert forall|i: int| 0 <= i < n implies t.contains(#[trigger] u[i]) by {
        assert(t.subrange(0, n).contains(u[i]));
        let j = choose|j: int| 0 <= j < n && t.subrange(0, n)[j] == u[i];
        assert(t[j] == t.subrange(0, n)[j]);
    }
}

/// Entries of entries of `s` are entries of `s`.
pub(super) proof fn lemma_from_trans(s: Seq<*mut u8>, t: Seq<*mut u8>, u: Seq<*mut u8>, n: int)
    requires
        from_all(t, s, t.len() as int),
        from_all(u, t, n),
    ensures
        from_all(u, s, n),
{
    assert forall|i: int| 0 <= i < n implies s.contains(#[trigger] u[i]) by {
        assert(t.contains(u[i]));
        let j = choose|j: int| 0 <= j < t.len() && t[j] == u[i];
        assert(s.contains(t[j]));
    }
}

/// The first `n` entries of `t` hold slots of `c` in the stack at `base`
/// as a push stores them ([`slot_entry`]) when every entry of `s` does and
/// they are entries of `s`.
pub(super) proof fn lemma_from_slots(s: Seq<*mut u8>, t: Seq<*mut u8>, c: int, n: int, base: *mut *mut u8)
    requires
        forall|j: int| 0 <= j < s.len() ==> slot_entry(c, #[trigger] s[j], base),
        from_all(t, s, n),
    ensures
        forall|i: int| 0 <= i < n ==> slot_entry(c, #[trigger] t[i], base),
{
    assert forall|i: int| 0 <= i < n implies slot_entry(c, #[trigger] t[i], base) by {
        assert(s.contains(t[i]));
        let j = choose|j: int| 0 <= j < s.len() && s[j] == t[i];
        assert(slot_entry(c, s[j], base));
    }
}

/// The run rebuild's moves ([`Heap::rebuild_run`]: the block of `n`
/// swapped with the bottom `n` of the `len` old entries, or rotated under
/// them) leave the first `len + n` entries entries of what was there.
pub(super) proof fn lemma_rebuild_moves(a: Seq<*mut u8>, b: Seq<*mut u8>, len: int, n: int)
    requires
        a.len() == len + n,
        b.len() == len + n,
        0 <= len,
        0 <= n,
        len >= n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] b[i] == if i < n { a[len + i] } else if i < len { a[i] } else { a[i - len] },
        len < n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] b[i] == if i < n { a[len + i] } else { a[i - n] },
    ensures
        from_all(b, a, len + n),
{
    assert forall|i: int| 0 <= i < len + n implies a.contains(#[trigger] b[i]) by {
        let j = if len >= n {
            if i < n { len + i } else if i < len { i } else { i - len }
        } else {
            if i < n { len + i } else { i - n }
        };
        assert(0 <= j < a.len());
        assert(a[j] == b[i]);
    }
}

/// The entry the run rebuild's moves ([`lemma_rebuild_moves`]) take entry
/// `i` of the first `len + n` from.
pub(super) open spec fn rebuild_src(len: int, n: int, i: int) -> int {
    if len >= n {
        if i < n { len + i } else if i < len { i } else { i - len }
    } else {
        if i < n { len + i } else { i - n }
    }
}

/// The run rebuild's moves are a permutation of the first `len + n`
/// entries ([`rebuild_src`]): `f` stays injective on them.
#[verifier::spinoff_prover]
pub(super) proof fn lemma_rebuild_distinct(a: Seq<*mut u8>, b: Seq<*mut u8>, len: int, n: int, f: spec_fn(*mut u8) -> int)
    requires
        a.len() == len + n,
        b.len() == len + n,
        0 <= len,
        0 <= n,
        len >= n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] b[i] == if i < n { a[len + i] } else if i < len { a[i] } else { a[i - len] },
        len < n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] b[i] == if i < n { a[len + i] } else { a[i - n] },
        forall|i: int, j: int| #![trigger f(a[i]), f(a[j])] 0 <= i < j < len + n ==> f(a[i]) != f(a[j]),
    ensures
        forall|i: int, j: int| #![trigger f(b[i]), f(b[j])] 0 <= i < j < len + n ==> f(b[i]) != f(b[j]),
        forall|i: int| 0 <= i < len + n ==> 0 <= #[trigger] rebuild_src(len, n, i) < len + n && b[i] == a[rebuild_src(len, n, i)],
{
    assert forall|i: int| 0 <= i < len + n implies 0 <= #[trigger] rebuild_src(len, n, i) < len + n && b[i] == a[rebuild_src(len, n, i)] by {
        // Names `b[i]`, which instantiates the moves' quantifier at `i`.
        assert(b[i] == b[i]);
    }
    assert forall|i: int, j: int| 0 <= i < j < len + n implies f(b[i]) != f(b[j]) by {
        let si = rebuild_src(len, n, i);
        let sj = rebuild_src(len, n, j);
        assert(0 <= si < len + n && b[i] == a[si]);
        assert(0 <= sj < len + n && b[j] == a[sj]);
        assert(si != sj);
        if si < sj {
            assert(f(a[si]) != f(a[sj]));
        } else {
            assert(f(a[sj]) != f(a[si]));
        }
    }
}

/// The tracking after [`Heap::rebuild_run`]'s two slices: the block of
/// `n` claimed slots (`claimed`, reversed into `outf`) went into entries
/// `len..len + want` of the stack of `p` (from `old` to `mid`), and entries
/// `0..len + n` were then moved (`all0` to `allf`, from `mid` to `new`).
pub(super) proof fn lemma_rebuild_tracks(
    old: CachePerms,
    mid: Map<*mut *mut u8, PointsTo<*mut u8>>,
    new: CachePerms,
    p: *mut CacheHeader,
    len: int,
    want: int,
    n: int,
    claimed: Seq<*mut u8>,
    outf: Seq<*mut u8>,
    all0: Seq<*mut u8>,
    allf: Seq<*mut u8>,
)
    requires
        old.wf(),
        old.tracks(),
        old.hdrs.contains_key(p),
        len == old.hdr(p).len,
        0 <= n <= want,
        len + want <= old.hdr(p).cap,
        new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
        new.cls == old.cls,
        new.owner == old.owner,
        new.hdr(p).slots == old.hdr(p).slots,
        new.hdr(p).cap == old.hdr(p).cap,
        new.hdr(p).len == len + n,
        slots_in(claimed, old.cls[p], 0, n),
        claimed.len() == want,
        outf.len() == want,
        forall|k: int| 0 <= k < n ==> (#[trigger] outf[k])@.addr == stored(claimed[n - 1 - k]@.addr, old.hdr(p).slots),
        regions_ok(),
        all0.len() == len + n,
        from_all(allf, all0, len + n),
        // The first slice (`len..len + want`, from `old.slots` to `mid`).
        slots_grow(old.slots, mid),
        forall|q: *mut *mut u8|
            old.slots.contains_key(q) && (forall|i: int| len <= i < len + want ==> q != #[trigger] slot_ptr(old.hdr(p).slots, i))
                ==> #[trigger] mid[q] == old.slots[q],
        forall|i: int| len <= i < len + want ==> #[trigger] mid[slot_ptr(old.hdr(p).slots, i)].is_init(),
        forall|i: int| len <= i < len + want ==> #[trigger] mid[slot_ptr(old.hdr(p).slots, i)].value() == outf[i - len],
        // The second (`0..len + n`, from `mid` to `new.slots`).
        forall|i: int|
            0 <= i < len + n && mid[slot_ptr(old.hdr(p).slots, i)].is_init()
                ==> all0[i] == #[trigger] mid[slot_ptr(old.hdr(p).slots, i)].value(),
        forall|i: int| 0 <= i < len + n ==> #[trigger] new.slots[slot_ptr(old.hdr(p).slots, i)].value() == allf[i],
        forall|q: *mut *mut u8|
            mid.contains_key(q) && (forall|i: int| 0 <= i < len + n ==> q != #[trigger] slot_ptr(old.hdr(p).slots, i))
                ==> #[trigger] new.slots[q] == mid[q],
    ensures
        new.tracks(),
        new.follows(old),
{
    let slots = old.hdr(p).slots;
    let c = old.cls[p];
    CachePerms::lemma_slice_frame(old, old.slots, mid, p, len, want);
    assert(slots_grow(old.slots, mid));
    CachePerms::lemma_slice_frame(old, mid, new.slots, p, 0, len + n);
    assert forall|j: int| 0 <= j < all0.len() implies slot_entry(c, #[trigger] all0[j], slots) by {
        assert(cell_ok(p, old.hdrs[p], old.slots));
        if j < len {
            assert(old.ent(p, j) == slot_ptr(slots, j));
            assert(old.owner[old.ent(p, j)] == (p, j));
            assert(old.slots[slot_ptr(slots, j)].is_init());
            assert(mid[old.ent(p, j)] == old.slots[old.ent(p, j)]);
            assert(mid[slot_ptr(slots, j)].is_init());
            assert(all0[j] == mid[slot_ptr(slots, j)].value());
            assert(old.holds(c, old.ent(p, j), old.hdr(p).slots));
        } else {
            assert(mid[slot_ptr(slots, j)].is_init());
            assert(all0[j] == mid[slot_ptr(slots, j)].value());
            let k = j - len;
            assert(mid[slot_ptr(slots, j)].value() == outf[k]);
            assert(process_regions().is_slot(c, claimed[n - 1 - k]@.addr));
            lemma_plain_stored(outf[k], claimed[n - 1 - k], c, slots);
        }
    }
    lemma_from_slots(all0, allf, c, len + n, slots);
    assert forall|i: int| 0 <= i < new.hdr(p).len implies #[trigger] new.holds(new.cls[p], new.ent(p, i), new.hdr(p).slots) by {
        assert(new.ent(p, i) == slot_ptr(slots, i));
    }
    assert forall|p2: *mut CacheHeader, j: int|
        old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap
            implies #[trigger] new.slots[old.ent(p2, j)] == old.slots[old.ent(p2, j)] by {
    }
    CachePerms::lemma_tracks_stack(old, new, p);
}

/// Entries `k..n` of the stack at `slots` take the tokens `from[n - 1 - i]`:
/// a claimed batch of `n` slots, reversed onto the stack.
#[verifier::spinoff_prover]
pub(super) proof fn move_reversed(
    tracked from: &mut Map<int, GhostSingleton<int>>,
    tracked to: &mut Map<*mut *mut u8, GhostSingleton<int>>,
    slots: *mut *mut u8,
    n: int,
    k: int,
)
    requires
        0 <= k <= n,
        forall|i: int| k <= i < n ==> #[trigger] old(from).contains_key(n - 1 - i),
        forall|i: int, j: int| #![trigger slot_ptr(slots, i), slot_ptr(slots, j)] k <= i < j < n ==> slot_ptr(slots, i) != slot_ptr(slots, j),
    ensures
        forall|i: int| k <= i < n ==> {
            &&& final(to).contains_key(#[trigger] slot_ptr(slots, i))
            &&& final(to)[slot_ptr(slots, i)] == old(from)[n - 1 - i]
        },
        forall|q: *mut *mut u8| #![trigger final(to).contains_key(q)] #![trigger old(to).contains_key(q)] !in_entries(slots, q, k, n) ==> {
            &&& final(to).contains_key(q) == old(to).contains_key(q)
            &&& old(to).contains_key(q) ==> final(to)[q] == old(to)[q]
        },
    decreases n - k,
{
    if k < n {
        let ghost from0 = *from;
        let ghost to0 = *to;
        let tracked t = from.tracked_remove(n - 1 - k);
        to.tracked_insert(slot_ptr(slots, k), t);
        let ghost to1 = *to;
        assert forall|i: int| k + 1 <= i < n implies #[trigger] from.contains_key(n - 1 - i) by {
            assert(from0.contains_key(n - 1 - i));
        }
        move_reversed(from, to, slots, n, k + 1);
        assert(!in_entries(slots, slot_ptr(slots, k), k + 1, n)) by {
            if in_entries(slots, slot_ptr(slots, k), k + 1, n) {
                let i = choose|i: int| k + 1 <= i < n && slot_ptr(slots, k) == slot_ptr(slots, i);
            }
        }
        assert(to.contains_key(slot_ptr(slots, k)) == to1.contains_key(slot_ptr(slots, k)));
        assert forall|q: *mut *mut u8| #![trigger to.contains_key(q)] #![trigger to0.contains_key(q)] !in_entries(slots, q, k, n) implies {
            &&& to.contains_key(q) == to0.contains_key(q)
            &&& to0.contains_key(q) ==> to[q] == to0[q]
        } by {
            assert(q != slot_ptr(slots, k));
            if in_entries(slots, q, k + 1, n) {
                let i = choose|i: int| k + 1 <= i < n && q == slot_ptr(slots, i);
                assert(in_entries(slots, q, k, n));
            }
        }
    }
}

/// `m` holds the token of `a[i]` under entry `i` of the stack at `slots`,
/// of authority `loc`, for every `lo <= i < hi`.
pub(super) open spec fn keyed_ents(m: Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, a: Seq<int>, loc: vstd::resource::Loc, lo: int, hi: int) -> bool {
    forall|i: int| lo <= i < hi ==> {
        let q = #[trigger] slot_ptr(slots, i);
        &&& m.contains_key(q)
        &&& m[q].id() == loc
        &&& m[q]@ == a[i]
    }
}

/// Entries `lo..hi` of the stack at `slots` are distinct words.
pub(super) open spec fn ents_distinct(slots: *mut *mut u8, lo: int, hi: int) -> bool {
    forall|i: int, j: int| #![trigger slot_ptr(slots, i), slot_ptr(slots, j)] lo <= i < j < hi ==> slot_ptr(slots, i) != slot_ptr(slots, j)
}

/// Every key of `new` but entries `lo..hi` of the stack at `slots` is as
/// in `old`.
pub(super) open spec fn ents_frame(new: Map<*mut *mut u8, GhostSingleton<int>>, old: Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, lo: int, hi: int) -> bool {
    forall|q: *mut *mut u8| #![trigger new.contains_key(q)] #![trigger old.contains_key(q)] !in_entries(slots, q, lo, hi) ==> {
        &&& new.contains_key(q) == old.contains_key(q)
        &&& old.contains_key(q) ==> new[q] == old[q]
    }
}

/// Entry `i` of the stack at `slots` is not one of entries `lo..hi`
/// other than itself.
pub(super) proof fn lemma_not_in_entries(slots: *mut *mut u8, i: int, lo: int, hi: int)
    requires
        ents_distinct(slots, lo, hi),
        lo <= i < hi,
    ensures
        !in_entries(slots, slot_ptr(slots, i), i + 1, hi),
        !in_entries(slots, slot_ptr(slots, i), lo, i),
{
    if in_entries(slots, slot_ptr(slots, i), i + 1, hi) {
        let k = choose|k: int| i + 1 <= k < hi && slot_ptr(slots, i) == slot_ptr(slots, k);
    }
    if in_entries(slots, slot_ptr(slots, i), lo, i) {
        let k = choose|k: int| lo <= k < i && slot_ptr(slots, i) == slot_ptr(slots, k);
    }
}

/// Move the tokens of entries `lo..hi` (of `a[lo..hi]`) into `bag`; they
/// are distinct, since tokens of one authority are disjoint.
#[verifier::spinoff_prover]
pub(super) proof fn gather_ents(tracked bag: &mut GhostSubset<int>, tracked m: &mut Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, a: Seq<int>, lo: int, hi: int)
    requires
        lo <= hi,
        keyed_ents(*old(m), slots, a, old(bag).id(), lo, hi),
        ents_distinct(slots, lo, hi),
    ensures
        final(bag).id() == old(bag).id(),
        final(bag)@ == old(bag)@.union(aset(a, lo, hi)),
        old(bag)@.disjoint(aset(a, lo, hi)),
        distinct(a, lo, hi),
        ents_frame(*final(m), *old(m), slots, lo, hi),
    decreases hi - lo,
{
    broadcast use crate::class::lemma_aset;
    if lo < hi {
        let ghost b0 = bag@;
        let ghost m0 = *m;
        let ghost q0 = slot_ptr(slots, lo);
        assert(m0.contains_key(q0) && m0[q0].id() == bag.id() && m0[q0]@ == a[lo]);
        let tracked t = m.tracked_remove(q0);
        bag.combine_singleton(t);
        let ghost b1 = bag@;
        let ghost m1 = *m;
        assert forall|i: int| lo + 1 <= i < hi implies {
            let q = #[trigger] slot_ptr(slots, i);
            &&& m.contains_key(q)
            &&& m[q].id() == bag.id()
            &&& m[q]@ == a[i]
        } by {
            assert(q0 != slot_ptr(slots, i));
            assert(m0.contains_key(slot_ptr(slots, i)));
        }
        gather_ents(bag, m, slots, a, lo + 1, hi);
        crate::class::lemma_aset_front(a, lo, hi);
        assert(bag@ =~= b0.union(aset(a, lo, hi)));
        assert forall|i: int, j: int| lo <= i < j < hi implies a[i] != a[j] by {
            if i == lo {
                crate::class::lemma_aset_step(a, lo + 1, j, hi);
                assert(b1.contains(a[lo]));
            }
        }
        lemma_not_in_entries(slots, lo, lo, hi);
        assert forall|q: *mut *mut u8| #![trigger m.contains_key(q)] #![trigger m0.contains_key(q)] !in_entries(slots, q, lo, hi) implies {
            &&& m.contains_key(q) == m0.contains_key(q)
            &&& m0.contains_key(q) ==> m[q] == m0[q]
        } by {
            assert(q != q0);
            if in_entries(slots, q, lo + 1, hi) {
                let k = choose|k: int| lo + 1 <= k < hi && q == slot_ptr(slots, k);
                assert(in_entries(slots, q, lo, hi));
            }
            assert(m.contains_key(q) == m1.contains_key(q));
        }
    } else {
        crate::class::lemma_aset_empty(a, lo, hi);
        assert(bag@ =~= bag@.union(aset(a, lo, hi)));
    }
}

/// [`gather_ents`] of entries `0..hi`, `hi > 0`, into a bag of their own.
#[verifier::spinoff_prover]
pub(super) proof fn gather_all_ents(tracked m: &mut Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, a: Seq<int>, loc: vstd::resource::Loc, hi: int) -> (tracked bag: GhostSubset<int>)
    requires
        0 < hi,
        keyed_ents(*old(m), slots, a, loc, 0, hi),
        ents_distinct(slots, 0, hi),
    ensures
        bag.id() == loc,
        bag@ == aset(a, 0, hi),
        distinct(a, 0, hi),
        ents_frame(*final(m), *old(m), slots, 0, hi),
{
    broadcast use crate::class::lemma_aset;
    let ghost m0 = *m;
    let ghost q0 = slot_ptr(slots, 0);
    assert(m0.contains_key(q0) && m0[q0].id() == loc && m0[q0]@ == a[0]);
    let tracked t = m.tracked_remove(q0);
    let tracked mut bag = t.subset();
    let ghost m1 = *m;
    assert forall|i: int| 1 <= i < hi implies {
        let q = #[trigger] slot_ptr(slots, i);
        &&& m.contains_key(q)
        &&& m[q].id() == bag.id()
        &&& m[q]@ == a[i]
    } by {
        assert(q0 != slot_ptr(slots, i));
        assert(m0.contains_key(slot_ptr(slots, i)));
    }
    let ghost b1 = bag@;
    gather_ents(&mut bag, m, slots, a, 1, hi);
    crate::class::lemma_aset_front(a, 0, hi);
    assert(bag@ =~= aset(a, 0, hi));
    assert forall|i: int, j: int| 0 <= i < j < hi implies a[i] != a[j] by {
        if i == 0 {
            crate::class::lemma_aset_step(a, 1, j, hi);
            assert(b1.contains(a[0]));
        }
    }
    lemma_not_in_entries(slots, 0, 0, hi);
    assert forall|q: *mut *mut u8| #![trigger m.contains_key(q)] #![trigger m0.contains_key(q)] !in_entries(slots, q, 0, hi) implies {
        &&& m.contains_key(q) == m0.contains_key(q)
        &&& m0.contains_key(q) ==> m[q] == m0[q]
    } by {
        assert(q != q0);
        if in_entries(slots, q, 1, hi) {
            let k = choose|k: int| 1 <= k < hi && q == slot_ptr(slots, k);
            assert(in_entries(slots, q, 0, hi));
        }
        assert(m.contains_key(q) == m1.contains_key(q));
    }
    bag
}

/// Move the tokens of `a[lo..hi]` out of `bag` into entries `lo..hi` of
/// the stack at `slots`.
#[verifier::spinoff_prover]
pub(super) proof fn scatter_ents(tracked bag: &mut GhostSubset<int>, tracked m: &mut Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, a: Seq<int>, lo: int, hi: int)
    requires
        lo <= hi,
        distinct(a, lo, hi),
        aset(a, lo, hi) <= old(bag)@,
        ents_distinct(slots, lo, hi),
    ensures
        final(bag).id() == old(bag).id(),
        final(bag)@ == old(bag)@ - aset(a, lo, hi),
        keyed_ents(*final(m), slots, a, old(bag).id(), lo, hi),
        ents_frame(*final(m), *old(m), slots, lo, hi),
    decreases hi - lo,
{
    broadcast use crate::class::lemma_aset;
    if lo < hi {
        let ghost b0 = bag@;
        let ghost id = bag.id();
        let ghost m0 = *m;
        let ghost q0 = slot_ptr(slots, lo);
        crate::class::lemma_aset_step(a, lo, lo, hi);
        crate::class::lemma_aset_front(a, lo, hi);
        let tracked t = bag.split_singleton(a[lo]);
        m.tracked_insert(q0, t);
        let ghost m1 = *m;
        assert forall|i: int, j: int| lo + 1 <= i < j < hi implies a[i] != a[j] by {
            assert(a[i] != a[j]);
        }
        assert forall|x: int| aset(a, lo + 1, hi).contains(x) implies bag@.contains(x) by {
            let i = choose|i: int| lo + 1 <= i < hi && a[i] == x;
            assert(a[i] != a[lo]);
        }
        scatter_ents(bag, m, slots, a, lo + 1, hi);
        assert(bag@ =~= b0 - aset(a, lo, hi));
        lemma_not_in_entries(slots, lo, lo, hi);
        assert(!in_entries(slots, q0, lo + 1, hi));
        assert(m.contains_key(q0) == m1.contains_key(q0));
        assert(m[q0] == m1[q0]);
        assert forall|i: int| lo <= i < hi implies {
            let q = #[trigger] slot_ptr(slots, i);
            &&& m.contains_key(q)
            &&& m[q].id() == id
            &&& m[q]@ == a[i]
        } by {
        }
        assert forall|q: *mut *mut u8| #![trigger m.contains_key(q)] #![trigger m0.contains_key(q)] !in_entries(slots, q, lo, hi) implies {
            &&& m.contains_key(q) == m0.contains_key(q)
            &&& m0.contains_key(q) ==> m[q] == m0[q]
        } by {
            assert(q != q0);
            if in_entries(slots, q, lo + 1, hi) {
                let k = choose|k: int| lo + 1 <= k < hi && q == slot_ptr(slots, k);
                assert(in_entries(slots, q, lo, hi));
            }
            assert(m.contains_key(q) == m1.contains_key(q));
        }
    } else {
        crate::class::lemma_aset_empty(a, lo, hi);
        assert(bag@ =~= bag@ - aset(a, lo, hi));
    }
}

impl CachePerms {
    /// The compacting pop's token moves ([`Heap::pop_compact_run`]): entry
    /// `at` takes the top entry's slot and token, the served slot's token
    /// leaves, and every other entry keeps its own.
    #[verifier::spinoff_prover]
    pub(super) proof fn lemma_tokened_compact(tracked toks: &mut Map<*mut *mut u8, GhostSingleton<int>>, o: CachePerms, new: CachePerms, p: *mut CacheHeader, at: int, top: int) -> (tracked served: GhostSingleton<int>)
        requires
            o.wf(),
            o.tracks(),
            o.tokened(),
            o.hdrs.contains_key(p),
            0 <= at <= top,
            top + 1 == o.hdr(p).len,
            *old(toks) == o.toks,
            new.hdrs == o.hdrs.insert(p, new.hdrs[p]),
            new.cls == o.cls,
            new.hdr(p).slots == o.hdr(p).slots,
            new.hdr(p).len == top,
            new.slots == o.slots.insert(o.ent(p, at), new.slots[o.ent(p, at)]),
            new.slots[o.ent(p, at)].value() == o.slots[o.ent(p, top)].value(),
        ensures
            (CachePerms { toks: *final(toks), ..new }).tokened(),
            served.id() == slot_loc(o.cls[p]),
            served@ == plain(o.slots[o.ent(p, at)].value(), o.hdr(p).slots) as int,
    {
        let q = o.ent(p, at);
        let qt = o.ent(p, top);
        assert(cell_ok(p, o.hdrs[p], o.slots));
        o.lemma_tok_get(p, at);
        o.lemma_tok_get(p, top);
        let tracked served = toks.tracked_remove(q);
        if top != at {
            let tracked u = toks.tracked_remove(qt);
            toks.tracked_insert(q, u);
        }
        let r = CachePerms { toks: *toks, ..new };
        assert forall|p2: *mut CacheHeader, j: int|
            o.hdrs.contains_key(p2) && p2 != p && 0 <= j < o.hdr(p2).cap implies {
                let e = #[trigger] o.ent(p2, j);
                &&& r.slots[e] == o.slots[e]
                &&& (o.toks.contains_key(e) ==> r.toks.contains_key(e) && r.toks[e] == o.toks[e])
            } by {
            o.lemma_ent_apart(p, at, p2, j);
            o.lemma_ent_apart(p, top, p2, j);
        }
        assert forall|i: int| 0 <= i < r.hdr(p).len implies #[trigger] r.tok_at(p, i) by {
            assert(r.ent(p, i) == o.ent(p, i));
            if i != at {
                o.lemma_tok_get(p, i);
                o.lemma_ent_apart(p, i, p, at);
                o.lemma_ent_apart(p, i, p, top);
            }
        }
        CachePerms::lemma_tok_update(o, r, p);
        served
    }

    /// [`Heap::rebuild_run`]'s token step, from the facts its slices and
    /// moves leave: the old entries' tokens (`p1`) and the claim's (`ctoks`,
    /// of `claimed[..n]`) end up under the entries holding
    /// their slots, and nothing but the tokens changes.
    #[verifier::spinoff_prover]
    #[verifier::rlimit(50)]
    pub(super) proof fn lemma_rebuild_step(
        tracked perms: &mut CachePerms,
        tracked ctoks: &mut crate::class::SlotToks,
        p1: CachePerms,
        s2: Map<*mut *mut u8, PointsTo<*mut u8>>,
        p: *mut CacheHeader,
        len: int,
        want: int,
        n: int,
        claimed: Seq<*mut u8>,
        outf: Seq<*mut u8>,
        all0: Seq<*mut u8>,
        allf: Seq<*mut u8>,
    )
        requires
            p1.wf(),
            p1.tracks(),
            p1.tokened(),
            p1.hdrs.contains_key(p),
            regions_ok(),
            len == p1.hdr(p).len,
            0 < n <= want,
            len + want <= p1.hdr(p).cap,
            old(perms).toks == p1.toks,
            old(perms).hdrs == p1.hdrs.insert(p, old(perms).hdrs[p]),
            old(perms).cls == p1.cls,
            old(perms).hdr(p).slots == p1.hdr(p).slots,
            old(perms).hdr(p).len == len + n,
            claimed.len() == want,
            slots_in(claimed, p1.cls[p], 0, n),
            crate::class::keyed(old(ctoks).toks, crate::class::ptr_addrs(claimed.subrange(0, n)), slot_loc(p1.cls[p]), 0, n),
            outf.len() == want,
            forall|k: int| 0 <= k < n ==> (#[trigger] outf[k])@.addr == stored(claimed[n - 1 - k]@.addr, p1.hdr(p).slots),
            forall|k: int|
                0 <= k < p1.hdr(p).cap && !(len <= k < len + want)
                    ==> #[trigger] s2[p1.ent(p, k)] == p1.slots[p1.ent(p, k)],
            forall|i: int| len <= i < len + want ==> #[trigger] s2[slot_ptr(p1.hdr(p).slots, i)].is_init(),
            forall|i: int| len <= i < len + want ==> #[trigger] s2[slot_ptr(p1.hdr(p).slots, i)].value() == outf[i - len],
            all0.len() == len + n,
            allf.len() == len + n,
            forall|i: int|
                0 <= i < len + n && s2[slot_ptr(p1.hdr(p).slots, i)].is_init()
                    ==> all0[i] == #[trigger] s2[slot_ptr(p1.hdr(p).slots, i)].value(),
            len >= n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] allf[i] == if i < n { all0[len + i] } else if i < len { all0[i] } else { all0[i - len] },
            len < n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] allf[i] == if i < n { all0[len + i] } else { all0[i - n] },
            forall|i: int| 0 <= i < len + n ==> #[trigger] old(perms).slots[slot_ptr(p1.hdr(p).slots, i)].value() == allf[i],
            forall|p2: *mut CacheHeader, j: int|
                p1.hdrs.contains_key(p2) && p2 != p && 0 <= j < p1.hdr(p2).cap
                    ==> #[trigger] old(perms).slots[p1.ent(p2, j)] == p1.slots[p1.ent(p2, j)],
        ensures
            *final(perms) == (CachePerms { toks: final(perms).toks, ..*old(perms) }),
            final(perms).tokened(),
    {
        let slots = p1.hdr(p).slots;
        let c = p1.cls[p];
        let ca = crate::class::ptr_addrs(claimed.subrange(0, n));
        let np = *perms;
        assert(cell_ok(p, p1.hdrs[p], p1.slots));
        assert forall|i: int| 0 <= i < len implies plain(#[trigger] all0[i], slots) == plain(p1.slots[slot_ptr(slots, i)].value(), slots) by {
            assert(p1.ent(p, i) == slot_ptr(slots, i));
            assert(p1.slots[slot_ptr(slots, i)].is_init());
            assert(s2[p1.ent(p, i)] == p1.slots[p1.ent(p, i)]);
        }
        assert forall|k: int| 0 <= k < n implies #[trigger] ca[k] == plain(all0[len + n - 1 - k], slots) as int by {
            let j = n - 1 - k;
            assert(claimed.subrange(0, n)[k] == claimed[k]);
            assert(s2[slot_ptr(slots, len + j)].is_init());
            assert(s2[slot_ptr(slots, len + j)].value() == outf[j]);
            assert(all0[len + j] == outf[j]);
            assert((outf[j])@.addr == stored(claimed[n - 1 - j]@.addr, slots));
            assert(process_regions().is_slot(c, claimed[n - 1 - j]@.addr));
            lemma_plain_stored(outf[j], claimed[n - 1 - j], c, slots);
        }
        CachePerms::lemma_rebuild_tokens(&mut perms.toks, &mut ctoks.toks, p1, np, p, len, n, all0, allf, ca);
        assert(*perms == (CachePerms { toks: perms.toks, ..np }));
    }

    /// The run rebuild's token moves ([`Heap::rebuild_run`]): the old
    /// entries' tokens and the claim's (`ctoks`, key `k` for address
    /// `ca[k]`, the block reversed) are gathered by address and given back
    /// to the entries the moves left their slots in.
    #[verifier::spinoff_prover]
    #[verifier::rlimit(50)]
    pub(super) proof fn lemma_rebuild_tokens(
        tracked toks: &mut Map<*mut *mut u8, GhostSingleton<int>>,
        tracked ctoks: &mut Map<int, GhostSingleton<int>>,
        p1: CachePerms,
        np: CachePerms,
        p: *mut CacheHeader,
        len: int,
        n: int,
        all0: Seq<*mut u8>,
        allf: Seq<*mut u8>,
        ca: Seq<int>,
    )
        requires
            p1.wf(),
            p1.tracks(),
            p1.tokened(),
            p1.hdrs.contains_key(p),
            *old(toks) == p1.toks,
            np.hdrs == p1.hdrs.insert(p, np.hdrs[p]),
            np.cls == p1.cls,
            np.hdr(p).slots == p1.hdr(p).slots,
            np.hdr(p).len == len + n,
            len == p1.hdr(p).len,
            0 < n,
            len + n <= p1.hdr(p).cap,
            all0.len() == len + n,
            allf.len() == len + n,
            len >= n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] allf[i] == if i < n { all0[len + i] } else if i < len { all0[i] } else { all0[i - len] },
            len < n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] allf[i] == if i < n { all0[len + i] } else { all0[i - n] },
            forall|i: int| 0 <= i < len ==> plain(#[trigger] all0[i], p1.hdr(p).slots) == plain(p1.slots[slot_ptr(p1.hdr(p).slots, i)].value(), p1.hdr(p).slots),
            forall|k: int| 0 <= k < n ==> #[trigger] ca[k] == plain(all0[len + n - 1 - k], p1.hdr(p).slots) as int,
            forall|i: int| 0 <= i < len + n ==> #[trigger] np.slots[slot_ptr(p1.hdr(p).slots, i)].value() == allf[i],
            forall|p2: *mut CacheHeader, j: int|
                p1.hdrs.contains_key(p2) && p2 != p && 0 <= j < p1.hdr(p2).cap
                    ==> #[trigger] np.slots[p1.ent(p2, j)] == p1.slots[p1.ent(p2, j)],
            crate::class::keyed(*old(ctoks), ca, slot_loc(p1.cls[p]), 0, n),
        ensures
            (CachePerms { toks: *final(toks), ..np }).tokened(),
    {
        broadcast use crate::class::lemma_aset;
        let slots = p1.hdr(p).slots;
        let c = p1.cls[p];
        let f = |e: *mut u8| plain(e, slots) as int;
        let aold = Seq::new((len + n) as nat, |i: int| f(all0[i]));
        assert(cell_ok(p, p1.hdrs[p], p1.slots));
        assert forall|i: int| 0 <= i < len implies {
            let q = #[trigger] slot_ptr(slots, i);
            &&& toks.contains_key(q)
            &&& toks[q].id() == slot_loc(c)
            &&& toks[q]@ == aold[i]
        } by {
            p1.lemma_tok_get(p, i);
            assert(p1.ent(p, i) == slot_ptr(slots, i));
            assert(f(all0[i]) == plain(all0[i], slots) as int);
        }
        assert forall|i: int, j: int| 0 <= i < j < len + n implies slot_ptr(slots, i) != slot_ptr(slots, j) by {
            p1.lemma_ent_apart(p, i, p, j);
        }
        assert forall|k: int| 0 <= k < n implies #[trigger] ca[k] == aold[len + n - 1 - k] by {
        }
        let tracked mut bag = crate::class::gather_all(ctoks, ca, slot_loc(c), n);
        gather_ents(&mut bag, toks, slots, aold, 0, len);
        crate::class::lemma_aset_rev(aold, ca, len, n);
        crate::class::lemma_aset_split(aold, 0, len, len + n);
        crate::class::lemma_distinct_join(aold, 0, len, len + n);
        assert forall|i: int, j: int| 0 <= i < j < len + n implies f(all0[i]) != f(all0[j]) by {
            assert(aold[i] != aold[j]);
        }
        lemma_rebuild_distinct(all0, allf, len, n, f);
        let af = Seq::new((len + n) as nat, |i: int| f(allf[i]));
        assert forall|i: int, j: int| 0 <= i < j < len + n implies af[i] != af[j] by {
            assert(f(allf[i]) != f(allf[j]));
        }
        assert forall|v: int| crate::class::aset(af, 0, len + n).contains(v) implies bag@.contains(v) by {
            let i = choose|i: int| 0 <= i < len + n && af[i] == v;
            let k = rebuild_src(len, n, i);
            assert(allf[i] == all0[k]);
            assert(aold[k] == v);
        }
        let ghost t1 = *toks;
        scatter_ents(&mut bag, toks, slots, af, 0, len + n);
        let r = CachePerms { toks: *toks, ..np };
        assert forall|p2: *mut CacheHeader, j: int|
            p1.hdrs.contains_key(p2) && p2 != p && 0 <= j < p1.hdr(p2).cap implies {
                let q = #[trigger] p1.ent(p2, j);
                &&& r.slots[q] == p1.slots[q]
                &&& (p1.toks.contains_key(q) ==> r.toks.contains_key(q) && r.toks[q] == p1.toks[q])
            } by {
            let q = p1.ent(p2, j);
            if in_entries(slots, q, 0, len + n) {
                let k = choose|k: int| 0 <= k < len + n && q == slot_ptr(slots, k);
                p1.lemma_ent_apart(p, k, p2, j);
            }
            if in_entries(slots, q, 0, len) {
                let k = choose|k: int| 0 <= k < len && q == slot_ptr(slots, k);
                assert(in_entries(slots, q, 0, len + n));
            }
            assert(t1.contains_key(q) == p1.toks.contains_key(q));
            assert(r.toks.contains_key(q) == t1.contains_key(q));
        }
        assert forall|i: int| 0 <= i < r.hdr(p).len implies #[trigger] r.tok_at(p, i) by {
            assert(r.ent(p, i) == slot_ptr(slots, i));
        }
        CachePerms::lemma_tok_update(p1, r, p);
    }

}

} // verus!
