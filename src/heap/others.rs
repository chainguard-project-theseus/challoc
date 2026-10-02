//! A heap's caches for arenas other than its home arena: one
//! [`ArenaCaches`] node per arena it has allocated from explicitly, and
//! the one [`FOREIGN`] node that plain frees of other arenas' slots go
//! onto (design 6.2, 6.3 step 4), on a singly linked list.

use core::alloc::Layout;
use core::ptr;

use crate::layout::NCLASSES;

use super::{CacheHeader, FOREIGN, Heap, Shared};
#[cfg(verus_only)]
use super::{CachePerms, cell_ok, lemma_handed_ok, node_hdr, node_placed};
#[cfg(verus_only)]
use crate::class::slot_loc;
#[cfg(verus_only)]
use crate::layout::process_regions;
#[cfg(verus_only)]
use vstd::map::Map;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, int, verus};
use vstd::raw_ptr::{PointsToRaw, ptr_mut_write};
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

/// The first line of a fresh `others` node: its `arena`, `pad` and `next`
/// ([`Heap::other_caches`]), stored field by field, `next` read after the
/// first two stores.
///
/// Trusted: the stores go through `&raw mut` projections of the raw node
/// pointer, which Verus does not support. `head` is the permission to
/// the line they write, and is used up.
///
/// # Safety
/// `node` is the start of a fresh node in a block this heap holds.
#[inline(always)]
#[verifier::external_body]
unsafe fn node_link(node: *mut ArenaCaches, arena: u16, pad: u8, next: &*mut ArenaCaches, Tracked(head): Tracked<PointsToRaw>)
    requires
        head.is_range(node@.addr as int, 64),
        head.provenance() == node@.provenance,
        node@.addr % 64 == 0,
{
    // SAFETY: guaranteed by the caller.
    unsafe {
        ptr::write(&raw mut (*node).arena, arena);
        ptr::write(&raw mut (*node).pad, pad);
        ptr::write(&raw mut (*node).next, *next);
    }
}

/// `&mut (*o).caches` for node `o`, the `r`-th from the end of heap `h`'s
/// `others` list, as plain headers, for the walk that folds them while it
/// holds `h` mutably (`Heap::set_tcache_enabled`): the
/// reference is not tied to a borrow of `h`. Trusted: a reference through
/// the raw node pointer; nothing is claimed about the headers.
///
/// # Safety
/// `o` is a live node owned by `h` (the requires), and nothing else
/// refers to its headers while the reference lives; that second half is
/// the caller's to keep, which verification does not check.
#[inline(always)]
#[verifier::external_body]
pub(super) unsafe fn node_caches_mut<'a>(o: *mut ArenaCaches, Ghost(h): Ghost<Heap>, Ghost(r): Ghost<int>) -> &'a mut [CacheHeader; NCLASSES]
    requires
        h.wf(),
        1 <= r <= h.list_len(),
        o == h.node_from_end(r),
{
    // SAFETY: guaranteed by the caller.
    unsafe { &mut (*o).caches }
}

/// `(*node).arena = arena`: the last step of `Heap::set_arena`'s
/// exchange, once node `node`'s headers and the home headers have traded
/// places (`Heap::swap_hdrs`), makes the node the cache of `arena`, the
/// home arena being left.
///
/// Trusted: a store through the raw node pointer, to the node's first
/// line, which no header permission covers ([`node_link`] used it up);
/// nothing is claimed.
///
/// # Safety
/// `node` is a live node owned by `h` (the requires).
#[inline(always)]
#[verifier::external_body]
pub(super) unsafe fn node_set_arena(node: *mut ArenaCaches, arena: u16, Ghost(h): Ghost<Heap>)
    requires
        h.owns_node(node),
{
    // SAFETY: guaranteed by the caller.
    unsafe { (*node).arena = arena };
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
    /// list: the node after it, null after the last.
    ///
    /// Trusted: a read through the raw node pointer. The claim is that the
    /// nodes' `next` fields chain the list as [`Heap::list`] has it
    /// ([`Heap::list_ok`]): `create_detached` starts the list empty,
    /// `other_caches` links a node in at the head, `set_arena`'s exchange
    /// (`Heap::swap_hdrs`, [`node_set_arena`]) leaves `next` alone, and
    /// `destroy`, the one writer that unlinks nodes, frees the heap.
    ///
    /// # Safety
    /// `o` is a live node owned by this heap (the requires).
    #[inline(always)]
    #[verifier::external_body]
    pub(super) unsafe fn node_next(&self, o: *mut ArenaCaches, Ghost(r): Ghost<int>) -> (n: *mut ArenaCaches)
        requires
            self.wf(),
            1 <= r <= self.list_len(),
            o == self.node_from_end(r),
        ensures
            r > 1 ==> n == self.node_from_end(r - 1),
            r == 1 ==> n@.addr == 0,
    {
        // SAFETY: guaranteed by the caller.
        unsafe { (*o).next }
    }

    /// `(*o).arena` for node `o`, the `r`-th from the end of the `others`
    /// list. Trusted: a read through the raw node pointer; nothing is
    /// claimed about the value.
    ///
    /// # Safety
    /// As for [`Heap::node_next`].
    #[inline(always)]
    #[verifier::external_body]
    pub(super) unsafe fn node_arena(&self, o: *mut ArenaCaches, Ghost(r): Ghost<int>) -> u16
        requires
            self.wf(),
            1 <= r <= self.list_len(),
            o == self.node_from_end(r),
    {
        // SAFETY: guaranteed by the caller.
        unsafe { (*o).arena }
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
            shared.wf(),
            class < NCLASSES,
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

// `node_hdr` and `Heap::node_cache_ptr`: the headers start at the node's
// second line.
const _: () = assert!(core::mem::offset_of!(ArenaCaches, caches) == 64);

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
    ) -> (r: *mut ArenaCaches)
        requires
            old(self).wf(),
            shared.wf(),
            create ==> arena != old(self).arena,
        ensures
            final(self).keeps(*old(self)),
            r@.addr != 0 ==> final(self).owns_node(r),
            r@.addr != 0 ==> final(self).in_list(r),
    {
        proof {
            self.lemma_list(0);
            self.axiom_list_bound();
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
        let (block, Tracked(raw)) = shared.meta.alloc_raw(node_block_layout());
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
        unsafe {
            node_link(node, arena, pad as u8, &self.others, Tracked(head));
        }
        for c in 0..NCLASSES
            invariant
                self.wf(),
                self.keeps(h0),
                h0 == *old(self),
                self.list == h0.list,
                self.others == others0,
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
                Heap::lemma_keeps_trans(h0, before, *self);
            }
        }
        let ghost s1 = *self;
        self.others = node;
        proof {
            self.list = Ghost(seq![node] + h0.list@);
            assert forall|i: int| 0 <= i < h0.list@.len() implies h0.list@[i] != node by {
                h0.lemma_list(h0.list@.len() - i);
                assert(h0.list@[i] == h0.node_from_end(h0.list@.len() - i));
                assert(h0.owns(node_hdr(h0.list@[i], 0)));
            }
            assert(self.list@[0] == node);
            assert(self.list@.contains(node));
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
    ) -> (r: *mut CacheHeader)
        requires
            class < NCLASSES,
            old(self).wf(),
            shared.wf(),
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
            old(self).wf(),
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
