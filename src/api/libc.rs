//! Standard and glibc allocation entry points (design.md section 13.1):
//! `malloc`, `free`, `calloc`, `realloc`, `aligned_alloc`, `posix_memalign`,
//! `memalign`, `valloc`, `pvalloc`, `free_sized`, `free_aligned_sized`,
//! `reallocarray`, `malloc_usable_size`, `malloc_trim`.
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
use crate::layout::{CLASSES, Kind, MEDIUM_MAX, class_of};

const ENOMEM: c_int = 12;
const EINVAL: c_int = 22;

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
    // (init, heap creation, refill, huge, ENOMEM, and the per-CPU mode of
    // design 15) is one tail call away so the exported `malloc` needs no
    // stack frame.
    let Some(c) = class_of(size) else { return malloc_slow(size) };
    let shared = super::shared_ptr();
    let Some(h) = heap::existing_heap(shared) else { return malloc_slow(size) };
    // SAFETY: this thread's heap exists, so this thread observed `READY`
    // (the byte was read once inside `existing_heap`; this is the same
    // load, merged by the compiler).
    let flags = unsafe { heap::fast_flags(shared) };
    if flags & heap::PUSH_PERCPU != 0 {
        return percpu_malloc(h, c, flags);
    }
    match h.alloc_fast(c, flags) {
        Some(p) => p,
        None => malloc_slow(size),
    }
}

/// `malloc` in the per-CPU mode: the running CPU's cache (the critical
/// section inlined here, `Heap::pop_percpu_mode`), then the slow path by
/// class (`class_of(size)` already ran). A tail call from `malloc`, so the
/// thread path keeps its shape; this function has no call of its own on
/// the hit path, so the mode's `malloc` is the entry point's dispatch plus
/// this body.
#[inline(never)]
fn percpu_malloc(h: &mut heap::Heap, class: usize, flags: u8) -> *mut u8 {
    let (p, st) = h.percpu_pop_raw(class, flags);
    if st & crate::rseq::POP_HIT != 0 { p } else { percpu_malloc_miss(h, class, flags, st) }
}

/// Everything `percpu_malloc` does off its hit path (the heap off the
/// mode, the scheduled pop, the refill, an id out of range), one cold tail
/// call away.
#[cold]
#[inline(never)]
fn percpu_malloc_miss(h: &mut heap::Heap, class: usize, flags: u8, st: u32) -> *mut u8 {
    match h.percpu_pop_miss(class, flags, st) {
        Some((p, _)) => p,
        None => malloc_slow_class(class),
    }
}

/// `malloc_slow` for a size whose class is known: the refill (or the
/// scheduled pop) through `heap::alloc`, null with `errno = ENOMEM` on
/// failure.
#[cold]
#[inline(never)]
fn malloc_slow_class(class: usize) -> *mut u8 {
    let p = heap::alloc(super::shared(), class);
    if p.is_null() { enomem() } else { p }
}

/// The cache pop alone: `None` whenever anything is off the fast path (no
/// heap yet, empty cache, huge size). For the C++ `operator new` family in
/// `cxx.rs`, whose failure handling differs from `malloc`'s.
#[inline(always)]
pub(super) fn malloc_fast(size: usize) -> Option<*mut u8> {
    let c = class_of(size)?;
    let shared = super::shared_ptr();
    let h = heap::existing_heap(shared)?;
    // SAFETY: as in `malloc`.
    let flags = unsafe { heap::fast_flags(shared) };
    if flags & heap::PUSH_PERCPU != 0 {
        return h.pop_percpu_mode(c, flags).map(|r| r.0);
    }
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
    let Some(size) = n.checked_mul(m) else { return enomem() };
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
    if p.is_null() {
        return;
    }
    // As in `malloc`: the check-and-push has no calls, and every other
    // outcome, including the aborts, is reached through one tail call.
    if let Some(h) = heap::existing_heap(super::shared_ptr()) {
        // SAFETY: this thread's heap exists, so this thread observed
        // `READY` when it created the heap.
        let shared = unsafe { super::shared_unchecked() };
        let flags = shared.push_flags();
        if flags & heap::PUSH_PERCPU != 0 {
            return percpu_free(shared, h, p, flags);
        }
        if heap::free_fast(shared, h, p, flags) {
            return;
        }
    }
    free_slow(p)
}

