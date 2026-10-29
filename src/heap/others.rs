//! A heap's caches for arenas other than its home arena: one
//! [`ArenaCaches`] node per arena it has allocated from explicitly, and
//! the one [`FOREIGN`] node that plain frees of other arenas' slots go
//! onto (design 6.2, 6.3 step 4), on a singly linked list.

use core::alloc::Layout;
use core::ptr;

use crate::layout::NCLASSES;

use crate::arena::field_at;

use super::{CacheHeader, CachePerms, FOREIGN, Heap, Shared};
#[cfg(verus_keep_ghost)]
use crate::arena::field_ptr;
use crate::meta::MetaClaim;
#[cfg(verus_keep_ghost)]
use vstd::layout::{align_of, size_of};
#[cfg(verus_keep_ghost)]
use vstd::map::Map;
use vstd::prelude::verus;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, int};
use vstd::raw_ptr::{PointsTo, PointsToRaw};

verus! {

/// A heap's caches for one arena other than its home arena (design 6.2,
/// `caches[arena]`), allocated from the metadata region the first time the
/// heap allocates from with `MALLOCX_ARENA` (or that `thread.arena` moved
/// it away from), plus the one node keyed [`FOREIGN`] that every plain
/// `free` of another arena's slot pushes onto (design 6.3 step 4), kept
/// on a singly linked list with the same shape as the home caches
/// (headers inline). The `FOREIGN` node is flush-only: nothing pops from
/// it, its capacity is half the ladder and it is released whole when
/// full (the same batch as a home cache's half flush, a quarter of the
/// ladder held on average), its flush routes each run of slabs to the
/// slab's arena (`Shared::release_routed`), and there is one per thread,
/// so a thread that frees other threads' memory holds at most half a
/// cache's worth per class of it, less than jemalloc's tcache. A node
/// per arena instead, one the default path never pops from, would leave
/// a 16-thread Ruby process whose GC sweep frees every thread's garbage
/// from whichever thread runs it holding ~200 MiB of dead slots in 16 x
/// 16 of them.
/// Valkey's main thread has one arena node (the Lua arena).
#[repr(C)]
pub struct ArenaCaches {
    pub(super) arena: u16,
    /// Cache lines between the metadata block this node lives in and the
    /// node itself (`NODE_STAGGER_LINES`); `Heap::destroy` frees the block.
    pub(super) pad: u8,
    pub(super) next: *mut ArenaCaches,
    pub(super) caches: [CacheHeader; NCLASSES],
}

/// The permissions to an `others` node's block that the heap keeps for
/// it ([`Heap`]'s `heads`): to the fields of the node's first line that
/// verified code reads and writes after [`node_link`] stored them,
/// `arena`, `pad` and `next`; to the rest of the block outside the node's
/// headers (`rest`); and the block's metadata claim (`claim`).
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct NodeHead {
    pub arena: PointsTo<u16>,
    pub next: PointsTo<*mut ArenaCaches>,
    pub pad: PointsTo<u8>,
    pub rest: PointsToRaw,
    pub claim: MetaClaim,
}


/// The address of node `n`'s `next` field (the `const` assertion below).
pub open spec fn next_ptr(n: *mut ArenaCaches) -> *mut *mut ArenaCaches {
    field_ptr::<ArenaCaches, *mut ArenaCaches>(n, 8)
}

/// The address of node `n`'s `pad` field (the `const` assertion below).
pub open spec fn pad_ptr(n: *mut ArenaCaches) -> *mut u8 {
    field_ptr::<ArenaCaches, u8>(n, 2)
}

/// The size of the block an `others` node is carved from
/// ([`node_block_layout`]).
pub open spec fn block_size() -> int {
    size_of::<ArenaCaches>() as int + 64 * 15
}

/// [`block_size`]'s value: the node's lines and the 15 spare ones.
pub(super) proof fn lemma_block_size()
    ensures
        block_size() == 64 + 64 * NCLASSES + 64 * 15,
{
}

/// The block node `n` was carved from, `pad` lines below it
/// (`Heap::other_caches`), as `Heap::destroy` computes it.
pub open spec fn node_block(n: *mut ArenaCaches, pad: u8) -> *mut u8 {
    crate::vspec::ptr::ptr_sub(n as *mut u8, (64 * pad) as usize)
}

/// The bytes of the block node `n` was carved from, `pad` lines below it,
/// outside the node's `arena`, `pad` and `next` fields and its headers.
pub open spec fn rest_dom(n: *mut ArenaCaches, pad: u8) -> Set<int> {
    let (a, b) = (n@.addr as int, n@.addr - 64 * pad);
    Set::range(b, b + block_size()).difference(Set::range(a, a + 3)).difference(Set::range(a + 8, a + 16)).difference(
        Set::range(a + 64, a + 64 + 64 * NCLASSES),
    )
}

/// The permissions to node `n`'s block in `nh` other than to its
/// `arena` and `next` fields: its `pad` field, holding the node's line
/// offset in the block, the rest of the block outside the node's
/// headers, and the block's claim from the process's `Meta`.
pub open spec fn block_ok(nh: NodeHead, n: *mut ArenaCaches) -> bool {
    let pad = nh.pad.value();
    &&& nh.pad.ptr() == pad_ptr(n)
    &&& nh.pad.is_init()
    &&& pad < 16
    &&& 64 * pad <= n@.addr
    &&& nh.rest.dom() == rest_dom(n, pad)
    &&& nh.rest.provenance() == n@.provenance
    &&& nh.claim.held(crate::meta::process_live_id(), node_block(n, pad), block_size(), 64)
}

/// Node `i` of the list `l` with keys `k` has its first-line permissions
/// in `heads`: its `arena` holds its key and its `next` the node after it,
/// null after the last; and the rest of its block's
/// ([`block_ok`]).
pub open spec fn head_ok(heads: Map<*mut ArenaCaches, NodeHead>, l: Seq<*mut ArenaCaches>, k: Seq<u16>, i: int) -> bool {
    let n = l[i];
    &&& heads.contains_key(n)
    &&& heads[n].arena.ptr() == n as *mut u16
    &&& heads[n].arena.is_init()
    &&& heads[n].arena.value() == k[i]
    &&& heads[n].next.ptr() == next_ptr(n)
    &&& heads[n].next.is_init()
    &&& i + 1 < l.len() ==> heads[n].next.value() == l[i + 1]
    &&& i + 1 == l.len() ==> heads[n].next.value()@.addr == 0
    &&& block_ok(heads[n], n)
}

/// Every node of the list `l` with keys `k` is [`head_ok`] in `heads`
/// ([`Heap::list_ok`]). Opaque: a proof that needs a node's head
/// reveals it.
#[verifier::opaque]
pub open spec fn heads_ok(heads: Map<*mut ArenaCaches, NodeHead>, l: Seq<*mut ArenaCaches>, k: Seq<u16>) -> bool {
    forall|i: int| 0 <= i < l.len() ==> #[trigger] head_ok(heads, l, k, i)
}

/// `p`, 64-byte aligned, plus `off` (a multiple of `size_of::<T>()`,
/// which divides 64) is aligned for a `T`.
pub(super) proof fn lemma_field_aligned<T>(p: int, off: int)
    requires
        p % 64 == 0,
        0 < size_of::<T>(),
        64int % (size_of::<T>() as int) == 0,
        off % (size_of::<T>() as int) == 0,
    ensures
        (p + off) % (align_of::<T>() as int) == 0,
{
    broadcast use vstd::layout::group_align_properties;

    let s = size_of::<T>() as int;
    crate::layout::lemma_aligned_down(p, 64, s);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(p, s);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(off, s);
    let (a, b) = (p / s, off / s);
    assert(p + off == (a + b) * s) by (nonlinear_arith)
        requires
            p == s * a + p % s,
            off == s * b + off % s,
            p % s == 0,
            off % s == 0;
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(a + b, s);
    crate::layout::lemma_aligned_down(p + off, s, align_of::<T>() as int);
}

/// Pairwise distinct `u16`s are at most 2^16 (the bound on the `others`
/// list, whose nodes' keys are: `Heap::lemma_list_bound`).
pub(super) proof fn lemma_distinct_u16s(k: Seq<u16>)
    requires
        forall|i: int, j: int| #![trigger k[i], k[j]] 0 <= i < j < k.len() ==> k[i] != k[j],
    ensures
        k.len() <= 0x1_0000,
{
    let ki = k.map_values(|x: u16| x as int);
    assert(ki.no_duplicates()) by {
        assert forall|i: int, j: int| 0 <= i < ki.len() && 0 <= j < ki.len() && i != j implies ki[i] != ki[j] by {
            if i < j {
                assert(k[i] != k[j]);
            } else {
                assert(k[j] != k[i]);
            }
        }
    }
    ki.unique_seq_to_set();
    let r = vstd::set_lib::set_int_range(0, 0x1_0000);
    assert(ki.to_set().subset_of(r)) by {
        assert forall|x: int| #[trigger] ki.to_set().contains(x) implies r.contains(x) by {
            let i = choose|i: int| 0 <= i < ki.len() && ki[i] == x;
            assert(ki[i] == k[i] as int);
        }
    }
    vstd::set_lib::lemma_int_range(0, 0x1_0000);
    vstd::set_lib::lemma_len_subset(ki.to_set(), r);
}

