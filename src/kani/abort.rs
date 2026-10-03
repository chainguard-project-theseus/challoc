//! `vspec::check::check`, the verified-callable abort.

use super::must_not_return;
use crate::vspec::check::check;

/// S268 `check`, `ensures c`: on a false `c` the call never returns (it
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
