//! Thread heaps: the per-thread caches in front of the slab bitmaps
//! (design.md section 6).
//!
//! Every thread that allocates gets a [`Heap`] in the metadata region,
//! reached through the one TLS slot in `sys/tls.rs`. A heap holds one
//! [`CacheHeader`] per size class inline, one 64-byte line each at
//! `heap + 128 + 64 * class`, so `malloc` reaches the header without
//! loading a pointer first. A header points at its stack of `cap` slot
//! pointers: slots marked allocated in their slab bitmaps but not live in
//! the program. The stack is a metadata object of its own, allocated on
//! the class's first slow path.
//!
//! - `malloc` pops the top of the class's stack. An empty stack is
//!   *refilled* with up to `cap / 2` slots claimed from the class's lowest
//!   partial slab, lowest address on top.
//! - `free` classifies the pointer, checks that its bitmap bit is set
//!   (invalid and stale double frees abort there) and pushes it. A full
//!   stack is *flushed*: its `cap / 2` highest addresses go back to their
//!   slabs in one batch grouped by slab.
//!
//! Neither fast path touches a slab descriptor, takes a lock or does an
//! atomic read-modify-write. Every header starts with `len == cap == 0`,
//! so an unused cache is simply both empty and full and the slow path
//! gives it a capacity and a stack. A thread without a heap (metadata
//! exhausted, inside heap creation, or past its exit destructor) is served
//! straight from the slab bitmaps; huge requests bypass the caches.
//!
//! On top of that: a compacting cache serves one pop in `2^COMPACT_SHIFT`
//! from a run of low addresses so a fragmented class drains its high slabs
//! (design 6.4); a heap keeps separate caches for other arenas and one
//! flush-only cache for frees of other arenas' slots (design 6.2, 6.3);
//! with pointer authentication every stack entry is stored signed with
//! the stack's base address as the modifier (`sys::pac`); and a
//! `pthread_key` destructor flushes and frees the heap at thread exit
//! (design 6.5).

mod cache;
mod compact;
mod flush;
pub mod live;
pub mod mallocx;
mod others;
mod paths;
mod shared;
mod tcaches;
mod thread;
mod zero;

#[cfg(test)]
mod linux_tests;
#[cfg(test)]
pub(crate) mod tests;

pub use cache::CacheHeader;
#[cfg(test)]
use compact::{
    COMPACT_MASK, COMPACT_MAX_REFILL_SLABS, COMPACT_MIN_SLABS, COMPACT_SHIFT, MODE_COMPACT,
    PROBE_MASK,
};
pub use compact::{MODE_OFF, MODE_PROBE};
pub use live::Live;
pub use others::ArenaCaches;
pub use paths::{
    alloc, alloc_declined, alloc_zeroed, free, free_direct, free_fast, free_fast_class,
    free_sized_fast,
};
#[cfg(verus_only)]
pub use shared::Built;
pub use shared::Shared;
#[cfg(test)]
use shared::{GuardTest, guard_interval_for};
pub use thread::{TLS_CREATING, TLS_EXITED, fast_flags};
pub use thread::{existing_heap, heap_destructor, heap_for_thread};

use core::alloc::Layout;
use core::ptr;

use crate::arena::Homed;
#[cfg(verus_only)]
use crate::arena::homed_on;
use crate::layout::{CLASSES, NCLASSES};
use crate::lock::LockOrder;
#[cfg(verus_only)]
use crate::lock::{rank_arenas, rank_class, rank_meta};

use others::{node_block_layout, node_caches_detached, node_set_arena};
pub(crate) use paths::shared_ptr;
use paths::{invalid_free, shared_of};
#[cfg(test)]
use shared::ncpus;
use shared::{now_ns, tid};

#[cfg(verus_only)]
use crate::class::slot_loc;
#[cfg(verus_only)]
use crate::class::{aset, distinct, ptr_addrs};
#[cfg(verus_only)]
use crate::layout::{class_table, disjoint, process_regions, slots_in};
use crate::vspec::zero::Zeroed;
use vstd::map::Map;
#[cfg(verus_only)]
use vstd::modes::tracked_swap;
#[cfg(verus_only)]
use vstd::prelude::int;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, verus};
use vstd::raw_ptr::{PointsTo, ptr_mut_ref, ptr_mut_write, ptr_ref};
use vstd::resource::set::GhostSingleton;
#[cfg(verus_only)]
use vstd::resource::set::GhostSubset;

/// Default cache capacity for a class: [`MAX_CAP`] (1024) slots for classes
/// up to 128 bytes, halved for every doubling of the class size, never
/// below [`MIN_CAP`].
#[cfg(test)]
pub const fn default_cap(class: usize) -> u32 {
    crate::config::ladder(class, MAX_CAP, 1)
}

