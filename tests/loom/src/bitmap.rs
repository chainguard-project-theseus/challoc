//! Slab bitmap claim and release with the `live` reservation.
//!
//! [`AtomicBitmap`] is a copy of `src/bitmap.rs`'s steps for a slab whose
//! bitmap is whole words (`mask == !0`, the only shape a model here
//! uses), over loom's atomics, one word for a slab of [`NSLOTS`] slots:
//!
//! - [`AtomicBitmap::init`] is `AtomicBitmap::init`'s whole-word branch:
//!   each word's padding (the bits at and above `nbits`) stored `Relaxed`
//!   (`vspec::word::store`);
//! - [`AtomicBitmap::claim_word`] is `AtomicBitmap::claim_word`: a
//!   `Relaxed` load of the word as a hint, the lowest `n` clear bits
//!   (`lowest_n_bits`), then `vspec::word::fetch_or` of them `Acquire`,
//!   which wins the bits that were clear;
//! - [`AtomicBitmap::claim_from`] is `AtomicBitmap::claim_from`: the first
//!   word, from `start` round, whose claim wins a bit;
//! - [`AtomicBitmap::release`] is `AtomicBitmap::release`:
//!   `vspec::word::fetch_and` of the complement `Release`.
//!
//! Around it, copies of the class steps that drive it, with the
//! orderings of the code they cite:
//!
//! - `live` is `SlabDesc::live` (`src/slab.rs`): `live()` loads
//!   `Acquire`, `add_live` and `sub_live` are `AcqRel`
//!   read-modify-writes;
//! - a reservation is `ClassState::take` (`src/class/mod.rs`, from
//!   `let live = desc.live()` to `desc.add_live(n)`): under the class
//!   lock (the copy of `RawLock`), `avail = nslots - live`, reserve
//!   `min(avail, want)`;
//! - the claim loop is the bitmap pass of `src/class/mod.rs` (`claim_from`
//!   until `want` slots are held, an empty pass counted towards
//!   `MAX_EMPTY_PASSES` and the `reservation_unmet` abort; the model yields
//!   after an empty pass, which the source does not, so loom can run the
//!   thread that holds the slot);
//! - a release is `release` then `dec_live` (`sub_live`), the order the
//!   free path keeps (`src/bitmap.rs` `release`, `src/class/mod.rs`
//!   `dec_live`).
//!
//! Checked: each slot's data is a loom `UnsafeCell` written by every
//! holder, so two holders at once, or a holder that does not see the
//! previous holder's writes, is a reported race; claimed masks are
//! disjoint and never include padding; a release never finds its bit
//! clear; and a reservation is met within one pass per concurrent
//! claimer, so the `Relaxed` hint load never hides a slot that `live`
//! promised (the abort's bound, 2^22 passes, is far above it).
//!
//! Left out: sub-word bitmaps (`mask != !0`, several slabs in one word),
//! whose `init` is a `fetch_and` and a `fetch_or`; the ghost tokens.

use crate::lock::RawLock;
use crate::{leak, model};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use loom::thread;

const NSLOTS: usize = 2;
const SLAB_MASK: u64 = (1 << NSLOTS) - 1;

/// `bitmap::lowest_n_bits`: the lowest `n` set bits of `m` (all of them
/// when `m` has at most `n`).
fn lowest_n_bits(m: u64, n: usize) -> u64 {
    let (mut m, mut r) = (m, 0);
    for _ in 0..n {
        if m == 0 {
            break;
        }
        let b = m & m.wrapping_neg();
        r |= b;
        m &= !b;
    }
    r
}

/// `bitmap::AtomicBitmap` over whole words.
pub(crate) struct AtomicBitmap<'a> {
    words: &'a [AtomicU64],
}

