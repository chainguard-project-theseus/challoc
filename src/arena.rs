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

use crate::class::{ClassState, SetTable, SlabTable};
use crate::heap::Shared;
#[cfg(verus_only)]
use crate::layout::process_regions;
use crate::layout::{NCLASSES, Regions};
use crate::lock::Lock;
use crate::meta::Meta;
use crate::stats::ArenaStats;
use crate::vspec::addr::addr_to_ptr;
#[cfg(verus_only)]
use crate::vspec::arena::axiom_arena_stats_lock_inv;
#[cfg(verus_only)]
use crate::vspec::ptr::{axiom_ptr_add_addr, axiom_raw_valid, in_alloc, ptr_add};
#[cfg(verus_only)]
use vstd::layout::{align_of, size_of};
use vstd::prelude::*;
#[cfg(verus_only)]
use vstd::raw_ptr::PointsToRaw;
use vstd::raw_ptr::{PointsTo, ptr_mut_write, ptr_ref};

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
    /// Ghost: the permission to the table at `sets`.
    #[allow(dead_code)]
    sets_perm: Tracked<PointsTo<SetTable>>,
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
// into the struct's or the array's value has no counterpart in vstd and
// is an axiom per struct ([`axiom_array_join`], `axiom_arena_assemble`,
// `Shared`'s `axiom_shared_built`).

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
/// that field on; what is left holds them from `next` on.
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
/// states no array layout).
pub(crate) axiom fn axiom_array_layout<T, const N: usize>()
    ensures
        size_of::<[T; N]>() == N * size_of::<T>(),
        align_of::<[T; N]>() == align_of::<T>(),
;

/// `m` holds a permission for each element `lo..hi` of the array of `T`s
/// from `base`, at its address.
pub open spec fn elems_at<T>(m: Map<int, PointsTo<T>>, base: *mut T, lo: int, hi: int) -> bool {
    forall|i: int| lo <= i < hi ==> #[trigger] m.contains_key(i) && m[i].ptr() == ptr_add(base, i as usize)
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
        assert forall|j: int| i <= j < n implies #[trigger] m.contains_key(j) && m[j].ptr() == ptr_add(base, j as usize)
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
        size_of::<[T; N]>() <= isize::MAX,
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

/// The initialised elements of an array, as the array: the value whose
/// element `i` each holds. Trusted: vstd has no way to join typed
/// permissions. The elements are at their addresses in one allocation
/// (`ptr_add` is the array's index projection there), so their bytes
/// are the array's.
pub(crate) axiom fn axiom_array_join<T, const N: usize>(base: *mut [T; N], tracked m: Map<int, PointsTo<T>>) -> (tracked a:
    PointsTo<[T; N]>)
    requires
        N > 0,
        in_alloc(base as *mut T, N as int * size_of::<T>()),
        base@.addr + N * size_of::<T>() <= usize::MAX,
        elems_at(m, base as *mut T, 0, N as int),
        elems_init(m, 0, N as int),
    ensures
        a.ptr() == base,
        a.is_init(),
        forall|i: int| 0 <= i < N ==> #[trigger] a.value()[i] == m[i].value(),
;

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
        field_fits::<Arena, Tracked<PointsTo<SetTable>>>(A_SETS_PERM, A_CLASSES as int),
        field_fits::<Arena, [ClassState; NCLASSES]>(A_CLASSES, A_LARGE as int),
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

} // verus!

/// [`field_fits`] for the build: a field of type `T` at offset `off` of
/// an `S` is aligned for `T`, `S`'s alignment is a multiple of `T`'s, and
/// the field ends by `next`.
pub(crate) const fn fits<S, T>(off: usize, next: usize) -> bool {
    off % core::mem::align_of::<T>() == 0
        && core::mem::align_of::<S>() % core::mem::align_of::<T>() == 0
        && off + core::mem::size_of::<T>() <= next
}

