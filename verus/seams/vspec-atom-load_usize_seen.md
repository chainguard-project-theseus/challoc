# `load_usize_seen` (`vspec/atom.rs`)

Seam S268 of `verus/SEAMS.md`: src/vspec/atom.rs:208. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 2), with the ghost
argument of part 1 for the copy it returns.

## Contract

At the package OWNER commit (parent add590d1):

```rust
pub fn load_usize_seen<K, P: InvariantPredicate<K, SeenVal>>(
    a: &AtomicUsize,
    ordering: Ordering,
    inv: &Tracked<AtomicInvariant<K, SeenVal, P>>,
) -> (r: (usize, Tracked<&'static GhostPersistentSubset<int>>))
    requires
        forall|v: SeenVal| #[trigger] P::inv(inv@.constant(), v) ==> v.0.id() == usize_id(a),
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        exists|v: SeenVal| #[trigger] P::inv(inv@.constant(), v) && v.0.value() == r.0
            && r.1@.id() == v.1.id() && r.1@@ == v.1@,
    opens_invariants [inv@.namespace()]
    no_unwind
```

The body, at that commit: `(a.load(ordering), Tracked::assume_new())`;
`inv` is named only by the specification. Its one call site is
`SlabTable::watermark_seen`, with `K` = `(int, usize, Loc)` and `P` =
`table::WatermarkPred` (the permission's value is at most the table's
slab count, and the persistent set holds the owner words' witness
marker of every slab below it), and the caller's ordering: `Acquire`
from `committed_slabs()` and `committed_seen()` (`SlabTable::owner`),
`Relaxed` from `is_handed_out`, `handed_out_seen` (`check_live`, so
`check_located` on the `free` slow path), `desc_if_committed` and
`mirror_watermark`. `SlabTable::watermark` drops the copy.

## Statement

Three parts.

1. **Ghost, the copy.** A `GhostPersistentSubset` is duplicable
   (`duplicate`, verified in vstd) and a duplicate frozen with
   `vstd::modes::tracked_static_ref` is a `&'static` to it: opening
   `inv` at the load's step, taking the permission's value and
   duplicating the set the invariant holds there is what verified code
   could do, and leaves the invariant as it was. The body's
   `Tracked::assume_new()` stands for that duplicate (no bytes).
2. **C11, per atomic.** As `vspec-atom-load_usize_in.md` part 1: the
   accesses to one atomic can be put in one total order consistent with
   happens-before, in which every read reads the last write before it;
   `core`'s `load` panics for `Release` and `AcqRel` and is otherwise
   one atomic load.
3. **loom.** The models of `vspec-atom-store_usize.md`, which run this
   wrapper over loom's `AtomicUsize` with the orderings of its call
   sites: `src/loom_tests/watermark.rs` (`Acquire`, then `Relaxed`,
   while two threads hand out groups; the owner words below an
   `Acquire` load are read through `vspec::atom::load_u16_at`),
   `src/loom_tests/free.rs` (the claimer's `Acquire` load and the slow
   path's `Relaxed` one) and `src/loom_tests/bitmap.rs` (a claimer's
   `Acquire` load). The invariant argument is a ghost value
   (`Tracked::assume_new()`, no bytes) of a test predicate Verus never
   sees; the models check the values the loads return themselves.
   Exhaustive:

