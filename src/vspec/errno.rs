//! The `errno` model: one ghost resource, [`ThreadErrno`], stands for the
//! calling thread's `errno` during one C call; [`set_errno`] is the only
//! write and takes it `&mut`.

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
