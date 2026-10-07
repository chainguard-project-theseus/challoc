# `load` (`vspec/flags.rs`)

Seam S311 of `verus/SEAMS.md`: src/vspec/flags.rs:77. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 3).

## Contract

At the commit that adds it (package FFLAGS):

```rust
pub fn load(b: &RouteByte, ordering: Ordering) -> (r: u8)
    requires
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r & !ROUTE == fixed(b),
    opens_invariants none
    no_unwind
```

The body: `b.b.load(ordering)`, the `core` load of the `AtomicU8` that
`RouteByte` wraps (`#[repr(transparent)]`, its field private to
`vspec/flags.rs`). Its one call site is `Shared::free_flags`
(`Relaxed`), the `free` fast path's dispatch byte, which requires
`Shared::wf`; `wf` says the byte's `fixed` bits are
`push_flags & !FREE_ROUTE` (`FREE_ROUTE` is `ROUTE`, 8, asserted at
compile time in `heap/shared.rs`). Before this package `free_flags`
itself was the trusted seam (`external_body` over a bare `AtomicU8`,
its `ensures r & !FREE_ROUTE == self.flags() & !FREE_ROUTE` a writers'
invariant stated in its comment); it is now verified over this wrapper.

## Statement

The byte's protocol: `init` makes the byte with `flags::new(push_flags)`
and writes it into the unpublished `Shared` (`put_flags`, a plain
`ptr::write` beside the `push_flags` write); the only later write is
`set_multi_arena`'s `flags::fetch_or(FREE_ROUTE, Relaxed)` (called by
`Arenas::build` for every arena after the first); the only read is this
`load`. That nothing else touches the byte is a type guarantee: the
`AtomicU8` inside `RouteByte` is private to `vspec/flags.rs`, so rustc
rejects any other atomic operation on it, and `new` is the only
constructor (no `Clone`, `Copy` or `Default`). What rustc does not rule
out, a raw write over the field (`ptr::write` of other bytes, a
transmute), `run.sh` checks by name (below). `fixed(b)` names the
byte's bits other than `ROUTE`, fixed by `new` (`vspec-flags-new.md`).
Two parts.

