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
/// and what the field writers stored in them: the `Shared` and the home
/// arena ([`init_fields`]), and `bound`, which `init_fields` stores as
/// `false` and [`mark_bound`] sets. Linear and not
/// `Copy` (it holds the permission): the one holder of the heap-head
/// permission, made by [`init_fields`] once per heap. That `shared`,
/// `arena` and `bound` are what the heap's fields hold is not a fact
/// Verus checks (the permission forgets the values): `init_fields` sets
/// them to what it stores, and nothing else builds one (its fields are
/// private to this module), takes one apart but [`FreshHeap::into_block`]
/// (verified, it returns exactly `head()` and `claim()`), or changes what
/// it records but `mark_bound`, which records its store of `bound` in it. [`mark_rehome`]
/// stores `rehome`, a field the token does not record, through the
/// permission it holds, and keeps what it records. It also holds the
/// heap block's metadata claim ([`FreshHeap::claim`]), which
/// [`init_fields`] is handed with the permission. The heap-head
/// permission and the claim go into the heap's ghost state
/// (`Heap::head`, [`FreshHeap::into_block`] at `tcaches::heap_new`),
/// from which `Heap::destroy` frees the block. `Tcaches::create` drops
/// the token instead, as it drops the cache permissions creation returns.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub(super) tracked struct FreshHeap {
    ghost heap: *mut Heap,
    tracked head: vstd::raw_ptr::PointsToRaw,
    tracked claim: crate::meta::MetaClaim,
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

    /// `bound`: `false` as [`init_fields`] stored it, `true` once
    /// [`mark_bound`] has recorded its store.
    pub(super) closed spec fn bound(&self) -> bool {
        self.bound
    }

    /// The `Shared` pointer [`init_fields`] stored in `shared`.
    pub(super) closed spec fn shared(&self) -> *const Shared {
        self.shared
    }

    /// The heap block's metadata claim the token holds.
    pub(super) closed spec fn claim(&self) -> crate::meta::MetaClaim {
        self.claim
    }

    /// The permission to the heap's plain fields and the block's claim,
    /// out of the token, which is used up.
    pub(super) proof fn into_block(tracked self) -> (tracked r: (vstd::raw_ptr::PointsToRaw, crate::meta::MetaClaim))
        ensures
            r.0 == self.head(),
            r.1 == self.claim(),
    {
        (self.head, self.claim)
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
// `bound` and `rehome`, inside `HeadRest`, which [`mark_bound`] and
// [`mark_rehome`] store alone.
const H_BOUND: usize = 73;
const H_REHOME: usize = 88;

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
/// `T` at byte offset `off` of the fresh heap at `h` ([`init_fields`], [`mark_bound`],
/// [`mark_rehome`]):
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
/// the heap block's claim, which goes into the [`FreshHeap`] too. The two
/// travel as one pair, of the type `Meta::alloc_raw` returns: every erased
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
    ensures
        r@.heap() == h,
        heap_head(h, r@.head()),
        r@.arena() == arena as int,
        !r@.bound(),
        r@.shared() == Shared::storage() as *const Shared,
        r@.claim() == head.1,
{
    broadcast use vstd::layout::layout_of_primitives, vstd::layout::layout_of_references_and_pointers,
        vstd::layout::layout_of_references_and_pointers_for_sized_types;

    let tracked (mut raw, claim) = head;
    let ghost base = h@.addr as int;
    proof {
        crate::vspec::ptr::axiom_raw_readable(&raw, h as *mut u8, 128);
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
    Tracked(FreshHeap { heap: h, head: raw, claim, arena: arena as int, bound: false, shared: sp })
}

/// `(*h).bound = true` for a heap [`Heap::create`] has just made: a
/// store of `true` to `bound` ([`H_BOUND`]) through a permission carved
/// from the token's ([`put_head`]), recorded in the token.
///
/// # Safety
/// `h` is a fresh heap, exclusively owned.
#[inline(always)]
pub(super) unsafe fn mark_bound(h: *mut Heap, Tracked(s): Tracked<&mut FreshHeap>)
    requires
        old(s).heap() == h,
        heap_head(h, old(s).head()),
    ensures
        final(s).heap() == h,
        heap_head(h, final(s).head()),
        final(s).arena() == old(s).arena(),
        final(s).bound(),
        final(s).shared() == old(s).shared(),
        final(s).claim() == old(s).claim(),
{
    broadcast use vstd::layout::layout_of_primitives;

    proof {
        crate::vspec::ptr::axiom_raw_readable(&s.head, h as *mut u8, 128);
        super::others::lemma_field_aligned::<bool>(h@.addr as int, H_BOUND as int);
    }
    // SAFETY: guaranteed by the caller.
    unsafe { put_head::<bool>(h, H_BOUND, Tracked(&mut s.head), true) };
    proof {
        s.bound = true;
    }
}

/// `(*h).rehome = true` for a thread heap [`Heap::create`] has just made
/// and marked bound ([`mark_bound`]): it may still move its home
/// ([`Heap::try_rehome`]). A store of `true` to `rehome` ([`H_REHOME`])
/// through a permission carved from the token's ([`put_head`]); the
/// token does not record `rehome`, and what it records stays as it was.
/// A heap with `rehome` set is bound (a conjunct of
/// [`Heap::homed_ok`]): the token must record [`mark_bound`]'s store
/// first.
///
/// # Safety
/// `h` is a fresh heap, exclusively owned.
#[inline(always)]
pub(super) unsafe fn mark_rehome(h: *mut Heap, Tracked(s): Tracked<&mut FreshHeap>)
    requires
        old(s).heap() == h,
        heap_head(h, old(s).head()),
        old(s).bound(),
    ensures
        final(s).heap() == h,
        heap_head(h, final(s).head()),
        final(s).arena() == old(s).arena(),
        final(s).bound(),
        final(s).shared() == old(s).shared(),
        final(s).claim() == old(s).claim(),
{
    broadcast use vstd::layout::layout_of_primitives;

    proof {
        crate::vspec::ptr::axiom_raw_readable(&s.head, h as *mut u8, 128);
        super::others::lemma_field_aligned::<bool>(h@.addr as int, H_REHOME as int);
    }
    // SAFETY: guaranteed by the caller.
    unsafe { put_head::<bool>(h, H_REHOME, Tracked(&mut s.head), true) };
}

} // verus!

// The offsets `init_fields`, `mark_bound` and `mark_rehome` store at
// (`H_*`, `HeadRest`); `FIELD_TYPES` below checks the types.
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
        && core::mem::offset_of!(Heap, bound) == H_BOUND
        && core::mem::offset_of!(Heap, rehome) == H_REHOME
        && H_REST + core::mem::size_of::<HeadRest>() <= core::mem::offset_of!(Heap, caches)
);

