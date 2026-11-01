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
use crate::heap::{self, Live, Shared};
#[cfg(verus_keep_ghost)]
use crate::layout::class_table;
use crate::layout::{CLASSES, Kind, MEDIUM_MAX, NCLASSES, SMALL_MAX, class_of, class_of_small};
#[cfg(verus_keep_ghost)]
use crate::lock::axiom_entry;
#[cfg(verus_keep_ghost)]
use crate::meta::is_pow2;
use crate::sys::Errno;
use crate::vspec::cbuf::{CCall, COut};
use crate::vspec::errno::{ThreadErrno, set_errno};
use crate::vspec::usable::Stored;
#[cfg(verus_keep_ghost)]
use crate::vspec::usable::{usable_is, usable_of};
use crate::vspec::zero::Zeroed;
#[cfg(verus_keep_ghost)]
use crate::vspec::zero::covers_usable;
#[cfg(verus_keep_ghost)]
use vstd::arithmetic::power2::pow2;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, verus};
use vstd::raw_ptr::ptr_mut_write;

verus! {

// The codes as `c_int`, for `errno` and `posix_memalign`'s return; the
// numbers live in `sys::errno`.
pub const ENOMEM: c_int = Errno::ENOMEM.0;
pub const EINVAL: c_int = Errno::EINVAL.0;

/// Null with `errno = ENOMEM`.
#[cold]
#[inline(never)]
fn enomem(Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    ensures
        r@.addr == 0,
        final(errno).val() == ENOMEM,
{
    set_errno(ENOMEM, Tracked(errno));
    ptr::null_mut()
}

/// Null with `errno = EINVAL`.
#[cold]
#[inline(never)]
fn einval(Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    ensures
        r@.addr == 0,
        final(errno).val() == EINVAL,
{
    set_errno(EINVAL, Tracked(errno));
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
        // The smallest class that serves `n` bytes at no alignment.
        r == spec_class_for(n, 1),
    opens_invariants none
{
    let r = class_of(n);
    proof {
        lemma_class_index(n, r);
    }
    r
}

/// [`class_index`] of a request of at most [`SMALL_MAX`] bytes, from the
/// small table alone ([`class_of_small`]): the same class.
#[inline(always)]
pub(super) fn class_index_small(n: usize) -> (r: usize)
    requires
        n <= SMALL_MAX,
    ensures
        r < NCLASSES,
        forall|j: int| r <= j < NCLASSES ==> n <= #[trigger] class_table()[j].size,
        forall|j: int| 0 <= j < r ==> #[trigger] class_table()[j].size < n,
        Some(r) == spec_class_for(n, 1),
    opens_invariants none
{
    let r = class_of_small(n);
    proof {
        lemma_class_index(n, Some(r));
    }
    r
}

/// What [`class_index`] ensures, from what `class_of` (and
/// `class_of_small`) ensure of `r`.
proof fn lemma_class_index(n: usize, r: Option<usize>)
    requires
        r is Some <==> n <= MEDIUM_MAX,
        r matches Some(c) ==> c < NCLASSES && n <= crate::layout::spec_class_size(c as int),
        r matches Some(c) ==> crate::layout::classes_below(c as int, n as int),
    ensures
        r matches Some(c) ==> forall|j: int| c <= j < NCLASSES ==> n <= #[trigger] class_table()[j].size,
        r matches Some(c) ==> forall|j: int| 0 <= j < c ==> #[trigger] class_table()[j].size < n,
        r == spec_class_for(n, 1),
{
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
        assert forall|j: int| 0 <= j < c implies !#[trigger] class_serves(j, n as int, 1) by {
            assert(class_table()[j].size < n);
        }
        lemma_class_for_some(n, 1, c as int);
    } else {
        assert forall|j: int| 0 <= j < NCLASSES implies !#[trigger] class_serves(j, n as int, 1) by {
            crate::vspec::class::axiom_class_table_wf(j);
        }
        lemma_class_for_none(n, 1);
    }
}

/// Class `c` serves `size` bytes at `align`: its slots hold `size` bytes
/// and, above the quantum, its size is a multiple of `align`.
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

/// The smallest class that serves `size` bytes at a power-of-two
/// `align`, `None` when no class does.
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
        crate::meta::is_pow2(align),
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

/// A slot of class `c` handed out for `size` bytes, with its token, gives
/// them ([`Live::gives`]).
pub(super) proof fn lemma_class_gives(blk: Live, p: *mut u8, c: int, size: usize)
    requires
        0 <= c < NCLASSES,
        size <= class_table()[c].size,
        p@.addr != 0 ==> blk.slot_block(p, c),
    ensures
        blk.gives(p, size as int),
{
    if p@.addr != 0 {
        blk.lemma_slot_len(p, c);
    }
}

/// Huge allocation on the calling thread, counted in its heap when it has
/// one. `align` is a power of two; `1` means none. `blk` gets the run's
/// token.
#[cold]
#[inline(never)]
fn alloc_huge(shared: &Shared, size: usize, align: usize, Tracked(blk): Tracked<&mut Live>) -> (r: (*mut u8, bool, Tracked<Zeroed>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        crate::meta::is_pow2(align),
    ensures
        r.0@.addr != 0 ==> crate::layout::huge_block(r.0@.addr, size, align),
        r.0@.addr != 0 ==> size <= shared.huge_usable(size),
        r.0@.addr != 0 ==> shared.huge_ok(size),
        r.0@.addr != 0 && r.1 ==> r.2@.covers(r.0, shared.huge_usable(size)),
        r.0@.addr != 0 ==> final(blk).block(r.0) && final(blk).len() == shared.huge_usable(size),
{
    match heap::heap_for_thread(shared) {
        Some(h) => h.alloc_huge(shared, size, align, Tracked(blk)),
        // Premise (lock order, `lock::axiom_entry`): the C caller holds no
        // allocator lock.
        None => shared.alloc_huge(size, align, 0, Tracked(blk), Tracked(&mut axiom_entry())),
    }
}

/// `malloc`. A non-null result is a block of at least `size` bytes in a
/// class or huge region, aligned to the quantum ([`heap::fits`]); a null
/// one comes with `errno = ENOMEM`. Its token ([`malloc_block`]) goes to
/// C with the block.
#[inline(always)]
pub fn malloc(size: usize, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        heap::fits(r, size as int, QUANTUM as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let tracked mut blk = Live::empty();
    malloc_block(size, Tracked(&mut blk), Tracked(errno))
}

/// [`malloc`] with the block's token: `blk` gets the token of the block
/// returned, which has at least `size` usable bytes ([`Live::gives`]).
#[inline(always)]
pub fn malloc_block(size: usize, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        heap::fits(r, size as int, QUANTUM as int),
        final(blk).gives(r, size as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    // The fast path is a cache pop with no calls in it; everything else
    // (init, heap creation, refill, huge, ENOMEM) is one tail call away so
    // the exported `malloc` needs no stack frame. A size above the small
    // table is one tail call too ([`malloc_medium`]): with the class taken
    // only from the table here, its stride-scaled entry addresses the
    // cache header as it is (`class_of_small`).
    if size > SMALL_MAX {
        return malloc_medium(size, Tracked(blk), Tracked(errno));
    }
    malloc_class(size, class_index_small(size), Tracked(blk), Tracked(errno))
}

/// `malloc` of a size above the small table: the same pop, behind the
/// computed class.
#[inline(never)]
fn malloc_medium(size: usize, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        heap::fits(r, size as int, QUANTUM as int),
        final(blk).gives(r, size as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let Some(c) = class_index(size) else { return malloc_slow(size, Tracked(blk), Tracked(errno)) };
    malloc_class(size, c, Tracked(blk), Tracked(errno))
}

/// `malloc`'s pop for a known class `c` of `size` (`class_index`'s).
#[inline(always)]
fn malloc_class(size: usize, c: usize, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        c < NCLASSES,
        forall|j: int| c <= j < NCLASSES ==> size <= #[trigger] class_table()[j].size,
    ensures
        heap::fits(r, size as int, QUANTUM as int),
        final(blk).gives(r, size as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let shared = super::shared_ptr();
    let Some(h) = heap::existing_heap(shared) else { return malloc_slow(size, Tracked(blk), Tracked(errno)) };
    // SAFETY: this thread's heap exists, so this thread observed `READY`
    // (the byte was read once inside `existing_heap`; this is the same
    // load, merged by the compiler).
    let flags = unsafe { heap::fast_flags(shared) };
    proof {
        h.lemma_wf_regions();
    }
    match h.alloc_fast(c, flags, Tracked(blk)) {
        Some(p) => {
            proof {
                lemma_class_fits(p, c as int, size);
                lemma_class_gives(*blk, p, c as int, size);
                heap::theorem_no_block_at_zero(p, c as int, 0);
            }
            p
        },
        // The class, not the size: the declined pop (an empty cache or
        // the scheduled pop) goes straight to the slow path of its class,
        // `heap::alloc_declined`, not through a second classification and
        // a second fast pop.
        None => {
            let p = malloc_slow_class(c, Tracked(blk), Tracked(errno));
            proof {
                lemma_class_fits(p, c as int, size);
                lemma_class_gives(*blk, p, c as int, size);
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
fn malloc_slow_class(class: usize, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        class < NCLASSES,
    ensures
        r@.addr != 0 ==> crate::layout::process_regions().is_slot(class as int, r@.addr),
        r@.addr != 0 ==> final(blk).slot_block(r, class as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let p = heap::alloc_declined(super::shared(), class, Tracked(blk), Tracked(&mut *errno));
    if p.is_null() { enomem(Tracked(errno)) } else { p }
}

/// The cache pop alone: `None` whenever anything is off the fast path (no
/// heap yet, empty cache, huge size). Shared with the C++ `operator new`
/// family in `cxx.rs`, whose failure handling differs from `malloc`'s.
#[inline(always)]
// The `let ... else`s stand for `?`, which the proofs spell out.
#[allow(clippy::question_mark)]
pub(super) fn malloc_fast(size: usize, Tracked(blk): Tracked<&mut Live>) -> (r: Option<*mut u8>)
    requires
        crate::sys::may_abort_corrupt(),
    ensures
        r matches Some(p) ==> heap::fits(p, size as int, QUANTUM as int),
        r matches Some(p) ==> final(blk).gives(p, size as int),
        r matches Some(p) ==> p@.addr != 0,
    opens_invariants none
{
    let Some(c) = class_index(size) else { return None };
    let shared = super::shared_ptr();
    let Some(h) = heap::existing_heap(shared) else { return None };
    // SAFETY: this thread's heap exists, so this thread observed `READY`
    // (the byte was read once inside `existing_heap`; this is the same
    // load, merged by the compiler).
    let flags = unsafe { heap::fast_flags(shared) };
    proof {
        h.lemma_wf_regions();
    }
    let r = h.alloc_fast(c, flags, Tracked(blk));
    proof {
        if let Some(p) = r {
            lemma_class_fits(p, c as int, size);
            lemma_class_gives(*blk, p, c as int, size);
            heap::theorem_no_block_at_zero(p, c as int, 0);
        }
    }
    r
}

/// Everything `malloc` does off its fast path. Null with `errno = ENOMEM`
/// on failure.
#[cold]
#[inline(never)]
pub(super) fn malloc_slow(size: usize, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        heap::fits(r, size as int, QUANTUM as int),
        final(blk).gives(r, size as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let shared = super::shared();
    proof {
        crate::meta::lemma_is_pow2_one();
        shared.lemma_regions_ok();
    }
    let p = match class_index(size) {
        Some(c) => {
            let p = heap::alloc(shared, c, Tracked(blk), Tracked(&mut *errno));
            proof {
                lemma_class_fits(p, c as int, size);
                lemma_class_gives(*blk, p, c as int, size);
            }
            p
        },
        None => {
            let p = alloc_huge(shared, size, 1, Tracked(blk)).0;
            proof {
                lemma_huge_result(p, size, 1);
            }
            p
        },
    };
    if p.is_null() { enomem(Tracked(errno)) } else { p }
}

/// `calloc`. Skips the `memset` when the slot or run is known to read as
/// zero (fresh or purged memory). The block's token goes to C with it.
#[inline(always)]
pub fn calloc(n: usize, m: usize, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        n * m > usize::MAX ==> r@.addr == 0,
        heap::fits(r, n * m, QUANTUM as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let tracked mut blk = Live::empty();
    calloc_zeroed(n, m, Tracked(&mut blk), Tracked(errno)).0
}

/// [`calloc`] with the proof that its memory reads as zero: a non-null
/// result comes with a [`Zeroed`] token for its `n * m` bytes, from the
/// `memset` or from
/// the allocator's report that the slot or run is known zero; and `blk`
/// gets the block's token, whose permission's byte state reads those
/// bytes as zero ([`Live::reads_zero`]): the `memset` wrote them through
/// the permission (`vspec::user::zero_user`), or the allocator's token
/// for them was turned into the byte state
/// (`vspec::user::axiom_zeroed_bytes`).
#[inline(always)]
pub fn calloc_zeroed(n: usize, m: usize, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: (*mut u8, Tracked<Zeroed>))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        n * m > usize::MAX ==> r.0@.addr == 0,
        heap::fits(r.0, n * m, QUANTUM as int),
        r.0@.addr != 0 ==> r.1@.covers(r.0, n * m),
        final(blk).gives(r.0, n * m),
        r.0@.addr != 0 ==> final(blk).reads_zero(0, n * m),
        r.0@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let Some(size) = n.checked_mul(m) else { return (enomem(Tracked(errno)), Tracked(Zeroed::empty(0))) };
    let shared = super::shared();
    proof {
        crate::meta::lemma_is_pow2_one();
        shared.lemma_regions_ok();
    }
    let (p, zeroed, z) = match class_index(size) {
        Some(c) => {
            let r = heap::alloc_zeroed(shared, c, Tracked(blk), Tracked(&mut *errno));
            proof {
                lemma_class_fits(r.0, c as int, size);
                lemma_class_gives(*blk, r.0, c as int, size);
            }
            r
        },
        None => {
            let r = alloc_huge(shared, size, 1, Tracked(blk));
            proof {
                lemma_huge_result(r.0, size, 1);
            }
            r
        },
    };
    if p.is_null() {
        return (enomem(Tracked(errno)), Tracked(Zeroed::empty(0)));
    }
    if !zeroed {
        let ghost orig = *blk;
        proof {
            blk.lemma_mem();
        }
        let tracked mut mem = blk.take_mem();
        // SAFETY: `mem` is the permission to the fresh block's usable bytes,
        // at least `size` of them.
        let z = unsafe { crate::vspec::user::zero_user(p, size, Tracked(&mut mem)) };
        proof {
            blk.put_mem(mem, orig);
        }
        return (p, z);
    }
    // Known zero: the allocator's token for the first `size` bytes, which
    // the block's permission then reads as zero.
    let Tracked(za) = z;
    let tracked z0;
    proof {
        let tracked (zs, _) = za.split(size as int);
        blk.lemma_mem();
        let ghost orig = *blk;
        let tracked mut mem = blk.take_mem();
        crate::vspec::user::axiom_zeroed_bytes(&zs, &mut mem);
        blk.put_mem(mem, orig);
        z0 = zs;
    }
    (p, Tracked(z0))
}

/// `free`. Null is a no-op; anything that is not a live allocation aborts.
/// C holds the token of the block it frees ([`heap::live::axiom_c_claim`],
/// the one trusted step of the C contract).
#[inline(always)]
pub fn free(p: *mut u8, Tracked(c): Tracked<CCall>, errno: Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        p@.addr != 0 ==> heap::free_target(p),
{
    let tracked blk = heap::live::axiom_c_claim(c);
    free_block(p, Tracked(blk), errno)
}

/// [`free`] with the caller's token for `p` ([`Live::claims`]), which the
/// free takes back.
#[inline(always)]
pub fn free_block(p: *mut u8, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        blk.claims(p),
    ensures
        final(errno).val() == old(errno).val(),
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> blk.block(p),
{
    let tracked mut blk = blk;
    // As in `malloc`: the check-and-push has no calls, and every other
    // outcome, including the aborts, is reached through one tail call.
    // Null is one of those outcomes: it is in no class region (the
    // superblock is mapped far above address 0, so `Shared::set_slot`'s
    // class index of null is out of range) and `free_slow` returns for it, so
    // the common case pays no test of its own (jemalloc's `free` fast
    // path leaves null to its slow path the same way).
    let tracked tls = crate::sys::tls::TlsSlot::entry();
    if let Some((t, tp)) = heap::tls_heap_slot(Tracked(&*tls)) {
        // SAFETY: this thread's heap exists, so this thread observed
        // `READY` when it created the heap.
        let shared = unsafe { super::shared_deref(super::shared_base()) };
        // SAFETY: the slot value of this thread.
        if unsafe { heap::free_fast_slot(shared, t, tp, p, Tracked(&mut blk), Tracked(tls)) }.is_some() {
            return;
        }
    }
    free_slow(p, Tracked(blk), Tracked(errno))
}

/// Sized free on the calling thread's heap: `hint` is the class the size
/// (and alignment) name, `class_for(size, align)`, or `None` for a huge
/// size. On the fast path the hint replaces the slab descriptor's bit test
/// (design 6.3): a slot whose class matches is pushed at once, so an
/// invalid or double free through this path is caught when the cache
/// flushes it, not at the call; a hint that does not match aborts
/// ([`heap::hint_ok`] of every non-null `p` it returns for). C holds the
/// token of the block it frees ([`heap::live::axiom_c_claim`]).
#[inline(always)]
pub fn free_sized_hint(p: *mut u8, size: usize, hint: Option<usize>, Tracked(c): Tracked<CCall>, errno: Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        hint matches Some(c) ==> c < NCLASSES,
    ensures
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> heap::hint_ok(p, hint),
{
    let tracked blk = heap::live::axiom_c_claim(c);
    free_sized_block(p, size, hint, Tracked(blk), errno)
}

/// [`free_sized_hint`] with the caller's token for `p` ([`Live::claims`]),
/// which the free takes back: the token of the live block `p`.
#[inline(always)]
pub fn free_sized_block(p: *mut u8, size: usize, hint: Option<usize>, Tracked(blk): Tracked<Live>, errno: Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        hint matches Some(c) ==> c < NCLASSES,
        blk.claims(p),
    ensures
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> heap::hint_ok(p, hint),
        p@.addr != 0 ==> heap::huge_hint_ok(blk, p, size),
        p@.addr != 0 ==> blk.block(p),
{
    let tracked mut blk = blk;
    // A huge size (no hint) has no fast path: straight to the slow one,
    // so the fast path carries a plain class and no test of the hint.
    let Some(c) = hint else {
        return free_sized_unhinted(p, size, Tracked(blk), errno);
    };
    // On x86_64 there is no null test here: null is below every class
    // region, so its offset from the hint's region base wraps to a value
    // with bits above the region set, `ClassLine::sized_ok` declines it,
    // and `free_sized_slow` returns on null. On aarch64 the test stays (a
    // `cbz`; its layout is kept, see `Heap`).
    #[cfg(not(target_arch = "x86_64"))]
    if p.is_null() {
        return;
    }
    let sp = super::shared_base();
    if let Some(h) = heap::existing_heap(sp) {
        proof {
            h.lemma_wf_regions();
        }
        // SAFETY: as in `free`.
        let shared = unsafe { super::shared_deref(sp) };
        if heap::free_sized_fast(shared, h, p, Some(c), shared.push_flags(), Tracked(&mut blk)) {
            return;
        }
    }
    free_sized_slow(p, size, hint, Tracked(blk), errno)
}

/// The sized free of a size no class holds (a huge run, or nothing): the
/// exact `free` behind the null test.
#[cold]
#[inline(never)]
fn free_sized_unhinted(p: *mut u8, size: usize, Tracked(blk): Tracked<Live>, errno: Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        blk.claims(p),
        crate::sys::may_syscall(),
    ensures
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> heap::hint_ok(p, None),
        p@.addr != 0 ==> heap::huge_hint_ok(blk, p, size),
        p@.addr != 0 ==> blk.block(p),
{
    if !p.is_null() {
        free_sized_slow(p, size, None, Tracked(blk), errno)
    }
}

/// Off the sized fast path: on x86_64 a null pointer (which the fast
/// path's `sized_ok` declines rather than testing for), the hint
/// mismatch abort, then everything the exact `free` does (so a full cache
/// or a huge run still gets the exact check, and a bad pointer the usual
/// message).
#[cold]
#[inline(never)]
fn free_sized_slow(p: *mut u8, size: usize, hint: Option<usize>, Tracked(blk): Tracked<Live>, errno: Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        blk.claims(p),
        crate::sys::may_syscall(),
    ensures
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> heap::hint_ok(p, hint),
        p@.addr != 0 ==> heap::huge_hint_ok(blk, p, size),
        p@.addr != 0 ==> blk.block(p),
{
    #[cfg(target_arch = "x86_64")]
    if p.is_null() {
        return;
    }
    match super::shared_if_ready() {
        Some(shared) => {
            shared.check_size_hint(p, size, hint, Tracked(&blk));
            heap::free(shared, p, Tracked(blk), errno)
        }
        None => free_before_init(p),
    }
}

/// C23 `free_sized(p, size)`: `size` must be the size requested from
/// `malloc`/`calloc`/`realloc` (or any size in the same class, or for a
/// huge run any size whose page count is the run's). A slot freed with
/// the size of another class, or with a huge size, aborts; so does a huge
/// run freed with a size of another page count.
#[inline(always)]
pub fn free_sized(p: *mut u8, size: usize, Tracked(c): Tracked<CCall>, errno: Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> heap::hint_ok(p, spec_class_for(size, 1)),
{
    let tracked blk = heap::live::axiom_c_claim(c);
    free_sized_with(p, size, Tracked(blk), errno)
}

/// [`free_sized`] with the caller's token for `p` ([`Live::claims`]),
/// which the free takes back (`free_sized_block`'s); also `sdallocx`'s
/// with no flags.
#[inline(always)]
pub fn free_sized_with(p: *mut u8, size: usize, Tracked(blk): Tracked<Live>, errno: Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        blk.claims(p),
    ensures
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> heap::hint_ok(p, spec_class_for(size, 1)),
        p@.addr != 0 ==> heap::huge_hint_ok(blk, p, size),
        p@.addr != 0 ==> blk.block(p),
{
    // As in `malloc`: a size above the small table is a tail call, so the
    // class comes only from the table here and the stride-scaled entry
    // addresses the class line and the cache header as it is. That keeps
    // the sized path within its registers, with no spill (on Genoa a
    // `push` and a `pop` here cost `valkey1` 8%, #46).
    if size > SMALL_MAX {
        return free_sized_medium(p, size, Tracked(blk), errno);
    }
    free_sized_block(p, size, Some(class_index_small(size)), Tracked(blk), errno)
}

/// `free_sized` of a size above the small table.
#[inline(never)]
fn free_sized_medium(p: *mut u8, size: usize, Tracked(blk): Tracked<Live>, errno: Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        blk.claims(p),
    ensures
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> heap::hint_ok(p, spec_class_for(size, 1)),
        p@.addr != 0 ==> heap::huge_hint_ok(blk, p, size),
        p@.addr != 0 ==> blk.block(p),
{
    free_sized_block(p, size, class_index(size), Tracked(blk), errno)
}

/// C23 `free_aligned_sized(p, align, size)`: the arguments given to
/// `aligned_alloc`. An alignment that is not a power of two matches no
/// class and aborts on a slab pointer.
pub fn free_aligned_sized(p: *mut u8, align: usize, size: usize, Tracked(c): Tracked<CCall>, errno: Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> heap::hint_ok(p, if is_pow2(align) { spec_class_for(size, align) } else { None }),
{
    let hint = if align.is_power_of_two() { class_for(size, align) } else { None };
    free_sized_hint(p, size, hint, Tracked(c), errno)
}

/// Everything `free` does off its fast path: heap creation, a full cache,
/// huge runs, and the aborts for invalid pointers (the checks are repeated
/// from scratch here, so a fast-path `false` for a bad pointer ends in the
/// same message it always did).
#[cold]
#[inline(never)]
fn free_slow(p: *mut u8, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_abort(),
        blk.claims(p),
        crate::sys::may_syscall(),
    ensures
        final(errno).val() == old(errno).val(),
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> blk.block(p),
{
    if p.is_null() {
        return;
    }
    match super::shared_if_ready() {
        Some(shared) => heap::free(shared, p, Tracked(blk), Tracked(errno)),
        None => free_before_init(p),
    }
}

/// The abort for a pointer freed (or queried) before any allocation.
#[cold]
#[inline(never)]
fn free_before_init(p: *mut u8) -> !
    requires
        crate::sys::may_abort(),
{
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
/// ([`crate::huge::Huge::usable_size`]). With a token that claims `p`
/// ([`Live::claims`]), the result is the block's usable bytes.
#[inline(always)]
pub fn usable_size(shared: &Shared, p: *mut u8, Tracked(blk): Tracked<&Live>) -> (r: usize)
    requires
        crate::sys::may_abort() || (crate::sys::valid_aborts() && blk.block(p)),
        crate::sys::may_syscall(),
        shared.wf(),
    ensures
        heap::free_target(p),
        crate::layout::process_regions().is_huge_block(p as usize, 1) ==> shared.huge_run_usable(p as usize, r),
        usable_of(p, r as int),
        blk.claims(p) ==> blk.block(p) && r == blk.len(),
        // No block at address 0: NULL aborts, and the size is positive.
        p@.addr != 0,
        0 < r,
{
    proof {
        shared.lemma_wf_slow();
        shared.lemma_wf_ready();
        shared.lemma_wf();
        shared.lemma_huge_pages();
        shared.lemma_regions_ok();
        heap::theorem_no_block_at_zero(p, 0, 0);
    }
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            proof {
                if blk.block(p) {
                    // The live slot, whose token `blk` holds.
                    blk.lemma_block_slot_addr(p, class as int);
                }
            }
            shared.table(class).check_live(slab as usize, slot as usize, p as usize, Tracked(blk.slot_ref()));
            proof {
                // `check_live` returned: not a guard slab.
                crate::layout::process_regions().lemma_is_slot(class as int, slab as int, slot as int, p as usize);
                crate::vspec::usable::lemma_usable_slot(p, class as int, 0);
                crate::vspec::usable::lemma_usable_of_pos(p, class_table()[class as int].size as int);
                if blk.claims(p) {
                    blk.lemma_claims_slot(p, class as int);
                    blk.lemma_claimed_block(p);
                    blk.lemma_slot_len(p, class as int);
                }
            }
            class_size(class)
        }
        Kind::Huge { page } => {
            proof {
                if blk.block(p) {
                    // The live run at `page`, whose token `blk` holds.
                    blk.lemma_claims(p);
                    shared.lemma_run_page(p, page, *blk);
                }
            }
            let r = shared.huge.usable_size(page, Tracked(blk.run_ref()));
            proof {
                shared.lemma_huge_run_usable(p as usize, r);
                crate::vspec::usable::lemma_usable_run(shared, p, r);
                crate::vspec::usable::lemma_usable_of_pos(p, r as int);
                if blk.claims(p) {
                    shared.lemma_run_page(p, page, *blk);
                }
            }
            r
        },
        Kind::Invalid => {
            proof {
                if blk.block(p) {
                    // A slot start or a page start of the huge region,
                    // which `classify` never calls `Invalid`.
                    blk.lemma_block_target(p);
                    if heap::slot_start(p) {
                        let c = choose|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr);
                        assert(shared.regions.is_slot_addr(c, p@.addr));
                    }
                }
                assert(!blk.block(p));
            }
            not_an_allocation(p)
        },
    }
}

/// The abort of [`usable_size`] for a pointer outside every allocation,
/// out of line so the query needs no frame for the message.
#[cold]
#[inline(never)]
fn not_an_allocation(p: *mut u8) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!("{:#x} is not an allocation", p as usize))
}


/// `malloc_usable_size`. Zero for null.
#[inline(always)]
pub fn malloc_usable_size(p: *mut u8) -> (r: usize)
    requires
        crate::sys::may_abort() || p@.addr == 0,
        crate::sys::may_syscall(),
    ensures
        p@.addr == 0 ==> r == 0,
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> usable_of(p, r as int),
        p@.addr != 0 ==> 0 < r,
{
    if p.is_null() {
        return 0;
    }
    let tracked none = Live::empty();
    match super::shared_if_ready() {
        Some(shared) => usable_size(shared, p, Tracked(&none)),
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
        lemma_mask_mod(s, align);
    }
}

/// What `realloc` and `rallocx` return when they keep `p` for `size`
/// bytes at `align` ([`old_size_or_keep`]): in the process's regions,
/// well formed and separated, a multiple of `align` and of the quantum,
/// and either a slot of a class at least `size` bytes large (the slab
/// half of `Regions::user_block`, so `heap::fits` for `size` at `align`)
/// or a page start of the huge region with `size` bytes (at least one) of
/// the region from it (the huge half of `Regions::user_block`): `size` is
/// at most the run's usable bytes, which its descriptor keeps inside the
/// region (`vspec::usable::lemma_run_bytes_block`). So a kept block
/// [`fits`](heap::fits) `size` bytes at `align` and at the quantum (both
/// are open specs, which Verus unfolds).
pub open spec fn kept(p: *mut u8, size: int, align: int) -> bool {
    &&& heap::regions_ok()
    &&& (p@.addr as int) % align == 0
    &&& (p@.addr as int) % (QUANTUM as int) == 0
    &&& (exists|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr) && size <= class_table()[c].size)
        || crate::layout::process_regions().is_huge_block(p@.addr, if size < 1 { 1 } else { size })
}

/// The keep rule of `realloc` and `rallocx` ([`old_size_or_keep`],
/// [`realloc_old`]), exactly: they return the live block `p`, of `len`
/// usable bytes, for `size` bytes at `align` (a power of two; 1 or the
/// quantum means the natural alignment) when `p` is a slot of the class
/// [`class_for`] picks for `size` at `align` ([`spec_class_for`], the
/// smallest serving class), or when `p` is a huge run start, no class
/// serves `size` at `align`, `size` rounded up to whole pages
/// ([`huge_usable`](crate::vspec::zero::huge_usable)) is the run's usable
/// bytes and `p` is a multiple of `align`; and they move it otherwise. A
/// run is never kept for a size some class serves, so a run allocated
/// for a small size at an alignment above `MEDIUM_MAX` moves on every
/// `realloc`, to a slot for any size a class serves, its usable size
/// included ([`lemma_keeps_usable`]).
pub open spec fn keeps(p: *mut u8, size: usize, align: usize, len: int) -> bool {
    ||| exists|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr) && spec_class_for(size, align) == Some(c as usize)
    ||| {
        &&& crate::layout::process_regions().is_huge_block(p@.addr, 1)
        &&& spec_class_for(size, align) is None
        &&& crate::vspec::zero::huge_usable(size as int) == len
        &&& (p@.addr as int) % (align as int) == 0
    }
}

/// For a slot of class `c`, [`keeps`] is "`size` at `align` maps to `c`".
proof fn lemma_keeps_slot(p: *mut u8, c: int, size: usize, align: usize, len: int)
    requires
        heap::regions_ok(),
        crate::layout::process_regions().is_slot(c, p@.addr),
    ensures
        keeps(p, size, align, len) <==> spec_class_for(size, align) == Some(c as usize),
{
    crate::vspec::zero::lemma_slot_not_huge(c, p@.addr);
    assert forall|c2: int| #[trigger] crate::layout::process_regions().is_slot(c2, p@.addr) implies c2 == c by {
        crate::vspec::zero::lemma_slot_unique(c, c2, p@.addr);
    }
}

/// For a huge page start, [`keeps`] is its run half: no slot is one.
proof fn lemma_keeps_run(p: *mut u8, size: usize, align: usize, len: int)
    requires
        heap::regions_ok(),
        crate::layout::process_regions().is_huge_block(p@.addr, 1),
    ensures
        keeps(p, size, align, len) <==> {
            &&& spec_class_for(size, align) is None
            &&& crate::vspec::zero::huge_usable(size as int) == len
            &&& (p@.addr as int) % (align as int) == 0
        },
{
    assert forall|c: int| !#[trigger] crate::layout::process_regions().is_slot(c, p@.addr) by {
        if crate::layout::process_regions().is_slot(c, p@.addr) {
            crate::vspec::zero::lemma_slot_not_huge(c, p@.addr);
        }
    }
}

/// What `Huge::usable_for(size)` reads against a run's usable bytes
/// `old` ([`usable_of`]): when it is defined it is `size` rounded up to
/// whole pages, and when `size` rounded up overflows it is no run's usable
/// size. So `usable_for(size) == Some(old)` exactly when
/// `huge_usable(size) == old`.
proof fn lemma_usable_for_run(shared: &Shared, p: *mut u8, size: usize, old: usize)
    requires
        shared.wf(),
        crate::layout::process_regions().is_huge_block(p@.addr, 1),
        usable_of(p, old as int),
    ensures
        size + shared.huge.spec_page() - 1 <= usize::MAX ==> shared.huge.spec_usable(size) == crate::vspec::zero::huge_usable(size as int),
        crate::vspec::zero::huge_usable(size as int) == old ==> size + shared.huge.spec_page() - 1 <= usize::MAX,
{
    shared.lemma_wf();
    shared.lemma_wf_slow();
    shared.lemma_wf_ready();
    let r = crate::layout::process_regions();
    let pg = (1usize << r.lg_page) as int;
    assert(shared.huge.spec_page() == pg);
    let page = ((p@.addr - r.huge_base) as usize) >> r.lg_page;
    let h = r.huge_len;
    let lg = r.lg_page;
    // `run_bytes`: the run and its guard lie below `(h >> lg) * pg <= h`.
    assert(page * pg + old + crate::huge::GUARD_PAGES * pg <= ((h >> lg) as int) * pg);
    crate::vspec::usable::lemma_page_shift(r.huge_len);
    assert(page * pg >= 0) by (nonlinear_arith)
        requires
            pg > 0,
    ;
    let s = size + pg - 1;
    let n = s / pg;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(s, pg);
    vstd::arithmetic::div_mod::lemma_mod_pos_bound(s, pg);
    if s > usize::MAX {
        // `n * pg > s - pg >= usize::MAX - pg + 1 > old`.
        assert(n * pg > s - pg) by (nonlinear_arith)
            requires
                s == pg * n + s % pg,
                s % pg < pg,
        ;
        assert(pg <= usize::MAX);
        assert(n != 0) by (nonlinear_arith)
            requires
                n * pg > s - pg,
                s > usize::MAX,
                pg <= usize::MAX,
        ;
    }
}

/// Some class serves `size` bytes at alignment 1 when one at or below
/// class `c` is at least `size` bytes large.
proof fn lemma_class_for_one(size: usize, c: int)
    requires
        0 <= c < NCLASSES,
        size <= class_table()[c].size,
    ensures
        spec_class_for(size, 1) is Some,
    decreases c,
{
    if exists|j: int| 0 <= j < c && #[trigger] class_serves(j, size as int, 1) {
        let j = choose|j: int| 0 <= j < c && #[trigger] class_serves(j, size as int, 1);
        lemma_class_for_one(size, j);
    } else {
        lemma_class_for_some(size, 1, c);
    }
}

/// [`class_of`] is [`class_for`] at alignment 1: the class at or above
/// `size` with every class below it smaller than `size`, `None` above
/// `MEDIUM_MAX`.
proof fn lemma_class_of_for(size: usize)
    ensures
        forall|c: int|
            0 <= c < NCLASSES && size <= crate::layout::spec_class_size(c) && #[trigger] crate::layout::classes_below(c, size as int)
                ==> spec_class_for(size, 1) == Some(c as usize),
        size > MEDIUM_MAX ==> spec_class_for(size, 1) is None,
{
    assert forall|c: int|
        0 <= c < NCLASSES && size <= crate::layout::spec_class_size(c) && #[trigger] crate::layout::classes_below(c, size as int)
            implies spec_class_for(size, 1) == Some(c as usize) by {
        reveal(crate::layout::classes_below);
        crate::vspec::class::axiom_class_table_wf(c);
        assert forall|j: int| 0 <= j < c implies !#[trigger] class_serves(j, size as int, 1) by {
            crate::vspec::class::axiom_class_table_wf(j);
            assert(crate::layout::spec_class_size(j) < size);
        }
        lemma_class_for_some(size, 1, c);
    }
    if size > MEDIUM_MAX {
        assert forall|j: int| 0 <= j < NCLASSES implies !#[trigger] class_serves(j, size as int, 1) by {
            crate::vspec::class::axiom_class_table_wf(j);
        }
        lemma_class_for_none(size, 1);
    }
}

/// `realloc(p, n)` for `p`'s usable size `n` ([`usable_of`]) keeps `p`
/// ([`keeps`], with the run's usable bytes `n`) exactly when `p` is a
/// slot, or a run of more than `MEDIUM_MAX` usable bytes: a slot's usable
/// size is its class's, which maps back to the class, and a run's is
/// whole pages, which round up to themselves, but a run of `MEDIUM_MAX`
/// bytes or fewer (allocated for a small size at an alignment above
/// `MEDIUM_MAX`) has a usable size some class serves.
pub proof fn lemma_keeps_usable(p: *mut u8, n: usize)
    requires
        heap::regions_ok(),
        heap::free_target(p),
        usable_of(p, n as int),
    ensures
        keeps(p, n, 1, n as int) <==> heap::slot_start(p) || n > MEDIUM_MAX,
{
    let r = crate::layout::process_regions();
    if heap::slot_start(p) {
        let c = choose|c: int| #[trigger] r.is_slot(c, p@.addr);
        crate::vspec::class::axiom_class_table_wf(c);
        assert(n == class_table()[c].size);
        assert forall|j: int| 0 <= j < c implies !#[trigger] class_serves(j, n as int, 1) by {
            crate::vspec::class::axiom_class_table_wf(j);
            crate::layout::lemma_class_size_mono(j, c);
        }
        lemma_class_for_some(n, 1, c);
        lemma_keeps_slot(p, c, n, 1, n as int);
    } else {
        lemma_keeps_run(p, n, 1, n as int);
        let lg = r.lg_page;
        let pg = (1usize << lg) as int;
        crate::vspec::usable::lemma_page_shift(r.huge_len);
        // `n` is whole pages, at least one, so it rounds up to itself.
        let k = (n as int) / pg;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(n as int, pg);
        assert(n + pg - 1 == pg * k + (pg - 1));
        vstd::arithmetic::div_mod::lemma_div_multiples_vanish_fancy(k, pg - 1, pg);
        assert(k != 0) by (nonlinear_arith)
            requires
                n == pg * k,
                n >= pg,
                pg > 0,
        ;
        assert((n + pg - 1) / pg == k);
        assert(k * pg == n) by (nonlinear_arith)
            requires
                n == pg * k,
        ;
        assert(crate::vspec::zero::huge_usable(n as int) == n);
        if n > MEDIUM_MAX {
            lemma_class_of_for(n);
        } else {
            crate::layout::lemma_last_class_size();
            crate::vspec::class::axiom_class_table_wf(NCLASSES - 1);
            lemma_class_for_one(n, NCLASSES - 1);
        }
    }
}

/// The reallocation decision shared by `realloc` and `rallocx`: `None`
/// when the live allocation `p` already serves `size` bytes at `align`
/// (a power of two; 1 or the quantum means the natural alignment) and
/// keeps its pointer, exactly when the new size maps to `p`'s class
/// (whose slots satisfy the alignment by construction) or, when no class
/// serves it, to a huge run of the same number of pages at a start that
/// satisfies it ([`keeps`]; the kept block fits: [`kept`]); otherwise
/// `Some(old)`, the usable bytes of `p` ([`usable_of`]), the most
/// a move has to copy: the class's size for a slot, the run's usable bytes
/// from its descriptor for a huge page start. Aborts, naming `what`, unless `p`
/// is a live slab slot or huge run start; so it returns only for a free
/// target, and `blk`, the caller's token for `p` ([`Live::claims`]), is
/// then the live block's, with `old` usable bytes, or `size` or more when
/// `p` is kept.
// realloc's path: left as written so its codegen does not move.
#[allow(clippy::manual_is_multiple_of)]
pub(super) fn old_size_or_keep(shared: &Shared, what: &str, p: *mut u8, size: usize, align: usize, Tracked(blk): Tracked<&Live>) -> (r: Option<usize>)
    requires
        crate::sys::may_abort() || (crate::sys::valid_aborts() && blk.block(p)),
        crate::sys::may_syscall(),
        shared.wf(),
        is_pow2(align),
        blk.claims(p),
    ensures
        heap::free_target(p),
        r is None ==> kept(p, size as int, align as int),
        r is None <==> keeps(p, size, align, blk.len()),
        r matches Some(o) ==> usable_of(p, o as int),
        r is None ==> blk.gives(p, size as int),
        r matches Some(o) ==> blk.block(p) && o == blk.len(),
{
    proof {
        shared.lemma_wf();
        shared.lemma_wf_slow();
        shared.lemma_wf_ready();
        shared.lemma_huge_pages();
        shared.lemma_regions_ok();
        crate::meta::lemma_pow2_bits(align);
    }
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            proof {
                if blk.block(p) {
                    // The live slot, whose token `blk` holds.
                    blk.lemma_block_slot_addr(p, class as int);
                }
            }
            shared.table(class).check_live(slab as usize, slot as usize, p as usize, Tracked(blk.slot_ref()));
            proof {
                // `check_live` returned: not a guard slab.
                crate::layout::process_regions().lemma_is_slot(class as int, slab as int, slot as int, p as usize);
            }
            proof {
                blk.lemma_claims_slot(p, class as int);
                blk.lemma_claimed_block(p);
                blk.lemma_slot_len(p, class as int);
                lemma_keeps_slot(p, class as int, size, align, blk.len());
            }
            if class_for(size, align) == Some(class) {
                proof {
                    lemma_class_aligned(class, align);
                    heap::lemma_slot_fits(p, class as int, size as int, align);
                    heap::lemma_slot_fits_quantum(p, class as int, size as int);
                }
                return None;
            }
            proof {
                // A slot belongs to one class, and its usable size is the
                // class's.
                crate::vspec::usable::lemma_usable_slot(p, class as int, size as int);
            }
            Some(class_size(class))
        }
        Kind::Huge { page } => {
            proof {
                if blk.block(p) {
                    // The live run at `page`, whose token `blk` holds.
                    blk.lemma_claims(p);
                    shared.lemma_run_page(p, page, *blk);
                }
            }
            let old = shared.huge.usable_size(page, Tracked(blk.run_ref()));
            proof {
                shared.lemma_run_page(p, page, *blk);
                // What the descriptor reads is `p`'s usable size; a page
                // start of the huge region is no slot.
                shared.lemma_huge_run_usable(p as usize, old);
                crate::vspec::usable::lemma_usable_run(shared, p, old);
                // The test is `keeps`'s run half.
                lemma_keeps_run(p, size, align, blk.len());
                lemma_usable_for_run(shared, p, size, old);
            }
            if class_for(size, align).is_none() && shared.huge.usable_for(size) == Some(old) && (p as usize) % align == 0 {
                proof {
                    heap::lemma_quantum_pow2();
                    heap::lemma_huge_fits(p, 0, align, QUANTUM);
                    // `size` is at most the run's usable bytes, which lie
                    // in the region.
                    crate::vspec::usable::lemma_run_bytes_block(p@.addr, old as int, size as int);
                }
                return None;
            }
            Some(old)
        }
        Kind::Invalid => {
            proof {
                if blk.block(p) {
                    // A slot start or a page start of the huge region,
                    // which `classify` never calls `Invalid`.
                    blk.lemma_block_target(p);
                    if heap::slot_start(p) {
                        let c = choose|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr);
                        assert(shared.regions.is_slot_addr(c, p@.addr));
                    }
                }
                assert(!blk.block(p));
            }
            crate::sys::abort_fmt(format_args!("{what} of {:#x}: not an allocation", p as usize))
        },
    }
}

/// `realloc`. The same pointer comes back when the new size maps to the
/// same class or, for a size no class serves, the same number of huge
/// pages (`old_size_or_keep`; exactly then: [`keeps`] in [`realloc_block`]);
/// otherwise allocate, copy `min(old usable, new size)`, free. On failure
/// the old block is intact and `errno = ENOMEM`; `realloc(p, 0)` frees `p`
/// and returns null without setting `errno` (a free never starts the
/// background thread, and leaves `errno` as it was). C holds the
/// token of the block it passes ([`heap::live::axiom_c_claim`]), and gets
/// the result's.
pub fn realloc(p: *mut u8, size: usize, Tracked(errno): Tracked<&mut ThreadErrno>, Tracked(c): Tracked<CCall>) -> (r: *mut u8)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        p@.addr != 0 && size == 0 ==> r@.addr == 0,
        heap::fits(r, size as int, QUANTUM as int),
        p@.addr != 0 ==> heap::free_target(p),
        r@.addr == 0 && !(p@.addr != 0 && size == 0) ==> final(errno).val() == ENOMEM,
        p@.addr != 0 && size == 0 ==> final(errno).val() == old(errno).val(),
{
    let tracked blk = heap::live::axiom_c_claim(c);
    let tracked mut out = Live::empty();
    realloc_block(p, size, Tracked(blk), Tracked(&mut out), Tracked(errno))
}

/// `realloc`'s copy: the first `n` bytes of the live block `p` into the
/// new live block `q`, with both blocks' permissions (`blk`'s to read,
/// `out`'s to write; two permissions held at once share no byte, so the
/// ranges do not overlap). Afterwards `out`'s byte state reads its first
/// `n` bytes as `blk`'s ([`Live::copied_from`]); `blk` is unchanged.
/// It reads `p`'s bytes, so it needs `may_read_user`, which the realloc
/// cores grant for this copy ([`realloc_block`], [`realloc_usable_stored`],
/// `api::jemalloc::rallocx_zeroed`).
///
/// # Safety
/// `p` and `q` are live blocks of at least `n` usable bytes each, which
/// do not overlap (the permissions show it).
#[inline(always)]
pub(super) unsafe fn copy_moved(p: *mut u8, q: *mut u8, n: usize, Tracked(blk): Tracked<&mut Live>, Tracked(out): Tracked<&mut Live>)
    requires
        crate::vspec::user::may_read_user(),
        heap::regions_ok(),
        old(blk).block(p),
        old(out).block(q),
        n <= old(blk).len(),
        n <= old(out).len(),
    ensures
        *final(blk) == *old(blk),
        final(out).same_but_bytes(*old(out)),
        final(out).copied_from(*old(blk), n as int),
        final(out).mem().frame(old(out).mem(), q@.addr as int, n as int),
{
    proof {
        blk.lemma_mem();
        out.lemma_mem();
        blk.lemma_mem_apart(out);
    }
    let ghost orig = *out;
    let tracked mut d = out.take_mem();
    // SAFETY: the permissions make `p` readable and `q` writable for `n`
    // bytes, and the two share no byte.
    unsafe { crate::vspec::user::copy_user(p, q, n, Tracked(blk.mem_ref()), Tracked(&mut d)) };
    proof {
        out.put_mem(d, orig);
    }
}

/// [`realloc`] with the tokens: `blk` is the caller's for `p`
/// ([`Live::claims`]), and `out` gets the result's: a new block's, or
/// `blk` itself when `p` is kept or the move fails (`p` is then still the
/// caller's). The new block is disjoint from `p`
/// ([`Live::theorem_apart`]) for the copy.
#[inline(always)]
pub fn realloc_block(p: *mut u8, size: usize, Tracked(blk): Tracked<Live>, Tracked(out): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        blk.claims(p),
    ensures
        p@.addr != 0 && size == 0 ==> r@.addr == 0,
        heap::fits(r, size as int, QUANTUM as int),
        p@.addr != 0 ==> heap::free_target(p),
        final(out).gives(r, size as int),
        p@.addr != 0 ==> blk.block(p),
        p@.addr != 0 && size != 0 && r@.addr == 0 ==> *final(out) == blk,
        p@.addr != 0 && size != 0 && r@.addr != 0 && r@.addr != p@.addr ==> final(out).copied_from(blk, if blk.len() < size { blk.len() } else { size as int }),
        p@.addr != 0 && size != 0 ==> (r@.addr == p@.addr <==> keeps(p, size, 1, blk.len())),
        r@.addr == 0 && !(p@.addr != 0 && size == 0) ==> final(errno).val() == ENOMEM,
        p@.addr != 0 && size == 0 ==> final(errno).val() == old(errno).val(),
{
    if p.is_null() {
        return malloc_block(size, Tracked(out), Tracked(errno));
    }
    if size == 0 {
        free_block(p, Tracked(blk), Tracked(&mut *errno));
        return ptr::null_mut();
    }
    let Some(shared) = super::shared_if_ready() else { free_before_init(p) };
    proof {
        crate::meta::lemma_is_pow2_one();
    }
    let tracked mut blk = blk;
    let Some(old) = old_size_or_keep(shared, "realloc", p, size, 1, Tracked(&blk)) else {
        proof {
            out.set(blk);
        }
        return p;
    };
    let q = malloc_block(size, Tracked(out), Tracked(errno));
    if q.is_null() {
        // The block at `p` stays the caller's, with its token.
        proof {
            out.set(blk);
        }
        return q;
    }
    proof {
        broadcast use vstd::layout::align_of_u8;
        shared.lemma_regions_ok();
        blk.theorem_apart(out);
        // The new block is apart from `p`'s, so not at `p`: a move.
        blk.lemma_len_pos();
        assert(q@.addr != p@.addr);
        // The copy reads `p`'s bytes: realloc's one read of user memory.
        crate::vspec::user::grant_user_reads();
    }
    // SAFETY: `blk` holds the permission to `p`'s `old` usable bytes and
    // `out` to `q`'s `size` or more; the two share no byte.
    unsafe { copy_moved(p, q, old.min(size), Tracked(&mut blk), Tracked(out)) };
    heap::free(shared, p, Tracked(blk), Tracked(&mut *errno));
    q
}

/// Realloc follows the class/page keep rule, composed as C
/// calls it: `n = malloc_usable_size(p)` ([`usable_size`], the live block's
/// usable size with its token `blk`), then `realloc(p, n)`
/// ([`realloc_block`] with the same token, as `axiom_c_claim` gives it)
/// returns `p` exactly when `p` is a slot, or a run of more than
/// `MEDIUM_MAX` usable bytes ([`lemma_keeps_usable`]).
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such function.
#[cfg(verus_keep_ghost)]
fn theorem_realloc_to_usable_size(p: *mut u8, Tracked(blk): Tracked<Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: (usize, *mut u8))
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        blk.claims(p),
        p@.addr != 0,
    ensures
        heap::free_target(p),
        usable_of(p, r.0 as int),
        r.1@.addr == p@.addr <==> heap::slot_start(p) || r.0 > MEDIUM_MAX,
{
    let Some(shared) = super::shared_if_ready() else { free_before_init(p) };
    let n = usable_size(shared, p, Tracked(&blk));
    proof {
        shared.lemma_regions_ok();
        blk.lemma_len_pos();
        lemma_keeps_usable(p, n);
    }
    let tracked mut out = Live::empty();
    let q = realloc_block(p, n, Tracked(blk), Tracked(&mut out), Tracked(errno));
    (n, q)
}

/// The lookups of a live block that `malloc_usable_size` ([`usable_size`])
/// and `realloc` ([`old_size_or_keep`]; `challoc_realloc_usable`'s
/// [`realloc_old`]) make, with the block's token, reach none of their
/// not-an-allocation, invalid-free or double-free checks: they verify
/// without [`crate::sys::may_abort`], with only [`crate::sys::valid_aborts`]'
/// two capabilities. `realloc`'s allocation after a move, and its free of
/// the old block, are not covered.
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such function.
#[cfg(verus_keep_ghost)]
fn theorem_valid_lookups_never_abort(shared: &Shared, p: *mut u8, size: usize, Tracked(blk): Tracked<&Live>)
    requires
        crate::sys::may_syscall(),
        crate::sys::kernel_aborts(),
        crate::sys::pending_aborts(),
        shared.wf(),
        blk.block(p),
{
    proof {
        crate::meta::lemma_is_pow2_one();
        shared.lemma_regions_ok();
        blk.lemma_claims(p);
    }
    let _ = usable_size(shared, p, Tracked(blk));
    let _ = old_size_or_keep(shared, "realloc", p, size, 1, Tracked(blk));
    let _ = realloc_old(shared, p, size, Tracked(blk));
}

/// `reallocarray`: `realloc(p, n * m)` with overflow reported as `ENOMEM`.
pub fn reallocarray(p: *mut u8, n: usize, m: usize, Tracked(errno): Tracked<&mut ThreadErrno>, Tracked(c): Tracked<CCall>) -> (r: *mut u8)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        n * m > usize::MAX ==> r@.addr == 0 && final(errno).val() == ENOMEM,
        heap::fits(r, n * m, QUANTUM as int),
        p@.addr != 0 && n * m <= usize::MAX ==> heap::free_target(p),
        r@.addr == 0 && !(p@.addr != 0 && n * m == 0) ==> final(errno).val() == ENOMEM,
        p@.addr != 0 && n * m == 0 ==> final(errno).val() == old(errno).val(),
{
    match n.checked_mul(m) {
        Some(size) => realloc(p, size, Tracked(errno), Tracked(c)),
        None => enomem(Tracked(errno)),
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
/// The write goes through `uout`, the permission the entry point was
/// handed for the caller's out-pointer (`exports::axiom_c_out`), and the
/// token is made from it.
#[inline(always)]
fn store_usable(usable: *mut usize, n: usize, Tracked(uout): Tracked<&mut COut<usize>>) -> (s: Tracked<Stored>)
    requires
        old(uout).of(usable),
    ensures
        final(uout).of(usable),
        s@.ptr() == usable,
        s@.val() == n,
{
    if !usable.is_null() {
        let tracked mut perm = uout.take();
        // The caller's `size_t`, through its permission.
        ptr_mut_write(usable, Tracked(&mut perm), n);
        proof {
            uout.put(perm);
        }
    }
    Tracked(Stored::stored(uout, usable, n))
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
/// pop used; everything else is one tail call away, as in `malloc`. The
/// block's token goes to C with it. `uout` is the call's token for
/// `usable`, minted by the export wrapper (`exports::axiom_c_out`).
#[inline(always)]
pub fn malloc_usable(size: usize, usable: *mut usize, Tracked(uout): Tracked<COut<usize>>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        uout.of(usable),
    ensures
        heap::fits(r, size as int, QUANTUM as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let tracked mut uout = uout;
    let tracked mut blk = Live::empty();
    malloc_usable_stored(size, usable, Tracked(&mut uout), Tracked(&mut blk), Tracked(errno)).0
}

/// [`malloc_usable`] with the token for what it stored through `usable`:
/// the block's usable size ([`usable_is`]: its class's size, or `size`
/// rounded up to whole pages for a huge run), and 0 for a null result.
/// `blk` gets the block's token, as in [`malloc_block`].
#[inline(always)]
pub fn malloc_usable_stored(size: usize, usable: *mut usize, Tracked(uout): Tracked<&mut COut<usize>>, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: (*mut u8, Tracked<Stored>))
    requires
        old(uout).of(usable),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        final(uout).of(usable),
        heap::fits(r.0, size as int, QUANTUM as int),
        r.1@.ptr() == usable,
        r.0@.addr != 0 ==> usable_is(r.0, size as int, r.1@.val() as int),
        final(blk).gives(r.0, size as int),
        r.0@.addr == 0 ==> r.1@.val() == 0,
        r.0@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let Some(c) = class_index(size) else { return malloc_usable_huge(size, usable, Tracked(uout), Tracked(blk), Tracked(errno)) };
    let shared = super::shared_ptr();
    let Some(h) = heap::existing_heap(shared) else {
        let (p, s) = malloc_usable_class(c, usable, Tracked(uout), Tracked(blk), Tracked(errno));
        proof {
            lemma_class_fits(p, c as int, size);
            if p@.addr != 0 {
                crate::vspec::usable::lemma_usable_slot(p, c as int, size as int);
            }
            lemma_class_gives(*blk, p, c as int, size);
        }
        return (p, s);
    };
    // SAFETY: as in `malloc`.
    let flags = unsafe { heap::fast_flags(shared) };
    proof {
        h.lemma_wf_regions();
    }
    match h.alloc_fast(c, flags, Tracked(blk)) {
        Some(p) => {
            let s = store_usable(usable, class_size(c), Tracked(uout));
            proof {
                lemma_class_fits(p, c as int, size);
                heap::theorem_no_block_at_zero(p, c as int, 0);
                crate::vspec::usable::lemma_usable_slot(p, c as int, size as int);
                lemma_class_gives(*blk, p, c as int, size);
            }
            (p, s)
        }
        None => {
            let (p, s) = malloc_usable_class(c, usable, Tracked(uout), Tracked(blk), Tracked(errno));
            proof {
                lemma_class_fits(p, c as int, size);
                if p@.addr != 0 {
                    crate::vspec::usable::lemma_usable_slot(p, c as int, size as int);
                }
                lemma_class_gives(*blk, p, c as int, size);
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
fn malloc_usable_class(class: usize, usable: *mut usize, Tracked(uout): Tracked<&mut COut<usize>>, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: (*mut u8, Tracked<Stored>))
    requires
        old(uout).of(usable),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        class < NCLASSES,
    ensures
        final(uout).of(usable),
        heap::regions_ok(),
        r.0@.addr != 0 ==> crate::layout::process_regions().is_slot(class as int, r.0@.addr),
        r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
        r.1@.ptr() == usable,
        r.0@.addr == 0 ==> r.1@.val() == 0,
        r.0@.addr != 0 ==> r.1@.val() == class_table()[class as int].size,
        r.0@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let shared = super::shared();
    proof {
        shared.lemma_regions_ok();
    }
    let p = heap::alloc_declined(shared, class, Tracked(blk), Tracked(&mut *errno));
    if p.is_null() {
        let s = store_usable(usable, 0, Tracked(uout));
        return (enomem(Tracked(errno)), s);
    }
    let s = store_usable(usable, class_size(class), Tracked(uout));
    (p, s)
}

/// `malloc_usable` of a huge size: the run's usable bytes come from the
/// huge table, the same read `malloc_usable_size` does.
#[cold]
#[inline(never)]
fn malloc_usable_huge(size: usize, usable: *mut usize, Tracked(uout): Tracked<&mut COut<usize>>, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: (*mut u8, Tracked<Stored>))
    requires
        old(uout).of(usable),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        final(uout).of(usable),
        heap::fits(r.0, size as int, QUANTUM as int),
        r.1@.ptr() == usable,
        r.0@.addr == 0 ==> r.1@.val() == 0,
        r.0@.addr != 0 ==> usable_is(r.0, size as int, r.1@.val() as int),
        final(blk).gives(r.0, size as int),
        r.0@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let shared = super::shared();
    proof {
        crate::meta::lemma_is_pow2_one();
        shared.lemma_regions_ok();
    }
    let (p, _, _) = alloc_huge(shared, size, 1, Tracked(blk));
    proof {
        lemma_huge_result(p, size, 1);
    }
    if p.is_null() {
        let s = store_usable(usable, 0, Tracked(uout));
        return (enomem(Tracked(errno)), s);
    }
    let s = store_usable(usable, usable_size(shared, p, Tracked(&*blk)), Tracked(uout));
    proof {
        // The run `alloc_huge` just returned, with its token: its
        // descriptor records `size` rounded up to whole pages.
        blk.lemma_claims(p);
        lemma_huge_usable(shared, p, size, s@.val());
    }
    (p, s)
}

/// The usable size of the run `alloc_huge` just returned for `size`
/// bytes, as the block's token states it (`u` is the token's
/// [`Live::len`], which `alloc_huge` ensures is `shared.huge_usable(size)`),
/// is `size` rounded up to whole pages.
proof fn lemma_huge_usable(shared: &Shared, p: *mut u8, size: usize, u: usize)
    requires
        shared.wf(),
        p@.addr != 0,
        crate::layout::huge_block(p@.addr, size, 1),
        size <= shared.huge_usable(size),
        u == shared.huge_usable(size),
    ensures
        crate::layout::process_regions().is_huge_block(p@.addr, 1),
        size <= u,
        u == crate::vspec::zero::huge_usable(size as int),
        usable_is(p, size as int, u as int),
{
    shared.lemma_wf();
    shared.lemma_wf_slow();
    shared.lemma_wf_ready();
    shared.lemma_regions_ok();
    assert(crate::layout::process_regions().is_huge_block(p@.addr, 1));
    shared.lemma_huge_usable(size);
    crate::vspec::usable::lemma_usable_huge(p, size as int);
}

/// `challoc_calloc_usable(n, m, usable)`: `calloc(n, m)` with the usable
/// size stored as in [`malloc_usable`]. The whole usable size reads as
/// zero, not only the `n * m` bytes asked for: the caller is told it owns
/// the usable bytes (Valkey's `zcalloc_usable` hands them on), and that is
/// what jemalloc's `calloc` zeroes. The block's token goes to C with it.
/// `uout` is the call's token for `usable`, as in [`malloc_usable`].
pub fn calloc_usable(n: usize, m: usize, usable: *mut usize, Tracked(uout): Tracked<COut<usize>>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        uout.of(usable),
    ensures
        n * m > usize::MAX ==> r@.addr == 0,
        heap::fits(r, n * m, QUANTUM as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let tracked mut uout = uout;
    let tracked mut blk = Live::empty();
    calloc_usable_zeroed(n, m, usable, Tracked(&mut uout), Tracked(&mut blk), Tracked(errno)).0
}

/// [`calloc_usable`] with the proof that its memory reads as zero, and
/// the token for what it stored through `usable`. A non-null result comes
/// with a [`Zeroed`] token for its `n * m` bytes and for its whole usable
/// size ([`covers_usable`]), and `usable` holds that size ([`usable_is`]:
/// the class's size, or `n * m` rounded up to whole pages); a null result
/// stores 0. `blk` gets the block's token. For a huge request the `memset`
/// length is the run's usable size read back from its descriptor
/// ([`usable_size`]), which is the usable size of the block's token: the
/// request rounded up to whole pages.
#[inline(always)]
pub fn calloc_usable_zeroed(n: usize, m: usize, usable: *mut usize, Tracked(uout): Tracked<&mut COut<usize>>, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: (*mut u8, Tracked<Zeroed>, Tracked<Stored>))
    requires
        old(uout).of(usable),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        final(uout).of(usable),
        n * m > usize::MAX ==> r.0@.addr == 0,
        heap::fits(r.0, n * m, QUANTUM as int),
        r.0@.addr != 0 ==> r.1@.covers(r.0, n * m),
        r.0@.addr != 0 ==> covers_usable(r.1@, r.0, 0, n * m),
        r.2@.ptr() == usable,
        r.0@.addr == 0 ==> r.2@.val() == 0,
        r.0@.addr != 0 ==> usable_is(r.0, n * m, r.2@.val() as int),
        final(blk).gives(r.0, n * m),
        r.0@.addr != 0 ==> final(blk).reads_zero(0, final(blk).len()),
        r.0@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let Some(size) = n.checked_mul(m) else {
        let s = store_usable(usable, 0, Tracked(uout));
        return (enomem(Tracked(errno)), Tracked(Zeroed::empty(0)), s);
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
            let (p, zeroed, z) = heap::alloc_zeroed(shared, c, Tracked(blk), Tracked(&mut *errno));
            proof {
                cls = c as int;
                lemma_class_fits(p, c as int, size);
                if p@.addr != 0 {
                    crate::vspec::usable::lemma_usable_slot(p, c as int, size as int);
                    // `p`'s token covers the class size, the zero fill's length.
                    blk.lemma_slot_len(p, c as int);
                    if zeroed {
                        crate::vspec::zero::lemma_covers_slot(z@, p, c as int, 0, size as int);
                    }
                }
                lemma_class_gives(*blk, p, c as int, size);
                if p@.addr != 0 {
                    blk.lemma_slot_len(p, c as int);
                }
            }
            (p, zeroed, class_size(c), z)
        }
        None => {
            let (p, zeroed, z) = alloc_huge(shared, size, 1, Tracked(blk));
            proof {
                lemma_huge_result(p, size, 1);
                if p@.addr != 0 {
                    blk.lemma_claims(p);
                }
            }
            let u = if p.is_null() { 0 } else { usable_size(shared, p, Tracked(&*blk)) };
            proof {
                if p@.addr != 0 {
                    // The run `alloc_huge` just returned, with its token.
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
        let s = store_usable(usable, 0, Tracked(uout));
        return (enomem(Tracked(errno)), Tracked(Zeroed::empty(0)), s);
    }
    let z = if !zeroed {
        let ghost orig = *blk;
        proof {
            blk.lemma_mem();
        }
        let tracked mut mem = blk.take_mem();
        // SAFETY: `mem` is the permission to the fresh block's `u` usable
        // bytes.
        let z = unsafe { crate::vspec::user::zero_user(p, u, Tracked(&mut mem)) };
        proof {
            blk.put_mem(mem, orig);
            // `u` is the block's usable size.
            if cls >= 0 {
                crate::vspec::zero::lemma_covers_slot(z@, p, cls, 0, size as int);
            } else {
                crate::vspec::zero::lemma_covers_huge(z@, p, 0, size as int);
            }
        }
        z
    } else {
        // Known zero: the allocator's token for the block's `u` usable
        // bytes, which its permission then reads as zero.
        let Tracked(za) = z;
        let tracked z0;
        proof {
            let tracked (zs, _) = za.split(u as int);
            blk.lemma_mem();
            let ghost orig = *blk;
            let tracked mut mem = blk.take_mem();
            crate::vspec::user::axiom_zeroed_bytes(&zs, &mut mem);
            blk.put_mem(mem, orig);
            if cls >= 0 {
                crate::vspec::zero::lemma_covers_slot(zs, p, cls, 0, size as int);
            } else {
                crate::vspec::zero::lemma_covers_huge(zs, p, 0, size as int);
            }
            z0 = zs;
        }
        Tracked(z0)
    };
    let s = store_usable(usable, u, Tracked(uout));
    (p, z, s)
}

/// `challoc_realloc_usable(p, size, usable)`: `realloc(p, size)` with the
/// usable size of the returned block stored as in [`malloc_usable`] (0
/// when null comes back, `realloc(p, 0)` included). The block kept in
/// place reports its class size from the classification `realloc` does
/// anyway; a moved block reports what the `malloc_usable` that made it
/// reported. C holds the token of the block it passes
/// ([`heap::live::axiom_c_claim`]), and gets the result's. `uout` is the
/// call's token for `usable`, as in [`malloc_usable`].
pub fn realloc_usable(p: *mut u8, size: usize, usable: *mut usize, Tracked(uout): Tracked<COut<usize>>, Tracked(errno): Tracked<&mut ThreadErrno>, Tracked(c): Tracked<CCall>) -> (r: *mut u8)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        uout.of(usable),
    ensures
        p@.addr != 0 && size == 0 ==> r@.addr == 0,
        heap::fits(r, size as int, QUANTUM as int),
        p@.addr != 0 ==> heap::free_target(p),
        r@.addr == 0 && !(p@.addr != 0 && size == 0) ==> final(errno).val() == ENOMEM,
        p@.addr != 0 && size == 0 ==> final(errno).val() == old(errno).val(),
{
    let tracked blk = heap::live::axiom_c_claim(c);
    let tracked mut uout = uout;
    let tracked mut out = Live::empty();
    realloc_usable_stored(p, size, usable, Tracked(&mut uout), Tracked(blk), Tracked(&mut out), Tracked(errno)).0
}

/// [`realloc_usable`] with the token for what it stored through `usable`:
/// for a non-null result, its usable size for a request of `size` bytes
/// ([`usable_is`]), whether the block was kept (the class's size, or a
/// run's usable size equal to `size` rounded up to whole pages: [`kept`]'s
/// test) or moved (what [`malloc_usable_stored`] stored). The block
/// tokens are as in [`realloc_block`].
pub fn realloc_usable_stored(p: *mut u8, size: usize, usable: *mut usize, Tracked(uout): Tracked<&mut COut<usize>>, Tracked(blk): Tracked<Live>, Tracked(out): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: (*mut u8, Tracked<Stored>))
    requires
        old(uout).of(usable),
        crate::sys::may_abort(),
        blk.claims(p),
        crate::sys::may_syscall(),
    ensures
        final(uout).of(usable),
        p@.addr != 0 && size == 0 ==> r.0@.addr == 0,
        heap::fits(r.0, size as int, QUANTUM as int),
        p@.addr != 0 ==> heap::free_target(p),
        r.1@.ptr() == usable,
        r.0@.addr != 0 ==> usable_is(r.0, size as int, r.1@.val() as int),
        final(out).gives(r.0, size as int),
        p@.addr != 0 ==> blk.block(p),
        p@.addr != 0 && size != 0 && r.0@.addr == 0 ==> *final(out) == blk,
        p@.addr != 0 && size != 0 && r.0@.addr != 0 && r.0@.addr != p@.addr ==> final(out).copied_from(blk, if blk.len() < size { blk.len() } else { size as int }),
        p@.addr != 0 && size != 0 ==> (r.0@.addr == p@.addr <==> keeps(p, size, 1, blk.len())),
        r.0@.addr == 0 ==> r.1@.val() == 0,
        r.0@.addr == 0 && !(p@.addr != 0 && size == 0) ==> final(errno).val() == ENOMEM,
        p@.addr != 0 && size == 0 ==> final(errno).val() == old(errno).val(),
{
    if p.is_null() {
        return malloc_usable_stored(size, usable, Tracked(uout), Tracked(out), Tracked(errno));
    }
    if size == 0 {
        free_block(p, Tracked(blk), Tracked(&mut *errno));
        let s = store_usable(usable, 0, Tracked(uout));
        return (ptr::null_mut(), s);
    }
    let Some(shared) = super::shared_if_ready() else { free_before_init(p) };
    let tracked mut blk = blk;
    let (keep, old) = realloc_old(shared, p, size, Tracked(&blk));
    if keep {
        let s = store_usable(usable, old, Tracked(uout));
        proof {
            out.set(blk);
        }
        return (p, s);
    }
    let (q, s) = malloc_usable_stored(size, usable, Tracked(uout), Tracked(out), Tracked(errno));
    if q.is_null() {
        // The block at `p` stays the caller's, with its token.
        proof {
            out.set(blk);
        }
        return (q, s);
    }
    proof {
        broadcast use vstd::layout::align_of_u8;
        shared.lemma_regions_ok();
        blk.theorem_apart(out);
        // The new block is apart from `p`'s, so not at `p`: a move.
        blk.lemma_len_pos();
        assert(q@.addr != p@.addr);
        // The copy reads `p`'s bytes: realloc's one read of user memory.
        crate::vspec::user::grant_user_reads();
    }
    // SAFETY: `blk` holds the permission to `p`'s `old` usable bytes and
    // `out` to `q`'s `size` or more; the two share no byte.
    unsafe { copy_moved(p, q, old.min(size), Tracked(&mut blk), Tracked(out)) };
    heap::free(shared, p, Tracked(blk), Tracked(&mut *errno));
    (q, s)
}

/// [`old_size_or_keep`]'s decision for `realloc` (alignment 1) with the
/// old usable size reported in both outcomes: `(keep, usable)`, `keep`
/// exactly when `size` maps to `p`'s class or, with no class, to a huge
/// run of the same page count ([`keeps`]; it fits: [`kept`]). The same
/// checks and aborts as `old_size_or_keep`, in a function of its own so
/// that `realloc` and `rallocx` keep their code.
///
/// The usable size is `p`'s ([`usable_of`]) and, for a kept block, the
/// usable size of a request of `size` bytes ([`usable_is`]). `blk` as for
/// `old_size_or_keep`.
fn realloc_old(shared: &Shared, p: *mut u8, size: usize, Tracked(blk): Tracked<&Live>) -> (r: (bool, usize))
    requires
        crate::sys::may_abort() || (crate::sys::valid_aborts() && blk.block(p)),
        crate::sys::may_syscall(),
        shared.wf(),
        blk.claims(p),
    ensures
        heap::free_target(p),
        r.0 ==> kept(p, size as int, 1),
        r.0 <==> keeps(p, size, 1, blk.len()),
        usable_of(p, r.1 as int),
        r.0 ==> usable_is(p, size as int, r.1 as int),
        blk.block(p),
        r.1 == blk.len(),
        r.0 ==> size <= blk.len(),
{
    proof {
        shared.lemma_wf();
        shared.lemma_wf_slow();
        shared.lemma_wf_ready();
        shared.lemma_huge_pages();
        shared.lemma_regions_ok();
    }
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            proof {
                if blk.block(p) {
                    // The live slot, whose token `blk` holds.
                    blk.lemma_block_slot_addr(p, class as int);
                }
            }
            shared.table(class).check_live(slab as usize, slot as usize, p as usize, Tracked(blk.slot_ref()));
            proof {
                // `check_live` returned: not a guard slab.
                crate::layout::process_regions().lemma_is_slot(class as int, slab as int, slot as int, p as usize);
            }
            proof {
                crate::vspec::class::axiom_class_table_wf(class as int);
                if size <= class_table()[class as int].size {
                    heap::lemma_slot_fits_quantum(p, class as int, size as int);
                }
                crate::vspec::usable::lemma_usable_slot(p, class as int, size as int);
                blk.lemma_claims_slot(p, class as int);
                blk.lemma_claimed_block(p);
                blk.lemma_slot_len(p, class as int);
                // `class_of` is `class_for` at alignment 1.
                lemma_class_of_for(size);
                lemma_keeps_slot(p, class as int, size, 1, blk.len());
            }
            (class_of(size) == Some(class), class_size(class))
        }
        Kind::Huge { page } => {
            proof {
                if blk.block(p) {
                    // The live run at `page`, whose token `blk` holds.
                    blk.lemma_claims(p);
                    shared.lemma_run_page(p, page, *blk);
                }
            }
            let old = shared.huge.usable_size(page, Tracked(blk.run_ref()));
            proof {
                crate::meta::lemma_is_pow2_one();
                heap::lemma_quantum_pow2();
                heap::lemma_huge_fits(p, 0, 1, QUANTUM);
                shared.lemma_huge_run_usable(p as usize, old);
                crate::vspec::usable::lemma_usable_run(shared, p, old);
                shared.lemma_huge_usable(size);
                crate::vspec::usable::lemma_usable_huge(p, size as int);
                shared.lemma_run_page(p, page, *blk);
                // A kept run has `size <= old` (`usable_for`), and its
                // usable bytes lie in the region.
                if size <= old {
                    crate::vspec::usable::lemma_run_bytes_block(p@.addr, old as int, size as int);
                }
                // The test is `keeps`'s run half.
                lemma_class_of_for(size);
                lemma_keeps_run(p, size, 1, blk.len());
                lemma_usable_for_run(shared, p, size, old);
            }
            (class_of(size).is_none() && shared.huge.usable_for(size) == Some(old), old)
        }
        Kind::Invalid => {
            proof {
                if blk.block(p) {
                    // A slot start or a page start of the huge region,
                    // which `classify` never calls `Invalid`.
                    blk.lemma_block_target(p);
                    if heap::slot_start(p) {
                        let c = choose|c: int| #[trigger] crate::layout::process_regions().is_slot(c, p@.addr);
                        assert(shared.regions.is_slot_addr(c, p@.addr));
                    }
                }
                assert(!blk.block(p));
            }
            crate::sys::abort_fmt(format_args!("realloc of {:#x}: not an allocation", p as usize))
        },
    }
}

/// `challoc_free_usable(p)`: `free(p)`, returning the usable size the
/// block had (0 for null). The exact fast path of `free` (design 6.3
/// steps 1 to 4: classify, the bitmap-word test, the owner word with
/// several arenas, the push), reporting the class it verified; every other
/// outcome, the aborts included, goes through one cold call that takes the
/// size from the same classification the exact `free` then repeats, so a
/// bad pointer dies with `free`'s message. C holds the token of the block
/// it frees ([`heap::live::axiom_c_claim`]).
#[inline(always)]
pub fn free_usable(p: *mut u8, Tracked(c): Tracked<CCall>, errno: Tracked<&mut ThreadErrno>) -> (r: usize)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        p@.addr == 0 ==> r == 0,
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> usable_of(p, r as int),
        p@.addr != 0 ==> 0 < r,
{
    let tracked blk = heap::live::axiom_c_claim(c);
    free_usable_block(p, Tracked(blk), errno)
}

/// [`free_usable`] with the caller's token for `p` ([`Live::claims`]),
/// which the free takes back: the token of the live block `p`, whose
/// usable size is the result.
#[inline(always)]
pub fn free_usable_block(p: *mut u8, Tracked(blk): Tracked<Live>, errno: Tracked<&mut ThreadErrno>) -> (r: usize)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        blk.claims(p),
    ensures
        p@.addr == 0 ==> r == 0,
        p@.addr != 0 ==> heap::free_target(p),
        p@.addr != 0 ==> usable_of(p, r as int),
        p@.addr != 0 ==> 0 < r,
        p@.addr != 0 ==> blk.block(p) && r == blk.len(),
{
    let tracked mut blk = blk;
    let ghost b0 = blk;
    if p.is_null() {
        return 0;
    }
    let tracked tls = crate::sys::tls::TlsSlot::entry();
    if let Some((t, tp)) = heap::tls_heap_slot(Tracked(&*tls)) {
        // SAFETY: as in `free`.
        let shared = unsafe { super::shared_deref(super::shared_base()) };
        // SAFETY: as in `free`.
        if let Some(class) = unsafe { heap::free_fast_slot(shared, t, tp, p, Tracked(&mut blk), Tracked(tls)) } {
            proof {
                shared.lemma_regions_ok();
                crate::vspec::usable::lemma_usable_slot(p, class as int, 0);
                b0.lemma_slot_len(p, class as int);
                crate::vspec::class::axiom_class_table_wf(class as int);
            }
            return class_size(class);
        }
    }
    free_usable_slow(p, Tracked(blk), errno)
}

/// Everything `free_usable` does off its fast path: the size first, from
/// the classification `free` makes (its aborts for an invalid, stale or
/// double free, and the huge table's for a run that is not live), then the
/// exact `free` (heap creation, a full cache, another arena, the huge
/// release). The size is the usable size of the caller's token for `p`.
#[cold]
#[inline(never)]
fn free_usable_slow(p: *mut u8, Tracked(blk): Tracked<Live>, errno: Tracked<&mut ThreadErrno>) -> (r: usize)
    requires
        crate::sys::may_abort(),
        blk.claims(p),
        crate::sys::may_syscall(),
    ensures
        heap::free_target(p),
        usable_of(p, r as int),
        r == blk.len(),
        blk.block(p),
        0 < r,
{
    let Some(shared) = super::shared_if_ready() else { free_before_init(p) };
    proof {
        shared.lemma_wf();
        shared.lemma_wf_slow();
        shared.lemma_wf_ready();
        shared.lemma_regions_ok();
    }
    let usable = match shared.classify_free(p, Tracked(&blk)) {
        Ok((class, _)) => {
            proof {
                crate::vspec::usable::lemma_usable_slot(p, class as int, 0);
                blk.lemma_claims_slot(p, class as int);
                blk.lemma_claimed_block(p);
                blk.lemma_slot_len(p, class as int);
            }
            class_size(class)
        },
        Err(page) => {
            let u = shared.huge.usable_size(page, Tracked(blk.run_ref()));
            proof {
                shared.lemma_huge_run_usable(p@.addr, u);
                crate::vspec::usable::lemma_usable_run(shared, p, u);
                shared.lemma_run_page(p, page, blk);
            }
            u
        },
    };
    proof {
        blk.lemma_nonnull();
    }
    heap::free(shared, p, Tracked(blk), errno);
    usable
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
/// `errno = ENOMEM` on exhaustion. `blk` gets the block's token.
pub fn alloc_aligned(align: usize, size: usize, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        is_pow2(align),
    ensures
        heap::fits(r, size as int, align as int),
        final(blk).gives(r, size as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    debug_assert!(align.is_power_of_two());
    if align <= QUANTUM {
        let p = malloc_block(size, Tracked(blk), Tracked(errno));
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
            let p = heap::alloc(shared, c, Tracked(blk), Tracked(&mut *errno));
            proof {
                if p@.addr != 0 {
                    crate::vspec::class::axiom_class_table_wf(c as int);
                    lemma_mask_mod(class_table()[c as int].size as usize, align);
                    heap::lemma_slot_fits(p, c as int, size as int, align);
                }
                lemma_class_gives(*blk, p, c as int, size);
            }
            p
        },
        None => {
            let p = alloc_huge(shared, size, align, Tracked(blk)).0;
            proof {
                lemma_huge_result(p, size, align);
            }
            p
        },
    };
    if p.is_null() { enomem(Tracked(errno)) } else { p }
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
        crate::meta::lemma_pow2_bits(align);
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
/// two. glibc does not require `size` to be a multiple of `align`. The
/// block's token goes to C with it.
pub fn aligned_alloc(align: usize, size: usize, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        !is_pow2(align) ==> r@.addr == 0 && final(errno).val() == EINVAL,
        is_pow2(align) ==> heap::fits(r, size as int, align as int),
        is_pow2(align) && r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let tracked mut blk = Live::empty();
    aligned_alloc_block(align, size, Tracked(&mut blk), Tracked(errno))
}

/// [`aligned_alloc`] with the block's token, as [`malloc_block`].
pub fn aligned_alloc_block(align: usize, size: usize, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        !is_pow2(align) ==> r@.addr == 0 && final(errno).val() == EINVAL,
        is_pow2(align) ==> heap::fits(r, size as int, align as int),
        is_pow2(align) && r@.addr == 0 ==> final(errno).val() == ENOMEM,
        final(blk).gives(r, size as int),
{
    if !align.is_power_of_two() {
        return einval(Tracked(errno));
    }
    alloc_aligned(align, size, Tracked(blk), Tracked(errno))
}

/// Store the allocation through `posix_memalign`'s out-pointer, with
/// `oout`, the permission `posix_memalign` was handed for it
/// (`exports::axiom_c_out`), which then holds `p`.
#[inline(always)]
fn store_out(out: *mut *mut u8, p: *mut u8, Tracked(oout): Tracked<&mut COut<*mut u8>>)
    requires
        old(oout).of(out),
        out@.addr != 0,
    ensures
        final(oout).of(out),
        final(oout).opt() matches Some(q) && q.is_init() && q.value() == p,
{
    let tracked mut perm = oout.take();
    // The caller's out-pointer, through its permission.
    ptr_mut_write(out, Tracked(&mut perm), p);
    proof {
        oout.put(perm);
    }
}

/// `posix_memalign`. Returns 0, `EINVAL` (alignment not a power of two or
/// not a multiple of `sizeof(void *)`) or `ENOMEM`; never touches `errno`
/// on the `EINVAL` path. The `ENOMEM` path is `alloc_aligned`'s, which
/// also sets `errno = ENOMEM`. The block's token goes to C with it. C
/// holds the permission to write through `out`, a valid pointer by
/// `posix_memalign`'s contract: `oout`, the call's token for it, minted by
/// the export wrapper (`exports::axiom_c_out`, the trusted step of the C
/// contract for an out-pointer).
pub fn posix_memalign(out: *mut *mut u8, align: usize, size: usize, Tracked(oout): Tracked<COut<*mut u8>>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: c_int)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        oout.of(out),
        out@.addr != 0,
    ensures
        r == 0 || r == ENOMEM || r == EINVAL,
        // `sizeof(void *)` is 8 on every target challoc builds for.
        r == EINVAL <==> !(is_pow2(align) && align % 8 == 0),
        r == EINVAL ==> final(errno).val() == old(errno).val(),
        r == ENOMEM ==> final(errno).val() == ENOMEM,
{
    let tracked mut oout = oout;
    let tracked mut blk = Live::empty();
    posix_memalign_block(out, align, size, Tracked(&mut oout), Tracked(&mut blk), Tracked(errno))
}

/// [`posix_memalign`] with the token of the block it stores: on a 0
/// return, `out` holds a block of at least `size` bytes aligned to
/// `align`, and `blk` gets its token.
// `%`: Verus does not specify `usize::is_multiple_of`.
#[allow(clippy::manual_is_multiple_of)]
pub fn posix_memalign_block(out: *mut *mut u8, align: usize, size: usize, Tracked(oout): Tracked<&mut COut<*mut u8>>, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: c_int)
    requires
        old(oout).of(out),
        out@.addr != 0,
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        final(oout).of(out),
        r == 0 || r == ENOMEM || r == EINVAL,
        // `sizeof(void *)` is 8 on every target challoc builds for.
        r == EINVAL <==> !(is_pow2(align) && align % 8 == 0),
        r == 0 ==> (final(oout).opt() matches Some(q) && q.is_init() && q.value()@.addr != 0
            && final(blk).gives(q.value(), size as int) && heap::fits(q.value(), size as int, align as int)),
        r == EINVAL ==> final(errno).val() == old(errno).val(),
        r == ENOMEM ==> final(errno).val() == ENOMEM,
{
    if !align.is_power_of_two() || align % core::mem::size_of::<*mut u8>() != 0 {
        return EINVAL;
    }
    let p = alloc_aligned(align, size, Tracked(blk), Tracked(errno));
    if p.is_null() {
        return ENOMEM;
    }
    store_out(out, p, Tracked(oout));
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

/// `valloc`: page-aligned. The block's token goes to C with it.
pub fn valloc(size: usize, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        heap::fits(r, size as int, (1usize << crate::layout::process_regions().lg_page) as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let tracked mut blk = Live::empty();
    valloc_block(size, Tracked(&mut blk), Tracked(errno))
}

/// [`valloc`] with the block's token, as [`malloc_block`].
pub fn valloc_block(size: usize, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        heap::fits(r, size as int, (1usize << crate::layout::process_regions().lg_page) as int),
        final(blk).gives(r, size as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let page = page_size(super::shared());
    alloc_aligned(page, size, Tracked(blk), Tracked(errno))
}

/// `pvalloc`: page-aligned, size rounded up to a whole page. The block's
/// token goes to C with it.
pub fn pvalloc(size: usize, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        heap::fits(r, size as int, (1usize << crate::layout::process_regions().lg_page) as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let tracked mut blk = Live::empty();
    pvalloc_block(size, Tracked(&mut blk), Tracked(errno))
}

/// [`pvalloc`] with the block's token, as [`malloc_block`].
pub fn pvalloc_block(size: usize, Tracked(blk): Tracked<&mut Live>, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        heap::fits(r, size as int, (1usize << crate::layout::process_regions().lg_page) as int),
        final(blk).gives(r, size as int),
        r@.addr == 0 ==> final(errno).val() == ENOMEM,
{
    let page = page_size(super::shared());
    proof { crate::meta::lemma_pow2_bits(page); }
    let Some(rounded) = size.checked_add(page - 1) else { return enomem(Tracked(errno)) };
    let p = alloc_aligned(page, rounded & !(page - 1), Tracked(blk), Tracked(errno));
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
///
/// `left` is [`purge_all`](crate::purge::purge_all)'s record after init
/// (nothing purged before): every arena it visited is covered and clean,
/// and the result is 1 iff it discarded a byte.
pub fn malloc_trim(_pad: usize, Tracked(left): Tracked<&mut Ghost<crate::purge::Left>>) -> (r: c_int)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r == 0 || r == 1,
        (r == 1) == (final(left)@.bytes > 0),
        final(left)@.covers(),
        final(left)@.clean(),
{
    proof {
        *left = Ghost(crate::purge::Left::none());
        crate::purge::Left::lemma_none();
    }
    let Some(shared) = super::shared_if_ready() else { return 0 };
    // Premise (lock order): `malloc_trim`'s caller holds no allocator lock.
    let tracked mut ord = axiom_entry();
    if let Some(h) = heap::existing_heap(heap::shared_ptr(shared)) {
        h.flush_all(shared, Tracked(&mut ord));
    }
    (crate::purge::purge_all(shared, Tracked(&mut *left), Tracked(&mut ord)) > 0) as c_int
}

} // verus!

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see the module docs).
// ---------------------------------------------------------------------------

/// Where `malloc` and `free` start, mod 64, in the shipped library (the
/// pin in `exports`).
///
/// x86_64: both at 0. Without the pin LLVM's 16-byte default let #68's
/// push change (same instructions, fewer of them) move `free` from 32 to
/// 16 mod 64 and `malloc` from 48 to 32, which cost glibc-simple 36% on
/// Zen 5 (c4d). Pinning either entry alone removed it there, so it is
/// the pair's layout, not one offset; both are pinned. The cost is 768
/// bytes of `.text`.
///
/// aarch64: `malloc` at 16, `free` at 28, in both quanta. LLVM aligns
/// functions to 4 here, so before the pin any `.text` growth ahead of the
/// pair moved both: #111 and #114 moved them from 16/28 to 8/20 without
/// touching either function, and that alone cost ref-arm64 (Neoverse V2)
/// xmalloc about 2% (#115; 8/20 was worse, Holm, in r3b-rss-xm4 and
/// xm5). 16/28 is where the quantum16 build had them on ff2c450; in a
/// sweep of seven offsets (r3b-pin-sweep1) no other offset measured
/// better. The quantum8 build (the one Wolfi's Valkey links) had them at
/// 52/52; at 16/28 its xmalloc measured 5.7% better and its Valkey-model
/// rows the same (r3b-pin-rv1).
///
/// Only these two entries are pinned. On aarch64 moving them out of
/// `.text` shifts the rest: `sdallocx`, the `challoc_*_usable` entries,
/// `calloc`, `realloc` and the C++ operators moved by up to 24 bytes mod
/// 64, and none of their rows moved (r3b-pin-rv1: valkey1,
/// valkey1-usable, valkey1-write-pair and larson1 for `new[]`/`delete[]`,
/// in both quanta). `je_malloc` and `je_free` are their own copies in
/// `.text`, not aliases of the pinned bodies, as on x86_64: a program
/// that calls the `je_` names gets the unpinned copies.
///
/// The test `malloc_and_free_entries_are_pinned` checks both against
/// `nm -D` of the shipped library.
#[cfg(target_arch = "x86_64")]
const MALLOC_OFF: usize = 0;
#[cfg(target_arch = "x86_64")]
const FREE_OFF: usize = 0;
#[cfg(target_arch = "aarch64")]
const MALLOC_OFF: usize = 16;
#[cfg(target_arch = "aarch64")]
const FREE_OFF: usize = 28;

#[cfg(not(test))]
mod exports {
    use super::*;
    use core::ffi::c_void;

    // `malloc` and `free` each sit alone in a 64-byte-aligned section (the
    // `global_asm!` below); their `je_` twins stay in `.text`.

    #[unsafe(no_mangle)]
    #[unsafe(link_section = ".text.challoc.malloc")]
    pub extern "C" fn malloc(size: usize) -> *mut c_void {
        super::malloc(size, Tracked::assume_new()) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn je_malloc(size: usize) -> *mut c_void {
        super::malloc(size, Tracked::assume_new()) as *mut c_void
    }

    #[unsafe(no_mangle)]
    #[unsafe(link_section = ".text.challoc.free")]
    pub extern "C" fn free(p: *mut c_void) {
        super::free(p as *mut u8, Tracked::assume_new(), Tracked::assume_new())
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn je_free(p: *mut c_void) {
        super::free(p as *mut u8, Tracked::assume_new(), Tracked::assume_new())
    }

    verus! {

    /// Trusted (the C contract of an out-pointer argument,
    /// `vspec::cbuf`): the token of this call for the C caller's
    /// out-pointer `p`, the permission to write the `T` it names: none for
    /// a null `p`, and `p` is not null when the contract does not allow
    /// null (`nullable` false: `posix_memalign`'s `memptr`). Linear, and
    /// private to this module, whose functions are the C entry points: an
    /// export mints its call's token once, for its own parameter, and
    /// hands it to the Rust entry point. No other verified code can mint
    /// one; a second permission for `p` would prove `false`, and so would
    /// a null `p` with `nullable` false (a C caller that passes one breaks
    /// the contract).
    axiom fn axiom_c_out<T>(p: *mut T, nullable: bool) -> (tracked o: COut<T>)
        ensures
            o.of(p),
            !nullable ==> p@.addr != 0,
    ;

    /// The token of a `posix_memalign` call for its `memptr`
    /// ([`axiom_c_out`], not nullable), as a `Tracked` value for the
    /// export's body. Called once by each of the two exports.
    // The parameter is read by the ghost code only.
    #[allow(unused_variables)]
    fn memptr_out(out: *mut *mut u8) -> (o: Tracked<COut<*mut u8>>)
        ensures
            o@.of(out),
            out@.addr != 0,
    {
        let tracked o = axiom_c_out(out, false);
        Tracked(o)
    }

    } // verus!

    // The alignment of the sections `malloc` and `free` are placed in: a
    // cache line, with `.space` padding (zeros, never executed) ahead of
    // the entry to put it at `MALLOC_OFF` / `FREE_OFF` mod 64. Stable Rust
    // has no per-function alignment (`#[align]` and
    // `-C min-function-alignment` are unstable), but a section's alignment
    // is the largest `.p2align` in it and a function alone in its section
    // starts right after what the section already holds, so this pins
    // each entry whatever the code before it, and the linker places both
    // sections together, so the pair's placement no longer moves with
    // unrelated code.
    core::arch::global_asm!(
        ".pushsection .text.challoc.malloc,\"ax\",@progbits",
        ".p2align 6",
        ".space {m}",
        ".popsection",
        ".pushsection .text.challoc.free,\"ax\",@progbits",
        ".p2align 6",
        ".space {f}",
        ".popsection",
        m = const MALLOC_OFF,
        f = const FREE_OFF,
    );

    export_pair! {
        fn calloc / je_calloc(n: usize, m: usize) -> *mut c_void {
            super::calloc(n, m, Tracked::assume_new()) as *mut c_void
        }
        fn realloc / je_realloc(p: *mut c_void, size: usize) -> *mut c_void {
            super::realloc(p as *mut u8, size, Tracked::assume_new(), Tracked::assume_new()) as *mut c_void
        }
        fn aligned_alloc / je_aligned_alloc(align: usize, size: usize) -> *mut c_void {
            super::aligned_alloc(align, size, Tracked::assume_new()) as *mut c_void
        }
        fn memalign / je_memalign(align: usize, size: usize) -> *mut c_void {
            super::aligned_alloc(align, size, Tracked::assume_new()) as *mut c_void
        }
        fn posix_memalign / je_posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int {
            let oout = memptr_out(out as *mut *mut u8);
            super::posix_memalign(out as *mut *mut u8, align, size, oout, Tracked::assume_new())
        }
        fn valloc / je_valloc(size: usize) -> *mut c_void {
            super::valloc(size, Tracked::assume_new()) as *mut c_void
        }
        fn pvalloc / je_pvalloc(size: usize) -> *mut c_void {
            super::pvalloc(size, Tracked::assume_new()) as *mut c_void
        }
        fn malloc_usable_size / je_malloc_usable_size(p: *mut c_void) -> usize {
            super::malloc_usable_size(p as *mut u8)
        }
    }

    // The glibc-only names, which jemalloc does not prefix.

    #[unsafe(no_mangle)]
    pub extern "C" fn reallocarray(p: *mut c_void, n: usize, m: usize) -> *mut c_void {
        super::reallocarray(
            p as *mut u8,
            n,
            m,
            Tracked::assume_new(),
            Tracked::assume_new(),
        ) as *mut c_void
    }

    /// C23 `free_sized`: the size is checked against the class and stands
    /// in for the slot bit test (design 6.3).
    #[unsafe(no_mangle)]
    pub extern "C" fn free_sized(p: *mut c_void, size: usize) {
        super::free_sized(
            p as *mut u8,
            size,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
    }

    /// C23 `free_aligned_sized`: as `free_sized`, with the alignment.
    #[unsafe(no_mangle)]
    pub extern "C" fn free_aligned_sized(p: *mut c_void, align: usize, size: usize) {
        super::free_aligned_sized(
            p as *mut u8,
            align,
            size,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn malloc_trim(pad: usize) -> c_int {
        super::malloc_trim(pad, Tracked::assume_new())
    }

    // challoc's size-returning entry points (design 13.7): challoc's own
    // names, no `je_` twins, declared in <challoc/challoc.h>.

    #[unsafe(no_mangle)]
    pub extern "C" fn challoc_malloc_usable(size: usize, usable: *mut usize) -> *mut c_void {
        super::malloc_usable(size, usable, Tracked::assume_new(), Tracked::assume_new())
            as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn challoc_calloc_usable(n: usize, m: usize, usable: *mut usize) -> *mut c_void {
        super::calloc_usable(n, m, usable, Tracked::assume_new(), Tracked::assume_new())
            as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn challoc_realloc_usable(
        p: *mut c_void,
        size: usize,
        usable: *mut usize,
    ) -> *mut c_void {
        super::realloc_usable(
            p as *mut u8,
            size,
            usable,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        ) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn challoc_free_usable(p: *mut c_void) -> usize {
        super::free_usable(p as *mut u8, Tracked::assume_new(), Tracked::assume_new())
    }
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
            let p = malloc(n, vstd::prelude::Tracked::assume_new());
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
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
        assert_eq!(malloc_usable_size(ptr::null_mut()), 0);
        free(
            ptr::null_mut(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }

    #[test]
    fn malloc_zero_returns_distinct_pointers() {
        let a = malloc(0, vstd::prelude::Tracked::assume_new());
        let b = malloc(0, vstd::prelude::Tracked::assume_new());
        assert!(!a.is_null() && !b.is_null() && a != b);
        free(
            a,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        free(
            b,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }

    #[test]
    fn calloc_zeroes_reused_slots_and_huge_runs() {
        for &n in &[24usize, 1000, 5000, MEDIUM_MAX + 1, 3 << 20] {
            // Dirty a block, free it, then calloc the same size: the reused
            // memory must read as zero.
            let p = malloc(n, vstd::prelude::Tracked::assume_new());
            fill(p, n, 0xAB);
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            let q = calloc(1, n, vstd::prelude::Tracked::assume_new());
            assert!(!q.is_null());
            for i in 0..n {
                assert_eq!(unsafe { *q.add(i) }, 0, "calloc({n}) byte {i} not zero");
            }
            free(
                q,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
        set_errno(0, vstd::prelude::Tracked::assume_new());
        assert!(calloc(usize::MAX, 2, vstd::prelude::Tracked::assume_new()).is_null());
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
        let mut p = malloc(sizes[0], vstd::prelude::Tracked::assume_new());
        fill(p, sizes[0], 1);
        let mut prev = sizes[0];
        for &n in &sizes[1..] {
            p = realloc(
                p,
                n,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            assert!(!p.is_null(), "realloc to {n}");
            check(p, prev.min(n), 1);
            fill(p, n, 1);
            prev = n;
        }
        // Same class: same pointer.
        let q = realloc(
            p,
            prev,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert_eq!(q, p);
        let q = realloc(
            p,
            prev + 1,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert_eq!(q, p, "10 -> 11 stays in the 16-byte class");
        assert!(
            realloc(
                p,
                0,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .is_null(),
            "realloc(p, 0) frees and returns null"
        );

        // Huge: same number of pages keeps the pointer.
        let page = super::super::shared().regions.page_size();
        let h = realloc(
            ptr::null_mut(),
            MEDIUM_MAX + 1,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert_eq!(
            realloc(
                h,
                MEDIUM_MAX + page,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            h
        );
        let h2 = realloc(
            h,
            MEDIUM_MAX + page + 1,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert_ne!(h2, h);
        free(
            h2,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );

        assert!(
            reallocarray(
                ptr::null_mut(),
                usize::MAX,
                2,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .is_null()
        );
        assert_eq!(errno(), ENOMEM);
        let r = reallocarray(
            ptr::null_mut(),
            8,
            8,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(malloc_usable_size(r) >= 64);
        free(
            r,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
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
                let p = aligned_alloc(align, size, vstd::prelude::Tracked::assume_new());
                assert!(!p.is_null(), "aligned_alloc({align}, {size})");
                assert_eq!(
                    p as usize % align,
                    0,
                    "aligned_alloc({align}, {size}) = {p:p}"
                );
                assert!(malloc_usable_size(p) >= size);
                fill(p, size, 9);
                check(p, size, 9);
                free(
                    p,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                );

                let mut out = ptr::null_mut();
                assert_eq!(
                    posix_memalign(
                        &mut out,
                        align,
                        size,
                        vstd::prelude::Tracked::assume_new(),
                        vstd::prelude::Tracked::assume_new()
                    ),
                    0
                );
                assert_eq!(out as usize % align, 0);
                free(
                    out,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                );
            }
            align <<= 1;
        }
    }

    #[test]
    fn aligned_allocation_picks_the_smallest_fitting_class() {
        // 48-byte request at 32-byte alignment: class 48 slots are only
        // 16-aligned, so class 64 is used.
        let p = aligned_alloc(32, 48, vstd::prelude::Tracked::assume_new());
        assert_eq!(malloc_usable_size(p), 64);
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        // Alignment already implied by the quantum: plain class.
        let p = aligned_alloc(QUANTUM, 48, vstd::prelude::Tracked::assume_new());
        assert_eq!(malloc_usable_size(p), class_size_of(48).unwrap());
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        // 4 KiB alignment for 5 KiB: the 6 KiB class (5120 + 1024) is not a
        // multiple of 4 KiB; 8 KiB is.
        let p = aligned_alloc(4096, 5 * 1024, vstd::prelude::Tracked::assume_new());
        assert_eq!(malloc_usable_size(p), 8192);
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }

    #[test]
    fn bad_alignment_is_einval_and_exhaustion_is_enomem() {
        set_errno(0, vstd::prelude::Tracked::assume_new());
        assert!(aligned_alloc(0, 16, vstd::prelude::Tracked::assume_new()).is_null());
        assert_eq!(errno(), EINVAL);
        set_errno(0, vstd::prelude::Tracked::assume_new());
        assert!(aligned_alloc(24, 16, vstd::prelude::Tracked::assume_new()).is_null());
        assert_eq!(errno(), EINVAL);

        let mut out = ptr::null_mut::<u8>();
        set_errno(0, vstd::prelude::Tracked::assume_new());
        assert_eq!(
            posix_memalign(
                &mut out,
                3,
                16,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            EINVAL
        );
        assert_eq!(
            posix_memalign(
                &mut out,
                4,
                16,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            EINVAL,
            "below sizeof(void *)"
        );
        assert_eq!(errno(), 0, "posix_memalign leaves errno alone");
        assert!(out.is_null());

        set_errno(0, vstd::prelude::Tracked::assume_new());
        assert!(malloc(usize::MAX / 2, vstd::prelude::Tracked::assume_new()).is_null());
        assert_eq!(errno(), ENOMEM);
        set_errno(0, vstd::prelude::Tracked::assume_new());
        assert!(
            realloc(
                ptr::null_mut(),
                usize::MAX / 2,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .is_null()
        );
        assert_eq!(errno(), ENOMEM);
        set_errno(0, vstd::prelude::Tracked::assume_new());
        assert_eq!(
            posix_memalign(
                &mut out,
                16,
                usize::MAX / 2,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            ENOMEM
        );
        set_errno(0, vstd::prelude::Tracked::assume_new());
        assert!(pvalloc(usize::MAX, vstd::prelude::Tracked::assume_new()).is_null());
        assert_eq!(errno(), ENOMEM);
    }

    #[test]
    fn valloc_and_pvalloc_are_page_aligned() {
        let page = super::super::shared().regions.page_size();
        let p = valloc(10, vstd::prelude::Tracked::assume_new());
        assert_eq!(p as usize % page, 0);
        assert!(malloc_usable_size(p) >= 10);
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        let p = pvalloc(page + 1, vstd::prelude::Tracked::assume_new());
        assert_eq!(p as usize % page, 0);
        assert!(malloc_usable_size(p) >= 2 * page);
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        let p = pvalloc(0, vstd::prelude::Tracked::assume_new());
        assert!(malloc_usable_size(p) >= page);
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
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
            let p = malloc_usable(
                n,
                &mut u,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
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
            assert_eq!(
                free_usable(
                    p,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new()
                ),
                u,
                "free_usable({n})"
            );

            // calloc_usable: the whole usable size reads as zero, even on a
            // slot or run that was dirtied and freed a moment ago.
            let d = malloc(n.max(1), vstd::prelude::Tracked::assume_new());
            fill(d, malloc_usable_size(d), 0xAB);
            free(
                d,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            let mut u = usize::MAX;
            let q = calloc_usable(
                1,
                n,
                &mut u,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
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
            let r = realloc_usable(
                q,
                n.max(1),
                &mut u2,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            assert_eq!(
                r, q,
                "realloc_usable({n}) to the same size keeps the pointer"
            );
            assert_eq!(u2, u);
            let grown = 2 * n + 4097;
            let mut u3 = usize::MAX;
            let g = realloc_usable(
                r,
                grown,
                &mut u3,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            assert!(!g.is_null());
            assert_eq!(u3, malloc_usable_size(g));
            assert!(u3 >= grown);
            check(g, u.min(grown), 4);
            assert_eq!(
                free_usable(
                    g,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new()
                ),
                u3
            );
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
        assert_eq!(
            free_usable(
                ptr::null_mut(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            0
        );
        let p = malloc_usable(
            100,
            ptr::null_mut(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!p.is_null());
        let q = calloc_usable(
            2,
            50,
            ptr::null_mut(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!q.is_null() && is_zero(q, 100));
        let r = realloc_usable(
            q,
            5000,
            ptr::null_mut(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!r.is_null());
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        free(
            r,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );

        let mut u = usize::MAX;
        let p = realloc_usable(
            ptr::null_mut(),
            300,
            &mut u,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!p.is_null());
        assert_eq!(u, malloc_usable_size(p));
        fill(p, 300, 8);
        set_errno(0, vstd::prelude::Tracked::assume_new());
        let mut u2 = usize::MAX;
        assert!(
            realloc_usable(
                p,
                usize::MAX / 2,
                &mut u2,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .is_null()
        );
        assert_eq!(errno(), ENOMEM);
        assert_eq!(u2, 0);
        check(p, 300, 8);
        let mut u3 = usize::MAX;
        assert!(
            realloc_usable(
                p,
                0,
                &mut u3,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .is_null(),
            "realloc_usable(p, 0) frees and returns null"
        );
        assert_eq!(u3, 0);

        set_errno(0, vstd::prelude::Tracked::assume_new());
        let mut u = usize::MAX;
        assert!(
            malloc_usable(
                usize::MAX / 2,
                &mut u,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .is_null()
        );
        assert_eq!((errno(), u), (ENOMEM, 0));
        set_errno(0, vstd::prelude::Tracked::assume_new());
        let mut u = usize::MAX;
        assert!(
            calloc_usable(
                usize::MAX,
                2,
                &mut u,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .is_null(),
            "overflow"
        );
        assert_eq!((errno(), u), (ENOMEM, 0));
        set_errno(0, vstd::prelude::Tracked::assume_new());
        let mut u = usize::MAX;
        assert!(
            calloc_usable(
                1,
                usize::MAX / 2,
                &mut u,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .is_null(),
            "exhaustion"
        );
        assert_eq!((errno(), u), (ENOMEM, 0));

        // Aligned blocks report their class, not the request.
        let a = aligned_alloc(4096, 5 * 1024, vstd::prelude::Tracked::assume_new());
        assert_eq!(
            free_usable(
                a,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            8192
        );
        // Off the fast path: a full cache and a huge run.
        let many: Vec<*mut u8> = (0..3000)
            .map(|_| malloc(64, vstd::prelude::Tracked::assume_new()))
            .collect();
        for p in many {
            assert_eq!(
                free_usable(
                    p,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new()
                ),
                64
            );
        }
        let h = malloc(3 << 20, vstd::prelude::Tracked::assume_new());
        assert_eq!(
            free_usable(
                h,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            malloc_usable_size_of_huge(3 << 20)
        );
    }

    fn malloc_usable_size_of_huge(n: usize) -> usize {
        super::super::shared().huge.usable_for(n).unwrap()
    }

    fn is_zero(p: *mut u8, n: usize) -> bool {
        (0..n).all(|i| unsafe { *p.add(i) } == 0)
    }

    #[test]
    fn free_preserves_errno() {
        let p = malloc(100, vstd::prelude::Tracked::assume_new());
        let h = malloc(1 << 20, vstd::prelude::Tracked::assume_new());
        set_errno(1234, vstd::prelude::Tracked::assume_new());
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        free(
            h,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        // Overflow a cache so the flush path runs too.
        let many: Vec<*mut u8> = (0..1000)
            .map(|_| malloc(64, vstd::prelude::Tracked::assume_new()))
            .collect();
        set_errno(1234, vstd::prelude::Tracked::assume_new());
        for p in many {
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
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
        // A background thread at a 1 ms decay would purge the run first.
        let _serial = crate::testutil::global_state();
        let purged = (0..8).any(|_| {
            let h = malloc(4 << 20, vstd::prelude::Tracked::assume_new());
            assert!(!h.is_null());
            unsafe { ptr::write_bytes(h, 1, 4 << 20) };
            free(
                h,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            malloc_trim(0, vstd::prelude::Tracked::assume_new()) == 1
        });
        assert!(purged, "malloc_trim never reported the freed run purged");
        let quiet = (0..32).any(|_| malloc_trim(0, vstd::prelude::Tracked::assume_new()) == 0);
        assert!(quiet, "malloc_trim never found nothing to purge");
    }

    #[test]
    fn cross_thread_free_and_thread_exit_with_full_cache() {
        let blocks: Vec<usize> = (0..4096)
            .map(|i| malloc(32 + (i % 8) * 16, vstd::prelude::Tracked::assume_new()) as usize)
            .collect();
        let handles: Vec<_> = blocks
            .chunks(512)
            .map(|chunk| {
                let chunk: Vec<usize> = chunk.to_vec();
                std::thread::spawn(move || {
                    for &p in &chunk {
                        free(
                            p as *mut u8,
                            vstd::prelude::Tracked::assume_new(),
                            vstd::prelude::Tracked::assume_new(),
                        );
                    }
                    // Leave with plenty in this thread's caches.
                    let mine: Vec<*mut u8> = (0..300)
                        .map(|_| malloc(48, vstd::prelude::Tracked::assume_new()))
                        .collect();
                    for p in mine {
                        free(
                            p,
                            vstd::prelude::Tracked::assume_new(),
                            vstd::prelude::Tracked::assume_new(),
                        );
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

    /// The pin of `malloc` and `free` (the `global_asm!`s of `exports`)
    /// holds in the shipped library: in `nm -D` each entry sits at
    /// `MALLOC_OFF` / `FREE_OFF` mod 64, 0/0 on x86_64 (#70) and 16/28 on
    /// aarch64 (#115). The pin depends on the release profile (fat LTO,
    /// one codegen unit) keeping each in its own section; without it
    /// `free` moved to 16 mod 64 and cost glibc-simple 36% on Zen 5
    /// (c4d), and on Neoverse V2 a move from 16/28 to 8/20 cost xmalloc
    /// about 2%.
    #[test]
    fn malloc_and_free_entries_are_pinned() {
        let so = crate::api::c_programs::release_so();
        let out = Command::new("nm")
            .args(["-D", "--defined-only", &so])
            .output()
            .expect("nm");
        assert!(out.status.success(), "nm -D {so} failed");
        let syms = String::from_utf8(out.stdout).unwrap();
        for (name, want) in [("malloc", super::MALLOC_OFF), ("free", super::FREE_OFF)] {
            let addr = syms
                .lines()
                .find_map(|l| {
                    let mut f = l.split_whitespace();
                    let (a, _, n) = (f.next()?, f.next()?, f.next()?);
                    (n.split('@').next() == Some(name))
                        .then(|| usize::from_str_radix(a, 16).ok())
                        .flatten()
                })
                .unwrap_or_else(|| panic!("{name} not in nm -D of {so}:\n{syms}"));
            std::eprintln!("entry {name} at {addr:#x}: {} mod 64", addr % 64);
            assert_eq!(
                addr % 64,
                want,
                "{name} at {addr:#x} is not at {want} mod 64 in {so}: the entry pin \
                 (src/api/libc.rs `exports`) no longer holds"
            );
        }
    }
}
