# `SlabDesc::init`

Seam S199 of `verus/SEAMS.md`: src/slab.rs:303. Kind: external_body. Category: atomics primitive.
Discharged by the C11 coherence rules and loom, given the locks'
happens-before edge that `lock::axiom_acquired` assumes (part 3).

## Contract

At commit 82ca40d7:

```rust
pub fn init(&self, arena: u16, Tracked(p): Tracked<&mut DescPerm>)
    requires
        old(p).id() == desc_id(self),
    ensures
        final(p).id() == old(p).id(),
        final(p).state() == state_byte(State::Fresh),
        final(p).dirty() == 0,
        final(p).arena() == arena,
```

The body, at that commit: five `Relaxed` stores, `state` = `Fresh`
(0), `shadow` = `Fresh + 1`, `arena` = `arena`, `live` = 0 and
`dirty_at` = 0. Its one call site is `SlabTable::take_group`
(`class/table.rs`), under the class table's commit lock with the
taking arena's class lock held, for each non-guard slab of the group
it hands out.

## Statement

Three facts, the third checked by loom.

1. **C11, per location.** C++20, which Rust adopts, orders the writes
   to one atomic object in one modification order consistent with
   happens-before, and its coherence rules ([intro.races] p15-p18) hold
   for every memory ordering, `Relaxed` included: a read that a write
   to the same object happens before reads that write or one later in
   the modification order (write-read coherence), and a read never
   reads a write that it happens before, nor one earlier in the
   modification order than a write it happens before it (read-write and
   read-read coherence).
2. **The locks.** `RawLock` (`lock.rs`) gives a happens-before edge from
   each release to the next acquire that takes the lock, so everything
   one holder did happens before everything the next holder does. The
   Verus proofs already assume this edge for every lock value
   (`lock::axiom_acquired`, S177, trusted: a named residual assumption
   of this record too), and the loom models of the real `RawLock`
   check it (`loom_lock_two_threads`, `loom_lock_three_threads`, below;
   `lock-LockGuard-drop.md` for the release, and the superseded
   `lock-LockGuard-deref.md`, which keeps the statement).
3. **loom.** `src/loom_tests/slab.rs` (see the common part below) runs
   a copy of the six accessors, line for line (pinned,
   `verus/loom/pins.txt`; `slab` is not built under `cfg(loom)`), over
   loom's atomics, under two real `RawLock`s standing for a class
   table's commit lock and an arena's class lock, with the permission
   made concrete: its three values in a loom `UnsafeCell` under the lock
   whose value holds it. At 82ca40d7 the `cfg(loom)` build did not
   compile (`vspec/count.rs`'s `new` is a `const fn` calling loom's
   `AtomicU32::new`, which is not `const`); `new` is now left out of
   that build (no model calls it; the models make their counters
   directly), and `run.sh` builds and runs every model unchanged.
   The two slab models are exhaustive:

