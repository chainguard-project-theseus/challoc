//! The report's trusted helper in `vspec/print.rs` that Kani can run:
//! `base_slab_sizes` (`verus/ledger/src/vspec/print.rs.md`).

use crate::layout::{BASE_CLASSES, NCLASSES};
use crate::vspec::print::base_slab_sizes;

/// `base_slab_sizes`, `requires hi < NCLASSES`, no `ensures`: under it
/// the call returns, for every `lo` and every `hi < NCLASSES` (and its
/// pair brackets every table slab size of `lo..=hi`, and is `(0, 0)` when
/// it is empty).
#[kani::proof]
#[kani::unwind(70)]
fn base_slab_sizes_returns() {
    let lo: usize = kani::any();
    let hi: usize = kani::any();
    kani::assume(hi < NCLASSES);
    let (min, max) = base_slab_sizes(lo, hi);
    if lo > hi {
        assert_eq!((min, max), (0, 0));
    } else {
        let mut c = lo;
        while c <= hi {
            let s = BASE_CLASSES[c].slab_size();
            assert!(min <= s && s <= max);
            c += 1;
        }
    }
}
