//! Standard and glibc allocation entry points (design.md section 13.1):
//! `malloc`, `free`, `calloc`, `realloc`, `aligned_alloc`, `posix_memalign`,
//! `memalign`, `valloc`, `pvalloc`, `free_sized`, `free_aligned_sized`,
//! `reallocarray`, `malloc_usable_size`.
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
//!   fast path touches only the heap, the cache and one descriptor word,
//!   and the slow paths use raw syscalls that never write `errno`.
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
use crate::layout::{CLASSES, Kind, MEDIUM_MAX, NCLASSES, class_of};

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
        None => shared.alloc_huge(size, align),
    }
}

/// `malloc`.
#[inline(always)]
pub fn malloc(size: usize) -> *mut u8 {
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
    match super::shared_if_ready() {
        Some(shared) => heap::free(shared, p),
        None => free_before_init(p),
    }
}

#[cold]
#[inline(never)]
fn free_before_init(p: *mut u8) -> ! {
    crate::sys::abort_fmt(format_args!(
        "challoc: {:#x} passed before any allocation: not an allocation",
        p as usize
    ))
}

/// Usable bytes of the live allocation `p`. Aborts unless `p` is a live
/// slab slot or huge run start.
pub fn usable_size(shared: &Shared, p: *mut u8) -> usize {
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, .. } => {
            let class = class as usize;
            shared.classes[class].check(p);
            CLASSES[class].size()
        }
        Kind::Huge { page } => shared.huge.usable_size(page),
        Kind::Invalid => crate::sys::abort_fmt(format_args!(
            "challoc: {:#x} is not an allocation",
            p as usize
        )),
    }
}

/// `malloc_usable_size`. Zero for null.
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
        Kind::Slab { class, .. } => {
            let class = class as usize;
            shared.classes[class].check(p);
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
            "challoc: realloc of {:#x}: not an allocation",
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

/// Allocate `size` bytes aligned to `align`, a power of two. Null with
/// `errno = ENOMEM` on exhaustion.
pub fn alloc_aligned(align: usize, size: usize) -> *mut u8 {
    debug_assert!(align.is_power_of_two());
    if align <= QUANTUM {
        return malloc(size);
    }
    let shared = super::shared();
    let p = match class_of(size) {
        Some(c) if align <= MEDIUM_MAX => {
            let mut c = c;
            while CLASSES[c].size() % align != 0 {
                c += 1;
                debug_assert!(c < NCLASSES);
            }
            heap::alloc(shared, c)
        }
        _ => alloc_huge(shared, size, align).0,
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

    /// C23 `free_sized`: the size is advisory and unchecked in Phase 0.
    #[unsafe(no_mangle)]
    pub extern "C" fn free_sized(p: *mut c_void, _size: usize) {
        super::free(p as *mut u8)
    }

    /// C23 `free_aligned_sized`: alignment and size are advisory.
    #[unsafe(no_mangle)]
    pub extern "C" fn free_aligned_sized(p: *mut c_void, _align: usize, _size: usize) {
        super::free(p as *mut u8)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn malloc_usable_size(p: *mut c_void) -> usize {
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
        let _ = std::fs::remove_file(&out);
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(run.status.success(), "libc_api failed\nstdout: {stdout}\nstderr: {stderr}");
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
