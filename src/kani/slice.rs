//! The slice and array methods of `core` that `vspec/slice.rs`,
//! `vspec/core_int.rs` and `vspec/sys.rs` specify, checked against their
//! `assume_specification`s on `u64` elements (the proofs instantiate them
//! at `*mut u8`, `usize` and pairs of `usize`, all plain `Copy` words, and
//! none of these methods looks at the element type beyond moving or
//! comparing it) for every slice up to `MAX` elements (`SORTED_MAX` for
//! `is_sorted`) with arbitrary contents. `sort_unstable` is checked at
//! every length to `SORT_MAX` on words below the length (every order
//! pattern; see [`sort_ranks_on`]), which reaches only its insertion
//! sort (to 20 elements; a small sort and a quicksort run past that).
//! `select_nth_unstable` has no harness: it switches algorithm with the
//! length, the proofs use it on slices of thousands, and Kani reaches
//! only the short branches. `rotate_left` (three rotation
//! algorithms) is checked at lengths that reach each of them, on
//! labelled elements (see [`rotate_on`]).

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

/// S372 `<[T]>::swap`: under `a, b < len`, `old.update(a, old[b])
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

/// S373 `<[T]>::reverse`: `old.reverse()`, element `k` is `old[n - 1 - k]`.
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

/// S381 `<[T]>::swap_with_slice`: under equal lengths, the contents trade
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

/// S374 `<[T]>::is_sorted`: no `requires`, no `ensures`; it returns
/// without a panic or undefined behaviour (and, here, says whether each
/// element is at most the next), for every slice up to `SORTED_MAX`.
#[kani::proof]
#[kani::unwind(42)]
fn slice_is_sorted() {
    let (a, n) = any_long_words();
    let r = a[..n].is_sorted();
    assert_eq!(r, (1..n).all(|k| a[k - 1] <= a[k]));
}

/// S375 `<[T]>::is_sorted_by_key::<'a, F, K>`: as `is_sorted`, for a key `x ^ c`
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

/// S301 `<[T]>::get_unchecked::<I>`: under the `index` precondition (in
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

/// S380 `<[T]>::get_unchecked_mut::<I>`: as `get_unchecked`, and a write
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

/// S369 `core::slice::from_ref`: the one-element sequence of `*s`.
#[kani::proof]
fn slice_from_ref() {
    let x: u64 = kani::any();
    let r = core::slice::from_ref(&x);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0], x);
}

/// S371 `<&'a mut [T] as core::iter::IntoIterator>::into_iter`: the iterator hands out
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

/// S384 `<[T; N] as core::iter::IntoIterator>::into_iter` and S386 its `next`: the
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

/// S393 `<[T]>::as_mut_ptr` and S394 `<[T]>::as_ptr`: no `requires`, no
/// `ensures`; they return without a panic (the address is the first
/// element's when there is one).
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
    assert!(same(&a[..n], &old[..n]));
    assert!(same(&a, &old));
}

/// `s.rotate_left(mid)` on the first `n` of `L` elements labelled
/// `label(0..L)`, for `n == L` or every `n <= L` (`!fixed`), and every
/// `mid <= n`: element `k < n` ends up holding the label of `(k + mid) %
/// n` (`old.subrange(mid, n) + old.subrange(0, mid)`), and the elements
/// from `n` on keep theirs.
///
/// `rotate_left` has no bound on `T`: it cannot look at the values, so
/// its control flow depends only on the length, `mid` and `T`'s size and
/// alignment (parametricity), and every copy, chunked swap or typed move
/// in it runs between element-aligned offsets over whole elements, so it
/// permutes whole elements with their bytes in place. Labelling every
/// word distinctly then makes one run per `(n, mid)` show the
/// permutation, so the result for every content of a `T` of that size
/// and alignment; the contents need not be symbolic. `n` and `mid` run
/// over their values as constants (with a symbolic length, Kani 0.68
/// copies one element too few of 8-byte elements: `src/kani/ptr.rs`).
fn rotate_on<T: Copy + PartialEq, const L: usize>(label: fn(usize) -> T, fixed: bool) {
    rotate_mids::<T, L>(label, fixed, 0, L);
}

/// [`rotate_on`] for the `mid`s from `lo` to `hi` only (those up to `n`).
fn rotate_mids<T: Copy + PartialEq, const L: usize>(
    label: fn(usize) -> T,
    fixed: bool,
    lo: usize,
    hi: usize,
) {
    for n in if fixed { L } else { 0 }..=L {
        for mid in lo..=if hi < n { hi } else { n } {
            let mut a: [T; L] = core::array::from_fn(label);
            a[..n].rotate_left(mid);
            for k in 0..L {
                let want = if k < n { (k + mid) % n } else { k };
                assert!(a[k] == label(want));
            }
        }
    }
}

/// A word labelled `k`.
fn word(k: usize) -> u64 {
    k as u64
}

