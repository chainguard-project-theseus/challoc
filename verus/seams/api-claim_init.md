# `claim_init`

Seam S28 of `verus/SEAMS.md`: src/api/mod.rs:409. Kind: external_body. Category: atomics primitive.
Discharged by the C11 atomicity rules and loom (part 3).

## Contract

At commit 0c9cd77:

```rust
fn claim_init() -> (r: (Result<u32, u32>, Tracked<Map<once::Step, once::Once>>))
    ensures
        r.0 is Ok ==> once::all_steps(r.1@),
```

The body, at that commit: `(STATE.compare_exchange(UNINIT, INITIALISING,
Acquire, Acquire), Tracked::assume_new())`. The seam is the minted
tokens: the contract hands every caller whose exchange succeeded the
process's one-shot tokens (`once::all_steps`), so it is sound only if at
most one call per process succeeds. Its one caller is `init_slow`.

## Statement

Two parts.

1. **C11.** `STATE` is an atomic whose writes are its initial value
   `UNINIT` (the static's initialiser), the successful
   `compare_exchange`s of `claim_init` (`UNINIT` to `INITIALISING`) and
   `init_slow`'s `READY` store: two write sites in `src/`, which
   `run.sh` checks. All writes are in one modification order, and a
   read-modify-write reads the last value before its own write in that
   order ([atomics.order] p10), for every ordering. A failed
   `compare_exchange` writes nothing.
2. **loom.** `src/loom_tests/init.rs` (described in `api-init_slow.md`)
   runs a copy of `init_slow` whose first step is this
   `compare_exchange` with these orderings (pinned in
   `verus/loom/pins.txt`), with two callers and a free, or with a
   re-entrant `init`. In every execution `init`, which only the winner
   runs, runs once.

Models: `loom_init_free_races_init`, `loom_init_two_callers`, `loom_init_reentry_aborts`

   - `loom_init_two_callers` (29839 executions),
     `loom_init_free_races_init` (10) and `loom_init_reentry_aborts`
     (213383): `init` runs exactly once.

## Translation

- **`ensures r.0 is Ok ==> once::all_steps(r.1@)`**: the tokens are
  ghost and Verus takes them as fresh; what makes that sound is that
  `r.0 is Ok` holds for at most one call in the process. By part 1: a
  successful exchange reads `UNINIT` and writes `INITIALISING`. The only
  write of `UNINIT` is the initial value, which comes first in the
  modification order, and an RMW reads the value immediately before its
  own write; so a successful exchange is the first write after the
  initial value, and there is one first write. This holds for every
  ordering, so the `Acquire` orderings play no part in it (they matter
  to `init_slow`'s publication, `api-init_slow.md`).
- **Fork.** A child forked before the exchange starts from `UNINIT` with
  no tokens taken, and is a different process; a child forked after it
  never sees `UNINIT` again. Verus's ghost world is per process, so
  neither duplicates a token within one.
- **Gaps.** The model is a copy (pinned); it has 2 callers. The argument
  in the first bullet does not depend on the number of callers. That
  `STATE` has no other writer is checked by `run.sh` syntactically
  (writes through `STATE.` in `src/`); a write through a raw pointer to
  it would be caught only by review.

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
  10, 29839 and 213383). `src/loom_tests/dpor.rs` fails without it. The
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
- A copy (`init.rs`, `exit.rs`, the class steps of `bitmap.rs`, the
  table steps of `free.rs`, the arena steps of `count.rs`) cites the
  code it mirrors; `verus/loom/pins.txt` lists the source lines it
  copies, which `run.sh` requires in place and in order. A change
  elsewhere in the copied functions is caught by review only.

The Verus proofs assume sequential consistency. A loom record checks
that the orderings the code uses give the happens-before edges and the
progress the SC proof or contract relies on, in the modelled
configuration, and argues why that configuration is enough; it does not
lift a proof to weak memory.

## Review

Fresh-context subagent, 2026-10-04: the models against the code they
run or copy, and this translation, looking for claims beyond what the
models check. Its findings are folded in: `load_owner` became evidence
(its premise is in the doc comment, not the contract), the access
checks were widened and the records now say what they check, the count
records state the happens-before a ticket's `unbind` needs, and the
free path's stale reads are described as fast-path misses.

## Tests

`sh verus/loom/run.sh` (`docs/testing.md`, "loom model checks") first
checks that `verus/SEAMS.md` is current, that the contract above is
still the ledger's entry for this seam, that the models the `Models:`
line names exist and are not ignored, that the source lines the copies
mirror are in place (`verus/loom/pins.txt`), and, by name, that the
atomic writes the word and count records rely on are where they say
(non-test `bitmap.rs` writes words only through `vspec/word.rs`;
`class/table.rs` writes atomics only on three listed lines; `arena.rs`
touches the bound counts only through `vspec/count.rs` and
`Arenas::init`'s zeroing; `STATE` has two writes); then it runs every
loom model and control. A body that breaks the property fails a model
where the model runs the real code (`lock.rs`, `bitmap.rs`,
`vspec/word.rs`, `vspec/count.rs`); a changed copied line fails the pin
check; a changed contract fails the ledger check, until this record and
its models are revisited. The script is run by hand (about 15 minutes
with the build); no CI job runs it yet.
