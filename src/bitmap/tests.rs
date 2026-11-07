use super::*;
use std::vec;
use std::vec::Vec;

use crate::testutil::xorshift;

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
        let bm = AtomicBitmap::from_words_t(&words);
        bm.init_t(nbits);
        for b in 0..nbits {
            assert!(!bm.is_set(b), "nbits={nbits} bit {b} should be clear");
        }
        for b in nbits..words.len() * 64 {
            assert!(bm.is_set(b), "nbits={nbits} padding bit {b} should be set");
        }
        assert!(!bm.is_full());
    }
}

#[test]
#[should_panic(expected = "nbits does not match the view")]
fn init_with_more_bits_than_the_view_aborts() {
    let words = atomic_words(1);
    AtomicBitmap::from_words_t(&words).init_t(65);
}

#[test]
#[should_panic(expected = "nbits does not match the view")]
fn init_of_a_sub_word_view_past_its_share_aborts() {
    let word = AtomicU64::new(0);
    AtomicBitmap::sub_word_t(&word, 8, 4).init_t(5);
}

#[test]
fn claim_takes_lowest_bits_first_and_never_padding() {
    let nbits = 130;
    let words = atomic_words(3);
    let bm = AtomicBitmap::from_words_t(&words);
    bm.init_t(nbits);
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
    let (prev, was_full) = bm.release_t(2, 0b10);
    assert_eq!(prev & 0b10, 0b10);
    assert!(was_full);
    assert!(!bm.is_full());
    assert!(!bm.release_t(2, 0b01).1);
    assert_eq!(bm.claim(1), Some((2, 0b01)));
    assert_eq!(bm.claim(5), Some((2, 0b10)));
}

#[test]
fn claim_from_wraps() {
    let words = atomic_words(4);
    let bm = AtomicBitmap::from_words_t(&words);
    bm.init_t(256);
    assert_eq!(bm.claim_from_t(2, 1), Some((2, 1)));
    assert_eq!(bm.claim_from_t(3, 64), Some((3, !0)));
    assert_eq!(bm.claim_from_t(3, 64), Some((0, !0)));
    assert_eq!(bm.claim_from_t(3, 64), Some((1, !0)));
    assert_eq!(bm.claim_from_t(3, 64), Some((2, !0 << 1)));
    assert_eq!(bm.claim_from_t(3, 64), None);
}

