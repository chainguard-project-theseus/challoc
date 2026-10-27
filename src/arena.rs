//! Arenas (design.md section 13.3). Explicit caches are
//! `heap/tcaches.rs`; the decoding of `MALLOCX_*` flags and their routing
//! onto heaps, caches and the direct paths (13.2) is `heap/mallocx.rs`.
//!
//! An [`Arena`] is one [`ClassState`] per size class over the shared class
//! regions (every arena's states for class `c` point at the same
//! `SlabTable`, so slabs of different arenas interleave in the region and
//! each descriptor records its owner) plus its own decay period. Arena 0 is
//! built inside `Shared` at init; the [`Arenas`] registry hands out the
//! others. Indices below `narenas` (the `malloc_conf` key; the default
//! resolves to `min(4 * ncpus, 64)`, `Config::effective_narenas`) are
//! *automatic*: a new thread heap is bound to the one with the fewest
//! bound heaps ([`Arenas::assign`], jemalloc's `arena_choose_hard`), the
//! binding is dropped when the heap is destroyed, and each arena is
//! materialised the first time something touches it. So a program with
//! `t` threads alive at once uses `min(t, narenas)` arenas, a
//! single-threaded program never leaves arena 0, and thread churn reuses
//! the arena an exited thread left instead of walking on to a fresh one
//! (round robin materialised every arena under churn and left a partial
//! slab per class in each: larson1 67 MiB against 13.6).
//! Indices from
//! `narenas` up are created explicitly by `arenas.create`. Arenas are
//! never destroyed (`arena.<i>.destroy` purges and retires one); they live
//! in the metadata region.
//!
//! Memory never migrates between arenas: a slot is claimed from and
//! released to the state of the arena that committed its slab, and a
//! thread's heap keeps one cache per `(arena, class)` it has touched
//! (`heap/`). Once a second arena exists `Shared::multi_arena` is set and
//! the `free` fast path reads the slab group's word in the class's owner
//! array (`SlabTable`, never the descriptor) to pick the cache; with one
//! arena that load is skipped, since every slab and every heap is arena 0.

use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};

use crate::class::{ClassState, SetTable, SlabTable};
use crate::heap::Shared;
#[cfg(verus_keep_ghost)]
use crate::layout::process_regions;
use crate::layout::{NCLASSES, Regions};
use crate::lock::{Lock, LockOrder};
#[cfg(verus_keep_ghost)]
use crate::lock::{rank_arena_stats, rank_meta, rank_sets};
use crate::meta::{Meta, MetaClaim};
use crate::purge::Left;
use crate::stats::ArenaStats;
#[cfg(verus_keep_ghost)]
use crate::vspec::arena::axiom_arena_stats_lock_inv;
#[cfg(verus_keep_ghost)]
use crate::vspec::ptr::{axiom_ptr_add_addr, axiom_raw_valid, in_alloc, ptr_add};
#[cfg(verus_keep_ghost)]
use crate::vspec::zero::ZeroRaw;
#[cfg(verus_keep_ghost)]
use vstd::layout::{align_of, size_of};
use vstd::prelude::*;
use vstd::raw_ptr::{PointsTo, PointsToRaw, ptr_mut_write, ptr_ref};
use vstd::resource::Loc;
use vstd::resource::map::GhostPersistentPointsTo;

verus! {

/// Persistent knowledge of one published arena: its index and the
/// locations of its class states' owned-slab records, by class (the
/// entry the arena registry publishes for it). What a class state that
/// takes groups holds (`SlabTable::take_group`'s `pubw`).
pub type Published = GhostPersistentPointsTo<int, Seq<Loc>>;

/// The location of the process registry's record of published arenas,
/// which a [`Published`] of the process is a point of. A placeholder:
/// uninterpreted, so it asserts nothing, and the class proofs use it only
/// in equalities (two records of the process's registry are at one
/// location). Later arena proofs are to define it as the registry's
/// location.
pub uninterp spec fn process_pub_loc() -> Loc;

} // verus!

/// Arena indices are the 12-bit `MALLOCX_ARENA` field; 4096 itself is
/// `MALLCTL_ARENAS_ALL`.
pub const MAX_ARENAS: usize = 4096;
/// `MALLCTL_ARENAS_ALL`.
pub const ARENAS_ALL: u32 = 4096;

verus! {

/// One arena (design 13.3): a class state per size class and a decay
/// period. Built in place because the states borrow the tables.
#[repr(C)]
pub struct Arena {
    index: u16,
    /// Decay period (`arena.<i>.dirty_decay_ms`) in nanoseconds;
    /// `purge::NEVER` for `-1`.
    decay_ns: AtomicU64,
    /// The slab sets of every class, one interleaved table per slab size
    /// (`class/`), a separate metadata object so that `Arena` keeps its
    /// size and the offsets of everything allocated after it.
    sets: *const SetTable,
    /// Ghost: the permission to the table at `sets`, frozen by `init`
    /// (the class states' sets share it).
    #[allow(dead_code)]
    sets_perm: Tracked<&'static PointsTo<SetTable>>,
    pub classes: [ClassState; NCLASSES],
    /// Live huge runs allocated through this arena and their pages (guard
    /// included), kept by `Shared::alloc_huge` / `free_huge` since the huge
    /// allocator itself is shared by every arena (`stats.arenas.<i>.large`).
    /// One word, runs in the high half and pages in the low
    /// ([`Arena::LARGE_RUN`]), so a reader's `(runs, pages)` is always a
    /// pair one run's add or subtract produced together and `pages >=
    /// runs * GUARD_PAGES` holds for every snapshot (`stats.rs` subtracts
    /// the guards exactly).
    large: AtomicU64,
    /// This arena's statistics as of the last `epoch` (`stats.rs`).
    stats: Lock<ArenaStats>,
    /// When the last exit purge ran ([`Arena::take_exit_purge`]), 0 before
    /// the first.
    last_exit_purge: AtomicU64,
}

} // verus!

// SAFETY: `sets` points at a never-freed metadata object whose only
// mutable state is behind its own lock (`SetTable::tables`); everything
// else in `Arena` is atomics, locks and `ClassState`s, which are `Sync`
// and `Send`. Sharing or moving an `Arena` between threads therefore
// hands out nothing but that pointer to a process-lifetime, locked table.
unsafe impl Sync for Arena {}
unsafe impl Send for Arena {}

/// Makes the next [`Arena::init`] over the metadata allocator at this
/// address fail before it allocates anything, as an exhausted region
/// would, for tests.
#[cfg(test)]
pub static FAIL_INIT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

verus! {

// Field and element pointers for the in-place builds (`Arena::init`,
// `Arenas::init`, `Shared::init`), and the field permissions of the one
// that is verified, `Arena::init`. Verus has no raw-pointer field
// projection (`&raw mut (*p).f`), so a field's pointer is the struct's
// pointer advanced by the field's byte offset (`offset_of!`, which
// compiles to the same address), and a field's permission is carved from
// the struct's: [`take_field`] splits the bytes at the offsets a layout
// axiom states (checked by the build: see `fits`), [`split_array`] splits
// an array into its elements. Putting the written values back together
// into the struct's value has no counterpart in vstd and is an axiom
// ([`axiom_arena_assemble`]).

/// `p`'s address plus `off` bytes as a `*mut T`, with `p`'s provenance:
/// the pointer `&raw mut (*p).f` gives for a field `f` of type `T` at
/// byte offset `off`.
pub open spec fn field_ptr<S, T>(p: *mut S, off: usize) -> *mut T {
    ptr_add(p as *mut u8, off) as *mut T
}

/// A field pointer cast to another type is that type's field pointer.
pub(crate) proof fn lemma_field_cast<S, A, B>(p: *mut S, off: usize)
    ensures
        field_ptr::<S, A>(p, off) as *mut B == field_ptr::<S, B>(p, off),
{
    broadcast use vstd::raw_ptr::group_raw_ptr_axioms;

    assert((field_ptr::<S, A>(p, off) as *mut B)@ == field_ptr::<S, B>(p, off)@);
}

/// [`field_ptr`]: `p` as bytes, advanced by `off`, as a `*mut T`, which
/// compiles to the address of the field projection.
///
/// # Safety
/// `off` is inside the allocation `p` points into (the precondition). A
/// caller that writes through the result also needs a `T` at `off` to lie
/// inside that allocation, aligned for `T`: the `fits` assertions below
/// check it for every field offset this file uses.
#[inline(always)]
pub(crate) unsafe fn field_at<S, T>(p: *mut S, off: usize) -> (r: *mut T)
    requires
        off <= isize::MAX,
        in_alloc(p as *mut u8, off as int),
    ensures
        r == field_ptr::<S, T>(p, off),
{
    broadcast use vstd::layout::layout_of_primitives;

    // SAFETY: `off` bytes from `p` stay inside its allocation.
    unsafe { (p as *mut u8).add(off) as *mut T }
}

/// Element `i` of the `n` from `base`: `base.add(i)`, the address of the
/// array index projection.
///
/// # Safety
/// The `n` elements are inside one allocation and `i < n` (the
/// precondition).
// `n` is read only by the precondition.
#[allow(unused_variables)]
#[inline(always)]
pub(crate) unsafe fn elem_at<T>(base: *mut T, i: usize, n: usize) -> (r: *mut T)
    requires
        i < n,
        n * size_of::<T>() <= isize::MAX,
        in_alloc(base, n * size_of::<T>()),
    ensures
        r == ptr_add(base, i),
{
    proof {
        vstd::arithmetic::mul::lemma_mul_inequality(i as int, n as int, size_of::<T>() as int);
    }
    // SAFETY: element `i < n` is inside the allocation.
    unsafe { base.add(i) }
}

/// A field of type `T` at byte offset `off` of an `S`: aligned for `T`
/// (in an `S` aligned for `S`), and its bytes end by `next` (the next
/// field's offset, or the size of `S`). `fits` is the same test for the
/// build.
pub open spec fn field_fits<S, T>(off: usize, next: int) -> bool {
    &&& off as int % align_of::<T>() as int == 0
    &&& align_of::<S>() as int % align_of::<T>() as int == 0
    &&& off + size_of::<T>() <= next
}

/// The permission to storage under construction while it is held:
/// [`Arena::init`] takes it out and hands it back built. A struct rather
/// than a bare `Option` so that Verus treats a `&mut` to it as ghost.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct Held<T> {
    pub tracked perm: Option<PointsTo<T>>,
}

