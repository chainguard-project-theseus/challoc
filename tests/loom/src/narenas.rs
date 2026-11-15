//! The arena registry's arena count and table (`src/arena.rs`,
//! `Arenas::count`, `Arenas::table`), a copy of the `Arenas` steps that
//! touch them through `vspec/count.rs`'s wrappers (`store`, `load`, over
//! the count, a `PAtomicU32`) and `vspec/entry.rs`'s (`store`, `load`,
//! over the entries, `PAtomicPtr`s): here the `cfg(loom)` stand-ins of
//! `src/sync.rs`'s `patomic`, `SeqCst` on loom's atomics. The wrappers run
//! here as they are (`crate::vspec`), with the steps' orderings, all
//! `SeqCst`:
//!
//! - [`Arenas::create`] is `Arenas::create`: under the creation lock (the
//!   copy of `RawLock`; the lock's value, the next index `n`, a loom
//!   `UnsafeCell` beside it), `None` at `index >= MAX_ARENAS`, then
//!   `build` (the arena's fields, here a loom `UnsafeCell`, then the
//!   table entry stored with `entry::store`), the lock's value set to
//!   `index + 1`, and `count::store(index + 1)`, still under the lock;
//! - [`Arenas::count`] is `Arenas::count`: `count::load`;
//! - [`Arenas::entry`] is `Arenas::get_existing`'s `entry::load` (and
//!   `get`'s, `get_frozen`'s and `load_entry`'s, the same operation);
//! - [`Arenas::scan`] is the walk the shipped code spells out with an
//!   index (`policy::empty_bytes`, `Shared::set_decay_ms`: every index
//!   below `count()`, through `get_existing`, a null entry skipped), and
//!   `Arenas::iter` (test code), the same loads.
//!
//! Checked: every load of the count returns at least the automatic count
//! and at most `MAX_ARENAS` (what `NarenasPred` bounds); a thread's loads
//! never go back (read-read coherence); a scan finds every explicit arena
//! below the count it read built, and reads its fields with no race; a
//! `create` holding the lock loads the count as the lock's value and
//! finds every explicit arena below it built (the frozen table that
//! `api::count_held` and `api::arena_held` prove from the creation lock's
//! ghost half, `arena::Created`); and once every thread is joined the
//! count is the lock's value. Left out: a failed `build` (`create`
//! returns before it moves the count), `build`'s spare storage
//! (`Arenas::spare`, swapped and stored only under the creation lock, so
//! ordered by it), `build`'s `set_multi_arena` (before the entry's store:
//! `flags.rs`), the automatic arenas' lazy build (here arena 0 is built
//! at the start, the only automatic one), and `Arenas::init`'s count,
//! written before the registry is published.

use crate::lock::RawLock;
use crate::sync::patomic::{PAtomicPtr, PAtomicU32};
use crate::vspec::{count, entry};
use crate::{leak, model};
use core::ptr;
use loom::cell::UnsafeCell;
use loom::thread;
use vstd::prelude::Tracked;

/// `MAX_ARENAS` for the model.
const MAX_ARENAS: u32 = 3;

/// The automatic arenas (`Arenas::auto`).
const AUTO: u32 = 1;

/// Where `build` stores the entry, against its write of the arena's
/// fields and the count's store.
#[derive(Clone, Copy)]
enum Publish {
    /// As shipped: the fields, the entry, then the count.
    Shipped,
    /// The control: the count stored before `build`.
    EarlyCount,
    /// The control: the entry stored before the fields are written.
    EarlyEntry,
}

struct Arenas {
    /// The creation lock (`Arenas::lock`, a `Lock<Created>`).
    lock: RawLock,
    /// The creation lock's value: the next index.
    next: UnsafeCell<u32>,
    /// `Arenas::count`.
    count: PAtomicU32,
    /// `Arenas::table` (a dangling non-null pointer stands for a built
    /// arena).
    table: [PAtomicPtr<u64>; MAX_ARENAS as usize],
    /// Each arena's fields, written by `build` before its entry.
    fields: [UnsafeCell<u64>; MAX_ARENAS as usize],
    /// The order of `build`'s steps.
    publish: Publish,
}

// SAFETY: `next` is touched only under `lock` and `fields[i]` before
// entry `i` is published, which is what is checked.
unsafe impl Sync for Arenas {}

impl Arenas {
    /// After `Arenas::init`: arena 0 built and published, the count and
    /// the lock's value `AUTO`.
    fn new(publish: Publish) -> Arenas {
        Arenas {
            lock: RawLock::new(),
            next: UnsafeCell::new(AUTO),
            count: PAtomicU32::new(AUTO).0,
            table: [
                PAtomicPtr::new(ptr::dangling_mut()).0,
                PAtomicPtr::new(ptr::null_mut()).0,
                PAtomicPtr::new(ptr::null_mut()).0,
            ],
            fields: [UnsafeCell::new(1), UnsafeCell::new(0), UnsafeCell::new(0)],
            publish,
        }
    }