#[test]
fn release_reports_double_release() {
    let words = atomic_words(1);
    let bm = AtomicBitmap::from_words_t(&words);
    bm.init_t(64);
    bm.claim(3);
    let (prev, _) = bm.release_t(0, 0b101);
    assert_eq!(prev & 0b101, 0b101);
    let (prev, _) = bm.release_t(0, 0b100);
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
        let bm = AtomicBitmap::from_words_t(&words);
        bm.init_t(nbits);
        let mut model = vec![false; nbits];
        let mut owned: Vec<usize> = Vec::new();
        for _ in 0..20_000 {
            match xorshift(&mut s) % 4 {
                0 | 1 => {
                    let n = (xorshift(&mut s) % 70) as usize;
                    let start = (xorshift(&mut s) as usize) % words.len();
                    let free_before = model.iter().filter(|b| !**b).count();
                    match bm.claim_from_t(start, n) {
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
                        let (prev, _) = bm.release_t(w, m);
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
            .map(|k| AtomicBitmap::sub_word_t(&word, (k * nbits) as u32, nbits))
            .collect();
        for v in &views {
            v.init_t(nslots);
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
        let (prev, was_full) = v.release_t(0, 0b1);
        assert_eq!(prev, (1u64 << nbits) - 1);
        assert!(was_full);
        assert!(!v.is_set(0) && v.is_set(1));
        let (prev, was_full) = v.release_t(0, 0b10);
        assert!(!was_full);
        assert_eq!(prev & 0b10, 0b10);
        assert_eq!(
            v.release_t(0, 0b10).0 & 0b10,
            0,
            "double release must be detectable"
        );
        // Re-init of slab 1 leaves the others alone and clears it.
        views[0].claim(3);
        v.init_t(nslots);
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
        let bm = AtomicBitmap::sub_word_t(&word, shift, nbits);
        let nslots = nbits - 1;
        bm.init_t(nslots);
        // A neighbour claims some bits of the same word.
        let other_shift = (shift + nbits as u32) % 64;
        let other = AtomicBitmap::sub_word_t(&word, other_shift, nbits);
        other.init_t(nslots);
        other.claim(2);
        let before = word.load(Ordering::Relaxed) & !bm.mask;
        let mut model = vec![false; nslots];
        for _ in 0..5000 {
            if xorshift(&mut s).is_multiple_of(2) {
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
                let (prev, was_full) = bm.release_t(0, 1 << b);
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
    AtomicBitmap::from_words_t(&words).init_t(NBITS);
    let stamps: Arc<Vec<AtomicU8>> = Arc::new((0..NBITS).map(|_| AtomicU8::new(0)).collect());
    let total = Arc::new(AtomicU64::new(0));

    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let words = Arc::clone(&words);
            let stamps = Arc::clone(&stamps);
            let total = Arc::clone(&total);
            thread::spawn(move || {
                let id = t as u8 + 1;
                let bm = AtomicBitmap::from_words_t(&words);
                let mut s =
                    0x1234_5678_9ABC_DEF0u64 ^ (t as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                let mut owned: Vec<usize> = Vec::new();
                let mut claimed = 0u64;
                for _ in 0..ROUNDS {
                    if xorshift(&mut s).is_multiple_of(2) || owned.is_empty() {
                        let n = 1 + (xorshift(&mut s) % 40) as usize;
                        let start = (xorshift(&mut s) as usize) % words.len();
                        if let Some((w, mut m)) = bm.claim_from_t(start, n) {
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
                            let (prev, _) = bm.release_t(bit / 64, 1 << (bit % 64));
                            assert_ne!(prev & (1 << (bit % 64)), 0);
                        }
                    }
                }
                for bit in owned {
                    assert_eq!(stamps[bit].swap(0, Ordering::AcqRel), id);
                    bm.release_t(bit / 64, 1 << (bit % 64));
                }
                total.fetch_add(claimed, Ordering::Relaxed);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let bm = AtomicBitmap::from_words_t(&words);
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
    let bm =
        unsafe { TwoLevelBitmap::from_raw(nbits, storage.as_mut_ptr(), Tracked::assume_new()) };
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
// Indexes the views, the models and the sizes together.
#[allow(clippy::needless_range_loop)]
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
        .map(|(v, &n)| unsafe {
            TwoLevelBitmap::from_raw_chunked(n, base.add(v), stride, Tracked::assume_new())
        })
        .collect();
    let mut models: Vec<Vec<bool>> = sizes.iter().map(|&n| vec![false; n]).collect();
    let mut dense: Vec<Owned> = sizes.iter().map(|&n| owned(n)).collect();
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..60_000 {
        let r = xorshift(&mut s);
        let v = (r % stride as u64) as usize;
        let nbits = sizes[v];
        let bit = if r.is_multiple_of(8) {
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
// Compares the bitmap with the model bit by bit.
#[allow(clippy::needless_range_loop)]
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
            let bit = if r.is_multiple_of(8) {
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
// Compares the bitmap with the model bit by bit.
#[allow(clippy::needless_range_loop)]
fn two_level_ranges_match_model() {
    let mut s = 0xDEAD_BEEF_CAFE_F00Du64;
    for &nbits in &[1usize, 63, 64, 65, 130, 4096, 4097, 20_000] {
        let mut o = owned(nbits);
        let mut model = vec![false; nbits];
        for _ in 0..3000 {
            let r = xorshift(&mut s);
            let start = (xorshift(&mut s) as usize) % (nbits + 1);
            let len = (xorshift(&mut s) as usize) % (nbits - start + 1);
            if r.is_multiple_of(2) {
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
    let v = AtomicBitmap::sub_word_t(&word, 8, 8);
    v.init_t(6);
    // Slab coordinates: bit 8 of an 8-bit view is the next slab's.
    v.release_t(0, 1 << 8);
}

#[test]
#[should_panic(expected = "padding bit of the slab was cleared")]
fn a_cleared_padding_bit_aborts_the_allocated_count() {
    let word = AtomicU64::new(0);
    let v = AtomicBitmap::sub_word_t(&word, 0, 8);
    v.init_t(6);
    assert_eq!(v.allocated(6), 0);
    // Bits 6 and 7 are padding and set by `init`; clearing one is a
    // release outside the real slots.
    v.release_t(0, 1 << 6);
    v.allocated(6);
}

/// Weak-memory stress of the claim and release edge (ignored;
/// `CHALLOC_STRESS_SECS`). The CPUs share one word of 4 slots, each
/// with 4 plain words of data. A claimer finds its slot's data 0
/// (what the last owner wrote before `release`), writes its own token
/// to all four words, checks them, writes 0 and releases. A claimer
/// that reads a token, or a holder whose token changed, saw a release
/// not ordered before the next claim: `release`'s `fetch_and(Release)`
/// to `claim_word`'s `fetch_or(Acquire)` (WEAKMEM.md, P1).
#[test]
#[ignore]
fn stress_bitmap_claim_release_edge() {
    use core::cell::UnsafeCell;
    use core::sync::atomic::AtomicBool;
    const SLOTS: usize = 4;
    struct Data([UnsafeCell<[u64; 4]>; SLOTS]);
    // SAFETY: each slot's data is touched only by the thread holding
    // the slot's bit, which is the property under test.
    unsafe impl Sync for Data {}
    let word = [AtomicU64::new(0)];
    let bm = AtomicBitmap::from_words_t(&word);
    bm.init_t(SLOTS);
    let data = Data(core::array::from_fn(|_| UnsafeCell::new([0; 4])));
    let threads = crate::testutil::stress_threads();
    let stop = AtomicBool::new(false);
    let started = std::time::Instant::now();
    // Per thread: (claims, stale data seen on claim, token changed
    // while held, the claim count at the first anomaly).
    let res: Vec<(u64, u64, u64, u64)> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|t| {
                let (bm, data, stop) = (&bm, &data, &stop);
                s.spawn(move || {
                    let (mut n, mut stale, mut changed, mut first) = (0u64, 0u64, 0u64, u64::MAX);
                    while !stop.load(Ordering::Relaxed) {
                        let Some((i, m)) = bm.claim(1) else {
                            core::hint::spin_loop();
                            continue;
                        };
                        assert_eq!(i, 0);
                        let p = data.0[m.trailing_zeros() as usize].get().cast::<u64>();
                        let token = ((t as u64 + 1) << 40) | (n + 1);
                        // SAFETY: the claimed bit makes this thread the
                        // slot's one holder; the four words are in `data`.
                        unsafe {
                            if (0..4).any(|k| p.add(k).read_volatile() != 0) {
                                stale += 1;
                                first = first.min(n);
                            }
                            for k in 0..4 {
                                p.add(k).write_volatile(token);
                            }
                            if (0..4).any(|k| p.add(k).read_volatile() != token) {
                                changed += 1;
                                first = first.min(n);
                            }
                            for k in 0..4 {
                                p.add(k).write_volatile(0);
                            }
                        }
                        let (prev, _) = bm.release_t(0, m);
                        assert_eq!(prev & m, m, "double release");
                        n += 1;
                    }
                    (n, stale, changed, first)
                })
            })
            .collect();
        std::thread::sleep(crate::testutil::stress_duration());
        stop.store(true, Ordering::Relaxed);
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let claims: u64 = res.iter().map(|r| r.0).sum();
    let stale: u64 = res.iter().map(|r| r.1).sum();
    let changed: u64 = res.iter().map(|r| r.2).sum();
    let first = res.iter().map(|r| r.3).min().unwrap_or(u64::MAX);
    crate::testutil::stress_report(
        "bitmap_claim_release_edge",
        &std::format!(
            "threads={threads} claims={claims} secs={:.1} stale={stale} changed={changed} first_anomaly_thread_claim={}",
            started.elapsed().as_secs_f64(),
            first as i64,
        ),
    );
    assert_eq!(
        (stale, changed),
        (0, 0),
        "a claim saw another holder's data"
    );
    assert_eq!(word[0].load(Ordering::Relaxed), !0u64 << SLOTS);
}
