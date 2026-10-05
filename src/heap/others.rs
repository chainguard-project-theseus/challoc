//! A heap's caches for arenas other than its home arena: one
//! [`ArenaCaches`] node per arena it has allocated from explicitly, and
//! the one [`FOREIGN`] node that plain frees of other arenas' slots go
//! onto (design 6.2, 6.3 step 4), on a singly linked list.

use core::alloc::Layout;
use core::ptr;

use crate::layout::NCLASSES;
use crate::lock::LockOrder;
#[cfg(verus_only)]
use crate::lock::rank_meta;

use super::{CacheHeader, FOREIGN, Heap, Live, Shared};
#[cfg(verus_only)]
use super::{CachePerms, cell_ok, lemma_handed_ok, node_hdr, node_placed};
use crate::arena::field_at;
#[cfg(verus_only)]
use crate::arena::field_ptr;
#[cfg(verus_only)]
use crate::class::slot_loc;
#[cfg(verus_only)]
use crate::layout::process_regions;
#[cfg(verus_only)]
use vstd::layout::{align_of, size_of};
#[cfg(verus_only)]
use vstd::map::Map;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, int, verus};
use vstd::raw_ptr::{PointsTo, PointsToRaw, ptr_mut_write, ptr_ref};
#[cfg(verus_only)]
use vstd::resource::set::GhostSingleton;

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

/// A heap's `n`-th `others` node is placed `n % NODE_STAGGER_LINES` cache
/// lines into its metadata block, so that the node heads a cross-arena
/// `free` walks (and the stack tops it pushes to) do not share L1 sets:
/// blocks of one metadata bin are a fixed stride apart (12 KiB here), which
/// is a multiple of the L1D way size on both reference CPUs (4 KiB on the
/// Xeon, 16 KiB / 4 ways on Neoverse V2), and 15 aliasing node heads cost
/// xmalloc-test -w 16 12..25% of its throughput when the 40 KiB class's
/// foreign stack is not allocated between them.
const NODE_STAGGER_LINES: usize = 16;

// Verus checks the sizes `node_block_layout` and the node offsets use.
#[cfg(feature = "quantum16")]
global layout ArenaCaches is size == 3392, align == 64;
#[cfg(feature = "quantum8")]
global layout ArenaCaches is size == 3648, align == 64;

/// Layout of the block an `others` node is carved from.
pub(super) fn node_block_layout() -> (r: Layout)
    ensures
        crate::vspec::layout::layout_size(r) == vstd::layout::size_of::<ArenaCaches>() + 64 * 15,
        crate::vspec::layout::layout_align(r) == 64,
{
    proof {
        assert(64usize & sub(64usize, 1) == 0) by (bit_vector);
    }
    Layout::from_size_align(
        core::mem::size_of::<ArenaCaches>() + 64 * (NODE_STAGGER_LINES - 1),
        64,
    )
    .expect("node block layout")
}

/// The permissions to the fields of an `others` node's first line that
/// verified code reads and writes after [`node_link`] stored them: `arena`
/// (at the node's address) and `next` (8 bytes on, [`next_ptr`]). A heap
/// holds one per node of its list ([`Heap::heads`], [`heads_ok`]).
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct NodeHead {
    pub arena: PointsTo<u16>,
    pub next: PointsTo<*mut ArenaCaches>,
}

/// The address of node `n`'s `next` field (the `const` assertion below).
pub open spec fn next_ptr(n: *mut ArenaCaches) -> *mut *mut ArenaCaches {
    field_ptr::<ArenaCaches, *mut ArenaCaches>(n, 8)
}

/// Node `i` of the list `l` with keys `k` has its first-line permissions
/// in `heads`: its `arena` holds its key and its `next` the node after it,
/// null after the last.
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
}

/// Every node of the list `l` with keys `k` is [`head_ok`] in `heads`
/// ([`Heap::list_ok`]). Opaque: only the node accessors and the two
/// writers of the list (`Heap::other_caches`, `Heap::set_arena`) look
/// inside.
#[verifier::opaque]
pub open spec fn heads_ok(heads: Map<*mut ArenaCaches, NodeHead>, l: Seq<*mut ArenaCaches>, k: Seq<u16>) -> bool {
    forall|i: int| 0 <= i < l.len() ==> #[trigger] head_ok(heads, l, k, i)
}

