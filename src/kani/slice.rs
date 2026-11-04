//! The slice and array methods of `core` that `vspec/slice.rs`,
//! `vspec/core_int.rs` and `vspec/sys.rs` specify, checked against their
//! `assume_specification`s on `u64` elements (the proofs instantiate them
//! at `*mut u8`, `usize` and pairs of `usize`, all plain `Copy` words, and
//! none of these methods looks at the element type beyond moving or
//! comparing it) for every slice up to `MAX` elements (`SORTED_MAX` for
//! `is_sorted`) with arbitrary contents (`verus/ledger/src/vspec/`).
//! The trusted wrappers `vspec::slice::sort_ptrs` and `sort_ptrs_by_key`
//! are checked at every length to `SORT_MAX` on pointers at addresses
//! below the length (every order pattern; see [`sort_ranks_on`]), which
//! reaches only the insertion sort of `core`'s `sort_unstable` (to 20
//! elements; a small sort and a quicksort run past that).
//! `from_raw_parts` is also checked at bytes, and `rotate_left` on two
//! longer slices, the shortest that reach its second algorithm.
//! `reverse`, which vstd specifies, is checked the same way.
//! The trusted wrappers `vspec::slice::select_ptrs` and
//! `select_ptrs_by_key` are checked as `sort_ptrs` is, at every length to
//! `SORT_MAX` and every index (see [`select_ranks_on`]), which reaches
//! `core`'s `select_nth_unstable` on its three short paths only (the
//! largest or smallest entry swapped into place at the last or first
//! index, an insertion sort at any other index, to 16 elements; a
//! quickselect runs past that, while the flush selects on slices of up
//! to a cache's 1024 slots).

/// The longest slice checked. The methods checked at this bound run one
/// loop whatever the length.
const MAX: usize = 6;

/// The longest slice `is_sorted` is checked on: past 33 elements it
/// compares in chunks, a second path: 40 runs one chunk and the tail.
const SORTED_MAX: usize = 40;

/// A slice of `MAX` arbitrary words and an arbitrary length up to `MAX`.
fn any_words() -> ([u64; MAX], usize) {
    let a: [u64; MAX] = kani::any();
    let n: usize = kani::any();
    kani::assume(n <= MAX);
    (a, n)
}

/// `a == b`, element by element (slice `==` on words is a `memcmp` of
/// their bytes, whose loop would need eight times the unwinding).
fn same<T: PartialEq>(a: &[T], b: &[T]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for k in 0..a.len() {
        if a[k] != b[k] {
            return false;
        }
    }
    true
}

/// `<[T]>::swap`: under `a, b < len`, `old.update(a, old[b])
/// .update(b, old[a])`.
#[kani::proof]
#[kani::unwind(8)]
fn slice_swap() {
    let (mut a, n) = any_words();
    let old = a;
    let i: usize = kani::any();
    let j: usize = kani::any();
    kani::assume(i < n && j < n);
    a[..n].swap(i, j);
    let mut want = old;
    want[i] = old[j];
    want[j] = old[i];
    assert!(same(&a, &want));
}

/// vstd's specification of `<[T]>::reverse`, which verified code uses
/// (`heap/compact.rs` and `heap/flush.rs`) and the ledger does not list:
/// `old.reverse()`, element `k` is `old[n - 1 - k]`.
#[kani::proof]
#[kani::unwind(8)]
fn slice_reverse() {
    let (mut a, n) = any_words();
    let old = a;
    a[..n].reverse();
    for k in 0..n {
        assert_eq!(a[k], old[n - 1 - k]);
    }
    assert!(same(&a[n..], &old[n..]));
}

/// A slice of `SORTED_MAX` arbitrary words and an arbitrary length up to
/// it.
fn any_long_words() -> ([u64; SORTED_MAX], usize) {
    let a: [u64; SORTED_MAX] = kani::any();
    let n: usize = kani::any();
    kani::assume(n <= SORTED_MAX);
    (a, n)
}

/// `<[T]>::is_sorted`: no `requires`, no `ensures`; it returns
/// without a panic or undefined behaviour (and, here, says whether each
/// element is at most the next), for every slice up to `SORTED_MAX`.
#[kani::proof]
#[kani::unwind(42)]
fn slice_is_sorted() {
    let (a, n) = any_long_words();
    let r = a[..n].is_sorted();
    assert_eq!(r, (1..n).all(|k| a[k - 1] <= a[k]));
}

