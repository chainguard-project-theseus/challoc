//! Specifications for `config.rs`: `i64::wrapping_neg` and `usize::ilog2`.

#[cfg(verus_only)]
use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;

verus! {

/// `i64::wrapping_neg`: `-x`, except that `i64::MIN` is its own negation.
pub assume_specification[ i64::wrapping_neg ](x: i64) -> (r: i64)
    ensures r == if x == i64::MIN { i64::MIN as int } else { -x },
;

/// `usize::ilog2`: the position of the highest set bit; panics for 0.
pub assume_specification[ usize::ilog2 ](x: usize) -> (r: u32)
    requires x > 0,
    ensures r < 64, pow2(r as nat) <= x, x < pow2(r as nat + 1),
;


} // verus!