/// `p`, 64-byte aligned, plus `off` (a multiple of `size_of::<T>()`,
/// which divides 64) is aligned for a `T`.
proof fn lemma_field_aligned<T>(p: int, off: int)
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

/// The first line of a fresh `others` node: its `arena`, `pad` and `next`
/// ([`Heap::other_caches`]), stored field by field, `next` read after the
/// first two stores, through permissions taken out of `head`, the
/// permission to the line. The ones to `arena` and `next` come back, for
/// the heap to keep ([`Heap::heads`]); the rest of the line is dropped.
///
/// # Safety
/// `node` is the start of a fresh node in a block this heap holds.
#[inline(always)]
unsafe fn node_link(node: *mut ArenaCaches, arena: u16, pad: u8, next: &*mut ArenaCaches, Tracked(head): Tracked<PointsToRaw>) -> (r: Tracked<NodeHead>)
    requires
        head.is_range(node@.addr as int, 64),
        head.provenance() == node@.provenance,
        node@.addr % 64 == 0,
        node_placed(node),
    ensures
        r@.arena.ptr() == node as *mut u16,
        r@.arena.is_init(),
        r@.arena.value() == arena,
        r@.next.ptr() == next_ptr(node),
        r@.next.is_init(),
        r@.next.value() == *next,
{
    broadcast use vstd::layout::layout_of_primitives, vstd::layout::layout_of_references_and_pointers_for_sized_types,
        vstd::raw_ptr::group_raw_ptr_axioms;

    let ghost base = node@.addr as int;
    let tracked (ha, rest) = head.split(Set::range(base, base + 2));
    let tracked (hp, rest) = rest.split(Set::range(base + 2, base + 3));
    let tracked (hn, _) = rest.split(Set::range(base + 8, base + 16));
    proof {
        lemma_field_aligned::<u16>(base, 0);
        lemma_field_aligned::<u8>(base, 2);
        lemma_field_aligned::<*mut ArenaCaches>(base, 8);
        crate::vspec::ptr::axiom_ptr_add_addr(node as *mut u8, 2);
        crate::vspec::ptr::axiom_ptr_add_addr(node as *mut u8, 8);
    }
    let tracked mut pa = ha.into_typed::<u16>(base as usize);
    let tracked mut pp = hp.into_typed::<u8>((base + 2) as usize);
    let tracked mut pn = hn.into_typed::<*mut ArenaCaches>((base + 8) as usize);
    proof {
        assert(pa.ptr()@ == (node as *mut u16)@);
        assert(pp.ptr()@ == field_ptr::<ArenaCaches, u8>(node, 2)@);
        assert(pn.ptr()@ == next_ptr(node)@);
    }
    // SAFETY: guaranteed by the caller; each store goes through the
    // permission to its field.
    unsafe {
        ptr_mut_write(node as *mut u16, Tracked(&mut pa), arena);
        ptr_mut_write(field_at::<ArenaCaches, u8>(node, 2), Tracked(&mut pp), pad);
        ptr_mut_write(field_at::<ArenaCaches, *mut ArenaCaches>(node, 8), Tracked(&mut pn), *next);
    }
    Tracked(NodeHead { arena: pa, next: pn })
}

/// `&(*o).caches` for node `o`, the `r`-th from the end of heap `h`'s
/// `others` list, as plain headers, for the walk that folds them while it
/// holds `h` mutably (`Heap::set_tcache_enabled`): the reference is not
/// tied to a borrow of `h`. Trusted: a reference through the raw node
/// pointer; nothing is claimed about the headers. Shared, not `&mut`: the
/// walk only reads them, and a write through a reference no permission
/// follows would leave the headers' permissions in `h` stale.
///
/// # Safety
/// `o` is a live node owned by `h` (the requires), and nothing writes its
/// headers while the reference lives; that second half is the caller's
/// to keep, which verification does not check.
#[inline(always)]
#[verifier::external_body]
pub(super) unsafe fn node_caches_detached<'a>(o: *mut ArenaCaches, Ghost(h): Ghost<Heap>, Ghost(r): Ghost<int>) -> &'a [CacheHeader; NCLASSES]
    requires
        h.wf(),
        1 <= r <= h.list_len(),
        o == h.node_from_end(r),
{
    // SAFETY: guaranteed by the caller.
    unsafe { &(*o).caches }
}

