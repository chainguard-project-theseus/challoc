//! A fresh heap's plain fields, the 128 bytes before its home headers:
//! [`init_fields`] stores them through the permission to them and hands
//! that permission on in a [`FreshHeap`] token, which only this module
//! can build or change (its fields are private here).

use core::ptr;

#[cfg(verus_keep_ghost)]
use super::heap_head;
use super::others::ArenaCaches;
use super::{FOREIGN, Heap, Shared, ThreadStats, now_ns, shared_ptr, tid};
#[cfg(verus_keep_ghost)]
use vstd::modes::tracked_swap;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, verus};
use vstd::raw_ptr::ptr_mut_write;

verus! {

/// The plain fields of the fresh heap at [`FreshHeap::heap`]: the
/// permission to them ([`FreshHeap::head`], the 128 bytes before
/// `caches`, [`heap_head`]), which [`init_fields`] takes and keeps here,
/// and what [`init_fields`] stored in them: the `Shared`, the home arena
/// and `bound`, which it stores as `false`. Linear and not `Copy` (it
/// holds the permission): the one holder of the heap-head permission,
/// made by [`init_fields`] once per heap. That `shared`, `arena` and
/// `bound` are what the heap's fields hold is not a fact Verus checks
/// (the permission forgets the values): `init_fields` sets them to what
/// it stores, and nothing else builds, takes apart or changes one (its
/// fields are private to this module). It records only `init_fields`'
/// stores: `mark_bound` and `mark_rehome` (heap/mod.rs) set `bound` and
/// `rehome` through the raw heap pointer and leave the token as it was,
/// so after them its `bound` no longer matches the field. It is valid
/// while its heap is live, and must be dropped before `destroy`, which
/// gets the whole heap block back, head included. `Tcaches::create`
/// drops it.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub(super) tracked struct FreshHeap {
    ghost heap: *mut Heap,
    tracked head: vstd::raw_ptr::PointsToRaw,
    ghost arena: vstd::prelude::int,
    ghost bound: bool,
    ghost shared: *const Shared,
}

impl FreshHeap {
    /// The heap the fields were stored in.
    pub(super) closed spec fn heap(&self) -> *mut Heap {
        self.heap
    }

    /// The permission to the heap's plain fields the token holds.
    pub(super) closed spec fn head(&self) -> vstd::raw_ptr::PointsToRaw {
        self.head
    }

    /// The home arena [`init_fields`] stored.
    pub(super) closed spec fn arena(&self) -> vstd::prelude::int {
        self.arena
    }

    /// The `bound` [`init_fields`] stored: `false`.
    pub(super) closed spec fn bound(&self) -> bool {
        self.bound
    }

    /// The `Shared` pointer [`init_fields`] stored in `shared`.
    pub(super) closed spec fn shared(&self) -> *const Shared {
        self.shared
    }
}

// The byte offsets of the fields before `caches` that `init_fields`
// stores one by one, and of [`HeadRest`], the ones after (the `const`
// assertion after this block): `heap.f` is at `heap + H_F`.
const H_SHARED: usize = 0;
const H_TID: usize = 8;
const H_ARENA: usize = 12;
const H_SLOW_TICKS: usize = 16;
const H_LAST_GC: usize = 24;
const H_REST: usize = 32;

/// The fields of a [`Heap`] from `uncached` up to `caches`, laid out as
/// in `Heap` from [`H_REST`] on (the `const` assertion after this
/// block): the value [`init_fields`] stores there, in one store, the
/// bytes `Heap` reads as its own fields. One store aligned as the
/// fields are in `Heap` (32 bytes at `uncached`), not one per field: the
/// stores merge into 16-byte ones, which are aligned instructions only
/// if the alignment is known.
#[repr(C, align(32))]
#[allow(dead_code, reason = "written as a whole, read back as `Heap`'s fields")]
struct HeadRest {
    uncached: ThreadStats,
    others: *mut ArenaCaches,
    snapshot: ThreadStats,
    tcache_enabled: bool,
    bound: bool,
    rng: u64,
    rehome: bool,
    rehome_from: u16,
}

global layout ThreadStats is size == 16, align == 8;

global layout HeadRest is size == 64, align == 32;

