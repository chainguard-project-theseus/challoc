//! A heap's caches for arenas other than its home arena: one
//! [`ArenaCaches`] node per arena it has allocated from explicitly, and
//! the one [`FOREIGN`] node that plain frees of other arenas' slots go
//! onto (design 6.2, 6.3 step 4), on a singly linked list.

use core::alloc::Layout;
use core::ptr;

use crate::layout::NCLASSES;

use super::{CacheHeader, FOREIGN, Heap, Shared};
#[cfg(verus_only)]
use super::{lemma_handed_ok, node_hdr};
#[cfg(verus_only)]
use crate::class::slot_loc;
#[cfg(verus_only)]
use crate::layout::process_regions;
#[cfg(verus_only)]
use vstd::map::Map;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, int, verus};
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
pub(super) fn node_block_layout() -> Layout {
    proof {
        assert(64usize & sub(64usize, 1) == 0) by (bit_vector);
    }
    Layout::from_size_align(
        core::mem::size_of::<ArenaCaches>() + 64 * (NODE_STAGGER_LINES - 1),
        64,
    )
    .expect("node block layout")
}

impl Heap {
    /// The header for `class` in an `others` node.
    ///
    /// Trusted: `&raw mut` through the raw node pointer.
    ///
    /// # Safety
    /// `node` is a live node owned by this heap.
    #[inline]
    #[verifier::external_body]
    pub(super) unsafe fn node_cache_ptr(node: *mut ArenaCaches, class: usize) -> (r: *mut CacheHeader)
        requires
            class < NCLASSES,
        ensures
            r == node_hdr(node, class as int),
    {
        debug_assert!(class < NCLASSES);
        // SAFETY: guaranteed by the caller; `class < NCLASSES`.
        unsafe { (&raw mut (*node).caches).cast::<CacheHeader>().add(class) }
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
    ) -> (r: (*mut u8, bool))
        requires
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
            r.0@.addr != 0 ==> process_regions().is_slot(class as int, r.0@.addr),
            r.0@.addr != 0 && r.1 ==> final(self).handed_ok(r.0, class as int),
    {
        broadcast use lemma_handed_ok;
        proof {
            shared.lemma_wf();
        }
        let node = self.other_caches(shared, arena, false);
        if !node.is_null() {
            // SAFETY: `node` is a live block owned by this heap.
            let cache = unsafe { Self::node_cache_ptr(node, class) };
            proof {
                assert(self.owns(node_hdr(node, class as int)));
                assert(self.hdr_class(node_hdr(node, class as int)) == class);
            }
            // Unused (`len == 0`) or this heap's cache.
            let len = self.hdr_ref(cache).len;
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
    pub(super) fn free_other(&mut self, shared: &Shared, class: usize, p: *mut u8, Tracked(tok): Tracked<GhostSingleton<int>>)
        requires
            process_regions().is_slot(class as int, p@.addr),
            tok.id() == slot_loc(class as int),
            tok@ == p@.addr as int,
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
    {
        let tracked mut tk = Map::<int, GhostSingleton<int>>::tracked_empty();
        proof {
            tk.tracked_insert(0, tok);
            shared.lemma_wf();
        }
        let node = self.other_caches(shared, FOREIGN, false);
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
        let tracked t = tk.tracked_remove(0);
        self.free_slow_in(shared, FOREIGN, class, p, Tracked(t))
    }
}

} // verus!

impl Heap {
    /// The nodes of an `others` list headed by `head`, in list order, for
    /// the cold walks (statistics, flush, GC, `cache_in`, destruction).
    /// Each node's `next` is read before the node is yielded, so a caller
    /// may free the node it was handed. The lookup on the cross-arena free
    /// path ([`Heap::other_caches`]) keeps its own loop.
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
    /// Trusted: the list walk dereferences raw node pointers, and a created
    /// node's `EMPTY` headers join the heap's permissions ([`Heap::wf`]),
    /// the one at index `c` caching class `c` ([`Heap::owns_node`]).
    #[verifier::external_body]
    pub(super) fn other_caches(
        &mut self,
        shared: &Shared,
        arena: u16,
        create: bool,
    ) -> (r: *mut ArenaCaches)
        requires
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
            r@.addr != 0 ==> final(self).owns_node(r),
    {
        let mut o = self.others;
        let mut n = 0usize;
        while !o.is_null() {
            // SAFETY: `others` is a list of live metadata blocks owned by
            // this heap.
            unsafe {
                if (*o).arena == arena {
                    return o;
                }
                o = (*o).next;
            }
            n += 1;
        }
        if !create {
            return ptr::null_mut();
        }
        assert_ne!(
            arena, self.arena,
            "heap: others node requested for the home arena"
        );
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
        unsafe {
            ptr::write(&raw mut (*node).arena, arena);
            ptr::write(&raw mut (*node).pad, pad as u8);
            ptr::write(&raw mut (*node).next, self.others);
            for c in 0..NCLASSES {
                ptr::write(Self::node_cache_ptr(node, c), CacheHeader::EMPTY);
            }
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
    ) -> (r: *mut CacheHeader)
        requires
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
            r@.addr != 0 ==> final(self).owns(r) && final(self).hdr_class(r) == class,
    {
        if arena == self.arena {
            proof {
                Heap::lemma_keeps_refl(*self);
            }
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
    ) -> (r: *mut CacheHeader)
        requires
            class < NCLASSES,
            node@.addr == 0 || old(self).owns_node(node),
        ensures
            *final(self) == *old(self),
            old(self).owns(r),
            old(self).hdr_class(r) == class,
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
