//! Kani harnesses for seams of `verus/ledger/` (`cfg(kani)` only:
//! ordinary builds, the tests and Verus never see this module). Each
//! harness checks a trusted item's body, or the `core` function an
//! `assume_specification` describes, against the contract Verus assumes,
//! for every input of its instantiated types or within the bounds its
//! comment states. Its comment names the item, as its ledger entry does.
//! A harness is evidence for its seam; it changes no ledger status.
//! `.chainguard/ci/kani.yaml` runs them all, under both quanta, on pull
//! requests that change `src/kani/`, `src/vspec/` or that file. A change
//! to an item a harness checks (`stats.rs`'s `fig_*`, `sys/abort.rs`, the
//! class table, the enums `eq.rs` matches) does not run the check; the
//! next pull request that touches `src/kani/` runs it and meets any
//! harness that change broke.
//!
//! Not every seam the ledger gives to Kani has a harness here. Those of
//! private items and of the layout constants (`arena.rs`'s `A_*` and
//! `AR_*`, `heap/shared.rs`'s `S_*`, `config.rs`'s `Config::get` and
//! others) need a harness inside their own module; and `hint::spin_loop`,
//! the `panic` accessors of `vspec/panic.rs`, `axiom_class_table_wf`, the
//! `ExWrite` trait specification and the two `div_ceil`s (`core_int`) have
//! none.

mod abort;
mod atomic;
mod core_int;
mod eq;
mod fmt;
mod misc;
mod ptr;
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
