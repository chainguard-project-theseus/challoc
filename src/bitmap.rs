//! Bitmaps (design.md sections 4, 5 and 7).
//!
//! Two structures live here and both are pure logic, unit tested on the host.
//!
//! [`AtomicBitmap`] is one slab's view of its class's slot bitmap, a
//! contiguous `[AtomicU64]` array in the metadata region (`class.rs`). It
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

/// Keep only the lowest `n` set bits of `m`.
#[inline]
pub const fn lowest_n_bits(mut m: u64, n: usize) -> u64 {
    if n >= 64 {
        return m;
    }
    let mut have = m.count_ones() as usize;
    while have > n {
        // `have > n >= 0` so `m` is non-zero and has a highest set bit.
        m &= !(1u64 << (63 - m.leading_zeros()));
        have -= 1;
    }
    m
}

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
        AtomicBitmap { words, shift: 0, mask: !0 }
    }

    /// View bits `shift .. shift + nbits` of `word` as the bitmap of a slab
    /// whose bits share that word with other slabs. `nbits` is the slab's
    /// padded bit count (`Class::slab_bits`), below 64.
    #[inline]
    pub fn sub_word(word: &'a AtomicU64, shift: u32, nbits: usize) -> AtomicBitmap<'a> {
        debug_assert!(nbits > 0 && nbits < 64 && shift as usize + nbits <= 64);
        AtomicBitmap { words: core::slice::from_ref(word), shift, mask: ((1u64 << nbits) - 1) << shift }
    }

    /// Number of words in the view (1 for a sub-word slab).
    #[inline]
    pub fn words(&self) -> usize {
        self.words.len()
    }

    /// Bits the slab owns, padding included.
    #[inline]
    pub fn nbits(&self) -> usize {
        if self.mask == !0 { self.words.len() * 64 } else { self.mask.count_ones() as usize }
    }

    /// Clear every bit for slots `0..nbits` and set the padding bits above
    /// `nbits` (up to the view's `slab_bits`) so they can never be claimed.
    /// `nbits` must be non-zero and fit the view. Atomic on a shared word,
    /// so a neighbouring slab that is already active is not disturbed.
    pub fn init(&self, nbits: usize) {
        assert!(nbits > 0 && nbits <= self.nbits(), "bitmap: nbits does not match the view");
        if self.mask == !0 {
            for (i, w) in self.words.iter().enumerate() {
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
            let pad = self.mask & !(((1u64 << nbits) - 1) << self.shift);
            self.words[0].fetch_and(!self.mask, Ordering::Relaxed);
            if pad != 0 {
                self.words[0].fetch_or(pad, Ordering::Relaxed);
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

    /// Claim up to `n` bits from the lowest word that has a clear bit. Returns
    /// the word index and the claimed mask, or `None` if every word was full
    /// at the time it was inspected. A single call never spans words; callers
    /// loop until they have enough or get `None`.
    #[inline]
    pub fn claim(&self, n: usize) -> Option<(usize, u64)> {
        self.claim_from(0, n)
    }

    /// Like [`claim`] but starts the search at word `start`, wrapping around,
    /// for the `slot_randomize` tunable.
    ///
    /// [`claim`]: AtomicBitmap::claim
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
        debug_assert!(m & !self.mask == 0, "bitmap: release outside the slab");
        let prev = self.words[i].fetch_and(!m, Ordering::Release);
        ((prev & self.mask) >> self.shift, prev & self.mask == self.mask)
    }

    /// Whether slot `bit` is set. This is the `free` check: a clear bit
    /// means an invalid or double free. Never writes.
    #[inline]
    pub fn is_set(&self, bit: usize) -> bool {
        let b = bit + self.shift as usize;
        (self.words[b / 64].load(Ordering::Relaxed) >> (b % 64)) & 1 == 1
    }

    /// The slab's bits in word `i` (slab coordinates, padding included).
    #[inline]
    pub fn load(&self, i: usize) -> u64 {
        (self.words[i].load(Ordering::Relaxed) & self.mask) >> self.shift
    }

    /// Set slot bits, padding excluded: the slab's allocated (or cached)
    /// slots for a class with `nslots` real slots.
    pub fn allocated(&self, nslots: usize) -> usize {
        let set: usize = (0..self.words.len()).map(|w| self.load(w).count_ones() as usize).sum();
        set - (self.nbits() - nslots)
    }

    /// Whether every bit, padding included, is set. A full slab leaves the
    /// partial set (design section 5).
    pub fn is_full(&self) -> bool {
        self.words.iter().all(|w| w.load(Ordering::Relaxed) & self.mask == self.mask)
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
}

impl TwoLevelBitmap {
    /// Words of storage needed for a set over `0..nbits`.
    pub const fn words_needed(nbits: usize) -> usize {
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
    pub const unsafe fn from_raw(nbits: usize, ptr: *mut u64) -> TwoLevelBitmap {
        let l0_words = nbits.div_ceil(64);
        TwoLevelBitmap { nbits, l0_words, l1_words: l0_words.div_ceil(64), l1_used: 0, ptr, stride: 1, chunked: false }
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
    pub const unsafe fn from_raw_chunked(nbits: usize, ptr: *mut u64, stride: usize) -> TwoLevelBitmap {
        let l0_words = nbits.div_ceil(64);
        debug_assert!(stride >= 1);
        TwoLevelBitmap { nbits, l0_words, l1_words: l0_words.div_ceil(64), l1_used: 0, ptr, stride, chunked: true }
    }

    /// Level-1 words a set over `0..nbits` has.
    pub const fn l1_words(nbits: usize) -> usize {
        nbits.div_ceil(64).div_ceil(64)
    }

    /// Rows a chunked view over `0..nbits` spans (65 per level-1 word).
    pub const fn chunked_rows(nbits: usize) -> usize {
        65 * Self::l1_words(nbits)
    }

    /// Row of level-0 word `w` in a chunked view.
    pub const fn chunked_l0_row(w: usize) -> usize {
        65 * (w / 64) + 1 + w % 64
    }

    /// Offset in words of level-1 word `j`.
    #[inline]
    fn l1_off(&self, j: usize) -> usize {
        if self.chunked { 65 * j * self.stride } else { j }
    }

    /// Offset in words of level-0 word `w`.
    #[inline]
    fn l0_off(&self, w: usize) -> usize {
        if self.chunked { Self::chunked_l0_row(w) * self.stride } else { self.l1_words + w }
    }

    /// Number of bits in the set's universe.
    #[inline]
    pub const fn len(&self) -> usize {
        self.nbits
    }

    /// Whether no bit is set.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.l1_used == 0
    }

    #[inline]
    fn l1(&self, i: usize) -> u64 {
        debug_assert!(i < self.l1_words);
        // SAFETY: `i < l1_words`, a word of this view (`from_raw*`).
        unsafe { *self.ptr.add(self.l1_off(i)) }
    }

    #[inline]
    fn l0(&self, i: usize) -> u64 {
        debug_assert!(i < self.l0_words);
        // SAFETY: level-0 word `i` of this view (`from_raw*`).
        unsafe { *self.ptr.add(self.l0_off(i)) }
    }

    #[inline]
    fn l1_mut(&mut self, i: usize) -> &mut u64 {
        debug_assert!(i < self.l1_words);
        // SAFETY: as in `l1`; `&mut self` gives exclusive access.
        unsafe { &mut *self.ptr.add(self.l1_off(i)) }
    }

    #[inline]
    fn l0_mut(&mut self, i: usize) -> &mut u64 {
        debug_assert!(i < self.l0_words);
        // SAFETY: as in `l0`; `&mut self` gives exclusive access.
        unsafe { &mut *self.ptr.add(self.l0_off(i)) }
    }

    /// Whether `bit` is in the set.
    #[inline]
    pub fn is_set(&self, bit: usize) -> bool {
        assert!(bit < self.nbits, "bitmap: bit out of range");
        (self.l0(bit / 64) >> (bit % 64)) & 1 == 1
    }

    /// Insert `bit`.
    pub fn set(&mut self, bit: usize) {
        assert!(bit < self.nbits, "bitmap: bit out of range");
        let w = bit / 64;
        *self.l0_mut(w) |= 1u64 << (bit % 64);
        *self.l1_mut(w / 64) |= 1u64 << (w % 64);
        if w / 64 >= self.l1_used {
            self.l1_used = w / 64 + 1;
        }
    }

    /// Remove `bit`. Removing a bit that is not present is a no-op.
    pub fn clear(&mut self, bit: usize) {
        assert!(bit < self.nbits, "bitmap: bit out of range");
        let w = bit / 64;
        let word = self.l0_mut(w);
        *word &= !(1u64 << (bit % 64));
        if *word == 0 {
            *self.l1_mut(w / 64) &= !(1u64 << (w % 64));
            while self.l1_used > 0 && self.l1(self.l1_used - 1) == 0 {
                self.l1_used -= 1;
            }
        }
    }

    /// Insert every bit in `start..start + len`, one word at a time.
    pub fn set_range(&mut self, start: usize, len: usize) {
        let end = start.checked_add(len).expect("bitmap: range wraps");
        assert!(end <= self.nbits, "bitmap: range out of range");
        let mut bit = start;
        while bit < end {
            let w = bit / 64;
            let lo = bit % 64;
            let hi = (end - w * 64).min(64);
            let mask = if hi - lo == 64 { !0 } else { ((1u64 << (hi - lo)) - 1) << lo };
            *self.l0_mut(w) |= mask;
            *self.l1_mut(w / 64) |= 1u64 << (w % 64);
            if w / 64 >= self.l1_used {
                self.l1_used = w / 64 + 1;
            }
            bit = w * 64 + hi;
        }
    }

    /// Remove every bit in `start..start + len`, one word at a time. Bits
    /// that were not present are ignored.
    pub fn clear_range(&mut self, start: usize, len: usize) {
        let end = start.checked_add(len).expect("bitmap: range wraps");
        assert!(end <= self.nbits, "bitmap: range out of range");
        let mut bit = start;
        while bit < end {
            let w = bit / 64;
            let lo = bit % 64;
            let hi = (end - w * 64).min(64);
            let mask = if hi - lo == 64 { !0 } else { ((1u64 << (hi - lo)) - 1) << lo };
            let word = self.l0_mut(w);
            *word &= !mask;
            if *word == 0 {
                *self.l1_mut(w / 64) &= !(1u64 << (w % 64));
            }
            bit = w * 64 + hi;
        }
        while self.l1_used > 0 && self.l1(self.l1_used - 1) == 0 {
            self.l1_used -= 1;
        }
    }

    /// Lowest bit in the set.
    #[inline]
    pub fn lowest_set(&self) -> Option<usize> {
        self.scan_l1(0)
    }

    /// Lowest bit in the set that is `>= from`.
    ///
    /// Level-0 words are read only when their level-1 bit is set. A set
    /// level-1 bit means the word is non-zero, so it has been written; the
    /// lazily committed tables in `class.rs` and `huge.rs` rely on this to
    /// keep scans away from words whose page is still `PROT_NONE`.
    pub fn lowest_set_from(&self, from: usize) -> Option<usize> {
        if from >= self.nbits {
            return None;
        }
        let w = from / 64;
        let j = w / 64;
        if j >= self.l1_used {
            return None;
        }
        // Non-zero level-0 words at or above `w` in the group covering `w`.
        let mut m1 = self.l1(j) & (!0u64 << (w % 64));
        if m1 != 0 && j * 64 + m1.trailing_zeros() as usize == w {
            let m = self.l0(w) & (!0u64 << (from % 64));
            if m != 0 {
                return Some(w * 64 + m.trailing_zeros() as usize);
            }
            m1 &= m1 - 1;
        }
        if m1 != 0 {
            let w2 = j * 64 + m1.trailing_zeros() as usize;
            return Some(w2 * 64 + self.l0(w2).trailing_zeros() as usize);
        }
        self.scan_l1(j + 1)
    }

    /// Lowest set bit whose level-1 word index is `>= j`.
    fn scan_l1(&self, j: usize) -> Option<usize> {
        for i in j..self.l1_used {
            let m1 = self.l1(i);
            if m1 != 0 {
                let w = i * 64 + m1.trailing_zeros() as usize;
                return Some(w * 64 + self.l0(w).trailing_zeros() as usize);
            }
        }
        None
    }
}

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
            let views: Vec<AtomicBitmap> =
                (0..64 / nbits).map(|k| AtomicBitmap::sub_word(&word, (k * nbits) as u32, nbits)).collect();
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
            assert_eq!(v.release(0, 0b10).0 & 0b10, 0, "double release must be detectable");
            // Re-init of slab 1 leaves the others alone and clears it.
            views[0].claim(3);
            v.init(nslots);
            assert_eq!(v.allocated(nslots), 0);
            assert_eq!(views[0].allocated(nslots), 3);
            let whole = word.load(Ordering::Relaxed);
            for (k, o) in views.iter().enumerate() {
                assert_eq!((whole >> (k * nbits)) & ((1u64 << nbits) - 1), o.load(0), "slab {k}");
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
                assert_eq!(word.load(Ordering::Relaxed) & !bm.mask, before, "neighbour bits changed");
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
                    let mut s = 0x1234_5678_9ABC_DEF0u64 ^ (t as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
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
        Owned { _storage: storage, bm }
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
        assert_eq!(bm.l1_used, 1, "used prefix shrinks when the top word empties");
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
    /// set table of `class.rs`: row `r` of view `v` at `r * stride + v`),
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
                    let expect = models[v].iter().skip(from).position(|b| *b).map(|p| p + from);
                    assert_eq!(views[v].lowest_set_from(from), expect, "view {v} from={from}");
                    assert_eq!(dense[v].bm.lowest_set_from(from), expect);
                }
            }
            assert_eq!(views[v].is_set(bit), models[v][bit]);
            assert_eq!(views[v].lowest_set(), models[v].iter().position(|b| *b), "view {v}");
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
            assert_ne!(storage[stride + v] & 1, 0, "view {v} level-0 word 0 in row 1");
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
                        assert_eq!(o.bm.lowest_set_from(from), expect, "nbits={nbits} from={from}");
                    }
                }
                assert_eq!(o.bm.is_set(bit), model[bit]);
                assert_eq!(o.bm.lowest_set(), model.iter().position(|b| *b), "nbits={nbits}");
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
                assert_eq!(o.bm.lowest_set(), model.iter().position(|b| *b), "nbits={nbits}");
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
}
