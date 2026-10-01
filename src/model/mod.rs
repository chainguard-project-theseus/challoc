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
//! `Purged` and its memory to zero, under the lock). The group handoff
//! (`adopt_idle_group`/`transfer_group`) is part of `Refill`'s locked
//! step when the scenario turns `group_handoff` on, as in `next_slab`.
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
//! the state byte, which is what the calloc race and the ownership
//! check were about.
//!
//! Four knobs replay the protocol before its fixes ([`Variant`]):
//! `prefix_claim: false` is the pre-7 (c) order (the zero flag decided in
//! `plan`, the bits claimed in `finish`), and the model finds the calloc
//! race (a slot reported zero whose memory another thread dirtied in the
//! window) and prints the interleaving; `owner_check: false` drops the
//! owner test of `on_live_zero` and `reopen` (plan Phase 1.5 task 15),
//! and with `group_handoff` on the model finds a slab of one arena in
//! another arena's sets; `batch_clip: false` (per-CPU mode, below) lets
//! a refill push-back onto a non-empty cache leave the zero prefix
//! unclipped (the code before challoc PR #4), and the scenario
//! `percpu_refill_race` finds a dirty slot inside the prefix;
//! `pop_zero_in_section: false` reads a pop's zero flag after its commit,
//! outside the section (the x86_64 `pop_raw` before challoc PR #5), and
//! `percpu_share` finds a refill raising `zeroed` over the emptied cache
//! in between. With all four on, every scenario passes.
//!
//! A change to the protocol in `class/` is a change to a step here.
//!
//! The per-CPU mode (design section 15, plan Phase 2.5 task 8;
//! the scenarios `percpu_*`). With [`Scenario::percpu`] the caches
//! are owned by a concurrency id, not a thread: [`NCACHES`] caches,
//! cache `c` bound to arena `c`, each a small array plus `len` and the
//! `zeroed` prefix of design 6.3. `Pop` and `Push(i)` are rseq critical
//! sections as the kernel defines them: the thread reads `mm_cid` and
//! picks the cache, reads `len` and the top entry (speculative locals),
//! then one commit store (a pop reads `zeroed` for its zero flag before
//! the commit, inside the section); the pop and push sections store what
//! `rseq.rs` stores, `zeroed` included (a pop never lowers it, a push
//! clips it at `len`, a refill push-back sets it on an empty stack and
//! clips it at the old `len` on a non-empty one), and a preemption may
//! abort a section after its pre-commit stores (the `zeroed` stores an
//! aborted push or push-back leaves behind). The flush is one section
//! that takes half the cache and clears `zeroed`; the real flush is
//! two sections, `take_all` and then `push_batch` of the kept entries,
//! and the model does not interleave another thread between them. The
//! scheduler is modelled by what the caches can observe of it: the
//! `mm_cid` a thread reads at a section's entry is the exploration's
//! choice (any cid: the kernel may have moved the thread anywhere since
//! its last section), and since running threads of one process never
//! share an `mm_cid`, a thread that still has an outstanding read
//! of that cid was descheduled in between: under [`Section::Rseq`]
//! the kernel sends it to the abort handler (the section's start:
//! the locals discarded, `mm_cid` re-read, possibly a new cid),
//! and a thread inside a section may also be preempted spontaneously
//! (a signal, a migration; the one explicit scheduler step) with the
//! same abort. So with more threads than cids the caches are shared
//! over time, a thread that popped on cid 0 may push on cid 1, and
//! a commit under `Rseq` is always by the thread that holds the cid
//! that picked the cache. The kernel-owned fields are read-only in the
//! model: the threads read `mm_cid`, nothing in the protocol writes it.
//! [`Section::None`] is the buggy variant that does the same loads and
//! stores with no section (a thread descheduled between its `mm_cid`
//! read and its store goes on with the stale cid and stale locals),
//! and [`Section::CidOutside`] reads `mm_cid` before the section (the
//! abort handler restarts with the stale index): both hand a slot to two
//! threads or lose a push, and the model prints the interleaving. An
//! empty cache leaves the section and refills through the existing
//! `Refill` steps (the class lock of the cid's arena), then pushes the
//! batch back through a section on whatever cid the thread is on then;
//! a full cache leaves the section, pops half through a section into the
//! thread's staging and releases it through the existing `Release` steps,
//! routed slab by slab to the owner arena (`Heap::release_routed`),
//! then retries.  The invariants gain: no slot in two caches or in a
//! cache and owned by a thread, every set bit has exactly one holder (a
//! cache, a thread's owned slot or its staging: a lost push is a bit with
//! no holder), the entries below `min(len, zeroed)` read zero
//! (`zeroed > len` is a normal state after pops), and
//! (`Variant::strict_cid`) a cache is only ever mutated by a section
//! whose cid matched at commit.
//!
//! Layout: `state` holds the modelled state (slabs, arenas, caches, the
//! per-thread machine) and its small helpers; `steps` the step function
//! of the thread-mode operations, the invariants and the depth-first
//! exploration; `percpu` the rseq sections of the per-CPU mode and the
//! composite `Pop`/`Push` micro-machines; `scenarios` the knobs
//! ([`Variant`], [`Section`]) and the named scenarios. This file keeps
//! the geometry constants, the outcome types, [`explore`] and the tests.

