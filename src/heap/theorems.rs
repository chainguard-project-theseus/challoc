//! The block and region theorems (verus/THEOREMS.md): region
//! separation, guard pages, overflow reach and metadata integrity, and the
//! slot, free-target and size-hint predicates they are stated over. Ghost
//! code only; `heap` re-exports it.

use super::*;
use vstd::prelude::*;

verus! {

/// The process's regions are well formed and separated: a heap exists
/// only once the `Shared` is published, and [`Regions::from_parts`],
/// which builds its regions (`Regions::reserve_owning` calls it), ensures
/// both.
///
/// [`Regions::from_parts`]: crate::layout::Regions::from_parts
pub open spec fn regions_ok() -> bool {
    process_regions().wf() && process_regions().separated()
}

/// What an allocation of `size` bytes at `align` (a power of two) returns
/// (pointer authentication on or off): null, or a block of
/// the process's class or huge regions with at least `size` usable bytes
/// ([`Regions::user_block`](crate::layout::Regions::user_block)) whose
/// address is a multiple of `align`.
pub open spec fn fits(p: *mut u8, size: int, align: int) -> bool {
    p@.addr != 0 ==> {
        &&& regions_ok()
        &&& process_regions().user_block(p@.addr, size)
        &&& (p@.addr as int) % align == 0
    }
}

/// Region separation for an allocation's result: a non-null block that
/// [`fits`] lies in a class region or the huge region, whole (its `size`
/// bytes, at least one), never in a gap between class regions, and
/// neither its start nor any of its bytes is in the metadata region.
pub proof fn theorem_region_separation(p: *mut u8, size: int, align: int)
    requires
        fits(p, size, align),
        p@.addr != 0,
        size >= 0,
    ensures
        ({
            let r = process_regions();
            &&& (exists|c: int|
                0 <= c < NCLASSES && #[trigger] r.class_base[c] <= p@.addr && p@.addr + (if size < 1 { 1 } else { size }) <= r.class_base[c] + (1usize << r.lg_region))
                || (r.huge_base <= p@.addr && p@.addr + (if size < 1 { 1 } else { size }) <= r.huge_base + r.huge_len)
            &&& !(r.meta_base <= p@.addr && p@.addr < r.meta_base + r.meta_len)
            &&& (p@.addr + size <= r.meta_base || r.meta_base + r.meta_len <= p@.addr)
        }),
{
    process_regions().lemma_user_block_separated(p@.addr, size);
}

/// Guard pages are never handed out: no byte of a non-null block that
/// [`fits`] lies in a guard slab of any class region
/// ([`guard_slab`](crate::layout::guard_slab)), the slabs `SlabTable`
/// keeps for its guard pages (`MADV_GUARD_INSTALL` on the page group's
/// first page, or the whole slab or page group left `PROT_NONE`). A slab
/// block is a slot start ([`Regions::is_slot`]), which excludes them.
///
/// [`Regions::is_slot`]: crate::layout::Regions::is_slot
pub proof fn theorem_guard_pages(p: *mut u8, size: int, align: int, c: int, slab: int)
    requires
        fits(p, size, align),
        p@.addr != 0,
        size >= 0,
        0 <= c < NCLASSES,
        0 <= slab < process_regions().spec_nslabs(c),
        crate::layout::guard_slab(c, slab),
    ensures
        crate::layout::disjoint(
            p@.addr as int,
            if size < 1 { 1 } else { size },
            process_regions().slab_span(c, slab).0,
            process_regions().slab_span(c, slab).1,
        ),
{
    process_regions().lemma_user_block_not_guard(p@.addr, size, c, slab);
}

/// The free side of [`theorem_guard_pages`]: a [`free_target`] is no
/// address in a guard slab. A slot start is never in a guard slab
/// ([`Regions::is_slot`]), and a huge page start lies in the huge region,
/// apart from every class region.
///
/// [`Regions::is_slot`]: crate::layout::Regions::is_slot
pub proof fn theorem_guard_free(p: *mut u8, c: int, slab: int)
    requires
        regions_ok(),
        free_target(p),
        0 <= c < NCLASSES,
        0 <= slab < process_regions().spec_nslabs(c),
        crate::layout::guard_slab(c, slab),
    ensures
        crate::layout::disjoint(p@.addr as int, 1, process_regions().slab_span(c, slab).0, process_regions().slab_span(c, slab).1),
{
    let r = process_regions();
    if slot_start(p) {
        let c2 = choose|c2: int| #[trigger] r.is_slot(c2, p@.addr);
        assert(r.is_slot(c2, p@.addr) && 0 <= class_table()[c2].size);
    }
    assert(r.user_block(p@.addr, 0));
    r.lemma_user_block_not_guard(p@.addr, 0, c, slab);
}

/// Overflow reach: from any byte `a` of a non-null block an allocation
/// returned ([`fits`]), every byte `b` in the metadata region or the
/// process's `Shared` static, or in a class region or the huge region that
/// does not hold `a` ([`Regions::foreign`]), above `a` or below it, has a
/// whole `MAX_SLAB` (4 MiB) run of reservation slack
/// ([`Regions::slack_run`]: inside challoc's own `PROT_NONE`
/// reservations, in no region) between them. A linear overflow or
/// underflow off the block, or one striding less than 4 MiB at a time,
/// therefore touches a never-committed byte before it reaches those bytes.
/// The run is in `a`'s own reservation, so this holds however the kernel
/// placed the three reservations, adjacent ones included. Other allocator
/// state (the `api` statics outside `Shared`, the thread pointer's TLS
/// slot) is not named in [`Regions::foreign`].
///
/// [`Regions::foreign`]: crate::layout::Regions::foreign
/// [`Regions::slack_run`]: crate::layout::Regions::slack_run
pub proof fn theorem_overflow_reach(p: *mut u8, size: int, align: int, a: int, b: int)
    requires
        fits(p, size, align),
        p@.addr != 0,
        size >= 0,
        crate::api::ready(),
        p@.addr <= a < p@.addr + (if size < 1 { 1 } else { size }),
        process_regions().foreign(a, b),
    ensures
        exists|lo: int|
            ((a < lo && lo + crate::layout::MAX_SLAB <= b) || (b < lo && lo + crate::layout::MAX_SLAB <= a))
                && #[trigger] process_regions().slack_run(lo),
{
    let r = process_regions();
    shared::lemma_process_padded();
    r.lemma_user_block_separated(p@.addr, size);
    let n = if size < 1 { 1 } else { size };
    if !(r.huge_base <= p@.addr && p@.addr + n <= r.huge_base + r.huge_len) {
        let c = choose|c: int|
            0 <= c < NCLASSES && #[trigger] r.class_base[c] <= p@.addr && p@.addr + n <= r.class_base[c] + (1usize << r.lg_region);
        assert(r.in_class(c, a));
    }
    r.lemma_overflow_reach(a, b);
}

/// Metadata integrity: a non-null block an allocation returned
/// ([`fits`]) shares no byte with the metadata the allocator keeps in
/// the metadata region
/// ([`Regions::in_meta`](crate::layout::Regions::in_meta)), nor with the
/// process's `Shared` static. Each conjunct names one kind of it and who
/// writes it:
///
/// - Every range in the process's `Meta`'s region, the metadata region
///   ([`Meta::holds`](crate::meta::Meta::holds), which `Meta::alloc_raw`,
///   `alloc_zeroed`, `reserve`, `reserve_committed_raw` and `reserve_table`
///   ensure of their results; the other metadata conjuncts name parts of
///   it by kind): heaps, `others` nodes, cache stacks, huge descriptors
///   and tables, arenas, set tables, the background thread's stack, and
///   the reused object `Meta::alloc_zeroed` clears.
/// - The free-list link word of every free object in every value the
///   metadata lock admits ([`Meta::is_link`](crate::meta::Meta::is_link)),
///   the one word `Meta` writes outside what it hands out
///   (`Meta::lemma_link_held`, from `meta::Inner::lemma_links_in_region`).
/// - The process's `Shared` static (the lock values of `Meta`, the huge
///   allocator and the class tables, arena 0, the statistics), which lies
///   outside all three reservations ([`Regions::outside`](crate::layout::Regions::outside),
///   a conjunct of `Regions::separated`).
/// - Each class table's descriptors, owner words and bitmap words, at the
///   addresses `SlabTable::desc_addr`, `owner_addr` and `bit_word_addr`
///   give (`SlabTable::meta_ok`); bitmap words
///   when the table's bit count is a multiple of 64.
/// - Every well-formed heap's cache headers and stack entries, the memory
///   its pushes, pops, refills and flushes write through permissions
///   ([`Heap::lemma_caches_in_meta`]).
/// - Every live huge descriptor, descriptor table slot and free-page
///   bitmap word of a value the huge lock protects
///   (`huge::Inner::lemma_in_meta`, `TwoLevelBitmap::lemma_words_placed`).
///
/// Trusted premises: `Shared::wf` of the published value
/// (`api::shared_ref` and `api::shared_deref` prove it from `STATE`'s
/// invariant and `api::axiom_published`); the kernel's `mmap` contract for the three
/// reservations (`layout::regions::linux::try_reserve`,
/// `Reserved::is_disjoint` and `Reserved::is_apart_from_static`);
/// the `Unbacked` token for the metadata
/// region (`Reserved::into_unbacked`, handed to `Meta::owning`);
/// `vspec::ptr::axiom_ptr_add_addr`; the descriptor table's empty entries
/// (`huge::table::axiom_zero_entry`, on bytes `ZeroRaw::axiom_new`
/// pairs with their zero token); the trusted constructors
/// `TwoLevelBitmap::from_raw` and `from_raw_chunked` (`place`).
///
/// Not covered: descriptors and owner words are written
/// through `&` references to atomics, located by the accessors but not
/// tied to permissions; bitmap words are written through `WordPerm`s held
/// in the table's word invariants, whose addresses the theorem does not
/// tie to `bit_word_addr` (that needs a `class::table` lemma); the
/// last word of a bitmap whose bit count is not a multiple of 64; the
/// arenas' slab-set bitmaps. Thread-local storage is not apart from every
/// block: a thread whose stack the program allocated (`pthread_attr_setstack`
/// over a `malloc`ed buffer) has its static TLS block, and so challoc's
/// heap pointer slot, inside that buffer.
pub(crate) proof fn theorem_metadata_integrity(shared: &Shared, p: *mut u8, size: int, align: int)
    requires
        shared.wf(),
        fits(p, size, align),
        p@.addr != 0,
        size >= 0,
    ensures
        ({
            let n = if size < 1 { 1 } else { size };
            let b = p@.addr as int;
            &&& forall|a: int, len: int| len >= 0 && #[trigger] shared.meta.holds(a, len) ==> disjoint(b, n, a, len)
            &&& forall|x: int| #[trigger] shared.meta.is_link(x) ==> disjoint(b, n, x, 8)
            &&& disjoint(b, n, crate::layout::shared_static_addr(), crate::layout::shared_static_len())
            &&& forall|c: int, slab: int|
                0 <= c < NCLASSES && 0 <= slab < shared.tables[c].spec_nslabs() ==> disjoint(
                    b,
                    n,
                    #[trigger] shared.tables[c].desc_addr(slab),
                    shared.tables[c].spec_desc_stride(),
                )
            &&& forall|c: int, g: int|
                0 <= c < NCLASSES && 0 <= g && g * shared.tables[c].spec_group() < shared.tables[c].spec_nslabs()
                    ==> disjoint(b, n, #[trigger] shared.tables[c].owner_addr(g), 2)
            &&& forall|c: int, bit: int|
                0 <= c < NCLASSES && 0 <= bit < shared.tables[c].spec_nbits() && shared.tables[c].spec_nbits() % 64 == 0
                    ==> disjoint(b, n, #[trigger] shared.tables[c].bit_word_addr(bit), 8)
            &&& forall|h: Heap, q: *mut CacheHeader| h.wf() && #[trigger] h.owns(q) ==> disjoint(b, n, q@.addr as int, 64)
            &&& forall|h: Heap, q: *mut CacheHeader, i: int|
                h.wf() && h.owns(q) && 0 <= i < h.hdr_cap(q) ==> disjoint(b, n, #[trigger] h.entry_addr(q, i), 8)
            &&& forall|v: crate::huge::Inner, d: *mut crate::huge::HugeDesc|
                v.wf() && #[trigger] v.has_desc(d) ==> disjoint(b, n, d@.addr as int, 48)
            &&& forall|v: crate::huge::Inner, i: int|
                v.wf() && 0 <= i < v.table_cap() ==> disjoint(b, n, #[trigger] v.table_slot_addr(i), 16)
            &&& forall|v: crate::huge::Inner, j: int|
                v.wf() && 0 <= j < v.free_map().spec_l1_words() ==> disjoint(b, n, #[trigger] v.free_map().l1_addr(j), 8)
            &&& forall|v: crate::huge::Inner, w: int|
                v.wf() && 0 <= w < v.free_map().spec_l0_words() ==> disjoint(b, n, #[trigger] v.free_map().l0_addr(w), 8)
        }),
{
    let r = process_regions();
    let a = p@.addr;
    shared.lemma_meta();
    assert forall|x: int, len: int| len >= 0 && #[trigger] shared.meta.holds(x, len) implies disjoint(
        a as int,
        if size < 1 { 1 } else { size },
        x,
        len,
    ) by {
        r.lemma_user_block_apart(a, size, x, len);
    }
    assert forall|x: int| #[trigger] shared.meta.is_link(x) implies disjoint(a as int, if size < 1 { 1 } else { size }, x, 8) by {
        shared.meta.lemma_link_held(x);
        r.lemma_user_block_apart(a, size, x, 8);
    }
    r.lemma_user_block_outside(a, size, crate::layout::shared_static_addr(), crate::layout::shared_static_len());
    assert forall|c: int, slab: int| 0 <= c < NCLASSES && 0 <= slab < shared.tables[c].spec_nslabs() implies disjoint(
        a as int,
        if size < 1 { 1 } else { size },
        #[trigger] shared.tables[c].desc_addr(slab),
        shared.tables[c].spec_desc_stride(),
    ) by {
        shared.tables[c].lemma_desc_in_meta(slab);
        r.lemma_user_block_apart(a, size, shared.tables[c].desc_addr(slab), shared.tables[c].spec_desc_stride());
    }
    assert forall|c: int, g: int|
        0 <= c < NCLASSES && 0 <= g && g * shared.tables[c].spec_group() < shared.tables[c].spec_nslabs() implies disjoint(
        a as int,
        if size < 1 { 1 } else { size },
        #[trigger] shared.tables[c].owner_addr(g),
        2,
    ) by {
        shared.tables[c].lemma_owner_in_meta(g);
        r.lemma_user_block_apart(a, size, shared.tables[c].owner_addr(g), 2);
    }
    assert forall|c: int, bit: int|
        0 <= c < NCLASSES && 0 <= bit < shared.tables[c].spec_nbits() && shared.tables[c].spec_nbits() % 64 == 0 implies disjoint(
        a as int,
        if size < 1 { 1 } else { size },
        #[trigger] shared.tables[c].bit_word_addr(bit),
        8,
    ) by {
        shared.tables[c].lemma_bit_word_in_meta(bit);
        r.lemma_user_block_apart(a, size, shared.tables[c].bit_word_addr(bit), 8);
    }
    assert forall|h: Heap, q: *mut CacheHeader| h.wf() && #[trigger] h.owns(q) implies disjoint(
        a as int,
        if size < 1 { 1 } else { size },
        q@.addr as int,
        64,
    ) by {
        h.lemma_caches_in_meta(q, 0);
        r.lemma_user_block_apart(a, size, q@.addr as int, 64);
    }
    assert forall|h: Heap, q: *mut CacheHeader, i: int| h.wf() && h.owns(q) && 0 <= i < h.hdr_cap(q) implies disjoint(
        a as int,
        if size < 1 { 1 } else { size },
        #[trigger] h.entry_addr(q, i),
        8,
    ) by {
        h.lemma_caches_in_meta(q, i);
        r.lemma_user_block_apart(a, size, h.entry_addr(q, i), 8);
    }
    assert forall|v: crate::huge::Inner, d: *mut crate::huge::HugeDesc| v.wf() && #[trigger] v.has_desc(d) implies disjoint(
        a as int,
        if size < 1 { 1 } else { size },
        d@.addr as int,
        48,
    ) by {
        v.lemma_in_meta(d, 0);
        r.lemma_user_block_apart(a, size, d@.addr as int, 48);
    }
    assert forall|v: crate::huge::Inner, i: int| v.wf() && 0 <= i < v.table_cap() implies disjoint(
        a as int,
        if size < 1 { 1 } else { size },
        #[trigger] v.table_slot_addr(i),
        16,
    ) by {
        v.lemma_in_meta(ptr::null_mut(), i);
        r.lemma_user_block_apart(a, size, v.table_slot_addr(i), 16);
    }
    assert forall|v: crate::huge::Inner, j: int| v.wf() && 0 <= j < v.free_map().spec_l1_words() implies disjoint(
        a as int,
        if size < 1 { 1 } else { size },
        #[trigger] v.free_map().l1_addr(j),
        8,
    ) by {
        v.lemma_in_meta(ptr::null_mut(), 0);
        assert(r.wf());
        v.free_map().lemma_words_placed(j, 0);
        r.lemma_user_block_apart(a, size, v.free_map().l1_addr(j), 8);
    }
    assert forall|v: crate::huge::Inner, w: int| v.wf() && 0 <= w < v.free_map().spec_l0_words() implies disjoint(
        a as int,
        if size < 1 { 1 } else { size },
        #[trigger] v.free_map().l0_addr(w),
        8,
    ) by {
        v.lemma_in_meta(ptr::null_mut(), 0);
        assert(r.wf());
        v.free_map().lemma_words_placed(0, w);
        r.lemma_user_block_apart(a, size, v.free_map().l0_addr(w), 8);
    }
}

/// `p` is a slot start of some class in the process's regions: what every
/// cache push and bitmap release acts on.
pub open spec fn slot_start(p: *mut u8) -> bool {
    exists|c: int| #[trigger] process_regions().is_slot(c, p@.addr)
}

/// Where a free may act: a slot start of some class, or a page start in
/// the huge region, of the process's regions. Any other pointer (outside
/// every class region and the huge region, in a gap, or inside a slot or
/// a page) is an invalid free. A slot start is never in a guard slab
/// ([`Regions::is_slot`](crate::layout::Regions::is_slot)), so a pointer
/// into one is not a free target ([`theorem_guard_free`]).
///
/// Not stated: that the block is live. A slot start whose bit is clear
/// aborts on the exact paths (`SlabTable::check_located` and
/// `check_live` test the bit at run time); the sized fast path
/// (`sdallocx`, `free_sized`) pushes a slot start of the hinted class
/// without the bit test, and leaves a stale one to a flush, which catches
/// it only if the slot is still unclaimed then. A page start
/// that is not a live run start aborts in `Huge::free_run`. That a slot
/// start the program passes was allocated to it and not freed since is
/// the C caller's contract.
pub open spec fn free_target(p: *mut u8) -> bool {
    ||| slot_start(p)
    ||| process_regions().is_huge_block(p@.addr, 1)
}

/// A sized free's size hint names `p`'s class: if `p` is a slot start of
/// class `c`, `hint` is `Some(c)`.
/// `hint` is the class the size (and alignment) the program passed name,
/// `None` for a size no class holds or an alignment that is not a power
/// of two, so a slot freed with the size of another class, or with a huge
/// size, fails it. The sized frees test it of a slot before they push or
/// release it (`Shared::sized_slot` on the fast path, else
/// `Shared::check_size_hint`, which aborts); a pointer that is no slot
/// start passes it, and the exact free behind it judges the pointer.
pub open spec fn hint_ok(p: *mut u8, hint: Option<usize>) -> bool {
    forall|c: int| #[trigger] process_regions().is_slot(c, p@.addr) ==> hint == Some(c as usize)
}

/// The huge half of a sized free's check ([`hint_ok`] is the slot half):
/// for `p` a page start of the huge region and no slot start, with `blk`
/// the caller's token for it (the run's, [`Live::claims`]), `size`
/// rounded up to whole pages ([`huge_usable`]) is the run's usable size,
/// the run's `nallocx` for a size no class serves. Also the small size of
/// a run an alignment made huge, and any size `realloc` kept the run for.
/// `Shared::check_size_hint` aborts on a size that fails it.
/// With it, a live run's [`Live::lemma_block_usable`] gives
/// `block_usable(p, size) == blk.len()`, as a slot's holds for any size.
///
/// [`huge_usable`]: crate::vspec::zero::huge_usable
pub open spec fn huge_hint_ok(blk: Live, p: *mut u8, size: usize) -> bool {
    blk.claims(p) && !slot_start(p) && process_regions().is_huge_block(p@.addr, 1) ==> {
        &&& size + live::page_size() - 1 <= usize::MAX
        &&& crate::vspec::zero::huge_usable(size as int) == blk.len()
    }
}

/// A misaligned pointer is no free target: every slot start and every
/// huge page start is a multiple of the quantum, so a [`free_target`] is
/// one too.
pub proof fn theorem_free_target_aligned(p: *mut u8)
    requires
        regions_ok(),
        free_target(p),
    ensures
        (p@.addr as int) % (crate::QUANTUM as int) == 0,
{
    if slot_start(p) {
        let c = choose|c: int| #[trigger] process_regions().is_slot(c, p@.addr);
        crate::vspec::class::lemma_class_table_wf(c);
        lemma_slot_fits_quantum(p, c, 0);
    } else {
        lemma_quantum_pow2();
        crate::meta::lemma_is_pow2_one();
        lemma_huge_fits(p, 0, 1, crate::QUANTUM);
    }
}

/// No block at address 0: with the process's regions well formed and
/// separated, no slot start, no huge page start, no block an allocation
/// of `size` bytes may return
/// ([`Regions::user_block`](crate::layout::Regions::user_block)) and no
/// free target is null. So a NULL result is never a block, and NULL is
/// never a [`free_target`].
pub proof fn theorem_no_block_at_zero(p: *mut u8, c: int, size: int)
    requires
        regions_ok(),
    ensures
        process_regions().is_slot(c, p@.addr) ==> p@.addr != 0,
        process_regions().is_huge_block(p@.addr, 1) ==> p@.addr != 0,
        process_regions().user_block(p@.addr, size) ==> p@.addr != 0,
        free_target(p) ==> p@.addr != 0,
{
    let r = process_regions();
    r.lemma_block_nonzero(c, p@.addr, 1, size);
    if slot_start(p) {
        let c2 = choose|c2: int| #[trigger] r.is_slot(c2, p@.addr);
        r.lemma_block_nonzero(c2, p@.addr, 1, size);
    }
}

/// A slot of class `c` serves `size` bytes at `align` when the class is at
/// least `size` bytes and its size a multiple of `align`.
pub proof fn lemma_slot_fits(p: *mut u8, c: int, size: int, align: usize)
    requires
        regions_ok(),
        process_regions().is_slot(c, p@.addr),
        size <= class_table()[c].size,
        crate::meta::is_pow2(align),
        (class_table()[c].size as int) % (align as int) == 0,
    ensures
        process_regions().user_block(p@.addr, size),
        (p@.addr as int) % (align as int) == 0,
{
    process_regions().lemma_slot_aligned(c, p@.addr, align);
}

/// A slot of class `c` serves `malloc(size)` when the class is at least
/// `size` bytes: aligned to the quantum, as every class size is a multiple
/// of it.
pub proof fn lemma_slot_fits_quantum(p: *mut u8, c: int, size: int)
    requires
        regions_ok(),
        process_regions().is_slot(c, p@.addr),
        size <= class_table()[c].size,
    ensures
        process_regions().user_block(p@.addr, size),
        (p@.addr as int) % (crate::QUANTUM as int) == 0,
{
    crate::vspec::class::lemma_class_table_wf(c);
    crate::layout::lemma_class_size_quantum(c);
    lemma_quantum_pow2();
    lemma_slot_fits(p, c, size, crate::QUANTUM);
}

/// The quantum is a power of two, at most 4 KiB.
pub proof fn lemma_quantum_pow2()
    ensures
        crate::meta::is_pow2(crate::QUANTUM),
        crate::QUANTUM <= 0x1000,
{
    vstd::arithmetic::power2::lemma2_to64();
    if crate::QUANTUM == 16 {
        assert(crate::QUANTUM as nat == vstd::arithmetic::power2::pow2(4));
    } else {
        assert(crate::QUANTUM as nat == vstd::arithmetic::power2::pow2(3));
    }
}

/// A huge run placed for `size` bytes at `a1` serves `size` bytes at any
/// power of two `a2` that divides `a1` or a page (4 KiB or more).
pub proof fn lemma_huge_fits(p: *mut u8, size: usize, a1: usize, a2: usize)
    requires
        regions_ok(),
        crate::layout::huge_block(p@.addr, size, a1),
        crate::meta::is_pow2(a1),
        crate::meta::is_pow2(a2),
        a2 <= a1 || a2 <= 0x1000,
    ensures
        process_regions().user_block(p@.addr, size as int),
        (p@.addr as int) % (a2 as int) == 0,
{
    let r = process_regions();
    let k1 = choose|k: nat| k < 64 && a1 as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
    let k2 = choose|k: nat| k < 64 && a2 as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
    vstd::arithmetic::power2::lemma_pow2_pos(k1);
    vstd::arithmetic::power2::lemma_pow2_pos(k2);
    vstd::arithmetic::div_mod::lemma_mod_self_0(a2 as int);
    if a2 <= a1 {
        crate::layout::lemma_pow2_divides_pow2(a2, a2 as int, k1);
        crate::layout::lemma_aligned_down(p@.addr as int, a1 as int, a2 as int);
    } else {
        // The page is at least 4 KiB and divides `MAX_SLAB`, which the
        // huge base is a multiple of.
        let lg = r.lg_page;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg as usize);
        let page = 1usize << lg;
        vstd::arithmetic::power2::lemma2_to64();
        vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
        if lg > 12 {
            vstd::arithmetic::power2::lemma_pow2_strictly_increases(12, lg as nat);
        }
        crate::layout::lemma_pow2_divides_pow2(a2, a2 as int, lg as nat);
        if lg < 22 {
            vstd::arithmetic::power2::lemma_pow2_strictly_increases(lg as nat, 22);
        }
        crate::meta::lemma_is_pow2_shl(lg as usize);
        vstd::arithmetic::div_mod::lemma_mod_self_0(page as int);
        crate::layout::lemma_pow2_divides_pow2(page, page as int, 22);
        assert(crate::layout::MAX_SLAB == 0x40_0000) by (bit_vector);
        crate::layout::lemma_aligned_down(r.huge_base as int, crate::layout::MAX_SLAB as int, page as int);
        let d = p@.addr - r.huge_base;
        crate::layout::lemma_mod_lin3(r.huge_base as int, 1, d, 0, 0, page as int);
        crate::layout::lemma_aligned_down(p@.addr as int, page as int, a2 as int);
    }
}

/// Whether cache entries are stored signed: the `Shared` in the process's
/// storage has [`PUSH_PAC`] set. One value, since a
/// process publishes one `Shared` and `push_flags` is written once, before
/// the `Shared` is published. Opaque; `heap::shared`'s
/// `lemma_process_shared` reveals it. The unit tests build several with
/// different flags, so the definition stands for the shipped library only.
#[verifier::opaque]
pub open(crate) spec fn pac_mode() -> bool {
    Shared::built_at(Shared::storage()).push_flags & PUSH_PAC != 0
}

/// A slot start of class `c` has no other class, so the hint `Some(c)`
/// names its class.
pub proof fn lemma_hint_ok(p: *mut u8, c: int)
    requires
        regions_ok(),
        process_regions().is_slot(c, p@.addr),
        0 <= c,
    ensures
        hint_ok(p, Some(c as usize)),
{
    assert forall|d: int| #[trigger] process_regions().is_slot(d, p@.addr) implies Some(c as usize) == Some(d as usize) by {
        crate::vspec::zero::lemma_slot_unique(c, d, p@.addr);
    }
}

} // verus!
