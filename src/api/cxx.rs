//! The C++ allocation operators (design 13.1): the eight
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
//! resolve to null and the throw degrades to an abort with a message).
//!
//! That exception, and one thrown by the `new_handler`, unwinds through
//! Rust frames, which the language does not define: the reference makes a
//! foreign exception crossing an `extern "C"` frame undefined behaviour,
//! and under `panic = "abort"` a `"C-unwind"` frame aborts it, so neither
//! ABI makes it defined. It works because of what rustc emits. `_Znwm` and
//! `_Znam` reach `new_slow` by a tail `jmp`, which reaches `oom` the same
//! way, and the frames left on the stack (`oom`, `throw_bad_alloc` and the
//! throwing aligned forms) each have an `.eh_frame` FDE whose CIE names no
//! personality routine. They have no landing pads, so the C++ unwinder
//! steps through them as it does C frames with unwind tables. The
//! `cxx_new` test (`cargo test --lib cxx_new`, `tests/c/cxx_new.cpp`)
//! catches both exceptions under the preload. The defined alternative,
//! throwing operators owned by `cxx_handler.cpp`, costs a call on every
//! `new` and breaks two hard rules in CONTRIBUTING.md (exports are Rust
//! `extern "C"` functions; no C++ in the library beyond the catch).
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

/// `g` is what [`get_new_handler_addr`] read: the address of the
/// definition of `std::get_new_handler()` that symbol resolution picks,
/// or null.
uninterp spec fn is_get_new_handler(g: *const c_void) -> bool;

