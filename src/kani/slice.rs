//! The slice and array methods of `core` that `vspec/slice.rs`,
//! `vspec/core_int.rs` and `vspec/sys.rs` specify, checked against their
//! `assume_specification`s on `u64` elements (the proofs instantiate them
//! at `*mut u8`, `usize` and pairs of `usize`, all plain `Copy` words, and
//! none of these methods looks at the element type beyond moving or
//! comparing it) for every slice up to `MAX` elements (`SORTED_MAX` for
//! `is_sorted`) with arbitrary contents. `sort_unstable`,
//! `select_nth_unstable` and `rotate_left` have no harness: each switches
//! algorithm with the length (insertion sort below 21 elements, a small
//! sort to 32, partitioning above; three rotation algorithms), the
//! proofs use them on slices of thousands, and Kani reaches only the
//! short branches.

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

/// S331 `<[T]>::swap`: under `a, b < len`, `old.update(a, old[b])
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

/// S332 `<[T]>::reverse`: `old.reverse()`, element `k` is `old[n - 1 - k]`.
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

/// S340 `<[T]>::swap_with_slice`: under equal lengths, the contents trade
/// places.
#[kani::proof]
#[kani::unwind(8)]
fn slice_swap_with_slice() {
    let (mut a, n) = any_words();
    let mut b: [u64; MAX] = kani::any();
    let (old_a, old_b) = (a, b);
    a[..n].swap_with_slice(&mut b[..n]);
    assert!(same(&a[..n], &old_b[..n]));
    assert!(same(&b[..n], &old_a[..n]));
}

/// A slice of `SORTED_MAX` arbitrary words and an arbitrary length up to
/// it.
fn any_long_words() -> ([u64; SORTED_MAX], usize) {
    let a: [u64; SORTED_MAX] = kani::any();
    let n: usize = kani::any();
    kani::assume(n <= SORTED_MAX);
    (a, n)
}

/// S333 `<[T]>::is_sorted`: no `requires`, no `ensures`; it returns
/// without a panic or undefined behaviour (and, here, says whether each
/// element is at most the next), for every slice up to `SORTED_MAX`.
#[kani::proof]
#[kani::unwind(42)]
fn slice_is_sorted() {
    let (a, n) = any_long_words();
    let r = a[..n].is_sorted();
    assert_eq!(r, (1..n).all(|k| a[k - 1] <= a[k]));
}

/// S334 `<[T]>::is_sorted_by_key`: as `is_sorted`, for a key `x ^ c`
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

/// S287 `<[T]>::get_unchecked`: under the `index` precondition (in
/// bounds), the element or subslice `index` gives, at the instantiations
/// the proofs use: `usize` (`heap/flush.rs`, `arena.rs`, `api/libc.rs`),
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

/// S339 `<[T]>::get_unchecked_mut`: as `get_unchecked`, and a write
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

/// S329 `core::slice::from_ref`: the one-element sequence of `*s`.
#[kani::proof]
fn slice_from_ref() {
    let x: u64 = kani::any();
    let r = core::slice::from_ref(&x);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0], x);
}

/// S330 `<&mut [T] as IntoIterator>::into_iter`: the iterator hands out
/// each element once, in order, holding its old value, and what is
/// written through each reference is the slice's final value there
/// (`remaining` is the sequence of references; the prophetic laws are
/// that `next` returns them in order and then `None`).
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

/// S343 `<[T; N] as IntoIterator>::into_iter` and S344 its `next`: the
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

/// S349 `<[T]>::as_mut_ptr` and S350 `<[T]>::as_ptr`: no `requires`, no
/// `ensures`; they return without a panic (the address is the first
/// element's when there is one).
#[kani::proof]
#[kani::unwind(8)]
fn slice_as_ptr() {
    let (mut a, n) = any_words();
    let p = a[..n].as_ptr();
    let q = a[..n].as_mut_ptr();
    assert_eq!(p, q as *const u64);
    if n > 0 {
        assert!(core::ptr::eq(p, &a[0]));
    }
}