// Verus checks the sizes the node offsets use.
#[cfg(feature = "quantum16")]
global layout ArenaCaches is size == 3392, align == 64;
#[cfg(feature = "quantum8")]
global layout ArenaCaches is size == 3648, align == 64;

} // verus!

/// A heap's `n`-th `others` node is placed `n % NODE_STAGGER_LINES` cache
/// lines into its metadata block, so that the node heads a cross-arena
/// `free` walks (and the stack tops it pushes to) do not share L1 sets:
/// blocks of one metadata bin are a fixed stride apart (12 KiB here), which
/// is a multiple of the L1D way size on both reference CPUs (4 KiB on the
/// Xeon, 16 KiB / 4 ways on Neoverse V2), and 15 aliasing node heads cost
/// xmalloc-test -w 16 12..25% of its throughput when the 40 KiB class's
/// foreign stack is not allocated between them.
const NODE_STAGGER_LINES: usize = 16;

/// Layout of the block an `others` node is carved from.
pub(super) fn node_block_layout() -> Layout {
    Layout::from_size_align(
        core::mem::size_of::<ArenaCaches>() + 64 * (NODE_STAGGER_LINES - 1),
        64,
    )
    .expect("node block layout")
}

/// The first line of a fresh `others` node: its `arena`, `pad` and `next`
/// ([`Heap::other_caches`]), stored field by field at their offsets (the
/// `const` assertion below), `next` read after the first two stores.
///
/// # Safety
/// `node` is the start of a fresh node in a block this heap holds.
#[inline(always)]
unsafe fn node_link(
    node: *mut ArenaCaches,
    arena: u16,
    pad: u8,
    next: &*mut ArenaCaches,
    _head: Tracked<PointsToRaw>,
) -> Tracked<NodeHead> {
    // SAFETY: guaranteed by the caller; each store is to its field.
    unsafe {
        ptr::write(node as *mut u16, arena);
        ptr::write(field_at::<ArenaCaches, u8>(node, 2), pad);
        ptr::write(field_at::<ArenaCaches, *mut ArenaCaches>(node, 8), *next);
    }
    Tracked::assume_new()
}

