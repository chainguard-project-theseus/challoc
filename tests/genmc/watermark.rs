// GenMC port of tests/loom/src/watermark.rs: the class table's hand-out
// watermark (`SlabTable::committed_slabs`) and the class line's copy.
// challoc's `vspec/atom.rs` wrappers (`store_usize`, `load_usize`,
// `load_usize_seen`, `load_u16_at`, `store_usize_at`, `usize_ptr`) run
// unchanged over `core` atomics; `take_group`, `committed_slabs`,
// `watermark`, `mirror_watermark` and `ClassLine::set_slab`'s load are
// copies, as in the loom model. The commit lock is genmc-std's `Mutex`
// (GenMC's own lock), not the `RawLock` copy: GenMC has no futex.
//
// The seam ledger cites this template's pass cases (verus/SEAMS.md,
// "Status") for `atom::store_usize`, `atom::load_usize`,
// `atom::load_usize_seen`, `atom::load_u16_at` and `atom::store_usize_at`,
// whose bodies run, and for `table::store_owner` (`src/class/table.rs`),
// whose owner-word store `take_group` copies onto `core`'s atomic. It is
// evidence only: the permissions are erased (`Tracked::assume_new()`, and
// an erased invariant for `load_usize_seen`), so no case checks a seam's
// contract.

use crate::vspec::atom;
use core::cell::UnsafeCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::thread;
use vstd::prelude::Tracked;

struct Table {
    lock: Mutex<()>,
    next: UnsafeCell<usize>,
    committed: AtomicUsize,
    owners: [AtomicU16; NSLABS],
    mirror: AtomicUsize,
    installed: UnsafeCell<bool>,
}
// SAFETY: `next` and `installed` are touched only under `lock` (GenMC
// reports a race if not).
unsafe impl Sync for Table {}

static T: Table = Table {
    lock: Mutex::new(()),
    next: UnsafeCell::new(0),
    committed: AtomicUsize::new(0),
    owners: [const { AtomicU16::new(0) }; NSLABS],
    mirror: AtomicUsize::new(0),
    installed: UnsafeCell::new(false),
};

impl Table {
    /// `SlabTable::committed_slabs` (`Acquire`) and `SlabTable::watermark`.
    fn watermark(&self, order: Ordering) -> usize {
        if order == Ordering::Acquire {
            vspec::load_usize_seen(&self.committed, order)
        } else {
            atom::load_usize(&self.committed, order, Tracked::assume_new())
        }
    }

    /// `SlabTable::take_group` for `arena`, one slab per group.
    fn take_group(&self, arena: u16) -> Option<usize> {
        let g = self.lock.lock().unwrap();
        // SAFETY: under the lock.
        let s = unsafe { *self.next.get() };
        if s >= NSLABS {
            drop(g);
            return None;
        }
        let end = s + 1;
        gassert!(
            s == self.watermark(Ordering::Relaxed),
            "check_zero: the watermark differs from next_slab"
        );
        self.owners[s].store(arena, Ordering::Relaxed);
        // SAFETY: under the lock.
        unsafe { *self.next.get() = end };
        atom::store_usize(&self.committed, end, PUBLISH, Tracked::assume_new());
        // SAFETY: under the lock.
        let installed = unsafe { *self.installed.get() };
        if installed {
            gassert!(
                self.mirror.load(Ordering::Relaxed) == s,
                "take_group: the copy differs from next_slab"
            );
        }
        if installed && !MIRROR_UNLOCKED {
            self.store_mirror(end);
        }
        drop(g);
        if installed && MIRROR_UNLOCKED {
            self.store_mirror(end);
        }
        Some(s)
    }

    fn store_mirror(&self, v: usize) {
        // SAFETY: `mirror` is a live atomic in the static table.
        unsafe {
            atom::store_usize_at(
                atom::usize_ptr(&self.mirror),
                v,
                Ordering::Release,
                Tracked::assume_new(),
            )
        }
    }