/// `(*node).arena = arena`: the last step of `Heap::set_arena`'s
/// exchange, once node `node`'s headers and the home headers have traded
/// places (`Heap::swap_hdrs`), makes the node the cache of `arena`, the
/// home arena being left. The store goes through the permission to the
/// field ([`NodeHead::arena`]), which the caller takes out of the heap's
/// [`Heap::heads`] and puts back with `arena` as the node's key in
/// [`Heap::keys`].
///
/// # Safety
/// `node` is a live node (the permission).
#[inline(always)]
pub(super) unsafe fn node_set_arena(node: *mut ArenaCaches, arena: u16, Tracked(perm): Tracked<&mut PointsTo<u16>>)
    requires
        old(perm).ptr() == node as *mut u16,
    ensures
        final(perm).ptr() == old(perm).ptr(),
        final(perm).is_init(),
        final(perm).value() == arena,
{
    ptr_mut_write(node as *mut u16, Tracked(perm), arena);
}

/// [`Heap::other_caches`]'s abort on a node for the home arena, which its
/// callers rule out (the requires).
///
/// Trusted: `assert_ne!` calls `core::panicking::assert_failed`, which
/// Verus cannot specify on stable Rust.
#[inline(always)]
#[verifier::external_body]
fn assert_not_home(arena: &u16, home: &u16)
    requires
        *arena != *home,
{
    assert_ne!(*arena, *home, "heap: others node requested for the home arena");
}

impl Heap {
    /// The header for `class` in an `others` node: `class` lines past the
    /// node's second, where `caches` starts (the `const` assertion below).
    ///
    /// # Safety
    /// `node` is a live node owned by this heap.
    #[inline]
    pub(super) unsafe fn node_cache_ptr(node: *mut ArenaCaches, class: usize) -> (r: *mut CacheHeader)
        requires
            class < NCLASSES,
            node_placed(node),
        ensures
            r == node_hdr(node, class as int),
    {
        proof {
            super::cache::lemma_header_layout();
            crate::vspec::ptr::axiom_ptr_add_addr(node as *mut u8, 64);
            let first = crate::vspec::ptr::ptr_add(node as *mut u8, 64usize) as *mut CacheHeader;
            assert(first@.addr == node@.addr + 64 && first@.provenance == node@.provenance);
            assert(class as int * 64 <= 64 * NCLASSES) by (nonlinear_arith)
                requires
                    class < NCLASSES;
            crate::vspec::ptr::axiom_ptr_add_addr(first, class);
        }
        debug_assert!(class < NCLASSES);
        // SAFETY: guaranteed by the caller; `class < NCLASSES`.
        unsafe { ((node as *mut u8).add(64) as *mut CacheHeader).add(class) }
    }

    /// `(*o).next` for node `o`, the `r`-th from the end of the `others`
    /// list: the node after it, null after the last, read through the
    /// permission to the field the heap holds ([`Heap::heads`],
    /// [`head_ok`]): `create_detached` starts the list empty and
    /// `other_caches` links a node in at the head with its `next`, both
    /// proved; across calls the permissions are part of [`Heap::wf`],
    /// which the trusted conversions that hand a heap to verified code
    /// (`tcaches::heap_at`, `thread::heap_from_slot`) assume. The
    /// unverified readers of `next` ([`Heap::nodes`], for `destroy` and
    /// `cache_in`) only read it; `destroy`, the one writer that unlinks
    /// nodes, frees the heap.
    ///
    /// # Safety
    /// `o` is a live node owned by this heap (the requires).
    #[inline(always)]
    pub(super) unsafe fn node_next(&self, o: *mut ArenaCaches, Ghost(r): Ghost<int>) -> (n: *mut ArenaCaches)
        requires
            self.wf(),
            1 <= r <= self.list_len(),
            o == self.node_from_end(r),
        ensures
            r > 1 ==> n == self.node_from_end(r - 1),
            r == 1 ==> n@.addr == 0,
    {
        proof {
            self.lemma_head(r);
            self.lemma_list(r);
        }
        // SAFETY: the permission is for the field, inside the node.
        unsafe { *ptr_ref(field_at::<ArenaCaches, *mut ArenaCaches>(o, 8), Tracked(&self.heads.borrow().tracked_borrow(o).next)) }
    }