// `axiom_arena_layout`, checked.
const _: () = assert!(
    fits::<Arena, u16>(A_INDEX, A_DECAY_NS)
        && fits::<Arena, AtomicU64>(A_DECAY_NS, A_SETS)
        && fits::<Arena, *const SetTable>(A_SETS, A_SETS_PERM)
        && fits::<Arena, Tracked<PointsTo<SetTable>>>(A_SETS_PERM, A_CLASSES)
        && fits::<Arena, [ClassState; NCLASSES]>(A_CLASSES, A_LARGE)
        && fits::<Arena, AtomicU64>(A_LARGE, A_STATS)
        && fits::<Arena, Lock<ArenaStats>>(A_STATS, A_LAST_EXIT_PURGE)
        && fits::<Arena, AtomicU64>(A_LAST_EXIT_PURGE, core::mem::size_of::<Arena>())
        && core::mem::size_of::<Arena>() <= isize::MAX as usize
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
    tracked sets_perm: PointsTo<Tracked<PointsTo<SetTable>>>,
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
        &&& self.sets_perm.ptr() == field_ptr::<Arena, Tracked<PointsTo<SetTable>>>(p, A_SETS_PERM)
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

impl Arena {
    /// The permissions to the fields of the uninitialised `Arena` `perm`
    /// is to: [`take_field`] for each field at the offsets
    /// [`axiom_arena_layout`] gives, and the class states split by
    /// [`split_array`].
    proof fn split(tracked perm: PointsTo<Arena>) -> (tracked r: (ArenaParts, Map<int, PointsTo<ClassState>>))
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
        let tracked (sets_perm, raw) = take_field::<Arena, Tracked<PointsTo<SetTable>>>(
            raw,
            p,
            A_SETS_PERM,
            A_CLASSES as int,
        );
        let tracked (classes, raw) = take_field::<Arena, [ClassState; NCLASSES]>(raw, p, A_CLASSES, A_LARGE as int);
        let tracked (large, raw) = take_field::<Arena, AtomicU64>(raw, p, A_LARGE, A_STATS as int);
        let tracked (stats, raw) = take_field::<Arena, Lock<ArenaStats>>(raw, p, A_STATS, A_LAST_EXIT_PURGE as int);
        let tracked (last_exit_purge, _raw) = take_field::<Arena, AtomicU64>(
            raw,
            p,
            A_LAST_EXIT_PURGE,
            size_of::<Arena>() as int,
        );
        axiom_array_layout::<ClassState, NCLASSES>();
        let tracked classes = split_array(classes);
        lemma_field_cast::<Arena, [ClassState; NCLASSES], ClassState>(p, A_CLASSES);
        (ArenaParts { index, decay_ns, sets, sets_perm, large, stats, last_exit_purge }, classes)
    }
}

