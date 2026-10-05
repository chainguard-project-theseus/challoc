# `load` (`vspec/count.rs`)

Seam S302 of `verus/SEAMS.md`: src/vspec/count.rs:114. Kind: external_body. Category: atomics primitive.
Evidence from loom and the C11 coherence rules (part 3): the contract
does not exclude the two orderings with which the body panics.

## Contract

At commit 0c9cd77:

```rust
pub fn load(c: &AtomicU32, ordering: Ordering, Tracked(perm): Tracked<&CountPerm>) -> (r: u32)
    requires
        perm.id() == count_id(c),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
```

The body, at that commit: `c.load(ordering)`. Its call site is
`Arenas::load_bound` (`Relaxed`), inside the counter's invariant.

## Statement

Two parts.

1. **C11, per counter.** For any one atomic object, C++20 (which Rust
   adopts) orders all its writes in one modification order consistent
   with happens-before ([intro.races] p4, p15-p18), and "atomic
   read-modify-write operations shall always read the last value (in the
   modification order) written before the write associated with the
   read-modify-write operation" ([atomics.order] p10), for every memory
   ordering. A load reads a write of that order no older than the last
   write that happens before it; equivalently (RC11's coherence axiom),
   the accesses to one counter can be put in one total order, consistent
   with happens-before, in which every read reads the last write before
   it. `fetch_add` and `fetch_sub` on `AtomicU32` wrap (`core`'s
   documentation).
2. **loom.** `src/loom_tests/count.rs` (see the common part below) runs
   the real wrappers of `vspec/count.rs` (compiled over loom's
   `AtomicU32`) with the orderings `arena.rs` passes (`Relaxed`), driven
   by copies of `Arenas::count_in` (and `bind`), `unbind` (with its
   abort on a previous count of 0), `load_bound` and `assign` (under the
   real `RawLock`, the first least `(bound, unbuilt)` key read, then
   `count_in`), for 2 arenas: the main thread's heap is homed on arena 0
   throughout and the main thread also binds a heap to arena 1 and
   unbinds it, while two threads are each assigned an arena and unbind
   it. A thread's tickets are the counts it added and has not yet taken
   back. Exhaustive:

Models: `loom_count_bind_assign_unbind`, `loom_count_split_rmw_loses_a_ticket`

   - `loom_count_bind_assign_unbind` (10872962 executions): no `unbind`
     reads a previous count of 0, no thread holding a ticket for an
     arena loads that arena's count as 0, and once every thread is
     joined each count is the number of tickets still out (1 and 0).
   - Control: with `count_in`'s `fetch_add` split into a `Relaxed` load
     and store (`loom_count_split_rmw_loses_a_ticket`), a ticket is lost
     and one of those checks fails.

## Translation

- **`requires perm.id() == count_id(c)`**: the permission is for this
  counter; part 1 is about that counter alone.
- **`ensures r == perm.value()`**: read `perm.value()` as in
  `vspec-count-fetch_add.md`, the value of the last write to the counter
  in the per-location order of part 1. Part 1 places this load in that
  order right after the write it reads, so `r` is the permission's value
  there, for every ordering `load` accepts. In `load_bound` the proof
  asserts at that point that `r` is the number of tickets out, wrapped;
  the model checks the consequence it can observe (a holder never reads
  its arena's count as 0).
- **Why evidence and not a discharge: the orderings.** `core`'s
  `AtomicU32::load` panics for `Release` and `AcqRel`. The contract has
  no `requires` excluding them (`vspec/word.rs`'s `load` and `store`,
  and `SlabTable::watermark`, have one since commit 1708c04), yet says
  `no_unwind` and, with no `may_abort` required, a call that cannot
  abort (`sys::may_abort`, theorem "Read-only APIs never abort"); with
  `Release` the panic reaches the panic handler and aborts. Every caller
  passes `Relaxed` (`Arenas::load_bound`, pinned in
  `verus/loom/pins.txt`), so no proof is affected, but the contract as
  stated claims a return for an ordering with which the body aborts.
  Adding `!(ordering is Release)` and `!(ordering is AcqRel)` to the
  `requires` (ghost only; the callers verify unchanged) would make the
  translation above a discharge.
- **`opens_invariants none`**: the body opens no Verus invariant (read
  from the source).
- **What the SC proof carries across locations.** Theorem 34's atomic
  invariants (`arena::BoundPred`) each name one counter, its permission
  and its arena's ticket set; no invariant relates two counters, and the
  tickets are ghost state (no memory is owned through them). The proof
  needs each counter's own history to match its tickets, which part 1
  gives per counter: its RMWs, in modification order, each read the
  previous one's result; and it needs a ticket's `fetch_sub` (`unbind`)
  to come after the `fetch_add` that minted it in that order, or
  `unbind` could read 0 and abort, which the SC proof rules out. That
  does need happens-before from the mint to the `unbind` (write-write
  coherence then orders the two RMWs). It is there: the ticket belongs
  to a heap (`Heap::bound`, `Heap::homed`), and the thread that unbinds
  reads that heap's plain fields (`bound`, `arena`, in `Heap::destroy`),
  written when the heap was bound; those reads race with nothing only if
  they happen after the binding, on the same thread in program order or
  through a synchronising hand-off of the heap. The models bind and
  unbind on one thread. `assign` reads the counts one by one; under SC
  those reads are not a snapshot either, and the theorem claims only the
  keys it read (`picks_least` over `snap`), so their not being a
  snapshot under C11 changes nothing it states.
- **Gaps.** 2 arenas, 3 threads, one ticket per heap; the `u32` wrap
  (2^32 tickets out) is not reached; the arena table's `Relaxed` pointer
  loads in `assign` are a fixed built/unbuilt pattern; building an arena
  and moving a heap between threads are not modelled. The `Arenas` steps
  are copies (pinned, `verus/loom/pins.txt`). The argument that relaxed
  accesses suffice for invariants over one atomic and pure ghost state
  is the one above, checked by the model for this instance, not proved
  in a weak-memory logic.

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