    /// `(*o).arena` for node `o`, the `r`-th from the end of the `others`
    /// list: its key ([`Heap::node_key`]), read through the permission to
    /// the field the heap holds ([`Heap::heads`], [`head_ok`]). The two
    /// writers store the key they record: [`node_link`] in `other_caches`
    /// (the new node's key, at the head) and [`node_set_arena`] in
    /// `set_arena` (the exchanged node's new key).
    ///
    /// # Safety
    /// As for [`Heap::node_next`].
    #[inline(always)]
    pub(super) unsafe fn node_arena(&self, o: *mut ArenaCaches, Ghost(r): Ghost<int>) -> (a: u16)
        requires
            self.wf(),
            1 <= r <= self.list_len(),
            o == self.node_from_end(r),
        ensures
            a == self.node_key(r),
    {
        proof {
            self.lemma_head(r);
        }
        *ptr_ref(o as *mut u16, Tracked(&self.heads.borrow().tracked_borrow(o).arena))
    }

    /// Node `r` from the end of the `others` list is [`head_ok`]
    /// ([`Heap::list_ok`]).
    proof fn lemma_head(&self, r: int)
        requires
            self.wf(),
            1 <= r <= self.list_len(),
        ensures
            head_ok(self.heads@, self.list@, self.keys@, self.list@.len() - r),
            self.node_from_end(r) == self.list@[self.list@.len() - r],
            self.node_key(r) == self.keys@[self.keys@.len() - r],
            self.keys@.len() == self.list@.len(),
    {
        reveal(heads_ok);
        assert(head_ok(self.heads@, self.list@, self.keys@, self.list@.len() - r));
    }

    /// The `others` list's nodes are [`heads_ok`] ([`Heap::list_ok`]).
    pub(super) proof fn lemma_heads(&self)
        requires
            self.list_ok(),
        ensures
            heads_ok(self.heads@, self.list@, self.keys@),
    {
    }

    /// `&(*o).caches` for node `o`, the `r`-th from the end of the
    /// `others` list, as plain headers (for [`Heap::stats`]). Trusted: a
    /// reference through the raw node pointer; nothing is claimed about
    /// the headers.
    ///
    /// # Safety
    /// As for [`Heap::node_next`].
    #[inline(always)]
    #[verifier::external_body]
    pub(super) unsafe fn node_caches(&self, o: *mut ArenaCaches, Ghost(r): Ghost<int>) -> &[CacheHeader; NCLASSES]
        requires
            self.wf(),
            1 <= r <= self.list_len(),
            o == self.node_from_end(r),
    {
        // SAFETY: guaranteed by the caller.
        unsafe { &(*o).caches }
    }