/// The storage of an `S` at `base`: non-null, aligned, inside one
/// allocation and clear of the end of the address space.
pub open spec fn storage_ok<S>(base: *mut S) -> bool {
    &&& base@.addr != 0
    &&& base@.addr as int % align_of::<S>() as int == 0
    &&& base@.addr + size_of::<S>() <= usize::MAX
    &&& size_of::<S>() <= isize::MAX
    &&& in_alloc(base as *mut u8, size_of::<S>() as int)
}

/// The permission to the field of type `T` at byte offset `off` of the
/// `S` at `base`, taken from `raw`, which holds the bytes of the `S` from
/// that field on; what is left holds them from `next` on, and every byte
/// of `raw` but the field's.
pub(crate) proof fn take_field<S, T>(tracked raw: PointsToRaw, base: *mut S, off: usize, next: int) -> (tracked r: (
    PointsTo<T>,
    PointsToRaw,
))
    requires
        field_fits::<S, T>(off, next),
        next <= size_of::<S>(),
        storage_ok(base),
        raw.contains_range(base@.addr + off, size_of::<S>() - off),
        raw.provenance() == base@.provenance,
    ensures
        r.0.ptr() == field_ptr::<S, T>(base, off),
        r.0.ptr()@.addr == base@.addr + off,
        r.0.is_uninit(),
        r.1.contains_range(base@.addr + next, size_of::<S>() - next),
        r.1.dom() == raw.dom().difference(Set::range(base@.addr + off, base@.addr + off + size_of::<T>())),
        r.1.provenance() == raw.provenance(),
{
    broadcast use vstd::layout::layout_of_primitives, vstd::layout::align_nonzero, vstd::raw_ptr::group_raw_ptr_axioms;

    let start = base@.addr + off;
    let tracked (taken, rest) = raw.split(Set::range(start, start + size_of::<T>()));
    crate::layout::lemma_aligned_down(base@.addr as int, align_of::<S>() as int, align_of::<T>() as int);
    crate::layout::lemma_mod_lin3(base@.addr as int, 1, off as int, 0, 0, align_of::<T>() as int);
    axiom_ptr_add_addr(base as *mut u8, off);
    let tracked f = taken.into_typed::<T>(start as usize);
    assert(f.ptr()@ == field_ptr::<S, T>(base, off)@);
    (f, rest)
}

/// An array of `N` `T`s is `N` `T`s back to back, aligned as one: the
/// size and alignment the Rust reference gives arrays. Trusted (vstd
/// states no array layout), and checked by the build at each instance
/// used (the `const` assertions after `Arena`'s layout). Only for an
/// array whose elements fit in an `isize`: a longer one has no layout
/// (rustc rejects it as too big), yet Verus still names its `size_of`,
/// which vstd bounds by `isize::MAX` (`layout_for_type_is_valid`), so
/// without the `requires` the axiom at `[u64; usize::MAX]` would prove
/// `false`.
pub(crate) axiom fn axiom_array_layout<T, const N: usize>()
    requires
        N * size_of::<T>() <= isize::MAX,
    ensures
        size_of::<[T; N]>() == N * size_of::<T>(),
        align_of::<[T; N]>() == align_of::<T>(),
;

/// `m` holds a permission for each element `lo..hi` of the array of `T`s
/// from `base`, at its address.
pub open spec fn elems_at<T>(m: Map<int, PointsTo<T>>, base: *mut T, lo: int, hi: int) -> bool {
    forall|i: int| #![trigger m.contains_key(i)] #![trigger m[i]] lo <= i < hi ==> m.contains_key(i) && m[i].ptr() == ptr_add(base, i as usize)
}

/// Every one of them is initialised.
pub open spec fn elems_init<T>(m: Map<int, PointsTo<T>>, lo: int, hi: int) -> bool {
    forall|i: int| lo <= i < hi ==> (#[trigger] m[i]).is_init()
}

/// Every one of them is uninitialised.
pub open spec fn elems_uninit<T>(m: Map<int, PointsTo<T>>, lo: int, hi: int) -> bool {
    forall|i: int| lo <= i < hi ==> (#[trigger] m[i]).is_uninit()
}

/// The permissions of elements `i..n` from `base`, out of `raw`, which
/// holds their bytes.
proof fn split_elems<T>(tracked raw: PointsToRaw, base: *mut T, i: nat, n: nat) -> (tracked m: Map<int, PointsTo<T>>)
    requires
        i <= n,
        n <= usize::MAX,
        raw.contains_range(base@.addr + i * size_of::<T>(), (n - i) * size_of::<T>()),
        raw.provenance() == base@.provenance,
        base@.addr != 0,
        base@.addr as int % align_of::<T>() as int == 0,
        base@.addr + n * size_of::<T>() <= usize::MAX,
        in_alloc(base, n as int * size_of::<T>()),
    ensures
        elems_at(m, base, i as int, n as int),
        elems_uninit(m, i as int, n as int),
    decreases n - i,
{
    broadcast use vstd::layout::align_properties, vstd::layout::align_nonzero, vstd::raw_ptr::group_raw_ptr_axioms;

    if i == n {
        Map::tracked_empty()
    } else {
        let sz = size_of::<T>() as int;
        let start = base@.addr + i * sz;
        assert(i * sz + sz == (i + 1) * sz) by (nonlinear_arith);
        assert((n - i) * sz == sz + (n - (i + 1)) * sz) by (nonlinear_arith);
        assert(0 <= i * sz <= n * sz) by (nonlinear_arith)
            requires
                i <= n,
                sz >= 0,
        ;
        let tracked (taken, rest) = raw.split(Set::range(start, start + sz));
        crate::layout::lemma_mod_lin3(base@.addr as int, i as int, sz, 0, 0, align_of::<T>() as int);
        axiom_ptr_add_addr(base, i as usize);
        let tracked e = taken.into_typed::<T>(start as usize);
        assert(e.ptr()@ == ptr_add(base, i as usize)@);
        let tracked mut m = split_elems(rest, base, i + 1, n);
        let ghost m0 = m;
        m.tracked_insert(i as int, e);
        assert forall|j: int| #![trigger m.contains_key(j)] #![trigger m[j]] i <= j < n implies m.contains_key(j) && m[j].ptr() == ptr_add(base, j as usize)
            && m[j].is_uninit() by {
            if j != i {
                assert(m0.contains_key(j));
            }
        }
        m
    }
}

/// The permissions of the elements of the uninitialised array `a`.
pub(crate) proof fn split_array<T, const N: usize>(tracked a: PointsTo<[T; N]>) -> (tracked m: Map<int, PointsTo<T>>)
    requires
        a.is_uninit(),
        a.ptr()@.addr + size_of::<[T; N]>() <= usize::MAX,
        0 < N * size_of::<T>() <= isize::MAX,
    ensures
        elems_at(m, a.ptr() as *mut T, 0, N as int),
        elems_uninit(m, 0, N as int),
        in_alloc(a.ptr() as *mut T, N as int * size_of::<T>()),
        N * size_of::<T>() <= isize::MAX,
        a.ptr()@.addr + N * size_of::<T>() <= usize::MAX,
{
    axiom_array_layout::<T, N>();
    a.is_nonnull();
    a.is_aligned();
    let base = a.ptr() as *mut T;
    let tracked raw = a.into_raw();
    axiom_raw_valid(&raw, a.ptr() as *mut u8, N as int * size_of::<T>());
    assert(0 * size_of::<T>() == 0) by (nonlinear_arith);
    split_elems(raw, base, 0, N as nat)
}

/// The bytes of the element permissions `lo..hi` of `m`, the array of
/// `T`s from `base`, whatever they hold (each value is given up), as one
/// permission: the inverse of [`split_elems`].
proof fn join_elems<T>(tracked m: Map<int, PointsTo<T>>, base: *mut T, lo: int, hi: int) -> (tracked r: PointsToRaw)
    requires
        0 <= lo <= hi <= usize::MAX,
        elems_at(m, base, lo, hi),
        in_alloc(base, hi * size_of::<T>()),
        base@.addr + hi * size_of::<T>() <= usize::MAX,
    ensures
        r.provenance() == base@.provenance,
        forall|x: int|
            base@.addr + lo * size_of::<T>() <= x < base@.addr + hi * size_of::<T>() ==> #[trigger] r.dom().contains(x),
    decreases hi - lo,
{
    broadcast use vstd::raw_ptr::group_raw_ptr_axioms;

    let sz = size_of::<T>() as int;
    if lo == hi {
        PointsToRaw::empty(base@.provenance)
    } else {
        assert(lo * sz + sz == (lo + 1) * sz) by (nonlinear_arith);
        assert(0 <= lo * sz <= hi * sz) by (nonlinear_arith)
            requires
                0 <= lo <= hi,
                sz >= 0,
        ;
        let tracked mut m = m;
        assert(m.contains_key(lo));
        let ghost m0 = m;
        let tracked mut e = m.tracked_remove(lo);
        assert(elems_at(m, base, lo + 1, hi)) by {
            assert forall|i: int| lo + 1 <= i < hi implies #[trigger] m.contains_key(i) && m[i].ptr() == ptr_add(base, i as usize) by {
                assert(m0.contains_key(i));
            }
        }
        let tracked rest = join_elems(m, base, lo + 1, hi);
        axiom_ptr_add_addr(base, lo as usize);
        e.leak_contents();
        let tracked er = e.into_raw();
        let tracked r = er.join(rest);
        assert forall|x: int| base@.addr + lo * sz <= x < base@.addr + hi * sz implies #[trigger] r.dom().contains(x) by {
            if x < base@.addr + (lo + 1) * sz {
                assert(er.dom().contains(x));
            } else {
                assert(rest.dom().contains(x));
            }
        }
        r
    }
}

