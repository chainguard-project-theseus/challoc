//! Runtime checks that abort.

use vstd::prelude::*;

verus! {

/// `assert!(c, "{}", msg)`: returns only if `c` holds. A failed check is a
/// panic, and challoc aborts on a panic (`panic = "abort"`). For a check of
/// state that verified code cannot constrain, such as a word some other
/// thread may write; a check of the caller's arguments is a `requires`.
#[verifier::external_body]
#[inline(always)]
pub fn check(c: bool, msg: &'static str)
    ensures
        c,
{
    assert!(c, "{}", msg);
}

} // verus!
