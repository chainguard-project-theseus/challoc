# `swap` (`vspec/count.rs`)

Seam S305 of `verus/SEAMS.md`: src/vspec/count.rs:177. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 atomicity rules (part 3).

## Contract

At commit 82ca40d7 (unchanged since):

```rust
pub fn swap(c: &AtomicU32, v: u32, ordering: Ordering, Tracked(perm): Tracked<&mut CountPerm>) -> (prev: u32)
    requires
        old(perm).id() == count_id(c),
    ensures
        final(perm).id() == old(perm).id(),
        prev == old(perm).value(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
```

The body, at that commit: `c.swap(v, ordering)`. Its one call site is
`Policy::apply` (`Relaxed`), which now (since #79, after 82ca40d7)
swaps one more than the band's position (clamped to `FRAC_ONE`; 0
outside the scaled band) into the policy's position
(`Policy::scaled16`) inside its invariant (`policy::FracPred`, at most
`FRAC_ONE + 1`) and compares the previous value with it for `changed`.

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
   the accesses to one atomic can be put in one total order, consistent
   with happens-before, in which every read reads the last write before
   it. `swap` is a read-modify-write that accepts every ordering.
2. **loom.** `src/loom_tests/position.rs` (see the common part below)
   runs the real wrappers of `vspec/count.rs` (`swap`, `load`, compiled
   over loom's `AtomicU32`) with the orderings `policy.rs` passes (both
   `Relaxed`), driven by copies of `Policy::apply`'s swap of the
   position and of `Policy::position`. `apply` takes no lock
   (`policy::evaluate` runs on whichever thread's sample is due), so two
   threads apply `Scaled(0x8000)` and `Scaled(FRAC_ONE)` (storing
   `0x8001` and `FRAC_ONE + 1`) concurrently while the main thread reads
   the position twice. Exhaustive:

Models: `loom_position_apply_swaps`, `loom_position_split_swap_breaks_the_chain`

   - `loom_position_apply_swaps` (940 executions): every load returns
     0 or a stored value, so at most `FRAC_ONE + 1`, and the swaps form
     one chain: the two values they return and the final value are 0,
     `0x8001` and `FRAC_ONE + 1`, each once.
   - Control: with the swap split into a `Relaxed` load and store
     (`loom_position_split_swap_breaks_the_chain`), both `apply`s can
     return the initial 0 and the chain check fails.

## Translation

- **`requires old(perm).id() == count_id(c)`**: the permission is for
  this counter; part 1 is about that counter alone.
- **`ensures`.** Read `perm.value()` as the value of the last write to
  the counter in its modification order among those performed so far
  (every access to the position goes through these wrappers, which
  `run.sh` checks; `Policy::new` makes it with `vspec::count::new`).
  `swap` is a read-modify-write: it reads the last value before its own
  write (`prev == old(perm).value()`) and writes `v`
  (`final(perm).value() == v`), for every ordering; the chain the model
  checks is that consequence. `final(perm).id() == old(perm).id()` is
  ghost.
- **`opens_invariants none`, `no_unwind`**: the body opens no Verus
  invariant (read from the source) and has no panic site (`swap`
  accepts every ordering).
- **What the SC proof carries across locations.** `FracPred` names one
  counter and a constant bound (`FRAC_ONE + 1`); no invariant relates
  the position to another location, and `apply`'s other fields are
  `Relaxed` heuristics outside any invariant (`verus/WEAKMEM.md`, "Out
  of scope"). The proofs use that a load returns at most `FRAC_ONE + 1`
  (`Policy::position`, then `Band::wf` of `Scaled(f - 1)` in
  `Policy::band`) and that the swap returns the value it replaced; part
  1 gives both per counter (every write is at most `FRAC_ONE + 1`,
  which Verus proves at the write, and an RMW reads its predecessor in
  modification order).
- **Gaps.** 2 appliers and one reader, 2 positions; the policy's other
  fields are not modelled; the `Policy` steps are copies (pinned,
  `verus/loom/pins.txt`). The argument that any ordering keeps an
  invariant over one atomic and constants is the one above, checked by
  the model for this instance, not proved in a weak-memory logic.

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

Fresh-context subagent, 2026-10-05: the models against the code they
run or copy, and this translation, looking for claims beyond what the
models check. Its findings are folded in: the arena count model now
asserts, under the creation lock, the frozen count and table that
`api::count_held` and `api::arena_held` assume (named in the records,
which no longer say the bound is all the code relies on),
`desc_if_committed` is named as a `Relaxed` reader of the watermark,
the parts of `build` and `take_group` the copies leave out are listed,
and the new models were also run under stock loom to back the common
part's claim about execution counts.

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
