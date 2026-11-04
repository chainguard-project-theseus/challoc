//! `core::slice` functions.

use vstd::prelude::*;

verus! {

/// `core::slice::from_raw_parts`, with the safety contract of the `core`
/// documentation that can be stated here: `data` is not null, aligned,
/// and valid for reads of the slice's `len * size_of::<T>()` bytes, at
/// most `isize::MAX`. The read bound must be the read-only one a C
/// caller's argument has (`r_len`, which only `vspec::cbuf`'s facts
/// bound), not the read-write one (`rw_len`) of memory verified code
/// holds a permission to (`vspec::ptr::axiom_raw_readable`), whose bytes
/// it may write through that permission while the slice lives. Not
/// stated: that the elements are initialised `T`s and unchanged for `'a`
/// (the one caller, `mallctl`'s `name_bytes`, reads the caller's string
/// for the call and ties `'a` to the call's token). The length is the
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

/// `s.is_sorted()`. Nothing is claimed about the result.
pub assume_specification<T: PartialOrd>[ <[T]>::is_sorted ](s: &[T]) -> bool;

/// `s.is_sorted_by_key(f)`. Nothing is claimed about the result.
pub assume_specification<'a, T, F: FnMut(&'a T) -> K, K: PartialOrd>[ <[T]>::is_sorted_by_key::<'a, F, K> ](s: &'a [T], f: F) -> bool
    requires
        forall|x: &'a T| #[trigger] f.requires((x,)),
;

