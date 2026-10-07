//! The arena registry's arena count and table (`arena.rs`,
//! `Arenas::count`, `Arenas::table`): the real wrappers of
//! `vspec/count.rs` (`store`, `load`) over loom's `AtomicU32` and of
//! `vspec/entry.rs` (`store`, `load`) over loom's `AtomicPtr`, driven by
//! copies of the `Arenas` steps that call them, with their orderings:
//!
//! - [`Arenas::create`] is `Arenas::create`: under the creation lock (the
//!   real `RawLock`; the lock's value, the next index, a loom
//!   `UnsafeCell` beside it), `None` at `index >= MAX_ARENAS`, then
//!   `build` (the arena's fields, here a loom `UnsafeCell`, then the
//!   table entry stored `Release` with `entry::store`), the lock's value
//!   set to `index + 1`, and `count::store(index + 1, Release)`, still
//!   under the lock;
//! - [`Arenas::count`] is `Arenas::count`: `count::load(Acquire)`;
//! - [`Arenas::scan`] is `Arenas::iter`: the indices below the count,
//!   each entry loaded `Acquire` (`entry::load`) and skipped when null.
//!
//! Checked: every load of the count returns at least the automatic count
//! and at most `MAX_ARENAS` (what `NarenasPred` bounds); a thread's loads
//! never go back (read-read coherence); a scan finds every explicit arena
//! below the count it read built, and reads its fields with no race; a
//! `create` holding the lock loads the count as the lock's value and
//! finds every explicit arena below it built (the frozen table that
//! `api::count_held` and `api::arena_held` prove from the creation
//! lock's ghost half, `arena::Created`); and once every
//! thread is joined the count is the lock's value. Left out: a failed
//! `build` (`create` returns before it moves the count), `build`'s
//! spare storage (`Arenas::spare`, swapped and stored only under the
//! creation lock, so ordered by it), `build`'s `set_multi_arena` (a
//! `Relaxed` flag, before the entry's store), the automatic arenas'
//! lazy build (here arena 0 is built at the start, the only automatic
//! one), and `Arenas::init`'s count, written before the registry is
//! published.

use super::model;
use crate::lock::RawLock;
use crate::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use crate::vspec::{count, entry};
use core::ptr;
use loom::cell::UnsafeCell;
use loom::thread;
use std::boxed::Box;
use vstd::prelude::Tracked;

/// `MAX_ARENAS` for the model.
const MAX_ARENAS: u32 = 3;

/// The automatic arenas (`Arenas::auto`).
const AUTO: u32 = 1;

struct Arenas {
    /// The creation lock (`Arenas::lock`, a `Lock<Created>`).
    lock: RawLock,
    /// The creation lock's value: the next index.
    next: UnsafeCell<u32>,
    /// `Arenas::count`.
    count: AtomicU32,
    /// `Arenas::table`, through the real wrappers of `vspec/entry.rs`
    /// (a dangling non-null pointer stands for a built arena).
    table: [AtomicPtr<u64>; MAX_ARENAS as usize],
    /// Each arena's fields, written by `build` before its entry.
    fields: [UnsafeCell<u64>; MAX_ARENAS as usize],
    /// The ordering of the count's store (`Release` as shipped).
    publish: Ordering,
    /// The ordering of the entry's store (`Release` as shipped).
    entry_order: Ordering,
}

// SAFETY: `next` is touched only under `lock` and `fields[i]` before
// entry `i` is published, which is what is checked.
unsafe impl Sync for Arenas {}

// `cfg(test)` again: `verus/seams.sh` skips test code only by the
// attribute on the item, and the permissions minted below are not seams.
#[cfg(test)]
impl Arenas {
    /// After `Arenas::init`: arena 0 built and published, the count and
    /// the lock's value `AUTO`.
    fn new(publish: Ordering, entry_order: Ordering) -> Arenas {
        Arenas {
            lock: RawLock::new(),
            next: UnsafeCell::new(AUTO),
            count: AtomicU32::new(AUTO),
            table: [
                AtomicPtr::new(ptr::dangling_mut()),
                AtomicPtr::new(ptr::null_mut()),
                AtomicPtr::new(ptr::null_mut()),
            ],
            fields: [UnsafeCell::new(1), UnsafeCell::new(0), UnsafeCell::new(0)],
            publish,
            entry_order,
        }
    }

    /// Entry `i`, loaded `Acquire` (`Arenas::get` and `get_frozen`; the
    /// plain load of `get_existing` and `iter` is the same operation).
    fn entry(&self, i: usize) -> *mut u64 {
        entry::load(&self.table[i], Ordering::Acquire, Tracked::assume_new())
    }

    /// `Arenas::count`.
    fn count(&self) -> u32 {
        count::load(&self.count, Ordering::Acquire, Tracked::assume_new())
    }

    /// `Arenas::create`.
    fn create(&self) -> Option<u32> {
        self.lock.lock();
        // SAFETY: under the lock.
        let index = self.next.with(|p| unsafe { *p });
        // Under the lock the count and the table are frozen: a load reads
        // the lock's value and every arena below it built (what
        // `api::count_held` and `api::arena_held` prove in the SC model).
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
        // SAFETY: entry `i` is not published yet.
        self.fields[i].with_mut(|p| unsafe { *p = u64::from(index) + 1 });
        entry::store(
            &self.table[i],
            ptr::dangling_mut(),
            self.entry_order,
            Tracked::assume_new(),
        );
        // SAFETY: under the lock.
        self.next.with_mut(|p| unsafe { *p = index + 1 });
        count::store(&self.count, index + 1, self.publish, Tracked::assume_new());
        self.lock.unlock();
        Some(index)
    }

    /// `Arenas::get` for an index known without the count (a `MALLOCX_ARENA`
    /// or `mallctl` index): every non-null entry, loaded `Acquire`, is an
    /// arena whose fields read with no race (the entry's `Release` store
    /// orders `build`'s writes before them, with no help from the count).
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

    /// `Arenas::iter` and the checks of the count around it.
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

/// Leaked, as in `init.rs`: a failed model must not drop loom objects.
fn leak<T>(v: T) -> &'static T {
    Box::leak(Box::new(v))
}

/// Two threads each create an arena while the main thread probes and scans the
/// registry; then a third creation finds the table full.
fn run(publish: Ordering, entry_order: Ordering) {
    model(move || {
        let a = leak(Arenas::new(publish, entry_order));
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

/// The shipped orderings.
#[test]
fn loom_narenas_create_publishes() {
    run(Ordering::Release, Ordering::Release);
}

/// The scan's check has teeth: with the count stored `Relaxed`, a scan
/// can read a count whose last arena's entry is still null.
#[test]
#[should_panic(expected = "is not built")]
fn loom_narenas_relaxed_store_is_unpublished() {
    run(Ordering::Relaxed, Ordering::Release);
}

/// The probe's check has teeth: with the entry stored `Relaxed`, a reader
/// that finds the entry without the count reads the arena's fields racing
/// with `build`'s writes, which loom reports.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_narenas_relaxed_entry_races() {
    run(Ordering::Release, Ordering::Relaxed);
}
