//! Kani harnesses for the seams of `verus/SEAMS.md` whose tool is Kani
//! (`cfg(kani)` only: ordinary builds and Verus never see this module).
//! Harnesses for private items sit next to them, in `kani_*` modules;
//! the ones here check `core` and crate-visible items. Each harness's
//! comment names the seams it checks, and each discharged seam has its
//! record under `verus/seams/`. `sh verus/kani/run.sh` runs them all;
//! `docs/testing.md` has the details.

mod abort;
mod core_int;
mod eq;
pub(crate) mod fmt;
mod misc;
mod report;
mod slice;
mod stats;

/// The end of a `#[kani::should_panic]` harness on the inputs for which
/// the call before it must not return. Kani counts a failed
/// `kani::assert` or `panic!` as the expected panic, and passes a
/// `should_panic` harness when any one path panics; reaching
/// `unreachable_unchecked` is a failure of another kind ("unreachable
/// code"), so the harness fails if the call returned on any such input.
pub(crate) fn must_not_return() -> ! {
    // SAFETY: none needed: Kani reports reaching this as a failure, and
    // nothing but a harness calls it.
    unsafe { core::hint::unreachable_unchecked() }
}
