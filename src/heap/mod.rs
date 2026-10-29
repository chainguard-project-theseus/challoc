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
mod fresh;
pub(crate) mod live;
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
pub use others::ArenaCaches;
#[cfg(test)]
pub use paths::free_fast;
pub use paths::{
    alloc, alloc_declined, alloc_zeroed, free, free_direct, free_fast_slot, free_sized_fast,
};
#[cfg(verus_keep_ghost)]
pub use shared::Built;
pub use shared::Shared;
#[cfg(verus_keep_ghost)]
pub use shared::shared_storage;
#[cfg(test)]
use shared::{FAIL_SETSPECIFIC, guard_interval_for};
pub use thread::{TLS_CREATING, TLS_EXITED, fast_flags};
pub use thread::{existing_heap, heap_destructor, heap_for_thread, tls_heap_slot};

use core::alloc::Layout;
use core::ptr;

#[cfg(verus_keep_ghost)]
use crate::class::aset;
#[cfg(verus_keep_ghost)]
use crate::class::slot_loc;
use crate::layout::{CLASSES, NCLASSES};
#[cfg(verus_keep_ghost)]
use crate::layout::{class_table, process_regions};
#[cfg(verus_keep_ghost)]
use vstd::resource::set::GhostSubset;

use crate::arena::Homed;
use crate::lock::LockOrder;
use crate::vspec::zero::Zeroed;
use fresh::FreshHeap;
#[cfg(verus_keep_ghost)]
pub(crate) use live::Live;
use others::{node_block_layout, node_caches_detached, node_set_arena};
use paths::invalid_free;
#[cfg(test)]
use shared::ncpus;
use shared::{now_ns, tid};
use vstd::map::Map;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, verus};
use vstd::raw_ptr::PointsTo;
use vstd::resource::set::GhostSingleton;

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

} // verus!

/// Slow-path entries between two periodic-work checks (design 6.4).
pub const PERIODIC_EVERY: u32 = 64;
/// Slow-path entries ([`Heap::slow_ticks`]) before which a new thread heap
/// may still move its home onto the arena of a slot it frees
/// (`Heap::try_rehome`, design 6.5). A thread that takes over a dead
/// thread's data (larson's successors) frees it from its first operation
/// on, and moved within 2..3 slow-path entries on average (C4A); a thread
/// that frees other threads' data only after building its own (mstress:
/// about 140 entries at the first such free) never qualifies, so it keeps
/// its own data at home. Below the limit a heap has refilled at most a
/// few caches, so the move's flush is small and abandons nothing.
pub const REHOME_TICKS: u32 = 8;

/// Default cache capacity for a class: [`MAX_CAP`] (1024) slots for classes
/// up to 128 bytes, halved for every doubling of the class size, never
/// below [`MIN_CAP`].
#[cfg(test)]
pub const fn default_cap(class: usize) -> u32 {
    crate::config::ladder(class, MAX_CAP, 1)
}

/// The `others` key of the one flush-only cache per class that plain
/// frees of another arena's slots go onto ([`ArenaCaches`]); never an
/// arena index (`arena::MAX_ARENAS` is 4096).
pub const FOREIGN: u16 = u16::MAX;

