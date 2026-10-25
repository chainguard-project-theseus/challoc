//! Bitmaps (design.md sections 4, 5 and 7).
//!
//! Two structures live here and both are pure logic.
//!
//! [`AtomicBitmap`] is one slab's view of its class's slot bitmap, a
//! contiguous `[AtomicU64]` array in the metadata region (`class/`). It
//! has no owner and is only ever touched with atomic read-modify-write
//! operations. A set bit means "not available to a refill": the slot is
//! live in the program or sitting in a thread cache. Padding bits past the
//! last real slot are kept permanently set so that they can never be
//! claimed and so that a full slab is simply one whose words are all ones.
//!
//! [`TwoLevelBitmap`] is the ordered set used for slab numbers (partial and
//! empty sets per class) and for free pages in the huge region. It is not
//! atomic: every user holds the class or huge lock while touching it. Level 0
//! holds the bits, level 1 holds one summary bit per level-0 word, and
//! `lowest_set` is a scan of the used prefix of level 1 followed by two
//! `tzcnt`s. The prefix is tracked by the highest level-1 word that can be
//! non-zero, so a heap with a few thousand slabs scans one word.

use core::sync::atomic::{AtomicU64, Ordering};
use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
use crate::vspec::core_int::popcount;
#[cfg(verus_keep_ghost)]
use crate::vspec::ptr::ptr_add;
use vstd::raw_ptr::{PointsTo, ptr_mut_ref, ptr_ref};
use vstd::resource::Loc;
#[cfg(verus_keep_ghost)]
use vstd::std_specs::bits::{
    axiom_u64_leading_zeros, axiom_u64_trailing_zeros, u64_leading_zeros, u64_trailing_zeros,
};

verus! {

/// Set bits of `x` among bits `0..k`.
pub open spec fn count_below(x: u64, k: nat) -> nat
    decreases k,
{
    if k == 0 {
        0
    } else {
        count_below(x, (k - 1) as nat) + if has_bit(x, (k - 1) as u64) { 1nat } else { 0nat }
    }
}

/// Set bits of `x`, counted bit by bit.
pub open spec fn ones(x: u64) -> nat {
    count_below(x, 64)
}

/// `popcount` (`vspec::core_int`, the spec of `count_ones`) counted bit by
/// bit.
proof fn lemma_popcount(x: u64)
    ensures
        popcount(x) == ones(x),
    decreases x,
{
    if x == 0 {
        assert forall|b: u64| b < 64 implies !#[trigger] has_bit(0u64, b) by {
            assert((0u64 >> b) & 1u64 != 1u64) by (bit_vector);
        }
        lemma_count_zero(0u64, 64);
    } else {
        lemma_popcount(x / 2);
        assert(x / 2 == x >> 1u64 && (x % 2 == 1 <==> has_bit(x, 0u64))) by (bit_vector);
        lemma_count_shr_prefix(x, 1, 63);
        assert(!has_bit(x >> 1u64, 63u64)) by (bit_vector);
        assert(count_below(x, 0) == 0);
        assert(count_below(x, 1) == if has_bit(x, 0u64) { 1nat } else { 0nat });
        assert(count_below(x >> 1u64, 64) == count_below(x >> 1u64, 63));
    }
}

/// Whether bit `b` of `x` is set.
pub open spec fn has_bit(x: u64, b: u64) -> bool {
    (x >> b) & 1u64 == 1u64
}

/// Every bit of `x` is a bit of `y`.
pub open spec fn subset(x: u64, y: u64) -> bool {
    x & !y == 0u64
}

pub proof fn lemma_bit_ops(x: u64, y: u64, b: u64)
    requires
        b < 64,
    ensures
        has_bit(x | y, b) == (has_bit(x, b) || has_bit(y, b)),
        has_bit(x & y, b) == (has_bit(x, b) && has_bit(y, b)),
        has_bit(!x, b) == !has_bit(x, b),
        has_bit(x & !y, b) == (has_bit(x, b) && !has_bit(y, b)),
{
    assert(((x | y) >> b) & 1u64 == 1u64 <==> ((x >> b) & 1u64 == 1u64 || (y >> b) & 1u64 == 1u64)) by (bit_vector)
        requires b < 64;
    assert(((x & y) >> b) & 1u64 == 1u64 <==> ((x >> b) & 1u64 == 1u64 && (y >> b) & 1u64 == 1u64)) by (bit_vector)
        requires b < 64;
    assert(((!x) >> b) & 1u64 == 1u64 <==> !((x >> b) & 1u64 == 1u64)) by (bit_vector)
        requires b < 64;
    assert(((x & !y) >> b) & 1u64 == 1u64 <==> ((x >> b) & 1u64 == 1u64 && !((y >> b) & 1u64 == 1u64)))
        by (bit_vector)
        requires b < 64;
}

/// `subset` bit by bit.
pub proof fn lemma_subset_bits(x: u64, y: u64)
    ensures
        subset(x, y) <==> forall|b: u64| b < 64 ==> #[trigger] has_bit(x, b) ==> has_bit(y, b),
{
    if subset(x, y) {
        assert forall|b: u64| b < 64 && #[trigger] has_bit(x, b) implies has_bit(y, b) by {
            lemma_bit_ops(x, y, b);
            assert((0u64 >> b) & 1u64 != 1u64) by (bit_vector);
        }
    }
    if forall|b: u64| b < 64 ==> #[trigger] has_bit(x, b) ==> has_bit(y, b) {
        assert forall|b: u64| b < 64 implies !#[trigger] has_bit(x & !y, b) by {
            lemma_bit_ops(x, y, b);
        }
        lemma_zero_of_no_bits(x & !y);
    }
}

/// A word with no set bit is zero.
proof fn lemma_zero_of_no_bits(x: u64)
    requires
        forall|b: u64| b < 64 ==> !#[trigger] has_bit(x, b),
    ensures
        x == 0u64,
{
    assert forall|b: u64| b < 64 implies #[trigger] (x >> b) & 1u64 == 0u64 by {
        assert(!has_bit(x, b));
        assert((x >> b) & 1u64 != 1u64 ==> (x >> b) & 1u64 == 0u64) by (bit_vector);
    }
    lemma_zero_of_low_bits_zero(x, 64);
}

/// Bits `0..k` of `x` clear and none above means `x` is zero.
proof fn lemma_zero_of_low_bits_zero(x: u64, k: u64)
    requires
        k <= 64,
        forall|b: u64| b < k ==> #[trigger] (x >> b) & 1u64 == 0u64,
        k < 64 ==> x >> k == 0u64,
    ensures
        x == 0u64,
    decreases k,
{
    if k > 0 {
        let j = (k - 1) as u64;
        assert((x >> j) & 1u64 == 0u64);
        assert((x >> j) & 1u64 == 0u64 && (j + 1 < 64 ==> x >> ((j + 1) as u64) == 0u64) ==> x >> j == 0u64)
            by (bit_vector)
            requires j < 64;
        if j == 63 {
            assert((x >> j) & 1u64 == 0u64 ==> j == 63 ==> x >> j == 0u64) by (bit_vector);
        }
        lemma_zero_of_low_bits_zero(x, j);
    } else {
        assert(x >> 0u64 == x) by (bit_vector);
    }
}

/// Clearing bit `h` of `x` removes it from the count of every prefix above.
proof fn lemma_count_clear(x: u64, h: u64, k: nat)
    requires
        k <= 64,
        h < 64,
        has_bit(x, h),
    ensures
        count_below(x & !(1u64 << h), k) + (if k > h { 1nat } else { 0nat })
            == count_below(x, k),
    decreases k,
{
    if k > 0 {
        let j = (k - 1) as u64;
        assert(has_bit(x & !(1u64 << h), j) == (has_bit(x, j) && j != h)) by (bit_vector)
            requires j < 64, h < 64;
        lemma_count_clear(x, h, (k - 1) as nat);
    }
}

/// No set bit among `0..k` means a zero count, and a zero word has none.
proof fn lemma_count_zero(x: u64, k: nat)
    requires
        k <= 64,
    ensures
        (forall|b: u64| b < k ==> !#[trigger] has_bit(x, b)) ==> count_below(x, k) == 0,
        count_below(x, k) == 0 ==> forall|b: u64| b < k ==> !#[trigger] has_bit(x, b),
    decreases k,
{
    if k > 0 {
        let j = (k - 1) as u64;
        lemma_count_zero(x, (k - 1) as nat);
        if forall|b: u64| b < k ==> !#[trigger] has_bit(x, b) {
            assert(!has_bit(x, j));
        }
    }
}

proof fn lemma_count_bound(x: u64, k: nat)
    ensures
        count_below(x, k) <= k,
    decreases k,
{
    if k > 0 {
        lemma_count_bound(x, (k - 1) as nat);
    }
}

/// Keep only the lowest `n` set bits of `m`.
#[inline]
pub const fn lowest_n_bits(m: u64, n: usize) -> (r: u64)
    ensures
        subset(r, m),
        popcount(r) == if popcount(m) <= n { popcount(m) } else { n as nat },
        // No set bit of `m` below a bit of `r` is left out.
        forall|b1: u64, b2: u64| b1 < b2 < 64 && #[trigger] has_bit(r, b2) && #[trigger] has_bit(m, b1) ==> has_bit(r, b1),
{
    let ghost m0 = m;
    let mut m = m;
    proof {
        assert(m & !m == 0u64) by (bit_vector);
        lemma_popcount(m);
    }
    if n >= 64 {
        proof {
            lemma_count_bound(m, 64);
        }
        return m;
    }
    let mut have = m.count_ones() as usize;
    while have > n
        invariant
            have as nat == ones(m),
            subset(m, m0),
            m == m0 || (have >= n && ones(m0) > n),
            forall|b1: u64, b2: u64| b1 < b2 < 64 && #[trigger] has_bit(m, b2) && #[trigger] has_bit(m0, b1) ==> has_bit(m, b1),
        decreases have,
    {
        proof {
            lemma_count_zero(m, 64);
            if m == 0 {
                assert forall|b: u64| b < 64 implies !#[trigger] has_bit(m, b) by {
                    assert((0u64 >> b) & 1u64 != 1u64) by (bit_vector);
                }
            }
            axiom_u64_leading_zeros(m);
        }
        let ghost prev = m;
        let ghost h = (63 - u64_leading_zeros(m)) as u64;
        // `have > n >= 0` so `m` is non-zero and has a highest set bit.
        m &= !(1u64 << (63 - m.leading_zeros()));
        have -= 1;
        proof {
            assert((prev >> h) & 1u64 != 0u64);
            assert((prev >> h) & 1u64 != 0u64 ==> (prev >> h) & 1u64 == 1u64) by (bit_vector);
            assert(has_bit(prev, h));
            lemma_count_clear(prev, h, 64);
            lemma_subset_bits(prev, m0);
            assert forall|b: u64| b < 64 implies #[trigger] has_bit(m, b) == (has_bit(prev, b) && b != h) by {
                assert(has_bit(prev & !(1u64 << h), b) == (has_bit(prev, b) && b != h)) by (bit_vector)
                    requires b < 64, h < 64;
            }
            lemma_subset_bits(m, m0);
            assert forall|b1: u64, b2: u64| b1 < b2 < 64 && #[trigger] has_bit(m, b2) && #[trigger] has_bit(m0, b1)
                implies has_bit(m, b1) by {
                // Bits of `prev` above `h` are clear, so `b2 < h`.
                assert(b2 <= h);
            }
        }
    }
    proof {
        lemma_popcount(m);
        lemma_popcount(m0);
    }
    m
}

/// Bits `s .. s + k` of a word.
pub open spec fn range_mask(k: u64, s: u64) -> u64 {
    (((1u64 << k) - 1) as u64) << s
}

/// `(1 << n) - 1` cannot underflow, and is `range_mask` at 0.
proof fn lemma_one_shl(n: usize)
    requires
        n < 64,
    ensures
        1u64 << n >= 1,
        forall|s: u32| s < 64 ==> #[trigger] ((((1u64 << n) - 1) as u64) << s) == range_mask(n as u64, s as u64),
{
    assert(1u64 << n >= 1) by (bit_vector)
        requires n < 64;
    assert forall|s: u32| s < 64 implies #[trigger] ((((1u64 << n) - 1) as u64) << s) == range_mask(n as u64, s as u64) by {
        assert(((((1u64 << n) - 1) as u64) << s) == ((((1u64 << (n as u64)) - 1) as u64) << (s as u64)))
            by (bit_vector)
            requires n < 64, s < 64;
    }
}

/// `range_mask(k, s)` has `k` bits and sits in `s .. s + k`.
proof fn lemma_range_mask(k: u64, s: u64)
    requires
        0 < k < 64,
        s + k <= 64,
    ensures
        ones(range_mask(k, s)) == k,
        range_mask(k, s) != !0u64,
        (range_mask(k, s) >> s) << s == range_mask(k, s),
        range_mask(k, s) >> s == ((1u64 << k) - 1) as u64,
        forall|b: u64| b < 64 ==> #[trigger] has_bit(range_mask(k, s), b) == (s <= b < s + k),
{
    let m = range_mask(k, s);
    assert forall|b: u64| b < 64 implies #[trigger] has_bit(m, b) == (s <= b < s + k) by {
        assert(((((((1u64 << k) - 1) as u64) << s) >> b) & 1u64 == 1u64) == (s <= b && b < s + k))
            by (bit_vector)
            requires 0 < k < 64, s + k <= 64, b < 64;
    }
    lemma_count_range(m, k, s, 64);
    assert(((((1u64 << k) - 1) as u64) << s) != !0u64) by (bit_vector)
        requires 0 < k < 64, s + k <= 64;
    assert((((((1u64 << k) - 1) as u64) << s) >> s) << s == ((((1u64 << k) - 1) as u64) << s))
        by (bit_vector)
        requires 0 < k < 64, s + k <= 64;
    assert((((((1u64 << k) - 1) as u64) << s) >> s) == (((1u64 << k) - 1) as u64))
        by (bit_vector)
        requires 0 < k < 64, s + k <= 64;
}

proof fn lemma_count_range(m: u64, k: u64, s: u64, j: nat)
    requires
        j <= 64,
        s + k <= 64,
        forall|b: u64| b < 64 ==> #[trigger] has_bit(m, b) == (s <= b < s + k),
    ensures
        count_below(m, j) == if j <= s { 0 } else if j >= s + k { k as int } else { j - s },
    decreases j,
{
    if j > 0 {
        assert(has_bit(m, (j - 1) as u64) == (s <= j - 1 < s + k));
        lemma_count_range(m, k, s, (j - 1) as nat);
    }
}

proof fn lemma_count_shr_prefix(x: u64, s: u64, j: nat)
    requires
        s < 64,
        j + s <= 64,
    ensures
        count_below(x >> s, j) + count_below(x, s as nat) == count_below(x, (j + s) as nat),
    decreases j,
{
    if j > 0 {
        let b = (j - 1) as u64;
        assert(((x >> s) >> b) & 1u64 == (x >> ((b + s) as u64)) & 1u64) by (bit_vector)
            requires s < 64, b + s < 64;
        lemma_count_shr_prefix(x, s, (j - 1) as nat);
    }
}

} // verus!

/// One slab's slots in the class bitmap (design section 4).
///
/// The class bitmap is one contiguous array of `AtomicU64` per class, slab
/// `s` owning bits `s * slab_bits .. (s + 1) * slab_bits`
/// (`Class::slab_bits`, `nslots` padded so that this range is either a
/// whole number of words or one range inside a single word). The view holds
/// the slab's words, the position of slot 0 in the first word (`shift`,
/// non-zero only for a sub-word slab) and the bits of each word that
/// belong to the slab (`mask`, all ones for whole-word slabs). Slot numbers
/// and the masks returned to callers are always in slab coordinates: bit
/// `b` of word `w` is slot `w * 64 + b`.
///
/// A set bit means "not available to a refill": live in the program or in
/// some thread's cache. The padding bits between `nslots` and `slab_bits`
/// are kept permanently set so they can never be claimed and a full slab is
/// one whose masked words are all ones. Bits outside `mask` belong to the
/// neighbouring slabs of the same arena's group and are never touched.
pub struct AtomicBitmap<'a> {
    words: &'a [AtomicU64],
    shift: u32,
    mask: u64,
}

