//! `core::slice` functions.

use vstd::prelude::*;

verus! {

/// `a.get_unchecked(i)` on an array, for the fast paths: vstd's
/// array-to-slice coercion (`vstd::array::array_as_slice`) declares Verus's
/// default invariant mask, `any`, which a function that opens none cannot
/// call. Trusted: the element at an index in bounds, which is
/// `get_unchecked`'s safety contract.
#[inline(always)]
#[verifier::external_body]
pub unsafe fn array_get_unchecked<T, const N: usize>(a: &[T; N], i: usize) -> (r: &T)
    requires
        i < N,
    ensures
        *r == a[i as int],
    opens_invariants none
    no_unwind
{
    // SAFETY: `i < N` (the caller's contract).
    unsafe { a.get_unchecked(i) }
}

} // verus!