verus! {

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

/// `Shared::free_flags` bit, never in `push_flags`: more than one arena
/// exists, so plain `free` compares the slot's owner word with the heap's
/// home arena before pushing.
pub const FREE_ROUTE: u8 = 8;

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
/// without the bit test, and the flush catches a stale one. A page start
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
        crate::vspec::class::axiom_class_table_wf(c);
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

/// Whether cache entries are stored signed: the process's `Shared` has
/// [`PUSH_PAC`] set. Uninterpreted, and no axiom ties it to a `Shared`'s
/// `push_flags`. One value, since a process publishes one `Shared` and
/// `push_flags` is written once, before the `Shared` is published. The
/// unit tests build several with different flags, so the spec stands for
/// the shipped library only.
pub uninterp spec fn pac_mode() -> bool;

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

/// One flush's split: the addresses a cache
/// held (`a`) are those it keeps (`k`) and those it released (`r`), and
/// none is both. Opaque, so a proof that uses it carries one
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

/// The stack of header `h` is owned entry by entry: `cap` entries from
/// `slots`, the first `len` of them written, each entry inside the
/// stack's allocation ([`slot_in`]) and, when `cap` is not 0, its end
/// too (`slots.add(cap)` is in bounds).
spec fn stack_ok(h: CacheHeader, slots: Map<*mut *mut u8, PointsTo<*mut u8>>) -> bool {
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
spec fn stack_claimed(h: CacheHeader, blocks: Map<*mut *mut u8, crate::meta::MetaClaim>) -> bool {
    &&& blocks.contains_key(h.slots)
    &&& blocks[h.slots].held(crate::meta::process_live_id(), h.slots as *mut u8, 8 * h.cap, 16)
}

/// `stacks` holds, for class `c`, the permission to the bytes of `h`'s
/// stack and its claim from the `Meta` whose live-object records are `id`
/// ([`Heap::free_caches`]).
spec fn stack_given(stacks: Map<int, (vstd::raw_ptr::PointsToRaw, crate::meta::MetaClaim)>, c: int, h: CacheHeader, id: vstd::resource::Loc) -> bool {
    &&& stacks.contains_key(c)
    &&& stacks[c].0.is_range(h.slots@.addr as int, 8 * h.cap)
    &&& stacks[c].0.provenance() == h.slots@.provenance
    &&& stacks[c].1.held(id, h.slots as *mut u8, 8 * h.cap, 16)
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
/// metadata region.
spec fn cell_in_meta(p: *mut CacheHeader, h: CacheHeader) -> bool {
    &&& process_regions().in_meta(p@.addr as int, 64)
    &&& h.cap != 0 ==> process_regions().in_meta(h.slots@.addr as int, 8 * h.cap)
}

/// `new` keeps every stack entry `old` owns, at the same address, and
/// every written one written, and files each entry it adds under the
/// entry's own address.
spec fn slots_grow(old: Map<*mut *mut u8, PointsTo<*mut u8>>, new: Map<*mut *mut u8, PointsTo<*mut u8>>) -> bool {
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
proof fn lemma_stack_shrink(old_h: CacheHeader, new_h: CacheHeader, slots: Map<*mut *mut u8, PointsTo<*mut u8>>)
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
proof fn lemma_stack_grow(h: CacheHeader, old_s: Map<*mut *mut u8, PointsTo<*mut u8>>, new_s: Map<*mut *mut u8, PointsTo<*mut u8>>)
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
spec fn in_entries(slots: *mut *mut u8, q: *mut *mut u8, k: int, n: int) -> bool {
    exists|i: int| k <= i < n && q == slot_ptr(slots, i)
}

/// Every stack entry but entries `0..n` of the stack at `slots` keeps its
/// token.
spec fn toks_kept(new: Map<*mut *mut u8, GhostSingleton<int>>, old: Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, n: int) -> bool {
    forall|q: *mut *mut u8| #![trigger new.contains_key(q)] #![trigger old.contains_key(q)] !in_entries(slots, q, 0, n) ==> {
        &&& new.contains_key(q) == old.contains_key(q)
        &&& old.contains_key(q) ==> new[q] == old[q]
    }
}

/// The entries of header `h`'s stack: `cap` words from `slots`.
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

/// `gone` holds the permission to entry `i` of the stack at `base`, which
/// lies inside the stack's allocation ([`join_entries`]).
spec fn entry_given(gone: Map<*mut *mut u8, PointsTo<*mut u8>>, base: *mut *mut u8, i: int) -> bool {
    let q = slot_ptr(base, i);
    &&& gone.contains_key(q)
    &&& gone[q].ptr() == q
    &&& slot_in(base, i)
}

/// The permission to the first `n` entries of the stack at `base` (`8 *
/// n` bytes), joined from the entries' permissions in `gone` with their
/// contents dropped.
proof fn join_entries(tracked gone: Map<*mut *mut u8, PointsTo<*mut u8>>, base: *mut *mut u8, n: int) -> (tracked r: vstd::raw_ptr::PointsToRaw)
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

impl CachePerms {
    /// Every owned header is [`cell_ok`], and every stack-entry permission
    /// is filed under its own address.
    spec fn wf(&self) -> bool {
        &&& forall|p: *mut CacheHeader| #[trigger] self.hdrs.contains_key(p) ==> cell_ok(p, self.hdrs[p], self.slots)
        &&& forall|q: *mut *mut u8| #[trigger] self.slots.contains_key(q) ==> self.slots[q].ptr() == q
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

    /// Every owned header with a stack holds the stack's claim
    /// ([`stack_claimed`]).
    #[verifier::opaque]
    spec fn claimed(&self) -> bool {
        forall|p: *mut CacheHeader| #[trigger] self.hdrs.contains_key(p) && self.hdr(p).cap != 0 ==> stack_claimed(self.hdr(p), self.blocks)
    }

    /// Header `p` of `new` is one `old` owned, with the same stack
    /// ([`CachePerms::lemma_claimed_frame`]).
    spec fn same_stack(old: CachePerms, new: CachePerms, p: *mut CacheHeader) -> bool {
        &&& old.hdrs.contains_key(p)
        &&& old.hdr(p).cap == new.hdr(p).cap
        &&& old.hdr(p).slots == new.hdr(p).slots
    }

    /// A change that keeps the claims, and leaves every owned header with
    /// a stack an owned header of `old` with the same stack, keeps
    /// [`CachePerms::claimed`].
    proof fn lemma_claimed_frame(old: CachePerms, new: CachePerms)
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
    proof fn lemma_claimed_hdr(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
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
    proof fn lemma_claimed_new(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
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
    proof fn tracked_put_claim(tracked &mut self, base: *mut *mut u8, cap: int, tracked mc: crate::meta::MetaClaim)
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

    /// Take out the claim of owned header `p`'s stack, to free the stack
    /// ([`Heap::free_caches_in`]). Every other owned header with a stack
    /// keeps its claim: its stack is at another base, since the stacks'
    /// first entries have one owner each ([`CachePerms::tracks`]).
    proof fn tracked_take_claim(tracked &mut self, p: *mut CacheHeader) -> (tracked r: crate::meta::MetaClaim)
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

    /// The slot tokens of what the caches hold: each of a header's first `len` entries
    /// holds the token of its slot ([`CachePerms::tok_at`]).
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

    /// Uniqueness for one
    /// heap's caches: two different cached entries of one class never
    /// stand for the same slot. Each holds its slot's token
    /// ([`CachePerms::tokened`]), and two
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
    proof fn lemma_keeps_refl(a: CachePerms)
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
    proof fn lemma_remove_hdrs(old: CachePerms, new: CachePerms)
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
    fn add(&mut self, size: u64, allocs: u64, frees: u64) {
        self.allocated = self.allocated.wrapping_add(size.wrapping_mul(allocs));
        self.deallocated = self.deallocated.wrapping_add(size.wrapping_mul(frees));
    }
}

/// Ghost: the permissions for the cache memory of one heap (its home
/// headers, the headers of its `others` nodes and their stacks), keyed by
/// address. `cls` and `owner` are ghost bookkeeping for what the caches
/// hold: the class each header caches, and for each stack entry the
/// header and index it is. `toks` holds the slot tokens of the cached
/// slots, by stack entry; `zero` the zero tokens of the headers' zero
/// prefixes; `handed` the token of the slot the last pop from inside a
/// zero prefix handed out; `released` the `(class, address)` pairs this
/// heap's flushes handed back; `blocks` the metadata claim of each stack,
/// by its base.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct CachePerms {
    pub tracked hdrs: Map<*mut CacheHeader, PointsTo<CacheHeader>>,
    pub tracked slots: Map<*mut *mut u8, PointsTo<*mut u8>>,
    pub ghost cls: Map<*mut CacheHeader, vstd::prelude::int>,
    pub ghost owner: Map<*mut *mut u8, (*mut CacheHeader, vstd::prelude::int)>,
    pub tracked toks: Map<*mut *mut u8, GhostSingleton<vstd::prelude::int>>,
    pub tracked zero: Map<*mut *mut u8, Zeroed>,
    pub tracked handed: Option<Zeroed>,
    pub ghost released: vstd::seq::Seq<(vstd::prelude::int, vstd::prelude::int)>,
    pub tracked blocks: Map<*mut *mut u8, crate::meta::MetaClaim>,
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
    /// This heap may still move its home onto the arena of a slot it
    /// frees ([`Heap::try_rehome`]): set for a new thread heap; cleared
    /// for an explicit cache, by a `thread.arena` write, by the one move,
    /// and once [`REHOME_TICKS`] slow-path entries have passed.
    rehome: bool,
    /// The arena of the last cross-arena free [`Heap::try_rehome`] looked
    /// at: the move needs two in a row into the same arena.
    rehome_from: u16,
    /// Caches of the home arena, one line per class; last, so the small
    /// fields above share two lines and `caches[c]` is `heap + 128 + 64 * c`.
    caches: [CacheHeader; NCLASSES],
    /// Ghost ([`CachePerms`], no bytes): the permissions for `caches`, the
    /// `others` nodes' headers and their stacks.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    perms: Tracked<CachePerms>,
    /// Ghost (no bytes): the `others` list, head first, as the nodes'
    /// `next` fields chain it.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    list: Ghost<vstd::seq::Seq<*mut ArenaCaches>>,
    /// Ghost (no bytes): the `others` nodes' keys, `keys[i]` the `arena`
    /// field of `list[i]`.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    keys: Ghost<vstd::seq::Seq<u16>>,
    /// Ghost (no bytes): the home headers, one per class, where creation
    /// wrote them.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    home_hdrs: Ghost<vstd::seq::Seq<*mut CacheHeader>>,
    /// Ghost (no bytes): the heap's ticket for its home arena while it is
    /// counted there.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    homed: Tracked<Option<Homed>>,
    /// Ghost (no bytes): what the heap keeps of each `others` node's block
    /// ([`others::NodeHead`]): the permissions to the `arena`, `pad` and
    /// `next` fields `other_caches` writes when it links the node in, the
    /// rest of the block, and its claim.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    heads: Tracked<Map<*mut ArenaCaches, others::NodeHead>>,
}

} // verus!

const _: () = assert!(core::mem::offset_of!(Heap, caches) == 128);

/// `slots[start..start + len]` as a slice, for the slice operations of
/// the refill, the flush and the run rebuild (claims, selections, sorts,
/// reversals).
///
/// # Safety
/// The entries are this heap's stack storage and nothing else reads or
/// writes them while the slice lives.
#[inline(always)]
#[allow(
    clippy::needless_lifetimes,
    reason = "the permission and the slice share the lifetime in the Verus signature"
)]
unsafe fn stack_mut<'a>(
    slots: *mut *mut u8,
    start: usize,
    len: usize,
    _perms: Tracked<&'a mut Map<*mut *mut u8, PointsTo<*mut u8>>>,
) -> &'a mut [*mut u8] {
    // SAFETY: guaranteed by the caller.
    unsafe { core::slice::from_raw_parts_mut(slots.add(start), len) }
}

