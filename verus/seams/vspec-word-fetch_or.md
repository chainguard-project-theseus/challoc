# `fetch_or` (`vspec/word.rs`)

Seam S365 of `verus/SEAMS.md`: src/vspec/word.rs:61. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 3).

## Contract

At commit 8635213:

```rust
pub fn fetch_or(w: &AtomicU64, v: u64, ordering: Ordering, Tracked(perm): Tracked<&mut WordPerm>) -> (prev: u64)
    requires
        old(perm).addr() == word_addr(w),
    ensures
        final(perm).addr() == old(perm).addr(),
        prev == old(perm).value(),
        final(perm).value() == prev | v,
    opens_invariants none
    no_unwind
```

The body, at that commit: `w.fetch_or(v, ordering)`. Its call sites are
`AtomicBitmap::claim_word` (`Acquire`) and `AtomicBitmap::init`'s
sub-word branch (`Relaxed`, before publication).

## Statement

Two parts.

1. **C11, per word.** For any one atomic object, C++20 (which Rust
   adopts) orders all its writes in one modification order consistent
   with happens-before ([intro.races] p4, p15-p18, the coherence
   rules), and "atomic read-modify-write operations shall always read
   the last value (in the modification order) written before the write
   associated with the read-modify-write operation" ([atomics.order]
   p10), for every memory ordering.
2. **loom.** The models of `src/loom_tests/bitmap.rs` (see the common
   part below) run the real `AtomicBitmap` code (`bitmap.rs`: `init`,
   `claim_from` through `claim_word`, `release`), which calls these
   wrappers (`vspec/word.rs`, compiled over loom's `AtomicU64`), on one
   word for a slab of 2 slots, with copies of the class steps around it
   (`live` and its reservation under the real `RawLock`, the claim loop,
   `release` then `sub_live`) and each slot's data in a loom
   `UnsafeCell` written by every holder. Exhaustive:

Models: `loom_bitmap_release_then_reserve`, `loom_bitmap_two_claimers`, `loom_bitmap_init_is_published`, `loom_bitmap_relaxed_release_is_a_race`, `loom_bitmap_relaxed_claim_is_a_race`, `loom_bitmap_unclaimed_write_is_a_race`

   - `loom_bitmap_release_then_reserve` (two owners each release a slot
     while a third thread reserves one and claims it; 2904 executions)
     and `loom_bitmap_two_claimers` (one owner releases both slots to
     two claimers; 2461): no race on slot data between a release and
     the next holder, disjoint claimed masks, no padding claimed, no
     release of a clear bit, and the reservation met within one pass per
     competing claimer.
   - `loom_bitmap_init_is_published` (`init`'s `Relaxed` writes, then a
     `Release` store of the commit count that a claimer loads
     `Acquire`, as `class/table.rs`'s `committed_slabs`; 6): the claimer
     never claims padding.
   - Controls: with the release's `fetch_and` `Relaxed`
     (`loom_bitmap_relaxed_release_is_a_race`) or the claim's `fetch_or`
     `Relaxed` (`loom_bitmap_relaxed_claim_is_a_race`, a copy of the
     claim's step), the first model reports a race on slot data; a
     write to a slot without claiming it races
     (`loom_bitmap_unclaimed_write_is_a_race`). In
     `loom_bitmap_release_then_reserve` the claim can take the slot of
     the owner whose `sub_live` the reservation has not seen, so only
     the bitmap's orderings order the two owners' writes; in
     `loom_bitmap_two_claimers` the reservation's `Acquire` load of
     `live` already orders them, so that model does not exercise the
     bitmap's orderings.

## Translation

- **`requires old(perm).addr() == word_addr(w)`**: the permission is
  for this word; the per-word statement is about that word alone.
- **The `ensures`.** Read `perm.value()` as the value of the last write
  to the word in its modification order among those performed so far,
  and order the wrappers' operations on the word by that order (every
  write to a class bitmap word goes through these wrappers,
  `vspec/word.rs`'s module comment; `AtomicBitmap`'s other loads are
  plain `core` loads whose result Verus takes as any value). Then part 1
  gives this wrapper's `ensures` for every ordering, as the next bullet
  says per wrapper. This is sequential consistency per location, which
  C11 has; it is not a global order across words.
- **What the SC proofs carry across locations.** Verus reasons about all
  words, their ghost permissions and the slot tokens in one interleaving.
  The tokens are ghost, but they stand for the slots' memory: a token
  says its holder may write the slot. That needs happens-before from the
  previous holder's accesses to the next holder's, and from `init`'s
  padding to a claimer's read of it, which C11 gives only through the
  orderings: a release's `fetch_and(Release)` heads a release sequence
  that every later write to the word continues, since every write to a
  word after the first slab on it is published is an RMW (claims,
  releases, and a later slab's `init` on a word it shares, whose
  `fetch_and` and `fetch_or` are RMWs; `init`'s plain `store` is only
  for a word its slab owns whole, before that slab is published). So
  the next claim's `fetch_or(Acquire)` reading any of them synchronises
  with the release. Part 2 checks these edges, disjointness and the
  padding in the modelled configuration.
- **Gaps.** One word, 2 slots, at most 3 threads. Not modelled: a
  slab's sub-word view (`init`'s `fetch_and` and `fetch_or` branch,
  `mask`/`shift`), claims spanning several words, purge, the claim's
  random start word, and the free path's `Relaxed` reads (WEAKMEM.md P5,
  open). The argument above for the edge is per word and does not
  depend on the number of slots, words or threads, but the models check
  only this instance. The class steps are copies (pinned,
  `verus/loom/pins.txt`).