    /// `SlabTable::mirror_watermark`.
    fn mirror_watermark(&self) {
        let g = self.lock.lock().unwrap();
        // SAFETY: under the lock.
        let wm = unsafe { *self.next.get() };
        self.store_mirror(wm);
        // SAFETY: under the lock.
        unsafe { *self.installed.get() = true };
        drop(g);
    }

    /// `ClassLine::set_slab`'s `Relaxed` load of the copy.
    fn read_mirror(&self) -> usize {
        let m = atom::load_usize(&self.mirror, Ordering::Relaxed, Tracked::assume_new());
        gassert!(m <= NSLABS, "copy above the slab count");
        m
    }

    /// A reader: `Acquire` then `Relaxed`, and the owners below the first.
    fn read(&self) {
        let w = self.watermark(Ordering::Acquire);
        gassert!(w <= NSLABS, "watermark above the slab count");
        for slab in 0..w {
            // SAFETY: the owner word is a live atomic in the static table.
            let owner = unsafe {
                atom::load_u16_at(&self.owners[slab], Ordering::Relaxed, Tracked::assume_new())
            };
            gassert!(owner != 0, "a slab below an Acquire watermark has no owner");
        }
        let w2 = self.watermark(Ordering::Relaxed);
        gassert!(w2 <= NSLABS, "watermark above the slab count");
        gassert!(w2 >= w, "the watermark went back");
    }
}

/// `HANDOUTS` threads each hand out a group while the main thread reads;
/// then one more hand-out finds the region full (loom: 2 slabs, 2 threads).
fn run() {
    let t: &'static Table = &T;
    let a = thread::spawn(move || t.take_group(1));
    let b = thread::spawn(move || t.take_group(2));
    let c = if HANDOUTS > 2 {
        Some(thread::spawn(move || t.take_group(3)))
    } else {
        None
    };
    t.read();
    let (sa, sb) = (a.join().unwrap(), b.join().unwrap());
    gassert!(
        sa.is_some() && sb.is_some() && sa != sb,
        "two hand-outs got one group"
    );
    if let Some(c) = c {
        let sc = c.join().unwrap();
        gassert!(
            sc.is_some() && sc != sa && sc != sb,
            "three hand-outs got one group"
        );
    }
    gassert!(t.take_group(9).is_none(), "a hand-out past the slab count");
    gassert!(
        t.watermark(Ordering::Relaxed) == NSLABS,
        "the watermark is not the slab count"
    );
    t.read();
}

/// loom's `run_mirror`. `MODE` 1: install first, two hand-outs race the
/// reader. 2: one hand-out races the install. 3 (loom cannot exhaust
/// it: three lock holders): the install races two hand-outs.
fn run_mirror() {
    let t: &'static Table = &T;
    if MODE == 1 {
        t.mirror_watermark();
    }
    let a = thread::spawn(move || t.take_group(1));
    let b = if MODE != 2 {
        Some(thread::spawn(move || t.take_group(2)))
    } else {
        None
    };
    let inst = if MODE == 3 {
        Some(thread::spawn(move || t.mirror_watermark()))
    } else {
        None
    };
    if MODE == 2 {
        t.mirror_watermark();
    }
    let m = t.read_mirror();
    let m2 = t.read_mirror();
    gassert!(m2 >= m, "the copy went back");
    let sa = a.join().unwrap();
    let sb = match b {
        Some(b) => b.join().unwrap(),
        None => t.take_group(2),
    };
    if let Some(i) = inst {
        i.join().unwrap();
    }
    gassert!(
        sa.is_some() && sb.is_some() && sa != sb,
        "two hand-outs got one group"
    );
    gassert!(t.read_mirror() >= m2, "the copy went back");
    gassert!(t.take_group(3).is_none(), "a hand-out past the slab count");
    gassert!(t.read_mirror() == NSLABS, "the copy is not the slab count");
}

fn harness() {
    if MODE == 0 { run() } else { run_mirror() }
}
