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

/// Trusted: reads the calling thread's `errno` through libc's pointer,
/// and claims the value is the token's ([`ThreadErrno::val`]): challoc's
/// last write through it, or the caller's `errno` before any. That is an
/// assumption: no libc call in trusted code has written `errno` since.
/// The one caller, [`crate::purge::background::start`], reads it before
/// the thread creation (`pthread_create`, `pthread_join`) and writes it
/// back. Only an allocation's tick and `mallctl("background_thread")`
/// reach it; a free's tick never starts the thread. On an allocation
/// path two libc calls can come before it in the same C call:
/// `pthread_setspecific`, when the call creates the thread's heap
/// (`ThreadKey::set`), which glibc lets write `errno` only when the
/// registration fails, and then the call is served without a heap and
/// never ticks; and `getauxval(AT_PAGESZ)` in `purge::discard`, which
/// writes `errno` only for an absent key, while `getauxval`'s trusted
/// spec already says the key is present. Init, which runs in the
/// process's first C call (an allocation or `mallctl`), adds `getauxval`
/// of `AT_PAGESZ` and `AT_SECURE`, keys Linux always passes, and
/// `pthread_key_create`, which returns its error instead of writing
/// `errno`. A later trusted seam on an
/// allocation path that calls libc must take the token `&mut` and state
/// nothing about its value.
#[verifier::external_body]
#[inline(always)]
pub fn get_errno(Tracked(errno): Tracked<&ThreadErrno>) -> (r: c_int)
    ensures
        r == errno.val(),
{
    // SAFETY: `__errno_location` returns the calling thread's `errno`.
    unsafe { *::libc::__errno_location() }
}

} // verus!
