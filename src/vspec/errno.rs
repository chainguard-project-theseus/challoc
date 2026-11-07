//! The `errno` model: one ghost resource, [`ThreadErrno`], stands for the
//! calling thread's `errno` during one C call; [`set_errno`] is the only
//! write through a token and takes it `&mut`, and [`get_errno`] the only
//! read through one. At an export's return, [`axiom_errno_at_return`]
//! uses the token up and names its value as the `errno` the C caller
//! reads ([`c_errno`]).

use core::ffi::c_int;
use vstd::prelude::*;

unsafe extern "C" {
    /// glibc's `__errno_location`, with the `libc` crate's signature and
    /// the same symbol (premise C11 of `verus/kani/premises.md`). Declared
    /// in this crate rather than taken from the `libc` crate so that
    /// `kani_errno` can stub it: Kani 0.68 stubs a foreign function only
    /// where this crate declares it, and silently ignores a stub on
    /// `libc::__errno_location`.
    fn __errno_location() -> *mut c_int;
}

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
/// code: the token is ghost. The wrappers are checked by
/// `api::tests::c_call_tokens_only_in_export_bodies`.
#[verifier::external_body]
#[inline(always)]
#[cfg_attr(test, allow(dead_code))]
pub fn lend() -> (e: Tracked<ThreadErrno>) {
    Tracked::assume_new()
}

/// The calling thread's `errno` as the C program reads it once the
/// export being verified has returned: the value
/// [`axiom_errno_at_return`] fixes from that export's token. It takes no
/// arguments because it names one return, the one of the function whose
/// `ensures` mention it: each export is verified on its own, calls the
/// axiom at most once, and no verified code calls an export, so no proof
/// sees two returns.
pub uninterp spec fn c_errno() -> c_int;

/// Trusted (the C-program contract of `errno`): when an export returns
/// and `failed` holds, the `errno` its C caller reads ([`c_errno`]) is
/// the value of the token the export lent ([`lend`]), as the token stands
/// after its last use. The token is used up, so nothing writes through it
/// afterwards.
///
/// `failed` is the export's own condition for a result that the entry
/// point returns right after challoc's last `errno` write: a null result
/// (`api::libc`'s `enomem` and `einval`), except a `realloc` (or
/// `reallocarray`) of a block to size 0, and `posix_memalign`'s
/// `ENOMEM`. Under it the claim holds. Without it the claim can fail: a
/// libc call in trusted code may write `errno` while the token keeps its
/// value ([`ThreadErrno`]; `pthread_setspecific` failing when a call
/// creates its thread's heap, [`get_errno`]'s doc), and then the C caller
/// reads that write. A `realloc` to size 0 is such a call: it frees the
/// block, the free may create the thread's heap, and it returns null
/// with no `errno` write of challoc's after that. Verus proves an
/// export's `c_errno()` clauses only where its `failed` holds, so a
/// clause about a success path, or that `errno` is unchanged after a
/// `realloc` to size 0, does not verify. Verus does not check `failed`
/// itself: an export that passed `true` would get the claim on every
/// path. The shape test requires each export's own condition, as the
/// exports spell them.
///
/// It lets an export's `ensures` restate its callee's `errno` claims,
/// which name the lent token, in terms the C caller sees: a nullable
/// export that returns null where its callee returned a block cannot
/// prove `c_errno() == ENOMEM` from the callee's `ensures`. Verus does
/// not stop the export from writing the token itself before the axiom;
/// the shape test does. Called only by the `extern "C"` exports of
/// `api::libc`, at most once each, on the token it lent, right after the
/// call whose result it returns, and only exports lend tokens
/// (`api::tests::exports_return_their_callees_result` checks the shape,
/// the call sites and each export's `failed`).
pub axiom fn axiom_errno_at_return(tracked e: ThreadErrno, failed: bool)
    ensures
        failed ==> c_errno() == e.val(),
;

