# `line_load`

Seam S26 of `verus/SEAMS.md`: src/api/mod.rs:562. Kind: external_body. Category: atomics primitive.
Discharged by the C11 coherence rules and loom (part 2).

## Contract

At the ONCECELL commit (the once-cells' invariants, which introduced it):

```rust
fn line_load(ordering: Ordering, Tracked(perm): Tracked<&LinePerm>) -> (r: u8)
    requires
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
```

The body: `LINE_STATE.load(ordering)`, the bare `core` call under
`#[inline(always)]`. Its one caller is `load_line`, inside the line's
invariant (`api::LinePred`), with `Acquire`; `line_ready` calls
`load_line` first and in its wait for another thread's build.

## Statement

Two parts.

1. **C11.** `LINE_STATE` is an atomic whose writes are its initial
   value `LINE_UNBUILT` (the static's initialiser), the successful
   exchanges of `line_compare_exchange` (`claim_line`'s `LINE_UNBUILT`
   to `LINE_BUILDING`, and `feature_line_fork_child`'s `LINE_BUILDING`
   back to `LINE_UNBUILT`, which only a fork child's one thread runs)
   and `line_store`'s `LINE_READY` store (`publish_line`): two write
   sites in `src/` outside the tests, which `run.sh` checks. All writes
   are in one modification order ([intro.races] p4); a
   read-modify-write reads the last value before its own write in that
   order ([atomics.order] p10), for every ordering; and by the coherence
   rules ([intro.races] p15-p18; RC11's coherence axiom) the accesses to
   `LINE_STATE` can be put in one total order, consistent with
   happens-before, in which every read reads the last write before it.
   A failed `compare_exchange` writes nothing and returns the value it
   read; the strong form fails only when that value differs from
   `current` (`core`: only the weak form "is allowed to spuriously
   fail"). A release store and an acquire load that reads it
   synchronise ([atomics.order] p2): what the storing thread wrote
   before the store happens before what the loading thread does after
   the load.
2. **loom.** `src/loom_tests/oncecell.rs` (see the common part below)
   runs a copy of `line_ready` with its wrappers and of
   `feature_line_fork_child` (`LINE_STATE`, `FEATURES` and
   `FEATURE_LINE` are statics built in `const` context, which loom
   cannot construct; the copied lines are pinned in
   `verus/loom/pins.txt`), the build as one write to each of two loom
   `UnsafeCell`s and the published line's read as a read of one.
   Exhaustive (no preemption bound):

Models: `loom_oncecell_line_two_requests`, `loom_oncecell_line_relaxed_publish_is_a_race`, `loom_oncecell_line_fork_child_rebuilds`

   - `loom_oncecell_line_two_requests` (5530 executions): two requests
     race and the main thread reads the line if it loads `LINE_READY`;
     the line is built once, every request returns the built line, and
     a reader that sees `LINE_READY` reads it, with no race on either
     cell and no thread left spinning.
   - `loom_oncecell_line_relaxed_publish_is_a_race` (control): with the
     `LINE_READY` store `Relaxed`, loom reports a read of the line as a
     causality violation.
   - `loom_oncecell_line_fork_child_rebuilds` (1 execution, one thread,
     as the child has): the reset from `LINE_BUILDING` makes the next
     request build the line rather than wait, and a reset of a ready
     line leaves it ready, built once.

## Translation

- **`requires !(ordering is Release)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `load` panics; with them excluded the
  body is one atomic load and returns (`no_unwind`). The call site
  passes `Acquire`.
- **`ensures r == perm.value()`**: Verus's model is sequentially
  consistent, and `perm.value()` is the value of the last write to
  `LINE_STATE` in part 1's per-location order (the permission starts at
  `LINE_UNBUILT`, `axiom_statics`' premise, and changes only through the
  wrappers, each at its one atomic operation). Part 1 places this load
  in that order right after the write it reads, so `r` is the
  permission's value there, for every ordering `load` accepts.
- **`opens_invariants none`**: the body opens no Verus invariant (read
  from the source).
- **What the SC proof carries across locations.** The line's invariant
  holds, besides `LINE_STATE`'s permission, the permissions to
  `FEATURES` and `FEATURE_LINE`: memory, handed between threads. A
  thread whose `load_line` reads `LINE_READY` gets the permissions
  frozen at the `LINE_READY` store (`publish_line`) and reads the line
  through them (`published_line`). Under C11 that read must happen
  after the builder's writes: the builder writes before its release
  store in program order, the reader reads after this acquire load,
  and the two synchronise (part 1), which is the edge the models check
  (the `Relaxed` control removes it and the read races). A load that
  reads `LINE_UNBUILT` or `LINE_BUILDING` hands out nothing.
- **Gaps.** The model is a copy (pinned) with two requests and a
  reader; the build is one write per cell; the fork reset runs on one
  thread. That `LINE_STATE` has no other writer is checked by `run.sh`
  syntactically (writes through `LINE_STATE.` in `src/` outside the
  tests); a write through a raw pointer to it would be caught only by
  review. The argument that the release/acquire pair suffices for the
  hand-off is the one above, checked by the model for this instance,
  not proved in a weak-memory logic.

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
- A copy (`init.rs`, `exit.rs`, `slab.rs`, the line and record steps of
  `oncecell.rs`, the class steps of
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

Written at the ONCECELL commit, 2026-10-07. A fresh-context subagent
reviewed the protocol code, the models, the pins, the `run.sh` checks
and this record against the code they run or copy, with a strict
soundness pass over the trusted items it rests on; it found no
overclaim in the records and no soundness problem in this seam. Its
findings elsewhere are folded in: the fork-child resets' call-site
test scans every non-test source file, and the orphan axioms'
residue says the child's second write can race with another thread
the child has started.

## Tests

`sh verus/loom/run.sh` (`docs/testing.md`, "loom model checks") first
checks that `verus/SEAMS.md` is current, that the contract above is
still the ledger's entry for this seam, that the models the `Models:`
line names exist and are not ignored, that the source lines the copies
mirror are in place (`verus/loom/pins.txt`), and, by name, the atomic
writes the records rely on (among them: `LINE_STATE` has two writes
outside the tests, `line_compare_exchange`'s and `line_store`'s;
non-test `sys/cgroup.rs` writes a record's phase only through
`vspec/byte.rs`, and only it (and the loom models) calls those
wrappers; `vspec/byte.rs` builds a `BytePerm` only in `new`, with one
literal; no `const` item names `byte::new`, `Deferred::now` or
`Deferred::later`); then it runs every loom model and control. A body
that breaks the property fails a model where the model runs the real
code (`vspec/byte.rs`); a changed copied line fails the pin check; a
changed contract fails the ledger check, until this record and its
models are revisited. The script is run by hand (about 15 minutes with
the build); no CI job runs it yet.