/// `<[T]>::is_sorted_by_key::<'a, F, K>`: as `is_sorted`, for a key `x ^ c`
/// with an arbitrary `c`.
#[kani::proof]
#[kani::unwind(8)]
fn slice_is_sorted_by_key() {
    let (a, n) = any_words();
    let c: u64 = kani::any();
    let key = |x: &u64| x ^ c;
    let r = a[..n].is_sorted_by_key(key);
    assert_eq!(r, (1..n).all(|k| key(&a[k - 1]) <= key(&a[k])));
}

/// `<[T]>::get_unchecked::<I>`: under the `index` precondition (in
/// bounds), the element or subslice `index` gives, at the instantiations
/// the proofs use: `usize`,
/// `RangeTo` and `RangeFrom`. Kani's undefined-behaviour checks cover the
/// unchecked access itself.
#[kani::proof]
#[kani::unwind(8)]
fn slice_get_unchecked() {
    let (a, n) = any_words();
    let s = &a[..n];
    let i: usize = kani::any();
    kani::assume(i < n);
    // SAFETY: `i < s.len()`.
    let e = unsafe { s.get_unchecked(i) };
    assert!(core::ptr::eq(e, &s[i]));
    let k: usize = kani::any();
    kani::assume(k <= n);
    // SAFETY: `k <= s.len()`.
    let lo = unsafe { s.get_unchecked(..k) };
    assert!(core::ptr::eq(lo, &s[..k]));
    // SAFETY: `k <= s.len()`.
    let hi = unsafe { s.get_unchecked(k..) };
    assert!(core::ptr::eq(hi, &s[k..]));
}

/// `<[T]>::get_unchecked_mut::<I>`: as `get_unchecked`, and a write
/// through the result is the write `index_mut` would make.
#[kani::proof]
#[kani::unwind(8)]
fn slice_get_unchecked_mut() {
    let (mut a, n) = any_words();
    let old = a;
    let i: usize = kani::any();
    kani::assume(i < n);
    let v: u64 = kani::any();
    let s = &mut a[..n];
    let want: *const u64 = &s[i];
    // SAFETY: `i < s.len()`.
    let e = unsafe { s.get_unchecked_mut(i) };
    assert!(core::ptr::eq(e, want));
    *e = v;
    let mut b = old;
    b[i] = v;
    assert!(same(&a, &b));
    let k: usize = kani::any();
    kani::assume(k <= n);
    let s = &mut a[..n];
    let (want_lo, want_hi): (*const [u64], *const [u64]) = (&s[..k], &s[k..]);
    // SAFETY: `k <= s.len()`.
    assert!(core::ptr::eq(unsafe { s.get_unchecked_mut(..k) }, want_lo));
    // SAFETY: `k <= s.len()`.
    assert!(core::ptr::eq(unsafe { s.get_unchecked_mut(k..) }, want_hi));
}

/// `core::slice::from_ref`: the one-element sequence of `*s`.
#[kani::proof]
fn slice_from_ref() {
    let x: u64 = kani::any();
    let r = core::slice::from_ref(&x);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0], x);
}

/// `<&'a mut [T] as core::iter::IntoIterator>::into_iter`: the iterator
/// hands out each element once, in order, holding its old value, and
/// what is written through each reference is the slice's final value
/// there (`remaining` is the sequence of references; the prophetic laws
/// are that `next` returns them in order and then `None`).
#[kani::proof]
#[kani::unwind(8)]
fn slice_iter_mut() {
    let (mut a, n) = any_words();
    let old = a;
    let new: [u64; MAX] = kani::any();
    let mut seen = 0;
    let mut it = (&mut a[..n]).into_iter();
    assert_eq!(it.len(), n);
    loop {
        match it.next() {
            Some(e) => {
                assert!(seen < n);
                assert_eq!(*e, old[seen]);
                *e = new[seen];
                seen += 1;
            }
            None => break,
        }
    }
    assert_eq!(seen, n);
    assert!(it.next().is_none());
    assert!(same(&a[..n], &new[..n]));
    assert!(same(&a[n..], &old[n..]));
}

