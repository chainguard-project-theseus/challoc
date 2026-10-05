# `WordPerm` (`vspec/word.rs`)

Seam S397 of `verus/SEAMS.md`: src/vspec/word.rs:50. Kind: external_body type. Category: atomics primitive.
Discharged by the records of its wrappers and `run.sh`'s access check
(part 3).

## Contract

At commit 0c9cd77:

```rust
pub tracked struct WordPerm
```

The type is an `external_body` tracked struct with one private unit
field; `addr()` and `value()` are uninterpreted spec functions on it. It
has no `Copy` or `Clone`, and no exec constructor: permissions come only
from `table::axiom_fresh_words` (S87, a separate seam), once per class
bitmap, by `SlabTable::new`.

## Statement

What Verus assumes of the type is what it is used for: a permission is a
ghost token that stands for one atomic (the word whose `word_addr` is
its `addr()`) and carries its current value. That splits into three
facts, each checked elsewhere:

1. **Not duplicable.** Verus checks it: a tracked value of a type with
   no `Copy` moves, and nothing in `src/` implements `Copy` or `Clone`
   for it (it is declared in `vspec/word.rs` only, with no derive).
2. **One permission per atomic.** The minting seam's
   (`table::axiom_fresh_words`, S87, once per class bitmap, by
   `SlabTable::new`); not claimed here.
3. **The value is the atomic's.** Every write of the atomic once it is
   shared, and every read whose value a proof uses, is one of the
   wrappers in `vspec/word.rs`, each of which has a record reading
   `value()` as the value of the last write in the atomic's per-location
   order (C11 coherence) and checking the protocol around it with loom:
   `vspec-word-fetch_or.md`, `vspec-word-fetch_and.md`,
   `vspec-word-store.md` and `vspec-word-load.md`, all discharged.
   `run.sh` checks this by name: non-test `src/bitmap.rs` writes atomics
   only through the wrappers (its other loads are plain `core` loads,
   whose result Verus takes as any value), and `src/class/table.rs`,
   which casts word addresses to `&AtomicU64` for the bitmap and the bit
   test, writes atomics only on two listed lines and through
   `vspec/atom.rs`'s `store_usize` (the watermark), none a bitmap word.

Models: `loom_bitmap_release_then_reserve`, `loom_bitmap_two_claimers`, `loom_bitmap_init_is_published`, `loom_free_after_synchronised_handoff`

## Translation

- **No `requires`, no `ensures`**: the type's contract is the meaning of
  its spec functions, which facts 1-3 give, under the per-location
  reading the wrappers' records state.
- **Across locations.** Verus composes all permissions in one
  interleaving; C11 gives one order per atomic. The wrappers' records
  say what the SC proofs carry across locations and why the orderings
  supply it (or, for the bit test, why nothing needs to be carried);
  this record adds nothing to that.
- **Gaps.** Those of the wrappers' records (the models' bounds, copies
  of the surrounding steps), the minting seam, and the access check,
  which matches source text: a write through a name or pointer it does
  not look for is caught only by review.

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
