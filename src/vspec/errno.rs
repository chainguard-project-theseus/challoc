//! The `errno` model: one ghost resource, [`ThreadErrno`], stands for the
//! calling thread's `errno` during one C call; [`set_errno`] is the only
//! write through a token and takes it `&mut`, and [`get_errno`] the only
//! read through one.

use core::ffi::c_int;
use vstd::prelude::*;

verus! {

/// Ghost resource: the calling thread's `errno` for the length of one
/// call into challoc. `val()` is the value challoc last wrote to it
/// through [`set_errno`] with this token, or, before any write, the
/// caller's `errno` (unknown).
///
/// The only write is [`set_errno`], which takes it `&mut`. The token
/// models challoc's own writes: libc calls inside trusted code may also
/// write `errno`, and no token stands for those.
// Ghost: ordinary builds keep the type but nothing reads its field.
#[allow(dead_code)]
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
/// libc during the outer one) gets its own token from its own wrapper;
/// an outer claim is about challoc's last write at the outer return. No
/// code: the token is ghost.
#[verifier::external_body]
#[inline(always)]
#[cfg_attr(test, allow(dead_code))]
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

/// Trusted: reads the calling thread's `errno` through libc's pointer.
/// Nothing is claimed about the value: libc calls in trusted code may
/// have written `errno` since challoc's last write, which `val()` does
/// not track.
#[verifier::external_body]
#[inline(always)]
pub fn get_errno(Tracked(errno): Tracked<&ThreadErrno>) -> c_int {
    // SAFETY: `__errno_location` returns the calling thread's `errno`.
    unsafe { *::libc::__errno_location() }
}

} // verus!
