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

use crate::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};
use core::ptr;

#[cfg(verus_only)]
use crate::class::SETS_PER_CLASS;
use crate::class::{ClassState, SetTable, SlabTable};
use crate::heap::Shared;
#[cfg(verus_only)]
use crate::layout::process_regions;
use crate::layout::{NCLASSES, Regions};
use crate::lock::{Lock, LockOrder};
#[cfg(verus_only)]
use crate::lock::{rank_arena_stats, rank_arenas, rank_class, rank_meta, rank_sets};
use crate::meta::Meta;
use crate::purge::Left;
#[cfg(verus_only)]
use crate::purge::paged;
use crate::stats::ArenaStats;
use crate::vspec::addr::addr_to_ptr;
#[cfg(verus_only)]
use crate::vspec::arena::axiom_arena_stats_lock_inv;
use crate::vspec::count::CountPerm;
#[cfg(verus_only)]
use crate::vspec::count::{count_id, wrap32};
#[cfg(verus_only)]
use crate::vspec::lock::lock_inv;
#[cfg(verus_only)]
use crate::vspec::ptr::{axiom_ptr_add_addr, axiom_raw_valid, in_alloc, ptr_add};
#[cfg(verus_only)]
use crate::vspec::zero::ZeroRaw;
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
#[cfg(verus_only)]
use vstd::layout::{align_of, size_of};
use vstd::prelude::*;
#[cfg(verus_only)]
use vstd::raw_ptr::PointsToRaw;
use vstd::raw_ptr::{PointsTo, ptr_mut_write, ptr_ref};
use vstd::resource::Loc;
#[cfg(verus_only)]
use vstd::resource::set::GhostSubset;
use vstd::resource::set::{GhostSetAuth, GhostSingleton};

verus! {

/// Arena indices are the 12-bit `MALLOCX_ARENA` field; 4096 itself is
/// `MALLCTL_ARENAS_ALL`.
pub const MAX_ARENAS: usize = 4096;
/// `MALLCTL_ARENAS_ALL`.
pub const ARENAS_ALL: u32 = 4096;

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

verus! {

// Field and element permissions for the in-place builds (`Arena::init`,
// `Arenas::init`, `Shared::init`). Verus has no raw-pointer field
// projection (`&raw mut (*p).f`), so a field's pointer is the struct's
// pointer advanced by the field's byte offset (`offset_of!`, which
// compiles to the same address), and a field's permission is carved from
// the struct's: [`take_field`] splits the bytes at the offsets a layout
// axiom states (checked by the build: see `fits`), [`split_array`] splits
// an array into its elements. Putting the written values back together
// into the struct's value has no counterpart in vstd and is an axiom per
// struct (`axiom_arena_assemble`, `axiom_arenas_assemble`, `Shared`'s
// `axiom_shared_built`). An array whose elements are only read after the
// build is not joined: `Shared::init` freezes its class tables' element
// permissions (`tracked_static_ref`), which `axiom_shared_built` reads the
// built tables from.

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
/// `off` is inside the allocation `p` points into (the precondition).
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
/// The `n` elements are inside one allocation (the precondition).
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
/// [`Arena::init`] and [`Arenas::init`] take it out and hand it back
/// built. A struct rather than a bare `Option` so that Verus treats a
/// `&mut` to it as ghost.
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

/// Multiples of `m` add to a multiple of `m`.
proof fn lemma_add_aligned(x: int, y: int, m: int)
    requires
        m > 0,
        x % m == 0,
        y % m == 0,
    ensures
        (x + y) % m == 0,
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(x, m);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(y, m);
    let a = x / m;
    let b = y / m;
    assert(x + y == (a + b) * m) by (nonlinear_arith)
        requires
            x == m * a,
            y == m * b,
    ;
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(a + b, m);
}

/// Of two powers of two, the larger is a multiple of the smaller.
proof fn lemma_pow2_divides_larger(a: usize, b: usize)
    requires
        crate::meta::is_pow2(a),
        crate::meta::is_pow2(b),
        a <= b,
    ensures
        b % a == 0,
{
    use vstd::arithmetic::power2::pow2;

    let ka = choose|k: nat| k < 64 && a as nat == #[trigger] pow2(k);
    let kb = choose|k: nat| k < 64 && b as nat == #[trigger] pow2(k);
    if ka > kb {
        vstd::arithmetic::power2::lemma_pow2_strictly_increases(kb, ka);
    }
    vstd::arithmetic::power2::lemma_pow2_adds(ka, (kb - ka) as nat);
    vstd::arithmetic::power2::lemma_pow2_pos(ka);
    assert(pow2(ka) * pow2((kb - ka) as nat) == pow2((kb - ka) as nat) * pow2(ka)) by (nonlinear_arith);
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(pow2((kb - ka) as nat) as int, pow2(ka) as int);
}

/// A multiple of a multiple of `m` is a multiple of `m`.
proof fn lemma_mul_aligned(i: int, sz: int, m: int)
    requires
        m > 0,
        sz % m == 0,
    ensures
        (i * sz) % m == 0,
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(sz, m);
    let q = sz / m;
    assert(i * sz == (i * q) * m) by (nonlinear_arith)
        requires
            sz == m * q,
    ;
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(i * q, m);
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
    lemma_add_aligned(base@.addr as int, off as int, align_of::<T>() as int);
    axiom_ptr_add_addr(base as *mut u8, off);
    let tracked f = taken.into_typed::<T>(start as usize);
    assert(f.ptr()@ == field_ptr::<S, T>(base, off)@);
    (f, rest)
}

/// An array of `N` `T`s is `N` `T`s back to back, aligned as one: the
/// size and alignment the Rust reference gives arrays. Trusted (vstd
/// states no array layout). Only for an array whose elements fit in an
/// `isize`: a longer one has no layout (rustc rejects it as too big), yet
/// Verus still names its `size_of`, which vstd bounds by `isize::MAX`
/// (`layout_for_type_is_valid`), so without the `requires` the axiom at
/// `[u64; usize::MAX]` proves `false` (`verus/audit/library-control.md`).
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
        lemma_mul_aligned(i as int, sz, align_of::<T>() as int);
        lemma_add_aligned(base@.addr as int, i * sz, align_of::<T>() as int);
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
        N * size_of::<T>() <= isize::MAX,
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
/// ending by the next one's offset, the last by the size. Trusted, and
/// checked by the build: the `const` assertion after this block
/// evaluates the same conditions with `fits`.
axiom fn axiom_arena_layout()
    ensures
        field_fits::<Arena, u16>(A_INDEX, A_DECAY_NS as int),
        field_fits::<Arena, AtomicU64>(A_DECAY_NS, A_SETS as int),
        field_fits::<Arena, *const SetTable>(A_SETS, A_SETS_PERM as int),
        field_fits::<Arena, Tracked<&'static PointsTo<SetTable>>>(A_SETS_PERM, A_CLASSES as int),
        field_fits::<Arena, [ClassState; NCLASSES]>(A_CLASSES, A_LARGE as int),
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

    off % core::mem::align_of::<T>() == 0
        && core::mem::align_of::<S>() % core::mem::align_of::<T>() == 0
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
        && NCLASSES as u128 * core::mem::size_of::<ClassState>() as u128 <= isize::MAX as u128
        && fits::<Arena, AtomicU64>(A_LARGE, A_STATS)
        && fits::<Arena, Lock<ArenaStats>>(A_STATS, A_LAST_EXIT_PURGE)
        && fits::<Arena, AtomicU64>(A_LAST_EXIT_PURGE, core::mem::size_of::<Arena>())
        && core::mem::size_of::<Arena>() <= isize::MAX as usize
);

// `Arena::axiom_array_layout` at `split`'s instance, checked (`Shared`
// checks its own two).
const _: () = assert!(
    core::mem::size_of::<[ClassState; NCLASSES]>() == NCLASSES * core::mem::size_of::<ClassState>()
        && core::mem::align_of::<[ClassState; NCLASSES]>() == core::mem::align_of::<ClassState>()
);

