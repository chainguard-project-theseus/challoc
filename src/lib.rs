//! challoc: a C-ABI malloc replacement for Linux, written in `no_std` Rust.
//!
//! See `docs/design.md` for the design and `docs/plan.md` for the task list.
//! Consumers are C programs; every entry point is a top-level
//! `pub extern "C"` function. The crate never allocates through anything but
//! its own machinery and never unwinds: `panic = "abort"` in every profile.
//! The unit tests (`cfg(test)`) link `std` for the test harness; the
//! library itself never does.

#![no_std]
#![deny(warnings)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(test)]
extern crate std;

#[cfg(all(feature = "quantum8", feature = "quantum16"))]
compile_error!("features `quantum8` and `quantum16` are mutually exclusive");

#[cfg(not(any(feature = "quantum8", feature = "quantum16")))]
compile_error!("exactly one of the `quantum8` or `quantum16` features must be enabled");

/// Quantum (minimum size-class granularity and alignment) in bytes, selected
/// at build time by the `quantum8` / `quantum16` features.
#[cfg(feature = "quantum16")]
pub const QUANTUM: usize = 16;
#[cfg(feature = "quantum8")]
pub const QUANTUM: usize = 8;

pub mod api;
pub mod arena;
pub mod bitmap;
pub mod class;
pub mod config;
pub mod heap;
pub mod huge;
pub mod layout;
pub mod lock;
pub mod meta;
#[cfg(test)]
mod model;
pub mod policy;
pub mod purge;
pub mod rseq;
pub mod slab;
pub mod slice;
pub mod stats;
pub mod sys;
#[cfg(test)]
mod testutil;
pub mod thp;

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
        Some(loc) => sys::abort_fmt(format_args!("{} [{}:{}]", info.message(), loc.file(), loc.line())),
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
