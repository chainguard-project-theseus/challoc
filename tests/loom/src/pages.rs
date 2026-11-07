//! Slab pages (docs/proposals/slab-pages.md, 3.4): the decay walk closing
//! a page while a claim takes a slot of it.
//!
//! One bitmap word of three slots over two pages, slot 1 straddling both
//! (page 0 holds slots 0 and 1, page 1 slots 1 and 2), and a closed-page
//! mask, `SlabPages::closed`. The main thread holds slot 0, so page 1 is
//! closable. The steps are copies, calling `vspec/word.rs`'s wrappers,
//! which run here as they are:
//!
//! - the walk is `ClassState::close_page` (`src/class/pages.rs`): the mask
//!   bit stored `Release` (`SlabPages::set_pages`), `fetch_or(0)` of the
//!   word `AcqRel` (`AtomicBitmap::probe`), the page reopened (`Release`)
//!   if a slot of it is set, else the discard, which writes the page's
//!   slots here;
//! - the claim is the bitmap pass of `ClassState::finish`
//!   (`src/class/mod.rs`): the mask loaded `Acquire` (`SlabPages::closed`),
//!   the open slots wanted (`OpenView::word`, `claim_word`'s `allow`),
//!   `fetch_or` of them `Acquire`, the mask loaded again, and the slots on
//!   a page closed meanwhile given back (`AtomicBitmap::release`'s
//!   `fetch_and`, `Release`); the claimer then writes the slots it kept.
//!
//! Checked: each slot's data is a loom `UnsafeCell`, so a discard and a
//! claimer's write of one slot that no happens-before orders is a reported
//! race: no slot a claim keeps is discarded. The controls: a claim without
//! its re-check, and a walk whose step 2 is a load rather than a
//! read-modify-write, each race.
//!
//! Left out: the class lock, which serialises walks with each other and
//! with reopens (one walk here); the reservation (`live`), which the walk
//! respects under the lock; the closed-slot count; the ghost tokens.

use crate::vspec::word;
use crate::{leak, model};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use loom::thread;
use vstd::prelude::Tracked;

/// The slots of each page: slot 1 straddles the two.
const PAGE_SLOTS: [u64; 2] = [0b011, 0b110];
const SLOTS: u64 = 0b111;
const NSLOTS: usize = 3;

/// `OpenView::word`: the slots every page of which is open.
fn open_slots(closed: u16) -> u64 {
    let mut m = SLOTS;
    for (p, s) in PAGE_SLOTS.iter().enumerate() {
        if (closed >> p) & 1 == 1 {
            m &= !s;
        }
    }
    m
}

/// `bitmap::lowest_n_bits` for one bit.
fn lowest(m: u64) -> u64 {
    m & m.wrapping_neg()
}

struct Slab {
    word: AtomicU64,
    closed: AtomicU16,
    stamp: AtomicU64,
    data: [UnsafeCell<u8>; NSLOTS],
}

// SAFETY: the cells are what the model checks for races.
unsafe impl Sync for Slab {}

impl Slab {
    fn write(&self, mask: u64, v: u8) {
        for (s, d) in self.data.iter().enumerate() {
            if mask & (1 << s) != 0 {
                // SAFETY: the model checks the access.
                d.with_mut(|p| unsafe { *p = v });
            }
        }
    }
}

/// Which step is a mutant.
#[derive(Clone, Copy, PartialEq)]
enum Mutant {
    None,
    NoRecheck,
    LoadProbe,
}

/// `ClassState::close_page` for page 1: whether it discarded it.
fn walk(s: &Slab, mutant: Mutant) -> bool {
    // Step 1: `SlabPages::set_pages`.
    s.stamp.store(1, Ordering::Relaxed);
    s.closed.store(0b10, Ordering::Release);
    // Step 2: `AtomicBitmap::probe`.
    let prev = if mutant == Mutant::LoadProbe {
        s.word.load(Ordering::Acquire)
    } else {
        word::fetch_or(&s.word, 0, Ordering::AcqRel, Tracked::assume_new())
    };
    if prev & PAGE_SLOTS[1] != 0 {
        s.stamp.store(1, Ordering::Relaxed);
        s.closed.store(0, Ordering::Release);
        return false;
    }
    // Step 3: the discard.
    s.write(PAGE_SLOTS[1], 0);
    true
}

/// The bitmap pass of `ClassState::finish` for one slot: the slots kept.
fn claim(s: &Slab, mutant: Mutant) -> u64 {
    let allow = open_slots(s.closed.load(Ordering::Acquire));
    let cur = s.word.load(Ordering::Relaxed);
    let want = lowest(!cur & SLOTS & allow);
    if want == 0 {
        return 0;
    }
    let prev = word::fetch_or(&s.word, want, Ordering::Acquire, Tracked::assume_new());
    let won = want & !prev;
    if mutant == Mutant::NoRecheck {
        s.write(won, 1);
        return won;
    }
    let closed = s.closed.load(Ordering::Acquire);
    let open = if closed == 0 { !0 } else { open_slots(closed) };
    let back = won & !open;
    if back != 0 {
        word::fetch_and(&s.word, !back, Ordering::Release, Tracked::assume_new());
    }
    let kept = won & !back;
    s.write(kept, 1);
    kept
}

fn race(mutant: Mutant) {
    model(move || {
        let s = leak(Slab {
            word: AtomicU64::new(!SLOTS | 0b001),
            closed: AtomicU16::new(0),
            stamp: AtomicU64::new(0),
            data: [UnsafeCell::new(0), UnsafeCell::new(0), UnsafeCell::new(0)],
        });
        s.write(0b001, 1);
        let w = thread::spawn(move || walk(s, mutant));
        let kept = claim(s, mutant);
        let discarded = w.join().unwrap();
        // A slot is kept or on a discarded page, never both.
        assert!(
            !(discarded && kept & PAGE_SLOTS[1] != 0),
            "a kept slot was discarded"
        );
    });
}

/// The walk closing page 1 races a claim of one slot.
///
/// Covers copies of `SlabPages::set_pages`, `SlabPages::closed`,
/// `AtomicBitmap::probe`, `ClassState::close_page`'s steps and
/// `ClassState::finish`'s re-check and give-back.
#[test]
fn loom_pages_walk_never_discards_a_kept_slot() {
    race(Mutant::None);
}

/// Control: a claim that keeps what it won without the re-check writes a
/// slot the walk discards.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_pages_claim_without_recheck_races() {
    race(Mutant::NoRecheck);
}

/// Control: a walk whose step 2 is a load, not a read-modify-write, is not
/// ordered before a claim that comes after it, which then misses the
/// closed page.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_pages_load_probe_races() {
    race(Mutant::LoadProbe);
}
