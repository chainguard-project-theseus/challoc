//! The class table's hand-out watermark (`src/class/table.rs`,
//! `committed_slabs`) and its copy in the class line (`heap::ClassLine`'s
//! `committed`), a copy of the `SlabTable` steps that write and read
//! them, with their orderings:
//!
//! - [`Table::take_group`] is `SlabTable::take_group` without the
//!   commits: under the commit lock (the copy of `RawLock`; the lock's
//!   value, `next_slab`, a loom `UnsafeCell` beside it), `None` at
//!   `s >= nslabs`, the `check_zero` comparison of `s` with a `Relaxed`
//!   load of the watermark, `store_owner` (the group's owner word,
//!   `Relaxed`), `next_slab = end`, then `store_usize(end, Release)` of
//!   the watermark and, once the class line's copy is installed
//!   (`gi.mirror` non-null), `store_usize_at(end, Release)` of the copy;
//! - [`Table::watermark`] is `SlabTable::committed_slabs`
//!   (`load_usize_seen`, `Acquire`) and `SlabTable::watermark`
//!   (`load_usize` with the caller's ordering, `Relaxed` for
//!   `is_handed_out` and `desc_if_committed`);
//! - [`Table::mirror_watermark`] is `SlabTable::mirror_watermark`: under
//!   the lock, the lock's value `next_slab` into the copy with
//!   `store_usize_at(.., Release)`, and the copy installed;
//!   [`Table::read_mirror`] is `ClassLine::set_slab`'s `Relaxed` load of
//!   the copy (`class::load_mirror`).
//!
//! The copy models (`run_mirror`) check, for `store_usize_at`'s contract:
//! under the lock each hand-out finds the copy at the group's start (the
//! previous holder's store; a load the model adds), the copy never
//! exceeds the slab count, a reader's loads of it never go back, and
//! once every thread is joined it is the slab count. The other models
//! never install the copy.
//!
//! Checked: every load returns at most the slab count (what
//! `WatermarkPred` bounds); a thread's loads never go back (read-read
//! coherence); the `check_zero` comparison under the lock always finds
//! the watermark at `next_slab`; an `Acquire` load that returns `w` sees
//! the owner of every group below `w` stored (the witness
//! `load_usize_seen` hands `load_owner`); and once every thread is joined
//! the watermark is `next_slab`. Left out: the commits (`ensure`, and its
//! failure, which returns before the store; `init` of the bitmap:
//! `free.rs` and `bitmap.rs` run `init` before this store), the
//! descriptors (`SlabDesc::init`, the `owner_mismatch` check,
//! `check_zero`'s descriptor check), the load of the previous owner word,
//! the THP advice after the store, and groups of more than one slab.

use crate::lock::RawLock;
use crate::{leak, model};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use loom::thread;

/// Slabs in the region, one per group (`lg_group` 0).
const NSLABS: usize = 2;

struct Table {
    /// The commit lock (`SlabTable::inner`).
    lock: RawLock,
    /// `next_slab`, under the lock.
    next: UnsafeCell<usize>,
    /// `SlabTable::committed_slabs`.
    committed: AtomicUsize,
    /// The groups' owner words (0: never written).
    owners: [AtomicU16; NSLABS],
    /// The class line's copy of the watermark (`heap::ClassLine`'s
    /// `committed`), written only through `store_usize_at`.
    mirror: AtomicUsize,
    /// Whether `mirror_watermark` has run (`gi.mirror` non-null), under
    /// the lock.
    installed: UnsafeCell<bool>,
    /// The ordering of the watermark's store (`Release` as shipped).
    publish: Ordering,
    /// The copy stored after the lock's release instead of under it (a
    /// control; `false` as shipped).
    mirror_unlocked: bool,
}

// SAFETY: `next` and `installed` are touched only under `lock` (loom
// reports a race if not).
unsafe impl Sync for Table {}

impl Table {
    fn new(publish: Ordering, mirror_unlocked: bool) -> Table {
        Table {
            lock: RawLock::new(),
            next: UnsafeCell::new(0),
            committed: AtomicUsize::new(0),
            owners: [AtomicU16::new(0), AtomicU16::new(0)],
            mirror: AtomicUsize::new(0),
            installed: UnsafeCell::new(false),
            publish,
            mirror_unlocked,
        }
    }

