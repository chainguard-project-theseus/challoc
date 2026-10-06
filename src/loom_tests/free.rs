//! P5: the `free` fast path's `Relaxed` reads (`verus/WEAKMEM.md`). The
//! bitmap is the real `AtomicBitmap` (`bitmap.rs`: `init`, `claim_from`,
//! `release`) and the bit test the real `vspec::word::load`, over one loom
//! word for a slab of [`NSLOTS`] slots, and the watermark's accesses the
//! real `vspec::atom` wrappers (`store_usize`, `load_usize_in`); the table
//! steps around them are copies of `class/table.rs` and `heap/shared.rs`,
//! with their orderings:
//!
//! - the hand-out is the tail of `SlabTable::take_group`: the slab's
//!   bitmap `init` (`Relaxed` writes), `store_owner` (the group's owner
//!   word, `Relaxed`), then `store_usize(end, Release)` of the watermark;
//! - a claimer finds the slab through `committed_slabs()` (`Acquire`), as
//!   in `bitmap.rs`'s `loom_bitmap_init_is_published`;
//! - the free path is `Shared::set_slot` and `home_owns` (through
//!   `heap::free_fast_slot` and `free_fast_push`; and
//!   `SlabTable::check_located`, which makes the same reads):
//!   `is_handed_out` (the watermark, `Relaxed`), `is_set_unchecked` (the
//!   bit, `Relaxed`, through `vspec::word::load`), then `owner_unchecked`
//!   (`load_owner`, `Relaxed`).
//!
//! The allocating thread hands the slot to the freeing thread through an
//! atomic: with a `Release` store and an `Acquire` load (a synchronising
//! hand-off, as a C program must use: C11 7.22.3p2 lets `malloc` and
//! `free` visibly modify the storage they allocate or deallocate, which
//! we read as making a free not ordered after its allocation a data race
//! on the block), the free must see the slab handed
//! out, the bit set and the owner stored, and its writes to the block must
//! not race with the allocating thread's; a third thread claims and
//! releases the other slot of the word meanwhile, so the bit test reads a
//! word other writes keep changing. With a `Relaxed` hand-off the model
//! records which outcomes appear, to document them.
//!
//! Left out: the `FREE_ROUTE` bit of `Shared::free_flags` (a `Relaxed`
//! byte read after the bit test and before the owner word, its bit set
//! once when a second arena is built; not modelled), the cache push after
//! the checks, more than one word or group, and the class lock and sets
//! that carry a slab from `take_group` to a claimer (here the claimer's
//! `Acquire` load of the watermark stands for that chain).

use super::{UsizeInv, model};
use crate::bitmap::AtomicBitmap;
use crate::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};
use crate::vspec::atom::{load_usize_in, store_usize};
use loom::cell::UnsafeCell;
use loom::thread;
use std::boxed::Box;
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
/// (`never_committed`, the double-free check) or release the slot to the
/// arena it read. The model makes the reads once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Out {
    /// Handed out, bit set, owner `ARENA`: the free goes ahead.
    Ok,
    /// The watermark read below the slab.
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
    /// `SlabTable::committed_inv` (ghost).
    inv: UsizeInv,
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

// `cfg(test)` again: `verus/seams.sh` skips test code only by the attribute
// on the item, and the permission minted below is not a seam.
#[cfg(test)]
impl Table {
    fn new() -> Table {
        Table {
            committed: AtomicUsize::new(0),
            inv: Tracked::assume_new(),
            owner: AtomicU16::new(0),
            words: [AtomicU64::new(0)],
            data: [UnsafeCell::new(0), UnsafeCell::new(0)],
            chan: AtomicUsize::new(0),
        }
    }

    fn bitmap(&self) -> AtomicBitmap<'_> {
        AtomicBitmap::from_words_t(&self.words)
    }

    /// The tail of `take_group`: `init`, `store_owner`, the watermark.
    fn hand_out(&self) {
        self.bitmap().init_t(NSLOTS);
        self.owner.store(ARENA, Ordering::Relaxed);
        store_usize(&self.committed, 1, Ordering::Release, Tracked::assume_new());
    }

    /// A claim of one slot of a slab found through `committed_slabs()`,
    /// whose load is `order` (`Acquire` as shipped);
    /// `None` when the slab is not handed out yet or no slot is clear.
    fn claim(&self, order: Ordering) -> Option<usize> {
        if load_usize_in(&self.committed, order, &self.inv) == 0 {
            return None;
        }
        let (w, bits) = self.bitmap().claim_from_t(0, 1)?;
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
        let (prev, _) = self.bitmap().release_t(0, 1 << slot);
        assert_ne!(prev & (1 << slot), 0, "released a clear bit");
    }

    /// The free path's checks of `slot` (slab 0).
    fn check(&self, slot: usize) -> Out {
        let slab = 0;
        // `is_handed_out`.
        if slab >= load_usize_in(&self.committed, Ordering::Relaxed, &self.inv) {
            return Out::NeverCommitted;
        }
        // `is_set_unchecked`.
        let x = crate::vspec::word::load(&self.words[0], Ordering::Relaxed, Tracked::assume_new());
        if (x >> slot) & 1 != 1 {
            return Out::NotLive;
        }
        // `owner_unchecked` (`load_owner`).
        let owner = self.owner.load(Ordering::Relaxed);
        if owner != ARENA {
            return Out::Misrouted(owner);
        }
        Out::Ok
    }
}

/// Leaked, as in `init.rs`: a failed model must not drop loom objects.
fn leak<T>(v: T) -> &'static T {
    Box::leak(Box::new(v))
}

/// The hand-out, an allocating thread (its watermark load `claim`) that
/// hands its slot to the main
/// thread with `handoff` (store, load), a thread that claims and releases
/// the other slot, and the main thread's free. `seen` collects the
/// outcomes of the free's checks; with `touch` the free writes the block
/// and releases the slot when the checks pass.
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
            if touch && r == Out::Ok {
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
/// handed out, live and owned by `ARENA`, and its writes to the block do
/// not race with the allocating thread's.
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
    assert_eq!(*seen, BTreeSet::from([Out::Ok]), "outcomes: {seen:?}");
}

/// A `Relaxed` hand-off (a data race on the block under C11, so outside
/// the C contract): documents what the checks may then read. Each of the
/// three reads can be stale: the watermark below the slab, the bit clear,
/// or the owner word's initial 0, each a fast-path miss (see [`Out`]:
/// a spurious abort or a slot released to arena 0 needs the slow path's
/// re-read stale too). The free does not touch the block here (that
/// would be a reported race).
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
    std::println!("relaxed hand-off outcomes: {seen:?}");
    let all = BTreeSet::from([
        Out::Ok,
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
    std::println!("unpublished owner outcomes: {seen:?}");
    assert!(seen.contains(&Out::Misrouted(0)), "outcomes: {seen:?}");
}
