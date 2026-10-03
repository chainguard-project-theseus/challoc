//! The C++ allocation operators (design.md section 13.1): the eight
//! `operator new` and twelve `operator delete` signatures that libstdc++
//! defines and jemalloc replaces. A C++ program that is only preloaded with
//! `malloc`/`free` still routes `new` through libstdc++'s `operator new`,
//! which is a second PLT hop plus a null-check loop on every allocation.
//! On larson, which allocates with `new char[]`, that cost 13% of the run.
//!
//! `operator new` never returns null: on failure it runs the standard
//! `new_handler` loop and, with no handler installed, throws
//! `std::bad_alloc`; the `nothrow_t` forms return null instead. Rust cannot
//! throw a C++ exception, so the throw is libstdc++'s own
//! `std::__throw_bad_alloc()` and the handler comes from
//! `std::get_new_handler()`. Both are reached through *weak* undefined
//! symbols loaded from the GOT in `asm!`, so a C process that never maps
//! libstdc++ still loads `libchalloc.so` under `BIND_NOW` (the slots
//! resolve to null and the throw degrades to an abort with a message). The
//! exception unwinds through the frames of this module, which carry
//! `.eh_frame` entries and no landing pads, exactly like C code compiled
//! with unwind tables; `tests/c/cxx_new.cpp` catches it under the preload.
//!
//! The sized `delete` forms are `free_sized` and `free_aligned_sized`
//! (design 13.1): the size (and `align_val_t`) name a class through the
//! same `class_for(size, align)` the matching `new` used, so a correct
//! program's hint always matches; the slot is pushed without the slab
//! descriptor's bit test (design 6.3) and a mismatched size aborts as
//! `free_sized` does. The unsized aligned forms have no size to check and
//! take the plain `free` path with the alignment ignored. Aligned `new`
//! goes through `aligned_alloc`. An `align_val_t` that is not a power of
//! two (0 included) can never be satisfied, so it fails at once without
//! calling the `new_handler`, as libstdc++ does: the throwing forms throw
//! `std::bad_alloc` and the `nothrow_t` forms return null.

use core::arch::asm;
use core::ffi::c_void;
use core::ptr;

use super::libc;

/// `std::new_handler`: `void (*)()`, null when none is installed.
type NewHandler = Option<unsafe extern "C" fn()>;

/// Address of libstdc++'s `std::get_new_handler()`, or null when no
/// libstdc++ is loaded in the process.
#[inline(never)]
fn get_new_handler_addr() -> *const c_void {
    let out: *const c_void;
    // SAFETY: a GOT load of a weak undefined symbol; no memory is written.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!(
            ".weak _ZSt15get_new_handlerv",
            "mov {out}, qword ptr [rip + _ZSt15get_new_handlerv@GOTPCREL]",
            out = out(reg) out,
            options(nostack, readonly, preserves_flags),
        );
        #[cfg(target_arch = "aarch64")]
        asm!(
            ".weak _ZSt15get_new_handlerv",
            "adrp {out}, :got:_ZSt15get_new_handlerv",
            "ldr {out}, [{out}, :got_lo12:_ZSt15get_new_handlerv]",
            out = out(reg) out,
            options(nostack, readonly, preserves_flags),
        );
    }
    out
}

/// Address of libstdc++'s `std::__throw_bad_alloc()`, or null.
#[inline(never)]
fn throw_bad_alloc_addr() -> *const c_void {
    let out: *const c_void;
    // SAFETY: as in `get_new_handler_addr`.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!(
            ".weak _ZSt17__throw_bad_allocv",
            "mov {out}, qword ptr [rip + _ZSt17__throw_bad_allocv@GOTPCREL]",
            out = out(reg) out,
            options(nostack, readonly, preserves_flags),
        );
        #[cfg(target_arch = "aarch64")]
        asm!(
            ".weak _ZSt17__throw_bad_allocv",
            "adrp {out}, :got:_ZSt17__throw_bad_allocv",
            "ldr {out}, [{out}, :got_lo12:_ZSt17__throw_bad_allocv]",
            out = out(reg) out,
            options(nostack, readonly, preserves_flags),
        );
    }
    out
}

/// The installed `new_handler`, if libstdc++ is present and one is set.
fn new_handler() -> NewHandler {
    let get = get_new_handler_addr();
    if get.is_null() {
        return None;
    }
    // SAFETY: `std::get_new_handler` takes no arguments and returns a plain
    // function pointer, which is C-compatible on both ABIs.
    let get: unsafe extern "C" fn() -> NewHandler = unsafe { core::mem::transmute(get) };
    unsafe { get() }
}

