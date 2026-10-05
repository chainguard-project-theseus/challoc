# `main` (`purge/background.rs`)

Seam S193 of `verus/SEAMS.md`: src/purge/background.rs:444. Kind: no_decreases. Category: termination.
Discharged by loom (termination after a final `request(false)`, given that each pass and the exit's lock return; part 3).

## Contract

At commit 8635213:

```rust
extern "C" fn main(arg: *mut c_void) -> *mut c_void
```

The seam is the termination of the thread's `loop`; its doc comment
states the claim this record checks: the loop ends when another thread
calls `request(false)`.

## Statement

loom (see the common part below) checks a copy of the loop's exit
protocol (`src/loom_tests/exit.rs`; `REQUESTED` and `SEQ` are statics
built in `const` context, which loom cannot construct). The thread `B`
loads `SEQ` `SeqCst`, returns if `REQUESTED` (`Relaxed`) is false, and
otherwise sleeps on the futex stand-in with the loaded value and no
timeout (the pass between, `sample` and `decay`, touches neither word;
no timeout is the worst case for this property). The withdrawer `W` is
`request(false)`: `REQUESTED.store(false, Relaxed)`, then `kick()`
(`SEQ.fetch_add(1, SeqCst)` and a wake of one waiter). Exhaustive:

Models: `loom_exit_after_withdrawal`, `loom_exit_after_withdrawal_with_waker`, `loom_exit_without_kick_is_lost`, `loom_exit_relaxed_seq_load_is_lost`

- `loom_exit_after_withdrawal` (`B` and `W`; 86 executions) and
  `loom_exit_after_withdrawal_with_waker` (plus a third thread's
  `kick()`, as `wake` makes; 72720): `B` returns in every execution.
- Controls: a withdrawal that does not kick
  (`loom_exit_without_kick_is_lost`) and a `Relaxed` `SEQ` load in `B`
  (`loom_exit_relaxed_seq_load_is_lost`) each leave `B` asleep, a
  reported deadlock.

## Translation

- **The claim.** If a `request(false)` is the last write to `REQUESTED`
  in its modification order (no `request(true)` after it), the loop
  reaches its exit decision and returns, given that each pass returns:
  `prctl`, `now_coarse_ns`, `sample` and `decay` (verified; their
  callees' termination, such as the cgroup sampler's, are their own
  seams) and a `futex_wait`, which returns at its deadline, on a wake,
  on a changed `SEQ` or on a signal; and that the exit's
  `STATE.lock()` returns, which is `RawLock::lock_slow`'s seam (S176,
  evidence only: the lock is not starvation-free).
- **Why the models are enough.** Every write to `SEQ` is a `fetch_add`
  (`kick`), so every value `B` can load after `W`'s `fetch_add` heads
  or continues that operation's release sequence. A pass whose `SEQ`
  load reads `W`'s `fetch_add` or a later value synchronises with it
  (`SeqCst` load, release `fetch_add`), so `W`'s `REQUESTED` store
  happens before `B`'s `REQUESTED` load, which reads `false` (no later
  write): `B` returns. A pass whose `SEQ` load reads an earlier value
  sleeps on that value: the kernel compares the word under the
  hash-bucket lock, so either `W`'s `fetch_add` precedes the compare and
  the wait returns at once, or `B` is queued before `W`'s wake, which
  reaches it (`B` is the only thread that waits on `SEQ`: `start`
  creates a thread only when none is present, and an exiting thread
  has left its wait; assumed: the bookkeeping tracks the live thread,
  which `fork_child` resets only in a child, where no thread survives;
  a test that runs the handlers in the parent can leave a second
  thread, and a wake of one waiter may then reach the other). The next
  pass then loads `SEQ` again. This is independent of the number of
  threads that kick (each kick is one more RMW in the release
  sequence); the models check it with one and two kickers. The step
  "the next pass's load reads at least the value the kernel compared"
  is the hardware's (the kernel's read is not a C11 access); the futex
  stand-in has it by its mutex.
- **Gaps.** `request(true)` after the withdrawal (the thread then rightly
  keeps running) is outside the claim. The state lock `B` takes to
  clear `present` on its way out comes after the decision; the models
  stop at the decision, and the lock's return is the condition above.
  The copied lines are pinned (`verus/loom/pins.txt`).
- **P3 (WEAKMEM.md).** The open lost wake-up of the decay pass (`B`
  asleep with no deadline while a class has `Empty` slabs) is about the
  thread's progress between passes, not its exit: `request(false)`
  kicks unconditionally, so this record does not depend on P3.

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

Fresh-context subagent, 2026-10-03: the models against the code they
run or copy, and this translation, looking for claims beyond what the
models check; its findings are folded in.

## Tests

`sh verus/loom/run.sh` (`docs/testing.md`, "loom model checks") first
checks that `verus/SEAMS.md` is current, that the contract above is
still the ledger's entry for this seam, that the models the `Models:`
line names exist and are not ignored, that the source lines the copies
mirror are in place (`verus/loom/pins.txt`), and, by name, that the
atomic writes the word and count records rely on are where they say
(non-test `bitmap.rs` writes words only through `vspec/word.rs`;
`class/table.rs` writes atomics only on two listed lines and through
`vspec/atom.rs`; `arena.rs` touches the bound counts and the arena
count, and `policy.rs` the position, only through `vspec/count.rs`;
`STATE` has two writes); then it runs every
loom model and control. A body that breaks the property fails a model
where the model runs the real code (`lock.rs`, `bitmap.rs`,
`vspec/word.rs`, `vspec/count.rs`); a changed copied line fails the pin
check; a changed contract fails the ledger check, until this record and
its models are revisited. The script is run by hand (about 15 minutes
with the build); no CI job runs it yet.