/// `assert_eq!(*len, 0, ..)`, the abort of [`Heap::free_caches`] and
/// [`Heap::free_caches_in`] on a cache that still holds slots: `*len == 0`
/// after it returns.
#[inline(always)]
#[track_caller]
fn assert_flushed(len: &u32) {
    assert_eq!(*len, 0, "heap: freeing a cache that was not flushed");
}

/// `assert_eq!(*len, 0, ..)`, [`Heap::refill`]'s abort on a heap
/// inconsistency: `*len == 0` after it returns.
#[inline(always)]
#[track_caller]
fn assert_refill_empty(len: &u32) {
    assert_eq!(*len, 0, "heap: refill of a cache that is not empty");
}

/// `assert_eq!(*len, 0, ..)`, [`CacheHeader::rekey`]'s abort on a cache
/// that still holds slots: `*len == 0` after it returns.
#[inline(always)]
#[track_caller]
fn assert_rekey_flushed(len: &u32) {
    assert_eq!(*len, 0, "heap: re-keying a cache that was not flushed");
}

/// `(*h).bound = true` for a heap [`Heap::create`] has just made: a field
/// store through the raw heap pointer.
///
/// # Safety
/// `h` is a fresh heap, exclusively owned.
#[inline(always)]
unsafe fn mark_bound(h: *mut Heap, _s: Tracked<&mut FreshHeap>) {
    // SAFETY: guaranteed by the caller.
    unsafe { (*h).bound = true };
}

