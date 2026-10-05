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
use crate::layout::{CLASSES, Kind, MEDIUM_MAX, NCLASSES, SMALL_MAX, class_of, class_of_small};
use crate::sys::Errno;

// The codes as `c_int`, for `errno` and `posix_memalign`'s return; the
// numbers live in `sys::errno`.
const ENOMEM: c_int = Errno::ENOMEM.0;
const EINVAL: c_int = Errno::EINVAL.0;

#[inline(always)]
fn set_errno(e: c_int) {
    // SAFETY: `__errno_location` returns the calling thread's `errno`.
    unsafe { *::libc::__errno_location() = e };
}

/// Null with `errno = ENOMEM`.
#[cold]
#[inline(never)]
fn enomem() -> *mut u8 {
    set_errno(ENOMEM);
    ptr::null_mut()
}

/// Null with `errno = EINVAL`.
#[cold]
#[inline(never)]
fn einval() -> *mut u8 {
    set_errno(EINVAL);
    ptr::null_mut()
}

/// Huge allocation on the calling thread, counted in its heap when it has
/// one. `align` is a power of two; `1` means none.
#[cold]
#[inline(never)]
fn alloc_huge(shared: &Shared, size: usize, align: usize) -> (*mut u8, bool) {
    match heap::heap_for_thread(shared) {
        Some(h) => h.alloc_huge(shared, size, align),
        None => shared.alloc_huge(size, align, 0),
    }
}

/// `malloc`.
#[inline(always)]
pub fn malloc(size: usize) -> *mut u8 {
    // The fast path is a cache pop with no calls in it; everything else
    // (init, heap creation, refill, huge, ENOMEM) is one tail call away so
    // the exported `malloc` needs no stack frame. A size above the small
    // table is one tail call too ([`malloc_medium`]): with the class taken
    // only from the table here, its stride-scaled entry addresses the
    // cache header as it is (`class_of_small`).
    if size > SMALL_MAX {
        return malloc_medium(size);
    }
    malloc_class(size, class_of_small(size))
}

/// `malloc` of a size above the small table: the same pop, behind the
/// computed class.
#[inline(never)]
fn malloc_medium(size: usize) -> *mut u8 {
    let Some(c) = class_of(size) else {
        return malloc_slow(size);
    };
    malloc_class(size, c)
}

/// `malloc`'s pop for a known class `c` of `size`.
#[inline(always)]
fn malloc_class(size: usize, c: usize) -> *mut u8 {
    let shared = super::shared_ptr();
    let Some(h) = heap::existing_heap(shared) else {
        return malloc_slow(size);
    };
    // SAFETY: this thread's heap exists, so this thread observed `READY`
    // (the byte was read once inside `existing_heap`; this is the same
    // load, merged by the compiler).
    let flags = unsafe { heap::fast_flags(shared) };
    match h.alloc_fast(c, flags) {
        Some(p) => p,
        // The class, not the size: the declined pop (an empty cache or
        // the scheduled pop) goes straight to the slow path of its class,
        // `heap::alloc_declined`, not through a second classification and
        // a second fast pop.
        None => malloc_slow_class(c),
    }
}

/// `malloc_slow` for a size whose class is known and whose fast pop has
/// just declined: the refill or the scheduled pop through
/// `heap::alloc_declined`, null with `errno = ENOMEM` on failure.
#[cold]
#[inline(never)]
fn malloc_slow_class(class: usize) -> *mut u8 {
    let p = heap::alloc_declined(super::shared(), class);
    if p.is_null() { enomem() } else { p }
}

/// The cache pop alone: `None` whenever anything is off the fast path (no
/// heap yet, empty cache, huge size). Shared with the C++ `operator new`
/// family in `cxx.rs`, whose failure handling differs from `malloc`'s.
#[inline(always)]
pub(super) fn malloc_fast(size: usize) -> Option<*mut u8> {
    let c = class_of(size)?;
    let shared = super::shared_ptr();
    let h = heap::existing_heap(shared)?;
    // SAFETY: this thread's heap exists, so this thread observed `READY`
    // (the byte was read once inside `existing_heap`; this is the same
    // load, merged by the compiler).
    let flags = unsafe { heap::fast_flags(shared) };
    h.alloc_fast(c, flags)
}