/// The address and provenance of [`field_ptr`]: `off` bytes past `p`'s,
/// with `p`'s provenance, for an offset inside the `S` at `p`.
pub(crate) proof fn lemma_field_addr<S, T>(p: *mut S, off: usize)
    requires
        storage_ok(p),
        off <= size_of::<S>(),
    ensures
        field_ptr::<S, T>(p, off)@.addr == p@.addr + off,
        field_ptr::<S, T>(p, off)@.provenance == p@.provenance,
{
    broadcast use vstd::raw_ptr::group_raw_ptr_axioms, vstd::layout::layout_of_primitives;

    axiom_ptr_add_addr(p as *mut u8, off);
}

} // verus!

verus! {

// Byte offsets of `Arena`'s fields: `offset_of!`, which Verus cannot
// evaluate, so their values are opaque to it; `axiom_arena_layout` states
// what verified code relies on, and the build checks it.
#[verifier::external_body]
const A_INDEX: usize = core::mem::offset_of!(Arena, index);
#[verifier::external_body]
const A_DECAY_NS: usize = core::mem::offset_of!(Arena, decay_ns);
#[verifier::external_body]
const A_SETS: usize = core::mem::offset_of!(Arena, sets);
#[verifier::external_body]
const A_SETS_PERM: usize = core::mem::offset_of!(Arena, sets_perm);
#[verifier::external_body]
const A_CLASSES: usize = core::mem::offset_of!(Arena, classes);
#[verifier::external_body]
const A_LARGE: usize = core::mem::offset_of!(Arena, large);
#[verifier::external_body]
const A_STATS: usize = core::mem::offset_of!(Arena, stats);
#[verifier::external_body]
const A_LAST_EXIT_PURGE: usize = core::mem::offset_of!(Arena, last_exit_purge);

/// `Arena`'s layout (`repr(C)`): each field at its offset, aligned, and
/// ending by the next one's offset, the last by the size, and a class
/// state is not empty (a zero-sized one would leave its array outside
/// any allocation, as far as `in_alloc` knows). Trusted, and
/// checked by the build: the `const` assertion after this block
/// evaluates the same conditions with `fits`.
axiom fn axiom_arena_layout()
    ensures
        field_fits::<Arena, u16>(A_INDEX, A_DECAY_NS as int),
        field_fits::<Arena, AtomicU64>(A_DECAY_NS, A_SETS as int),
        field_fits::<Arena, *const SetTable>(A_SETS, A_SETS_PERM as int),
        field_fits::<Arena, Tracked<&'static PointsTo<SetTable>>>(A_SETS_PERM, A_CLASSES as int),
        field_fits::<Arena, [ClassState; NCLASSES]>(A_CLASSES, A_LARGE as int),
        0 < size_of::<ClassState>(),
        NCLASSES * size_of::<ClassState>() <= isize::MAX,
        field_fits::<Arena, AtomicU64>(A_LARGE, A_STATS as int),
        // Over several lines: `api`'s lock inventory test reads a line
        // `name: ...Lock<...>` as a lock field.
        field_fits::<
            Arena,
            Lock<ArenaStats>,
        >(A_STATS, A_LAST_EXIT_PURGE as int),
        field_fits::<Arena, AtomicU64>(A_LAST_EXIT_PURGE, size_of::<Arena>() as int),
        size_of::<Arena>() <= isize::MAX,
;

/// [`field_fits`] for the build: a field of type `T` at offset `off` of
/// an `S` is aligned for `T`, `S`'s alignment is a multiple of `T`'s, and
/// the field ends by `next`. The `requires` is met where the build
/// evaluates it, in the `const` assertions: an overflow there is a
/// compile error.
pub(crate) const fn fits<S, T>(off: usize, next: usize) -> (r: bool)
    requires
        off + size_of::<T>() <= usize::MAX,
    ensures
        r == field_fits::<S, T>(off, next as int),
{
    broadcast use vstd::layout::align_nonzero;

    off.is_multiple_of(core::mem::align_of::<T>())
        && core::mem::align_of::<S>().is_multiple_of(core::mem::align_of::<T>())
        && off + core::mem::size_of::<T>() <= next
}

} // verus!

// `axiom_arena_layout`, checked.
const _: () = assert!(
    fits::<Arena, u16>(A_INDEX, A_DECAY_NS)
        && fits::<Arena, AtomicU64>(A_DECAY_NS, A_SETS)
        && fits::<Arena, *const SetTable>(A_SETS, A_SETS_PERM)
        && fits::<Arena, Tracked<&'static PointsTo<SetTable>>>(A_SETS_PERM, A_CLASSES)
        && fits::<Arena, [ClassState; NCLASSES]>(A_CLASSES, A_LARGE)
        && core::mem::size_of::<ClassState>() > 0
        && NCLASSES as u128 * core::mem::size_of::<ClassState>() as u128 <= isize::MAX as u128
        && fits::<Arena, AtomicU64>(A_LARGE, A_STATS)
        && fits::<Arena, Lock<ArenaStats>>(A_STATS, A_LAST_EXIT_PURGE)
        && fits::<Arena, AtomicU64>(A_LAST_EXIT_PURGE, core::mem::size_of::<Arena>())
        && core::mem::size_of::<Arena>() <= isize::MAX as usize
);

// `axiom_array_layout` at `Arena::split`'s instance, checked.
const _: () = assert!(
    core::mem::size_of::<[ClassState; NCLASSES]>() == NCLASSES * core::mem::size_of::<ClassState>()
        && core::mem::align_of::<[ClassState; NCLASSES]>() == core::mem::align_of::<ClassState>()
);

