//! An exhaustive interleaving model of the slab protocol (design.md
//! section 17, the "model-checked protocol" row; plan Phase 2.5 task 13).
//! Test-only: `cargo test --features std model::`.
//!
//! What is modelled. One class, [`NSLABS`] slabs of [`NSLOTS`] slots each
//! (the bits of a slab fit one word, as every real slab's do), one or two
//! arenas, two or three threads, and the shared state the protocol of
//! `class/` runs on: per slab the state byte, the `live` count, the
//! slot bitmap word, the descriptor's arena field and the slot memory
//! (one bit per slot: zero or dirty); per arena the class lock and the
//! `partial`, `empty` and `purged` sets and the group cursor; the owner
//! word per group and the class-wide next fresh group. Every thread runs
//! a short program of operations, each split into the atomic steps of the
//! real code in the real order (the line of `class/` each step mirrors
//! is quoted at the step), and [`explore`] enumerates every interleaving
//! of those steps depth first, memoising on the whole state, and checks
//! the invariants of [`check`] at every state. A violation returns the
//! interleaving that reached it.
//!
//! Operations: `Refill(n)` is `ClassState::claim` (`plan` under the lock:
//! the slab selection, `take`, the zero flag and, with the fix of plan
//! Phase 1.5 task 7 (c), `claim_prefix`; then `finish` outside the lock:
//! the `fetch_or` bit loop of an `Active` pick, one atomic per word);
//! `Write(i)` dirties the `i`th slot the thread holds; `Release` gives
//! every held slot back (`release_owned`: `fetch_and` per slab, then
//! `dec_live`, whose `sub_live` reaching zero runs `on_live_zero` under
//! the lock, then `reopen` under the lock for the slabs whose word was
//! full); `Purge` is `purge_empty` (every `Empty` slab of the arena to
//! `Purged` and its memory to zero, under the lock).
//!
//! Abstractions, stated: a lock hold that only reads and writes
//! lock-protected state is one atomic step (the sets, the cursors, the
//! state byte, which is written under the lock only); the lock-free
//! atomics (`fetch_or`, `fetch_and`, `sub_live`, the memory write) are
//! one step each and interleave with everything, including another
//! thread's lock hold, as they do in the real code; `add_live` and the
//! zero pick's prefix `fetch_or`s run inside `take`'s lock hold and are
//! folded into that step (a concurrent `sub_live` between the `live()`
//! read and the `fetch_add` changes nothing the code relies on, and the
//! prefix bits of a `Fresh`/`Purged` slab have no contender). Guard slabs,
//! page groups larger than a slab (`spp == 1`), `slot_randomize`, the
//! thread cache above `claim`/`release` and the decay curve are not
//! modelled: the model is the protocol of the sets, the bits, `live` and
//! the state byte, which is what the calloc race was about.
//!
//! One knob replays the protocol before its fix ([`Variant`]):
//! `prefix_claim: false` is the pre-7 (c) order (the zero flag decided in
//! `plan`, the bits claimed in `finish`), and the model finds the calloc
//! race (a slot reported zero whose memory another thread dirtied in the
//! window) and prints the interleaving. With it on, every scenario
//! passes.
//!
//! A change to the protocol in `class/` is a change to a step here.
//!
//! Layout: `state` holds the modelled state (slabs, arenas, the
//! per-thread machine) and its small helpers; `steps` the step function
//! of the operations, the invariants and the depth-first exploration;
//! `scenarios` the knob ([`Variant`]) and the named scenarios. This file keeps
//! the geometry constants, the outcome types, [`explore`] and the tests.

mod scenarios;
mod state;
mod steps;

pub use scenarios::*;
use state::*;

use std::collections::HashSet;
use std::string::String;
use std::time::Instant;
use std::vec::Vec;
use std::{eprintln, format, vec};

/// Slabs in the model. Two groups of one slab (the small classes' real
/// geometry, `slabs_per_line == 1`) when [`Scenario::lg_group`] is 0, one
/// group of two when it is 1.
pub const NSLABS: usize = 2;
/// Slots per slab; the slab's bits are the low `NSLOTS` bits of one word.
pub const NSLOTS: u8 = 4;
const FULL: u8 = (1 << NSLOTS) - 1;
const NARENAS: usize = 2;
const MAX_THREADS: usize = 3;
const MAX_HELD: usize = 4;
/// Picks per plan: the real bound is `class::MAX_PICKS`; the model's
/// smaller one is enough for [`NSLABS`] slabs and must not exceed it.
const MAX_PICKS: usize = 2;
const _: () = assert!(MAX_PICKS <= crate::class::MAX_PICKS);
const MAX_OPS: usize = 6;
/// States one exploration may visit before it gives up: a scenario past
/// this is too large for the 30 s budget of the unit suite and must be
/// cut, not waited for.
const MAX_STATES: usize = 5_000_000;

/// A step taken: thread, operation, sub-step, and what it did.
#[derive(Clone, Debug)]
pub struct Trace(pub Vec<(u8, Op, u8, String)>);

/// What [`explore`] found.
#[derive(Debug)]
pub struct Outcome {
    pub states: usize,
    pub violation: Option<(String, Trace)>,
}

/// The exploration in progress: the scenario, the knobs, the states
/// seen and the path to the current one.
struct Model<'a> {
    pub(super) sc: &'a Scenario,
    pub(super) v: Variant,
    pub(super) seen: HashSet<Node>,
    pub(super) trace: Vec<(u8, Op, u8, String)>,
}

