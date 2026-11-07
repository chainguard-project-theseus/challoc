// GenMC port of tests/loom/src/narenas.rs: the arena registry's arena
// count and table (`src/arena.rs`, `Arenas::count`, `Arenas::table`).
// challoc's `vspec/count.rs` wrappers (`store`, `load`, over the
// `AtomicU32` count) and `vspec/entry.rs`'s (`store`, `load`, over the
// `AtomicPtr` entries) run unchanged over `core` atomics;
// `Arenas::create`, `count`, `get_existing`'s load and the scan below the
// count are copies, as in the loom model, with the source's orderings.
// The creation lock is genmc-std's `Mutex`, not the `RawLock` copy:
// GenMC has no futex.
//
// Two threads each create an arena while the main thread probes and
// scans the registry; then a third creation finds the table full.
// `PUBLISH` and `ENTRY` are the orderings of the count's and the entry's
// stores (`Release` as shipped; loom's controls make each `Relaxed`).
// Each arena's fields are a plain value `build` writes before the entry,
// so a reader that finds the entry without the writes ordered before it
// is a non-atomic race. The readers' plain reads are volatile: at
// `-Copt-level=1` LLVM may hoist a plain read of a static above the
// null test that guards it, a race the source does not have.
//
// The seam ledger cites this template's pass cases (verus/SEAMS.md,
// "Status") for `count::store`, `count::load`, `entry::store` and
// `entry::load`, as evidence only: the permissions are erased
// (`Tracked::assume_new()`), so no case checks a wrapper's contract.

use crate::vspec::{count, entry};
use core::cell::UnsafeCell;
use core::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::thread;
use vstd::prelude::Tracked;

// `MAX_ARENAS` for the model.
const MAX_ARENAS: u32 = 3;

// The automatic arenas (`Arenas::auto`).
const AUTO: u32 = 1;

struct Arenas {
    lock: Mutex<()>,
    // The creation lock's value: the next index.
    next: UnsafeCell<u32>,
    count: AtomicU32,
    // A non-null entry stands for a built arena.
    table: [AtomicPtr<u64>; MAX_ARENAS as usize],
    fields: [UnsafeCell<u64>; MAX_ARENAS as usize],
}

// SAFETY: `next` is touched only under `lock` and `fields[i]` before
// entry `i` is published, which is what is checked.
unsafe impl Sync for Arenas {}

// After `Arenas::init`: arena 0 built and published, the count and the
// lock's value `AUTO`.
static A: Arenas = Arenas {
    lock: Mutex::new(()),
    next: UnsafeCell::new(AUTO),
    count: AtomicU32::new(AUTO),
    table: [
        AtomicPtr::new(ptr::dangling_mut()),
        AtomicPtr::new(ptr::null_mut()),
        AtomicPtr::new(ptr::null_mut()),
    ],
    fields: [UnsafeCell::new(1), UnsafeCell::new(0), UnsafeCell::new(0)],
};

impl Arenas {
    // Entry `i`, loaded `Acquire` (`Arenas::get_existing`, `get`,
    // `get_frozen`).
    fn entry(&self, i: usize) -> *mut u64 {
        entry::load(&self.table[i], Ordering::Acquire, Tracked::assume_new())
    }

    // `Arenas::count`.
    fn count(&self) -> u32 {
        count::load(&self.count, Ordering::Acquire, Tracked::assume_new())
    }

    fn field(&self, i: usize) -> u64 {
        // SAFETY: entry `i` is published (GenMC reports a race if the
        // load did not order `build`'s write before this read).
        unsafe { ptr::read_volatile(self.fields[i].get()) }
    }

    // `Arenas::create`.
    fn create(&self) -> Option<u32> {
        let g = self.lock.lock().unwrap();
        // SAFETY: under the lock.
        let index = unsafe { *self.next.get() };
        // Under the lock the count and the table are frozen (loads the
        // model adds, not in the source).
        gassert!(
            self.count() == index,
            "the count under the lock is not its value"
        );
        for j in AUTO as usize..index as usize {
            gassert!(
                !self.entry(j).is_null(),
                "an arena below the count under the lock is not built"
            );
        }
        if index >= MAX_ARENAS {
            drop(g);
            return None;
        }
        // `build`: `Arena::init`, then the entry.
        let i = index as usize;
        // SAFETY: entry `i` is not published yet.
        unsafe { *self.fields[i].get() = u64::from(index) + 1 };
        entry::store(
            &self.table[i],
            ptr::dangling_mut(),
            ENTRY,
            Tracked::assume_new(),
        );
        // SAFETY: under the lock.
        unsafe { *self.next.get() = index + 1 };
        count::store(&self.count, index + 1, PUBLISH, Tracked::assume_new());
        drop(g);
        Some(index)
    }

    // `Arenas::get` for an index known without the count: every non-null
    // entry is an arena whose fields read with no race.
    fn probe(&self) {
        for i in 0..MAX_ARENAS as usize {
            if !self.entry(i).is_null() {
                gassert!(self.field(i) == i as u64 + 1, "an arena's fields");
            }
        }
    }

    // The walk below the count, and the checks of the count around it.
    fn scan(&self) {
        let n = self.count();
        gassert!(AUTO <= n && n <= MAX_ARENAS, "count out of range");
        for i in 0..n as usize {
            if self.entry(i).is_null() {
                gassert!(i < AUTO as usize, "an arena below the count is not built");
                continue;
            }
            gassert!(self.field(i) == i as u64 + 1, "an arena's fields");
        }
        let n2 = self.count();
        gassert!(n2 <= MAX_ARENAS, "count out of range");
        gassert!(n2 >= n, "the count went back");
    }
}

fn harness() {
    let a: &'static Arenas = &A;
    let x = thread::spawn(move || a.create());
    let y = thread::spawn(move || a.create());
    a.probe();
    a.scan();
    let (ix, iy) = (x.join().unwrap(), y.join().unwrap());
    gassert!(
        ix.is_some() && iy.is_some() && ix != iy,
        "two creations got one index"
    );
    gassert!(a.create().is_none(), "a creation past MAX_ARENAS");
    // SAFETY: every thread is joined.
    let next = unsafe { *a.next.get() };
    gassert!(a.count() == next, "the count is not the lock's value");
    a.scan();
}
