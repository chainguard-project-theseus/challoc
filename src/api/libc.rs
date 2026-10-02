//! Standard and glibc allocation entry points (design.md section 13.1):
//! `malloc`, `free`, `calloc`, `realloc`, `aligned_alloc`, `posix_memalign`,
//! `memalign`, `valloc`, `pvalloc`, `free_sized`, `free_aligned_sized`,
//! `reallocarray`, `malloc_usable_size`, `malloc_trim`, and challoc's own
//! size-returning `challoc_malloc_usable`, `challoc_calloc_usable`,
//! `challoc_realloc_usable` and `challoc_free_usable` (section 13.7).
//!
//! Every export is a thin `pub extern "C"` wrapper over a Rust function in
//! this module so that the logic is unit-testable in the `cargo test`
//! binary without interposing that binary's own `malloc`: the exports are
//! compiled out under `cfg(test)` and exercised instead through the release
//! `.so` under `LD_PRELOAD` (see `tests/c/libc_api.c`).
//!
//! Rules followed here (glibc's documented contract for a replacement
//! malloc, design section 1):
//!
//! - Failure returns null with `errno = ENOMEM`; `aligned_alloc` and
//!   `memalign` set `EINVAL` for a bad alignment; `posix_memalign` returns
//!   the error and leaves `errno` alone.
//! - `free` preserves `errno`. Nothing on its path calls into libc: the
//!   fast path touches only the heap, the cache, the slab's bitmap word
//!   and (with several arenas) its owner word, never the descriptor
//!   (design 6.3), and the slow paths use raw syscalls that never write
//!   `errno`.
//! - `malloc(0)` returns a distinct pointer (a real class-0 slot);
//!   `realloc(p, 0)` frees `p` and returns null, as glibc does.
//!
//! Alignment above the quantum is served from the smallest class at or
//! above the request whose slot size is a multiple of the alignment. Slabs
//! are aligned to their own power-of-two size and slots sit at multiples of
//! the slot size, so every slot of such a class is aligned; the power-of-two
//! class at or above `max(size, align)` always qualifies, so the search
//! never leaves the medium tier for alignments up to `MEDIUM_MAX`. Anything
//! larger is a huge run, which the huge allocator aligns itself.
use core::ffi::c_int;
use core::ptr;

use crate::QUANTUM;
use crate::heap::{self, Shared};
#[cfg(verus_only)]
use crate::layout::class_table;
use crate::layout::{CLASSES, Kind, MEDIUM_MAX, NCLASSES, class_of};
use crate::sys::Errno;
use crate::vspec::usable::Stored;
#[cfg(verus_only)]
use crate::vspec::usable::{usable_is, usable_of};
use crate::vspec::zero::Zeroed;
#[cfg(verus_only)]
use crate::vspec::zero::covers_usable;
#[cfg(verus_only)]
use vstd::arithmetic::power2::pow2;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, verus};

verus! {

// The codes as `c_int`, for `errno` and `posix_memalign`'s return; the
// numbers live in `sys::errno` (checked below).
pub const ENOMEM: c_int = 12;
pub const EINVAL: c_int = 22;

} // verus!

const _: () = assert!(ENOMEM == Errno::ENOMEM.0 && EINVAL == Errno::EINVAL.0);

verus! {

/// `x` is a power of two, as `usize::is_power_of_two` answers.
pub open spec fn is_pow2(x: usize) -> bool {
    exists|k: nat| k < 64 && x as nat == #[trigger] pow2(k)
}

/// A power of two is at least 1.
pub(super) proof fn lemma_pow2_pos(x: usize)
    requires
        is_pow2(x),
    ensures
        x >= 1,
{
    let k = choose|k: nat| k < 64 && x as nat == #[trigger] pow2(k);
    vstd::arithmetic::power2::lemma_pow2_pos(k);
}

/// Trusted: writes the calling thread's `errno` through libc's pointer.
#[verifier::external_body]
#[inline(always)]
pub(super) fn set_errno(e: c_int) {
    // SAFETY: `__errno_location` returns the calling thread's `errno`.
    unsafe { *::libc::__errno_location() = e };
}

/// Null with `errno = ENOMEM`.
#[cold]
#[inline(never)]
fn enomem() -> (r: *mut u8)
    ensures
        r@.addr == 0,
{
    set_errno(ENOMEM);
    ptr::null_mut()
}

/// Null with `errno = EINVAL`.
#[cold]
#[inline(never)]
fn einval() -> (r: *mut u8)
    ensures
        r@.addr == 0,
{
    set_errno(EINVAL);
    ptr::null_mut()
}

/// [`class_of`] with the facts about the class table the entry points
/// rely on: the class is in the table, its slot size is at least `n`, and
/// so is every later class's (the table is sorted by size), and no
/// earlier class's: the class is the smallest that holds `n` bytes.
#[inline(always)]
pub(super) fn class_index(n: usize) -> (r: Option<usize>)
    ensures
        r is Some <==> n <= MEDIUM_MAX,
        r matches Some(c) ==> c < NCLASSES,
        r matches Some(c) ==> forall|j: int| c <= j < NCLASSES ==> n <= #[trigger] class_table()[j].size,
        r matches Some(c) ==> forall|j: int| 0 <= j < c ==> #[trigger] class_table()[j].size < n,
{
    let r = class_of(n);
    proof {
        reveal(crate::layout::classes_below);
        if let Some(c) = r {
            assert forall|j: int| c <= j < NCLASSES implies n <= #[trigger] class_table()[j].size by {
                crate::vspec::class::axiom_class_table_wf(j);
                if j > c {
                    crate::layout::lemma_class_size_mono(c as int, j);
                }
            }
            assert forall|j: int| 0 <= j < c implies #[trigger] class_table()[j].size < n by {
                crate::vspec::class::axiom_class_table_wf(j);
                assert(crate::layout::spec_class_size(j) < n);
            }
        }
    }
    r
}

/// A slot result of the class `class_index(size)` (or any later one)
/// returned serves `malloc(size)` ([`heap::fits`] at the quantum).
proof fn lemma_class_fits(p: *mut u8, c: int, size: usize)
    requires
        heap::regions_ok(),
        0 <= c < NCLASSES,
        size <= class_table()[c].size,
        p@.addr != 0 ==> crate::layout::process_regions().is_slot(c, p@.addr),
    ensures
        heap::fits(p, size as int, QUANTUM as int),
{
    if p@.addr != 0 {
        heap::lemma_slot_fits_quantum(p, c, size as int);
    }
}

/// A huge result for `size` bytes at `align` serves them at `align`, and
/// at the quantum.
proof fn lemma_huge_result(p: *mut u8, size: usize, align: usize)
    requires
        heap::regions_ok(),
        is_pow2(align),
        p@.addr != 0 ==> crate::layout::huge_block(p@.addr, size, align),
    ensures
        heap::fits(p, size as int, align as int),
        heap::fits(p, size as int, QUANTUM as int),
{
    if p@.addr != 0 {
        heap::lemma_quantum_pow2();
        heap::lemma_huge_fits(p, size, align, align);
        heap::lemma_huge_fits(p, size, align, QUANTUM);
    }
}

/// A slot of class `c` is no page of the huge region: it lies in its
/// class region, which `separated` keeps apart from the huge region.
proof fn lemma_slot_not_huge(r: crate::layout::Regions, c: int, p: usize)
    requires
        r.wf(),
        r.separated(),
        r.is_slot(c, p),
    ensures
        !r.is_huge_block(p, 1),
{
    let stride = (1usize << r.lg_stride) as int;
    r.lemma_slot_in_region(c, p);
    crate::vspec::class::axiom_class_table_wf(c);
    assert(r.superblock + c * stride <= r.class_base[c]);
    assert((c + 1) * stride <= NCLASSES * stride) by (nonlinear_arith)
        requires
            c < NCLASSES,
            stride >= 0,
    ;
}

/// Huge allocation on the calling thread, counted in its heap when it has
/// one. `align` is a power of two; `1` means none.
#[cold]
#[inline(never)]
fn alloc_huge(shared: &Shared, size: usize, align: usize) -> (r: (*mut u8, bool, Tracked<Zeroed>))
    requires
        shared.wf(),
        is_pow2(align),
    ensures
        r.0@.addr != 0 ==> crate::layout::huge_block(r.0@.addr, size, align),
        r.0@.addr != 0 ==> size <= shared.huge_usable(size),
        r.0@.addr != 0 ==> shared.huge_ok(size),
        r.0@.addr != 0 ==> crate::huge::allocated(r.0@.addr, size),
        r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, shared.huge_usable(size)),
{
    match heap::heap_for_thread(shared) {
        Some(h) => h.alloc_huge(shared, size, align),
        None => shared.alloc_huge(size, align, 0),
    }
}

/// `malloc`. A non-null result is a block of at least `size` bytes in a
/// class or huge region, aligned to the quantum ([`heap::fits`]).
#[inline(always)]
pub fn malloc(size: usize) -> (r: *mut u8)
    ensures
        heap::fits(r, size as int, QUANTUM as int),
{
    // The fast path is a cache pop with no calls in it; everything else
    // (init, heap creation, refill, huge, ENOMEM) is one tail call away so
    // the exported `malloc` needs no stack frame.
    let Some(c) = class_index(size) else { return malloc_slow(size) };
    let shared = super::shared_ptr();
    let Some(h) = heap::existing_heap(shared) else { return malloc_slow(size) };
    // SAFETY: this thread's heap exists, so this thread observed `READY`
    // (the byte was read once inside `existing_heap`; this is the same
    // load, merged by the compiler).
    let flags = unsafe { heap::fast_flags(shared) };
    proof {
        h.lemma_wf_regions();
    }
    match h.alloc_fast(c, flags) {
        Some(p) => {
            proof {
                lemma_class_fits(p, c as int, size);
            }
            p
        },
        // The class, not the size: the declined pop (an empty cache or
        // the scheduled pop) goes straight to the slow path of its class,
        // `heap::alloc_declined`, not through a second classification and
        // a second fast pop.
        None => {
            let p = malloc_slow_class(c);
            proof {
                lemma_class_fits(p, c as int, size);
            }
            p
        },
    }
}