impl<'a> AtomicBitmap<'a> {
    /// View `words` as the bitmap of a slab that owns them whole. Nothing is
    /// initialised; call [`init`] before the first claim on a fresh slab.
    ///
    /// [`init`]: AtomicBitmap::init
    #[inline]
    pub fn from_words(words: &'a [AtomicU64]) -> AtomicBitmap<'a> {
        AtomicBitmap {
            words,
            shift: 0,
            mask: !0,
        }
    }

    /// View bits `shift .. shift + nbits` of `word` as the bitmap of a slab
    /// whose bits share that word with other slabs. `nbits` is the slab's
    /// padded bit count (`Class::slab_bits`), below 64.
    #[inline]
    pub fn sub_word(word: &'a AtomicU64, shift: u32, nbits: usize) -> AtomicBitmap<'a> {
        debug_assert!(nbits > 0 && nbits < 64 && shift as usize + nbits <= 64);
        AtomicBitmap {
            words: core::slice::from_ref(word),
            shift,
            mask: ((1u64 << nbits) - 1) << shift,
        }
    }

    /// Number of words in the view (1 for a sub-word slab).
    #[inline]
    pub fn words(&self) -> usize {
        self.words.len()
    }

    /// Bits the slab owns, padding included.
    #[inline]
    pub fn nbits(&self) -> usize {
        if self.mask == !0 {
            self.words.len() * 64
        } else {
            self.mask.count_ones() as usize
        }
    }

    /// Clear every bit for slots `0..nbits` and set the padding bits above
    /// `nbits` (up to the view's `slab_bits`) so they can never be claimed.
    /// `nbits` must be non-zero and fit the view. Atomic on a shared word,
    /// so a neighbouring slab that is already active is not disturbed.
    // The loop is the spelled-out `for`, not a `while let`.
    #[allow(clippy::while_let_loop)]
    pub fn init(&self, nbits: usize) {
        assert!(
            nbits > 0 && nbits <= self.nbits(),
            "bitmap: nbits does not match the view"
        );
        if self.mask == !0 {
            // `for (i, w) in self.words.iter().enumerate()`, spelled out
            // (Verus has no `Enumerate`); the count is taken and bumped as
            // `Enumerate::next` does.
            let mut iter = self.words.iter();
            let mut count: usize = 0;
            loop {
                let w = match iter.next() {
                    Some(w) => w,
                    None => break,
                };
                let i = count;
                count += 1;
                let lo = i * 64;
                let pad = if nbits >= lo + 64 {
                    0
                } else if nbits <= lo {
                    !0
                } else {
                    !0u64 << (nbits - lo)
                };
                w.store(pad, Ordering::Relaxed);
            }
        } else {
            let w = &self.words[0];
            let pad = self.mask & !(((1u64 << nbits) - 1) << self.shift);
            w.fetch_and(!self.mask, Ordering::Relaxed);
            if pad != 0 {
                w.fetch_or(pad, Ordering::Relaxed);
            }
        }
    }

    /// Claim up to `n` clear bits from word `i`. Returns the mask of slots
    /// this call actually claimed (slab coordinates), which may be zero if
    /// the word was full or a concurrent claimer won every bit we wanted.
    #[inline]
    pub fn claim_word(&self, i: usize, n: usize) -> u64 {
        let w = &self.words[i];
        let cur = w.load(Ordering::Relaxed);
        let want = lowest_n_bits(!cur & self.mask, n);
        if want == 0 {
            return 0;
        }
        // Acquire pairs with the Release in `release`: the new owner's
        // accesses to the slot happen after the previous owner's.
        let prev = w.fetch_or(want, Ordering::Acquire);
        (want & !prev) >> self.shift
    }

    /// Tests: [`claim_from`] word 0.
    ///
    /// [`claim_from`]: AtomicBitmap::claim_from
    #[cfg(test)]
    pub fn claim(&self, n: usize) -> Option<(usize, u64)> {
        self.claim_from(0, n)
    }

    /// Claim up to `n` bits from the lowest word at or after `start`
    /// (wrapping around; `slot_randomize` starts at a random word, the
    /// address-ordered claim at 0) that has a clear bit. Returns the word
    /// index and the claimed mask, or `None` if every word was full at the
    /// time it was inspected. A single call never spans words; callers loop
    /// until they have enough or get `None`.
    pub fn claim_from(&self, start: usize, n: usize) -> Option<(usize, u64)> {
        let len = self.words.len();
        assert!(start < len, "bitmap: start word out of range");
        let mut i = start;
        for _ in 0..len {
            let got = self.claim_word(i, n);
            if got != 0 {
                return Some((i, got));
            }
            i += 1;
            if i == len {
                i = 0;
            }
        }
        None
    }

    /// Release the slots in `mask` (slab coordinates) of word `i`. Returns
    /// the slab's previous bits in that word and whether they were all set
    /// (the slab's part of the word was full). The caller must abort if
    /// `prev & mask != mask`: a bit that was already clear means a slot was
    /// released twice.
    #[inline]
    pub fn release(&self, i: usize, mask: u64) -> (u64, bool) {
        let m = mask << self.shift;
        assert!(m & !self.mask == 0, "bitmap: release outside the slab");
        let w = &self.words[i];
        let prev = w.fetch_and(!m, Ordering::Release);
        (
            (prev & self.mask) >> self.shift,
            prev & self.mask == self.mask,
        )
    }

    /// Whether slot `bit` is set. This is the `free` check: a clear bit
    /// means an invalid or double free. Never writes.
    #[inline]
    #[cfg(test)]
    pub fn is_set(&self, bit: usize) -> bool {
        let b = bit + self.shift as usize;
        (self.words[b / 64].load(Ordering::Relaxed) >> (b % 64)) & 1 == 1
    }

    /// The slab's bits in word `i` (slab coordinates, padding included).
    #[inline]
    pub fn load(&self, i: usize) -> u64 {
        let v = self.words[i].load(Ordering::Relaxed);
        (v & self.mask) >> self.shift
    }

    /// Set slot bits, padding excluded: the slab's allocated (or cached)
    /// slots for a class with `nslots` real slots.
    pub fn allocated(&self, nslots: usize) -> usize {
        // The `map` and `sum` of the word counts, as a loop: Verus does
        // not specify those adapters.
        let mut set: usize = 0;
        let mut w = 0;
        while w < self.words.len() {
            let x = self.load(w);
            set += x.count_ones() as usize;
            w += 1;
        }
        let padding = self.nbits() - nslots;
        // Fewer set bits than padding bits means a padding bit was cleared:
        // a release outside the slab's real slots.
        assert!(
            set >= padding,
            "bitmap: a padding bit of the slab was cleared"
        );
        set - padding
    }

    /// Whether every bit, padding included, is set. A full slab leaves the
    /// partial set (design section 5).
    pub fn is_full(&self) -> bool {
        self.words
            .iter()
            .all(|w| w.load(Ordering::Relaxed) & self.mask == self.mask)
    }
}

verus! {

/// Row of level-1 word `j` in a chunked view.
pub open spec fn l1_row(j: int) -> int {
    65 * j
}

/// Words from its first that a chunked view of `l1` level-1 words and
/// row stride `stride` spans: up to and including its word in the last
/// row, row `65 * l1 - 1`, at offset `(65 * l1 - 1) * stride`. The other
/// words of each row belong to other views.
pub open spec fn chunked_span(l1: int, stride: int) -> int {
    if l1 == 0 { 0 } else { (65 * l1 - 1) * stride + 1 }
}

/// Row of level-0 word `w` in a chunked view.
pub open spec fn l0_row(w: int) -> int {
    65 * (w / 64) + 1 + w % 64
}

proof fn lemma_mul_cancel(a: int, b: int, s: int)
    requires
        a != b,
        s >= 1,
    ensures
        a * s != b * s,
{
    assert(a * s != b * s) by (nonlinear_arith)
        requires a != b, s >= 1;
}

/// The mask `set_range` and `clear_range` build: bits `lo..hi`.
proof fn lemma_word_mask(mask: u64, lo: u64, hi: u64)
    requires
        lo < hi <= 64,
        mask == if hi - lo == 64 { !0u64 } else { ((((1u64 << (hi - lo) as u64) - 1) as u64) << lo) },
    ensures
        forall|b: u64| b < 64 ==> #[trigger] has_bit(mask, b) == (lo <= b < hi),
{
    if hi - lo == 64 {
        assert forall|b: u64| b < 64 implies #[trigger] has_bit(mask, b) == (lo <= b < hi) by {
            assert(((!0u64) >> b) & 1u64 == 1u64) by (bit_vector) requires b < 64;
        }
    } else {
        lemma_range_mask((hi - lo) as u64, lo);
    }
}

/// Facts about `trailing_zeros` in terms of `has_bit`.
pub proof fn lemma_tz(m: u64)
    ensures
        u64_trailing_zeros(m) <= 64,
        m == 0u64 <==> u64_trailing_zeros(m) == 64,
        m != 0u64 ==> has_bit(m, u64_trailing_zeros(m) as u64),
        forall|k: u64| k < u64_trailing_zeros(m) ==> !#[trigger] has_bit(m, k),
{
    axiom_u64_trailing_zeros(m);
    assert forall|k: u64| k < u64_trailing_zeros(m) implies !#[trigger] has_bit(m, k) by {
        assert((m >> k) & 1u64 == 0u64);
    }
}

/// `!0 << s` holds bits `s..64`.
proof fn lemma_shl_ones(s: u64)
    requires
        s < 64,
    ensures
        forall|k: u64| k < 64 ==> #[trigger] has_bit(!0u64 << s, k) == (k >= s),
{
    assert forall|k: u64| k < 64 implies #[trigger] has_bit(!0u64 << s, k) == (k >= s) by {
        assert((((!0u64 << s) >> k) & 1u64 == 1u64) == (k >= s)) by (bit_vector)
            requires s < 64, k < 64;
    }
}

/// `m & (m - 1)` is `m` without its lowest bit.
pub proof fn lemma_clear_lowest(m: u64)
    requires
        m != 0u64,
    ensures
        forall|k: u64| k < 64 ==> #[trigger] has_bit(m & ((m - 1) as u64), k) == (has_bit(m, k) && k != u64_trailing_zeros(m)),
{
    lemma_tz(m);
    let t = u64_trailing_zeros(m) as u64;
    // The bits below `t` are clear: `m & ((1 << t) - 1) == 0`.
    assert forall|k: u64| k < 64 implies !#[trigger] has_bit(m & (((1u64 << t) - 1) as u64), k) by {
        lemma_bit_ops(m, ((1u64 << t) - 1) as u64, k);
        assert((((((1u64 << t) - 1) as u64) >> k) & 1u64 == 1u64) == (k < t)) by (bit_vector)
            requires t < 64, k < 64;
    }
    lemma_zero_of_no_bits(m & (((1u64 << t) - 1) as u64));
    assert forall|k: u64| k < 64 implies #[trigger] has_bit(m & ((m - 1) as u64), k) == (has_bit(m, k) && k != t) by {
        assert((((m & ((m - 1) as u64)) >> k) & 1u64 == 1u64) == (((m >> k) & 1u64 == 1u64) && k != t)) by (bit_vector)
            requires t < 64, k < 64, (m >> t) & 1u64 == 1u64, m & (((1u64 << t) - 1) as u64) == 0u64;
    }
}

/// `x / 4096`, as two divisions by 64, grows with `x`.
proof fn lemma_div_div_mono(a: int, b: int)
    requires
        0 <= a <= b,
    ensures
        a / 64 / 64 <= b / 64 / 64,
{
    assert(a / 64 <= b / 64) by (nonlinear_arith)
        requires 0 <= a <= b;
    assert(a / 64 / 64 <= b / 64 / 64) by (nonlinear_arith)
        requires 0 <= a / 64 <= b / 64;
}

/// `1 << b` has only bit `b`; `x` with a set bit is non-zero.
proof fn lemma_one_bit(b: u64)
    requires
        b < 64,
    ensures
        forall|k: u64| k < 64 ==> #[trigger] has_bit(1u64 << b, k) == (k == b),
        forall|x: u64, k: u64| k < 64 && #[trigger] has_bit(x, k) ==> x != 0u64,
{
    assert forall|k: u64| k < 64 implies #[trigger] has_bit(1u64 << b, k) == (k == b) by {
        assert((((1u64 << b) >> k) & 1u64 == 1u64) == (k == b)) by (bit_vector)
            requires b < 64, k < 64;
    }
    assert forall|x: u64, k: u64| k < 64 && #[trigger] has_bit(x, k) implies x != 0u64 by {
        assert((0u64 >> k) & 1u64 != 1u64) by (bit_vector);
    }
}

/// Ordered set of small integers with a one-word-per-64 summary level
/// (design section 5). Not thread safe; callers hold a lock.
///
/// Storage is caller-provided: [`words_needed`] words of zeroed `u64`, level
/// 1 first then level 0, so that it can be carved out of the metadata region.
/// A [`from_raw_chunked`] view lays its words out in rows of `stride` words
/// shared with other views (word `k` of the view is at `ptr.add(k * stride)`)
/// and in chunks of 65 rows, one level-1 word followed by the 64 level-0
/// words it summarises, so that a table of many sets can be committed as a
/// prefix that grows with the slab numbers in use and the words a small heap
/// touches (level-1 word 0 and level-0 word 0 of every set) share one or two
/// pages instead of one page per set.
///
/// [`words_needed`]: TwoLevelBitmap::words_needed
/// [`from_raw_chunked`]: TwoLevelBitmap::from_raw_chunked
pub struct TwoLevelBitmap {
    nbits: usize,
    l0_words: usize,
    l1_words: usize,
    /// Number of leading level-1 words that may be non-zero. Everything at
    /// or above this index is zero. Grows on `set`, shrinks on `clear`.
    l1_used: usize,
    /// Dense (`from_raw`): `l1_words` summary words followed by `l0_words`
    /// bit words at `ptr`. Chunked (`from_raw_chunked`): row `r` of the
    /// view is `ptr.add(r * stride)`, level-1 word `j` is row `65 * j` and
    /// level-0 word `w` is row `65 * (w / 64) + 1 + w % 64`.
    ptr: *mut u64,
    stride: usize,
    chunked: bool,
    /// Ghost permission for each word of the view, by offset from `ptr`
    /// (read only by ghost code, which ordinary builds erase).
    #[allow(dead_code)]
    perms: Tracked<Map<int, PointsTo<u64>>>,
}

impl TwoLevelBitmap {
    /// Offset in words of level-1 word `j`.
    spec fn l1o(&self, j: int) -> int {
        if self.chunked { l1_row(j) * self.stride } else { j }
    }

    /// Offset in words of level-0 word `w`.
    spec fn l0o(&self, w: int) -> int {
        if self.chunked { l0_row(w) * self.stride } else { self.l1_words + w }
    }

    /// The view holds an initialised word at offset `k`, which lies inside
    /// the allocation `ptr` points into (so `ptr.add(k)` is in bounds; the
    /// constructors take both from their caller's safety contract).
    spec fn ok(&self, k: int) -> bool {
        &&& self.perms@.contains_key(k)
        &&& crate::vspec::ptr::in_alloc(self.ptr, k * vstd::layout::size_of::<u64>())
        &&& self.perms@[k].ptr() == ptr_add(self.ptr, k as usize)
        &&& self.perms@[k].is_init()
    }

    spec fn l1v(&self, j: int) -> u64 {
        self.perms@[self.l1o(j)].value()
    }

    spec fn l0v(&self, w: int) -> u64 {
        self.perms@[self.l0o(w)].value()
    }

    /// Everything but the words is the same.
    spec fn frame(&self, o: &Self) -> bool {
        &&& self.nbits == o.nbits
        &&& self.l0_words == o.l0_words
        &&& self.l1_words == o.l1_words
        &&& self.ptr == o.ptr
        &&& self.stride == o.stride
        &&& self.chunked == o.chunked
    }

