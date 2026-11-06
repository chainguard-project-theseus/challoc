//! The `free` fast path's `Relaxed` reads. The bitmap is the copy of
//! `AtomicBitmap` (`bitmap.rs`: `init`, `claim_from`, `release`) over one
//! loom word for a slab of [`NSLOTS`] slots; the table steps around it
//! are copies of `src/class/table.rs` and `src/heap/shared.rs`, with
//! their orderings, calling `vspec/atom.rs`'s and `vspec/word.rs`'s
//! wrappers, which run here as they are, over loom's atomics
//! (`crate::vspec`):
//!
//! - the hand-out is the tail of `SlabTable::take_group`: the slab's
//!   bitmap `init` (`Relaxed` writes), `store_owner` (the group's owner
//!   word, `Relaxed`), then `store_usize(end, Release)` of the watermark
//!   and `store_usize_at(end, Release)` of its copy in the class line
//!   (`heap::ClassLine`'s `committed`, through the pointer
//!   `SlabTable::mirror_watermark` stored);
//! - a claimer finds the slab through `committed_slabs()` (`Acquire`), as
//!   in `bitmap.rs`'s `loom_bitmap_init_is_published`;
//! - the free path is `Shared::set_slot` and `home_owns` (through
//!   `heap::free_fast_slot` and `free_fast_tail`): `ClassLine::set_slab`'s
//!   watermark copy (`class::load_mirror`, `Relaxed`) and bit
//!   (`SlabTable::bit_word`, `Relaxed`, through `vspec::word::load`),
//!   then, when the flag byte has `FREE_ROUTE` (`home_owns`),
//!   `ClassLine::owner` (`owner_from`, `load_owner`'s `load_u16_at`,
//!   `Relaxed`; the model always reads it); `SlabTable::check_located`,
//!   on the slow path, makes the same reads from the table (`check_live`:
//!   `is_handed_out`, the watermark itself, `Relaxed`, and
//!   `is_set_unchecked`; then `owner_at`).
//!
//! The allocating thread hands the slot to the freeing thread through an
//! atomic: with a `Release` store and an `Acquire` load (a synchronising
//! hand-off, as a C program must use: C11 7.22.3p2 lets `malloc` and
//! `free` visibly modify the storage they allocate or deallocate, which
//! we read as making a free not ordered after its allocation a data race
//! on the block), the free must see the slab handed out, the bit set and
//! the owner stored, and its writes to the block must not race with the
//! allocating thread's; a third thread claims and releases the other slot
//! of the word meanwhile, so the bit test reads a word other writes keep
//! changing. With a `Relaxed` hand-off the model records which outcomes
//! appear, to document them.
//!
//! Left out: `Shared::free_flags` itself (a `Relaxed` byte read after the
//! bit test, whose `FREE_ROUTE` bit, set once when a second arena is
//! built, decides whether the owner word is read: `flags.rs`), the cache
//! push after the checks, more than one word or group, and the class lock
//! and sets that carry a slab from `take_group` to a claimer (here the
//! claimer's `Acquire` load of the watermark stands for that chain).

use crate::bitmap::AtomicBitmap;
use crate::vspec::{self, atom, word};
use crate::{leak, model};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};
use loom::thread;
use std::collections::BTreeSet;
use std::sync::Mutex;
use vstd::prelude::Tracked;

const NSLOTS: usize = 2;

/// The arena `take_group` stores as the group's owner (not 0, the value of
/// the never-written word).
const ARENA: u16 = 3;

/// What the free path's checks found. Every outcome but `Ok` is a miss of
/// the fast path (`free_fast_slot` returns `None`); the slow path
/// (`classify_free`, then `check_located`) repeats the same `Relaxed`
/// reads, and only if its re-read is stale too does it abort
/// (`never_committed`, or `not_live`, the double-free check) or release
/// the slot to the arena it read. The model makes the reads once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Out {
    /// Handed out, bit set, owner `ARENA`: the free goes ahead.
    Ok,
    /// As `Ok`, but the class line's watermark copy read below the slab
    /// (it is stored after the watermark, so a free synchronised with the
    /// watermark's store need not see it): the fast path declines, and the
    /// slow path, which repeats the check against the table's watermark
    /// (`check_live`), goes ahead.
    Lagged,
    /// The watermark copy and then the table's watermark read below the
    /// slab.
    NeverCommitted,
    /// The bit read clear.
    NotLive,
    /// The owner read as another arena.
    Misrouted(u16),
}

