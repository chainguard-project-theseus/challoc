# Weak-memory checking

The Verus proofs assume sequential consistency (SC). vstd's `std`
atomic specifications return any value for a load and ignore the
ordering, and the bitmap word wrappers (`vspec/word.rs`: `load`,
`fetch_or`, `fetch_and`, `store`) are single atomic steps whose
specifications drop the `Ordering` argument. Verus therefore does not
check that the `Relaxed`, `Acquire` and `Release` orderings are strong
enough. This file names the protocols that depend on those orderings,
says what the loom models in `tests/loom` check about each, and states
what the checks leave out.

The loom models show that the shipped orderings give the happens-before
edges the SC proofs rely on, in small configurations. They do not lift
the proofs to weak memory: the Verus proofs remain SC proofs.

Marked **unconfirmed**: facts read from the code or tool documentation
and not run.

## Inventory

361 lines of shipped source carry an `Ordering::` (`grep` over `src/`,
leaving out `tests.rs`, `linux_tests.rs`, `src/kani/`, `src/model/`,
inline `#[cfg(test)]` and `#[cfg(kani)]` modules, and comment lines).
Files that pass an ordering through an imported name or a `vspec`
wrapper argument (`class/mod.rs`, for example) are not in this count.
Most of these lines are `Relaxed` statistics, configuration and
heuristics. `purge/background.rs` holds the crate's two `fence` calls
(in `wake` and `start`, P3), and there is no `compiler_fence`.
`sys/raw.rs`'s syscall `asm!` blocks have no `nomem`, so each syscall is
a compiler barrier.

| File | Lines | Atomics |
|---|---|---|
| `thp.rs` | 45 | init-time configuration and counters |
| `purge/background.rs` | 43 | `PRESENT`, `REQUESTED`, `SEQ`; the lazy start's `AUTO`, `AUTO_FAILS`, `AUTO_NEXT`, `AUTO_SINCE`, `AUTO_BASE`, all `Relaxed` (rate limits and heuristics; the request they set or withdraw is written under the state lock, as `request`'s) |
| `sys/cgroup.rs` | 40 | sampled cgroup limits and the deferred discovery's phase |
| `purge.rs` | 36 | flags, counters, `Decayer` |
| `arena.rs` | 30 | registry `AtomicPtr`s, counts, statistics |
| `api/mod.rs` | 28 | `STATE`, `INIT_TID`, `SEALED`, `LINE_STATE` |
| `stats.rs`, `policy.rs` | 26, 26 | statistics and the sampled policy, `Relaxed` |
| `slab.rs` | 20 | `SlabDesc`: state, arena, `live`, dirty |
| `class/table.rs` | 15 | watermark; bitmap and owner words; the watermark copy's stores (`vspec::atom::store_usize_at`) |
| `lock.rs` | 11 | the lock word |
| `heap/slotstate.rs` | 11 | `double_free_scan`'s per-slot state byte |
| `huge/mod.rs`, `bitmap.rs` | 9, 9 | `retain_max`, `purge_on_free`; `&[AtomicU64]` words |
| `heap/tcaches.rs` | 5 | `[AtomicPtr<u8>; 1024]` |
| `heap/shared.rs` | 4 | `free_flags`; the class lines' watermark copies, read through `class::load_mirror` |

## Protocols that matter

### P1. Slab bitmap claim and release

- Claim: `AtomicBitmap::claim_word` (`bitmap.rs`) reads the word
  `Relaxed` as a hint, then claims with `fetch_or(Acquire)`. Release:
  `AtomicBitmap::release`'s `fetch_and(Release)`, then
  `SlabDesc::sub_live` (`slab.rs`), a `fetch_sub(AcqRel)`.
- Edge: the releaser's `fetch_and` synchronises with the next
  claimer's `fetch_or`, so the previous owner's writes to the slot
  happen before the next owner's (the comment in `claim_word`).
  Intermediate claims on the word are read-modify-writes (RMWs), so the
  release sequence continues through them.
- Reservation: under the class lock, `ClassState::take` reads `live`
  `Acquire` and adds with `AcqRel`. Releasers clear bits before they
  subtract, so a reader of the lower `live` sees the cleared bits.
- Init: `AtomicBitmap::init` writes `Relaxed` under the commit lock,
  and `SlabTable::take_group` publishes the slab with the watermark's
  `Release` store (`committed_slabs`); the owner words likewise.
