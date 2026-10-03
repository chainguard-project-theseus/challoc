//! Runtime checks that abort.

use vstd::prelude::*;

verus! {

/// `assert!(c, "{}", msg)`: returns only if `c` holds. A failed check is a
/// panic, and challoc aborts on a panic (`panic = "abort"`). For a check of
/// state that verified code cannot constrain, such as a word some other
/// thread may write; a check of the caller's arguments is a `requires`.
/// Such a check is an integrity check (theorem "Read-only APIs never
/// abort"): it needs only `may_abort_corrupt`, and nothing when `c` is
/// known.
#[verifier::external_body]
#[inline(always)]
pub fn check(c: bool, msg: &'static str)
    requires
        crate::sys::may_abort_corrupt() || c,
    ensures
        c,
{
    assert!(c, "{}", msg);
}

} // verus!
