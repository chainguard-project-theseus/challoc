//! The statistics sums the proofs take on trust: `stats::fig_add`,
//! `fig_mul`, `fig_add64` and `vspec::print::stats_allocated`. Kani builds
//! with overflow checks on, as the tests do: there an overflowing sum
//! panics (and challoc's panics abort), and the shipped library, built
//! without them, wraps. The contracts claim the exact result only when it
//! fits, which is what the harnesses check, for every pair of inputs.

use crate::stats::{ArenaStats, fig_add, fig_add64, fig_mul};
use crate::vspec::print::stats_allocated;

/// S209 `fig_add`: `a + b <= usize::MAX ==> r == a + b`.
#[kani::proof]
fn fig_add_exact() {
    let a: usize = kani::any();
    let b: usize = kani::any();
    kani::assume(a as u128 + b as u128 <= usize::MAX as u128);
    assert_eq!(fig_add(a, b) as u128, a as u128 + b as u128);
}

/// S210 `fig_mul`: `a * b <= usize::MAX ==> r == a * b`.
#[kani::proof]
#[kani::solver(kissat)]
fn fig_mul_exact() {
    let a: usize = kani::any();
    let b: usize = kani::any();
    kani::assume(a as u128 * b as u128 <= usize::MAX as u128);
    assert_eq!(fig_mul(a, b) as u128, a as u128 * b as u128);
}

/// S211 `fig_add64`: `a + b <= u64::MAX ==> r == a + b`.
#[kani::proof]
fn fig_add64_exact() {
    let a: u64 = kani::any();
    let b: u64 = kani::any();
    kani::assume(a as u128 + b as u128 <= u64::MAX as u128);
    assert_eq!(fig_add64(a, b) as u128, a as u128 + b as u128);
}

/// S346 `stats_allocated`: `small + large <= usize::MAX ==> r == small +
/// large`, for every pair (the fields it reads are arbitrary; the others
/// are zero, and `ArenaStats::allocated` reads only those two).
#[kani::proof]
fn stats_allocated_returns() {
    let mut s = ArenaStats::empty(kani::any());
    s.small_allocated = kani::any();
    s.large_allocated = kani::any();
    kani::assume(s.small_allocated as u128 + s.large_allocated as u128 <= usize::MAX as u128);
    assert_eq!(stats_allocated(&s), s.small_allocated + s.large_allocated);
}