/// `malloc_slow` for a size whose class is known and whose fast pop has
/// just declined: the refill or the scheduled pop through
/// `heap::alloc_declined`, null with `errno = ENOMEM` on failure.
#[cold]
#[inline(never)]
fn malloc_slow_class(class: usize) -> (r: *mut u8)
    requires
        class < NCLASSES,
    ensures
        r@.addr != 0 ==> crate::layout::process_regions().is_slot(class as int, r@.addr),
{
    let p = heap::alloc_declined(super::shared(), class);
    if p.is_null() { enomem() } else { p }
}

/// The cache pop alone: `None` whenever anything is off the fast path (no
/// heap yet, empty cache, huge size). Shared with the C++ `operator new`
/// family in `cxx.rs`, whose failure handling differs from `malloc`'s.
#[inline(always)]
pub(super) fn malloc_fast(size: usize) -> (r: Option<*mut u8>)
    ensures
        r matches Some(p) ==> heap::fits(p, size as int, QUANTUM as int),
{
    let c = class_index(size)?;
    let shared = super::shared_ptr();
    let h = heap::existing_heap(shared)?;
    // SAFETY: this thread's heap exists, so this thread observed `READY`
    // (the byte was read once inside `existing_heap`; this is the same
    // load, merged by the compiler).
    let flags = unsafe { heap::fast_flags(shared) };
    proof {
        h.lemma_wf_regions();
    }
    let r = h.alloc_fast(c, flags);
    proof {
        if let Some(p) = r {
            lemma_class_fits(p, c as int, size);
        }
    }
    r
}

/// Everything `malloc` does off its fast path. Null with `errno = ENOMEM`
/// on failure.
#[cold]
#[inline(never)]
pub(super) fn malloc_slow(size: usize) -> (r: *mut u8)
    ensures
        heap::fits(r, size as int, QUANTUM as int),
{
    let shared = super::shared();
    proof {
        crate::meta::lemma_is_pow2_one();
        shared.lemma_regions_ok();
    }
    let p = match class_index(size) {
        Some(c) => {
            let p = heap::alloc(shared, c);
            proof {
                lemma_class_fits(p, c as int, size);
            }
            p
        },
        None => {
            let p = alloc_huge(shared, size, 1).0;
            proof {
                lemma_huge_result(p, size, 1);
            }
            p
        },
    };
    if p.is_null() { enomem() } else { p }
}

/// `calloc`. Skips the `memset` when the slot or run is known to read as
/// zero (fresh or purged memory).
#[inline(always)]
pub fn calloc(n: usize, m: usize) -> (r: *mut u8)
    ensures
        n * m > usize::MAX ==> r@.addr == 0,
        heap::fits(r, n * m, QUANTUM as int),
{
    calloc_zeroed(n, m).0
}

/// [`calloc`] with the proof that its memory reads as zero: a non-null
/// result comes with a [`Zeroed`] token for its `n * m` bytes (the
/// "Zeroed memory" theorem of `verus/TODO.md`), from the `memset` or from
/// the allocator's report that the slot or run is known zero.
#[inline(always)]
pub fn calloc_zeroed(n: usize, m: usize) -> (r: (*mut u8, Tracked<Zeroed>))
    ensures
        n * m > usize::MAX ==> r.0@.addr == 0,
        heap::fits(r.0, n * m, QUANTUM as int),
        r.0@.addr != 0 ==> r.1@.covers(r.0, n * m),
{
    let Some(size) = n.checked_mul(m) else { return (enomem(), Tracked(Zeroed::empty(0))) };
    let shared = super::shared();
    proof {
        crate::meta::lemma_is_pow2_one();
        shared.lemma_regions_ok();
    }
    let (p, zeroed, z) = match class_index(size) {
        Some(c) => {
            let r = heap::alloc_zeroed(shared, c);
            proof {
                lemma_class_fits(r.0, c as int, size);
            }
            r
        },
        None => {
            let r = alloc_huge(shared, size, 1);
            proof {
                lemma_huge_result(r.0, size, 1);
            }
            r
        },
    };
    if p.is_null() {
        return (enomem(), Tracked(Zeroed::empty(0)));
    }
    if !zeroed {
        proof {
            broadcast use vstd::layout::align_of_u8;
            crate::vspec::addr::axiom_fresh_block(p, size);
        }
        // SAFETY: `p` is a fresh allocation of at least `size` bytes.
        let z = unsafe { crate::vspec::zero::write_zero(p, size) };
        return (p, z);
    }
    (p, z)
}

/// `free`. Null is a no-op; anything that is not a live allocation aborts.
#[inline(always)]
pub fn free(p: *mut u8)
    ensures
        p@.addr != 0 ==> heap::free_target(p),
{
    if p.is_null() {
        return;
    }
    // As in `malloc`: the check-and-push has no calls, and every other
    // outcome, including the aborts, is reached through one tail call.
    if let Some(h) = heap::existing_heap(super::shared_ptr()) {
        // SAFETY: this thread's heap exists, so this thread observed
        // `READY` when it created the heap.
        let shared = unsafe { super::shared_unchecked() };
        if heap::free_fast(shared, h, p, shared.push_flags()) {
            return;
        }
    }
    free_slow(p)
}

/// Sized free on the calling thread's heap: `hint` is the class the size
/// (and alignment) name, `class_for(size, align)`, or `None` for a huge
/// size. On the fast path the hint replaces the slab descriptor's bit test
/// (design 6.3): a slot whose class matches is pushed at once, so an
/// invalid or double free through this path is caught when the cache
/// flushes it, not at the call; a hint that does not match aborts.
#[inline(always)]
pub fn free_sized_hint(p: *mut u8, size: usize, hint: Option<usize>)
    requires
        hint matches Some(c) ==> c < NCLASSES,
    ensures
        p@.addr != 0 ==> heap::free_target(p),
{
    // A huge size (no hint) has no fast path: straight to the slow one,
    // so the fast path carries a plain class and no test of the hint.
    let Some(c) = hint else {
        return free_sized_unhinted(p, size);
    };
    if p.is_null() {
        return;
    }
    if let Some(h) = heap::existing_heap(super::shared_ptr()) {
        // SAFETY: as in `free`.
        let shared = unsafe { super::shared_unchecked() };
        if heap::free_sized_fast(shared, h, p, Some(c), shared.push_flags()) {
            return;
        }
    }
    free_sized_slow(p, size, hint)
}

/// The sized free of a size no class holds (a huge run, or nothing): the
/// exact `free` behind the null test.
#[cold]
#[inline(never)]
fn free_sized_unhinted(p: *mut u8, size: usize)
    ensures
        p@.addr != 0 ==> heap::free_target(p),
{
    if !p.is_null() {
        free_sized_slow(p, size, None)
    }
}

/// Off the sized fast path: the hint mismatch abort, then everything the
/// exact `free` does (so a full cache or a huge run still gets the exact
/// check, and a bad pointer the usual message).
#[cold]
#[inline(never)]
fn free_sized_slow(p: *mut u8, size: usize, hint: Option<usize>)
    ensures
        heap::free_target(p),
{
    match super::shared_if_ready() {
        Some(shared) => {
            shared.check_size_hint(p, size, hint);
            heap::free(shared, p)
        }
        None => free_before_init(p),
    }
}

/// C23 `free_sized(p, size)`: `size` must be the size requested from
/// `malloc`/`calloc`/`realloc` (or any size in the same class).
#[inline(always)]
pub fn free_sized(p: *mut u8, size: usize)
    ensures
        p@.addr != 0 ==> heap::free_target(p),
{
    free_sized_hint(p, size, class_index(size))
}

/// C23 `free_aligned_sized(p, align, size)`: the arguments given to
/// `aligned_alloc`. An alignment that is not a power of two matches no
/// class and aborts on a slab pointer.
pub fn free_aligned_sized(p: *mut u8, align: usize, size: usize)
    ensures
        p@.addr != 0 ==> heap::free_target(p),
{
    let hint = if align.is_power_of_two() { class_for(size, align) } else { None };
    free_sized_hint(p, size, hint)
}

/// Everything `free` does off its fast path: heap creation, a full cache,
/// huge runs, and the aborts for invalid pointers (the checks are repeated
/// from scratch here, so a fast-path `false` for a bad pointer ends in the
/// same message it always did).
#[cold]
#[inline(never)]
fn free_slow(p: *mut u8)
    ensures
        heap::free_target(p),
{
    match super::shared_if_ready() {
        Some(shared) => heap::free(shared, p),
        None => free_before_init(p),
    }
}

/// The abort for a pointer freed (or queried) before any allocation.
#[cold]
#[inline(never)]
fn free_before_init(p: *mut u8) -> ! {
    crate::sys::abort_fmt(format_args!(
        "{:#x} passed before any allocation: not an allocation",
        p as usize
    ))
}

/// Usable bytes of the live allocation `p`. Aborts unless `p` is a live
/// slab slot or huge run start. A slab pointer takes the `free` fast
/// path's route (design 6.3): `Regions::classify` finds class, slab and
/// slot from the dense arrays, `check_live` tests the bit, and the size is
/// the class's. A huge run's is what its descriptor records
/// ([`crate::huge::Huge::usable_size`]).
#[inline(always)]
pub fn usable_size(shared: &Shared, p: *mut u8) -> (r: usize)
    requires
        shared.wf(),
    ensures
        heap::free_target(p),
        crate::layout::process_regions().is_huge_block(p as usize, 1) ==> shared.huge_run_usable(p as usize, r),
        usable_of(p, r as int),
{
    proof {
        shared.lemma_wf();
        shared.lemma_huge_pages();
    }
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            shared.table(class).check_live(slab as usize, slot as usize, p as usize);
            proof {
                // A slot lies in its class region, which is apart from
                // the huge region.
                lemma_slot_not_huge(crate::layout::process_regions(), class as int, p as usize);
                shared.lemma_regions_ok();
                crate::vspec::usable::lemma_usable_slot(p, class as int, 0);
            }
            class_size(class)
        }
        Kind::Huge { page } => {
            let r = shared.huge.usable_size(page);
            proof {
                shared.lemma_huge_run_usable(p as usize, r);
                crate::vspec::usable::lemma_usable_run(shared, p, r);
            }
            r
        },
        Kind::Invalid => not_an_allocation(p),
    }
}