    spec fn shape(&self) -> bool {
        &&& self.l0_words == (self.nbits + 63) / 64
        &&& self.l1_words == (self.l0_words + 63) / 64
        &&& self.l1_used <= self.l1_words
        &&& self.stride >= 1
        &&& self.chunked ==> 65 * self.l1_words * self.stride * 8 <= isize::MAX
        &&& !self.chunked ==> self.stride == 1 && (self.l1_words + self.l0_words) * 8 <= isize::MAX
    }

    /// The fields agree and every word of the view is held.
    spec fn valid(&self) -> bool {
        &&& self.shape()
        &&& forall|j: int| 0 <= j < self.l1_words ==> #[trigger] self.ok(self.l1o(j))
        &&& forall|w: int| 0 <= w < self.l0_words ==> #[trigger] self.ok(self.l0o(w))
    }

    /// Bit `k` of level-1 word `j` is set exactly when level-0 word
    /// `64 * j + k` is non-zero.
    spec fn summary(&self) -> bool {
        forall|j: int, k: u64| 0 <= j < self.l1_words && k < 64 ==>
            #[trigger] has_bit(self.l1v(j), k) == (j * 64 + k < self.l0_words && self.l0v(j * 64 + k) != 0u64)
    }

    /// No bit at or above `nbits` is set.
    spec fn padding(&self) -> bool {
        forall|w: int, b: u64| 0 <= w < self.l0_words && b < 64 && w * 64 + b >= self.nbits ==>
            !#[trigger] has_bit(self.l0v(w), b)
    }

    /// Level-1 words at or above `l1_used` are zero.
    spec fn tail(&self) -> bool {
        forall|j: int| self.l1_used <= j < self.l1_words ==> #[trigger] self.l1v(j) == 0u64
    }

    /// The level-1 word below `l1_used` is non-zero.
    spec fn tight(&self) -> bool {
        self.l1_used > 0 ==> self.l1v(self.l1_used - 1) != 0u64
    }

    spec fn inner(&self) -> bool {
        self.valid() && self.summary() && self.padding()
    }

    /// Whether `b` is in the set.
    pub closed spec fn has(&self, b: int) -> bool {
        0 <= b < self.nbits && has_bit(self.l0v(b / 64), (b % 64) as u64)
    }

    /// The set.
    pub closed spec fn view(&self) -> Set<nat> {
        Set::<nat>::range(0, self.nbits as nat).filter(|b: nat| self.has(b as int))
    }

    /// The universe is `0..spec_nbits()`.
    pub closed spec fn spec_nbits(&self) -> nat {
        self.nbits as nat
    }

    /// The invariant every operation keeps.
    pub closed spec fn wf(&self) -> bool {
        self.inner() && self.tail() && self.tight()
    }

    /// Words from `ptr` that the view spans: every word offset it holds
    /// is below this.
    spec fn span(&self) -> int {
        if self.chunked { chunked_span(self.l1_words as int, self.stride as int) } else { self.l1_words + self.l0_words }
    }

    /// Where the view lies: the address of its first word and the bytes
    /// it spans. Every operation keeps it; the constructors set it.
    pub closed spec fn place(&self) -> (int, int) {
        (self.ptr@.addr as int, 8 * self.span())
    }

    /// The address of level-1 word `j` (its permission's).
    pub closed spec fn l1_addr(&self, j: int) -> int {
        self.perms@[self.l1o(j)].ptr()@.addr as int
    }

    /// The address of level-0 word `w` (its permission's).
    pub closed spec fn l0_addr(&self, w: int) -> int {
        self.perms@[self.l0o(w)].ptr()@.addr as int
    }

    /// Level-1 words.
    pub closed spec fn spec_l1_words(&self) -> int {
        self.l1_words as int
    }

    /// Level-0 words.
    pub closed spec fn spec_l0_words(&self) -> int {
        self.l0_words as int
    }

    /// Every word the view reads and writes (through its permission) is
    /// eight bytes inside its [`place`](TwoLevelBitmap::place).
    pub proof fn lemma_words_placed(&self, j: int, w: int)
        requires
            self.wf(),
            self.place().0 + self.place().1 <= usize::MAX,
        ensures
            0 <= j < self.spec_l1_words() ==> self.place().0 <= self.l1_addr(j) && self.l1_addr(j) + 8 <= self.place().0 + self.place().1,
            0 <= w < self.spec_l0_words() ==> self.place().0 <= self.l0_addr(w) && self.l0_addr(w) + 8 <= self.place().0 + self.place().1,
    {
        self.lemma_offsets();
        let s = self.stride as int;
        let n1 = self.l1_words as int;
        if 0 <= j < self.l1_words {
            let k = self.l1o(j);
            assert(self.ok(k));
            if self.chunked {
                assert(l1_row(j) * s + 1 <= (65 * n1 - 1) * s + 1) by (nonlinear_arith)
                    requires 0 <= j < n1, s >= 1;
            }
            assert(k + 1 <= self.span() && (k as usize) as int == k);
            crate::vspec::ptr::axiom_ptr_add_addr(self.ptr, k as usize);
        }
        if 0 <= w < self.l0_words {
            let k = self.l0o(w);
            assert(self.ok(k));
            if self.chunked {
                assert(w / 64 < n1);
                assert(l0_row(w) * s + 1 <= (65 * n1 - 1) * s + 1) by (nonlinear_arith)
                    requires 0 <= l0_row(w) < 65 * n1, s >= 1;
            }
            assert(k + 1 <= self.span() && (k as usize) as int == k);
            crate::vspec::ptr::axiom_ptr_add_addr(self.ptr, k as usize);
        }
    }

    /// A view as built by `from_raw*`: every word held and zero.
    pub closed spec fn fresh(&self, nbits: usize) -> bool {
        &&& self.valid()
        &&& self.nbits == nbits
        &&& self.l1_used == 0
        &&& forall|j: int| 0 <= j < self.l1_words ==> #[trigger] self.l1v(j) == 0u64
        &&& forall|w: int| 0 <= w < self.l0_words ==> #[trigger] self.l0v(w) == 0u64
    }

