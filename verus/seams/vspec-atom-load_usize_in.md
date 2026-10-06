# `load_usize_in` (`vspec/atom.rs`)

Seam S266 of `verus/SEAMS.md`: src/vspec/atom.rs:95. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 3).

## Contract

At commit 82ca40d7:

```rust
pub fn load_usize_in<P: InvariantPredicate<(int, usize), UsizePerm>>(
    a: &AtomicUsize,
    ordering: Ordering,
    inv: &Tracked<AtomicInvariant<(int, usize), UsizePerm, P>>,
) -> (r: usize)
    requires
        inv@.constant().0 == usize_id(a),
        forall|p: UsizePerm| #[trigger] P::inv(inv@.constant(), p) ==> p.id() == inv@.constant().0,
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        exists|p: UsizePerm| #[trigger] P::inv(inv@.constant(), p) && p.value() == r,
    opens_invariants [inv@.namespace()]
    no_unwind
```

The body, at that commit: `a.load(ordering)`; `inv` is named only by
the specification. Its one call site is `SlabTable::watermark`, with
`P` = `table::CommittedPred` (the permission's value is at most the
table's slab count) and the caller's ordering: `Acquire` from
`committed_slabs()`, `Relaxed` from `is_handed_out` (the `free` path)
and `desc_if_committed`.

## Statement

Two parts.

1. **C11, per atomic.** For any one atomic object, C++20 (which Rust
   adopts) orders all its writes in one modification order consistent
   with happens-before ([intro.races] p4, p15-p18), and a load reads a
   write of that order no older than the last write that happens before
   it; equivalently (RC11's coherence axiom), the accesses to one atomic
   can be put in one total order, consistent with happens-before, in
   which every read reads the last write before it. `core`'s `load`
   panics for `Release` and `AcqRel` and is otherwise one atomic load.
2. **loom.** The models of `vspec-atom-store_usize.md` (see the common
   part below), which run this wrapper over loom's `AtomicUsize` with
   both orderings of the call site: `src/loom_tests/watermark.rs`
   (`Acquire`, then `Relaxed`, while two threads hand out groups),
   `src/loom_tests/free.rs` (the claimer's `Acquire` load, the free
   path's `Relaxed` one) and `src/loom_tests/bitmap.rs` (a claimer's
   `Acquire` load). The invariant argument is a ghost value
   (`Tracked::assume_new()`, no bytes) of a test predicate that Verus
   never sees; the models check the values the loads return
   themselves. Exhaustive:

Models: `loom_watermark_take_group_publishes`, `loom_watermark_relaxed_store_is_unpublished`, `loom_free_after_synchronised_handoff`, `loom_free_after_relaxed_handoff`, `loom_free_unpublished_owner_is_misrouted`, `loom_bitmap_init_is_published`

   - `loom_watermark_take_group_publishes` (4482 executions): every
     load, of either ordering, returns at most the slab count, and a
     thread's loads never go back.
   - The free path's outcomes (`vspec-word-load.md`): after a
     synchronising hand-off (13337) the `Relaxed` load always finds the
     slab handed out; after a `Relaxed` hand-off (17501, outside the C
     contract) it may read the initial 0, a fast-path miss.
   - The controls of `vspec-atom-store_usize.md`.

## Translation

- **`requires inv@.constant().0 == usize_id(a)` and the `forall`**:
  every permission the invariant can hold is for `a`, so the permission
  inside `inv` is `a`'s; part 1 is about that atomic alone. That it is
  the only permission for `a` is `new_usize`'s, which is verified and
  the only code that builds a `UsizePerm` (`run.sh` checks it by name).
- **`requires !(ordering is Release)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `load` panics; with them excluded the
  body is one atomic load and returns (`no_unwind`). The call sites
  pass `Acquire` and `Relaxed`.
- **`ensures`.** The contract is what opening `inv` around a load
  through its permission would prove (the doc comment): the permission
  `p` held in `inv` satisfies `P::inv` at every step, and the load
  returns its value. Read `p.value()` as the value of the last write to
  `a` in the per-location order of part 1 (every write goes through
  `store_usize`, inside `inv`; `run.sh`'s access check). Part 1 places
  this load in that order right after the write it reads, so `r` is
  `p.value()` there and `p` is a witness of the `exists`, for every
  ordering `load` accepts. `P::inv` is a function of the invariant's
  constant key and of `p` alone, so it relates `a`'s value to constants
  only: nothing about another location is claimed.
- **`opens_invariants [inv@.namespace()]`**: the body opens no
  invariant (read from the source); declaring `inv`'s namespace only
  stops a caller from calling it while `inv` is open, as opening it
  would.
- **What the SC proof carries across locations.** Nothing:
  `CommittedPred` bounds the watermark by the slab count, a constant,
  and the proofs use only that bound. That a load past a slab sees the
  slab's bitmap and owner is the table's publication protocol, which
  `vspec-atom-store_usize.md` describes and the models check; it is not
  part of this contract.
- **Gaps.** Those of `vspec-atom-store_usize.md`; the predicate in the
  models is a stand-in (Verus is not run on them), so a model checks the
  loaded value against the bound itself, not `P`.

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