/// `*field_at::<Heap, T>(h, off) = v` for the field (or fields) of type
/// `T` at byte offset `off` of the fresh heap at `h` ([`init_fields`]):
/// the store goes through a permission carved from `raw`, the
/// permission to the heap's plain fields ([`heap_head`]), which is put
/// back, its value forgotten, so `raw` holds the same bytes after.
///
/// # Safety
/// `h` is a fresh metadata block for a `Heap`, exclusively owned.
#[inline(always)]
unsafe fn put_head<T>(h: *mut Heap, off: usize, Tracked(raw): Tracked<&mut vstd::raw_ptr::PointsToRaw>, v: T)
    requires
        heap_head(h, *old(raw)),
        crate::vspec::ptr::in_alloc(h as *mut u8, 128),
        h@.addr + 128 <= usize::MAX,
        off + vstd::layout::size_of::<T>() <= 128,
        (h@.addr + off) % (vstd::layout::align_of::<T>() as int) == 0,
    ensures
        heap_head(h, *final(raw)),
{
    broadcast use vstd::raw_ptr::group_raw_ptr_axioms;

    let ghost a = h@.addr + off;
    let ghost dom = raw.dom();
    let tracked mut all = vstd::raw_ptr::PointsToRaw::empty(h@.provenance);
    proof {
        tracked_swap(raw, &mut all);
        crate::vspec::ptr::axiom_ptr_add_addr(h as *mut u8, off);
    }
    let tracked (part, rest) = all.split(Set::range(a, a + vstd::layout::size_of::<T>()));
    let tracked mut f = part.into_typed::<T>(a as usize);
    // SAFETY: `off` is inside the heap's 128 bytes (the precondition).
    let p = unsafe { crate::arena::field_at::<Heap, T>(h, off) };
    proof {
        assert(f.ptr()@ == p@);
    }
    ptr_mut_write(p, Tracked(&mut f), v);
    proof {
        f.leak_contents();
        let tracked mut back = rest.join(f.into_raw());
        assert(back.dom() =~= dom);
        tracked_swap(raw, &mut back);
    }
}

/// The fields of a fresh heap before its home headers, stored field by
/// field up to `last_gc` and as one [`HeadRest`] after
/// ([`Heap::create_detached`]), each store through a permission carved
/// from `head.0`, the permission to the 128 bytes before `caches`
/// ([`heap_head`], [`put_head`]), which goes into the [`FreshHeap`]
/// returned with what was stored in `shared`, `arena` and `bound`: the
/// `shared` stored is the well-formed `&Shared` given, as a pointer, the
/// one at [`Shared::storage`] ([`shared_ptr`]). `head.1` is
/// the heap block's claim, which is dropped: `destroy` gets it back
/// with the whole block. It comes along so that the permission travels
/// with the same type as `Meta::alloc_raw`'s: every erased
/// `Tracked(..)` expression is a call to
/// `Tracked::<T>::assume_new_fallback`, and rustc's MIR inliner gives up
/// on a function from which more than 16 instances of it, by `T`, are
/// reachable (`mir_callgraph_cyclic`), which `heap_for_thread` is at.
///
/// # Safety
/// `h` is a fresh metadata block for a `Heap`, exclusively owned.
#[inline(always)]
pub(super) unsafe fn init_fields(
    h: *mut Heap,
    shared: &Shared,
    arena: u16,
    Tracked(head): Tracked<(vstd::raw_ptr::PointsToRaw, crate::meta::MetaClaim)>,
) -> (r: Tracked<FreshHeap>)
    requires
        crate::sys::may_syscall(),
        heap_head(h, head.0),
        shared.wf(),
        h@.addr + 128 <= usize::MAX,
    ensures
        r@.heap() == h,
        heap_head(h, r@.head()),
        r@.arena() == arena as int,
        !r@.bound(),
        r@.shared() == Shared::storage() as *const Shared,
{
    broadcast use vstd::layout::layout_of_primitives, vstd::layout::layout_of_references_and_pointers,
        vstd::layout::layout_of_references_and_pointers_for_sized_types;

    let tracked (mut raw, _) = head;
    let ghost base = h@.addr as int;
    proof {
        crate::vspec::ptr::axiom_raw_valid(&raw, h as *mut u8, 128);
        super::others::lemma_field_aligned::<*const Shared>(base, H_SHARED as int);
        super::others::lemma_field_aligned::<u32>(base, H_TID as int);
        super::others::lemma_field_aligned::<u16>(base, H_ARENA as int);
        super::others::lemma_field_aligned::<u32>(base, H_SLOW_TICKS as int);
        super::others::lemma_field_aligned::<u64>(base, H_LAST_GC as int);
        assert((base + H_REST as int) % 32 == 0);
    }
    let sp = shared_ptr(shared);
    // SAFETY: guaranteed by the caller.
    unsafe {
        put_head::<*const Shared>(h, H_SHARED, Tracked(&mut raw), sp);
        put_head::<u32>(h, H_TID, Tracked(&mut raw), tid());
        put_head::<u16>(h, H_ARENA, Tracked(&mut raw), arena);
        put_head::<u32>(h, H_SLOW_TICKS, Tracked(&mut raw), 0u32);
        put_head::<u64>(h, H_LAST_GC, Tracked(&mut raw), now_ns());
        put_head::<HeadRest>(
            h,
            H_REST,
            Tracked(&mut raw),
            HeadRest {
                uncached: ThreadStats { allocated: 0, deallocated: 0 },
                others: ptr::null_mut(),
                snapshot: ThreadStats { allocated: 0, deallocated: 0 },
                tcache_enabled: true,
                bound: false,
                rng: 0,
                rehome: false,
                rehome_from: FOREIGN,
            },
        );
    }
    Tracked(FreshHeap { heap: h, head: raw, arena: arena as int, bound: false, shared: sp })
}

} // verus!

