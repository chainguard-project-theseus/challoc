//! Address-to-pointer casts, which Verus does not model, and the null
//! test on a pointer's address.

use vstd::prelude::*;

verus! {

/// The pointer `addr as *mut u8` yields: the address with the provenance
/// the cast picks up. Left abstract; one value per address, so a claim
/// about the memory at `addr` can name it.
pub uninterp spec fn addr_ptr(addr: usize) -> *mut u8;

/// The provenance every integer-to-pointer cast picks up: an address cast
/// to a pointer names no allocation of its own, so all of them share one
/// (Rust's exposed provenance).
pub uninterp spec fn cast_provenance() -> vstd::raw_ptr::Provenance;

/// `addr as *mut u8`: a pointer to `addr` (Verus supports only the other
/// direction of the cast), [`addr_ptr`], with [`cast_provenance`].
/// `#[inline(always)]` leaves the machine code of the bare cast.
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

/// `core::ptr::copy_nonoverlapping`, with the safety contract of the
/// `core` documentation: both pointers are properly aligned and, unless
/// the copy is empty, `src` is valid for reads and `dst` for writes of
/// `count * size_of::<T>()` bytes, and the two ranges do not overlap. Its
/// effect is not stated, so Verus assumes it changes no memory a ghost
/// permission covers: as for `vspec::ptr`'s unaligned writes, the
/// destination must be a C caller's buffer apart from challoc's memory
/// (`vspec::cbuf::c_apart`) whose writes are open
/// (`vspec::cbuf::c_writes`), so no permission the call's token lent is
/// held across the copy.
pub assume_specification<T>[ core::ptr::copy_nonoverlapping::<T> ](src: *const T, dst: *mut T, count: usize)
    requires
        src@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        dst@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        count as int * vstd::layout::size_of::<T>() == 0 || {
            let len = count as int * vstd::layout::size_of::<T>();
            &&& crate::vspec::ptr::valid_reads(src, len)
            &&& crate::vspec::ptr::valid_writes(dst, len)
            &&& crate::vspec::cbuf::c_apart(dst@.addr as int, len)
            &&& crate::vspec::cbuf::c_writes(dst@.provenance)
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