    /// `SlabTable::committed_slabs` (`Acquire`) and
    /// `SlabTable::watermark`.
    fn watermark(&self, order: Ordering) -> usize {
        self.committed.load(order)
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
        self.committed.store(end, self.publish);
        // SAFETY: under the lock.
        let installed = self.installed.with(|p| unsafe { *p });
        if installed {
            // The copy's last write is the previous holder's (a load the
            // model adds, not in the source).
            assert_eq!(
                self.mirror.load(Ordering::Relaxed),
                s,
                "take_group at next_slab {s} but the copy differs"
            );
        }
        if installed && !self.mirror_unlocked {
            self.store_mirror(end);
        }
        self.lock.unlock();
        if installed && self.mirror_unlocked {
            self.store_mirror(end);
        }
        Some(s)
    }

    /// The copy's store, `store_usize_at(.., Release)`.
    fn store_mirror(&self, v: usize) {
        self.mirror.store(v, Ordering::Release);
    }

    /// `SlabTable::mirror_watermark`: under the lock, the lock's value
    /// `next_slab` into the copy, and the copy installed.
    fn mirror_watermark(&self) {
        self.lock.lock();
        // SAFETY: under the lock.
        let wm = self.next.with(|p| unsafe { *p });
        self.store_mirror(wm);
        // SAFETY: under the lock.
        self.installed.with_mut(|p| unsafe { *p = true });
        self.lock.unlock();
    }

    /// A load of the copy (`ClassLine::set_slab`'s, `Relaxed`).
    fn read_mirror(&self) -> usize {
        let m = self.mirror.load(Ordering::Relaxed);
        assert!(m <= NSLABS, "copy {m} above the slab count");
        m
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

/// Two threads each hand out a group (arenas 1 and 2) while the main
/// thread reads the watermark; then a third hand-out finds the region
/// full.
fn run(publish: Ordering) {
    model(move || {
        let t = leak(Table::new(publish, false));
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
///
/// Covers copies of `table::store_owner` (`src/class/table.rs`) and
/// `vspec::atom::store_usize`, `vspec::atom::load_usize`,
/// `vspec::atom::load_usize_seen` and `vspec::atom::load_u16_at`
/// (`vspec/atom.rs`).
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

/// The class line's copy. With `install_first`, the main thread installs
/// the copy (`mirror_watermark`), then two threads each hand out a group
/// while it reads the copy twice; otherwise one thread hands out a group
/// while the main thread installs the copy and reads it, then hands out
/// the second itself. Then a last hand-out finds the region full and the
/// copy is the slab count. `unlocked` stores the copy after the lock's
/// release. (Installing the copy while two threads hand out would be a model of
/// three lock holders, too large to exhaust.)
fn run_mirror(install_first: bool, unlocked: bool) {
    model(move || {
        let t = leak(Table::new(Ordering::Release, unlocked));
        if install_first {
            t.mirror_watermark();
        }
        let a = thread::spawn(move || t.take_group(1));
        let b = install_first.then(|| thread::spawn(move || t.take_group(2)));
        if !install_first {
            t.mirror_watermark();
        }
        let m = t.read_mirror();
        let m2 = t.read_mirror();
        assert!(m2 >= m, "the copy went back from {m} to {m2}");
        let sa = a.join().unwrap();
        let sb = match b {
            Some(b) => b.join().unwrap(),
            None => t.take_group(2),
        };
        assert!(sa.is_some() && sb.is_some() && sa != sb);
        assert!(t.read_mirror() >= m2, "the copy went back from {m2}");
        assert_eq!(t.take_group(3), None);
        assert_eq!(t.read_mirror(), NSLABS, "the copy is not the slab count");
    });
}

/// `store_usize_at`'s contract, the value of the copy's last write,
/// with the copy installed before two concurrent hand-outs: under the
/// lock each hand-out finds the copy at the previous holder's store
/// (the watermark), a reader's loads never go back, and once every
/// thread is joined the copy is the slab count.
///
/// Covers copies of `vspec::atom::store_usize_at` and
/// `vspec::atom::store_usize` (`vspec/atom.rs`).
#[test]
fn loom_watermark_mirror_follows() {
    run_mirror(true, false);
}

/// The same, with the copy installed concurrently with a hand-out:
/// `mirror_watermark` stores the `next_slab` it reads under the lock,
/// before or after the hand-out's.
///
/// Covers copies of `vspec::atom::store_usize_at` and
/// `vspec::atom::store_usize` (`vspec/atom.rs`).
#[test]
fn loom_watermark_mirror_installed_concurrently() {
    run_mirror(false, false);
}

/// The copy check has teeth: stored after the lock's release, the copy
/// can lag the watermark under the lock (or end below the slab count).
#[test]
#[should_panic(expected = "the copy")]
fn loom_watermark_unlocked_mirror_lags() {
    run_mirror(true, true);
}