/// Everything `malloc` does off its fast path. Null with `errno = ENOMEM`
/// on failure.
#[cold]
#[inline(never)]
pub(super) fn malloc_slow(size: usize) -> *mut u8 {
    let shared = super::shared();
    let p = match class_of(size) {
        Some(c) => heap::alloc(shared, c),
        None => alloc_huge(shared, size, 1).0,
    };
    if p.is_null() { enomem() } else { p }
}

/// `calloc`. Skips the `memset` when the slot or run is known to read as
/// zero (fresh or purged memory).
#[inline(always)]
pub fn calloc(n: usize, m: usize) -> *mut u8 {
    let Some(size) = n.checked_mul(m) else {
        return enomem();
    };
    let shared = super::shared();
    let (p, zeroed) = match class_of(size) {
        Some(c) => heap::alloc_zeroed(shared, c),
        None => alloc_huge(shared, size, 1),
    };
    if p.is_null() {
        return enomem();
    }
    if !zeroed {
        // SAFETY: `p` is a fresh allocation of at least `size` bytes.
        unsafe { ptr::write_bytes(p, 0, size) };
    }
    p
}

/// `free`. Null is a no-op; anything that is not a live allocation aborts.
#[inline(always)]
pub fn free(p: *mut u8) {
    // As in `malloc`: the check-and-push has no calls, and every other
    // outcome, including the aborts, is reached through one tail call.
    // Null is one of those outcomes: it is in no class region (the
    // superblock is mapped far above address 0, so `Shared::set_slot`'s
    // class index of null is out of range) and `free_slow` returns for it, so
    // the common case pays no test of its own (jemalloc's `free` fast
    // path leaves null to its slow path the same way).
    if let Some((t, tp)) = heap::tls_heap_slot() {
        // SAFETY: this thread's heap exists, so this thread observed
        // `READY` when it created the heap.
        let shared = unsafe { &*super::shared_base() };
        // SAFETY: the slot value of this thread.
        if unsafe { heap::free_fast_slot(shared, t, tp, p) }.is_some() {
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
pub fn free_sized_hint(p: *mut u8, size: usize, hint: Option<usize>) {
    // A huge size (no hint) has no fast path: straight to the slow one,
    // so the fast path carries a plain class and no test of the hint.
    let Some(c) = hint else {
        return free_sized_unhinted(p, size);
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
        // SAFETY: as in `free`.
        let shared = unsafe { &*sp };
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
fn free_sized_unhinted(p: *mut u8, size: usize) {
    if !p.is_null() {
        free_sized_slow(p, size, None)
    }
}

/// Off the sized fast path: on x86_64 a null pointer (which the fast
/// path's `sized_ok` declines rather than testing for), the hint
/// mismatch abort, then everything the exact `free` does (so a full cache
/// or a huge run still gets the exact check, and a bad pointer the usual
/// message).
#[cold]
#[inline(never)]
fn free_sized_slow(p: *mut u8, size: usize, hint: Option<usize>) {
    #[cfg(target_arch = "x86_64")]
    if p.is_null() {
        return;
    }
    match super::shared_if_ready() {
        Some(shared) => {
            shared.check_size_hint(p, size, hint);
            heap::free(shared, p)
        }
        None => free_before_init(p),
    }
}

/// C23 `free_sized(p, size)`: `size` must be the size requested from
/// `malloc`/`calloc`/`realloc` (or any size in the same class, or for a
/// huge run any size whose page count is the run's).
#[inline(always)]
pub fn free_sized(p: *mut u8, size: usize) {
    // As in `malloc`: a size above the small table is a tail call, so the
    // class comes only from the table here and the stride-scaled entry
    // addresses the class line and the cache header as it is. That keeps
    // the sized path within its registers, with no spill (on Genoa a
    // `push` and a `pop` here cost `valkey1` 8%, #46).
    if size > SMALL_MAX {
        return free_sized_medium(p, size);
    }
    free_sized_hint(p, size, Some(class_of_small(size)))
}

/// `free_sized` of a size above the small table.
#[inline(never)]
fn free_sized_medium(p: *mut u8, size: usize) {
    free_sized_hint(p, size, class_of(size))
}

/// C23 `free_aligned_sized(p, align, size)`: the arguments given to
/// `aligned_alloc`. An alignment that is not a power of two matches no
/// class and aborts on a slab pointer.
pub fn free_aligned_sized(p: *mut u8, align: usize, size: usize) {
    let hint = if align.is_power_of_two() {
        class_for(size, align)
    } else {
        None
    };
    free_sized_hint(p, size, hint)
}

/// Everything `free` does off its fast path: heap creation, a full cache,
/// huge runs, and the aborts for invalid pointers (the checks are repeated
/// from scratch here, so a fast-path `false` for a bad pointer ends in the
/// same message it always did).
#[cold]
#[inline(never)]
fn free_slow(p: *mut u8) {
    if p.is_null() {
        return;
    }
    match super::shared_if_ready() {
        Some(shared) => heap::free(shared, p),
        None => free_before_init(p),
    }
}

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
/// the class's.
#[inline(always)]
pub fn usable_size(shared: &Shared, p: *mut u8) -> usize {
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            shared
                .table(class)
                .check_live(slab as usize, slot as usize, p as usize);
            CLASSES[class].size()
        }
        Kind::Huge { page } => shared.huge.usable_size(page),
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
pub fn malloc_usable_size(p: *mut u8) -> usize {
    if p.is_null() {
        return 0;
    }
    match super::shared_if_ready() {
        Some(shared) => usable_size(shared, p),
        None => free_before_init(p),
    }
}

/// The reallocation decision shared by `realloc` and `rallocx`: `None`
/// when the live allocation `p` already serves `size` bytes at `align`
/// (a power of two; 1 or the quantum means the natural alignment) and
/// keeps its pointer, that is when the new size maps to `p`'s class
/// (whose slots satisfy the alignment by construction) or to a huge run
/// of the same number of pages at a start that satisfies it; otherwise
/// `Some(old)`, the usable bytes of `p`, the most a move has to copy.
/// Aborts, naming `what`, unless `p` is a live slab slot or huge run
/// start.
pub(super) fn old_size_or_keep(
    shared: &Shared,
    what: &str,
    p: *mut u8,
    size: usize,
    align: usize,
) -> Option<usize> {
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            shared
                .table(class)
                .check_live(slab as usize, slot as usize, p as usize);
            if class_for(size, align) == Some(class) {
                return None;
            }
            Some(CLASSES[class].size())
        }
        Kind::Huge { page } => {
            let old = shared.huge.usable_size(page);
            if class_for(size, align).is_none()
                && shared.huge.usable_for(size) == Some(old)
                && (p as usize) % align == 0
            {
                return None;
            }
            Some(old)
        }
        Kind::Invalid => crate::sys::abort_fmt(format_args!(
            "{what} of {:#x}: not an allocation",
            p as usize
        )),
    }
}

/// `realloc`. The same pointer comes back when the new size maps to the
/// same class or the same number of huge pages (`old_size_or_keep`);
/// otherwise allocate, copy `min(old usable, new size)`, free. On failure
/// the old block is intact.
pub fn realloc(p: *mut u8, size: usize) -> *mut u8 {
    if p.is_null() {
        return malloc(size);
    }
    if size == 0 {
        free(p);
        return ptr::null_mut();
    }
    let Some(shared) = super::shared_if_ready() else {
        free_before_init(p)
    };
    let Some(old) = old_size_or_keep(shared, "realloc", p, size, 1) else {
        return p;
    };
    let q = malloc(size);
    if q.is_null() {
        return q;
    }
    // SAFETY: `p` has `old` readable bytes, `q` at least `size` writable
    // ones, and they are distinct allocations.
    unsafe { ptr::copy_nonoverlapping(p, q, old.min(size)) };
    heap::free(shared, p);
    q
}

/// `reallocarray`: `realloc(p, n * m)` with overflow reported as `ENOMEM`.
pub fn reallocarray(p: *mut u8, n: usize, m: usize) -> *mut u8 {
    match n.checked_mul(m) {
        Some(size) => realloc(p, size),
        None => enomem(),
    }
}

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

/// Store `n` through `usable` unless it is null (`usable` is optional for
/// the three allocating entry points; a null makes them plain
/// `malloc`/`calloc`/`realloc`).
#[inline(always)]
fn store_usable(usable: *mut usize, n: usize) {
    if !usable.is_null() {
        // SAFETY: the caller passed a writable `size_t` or null.
        unsafe { *usable = n };
    }
}

/// `CLASSES[class].size()` for a class a fast path produced: below
/// `NCLASSES` by construction (`class_of`, `checked_slot`), so no bounds
/// check and no panic call sits in the inlined body (design 6.3).
#[inline(always)]
fn class_size(class: usize) -> usize {
    debug_assert!(class < NCLASSES);
    // SAFETY: as above.
    unsafe { CLASSES.get_unchecked(class) }.size()
}

/// `challoc_malloc_usable(size, usable)`: `malloc(size)`, and the block's
/// usable size, what `malloc_usable_size` would return for it, stored
/// through `usable` (left alone when `usable` is null; 0 on a null return).
/// The same pop as `malloc` with the class size added from the class the
/// pop used; everything else is one tail call away, as in `malloc`.
#[inline(always)]
pub fn malloc_usable(size: usize, usable: *mut usize) -> *mut u8 {
    let Some(c) = class_of(size) else {
        return malloc_usable_huge(size, usable);
    };
    let shared = super::shared_ptr();
    let Some(h) = heap::existing_heap(shared) else {
        return malloc_usable_class(c, usable);
    };
    // SAFETY: as in `malloc`.
    let flags = unsafe { heap::fast_flags(shared) };
    match h.alloc_fast(c, flags) {
        Some(p) => {
            store_usable(usable, class_size(c));
            p
        }
        None => malloc_usable_class(c, usable),
    }
}

/// `malloc_usable` off its fast path for a slab class: the refill, the
/// scheduled pop and heap creation (`heap::alloc_declined`, then
/// `Heap::alloc_slow`, dispatch all three),
/// null with `errno = ENOMEM` on failure.
#[cold]
#[inline(never)]
fn malloc_usable_class(class: usize, usable: *mut usize) -> *mut u8 {
    let p = heap::alloc_declined(super::shared(), class);
    if p.is_null() {
        store_usable(usable, 0);
        return enomem();
    }
    store_usable(usable, class_size(class));
    p
}

/// `malloc_usable` of a huge size: the run's usable bytes come from the
/// huge table, the same read `malloc_usable_size` does.
#[cold]
#[inline(never)]
fn malloc_usable_huge(size: usize, usable: *mut usize) -> *mut u8 {
    let shared = super::shared();
    let (p, _) = alloc_huge(shared, size, 1);
    if p.is_null() {
        store_usable(usable, 0);
        return enomem();
    }
    store_usable(usable, usable_size(shared, p));
    p
}

/// `challoc_calloc_usable(n, m, usable)`: `calloc(n, m)` with the usable
/// size stored as in [`malloc_usable`]. The whole usable size reads as
/// zero, not only the `n * m` bytes asked for: the caller is told it owns
/// the usable bytes (Valkey's `zcalloc_usable` hands them on), and that is
/// what jemalloc's `calloc` zeroes.
pub fn calloc_usable(n: usize, m: usize, usable: *mut usize) -> *mut u8 {
    let Some(size) = n.checked_mul(m) else {
        store_usable(usable, 0);
        return enomem();
    };
    let shared = super::shared();
    let (p, zeroed, u) = match class_of(size) {
        Some(c) => {
            let (p, zeroed) = heap::alloc_zeroed(shared, c);
            (p, zeroed, class_size(c))
        }
        None => {
            let (p, zeroed) = alloc_huge(shared, size, 1);
            (
                p,
                zeroed,
                if p.is_null() {
                    0
                } else {
                    usable_size(shared, p)
                },
            )
        }
    };
    if p.is_null() {
        store_usable(usable, 0);
        return enomem();
    }
    if !zeroed {
        // SAFETY: `p` is a fresh allocation of `u` usable bytes.
        unsafe { ptr::write_bytes(p, 0, u) };
    }
    store_usable(usable, u);
    p
}

/// `challoc_realloc_usable(p, size, usable)`: `realloc(p, size)` with the
/// usable size of the returned block stored as in [`malloc_usable`] (0
/// when null comes back, `realloc(p, 0)` included). The block kept in
/// place reports its class size from the classification `realloc` does
/// anyway; a moved block reports what the `malloc_usable` that made it
/// reported.
pub fn realloc_usable(p: *mut u8, size: usize, usable: *mut usize) -> *mut u8 {
    if p.is_null() {
        return malloc_usable(size, usable);
    }
    if size == 0 {
        free(p);
        store_usable(usable, 0);
        return ptr::null_mut();
    }
    let Some(shared) = super::shared_if_ready() else {
        free_before_init(p)
    };
    let (keep, old) = realloc_old(shared, p, size);
    if keep {
        store_usable(usable, old);
        return p;
    }
    let q = malloc_usable(size, usable);
    if q.is_null() {
        return q;
    }
    // SAFETY: `p` has `old` readable bytes, `q` at least `size` writable
    // ones, and they are distinct allocations.
    unsafe { ptr::copy_nonoverlapping(p, q, old.min(size)) };
    heap::free(shared, p);
    q
}

/// [`old_size_or_keep`]'s decision for `realloc` (alignment 1) with the
/// old usable size reported in both outcomes: `(keep, usable)`, `keep`
/// when `size` maps to `p`'s class or to a huge run of the same page
/// count. The same checks and aborts as `old_size_or_keep`, which stays as
/// it is so `realloc` and `rallocx` are untouched.
fn realloc_old(shared: &Shared, p: *mut u8, size: usize) -> (bool, usize) {
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            shared
                .table(class)
                .check_live(slab as usize, slot as usize, p as usize);
            (class_of(size) == Some(class), CLASSES[class].size())
        }
        Kind::Huge { page } => {
            let old = shared.huge.usable_size(page);
            (
                class_of(size).is_none() && shared.huge.usable_for(size) == Some(old),
                old,
            )
        }
        Kind::Invalid => crate::sys::abort_fmt(format_args!(
            "realloc of {:#x}: not an allocation",
            p as usize
        )),
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
pub fn free_usable(p: *mut u8) -> usize {
    if p.is_null() {
        return 0;
    }
    if let Some((t, tp)) = heap::tls_heap_slot() {
        // SAFETY: as in `free`.
        let shared = unsafe { &*super::shared_base() };
        // SAFETY: as in `free`.
        if let Some(class) = unsafe { heap::free_fast_slot(shared, t, tp, p) } {
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
#[cold]
#[inline(never)]
fn free_usable_slow(p: *mut u8) -> usize {
    let Some(shared) = super::shared_if_ready() else {
        free_before_init(p)
    };
    let usable = match shared.classify_free(p) {
        Ok((class, _)) => CLASSES[class].size(),
        Err(page) => shared.huge.usable_size(page),
    };
    heap::free(shared, p);
    usable
}

/// The class that serves `size` bytes at `align` (a power of two; 1 or
/// the quantum means the natural alignment): the smallest class at or
/// above `size` whose slot size is a multiple of `align`. `None` when the
/// request is a huge run, either because `size` is above `MEDIUM_MAX` or
/// because `align` is. Shared by `aligned_alloc`, `mallocx`, `rallocx`
/// and `nallocx`.
#[inline]
pub(super) fn class_for(size: usize, align: usize) -> Option<usize> {
    let c = class_of(size)?;
    if align <= QUANTUM {
        return Some(c);
    }
    if align > MEDIUM_MAX {
        return None;
    }
    // The power-of-two class at or above `max(size, align)` qualifies and
    // is at most `MEDIUM_MAX`, so this stops inside the table; `get` (not
    // an index) so that no bounds-check panic call sits in the functions
    // this is inlined into (`sdallocx`: a call in the body costs the fast
    // path a frame it otherwise needs no part of, design 6.3), and `align`
    // is a power of two so the divisibility test is a mask.
    let mut c = c;
    loop {
        let cls = CLASSES.get(c)?;
        if cls.size() & (align - 1) == 0 {
            return Some(c);
        }
        c += 1;
    }
}

/// Allocate `size` bytes aligned to `align`, a power of two. Null with
/// `errno = ENOMEM` on exhaustion.
pub fn alloc_aligned(align: usize, size: usize) -> *mut u8 {
    debug_assert!(align.is_power_of_two());
    if align <= QUANTUM {
        return malloc(size);
    }
    let shared = super::shared();
    let p = match class_for(size, align) {
        Some(c) => heap::alloc(shared, c),
        None => alloc_huge(shared, size, align).0,
    };
    if p.is_null() { enomem() } else { p }
}

/// `aligned_alloc` and `memalign`: `EINVAL` unless `align` is a power of
/// two. glibc does not require `size` to be a multiple of `align`.
pub fn aligned_alloc(align: usize, size: usize) -> *mut u8 {
    if !align.is_power_of_two() {
        return einval();
    }
    alloc_aligned(align, size)
}

/// `posix_memalign`. Returns 0, `EINVAL` (alignment not a power of two or
/// not a multiple of `sizeof(void *)`) or `ENOMEM`; never touches `errno`
/// on the `EINVAL` path.
pub fn posix_memalign(out: *mut *mut u8, align: usize, size: usize) -> c_int {
    if !align.is_power_of_two() || align % core::mem::size_of::<*mut u8>() != 0 {
        return EINVAL;
    }
    let p = alloc_aligned(align, size);
    if p.is_null() {
        return ENOMEM;
    }
    // SAFETY: the caller passes a valid out-pointer.
    unsafe { *out = p };
    0
}

/// `valloc`: page-aligned.
pub fn valloc(size: usize) -> *mut u8 {
    let page = super::shared().regions.page_size();
    alloc_aligned(page, size)
}

/// `pvalloc`: page-aligned, size rounded up to a whole page.
pub fn pvalloc(size: usize) -> *mut u8 {
    let page = super::shared().regions.page_size();
    let Some(rounded) = size.checked_add(page - 1) else {
        return enomem();
    };
    alloc_aligned(page, rounded & !(page - 1))
}

/// `malloc_trim`: return the calling thread's cached slots to their slabs,
/// then purge every `Empty` slab and huge extent whatever its age (design
/// section 8). Returns 1 if anything was purged, 0 otherwise or before
/// init. `pad` is glibc's top-of-heap allowance and has no meaning here.
pub fn malloc_trim(_pad: usize) -> c_int {
    let Some(shared) = super::shared_if_ready() else {
        return 0;
    };
    if let Some(h) = heap::existing_heap(shared) {
        h.flush_all(shared);
    }
    (crate::purge::purge_all(shared) > 0) as c_int
}

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
        super::malloc(size) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn je_malloc(size: usize) -> *mut c_void {
        super::malloc(size) as *mut c_void
    }

    #[unsafe(no_mangle)]
    #[unsafe(link_section = ".text.challoc.free")]
    pub extern "C" fn free(p: *mut c_void) {
        super::free(p as *mut u8)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn je_free(p: *mut c_void) {
        super::free(p as *mut u8)
    }

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
        // A background thread at a 1 ms decay would purge the run first.
        let _serial = crate::testutil::global_state();
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
