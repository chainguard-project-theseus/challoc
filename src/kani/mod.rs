//! Kani harnesses for seams of `verus/ledger/` (`cfg(kani)` only:
//! ordinary builds, the tests and Verus never see this module). Each
//! harness checks a trusted item's body, or the `core` function an
//! `assume_specification` describes, against the contract Verus assumes,
//! for every input of its instantiated types or within the bounds its
//! comment states. Its comment names the item, as its ledger entry does.
//! A harness is evidence for its seam; it changes no ledger status.
//! `.chainguard/ci/kani.yaml` runs them all, under both quanta, in two
//! checks. `kani` runs on pull requests that change `src/kani/`,
//! `src/vspec/`, a file in its `changed:` list (the files with a `kani_*`
//! module that proof lanes rarely touch) or `kani.yaml`. `kani-main` runs
//! after every landing on main and covers the rest: the `kani_*` modules in
//! files most proof pull requests touch (`heap/cache.rs`, `purge.rs`), and
//! the checked items outside any listed path (`stats.rs`'s `fig_*`,
//! `sys/abort.rs`, the class table, the enums `eq.rs` matches). The list is
//! kept by hand: a `kani_*` module added to a rarely changed file belongs in
//! `changed:`; one added to a hot file, one most proof pull requests
//! touch (today `heap/cache.rs` and `purge.rs`), is left to `kani-main`, so
//! a change that breaks it is caught after it lands, not before.
//!
//! The harnesses of items with private parts sit next to them, in a
//! `kani_*` module of the item's own file (`config.rs`'s `kani_config`,
//! for one). Not every seam the ledger gives to Kani has a harness: some
//! private items, the layout constants (`arena.rs`'s `A_*` and `AR_*`,
//! `heap/shared.rs`'s `S_*` and others), `hint::spin_loop`, the `panic`
//! accessors of `vspec/panic.rs`, `axiom_class_table_wf`, the `ExWrite`
//! trait specification and the two `div_ceil`s (`core_int`) have none.

mod abort;
mod atomic;
mod core_int;
mod eq;
pub(crate) mod fmt;
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