    /// A fresh view is a well-formed empty set.
    pub proof fn lemma_fresh(&self, nbits: usize)
        requires
            self.fresh(nbits),
        ensures
            self.wf(),
            self.view() == Set::<nat>::empty(),
            self.spec_nbits() == nbits,
    {
        assert forall|b: u64| b < 64 implies !#[trigger] has_bit(0u64, b) by {
            assert((0u64 >> b) & 1u64 != 1u64) by (bit_vector);
        }
        assert forall|j: int, k: u64| 0 <= j < self.l1_words && k < 64 implies
            #[trigger] has_bit(self.l1v(j), k) == (j * 64 + k < self.l0_words && self.l0v(j * 64 + k) != 0u64) by {
            assert(self.l1v(j) == 0u64);
            if j * 64 + k < self.l0_words {
                assert(self.l0v(j * 64 + k) == 0u64);
            }
        }
        assert forall|w: int, b: u64| 0 <= w < self.l0_words && b < 64 && w * 64 + b >= self.nbits implies
            !#[trigger] has_bit(self.l0v(w), b) by {
            assert(self.l0v(w) == 0u64);
        }
        assert forall|b: int| !#[trigger] self.has(b) by {
            if 0 <= b < self.nbits {
                assert(b / 64 < self.l0_words);
                assert(self.l0v(b / 64) == 0u64);
            }
        }
        assert(self.view() =~= Set::<nat>::empty());
    }

    /// Offsets are distinct and small enough for `ptr.add`.
    proof fn lemma_offsets(&self)
        requires
            self.shape(),
        ensures
            forall|j: int| 0 <= j < self.l1_words ==> 0 <= #[trigger] self.l1o(j) && self.l1o(j) * 8 <= isize::MAX,
            forall|w: int| 0 <= w < self.l0_words ==> 0 <= #[trigger] self.l0o(w) && self.l0o(w) * 8 <= isize::MAX,
            forall|j1: int, j2: int| 0 <= j1 < self.l1_words && 0 <= j2 < self.l1_words && j1 != j2 ==>
                #[trigger] self.l1o(j1) != #[trigger] self.l1o(j2),
            forall|w1: int, w2: int| 0 <= w1 < self.l0_words && 0 <= w2 < self.l0_words && w1 != w2 ==>
                #[trigger] self.l0o(w1) != #[trigger] self.l0o(w2),
            forall|j: int, w: int| 0 <= j < self.l1_words && 0 <= w < self.l0_words ==>
                #[trigger] self.l1o(j) != #[trigger] self.l0o(w),
    {
        let s = self.stride as int;
        let n1 = self.l1_words as int;
        if self.chunked {
            assert forall|j: int| 0 <= j < self.l1_words implies 0 <= #[trigger] self.l1o(j) && self.l1o(j) * 8 <= isize::MAX by {
                assert(0 <= l1_row(j) * s <= 65 * n1 * s) by (nonlinear_arith)
                    requires 0 <= j < n1, s >= 1;
            }
            assert forall|w: int| 0 <= w < self.l0_words implies 0 <= #[trigger] self.l0o(w) && self.l0o(w) * 8 <= isize::MAX by {
                assert(w / 64 < n1);
                assert(0 <= l0_row(w) < 65 * n1);
                assert(0 <= l0_row(w) * s <= 65 * n1 * s) by (nonlinear_arith)
                    requires 0 <= l0_row(w) < 65 * n1, s >= 1;
            }
            assert forall|j1: int, j2: int| 0 <= j1 < self.l1_words && 0 <= j2 < self.l1_words && j1 != j2 implies
                #[trigger] self.l1o(j1) != #[trigger] self.l1o(j2) by {
                lemma_mul_cancel(l1_row(j1), l1_row(j2), s);
            }
            assert forall|w1: int, w2: int| 0 <= w1 < self.l0_words && 0 <= w2 < self.l0_words && w1 != w2 implies
                #[trigger] self.l0o(w1) != #[trigger] self.l0o(w2) by {
                lemma_mul_cancel(l0_row(w1), l0_row(w2), s);
            }
            assert forall|j: int, w: int| 0 <= j < self.l1_words && 0 <= w < self.l0_words implies
                #[trigger] self.l1o(j) != #[trigger] self.l0o(w) by {
                lemma_mul_cancel(l1_row(j), l0_row(w), s);
            }
        }
    }

    /// A word written through `l0_mut` or `l1_mut`: the view is still
    /// valid, and only the word at offset `k` changed, to `v`.
    proof fn lemma_write(o: &Self, n: &Self, k: int, v: u64)
        requires
            o.valid(),
            n.frame(o),
            n.l1_used == o.l1_used,
            o.ok(k),
            n.perms@ == o.perms@.insert(k, n.perms@[k]),
            n.perms@[k].ptr() == o.perms@[k].ptr(),
            n.perms@[k].is_init(),
            n.perms@[k].value() == v,
        ensures
            n.valid(),
            forall|j: int| 0 <= j < o.l1_words ==> #[trigger] n.l1v(j) == if o.l1o(j) == k { v } else { o.l1v(j) },
            forall|w: int| 0 <= w < o.l0_words ==> #[trigger] n.l0v(w) == if o.l0o(w) == k { v } else { o.l0v(w) },
    {
        assert forall|j: int| 0 <= j < n.l1_words implies #[trigger] n.ok(n.l1o(j)) by {
            assert(o.ok(o.l1o(j)));
        }
        assert forall|w: int| 0 <= w < n.l0_words implies #[trigger] n.ok(n.l0o(w)) by {
            assert(o.ok(o.l0o(w)));
        }
    }

    /// Level-0 word `w` became `a2` and its summary bit follows it; every
    /// other word is unchanged. The summary and padding invariants hold,
    /// and the set changed only in word `w`.
    proof fn lemma_word(o: &Self, n: &Self, w: int, a2: u64)
        requires
            o.inner(),
            n.valid(),
            n.frame(o),
            0 <= w < o.l0_words,
            forall|b: u64| b < 64 && w * 64 + b >= o.nbits ==> !#[trigger] has_bit(a2, b),
            n.l0v(w) == a2,
            forall|w2: int| 0 <= w2 < o.l0_words && w2 != w ==> #[trigger] n.l0v(w2) == o.l0v(w2),
            forall|j: int| 0 <= j < o.l1_words && j != w / 64 ==> #[trigger] n.l1v(j) == o.l1v(j),
            forall|k: u64| k < 64 && k != (w % 64) as u64 ==> #[trigger] has_bit(n.l1v(w / 64), k) == has_bit(o.l1v(w / 64), k),
            has_bit(n.l1v(w / 64), (w % 64) as u64) == (a2 != 0u64),
        ensures
            n.inner(),
            forall|b: int| #[trigger] n.has(b) == if b / 64 == w { 0 <= b < n.nbits && has_bit(a2, (b % 64) as u64) } else { o.has(b) },
    {
        assert forall|j: int, k: u64| 0 <= j < n.l1_words && k < 64 implies
            #[trigger] has_bit(n.l1v(j), k) == (j * 64 + k < n.l0_words && n.l0v(j * 64 + k) != 0u64) by {
            assert(has_bit(o.l1v(j), k) == (j * 64 + k < o.l0_words && o.l0v(j * 64 + k) != 0u64));
            if j * 64 + k == w {
                assert(j == w / 64 && k == (w % 64) as u64);
            } else if j == w / 64 {
                assert(k != (w % 64) as u64);
            }
        }
        assert forall|w2: int, b: u64| 0 <= w2 < n.l0_words && b < 64 && w2 * 64 + b >= n.nbits implies
            !#[trigger] has_bit(n.l0v(w2), b) by {
            if w2 != w {
                assert(!has_bit(o.l0v(w2), b));
            }
        }
        assert forall|b: int| #[trigger] n.has(b) == if b / 64 == w { 0 <= b < n.nbits && has_bit(a2, (b % 64) as u64) } else { o.has(b) } by {
            if 0 <= b < n.nbits && b / 64 != w {
                assert(b / 64 < o.l0_words);
                assert(n.l0v(b / 64) == o.l0v(b / 64));
            }
        }
    }

    /// `clear`'s level-0 write, before the summary follows it: the new word
    /// is the old one less `b`'s bit, and the set lost `b` once the summary
    /// is right. With a non-zero word the summary already is.
    proof fn lemma_clear_word(o: &Self, n: &Self, b: int)
        requires
            o.wf(),
            0 <= b < o.nbits,
            n.frame(o),
            n.l1_used == o.l1_used,
            o.ok(o.l0o(b / 64)),
            n.perms@ == o.perms@.insert(o.l0o(b / 64), n.perms@[o.l0o(b / 64)]),
            n.perms@[o.l0o(b / 64)].ptr() == o.perms@[o.l0o(b / 64)].ptr(),
            n.perms@[o.l0o(b / 64)].is_init(),
            n.perms@[o.l0o(b / 64)].value() == o.l0v(b / 64) & !(1u64 << ((b % 64) as u64)),
        ensures
            n.valid(),
            forall|w: int| 0 <= w < o.l0_words && w != b / 64 ==> #[trigger] n.l0v(w) == o.l0v(w),
            forall|j: int| 0 <= j < o.l1_words ==> #[trigger] n.l1v(j) == o.l1v(j),
            forall|k: u64| k < 64 ==> #[trigger] has_bit(n.l0v(b / 64), k) == (has_bit(o.l0v(b / 64), k) && k != (b % 64) as u64),
            forall|x: int| 0 <= x < o.nbits && x / 64 == b / 64 ==> #[trigger] has_bit(n.l0v(b / 64), (x % 64) as u64) == (o.has(x) && x != b),
            forall|k: u64| k < 64 && b / 64 * 64 + k >= o.nbits ==> !#[trigger] has_bit(n.l0v(b / 64), k),
            n.l0v(b / 64) != 0u64 ==> n.inner() && forall|x: int| #[trigger] n.has(x) == (o.has(x) && x != b),
    {
        let w = b / 64;
        o.lemma_offsets();
        Self::lemma_write(o, n, o.l0o(w), n.perms@[o.l0o(w)].value());
        lemma_one_bit((b % 64) as u64);
        assert forall|k: u64| k < 64 implies #[trigger] has_bit(n.l0v(w), k) == (has_bit(o.l0v(w), k) && k != (b % 64) as u64) by {
            lemma_bit_ops(o.l0v(w), 1u64 << ((b % 64) as u64), k);
        }
        assert forall|k: u64| k < 64 && w * 64 + k >= o.nbits implies !#[trigger] has_bit(n.l0v(w), k) by {
            assert(!has_bit(o.l0v(w), k));
        }
        assert forall|x: int| 0 <= x < o.nbits && x / 64 == w implies #[trigger] has_bit(n.l0v(w), (x % 64) as u64) == (o.has(x) && x != b) by {
            assert(has_bit(n.l0v(w), (x % 64) as u64) == (has_bit(o.l0v(w), (x % 64) as u64) && (x % 64) as u64 != (b % 64) as u64));
        }
        if n.l0v(w) != 0u64 {
            let a2 = n.l0v(w);
            // A non-zero word was non-zero before: its summary bit is set.
            if o.l0v(w) == 0u64 {
                assert forall|k: u64| k < 64 implies !#[trigger] has_bit(a2, k) by {
                    assert((0u64 >> k) & 1u64 != 1u64) by (bit_vector);
                }
                lemma_zero_of_no_bits(a2);
            }
            assert(w / 64 * 64 + w % 64 == w);
            assert(has_bit(o.l1v(w / 64), (w % 64) as u64));
            Self::lemma_word(o, n, w, a2);
            assert forall|x: int| #[trigger] n.has(x) == (o.has(x) && x != b) by {
                if 0 <= x < o.nbits && x / 64 == w {
                    assert(has_bit(n.l0v(w), (x % 64) as u64) == (o.has(x) && x != b));
                }
            }
        }
    }

    /// A level-0 word lost the bits of `m`, before the summary follows it.
    /// With a non-zero word the summary already is right.
    proof fn lemma_and_not_word(o: &Self, n: &Self, w: int, m: u64)
        requires
            o.inner(),
            0 <= w < o.l0_words,
            n.frame(o),
            n.l1_used == o.l1_used,
            o.ok(o.l0o(w)),
            n.perms@ == o.perms@.insert(o.l0o(w), n.perms@[o.l0o(w)]),
            n.perms@[o.l0o(w)].ptr() == o.perms@[o.l0o(w)].ptr(),
            n.perms@[o.l0o(w)].is_init(),
            n.perms@[o.l0o(w)].value() == o.l0v(w) & !m,
        ensures
            n.valid(),
            forall|w2: int| 0 <= w2 < o.l0_words && w2 != w ==> #[trigger] n.l0v(w2) == o.l0v(w2),
            forall|j: int| 0 <= j < o.l1_words ==> #[trigger] n.l1v(j) == o.l1v(j),
            forall|x: int| 0 <= x < o.nbits && x / 64 == w ==> #[trigger] has_bit(n.l0v(w), (x % 64) as u64) == (o.has(x) && !has_bit(m, (x % 64) as u64)),
            forall|k: u64| k < 64 && w * 64 + k >= o.nbits ==> !#[trigger] has_bit(n.l0v(w), k),
            n.l0v(w) != 0u64 ==> n.inner() && forall|x: int| #[trigger] n.has(x) == (o.has(x) && !(x / 64 == w && has_bit(m, (x % 64) as u64))),
    {
        o.lemma_offsets();
        Self::lemma_write(o, n, o.l0o(w), n.perms@[o.l0o(w)].value());
        assert forall|k: u64| k < 64 implies #[trigger] has_bit(n.l0v(w), k) == (has_bit(o.l0v(w), k) && !has_bit(m, k)) by {
            lemma_bit_ops(o.l0v(w), m, k);
        }
        assert forall|k: u64| k < 64 && w * 64 + k >= o.nbits implies !#[trigger] has_bit(n.l0v(w), k) by {
            assert(!has_bit(o.l0v(w), k));
        }
        assert forall|x: int| 0 <= x < o.nbits && x / 64 == w implies #[trigger] has_bit(n.l0v(w), (x % 64) as u64) == (o.has(x) && !has_bit(m, (x % 64) as u64)) by {
            assert(has_bit(n.l0v(w), (x % 64) as u64) == (has_bit(o.l0v(w), (x % 64) as u64) && !has_bit(m, (x % 64) as u64)));
        }
        if n.l0v(w) != 0u64 {
            let a2 = n.l0v(w);
            if o.l0v(w) == 0u64 {
                assert forall|k: u64| k < 64 implies !#[trigger] has_bit(a2, k) by {
                    assert((0u64 >> k) & 1u64 != 1u64) by (bit_vector);
                }
                lemma_zero_of_no_bits(a2);
            }
            assert(w / 64 * 64 + w % 64 == w);
            assert(has_bit(o.l1v(w / 64), (w % 64) as u64));
            Self::lemma_word(o, n, w, a2);
            assert forall|x: int| #[trigger] n.has(x) == (o.has(x) && !(x / 64 == w && has_bit(m, (x % 64) as u64))) by {
                if 0 <= x < o.nbits && x / 64 == w {
                    assert(has_bit(n.l0v(w), (x % 64) as u64) == (o.has(x) && !has_bit(m, (x % 64) as u64)));
                }
            }
        }
    }

    /// A member's word is non-zero, its summary bit is set, and its
    /// level-1 word is below `l1_used`.
    proof fn lemma_member(&self, x: int)
        requires
            self.wf(),
            self.has(x),
        ensures
            0 <= x < self.nbits,
            x / 64 < self.l0_words,
            x / 64 / 64 < self.l1_used,
            self.l0v(x / 64) != 0u64,
            has_bit(self.l1v(x / 64 / 64), (x / 64 % 64) as u64),
    {
        lemma_one_bit((x % 64) as u64);
        let w = x / 64;
        assert(w / 64 * 64 + w % 64 == w);
        assert(has_bit(self.l1v(w / 64), (w % 64) as u64));
        lemma_one_bit((w % 64) as u64);
        if w / 64 >= self.l1_used {
            assert(self.l1v(w / 64) == 0u64);
        }
    }

    /// Summary bit `t` of level-1 word `i` names a non-zero word, and that
    /// word's lowest bit is a member.
    proof fn lemma_found(&self, i: int, t: u64)
        requires
            self.wf(),
            0 <= i < self.l1_words,
            t < 64,
            has_bit(self.l1v(i), t),
        ensures
            i * 64 + t < self.l0_words,
            self.l0v(i * 64 + t) != 0u64,
            u64_trailing_zeros(self.l0v(i * 64 + t)) < 64,
            self.has((i * 64 + t) * 64 + u64_trailing_zeros(self.l0v(i * 64 + t))),
            forall|k: u64| k < u64_trailing_zeros(self.l0v(i * 64 + t)) ==> !#[trigger] has_bit(self.l0v(i * 64 + t), k),
    {
        let w = i * 64 + t;
        lemma_tz(self.l0v(w));
        let t0 = u64_trailing_zeros(self.l0v(w)) as int;
        let b = w * 64 + t0;
        assert(b / 64 == w && b % 64 == t0);
        if b >= self.nbits {
            assert(!has_bit(self.l0v(w), t0 as u64));
        }
    }

    /// Only `l1_used` changed.
    proof fn lemma_used(o: &Self, n: &Self)
        requires
            o.inner(),
            n.frame(o),
            n.perms == o.perms,
            n.l1_used <= n.l1_words,
        ensures
            n.inner(),
            forall|j: int| #[trigger] n.l1v(j) == o.l1v(j),
            forall|w: int| #[trigger] n.l0v(w) == o.l0v(w),
            forall|b: int| #[trigger] n.has(b) == o.has(b),
    {
        assert forall|j: int| 0 <= j < n.l1_words implies #[trigger] n.ok(n.l1o(j)) by {
            assert(o.ok(o.l1o(j)));
        }
        assert forall|w: int| 0 <= w < n.l0_words implies #[trigger] n.ok(n.l0o(w)) by {
            assert(o.ok(o.l0o(w)));
        }
        assert forall|j: int, k: u64| 0 <= j < n.l1_words && k < 64 implies
            #[trigger] has_bit(n.l1v(j), k) == (j * 64 + k < n.l0_words && n.l0v(j * 64 + k) != 0u64) by {
            assert(has_bit(o.l1v(j), k) == (j * 64 + k < o.l0_words && o.l0v(j * 64 + k) != 0u64));
        }
        assert forall|w: int, b: u64| 0 <= w < n.l0_words && b < 64 && w * 64 + b >= n.nbits implies
            !#[trigger] has_bit(n.l0v(w), b) by {
            assert(!has_bit(o.l0v(w), b));
        }
    }

    /// Membership changed by adding `b`: the set is the old set plus `b`.
    proof fn lemma_view_insert(o: &Self, n: &Self, b: int)
        requires
            n.nbits == o.nbits,
            0 <= b < o.nbits,
            forall|x: int| #[trigger] n.has(x) == (o.has(x) || x == b),
        ensures
            n.view() == o.view().insert(b as nat),
    {
        broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
        assert(n.view() =~= o.view().insert(b as nat));
    }

    /// Membership changed by removing `b`: the set is the old set less `b`.
    proof fn lemma_view_remove(o: &Self, n: &Self, b: int)
        requires
            n.nbits == o.nbits,
            0 <= b,
            forall|x: int| #[trigger] n.has(x) == (o.has(x) && x != b),
        ensures
            n.view() == o.view().remove(b as nat),
    {
        broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
        assert forall|x: nat| #[trigger] n.view().contains(x) == o.view().remove(b as nat).contains(x) by {
            assert(n.has(x as int) == (o.has(x as int) && x as int != b));
        }
        assert(n.view() =~= o.view().remove(b as nat));
    }

    /// Words of storage needed for a set over `0..nbits`.
    pub const fn words_needed(nbits: usize) -> (r: usize)
        ensures
            r == (nbits + 63) / 64 + ((nbits + 63) / 64 + 63) / 64,
    {
        let l0 = nbits.div_ceil(64);
        l0 + l0.div_ceil(64)
    }

    /// Build a set over `0..nbits` on top of `words_needed(nbits)` words at
    /// `ptr`, all zero.
    ///
    /// # Safety
    ///
    /// `ptr` must be valid for reads and writes of `words_needed(nbits)`
    /// `u64`s for as long as the set is used, must be zeroed, and must not be
    /// accessed by anything else while the set exists.
    ///
    /// Trusted: the view's word permissions (`perms`), each word zero, stand
    /// for that safety contract, which Verus does not check.
    #[verifier::external_body]
    pub const unsafe fn from_raw(nbits: usize, ptr: *mut u64) -> (r: TwoLevelBitmap)
        requires
            ((((nbits as int) + 63) / 64 + 63) / 64 + ((nbits as int) + 63) / 64) * 8 <= isize::MAX,
            // `u64`'s alignment (8 on both targets).
            ptr@.addr as int % 8 == 0,
            ptr@.addr != 0,
            ptr@.addr + 8 * ((((nbits as int) + 63) / 64 + 63) / 64 + ((nbits as int) + 63) / 64) <= usize::MAX,
            crate::vspec::ptr::in_alloc(ptr, 8 * ((((nbits as int) + 63) / 64 + 63) / 64 + ((nbits as int) + 63) / 64)),
        ensures
            r.fresh(nbits),
            r.place() == (ptr@.addr as int, 8 * ((((nbits as int) + 63) / 64 + 63) / 64 + ((nbits as int) + 63) / 64)),
    {
        let l0_words = nbits.div_ceil(64);
        TwoLevelBitmap {
            nbits,
            l0_words,
            l1_words: l0_words.div_ceil(64),
            l1_used: 0,
            ptr,
            stride: 1,
            chunked: false,
            perms: Tracked::assume_new(),
        }
    }

    /// Build a chunked view over `0..nbits` (struct doc): row `r` of the
    /// view is `ptr.add(r * stride)`, rows `0..chunked_rows(nbits)`.
    ///
    /// # Safety
    ///
    /// `stride >= 1`; the row-0 words and every row a used bit needs (its
    /// chunk's level-1 row and its own level-0 row, committed before the
    /// bit is set, cleared or tested) must be valid for reads and writes
    /// for as long as the set is used, zeroed, and not accessed by anything
    /// else while the set exists. Words between the view's own are
    /// somebody else's.
    ///
    /// Trusted, as [`from_raw`](TwoLevelBitmap::from_raw): the view's word
    /// permissions, each word zero, stand for the safety contract. The rows
    /// lie inside the allocation `ptr` points into (`ptr.add` reaches
    /// them), which the caller states.
    #[verifier::external_body]
    pub const unsafe fn from_raw_chunked(
        nbits: usize,
        ptr: *mut u64,
        stride: usize,
    ) -> (r: TwoLevelBitmap)
        requires
            stride >= 1,
            65 * ((((nbits as int) + 63) / 64 + 63) / 64) * (stride as int) * 8 <= isize::MAX,
            // `u64`'s alignment, as `from_raw` requires.
            ptr@.addr as int % 8 == 0,
            ptr@.addr != 0,
            ptr@.addr + 8 * chunked_span((((nbits as int) + 63) / 64 + 63) / 64, stride as int) <= usize::MAX,
            crate::vspec::ptr::in_alloc(ptr, 8 * chunked_span((((nbits as int) + 63) / 64 + 63) / 64, stride as int)),
        ensures
            r.fresh(nbits),
            r.place() == (ptr@.addr as int, 8 * chunked_span((((nbits as int) + 63) / 64 + 63) / 64, stride as int)),
    {
        let l0_words = nbits.div_ceil(64);
        debug_assert!(stride >= 1);
        TwoLevelBitmap {
            nbits,
            l0_words,
            l1_words: l0_words.div_ceil(64),
            l1_used: 0,
            ptr,
            stride,
            chunked: true,
            perms: Tracked::assume_new(),
        }
    }

    /// Level-1 words a set over `0..nbits` has.
    pub const fn l1_words(nbits: usize) -> (r: usize)
        ensures
            r == ((nbits + 63) / 64 + 63) / 64,
    {
        nbits.div_ceil(64).div_ceil(64)
    }

    /// Rows a chunked view over `0..nbits` spans (65 per level-1 word).
    pub const fn chunked_rows(nbits: usize) -> (r: usize)
        ensures
            r == 65 * (((nbits + 63) / 64 + 63) / 64),
    {
        65 * Self::l1_words(nbits)
    }

    /// Row of level-0 word `w` in a chunked view.
    pub const fn chunked_l0_row(w: usize) -> (r: usize)
        requires
            65 * (w / 64) + 64 <= usize::MAX,
        ensures
            r == l0_row(w as int),
    {
        65 * (w / 64) + 1 + w % 64
    }

    /// Offset in words of level-1 word `j`.
    #[inline]
    fn l1_off(&self, j: usize) -> (r: usize)
        requires
            self.shape(),
            j < self.l1_words,
        ensures
            r == self.l1o(j as int),
    {
        proof {
            self.lemma_offsets();
            assert(self.l1o(j as int) >= 0);
            assert(65 * j <= 65 * self.l1_words);
            assert(65 * j * self.stride <= 65 * self.l1_words * self.stride) by (nonlinear_arith)
                requires j < self.l1_words, self.stride >= 1;
        }
        if self.chunked { 65 * j * self.stride } else { j }
    }

    /// Offset in words of level-0 word `w`.
    #[inline]
    fn l0_off(&self, w: usize) -> (r: usize)
        requires
            self.shape(),
            w < self.l0_words,
        ensures
            r == self.l0o(w as int),
    {
        proof {
            self.lemma_offsets();
            assert(self.l0o(w as int) >= 0);
            if self.chunked {
                assert(w / 64 < self.l1_words);
                assert(0 <= l0_row(w as int) * self.stride <= 65 * self.l1_words * self.stride) by (nonlinear_arith)
                    requires 0 <= l0_row(w as int) < 65 * self.l1_words, self.stride >= 1;
            }
        }
        if self.chunked { Self::chunked_l0_row(w) * self.stride } else { self.l1_words + w }
    }

    /// Tests: number of bits in the set's universe.
    #[cfg(test)]
    pub const fn len(&self) -> usize {
        self.nbits
    }

    /// Tests: whether no bit is set.
    #[cfg(test)]
    pub const fn is_empty(&self) -> bool {
        self.l1_used == 0
    }

    /// Level-1 word `i`, read through the word's permission in `perms`
    /// (`vstd::raw_ptr::ptr_ref`). Trusted: Verus does not yet check that
    /// the word is in `perms` or that `ptr.add(..)` is that permission's
    /// pointer; the contract is what that permission gives.
    #[inline]
    #[verifier::external_body]
    fn l1(&self, i: usize) -> (r: u64)
        requires
            self.valid(),
            i < self.l1_words,
        ensures
            r == self.l1v(i as int),
    {
        debug_assert!(i < self.l1_words);
        // SAFETY: `i < l1_words`, a word of this view (`from_raw*`).
        unsafe { *ptr_ref(self.ptr.add(self.l1_off(i)), Tracked(self.perms.borrow().tracked_borrow(self.l1o(i as int)))) }
    }

    /// Level-0 word `i`. Trusted, as [`l1`](TwoLevelBitmap::l1).
    #[inline]
    #[verifier::external_body]
    fn l0(&self, i: usize) -> (r: u64)
        requires
            self.valid(),
            i < self.l0_words,
        ensures
            r == self.l0v(i as int),
    {
        debug_assert!(i < self.l0_words);
        // SAFETY: level-0 word `i` of this view (`from_raw*`).
        unsafe { *ptr_ref(self.ptr.add(self.l0_off(i)), Tracked(self.perms.borrow().tracked_borrow(self.l0o(i as int)))) }
    }

    /// Level-1 word `i`, to write, through the word's permission in `perms`
    /// (`vstd::raw_ptr::ptr_mut_ref`). Trusted: Verus does not yet check
    /// that the word is in `perms`, that `ptr.add(..)` is that permission's
    /// pointer, or the frame; the contract is that a write through the
    /// reference changes that one word and nothing else in the view.
    #[inline]
    #[verifier::external_body]
    fn l1_mut(&mut self, i: usize) -> (r: &mut u64)
        requires
            old(self).valid(),
            i < old(self).l1_words,
        ensures
            *r == old(self).l1v(i as int),
            final(self).frame(old(self)),
            final(self).l1_used == old(self).l1_used,
            final(self).perms@ == old(self).perms@.insert(old(self).l1o(i as int), final(self).perms@[old(self).l1o(i as int)]),
            final(self).perms@[old(self).l1o(i as int)].ptr() == old(self).perms@[old(self).l1o(i as int)].ptr(),
            final(self).perms@[old(self).l1o(i as int)].is_init(),
            final(self).perms@[old(self).l1o(i as int)].value() == *final(r),
            old(self).ok(old(self).l1o(i as int)),
    {
        debug_assert!(i < self.l1_words);
        let ghost key = self.l1o(i as int);
        // SAFETY: as in `l1`; `&mut self` gives exclusive access.
        unsafe { ptr_mut_ref(self.ptr.add(self.l1_off(i)), Tracked(self.perms.borrow_mut().tracked_borrow_mut(key))) }
    }

    /// Level-0 word `i`, to write. Trusted, as
    /// [`l1_mut`](TwoLevelBitmap::l1_mut).
    #[inline]
    #[verifier::external_body]
    fn l0_mut(&mut self, i: usize) -> (r: &mut u64)
        requires
            old(self).valid(),
            i < old(self).l0_words,
        ensures
            *r == old(self).l0v(i as int),
            final(self).frame(old(self)),
            final(self).l1_used == old(self).l1_used,
            final(self).perms@ == old(self).perms@.insert(old(self).l0o(i as int), final(self).perms@[old(self).l0o(i as int)]),
            final(self).perms@[old(self).l0o(i as int)].ptr() == old(self).perms@[old(self).l0o(i as int)].ptr(),
            final(self).perms@[old(self).l0o(i as int)].is_init(),
            final(self).perms@[old(self).l0o(i as int)].value() == *final(r),
            old(self).ok(old(self).l0o(i as int)),
    {
        debug_assert!(i < self.l0_words);
        let ghost key = self.l0o(i as int);
        // SAFETY: as in `l0`; `&mut self` gives exclusive access.
        unsafe { ptr_mut_ref(self.ptr.add(self.l0_off(i)), Tracked(self.perms.borrow_mut().tracked_borrow_mut(key))) }
    }

    /// Whether `bit` is in the set.
    #[inline]
    pub fn is_set(&self, bit: usize) -> (r: bool)
        requires
            self.wf(),
            bit < self.spec_nbits(),
        ensures
            r == self.view().contains(bit as nat),
    {
        assert!(bit < self.nbits, "bitmap: bit out of range");
        proof {
            broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
        }
        (self.l0(bit / 64) >> (bit % 64)) & 1 == 1
    }

    /// Insert `bit`.
    pub fn set(&mut self, bit: usize)
        requires
            old(self).wf(),
            bit < old(self).spec_nbits(),
        ensures
            final(self).place() == old(self).place(),
            final(self).wf(),
            final(self).spec_nbits() == old(self).spec_nbits(),
            final(self).view() == old(self).view().insert(bit as nat),
    {
        assert!(bit < self.nbits, "bitmap: bit out of range");
        let w = bit / 64;
        let ghost o = *self;
        proof {
            lemma_one_bit((bit % 64) as u64);
            lemma_one_bit((w % 64) as u64);
            assert(w < self.l0_words);
            assert(w / 64 < self.l1_words);
        }
        *self.l0_mut(w) |= 1u64 << (bit % 64);
        let ghost s1 = *self;
        proof {
            o.lemma_offsets();
            Self::lemma_write(&o, &s1, o.l0o(w as int), s1.l0v(w as int));
        }
        *self.l1_mut(w / 64) |= 1u64 << (w % 64);
        let ghost s2 = *self;
        proof {
            s1.lemma_offsets();
            Self::lemma_write(&s1, &s2, s1.l1o(w as int / 64), s2.l1v(w as int / 64));
            let a2 = s2.l0v(w as int);
            assert forall|b: u64| b < 64 implies #[trigger] has_bit(a2, b) == (has_bit(o.l0v(w as int), b) || b == (bit % 64) as u64) by {
                lemma_bit_ops(o.l0v(w as int), 1u64 << ((bit % 64) as u64), b);
            }
            assert forall|k: u64| k < 64 implies #[trigger] has_bit(s2.l1v(w as int / 64), k) == (has_bit(o.l1v(w as int / 64), k) || k == (w % 64) as u64) by {
                lemma_bit_ops(o.l1v(w as int / 64), 1u64 << ((w % 64) as u64), k);
            }
            assert forall|b: u64| b < 64 && w * 64 + b >= o.nbits implies !#[trigger] has_bit(a2, b) by {
                assert(!has_bit(o.l0v(w as int), b));
            }
            assert(has_bit(a2, (bit % 64) as u64));
            assert(has_bit(s2.l1v(w as int / 64), (w % 64) as u64));
            Self::lemma_word(&o, &s2, w as int, a2);
        }
        if w / 64 >= self.l1_used {
            self.l1_used = w / 64 + 1;
        }
        proof {
            let n = *self;
            Self::lemma_used(&s2, &n);
            assert(has_bit(n.l1v(w as int / 64), (w % 64) as u64));
            assert forall|j: int| n.l1_used <= j < n.l1_words implies #[trigger] n.l1v(j) == 0u64 by {
                assert(o.l1v(j) == 0u64);
            }
            assert(n.l1v(w as int / 64) != 0u64);
            if n.l1_used > 0 && n.l1_used - 1 != w / 64 {
                assert(n.l1v(n.l1_used - 1) == o.l1v(o.l1_used - 1));
            }
            Self::lemma_view_insert(&o, &n, bit as int);
        }
    }

    /// Remove `bit`. Removing a bit that is not present is a no-op.
    pub fn clear(&mut self, bit: usize)
        requires
            old(self).wf(),
            bit < old(self).spec_nbits(),
        ensures
            final(self).place() == old(self).place(),
            final(self).wf(),
            final(self).spec_nbits() == old(self).spec_nbits(),
            final(self).view() == old(self).view().remove(bit as nat),
    {
        assert!(bit < self.nbits, "bitmap: bit out of range");
        let w = bit / 64;
        let ghost o = *self;
        proof {
            lemma_one_bit((bit % 64) as u64);
            lemma_one_bit((w % 64) as u64);
            assert(w < self.l0_words);
            assert(w / 64 < self.l1_words);
            o.lemma_offsets();
        }
        let word = self.l0_mut(w);
        *word &= !(1u64 << (bit % 64));
        if *word == 0 {
            let ghost s1 = *self;
            proof {
                Self::lemma_clear_word(&o, &s1, bit as int);
            }
            *self.l1_mut(w / 64) &= !(1u64 << (w % 64));
            let ghost s2 = *self;
            proof {
                s1.lemma_offsets();
                Self::lemma_write(&s1, &s2, s1.l1o(w as int / 64), s2.l1v(w as int / 64));
                assert forall|k: u64| k < 64 implies #[trigger] has_bit(s2.l1v(w as int / 64), k) == (has_bit(o.l1v(w as int / 64), k) && k != (w % 64) as u64) by {
                    lemma_bit_ops(o.l1v(w as int / 64), 1u64 << ((w % 64) as u64), k);
                }
                assert forall|b: u64| b < 64 implies !#[trigger] has_bit(0u64, b) by {
                    assert((0u64 >> b) & 1u64 != 1u64) by (bit_vector);
                }
                Self::lemma_word(&o, &s2, w as int, 0u64);
                // The summary word only lost a bit, so a zero word stays zero.
                if o.l1v(w as int / 64) == 0u64 {
                    assert forall|k: u64| k < 64 implies !#[trigger] has_bit(s2.l1v(w as int / 64), k) by {
                        assert(!has_bit(0u64, k));
                    }
                    lemma_zero_of_no_bits(s2.l1v(w as int / 64));
                }
                assert forall|j: int| s2.l1_used <= j < s2.l1_words implies #[trigger] s2.l1v(j) == 0u64 by {
                    assert(o.l1v(j) == 0u64);
                }
            }
            while self.l1_used > 0 && self.l1(self.l1_used - 1) == 0
                invariant
                    self.inner(),
                    self.perms == s2.perms,
                    self.frame(&s2),
                    self.tail(),
                    forall|b: int| #[trigger] self.has(b) == s2.has(b),
                decreases self.l1_used,
            {
                let ghost p = *self;
                self.l1_used -= 1;
                proof {
                    Self::lemma_used(&p, self);
                    assert forall|j: int| self.l1_used <= j < self.l1_words implies #[trigger] self.l1v(j) == 0u64 by {
                        if j > self.l1_used {
                            assert(p.l1v(j) == 0u64);
                        }
                    }
                }
            }
            proof {
                Self::lemma_view_remove(&o, self, bit as int);
            }
        } else {
            proof {
                let s1 = *self;
                Self::lemma_clear_word(&o, &s1, bit as int);
                assert forall|j: int| s1.l1_used <= j < s1.l1_words implies #[trigger] s1.l1v(j) == 0u64 by {
                    assert(o.l1v(j) == 0u64);
                }
                if s1.l1_used > 0 {
                    assert(s1.l1v(s1.l1_used - 1) == o.l1v(o.l1_used - 1));
                }
                Self::lemma_view_remove(&o, &s1, bit as int);
            }
        }
    }

    /// Insert every bit in `start..start + len`, one word at a time.
    pub fn set_range(&mut self, start: usize, len: usize)
        requires
            old(self).wf(),
            start + len <= old(self).spec_nbits(),
        ensures
            final(self).place() == old(self).place(),
            final(self).wf(),
            final(self).spec_nbits() == old(self).spec_nbits(),
            final(self).view() == old(self).view().union(Set::<nat>::range(start as nat, (start + len) as nat)),
    {
        let end = start.checked_add(len).expect("bitmap: range wraps");
        assert!(end <= self.nbits, "bitmap: range out of range");
        let mut bit = start;
        let ghost o = *self;
        while bit < end
            invariant
                self.wf(),
                self.frame(&o),
                start <= bit <= end,
                end == start + len,
                end <= self.nbits,
                forall|x: int| #[trigger] self.has(x) == (o.has(x) || start <= x < bit),
            decreases end - bit,
        {
            let w = bit / 64;
            let lo = bit % 64;
            let hi = (end - w * 64).min(64);
            let ghost p = *self;
            proof {
                assert(w < self.l0_words);
                assert(w / 64 < self.l1_words);
                assert(lo < hi <= 64);
                lemma_one_bit((w % 64) as u64);
                p.lemma_offsets();
                if hi - lo < 64 {
                    lemma_one_shl((hi - lo) as usize);
                }
            }
            let mask = if hi - lo == 64 { !0 } else { ((1u64 << (hi - lo)) - 1) << lo };
            proof {
                lemma_word_mask(mask, lo as u64, hi as u64);
            }
            *self.l0_mut(w) |= mask;
            let ghost s1 = *self;
            proof {
                Self::lemma_write(&p, &s1, p.l0o(w as int), s1.l0v(w as int));
            }
            *self.l1_mut(w / 64) |= 1u64 << (w % 64);
            let ghost s2 = *self;
            proof {
                s1.lemma_offsets();
                Self::lemma_write(&s1, &s2, s1.l1o(w as int / 64), s2.l1v(w as int / 64));
                let a2 = s2.l0v(w as int);
                assert forall|b: u64| b < 64 implies #[trigger] has_bit(a2, b) == (has_bit(p.l0v(w as int), b) || (lo <= b < hi)) by {
                    lemma_bit_ops(p.l0v(w as int), mask, b);
                }
                assert forall|k: u64| k < 64 implies #[trigger] has_bit(s2.l1v(w as int / 64), k) == (has_bit(p.l1v(w as int / 64), k) || k == (w % 64) as u64) by {
                    lemma_bit_ops(p.l1v(w as int / 64), 1u64 << ((w % 64) as u64), k);
                }
                assert forall|b: u64| b < 64 && w * 64 + b >= p.nbits implies !#[trigger] has_bit(a2, b) by {
                    assert(!has_bit(p.l0v(w as int), b));
                }
                assert(has_bit(a2, lo as u64));
                assert(has_bit(s2.l1v(w as int / 64), (w % 64) as u64));
                Self::lemma_word(&p, &s2, w as int, a2);
            }
            if w / 64 >= self.l1_used {
                self.l1_used = w / 64 + 1;
            }
            bit = w * 64 + hi;
            proof {
                let n = *self;
                Self::lemma_used(&s2, &n);
                assert(has_bit(n.l1v(w as int / 64), (w % 64) as u64));
                assert forall|j: int| n.l1_used <= j < n.l1_words implies #[trigger] n.l1v(j) == 0u64 by {
                    assert(p.l1v(j) == 0u64);
                }
                assert(n.l1v(w as int / 64) != 0u64);
                if n.l1_used > 0 && n.l1_used - 1 != w / 64 {
                    assert(n.l1v(n.l1_used - 1) == p.l1v(p.l1_used - 1));
                }
                assert forall|x: int| #[trigger] n.has(x) == (o.has(x) || start <= x < bit) by {
                    if x / 64 == w && 0 <= x < n.nbits {
                        assert(has_bit(s2.l0v(w as int), (x % 64) as u64) == (has_bit(p.l0v(w as int), (x % 64) as u64) || (lo <= (x % 64) as u64 && (x % 64) < hi)));
                        assert(p.has(x) == has_bit(p.l0v(w as int), (x % 64) as u64));
                    }
                }
            }
        }
        proof {
            broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
            assert(self.view() =~= o.view().union(Set::<nat>::range(start as nat, (start + len) as nat)));
        }
    }

    /// Remove every bit in `start..start + len`, one word at a time. Bits
    /// that were not present are ignored.
    pub fn clear_range(&mut self, start: usize, len: usize)
        requires
            old(self).wf(),
            start + len <= old(self).spec_nbits(),
        ensures
            final(self).place() == old(self).place(),
            final(self).wf(),
            final(self).spec_nbits() == old(self).spec_nbits(),
            final(self).view() == old(self).view().difference(Set::<nat>::range(start as nat, (start + len) as nat)),
    {
        let end = start.checked_add(len).expect("bitmap: range wraps");
        assert!(end <= self.nbits, "bitmap: range out of range");
        let mut bit = start;
        let ghost o = *self;
        while bit < end
            invariant
                self.inner(),
                self.tail(),
                self.frame(&o),
                start <= bit <= end,
                end == start + len,
                end <= self.nbits,
                forall|x: int| #[trigger] self.has(x) == (o.has(x) && !(start <= x < bit)),
            decreases end - bit,
        {
            let w = bit / 64;
            let lo = bit % 64;
            let hi = (end - w * 64).min(64);
            let ghost p = *self;
            proof {
                assert(w < self.l0_words);
                assert(w / 64 < self.l1_words);
                assert(lo < hi <= 64);
                lemma_one_bit((w % 64) as u64);
                p.lemma_offsets();
                if hi - lo < 64 {
                    lemma_one_shl((hi - lo) as usize);
                }
            }
            let mask = if hi - lo == 64 { !0 } else { ((1u64 << (hi - lo)) - 1) << lo };
            proof {
                lemma_word_mask(mask, lo as u64, hi as u64);
            }
            let word = self.l0_mut(w);
            *word &= !mask;
            if *word == 0 {
                let ghost s1 = *self;
                proof {
                    Self::lemma_and_not_word(&p, &s1, w as int, mask);
                }
                *self.l1_mut(w / 64) &= !(1u64 << (w % 64));
                let ghost s2 = *self;
                proof {
                    s1.lemma_offsets();
                    Self::lemma_write(&s1, &s2, s1.l1o(w as int / 64), s2.l1v(w as int / 64));
                    assert forall|k: u64| k < 64 implies #[trigger] has_bit(s2.l1v(w as int / 64), k) == (has_bit(p.l1v(w as int / 64), k) && k != (w % 64) as u64) by {
                        lemma_bit_ops(p.l1v(w as int / 64), 1u64 << ((w % 64) as u64), k);
                    }
                    assert forall|b: u64| b < 64 implies !#[trigger] has_bit(0u64, b) by {
                        assert((0u64 >> b) & 1u64 != 1u64) by (bit_vector);
                    }
                    Self::lemma_word(&p, &s2, w as int, 0u64);
                    if p.l1v(w as int / 64) == 0u64 {
                        assert forall|k: u64| k < 64 implies !#[trigger] has_bit(s2.l1v(w as int / 64), k) by {
                            assert(!has_bit(0u64, k));
                        }
                        lemma_zero_of_no_bits(s2.l1v(w as int / 64));
                    }
                    assert forall|j: int| s2.l1_used <= j < s2.l1_words implies #[trigger] s2.l1v(j) == 0u64 by {
                        assert(p.l1v(j) == 0u64);
                    }
                    assert forall|x: int| #[trigger] s2.has(x) == (p.has(x) && !(x / 64 == w && has_bit(mask, (x % 64) as u64))) by {
                        if 0 <= x < p.nbits && x / 64 == w {
                            assert(has_bit(s1.l0v(w as int), (x % 64) as u64) == (p.has(x) && !has_bit(mask, (x % 64) as u64)));
                        }
                    }
                }
            } else {
                proof {
                    let s1 = *self;
                    Self::lemma_and_not_word(&p, &s1, w as int, mask);
                    assert forall|j: int| s1.l1_used <= j < s1.l1_words implies #[trigger] s1.l1v(j) == 0u64 by {
                        assert(p.l1v(j) == 0u64);
                    }
                }
            }
            bit = w * 64 + hi;
            proof {
                let n = *self;
                assert forall|x: int| #[trigger] n.has(x) == (o.has(x) && !(start <= x < bit)) by {
                    if x / 64 == w && 0 <= x {
                        assert(has_bit(mask, (x % 64) as u64) == (lo <= (x % 64) as u64 && (x % 64) < hi));
                    }
                }
            }
        }
        let ghost s3 = *self;
        while self.l1_used > 0 && self.l1(self.l1_used - 1) == 0
            invariant
                self.inner(),
                self.perms == s3.perms,
                self.frame(&s3),
                self.tail(),
                forall|b: int| #[trigger] self.has(b) == s3.has(b),
            decreases self.l1_used,
        {
            let ghost p = *self;
            self.l1_used -= 1;
            proof {
                Self::lemma_used(&p, self);
                assert forall|j: int| self.l1_used <= j < self.l1_words implies #[trigger] self.l1v(j) == 0u64 by {
                    if j > self.l1_used {
                        assert(p.l1v(j) == 0u64);
                    }
                }
            }
        }
        proof {
            broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
            assert(self.view() =~= o.view().difference(Set::<nat>::range(start as nat, (start + len) as nat)));
        }
    }

    /// Lowest bit in the set.
    #[inline]
    pub fn lowest_set(&self) -> (r: Option<usize>)
        requires
            self.wf(),
        ensures
            r is None ==> self.view() == Set::<nat>::empty(),
            r matches Some(b) ==> self.view().contains(b as nat)
                && forall|x: nat| #[trigger] self.view().contains(x) ==> b <= x,
    {
        let r = self.scan_l1(0);
        proof {
            broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
            if r is None {
                assert(self.view() =~= Set::<nat>::empty());
            }
        }
        r
    }

    /// Lowest bit in the set that is `>= from`.
    ///
    /// Level-0 words are read only when their level-1 bit is set. A set
    /// level-1 bit means the word is non-zero, so it has been written; the
    /// lazily committed tables in `class/` and `huge/` rely on this to
    /// keep scans away from words whose page is still `PROT_NONE`.
    pub fn lowest_set_from(&self, from: usize) -> (r: Option<usize>)
        requires
            self.wf(),
        ensures
            r is None ==> forall|x: nat| #[trigger] self.view().contains(x) ==> x < from,
            r matches Some(b) ==> self.view().contains(b as nat) && from <= b
                && forall|x: nat| #[trigger] self.view().contains(x) && from <= x ==> b <= x,
    {
        proof {
            broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
        }
        if from >= self.nbits {
            return None;
        }
        let w = from / 64;
        let j = w / 64;
        if j >= self.l1_used {
            proof {
                assert forall|x: int| #[trigger] self.has(x) implies x < from by {
                    self.lemma_member(x);
                    if x >= from {
                        lemma_div_div_mono(from as int, x);
                    }
                }
            }
            return None;
        }
        proof {
            assert(w < self.l0_words);
            assert(j < self.l1_words);
            lemma_shl_ones((w % 64) as u64);
        }
        // Non-zero level-0 words at or above `w` in the group covering `w`.
        let mut m1 = self.l1(j) & (!0u64 << (w % 64));
        let ghost m1_0 = m1;
        let ghost mut taken = false;
        proof {
            assert forall|k: u64| k < 64 implies #[trigger] has_bit(m1, k) == (has_bit(self.l1v(j as int), k) && k >= w % 64) by {
                lemma_bit_ops(self.l1v(j as int), !0u64 << ((w % 64) as u64), k);
            }
            lemma_tz(m1);
        }
        if m1 != 0 && j * 64 + m1.trailing_zeros() as usize == w {
            let m = self.l0(w) & (!0u64 << (from % 64));
            proof {
                lemma_shl_ones((from % 64) as u64);
                assert forall|k: u64| k < 64 implies #[trigger] has_bit(m, k) == (has_bit(self.l0v(w as int), k) && k >= from % 64) by {
                    lemma_bit_ops(self.l0v(w as int), !0u64 << ((from % 64) as u64), k);
                }
                lemma_tz(m);
            }
            if m != 0 {
                proof {
                    let t = u64_trailing_zeros(m) as int;
                    let b = w * 64 + t;
                    assert(has_bit(self.l0v(w as int), t as u64));
                    assert(b / 64 == w && b % 64 == t);
                    assert(b < self.nbits) by {
                        if b >= self.nbits {
                            assert(!has_bit(self.l0v(w as int), t as u64));
                        }
                    }
                    assert(self.has(b));
                    assert forall|x: int| #[trigger] self.has(x) && from <= x implies b <= x by {
                        if x / 64 == w {
                            assert(has_bit(m, (x % 64) as u64));
                        }
                    }
                }
                return Some(w * 64 + m.trailing_zeros() as usize);
            }
            proof {
                lemma_clear_lowest(m1);
                // Word `w` has no member at or above `from`.
                assert forall|x: int| #[trigger] self.has(x) && x / 64 == w implies x < from by {
                    if x >= from {
                        assert(has_bit(m, (x % 64) as u64));
                    }
                }
                taken = true;
            }
            m1 &= m1 - 1;
        }
        proof {
            let t0 = u64_trailing_zeros(m1_0) as int;
            assert(w == j * 64 + w % 64);
            // `m1` holds the level-1 bits of group `j` above `w % 64`, and
            // word `w` has no member at or above `from`.
            if !taken && m1_0 != 0u64 {
                assert(t0 >= w % 64) by {
                    if t0 < w % 64 {
                        assert(has_bit(m1_0, t0 as u64));
                    }
                }
            }
            assert forall|k: u64| k < 64 implies #[trigger] has_bit(m1, k) == (k > w % 64 && has_bit(self.l1v(j as int), k)) by {
                if !taken && m1_0 != 0u64 && k == w % 64 {
                    assert(!has_bit(m1_0, k)) by {
                        if has_bit(m1_0, k) {
                            assert(t0 <= k) by {
                                if t0 > k {
                                    assert(!has_bit(m1_0, k));
                                }
                            }
                        }
                    }
                }
                if !taken && m1_0 == 0u64 {
                    assert(!has_bit(0u64, k)) by {
                        assert((0u64 >> k) & 1u64 != 1u64) by (bit_vector);
                    }
                }
            }
            assert forall|x: int| #[trigger] self.has(x) && x / 64 == w implies x < from by {
                if !taken && x >= from {
                    self.lemma_member(x);
                    assert(has_bit(m1_0, (w % 64) as u64));
                    assert(t0 <= w % 64) by {
                        if t0 > w % 64 {
                            assert(!has_bit(m1_0, (w % 64) as u64));
                        }
                    }
                }
            }
            // Every member at or above `from` in group `j` is in a word
            // above `w` whose bit is in `m1`.
            assert forall|x: int| #[trigger] self.has(x) && from <= x && x / 64 / 64 <= j implies
                x / 64 / 64 == j && has_bit(m1, (x / 64 % 64) as u64) by {
                self.lemma_member(x);
                lemma_div_div_mono(from as int, x);
                assert(x / 64 >= w);
                assert(x / 64 != w);
            }
            lemma_tz(m1);
        }
        if m1 != 0 {
            let w2 = j * 64 + m1.trailing_zeros() as usize;
            proof {
                let t = u64_trailing_zeros(m1) as u64;
                assert(has_bit(m1, t));
                self.lemma_found(j as int, t);
            }
            let r = w2 * 64 + self.l0(w2).trailing_zeros() as usize;
            proof {
                let t = u64_trailing_zeros(m1) as int;
                let t2 = u64_trailing_zeros(self.l0v(w2 as int)) as int;
                assert(self.has(r as int));
                assert forall|x: nat| #[trigger] self.view().contains(x) && from <= x implies r <= x by {
                    let xi = x as int;
                    assert(self.has(xi));
                    if xi / 64 / 64 <= j {
                        assert(has_bit(m1, (xi / 64 % 64) as u64));
                        assert(xi / 64 % 64 >= t) by {
                            if xi / 64 % 64 < t {
                                assert(!has_bit(m1, (xi / 64 % 64) as u64));
                            }
                        }
                        if xi / 64 == w2 {
                            assert(xi % 64 >= t2) by {
                                if xi % 64 < t2 {
                                    assert(!has_bit(self.l0v(w2 as int), (xi % 64) as u64));
                                }
                            }
                        }
                    }
                }
            }
            return Some(r);
        }
        let r = self.scan_l1(j + 1);
        proof {
            assert forall|x: int| #[trigger] self.has(x) && from <= x implies x / 64 / 64 >= j + 1 by {
                if x / 64 / 64 <= j {
                    assert(has_bit(m1, (x / 64 % 64) as u64));
                    assert(!has_bit(0u64, (x / 64 % 64) as u64)) by {
                        let k = (x / 64 % 64) as u64;
                        assert((0u64 >> k) & 1u64 != 1u64) by (bit_vector);
                    }
                }
            }
            if r is None {
                assert forall|x: nat| #[trigger] self.view().contains(x) implies x < from by {
                    assert(self.has(x as int));
                }
            }
            if let Some(b) = r {
                assert(from <= b) by {
                    if b < from {
                        lemma_div_div_mono(b as int, from as int);
                    }
                }
            }
        }
        r
    }

    /// Lowest set bit whose level-1 word index is `>= j`.
    fn scan_l1(&self, j: usize) -> (r: Option<usize>)
        requires
            self.wf(),
            j <= self.l1_used,
        ensures
            r matches Some(b) ==> self.has(b as int) && b as int / 64 / 64 >= j
                && forall|x: int| #[trigger] self.has(x) && x / 64 / 64 >= j ==> b <= x,
            r is None ==> forall|x: int| #[trigger] self.has(x) ==> x / 64 / 64 < j,
    {
        for i in j..self.l1_used
            invariant
                self.wf(),
                forall|x: int| #[trigger] self.has(x) && x / 64 / 64 >= j ==> x / 64 / 64 >= i,
        {
            let m1 = self.l1(i);
            proof {
                lemma_tz(m1);
                if m1 == 0 {
                    assert forall|x: int| #[trigger] self.has(x) && x / 64 / 64 >= j implies x / 64 / 64 >= i + 1 by {
                        self.lemma_member(x);
                    }
                }
            }
            if m1 != 0 {
                let w = i * 64 + m1.trailing_zeros() as usize;
                proof {
                    self.lemma_found(i as int, u64_trailing_zeros(m1) as u64);
                }
                let r = w * 64 + self.l0(w).trailing_zeros() as usize;
                proof {
                    let t = u64_trailing_zeros(m1) as int;
                    let t0 = u64_trailing_zeros(self.l0v(w as int)) as int;
                    assert forall|x: int| #[trigger] self.has(x) && x / 64 / 64 >= j implies r <= x by {
                        self.lemma_member(x);
                        if x / 64 / 64 == i {
                            let k = x / 64 % 64;
                            assert(has_bit(m1, k as u64));
                            assert(k >= t);
                            if x / 64 == w {
                                assert(x % 64 >= t0);
                            }
                        }
                    }
                }
                return Some(r);
            }
        }
        proof {
            assert forall|x: int| #[trigger] self.has(x) implies x / 64 / 64 < j by {
                self.lemma_member(x);
            }
        }
        None
    }
}

