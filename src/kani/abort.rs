//! `vspec::check::check`, the verified-callable abort.

use super::must_not_return;
use crate::vspec::check::check;

/// S283 `check`, `ensures c`: on a false `c` the call never returns (it
/// panics, and challoc's panics abort), so it returns only if `c`.
#[kani::proof]
#[kani::should_panic]
fn check_returns_only_if_c() {
    let c: bool = kani::any();
    check(c, "kani: check failed");
    if !c {
        must_not_return();
    }
}

/// `check` on a true `c` returns (no spurious abort; not part of the
/// contract).
#[kani::proof]
fn check_true_returns() {
    check(true, "kani: check failed");
}

/// What `sys::abort_fmt` does under Kani: it never returns. Its body ends
/// in raw syscalls (inline assembly, which Kani does not run); that it
/// never returns is its type (`-> !`), and Verus verifies its body.
#[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
fn stub_abort_fmt(_args: core::fmt::Arguments<'_>) -> ! {
    panic!("kani: abort_fmt")
}

/// S217 `abort_integrity`, `requires may_abort_corrupt()`, `-> !`: the
/// call reaches `abort_fmt` with no other failure on the way (its own
/// `-> !` already rules out a return; it passes `args` on unread).
#[kani::proof]
#[kani::should_panic]
#[kani::stub(crate::sys::abort::abort_fmt, stub_abort_fmt)]
fn abort_integrity_reaches_abort_fmt() {
    let v: usize = kani::any();
    crate::sys::abort_integrity(format_args!("kani: integrity {v}"));
}