verus! {

// Verus checks the size [`Arena::alloc_sets`] relies on: the slab set
// table fits a metadata object (the same in every configuration and on
// both targets).
global layout SetTable is size == 688, align == 8;

/// The slab set table's permission of a block of `raw` at `p`, when there
/// is a block.
proof fn table_perm(tracked raw: PointsToRaw, p: *mut SetTable) -> (tracked r: Option<PointsTo<SetTable>>)
    ensures
        p@.addr != 0 && raw.is_range(p@.addr as int, size_of::<SetTable>() as int)
            && raw.provenance() == p@.provenance && p@.addr as int % align_of::<SetTable>() as int == 0
            ==> r is Some && r->0.ptr() == p && r->0.is_uninit(),
{
    if p@.addr != 0 && raw.is_range(p@.addr as int, size_of::<SetTable>() as int)
        && raw.provenance() == p@.provenance && p@.addr as int % align_of::<SetTable>() as int == 0 {
        let tracked perm = raw.into_typed::<SetTable>(p@.addr);
        assert(perm.ptr()@ == p@);
        Some(perm)
    } else {
        None
    }
}

/// The permissions to the fields of an `Arena` under construction but
/// its class states, which are a map of their own, element by element
/// ([`Arena::split`]).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
tracked struct ArenaParts {
    tracked index: PointsTo<u16>,
    tracked decay_ns: PointsTo<AtomicU64>,
    tracked sets: PointsTo<*const SetTable>,
    tracked sets_perm: PointsTo<Tracked<&'static PointsTo<SetTable>>>,
    tracked large: PointsTo<AtomicU64>,
    tracked stats: PointsTo<Lock<ArenaStats>>,
    tracked last_exit_purge: PointsTo<AtomicU64>,
}

impl ArenaParts {
    /// Each permission is to its field of the `Arena` at `p`, which is
    /// [`storage_ok`], and the class states are inside it.
    spec fn at(&self, p: *mut Arena) -> bool {
        &&& storage_ok(p)
        &&& self.index.ptr() == field_ptr::<Arena, u16>(p, A_INDEX)
        &&& self.decay_ns.ptr() == field_ptr::<Arena, AtomicU64>(p, A_DECAY_NS)
        &&& self.sets.ptr() == field_ptr::<Arena, *const SetTable>(p, A_SETS)
        &&& self.sets_perm.ptr() == field_ptr::<Arena, Tracked<&'static PointsTo<SetTable>>>(p, A_SETS_PERM)
        &&& self.large.ptr() == field_ptr::<Arena, AtomicU64>(p, A_LARGE)
        &&& self.stats.ptr() == field_ptr::<Arena, Lock<ArenaStats>>(p, A_STATS)
        &&& self.last_exit_purge.ptr() == field_ptr::<Arena, AtomicU64>(p, A_LAST_EXIT_PURGE)
        &&& in_alloc(field_ptr::<Arena, ClassState>(p, A_CLASSES), NCLASSES as int * size_of::<ClassState>())
        &&& NCLASSES * size_of::<ClassState>() <= isize::MAX
    }
}

/// The class state permissions of the `Arena` at `p`, element by element.
spec fn classes_at(m: Map<int, PointsTo<ClassState>>, p: *mut Arena) -> bool {
    elems_at(m, field_ptr::<Arena, ClassState>(p, A_CLASSES), 0, NCLASSES as int)
}

/// Byte `x` is in the field of type `T` at offset `off` of the `Arena`
/// at `p`.
spec fn in_field<T>(x: int, p: *mut Arena, off: usize) -> bool {
    p@.addr + off <= x < p@.addr + off + size_of::<T>()
}

/// Every byte of the `Arena` at `p` is `pad`'s or a field's: what
/// [`Arena::split`] keeps of the storage once it has taken the fields
/// (the padding), which [`axiom_arena_assemble`] takes back.
spec fn pad_covers(pad: PointsToRaw, p: *mut Arena) -> bool {
    forall|x: int|
        p@.addr <= x < p@.addr + size_of::<Arena>() ==> #[trigger] pad.dom().contains(x)
            || in_field::<u16>(x, p, A_INDEX)
            || in_field::<AtomicU64>(x, p, A_DECAY_NS)
            || in_field::<*const SetTable>(x, p, A_SETS)
            || in_field::<Tracked<&'static PointsTo<SetTable>>>(x, p, A_SETS_PERM)
            || in_field::<[ClassState; NCLASSES]>(x, p, A_CLASSES)
            || in_field::<AtomicU64>(x, p, A_LARGE)
            || in_field::<Lock<ArenaStats>>(x, p, A_STATS)
            || in_field::<AtomicU64>(x, p, A_LAST_EXIT_PURGE)
}

impl Arena {
    /// The permissions to the fields of the uninitialised `Arena` `perm`
    /// is to: [`take_field`] for each field at the offsets
    /// [`axiom_arena_layout`] gives, and the class states split by
    /// [`split_array`]; and the bytes left over, the padding.
    proof fn split(tracked perm: PointsTo<Arena>) -> (tracked r: (ArenaParts, Map<int, PointsTo<ClassState>>, PointsToRaw))
        requires
            perm.is_uninit(),
            perm.ptr()@.addr + size_of::<Arena>() <= usize::MAX,
        ensures
            r.0.at(perm.ptr()),
            r.0.index.is_uninit(),
            r.0.decay_ns.is_uninit(),
            r.0.sets.is_uninit(),
            r.0.sets_perm.is_uninit(),
            r.0.large.is_uninit(),
            r.0.stats.is_uninit(),
            r.0.last_exit_purge.is_uninit(),
            classes_at(r.1, perm.ptr()),
            elems_uninit(r.1, 0, NCLASSES as int),
            pad_covers(r.2, perm.ptr()),
            r.2.provenance() == perm.ptr()@.provenance,
    {
        broadcast use vstd::raw_ptr::group_raw_ptr_axioms;

        axiom_arena_layout();
        let p = perm.ptr();
        perm.is_nonnull();
        perm.is_aligned();
        let tracked raw = perm.into_raw();
        axiom_raw_valid(&raw, p as *mut u8, size_of::<Arena>() as int);
        let tracked (index, raw) = take_field::<Arena, u16>(raw, p, A_INDEX, A_DECAY_NS as int);
        let tracked (decay_ns, raw) = take_field::<Arena, AtomicU64>(raw, p, A_DECAY_NS, A_SETS as int);
        let tracked (sets, raw) = take_field::<Arena, *const SetTable>(raw, p, A_SETS, A_SETS_PERM as int);
        let tracked (sets_perm, raw) = take_field::<Arena, Tracked<&'static PointsTo<SetTable>>>(
            raw,
            p,
            A_SETS_PERM,
            A_CLASSES as int,
        );
        let tracked (classes, raw) = take_field::<Arena, [ClassState; NCLASSES]>(raw, p, A_CLASSES, A_LARGE as int);
        let tracked (large, raw) = take_field::<Arena, AtomicU64>(raw, p, A_LARGE, A_STATS as int);
        let tracked (stats, raw) = take_field::<Arena, Lock<ArenaStats>>(raw, p, A_STATS, A_LAST_EXIT_PURGE as int);
        let tracked (last_exit_purge, pad) = take_field::<Arena, AtomicU64>(
            raw,
            p,
            A_LAST_EXIT_PURGE,
            size_of::<Arena>() as int,
        );
        axiom_array_layout::<ClassState, NCLASSES>();
        assert(0 < NCLASSES * size_of::<ClassState>()) by (nonlinear_arith)
            requires
                0 < NCLASSES,
                0 < size_of::<ClassState>(),
        ;
        let tracked classes = split_array(classes);
        lemma_field_cast::<Arena, [ClassState; NCLASSES], ClassState>(p, A_CLASSES);
        (ArenaParts { index, decay_ns, sets, sets_perm, large, stats, last_exit_purge }, classes, pad)
    }

    /// The storage of the `Arena` at `p` as one uninitialised permission
    /// again, from the permissions [`Arena::split`] made of it, whatever
    /// they hold (each value is given up): what a failed [`Arena::init`]
    /// hands back.
    proof fn unsplit(
        tracked parts: ArenaParts,
        tracked classes: Map<int, PointsTo<ClassState>>,
        tracked pad: PointsToRaw,
        p: *mut Arena,
    ) -> (tracked a: PointsTo<Arena>)
        requires
            parts.at(p),
            classes_at(classes, p),
            pad_covers(pad, p),
            pad.provenance() == p@.provenance,
        ensures
            a.ptr() == p,
            a.is_uninit(),
    {
        broadcast use vstd::raw_ptr::group_raw_ptr_axioms, vstd::layout::layout_of_primitives;

        axiom_arena_layout();
        axiom_array_layout::<ClassState, NCLASSES>();
        let tracked ArenaParts {
            mut index,
            mut decay_ns,
            mut sets,
            mut sets_perm,
            mut large,
            mut stats,
            mut last_exit_purge,
        } = parts;
        lemma_field_addr::<Arena, u16>(p, A_INDEX);
        lemma_field_addr::<Arena, AtomicU64>(p, A_DECAY_NS);
        lemma_field_addr::<Arena, *const SetTable>(p, A_SETS);
        lemma_field_addr::<Arena, Tracked<&'static PointsTo<SetTable>>>(p, A_SETS_PERM);
        lemma_field_addr::<Arena, ClassState>(p, A_CLASSES);
        lemma_field_addr::<Arena, AtomicU64>(p, A_LARGE);
        // Over several lines, as in `axiom_arena_layout`.
        lemma_field_addr::<
            Arena,
            Lock<ArenaStats>,
        >(p, A_STATS);
        lemma_field_addr::<Arena, AtomicU64>(p, A_LAST_EXIT_PURGE);
        index.leak_contents();
        decay_ns.leak_contents();
        sets.leak_contents();
        sets_perm.leak_contents();
        large.leak_contents();
        stats.leak_contents();
        last_exit_purge.leak_contents();
        let tracked r_index = index.into_raw();
        let tracked r_decay_ns = decay_ns.into_raw();
        let tracked r_sets = sets.into_raw();
        let tracked r_sets_perm = sets_perm.into_raw();
        let tracked r_large = large.into_raw();
        let tracked r_stats = stats.into_raw();
        let tracked r_last = last_exit_purge.into_raw();
        let base = field_ptr::<Arena, ClassState>(p, A_CLASSES);
        let tracked r_classes = join_elems(classes, base, 0, NCLASSES as int);
        let tracked raw = pad.join(r_index).join(r_decay_ns).join(r_sets).join(r_sets_perm).join(r_classes).join(r_large).join(
            r_stats,
        ).join(r_last);
        let sz = size_of::<Arena>() as int;
        assert forall|x: int| p@.addr <= x < p@.addr + sz implies #[trigger] raw.dom().contains(x) by {
            assert(pad.dom().contains(x) || in_field::<u16>(x, p, A_INDEX) || in_field::<AtomicU64>(x, p, A_DECAY_NS)
                || in_field::<*const SetTable>(x, p, A_SETS) || in_field::<Tracked<&'static PointsTo<SetTable>>>(x, p, A_SETS_PERM)
                || in_field::<[ClassState; NCLASSES]>(x, p, A_CLASSES) || in_field::<AtomicU64>(x, p, A_LARGE)
                || in_field::<Lock<ArenaStats>>(x, p, A_STATS) || in_field::<AtomicU64>(x, p, A_LAST_EXIT_PURGE));
            if in_field::<[ClassState; NCLASSES]>(x, p, A_CLASSES) {
                assert(r_classes.dom().contains(x));
            }
        }
        let tracked (exact, _) = raw.split(Set::range(p@.addr as int, p@.addr + sz));
        let tracked a = exact.into_typed::<Arena>(p@.addr);
        assert(a.ptr()@ == p@);
        a
    }
}

