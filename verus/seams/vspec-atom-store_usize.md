# `store_usize` (`vspec/atom.rs`)

Seam S267 of `verus/SEAMS.md`: src/vspec/atom.rs:118. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 3).

## Contract

At commit 82ca40d7:

```rust
pub fn store_usize(a: &AtomicUsize, v: usize, ordering: Ordering, Tracked(perm): Tracked<&mut UsizePerm>)
    requires
        old(perm).id() == usize_id(a),
        !(ordering is Acquire),
        !(ordering is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
```

The body, at that commit: `a.store(v, ordering)`. Its one call site is
`SlabTable::take_group` (`Release`), which stores the end of the group
it hands out into the watermark inside the watermark's invariant
(`table::CommittedPred`), under the table's commit lock, after the
group's bitmap `init` and its owner word (`store_owner`).

## Statement

Two parts.

1. **C11, per atomic.** For any one atomic object, C++20 (which Rust
   adopts) orders all its writes in one modification order consistent
   with happens-before ([intro.races] p4, p15-p18), and a load reads a
   write of that order no older than the last write that happens before
   it; equivalently (RC11's coherence axiom), the accesses to one atomic
   can be put in one total order, consistent with happens-before, in
   which every read reads the last write before it. `core`'s `store`
   panics for `Acquire` and `AcqRel` and is otherwise one atomic store.
2. **loom.** Three model files (see the common part below) run the real
   wrappers of `vspec/atom.rs` (`store_usize`, `load_usize_in`, compiled
   over loom's `AtomicUsize`) with the orderings `class/table.rs`
   passes:
   - `src/loom_tests/watermark.rs`: copies of `take_group` (under the
     real `RawLock` as the commit lock, its value `next_slab` in a loom
     `UnsafeCell`: `None` at `s >= nslabs`, the `check_zero` comparison
     of `s` with a `Relaxed` load of the watermark, the owner word
     stored `Relaxed`, `next_slab = end`, then `store_usize(end,
     Release)`) and of `SlabTable::watermark` (`load_usize_in`,
     `Acquire` as `committed_slabs()`, then `Relaxed` as
     `is_handed_out` and `desc_if_committed`), over 2 slabs of one slab
     per group: two threads each hand out a group (to arenas 1 and 2)
     while the main thread reads, then a third hand-out finds the
     region full.
   - `src/loom_tests/free.rs` (`vspec-word-load.md`): the hand-out's
     tail (the real `AtomicBitmap::init`, the owner word, then
     `store_usize(1, Release)`), a claimer's `Acquire` load and the
     free path's `Relaxed` load (`is_handed_out`).
   - `src/loom_tests/bitmap.rs`: the real `init`, then `store_usize(1,
     Release)`, and a claimer's `Acquire` load.

   Exhaustive:

Models: `loom_watermark_take_group_publishes`, `loom_watermark_relaxed_store_is_unpublished`, `loom_free_after_synchronised_handoff`, `loom_free_unpublished_owner_is_misrouted`, `loom_bitmap_init_is_published`

   - `loom_watermark_take_group_publishes` (4482 executions): every
     load returns at most the slab count, a thread's loads never go
     back, the `check_zero` comparison under the lock always finds the
     watermark at `next_slab`, an `Acquire` load that returns `w` sees
     the owner of every group below `w` stored, the full region returns
     `None`, and once every thread is joined the watermark is the slab
     count.
   - `loom_free_after_synchronised_handoff` (13337): a free that
     receives the slot through a synchronising hand-off finds the slab
     handed out (the watermark past it), the bit set and the owner
     stored. `loom_bitmap_init_is_published` (6): a claimer that loads
     the watermark past the slab never claims `init`'s padding.
   - Controls: with the watermark stored `Relaxed`
     (`loom_watermark_relaxed_store_is_unpublished`), an `Acquire` load
     can return a group whose owner word still reads 0; with the
     claimer's load `Relaxed` (`loom_free_unpublished_owner_is_misrouted`,
     38029), a synchronised free can read the owner as 0.

## Translation

- **`requires old(perm).id() == usize_id(a)`**: the permission is for
  this atomic; part 1 is about that atomic alone.
- **`requires !(ordering is Acquire)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `store` panics; with them excluded
  the body is one atomic store and returns (`no_unwind`). The call site
  passes `Release`.
- **`ensures`.** Read `perm.value()` as the value of the last write to
  the atomic in its modification order among those performed so far
  (every write to the watermark goes through this wrapper, which
  `run.sh` checks; `SlabTable::new` makes it with `new_usize` before the
  table is shared). A store is a write: after it the value is `v`
  (`final(perm).value() == v`) until the next write;
  `final(perm).id() == old(perm).id()` is ghost. A plain store
  overwrites any concurrent write; here every write is under the commit
  lock, whose release/acquire edge (`lock-LockGuard-drop.md`) orders
  the stores in happens-before and so in modification order, which the
  model checks (the `check_zero` comparison, and the final watermark).
- **`opens_invariants none`**: the body opens no Verus invariant (read
  from the source).
- **What the SC proof carries across locations.** `CommittedPred` names
  one atomic and a constant (the table's slab count); no invariant
  relates the watermark to another location. The proofs use only that a
  load returns at most the slab count (`SlabTable::watermark`), which
  part 1 gives: every write is in bounds (the initial 0 and each `end`,
  which Verus proves at most `nslabs` at the write), and a load returns
  one of them. That a load past a slab sees the slab's bitmap and owner
  word is the table's publication protocol, a premise of
  `load_owner` (`class-table-load_owner.md`) and `desc_unchecked`, not
  part of this contract; the models check that the `Release` store and
  an `Acquire` load supply it, and the controls show a `Relaxed` store
  or load would not.
- **Gaps.** 2 hand-outs, one slab per group, at most 3 threads; the
  commits (`ensure`, and its failure, which returns before the store),
  the descriptors (`SlabDesc::init`, the `owner_mismatch` check,
  `check_zero`'s descriptor check), the load of the previous owner word
  and the THP advice after the store are not modelled; for
  `desc_if_committed` the model checks the bound and the order of the
  loads, not its descriptor reads; the table steps are copies (pinned,
  `verus/loom/pins.txt`). The argument that any ordering keeps an
  invariant over one atomic and constants is the one above, checked by
  the models for these instances, not proved in a weak-memory logic.

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