impl<'a> AtomicBitmap<'a> {
    pub(crate) fn from_words(words: &'a [AtomicU64]) -> AtomicBitmap<'a> {
        AtomicBitmap { words }
    }

    /// `AtomicBitmap::init`, whole words: slots `0..nbits` clear, the
    /// padding set.
    pub(crate) fn init(&self, nbits: usize) {
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
    }

    /// `AtomicBitmap::claim_word`: the bits of word `i` this call won.
    fn claim_word(&self, i: usize, n: usize) -> u64 {
        let w = &self.words[i];
        let cur = w.load(Ordering::Relaxed);
        let want = lowest_n_bits(!cur, n);
        if want == 0 {
            return 0;
        }
        let prev = w.fetch_or(want, Ordering::Acquire);
        want & !prev
    }

    /// `AtomicBitmap::claim_from`.
    pub(crate) fn claim_from(&self, start: usize, n: usize) -> Option<(usize, u64)> {
        let len = self.words.len();
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

    /// `AtomicBitmap::claim` (test code): `claim_from(0, n)`.
    pub(crate) fn claim(&self, n: usize) -> Option<(usize, u64)> {
        self.claim_from(0, n)
    }

    /// `AtomicBitmap::release`: the word's previous value.
    pub(crate) fn release(&self, i: usize, mask: u64) -> u64 {
        self.words[i].fetch_and(!mask, Ordering::Release)
    }
}

/// One slab: its bitmap word, `live`, the class lock and the slot data.
struct Slab {
    words: [AtomicU64; 1],
    live: AtomicU32,
    lock: RawLock,
    data: [UnsafeCell<u64>; NSLOTS],
}

// SAFETY: `data[i]` is touched only by the holder of slot `i`, which is
// what is checked.
unsafe impl Sync for Slab {}

impl Slab {
    fn new() -> Slab {
        Slab {
            words: [AtomicU64::new(0)],
            live: AtomicU32::new(0),
            lock: RawLock::new(),
            data: [UnsafeCell::new(0), UnsafeCell::new(0)],
        }
    }

    fn bitmap(&self) -> AtomicBitmap<'_> {
        AtomicBitmap::from_words(&self.words)
    }

    /// `ClassState::take`'s reservation: `n` of `want` slots.
    fn reserve(&self, want: usize) -> usize {
        self.lock.lock();
        let live = self.live.load(Ordering::Acquire) as usize;
        assert!(live <= NSLOTS, "live above nslots");
        let n = (NSLOTS - live).min(want);
        self.live.fetch_add(n as u32, Ordering::AcqRel);
        self.lock.unlock();
        n
    }

    /// The claim loop for a reservation of `want`, allowing `passes`
    /// empty passes. Returns the mask claimed.
    fn claim(&self, want: usize, passes: u32) -> u64 {
        let bm = self.bitmap();
        let mut got = 0u64;
        let mut empty = 0u32;
        while (got.count_ones() as usize) < want {
            match bm.claim_from(0, want - got.count_ones() as usize) {
                Some((w, bits)) => {
                    assert_eq!(w, 0);
                    assert_eq!(got & bits, 0, "a slot claimed twice by one claimer");
                    got |= bits;
                }
                None => {
                    empty += 1;
                    assert!(
                        empty < passes,
                        "reservation unmet: {} of {want} after {empty} empty passes",
                        got.count_ones()
                    );
                    thread::yield_now();
                }
            }
        }
        assert_eq!(got & !SLAB_MASK, 0, "claimed padding: {got:#x}");
        got
    }

    /// The holder's writes to its slots.
    fn write(&self, mask: u64, v: u64) {
        for (i, d) in self.data.iter().enumerate() {
            if mask & (1 << i) != 0 {
                // SAFETY: this thread holds slot `i` (loom reports a race if not).
                d.with_mut(|p| unsafe { *p = v });
            }
        }
    }

    /// `release`, then `dec_live`.
    fn release(&self, mask: u64) {
        let prev = self.bitmap().release(0, mask);
        assert_eq!(prev & mask, mask, "released a clear bit");
        let n = mask.count_ones();
        let prev = self.live.fetch_sub(n, Ordering::AcqRel);
        assert!(prev >= n, "live underflow");
    }

    /// A mutant of [`Slab::release`] for the controls: the bitmap's
    /// `fetch_and` `Relaxed` instead of `release`'s `Release`.
    fn release_relaxed(&self, mask: u64) {
        let prev = self.words[0].fetch_and(!mask, Ordering::Relaxed);
        assert_eq!(prev & mask, mask, "released a clear bit");
        let n = mask.count_ones();
        let prev = self.live.fetch_sub(n, Ordering::AcqRel);
        assert!(prev >= n, "live underflow");
    }

    /// A mutant of [`Slab::claim`] of one slot for the controls: the
    /// lowest clear bit taken by a `Relaxed` `fetch_or` instead of
    /// `claim_word`'s `Acquire`.
    fn claim_relaxed(&self, passes: u32) -> u64 {
        for _ in 0..passes {
            let free = !self.words[0].load(Ordering::Relaxed) & SLAB_MASK;
            let bit = free & free.wrapping_neg();
            if bit != 0 && self.words[0].fetch_or(bit, Ordering::Relaxed) & bit == 0 {
                return bit;
            }
            thread::yield_now();
        }
        panic!("reservation unmet");
    }
}

/// A slab whose slots are all held by the main thread before any other
/// thread starts.
fn full_slab() -> &'static Slab {
    let s = leak(Slab::new());
    s.bitmap().init(NSLOTS);
    assert_eq!(s.reserve(NSLOTS), NSLOTS);
    let m = s.claim(NSLOTS, 1);
    assert_eq!(m, SLAB_MASK);
    s.write(m, 1);
    s
}

/// Which step of [`release_then_reserve`] is a mutant.
#[derive(Clone, Copy, PartialEq)]
enum Mutant {
    None,
    RelaxedRelease,
    RelaxedClaim,
}