mod percpu;
mod scenarios;
mod state;
mod steps;

pub use scenarios::*;
use percpu::*;
use state::*;

use std::cell::Cell;
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
/// this is too large for the 30 s budget of the host suite and must be
/// cut, not waited for.
const MAX_STATES: usize = 5_000_000;
/// Per-cid caches in the per-CPU mode; cache `c` is bound to arena `c`
/// (design 15: arenas per CPU when the mode is on), so `NCACHES ==
/// NARENAS`.
pub const NCACHES: usize = NARENAS;
/// Entries per cache. Small: the refill's batch is the real code's
/// `cap / 2`, here [`Scenario::refill`] slots, so that a refill hands one
/// out and pushes the rest back into the cache.
pub const CACHE_CAP: u8 = 2;
/// Slots a `Pop` claims when its cache is empty, unless the scenario says
/// otherwise ([`Scenario::refill`]).
const REFILL: u8 = 2;
/// Slots the flush of a full cache pops out (`cap / 2`).
const FLUSH_HALF: u8 = CACHE_CAP / 2;
/// Slots a thread's program owns at once in the per-CPU mode.
const MAX_OWNED: usize = 2;
/// Sub-step bases of the composite per-CPU operations: `Pop` is the
/// section (0..2, then [`POP_LATE_ZERO`]), then `Refill`'s steps (10..),
/// then the batch push section (20..23), then `Release` of the overflow
/// (30..); `Push` is the section (0..3), then the flush section (10..13),
/// then `Release` of the flushed slots (20..) and back to 0.
const POP_REFILL: u8 = 10;
const POP_PUSHBACK: u8 = 20;
const POP_RELEASE: u8 = 30;
const PUSH_FLUSH: u8 = 10;
const PUSH_RELEASE: u8 = 20;
/// The pre-fix x86_64 pop's read of `zeroed` after its commit, outside
/// the section ([`Variant::pop_zero_in_section`] false).
const POP_LATE_ZERO: u8 = 3;

/// A step taken: thread, operation, sub-step, and what it did.
#[derive(Clone, Debug)]
pub struct Trace(pub Vec<(u8, Op, u8, String)>);

/// What [`explore`] found.
#[derive(Debug)]
pub struct Outcome {
    pub states: usize,
    pub violation: Option<(String, Trace)>,
    /// Per-CPU mode: commits of a `Push` of a slot of one arena into
    /// another cid's cache (the thread migrated between its pop and its
    /// push; the flush routes the slot back), and sections the scheduler
    /// aborted; counted over the exploration so a test can show a
    /// scenario exercised them.
    pub cross_pushes: usize,
    pub aborts: usize,
}

/// The exploration in progress: the scenario, the knobs, the states
/// seen and the path to the current one.
struct Model<'a> {
    pub(super) sc: &'a Scenario,
    pub(super) v: Variant,
    pub(super) seen: HashSet<Node>,
    pub(super) trace: Vec<(u8, Op, u8, String)>,
    pub(super) cross_pushes: Cell<usize>,
    pub(super) aborts: Cell<usize>,
}