/// `free` in the per-CPU mode (a tail call from `free`, see `malloc`):
/// the check and the section inline, every other outcome one cold tail
/// call.
#[inline(never)]
fn percpu_free(shared: &Shared, h: &mut heap::Heap, p: *mut u8, flags: u8) {
    let st = heap::percpu_push_raw(shared, h, p, flags, None, false);
    if st != crate::rseq::PUSH_PUSHED {
        percpu_free_miss(shared, h, p, flags, st)
    }
}

#[cold]
#[inline(never)]
fn percpu_free_miss(shared: &Shared, h: &mut heap::Heap, p: *mut u8, flags: u8, st: u32) {
    if !heap::percpu_push_miss(shared, h, p, flags, None, false, st) {
        free_slow(p)
    }
}

/// Sized free on the calling thread's heap: `hint` is the class the size
/// (and alignment) name, `class_for(size, align)`, or `None` for a huge
/// size. On the fast path the hint replaces the slab descriptor's bit test
/// (design 6.3): a slot whose class matches is pushed at once, so an
/// invalid or double free through this path is caught when the cache
/// flushes it, not at the call; a hint that does not match aborts.
#[inline(always)]
pub fn free_sized_hint(p: *mut u8, size: usize, hint: Option<usize>) {
    if p.is_null() {
        return;
    }
    if let Some(h) = heap::existing_heap(super::shared_ptr()) {
        // SAFETY: as in `free`.
        let shared = unsafe { super::shared_unchecked() };
        let flags = shared.push_flags();
        if flags & heap::PUSH_PERCPU != 0 {
            return percpu_free_sized(shared, h, p, size, hint, flags);
        }
        if heap::free_sized_fast(shared, h, p, hint, flags) {
            return;
        }
    }
    free_sized_slow(p, size, hint)
}

/// The sized free in the per-CPU mode (a tail call, see `malloc` and
/// `percpu_free`).
#[inline(never)]
fn percpu_free_sized(shared: &Shared, h: &mut heap::Heap, p: *mut u8, size: usize, hint: Option<usize>, flags: u8) {
    let st = heap::percpu_push_raw(shared, h, p, flags, hint, true);
    if st != crate::rseq::PUSH_PUSHED {
        percpu_free_sized_miss(shared, h, p, size, hint, flags, st)
    }
}

#[cold]
#[inline(never)]
fn percpu_free_sized_miss(shared: &Shared, h: &mut heap::Heap, p: *mut u8, size: usize, hint: Option<usize>, flags: u8, st: u32) {
    if !heap::percpu_push_miss(shared, h, p, flags, hint, true, st) {
        free_sized_slow(p, size, hint)
    }
}

