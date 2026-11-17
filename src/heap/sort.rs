//! The flush's sorts of a cache's slot pointers: an introsort
//! ([`sort_ptrs_by_key`]) ordering entries by a `usize` key, with the
//! address-ordered [`sort_ptrs`] on top. It is the classic algorithm: a
//! quicksort with a median-of-three pivot and a Hoare partition, an
//! insertion sort for ranges of up to [`SMALL`] entries, and a heapsort
//! once the partitions have gone `2 log2(len)` levels deep, so every
//! call is O(len log len).
//!
//! Verus checks that no index leaves the slice, that every loop and
//! every recursion ends, and that the slice ends up a permutation of
//! what it held (the same multiset). The order itself is not part of
//! the contract: no proof needs it. The unit tests below check it
//! against `core`'s `sort_unstable`. Where entries with equal keys are
//! equal, as on the flush (an address, or under PAC the address bits of
//! entries all signed with one modifier), an ascending order of the
//! multiset is unique, so the result is the one `sort_unstable` gives.

use vstd::multiset::Multiset;
use vstd::prelude::*;

verus! {

/// The longest range sorted by insertion alone.
const SMALL: usize = 20;

/// `v.swap(i, j)`, which keeps the multiset.
#[inline(always)]
fn swap(v: &mut [*mut u8], i: usize, j: usize, Ghost(m): Ghost<Multiset<*mut u8>>)
    requires
        i < old(v)@.len(),
        j < old(v)@.len(),
        old(v)@.to_multiset() == m,
    ensures
        final(v)@.len() == old(v)@.len(),
        final(v)@.to_multiset() == m,
{
    proof {
        crate::class::lemma_swap_permutes(v@, i as int, j as int);
    }
    v.swap(i, j);
}

/// Every entry of a permutation of `v0` is one of `v0`'s (also
/// `flush::flag_sort`'s).
pub(super) proof fn lemma_in_perm(s: Seq<*mut u8>, v0: Seq<*mut u8>)
    requires
        s.to_multiset() == v0.to_multiset(),
    ensures
        forall|i: int| 0 <= i < s.len() ==> v0.contains(#[trigger] s[i]),
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures;
    assert forall|i: int| 0 <= i < s.len() implies v0.contains(#[trigger] s[i]) by {
        assert(s.contains(s[i]));
        assert(s.to_multiset().count(s[i]) > 0);
        assert(v0.to_multiset().count(s[i]) > 0);
    }
}

/// Insertion sort of `v[lo..hi]` by `key`: each entry is lifted out and
/// the larger ones before it shift up one place.
fn insertion<K: Fn(*mut u8) -> usize>(v: &mut [*mut u8], lo: usize, hi: usize, key: &K, Ghost(m): Ghost<Multiset<*mut u8>>)
    requires
        lo <= hi <= old(v)@.len(),
        old(v)@.to_multiset() == m,
        forall|p: *mut u8| #[trigger] key.requires((p,)),
    ensures
        final(v)@.len() == old(v)@.len(),
        final(v)@.to_multiset() == m,
{
    let ghost n = v@.len();
    if hi - lo < 2 {
        return;
    }
    let mut i = lo + 1;
    while i < hi
        invariant
            lo < i <= hi <= n,
            v@.len() == n,
            v@.to_multiset() == m,
            forall|p: *mut u8| #[trigger] key.requires((p,)),
        decreases hi - i,
    {
        let x = v[i];
        let kx = key(x);
        let mut j = i;
        proof {
            assert(v@.update(j as int, x) =~= v@);
        }
        while j > lo && key(v[j - 1]) > kx
            invariant
                lo <= j <= i < hi <= n,
                v@.len() == n,
                v@.update(j as int, x).to_multiset() == m,
                forall|p: *mut u8| #[trigger] key.requires((p,)),
            decreases j,
        {
            let ghost s = v@.update(j as int, x);
            let y = v[j - 1];
            v[j] = y;
            proof {
                crate::class::lemma_swap_permutes(s, j - 1, j as int);
                assert(v@.update(j - 1, x) =~= s.update(j - 1, s[j as int]).update(j as int, s[j - 1]));
            }
            j -= 1;
        }
        v[j] = x;
        i += 1;
    }
}

/// Sifts the entry at heap node `node` of the max-heap on
/// `v[lo..lo + end]` (node `k`'s children are `2k + 1` and `2k + 2`)
/// down to its place.
fn sift_down<K: Fn(*mut u8) -> usize>(
    v: &mut [*mut u8],
    lo: usize,
    node: usize,
    end: usize,
    key: &K,
    Ghost(m): Ghost<Multiset<*mut u8>>,
)
    requires
        lo + end <= old(v)@.len(),
        lo + end <= usize::MAX,
        old(v)@.to_multiset() == m,
        forall|p: *mut u8| #[trigger] key.requires((p,)),
    ensures
        final(v)@.len() == old(v)@.len(),
        final(v)@.to_multiset() == m,
{
    let ghost n = v@.len();
    let mut node = node;
    while node < end / 2
        invariant
            n == old(v)@.len(),
            lo + end <= n,
            lo + end <= usize::MAX,
            v@.len() == n,
            v@.to_multiset() == m,
            forall|p: *mut u8| #[trigger] key.requires((p,)),
        decreases end - node,
    {
        assert(2 * node + 1 < end) by (nonlinear_arith)
            requires
                node < end / 2,
        ;
        let mut child = 2 * node + 1;
        if child + 1 < end && key(v[lo + child]) < key(v[lo + child + 1]) {
            child += 1;
        }
        if key(v[lo + node]) >= key(v[lo + child]) {
            return;
        }
        swap(v, lo + node, lo + child, Ghost(m));
        node = child;
    }
}

/// Heapsort of `v[lo..hi]` by `key`.
fn heapsort<K: Fn(*mut u8) -> usize>(v: &mut [*mut u8], lo: usize, hi: usize, key: &K, Ghost(m): Ghost<Multiset<*mut u8>>)
    requires
        lo <= hi <= old(v)@.len(),
        old(v)@.to_multiset() == m,
        forall|p: *mut u8| #[trigger] key.requires((p,)),
    ensures
        final(v)@.len() == old(v)@.len(),
        final(v)@.to_multiset() == m,
{
    let ghost n = v@.len();
    let len = hi - lo;
    let mut i = len / 2;
    while i > 0
        invariant
            lo + len == hi,
            lo + len <= n,
            v@.len() == n,
            v@.to_multiset() == m,
            forall|p: *mut u8| #[trigger] key.requires((p,)),
        decreases i,
    {
        i -= 1;
        sift_down(v, lo, i, len, key, Ghost(m));
    }
    let mut end = len;
    while end > 1
        invariant
            end <= len,
            lo + len == hi,
            lo + len <= n,
            v@.len() == n,
            v@.to_multiset() == m,
            forall|p: *mut u8| #[trigger] key.requires((p,)),
        decreases end,
    {
        end -= 1;
        swap(v, lo, lo + end, Ghost(m));
        sift_down(v, lo, 0, end, key, Ghost(m));
    }
}

/// Partitions `v[lo..hi]`, at least three entries, around a pivot, the
/// median by `key` of its first, middle and last entries, and returns
/// the pivot's final index `p`: entries in `v[lo..p]` have keys at most
/// the pivot's and entries in `v[p + 1..hi]` at least.
fn partition<K: Fn(*mut u8) -> usize>(v: &mut [*mut u8], lo: usize, hi: usize, key: &K, Ghost(m): Ghost<Multiset<*mut u8>>) -> (p: usize)
    requires
        lo + 3 <= hi <= old(v)@.len(),
        old(v)@.to_multiset() == m,
        forall|p: *mut u8| #[trigger] key.requires((p,)),
    ensures
        final(v)@.len() == old(v)@.len(),
        final(v)@.to_multiset() == m,
        lo <= p < hi,
{
    let ghost n = v@.len();
    let mid = lo + (hi - lo) / 2;
    let last = hi - 1;
    // Order the three samples, then move the median to the front.
    if key(v[mid]) < key(v[lo]) {
        swap(v, mid, lo, Ghost(m));
    }
    if key(v[last]) < key(v[mid]) {
        swap(v, last, mid, Ghost(m));
        if key(v[mid]) < key(v[lo]) {
            swap(v, mid, lo, Ghost(m));
        }
    }
    swap(v, lo, mid, Ghost(m));
    let pk = key(v[lo]);
    let mut i = lo + 1;
    let mut j = last;
    loop
        invariant
            lo < i <= j + 1,
            lo <= j < hi <= n,
            v@.len() == n,
            v@.to_multiset() == m,
            forall|p: *mut u8| #[trigger] key.requires((p,)),
        decreases j + 1 - i,
    {
        let ghost (i0, j0) = (i, j);
        while i <= j && key(v[i]) < pk
            invariant
                lo < i <= j + 1,
                lo <= j < hi <= n,
                i0 <= i,
                j == j0,
                v@.len() == n,
                forall|p: *mut u8| #[trigger] key.requires((p,)),
            decreases j + 1 - i,
        {
            i += 1;
        }
        while i <= j && key(v[j]) > pk
            invariant
                lo < i <= j + 1,
                lo <= j < hi <= n,
                i0 <= i,
                j <= j0,
                v@.len() == n,
                forall|p: *mut u8| #[trigger] key.requires((p,)),
            decreases j,
        {
            j -= 1;
        }
        if i >= j {
            break;
        }
        swap(v, i, j, Ghost(m));
        i += 1;
        j -= 1;
    }
    swap(v, lo, j, Ghost(m));
    j
}

/// The partition depth after which a range is heapsorted: `2 log2(len)`.
fn depth_limit(len: usize) -> u32 {
    let mut n = len;
    let mut r = 0u32;
    // A `usize` halves to 1 within 64 steps, so the cap never binds.
    while n > 1 && r < 126
        invariant
            r <= 127,
        decreases n,
    {
        n /= 2;
        r += 2;
    }
    r
}

/// Introsort of `v[lo..hi]` by `key`, `limit` partitions deep at most
/// before a heapsort. The smaller side of each partition recurses and
/// the larger one loops, so the recursion is at most `log2(len)` deep.
fn quicksort<K: Fn(*mut u8) -> usize>(
    v: &mut [*mut u8],
    lo0: usize,
    hi0: usize,
    limit: u32,
    key: &K,
    Ghost(m): Ghost<Multiset<*mut u8>>,
)
    requires
        lo0 <= hi0 <= old(v)@.len(),
        old(v)@.to_multiset() == m,
        forall|p: *mut u8| #[trigger] key.requires((p,)),
    ensures
        final(v)@.len() == old(v)@.len(),
        final(v)@.to_multiset() == m,
    decreases hi0 - lo0,
{
    let ghost n = v@.len();
    let (mut lo, mut hi, mut limit) = (lo0, hi0, limit);
    loop
        invariant
            n == old(v)@.len(),
            lo <= hi <= n,
            hi - lo <= hi0 - lo0,
            v@.len() == n,
            v@.to_multiset() == m,
            forall|p: *mut u8| #[trigger] key.requires((p,)),
        decreases hi - lo,
    {
        if hi - lo <= SMALL {
            insertion(v, lo, hi, key, Ghost(m));
            return;
        }
        if limit == 0 {
            heapsort(v, lo, hi, key, Ghost(m));
            return;
        }
        limit -= 1;
        let p = partition(v, lo, hi, key, Ghost(m));
        if p - lo < hi - p - 1 {
            quicksort(v, lo, p, limit, key, Ghost(m));
            lo = p + 1;
        } else {
            quicksort(v, p + 1, hi, limit, key, Ghost(m));
            hi = p;
        }
    }
}

/// Sorts `s` by `key`, ascending, not stably: permutes `s`.
pub(crate) fn sort_ptrs_by_key<K: Fn(*mut u8) -> usize>(s: &mut [*mut u8], key: K)
    requires
        forall|p: *mut u8| #[trigger] key.requires((p,)),
    ensures
        final(s)@.len() == old(s)@.len(),
        final(s)@.to_multiset() == old(s)@.to_multiset(),
        forall|i: int| 0 <= i < old(s)@.len() ==> old(s)@.contains(#[trigger] final(s)@[i]),
{
    let ghost s0 = s@;
    let len = s.len();
    quicksort(s, 0, len, depth_limit(len), &key, Ghost(s0.to_multiset()));
    proof {
        lemma_in_perm(s@, s0);
    }
}

/// Sorts `s` by address, ascending: permutes `s`.
pub(crate) fn sort_ptrs(s: &mut [*mut u8])
    ensures
        final(s)@.len() == old(s)@.len(),
        final(s)@.to_multiset() == old(s)@.to_multiset(),
        forall|i: int| 0 <= i < old(s)@.len() ==> old(s)@.contains(#[trigger] final(s)@[i]),
{
    sort_ptrs_by_key(s, |p: *mut u8| p as usize)
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    /// A small deterministic generator (xorshift), so a failure repeats.
    fn rng(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    /// Slices of every shape the flush sees and the cases that reach
    /// each algorithm: random, sorted, reversed, few distinct keys and a
    /// sawtooth, at lengths around [`SMALL`] and up to past a cache.
    fn inputs() -> Vec<Vec<*mut u8>> {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut out = Vec::new();
        for len in [
            0, 1, 2, 3, 4, 5, 19, 20, 21, 22, 23, 40, 63, 64, 65, 100, 255, 256, 511, 1000, 1024,
            4097,
        ] {
            let addr = |x: u64| core::ptr::without_provenance_mut::<u8>(x as usize);
            out.push((0..len).map(|_| addr(rng(&mut seed) >> 16)).collect());
            out.push((0..len).map(|i| addr(i as u64 * 64)).collect());
            out.push((0..len).map(|i| addr((len - i) as u64 * 64)).collect());
            out.push((0..len).map(|_| addr(rng(&mut seed) % 4)).collect());
            out.push((0..len).map(|i| addr((i % 7) as u64)).collect());
            out.push((0..len).map(|_| addr(7)).collect());
        }
        out
    }

    #[test]
    fn sort_matches_core() {
        for v in inputs() {
            let mut want = v.clone();
            want.sort_unstable();
            let mut got = v.clone();
            sort_ptrs(&mut got);
            assert_eq!(got, want, "len {}", v.len());
            let mut got = v.clone();
            sort_ptrs_by_key(&mut got, |p: *mut u8| usize::MAX - p as usize);
            want.reverse();
            assert_eq!(got, want, "by key, len {}", v.len());
        }
    }

    #[test]
    fn heapsort_and_insertion_sort() {
        for v in inputs() {
            let mut want = v.clone();
            want.sort_unstable();
            let mut got = v.clone();
            heapsort(
                &mut got,
                0,
                v.len(),
                &|p: *mut u8| p as usize,
                Ghost::assume_new(),
            );
            assert_eq!(got, want, "heapsort, len {}", v.len());
            if v.len() <= 256 {
                let mut got = v.clone();
                insertion(
                    &mut got,
                    0,
                    v.len(),
                    &|p: *mut u8| p as usize,
                    Ghost::assume_new(),
                );
                assert_eq!(got, want, "insertion, len {}", v.len());
            }
            if v.len() >= 3 {
                let mut got = v.clone();
                quicksort(
                    &mut got,
                    0,
                    v.len(),
                    0,
                    &|p: *mut u8| p as usize,
                    Ghost::assume_new(),
                );
                assert_eq!(got, want, "limit 0, len {}", v.len());
            }
        }
    }
}