/// A 32-byte element labelled `k`, each word distinctly (the largest
/// element the block-swap algorithm takes).
fn quad(k: usize) -> [u64; 4] {
    let k = 4 * k as u64;
    [k, k + 1, k + 2, k + 3]
}

/// A 40-byte element labelled `k`, each word distinctly (past
/// `[usize; 4]`: the cycle algorithm whenever the shorter side does not
/// fit the buffer).
fn quint(k: usize) -> [u64; 5] {
    let k = 5 * k as u64;
    [k, k + 1, k + 2, k + 3, k + 4]
}

/// S382 `<[T]>::rotate_left`: under `mid <= len`, `final(s)@ ==
/// old(s)@.subrange(mid, len) + old(s)@.subrange(0, mid)`. `core` picks
/// one of three algorithms from `min(mid, len - mid)`, `len` and `T`'s
/// size (`core/src/slice/rotate.rs`): through a 256-byte stack buffer
/// when the shorter side fits in it, else by cycles (gcd) when `len < 24`
/// or `T` is over 32 bytes, else by block swaps. On 8-byte elements (the
/// proofs' slot words; the one caller is `heap::compact`'s refill) the
/// buffer takes a shorter side of up to 32: every `len <= 8` and `mid`
/// here (the buffer), and `len` 70 below.
#[kani::proof]
#[kani::unwind(10)]
fn slice_rotate_left() {
    rotate_on::<u64, 8>(word, false);
}

/// S383 on 8-byte elements at `len` 70, `mid` 31 to 39: the buffer for a
/// shorter side of 31, 32 (`mid` 31, 32, 38, 39), block swaps past it
/// (`mid` 33 to 37). All `mid`s of this length took CBMC over 90 minutes.
#[kani::proof]
#[kani::unwind(72)]
fn slice_rotate_left_long() {
    rotate_mids::<u64, 70>(word, true, 31, 39);
}

/// S383, the cycle algorithm, which 8-byte elements never reach: on
/// 40-byte elements (over `[usize; 4]`, a buffer of 6) at `len` 16.
#[kani::proof]
#[kani::unwind(42)]
fn slice_rotate_left_cycles() {
    rotate_on::<[u64; 5], 16>(quint, true);
}

/// S383 on 32-byte elements (a buffer of 8): cycles below `len` 24
/// (`len` 20), block swaps from there (`len` 26).
#[kani::proof]
#[kani::unwind(106)]
fn slice_rotate_left_quads() {
    rotate_on::<[u64; 4], 20>(quad, true);
    rotate_on::<[u64; 4], 26>(quad, true);
}

/// The longest slice `sort_unstable` is checked on, at every length up to
/// it: `core` sorts these by insertion alone (up to 20 elements).
const SORT_MAX: usize = 12;

/// `old` and `new` hold `v` equally often.
fn same_count<const L: usize>(old: &[u64; L], new: &[u64; L], v: u64) -> bool {
    let (mut a, mut b) = (0usize, 0usize);
    for k in 0..L {
        a += (old[k] == v) as usize;
        b += (new[k] == v) as usize;
    }
    a == b
}

/// `sort_unstable` on a slice of `L` arbitrary words below `L`: the
/// length is kept, an arbitrary entry, old or new, occurs as often after
/// as before (so the multiset is kept), and the words end sorted. Words
/// below `L` give every order pattern `L` words can have, ties included
/// (each is its own dense rank), which is all a comparison sort sees;
/// see the record. `L` is a constant: with a symbolic length CBMC would
/// also unwind every longer algorithm `core` picks by length.
fn sort_ranks_on<const L: usize>() {
    let mut s: [u64; L] = kani::any();
    for k in 0..L {
        kani::assume(s[k] < L as u64);
    }
    let old = s;
    let v: &mut [u64] = &mut s;
    v.sort_unstable();
    assert!(v.len() == L);
    let i: usize = kani::any();
    if i < L {
        assert!(same_count(&old, &s, old[i]));
        assert!(same_count(&old, &s, s[i]));
    }
    for k in 1..L {
        assert!(s[k - 1] <= s[k]);
    }
}

/// S378 `<[T]>::sort_unstable`: no `requires`; the length and the
/// multiset are kept (so every element is one of the old ones), for
/// every slice of up to `SORT_MAX` words.
#[kani::proof]
#[kani::unwind(14)]
fn slice_sort_unstable() {
    const _: () = assert!(SORT_MAX == 12);
    sort_ranks_on::<0>();
    sort_ranks_on::<1>();
    sort_ranks_on::<2>();
    sort_ranks_on::<3>();
    sort_ranks_on::<4>();
    sort_ranks_on::<5>();
    sort_ranks_on::<6>();
    sort_ranks_on::<7>();
    sort_ranks_on::<8>();
    sort_ranks_on::<9>();
    sort_ranks_on::<10>();
    sort_ranks_on::<11>();
    sort_ranks_on::<12>();
}
