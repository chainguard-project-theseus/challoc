//! Specifications for the class protocol (`class/`): `core::mem::drop`
//! and the class table's well-formedness for proofs.

use vstd::prelude::*;

verus! {

/// Every entry of the class table is well formed and has its class's
/// size: [`crate::layout::class_table`] is a table that is
/// [`crate::layout::table_ok`] by its definition.
pub broadcast proof fn lemma_class_table_wf(i: int)
    requires
        0 <= i < crate::layout::NCLASSES,
    ensures
        #[trigger] crate::layout::class_table()[i].wf(),
        crate::layout::class_table()[i].size == crate::layout::spec_class_size(i),
{
    reveal(crate::layout::class_table);
    crate::layout::lemma_spec_ok_table_ok();
}

/// `core::mem::drop`: runs `x`'s destructor (for a lock guard, the unlock).
pub assume_specification<T>[ core::mem::drop ](x: T);

} // verus!
