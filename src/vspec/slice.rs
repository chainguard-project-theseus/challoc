//! `core::slice` functions.

use vstd::prelude::*;

verus! {

/// `a.get_unchecked(i)` on an array, for the fast paths: vstd's
/// array-to-slice coercion (`vstd::array::array_as_slice`) declares Verus's
/// default invariant mask, `any`, which a function that opens none cannot
/// call. Trusted: the element at an index in bounds, which is
/// `get_unchecked`'s safety contract.
#[inline(always)]
#[verifier::external_body]
pub unsafe fn array_get_unchecked<T, const N: usize>(a: &[T; N], i: usize) -> (r: &T)
    requires
        i < N,
    ensures
        *r == a[i as int],
    opens_invariants none
    no_unwind
{
    // SAFETY: `i < N` (the caller's contract).
    unsafe { a.get_unchecked(i) }
}

/// `core::slice::from_raw_parts(p, n)` over `n` initialised `T`s whose
/// permissions `perms` holds from key `j0` on, element `i`'s at key
/// `j0 + i`, at `p`, `p + size_of::<T>()`, ... (a slice view of
/// permissions, which vstd lacks: it makes a reference from a permission
/// only for one sized `T`, `ptr_ref`). Trusted: such permissions give what
/// `from_raw_parts` asks for (every element initialised, aligned and valid
/// for reads, `p` non-null since a permission's pointer to a `T` of
/// non-zero size is), the slice's elements are the permissions' values,
/// and the slice lives as long as their borrow, so no permission holder
/// writes them meanwhile (a type with interior mutability, such as
/// `AtomicU64`, still changes through the slice's shared references, as
/// through the permissions' pointers). A slice permission lent by the
/// element permissions (an axiom with a `ptr_ref`-like builder) would let
/// this verify, but only by moving the trusted step into the builder, and
/// it would add a `Tracked` type at the call, on the flush path.
///
/// # Safety
/// The `n` elements lie in one allocation, which `from_raw_parts` also
/// asks and the permissions do not say.
#[verifier::external_body]
#[inline(always)]
pub unsafe fn slice_from_perms<T>(
    p: *const T,
    n: usize,
    Ghost(j0): Ghost<int>,
    Tracked(perms): Tracked<&Map<int, vstd::raw_ptr::PointsTo<T>>>,
) -> (r: &[T])
    requires
        0 < n,
        0 < vstd::layout::size_of::<T>(),
        n * vstd::layout::size_of::<T>() <= isize::MAX,
        p@.addr + n * vstd::layout::size_of::<T>() <= usize::MAX,
        forall|i: int| #![trigger perms.contains_key(j0 + i)] #![trigger perms[j0 + i]] 0 <= i < n ==> {
            &&& perms.contains_key(j0 + i)
            &&& perms[j0 + i].is_init()
            &&& perms[j0 + i].ptr() == vstd::raw_ptr::ptr_mut_from_data::<T>(vstd::raw_ptr::PtrData::<T> {
                addr: (p@.addr + vstd::layout::size_of::<T>() * i) as usize,
                provenance: p@.provenance,
                metadata: (),
            })
        },
    ensures
        r@.len() == n,
        forall|i: int| 0 <= i < n ==> #[trigger] r@[i] == perms[j0 + i].value(),
{
    // SAFETY: the precondition, and the caller's one-allocation contract.
    unsafe { core::slice::from_raw_parts(p, n) }
}

/// `s.swap(a, b)`: panics unless both indices are in bounds.
pub assume_specification<T>[ <[T]>::swap ](s: &mut [T], a: usize, b: usize)
    requires
        a < old(s)@.len(),
        b < old(s)@.len(),
    ensures
        final(s)@ == old(s)@.update(a as int, old(s)@[b as int]).update(b as int, old(s)@[a as int]),
;

/// A one-element slice of `*s`.
pub assume_specification<T>[ core::slice::from_ref ](s: &T) -> (r: &[T])
    ensures
        r@ == seq![*s],
;

/// `core::array::IntoIter`, the iterator `for x in [a, b]` walks. A loop
/// with a `continue`, which Verus rejects in a `for`, is spelled out as the
/// `for` desugaring (`into_iter`, then `next` until `None`) so that the
/// machine code stays that of the `for`; these specifications make that
/// form verifiable.
#[cfg(verus_keep_ghost)]
#[verifier::external_type_specification]
#[verifier::external_body]
#[verifier::accept_recursive_types(T)]
pub struct ExArrayIntoIter<T, const N: usize>(core::array::IntoIter<T, N>);

/// The elements an array iterator has yet to hand out, front first.
pub uninterp spec fn array_iter_rest<T, const N: usize>(i: core::array::IntoIter<T, N>) -> Seq<T>;

/// The iterator starts with every element of the array, in order.
pub assume_specification<T, const N: usize>[ <[T; N] as core::iter::IntoIterator>::into_iter ](a: [T; N]) -> (r: <[
    T;
    N
] as core::iter::IntoIterator>::IntoIter)
    ensures
        array_iter_rest(r) == a@,
;

/// `next` hands out the front element, or `None` once every element has
/// been handed out.
pub assume_specification<T, const N: usize>[ <core::array::IntoIter<T, N> as core::iter::Iterator>::next ](i: &mut core::array::IntoIter<T, N>) -> (r: Option<
    <core::array::IntoIter<T, N> as core::iter::Iterator>::Item,
>)
    ensures
        array_iter_rest(*old(i)).len() > 0 ==> r == Some(array_iter_rest(*old(i))[0]) && array_iter_rest(*final(i))
            == array_iter_rest(*old(i)).drop_first(),
        array_iter_rest(*old(i)).len() == 0 ==> r is None && array_iter_rest(*final(i)) == array_iter_rest(*old(i)),
;

} // verus!