/// Run `sc` under `v`: every interleaving, every state checked. The
/// exploration is depth first and recursive, one frame per step of the
/// path (a `Node` clone each), so it runs on its own thread with a 1 GiB
/// stack (reserved, not touched) rather than the 2 MiB of a test thread.
pub fn explore(sc: &Scenario, v: Variant) -> Outcome {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(scope, || explore_here(sc, v))
            .expect("model: spawn the exploration thread")
            .join()
            .expect("model: the exploration thread panicked")
    })
}

fn explore_here(sc: &Scenario, v: Variant) -> Outcome {
    assert!(sc.threads.len() <= MAX_THREADS);
    for (_, p) in &sc.threads {
        assert!(p.len() <= MAX_OPS);
    }
    let shared = Shared {
        lock: [None; NARENAS],
        slabs: [Slab {
            state: St::Fresh,
            live: 0,
            bits: 0,
            mem: 0,
            arena: 0,
            committed: false,
        }; NSLABS],
        partial: [0; NARENAS],
        empty: [0; NARENAS],
        purged: [0; NARENAS],
        owner: [NARENAS as u8; NSLABS],
        group_next: [0; NARENAS],
        group_end: [0; NARENAS],
        next_group: 0,
    };
    let threads = sc.threads.iter().map(|(a, _)| Thread::new(*a)).collect();
    let mut m = Model {
        sc,
        v,
        seen: HashSet::new(),
        trace: Vec::new(),
    };
    let violation = m.explore(Node { shared, threads }).err();
    Outcome {
        states: m.seen.len(),
        violation,
    }
}

impl Trace {
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, (t, op, sub, desc)) in self.0.iter().enumerate() {
            out += &format!("  {i:>2}. T{t} {op:?}[{sub}]: {desc}\n");
        }
        out
    }
}

fn run(sc: &Scenario, v: Variant) -> Outcome {
    let start = Instant::now();
    let out = explore(sc, v);
    eprintln!(
        "model: {} under {v:?}: {} states in {:.2} s, {}",
        sc.name,
        out.states,
        start.elapsed().as_secs_f64(),
        match &out.violation {
            None => String::from("no violation"),
            Some((m, tr)) => format!("VIOLATION: {m}\n{}", tr.render()),
        }
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pre_fix_order_has_the_calloc_race_and_the_model_prints_it() {
        let out = run(
            &calloc_race(),
            Variant {
                prefix_claim: false,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("the pre-fix protocol must show the race");
        assert!(
            msg.contains("reported zero") && msg.contains("dirty"),
            "{msg}"
        );
        // The interleaving is the campaign's mechanism: A's plan flags a
        // zero pick and drops the lock; B claims the slot, writes it and
        // releases it; A's bit loop then takes the same slot.
        let steps: Vec<&(u8, Op, u8, String)> = tr.0.iter().collect();
        let a_plan = steps
            .iter()
            .position(|s| s.0 == 0 && s.3.contains("zero pick"))
            .expect("A plans a zero pick");
        let b_write = steps
            .iter()
            .position(|s| s.0 == 1 && matches!(s.1, Op::Write(_)))
            .expect("B writes");
        let b_release = steps
            .iter()
            .position(|s| s.0 == 1 && s.3.starts_with("release: fetch_and"))
            .expect("B releases");
        let a_claim = steps
            .iter()
            .rposition(|s| s.0 == 0 && s.3.contains("pick flagged zero"))
            .expect("A claims under the stale flag");
        assert!(
            a_plan < b_write && b_write < b_release && b_release < a_claim,
            "{}",
            tr.render()
        );
    }

    #[test]
    fn the_current_order_passes_the_calloc_race_scenario() {
        let out = run(&calloc_race(), CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
        assert!(out.states > 100);
    }

    #[test]
    fn the_current_order_passes_the_churn_scenario() {
        let out = run(&churn(), CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
    }

    /// States a scenario of the unit suite may take: the 30 s budget at
    /// the roughly 10 000 states a second of a debug build in a loaded dev
    /// container. A count, not a clock, so a busy host cannot fail it.
    const HOST_BUDGET_STATES: usize = 300_000;

    #[test]
    fn the_current_order_passes_the_three_way_scenario() {
        let out = run(&three_way(), CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
        assert!(
            out.states <= HOST_BUDGET_STATES,
            "{} states: the scenario must stay inside the host budget ({HOST_BUDGET_STATES})",
            out.states
        );
    }

    #[test]
    fn the_pre_fix_order_has_the_race_on_a_purged_slab_too() {
        // The same window after a purge: A's second refill picks the
        // `Purged` slab. Drop the `Fresh` round from the programs so the
        // violation the model finds is the `Purged` one.
        let sc = Scenario {
            name: "calloc race on a Purged slab",
            threads: vec![
                (
                    0,
                    vec![
                        Op::Refill(1),
                        Op::Write(0),
                        Op::Release,
                        Op::Purge,
                        Op::Refill(1),
                    ],
                ),
                (0, vec![Op::Refill(1), Op::Write(0), Op::Release]),
            ],
            lg_group: 1,
        };
        let out = run(
            &sc,
            Variant {
                prefix_claim: false,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("the pre-fix protocol must show the race on a Purged slab");
        assert!(msg.contains("the calloc race"), "{msg}");
        assert!(
            tr.0.iter()
                .any(|s| s.3.contains("Purged slab 0 out of the purged set")),
            "{}",
            tr.render()
        );
        let out = run(&sc, CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
    }

    #[test]
    fn lowest_n_clear_takes_the_lowest_clear_bits() {
        assert_eq!(lowest_n_clear(0b0000, 1), 0b0001);
        assert_eq!(lowest_n_clear(0b0101, 2), 0b1010);
        assert_eq!(lowest_n_clear(0b1111, 1), 0);
        assert_eq!(lowest_n_clear(0b0001, 4), 0b1110);
    }
}