/// The abort of [`usable_size`] for a pointer outside every allocation,
/// out of line so the query needs no frame for the message.
#[cold]
#[inline(never)]
fn not_an_allocation(p: *mut u8) -> ! {
    crate::sys::abort_fmt(format_args!("{:#x} is not an allocation", p as usize))
}

/// `malloc_usable_size`. Zero for null.
#[inline(always)]
pub fn malloc_usable_size(p: *mut u8) -> (r: usize)
    ensures
        p@.addr == 0 ==> r == 0,
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> usable_of(p, r as int),
{
    if p.is_null() {
        return 0;
    }
    match super::shared_if_ready() {
        Some(shared) => usable_size(shared, p),
        None => free_before_init(p),
    }
}

/// The class [`class_for`] picks for `align` has a size that `align`
/// divides: a multiple of the quantum at or below it, of `align` above.
pub(super) proof fn lemma_class_aligned(c: usize, align: usize)
    requires
        c < NCLASSES,
        crate::meta::is_pow2(align),
        align <= QUANTUM || class_table()[c as int].size as usize & (align - 1) as usize == 0,
    ensures
        (class_table()[c as int].size as int) % (align as int) == 0,
{
    crate::vspec::class::axiom_class_table_wf(c as int);
    crate::meta::lemma_pow2_bits(align);
    let s = class_table()[c as int].size as usize;
    if align <= QUANTUM {
        crate::layout::lemma_class_size_quantum(c as int);
        crate::heap::lemma_quantum_pow2();
        let k = choose|k: nat| k < 64 && QUANTUM as nat == #[trigger] vstd::arithmetic::power2::pow2(k);
        vstd::arithmetic::div_mod::lemma_mod_self_0(align as int);
        crate::layout::lemma_pow2_divides_pow2(align, align as int, k);
        crate::layout::lemma_aligned_down(s as int, QUANTUM as int, align as int);
    } else {
        assert(s & ((align - 1) as usize) == 0 ==> s % align == 0) by (bit_vector)
            requires
                align > 0,
                align & sub(align, 1) == 0,
        ;
    }
}

/// What `realloc` and `rallocx` return when they keep `p` for `size`
/// bytes at `align` ([`old_size_or_keep`]): in the process's regions,
/// well formed and separated, a multiple of `align` and of the quantum,
/// and either a slot of a class at least `size` bytes large (the slab
/// half of `Regions::user_block`, so `heap::fits` for `size` at `align`)
/// or a page start of the huge region. A kept run's size bound, `size`
/// at most the run's usable bytes, is the huge table's: its descriptors
/// state no bound on the usable bytes against the region, so `size`
/// bytes of the region from `p` is not stated.
pub open spec fn kept(p: *mut u8, size: int, align: int) -> bool {
    &&& heap::regions_ok()
    &&& (p@.addr as int) % align == 0
    &&& (p@.addr as int) % (QUANTUM as int) == 0
    &&& (exists|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr) && size <= class_table()[c].size)
        || crate::layout::process_regions().is_huge_block(p@.addr, 1)
}

/// The reallocation decision shared by `realloc` and `rallocx`: `None`
/// when the live allocation `p` already serves `size` bytes at `align`
/// (a power of two; 1 or the quantum means the natural alignment) and
/// keeps its pointer, that is when the new size maps to `p`'s class
/// (whose slots satisfy the alignment by construction) or to a huge run
/// of the same number of pages at a start that satisfies it ([`kept`]);
/// otherwise `Some(old)`, the usable bytes of `p`, the most a move has to
/// copy: the class's size for a slot. Aborts, naming `what`, unless `p`
/// is a live slab slot or huge run start; so it returns only for a free
/// target.
pub(super) fn old_size_or_keep(shared: &Shared, what: &str, p: *mut u8, size: usize, align: usize) -> (r: Option<usize>)
    requires
        shared.wf(),
        is_pow2(align),
    ensures
        heap::free_target(p),
        r is None ==> kept(p, size as int, align as int),
        r matches Some(o) ==> forall|c: int|
            #[trigger] crate::layout::process_regions().is_slot(c, p@.addr) ==> o == class_table()[c].size,
{
    proof {
        shared.lemma_wf();
        shared.lemma_huge_pages();
        shared.lemma_regions_ok();
        lemma_pow2_pos(align);
    }
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            shared.table(class).check_live(slab as usize, slot as usize, p as usize);
            if class_for(size, align) == Some(class) {
                proof {
                    lemma_class_aligned(class, align);
                    heap::lemma_slot_fits(p, class as int, size as int, align);
                    heap::lemma_slot_fits_quantum(p, class as int, size as int);
                }
                return None;
            }
            proof {
                // A slot belongs to one class.
                assert forall|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr)
                    implies c == class as int by {
                    crate::vspec::zero::lemma_slot_unique(class as int, c, p@.addr);
                }
            }
            Some(class_size(class))
        }
        Kind::Huge { page } => {
            let old = shared.huge.usable_size(page);
            proof {
                // A page start of the huge region is no slot.
                assert forall|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr)
                    implies false by {
                    crate::vspec::zero::lemma_slot_not_huge(c, p@.addr);
                }
            }
            if class_for(size, align).is_none() && shared.huge.usable_for(size) == Some(old) && (p as usize) % align == 0 {
                proof {
                    heap::lemma_quantum_pow2();
                    heap::lemma_huge_fits(p, 0, align, QUANTUM);
                }
                return None;
            }
            Some(old)
        }
        Kind::Invalid => crate::sys::abort_fmt(format_args!("{what} of {:#x}: not an allocation", p as usize)),
    }
}

/// `realloc`. The same pointer comes back when the new size maps to the
/// same class or the same number of huge pages (`old_size_or_keep`);
/// otherwise allocate, copy `min(old usable, new size)`, free. On failure
/// the old block is intact.
pub fn realloc(p: *mut u8, size: usize) -> (r: *mut u8)
    ensures
        p@.addr != 0 && size == 0 ==> r@.addr == 0,
        r != p ==> heap::fits(r, size as int, QUANTUM as int),
        heap::fits(r, size as int, QUANTUM as int) || kept(r, size as int, 1),
        p@.addr != 0 ==> heap::free_target(p),
{
    if p.is_null() {
        return malloc(size);
    }
    if size == 0 {
        free(p);
        return ptr::null_mut();
    }
    let Some(shared) = super::shared_if_ready() else { free_before_init(p) };
    proof {
        crate::meta::lemma_is_pow2_one();
    }
    let Some(old) = old_size_or_keep(shared, "realloc", p, size, 1) else { return p };
    let q = malloc(size);
    if q.is_null() {
        return q;
    }
    proof {
        broadcast use vstd::layout::align_of_u8;
        crate::vspec::addr::axiom_moved_block(p, old, q, size);
    }
    // SAFETY: `p` has `old` readable bytes, `q` at least `size` writable
    // ones, and they are distinct allocations.
    unsafe { ptr::copy_nonoverlapping(p, q, old.min(size)) };
    heap::free(shared, p);
    q
}

/// `reallocarray`: `realloc(p, n * m)` with overflow reported as `ENOMEM`.
pub fn reallocarray(p: *mut u8, n: usize, m: usize) -> (r: *mut u8)
    ensures
        n * m > usize::MAX ==> r@.addr == 0,
        r != p ==> heap::fits(r, n * m, QUANTUM as int),
        heap::fits(r, n * m, QUANTUM as int) || kept(r, n * m, 1),
        p@.addr != 0 && n * m <= usize::MAX ==> heap::free_target(p),
{
    match n.checked_mul(m) {
        Some(size) => realloc(p, size),
        None => enomem(),
    }
}

} // verus!

// ---------------------------------------------------------------------------
// challoc's size-returning entry points (design 13.7): `challoc_malloc_usable`,
// `challoc_calloc_usable`, `challoc_realloc_usable` and `challoc_free_usable`.
// Valkey asks `malloc_usable_size` after every allocation and before every
// unsized `zfree` for its `used_memory` accounting; that second lookup
// (`Regions::classify`, the bitmap-word test, the class size) was the one
// allocator-side term above jemalloc's cost in the Valkey write phase
// on x86_64. These entry points return the usable size from the
// classification the allocation or the free already did, so the query
// disappears and no check is dropped: `challoc_free_usable` keeps the exact
// bit test of plain `free` (it is `free`, reporting the class it verified).
// The fast paths of `malloc`, `free` and `sdallocx` are not touched.
// ---------------------------------------------------------------------------

