//! The report's trusted helpers in `vspec/print.rs` that Kani can run:
//! `base_slab_sizes`.

use crate::layout::{BASE_CLASSES, NCLASSES};
use crate::vspec::print::base_slab_sizes;

/// S352 `base_slab_sizes`, `requires hi < NCLASSES`, no `ensures`: under
/// it the call returns, for every `lo` and `hi` (and its pair brackets
/// every table slab size of `lo..=hi`, and is `(0, 0)` when it is empty).
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
            let s = 1usize << BASE_CLASSES[c].lg_slab;
            assert!(min <= s && s <= max);
            c += 1;
        }
    }
}
