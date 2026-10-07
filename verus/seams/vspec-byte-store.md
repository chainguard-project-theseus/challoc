# `store` (`vspec/byte.rs`)

Seam S271 of `verus/SEAMS.md`: src/vspec/byte.rs:114. Kind: external_body. Category: atomics primitive.
Discharged by the C11 release/acquire rules and loom (part 2).

## Contract

At the ONCECELL commit (the once-cells' invariants, which introduced it):

```rust
pub fn store(b: &AtomicU8, v: u8, ordering: Ordering, Tracked(perm): Tracked<&mut BytePerm>)
    requires
        old(perm).id() == byte_id(b),
        !(ordering is Acquire),
        !(ordering is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
```

The body: `b.store(v, ordering)`, the bare `core` call under
`#[inline(always)]`. Its one caller is `Deferred::publish` (`DONE`,
`Release`), inside the phase's invariant (`sys::cgroup::PhasePred`),
after the winner's write of the cell and its `found`; the store
freezes the cell's permission into the invariant.

## Statement

Two parts.

1. **C11.** A deferred record's `phase` is an atomic whose writes are
   its initial value (`vspec::byte::new` in `Deferred::now`, `DONE`, or
   `Deferred::later`, `PENDING`; the record is moved into the `Shared`
   that `init` builds, before `STATE`'s `READY`), the successful
   exchanges of `compare_exchange` (`Deferred::claim`'s `PENDING` to
   `RUNNING`, and `Deferred::fork_child`'s `RUNNING` back to `PENDING`,
   which only a fork child's one thread runs) and `store`'s `DONE`
   store (`Deferred::publish`): non-test `sys/cgroup.rs` writes the
   phase only through these wrappers, which `run.sh` checks. All writes
   are in one modification order ([intro.races] p4); a
   read-modify-write reads the last value before its own write in that
   order ([atomics.order] p10), for every ordering; and by the coherence
   rules ([intro.races] p15-p18; RC11's coherence axiom) the accesses to
   the phase can be put in one total order, consistent with
   happens-before, in which every read reads the last write before it.
   A failed `compare_exchange` writes nothing and returns the value it
   read; the strong form fails only when that value differs from
   `current` (`core`: only the weak form "is allowed to spuriously
   fail"). A release store and an acquire load that reads it
   synchronise ([atomics.order] p2): what the storing thread wrote
   before the store happens before what the loading thread does after
   the load.
2. **loom.** `src/loom_tests/oncecell.rs` (see the common part below)
   runs the real wrappers of `vspec/byte.rs` (`load`,
   `compare_exchange`, `store`, compiled over loom's `AtomicU8`) with
   the orderings `sys/cgroup.rs` passes, driven by copies of
   `Deferred::get`, `Deferred::discover` (without `found`) and
   `Deferred::fork_child` (`sys` is not in the `cfg(loom)` build; the
   copied lines are pinned in `verus/loom/pins.txt`), the cell as one
   loom `UnsafeCell`. Exhaustive (no preemption bound):

Models: `loom_oncecell_deferred_discover_races_get`, `loom_oncecell_deferred_relaxed_publish_is_a_race`, `loom_oncecell_deferred_fork_child_restarts`

   - `loom_oncecell_deferred_discover_races_get` (933 executions): a
     tick discovers without waiting while a reader of the figures
     discovers and waits, and the main thread samples (`get`); exactly
     one discovery runs, the waiting reader returns with the record
     published and reads it, the sampler sees nothing or the record,
     with no race on the cell, and no thread is left spinning.
   - `loom_oncecell_deferred_relaxed_publish_is_a_race` (control): with
     the `DONE` store `Relaxed`, loom reports a read of the cell as a
     causality violation.
   - `loom_oncecell_deferred_fork_child_restarts` (1 execution, one
     thread, as the child has): the reset from `RUNNING` makes the
     child's waiting discovery run rather than spin for ever, and a
     reset of a published record leaves it.

## Translation

- **The identity.** `requires ... id() == byte_id(b)`: the permission
  is for this byte; part 1 is about that byte alone. The identity is a
  ghost one, carried by the atomic's value (`vspec/byte.rs`'s trusted
  list): `new` mints the one permission with the atomic it makes, and a
  record's phase is made by `new` in `Deferred::now` or
  `Deferred::later`, then moved into place.
- **`requires !(ordering is Acquire)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `store` panics; with them excluded
  the body is one atomic store and returns (`no_unwind`). The call site
  passes `Release`.
- **`ensures`**: `final(perm).value() == v`: the store is a write, the
  next one in the phase's modification order after the last write
  before it in part 1's per-location order, so the permission, read as
  in `vspec-byte-load.md`, holds `v` from there on;
  `final(perm).id() == old(perm).id()` is ghost.
- **`opens_invariants none`**: the body opens no Verus invariant (read
  from the source).
- **What the SC proof carries across locations.** `publish` puts the
  cell's permission, the record written, into the invariant at this
  store, frozen (`&'static`), for every thread that later loads `DONE`
  (`load_phase`). Under C11 a reader's read of the record must happen
  after the discoverer's write: the write is before this release store
  in program order, the read after the reader's acquire load of
  `DONE`, and the two synchronise (part 1); the model's `Relaxed`
  control shows the read racing without it. After `DONE` nothing writes
  the phase (the fork reset only acts on `RUNNING`), so the frozen
  permission stays valid: what the proof's `DONE ==> frozen` invariant
  states. A waiting `discover` (a plain `Acquire` load until `DONE`)
  and its caller `Shared::cgroup_now`, which then calls `get`, read the
  record only through `get`'s own acquire load.
- **Gaps.** As in `vspec-byte-load.md`. The fork child's double write
  when the forking thread was itself the discoverer is
  `sys::cgroup::axiom_cell_orphaned`'s residue, outside this contract.

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
