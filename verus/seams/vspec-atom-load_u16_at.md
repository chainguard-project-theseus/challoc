# `load_u16_at` (`vspec/atom.rs`)

Seam S269 of `verus/SEAMS.md`: src/vspec/atom.rs:246. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 2).

## Contract

At the package OWNER commit (parent add590d1):

```rust
pub(crate) unsafe fn load_u16_at(p: *const AtomicU16, ordering: Ordering, Tracked(perm): Tracked<&PointsTo<AtomicU16>>) -> (r: u16)
    requires
        perm.ptr() == p,
        perm.is_init(),
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r == u16_value(perm.value()),
    opens_invariants none
    no_unwind
```

The body, at that commit: `unsafe { (*p).load(ordering) }`, the load
`ptr_ref(p, perm).load(ordering)` made before it. Its call sites are
the class tables' owner word loads, each `Relaxed`, inside the word's
invariant (`table::OwnerPred`), which holds `perm`:
`table::load_owner` (for `SlabTable::owner` and `check_located`, with
the witness that the group was handed out), `SlabTable::owner_from`
(the `free` fast path's `ClassLine::owner`, which only compares the
value) and `SlabTable::take_group`'s cross-check of the previous owner
(under the commit lock, before the store).

## Statement

Two parts.

1. **C11, per word.** As `class-table-store_owner.md` part 1: an owner
   word's writes are, in modification order, its initial value 0 (the
   committed metadata page is zero) and `store_owner`'s one store; by
   write-read coherence ([intro.races] p18) a load that the store
   happens before reads it (there is no later write), and a load not
   ordered after it may read 0. `core`'s `load` panics for `Release`
   and `AcqRel` and is otherwise one atomic load.
2. **loom.** The models of `src/loom_tests/free.rs` and
   `src/loom_tests/watermark.rs` run this wrapper over loom's
   `AtomicU16` for the owner loads: the `free` fast path's (as
   `owner_from`, after the class line's watermark copy and the bit
   test) and a reader's after an `Acquire` load of the watermark (as
   `load_owner` from `SlabTable::owner`). The permission argument is a
   ghost value (`Tracked::assume_new()`, no bytes). Exhaustive:

Models: `loom_watermark_take_group_publishes`, `loom_watermark_relaxed_store_is_unpublished`, `loom_free_after_synchronised_handoff`, `loom_free_after_relaxed_handoff`, `loom_free_unpublished_owner_is_misrouted`

   - `loom_watermark_take_group_publishes` (4482 executions): after an
     `Acquire` load of the watermark that returns `w`, every owner word
     below `w` reads its stored arena.
   - `loom_free_after_synchronised_handoff` (31214): with a
     `Release`/`Acquire` hand-off of the slot, every free reads the
     stored arena.
   - `loom_free_after_relaxed_handoff` (50926): with a `Relaxed`
     hand-off the free can read the owner as 0, besides the other stale
     outcomes (a fast-path miss; the slow path re-reads,
     `vspec-word-load.md`).
   - Controls: `loom_watermark_relaxed_store_is_unpublished` (with the
     watermark stored `Relaxed`, a word below an `Acquire` load can read
     0) and `loom_free_unpublished_owner_is_misrouted` (91770; with the
     claimer's watermark load `Relaxed`, a free after a synchronising
     hand-off can read 0).

## Translation

- **`requires perm.ptr() == p`, `perm.is_init()`**: `p` is the word
  `perm` is to, initialised; part 1 is about that word alone.
- **`requires !(ordering is Release)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `load` panics; with them excluded the
  body is one atomic load and returns (`no_unwind`). The call sites
  pass `Relaxed`.
- **`ensures r == u16_value(perm.value())`.** Read `u16_value` of the
  permission's value as the value of the last write to the word in its
  modification order: every write to an owner word goes through
  `store_owner`, which sets it to the stored arena, inside the word's
  invariant (`run.sh` checks that only `class/table.rs` names the words'
  invariants, `owner_invs`, and that it writes the words
  only there and in the test helper `corrupt_owner_word`), and before
  the first, the word is the zero of its committed page (unconstrained
  in Verus: `OwnerPred` claims a value only once the word is stored).
  Under SC the load reads that write; under C11 part 1 gives it for a
  load the store happens before. Which loads are so ordered is not in
  this contract: `load_owner`'s readers hold the witness that the
  watermark passed the group (`vspec-atom-load_usize_seen.md`, whose
  translation states the weak-memory reading of it and its `Relaxed`
  readers, `verus/WEAKMEM.md` P5); `owner_from` and the cross-check
  claim nothing of the value.
- **`opens_invariants none`**, **`#[verifier::atomic]`**: the body is
  one atomic step and opens no invariant (read from the source), so it
  can run inside the word's invariant.
- **Gaps.** Those of `class-table-store_owner.md` (one group of one
  slab, the copies of the table steps, the class lock and sets between
  `take_group` and a claimer stood in for by the claimer's `Acquire`
  watermark load); the cross-check's load (`take_group`) is not
  modelled.

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