/// The written fields of the `Arena` at `p` (`Arena::split`'s parts and
/// class states, every one initialised), as the `Arena`: the value of
/// each field is what its permission holds. Trusted: vstd has no way to
/// join typed permissions; the permissions cover every field at its
/// offset, and `pad` every byte between and around them
/// ([`pad_covers`]), so their bytes are the `Arena`'s. (Without `pad`,
/// a caller that kept a padding byte's permission would hold it and the
/// `Arena`'s, two permissions for one byte, which proves `false`.) The
/// type invariant `Arena::inv` is a precondition, so the value is one an
/// `Arena` may hold.
axiom fn axiom_arena_assemble(
    tracked parts: ArenaParts,
    tracked classes: Map<int, PointsTo<ClassState>>,
    tracked pad: PointsToRaw,
    p: *mut Arena,
) -> (tracked a: PointsTo<Arena>)
    requires
        parts.at(p),
        classes_at(classes, p),
        pad_covers(pad, p),
        pad.provenance() == p@.provenance,
        parts.index.is_init(),
        parts.decay_ns.is_init(),
        parts.sets.is_init(),
        parts.sets_perm.is_init(),
        elems_init(classes, 0, NCLASSES as int),
        parts.large.is_init(),
        parts.stats.is_init(),
        parts.last_exit_purge.is_init(),
        forall|c: int| 0 <= c < NCLASSES ==> (#[trigger] classes[c]).value().spec_class() == c,
        forall|c: int| 0 <= c < NCLASSES ==> (#[trigger] classes[c]).value().spec_arena() == parts.index.value(),
        parts.sets_perm.value()@.ptr() == parts.sets.value(),
        parts.sets_perm.value()@.is_init(),
        parts.stats.value().rank() == rank_arena_stats(parts.index.value() as int),
        parts.sets_perm.value()@.value().rank() == rank_sets(parts.index.value() as int),
    ensures
        a.ptr() == p,
        a.is_init(),
        a.value().index == parts.index.value(),
        a.value().decay_ns == parts.decay_ns.value(),
        a.value().sets == parts.sets.value(),
        a.value().sets_perm == parts.sets_perm.value(),
        forall|c: int| 0 <= c < NCLASSES ==> #[trigger] a.value().classes[c] == classes[c].value(),
        a.value().large == parts.large.value(),
        a.value().stats == parts.stats.value(),
        a.value().last_exit_purge == parts.last_exit_purge.value(),
;

/// The in-place writes of [`Arena::init`], each through the permission
/// to the field it writes.
impl Arena {
    /// `index` and `decay_ns`.
    ///
    /// # Safety
    /// `this` is `init`'s storage.
    #[inline(always)]
    unsafe fn put_head(this: *mut Arena, index: u16, decay_ns: u64, Tracked(parts): Tracked<&mut ArenaParts>)
        requires
            old(parts).at(this),
        ensures
            final(parts).at(this),
            final(parts).index.is_init(),
            final(parts).index.value() == index,
            final(parts).decay_ns.is_init(),
            final(parts).sets == old(parts).sets,
            final(parts).sets_perm == old(parts).sets_perm,
            final(parts).large == old(parts).large,
            final(parts).stats == old(parts).stats,
            final(parts).last_exit_purge == old(parts).last_exit_purge,
    {
        proof {
            axiom_arena_layout();
        }
        // SAFETY: the fields of `init`'s storage, through their permissions.
        unsafe {
            ptr_mut_write(field_at(this, A_INDEX), Tracked(&mut parts.index), index);
            ptr_mut_write(field_at(this, A_DECAY_NS), Tracked(&mut parts.decay_ns), AtomicU64::new(decay_ns));
        }
    }

    /// Storage for the slab set table from `meta`, null when it is full,
    /// with the permission to its bytes and the claim [`Meta::free`] takes
    /// back with them. The table fits an object (`global layout
    /// SetTable`).
    // The tracked result is part of the signature.
    #[allow(clippy::type_complexity)]
    #[inline(always)]
    fn alloc_sets(meta: &Meta) -> (r: (*mut SetTable, Tracked<(PointsToRaw, MetaClaim)>))
        requires
            crate::sys::may_syscall(),
        ensures
            r.0@.addr != 0 ==> {
                &&& r.1@.0.is_range(r.0@.addr as int, size_of::<SetTable>() as int)
                &&& r.1@.0.provenance() == r.0@.provenance
                &&& r.0@.addr as int % align_of::<SetTable>() as int == 0
                &&& r.1@.1.held(meta.live_id(), r.0 as *mut u8, size_of::<SetTable>() as int, align_of::<SetTable>() as int)
            },
    {
        broadcast use vstd::raw_ptr::group_raw_ptr_axioms;

        proof {
            assert(crate::meta::MAX_BINNED == 0x10_0000usize) by (compute_only);
            assert(8 <= crate::meta::MAX_ALIGN) by (compute_only);
        }
        let (p, blk) = meta.alloc_raw(core::alloc::Layout::new::<SetTable>());
        let ghost p8 = p;
        let p = p as *mut SetTable;
        proof {
            assert((p as *mut u8)@ == p8@);
        }
        (p, blk)
    }

    /// `sets_ptr` into the field and its frozen permission `perm` into
    /// `sets_perm`.
    ///
    /// # Safety
    /// `this` is `init`'s storage.
    #[inline(always)]
    unsafe fn put_sets(
        this: *mut Arena,
        sets_ptr: *mut SetTable,
        perm: Tracked<&'static PointsTo<SetTable>>,
        Tracked(sets): Tracked<&mut PointsTo<*const SetTable>>,
        Tracked(sets_perm): Tracked<&mut PointsTo<Tracked<&'static PointsTo<SetTable>>>>,
    )
        requires
            storage_ok(this),
            old(sets).ptr() == field_ptr::<Arena, *const SetTable>(this, A_SETS),
            old(sets_perm).ptr() == field_ptr::<Arena, Tracked<&'static PointsTo<SetTable>>>(this, A_SETS_PERM),
        ensures
            final(sets).ptr() == old(sets).ptr(),
            final(sets).is_init(),
            final(sets).value() == sets_ptr as *const SetTable,
            final(sets_perm).ptr() == old(sets_perm).ptr(),
            final(sets_perm).is_init(),
            final(sets_perm).value() == perm,
    {
        proof {
            axiom_arena_layout();
        }
        // SAFETY: the fields of `init`'s storage, through their
        // permissions.
        unsafe {
            ptr_mut_write(field_at(this, A_SETS), Tracked(sets), sets_ptr as *const SetTable);
            ptr_mut_write(field_at(this, A_SETS_PERM), Tracked(sets_perm), perm);
        }
    }

    /// `cs` into `classes[c]`, through the permission `slot` to it.
    ///
    /// # Safety
    /// `this` is `init`'s storage and `c < NCLASSES`.
    #[inline(always)]
    unsafe fn put_class(this: *mut Arena, c: usize, cs: ClassState, Tracked(slot): Tracked<&mut PointsTo<ClassState>>)
        requires
            storage_ok(this),
            c < NCLASSES,
            old(slot).ptr() == ptr_add(field_ptr::<Arena, ClassState>(this, A_CLASSES), c),
            in_alloc(field_ptr::<Arena, ClassState>(this, A_CLASSES), NCLASSES as int * size_of::<ClassState>()),
            NCLASSES * size_of::<ClassState>() <= isize::MAX,
        ensures
            final(slot).ptr() == old(slot).ptr(),
            final(slot).is_init(),
            final(slot).value() == cs,
    {
        proof {
            axiom_arena_layout();
        }
        // SAFETY: element `c` of the field, through its permission.
        unsafe {
            let classes: *mut ClassState = field_at(this, A_CLASSES);
            ptr_mut_write(elem_at(classes, c, NCLASSES), Tracked(slot), cs);
        }
    }

    /// `large`, `stats` and `last_exit_purge`.
    ///
    /// # Safety
    /// `this` is `init`'s storage.
    #[inline(always)]
    unsafe fn put_tail(this: *mut Arena, index: u16, Tracked(parts): Tracked<&mut ArenaParts>)
        requires
            old(parts).at(this),
        ensures
            final(parts).at(this),
            final(parts).index == old(parts).index,
            final(parts).decay_ns == old(parts).decay_ns,
            final(parts).sets == old(parts).sets,
            final(parts).sets_perm == old(parts).sets_perm,
            final(parts).large.is_init(),
            final(parts).stats.is_init(),
            final(parts).stats.value().rank() == rank_arena_stats(index as int),
            final(parts).last_exit_purge.is_init(),
    {
        broadcast use axiom_arena_stats_lock_inv;

        proof {
            axiom_arena_layout();
        }
        // SAFETY: the fields of `init`'s storage, through their permissions.
        unsafe {
            ptr_mut_write(field_at(this, A_LARGE), Tracked(&mut parts.large), AtomicU64::new(0));
            ptr_mut_write(
                field_at(this, A_STATS),
                Tracked(&mut parts.stats),
                Lock::new(ArenaStats::empty(index), Ghost(rank_arena_stats(index as int))),
            );
            ptr_mut_write(field_at(this, A_LAST_EXIT_PURGE), Tracked(&mut parts.last_exit_purge), AtomicU64::new(0));
        }
    }

    /// Initialise `this` in place as arena `index` over the class tables
    /// from `tables` on (their frozen permissions in `tperms`), through the
    /// permission to its storage in `slot`, which is handed back
    /// initialised. Returns `false` if the metadata region cannot hold the
    /// slab sets, leaving `this` unusable until a later `init` over the
    /// same storage succeeds: `slot` gets the storage back uninitialised,
    /// and the slab set table object, when the sets themselves did not
    /// fit, goes back to `meta`. Every `classes[c]` is written, with a
    /// state of class `c` (the type invariant), before `true` is returned.
    ///
    /// # Safety
    /// `this` must point to writable storage for an `Arena` that nothing
    /// else reads until this returns `true`; the `NCLASSES` tables from
    /// `tables` must outlive it.
    // The ghost arguments are part of the signature.
    #[allow(clippy::too_many_arguments)]
    #[verifier::loop_isolation(false)]
    pub unsafe fn init(
        this: *mut Arena,
        meta: &Meta,
        regions: &Regions,
        tables: *mut SlabTable,
        Tracked(tperms): Tracked<&'static Map<int, PointsTo<SlabTable>>>,
        index: u16,
        decay_ns: u64,
        Tracked(slot): Tracked<&mut Held<Arena>>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: bool)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_meta()),
            old(slot).perm matches Some(p) && p.ptr() == this && p.is_uninit(),
            this@.addr + size_of::<Arena>() <= usize::MAX,
            regions.wf(),
            regions.separated(),
            *regions == process_regions(),
            meta.is_process(),
            crate::class::tables_ok(*tperms, tables),
        ensures
            r ==> (final(slot).perm matches Some(p) && p.ptr() == this && p.is_init() && p.value().spec_index() == index),
            !r ==> (final(slot).perm matches Some(p) && p.ptr() == this && p.is_uninit()),
            *final(ord) == *old(ord),
    {
        let tracked (mut parts, mut classes, pad) = Arena::split(slot.perm.tracked_take());
        // SAFETY: the caller guarantees `this` is writable and unaliased;
        // every write goes through the permission to its field.
        unsafe {
            Arena::put_head(this, index, decay_ns, Tracked(&mut parts));
            #[cfg(test)]
            if FAIL_INIT
                .compare_exchange(meta as *const Meta as usize, 0, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return false;
            }
            let (sets_ptr, Tracked(blk)) = Arena::alloc_sets(meta);
            if sets_ptr.is_null() {
                proof {
                    slot.perm = Some(Arena::unsplit(parts, classes, pad, this));
                }
                return false;
            }
            let tracked (raw, claim) = blk;
            let tracked mut perm = table_perm(raw, sets_ptr).tracked_unwrap();
            // Ghost: the permissions to the class sets' slots, each taken
            // once, by its class's `ClassState::new`.
            let tracked mut slots: Map<(int, int), ZeroRaw> = Map::tracked_empty();
            let Tracked(sets_perm) = match SetTable::new(meta, regions, Ghost(index), Tracked(&mut slots), Tracked(ord)) {
                Some(sets) => {
                    ptr_mut_write(sets_ptr, Tracked(&mut perm), sets);
                    // Frozen: nothing writes or frees the table again; the
                    // arena and its class states' sets share it.
                    let tracked perm = vstd::modes::tracked_static_ref(perm);
                    Arena::put_sets(this, sets_ptr, Tracked(perm), Tracked(&mut parts.sets), Tracked(&mut parts.sets_perm));
                    Tracked(perm)
                },
                None => {
                    let tracked raw = perm.into_raw();
                    meta.free(sets_ptr as *mut u8, core::alloc::Layout::new::<SetTable>(), Tracked((raw, claim)));
                    proof {
                        slot.perm = Some(Arena::unsplit(parts, classes, pad, this));
                    }
                    return false;
                },
            };
            for c in 0..NCLASSES
                invariant
                    classes_at(classes, this),
                    elems_init(classes, 0, c as int),
                    forall|d: int| 0 <= d < c ==> (#[trigger] classes[d]).value().spec_class() == d,
                    forall|d: int| 0 <= d < c ==> (#[trigger] classes[d]).value().spec_arena() == index,
                    parts.index.is_init() && parts.index.value() == index,
                    // `SETS_PER_CLASS` sets per class.
                    forall|d: int, k: int| c <= d < NCLASSES && 0 <= k < 3 ==> #[trigger] sets_perm.value().holds_set(slots, d, k),
            {
                let ghost slots0 = slots;
                let tp = elem_at(tables, c, NCLASSES);
                match ClassState::new(meta, regions, tp, Tracked(tperms), Ghost(tables), Ghost(c as int), index, sets_ptr, Tracked(sets_perm), Tracked(&mut slots)) {
                    Some(cs) => {
                        let ghost before = classes;
                        proof {
                            assert(classes.contains_key(c as int));
                        }
                        let tracked slot_c = classes.tracked_borrow_mut(c as int);
                        Arena::put_class(this, c, cs, Tracked(slot_c));
                        proof {
                            assert forall|d: int| #![trigger classes.contains_key(d)] #![trigger classes[d]] 0 <= d < NCLASSES implies classes.contains_key(d)
                                && classes[d].ptr() == ptr_add(field_ptr::<Arena, ClassState>(this, A_CLASSES), d as usize) by {
                                assert(before.contains_key(d));
                            }
                            assert forall|d: int| 0 <= d < c + 1 implies (#[trigger] classes[d]).is_init()
                                && classes[d].value().spec_class() == d && classes[d].value().spec_arena() == index by {
                                if d < c {
                                    assert(before[d] == classes[d]);
                                }
                            }
                            assert(classes_at(classes, this));
                            assert forall|d: int, k: int| c + 1 <= d < NCLASSES && 0 <= k < 3 implies #[trigger] sets_perm.value().holds_set(slots, d, k) by {
                                assert(sets_perm.value().holds_set(slots0, d, k));
                            }
                        }
                    },
                    None => {
                        proof {
                            slot.perm = Some(Arena::unsplit(parts, classes, pad, this));
                        }
                        return false;
                    },
                }
            }
            Arena::put_tail(this, index, Tracked(&mut parts));
        }
        proof {
            slot.perm = Some(axiom_arena_assemble(parts, classes, pad, this));
        }
        true
    }

    /// `classes[c]` is the state of class `c` in this arena (its
    /// `spec_arena` is `index`: the class states are the arena's, which
    /// arena isolation needs). `init` builds the value in place, so no
    /// struct expression checks this: `init` proves that it writes every
    /// `classes[c]` with a state of class `c` and arena `index` (from
    /// `ClassState::new`'s postcondition over `tables[c]`, whose class is
    /// `c` by `init`'s precondition) before it returns `true`.
    /// `sets_perm` is the initialised table at `sets`, and the two locks
    /// have their arena's ranks. These are preconditions of
    /// `axiom_arena_assemble`, the trusted step that turns the written
    /// fields into the `Arena`.
    #[verifier::type_invariant]
    spec fn inv(self) -> bool {
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.classes[c].spec_class() == c
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.classes[c].spec_arena() == self.index
        &&& self.sets_perm@.ptr() == self.sets
        &&& self.sets_perm@.is_init()
        &&& self.stats.rank() == rank_arena_stats(self.index as int)
        &&& self.sets_perm@.value().rank() == rank_sets(self.index as int)
    }

    /// The arena's slab set table (fork handlers, tests).
    #[inline]
    pub fn sets(&self) -> (r: &SetTable)
        ensures
            *r == self.spec_sets(),
            r.rank() == rank_sets(self.spec_index() as int),
    {
        proof {
            use_type_invariant(self);
        }
        // Written by `init` to a live, never-freed metadata object.
        ptr_ref(self.sets, Tracked(*self.sets_perm.borrow()))
    }

    /// The slab set table [`Arena::sets`] returns.
    pub closed spec fn spec_sets(&self) -> SetTable {
        self.sets_perm@.value()
    }

    /// One run in the `large` word; the low 32 bits count pages (a run's
    /// `pages` is a `u32` in its descriptor, and the region holds fewer
    /// than 2^32 pages in all).
    const LARGE_RUN: u64 = 1 << 32;

    /// The `large` word for one run of `pages` (guard included).
    fn large_word(pages: usize) -> u64
        requires
            pages < 0x1_0000_0000,
    {
        proof {
            assert((1usize << 32usize) == 0x1_0000_0000usize) by (bit_vector);
        }
        assert!(pages < (1 << 32), "arena: huge run of {pages} pages");
        Self::LARGE_RUN | pages as u64
    }

    /// A huge run of `pages` (guard included) was allocated through this
    /// arena.
    pub fn note_huge_alloc(&self, pages: usize)
        requires
            pages < 0x1_0000_0000,
    {
        self.large.fetch_add(Self::large_word(pages), Ordering::Relaxed);
    }

    /// A huge run of `pages` (guard included) of this arena was freed.
    pub fn note_huge_free(&self, pages: usize)
        requires
            pages < 0x1_0000_0000,
    {
        self.large.fetch_sub(Self::large_word(pages), Ordering::Relaxed);
    }

    /// Live huge runs of this arena and their pages, guards included, as
    /// one snapshot (see the field).
    pub fn large(&self) -> (r: (usize, usize))
        ensures
            r.0 < 0x1_0000_0000,
            r.1 < 0x1_0000_0000,
    {
        proof {
            assert(Self::LARGE_RUN == 0x1_0000_0000) by (compute_only);
        }
        let w = self.large.load(Ordering::Relaxed);
        proof {
            assert((w >> 32u64) < 0x1_0000_0000u64 && (w & 0xffff_ffffu64) < 0x1_0000_0000u64) by (bit_vector);
        }
        ((w >> 32) as usize, (w & (Self::LARGE_RUN - 1)) as usize)
    }

    /// The statistics stored by the last `epoch`: they satisfy the
    /// accounting identities (the lock's invariant).
    pub fn stats(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: ArenaStats)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_arena_stats(0)),
        ensures
            r.identities(),
            *final(ord) == *old(ord),
    {
        broadcast use axiom_arena_stats_lock_inv;

        proof {
            use_type_invariant(self);
        }
        *self.stats.lock(Tracked(ord), Tracked(&mut LockOrder::spare()))
    }

    /// Store the statistics of an `epoch`.
    pub fn store_stats(&self, s: &ArenaStats, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_arena_stats(0)),
            s.identities(),
        ensures
            *final(ord) == *old(ord),
    {
        broadcast use axiom_arena_stats_lock_inv;

        proof {
            use_type_invariant(self);
            // The value this holder leaves satisfies the lock's invariant.
            assert(crate::lock::lock_inv(*s));
        }
        *self.stats.lock(Tracked(ord), Tracked(&mut LockOrder::spare())) = *s;
    }

    /// The statistics lock, for the fork handlers (a leaf: no lock is
    /// taken while it is held; `epoch` takes it under the process
    /// statistics lock).
    pub fn stats_raw_lock(&self) -> (r: &crate::lock::RawLock)
        ensures
            *r == self.spec_stats_lock(),
            r.rank() == rank_arena_stats(self.spec_index() as int),
    {
        proof {
            use_type_invariant(self);
        }
        self.stats.raw()
    }

    /// The lock [`Self::stats_raw_lock`] returns.
    pub closed spec fn spec_stats_lock(&self) -> crate::lock::RawLock {
        self.stats.spec_raw()
    }

    pub fn index(&self) -> (r: u16)
        ensures
            r == self.spec_index(),
    {
        self.index
    }

    /// The arena's index: the registry slot it is published in, and the
    /// arena of each of its class states.
    pub closed spec fn spec_index(&self) -> u16 {
        self.index
    }

    /// The decay period in nanoseconds (`purge::NEVER` when purging is off).
    #[inline]
    pub fn decay_ns(&self) -> u64 {
        self.decay_ns.load(Ordering::Relaxed)
    }

    /// The exit purge's gate (design 13.3): the last thread heap homed on
    /// this arena is leaving at `now`; `true` when its `Empty` slabs
    /// should be purged now, which is at most once per decay period of
    /// the arena and never with decay off (`-1`). Thread churn (a pool
    /// that retires and recreates its threads, larson, mstress) would
    /// otherwise purge and refault the same slabs at every exit; a
    /// process whose workers leave once (the Ruby soak's end) purges
    /// once. Called under no lock; two exits racing on one arena within
    /// a period both see the old stamp and both purge, which is harmless.
    pub fn take_exit_purge(&self, now: u64) -> bool {
        let period = self.decay_ns();
        if period == crate::purge::NEVER {
            return false;
        }
        let last = self.last_exit_purge.load(Ordering::Relaxed);
        if last != 0 && now.wrapping_sub(last) < period {
            return false;
        }
        self.last_exit_purge.store(now.max(1), Ordering::Relaxed);
        true
    }
}

} // verus!

impl Arena {
    /// Class state for `class`, without the bounds check the fast paths do
    /// not need (`class` comes from `class_of`).
    #[inline(always)]
    pub fn class(&self, class: usize) -> &ClassState {
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of` only yields indices below `NCLASSES`.
        unsafe { self.classes.get_unchecked(class) }
    }

    /// Class state for `class`, with the bounds check: the accessor for the
    /// cold walks (the statistics), whose proofs then do not depend on the
    /// fast paths' [`Arena::class`].
    pub fn class_at(&self, class: usize) -> &ClassState {
        &self.classes[class]
    }

    /// Change the decay period; `-1` turns purging off. Takes effect at the
    /// next periodic tick, or at once for a background thread, which is
    /// woken to recompute its deadline.
    pub fn set_decay_ms(&self, ms: i64) {
        self.decay_ns
            .store(crate::purge::decay_ns_from_ms(ms), Ordering::Relaxed);
        crate::purge::note_decay_changed();
    }

    /// Decay step over every class (see `ClassState::decay_with`) with
    /// the period the cgroup policy gives this arena and its keep ratio
    /// for the retention cap (design 11): the earliest next deadline,
    /// `None` when nothing is `Empty`; `NEVER` does nothing.
    pub fn decay_with(
        &self,
        now: u64,
        decay_ns: u64,
        keep16: u32,
        _ord: Tracked<&mut LockOrder>,
    ) -> Option<u64> {
        if decay_ns == crate::purge::NEVER {
            return None;
        }
        let mut next: Option<u64> = None;
        for cs in self.classes.iter() {
            if let Some(d) = cs.decay_with(now, decay_ns, keep16, Tracked::assume_new()) {
                next = Some(next.map_or(d, |n| n.min(d)));
            }
        }
        next
    }

    /// `Empty` bytes over every class (one lock hold per class that has
    /// any), for the cgroup policy's retention figure.
    pub fn empty_bytes(&self, _ord: Tracked<&mut LockOrder>) -> usize {
        let mut sum = 0usize;
        let mut c = 0;
        while c < NCLASSES {
            let cs = &self.classes[c];
            if cs.oldest_empty() != 0 {
                sum += cs.empty_bytes(Tracked::assume_new());
            }
            c += 1;
        }
        sum
    }

    /// Purge every `Empty` slab of every class whatever its age. Returns
    /// how many were purged. `left` is ghost: what the purge left
    /// ([`Left`]).
    pub fn purge_all(
        &self,
        _left: Tracked<&mut Ghost<Left>>,
        _ord: Tracked<&mut LockOrder>,
    ) -> usize {
        let mut sum = 0usize;
        let mut c = 0;
        while c < NCLASSES {
            let cs = &self.classes[c];
            sum += cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new());
            c += 1;
        }
        sum
    }
}

/// The arena registry. Index 0 is `Shared::arena0`; the rest live in the
/// metadata region. `count` is the number of indices in use: the automatic
/// ones from init plus every `create` since, published with a release
/// store once the arena is usable (automatic arenas may still be
/// unmaterialised, see [`Arenas::get`]).
///
/// `repr(C)`: the fields lie in declaration order, so the layout
/// assertion below holds on every compiler.
#[repr(C)]
pub struct Arenas {
    table: [AtomicPtr<Arena>; MAX_ARENAS],
    /// Thread heaps currently homed on each arena (`assign`/`bind` add,
    /// `unbind` subtracts); explicit caches are not counted.
    bound: [AtomicU32; MAX_ARENAS],
    /// Creation lock; the value is `count` for the writer.
    lock: Lock<u32>,
    /// The storage of a build whose `Arena::init` failed, for the next
    /// build to reuse: a reservation is never freed, so a retry under
    /// metadata pressure would otherwise strand one per attempt. Null
    /// when there is none; only touched under `lock`.
    spare: AtomicPtr<Arena>,
    count: AtomicU32,
    /// Indices below this are automatic (`narenas`).
    auto: u32,
}

// Byte offsets of `Arenas`'s fields.
const AR_TABLE: usize = core::mem::offset_of!(Arenas, table);
const AR_BOUND: usize = core::mem::offset_of!(Arenas, bound);
const AR_LOCK: usize = core::mem::offset_of!(Arenas, lock);
const AR_COUNT: usize = core::mem::offset_of!(Arenas, count);
const AR_AUTO: usize = core::mem::offset_of!(Arenas, auto);
const AR_SPARE: usize = core::mem::offset_of!(Arenas, spare);

// `Arenas`'s layout: each field at its offset, aligned, and ending by the
// next one's offset, the last by the size. `Arenas` is `repr(C)`, which
// fixes the fields in declaration order (the table, the bound counts,
// `lock`, `spare`, `count`, `auto`), so this holds on every compiler.
const _: () = assert!(
    fits::<Arenas, [AtomicPtr<Arena>; MAX_ARENAS]>(AR_TABLE, AR_BOUND)
        && MAX_ARENAS as u128 * core::mem::size_of::<AtomicPtr<Arena>>() as u128
            <= isize::MAX as u128
        && fits::<Arenas, [AtomicU32; MAX_ARENAS]>(AR_BOUND, AR_LOCK)
        && MAX_ARENAS as u128 * core::mem::size_of::<AtomicU32>() as u128 <= isize::MAX as u128
        && fits::<Arenas, Lock<u32>>(AR_LOCK, AR_SPARE)
        && fits::<Arenas, AtomicPtr<Arena>>(AR_SPARE, AR_COUNT)
        && fits::<Arenas, AtomicU32>(AR_COUNT, AR_AUTO)
        && fits::<Arenas, u32>(AR_AUTO, core::mem::size_of::<Arenas>())
        && core::mem::size_of::<Arenas>() <= isize::MAX as usize
);

// An array of `N` `T`s is `N` `T`s back to back, aligned as one, at
// `Arenas`'s two arrays.
const _: () = assert!(
    core::mem::size_of::<[AtomicPtr<Arena>; MAX_ARENAS]>()
        == MAX_ARENAS * core::mem::size_of::<AtomicPtr<Arena>>()
        && core::mem::align_of::<[AtomicPtr<Arena>; MAX_ARENAS]>()
            == core::mem::align_of::<AtomicPtr<Arena>>()
        && core::mem::size_of::<[AtomicU32; MAX_ARENAS]>()
            == MAX_ARENAS * core::mem::size_of::<AtomicU32>()
        && core::mem::align_of::<[AtomicU32; MAX_ARENAS]>() == core::mem::align_of::<AtomicU32>()
);

/// `a < b` on `(u32, bool)`: lexicographic, `false < true`, as `core`
/// documents tuple and `bool` order.
#[inline(always)]
fn key_less(a: (u32, bool), b: (u32, bool)) -> bool {
    a < b
}

/// The in-place writes of [`Arenas::init`], each to the field it names.
impl Arenas {
    /// `lock`, `count` and `auto`, from the automatic count `auto`.
    ///
    /// # Safety
    /// `this` is `init`'s storage.
    #[inline(always)]
    unsafe fn put_head(this: *mut Arenas, auto: u32) {
        // SAFETY: the fields of `init`'s storage.
        unsafe {
            ptr::write(
                field_at(this, AR_LOCK),
                Lock::new(auto, Ghost::assume_new()),
            );
            ptr::write(field_at(this, AR_COUNT), AtomicU32::new(auto));
            ptr::write(field_at(this, AR_AUTO), auto);
        }
    }

    /// Entry `i` of `table`, null, and of `bound`, a count of 0.
    ///
    /// # Safety
    /// `this` is `init`'s storage and `i < MAX_ARENAS`.
    #[inline(always)]
    unsafe fn put_entry(this: *mut Arenas, i: usize) {
        // SAFETY: element `i` of each array field.
        unsafe {
            let table: *mut AtomicPtr<Arena> = field_at(this, AR_TABLE);
            let bound: *mut AtomicU32 = field_at(this, AR_BOUND);
            ptr::write(
                elem_at(table, i, MAX_ARENAS),
                AtomicPtr::new(ptr::null_mut()),
            );
            ptr::write(elem_at(bound, i, MAX_ARENAS), AtomicU32::new(0));
        }
    }

    /// `arena0` into entry 0 of `table`.
    ///
    /// # Safety
    /// `this` is `init`'s storage, its entry 0 written.
    #[inline(always)]
    unsafe fn put_arena0(this: *mut Arenas, arena0: *mut Arena) {
        // SAFETY: element 0 of the field, written by `put_entry`.
        unsafe {
            let table: *mut AtomicPtr<Arena> = field_at(this, AR_TABLE);
            (*elem_at(table, 0, MAX_ARENAS)).store(arena0, Ordering::Release);
        }
    }

    /// `spare`, null.
    ///
    /// # Safety
    /// `this` is `init`'s storage.
    #[inline(always)]
    unsafe fn put_spare(this: *mut Arenas) {
        // SAFETY: the field of `init`'s storage.
        unsafe {
            ptr::write(
                field_at(this, AR_SPARE),
                AtomicPtr::<Arena>::new(ptr::null_mut()),
            );
        }
    }
}

/// An arena's bound count went below zero: a heap was unbound twice, or
/// never bound.
#[cold]
#[inline(never)]
#[track_caller]
fn unbind_unbound(index: u16) -> ! {
    panic!("arena {index}: unbinding a heap that was never bound")
}

impl Arenas {
    /// Initialise `this` in place with `arena0` at index 0 and `narenas`
    /// automatic indices (clamped to `1..=MAX_ARENAS`).
    ///
    /// # Safety
    /// `this` must point to writable storage that nothing else reads until
    /// this returns; `arena0` must outlive the registry.
    pub unsafe fn init(this: *mut Arenas, arena0: *mut Arena, narenas: u32) {
        let auto = narenas.clamp(1, MAX_ARENAS as u32);
        // SAFETY: the caller guarantees `this` is writable and unaliased.
        unsafe {
            Arenas::put_head(this, auto);
            for i in 0..MAX_ARENAS {
                Arenas::put_entry(this, i);
            }
            Arenas::put_arena0(this, arena0);
            Arenas::put_spare(this);
        }
    }

    /// Number of arena indices in use (`arenas.narenas`).
    #[inline]
    pub fn count(&self) -> u32 {
        self.count.load(Ordering::Acquire)
    }

    /// Number of automatic arenas (`opt.narenas` as applied).
    #[inline]
    pub fn auto(&self) -> u32 {
        self.auto
    }

    /// Pick the home arena for a new thread heap and bind the heap to it:
    /// the automatic arena with the fewest bound heaps, ties going to one
    /// that is already built, then to the lowest index. Under the creation
    /// lock so two threads created at once do not both take the same idle
    /// arena. Does not materialise the arena; the heap's first refill does,
    /// which is why a single-threaded program (main thread on arena 0,
    /// nothing else ever bound) never builds a second arena and never
    /// pays the `multi_arena` routing on `free`. The caller undoes the
    /// binding with [`Arenas::unbind`] when the heap is destroyed.
    pub fn assign(&self) -> u16 {
        if self.auto == 1 {
            self.count_in(0);
            return 0;
        }
        let _g = self.lock.lock(Tracked::assume_new(), Tracked::assume_new());
        let mut best = 0usize;
        // (bound heaps, not yet built): the smallest wins.
        let mut best_key = (u32::MAX, true);
        for i in 0..self.auto as usize {
            let key = (
                self.load_bound(i),
                self.table[i].load(Ordering::Relaxed).is_null(),
            );
            if key_less(key, best_key) {
                best = i;
                best_key = key;
                if key == (0, false) {
                    break;
                }
            }
        }
        self.count_in(best);
        best as u16
    }

    /// `bound[i]`.
    #[inline(always)]
    fn load_bound(&self, i: usize) -> u32 {
        self.bound[i].load(Ordering::Relaxed)
    }

    /// `bound[i] += 1`.
    #[inline(always)]
    fn count_in(&self, i: usize) {
        self.bound[i].fetch_add(1, Ordering::Relaxed);
    }

    /// Count a thread heap as homed on `index` (`thread.arena` moving a
    /// heap to an arena `assign` did not pick).
    pub fn bind(&self, index: u16) {
        self.count_in(index as usize)
    }

    /// Bind a thread heap to `index` only if no heap is homed there now
    /// (one compare-and-swap of its count from 0 to 1): the late rehoming
    /// of a young heap onto the arena an exited thread left
    /// (`Heap::try_rehome`). Two heaps racing for one idle arena cannot
    /// both win, so a rehome never stacks a second heap on an arena. A
    /// plain load first: a busy arena (xmalloc's live producers) is
    /// refused without taking its counter's line exclusive.
    pub fn try_bind_idle(&self, index: u16) -> bool {
        let i = index as usize;
        if self.load_bound(i) != 0 {
            return false;
        }
        self.bind_if_idle(i)
    }

    /// `bound[i]`'s compare-and-swap from 0 to 1.
    #[inline(always)]
    fn bind_if_idle(&self, i: usize) -> bool {
        self.bound[i]
            .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    /// A thread heap homed on `index` is destroyed or moves away.
    pub fn unbind(&self, index: u16) {
        let prev = self.bound[index as usize].fetch_sub(1, Ordering::Relaxed);
        if prev == 0 {
            unbind_unbound(index);
        }
    }

    /// Thread heaps currently homed on `index`.
    pub fn bound(&self, index: u16) -> u32 {
        self.load_bound(index as usize)
    }

    /// Arena `index` if it exists. An automatic index whose arena has not
    /// been built yet is built here (cold); an explicit index that
    /// `create` has not handed out, or one beyond the table, is `None`.
    #[inline]
    pub fn get<'a>(&'a self, shared: &'a Shared, index: u16) -> Option<&'a Arena> {
        let i = index as usize;
        if i >= MAX_ARENAS {
            return None;
        }
        let p = self.table[i].load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: arenas are never freed; the pointer was published
            // after `Arena::init` with a release store.
            return Some(unsafe { &*p });
        }
        self.materialise(shared, index)
    }

    /// Arena `index` if it has been built, without building it.
    pub fn get_existing(&self, index: u16) -> Option<&Arena> {
        let i = index as usize;
        if i >= MAX_ARENAS {
            return None;
        }
        let p = self.table[i].load(Ordering::Acquire);
        // SAFETY: as in `get`.
        if p.is_null() {
            None
        } else {
            Some(unsafe { &*p })
        }
    }

    #[cold]
    #[inline(never)]
    fn materialise<'a>(&'a self, shared: &'a Shared, index: u16) -> Option<&'a Arena> {
        if index as u32 >= self.auto {
            return None;
        }
        let _g = self.lock.lock(Tracked::assume_new(), Tracked::assume_new());
        let p = self.table[index as usize].load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: as in `get`.
            return Some(unsafe { &*p });
        }
        self.build(shared, index)
    }

    /// Build arena `index` in the metadata region and publish it. Called
    /// with the creation lock held and the slot known to be empty.
    fn build<'a>(&'a self, shared: &'a Shared, index: u16) -> Option<&'a Arena> {
        let mut p = self.spare.swap(ptr::null_mut(), Ordering::Relaxed);
        if p.is_null() {
            let layout = core::alloc::Layout::new::<Arena>();
            // Never freed, so a reservation (page aligned, which covers the
            // 128-byte class state alignment) rather than an object.
            p = shared.meta.reserve_committed(
                layout.size(),
                layout.align().max(shared.regions.page_size()),
            )? as *mut Arena;
        }
        // SAFETY: exclusively owned storage of the right size and
        // alignment, fresh or left by a failed build (never published;
        // `init` writes every field); the tables live in `Shared` for the
        // process.
        let ok = unsafe {
            Arena::init(
                p,
                &shared.meta,
                &shared.regions,
                shared.tables().as_ptr().cast_mut(),
                Tracked::assume_new(),
                index,
                shared.default_decay_ns(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
        };
        if !ok {
            self.spare.store(p, Ordering::Relaxed);
            return None;
        }
        shared.set_multi_arena();
        self.table[index as usize].store(p, Ordering::Release);
        // SAFETY: just built and published.
        Some(unsafe { &*p })
    }

    /// `arenas.create`: a new explicit arena. `None` when the table or the
    /// metadata region is full.
    pub fn create(&self, shared: &Shared) -> Option<u16> {
        let mut g = self.lock.lock(Tracked::assume_new(), Tracked::assume_new());
        let index = *g;
        if index as usize >= MAX_ARENAS {
            return None;
        }
        self.build(shared, index as u16)?;
        *g = index + 1;
        self.count.store(index + 1, Ordering::Release);
        Some(index as u16)
    }

    /// The creation lock, for fork handlers (taken before every class lock).
    pub fn raw_lock(&self) -> &crate::lock::RawLock {
        self.lock.raw()
    }

    /// Every arena that exists, in index order. Tests only: the library
    /// scans `0..count()` with `get_existing` instead.
    #[cfg(test)]
    pub fn iter<'a>(&'a self) -> impl Iterator<Item = &'a Arena> + 'a {
        let n = self.count() as usize;
        (0..n).filter_map(move |i| {
            let p = self.table[i].load(Ordering::Acquire);
            // SAFETY: as in `get`.
            if p.is_null() {
                None
            } else {
                Some(unsafe { &*p })
            }
        })
    }
}