/// Trusted: writes `e` to the calling thread's `errno` through libc's
/// pointer. `kani_errno` checks the write on premise C11 of
/// `verus/kani/premises.md`.
#[verifier::external_body]
#[inline(always)]
pub fn set_errno(e: c_int, Tracked(errno): Tracked<&mut ThreadErrno>)
    ensures
        final(errno).val() == e,
{
    // SAFETY: `__errno_location` returns the calling thread's `errno`.
    unsafe { *__errno_location() = e };
}

/// Trusted: reads the calling thread's `errno` through libc's pointer,
/// and claims the value is the token's ([`ThreadErrno::val`]): challoc's
/// last write through it, or the caller's `errno` before any. That is an
/// assumption: no libc call in trusted code, and no other code on the
/// thread (a signal handler that does not restore it), has written
/// `errno` since. `kani_errno` checks the read itself on premise C11 of
/// `verus/kani/premises.md`.
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
/// `pthread_key_create` and `pthread_atfork` (`atfork_handlers`), which
/// return their error instead of writing `errno`; a failed registration
/// aborts. A later trusted seam on an
/// allocation path that calls libc must take the token `&mut` and state
/// nothing about its value.
#[verifier::external_body]
#[inline(always)]
pub fn get_errno(Tracked(errno): Tracked<&ThreadErrno>) -> (r: c_int)
    ensures
        r == errno.val(),
{
    // SAFETY: `__errno_location` returns the calling thread's `errno`.
    unsafe { *__errno_location() }
}

} // verus!

/// Kani checks of `set_errno` and `get_errno`
/// (`verus/ledger/src/vspec/errno.rs.md`; `cfg(kani)` only, see
/// `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_errno {
    use super::{get_errno, set_errno};
    use core::ffi::c_int;
    use vstd::prelude::Tracked;

    /// The `errno` the stand-in for `__errno_location` gives the address
    /// of, and how many times it was called.
    static mut ERRNO: c_int = 0;
    static mut CALLS: usize = 0;

    /// A stand-in for glibc's `__errno_location` (premise C11): it records
    /// its call and returns the address of `ERRNO`.
    #[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
    unsafe extern "C" fn errno_location_model() -> *mut c_int {
        // SAFETY: the harness's own static; no other code runs in a
        // harness.
        unsafe {
            CALLS += 1;
            &raw mut ERRNO
        }
    }

    /// `set_errno`: its `ensures` is that the token's value is `e`; the
    /// fact it stands for is that the thread's `errno` reads `e`. For any
    /// earlier `errno` and any `e`, it calls `__errno_location` once and
    /// leaves `e` in the `int` whose address that returned.
    #[kani::proof]
    #[kani::stub(super::__errno_location, errno_location_model)]
    fn set_errno_writes_errno() {
        let before: c_int = kani::any();
        let e: c_int = kani::any();
        kani::cover!(before != e);
        // SAFETY: as in `errno_location_model`.
        unsafe { ERRNO = before };
        set_errno(e, Tracked::assume_new());
        // SAFETY: as in `errno_location_model`.
        let (calls, errno) = unsafe { (CALLS, ERRNO) };
        assert_eq!(calls, 1);
        assert_eq!(errno, e);
    }

    /// `get_errno`: its `ensures` is that the result is the token's value;
    /// what a harness can show of it is that, for any `errno`, it calls
    /// `__errno_location` once and returns the `int` at the address that
    /// returned, leaving it unchanged.
    #[kani::proof]
    #[kani::stub(super::__errno_location, errno_location_model)]
    fn get_errno_reads_errno() {
        let v: c_int = kani::any();
        // SAFETY: as in `errno_location_model`.
        unsafe { ERRNO = v };
        let r = get_errno(Tracked::assume_new());
        // SAFETY: as in `errno_location_model`.
        let (calls, errno) = unsafe { (CALLS, ERRNO) };
        assert_eq!(calls, 1);
        assert_eq!(r, v);
        assert_eq!(errno, v);
    }
}
