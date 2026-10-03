//! The `errno` model: what the entry points state about the calling
//! thread's `errno` (`verus/THEOREMS.md`, "Interface contract", the F
//! column). One ghost resource, [`ThreadErrno`], stands for the calling
//! thread's `errno` during one C call; [`set_errno`] is the only write and
//! takes it `&mut`.

use core::ffi::c_int;
use vstd::prelude::*;

verus! {

/// Ghost resource: the calling thread's `errno` for the length of one
/// call into challoc. `val()` is the value challoc last wrote to it
/// through [`set_errno`] during the call, or, before any write, the
/// caller's `errno` (unknown).
///
/// A token is made only by [`lend`], which the export wrappers call
/// once each (`api/libc.rs`, `api/cxx.rs`, `api/print.rs`); the only write
/// is [`set_errno`], which takes it `&mut`. So a Rust function that is not
/// handed the token (`free`, the sized frees, the `*allocx` family) cannot
/// write `errno` from verified code, and one that is handed it states its
/// final value in its postcondition (the `operator new` paths only pass
/// it on).
///
/// The token models challoc's own writes. libc calls inside trusted code
/// may also write `errno` (`getauxval`, `pthread_key_create` and
/// `pthread_atfork` during init, `pthread_setspecific` at heap creation,
/// `pthread_attr_*`, `pthread_create` and `pthread_join` in
/// `purge::background::start`, `fwrite` in `malloc_info`'s sink), and no
/// token stands for those. Every failure path ends with its
/// [`set_errno`] (its failure result, a null, `-1` or the error code, is
/// returned right after), so a stated value holds at return. That a
/// function without the token leaves `errno` alone needs the further
/// premise that its path makes no such libc write, or restores it
/// (`background::start` saves and restores `errno` around its calls).
pub tracked struct ThreadErrno {
    ghost val: c_int,
}

impl ThreadErrno {
    /// The value of `errno` as challoc last wrote it during this call.
    pub closed spec fn val(&self) -> c_int {
        self.val
    }
}

/// Trusted: lends the calling thread's `errno` to one call into challoc,
/// with its value unknown. Called only by the export wrappers, once each,
/// before the call that may write it. A nested call (an allocation from
/// libc or from a C++ `new_handler` during the outer one) gets its own
/// token from its own wrapper; an outer claim is about challoc's last
/// write at the outer return. No code: the token is ghost.
#[verifier::external_body]
#[inline(always)]
pub fn lend() -> (e: Tracked<ThreadErrno>) {
    Tracked::assume_new()
}

/// Trusted: writes `e` to the calling thread's `errno` through libc's
/// pointer.
#[verifier::external_body]
#[inline(always)]
pub fn set_errno(e: c_int, Tracked(errno): Tracked<&mut ThreadErrno>)
    ensures
        final(errno).val() == e,
{
    // SAFETY: `__errno_location` returns the calling thread's `errno`.
    unsafe { *::libc::__errno_location() = e };
}

} // verus!
