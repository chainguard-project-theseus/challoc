//! The arena registry's bound counts (`src/arena.rs`, `Arenas::bound`), a
//! copy of the `Arenas` steps that touch them through `vspec/count.rs`'s
//! wrappers (`fetch_add`, `fetch_sub`, `load`, `compare_exchange`), which
//! run here as they are, over loom's `AtomicU32` (`crate::vspec`), with
//! the steps' orderings (all `Relaxed`):
//!
//! - [`Arenas::count_in`] is `Arenas::count_in` (and `bind`, its one-line
//!   caller): `fetch_add(1)`;
//! - [`Arenas::unbind`] is `Arenas::unbind`: `fetch_sub(1)`, and the
//!   `unbind_unbound` abort on a previous count of 0;
//! - [`Arenas::load_bound`] is `Arenas::load_bound` (`bound`): `load`;
//! - [`Arenas::assign`] is `Arenas::assign`: under the creation lock (the
//!   copy of `RawLock`), the first least `(bound, unbuilt)` key read
//!   (`key_less`), then `count_in` of it, still under the lock;
//! - [`Arenas::try_bind_idle`] is `Arenas::try_bind_idle` (and
//!   `bind_if_idle`, its tail; `Heap::try_rehome` calls it without a
//!   lock): a `load`, and on 0 the strong `compare_exchange(0, 1)`, a
//!   fresh ticket only when it succeeds.
//!
//! A ticket (`arena::Homed`) is ghost; here a thread's tickets are what it
//! has counted in and not yet unbound. Checked: an `unbind` never reads a
//! count of 0 (its own ticket is counted), a holder's `load` never reads
//! less than its own tickets, and once every thread is joined each count
//! is the number of tickets still out; and (`run_idle`) a
//! `try_bind_idle` wins only on an arena with no ticket out, so two
//! racing for one idle arena never both win, and of two racing alone
//! for an idle arena exactly one wins. Left out: the arena table's
//! `Relaxed` pointer loads in `assign` (here a fixed built/unbuilt
//! pattern), building an arena, and the `u32` wrap (2^32 tickets).

use crate::lock::RawLock;
use crate::vspec::count;
use crate::{leak, model};
use loom::sync::atomic::{AtomicU32, Ordering};
use loom::thread;
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
    /// The `compare_exchange` of [`Arenas::try_bind_idle`] split into a
    /// load and a store of 1 on 0, for its control.
    split_cas: bool,
}

impl Arenas {
    fn new(split: bool, split_cas: bool) -> Arenas {
        Arenas {
            lock: RawLock::new(),
            bound: [AtomicU32::new(0), AtomicU32::new(0)],
            split,
            split_cas,
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

    /// `Arenas::try_bind_idle` and `bind_if_idle`: a fresh ticket for
    /// arena `i` only if none is out (no lock, as `Heap::try_rehome`
    /// calls it).
    fn try_bind_idle(&self, i: usize) -> bool {
        if self.load_bound(i) != 0 {
            return false;
        }
        if self.split_cas {
            if self.bound[i].load(Ordering::Relaxed) != 0 {
                return false;
            }
            self.bound[i].store(1, Ordering::Relaxed);
            true
        } else {
            count::compare_exchange(
                &self.bound[i],
                0,
                1,
                Ordering::Relaxed,
                Ordering::Relaxed,
                Tracked::assume_new(),
            )
            .is_ok()
        }
    }
}

/// The main thread's heap is homed on arena 0 (one ticket throughout) and
/// also counts a second heap in on arena 1 (`bind`) and out again; two new
/// threads are each assigned an arena and later exit (`unbind`).
fn run(split: bool) {
    model(move || {
        let a = leak(Arenas::new(split, false));
        a.count_in(0);
        let assigned: Vec<_> = (0..2)
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
/// About six minutes in release, most of a full loom run, so it is
/// ignored here and the `loom-count` check (`.chainguard/ci/lint.yaml`)
/// runs it with `--ignored` on changes to this model or the code it
/// copies: `sh tests/loom/run.sh -- --ignored`.
///
/// Runs `vspec::count::fetch_add`, `vspec::count::fetch_sub` and
/// `vspec::count::load`.
#[test]
#[ignore = "slow: the loom-count check runs it (sh tests/loom/run.sh -- --ignored)"]
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

/// The main thread's heap is homed on arena 0 (one ticket throughout);
/// two new threads race to move a heap onto the idle arena 1
/// (`try_bind_idle`), each keeping a ticket it wins until it is joined;
/// with `rival`, the main thread meanwhile counts a heap in on arena 1
/// (`bind`) and out again.
fn run_idle(split_cas: bool, rival: bool) {
    model(move || {
        let a = leak(Arenas::new(false, split_cas));
        a.count_in(0);
        let racers: Vec<_> = (0..2)
            .map(|_| {
                thread::spawn(move || {
                    let won = a.try_bind_idle(1);
                    if won {
                        assert!(a.load_bound(1) >= 1, "a holder read its arena's count as 0");
                    }
                    won
                })
            })
            .collect();
        if rival {
            a.count_in(1);
            assert!(a.load_bound(1) >= 1, "a holder read its arena's count as 0");
            a.unbind(1);
        }
        let mut won = 0u32;
        for h in racers {
            if h.join().unwrap() {
                won += 1;
            }
        }
        assert!(
            won <= 1,
            "two heaps bound to one idle arena (count 0 to 1 twice)"
        );
        if !rival {
            assert_eq!(
                won, 1,
                "no heap bound the idle arena (its count compare-and-swap failed)"
            );
        }
        assert_eq!(a.load_bound(0), 1, "arena 0's count is not its tickets");
        assert_eq!(a.load_bound(1), won, "arena 1's count is not its tickets");
    });
}

/// The shipped `try_bind_idle`, against a rival `bind` and `unbind`: at
/// most one heap wins the arena, and every count stays the number of
/// tickets out.
///
/// Runs `vspec::count::compare_exchange`,
/// `vspec::count::fetch_add`, `vspec::count::fetch_sub` and
/// `vspec::count::load`.
#[test]
fn loom_count_try_bind_idle_rival() {
    run_idle(false, true);
}

/// The shipped `try_bind_idle`, two racers alone: exactly one wins.
/// loom's compare-and-swap never fails spuriously (its weak form is its
/// strong one), so this checks the copy's use of it, not that the
/// strong form has no spurious failure.
///
/// Runs `vspec::count::compare_exchange` and
/// `vspec::count::load`.
#[test]
fn loom_count_try_bind_idle_one_wins() {
    run_idle(false, false);
}

/// The check has teeth: a compare-and-swap split into a load and a store
/// lets both racers read 0 and win, which the winner check reports.
#[test]
#[should_panic(expected = "two heaps bound")]
fn loom_count_split_cas_binds_twice() {
    run_idle(true, false);
}

/// As [`loom_count_split_cas_binds_twice`], against a rival: both racers
/// can still read 0 and win (a racer's store of 1 can also overwrite the
/// rival's ticket; the first failing execution loom reaches is a double
/// win).
#[test]
#[should_panic(expected = "two heaps bound")]
fn loom_count_split_cas_binds_twice_rival() {
    run_idle(true, true);
}
