//! Address-to-pointer casts and byte fills, which Verus does not model.

use vstd::prelude::*;

verus! {

/// The pointer `addr as *mut u8` yields: the address with the provenance
/// the cast picks up (challoc's memory comes from `mmap`, whose ranges
/// the kernel hands out as plain addresses). Left abstract; one value per
/// address, so a claim about the memory at `addr` can name it.
pub uninterp spec fn addr_ptr(addr: usize) -> *mut u8;

/// The provenance every integer-to-pointer cast picks up: an address cast
/// to a pointer names no allocation of its own, so all of them share one
/// (Rust's exposed provenance). The permissions minted for `mmap`'d
/// memory carry it (`vspec::unbacked::Unbacked::commit`).
pub uninterp spec fn cast_provenance() -> vstd::raw_ptr::Provenance;

/// `addr as *mut u8`: a pointer to `addr` (Verus supports only the other
/// direction of the cast), [`addr_ptr`], with [`cast_provenance`]. Verified
/// code compares and returns it, and passes it to the functions below
/// only under an axiom about the memory at `addr` or with a permission
/// of the same provenance.
#[verifier::external_body]
#[inline(always)]
pub fn addr_to_ptr(addr: usize) -> (p: *mut u8)
    ensures
        p as usize == addr,
        p == addr_ptr(addr),
        p@.provenance == cast_provenance(),
    opens_invariants none
{
    addr as *mut u8
}

/// `core::ptr::write_bytes`, with the safety contract of the `core`
/// documentation: `dst` is properly aligned and, unless the fill is empty,
/// valid for writes of `count * size_of::<T>()` bytes. Its effect is not
/// stated, so Verus assumes it changes no memory a ghost permission covers.
pub assume_specification<T>[ core::ptr::write_bytes::<T> ](dst: *mut T, val: u8, count: usize)
    requires
        dst@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        count as int * vstd::layout::size_of::<T>() == 0 || crate::vspec::ptr::valid_writes(dst, count as int * vstd::layout::size_of::<T>()),
;

/// `core::ptr::copy_nonoverlapping`, with the safety contract of the
/// `core` documentation: both pointers are properly aligned and, unless
/// the copy is empty, `src` is valid for reads and `dst` for writes of
/// `count * size_of::<T>()` bytes, and the two ranges do not overlap. As
/// for `write_bytes`, its effect is not stated.
pub assume_specification<T>[ core::ptr::copy_nonoverlapping::<T> ](src: *const T, dst: *mut T, count: usize)
    requires
        src@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        dst@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        count as int * vstd::layout::size_of::<T>() == 0 || {
            let len = count as int * vstd::layout::size_of::<T>();
            &&& crate::vspec::ptr::valid_reads(src, len)
            &&& crate::vspec::ptr::valid_writes(dst, len)
            &&& (src@.addr + len <= dst@.addr || dst@.addr + len <= src@.addr)
        },
;

/// `<*mut T>::is_null`: the address is zero.
pub assume_specification<T: core::marker::PointeeSized>[ <*mut T>::is_null ](p: *mut T) -> (r: bool)
    ensures
        r == (p@.addr == 0),
    opens_invariants none
    no_unwind
;

/// `<*const T>::is_null`: the address is zero.
pub assume_specification<T: core::marker::PointeeSized>[ <*const T>::is_null ](p: *const T) -> (r: bool)
    ensures
        r == (p@.addr == 0),
    opens_invariants none
    no_unwind
;

} // verus!
