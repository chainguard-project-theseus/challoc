# `LockGuard::deref`

Seam S179 of `verus/SEAMS.md`: src/lock.rs:710. Kind: external_body. Category: UnsafeCell/volatile/transmute.
Discharged by the C11 data-race rules, given the lock's mutual exclusion
and acquire/release edge, which loom checks on the real lock word (part
3); the models do not run the body below.

## Contract

At commit 0c9cd77:

```rust
fn deref(&self) -> (r: &T)
    ensures
        *r == self@,
```

The body, at that commit: `unsafe { &*self.lock.value.get() }`, with the
safety comment that the guard exists only while the raw lock is held.
`LockGuard`'s view (`self@`) is an uninterpreted spec function: the
guarded value. `Lock::lock` builds a guard right after `RawLock::lock`
returns and gives its view the lock invariant (`axiom_acquired`, a
separate seam); the guard's `Drop` releases the raw lock
(`lock-LockGuard-drop.md`).

## Statement

Two parts.

1. **C11, non-atomic data.** A read of a non-atomic object that is not
   in a data race reads its visible side effect, the write that happens
   before it with no other write in between ([intro.races] p13); writes
   not ordered with every other access are data races, which are
   undefined.
2. **loom.** The models of `src/loom_tests/lock.rs` (described in
   `lock-LockGuard-drop.md`) run the real `RawLock` (`lock.rs` over
   loom's atomics, with the futex stand-in) around a counter in a loom
   `UnsafeCell`, as `Lock<T>` holds its value: each thread locks, reads
   and writes the cell, and unlocks.

Models: `loom_lock_two_threads`, `loom_lock_three_threads`

   - `loom_lock_two_threads` (exhaustive; 17 executions) and
     `loom_lock_three_threads` (at most 5 preemptions; 156584): no race
     on the cell, and it ends exact: each holder reads the value the
     previous holder wrote.

## Translation

- **`ensures *r == self@`**: the reference is to the lock's value, and
  the guard's view is that value: reads through `r` return what the cell
  holds while the guard lives.
- **Why it holds.** While a guard lives its thread holds the raw lock,
  so no other thread accesses the value (mutual exclusion), and the
  previous holder's accesses happen before this holder's (its `Release`
  swap heads a release sequence of RMWs on the lock word that this
  holder's `Acquire` read joins). So the holder's accesses through the
  reference race with nothing, and by part 1 each read returns the
  latest write in this thread or the previous holder's last one: the
  value as the guard sees it. The models check mutual exclusion and the
  edge (a race or a lost update would fail them).
- **Gaps.** `Lock<T>` and `LockGuard` are not built under loom
  (`Lock::new` is a `const fn` over loom-less atomics, and the value
  cell is `core`'s `UnsafeCell`, which loom does not track), so the
  models run the real lock word around a loom cell standing for the
  value, not the one-line body above. Three threads are checked within 5
  preemptions only. The futex is the stand-in (common part). That the
  value satisfies `lock_inv` is not this seam's (`axiom_acquired`).

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