/// Run `sc` under `v`: every interleaving, every state checked. The
/// exploration is depth first and recursive, one frame per step of the
/// path (a `Node` clone each), and with the scheduler as an actor a path
/// runs tens of thousands of steps deep, past the 2 MiB stack of a test
/// thread: it runs on its own thread with a 1 GiB stack (reserved, not
/// touched).
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
        caches: [Cache::default(); NCACHES],
        stale_commit: None,
    };
    let threads = sc.threads.iter().map(|(a, _)| Thread::new(*a)).collect();
    let mut m = Model {
        sc,
        v,
        seen: HashSet::new(),
        trace: Vec::new(),
        cross_pushes: Cell::new(0),
        aborts: Cell::new(0),
    };
    let violation = m.explore(Node { shared, threads }).err();
    Outcome {
        states: m.seen.len(),
        violation,
        cross_pushes: m.cross_pushes.get(),
        aborts: m.aborts.get(),
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

    #[test]
    fn the_current_order_passes_the_three_way_scenario() {
        let start = Instant::now();
        let out = run(&three_way(), CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
        assert!(
            start.elapsed().as_secs() < 30,
            "the model must stay under 30 s on the host"
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
            group_handoff: false,
            percpu: false,
            refill: REFILL,
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
    fn the_handoff_without_the_owner_check_puts_a_foreign_slab_in_an_arenas_sets() {
        let out = run(
            &handoff(),
            Variant {
                owner_check: false,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("without the owner check the handoff scenario must fail");
        assert!(
            msg.contains("owned by arena")
                || msg.contains("more than one set")
                || msg.contains("held by threads"),
            "{msg}\n{}",
            tr.render()
        );
        assert!(
            tr.0.iter().any(|s| s.3.contains("adopt: group")),
            "{}",
            tr.render()
        );
    }

    #[test]
    fn the_current_order_passes_the_handoff_scenario() {
        let out = run(&handoff(), CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
        assert!(out.states > 100);
    }

    #[test]
    fn lowest_n_clear_takes_the_lowest_clear_bits() {
        assert_eq!(lowest_n_clear(0b0000, 1), 0b0001);
        assert_eq!(lowest_n_clear(0b0101, 2), 0b1010);
        assert_eq!(lowest_n_clear(0b1111, 1), 0);
        assert_eq!(lowest_n_clear(0b0001, 4), 0b1110);
    }

    /// The per-CPU protocol as designed (`CURRENT`: `Section::Rseq`)
    /// passes `sc` inside the host budget.
    fn percpu_passes(sc: &Scenario) -> Outcome {
        let start = Instant::now();
        let out = run(sc, CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
        assert!(
            start.elapsed().as_secs() < 30,
            "the model must stay under 30 s on the host"
        );
        out
    }

    /// A violation the buggy per-CPU variants are expected to produce:
    /// the stale commit itself, a slot handed to two holders, a slot lost
    /// (a push overwritten), or a dirty slot reported zero.
    fn is_percpu_corruption(msg: &str) -> bool {
        msg.contains("stale mm_cid")
            || msg.contains("at once")
            || msg.contains("a slot lost")
            || msg.contains("calloc race")
    }

    #[test]
    fn the_rseq_protocol_passes_the_per_cpu_share_scenario() {
        let out = percpu_passes(&percpu_share());
        assert!(out.states > 100);
        assert!(out.aborts > 0, "the scheduler never aborted a section");
        assert!(
            out.cross_pushes > 0,
            "no slot was pushed into a cache other than the one it came from"
        );
    }

    #[test]
    fn the_rseq_protocol_passes_the_per_cpu_migrate_scenario() {
        let out = percpu_passes(&percpu_migrate());
        assert!(out.cross_pushes > 0);
    }

    #[test]
    fn the_rseq_protocol_passes_the_per_cpu_purge_scenario() {
        percpu_passes(&percpu_purge());
    }

    #[test]
    fn the_cache_sections_keep_zeroed_as_the_asm_does() {
        let z = |slot| Held {
            slab: 0,
            slot,
            zero: true,
        };
        let d = |slot| Held {
            slab: 1,
            slot,
            zero: false,
        };
        // A zero batch of two onto an empty stack: the prefix is the batch.
        let mut c = Cache::default();
        assert_eq!(c.push_batch(0, &[z(2), z(1)], true, true), 2);
        assert_eq!((c.len, c.zeroed), (2, 2));
        // A pop commits `len` alone: `zeroed` stays above it.
        c.set_len(1);
        assert_eq!((c.len, c.zeroed), (1, 2));
        // A dirty batch onto the non-empty stack: clipped at the old
        // `len` with the fix, left above it before.
        let mut old = c;
        assert_eq!(old.push_batch(1, &[d(0), d(1)], false, false), 1);
        assert_eq!((old.len, old.zeroed), (2, 2));
        assert_eq!(c.push_batch(1, &[d(0), d(1)], false, true), 1);
        assert_eq!((c.len, c.zeroed), (2, 1));
        // A push clips at its `len` whatever it pushes.
        let mut c = Cache::default();
        c.push_batch(0, &[z(2), z(1)], true, true);
        c.set_len(0);
        c.push(0, z(3));
        assert_eq!((c.len, c.zeroed), (1, 0));
    }

    #[test]
    fn without_the_batch_clip_a_dirty_refill_lands_in_the_zero_prefix() {
        let out = run(
            &percpu_refill_race(),
            Variant {
                batch_clip: false,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("a push-back that leaves zeroed above the old len must show the race");
        assert!(msg.contains("the calloc race"), "{msg}\n{}", tr.render());
        // The mechanism: a pop leaves `zeroed` above `len`, and a dirty
        // batch lands on the non-empty cache with the prefix unclipped.
        assert!(
            tr.0.iter().any(|s| s.3.contains("pop: commit len 2 -> 1")
                && s.3.contains("(zeroed 2 stays)")),
            "{}",
            tr.render()
        );
        assert!(
            tr.0.iter()
                .any(|s| s.3.contains("dirty into cache")
                    && s.3.contains("(len 1 -> 2, zeroed 2 -> 2)")),
            "{}",
            tr.render()
        );
    }

    #[test]
    fn with_the_pop_zero_read_after_the_commit_a_refill_raises_zeroed_under_it() {
        let out = run(
            &percpu_share(),
            Variant {
                pop_zero_in_section: false,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("a zero flag read after the pop's commit must show the race");
        assert!(msg.contains("the calloc race"), "{msg}\n{}", tr.render());
        // The mechanism: a pop empties a cache, a refill on that cid sets
        // the prefix over the empty stack, and the pop's late read of
        // `zeroed` then reports its slot zero.
        let late =
            tr.0.iter()
                .position(|s| s.3.contains("after the commit") && s.3.contains("(reported zero)"))
                .expect("the pop reads the raised zeroed");
        let popper = tr.0[late].0;
        let empty = tr.0[..late]
            .iter()
            .rposition(|s| s.0 == popper && s.3.contains("pop: commit len 1 -> 0"))
            .expect("that pop emptied a cache");
        assert!(
            tr.0[empty..late].iter().any(|s| s.0 != popper
                && s.3.contains("zero into cache")
                && s.3.contains("(len 0 ->")),
            "another thread's refill pushes a zero batch onto the emptied cache in between\n{}",
            tr.render()
        );
    }

    #[test]
    fn the_rseq_protocol_passes_the_per_cpu_refill_race_scenario() {
        percpu_passes(&percpu_refill_race());
    }

    #[test]
    fn without_the_section_a_slot_is_handed_to_two_threads_or_a_push_is_lost() {
        // With the cid check off the model runs past the stale commit to
        // the corruption it causes and names it.
        let out = run(
            &percpu_share(),
            Variant {
                section: Section::None,
                strict_cid: false,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("plain loads and stores under a stale cid must corrupt a cache");
        assert!(
            msg.contains("at once") || msg.contains("a slot lost") || msg.contains("calloc race"),
            "{msg}\n{}",
            tr.render()
        );
        // The mechanism: a thread taken off its cid between a speculative
        // read and its commit, and no abort.
        assert!(
            tr.0.iter().any(|s| s.3.contains("goes on stale")),
            "{}",
            tr.render()
        );
        assert!(
            !tr.0.iter().any(|s| s.3.contains("rseq abort")),
            "{}",
            tr.render()
        );
        // With the check on, the stale commit is named as such unless the
        // corruption is in the same state.
        let out = run(
            &percpu_share(),
            Variant {
                section: Section::None,
                ..CURRENT
            },
        );
        let (msg, tr) = out.violation.expect("the stale commit must be found");
        assert!(is_percpu_corruption(&msg), "{msg}\n{}", tr.render());
    }

    #[test]
    fn the_cid_read_before_the_section_is_the_same_bug() {
        let out = run(
            &percpu_share(),
            Variant {
                section: Section::CidOutside,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("a cache index from an mm_cid read outside the section must fail");
        assert!(is_percpu_corruption(&msg), "{msg}\n{}", tr.render());
        // The section did abort where it was preempted; the index it
        // kept is what went stale.
        assert!(
            tr.0.iter().any(|s| s.3.contains("read before the section")),
            "{}",
            tr.render()
        );
    }
}
