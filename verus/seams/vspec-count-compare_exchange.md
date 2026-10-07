# `compare_exchange` (`vspec/count.rs`)

Seam S309 of `verus/SEAMS.md`: src/vspec/count.rs:198. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 atomicity rules (part 3).

## Contract

At commit c704a90e:

```rust
pub fn compare_exchange(
    c: &AtomicU32,
    current: u32,
    new: u32,
    success: Ordering,
    failure: Ordering,
    Tracked(perm): Tracked<&mut CountPerm>,
) -> (r: Result<u32, u32>)
    requires
        old(perm).id() == count_id(c),
        !(failure is Release),
        !(failure is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        r matches Ok(prev) ==> prev == current && old(perm).value() == current && final(perm).value() == new,
        r matches Err(prev) ==> prev == old(perm).value() && prev != current && final(perm).value() == prev,
    opens_invariants none
    no_unwind
```

The body, at that commit: `c.compare_exchange(current, new, success,
failure)`. Its one call site is `Arenas::bind_if_idle` (`current` 0,
`new` 1, `Relaxed` for both orderings), inside the bound count's
invariant (`arena::BoundPred`), which `Arenas::try_bind_idle` calls
after a `Relaxed` `load_bound` of the same count reads 0; its caller is
`Heap::try_rehome`, with no lock held.

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
   it. `compare_exchange` is the strong compare-and-swap
   ([atomics.types.operations], `compare_exchange_strong`): it reads the
   value, and if and only if it equals `current` it writes `new` in the
   same atomic step, a read-modify-write; otherwise it is an atomic load
   of the value it read, which it returns (`Err`). Unlike
   `compare_exchange_weak`, it does not fail spuriously (`core`'s
   documentation: only the weak form "is allowed to spuriously fail even
   when the comparison succeeds"; C++20's `compare_exchange_strong`
   fails only when the comparison is false). `core`'s `compare_exchange`
   panics for a `Release` or `AcqRel` failure ordering and accepts every
   other pair (since Rust 1.64 the failure ordering may be stronger than
   the success ordering).
2. **loom.** `src/loom_tests/count.rs` (see the common part below) runs
   the real wrappers of `vspec/count.rs` (`compare_exchange`, `load`,
   `fetch_add`, `fetch_sub`, compiled over loom's `AtomicU32`) with the
   orderings `arena.rs` passes (all `Relaxed`), driven by copies of
   `Arenas::try_bind_idle` and `bind_if_idle` (the `load_bound` that
   refuses a busy arena, then `compare_exchange(0, 1)`; pinned,
   `verus/loom/pins.txt`), `count_in` (and `bind`), `unbind` (with its
   abort on a previous count of 0) and `load_bound`, for 2 arenas: the
   main thread's heap is homed on arena 0 throughout, and two threads
   each try to move a heap onto arena 1 (`try_bind_idle`, no lock),
   keeping a ticket they win until they are joined. With a rival, the
   main thread meanwhile binds a heap to arena 1 and unbinds it.
   Exhaustive:

Models: `loom_count_try_bind_idle_rival`, `loom_count_try_bind_idle_one_wins`, `loom_count_split_cas_binds_twice`, `loom_count_split_cas_binds_twice_rival`

   - `loom_count_try_bind_idle_rival` (27471 executions; 11782 under
     stock loom): at most one racer wins, a racer that wins never loads
     the arena's count as 0, the rival's `unbind` never reads a
     previous count of 0, and once every thread is joined each count is
     the number of tickets still out (arena 0's 1, arena 1's the number
     of winners).
   - `loom_count_try_bind_idle_one_wins` (96; 24 under stock loom): the
     two racers alone; exactly one wins, and the final counts are 1 and
     1. loom's `compare_exchange` never fails spuriously (its
     `compare_exchange_weak` calls it), so this checks how the wrapper
     and its caller use the compare-and-swap, not the absence of
     spurious failure, which rests on part 1 alone.
   - Controls: with the compare-and-swap split into a `Relaxed` load and,
     on 0, a store of 1 (`loom_count_split_cas_binds_twice`, without the
     rival, and `loom_count_split_cas_binds_twice_rival`, with it), both
     racers can read 0 and win, and the winner check fails (each
     control's `should_panic` names its message; that the rival
     control's first failing execution is a double win, not the
     rival's `unbind` abort or a final count, depends on loom's
     exploration order, so a newer loom may need its message revisited).

