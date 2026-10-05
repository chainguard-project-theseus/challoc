# `load` (`vspec/word.rs`)

Seam S389 of `verus/SEAMS.md`: src/vspec/word.rs:105. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 3).

## Contract

At commit 0c9cd77:

```rust
pub fn load(w: &AtomicU64, ordering: Ordering, Tracked(perm): Tracked<&WordPerm>) -> (r: u64)
    requires
        perm.addr() == word_addr(w),
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
```

The body, at that commit: `w.load(ordering)`. Its one call site is
`SlabTable::is_set_unchecked` (`Relaxed`), the `free` path's bit test,
inside the word's invariant; the proof uses the value only for its
`ensures` that a set bit is not in a guard slab (`bitmap::guards_ok`: a
guard slab's bits are clear in every value the invariant allows).

## Statement

Two parts.

1. **C11, per word.** C++20 (which Rust adopts) orders all writes to an
   atomic object in one modification order, and the coherence rules
   ([intro.races] p15-p18) make every load read a write of that order
   which is not older than the last write that happens before the load,
   and no later than any write the load happens before; read-read
   coherence keeps two loads in order. Equivalently (RC11's coherence
   axiom), the accesses to one location can be put in one total order,
   consistent with happens-before, in which every load reads the last
   write before it.
2. **loom.** The models of `src/loom_tests/free.rs` (see the common part
   below) run the real `vspec::word::load` (over loom's `AtomicU64`),
   with the real `AtomicBitmap` (`init`, `claim_from`, `release`)
   writing the word, and copies of the table steps around them: the
   hand-out (`take_group`'s bitmap `init`, `store_owner`, the
   watermark's `Release` store), a claimer that finds the slab through
   the watermark's `Acquire` load, and the free path's checks
   (`is_handed_out`, then the bit test, then the owner word). The
   allocating thread hands its slot to the freeing (main) thread through
   an atomic, while a third thread claims and releases the word's other
   slot. Exhaustive:

Models: `loom_free_after_synchronised_handoff`, `loom_free_after_relaxed_handoff`, `loom_free_relaxed_handoff_is_a_race`, `loom_free_unpublished_owner_is_misrouted`

   - `loom_free_after_synchronised_handoff` (`Release`/`Acquire`
     hand-off; 13337 executions): every free that receives the slot sees
     it handed out, its bit set and the owner stored, and its writes to
     the block do not race with the allocating thread's.
   - `loom_free_after_relaxed_handoff` (`Relaxed` hand-off, the free
     does not touch the block; 17501): records the outcomes; all four
     appear: the checks pass, the watermark reads below the slab, the
     bit reads clear, and the owner word reads its initial 0. Each of
     the last three fails the fast path (`checked_slot` returns `None`);
     the slow path (`classify_free`, then `check_located`) repeats the
     same `Relaxed` reads and aborts (`never_committed`, the double-free
     check) or releases the slot to arena 0 only if its re-read is stale
     too, which the same reasoning allows but the model does not run.
     Documentation of WEAKMEM.md P5's open question, not a property.
   - Controls: with the `Relaxed` hand-off a free that touches the block
     races with the allocating thread
     (`loom_free_relaxed_handoff_is_a_race`); with the claimer's
     watermark load `Relaxed`, a free after a synchronising hand-off can
     read the owner as 0 (`loom_free_unpublished_owner_is_misrouted`;
     38029).

## Translation

- **`requires perm.addr() == word_addr(w)`**: the permission is for this
  word; part 1 is about that word alone.
- **`requires !(ordering is Release)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `load` panics; with them excluded the
  body is one atomic load and returns (`no_unwind`: no other panic
  site).
- **`ensures r == perm.value()`**: read `perm.value()`, as in the other
  wrappers' records (`vspec-word-fetch_or.md`), as the value of the last
  write to the word in the per-location order of part 1 (every write of
  a class bitmap word goes through the wrappers: `run.sh` checks by name
  that non-test `bitmap.rs` writes atomics only through them and that
  `class/table.rs`, which casts word addresses to `&AtomicU64`, writes
  none; see `vspec-word-WordPerm.md`). Part 1 places this load in that
  order right after the write it reads, so `r` is the permission's value
  there, for every ordering. The order is per word: Verus's one
  interleaving of all words and slot tokens is not a C11 fact, and what
  the SC proof carries across locations is the subject of the next
  bullet.
- **What the proof takes from `r`, across locations.** Only that a set
  bit is not in a guard slab. A guard slab's bits are clear in every
  value any write leaves (the word's invariant, kept by every writer),
  so the conclusion holds whichever write the load reads; it does not
  need the load to see any particular write, and so needs no ordering.
- **What the free needs beyond the contract** (WEAKMEM.md P5): that a
  live block's bit reads set, so the double-free check does not fire
  falsely. That is not in the contract (Verus gives the bit no meaning
  for the caller beyond the guard fact) but it is what the `Relaxed`
  load relies on: the claim's `fetch_or` happens before the free when
  the program hands the block over with synchronisation. C asks that of
  a correct program: C11 7.22.3p2 lets `malloc` and `free` "visibly
  modify the storage that they allocate or deallocate", which we read as
  making a free not ordered after its allocation a data race on the
  block. Then write-read coherence makes the load read the claim or a
  later write; only a release of the same slot, which a correct program
  does not race with its own free, can clear the bit again. The
  synchronised model checks this; the `Relaxed` model shows the bit read
  clear outside that premise.
- **Gaps.** One word, 2 slots, 4 threads, one slab and one group. Not
  modelled: `multi_arena`, sub-word slabs, the cache push after the
  checks, and the class lock and sets that carry a slab from
  `take_group` to a claimer (the claimer's `Acquire` load of the
  watermark stands for that chain). The table steps are copies (pinned,
  `verus/loom/pins.txt`).

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
`class/table.rs` writes atomics only on two listed lines and through
`vspec/atom.rs`; `arena.rs` touches the bound counts and the arena
count, and `policy.rs` the position, only through `vspec/count.rs`;
`STATE` has two writes); then it runs every
loom model and control. A body that breaks the property fails a model
where the model runs the real code (`lock.rs`, `bitmap.rs`,
`vspec/word.rs`, `vspec/count.rs`); a changed copied line fails the pin
check; a changed contract fails the ledger check, until this record and
its models are revisited. The script is run by hand (about 15 minutes
with the build); no CI job runs it yet.