verus! {

/// Store `n` through `usable` unless it is null (`usable` is optional for
/// the three allocating entry points; a null makes them plain
/// `malloc`/`calloc`/`realloc`), with the [`Stored`] token for the store.
/// Trusted: writes through the caller's pointer, and the token states
/// what it wrote.
#[verifier::external_body]
#[inline(always)]
fn store_usable(usable: *mut usize, n: usize) -> (s: Tracked<Stored>)
    ensures
        s@.ptr() == usable,
        s@.val() == n,
{
    if !usable.is_null() {
        // SAFETY: the caller passed a writable `size_t` or null.
        unsafe { *usable = n };
    }
    Tracked::assume_new()
}

/// `CLASSES[class].size()` for a class a fast path produced: below
/// `NCLASSES` by construction (`class_index`, `checked_slot`), so no bounds
/// check and no panic call sits in the inlined body (design 6.3).
#[inline(always)]
fn class_size(class: usize) -> (r: usize)
    requires
        class < NCLASSES,
    ensures
        r == class_table()[class as int].size,
{
    debug_assert!(class < NCLASSES);
    // SAFETY: as above.
    unsafe { CLASSES.table().get_unchecked(class) }.size()
}

/// `challoc_malloc_usable(size, usable)`: `malloc(size)`, and the block's
/// usable size, what `malloc_usable_size` would return for it, stored
/// through `usable` (left alone when `usable` is null; 0 on a null return).
/// The same pop as `malloc` with the class size added from the class the
/// pop used; everything else is one tail call away, as in `malloc`.
#[inline(always)]
pub fn malloc_usable(size: usize, usable: *mut usize) -> (r: *mut u8)
    ensures
        heap::fits(r, size as int, QUANTUM as int),
{
    malloc_usable_stored(size, usable).0
}

/// [`malloc_usable`] with the token for what it stored through `usable`:
/// the block's usable size ([`usable_is`]: its class's size, or `size`
/// rounded up to whole pages for a huge run). It stores 0 for a null
/// result, which is not stated: the regions' spec does not rule out a
/// slot at address 0, and the fast path's pop states only that it is one.
#[inline(always)]
pub fn malloc_usable_stored(size: usize, usable: *mut usize) -> (r: (*mut u8, Tracked<Stored>))
    ensures
        heap::fits(r.0, size as int, QUANTUM as int),
        r.1@.ptr() == usable,
        r.0@.addr != 0 ==> usable_is(r.0, size as int, r.1@.val() as int),
{
    let Some(c) = class_index(size) else { return malloc_usable_huge(size, usable) };
    let shared = super::shared_ptr();
    let Some(h) = heap::existing_heap(shared) else {
        let (p, s) = malloc_usable_class(c, usable);
        proof {
            lemma_class_fits(p, c as int, size);
            if p@.addr != 0 {
                crate::vspec::usable::lemma_usable_slot(p, c as int, size as int);
            }
        }
        return (p, s);
    };
    // SAFETY: as in `malloc`.
    let flags = unsafe { heap::fast_flags(shared) };
    proof {
        h.lemma_wf_regions();
    }
    match h.alloc_fast(c, flags) {
        Some(p) => {
            let s = store_usable(usable, class_size(c));
            proof {
                lemma_class_fits(p, c as int, size);
                crate::vspec::usable::lemma_usable_slot(p, c as int, size as int);
            }
            (p, s)
        }
        None => {
            let (p, s) = malloc_usable_class(c, usable);
            proof {
                lemma_class_fits(p, c as int, size);
                if p@.addr != 0 {
                    crate::vspec::usable::lemma_usable_slot(p, c as int, size as int);
                }
            }
            (p, s)
        },
    }
}

/// `malloc_usable` off its fast path for a slab class: the refill, the
/// scheduled pop and heap creation (`heap::alloc_declined`, then
/// `Heap::alloc_slow`, dispatch all three),
/// null with `errno = ENOMEM` on failure.
#[cold]
#[inline(never)]
fn malloc_usable_class(class: usize, usable: *mut usize) -> (r: (*mut u8, Tracked<Stored>))
    requires
        class < NCLASSES,
    ensures
        heap::regions_ok(),
        r.0@.addr != 0 ==> crate::layout::process_regions().is_slot(class as int, r.0@.addr),
        r.1@.ptr() == usable,
        r.0@.addr == 0 ==> r.1@.val() == 0,
        r.0@.addr != 0 ==> r.1@.val() == class_table()[class as int].size,
{
    let shared = super::shared();
    proof {
        shared.lemma_regions_ok();
    }
    let p = heap::alloc_declined(shared, class);
    if p.is_null() {
        let s = store_usable(usable, 0);
        return (enomem(), s);
    }
    let s = store_usable(usable, class_size(class));
    (p, s)
}

/// `malloc_usable` of a huge size: the run's usable bytes come from the
/// huge table, the same read `malloc_usable_size` does.
#[cold]
#[inline(never)]
fn malloc_usable_huge(size: usize, usable: *mut usize) -> (r: (*mut u8, Tracked<Stored>))
    ensures
        heap::fits(r.0, size as int, QUANTUM as int),
        r.1@.ptr() == usable,
        r.0@.addr == 0 ==> r.1@.val() == 0,
        r.0@.addr != 0 ==> usable_is(r.0, size as int, r.1@.val() as int),
{
    let shared = super::shared();
    proof {
        crate::meta::lemma_is_pow2_one();
        shared.lemma_regions_ok();
    }
    let (p, _, _) = alloc_huge(shared, size, 1);
    proof {
        lemma_huge_result(p, size, 1);
    }
    if p.is_null() {
        let s = store_usable(usable, 0);
        return (enomem(), s);
    }
    let s = store_usable(usable, usable_size(shared, p));
    proof {
        // The run `alloc_huge` just returned, still this caller's: its
        // descriptor records `size` rounded up to whole pages.
        lemma_huge_usable(shared, p, size, s@.val());
    }
    (p, s)
}

/// The usable size [`usable_size`] reads for the run `alloc_huge` just
/// returned for `size` bytes, still the caller's, is `size` rounded up to
/// whole pages ([`crate::vspec::huge::axiom_owned_run`]).
proof fn lemma_huge_usable(shared: &Shared, p: *mut u8, size: usize, u: usize)
    requires
        shared.wf(),
        p@.addr != 0,
        crate::layout::huge_block(p@.addr, size, 1),
        crate::huge::allocated(p@.addr, size),
        crate::layout::process_regions().is_huge_block(p@.addr, 1) ==> shared.huge_run_usable(p@.addr, u),
    ensures
        crate::layout::process_regions().is_huge_block(p@.addr, 1),
        size <= u,
        u == crate::vspec::zero::huge_usable(size as int),
        usable_is(p, size as int, u as int),
{
    shared.lemma_wf();
    shared.lemma_regions_ok();
    assert(crate::layout::process_regions().is_huge_block(p@.addr, 1));
    crate::vspec::huge::axiom_owned_run(shared, p@.addr, size, u);
    shared.lemma_huge_usable(size);
    crate::vspec::usable::lemma_usable_huge(p, size as int);
}

/// `challoc_calloc_usable(n, m, usable)`: `calloc(n, m)` with the usable
/// size stored as in [`malloc_usable`]. The whole usable size reads as
/// zero, not only the `n * m` bytes asked for: the caller is told it owns
/// the usable bytes (Valkey's `zcalloc_usable` hands them on), and that is
/// what jemalloc's `calloc` zeroes.
pub fn calloc_usable(n: usize, m: usize, usable: *mut usize) -> (r: *mut u8)
    ensures
        n * m > usize::MAX ==> r@.addr == 0,
        heap::fits(r, n * m, QUANTUM as int),
{
    calloc_usable_zeroed(n, m, usable).0
}

/// [`calloc_usable`] with the proof that its memory reads as zero, and
/// the token for what it stored through `usable`. A non-null result comes
/// with a [`Zeroed`] token for its `n * m` bytes and for its whole usable
/// size ([`covers_usable`]), and `usable` holds that size ([`usable_is`]:
/// the class's size, or `n * m` rounded up to whole pages); a null result
/// stores 0. For a huge request the `memset` length is the run's usable
/// size read back from its descriptor ([`usable_size`]), which is the
/// request rounded up to whole pages while the block is this caller's
/// ([`crate::vspec::huge::axiom_owned_run`]).
#[inline(always)]
pub fn calloc_usable_zeroed(n: usize, m: usize, usable: *mut usize) -> (r: (*mut u8, Tracked<Zeroed>, Tracked<Stored>))
    ensures
        n * m > usize::MAX ==> r.0@.addr == 0,
        heap::fits(r.0, n * m, QUANTUM as int),
        r.0@.addr != 0 ==> r.1@.covers(r.0, n * m),
        r.0@.addr != 0 ==> covers_usable(r.1@, r.0, 0, n * m),
        r.2@.ptr() == usable,
        r.0@.addr == 0 ==> r.2@.val() == 0,
        r.0@.addr != 0 ==> usable_is(r.0, n * m, r.2@.val() as int),
{
    let Some(size) = n.checked_mul(m) else {
        let s = store_usable(usable, 0);
        return (enomem(), Tracked(Zeroed::empty(0)), s);
    };
    let shared = super::shared();
    proof {
        crate::meta::lemma_is_pow2_one();
        shared.lemma_regions_ok();
    }
    // The class `p` is a slot of, or -1 for a huge run.
    let ghost mut cls: int = -1;
    let (p, zeroed, u, z) = match class_index(size) {
        Some(c) => {
            let (p, zeroed, z) = heap::alloc_zeroed(shared, c);
            proof {
                cls = c as int;
                lemma_class_fits(p, c as int, size);
                if p@.addr != 0 {
                    crate::vspec::usable::lemma_usable_slot(p, c as int, size as int);
                    if zeroed {
                        crate::vspec::zero::lemma_covers_slot(z@, p, c as int, 0, size as int);
                    }
                }
            }
            (p, zeroed, class_size(c), z)
        }
        None => {
            let (p, zeroed, z) = alloc_huge(shared, size, 1);
            proof {
                lemma_huge_result(p, size, 1);
            }
            let u = if p.is_null() { 0 } else { usable_size(shared, p) };
            proof {
                if p@.addr != 0 {
                    // The run `alloc_huge` just returned, still this
                    // caller's.
                    lemma_huge_usable(shared, p, size, u);
                    if zeroed {
                        shared.lemma_huge_usable(size);
                        shared.lemma_wf();
                        crate::vspec::zero::lemma_covers_huge(z@, p, 0, size as int);
                    }
                }
            }
            (p, zeroed, u, z)
        }
    };
    if p.is_null() {
        let s = store_usable(usable, 0);
        return (enomem(), Tracked(Zeroed::empty(0)), s);
    }
    let z = if !zeroed {
        proof {
            broadcast use vstd::layout::align_of_u8;
            crate::vspec::addr::axiom_fresh_block(p, u);
        }
        // SAFETY: `p` is a fresh allocation of `u` usable bytes.
        let z = unsafe { crate::vspec::zero::write_zero(p, u) };
        proof {
            // `u` is the block's usable size.
            if cls >= 0 {
                crate::vspec::zero::lemma_covers_slot(z@, p, cls, 0, size as int);
            } else {
                crate::vspec::zero::lemma_covers_huge(z@, p, 0, size as int);
            }
        }
        z
    } else {
        z
    };
    let s = store_usable(usable, u);
    (p, z, s)
}

/// `challoc_realloc_usable(p, size, usable)`: `realloc(p, size)` with the
/// usable size of the returned block stored as in [`malloc_usable`] (0
/// when null comes back, `realloc(p, 0)` included). The block kept in
/// place reports its class size from the classification `realloc` does
/// anyway; a moved block reports what the `malloc_usable` that made it
/// reported.
pub fn realloc_usable(p: *mut u8, size: usize, usable: *mut usize) -> (r: *mut u8)
    ensures
        p@.addr != 0 && size == 0 ==> r@.addr == 0,
        r != p ==> heap::fits(r, size as int, QUANTUM as int),
        heap::fits(r, size as int, QUANTUM as int) || kept(r, size as int, 1),
        p@.addr != 0 ==> heap::free_target(p),
{
    realloc_usable_stored(p, size, usable).0
}

/// [`realloc_usable`] with the token for what it stored through `usable`:
/// for a non-null result, its usable size for a request of `size` bytes
/// ([`usable_is`]), whether the block was kept (the class's size, or a
/// run's usable size equal to `size` rounded up to whole pages: [`kept`]'s
/// test) or moved (what [`malloc_usable_stored`] stored).
pub fn realloc_usable_stored(p: *mut u8, size: usize, usable: *mut usize) -> (r: (*mut u8, Tracked<Stored>))
    ensures
        p@.addr != 0 && size == 0 ==> r.0@.addr == 0,
        r.0 != p ==> heap::fits(r.0, size as int, QUANTUM as int),
        heap::fits(r.0, size as int, QUANTUM as int) || kept(r.0, size as int, 1),
        p@.addr != 0 ==> heap::free_target(p),
        r.1@.ptr() == usable,
        r.0@.addr != 0 ==> usable_is(r.0, size as int, r.1@.val() as int),
{
    if p.is_null() {
        return malloc_usable_stored(size, usable);
    }
    if size == 0 {
        free(p);
        let s = store_usable(usable, 0);
        return (ptr::null_mut(), s);
    }
    let Some(shared) = super::shared_if_ready() else { free_before_init(p) };
    let (keep, old) = realloc_old(shared, p, size);
    if keep {
        let s = store_usable(usable, old);
        return (p, s);
    }
    let (q, s) = malloc_usable_stored(size, usable);
    if q.is_null() {
        return (q, s);
    }
    proof {
        broadcast use vstd::layout::align_of_u8;
        crate::vspec::addr::axiom_moved_block(p, old, q, size);
    }
    // SAFETY: `p` has `old` readable bytes, `q` at least `size` writable
    // ones, and they are distinct allocations.
    unsafe { ptr::copy_nonoverlapping(p, q, old.min(size)) };
    heap::free(shared, p);
    (q, s)
}

/// [`old_size_or_keep`]'s decision for `realloc` (alignment 1) with the
/// old usable size reported in both outcomes: `(keep, usable)`, `keep`
/// when `size` maps to `p`'s class or to a huge run of the same page
/// count ([`kept`]). The same checks and aborts as `old_size_or_keep`, in
/// a function of its own so that `realloc` and `rallocx` keep their code.
/// The usable size is `p`'s ([`usable_of`]) and, for a kept block, the
/// usable size of a request of `size` bytes ([`usable_is`]).
fn realloc_old(shared: &Shared, p: *mut u8, size: usize) -> (r: (bool, usize))
    requires
        shared.wf(),
    ensures
        heap::free_target(p),
        r.0 ==> kept(p, size as int, 1),
        usable_of(p, r.1 as int),
        r.0 ==> usable_is(p, size as int, r.1 as int),
{
    proof {
        shared.lemma_wf();
        shared.lemma_huge_pages();
        shared.lemma_regions_ok();
    }
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            shared.table(class).check_live(slab as usize, slot as usize, p as usize);
            proof {
                crate::vspec::class::axiom_class_table_wf(class as int);
                if size <= class_table()[class as int].size {
                    heap::lemma_slot_fits_quantum(p, class as int, size as int);
                }
                crate::vspec::usable::lemma_usable_slot(p, class as int, size as int);
            }
            (class_of(size) == Some(class), class_size(class))
        }
        Kind::Huge { page } => {
            let old = shared.huge.usable_size(page);
            proof {
                crate::meta::lemma_is_pow2_one();
                heap::lemma_quantum_pow2();
                heap::lemma_huge_fits(p, 0, 1, QUANTUM);
                shared.lemma_huge_run_usable(p as usize, old);
                crate::vspec::usable::lemma_usable_run(shared, p, old);
                shared.lemma_huge_usable(size);
                crate::vspec::usable::lemma_usable_huge(p, size as int);
            }
            (class_of(size).is_none() && shared.huge.usable_for(size) == Some(old), old)
        }
        Kind::Invalid => crate::sys::abort_fmt(format_args!("realloc of {:#x}: not an allocation", p as usize)),
    }
}

