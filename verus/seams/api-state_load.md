# `state_load`

Seam S22 of `verus/SEAMS.md`: src/api/mod.rs:389. Kind: external_body. Category: atomics primitive.
Discharged by the C11 coherence rules and loom (part 3).

## Contract

At commit 4f75a5b7 (`STATE`'s invariant; with `state_claim`, whose
record is `api-state_claim.md`, and `state_publish`, `api-state_publish.md`):

```rust
fn state_load(Tracked(perm): Tracked<&StatePerm>) -> (r: u32)
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
```

The body: `STATE.load(Ordering::Acquire)`, the bare `core` call under
`#[inline(always)]`. The seam is that `perm`, `STATE`'s ghost
permission (`api::StatePerm`, held by `STATE`'s invariant,
`api::axiom_state`), carries `STATE`'s value to the load. Its one caller
is `load_state`, inside the invariant, which hands the thread the
permissions frozen at `READY` (`v.published`) when the load returns
`READY`, and takes nothing otherwise; `load_state` is what `shared`,
`shared_if_ready` and `init_slow`'s wait call.

## Statement

Two parts.

1. **C11, per atomic.** For `STATE`, C++20 (which Rust adopts) orders
   all writes in one modification order consistent with happens-before
   ([intro.races] p4, p15-p18), and a load reads a write of that order
   no older than the last write that happens before it; equivalently
   (RC11's coherence axiom), the accesses to `STATE` can be put in one
   total order, consistent with happens-before, in which every read
   reads the last write before it. `STATE`'s writes are its initial
   value `UNINIT`, the successful `compare_exchange` of `state_claim`
   (`UNINIT` to `INITIALISING`) and `state_publish`'s `READY` store: two
   write sites in `src/`, which `run.sh` checks. Each value is written
   once, in that order (the exchange succeeds only on reading `UNINIT`,
   the last write before it by the rule for read-modify-writes, so once
   and before the `READY` store, which only its winner makes). `core`'s
   `load` panics for `Release` and `AcqRel` and is otherwise one atomic
   load.
2. **loom.** `src/loom_tests/init.rs` (described in `api-init_slow.md`)
   runs a copy of `shared`, `shared_if_ready` and `init_slow` whose
   loads of `STATE` are this `Acquire` load (pinned in
   `verus/loom/pins.txt`, with the exchange and the `READY` store), with
   two callers and a free, or with a re-entrant `init`. Every load goes
   through the copy's `Statics::state_load`, which checks that it reads
   one of the three values and never one before the thread's last
   value of `STATE` (read or written) in the order `UNINIT`,
   `INITIALISING`, `READY`; the copy's exchange is checked the same way.
   In every execution a caller that loads `READY` reads the value
   `init` built with no race, and with the `READY` store `Relaxed` the
   reader races (the control).

Models: `loom_init_free_races_init`, `loom_init_two_callers`, `loom_init_reentry_aborts`, `loom_init_publisher_reads_ready`, `loom_init_relaxed_publish_is_a_race`

   - `loom_init_free_races_init` (10 executions),
     `loom_init_two_callers` (29839), `loom_init_reentry_aborts`
     (213383) and `loom_init_publisher_reads_ready` (34144): every load
     of `STATE` reads `UNINIT`, `INITIALISING` or `READY`, never going
     back from the thread's last value; a load that reads `READY` is
     followed by a race-free read of the built value.
   - Control: `loom_init_relaxed_publish_is_a_race` (the `READY` store
     `Relaxed`) fails with a race on the published value, so the
     `Acquire` load's edge is what the models rely on.

## Translation

- **`ensures r == perm.value()`.** Verus's model is sequentially
  consistent: read `perm.value()`, as in the other wrappers' records
  (`api-state_claim.md`, `vspec-word-load.md`), as the value of the
  last write to `STATE` in the per-location order of part 1 (every
  write of `STATE` goes through the two wrappers, inside the
  invariant: `run.sh` checks that `STATE` has two writes). Part 1
  places this load in that order right after the write it reads, so
  `r` is the permission's value there, for every ordering. The order
  is per location: Verus's one interleaving of all locations is not a
  C11 fact, and what the SC proof carries across locations is the
  next bullet's.
- **What the proof takes from `r`, across locations.** Only, at `r ==
  READY`, the permissions the invariant holds at `READY`
  (`StatePred`: `READY ==> published.ok()`), which `load_state` hands
  to the thread; with them it reads `SHARED` and what else `init`
  wrote (plain memory). That needs `init`'s writes to happen before the
  reader's reads: `state_publish`'s `Release` store, read by this
  `Acquire` load, synchronises with it ([atomics.order] p2), and `init`
  is sequenced before the store. The models check exactly that read
  (no causality violation) and the control shows it needs the
  orderings. `READY` is the last value written, never overwritten (a
  later exchange reads `READY` and fails), so the permissions frozen
  there stay valid for every later reader.
- **A load that reads an older value than the last write.** Coherence
  lets a load return `UNINIT` or `INITIALISING` after a later write
  (part 1 then places it before that write, but Verus's interleaving
  of all locations may not). Why that cannot make the proof derive
  something false:
  - `READY` is written once and last, so a load that returns `READY`
    read the `Release` store itself: taking `published` never rests
    on a stale read.
  - At an older value the contract hands out no resource: the load
    leaves `perm` unchanged, and the invariant's other ghost state
    (the once-tokens `steps`, held at `UNINIT`) is linear, so whatever
    a proof took from it inside the opening it must put back before
    the opening closes (`load_state` takes nothing). Only the
    exchange (`state_claim`, a read-modify-write, which reads the last
    write) takes the tokens out.
  - No ghost resource gives a thread a lower bound on `STATE`'s value
    (nothing a thread holds says `STATE` is past `UNINIT`), so a stale
    value contradicts no fact the thread holds. The residual: if a
    once-token ever reached another thread without a happens-before
    edge from the exchange, that thread could hold a token while a
    stale load there returns `UNINIT`, which with `StatePred` would
    give two copies of the token. Today the tokens are taken by the
    exchange's winner and spent by `init` on that thread (through
    `Shared::init`); anything of theirs another thread can hold comes
    to it inside `published`, through a load that read `READY`, after
    which coherence keeps that thread's loads at `READY`.
- **`opens_invariants none`**: the body opens no Verus invariant.
  **`no_unwind`**: an `Acquire` load does not panic.
- **Fork.** As for `state_claim`: a child forked at any point starts
  from the parent's `STATE` with the parent's ghost world, per
  process; the published permissions frozen before the fork stay
  valid in the child (its memory is a copy).
- **Gaps.** The model is a copy (pinned); it has 2 callers and a free.
  The argument above does not depend on their number. That `STATE` has
  no other writer is checked by `run.sh` syntactically (writes through
  `STATE.` in `src/`); a write through a raw pointer to it would be
  caught only by review. The coherence checks witness what C11
  guarantees (no control can make them fail without breaking the
  memory model), so they confirm the model's reading, not the code.

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

Written 2026-10-07 at commit 4f75a5b7 (P1), following `state_claim`'s
record: the contract above is the ledger's entry there, and the models
and this translation were compared with each clause. A fresh-context
subagent reviewed this record against the models and the contract,
looking for claims beyond what the models check; its findings are
folded in.

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
