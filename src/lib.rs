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
// The `cfg(loom)` build is cut down to the modules its tests check
// (`loom_tests`), which leaves the rest of their API unused.
#![cfg_attr(loom, allow(dead_code))]

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
// `vspec`) and every user address as below 2^57 (the slab-table and huge
// region bounds): x86_64 (five-level paging tops out at 57 bits) and
// aarch64 (52 bits) are the targets those hold for, and the only ones with
// the asm the crate needs (TLS, rseq, syscalls).
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

// `cfg(loom)` (`loom_tests`, `verus/WEAKMEM.md`) builds only the modules
// whose real code the loom tests check, over loom's atomics: the rest
// build their statics in `const` context, which loom's atomics cannot be.
#[cfg(not(loom))]
pub(crate) mod api;
#[cfg(not(loom))]
pub(crate) mod arena;
pub(crate) mod bitmap;
#[cfg(not(loom))]
pub(crate) mod class;
#[cfg(not(loom))]
pub(crate) mod config;
#[cfg(not(loom))]
pub(crate) mod heap;
#[cfg(not(loom))]
pub(crate) mod huge;
#[cfg(not(loom))]
pub(crate) mod layout;
pub(crate) mod lock;
#[cfg(loom)]
#[cfg(test)]
mod loom_tests;
#[cfg(not(loom))]
pub(crate) mod meta;
#[cfg(not(loom))]
#[cfg(test)]
mod model;
#[cfg(not(loom))]
pub(crate) mod policy;
#[cfg(not(loom))]
pub(crate) mod purge;
#[cfg(not(loom))]
pub(crate) mod rseq;
#[cfg(not(loom))]
pub(crate) mod slab;
#[cfg(not(loom))]
pub(crate) mod stats;
pub(crate) mod sync;
#[cfg(not(loom))]
pub(crate) mod sys;
#[cfg(not(loom))]
#[cfg(test)]
mod testutil;
#[cfg(not(loom))]
pub(crate) mod thp;
#[allow(dead_code)]
mod vspec;

/// Panic handler for the `no_std` library. The crate is built with
/// `panic = "abort"`, so this is only reached by an explicit `panic!` or a
/// failed debug assertion. It reports through `sys::abort_fmt`, which writes
/// to stderr with `write(2)` and raises `SIGABRT`, never unwinding. The
/// line reads `challoc: <message> [<file>:<line>]`: the message first, since
/// it is what a user sees when their program frees a pointer twice.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
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

#[cfg(test)]
mod tests {
    use super::QUANTUM;

    #[test]
    fn quantum_is_a_power_of_two() {
        assert!(QUANTUM.is_power_of_two());
        assert!(QUANTUM == 8 || QUANTUM == 16);
    }
}
