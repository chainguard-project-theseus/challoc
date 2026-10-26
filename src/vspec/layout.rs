//! `core::alloc::Layout`: its size and alignment, with the invariant
//! `Layout` guarantees (the alignment is a power of two and the size,
//! rounded up to it, is at most `isize::MAX`).

use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;

use core::alloc::Layout;

verus! {

#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExLayout(Layout);

#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExLayoutError(core::alloc::LayoutError);

pub uninterp spec fn layout_size(l: Layout) -> usize;

pub uninterp spec fn layout_align(l: Layout) -> usize;

/// What every `Layout` satisfies (`Layout::from_size_align` checks it).
pub open spec fn layout_ok(l: Layout) -> bool {
    &&& exists|k: nat| k < 64 && layout_align(l) as nat == #[trigger] pow2(k)
    &&& layout_size(l) + layout_align(l) - 1 <= isize::MAX
}

pub assume_specification[ Layout::size ](l: &Layout) -> (r: usize)
    ensures
        r == layout_size(*l),
        layout_ok(*l),
;

pub assume_specification[ Layout::align ](l: &Layout) -> (r: usize)
    ensures
        r == layout_align(*l),
        layout_ok(*l),
;

/// `Ok` exactly when `align` is a power of two and `size` rounded up to it
/// does not exceed `isize::MAX`.
pub assume_specification[ Layout::from_size_align ](size: usize, align: usize) -> (r: Result<
    Layout,
    core::alloc::LayoutError,
>)
    ensures
        r is Ok <==> (align > 0 && align & sub(align, 1) == 0 && size <= isize::MAX - (align - 1)),
        r matches Ok(l) ==> layout_size(l) == size && layout_align(l) == align,
;

} // verus!
