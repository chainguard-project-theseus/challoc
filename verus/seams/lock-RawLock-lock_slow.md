# `RawLock::lock_slow`

Seam S180 of `verus/SEAMS.md`: src/lock.rs:122. Kind: no_decreases. Category: termination.
Evidence from loom (deadlock freedom; termination is not established, part 3).

## Contract

At commit 8635213:

```rust
fn lock_slow(&self)
    requires
        crate::sys::may_syscall(),
```

The seam is the termination of the body's loops. The `for` over
`SPINS` is bounded; the `while c != UNLOCKED` wait is not, and its doc
comment does not claim it ends.

## Statement

The models of `LockGuard::drop`'s record (`verus/seams/lock-LockGuard-drop.md`),
which run the real `RawLock::lock_slow` (2 spin rounds under loom).

Models: `loom_lock_two_threads`, `loom_lock_three_threads`, `loom_lock_dropped_mark_loses_a_wake_up`

In every explored execution of two lockers (exhaustive) and of three
(at most 5 preemptions), with each locker taking the lock once, every
`lock_slow` returns: no waiter is left asleep, including the one
behind another waiter that the control's mutant strands.

## Translation

Evidence, not a discharge: termination does not hold unconditionally.
Drepper's mutex (`lock.rs`, "mutex 3") is not starvation-free: a waiter
woken by an `unlock` can lose the word to another thread's
`compare_exchange` and sleep again, as often as other threads keep
taking the lock, under any scheduler. What the models show is the
lock's deadlock freedom (no lost wake-up between the `swap(CONTENDED)`
and the sleep, nor for a sleeper behind another waiter) in the bounded
configurations: two and three threads, one acquisition each, the
three-thread one bounded at 5 preemptions. That a waiter returns when
the other threads take the lock finitely often follows for every thread
count from the futex's compare-and-queue under the bucket lock and the
`CONTENDED` mark every waiter leaves (Drepper, "Futexes Are Tricky",
mutex 3); the models check instances of it, not the general argument.
The seam stays an assumption: a waiter returns once the lock is
released and not retaken for ever by others.

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