    /// Entry `i` (`Arenas::get_existing`, `get`, `get_frozen` and
    /// `load_entry`, and `iter`, test code).
    fn entry(&self, i: usize) -> *mut u64 {
        entry::load(&self.table[i], Tracked::assume_new())
    }

    /// `Arenas::count`.
    fn count(&self) -> u32 {
        count::load(&self.count, Tracked::assume_new())
    }

    /// `Arenas::create`.
    fn create(&self) -> Option<u32> {
        self.lock.lock();
        // SAFETY: under the lock.
        let index = self.next.with(|p| unsafe { *p });
        // Under the lock the count and the table are frozen: a load reads
        // the lock's value and every arena below it built (what
        // `api::count_held` and `api::arena_held` prove in the SC model;
        // loads the model adds, not in the source).
        assert_eq!(
            self.count(),
            index,
            "the count under the lock is not its value"
        );
        for j in AUTO as usize..index as usize {
            assert!(
                !self.entry(j).is_null(),
                "arena {j} below the count under the lock is not built"
            );
        }
        if index >= MAX_ARENAS {
            self.lock.unlock();
            return None;
        }
        // `build`: `Arena::init`, then the entry.
        let i = index as usize;
        if matches!(self.publish, Publish::EarlyCount) {
            count::store(&self.count, index + 1, Tracked::assume_new());
        }
        if matches!(self.publish, Publish::EarlyEntry) {
            entry::store(&self.table[i], ptr::dangling_mut(), Tracked::assume_new());
        }
        // SAFETY: entry `i` is not published yet.
        self.fields[i].with_mut(|p| unsafe { *p = u64::from(index) + 1 });
        if !matches!(self.publish, Publish::EarlyEntry) {
            entry::store(&self.table[i], ptr::dangling_mut(), Tracked::assume_new());
        }
        // SAFETY: under the lock.
        self.next.with_mut(|p| unsafe { *p = index + 1 });
        if !matches!(self.publish, Publish::EarlyCount) {
            count::store(&self.count, index + 1, Tracked::assume_new());
        }
        self.lock.unlock();
        Some(index)
    }

    /// `Arenas::get` for an index known without the count (a
    /// `MALLOCX_ARENA` or `mallctl` index): every non-null entry is an
    /// arena whose fields read with no race (the entry's store orders
    /// `build`'s writes before them, with no help from the count).
    fn probe(&self) {
        for i in 0..MAX_ARENAS as usize {
            if !self.entry(i).is_null() {
                // SAFETY: entry `i` is published (loom reports a race if
                // the load did not order `build`'s write before this read).
                let f = self.fields[i].with(|p| unsafe { *p });
                assert_eq!(f, i as u64 + 1, "arena {i}'s fields");
            }
        }
    }

    /// The walk below the count, and the checks of the count around it.
    fn scan(&self) {
        let n = self.count();
        assert!((AUTO..=MAX_ARENAS).contains(&n), "count {n} out of range");
        for i in 0..n as usize {
            if self.entry(i).is_null() {
                assert!(
                    i < AUTO as usize,
                    "arena {i} below the count {n} is not built"
                );
                continue;
            }
            // SAFETY: entry `i` is published (loom reports a race if the
            // load did not order `build`'s write before this read).
            let f = self.fields[i].with(|p| unsafe { *p });
            assert_eq!(f, i as u64 + 1, "arena {i}'s fields");
        }
        let n2 = self.count();
        assert!(n2 <= MAX_ARENAS, "count {n2} out of range");
        assert!(n2 >= n, "the count went back from {n} to {n2}");
    }
}

/// Two threads each create an arena while the main thread probes and
/// scans the registry; then a third creation finds the table full.
fn run(publish: Publish) {
    model(move || {
        let a = leak(Arenas::new(publish));
        let x = thread::spawn(move || a.create());
        let y = thread::spawn(move || a.create());
        a.probe();
        a.scan();
        let (ix, iy) = (x.join().unwrap(), y.join().unwrap());
        assert!(ix.is_some() && iy.is_some() && ix != iy);
        assert_eq!(a.create(), None);
        // SAFETY: every thread is joined.
        let next = a.next.with(|p| unsafe { *p });
        assert_eq!(a.count(), next, "the count is not the lock's value");
        a.scan();
    });
}

/// The shipped order.
///
/// Runs `vspec::count::store`, `vspec::count::load`,
/// `vspec::entry::store` and `vspec::entry::load`.
#[test]
fn loom_narenas_create_publishes() {
    run(Publish::Shipped);
}

/// The scan's check has teeth: with the count stored before `build`
/// stores the entry, a scan can read a count whose last arena's entry is
/// still null.
#[test]
#[should_panic(expected = "is not built")]
fn loom_narenas_early_store_is_unpublished() {
    run(Publish::EarlyCount);
}

/// The probe's check has teeth: with the entry stored before `build`
/// writes the arena's fields, a reader that finds the entry reads the
/// fields racing with that write, which loom reports.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_narenas_early_entry_races() {
    run(Publish::EarlyEntry);
}
