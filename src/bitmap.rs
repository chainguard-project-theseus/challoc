//! Bitmaps (design.md sections 4, 5 and 7).
//!
//! Two structures live here and both are pure logic, unit tested on the host.
//!
//! [`AtomicBitmap`] is the per-slab slot bitmap. It is a view over the
//! `[AtomicU64]` words stored inline in a slab descriptor, has no owner, and
//! is only ever touched with atomic read-modify-write operations. A set bit
//! means "not available to a refill": the slot is live in the program or
//! sitting in a thread cache. Padding bits past the last real slot are kept
//! permanently set so that they can never be claimed and so that a full
//! bitmap is simply one whose words are all ones.
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

/// Atomic slot bitmap over a slice of words (design section 4).
///
/// The view is `repr(transparent)` over `[AtomicU64]` so a descriptor's
/// inline array can be reinterpreted without copying.
#[repr(transparent)]
pub struct AtomicBitmap {
    words: [AtomicU64],
}

impl AtomicBitmap {
    /// View `words` as a bitmap. Nothing is initialised; call [`init`] before
    /// the first claim on a fresh descriptor.
    ///
    /// [`init`]: AtomicBitmap::init
    #[inline]
    pub fn from_words(words: &[AtomicU64]) -> &AtomicBitmap {
        // SAFETY: `AtomicBitmap` is `repr(transparent)` over `[AtomicU64]`,
        // so the pointer metadata and layout are identical.
        unsafe { &*(words as *const [AtomicU64] as *const AtomicBitmap) }
    }

    /// Number of 64-bit words in the view.
    #[inline]
    pub fn words(&self) -> usize {
        self.words.len()
    }

    /// Clear every bit for slots `0..nbits` and set the padding bits above
    /// `nbits` in the last word so they can never be claimed. `nbits` must
    /// fit the view and use its last word.
    pub fn init(&self, nbits: usize) {
        let n = self.words.len();
        assert!(nbits <= n * 64 && nbits > (n - 1) * 64, "bitmap: nbits does not match word count");
        for w in &self.words[..n - 1] {
            w.store(0, Ordering::Relaxed);
        }
        let tail = nbits % 64;
        let pad = if tail == 0 { 0 } else { !0u64 << tail };
        self.words[n - 1].store(pad, Ordering::Relaxed);
    }

    /// Claim up to `n` clear bits from word `i`. Returns the mask of bits this
    /// call actually claimed, which may be zero if the word was full or a
    /// concurrent claimer won every bit we wanted.
    #[inline]
    pub fn claim_word(&self, i: usize, n: usize) -> u64 {
        let w = &self.words[i];
        let cur = w.load(Ordering::Relaxed);
        let want = lowest_n_bits(!cur, n);
        if want == 0 {
            return 0;
        }
        // Acquire pairs with the Release in `release`: the new owner's
        // accesses to the slot happen after the previous owner's.
        let prev = w.fetch_or(want, Ordering::Acquire);
        want & !prev
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

    /// Release the bits in `mask` of word `i`. Returns the word's previous
    /// value; the caller must abort if `prev & mask != mask`, because a bit
    /// that was already clear means a slot was released twice.
    #[inline]
    pub fn release(&self, i: usize, mask: u64) -> u64 {
        self.words[i].fetch_and(!mask, Ordering::Release)
    }

    /// Whether `bit` is set. This is the `free` check: a clear bit means an
    /// invalid or double free. Never writes.
    #[inline]
    pub fn is_set(&self, bit: usize) -> bool {
        (self.words[bit / 64].load(Ordering::Relaxed) >> (bit % 64)) & 1 == 1
    }

    /// Current value of word `i`.
    #[inline]
    pub fn load(&self, i: usize) -> u64 {
        self.words[i].load(Ordering::Relaxed)
    }

    /// Whether every bit, padding included, is set. A full slab leaves the
    /// partial set (design section 5).
    pub fn is_full(&self) -> bool {
        self.words.iter().all(|w| w.load(Ordering::Relaxed) == !0)
    }
}

/// Ordered set of small integers with a one-word-per-64 summary level
/// (design section 5). Not thread safe; callers hold a lock.
///
/// Storage is caller-provided: [`words_needed`] words of zeroed `u64`, level
/// 1 first then level 0, so that it can be carved out of the metadata region.
///
/// [`words_needed`]: TwoLevelBitmap::words_needed
pub struct TwoLevelBitmap {
    nbits: usize,
    l0_words: usize,
    l1_words: usize,
    /// Number of leading level-1 words that may be non-zero. Everything at
    /// or above this index is zero. Grows on `set`, shrinks on `clear`.
    l1_used: usize,
    /// `l1_words` summary words followed by `l0_words` bit words.
    ptr: *mut u64,
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
        TwoLevelBitmap { nbits, l0_words, l1_words: l0_words.div_ceil(64), l1_used: 0, ptr }
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
        // SAFETY: `i < l1_words`, inside the storage promised to `from_raw`.
        unsafe { *self.ptr.add(i) }
    }

    #[inline]
    fn l0(&self, i: usize) -> u64 {
        debug_assert!(i < self.l0_words);
        // SAFETY: `l1_words + i < l1_words + l0_words`, inside the storage.
        unsafe { *self.ptr.add(self.l1_words + i) }
    }

    #[inline]
    fn l1_mut(&mut self, i: usize) -> &mut u64 {
        debug_assert!(i < self.l1_words);
        // SAFETY: as in `l1`; `&mut self` gives exclusive access.
        unsafe { &mut *self.ptr.add(i) }
    }

    #[inline]
    fn l0_mut(&mut self, i: usize) -> &mut u64 {
        debug_assert!(i < self.l0_words);
        // SAFETY: as in `l0`; `&mut self` gives exclusive access.
        unsafe { &mut *self.ptr.add(self.l1_words + i) }
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

    /// Lowest bit in the set.
    #[inline]
    pub fn lowest_set(&self) -> Option<usize> {
        self.scan_l1(0)
    }

    /// Lowest bit in the set that is `>= from`.
    pub fn lowest_set_from(&self, from: usize) -> Option<usize> {
        if from >= self.nbits {
            return None;
        }
        let w = from / 64;
        let m = self.l0(w) & (!0u64 << (from % 64));
        if m != 0 {
            return Some(w * 64 + m.trailing_zeros() as usize);
        }
        // Rest of the level-1 word that covers `w`, excluding `w` itself.
        let j = w / 64;
        if j >= self.l1_used {
            return None;
        }
        let shift = w % 64 + 1;
        let m1 = if shift == 64 { 0 } else { self.l1(j) & (!0u64 << shift) };
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
        let prev = bm.release(2, 0b10);
        assert_eq!(prev & 0b10, 0b10);
        assert!(!bm.is_full());
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
        let prev = bm.release(0, 0b101);
        assert_eq!(prev & 0b101, 0b101);
        let prev = bm.release(0, 0b100);
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
                            let prev = bm.release(w, m);
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
                                let prev = bm.release(bit / 64, 1 << (bit % 64));
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
}