verus! {

/// The slab set table's permission of a block of `raw` at `p`, when there
/// is a block (as `huge::desc_perm` for descriptors).
proof fn table_perm(tracked raw: PointsToRaw, p: *mut SetTable) -> (tracked r: Option<PointsTo<SetTable>>)
    ensures
        p@.addr != 0 && raw.is_range(p@.addr as int, vstd::layout::size_of::<SetTable>() as int)
            && raw.provenance() == p@.provenance && p@.addr as int % vstd::layout::align_of::<SetTable>() as int == 0
            ==> r is Some && r->0.ptr() == p && r->0.is_uninit(),
{
    if p@.addr != 0 && raw.is_range(p@.addr as int, vstd::layout::size_of::<SetTable>() as int)
        && raw.provenance() == p@.provenance && p@.addr as int % vstd::layout::align_of::<SetTable>() as int == 0 {
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
        let tracked classes = split_array(classes);
        lemma_field_cast::<Arena, [ClassState; NCLASSES], ClassState>(p, A_CLASSES);
        (ArenaParts { index, decay_ns, sets, sets_perm, large, stats, last_exit_purge }, classes, pad)
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
    /// with the permission to it. The table fits an object (the check is
    /// on constants, so the build folds it away).
    #[inline(always)]
    fn alloc_sets(meta: &Meta, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (*mut SetTable, Tracked<Option<PointsTo<SetTable>>>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_meta()),
        ensures
            r.0@.addr != 0 ==> r.1@ is Some && r.1@->0.ptr() == r.0 && r.1@->0.is_uninit(),
            *final(ord) == *old(ord),
    {
        let layout = core::alloc::Layout::new::<SetTable>();
        crate::vspec::check::check(
            layout.size() <= crate::meta::MAX_BINNED && layout.align() <= crate::meta::MAX_ALIGN,
            "arena: slab set table larger than an object",
        );
        let (p, Tracked(blk)) = meta.alloc_raw(layout, Tracked(ord));
        let tracked (raw, _) = blk;
        let p = p as *mut SetTable;
        let tracked perm = table_perm(raw, p);
        (p, Tracked(perm))
    }

    /// `sets_ptr` into the field and its frozen permission `perm` into
    /// `sets_perm`.
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
            ptr_mut_write(field_at(this, A_STATS), Tracked(&mut parts.stats), Lock::new(ArenaStats::empty(index), Ghost(rank_arena_stats(index as int))));
            ptr_mut_write(field_at(this, A_LAST_EXIT_PURGE), Tracked(&mut parts.last_exit_purge), AtomicU64::new(0));
        }
    }

    /// Initialise `this` in place as arena `index` over the class tables
    /// from `tables` on (their frozen permissions in `tperms`), through the
    /// permission to its storage in `slot`, which is handed back
    /// initialised. Returns `false` if the metadata region cannot hold the
    /// slab sets, leaving `this` unusable (and `slot` empty). Every
    /// `classes[c]` is written, with a state of class `c` (the type
    /// invariant), before `true` is returned.
    ///
    /// # Safety
    /// `this` must point to writable storage for an `Arena` that nothing
    /// else reads until this returns `true`.
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
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_meta()),
            old(slot).perm matches Some(p) && p.ptr() == this && p.is_uninit(),
            this@.addr + size_of::<Arena>() <= usize::MAX,
            regions.wf(),
            regions.separated(),
            *regions == process_regions(),
            crate::class::tables_ok(*tperms, tables),
        ensures
            r ==> (final(slot).perm matches Some(p) && p.ptr() == this && p.is_init() && p.value().spec_index() == index),
            *final(ord) == *old(ord),
    {
        let tracked (mut parts, mut classes, pad) = Arena::split(slot.perm.tracked_take());
        // SAFETY: the caller guarantees `this` is writable and unaliased;
        // every write goes through the permission to its field.
        unsafe {
            Arena::put_head(this, index, decay_ns, Tracked(&mut parts));
            let (sets_ptr, Tracked(perm)) = Arena::alloc_sets(meta, Tracked(ord));
            if sets_ptr.is_null() {
                return false;
            }
            // Ghost: the permissions to the class sets' slots, each taken
            // once, by its class's `ClassState::new`.
            let tracked mut slots: Map<(int, int), ZeroRaw> = Map::tracked_empty();
            let Tracked(sets_perm) = match SetTable::new(meta, regions, Ghost(index), Tracked(&mut slots), Tracked(ord)) {
                Some(sets) => {
                    let tracked mut perm = perm.tracked_unwrap();
                    ptr_mut_write(sets_ptr, Tracked(&mut perm), sets);
                    // Frozen: nothing writes or frees the table again; the
                    // arena and its class states' sets share it.
                    let tracked perm = vstd::modes::tracked_static_ref(perm);
                    Arena::put_sets(this, sets_ptr, Tracked(perm), Tracked(&mut parts.sets), Tracked(&mut parts.sets_perm));
                    Tracked(perm)
                },
                None => return false,
            };
            for c in 0..NCLASSES
                invariant
                    classes_at(classes, this),
                    elems_init(classes, 0, c as int),
                    forall|d: int| 0 <= d < c ==> (#[trigger] classes[d]).value().spec_class() == d,
                    forall|d: int| 0 <= d < c ==> (#[trigger] classes[d]).value().spec_arena() == index,
                    parts.index.is_init() && parts.index.value() == index,
                    forall|d: int, k: int| c <= d < NCLASSES && 0 <= k < SETS_PER_CLASS ==> #[trigger] sets_perm.value().holds_set(slots, d, k),
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
                            assert forall|d: int, k: int| c + 1 <= d < NCLASSES && 0 <= k < SETS_PER_CLASS implies #[trigger] sets_perm.value().holds_set(slots, d, k) by {
                                assert(sets_perm.value().holds_set(slots0, d, k));
                            }
                        }
                    },
                    None => return false,
                }
            }
            Arena::put_tail(this, index, Tracked(&mut parts));
        }
        proof {
            slot.perm = Some(axiom_arena_assemble(parts, classes, pad, this));
        }
        true
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
}

} // verus!

verus! {

impl Arena {
    /// `classes[c]` is the state of class `c` in this arena (its
    /// `spec_arena` is `index`: the class states are the arena's, which
    /// arena isolation needs). `init` builds the value in
    /// place, so no struct expression checks this: `init` proves that it
    /// writes every `classes[c]` with a state of class `c` and arena
    /// `index` (from
    /// `ClassState::new`'s postcondition over `tables[c]`, whose class is
    /// `c` by `init`'s precondition) before it returns `true`; its callers,
    /// `Shared::init` and `Arenas::build`, meet those preconditions.
    /// `sets_perm` is the initialised table at `sets`. Both are
    /// preconditions of `axiom_arena_assemble`, the trusted step that turns
    /// the written fields into the `Arena`.
    #[verifier::type_invariant]
    spec fn inv(self) -> bool {
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.classes[c].spec_class() == c
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.classes[c].spec_arena() == self.index
        &&& self.sets_perm@.ptr() == self.sets
        &&& self.sets_perm@.is_init()
        &&& self.stats.rank() == rank_arena_stats(self.index as int)
        &&& self.sets_perm@.value().rank() == rank_sets(self.index as int)
    }

    /// `Empty` bytes over every class (one lock hold per class that has
    /// any), for the cgroup policy's retention figure.
    pub fn empty_bytes(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: usize)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            r <= NCLASSES * 0x200_0000_0000_0000,
            *final(ord) == *old(ord),
    {
        let mut sum = 0usize;
        let mut c = 0;
        while c < NCLASSES
            invariant
                crate::sys::may_syscall(),
                c <= NCLASSES,
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
                sum <= c * 0x200_0000_0000_0000,
            decreases NCLASSES - c,
        {
            let cs = &self.classes[c];
            if cs.oldest_empty() != 0 {
                sum += cs.empty_bytes(Tracked(ord));
            }
            c += 1;
        }
        sum
    }

    /// Purge every `Empty` slab of every class whatever its age. Returns
    /// how many were purged. `left` gets the bytes discarded, nonzero iff
    /// a slab was purged, and every class of this arena, none with an
    /// `Empty` slab in the lock value its purge released unless it has
    /// several slabs per page ([`Left::clean`]).
    pub fn purge_all(&self, Tracked(left): Tracked<&mut Ghost<Left>>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: usize)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            r <= NCLASSES * 0x2000_0000_0000,
            *final(ord) == *old(ord),
            (r > 0) == (final(left)@.bytes > 0),
            final(left)@.arenas == set![self.spec_index() as int],
            final(left)@.covers(),
            !final(left)@.huge,
            final(left)@.clean(),
    {
        proof {
            use_type_invariant(self);
            *left = Ghost(
                Left {
                    bytes: 0,
                    arenas: set![self.spec_index() as int],
                    classes: Set::empty(),
                    slabs: ISet::empty(),
                    huge: false,
                    extents: ISet::empty(),
                },
            );
        }
        let mut sum = 0usize;
        let mut c = 0;
        while c < NCLASSES
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                c <= NCLASSES,
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
                sum <= c * 0x2000_0000_0000,
                (sum > 0) == (left@.bytes > 0),
                left@.arenas == set![self.spec_index() as int],
                forall|a: int, k: int| #[trigger] left@.classes.contains((a, k))
                    <==> a == self.spec_index() && 0 <= k < c,
                !left@.huge,
                left@.clean(),
            decreases NCLASSES - c,
        {
            let cs = &self.classes[c];
            cs.nslabs_fit();
            let tracked mut lc = Ghost(Left::none());
            sum += cs.purge_empty(u64::MAX, Tracked(&mut lc), Tracked(ord));
            proof {
                use_type_invariant(self);
                let l0 = left@;
                *left = Ghost(l0.join(lc@));
                assert forall|a: int, k: int| #[trigger] left@.classes.contains((a, k))
                    <==> a == self.spec_index() && 0 <= k < c + 1 by {
                    assert(l0.classes.contains((a, k)) <==> a == self.spec_index() && 0 <= k < c);
                }
                assert(left@.arenas =~= set![self.spec_index() as int]);
                assert forall|t: (int, int, int)| #[trigger] left@.slabs.contains(t) implies paged(t.1) by {
                    if l0.slabs.contains(t) {
                    } else {
                        assert(lc@.slabs.contains(t));
                    }
                }
                assert(left@.extents =~= ISet::<int>::empty());
            }
            c += 1;
        }
        sum
    }
}

} // verus!

