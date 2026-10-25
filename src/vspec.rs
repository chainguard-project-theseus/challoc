//! Specifications of the `core` functions the verified code calls that
//! vstd does not specify (`verus/README.md`, "What counts as verified").
//! Each is a seam: Verus takes it on trust, so each states only what the
//! `core` documentation promises. Verus alone compiles this module.

use vstd::prelude::*;

verus! {

// challoc runs on x86_64 and aarch64 only (its TLS, rseq and syscall asm),
// where `usize` is 8 bytes; Verus checks the size when it compiles the
// crate. `Curve::allowance` casts a `u64` to `usize`.
global size_of usize == 8;

/// `u64::div_ceil`: the quotient rounded up; panics when `rhs == 0`
/// (core docs), and cannot overflow otherwise.
pub assume_specification[<u64>::div_ceil](lhs: u64, rhs: u64) -> (q: u64)
    requires
        rhs > 0,
    ensures
        q == (lhs + rhs - 1) / rhs as int,
;

} // verus!
