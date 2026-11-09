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

408 lines of source carry an `Ordering::` (`grep` over `src/`,
leaving out `tests.rs`, `linux_tests.rs`, `src/heap/tests/`,
`src/kani/`, `src/model/`, `src/heap/tlscheck.rs` (built only with
`cfg(challoc_tls_check)`), inline `#[cfg(test)]` and `#[cfg(kani)]`
modules, and comment lines). The count keeps lines in `#[cfg(test)]`
functions and statements outside those modules: `sys/abort.rs` and
`sys/pac.rs` have only such lines (test hooks), and several other rows
include a few.
Files that pass an ordering through an imported name or a `vspec`
wrapper argument are not in this count.
Most of these lines are `Relaxed` statistics, configuration and
heuristics. `purge/background.rs` holds the crate's two `fence` calls
(in `wake` and `start`, P3), and there is no `compiler_fence`.
`sys/raw.rs`'s syscall `asm!` blocks have no `nomem`, so each syscall is
a compiler barrier.

| File | Lines | Atomics |
|---|---|---|
| `purge/background.rs` | 56 | `PRESENT`, `REQUESTED`, `SEQ`; the lazy start's `AUTO`, `AUTO_FAILS`, `AUTO_NEXT`, `AUTO_SINCE`, `AUTO_BASE`, all `Relaxed` (rate limits and heuristics; the request they set or withdraw is written under the state lock, as `request`'s) |
| `thp.rs` | 45 | init-time configuration and counters |
| `sys/cgroup.rs` | 40 | sampled cgroup limits and the deferred discovery's phase |
| `purge.rs` | 37 | flags, counters, `Decayer` |
| `arena.rs` | 30 | registry `AtomicPtr`s, counts, statistics |
| `api/mod.rs` | 28 | `STATE` (`word`, `line`), `INIT_TID`, `SEALED` |
| `stats.rs`, `policy.rs` | 26, 26 | statistics and the sampled policy, `Relaxed` |
| `slab.rs` | 24 | `SlabDesc`: state, arena, `live`, dirty, `closed`, `shadow`, `stamp` |
| `class/mod.rs` | 16 | per-class statistics counters (`claimed`, `released`, `refilled`, `compacted`, `clamped`, `compact_on`, `compact_off`, `gate_refill`), `Relaxed` |
| `class/table.rs` | 16 | watermark; bitmap and owner words; the watermark copy's stores (`vspec::atom::store_usize_at`) |
| `lock.rs` | 12 | the lock word; the fork window's record (`FORK_PID`, `FORK_HOLDER`), `Relaxed` |
| `heap/slotstate.rs` | 11 | `double_free_scan`'s per-slot state byte |
| `bitmap.rs` | 10 | `&[AtomicU64]` words |
| `class/pages.rs` | 8 | `hint_due`, `hint_armed`, `pages_due`, `Relaxed` |
| `huge/mod.rs`, `huge/free.rs` | 7, 2 | `retain_max`, `purge_on_free`, `mixed_guards`, `Relaxed` |
| `heap/tcaches.rs` | 6 | `[AtomicPtr<u8>; 1024]` |
| `heap/shared.rs` | 4 | `free_flags`; the class lines' watermark copies, read through `class::load_mirror` |
| `sys/abort.rs`, `sys/pac.rs` | 2, 2 | test hooks only: `REAL_ABORT` and `TEST_OPS`, under `#[cfg(test)]` |

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
  GenMC's port (`tests/genmc/background.rs`, "GenMC" below) checks the
  pair under RC11's `SeqCst` fences, and under IMM and ARMv8.
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