- **Per wrapper.** `fetch_or` is a read-modify-write: it reads the last
  value before its own write in modification order (`prev ==
  old(perm).value()`) and writes `prev | v` (`final(perm).value() ==
  prev | v`); `final(perm).addr() == old(perm).addr()` is ghost. The
  claim's `Acquire` is what the models check for the slot data.

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
  re-read a store it saw before when a newer one exists (loom's
  eventual visibility; C++ only says an implementation *should* make
  stores visible in finite time). No load buffering: a load never reads
  a store that comes later in loom's execution order. A `SeqCst` load
  does not read a `SeqCst` store when a newer `SeqCst` store to the
  same atomic has already run; C++20 allows that read when the load
  precedes the newer store in its total order `S`, and loom produces
  the same read in the schedule that runs the load first (the two are
  dependent, so DPOR explores both orders); otherwise `SeqCst` accesses
  synchronise as `AcqRel`, and `SeqCst` fences are stronger than
  C++20's (none of the models below uses one). That loom's `SeqCst`
  reads cover C++20's is argued here, not checked.
- Exploration: exhaustive DPOR over the modelled threads (at most 5),
  with no preemption bound unless the record names one (`run.sh` unsets
  the `LOOM_*` bounds). Stock loom's DPOR is incomplete: it never runs a
  store ahead of another thread's earlier load when the storing thread
  loaded the atomic last. The patch races a store or rmw against every
  thread's last load too, and changes nothing else, so it only adds
  backtracking points (the execution counts measured are all at least
  stock loom's; the initialisation models' grew from 1, 522 and 1472 to 10,
  29839 and 213383). `src/loom_tests/dpor.rs` fails without it. The
  patch is ours, reviewed but not proved; loom's own test suite was not
  run with it (its dev-dependencies are not in the offline registry).
- The futex is the stand-in `src/loom_tests/futex.rs`: one futex word
  per model, the compare under a loom mutex as the kernel's is under
  the hash-bucket lock, and a wake of one waiter. The mutex also
  orders a waker's earlier accesses before the woken thread's return.
  The code does not rely on that (after a wait each caller re-checks
  the word with its own acquiring access), but on a path through a
  wake the models cannot show it: an ordering missing from that
  re-check would be supplied by the stand-in. Linux's futex has such an
  edge in practice and documents no memory ordering.
- A copy (`init.rs`, `exit.rs`, the class steps of `bitmap.rs`) cites
  the code it mirrors; `verus/loom/pins.txt` lists the source lines it
  copies, which `run.sh` requires in place and in order. A change
  elsewhere in the copied functions is caught by review only.

The Verus proofs assume sequential consistency. A loom record checks
that the orderings the code uses give the happens-before edges and the
progress the SC proof or contract relies on, in the modelled
configuration, and argues why that configuration is enough; it does not
lift a proof to weak memory.

## Review

Fresh-context subagent, 2026-10-03: the models against the code they
run or copy, and this translation, looking for claims beyond what the
models check; its findings are folded in.

## Tests

`sh verus/loom/run.sh` (`docs/testing.md`, "loom model checks") first
checks that `verus/SEAMS.md` is current, that the contract above is
still the ledger's entry for this seam, that the models the `Models:`
line names exist and are not ignored, and that the source lines the
copies mirror are in place (`verus/loom/pins.txt`); then it runs every
loom model and control. A body that breaks the property fails a model
where the model runs the real code (`lock.rs`, `bitmap.rs`,
`vspec/word.rs`); a changed copied line fails the pin check; a changed
contract fails the ledger check, until this record and its models are
revisited. The script is run by hand (about 6 minutes with the build);
no CI job runs it yet.
