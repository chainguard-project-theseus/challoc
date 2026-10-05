//! The arena registry's bound counts (`arena.rs`; `verus/THEOREMS.md`,
//! theorem 34): the real wrappers of `vspec/count.rs` (`fetch_add`,
//! `fetch_sub`, `load`) over loom's `AtomicU32`, driven by copies of the
//! `Arenas` steps that call them, with their orderings (all `Relaxed`):
//!
//! - [`Arenas::count_in`] is `Arenas::count_in` (and `bind`, its one-line
//!   caller): `fetch_add(1)`;
//! - [`Arenas::unbind`] is `Arenas::unbind`: `fetch_sub(1)`, and the
//!   `unbind_unbound` abort on a previous count of 0;
//! - [`Arenas::load_bound`] is `Arenas::load_bound` (`bound`): `load`;
//! - [`Arenas::assign`] is `Arenas::assign`: under the creation lock (the
//!   real `RawLock`), the first least `(bound, unbuilt)` key read, then
//!   `count_in` of it, still under the lock.
//!
//! A ticket (`arena::Homed`) is ghost; here a thread's tickets are what it
//! has counted in and not yet unbound. Checked: an `unbind` never reads a
//! count of 0 (its own ticket is counted), a holder's `load` never reads
//! less than its own tickets, and once every thread is joined each count
//! is the number of tickets still out. Left out: the arena table's
//! `Relaxed` pointer loads in `assign` (here a fixed built/unbuilt
//! pattern), building an arena, and the `u32` wrap (2^32 tickets).

use super::model;
use crate::lock::RawLock;
use crate::sync::atomic::{AtomicU32, Ordering};
use crate::vspec::count;
use loom::thread;
use std::boxed::Box;
use vstd::prelude::Tracked;

const NARENAS: usize = 2;

/// Whether each arena is built (`table[i]` not null), fixed for the model.
const BUILT: [bool; NARENAS] = [true, false];

struct Arenas {
    /// The creation lock.
    lock: RawLock,
    /// `Arenas::bound`.
    bound: [AtomicU32; NARENAS],
    /// The `fetch_add` of [`Arenas::count_in`] split into a load and a
    /// store, for the control.
    split: bool,
}

// `cfg(test)` again: `verus/seams.sh` skips test code only by the attribute
// on the item, and the permissions minted below are not seams.
#[cfg(test)]
impl Arenas {
    fn new(split: bool) -> Arenas {
        Arenas {
            lock: RawLock::new(),
            bound: [AtomicU32::new(0), AtomicU32::new(0)],
            split,
        }
    }

    /// `Arenas::load_bound`.
    fn load_bound(&self, i: usize) -> u32 {
        count::load(&self.bound[i], Ordering::Relaxed, Tracked::assume_new())
    }

    /// `Arenas::count_in`: a fresh ticket for arena `i`.
    fn count_in(&self, i: usize) {
        if self.split {
            let n = self.bound[i].load(Ordering::Relaxed);
            self.bound[i].store(n.wrapping_add(1), Ordering::Relaxed);
        } else {
            count::fetch_add(&self.bound[i], 1, Ordering::Relaxed, Tracked::assume_new());
        }
    }

    /// `Arenas::unbind`: the ticket for arena `i` goes back.
    fn unbind(&self, i: usize) {
        let prev = count::fetch_sub(&self.bound[i], 1, Ordering::Relaxed, Tracked::assume_new());
        assert!(
            prev != 0,
            "arena {i}: unbinding a heap that was never bound (count 0)"
        );
    }

    /// `Arenas::assign` (more than one automatic arena).
    fn assign(&self) -> usize {
        self.lock.lock();
        let mut best = 0usize;
        let mut best_key = (u32::MAX, true);
        for (i, built) in BUILT.iter().enumerate() {
            let key = (self.load_bound(i), !built);
            if key < best_key {
                best = i;
                best_key = key;
                if key == (0, false) {
                    break;
                }
            }
        }
        self.count_in(best);
        self.lock.unlock();
        best
    }
}

/// Leaked, as in `init.rs`: a failed model must not drop loom objects.
fn leak<T>(v: T) -> &'static T {
    Box::leak(Box::new(v))
}

/// The main thread's heap is homed on arena 0 (one ticket throughout) and
/// also counts a second heap in on arena 1 (`bind`) and out again; two new
/// threads are each assigned an arena and later exit (`unbind`).
fn run(split: bool) {
    model(move || {
        let a = leak(Arenas::new(split));
        a.count_in(0);
        let assigned: std::vec::Vec<_> = (0..2)
            .map(|_| {
                thread::spawn(move || {
                    let i = a.assign();
                    assert!(a.load_bound(i) >= 1, "a holder read its arena's count as 0");
                    a.unbind(i);
                })
            })
            .collect();
        a.count_in(1);
        assert!(a.load_bound(1) >= 1, "a holder read its arena's count as 0");
        a.unbind(1);
        assert!(a.load_bound(0) >= 1, "a holder read its arena's count as 0");
        for h in assigned {
            h.join().unwrap();
        }
        assert_eq!(a.load_bound(0), 1, "arena 0's count is not its tickets");
        assert_eq!(a.load_bound(1), 0, "arena 1's count is not its tickets");
    });
}

/// The shipped orderings: every count stays the number of tickets out.
#[test]
fn loom_count_bind_assign_unbind() {
    run(false);
}

/// The check has teeth: a count bumped by a load and a store instead of
/// one read-modify-write loses a ticket, which one of the three count
/// checks reports (each message names the count; which one fires first
/// depends on the execution).
#[test]
#[should_panic(expected = "count")]
fn loom_count_split_rmw_loses_a_ticket() {
    run(true);
}