/// `(*h).rehome = true` for a thread heap [`Heap::create`] has just made
/// and marked bound ([`mark_bound`]): it may still move its home
/// ([`Heap::try_rehome`]).
///
/// # Safety
/// `h` is a fresh heap, exclusively owned.
#[inline(always)]
unsafe fn mark_rehome(h: *mut Heap, _s: Tracked<&mut FreshHeap>) {
    // SAFETY: guaranteed by the caller.
    unsafe { (*h).rehome = true };
}

/// [`Arenas::assign`](crate::arena::Arenas::assign) on the process's
/// arenas, with the new heap's ticket for the arena it picked.
#[inline(always)]
fn assign_home(shared: &Shared, _ord: Tracked<&mut LockOrder>) -> (u16, Tracked<Homed>) {
    let (arena, t, _) = shared.arenas.assign(Tracked::assume_new());
    (arena, t)
}

impl Heap {
    /// Create a heap in the metadata region for the calling thread, homed
    /// on the least-loaded automatic arena and counted there
    /// (`Arenas::assign`; `destroy` releases the binding). Null if the
    /// region is exhausted. Does not touch TLS or the thread key.
    pub fn create(shared: &Shared) -> *mut Heap {
        let (arena, ticket) = assign_home(shared, Tracked::assume_new());
        let (h, _, _) = Self::create_detached(shared, arena, Tracked::assume_new());
        if h.is_null() {
            shared.arenas.unbind(arena, ticket);
        } else {
            // SAFETY: fresh, exclusively owned.
            unsafe { mark_bound(h, Tracked::assume_new()) };
            // SAFETY: as above.
            unsafe { mark_rehome(h, Tracked::assume_new()) };
        }
        h
    }