/// Two owners release one slot each while a third thread reserves one
/// and claims it.
fn release_then_reserve(mutant: Mutant) {
    model(move || {
        let s = full_slab();
        let rel: Vec<_> = (0..NSLOTS)
            .map(|i| {
                thread::spawn(move || {
                    s.write(1 << i, 2);
                    if mutant == Mutant::RelaxedRelease {
                        s.release_relaxed(1 << i);
                    } else {
                        s.release(1 << i);
                    }
                })
            })
            .collect();
        if s.reserve(1) == 1 {
            let m = if mutant == Mutant::RelaxedClaim {
                s.claim_relaxed(1)
            } else {
                s.claim(1, 1)
            };
            s.write(m, 3);
            s.release(m);
        }
        for h in rel {
            h.join().unwrap();
        }
        assert_eq!(s.live.load(Ordering::Relaxed), 0);
        assert_eq!(s.words[0].load(Ordering::Relaxed) & SLAB_MASK, 0);
    });
}

/// Two owners release one slot each while a third thread reserves one
/// and claims it: whichever release the reservation's `live` load saw,
/// that slot's cleared bit is visible to the claim's first pass, and the
/// new owner's write is ordered after the old owner's. The claim may
/// take the other owner's slot, whose release `live` has not counted
/// yet: then only the bitmap's `Release` and `Acquire` order the two
/// owners' writes (the two controls below).
#[test]
fn loom_bitmap_release_then_reserve() {
    release_then_reserve(Mutant::None);
}

/// The release's ordering is checked: with a `Relaxed` `fetch_and`, the
/// next owner's write races the previous owner's.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_bitmap_relaxed_release_is_a_race() {
    release_then_reserve(Mutant::RelaxedRelease);
}

/// The claim's ordering is checked: with a `Relaxed` `fetch_or`, the new
/// owner's write races the previous owner's.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_bitmap_relaxed_claim_is_a_race() {
    release_then_reserve(Mutant::RelaxedClaim);
}

/// One owner releases both slots; two claimers reserve one each and race
/// on the word. The loser of a `fetch_or` sees the winner's bit on its
/// next pass, so each is met within two passes, and the slots are
/// disjoint.
#[test]
fn loom_bitmap_two_claimers() {
    model(|| {
        let s = full_slab();
        let claimer = |s: &'static Slab| {
            thread::spawn(move || {
                if s.reserve(1) == 1 {
                    let m = s.claim(1, 2);
                    s.write(m, 3);
                    m
                } else {
                    0
                }
            })
        };
        let a = claimer(s);
        let b = claimer(s);
        s.write(SLAB_MASK, 2);
        s.release(SLAB_MASK);
        let (ma, mb) = (a.join().unwrap(), b.join().unwrap());
        assert_eq!(ma & mb, 0, "two claimers hold one slot");
        let held = (ma | mb).count_ones();
        assert_eq!(s.live.load(Ordering::Relaxed), held);
    });
}

/// `init`'s `Relaxed` writes, published by a `Release` store and read
/// through an `Acquire` load of the commit count (`src/class/table.rs`,
/// `committed_slabs`, through `vspec::atom::store_usize` and
/// `load_usize_seen`), with the commit count's store ordered `publish`:
/// a claimer that sees the slab committed finds the padding set in the
/// word with the `Relaxed` load `claim_word` takes as its hint, and never
/// claims the padding. The hint is the check: the claim's `fetch_or`
/// reads the latest value under any ordering.
fn init_published(publish: Ordering) {
    model(move || {
        let s = leak(Slab::new());
        let committed = leak(AtomicUsize::new(0));
        let t = thread::spawn(move || {
            s.bitmap().init(NSLOTS);
            committed.store(1, publish);
        });
        if committed.load(Ordering::Acquire) == 1 {
            let hint = s.words[0].load(Ordering::Relaxed);
            assert_eq!(
                hint & !SLAB_MASK,
                !SLAB_MASK,
                "the hint missed the padding: {hint:#x}"
            );
            let m = s.bitmap().claim(NSLOTS + 1).map_or(0, |(_, m)| m);
            assert_eq!(m & !SLAB_MASK, 0, "claimed padding: {m:#x}");
        }
        t.join().unwrap();
    });
}

/// The shipped orderings.
#[test]
fn loom_bitmap_init_is_published() {
    init_published(Ordering::Release);
}

/// The publication check has teeth: with the commit count stored
/// `Relaxed`, a claimer that sees the slab committed can read the word
/// as it was before `init`.
#[test]
#[should_panic(expected = "the hint missed the padding")]
fn loom_bitmap_relaxed_commit_is_unpublished() {
    init_published(Ordering::Relaxed);
}

/// The data checks have teeth: a "claimer" that writes a slot without
/// claiming it races with the owner.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_bitmap_unclaimed_write_is_a_race() {
    model(|| {
        let s = full_slab();
        let t = thread::spawn(move || {
            s.write(1, 2);
            s.release(1);
        });
        s.write(1, 3);
        t.join().unwrap();
    });
}