/// `challoc_free_usable(p)`: `free(p)`, returning the usable size the
/// block had (0 for null). The exact fast path of `free` (design 6.3
/// steps 1 to 4: classify, the bitmap-word test, the owner word with
/// several arenas, the push), reporting the class it verified; every other
/// outcome, the aborts included, goes through one cold call that takes the
/// size from the same classification the exact `free` then repeats, so a
/// bad pointer dies with `free`'s message.
#[inline(always)]
pub fn free_usable(p: *mut u8) -> (r: usize)
    ensures
        p@.addr == 0 ==> r == 0,
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> usable_of(p, r as int),
{
    if p.is_null() {
        return 0;
    }
    if let Some(h) = heap::existing_heap(super::shared_ptr()) {
        // SAFETY: as in `free`.
        let shared = unsafe { super::shared_unchecked() };
        if let Some(class) = heap::free_fast_class(shared, h, p, shared.push_flags()) {
            proof {
                shared.lemma_regions_ok();
                crate::vspec::usable::lemma_usable_slot(p, class as int, 0);
            }
            return class_size(class);
        }
    }
    free_usable_slow(p)
}

/// Everything `free_usable` does off its fast path: the size first, from
/// the classification `free` makes (its aborts for an invalid, stale or
/// double free, and the huge table's for a run that is not live), then the
/// exact `free` (heap creation, a full cache, another arena, the huge
/// release).
///
#[cold]
#[inline(never)]
fn free_usable_slow(p: *mut u8) -> (r: usize)
    ensures
        heap::free_target(p),
        usable_of(p, r as int),
{
    let Some(shared) = super::shared_if_ready() else { free_before_init(p) };
    proof {
        shared.lemma_wf();
        shared.lemma_regions_ok();
    }
    let usable = match shared.classify_free(p) {
        Ok((class, _)) => {
            proof {
                crate::vspec::usable::lemma_usable_slot(p, class as int, 0);
            }
            class_size(class)
        },
        Err(page) => {
            let u = shared.huge.usable_size(page);
            proof {
                shared.lemma_huge_run_usable(p@.addr, u);
                crate::vspec::usable::lemma_usable_run(shared, p, u);
            }
            u
        },
    };
    heap::free(shared, p);
    usable
}

/// Class `c` serves `size` bytes at `align`: its slots hold `size` bytes
/// and, above the quantum, its size is a multiple of `align` (the mask
/// test [`class_for`] makes, for a power of two).
pub open spec fn class_serves(c: int, size: int, align: usize) -> bool {
    &&& size <= class_table()[c].size
    &&& align <= QUANTUM || class_table()[c].size as usize & (align - 1) as usize == 0
}

/// `c` is the smallest class that serves `size` bytes at `align`.
pub open spec fn least_class(c: int, size: int, align: usize) -> bool {
    &&& 0 <= c < NCLASSES
    &&& class_serves(c, size, align)
    &&& forall|j: int| 0 <= j < c ==> !#[trigger] class_serves(j, size, align)
}

/// What [`class_for`] returns for a power-of-two `align`: the smallest
/// class that serves `size` bytes at `align`, `None` when no class does.
pub open spec fn spec_class_for(size: usize, align: usize) -> Option<usize> {
    if exists|c: int| least_class(c, size as int, align) {
        Some((choose|c: int| least_class(c, size as int, align)) as usize)
    } else {
        None
    }
}

/// The smallest serving class is unique.
proof fn lemma_class_for_some(size: usize, align: usize, c: int)
    requires
        least_class(c, size as int, align),
    ensures
        spec_class_for(size, align) == Some(c as usize),
{
    let d = choose|d: int| least_class(d, size as int, align);
    if d < c {
        assert(!class_serves(d, size as int, align));
    } else if c < d {
        assert(!class_serves(c, size as int, align));
    }
}

/// No class serves `size` bytes at `align`.
proof fn lemma_class_for_none(size: usize, align: usize)
    requires
        forall|j: int| 0 <= j < NCLASSES ==> !#[trigger] class_serves(j, size as int, align),
    ensures
        spec_class_for(size, align) is None,
{
    if exists|c: int| least_class(c, size as int, align) {
        let c = choose|c: int| least_class(c, size as int, align);
        assert(class_serves(c, size as int, align));
    }
}

/// The class that serves `size` bytes at `align` (a power of two; 1 or
/// the quantum means the natural alignment): the smallest class at or
/// above `size` whose slot size is a multiple of `align`
/// ([`spec_class_for`]). `None` when the request is a huge run, either
/// because `size` is above `MEDIUM_MAX` or because `align` is. Shared by
/// `aligned_alloc`, `mallocx`, `rallocx` and `nallocx`. The mask in the second postcondition says "a multiple
/// of `align`" because every caller passes a power of two.
#[inline]
pub(super) fn class_for(size: usize, align: usize) -> (r: Option<usize>)
    ensures
        r matches Some(c) ==> c < NCLASSES && size <= class_table()[c as int].size,
        r matches Some(c) ==> align <= QUANTUM || class_table()[c as int].size as usize & (align - 1) as usize == 0,
        size <= MEDIUM_MAX && align <= QUANTUM ==> r is Some,
        size > MEDIUM_MAX || align > MEDIUM_MAX ==> r is None,
        is_pow2(align) ==> r == spec_class_for(size, align),
{
    proof {
        if size > MEDIUM_MAX {
            assert forall|j: int| 0 <= j < NCLASSES implies !#[trigger] class_serves(j, size as int, align) by {
                crate::vspec::class::axiom_class_table_wf(j);
            }
            lemma_class_for_none(size, align);
        }
    }
    let c = class_index(size)?;
    if align <= QUANTUM {
        proof {
            assert forall|j: int| 0 <= j < c implies !#[trigger] class_serves(j, size as int, align) by {
                assert(class_table()[j].size < size);
            }
            lemma_class_for_some(size, align, c as int);
        }
        return Some(c);
    }
    if align > MEDIUM_MAX {
        proof {
            if is_pow2(align) {
                // A class size is below `align` and not zero, so not a
                // multiple of it.
                assert forall|j: int| 0 <= j < NCLASSES implies !#[trigger] class_serves(j, size as int, align) by {
                    crate::vspec::class::axiom_class_table_wf(j);
                    lemma_mask_mod(class_table()[j].size as usize, align);
                    let s = class_table()[j].size as usize;
                    if s & ((align - 1) as usize) == 0 {
                        vstd::arithmetic::div_mod::lemma_small_mod(s as nat, align as nat);
                    }
                }
                lemma_class_for_none(size, align);
            }
        }
        return None;
    }
    // The power-of-two class at or above `max(size, align)` qualifies and
    // is at most `MEDIUM_MAX`, so this stops inside the table; `get` (not
    // an index) so that no bounds-check panic call sits in the functions
    // this is inlined into (`sdallocx`: a call in the body costs the fast
    // path a frame it otherwise needs no part of, design 6.3), and `align`
    // is a power of two so the divisibility test is a mask.
    let ghost c0 = c;
    let mut c = c;
    loop
        invariant
            c0 <= c <= NCLASSES,
            c0 < NCLASSES,
            QUANTUM < align <= MEDIUM_MAX,
            size <= MEDIUM_MAX,
            forall|j: int| c0 <= j < NCLASSES ==> size <= #[trigger] class_table()[j].size,
            forall|j: int| 0 <= j < c0 ==> #[trigger] class_table()[j].size < size,
            forall|j: int| 0 <= j < c ==> !#[trigger] class_serves(j, size as int, align),
        decreases NCLASSES - c,
    {
        proof {
            if c >= NCLASSES {
                lemma_class_for_none(size, align);
            }
        }
        let cls = CLASSES.table().get(c)?;
        if cls.size() & (align - 1) == 0 {
            proof {
                lemma_class_for_some(size, align, c as int);
            }
            return Some(c);
        }
        c += 1;
    }
}

