//! `core` specifications for the purge and cgroup policy code
//! (`policy.rs`, `purge.rs`, `purge/`, `class/decay.rs`).

use vstd::prelude::*;

verus! {

/// `u64::div_ceil`: the quotient rounded up; panics when `rhs == 0`
/// (core docs), and cannot overflow otherwise.
pub assume_specification[<u64>::div_ceil](lhs: u64, rhs: u64) -> (q: u64)
    requires
        rhs > 0,
    ensures
        q == (lhs + rhs - 1) / rhs as int,
;

} // verus!
