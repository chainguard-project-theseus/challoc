// GenMC port of tests/loom/src/bitmap.rs: the slab bitmap's claim and
// release with the `live` reservation. challoc's `vspec/word.rs`
// wrappers (`store`, `fetch_or`, `fetch_and`) and, for the commit count,
// `vspec/atom.rs`'s (`store_usize`, `load_usize_seen`) run unchanged
// over `core` atomics; `AtomicBitmap::init` (whole words),
// `claim_word`, `claim_from`, `release`, `ClassState::take`'s
// reservation, the class bitmap pass and the free path's release then
// `dec_live` are copies, as in the loom model, for one slab of
// `NSLOTS` slots in one word. The class lock is genmc-std's `Mutex`,
// not the `RawLock` copy: GenMC has no futex.
//
// Each slot's data is a plain value every holder writes, so two holders
// at once, or a holder whose writes are not ordered after the previous
// holder's, is a non-atomic race. The writes are volatile so that
// `-Copt-level=1` keeps each one.
//
// `MODE` 0 is loom's `release_then_reserve` (two owners release one slot
// each while a third thread reserves one and claims it), with `MUTANT`
// 1 (the release's `fetch_and` `Relaxed`) and 2 (the claim's `fetch_or`
// `Relaxed`) as its controls. `MODE` 1 is `loom_bitmap_two_claimers`.
// `MODE` 2 is loom's `init_published`: `init`'s `Relaxed` writes,
// published by the commit count's store (ordered `PUBLISH`), as a
// claimer that sees the slab committed reads them. `MODE` 3 is
// `loom_bitmap_unclaimed_write_is_a_race`, a control.
//
// The seam ledger cites this template's pass cases (verus/SEAMS.md,
// "Status") for `word::store` (`bitmap_init_published` only),
// `word::fetch_or`, `word::fetch_and`, `atom::store_usize` and
// `atom::load_usize_seen`, as evidence only: the
// permissions are erased (`Tracked::assume_new()`, and an erased
// invariant for `load_usize_seen`), so no case checks a wrapper's
// contract.

use crate::vspec::{atom, word};
use core::cell::UnsafeCell;
use core::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use vstd::prelude::Tracked;

const NSLOTS: usize = 2;
const SLAB_MASK: u64 = (1 << NSLOTS) - 1;

// `usize::min`. At `-Copt-level=1` LLVM turns a min, however it is
// spelled, into `llvm.umin`, which GenMC 0.20's interpreter does not
// implement; a volatile store keeps the choice a branch.
fn min(a: usize, b: usize) -> usize {
    let mut m = b;
    if a < b {
        // SAFETY: `m` is a local.
        unsafe { ptr::write_volatile(&mut m, a) };
    }
    m
}

// `bitmap::lowest_n_bits`: the lowest `n` set bits of `m`.
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

// One slab: its bitmap word, `live`, the class lock and the slot data;
// and the class table's commit count.
struct Slab {
    words: [AtomicU64; 1],
    live: AtomicU32,
    lock: Mutex<()>,
    data: [UnsafeCell<u64>; NSLOTS],
    committed: AtomicUsize,
}

// SAFETY: `data[i]` is touched only by the holder of slot `i`, which is
// what is checked.
unsafe impl Sync for Slab {}

static S: Slab = Slab {
    words: [AtomicU64::new(0)],
    live: AtomicU32::new(0),
    lock: Mutex::new(()),
    data: [UnsafeCell::new(0), UnsafeCell::new(0)],
    committed: AtomicUsize::new(0),
};

impl Slab {
    // `AtomicBitmap::init`, whole words: slots `0..nbits` clear, the
    // padding set.
    fn init(&self, nbits: usize) {
        for (i, w) in self.words.iter().enumerate() {
            let lo = i * 64;
            let pad = if nbits >= lo + 64 {
                0
            } else if nbits <= lo {
                !0
            } else {
                !0u64 << (nbits - lo)
            };
            word::store(w, pad, Ordering::Relaxed, Tracked::assume_new());
        }
    }

    // `AtomicBitmap::claim_word`: the bits of word `i` this call won.
    fn claim_word(&self, i: usize, n: usize) -> u64 {
        let w = &self.words[i];
        let cur = w.load(Ordering::Relaxed);
        let want = lowest_n_bits(!cur, n);
        if want == 0 {
            return 0;
        }
        let prev = word::fetch_or(w, want, Ordering::Acquire, Tracked::assume_new());
        want & !prev
    }