    /// Allocation of `class` from another arena than the home one: through
    /// this heap's cache for that arena if the `others` list has one with
    /// entries, else the slow path, whose `ensure_cache` creates the node
    /// and the cache only for a class this heap caches there
    /// (`Config::cap_for`) and serves the rest from the bitmaps.
    /// `arena` is not [`FOREIGN`], so the node popped is the one keyed
    /// `arena` ([`Heap::other_caches`]), never the flush-only one.
    #[cold]
    #[inline(never)]
    pub(super) fn alloc_other(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
        Tracked(blk): Tracked<&mut super::Live>,
    ) -> (r: (*mut u8, bool))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            class < NCLASSES,
            arena != FOREIGN,
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
            r.0@.addr != 0 ==> process_regions().is_slot(class as int, r.0@.addr),
            r.0@.addr != 0 && r.1 ==> final(self).handed_ok(r.0, class as int),
            r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
    {
        broadcast use lemma_handed_ok;
        proof {
            shared.lemma_wf();
        }
        let node = self.other_caches(shared, arena, false, Tracked(&mut LockOrder::spare()));
        if !node.is_null() {
            // SAFETY: `node` is a live block owned by this heap.
            let cache = unsafe { Self::node_cache_ptr(node, class) };
            proof {
                assert(self.owns(node_hdr(node, class as int)));
                assert(self.hdr_class(node_hdr(node, class as int)) == class);
                self.lemma_keyed_routes(node, arena, class as int);
                self.lemma_poppable(arena, class as int, cache);
            }
            // Unused (`len == 0`) or this heap's cache.
            let len = self.hdr_ref(cache).len;
            if len != 0 {
                // SAFETY: a real cache with `len` entries.
                return unsafe { self.pop(cache, class, len, shared.pac(), Tracked(blk)) };
            }
        }
        self.alloc_slow_in(shared, arena, class, Tracked(blk))
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
    pub(super) fn free_other(&mut self, shared: &Shared, class: usize, p: *mut u8, Tracked(blk): Tracked<Live>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            process_regions().is_slot(class as int, p@.addr),
            blk.slot_block(p, class as int),
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
    {
        // As in `Heap::push`.
        let tracked mut blk = blk;
        let ghost orig = blk;
        let tracked mut tk = Map::<int, GhostSingleton<int>>::tracked_empty();
        proof {
            shared.lemma_wf();
            shared.lemma_regions_ok();
            blk.lemma_mem();
            let tracked tok = blk.take_slot(class as int);
            tk.tracked_insert(0, tok);
        }
        let node = self.other_caches(shared, FOREIGN, false, Tracked(&mut LockOrder::spare()));
        if !node.is_null() {
            proof {
                assert(self.owns(node_hdr(node, class as int)));
            }
            // SAFETY: `node` is a live block owned by this heap; the cache
            // is unused (`len == cap == 0`) or ours.
            if unsafe {
                Self::push_cache_flags(Self::node_cache_ptr(node, class), p, shared.push_flags(), Tracked(self.perms.borrow_mut()), Tracked(&mut tk))
            } {
                return;
            }
        }
        proof {
            blk.put_slot(tk.tracked_remove(0), orig, p, class as int);
        }
        self.free_slow_in(shared, FOREIGN, class, p, Tracked(blk))
    }
}

/// Pairwise distinct `u16`s are at most 2^16 (the bound on the `others`
/// list, whose nodes' keys are: `Heap::lemma_list_bound`).
pub(super) proof fn lemma_distinct_u16s(k: Seq<u16>)
    requires
        forall|i: int, j: int| 0 <= i < j < k.len() ==> k[i] != k[j],
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
        assert forall|x: int| ki.to_set().contains(x) implies r.contains(x) by {
            let i = choose|i: int| 0 <= i < ki.len() && ki[i] == x;
            assert(ki[i] == k[i] as int);
        }
    }
    vstd::set_lib::lemma_int_range(0, 0x1_0000);
    vstd::set_lib::lemma_len_subset(ki.to_set(), r);
}

} // verus!

// `node_hdr` and `Heap::node_cache_ptr`: the headers start at the node's
// second line.
const _: () = assert!(core::mem::offset_of!(ArenaCaches, caches) == 64);
// `node_link`, `Heap::node_arena` and `Heap::node_next` (`next_ptr`):
// `arena` at the node's address, `pad` 2 bytes on, `next` 8 bytes on.
const _: () = assert!(
    core::mem::offset_of!(ArenaCaches, arena) == 0
        && core::mem::offset_of!(ArenaCaches, pad) == 2
        && core::mem::offset_of!(ArenaCaches, next) == 8
);