// The offsets `init_fields` stores at (`H_*`, `HeadRest`); `FIELD_TYPES`
// below checks the types.
const _: () = assert!(
    core::mem::offset_of!(Heap, shared) == H_SHARED
        && core::mem::offset_of!(Heap, tid) == H_TID
        && core::mem::offset_of!(Heap, arena) == H_ARENA
        && core::mem::offset_of!(Heap, slow_ticks) == H_SLOW_TICKS
        && core::mem::offset_of!(Heap, last_gc) == H_LAST_GC
        && core::mem::offset_of!(Heap, uncached)
            == H_REST + core::mem::offset_of!(HeadRest, uncached)
        && core::mem::offset_of!(Heap, others) == H_REST + core::mem::offset_of!(HeadRest, others)
        && core::mem::offset_of!(Heap, snapshot)
            == H_REST + core::mem::offset_of!(HeadRest, snapshot)
        && core::mem::offset_of!(Heap, tcache_enabled)
            == H_REST + core::mem::offset_of!(HeadRest, tcache_enabled)
        && core::mem::offset_of!(Heap, bound) == H_REST + core::mem::offset_of!(HeadRest, bound)
        && core::mem::offset_of!(Heap, rng) == H_REST + core::mem::offset_of!(HeadRest, rng)
        && core::mem::offset_of!(Heap, rehome) == H_REST + core::mem::offset_of!(HeadRest, rehome)
        && core::mem::offset_of!(Heap, rehome_from)
            == H_REST + core::mem::offset_of!(HeadRest, rehome_from)
        && H_REST + core::mem::size_of::<HeadRest>() <= core::mem::offset_of!(Heap, caches)
);

// The types `init_fields` stores at those offsets are `Heap`'s fields'
// (`put_head::<T>` with these `T`, and `HeadRest`'s fields).
#[allow(
    dead_code,
    clippy::type_complexity,
    reason = "a compile-time check of the field types"
)]
const FIELD_TYPES: fn(&Heap) -> (*const Shared, u32, u16, u32, u64, HeadRest) = |h| {
    (
        h.shared,
        h.tid,
        h.arena,
        h.slow_ticks,
        h.last_gc,
        HeadRest {
            uncached: h.uncached,
            others: h.others,
            snapshot: h.snapshot,
            tcache_enabled: h.tcache_enabled,
            bound: h.bound,
            rng: h.rng,
            rehome: h.rehome,
            rehome_from: h.rehome_from,
        },
    )
};
