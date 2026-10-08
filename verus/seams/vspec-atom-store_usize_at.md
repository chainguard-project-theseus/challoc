# `store_usize_at` (`vspec/atom.rs`)

Seam S273 of `verus/SEAMS.md`: src/vspec/atom.rs:168. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 3), given `usize_ptr`'s identity (S274, trusted).

## Contract

At commit 4f75a5b7 (added by the port of upstream #87, the class line's
copy of the hand-out watermark):

```rust
pub unsafe fn store_usize_at(p: *const AtomicUsize, v: usize, ordering: Ordering, Tracked(perm): Tracked<&mut UsizePerm>)
    requires
        old(perm).id() == usize_id_at(p),
        !(ordering is Acquire),
        !(ordering is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
```

The body: `(*p).store(v, ordering)`, `#[inline(always)]`, under the
safety contract that `p` points to a live `AtomicUsize`, the one it was
made from, still in place. Its two call sites are in `class/table.rs`,
both under the table's commit lock and inside the copy's invariant
(`ClassLine::committed_inv`, a `CommittedPred` over the table's slab
count): `SlabTable::mirror_watermark` (`Release`) stores the
watermark's current value (a `Relaxed` load under the lock) into the
copy when `Shared::init` installs it, and `SlabTable::take_group`
(`Release`), when a copy is installed (`gi.mirror` non-null), stores
the end of the group it hands out, right after the watermark's own
store (`store_usize`, `vspec-atom-store_usize.md`). The pointer is
`usize_ptr(&line.committed)` (`heap/shared.rs`, `Shared::init`), into
the never-moving `Shared`'s class line; `usize_ptr`'s `ensures
usize_id_at(r) == usize_id(a)` (S274) is the identity half of this
package, which stays trusted (whether a Kani record of an address
identity counts as discharging it is an open question for the
maintainers, not settled in this repository).

## Statement

Two parts.

1. **C11, per atomic.** As in `vspec-atom-store_usize.md`: for the copy
   (one atomic object), C++20 orders all writes in one modification
   order consistent with happens-before, and a load reads a write of
   that order no older than the last write that happens before it
   (RC11's coherence). `core`'s `store` panics for `Acquire` and
   `AcqRel` and is otherwise one atomic store; through `p` it is that
   store on the atomic `p` points to.
2. **loom.** The model files run the real wrapper (`store_usize_at`,
   with `usize_ptr` making its pointer, compiled over loom's
   `AtomicUsize`) with the orderings `class/table.rs` passes:
   - `src/loom_tests/watermark.rs`, its copy model (since ATOMAT):
     copies of `take_group` (under the real `RawLock`, as in the
     watermark model of `vspec-atom-store_usize.md`) that, once the copy
     is installed (a flag under the lock, `gi.mirror`), store the end
     of the group into the copy after the watermark, and of
     `mirror_watermark` (under the lock: a `Relaxed` load of the
     watermark, `store_usize_at` of it, the flag set); the reads are
     `ClassLine::set_slab`'s `Relaxed` load of the copy
     (`load_usize_in`, as `class::load_mirror`). Each hand-out also
     loads the copy under the lock before its store and checks it is
     the group's start (the previous holder's store; a load the model
     adds). The lines copied are pinned in `verus/loom/pins.txt`.
   - `src/loom_tests/free.rs` (`vspec-word-load.md`): the hand-out's
     tail stores the copy through `store_usize_at` after the watermark,
     and the free path reads the copy first.

   Exhaustive:

Models: `loom_watermark_mirror_follows`, `loom_watermark_mirror_installed_concurrently`, `loom_watermark_unlocked_mirror_lags`, `loom_free_after_synchronised_handoff`

   - `loom_watermark_mirror_follows` (4482 executions): the copy
     installed, then two threads each hand out a group while the main
     thread reads the copy: under the lock each hand-out finds the copy
     at the group's start, the copy never exceeds the slab count, a
     reader's loads never go back, and once every thread is joined (and
     a third hand-out finds the region full) the copy is the slab count.
   - `loom_watermark_mirror_installed_concurrently` (143): the copy
     installed while another thread hands out a group (before or after
     it, under the lock), then the main thread hands out the second:
     the same checks.
   - `loom_free_after_synchronised_handoff` (31214): a free that
     receives the slot through a synchronising hand-off finds the slab
     handed out, through the copy or, when it lags, the slow path's
     re-read of the watermark (`vspec-atom-store_usize.md`).
   - Control: with the copy stored after the lock's release
     (`loom_watermark_unlocked_mirror_lags`), a hand-out can find the
     copy behind the watermark under the lock, so the check sees the
     copy's last write, not just any write.
   - Not run: one model with the copy installed while two threads hand
     out (three lock holders); loom did not finish it in
     20 minutes. The two models above split it.

## Translation

- **`requires old(perm).id() == usize_id_at(p)`**: the permission is for
  the atomic `p` names. With `usize_ptr`'s `ensures` (S274, trusted),
  that is the atomic `p` was made from, `line.committed`; the safety
  contract (the atomic still in place: `Shared` never moves) makes
  `*p` that atomic, so part 1 is about it alone. The identity and the
  liveness are premises here (S274 and `mirror_watermark`'s safety
  contract), not checked by the models, which make the pointer from a
  field of a leaked table.
- **`requires !(ordering is Acquire)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `store` panics; with them excluded
  the body is one atomic store and returns (`no_unwind`). Both call
  sites pass `Release`.
- **`ensures`.** Read `perm.value()` as the value of the last write to
  the copy in its modification order among those performed so far.
  Every write of the copy goes through this wrapper (`run.sh` checks
  that non-test `heap/shared.rs` writes no class line's `.committed`
  directly, that only `class/table.rs` calls the `vspec/atom.rs`
  wrappers and only `heap/shared.rs` makes such a pointer with
  `usize_ptr`), and the copy starts at 0 (`new_mirror`'s `new_usize(0)`,
  before `Shared` is published). A store is a write: after it the value
  is `v` (`final(perm).value() == v`) until the next write;
  `final(perm).id() == old(perm).id()` is ghost. Every write is under
  the commit lock, whose release/acquire edge (`lock-LockGuard-drop.md`)
  orders the stores in happens-before and so in modification order,
  which the copy model checks (the comparison under the lock, and the
  final value); the control shows a store outside the lock breaks it.
- **`opens_invariants none`**: the body opens no Verus invariant (read
  from the source).
- **What the SC proof carries across locations.** The copy's invariant
  names one atomic and a constant (the table's slab count); no
  invariant relates the copy to another location. The proofs use only
  that a load returns at most the slab count (`load_mirror`'s
  `ensures`), which part 1 gives: every write is in bounds (each is a
  watermark value, at most `nslabs`, which Verus proves at the write),
  and a load returns one of them. That the copy is never ahead of the
  watermark, and that a slab below it has its bitmap and owner word
  (`heap::ClassLine`'s doc comment), is the free path's protocol, not
  part of this contract; `free.rs` checks the free path's outcomes
  with it (a lagging copy only sends a free to the slow path).
- **Gaps.** 2 slabs of one slab per group, at most 3 threads and, in
  each model, at most two lock holders racing with the install; the
  table steps are copies (pinned); the commits, descriptors, owner
  word's previous value and THP advice are left out as in
  `vspec-atom-store_usize.md`. The argument that any ordering keeps an
  invariant over one atomic and constants is the one above, checked by
  the models for these instances, not proved in a weak-memory logic.

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

Written 2026-10-07 at commit 4f75a5b7 (ATOMAT, the store half),
following `store_usize`'s record: the contract above is the ledger's
entry there, and the models and this translation were compared with
each clause. A fresh-context subagent reviewed this record against the
models and the contract, looking for claims beyond what the models
check; its findings are folded in.

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