// Slots of a class bitmap.
//
// The class bitmap's bits are numbered `slab * slab_bits + slot` across
// the whole class region; [`Slots`] holds a class's geometry, and
// [`Slots::addr`] maps a bit to its slot's address, one to one on slots
// ([`lemma_addr_inj`]).

/// One class bitmap's geometry: `loc` and `mloc`, two ghost resource
/// locations that name the class's slots in ghost state; the bits per slab (`sb`), the slots per slab, the bits of the whole region
/// (`nslabs * sb`); the slot geometry (the class region's base, the slab
/// size and the slot size); and the guard geometry (the slab table's
/// `lg_slabs_per_page` and `guard_mask`).
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost struct Slots {
    pub loc: Loc,
    pub mloc: Loc,
    pub sb: int,
    pub nslots: int,
    pub nbits: int,
    pub base: int,
    pub slab_size: int,
    pub size: int,
    pub glg: u8,
    pub gmask: u32,
}

impl Slots {
    /// Bit `b` of the class bitmap is a slot: slot `b % sb` of slab
    /// `b / sb` is below `nslots`, and the slab is in the region.
    pub open spec fn real(self, b: int) -> bool {
        0 <= b < self.nbits && b % self.sb < self.nslots
    }

    /// The address of bit `b`'s slot: slot `b % sb` of slab `b / sb`.
    pub open spec fn addr(self, b: int) -> int {
        self.base + (b / self.sb) * self.slab_size + (b % self.sb) * self.size
    }

