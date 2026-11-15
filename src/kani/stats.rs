//! The statistics sums `stats::fig_add`, `fig_mul` and `fig_add64`, which
//! Verus verifies and which wrap in every build. The contracts claim the
//! exact result only when it fits; these harnesses check that again for
//! every pair of inputs, against `u128` arithmetic.

use crate::stats::{fig_add, fig_add64, fig_mul};

/// `fig_add`: `a + b <= usize::MAX ==> r == a + b`.
#[kani::proof]
fn fig_add_exact() {
    let a: usize = kani::any();
    let b: usize = kani::any();
    kani::assume(a as u128 + b as u128 <= usize::MAX as u128);
    assert_eq!(fig_add(a, b) as u128, a as u128 + b as u128);
}

/// `fig_mul`: `a * b <= usize::MAX ==> r == a * b`.
#[kani::proof]
#[kani::solver(kissat)]
fn fig_mul_exact() {
    let a: usize = kani::any();
    let b: usize = kani::any();
    kani::assume(a as u128 * b as u128 <= usize::MAX as u128);
    assert_eq!(fig_mul(a, b) as u128, a as u128 * b as u128);
}

/// `fig_add64`: `a + b <= u64::MAX ==> r == a + b`.
#[kani::proof]
fn fig_add64_exact() {
    let a: u64 = kani::any();
    let b: u64 = kani::any();
    kani::assume(a as u128 + b as u128 <= u64::MAX as u128);
    assert_eq!(fig_add64(a, b) as u128, a as u128 + b as u128);
}
