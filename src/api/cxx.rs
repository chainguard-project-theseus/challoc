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
//! The `nothrow_t` forms must return null even when the handler throws,
//! and Rust cannot catch a C++ exception, so they call the handler through
//! `challoc_call_new_handler` in `cxx_handler.cpp` (compiled by build.rs),
//! which catches anything the handler throws and reports it as 0. The
//! throwing forms call the handler directly, so its exception reaches the
//! caller unchanged.
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
use crate::heap::Live;
#[cfg(verus_keep_ghost)]
use crate::meta::is_pow2;
use crate::vspec::cbuf::CCall;
use crate::vspec::errno::ThreadErrno;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, verus};

/// `std::new_handler`: `void (*)()`, null when none is installed.
type NewHandler = Option<unsafe extern "C" fn()>;

unsafe extern "C" {
    /// `src/api/cxx_handler.cpp`: calls `handler` and returns 1, or 0 when
    /// it threw (the exception is caught and destroyed there).
    fn challoc_call_new_handler(handler: unsafe extern "C" fn()) -> core::ffi::c_int;
}

verus! {

/// Address of libstdc++'s `std::get_new_handler()`, or null when no
/// libstdc++ is loaded in the process.
/// Trusted: inline assembly.
#[verifier::external_body]
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

/// `t` is what [`throw_bad_alloc_addr`] read: the address of libstdc++'s
/// `std::__throw_bad_alloc()`, or null.
uninterp spec fn is_throw_bad_alloc(t: *const c_void) -> bool;

/// Address of libstdc++'s `std::__throw_bad_alloc()`, or null.
/// Trusted: inline assembly.
#[verifier::external_body]
#[inline(never)]
fn throw_bad_alloc_addr() -> (r: *const c_void)
    ensures
        is_throw_bad_alloc(r),
{
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

/// The installed `new_handler`, opaque to Verus (which has no function
/// pointer types).
#[verifier::external_body]
struct Handler(unsafe extern "C" fn());

/// Call `std::get_new_handler` at `get`, its non-null address: the
/// installed `new_handler`, if one is set.
/// Trusted: calls a function pointer and reads one from libstdc++. That
/// `get` is the address [`get_new_handler_addr`] returned is not stated;
/// [`installed_handler`] is the one caller.
#[verifier::external_body]
#[inline(always)]
fn call_get_new_handler(get: *const c_void) -> Option<Handler>
    requires
        crate::sys::may_syscall(),
        get@.addr != 0,
{
    // SAFETY: `std::get_new_handler` takes no arguments and returns a plain
    // function pointer, which is C-compatible on both ABIs.
    let get: unsafe extern "C" fn() -> NewHandler = unsafe { core::mem::transmute(get) };
    unsafe { get() }.map(Handler)
}

/// The installed `new_handler`, if libstdc++ is present and one is set.
fn installed_handler() -> Option<Handler>
    requires
        crate::sys::may_syscall(),
{
    let get = get_new_handler_addr();
    if get.is_null() {
        return None;
    }
    call_get_new_handler(get)
}

/// Call `std::__throw_bad_alloc` at `t`, its non-null address
/// ([`is_throw_bad_alloc`]).
/// Trusted: calls a function pointer (Verus has no function pointer
/// types).
#[verifier::external_body]
#[inline(always)]
fn call_throw_bad_alloc(t: *const c_void) -> !
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        is_throw_bad_alloc(t),
        t@.addr != 0,
{
    // SAFETY: `is_throw_bad_alloc(t)` makes `t` the address of
    // `std::__throw_bad_alloc`, which takes no arguments and never returns.
    let t: unsafe extern "C" fn() -> ! = unsafe { core::mem::transmute(t) };
    unsafe { t() }
}

/// Throw `std::bad_alloc` from libstdc++, or abort when it is not loaded.
#[cold]
#[inline(never)]
fn throw_bad_alloc() -> !
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    let t = throw_bad_alloc_addr();
    if t.is_null() {
        crate::sys::abort_fmt(format_args!(
            "operator new failed and std::__throw_bad_alloc is not available"
        ));
    }
    call_throw_bad_alloc(t)
}

/// Call the program's `new_handler`, which may write `errno`: `errno` is
/// the calling thread's, taken `&mut` with no ensures, so nothing is
/// known of its value afterwards.
/// Trusted: calls a function pointer; it may throw, which unwinds through
/// here.
#[verifier::external_body]
#[inline(always)]
fn call_handler(handler: Handler, _errno: Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: a `void (*)()` installed by the program through
    // `std::set_new_handler`; an exception it throws unwinds through here
    // to the caller of `operator new`.
    unsafe { (handler.0)() }
}

/// Call the program's `new_handler` through `challoc_call_new_handler`
/// (`cxx_handler.cpp`), which catches anything it throws: `false` when it
/// threw. The handler may write `errno`, as for [`call_handler`].
/// Trusted: an FFI call with a function pointer; nothing is claimed
/// about the result or `errno`.
#[verifier::external_body]
#[inline(always)]
fn call_handler_caught(handler: Handler, _errno: Tracked<&mut ThreadErrno>) -> bool
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: a `void (*)()` installed by the program through
    // `std::set_new_handler`; the shim catches whatever it throws.
    unsafe { challoc_call_new_handler(handler.0) != 0 }
}

