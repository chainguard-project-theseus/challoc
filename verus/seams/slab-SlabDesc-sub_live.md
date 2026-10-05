# `SlabDesc::sub_live`

Seam S205 of `verus/SEAMS.md`: src/slab.rs:264. Kind: external_body. Category: atomics primitive.
Discharged by reading the body (part 3); the loom models are context only,
for the allocator invariant the contract no longer claims.

## Contract

At commit 0c9cd77:

```rust
pub fn sub_live(&self, n: usize) -> usize
    requires
        crate::sys::may_abort(),
```

The body, at that commit: `let prev = self.live.fetch_sub(n as u32,
AcqRel) as usize; assert!(prev >= n, ...); prev`. The contract claims no
value, and `requires crate::sys::may_abort()` lets the call abort: the
assertion is a `panic!`, which the `panic = "abort"` profiles turn into
an abort. (At 8635213, with no `requires`, the contract said the
assertion never fires; that was evidence only.)

## Statement

loom (see the common part below), the models of `src/loom_tests/bitmap.rs`
(described in `verus/seams/vspec-word-fetch_or.md`), whose release step
is a copy of `sub_live` (`fetch_sub(AcqRel)` and the `prev >= n`
assertion, "live underflow") after the real `AtomicBitmap::release`,
with the reservation (`add_live`, `AcqRel`, under the real `RawLock`)
before every claim:

Models: `loom_bitmap_release_then_reserve`, `loom_bitmap_two_claimers`

In every execution (exhaustive) the assertion holds and `live` ends at
the number of slots held; the `Acquire` load of `live` under the lock
(`take`) sees the bits that the releases it counts cleared.

## Translation

- **`requires crate::sys::may_abort()`**: the caller may abort, so the
  assertion firing is within the contract (the panic reaches the
  panic handler, which aborts through `abort_fmt`, the path
  `may_abort` names). No `ensures`: nothing is claimed of the value
  returned.
- **What is left to show**: that the call returns or aborts, with no
  undefined behaviour. The body is one `fetch_sub` (a single atomic
  read-modify-write, wait-free for every ordering), a comparison and
  the assertion: no loop, no pointer, no other panic site (`n as u32`
  truncates and `as usize` widens; neither panics). The models run a
  copy of it in every explored execution and every one returns.
- **Not claimed, and not needed by the contract**: that the assertion
  is unreachable. That is an invariant of the whole allocator: every
  slot a release returns was reserved in `live` by the claim that
  handed it out, and is released once (the free path's double-free
  check). Its memory-model half holds generally: after
  `SlabDesc::init`'s `store(0)`, which comes before the slab is
  published, `live`'s writes are all RMWs, in one modification order;
  a slot's reservation happens before its claim, which happens before
  its release (on the claiming thread, or through the program's hand-off
  of the pointer to the freeing thread, which must synchronise:
  `verus/seams/class-table-load_owner.md`), so the `add_live` precedes
  the `fetch_sub` in that order and the running value never drops below
  the slots still held. The models check this for 2 slots and 3
  threads, on a copy (pinned, `verus/loom/pins.txt`).
- **Gap.** `n as u32` truncates, so an `n` above `u32::MAX` subtracts
  the wrong amount and the assertion compares the untruncated `n`;
  every caller passes a slot count of one slab (at most `nslots`, far
  below 2^32), which the contract does not state, and which changes
  only the value returned (not claimed) or whether it aborts (allowed).

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

Fresh-context subagent, 2026-10-03: the models against the code they
run or copy, and this translation, looking for claims beyond what the
models check; its findings are folded in. Again 2026-10-04, after the
contract gained `requires may_abort()` and the record became a
discharge: it found the loom models add nothing the contract needs,
which the header now says.

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