/// `<[T; N] as core::iter::IntoIterator>::into_iter` and its `next`: the
/// iterator starts with every element in order, `next` hands out the
/// front one, and returns `None` (and stays empty) once all are out.
#[kani::proof]
fn array_into_iter() {
    let a: [u64; 3] = kani::any();
    let mut it = a.into_iter();
    assert!(same(it.as_slice(), &a));
    assert_eq!(it.next(), Some(a[0]));
    assert!(same(it.as_slice(), &a[1..]));
    assert_eq!(it.next(), Some(a[1]));
    assert_eq!(it.next(), Some(a[2]));
    assert_eq!(it.as_slice().len(), 0);
    assert_eq!(it.next(), None);
    assert_eq!(it.as_slice().len(), 0);
    assert_eq!(it.next(), None);
    let e: [u64; 0] = [];
    let mut it = e.into_iter();
    assert_eq!(it.next(), None);
}

/// `<[T]>::as_mut_ptr` (`ensures` the length is kept) and `<[T]>::as_ptr`
/// (no `ensures`): they return without a panic; beyond the contracts, the
/// address is the first element's when there is one, and the contents
/// are kept.
#[kani::proof]
#[kani::unwind(8)]
fn slice_as_ptr() {
    let (mut a, n) = any_words();
    let old = a;
    let p = a[..n].as_ptr();
    let q = a[..n].as_mut_ptr();
    assert_eq!(p, q as *const u64);
    if n > 0 {
        assert!(core::ptr::eq(p, &a[0]));
    }
    assert!(same(&a, &old));
}

/// The longest slice `sort_ptrs` and `select_ptrs` are checked on, at
/// every length up to it: `core`'s `sort_unstable` sorts these by
/// insertion alone (up to 20 elements), and `select_nth_unstable` takes
/// its short paths (insertion up to 16).
const SORT_MAX: usize = 12;

/// `old` and `new` hold `v` equally often.
fn same_count<const L: usize>(old: &[*mut u8; L], new: &[*mut u8; L], v: *mut u8) -> bool {
    let (mut a, mut b) = (0usize, 0usize);
    for k in 0..L {
        a += (old[k] == v) as usize;
        b += (new[k] == v) as usize;
    }
    a == b
}

/// `sort_ptrs` (or, `by_key`, `sort_ptrs_by_key` with the key `L - addr`)
/// on a slice of `L` pointers at arbitrary addresses below `L`: the
/// length is kept, an arbitrary entry, old or new, occurs as often after
/// as before (so the multiset is kept, and every entry is one of the old
/// ones), and the addresses end sorted (descending, `by_key`). Addresses below
/// `L` give every order pattern `L` pointers can have, ties included
/// (each is its own dense rank), which is all a comparison sort sees,
/// and a thin pointer's `Ord` compares addresses alone. `L` is a
/// constant: with a symbolic length CBMC would also unwind every longer
/// algorithm `core` picks by length.
fn sort_ranks_on<const L: usize>(by_key: bool) {
    let mut s: [*mut u8; L] = [core::ptr::null_mut(); L];
    for e in s.iter_mut() {
        let r: usize = kani::any();
        kani::assume(r < L);
        *e = core::ptr::without_provenance_mut(r);
    }
    let old = s;
    let v: &mut [*mut u8] = &mut s;
    if by_key {
        crate::vspec::slice::sort_ptrs_by_key(v, |p| L - *p as usize);
    } else {
        crate::vspec::slice::sort_ptrs(v);
    }
    assert!(v.len() == L);
    let i: usize = kani::any();
    if i < L {
        assert!(same_count(&old, &s, old[i]));
        assert!(same_count(&old, &s, s[i]));
    }
    for k in 1..L {
        let (a, b) = (s[k - 1] as usize, s[k] as usize);
        assert!(if by_key { a >= b } else { a <= b });
    }
}

/// `sort_ptrs`: no `requires`; the length and the multiset are kept, and
/// every entry is one of the old ones, for every slice of up to
/// `SORT_MAX` pointers: here the lengths to 9, and each longer length in
/// a harness of its own, which keeps each harness to a few minutes.
#[kani::proof]
#[kani::unwind(14)]
fn sort_ptrs_short() {
    sort_ranks_on::<0>(false);
    sort_ranks_on::<1>(false);
    sort_ranks_on::<2>(false);
    sort_ranks_on::<3>(false);
    sort_ranks_on::<4>(false);
    sort_ranks_on::<5>(false);
    sort_ranks_on::<6>(false);
    sort_ranks_on::<7>(false);
    sort_ranks_on::<8>(false);
    sort_ranks_on::<9>(false);
}