    /// The geometry of a class: slots fit in a slab.
    pub open spec fn wf(self) -> bool {
        &&& 0 < self.nslots <= self.sb
        &&& 0 < self.size
        &&& self.nslots * self.size <= self.slab_size
    }
}

/// The addresses of the slots of bits `bits`.
pub open spec fn addrs(s: Slots, bits: Set<int>) -> Set<int> {
    bits.map(|b: int| s.addr(b))
}

/// Two slots at one address are one slot.
pub proof fn lemma_addr_inj(s: Slots, b1: int, b2: int)
    requires
        s.wf(),
        s.real(b1),
        s.real(b2),
        s.addr(b1) == s.addr(b2),
    ensures
        b1 == b2,
{
    let (q1, r1, q2, r2) = (b1 / s.sb, b1 % s.sb, b2 / s.sb, b2 % s.sb);
    assert(0 <= r1 * s.size < s.slab_size) by (nonlinear_arith)
        requires 0 <= r1 < s.nslots, 0 < s.size, s.nslots * s.size <= s.slab_size;
    assert(0 <= r2 * s.size < s.slab_size) by (nonlinear_arith)
        requires 0 <= r2 < s.nslots, 0 < s.size, s.nslots * s.size <= s.slab_size;
    let x = q1 * s.slab_size + r1 * s.size;
    assert(x == q2 * s.slab_size + r2 * s.size);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(x, s.slab_size, q1, r1 * s.size);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(x, s.slab_size, q2, r2 * s.size);
    assert(r1 == r2) by (nonlinear_arith)
        requires r1 * s.size == r2 * s.size, 0 < s.size;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(b1, s.sb);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(b2, s.sb);
}

