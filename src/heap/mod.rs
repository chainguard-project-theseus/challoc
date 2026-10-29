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
pub use live::Live;
pub use others::ArenaCaches;
#[cfg(test)]
pub use paths::free_fast;
pub(crate) use paths::shared_ptr;
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
pub use thread::{HeapPerm, TLS_CREATING, TLS_EXITED, fast_flags};
pub use thread::{existing_heap, heap_destructor, heap_for_thread, tls_heap_slot};

use core::alloc::Layout;
use core::ptr;

#[cfg(verus_keep_ghost)]
use crate::class::slot_loc;
#[cfg(verus_keep_ghost)]
use crate::class::{aset, distinct};
use crate::layout::{CLASSES, NCLASSES};
#[cfg(verus_keep_ghost)]
use crate::layout::{class_table, process_regions, slots_in};
#[cfg(verus_keep_ghost)]
use vstd::resource::set::GhostSubset;

use crate::arena::Homed;
#[cfg(verus_keep_ghost)]
use crate::arena::homed_on;
use crate::lock::LockOrder;
use crate::vspec::zero::Zeroed;
use fresh::FreshHeap;
use others::{node_block_layout, node_caches_detached, node_set_arena};
use paths::invalid_free;
#[cfg(test)]
use shared::ncpus;
use shared::{now_ns, tid};
use vstd::map::Map;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, verus};
use vstd::raw_ptr::{PointsTo, ptr_mut_ref, ptr_ref};
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

verus! {

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
/// [`PUSH_PAC`] set. Uninterpreted; [`Shared::pac`]'s trusted ensures is
/// the seam that ties it to what a well-formed `Shared`'s `pac` returns
/// (`push_flags & PUSH_PAC`, in a trusted body). One
/// value, since a process publishes one `Shared` and `push_flags` is
/// written once, before the `Shared` is published. The unit tests build
/// several with different flags, which that tie would contradict, so the
/// spec stands for the shipped library only.
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
    forall|i: int, j: int| #![trigger slot_ptr(slots, i), slot_ptr(slots, j)] lo <= i < j < hi ==> slot_ptr(slots, i) != slot_ptr(slots, j)
}