/// Address of libstdc++'s `std::get_new_handler()`, or null when no
/// libstdc++ is loaded in the process.
/// Trusted: inline assembly.
#[verifier::external_body]
#[inline(never)]
fn get_new_handler_addr() -> (r: *const c_void)
    ensures
        is_get_new_handler(r),
{
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

/// Call `std::get_new_handler` at `get`, its non-null address
/// ([`is_get_new_handler`]): the installed `new_handler`, if one is set.
/// Trusted: calls a function pointer and reads one from libstdc++.
#[verifier::external_body]
#[inline(always)]
fn call_get_new_handler(get: *const c_void) -> Option<Handler>
    requires
        crate::sys::may_syscall(),
        is_get_new_handler(get),
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
    // It leaves by unwinding through this frame, which the language does
    // not define (module doc).
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
/// here (not defined by the language; module doc).
#[verifier::external_body]
#[inline(always)]
fn call_handler(handler: Handler, _errno: Tracked<&mut ThreadErrno>)
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: a `void (*)()` installed by the program through
    // `std::set_new_handler`. An exception it throws unwinds through here
    // to the caller of `operator new`, which rests on codegen rather than
    // the language (module doc).
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
/// the handler throws). `align` is 0 for the unaligned operators, else
/// the power of two `new_aligned_block` checked. `blk` gets the block's
/// token.
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
        align == 0 || is_pow2(align),
    ensures
        align == 0 ==> crate::heap::fits(r, size as int, crate::QUANTUM as int),
        align != 0 ==> crate::heap::fits(r, size as int, align as int),
        final(blk).gives(r, size as int),
        !nothrow ==> r@.addr != 0,
{
    loop
        invariant
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            align == 0 || is_pow2(align),
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
            libc::alloc_aligned(align, size, Tracked(blk), Tracked(errno))
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
    let p = libc::alloc_aligned(align, size, Tracked(blk), Tracked(errno));
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
pub fn delete(p: *mut u8, Tracked(c): Tracked<CCall>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        final(errno).val() == old(errno).val(),
        p@.addr != 0 ==> crate::heap::free_target(p),
{
    libc::free(p, Tracked(c), Tracked(errno))
}

/// `operator delete(void*, size_t)` and its array form: `free_sized`. The
/// size is the one the matching `operator new(size)` was called with, so
/// `class_of(size)` is the slot's class.
#[inline(always)]
pub fn delete_sized(p: *mut u8, size: usize, Tracked(c): Tracked<CCall>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        final(errno).val() == old(errno).val(),
        p@.addr != 0 ==> crate::heap::free_target(p),
        p@.addr != 0 ==> crate::heap::hint_ok(p, libc::spec_class_for(size, 1)),
{
    libc::free_sized(p, size, Tracked(c), Tracked(errno))
}

/// `operator delete(void*, size_t, std::align_val_t)` and its array form:
/// `free_aligned_sized` with the arguments the matching aligned `new`
/// passed to `aligned_alloc`.
#[inline(always)]
pub fn delete_sized_aligned(p: *mut u8, size: usize, align: usize, Tracked(c): Tracked<CCall>, Tracked(errno): Tracked<&mut ThreadErrno>)
    requires
        c.block() == Some(p),
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        final(errno).val() == old(errno).val(),
        p@.addr != 0 ==> crate::heap::free_target(p),
        p@.addr != 0 ==> crate::heap::hint_ok(p, if is_pow2(align) { libc::spec_class_for(size, align) } else { None }),
{
    libc::free_aligned_sized(p, align, size, Tracked(c), Tracked(errno))
}

} // verus!

/// Kani checks of `call_get_new_handler`, `call_throw_bad_alloc`,
/// `call_handler` and `call_handler_caught`
/// (`verus/ledger/src/api/cxx.rs.md`; `cfg(kani)` only, see
/// `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_cxx {
    use super::{
        Handler, NewHandler, call_get_new_handler, call_handler, call_handler_caught,
        call_throw_bad_alloc,
    };
    use core::ffi::{c_int, c_void};
    use vstd::prelude::Tracked;

    /// How many times `record` was called.
    static mut HANDLED: usize = 0;

    /// A `new_handler` that records its call.
    unsafe extern "C" fn record() {
        // SAFETY: the harness's own statics; no other code runs in a
        // harness.
        unsafe { HANDLED += 1 };
    }

    unsafe extern "C" fn other() {}

    /// `call_handler`: no `ensures`; it returns when the handler returns
    /// (the token is ghost), and (beyond the contract) calls the handler
    /// once.
    #[kani::proof]
    fn call_handler_calls_once() {
        call_handler(Handler(record), Tracked::assume_new());
        // SAFETY: as in `record`.
        assert_eq!(unsafe { HANDLED }, 1);
    }

    /// The handler the stand-in for `std::get_new_handler` returns, and
    /// how many times it was called.
    static mut INSTALLED: NewHandler = None;
    static mut GETS: usize = 0;

    /// A `std::get_new_handler` that returns `INSTALLED` and records its
    /// call.
    unsafe extern "C" fn get_installed() -> NewHandler {
        // SAFETY: as in `record`.
        unsafe {
            GETS += 1;
            INSTALLED
        }
    }

    /// The address of a handler, 0 for none.
    fn addr(h: NewHandler) -> usize {
        h.map_or(0, |f| f as usize)
    }

    /// `call_get_new_handler`: no `ensures`; it returns for the address
    /// of a `std::get_new_handler` (its `requires`), and (beyond the
    /// contract) calls it once and gives the handler it returned, for
    /// none or one of two handlers.
    #[kani::proof]
    fn call_get_new_handler_gives_installed() {
        let h: NewHandler = if kani::any() {
            None
        } else if kani::any() {
            Some(record)
        } else {
            Some(other)
        };
        // SAFETY: as in `record`.
        unsafe { INSTALLED = h };
        kani::cover!(h.is_none());
        kani::cover!(addr(h) == record as *const () as usize);
        kani::cover!(addr(h) == other as *const () as usize);
        assert_ne!(record as *const () as usize, other as *const () as usize);
        let got = call_get_new_handler(get_installed as *const c_void);
        // SAFETY: as in `record`.
        assert_eq!(unsafe { GETS }, 1);
        assert_eq!(addr(got.map(|g| g.0)), addr(h));
    }

    /// A stand-in for `std::__throw_bad_alloc` that leaves by a panic, as
    /// the runtime's function leaves by a throw.
    unsafe extern "C" fn throw_panics() -> ! {
        panic!("std::bad_alloc");
    }

    /// `call_throw_bad_alloc`: no `ensures`, and `-> !`, so it never
    /// returns. With `throw_panics` at `t`, the call panics and fails in
    /// no other way (an abort, an unwinding bound, a bad access).
    #[kani::proof]
    #[kani::should_panic]
    fn call_throw_bad_alloc_leaves_by_panic() {
        call_throw_bad_alloc(throw_panics as *const c_void)
    }

    /// A stand-in for `std::__throw_bad_alloc` that ends the path where
    /// the runtime's function would throw: no path goes on past it, so it
    /// is called at most once.
    unsafe extern "C" fn throw_ends() -> ! {
        kani::cover!(true);
        kani::assume(false);
        unreachable!()
    }

    /// `call_throw_bad_alloc` (beyond the contract): nothing before the
    /// call of the function at `t` fails, since a panic or an abort fails
    /// this harness, which has no `should_panic`. With
    /// `call_throw_bad_alloc_leaves_by_panic`, which fails unless the call
    /// panics, the pair shows that the body calls the function at `t` and
    /// leaves through it.
    #[kani::proof]
    fn call_throw_bad_alloc_calls_once() {
        call_throw_bad_alloc(throw_ends as *const c_void)
    }

    /// Whether the stand-in for `cxx_handler.cpp`'s shim reports a throw,
    /// and the handler it was passed.
    static mut THREW: bool = false;
    static mut PASSED: usize = 0;

    /// A stand-in for `challoc_call_new_handler`: when `THREW`, the
    /// handler threw before any effect and the shim caught it (0);
    /// otherwise it calls the handler and returns 1.
    #[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
    unsafe extern "C" fn caught_model(handler: unsafe extern "C" fn()) -> c_int {
        // SAFETY: as in `record`; `handler` is the harness's `record`.
        unsafe {
            PASSED = handler as usize;
            if THREW {
                return 0;
            }
            handler();
        }
        1
    }

    /// `call_handler_caught` with the shim replaced by `caught_model`:
    /// it passes the handler on and reports the shim's 0 as `false`, its 1
    /// as `true`. What the shim does is not checked.
    #[kani::proof]
    #[kani::stub(super::challoc_call_new_handler, caught_model)]
    fn call_handler_caught_reports_shim() {
        let threw: bool = kani::any();
        kani::cover!(threw);
        kani::cover!(!threw);
        // SAFETY: as in `record`.
        unsafe { THREW = threw };
        let r = call_handler_caught(Handler(record), Tracked::assume_new());
        assert_eq!(r, !threw);
        // SAFETY: as in `record`.
        let (passed, handled) = unsafe { (PASSED, HANDLED) };
        assert_eq!(passed, record as *const () as usize);
        assert_eq!(handled, if threw { 0 } else { 1 });
    }
}

// ---------------------------------------------------------------------------
// Itanium C++ ABI symbols (identical on x86_64 and aarch64: `m` is
// `unsigned long`). Compiled out of the test binary like `libc::exports`.
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    #[cfg(verus_keep_ghost)]
    use crate::vspec::cbuf::{CCall, CEntry};
    use core::ffi::c_void;
    #[cfg(verus_keep_ghost)]
    use vstd::prelude::*;
    use vstd::prelude::{Tracked, verus};

    verus! {

    /// The token of a C++ call that hands over the block at `p` to an
    /// `operator delete` ([`CCall`], [`CEntry::Frees`]). Trusted: as
    /// `libc`'s `axiom_c_call`, which this module cannot name. Called only
    /// in the export bodies, once each, for the call's own `p`: checked
    /// by `api::tests::c_call_tokens_only_in_export_bodies`.
    axiom fn axiom_c_call(e: CEntry) -> (tracked c: CCall)
        requires
            e is Frees,
        ensures
            c.fresh(e),
    ;

    /// `operator new(size_t)`
    #[unsafe(export_name = "_Znwm")]
    pub extern "C" fn new(size: usize) -> (r: *mut c_void)
        ensures
            crate::heap::fits(r as *mut u8, size as int, crate::QUANTUM as int),
            r@.addr != 0,
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new(size, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new[](size_t)`
    #[unsafe(export_name = "_Znam")]
    pub extern "C" fn new_array(size: usize) -> (r: *mut c_void)
        ensures
            crate::heap::fits(r as *mut u8, size as int, crate::QUANTUM as int),
            r@.addr != 0,
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new(size, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new(size_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZnwmRKSt9nothrow_t")]
    pub extern "C" fn new_nothrow(size: usize, _tag: *const c_void) -> (r: *mut c_void)
        ensures
            crate::heap::fits(r as *mut u8, size as int, crate::QUANTUM as int),
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new_nothrow(size, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new[](size_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZnamRKSt9nothrow_t")]
    pub extern "C" fn new_array_nothrow(size: usize, _tag: *const c_void) -> (r: *mut c_void)
        ensures
            crate::heap::fits(r as *mut u8, size as int, crate::QUANTUM as int),
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new_nothrow(size, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new(size_t, std::align_val_t)`
    #[unsafe(export_name = "_ZnwmSt11align_val_t")]
    pub extern "C" fn new_aligned(size: usize, align: usize) -> (r: *mut c_void)
        ensures
            crate::meta::is_pow2(align) ==> crate::heap::fits(r as *mut u8, size as int, align as int),
            !crate::meta::is_pow2(align) ==> r@.addr == 0,
            r@.addr != 0,
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new_aligned(size, align, false, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator new[](size_t, std::align_val_t)`
    #[unsafe(export_name = "_ZnamSt11align_val_t")]
    pub extern "C" fn new_array_aligned(size: usize, align: usize) -> (r: *mut c_void)
        ensures
            crate::meta::is_pow2(align) ==> crate::heap::fits(r as *mut u8, size as int, align as int),
            !crate::meta::is_pow2(align) ==> r@.addr == 0,
            r@.addr != 0,
    {
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
    ) -> (r: *mut c_void)
        ensures
            crate::meta::is_pow2(align) ==> crate::heap::fits(r as *mut u8, size as int, align as int),
            !crate::meta::is_pow2(align) ==> r@.addr == 0,
    {
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
    ) -> (r: *mut c_void)
        ensures
            crate::meta::is_pow2(align) ==> crate::heap::fits(r as *mut u8, size as int, align as int),
            !crate::meta::is_pow2(align) ==> r@.addr == 0,
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::new_aligned(size, align, true, Tracked(&mut errno)) as *mut c_void
    }

    /// `operator delete(void*)`
    #[unsafe(export_name = "_ZdlPv")]
    pub extern "C" fn delete(p: *mut c_void)
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete[](void*)`
    #[unsafe(export_name = "_ZdaPv")]
    pub extern "C" fn delete_array(p: *mut c_void)
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete(void*, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZdlPvRKSt9nothrow_t")]
    pub extern "C" fn delete_nothrow(p: *mut c_void, _tag: *const c_void)
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete[](void*, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZdaPvRKSt9nothrow_t")]
    pub extern "C" fn delete_array_nothrow(p: *mut c_void, _tag: *const c_void)
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete(void*, size_t)`
    #[unsafe(export_name = "_ZdlPvm")]
    pub extern "C" fn delete_sized(p: *mut c_void, size: usize)
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
            p@.addr != 0 ==> crate::heap::hint_ok(p as *mut u8, super::libc::spec_class_for(size, 1)),
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete_sized(p as *mut u8, size, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete[](void*, size_t)`
    #[unsafe(export_name = "_ZdaPvm")]
    pub extern "C" fn delete_array_sized(p: *mut c_void, size: usize)
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
            p@.addr != 0 ==> crate::heap::hint_ok(p as *mut u8, super::libc::spec_class_for(size, 1)),
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete_sized(p as *mut u8, size, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete(void*, std::align_val_t)`
    #[unsafe(export_name = "_ZdlPvSt11align_val_t")]
    pub extern "C" fn delete_aligned(p: *mut c_void, _align: usize)
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete[](void*, std::align_val_t)`
    #[unsafe(export_name = "_ZdaPvSt11align_val_t")]
    pub extern "C" fn delete_array_aligned(p: *mut c_void, _align: usize)
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete(void*, std::align_val_t, const std::nothrow_t&)`
    #[unsafe(export_name = "_ZdlPvSt11align_val_tRKSt9nothrow_t")]
    pub extern "C" fn delete_aligned_nothrow(p: *mut c_void, _align: usize, _tag: *const c_void)
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
    {
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
    )
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
    {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::delete(p as *mut u8, Tracked(axiom_c_call(CEntry::Frees { ptr: p as *mut u8 })), Tracked(&mut errno))
    }

    /// `operator delete(void*, size_t, std::align_val_t)`
    #[unsafe(export_name = "_ZdlPvmSt11align_val_t")]
    pub extern "C" fn delete_sized_aligned(p: *mut c_void, size: usize, align: usize)
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
            p@.addr != 0 ==> crate::heap::hint_ok(p as *mut u8, if crate::meta::is_pow2(align) { super::libc::spec_class_for(size, align) } else { None }),
    {
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
    pub extern "C" fn delete_array_sized_aligned(p: *mut c_void, size: usize, align: usize)
        ensures
            p@.addr != 0 ==> crate::heap::free_target(p as *mut u8),
            p@.addr != 0 ==> crate::heap::hint_ok(p as *mut u8, if crate::meta::is_pow2(align) { super::libc::spec_class_for(size, align) } else { None }),
    {
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