/// `s.select_nth_unstable(index)` on a slice of raw pointers, its
/// returned parts dropped: permutes `s`. Trusted: `core` documents a
/// panic when `index` is out of bounds and for an `Ord` that is not a
/// total order, and a thin pointer's `Ord` compares addresses, a total
/// order that does not panic, so with `index` in bounds the call returns
/// with `s` a permutation of what it held. A wrapper for `*mut u8`
/// alone, where an `assume_specification` of
/// `<[T]>::select_nth_unstable` would cover every `T: Ord`; it returns
/// nothing, so no caller writes through the parts (reborrows of `s`)
/// that `select_nth_unstable` hands back, and the contract can state the
/// permutation of `final(s)`. `#[inline(always)]` leaves the machine code
/// of the bare call.
#[inline(always)]
#[verifier::external_body]
pub fn select_ptrs(s: &mut [*mut u8], index: usize)
    requires
        index < old(s)@.len(),
    ensures
        final(s)@.len() == old(s)@.len(),
        final(s)@.to_multiset() == old(s)@.to_multiset(),
        forall|i: int| 0 <= i < old(s)@.len() ==> old(s)@.contains(#[trigger] final(s)@[i]),
{
    s.select_nth_unstable(index);
}

/// `s.select_nth_unstable_by_key(index, f)` on a slice of raw pointers
/// with a `usize` key, its returned parts dropped: permutes `s`. Trusted
/// as `select_ptrs`, with `usize`'s `Ord`, a total order that does not
/// panic, and `f` callable on every entry. A wrapper for `*mut u8`
/// entries and `usize` keys alone, where an `assume_specification` of
/// `<[T]>::select_nth_unstable_by_key` would cover every `K: Ord`.
/// `#[inline(always)]` leaves the machine code of the bare call.
#[inline(always)]
#[verifier::external_body]
pub fn select_ptrs_by_key<F: FnMut(&*mut u8) -> usize>(s: &mut [*mut u8], index: usize, f: F)
    requires
        index < old(s)@.len(),
        forall|x: &*mut u8| #[trigger] f.requires((x,)),
    ensures
        final(s)@.len() == old(s)@.len(),
        final(s)@.to_multiset() == old(s)@.to_multiset(),
        forall|i: int| 0 <= i < old(s)@.len() ==> old(s)@.contains(#[trigger] final(s)@[i]),
{
    s.select_nth_unstable_by_key(index, f);
}

/// `s.sort_unstable_by_key(f)` on a slice of raw pointers with a `usize`
/// key: permutes `s`. Trusted: `core` documents panics for an `Ord` that
/// is not a total order or that panics itself, and `usize`'s `Ord` is a
/// total order that does not panic, so with `f` callable on every entry
/// the call returns with `s` a permutation of what it held. A wrapper for
/// `*mut u8` entries and `usize` keys alone, where an
/// `assume_specification` of `<[T]>::sort_unstable_by_key` would cover
/// every `K: Ord`. `#[inline(always)]` leaves the machine code of the
/// bare call.
#[inline(always)]
#[verifier::external_body]
pub fn sort_ptrs_by_key<F: FnMut(&*mut u8) -> usize>(s: &mut [*mut u8], f: F)
    requires
        forall|x: &*mut u8| #[trigger] f.requires((x,)),
    ensures
        final(s)@.len() == old(s)@.len(),
        final(s)@.to_multiset() == old(s)@.to_multiset(),
        forall|i: int| 0 <= i < old(s)@.len() ==> old(s)@.contains(#[trigger] final(s)@[i]),
{
    s.sort_unstable_by_key(f)
}

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
/// permission again and no other, with its pointer, initialised to the
/// slice's final value there. Trusted: such permissions give what `from_raw_parts_mut` asks
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
        final(elems).dom() == old(elems).dom(),
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
        final(perms).dom() == old(perms).dom(),
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

/// Kani checks of `slice_from_perms` and `slice_mut_elems`
/// (`verus/ledger/src/vspec/slice.rs.md`; `cfg(kani)` only, see
/// `verus/README.md`, "Kani"). The permissions are ghost, so each harness
/// passes erased ones and reads them as the `requires` describe them: the
/// elements of one array, consecutive from the pointer, initialised.
#[cfg(kani)]
mod kani_perms {
    use super::{slice_from_perms, slice_mut_elems};
    use core::sync::atomic::{AtomicU64, Ordering};
    use vstd::prelude::{Ghost, Tracked};

    /// The longest array checked.
    const N: usize = 5;

    /// `slice_from_perms` on an array of `N` words: from every `i0`, every
    /// `n > 0` with `i0 + n <= N` (the permissions of the `n` elements from
    /// `i0`, at consecutive addresses, initialised): the pointer and length
    /// are what `from_raw_parts` requires (`can_dereference` of the
    /// slice: non-null, aligned, in bounds, valid values), `r.len() == n`,
    /// and `r[i]` is element `i0 + i`, the permission `j0 + i`'s value, at
    /// its address.
    fn from_perms_on<T: Copy + PartialEq + kani::Arbitrary>() {
        let a: [T; N] = kani::any();
        let i0: usize = kani::any();
        let n: usize = kani::any();
        kani::assume(0 < n && i0 <= N && n <= N - i0);
        // SAFETY: `i0 <= N`: inside the array or one past it.
        let p = unsafe { a.as_ptr().add(i0) };
        assert!(kani::mem::can_dereference(core::ptr::slice_from_raw_parts(
            p, n
        )));
        // SAFETY: the contract's precondition, with the `n` elements in
        // one allocation (the array).
        let r = unsafe { slice_from_perms(p, n, Ghost::assume_new(), Tracked::assume_new()) };
        assert_eq!(r.len(), n);
        for i in 0..n {
            assert!(r[i] == a[i0 + i]);
            assert!(core::ptr::eq(&r[i], &a[i0 + i]));
        }
    }

    /// `slice_from_perms`, `ensures r@.len() == n` and `r@[i] ==
    /// perms[j0 + i].value()`: at a word, a padded pair, and an
    /// `AtomicU64` (the class table's bitmap words, its one caller),
    /// whose values are read through the slice with `load`.
    #[kani::proof]
    #[kani::unwind(7)]
    fn slice_from_perms_elems() {
        from_perms_on::<u64>();
        from_perms_on::<(u64, u8)>();
        let v: [u64; N] = kani::any();
        let a = v.map(AtomicU64::new);
        let i0: usize = kani::any();
        let n: usize = kani::any();
        kani::assume(0 < n && i0 <= N && n <= N - i0);
        // SAFETY: as in `from_perms_on`.
        let p = unsafe { a.as_ptr().add(i0) };
        assert!(kani::mem::can_dereference(core::ptr::slice_from_raw_parts(
            p, n
        )));
        // SAFETY: as in `from_perms_on`.
        let r = unsafe { slice_from_perms(p, n, Ghost::assume_new(), Tracked::assume_new()) };
        assert_eq!(r.len(), n);
        for i in 0..n {
            assert_eq!(r[i].load(Ordering::Relaxed), v[i0 + i]);
            assert!(core::ptr::eq(&r[i], &a[i0 + i]));
        }
    }

    /// `slice_mut_elems` on an array of `N` `T`s at `base`: from every
    /// `start`, every `n > 0` with `start + n <= N` (the permissions of
    /// elements `start..start + n`, keyed `base.add(i)`), the slice is
    /// what `from_raw_parts_mut` requires (`can_dereference`), `r.len() ==
    /// n`, it starts out holding the elements, and a write through `r[k]`
    /// is the array's element `start + k` afterwards (`final(elems)`
    /// holds `final(r)`), every other element unchanged.
    fn mut_elems_on<T: Copy + PartialEq>(any: fn() -> T) {
        let mut a: [T; N] = [any(); N];
        for e in a.iter_mut() {
            *e = any();
        }
        let old = a;
        let start: usize = kani::any();
        let n: usize = kani::any();
        kani::assume(0 < n && start <= N && n <= N - start);
        let base = a.as_mut_ptr();
        // SAFETY: `start <= N`: inside the array or one past it.
        let s = core::ptr::slice_from_raw_parts(unsafe { base.add(start) } as *const T, n);
        assert!(kani::mem::can_dereference(s));
        // SAFETY: the contract's precondition and premises: the elements
        // lie in one array, `T` is not zero-sized, all are initialised.
        let r = unsafe { slice_mut_elems(base, start, n, Tracked::assume_new()) };
        assert_eq!(r.len(), n);
        for i in 0..n {
            assert!(r[i] == old[start + i]);
        }
        let k: usize = kani::any();
        kani::assume(k < n);
        let v = any();
        r[k] = v;
        for i in 0..N {
            assert!(a[i] == if i == start + k { v } else { old[i] });
        }
    }

    /// `slice_mut_elems`, `ensures r@.len() == n`, the elements'
    /// permissions afterwards initialised to `final(r)`, and `r@` the
    /// initialised elements' old values: at a word and at `*mut u8` (a
    /// cache stack's entries, its one caller's).
    #[kani::proof]
    #[kani::unwind(7)]
    fn slice_mut_elems_writes_through() {
        mut_elems_on::<u64>(kani::any);
        mut_elems_on::<*mut u8>(|| core::ptr::without_provenance_mut(kani::any()));
    }
}