/// One class table line, one group of one slab.
struct Table {
    /// `SlabTable::committed_slabs`, the watermark.
    committed: AtomicUsize,
    /// The watermark's copy in the class line (`heap::ClassLine`).
    mirror: AtomicUsize,
    /// The group's owner word.
    owner: AtomicU16,
    /// The slab's bitmap word.
    words: [AtomicU64; 1],
    /// The slots' first words (user data, then the cache link).
    data: [UnsafeCell<u64>; NSLOTS],
    /// The program's hand-off of a slot (`slot + 1`; 0 for none yet).
    chan: AtomicUsize,
}

// SAFETY: `data[i]` is touched only by the holder of slot `i`, which is
// what is checked.
unsafe impl Sync for Table {}

impl Table {
    fn new() -> Table {
        Table {
            committed: AtomicUsize::new(0),
            mirror: AtomicUsize::new(0),
            owner: AtomicU16::new(0),
            words: [AtomicU64::new(0)],
            data: [UnsafeCell::new(0), UnsafeCell::new(0)],
            chan: AtomicUsize::new(0),
        }
    }

    fn bitmap(&self) -> AtomicBitmap<'_> {
        AtomicBitmap::from_words(&self.words)
    }

    /// The tail of `take_group`: `init`, `store_owner`, the watermark,
    /// then its copy.
    fn hand_out(&self) {
        self.bitmap().init(NSLOTS);
        self.owner.store(ARENA, Ordering::Relaxed);
        atom::store_usize(&self.committed, 1, Ordering::Release, Tracked::assume_new());
        // SAFETY: `mirror` is a live atomic in the leaked table.
        unsafe {
            atom::store_usize_at(
                atom::usize_ptr(&self.mirror),
                1,
                Ordering::Release,
                Tracked::assume_new(),
            )
        }
    }

    /// `SlabTable::committed_slabs` (`load_usize_seen`) for an `Acquire`
    /// load, else `SlabTable::watermark` (`load_usize`).
    fn watermark(&self, order: Ordering) -> usize {
        if order == Ordering::Acquire {
            vspec::load_usize_seen(&self.committed, order)
        } else {
            atom::load_usize(&self.committed, order, Tracked::assume_new())
        }
    }

    /// A claim of one slot of a slab found through `committed_slabs()`,
    /// whose load is `order` (`Acquire` as shipped);
    /// `None` when the slab is not handed out yet or no slot is clear.
    fn claim(&self, order: Ordering) -> Option<usize> {
        if self.watermark(order) == 0 {
            return None;
        }
        let (w, bits) = self.bitmap().claim_from(0, 1)?;
        assert_eq!(w, 0);
        assert_eq!(bits.count_ones(), 1, "claimed {bits:#x}");
        let slot = bits.trailing_zeros() as usize;
        assert!(slot < NSLOTS, "claimed padding: {bits:#x}");
        Some(slot)
    }

    /// The holder's write to its slot.
    fn write(&self, slot: usize, v: u64) {
        // SAFETY: this thread holds `slot` (loom reports a race if not).
        self.data[slot].with_mut(|p| unsafe { *p = v });
    }

    /// `release` of one slot (the class's `dec_live` is `bitmap.rs`'s).
    fn release(&self, slot: usize) {
        let prev = self.bitmap().release(0, 1 << slot);
        assert_ne!(prev & (1 << slot), 0, "released a clear bit");
    }

    /// The free path's checks of `slot` (slab 0).
    fn check(&self, slot: usize) -> Out {
        let slab = 0;
        // `ClassLine::set_slab`'s watermark copy (`load_mirror`); below
        // the slab, the slow path's `check_live` reads the table's.
        let lagged =
            slab >= atom::load_usize(&self.mirror, Ordering::Relaxed, Tracked::assume_new());
        if lagged && slab >= self.watermark(Ordering::Relaxed) {
            return Out::NeverCommitted;
        }
        // `bit_word`.
        let x = word::load(&self.words[0], Ordering::Relaxed, Tracked::assume_new());
        if (x >> slot) & 1 != 1 {
            return Out::NotLive;
        }
        // `ClassLine::owner` (`owner_from`, `load_owner`).
        // SAFETY: the owner word is a live atomic in the leaked table.
        let owner =
            unsafe { atom::load_u16_at(&self.owner, Ordering::Relaxed, Tracked::assume_new()) };
        if owner != ARENA {
            return Out::Misrouted(owner);
        }
        if lagged { Out::Lagged } else { Out::Ok }
    }
}

