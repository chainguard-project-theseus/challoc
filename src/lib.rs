//! challoc: a C-ABI malloc replacement for Linux, written in `no_std` Rust.
//!
//! See `docs/design.md` for the design.
//! Consumers are C programs; every entry point is a top-level
//! `pub extern "C"` function. The crate never allocates through anything but
//! its own machinery and never unwinds: `panic = "abort"` in every profile.
//! The unit tests (`cfg(test)`) link `std` for the test harness; the
//! library itself never does.

#![no_std]
#![deny(warnings)]
#![deny(unsafe_op_in_unsafe_fn)]
// Kani checks the exported `malloc` family against the libc signatures
// `std` links to; challoc defines them, as it must.
#![cfg_attr(kani, allow(suspicious_runtime_symbol_definitions))]

#[cfg(test)]
extern crate std;

// Verus needs its prelude imported at the crate root (`verus/README.md`).
#[allow(unused_imports)]
use vstd::prelude::*;

#[cfg(all(feature = "quantum8", feature = "quantum16"))]
compile_error!("features `quantum8` and `quantum16` are mutually exclusive");

#[cfg(not(any(feature = "quantum8", feature = "quantum16")))]
compile_error!("exactly one of the `quantum8` or `quantum16` features must be enabled");

// The proofs take `usize` as 8 bytes (`global size_of usize == 8` in
// `vspec`): x86_64 and aarch64 are 64-bit, and the only targets with the
// asm the crate needs (TLS, rseq, syscalls).
#[cfg(not(target_pointer_width = "64"))]
compile_error!("challoc needs a 64-bit target");

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("challoc supports only x86_64 and aarch64");

verus! {

/// Quantum (minimum size-class granularity and alignment) in bytes, selected
/// at build time by the `quantum8` / `quantum16` features.
#[cfg(feature = "quantum16")]
pub const QUANTUM: usize = 16;
#[cfg(feature = "quantum8")]
pub const QUANTUM: usize = 8;

} // verus!

pub(crate) mod api;
pub(crate) mod arena;
pub(crate) mod bitmap;
pub(crate) mod class;
pub(crate) mod config;
pub(crate) mod heap;
pub(crate) mod huge;
pub(crate) mod layout;
pub(crate) mod lock;
pub(crate) mod meta;
#[cfg(test)]
mod model;
pub(crate) mod policy;
pub(crate) mod purge;
pub(crate) mod rseq;
pub(crate) mod slab;
pub(crate) mod stats;
pub(crate) mod sys;
#[cfg(test)]
mod testutil;
pub(crate) mod thp;
// Specifications of `core` functions vstd lacks, and the trusted wrappers
// the executable code calls for operations Verus does not model
// (`verus/README.md`). Ordinary builds compile the wrappers; the
// specifications are ghost and vanish there, and the expansion of
// `global` is a braced block.
#[allow(unused_braces)]
mod vspec;
// Kani harnesses (`src/kani/`, `.chainguard/ci/kani.yaml`); never part of
// an ordinary build.
#[cfg(kani)]
#[path = "kani/mod.rs"]
mod kani_proofs;

verus! {

/// Panic handler for the `no_std` library. The crate is built with
/// `panic = "abort"`, so this is only reached by an explicit `panic!` or a
/// failed debug assertion. It reports through `sys::abort_fmt`, which writes
/// to stderr with `write(2)` and raises `SIGABRT`, never unwinding. The
/// line reads `challoc: <message> [<file>:<line>]`: the message first, since
/// it is what a user sees when their program frees a pointer twice. Not
/// under Kani, whose harnesses link `std` and its handler.
#[cfg(not(any(test, kani)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    proof {
        // A root of the call graph: the panic runtime calls it, nothing
        // in the crate does.
        crate::sys::grant_aborts();
        broadcast use crate::vspec::panic::axiom_fmt_req_all_panic_message;
    }
    match info.location() {
        Some(loc) => sys::abort_fmt(format_args!(
            "{} [{}:{}]",
            info.message(),
            loc.file(),
            loc.line()
        )),
        None => sys::abort_fmt(format_args!("{}", info.message())),
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::QUANTUM;

    #[test]
    // The constant checks are the test.
    #[allow(clippy::assertions_on_constants)]
    fn quantum_is_a_power_of_two() {
        assert!(QUANTUM.is_power_of_two());
        assert!(QUANTUM == 8 || QUANTUM == 16);
    }
}