/// Off the sized fast path: the hint mismatch abort, then everything the
/// exact `free` does (so a full cache or a huge run still gets the exact
/// check, and a bad pointer the usual message).
#[cold]
#[inline(never)]
fn free_sized_slow(p: *mut u8, size: usize, hint: Option<usize>) {
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
pub fn free_sized(p: *mut u8, size: usize) {
    free_sized_hint(p, size, class_of(size))
}

/// C23 `free_aligned_sized(p, align, size)`: the arguments given to
/// `aligned_alloc`. An alignment that is not a power of two matches no
/// class and aborts on a slab pointer.
pub fn free_aligned_sized(p: *mut u8, align: usize, size: usize) {
    let hint = if align.is_power_of_two() { class_for(size, align) } else { None };
    free_sized_hint(p, size, hint)
}

/// Everything `free` does off its fast path: heap creation, a full cache,
/// huge runs, and the aborts for invalid pointers (the checks are repeated
/// from scratch here, so a fast-path `false` for a bad pointer ends in the
/// same message it always did).
#[cold]
#[inline(never)]
fn free_slow(p: *mut u8) {
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
            shared.table(class).check_live(slab as usize, slot as usize, p as usize);
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

/// `realloc`. The same pointer comes back when the new size maps to the
/// same class or the same number of huge pages; otherwise allocate, copy
/// `min(old usable, new size)`, free. On failure the old block is intact.
pub fn realloc(p: *mut u8, size: usize) -> *mut u8 {
    if p.is_null() {
        return malloc(size);
    }
    if size == 0 {
        free(p);
        return ptr::null_mut();
    }
    let Some(shared) = super::shared_if_ready() else { free_before_init(p) };
    let old = match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            shared.table(class).check_live(slab as usize, slot as usize, p as usize);
            if class_of(size) == Some(class) {
                return p;
            }
            CLASSES[class].size()
        }
        Kind::Huge { page } => {
            let old = shared.huge.usable_size(page);
            if class_of(size).is_none() && shared.huge.usable_for(size) == Some(old) {
                return p;
            }
            old
        }
        Kind::Invalid => crate::sys::abort_fmt(format_args!(
            "realloc of {:#x}: not an allocation",
            p as usize
        )),
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
    let Some(rounded) = size.checked_add(page - 1) else { return enomem() };
    alloc_aligned(page, rounded & !(page - 1))
}

/// `malloc_trim`: return the calling thread's cached slots to their slabs,
/// then purge every `Empty` slab and huge extent whatever its age (design
/// section 8). Returns 1 if anything was purged, 0 otherwise or before
/// init. `pad` is glibc's top-of-heap allowance and has no meaning here.
pub fn malloc_trim(_pad: usize) -> c_int {
    let Some(shared) = super::shared_if_ready() else { return 0 };
    if let Some(h) = heap::existing_heap(shared) {
        h.flush_all(shared);
    }
    (crate::purge::purge_all(shared) > 0) as c_int
}

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see the module docs).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    use super::*;
    use core::ffi::c_void;

    #[unsafe(no_mangle)]
    pub extern "C" fn malloc(size: usize) -> *mut c_void {
        super::malloc(size) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn free(p: *mut c_void) {
        super::free(p as *mut u8)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn calloc(n: usize, m: usize) -> *mut c_void {
        super::calloc(n, m) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn realloc(p: *mut c_void, size: usize) -> *mut c_void {
        super::realloc(p as *mut u8, size) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn reallocarray(p: *mut c_void, n: usize, m: usize) -> *mut c_void {
        super::reallocarray(p as *mut u8, n, m) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn aligned_alloc(align: usize, size: usize) -> *mut c_void {
        super::aligned_alloc(align, size) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn memalign(align: usize, size: usize) -> *mut c_void {
        super::aligned_alloc(align, size) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int {
        super::posix_memalign(out as *mut *mut u8, align, size)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn valloc(size: usize) -> *mut c_void {
        super::valloc(size) as *mut c_void
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn pvalloc(size: usize) -> *mut c_void {
        super::pvalloc(size) as *mut c_void
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

    #[unsafe(no_mangle)]
    pub extern "C" fn malloc_usable_size(p: *mut c_void) -> usize {
        super::malloc_usable_size(p as *mut u8)
    }

    // `je_` aliases (design 13.2): what a program built against a
    // `--with-jemalloc-prefix=je_` jemalloc, such as Valkey, calls.

    #[unsafe(export_name = "je_malloc")]
    pub extern "C" fn je_malloc(size: usize) -> *mut c_void {
        super::malloc(size) as *mut c_void
    }

    #[unsafe(export_name = "je_free")]
    pub extern "C" fn je_free(p: *mut c_void) {
        super::free(p as *mut u8)
    }

    #[unsafe(export_name = "je_calloc")]
    pub extern "C" fn je_calloc(n: usize, m: usize) -> *mut c_void {
        super::calloc(n, m) as *mut c_void
    }

    #[unsafe(export_name = "je_realloc")]
    pub extern "C" fn je_realloc(p: *mut c_void, size: usize) -> *mut c_void {
        super::realloc(p as *mut u8, size) as *mut c_void
    }

    #[unsafe(export_name = "je_aligned_alloc")]
    pub extern "C" fn je_aligned_alloc(align: usize, size: usize) -> *mut c_void {
        super::aligned_alloc(align, size) as *mut c_void
    }

    #[unsafe(export_name = "je_memalign")]
    pub extern "C" fn je_memalign(align: usize, size: usize) -> *mut c_void {
        super::aligned_alloc(align, size) as *mut c_void
    }

    #[unsafe(export_name = "je_posix_memalign")]
    pub extern "C" fn je_posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int {
        super::posix_memalign(out as *mut *mut u8, align, size)
    }

    #[unsafe(export_name = "je_valloc")]
    pub extern "C" fn je_valloc(size: usize) -> *mut c_void {
        super::valloc(size) as *mut c_void
    }

    #[unsafe(export_name = "je_pvalloc")]
    pub extern "C" fn je_pvalloc(size: usize) -> *mut c_void {
        super::pvalloc(size) as *mut c_void
    }

    #[unsafe(export_name = "je_malloc_usable_size")]
    pub extern "C" fn je_malloc_usable_size(p: *mut c_void) -> usize {
        super::malloc_usable_size(p as *mut u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{MEDIUM_MAX, class_size_of};
    use std::env;
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
        for &n in &[0usize, 1, 7, 16, 17, 100, 1024, 1025, 4096, 65536, MEDIUM_MAX, MEDIUM_MAX + 1, 1 << 20, 10 << 20]
        {
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
        let sizes = [10usize, 100, 1000, 10_000, 100_000, MEDIUM_MAX + 1, 2 << 20, 100_000, 1000, 10];
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
        assert!(realloc(p, 0).is_null(), "realloc(p, 0) frees and returns null");

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
            for &size in &[1usize, align - 1, align, align + 1, 3 * align, MEDIUM_MAX + 1] {
                let p = aligned_alloc(align, size);
                assert!(!p.is_null(), "aligned_alloc({align}, {size})");
                assert_eq!(p as usize % align, 0, "aligned_alloc({align}, {size}) = {p:p}");
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
        assert_eq!(posix_memalign(&mut out, 4, 16), EINVAL, "below sizeof(void *)");
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

    #[test]
    fn cross_thread_free_and_thread_exit_with_full_cache() {
        let blocks: Vec<usize> = (0..4096).map(|i| malloc(32 + (i % 8) * 16) as usize).collect();
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

    // ---- release .so under LD_PRELOAD -------------------------------------

    fn release_so() -> String {
        let target = env::var("CARGO_TARGET_DIR")
            .unwrap_or_else(|_| std::format!("{}/target", env!("CARGO_MANIFEST_DIR")));
        let so = std::format!("{target}/release/libchalloc.so");
        assert!(
            std::path::Path::new(&so).exists(),
            "{so} missing: run `cargo build --release` first (`make test` does)"
        );
        so
    }

    #[test]
    fn shared_object_exports_every_libc_entry_point() {
        let so = release_so();
        let out = Command::new("readelf").args(["--dyn-syms", "-W", &so]).output().expect("readelf");
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
                syms.lines().any(|l| l.contains(" FUNC ") && l.contains(" GLOBAL ") && l.ends_with(&std::format!(" {name}"))),
                "{name} not exported as a global function:\n{syms}"
            );
        }
    }

    /// `tests/c/libc_api.c` under `LD_PRELOAD`, then a few ordinary
    /// programs to show the library survives real startup sequences.
    #[test]
    fn c_program_exercises_the_api_under_ld_preload() {
        let so = release_so();
        let root = env!("CARGO_MANIFEST_DIR");
        let src = std::format!("{root}/tests/c/libc_api.c");
        let out = std::format!("{}/libc_api-{}", env::temp_dir().display(), std::process::id());
        let cc = env::var("CC").unwrap_or_else(|_| "cc".into());
        let quantum = std::format!("-DQUANTUM={QUANTUM}");
        let status = Command::new(&cc)
            .args(["-O2", "-Wall", "-Wextra", "-Werror", "-pthread", &quantum, &src, "-o", &out, "-ldl"])
            .status()
            .unwrap();
        assert!(status.success(), "compiling {src} failed");

        // Without the preload the program must notice it is on glibc.
        let bare = Command::new(&out).output().unwrap();
        assert!(!bare.status.success(), "libc_api passed without libchalloc preloaded");

        let run = Command::new(&out).env("LD_PRELOAD", &so).output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(run.status.success(), "libc_api failed\nstdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("ok libc_api"), "stdout: {stdout}");

        // `errno` across the `free` that creates the background thread
        // (design 8.1): the thread is requested by the
        // configuration and created by a periodic tick, which the mode
        // arranges to land in a free.
        let run = Command::new(&out)
            .arg("background")
            .env("LD_PRELOAD", &so)
            .env("MALLOC_CONF", "abort_conf:true,background_thread:true")
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&out);
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(run.status.success(), "libc_api background failed\nstdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("ok libc_api"), "stdout: {stdout}");

        for cmd in [
            std::vec!["sh", "-c", "ls /usr/lib >/dev/null && echo preload-ok"],
            std::vec!["readelf", "-h", &so],
            std::vec![cc.as_str(), "--version"],
        ] {
            let run = Command::new(cmd[0]).args(&cmd[1..]).env("LD_PRELOAD", &so).output().unwrap();
            assert!(
                run.status.success(),
                "{cmd:?} failed under LD_PRELOAD: {}",
                String::from_utf8_lossy(&run.stderr)
            );
        }
    }
}
