//! `core::slice` functions.

use vstd::prelude::*;

verus! {

/// `core::slice::from_raw_parts`, with the safety contract of the `core`
/// documentation that can be stated here: `data` is not null, aligned,
/// and valid for reads of the slice's `len * size_of::<T>()` bytes, at
/// most `isize::MAX`. The read bound must be the read-only one a C
/// caller's argument has (`r_len`, which only `vspec::cbuf`'s facts
/// bound), not the read-write one of memory verified code holds a
/// permission to (`rw_len`, `axiom_raw_valid`), whose bytes it may write
/// through that permission while the slice lives. Not stated: that the
/// elements are initialised `T`s and unchanged for `'a` (the one caller,
/// `mallctl`'s `name_bytes`, reads the caller's string for the call and
/// ties `'a` to the call's token). The length is the result's; its
/// elements are not stated.
pub assume_specification<'a, T>[ core::slice::from_raw_parts::<'a, T> ](data: *const T, len: usize) -> (r: &'a [T])
    requires
        data@.addr != 0,
        data@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        len as int * vstd::layout::size_of::<T>() <= isize::MAX,
        len as int * vstd::layout::size_of::<T>() <= crate::vspec::ptr::r_len(data@.provenance, data@.addr),
    ensures
        r@.len() == len,
;

} // verus!

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

