//! `core::slice` functions.

use vstd::prelude::*;

verus! {

/// A one-element slice of `*s`.
pub assume_specification<T>[ core::slice::from_ref ](s: &T) -> (r: &[T])
    ensures
        r@ == seq![*s],
;

} // verus!

verus! {

/// `for e in s` over a mutable slice: `s.iter_mut()`, whose `vstd`
/// specification this repeats (the loop hands out each element once, and
/// each one's final value flows back into `s`).
pub assume_specification<'a, T>[ <&'a mut [T] as core::iter::IntoIterator>::into_iter ](s: &'a mut [T]) -> (iter: core::slice::IterMut<'a, T>)
    ensures
        vstd::std_specs::iter::IteratorSpec::remaining(&iter).len() == old(s)@.len() == final(s)@.len(),
        forall|i: int| #![trigger vstd::std_specs::iter::IteratorSpec::remaining(&iter)[i]]
            0 <= i < old(s)@.len() ==> *(vstd::std_specs::iter::IteratorSpec::remaining(&iter)[i]) == old(s)@[i],
        forall|i: int|
            #![trigger vstd::std_specs::iter::IteratorSpec::remaining(&iter)[i]]
            #![trigger final(s)@[i]]
            0 <= i < old(s)@.len() ==> *final(vstd::std_specs::iter::IteratorSpec::remaining(&iter)[i]) == final(s)@[i],
        vstd::std_specs::iter::IteratorSpec::obeys_prophetic_iter_laws(&iter),
        vstd::std_specs::iter::IteratorSpec::will_return_none(&iter),
        vstd::std_specs::iter::IteratorSpec::decrease(&iter) is Some,
;

} // verus!

verus! {

/// `s.swap(a, b)`: panics unless both indices are in bounds.
pub assume_specification<T>[ <[T]>::swap ](s: &mut [T], a: usize, b: usize)
    requires
        a < old(s)@.len(),
        b < old(s)@.len(),
    ensures
        final(s)@ == old(s)@.update(a as int, old(s)@[b as int]).update(b as int, old(s)@[a as int]),
;

} // verus!

verus! {

/// `s.reverse()`.
pub assume_specification<T>[ <[T]>::reverse ](s: &mut [T])
    ensures
        final(s)@ == old(s)@.reverse(),
;

} // verus!

verus! {

// The ordering operations of the cache flush (`heap::Heap::flush`).
// Only what callers rely on is stated: the length,
// the panics' conditions and, for the plain and keyed selections and
// sorts, that they permute the elements (the multiset is unchanged, so
// the flush's slot tokens can follow their entries; that every element
// left was there before is stated too, for the kept entries). The key
// closures are called on elements, so they must accept any. The keyed
// forms' permutation assumes the key
// closure changes no element (no interior mutability in `T`); the one
// instantiation is `*mut u8`.
// Since Rust 1.81 the sorts and selections may panic when `Ord` is not a
// total order; that is not stated, and the one instantiation here
// (`*mut u8` and `usize` keys) is total.

/// `s.is_sorted()`. Nothing is claimed about the result.
pub assume_specification<T: PartialOrd>[ <[T]>::is_sorted ](s: &[T]) -> bool;

/// `s.is_sorted_by_key(f)`. Nothing is claimed about the result.
pub assume_specification<'a, T, F: FnMut(&'a T) -> K, K: PartialOrd>[ <[T]>::is_sorted_by_key::<'a, F, K> ](s: &'a [T], f: F) -> bool
    requires
        forall|x: &'a T| #[trigger] f.requires((x,)),
;