verus! {

impl Arena {
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
        }

        *self.stats.lock(Tracked(ord), Tracked(&mut LockOrder::spare())) = *s;
    }

    /// The statistics lock, for the fork handlers (a leaf: never held
    /// while another lock is taken).
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

    /// Class state for `class`, without the bounds check the fast paths do
    /// not need (`class` comes from `class_of`).
    #[inline(always)]
    pub fn class(&self, class: usize) -> (r: &ClassState)
        requires
            class < NCLASSES,
        ensures
            r.spec_class() == class,
            *r == self.spec_class_state(class as int),
            r.spec_arena() == self.spec_index(),
    {
        proof {
            use_type_invariant(self);
        }
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of` only yields indices below `NCLASSES`.
        unsafe { self.classes.get_unchecked(class) }
    }

    /// The class state [`Arena::class`] returns.
    pub closed spec fn spec_class_state(&self, class: int) -> ClassState {
        self.classes[class]
    }

    /// The decay period in nanoseconds (`purge::NEVER` when purging is off).
    #[inline]
    pub fn decay_ns(&self) -> u64 {
        self.decay_ns.load(Ordering::Relaxed)
    }

    /// Change the decay period; `-1` turns purging off. Takes effect at the
    /// next periodic tick, or at once for a background thread, which is
    /// woken to recompute its deadline.
    pub fn set_decay_ms(&self, ms: i64)
        requires
            crate::sys::may_syscall(),
    {
        self.decay_ns
            .store(crate::purge::decay_ns_from_ms(ms), Ordering::Relaxed);
        crate::purge::note_decay_changed();
    }

    /// Decay step over every class (see `ClassState::decay`): the earliest
    /// next deadline, `None` when nothing is `Empty` or decay is off.
    pub fn decay(&self, now: u64, Tracked(ord): Tracked<&mut LockOrder>) -> Option<u64>
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        self.decay_with(now, self.decay_ns(), crate::policy::KEEP_ALL, Tracked(ord))
    }

    /// [`decay`](Arena::decay) with the period the cgroup policy gives
    /// this arena and its keep ratio for the retention cap (design 11);
    /// `NEVER` does nothing.
    pub fn decay_with(&self, now: u64, decay_ns: u64, keep16: u32, Tracked(ord): Tracked<&mut LockOrder>) -> Option<u64>
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        if decay_ns == crate::purge::NEVER {
            return None;
        }
        let mut next: Option<u64> = None;
        for cs in self.classes.iter()
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
        {
            if let Some(d) = cs.decay_with(now, decay_ns, keep16, Tracked(ord)) {
                next = Some(next.map_or(d, |n| n.min(d)));
            }
        }
        next
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

verus! {

/// The arena registry. Index 0 is `Shared::arena0`; the rest live in the
/// metadata region. `count` is the number of indices in use: the automatic
/// ones from init plus every `create` since, published with a release
/// store once the arena is usable (automatic arenas may still be
/// unmaterialised, see [`Arenas::get`]).
pub struct Arenas {
    /// Creation lock; the value is `count` for the writer.
    lock: Lock<u32>,
    count: AtomicU32,
    /// Indices below this are automatic (`narenas`).
    auto: u32,
    /// Thread heaps currently homed on each arena (`assign`/`bind` add,
    /// `unbind` subtracts); explicit caches are not counted.
    bound: [AtomicU32; MAX_ARENAS],
    table: [AtomicPtr<Arena>; MAX_ARENAS],
    /// Ghost (no bytes): the invariant of each bound count
    /// ([`BoundPred`]), one per index ([`Arenas::wf`]).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    invs: Tracked<Map<int, BoundInv>>,
    /// Ghost (no bytes): the invariant of `count` ([`NarenasPred`];
    /// the type invariant names it).
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    count_inv: Tracked<NarenasInv>,
}

/// Namespace of the bound-count invariants.
pub open spec fn bound_ns() -> int {
    0x626e_6463
}

/// Namespace of the arena count's invariant ([`NarenasPred`]).
pub open spec fn narenas_ns() -> int {
    0x6e61_726e
}

/// The arena count's invariant, about the counter whose [`count_id`] is
/// the key: its permission's value is at most [`MAX_ARENAS`]. Its two
/// writers keep it: `Arenas::init` stores the automatic count it clamped
/// to `1..=MAX_ARENAS` (with the permission `vspec::count::new` minted for
/// it), and `Arenas::create` stores `index + 1` for an `index` below `MAX_ARENAS`.
pub struct NarenasPred;

impl InvariantPredicate<int, CountPerm> for NarenasPred {
    open spec fn inv(k: int, v: CountPerm) -> bool {
        &&& v.id() == k
        &&& v.value() <= MAX_ARENAS
    }
}

/// The invariant of the arena count.
pub type NarenasInv = AtomicInvariant<int, CountPerm, NarenasPred>;

/// What one bound count is about: the counter ([`count_id`]) and the
/// ghost set of the tickets of the heaps homed on its arena.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost struct BoundKey {
    pub id: int,
    pub loc: Loc,
}

/// A thread heap's ticket for arena `arena`: `tok` is one element of the
/// arena's ticket set ([`BoundPred`]). Handed out by [`Arenas::assign`]
/// and [`Arenas::bind`] and used up by [`Arenas::unbind`]; a thread heap
/// holds the one for its home arena while it is counted there
/// (`Heap::homed_ok`).
#[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
pub tracked struct Homed {
    pub tracked tok: GhostSingleton<int>,
    pub ghost arena: int,
}

/// `t` is a ticket for arena `i` of the process's registry
/// ([`bound_loc`]).
pub(crate) open spec fn homed_on(t: Homed, i: int) -> bool {
    &&& t.arena == i
    &&& 0 <= i < MAX_ARENAS
    &&& t.tok.id() == bound_loc(i)
}

/// What a bound count's invariant holds: the counter's permission, the
/// authority over the arena's tickets, and a bound above every ticket
/// (where the next fresh one comes from).
pub type BoundVal = (CountPerm, GhostSetAuth<int>, Ghost<int>);

/// A bound count's invariant: the counter is the number of tickets out
/// for its arena, as a `u32` wraps it ([`wrap32`]).
pub struct BoundPred;

impl InvariantPredicate<BoundKey, BoundVal> for BoundPred {
    open spec fn inv(k: BoundKey, v: BoundVal) -> bool {
        &&& v.0.id() == k.id
        &&& v.1.id() == k.loc
        &&& forall|t: int| #[trigger] v.1@.contains(t) ==> t < v.2@
        &&& v.0.value() == wrap32(v.1@.len() as int)
    }
}

/// The invariant of one bound count.
pub type BoundInv = AtomicInvariant<BoundKey, BoundVal, BoundPred>;

/// The ticket set of arena `i` of the process's registry: a heap holding
/// a [`Homed`] of this location is counted in `bound[i]` of the published
/// `Shared` (`Shared::wf` states that its registry's locations are these).
pub(crate) open spec fn bound_loc(i: int) -> Loc {
    Shared::built_at(Shared::storage()).arenas.spec_loc(i)
}

/// The order [`Arenas::assign`] picks by: fewer bound heaps first, then
/// built (`false`) before unbuilt; `(u32, bool)`'s lexicographic `<`.
pub open spec fn key_lt(a: (u32, bool), b: (u32, bool)) -> bool {
    a.0 < b.0 || (a.0 == b.0 && !a.1 && b.1)
}

/// `r` indexes the first least key of `snap` ([`key_lt`]): no key is below
/// it, and every key before it is above it.
pub open spec fn least_of(snap: Seq<(u32, bool)>, r: int) -> bool {
    &&& 0 <= r < snap.len()
    &&& forall|j: int| 0 <= j < snap.len() ==> !key_lt(#[trigger] snap[j], snap[r])
    &&& forall|j: int| 0 <= j < r ==> key_lt(snap[r], #[trigger] snap[j])
}

/// `a < b` on `(u32, bool)`: [`key_lt`]. Trusted: `core`'s tuple
/// `PartialOrd` is lexicographic and `false < true`, as `core` documents;
/// vstd specifies neither `bool`'s order nor, so, this tuple's. The body
/// is the bare comparison (a verified spelling of it,
/// `if a.0 != b.0 { a.0 < b.0 } else { ... }`, changed
/// `create_heap_for_thread`'s machine code on x86_64).
#[inline(always)]
#[verifier::external_body]
fn key_less(a: (u32, bool), b: (u32, bool)) -> (r: bool)
    ensures
        r == key_lt(a, b),
{
    a < b
}

/// What [`Arenas::assign`] returns over `snap`, the `(bound, unbuilt)`
/// keys it read for indices `0..snap.len()`, in order, under the creation
/// lock: the first least of them ([`least_of`]: ties go to the lowest
/// index). Indices from `snap.len()` to `auto` were not read only because
/// `r`'s key is `(0, false)`, the least key there is.
pub open spec fn picks_least(snap: Seq<(u32, bool)>, auto: int, r: int) -> bool {
    &&& least_of(snap, r)
    &&& snap.len() <= auto
    &&& snap.len() < auto ==> snap[r] == (0u32, false)
}

} // verus!

verus! {

// Byte offsets of `Arenas`'s fields, opaque to Verus as `Arena`'s are;
// `axiom_arenas_layout` states what verified code relies on, and the
// build checks it.
#[verifier::external_body]
const AR_TABLE: usize = core::mem::offset_of!(Arenas, table);
#[verifier::external_body]
const AR_BOUND: usize = core::mem::offset_of!(Arenas, bound);
#[verifier::external_body]
const AR_LOCK: usize = core::mem::offset_of!(Arenas, lock);
#[verifier::external_body]
const AR_COUNT: usize = core::mem::offset_of!(Arenas, count);
#[verifier::external_body]
const AR_AUTO: usize = core::mem::offset_of!(Arenas, auto);
#[verifier::external_body]
const AR_INVS: usize = core::mem::offset_of!(Arenas, invs);
#[verifier::external_body]
const AR_COUNT_INV: usize = core::mem::offset_of!(Arenas, count_inv);

/// `Arenas`'s layout: each field at its offset, aligned, and ending by
/// the next one's offset, the last (the zero-sized ghost `count_inv`,
/// after `invs`) by the size. `Arenas` is not `repr(C)`, so the
/// order is the one rustc picks (the table, the bound counts, then the
/// rest in declaration order).
/// Trusted, and checked by the build: the `const` assertion after this
/// block evaluates the same conditions with `fits`, so a compiler that
/// orders the fields otherwise fails the build there.
axiom fn axiom_arenas_layout()
    ensures
        field_fits::<Arenas, [AtomicPtr<Arena>; MAX_ARENAS]>(AR_TABLE, AR_BOUND as int),
        MAX_ARENAS * size_of::<AtomicPtr<Arena>>() <= isize::MAX,
        field_fits::<Arenas, [AtomicU32; MAX_ARENAS]>(AR_BOUND, AR_LOCK as int),
        MAX_ARENAS * size_of::<AtomicU32>() <= isize::MAX,
        // Over several lines, as in `axiom_arena_layout`.
        field_fits::<
            Arenas,
            Lock<u32>,
        >(AR_LOCK, AR_COUNT as int),
        field_fits::<Arenas, AtomicU32>(AR_COUNT, AR_AUTO as int),
        field_fits::<Arenas, u32>(AR_AUTO, AR_INVS as int),
        field_fits::<Arenas, Tracked<Map<int, BoundInv>>>(AR_INVS, AR_COUNT_INV as int),
        field_fits::<Arenas, Tracked<NarenasInv>>(AR_COUNT_INV, size_of::<Arenas>() as int),
        size_of::<Arenas>() <= isize::MAX,
;

} // verus!

// `axiom_arenas_layout`, checked.
const _: () = assert!(
    fits::<Arenas, [AtomicPtr<Arena>; MAX_ARENAS]>(AR_TABLE, AR_BOUND)
        && MAX_ARENAS as u128 * core::mem::size_of::<AtomicPtr<Arena>>() as u128
            <= isize::MAX as u128
        && fits::<Arenas, [AtomicU32; MAX_ARENAS]>(AR_BOUND, AR_LOCK)
        && MAX_ARENAS as u128 * core::mem::size_of::<AtomicU32>() as u128 <= isize::MAX as u128
        && fits::<Arenas, Lock<u32>>(AR_LOCK, AR_COUNT)
        && fits::<Arenas, AtomicU32>(AR_COUNT, AR_AUTO)
        && fits::<Arenas, u32>(AR_AUTO, AR_INVS)
        && fits::<Arenas, Tracked<Map<int, BoundInv>>>(AR_INVS, AR_COUNT_INV)
        && fits::<Arenas, Tracked<NarenasInv>>(AR_COUNT_INV, core::mem::size_of::<Arenas>())
        && core::mem::size_of::<Arenas>() <= isize::MAX as usize
);

// `Arena::axiom_array_layout` at `Arenas::split`'s two instances, checked.
const _: () = assert!(
    core::mem::size_of::<[AtomicPtr<Arena>; MAX_ARENAS]>()
        == MAX_ARENAS * core::mem::size_of::<AtomicPtr<Arena>>()
        && core::mem::align_of::<[AtomicPtr<Arena>; MAX_ARENAS]>()
            == core::mem::align_of::<AtomicPtr<Arena>>()
        && core::mem::size_of::<[AtomicU32; MAX_ARENAS]>()
            == MAX_ARENAS * core::mem::size_of::<AtomicU32>()
        && core::mem::align_of::<[AtomicU32; MAX_ARENAS]>() == core::mem::align_of::<AtomicU32>()
);

verus! {

/// The creation lock (`Arenas` holds the only `Lock<u32>` outside the
/// tests) has no invariant: every value satisfies it, so its holders owe
/// nothing and learn nothing from it. `Arenas::init`'s `Lock::new` needs
/// it of the automatic count. The bound on the count is not stated here:
/// the arena count's own invariant ([`NarenasPred`]) carries it. Trusted
/// only because `lock_inv` is uninterpreted.
pub broadcast axiom fn axiom_arenas_lock_inv(v: u32)
    ensures
        #[trigger] lock_inv(v),
;

/// The permissions to the fields of an `Arenas` under construction but
/// its two arrays, which are maps of their own, element by element
/// ([`Arenas::split`]).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
tracked struct ArenasParts {
    tracked lock: PointsTo<Lock<u32>>,
    tracked count: PointsTo<AtomicU32>,
    tracked auto: PointsTo<u32>,
    tracked invs: PointsTo<Tracked<Map<int, BoundInv>>>,
    tracked count_inv: PointsTo<Tracked<NarenasInv>>,
}

impl ArenasParts {
    /// Each permission is to its field of the `Arenas` at `p`, which is
    /// [`storage_ok`], and the two arrays are inside it.
    spec fn at(&self, p: *mut Arenas) -> bool {
        &&& storage_ok(p)
        &&& self.lock.ptr() == field_ptr::<Arenas, Lock<u32>>(p, AR_LOCK)
        &&& self.count.ptr() == field_ptr::<Arenas, AtomicU32>(p, AR_COUNT)
        &&& self.auto.ptr() == field_ptr::<Arenas, u32>(p, AR_AUTO)
        &&& self.invs.ptr() == field_ptr::<Arenas, Tracked<Map<int, BoundInv>>>(p, AR_INVS)
        &&& self.count_inv.ptr() == field_ptr::<Arenas, Tracked<NarenasInv>>(p, AR_COUNT_INV)
        &&& in_alloc(field_ptr::<Arenas, AtomicPtr<Arena>>(p, AR_TABLE), MAX_ARENAS as int * size_of::<AtomicPtr<Arena>>())
        &&& MAX_ARENAS * size_of::<AtomicPtr<Arena>>() <= isize::MAX
        &&& in_alloc(field_ptr::<Arenas, AtomicU32>(p, AR_BOUND), MAX_ARENAS as int * size_of::<AtomicU32>())
        &&& MAX_ARENAS * size_of::<AtomicU32>() <= isize::MAX
    }
}

/// The table entry permissions of the `Arenas` at `p`, element by element.
spec fn tables_at(m: Map<int, PointsTo<AtomicPtr<Arena>>>, p: *mut Arenas) -> bool {
    elems_at(m, field_ptr::<Arenas, AtomicPtr<Arena>>(p, AR_TABLE), 0, MAX_ARENAS as int)
}

/// The bound count permissions of the `Arenas` at `p`, element by element.
spec fn bounds_at(m: Map<int, PointsTo<AtomicU32>>, p: *mut Arenas) -> bool {
    elems_at(m, field_ptr::<Arenas, AtomicU32>(p, AR_BOUND), 0, MAX_ARENAS as int)
}

/// Byte `x` is in the field of type `T` at offset `off` of the `Arenas`
/// at `p`.
spec fn in_arenas_field<T>(x: int, p: *mut Arenas, off: usize) -> bool {
    p@.addr + off <= x < p@.addr + off + size_of::<T>()
}

/// Every byte of the `Arenas` at `p` is `pad`'s or a field's: what
/// [`Arenas::split`] keeps of the storage once it has taken the fields
/// (the padding), which [`axiom_arenas_assemble`] takes back.
spec fn arenas_pad_covers(pad: PointsToRaw, p: *mut Arenas) -> bool {
    forall|x: int|
        p@.addr <= x < p@.addr + size_of::<Arenas>() ==> #[trigger] pad.dom().contains(x)
            || in_arenas_field::<[AtomicPtr<Arena>; MAX_ARENAS]>(x, p, AR_TABLE)
            || in_arenas_field::<[AtomicU32; MAX_ARENAS]>(x, p, AR_BOUND)
            || in_arenas_field::<Lock<u32>>(x, p, AR_LOCK)
            || in_arenas_field::<AtomicU32>(x, p, AR_COUNT)
            || in_arenas_field::<u32>(x, p, AR_AUTO)
            || in_arenas_field::<Tracked<Map<int, BoundInv>>>(x, p, AR_INVS)
            || in_arenas_field::<Tracked<NarenasInv>>(x, p, AR_COUNT_INV)
}

impl Arenas {
    /// The permissions to the fields of the uninitialised `Arenas` `perm`
    /// is to: [`take_field`] for each field at the offsets
    /// [`axiom_arenas_layout`] gives, and the two arrays split by
    /// [`split_array`]; and the bytes left over, the padding.
    proof fn split(tracked perm: PointsTo<Arenas>) -> (tracked r: (
        ArenasParts,
        Map<int, PointsTo<AtomicPtr<Arena>>>,
        Map<int, PointsTo<AtomicU32>>,
        PointsToRaw,
    ))
        requires
            perm.is_uninit(),
            perm.ptr()@.addr + size_of::<Arenas>() <= usize::MAX,
        ensures
            r.0.at(perm.ptr()),
            r.0.lock.is_uninit(),
            r.0.count.is_uninit(),
            r.0.auto.is_uninit(),
            r.0.invs.is_uninit(),
            r.0.count_inv.is_uninit(),
            tables_at(r.1, perm.ptr()),
            elems_uninit(r.1, 0, MAX_ARENAS as int),
            bounds_at(r.2, perm.ptr()),
            elems_uninit(r.2, 0, MAX_ARENAS as int),
            arenas_pad_covers(r.3, perm.ptr()),
            r.3.provenance() == perm.ptr()@.provenance,
    {
        broadcast use vstd::raw_ptr::group_raw_ptr_axioms, vstd::layout::layout_of_primitives;

        axiom_arenas_layout();
        let p = perm.ptr();
        perm.is_nonnull();
        perm.is_aligned();
        let tracked raw = perm.into_raw();
        axiom_raw_valid(&raw, p as *mut u8, size_of::<Arenas>() as int);
        let tracked (table, raw) = take_field::<Arenas, [AtomicPtr<Arena>; MAX_ARENAS]>(raw, p, AR_TABLE, AR_BOUND as int);
        let tracked (bound, raw) = take_field::<Arenas, [AtomicU32; MAX_ARENAS]>(raw, p, AR_BOUND, AR_LOCK as int);
        let tracked (lock, raw) = take_field::<Arenas, Lock<u32>>(raw, p, AR_LOCK, AR_COUNT as int);
        let tracked (count, raw) = take_field::<Arenas, AtomicU32>(raw, p, AR_COUNT, AR_AUTO as int);
        let tracked (auto, raw) = take_field::<Arenas, u32>(raw, p, AR_AUTO, AR_INVS as int);
        let tracked (invs, raw) = take_field::<Arenas, Tracked<Map<int, BoundInv>>>(raw, p, AR_INVS, AR_COUNT_INV as int);
        let tracked (count_inv, pad) = take_field::<Arenas, Tracked<NarenasInv>>(
            raw,
            p,
            AR_COUNT_INV,
            size_of::<Arenas>() as int,
        );
        axiom_array_layout::<AtomicPtr<Arena>, MAX_ARENAS>();
        axiom_array_layout::<AtomicU32, MAX_ARENAS>();
        let tracked tables = split_array(table);
        let tracked bounds = split_array(bound);
        lemma_field_cast::<Arenas, [AtomicPtr<Arena>; MAX_ARENAS], AtomicPtr<Arena>>(p, AR_TABLE);
        lemma_field_cast::<Arenas, [AtomicU32; MAX_ARENAS], AtomicU32>(p, AR_BOUND);
        (ArenasParts { lock, count, auto, invs, count_inv }, tables, bounds, pad)
    }
}

/// The written fields of the `Arenas` at `p` (`Arenas::split`'s parts
/// and array elements, every one initialised), as the `Arenas`: the value
/// of each field is what its permission holds. Trusted, as
/// [`axiom_arena_assemble`] is for `Arena`: vstd has no way to join typed
/// permissions; the permissions cover every field at its offset, and
/// `pad` every byte between and around them ([`arenas_pad_covers`]), so
/// their bytes are the `Arenas`'s. The type invariant
/// (`Arenas::type_inv`) is a precondition, so the value is one an
/// `Arenas` may hold.
axiom fn axiom_arenas_assemble(
    tracked parts: ArenasParts,
    tracked tables: Map<int, PointsTo<AtomicPtr<Arena>>>,
    tracked bounds: Map<int, PointsTo<AtomicU32>>,
    tracked pad: PointsToRaw,
    p: *mut Arenas,
) -> (tracked a: PointsTo<Arenas>)
    requires
        parts.at(p),
        tables_at(tables, p),
        bounds_at(bounds, p),
        arenas_pad_covers(pad, p),
        pad.provenance() == p@.provenance,
        parts.lock.is_init(),
        parts.count.is_init(),
        parts.auto.is_init(),
        parts.invs.is_init(),
        parts.count_inv.is_init(),
        elems_init(tables, 0, MAX_ARENAS as int),
        elems_init(bounds, 0, MAX_ARENAS as int),
        parts.lock.value().rank() == rank_arenas(),
        parts.count_inv.value()@.constant() == count_id(&parts.count.value()),
        parts.count_inv.value()@.namespace() == narenas_ns(),
    ensures
        a.ptr() == p,
        a.is_init(),
        a.value().lock == parts.lock.value(),
        a.value().count == parts.count.value(),
        a.value().auto == parts.auto.value(),
        forall|i: int| 0 <= i < MAX_ARENAS ==> #[trigger] a.value().table[i] == tables[i].value(),
        forall|i: int| 0 <= i < MAX_ARENAS ==> #[trigger] a.value().bound[i] == bounds[i].value(),
        a.value().invs == parts.invs.value(),
        a.value().count_inv == parts.count_inv.value(),
;

/// The set `{0}`, the one element each fresh ticket set starts with to
/// tell its location from the others' ([`fresh_sets`]).
spec fn just0() -> Set<int> {
    Set::empty().insert(0int)
}

/// `f` is at a location none of `frags[0..k]` is at: each holds `0`, as
/// `f` does, and two subsets at one location hold disjoint sets.
proof fn lemma_apart(tracked f: &mut GhostSubset<int>, tracked frags: &Map<int, GhostSubset<int>>, k: nat)
    requires
        old(f)@ == just0(),
        forall|j: int| 0 <= j < k ==> #[trigger] frags.contains_key(j),
        forall|j: int| 0 <= j < k ==> #[trigger] frags[j]@ == just0(),
    ensures
        final(f).id() == old(f).id(),
        final(f)@ == old(f)@,
        forall|j: int| 0 <= j < k ==> #[trigger] frags[j].id() != final(f).id(),
    decreases k,
{
    if k > 0 {
        lemma_apart(f, frags, (k - 1) as nat);
        let j = k - 1;
        if f.id() == frags[j].id() {
            f.disjoint(frags.tracked_borrow(j));
            assert(f@.contains(0int));
            assert(frags[j]@.contains(0int));
        }
    }
}

/// `n` ticket set authorities, each at a location of its own, holding
/// `{0}`, with the subset of `{0}` that shows it ([`lemma_apart`]).
proof fn fresh_sets(n: nat) -> (tracked r: (Map<int, GhostSetAuth<int>>, Map<int, GhostSubset<int>>))
    ensures
        forall|i: int| 0 <= i < n ==> #[trigger] r.0.contains_key(i),
        forall|i: int| 0 <= i < n ==> #[trigger] r.1.contains_key(i),
        forall|i: int| 0 <= i < n ==> #[trigger] r.0[i].id() == r.1[i].id(),
        forall|i: int| 0 <= i < n ==> #[trigger] r.0[i]@ == just0(),
        forall|i: int| 0 <= i < n ==> #[trigger] r.1[i]@ == just0(),
        forall|i: int, j: int| 0 <= i < n && 0 <= j < n && i != j ==> #[trigger] r.0[i].id() != #[trigger] r.0[j].id(),
    decreases n,
{
    if n == 0 {
        (Map::tracked_empty(), Map::tracked_empty())
    } else {
        let k = (n - 1) as nat;
        let tracked (mut auths, mut frags) = fresh_sets(k);
        let tracked (auth, mut frag) = GhostSetAuth::new(just0());
        lemma_apart(&mut frag, &frags, k);
        let ghost (a0, f0) = (auths, frags);
        auths.tracked_insert(k as int, auth);
        frags.tracked_insert(k as int, frag);
        assert forall|i: int| 0 <= i < k implies #[trigger] auths[i] == a0[i] by {}
        assert forall|i: int| 0 <= i < k implies #[trigger] frags[i] == f0[i] by {}
        (auths, frags)
    }
}

/// The invariants of `n` fresh bound counts ([`BoundPred`]): count `i`'s
/// permission (value 0) from `counts`, its ticket set from `auths`, made
/// empty by taking back the subset `{0}` from `frags`, and no ticket.
proof fn fresh_invs(
    tracked counts: Map<int, CountPerm>,
    tracked auths: Map<int, GhostSetAuth<int>>,
    tracked frags: Map<int, GhostSubset<int>>,
    n: nat,
) -> (tracked m: Map<int, BoundInv>)
    requires
        forall|i: int| 0 <= i < n ==> #[trigger] counts.contains_key(i),
        forall|i: int| 0 <= i < n ==> #[trigger] counts[i].value() == 0,
        forall|i: int| 0 <= i < n ==> #[trigger] auths.contains_key(i),
        forall|i: int| 0 <= i < n ==> #[trigger] frags.contains_key(i),
        forall|i: int| 0 <= i < n ==> #[trigger] auths[i].id() == frags[i].id(),
        forall|i: int| 0 <= i < n ==> #[trigger] auths[i]@ == just0(),
        forall|i: int| 0 <= i < n ==> #[trigger] frags[i]@ == just0(),
    ensures
        forall|i: int| 0 <= i < n ==> #[trigger] m.contains_key(i),
        forall|i: int| 0 <= i < n ==> #[trigger] m[i].constant() == (BoundKey { id: counts[i].id(), loc: auths[i].id() }),
        forall|i: int| 0 <= i < n ==> #[trigger] m[i].namespace() == bound_ns(),
    decreases n,
{
    if n == 0 {
        Map::tracked_empty()
    } else {
        let i = n - 1;
        let ghost (c0, a0, f0) = (counts, auths, frags);
        let tracked mut counts = counts;
        let tracked mut auths = auths;
        let tracked mut frags = frags;
        let tracked c = counts.tracked_remove(i);
        let tracked mut a = auths.tracked_remove(i);
        let tracked f = frags.tracked_remove(i);
        a.delete(f);
        assert(a@ =~= Set::<int>::empty());
        let k = BoundKey { id: c.id(), loc: a.id() };
        assert(wrap32(0) == 0u32);
        let tracked inv = BoundInv::new(k, (c, a, Ghost(0int)), bound_ns());
        assert forall|j: int| 0 <= j < i implies #[trigger] counts[j] == c0[j] by {}
        assert forall|j: int| 0 <= j < i implies #[trigger] auths[j] == a0[j] by {}
        assert forall|j: int| 0 <= j < i implies #[trigger] frags[j] == f0[j] by {}
        let tracked mut m = fresh_invs(counts, auths, frags, i as nat);
        let ghost m0 = m;
        m.tracked_insert(i, inv);
        assert forall|j: int| 0 <= j < i implies m[j] == m0[j] by {}
        m
    }
}

/// The in-place writes of [`Arenas::init`], each through the permission
/// to the field it writes.
impl Arenas {
    /// `lock`, `count` and `auto`, from the automatic count `auto`; the
    /// count's permission.
    #[inline(always)]
    unsafe fn put_head(this: *mut Arenas, auto: u32, Tracked(parts): Tracked<&mut ArenasParts>) -> (r: Tracked<CountPerm>)
        requires
            old(parts).at(this),
            auto as usize <= MAX_ARENAS,
        ensures
            final(parts).at(this),
            final(parts).lock.is_init(),
            final(parts).lock.value().rank() == rank_arenas(),
            final(parts).count.is_init(),
            final(parts).auto.is_init(),
            final(parts).auto.value() == auto,
            final(parts).invs == old(parts).invs,
            final(parts).count_inv == old(parts).count_inv,
            r@.id() == count_id(&final(parts).count.value()),
            r@.value() == auto,
    {
        broadcast use axiom_arenas_lock_inv;

        proof {
            axiom_arenas_layout();
        }
        // SAFETY: the fields of `init`'s storage, through their permissions.
        unsafe {
            ptr_mut_write(field_at(this, AR_LOCK), Tracked(&mut parts.lock), Lock::new(auto, Ghost(rank_arenas())));
            let (c, perm) = crate::vspec::count::new(auto);
            ptr_mut_write(field_at(this, AR_COUNT), Tracked(&mut parts.count), c);
            ptr_mut_write(field_at(this, AR_AUTO), Tracked(&mut parts.auto), auto);
            perm
        }
    }

    /// Entry `i` of `table`, null, and of `bound`, a new count of 0,
    /// through the permissions `t` and `b` to them; the count's
    /// permission.
    #[inline(always)]
    unsafe fn put_entry(
        this: *mut Arenas,
        i: usize,
        Tracked(t): Tracked<&mut PointsTo<AtomicPtr<Arena>>>,
        Tracked(b): Tracked<&mut PointsTo<AtomicU32>>,
    ) -> (r: Tracked<CountPerm>)
        requires
            storage_ok(this),
            i < MAX_ARENAS,
            old(t).ptr() == ptr_add(field_ptr::<Arenas, AtomicPtr<Arena>>(this, AR_TABLE), i),
            old(b).ptr() == ptr_add(field_ptr::<Arenas, AtomicU32>(this, AR_BOUND), i),
            in_alloc(field_ptr::<Arenas, AtomicPtr<Arena>>(this, AR_TABLE), MAX_ARENAS as int * size_of::<AtomicPtr<Arena>>()),
            MAX_ARENAS * size_of::<AtomicPtr<Arena>>() <= isize::MAX,
            in_alloc(field_ptr::<Arenas, AtomicU32>(this, AR_BOUND), MAX_ARENAS as int * size_of::<AtomicU32>()),
            MAX_ARENAS * size_of::<AtomicU32>() <= isize::MAX,
        ensures
            final(t).ptr() == old(t).ptr(),
            final(t).is_init(),
            final(b).ptr() == old(b).ptr(),
            final(b).is_init(),
            r@.id() == count_id(&final(b).value()),
            r@.value() == 0,
    {
        proof {
            axiom_arenas_layout();
        }
        // SAFETY: element `i` of each array field, through its permission.
        unsafe {
            let table: *mut AtomicPtr<Arena> = field_at(this, AR_TABLE);
            let bound: *mut AtomicU32 = field_at(this, AR_BOUND);
            ptr_mut_write(elem_at(table, i, MAX_ARENAS), Tracked(t), AtomicPtr::new(ptr::null_mut()));
            let (c, perm) = crate::vspec::count::new(0);
            ptr_mut_write(elem_at(bound, i, MAX_ARENAS), Tracked(b), c);
            perm
        }
    }

    /// `arena0` into entry 0 of `table`, through the permission `t` to it.
    #[inline(always)]
    unsafe fn put_arena0(this: *mut Arenas, arena0: *mut Arena, Tracked(t): Tracked<&PointsTo<AtomicPtr<Arena>>>)
        requires
            storage_ok(this),
            t.ptr() == ptr_add(field_ptr::<Arenas, AtomicPtr<Arena>>(this, AR_TABLE), 0),
            t.is_init(),
            in_alloc(field_ptr::<Arenas, AtomicPtr<Arena>>(this, AR_TABLE), MAX_ARENAS as int * size_of::<AtomicPtr<Arena>>()),
            MAX_ARENAS * size_of::<AtomicPtr<Arena>>() <= isize::MAX,
    {
        proof {
            axiom_arenas_layout();
        }
        // SAFETY: element 0 of the field, through its permission.
        unsafe {
            let table: *mut AtomicPtr<Arena> = field_at(this, AR_TABLE);
            ptr_ref(elem_at(table, 0, MAX_ARENAS), Tracked(t)).store(arena0, Ordering::Release);
        }
    }

    /// The ghost `invs` and `count_inv` (no bytes), through the
    /// permissions to them.
    #[inline(always)]
    unsafe fn put_invs(
        this: *mut Arenas,
        invs: Tracked<Map<int, BoundInv>>,
        count_inv: Tracked<NarenasInv>,
        Tracked(parts): Tracked<&mut ArenasParts>,
    )
        requires
            old(parts).at(this),
        ensures
            final(parts).at(this),
            final(parts).invs.is_init(),
            final(parts).invs.value() == invs,
            final(parts).count_inv.is_init(),
            final(parts).count_inv.value() == count_inv,
            final(parts).lock == old(parts).lock,
            final(parts).count == old(parts).count,
            final(parts).auto == old(parts).auto,
    {
        proof {
            axiom_arenas_layout();
        }
        // SAFETY: the fields of `init`'s storage, through their permissions.
        unsafe {
            ptr_mut_write(field_at(this, AR_INVS), Tracked(&mut parts.invs), invs);
            ptr_mut_write(field_at(this, AR_COUNT_INV), Tracked(&mut parts.count_inv), count_inv);
        }
    }
}

} // verus!

verus! {

impl Arenas {
    /// Initialise `this` in place with `arena0` at index 0 and `narenas`
    /// automatic indices (clamped to `1..=MAX_ARENAS`), through the
    /// permission to its storage in `slot`, handed back initialised and
    /// well formed ([`Arenas::wf`]): every bound count is written 0, and
    /// its invariant ([`BoundPred`]) holds the permission
    /// `vspec::count::new` minted for it and a fresh, empty ticket set, each
    /// set at a location of its own; no ticket exists yet. Likewise the
    /// ghost `count_inv`: `count` is written the clamped automatic count,
    /// and its invariant ([`NarenasPred`], in [`narenas_ns`]) holds the
    /// permission `vspec::count::new` minted for it.
    ///
    /// # Safety
    /// `this` must point to writable storage that nothing else reads until
    /// this returns; `arena0` must outlive the registry.
    #[verifier::loop_isolation(false)]
    pub unsafe fn init(this: *mut Arenas, arena0: *mut Arena, narenas: u32, Tracked(slot): Tracked<&mut Held<Arenas>>)
        requires
            old(slot).perm matches Some(p) && p.ptr() == this && p.is_uninit(),
            this@.addr + size_of::<Arenas>() <= usize::MAX,
        ensures
            final(slot).perm matches Some(p) && p.ptr() == this && p.is_init() && p.value().wf(),
    {
        let auto = narenas.clamp(1, MAX_ARENAS as u32);
        let tracked (mut parts, mut tables, mut bounds, pad) = Arenas::split(slot.perm.tracked_take());
        // Ghost: each bound count's permission, as `put_entry` mints it.
        let tracked mut counts: Map<int, CountPerm> = Map::tracked_empty();
        // SAFETY: the caller guarantees `this` is writable and unaliased;
        // every write goes through the permission to its field.
        unsafe {
            let Tracked(nperm) = Arenas::put_head(this, auto, Tracked(&mut parts));
            for i in 0..MAX_ARENAS
                invariant
                    tables_at(tables, this),
                    bounds_at(bounds, this),
                    elems_init(tables, 0, i as int),
                    elems_init(bounds, 0, i as int),
                    forall|j: int| 0 <= j < i ==> #[trigger] counts.contains_key(j),
                    forall|j: int| 0 <= j < i ==> #[trigger] counts[j].id() == count_id(&bounds[j].value()),
                    forall|j: int| 0 <= j < i ==> #[trigger] counts[j].value() == 0,
            {
                let ghost (t0, b0, c0) = (tables, bounds, counts);
                proof {
                    assert(tables.contains_key(i as int));
                    assert(bounds.contains_key(i as int));
                }
                let tracked t = tables.tracked_borrow_mut(i as int);
                let tracked b = bounds.tracked_borrow_mut(i as int);
                let Tracked(c) = Arenas::put_entry(this, i, Tracked(t), Tracked(b));
                proof {
                    counts.tracked_insert(i as int, c);
                    assert forall|j: int| 0 <= j < MAX_ARENAS implies #[trigger] tables.contains_key(j) by {
                        assert(t0.contains_key(j));
                    }
                    assert forall|j: int| 0 <= j < MAX_ARENAS implies (#[trigger] tables[j]).ptr()
                        == ptr_add(field_ptr::<Arenas, AtomicPtr<Arena>>(this, AR_TABLE), j as usize) by {
                        assert(t0.contains_key(j));
                    }
                    assert forall|j: int| 0 <= j < MAX_ARENAS implies #[trigger] bounds.contains_key(j) by {
                        assert(b0.contains_key(j));
                    }
                    assert forall|j: int| 0 <= j < MAX_ARENAS implies (#[trigger] bounds[j]).ptr()
                        == ptr_add(field_ptr::<Arenas, AtomicU32>(this, AR_BOUND), j as usize) by {
                        assert(b0.contains_key(j));
                    }
                    assert forall|j: int| 0 <= j < i + 1 implies (#[trigger] tables[j]).is_init() by {
                        if j < i {
                            assert(t0[j] == tables[j]);
                        }
                    }
                    assert forall|j: int| 0 <= j < i + 1 implies (#[trigger] bounds[j]).is_init() by {
                        if j < i {
                            assert(b0[j] == bounds[j]);
                        }
                    }
                    assert forall|j: int| 0 <= j < i + 1 implies #[trigger] counts.contains_key(j) by {
                        if j < i {
                            assert(c0.contains_key(j));
                        }
                    }
                    assert forall|j: int| 0 <= j < i + 1 implies #[trigger] counts[j].id() == count_id(&bounds[j].value()) by {
                        if j < i {
                            assert(c0[j] == counts[j]);
                            assert(b0[j] == bounds[j]);
                        }
                    }
                    assert forall|j: int| 0 <= j < i + 1 implies #[trigger] counts[j].value() == 0 by {
                        if j < i {
                            assert(c0[j] == counts[j]);
                        }
                    }
                }
            }
            proof {
                assert(tables.contains_key(0));
            }
            Arenas::put_arena0(this, arena0, Tracked(tables.tracked_borrow(0)));
            let tracked (auths, frags) = fresh_sets(MAX_ARENAS as nat);
            let ghost locs = auths;
            let tracked invs = fresh_invs(counts, auths, frags, MAX_ARENAS as nat);
            let tracked count_inv = NarenasInv::new(nperm.id(), nperm, narenas_ns());
            Arenas::put_invs(this, Tracked(invs), Tracked(count_inv), Tracked(&mut parts));
            proof {
                let tracked a = axiom_arenas_assemble(parts, tables, bounds, pad, this);
                let v = a.value();
                assert forall|i: int| 0 <= i < MAX_ARENAS implies #[trigger] v.invs@.contains_key(i) by {}
                assert forall|i: int| 0 <= i < MAX_ARENAS implies #[trigger] v.invs@[i].constant().id
                    == count_id(&v.bound[i]) by {
                    assert(v.bound[i] == bounds[i].value());
                }
                assert forall|i: int| 0 <= i < MAX_ARENAS implies #[trigger] v.invs@[i].namespace() == bound_ns() by {}
                assert forall|i: int, j: int| 0 <= i < MAX_ARENAS && 0 <= j < MAX_ARENAS && i != j
                    implies #[trigger] v.spec_loc(i) != #[trigger] v.spec_loc(j) by {
                    assert(v.spec_loc(i) == locs[i].id());
                    assert(v.spec_loc(j) == locs[j].id());
                }
                assert(v.wf());
                slot.perm = Some(a);
            }
        }
    }

    /// The registry as `init` leaves it: between 1 and `MAX_ARENAS`
    /// automatic arenas, and for every index the invariant of its bound
    /// count ([`BoundPred`]) over that counter, in the namespace
    /// [`bound_ns`], with a ticket set at a location no other index uses.
    pub closed spec fn wf(&self) -> bool {
        &&& 1 <= self.auto <= MAX_ARENAS
        // One quantifier per conjunct, each with both triggers (a fact
        // about entry `i` is found from either term).
        &&& forall|i: int|
            #![trigger self.invs@.contains_key(i)]
            #![trigger self.invs@[i]]
            0 <= i < MAX_ARENAS ==> self.invs@.contains_key(i)
        &&& forall|i: int|
            #![trigger self.invs@.contains_key(i)]
            #![trigger self.invs@[i]]
            0 <= i < MAX_ARENAS ==> self.invs@[i].constant().id == count_id(&self.bound[i])
        &&& forall|i: int|
            #![trigger self.invs@.contains_key(i)]
            #![trigger self.invs@[i]]
            0 <= i < MAX_ARENAS ==> self.invs@[i].namespace() == bound_ns()
        &&& forall|i: int, j: int| 0 <= i < MAX_ARENAS && 0 <= j < MAX_ARENAS && i != j
            ==> #[trigger] self.spec_loc(i) != #[trigger] self.spec_loc(j)
    }

    /// The location of arena `i`'s ticket set ([`BoundPred`]).
    pub closed spec fn spec_loc(&self, i: int) -> Loc {
        self.invs@[i].constant().loc
    }

    /// The bound-count invariants are the process registry's (those of
    /// `Shared::built_at(Shared::storage())`, `Shared::wf`), so their ticket
    /// sets are at [`bound_loc`] and a heap's ticket names its arena in
    /// this registry: one set of invariants, one authority per location.
    pub(crate) closed spec fn is_process(&self) -> bool {
        self.invs == Shared::built_at(Shared::storage()).arenas.invs
    }

    /// The process registry is [`Arenas::is_process`].
    pub(crate) proof fn lemma_is_process(&self)
        requires
            *self == Shared::built_at(Shared::storage()).arenas,
        ensures
            self.is_process(),
    {
    }

    /// In the process registry, a ticket for arena `i` of the process
    /// ([`homed_on`]) is one of this registry ([`Arenas::ticket_of`]).
    pub(crate) proof fn lemma_homed(&self, t: Homed, i: int)
        requires
            self.is_process(),
            0 <= i < MAX_ARENAS,
        ensures
            homed_on(t, i) <==> self.ticket_of(t, i),
    {
    }

    /// `t` is a ticket for arena `i` of this registry.
    pub open spec fn ticket_of(&self, t: Homed, i: int) -> bool {
        &&& t.arena == i
        &&& t.tok.id() == self.spec_loc(i)
    }

    /// [`Arenas::wf`]'s automatic arena count.
    pub proof fn lemma_wf(&self)
        requires
            self.wf(),
        ensures
            1 <= self.spec_auto() <= MAX_ARENAS,
    {
    }
}

} // verus!

impl Arenas {
    /// Every arena that exists, in index order (`Shared::set_decay_ms`,
    /// `policy::empty_bytes`). The table index is below `count()`, which
    /// is verified to be at most `MAX_ARENAS`, so it is in bounds.
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

verus! {

/// The arena a non-null registry entry points at. Trusted: Verus has no
/// raw-pointer dereference. Entries are published by `build` with a
/// release store after `Arena::init`, and arenas are never freed. The
/// arena at entry `index` is arena `index` (`spec_index`): `build`
/// stores the arena `Arena::init` built as `index` into entry `index`,
/// `Arenas::init` stores `Shared::arena0`, built as 0, into entry 0, and
/// nothing else writes an entry. The contract states the address fact
/// every caller shows, that `p` is not null; that a non-null entry is a
/// live, initialised arena is the registry's protocol above, which no
/// ghost state carries (`build` holds the arena's permission only until
/// it publishes it).
///
/// # Safety
/// `p` is a non-null entry loaded from `Arenas::table[index]`.
#[verifier::external_body]
#[inline(always)]
unsafe fn arena_at<'a>(p: *mut Arena, Ghost(index): Ghost<u16>) -> (r: &'a Arena)
    requires
        p@.addr != 0,
    ensures
        r.spec_index() == index,
{
    // SAFETY: guaranteed by the caller.
    unsafe { &*p }
}

/// An arena's bound count went below zero: a heap was unbound twice, or
/// never bound.
#[cold]
#[inline(never)]
fn unbind_unbound(index: u16) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!("arena {index}: unbinding a heap that was never bound"))
}

/// A fresh ticket for `auth`, all of whose tickets are below `next`:
/// `next` itself, added to the set.
proof fn new_ticket(tracked auth: &mut GhostSetAuth<int>, next: int) -> (tracked t: GhostSingleton<int>)
    requires
        forall|x: int| #[trigger] old(auth)@.contains(x) ==> x < next,
    ensures
        final(auth).id() == old(auth).id(),
        final(auth)@.len() == old(auth)@.len() + 1,
        forall|x: int| #[trigger] final(auth)@.contains(x) ==> x < next + 1,
        t.id() == old(auth).id(),
{
    broadcast use vstd::set::lemma_set_insert_len;

    auth.insert(next)
}

/// A count changed by `d`, on its `u32` wrap, is the wrap of the changed
/// count.
proof fn lemma_wrap_step(n: int, d: int)
    ensures
        wrap32(wrap32(n) + d) == wrap32(n + d),
{
    let m = 0x1_0000_0000int;
    vstd::arithmetic::div_mod::lemma_add_mod_noop_right(d, n, m);
    assert(wrap32(n) as int == n % m);
}

impl Arenas {
    /// A thread heap homed on `index` is destroyed or moves away: its
    /// ticket `t` leaves the arena's ticket set and the count goes down
    /// with it ([`BoundPred`]). The count read was the number of tickets
    /// out, `t`'s among them, as a `u32` wraps it; the proof asserts that
    /// the abort (a count of zero) is reached only when that number, at
    /// least 1, is a multiple of `2^32`.
    pub fn unbind(&self, index: u16, Tracked(t): Tracked<Homed>)
        requires
            crate::sys::may_abort(),
            (index as usize) < MAX_ARENAS,
            self.wf(),
            self.ticket_of(t, index as int),
    {
        proof {
            assert(self.invs@.contains_key(index as int));
        }
        let tracked inv = self.invs.borrow().tracked_borrow(index as int);
        let prev;
        // The tickets out for the arena at the decrement.
        let ghost mut held: int = 0;
        vstd::open_atomic_invariant!(inv => v => {
            let tracked (mut perm, mut auth, next) = v;
            proof {
                t.tok.agree(&auth);
                held = auth@.len() as int;
                vstd::set::lemma_set_contains_len(auth@, t.tok@);
            }
            prev = crate::vspec::count::fetch_sub(&self.bound[index as usize], 1, Ordering::Relaxed, Tracked(&mut perm));
            proof {
                let n = auth@.len() as int;
                let ghost s0 = auth@;
                let ghost x = t.tok@;
                auth.delete_singleton(t.tok);
                vstd::set::lemma_set_remove_len(s0, x);
                assert(auth@.len() == n - 1);
                lemma_wrap_step(n, -1);
                v = (perm, auth, next);
            }
        });
        proof {
            assert(held >= 1);
            assert(prev == wrap32(held));
            assert(prev == 0 ==> held % 0x1_0000_0000 == 0);
        }
        if prev == 0 {
            unbind_unbound(index);
        }
    }

    /// Arena `index` if it exists. An automatic index whose arena has not
    /// been built yet is built here (cold); an explicit index that
    /// `create` has not handed out, or one beyond the table, is `None`.
    #[inline]
    pub fn get<'a>(&'a self, shared: &'a Shared, index: u16, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<&'a Arena>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            old(ord).allows(rank_arenas()),
        ensures
            r matches Some(a) ==> a.spec_index() == index,
            *final(ord) == *old(ord),
    {
        let i = index as usize;
        if i >= MAX_ARENAS {
            return None;
        }
        let p = self.table[i].load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: arenas are never freed; the pointer was published
            // after `Arena::init` with a release store.
            return Some(unsafe { arena_at(p, Ghost(index)) });
        }
        self.materialise(shared, index, Tracked(ord))
    }

    #[cold]
    #[inline(never)]
    fn materialise<'a>(&'a self, shared: &'a Shared, index: u16, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<&'a Arena>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            (index as usize) < MAX_ARENAS,
            shared.wf(),
            old(ord).allows(rank_arenas()),
        ensures
            r matches Some(a) ==> a.spec_index() == index,
            *final(ord) == *old(ord),
    {
        if index as u32 >= self.auto {
            return None;
        }
        proof {
            use_type_invariant(self);
        }
        let tracked mut under = LockOrder::spare();
        let _g = self.lock.lock(Tracked(ord), Tracked(&mut under));
        let p = self.table[index as usize].load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: as in `get`.
            return Some(unsafe { arena_at(p, Ghost(index)) });
        }
        self.build(shared, index, Tracked(&mut under))
    }

    /// Build arena `index` in the metadata region and publish it. Called
    /// with the creation lock held and the slot known to be empty.
    /// `shared` is the published `Shared`, whose `wf` gives
    /// `Arena::init`'s preconditions (the process's well-formed regions and
    /// the tables in class order).
    fn build<'a>(&'a self, shared: &'a Shared, index: u16, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<&'a Arena>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            (index as usize) < MAX_ARENAS,
            shared.wf(),
            old(ord).allows(rank_meta()),
        ensures
            r matches Some(a) ==> a.spec_index() == index,
            *final(ord) == *old(ord),
    {
        let layout = core::alloc::Layout::new::<Arena>();
        proof {
            shared.lemma_wf();
            let lg = shared.regions.lg_page;
            assert((1usize << lg) == (1usize << (lg as usize))) by (bit_vector)
                requires
                    lg < 64,
            ;
            crate::meta::lemma_is_pow2_shl(lg as usize);
        }
        // Never freed, so a reservation (page aligned, which covers the
        // 128-byte class state alignment) rather than an object.
        let align = layout.align().max(shared.regions.page_size());
        let (s, Tracked(raw), _) = shared.meta.reserve_committed_raw(layout.size(), align, Tracked(ord));
        let s = s?;
        let p = addr_to_ptr(s) as *mut Arena;
        let tracked mut slot = Held::<Arena> { perm: None };
        proof {
            // The permission to the reservation's first `size_of::<Arena>()`
            // bytes, as an uninitialised `Arena`: the reservation is aligned
            // for it (a page is a multiple of any smaller power of two).
            let a = crate::vspec::layout::layout_align(layout);
            let page = 1usize << shared.regions.lg_page;
            assert(align >= a && (align == a || align == page));
            if align != a {
                lemma_pow2_divides_larger(a, page);
                crate::layout::lemma_aligned_down(s as int, align as int, a as int);
            }
            let tracked (raw_a, _) = raw.split(Set::range(s as int, s + size_of::<Arena>()));
            slot.perm = Some(raw_a.into_typed::<Arena>(s));
            assert(slot.perm->0.ptr()@ == p@);
        }
        // The class tables, from arena 0's states, which were built over
        // them.
        proof {
            use_type_invariant(&shared.arena0);
        }
        let (tables, Tracked(tperms)) = shared.arena0.classes[0].tables();
        // SAFETY: fresh, zeroed, exclusively owned storage of the right
        // size and alignment.
        let ok = unsafe {
            Arena::init(
                p,
                &shared.meta,
                &shared.regions,
                tables,
                Tracked(tperms),
                index,
                shared.default_decay_ns(),
                Tracked(&mut slot),
                Tracked(ord),
            )
        };
        if !ok {
            // The reservation is lost; the metadata region is exhausted
            // anyway.
            return None;
        }
        proof {
            slot.perm.tracked_borrow().is_nonnull();
        }
        shared.set_multi_arena();
        self.table[index as usize].store(p, Ordering::Release);
        // SAFETY: just built and published.
        Some(unsafe { arena_at(p, Ghost(index)) })
    }

    /// `arenas.create`: a new explicit arena. `None` when the table or the
    /// metadata region is full.
    pub fn create(&self, shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<u16>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            old(ord).allows(rank_arenas()),
        ensures
            r matches Some(i) ==> (i as usize) < MAX_ARENAS,
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let tracked mut under = LockOrder::spare();
        let mut g = self.lock.lock(Tracked(ord), Tracked(&mut under));
        let index = *g;
        if index as usize >= MAX_ARENAS {
            return None;
        }
        self.build(shared, index as u16, Tracked(&mut under))?;
        *g = index + 1;
        // Inside the count's invariant ([`NarenasPred`]): `index + 1` is at
        // most `MAX_ARENAS`.
        vstd::open_atomic_invariant!(self.count_inv.borrow() => perm => {
            crate::vspec::count::store(&self.count, index + 1, Ordering::Release, Tracked(&mut perm));
        });
        Some(index as u16)
    }

    /// Arena `index` if it has been built, without building it.
    pub fn get_existing(&self, index: u16) -> (r: Option<&Arena>)
        ensures
            r matches Some(a) ==> a.spec_index() == index,
    {
        let i = index as usize;
        if i >= MAX_ARENAS {
            return None;
        }
        let p = self.table[i].load(Ordering::Acquire);
        // SAFETY: a non-null entry of the table.
        if p.is_null() { None } else { Some(unsafe { arena_at(p, Ghost(index)) }) }
    }

    /// Automatic arenas, as `init` clamped them.
    pub closed spec fn spec_auto(&self) -> int {
        self.auto as int
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
    ///
    /// With the arena, the heap's ticket for it ([`Homed`]) and, as a
    /// ghost, the `(bound, unbuilt)` keys read under the lock: the arena
    /// returned has the least key read, the first one with it
    /// ([`picks_least`]). Each count is read inside its invariant
    /// ([`Arenas::load_bound`]), where it is the number of tickets out
    /// for its arena, as a `u32` wraps it; no `ensures` can name that
    /// instant, so the snapshot's counts are not restated here.
    pub fn assign(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (u16, Tracked<Homed>, Ghost<Seq<(u32, bool)>>))
        requires
            crate::sys::may_syscall(),
            self.wf(),
            old(ord).allows(rank_arenas()),
        ensures
            (r.0 as int) < self.spec_auto(),
            self.ticket_of(r.1@, r.0 as int),
            self.spec_auto() == 1 ==> r.0 == 0,
            self.spec_auto() > 1 ==> picks_least(r.2@, self.spec_auto(), r.0 as int),
            *final(ord) == *old(ord),
    {
        if self.auto == 1 {
            let t = self.count_in(0);
            return (0, t, Ghost(Seq::empty()));
        }
        proof {
            use_type_invariant(self);
        }
        let _g = self.lock.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        let mut best = 0usize;
        // (bound heaps, not yet built): the smallest wins.
        let mut best_key = (u32::MAX, true);
        let ghost mut snap: Seq<(u32, bool)> = Seq::empty();
        for i in 0..self.auto as usize
            invariant_except_break
                snap.len() == i,
            invariant
                self.wf(),
                2 <= self.auto <= MAX_ARENAS,
                best < MAX_ARENAS,
                snap.len() <= self.auto,
                snap.len() == 0 ==> best == 0 && best_key == (u32::MAX, true),
                snap.len() > 0 ==> least_of(snap, best as int) && best_key == snap[best as int],
            ensures
                snap.len() > 0,
                least_of(snap, best as int),
                best_key == snap[best as int],
                snap.len() < self.auto ==> best_key == (0u32, false),
        {
            let key = (self.load_bound(i), self.table[i].load(Ordering::Relaxed).is_null());
            proof {
                snap = snap.push(key);
            }
            if key_less(key, best_key) {
                best = i;
                best_key = key;
                if key == (0, false) {
                    break;
                }
            }
        }
        let t = self.count_in(best);
        (best as u16, t, Ghost(snap))
    }

    /// `bound[i]`, read inside its invariant: the number of tickets out for
    /// arena `i` at the read, as a `u32` wraps it ([`BoundPred`]), which the
    /// proof asserts there (no `ensures` can name that instant).
    #[inline(always)]
    fn load_bound(&self, i: usize) -> u32
        requires
            self.wf(),
            i < MAX_ARENAS,
    {
        proof {
            assert(self.invs@.contains_key(i as int));
            assert(self.invs@[i as int].constant().id == count_id(&self.bound[i as int]));
        }
        let tracked inv = self.invs.borrow().tracked_borrow(i as int);
        let n;
        vstd::open_atomic_invariant!(inv => v => {
            let tracked (perm, auth, next) = v;
            n = crate::vspec::count::load(&self.bound[i], Ordering::Relaxed, Tracked(&perm));
            proof {
                assert(n == wrap32(auth@.len() as int));
                v = (perm, auth, next);
            }
        });
        n
    }

    /// `bound[i] += 1`, with a fresh ticket for arena `i` ([`BoundPred`]).
    #[inline(always)]
    fn count_in(&self, i: usize) -> (r: Tracked<Homed>)
        requires
            self.wf(),
            i < MAX_ARENAS,
        ensures
            self.ticket_of(r@, i as int),
    {
        proof {
            assert(self.invs@.contains_key(i as int));
            assert(self.invs@[i as int].constant().id == count_id(&self.bound[i as int]));
        }
        let tracked inv = self.invs.borrow().tracked_borrow(i as int);
        let tracked t;
        vstd::open_atomic_invariant!(inv => v => {
            let tracked (mut perm, mut auth, next) = v;
            crate::vspec::count::fetch_add(&self.bound[i], 1, Ordering::Relaxed, Tracked(&mut perm));
            proof {
                let n = auth@.len() as int;
                t = Homed { tok: new_ticket(&mut auth, next@), arena: i as int };
                lemma_wrap_step(n, 1);
                v = (perm, auth, Ghost(next@ + 1));
            }
        });
        Tracked(t)
    }

    /// Number of arena indices in use (`arenas.narenas`): at most
    /// [`MAX_ARENAS`], read inside the count's invariant
    /// ([`NarenasPred`]). The fork handlers' lock order needs the bound
    /// (they visit the indices below the count as `u16` arena indices),
    /// and so do the scans over the registry ([`Arenas::iter`] and
    /// `stats::arena_at`).
    #[inline]
    pub fn count(&self) -> (r: u32)
        ensures
            r as usize <= MAX_ARENAS,
    {
        proof {
            use_type_invariant(self);
        }
        let r;
        vstd::open_atomic_invariant!(self.count_inv.borrow() => perm => {
            r = crate::vspec::count::load(&self.count, Ordering::Acquire, Tracked(&perm));
        });
        r
    }

    /// Number of automatic arenas (`opt.narenas` as applied).
    #[inline]
    pub fn auto(&self) -> u32 {
        self.auto
    }

    /// Count a thread heap as homed on `index` (`thread.arena` moving a
    /// heap to an arena `assign` did not pick): a fresh ticket for the
    /// arena, which the heap keeps while it is counted there.
    pub fn bind(&self, index: u16) -> (r: Tracked<Homed>)
        requires
            index < MAX_ARENAS,
            self.wf(),
        ensures
            self.ticket_of(r@, index as int),
    {
        self.count_in(index as usize)
    }

    /// Thread heaps currently homed on `index`: the tickets out for it
    /// at the read, as a `u32` wraps their number ([`Arenas::load_bound`];
    /// not restated by an `ensures`).
    pub fn bound(&self, index: u16) -> u32
        requires
            index < MAX_ARENAS,
            self.wf(),
    {
        self.load_bound(index as usize)
    }

    /// The creation lock, for fork handlers (taken before every class lock).
    pub fn raw_lock(&self) -> (r: &crate::lock::RawLock)
        ensures
            *r == self.spec_raw_lock(),
            r.rank() == rank_arenas(),
    {
        proof {
            use_type_invariant(self);
        }
        self.lock.raw()
    }

    /// The lock [`Self::raw_lock`] returns.
    pub closed spec fn spec_raw_lock(&self) -> crate::lock::RawLock {
        self.lock.spec_raw()
    }

    /// The creation lock is the arena creation lock of the lock order, and
    /// `count_inv` is the invariant of `count` ([`NarenasPred`]) in
    /// [`narenas_ns`] (`init` makes both so: `axiom_arenas_assemble`
    /// requires them).
    #[verifier::type_invariant]
    spec fn type_inv(self) -> bool {
        &&& self.lock.rank() == rank_arenas()
        &&& self.count_inv@.constant() == count_id(&self.count)
        &&& self.count_inv@.namespace() == narenas_ns()
    }

}

} // verus!

/// Kani checks of `axiom_arena_layout`, the offsets it names and
/// `axiom_array_layout` at the array `Arena` splits (records under
/// `verus/seams/`).
#[cfg(kani)]
pub(crate) mod kani_layout {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    /// `field_fits::<S, T>(off, next)` with its `int` sum in `u128`.
    fn field_fits<S, T>(off: usize, next: usize) -> bool {
        off % align_of::<T>() == 0
            && align_of::<S>() % align_of::<T>() == 0
            && off as u128 + size_of::<T>() as u128 <= next as u128
    }

    /// S48..S55, S56: each `A_*` is its field's `offset_of!`, and the
    /// fields fit as `axiom_arena_layout` says.
    #[kani::proof]
    fn arena_layout() {
        assert_eq!(A_INDEX, offset_of!(Arena, index));
        assert_eq!(A_DECAY_NS, offset_of!(Arena, decay_ns));
        assert_eq!(A_SETS, offset_of!(Arena, sets));
        assert_eq!(A_SETS_PERM, offset_of!(Arena, sets_perm));
        assert_eq!(A_CLASSES, offset_of!(Arena, classes));
        assert_eq!(A_LARGE, offset_of!(Arena, large));
        assert_eq!(A_STATS, offset_of!(Arena, stats));
        assert_eq!(A_LAST_EXIT_PURGE, offset_of!(Arena, last_exit_purge));
        assert!(field_fits::<Arena, u16>(A_INDEX, A_DECAY_NS));
        assert!(field_fits::<Arena, AtomicU64>(A_DECAY_NS, A_SETS));
        assert!(field_fits::<Arena, *const SetTable>(A_SETS, A_SETS_PERM));
        assert!(field_fits::<Arena, Tracked<&'static PointsTo<SetTable>>>(
            A_SETS_PERM,
            A_CLASSES
        ));
        assert!(field_fits::<Arena, [ClassState; NCLASSES]>(
            A_CLASSES, A_LARGE
        ));
        assert!(NCLASSES as u128 * size_of::<ClassState>() as u128 <= isize::MAX as u128);
        assert!(field_fits::<Arena, AtomicU64>(A_LARGE, A_STATS));
        assert!(field_fits::<Arena, Lock<ArenaStats>>(
            A_STATS,
            A_LAST_EXIT_PURGE
        ));
        assert!(field_fits::<Arena, AtomicU64>(
            A_LAST_EXIT_PURGE,
            size_of::<Arena>()
        ));
        assert!(size_of::<Arena>() <= isize::MAX as usize);
    }

    /// `axiom_array_layout::<T, N>`'s two equalities, the product in
    /// `u128`.
    pub(crate) fn array_layout<T, const N: usize>() {
        assert_eq!(
            size_of::<[T; N]>() as u128,
            N as u128 * size_of::<T>() as u128
        );
        assert_eq!(align_of::<[T; N]>(), align_of::<T>());
    }

    /// S47 `Arena::axiom_array_layout` at every instantiation the proofs
    /// use (`[ClassState; NCLASSES]` here, the two arrays of `Shared` in
    /// `heap/shared.rs`), and at primitive, zero-sized and padded element
    /// types and lengths 0, 1 and 7.
    #[kani::proof]
    fn array_layout_instances() {
        array_layout::<ClassState, NCLASSES>();
        array_layout::<u8, 0>();
        array_layout::<u8, 7>();
        array_layout::<u64, 1>();
        array_layout::<u64, 7>();
        array_layout::<(u64, u8), 7>();
        array_layout::<(), 7>();
        array_layout::<[u16; 3], 7>();
        array_layout::<AtomicU64, 7>();
    }
}