impl Heap {
    /// The nodes of an `others` list headed by `head`, in list order, for
    /// the cold walks Verus does not check (`destroy`, `cache_in`). Each
    /// node's `next` is read before the node is yielded, so a caller may
    /// free the node it was handed. The verified walks (statistics, flush,
    /// GC, `set_tcache_enabled`, the lookup in [`Heap::other_caches`])
    /// spell the same loop out over [`Heap::node_next`].
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

verus! {

impl Heap {
    /// The `others` node for `arena`, created (if `create`) on first use.
    /// Null when there is none (and none could be allocated). Never called
    /// with the home arena: creating a node for it would split the home
    /// caches in two, which the creation branch aborts on (the lookup walk
    /// is xmalloc's per-free path and cannot find a node that was never
    /// created).
    ///
    /// The lookup walk reads each node's `arena` and `next` through
    /// [`Heap::node_arena`] and [`Heap::node_next`]. A created node's
    /// headers come out of the block's permission ([`Meta::alloc_raw`]),
    /// written `EMPTY`; that they are new to the heap follows from the
    /// permissions it already holds (`PointsTo::is_disjoint`).
    ///
    /// [`Meta::alloc_raw`]: crate::meta::Meta::alloc_raw
    pub(super) fn other_caches(
        &mut self,
        shared: &Shared,
        arena: u16,
        create: bool,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: *mut ArenaCaches)
        requires
            crate::sys::may_syscall(),
            old(self).wf(),
            shared.wf(),
            create ==> arena != old(self).arena,
            create ==> old(ord).allows(rank_meta()),
        ensures
            *final(ord) == *old(ord),
            final(self).keeps(*old(self)),
            r@.addr != 0 ==> final(self).owns_node(r),
            r@.addr != 0 ==> final(self).in_list(r),
            r@.addr != 0 ==> final(self).keyed(r, arena),
            r@.addr == 0 ==> !final(self).has_key(arena),
    {
        proof {
            self.lemma_list(0);
            self.lemma_list_bound();
            Heap::lemma_keeps_refl(*self);
        }
        let mut o = self.others;
        let mut n = 0usize;
        let ghost mut r = self.list_len();
        while !o.is_null()
            invariant
                *self == *old(self),
                self.wf(),
                self.keeps(*old(self)),
                self.list_len() < usize::MAX,
                0 <= r <= self.list_len(),
                r > 0 ==> o == self.node_from_end(r),
                r == 0 ==> o@.addr == 0,
                n == self.list_len() - r,
                forall|i: int| 0 <= i < self.list_len() - r ==> #[trigger] self.keys@[i] != arena,
                self.keys@.len() == self.list_len(),
            decreases r,
        {
            proof {
                self.lemma_list(r);
                self.lemma_in_list(r);
            }
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                if self.node_arena(o, Ghost(r)) == arena {
                    proof {
                        assert(self.keys@[self.list_len() - r] == arena);
                    }
                    return o;
                }
                o = self.node_next(o, Ghost(r));
            }
            n += 1;
            proof {
                r = r - 1;
            }
        }
        if !create {
            return ptr::null_mut();
        }
        assert_not_home(&arena, &self.arena);
        proof {
            shared.lemma_meta();
            assert(crate::meta::MAX_BINNED == 0x10_0000usize) by (compute_only);
        }
        let (block, Tracked(blk)) = shared.meta.alloc_raw(node_block_layout(), Tracked(ord));
        let tracked (raw, _) = blk;
        if block.is_null() {
            return ptr::null_mut();
        }
        let pad = n % NODE_STAGGER_LINES;
        let ghost size = vstd::layout::size_of::<ArenaCaches>() + 64 * 15;
        proof {
            crate::vspec::ptr::axiom_raw_valid(&raw, block, size as int);
            crate::vspec::ptr::axiom_ptr_add_addr(block, (64 * pad) as usize);
            assert(size == 64 + 64 * NCLASSES + 64 * 15);
        }
        // SAFETY: the block has room for the node at any of the
        // `NODE_STAGGER_LINES` line offsets and is exclusively ours. Field
        // by field: a whole `ArenaCaches` temporary would put 3 KiB on the
        // stack of every call (this function sits on the cross-arena free
        // path).
        let node = unsafe { block.add(64 * pad) } as *mut ArenaCaches;
        let ghost base = node@.addr as int;
        proof {
            assert(base == block@.addr + 64 * pad);
            assert(base % 64 == 0) by {
                assert(block@.addr as int % 64 == 0);
            }
        }
        let tracked (head, mut rest) = raw.split(Set::range(base, base + 64));
        proof {
            crate::vspec::ptr::axiom_raw_valid(&head, node as *mut u8, 64);
            assert(Set::range(base + 64, base + 64 + 64 * NCLASSES) <= rest.dom());
            crate::vspec::ptr::axiom_raw_valid(&rest, node_hdr(node, 0) as *mut u8, 64 * NCLASSES);
            assert(node_placed(node));
        }
        let ghost h0 = *self;
        let ghost others0 = self.others;
        // SAFETY: as above.
        let Tracked(nh) = unsafe { node_link(node, arena, pad as u8, &self.others, Tracked(head)) };
        for c in 0..NCLASSES
            invariant
                self.wf(),
                self.keeps(h0),
                h0 == *old(self),
                self.list == h0.list,
                self.keys == h0.keys,
                self.arena == h0.arena,
                self.others == others0,
                self.heads == h0.heads,
                nh.arena.ptr() == node as *mut u16,
                nh.arena.is_init(),
                nh.arena.value() == arena,
                nh.next.ptr() == next_ptr(node),
                nh.next.is_init(),
                nh.next.value() == others0,
                others0 == old(self).others,
                rest.provenance() == node@.provenance,
                rest.contains_range(base + 64 + 64 * c, 64 * (NCLASSES - c)),
                base == node@.addr,
                base % 64 == 0,
                node_placed(node),
                crate::layout::process_regions().in_meta(base, 64 + 64 * NCLASSES),
                forall|k: int| 0 <= k < c ==> self.owns(#[trigger] node_hdr(node, k)) && self.hdr_class(node_hdr(node, k)) == k,
                forall|k: int| 0 <= k < c ==> !h0.owns(#[trigger] node_hdr(node, k)),
        {
            let ghost a = base + 64 + 64 * c;
            proof {
                super::cache::lemma_header_layout();
                assert(a % 64 == 0) by (nonlinear_arith)
                    requires
                        a == base + 64 + 64 * c,
                        base % 64 == 0;
            }
            let tracked (hraw, rest2) = rest.split(Set::range(a, a + 64));
            proof {
                rest = rest2;
                assert(Set::range(a + 64, a + 64 + 64 * (NCLASSES - c - 1)) <= rest.dom());
            }
            let tracked mut hp = hraw.into_typed::<CacheHeader>(a as usize);
            // SAFETY: `c < NCLASSES`.
            let cache = unsafe { Self::node_cache_ptr(node, c) };
            proof {
                assert(hp.ptr() == cache);
                if self.perms@.hdrs.contains_key(cache) {
                    let tracked other = self.perms.borrow().hdrs.tracked_borrow(cache);
                    assert(cell_ok(cache, self.perms@.hdrs[cache], self.perms@.slots));
                    hp.is_disjoint(other);
                    assert(false);
                }
                assert(!h0.owns(cache));
            }
            ptr_mut_write(cache, Tracked(&mut hp), CacheHeader::EMPTY);
            let ghost before = *self;
            proof {
                let ghost old_perms = self.perms@;
                self.perms.borrow_mut().hdrs.tracked_insert(cache, hp);
                self.perms.borrow_mut().cls = old_perms.cls.insert(cache, c as int);
                assert(cell_ok(cache, self.perms@.hdrs[cache], self.perms@.slots));
                CachePerms::lemma_insert_unused(old_perms, self.perms@, cache, c as int);
                CachePerms::lemma_claimed_frame(before.perms@, self.perms@);
                Heap::lemma_keeps_trans(h0, before, *self);
            }
        }
        let ghost s1 = *self;
        self.others = node;
        proof {
            self.list = Ghost(seq![node] + h0.list@);
            self.keys = Ghost(seq![arena] + h0.keys@);
            self.heads.borrow_mut().tracked_insert(node, nh);
            assert forall|i: int| 0 <= i < h0.list@.len() implies h0.list@[i] != node by {
                h0.lemma_list(h0.list@.len() - i);
                assert(h0.list@[i] == h0.node_from_end(h0.list@.len() - i));
                assert(h0.owns(node_hdr(h0.list@[i], 0)));
            }
            assert(self.list@[0] == node);
            assert(self.list@.contains(node));
            assert(h0.keys@.len() == h0.list@.len());
            assert forall|i: int| 0 <= i < h0.keys@.len() implies #[trigger] h0.keys@[i] != arena by {
                assert(h0.keys@[i] == old(self).keys@[i]);
            }
            assert(self.keys@[0] == arena);
            assert forall|i: int| 1 <= i < self.keys@.len() implies self.keys@[i] == h0.keys@[i - 1] by {}
            assert forall|i: int| 1 <= i < self.list@.len() implies self.list@[i] == h0.list@[i - 1] by {}
            assert forall|i: int| 0 <= i < h0.list@.len() implies self.owns_node(#[trigger] h0.list@[i]) by {
                h0.lemma_list(h0.list@.len() - i);
                assert(h0.list@[i] == h0.node_from_end(h0.list@.len() - i));
                Heap::lemma_keeps_node(h0, s1, h0.list@[i]);
            }
            // The new node's headers were not the heap's before, so none
            // is a home header.
            assert forall|cc: int, i: int, k: int|
                0 <= cc < NCLASSES && 0 <= i < self.list@.len() && 0 <= k < NCLASSES
                    implies #[trigger] self.home_hdrs@[cc] != #[trigger] node_hdr(self.list@[i], k) by {
                assert(h0.owns(h0.home_hdrs@[cc]));
                if i > 0 {
                    assert(self.list@[i] == h0.list@[i - 1]);
                }
            }
            // The new node's `next` is the old head, and every other
            // node's permissions are where they were.
            h0.lemma_heads();
            reveal(heads_ok);
            assert forall|i: int| 0 <= i < self.list@.len() implies #[trigger] head_ok(self.heads@, self.list@, self.keys@, i) by {
                if i > 0 {
                    assert(head_ok(h0.heads@, h0.list@, h0.keys@, i - 1));
                    assert(self.list@[i] == h0.list@[i - 1]);
                    if i + 1 < self.list@.len() {
                        assert(self.list@[i + 1] == h0.list@[i]);
                    }
                } else {
                    h0.lemma_list(h0.list@.len() as int);
                    if h0.list@.len() > 0 {
                        assert(self.list@[1] == h0.list@[0]);
                    }
                }
            }
        }
        node
    }

