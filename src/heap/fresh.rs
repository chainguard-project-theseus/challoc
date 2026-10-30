//! A fresh heap's plain fields, the 128 bytes before its home headers:
//! [`init_fields`] stores them and hands on the token for them, a
//! [`FreshHeap`].

use core::ptr;

use super::others::ArenaCaches;
use super::{FOREIGN, Heap, Shared, ThreadStats, now_ns, shared_ptr, tid};
use vstd::prelude::{Tracked, verus};
use vstd::raw_ptr::ptr_mut_write;

verus! {

/// Ghost: what [`Heap::create_detached`](super::Heap::create_detached)
/// stored in a fresh heap's plain fields, and the permission to them:
/// the heap, the permission to its head (`head`), the home arena, whether
/// [`mark_bound`](super::mark_bound) set its `bound` flag, and its
/// `Shared`.
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub(super) tracked struct FreshHeap {
    ghost heap: *mut Heap,
    tracked head: vstd::raw_ptr::PointsToRaw,
    ghost arena: vstd::prelude::int,
    ghost bound: bool,
    ghost shared: *const Shared,
}

} // verus!

// The byte offsets of the fields before `caches` that `init_fields`
// stores one by one, and of [`HeadRest`], the ones after (the `const`
// assertion below): `heap.f` is at `heap + H_F`.
const H_SHARED: usize = 0;
const H_TID: usize = 8;
const H_ARENA: usize = 12;
const H_SLOW_TICKS: usize = 16;
const H_LAST_GC: usize = 24;
const H_REST: usize = 32;

/// The fields of a [`Heap`] from `uncached` up to `caches`, laid out as
/// in `Heap` from [`H_REST`] on (the `const` assertion below): the value
/// [`init_fields`] stores there, in one store, the bytes `Heap` reads as
/// its own fields. One store aligned as the fields are in `Heap` (32
/// bytes at `uncached`), not one per field: the stores merge into
/// 16-byte ones, which are aligned instructions only if the alignment is
/// known.
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

/// `*field_at::<Heap, T>(h, off) = v` for the field (or fields) of type
/// `T` at byte offset `off` of the fresh heap at `h` ([`init_fields`]),
/// through the permission to the heap's plain fields.
///
/// # Safety
/// `h` is a fresh metadata block for a `Heap`, exclusively owned, and a
/// `T` at `off` is one of its fields before `caches`.
#[inline(always)]
unsafe fn put_head<T>(
    h: *mut Heap,
    off: usize,
    _raw: Tracked<&mut vstd::raw_ptr::PointsToRaw>,
    v: T,
) {
    // SAFETY: `off` is inside the heap's 128 bytes (the caller).
    let p = unsafe { crate::arena::field_at::<Heap, T>(h, off) };
    ptr_mut_write(p, Tracked::assume_new(), v);
}

/// The fields of a fresh heap before its home headers, stored field by
/// field up to `last_gc` and as one [`HeadRest`] after
/// ([`Heap::create_detached`]), through `head.0`, the permission to the
/// 128 bytes before `caches`, which goes into the [`FreshHeap`]
/// returned. `head.1` is the heap block's claim, which is dropped. It
/// comes along so that the permission travels with the same type as
/// `Meta::alloc_raw`'s: every erased `Tracked(..)` expression is a call
/// to `Tracked::<T>::assume_new_fallback`, and rustc's MIR inliner gives
/// up on a function from which more than 16 instances of it, by `T`, are
/// reachable (`mir_callgraph_cyclic`), which `heap_for_thread` is at.
///
/// # Safety
/// `h` is a fresh metadata block for a `Heap`, exclusively owned.
#[inline(always)]
pub(super) unsafe fn init_fields(
    h: *mut Heap,
    shared: &Shared,
    arena: u16,
    _head: Tracked<(vstd::raw_ptr::PointsToRaw, crate::meta::MetaClaim)>,
) -> Tracked<FreshHeap> {
    let sp = shared_ptr(shared);
    // SAFETY: guaranteed by the caller.
    unsafe {
        put_head::<*const Shared>(h, H_SHARED, Tracked::assume_new(), sp);
        put_head::<u32>(h, H_TID, Tracked::assume_new(), tid());
        put_head::<u16>(h, H_ARENA, Tracked::assume_new(), arena);
        put_head::<u32>(h, H_SLOW_TICKS, Tracked::assume_new(), 0u32);
        put_head::<u64>(h, H_LAST_GC, Tracked::assume_new(), now_ns());
        put_head::<HeadRest>(
            h,
            H_REST,
            Tracked::assume_new(),
            HeadRest {
                uncached: ThreadStats {
                    allocated: 0,
                    deallocated: 0,
                },
                others: ptr::null_mut(),
                snapshot: ThreadStats {
                    allocated: 0,
                    deallocated: 0,
                },
                tcache_enabled: true,
                bound: false,
                rng: 0,
                rehome: false,
                rehome_from: FOREIGN,
            },
        );
    }
    Tracked::assume_new()
}

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
