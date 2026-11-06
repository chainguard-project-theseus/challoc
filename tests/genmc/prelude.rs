// The start of every harness (`run.sh` writes each case's `main.rs` as
// `#![no_main]`, this file, the case's constants, then its template).
// challoc's `src/sync.rs` and the `vspec` wrappers, compiled unchanged
// over `core`'s atomics, as the release build compiles them. The
// wrapper modules are the loom crate's `vspec.rs` (its `#[path]`s
// resolve from its own directory), so both crates compile one list.
#[path = "../../src/sync.rs"]
mod sync;
#[allow(dead_code)]
#[path = "../loom/src/vspec.rs"]
mod vspec;

// GenMC's Rust front end finds `main` behind `std::rt::lang_start` only
// in unoptimised IR, and the harnesses are compiled at `-Copt-level=1`
// (`run.sh`), so the entry point is a C `main`.
#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    harness();
    0
}

// GenMC reports a call of `__VERIFIER_assert_fail` (genmc-std defines it)
// as a safety violation carrying the message. `gassert!` is `assert!`
// over it: `core`'s panic machinery is precompiled, not in the IR GenMC
// interprets, so a harness never panics on purpose.
unsafe extern "C" {
    fn __VERIFIER_assert_fail(msg: *const u8, file: *const u8, line: i32);
}
macro_rules! gassert {
    ($c:expr, $m:literal) => {
        if !$c {
            // SAFETY: both strings are NUL-terminated literals, as
            // `__VERIFIER_assert_fail` reads them.
            unsafe {
                crate::__VERIFIER_assert_fail(
                    concat!($m, "\0").as_ptr(),
                    concat!(file!(), "\0").as_ptr(),
                    line!() as i32,
                )
            }
        }
    };
}