    /// Create a heap homed on `arena` (an explicit cache, or a thread heap
    /// before it is registered). Null if the region is exhausted. With
    /// the heap, the permissions to its caches and the token for its plain
    /// fields (ghost).
    fn create_detached(
        shared: &Shared,
        arena: u16,
        _ord: Tracked<&mut LockOrder>,
    ) -> (*mut Heap, Tracked<CachePerms>, Tracked<Option<FreshHeap>>) {
        let block = shared.meta.alloc(Layout::new::<Heap>());
        let h = block as *mut Heap;
        if h.is_null() {
            return (h, Tracked::assume_new(), Tracked::assume_new());
        }
        // SAFETY: `h` is a fresh, exclusively owned metadata block of the
        // right size and alignment. Field by field: a whole `Heap`
        // temporary (3.4 KiB with the headers inline) would deepen the
        // stack of every thread's first allocation by one page, which
        // larson's thread churn turns into +1.7 MiB of RSS per 437
        // threads (thread stacks stay mapped; measured on arm64).
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
            ptr::write(&raw mut (*h).rehome, false);
            ptr::write(&raw mut (*h).rehome_from, FOREIGN);
        }
        // SAFETY: as above; `caches` is 128 bytes in (the `const`
        // assertion after `Heap`).
        let caches = unsafe { block.add(128) } as *mut CacheHeader;
        for c in 0..NCLASSES {
            // SAFETY: `c < NCLASSES`, inside the block.
            let cache = unsafe { caches.add(c) };
            // SAFETY: as above.
            unsafe { ptr::write(cache, CacheHeader::EMPTY) };
        }
        (h, Tracked::assume_new(), Tracked::assume_new())
    }

    /// Flush every cache, free them and the heap itself back to the
    /// metadata allocator. `h` is dangling afterwards.
    ///
    /// # Safety
    /// `h` must come from [`Heap::create`] on `shared` and no longer be in
    /// use by any thread.
    pub unsafe fn destroy(h: *mut Heap, shared: &Shared, _ord: Tracked<&mut LockOrder>) {
        // SAFETY: the caller gives us exclusive ownership of the heap.
        let heap = unsafe { tcaches::heap_at(h) };
        // Unbound before the flush: the arena is idle from here on, so a
        // thread this one just started (larson's successor, which frees
        // what this one allocated) can be homed on it, or move there
        // ([`Heap::try_rehome`]), while the flush below runs. The heap is
        // not counted from then on: `bound` and `rehome` are cleared
        // (nothing reads them again but this, which keeps the `bound` it
        // read for the exit purge).
        let bound = heap.bound;
        if bound {
            shared
                .arenas
                .unbind(heap.arena, vstd::prelude::Tracked::assume_new());
            heap.bound = false;
            heap.rehome = false;
        }
        heap.flush_all(shared, Tracked::assume_new());
        // SAFETY: the home array (null).
        unsafe { heap.free_caches_in(shared, ptr::null_mut(), Tracked::assume_new()) };
        // `for o in Self::nodes(heap.others)`, spelled out over
        // `node_next`: as the iterator does, the node's `next` is read
        // before the node is freed. Each node leaves the list as it goes
        // (`others` is its successor).
        let mut o = heap.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap; the node is freed below, after its `next`.
            let next = unsafe { heap.node_next(o, Ghost::assume_new()) };
            // SAFETY: `o` is a live node of this heap's `others` list.
            unsafe { heap.free_caches_in(shared, o, Tracked::assume_new()) };
            // SAFETY: as above.
            let pad = unsafe { heap.node_pad(o, Ghost::assume_new()) };
            // SAFETY: the node lies `pad` lines into its block.
            let block = unsafe { (o as *mut u8).sub(64 * pad as usize) };
            shared.meta.free(
                block,
                node_block_layout(),
                vstd::prelude::Tracked::assume_new(),
            );
            heap.others = next;
            o = next;
        }
        heap.others = ptr::null_mut();
        if bound {
            let arena = heap.arena;
            // jemalloc's rule (`tcache_destroy`): the last thread homed on
            // an arena leaves, so nothing will refill its `Empty` slabs
            // soon; purge them now instead of letting them sit for the
            // decay window. The caches just flushed above are most of
            // them. Not with a background thread, which decays on its own
            // schedule (as jemalloc), and at most once per decay period
            // per arena (`Arena::take_exit_purge`: thread churn would
            // purge and refault the same slabs at every exit); and not
            // when another heap has been homed there since the unbind
            // above. Design 13.3.
            if shared.arenas.bound(arena) == 0
                && !crate::purge::background::present()
                && let Some(a) = shared.arenas.get_existing(arena)
                && a.take_exit_purge(now_ns())
            {
                crate::purge::purge_arena(
                    shared,
                    arena,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                );
            }
        }
        shared.meta.free(
            h as *mut u8,
            Layout::new::<Heap>(),
            vstd::prelude::Tracked::assume_new(),
        );
    }

    /// Reset every (flushed) header of one array to [`CacheHeader::EMPTY`]
    /// and free its stack, through a plain reference to the array (the
    /// tests' teardown of a bare cache array). The heap's own arrays go
    /// through [`Heap::free_caches_in`].
    #[cfg(test)]
    fn free_caches(
        shared: &Shared,
        caches: &mut [CacheHeader; NCLASSES],
        _stacks: Tracked<
            Map<vstd::prelude::int, (vstd::raw_ptr::PointsToRaw, crate::meta::MetaClaim)>,
        >,
    ) {
        for cache in caches.iter_mut() {
            if cache.cap != 0 {
                assert_flushed(&cache.len);
                shared.meta.free(
                    cache.slots as *mut u8,
                    CacheHeader::slots_layout(cache.cap),
                    vstd::prelude::Tracked::assume_new(),
                );
            }
            *cache = CacheHeader::EMPTY;
        }
    }

    /// [`Heap::free_caches`] header by header, for the home array (`node`
    /// null) or `node`'s: reset every (flushed) header to
    /// [`CacheHeader::EMPTY`] and free its stack.
    ///
    /// Unsafe for its caller's precondition; a later proof PR can make it
    /// a safe `fn` again once a Verus `requires` carries that precondition.
    ///
    /// # Safety
    /// `node` is null or a live node owned by this heap.
    unsafe fn free_caches_in(
        &mut self,
        shared: &Shared,
        node: *mut ArenaCaches,
        _ord: Tracked<&mut LockOrder>,
    ) {
        for class in 0..NCLASSES {
            // SAFETY: `node` is null or a live node owned by this heap.
            let cache = unsafe { self.header_in(node, class) };
            // SAFETY: `header_in` returned one of this heap's headers.
            let h = unsafe { self.hdr_mut(cache) };
            if h.cap != 0 {
                assert_flushed(&h.len);
                shared.meta.free(
                    h.slots as *mut u8,
                    CacheHeader::slots_layout(h.cap),
                    vstd::prelude::Tracked::assume_new(),
                );
            }
            *h = CacheHeader::EMPTY;
        }
    }

    /// Trade the values of owned headers `a` and `b` of one class
    /// (`thread.arena`'s exchange of the home caches with a node's,
    /// [`Heap::set_arena`]); each stack goes with its header.
    ///
    /// Unsafe for its caller's precondition; a later proof PR can make it
    /// a safe `fn` again once a Verus `requires` carries that precondition.
    ///
    /// # Safety
    /// `a` and `b` are distinct headers this heap owns.
    unsafe fn swap_hdrs(&mut self, a: *mut CacheHeader, b: *mut CacheHeader) {
        // SAFETY: guaranteed by the caller.
        unsafe { ptr::swap(a, b) };
    }

    /// `&*cache` for an owned header (a `(*cache).field` load).
    ///
    /// Unsafe for its caller's precondition; a later proof PR can make it
    /// a safe `fn` again once a Verus `requires` carries that precondition.
    ///
    /// # Safety
    /// `cache` is a header this heap owns (a home header or one of a live
    /// `others` node's), and nothing writes it while the reference lives.
    #[inline(always)]
    unsafe fn hdr_ref(&self, cache: *mut CacheHeader) -> &CacheHeader {
        // SAFETY: guaranteed by the caller.
        unsafe { &*cache }
    }

    /// `&mut *cache` for an owned header (`(*cache).field` stores).
    ///
    /// Unsafe for its caller's precondition; a later proof PR can make it
    /// a safe `fn` again once a Verus `requires` carries that precondition.
    ///
    /// # Safety
    /// `cache` is a header this heap owns (a home header or one of a live
    /// `others` node's), and nothing else reads or writes it while the
    /// reference lives.
    #[inline(always)]
    unsafe fn hdr_mut(&mut self, cache: *mut CacheHeader) -> &mut CacheHeader {
        // SAFETY: guaranteed by the caller.
        unsafe { &mut *cache }
    }

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

    /// Home arena (design 6.2 `heap.arena`; `thread.arena`).
    #[inline]
    pub fn arena(&self) -> u16 {
        self.arena
    }

    /// Change the home arena (`thread.arena`). The caches of the old home
    /// move to the `others` list and the new home's, if any, take their
    /// place, so nothing is flushed. `arena` must exist.
    pub fn set_arena(&mut self, shared: &Shared, arena: u16, _ord: Tracked<&mut LockOrder>) {
        // An explicit home is kept, even one equal to the current home:
        // no late move away from it ([`Heap::try_rehome`]).
        self.rehome = false;
        if arena == self.arena {
            return;
        }
        let old = self.arena;
        let node = self.other_caches(shared, arena, true);
        if node.is_null() {
            // No metadata for a new node: the caches of the new home start
            // empty and the old home's must go back first.
            self.drop_home_caches(shared, Tracked::assume_new());
        } else {
            // The node's headers and the home headers trade places, and
            // the node becomes the cache of `old`, the home arena being
            // left.
            for c in 0..NCLASSES {
                let a = self.cache_ptr(c);
                // SAFETY: `node` is a live block owned by this heap.
                let b = unsafe { Self::node_cache_ptr(node, c) };
                // SAFETY: a home header and a header of the node, both
                // this heap's, so distinct.
                unsafe { self.swap_hdrs(a, b) };
            }
            // SAFETY: as above.
            unsafe { node_set_arena(node, old, Tracked::assume_new()) };
        }
        self.arena = arena;
        if self.bound {
            shared
                .arenas
                .unbind(old, vstd::prelude::Tracked::assume_new());
            let _ticket = shared.arenas.bind(arena);
        }
    }

    /// The `FOREIGN` slow path of a young heap's cross-arena `free`
    /// ([`Heap::rehome`] set): the first free of each class, while the
    /// class has no `FOREIGN` cache yet, and the flush of a full one. The
    /// heap moves onto the slot's arena when [`Heap::try_rehome`] allows
    /// it, and the slot then takes the home push; otherwise (false) the
    /// caller goes on to the ordinary `FOREIGN` path. Here rather than in
    /// [`Heap::free_other`] so that the cross-arena fast push (every free
    /// of xmalloc-test's consumers) carries no test for it.
    ///
    /// The move flushes the `FOREIGN` caches, and with them a first copy
    /// of `p` if this free is a double free: before the move the
    /// `double_free_scan` compare runs against the `FOREIGN` cache (the
    /// one `free_slow_in` would have run), and after it the exact bit test
    /// runs again on `p`, so a slot the flush just released aborts as
    /// `invalid or double free` instead of being pushed into a cache that
    /// `malloc` pops (which would hand it out twice).
    #[cold]
    #[inline(never)]
    fn rehome_on_free(&mut self, shared: &Shared, class: usize, p: *mut u8) -> bool {
        let to = shared.arena_of_slot(class, p);
        if shared.double_free_scan() {
            let foreign = self.other_caches(shared, FOREIGN, false);
            if !foreign.is_null() {
                // SAFETY: `foreign` is this heap's live `FOREIGN` node; a
                // header with `cap != 0` is a real cache.
                unsafe {
                    let cache = Self::node_cache_ptr(foreign, class);
                    if self.hdr_ref(cache).cap != 0 {
                        Self::scan_or_abort(shared, cache, p);
                    }
                }
            }
        }
        if !self.try_rehome(shared, to) {
            return false;
        }
        // The flush released every cached slot: test `p`'s bit again.
        match shared.classify_free(p, Tracked::assume_new()) {
            Ok((c, a)) => debug_assert!(c == class && a == to),
            Err(_) => invalid_free(p as usize),
        }
        self.push(shared, class, p);
        true
    }

    /// Move the home of a young thread heap onto `to`, the arena owning a
    /// slot it is freeing, if no heap is homed there (design 6.5). Called
    /// from the cross-arena `free` ([`Heap::rehome_on_free`]) while
    /// [`Heap::rehome`] is set, which this clears once the heap has made
    /// [`REHOME_TICKS`] slow-path entries; true when the heap moved.
    ///
    /// Why: a thread that takes over a dead thread's data was homed when
    /// it started, while that thread still held its arena, so it was put
    /// on another one, and every `free` of the inherited data took the
    /// foreign cache while its own allocations came from fresh refills of
    /// its own arena (larson: each successor alternated between two
    /// arenas, `narenas:1` was 31% faster at 10 rounds per thread on the
    /// C4A). Most successors now create their heap after the exiting
    /// thread's unbind ([`Heap::destroy`] unbinds before its flush) and
    /// are homed on its arena directly; one started earlier moves there
    /// here once that arena is idle, and the inherited slots feed its
    /// home caches again.
    ///
    /// The move flushes the home caches and the `FOREIGN` caches through
    /// the ordinary flush (every cached slot goes back to its slab, its bit
    /// cleared, so a stale free of one aborts as before; nothing stays
    /// cached across the move), keeps the stacks and re-keys the home
    /// headers to `to`. Only an automatic arena, only one with no heap
    /// homed on it (`Arenas::try_bind_idle`, one compare-and-swap, so two
    /// heaps never both take it), never one this heap already keeps an
    /// `others` node for, and at most once per heap.
    ///
    /// Only while the heap is young by its slow-path count: a thread that
    /// has built data of its own at home and then frees other threads'
    /// data (mstress's workers take transfer slots from every arena) would
    /// strand its own data on the arena it leaves and send every later
    /// free of it to the `FOREIGN` cache. Moving on the first free from
    /// any idle arena cost mstress 6% (700 of its 775 workers moved, after
    /// about 140 slow-path entries each); under the limit 46 still moved
    /// (+2.3%). Hence the second condition: two cross-arena frees in a
    /// row seen here went to the same arena (an inherited data set comes
    /// from one arena; mstress's transfer slots from all of them).
    fn try_rehome(&mut self, shared: &Shared, to: u16) -> bool {
        debug_assert!(self.bound);
        if self.slow_ticks >= REHOME_TICKS {
            self.rehome = false;
            return false;
        }
        if to != self.rehome_from {
            self.rehome_from = to;
            return false;
        }
        if to as u32 >= shared.arenas.auto()
            || to == self.arena
            || shared.arenas.bound(to) != 0
            || !self.other_caches(shared, to, false).is_null()
            || !shared.arenas.try_bind_idle(to).0
        {
            return false;
        }
        let old = self.arena;
        self.flush_arena(shared, old, ptr::null_mut(), Tracked::assume_new());
        let foreign = self.other_caches(shared, FOREIGN, false);
        if !foreign.is_null() {
            self.flush_arena(shared, FOREIGN, foreign, Tracked::assume_new());
        }
        let a = shared.owner(to);
        for class in 0..NCLASSES {
            let c = self.cache_ptr(class);
            // SAFETY: `cache_ptr` is this heap's home header for `class`.
            if unsafe { self.hdr_ref(c) }.cap != 0 {
                let refilled = a.0.class(class).refilled() as u32;
                // SAFETY: as above.
                let h = unsafe { self.hdr_mut(c) };
                h.rekey(to, refilled);
            }
        }
        self.arena = to;
        self.rehome = false;
        shared
            .arenas
            .unbind(old, vstd::prelude::Tracked::assume_new());
        true
    }

    /// Flush the home caches, fold their byte counters into `uncached` and
    /// free them: the home array is `EMPTY` afterwards and `stats()` is
    /// unchanged. For a `thread.arena` change that could get no `others`
    /// node for the old home.
    fn drop_home_caches(&mut self, shared: &Shared, _ord: Tracked<&mut LockOrder>) {
        let home = self.arena;
        self.flush_arena(shared, home, ptr::null_mut(), Tracked::assume_new());
        Self::fold_counters(&mut self.uncached, &self.caches);
        // SAFETY: the home array (null).
        unsafe { self.free_caches_in(shared, ptr::null_mut(), Tracked::assume_new()) };
    }

    /// Add the byte counters a cache array stands for (`allocs` pops and
    /// `len + allocs - moved` pushes per cache, each worth the class size)
    /// to `into`. [`Heap::stats`] derives the thread counters this way on
    /// every read; a cache that is about to be freed is folded into
    /// `uncached` first so that `thread.allocated` never goes backwards.
    #[allow(
        clippy::while_let_loop,
        reason = "the desugared `for`, which Verus needs for `continue`"
    )]
    fn fold_counters(into: &mut ThreadStats, caches: &[CacheHeader; NCLASSES]) {
        // `for (class, c) in caches.iter().enumerate()`, spelled out: Verus
        // has no `Enumerate` and rejects `continue` in a `for` loop. The
        // count is taken and bumped as `Enumerate::next` does.
        let mut iter = caches.iter();
        let mut count: usize = 0;
        loop {
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
    pub fn stats(&self) -> ThreadStats {
        let mut s = self.uncached;
        Self::fold_counters(&mut s, &self.caches);
        // `for o in Self::nodes(self.others)`, spelled out: Verus has no
        // `from_fn`.
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            let next = unsafe { self.node_next(o, Ghost::assume_new()) };
            // SAFETY: as above.
            unsafe { Self::fold_counters(&mut s, self.node_caches(o, Ghost::assume_new())) };
            o = next;
        }
        s
    }

    /// [`Heap::stats`] materialised into the heap for `thread.allocatedp`
    /// and `thread.deallocatedp` (design 13.4). The counters are derived
    /// on every read, so the pointers those keys hand out refer to this
    /// copy, which is refreshed by every read of any of the four
    /// `thread.*allocated*` keys and stale in between.
    pub fn materialise_stats(&mut self) -> &ThreadStats {
        self.snapshot = self.stats();
        &self.snapshot
    }

    /// `thread.tcache.enabled`.
    #[inline]
    pub fn tcache_enabled(&self) -> bool {
        self.tcache_enabled
    }

    /// `thread.tcache.enabled` write. Turning caches off folds the derived
    /// counters into `uncached` (so `thread.allocated` keeps counting),
    /// flushes and frees every cache of every arena, and makes
    /// `ensure_cache` refuse to create new ones; turning them on lets the
    /// next slow path create caches again.
    pub fn set_tcache_enabled(&mut self, shared: &Shared, on: bool, _ord: Tracked<&mut LockOrder>) {
        self.tcache_enabled = on;
        if on {
            return;
        }
        self.flush_all(shared, Tracked::assume_new());
        Self::fold_counters(&mut self.uncached, &self.caches);
        // SAFETY: the home array (null).
        unsafe { self.free_caches_in(shared, ptr::null_mut(), Tracked::assume_new()) };
        // The walk of `flush_all`. Folding and freeing a node's headers
        // leaves the list as it is.
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            let next = unsafe { self.node_next(o, Ghost::assume_new()) };
            // SAFETY: as above; the fold only reads the headers.
            unsafe {
                Self::fold_counters(
                    &mut self.uncached,
                    node_caches_detached(
                        o,
                        Tracked::assume_new(),
                        Ghost::assume_new(),
                        Ghost::assume_new(),
                    ),
                )
            };
            // SAFETY: `o` is a live node of this heap's `others` list.
            unsafe { self.free_caches_in(shared, o, Tracked::assume_new()) };
            o = next;
        }
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

    /// Flush every cache of one array (the home array when `node` is null,
    /// else the node's, whose arena is `arena`) completely.
    fn flush_arena(
        &mut self,
        shared: &Shared,
        arena: u16,
        node: *mut ArenaCaches,
        _ord: Tracked<&mut LockOrder>,
    ) {
        for class in 0..NCLASSES {
            // SAFETY: `node` is null or a live node owned by this heap; an
            // unused header has `len == 0` and is skipped.
            unsafe {
                let cache = self.header_in(node, class);
                let len = self.hdr_ref(cache).len as usize;
                if len != 0 {
                    self.flush(shared, arena, cache, class, len, Tracked::assume_new());
                }
            }
        }
    }

    /// Return every cached slot of this heap to its slab. Caches stay
    /// allocated.
    pub fn flush_all(&mut self, shared: &Shared, _ord: Tracked<&mut LockOrder>) {
        let home = self.arena;
        self.flush_arena(shared, home, ptr::null_mut(), Tracked::assume_new());
        // `for o in Self::nodes(self.others)`, spelled out: Verus has no
        // `from_fn`. As the iterator does, the node's `next` is read before
        // its turn.
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            let next = unsafe { self.node_next(o, Ghost::assume_new()) };
            // SAFETY: as above.
            let arena = unsafe { self.node_arena(o, Ghost::assume_new()) };
            self.flush_arena(shared, arena, o, Tracked::assume_new());
            o = next;
        }
    }

    /// Cache GC (design 6.4): for every cache, release three quarters of
    /// the entries that went unused since the last GC, then restart the
    /// low-water tracking from the current length.
    pub fn gc(&mut self, shared: &Shared, _ord: Tracked<&mut LockOrder>) {
        let home = self.arena;
        self.gc_arena(shared, home, ptr::null_mut(), Tracked::assume_new());
        // The walk of `flush_all`.
        let mut o = self.others;
        while !o.is_null() {
            // SAFETY: as in `flush_all`.
            let next = unsafe { self.node_next(o, Ghost::assume_new()) };
            // SAFETY: as above.
            let arena = unsafe { self.node_arena(o, Ghost::assume_new()) };
            self.gc_arena(shared, arena, o, Tracked::assume_new());
            o = next;
        }
    }

    /// One GC step over one array (the home array when `node` is null,
    /// else the node's, whose arena is `arena`).
    #[allow(
        clippy::while_let_loop,
        reason = "the desugared `for`, which Verus needs for `continue`"
    )]
    fn gc_arena(
        &mut self,
        shared: &Shared,
        arena: u16,
        node: *mut ArenaCaches,
        _ord: Tracked<&mut LockOrder>,
    ) {
        // `for class in 0..NCLASSES`, spelled out: Verus rejects `continue`
        // in a `for` loop.
        let mut iter = 0..NCLASSES;
        loop {
            let class = match iter.next() {
                Some(c) => c,
                None => break,
            };
            // SAFETY: `node` is null or a live node owned by this heap; an
            // unused header (`cap == 0`) is skipped.
            unsafe {
                let cache = self.header_in(node, class);
                if self.hdr_ref(cache).cap == 0 {
                    continue;
                }
                let lw = self.hdr_ref(cache).low_water as usize;
                if arena == FOREIGN {
                    // Never popped: everything it holds went unused.
                    let len = self.hdr_ref(cache).len as usize;
                    if len > 0 {
                        self.flush(shared, arena, cache, class, len, Tracked::assume_new());
                    }
                } else if lw > 0 {
                    self.flush(
                        shared,
                        arena,
                        cache,
                        class,
                        lw - lw / 4,
                        Tracked::assume_new(),
                    );
                }
                let h = self.hdr_mut(cache);
                h.low_water = h.len;
            }
        }
    }

    /// Count a slow-path entry and run periodic work every
    /// [`PERIODIC_EVERY`] of them: cache GC when the interval has elapsed,
    /// then decay (or starting the background thread that will do it,
    /// `purge::on_tick`). Nothing else: thread statistics are merged by
    /// `epoch` (`stats.rs`), not here. `ALLOC` is whether the caller is
    /// an allocation: only an allocation's tick may create the thread
    /// (design 8.1), because glibc calls `free` with locks held that
    /// `pthread_create` and `pthread_join` take again (the stack cache
    /// lock, from `pthread_join`'s TLS teardown).
    // Hot slow-path tick: left as written so its codegen does not move.
    #[allow(clippy::manual_is_multiple_of)]
    fn tick<const ALLOC: bool>(&mut self, shared: &Shared) {
        self.slow_ticks = self.slow_ticks.wrapping_add(1);
        if self.slow_ticks % PERIODIC_EVERY == 0 {
            let now = now_ns();
            if now.wrapping_sub(self.last_gc) >= shared.gc_interval() {
                self.last_gc = now;
                self.gc(shared, Tracked::assume_new());
            }
            crate::purge::on_tick(shared, now, ALLOC, vstd::prelude::Tracked::assume_new());
        }
    }
}