/// Allocate `size` bytes aligned to `align`, a power of two. Null with
/// `errno = ENOMEM` on exhaustion.
pub fn alloc_aligned(align: usize, size: usize) -> (r: *mut u8)
    requires
        is_pow2(align),
    ensures
        heap::fits(r, size as int, align as int),
{
    debug_assert!(align.is_power_of_two());
    if align <= QUANTUM {
        let p = malloc(size);
        proof {
            heap::lemma_quantum_pow2();
            let k = choose|k: nat| k < 64 && QUANTUM as nat == #[trigger] pow2(k);
            assert(is_pow2(QUANTUM));
            lemma_fits_down(p, size as int, QUANTUM, align);
        }
        return p;
    }
    let shared = super::shared();
    proof {
        shared.lemma_regions_ok();
    }
    let p = match class_for(size, align) {
        Some(c) => {
            let p = heap::alloc(shared, c);
            proof {
                if p@.addr != 0 {
                    crate::vspec::class::axiom_class_table_wf(c as int);
                    lemma_mask_mod(class_table()[c as int].size as usize, align);
                    heap::lemma_slot_fits(p, c as int, size as int, align);
                }
            }
            p
        },
        None => {
            let p = alloc_huge(shared, size, align).0;
            proof {
                lemma_huge_result(p, size, align);
            }
            p
        },
    };
    if p.is_null() { enomem() } else { p }
}

/// A result aligned to the quantum is aligned to every smaller power of
/// two.
proof fn lemma_fits_down(p: *mut u8, size: int, big: usize, align: usize)
    requires
        heap::fits(p, size, big as int),
        is_pow2(big),
        is_pow2(align),
        align <= big,
    ensures
        heap::fits(p, size, align as int),
{
    if p@.addr != 0 {
        let k = choose|k: nat| k < 64 && big as nat == #[trigger] pow2(k);
        vstd::arithmetic::power2::lemma_pow2_pos(k);
        lemma_pow2_pos(align);
        vstd::arithmetic::div_mod::lemma_mod_self_0(align as int);
        crate::layout::lemma_pow2_divides_pow2(align, align as int, k);
        crate::layout::lemma_aligned_down(p@.addr as int, big as int, align as int);
    }
}

/// For a power of two `a`, `x & (a - 1) == 0` says `a` divides `x`.
proof fn lemma_mask_mod(x: usize, a: usize)
    requires
        is_pow2(a),
    ensures
        x & ((a - 1) as usize) == 0 ==> (x as int) % (a as int) == 0,
{
    crate::meta::lemma_pow2_bits(a);
    assert(x & ((a - 1) as usize) == 0 ==> x % a == 0) by (bit_vector)
        requires
            a > 0,
            a & sub(a, 1) == 0,
    ;
}

/// `aligned_alloc` and `memalign`: `EINVAL` unless `align` is a power of
/// two. glibc does not require `size` to be a multiple of `align`.
pub fn aligned_alloc(align: usize, size: usize) -> (r: *mut u8)
    ensures
        !is_pow2(align) ==> r@.addr == 0,
        is_pow2(align) ==> heap::fits(r, size as int, align as int),
{
    if !align.is_power_of_two() {
        return einval();
    }
    alloc_aligned(align, size)
}

/// Store the allocation through `posix_memalign`'s out-pointer.
/// Trusted: writes through the caller's pointer.
#[verifier::external_body]
#[inline(always)]
fn store_out(out: *mut *mut u8, p: *mut u8) {
    // SAFETY: the caller passes a valid out-pointer.
    unsafe { *out = p };
}

/// `posix_memalign`. Returns 0, `EINVAL` (alignment not a power of two or
/// not a multiple of `sizeof(void *)`) or `ENOMEM`; never touches `errno`
/// on the `EINVAL` path.
pub fn posix_memalign(out: *mut *mut u8, align: usize, size: usize) -> (r: c_int)
    ensures
        r == 0 || r == ENOMEM || r == EINVAL,
        // `sizeof(void *)` is 8 on every target challoc builds for.
        r == EINVAL <==> !(is_pow2(align) && align % 8 == 0),
{
    if !align.is_power_of_two() || align % core::mem::size_of::<*mut u8>() != 0 {
        return EINVAL;
    }
    let p = alloc_aligned(align, size);
    if p.is_null() {
        return ENOMEM;
    }
    store_out(out, p);
    0
}

/// `shared.regions.page_size()`, the process's page (`1 << lg_page` of
/// its regions), a power of two: `Regions::wf` (part of `Shared::wf`)
/// bounds `lg_page` below 64.
#[inline(always)]
fn page_size(shared: &Shared) -> (r: usize)
    requires
        shared.wf(),
    ensures
        is_pow2(r),
        r == 1usize << crate::layout::process_regions().lg_page,
{
    proof {
        shared.lemma_wf();
    }
    let r = shared.regions.page_size();
    proof {
        let k = shared.regions.lg_page as nat;
        vstd::arithmetic::power2::lemma2_to64();
        vstd::arithmetic::power2::lemma2_to64_rest();
        if k < 63 {
            vstd::arithmetic::power2::lemma_pow2_strictly_increases(k, 63);
        }
        vstd::bits::lemma_usize_shl_is_mul(1, k as usize);
        assert(r as nat == vstd::arithmetic::power2::pow2(k));
    }
    r
}

/// `valloc`: page-aligned.
pub fn valloc(size: usize) -> (r: *mut u8)
    ensures
        heap::fits(r, size as int, (1usize << crate::layout::process_regions().lg_page) as int),
{
    let page = page_size(super::shared());
    alloc_aligned(page, size)
}

/// `pvalloc`: page-aligned, size rounded up to a whole page.
pub fn pvalloc(size: usize) -> (r: *mut u8)
    ensures
        heap::fits(r, size as int, (1usize << crate::layout::process_regions().lg_page) as int),
{
    let page = page_size(super::shared());
    proof { lemma_pow2_pos(page); }
    let Some(rounded) = size.checked_add(page - 1) else { return enomem() };
    let p = alloc_aligned(page, rounded & !(page - 1));
    proof {
        let m = rounded & !((page - 1) as usize);
        crate::meta::lemma_pow2_bits(page);
        assert(m >= size) by (bit_vector)
            requires
                page > 0,
                page & sub(page, 1) == 0,
                rounded == size + (page - 1),
                m == rounded & !((page - 1) as usize),
        ;
        if p@.addr != 0 {
            lemma_user_block_shrink(p@.addr, m as int, size as int);
        }
    }
    p
}

/// A block for `big` bytes is one for `small <= big`.
proof fn lemma_user_block_shrink(p: usize, big: int, small: int)
    requires
        crate::layout::process_regions().user_block(p, big),
        0 <= small <= big,
    ensures
        crate::layout::process_regions().user_block(p, small),
{
    let r = crate::layout::process_regions();
    if exists|c: int| #[trigger] r.is_slot(c, p) && big <= class_table()[c].size {
        let c = choose|c: int| #[trigger] r.is_slot(c, p) && big <= class_table()[c].size;
        assert(r.is_slot(c, p) && small <= class_table()[c].size);
    }
}