/// `s.sort_unstable()` on a slice of raw pointers: permutes `s`. Trusted:
/// `core` documents panics for an `Ord` that is not a total order or
/// that panics itself, and a thin pointer's `Ord` compares addresses,
/// a total order that does not panic, so the call returns with `s` a
/// permutation of what it held. A wrapper for `*mut u8` alone, where an
/// `assume_specification` of `<[T]>::sort_unstable` would cover every
/// `T: Ord`. `#[inline(always)]` leaves the machine code of the bare call.
#[inline(always)]
#[verifier::external_body]
pub fn sort_ptrs(s: &mut [*mut u8])
    ensures
        final(s)@.len() == old(s)@.len(),
        final(s)@.to_multiset() == old(s)@.to_multiset(),
        forall|i: int| 0 <= i < old(s)@.len() ==> old(s)@.contains(#[trigger] final(s)@[i]),
{
    s.sort_unstable()
}

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

/// `q` is one of the `ptr_add` keys `start..start + n` from `base`.
pub open spec fn elem_key<T>(base: *mut T, start: int, n: int, q: *mut T) -> bool {
    exists|i: int| start <= i < start + n && q == #[trigger] crate::vspec::ptr::ptr_add(base, i as usize)
}

/// `core::slice::from_raw_parts_mut(base.add(start), n)` over the elements
/// `start..start + n` of the `T`s at `base`, whose permissions `elems`
/// holds keyed by their pointers, element `i`'s at `base.add(i)`, and no
/// other permission: the slice of those elements, for as long as `elems`
/// stays borrowed. Each permission must be owned, not necessarily
/// initialised; the slice starts out holding the initialised elements'
/// values, and when the borrow ends `elems` holds each element's
/// permission again, with its pointer, initialised to the slice's final
/// value there. Trusted: such permissions give what `from_raw_parts_mut` asks
/// for (the elements valid for reads and writes and aligned, and accessed
/// through the slice alone while it lives, which the borrow of `elems`
/// keeps), and a write through a slice leaves the element initialised.
/// Premises, not in the `requires` (the keys are `ptr_add`s, which say
/// nothing of addresses on their own): the elements are consecutive in
/// one allocation of at most `isize::MAX` bytes, so `base.add(start)` is
/// in bounds, and `T` is not zero-sized, so no two keys are one pointer;
/// and every element's bytes are initialised, as `from_raw_parts_mut`
/// asks and a permission that is not initialised does not show (a cache
/// stack's entries hold the pointer last stored there, or zero from the
/// metadata region's fresh pages). The one
/// caller is `slice_mut_from_perms`, for `heap::stack_mut`, which passes
/// a cache stack's `*mut u8` entries (in the stack's allocation,
/// `heap::slot_in`); `stack_mut`'s callers read an entry only after a
/// claim or a push wrote it.
#[verifier::external_body]
#[inline(always)]
#[allow(clippy::needless_lifetimes, reason = "the permission and the slice share the lifetime in the Verus signature")]
pub unsafe fn slice_mut_elems<'a, T>(
    base: *mut T,
    start: usize,
    n: usize,
    Tracked(elems): Tracked<&'a mut Map<*mut T, vstd::raw_ptr::PointsTo<T>>>,
) -> (r: &'a mut [T])
    requires
        0 < n,
        forall|q: *mut T| #[trigger] old(elems).contains_key(q) ==> elem_key(base, start as int, n as int, q),
        forall|i: int| start <= i < start + n ==> {
            let q = #[trigger] crate::vspec::ptr::ptr_add(base, i as usize);
            old(elems).contains_key(q) && old(elems)[q].ptr() == q
        },
    ensures
        r@.len() == n,
        final(r)@.len() == n,
        forall|i: int| start <= i < start + n ==> {
            let q = #[trigger] crate::vspec::ptr::ptr_add(base, i as usize);
            &&& final(elems).contains_key(q)
            &&& final(elems)[q].ptr() == q
            &&& final(elems)[q].opt_value() == vstd::raw_ptr::MemContents::Init(final(r)@[i - start])
        },
        forall|i: int|
            start <= i < start + n && old(elems)[crate::vspec::ptr::ptr_add(base, i as usize)].is_init()
                ==> r@[i - start] == #[trigger] old(elems)[crate::vspec::ptr::ptr_add(base, i as usize)].value(),
{
    // SAFETY: the precondition and the premises above.
    unsafe { core::slice::from_raw_parts_mut(base.add(start), n) }
}

/// `core::slice::from_raw_parts_mut(base.add(start), n)` over the elements
/// `start..start + n` of the `T`s at `base`, whose permissions `perms`
/// holds keyed by their pointers, element `i`'s at `base.add(i)`:
/// [`slice_mut_elems`] over those elements' permissions, the others kept
/// apart (`Map::tracked_borrow_mut_split`). The slice borrows the
/// permissions: each must be owned, not necessarily initialised; when the
/// borrow ends every one it covered is initialised to the slice's final
/// value there, every permission keeps its pointer and, if it was
/// initialised, stays so, and no other permission changes. The slice
/// starts out holding the initialised elements' values. The one caller is
/// `heap::stack_mut` (a cache stack's entries).
///
/// # Safety
/// The premises of [`slice_mut_elems`].
#[inline(always)]
#[allow(clippy::needless_lifetimes, reason = "the permission and the slice share the lifetime in the Verus signature")]
pub unsafe fn slice_mut_from_perms<'a, T>(
    base: *mut T,
    start: usize,
    n: usize,
    Tracked(perms): Tracked<&'a mut Map<*mut T, vstd::raw_ptr::PointsTo<T>>>,
) -> (r: &'a mut [T])
    requires
        0 < n,
        forall|i: int| start <= i < start + n ==> {
            let q = #[trigger] crate::vspec::ptr::ptr_add(base, i as usize);
            old(perms).contains_key(q) && old(perms)[q].ptr() == q
        },
    ensures
        r@.len() == n,
        final(r)@.len() == n,
        forall|q: *mut T| #![trigger old(perms).contains_key(q)] #![trigger final(perms).contains_key(q)] #![trigger final(perms)[q]] old(perms).contains_key(q) ==> {
            &&& final(perms).contains_key(q)
            &&& final(perms)[q].ptr() == old(perms)[q].ptr()
            &&& (old(perms)[q].is_init() ==> final(perms)[q].is_init())
        },
        forall|i: int| start <= i < start + n ==> #[trigger] final(perms)[crate::vspec::ptr::ptr_add(base, i as usize)].is_init(),
        forall|i: int|
            start <= i < start + n && old(perms)[crate::vspec::ptr::ptr_add(base, i as usize)].is_init()
                ==> r@[i - start] == #[trigger] old(perms)[crate::vspec::ptr::ptr_add(base, i as usize)].value(),
        forall|i: int| start <= i < start + n ==> #[trigger] final(perms)[crate::vspec::ptr::ptr_add(base, i as usize)].value() == final(r)@[i - start],
        forall|q: *mut T|
            old(perms).contains_key(q) && (forall|i: int| start <= i < start + n ==> q != #[trigger] crate::vspec::ptr::ptr_add(base, i as usize))
                ==> #[trigger] final(perms)[q] == old(perms)[q],
{
    let ghost keys = perms.dom().filter(|q: *mut T| elem_key(base, start as int, n as int, q));
    let tracked (elems, _rest) = perms.tracked_borrow_mut_split(keys);
    proof {
        assert forall|i: int| start <= i < start + n implies {
            let q = #[trigger] crate::vspec::ptr::ptr_add(base, i as usize);
            elems[q] == old(perms)[q]
        } by {
            assert(keys.contains(crate::vspec::ptr::ptr_add(base, i as usize)));
        }
    }
    // SAFETY: guaranteed by the caller.
    unsafe { slice_mut_elems(base, start, n, Tracked(elems)) }
}

} // verus!