/// The `operator new` failure protocol: call the `new_handler` and retry
/// until an attempt succeeds or no handler is installed, then throw
/// `std::bad_alloc` (or return null for the `nothrow_t` forms, also when
/// the handler throws). `align` is 0 for the unaligned operators. `blk`
/// gets the block's token.
#[cold]
#[inline(never)]
// Ends only when the program's handler frees memory, is removed or throws.
#[verifier::exec_allows_no_decreases_clause]
fn oom(
    size: usize,
    align: usize,
    nothrow: bool,
    Tracked(blk): Tracked<&mut Live>,
    Tracked(errno): Tracked<&mut ThreadErrno>,
) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        align == 0 ==> crate::heap::fits(r, size as int, crate::QUANTUM as int),
        align != 0 && is_pow2(align) ==> crate::heap::fits(r, size as int, align as int),
        // A non-zero alignment that is not a power of two never gets a
        // block: `aligned_alloc` refuses it on every attempt.
        align != 0 && !is_pow2(align) ==> r@.addr == 0,
        final(blk).gives(r, size as int),
        !nothrow ==> r@.addr != 0,
{
    loop
        invariant
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
    {
        let Some(handler) = installed_handler() else {
            if nothrow {
                return ptr::null_mut();
            }
            throw_bad_alloc()
        };
        if nothrow {
            if !call_handler_caught(handler, Tracked(&mut *errno)) {
                return ptr::null_mut();
            }
        } else {
            call_handler(handler, Tracked(&mut *errno));
        }
        let p = if align == 0 {
            libc::malloc_block(size, Tracked(blk), Tracked(errno))
        } else {
            libc::aligned_alloc_block(align, size, Tracked(blk), Tracked(errno))
        };
        if !p.is_null() {
            return p;
        }
    }
}

/// `malloc`'s slow path with the `new` failure protocol on top.
#[cold]
#[inline(never)]
fn new_slow(
    size: usize,
    nothrow: bool,
    Tracked(blk): Tracked<&mut Live>,
    Tracked(errno): Tracked<&mut ThreadErrno>,
) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        crate::heap::fits(r, size as int, crate::QUANTUM as int),
        final(blk).gives(r, size as int),
        !nothrow ==> r@.addr != 0,
{
    let p = libc::malloc_slow(size, Tracked(blk), Tracked(errno));
    if p.is_null() {
        oom(size, 0, nothrow, Tracked(blk), Tracked(errno))
    } else {
        p
    }
}