/// Throw `std::bad_alloc` from libstdc++, or abort when it is not loaded.
#[cold]
#[inline(never)]
fn throw_bad_alloc() -> ! {
    let t = throw_bad_alloc_addr();
    if t.is_null() {
        crate::sys::abort_fmt(format_args!(
            "operator new failed and std::__throw_bad_alloc is not available"
        ));
    }
    // SAFETY: `std::__throw_bad_alloc` takes no arguments and never returns.
    let t: unsafe extern "C" fn() -> ! = unsafe { core::mem::transmute(t) };
    unsafe { t() }
}

/// The `operator new` failure protocol: call the `new_handler` and retry
/// until an attempt succeeds or no handler is installed, then throw
/// `std::bad_alloc` (or return null for the `nothrow_t` forms). `align` is
/// 0 for the unaligned operators.
#[cold]
#[inline(never)]
fn oom(size: usize, align: usize, nothrow: bool) -> *mut u8 {
    loop {
        let Some(handler) = new_handler() else {
            if nothrow {
                return ptr::null_mut();
            }
            throw_bad_alloc()
        };
        // SAFETY: a `void (*)()` installed by the program through
        // `std::set_new_handler`; it may throw, which unwinds through here.
        unsafe { handler() };
        let p = if align == 0 {
            libc::malloc(size)
        } else {
            libc::aligned_alloc(align, size)
        };
        if !p.is_null() {
            return p;
        }
    }
}

/// `malloc`'s slow path with the `new` failure protocol on top.
#[cold]
#[inline(never)]
fn new_slow(size: usize, nothrow: bool) -> *mut u8 {
    let p = libc::malloc_slow(size);
    if p.is_null() {
        oom(size, 0, nothrow)
    } else {
        p
    }
}

/// `operator new(size)` and `operator new[](size)`: `malloc`'s fast path
/// inline, everything else one tail call away.
#[inline(always)]
pub fn new(size: usize) -> *mut u8 {
    match libc::malloc_fast(size) {
        Some(p) => p,
        None => new_slow(size, false),
    }
}

/// The `nothrow_t` forms: null on failure after the handler loop.
#[inline(always)]
pub fn new_nothrow(size: usize) -> *mut u8 {
    match libc::malloc_fast(size) {
        Some(p) => p,
        None => new_slow(size, true),
    }
}

/// The `align_val_t` forms. A non-power-of-two `align` fails here and
/// skips the handler loop: no amount of freed memory makes it valid.
pub fn new_aligned(size: usize, align: usize, nothrow: bool) -> *mut u8 {
    if !align.is_power_of_two() {
        if nothrow {
            return ptr::null_mut();
        }
        throw_bad_alloc()
    }
    let p = libc::aligned_alloc(align, size);
    if p.is_null() {
        oom(size, align, nothrow)
    } else {
        p
    }
}

/// The unsized `operator delete` forms: `free`, with any alignment ignored.
#[inline(always)]
pub fn delete(p: *mut u8) {
    libc::free(p)
}

/// `operator delete(void*, size_t)` and its array form: `free_sized`. The
/// size is the one the matching `operator new(size)` was called with, so
/// `class_of(size)` is the slot's class.
#[inline(always)]
pub fn delete_sized(p: *mut u8, size: usize) {
    libc::free_sized(p, size)
}

/// `operator delete(void*, size_t, std::align_val_t)` and its array form:
/// `free_aligned_sized` with the arguments the matching aligned `new`
/// passed to `aligned_alloc`.
#[inline(always)]
pub fn delete_sized_aligned(p: *mut u8, size: usize, align: usize) {
    libc::free_aligned_sized(p, align, size)
}