Models: `loom_slab_desc_reads_through_the_lock`, `loom_slab_desc_unlocked_read_is_stale`, `loom_lock_two_threads`, `loom_lock_three_threads`

   - `loom_slab_desc_reads_through_the_lock` (36 executions): one thread
     does what `SlabTable::take_group` and its caller do (under the
     class lock: take the commit lock, move the permission out of the
     table's value, `init`, release the commit lock, `set_state(Active)`,
     keep the permission in the class lock's value); another, under the
     class lock, reads the three fields through the permission when the
     value has it (`state_byte_held`, `dirty_held`, `arena_held`), then
     `set_state(Empty)` and `set_dirty_at`; after both are joined the
     three are read again. Every read returns the value the permission
     records, and no permission cell is touched by two threads without
     a happens-before edge between them.
   - Control: `loom_slab_desc_unlocked_read_is_stale` reads the state
     outside the class lock, told of the hand-over only by a `Relaxed`
     flag, and reads the value from before `set_state` (the model fails
     the read's assertion, as the control expects): the reads rely on
     the locks' edges, not on the descriptor's orderings.
   - `loom_lock_two_threads` (exhaustive) and `loom_lock_three_threads`
     (at most 5 preemptions, as in the lock records): the lock's mutual
     exclusion and edge (fact 2).

## Translation

- **Reading the permission.** `DescPerm` (`slab.rs`) is plain ghost
  data: not `Copy`, fields private to `slab.rs`, and built only by
  `class::table::axiom_init_descs` (S88, a separate seam, tool none),
  once per descriptor of a class table, recording the zero the
  reservation's bytes hold. Read `p.state()`, `p.dirty()` and
  `p.arena()` as the values of the last writes, in the modification
  orders of the descriptor's `state`, `dirty_at` and `arena`, made
  through `p`, or the minted zeros before any; `desc_id(self)` is the
  descriptor's address (`axiom_init_descs` ties the two), so
  `p.id() == desc_id(self)` (or `perm_for(m, k, self)`) says `p` is
  this descriptor's.
- **Every write is through a permission.** The three fields are
  private to `slab.rs`, and `verus/loom/run.sh` checks non-test
  `slab.rs` line by line: every occurrence of `.state`, `.shadow`,
  `.arena` or `.dirty_at` on any receiver is a `Relaxed` load, a
  `DescPerm` spec body (`self.state`, `self.arena`), or one of the
  eight write lines of `init`, `set_state`, `set_dirty_at` and the
  `#[cfg(test)]` `set_arena`, each in its function and each exactly
  once; and no line starts a split `.store(` (or another write) chain,
  takes a field's address, names `SlabDesc { .. }` in a pattern or
  literal, or writes through `as *mut SlabDesc`, `write_bytes`,
  `write_volatile`, `addr_of` or `transmute`. `init`, `set_state` and
  `set_dirty_at` each require `old(p).id() == desc_id(self)`, so in the
  shipped library every write of the three fields is made through this
  descriptor's permission; that no code elsewhere writes the
  descriptor's bytes through a pointer is what `PointsTo<SlabDesc>`,
  held by the class table, gives the verified code, and review for the
  rest.
- **Each access is ordered after the previous one.** An accessor is
  called by a thread holding the permission (`&mut DescPerm` for a
  write, a `&Map` containing it for a read), which Verus checks. The
  permissions live in two lock values (`class::table::TableInner::dperms`,
  the class table's commit lock, until `SlabTable::take_group` moves a
  group's out, with that lock and the taking arena's class lock both
  held, into `class::Inner::dperms`, the class lock's value, where they
  stay), and the code reaches a lock's value only through a guard or a
  `LockHeld` token (`lock.rs`). So every access through a permission is
  made while holding the lock whose value holds it, and by fact 2 the
  successive holders of a permission are ordered by happens-before
  (the move between the two locks is made holding both). A `fork` child
  runs on the forking thread, which `fork_prepare` makes hold every
  class lock, so the child's accesses come after the parent's in
  program order or through those locks. With fact 1, a read through
  `p` returns the last write through it: a write before the read in
  happens-before is no later than the one it reads, and a write after
  it is not read. The first holder's reads see the minted zeros:
  the reservation is zero from the kernel, and the table is published
  to other threads (through `Shared`'s publication) after it is built.
- **`init`.** `requires old(p).id() == desc_id(self)`: the permission
  is this descriptor's. The body is five stores, each of which returns,
  and no panic site (`State::Fresh as u8 + 1` is 1). Its stores of
  `state`, `arena` and `dirty_at` are the last writes through `p` when
  it returns, so `final(p).state() == state_byte(State::Fresh)` (0),
  `final(p).dirty() == 0` and `final(p).arena() == arena`;
  `final(p).id() == old(p).id()` is ghost.
- **Not claimed.** `live` (an RMW count, `slab-SlabDesc-sub_live.md`,
  whose `init` store comes before the slab's slots are handed out) and
  `shadow` (no permission records it) are outside the permission. The
  proofs are sequentially consistent; this record lifts only the
  per-location reading above, for these three fields, and relies on
  the lock records for the edges across them.
- **Gaps.** The loom model is a copy (pinned), of one descriptor, two
  threads and two locks; the argument above does not depend on those
  numbers. The hand-over between the two locks runs on one thread: what
  crosses threads in the model is the class lock's edge to the next
  holder, and that `take_group` makes the move holding both locks is read
  from the source; no modelled thread reads the minted zeros. The access
  check matches source text; a write through a name or pointer it does
  not look for is caught only by review. The zero contents at minting
  are the minting seam's and the commit path's (`Unbacked::into_lazy`),
  not this record's.

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

Written on 2026-10-05; reviewed then by a fresh-context subagent (a
Claude subagent with no prior context of this work), told to look for
overclaiming translations and holes in the checks.

## Tests

`sh verus/loom/run.sh` (`docs/testing.md`, "loom model checks") first
checks that `verus/SEAMS.md` is current, that the contract above is
still the ledger's entry for this seam, that the models the `Models:`
line names exist and are not ignored, that the source lines the copy
mirrors are in place (`verus/loom/pins.txt`: the six accessors' atomic
accesses, in order), and the access check on `slab.rs` named above;
then it runs every loom model and control. A changed accessor line
fails the pin check, a new write of the three fields outside the
accessors fails the access check, and a changed contract fails the
ledger check, until this record and its model are revisited. It is run
by hand (about 15 minutes with the build); no CI job runs it yet.