/// `sort_ptrs` on 10 pointers (`sort_ptrs_short`).
#[kani::proof]
#[kani::unwind(14)]
fn sort_ptrs_10() {
    sort_ranks_on::<10>(false);
}

/// `sort_ptrs` on 11 pointers (`sort_ptrs_short`).
#[kani::proof]
#[kani::unwind(14)]
fn sort_ptrs_11() {
    sort_ranks_on::<11>(false);
}

/// `sort_ptrs` on `SORT_MAX` pointers (`sort_ptrs_short`).
#[kani::proof]
#[kani::unwind(14)]
fn sort_ptrs_12() {
    const _: () = assert!(SORT_MAX == 12);
    sort_ranks_on::<12>(false);
}

/// `sort_ptrs_by_key`, with the key `L - addr` (so the addresses end in
/// descending order): the length and the multiset are kept, and every
/// entry is one of the old ones, for every slice of up to `SORT_MAX`
/// pointers, split by length as for `sort_ptrs`. Every address is below
/// `L`, so the key never overflows; the harness does not check the
/// contract's `forall f.requires` for a pointer above `L`.
#[kani::proof]
#[kani::unwind(14)]
fn sort_ptrs_by_key_short() {
    sort_ranks_on::<0>(true);
    sort_ranks_on::<1>(true);
    sort_ranks_on::<2>(true);
    sort_ranks_on::<3>(true);
    sort_ranks_on::<4>(true);
    sort_ranks_on::<5>(true);
    sort_ranks_on::<6>(true);
    sort_ranks_on::<7>(true);
    sort_ranks_on::<8>(true);
    sort_ranks_on::<9>(true);
}

/// `sort_ptrs_by_key` on 10 pointers (`sort_ptrs_by_key_short`).
#[kani::proof]
#[kani::unwind(14)]
fn sort_ptrs_by_key_10() {
    sort_ranks_on::<10>(true);
}

/// `sort_ptrs_by_key` on 11 pointers (`sort_ptrs_by_key_short`).
#[kani::proof]
#[kani::unwind(14)]
fn sort_ptrs_by_key_11() {
    sort_ranks_on::<11>(true);
}

/// `sort_ptrs_by_key` on `SORT_MAX` pointers (`sort_ptrs_by_key_short`).
#[kani::proof]
#[kani::unwind(14)]
fn sort_ptrs_by_key_12() {
    sort_ranks_on::<12>(true);
}

/// `select_ptrs` (or, `by_key`, `select_ptrs_by_key` with the key
/// `L - addr`) at an arbitrary index below `L` on a slice of `L`
/// pointers at arbitrary addresses below `L`, as [`sort_ranks_on`]: the
/// length is kept, an arbitrary entry, old or new, occurs as often after
/// as before, and (beyond the contract) the entry at the index splits the
/// slice: no entry before it has a higher address and no entry after it
/// a lower one (the other way round, `by_key`). `L` is a constant and at
/// least 1, the contract's `index < len` for some index.
fn select_ranks_on<const L: usize>(by_key: bool) {
    let mut s: [*mut u8; L] = [core::ptr::null_mut(); L];
    for e in s.iter_mut() {
        let r: usize = kani::any();
        kani::assume(r < L);
        *e = core::ptr::without_provenance_mut(r);
    }
    let old = s;
    let index: usize = kani::any();
    kani::assume(index < L);
    let v: &mut [*mut u8] = &mut s;
    if by_key {
        crate::vspec::slice::select_ptrs_by_key(v, index, |p| L - *p as usize);
    } else {
        crate::vspec::slice::select_ptrs(v, index);
    }
    assert!(v.len() == L);
    let i: usize = kani::any();
    if i < L {
        assert!(same_count(&old, &s, old[i]));
        assert!(same_count(&old, &s, s[i]));
    }
    let m = s[index] as usize;
    for k in 0..L {
        let a = s[k] as usize;
        if k < index {
            assert!(if by_key { a >= m } else { a <= m });
        }
        if k > index {
            assert!(if by_key { a <= m } else { a >= m });
        }
    }
}

