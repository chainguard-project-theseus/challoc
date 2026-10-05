# `load_owner` (`class/table.rs`)

Seam S95 of `verus/SEAMS.md`: src/class/table.rs:295. Kind: external_body. Category: allocator invariant.
Evidence from loom and the C11 coherence rules (part 3): they settle the
memory-model half (a load ordered after the owner store reads it); that
every caller's load is so ordered, the doc comment's premise, is not in
the contract and stays an assumption.

## Contract

At commit 0c9cd77:

```rust
fn load_owner(w: &AtomicU16, Ghost(t): Ghost<SlabTable>, Ghost(slab): Ghost<usize>) -> (r: u16)
    requires
        slab < t.spec_nslabs(),
        *w == owner_word_at(t, slab >> t.lg_group),
    ensures
        r == slab_owner(t.spec_class() as int, slab as int),
    opens_invariants none
```

The body, at that commit: `w.load(Ordering::Relaxed)`. The contract's
`requires` say only which word it is; the doc comment adds a premise
Verus does not check: the caller has seen the watermark past `slab` (the
safety condition of `SlabTable::owner_at`, its one caller), so the group
was handed out and its word was stored once, by `store_owner`, before
the watermark moved. Without that premise the `ensures` is false even
under SC: for a slab below `nslabs` never handed out the load returns 0,
while `slab_owner` of it is whatever a later `store_owner` makes it.
Callers: `owner` (after an `Acquire` load of the watermark),
`check_located` (after `check_live`'s `Relaxed` watermark load and bit
test) and `owner_unchecked` (from `Shared::checked_slot`, the `free`
fast path, after `set_slot`'s `Relaxed` watermark load and bit test).

## Statement

Two parts.

1. **C11, per word.** The owner word's writes are, in modification
   order, its initial value 0 (the committed metadata page is zero) and
   `store_owner`'s one store (see `class-table-store_owner.md` for why
   there is only one). By write-read coherence ([intro.races] p18) a
   load that the store happens before reads the store (there is no later
   write); a load not ordered after it may read 0.
2. **loom.** The models of `src/loom_tests/free.rs`, described in
   `vspec-word-load.md`: a copy of `take_group`'s tail (bitmap `init`,
   the owner store, the watermark's `Release` store), a claimer that
   finds the slab through an `Acquire` load of the watermark, and a free
   path copied from `checked_slot`/`set_slot` (`Relaxed` watermark, the
   real `vspec::word::load` bit test, then the owner load, `Relaxed`, as
   `load_owner`).

Models: `loom_free_after_synchronised_handoff`, `loom_free_after_relaxed_handoff`, `loom_free_unpublished_owner_is_misrouted`

   - `loom_free_after_synchronised_handoff` (13337 executions): with a
     `Release`/`Acquire` hand-off of the slot, every free reads the
     stored arena.
   - `loom_free_after_relaxed_handoff` (17501): with a `Relaxed`
     hand-off the free can read the owner as 0, besides the other stale
     outcomes (on the fast path that is a miss; the slow path re-reads,
     `vspec-word-load.md`).
   - `loom_free_unpublished_owner_is_misrouted` (control, 38029): with
     the claimer's watermark load `Relaxed` (the hand-out's `Release`
     store kept), a free after a synchronising hand-off can read 0; so
     the claimer's `Acquire` load carries the edge the synchronised
     model relies on.

## Translation

- **`requires slab < t.spec_nslabs()`, `*w == owner_word_at(t, slab >>
  t.lg_group)`**: the word is the owner word of `slab`'s group; part 1
  is about that word.
- **`ensures r == slab_owner(c, slab)`, under the doc comment's
  premise.** Read "has seen the watermark past `slab`" as "the load
  happens after a watermark store past `slab`" (under SC the two are the
  same). The owner store precedes that watermark store in `take_group`'s
  program order, so it happens before the load, and part 1 gives `r` =
  the stored arena = `slab_owner` (`store_owner`'s `ensures`). This half
  is what the record establishes.
- **Why evidence: the premise.** That each caller meets the premise, so
  read, is an allocator invariant Verus does not check (the seam's
  category) and that a weak-memory tool cannot discharge alone:
  - `owner`: its watermark load is `Acquire` and reads a `Release` store
    past `slab` (every write of the watermark after `new` is such a
    store, by `take_group` under the commit lock), so it meets it.
  - `check_located` and `owner_unchecked` (the `free` paths): their
    watermark loads are `Relaxed` and give no edge. The edge comes from
    the program: the slot's bit read set, so some claim handed the slot
    out; for a block the program allocated and frees after the
    allocation, the claim happens after a slab hand-out past `slab` (the
    claimer found the slab through the class's lock and sets, which run
    after `take_group`; the model has an `Acquire` watermark load
    there), and the free happens after the claim. C asks a correct
    program to order its free after the allocation: C11 7.22.3p2 lets
    `malloc` and `free` "visibly modify the storage that they allocate
    or deallocate", which we read as making a free not so ordered a data
    race on the block.
  - A free not ordered after the slab's hand-out (a pointer passed
    through a `Relaxed` atomic, or an invalid free of a slot whose bit
    another thread's claim set, say a slot in another thread's cache)
    can read 0, where under SC it reads the stored arena
    (`loom_free_after_relaxed_handoff`). Such programs have undefined
    behaviour in C; the theorems that use `slab_owner` (arena isolation)
    hold under weak memory only for programs without it. This is
    WEAKMEM.md P5's documented outcome, not a new bug. Stating the
    premise in the `requires` (a ghost witness that the caller observed
    the watermark past `slab`, with the hand-off premise for the free
    paths) would leave the memory-model half above as the whole seam.
- **`opens_invariants none`**: the body opens no Verus invariant.
- **Gaps (model).** One group of one slab, 4 threads; `multi_arena` is
  not modelled; the class lock and sets between `take_group` and a
  claimer are stood in for by the claimer's `Acquire` watermark load;
  the steps are copies (pinned, `verus/loom/pins.txt`).

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
  10, 29839 and 213383). `src/loom_tests/dpor.rs` fails without it. The
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
- A copy (`init.rs`, `exit.rs`, the class steps of `bitmap.rs`, the
  table steps of `free.rs`, the arena steps of `count.rs`) cites the
  code it mirrors; `verus/loom/pins.txt` lists the source lines it
  copies, which `run.sh` requires in place and in order. A change
  elsewhere in the copied functions is caught by review only.

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
atomic writes the word and count records rely on are where they say
(non-test `bitmap.rs` writes words only through `vspec/word.rs`;
`class/table.rs` writes atomics only on three listed lines; `arena.rs`
touches the bound counts only through `vspec/count.rs` and
`Arenas::init`'s zeroing; `STATE` has two writes); then it runs every
loom model and control. A body that breaks the property fails a model
where the model runs the real code (`lock.rs`, `bitmap.rs`,
`vspec/word.rs`, `vspec/count.rs`); a changed copied line fails the pin
check; a changed contract fails the ledger check, until this record and
its models are revisited. The script is run by hand (about 15 minutes
with the build); no CI job runs it yet.
