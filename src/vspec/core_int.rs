//! `usize` bit and rounding methods, stated through vstd's `u64`
//! specifications (`usize` is 64 bits wide on every target challoc builds
//! for; see `mod.rs`).

use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;
use vstd::std_specs::bits::{u64_leading_zeros, u64_trailing_zeros};

verus! {

pub assume_specification[ usize::trailing_zeros ](x: usize) -> (r: u32)
    ensures r == u64_trailing_zeros(x as u64),
;

pub assume_specification[ usize::leading_zeros ](x: usize) -> (r: u32)
    ensures r as int == u64_leading_zeros(x as u64),
    opens_invariants none
;

/// `next_power_of_two` panics (debug) or wraps (release) above `2^63`;
/// challoc never asks for that.
pub assume_specification[ usize::next_power_of_two ](x: usize) -> (r: usize)
    requires x <= 0x8000_0000_0000_0000,
    ensures
        exists|k: nat| k < 64 && r as nat == #[trigger] pow2(k),
        x <= r,
        x <= 1 ==> r == 1,
        x > 1 ==> r < 2 * x,
;

} // verus!