verus! {


/// Largest default cache capacity (classes up to 128 bytes). 1024 rather
/// than jemalloc's 200: every flush or refill writes the descriptor lines of
/// the slabs it touches and invalidates them in every other core's cache,
/// and with one arena those slabs are shared, so the `free` check's bitmap
/// load misses in proportion to the flush rate. Doubling the capacity from
/// 256 to 512 and again to 1024 took larson from 225 to 243 to 252 M ops/s
/// and xmalloc-test from 201 to 390 to 590 M frees/s at 10 threads for
/// 8 MiB of larson RSS.
pub const MAX_CAP: u32 = 1024;
/// Smallest cache capacity.
pub const MIN_CAP: u32 = 8;
/// Slow-path entries between two periodic-work checks (design 6.4).
pub const PERIODIC_EVERY: u32 = 64;

/// The `others` key of the one flush-only cache per class that plain
/// frees of another arena's slots go onto ([`ArenaCaches`]); never an
/// arena index (`arena::MAX_ARENAS` is 4096).
pub const FOREIGN: u16 = u16::MAX;

/// `Shared::push_flags` bit: `double_free_scan` (compare the pushed slot
/// against the top four cache entries).
pub const PUSH_SCAN: u8 = 1;
/// `Shared::push_flags` bit: `zero_on_free` (clear the block before the
/// push or the huge release).
pub const PUSH_ZERO: u8 = 2;
/// `Shared::push_flags` bit: pointer authentication is on (`sys::pac`).
/// Read by the pop as well as the push: every slot
/// pointer in a cache's stack is stored signed (`pacda`, modifier the
/// stack's base address) and authenticated on the way out (`autda`), and
/// the heap pointer in TLS is signed with the thread pointer. Set by `init`
/// when the kernel exposes `HWCAP_PACA`/`PACG` and `pac_pointers` is not
/// `false`; never set on x86_64.
pub const PUSH_PAC: u8 = 4;

/// The process's `Shared` has [`PUSH_PAC`] set: cache entries are stored
/// signed ([`Shared::wf`], [`Heap::wf`]). One value, since a process
/// publishes one `Shared` and `push_flags` is written once, before the
/// `Shared` is published. The unit tests build several with different
/// flags, so the assumption is about the shipped library only.
pub uninterp spec fn pac_mode() -> bool;

/// The process's regions are well formed and separated (`Shared::wf`): a
/// heap exists only once the `Shared` is published, so every heap a
/// verified path is handed ([`Heap::wf`]) comes with them.
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

/// Region separation for an allocation's result (whole-allocator theorem
/// 1, `verus/TODO.md`): a non-null block `malloc`, `calloc`, `realloc`,
/// the aligned entry points, `mallocx`/`rallocx` or `operator new`
/// returned ([`fits`], their postcondition) lies in a class region or the
/// huge region, whole (its `size` bytes, at least one), never in a gap
/// between class regions, and neither its start nor any of its bytes is in
/// the metadata region.
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

/// Guard pages are never handed out (whole-allocator theorem 7,
/// `verus/THEOREMS.md`): no byte of a non-null block an allocation
/// returned ([`fits`]) lies in a guard slab of any class region
/// ([`guard_slab`](crate::layout::guard_slab)), the slabs `SlabTable`
/// keeps for its guard pages (`MADV_GUARD_INSTALL` on the page group's
/// first page, or the whole slab or page group left `PROT_NONE`).
/// Every slab path's result is a slot start ([`Regions::is_slot`]), which
/// excludes them: a claim takes slots only from slabs that are not guard
/// slabs (the class lock's `ready`), and the free paths push only slots
/// whose slab is not one (theorem 3, [`free_target`]).
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

/// The free side of theorem 7: a free that returns acted on no address in
/// a guard slab. Every free entry point ensures [`free_target`] of the
/// non-null pointer it returns for, so a pointer into a guard slab aborts
/// before any cache push or bitmap release (which require
/// [`Regions::is_slot`]): the exact path's bit test
/// (`SlabTable::check_live`, by name in `guard_free` for a handed-out
/// guard slab, whose bits are never set), the fast path's
/// (`Shared::checked_slot`, which then falls back to the exact path), or
/// the sized path's guard test (`Shared::sized_slot`, then
/// `check_size_hint` and the exact path).
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

/// Metadata integrity (whole-allocator theorem 4, `verus/TODO.md`): a
/// non-null block an allocation returned ([`fits`]) shares no byte with
/// the metadata the allocator keeps in the metadata region
/// ([`Regions::in_meta`](crate::layout::Regions::in_meta)), nor with the
/// process's `Shared` static. Each conjunct names one kind of it and who
/// writes it:
///
/// - Every range the process's `Meta` hands out ([`Meta::holds`](crate::meta::Meta::holds),
///   which `Meta::alloc`, `alloc_zeroed`, `reserve`, `reserve_committed`
///   and `reserve_table` ensure of their results): heaps, `others` nodes,
///   cache stacks, huge descriptors and tables, arenas, set tables,
///   the background thread's stack, and the reused object
///   `Meta::alloc_zeroed` clears.
/// - The free-list link word of every free object in every value the
///   metadata lock admits ([`Meta::is_link`](crate::meta::Meta::is_link)),
///   the one word `Meta` writes outside what it hands out
///   (`Meta::lemma_link_held`, from `meta::Inner::lemma_links_in_region`).
/// - The process's `Shared` static (the lock values of `Meta`, the huge
///   allocator and the class tables, arena 0, the statistics), which lies
///   outside all three reservations ([`Regions::outside`](crate::layout::Regions::outside),
///   a conjunct of `Regions::separated`).
/// - Each class table's descriptors, owner words and bitmap words, at the
///   addresses its trusted accessors compute (`SlabTable::desc_addr`,
///   `owner_addr`, `bit_word_addr`; `SlabTable::meta_ok`); bitmap words
///   when the table's bit count is a multiple of 64.
/// - Every well-formed heap's cache headers and stack entries, the memory
///   its pushes, pops, refills and flushes write through permissions
///   ([`Heap::lemma_caches_in_meta`]).
/// - Every live huge descriptor, descriptor table slot and free-page
///   bitmap word of a value the huge lock protects
///   (`huge::Inner::lemma_in_meta`, `TwoLevelBitmap::lemma_words_placed`).
///
/// Trusted premises: `Shared::wf` of the published value
/// (`api::shared_unchecked`; `api::shared_ref` proves it from `STATE`'s
/// invariant); the kernel's `mmap` contract for the three
/// reservations (`layout::regions::linux::try_reserve`,
/// `Reserved::is_disjoint` and `Reserved::is_apart_from_static`);
/// `Meta`'s lock axioms (`vspec::meta::axiom_meta_lock_inv`,
/// `axiom_meta_lock_frame`) and the `Unbacked` token for the metadata
/// region (`Reserved::into_unbacked`, handed to `Meta::owning`); the
/// class tables' trusted accessors' address arithmetic;
/// `vspec::ptr::axiom_ptr_add_addr`; the trusted constructors
/// `TwoLevelBitmap::from_raw` and `from_raw_chunked` (`place`).
///
/// Not covered (`verus/TODO.md`): descriptors and owner words are written
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

/// Where a free may act (whole-allocator theorem 3, `verus/TODO.md`): a
/// slot start of some class, or a page start in the huge region, of the
/// process's regions. `free`, `free_sized`, `free_aligned_sized`,
/// `realloc`, `reallocarray`, the `_usable` entry points, `dallocx`,
/// `sdallocx`, `rallocx` and the `operator delete` family ensure it of
/// every non-null pointer they return for (`reallocarray` only when
/// `n * m` does not overflow; an overflow returns null before `p` is
/// looked at): any other pointer (outside every class region and the
/// huge region, in a gap, or inside a slot or a page) aborts. The
/// metadata writers the free paths reach require it before they write:
/// the cache pushes ([`slot_start`] on
/// `Heap::push_cache`, the slot of its class on `Heap::push_fast`, `push`,
/// `free_slow`, `free_other`, `free_slow_in`), the
/// bitmap release (`Shared::free_direct`) and `zero_slot`; the huge
/// release (`Shared::free_huge`) takes the page index that
/// `Shared::classify_free` computed from an exact page start. So a
/// pointer that is not a free target reaches no cache, bitmap or huge
/// table write before the abort. It may reach one other metadata write:
/// a thread's first `free` creates the thread's heap (`heap_for_thread`)
/// before it classifies the pointer. A slot start is never in a guard
/// slab ([`Regions::is_slot`](crate::layout::Regions::is_slot)), so a
/// pointer into one aborts too ([`theorem_guard_free`]).
///
/// Not proved: that the block is live. A slot start whose bit is clear
/// aborts on the exact paths (`SlabTable::check_located` and
/// `check_live` test the bit at run time); the sized fast path
/// (`sdallocx`, `free_sized`) pushes a slot start of the hinted class
/// without the bit test, and the flush catches a stale one. A page start
/// that is not a live run start aborts in `Huge::free_run`. That a slot
/// start the program passes was allocated to it and not freed since is
/// the C caller's contract.
pub open spec fn free_target(p: *mut u8) -> bool {
    ||| slot_start(p)
    ||| process_regions().is_huge_block(p@.addr, 1)
}

/// A sized free's size hint names `p`'s class (`verus/THEOREMS.md`, "Bad
/// input"): if `p` is a slot start of class `c`, `hint` is `Some(c)`.
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

/// A misaligned pointer is no free target (`verus/THEOREMS.md`, "Bad
/// input"): every slot start and every huge page start is a multiple of
/// the quantum, so a free of a pointer that is not aborts (theorem 3).
pub proof fn theorem_free_target_aligned(p: *mut u8)
    requires
        regions_ok(),
        free_target(p),
    ensures
        (p@.addr as int) % (crate::QUANTUM as int) == 0,
{
    if slot_start(p) {
        let c = choose|c: int| #[trigger] process_regions().is_slot(c, p@.addr);
        crate::vspec::class::axiom_class_table_wf(c);
        lemma_slot_fits_quantum(p, c, 0);
    } else {
        lemma_quantum_pow2();
        crate::meta::lemma_is_pow2_one();
        lemma_huge_fits(p, 0, 1, crate::QUANTUM);
    }
}

/// No block at address 0 (`verus/THEOREMS.md`): with the process's regions
/// well formed and separated, no slot start, no huge page start, no block
/// an allocation of `size` bytes may return
/// ([`Regions::user_block`](crate::layout::Regions::user_block)) and no
/// free target is null. So a NULL result is never a block, and a pointer
/// an entry point ensures [`free_target`] of is non-null: where that holds
/// unconditionally (`sallocx`, `xallocx`, `usable_size`), NULL aborts.
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
    crate::vspec::class::axiom_class_table_wf(c);
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

/// `out[0]` is the token of slot `p` of class `c` (`verus/TODO.md`,
/// "Uniqueness: slot tokens"), when `ok` (the caller's signed-entry
/// flag agrees with [`pac_mode`]; otherwise `p` is not the slot's address).
pub open spec fn slot_token(out: Map<int, GhostSingleton<int>>, c: int, ok: bool, p: *mut u8) -> bool {
    &&& out.contains_key(0)
    &&& out[0].id() == slot_loc(c)
    &&& ok ==> out[0]@ == p@.addr as int
}

/// Hand out slot `p` of class `c`: its slot token, `out[0]`
/// ([`slot_token`]), becomes the block's [`Live`] token in `blk`, which
/// the allocation paths return to their callers (`verus/THEOREMS.md`,
/// "Interface contract", gap 1).
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
spec fn popped(old: CachePerms, new: CachePerms, out: Map<int, GhostSingleton<int>>, cache: *mut CacheHeader, ok: bool, p: *mut u8) -> bool {
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

/// A slot start stored in the stack at `base` the way a push stores it
/// (signed with [`pac_mode`]) stands for itself.
pub proof fn lemma_plain_stored(e: *mut u8, p: *mut u8, c: int, base: *mut *mut u8)
    requires
        regions_ok(),
        process_regions().is_slot(c, p@.addr),
        e@.addr == stored(p@.addr, base),
    ensures
        plain(e, base) == p@.addr,
        process_regions().is_slot(c, plain(e, base)),
{
    process_regions().lemma_slot_below_user_va(c, p@.addr);
    crate::sys::pac::axiom_auth_sign(p@.addr, base@.addr);
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
/// [`Heap::owns_node`] carries the bound from the node's creation
/// (`Heap::other_caches`, from the block's permission) for as long as the
/// heap owns the node: a node block is freed only with its heap
/// (`destroy`).
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
/// what [`CachePerms::released`] records and [`Heap::held`] states.
pub open spec fn tagged(c: int, a: Seq<int>) -> Seq<(int, int)> {
    Seq::new(a.len(), |i: int| (c, a[i]))
}

/// The released log `new` is `old` with entries appended, and the entries
/// appended are the set `s` (whole-allocator theorem 10).
pub open spec fn log_adds(new: Seq<(int, int)>, old: Seq<(int, int)>, s: Set<(int, int)>) -> bool {
    &&& old.len() <= new.len()
    &&& new.subrange(0, old.len() as int) == old
    &&& new.subrange(old.len() as int, new.len() as int).to_set() == s
}

/// An unchanged log has nothing appended.
proof fn lemma_log_same(a: Seq<(int, int)>)
    ensures
        log_adds(a, a, Set::empty()),
{
    assert(a.subrange(0, a.len() as int) =~= a);
    assert(a.subrange(a.len() as int, a.len() as int).to_set() =~= Set::empty());
}

/// Appending `t` adds its entries.
proof fn lemma_log_append(a: Seq<(int, int)>, t: Seq<(int, int)>)
    ensures
        log_adds(a + t, a, t.to_set()),
{
    assert((a + t).subrange(0, a.len() as int) =~= a);
    assert((a + t).subrange(a.len() as int, (a + t).len() as int) =~= t);
}

/// Two appends add the union of what each added.
proof fn lemma_log_trans(a: Seq<(int, int)>, b: Seq<(int, int)>, c: Seq<(int, int)>, s1: Set<(int, int)>, s2: Set<(int, int)>)
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

/// One flush's split (whole-allocator theorem 10): the addresses a cache
/// held (`a`) are those it keeps (`k`) and those it released (`r`), and
/// none is both. Opaque, so the flush's large proof context carries one
/// fact rather than two set identities.
#[verifier::opaque]
spec fn split_ok(a: Seq<int>, k: Seq<int>, r: Seq<int>) -> bool {
    &&& aset(a, 0, a.len() as int) == aset(k, 0, k.len() as int) + aset(r, 0, r.len() as int)
    &&& aset(k, 0, k.len() as int).disjoint(aset(r, 0, r.len() as int))
}

/// A flush's sets of addresses lifted to `(class, address)` pairs: the
/// slots cached before (`a`) are those kept (`k`) and those released
/// (`r`), and none is both, so the released pairs are the cached ones the
/// cache no longer holds.
proof fn lemma_tagged_split(c: int, a: Seq<int>, k: Seq<int>, r: Seq<int>)
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
spec fn slot_ptr(base: *mut *mut u8, i: int) -> *mut *mut u8 {
    crate::vspec::ptr::ptr_add(base, i as usize)
}

/// Entry `i` of the stack at `base` lies inside the stack's allocation,
/// so `base.add(i)` is in bounds.
spec fn slot_in(base: *mut *mut u8, i: int) -> bool {
    crate::vspec::ptr::in_alloc(base, i * vstd::layout::size_of::<*mut u8>())
}

/// Ghost: the permissions for the cache memory of one heap (its home
/// headers, the headers of its `others` nodes and their stacks), keyed by
/// address.
///
/// `cls` and `owner` are ghost bookkeeping for what the caches hold
/// ([`CachePerms::tracks`]): the class each header caches, and for each
/// stack entry the header and index it is.
///
/// `toks` holds the slot tokens of the cached slots, by stack entry
/// ([`CachePerms::tokened`]; `verus/TODO.md`, "Uniqueness: slot tokens").
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct CachePerms {
    pub tracked hdrs: Map<*mut CacheHeader, PointsTo<CacheHeader>>,
    pub tracked slots: Map<*mut *mut u8, PointsTo<*mut u8>>,
    pub ghost cls: Map<*mut CacheHeader, vstd::prelude::int>,
    pub ghost owner: Map<*mut *mut u8, (*mut CacheHeader, vstd::prelude::int)>,
    pub tracked toks: Map<*mut *mut u8, GhostSingleton<vstd::prelude::int>>,
    /// The zero tokens of the headers' zero prefixes, keyed by stack entry
    /// ([`CachePerms::zero_ok`]).
    pub tracked zero: Map<*mut *mut u8, Zeroed>,
    /// The token of the slot the last pop from inside a zero prefix
    /// handed out, for its caller to take. A pop outside the prefix leaves
    /// it alone, so it can be stale: only a pop that reports zero proves it
    /// current ([`Heap::handed_ok`]), and only then is it taken.
    pub tracked handed: Option<Zeroed>,
    /// The slots this heap's flushes have handed back, in order, as
    /// `(class, address)` pairs: each went to `Shared::release_routed`
    /// with its token, which `release_routed` proves it took out of the
    /// bag it was handed (whole-allocator theorem 10, [`Heap::flush_all`]).
    /// Only [`Heap::flush`] appends to it (by inspection: no spec keeps it
    /// fixed elsewhere).
    pub ghost released: vstd::seq::Seq<(vstd::prelude::int, vstd::prelude::int)>,
}

/// The stack of header `h` is owned entry by entry: `cap` entries from
/// `slots`, the first `len` of them written.
spec fn stack_ok(h: CacheHeader, slots: Map<*mut *mut u8, PointsTo<*mut u8>>) -> bool {
    forall|i: int|
        0 <= i < h.cap ==> {
            let q = #[trigger] slot_ptr(h.slots, i);
            &&& slots.contains_key(q)
            &&& slots[q].ptr() == q
            &&& (i < h.len ==> slots[q].is_init())
            &&& slot_in(h.slots, i)
        }
}

/// Header `p` is owned through `hp`, satisfies [`CacheHeader::inv`],
/// owns its stack, and it and its stack are metadata.
spec fn cell_ok(p: *mut CacheHeader, hp: PointsTo<CacheHeader>, slots: Map<*mut *mut u8, PointsTo<*mut u8>>) -> bool {
    &&& hp.ptr() == p
    &&& hp.is_init()
    &&& hp.value().inv()
    &&& stack_ok(hp.value(), slots)
    &&& cell_in_meta(p, hp.value())
}

/// Header `p`, holding `h`, and once it has one its stack lie in the
/// metadata region (whole-allocator theorem 4): every header is part of a
/// heap or an `others` node and every stack a block, each from
/// `Shared::meta` ([`Heap::ensure_cache`] for stacks, `Heap::other_caches`
/// for nodes; the heap's own headers join the permissions in trusted
/// code, heap creation, which takes its block from it).
spec fn cell_in_meta(p: *mut CacheHeader, h: CacheHeader) -> bool {
    &&& process_regions().in_meta(p@.addr as int, 64)
    &&& h.cap != 0 ==> process_regions().in_meta(h.slots@.addr as int, 8 * h.cap)
}

/// `new` keeps every stack entry `old` owns, at the same address, and
/// every written one written.
spec fn slots_grow(old: Map<*mut *mut u8, PointsTo<*mut u8>>, new: Map<*mut *mut u8, PointsTo<*mut u8>>) -> bool {
    forall|q: *mut *mut u8|
        #[trigger] old.contains_key(q) ==> {
            &&& new.contains_key(q)
            &&& new[q].ptr() == old[q].ptr()
            &&& (old[q].is_init() ==> new[q].is_init())
        }
}

/// A header whose stack keeps its base and capacity and loses entries
/// from the top still owns its stack.
proof fn lemma_stack_shrink(old_h: CacheHeader, new_h: CacheHeader, slots: Map<*mut *mut u8, PointsTo<*mut u8>>)
    requires
        stack_ok(old_h, slots),
        new_h.slots == old_h.slots,
        new_h.cap == old_h.cap,
        new_h.len <= old_h.len,
    ensures
        stack_ok(new_h, slots),
{
    assert forall|i: int| 0 <= i < new_h.cap implies {
        let q = #[trigger] slot_ptr(new_h.slots, i);
        &&& slots.contains_key(q)
        &&& slots[q].ptr() == q
        &&& (i < new_h.len ==> slots[q].is_init())
        &&& slot_in(new_h.slots, i)
    } by {
        assert(slot_ptr(old_h.slots, i) == slot_ptr(new_h.slots, i));
    }
}

/// A stack stays owned when its entries' permissions only grow.
proof fn lemma_stack_grow(h: CacheHeader, old_s: Map<*mut *mut u8, PointsTo<*mut u8>>, new_s: Map<*mut *mut u8, PointsTo<*mut u8>>)
    requires
        stack_ok(h, old_s),
        slots_grow(old_s, new_s),
    ensures
        stack_ok(h, new_s),
{
    assert forall|i: int| 0 <= i < h.cap implies {
        let q = #[trigger] slot_ptr(h.slots, i);
        &&& new_s.contains_key(q)
        &&& new_s[q].ptr() == q
        &&& (i < h.len ==> new_s[q].is_init())
        &&& slot_in(h.slots, i)
    } by {
        assert(old_s.contains_key(slot_ptr(h.slots, i)));
    }
}

/// A push: the entry at the old length written, the length one more.
proof fn lemma_stack_push(
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
    assert forall|i: int| 0 <= i < new_h.cap implies {
        let q = #[trigger] slot_ptr(new_h.slots, i);
        &&& new_s.contains_key(q)
        &&& new_s[q].ptr() == q
        &&& (i < new_h.len ==> new_s[q].is_init())
        &&& slot_in(new_h.slots, i)
    } by {
        assert(old_s.contains_key(slot_ptr(old_h.slots, i)));
    }
}

/// A stack whose entries `start..end` were all written (a claim into a
/// slice over them, [`stack_mut`]) and whose length grew to at most `end`
/// from at least `start` still owns its stack.
proof fn lemma_stack_fill(
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
    assert forall|i: int| 0 <= i < new_h.cap implies {
        let q = #[trigger] slot_ptr(new_h.slots, i);
        &&& new_s.contains_key(q)
        &&& new_s[q].ptr() == q
        &&& (i < new_h.len ==> new_s[q].is_init())
        &&& slot_in(new_h.slots, i)
    } by {
        assert(old_s.contains_key(slot_ptr(old_h.slots, i)));
        if i < start {
            assert(old_s[slot_ptr(old_h.slots, i)].is_init());
        }
    }
}

/// The first `n` entries of `t` are entries of `s`: what a permutation of
/// `s` into `t` (a selection, a sort) leaves.
spec fn from_all(t: Seq<*mut u8>, s: Seq<*mut u8>, n: int) -> bool {
    forall|i: int| 0 <= i < n ==> s.contains(#[trigger] t[i])
}

/// Every entry of `s` is an entry of `s`.
proof fn lemma_from_refl(s: Seq<*mut u8>)
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
proof fn lemma_from_sub(t: Seq<*mut u8>, n: int, u: Seq<*mut u8>)
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
proof fn lemma_from_trans(s: Seq<*mut u8>, t: Seq<*mut u8>, u: Seq<*mut u8>, n: int)
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

/// The first `n` entries of `t` stand for slots of `c` in the stack at
/// `base` ([`plain`]) when every entry of `s` does and they are entries
/// of `s`.
proof fn lemma_from_slots(s: Seq<*mut u8>, t: Seq<*mut u8>, c: int, n: int, base: *mut *mut u8)
    requires
        forall|j: int| 0 <= j < s.len() ==> process_regions().is_slot(c, plain(#[trigger] s[j], base)),
        from_all(t, s, n),
    ensures
        forall|i: int| 0 <= i < n ==> process_regions().is_slot(c, plain(#[trigger] t[i], base)),
{
    assert forall|i: int| 0 <= i < n implies process_regions().is_slot(c, plain(#[trigger] t[i], base)) by {
        assert(s.contains(t[i]));
        let j = choose|j: int| 0 <= j < s.len() && s[j] == t[i];
        assert(process_regions().is_slot(c, plain(s[j], base)));
    }
}

/// What a push stores for the slot start `p` in the stack at `base`: `p`,
/// or with [`pac_mode`] `pac::sign` of it with the stack base as the
/// modifier.
pub open spec fn stored(p: usize, base: *mut *mut u8) -> usize {
    if pac_mode() { crate::sys::pac::spec_sign(p, base@.addr) } else { p }
}

/// The run rebuild's moves ([`Heap::rebuild_run`]: the block of `n`
/// swapped with the bottom `n` of the `len` old entries, or rotated under
/// them) leave the first `len + n` entries entries of what was there.
proof fn lemma_rebuild_moves(a: Seq<*mut u8>, b: Seq<*mut u8>, len: int, n: int)
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
spec fn rebuild_src(len: int, n: int, i: int) -> int {
    if len >= n {
        if i < n { len + i } else if i < len { i } else { i - len }
    } else {
        if i < n { len + i } else { i - n }
    }
}

/// The run rebuild's moves are a permutation of the first `len + n`
/// entries ([`rebuild_src`]): `f` stays injective on them.
#[verifier::spinoff_prover]
proof fn lemma_rebuild_distinct(a: Seq<*mut u8>, b: Seq<*mut u8>, len: int, n: int, f: spec_fn(*mut u8) -> int)
    requires
        a.len() == len + n,
        b.len() == len + n,
        0 <= len,
        0 <= n,
        len >= n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] b[i] == if i < n { a[len + i] } else if i < len { a[i] } else { a[i - len] },
        len < n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] b[i] == if i < n { a[len + i] } else { a[i - n] },
        forall|i: int, j: int| 0 <= i < j < len + n ==> f(a[i]) != f(a[j]),
    ensures
        forall|i: int, j: int| 0 <= i < j < len + n ==> f(b[i]) != f(b[j]),
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
proof fn lemma_rebuild_tracks(
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
    assert forall|j: int| 0 <= j < all0.len() implies process_regions().is_slot(c, plain(#[trigger] all0[j], slots)) by {
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


/// Stack entry `q` is one of entries `k..n` of the stack at `slots`.
spec fn in_entries(slots: *mut *mut u8, q: *mut *mut u8, k: int, n: int) -> bool {
    exists|i: int| k <= i < n && q == slot_ptr(slots, i)
}

/// Every stack entry but entries `0..n` of the stack at `slots` keeps its
/// token.
spec fn toks_kept(new: Map<*mut *mut u8, GhostSingleton<int>>, old: Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, n: int) -> bool {
    forall|q: *mut *mut u8| !in_entries(slots, q, 0, n) ==> {
        &&& #[trigger] new.contains_key(q) == old.contains_key(q)
        &&& old.contains_key(q) ==> new[q] == old[q]
    }
}

/// Entries `k..n` of the stack at `slots` take the tokens `from[n - 1 - i]`:
/// a claimed batch of `n` slots, reversed onto the stack.
#[verifier::spinoff_prover]
proof fn move_reversed(
    tracked from: &mut Map<int, GhostSingleton<int>>,
    tracked to: &mut Map<*mut *mut u8, GhostSingleton<int>>,
    slots: *mut *mut u8,
    n: int,
    k: int,
)
    requires
        0 <= k <= n,
        forall|i: int| k <= i < n ==> #[trigger] old(from).contains_key(n - 1 - i),
        forall|i: int, j: int| k <= i < j < n ==> slot_ptr(slots, i) != slot_ptr(slots, j),
    ensures
        forall|i: int| k <= i < n ==> {
            &&& final(to).contains_key(#[trigger] slot_ptr(slots, i))
            &&& final(to)[slot_ptr(slots, i)] == old(from)[n - 1 - i]
        },
        forall|q: *mut *mut u8| !in_entries(slots, q, k, n) ==> {
            &&& #[trigger] final(to).contains_key(q) == old(to).contains_key(q)
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
        assert forall|q: *mut *mut u8| !in_entries(slots, q, k, n) implies {
            &&& #[trigger] to.contains_key(q) == to0.contains_key(q)
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
spec fn keyed_ents(m: Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, a: Seq<int>, loc: vstd::resource::Loc, lo: int, hi: int) -> bool {
    forall|i: int| lo <= i < hi ==> {
        let q = #[trigger] slot_ptr(slots, i);
        &&& m.contains_key(q)
        &&& m[q].id() == loc
        &&& m[q]@ == a[i]
    }
}

/// Entries `lo..hi` of the stack at `slots` are distinct words.
spec fn ents_distinct(slots: *mut *mut u8, lo: int, hi: int) -> bool {
    forall|i: int, j: int| lo <= i < j < hi ==> slot_ptr(slots, i) != slot_ptr(slots, j)
}

/// Every key of `new` but entries `lo..hi` of the stack at `slots` is as
/// in `old`.
spec fn ents_frame(new: Map<*mut *mut u8, GhostSingleton<int>>, old: Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, lo: int, hi: int) -> bool {
    forall|q: *mut *mut u8| !in_entries(slots, q, lo, hi) ==> {
        &&& #[trigger] new.contains_key(q) == old.contains_key(q)
        &&& old.contains_key(q) ==> new[q] == old[q]
    }
}

/// Entry `i` of the stack at `slots` is not one of entries `lo..hi`
/// other than itself.
proof fn lemma_not_in_entries(slots: *mut *mut u8, i: int, lo: int, hi: int)
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
proof fn gather_ents(tracked bag: &mut GhostSubset<int>, tracked m: &mut Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, a: Seq<int>, lo: int, hi: int)
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
        assert forall|q: *mut *mut u8| !in_entries(slots, q, lo, hi) implies {
            &&& #[trigger] m.contains_key(q) == m0.contains_key(q)
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
proof fn gather_all_ents(tracked m: &mut Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, a: Seq<int>, loc: vstd::resource::Loc, hi: int) -> (tracked bag: GhostSubset<int>)
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
    assert forall|q: *mut *mut u8| !in_entries(slots, q, 0, hi) implies {
        &&& #[trigger] m.contains_key(q) == m0.contains_key(q)
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
proof fn scatter_ents(tracked bag: &mut GhostSubset<int>, tracked m: &mut Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, a: Seq<int>, lo: int, hi: int)
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
        assert forall|q: *mut *mut u8| !in_entries(slots, q, lo, hi) implies {
            &&& #[trigger] m.contains_key(q) == m0.contains_key(q)
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
/// `slots[start..start + len]` as a slice, for the slice operations of
/// the refill, the flush and the run rebuild (claims, selections, sorts,
/// reversals). The slice borrows the entries' permissions: every entry it
/// covers must be owned (not necessarily written: a refill claims into
/// unwritten ones), and when the borrow ends every entry it covered is
/// written (a slice holds values) and every permission keeps its address
/// and, if it was written, stays written. The slice starts out holding
/// the written entries' values, the entries end up holding the slice's
/// final values, and no other permission changes
/// ([`slice_mut_from_perms`](crate::vspec::slice::slice_mut_from_perms)
/// over the stack entries, keyed by [`slot_ptr`]).
///
/// # Safety
/// The entries are this heap's stack storage and nothing else reads or
/// writes them while the slice lives.
#[inline(always)]
#[allow(clippy::needless_lifetimes, reason = "the permission and the slice share the lifetime in the Verus signature")]
unsafe fn stack_mut<'a>(
    slots: *mut *mut u8,
    start: usize,
    len: usize,
    Tracked(perms): Tracked<&'a mut Map<*mut *mut u8, PointsTo<*mut u8>>>,
) -> (r: &'a mut [*mut u8])
    requires
        forall|i: int| start <= i < start + len ==> {
            let q = #[trigger] slot_ptr(slots, i);
            old(perms).contains_key(q) && old(perms)[q].ptr() == q
        },
        len > 0,
    ensures
        r@.len() == len,
        final(r)@.len() == len,
        slots_grow(*old(perms), *final(perms)),
        forall|i: int| start <= i < start + len ==> #[trigger] final(perms)[slot_ptr(slots, i)].is_init(),
        forall|i: int|
            start <= i < start + len && old(perms)[slot_ptr(slots, i)].is_init()
                ==> r@[i - start] == #[trigger] old(perms)[slot_ptr(slots, i)].value(),
        forall|i: int| start <= i < start + len ==> #[trigger] final(perms)[slot_ptr(slots, i)].value() == final(r)@[i - start],
        forall|q: *mut *mut u8|
            old(perms).contains_key(q) && (forall|i: int| start <= i < start + len ==> q != #[trigger] slot_ptr(slots, i))
                ==> #[trigger] final(perms)[q] == old(perms)[q],
{
    proof {
        // The primitive's contract names the entries by `ptr_add`.
        assert forall|i: int| #[trigger] crate::vspec::ptr::ptr_add(slots, i as usize) == slot_ptr(slots, i) by {}
    }
    // SAFETY: guaranteed by the caller.
    unsafe { crate::vspec::slice::slice_mut_from_perms(slots, start, len, Tracked(perms)) }
}

/// [`Heap::handed_ok`] unfolded, for the heap's pops and their callers.
broadcast proof fn lemma_handed_ok(h: Heap, p: *mut u8, c: int)
    ensures
        #[trigger] h.handed_ok(p, c) == CachePerms::handed_ok(h.perms@.handed, p, c),
{
}

/// The `cap` entries of the fresh stack `slots` join `perms`, unwritten,
/// as entries `0..cap` of header `hdr` (`CachePerms::owner`).
///
/// Trusted: the entries are carved from `raw`, the permission to the
/// block's `8 * cap` bytes ([`CacheHeader::slots_layout`]) that
/// `Meta::alloc_raw` handed out with it, which the axiom uses up. So the
/// block is one this heap alone holds: an entry `perms` already owns
/// there would be a second permission to its bytes (it keeps its address
/// and value), and none is an entry of a stack an owned header still has
/// (`hdr`, whose capacity is 0, has none), so those keep their owners.
/// The `cap` entries lie inside the block, so `slots.add(i)` is in
/// bounds for each, and they are `cap` distinct words.
///
/// What is trusted is the carving into `CachePerms`' shape, not the
/// freshness: a call with a stack an owned header still has (whose two
/// owner clauses would contradict each other) or a second call for one
/// block needs a second permission to it, which `Meta` does not hand out
/// (`verus/audit/invariants.md`, `axiom_fresh_stack`). Its one call site
/// ([`Heap::ensure_cache`]) passes the block and permission
/// `Meta::alloc_raw` has just returned, for a header whose capacity is 0.
#[verifier::external_body]
#[allow(unused_variables)]
proof fn axiom_fresh_stack(
    tracked perms: &mut CachePerms,
    hdr: *mut CacheHeader,
    slots: *mut *mut u8,
    cap: u32,
    tracked raw: vstd::raw_ptr::PointsToRaw,
)
    requires
        slots@.addr != 0,
        old(perms).hdrs.contains_key(hdr),
        old(perms).hdr(hdr).cap == 0,
        raw.is_range(slots@.addr as int, 8 * cap),
        raw.provenance() == slots@.provenance,
    ensures
        final(perms).hdrs == old(perms).hdrs,
        final(perms).cls == old(perms).cls,
        slots_grow(old(perms).slots, final(perms).slots),
        forall|q: *mut *mut u8| #[trigger] old(perms).slots.contains_key(q) ==> final(perms).slots[q] == old(perms).slots[q],
        final(perms).toks == old(perms).toks,
        final(perms).zero == old(perms).zero,
        final(perms).handed == old(perms).handed,
        forall|i: int| 0 <= i < cap ==> {
            let q = #[trigger] slot_ptr(slots, i);
            &&& final(perms).slots.contains_key(q)
            &&& final(perms).slots[q].ptr() == q
            &&& slot_in(slots, i)
            &&& final(perms).owner.contains_key(q)
            &&& final(perms).owner[q] == (hdr, i)
        },
        forall|p: *mut CacheHeader, j: int|
            old(perms).hdrs.contains_key(p) && 0 <= j < old(perms).hdr(p).cap && old(perms).owner.contains_key(old(perms).ent(p, j)) ==> {
                let q = #[trigger] old(perms).ent(p, j);
                &&& final(perms).owner.contains_key(q)
                &&& final(perms).owner[q] == old(perms).owner[q]
            },
{
}

/// The entries of header `h`'s stack: `cap` words from `slots`
/// ([`Heap::free_caches_in`] takes them out of the permissions before it
/// frees the stack).
spec fn stack_set(h: CacheHeader) -> Set<*mut *mut u8> {
    vstd::set_lib::set_int_range(0, h.cap as int).map(|i: int| slot_ptr(h.slots, i))
}

/// What [`stack_set`] contains.
proof fn lemma_stack_set(h: CacheHeader, q: *mut *mut u8)
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

/// The `owner` of a stack entry after headers `a` and `b` trade values
/// ([`Heap::swap_hdrs`]): an entry of `a`'s stack is now `b`'s at the same
/// index, and the other way round.
spec fn swap_owner(
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

/// `assert_eq!(*len, 0, ..)`, the abort of [`Heap::free_caches`] and
/// [`Heap::free_caches_in`] on a cache that still holds slots: `*len == 0`
/// after it returns. Trusted: as
/// [`assert_refill_empty`].
#[verifier::external_body]
#[inline(always)]
fn assert_flushed(len: &u32)
    requires
        crate::sys::may_abort() || *len == 0,
    ensures
        *len == 0,
{
    assert_eq!(*len, 0, "heap: freeing a cache that was not flushed");
}

/// `assert_eq!(*len, 0, ..)`, [`Heap::refill`]'s abort on a heap
/// inconsistency: `*len == 0` after it returns, as for `vspec::check`.
///
/// Trusted: `assert_eq!` calls `core::panicking::assert_failed`, which
/// Verus cannot specify on stable Rust.
#[verifier::external_body]
#[inline(always)]
fn assert_refill_empty(len: &u32)
    requires
        crate::sys::may_abort() || *len == 0,
    ensures
        *len == 0,
{
    assert_eq!(*len, 0, "heap: refill of a cache that is not empty");
}

impl CachePerms {
    /// Every owned header is [`cell_ok`].
    spec fn wf(&self) -> bool {
        forall|p: *mut CacheHeader| #[trigger] self.hdrs.contains_key(p) ==> cell_ok(p, self.hdrs[p], self.slots)
    }

    /// `self` is well formed and owns every header `old` owned, and every
    /// header that had a stack still has the same one.
    spec fn keeps(&self, old: CachePerms) -> bool {
        &&& self.wf()
        &&& forall|p: *mut CacheHeader| #[trigger] old.hdrs.contains_key(p) ==> self.hdrs.contains_key(p)
        &&& forall|p: *mut CacheHeader| #[trigger] old.hdrs.contains_key(p) && old.hdr(p).cap != 0 ==> {
            &&& self.hdr(p).cap == old.hdr(p).cap
            &&& self.hdr(p).slots == old.hdr(p).slots
        }
    }

    /// The value of owned header `p`.
    spec fn hdr(&self, p: *mut CacheHeader) -> CacheHeader {
        self.hdrs[p].value()
    }

    /// Entry `i` of owned header `p`'s stack.
    spec fn ent(&self, p: *mut CacheHeader, i: int) -> *mut *mut u8 {
        slot_ptr(self.hdr(p).slots, i)
    }

    /// Stack entry `q` of the stack at `base` holds a slot start of class
    /// `c` in the process's regions: the word itself, or with
    /// [`pac_mode`] the word `pac::auth` gives back with the stack base as
    /// the modifier ([`plain`]).
    spec fn holds(&self, c: int, q: *mut *mut u8, base: *mut *mut u8) -> bool {
        process_regions().is_slot(c, plain(self.slots[q].value(), base))
    }

    /// What the caches hold: every entry of an owned header's stack is
    /// that header's alone (`owner` maps it to the header and its index,
    /// so no two stacks, and no two indices of one stack, share an
    /// entry), and each of the first `len` entries holds a slot start of
    /// the header's class (`cls`). What pops rely on and pushes keep.
    spec fn tracks(&self) -> bool {
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
    spec fn tok_at(&self, p: *mut CacheHeader, i: int) -> bool {
        let q = self.ent(p, i);
        &&& self.toks.contains_key(q)
        &&& self.toks[q].id() == slot_loc(self.cls[p])
        &&& self.toks[q]@ == plain(self.slots[q].value(), self.hdr(p).slots) as int
    }

    /// The slot tokens of what the caches hold (`verus/TODO.md`,
    /// "Uniqueness: slot tokens"): each of a header's first `len` entries
    /// holds the token of its slot ([`CachePerms::tok_at`]). A conjunct of
    /// [`Heap::wf`]; every verified cache writer keeps it.
    spec fn tokened(&self) -> bool {
        forall|p: *mut CacheHeader, i: int|
            #![trigger self.ent(p, i)]
            self.hdrs.contains_key(p) && 0 <= i < self.hdr(p).len ==> self.tok_at(p, i)
    }

    /// An entry of a cached slot holds its token.
    proof fn lemma_tok_get(&self, p: *mut CacheHeader, i: int)
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
    proof fn lemma_tok_update(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
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
    proof fn lemma_ent_apart(&self, p: *mut CacheHeader, i: int, p2: *mut CacheHeader, j: int)
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
    proof fn lemma_tokened_refill(old: CachePerms, new: CachePerms, p: *mut CacheHeader, n: int)
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
    proof fn lemma_toks_only(a: CachePerms, b: CachePerms, o: CachePerms)
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
    proof fn lemma_tokened_hdr(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
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
    proof fn lemma_tokened_pop(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
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
    proof fn lemma_tokened_push(old: CachePerms, new: CachePerms, p: *mut CacheHeader, t: GhostSingleton<vstd::prelude::int>)
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
    proof fn lemma_swap(o: CachePerms, n: CachePerms, a: *mut CacheHeader, b: *mut CacheHeader)
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

    /// Whole-allocator theorem 5, uniqueness (`verus/TODO.md`), for one
    /// heap's caches: two different cached entries of one class never
    /// stand for the same slot. Each holds its slot's token
    /// ([`CachePerms::tokened`], a conjunct of [`Heap::wf`]), and two
    /// tokens of one authority are disjoint.
    proof fn theorem_uniqueness_cached(tracked perms: &mut CachePerms, p1: *mut CacheHeader, i1: int, p2: *mut CacheHeader, i2: int)
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

    /// Whole-allocator theorem 5 against every other holder: a slot whose
    /// token `held` holds is cached nowhere in this heap. `held` stands for
    /// whatever else may hold tokens of the class's authority: the bitmap
    /// words' invariants (the free slots), another heap's caches, a claim
    /// in flight, or the slot `Heap::alloc_fast_tokens` handed out.
    proof fn theorem_uniqueness_held(tracked perms: &CachePerms, tracked held: &mut GhostSubset<int>, p: *mut CacheHeader, i: int)
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

    /// The compacting pop's token moves ([`Heap::pop_compact_run`]): entry
    /// `at` takes the top entry's slot and token, the served slot's token
    /// leaves, and every other entry keeps its own.
    #[verifier::spinoff_prover]
    proof fn lemma_tokened_compact(tracked toks: &mut Map<*mut *mut u8, GhostSingleton<int>>, o: CachePerms, new: CachePerms, p: *mut CacheHeader, at: int, top: int) -> (tracked served: GhostSingleton<int>)
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
    /// of `claimed[..n]` in table `t`) end up under the entries holding
    /// their slots, and nothing but the tokens changes.
    #[verifier::spinoff_prover]
    #[verifier::rlimit(50)]
    proof fn lemma_rebuild_step(
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
    proof fn lemma_rebuild_tokens(
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

    /// `self` [`tracks`](CachePerms::tracks) its contents and every header
    /// `old` owned still caches the class it did.
    spec fn follows(&self, old: CachePerms) -> bool {
        &&& self.tracks()
        &&& forall|p: *mut CacheHeader| #[trigger] old.hdrs.contains_key(p) ==> self.cls[p] == old.cls[p]
    }

    /// Unchanged permissions follow themselves.
    proof fn lemma_tracks_same(a: CachePerms)
        requires
            a.tracks(),
        ensures
            a.follows(a),
    {
    }

    /// A change to header `p` alone keeps the tracking of every other
    /// header, as long as their entries keep their owners and values;
    /// `p`'s own entries are the caller's to show.
    proof fn lemma_tracks_frame(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
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
    proof fn lemma_tracks_stack(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
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
    proof fn lemma_tracks_hdr(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
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
    proof fn lemma_tracks_put(old: CachePerms, new: CachePerms, p: *mut CacheHeader, i: int)
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
    proof fn lemma_slice_frame(
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
    proof fn lemma_update(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
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
                assert forall|i: int| 0 <= i < h.cap implies {
                    let s = #[trigger] slot_ptr(h.slots, i);
                    &&& new.slots.contains_key(s)
                    &&& new.slots[s].ptr() == s
                    &&& (i < h.len ==> new.slots[s].is_init())
                    &&& slot_in(h.slots, i)
                } by {
                    assert(old.slots.contains_key(slot_ptr(h.slots, i)));
                }
            }
        }
    }

    /// Well-formed permissions keep themselves.
    proof fn lemma_keeps_refl(a: CachePerms)
        requires
            a.wf(),
        ensures
            a.keeps(a),
    {
    }

    /// A header `old` does not own joining it unused (no stack, nothing
    /// cached), as a header of class `c`, keeps everything `old` had.
    proof fn lemma_insert_unused(old: CachePerms, new: CachePerms, p: *mut CacheHeader, c: int)
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
    proof fn lemma_write_grows(old: Map<*mut *mut u8, PointsTo<*mut u8>>, new: Map<*mut *mut u8, PointsTo<*mut u8>>, q: *mut *mut u8)
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


/// Per-thread byte counters (`thread.allocated`, `thread.deallocated`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadStats {
    pub allocated: u64,
    pub deallocated: u64,
}

impl ThreadStats {
    /// Add `n` operations of `size` bytes each.
    #[inline]
    fn add(&mut self, size: u64, allocs: u64, frees: u64)
        ensures
            final(self).allocated == old(self).allocated.wrapping_add(size.wrapping_mul(allocs)),
            final(self).deallocated == old(self).deallocated.wrapping_add(size.wrapping_mul(frees)),
    {
        self.allocated = self.allocated.wrapping_add(size.wrapping_mul(allocs));
        self.deallocated = self.deallocated.wrapping_add(size.wrapping_mul(frees));
    }
}


/// One thread's allocator state (design 6.2). Lives in the metadata region;
/// reached through the TLS slot. Also the shape of an explicit cache
/// (`tcache.create`), which is a heap owned by no thread. `repr(C)` so the
/// header array sits at a fixed offset the fast paths can add to the TLS
/// pointer.
#[repr(C)]
pub struct Heap {
    shared: *const Shared,
    tid: u32,
    /// Home arena: where plain `malloc` allocates from, and the arena whose
    /// caches are `caches` (design 6.2 `heap.arena`).
    arena: u16,
    slow_ticks: u32,
    last_gc: u64,
    /// Bytes counted per operation rather than derived from a cache
    /// header: huge runs, slots served directly from a slab (no cache, or
    /// `MALLOCX_TCACHE_NONE`). The thread caches are derived from their
    /// headers in [`Heap::stats`].
    uncached: ThreadStats,
    /// Caches of every other arena this heap has allocated from
    /// explicitly, and the [`FOREIGN`] node ([`ArenaCaches`]).
    others: *mut ArenaCaches,
    /// `thread.allocated` / `thread.deallocated` as of the last `mallctl`
    /// read of any `thread.*allocated*` key; `thread.allocatedp` and
    /// `thread.deallocatedp` point into it (see [`Heap::materialise_stats`]).
    snapshot: ThreadStats,
    /// `thread.tcache.enabled`: while false, `ensure_cache` creates no
    /// cache and the thread is served straight from the slab bitmaps.
    tcache_enabled: bool,
    /// Counted in `Arenas::bound` for its home arena: thread heaps are,
    /// explicit caches (`create_detached`) are not.
    bound: bool,
    /// `slot_randomize`'s xorshift state: 0 until the heap's first
    /// randomised claim seeds it from `getrandom`, so the
    /// default configuration makes no syscall per heap creation.
    rng: u64,
    /// Caches of the home arena, one line per class; last, so the small
    /// fields above share two lines and `caches[c]` is `heap + 128 + 64 * c`.
    caches: [CacheHeader; NCLASSES],
    /// Ghost ([`CachePerms`], no bytes): the permissions for `caches`, the
    /// `others` nodes' headers and their stacks ([`Heap::wf`]).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    perms: Tracked<CachePerms>,
    /// Ghost (no bytes): the `others` list, head first, as the nodes'
    /// `next` fields chain it ([`Heap::list_ok`]).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    list: Ghost<vstd::seq::Seq<*mut ArenaCaches>>,
    /// Ghost (no bytes): the `others` nodes' keys, `keys[i]` the `arena`
    /// field of `list[i]` ([`Heap::node_arena`]), pairwise distinct and
    /// never the home arena ([`Heap::list_ok`]).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    keys: Ghost<vstd::seq::Seq<u16>>,
    /// Ghost (no bytes): the home headers, one per class, where creation
    /// wrote them ([`home_hdr`]), as `create_heap_for_thread` records them
    /// ([`Heap::home_ok`]).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    home_hdrs: Ghost<vstd::seq::Seq<*mut CacheHeader>>,
    /// Ghost (no bytes): the heap's ticket for its home arena while it is
    /// counted there ([`Heap::homed_ok`]).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    homed: Tracked<Option<Homed>>,
    /// Ghost (no bytes): the permissions to the `arena` and `next` fields
    /// of each `others` node, which `other_caches` writes when it links
    /// the node in ([`others::NodeHead`], [`others::heads_ok`]).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    heads: Tracked<Map<*mut ArenaCaches, others::NodeHead>>,
}

} // verus!

const _: () = assert!(core::mem::offset_of!(Heap, caches) == 128);

verus! {

impl Heap {
    /// The heap's cache permissions are well formed ([`CachePerms::wf`]):
    /// every header it owns satisfies [`CacheHeader::inv`] and owns its
    /// stack; and they track what the caches hold
    /// ([`CachePerms::tracks`]): every stack entry belongs to one header
    /// and index, and each of a header's first `len` entries is a slot
    /// start of the header's class (unless entries are signed). Each of
    /// those entries holds its slot's token ([`CachePerms::tokened`],
    /// `verus/TODO.md`, "Uniqueness: slot tokens"). The permissions
    /// [`Heap::create_detached`] returns make a new heap well formed
    /// ([`CachePerms::fresh`]; a new thread heap is proved `wf` in
    /// `thread::create_heap_for_thread`, while `Tcaches::create` drops
    /// them, so an explicit cache is first assumed `wf`), and every
    /// writer of cache memory proves it keeps it: the pushes, pops,
    /// refills, flushes and rebuilds, the slices they borrow
    /// ([`stack_mut`], whose contract states every entry it changes), the
    /// `thread.arena` exchange ([`Heap::swap_hdrs`]) and the cache resets
    /// ([`Heap::free_caches_in`]). Assumed of every existing heap a
    /// verified path is handed (`tcaches::heap_at`,
    /// `thread::heap_from_slot`): the heap behind a TLS slot or a
    /// `tcache` id is as the last verified writer left it, since no ghost
    /// state survives between calls. `destroy` (trusted) frees the heap,
    /// so it claims nothing after. The `others` list is [`Heap::list`]
    /// ([`Heap::list_ok`]): creation starts it empty, `other_caches`
    /// links nodes in, and `destroy` frees the heap. A heap exists only
    /// once the process's `Shared` is published, so its regions come with
    /// it ([`regions_ok`]). The home headers are the ones
    /// `create_heap_for_thread` recorded ([`Heap::home_ok`]). They are
    /// also the plain field `caches`, whose value Verus models apart from
    /// the permissions to the same bytes: verified code only reads the
    /// field (`fold_counters`, `stats`), and only `destroy`'s
    /// [`Heap::free_caches`] writes through it. A heap counted in its
    /// home arena's bound count holds its ticket ([`Heap::homed_ok`]).
    pub closed spec fn wf(&self) -> bool {
        &&& self.perms@.wf()
        &&& self.perms@.tracks()
        &&& self.perms@.zero_ok()
        &&& self.perms@.tokened()
        &&& regions_ok()
        &&& self.list_ok()
        &&& self.home_ok()
        &&& self.homed_ok()
    }

    /// The heap's ticket ([`Homed`]; `verus/THEOREMS.md`, "Balanced arena
    /// assignment"): a heap counted in `Arenas::bound` (`bound`: a thread
    /// heap, from `create` to `destroy`) holds the ticket for its home
    /// arena, and a heap that is not counted holds none. `create` installs
    /// it, `set_arena` exchanges it with the binding, and `destroy`
    /// (trusted) gives it back with the binding.
    pub closed spec fn homed_ok(&self) -> bool {
        &&& self.bound == self.homed@ is Some
        &&& self.homed@ matches Some(t) ==> homed_on(t, self.arena as int)
    }

    /// `self` is well formed, owns every header `old` owned, and every
    /// header that had a stack still has the same one; its `others` list
    /// is `old`'s with nodes added at the head, if any (their keys with
    /// them), and its home headers and home arena are `old`'s, with its
    /// ticket ([`Heap::homed_ok`]).
    pub closed spec fn keeps(&self, old: Heap) -> bool {
        &&& self.perms@.keeps(old.perms@)
        &&& self.perms@.follows(old.perms@)
        &&& self.perms@.zero_ok()
        &&& self.perms@.tokened()
        &&& regions_ok()
        &&& self.list_ok()
        &&& self.home_ok()
        &&& self.home_hdrs == old.home_hdrs
        &&& list_grows(self.list@, old.list@)
        &&& list_grows(self.keys@, old.keys@)
        &&& self.arena == old.arena
        &&& self.bound == old.bound
        &&& self.homed == old.homed
        &&& self.homed_ok()
    }

    /// The home headers ([`Heap::home_hdrs`]): one per class, owned, caching
    /// that class, and none a header of a node on the `others` list (each
    /// node lives in a metadata block of its own, `Heap::other_caches`).
    pub closed spec fn home_ok(&self) -> bool {
        &&& self.home_hdrs@.len() == NCLASSES
        &&& forall|c: int| 0 <= c < NCLASSES ==> self.owns(#[trigger] self.home_hdrs@[c]) && self.hdr_class(self.home_hdrs@[c]) == c
        &&& forall|c: int, i: int, k: int|
            0 <= c < NCLASSES && 0 <= i < self.list@.len() && 0 <= k < NCLASSES
                ==> #[trigger] self.home_hdrs@[c] != #[trigger] node_hdr(self.list@[i], k)
    }

    /// The home header for `class` ([`Heap::home_ok`]).
    pub closed spec fn home_ptr(&self, class: int) -> *mut CacheHeader {
        self.home_hdrs@[class]
    }

    /// [`Heap::home_ok`] survives a change that keeps the home headers'
    /// record, the headers themselves and their classes, and the
    /// `others` list.
    proof fn lemma_home_kept(old: Heap, new: Heap)
        requires
            old.home_ok(),
            new.home_hdrs == old.home_hdrs,
            new.list == old.list,
            new.keys == old.keys,
            new.arena == old.arena,
            forall|p: *mut CacheHeader| #[trigger] old.owns(p) ==> new.owns(p) && new.hdr_class(p) == old.hdr_class(p),
        ensures
            new.home_ok(),
    {
        assert forall|c: int| 0 <= c < NCLASSES implies new.owns(#[trigger] new.home_hdrs@[c]) && new.hdr_class(new.home_hdrs@[c]) == c by {
            assert(old.owns(old.home_hdrs@[c]));
        }
    }

    /// The `others` list as [`Heap::list`] has it: `others` heads it (null
    /// when it is empty), and its nodes are distinct, non-null and owned
    /// ([`Heap::owns_node`]). Each node's `next` is the one after it
    /// (null after the last) and its `arena` is its key, through the
    /// permissions to those fields the heap holds ([`Heap::heads`],
    /// [`others::heads_ok`]), which [`Heap::node_next`] and
    /// [`Heap::node_arena`] read through. The
    /// nodes' keys ([`Heap::keys`]: the arena a node caches, or
    /// [`FOREIGN`]) are pairwise distinct and none is the home arena
    /// (`verus/THEOREMS.md`, "Cache routing"): `other_caches` creates a
    /// node only for a key its walk did not find and never for the home
    /// arena, and `set_arena`'s exchange gives the new home's node the
    /// old home as its key.
    pub closed spec fn list_ok(&self) -> bool {
        let l = self.list@;
        let k = self.keys@;
        &&& l.len() == 0 ==> self.others@.addr == 0
        &&& l.len() > 0 ==> self.others == l[0]
        &&& forall|i: int| 0 <= i < l.len() ==> (#[trigger] l[i])@.addr != 0 && self.owns_node(l[i])
        &&& forall|i: int, j: int| 0 <= i < j < l.len() ==> l[i] != l[j]
        &&& k.len() == l.len()
        &&& forall|i: int, j: int| 0 <= i < j < k.len() ==> k[i] != k[j]
        &&& forall|i: int| 0 <= i < k.len() ==> #[trigger] k[i] != self.arena
        &&& others::heads_ok(self.heads@, l, k)
    }

    /// `n` is a node of the `others` list ([`Heap::list`]).
    pub closed spec fn in_list(&self, n: *mut ArenaCaches) -> bool {
        self.list@.contains(n)
    }

    /// The node `r` places from the end of the list is on it.
    proof fn lemma_in_list(&self, r: int)
        requires
            1 <= r <= self.list_len(),
        ensures
            self.in_list(self.node_from_end(r)),
    {
        assert(self.list@[self.list@.len() - r] == self.node_from_end(r));
    }

    /// A heap whose cache permissions are those creation returns
    /// ([`CachePerms::fresh`]), whose `others` list is empty and whose home
    /// headers are where creation wrote them at `h` is well formed, once
    /// the process's `Shared` is ([`regions_ok`]); a thread heap, counted
    /// and holding its ticket ([`Heap::homed_ok`]).
    proof fn lemma_fresh_wf(&self, h: *mut Heap)
        requires
            self.perms@.fresh(h),
            self.list@.len() == 0,
            self.keys@.len() == 0,
            self.others@.addr == 0,
            self.home_hdrs@ == Seq::new(NCLASSES as nat, |c: int| home_hdr(h, c)),
            regions_ok(),
            self.bound,
            self.homed@ matches Some(t) && homed_on(t, self.arena as int),
        ensures
            self.wf(),
    {
        reveal(others::heads_ok);
        assert forall|c: int| 0 <= c < NCLASSES implies self.owns(#[trigger] self.home_hdrs@[c]) && self.hdr_class(self.home_hdrs@[c]) == c by {
            assert(self.home_hdrs@[c] == home_hdr(h, c));
        }
    }

    /// The home arena ([`Heap::arena`]).
    pub closed spec fn home(&self) -> u16 {
        self.arena
    }

    /// The length of the `others` list ([`Heap::list`]).
    pub closed spec fn list_len(&self) -> int {
        self.list@.len() as int
    }

    /// The node `r` places from the end of the `others` list (`r` counts
    /// from 1), which nodes added at the head do not move.
    pub closed spec fn node_from_end(&self, r: int) -> *mut ArenaCaches {
        self.list@[self.list@.len() - r]
    }

    /// The key of the node `r` places from the end of the `others` list
    /// ([`Heap::keys`]): the arena it caches, or [`FOREIGN`].
    pub closed spec fn node_key(&self, r: int) -> u16 {
        self.keys@[self.keys@.len() - r]
    }

    /// `node` is on the `others` list, with key `arena`.
    pub closed spec fn keyed(&self, node: *mut ArenaCaches, arena: u16) -> bool {
        exists|i: int| 0 <= i < self.list@.len() && self.list@[i] == node && #[trigger] self.keys@[i] == arena
    }

    /// Some node of the `others` list has key `arena`.
    pub closed spec fn has_key(&self, arena: u16) -> bool {
        exists|i: int| 0 <= i < self.keys@.len() && #[trigger] self.keys@[i] == arena
    }

    /// Cache routing: `cache` is the header for `class` of `arena`'s cache
    /// in this heap, the home header for the home arena, else the node
    /// keyed `arena`'s.
    pub closed spec fn routes(&self, arena: u16, class: int, cache: *mut CacheHeader) -> bool {
        if arena == self.arena {
            cache == self.home_ptr(class)
        } else {
            exists|i: int| 0 <= i < self.list@.len() && #[trigger] self.keys@[i] == arena && cache == node_hdr(self.list@[i], class)
        }
    }

    /// A header a pop may take from (`verus/THEOREMS.md`, "Cache
    /// routing"): the header for `class` of the home arena or of a node
    /// whose key is an arena, never [`FOREIGN`]. [`Heap::pop`],
    /// [`Heap::pop_compact`] and [`Heap::pop_compact_run`] require it; the
    /// fourth caller of [`Heap::pop_top`], [`Heap::pop_fast`], pops the
    /// home header.
    pub open spec fn poppable(&self, class: int, cache: *mut CacheHeader) -> bool {
        exists|a: u16| (a != FOREIGN || a == self.home()) && #[trigger] self.routes(a, class, cache)
    }

    /// A header routed for an arena that is not [`FOREIGN`], or for the
    /// home arena, may be popped.
    pub proof fn lemma_poppable(&self, arena: u16, class: int, cache: *mut CacheHeader)
        requires
            self.routes(arena, class, cache),
            arena != FOREIGN || arena == self.home(),
        ensures
            self.poppable(class, cache),
    {
    }

    /// The home header for `class` routes the home arena.
    pub proof fn lemma_routes_home(&self, class: int)
        ensures
            self.routes(self.home(), class, self.home_ptr(class)),
            self.poppable(class, self.home_ptr(class)),
    {
        self.lemma_poppable(self.home(), class, self.home_ptr(class));
    }

    /// A node on the `others` list with key `arena` holds the header
    /// `arena` routes to; its key is not the home arena.
    proof fn lemma_keyed_routes(&self, node: *mut ArenaCaches, arena: u16, class: int)
        requires
            self.wf(),
            self.keyed(node, arena),
        ensures
            arena != self.home(),
            self.routes(arena, class, node_hdr(node, class)),
    {
        let i = choose|i: int| 0 <= i < self.list@.len() && self.list@[i] == node && #[trigger] self.keys@[i] == arena;
        assert(self.keys@[i] != self.arena);
    }

    /// [`Heap::keeps`] keeps routing: the home arena and headers, and
    /// every node and key in its place counted from the end.
    proof fn lemma_keeps_routes(a: Heap, b: Heap, arena: u16, class: int, cache: *mut CacheHeader)
        requires
            a.list_ok(),
            b.keeps(a),
            a.routes(arena, class, cache),
        ensures
            b.routes(arena, class, cache),
    {
        if arena != a.arena {
            let i = choose|i: int| 0 <= i < a.list@.len() && #[trigger] a.keys@[i] == arena && cache == node_hdr(a.list@[i], class);
            let r = a.list@.len() - i;
            let j = b.list@.len() - r;
            assert(b.list@[b.list@.len() - r] == a.list@[a.list@.len() - r]);
            assert(b.keys@[b.keys@.len() - r] == a.keys@[a.keys@.len() - r]);
            assert(b.keys@[j] == arena && cache == node_hdr(b.list@[j], class));
        }
    }

    /// [`Heap::poppable`] depends only on the home arena and headers and
    /// the `others` list and keys, not on what the caches hold.
    proof fn lemma_poppable_same(a: Heap, b: Heap, class: int, cache: *mut CacheHeader)
        requires
            a.poppable(class, cache),
            b.arena == a.arena,
            b.home_hdrs == a.home_hdrs,
            b.list == a.list,
            b.keys == a.keys,
        ensures
            b.poppable(class, cache),
    {
        let x = choose|x: u16| (x != FOREIGN || x == a.home()) && #[trigger] a.routes(x, class, cache);
        b.lemma_poppable(x, class, cache);
    }

    /// [`Heap::keeps`] keeps [`Heap::poppable`].
    proof fn lemma_keeps_poppable(a: Heap, b: Heap, class: int, cache: *mut CacheHeader)
        requires
            a.list_ok(),
            b.keeps(a),
            a.poppable(class, cache),
        ensures
            b.poppable(class, cache),
    {
        let x = choose|x: u16| (x != FOREIGN || x == a.home()) && #[trigger] a.routes(x, class, cache);
        Heap::lemma_keeps_routes(a, b, x, class, cache);
        b.lemma_poppable(x, class, cache);
    }

    /// Cache routing (`verus/THEOREMS.md`, "Cache routing"), on a
    /// well-formed heap: the `others` keys are pairwise distinct and none
    /// is the home arena; a header is routed for at most one arena, so the
    /// pop sites' [`Heap::routes`] names the requested arena's cache and no
    /// other; and no header [`Heap::poppable`] admits is one of the
    /// [`FOREIGN`] node's, which therefore is never popped. Every pop
    /// requires [`Heap::poppable`] ([`Heap::pop`], [`Heap::pop_compact`],
    /// [`Heap::pop_compact_run`]; [`Heap::pop_fast`] pops the home
    /// header).
    pub proof fn theorem_cache_routing(&self, class: int, cache: *mut CacheHeader, a: u16, b: u16)
        requires
            self.wf(),
            0 <= class < NCLASSES,
        ensures
            forall|r: int, s: int| 1 <= r < s <= self.list_len() ==> self.node_key(r) != self.node_key(s),
            forall|r: int| 1 <= r <= self.list_len() ==> self.node_key(r) != self.home(),
            self.routes(a, class, cache) && self.routes(b, class, cache) ==> a == b,
            self.poppable(class, cache) ==> forall|r: int, k: int|
                1 <= r <= self.list_len() && self.node_key(r) == FOREIGN && 0 <= k < NCLASSES
                    ==> cache != node_hdr(self.node_from_end(r), k),
    {
        let n = self.list@.len();
        assert forall|r: int, s: int| 1 <= r < s <= self.list_len() implies self.node_key(r) != self.node_key(s) by {
            assert(self.keys@[n - s] != self.keys@[n - r]);
        }
        assert forall|r: int| 1 <= r <= self.list_len() implies self.node_key(r) != self.home() by {
            assert(self.keys@[n - r] != self.arena);
        }
        if self.routes(a, class, cache) && self.routes(b, class, cache) && a != b {
            if a == self.arena {
                let j = choose|j: int| 0 <= j < n && #[trigger] self.keys@[j] == b && cache == node_hdr(self.list@[j], class);
                self.lemma_home_apart(class, j, class);
            } else if b == self.arena {
                let i = choose|i: int| 0 <= i < n && #[trigger] self.keys@[i] == a && cache == node_hdr(self.list@[i], class);
                self.lemma_home_apart(class, i, class);
            } else {
                let i = choose|i: int| 0 <= i < n && #[trigger] self.keys@[i] == a && cache == node_hdr(self.list@[i], class);
                let j = choose|j: int| 0 <= j < n && #[trigger] self.keys@[j] == b && cache == node_hdr(self.list@[j], class);
                self.lemma_nodes_apart(i, j, class, class);
            }
        }
        if self.poppable(class, cache) {
            let x = choose|x: u16| (x != FOREIGN || x == self.home()) && #[trigger] self.routes(x, class, cache);
            assert forall|r: int, k: int|
                1 <= r <= self.list_len() && self.node_key(r) == FOREIGN && 0 <= k < NCLASSES
                    implies cache != node_hdr(self.node_from_end(r), k) by {
                let j = n - r;
                if x == self.arena {
                    self.lemma_home_apart(class, j, k);
                } else {
                    let i = choose|i: int| 0 <= i < n && #[trigger] self.keys@[i] == x && cache == node_hdr(self.list@[i], class);
                    if cache == node_hdr(self.node_from_end(r), k) {
                        assert(i != j);
                        self.lemma_nodes_apart(i, j, class, k);
                    }
                }
            }
        }
    }

    /// What a well-formed heap tells its callers outside this module: the
    /// process's regions are well formed and separated.
    pub proof fn lemma_wf_regions(&self)
        requires
            self.wf(),
        ensures
            regions_ok(),
    {
    }

    /// Owned header `p`'s capacity.
    pub closed spec fn hdr_cap(&self, p: *mut CacheHeader) -> int {
        self.perms@.hdr(p).cap as int
    }

    /// The address of entry `i` of owned header `p`'s stack: where the
    /// permission that pushes write and pops read it through points.
    pub closed spec fn entry_addr(&self, p: *mut CacheHeader, i: int) -> int {
        self.perms@.ent(p, i)@.addr as int
    }

    /// The cache memory a heap reads and writes through its permissions
    /// is metadata (whole-allocator theorem 4): an owned header's 64
    /// bytes and each of its stack's entries.
    pub proof fn lemma_caches_in_meta(&self, p: *mut CacheHeader, i: int)
        requires
            self.wf(),
            self.owns(p),
        ensures
            process_regions().in_meta(p@.addr as int, 64),
            0 <= i < self.hdr_cap(p) ==> process_regions().in_meta(self.entry_addr(p, i), 8),
    {
        let h = self.perms@.hdr(p);
        assert(cell_ok(p, self.perms@.hdrs[p], self.perms@.slots));
        if 0 <= i < h.cap {
            assert(stack_ok(h, self.perms@.slots));
            let _q = slot_ptr(h.slots, i);
            assert(slot_in(h.slots, i));
            assert(vstd::layout::size_of::<*mut u8>() == 8);
            assert(regions_ok());
            assert(cell_in_meta(p, h));
            assert(process_regions().in_meta(h.slots@.addr as int, 8 * h.cap));
            assert(process_regions().meta_base + process_regions().meta_len <= usize::MAX);
            assert((i as usize) as int == i);
            assert(h.slots@.addr + (i as usize) as int * 8 <= usize::MAX);
            crate::vspec::ptr::axiom_ptr_add_addr(h.slots, i as usize);
        }
    }

    /// The token the last pop handed out is that of slot `p` of class `c`
    /// ([`CachePerms::handed`]).
    pub closed spec fn handed_ok(&self, p: *mut u8, c: int) -> bool {
        CachePerms::handed_ok(self.perms@.handed, p, c)
    }

    /// `p` is a header this heap owns.
    pub closed spec fn owns(&self, p: *mut CacheHeader) -> bool {
        self.perms@.hdrs.contains_key(p)
    }

    /// The value of owned header `p`.
    pub closed spec fn hdr(&self, p: *mut CacheHeader) -> CacheHeader {
        self.perms@.hdr(p)
    }

    /// Every header of `node` is one this heap owns, the header for class
    /// `c` caching class `c`, and the node is placed ([`node_placed`]).
    pub closed spec fn owns_node(&self, node: *mut ArenaCaches) -> bool {
        &&& forall|c: int| #![trigger node_hdr(node, c)] 0 <= c < NCLASSES ==> self.owns(node_hdr(node, c)) && self.hdr_class(node_hdr(node, c)) == c
        &&& node_placed(node)
    }


    /// The class owned header `p` caches ([`CachePerms::cls`]).
    pub closed spec fn hdr_class(&self, p: *mut CacheHeader) -> int {
        self.perms@.cls[p]
    }

    /// Every slot this heap's caches hold holds its token
    /// ([`CachePerms::tokened`]; `verus/TODO.md`, "Uniqueness: slot tokens").
    pub closed spec fn tokened(&self) -> bool {
        self.perms@.tokened()
    }

    /// The slot address entry `i` of owned header `p` stands for: the
    /// entry itself, or with [`pac_mode`] the word `pac::auth` gives back
    /// with the stack base as the modifier ([`plain`]).
    pub closed spec fn cached(&self, p: *mut CacheHeader, i: int) -> usize {
        plain(self.perms@.slots[self.perms@.ent(p, i)].value(), self.perms@.hdr(p).slots)
    }

    /// Owned header `p`'s length: its first `len` entries are the slots
    /// it caches.
    pub closed spec fn cached_len(&self, p: *mut CacheHeader) -> int {
        self.perms@.hdr(p).len as int
    }

    /// What owned header `p` caches, as `(class, address)` pairs: the
    /// slots of its first `len` entries ([`Heap::cached`]), whose tokens
    /// the heap holds ([`Heap::tokened`]).
    pub closed spec fn held(&self, p: *mut CacheHeader) -> Seq<(int, int)> {
        tagged(self.hdr_class(p), Seq::new(self.cached_len(p) as nat, |i: int| self.cached(p, i) as int))
    }

    /// The slots this heap's flushes have released so far
    /// ([`CachePerms::released`]).
    pub closed spec fn released(&self) -> Seq<(int, int)> {
        self.perms@.released
    }

    /// The header for `class` of the cache array `node` names: the home
    /// header ([`Heap::home_ptr`]) when `node` is null, else the node's.
    pub closed spec fn array_hdr(&self, node: *mut ArenaCaches, class: int) -> *mut CacheHeader {
        if node@.addr == 0 { self.home_ptr(class) } else { node_hdr(node, class) }
    }

    /// The slots the headers for classes below `k` of the array `node`
    /// hold ([`Heap::held`], [`Heap::array_hdr`]).
    pub closed spec fn array_held(&self, node: *mut ArenaCaches, k: int) -> Set<(int, int)>
        decreases k,
    {
        if k <= 0 {
            Set::empty()
        } else {
            self.array_held(node, k - 1) + self.held(self.array_hdr(node, k - 1)).to_set()
        }
    }

    /// The slots the home array and the first `k` nodes of the `others`
    /// list hold.
    pub closed spec fn held_upto(&self, k: int) -> Set<(int, int)>
        decreases k,
    {
        if k <= 0 {
            self.array_held(ptr::null_mut(), NCLASSES as int)
        } else {
            self.held_upto(k - 1) + self.array_held(self.list@[k - 1], NCLASSES as int)
        }
    }

    /// Every slot the heap's caches hold (whole-allocator theorem 10): those
    /// of its home headers and of the headers of every node on its
    /// `others` list, the cache memory `destroy` frees.
    pub closed spec fn held_all(&self) -> Set<(int, int)> {
        self.held_upto(self.list@.len() as int)
    }

    /// Every header of the home array and of every node on the `others`
    /// list caches nothing.
    pub closed spec fn drained(&self) -> bool {
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.cached_len(self.home_ptr(c)) == 0
        &&& forall|i: int, c: int| 0 <= i < self.list@.len() && 0 <= c < NCLASSES ==> #[trigger] self.cached_len(node_hdr(self.list@[i], c)) == 0
    }

    /// Every header of the home array and of every node on the `others`
    /// list has no stack (capacity 0), so caches nothing
    /// ([`Heap::lemma_cleared_drained`]).
    pub closed spec fn cleared(&self) -> bool {
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.hdr_cap(self.home_ptr(c)) == 0
        &&& forall|i: int, c: int| 0 <= i < self.list@.len() && 0 <= c < NCLASSES ==> #[trigger] self.hdr_cap(node_hdr(self.list@[i], c)) == 0
    }

    /// A well-formed [`Heap::cleared`] heap is [`Heap::drained`]: a
    /// header's length is at most its capacity ([`CacheHeader::inv`]).
    pub proof fn lemma_cleared_drained(&self)
        requires
            self.wf(),
            self.cleared(),
        ensures
            self.drained(),
    {
        assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] self.cached_len(self.home_ptr(c)) == 0 by {
            let p = self.home_ptr(c);
            assert(self.hdr_cap(p) == 0);
            assert(cell_ok(p, self.perms@.hdrs[p], self.perms@.slots));
        }
        assert forall|i: int, c: int| 0 <= i < self.list@.len() && 0 <= c < NCLASSES implies #[trigger] self.cached_len(node_hdr(self.list@[i], c)) == 0 by {
            let p = node_hdr(self.list@[i], c);
            assert(self.owns_node(self.list@[i]));
            assert(self.hdr_cap(p) == 0);
            assert(cell_ok(p, self.perms@.hdrs[p], self.perms@.slots));
        }
    }

    /// An owned header without a stack caches nothing
    /// ([`CacheHeader::inv`]: `len <= cap`).
    pub proof fn lemma_cap_len(&self, p: *mut CacheHeader)
        requires
            self.wf(),
            self.owns(p),
            self.hdr_cap(p) == 0,
        ensures
            self.cached_len(p) == 0,
    {
        assert(cell_ok(p, self.perms@.hdrs[p], self.perms@.slots));
    }

    /// [`Heap::held_all`] depends only on the cache permissions, the
    /// `others` list and the home headers.
    proof fn lemma_held_all_same(a: Heap, b: Heap)
        requires
            a.perms@ == b.perms@,
            a.list@ == b.list@,
            a.home_hdrs@ == b.home_hdrs@,
        ensures
            a.held_all() == b.held_all(),
    {
        Heap::lemma_held_upto_same(a, b, a.list@.len() as int);
    }

    /// [`Heap::held_upto`] depends only on the same three.
    proof fn lemma_held_upto_same(a: Heap, b: Heap, k: int)
        requires
            a.perms@ == b.perms@,
            a.list@ == b.list@,
            a.home_hdrs@ == b.home_hdrs@,
            k <= a.list@.len(),
        ensures
            a.held_upto(k) == b.held_upto(k),
        decreases k,
    {
        let node = if k <= 0 { ptr::null_mut() } else { a.list@[k - 1] };
        assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] a.held(a.array_hdr(node, c)) == b.held(b.array_hdr(node, c)) by {
            assert(a.array_hdr(node, c) == b.array_hdr(node, c));
        }
        Heap::lemma_array_held_same(a, b, node, NCLASSES as int);
        if k > 0 {
            Heap::lemma_held_upto_same(a, b, k - 1);
        }
    }

    /// Whether this heap may create caches (`thread.tcache.enabled`).
    pub closed spec fn tcache_on(&self) -> bool {
        self.tcache_enabled
    }

    /// What a GC step (design 6.4) leaves in owned header `p`, which held
    /// `old.hdr(p)` before it: nothing if the header is the `FOREIGN`
    /// arena's (`foreign`), else its length less `lw - lw / 4` (three
    /// quarters, rounded up, of the `lw` entries unused since the last GC,
    /// `lw` its low-water mark); a subset of what it held; and a low-water
    /// mark restarted at the new length.
    pub closed spec fn gc_left(&self, old: Heap, p: *mut CacheHeader, foreign: bool) -> bool {
        let lw = old.hdr(p).low_water as int;
        &&& self.cached_len(p) == if foreign { 0 } else { old.cached_len(p) - (lw - lw / 4) }
        &&& self.held(p).to_set() <= old.held(p).to_set()
        &&& self.hdr(p).low_water == self.hdr(p).len
    }

    /// Every header of the home array and of every node on `old`'s
    /// `others` list is left as [`Heap::gc_left`] says relative to `old`:
    /// a home header emptied if the home arena is `FOREIGN`, a node's
    /// emptied or trimmed (a node's arena is not modelled).
    pub closed spec fn gc_all(&self, old: Heap) -> bool {
        &&& forall|c: int| #![trigger old.home_ptr(c)] 0 <= c < NCLASSES ==> self.gc_left(old, old.home_ptr(c), old.home() == FOREIGN)
        &&& forall|i: int, c: int|
            0 <= i < old.list@.len() && 0 <= c < NCLASSES
                ==> #[trigger] self.gc_left(old, node_hdr(old.list@[i], c), false) || self.gc_left(old, node_hdr(old.list@[i], c), true)
    }

    /// `perms` are this heap's cache permissions, lent to a proof
    /// (`heap.perms.borrow_mut()`).
    pub closed spec fn perms_are(&self, perms: CachePerms) -> bool {
        self.perms@ == perms
    }

    /// `perms` gave back what [`Heap::theorem_uniqueness`] borrowed: the
    /// same headers, stacks, entries and tokens' slots, so the heap is
    /// as it was.
    pub closed spec fn perms_back(&self, perms: CachePerms) -> bool {
        &&& perms.hdrs == self.perms@.hdrs
        &&& perms.slots == self.perms@.slots
        &&& perms.cls == self.perms@.cls
        &&& perms.owner == self.perms@.owner
        &&& perms.zero == self.perms@.zero
        &&& perms.handed == self.perms@.handed
        &&& perms.tokened()
    }

    /// `self` caches exactly what `old` cached (`verus/THEOREMS.md`,
    /// "`MALLOCX_TCACHE_NONE` bypasses thread caches"): the same cache
    /// permissions (headers, stacks, entries, and the slot tokens and zero
    /// tokens under them), the same `caches` field, `others` list and
    /// keys, home headers and home arena. The other fields (`shared`, `tid`,
    /// `slow_ticks`, `last_gc`, `uncached`, `snapshot`, `tcache_enabled`,
    /// `bound`, `rng`) are unconstrained.
    pub closed spec fn caches_same(&self, old: Heap) -> bool {
        &&& self.perms@ == old.perms@
        &&& self.caches == old.caches
        &&& self.others == old.others
        &&& self.list@ == old.list@
        &&& self.keys@ == old.keys@
        &&& self.home_hdrs@ == old.home_hdrs@
        &&& self.heads == old.heads
        &&& self.arena == old.arena
        &&& self.bound == old.bound
        &&& self.homed == old.homed
    }

    /// What [`Heap::caches_same`] means outside this module: every header
    /// `old` owns is still owned, no other is, and each holds the same
    /// value, so the same length and the same entries standing for the
    /// same slots; no cache gained or lost a slot. Well-formedness and the
    /// home arena carry over.
    pub proof fn lemma_caches_same(&self, old: Heap, p: *mut CacheHeader, i: int)
        requires
            self.caches_same(old),
        ensures
            self.owns(p) == old.owns(p),
            self.hdr(p) == old.hdr(p),
            self.hdr_class(p) == old.hdr_class(p),
            self.cached_len(p) == old.cached_len(p),
            self.cached(p, i) == old.cached(p, i),
            self.tokened() == old.tokened(),
            self.home() == old.home(),
            old.wf() ==> self.wf(),
    {
    }

    /// Whole-allocator theorem 5, uniqueness (`verus/TODO.md`,
    /// "Uniqueness: slot tokens"), for one cached entry of a well-formed
    /// heap: it stands for a slot of its header's class, and that slot is
    /// in no `held`, any other holder of tokens of the class's authority
    /// (the bitmap words' invariants, which hold the free slots'; a claim
    /// in flight; a live block's token, `heap::live::Live`, which the
    /// program holds from the allocation to the free). A heap is well formed
    /// from creation on ([`CachePerms::fresh`]: it caches nothing) and
    /// every writer of its caches keeps it so (see [`Heap::wf`] for the
    /// premises).
    pub proof fn theorem_uniqueness_held(
        h: Heap,
        tracked perms: &CachePerms,
        tracked held: &mut GhostSubset<int>,
        p: *mut CacheHeader,
        i: int,
    )
        requires
            h.wf(),
            h.perms_are(*perms),
            h.owns(p),
            0 <= i < h.cached_len(p),
            old(held).id() == slot_loc(h.hdr_class(p)),
        ensures
            process_regions().is_slot(h.hdr_class(p), h.cached(p, i)),
            !old(held)@.contains(h.cached(p, i) as int),
            final(held).id() == old(held).id(),
            final(held)@ == old(held)@,
    {
        assert(perms.ent(p, i) == h.perms@.ent(p, i));
        CachePerms::theorem_uniqueness_held(perms, held, p, i);
    }

    /// Whole-allocator theorem 5 within one well-formed heap: two different
    /// cached entries of one class stand for two different slots.
    pub proof fn theorem_uniqueness(
        h: Heap,
        tracked perms: &mut CachePerms,
        p1: *mut CacheHeader,
        i1: int,
        p2: *mut CacheHeader,
        i2: int,
    )
        requires
            h.wf(),
            h.perms_are(*old(perms)),
            h.owns(p1),
            h.owns(p2),
            0 <= i1 < h.cached_len(p1),
            0 <= i2 < h.cached_len(p2),
            p1 != p2 || i1 != i2,
            h.hdr_class(p1) == h.hdr_class(p2),
        ensures
            h.cached(p1, i1) != h.cached(p2, i2),
            h.perms_back(*final(perms)),
    {
        CachePerms::theorem_uniqueness_cached(perms, p1, i1, p2, i2);
    }

    /// Whole-allocator theorem 5 against the program's blocks: a slot this
    /// heap caches is no live block of its class ([`Live`], the token the
    /// program holds from the allocation to the free), so no slot is
    /// handed out while a cache still holds it, nor cached while it is
    /// live.
    pub proof fn theorem_uniqueness_live(
        h: Heap,
        tracked perms: &CachePerms,
        tracked blk: &mut Live,
        p: *mut CacheHeader,
        i: int,
    )
        requires
            h.wf(),
            h.perms_are(*perms),
            h.owns(p),
            0 <= i < h.cached_len(p),
            old(blk).slot_of(h.hdr_class(p)),
        ensures
            h.cached(p, i) != old(blk).addr(),
            final(blk).slot_of(h.hdr_class(p)),
            final(blk).addr() == old(blk).addr(),
            final(blk).len() == old(blk).len(),
            final(blk).wf() == old(blk).wf(),
    {
        perms.lemma_tok_get(p, i);
        blk.lemma_slot_apart(perms.toks.tracked_borrow(perms.ent(p, i)), h.hdr_class(p));
    }

    /// Whole-allocator theorem 5 across heaps: a slot two well-formed
    /// heaps' caches both hold would have two tokens of one authority, so
    /// cached entries of one class in two heaps stand for different slots.
    pub proof fn theorem_uniqueness_apart(
        h1: Heap,
        tracked perms1: &mut CachePerms,
        p1: *mut CacheHeader,
        i1: int,
        h2: Heap,
        tracked perms2: &CachePerms,
        p2: *mut CacheHeader,
        i2: int,
    )
        requires
            h1.wf(),
            h1.perms_are(*old(perms1)),
            h2.wf(),
            h2.perms_are(*perms2),
            h1.owns(p1),
            h2.owns(p2),
            0 <= i1 < h1.cached_len(p1),
            0 <= i2 < h2.cached_len(p2),
            h1.hdr_class(p1) == h2.hdr_class(p2),
        ensures
            h1.cached(p1, i1) != h2.cached(p2, i2),
            h1.perms_back(*final(perms1)),
    {
        let ghost o = *perms1;
        let q1 = perms1.ent(p1, i1);
        let q2 = perms2.ent(p2, i2);
        perms1.lemma_tok_get(p1, i1);
        perms2.lemma_tok_get(p2, i2);
        let tracked mut t1 = perms1.toks.tracked_remove(q1);
        t1.disjoint(perms2.toks.tracked_borrow(q2));
        perms1.toks.tracked_insert(q1, t1);
        assert forall|p: *mut CacheHeader, i: int|
            #![trigger perms1.ent(p, i)]
            perms1.hdrs.contains_key(p) && 0 <= i < perms1.hdr(p).len implies perms1.tok_at(p, i) by {
            o.lemma_tok_get(p, i);
        }
    }

    /// [`Heap::keeps`] keeps [`Heap::owns_node`].
    pub proof fn lemma_keeps_node(a: Heap, b: Heap, node: *mut ArenaCaches)
        requires
            b.keeps(a),
            a.owns_node(node),
        ensures
            b.owns_node(node),
    {
        assert forall|c: int| #![trigger node_hdr(node, c)] 0 <= c < NCLASSES implies b.owns(node_hdr(node, c)) && b.hdr_class(node_hdr(node, c)) == c by {
            assert(a.owns(node_hdr(node, c)) && a.hdr_class(node_hdr(node, c)) == c);
            assert(a.perms@.hdrs.contains_key(node_hdr(node, c)));
        }
    }

    /// What [`Heap::list_ok`] gives a walk of the `others` list: `others`
    /// is the node farthest from the end (null for an empty list), and
    /// every node is non-null and owned.
    proof fn lemma_list(&self, r: int)
        requires
            self.wf(),
        ensures
            self.list_len() >= 0,
            self.list_len() == 0 ==> self.others@.addr == 0,
            self.list_len() > 0 ==> self.others == self.node_from_end(self.list_len()),
            1 <= r <= self.list_len() ==> self.node_from_end(r)@.addr != 0 && self.owns_node(self.node_from_end(r)),
    {
        if 1 <= r <= self.list_len() {
            assert(self.list@[self.list@.len() - r] == self.node_from_end(r));
        }
    }

    /// The `others` list of a well-formed heap has fewer than `usize::MAX`
    /// nodes: its nodes' keys ([`Heap::list_ok`]) are pairwise distinct
    /// `u16`s, so there are at most 2^16 of them.
    proof fn lemma_list_bound(&self)
        requires
            self.wf(),
        ensures
            self.list_len() <= 0x1_0000,
    {
        others::lemma_distinct_u16s(self.keys@);
    }

    /// [`Heap::keeps`] keeps every node of the `others` list in its place
    /// counted from the end.
    proof fn lemma_keeps_list(a: Heap, b: Heap, r: int)
        requires
            b.keeps(a),
            1 <= r <= a.list_len(),
        ensures
            a.list_len() <= b.list_len(),
            b.node_from_end(r) == a.node_from_end(r),
    {
        assert(b.list@[b.list@.len() - r] == a.list@[a.list@.len() - r]);
    }

    /// A change to the cache permissions alone, which keeps them as
    /// [`Heap::keeps`] asks, keeps the heap: the `others` list and its
    /// nodes' ownership come along unchanged. For writers that never touch
    /// the list, so their proofs need not carry [`Heap::list_ok`].
    proof fn lemma_keeps_perms(old: Heap, new: Heap)
        requires
            old.wf(),
            new.list == old.list,
            new.keys == old.keys,
            new.arena == old.arena,
            new.others == old.others,
            new.home_hdrs == old.home_hdrs,
            new.heads == old.heads,
            new.bound == old.bound,
            new.homed == old.homed,
            new.perms@.keeps(old.perms@),
            new.perms@.follows(old.perms@),
            new.perms@.zero_ok(),
            new.perms@.tokened(),
        ensures
            new.keeps(old),
    {
        Heap::lemma_home_kept(old, new);
        assert forall|i: int| 0 <= i < new.list@.len() implies new.owns_node(#[trigger] new.list@[i]) by {
            assert(old.owns_node(old.list@[i]));
            assert forall|c: int| #![trigger node_hdr(new.list@[i], c)] 0 <= c < NCLASSES implies new.owns(node_hdr(new.list@[i], c)) && new.hdr_class(node_hdr(new.list@[i], c)) == c by {
                assert(old.perms@.hdrs.contains_key(node_hdr(old.list@[i], c)));
            }
        }
    }

    /// [`Heap::keeps`] is transitive.
    pub proof fn lemma_keeps_trans(a: Heap, b: Heap, c: Heap)
        requires
            b.keeps(a),
            c.keeps(b),
        ensures
            c.keeps(a),
    {
        assert forall|r: int| 1 <= r <= a.list@.len() implies c.list@[c.list@.len() - r] == #[trigger] a.list@[a.list@.len() - r] by {
            assert(b.list@[b.list@.len() - r] == a.list@[a.list@.len() - r]);
        }
    }

    /// A well-formed heap [`Heap::keeps`] itself.
    proof fn lemma_keeps_refl(a: Heap)
        requires
            a.wf(),
        ensures
            a.keeps(a),
    {
    }

    /// A heap that differs from a well-formed one only in the token its
    /// last pop handed out keeps it ([`CachePerms::handed`]).
    proof fn lemma_keeps_handed(a: Heap, b: Heap)
        requires
            a.wf(),
            b.perms@.hdrs == a.perms@.hdrs,
            b.perms@.slots == a.perms@.slots,
            b.perms@.cls == a.perms@.cls,
            b.perms@.owner == a.perms@.owner,
            b.perms@.toks == a.perms@.toks,
            b.perms@.zero == a.perms@.zero,
            b.list == a.list,
            b.keys == a.keys,
            b.arena == a.arena,
            b.bound == a.bound,
            b.homed == a.homed,
            b.others == a.others,
            b.home_hdrs == a.home_hdrs,
            b.heads == a.heads,
        ensures
            b.keeps(a),
    {
        Heap::lemma_home_kept(a, b);
        CachePerms::lemma_zero_same(a.perms@, b.perms@);
        CachePerms::lemma_keeps_refl(a.perms@);
        CachePerms::lemma_tracks_same(a.perms@);
        CachePerms::lemma_same_but_tokens(a.perms@, b.perms@, a.perms@);
    }


    /// Reset every (flushed) header of one array to [`CacheHeader::EMPTY`]
    /// and free its stack, for `destroy`, which frees the heap next. It
    /// writes the headers through the plain field, so the permissions are
    /// stale afterwards ([`Heap::wf`]); [`Heap::free_caches_in`] is the
    /// form that keeps them.
    fn free_caches(shared: &Shared, caches: &mut [CacheHeader; NCLASSES], Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_meta()),
        ensures
            *final(ord) == *old(ord),
    {
        for cache in caches.iter_mut()
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                ord.allows(rank_meta()),
                *ord == *old(ord),
        {
            if cache.cap != 0 {
                assert_flushed(&cache.len);
                shared
                    .meta
                    .free(cache.slots as *mut u8, CacheHeader::slots_layout(cache.cap), Tracked(ord));
            }
            *cache = CacheHeader::EMPTY;
        }
    }

    /// [`Heap::free_caches`] through the permissions, for the home array
    /// (`node` null) or `node`'s: reset every (flushed) header to
    /// [`CacheHeader::EMPTY`] and free its stack. The heap stays well
    /// formed (an `EMPTY` header holds nothing), but not [`Heap::keeps`]
    /// its old self (the headers lose their stacks). A stack's entry
    /// permissions leave the heap's before the stack goes back to `Meta`,
    /// whose `free` takes the whole block. The array's headers must be
    /// flushed, so the `assert_flushed` abort is never reached; no other
    /// header changes, and the released log stays as it is.
    fn free_caches_in(&mut self, shared: &Shared, node: *mut ArenaCaches, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            old(self).wf(),
            node@.addr == 0 || old(self).owns_node(node),
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] old(self).cached_len(old(self).array_hdr(node, c)) == 0,
        ensures
            *final(ord) == *old(ord),
            final(self).wf(),
            final(self).same_shape(*old(self)),
            final(self).tcache_on() == old(self).tcache_on(),
            final(self).released() == old(self).released(),
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] final(self).hdr_cap(old(self).array_hdr(node, c)) == 0,
            forall|p: *mut CacheHeader|
                old(self).owns(p) && (forall|c: int| 0 <= c < NCLASSES ==> p != #[trigger] old(self).array_hdr(node, c))
                    ==> #[trigger] final(self).hdr(p) == old(self).hdr(p),
    {
        proof {
            assert(self.perms@.hdrs.dom() =~= old(self).perms@.hdrs.dom());
            old(self).lemma_array_hdr(node);
        }
        for class in 0..NCLASSES
            invariant
                self.tcache_on() == old(self).tcache_on(),
                crate::sys::may_syscall(),
                forall|c: int| 0 <= c < NCLASSES ==> #[trigger] old(self).cached_len(old(self).array_hdr(node, c)) == 0,
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
                self.wf(),
                self.same_shape(*old(self)),
                node@.addr == 0 || self.owns_node(node),
                self.released() == old(self).released(),
                forall|c: int| 0 <= c < NCLASSES ==> old(self).owns(#[trigger] old(self).array_hdr(node, c)) && old(self).hdr_class(old(self).array_hdr(node, c)) == c,
                forall|c: int| 0 <= c < class ==> #[trigger] self.hdr_cap(old(self).array_hdr(node, c)) == 0,
                forall|p: *mut CacheHeader|
                    old(self).owns(p) && (forall|c: int| 0 <= c < class ==> p != #[trigger] old(self).array_hdr(node, c))
                        ==> #[trigger] self.hdr(p) == old(self).hdr(p),
        {
            // SAFETY: `node` is null or a live node owned by this heap.
            let cache = unsafe { self.header_in(node, class) };
            let ghost h0 = *self;
            proof {
                assert(cache == old(self).array_hdr(node, class as int));
                // The headers before this one cache other classes, so this
                // one is still as flushed.
                assert forall|c: int| 0 <= c < class implies cache != #[trigger] old(self).array_hdr(node, c) by {
                    assert(old(self).hdr_class(old(self).array_hdr(node, c)) == c);
                }
                assert(h0.hdr(cache) == old(self).hdr(cache));
                assert(old(self).cached_len(old(self).array_hdr(node, class as int)) == 0);
                Heap::lemma_stack_keys(h0, cache);
                let tracked _gone = self.perms.borrow_mut().slots.tracked_remove_keys(stack_set(h0.hdr(cache)));
            }
            // `hdr_mut` without its `wf` premise, which the removal broke.
            let h = ptr_mut_ref(cache, Tracked(self.perms.borrow_mut().hdrs.tracked_borrow_mut(cache)));
            if h.cap != 0 {
                proof {
                    assert(h.len == 0);
                }
                assert_flushed(&h.len);
                shared
                    .meta
                    .free(h.slots as *mut u8, CacheHeader::slots_layout(h.cap), Tracked(ord));
            }
            *h = CacheHeader::EMPTY;
            proof {
                Heap::lemma_set_empty(h0, *self, cache, self.hdr(cache));
                Heap::lemma_same_trans(*old(self), h0, *self);
                if node@.addr != 0 {
                    Heap::lemma_same_node(h0, *self, node);
                }
                assert forall|p: *mut CacheHeader|
                    old(self).owns(p) && (forall|c: int| 0 <= c < class + 1 ==> p != #[trigger] old(self).array_hdr(node, c))
                        implies #[trigger] self.hdr(p) == old(self).hdr(p) by {
                    assert(p != old(self).array_hdr(node, class as int));
                    assert(h0.hdr(p) == old(self).hdr(p));
                    assert(self.perms@.hdrs[p] == h0.perms@.hdrs[p]);
                }
                assert forall|c: int| 0 <= c < class + 1 implies #[trigger] self.hdr_cap(old(self).array_hdr(node, c)) == 0 by {
                    let p = old(self).array_hdr(node, c);
                    if c < class {
                        assert(p != cache);
                        assert(h0.hdr_cap(p) == 0);
                        assert(self.perms@.hdrs[p] == h0.perms@.hdrs[p]);
                    }
                }
            }
        }
    }

    /// Theorem 10's accounting for one [`Heap::flush`] of `cache`, from
    /// what its proof establishes: `a` are the addresses `cache` held, `k`
    /// those it keeps (its new entries), `r` those it released, and the
    /// released log gained `r`. The slots `cache` no longer holds are then
    /// exactly the log's new entries, and no other header's holdings change.
    proof fn lemma_flush_accounting(old: Heap, new: Heap, cache: *mut CacheHeader, a: Seq<int>, k: Seq<int>, r: Seq<int>)
        requires
            old.wf(),
            old.owns(cache),
            new.perms@.hdrs == old.perms@.hdrs.insert(cache, new.perms@.hdrs[cache]),
            new.perms@.cls == old.perms@.cls,
            new.list == old.list,
            new.keys == old.keys,
            new.arena == old.arena,
            new.bound == old.bound,
            new.homed == old.homed,
            new.others == old.others,
            new.home_hdrs == old.home_hdrs,
            new.heads == old.heads,
            forall|p2: *mut CacheHeader, j: int|
                old.perms@.hdrs.contains_key(p2) && p2 != cache && 0 <= j < old.perms@.hdr(p2).cap
                    ==> new.perms@.slots[#[trigger] old.perms@.ent(p2, j)] == old.perms@.slots[old.perms@.ent(p2, j)],
            a.len() == old.perms@.hdr(cache).len,
            forall|i: int| 0 <= i < a.len() ==> a[i] == plain(old.perms@.slots[slot_ptr(old.perms@.hdr(cache).slots, i)].value(), old.perms@.hdr(cache).slots) as int,
            k.len() == new.perms@.hdr(cache).len,
            forall|i: int| 0 <= i < k.len() ==> k[i] == plain(new.perms@.slots[#[trigger] new.perms@.ent(cache, i)].value(), new.perms@.hdr(cache).slots) as int,
            split_ok(a, k, r),
            new.perms@.released == old.perms@.released + tagged(old.perms@.cls[cache], r),
        ensures
            new.same_shape(old),
            forall|p: *mut CacheHeader| old.owns(p) && p != cache ==> #[trigger] new.held(p) == old.held(p),
            forall|p: *mut CacheHeader| old.owns(p) && p != cache ==> #[trigger] new.hdr(p) == old.hdr(p),
            new.held(cache).to_set() <= old.held(cache).to_set(),
            log_adds(new.released(), old.released(), old.held(cache).to_set() - new.held(cache).to_set()),
    {
        let c = old.hdr_class(cache);
        assert(new.perms@.hdrs.dom() =~= old.perms@.hdrs.dom());
        assert(old.held(cache) =~= tagged(c, a));
        assert(new.held(cache) =~= tagged(c, k));
        lemma_tagged_split(c, a, k, r);
        lemma_log_append(old.released(), tagged(c, r));
        assert forall|p: *mut CacheHeader| old.owns(p) && p != cache implies #[trigger] new.held(p) == old.held(p) by {
            assert(cell_ok(p, old.perms@.hdrs[p], old.perms@.slots));
            assert(new.perms@.hdr(p) == old.perms@.hdr(p));
            assert forall|i: int| 0 <= i < old.cached_len(p) implies new.cached(p, i) == old.cached(p, i) by {
                assert(new.perms@.ent(p, i) == old.perms@.ent(p, i));
                assert(new.perms@.slots[old.perms@.ent(p, i)] == old.perms@.slots[old.perms@.ent(p, i)]);
            }
            assert(new.held(p) =~= old.held(p));
        }
    }

    /// `self` is `old` with at most its headers' values changed: the same
    /// headers, caching the same classes, the same `others` list and
    /// keys (and the permissions to its nodes' `arena` and `next`), home
    /// headers and home arena.
    pub closed spec fn same_shape(&self, old: Heap) -> bool {
        &&& self.perms@.hdrs.dom() == old.perms@.hdrs.dom()
        &&& self.perms@.cls == old.perms@.cls
        &&& self.list == old.list
        &&& self.keys == old.keys
        &&& self.arena == old.arena
        &&& self.bound == old.bound
        &&& self.homed == old.homed
        &&& self.others == old.others
        &&& self.home_hdrs == old.home_hdrs
        &&& self.heads == old.heads
    }

    /// [`Heap::same_shape`] is transitive.
    proof fn lemma_same_trans(a: Heap, b: Heap, c: Heap)
        requires
            b.same_shape(a),
            c.same_shape(b),
        ensures
            c.same_shape(a),
    {
    }

    /// [`Heap::same_shape`] keeps [`Heap::owns_node`].
    proof fn lemma_same_node(a: Heap, b: Heap, node: *mut ArenaCaches)
        requires
            b.same_shape(a),
            a.owns_node(node),
        ensures
            b.owns_node(node),
    {
        assert forall|c: int| #![trigger node_hdr(node, c)] 0 <= c < NCLASSES implies b.owns(node_hdr(node, c)) && b.hdr_class(node_hdr(node, c)) == c by {
            assert(a.owns(node_hdr(node, c)) && a.hdr_class(node_hdr(node, c)) == c);
            assert(a.perms@.hdrs.dom().contains(node_hdr(node, c)));
        }
    }

    /// [`Heap::same_shape`] keeps [`Heap::list_ok`].
    proof fn lemma_same_list(a: Heap, b: Heap)
        requires
            b.same_shape(a),
            a.list_ok(),
        ensures
            b.list_ok(),
    {
        assert forall|i: int| 0 <= i < b.list@.len() implies (#[trigger] b.list@[i])@.addr != 0 && b.owns_node(b.list@[i]) by {
            Heap::lemma_same_node(a, b, a.list@[i]);
        }
    }

    /// The entries of owned header `cache`'s stack are owned entries, and
    /// they are its own ([`CachePerms::tracks`]): no other owned header's
    /// stack has one.
    proof fn lemma_stack_keys(h: Heap, cache: *mut CacheHeader)
        requires
            h.wf(),
            h.owns(cache),
        ensures
            h.perms@.hdrs[cache].ptr() == cache,
            h.perms@.hdrs[cache].is_init(),
            stack_set(h.hdr(cache)).subset_of(h.perms@.slots.dom()),
            forall|q: *mut *mut u8|
                #[trigger] stack_set(h.hdr(cache)).contains(q) ==> h.perms@.owner.contains_key(q) && h.perms@.owner[q].0 == cache,
    {
        let o = h.perms@;
        let hc = o.hdr(cache);
        assert(cell_ok(cache, o.hdrs[cache], o.slots));
        assert forall|q: *mut *mut u8| #[trigger] stack_set(hc).contains(q) implies o.slots.contains_key(q) && o.owner.contains_key(q) && o.owner[q].0 == cache by {
            lemma_stack_set(hc, q);
            let i = choose|i: int| 0 <= i < hc.cap && slot_ptr(hc.slots, i) == q;
            assert(o.ent(cache, i) == q);
        }
    }

    /// A store of `EMPTY` into owned header `cache`, which caches nothing,
    /// with its stack's entries gone from the permissions, keeps the heap
    /// well formed: no other header owns one of those entries
    /// ([`Heap::lemma_stack_keys`]).
    proof fn lemma_set_empty(old: Heap, new: Heap, cache: *mut CacheHeader, v: CacheHeader)
        requires
            old.wf(),
            old.owns(cache),
            old.hdr(cache).len == 0,
            new.perms@.hdrs == old.perms@.hdrs.insert(cache, new.perms@.hdrs[cache]),
            new.perms@.hdrs[cache].ptr() == cache,
            new.perms@.hdrs[cache].is_init(),
            new.perms@.hdrs[cache].value() == v,
            new.perms@.slots == old.perms@.slots.remove_keys(stack_set(old.hdr(cache))),
            new.perms@.zero == old.perms@.zero,
            new.perms@.toks == old.perms@.toks,
            new.perms@.owner == old.perms@.owner,
            new.perms@.cls == old.perms@.cls,
            new.list == old.list,
            new.keys == old.keys,
            new.arena == old.arena,
            new.bound == old.bound,
            new.homed == old.homed,
            new.others == old.others,
            new.home_hdrs == old.home_hdrs,
            new.heads == old.heads,
            v.len == 0 && v.cap == 0 && v.low_water == 0 && v.zeroed == 0 && v.run_end == 0,
        ensures
            new.wf(),
            new.same_shape(old),
    {
        let o = old.perms@;
        let n = new.perms@;
        let s = stack_set(o.hdr(cache));
        Heap::lemma_stack_keys(old, cache);
        assert(n.hdrs.dom() =~= o.hdrs.dom());
        // The other headers' entries are not the freed stack's.
        assert forall|p: *mut CacheHeader, j: int|
            o.hdrs.contains_key(p) && p != cache && 0 <= j < o.hdr(p).cap implies !s.contains(#[trigger] o.ent(p, j))
                && n.slots.contains_key(o.ent(p, j)) && n.slots[o.ent(p, j)] == o.slots[o.ent(p, j)] by {
            assert(cell_ok(p, o.hdrs[p], o.slots));
            assert(o.owner[o.ent(p, j)] == (p, j));
            assert(o.slots.contains_key(slot_ptr(o.hdr(p).slots, j)));
        }
        assert forall|p: *mut CacheHeader| #[trigger] n.hdrs.contains_key(p) implies cell_ok(p, n.hdrs[p], n.slots) by {
            if p != cache {
                assert(cell_ok(p, o.hdrs[p], o.slots));
                assert forall|i: int| 0 <= i < n.hdr(p).cap implies {
                    let q = #[trigger] slot_ptr(n.hdr(p).slots, i);
                    &&& n.slots.contains_key(q)
                    &&& n.slots[q].ptr() == q
                    &&& (i < n.hdr(p).len ==> n.slots[q].is_init())
                    &&& slot_in(n.hdr(p).slots, i)
                } by {
                    assert(slot_ptr(n.hdr(p).slots, i) == o.ent(p, i));
                }
            }
        }
        assert forall|p: *mut CacheHeader, i: int|
            #![trigger n.ent(p, i)]
            n.hdrs.contains_key(p) && 0 <= i < n.hdr(p).cap implies {
                &&& n.owner.contains_key(n.ent(p, i))
                &&& n.owner[n.ent(p, i)] == (p, i)
                &&& (i < n.hdr(p).len ==> n.holds(n.cls[p], n.ent(p, i), n.hdr(p).slots))
            } by {
            assert(p != cache);
            assert(n.ent(p, i) == o.ent(p, i));
        }
        assert forall|p: *mut CacheHeader, i: int|
            #![trigger n.ent(p, i)]
            n.hdrs.contains_key(p) && 0 <= i < n.hdr(p).len implies n.tok_at(p, i) by {
            assert(p != cache);
            assert(n.ent(p, i) == o.ent(p, i));
            assert(o.tok_at(p, i));
        }
        assert forall|q: *mut *mut u8|
            #[trigger] o.owner.contains_key(q) && o.owner[q].0 != cache && o.slots.contains_key(q) implies n.slots[q] == o.slots[q] by {
            assert(!s.contains(q));
        }
        CachePerms::lemma_zero_update(o, n, cache);
        Heap::lemma_home_kept(old, new);
        Heap::lemma_same_list(old, new);
    }

    /// Trade the values of owned headers `a` and `b` of one class through
    /// their permissions (`thread.arena`'s exchange of the home caches
    /// with a node's, [`Heap::set_arena`]); each stack goes with its
    /// header, and its entries' owner with it ([`swap_owner`]).
    fn swap_hdrs(&mut self, a: *mut CacheHeader, b: *mut CacheHeader)
        requires
            old(self).wf(),
            old(self).owns(a),
            old(self).owns(b),
            a != b,
            old(self).hdr_class(a) == old(self).hdr_class(b),
        ensures
            final(self).wf(),
            final(self).same_shape(*old(self)),
    {
        let ghost o = self.perms@;
        let tracked mut pa = self.perms.borrow_mut().hdrs.tracked_remove(a);
        let tracked mut pb = self.perms.borrow_mut().hdrs.tracked_remove(b);
        proof {
            assert(cell_ok(a, o.hdrs[a], o.slots));
            assert(cell_ok(b, o.hdrs[b], o.slots));
        }
        core::mem::swap(ptr_mut_ref(a, Tracked(&mut pa)), ptr_mut_ref(b, Tracked(&mut pb)));
        proof {
            self.perms.borrow_mut().hdrs.tracked_insert(a, pa);
            self.perms.borrow_mut().hdrs.tracked_insert(b, pb);
            self.perms.borrow_mut().owner = swap_owner(o.owner, a, b);
            assert(self.perms@.hdrs =~= o.hdrs.insert(a, self.perms@.hdrs[a]).insert(b, self.perms@.hdrs[b]));
            CachePerms::lemma_swap(o, self.perms@, a, b);
            assert(self.perms@.hdrs.dom() =~= o.hdrs.dom());
            Heap::lemma_same_list(*old(self), *self);
            Heap::lemma_home_kept(*old(self), *self);
        }
    }

    /// Home arena (design 6.2 `heap.arena`; `thread.arena`).
    #[inline]
    pub fn arena(&self) -> u16 {
        self.arena
    }

    /// The home arena, [`Heap::arena`]'s field.
    pub closed spec fn spec_arena(&self) -> u16 {
        self.arena
    }

    /// [`Heap::arena`] through a volatile load, for the `free` check's
    /// arena test ([`Shared::checked_slot`]): a plain load is hoisted
    /// above the multi-arena flag test and touches the heap's header line
    /// on every single-arena `free`. A volatile read of a plain field,
    /// which reads the field's value: the `rr::` specification states the
    /// `ensures`, and RefinedRust proves the body against it
    /// (`verus/refinedrust/run.sh`, record under `verus/seams/`).
    #[inline(always)]
    #[cfg_attr(refinedrust, rr::params("l", "sh" : "loc", "tid" : "Z", "ar" : "Z"))]
    #[cfg_attr(refinedrust, rr::args("l" @ "alias_ref_t"))]
    #[cfg_attr(refinedrust, rr::requires(#iris "l ◁ₗ[π, Shared ulft_1] #(-[#sh; #tid; #ar]) @ ◁ HeapArenaHead_ty"))]
    #[cfg_attr(refinedrust, rr::returns("ar"))]
    #[verifier::external_body]
    pub(super) fn arena_volatile(&self) -> (r: u16)
        ensures
            r == self.spec_arena(),
        opens_invariants none
    {
        // SAFETY: `self` is a live heap and `arena` a plain field.
        unsafe { ptr::read_volatile(&raw const self.arena) }
    }

    /// Flush the home caches, fold their byte counters into `uncached` and
    /// free them: the home array is `EMPTY` afterwards and `stats()` is
    /// unchanged. For a `thread.arena` change that could get no `others`
    /// node for the old home.
    ///
    /// The heap stays well formed: the headers are reset through their
    /// permissions ([`Heap::free_caches_in`]).
    fn drop_home_caches(&mut self, shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            shared.wf(),
            old(self).wf(),
        ensures
            *final(ord) == *old(ord),
            final(self).wf(),
            final(self).same_shape(*old(self)),
    {
        let home = self.arena;
        self.flush_arena(shared, home, ptr::null_mut(), Tracked(ord));
        let ghost hf = *self;
        Self::fold_counters(&mut self.uncached, &self.caches);
        proof {
            assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] self.cached_len(self.array_hdr(ptr::null_mut(), c)) == 0 by {
                assert(hf.cached_len(old(self).array_hdr(ptr::null_mut(), c)) == 0);
            }
        }
        self.free_caches_in(shared, ptr::null_mut(), Tracked(ord));
    }

    /// Add the byte counters a cache array stands for (`allocs` pops and
    /// `len + allocs - moved` pushes per cache, each worth the class size)
    /// to `into`. [`Heap::stats`] derives the thread counters this way on
    /// every read; a cache that is about to be freed is folded into
    /// `uncached` first so that `thread.allocated` never goes backwards.
    #[allow(clippy::while_let_loop, reason = "the desugared `for`, which Verus needs for `continue`")]
    fn fold_counters(into: &mut ThreadStats, caches: &[CacheHeader; NCLASSES]) {
        // `for (class, c) in caches.iter().enumerate()`, spelled out: Verus
        // has no `Enumerate` and rejects `continue` in a `for` loop. The
        // count is taken and bumped as `Enumerate::next` does.
        let mut iter = caches.iter();
        let mut count: usize = 0;
        loop
            invariant
                count + vstd::std_specs::iter::IteratorSpec::remaining(&iter).len() == NCLASSES,
            decreases NCLASSES - count,
        {
            let c = match iter.next() {
                Some(c) => c,
                None => break,
            };
            let class = count;
            count += 1;
            if c.cap == 0 {
                continue;
            }
            let frees = (c.len as u64).wrapping_add(c.allocs).wrapping_sub(c.moved);
            into.add(CLASSES.table()[class].size() as u64, c.allocs, frees);
        }
    }

    /// Byte counters for this thread: the uncached part plus what every
    /// cache of every arena stands for ([`Heap::fold_counters`]).
    pub fn stats(&self) -> ThreadStats
        requires
            self.wf(),
    {
        let mut s = self.uncached;
        Self::fold_counters(&mut s, &self.caches);
        // `for o in Self::nodes(self.others)`, spelled out: Verus has no
        // `from_fn`. `r` counts the nodes left, this one included.
        let mut o = self.others;
        let ghost mut r = self.list_len();
        proof {
            self.lemma_list(r);
        }
        while !o.is_null()
            invariant
                self.wf(),
                0 <= r <= self.list_len(),
                r > 0 ==> o == self.node_from_end(r),
                r == 0 ==> o@.addr == 0,
            decreases r,
        {
            proof {
                self.lemma_list(r);
            }
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            let next = unsafe { self.node_next(o, Ghost(r)) };
            unsafe { Self::fold_counters(&mut s, self.node_caches(o, Ghost(r))) };
            proof {
                r = r - 1;
            }
            o = next;
        }
        s
    }

    /// [`Heap::stats`] materialised into the heap for `thread.allocatedp`
    /// and `thread.deallocatedp` (design 13.4). The counters are derived
    /// on every read, so the pointers those keys hand out refer to this
    /// copy, which is refreshed by every read of any of the four
    /// `thread.*allocated*` keys and stale in between.
    pub fn materialise_stats(&mut self) -> &ThreadStats
        requires
            old(self).wf(),
    {
        self.snapshot = self.stats();
        &self.snapshot
    }

    /// `thread.tcache.enabled`.
    #[inline]
    pub fn tcache_enabled(&self) -> (r: bool)
        ensures
            r == self.tcache_on(),
    {
        self.tcache_enabled
    }

    /// Flush every cache of one array (the home array when `node` is null,
    /// else the node's, whose arena is `arena`) completely: afterwards
    /// none of its headers caches anything, the released log has gained
    /// the slots they held, and no other header has changed what it holds
    /// (whole-allocator theorem 10).
    fn flush_arena(&mut self, shared: &Shared, arena: u16, node: *mut ArenaCaches, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            shared.wf(),
            old(self).wf(),
            node@.addr == 0 || old(self).owns_node(node),
        ensures
            *final(ord) == *old(ord),
            final(self).keeps(*old(self)),
            final(self).same_shape(*old(self)),
            final(self).tcache_on() == old(self).tcache_on(),
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] final(self).cached_len(old(self).array_hdr(node, c)) == 0,
            forall|p: *mut CacheHeader|
                old(self).owns(p) && (forall|c: int| 0 <= c < NCLASSES ==> p != #[trigger] old(self).array_hdr(node, c))
                    ==> #[trigger] final(self).held(p) == old(self).held(p),
            log_adds(final(self).released(), old(self).released(), old(self).array_held(node, NCLASSES as int)),
    {
        proof {
            Heap::lemma_keeps_refl(*self);
            lemma_log_same(self.released());
            assert(old(self).array_held(node, 0) =~= Set::empty());
            old(self).lemma_array_hdr(node);
        }
        for class in 0..NCLASSES
            invariant
                crate::sys::may_abort(),
                self.tcache_on() == old(self).tcache_on(),
                crate::sys::may_syscall(),
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
                self.keeps(*old(self)),
                self.same_shape(*old(self)),
                node@.addr == 0 || old(self).owns_node(node),
                shared.wf(),
                forall|c: int| 0 <= c < NCLASSES ==> old(self).owns(#[trigger] old(self).array_hdr(node, c)) && old(self).hdr_class(old(self).array_hdr(node, c)) == c,
                forall|c: int| 0 <= c < class ==> #[trigger] self.cached_len(old(self).array_hdr(node, c)) == 0,
                forall|p: *mut CacheHeader|
                    old(self).owns(p) && (forall|c: int| 0 <= c < class ==> p != #[trigger] old(self).array_hdr(node, c))
                        ==> #[trigger] self.held(p) == old(self).held(p),
                log_adds(self.released(), old(self).released(), old(self).array_held(node, class as int)),
        {
            proof {
                if node@.addr != 0 {
                    Heap::lemma_keeps_node(*old(self), *self, node);
                }
            }
            let ghost h0 = *self;
            let ghost hc = old(self).array_hdr(node, class as int);
            proof {
                // The headers before this one cache other classes.
                assert forall|c: int| 0 <= c < class implies hc != #[trigger] old(self).array_hdr(node, c) by {
                    assert(old(self).hdr_class(old(self).array_hdr(node, c)) == c);
                }
                assert(h0.held(hc) == old(self).held(hc));
            }
            // SAFETY: `node` is null or a live node owned by this heap; an
            // unused header has `len == 0` and is skipped.
            unsafe {
                let cache = self.header_in(node, class);
                let len = self.hdr_ref(cache).len as usize;
                if len != 0 {
                    self.flush(shared, arena, cache, class, len, Tracked(ord));
                    proof {
                        assert(self.held(hc).len() == self.cached_len(hc));
                        assert(self.held(hc).to_set() =~= Set::empty());
                        assert(h0.held(hc).to_set() - self.held(hc).to_set() =~= old(self).held(hc).to_set());
                        lemma_log_trans(old(self).released(), h0.released(), self.released(), old(self).array_held(node, class as int), old(self).held(hc).to_set());
                    }
                } else {
                    proof {
                        assert(old(self).held(hc).len() == old(self).cached_len(hc));
                        assert(old(self).held(hc).to_set() =~= Set::empty());
                        assert(old(self).array_held(node, class as int) + old(self).held(hc).to_set() =~= old(self).array_held(node, class as int));
                    }
                }
            }
            proof {
                assert(old(self).array_held(node, class + 1) == old(self).array_held(node, class as int) + old(self).held(hc).to_set());
                assert forall|p: *mut CacheHeader|
                    old(self).owns(p) && (forall|c: int| 0 <= c < class + 1 ==> p != #[trigger] old(self).array_hdr(node, c))
                        implies #[trigger] self.held(p) == old(self).held(p) by {
                    assert(p != old(self).array_hdr(node, class as int));
                    assert(h0.held(p) == old(self).held(p));
                }
                assert forall|c: int| 0 <= c < class + 1 implies #[trigger] self.cached_len(old(self).array_hdr(node, c)) == 0 by {
                    if c < class {
                        let p = old(self).array_hdr(node, c);
                        assert(p != hc);
                        assert(h0.held(p) == self.held(p));
                        assert(self.held(p).len() == self.cached_len(p));
                        assert(h0.held(p).len() == h0.cached_len(p));
                    }
                }
            }
        }
    }

    /// The headers of the array `node` names ([`Heap::array_hdr`]) are
    /// owned, the one for class `c` caching `c`.
    proof fn lemma_array_hdr(&self, node: *mut ArenaCaches)
        requires
            self.wf(),
            node@.addr == 0 || self.owns_node(node),
        ensures
            forall|c: int| 0 <= c < NCLASSES ==> self.owns(#[trigger] self.array_hdr(node, c)) && self.hdr_class(self.array_hdr(node, c)) == c,
    {
        assert forall|c: int| 0 <= c < NCLASSES implies self.owns(#[trigger] self.array_hdr(node, c)) && self.hdr_class(self.array_hdr(node, c)) == c by {
            if node@.addr == 0 {
                assert(self.owns(self.home_hdrs@[c]) && self.hdr_class(self.home_hdrs@[c]) == c);
            } else {
                assert(self.owns(node_hdr(node, c)) && self.hdr_class(node_hdr(node, c)) == c);
            }
        }
    }

    /// Return every cached slot of this heap to its slab. Caches stay
    /// allocated.
    ///
    /// Whole-allocator theorem 10 (no leak on thread exit, as `destroy`
    /// runs this first): afterwards no header of the home array or of a
    /// node on the `others` list caches anything ([`Heap::drained`]), and
    /// the released log ([`Heap::released`]) has gained exactly the slots
    /// they held ([`Heap::held_all`]), each handed with its token to
    /// `Shared::release_routed`, which proves it used up their tokens (that
    /// they reach their bitmap words is read from the verified bodies).
    pub fn flush_all(&mut self, shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            shared.wf(),
            old(self).wf(),
        ensures
            *final(ord) == *old(ord),
            final(self).keeps(*old(self)),
            final(self).same_shape(*old(self)),
            final(self).tcache_on() == old(self).tcache_on(),
            final(self).drained(),
            log_adds(final(self).released(), old(self).released(), old(self).held_all()),
    {
        let home = self.arena;
        self.flush_arena(shared, home, ptr::null_mut(), Tracked(ord));
        // `for o in Self::nodes(self.others)`, spelled out: Verus has no
        // `from_fn`. As the iterator does, the node's `next` is read before
        // its turn. `r` counts the nodes left, this one included.
        let mut o = self.others;
        let ghost mut r = self.list_len();
        let ghost l = old(self).list@;
        proof {
            self.lemma_list(r);
            assert(old(self).held_upto(0) == old(self).array_held(ptr::null_mut(), NCLASSES as int));
            assert forall|p: *mut CacheHeader|
                old(self).owns(p) && (forall|c: int| 0 <= c < NCLASSES ==> p != #[trigger] old(self).home_ptr(c))
                    && (forall|i: int, c: int| 0 <= i < l.len() - r && 0 <= c < NCLASSES ==> p != #[trigger] node_hdr(l[i], c))
                    implies #[trigger] self.held(p) == old(self).held(p) by {
                assert forall|c: int| 0 <= c < NCLASSES implies p != #[trigger] old(self).array_hdr(ptr::null_mut(), c) by {
                    assert(p != old(self).home_ptr(c));
                }
            }
            assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] self.cached_len(old(self).home_ptr(c)) == 0 by {
                assert(self.cached_len(old(self).array_hdr(ptr::null_mut(), c)) == 0);
            }
        }
        while !o.is_null()
            invariant
                crate::sys::may_abort(),
                self.tcache_on() == old(self).tcache_on(),
                crate::sys::may_syscall(),
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
                shared.wf(),
                self.keeps(*old(self)),
                self.same_shape(*old(self)),
                l == old(self).list@,
                old(self).wf(),
                0 <= r <= self.list_len(),
                r > 0 ==> o == self.node_from_end(r),
                r == 0 ==> o@.addr == 0,
                forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.cached_len(old(self).home_ptr(c)) == 0,
                forall|i: int, c: int| 0 <= i < l.len() - r && 0 <= c < NCLASSES ==> #[trigger] self.cached_len(node_hdr(l[i], c)) == 0,
                forall|p: *mut CacheHeader|
                    old(self).owns(p) && (forall|c: int| 0 <= c < NCLASSES ==> p != #[trigger] old(self).home_ptr(c))
                        && (forall|i: int, c: int| 0 <= i < l.len() - r && 0 <= c < NCLASSES ==> p != #[trigger] node_hdr(l[i], c))
                        ==> #[trigger] self.held(p) == old(self).held(p),
                log_adds(self.released(), old(self).released(), old(self).held_upto(l.len() - r)),
            decreases r,
        {
            let ghost h0 = *self;
            let ghost d = l.len() - r;
            proof {
                self.lemma_list(r);
                old(self).lemma_list(r);
                assert(old(self).node_from_end(r) == l[d]);
                assert(o == l[d]);
                // `o`'s headers are neither home headers nor those of a node
                // already flushed, so they still hold what they held.
                old(self).lemma_array_hdr(o);
                assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] h0.held(h0.array_hdr(o, c)) == old(self).held(old(self).array_hdr(o, c)) by {
                    let p = node_hdr(o, c);
                    assert(old(self).array_hdr(o, c) == p);
                    assert(h0.array_hdr(o, c) == p);
                    assert forall|k: int| 0 <= k < NCLASSES implies p != #[trigger] old(self).home_ptr(k) by {
                        old(self).lemma_home_apart(k, d, c);
                    }
                    assert forall|i: int, k: int| 0 <= i < l.len() - r && 0 <= k < NCLASSES implies p != #[trigger] node_hdr(l[i], k) by {
                        old(self).lemma_nodes_apart(d, i, c, k);
                    }
                }
                Heap::lemma_array_held_same(h0, *old(self), o, NCLASSES as int);
            }
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            let next = unsafe { self.node_next(o, Ghost(r)) };
            let arena = unsafe { self.node_arena(o, Ghost(r)) };
            self.flush_arena(shared, arena, o, Tracked(ord));
            proof {
                Heap::lemma_keeps_trans(*old(self), h0, *self);
                if r > 1 {
                    Heap::lemma_keeps_list(h0, *self, r - 1);
                }
                lemma_log_trans(old(self).released(), h0.released(), self.released(), old(self).held_upto(d), old(self).array_held(o, NCLASSES as int));
                assert(old(self).held_upto(d + 1) == old(self).held_upto(d) + old(self).array_held(l[d], NCLASSES as int));
                // Nothing outside `o` changed.
                assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] self.cached_len(old(self).home_ptr(c)) == 0 by {
                    let p = old(self).home_ptr(c);
                    assert forall|k: int| 0 <= k < NCLASSES implies p != #[trigger] h0.array_hdr(o, k) by {
                        old(self).lemma_home_apart(c, d, k);
                    }
                    assert(self.held(p) == h0.held(p));
                    assert(self.held(p).len() == self.cached_len(p));
                    assert(h0.held(p).len() == h0.cached_len(p));
                }
                assert forall|i: int, c: int| 0 <= i < d + 1 && 0 <= c < NCLASSES implies #[trigger] self.cached_len(node_hdr(l[i], c)) == 0 by {
                    if i < d {
                        let p = node_hdr(l[i], c);
                        assert forall|k: int| 0 <= k < NCLASSES implies p != #[trigger] h0.array_hdr(o, k) by {
                            old(self).lemma_nodes_apart(i, d, c, k);
                        }
                        assert(self.held(p) == h0.held(p));
                        assert(self.held(p).len() == self.cached_len(p));
                        assert(h0.held(p).len() == h0.cached_len(p));
                    } else {
                        assert(self.cached_len(h0.array_hdr(o, c)) == 0);
                    }
                }
                assert forall|p: *mut CacheHeader|
                    old(self).owns(p) && (forall|c: int| 0 <= c < NCLASSES ==> p != #[trigger] old(self).home_ptr(c))
                        && (forall|i: int, c: int| 0 <= i < d + 1 && 0 <= c < NCLASSES ==> p != #[trigger] node_hdr(l[i], c))
                        implies #[trigger] self.held(p) == old(self).held(p) by {
                    assert forall|k: int| 0 <= k < NCLASSES implies p != #[trigger] h0.array_hdr(o, k) by {
                        assert(p != node_hdr(l[d], k));
                    }
                    assert(h0.owns(p));
                    assert(self.held(p) == h0.held(p));
                }
                r = r - 1;
            }
            o = next;
        }
        proof {
            self.lemma_list(r);
            if r > 0 {
                assert(o@.addr != 0);
            }
            assert(l.len() - r == l.len());
            assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] self.cached_len(self.home_ptr(c)) == 0 by {
                assert(self.home_ptr(c) == old(self).home_ptr(c));
            }
            assert forall|i: int, c: int| 0 <= i < self.list@.len() && 0 <= c < NCLASSES implies #[trigger] self.cached_len(node_hdr(self.list@[i], c)) == 0 by {
                assert(self.list@[i] == l[i]);
            }
        }
    }

    /// A home header is no header of a node on the `others` list.
    proof fn lemma_home_apart(&self, c: int, i: int, k: int)
        requires
            self.wf(),
            0 <= c < NCLASSES,
            0 <= i < self.list@.len(),
            0 <= k < NCLASSES,
        ensures
            self.home_ptr(c) != node_hdr(self.list@[i], k),
    {
        assert(self.home_hdrs@[c] != node_hdr(self.list@[i], k));
    }

    /// The headers of two different nodes on the `others` list are
    /// different headers.
    proof fn lemma_nodes_apart(&self, i: int, j: int, c: int, k: int)
        requires
            self.wf(),
            0 <= i < self.list@.len(),
            0 <= j < self.list@.len(),
            i != j,
            0 <= c < NCLASSES,
            0 <= k < NCLASSES,
        ensures
            node_hdr(self.list@[i], c) != node_hdr(self.list@[j], k),
    {
        let a = self.list@[i];
        let b = self.list@[j];
        assert(self.owns_node(a));
        assert(self.owns_node(b));
        if node_hdr(a, c) == node_hdr(b, k) {
            assert(self.hdr_class(node_hdr(a, c)) == c);
            assert(self.hdr_class(node_hdr(b, k)) == k);
            assert(node_hdr(a, c)@.addr == node_hdr(b, k)@.addr);
            assert(a@.addr == b@.addr);
            assert(a@.provenance == b@.provenance);
            assert(a@ == b@);
            assert(a == b);
            if i < j {
                assert(self.list@[i] != self.list@[j]);
            } else {
                assert(self.list@[j] != self.list@[i]);
            }
        }
    }

    /// [`Heap::array_held`] depends only on what the array's headers hold.
    proof fn lemma_array_held_same(a: Heap, b: Heap, node: *mut ArenaCaches, k: int)
        requires
            forall|c: int| 0 <= c < k ==> #[trigger] a.held(a.array_hdr(node, c)) == b.held(b.array_hdr(node, c)),
        ensures
            a.array_held(node, k) == b.array_held(node, k),
        decreases k,
    {
        if k > 0 {
            Heap::lemma_array_held_same(a, b, node, k - 1);
        }
    }

    /// Cache GC (design 6.4): for every cache, release three quarters of
    /// the entries that went unused since the last GC, then restart the
    /// low-water tracking from the current length.
    ///
    /// What it leaves (whole-allocator theorem "cache flush
    /// completeness"): every home header and every header of a node on the
    /// `others` list as [`Heap::gc_left`] says: a home header emptied if
    /// the home arena is `FOREIGN`, else trimmed; a node's header emptied
    /// or trimmed (the node's arena is not modelled, hence the
    /// disjunction; [`Heap::gc_arena`] states the exact case per array).
    pub fn gc(&mut self, shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            shared.wf(),
            old(self).wf(),
        ensures
            *final(ord) == *old(ord),
            final(self).keeps(*old(self)),
            final(self).same_shape(*old(self)),
            final(self).gc_all(*old(self)),
    {
        let home = self.arena;
        self.gc_arena(shared, home, ptr::null_mut(), Tracked(ord));
        // The walk of `flush_all`.
        let mut o = self.others;
        let ghost mut r = self.list_len();
        let ghost l = old(self).list@;
        proof {
            self.lemma_list(r);
            assert forall|c: int| #![trigger old(self).home_ptr(c)] 0 <= c < NCLASSES implies self.gc_left(*old(self), old(self).home_ptr(c), old(self).home() == FOREIGN) by {
                assert(old(self).array_hdr(ptr::null_mut(), c) == old(self).home_ptr(c));
            }
            assert forall|p: *mut CacheHeader| #![trigger self.hdr(p)] #![trigger self.held(p)]
                old(self).owns(p) && (forall|c: int| 0 <= c < NCLASSES ==> p != #[trigger] old(self).home_ptr(c))
                    implies self.hdr(p) == old(self).hdr(p) && self.held(p) == old(self).held(p) by {
                assert forall|c: int| 0 <= c < NCLASSES implies p != #[trigger] old(self).array_hdr(ptr::null_mut(), c) by {
                    assert(p != old(self).home_ptr(c));
                }
                Heap::lemma_kept_apart(*old(self), *self, ptr::null_mut(), p);
            }
        }
        while !o.is_null()
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
                shared.wf(),
                self.keeps(*old(self)),
                self.same_shape(*old(self)),
                l == old(self).list@,
                old(self).wf(),
                0 <= r <= self.list_len(),
                r > 0 ==> o == self.node_from_end(r),
                r == 0 ==> o@.addr == 0,
                forall|c: int| #![trigger old(self).home_ptr(c)] 0 <= c < NCLASSES ==> self.gc_left(*old(self), old(self).home_ptr(c), old(self).home() == FOREIGN),
                forall|i: int, c: int|
                    0 <= i < l.len() - r && 0 <= c < NCLASSES
                        ==> #[trigger] self.gc_left(*old(self), node_hdr(l[i], c), false) || self.gc_left(*old(self), node_hdr(l[i], c), true),
                forall|p: *mut CacheHeader| #![trigger self.hdr(p)] #![trigger self.held(p)]
                    old(self).owns(p) && (forall|c: int| 0 <= c < NCLASSES ==> p != #[trigger] old(self).home_ptr(c))
                        && (forall|i: int, c: int| 0 <= i < l.len() - r && 0 <= c < NCLASSES ==> p != #[trigger] node_hdr(l[i], c))
                        ==> self.hdr(p) == old(self).hdr(p) && self.held(p) == old(self).held(p),
            decreases r,
        {
            let ghost h0 = *self;
            let ghost d = l.len() - r;
            proof {
                self.lemma_list(r);
                old(self).lemma_list(r);
                assert(old(self).node_from_end(r) == l[d]);
                assert(o == l[d]);
                // `o`'s headers are neither home headers nor those of a node
                // already collected, so they still hold what they held.
                old(self).lemma_array_hdr(o);
                assert forall|c: int| #![trigger h0.hdr(node_hdr(o, c))] #![trigger h0.held(node_hdr(o, c))]
                    0 <= c < NCLASSES implies h0.hdr(node_hdr(o, c)) == old(self).hdr(node_hdr(o, c))
                    && h0.held(node_hdr(o, c)) == old(self).held(node_hdr(o, c)) by {
                    let p = node_hdr(o, c);
                    assert(old(self).array_hdr(o, c) == p);
                    assert forall|k: int| 0 <= k < NCLASSES implies p != #[trigger] old(self).home_ptr(k) by {
                        old(self).lemma_home_apart(k, d, c);
                    }
                    assert forall|i: int, k: int| 0 <= i < l.len() - r && 0 <= k < NCLASSES implies p != #[trigger] node_hdr(l[i], k) by {
                        old(self).lemma_nodes_apart(d, i, c, k);
                    }
                    assert(self.hdr(p) == old(self).hdr(p));
                    assert(self.held(p) == old(self).held(p));
                }
            }
            // SAFETY: as in `flush_all`.
            let next = unsafe { self.node_next(o, Ghost(r)) };
            let arena = unsafe { self.node_arena(o, Ghost(r)) };
            self.gc_arena(shared, arena, o, Tracked(ord));
            proof {
                Heap::lemma_keeps_trans(*old(self), h0, *self);
                Heap::lemma_same_trans(*old(self), h0, *self);
                if r > 1 {
                    Heap::lemma_keeps_list(h0, *self, r - 1);
                }
                // `o`'s headers, left as `gc_left` says from `h0`, which had
                // their old values.
                assert forall|c: int| #![trigger node_hdr(l[d], c)] 0 <= c < NCLASSES implies self.gc_left(*old(self), node_hdr(l[d], c), arena == FOREIGN) by {
                    assert(h0.array_hdr(o, c) == node_hdr(o, c));
                    assert(old(self).array_hdr(o, c) == node_hdr(o, c));
                    assert(self.gc_left(h0, node_hdr(o, c), arena == FOREIGN));
                }
                // Nothing outside `o` changed.
                assert forall|c: int| #![trigger old(self).home_ptr(c)] 0 <= c < NCLASSES implies self.gc_left(*old(self), old(self).home_ptr(c), old(self).home() == FOREIGN) by {
                    let p = old(self).home_ptr(c);
                    assert forall|k: int| 0 <= k < NCLASSES implies p != #[trigger] h0.array_hdr(o, k) by {
                        old(self).lemma_home_apart(c, d, k);
                    }
                    assert(h0.owns(p));
                    Heap::lemma_kept_apart(h0, *self, o, p);
                    assert(self.hdr(p) == h0.hdr(p));
                    assert(self.held(p) == h0.held(p));
                    assert(h0.gc_left(*old(self), p, old(self).home() == FOREIGN));
                }
                assert forall|i: int, c: int| 0 <= i < d + 1 && 0 <= c < NCLASSES implies #[trigger] self.gc_left(*old(self), node_hdr(l[i], c), false)
                    || self.gc_left(*old(self), node_hdr(l[i], c), true) by {
                    if i < d {
                        let p = node_hdr(l[i], c);
                        assert forall|k: int| 0 <= k < NCLASSES implies p != #[trigger] h0.array_hdr(o, k) by {
                            old(self).lemma_nodes_apart(i, d, c, k);
                        }
                        assert(old(self).owns_node(l[i]));
                        assert(h0.owns(p));
                        Heap::lemma_kept_apart(h0, *self, o, p);
                        assert(self.hdr(p) == h0.hdr(p));
                        assert(self.held(p) == h0.held(p));
                        assert(h0.gc_left(*old(self), p, false) || h0.gc_left(*old(self), p, true));
                    } else {
                        assert(self.gc_left(*old(self), node_hdr(l[d], c), arena == FOREIGN));
                    }
                }
                assert forall|p: *mut CacheHeader| #![trigger self.hdr(p)] #![trigger self.held(p)]
                    old(self).owns(p) && (forall|c: int| 0 <= c < NCLASSES ==> p != #[trigger] old(self).home_ptr(c))
                        && (forall|i: int, c: int| 0 <= i < d + 1 && 0 <= c < NCLASSES ==> p != #[trigger] node_hdr(l[i], c))
                        implies self.hdr(p) == old(self).hdr(p) && self.held(p) == old(self).held(p) by {
                    assert forall|k: int| 0 <= k < NCLASSES implies p != #[trigger] h0.array_hdr(o, k) by {
                        assert(p != node_hdr(l[d], k));
                    }
                    assert(h0.owns(p));
                    Heap::lemma_kept_apart(h0, *self, o, p);
                }
                r = r - 1;
            }
            o = next;
        }
        proof {
            self.lemma_list(r);
            if r > 0 {
                assert(o@.addr != 0);
            }
            assert(l.len() - r == l.len());
        }
    }

    /// One GC step over one array (the home array when `node` is null,
    /// else the node's, whose arena is `arena`): every header of it is
    /// left as [`Heap::gc_left`] says (emptied when `arena` is `FOREIGN`),
    /// and no other header changes.
    #[allow(clippy::while_let_loop, reason = "the desugared `for`, which Verus needs for `continue`")]
    fn gc_arena(&mut self, shared: &Shared, arena: u16, node: *mut ArenaCaches, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            shared.wf(),
            old(self).wf(),
            node@.addr == 0 || old(self).owns_node(node),
        ensures
            *final(ord) == *old(ord),
            final(self).keeps(*old(self)),
            final(self).same_shape(*old(self)),
            forall|c: int| #![trigger old(self).array_hdr(node, c)] 0 <= c < NCLASSES ==> final(self).gc_left(*old(self), old(self).array_hdr(node, c), arena == FOREIGN),
            final(self).kept_apart(*old(self), node),
    {
        proof {
            Heap::lemma_keeps_refl(*self);
            old(self).lemma_array_hdr(node);
            assert(self.kept_outside(*old(self), node, 0));
        }
        // `for class in 0..NCLASSES`, spelled out: Verus rejects `continue`
        // in a `for` loop.
        let mut iter = 0..NCLASSES;
        loop
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
                iter.end == NCLASSES,
                iter.start <= NCLASSES,
                old(self).wf(),
                self.keeps(*old(self)),
                self.same_shape(*old(self)),
                node@.addr == 0 || old(self).owns_node(node),
                shared.wf(),
                forall|c: int| 0 <= c < NCLASSES ==> old(self).owns(#[trigger] old(self).array_hdr(node, c)) && old(self).hdr_class(old(self).array_hdr(node, c)) == c,
                forall|c: int| #![trigger old(self).array_hdr(node, c)] 0 <= c < iter.start ==> self.gc_left(*old(self), old(self).array_hdr(node, c), arena == FOREIGN),
                self.kept_outside(*old(self), node, iter.start as int),
            ensures
                iter.start == NCLASSES,
            decreases NCLASSES - iter.start,
        {
            let class = match iter.next() {
                Some(c) => c,
                None => break,
            };
            let ghost h0 = *self;
            let ghost hc = old(self).array_hdr(node, class as int);
            proof {
                if node@.addr != 0 {
                    Heap::lemma_keeps_node(*old(self), *self, node);
                }
                assert(self.array_hdr(node, class as int) == hc);
                assert(iter.start as int == class + 1);
            }
            // SAFETY: `node` is null or a live node owned by this heap; an
            // unused header (`cap == 0`) is skipped.
            unsafe {
                let cache = self.header_in(node, class);
                if self.hdr_ref(cache).cap == 0 {
                    proof {
                        Heap::lemma_gc_step(*old(self), h0, *self, node, hc, class as int, iter.start as int, arena == FOREIGN, true);
                    }
                    continue;
                }
                let lw = self.hdr_ref(cache).low_water as usize;
                if arena == FOREIGN {
                    // Never popped: everything it holds went unused.
                    let len = self.hdr_ref(cache).len as usize;
                    if len > 0 {
                        self.flush(shared, arena, cache, class, len, Tracked(ord));
                        proof {
                            Heap::lemma_keeps_trans(*old(self), h0, *self);
                            Heap::lemma_same_trans(*old(self), h0, *self);
                            // Cache routing: the `FOREIGN` cache is released whole.
                            assert(self.cached_len(cache) == 0);
                        }
                    }
                } else if lw > 0 {
                    self.flush(shared, arena, cache, class, lw - lw / 4, Tracked(ord));
                    proof {
                        Heap::lemma_keeps_trans(*old(self), h0, *self);
                        Heap::lemma_same_trans(*old(self), h0, *self);
                    }
                }
                let ghost h1 = *self;
                proof {
                    assert(h1.hdr(cache).len == if arena == FOREIGN { 0 } else { h0.hdr(cache).len - (lw - lw / 4) });
                    assert(h1.held(cache).to_set() <= h0.held(cache).to_set());
                    assert(Heap::frame(h0, h1, cache));
                }
                let h = self.hdr_mut(cache);
                h.low_water = h.len;
                proof {
                    Heap::lemma_set(h1, *self, cache, *h);
                    Heap::lemma_updates_held(h1, *self, cache, *h);
                    assert(self.perms@.hdrs.dom() =~= h1.perms@.hdrs.dom());
                    Heap::lemma_keeps_trans(*old(self), h1, *self);
                    Heap::lemma_same_trans(*old(self), h1, *self);
                    assert(self.gc_left(h0, hc, arena == FOREIGN));
                    assert(Heap::frame(h1, *self, cache));
                    assert(Heap::frame(h0, *self, cache));
                    Heap::lemma_gc_step(*old(self), h0, *self, node, hc, class as int, iter.start as int, arena == FOREIGN, false);
                }
            }
        }
        proof {
            reveal(Heap::kept_apart);
            assert forall|p: *mut CacheHeader| #![trigger self.hdr(p)] #![trigger self.held(p)]
                old(self).owns(p) && (forall|c: int| 0 <= c < NCLASSES ==> p != #[trigger] old(self).array_hdr(node, c))
                    implies self.hdr(p) == old(self).hdr(p) && self.held(p) == old(self).held(p) by {
                if old(self).in_array(node, NCLASSES as int, p) {
                    assert(p != old(self).array_hdr(node, old(self).hdr_class(p)));
                }
            }
        }
    }

    /// Every header `old` owns outside the array `node` names keeps its
    /// value and what it holds: what [`Heap::gc_arena`] leaves. Opaque, so
    /// its callers use it through [`Heap::lemma_kept_apart`], one header
    /// at a time, instead of matching a nested quantifier under their own
    /// context.
    // Each conjunct is a trigger: a query that needs only the `held`
    // conjunct may have no `hdr` term to match.
    #[verifier::opaque]
    closed spec fn kept_apart(&self, old: Heap, node: *mut ArenaCaches) -> bool {
        forall|p: *mut CacheHeader| #![trigger self.hdr(p)] #![trigger self.held(p)]
            old.owns(p) && (forall|c: int| 0 <= c < NCLASSES ==> p != #[trigger] old.array_hdr(node, c))
                ==> self.hdr(p) == old.hdr(p) && self.held(p) == old.held(p)
    }

    /// [`Heap::kept_apart`] at header `p`.
    proof fn lemma_kept_apart(old: Heap, new: Heap, node: *mut ArenaCaches, p: *mut CacheHeader)
        requires
            new.kept_apart(old, node),
            old.owns(p),
            forall|c: int| 0 <= c < NCLASSES ==> p != #[trigger] old.array_hdr(node, c),
        ensures
            new.hdr(p) == old.hdr(p),
            new.held(p) == old.held(p),
    {
        reveal(Heap::kept_apart);
    }

    /// Owned header `p` is the header of the array `node` names for one of
    /// the classes below `k` ([`Heap::array_hdr`]).
    closed spec fn in_array(&self, node: *mut ArenaCaches, k: int, p: *mut CacheHeader) -> bool {
        &&& 0 <= self.hdr_class(p) < k
        &&& p == self.array_hdr(node, self.hdr_class(p))
    }

    /// Every header `old` owns outside the first `k` classes of the array
    /// `node` names ([`Heap::in_array`]) has its value and holds what it
    /// held in `old`.
    closed spec fn kept_outside(&self, old: Heap, node: *mut ArenaCaches, k: int) -> bool {
        forall|p: *mut CacheHeader|
            old.owns(p) && !old.in_array(node, k, p) ==> #[trigger] self.hdr(p) == old.hdr(p) && self.held(p) == old.held(p)
    }

    /// [`Heap::kept_outside`] at one header.
    proof fn lemma_kept_at(h: Heap, old: Heap, node: *mut ArenaCaches, k: int, p: *mut CacheHeader)
        requires
            h.kept_outside(old, node, k),
            old.owns(p),
            !old.in_array(node, k, p),
        ensures
            h.hdr(p) == old.hdr(p),
            h.held(p) == old.held(p),
    {
        assert(h.hdr(p) == old.hdr(p) && h.held(p) == old.held(p));
    }

    /// [`Heap::frame`] at one header.
    proof fn lemma_frame_at(a: Heap, b: Heap, cache: *mut CacheHeader, p: *mut CacheHeader)
        requires
            Heap::frame(a, b, cache),
            a.owns(p),
            p != cache,
        ensures
            b.hdr(p) == a.hdr(p),
            b.held(p) == a.held(p),
    {
        assert(b.hdr(p) == a.hdr(p) && b.held(p) == a.held(p));
    }

    /// From `a` to `b` no owned header but `cache` changed its value or
    /// what it holds.
    closed spec fn frame(a: Heap, b: Heap, cache: *mut CacheHeader) -> bool {
        forall|p: *mut CacheHeader| a.owns(p) && p != cache ==> #[trigger] b.hdr(p) == a.hdr(p) && b.held(p) == a.held(p)
    }

    /// A header store that keeps its length and stack keeps what every
    /// owned header holds, and every other header's value.
    proof fn lemma_updates_held(old: Heap, new: Heap, cache: *mut CacheHeader, v: CacheHeader)
        requires
            old.owns(cache),
            new.updates(old, cache, v),
            v.len == old.hdr(cache).len,
            v.slots == old.hdr(cache).slots,
        ensures
            forall|p: *mut CacheHeader| old.owns(p) ==> #[trigger] new.held(p) == old.held(p),
            forall|p: *mut CacheHeader| old.owns(p) && p != cache ==> #[trigger] new.hdr(p) == old.hdr(p),
    {
        assert forall|p: *mut CacheHeader| old.owns(p) implies #[trigger] new.held(p) == old.held(p) by {
            assert(new.cached_len(p) == old.cached_len(p));
            assert forall|i: int| 0 <= i < old.cached_len(p) implies new.cached(p, i) == old.cached(p, i) by {
                assert(new.perms@.ent(p, i) == old.perms@.ent(p, i));
            }
            assert(new.held(p) =~= old.held(p));
        }
    }

    /// [`Heap::gc_arena`]'s loop step for class `class`, whose header in
    /// the array is `hc`: from `h0`, the heap at the top of the step, `h`
    /// differs only in `hc`, left as [`Heap::gc_left`] says relative to
    /// `h0`, which still had `old`'s value of it (or skipped, unused). So
    /// the loop invariant holds again with `next`, the class after
    /// `class`.
    #[verifier::spinoff_prover]
    proof fn lemma_gc_step(old: Heap, h0: Heap, h: Heap, node: *mut ArenaCaches, hc: *mut CacheHeader, class: int, next: int, foreign: bool, skipped: bool)
        requires
            0 <= class < NCLASSES,
            next == class + 1,
            hc == old.array_hdr(node, class),
            forall|c: int| 0 <= c < NCLASSES ==> old.owns(#[trigger] old.array_hdr(node, c)) && old.hdr_class(old.array_hdr(node, c)) == c,
            forall|c: int| #![trigger old.array_hdr(node, c)] 0 <= c < class ==> h0.gc_left(old, old.array_hdr(node, c), foreign),
            h0.kept_outside(old, node, class),
            h0.same_shape(old),
            skipped ==> h == h0 && h0.hdr(hc).cap == 0 && h0.hdr(hc).inv(),
            !skipped ==> h.gc_left(h0, hc, foreign),
            Heap::frame(h0, h, hc),
        ensures
            forall|c: int| #![trigger old.array_hdr(node, c)] 0 <= c < next ==> h.gc_left(old, old.array_hdr(node, c), foreign),
            h.kept_outside(old, node, next),
    {
        assert(old.owns(hc));
        assert(old.hdr_class(hc) == class);
        assert(!old.in_array(node, class, hc));
        Heap::lemma_kept_at(h0, old, node, class, hc);
        assert forall|c: int| #![trigger old.array_hdr(node, c)] 0 <= c < next implies h.gc_left(old, old.array_hdr(node, c), foreign) by {
            let p = old.array_hdr(node, c);
            if c < class {
                assert(old.hdr_class(p) == c);
                assert(h0.gc_left(old, p, foreign));
                assert(p != hc);
                assert(h0.owns(p));
                Heap::lemma_frame_at(h0, h, hc, p);
            } else if skipped {
                // An unused header: nothing cached, a low-water mark of 0.
                assert(h0.hdr(hc).len == 0);
                assert(h.held(hc) =~= old.held(hc));
            }
        }
        assert forall|p: *mut CacheHeader|
            old.owns(p) && !old.in_array(node, next, p) implies #[trigger] h.hdr(p) == old.hdr(p) && h.held(p) == old.held(p) by {
            assert(p != hc);
            assert(!old.in_array(node, class, p));
            Heap::lemma_kept_at(h0, old, node, class, p);
            assert(h0.owns(p));
            Heap::lemma_frame_at(h0, h, hc, p);
        }
        assert(h.kept_outside(old, node, next));
    }

    /// `thread.tcache.enabled` write. Turning caches off folds the derived
    /// counters into `uncached` (so `thread.allocated` keeps counting),
    /// flushes and frees every cache of every arena, and makes
    /// `ensure_cache` refuse to create new ones; turning them on lets the
    /// next slow path create caches again.
    ///
    /// The heap stays well formed: the headers are reset through their
    /// permissions ([`Heap::free_caches_in`]). Whole-allocator theorem
    /// "cache flush completeness": turning caches on changes no cache;
    /// turning them off leaves every home header and every header of a
    /// node on the `others` list without a stack ([`Heap::cleared`]), so
    /// caching nothing ([`Heap::drained`]), with the released log extended
    /// by exactly what they held, as [`Heap::flush_all`] states.
    pub fn set_tcache_enabled(&mut self, shared: &Shared, on: bool, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            shared.wf(),
            old(self).wf(),
        ensures
            *final(ord) == *old(ord),
            final(self).wf(),
            final(self).tcache_on() == on,
            on ==> final(self).caches_same(*old(self)),
            !on ==> final(self).same_shape(*old(self)),
            !on ==> final(self).cleared(),
            !on ==> final(self).drained(),
            !on ==> log_adds(final(self).released(), old(self).released(), old(self).held_all()),
    {
        self.tcache_enabled = on;
        if on {
            return;
        }
        let ghost h_off = *self;
        self.flush_all(shared, Tracked(ord));
        let ghost hf = *self;
        proof {
            Heap::lemma_held_all_same(*old(self), h_off);
        }
        Self::fold_counters(&mut self.uncached, &self.caches);
        proof {
            assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] self.cached_len(self.array_hdr(ptr::null_mut(), c)) == 0 by {
                assert(hf.cached_len(hf.home_ptr(c)) == 0);
            }
        }
        let ghost h2 = *self;
        self.free_caches_in(shared, ptr::null_mut(), Tracked(ord));
        // The walk of `flush_all`. Folding and freeing a node's headers
        // leaves the list as it is.
        let mut o = self.others;
        let ghost mut r = self.list_len();
        let ghost l = old(self).list@;
        proof {
            self.lemma_list(r);
            assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] self.hdr_cap(self.home_ptr(c)) == 0 by {
                assert(h2.array_hdr(ptr::null_mut(), c) == self.home_ptr(c));
                assert(self.hdr_cap(h2.array_hdr(ptr::null_mut(), c)) == 0);
            }
            assert forall|i: int, c: int| 0 <= i < l.len() && 0 <= c < NCLASSES implies #[trigger] self.cached_len(node_hdr(l[i], c)) == 0 by {
                let p = node_hdr(l[i], c);
                assert(hf.owns_node(l[i]));
                assert forall|k: int| 0 <= k < NCLASSES implies p != #[trigger] h2.array_hdr(ptr::null_mut(), k) by {
                    hf.lemma_home_apart(k, i, c);
                }
                assert(self.hdr(p) == h2.hdr(p));
                assert(hf.cached_len(node_hdr(hf.list@[i], c)) == 0);
            }
        }
        while !o.is_null()
            invariant
                self.tcache_on() == on,
                crate::sys::may_syscall(),
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
                self.wf(),
                self.same_shape(*old(self)),
                self.released() == hf.released(),
                l == old(self).list@,
                0 <= r <= self.list_len(),
                r > 0 ==> o == self.node_from_end(r),
                r == 0 ==> o@.addr == 0,
                forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.hdr_cap(self.home_ptr(c)) == 0,
                forall|i: int, c: int| 0 <= i < l.len() - r && 0 <= c < NCLASSES ==> #[trigger] self.hdr_cap(node_hdr(l[i], c)) == 0,
                forall|i: int, c: int| 0 <= i < l.len() && 0 <= c < NCLASSES ==> #[trigger] self.cached_len(node_hdr(l[i], c)) == 0,
            decreases r,
        {
            let ghost d = l.len() - r;
            proof {
                self.lemma_list(r);
                assert(o == l[d]);
            }
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            let next = unsafe { self.node_next(o, Ghost(r)) };
            let ghost h = *self;
            unsafe {
                Self::fold_counters(&mut self.uncached, node_caches_detached(o, Ghost(h), Ghost(r)));
            }
            proof {
                assert(self.owns_node(o));
                assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] self.cached_len(self.array_hdr(o, c)) == 0 by {
                    assert(self.array_hdr(o, c) == node_hdr(l[d], c));
                    assert(h.cached_len(node_hdr(l[d], c)) == 0);
                }
            }
            let ghost h1 = *self;
            self.free_caches_in(shared, o, Tracked(ord));
            proof {
                Heap::lemma_same_trans(*old(self), h1, *self);
                // Only `o`'s headers changed.
                assert forall|k: int, c: int| 0 <= k < l.len() && 0 <= c < NCLASSES && k != d
                    implies #[trigger] self.hdr(node_hdr(l[k], c)) == h1.hdr(node_hdr(l[k], c)) by {
                    assert(h1.owns_node(l[k]));
                    assert forall|j: int| 0 <= j < NCLASSES implies node_hdr(l[k], c) != #[trigger] h1.array_hdr(o, j) by {
                        h1.lemma_nodes_apart(k, d, c, j);
                    }
                }
                assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] self.hdr_cap(self.home_ptr(c)) == 0 by {
                    let p = self.home_ptr(c);
                    assert forall|j: int| 0 <= j < NCLASSES implies p != #[trigger] h1.array_hdr(o, j) by {
                        h1.lemma_home_apart(c, d, j);
                    }
                    assert(self.hdr(p) == h1.hdr(p));
                    assert(h.hdr_cap(h.home_ptr(c)) == 0);
                }
                assert forall|i: int, c: int| 0 <= i < d + 1 && 0 <= c < NCLASSES implies #[trigger] self.hdr_cap(node_hdr(l[i], c)) == 0 by {
                    if i == d {
                        assert(h1.array_hdr(o, c) == node_hdr(l[i], c));
                        assert(self.hdr_cap(h1.array_hdr(o, c)) == 0);
                    } else {
                        assert(self.hdr(node_hdr(l[i], c)) == h1.hdr(node_hdr(l[i], c)));
                        assert(h.hdr_cap(node_hdr(l[i], c)) == 0);
                    }
                }
                assert forall|i: int, c: int| 0 <= i < l.len() && 0 <= c < NCLASSES implies #[trigger] self.cached_len(node_hdr(l[i], c)) == 0 by {
                    if i == d {
                        assert(h1.array_hdr(o, c) == node_hdr(l[i], c));
                        assert(self.owns(node_hdr(l[i], c)));
                        assert(self.hdr_cap(h1.array_hdr(o, c)) == 0);
                        self.lemma_cap_len(node_hdr(l[i], c));
                    } else {
                        assert(self.hdr(node_hdr(l[i], c)) == h1.hdr(node_hdr(l[i], c)));
                        assert(h.cached_len(node_hdr(l[i], c)) == 0);
                    }
                }
                r = r - 1;
            }
            o = next;
        }
        proof {
            self.lemma_list(r);
            if r > 0 {
                assert(o@.addr != 0);
            }
            assert(self.list@ == l);
            self.lemma_cleared_drained();
        }
    }

    /// Change the home arena (`thread.arena`). The caches of the old home
    /// move to the `others` list and the new home's, if any, take their
    /// place, so nothing is flushed. `arena` must exist.
    ///
    /// The heap stays well formed: the exchange goes header by header
    /// through their permissions ([`Heap::swap_hdrs`]).
    pub fn set_arena(&mut self, shared: &Shared, arena: u16, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            shared.wf(),
            old(self).wf(),
            (arena as usize) < crate::arena::MAX_ARENAS,
        ensures
            *final(ord) == *old(ord),
            final(self).wf(),
    {
        if arena == self.arena {
            return;
        }
        let old = self.arena;
        let node = self.other_caches(shared, arena, true, Tracked(ord));
        let ghost h1 = *self;
        if node.is_null() {
            // No metadata for a new node: the caches of the new home start
            // empty and the old home's must go back first.
            self.drop_home_caches(shared, Tracked(ord));
        } else {
            // The node's headers and the home headers trade places, and
            // the node becomes the cache of `old`, the home arena being
            // left.
            for c in 0..NCLASSES
                invariant
                    self.wf(),
                    self.owns_node(node),
                    self.in_list(node),
                    self.list == h1.list,
                    self.keys == h1.keys,
                    self.arena == h1.arena,
                    self.heads == h1.heads,
            {
                let a = self.cache_ptr(c);
                // SAFETY: `node` is a live block owned by this heap.
                let b = unsafe { Self::node_cache_ptr(node, c) };
                let ghost h0 = *self;
                self.swap_hdrs(a, b);
                proof {
                    Heap::lemma_same_node(h0, *self, node);
                }
            }
            let ghost i = choose|i: int| 0 <= i < h1.list@.len() && h1.list@[i] == node && #[trigger] h1.keys@[i] == arena;
            proof {
                h1.lemma_heads();
                reveal(others::heads_ok);
                assert(others::head_ok(h1.heads@, h1.list@, h1.keys@, i));
            }
            let tracked mut hd = self.heads.borrow_mut().tracked_remove(node);
            // SAFETY: as above.
            unsafe { node_set_arena(node, old, Tracked(&mut hd.arena)) };
            proof {
                self.heads.borrow_mut().tracked_insert(node, hd);
                // The node keyed `arena` (the only one, keys being
                // distinct) is now keyed `old`, which no node was; the
                // other nodes' first lines are as they were.
                self.keys = Ghost(h1.keys@.update(i, old));
                reveal(others::heads_ok);
                assert forall|j: int| 0 <= j < self.list@.len() implies #[trigger] others::head_ok(self.heads@, self.list@, self.keys@, j) by {
                    assert(others::head_ok(h1.heads@, h1.list@, h1.keys@, j));
                    if j < i {
                        assert(h1.list@[j] != h1.list@[i]);
                    } else if j > i {
                        assert(h1.list@[i] != h1.list@[j]);
                    }
                }
                assert forall|j: int| 0 <= j < self.keys@.len() implies #[trigger] self.keys@[j] != arena by {
                    if j != i {
                        if j < i {
                            assert(h1.keys@[j] != h1.keys@[i]);
                        } else {
                            assert(h1.keys@[i] != h1.keys@[j]);
                        }
                    }
                }
                assert forall|j: int, k: int| 0 <= j < k < self.keys@.len() implies self.keys@[j] != self.keys@[k] by {
                    assert(h1.keys@[j] != old && h1.keys@[k] != old);
                    assert(h1.keys@[j] != h1.keys@[k]);
                }
            }
        }
        self.arena = arena;
        if self.bound {
            proof {
                shared.lemma_wf();
            }
            // The ticket for the old home goes back and one for the new
            // home takes its place (`homed_ok`).
            let tracked t = self.homed.borrow_mut().tracked_take();
            proof {
                shared.arenas.lemma_homed(t, old as int);
            }
            shared.arenas.unbind(old, Tracked(t));
            let Tracked(t) = shared.arenas.bind(arena);
            proof {
                shared.arenas.lemma_homed(t, arena as int);
                self.homed = Tracked(Some(t));
            }
        }
    }

    /// Count a slow-path entry and run periodic work every
    /// [`PERIODIC_EVERY`] of them: cache GC when the interval has elapsed,
    /// then decay (or starting the background thread that will do it,
    /// `purge::on_tick`). Nothing else: thread statistics are merged by
    /// `epoch` (`stats.rs`), not here.
    fn tick(&mut self, shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            old(self).wf(),
            shared.wf(),
        ensures
            *final(ord) == *old(ord),
            final(self).keeps(*old(self)),
    {
        self.slow_ticks = self.slow_ticks.wrapping_add(1);
        if self.slow_ticks % PERIODIC_EVERY == 0 {
            let now = now_ns();
            if now.wrapping_sub(self.last_gc) >= shared.gc_interval() {
                self.last_gc = now;
                self.gc(shared, Tracked(ord));
            }
            crate::purge::on_tick(shared, now, Tracked(ord));
        }
    }

    /// `&*cache` for an owned header (a `(*cache).field` load).
    #[inline(always)]
    fn hdr_ref(&self, cache: *mut CacheHeader) -> (r: &CacheHeader)
        requires
            self.wf(),
            self.owns(cache),
        ensures
            *r == self.hdr(cache),
            r.inv(),
    {
        proof {
            assert(cell_ok(cache, self.perms@.hdrs[cache], self.perms@.slots));
        }
        ptr_ref(cache, Tracked(self.perms.borrow().hdrs.tracked_borrow(cache)))
    }

    /// `&*cache` through the header's permission alone, for code that has
    /// moved the stack permissions out of the heap for a while (so
    /// [`Heap::wf`] does not hold).
    #[inline(always)]
    fn hdr_at(&self, cache: *mut CacheHeader) -> (r: &CacheHeader)
        requires
            self.perms@.hdrs.contains_key(cache),
            self.perms@.hdrs[cache].ptr() == cache,
            self.perms@.hdrs[cache].is_init(),
        ensures
            *r == self.perms@.hdrs[cache].value(),
    {
        ptr_ref(cache, Tracked(self.perms.borrow().hdrs.tracked_borrow(cache)))
    }

    /// `&mut *cache` for an owned header (`(*cache).field` stores). The
    /// caller proves the stores keep the heap well formed
    /// ([`Heap::lemma_set`]).
    #[inline(always)]
    fn hdr_mut(&mut self, cache: *mut CacheHeader) -> (r: &mut CacheHeader)
        requires
            old(self).wf(),
            old(self).owns(cache),
        ensures
            *r == old(self).hdr(cache),
            r.inv(),
            final(self).updates(*old(self), cache, *final(r)),
    {
        proof {
            assert(cell_ok(cache, self.perms@.hdrs[cache], self.perms@.slots));
        }
        ptr_mut_ref(cache, Tracked(self.perms.borrow_mut().hdrs.tracked_borrow_mut(cache)))
    }

    /// `self` is `old` with owned header `cache` holding `v`, its stack
    /// untouched.
    pub closed spec fn updates(&self, old: Heap, cache: *mut CacheHeader, v: CacheHeader) -> bool {
        &&& self.perms@.slots == old.perms@.slots
        &&& self.perms@.hdrs == old.perms@.hdrs.insert(cache, self.perms@.hdrs[cache])
        &&& self.perms@.zero == old.perms@.zero
        &&& self.perms@.toks == old.perms@.toks
        &&& self.perms@.owner == old.perms@.owner
        &&& self.perms@.cls == old.perms@.cls
        &&& self.perms@.hdrs[cache].ptr() == cache
        &&& self.perms@.hdrs[cache].is_init()
        &&& self.perms@.hdrs[cache].value() == v
        &&& self.list == old.list
        &&& self.keys == old.keys
        &&& self.arena == old.arena
        &&& self.bound == old.bound
        &&& self.homed == old.homed
        &&& self.others == old.others
        &&& self.home_hdrs == old.home_hdrs
        &&& self.heads == old.heads
    }

    /// Stores into a header that keep [`CacheHeader::inv`], its stack and
    /// capacity, and pop entries off the top at most, keep the heap well
    /// formed.
    proof fn lemma_set(old: Heap, new: Heap, cache: *mut CacheHeader, v: CacheHeader)
        requires
            old.wf(),
            old.owns(cache),
            new.updates(old, cache, v),
            v.inv(),
            v.slots == old.hdr(cache).slots,
            v.cap == old.hdr(cache).cap,
            v.len <= old.hdr(cache).len,
            v.zeroed <= old.hdr(cache).zeroed,
        ensures
            new.keeps(old),
    {
        assert(cell_ok(cache, old.perms@.hdrs[cache], old.perms@.slots));
        lemma_stack_shrink(old.hdr(cache), v, old.perms@.slots);
        Heap::lemma_home_kept(old, new);
        CachePerms::lemma_update(old.perms@, new.perms@, cache);
        CachePerms::lemma_tracks_hdr(old.perms@, new.perms@, cache);
        CachePerms::lemma_zero_hdr(old.perms@, new.perms@, cache);
        CachePerms::lemma_tokened_hdr(old.perms@, new.perms@, cache);
    }
}

} // verus!

verus! {

/// `head` is the permission to the 128 bytes of the heap at `h` before
/// its home headers (its plain fields): what [`Heap::create_detached`]
/// splits off the heap's block for [`init_fields`], which keeps it in the
/// [`FreshHeap`] returned with the heap; `h` is not null and aligned for a
/// `Heap` (64 bytes).
pub(super) open spec fn heap_head(h: *mut Heap, head: vstd::raw_ptr::PointsToRaw) -> bool {
    &&& head.is_range(h@.addr as int, 128)
    &&& head.provenance() == h@.provenance
    &&& h@.addr != 0
    &&& h@.addr as int % 64 == 0
}

/// The home header for `class` of the heap at `h`: `h + 128 + 64 *
/// class`, where [`Heap::create_detached`] writes it and
/// `Heap::home_ptr_at` (under [`Heap::cache_ptr`]) computes it.
pub(super) open spec fn home_hdr(h: *mut Heap, class: int) -> *mut CacheHeader {
    vstd::raw_ptr::ptr_mut_from_data(vstd::raw_ptr::PtrData::<CacheHeader> {
        addr: (h@.addr + 128 + 64 * class) as usize,
        provenance: h@.provenance,
        metadata: (),
    })
}

impl CachePerms {
    /// The cache permissions of a heap [`Heap::create_detached`] has just
    /// made at `h`: well formed and tracking, and holding nothing (every
    /// header it owns is empty, with no stack, and it holds no stack entry
    /// and no slot token, [`CachePerms::tokened`]); among its headers are
    /// the heap's home headers, the one for class `c` caching class `c`.
    spec fn fresh(&self, h: *mut Heap) -> bool {
        &&& self.wf()
        &&& self.tracks()
        &&& self.zero_ok()
        &&& self.tokened()
        &&& self.slots.is_empty()
        &&& self.toks.is_empty()
        &&& forall|p: *mut CacheHeader| #[trigger] self.hdrs.contains_key(p) ==> self.hdr(p).len == 0 && self.hdr(p).cap == 0
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.hdrs.contains_key(home_hdr(h, c)) && self.cls.contains_key(home_hdr(h, c)) && self.cls[home_hdr(h, c)] == c
    }

    /// Permissions that own nothing.
    proof fn tracked_empty() -> (tracked r: CachePerms)
        ensures
            r.hdrs.is_empty(),
            r.slots.is_empty(),
            r.toks.is_empty(),
            r.zero.is_empty(),
            r.wf(),
            r.tracks(),
            r.zero_ok(),
            r.tokened(),
    {
        let tracked r = CachePerms {
            hdrs: Map::tracked_empty(),
            slots: Map::tracked_empty(),
            cls: Map::empty(),
            owner: Map::empty(),
            toks: Map::tracked_empty(),
            zero: Map::tracked_empty(),
            handed: None,
            released: Seq::empty(),
        };
        r.lemma_zero_intro();
        r
    }
}

// The size `Layout::new::<Heap>()` gives `Meta::alloc_raw`: the 128 bytes
// before `caches` and one line per class.
#[cfg(feature = "quantum16")]
global layout Heap is size == 3456, align == 64;
#[cfg(feature = "quantum8")]
global layout Heap is size == 3712, align == 64;

/// The plain fields of the fresh heap at [`FreshHeap::heap`]: the
/// permission to them ([`FreshHeap::head`], the 128 bytes before
/// `caches`, [`heap_head`]), which [`init_fields`] takes and keeps here,
/// and what the trusted writers stored in them, for `tcaches::heap_new`
/// to read back: the home arena ([`init_fields`]) and whether the heap is
/// counted ([`mark_bound`]). Linear and not `Copy`: the one holder of the
/// heap-head permission, made by [`init_fields`] once per heap and used up
/// by `heap_new`, which gives the permission to the reference. It is valid
/// while its heap is live: `destroy` (trusted) does not take it, so one
/// kept past `destroy` would be stale for a later heap in the same block.
/// `create_heap_for_thread` uses it right after `create`, and
/// `Tcaches::create` drops it.
#[verifier::external_body]
pub(super) tracked struct FreshHeap {
    _no_copy: (),
}

impl FreshHeap {
    /// The heap the fields were stored in.
    pub(super) uninterp spec fn heap(&self) -> *mut Heap;

    /// The permission to the heap's plain fields the token holds.
    pub(super) uninterp spec fn head(&self) -> vstd::raw_ptr::PointsToRaw;

    /// The home arena [`init_fields`] stored.
    pub(super) uninterp spec fn arena(&self) -> int;

    /// [`mark_bound`] set `bound`.
    pub(super) uninterp spec fn bound(&self) -> bool;
}

/// The fields of a fresh heap before its home headers, stored field by
/// field ([`Heap::create_detached`]). Trusted: `&raw mut` projections of
/// the raw heap pointer, which Verus does not support. `head` is the
/// permission to the 128 bytes before `caches` ([`heap_head`]); the
/// stores leave it as it was, and it goes into the [`FreshHeap`] returned,
/// which claims only that and what was stored in `arena` and `bound`.
///
/// # Safety
/// `h` is a fresh metadata block for a `Heap`, exclusively owned.
#[inline(always)]
#[verifier::external_body]
unsafe fn init_fields(
    h: *mut Heap,
    shared: &Shared,
    arena: u16,
    Tracked(head): Tracked<vstd::raw_ptr::PointsToRaw>,
) -> (r: Tracked<FreshHeap>)
    requires
        crate::sys::may_syscall(),
        heap_head(h, head),
    ensures
        r@.heap() == h,
        r@.head() == head,
        r@.arena() == arena as int,
        !r@.bound(),
{
    // SAFETY: guaranteed by the caller.
    unsafe {
        ptr::write(&raw mut (*h).shared, shared);
        ptr::write(&raw mut (*h).tid, tid());
        ptr::write(&raw mut (*h).arena, arena);
        ptr::write(&raw mut (*h).slow_ticks, 0);
        ptr::write(&raw mut (*h).last_gc, now_ns());
        ptr::write(&raw mut (*h).uncached, ThreadStats::default());
        ptr::write(&raw mut (*h).others, ptr::null_mut());
        ptr::write(&raw mut (*h).snapshot, ThreadStats::default());
        ptr::write(&raw mut (*h).tcache_enabled, true);
        ptr::write(&raw mut (*h).bound, false);
        ptr::write(&raw mut (*h).rng, 0);
    }
    Tracked::assume_new()
}

/// [`Arenas::assign`](crate::arena::Arenas::assign) on the process's
/// arenas, with the new heap's ticket for the arena it picked.
#[inline(always)]
fn assign_home(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (u16, Tracked<Homed>))
    requires
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_arenas()),
    ensures
        homed_on(r.1@, r.0 as int),
        *final(ord) == *old(ord),
{
    proof {
        shared.lemma_wf();
    }
    let (arena, t, _) = shared.arenas.assign(Tracked(ord));
    proof {
        shared.arenas.lemma_wf();
        shared.arenas.lemma_homed(t@, arena as int);
    }
    (arena, t)
}

/// `(*h).bound = true` for a heap [`Heap::create`] has just made.
/// A field store through the raw heap pointer, to a field [`init_fields`]
/// wrote, inside the 128 bytes `s` holds the permission to
/// ([`FreshHeap::head`], [`heap_head`]); it leaves the permission as it
/// was and records the store in `s`. RefinedRust proves the body against
/// the `rr::` specification, which owns the heap's fields up to `bound`,
/// `bound` holding a `bool` and the others any bytes, and gives them
/// back with `bound` holding `true` and the others' bytes unchanged
/// (`verus/refinedrust/run.sh`, record under `verus/seams/`); that this
/// is what `s` records rests on [`FreshHeap`]'s trusted meaning.
///
/// # Safety
/// `h` is a fresh heap, exclusively owned.
#[inline(always)]
#[cfg_attr(refinedrust, rr::params("l" : "loc", "sh", "tid", "ar", "st", "lg", "un", "ot", "sn", "te" : "val", "b" : "bool"))]
#[cfg_attr(refinedrust, rr::args("l", "()"))]
#[cfg_attr(refinedrust, rr::requires(#type "l" : "HeapHead_fresh_rfn sh tid ar st lg un ot sn te b" @ "HeapHead_fresh heap_ThreadStats_st"))]
#[cfg_attr(refinedrust, rr::ensures(#type "l" : "HeapHead_fresh_rfn sh tid ar st lg un ot sn te true" @ "HeapHead_fresh heap_ThreadStats_st"))]
#[verifier::external_body]
unsafe fn mark_bound(h: *mut Heap, Tracked(s): Tracked<&mut FreshHeap>)
    requires
        old(s).heap() == h,
        heap_head(h, old(s).head()),
    ensures
        final(s).heap() == h,
        final(s).head() == old(s).head(),
        final(s).arena() == old(s).arena(),
        final(s).bound(),
{
    // SAFETY: guaranteed by the caller.
    unsafe { (*h).bound = true };
}

/// Heap creation and teardown. Creation is verified up to the field
/// stores before the home headers ([`init_fields`]): it writes each home
/// header `EMPTY` through the permission `Meta::alloc_raw` hands out for
/// its line, and returns those permissions, which [`CachePerms::fresh`]
/// describes (a heap that holds nothing). `thread::create_heap_for_thread`
/// turns a new thread heap into a reference, installs them as its ghost
/// state and proves [`Heap::wf`] of it ([`Heap::lemma_fresh_wf`]);
/// `Tcaches::create` keeps only the pointer and drops them. The trusted
/// conversions that hand an
/// existing heap to verified code (`tcaches::heap_at`,
/// `thread::heap_from_slot`) assume `wf` of every heap they return, which
/// creation establishes and every writer keeps (see [`Heap::wf`]).
/// `destroy` stays trusted: verifying it changes LTO's inlining of
/// `Meta::free` and `meta::Inner::alloc` elsewhere in the library.
impl Heap {
    /// Create a heap in the metadata region for the calling thread, homed
    /// on the least-loaded automatic arena and counted there
    /// (`Arenas::assign`; `destroy` releases the binding). Null if the
    /// region is exhausted. Does not touch TLS or the thread key, and makes
    /// no call that can allocate. With the heap, the permissions to its
    /// caches ([`CachePerms::fresh`]). The call is counted on the thread's
    /// slot token (`TlsSlot::creates`), which it otherwise leaves alone.
    /// And the heap's ticket for its home arena ([`Homed`]), which the
    /// caller installs with the permissions, with the permission to the
    /// heap's plain fields and what was stored in its `arena` and `bound`
    /// ([`FreshHeap`], for `tcaches::heap_new`); on null, the ticket is
    /// given back (`unbind`).
    fn create(
        shared: &Shared,
        Tracked(tls): Tracked<&mut crate::sys::tls::TlsSlot>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: (*mut Heap, Tracked<CachePerms>, Tracked<Option<(Homed, FreshHeap)>>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            old(ord).allows(rank_arenas()),
        ensures
            r.0@.addr != 0 ==> r.1@.fresh(r.0),
            r.0@.addr != 0 ==> (r.2@ matches Some((t, s)) && homed_on(t, t.arena) && s.heap() == r.0 && s.arena() == t.arena
                && s.bound() && heap_head(r.0, s.head())),
            final(tls).value() == old(tls).value(),
            final(tls).creates() == old(tls).creates() + 1,
            *final(ord) == *old(ord),
    {
        proof {
            tls.note_create();
            shared.lemma_wf();
        }
        let (arena, Tracked(t)) = assign_home(shared, Tracked(ord));
        let (h, perms, Tracked(stored)) = Self::create_detached(shared, arena, Tracked(ord));
        let tracked mut fresh = None;
        if h.is_null() {
            proof {
                shared.arenas.lemma_homed(t, arena as int);
            }
            shared.arenas.unbind(arena, Tracked(t));
        } else {
            let tracked mut s = stored.tracked_unwrap();
            // SAFETY: fresh, exclusively owned.
            unsafe { mark_bound(h, Tracked(&mut s)) };
            proof {
                fresh = Some((t, s));
            }
        }
        (h, perms, Tracked(fresh))
    }

    /// Create a heap homed on `arena` (an explicit cache, or a thread heap
    /// before it is registered). Null if the region is exhausted. With the
    /// heap, the permissions to its caches ([`CachePerms::fresh`]) and the
    /// token holding the permission to its plain fields, with what was
    /// stored in them ([`FreshHeap`]).
    fn create_detached(
        shared: &Shared,
        arena: u16,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: (*mut Heap, Tracked<CachePerms>, Tracked<Option<FreshHeap>>))
        requires
            crate::sys::may_syscall(),
            shared.wf(),
            old(ord).allows(rank_meta()),
        ensures
            r.0@.addr != 0 ==> r.1@.fresh(r.0),
            r.0@.addr != 0 ==> (r.2@ matches Some(st) && st.heap() == r.0 && st.arena() == arena as int && !st.bound()
                && heap_head(r.0, st.head())),
            *final(ord) == *old(ord),
    {
        proof {
            shared.lemma_meta();
            shared.lemma_wf();
            cache::lemma_header_layout();
            assert(crate::meta::MAX_BINNED == 0x10_0000usize) by (compute_only);
        }
        let (block, Tracked(raw)) = shared.meta.alloc_raw(Layout::new::<Heap>(), Tracked(ord));
        let h = block as *mut Heap;
        if h.is_null() {
            let tracked none = CachePerms::tracked_empty();
            return (h, Tracked(none), Tracked(None));
        }
        let ghost size = vstd::layout::size_of::<Heap>() as int;
        let ghost base = h@.addr as int;
        proof {
            assert(size == 128 + 64 * NCLASSES);
            crate::vspec::ptr::axiom_raw_valid(&raw, block, size);
            crate::vspec::ptr::axiom_ptr_add_addr(block, 128);
        }
        let tracked (head, mut rest) = raw.split(Set::range(base, base + 128));
        // SAFETY: `h` is a fresh, exclusively owned metadata block of the
        // right size and alignment. Field by field: a whole `Heap`
        // temporary (3.4 KiB with the headers inline) would deepen the
        // stack of every thread's first allocation by one page, which
        // larson's thread churn turns into +1.7 MiB of RSS per 437
        // threads (thread stacks stay mapped; measured on arm64).
        let Tracked(stored) = unsafe { init_fields(h, shared, arena, Tracked(head)) };
        // SAFETY: as above; `caches` is 128 bytes in (the `const`
        // assertion after `Heap`).
        let caches = unsafe { block.add(128) } as *mut CacheHeader;
        let tracked mut perms = CachePerms::tracked_empty();
        proof {
            assert(Set::range(base + 128, base + size) <= rest.dom());
            crate::vspec::ptr::axiom_raw_valid(&rest, caches as *mut u8, 64 * NCLASSES);
            assert(caches@.addr == base + 128 && caches@.provenance == h@.provenance);
            assert(base % 64 == 0) by {
                assert(block@.addr as int % 64 == 0);
            }
            assert(process_regions().in_meta(base, size));
        }
        for c in 0..NCLASSES
            invariant
                rest.provenance() == h@.provenance,
                rest.contains_range(base + 128 + 64 * c, 64 * (NCLASSES - c)),
                base == h@.addr,
                base == block@.addr,
                base % 64 == 0,
                caches@.addr == base + 128,
                caches@.provenance == h@.provenance,
                caches@.metadata == (),
                size == 128 + 64 * NCLASSES,
                process_regions().in_meta(base, size),
                process_regions().meta_base + process_regions().meta_len <= usize::MAX,
                crate::vspec::ptr::in_alloc(caches, 64 * NCLASSES),
                perms.wf(),
                perms.tracks(),
                perms.zero_ok(),
                perms.tokened(),
                perms.slots.is_empty(),
                perms.toks.is_empty(),
                forall|p: *mut CacheHeader| #[trigger] perms.hdrs.contains_key(p) ==> perms.hdr(p).len == 0 && perms.hdr(p).cap == 0,
                forall|p: *mut CacheHeader| #[trigger] perms.hdrs.contains_key(p) ==> p@.addr < base + 128 + 64 * c,
                forall|k: int| 0 <= k < c ==> perms.hdrs.contains_key(#[trigger] home_hdr(h, k)) && perms.cls.contains_key(home_hdr(h, k)) && perms.cls[home_hdr(h, k)] == k,
        {
            let ghost a = base + 128 + 64 * c;
            proof {
                cache::lemma_header_layout();
                assert(a % 64 == 0) by (nonlinear_arith)
                    requires
                        a == base + 128 + 64 * c,
                        base % 64 == 0;
                assert(c as int * 64 <= 64 * NCLASSES) by (nonlinear_arith)
                    requires
                        c < NCLASSES;
            }
            let tracked (hraw, rest2) = rest.split(Set::range(a, a + 64));
            proof {
                rest = rest2;
                assert(Set::range(a + 64, a + 64 + 64 * (NCLASSES - c - 1)) <= rest.dom());
            }
            let tracked mut hp = hraw.into_typed::<CacheHeader>(a as usize);
            proof {
                crate::vspec::ptr::axiom_ptr_add_addr(caches, c);
            }
            // SAFETY: `c < NCLASSES`, inside the block.
            let cache = unsafe { caches.add(c) };
            proof {
                assert(cache@.addr == a);
                assert(hp.ptr() == cache);
                assert(cache == home_hdr(h, c as int));
            }
            ptr_mut_write(cache, Tracked(&mut hp), CacheHeader::EMPTY);
            proof {
                let ghost old_perms = perms;
                assert(!old_perms.hdrs.contains_key(cache));
                perms.hdrs.tracked_insert(cache, hp);
                perms.cls = old_perms.cls.insert(cache, c as int);
                assert(process_regions().in_meta(a, 64));
                assert(cell_ok(cache, perms.hdrs[cache], perms.slots));
                CachePerms::lemma_insert_unused(old_perms, perms, cache, c as int);
                assert forall|k: int| 0 <= k <= c implies perms.hdrs.contains_key(#[trigger] home_hdr(h, k)) && perms.cls.contains_key(home_hdr(h, k)) && perms.cls[home_hdr(h, k)] == k by {
                    if k < c {
                        assert(home_hdr(h, k) != cache) by {
                            assert(home_hdr(h, k)@.addr != cache@.addr);
                        }
                    }
                }
                assert(forall|k: int| 0 <= k < c + 1 ==> perms.hdrs.contains_key(#[trigger] home_hdr(h, k)) && perms.cls.contains_key(home_hdr(h, k)) && perms.cls[home_hdr(h, k)] == k);
            }
        }
        (h, Tracked(perms), Tracked(Some(stored)))
    }

    /// Flush every cache, free them and the heap itself back to the
    /// metadata allocator. `h` is dangling afterwards. A counted heap
    /// (`bound`) is unbound from its home arena with its ticket
    /// ([`Heap::homed_ok`]; the trusted body passes `Tracked::assume_new()`
    /// for it, and for the exit purge's ghost record).
    ///
    /// # Safety
    /// `h` must come from [`Heap::create`] on `shared` and no longer be in
    /// use by any thread.
    ///
    /// Lock order (trusted with the body): the flush takes class locks,
    /// the frees the metadata lock, and the exit purge class locks and
    /// the huge lock, one at a time.
    #[verifier::external_body]
    pub unsafe fn destroy(h: *mut Heap, shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            // `flush_all`'s, which the body calls.
            shared.wf(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        // SAFETY: the caller gives us exclusive ownership of the heap.
        let heap = unsafe { &mut *h };
        heap.flush_all(shared, Tracked(&mut *ord));
        Self::free_caches(shared, &mut heap.caches, Tracked(&mut *ord));
        for o in Self::nodes(heap.others) {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap; the iterator has read `next` already, so the
            // block can go.
            unsafe {
                Self::free_caches(shared, &mut (*o).caches, Tracked(&mut *ord));
                let block = (o as *mut u8).sub(64 * (*o).pad as usize);
                shared.meta.free(block, node_block_layout(), Tracked(&mut *ord));
            }
        }
        heap.others = ptr::null_mut();
        if heap.bound {
            let arena = heap.arena;
            // The heap's ticket (`homed`, which `Heap::homed_ok` says it
            // holds while `bound`) goes back with the binding.
            shared.arenas.unbind(arena, Tracked::assume_new());
            // jemalloc's rule (`tcache_destroy`): the last thread homed on
            // an arena leaves, so nothing will refill its `Empty` slabs
            // soon; purge them now instead of letting them sit for the
            // decay window. The caches just flushed above are most of
            // them. Not with a background thread, which decays on its own
            // schedule (as jemalloc), and at most once per decay period
            // per arena (`Arena::take_exit_purge`: thread churn would
            // purge and refault the same slabs at every exit). Design 13.3.
            if shared.arenas.bound(arena) == 0 && !crate::purge::background::present() {
                if let Some(a) = shared.arenas.get_existing(arena) {
                    if a.take_exit_purge(now_ns()) {
                        crate::purge::purge_arena(shared, arena, Tracked::assume_new(), Tracked(&mut *ord));
                    }
                }
            }
        }
        shared.meta.free(h as *mut u8, Layout::new::<Heap>(), Tracked(ord));
    }
}

} // verus!

impl Heap {
    /// The `Shared` this heap was created on.
    #[cfg(test)]
    pub fn shared(&self) -> &Shared {
        // SAFETY: a heap never outlives the `Shared` it was created on.
        unsafe { &*self.shared }
    }

    /// The owning thread's kernel thread id at creation.
    #[cfg(test)]
    pub fn tid(&self) -> u32 {
        self.tid
    }

    /// Slow-path entries so far (the periodic tick's counter).
    #[cfg(test)]
    pub fn slow_ticks(&self) -> u32 {
        self.slow_ticks
    }

    /// The home arena's cache for `class`, or `None` while it has never
    /// been used.
    #[cfg(test)]
    pub fn cache(&self, class: usize) -> Option<&CacheHeader> {
        let c = &self.caches[class];
        if c.cap == 0 { None } else { Some(c) }
    }

    /// The cache for `class` in `arena`, home or not, or `None` while it
    /// has never been used (or the arena was never touched).
    #[cfg(test)]
    pub fn cache_in(&self, arena: u16, class: usize) -> Option<&CacheHeader> {
        if arena == self.arena {
            return self.cache(class);
        }
        // SAFETY: `others` is a list of live metadata blocks owned by this
        // heap.
        let node = Self::nodes(self.others).find(|&o| unsafe { (*o).arena } == arena)?;
        // SAFETY: as above.
        let c = unsafe { &(*node).caches[class] };
        if c.cap == 0 { None } else { Some(c) }
    }
}

/// Kani checks of the two `assert_eq!` aborts (records under
/// `verus/seams/`).
#[cfg(kani)]
mod kani_aborts {
    use super::{assert_flushed, assert_refill_empty};
    use crate::kani_proofs::must_not_return;

    /// S122 `assert_flushed`, `ensures *len == 0`: on any other `len` the
    /// call never returns.
    #[kani::proof]
    #[kani::should_panic]
    fn assert_flushed_returns_only_on_zero() {
        let len: u32 = kani::any();
        assert_flushed(&len);
        if len != 0 {
            must_not_return();
        }
    }

    /// S123 `assert_refill_empty`, `ensures *len == 0`: as
    /// `assert_flushed`.
    #[kani::proof]
    #[kani::should_panic]
    fn assert_refill_empty_returns_only_on_zero() {
        let len: u32 = kani::any();
        assert_refill_empty(&len);
        if len != 0 {
            must_not_return();
        }
    }

    /// Both return on 0 (no spurious abort; not part of the contracts).
    #[kani::proof]
    fn asserts_return_on_zero() {
        assert_flushed(&0);
        assert_refill_empty(&0);
    }
}