/// The address of node `o`'s `caches` array, computed as `&(*o).caches`
/// does: its second line (the `const` assertion below).
///
/// # Safety
/// `o` is a placed node.
#[inline(always)]
unsafe fn node_caches_ptr(o: *mut ArenaCaches) -> *mut [CacheHeader; NCLASSES] {
    // SAFETY: the node's first line is inside its block.
    unsafe { field_at::<ArenaCaches, [CacheHeader; NCLASSES]>(o, 64) }
}

/// `&(*o).caches` for node `o` of a heap's `others` list, as plain
/// headers, for the walk that folds them while it holds the heap mutably
/// (`Heap::set_tcache_enabled`). Shared, not `&mut`: the walk only reads
/// them.
///
/// # Safety
/// `o` is a live node owned by the heap, and nothing writes its headers
/// while the reference lives.
#[inline(always)]
#[allow(
    clippy::needless_lifetimes,
    reason = "the permission and the headers share the lifetime in the Verus signature"
)]
pub(super) unsafe fn node_caches_detached<'a>(
    o: *mut ArenaCaches,
    _perms: Tracked<&'a CachePerms>,
    _h: Ghost<Heap>,
    _r: Ghost<int>,
) -> &'a [CacheHeader; NCLASSES] {
    // SAFETY: guaranteed by the caller.
    unsafe { &*node_caches_ptr(o) }
}

/// `(*node).arena = arena`: the last step of `Heap::set_arena`'s
/// exchange, once node `node`'s headers and the home headers have traded
/// places (`Heap::swap_hdrs`), makes the node the cache of `arena`, the
/// home arena being left.
///
/// # Safety
/// `node` is a live node owned by the heap.
#[inline(always)]
pub(super) unsafe fn node_set_arena(
    node: *mut ArenaCaches,
    arena: u16,
    _perm: Tracked<&mut PointsTo<u16>>,
) {
    // SAFETY: guaranteed by the caller; `arena` is at the node's address.
    unsafe { ptr::write(node as *mut u16, arena) };
}

/// [`Heap::other_caches`]'s abort on a node for the home arena.
#[inline(always)]
#[track_caller]
fn assert_not_home(arena: &u16, home: &u16) {
    assert_ne!(
        *arena, *home,
        "heap: others node requested for the home arena"
    );
}

