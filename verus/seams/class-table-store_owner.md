# `store_owner` (`class/table.rs`)

Seam S86 of `verus/SEAMS.md`: src/class/table.rs:502. Kind: external_body. Category: allocator invariant.
Discharged by loom and the C11 coherence rules (part 3); the store's
uniqueness is Verus's (the owner-store markers it uses up).

## Contract

At the package OWNER commit (parent add590d1):

```rust
fn store_owner(
    w: *const AtomicU16,
    arena: u16,
    Ghost(t): Ghost<SlabTable>,
    Ghost(group): Ghost<usize>,
    Tracked(perm): Tracked<&mut PointsTo<AtomicU16>>,
    Tracked(marks): Tracked<GhostSubset<int>>,
)
    requires
        old(perm).ptr() == w,
        old(perm).is_init(),
        marks.id() == mark_loc(t.spec_class() as int),
        owner_marks(t.spec_group_range(group).0 as int, t.spec_group_range(group).1 as int).subset_of(marks@),
    ensures
        final(perm).ptr() == old(perm).ptr(),
        final(perm).is_init(),
        u16_value(final(perm).value()) == arena,
        forall|x: int| t.spec_group_range(group).0 <= x < t.spec_group_range(group).1
            ==> #[trigger] slab_owner(t.spec_class() as int, x) == arena,
    opens_invariants none
    no_unwind
```

The body, at that commit: `unsafe { (*w).store(arena, Ordering::Relaxed) }`
(`w.store(arena, Ordering::Relaxed)` on the word's reference before
package OWNER, which added `perm` and its `ensures` and the atomic
attribute). Its one caller is `SlabTable::take_group`, under the commit
lock, for the group at the watermark, past every fallible step, inside
the word's invariant (`table::OwnerPred`), which holds `perm`;
`take_group` then stores the watermark past the group with `Release`.

## Statement

Two parts.

1. **C11, per word.** A store is one write in the word's modification
   order; a load reads some write of that order, and by write-read
   coherence ([intro.races] p18) one that the store happens before reads
   the store or a later write. With the one store per word (the
   markers, below), the word's writes are its initial 0 and this store.
2. **loom.** The models of `src/loom_tests/free.rs` (described in
   `vspec-word-load.md` and `vspec-atom-load_u16_at.md`), whose hand-out
   step copies `take_group`'s order: bitmap `init`, the owner store
   (`Relaxed`), the watermark store (`Release`); and those of
   `src/loom_tests/watermark.rs`, whose `take_group` copy stores each
   group's owner `Relaxed` before the watermark, the readers loading the
   words through `vspec::atom::load_u16_at`.

Models: `loom_free_after_synchronised_handoff`, `loom_free_unpublished_owner_is_misrouted`, `loom_watermark_take_group_publishes`

   - `loom_free_after_synchronised_handoff` (31214 executions): every
     free ordered after the hand-out through the claimer's `Acquire`
     watermark load and a synchronising hand-off reads the stored arena.
   - `loom_watermark_take_group_publishes` (4482): a reader whose
     `Acquire` load returns `w` reads the stored arena of every group
     below `w`.
   - `loom_free_unpublished_owner_is_misrouted` (control, 91770):
     without the `Acquire` edge it can read 0.

## Translation

- **`requires old(perm).ptr() == w`, `old(perm).is_init()`**: `perm` is
  the permission to the word at `w`, which `take_group` takes from the
  word's invariant; part 1 is about that word.
- **`requires` on `marks`**: the call uses up the owner-store markers of
  the group's slabs, which the class's table minted once each and its
  commit lock holds for the slabs from the watermark on. That makes this
  the group's only store of an owner: Verus checks the markers'
  accounting, under the lock's premise (`axiom_table_lock_inv`, a
  separate seam). No memory-model content.
- **`ensures final(perm).ptr() == old(perm).ptr()`, `is_init()`**: the
  store writes through the pointer the permission is to, and leaves the
  word initialised.
- **`ensures u16_value(final(perm).value()) == arena`**: read the
  permission's value as the value of the word's last write in its
  modification order (`vspec-atom-load_u16_at.md`): after the store it
  is `arena`, and with this the only store, it stays so.
- **`ensures` `slab_owner(c, x) == arena` for the group's slabs**: a
  ghost fact, timeless in Verus. Its weak-memory reading is that every
  load of the word that the store happens before returns `arena`: with
  the store the word's only write after its initial 0, part 1 gives
  that. Loads not ordered after the store may read 0. Excluding them is
  the reader's business: since package OWNER, `table::load_owner`
  requires a witness that the reader's load of the watermark passed the
  group (`vspec-atom-load_usize_seen.md`, whose translation says which
  loads are so ordered, and `verus/WEAKMEM.md` P5 for the programs where
  they are not). This record claims the store's half only.
- **`opens_invariants none`**, **`#[verifier::atomic]`**: the body is
  one atomic store and opens no invariant (read from the source), so it
  can run inside the word's invariant; it returns (`no_unwind`).
- **Gaps.** One group of one slab (`free.rs`) or two (`watermark.rs`),
  at most 4 threads; the class lock and sets between `take_group` and a
  claimer are stood in for by the claimer's `Acquire` watermark load;
  the steps are copies (pinned, `verus/loom/pins.txt`). The word's
  initial 0 is the committed metadata page's; that a handed-out group's
  owner page stays committed is not checked here.

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
