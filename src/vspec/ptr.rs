//! Raw pointer arithmetic, and the validity of raw memory for the `core`
//! functions that take raw pointers.

use vstd::prelude::*;
#[cfg(verus_only)]
use vstd::raw_ptr::Provenance;

verus! {

/// `p.add(count)`, left abstract: verified code only dereferences the
/// result through a `vstd::raw_ptr::PointsTo` whose `ptr()` is this value,
/// so nothing about the address itself is needed.
pub uninterp spec fn ptr_add<T>(p: *mut T, count: usize) -> *mut T;

/// How many bytes from `addr` to the end of the allocation that a pointer
/// with provenance `prov` and address `addr` is derived from (negative
/// when there is none). Uninterpreted: verified code cannot learn it on
/// its own. The trusted function that produced a pointer, or an axiom
/// about the reservation it came from, states a bound on it for the
/// offsets its callers use, and verified code carries that bound from
/// there. Address and provenance survive pointer casts, so the bound does
/// too.
pub uninterp spec fn alloc_len(prov: Provenance, addr: usize) -> int;

/// How many bytes from `addr` are valid for reads and writes through a
/// pointer with provenance `prov`: they lie inside one live allocation
/// that pointer is derived from, and the memory may be read and written.
/// Uninterpreted, as [`alloc_len`]. challoc's own memory is all readable
/// and writable, so one bound serves reads and writes. The model has no
/// memory state: a bound holds at the point it is established (a free or
/// a decommit ends it), and verified code uses each one right away.
pub uninterp spec fn rw_len(prov: Provenance, addr: usize) -> int;

/// The `len` bytes from `p` lie inside the allocation `p` is derived from
/// (the range may end one past its last byte): the bounds condition of
/// `<*mut T>::add`.
pub open spec fn in_alloc<T>(p: *mut T, len: int) -> bool {
    0 <= len <= alloc_len(p@.provenance, p@.addr)
}

/// `p` is valid for writes of `len` bytes.
pub open spec fn valid_writes<T>(p: *mut T, len: int) -> bool {
    0 <= len <= rw_len(p@.provenance, p@.addr)
}

/// `p` is valid for reads of `len` bytes.
pub open spec fn valid_reads<T>(p: *const T, len: int) -> bool {
    0 <= len <= rw_len(p@.provenance, p@.addr)
}

/// The address [`ptr_add`] stands for: `count` elements past `p`, when
/// that stays inside the allocation `p` points into (the only case in
/// which `<*mut T>::add` is defined, and so the only one verified code
/// reaches), and the address does not wrap (the caller shows it: no
/// allocation does, but `alloc_len` is not tied to the address space).
/// Trusted: the `core` documentation of `add` (an offset of
/// `count * size_of::<T>()` bytes), which Verus does not model for raw
/// pointers. Not a broadcast: the proofs that place memory call it.
#[verifier::external_body]
pub proof fn axiom_ptr_add_addr<T>(p: *mut T, count: usize)
    requires
        in_alloc(p, count as int * vstd::layout::size_of::<T>()),
        p@.addr + count as int * vstd::layout::size_of::<T>() <= usize::MAX,
    ensures
        ptr_add(p, count)@.addr == p@.addr + count as int * vstd::layout::size_of::<T>(),
{
}

/// `<*mut T>::add`, with the safety contract of the `core` documentation:
/// the byte offset `count * size_of::<T>()` fits in an `isize`, and, when
/// it is not zero, the whole range from `p` to the result is inside the
/// allocation `p` points into ([`in_alloc`]).
pub assume_specification<T>[ <*mut T>::add ](p: *mut T, count: usize) -> (r: *mut T)
    requires
        count as int * vstd::layout::size_of::<T>() <= isize::MAX,
        count as int * vstd::layout::size_of::<T>() == 0 || in_alloc(p, count as int * vstd::layout::size_of::<T>()),
    ensures
        r == ptr_add(p, count),
;

} // verus!