impl Heap {
    /// The header for `class` in an `others` node: `class` lines past the
    /// node's second, where `caches` starts (the `const` assertion below).
    ///
    /// # Safety
    /// `node` is a live node owned by this heap.
    #[inline]
    pub(super) unsafe fn node_cache_ptr(node: *mut ArenaCaches, class: usize) -> *mut CacheHeader {
        debug_assert!(class < NCLASSES);
        // SAFETY: guaranteed by the caller; `class < NCLASSES`.
        unsafe { ((node as *mut u8).add(64) as *mut CacheHeader).add(class) }
    }

    /// `(*o).next` for node `o` of the `others` list: the node after it,
    /// null after the last, read at its offset (the `const` assertion
    /// below). `destroy`, the one walk that unlinks nodes, frees each one
    /// after reading its `next`.
    ///
    /// # Safety
    /// `o` is a live node owned by this heap.
    #[inline(always)]
    pub(super) unsafe fn node_next(&self, o: *mut ArenaCaches, _r: Ghost<int>) -> *mut ArenaCaches {
        // SAFETY: the field is inside the node.
        unsafe { *field_at::<ArenaCaches, *mut ArenaCaches>(o, 8) }
    }

    /// `(*o).arena` for node `o` of the `others` list: the arena it
    /// caches, or [`FOREIGN`]. The two writers store it: [`node_link`] in
    /// `other_caches` (the new node's key, at the head) and
    /// [`node_set_arena`] in `set_arena` (the exchanged node's new key).
    ///
    /// # Safety
    /// As for [`Heap::node_next`].
    #[inline(always)]
    pub(super) unsafe fn node_arena(&self, o: *mut ArenaCaches, _r: Ghost<int>) -> u16 {
        // SAFETY: the field is at the node's address.
        unsafe { *(o as *mut u16) }
    }

    /// `(*o).pad` for node `o` of the `others` list: the lines between its
    /// block and it, which [`node_link`] stored.
    ///
    /// # Safety
    /// As for [`Heap::node_next`].
    #[inline(always)]
    pub(super) unsafe fn node_pad(&self, o: *mut ArenaCaches, _r: Ghost<int>) -> u8 {
        // SAFETY: the field is inside the node.
        unsafe { *field_at::<ArenaCaches, u8>(o, 2) }
    }

    /// `&(*o).caches` for node `o` of the `others` list, as plain headers
    /// (for [`Heap::stats`]).
    ///
    /// # Safety
    /// As for [`Heap::node_next`].
    #[inline(always)]
    pub(super) unsafe fn node_caches(
        &self,
        o: *mut ArenaCaches,
        _r: Ghost<int>,
    ) -> &[CacheHeader; NCLASSES] {
        // SAFETY: the node is placed and owned by this heap.
        unsafe { &*node_caches_ptr(o) }
    }

    /// Allocation of `class` from another arena than the home one: through
    /// this heap's cache for that arena if the `others` list has one with
    /// entries, else the slow path, whose `ensure_cache` creates the node
    /// and the cache only for a class this heap caches there
    /// (`Config::cap_for`) and serves the rest from the bitmaps.
    #[cold]
    #[inline(never)]
    pub(super) fn alloc_other(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
    ) -> (*mut u8, bool) {
        let node = self.other_caches(shared, arena, false);
        if !node.is_null() {
            // SAFETY: `node` is a live block owned by this heap.
            let cache = unsafe { Self::node_cache_ptr(node, class) };
            // SAFETY: unused (`len == 0`) or this heap's cache.
            let len = unsafe { self.hdr_ref(cache) }.len;
            if len != 0 {
                // SAFETY: a real cache with `len` entries.
                return unsafe { self.pop(cache, class, len, shared.pac()) };
            }
        }
        self.alloc_slow_in(shared, arena, class)
    }

    /// Free of a slot whose slab belongs to another arena than the home
    /// one: onto this heap's [`FOREIGN`] cache for the class if it exists
    /// and has room (xmalloc-test's consumers take this path on every
    /// free: a list walk and a push, no policy test), else the slow path,
    /// whose `ensure_cache` creates the node and the cache only for a
    /// class this heap caches there (`Config::cap_for`) and releases the
    /// rest straight to the owner's bitmap. The cache is never popped;
    /// its flush routes every slot to its slab's arena.
    #[cold]
    #[inline(never)]
    pub(super) fn free_other(&mut self, shared: &Shared, class: usize, p: *mut u8) {
        let node = self.other_caches(shared, FOREIGN, false);
        if !node.is_null() {
            // SAFETY: `node` is a live block owned by this heap; the cache
            // is unused (`len == cap == 0`) or ours.
            if unsafe {
                Self::push_cache_flags(Self::node_cache_ptr(node, class), p, shared.push_flags())
            } {
                return;
            }
        }
        self.free_slow_in(shared, FOREIGN, class, p)
    }