- Fresh slab commit: `SlabTable::commit_run` makes a run of the arena's
  own group read-write (one `mprotect`, and `install_guard` for a guard
  slab) without the commit lock, which it takes only for the guard
  count. Nothing is published through memory: the syscalls touch no
  atomic or lock value, the run is at or past the arena's cursor in a
  group no other arena owns, the caller holds the arena's class lock
  (which `fork_prepare` also takes), and a slab's memory reaches another
  thread only through the claim edges above. The kernel orders the
  protection changes (`mmap_lock`); that is outside the memory model, so
  no loom model covers it.
- Property: no slot is held by two threads; slot data is race-free
  across a release and the next claim; a claimer that reserved a slot
  (`live`) finds a clear bit. A stale `Relaxed` hint costs passes:
  `MAX_EMPTY_PASSES` (`1 << 22`, `class/mod.rs`) empty passes in a row
  reach the `reservation_unmet` abort (`class/abort.rs`), which a
  bounded checker reaches only with the constant lowered.

### P2. `Lock`

- `RawLock` (`lock.rs`) is a futex mutex over one `u32` (Drepper's
  "mutex, take 3"): `try_acquire`'s `compare_exchange(0, 1, Acquire,
  Relaxed)`; in `lock_slow`, a `Relaxed` spin of up to `SPINS` (100)
  rounds, then `swap(2, Acquire)` and `FUTEX_WAIT`; `release`'s
  `swap(0, Release)`, and `FUTEX_WAKE` when the old value was 2.
- Every lock value (class `Inner`, the commit lock's `TableInner`,
  sets, `meta::Inner`, `huge::Inner`, the arena registry, statistics,
  `Tcaches`, the background `STATE`) is plain data under the lock, with
  no atomics, so checking the lock covers them. `Huge` itself keeps
  `retain_max`, `purge_on_free` and a `Decayer` outside the lock; they
  are out of scope.
- Property: mutual exclusion, the acquire/release edge for the
  protected data, and no lost wake-up between the `swap(2)` and
  `FUTEX_WAIT`.

### P3. Background thread wake-up

- `SEQ` is the futex word: `kick` does `fetch_add(SeqCst)`, then
  `futex_wake`; `background::main` loads `SEQ` `SeqCst` before its pass
  and waits on that value. `PRESENT` and `REQUESTED` are `Relaxed`
  (also in `fork_child`).
- `wake` kicks only if `present()`. `note_empty` calls it on a class's
  first `Empty` slab, and `Decayer::note_first_empty` (`purge.rs`)
  reports only the first.
- Two windows can lose a wake-up. In the first, possible even under SC,
  the new thread's first pass reads a class's `oldest_empty` as 0 and
  computes no deadline; a free then empties the class's first slab, and
  `wake` sees `PRESENT` still false and does not kick; the thread sleeps
  with no timeout. From then on `on_tick` skips foreground decay because
  `present()` is true, and the class's later empties do not wake it
  because `oldest_empty` is set, so the class's `Empty` slabs stay
  resident until another wake. `start` closes this window: once it has
  stored `PRESENT`, it kicks the thread, which makes it scan again.
- The second window is store buffering, which weak memory allows: the
  free stores `oldest_empty` and loads `PRESENT`, `start` stores
  `PRESENT`, and the thread loads `oldest_empty`, all `Relaxed`, so both
  loads may read the old values. A `fence(SeqCst)` on each side closes
  it: one in `wake` before the `present()` load (after the caller's
  `oldest_empty` store), one in `start` between `PRESENT.store` and
  `kick()`. Under C++20 (P0668) the two fences are totally ordered. If
  `start`'s comes first, the free reads `PRESENT == true` and kicks. If
  the free's comes first, the thread reads the free's store, because
  `start`'s fence happens before the thread's load (through the kick
  and `SEQ`), and a load reading 0 would put `start`'s fence before the
  free's. loom's `SeqCst` fence is stronger than C++20's (it joins the
  fenced threads' views into one clock), so a passing model rests on
  that argument; the failing controls are behaviours C++20 allows.
- Both fences are off the fast paths: `wake` runs on a class's first
  `Empty` slab and on a decay-period change, `start` once per thread
  start. Making the four accesses `SeqCst` instead is also correct under
  C++20, but loom treats `SeqCst` accesses as `AcqRel` and reports a
  false deadlock (loom's README, "Unsupported features"), so only the
  fenced form is checked.
- Hardware: the class lock the free releases before `wake` and the
  state lock `start` stores `PRESENT` under change nothing in C++20,
  since a `Release` swap does not keep a later load behind an earlier
  store. On x86_64 the swap is an `xchg`, a full barrier, so the
  store-buffering window needs aarch64 (or a compiler that moves the
  accesses) to show on hardware; the first window needs neither.
- Verus calls `fence` through `vspec::misc`'s specification of it
  (panics on `Relaxed` only, no effect Verus models), so `wake` and
  `start` stay verified.

### P4. Publishing `Shared`

- `api::init_slow`: `compare_exchange(UNINIT, INITIALISING, Acquire,
  Acquire)` (`state_claim`), `init()`, then `STATE.store(READY,
  Release)` (`state_publish`). Readers load `Acquire` (`load_state`,
  `state_load`), and waiters spin on an `Acquire` load. The `READY`
  store publishes `SHARED`, `FEATURES` and `FEATURE_LINE` (`UnsafeCell`
  statics in `api/mod.rs`), the class table's page geometry
  (`layout::adopt_page_geometry`), and the init-time `Relaxed`
  configuration stores (thp, purge, background, `SEALED`).
- The fast paths skip the `READY` load (`api::shared_base`,
  `shared_deref`, `shared_ptr`): the thread's own TLS heap pointer is
  the witness, since heaps are created under `shared()`. This crosses
  no thread, because the TLS slot is per thread (`sys/tls.rs`).
- `STATE`'s invariant (`api::axiom_state`) states this premise for the
  proofs: a thread that loads `READY` gets the permissions `init` froze
  (`api::Published`), so the published value is the one `Shared::init`
  built (`api::shared_ref`). The proofs carry the fact as `api::ready()`
  from the load to the thread's heap and TLS slot token and read through
  the frozen permissions (`api::axiom_published`). The premise is that a
  `ready()` fact reaches a thread only after it loaded `READY` (or
  published it), or after a thread that did (the background thread,
  through `pthread_create`): the same happens-before. Besides
  `load_state`, the trusted `TlsSlot::entry`, `heap_at` and `shared_arg`
  assert it, and `mallctl::shared` requires it, from `mallctl::ctl`'s
  well-formed `&Shared`.
- Property: every `READY` reader sees the built value, including the
  page geometry, which is a plain write.

### P5. The free fast path's `Relaxed` reads

- The free path reads the `FREE_ROUTE` bit of the `free_flags` byte
  (`Shared::free_flags`, after the bit test), the class line's copy of
  the watermark (`heap::ClassLine`, through `class::load_mirror`; the
  slow path reads the table's watermark), the bitmap bit and the owner
  word `Relaxed`, with no acquire. The argument (comments in
  `class/table.rs` and `heap/shared.rs`) is that the program frees after
  it allocates, so happens-before runs from the allocation.
- A pointer handed between threads through a non-synchronising channel
  (a `Relaxed` atomic) breaks that chain. A stale byte without
  `FREE_ROUTE` skips the arena check, and the slot goes to the wrong
  arena's cache; a stale bit weakens only the double-free check.
- Property: with a synchronising hand-off, a free never aborts
  spuriously and never misroutes. With a `Relaxed` hand-off (a data race
  on the block, outside the C contract), the models record which
  outcomes appear.

### P6. Explicit caches and the arena registry

- `Tcaches::create` (`heap/tcaches.rs`) stores `Release` under the
  table lock; `get` loads `Acquire` without it; `destroy` swaps
  `AcqRel`, then frees. Concurrent use of one explicit cache, or use
  after `destroy`, is the caller's bug, as in jemalloc.
- Arena entries (`arena.rs`) are stored `Release` and loaded `Acquire`
  (through `vspec/entry.rs`); `Arenas::count` likewise (through
  `vspec/count.rs`).
- Thread heaps are touched only by their own thread; no code walks
  another thread's heap.

### Out of scope

Statistics and heuristics whose stale values change only reports or
timing are out of scope: class counters, `Arena::large`,
`Decayer::next_walk`, `policy.rs`, `sys/cgroup.rs`'s samples, and
`thp.rs` (written at init). `LAZY_EVER` (`purge.rs`) is global and
written under whichever class or huge lock flushes the batch, so
different locks do not order it. The read that matters is `take`'s, of
a slab whose last discard was flushed under that slab's own lock, so
that lock orders the one store it must see (P2).

## The tools

### loom

loom runs a test body under every interleaving it can reach and models
part of C11: stale `Relaxed` and `Acquire` loads within a bounded store
history (**unconfirmed**: no load buffering). Its `UnsafeCell` reports
data races, which is the check P1 needs for slot data.

challoc's statics are built in `const` context, which loom's
constructors cannot do, and some atomics are made from raw metadata
memory (`class/table.rs` casts addresses to `&AtomicU64` and
`&AtomicU16`). So the crate is not built under `cfg(loom)`, and every
model in `tests/loom` (its own crate) is a copy of the code it checks,
step by step, naming the function each step mirrors
(`tests/loom/src/lib.rs`). Two source tests keep the copies tied to the
code:

- `tests/loom/tests/pins.rs` checks that the source still has the
  copied lines, in order, and that every other line with an
  `Ordering::`, a `fence(` or a `futex_` call inside a function a pin
  falls in is pinned too or waived with its reason.
- `tests/loom/tests/access.rs` checks, by source text, the facts the
  ledger's weak-memory arguments state for the seams that cite the
  models: atomic writes only through the wrappers and accessors they
  name, and one permission minted per atomic. It matches
  rustfmt-formatted text, so a write through a pointer or a name it does
  not look for is caught only by review; each check has a control that
  adds one line it must reject.

loom has no futex. `tests/loom/src/futex.rs` stands in for
`sys::futex_wait` and `futex_wake` with one loom mutex and condition
variable for a model's one futex word (checked): the compare happens
under the mutex, as the kernel's happens under the hash-bucket lock, and
a wake reaches one waiter (`notify_one`, as `FUTEX_WAKE` with `n == 1`;
waking all would hide a lost wake-up). The stand-in's mutex also orders
a waker's earlier accesses before the woken thread's return. The code
re-checks the word after every wait with its own acquiring access and
does not need that edge, but on a path through a wake the models cannot
show that. Linux's futex has such an edge in practice and documents no
memory ordering.

`sh tests/loom/run.sh` runs the models under loom 0.7.2 with
`tests/loom/loom-0.7.2-dpor.patch`. Stock loom 0.7.2 races an atomic
store or RMW only against the atomic's last access; when that access is
the storing thread's own load, an earlier load by another thread is
never raced, so the store never runs ahead of it. The patch also races
a store or RMW against each thread's last load of the atomic and changes
nothing else, so it only adds backtracking points. The same defect is
fixed in ToyOSOrg/loom's `toyos` branch (ToyOSOrg/ToyOS PR #567).
`tests/loom/src/dpor.rs` holds two controls that fail without the patch.

Every model is exhaustive except `lock::loom_lock_three_threads` and
its control `lock::loom_lock_dropped_mark_loses_a_wake_up`, bounded at
5 preemptions. `LOOM_LOG=info` prints loom's execution count per model.
The `loom` check (`.chainguard/ci/lint.yaml`) runs `tests/loom/run.sh`
on every pull request. The `loom-count` check runs the slow count model
(`count::loom_count_bind_assign_unbind`, ignored in `loom`) on pull
requests that change `tests/loom/` or the code that model copies.

### GenMC

GenMC checks C and C++ programs under RC11 (and IMM) by exhaustive,
stateless exploration with no bound on store history, so it finds weak
behaviours that loom's bound can miss. It reads C through its own clang
front end. Feeding it rustc's LLVM IR would need matching LLVM versions
and a way to map Rust's atomics and the futex syscall onto GenMC's
primitives (**unconfirmed**; not tried). No GenMC model of challoc
exists, so every weak-memory result here is loom's.

### The SC slab model

`src/model/` is an SC interleaving explorer of the slab protocol: one
class, 2 slabs of 4 slots, 1-2 arenas, 2-3 threads; Refill, Write,
Release and Purge split into `class/`'s atomic steps; depth-first search
with memoisation (`model/steps.rs`), and invariants for no double hold,
held bit set, the calloc zero claim, `live` bounds and set membership.
It has no orderings or stale reads, no lock or futex internals, no
publication and nothing of the background thread. Its invariants are
the properties P1's loom models assert.

## loom models

Each model below runs a copy of the code it names (`tests/loom/src`).
Every model has at least one control, a mutant that must fail, which
shows the check can fail.

- P2, `lock.rs` (`RawLock`): two and three lockers bump a loom
  `UnsafeCell` counter. No race (mutual exclusion and the
  acquire/release edge), the count is exact, and no deadlock (no lost
  wake-up between the `swap(2)` and the sleep, nor for a sleeper behind
  another waiter). Controls: a mutant whose woken waiter retakes the
  lock by a compare-and-exchange to `LOCKED`, dropping a sleeper's
  `CONTENDED` mark, deadlocks (`loom_lock_dropped_mark_loses_a_wake_up`),
  and a bare wait on the stand-in deadlocks (`loom_futex_stand_in_sleeps`).
- P1, `bitmap.rs` (`AtomicBitmap`'s whole-word steps over one loom
  word, with copies of the class steps around it, citing `slab.rs` and
  `class/mod.rs`): two owners release while a third thread reserves
  through `live` under the `RawLock` copy and claims; one owner releases
  two slots to two claimers; `init` is published by `Release` and
  `Acquire`. Slot data is a loom `UnsafeCell` per slot. No race between
  a release and the next owner, disjoint masks, no padding claimed, no
  double release, and the `live` reservation is met on the first pass
  (one pass more per competing claimer): the `Relaxed` hint load always
  sees a bit that `live` promised, because the `Acquire` load of `live`
  reads the releaser's `AcqRel` `fetch_sub`, which follows its `Release`
  `fetch_and`. Controls: a `Relaxed` release, a `Relaxed` claim and a
  `Relaxed` commit each race or leave the slab unpublished, and a write
  to an unclaimed slot races (`loom_bitmap_unclaimed_write_is_a_race`).
  Not modelled: sub-word views, more than one word, purge, and the
  claim's `xorshift` start word.
- P3, `background.rs` (`purge/background.rs`, `purge.rs`): the free,
  the starter and the thread. `loom_background_wake` (both fences)
  passes; without the fences, or with either one alone, the thread is
  left asleep without a deadline (three `should_panic` controls).
- P4, `init.rs` (`api::init_slow`, `shared`, `shared_if_ready`): two
  callers and a free, and a re-entrant `init`. `init` runs once, every
  caller and the free read the value `init` wrote with no race, no
  waiter is left spinning, re-entry aborts, and no other thread takes
  that abort. Controls: a `Relaxed` `READY` store races; a waiter on an
  unpublished `INITIALISING` fails. loom cannot explore two waiters
  spinning at once (they yield to each other with no bound), so each
  model has one.
- P5, `free.rs` (the `AtomicBitmap` copy over one loom word; copies of
  `take_group`'s tail, a claimer's `Acquire` watermark load, and the
  free path's checks in `Shared::set_slot` and `home_owns`, through the
  class line): an allocating thread hands its slot to the freeing thread
  while a third claims and releases the word's other slot. With a
  `Release`/`Acquire` hand-off, every free sees the slab handed out, the
  bit set and the owner stored, with no race on the block. The fast
  path's load is of the class line's watermark copy, stored after the
  watermark, so a synchronised free can also read the copy behind
  (`Lagged`): a fast-path miss whose slow path re-reads the table's
  watermark and finds the slab. With a `Relaxed` hand-off all four
  outcomes appear: the checks pass, the watermark reads below the slab,
  the bit reads clear, and the owner word reads as its initial 0. Each
  of the last three is a fast-path miss; the slow path re-reads, and
  only a stale re-read too causes a spurious abort or releases the slot
  to arena 0 (possible, not modelled). Controls: the `Relaxed` hand-off
  with the free touching the block races; with the claimer's watermark
  load `Relaxed`, a synchronised free can read the owner as 0. The
  `free_flags` route bit is modelled in `flags.rs`, not here.
- Arena bound counts, `count.rs` (copies of `vspec/count.rs`'s wrappers,
  `Relaxed` as in `arena.rs`, driven by copies of `assign` under the
  `RawLock` copy, `count_in`, `unbind`, `load_bound` and
  `try_bind_idle`): no `unbind` reads 0, no holder reads its arena's
  count below its own tickets, the counts end at the tickets out, and a
  `try_bind_idle` wins only on an arena with no ticket out. Controls: a
  `fetch_add` split into a load and a store loses a ticket; a split
  compare-and-exchange binds twice.
- Arena count and table, `narenas.rs` (copies of `vspec/count.rs`'s
  `store` and `load` and `vspec/entry.rs`'s, driven by copies of
  `create` under the `RawLock` copy, `count`, `get_existing` and the
  index walk): two creators and a scanner, `MAX_ARENAS` 3. The count
  stays in range and never goes back for a reader; a scan finds every
  explicit arena below the count it read built and reads it race-free;
  a `create` under the lock reads the count as the lock's value with
  every arena below it built, the frozen table that `api::count_held`
  and `api::arena_held` prove for the fork handlers in the SC model; and
  the count ends at the lock's value. Controls: a `Relaxed` count store
  lets a scan see a count before its arena; a `Relaxed` entry store
  races.
- Policy position, `position.rs` (`Policy::scaled16`, one more than the
  position; copies of `apply`'s swap and `position`): two concurrent
  appliers and a reader. Every load is a stored value, at most
  `FRAC_ONE + 1`, and the swaps' results and the final value form one
  chain. Control: a swap split into a load and a store breaks the chain.
- Class table watermark, `watermark.rs` (copies of `store_usize`,
  `load_usize`, `load_usize_seen` and `store_usize_at`, driven by copies
  of `take_group` under the `RawLock` copy, `committed_slabs`,
  `watermark` and `mirror_watermark`): two hand-outs and a reader. Every
  load is at most the slab count and never goes back, `take_group`'s
  `check_zero` comparison under the lock always holds, an `Acquire` load
  sees the owner of every group below it, and the class line's copy
  follows the watermark. Controls: a `Relaxed` watermark store does not
  publish the owner; a copy stored after the lock's release lags the
  watermark.
- Slab descriptors' lock-protected fields, `slab.rs` (a copy of the six
  `DescPerm` accessors, under two `RawLock` copies for the commit lock
  and a class lock, the permission a loom `UnsafeCell` in the value of
  the lock that holds it): one thread hands a slab out (`take_group`'s
  `init` and the move between the locks, then `set_state(Active)`), and
  another reads the three fields through the permission under the class
  lock and writes `Empty` and a stamp. Every read returns the last write
  through the permission, with no race on the permission. Control: a
  read outside the class lock, after a `Relaxed` flag, is stale.
- The background thread's exit, `exit.rs` (copies of `background::main`'s
  loop and `request(false)`): the thread returns after a withdrawal, also
  with another kick in between. Controls: a withdrawal without its kick,
  or a `Relaxed` `SEQ` load in the thread, leaves it asleep. The exit
  does not depend on P3, because `request(false)` kicks unconditionally.
- The background thread's publication against its exit, `publish.rs`
  (copies of `start`'s two state lock holds and kick, `main`'s exit and
  `request(false)`): after a final withdrawal the thread has exited, and
  neither the state nor `PRESENT` says it is present, also when it exits
  between `start`'s two holds. Control: publishing `thread.is_some()`
  alone, without the `exited` test, leaves the exited thread published.
  The flags are under the state lock, so this model is about the
  interleaving, not the orderings; Verus proves the lock-value side
  (`exited ==> !present` in `axiom_state_lock_inv`).
- The once-cells, `oncecell.rs`. The feature line (copies of
  `line_ready`, `feature_line_fork_child` and the `LINE_STATE` wrappers
  `line_load`, `line_compare_exchange` and `line_store`): the line is
  built once, and every reader that sees `LINE_READY` reads it
  race-free. The deferred cgroup record (copies of `vspec/byte.rs`'s
  wrappers driven by copies of `Deferred::get`, `discover` and
  `fork_child`): one discovery runs, and every reader that sees `DONE`
  reads the record race-free. Controls: a `Relaxed` `LINE_READY` (or
  `DONE`) store lets a reader see it without the write. The fork-child
  resets run on one thread, as in the child: the next request builds
  (discovers) rather than waits. Verus proves the hand-off of the cells'
  permissions (`api::LinePred`, `sys::cgroup::PhasePred`); the models
  check that the release/acquire pairs give the edges it relies on.
- The free path's dispatch byte, `flags.rs` (a copy of `vspec/flags.rs`'s
  `RouteByte`, driven by copies of `init`'s `Shared::put_flags` and
  publication, `Arenas::build`'s `Shared::set_multi_arena`, and the free
  path's `Shared::free_flags` load): every value a load or the
  `fetch_or` returns is `push_flags` with or without `ROUTE`, with no
  race on the storage `init` wrote. Controls: a `Relaxed` `READY` store
  leaves the reader unordered after `init`'s write; a bit outside
  `ROUTE` fails the assertions.
- The double-free state byte, `slotstate.rs` (a copy of
  `slotstate::free`'s compare-and-exchange from `LIVE`): two
  simultaneous frees of one live slot. In every interleaving exactly one
  finds the slot `LIVE` and moves it; the other sees what the first
  stored and aborts by name. Control: the load-then-store form lets both
  pass.

## Seams

A seam that cites loom names its models in its ledger status
(`loom: <mod>::<test>`, [SEAMS.md](SEAMS.md), "Status"), and the comment
above it in its `.overrides` file under `verus/ledger/src/` gives its
C11 argument. Because a model runs a copy of the code, a loom citation
makes a seam at most partly checked; the status names what the models
leave unchecked. Where the ledger also cites Kani, the Kani harness
checks the wrapper's own body on one thread.

40 seams cite loom, and every one is partly checked
(`grep -rh 'Status: .*loom:' verus/ledger/src`). `RawLock::lock_slow`
(`lock.rs.md`, tool loom/GenMC) stays trusted: its contract is
termination, and the lock models show deadlock freedom only. Drepper's
mutex is not starvation-free, so a woken waiter can lose the word to
another thread's exchange as often as other threads retake the lock.

| ledger file (`verus/ledger/src/`) | seam | status |
|---|---|---|
| `api/mod.rs.md` | `state_load` | partly checked (loom) |
| `api/mod.rs.md` | `state_claim` | partly checked (loom) |
| `api/mod.rs.md` | `state_publish` | partly checked (loom) |
| `api/mod.rs.md` | `line_load` | partly checked (loom) |
| `api/mod.rs.md` | `line_compare_exchange` | partly checked (loom) |
| `api/mod.rs.md` | `line_store` | partly checked (loom) |
| `api/mod.rs.md` | `init_slow` | partly checked (loom) |
| `class/table.rs.md` | `store_owner` | partly checked (loom) |
| `lock.rs.md` | `RawLock::lock_slow` | trusted |
| `lock.rs.md` | `LockGuard::drop` | partly checked (loom) |
| `purge/background.rs.md` | `main` | partly checked (loom) |
| `slab.rs.md` | `SlabDesc::init` | partly checked (loom) |
| `slab.rs.md` | `SlabDesc::state_byte_held` | partly checked (loom) |
| `slab.rs.md` | `SlabDesc::dirty_held` | partly checked (loom) |
| `slab.rs.md` | `SlabDesc::set_state` | partly checked (loom) |
| `slab.rs.md` | `SlabDesc::arena_held` | partly checked (loom) |
| `slab.rs.md` | `SlabDesc::set_dirty_at` | partly checked (loom) |
| `vspec/atom.rs.md` | `store_usize` | partly checked (Kani, loom) |
| `vspec/atom.rs.md` | `load_usize` | partly checked (Kani, loom) |
| `vspec/atom.rs.md` | `store_usize_at` | partly checked (Kani, loom) |
| `vspec/atom.rs.md` | `load_usize_seen` | partly checked (loom) |
| `vspec/atom.rs.md` | `load_u16_at` | partly checked (Kani, loom) |
| `vspec/byte.rs.md` | `load` | partly checked (Kani, loom) |
| `vspec/byte.rs.md` | `store` | partly checked (Kani, loom) |
| `vspec/byte.rs.md` | `compare_exchange` | partly checked (Kani, loom) |
| `vspec/count.rs.md` | `fetch_add` | partly checked (Kani, loom) |
| `vspec/count.rs.md` | `fetch_sub` | partly checked (Kani, loom) |
| `vspec/count.rs.md` | `store` | partly checked (Kani, loom) |
| `vspec/count.rs.md` | `load` | partly checked (Kani, loom) |
| `vspec/count.rs.md` | `swap` | partly checked (Kani, loom) |
| `vspec/count.rs.md` | `compare_exchange` | partly checked (Kani, loom) |
| `vspec/entry.rs.md` | `store` | partly checked (Kani, loom) |
| `vspec/entry.rs.md` | `load` | partly checked (Kani, loom) |
| `vspec/flags.rs.md` | `new` | partly checked (Kani, loom) |
| `vspec/flags.rs.md` | `load` | partly checked (Kani, loom) |
| `vspec/flags.rs.md` | `fetch_or` | partly checked (Kani, loom) |
| `vspec/word.rs.md` | `WordPerm` | partly checked (loom) |
| `vspec/word.rs.md` | `fetch_or` | partly checked (Kani, loom) |
| `vspec/word.rs.md` | `store` | partly checked (Kani, loom) |
| `vspec/word.rs.md` | `load` | partly checked (Kani, loom) |
| `vspec/word.rs.md` | `fetch_and` | partly checked (Kani, loom) |

The arguments, by group (the overrides comments give each in full):

- `init_slow` (P4 models): it terminates given `init` returns, a fair
  scheduler and visible stores. `state_claim`: the exchange reads the
  last value in `STATE`'s modification order, for every ordering, so the
  permission carries it; at most one success follows from `STATE`'s
  invariant, given `axiom_state` denotes one invariant (assumed).
  `state_load`, `state_publish` and the `line_*` wrappers give the
  release/acquire pairs the init and once-cell models check.
- `background::main` (exit models): it returns after a final
  `request(false)`, given each pass returns and the exit takes the state
  lock.
- `LockGuard::drop` (P2 models): its body is `RawLock::release`.
- The bitmap word wrappers (`vspec/word.rs`): C11 per-location
  coherence gives each contract, and the P1 and P5 models check the
  cross-location edges the SC proof needs. The proofs use `load` only
  for "a set bit is not a guard slab", true of every value. `WordPerm`
  is the SC abstraction itself; its content is the wrappers' contracts
  and the fact that every write goes through them, which `access.rs`
  checks.
- The count wrappers (`vspec/count.rs`): RMW atomicity holds for every
  ordering (`fetch_add`, `fetch_sub`, `swap`, `compare_exchange`); each
  invariant bounds one atomic by a constant (`NarenasPred`, `FracPred`)
  or names one counter and pure ghost tickets (theorem 34), so any
  ordering keeps it; `store` writes under the creation lock, which
  orders the stores; `load`'s contract excludes `Release` and `AcqRel`,
  for which `core`'s `load` panics. Count, narenas and position models.
- The watermark wrappers (`vspec/atom.rs`): `store_usize` writes under
  the commit lock, and `CommittedPred` bounds one atomic by a constant;
  `load_usize` and `load_usize_seen` get the held permission's value by
  per-atomic coherence, and `load_usize_seen` hands the reader the owner
  words' witness markers below the watermark (`table::WatermarkPred`);
  `load_u16_at` reads a word whose writes are its initial 0 and
  `store_owner`'s one store, so a load ordered after the store reads it.
  Under SC the witness says which loads are so ordered; under C11 an
  `Acquire` watermark load or the program's hand-off supplies the edge,
  and a free not ordered after its allocation can read 0 (P5's recorded
  outcome). `store_owner` is the owner word's only write (Verus's
  markers) and records the value (`u16_value`) inside the word's
  invariant (`table::OwnerPred`). Watermark, free and bitmap models.
- The entry and byte wrappers (`vspec/entry.rs`, `vspec/byte.rs`): the
  narenas and once-cell models check their release/acquire pairs.
- The flag byte (`vspec/flags.rs`): every write is `new`'s or a
  `fetch_or` within `ROUTE`, so every value in the modification order
  keeps the fixed bits, for any ordering, for a reader that happens after
  `init` (the premise of the trusted seams that hand out a well-formed
  `&Shared`). The `RouteByte` type keeps its `AtomicU8` private to its
  module, so its only operations are the wrappers. Flags models.
- The slab descriptor accessors (`slab.rs`): every access through a
  permission is made holding the lock whose value holds it (the commit
  lock's, then the class lock's), so successive holders are ordered by
  happens-before, and per-location coherence gives the last write
  through it, for `Relaxed` too, given the lock edge `axiom_acquired`
  assumes. `access.rs` checks that `slab.rs` writes the three fields
  only in the accessors. The slab model runs the hand-over between the
  two locks on one thread and checks the class lock's edge to the next
  holder.

The checks leave out what the statuses list: the bodies themselves
(the models run copies), configurations larger than the models', the
kernel's futex (replaced by the stand-in), and the C11 arguments, which
`access.rs` backs by matching source text only. Remaining weak-memory
work is tracked in
[wolfi-dev/challoc#397](https://github.com/wolfi-dev/challoc/issues/397).