1. **C11, per byte.** C++20 (which Rust adopts) orders all writes to
   an atomic object in one modification order, and a load reads a write
   of that order no older than the last write that happens before it
   ([intro.races] p15-p18); a read-modify-write reads the write just
   before its own in that order ([atomics.order] p10). The object's
   initialisation is the `ptr::write` of the byte `new` made, the first
   write in the order for every reader that happens after it (a reader
   that does not is in a data race with it, undefined behaviour). By
   induction over the order, every write keeps the bits outside `ROUTE`:
   the initial value has them as `v & !ROUTE` (`new`'s `ensures`), and
   `fetch_or(v')` with `v' & !ROUTE == 0` (its `requires`, which Verus
   checks at the one call site) writes `prev | v'`, whose bits outside
   `ROUTE` are `prev`'s. So every value a load can read has
   `r & !ROUTE == fixed(b)`, for every ordering.
2. **loom.** `src/loom_tests/flags.rs` (see the common part below) runs
   the real wrappers of `vspec/flags.rs` (`new`, `fetch_or`, `load`,
   compiled over loom's `AtomicU8`) with the orderings
   `heap/shared.rs` passes, driven by copies of the steps around them:
   `init` (`push_flags` and the byte `new` made written into a loom
   `UnsafeCell`, then `init_slow`'s `READY` store, `Release`), builders
   (`Arenas::build`'s `set_multi_arena`, which runs with the published
   `Shared`, after an `Acquire` load of `STATE` that saw `READY`;
   then `fetch_or(ROUTE, Relaxed)`) and the main thread as the `free`
   fast path (`api::shared_if_ready`'s `Acquire` load, standing in for
   the edge `api::shared_deref`'s heap witness carries, then two
   `load(Relaxed)`). `push_flags` is `0b111`, every bit `init` may set.
   Exhaustive (execution counts as printed by `src/loom_tests/mod.rs`'s
   `check` under `--nocapture`):

Models: `loom_flags_route_byte_keeps_push_flags`, `loom_flags_two_builders`, `loom_flags_relaxed_publish_is_a_race`, `loom_flags_bit_outside_route_is_caught`

   - `loom_flags_route_byte_keeps_push_flags` (one builder; 478
     executions) and `loom_flags_two_builders` (two; 613414): every
     value a load or a `fetch_or` returns is `push_flags` with or
     without `ROUTE`, read with no race on the storage `init` wrote; a
     thread's second load keeps `ROUTE` once its first saw it; and once
     every thread is joined the byte is `push_flags`, plus `ROUTE` if a
     builder ran.
   - Controls: with `READY` stored `Relaxed`
     (`loom_flags_relaxed_publish_is_a_race`), a reader that sees
     `READY` reads the storage `init` wrote unordered, which loom
     reports (a causality violation): the `Release`/`Acquire` pair is
     what puts every reader after the initialisation. With a builder
     that sets a bit outside `ROUTE` (`loom_flags_bit_outside_route_is_caught`,
     what `fetch_or`'s `requires` rules out), the assertions fail: they
     have teeth.

## Translation

- **`requires !(ordering is Release)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `load` panics; with them excluded
  the body is one atomic load and returns (`no_unwind`). The call site
  passes `Relaxed`.
- **`ensures r & !ROUTE == fixed(b)`**: part 1, for a reader that
  happens after `init`'s write of the byte. That premise is not in the
  contract, which is about any `RouteByte`; the only `RouteByte` is
  `Shared`'s (one `new` call, checked by name), and every `&Shared` the
  verified code holds is `init`'s own (before it is published) or the
  published one, reached after an `Acquire` load of `STATE` that saw
  `READY` (`api::shared`, `api::shared_if_ready`; the `free` fast
  path's `api::shared_deref` is reached through a thread's heap,
  created after such a load), as `Shared::wf`'s comment and the seams
  that return a well-formed `Shared` (`api::shared_deref`,
  `mallctl::shared`, `heap_at`, `class_entry`, `shared_arg`, ...)
  already assume. The model checks that the `Release`/`Acquire` pair
  gives that order, and the control that it is needed.
- **`fixed(b)`** is a spec function of the `RouteByte` value, which no
  operation through `&RouteByte` changes and a move keeps; `new` ties it
  to the real bits (`vspec-flags-new.md`) and part 1 keeps it, given
  that every write is `new`'s or `fetch_or`'s, which the type and the
  `run.sh` check give.
- **`opens_invariants none`**: the body opens no Verus invariant (read
  from the source).
- **Across locations.** Nothing: the `ensures` is about this byte and
  ghost constants (`push_flags` is a plain field `init` writes before
  the byte and nothing writes after, so the reader sees `init`'s value
  under the same premise).
- **Gaps.** One or two builders and one reader; the copies of `init`'s
  two writes, the publication and `build`'s call are pinned
  (`verus/loom/pins.txt`), the rest of `init` and `build` is left out;
  part 1's induction is argued here and checked by the model for these
  instances, not proved in a weak-memory logic. The `run.sh` check is
  heuristic, matching source text: it allows exactly one call of each
  wrapper (the pinned lines of `heap/shared.rs`) and one raw write
  through `S_FREE_FLAGS` (of `new`'s byte), and rejects lines naming
  `free_flags` or `RouteByte` with a raw-pointer, transmute, zeroing,
  `mem::swap`/`replace`/`take` or `ptr::write`/`replace`/`swap`/`read`
  token, assignments and `&mut` borrows of a `free_flags` field, wrapper
  calls by a shorter path and imports from `vspec/flags.rs` other than
  `RouteByte` (the list is in `run.sh`). These matter only for
  unverified or test code (verified code that moves or replaces the
  value keeps Verus's view of it in step), and a write of `Shared`'s
  bytes through a pointer or name the patterns do not see (a
  whole-`Shared` write, a byte offset computed otherwise, a reborrow
  through a local) is caught only by review.

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
  `position.rs`, the `Shared` steps of `flags.rs`) cites the code it
  mirrors; `verus/loom/pins.txt` lists the source lines it copies,
  which `run.sh` requires in place and in order. A change elsewhere in
  the copied functions is caught by review only.

The Verus proofs assume sequential consistency. A loom record checks
that the orderings the code uses give the happens-before edges and the
progress the SC proof or contract relies on, in the modelled
configuration, and argues why that configuration is enough; it does not
lift a proof to weak memory.

## Review

Fresh-context subagent, 2026-10-07 (package FFLAGS): a strict soundness
pass on the three wrappers, `fixed`, the strengthened
`axiom_shared_built` and `Shared::wf`, the model and these records.
Its findings are folded in: the wrappers first took a bare `&AtomicU8`,
so verified code could have stored into a byte `new` made (vstd's
`store` has no `ensures`) and then `load` it with the stale `fixed`
bits, or `load` any other `AtomicU8`; the byte is now `RouteByte`, its
`AtomicU8` private to `vspec/flags.rs` (the machine code is unchanged).
The `run.sh` check, which missed several write forms, now counts the
wrapper calls and raw writes exactly and says it is heuristic; a
control shows the model's assertions fail when a builder sets a bit
outside `ROUTE`; `FREE_ROUTE == ROUTE` is a compile-time assertion; the
claim that `fixed` is protected like `count_id` was dropped; and the
execution counts say how they were measured.

## Tests

`sh verus/loom/run.sh` (`docs/testing.md`, "loom model checks") first
checks that `verus/SEAMS.md` is current, that the contract above is
still the ledger's entry for this seam, that the models the `Models:`
line names exist and are not ignored, that the source lines the copies
mirror are in place (`verus/loom/pins.txt`), and, by name, the atomic
accesses the records rely on (for this one, the flag-byte check above);
then it runs every loom model and control. A body that breaks the
property fails a model (the models run the real `vspec/flags.rs`); a
changed copied line fails the pin check; a changed contract fails the
ledger check, until this record and its models are revisited.