/// Every key of `new` but entries `lo..hi` of the stack at `slots` is as
/// in `old`.
spec fn ents_frame(new: Map<*mut *mut u8, GhostSingleton<int>>, old: Map<*mut *mut u8, GhostSingleton<int>>, slots: *mut *mut u8, lo: int, hi: int) -> bool {
    forall|q: *mut *mut u8| #![trigger new.contains_key(q)] #![trigger old.contains_key(q)] !in_entries(slots, q, lo, hi) ==> {
        &&& new.contains_key(q) == old.contains_key(q)
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
    /// of `claimed[..n]`) end up under the entries holding
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

verus! {

impl Heap {
    /// The heap's cache permissions are well formed ([`CachePerms::wf`]):
    /// every header it owns satisfies [`CacheHeader::inv`] and owns its
    /// stack; and they track what the caches hold
    /// ([`CachePerms::tracks`]): every stack entry belongs to one header
    /// and index, and each of a header's first `len` entries is a slot
    /// start of the header's class. Each of those entries holds its slot's
    /// token ([`CachePerms::tokened`]), each entry of a zero prefix holds
    /// its zero token ([`CachePerms::zero_ok`]), and each stack holds its
    /// claim ([`CachePerms::claimed`]). [`CachePerms::fresh`] states the
    /// permissions of a heap that caches nothing. The process's regions
    /// are well formed ([`regions_ok`]) and its `Shared` is published
    /// ([`crate::api::ready`]), and the heap's `shared` field is that
    /// `Shared`, at its storage. The `others` list is [`Heap::list`]
    /// ([`Heap::list_ok`]). The home headers are [`Heap::home_hdrs`]
    /// ([`Heap::home_ok`]); they are also the plain field `caches`, whose
    /// value Verus models apart from the permissions to the same bytes. A
    /// heap counted in its home arena's bound count holds its ticket
    /// ([`Heap::homed_ok`]).
    pub closed spec fn wf(&self) -> bool {
        &&& self.perms@.wf()
        &&& self.perms@.tracks()
        &&& self.perms@.zero_ok()
        &&& self.perms@.tokened()
        &&& self.perms@.claimed()
        &&& regions_ok()
        &&& crate::api::ready()
        &&& self.shared == Shared::storage() as *const Shared
        &&& self.list_ok()
        &&& self.home_ok()
        &&& self.homed_ok()
    }

    /// The heap's ticket ([`Homed`]): a heap counted in `Arenas::bound`
    /// (its `bound` field) holds the ticket for its home arena, a heap
    /// that is not counted holds none, and a heap that may still move its
    /// home (`rehome`) is counted.
    pub closed spec fn homed_ok(&self) -> bool {
        &&& self.bound == self.homed@ is Some
        &&& self.homed@ matches Some(t) ==> homed_on(t, self.arena as int)
        &&& self.rehome ==> self.bound
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
        &&& self.perms@.claimed()
        &&& regions_ok()
        &&& crate::api::ready()
        &&& self.shared == Shared::storage() as *const Shared
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

    /// The heap's home headers are those inside the heap at `h`
    /// ([`home_hdr`]), and the heap's block, from `h`, does not wrap the
    /// address space (it is a metadata block).
    pub(super) closed spec fn home_at(&self, h: *mut Heap) -> bool {
        &&& self.home_hdrs@ == Seq::new(NCLASSES as nat, |c: int| home_hdr(h, c))
        &&& h@.addr + vstd::layout::size_of::<Heap>() <= usize::MAX
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
    /// [`others::heads_ok`]). The nodes' keys ([`Heap::keys`]: the arena a
    /// node caches, or [`FOREIGN`]) are pairwise distinct and none is the
    /// home arena.
    pub closed spec fn list_ok(&self) -> bool {
        let l = self.list@;
        let k = self.keys@;
        &&& l.len() == 0 ==> self.others@.addr == 0
        &&& l.len() > 0 ==> self.others == l[0]
        &&& forall|i: int| 0 <= i < l.len() ==> (#[trigger] l[i])@.addr != 0 && self.owns_node(l[i])
        &&& forall|i: int, j: int| #![trigger l[i], l[j]] 0 <= i < j < l.len() ==> l[i] != l[j]
        &&& k.len() == l.len()
        &&& forall|i: int, j: int| #![trigger k[i], k[j]] 0 <= i < j < k.len() ==> k[i] != k[j]
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
    /// the process's `Shared` is published ([`regions_ok`], [`crate::api::ready`])
    /// and its `shared` is that one; a thread heap, counted and holding its
    /// ticket ([`Heap::homed_ok`]).
    proof fn lemma_fresh_wf(&self, h: *mut Heap)
        requires
            self.perms@.fresh(h),
            self.list@.len() == 0,
            self.keys@.len() == 0,
            self.others@.addr == 0,
            self.shared == Shared::storage() as *const Shared,
            self.home_hdrs@ == Seq::new(NCLASSES as nat, |c: int| home_hdr(h, c)),
            regions_ok(),
            crate::api::ready(),
            self.bound,
            self.homed@ matches Some(t) && homed_on(t, self.arena as int),
        ensures
            self.wf(),
    {
        reveal(others::heads_ok);
        reveal(CachePerms::claimed);
        assert forall|c: int| 0 <= c < NCLASSES implies self.owns(#[trigger] self.home_hdrs@[c]) && self.hdr_class(self.home_hdrs@[c]) == c by {
            assert(self.home_hdrs@[c] == home_hdr(h, c));
        }
    }

    /// The heap may still move its home ([`Heap::rehome`]); the measure
    /// of the free paths' recursion through [`Heap::rehome_on_free`].
    pub closed spec fn spec_rehome(&self) -> bool {
        self.rehome
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

    /// A header a pop may take from: the header for `class` of the home
    /// arena or of a node whose key is an arena, never [`FOREIGN`]. A pop
    /// that requires it never takes from the [`FOREIGN`] node.
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

    /// Cache routing, on a
    /// well-formed heap: the `others` keys are pairwise distinct and none
    /// is the home arena; a header is routed for at most one arena
    /// ([`Heap::routes`]); and no header [`Heap::poppable`] admits is one
    /// of the [`FOREIGN`] node's, so a pop that requires it never takes
    /// from that node.
    pub proof fn theorem_cache_routing(&self, class: int, cache: *mut CacheHeader, a: u16, b: u16)
        requires
            self.wf(),
            0 <= class < NCLASSES,
        ensures
            forall|r: int, s: int| #![trigger self.node_key(r), self.node_key(s)] 1 <= r < s <= self.list_len() ==> self.node_key(r) != self.node_key(s),
            forall|r: int| 1 <= r <= self.list_len() ==> #[trigger] self.node_key(r) != self.home(),
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
    /// process's regions are well formed and separated, and the allocator
    /// is published ([`crate::api::ready`]).
    pub proof fn lemma_wf_regions(&self)
        requires
            self.wf(),
        ensures
            regions_ok(),
            crate::api::ready(),
    {
    }

    /// A well-formed heap's `shared` is the process's `Shared`, at its
    /// storage, which is published.
    proof fn lemma_wf_shared(&self)
        requires
            self.wf(),
        ensures
            self.shared == Shared::storage() as *const Shared,
            crate::api::ready(),
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
    /// is metadata: an owned header's 64
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


    /// The headers of a node the heap owns: each has its initialised
    /// permission in the heap's ([`cell_ok`]), which holds its value.
    proof fn lemma_node_cells(&self, o: *mut ArenaCaches)
        requires
            self.wf(),
            self.owns_node(o),
        ensures
            forall|c: int| 0 <= c < NCLASSES ==> self.perms@.hdrs.contains_key(#[trigger] node_hdr(o, c)),
            forall|c: int| 0 <= c < NCLASSES ==> self.perms@.hdrs[#[trigger] node_hdr(o, c)].ptr() == node_hdr(o, c),
            forall|c: int| 0 <= c < NCLASSES ==> self.perms@.hdrs[#[trigger] node_hdr(o, c)].is_init(),
            forall|c: int| 0 <= c < NCLASSES ==> self.hdr(#[trigger] node_hdr(o, c)) == self.perms@.hdrs[node_hdr(o, c)].value(),
    {
        assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] cell_ok(node_hdr(o, c), self.perms@.hdrs[node_hdr(o, c)], self.perms@.slots) by {
            assert(self.owns(node_hdr(o, c)));
        }
    }

    /// The class owned header `p` caches ([`CachePerms::cls`]).
    pub closed spec fn hdr_class(&self, p: *mut CacheHeader) -> int {
        self.perms@.cls[p]
    }

    /// Every slot this heap's caches hold holds its token
    /// ([`CachePerms::tokened`]).
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

    /// Every slot the heap's caches hold: those
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
    /// emptied or trimmed, whatever its key.
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
        &&& perms.blocks == self.perms@.blocks
        &&& perms.released == self.perms@.released
        &&& perms.tokened()
    }

    /// `self` caches exactly what `old` cached: the same cache
    /// permissions (headers, stacks, entries, and the slot tokens and zero
    /// tokens under them), the same `caches` field, `shared` pointer,
    /// `others` list and keys, home headers, home arena, binding, ticket
    /// and `rehome`. The other fields (`tid`, `slow_ticks`, `last_gc`,
    /// `uncached`, `snapshot`, `tcache_enabled`, `rng`, `rehome_from`) are
    /// unconstrained.
    pub closed spec fn caches_same(&self, old: Heap) -> bool {
        &&& self.perms@ == old.perms@
        &&& self.caches == old.caches
        &&& self.others == old.others
        &&& self.shared == old.shared
        &&& self.list@ == old.list@
        &&& self.keys@ == old.keys@
        &&& self.home_hdrs@ == old.home_hdrs@
        &&& self.heads == old.heads
        &&& self.arena == old.arena
        &&& self.bound == old.bound
        &&& self.rehome == old.rehome
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

    /// Uniqueness, for one cached entry of a well-formed
    /// heap: it stands for a slot of its header's class, and that slot is
    /// in no `held`, any other holder of tokens of the class's authority
    /// (the bitmap words' invariants, which hold the free slots'; a claim
    /// in flight; a live block's token, `heap::live::Live`, which the
    /// program holds from the allocation to the free).
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

    /// Uniqueness within one well-formed heap: two different
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

    /// Uniqueness against the program's blocks: a slot this
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

    /// Uniqueness across heaps: a slot two well-formed
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
            new.shared == old.shared,
            new.home_hdrs == old.home_hdrs,
            new.heads == old.heads,
            new.bound == old.bound,
            new.homed == old.homed,
            new.rehome == old.rehome,
            new.perms@.keeps(old.perms@),
            new.perms@.follows(old.perms@),
            new.perms@.zero_ok(),
            new.perms@.tokened(),
            new.perms@.claimed(),
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
            b.perms@.blocks == a.perms@.blocks,
            b.list == a.list,
            b.keys == a.keys,
            b.arena == a.arena,
            b.bound == a.bound,
            b.homed == a.homed,
            b.rehome == a.rehome,
            b.others == a.others,
            b.shared == a.shared,
            b.home_hdrs == a.home_hdrs,
            b.heads == a.heads,
        ensures
            b.keeps(a),
    {
        reveal(CachePerms::claimed);
        Heap::lemma_home_kept(a, b);
        CachePerms::lemma_zero_same(a.perms@, b.perms@);
        CachePerms::lemma_keeps_refl(a.perms@);
        CachePerms::lemma_tracks_same(a.perms@);
        CachePerms::lemma_same_but_tokens(a.perms@, b.perms@, a.perms@);
    }

    /// The accounting for one [`Heap::flush`] of `cache`, from
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
            new.rehome == old.rehome,
            new.others == old.others,
            new.shared == old.shared,
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

    /// `&*cache` for an owned header through the header's permission
    /// alone, for the refill and the flush, which hold the stack
    /// permissions apart from the heap for a while, so [`Heap::hdr_ref`]'s
    /// `Heap::wf` does not hold there.
    ///
    /// # Safety
    /// `cache` is a header this heap owns (a home header or one of a live
    /// `others` node's), and nothing writes it while the reference lives;
    /// a verified caller proves it with the `requires`.
    #[inline(always)]
    unsafe fn hdr_at(&self, cache: *mut CacheHeader) -> (r: &CacheHeader)
        requires
            self.perms@.hdrs.contains_key(cache),
            self.perms@.hdrs[cache].ptr() == cache,
            self.perms@.hdrs[cache].is_init(),
        ensures
            *r == self.perms@.hdrs[cache].value(),
    {
        vstd::raw_ptr::ptr_ref(cache, Tracked(self.perms.borrow().hdrs.tracked_borrow(cache)))
    }
}


/// The home header for `class` of the heap at `h`: `h + 128 + 64 *
/// class`, where [`Heap::create_detached`] writes it and
/// [`Heap::cache_ptr`] computes it.
pub(super) open spec fn home_hdr(h: *mut Heap, class: int) -> *mut CacheHeader {
    vstd::raw_ptr::ptr_mut_from_data(vstd::raw_ptr::PtrData::<CacheHeader> {
        addr: (h@.addr + 128 + 64 * class) as usize,
        provenance: h@.provenance,
        metadata: (),
    })
}
/// [`Heap::handed_ok`] unfolded, for the heap's pops and their callers.
broadcast proof fn lemma_handed_ok(h: Heap, p: *mut u8, c: int)
    ensures
        #[trigger] h.handed_ok(p, c) == CachePerms::handed_ok(h.perms@.handed, p, c),
{
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
        &&& forall|c: int| #![trigger self.hdrs.contains_key(home_hdr(h, c))] #![trigger self.cls.contains_key(home_hdr(h, c))] #![trigger self.cls[home_hdr(h, c)]] 0 <= c < NCLASSES ==> self.hdrs.contains_key(home_hdr(h, c)) && self.cls.contains_key(home_hdr(h, c)) && self.cls[home_hdr(h, c)] == c
    }

}

impl Heap {
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
}

} // verus!

const _: () = assert!(core::mem::offset_of!(Heap, caches) == 128);

verus! {

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
// Its own solver: in the module's shared one the per-entry
// postconditions fail on the earlier queries' state.
#[verifier::spinoff_prover]
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
        final(perms).dom() == old(perms).dom(),
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

} // verus!

/// `assert_eq!(*len, 0, ..)`, the abort of [`Heap::free_caches`] and
/// [`Heap::free_caches_in`] on a cache that still holds slots: `*len == 0`
/// after it returns.
#[inline(always)]
#[track_caller]
fn assert_flushed(len: &u32) {
    assert_eq!(*len, 0, "heap: freeing a cache that was not flushed");
}

verus! {

/// `assert_eq!(*len, 0, ..)`, [`Heap::refill`]'s abort on a heap
/// inconsistency: `*len == 0` after it returns.
///
/// Trusted: `assert_eq!` calls `core::panicking::assert_failed`, which
/// Verus cannot specify on stable Rust.
#[verifier::external_body]
#[inline(always)]
#[track_caller]
fn assert_refill_empty(len: &u32)
    requires
        crate::sys::may_abort() || *len == 0,
    ensures
        *len == 0,
{
    assert_eq!(*len, 0, "heap: refill of a cache that is not empty");
}

} // verus!

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
                        self.scan_or_abort(shared, cache, p);
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
        self.push(shared, class, p, Tracked::assume_new());
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
            crate::purge::on_tick(
                shared,
                now,
                ALLOC,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
    }
}

verus! {

impl Heap {
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
        &&& self.rehome == old.rehome
        &&& self.homed == old.homed
        &&& self.others == old.others
        &&& self.shared == old.shared
        &&& self.home_hdrs == old.home_hdrs
        &&& self.heads == old.heads
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

    /// `self` is `old` with owned header `cache` holding `v`, its stack
    /// untouched.
    pub closed spec fn updates(&self, old: Heap, cache: *mut CacheHeader, v: CacheHeader) -> bool {
        &&& self.perms@.slots == old.perms@.slots
        &&& self.perms@.hdrs == old.perms@.hdrs.insert(cache, self.perms@.hdrs[cache])
        &&& self.perms@.zero == old.perms@.zero
        &&& self.perms@.toks == old.perms@.toks
        &&& self.perms@.blocks == old.perms@.blocks
        &&& self.perms@.owner == old.perms@.owner
        &&& self.perms@.cls == old.perms@.cls
        &&& self.perms@.hdrs[cache].ptr() == cache
        &&& self.perms@.hdrs[cache].is_init()
        &&& self.perms@.hdrs[cache].value() == v
        &&& self.list == old.list
        &&& self.keys == old.keys
        &&& self.arena == old.arena
        &&& self.bound == old.bound
        &&& self.rehome == old.rehome
        &&& self.homed == old.homed
        &&& self.others == old.others
        &&& self.shared == old.shared
        &&& self.home_hdrs == old.home_hdrs
        &&& self.heads == old.heads
    }

    /// Trade the values of owned headers `a` and `b` of one class
    /// (`thread.arena`'s exchange of the home caches with a node's,
    /// [`Heap::set_arena`]); each stack goes with its header, through the
    /// headers' permissions, and the heap stays well formed with the same
    /// shape.
    ///
    /// Unsafe for its callers' sake: the requires carry its precondition,
    /// and its caller (`set_arena`) is unverified.
    ///
    /// # Safety
    /// `a` and `b` are distinct headers of one class this heap owns (the
    /// requires).
    unsafe fn swap_hdrs(&mut self, a: *mut CacheHeader, b: *mut CacheHeader)
        requires
            old(self).wf(),
            old(self).owns(a),
            old(self).owns(b),
            a != b,
            old(self).hdr_class(a) == old(self).hdr_class(b),
        ensures
            final(self).wf(),
            final(self).same_shape(*old(self)),
            final(self).hdr(a) == old(self).hdr(b),
            final(self).hdr(b) == old(self).hdr(a),
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
            reveal(CachePerms::claimed);
            assert forall|p: *mut CacheHeader| #[trigger] self.perms@.hdrs.contains_key(p) && self.perms@.hdr(p).cap != 0 implies stack_claimed(self.perms@.hdr(p), self.perms@.blocks) by {
                if p == a {
                    assert(stack_claimed(o.hdr(b), o.blocks));
                } else if p == b {
                    assert(stack_claimed(o.hdr(a), o.blocks));
                } else {
                    assert(stack_claimed(o.hdr(p), o.blocks));
                }
            }
        }
    }

    /// `&*cache` for an owned header (a `(*cache).field` load), through
    /// its permission.
    ///
    /// Unsafe for its callers' sake: the requires carry its precondition,
    /// and most of its callers are unverified.
    ///
    /// # Safety
    /// `cache` is a header this heap owns (a home header or one of a live
    /// `others` node's), and nothing writes it while the reference lives.
    #[inline(always)]
    unsafe fn hdr_ref(&self, cache: *mut CacheHeader) -> (r: &CacheHeader)
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

    /// `&mut *cache` for an owned header (`(*cache).field` stores),
    /// through its permission ([`Heap::updates`]: the header holds the
    /// stored value, the rest of the heap is as it was).
    ///
    /// Unsafe for its callers' sake: the requires carry its precondition,
    /// and most of its callers are unverified.
    ///
    /// # Safety
    /// `cache` is a header this heap owns (a home header or one of a live
    /// `others` node's), and nothing else reads or writes it while the
    /// reference lives (the requires).
    #[inline(always)]
    unsafe fn hdr_mut(&mut self, cache: *mut CacheHeader) -> (r: &mut CacheHeader)
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

    /// Home arena (design 6.2 `heap.arena`; `thread.arena`).
    #[inline]
    pub fn arena(&self) -> u16 {
        self.arena
    }

    /// [`Heap::arena`] through a volatile load, for the `free` check's
    /// arena test ([`Shared::home_owns`]): a plain load is hoisted above
    /// the multi-arena test (`FREE_ROUTE`) and touches the heap's header
    /// line on every single-arena `free`.
    ///
    /// Trusted: Verus has no volatile read; the value is not specified,
    /// since `home_owns` only compares it.
    #[inline(always)]
    #[verifier::external_body]
    pub(super) fn arena_volatile(&self) -> u16
        opens_invariants none
    {
        // SAFETY: `self` is a live heap and `arena` a plain field.
        unsafe { ptr::read_volatile(&raw const self.arena) }
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
            // SAFETY: `o` is the `r`-th node from the end of this heap's
            // `others` list (the invariant).
            let next = unsafe { self.node_next(o, Ghost(r)) };
            // SAFETY: as above.
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
        ensures
            final(self).wf(),
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
}

} // verus!