    /// The header for `(arena, class)`, creating the `others` node if
    /// needed. Null if no node could be allocated.
    pub(super) fn cache_slot(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: *mut CacheHeader)
        requires
            crate::sys::may_syscall(),
            class < NCLASSES,
            old(self).wf(),
            shared.wf(),
            old(ord).allows(rank_meta()),
        ensures
            *final(ord) == *old(ord),
            final(self).keeps(*old(self)),
            r@.addr != 0 ==> final(self).owns(r) && final(self).hdr_class(r) == class,
            r@.addr != 0 ==> final(self).routes(arena, class as int, r),
    {
        if arena == self.arena {
            proof {
                Heap::lemma_keeps_refl(*self);
                self.lemma_routes_home(class as int);
            }
            return self.cache_ptr(class);
        }
        let node = self.other_caches(shared, arena, true, Tracked(ord));
        if node.is_null() {
            return ptr::null_mut();
        }
        proof {
            self.lemma_keyed_routes(node, arena, class as int);
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
    ) -> (r: *mut CacheHeader)
        requires
            class < NCLASSES,
            old(self).wf(),
            node@.addr == 0 || old(self).owns_node(node),
        ensures
            *final(self) == *old(self),
            old(self).owns(r),
            old(self).hdr_class(r) == class,
            r == old(self).array_hdr(node, class as int),
    {
        // SAFETY: guaranteed by the caller.
        if node.is_null() {
            self.cache_ptr(class)
        } else {
            unsafe { Self::node_cache_ptr(node, class) }
        }
    }
}

} // verus!

/// Kani check of the `assert_ne!` abort (record under `verus/seams/`).
#[cfg(kani)]
mod kani_aborts {
    use super::assert_not_home;

    /// S124 `assert_not_home`, `requires *arena != *home`: under it the
    /// call returns, for every pair.
    #[kani::proof]
    fn assert_not_home_returns() {
        let arena: u16 = kani::any();
        let home: u16 = kani::any();
        kani::assume(arena != home);
        assert_not_home(&arena, &home);
    }
}