Models: `loom_watermark_take_group_publishes`, `loom_watermark_relaxed_store_is_unpublished`, `loom_free_after_synchronised_handoff`, `loom_free_after_relaxed_handoff`, `loom_free_unpublished_owner_is_misrouted`, `loom_bitmap_init_is_published`

   - `loom_watermark_take_group_publishes` (4482 executions): every
     load, of either ordering, returns at most the slab count, a
     thread's loads never go back, and an `Acquire` load that returns
     `w` finds the owner of every group below `w` stored (the fact the
     copy stands for, `table::WatermarkPred`).
   - The free path's outcomes (`vspec-word-load.md`): after a
     synchronising hand-off (31214) the slow path's `Relaxed` load
     finds the slab handed out; after a `Relaxed` hand-off (50926,
     outside the C contract) it may read the initial 0, a miss.
   - `loom_bitmap_init_is_published` (6): a claimer that loads the
     watermark past the slab never claims `init`'s padding.
   - The controls of `vspec-atom-store_usize.md`
     (`loom_watermark_relaxed_store_is_unpublished`: with the store
     `Relaxed`, an `Acquire` load can return a group whose owner word
     reads 0; `loom_free_unpublished_owner_is_misrouted`, 91770: with
     the claimer's load `Relaxed`, a synchronised free can read the
     owner as 0).

## Translation

- **`requires` the `forall`**: every value the invariant can hold has
  `a`'s permission, so the permission inside `inv` is `a`'s; part 2 is
  about that atomic alone. That it is the only permission for `a` is
  `new_usize`'s (`vspec-atom-load_usize_in.md`).
- **`requires !(ordering is Release)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `load` panics; with them excluded the
  body is one atomic load and returns (`no_unwind`). The call sites
  pass `Acquire` and `Relaxed`.
- **`ensures`, the value.** As `vspec-atom-load_usize_in.md`: read
  `v.0.value()` as the value of the last write to `a` in the order of
  part 2 (every write goes through `store_usize`, inside `inv`;
  `run.sh`'s access check); `r.0` is that value, and `v` is a witness
  of the `exists`.
- **`ensures`, the copy.** `r.1@` has `v.1`'s location and elements:
  part 1, for the `v` of the previous bullet, the value the invariant
  held at the load's step. `P::inv` is a function of the invariant's
  constant key and of `v` alone.
- **`opens_invariants [inv@.namespace()]`**: the body opens no
  invariant (read from the source); declaring `inv`'s namespace only
  stops a caller from calling it while `inv` is open, as opening it
  would.
- **What the SC proof carries across locations.** `WatermarkPred`'s set
  is ghost: it names the owner words' witness markers, whose meaning is
  the owner words' invariants (`table::OwnerPred`): a slab's marker is
  persistent only once its group's owner word is stored
  (`store_owner`, before `take_group` stores the watermark past the
  group). So the SC proof concludes that a reader whose load returned a
  watermark past a slab reads the slab's owner (`table::load_owner`).
  Its weak-memory reading is the table's publication protocol, which
  the models check: the `Release` store and an `Acquire` load supply
  the edge (`loom_watermark_take_group_publishes`, and the controls show
  a `Relaxed` store or claimer load would not). The `Relaxed` loads of
  the `free` slow path (`handed_out_seen`) get it from the program: a
  free ordered after the slot's allocation, which happens after the
  hand-out, as C asks of a correct program
  (`loom_free_after_synchronised_handoff`); a free not so ordered can
  read a stale owner where the SC proof says it reads the stored one
  (`loom_free_after_relaxed_handoff`), the documented outcome
  `verus/WEAKMEM.md` P5 for programs with undefined behaviour in C, as
  for `vspec-atom-load_u16_at.md`.
- **Gaps.** Those of `vspec-atom-store_usize.md`; the predicate in the
  models is a stand-in (Verus is not run on them), so a model checks
  the loaded value and the owner words itself, not `P`; part 1 is an
  argument about vstd's resource algebra, not checked by a tool.

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

Fresh-context subagent, 2026-10-07: a strict soundness pass on package
OWNER's trusted items (`load_usize_seen`, `load_u16_at`, `store_owner`'s
new contract, and the new conjuncts of `axiom_table_lock_inv`,
`axiom_table_lock_frame` and `axiom_class_lock`) and on these records.
It found no soundness issue. Its findings are folded in: `run.sh` now
also checks that only `class/table.rs` names the owner words'
invariants, `load_u16_at` is `pub(crate)`, `owned_rec` is split into
one quantifier per conjunct, and four doc inaccuracies are fixed.

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