/// The addresses of a difference of slot sets are the difference of their
/// addresses.
pub proof fn lemma_addrs_difference(s: Slots, a: Set<int>, b: Set<int>)
    requires
        s.wf(),
        forall|x: int| #[trigger] a.contains(x) ==> s.real(x),
        forall|x: int| #[trigger] b.contains(x) ==> s.real(x),
    ensures
        addrs(s, a.difference(b)) == addrs(s, a).difference(addrs(s, b)),
{
    broadcast use Set::lemma_map_contains;
    let f = |x: int| s.addr(x);
    assert forall|y: int| #[trigger] addrs(s, a.difference(b)).contains(y) == addrs(s, a).difference(addrs(s, b)).contains(y) by {
        if addrs(s, a.difference(b)).contains(y) {
            let x = choose|x: int| a.difference(b).contains(x) && y == f(x);
            if addrs(s, b).contains(y) {
                let x2 = choose|x2: int| b.contains(x2) && y == f(x2);
                lemma_addr_inj(s, x, x2);
            }
        }
        if addrs(s, a).difference(addrs(s, b)).contains(y) {
            let x = choose|x: int| a.contains(x) && y == f(x);
            assert(!b.contains(x));
            assert(a.difference(b).contains(x));
        }
    }
    assert(addrs(s, a.difference(b)) =~= addrs(s, a).difference(addrs(s, b)));
}

/// The addresses of a union are the union of the addresses.
pub proof fn lemma_addrs_union(s: Slots, a: Set<int>, b: Set<int>)
    ensures
        addrs(s, a.union(b)) == addrs(s, a).union(addrs(s, b)),
{
    a.lemma_map_union_commute(b, |x: int| s.addr(x));
}

/// The slots of mask `x` of word `i` of a slab whose slot 0 is bit
/// `first`: bits `first + 64 * i + k` for the set bits `k` of `x`.
pub open spec fn mask_bits(s: Slots, first: int, i: int, x: u64) -> Set<int> {
    Set::<int>::range(first + 64 * i, first + 64 * i + 64).filter(
        |b: int| has_bit(x, (b - first - 64 * i) as u64) && s.real(b))
}