    /// The `others` node for `arena`, created (if `create`) on first use.
    /// Null when there is none (and none could be allocated). Never called
    /// with the home arena: creating a node for it would split the home
    /// caches in two, which the creation branch aborts on (the lookup walk
    /// is xmalloc's per-free path and cannot find a node that was never
    /// created).
    pub(super) fn other_caches(
        &mut self,
        shared: &Shared,
        arena: u16,
        create: bool,
    ) -> *mut ArenaCaches {
        let mut o = self.others;
        let mut n = 0usize;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                if self.node_arena(o, Ghost::assume_new()) == arena {
                    return o;
                }
                o = self.node_next(o, Ghost::assume_new());
            }
            n += 1;
        }
        if !create {
            return ptr::null_mut();
        }
        assert_not_home(&arena, &self.arena);
        let block = shared.meta.alloc(node_block_layout());
        if block.is_null() {
            return ptr::null_mut();
        }
        let pad = n % NODE_STAGGER_LINES;
        // SAFETY: the block has room for the node at any of the
        // `NODE_STAGGER_LINES` line offsets and is exclusively ours. Field
        // by field: a whole `ArenaCaches` temporary would put 3 KiB on the
        // stack of every call (this function sits on the cross-arena free
        // path).
        let node = unsafe { block.add(64 * pad) } as *mut ArenaCaches;
        // SAFETY: as above.
        unsafe { node_link(node, arena, pad as u8, &self.others, Tracked::assume_new()) };
        for c in 0..NCLASSES {
            // SAFETY: `c < NCLASSES`, inside the block.
            let cache = unsafe { Self::node_cache_ptr(node, c) };
            // SAFETY: as above.
            unsafe { ptr::write(cache, CacheHeader::EMPTY) };
        }
        self.others = node;
        node
    }

    /// The header for `(arena, class)`, creating the `others` node if
    /// needed. Null if no node could be allocated.
    pub(super) fn cache_slot(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
    ) -> *mut CacheHeader {
        if arena == self.arena {
            return self.cache_ptr(class);
        }
        let node = self.other_caches(shared, arena, true);
        if node.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `node` is a live block owned by this heap.
        unsafe { Self::node_cache_ptr(node, class) }
    }

    /// The header for `class` in the home array (`node` null) or in `node`.
    ///
    /// # Safety
    /// `node` is null or a live node owned by this heap.
    #[inline]
    pub(super) unsafe fn header_in(
        &mut self,
        node: *mut ArenaCaches,
        class: usize,
    ) -> *mut CacheHeader {
        // SAFETY: guaranteed by the caller.
        if node.is_null() {
            self.cache_ptr(class)
        } else {
            unsafe { Self::node_cache_ptr(node, class) }
        }
    }
}

// `node_caches_ptr` and `Heap::node_cache_ptr`: the headers start at the
// node's second line.
const _: () = assert!(core::mem::offset_of!(ArenaCaches, caches) == 64);
// `node_link`, `node_set_arena`, `Heap::node_arena`, `Heap::node_pad` and
// `Heap::node_next`: `arena` at the node's address, `pad` 2 bytes on,
// `next` 8 bytes on.
const _: () = assert!(
    core::mem::offset_of!(ArenaCaches, arena) == 0
        && core::mem::offset_of!(ArenaCaches, pad) == 2
        && core::mem::offset_of!(ArenaCaches, next) == 8
);

#[cfg(test)]
impl Heap {
    /// The nodes of an `others` list headed by `head`, in list order, for
    /// the tests' walks (`cache_in`). Each node's `next` is read before
    /// the node is yielded, so a caller may free the node it was handed.
    /// The library's walks (statistics, flush, GC, `set_tcache_enabled`,
    /// `destroy`, the lookup in [`Heap::other_caches`]) spell the same
    /// loop out over [`Heap::node_next`].
    pub(super) fn nodes(head: *mut ArenaCaches) -> impl Iterator<Item = *mut ArenaCaches> {
        let mut o = head;
        core::iter::from_fn(move || {
            if o.is_null() {
                return None;
            }
            let cur = o;
            // SAFETY: the list is live metadata blocks owned by the heap
            // whose `others` head this was.
            o = unsafe { (*cur).next };
            Some(cur)
        })
    }
}