- `api::init_slow`: `STATE.word`'s `compare_exchange(UNINIT,
  INITIALISING, Acquire, Acquire)` (`claim_init`, through
  `vspec::count::compare_exchange`), `init()`, then its `store(READY,
  Release)` (`publish_ready`, `vspec::count::store`). Readers load
  `Acquire` (`load_state`, `vspec::count::load`), and waiters spin on an `Acquire` load. The `READY`
  store publishes `SHARED`, `FEATURES` and `FEATURE_LINE` (`UnsafeCell`
  statics in `api/mod.rs`), the class table's page geometry
  (`layout::adopt_page_geometry`), and the init-time `Relaxed`
  configuration stores (thp, purge, background, `SEALED`).
- The fast paths skip the `READY` load (`api::shared_base`,
  `shared_deref`, `shared_ptr`): the thread's own TLS heap pointer is
  the witness, since heaps are created under `shared()`. This crosses
  no thread, because the TLS slot is per thread (`sys/tls.rs`).
- `STATE`'s invariant (`api::StatePred`, which `STATE`'s verified
  initialiser builds with the process-start token, `api::axiom_start`)
  states this premise for the
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
history. Its `UnsafeCell` reports data races, which is the check P1
needs for slot data. Two limits, both run with stock loom 0.7.2 on the
two shapes of `tests/genmc/litmus.rs` in a scratch crate that is not in
the tree (so CI does not rerun the loom half), and reached by GenMC
("GenMC"):

- No load buffering. loom runs each load against stores already made,
  so in the shape where each of two threads loads one atomic and then
  stores the other, no load reads the other thread's later store: none
  of its 15 executions has both loads read 1.
- A store history of 7. loom keeps the last 7 stores of an atomic
  (`MAX_ATOMIC_HISTORY` in loom 0.7.2's `src/rt/mod.rs`). When a thread
  stores 1 to 8 to an atomic and then sets a `Relaxed` flag, a reader
  that sees the flag never reads the first store in loom's 393
  executions; with 3 stores it does (73 executions). No loom model here
  stores one atomic more than 7 times.

challoc's statics are built in `const` context, which loom's
constructors cannot do, and some atomics are made from raw metadata
memory (`class/table.rs` casts addresses to `&AtomicU64` and
`&AtomicU16`). So the crate is not built under `cfg(loom)`, and every
model in `tests/loom` (its own crate) is a copy of the code it checks,
step by step, naming the function each step mirrors
(`tests/loom/src/lib.rs`). The atomic wrappers those steps call are
challoc's own: the loom crate compiles `src/vspec/{atom,byte,count,entry,flags,word}.rs`
and `src/sync.rs` as its own modules under `cfg(loom)`, where
`crate::sync::atomic` is loom's, so a model runs the wrappers' real
bodies. Two source tests keep the copies tied to the code:

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

GenMC (0.20.0, the `genmc` package, built against LLVM 22) explores a
program's executions exhaustively and statelessly, with no bound on
store history, under RC11 (its default), IMM (`--imm`) or ARMv8
(`--arm8`). Its Rust mode compiles a `.rs` file with rustc, whose LLVM
major version must be GenMC's (rust-1.99 is LLVM 22), links genmc-std
as `std`, and interprets the LLVM IR, so `Relaxed`, `Acquire` and
`Release` mean what rustc emits. A plain access racing another is a
reported non-atomic race, which is the check P1 needs for slot data.

`tests/genmc` holds the harnesses, one template per protocol, each a
GenMC port of a loom model, and `tests/genmc/cases` lists the cases made
from them (`run.sh` writes each as one file). As in the loom crate, the
`vspec` wrappers are challoc's own: `prelude.rs`, through the loom
crate's `tests/loom/src/vspec.rs`, compiles
`src/vspec/{atom,byte,count,entry,flags,word}.rs` and `src/sync.rs`
unchanged, without `cfg(loom)`, so they run over
`core`'s atomics as in the release build. The steps around them are
copies, as the loom models' are:

- `flags.rs` (P5, `vspec::flags`): the dispatch byte, with one and two
  builders.
- `watermark.rs` (`vspec::atom`): the hand-out watermark and its copy,
  with 2 and 3 slabs, and the copy's install racing two hand-outs (three
  holders of the commit lock, which the loom model's comments call too
  large to exhaust).
- `count.rs` (`vspec::count`): the arena bound counts
  (`bind`/`assign`/`unbind`, and `try_bind_idle` with and without a
  rival).
- `narenas.rs` (`vspec::count`, `vspec::entry`): the arena count and
  table.
- `slotstate.rs` (`vspec::byte`): two simultaneous frees of one slot.
- `bitmap.rs` (P1, `vspec::word`): release then reserve, two claimers,
  and `init`'s publication through the commit count.
- `litmus.rs` (`vspec::count`): the two shapes of loom's limits above.
- `init.rs` (P4, `api/mod.rs`'s own functions over `STATE` and
  `INIT_TID`, no `vspec` wrapper, so a copy through and through): one,
  two and three callers with a free, a re-entrant `init`, and the
  publisher's read-back. Three callers put two waiters on `STATE` at
  once, which loom cannot run.
- `background.rs`, `exit.rs`, `publish.rs` (P3): the background
  thread's wake-up against a class's first `Empty` slab, its exit after
  `request(false)` (also with another kick in between), and `start`'s
  publication against its exit, each also with two spurious wake-ups
  of the thread.

The P3 harnesses wait on `SEQ` through `prelude.rs`'s futex stand-in,
since GenMC has no futex: `futex_wait(word, val)` blocks until a
`Relaxed` load of the word reads another value than `val`, and
`futex_wake` does nothing. Each harness has one waiter, as the code
has (`FUTEX_WAKE` of one). The stand-in assumes of the kernel no lost
wake-up given the value check: a waiter whose compare read `val` is
woken by a wake that follows a change of the word. It wakes the waiter
at the change itself, which holds the code to `kick`'s `fetch_add`
then `futex_wake` (both pinned); that `kick` is the only writer of
`SEQ` rests on review, not a test. Spurious wake-ups are allowed: a
case's `SPURIOUS` makes the thread's first `SPURIOUS` waits return at
once, with no compare (GenMC explores every interleaving, so at once
stands for at any later point in that wait). The wait adds no ordering
(the kernel's hash-bucket lock would), so the callers' own `SEQ` loads
are all that order them. The wait, and `init.rs`'s spin on `STATE`, are
`await_until!`, GenMC's manual spin-assume markers: one pass of the
loop's loads, which blocks the thread when it fails. A case run with
`+live` checks liveness (`--check-liveness`): a thread blocked with
each load of its last pass reading its location's last write is a
reported "Liveness violation", which is how a lost wake-up, or a
waiter left spinning, fails. Those runs turn GenMC's automatic
spin-assume pass off: it might take the background thread's loop,
which sleeps in the wait, for a spin on its loads, and the liveness
check assumes a blocked spin would later read newer values, so it
would not report a thread asleep on a stale `REQUESTED`. Under RC11
`background.rs` checks `wake`'s and `start`'s fences with RC11's
`SeqCst` fences, the semantics C++20 took from RC11 (P0668), not
loom's stronger ones (P3 above). GenMC 0.20 cannot run a thread
created by a thread other than the main one under IMM and ARMv8
(`Attempt to access non-allocated memory!` on the new thread's
closure, or the `allocInfoMap_` internal check), so the main thread
spawns the background thread first, and it waits (`prelude.rs`'s
`created`) for a `Release` store where its creator would create it:
the happens-before edge `pthread_create` gives.


`tests/loom/tests/pins.rs` ties the copies to the code from both sides
(`genmc_copies_in_place`). `GENMC_PINS` pairs each source line a harness
copies (its atomic operations and their orderings, and the guards and
lock steps around them) with the harness's copy. Both match as whole
lines after trimming whitespace. The source lines must stay in their
file's order, each in the same function as the pin before it unless it
is a function's own `fn` line (pinned as an anchor), so a changed source
line fails until its pair is revisited, and so does a changed copy.
An ordering a harness takes from a case constant (`PUBLISH`, `ENTRY`),
and a control switch (`SPLIT`, `LOAD_STORE`, `MUTANT`), must be the
source's in every `pass` case of `tests/genmc/cases` (`GENMC_SHIPPED`),
and every `Ordering` constant a case sets must be listed there. Every
template but `litmus.rs` must have pins.
`loom_pins_cover_every_atomic_in_pinned_fns` also covers the functions
the GenMC pins fall in, so a new line with an `Ordering::`, a `fence(`
or a `futex_` call there fails until it is pinned or waived. The pins
run in the `loom` check, which runs on every pull request.

Every case loom also runs has loom's controls (the `Release` stores made
`Relaxed`, a read-modify-write split into a load and a store), each a
`fail` case that must fail with its expected report. Two controls loom's
models do not have: `exit.rs`'s kick before the `REQUESTED` store, and
`publish.rs`'s `Relaxed` `SEQ` load, each of which leaves the thread
asleep. `sh tests/genmc/run.sh`
runs every case under each model it names and checks each verdict: 126
runs, all as expected, in 63 s from a cold cargo cache (43 s of it the
cases) on a 4-vCPU guest. Runs skip
GenMC's random estimation pass (`--disable-estimation`), so each
explores in the same order every time and reports the same error.
Every shipped configuration passes under RC11; all but `narenas` and
`bitmap_two_claimers` also pass under IMM, so they do not rely on the
absence of load buffering, and eleven pass under ARMv8 as well (the cases
file names the models of each). Under ARMv8 GenMC 0.20 did not report
the race of `bitmap_relaxed_release` (that run passed), so the ARMv8
runs are evidence for the harnesses' assertions, not for their race
checks.
The load-buffering shape's outcome is never reached under RC11, which
forbids it, and is reached under IMM and ARMv8; the first of 8 stores
is read after the flag under RC11 and IMM. The `genmc` check
(`.chainguard/ci/genmc.yaml`) runs `run.sh` on pull requests that change
the wrappers, `src/sync.rs`, the files the harnesses copy steps from,
`Cargo.toml`, `tests/genmc/` or `tests/loom/src/vspec.rs`. GenMC found no bug in challoc.

The seam ledger cites the `pass` cases next to loom, as evidence only
(`GenMC: <case> [<models>]`, [SEAMS.md](SEAMS.md), "Status"), in 22
statuses (`grep -rh 'Status: .*GenMC:' verus/ledger/src`): the 19
`vspec` atomic wrappers whose bodies a case runs;
`table::store_owner`, whose atomic line a harness copies; and
`api::init_slow` and `background::main`, whose steps `init.rs` and the
P3 harnesses copy; each copy pinned in `GENMC_PINS`. The P3 citations
also rest on the futex stand-in's assumptions, which their statuses
name. The cases erase
every permission (`Tracked::assume_new()`), so none checks a wrapper's
contract or sees a ghost value; what they show is that the callers'
hand-offs hold in the cases' configurations, under the models each case
names. Every such status stays partly checked and names what the cases
leave out for it: other call sites, the models a case does not run
under, a copy run in place of the body.

What GenMC does not check here:

- No futex. The harnesses take genmc-std's `Mutex` for the `RawLock`
  copy, so they say nothing of P2. P3's harnesses rest on the futex
  stand-in's assumptions above, and none of them takes a timeout (the
  models' worst case, no deadline).
- A `+live` run cannot see a wait the compiler emptied of loads. When
  nothing in a case writes a private static (loom's no-kick exit
  control writes no `SEQ`), rustc at `-Copt-level=1` treats it as a
  constant: the IR has no `SEQ`, the wait is `await_until!` with no
  load (`__VERIFIER_spin_end(false)`), and GenMC explores 1 execution
  and reports no error and no blocked execution, also with
  `--disable-load-annotation`. `futex_wait` guards its own word by
  passing it through `black_box`, after which that control fails with
  "Liveness violation" (`exit_no_kick`); `init.rs`'s wait on `STATE`,
  which every case writes, has no such guard.
- `init.rs` (P4) is a copy of `api/mod.rs`'s functions through and
  through, tied to the code only by its line pins; so are the P3
  harnesses' steps, which call no `vspec` wrapper either. They leave
  out what their headers name, as the loom models do (the fork gate in
  `init.rs`, the class lock and the state lock in `background.rs`, the
  state lock in `exit.rs`).
- The GenMC pins match whole lines, not what they do. A harness step
  that is not a line-for-line copy is not pinned: `bitmap.rs`'s
  `lowest_n_bits`, which the harness writes its own way, `RawLock`'s own
  body (a `Mutex` here; the lock and unlock calls are pinned), and the
  order of steps that the source splits across files (`put_flags` before
  `READY`'s store, `take_group`'s bitmap `init` before the watermark's
  store) beyond each file's own order. The harness side is checked for
  presence, not order, so a short copy (`return None;`, `drop(g);`) can
  still be found elsewhere in its harness. A switch that is not an
  `Ordering` (a `bool` or a `u32` mode) is checked only if
  `GENMC_SHIPPED` lists it. A new atomic line in a pinned function is
  caught only when its code names an `Ordering::`, a `fence(` or a
  `futex_` call, as for loom's pins.
- `-Copt-level=1` only. At 0, GenMC 0.20 stops on rustc's IR (an
  internal check in `PromoteMemIntrinsicPass`, or `Constant
  unimplemented for type`); at 1, LLVM can hoist a plain read of a
  static above the test that guards it, a race the source does not
  have, so the harnesses' plain reads behind a flag test are volatile.
- GenMC rejects mixed-size accesses and some heap accesses in racy
  executions, so the harnesses keep their state in statics, one field
  per access size.
- The `narenas` cases and `bitmap_two_claimers` run under RC11 only:
  under IMM and ARMv8, GenMC 0.20 stops on an internal check
  (`allocInfoMap_`).

### The SC slab model

`src/model/` is an SC interleaving explorer of the slab protocol: one
class, 2 slabs of 4 slots, 1-2 arenas, 2-3 threads; Refill, Write,
Release and Purge split into `class/`'s atomic steps; depth-first search
with memoisation (`model/steps.rs`), and invariants for no double hold,
held bit set, the calloc zero claim, `live` bounds and set membership.
It has no orderings or stale reads, no lock or futex internals, no
publication and nothing of the background thread. Its invariants are
the properties P1's loom models assert.

### Hardware stress tests

Three ignored unit tests run the shipped code, not a copy, for
`CHALLOC_STRESS_SECS` seconds each, on as many threads as the process
has CPUs unless `CHALLOC_STRESS_THREADS` says otherwise, and check plain
data across the edges P1 and P2 order:

- `lock::tests::stress_lock_edges` (P2): twice that many threads take
  one `Lock` over four plain words, check that the words are equal and
  bump them all. A torn read or a lost update is a critical section
  whose accesses were not ordered before the next holder's.
- `bitmap::tests::stress_bitmap_claim_release_edge` (P1): the threads
  claim and release the 4 slots of one bitmap word; each claimer checks
  that its slot's data reads 0, as the last holder left it before
  `release`.
- `class::tests::stress_class_cross_thread_release` (P1 end to end):
  producers claim batches from a class and hand them to consumers, which
  release them from their own threads while a purger purges the empty
  slabs. Its 4-slot slabs send most releases through the class lock, so
  it does not isolate the bitmap edge, and no weakened ordering has been
  tried against it.

They are evidence, not proof: a reordering the hardware never makes
passes. x86_64 (TSO) lets only a later load pass an earlier store, so the
`weakmem-stress` check (`.chainguard/ci/weakmem.yaml`, on request) runs
them 60 s each on aarch64. Each ordering below was weakened to `Relaxed`
alone and the test that targets it run for 60 s, on an 8-vCPU aarch64
guest and an 8-vCPU x86_64 one:

| Weakened ordering | aarch64 | x86_64 |
|---|---|---|
| `RawLock::try_acquire`'s `compare_exchange` success `Acquire` | caught in 2 of 2 runs: 40,800 and 285,126 torn reads, 35,473,359 and 47,362,058 lost updates, in 594M and 656M acquisitions; the first torn read at the 33,379th and 10,480th update | not caught (252.5M acquisitions) |
| `AtomicBitmap::release`'s `fetch_and` `Release` | caught in 5 of 5 runs: 5 to 25 stale claims in 364M-425M claims, the first within about 7 s | not caught (209M claims) |
| `RawLock::release`'s `swap` `Release` | not caught: 535M acquisitions, and 1,217M in 120 s | not caught |
| `AtomicBitmap::claim_word`'s `fetch_or` `Acquire` | not caught (515M claims) | not caught |

The `release` mutant shows only if a critical section's access moves
past the unlocking swap (a store-store or load-store reordering), which
the core tried did not make in 1.2G acquisitions; the `try_acquire`
mutant, a load run ahead of the claiming CAS, shows at once. The
`claim_word` mutant did not show on aarch64, and is not expected to: in
the compiled test the data address is computed from the claimed mask, an
address dependency that orders the load on aarch64 whatever the C11
ordering says, and the allocator likewise computes a slot's address from
the claimed bits. That holds only while the compiler keeps the
dependency. A model checker sees that mutant (`tests/loom/src/bitmap.rs`
has it as a control). `Shared`'s publication (P4) has no hardware stress
test: a fork-based one, with `api::publish_ready`'s `READY` store
(`src/api/mod.rs`) weakened to `Relaxed`, caught nothing in 5,905 rounds
on aarch64.

## loom models

Each model below runs a copy of the code it names (`tests/loom/src`),
calling the `vspec` atomic wrappers' real bodies ("The tools").
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
  word, through `vspec/word.rs`'s wrappers as they are, with copies of the class steps around it, citing `slab.rs` and
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
- P5, `free.rs` (the `AtomicBitmap` copy over one loom word, through
  `vspec/atom.rs`'s and `vspec/word.rs`'s wrappers as they are; copies of
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
- Arena bound counts, `count.rs` (`vspec/count.rs`'s wrappers as they
  are, `Relaxed` as in `arena.rs`, driven by copies of `assign` under the
  `RawLock` copy, `count_in`, `unbind`, `load_bound` and
  `try_bind_idle`): no `unbind` reads 0, no holder reads its arena's
  count below its own tickets, the counts end at the tickets out, and a
  `try_bind_idle` wins only on an arena with no ticket out. Controls: a
  `fetch_add` split into a load and a store loses a ticket; a split
  compare-and-exchange binds twice.
- Arena count and table, `narenas.rs` (`vspec/count.rs`'s `store` and
  `load` and `vspec/entry.rs`'s, as they are, driven by copies of
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
- Class table watermark, `watermark.rs` (`store_usize`, `load_usize`,
  `load_usize_seen` and `store_usize_at` as they are, driven by copies
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
  (`exited ==> !present` in `purge::background::State`'s lock invariant,
  which `lemma_state_inv_unpublishes_exited` reads from it).
- The once-cells, `oncecell.rs`. The feature line (copies of
  `line_ready`, `feature_line_fork_child` and their `vspec/byte.rs`
  calls on `STATE.line`, `load`, `compare_exchange` and `store`): the line is
  built once, and every reader that sees `LINE_READY` reads it
  race-free. The deferred cgroup record (`vspec/byte.rs`'s wrappers as
  they are, driven by copies of `Deferred::get`, `discover` and
  `fork_child`): one discovery runs, and every reader that sees `DONE`
  reads the record race-free. Controls: a `Relaxed` `LINE_READY` (or
  `DONE`) store lets a reader see it without the write. The fork-child
  resets run on one thread, as in the child: the next request builds
  (discovers) rather than waits. Verus proves the hand-off of the cells'
  permissions (`api::LinePred`, `sys::cgroup::PhasePred`); the models
  check that the release/acquire pairs give the edges it relies on.
- The free path's dispatch byte, `flags.rs` (`vspec/flags.rs`'s
  `RouteByte` and wrappers as they are, driven by copies of `init`'s `Shared::put_flags` and
  publication, `Arenas::build`'s `Shared::set_multi_arena`, and the free
  path's `Shared::free_flags` load): every value a load or the
  `fetch_or` returns is `push_flags` with or without `ROUTE`, with no
  race on the storage `init` wrote. Controls: a `Relaxed` `READY` store
  leaves the reader unordered after `init`'s write; a bit outside
  `ROUTE` fails the assertions.
- The double-free state byte, `slotstate.rs` (a copy of
  `slotstate::free`'s compare-and-exchange from `LIVE`, through
  `vspec::byte::compare_exchange` as it is): two
  simultaneous frees of one live slot. In every interleaving exactly one
  finds the slot `LIVE` and moves it; the other sees what the first
  stored and aborts by name. Control: the load-then-store form lets both
  pass.

## Seams

A seam that cites loom names its models in its ledger status
(`loom: <mod>::<test>`, [SEAMS.md](SEAMS.md), "Status"), and the comment
above it in its `.overrides` file under `verus/ledger/src/` gives its
C11 argument. Because a model runs a copy of the code, and runs a
wrapper's own body only within the part of C11 loom models, a loom
citation makes a seam at most partly checked; the status names what the models
leave unchecked. Where the ledger also cites Kani, the Kani harness
checks the wrapper's own body on one thread. Where it also cites GenMC
(`GenMC: <case> [<models>]`, "GenMC" above), the cases run the wrapper's
own body, or for `store_owner` a pinned copy of its atomic line, and for `init_slow`
and `background::main` a pinned copy of their steps, over `core`'s
atomics with the
permissions erased: evidence for the callers' hand-offs under the models
named, which moves no status (the "status" column marks a copy).

37 seams cite loom, and every one is partly checked
(`grep -rh 'Status: .*loom:' verus/ledger/src`). `RawLock::lock_slow`
(`lock.rs.md`, tool loom/GenMC) stays trusted: its contract is
termination, and the lock models show deadlock freedom only. Drepper's
mutex is not starvation-free, so a woken waiter can lose the word to
another thread's exchange as often as other threads retake the lock.

| ledger file (`verus/ledger/src/`) | seam | status |
|---|---|---|
| `api/mod.rs.md` | `init_slow` | partly checked (loom, GenMC copy) |
| `class/table.rs.md` | `store_owner` | partly checked (loom, GenMC copy) |
| `lock.rs.md` | `RawLock::lock_slow` | trusted |
| `lock.rs.md` | `axiom_acquired` | partly checked (loom) |
| `lock.rs.md` | `LockGuard::drop` | partly checked (loom) |
| `purge/background.rs.md` | `main` | partly checked (loom, GenMC copy) |
| `slab.rs.md` | `SlabDesc::init` | partly checked (loom) |
| `slab.rs.md` | `SlabDesc::state_byte_held` | partly checked (loom) |
| `slab.rs.md` | `SlabDesc::dirty_held` | partly checked (loom) |
| `slab.rs.md` | `SlabDesc::set_state` | partly checked (loom) |
| `slab.rs.md` | `SlabDesc::arena_held` | partly checked (loom) |
| `slab.rs.md` | `SlabDesc::set_dirty_at` | partly checked (loom) |
| `vspec/atom.rs.md` | `store_usize` | partly checked (Kani, loom, GenMC) |
| `vspec/atom.rs.md` | `load_usize` | partly checked (Kani, loom, GenMC) |
| `vspec/atom.rs.md` | `store_usize_at` | partly checked (Kani, loom, GenMC) |
| `vspec/atom.rs.md` | `load_usize_seen` | partly checked (loom, GenMC) |
| `vspec/atom.rs.md` | `load_u16_at` | partly checked (Kani, loom, GenMC) |
| `vspec/byte.rs.md` | `load` | partly checked (Kani, loom) |
| `vspec/byte.rs.md` | `store` | partly checked (Kani, loom) |
| `vspec/byte.rs.md` | `compare_exchange` | partly checked (Kani, loom, GenMC) |
| `vspec/count.rs.md` | `fetch_add` | partly checked (Kani, loom, GenMC) |
| `vspec/count.rs.md` | `fetch_sub` | partly checked (Kani, loom, GenMC) |
| `vspec/count.rs.md` | `store` | partly checked (Kani, loom, GenMC) |
| `vspec/count.rs.md` | `load` | partly checked (Kani, loom, GenMC) |
| `vspec/count.rs.md` | `swap` | partly checked (Kani, loom) |
| `vspec/count.rs.md` | `compare_exchange` | partly checked (Kani, loom, GenMC) |
| `vspec/entry.rs.md` | `store` | partly checked (Kani, loom, GenMC) |
| `vspec/entry.rs.md` | `load` | partly checked (Kani, loom, GenMC) |
| `vspec/flags.rs.md` | `new` | partly checked (Kani, loom, GenMC) |
| `vspec/flags.rs.md` | `load` | partly checked (Kani, loom, GenMC) |
| `vspec/flags.rs.md` | `fetch_or` | partly checked (Kani, loom, GenMC) |
| `vspec/word.rs.md` | `WordPerm` | partly checked (loom) |
| `vspec/word.rs.md` | `fetch_or` | partly checked (Kani, loom, GenMC) |
| `vspec/word.rs.md` | `store` | partly checked (Kani, loom, GenMC) |
| `vspec/word.rs.md` | `load` | partly checked (Kani, loom) |
| `vspec/word.rs.md` | `fetch_and` | partly checked (Kani, loom, GenMC) |

The arguments, by group (the overrides comments give each in full):

- `init_slow` (P4 models): it terminates given `init` returns, a fair
  scheduler and visible stores. `claim_init`'s exchange on `STATE.word`
  (`vspec::count::compare_exchange`) reads the last value in its
  modification order, for every ordering, so the permission carries it;
  at most one success follows from `STATE`'s invariant, which `STATE`'s
  verified initialiser builds once (`axiom_start`, assumed, is the token
  it takes). The loads and stores on `STATE.word` and `STATE.line`
  (`vspec/count.rs`, `vspec/byte.rs`) give the release/acquire pairs the
  init and once-cell models check on copies of the steps
  (`count.rs.overrides`, `byte.rs.overrides`).
- `background::main` (exit models): it returns after a final
  `request(false)`, given each pass returns and the exit takes the state
  lock.
- `LockGuard::drop` (P2 models): its body is `RawLock::release`.
- `axiom_acquired` (P2 models): the permission passes from holder to
  holder across `RawLock`'s mutual exclusion and its acquire/release
  edge, which the models check; the permission itself is Verus's model,
  its invariant and frame rest on the holders' releases, and at the first
  acquisition the initial value's frame is checked by nothing and its
  visibility rests on the lock's publication.
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
(the models run copies, and the wrappers' own bodies only within
loom's part of C11; the GenMC cases run them only in their
configurations and under the models each names), configurations larger
than the models' and the cases', the kernel's futex (replaced by the
stand-in), and the C11 arguments, which `access.rs` backs by matching
source text only and which no GenMC case checks, since the cases erase
the permissions. Remaining weak-memory
work is tracked in
[wolfi-dev/challoc#397](https://github.com/wolfi-dev/challoc/issues/397).