/// `select_ptrs`: `requires index < len`; the length and the multiset
/// are kept, every entry is one of the old ones, and the entry at the
/// index splits the slice, for every slice of up to `SORT_MAX` pointers
/// and every index in it, split by length as for `sort_ptrs`.
#[kani::proof]
#[kani::unwind(14)]
fn select_ptrs_short() {
    select_ranks_on::<1>(false);
    select_ranks_on::<2>(false);
    select_ranks_on::<3>(false);
    select_ranks_on::<4>(false);
    select_ranks_on::<5>(false);
    select_ranks_on::<6>(false);
    select_ranks_on::<7>(false);
    select_ranks_on::<8>(false);
    select_ranks_on::<9>(false);
}

/// `select_ptrs` on 10 pointers (`select_ptrs_short`).
#[kani::proof]
#[kani::unwind(14)]
fn select_ptrs_10() {
    select_ranks_on::<10>(false);
}

/// `select_ptrs` on 11 pointers (`select_ptrs_short`).
#[kani::proof]
#[kani::unwind(14)]
fn select_ptrs_11() {
    select_ranks_on::<11>(false);
}

/// `select_ptrs` on `SORT_MAX` pointers (`select_ptrs_short`).
#[kani::proof]
#[kani::unwind(14)]
fn select_ptrs_12() {
    const _: () = assert!(SORT_MAX == 12);
    select_ranks_on::<12>(false);
}

/// `select_ptrs_by_key`, with the key `L - addr`: as `select_ptrs`; and,
/// as for `sort_ptrs_by_key`, the key never overflows, and the harness
/// does not check the contract's `forall f.requires` for a pointer above
/// `L`.
#[kani::proof]
#[kani::unwind(14)]
fn select_ptrs_by_key_short() {
    select_ranks_on::<1>(true);
    select_ranks_on::<2>(true);
    select_ranks_on::<3>(true);
    select_ranks_on::<4>(true);
    select_ranks_on::<5>(true);
    select_ranks_on::<6>(true);
    select_ranks_on::<7>(true);
    select_ranks_on::<8>(true);
    select_ranks_on::<9>(true);
}

/// `select_ptrs_by_key` on 10 pointers (`select_ptrs_by_key_short`).
#[kani::proof]
#[kani::unwind(14)]
fn select_ptrs_by_key_10() {
    select_ranks_on::<10>(true);
}

/// `select_ptrs_by_key` on 11 pointers (`select_ptrs_by_key_short`).
#[kani::proof]
#[kani::unwind(14)]
fn select_ptrs_by_key_11() {
    select_ranks_on::<11>(true);
}

/// `select_ptrs_by_key` on `SORT_MAX` pointers (`select_ptrs_by_key_short`).
#[kani::proof]
#[kani::unwind(14)]
fn select_ptrs_by_key_12() {
    select_ranks_on::<12>(true);
}

/// `from_raw_parts` at `T` on an array of `N` `T`s: from element `i`
/// (`i <= N`), every `len` with `i + len <= N` (the contract's `r_len`
/// bound, read as "the `len` elements lie inside the array"; an array
/// element is non-null and aligned, and the array is under `isize::MAX`
/// bytes): the pointer and length are what `core` requires
/// (`can_dereference` of the slice: non-null, aligned, in bounds, valid
/// `T`s), the slice has length `len`, and (beyond the contract) its
/// elements are the array's from `i`, at their addresses.
fn from_raw_parts_on<T: Copy + PartialEq + kani::Arbitrary, const N: usize>() {
    let a: [T; N] = kani::any();
    let i: usize = kani::any();
    let len: usize = kani::any();
    kani::assume(i <= N && len <= N - i);
    // SAFETY: `i <= N`: inside the array or one past it.
    let data = unsafe { a.as_ptr().add(i) };
    assert!(kani::mem::can_dereference(core::ptr::slice_from_raw_parts(
        data, len
    )));
    // SAFETY: the contract's precondition, checked above.
    let r = unsafe { core::slice::from_raw_parts(data, len) };
    assert_eq!(r.len(), len);
    for k in 0..len {
        assert!(r[k] == a[i + k]);
        assert!(core::ptr::eq(&r[k], &a[i + k]));
    }
}

