//! P1: slab bitmap claim and release with the `live` reservation.
//!
//! The bitmap is the real `AtomicBitmap` (`bitmap.rs`: `init`,
//! `claim_word` through `claim_from`, `release`) over loom's atomics,
//! one word for a slab of [`NSLOTS`] slots. Around it, copies of the
//! class steps that drive it, with the orderings of the code they cite:
//!
//! - `live` is `SlabDesc::live` (`slab.rs`): `live()` loads `Acquire`,
//!   `add_live` and `sub_live` are `AcqRel` read-modify-writes;
//! - a reservation is `ClassState::take` (`class/mod.rs`, from
//!   `let live = desc.live()` to `desc.add_live(n)`): under the class
//!   lock (the real `RawLock`), `avail = nslots - live`, reserve
//!   `min(avail, want)`;
//! - the claim loop is the bitmap pass of `class/mod.rs` (`claim_from`
//!   until `want` slots are held, an empty pass counted towards
//!   `MAX_EMPTY_PASSES` and the `reservation_unmet` abort);
//! - a release is `release` then `dec_live` (`sub_live`), the order the
//!   free path keeps (`bitmap.rs` `release`, `class/mod.rs` `dec_live`).
//!
//! Checked: each slot's data is a loom `UnsafeCell` written by every
//! holder, so two holders at once, or a holder that does not see the
//! previous holder's writes, is a reported race; claimed masks are
//! disjoint and never include padding; a release never finds its bit
//! clear; and a reservation is met within one pass per concurrent
//! claimer, so the `Relaxed` hint load never hides a slot that `live`
//! promised (the abort's bound, 2^22 passes, is far above it).

use super::model;
use crate::bitmap::AtomicBitmap;
use crate::lock::RawLock;
use crate::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use loom::cell::UnsafeCell;
use loom::thread;
use std::boxed::Box;
use std::vec::Vec;

const NSLOTS: usize = 2;
const SLAB_MASK: u64 = (1 << NSLOTS) - 1;

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
        AtomicBitmap::from_words_t(&self.words)
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
            match bm.claim_from_t(0, want - got.count_ones() as usize) {
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
        let (prev, _) = self.bitmap().release_t(0, mask);
        assert_eq!(prev & mask, mask, "released a clear bit");
        let n = mask.count_ones();
        let prev = self.live.fetch_sub(n, Ordering::AcqRel);
        assert!(prev >= n, "live underflow");
    }
}

/// Leaked, not an `Arc`: a failed model ends the execution with threads
/// parked, and dropping a loom object while that panic unwinds aborts
/// the test binary.
fn leak<T>(v: T) -> &'static T {
    Box::leak(Box::new(v))
}

/// A slab whose slots are all held by the main thread before any other
/// thread starts.
fn full_slab() -> &'static Slab {
    let s = leak(Slab::new());
    s.bitmap().init_t(NSLOTS);
    assert_eq!(s.reserve(NSLOTS), NSLOTS);
    let m = s.claim(NSLOTS, 1);
    assert_eq!(m, SLAB_MASK);
    s.write(m, 1);
    s
}

/// Two owners release one slot each while a third thread reserves one
/// and claims it: whichever release the reservation's `live` load saw,
/// that slot's cleared bit is visible to the claim's first pass, and the
/// new owner's write is ordered after the old owner's.
#[test]
fn loom_bitmap_release_then_reserve() {
    model(|| {
        let s = full_slab();
        let rel: Vec<_> = (0..NSLOTS)
            .map(|i| {
                thread::spawn(move || {
                    s.write(1 << i, 2);
                    s.release(1 << i);
                })
            })
            .collect();
        if s.reserve(1) == 1 {
            let m = s.claim(1, 1);
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
/// through an `Acquire` load (`class/table.rs`, `committed_slabs`): a
/// claimer that sees the slab committed never claims its padding.
#[test]
fn loom_bitmap_init_is_published() {
    model(|| {
        let s = leak(Slab::new());
        let committed = leak(AtomicU32::new(0));
        let t = thread::spawn(move || {
            s.bitmap().init_t(NSLOTS);
            committed.store(1, Ordering::Release);
        });
        if committed.load(Ordering::Acquire) == 1 {
            let m = s.bitmap().claim(NSLOTS + 1).map_or(0, |(_, m)| m);
            assert_eq!(m & !SLAB_MASK, 0, "claimed padding: {m:#x}");
        }
        t.join().unwrap();
    });
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
