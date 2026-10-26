//! Raw pointer arithmetic.

use vstd::prelude::*;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::Provenance;

verus! {

/// `p.add(count)`, left abstract: verified code only dereferences the
/// result through a `vstd::raw_ptr::PointsTo` whose `ptr()` is this value,
/// so nothing about the address itself is needed.
pub uninterp spec fn ptr_add<T>(p: *mut T, count: usize) -> *mut T;

/// How many bytes from `addr` to the end of the allocation that a pointer
/// with provenance `prov` and address `addr` is derived from (negative
/// when there is none). Uninterpreted: verified code cannot learn it on
/// its own. The trusted function that produced a pointer states a bound on
/// it for the offsets its callers use, and verified code carries that
/// bound from there. Address and provenance survive pointer casts, so the
/// bound does too.
pub uninterp spec fn alloc_len(prov: Provenance, addr: usize) -> int;

/// The `len` bytes from `p` lie inside the allocation `p` is derived from
/// (the range may end one past its last byte): the bounds condition of
/// `<*mut T>::add`.
pub open spec fn in_alloc<T>(p: *mut T, len: int) -> bool {
    0 <= len <= alloc_len(p@.provenance, p@.addr)
}

/// The address [`ptr_add`] stands for: `count` elements past `p`, when
/// that stays inside the allocation `p` points into (the only case in
/// which `<*mut T>::add` is defined, and so the only one verified code
/// reaches), and the address does not wrap (the caller shows it: no
/// allocation does, but `alloc_len` is not tied to the address space).
/// Trusted: the `core` documentation of `add` (an offset of
/// `count * size_of::<T>()` bytes, keeping `p`'s provenance and
/// metadata), which Verus does not model for raw pointers. Not a
/// broadcast: the proofs that place memory call it.
#[verifier::external_body]
pub proof fn axiom_ptr_add_addr<T>(p: *mut T, count: usize)
    requires
        in_alloc(p, count as int * vstd::layout::size_of::<T>()),
        p@.addr + count as int * vstd::layout::size_of::<T>() <= usize::MAX,
    ensures
        ptr_add(p, count)@.addr == p@.addr + count as int * vstd::layout::size_of::<T>(),
        ptr_add(p, count)@.provenance == p@.provenance,
        ptr_add(p, count)@.metadata == p@.metadata,
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
    opens_invariants none
;

/// `<*const T>::add`, with the safety contract of `<*mut T>::add`: the
/// result is [`ptr_add`] of the pointer as a `*mut T`, back as a
/// `*const T` (`axiom_ptr_add_addr` gives its address).
pub assume_specification<T>[ <*const T>::add ](p: *const T, count: usize) -> (r: *const T)
    requires
        count as int * vstd::layout::size_of::<T>() <= isize::MAX,
        count as int * vstd::layout::size_of::<T>() == 0 || in_alloc(p as *mut T, count as int * vstd::layout::size_of::<T>()),
    ensures
        r == ptr_add(p as *mut T, count) as *const T,
    opens_invariants none
;

/// Element `i` of a frozen array: a `'static` shared permission to a
/// `[T; N]` gives one to each of its elements, holding that element, at
/// [`ptr_add`] of the array's address as a `*mut T`; and the array's first
/// `i` elements lie inside the allocation it is in, in fewer than
/// `isize::MAX` bytes, so `<*mut T>::add(i)` is defined on that pointer.
/// Trusted: the `core` layout of arrays (`N` elements `size_of::<T>()`
/// apart, from the array's address; a Rust object is at most `isize::MAX`
/// bytes) and the meaning of a permission (one to an array owns its
/// elements' bytes), neither of which vstd states. Shared and `'static`
/// only, so nothing is written through either permission.
pub axiom fn axiom_array_elem<T, const N: usize>(tracked a: &'static vstd::raw_ptr::PointsTo<[T; N]>, i: usize) -> (tracked r: &'static vstd::raw_ptr::PointsTo<T>)
    requires
        i < N,
        a.is_init(),
    ensures
        in_alloc(a.ptr() as *mut T, i as int * vstd::layout::size_of::<T>()),
        i as int * vstd::layout::size_of::<T>() <= isize::MAX,
        r.ptr() == ptr_add(a.ptr() as *mut T, i),
        r.is_init(),
        r.value() == a.value()[i as int],
;

} // verus!