/// The hand-out, an allocating thread (its watermark load `claim`) that
/// hands its slot to the main thread with `handoff` (store, load), a
/// thread that claims and releases the other slot, and the main thread's
/// free. `seen` collects the outcomes of the free's checks; with `touch`
/// the free writes the block and releases the slot when the checks pass.
fn run(
    claim: Ordering,
    handoff: (Ordering, Ordering),
    touch: bool,
    seen: &'static Mutex<BTreeSet<Out>>,
) {
    model(move || {
        let t = leak(Table::new());
        let out = thread::spawn(move || t.hand_out());
        let alloc = thread::spawn(move || {
            if let Some(slot) = t.claim(claim) {
                t.write(slot, 0xa110c);
                t.chan.store(slot + 1, handoff.0);
            }
        });
        let other = thread::spawn(move || {
            if let Some(slot) = t.claim(claim) {
                t.write(slot, 0x07e7);
                t.release(slot);
            }
        });
        let p = t.chan.load(handoff.1);
        if p != 0 {
            let slot = p - 1;
            let r = t.check(slot);
            seen.lock().unwrap().insert(r);
            if touch && matches!(r, Out::Ok | Out::Lagged) {
                // The cache link the free writes into the block.
                t.write(slot, 0);
                t.release(slot);
            }
        }
        out.join().unwrap();
        alloc.join().unwrap();
        other.join().unwrap();
    });
}

/// A synchronising hand-off: every free that receives the slot finds it
/// handed out, live and owned by `ARENA` (on the fast path, or, when the
/// class line's watermark copy lags, on the slow path's re-read of the
/// table), and its writes to the block do not race with the allocating
/// thread's.
///
/// Covers a copy of `table::store_owner` (`src/class/table.rs`) and runs
/// `vspec::atom::store_usize`, `vspec::atom::store_usize_at`,
/// `vspec::atom::load_usize`, `vspec::atom::load_usize_seen` and
/// `vspec::atom::load_u16_at` (`vspec/atom.rs`) and `vspec::word::load`
/// (`vspec/word.rs`).
#[test]
fn loom_free_after_synchronised_handoff() {
    static SEEN: Mutex<BTreeSet<Out>> = Mutex::new(BTreeSet::new());
    run(
        Ordering::Acquire,
        (Ordering::Release, Ordering::Acquire),
        true,
        &SEEN,
    );
    let seen = SEEN.lock().unwrap();
    assert_eq!(
        *seen,
        BTreeSet::from([Out::Ok, Out::Lagged]),
        "outcomes: {seen:?}"
    );
}

/// A `Relaxed` hand-off (a data race on the block under C11, so outside
/// the C contract): documents what the checks may then read. Each of the
/// three reads can be stale: the watermark copy below the slab (with the
/// table's watermark too, or not: `Lagged`), the bit clear, or the owner
/// word's initial 0, each a fast-path miss (see [`Out`]: a spurious abort
/// or a slot released to arena 0 needs the slow path's re-read stale
/// too). The free does not touch the block here (that would be a reported
/// race).
///
/// Runs `vspec::atom::load_usize`,
/// `vspec::atom::load_usize_seen` and `vspec::atom::load_u16_at`
/// (`vspec/atom.rs`) and `vspec::word::load` (`vspec/word.rs`).
#[test]
fn loom_free_after_relaxed_handoff() {
    static SEEN: Mutex<BTreeSet<Out>> = Mutex::new(BTreeSet::new());
    run(
        Ordering::Acquire,
        (Ordering::Relaxed, Ordering::Relaxed),
        false,
        &SEEN,
    );
    let seen = SEEN.lock().unwrap();
    let all = BTreeSet::from([
        Out::Ok,
        Out::Lagged,
        Out::NeverCommitted,
        Out::NotLive,
        Out::Misrouted(0),
    ]);
    assert_eq!(*seen, all, "outcomes: {seen:?}");
}

/// The race check has teeth: with a `Relaxed` hand-off, the free's write
/// to the block races with the allocating thread's.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_free_relaxed_handoff_is_a_race() {
    static SEEN: Mutex<BTreeSet<Out>> = Mutex::new(BTreeSet::new());
    run(
        Ordering::Acquire,
        (Ordering::Relaxed, Ordering::Relaxed),
        true,
        &SEEN,
    );
}

/// The owner check has teeth: with the claimer's watermark load `Relaxed`
/// (no edge from `take_group`'s `Release` store), a free after a
/// synchronising hand-off can read the owner word's initial 0.
#[test]
fn loom_free_unpublished_owner_is_misrouted() {
    static SEEN: Mutex<BTreeSet<Out>> = Mutex::new(BTreeSet::new());
    run(
        Ordering::Relaxed,
        (Ordering::Release, Ordering::Acquire),
        false,
        &SEEN,
    );
    let seen = SEEN.lock().unwrap();
    assert!(seen.contains(&Out::Misrouted(0)), "outcomes: {seen:?}");
}
