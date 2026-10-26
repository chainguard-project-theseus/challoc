//! Specifications of the `core` functions the verified code calls that
//! vstd does not specify (`verus/README.md`, "What counts as verified"),
//! and trusted wrappers for operations Verus does not model. Each is a
//! seam (`verus/SEAMS.md`): Verus takes it on trust, so each states only
//! what the `core` documentation promises. Ordinary builds compile the
//! wrappers; the specifications are ghost and vanish there.

use vstd::prelude::*;

pub(crate) mod addr;
// Specifications only: Verus alone compiles these.
#[cfg(verus_keep_ghost)]
pub(crate) mod class;
#[cfg(verus_keep_ghost)]
pub(crate) mod core_int;
#[cfg(verus_keep_ghost)]
pub(crate) mod layout;
#[cfg(verus_keep_ghost)]
pub(crate) mod ptr;

pub(crate) mod slice;
pub(crate) mod word;

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