/// Slot `t` of slab `slab` is bit `slab * sb + t`, at the slot's address,
/// and a slot exactly when `t < nslots` (for a slab in the region).
pub proof fn lemma_slab_bit(s: Slots, slab: int, t: int)
    requires
        s.wf(),
        0 <= slab,
        0 <= t < s.sb,
        slab * s.sb + s.sb <= s.nbits,
    ensures
        (slab * s.sb + t) / s.sb == slab,
        (slab * s.sb + t) % s.sb == t,
        s.addr(slab * s.sb + t) == s.base + slab * s.slab_size + t * s.size,
        s.real(slab * s.sb + t) == (t < s.nslots),
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(slab * s.sb + t, s.sb, slab, t);
    assert(0 <= slab * s.sb) by (nonlinear_arith)
        requires 0 <= slab, 0 < s.sb;
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// Deterministic xorshift, as in `layout::tests`.
    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    fn atomic_words(n: usize) -> Vec<AtomicU64> {
        (0..n).map(|_| AtomicU64::new(0)).collect()
    }

    #[test]
    fn lowest_n_bits_keeps_the_lowest() {
        assert_eq!(lowest_n_bits(0, 5), 0);
        assert_eq!(lowest_n_bits(!0, 0), 0);
        assert_eq!(lowest_n_bits(!0, 64), !0);
        assert_eq!(lowest_n_bits(!0, 100), !0);
        assert_eq!(lowest_n_bits(!0, 3), 0b111);
        assert_eq!(lowest_n_bits(0b1011_0000, 2), 0b0011_0000);
        assert_eq!(lowest_n_bits(1 << 63, 1), 1 << 63);
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..20_000 {
            let m = xorshift(&mut s);
            let n = (xorshift(&mut s) % 70) as usize;
            let r = lowest_n_bits(m, n);
            assert_eq!(r & !m, 0, "result must be a subset");
            assert_eq!(r.count_ones() as usize, n.min(m.count_ones() as usize));
            // Every bit of m below the highest bit of r is in r.
            if r != 0 {
                let hi = 63 - r.leading_zeros();
                let below = if hi == 63 { !0 } else { (1u64 << (hi + 1)) - 1 };
                assert_eq!(r, m & below);
            }
        }
    }

    #[test]
    fn init_sets_padding_only() {
        for nbits in [1usize, 5, 63, 64, 65, 127, 128, 129, 4096, 4097] {
            let words = atomic_words(nbits.div_ceil(64));
            let bm = AtomicBitmap::from_words(&words);
            bm.init(nbits);
            for b in 0..nbits {
                assert!(!bm.is_set(b), "nbits={nbits} bit {b} should be clear");
            }
            for b in nbits..words.len() * 64 {
                assert!(bm.is_set(b), "nbits={nbits} padding bit {b} should be set");
            }
            assert_eq!(bm.is_full(), false);
        }
    }

    #[test]
    #[should_panic(expected = "nbits does not match the view")]
    fn init_with_more_bits_than_the_view_aborts() {
        let words = atomic_words(1);
        AtomicBitmap::from_words(&words).init(65);
    }

    #[test]
    #[should_panic(expected = "nbits does not match the view")]
    fn init_of_a_sub_word_view_past_its_share_aborts() {
        let word = AtomicU64::new(0);
        AtomicBitmap::sub_word(&word, 8, 4).init(5);
    }

    #[test]
    fn claim_takes_lowest_bits_first_and_never_padding() {
        let nbits = 130;
        let words = atomic_words(3);
        let bm = AtomicBitmap::from_words(&words);
        bm.init(nbits);
        let (w, m) = bm.claim(10).unwrap();
        assert_eq!((w, m), (0, (1 << 10) - 1));
        let (w, m) = bm.claim(64).unwrap();
        assert_eq!((w, m), (0, !0 << 10));
        let (w, m) = bm.claim(100).unwrap();
        assert_eq!((w, m), (1, !0));
        let (w, m) = bm.claim(100).unwrap();
        assert_eq!((w, m), (2, 0b11));
        assert!(bm.claim(1).is_none());
        assert!(bm.is_full());
        let (prev, was_full) = bm.release(2, 0b10);
        assert_eq!(prev & 0b10, 0b10);
        assert!(was_full);
        assert!(!bm.is_full());
        assert!(!bm.release(2, 0b01).1);
        assert_eq!(bm.claim(1), Some((2, 0b01)));
        assert_eq!(bm.claim(5), Some((2, 0b10)));
    }

    #[test]
    fn claim_from_wraps() {
        let words = atomic_words(4);
        let bm = AtomicBitmap::from_words(&words);
        bm.init(256);
        assert_eq!(bm.claim_from(2, 1), Some((2, 1)));
        assert_eq!(bm.claim_from(3, 64), Some((3, !0)));
        assert_eq!(bm.claim_from(3, 64), Some((0, !0)));
        assert_eq!(bm.claim_from(3, 64), Some((1, !0)));
        assert_eq!(bm.claim_from(3, 64), Some((2, !0 << 1)));
        assert_eq!(bm.claim_from(3, 64), None);
    }

    #[test]
    fn release_reports_double_release() {
        let words = atomic_words(1);
        let bm = AtomicBitmap::from_words(&words);
        bm.init(64);
        bm.claim(3);
        let (prev, _) = bm.release(0, 0b101);
        assert_eq!(prev & 0b101, 0b101);
        let (prev, _) = bm.release(0, 0b100);
        assert_ne!(prev & 0b100, 0b100, "second release must be detectable");
        assert!(!bm.is_set(2));
        assert!(bm.is_set(1));
    }

    /// Single-threaded model check of claim/release against a `Vec<bool>`.
    #[test]
    fn atomic_bitmap_matches_model() {
        let mut s = 0xDEAD_BEEF_CAFE_F00Du64;
        for &nbits in &[1usize, 64, 65, 100, 1000, 4096, 4099] {
            let words = atomic_words(nbits.div_ceil(64));
            let bm = AtomicBitmap::from_words(&words);
            bm.init(nbits);
            let mut model = vec![false; nbits];
            let mut owned: Vec<usize> = Vec::new();
            for _ in 0..20_000 {
                match xorshift(&mut s) % 4 {
                    0 | 1 => {
                        let n = (xorshift(&mut s) % 70) as usize;
                        let start = (xorshift(&mut s) as usize) % words.len();
                        let free_before = model.iter().filter(|b| !**b).count();
                        match bm.claim_from(start, n) {
                            Some((w, m)) => {
                                assert!(n > 0);
                                assert!(m != 0);
                                assert_eq!(m.count_ones() as usize, n.min(m.count_ones() as usize));
                                let mut rest = m;
                                while rest != 0 {
                                    let bit = w * 64 + rest.trailing_zeros() as usize;
                                    rest &= rest - 1;
                                    assert!(bit < nbits, "claimed padding bit {bit}");
                                    assert!(!model[bit], "claimed a set bit {bit}");
                                    model[bit] = true;
                                    owned.push(bit);
                                }
                                // Claims are exhaustive within a word: if we
                                // got fewer than n, the word is now full.
                                if (m.count_ones() as usize) < n {
                                    assert_eq!(bm.load(w), !0);
                                }
                            }
                            None => assert!(n == 0 || free_before == 0),
                        }
                    }
                    _ => {
                        if owned.is_empty() {
                            continue;
                        }
                        // Release a random batch grouped by word.
                        let k = 1 + (xorshift(&mut s) as usize) % owned.len().min(80);
                        let mut masks: Vec<(usize, u64)> = Vec::new();
                        for _ in 0..k {
                            let i = (xorshift(&mut s) as usize) % owned.len();
                            let bit = owned.swap_remove(i);
                            assert!(model[bit]);
                            model[bit] = false;
                            match masks.iter_mut().find(|(w, _)| *w == bit / 64) {
                                Some((_, m)) => *m |= 1 << (bit % 64),
                                None => masks.push((bit / 64, 1 << (bit % 64))),
                            }
                        }
                        for (w, m) in masks {
                            let (prev, _) = bm.release(w, m);
                            assert_eq!(prev & m, m);
                        }
                    }
                }
                for (b, &set) in model.iter().enumerate() {
                    assert_eq!(bm.is_set(b), set, "nbits={nbits} bit {b}");
                }
                assert_eq!(bm.is_full(), model.iter().all(|b| *b));
            }
        }
    }

    /// Sub-word slabs: several slabs share one word, each view sees only
    /// its own range, in slab coordinates, and never touches a neighbour.
    #[test]
    fn sub_word_views_are_independent() {
        for nbits in [4usize, 8, 16, 32] {
            let word = AtomicU64::new(0);
            let nslots = nbits - nbits / 4; // some padding in every slab
            let views: Vec<AtomicBitmap> = (0..64 / nbits)
                .map(|k| AtomicBitmap::sub_word(&word, (k * nbits) as u32, nbits))
                .collect();
            for v in &views {
                v.init(nslots);
                assert_eq!(v.words(), 1);
                assert_eq!(v.nbits(), nbits);
                assert_eq!(v.allocated(nslots), 0);
                assert!(!v.is_full());
            }
            // Padding bits set, slot bits clear, for every slab.
            for (k, v) in views.iter().enumerate() {
                for b in 0..nslots {
                    assert!(!v.is_set(b), "nbits {nbits} slab {k} slot {b}");
                }
                assert_eq!(v.load(0), (!0u64 << nslots) & ((1u64 << nbits) - 1));
            }
            // Fill slab 1 only; its neighbours are untouched.
            let v = &views[1];
            let (w, m) = v.claim(64).unwrap();
            assert_eq!((w, m), (0, (1u64 << nslots) - 1));
            assert!(v.is_full());
            assert!(v.claim(1).is_none());
            assert_eq!(v.allocated(nslots), nslots);
            for (k, o) in views.iter().enumerate() {
                if k != 1 {
                    assert!(!o.is_full(), "slab {k}");
                    assert_eq!(o.allocated(nslots), 0, "slab {k}");
                }
            }
            // Release one slot: the slab's part of the word was full.
            let (prev, was_full) = v.release(0, 0b1);
            assert_eq!(prev, (1u64 << nbits) - 1);
            assert!(was_full);
            assert!(!v.is_set(0) && v.is_set(1));
            let (prev, was_full) = v.release(0, 0b10);
            assert!(!was_full);
            assert_eq!(prev & 0b10, 0b10);
            assert_eq!(
                v.release(0, 0b10).0 & 0b10,
                0,
                "double release must be detectable"
            );
            // Re-init of slab 1 leaves the others alone and clears it.
            views[0].claim(3);
            v.init(nslots);
            assert_eq!(v.allocated(nslots), 0);
            assert_eq!(views[0].allocated(nslots), 3);
            let whole = word.load(Ordering::Relaxed);
            for (k, o) in views.iter().enumerate() {
                assert_eq!(
                    (whole >> (k * nbits)) & ((1u64 << nbits) - 1),
                    o.load(0),
                    "slab {k}"
                );
            }
        }
    }

    /// Model check of a sub-word view against a `Vec<bool>`, like the
    /// whole-word one, with live neighbours in the same word.
    #[test]
    fn sub_word_matches_model() {
        let mut s = 0x1234_5678_9ABC_DEF0u64;
        for nbits in [4usize, 8, 16, 32] {
            let word = AtomicU64::new(0);
            let shift = (xorshift(&mut s) as u32 % (64 / nbits as u32)) * nbits as u32;
            let bm = AtomicBitmap::sub_word(&word, shift, nbits);
            let nslots = nbits - 1;
            bm.init(nslots);
            // A neighbour claims some bits of the same word.
            let other_shift = (shift + nbits as u32) % 64;
            let other = AtomicBitmap::sub_word(&word, other_shift, nbits);
            other.init(nslots);
            other.claim(2);
            let before = word.load(Ordering::Relaxed) & !bm.mask;
            let mut model = vec![false; nslots];
            for _ in 0..5000 {
                if xorshift(&mut s) % 2 == 0 {
                    let n = (xorshift(&mut s) % 6) as usize;
                    match bm.claim(n) {
                        Some((0, m)) => {
                            assert!(n > 0 && m != 0 && m < 1 << nslots);
                            let mut rest = m;
                            while rest != 0 {
                                let b = rest.trailing_zeros() as usize;
                                rest &= rest - 1;
                                assert!(!model[b]);
                                model[b] = true;
                            }
                        }
                        Some(_) => unreachable!(),
                        None => assert!(n == 0 || model.iter().all(|b| *b)),
                    }
                } else {
                    let set: Vec<usize> = (0..nslots).filter(|&b| model[b]).collect();
                    if set.is_empty() {
                        continue;
                    }
                    let b = set[xorshift(&mut s) as usize % set.len()];
                    let (prev, was_full) = bm.release(0, 1 << b);
                    assert_eq!(prev & (1 << b), 1 << b);
                    assert_eq!(was_full, set.len() == nslots);
                    model[b] = false;
                }
                for (b, &v) in model.iter().enumerate() {
                    assert_eq!(bm.is_set(b), v);
                }
                assert_eq!(bm.is_full(), model.iter().all(|b| *b));
                assert_eq!(bm.allocated(nslots), model.iter().filter(|b| **b).count());
                assert_eq!(
                    word.load(Ordering::Relaxed) & !bm.mask,
                    before,
                    "neighbour bits changed"
                );
            }
        }
    }

    /// Concurrent claimers never receive the same slot. Each thread stamps
    /// its id into a per-slot cell on claim and expects to find it there on
    /// release; any overlap shows up as a stale stamp.
    #[test]
    fn concurrent_claims_are_disjoint() {
        use core::sync::atomic::AtomicU8;
        use std::sync::Arc;
        use std::thread;

        const NBITS: usize = 1000;
        const THREADS: usize = 8;
        const ROUNDS: usize = 4000;
        let words: Arc<Vec<AtomicU64>> = Arc::new(atomic_words(NBITS.div_ceil(64)));
        AtomicBitmap::from_words(&words).init(NBITS);
        let stamps: Arc<Vec<AtomicU8>> = Arc::new((0..NBITS).map(|_| AtomicU8::new(0)).collect());
        let total = Arc::new(AtomicU64::new(0));

        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let words = Arc::clone(&words);
                let stamps = Arc::clone(&stamps);
                let total = Arc::clone(&total);
                thread::spawn(move || {
                    let id = t as u8 + 1;
                    let bm = AtomicBitmap::from_words(&words);
                    let mut s = 0x1234_5678_9ABC_DEF0u64
                        ^ (t as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    let mut owned: Vec<usize> = Vec::new();
                    let mut claimed = 0u64;
                    for _ in 0..ROUNDS {
                        if xorshift(&mut s) % 2 == 0 || owned.is_empty() {
                            let n = 1 + (xorshift(&mut s) % 40) as usize;
                            let start = (xorshift(&mut s) as usize) % words.len();
                            if let Some((w, mut m)) = bm.claim_from(start, n) {
                                while m != 0 {
                                    let bit = w * 64 + m.trailing_zeros() as usize;
                                    m &= m - 1;
                                    assert!(bit < NBITS);
                                    let prev = stamps[bit].swap(id, Ordering::AcqRel);
                                    assert_eq!(prev, 0, "slot {bit} was owned by thread {prev}");
                                    owned.push(bit);
                                    claimed += 1;
                                }
                            }
                        } else {
                            let k = 1 + (xorshift(&mut s) as usize) % owned.len().min(32);
                            for _ in 0..k {
                                let i = (xorshift(&mut s) as usize) % owned.len();
                                let bit = owned.swap_remove(i);
                                assert!(bm.is_set(bit));
                                let prev = stamps[bit].swap(0, Ordering::AcqRel);
                                assert_eq!(prev, id, "slot {bit} stamp changed under us");
                                let (prev, _) = bm.release(bit / 64, 1 << (bit % 64));
                                assert_ne!(prev & (1 << (bit % 64)), 0);
                            }
                        }
                    }
                    for bit in owned {
                        assert_eq!(stamps[bit].swap(0, Ordering::AcqRel), id);
                        bm.release(bit / 64, 1 << (bit % 64));
                    }
                    total.fetch_add(claimed, Ordering::Relaxed);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let bm = AtomicBitmap::from_words(&words);
        for b in 0..NBITS {
            assert!(!bm.is_set(b), "bit {b} leaked");
            assert_eq!(stamps[b].load(Ordering::Relaxed), 0);
        }
        for b in NBITS..words.len() * 64 {
            assert!(bm.is_set(b), "padding bit {b} was claimed");
        }
        assert!(total.load(Ordering::Relaxed) > (THREADS * ROUNDS / 4) as u64);
    }

    /// Owned storage for a `TwoLevelBitmap` in tests.
    struct Owned {
        _storage: Vec<u64>,
        bm: TwoLevelBitmap,
    }

    fn owned(nbits: usize) -> Owned {
        let mut storage = vec![0u64; TwoLevelBitmap::words_needed(nbits)];
        // SAFETY: the vector is zeroed, sized by `words_needed`, and lives
        // exactly as long as the bitmap in the same struct.
        let bm = unsafe { TwoLevelBitmap::from_raw(nbits, storage.as_mut_ptr()) };
        Owned {
            _storage: storage,
            bm,
        }
    }

    #[test]
    fn words_needed_is_exact() {
        assert_eq!(TwoLevelBitmap::words_needed(1), 2);
        assert_eq!(TwoLevelBitmap::words_needed(64), 2);
        assert_eq!(TwoLevelBitmap::words_needed(65), 3);
        assert_eq!(TwoLevelBitmap::words_needed(4096), 65);
        assert_eq!(TwoLevelBitmap::words_needed(4097), 67);
        assert_eq!(TwoLevelBitmap::words_needed(1 << 22), 65536 + 1024);
    }

    #[test]
    fn two_level_basics() {
        let mut o = owned(5000);
        let bm = &mut o.bm;
        assert_eq!(bm.len(), 5000);
        assert!(bm.is_empty());
        assert_eq!(bm.lowest_set(), None);
        bm.set(4999);
        bm.set(70);
        bm.set(4100);
        assert!(!bm.is_empty());
        assert_eq!(bm.lowest_set(), Some(70));
        assert_eq!(bm.lowest_set_from(0), Some(70));
        assert_eq!(bm.lowest_set_from(70), Some(70));
        assert_eq!(bm.lowest_set_from(71), Some(4100));
        assert_eq!(bm.lowest_set_from(4100), Some(4100));
        assert_eq!(bm.lowest_set_from(4101), Some(4999));
        assert_eq!(bm.lowest_set_from(4999), Some(4999));
        assert_eq!(bm.lowest_set_from(5000), None);
        assert_eq!(bm.lowest_set_from(usize::MAX), None);
        assert_eq!(bm.l1_used, 2);
        bm.clear(4999);
        assert_eq!(bm.l1_used, 2);
        bm.clear(4100);
        assert_eq!(
            bm.l1_used, 1,
            "used prefix shrinks when the top word empties"
        );
        bm.clear(4100); // no-op
        assert_eq!(bm.lowest_set(), Some(70));
        bm.clear(70);
        assert!(bm.is_empty());
        assert_eq!(bm.l1_used, 0);
        assert_eq!(bm.lowest_set(), None);
    }

    #[test]
    fn two_level_tiny_universes() {
        for nbits in [1usize, 2, 63, 64, 65, 128, 129] {
            let mut o = owned(nbits);
            for b in (0..nbits).rev() {
                o.bm.set(b);
                assert_eq!(o.bm.lowest_set(), Some(b));
            }
            for b in 0..nbits {
                assert_eq!(o.bm.lowest_set(), Some(b));
                assert_eq!(o.bm.lowest_set_from(b), Some(b));
                o.bm.clear(b);
            }
            assert!(o.bm.is_empty());
        }
    }

    /// Randomised model check of `TwoLevelBitmap` against a `Vec<bool>`.
    /// Several chunked views interleaved in one table (the per-arena slab
    /// set table of `class/`: row `r` of view `v` at `r * stride + v`),
    /// with universes of different sizes, each checked against its own
    /// model and against the dense form.
    #[test]
    fn chunked_views_interleave_and_match_model() {
        let sizes = [1usize, 64, 65, 4096, 4097, 10_000, 300_000, 70, 5000];
        let stride = sizes.len();
        let max_bits = *sizes.iter().max().unwrap();
        let rows = TwoLevelBitmap::chunked_rows(max_bits);
        assert_eq!(rows, 65 * 74);
        assert_eq!(TwoLevelBitmap::chunked_l0_row(0), 1);
        assert_eq!(TwoLevelBitmap::chunked_l0_row(63), 64);
        assert_eq!(TwoLevelBitmap::chunked_l0_row(64), 66);
        let mut storage = vec![0u64; rows * stride];
        let base = storage.as_mut_ptr();
        // SAFETY: every view's words are `r * stride + v`, disjoint across
        // views, inside the zeroed vector, which outlives the views.
        let mut views: Vec<TwoLevelBitmap> = sizes
            .iter()
            .enumerate()
            .map(|(v, &n)| unsafe { TwoLevelBitmap::from_raw_chunked(n, base.add(v), stride) })
            .collect();
        let mut models: Vec<Vec<bool>> = sizes.iter().map(|&n| vec![false; n]).collect();
        let mut dense: Vec<Owned> = sizes.iter().map(|&n| owned(n)).collect();
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..60_000 {
            let r = xorshift(&mut s);
            let v = (r % stride as u64) as usize;
            let nbits = sizes[v];
            let bit = if r % 8 == 0 {
                (xorshift(&mut s) as usize) % nbits
            } else {
                (xorshift(&mut s) as usize) % nbits.min(700)
            };
            match (r >> 8) % 5 {
                0 | 1 => {
                    views[v].set(bit);
                    dense[v].bm.set(bit);
                    models[v][bit] = true;
                }
                2 | 3 => {
                    views[v].clear(bit);
                    dense[v].bm.clear(bit);
                    models[v][bit] = false;
                }
                _ => {
                    let from = (xorshift(&mut s) as usize) % (nbits + 2);
                    let expect = models[v]
                        .iter()
                        .skip(from)
                        .position(|b| *b)
                        .map(|p| p + from);
                    assert_eq!(
                        views[v].lowest_set_from(from),
                        expect,
                        "view {v} from={from}"
                    );
                    assert_eq!(dense[v].bm.lowest_set_from(from), expect);
                }
            }
            assert_eq!(views[v].is_set(bit), models[v][bit]);
            assert_eq!(
                views[v].lowest_set(),
                models[v].iter().position(|b| *b),
                "view {v}"
            );
            assert_eq!(views[v].l1_used, dense[v].bm.l1_used, "view {v}");
        }
        for (v, m) in models.iter().enumerate() {
            for b in 0..sizes[v] {
                assert_eq!(views[v].is_set(b), m[b], "view {v} bit {b}");
            }
        }
        // Word 0 of both levels of every view sits in the first two rows:
        // what a small heap touches of the whole table. Set bit 0 of every
        // view and check that exactly rows 0 and 1 hold those words.
        for v in views.iter_mut() {
            v.set(0);
        }
        drop(views);
        for v in 0..stride {
            assert_ne!(storage[v] & 1, 0, "view {v} level-1 word 0 in row 0");
            assert_ne!(
                storage[stride + v] & 1,
                0,
                "view {v} level-0 word 0 in row 1"
            );
        }
    }

    #[test]
    fn two_level_matches_model() {
        let mut s = 0x0123_4567_89AB_CDEFu64;
        for &nbits in &[1usize, 64, 65, 4096, 4097, 10_000, 300_000] {
            let mut o = owned(nbits);
            let mut model = vec![false; nbits];
            // Bias towards a small prefix so lowest_set has work to do, but
            // also touch the top so l1_used grows and shrinks.
            let ops = if nbits > 100_000 { 20_000 } else { 40_000 };
            for _ in 0..ops {
                let r = xorshift(&mut s);
                let bit = if r % 8 == 0 {
                    (xorshift(&mut s) as usize) % nbits
                } else {
                    (xorshift(&mut s) as usize) % nbits.min(700)
                };
                match r % 5 {
                    0 | 1 => {
                        o.bm.set(bit);
                        model[bit] = true;
                    }
                    2 | 3 => {
                        o.bm.clear(bit);
                        model[bit] = false;
                    }
                    _ => {
                        let from = (xorshift(&mut s) as usize) % (nbits + 2);
                        let expect = model.iter().skip(from).position(|b| *b).map(|p| p + from);
                        assert_eq!(
                            o.bm.lowest_set_from(from),
                            expect,
                            "nbits={nbits} from={from}"
                        );
                    }
                }
                assert_eq!(o.bm.is_set(bit), model[bit]);
                assert_eq!(
                    o.bm.lowest_set(),
                    model.iter().position(|b| *b),
                    "nbits={nbits}"
                );
                assert_eq!(o.bm.is_empty(), !model.iter().any(|b| *b));
                // The used prefix bound is tight: nothing above it is set.
                if let Some(hi) = model.iter().rposition(|b| *b) {
                    assert_eq!(o.bm.l1_used, hi / 4096 + 1);
                } else {
                    assert_eq!(o.bm.l1_used, 0);
                }
            }
            for b in 0..nbits {
                assert_eq!(o.bm.is_set(b), model[b]);
            }
        }
    }

    #[test]
    fn two_level_ranges_match_model() {
        let mut s = 0xDEAD_BEEF_CAFE_F00Du64;
        for &nbits in &[1usize, 63, 64, 65, 130, 4096, 4097, 20_000] {
            let mut o = owned(nbits);
            let mut model = vec![false; nbits];
            for _ in 0..3000 {
                let r = xorshift(&mut s);
                let start = (xorshift(&mut s) as usize) % (nbits + 1);
                let len = (xorshift(&mut s) as usize) % (nbits - start + 1);
                if r % 2 == 0 {
                    o.bm.set_range(start, len);
                    for b in &mut model[start..start + len] {
                        *b = true;
                    }
                } else {
                    o.bm.clear_range(start, len);
                    for b in &mut model[start..start + len] {
                        *b = false;
                    }
                }
                assert_eq!(
                    o.bm.lowest_set(),
                    model.iter().position(|b| *b),
                    "nbits={nbits}"
                );
                let from = (xorshift(&mut s) as usize) % (nbits + 1);
                let expect = model.iter().skip(from).position(|b| *b).map(|p| p + from);
                assert_eq!(o.bm.lowest_set_from(from), expect);
                if let Some(hi) = model.iter().rposition(|b| *b) {
                    assert_eq!(o.bm.l1_used, hi / 4096 + 1);
                } else {
                    assert_eq!(o.bm.l1_used, 0);
                }
                for b in 0..nbits {
                    assert_eq!(o.bm.is_set(b), model[b]);
                }
            }
        }
    }

    #[test]
    #[should_panic(expected = "release outside the slab")]
    fn a_release_outside_the_sub_word_view_aborts() {
        let word = AtomicU64::new(0);
        let v = AtomicBitmap::sub_word(&word, 8, 8);
        v.init(6);
        // Slab coordinates: bit 8 of an 8-bit view is the next slab's.
        v.release(0, 1 << 8);
    }

    #[test]
    #[should_panic(expected = "padding bit of the slab was cleared")]
    fn a_cleared_padding_bit_aborts_the_allocated_count() {
        let word = AtomicU64::new(0);
        let v = AtomicBitmap::sub_word(&word, 0, 8);
        v.init(6);
        assert_eq!(v.allocated(6), 0);
        // Bits 6 and 7 are padding and set by `init`; clearing one is a
        // release outside the real slots.
        v.release(0, 1 << 6);
        v.allocated(6);
    }
}
