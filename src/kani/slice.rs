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
//! `select_nth_unstable` and `select_nth_unstable_by_key` have no
//! harness: they switch algorithm with the length, the proofs use them
//! on slices of thousands, and Kani reaches only the short branches.

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
fn same(a: &[u64], b: &[u64]) -> bool {
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

/// The longest slice `sort_ptrs` is checked on, at every length up to it:
/// `core`'s `sort_unstable` sorts these by insertion alone (up to 20
/// elements).
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