/// The written fields of the `Arena` at `p` (`Arena::split`'s parts and
/// class states, every one initialised), as the `Arena`: the value of
/// each field is what its permission holds. Trusted: vstd has no way to
/// join typed permissions; the permissions cover every field at its
/// offset, so their bytes are the `Arena`'s (padding aside). The type
/// invariant `Arena::inv` is a precondition, so the value is one an
/// `Arena` may hold.
axiom fn axiom_arena_assemble(tracked parts: ArenaParts, tracked classes: Map<int, PointsTo<ClassState>>, p: *mut Arena) -> (tracked a:
    PointsTo<Arena>)
    requires
        parts.at(p),
        classes_at(classes, p),
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
    fn alloc_sets(meta: &Meta) -> (r: (*mut SetTable, Tracked<Option<PointsTo<SetTable>>>))
        ensures
            r.0@.addr != 0 ==> r.1@ is Some && r.1@->0.ptr() == r.0 && r.1@->0.is_uninit(),
    {
        let layout = core::alloc::Layout::new::<SetTable>();
        crate::vspec::check::check(
            layout.size() <= crate::meta::MAX_BINNED && layout.align() <= crate::meta::MAX_ALIGN,
            "arena: slab set table larger than an object",
        );
        let (p, Tracked(raw)) = meta.alloc_raw(layout);
        let p = p as *mut SetTable;
        let tracked perm = table_perm(raw, p);
        (p, Tracked(perm))
    }

    /// `sets_ptr` into the field and its permission `perm` into
    /// `sets_perm`; the table, where it lives.
    // `held` is read only by the ghost argument of the last `ptr_ref`.
    #[allow(unused_variables)]
    #[inline(always)]
    unsafe fn put_sets<'a>(
        this: *mut Arena,
        sets_ptr: *mut SetTable,
        perm: Tracked<PointsTo<SetTable>>,
        Tracked(sets): Tracked<&mut PointsTo<*const SetTable>>,
        Tracked(sets_perm): Tracked<&'a mut PointsTo<Tracked<PointsTo<SetTable>>>>,
    ) -> (r: &'a SetTable)
        requires
            storage_ok(this),
            old(sets).ptr() == field_ptr::<Arena, *const SetTable>(this, A_SETS),
            old(sets_perm).ptr() == field_ptr::<Arena, Tracked<PointsTo<SetTable>>>(this, A_SETS_PERM),
            perm@.ptr() == sets_ptr,
            perm@.is_init(),
        ensures
            final(sets).ptr() == old(sets).ptr(),
            final(sets).is_init(),
            final(sets).value() == sets_ptr as *const SetTable,
            final(sets_perm).ptr() == old(sets_perm).ptr(),
            final(sets_perm).is_init(),
            final(sets_perm).value() == perm,
            *r == perm@.value(),
    {
        proof {
            axiom_arena_layout();
        }
        // SAFETY: the fields of `init`'s storage, through their
        // permissions; `sets_ptr` is the metadata object the table was just
        // written to, through `perm`.
        unsafe {
            ptr_mut_write(field_at(this, A_SETS), Tracked(sets), sets_ptr as *const SetTable);
            ptr_mut_write(field_at(this, A_SETS_PERM), Tracked(&mut *sets_perm), perm);
            let held: &'a Tracked<PointsTo<SetTable>> = ptr_ref(field_at(this, A_SETS_PERM), Tracked(sets_perm));
            ptr_ref(sets_ptr, Tracked(held.borrow()))
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
            final(parts).last_exit_purge.is_init(),
    {
        broadcast use axiom_arena_stats_lock_inv;

        proof {
            axiom_arena_layout();
        }
        // SAFETY: the fields of `init`'s storage, through their permissions.
        unsafe {
            ptr_mut_write(field_at(this, A_LARGE), Tracked(&mut parts.large), AtomicU64::new(0));
            ptr_mut_write(field_at(this, A_STATS), Tracked(&mut parts.stats), Lock::new(ArenaStats::empty(index)));
            ptr_mut_write(field_at(this, A_LAST_EXIT_PURGE), Tracked(&mut parts.last_exit_purge), AtomicU64::new(0));
        }
    }

    /// Initialise `this` in place as arena `index` over `tables`, through
    /// the permission to its storage in `slot`, which is handed back
    /// initialised. Returns `false` if the metadata region cannot hold the
    /// slab sets, leaving `this` unusable (and `slot` empty). Every
    /// `classes[c]` is written, with a state of class `c` (the type
    /// invariant), before `true` is returned.
    ///
    /// # Safety
    /// `this` must point to writable storage for an `Arena` that nothing
    /// else reads until this returns `true`; `tables` must outlive it.
    #[verifier::loop_isolation(false)]
    pub unsafe fn init(
        this: *mut Arena,
        meta: &Meta,
        regions: &Regions,
        tables: &[SlabTable; NCLASSES],
        index: u16,
        decay_ns: u64,
        Tracked(slot): Tracked<&mut Held<Arena>>,
    ) -> (r: bool)
        requires
            old(slot).perm matches Some(p) && p.ptr() == this && p.is_uninit(),
            this@.addr + size_of::<Arena>() <= usize::MAX,
            regions.wf(),
            *regions == process_regions(),
            forall|c: int| 0 <= c < NCLASSES ==> #[trigger] tables[c].spec_class() == c,
        ensures
            r ==> (final(slot).perm matches Some(p) && p.ptr() == this && p.is_init() && p.value().spec_index() == index),
    {
        let tracked (mut parts, mut classes) = Arena::split(slot.perm.tracked_take());
        // SAFETY: the caller guarantees `this` is writable and unaliased;
        // every write goes through the permission to its field.
        unsafe {
            Arena::put_head(this, index, decay_ns, Tracked(&mut parts));
            let (sets_ptr, Tracked(perm)) = Arena::alloc_sets(meta);
            if sets_ptr.is_null() {
                return false;
            }
            let sets = match SetTable::new(meta, regions) {
                Some(sets) => {
                    let tracked mut perm = perm.tracked_unwrap();
                    ptr_mut_write(sets_ptr, Tracked(&mut perm), sets);
                    Arena::put_sets(this, sets_ptr, Tracked(perm), Tracked(&mut parts.sets), Tracked(&mut parts.sets_perm))
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
            {
                match ClassState::new(meta, regions, &tables[c], index, sets) {
                    Some(cs) => {
                        let ghost before = classes;
                        proof {
                            assert(classes.contains_key(c as int));
                        }
                        let tracked slot_c = classes.tracked_borrow_mut(c as int);
                        Arena::put_class(this, c, cs, Tracked(slot_c));
                        proof {
                            assert forall|d: int| 0 <= d < NCLASSES implies #[trigger] classes.contains_key(d)
                                && classes[d].ptr() == ptr_add(field_ptr::<Arena, ClassState>(this, A_CLASSES), d as usize) by {
                                assert(before.contains_key(d));
                            }
                            assert forall|d: int| 0 <= d < c + 1 implies (#[trigger] classes[d]).is_init()
                                && classes[d].value().spec_class() == d && classes[d].value().spec_arena() == index by {
                                if d < c {
                                    assert(before[d] == classes[d]);
                                }
                            }
                        }
                    },
                    None => return false,
                }
            }
            Arena::put_tail(this, index, Tracked(&mut parts));
        }
        proof {
            slot.perm = Some(axiom_arena_assemble(parts, classes, this));
        }
        true
    }

    /// The arena's slab set table (fork handlers, tests).
    #[inline]
    pub fn sets(&self) -> (r: &SetTable)
        ensures
            *r == self.spec_sets(),
    {
        proof {
            use_type_invariant(self);
        }
        // Written by `init` to a live, never-freed metadata object.
        ptr_ref(self.sets, Tracked(self.sets_perm.borrow()))
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
    }

    /// `Empty` bytes over every class (one lock hold per class that has
    /// any), for the cgroup policy's retention figure.
    pub fn empty_bytes(&self) -> (r: usize)
        ensures
            r <= NCLASSES * 0x200_0000_0000_0000,
    {
        let mut sum = 0usize;
        let mut c = 0;
        while c < NCLASSES
            invariant
                c <= NCLASSES,
                sum <= c * 0x200_0000_0000_0000,
            decreases NCLASSES - c,
        {
            let cs = &self.classes[c];
            if cs.oldest_empty() != 0 {
                sum += cs.empty_bytes();
            }
            c += 1;
        }
        sum
    }

    /// Purge every `Empty` slab of every class whatever its age. Returns
    /// how many were purged.
    pub fn purge_all(&self) -> (r: usize)
        ensures
            r <= NCLASSES * 0x2000_0000_0000,
    {
        let mut sum = 0usize;
        let mut c = 0;
        while c < NCLASSES
            invariant
                c <= NCLASSES,
                sum <= c * 0x2000_0000_0000,
            decreases NCLASSES - c,
        {
            let cs = &self.classes[c];
            cs.nslabs_fit();
            sum += cs.purge_empty(u64::MAX);
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
    pub fn large(&self) -> (usize, usize) {
        proof {
            assert(Self::LARGE_RUN == 0x1_0000_0000) by (compute_only);
        }
        let w = self.large.load(Ordering::Relaxed);
        ((w >> 32) as usize, (w & (Self::LARGE_RUN - 1)) as usize)
    }

    /// The statistics stored by the last `epoch`.
    pub fn stats(&self) -> ArenaStats {
        *self.stats.lock()
    }

    /// Store the statistics of an `epoch`.
    pub fn store_stats(&self, s: &ArenaStats) {
        broadcast use axiom_arena_stats_lock_inv;

        *self.stats.lock() = *s;
    }

    /// The statistics lock, for the fork handlers (a leaf: never held
    /// while another lock is taken).
    pub fn stats_raw_lock(&self) -> (r: &crate::lock::RawLock)
        ensures
            *r == self.spec_stats_lock(),
    {
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
    pub fn set_decay_ms(&self, ms: i64) {
        self.decay_ns
            .store(crate::purge::decay_ns_from_ms(ms), Ordering::Relaxed);
        crate::purge::note_decay_changed();
    }

    /// Decay step over every class (see `ClassState::decay`): the earliest
    /// next deadline, `None` when nothing is `Empty` or decay is off.
    pub fn decay(&self, now: u64) -> Option<u64> {
        self.decay_with(now, self.decay_ns(), crate::policy::KEEP_ALL)
    }

    /// [`decay`](Arena::decay) with the period the cgroup policy gives
    /// this arena and its keep ratio for the retention cap (design 11);
    /// `NEVER` does nothing.
    pub fn decay_with(&self, now: u64, decay_ns: u64, keep16: u32) -> Option<u64> {
        if decay_ns == crate::purge::NEVER {
            return None;
        }
        let mut next: Option<u64> = None;
        for cs in self.classes.iter() {
            if let Some(d) = cs.decay_with(now, decay_ns, keep16) {
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
}

} // verus!

verus! {

impl Arenas {
    /// Initialise `this` in place with `arena0` at index 0 and `narenas`
    /// automatic indices (clamped to `1..=MAX_ARENAS`), through the
    /// permission to its storage in `slot`, handed back initialised.
    /// Trusted: field-by-field writes through `&raw mut` projections.
    ///
    /// # Safety
    /// `this` must point to writable storage that nothing else reads until
    /// this returns; `arena0` must outlive the registry.
    #[verifier::external_body]
    pub unsafe fn init(this: *mut Arenas, arena0: *mut Arena, narenas: u32, Tracked(slot): Tracked<&mut Held<Arenas>>)
        requires
            old(slot).perm matches Some(p) && p.ptr() == this && p.is_uninit(),
        ensures
            final(slot).perm matches Some(p) && p.ptr() == this && p.is_init(),
    {
        let auto = narenas.clamp(1, MAX_ARENAS as u32);
        // SAFETY: the caller guarantees `this` is writable and unaliased.
        unsafe {
            ptr::write(&raw mut (*this).lock, Lock::new(auto));
            ptr::write(&raw mut (*this).count, AtomicU32::new(auto));
            ptr::write(&raw mut (*this).auto, auto);
            let table = &raw mut (*this).table;
            let bound = &raw mut (*this).bound;
            for i in 0..MAX_ARENAS {
                ptr::write(&raw mut (*table)[i], AtomicPtr::new(ptr::null_mut()));
                ptr::write(&raw mut (*bound)[i], AtomicU32::new(0));
            }
            (*table)[0].store(arena0, Ordering::Release);
        }
    }
}

} // verus!

impl Arenas {
    /// Every arena that exists, in index order (`Shared::set_decay_ms`,
    /// `policy::empty_bytes`).
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
/// nothing else writes an entry.
///
/// # Safety
/// `p` is a non-null entry loaded from `Arenas::table[index]`.
#[verifier::external_body]
#[inline(always)]
unsafe fn arena_at<'a>(p: *mut Arena, Ghost(index): Ghost<u16>) -> (r: &'a Arena)
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
fn unbind_unbound(index: u16) -> ! {
    crate::sys::abort_fmt(format_args!("arena {index}: unbinding a heap that was never bound"))
}

impl Arenas {
    /// A thread heap homed on `index` is destroyed or moves away.
    pub fn unbind(&self, index: u16)
        requires
            (index as usize) < MAX_ARENAS,
    {
        let prev = self.bound[index as usize].fetch_sub(1, Ordering::Relaxed);
        if prev == 0 {
            unbind_unbound(index);
        }
    }

    /// Arena `index` if it exists. An automatic index whose arena has not
    /// been built yet is built here (cold); an explicit index that
    /// `create` has not handed out, or one beyond the table, is `None`.
    #[inline]
    pub fn get<'a>(&'a self, shared: &'a Shared, index: u16) -> (r: Option<&'a Arena>)
        requires
            shared.wf(),
        ensures
            r matches Some(a) ==> a.spec_index() == index,
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
        self.materialise(shared, index)
    }

    #[cold]
    #[inline(never)]
    fn materialise<'a>(&'a self, shared: &'a Shared, index: u16) -> (r: Option<&'a Arena>)
        requires
            (index as usize) < MAX_ARENAS,
            shared.wf(),
        ensures
            r matches Some(a) ==> a.spec_index() == index,
    {
        if index as u32 >= self.auto {
            return None;
        }
        let _g = self.lock.lock();
        let p = self.table[index as usize].load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: as in `get`.
            return Some(unsafe { arena_at(p, Ghost(index)) });
        }
        self.build(shared, index)
    }

    /// Build arena `index` in the metadata region and publish it. Called
    /// with the creation lock held and the slot known to be empty.
    /// `shared` is the published `Shared`, whose `wf` gives
    /// `Arena::init`'s preconditions (the process's well-formed regions and
    /// the tables in class order).
    fn build<'a>(&'a self, shared: &'a Shared, index: u16) -> (r: Option<&'a Arena>)
        requires
            (index as usize) < MAX_ARENAS,
            shared.wf(),
        ensures
            r matches Some(a) ==> a.spec_index() == index,
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
        let (s, Tracked(raw), _) = shared.meta.reserve_committed_raw(layout.size(), align);
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
        // SAFETY: fresh, zeroed, exclusively owned storage of the right
        // size and alignment; the tables live in `Shared` for the process.
        let ok = unsafe {
            Arena::init(
                p,
                &shared.meta,
                &shared.regions,
                shared.tables(),
                index,
                shared.default_decay_ns(),
                Tracked(&mut slot),
            )
        };
        if !ok {
            // The reservation is lost; the metadata region is exhausted
            // anyway.
            return None;
        }
        shared.set_multi_arena();
        self.table[index as usize].store(p, Ordering::Release);
        // SAFETY: just built and published.
        Some(unsafe { arena_at(p, Ghost(index)) })
    }

    /// `arenas.create`: a new explicit arena. `None` when the table or the
    /// metadata region is full.
    pub fn create(&self, shared: &Shared) -> Option<u16>
        requires
            shared.wf(),
    {
        let mut g = self.lock.lock();
        let index = *g;
        if index as usize >= MAX_ARENAS {
            return None;
        }
        self.build(shared, index as u16)?;
        *g = index + 1;
        self.count.store(index + 1, Ordering::Release);
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
    pub fn assign(&self) -> (r: u16)
        requires
            self.spec_auto() <= MAX_ARENAS,
        ensures
            r < MAX_ARENAS,
    {
        if self.auto == 1 {
            self.bound[0].fetch_add(1, Ordering::Relaxed);
            return 0;
        }
        let _g = self.lock.lock();
        let mut best = 0usize;
        // (bound heaps, not yet built): the smallest wins.
        let mut best_key = (u32::MAX, true);
        for i in 0..self.auto as usize
            invariant
                self.auto <= MAX_ARENAS,
                best < MAX_ARENAS,
        {
            let key = (self.bound[i].load(Ordering::Relaxed), self.table[i].load(Ordering::Relaxed).is_null());
            if key < best_key {
                best = i;
                best_key = key;
                if key == (0, false) {
                    break;
                }
            }
        }
        self.bound[best].fetch_add(1, Ordering::Relaxed);
        best as u16
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

    /// Count a thread heap as homed on `index` (`thread.arena` moving a
    /// heap to an arena `assign` did not pick).
    pub fn bind(&self, index: u16)
        requires
            index < MAX_ARENAS,
    {
        self.bound[index as usize].fetch_add(1, Ordering::Relaxed);
    }

    /// Thread heaps currently homed on `index`.
    pub fn bound(&self, index: u16) -> u32
        requires
            index < MAX_ARENAS,
    {
        self.bound[index as usize].load(Ordering::Relaxed)
    }

    /// The creation lock, for fork handlers (taken before every class lock).
    pub fn raw_lock(&self) -> (r: &crate::lock::RawLock)
        ensures
            *r == self.spec_raw_lock(),
    {
        self.lock.raw()
    }

    /// The lock [`Self::raw_lock`] returns.
    pub closed spec fn spec_raw_lock(&self) -> crate::lock::RawLock {
        self.lock.spec_raw()
    }
}

} // verus!
