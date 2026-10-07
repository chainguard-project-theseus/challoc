//! `core::slice` functions.

use vstd::prelude::*;

verus! {

/// A one-element slice of `*s`.
pub assume_specification<T>[ core::slice::from_ref ](s: &T) -> (r: &[T])
    ensures
        r@ == seq![*s],
;

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
/// ties `'a` to the call's token, which every write and call back into C
/// takes `&mut`, `vspec::cbuf::CCall::open_writes`). The length is the
/// result's; its elements are not stated.
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
/// call (theorem 9 of `verus/THEOREMS.md`). Trusted: the
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

verus! {

// Slices over element permissions. vstd makes a reference from a
// permission only for one sized `T` (`ptr_ref`, `ptr_mut_ref` over a
// `PointsTo<T>`). An array permission `PointsTo<[T; N]>` would give a
// `&[T; N]`, and so a slice, but its `N` is fixed at compile time, and
// neither region here has one: the class bitmap is the
// `nslabs * slab_bits` bits of a run-time reservation, of which
// `class::table::words_ref` takes one slab's `slab_bits / 64` words (1
// to `MAX_SLAB_WORDS`, by class); a cache stack holds `cap` entries
// (`Config::cap_for`, a run-time `u32`), of which `heap::stack_mut`
// takes any `start..start + len`. A permission for a longer fixed array
// would claim memory past the region (after the bitmap's last slab, past
// the stack's allocation), and a dispatch on the length would change the
// machine code. So the element permissions stand for a slice
// permission, with one trusted step per slice kind: vstd specifies
// neither `from_raw_parts` nor `from_raw_parts_mut`, this crate
// specifies no `from_raw_parts_mut`, and its `core::slice::from_raw_parts`
// specification asks for a C caller's read bound (`r_len`) and states no
// values. The mutable slice's trusted
// step, `slice_mut_elems`, takes a map holding exactly its elements'
// permissions, which `slice_mut_from_perms` carves out of its caller's
// map (`Map::tracked_borrow_mut_split`), so the rest of that contract is
// proved. `slice_from_perms` stays one trusted step: a shared borrow
// changes no permission, so its contract has no frame to prove, and a
// slice permission type lent by the element permissions (an axiom, with
// a `ptr_ref`-like builder) lets `slice_from_perms` verify but only
// moves the trusted step into the builder, and adds a `Tracked` type at
// the call, which changed the machine code of the flush path
// (`Heap::flush` and `group_by_slab` reach `words_ref`).

/// `core::slice::from_raw_parts(p, n)` over `n` initialised `T`s whose
/// permissions `perms` holds from key `j0` on, element `i`'s at key
/// `j0 + i`, at `p`, `p + size_of::<T>()`, ... (a slice view of
/// permissions, which vstd lacks). Trusted: such permissions give what
/// `from_raw_parts` asks for (every element initialised, aligned and
/// valid for reads, `p` non-null since a permission's pointer is), the
/// slice's elements are the permissions' values, and the slice lives as
/// long as their borrow, so no permission-holder writes them meanwhile
/// (a type with interior mutability, such as `AtomicU64`, still changes
/// through the slice's shared references, as through the permissions'
/// pointers). Premise, not in the `requires`: the `n` elements lie in one
/// allocation, which `from_raw_parts` also asks; consecutive permissions
/// carved from one `PointsToRaw` do (`vspec::ptr::axiom_raw_valid`), as
/// at the one caller, `class::table::words_ref` (the class bitmap words,
/// from one reservation).
#[verifier::external_body]
#[inline(always)]
pub fn slice_from_perms<'a, T>(
    p: *const T,
    n: usize,
    Ghost(j0): Ghost<int>,
    Tracked(perms): Tracked<&'a Map<int, vstd::raw_ptr::PointsTo<T>>>,
) -> (r: &'a [T])
    requires
        0 < n,
        n * vstd::layout::size_of::<T>() <= isize::MAX,
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
    // SAFETY: the precondition.
    unsafe { core::slice::from_raw_parts(p, n) }
}

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
/// and an element read through the slice before it is written holds a
/// value of `T` (`from_raw_parts_mut` asks for initialised elements,
/// which a permission that is not initialised does not show). The one
/// caller is `slice_mut_from_perms`, for `heap::stack_mut`, which passes
/// a cache stack's `*mut u8` entries (in the stack's allocation,
/// `heap::slot_in`); `stack_mut`'s callers read an entry only after a
/// claim or a push wrote it.
#[verifier::external_body]
#[inline(always)]
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