// ---------------------------------------------------------------------------
// Itanium C++ ABI symbols (identical on x86_64 and aarch64: `m` is
// `unsigned long`). Compiled out of the test binary like `libc::exports`.
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    use core::ffi::c_void;

    /// `operator new(size_t)`
    #[unsafe(export_name = "_Znwm")]
    pub extern "C" fn new(size: usize) -> *mut c_void {
        super::new(size) as *mut c_void
    }

    /// `operator new[](size_t)`
    #[unsafe(export_name = "_Znam")]
    pub extern "C" fn new_array(size: usize) -> *mut c_void {
        super::new(size) as *mut c_void
    }

    /// `operator new(size_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZnwmRKSt9nothrow_t")]
    pub extern "C" fn new_nothrow(size: usize, _tag: *const c_void) -> *mut c_void {
        super::new_nothrow(size) as *mut c_void
    }

    /// `operator new[](size_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZnamRKSt9nothrow_t")]
    pub extern "C" fn new_array_nothrow(size: usize, _tag: *const c_void) -> *mut c_void {
        super::new_nothrow(size) as *mut c_void
    }

    /// `operator new(size_t, std::align_val_t)`
    #[unsafe(export_name = "_ZnwmSt11align_val_t")]
    pub extern "C" fn new_aligned(size: usize, align: usize) -> *mut c_void {
        super::new_aligned(size, align, false) as *mut c_void
    }

    /// `operator new[](size_t, std::align_val_t)`
    #[unsafe(export_name = "_ZnamSt11align_val_t")]
    pub extern "C" fn new_array_aligned(size: usize, align: usize) -> *mut c_void {
        super::new_aligned(size, align, false) as *mut c_void
    }

    /// `operator new(size_t, std::align_val_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZnwmSt11align_val_tRKSt9nothrow_t")]
    pub extern "C" fn new_aligned_nothrow(
        size: usize,
        align: usize,
        _tag: *const c_void,
    ) -> *mut c_void {
        super::new_aligned(size, align, true) as *mut c_void
    }

    /// `operator new[](size_t, std::align_val_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZnamSt11align_val_tRKSt9nothrow_t")]
    pub extern "C" fn new_array_aligned_nothrow(
        size: usize,
        align: usize,
        _tag: *const c_void,
    ) -> *mut c_void {
        super::new_aligned(size, align, true) as *mut c_void
    }

    /// `operator delete(void*)`
    #[unsafe(export_name = "_ZdlPv")]
    pub extern "C" fn delete(p: *mut c_void) {
        super::delete(p as *mut u8)
    }

    /// `operator delete[](void*)`
    #[unsafe(export_name = "_ZdaPv")]
    pub extern "C" fn delete_array(p: *mut c_void) {
        super::delete(p as *mut u8)
    }

    /// `operator delete(void*, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZdlPvRKSt9nothrow_t")]
    pub extern "C" fn delete_nothrow(p: *mut c_void, _tag: *const c_void) {
        super::delete(p as *mut u8)
    }

    /// `operator delete[](void*, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZdaPvRKSt9nothrow_t")]
    pub extern "C" fn delete_array_nothrow(p: *mut c_void, _tag: *const c_void) {
        super::delete(p as *mut u8)
    }

    /// `operator delete(void*, size_t)`
    #[unsafe(export_name = "_ZdlPvm")]
    pub extern "C" fn delete_sized(p: *mut c_void, size: usize) {
        super::delete_sized(p as *mut u8, size)
    }

    /// `operator delete[](void*, size_t)`
    #[unsafe(export_name = "_ZdaPvm")]
    pub extern "C" fn delete_array_sized(p: *mut c_void, size: usize) {
        super::delete_sized(p as *mut u8, size)
    }

    /// `operator delete(void*, std::align_val_t)`
    #[unsafe(export_name = "_ZdlPvSt11align_val_t")]
    pub extern "C" fn delete_aligned(p: *mut c_void, _align: usize) {
        super::delete(p as *mut u8)
    }

    /// `operator delete[](void*, std::align_val_t)`
    #[unsafe(export_name = "_ZdaPvSt11align_val_t")]
    pub extern "C" fn delete_array_aligned(p: *mut c_void, _align: usize) {
        super::delete(p as *mut u8)
    }

    /// `operator delete(void*, std::align_val_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZdlPvSt11align_val_tRKSt9nothrow_t")]
    pub extern "C" fn delete_aligned_nothrow(p: *mut c_void, _align: usize, _tag: *const c_void) {
        super::delete(p as *mut u8)
    }

    /// `operator delete[](void*, std::align_val_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZdaPvSt11align_val_tRKSt9nothrow_t")]
    pub extern "C" fn delete_array_aligned_nothrow(
        p: *mut c_void,
        _align: usize,
        _tag: *const c_void,
    ) {
        super::delete(p as *mut u8)
    }

    /// `operator delete(void*, size_t, std::align_val_t)`
    #[unsafe(export_name = "_ZdlPvmSt11align_val_t")]
    pub extern "C" fn delete_sized_aligned(p: *mut c_void, size: usize, align: usize) {
        super::delete_sized_aligned(p as *mut u8, size, align)
    }

    /// `operator delete[](void*, size_t, std::align_val_t)`
    #[unsafe(export_name = "_ZdaPvmSt11align_val_t")]
    pub extern "C" fn delete_array_sized_aligned(p: *mut c_void, size: usize, align: usize) {
        super::delete_sized_aligned(p as *mut u8, size, align)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libstdcxx_symbols_are_absent_from_the_test_binary() {
        // The test binary is a Rust program without libstdc++, so both weak
        // references must resolve to null and the throw must be the abort
        // path rather than a wild call. Only the address lookups are safe to
        // run here.
        assert!(get_new_handler_addr().is_null());
        assert!(throw_bad_alloc_addr().is_null());
        assert!(new_handler().is_none());
    }
}