/// `core::slice::from_raw_parts::<'a, T>`: `r@.len() == len`, under the
/// `requires`, at a byte (`mallctl`'s `name_bytes`, the one verified caller)
/// and a word, for every in-array start and length up to `MAX`.
#[kani::proof]
#[kani::unwind(8)]
fn slice_from_raw_parts() {
    from_raw_parts_on::<u8, MAX>();
    from_raw_parts_on::<u64, MAX>();
}

/// `swap_with_slice` on two arrays of `MAX` `T`s, at every length `n`.
fn swap_with_slice_on<T: Copy + PartialEq>(any: fn() -> T) {
    for n in 0..=MAX {
        let mut a: [T; MAX] = [any(); MAX];
        let mut b: [T; MAX] = [any(); MAX];
        for k in 0..MAX {
            a[k] = any();
            b[k] = any();
        }
        let (old_a, old_b) = (a, b);
        a[..n].swap_with_slice(&mut b[..n]);
        assert!(same(&a[..n], &old_b[..n]));
        assert!(same(&b[..n], &old_a[..n]));
        assert!(same(&a[n..], &old_a[n..]));
        assert!(same(&b[n..], &old_b[n..]));
    }
}

/// `<[T]>::swap_with_slice`: under equal lengths, `final(a)@ == old(b)@`
/// and `final(b)@ == old(a)@`, for every pair of slices up to `MAX`
/// elements (here of two arrays, past whose slices nothing changes), of
/// words and of `*mut u8` (a cache stack's entries, verified code's one
/// instance). The length is a constant at each step, not symbolic (as in
/// `ptr.rs`: Kani 0.68 can model a copy of a symbolic count short by an
/// element).
#[kani::proof]
#[kani::unwind(8)]
fn slice_swap_with_slice() {
    swap_with_slice_on::<u64>(kani::any);
    swap_with_slice_on::<*mut u8>(|| core::ptr::without_provenance_mut(kani::any()));
}

/// `s.rotate_left(mid)` on a slice of `L` arbitrary words, for every
/// `mid <= L` (each a constant, as in `slice_swap_with_slice`):
/// `final(s)@ == old(s)@[mid..] + old(s)@[..mid]`.
fn rotate_left_on<const L: usize>() {
    for mid in 0..=L {
        let mut s: [u64; L] = kani::any();
        let old = s;
        s.rotate_left(mid);
        for k in 0..L {
            assert_eq!(
                s[k],
                if k < L - mid {
                    old[mid + k]
                } else {
                    old[k - (L - mid)]
                }
            );
        }
    }
}

/// `s.rotate_left(mid)` on `L` words holding `0..L` (distinct values,
/// which identify every element), at one `mid`: the same equation.
fn rotate_left_distinct<const L: usize>(mid: usize) {
    let mut s = [0u64; L];
    for k in 0..L {
        s[k] = k as u64;
    }
    s.rotate_left(mid);
    for k in 0..L {
        assert_eq!(s[k], ((mid + k) % L) as u64);
    }
}

/// `<[T]>::rotate_left`: under `mid <= len`, `final(s)@ ==
/// old(s)@.subrange(mid, len) + old(s)@.subrange(0, mid)`. On words,
/// `core` picks one of two algorithms: a copy through a 256-byte stack
/// buffer when `min(mid, len - mid)` is at most 32 elements, and block
/// swaps past that. This harness checks the buffer path, for every slice
/// up to `MAX` arbitrary words and every `mid`; `slice_rotate_left_swap`
/// checks the other.
#[kani::proof]
#[kani::unwind(8)]
fn slice_rotate_left() {
    rotate_left_on::<0>();
    rotate_left_on::<1>();
    rotate_left_on::<2>();
    rotate_left_on::<3>();
    rotate_left_on::<4>();
    rotate_left_on::<5>();
    rotate_left_on::<MAX>();
}

/// `<[T]>::rotate_left`'s block-swap path (`slice_rotate_left`), on 66
/// and 67 words (the shortest slices that reach it) at the `mid`s that
/// reach it (33; 33 and 34), with distinct values: the method moves
/// elements without reading them, so where each element goes is the
/// whole of its effect.
#[kani::proof]
#[kani::unwind(69)]
fn slice_rotate_left_swap() {
    rotate_left_distinct::<66>(33);
    rotate_left_distinct::<67>(33);
    rotate_left_distinct::<67>(34);
}