    // `AtomicBitmap::claim_from`.
    fn claim_from(&self, start: usize, n: usize) -> Option<(usize, u64)> {
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

    // `ClassState::take`'s reservation: `n` of `want` slots.
    fn reserve(&self, want: usize) -> usize {
        let g = self.lock.lock().unwrap();
        let live = self.live.load(Ordering::Acquire) as usize;
        gassert!(live <= NSLOTS, "live above nslots");
        let n = min(NSLOTS - live, want);
        self.live.fetch_add(n as u32, Ordering::AcqRel);
        drop(g);
        n
    }

    // The claim loop for a reservation of `want`, allowing `passes` empty
    // passes. Returns the mask claimed.
    fn claim(&self, want: usize, passes: u32) -> u64 {
        let mut got = 0u64;
        let mut empty = 0u32;
        while (got.count_ones() as usize) < want {
            match self.claim_from(0, want - got.count_ones() as usize) {
                Some((w, bits)) => {
                    gassert!(w == 0, "a claim past the slab's word");
                    gassert!(got & bits == 0, "a slot claimed twice by one claimer");
                    got |= bits;
                }
                None => {
                    empty += 1;
                    gassert!(empty < passes, "reservation unmet");
                    if empty >= passes {
                        return got;
                    }
                }
            }
        }
        gassert!(got & !SLAB_MASK == 0, "claimed padding");
        got
    }

    // The holder's writes to its slots.
    fn write(&self, mask: u64, v: u64) {
        for (i, d) in self.data.iter().enumerate() {
            if mask & (1 << i) != 0 {
                // SAFETY: this thread holds slot `i` (GenMC reports a race
                // if not).
                unsafe { ptr::write_volatile(d.get(), v) };
            }
        }
    }

    // `release`, then `dec_live`; `MUTANT` 1 orders the bitmap's
    // `fetch_and` `Relaxed` instead of `release`'s `Release`.
    fn release(&self, mask: u64, relaxed: bool) {
        let prev = if relaxed {
            self.words[0].fetch_and(!mask, Ordering::Relaxed)
        } else {
            word::fetch_and(
                &self.words[0],
                !mask,
                Ordering::Release,
                Tracked::assume_new(),
            )
        };
        gassert!(prev & mask == mask, "released a clear bit");
        let n = mask.count_ones();
        let prev = self.live.fetch_sub(n, Ordering::AcqRel);
        gassert!(prev >= n, "live underflow");
    }

    // `MUTANT` 2's claim of one slot: the lowest clear bit taken by a
    // `Relaxed` `fetch_or` instead of `claim_word`'s `Acquire`.
    fn claim_relaxed(&self) -> u64 {
        let free = !self.words[0].load(Ordering::Relaxed) & SLAB_MASK;
        let bit = free & free.wrapping_neg();
        if bit != 0 && self.words[0].fetch_or(bit, Ordering::Relaxed) & bit == 0 {
            return bit;
        }
        gassert!(false, "reservation unmet");
        0
    }
}

// The slab with all its slots held by the main thread before any other
// thread starts.
fn full_slab() -> &'static Slab {
    let s: &'static Slab = &S;
    s.init(NSLOTS);
    gassert!(
        s.reserve(NSLOTS) == NSLOTS,
        "the first reservation is short"
    );
    let m = s.claim(NSLOTS, 1);
    gassert!(m == SLAB_MASK, "the first claim is short");
    s.write(m, 1);
    s
}

fn release_then_reserve() {
    let s = full_slab();
    let r0 = thread::spawn(move || {
        s.write(1, 2);
        s.release(1, MUTANT == 1);
    });
    let r1 = thread::spawn(move || {
        s.write(2, 2);
        s.release(2, MUTANT == 1);
    });
    if s.reserve(1) == 1 {
        let m = if MUTANT == 2 {
            s.claim_relaxed()
        } else {
            s.claim(1, 1)
        };
        s.write(m, 3);
        s.release(m, false);
    }
    r0.join().unwrap();
    r1.join().unwrap();
    gassert!(
        s.live.load(Ordering::Relaxed) == 0,
        "live is not 0 at the end"
    );
    gassert!(
        s.words[0].load(Ordering::Relaxed) & SLAB_MASK == 0,
        "a slot is still claimed at the end"
    );
}

fn claimer() -> u64 {
    let s: &'static Slab = &S;
    if s.reserve(1) == 1 {
        let m = s.claim(1, 2);
        s.write(m, 3);
        m
    } else {
        0
    }
}

fn two_claimers() {
    let s = full_slab();
    let a = thread::spawn(claimer);
    let b = thread::spawn(claimer);
    s.write(SLAB_MASK, 2);
    s.release(SLAB_MASK, false);
    let (ma, mb) = (a.join().unwrap(), b.join().unwrap());
    gassert!(ma & mb == 0, "two claimers hold one slot");
    gassert!(
        s.live.load(Ordering::Relaxed) == (ma | mb).count_ones(),
        "live is not the slots held"
    );
}

fn init_published() {
    let s: &'static Slab = &S;
    let t = thread::spawn(move || {
        s.init(NSLOTS);
        atom::store_usize(&s.committed, 1, PUBLISH, Tracked::assume_new());
    });
    if vspec::load_usize_seen(&s.committed, Ordering::Acquire) == 1 {
        let hint = s.words[0].load(Ordering::Relaxed);
        gassert!(
            hint & !SLAB_MASK == !SLAB_MASK,
            "the hint missed the padding"
        );
        let m = s.claim_from(0, NSLOTS + 1).map_or(0, |(_, m)| m);
        gassert!(m & !SLAB_MASK == 0, "claimed padding");
    }
    t.join().unwrap();
}

fn unclaimed_write() {
    let s = full_slab();
    let t = thread::spawn(move || {
        s.write(1, 2);
        s.release(1, false);
    });
    s.write(1, 3);
    t.join().unwrap();
}

fn harness() {
    match MODE {
        0 => release_then_reserve(),
        1 => two_claimers(),
        2 => init_published(),
        _ => unclaimed_write(),
    }
}
