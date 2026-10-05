//! Specifications for the class protocol (`class/`): `core::mem::drop`
//! and the class table's well-formedness for proofs.

use vstd::prelude::*;

verus! {

/// Every entry of the class table is well formed and has its class's
/// size: the facts
/// [`crate::layout::ClassTable::table`] returns with every read, available
/// to proofs that do not read the table themselves (same trust).
pub broadcast axiom fn axiom_class_table_wf(i: int)
    requires
        0 <= i < crate::layout::NCLASSES,
    ensures
        #[trigger] crate::layout::class_table()[i].wf(),
        crate::layout::class_table()[i].size == crate::layout::spec_class_size(i),
;

/// `core::mem::drop`: runs `x`'s destructor (for a lock guard, the unlock).
pub assume_specification<T>[ core::mem::drop ](x: T);

} // verus!
