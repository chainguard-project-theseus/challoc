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
/// direction of the cast), [`addr_ptr`], with [`cast_provenance`]. `rseq::area`
/// and `meta::Inner::alloc` are its callers. `#[inline(always)]` leaves the machine code of the bare
/// cast.
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