// The types `init_fields`, `mark_bound` and `mark_rehome` store at those
// offsets are `Heap`'s fields' (`put_head::<T>` with these `T`, and
// `HeadRest`'s fields).
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

// `Heap` stays `!Unpin` (its `_pin` field), so a `&mut Heap` is not
// `noalias`. A path names an inherent item when its bounds hold, so
// `IsUnpin::<T>::VALUE` is the inherent `true` when `T: Unpin` and the
// trait's `false` otherwise.
struct IsUnpin<T>(core::marker::PhantomData<T>);

trait NotUnpin {
    const VALUE: bool = false;
}

impl<T> NotUnpin for IsUnpin<T> {}

impl<T: Unpin> IsUnpin<T> {
    #[allow(dead_code, reason = "named only when `Heap` is `Unpin`")]
    const VALUE: bool = true;
}

const _: () = assert!(!IsUnpin::<Heap>::VALUE);

/// Kani check of the exec half of `heap::thread::HeapPerm::assemble`
/// (`verus/ledger/src/heap/thread.rs.md`; `cfg(kani)` only, see
/// `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_assemble {
    use super::{Heap, Shared, init_fields, mark_bound, mark_rehome};
    use core::mem::{MaybeUninit, size_of};
    use vstd::prelude::Tracked;

    /// The `Shared` the harness hands `init_fields`, all zero. A static's
    /// value must be valid for its type, which const evaluation checks, so
    /// this builds only while every field of `Shared` is valid all-zero.
    // SAFETY: const evaluation rejects an all-zero `Shared` that is not a
    // valid value.
    static ZERO_SHARED: Shared = unsafe { core::mem::zeroed() };

    /// `tid` for the harness: any thread id.
    #[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
    fn any_tid() -> u32 {
        kani::any()
    }

    /// `now_ns` for the harness: any time.
    #[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
    fn any_ns() -> u64 {
        kani::any()
    }

    /// `HeapPerm::assemble` at its one call, `tcaches::heap_new`, whose one
    /// caller, `heap::thread::create_heap_for_thread`, gets the token from
    /// `Heap::create`: `Heap::create_detached` runs `init_fields`, then
    /// `Heap::create` runs `mark_bound` and `mark_rehome`. The axiom
    /// states that the heap at `s.heap()` has a null `others` and the
    /// `arena`, `bound` and `shared` the token records, which are what
    /// those writers stored: the `arena` given, `bound` `true`, and
    /// `shared` the pointer `api::shared_ptr` gives (`Shared::storage()`).
    /// The writers are called only there and here, and `heap_new` only
    /// there (`crate::tests::assembled_writes_only_at_checked_sites`). The
    /// harness runs the three writers in that order, the only one it
    /// checks, for any `arena`, on storage of a `Heap`'s size and
    /// alignment holding arbitrary bytes, so a field the writers do not
    /// store keeps them. It hands `init_fields` a `Shared` of its own
    /// (`ZERO_SHARED`), not the process's: outside `cfg(test)`
    /// `heap::paths::shared_ptr` reads nothing of the one it is given and
    /// returns `api::shared_ptr()`, so the stored `shared` must still be
    /// that pointer. It reads those four fields of the `Heap` there,
    /// each by its own place. Kani checks that every store is inside the
    /// storage. `tid` and `now_ns`, system calls whose values the axiom
    /// does not state, return any value. Not checked: the join itself
    /// (that the token's raw permission to the head and `perms`' home
    /// headers stand for a permission to the whole `Heap`), a premise of
    /// Verus's own model; that nothing else writes those fields between
    /// the writers and the axiom, which the token's linear permission gives
    /// verified code; the heap's other fields and its ghost fields, which
    /// the axiom claims nothing of; and the provenance of `shared`, which
    /// Kani does not observe (the harness compares addresses).
    #[kani::proof]
    #[kani::stub(crate::heap::shared::tid, any_tid)]
    #[kani::stub(crate::heap::shared::now_ns, any_ns)]
    fn writers_store_the_assembled_fields() {
        let mut slot = MaybeUninit::<Heap>::uninit();
        let h = slot.as_mut_ptr();
        // SAFETY: `h` is the slot's storage, of `size_of::<Heap>()` bytes,
        // and a byte array has alignment 1.
        unsafe { h.cast::<[u8; size_of::<Heap>()]>().write(kani::any()) };
        let arena: u16 = kani::any();
        let sp = crate::api::shared_ptr();
        // SAFETY: the slot is a fresh, exclusively owned block of a
        // `Heap`'s size and alignment. `ZERO_SHARED` need not be `wf()`:
        // outside `cfg(test)` `heap::paths::shared_ptr` reads none of it.
        let _s = unsafe { init_fields(h, &ZERO_SHARED, arena, Tracked::assume_new()) };
        // SAFETY: as above.
        unsafe { mark_bound(h, Tracked::assume_new()) };
        // SAFETY: as above; `bound` is stored.
        unsafe { mark_rehome(h, Tracked::assume_new()) };
        // SAFETY: each a field of the slot, read by its place: the
        // writers stored all four (`bound` as a `bool`), so each holds a
        // value of its type, whatever the arbitrary bytes were.
        let (others, at, b, s) = unsafe { ((*h).others, (*h).arena, (*h).bound, (*h).shared) };
        assert!(others.is_null());
        assert_eq!(at, arena);
        assert!(b);
        assert!(s == sp);
    }
}