/// `malloc_trim`: return the calling thread's cached slots to their slabs,
/// then purge every `Empty` slab and huge extent whatever its age (design
/// section 8). Returns 1 if anything was purged, 0 otherwise or before
/// init. `pad` is glibc's top-of-heap allowance and has no meaning here.
pub fn malloc_trim(_pad: usize) -> (r: c_int)
    ensures
        r == 0 || r == 1,
{
    let Some(shared) = super::shared_if_ready() else { return 0 };
    if let Some(h) = heap::existing_heap(heap::shared_ptr(shared)) {
        h.flush_all(shared);
    }
    (crate::purge::purge_all(shared) > 0) as c_int
}

} // verus!

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see the module docs).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    use super::*;
    use core::ffi::c_void;

    verus! {

    // `malloc` and `free` each sit alone in a 64-byte-aligned section on
    // x86_64 (the `global_asm!` below); their `je_` twins stay in `.text`.

    #[unsafe(no_mangle)]
    #[cfg_attr(target_arch = "x86_64", unsafe(link_section = ".text.challoc.malloc"))]
    pub extern "C" fn malloc(size: usize) -> *mut c_void {
        super::malloc(size) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn je_malloc(size: usize) -> *mut c_void {
        super::malloc(size) as *mut c_void
    }

    #[unsafe(no_mangle)]
    #[cfg_attr(target_arch = "x86_64", unsafe(link_section = ".text.challoc.free"))]
    pub extern "C" fn free(p: *mut c_void) {
        super::free(p as *mut u8)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn je_free(p: *mut c_void) {
        super::free(p as *mut u8)
    }

    } // verus!

    // The alignment of the sections `malloc` and `free` are placed in on
    // x86_64: a cache line. Stable Rust has no per-function alignment
    // (`#[align]` and `-C min-function-alignment` are unstable), but a
    // section's alignment is the largest `.p2align` in it and a function
    // alone in its section starts at the section's start, so this pins
    // the entry at offset 0 mod 64 whatever the code before it, and
    // the linker puts both sections ahead of the rest of `.text`, so the
    // pair's placement no longer moves with unrelated code. Without it
    // LLVM's 16-byte default let #68's push change (same instructions,
    // fewer of them) move `free` from 32 to 16 mod 64 and `malloc` from
    // 48 to 32, which cost glibc-simple 36% on Zen 5 (c4d). Pinning
    // either entry alone removed it there, so it is the pair's layout,
    // not one offset; both are pinned. The cost is 768 bytes of `.text`.
    #[cfg(target_arch = "x86_64")]
    core::arch::global_asm!(
        ".pushsection .text.challoc.malloc,\"ax\",@progbits",
        ".p2align 6",
        ".popsection",
        ".pushsection .text.challoc.free,\"ax\",@progbits",
        ".p2align 6",
        ".popsection",
    );

    // `verified_export_pair!` expands inside its own `verus!`; this outer
    // block holds it (Verus passes the invocation through) and the single
    // exports below.
    verus! {

    verified_export_pair! {
        fn calloc / je_calloc(n: usize, m: usize) -> *mut c_void {
            super::calloc(n, m) as *mut c_void
        }
        fn realloc / je_realloc(p: *mut c_void, size: usize) -> *mut c_void {
            super::realloc(p as *mut u8, size) as *mut c_void
        }
        fn aligned_alloc / je_aligned_alloc(align: usize, size: usize) -> *mut c_void {
            super::aligned_alloc(align, size) as *mut c_void
        }
        fn memalign / je_memalign(align: usize, size: usize) -> *mut c_void {
            super::aligned_alloc(align, size) as *mut c_void
        }
        fn posix_memalign / je_posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int {
            super::posix_memalign(out as *mut *mut u8, align, size)
        }
        fn valloc / je_valloc(size: usize) -> *mut c_void {
            super::valloc(size) as *mut c_void
        }
        fn pvalloc / je_pvalloc(size: usize) -> *mut c_void {
            super::pvalloc(size) as *mut c_void
        }
        fn malloc_usable_size / je_malloc_usable_size(p: *mut c_void) -> usize {
            super::malloc_usable_size(p as *mut u8)
        }
    }

    // The glibc-only names, which jemalloc does not prefix.

    #[unsafe(no_mangle)]
    pub extern "C" fn reallocarray(p: *mut c_void, n: usize, m: usize) -> *mut c_void {
        super::reallocarray(p as *mut u8, n, m) as *mut c_void
    }

    /// C23 `free_sized`: the size is checked against the class and stands
    /// in for the slot bit test (design 6.3).
    #[unsafe(no_mangle)]
    pub extern "C" fn free_sized(p: *mut c_void, size: usize) {
        super::free_sized(p as *mut u8, size)
    }

    /// C23 `free_aligned_sized`: as `free_sized`, with the alignment.
    #[unsafe(no_mangle)]
    pub extern "C" fn free_aligned_sized(p: *mut c_void, align: usize, size: usize) {
        super::free_aligned_sized(p as *mut u8, align, size)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn malloc_trim(pad: usize) -> c_int {
        super::malloc_trim(pad)
    }

    // challoc's size-returning entry points (design 13.7): challoc's own
    // names, no `je_` twins, declared in <challoc/challoc.h>.

    #[unsafe(no_mangle)]
    pub extern "C" fn challoc_malloc_usable(size: usize, usable: *mut usize) -> *mut c_void {
        super::malloc_usable(size, usable) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn challoc_calloc_usable(n: usize, m: usize, usable: *mut usize) -> *mut c_void {
        super::calloc_usable(n, m, usable) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn challoc_realloc_usable(
        p: *mut c_void,
        size: usize,
        usable: *mut usize,
    ) -> *mut c_void {
        super::realloc_usable(p as *mut u8, size, usable) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn challoc_free_usable(p: *mut c_void) -> usize {
        super::free_usable(p as *mut u8)
    }

    } // verus!
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{MEDIUM_MAX, class_size_of};
    use std::process::Command;
    use std::string::String;
    use std::vec::Vec;

    fn errno() -> c_int {
        unsafe { *::libc::__errno_location() }
    }

    fn fill(p: *mut u8, n: usize, seed: u8) {
        for i in 0..n {
            unsafe { *p.add(i) = seed.wrapping_add(i as u8) };
        }
    }

    fn check(p: *mut u8, n: usize, seed: u8) {
        for i in 0..n {
            assert_eq!(unsafe { *p.add(i) }, seed.wrapping_add(i as u8), "byte {i}");
        }
    }

    #[test]
    fn malloc_free_round_trip_every_tier() {
        for &n in &[
            0usize,
            1,
            7,
            16,
            17,
            100,
            1024,
            1025,
            4096,
            65536,
            MEDIUM_MAX,
            MEDIUM_MAX + 1,
            1 << 20,
            10 << 20,
        ] {
            let p = malloc(n);
            assert!(!p.is_null(), "malloc({n})");
            assert_eq!(p as usize % QUANTUM, 0);
            let usable = malloc_usable_size(p);
            assert!(usable >= n, "usable {usable} < {n}");
            match class_size_of(n) {
                Some(s) => assert_eq!(usable, s),
                None => assert_eq!(usable, super::super::shared().huge.usable_for(n).unwrap()),
            }
            fill(p, usable, 3);
            check(p, usable, 3);
            free(p);
        }
        assert_eq!(malloc_usable_size(ptr::null_mut()), 0);
        free(ptr::null_mut());
    }

    #[test]
    fn malloc_zero_returns_distinct_pointers() {
        let a = malloc(0);
        let b = malloc(0);
        assert!(!a.is_null() && !b.is_null() && a != b);
        free(a);
        free(b);
    }

    #[test]
    fn calloc_zeroes_reused_slots_and_huge_runs() {
        for &n in &[24usize, 1000, 5000, MEDIUM_MAX + 1, 3 << 20] {
            // Dirty a block, free it, then calloc the same size: the reused
            // memory must read as zero.
            let p = malloc(n);
            fill(p, n, 0xAB);
            free(p);
            let q = calloc(1, n);
            assert!(!q.is_null());
            for i in 0..n {
                assert_eq!(unsafe { *q.add(i) }, 0, "calloc({n}) byte {i} not zero");
            }
            free(q);
        }
        set_errno(0);
        assert!(calloc(usize::MAX, 2).is_null());
        assert_eq!(errno(), ENOMEM);
    }

    #[test]
    fn realloc_moves_and_preserves_content() {
        // Grow through the tiers and back down, checking content each step.
        let sizes = [
            10usize,
            100,
            1000,
            10_000,
            100_000,
            MEDIUM_MAX + 1,
            2 << 20,
            100_000,
            1000,
            10,
        ];
        let mut p = malloc(sizes[0]);
        fill(p, sizes[0], 1);
        let mut prev = sizes[0];
        for &n in &sizes[1..] {
            p = realloc(p, n);
            assert!(!p.is_null(), "realloc to {n}");
            check(p, prev.min(n), 1);
            fill(p, n, 1);
            prev = n;
        }
        // Same class: same pointer.
        let q = realloc(p, prev);
        assert_eq!(q, p);
        let q = realloc(p, prev + 1);
        assert_eq!(q, p, "10 -> 11 stays in the 16-byte class");
        assert!(
            realloc(p, 0).is_null(),
            "realloc(p, 0) frees and returns null"
        );

        // Huge: same number of pages keeps the pointer.
        let page = super::super::shared().regions.page_size();
        let h = realloc(ptr::null_mut(), MEDIUM_MAX + 1);
        assert_eq!(realloc(h, MEDIUM_MAX + page), h);
        let h2 = realloc(h, MEDIUM_MAX + page + 1);
        assert_ne!(h2, h);
        free(h2);

        assert!(reallocarray(ptr::null_mut(), usize::MAX, 2).is_null());
        assert_eq!(errno(), ENOMEM);
        let r = reallocarray(ptr::null_mut(), 8, 8);
        assert!(malloc_usable_size(r) >= 64);
        free(r);
    }

    #[test]
    fn aligned_allocations_honour_every_power_of_two() {
        let mut align = QUANTUM;
        while align <= 4 << 20 {
            for &size in &[
                1usize,
                align - 1,
                align,
                align + 1,
                3 * align,
                MEDIUM_MAX + 1,
            ] {
                let p = aligned_alloc(align, size);
                assert!(!p.is_null(), "aligned_alloc({align}, {size})");
                assert_eq!(
                    p as usize % align,
                    0,
                    "aligned_alloc({align}, {size}) = {p:p}"
                );
                assert!(malloc_usable_size(p) >= size);
                fill(p, size, 9);
                check(p, size, 9);
                free(p);

                let mut out = ptr::null_mut();
                assert_eq!(posix_memalign(&mut out, align, size), 0);
                assert_eq!(out as usize % align, 0);
                free(out);
            }
            align <<= 1;
        }
    }

    #[test]
    fn aligned_allocation_picks_the_smallest_fitting_class() {
        // 48-byte request at 32-byte alignment: class 48 slots are only
        // 16-aligned, so class 64 is used.
        let p = aligned_alloc(32, 48);
        assert_eq!(malloc_usable_size(p), 64);
        free(p);
        // Alignment already implied by the quantum: plain class.
        let p = aligned_alloc(QUANTUM, 48);
        assert_eq!(malloc_usable_size(p), class_size_of(48).unwrap());
        free(p);
        // 4 KiB alignment for 5 KiB: the 6 KiB class (5120 + 1024) is not a
        // multiple of 4 KiB; 8 KiB is.
        let p = aligned_alloc(4096, 5 * 1024);
        assert_eq!(malloc_usable_size(p), 8192);
        free(p);
    }

    #[test]
    fn bad_alignment_is_einval_and_exhaustion_is_enomem() {
        set_errno(0);
        assert!(aligned_alloc(0, 16).is_null());
        assert_eq!(errno(), EINVAL);
        set_errno(0);
        assert!(aligned_alloc(24, 16).is_null());
        assert_eq!(errno(), EINVAL);

        let mut out = ptr::null_mut::<u8>();
        set_errno(0);
        assert_eq!(posix_memalign(&mut out, 3, 16), EINVAL);
        assert_eq!(
            posix_memalign(&mut out, 4, 16),
            EINVAL,
            "below sizeof(void *)"
        );
        assert_eq!(errno(), 0, "posix_memalign leaves errno alone");
        assert!(out.is_null());

        set_errno(0);
        assert!(malloc(usize::MAX / 2).is_null());
        assert_eq!(errno(), ENOMEM);
        set_errno(0);
        assert!(realloc(ptr::null_mut(), usize::MAX / 2).is_null());
        assert_eq!(errno(), ENOMEM);
        set_errno(0);
        assert_eq!(posix_memalign(&mut out, 16, usize::MAX / 2), ENOMEM);
        set_errno(0);
        assert!(pvalloc(usize::MAX).is_null());
        assert_eq!(errno(), ENOMEM);
    }

    #[test]
    fn valloc_and_pvalloc_are_page_aligned() {
        let page = super::super::shared().regions.page_size();
        let p = valloc(10);
        assert_eq!(p as usize % page, 0);
        assert!(malloc_usable_size(p) >= 10);
        free(p);
        let p = pvalloc(page + 1);
        assert_eq!(p as usize % page, 0);
        assert!(malloc_usable_size(p) >= 2 * page);
        free(p);
        let p = pvalloc(0);
        assert!(malloc_usable_size(p) >= page);
        free(p);
    }

    /// Design 13.7: the size-returning entry points answer exactly what
    /// `malloc_usable_size` does, on every tier, and free what they made.
    #[test]
    fn usable_entry_points_agree_with_malloc_usable_size_on_every_tier() {
        for &n in &[
            0usize,
            1,
            7,
            16,
            17,
            100,
            1024,
            1025,
            4096,
            65536,
            MEDIUM_MAX,
            MEDIUM_MAX + 1,
            1 << 20,
            10 << 20,
        ] {
            let mut u = usize::MAX;
            let p = malloc_usable(n, &mut u);
            assert!(!p.is_null(), "malloc_usable({n})");
            assert_eq!(p as usize % QUANTUM, 0);
            assert_eq!(u, malloc_usable_size(p), "malloc_usable({n})");
            assert!(u >= n);
            match class_size_of(n) {
                Some(s) => assert_eq!(u, s),
                None => assert_eq!(u, super::super::shared().huge.usable_for(n).unwrap()),
            }
            fill(p, u, 3);
            check(p, u, 3);
            assert_eq!(free_usable(p), u, "free_usable({n})");

            // calloc_usable: the whole usable size reads as zero, even on a
            // slot or run that was dirtied and freed a moment ago.
            let d = malloc(n.max(1));
            fill(d, malloc_usable_size(d), 0xAB);
            free(d);
            let mut u = usize::MAX;
            let q = calloc_usable(1, n, &mut u);
            assert!(!q.is_null(), "calloc_usable({n})");
            assert_eq!(u, malloc_usable_size(q));
            for i in 0..u {
                assert_eq!(
                    unsafe { *q.add(i) },
                    0,
                    "calloc_usable({n}) byte {i} of {u} not zero"
                );
            }
            fill(q, u, 4);

            // realloc_usable: in place within the class (same pointer,
            // same size; `realloc_usable(q, 0)` would free, so 0 asks for
            // 1, still class 0), a move across tiers keeps the content.
            let mut u2 = usize::MAX;
            let r = realloc_usable(q, n.max(1), &mut u2);
            assert_eq!(
                r, q,
                "realloc_usable({n}) to the same size keeps the pointer"
            );
            assert_eq!(u2, u);
            let grown = 2 * n + 4097;
            let mut u3 = usize::MAX;
            let g = realloc_usable(r, grown, &mut u3);
            assert!(!g.is_null());
            assert_eq!(u3, malloc_usable_size(g));
            assert!(u3 >= grown);
            check(g, u.min(grown), 4);
            assert_eq!(free_usable(g), u3);
        }
    }

    /// The null and failure cases of design 13.7: a null `usable` makes the
    /// three plain `malloc`/`calloc`/`realloc`; `free_usable(NULL)` is 0;
    /// `realloc_usable(NULL, n)` allocates and `realloc_usable(p, 0)` frees
    /// and reports 0; a failed allocation returns null with `errno =
    /// ENOMEM` and `*usable = 0`, and a failed `realloc_usable` leaves the
    /// block intact.
    #[test]
    fn usable_entry_points_null_and_failure_cases() {
        assert_eq!(free_usable(ptr::null_mut()), 0);
        let p = malloc_usable(100, ptr::null_mut());
        assert!(!p.is_null());
        let q = calloc_usable(2, 50, ptr::null_mut());
        assert!(!q.is_null() && is_zero(q, 100));
        let r = realloc_usable(q, 5000, ptr::null_mut());
        assert!(!r.is_null());
        free(p);
        free(r);

        let mut u = usize::MAX;
        let p = realloc_usable(ptr::null_mut(), 300, &mut u);
        assert!(!p.is_null());
        assert_eq!(u, malloc_usable_size(p));
        fill(p, 300, 8);
        set_errno(0);
        let mut u2 = usize::MAX;
        assert!(realloc_usable(p, usize::MAX / 2, &mut u2).is_null());
        assert_eq!(errno(), ENOMEM);
        assert_eq!(u2, 0);
        check(p, 300, 8);
        let mut u3 = usize::MAX;
        assert!(
            realloc_usable(p, 0, &mut u3).is_null(),
            "realloc_usable(p, 0) frees and returns null"
        );
        assert_eq!(u3, 0);

        set_errno(0);
        let mut u = usize::MAX;
        assert!(malloc_usable(usize::MAX / 2, &mut u).is_null());
        assert_eq!((errno(), u), (ENOMEM, 0));
        set_errno(0);
        let mut u = usize::MAX;
        assert!(calloc_usable(usize::MAX, 2, &mut u).is_null(), "overflow");
        assert_eq!((errno(), u), (ENOMEM, 0));
        set_errno(0);
        let mut u = usize::MAX;
        assert!(
            calloc_usable(1, usize::MAX / 2, &mut u).is_null(),
            "exhaustion"
        );
        assert_eq!((errno(), u), (ENOMEM, 0));

        // Aligned blocks report their class, not the request.
        let a = aligned_alloc(4096, 5 * 1024);
        assert_eq!(free_usable(a), 8192);
        // Off the fast path: a full cache and a huge run.
        let many: Vec<*mut u8> = (0..3000).map(|_| malloc(64)).collect();
        for p in many {
            assert_eq!(free_usable(p), 64);
        }
        let h = malloc(3 << 20);
        assert_eq!(free_usable(h), malloc_usable_size_of_huge(3 << 20));
    }

    fn malloc_usable_size_of_huge(n: usize) -> usize {
        super::super::shared().huge.usable_for(n).unwrap()
    }

    fn is_zero(p: *mut u8, n: usize) -> bool {
        (0..n).all(|i| unsafe { *p.add(i) } == 0)
    }

    #[test]
    fn free_preserves_errno() {
        let p = malloc(100);
        let h = malloc(1 << 20);
        set_errno(1234);
        free(p);
        free(h);
        // Overflow a cache so the flush path runs too.
        let many: Vec<*mut u8> = (0..1000).map(|_| malloc(64)).collect();
        set_errno(1234);
        for p in many {
            free(p);
        }
        assert_eq!(errno(), 1234);
    }

    /// `malloc_trim` answers 1 when it purged something and 0 when there
    /// was nothing to purge: a freed huge run is `Empty` until a purge
    /// takes it, so the call right after its free says 1, and a call with
    /// nothing `Empty` anywhere says 0. Other tests of this binary free and
    /// purge concurrently, so each answer is looked for over a few
    /// attempts rather than asserted of one call.
    #[test]
    fn malloc_trim_reports_whether_anything_was_purged() {
        let purged = (0..8).any(|_| {
            let h = malloc(4 << 20);
            assert!(!h.is_null());
            unsafe { ptr::write_bytes(h, 1, 4 << 20) };
            free(h);
            malloc_trim(0) == 1
        });
        assert!(purged, "malloc_trim never reported the freed run purged");
        let quiet = (0..32).any(|_| malloc_trim(0) == 0);
        assert!(quiet, "malloc_trim never found nothing to purge");
    }

    #[test]
    fn cross_thread_free_and_thread_exit_with_full_cache() {
        let blocks: Vec<usize> = (0..4096)
            .map(|i| malloc(32 + (i % 8) * 16) as usize)
            .collect();
        let handles: Vec<_> = blocks
            .chunks(512)
            .map(|chunk| {
                let chunk: Vec<usize> = chunk.to_vec();
                std::thread::spawn(move || {
                    for &p in &chunk {
                        free(p as *mut u8);
                    }
                    // Leave with plenty in this thread's caches.
                    let mine: Vec<*mut u8> = (0..300).map(|_| malloc(48)).collect();
                    for p in mine {
                        free(p);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn shared_object_exports_every_libc_entry_point() {
        let so = crate::api::c_programs::release_so();
        let out = Command::new("readelf")
            .args(["--dyn-syms", "-W", &so])
            .output()
            .expect("readelf");
        assert!(out.status.success());
        let syms = String::from_utf8(out.stdout).unwrap();
        for name in [
            "malloc",
            "free",
            "calloc",
            "realloc",
            "reallocarray",
            "aligned_alloc",
            "memalign",
            "posix_memalign",
            "valloc",
            "pvalloc",
            "free_sized",
            "free_aligned_sized",
            "malloc_usable_size",
        ] {
            assert!(
                syms.lines().any(|l| l.contains(" FUNC ")
                    && l.contains(" GLOBAL ")
                    && l.ends_with(&std::format!(" {name}"))),
                "{name} not exported as a global function:\n{syms}"
            );
        }
    }
}
