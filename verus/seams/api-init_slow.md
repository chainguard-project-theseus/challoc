# `init_slow`

Seam S25 of `verus/SEAMS.md`: src/api/mod.rs:675. Kind: no_decreases. Category: termination.
Discharged by loom (termination given that `init` returns; part 3).

## Contract

At commit 0c9cd77:

```rust
fn init_slow() -> (r: &'static Shared)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r.wf(),
```

The seam is the termination of the body's one loop, the wait
`while load_state(..) != READY { spin_loop() }` (an `Acquire` load of
`STATE`, `state_load`): Verus proved the `ensures` for every run that
returns.

## Statement

loom (see the common part below) checks a copy of `init_slow`,
`shared` and `shared_if_ready` (`src/loom_tests/init.rs`; `STATE`,
`INIT_TID` and `SHARED` are statics built in `const` context, which
loom cannot construct), with `init`'s writes as one loom `UnsafeCell`
and `gettid` as a distinct non-zero number per thread. Exhaustive (no
preemption bound):

Models: `loom_init_free_races_init`, `loom_init_two_callers`, `loom_init_reentry_aborts`, `loom_init_relaxed_publish_is_a_race`, `loom_init_unpublished_wait_fails`

- `loom_init_free_races_init` (one caller of `shared`, the main thread
  in `shared_if_ready`; 10 executions) and `loom_init_two_callers` (two
  callers and the free; 29839): in every execution `init` runs once,
  every caller returns the value `init` wrote, with no race on it, the
  free reads `None` or that value, and no thread is left spinning.
- `loom_init_reentry_aborts` (two callers, `init` itself calls
  `shared`; 213383): the re-entrant call takes the abort in every
  execution, and the other caller waits for the outer `init` and
  returns its value.
- Controls: `loom_init_relaxed_publish_is_a_race` (the `READY` store
  `Relaxed`) fails with a race on the published value;
  `loom_init_unpublished_wait_fails` (a waiter on an `INITIALISING`
  nobody publishes) fails on the branch bound, so a waiter left
  spinning is reported.

Under stock loom (no DPOR patch) the first three models ran 1, 522 and
1472 executions: stock loom never ran the initialiser's `compare_exchange`
and `READY` store ahead of another thread's earlier load of `STATE`, the
orders the publication check needs.

## Translation

- **Termination.** A thread reaches the wait only when its
  `compare_exchange` failed, read neither `UNINIT` nor `READY` (both
  other arms leave the `match`), so it read `INITIALISING`, and its
  `INIT_TID` load did not return its own tid. `STATE` has two writes in
  the source (`api/mod.rs`, the `compare_exchange` and the `READY`
  store), so `INITIALISING` was written by the one successful
  `compare_exchange`, whose thread `W` then stores `INIT_TID`, runs
  `init` and stores `READY`, never overwritten. If the waiter were `W`,
  its `INIT_TID` load would read `W`'s own earlier store or a later one
  (read-after-write coherence; no later store exists), its tid, and it
  would abort instead: the models check this arm. So the waiter is
  another thread, and its loop ends once `W`'s `READY` store is visible
  to it, which takes: (a) `init` returns on `W` (its body is verified;
  its callees' termination is their own seams'); (b) `W` is scheduled
  (a fair OS scheduler); (c) the store becomes visible in finite time
  (C++20 [intro.progress] says *should*; every target CPU drains its
  store buffer). The models check that under (a)-(c), as loom models
  them, no execution leaves a waiter spinning. Each waiter's exit
  depends only on `STATE`, which waiters only read, so more waiters
  behave as one; loom cannot explore two spinning waiters at once (two
  threads yielding to each other while `W` is not scheduled is a path
  no branch bound cuts short), so the models have one at a time.
- **Gaps (termination).** A thread that allocates in a child forked
  while `W` is in `init` spins for ever: `W` does not exist in the
  child. POSIX allows a multithreaded program's child only
  async-signal-safe calls until `exec`, and `malloc` is not one, so this
  is outside the C contract; it is recorded, not claimed. Likewise a
  signal handler on `W` that allocates between its `compare_exchange`
  and its `INIT_TID` store reads 0 and spins on its own `INITIALISING`.
- **`requires crate::sys::may_abort()`**: the re-entry arm aborts
  (`sys::abort`), which this capability allows; the models check that
  only the initialising thread takes it.
- **`requires crate::sys::may_syscall()`**: not used by the loop.
- **`ensures r.wf()`**: proved by Verus: `shared_ref` (verified) reads
  the value through the permissions `STATE`'s invariant holds from
  `READY` on (`api::axiom_state`, a separate seam), which this function
  gets from `claim_init`, its own `init` or the wait's load. The
  invariant's premise for a thread that observed `READY` with an
  acquire load is that it reads the value `init` built. The models
  check that premise's weak-memory half for these callers (no race on
  the published cell after an `Acquire` load of `READY`, including on
  the free path's `shared_if_ready`).
- **Copy.** The model is a copy, kept in step by review. It leaves out
  what `init` does besides its writes (verified, and it does not touch
  `STATE`), the `Relaxed` configuration statics `init` stores (their
  readers rely only on what every store satisfies), and the fast paths
  that skip the `READY` load (their witness is the thread's own TLS
  heap pointer, WEAKMEM.md P4).

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

Fresh-context subagent, 2026-10-03: the models against the code they
run or copy, and this translation, looking for claims beyond what the
models check; its findings are folded in.

Re-quoted at 0c9cd77 on 2026-10-04 (the contract gained a `requires`
line), with a bullet for it in the translation; checked by the
second pass's fresh-context review.

Revisited on 2026-10-05 (`STATE`'s invariant): the body's loads and
writes of `STATE` go through `load_state`, `claim_init` and
`publish_ready` (the wrappers `state_load`, `state_claim`,
`state_publish`, each the bare `core` call), with the same orderings
and the same machine code (x86_64); the pinned lines moved with them
(`verus/loom/pins.txt`). The contract is unchanged, and so are the
models.

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
