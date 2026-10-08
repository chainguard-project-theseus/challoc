# `state_publish`

Seam S25 of `verus/SEAMS.md`: src/api/mod.rs:527. Kind: external_body. Category: atomics primitive.
Discharged by the C11 coherence rules and loom (part 3).

## Contract

At commit 4f75a5b7 (`STATE`'s invariant; with `state_claim`, whose
record is `api-state_claim.md`, and `state_load`, `api-state_load.md`):

```rust
fn state_publish(Tracked(perm): Tracked<&mut StatePerm>)
    ensures
        final(perm).value() == READY,
    opens_invariants none
    no_unwind
```

The body: `STATE.store(READY, Ordering::Release)`, the bare `core` call
under `#[inline(always)]`. The seam is that `perm`, `STATE`'s ghost
permission (`api::StatePerm`, held by `STATE`'s invariant,
`api::axiom_state`), carries `STATE`'s value across the store. Its one
caller is `publish_ready`, inside the invariant, which in the same
opening puts the permissions `init` froze into the invariant
(`v.published`, required `ok()`), as `StatePred` requires at `READY`;
`init_slow` calls it once, on the exchange's winner, after `init`.

## Statement

Two parts.

1. **C11, per atomic.** For `STATE`, C++20 (which Rust adopts) orders
   all writes in one modification order consistent with happens-before
   ([intro.races] p4, p15-p18), and a read-modify-write reads the last
   write before its own in that order ([atomics.order] p10).
   `STATE`'s writes are its initial value `UNINIT`, the successful
   `compare_exchange` of `state_claim` (`UNINIT` to `INITIALISING`) and
   this `READY` store: two write sites in `src/`, which `run.sh`
   checks. `core`'s `store` panics for `Acquire` and `AcqRel` and is
   otherwise one atomic store.
2. **loom.** `src/loom_tests/init.rs` (described in `api-init_slow.md`)
   runs a copy of `init_slow` whose winner stores `READY` with this
   ordering after `init` (pinned in `verus/loom/pins.txt`, with the
   exchange and the loads), with two callers and a free, or with a
   re-entrant `init`. In every execution `init` runs once, every
   caller returns the value `init` built, with no race on it, and
   every load or exchange of `STATE` never goes back from the thread's
   last value (so a thread that has seen `READY` sees nothing else);
   in `loom_init_publisher_reads_ready` the winner loads `STATE` right
   after its store (a load the model adds) and reads `READY`.

Models: `loom_init_free_races_init`, `loom_init_two_callers`, `loom_init_reentry_aborts`, `loom_init_publisher_reads_ready`, `loom_init_relaxed_publish_is_a_race`

   - `loom_init_free_races_init` (10 executions),
     `loom_init_two_callers` (29839), `loom_init_reentry_aborts`
     (213383): `init` runs once and every reader of `READY` reads the
     built value race-free; `loom_init_publisher_reads_ready` (34144):
     also, the publisher reads back `READY`.
   - Control: `loom_init_relaxed_publish_is_a_race` (this store
     `Relaxed`) fails with a race on the published value.

## Translation

- **`ensures final(perm).value() == READY`.** Read `perm.value()`, as
  in the other wrappers' records, as the value of the last write to
  `STATE` in its modification order (part 1; every write goes through
  the two wrappers, inside the invariant). A store is a write: after
  it the last write is `READY` until a later one, and there is none:
  the only other write site is the exchange, which writes only when it
  reads `UNINIT`, and as a read-modify-write it reads the last write
  in the order, which from here on is `READY` (the initial `UNINIT`
  comes first, the one successful exchange next). So `READY` stays the
  value for good, which is what lets `StatePred` freeze `published` at
  it. The model's readback and its coherence checks show the same of
  the executions it explores.
- **What the proof carries across locations.** `publish_ready` stores
  `published` into the invariant at this store, and `state_load`'s
  readers take it at `READY` (`api-state_load.md`). That `init`'s
  plain writes are visible to them is this `Release` store's
  synchronisation with their `Acquire` loads, `init` being sequenced
  before the store; the models check it and the control shows the
  ordering is needed.
- **`opens_invariants none`**: the body opens no Verus invariant.
  **`no_unwind`**: a `Release` store does not panic.
- **Gaps.** The model is a copy (pinned); it has 2 callers and a free,
  and the readback is an added load, not the code's (the code makes
  none: the publisher returns its own `init`'s value). That `STATE`
  has no other writer is checked by `run.sh` syntactically (writes
  through `STATE.` in `src/`); a write through a raw pointer to it
  would be caught only by review.

Common to every loom record. loom 0.7.2 with
`verus/loom/loom-0.7.2-dpor.patch` (`sh verus/loom/run.sh`, release
profile, `--cfg loom`, rustc 1.98.1) runs a test body under every
execution its C11 model reaches and fails on a data race on a loom
`UnsafeCell` (two accesses, one a write, not ordered by happens-before),
a deadlock (no thread runnable and one not finished), a spinning thread
that makes no progress (`spin_loop` yields; the execution exceeds the
branch bound) or a failed assertion. Its model and its bounds:

- Memory model: C++20's, as Rust adopts it, with these differences.
  Loads may read any of the last 7 stores to an atomic that coherence
  allows (`MAX_ATOMIC_HISTORY`). A load after a `spin_loop` does not
  re-read a store it saw before when a newer one exists (loom's eventual
  visibility; C++ only says an implementation *should* make stores
  visible in finite time). No load buffering: a load never reads a store
  that comes later in loom's execution order. A `SeqCst` load does not
  read a `SeqCst` store when a newer `SeqCst` store to the same atomic
  has already run; C++20 allows that read when the load precedes the
  newer store in its total order `S`, and loom produces the same read in
  the schedule that runs the load first (the two are dependent, so DPOR
  explores both orders); otherwise `SeqCst` accesses synchronise as
  `AcqRel`, and `SeqCst` fences are stronger than C++20's (none of the
  models below uses one). That loom's `SeqCst` reads cover C++20's is
  argued here, not checked.
- Exploration: exhaustive DPOR over the modelled threads (at most 5),
  with no preemption bound unless the record names one (`run.sh` unsets
  the `LOOM_*` bounds). Stock loom's DPOR is incomplete: it never runs a
  store ahead of another thread's earlier load when the storing thread
  loaded the atomic last. The patch races a store or rmw against every
  thread's last load too, and changes nothing else, so it only adds
  backtracking points (the execution counts measured are all at least
  stock loom's; the initialisation models' grew from 1, 522 and 1472 to
  10, 29839 and 213383, the arena count and watermark models' from 36
  to 4482 each, and their `Relaxed`-store controls pass under stock
  loom). `src/loom_tests/dpor.rs` fails without it. The
  patch is ours, reviewed but not proved; loom's own test suite was not
  run with it (its dev-dependencies are not in the offline registry).
- The futex is the stand-in `src/loom_tests/futex.rs`: one futex word
  per model, the compare under a loom mutex as the kernel's is under the
  hash-bucket lock, and a wake of one waiter. The mutex also orders a
  waker's earlier accesses before the woken thread's return. The code
  does not rely on that (after a wait each caller re-checks the word
  with its own acquiring access), but on a path through a wake the
  models cannot show it: an ordering missing from that re-check would be
  supplied by the stand-in. Linux's futex has such an edge in practice
  and documents no memory ordering.
- A copy (`init.rs`, `exit.rs`, `slab.rs`, the class steps of
  `bitmap.rs`, the table steps of `free.rs` and `watermark.rs`, the
  arena steps of `count.rs` and `narenas.rs`, the policy step of
  `position.rs`) cites the code it mirrors; `verus/loom/pins.txt` lists
  the source lines it copies, which `run.sh` requires in place and in
  order. A change elsewhere in the copied functions is caught by review
  only.

The Verus proofs assume sequential consistency. A loom record checks
that the orderings the code uses give the happens-before edges and the
progress the SC proof or contract relies on, in the modelled
configuration, and argues why that configuration is enough; it does not
lift a proof to weak memory.

## Review

Written 2026-10-07 at commit 4f75a5b7 (P1), following `state_claim`'s
record: the contract above is the ledger's entry there, and the models
and this translation were compared with each clause. A fresh-context
subagent reviewed this record against the models and the contract,
looking for claims beyond what the models check; its findings are
folded in.

## Tests

`sh verus/loom/run.sh` (`docs/testing.md`, "loom model checks") first
checks that `verus/SEAMS.md` is current, that the contract above is
still the ledger's entry for this seam, that the models the `Models:`
line names exist and are not ignored, that the source lines the copies
mirror are in place (`verus/loom/pins.txt`), and, by name, that the
atomic writes the word, count, watermark and descriptor records rely on
are where they say (non-test `bitmap.rs` writes words only through
`vspec/word.rs`; `class/table.rs` writes atomics only on two listed
lines and through `vspec/atom.rs`; `arena.rs` touches the bound counts
and the arena count, and `policy.rs` the position, only through
`vspec/count.rs`; nothing writes the watermark but `store_usize`, and
only `class/table.rs` calls `vspec/atom.rs` (the loom models aside);
`STATE` has two writes; non-test `slab.rs` writes a descriptor's
`state`, `shadow`, `arena` and `dirty_at` only in its accessors); then
it runs every loom model and control. A
body that breaks the property fails a model where the model runs the
real code (`lock.rs`, `bitmap.rs`, `vspec/word.rs`, `vspec/count.rs`,
`vspec/atom.rs`); a changed copied line fails the pin check; a changed
contract fails the ledger check, until this record and its models are
revisited. The script is run by hand (about 15 minutes with the build);
no CI job runs it yet.
