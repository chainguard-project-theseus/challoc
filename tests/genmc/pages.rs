// GenMC port of tests/loom/src/pages.rs: the slab-pages walk closing a
// page while a claim takes a slot of it (docs/proposals/slab-pages.md,
// 3.4). The argument rests on read-modify-write release sequences (the
// probe's `fetch_or(0)` against the claim's `fetch_or`), which loom
// models only in part; GenMC explores them under RC11, IMM and ARMv8.
//
// One bitmap word of three slots over two pages, slot 1 straddling both
// (page 0 holds slots 0 and 1, page 1 slots 1 and 2), and the
// closed-page mask, `SlabPages::closed`. The main thread holds slot 0,
// so page 1 is closable. challoc's `vspec/word.rs` wrappers
// (`fetch_or`, `fetch_and`) run unchanged over `core` atomics; the steps
// are copies, as in the loom model:
//
// - the walk is `ClassState::close_page` (`src/class/pages.rs`): the
//   mask bit stored `Release` (`SlabPages::set_pages`), `fetch_or(0)` of
//   the word `AcqRel` (`AtomicBitmap::probe`), the page reopened
//   (`Release`) if a slot of it is set, else the discard, which writes
//   the page's slots here;
// - the claim is the bitmap pass of `ClassState::finish`
//   (`src/class/mod.rs`): the mask loaded `Acquire`, the open slots
//   wanted (`OpenView::word`, `claim_word`'s `allow`), `fetch_or` of them
//   `Acquire`, the mask loaded again, and the slots on a page closed
//   meanwhile given back (`AtomicBitmap::release`'s `fetch_and`,
//   `Release`); the claimer then writes the slots it kept.
//
// Each slot's data is a plain value, written volatile so `-Copt-level=1`
// keeps each write: a discard and a claimer's write of one slot that no
// happens-before orders is a non-atomic race, so no slot a claim keeps is
// discarded. `MUTANT` 1 (the claim keeps what it won without its
// re-check) and 2 (the walk's step 2 a load, not a read-modify-write)
// are the controls, each a race.
//
// Left out, as in the loom model: the class lock, which serialises walks
// with each other and with reopens (one walk here); the reservation
// (`live`), which the walk respects under the lock; the closed-slot
// count; the ghost tokens.

use crate::vspec::word;
use core::cell::UnsafeCell;
use core::ptr;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::thread;
use vstd::prelude::Tracked;

// The slots of each page: slot 1 straddles the two.
const PAGE_SLOTS: [u64; 2] = [0b011, 0b110];
const SLOTS: u64 = 0b111;
const NSLOTS: usize = 3;

// `OpenView::word`: the slots every page of which is open.
fn open_slots(closed: u16) -> u64 {
    let mut m = SLOTS;
    if closed & 1 != 0 {
        m &= !PAGE_SLOTS[0];
    }
    if closed & 2 != 0 {
        m &= !PAGE_SLOTS[1];
    }
    m
}

struct Slab {
    word: AtomicU64,
    closed: AtomicU16,
    stamp: AtomicU64,
    data: [UnsafeCell<u64>; NSLOTS],
}

// SAFETY: the cells are what GenMC checks for races.
unsafe impl Sync for Slab {}

static S: Slab = Slab {
    word: AtomicU64::new(!SLOTS | 0b001),
    closed: AtomicU16::new(0),
    stamp: AtomicU64::new(0),
    data: [UnsafeCell::new(0), UnsafeCell::new(0), UnsafeCell::new(0)],
};

impl Slab {
    fn write(&self, mask: u64, v: u64) {
        for (i, d) in self.data.iter().enumerate() {
            if mask & (1 << i) != 0 {
                // SAFETY: GenMC checks the access.
                unsafe { ptr::write_volatile(d.get(), v) };
            }
        }
    }

    // `ClassState::close_page` for page 1: whether it discarded it.
    fn walk(&self) -> bool {
        // Step 1: `SlabPages::set_pages`.
        self.stamp.store(1, Ordering::Relaxed);
        self.closed.store(0b10, Ordering::Release);
        // Step 2: `AtomicBitmap::probe`.
        let prev = if MUTANT == 2 {
            self.word.load(Ordering::Acquire)
        } else {
            word::fetch_or(&self.word, 0, Ordering::AcqRel, Tracked::assume_new())
        };
        if prev & PAGE_SLOTS[1] != 0 {
            self.stamp.store(1, Ordering::Relaxed);
            self.closed.store(0, Ordering::Release);
            return false;
        }
        // Step 3: the discard.
        self.write(PAGE_SLOTS[1], 0);
        true
    }

    // The bitmap pass of `ClassState::finish` for one slot: the slots kept.
    fn claim(&self) -> u64 {
        let allow = open_slots(self.closed.load(Ordering::Acquire));
        let cur = self.word.load(Ordering::Relaxed);
        let free = !cur & SLOTS & allow;
        let want = free & free.wrapping_neg();
        if want == 0 {
            return 0;
        }
        let prev = word::fetch_or(&self.word, want, Ordering::Acquire, Tracked::assume_new());
        let won = want & !prev;
        if MUTANT == 1 {
            self.write(won, 1);
            return won;
        }
        let closed = self.closed.load(Ordering::Acquire);
        let open = if closed == 0 { !0 } else { open_slots(closed) };
        let back = won & !open;
        if back != 0 {
            word::fetch_and(&self.word, !back, Ordering::Release, Tracked::assume_new());
        }
        let kept = won & !back;
        self.write(kept, 1);
        kept
    }
}

fn harness() {
    let s: &'static Slab = &S;
    s.write(0b001, 1);
    let w = thread::spawn(move || s.walk());
    let kept = s.claim();
    let discarded = w.join().unwrap();
    // A slot is kept or on a discarded page, never both.
    gassert!(
        !(discarded && kept & PAGE_SLOTS[1] != 0),
        "a kept slot was discarded"
    );
}
