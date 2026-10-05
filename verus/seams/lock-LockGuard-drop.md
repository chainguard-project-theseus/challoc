# `LockGuard::drop`

Seam S186 of `verus/SEAMS.md`: src/lock.rs:740. Kind: external_body. Category: atomics primitive.
Discharged by loom.

## Contract

At commit 8635213:

```rust
fn drop(&mut self)
    opens_invariants none
    no_unwind
```

The body, at that commit: `self.lock.raw.unlock();`, which is
`RawLock::unlock` (`src/lock.rs`): `state.swap(UNLOCKED, Release)`, then
`wake(&state)` (a `FUTEX_WAKE` of one waiter) if the old value was
`CONTENDED`.

## Statement

loom (see the common part below) runs the real `RawLock` code
(`lock.rs` over loom's atomics, with the futex stand-in and 2 spin
rounds instead of 100) in `src/loom_tests/lock.rs`: each thread locks,
bumps a counter in a loom `UnsafeCell` and unlocks.

Models: `loom_lock_two_threads`, `loom_lock_three_threads`, `loom_lock_dropped_mark_loses_a_wake_up`, `loom_futex_stand_in_sleeps`

- `loom_lock_two_threads` (exhaustive; 17 executions) and
  `loom_lock_three_threads` (at most 5 preemptions; no bound did not
  finish in 25 minutes): every `unlock` returns, the counter has no
  race (the `Release` swap orders each holder's writes before the next
  holder's acquire) and ends exact, and no thread is left asleep (an
  unlock that finds `CONTENDED` wakes a sleeper, including one behind
  another waiter).
- Controls: a mutant whose woken waiter retakes the lock by a CAS to
  `LOCKED` deadlocks (`loom_lock_dropped_mark_loses_a_wake_up`), and a
  bare wait on the stand-in deadlocks (`loom_futex_stand_in_sleeps`).

## Translation

- **No `requires`, no `ensures`**: the call returns. `unlock`'s body is
  a swap, a compare and the wake, whose result it ignores; it has no
  panic and no loop, and the models run it in every explored execution
  and every one returns.
- **`no_unwind`**: no panic site in the body; the shipped profiles also
  abort on panic.
- **`opens_invariants none`**: the body opens no Verus invariant (read
  from the source: `unlock` has no `open_atomic_invariant!`); not a
  loom fact.
- **What the release must do for the SC proofs** (the lock's contract,
  `LockHeld`: no other thread holds the lock until `unlock`, and the
  next holder sees the value as this one left it): the models check
  mutual exclusion and that edge for two threads in every execution
  and for three within the preemption bound. The edge does not depend
  on the thread count: every write to the lock word is an RMW (the
  acquire `compare_exchange` and `swap`s, the release `swap`), so the
  next acquirer reads from the release sequence of the last release.
  On a path through a wake the futex stand-in also orders them (the
  common part), so there the models do not separate the code's edge
  from the stand-in's.
- **Gaps.** `Lock<T>` and `LockGuard` are not built under loom (`Lock`'s
  `const fn new`); the model calls `RawLock::unlock` directly, which is
  the whole body. The three-thread model is bounded at 5 preemptions.
  The kernel's futex is the stand-in.

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
`class/table.rs` writes atomics only on three listed lines; `arena.rs`
touches the bound counts only through `vspec/count.rs` and
`Arenas::init`'s zeroing; `STATE` has two writes); then it runs every
loom model and control. A body that breaks the property fails a model
where the model runs the real code (`lock.rs`, `bitmap.rs`,
`vspec/word.rs`, `vspec/count.rs`); a changed copied line fails the pin
check; a changed contract fails the ledger check, until this record and
its models are revisited. The script is run by hand (about 15 minutes
with the build); no CI job runs it yet.
