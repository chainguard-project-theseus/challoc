//! The `Location` accessors of `core` that `vspec/panic.rs` specifies,
//! `Location::file` and `Location::line`
//! (`verus/ledger/src/vspec/panic.rs.md`). Their contracts claim only
//! that the call returns. A `Location` comes from the compiler alone
//! (`core` gives it no constructor and keeps its fields private), and
//! `core`'s accessors read a field and do not branch on its value (`file`
//! relies on `core`'s invariant that the field points to a `str`, which
//! no harness can vary), so the harnesses run them on one
//! `Location`: [`AT`]'s, which the compiler builds when it evaluates
//! `Location::caller` in a constant (Kani 0.68 does not support the
//! `caller_location` intrinsic at run time).
//!
//! `PanicInfo::location` and `PanicInfo::message` have no harness: code
//! outside a panic handler cannot build a `PanicInfo` (`core` keeps its
//! constructor `pub(crate)`), and a harness links `std`'s handler, not
//! challoc's (`lib.rs`'s `panic` is `cfg(not(any(test, kani)))`).

use core::panic::Location;

/// The `Location` of this constant's own line, and that line.
const AT: (&Location<'static>, u32) = (Location::caller(), line!());

/// `Location::file`, no `requires` or `ensures`: the call returns (and
/// gives this file's name, which the contract does not claim).
#[kani::proof]
fn location_file_returns() {
    let f = AT.0.file();
    assert_eq!(f, file!());
    kani::cover!(!f.is_empty());
}

/// `Location::line`, no `requires` or `ensures`: the call returns (and
/// gives [`AT`]'s line, which the contract does not claim).
#[kani::proof]
fn location_line_returns() {
    let l = AT.0.line();
    assert_eq!(l, AT.1);
    kani::cover!(l != 0);
}