## Translation

- **`requires old(perm).id() == count_id(c)`**: the permission is for
  this counter; part 1 is about that counter alone.
- **`requires !(failure is Release)`, `!(failure is AcqRel)`**: the two
  failure orderings for which `core`'s `compare_exchange` panics; with
  them excluded the body is one atomic compare-and-swap and returns
  (`no_unwind`). The call site passes `Relaxed` for both.
- **`ensures`.** Read `perm.value()` as in `vspec-count-fetch_add.md`,
  the value of the last write to the counter in the per-location order
  of part 1 (every access to a bound count goes through the
  `vspec/count.rs` wrappers, which `run.sh` checks; `Arenas::init`
  writes 0 before the registry is published).
  - `r matches Ok(prev) ==> prev == current && old(perm).value() ==
    current && final(perm).value() == new`: a successful
    compare-and-swap is a read-modify-write, so it reads the last value
    before its own write in modification order, which equalled
    `current` (it returns it), and writes `new`, for every ordering.
  - `r matches Err(prev) ==> prev == old(perm).value() && prev !=
    current && final(perm).value() == prev`: a failed one is an atomic
    load, which part 1 places in the per-location order right after the
    write it reads, so `prev` is the permission's value there; the
    strong form fails only when that value differs from `current`; and
    it writes nothing, so the value is unchanged.
  - `final(perm).id() == old(perm).id()` is ghost.

  The model checks the consequences the proof uses: on `Ok` the count
  was 0 and is 1, so the winner's fresh ticket is the only one out
  (`bind_if_idle` mints it, and `lemma_wrap_step` keeps the count the
  number of tickets out, wrapped); on `Err` no ticket is minted and the
  count is untouched. A stale 0 from `try_bind_idle`'s first
  `load_bound` cannot let a second racer in, because the decision is
  the compare-and-swap's, an atomic step; a stale non-zero only refuses
  an idle arena, which the contract allows (`try_bind_idle` ensures
  nothing when it returns `false`).
- **`opens_invariants none`**: the body opens no Verus invariant (read
  from the source).
- **What the SC proof carries across locations.** As for `fetch_add`
  (`vspec-count-fetch_add.md`): `BoundPred` names one counter, its
  permission and its arena's ticket set, and no invariant relates two
  counters. A ticket `bind_if_idle` mints is later given back by
  `unbind`, whose `fetch_sub` must come after the compare-and-swap in
  modification order; it does, by the happens-before argument of that
  record: the heap's `arena` field, which `try_rehome` sets to the won
  arena after the win, on the thread that owns the heap, and its
  `bound`, still set, are read by the thread that unbinds, in
  `Heap::destroy`, or on the owning thread itself in `Heap::set_arena`
  (program order). `Heap::try_rehome`'s claim that a rehome never
  stacks a second heap on an arena rests on the count being 0 at the
  compare-and-swap, which is part 1's per-location order; it does not
  rely on any ordering with other locations (the moved heap's data is
  published by other means, outside this seam).
- **Gaps.** 2 arenas, 3 threads, one ticket per heap; the `u32` wrap
  (2^32 tickets out) is not reached; a winner does not unbind inside the
  model (`loom_count_bind_assign_unbind` covers `unbind` after
  `count_in`'s read-modify-write on the same counter); `Err ==> prev !=
  current` (no spurious failure) is not checked by loom, whose
  compare-and-swap has none, and rests on part 1; the `Arenas`
  steps are copies (pinned). The argument that relaxed accesses suffice
  for an invariant over one atomic and pure ghost state is the one
  above, checked by the model for this instance, not proved in a
  weak-memory logic.

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

Written at c704a90e, 2026-10-06: the models, the pins and this record
were reviewed by a fresh-context subagent against the code they run or
copy, looking for claims beyond what the models check. Its findings
are folded in: loom's compare-and-swap cannot fail spuriously, so the
no-spurious-failure conjunct is named as resting on part 1 alone; the
controls' `should_panic` now names the winner check's message; the
`core` quote is the weak form's; `set_arena` is named as an unbinder.
The new models were also run under stock loom to back the
common part's claim about execution counts.

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
