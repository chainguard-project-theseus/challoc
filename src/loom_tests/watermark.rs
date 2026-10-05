//! The class table's hand-out watermark (`class/table.rs`,
//! `committed_slabs`): the real wrappers of `vspec/atom.rs`
//! (`store_usize`, `load_usize_in`) over loom's `AtomicUsize`, driven by
//! copies of the `SlabTable` steps that call them, with their orderings:
//!
//! - [`Table::take_group`] is `SlabTable::take_group` without the
//!   commits: under the commit lock (the real `RawLock`; the lock's
//!   value, `next_slab`, a loom `UnsafeCell` beside it), `None` at
//!   `s >= nslabs`, the `check_zero` comparison of `s` with a `Relaxed`
//!   load of the watermark, `store_owner` (the group's owner word,
//!   `Relaxed`), `next_slab = end`, then `store_usize(end, Release)`;
//! - [`Table::watermark`] is `SlabTable::watermark`: `load_usize_in`
//!   with the caller's ordering, `Acquire` for `committed_slabs()` and
//!   `Relaxed` for `is_handed_out` and `desc_if_committed`.
//!
//! Checked: every load returns at most the slab count (what
//! `CommittedPred` bounds); a thread's loads never go back (read-read
//! coherence); the `check_zero` comparison under the lock always finds
//! the watermark at `next_slab`; an `Acquire` load that returns `w`
//! sees the owner of every group below `w` stored; and once every
//! thread is joined the watermark is `next_slab`. Left out: the
//! commits (`ensure`, and its failure, which returns before the store;
//! `init` of the bitmap: `free.rs` and `bitmap.rs` run `init` before
//! this store), the descriptors (`SlabDesc::init`, the `owner_mismatch`
//! check, `check_zero`'s descriptor check), the load of the previous
//! owner word, the THP advice after the store, and groups of more than
//! one slab.

use super::{UsizeInv, model};
use crate::lock::RawLock;
use crate::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use crate::vspec::atom::{load_usize_in, store_usize};
use loom::cell::UnsafeCell;
use loom::thread;
use std::boxed::Box;
use vstd::prelude::Tracked;

/// Slabs in the region, one per group (`lg_group` 0).
const NSLABS: usize = 2;

struct Table {
    /// The commit lock (`SlabTable::inner`).
    lock: RawLock,
    /// `next_slab`, under the lock.
    next: UnsafeCell<usize>,
    /// `SlabTable::committed_slabs`.
    committed: AtomicUsize,
    /// `SlabTable::committed_inv` (ghost).
    inv: UsizeInv,
    /// The groups' owner words (0: never written).
    owners: [AtomicU16; NSLABS],
    /// The ordering of the watermark's store (`Release` as shipped).
    publish: Ordering,
}

// SAFETY: `next` is touched only under `lock` (loom reports a race if
// not).
unsafe impl Sync for Table {}

// `cfg(test)` again: `verus/seams.sh` skips test code only by the
// attribute on the item, and the permissions minted below are not seams.
#[cfg(test)]
impl Table {
    fn new(publish: Ordering) -> Table {
        Table {
            lock: RawLock::new(),
            next: UnsafeCell::new(0),
            committed: AtomicUsize::new(0),
            inv: Tracked::assume_new(),
            owners: [AtomicU16::new(0), AtomicU16::new(0)],
            publish,
        }
    }

    /// `SlabTable::watermark`.
    fn watermark(&self, order: Ordering) -> usize {
        load_usize_in(&self.committed, order, &self.inv)
    }

    /// `SlabTable::take_group` for `arena`: the group `[s, s + 1)`.
    fn take_group(&self, arena: u16) -> Option<usize> {
        self.lock.lock();
        // SAFETY: under the lock.
        let s = self.next.with(|p| unsafe { *p });
        if s >= NSLABS {
            self.lock.unlock();
            return None;
        }
        let end = s + 1;
        assert_eq!(
            s,
            self.committed.load(Ordering::Relaxed),
            "check_zero: take_group at next_slab {s} but the watermark differs"
        );
        self.owners[s].store(arena, Ordering::Relaxed);
        // SAFETY: under the lock.
        self.next.with_mut(|p| unsafe { *p = end });
        store_usize(&self.committed, end, self.publish, Tracked::assume_new());
        self.lock.unlock();
        Some(s)
    }

    /// A reader's two loads, `Acquire` (`committed_slabs()`) then
    /// `Relaxed` (`is_handed_out`), and the owners below the first.
    fn read(&self) {
        let w = self.watermark(Ordering::Acquire);
        assert!(w <= NSLABS, "watermark {w} above the slab count");
        for slab in 0..w {
            assert_ne!(
                self.owners[slab].load(Ordering::Relaxed),
                0,
                "slab {slab} below an Acquire watermark {w} has no owner"
            );
        }
        let w2 = self.watermark(Ordering::Relaxed);
        assert!(w2 <= NSLABS, "watermark {w2} above the slab count");
        assert!(w2 >= w, "the watermark went back from {w} to {w2}");
    }
}

/// Leaked, as in `init.rs`: a failed model must not drop loom objects.
fn leak<T>(v: T) -> &'static T {
    Box::leak(Box::new(v))
}

/// Two threads each hand out a group (arenas 1 and 2) while the main
/// thread reads the watermark; then a third hand-out finds the region
/// full.
fn run(publish: Ordering) {
    model(move || {
        let t = leak(Table::new(publish));
        let a = thread::spawn(move || t.take_group(1));
        let b = thread::spawn(move || t.take_group(2));
        t.read();
        let (sa, sb) = (a.join().unwrap(), b.join().unwrap());
        assert!(sa.is_some() && sb.is_some() && sa != sb);
        assert_eq!(t.take_group(3), None);
        assert_eq!(t.watermark(Ordering::Relaxed), NSLABS);
        t.read();
    });
}

/// The shipped orderings.
#[test]
fn loom_watermark_take_group_publishes() {
    run(Ordering::Release);
}

/// The publication check has teeth: with the watermark stored `Relaxed`,
/// an `Acquire` load of it can return a group whose owner word reads 0.
#[test]
#[should_panic(expected = "has no owner")]
fn loom_watermark_relaxed_store_is_unpublished() {
    run(Ordering::Relaxed);
}