/// `operator new(size)` and `operator new[](size)`: `malloc`'s fast path
/// inline, everything else one tail call away. The block's token goes to
/// C++ with it.
#[inline(always)]
pub fn new(size: usize, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        crate::heap::fits(r, size as int, crate::QUANTUM as int),
        // The C++ contract: a throwing `operator new` never returns null.
        r@.addr != 0,
{
    let tracked mut blk = Live::empty();
    new_block(size, false, Tracked(&mut blk), Tracked(errno))
}

/// The `nothrow_t` forms: null on failure after the handler loop.
#[inline(always)]
pub fn new_nothrow(size: usize, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        crate::heap::fits(r, size as int, crate::QUANTUM as int),
{
    let tracked mut blk = Live::empty();
    new_block(size, true, Tracked(&mut blk), Tracked(errno))
}

/// [`new`] (`nothrow` false) and [`new_nothrow`] with the block's token,
/// as `libc::malloc_block`.
#[inline(always)]
pub fn new_block(
    size: usize,
    nothrow: bool,
    Tracked(blk): Tracked<&mut Live>,
    Tracked(errno): Tracked<&mut ThreadErrno>,
) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        crate::heap::fits(r, size as int, crate::QUANTUM as int),
        final(blk).gives(r, size as int),
        !nothrow ==> r@.addr != 0,
{
    match libc::malloc_fast(size, Tracked(blk)) {
        Some(p) => p,
        None => new_slow(size, nothrow, Tracked(blk), Tracked(errno)),
    }
}

/// The `align_val_t` forms. The block's token goes to C++ with it. An
/// alignment that is not a power of two (0 included; undefined in C++)
/// never gets a block: it fails here and skips the handler loop, since no
/// amount of freed memory makes it valid. The `nothrow_t` forms return
/// null and the others throw.
pub fn new_aligned(
    size: usize,
    align: usize,
    nothrow: bool,
    Tracked(errno): Tracked<&mut ThreadErrno>,
) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        is_pow2(align) ==> crate::heap::fits(r, size as int, align as int),
        !is_pow2(align) ==> r@.addr == 0,
        !nothrow ==> r@.addr != 0,
{
    let tracked mut blk = Live::empty();
    new_aligned_block(size, align, nothrow, Tracked(&mut blk), Tracked(errno))
}

/// [`new_aligned`] with the block's token, as `libc::aligned_alloc_block`.
pub fn new_aligned_block(
    size: usize,
    align: usize,
    nothrow: bool,
    Tracked(blk): Tracked<&mut Live>,
    Tracked(errno): Tracked<&mut ThreadErrno>,
) -> (r: *mut u8)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        is_pow2(align) ==> crate::heap::fits(r, size as int, align as int),
        !is_pow2(align) ==> r@.addr == 0,
        final(blk).gives(r, size as int),
        !nothrow ==> r@.addr != 0,
{
    if !align.is_power_of_two() {
        if nothrow {
            return ptr::null_mut();
        }
        throw_bad_alloc()
    }
    proof {
        crate::meta::lemma_pow2_bits(align);
    }
    let p = libc::aligned_alloc_block(align, size, Tracked(blk), Tracked(errno));
    if p.is_null() {
        oom(size, align, nothrow, Tracked(blk), Tracked(errno))
    } else {
        p
    }
}

/// The unsized `operator delete` forms: `free`, with any alignment ignored.
/// C++ holds the token of the block it frees
/// ([`crate::heap::live::axiom_c_claim`]).
#[inline(always)]
pub fn delete(p: *mut u8, Tracked(c): Tracked<CCall>, errno: Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        p@.addr != 0 ==> crate::heap::free_target(p),
{
    libc::free(p, Tracked(c), errno)
}

/// `operator delete(void*, size_t)` and its array form: `free_sized`. The
/// size is the one the matching `operator new(size)` was called with, so
/// `class_of(size)` is the slot's class.
#[inline(always)]
pub fn delete_sized(p: *mut u8, size: usize, Tracked(c): Tracked<CCall>, errno: Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        p@.addr != 0 ==> crate::heap::free_target(p),
        p@.addr != 0 ==> crate::heap::hint_ok(p, libc::spec_class_for(size, 1)),
{
    libc::free_sized(p, size, Tracked(c), errno)
}

/// `operator delete(void*, size_t, std::align_val_t)` and its array form:
/// `free_aligned_sized` with the arguments the matching aligned `new`
/// passed to `aligned_alloc`.
#[inline(always)]
pub fn delete_sized_aligned(p: *mut u8, size: usize, align: usize, Tracked(c): Tracked<CCall>, errno: Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        p@.addr != 0 ==> crate::heap::free_target(p),
        p@.addr != 0 ==> crate::heap::hint_ok(p, if is_pow2(align) { libc::spec_class_for(size, align) } else { None }),
{
    libc::free_aligned_sized(p, align, size, Tracked(c), errno)
}

} // verus!

// ---------------------------------------------------------------------------
// Itanium C++ ABI symbols (identical on x86_64 and aarch64: `m` is
// `unsigned long`). Compiled out of the test binary like `libc::exports`.
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    #[cfg(verus_keep_ghost)]
    use crate::vspec::cbuf::{CCall, CEntry};
    use core::ffi::c_void;
    use vstd::prelude::{Tracked, verus};

    verus! {

    /// The token of a C++ call that hands over the block at `p` to an
    /// `operator delete` ([`CCall`], [`CEntry::Frees`]). Trusted: as
    /// `libc`'s `axiom_c_call`, which this module cannot name.
    axiom fn axiom_c_call(e: CEntry) -> (tracked c: CCall)
        requires
            e is Frees,
        ensures
            c.fresh(e),
    ;

    /// `operator new(size_t)`
    #[unsafe(export_name = "_Znwm")]
    pub extern "C" fn new(size: usize) -> *mut c_void {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new(size, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new[](size_t)`
    #[unsafe(export_name = "_Znam")]
    pub extern "C" fn new_array(size: usize) -> *mut c_void {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new(size, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new(size_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZnwmRKSt9nothrow_t")]
    pub extern "C" fn new_nothrow(size: usize, _tag: *const c_void) -> *mut c_void {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new_nothrow(size, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new[](size_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZnamRKSt9nothrow_t")]
    pub extern "C" fn new_array_nothrow(size: usize, _tag: *const c_void) -> *mut c_void {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new_nothrow(size, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new(size_t, std::align_val_t)`
    #[unsafe(export_name = "_ZnwmSt11align_val_t")]
    pub extern "C" fn new_aligned(size: usize, align: usize) -> *mut c_void {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new_aligned(size, align, false, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new[](size_t, std::align_val_t)`
    #[unsafe(export_name = "_ZnamSt11align_val_t")]
    pub extern "C" fn new_array_aligned(size: usize, align: usize) -> *mut c_void {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new_aligned(size, align, false, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new(size_t, std::align_val_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZnwmSt11align_val_tRKSt9nothrow_t")]
    pub extern "C" fn new_aligned_nothrow(
        size: usize,
        align: usize,
        _tag: *const c_void,
    ) -> *mut c_void {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new_aligned(size, align, true, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new[](size_t, std::align_val_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZnamSt11align_val_tRKSt9nothrow_t")]
    pub extern "C" fn new_array_aligned_nothrow(
        size: usize,
        align: usize,
        _tag: *const c_void,
    ) -> *mut c_void {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new_aligned(size, align, true, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator delete(void*)`
    #[unsafe(export_name = "_ZdlPv")]
    pub extern "C" fn delete(p: *mut c_void) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete[](void*)`
    #[unsafe(export_name = "_ZdaPv")]
    pub extern "C" fn delete_array(p: *mut c_void) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete(void*, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZdlPvRKSt9nothrow_t")]
    pub extern "C" fn delete_nothrow(p: *mut c_void, _tag: *const c_void) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete[](void*, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZdaPvRKSt9nothrow_t")]
    pub extern "C" fn delete_array_nothrow(p: *mut c_void, _tag: *const c_void) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete(void*, size_t)`
    #[unsafe(export_name = "_ZdlPvm")]
    pub extern "C" fn delete_sized(p: *mut c_void, size: usize) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete_sized(p as *mut u8, size, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete[](void*, size_t)`
    #[unsafe(export_name = "_ZdaPvm")]
    pub extern "C" fn delete_array_sized(p: *mut c_void, size: usize) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete_sized(p as *mut u8, size, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete(void*, std::align_val_t)`
    #[unsafe(export_name = "_ZdlPvSt11align_val_t")]
    pub extern "C" fn delete_aligned(p: *mut c_void, _align: usize) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete[](void*, std::align_val_t)`
    #[unsafe(export_name = "_ZdaPvSt11align_val_t")]
    pub extern "C" fn delete_array_aligned(p: *mut c_void, _align: usize) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete(void*, std::align_val_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZdlPvSt11align_val_tRKSt9nothrow_t")]
    pub extern "C" fn delete_aligned_nothrow(p: *mut c_void, _align: usize, _tag: *const c_void) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete[](void*, std::align_val_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZdaPvSt11align_val_tRKSt9nothrow_t")]
    pub extern "C" fn delete_array_aligned_nothrow(
        p: *mut c_void,
        _align: usize,
        _tag: *const c_void,
    ) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete(void*, size_t, std::align_val_t)`
    #[unsafe(export_name = "_ZdlPvmSt11align_val_t")]
    pub extern "C" fn delete_sized_aligned(p: *mut c_void, size: usize, align: usize) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete_sized_aligned(
            p as *mut u8,
            size,
            align,
            Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })),
            Tracked(&mut errno),
        )
    }

    /// `operator delete[](void*, size_t, std::align_val_t)`
    #[unsafe(export_name = "_ZdaPvmSt11align_val_t")]
    pub extern "C" fn delete_array_sized_aligned(p: *mut c_void, size: usize, align: usize) {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete_sized_aligned(
            p as *mut u8,
            size,
            align,
            Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })),
            Tracked(&mut errno),
        )
    }
    } // verus!
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
        assert!(installed_handler().is_none());
    }
}