/// `s.select_nth_unstable(index)`: panics unless `index` is in bounds;
/// permutes `s` and returns it as the `index` elements before position
/// `index`, the element there and the rest. The permutation is stated of
/// the three parts' values on return, and `s`'s final value is theirs:
/// a caller may write through them (the parts reborrow `s`), so a
/// contract on `final(s)` alone would let a verified caller prove `s`
/// still a permutation after such a write (`verus/audit/library-control.md`).
pub assume_specification<T: Ord>[ <[T]>::select_nth_unstable ](s: &mut [T], index: usize) -> (r: (&mut [T], &mut T, &mut [T]))
    requires
        index < old(s)@.len(),
    ensures
        r.0@.len() == index,
        r.0@.len() + 1 + r.2@.len() == old(s)@.len(),
        (r.0@ + seq![*r.1] + r.2@).to_multiset() == old(s)@.to_multiset(),
        forall|i: int| 0 <= i < old(s)@.len() ==> old(s)@.contains(#[trigger] (r.0@ + seq![*r.1] + r.2@)[i]),
        final(s)@ == final(r.0)@ + seq![*final(r.1)] + final(r.2)@,
;

/// `s.select_nth_unstable_by_key(index, f)`, as `select_nth_unstable`.
pub assume_specification<T, K: Ord, F: FnMut(&T) -> K>[ <[T]>::select_nth_unstable_by_key::<K, F> ](s: &mut [T], index: usize, f: F) -> (r: (&mut [T], &mut T, &mut [T]))
    requires
        index < old(s)@.len(),
        forall|x: &T| #[trigger] f.requires((x,)),
    ensures
        r.0@.len() == index,
        r.0@.len() + 1 + r.2@.len() == old(s)@.len(),
        (r.0@ + seq![*r.1] + r.2@).to_multiset() == old(s)@.to_multiset(),
        forall|i: int| 0 <= i < old(s)@.len() ==> old(s)@.contains(#[trigger] (r.0@ + seq![*r.1] + r.2@)[i]),
        final(s)@ == final(r.0)@ + seq![*final(r.1)] + final(r.2)@,
;

/// `s.sort_unstable()`: permutes `s`.
pub assume_specification<T: Ord>[ <[T]>::sort_unstable ](s: &mut [T])
    ensures
        final(s)@.len() == old(s)@.len(),
        final(s)@.to_multiset() == old(s)@.to_multiset(),
        forall|i: int| 0 <= i < old(s)@.len() ==> old(s)@.contains(#[trigger] final(s)@[i]),
;

/// `s.sort_unstable_by_key(f)`: permutes `s`.
pub assume_specification<T, K: Ord, F: FnMut(&T) -> K>[ <[T]>::sort_unstable_by_key::<K, F> ](s: &mut [T], f: F)
    requires
        forall|x: &T| #[trigger] f.requires((x,)),
    ensures
        final(s)@.len() == old(s)@.len(),
        final(s)@.to_multiset() == old(s)@.to_multiset(),
        forall|i: int| 0 <= i < old(s)@.len() ==> old(s)@.contains(#[trigger] final(s)@[i]),
;

/// `get_unchecked_mut` is `index_mut` without the bounds check: its safety
/// contract is that the index is in bounds, so it has `index_mut`'s
/// precondition.
pub assume_specification<T, I>[ <[T]>::get_unchecked_mut::<I> ](slice: &mut [T], i: I) -> (b: &mut <I as core::slice::SliceIndex<[T]>>::Output)
    where I: core::slice::SliceIndex<[T]>
    requires call_requires(<I as core::slice::SliceIndex<[T]>>::index_mut, (i, old(slice))),
    ensures call_ensures(<I as core::slice::SliceIndex<[T]>>::index_mut, (i, old(slice)), b),
;

} // verus!

verus! {

/// `a.swap_with_slice(b)`: panics unless the lengths are equal.
pub assume_specification<T>[ <[T]>::swap_with_slice ](a: &mut [T], b: &mut [T])
    requires
        old(a)@.len() == old(b)@.len(),
    ensures
        final(a)@ == old(b)@,
        final(b)@ == old(a)@,
;

/// `s.rotate_left(mid)`: panics unless `mid <= s.len()`.
pub assume_specification<T>[ <[T]>::rotate_left ](s: &mut [T], mid: usize)
    requires
        mid <= old(s)@.len(),
    ensures
        final(s)@ == old(s)@.subrange(mid as int, old(s)@.len() as int) + old(s)@.subrange(0, mid as int),
;

} // verus!

verus! {

/// `core::array::IntoIter`, the iterator `for x in [a, b]` walks. A loop
/// with a `continue`, which Verus rejects in a `for`, is spelled out as the
/// `for` desugaring (`into_iter`, then `next` until `None`) so that the
/// machine code stays that of the `for`; these specifications make that
/// form verifiable.
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

verus! {

/// `a.get_unchecked(i)` on an array, for the fast paths: vstd's
/// array-to-slice coercion (`vstd::array::array_as_slice`) declares Verus's
/// default invariant mask, `any`, which a function that opens none cannot
/// call (`vspec::atomic`, theorem 9 of `verus/THEOREMS.md`). Trusted: the
/// element at an index in bounds, which is `get_unchecked`'s safety
/// contract.
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

} // verus!
