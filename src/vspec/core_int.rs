//! Integer methods of `core` that vstd does not specify, stated through
//! vstd's `u64` specifications (`usize` is 64 bits wide on every target
//! challoc builds for; see `mod.rs`).

use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;
use vstd::std_specs::bits::{u64_leading_zeros, u64_trailing_zeros};

verus! {

/// Number of set bits.
pub open spec fn popcount(x: u64) -> nat
    decreases x,
{
    if x == 0 { 0 } else { (x % 2) as nat + popcount(x / 2) }
}

/// `u64::count_ones`: the number of set bits.
pub assume_specification[ u64::count_ones ](x: u64) -> (r: u32)
    ensures r as nat == popcount(x),
;

/// `usize::div_ceil`: the quotient rounded up; panics when `d == 0`
/// (core docs), and cannot overflow otherwise.
pub assume_specification[ usize::div_ceil ](x: usize, d: usize) -> (r: usize)
    requires d > 0,
    ensures r as int == (x as int + d as int - 1) / d as int,
;

pub assume_specification[ usize::trailing_zeros ](x: usize) -> (r: u32)
    ensures r == u64_trailing_zeros(x as u64),
;

pub assume_specification[ usize::leading_zeros ](x: usize) -> (r: u32)
    ensures r as int == u64_leading_zeros(x as u64),
    opens_invariants none
;

pub assume_specification[ usize::is_power_of_two ](x: usize) -> (r: bool)
    ensures r <==> exists|k: nat| k < 64 && x as nat == #[trigger] pow2(k),
;

pub assume_specification[ u64::is_power_of_two ](x: u64) -> (r: bool)
    ensures r <==> exists|k: nat| k < 64 && x as nat == #[trigger] pow2(k),
;

/// `u32::is_power_of_two`.
pub assume_specification[ u32::is_power_of_two ](x: u32) -> (r: bool)
    ensures r <==> exists|k: nat| k < 32 && x as nat == #[trigger] pow2(k),
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

/// `i64::wrapping_neg`: `-x`, except that `i64::MIN` is its own negation.
pub assume_specification[ i64::wrapping_neg ](x: i64) -> (r: i64)
    ensures r == if x == i64::MIN { i64::MIN as int } else { -x },
;

/// `usize::ilog2`: the position of the highest set bit; panics for 0.
pub assume_specification[ usize::ilog2 ](x: usize) -> (r: u32)
    requires x > 0,
    ensures r < 64, pow2(r as nat) <= x, x < pow2(r as nat + 1),
;

/// `x` rotated right by `n` bits (`u32::rotate_right`).
pub open spec fn rotr32(x: u32, n: u32) -> u32 {
    if n % 32 == 0 {
        x
    } else {
        (x >> (n % 32)) | (x << ((32 - n % 32) as u32))
    }
}

/// `u32::rotate_right`: the bits shifted out at the bottom come back in at
/// the top.
pub assume_specification[ u32::rotate_right ](x: u32, n: u32) -> (r: u32)
    ensures
        r == rotr32(x, n),
    opens_invariants none
;

/// `x` rotated right by `n` bits (`u64::rotate_right`), the count taken
/// modulo 64.
pub open spec fn rotr64(x: u64, n: u32) -> u64 {
    if n % 64 == 0 {
        x
    } else {
        (x >> ((n % 64) as u64)) | (x << ((64 - n % 64) as u64))
    }
}

/// `u64::rotate_right`: the bits shifted out at the bottom come back in at
/// the top (the count is taken modulo 64, as `core` documents).
pub assume_specification[ u64::rotate_right ](x: u64, n: u32) -> (r: u64)
    ensures
        r == rotr64(x, n),
    opens_invariants none
;

} // verus!

verus! {

/// A spin-wait hint to the CPU (`pause`, `isb`); no effect on memory.
pub assume_specification[ core::hint::spin_loop ]();

} // verus!
