# `store` (`vspec/count.rs`)

Seam S301 of `verus/SEAMS.md`: src/vspec/count.rs:124. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 3).

## Contract

At commit 82ca40d7:

```rust
pub fn store(c: &AtomicU32, v: u32, ordering: Ordering, Tracked(perm): Tracked<&mut CountPerm>)
    requires
        old(perm).id() == count_id(c),
        !(ordering is Acquire),
        !(ordering is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
```

The body, at that commit: `c.store(v, ordering)`. Its one call site is
`Arenas::create` (`Release`), which stores `index + 1` into the arena
count inside the count's invariant (`arena::NarenasPred`), under the
creation lock, after `build` has published the new arena's table entry.

## Statement

Two parts.

1. **C11, per counter.** For any one atomic object, C++20 (which Rust
   adopts) orders all its writes in one modification order consistent
   with happens-before ([intro.races] p4, p15-p18), and a load reads a
   write of that order no older than the last write that happens before
   it; equivalently (RC11's coherence axiom), the accesses to one atomic
   can be put in one total order, consistent with happens-before, in
   which every read reads the last write before it. `core`'s `store`
   panics for `Acquire` and `AcqRel` and is otherwise one atomic store.
2. **loom.** `src/loom_tests/narenas.rs` (see the common part below)
   runs the real wrappers of `vspec/count.rs` (`store`, `load`,
   compiled over loom's `AtomicU32`) with the orderings `arena.rs`
   passes, driven by copies of `Arenas::create` (under the real
   `RawLock` as the creation lock, its value the next index in a loom
   `UnsafeCell`: `None` at `MAX_ARENAS`, `build`'s write of the arena
   and `Release` store of its entry, the lock's value set to
   `index + 1`, then `store(index + 1, Release)`), `Arenas::count`
   (`load`, `Acquire`) and `Arenas::iter` (each entry below the count
   loaded `Acquire`, null skipped). `MAX_ARENAS` is 3 and arena 0, the
   one automatic arena, is built from the start: two threads each
   create an arena while the main thread scans the registry, then the
   main thread's third `create` finds the table full. Exhaustive:

Models: `loom_narenas_create_publishes`, `loom_narenas_relaxed_store_is_unpublished`

   - `loom_narenas_create_publishes` (4482 executions): every load of
     the count is between the automatic count and `MAX_ARENAS`, a
     thread's two loads never go back, a scan finds every explicit
     arena below the count it read built and reads its fields with no
     race, a `create` holding the lock loads the count as the lock's
     value and finds every explicit arena below it built, the full
     table returns `None`, and once every thread is joined the count
     is the lock's value (3).
   - Control: with the count stored `Relaxed`
     (`loom_narenas_relaxed_store_is_unpublished`), a scan reads a count
     whose last arena's entry is still null.

## Translation

- **`requires old(perm).id() == count_id(c)`**: the permission is for
  this counter; part 1 is about that counter alone.
- **`requires !(ordering is Acquire)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `store` panics; with them excluded
  the body is one atomic store and returns (`no_unwind`). The call site
  passes `Release`.
- **`ensures`.** Read `perm.value()` as the value of the last write to
  the counter in its modification order among those performed so far
  (every write to the arena count goes through these wrappers, which
  `run.sh` checks; `Arenas::init` makes it with `vspec::count::new`
  before the registry is published). A store is a write: after it the
  value is `v` (`final(perm).value() == v`) until the next write;
  `final(perm).id() == old(perm).id()` is ghost. A plain store
  overwrites any concurrent write; here every write is under the
  creation lock, whose release/acquire edge (`RawLock`,
  `lock-LockGuard-drop.md`) orders the stores in happens-before and so
  in modification order, which the model checks (the final count is
  the lock's value).
- **`opens_invariants none`**: the body opens no Verus invariant (read
  from the source).
- **What the SC proof carries across locations.** Since package P8,
  `NarenasPred` names the count, every table entry (`vspec/entry.rs`)
  and ghost state: the frozen permissions of the published arenas and
  the authority of the registry's snapshot, whose other half is in the
  creation lock's value (`arena::Created`). The count's facts the
  verified proofs use are still per location: a load returns at most
  `MAX_ARENAS` (`Arenas::count`: the fork handlers' lock order and the
  registry scans), which part 1 gives, every write being in bounds (the
  initial automatic count and each `index + 1` with
  `index < MAX_ARENAS`, which Verus proves at the write), and a load
  under the creation lock reads the snapshot's count
  (`Arenas::count_frozen`, for `api::count_held`), which needs the
  holder's read to see the last write, made by an earlier holder: the
  lock's release/acquire edge orders the two, which the model checks
  (`create` asserts the count is the lock's value under the lock). A
  scan's finding the arenas below the count it read built is still the
  registry's protocol, which no proof depends on (a null entry is
  skipped); the control shows a `Relaxed` store breaks it.
- **Gaps.** 2 creators and one scanner; `MAX_ARENAS` 3 and one
  automatic arena, built from the start (the lazy build of automatic
  arenas by `Arenas::get` is not modelled); `build` cannot fail, and its
  `set_multi_arena` (a `Relaxed` flag stored before the entry) and its
  spare storage (`Arenas::spare`, touched only under the creation lock)
  are left out; the fork handlers' reads are not modelled, only
  `create`'s under the lock; the `Arenas` steps are copies (pinned,
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
