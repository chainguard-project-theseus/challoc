# Weak-memory checking: scope and plan

*The weak-memory scope, plan and loom results of the Verus effort, written for the version of the proofs in wolfi-dev/challoc#138 (the tracking PR of the series) and refreshed against main `563886e6`: `file:line`s point at main, and notes marked "On main" say where main differs.*

The Verus proofs assume sequential consistency: vstd's `std` atomic
specifications return any value for a load and ignore the ordering, and
the bitmap word wrappers (`vspec/word.rs:57`, `:75`, `:113`) are single
atomic steps whose specification drops the `Ordering` argument. Nothing
checks that the `Relaxed`, `Acquire` and `Release` orderings are strong
enough. This file says which protocols need that check, what loom and
GenMC would each need, and a plan. Read at the commit that verifies
`Tcaches::create`, rebased onto upstream `c2962c8`; line numbers are
main's at `563886e6`; the counts in "Inventory" are from that commit,
not recounted on main.

Marked **unconfirmed**: facts read from the code or tool documentation
and not run.

## Inventory

226 non-test lines carry an `Ordering::` (`grep`, excluding `tests.rs`,
`linux_tests.rs`, inline `#[cfg(test)]` code and `src/model/`). About
three quarters are `Relaxed` statistics, configuration and heuristics
(`thp.rs` 43, `purge.rs` 30, `policy.rs` 24, `sys/cgroup.rs` 24). The
protocols below cover about 40 sites. There is no `fence` or
`compiler_fence` call; `sys/raw.rs`'s syscall asm has no `nomem`, so
each syscall is a compiler barrier.

| File | Sites | Atomics |
|---|---|---|
| `thp.rs` | 43 | init-time config and counters |
| `purge.rs` | 30 | flags, counters, `Decayer` (`purge.rs:1699`, `:1701`) |
| `policy.rs`, `sys/cgroup.rs` | 24, 24 | sampled policy, all `Relaxed` |
| `arena.rs` | 21 | registry `AtomicPtr`s, count, stats |
| `slab.rs` | 16 | `SlabDesc`: state, arena, `live`, dirty |
| `class/mod.rs` | 15 | class counters |
| `class/table.rs` | 11 | watermark; bitmap and owner words (since upstream #87 also the watermark copy's two stores, through `vspec::atom::store_usize_at`, not in the count) |
| `api/mod.rs` | 9 | `STATE`, `INIT_TID`, `SEALED` |
| `purge/background.rs` | 9 | `PRESENT`, `REQUESTED`, `SEQ`; since upstream #111 also (not in the count) the lazy start's `AUTO`, `AUTO_FAILS`, `AUTO_NEXT`, `AUTO_SINCE`, `AUTO_BASE`, all `Relaxed` (their decisions are rate limits and heuristics; the request they set or withdraw is written under the state lock, as `request`'s) |
| `bitmap.rs` | 8 | `&[AtomicU64]` words |
| `heap/tcaches.rs` | 5 | `[AtomicPtr<u8>; 1024]` |
| `lock.rs` | 5 | the lock word |
| `huge/mod.rs` | 4 | `retain_max`, `purge_on_free` |
| `heap/shared.rs` | 2 | `free_flags` (until upstream #78, `multi_arena`); since upstream #87 the class lines' watermark copies, read through `class::load_mirror` (not in the count) |

## Protocols that matter

### P1. Slab bitmap claim and release

- Claim: `AtomicBitmap::claim_word` reads the word `Relaxed` as a hint
  (`bitmap.rs:1024`), then `fetch_or(Acquire)` (`:1068`). Release:
  `fetch_and(Release)` (`bitmap.rs:1276`), then `SlabDesc::sub_live`,
  `fetch_sub(AcqRel)` (`slab.rs:489`).
- Edge: the releaser's `fetch_and` synchronises with the next claimer's
  `fetch_or`, so the previous owner's writes to the slot happen before
  the next owner's (comment at `bitmap.rs:1063`). Intermediate claims on
  the word are RMWs, so the release sequence continues through them.
- Reservation: under the class lock `take` reads `live` `Acquire`
  (`slab.rs:474`) and adds with `AcqRel` (`slab.rs:480`); releasers clear
  bits before they subtract, so a reader of the lower `live` sees the
  cleared bits (`slab.rs:466`).
- Init: `AtomicBitmap::init` writes `Relaxed` (a store at `bitmap.rs:785`,
  `fetch_and` and `fetch_or` at `:853`, `:881`) under the commit
  lock, published by `committed_slabs.store(Release)`
  (`class/table.rs:4139`); owner words likewise (`class/table.rs:4095`).
- Fresh slab commit: since upstream #114 `SlabTable::commit_run` makes
  a run of the arena's own group read-write (one `mprotect`, and
  `install_guard` for a guard slab) without the commit lock, which it
  takes only for the guard count. Nothing is published through memory:
  the syscalls touch no atomic or lock value, the run is at or past the
  arena's cursor in a group no other arena owns, the caller holds the
  arena's class lock (which `fork_prepare` also takes), and a slab's
  memory reaches another thread only through the claim edges above. The
  kernel orders the protection changes (`mmap_lock`); that is outside
  the memory model, so no loom model is added.
- What to check: no slot is held by two threads; slot data is
  race-free across a release and the next claim; a claimer that
  reserved a slot (`live`) finds a clear bit. A stale `Relaxed` hint
  costs passes; `MAX_EMPTY_PASSES` (0x40_0000, `class/mod.rs:1022`)
  empty passes in a row reach the `reservation_unmet` abort (`:3358`). A
  bounded checker reaches that only with the constant lowered for the
  harness.

### P2. `Lock`

- `RawLock` (`lock.rs`) is a futex mutex over one `u32` (Drepper's
  "mutex 3"): `compare_exchange(0, 1, Acquire, Relaxed)` (`lock.rs:80`),
  a `Relaxed` spin of up to 100 rounds (`:118`), `swap(2, Acquire)` and
  `FUTEX_WAIT` (`:125`, `:130`, `:517`), unlock `swap(0, Release)`
  (`:146`) and `FUTEX_WAKE` if the old value was 2 (`:147`, `:527`).
- Every lock value (class `Inner`, the commit lock's `TableInner`, sets,
  `meta::Inner`, `huge::Inner`, arena registry, stats, `Tcaches`,
  background `STATE`) is plain data under the lock, with no atomics.
  Checking the lock covers them. (`Huge` itself keeps `retain_max`,
  `purge_on_free` and a `Decayer` outside the lock: out of scope.)
- What to check: mutual exclusion, acquire/release of the protected
  data, no lost wake-up between the `swap(2)` and `FUTEX_WAIT`.

### P3. Background thread wake-up

- `SEQ` is the futex word: `kick` does `fetch_add(SeqCst)` then
  `futex_wake` (`purge/background.rs:236`, `:238`); the thread loads
  `SEQ` `SeqCst` before its pass (`:778`) and waits on that value
  (`:815`). `PRESENT` and `REQUESTED` are `Relaxed` (`:160`, `:165`,
  `:173`, `:293`, `:789`, and `:942`, `:943` in `fork_child`).
- `wake` kicks only if `present()` (`purge/background.rs:227`);
  `note_empty` calls it on a class's first `Empty` slab
  (`purge.rs:2424`), and `Decayer::note_first_empty` (`purge.rs:1746`)
  reports only the first.
- Lost wake-up, possible even under SC (found by reading; reproduced
  by loom, "loom results" below): the new thread's first pass reads a class's
  `oldest_empty` as 0 and computes no deadline; a free then empties the
  class's first slab, `wake` sees `PRESENT` still false (the starter has
  not reached `purge/background.rs:293`) and does not kick; the thread
  sleeps with no timeout (`:778`-`:815`). From then on `on_tick` skips
  foreground decay because `present()` is true (`purge.rs:2379`), and the
  class's later empties do not wake it because `oldest_empty` is set.
  The class's `Empty` slabs stay resident until another wake.
  Fixed upstream by chainguard-dev/challoc PR #72 (in the base,
  `c2962c8`): once it has stored `PRESENT`, `start` kicks the thread
  (`purge/background.rs:306`), which makes it scan again.
- Under weak memory the window does not close by reordering the
  existing steps: the free stores `oldest_empty` and loads `PRESENT`,
  `start` stores `PRESENT`, the thread loads `oldest_empty`, all
  `Relaxed`. That is the store-buffering shape: both loads may read the
  old values unless the four accesses are `SeqCst` or fenced (loom
  reproduced it with PR #72's kick; "loom results" below). Fixed by
  upstream #124: a `fence(SeqCst)` on each side (`wake`, `start`).

### P4. Publishing `Shared`

- `init_slow` (`api/mod.rs:1259`):
  `compare_exchange(UNINIT, INITIALISING, Acquire, Acquire)`, `init()`,
  `STATE.store(READY, Release)`; readers `load(Acquire)` (`:612`,
  `:636`), waiters spin `Acquire` (`:1286`). Published by it: `SHARED`,
  `FEATURES`, `FEATURE_LINE` (`UnsafeCell`s, `api/mod.rs:507`-`:511`), the class
  table's page geometry (`layout::adopt_page_geometry`), and the
  init-time `Relaxed` configuration stores (thp, purge, background,
  `SEALED`).
- The fast paths skip the `READY` load (`shared_unchecked`; since
  upstream #77 `shared_base` and `shared_deref`;
  `shared_ptr`, `api/mod.rs:883`, `:951`): the thread's own TLS heap
  pointer is the witness, since heaps are created under `shared()`.
  This crosses no thread; the TLS slot is per thread
  (`sys/tls.rs:323`, `:405`).
- This is the premise `STATE`'s invariant (`api::axiom_state`) states
  for the proofs: a thread that loads `READY` gets the permissions
  `init` froze (`api::Published`), so the published value is the one
  `Shared::init` built (`api::shared_ref`); `shared_unchecked` (since
  #77 `shared_deref`) assumed
  it on the fast paths until package P7, which carries the fact as
  `api::ready()` from the load to the thread's heap and TLS slot token
  and reads through the frozen permissions (`api::axiom_published`):
  the premise is now that a `ready()` fact reaches a thread only after
  it loaded `READY` (or published it), or after a thread that did (the
  background thread, through `pthread_create`): the same happens-before.
  Besides `load_state`, the trusted `TlsSlot::entry`, `heap_at`
  and `shared_arg` assert it (`api::ready`); `mallctl::shared` requires
  it since package RDE, from `mallctl::ctl`'s well-formed `&Shared`.
- What to check: every `READY` reader sees the built value, including
  the page geometry, which is a plain write.

### P5. The free fast path's `Relaxed` reads

- The free path reads `multi_arena` (`heap/shared.rs:3764`; since
  upstream #78 the `FREE_ROUTE` bit of the `free_flags` byte, read after
  the bit test, `Shared::free_flags`), the
  watermark (`class/table.rs:2958`, `:3016`; since upstream #87 its copy
  in the class line, `heap::ClassLine`, which `take_group` stores right
  after the watermark), the bitmap bit (`:3113`)
  and the owner word (`:3305`) `Relaxed`, with no acquire. The argument
  (`class/table.rs:2997`-`:3009`, `heap/shared.rs:101`) is that the
  program frees after it allocates, so happens-before runs from the
  allocation.
- A pointer handed between threads through a non-synchronising channel
  (a `Relaxed` atomic) breaks that chain. A stale `multi_arena == false`
  (now a stale byte without `FREE_ROUTE`)
  skips the arena check and the slot goes to the wrong arena's cache; a
  stale bit weakens only the double-free check.
- What to check: with a synchronising hand-off, a free never aborts
  spuriously and never misroutes; with a `Relaxed` hand-off, which
  outcomes appear (to document, not to fix).

### P6. Explicit caches and the arena registry

- `Tcaches::create` stores `Release` under the table lock
  (`heap/tcaches.rs:272`); `get` loads `Acquire` without it (`:298`);
  `destroy` swaps `AcqRel` (`:386`), then frees. Concurrent use of one
  explicit cache, or use after `destroy`, is the caller's bug, as in
  jemalloc.
- Arena entries: `Release` stores (`arena.rs:2382`, `:3689`), `Acquire`
  loads (`:2869`, `:2906`, `:2987`, `:3449`, `:3515`); `count` likewise (`:3752`,
  `:3232`).
- Thread heaps are touched only by their own thread; no code walks
  another thread's heap.

### Out of scope

Statistics and heuristics whose stale values change only reports or
timing: class counters (`class/mod.rs:4129`, `:5331`, `:5490`-`:5576`),
`Arena::large`, `Decayer::next_walk`, `policy.rs`, `sys/cgroup.rs`,
`thp.rs` (written at init). `LAZY_EVER` (`purge.rs:1473`) is global and written
under whichever class or huge lock flushes the batch; different locks
do not order it. The read that matters is `take`'s, of a slab whose
last discard was flushed under that slab's own lock (`class/mod.rs:4139`),
so that lock orders the one store it must see; covered by P2.

## What each tool needs

### loom

loom runs a test body under every interleaving it can reach and models
part of C11 (stale `Relaxed`/`Acquire` loads within a bounded store
history; **unconfirmed**: no load buffering). Its `UnsafeCell` reports
data races, which is the check P1 needs for slot data.

- Shim: every file imports `core::sync::atomic` directly (16 non-test
  lines in 16 files,
  e.g. `lock.rs:20`, `bitmap.rs:21`, `vspec/word.rs:30`). A
  `crate::sync::atomic` module re-exporting `core`'s, or `loom`'s under
  `cfg(loom)`, plus `cfg(loom)` in `Cargo.toml`'s `check-cfg` list and a
  `cfg(loom)` dev-dependency. A re-export is the same type, so vstd's
  specifications and the release machine code are unchanged (done; the
  x86_64 disassembly is identical, "loom results" below).
  On main there is no shim: `src/sync.rs` is gone, the crate is not
  built under `cfg(loom)`, and the files import `core::sync::atomic`
  directly.
- Futex: loom has no futex. `sys::futex_wait`/`futex_wake`
  (`sys/mod.rs:884`, `:913`) need a `cfg(loom)` stand-in, for example a
  per-address `loom::sync::Condvar` (done: `tests/loom/src/futex.rs`, one
  condition variable for a model's one futex word, waking one waiter).
- What loom cannot take as is: atomics made from raw metadata memory
  (`class/table.rs:3180`, `:3318`, `:4299`, `:4333` cast addresses to
  `&AtomicU64`/`&AtomicU16`); `static` atomics built in `const` context
  (`STATE`, `SEQ`, `PRESENT`, the `thp`/`purge` statics), since loom's
  constructors are not `const` and its state is per test; `no_std` (the
  crate already links `std` under `cfg(test)`). So: real code for the
  struct-based protocols (P1 through `AtomicBitmap::from_words` over a
  slice of loom atomics, P2 through `RawLock`), small copies for the
  static-based ones (P3, P4; P3's copy is upstream's `tests/loom` crate now).
  On main every model is a copy in `tests/loom` (its own crate),
  P1 and P2 included; `tests/loom/tests/pins.rs` checks that the source
  still has the copied lines.

### GenMC

GenMC checks C and C++ programs under RC11 (and IMM) by exhaustive,
stateless exploration with no bound on store history, so it finds the
weak behaviours loom's bound can miss. It reads C through its own clang
front end. Feeding it rustc's LLVM IR would need matching LLVM versions
and a way to map Rust's atomics and the futex syscall onto GenMC's
primitives (**unconfirmed**; not tried). The practical route is C models
of P1, P2 and P3 written to mirror the Rust line by line, with the Rust
line numbers in comments, kept in step by review.

### The existing model

`src/model/` is a sequentially consistent interleaving explorer of the
slab protocol (`model/mod.rs:1`-`:59`): one class, 2 slabs of 4 slots,
1-2 arenas, 2-3 threads; Refill, Write, Release and Purge split into
`class/`'s atomic steps; depth-first search with memoisation
(`model/steps.rs:787`); invariants for no double hold, held bit set,
the calloc zero claim, `live` bounds and set membership
(`model/steps.rs:623`-`:784`). It has no orderings or stale reads, no
lock or futex internals, no publication and nothing of the background
thread. Its invariants are the properties P1's loom and GenMC checks
should assert.

## loom results (plan steps 0-4)

Run with loom 0.7.2 and the DPOR fix below: `sh tests/loom/run.sh`
(about 2 minutes of model checking after the build). The tests are in
`tests/loom/src/`; the module comment there says how the build is
cut down.

On main the models are in `tests/loom`, its own crate, and every one
of them is a copy of the code it checks (`tests/loom/src/lib.rs`), with
`tests/loom/tests/pins.rs` checking that the source still has the
copied lines, in order. Where a bullet below says a model runs the real
code (P1, P2, the `vspec` wrappers), that was true in #138, which
built the crate under `cfg(loom)`; on main it runs a copy.

- loom's DPOR is incomplete, and the script patches it
  (`tests/loom/loom-0.7.2-dpor.patch`). Stock loom 0.7.2 races an
  atomic store or read-modify-write only against the atomic's last
  access; when that is the storing thread's own load, an earlier load
  by another thread is never raced, so the store never runs ahead of
  it. Found here when the `READY` publication model with a `Relaxed`
  publish (a planted race) passed after one execution; the same defect
  is fixed in ToyOSOrg/loom's `toyos` branch (ToyOSOrg/ToyOS PR #567).
  With the patch a store or rmw is also raced against each thread's
  last load of the atomic; nothing else changes, so it only adds
  backtracking points. `tests/loom/src/dpor.rs` holds two controls
  that fail without it. Every model and control other than those two
  and the `Relaxed`-publish control of P4 (which passed under stock
  loom) passes or fails as it did under stock loom; the executions
  explored grew where the defect pruned them (the initialisation
  models from 1, 522 and 1472 to 10, 29839 and 213383, the two-claimer
  bitmap model from 1501 to 2461) and stayed the same for the other
  models counted (all but the three-thread lock model and the controls).
- The futex stand-in's mutex orders a waker's earlier accesses before
  the woken thread's return. The code re-checks the word after every
  wait with its own acquiring access and does not need that edge, but
  on a path through a wake the models cannot show it. Linux's futex
  has such an edge in practice and documents no memory ordering.

- Shim: `src/sync.rs` re-exports `core::sync::atomic` and
  `core::hint::spin_loop`, or loom's under `cfg(loom)`; the 16 imports
  and `lock.rs`'s spin go through it. The release `libchalloc.so`
  (x86_64) disassembles identically to the tree before it, and Verus
  verifies all three configurations unchanged. Under `cfg(loom)`,
  `lib.rs` compiles only `bitmap`, `lock`, `sync` and `vspec` (less
  `vspec/print.rs`): every other module builds its statics in `const`
  context. `RawLock::new` has a non-`const` twin there, `Lock<T>` (its
  `const fn new`) and `vspec::count::new` (a `const fn`; the models
  make their counters directly) are left out, and `lock.rs` sleeps on
  the stand-in `tests/loom/src/futex.rs`: one loom mutex and condition
  variable for the model's one futex word (checked), the compare under
  the mutex as the kernel's is under the hash-bucket lock, and a wake
  of one waiter (`notify_one`, as `FUTEX_WAKE` with `n == 1`; waking
  all would hide a lost wake-up). `SPINS` is 2 under loom.
  On main this shim and the `cfg(loom)` build are gone (see above).
- Bounds: every model is exhaustive (no preemption bound) except the
  three-thread lock model and its control (the dropped mark), bounded
  at 5 preemptions (unbounded did not finish in 25 minutes).
- P2 (`RawLock`, real code): two and three lockers bump a loom
  `UnsafeCell` counter. No race (mutual exclusion and the
  acquire/release edge), the count is exact, and no deadlock (no lost
  wake-up between the `swap(2)` and the sleep, nor for a sleeper
  behind another waiter). Two controls show the deadlock check can
  fail: a mutant
  whose woken waiter retakes the lock by CAS to `LOCKED`, dropping a
  sleeper's `CONTENDED` mark, deadlocks
  (`loom_lock_dropped_mark_loses_a_wake_up`), and a bare wait on the
  stand-in deadlocks (`loom_futex_stand_in_sleeps`).
- P1 (bitmap, real `AtomicBitmap` code over one loom word; the class
  steps around it are copies citing `slab.rs` and `class/mod.rs`): two
  owners release while a third thread reserves through `live` under the
  real `RawLock` and claims; one owner releases two slots to two
  claimers; `init` published by `Release`/`Acquire`. Slot data is a
  loom `UnsafeCell` per slot. No race between a release and the next
  owner, disjoint masks, no padding claimed, no double release, and the
  `live` reservation is met on the first pass (one pass more per
  competing claimer): the `Relaxed` hint load always sees a bit that
  `live` promised, because the `Acquire` load of `live` reads the
  releaser's `AcqRel` `fetch_sub`, which follows its `Release`
  `fetch_and`. A control (`loom_bitmap_unclaimed_write_is_a_race`)
  shows the slot-data check has teeth. Not modelled: sub-word views,
  more than one word, purge, the claim's `xorshift` start word.
- P3 (copy of `purge/background.rs`, `purge.rs`): both lost wake-ups
  reproduced, as a loom deadlock with the thread asleep without a
  deadline, in a copy this effort kept in `src/loom_tests/background.rs`
  (removed with the upstream fix, whose model replaces it):
  - The startup race: the tree before PR #72 (`start` did not kick).
    Reachable under SC; fixed by PR #72's kick after `start` stores
    `PRESENT`.
  - Store buffering: with PR #72's kick and the `Relaxed` orderings,
    the free stores `oldest_empty` and reads `PRESENT == false`;
    `start` stores `PRESENT` and kicks; the thread, woken by that kick
    (it synchronises with it through `SEQ`), still reads
    `oldest_empty == 0`. Not reachable under SC; a real ordering bug of
    the PR #72 code.
  - The fix, upstream #124 (in this tree): a `fence(SeqCst)` in `wake`
    before the `present()` load (after the caller's `oldest_empty`
    store) and one in `start` between `PRESENT.store` and `kick()`.
    Under C++20 (P0668) the two fences are totally ordered: if
    `start`'s comes first, the free reads `PRESENT == true` and kicks;
    if the free's comes first, the thread reads the free's store,
    since `start`'s fence happens before the thread's load (through the
    kick and `SEQ`) and a load reading 0 would put `start`'s fence
    before the free's. loom's `SeqCst` fence is stronger than C++20's
    (it joins the fenced threads' views into one clock), so a pass
    rests on that argument; the failures are behaviours C++20 allows.
    The model is upstream's `tests/loom/src/background.rs` (its own
    crate, stock loom 0.7.2, run by CI): `loom_background_wake` (both
    fences) passes, and without the fences or with either one alone
    the deadlock is reached (`should_panic` tests);
    `tests/loom/tests/pins.rs` checks that the source still has the
    copied lines, in order. Both fences are off the fast paths: `wake`
    runs on a class's first `Empty` slab and on a decay-period change,
    `start` once per thread start. Making the four accesses `SeqCst`
    instead is also correct under C++20, but loom regards `SeqCst`
    accesses as `AcqRel` and reports a false deadlock (loom's README,
    "Unsupported features"), so only the fenced form is checked.
  - Hardware: the model leaves out the class lock the free releases
    before `wake` and the state lock `start` stores `PRESENT` under. A
    `Release` swap does not keep a later load behind an earlier store,
    so in C++20 they change nothing; on x86_64 the swap is an `xchg`, a
    full barrier, so the store-buffering lost wake-up needs aarch64 (or
    a compiler that moves the accesses) to show on hardware. The
    startup race needs neither.
  Verus calls `fence` through `vspec::misc`'s specification of it
  (panics on `Relaxed` only, no effect Verus models), so `wake` and
  `start` stay verified.
- P4 (copy of `api/mod.rs`'s `init_slow`, `shared`, `shared_if_ready`;
  `tests/loom/src/init.rs`): two callers and a free, and a re-entrant
  `init`. `init` runs once, every caller and the free read the value
  `init` wrote with no race, no waiter is left spinning, re-entry
  aborts and no other thread takes that abort. Controls: a `Relaxed`
  `READY` store races; a waiter on an unpublished `INITIALISING`
  fails. loom cannot explore two waiters spinning at once (they yield
  to each other with no bound), so the models have one.
- P5 (`tests/loom/src/free.rs`: the real `AtomicBitmap` and
  `vspec::word::load` over one loom word; copies of `take_group`'s tail,
  a claimer's `Acquire` watermark load and the free path's checks from
  `checked_slot`/`set_slot`, since upstream #87 through the class line):
  an allocating thread hands its slot to
  the freeing thread while a third claims and releases the word's
  other slot. With a `Release`/`Acquire` hand-off (31214 executions since the port of upstream #87, 13337 before)
  every free sees the slab handed out, the bit set and the owner
  stored, with no race on the block. Since upstream #87 the fast
  path's load is of the class line's watermark copy, stored after the
  watermark, so a synchronised free can also read the copy behind
  (`Lagged`): a fast-path miss whose slow path re-reads the table's
  watermark and finds the slab. With a `Relaxed` hand-off (50926, 17501 before upstream #87;
  a data race on the block, so outside the C contract) all four
  outcomes appear: the checks pass, the watermark read below the slab,
  the bit read clear, and the owner word read as its initial 0. Each
  of the last three is a fast-path miss; the slow path re-reads, and
  only a stale re-read too makes a spurious abort or releases the slot
  to arena 0 (possible, not modelled). Controls: the `Relaxed`
  hand-off with the free touching the block races; with the claimer's
  watermark load `Relaxed`, a synchronised free can read the owner as 0
  (91770; 38029 before upstream #87). `multi_arena` (now `free_flags`' `FREE_ROUTE` bit) is not
  modelled.
- Arena bound counts (`tests/loom/src/count.rs`: the real `vspec/count.rs`
  wrappers, `Relaxed` as `arena.rs`; copies of `assign` under the real
  `RawLock`, `count_in`, `unbind`, `load_bound`): 2 arenas, the main
  thread homed on arena 0 and binding and unbinding a heap on arena 1,
  two threads assigned and unbound (10872962 executions, about 6
  minutes): no `unbind` reads 0, no holder reads its arena's count as
  0, and the counts end at the tickets out. Control: a `fetch_add`
  split into a load and a store loses a ticket.
- Arena count (`tests/loom/src/narenas.rs`: the real `vspec/count.rs`
  `store` and `load`; copies of `create` under the real `RawLock`,
  `count`, `iter`): two creators and a scanner, `MAX_ARENAS` 3 (4482
  executions): the count stays in range and never goes back for a
  reader, a scan finds every explicit arena below the count it read
  built and reads it race-free, a `create` under the lock reads the
  count as the lock's value with every arena below it built, and the
  count ends at the lock's value. Control: a `Relaxed` count store lets
  a scan see a count before its arena.
- Policy position (`Policy::scaled16`, one more than the position;
  `tests/loom/src/position.rs`: the real `swap` and `load`; copies of
  `apply`'s swap and `position`): two concurrent appliers and a reader
  (940): every load is a stored value, at most `FRAC_ONE + 1`, and the
  swaps' results and the final value form one chain. Control: a swap
  split into a load and a store breaks the chain.
- Class table watermark (`tests/loom/src/watermark.rs`: the real
  `vspec/atom.rs` `store_usize` and `load_usize_in` (main: `load_usize`); copies of
  `take_group` under the real `RawLock` and `watermark`): two hand-outs
  and a reader (4482): every load is at most the slab count and never
  goes back, `take_group`'s `check_zero` comparison under the lock
  always holds, and an `Acquire` load sees the owner of every group
  below it. Control: a `Relaxed` watermark store does not publish the
  owner. P5's and P1's publication models (`free.rs`,
  `loom_bitmap_init_is_published`) now go through the same wrappers;
  their execution counts are unchanged.
- Slab descriptors' lock-protected fields (`tests/loom/src/slab.rs`: a
  copy of the six `DescPerm` accessors, under two real `RawLock`s for
  the commit lock and a class lock, the permission a loom `UnsafeCell`
  in the lock value holding it): one thread hands a slab out
  (`take_group`'s `init` and the move between the locks, then
  `set_state(Active)`), another reads the three fields through the
  permission under the class lock and writes `Empty` and a stamp (36
  executions): every read returns the last write through the
  permission, with no race on the permission. Control: a read outside
  the class lock, after a `Relaxed` flag, is stale.
- The background thread's exit (copy of `background::main`'s loop and
  `request(false)`; `tests/loom/src/exit.rs`): the thread returns after a
  withdrawal, also with another kick in between. Controls: a withdrawal
  without its kick, or a `Relaxed` `SEQ` load in the thread, leaves it
  asleep. The exit does not depend on P3: `request(false)` kicks
  unconditionally.
- The background thread's publication against its exit (upstream #128;
  copy of `start`'s two state lock holds and kick, `main`'s exit and
  `request(false)`; `tests/loom/src/publish.rs`): after a final withdrawal
  the thread has exited and neither the state nor `PRESENT` says it is
  present, also when it exits between `start`'s two holds (503448
  executions). Control: a
  publication of `thread.is_some()` alone (the code before #128) leaves
  the exited thread published. The flags are under the state lock, so
  this is about the interleaving, not the orderings; Verus proves the
  lock-value side (`exited ==> !present` in `axiom_state_lock_inv`).
- The once-cells (ONCECELL; `tests/loom/src/oncecell.rs`). The feature
  line, a copy of `line_ready` and its `LINE_STATE` wrappers, the build
  a write to each of two cells: two requests and a reader (5530
  executions): the line is built once and every reader that sees
  `LINE_READY` reads it race-free. The deferred cgroup record, the
  real `vspec/byte.rs` wrappers driven by copies of `Deferred::get`,
  `discover` and `fork_child`: a tick, a waiting reader and a sampler
  (933): one discovery runs and every reader that sees `DONE` reads the
  record race-free. Controls: a `Relaxed` `LINE_READY` (`DONE`) store
  lets a reader see it without the write. The fork-child resets run on
  one thread, as the child has: the next request builds (discovers)
  rather than waits. Verus proves the hand-off of the cells'
  permissions (`api::LinePred`, `sys::cgroup::PhasePred`); the models
  check that the release/acquire pairs give the edges it relies on.

## Plan

| Step | Work | Days | Status |
|---|---|---|---|
| 0 | `crate::sync::atomic` shim, 16 imports, check-cfg; disasm | 0.5 | done (gone on main: the models are copies) |
| 1 | loom: `RawLock` with a futex stand-in (P2) | 1-2 | done |
| 2 | loom: bitmap claim/release, `live`, slot data cells (P1) | 2-3 | done |
| 3 | loom: background wake-up copy; try the P3 race | 1 | done |
| 4 | loom: `READY` publication copy (P4); free hand-off (P5) | 1-2 | done |
| 5 | GenMC: C models of P1-P3 under RC11 | 3-4 | open |
| 6 | CI: `sh tests/loom/run.sh` | 0.5 | open (on main: done, the `lint` check runs it, `.chainguard/ci/lint.yaml`) |

About 9-13 days. Steps 1-3 carry most of the value: P1 is the one
protocol whose orderings guard user data, P2 guards every lock value,
and P3 has a candidate bug to confirm or refute. Step 0 touches 16
files outside `verus/` and needs the codegen check; steps 1-4 add test
code only. The P3 races are confirmed ("loom results"). PR #72 (in the
base) added the kick after `start` stores `PRESENT`, which closes the
startup race; the rest of the fix, a `fence(SeqCst)` between that store
and the kick and one in `wake`, is upstream #124 (in this tree), with
its own loom model in `tests/loom` (the four accesses made `SeqCst` also
work, but loom cannot check that form).

What stays assumed after the plan: the Verus proofs are still SC
proofs. The checks above show that the shipped orderings give the
happens-before edges the SC proofs rely on, for small configurations;
they do not lift the proofs to weak memory.

## Seams

On main every seam below that this file discharges with loom, or
records as evidence, is `trusted`: main's `verus/seams.sh` accepts only Kani, const-assert and Isla
citations, the `verus/seams/` records are gone, and
`tests/loom/run.sh` does not make `verus/loom/run.sh`'s source-wide checks
(writes only through the wrappers, one permission minted per atomic).
The arguments below still back those seams; "Status on main" at the end
lists them. Returning them to `discharged` is open work in
wolfi-dev/challoc#397. The `run.sh` this section names is
#138's `verus/loom/run.sh`. The `S<n>` are each pass's own ledger numbers;
main's ledger names a seam by its item.

The 25 seams `verus/SEAMS.md` gave to loom/GenMC, triaged (records
under `verus/seams/`, models re-run by `sh verus/loom/run.sh`, which
first checks each record against the ledger; seam numbers of commit
8635213, when it was done). No model found an outcome
a contract excludes; P3, the one weak-memory bug found, is fixed (upstream #124), and no seam
below depends on it. GenMC is not installed in the environment this was
done in (no C toolchain to build it), so every result is loom's.

| Seam | Result | Why |
|---|---|---|
| `init_slow` (S27) | discharged | P4 models; terminates given `init` returns, a fair scheduler and visible stores |
| `background::main` (S195) | discharged | exit models; returns after a final `request(false)`, given each pass returns and the exit takes the state lock (`lock_slow`, evidence only) |
| `LockGuard::drop` (S181) | discharged | P2 models run the real `unlock` |
| `fetch_or`, `fetch_and`, `store` (`vspec/word.rs`; S361, S364, S362) | discharged | C11 per-location coherence gives the contract; P1 models check the cross-location edges the SC proof needs |
| `RawLock::lock_slow` (S176) | evidence | P2 models show deadlock freedom; termination does not hold (the mutex is not starvation-free) |
| `SlabDesc::sub_live` (S202) | evidence, then discharged (second pass) | P1 models; that the abort is unreachable is an allocator invariant, which the contract (`requires may_abort()`, since theorem 44 `may_abort_pending()`) no longer claims |
| `load` (`vspec/word.rs`, S363) | trusted, then discharged (second pass) | its caller, the `free` bit test, is P5, which no model reached then |
| `WordPerm` (S360) | trusted, then discharged (second pass) | the type is the SC abstraction itself; its content is the wrappers' contracts and that every write goes through them |
| `ClassState::claim_with` (S74) | trusted, retooled to RefinedRust | termination rests on `live` not rising between `plan`'s and `take`'s reads under the class lock (read-read coherence); a Verus `decreases` with that fact as an allocator-invariant axiom fits better than a bounded checker |
| `SlabTable::watermark`, `Policy::position`, `load_lg_slab_min`, `load_lg_pmd`, `load_lg_meta` (S91, S184, S244-S246) | reassigned: allocator invariant, RefinedRust | every store writes a value in range, and a load of any ordering returns a value some store wrote: no memory-model question; since verified in Verus, no longer seams: the watermark and the position over atomic invariants (`vspec/atom.rs`, `vspec/count.rs`), the three loads with no range claim (their readers shift with `wrapping_shl`) |
| `Arenas::init` (S58) | reassigned: raw pointer (internal), RefinedRust | single-threaded in-place construction; its `Release` store is published again by `READY` |
| `AtomicPtr::{load, store, new, swap}`, `AtomicU32::as_ptr`, `compiler_fence` (S256-S259, S312, S310) | reassigned: core library, Kani | specifications of `core` functions with no value claims: whether they return without a panic |
| `load_bool`, `Explicit::drop` (S261, S186) | reassigned: Kani (`load_bool` removed with upstream #78: no caller left) | a one-operation body that must return; no ordering is relied on |

Contracts the reassignment exposed: `AtomicPtr::load`'s specification
and `load_bool` (since removed) have no `requires` on the ordering, but `core`'s `load`
panics for `Release` and `AcqRel` (the `store` specification has the
matching `requires`). Neither is `no_unwind`, so Verus already allows
the call to unwind and no proof is unsound; but under the Kani records'
policy (a trusted body returns under its `requires` without a panic) a
harness over every ordering would fail until the `requires` is added.
Every caller passes `Relaxed` or `Acquire`. vstd's own `load`
specifications have the same gap. The `compiler_fence` specification
has no caller in `src/`.

### Second pass (2026-10-04)

The 14 seams still with loom/GenMC and no record at commit 0c9cd77,
plus the two earlier evidence records (seam numbers of that commit). GenMC
is still not installed (no C toolchain to build it), so every result is
loom's. No model found an outcome a contract excludes for a program
within the C contract; the free path's outcomes after a `Relaxed`
hand-off (a data race on the block) are P5's documented ones.

| Seam | Result | Why |
|---|---|---|
| `load` (`vspec/word.rs`, S373) | discharged | per-word coherence gives the value; the proof uses it only for "a set bit is not a guard slab", true of every value; P5 models |
| `WordPerm` (S370) | discharged | its four wrappers are discharged; `run.sh` checks by name that non-test `bitmap.rs` writes words only through them and `class/table.rs` writes none |
| `fetch_add`, `fetch_sub` (`vspec/count.rs`, S289, S290) | discharged | RMW atomicity for every ordering; theorem 34's invariants each name one counter and pure ghost tickets; count models |
| `load` (`vspec/count.rs`, S291) | evidence | as `fetch_add`, but the contract does not exclude `Release`/`AcqRel`, with which `core`'s `load` panics (no caller passes them; a `requires` would make it a discharge) |
| `CountPerm` (S288) | discharged | as `WordPerm`, over the count wrappers; `run.sh` checks by name that `arena.rs` touches the counts only through them and `init`'s zeroing |
| `load_owner` (S86) | evidence | write-read coherence gives the stored arena to a load ordered after the store; that each caller is so ordered is the doc comment's premise, not in the contract (a free meets it when it follows its allocation, as C requires; a racy or invalid free can read 0) |
| `store_owner` (S87) | discharged | the store is the word's only write (Verus's markers), so every load ordered after it reads `arena`; excluding the others is the reader's premise |
| `state_claim` (was `claim_init`'s seam, now verified) | discharged | the exchange reads the last value in `STATE`'s modification order, for every ordering, so the permission carries it; at most one success is proved from `STATE`'s invariant, given `axiom_state` denotes one invariant (assumed); `run.sh` checks `STATE` has two writes; `StatePerm`, `state_load`, `state_publish` and `axiom_state` stay trusted |
| `LockGuard::deref`, `deref_mut` (S183, S184) | superseded (now verified over the lock's `PCell`; the mutual exclusion and edge are `axiom_acquired`'s assumption) | was: P2 models (mutual exclusion and the release/acquire edge) and C11's visible side effect; the models use a loom cell beside the real lock word, not `Lock<T>` |
| `SlabDesc::sub_live` (S206) | discharged | the contract now requires `may_abort()` (since theorem 44 `may_abort_pending()`) and claims no value, so the assertion's abort is allowed; one wait-free RMW |
| `RawLock::lock_slow` (S178) | evidence (unchanged) | deadlock freedom only; the mutex is not starvation-free |
| `Arenas::count` (S63) | declined, tool Verus | every store writes at most `MAX_ARENAS` and a load returns a stored value: an allocator invariant, provable as the bound counts are |
| `words_ref` (S93; its trusted part is now `slice_from_perms`, which it calls) | declined, tool none | a raw-pointer slice over permissions; no memory-model content |
| `Lock::get_unlocked` (S182) | superseded (now verified over `axiom_held`) | was: its `ensures` is the lock invariant (every holder restores it), the assumption `axiom_acquired` also makes |

### Third pass (2026-10-05)

The loom/GenMC seams of `vspec/count.rs`, `vspec/atom.rs` and `lock.rs`
still without a discharge at commit 82ca40d7, and the six `SlabDesc`
accessors of the descriptor permissions (`slab::DescPerm`), trusted
since the class lock's ghost took the permissions over (seam numbers of
that commit; `vspec/word.rs` had none left). GenMC is still not
installed, so every result is loom's. No model found an outcome a
contract excludes. The slab model (`tests/loom/src/slab.rs`) is a copy,
since `slab` needs `layout`, which the `cfg(loom)` build leaves out; the
others run the real `vspec` wrappers. The base's `run.sh` no longer
built: `vspec::count::new` had become `const` (for `Policy::new`), and
loom's `AtomicU32::new` is not; `new` is now left out of the
`cfg(loom)` build (the models make their counters directly).

| Seam | Result | Why |
|---|---|---|
| `store` (`vspec/count.rs`, S294) | discharged | one store under the creation lock, which orders the stores; `NarenasPred` bounds one atomic by a constant, so any ordering keeps it; narenas models (the `Release`/`Acquire` pair also publishes the arena, which no verified proof relies on) |
| `swap` (`vspec/count.rs`, S297) | discharged | RMW atomicity for every ordering; `FracPred` bounds one atomic by a constant; position models (concurrent appliers form one chain) |
| `load` (`vspec/count.rs`, S295) | evidence, now discharged | the contract now excludes `Release` and `AcqRel` (ghost only; every caller passes `Relaxed` or `Acquire`); count, narenas and position models |
| `UsizePerm` (S261) | discharged | as `CountPerm`, over the two watermark wrappers; `run.sh` checks by name that nothing writes `committed_slabs` but `store_usize` and that only `class/table.rs` (and the loom models) calls the wrappers |
| `store_usize` (S264) | discharged | one store under the commit lock; `CommittedPred` bounds one atomic by a constant; watermark, free and bitmap models (the publication of the owner word and the bitmap is `load_owner`'s premise, checked, not in the contract) |
| `load_usize_in` (S263) | discharged | per-atomic coherence gives the held permission's value, whose predicate relates it to constants only; the same models, with both orderings of `SlabTable::watermark` |
| `SlabDesc::init`, `state_byte_held`, `dirty_held`, `arena_held`, `set_state`, `set_dirty_at` (S197-S201, S203) | discharged, given the lock edge `axiom_acquired` assumes | every access through a permission is made holding the lock whose value holds it (the commit lock's, then the class lock's), so successive holders are ordered by happens-before and per-location coherence gives the last write through it, for `Relaxed` too; `run.sh` checks by name that `slab.rs` writes the three fields only in the accessors; the model runs the hand-over between the two locks on one thread and checks the class lock's edge to the next holder, and a control shows a read outside the lock is stale |
| `RawLock::lock_slow` (S171) | evidence (unchanged) | deadlock freedom only; the mutex is not starvation-free, so termination is an assumption no bounded model can discharge |

Left in `vspec/count.rs`, `vspec/atom.rs` and `lock.rs`: `new` (S296)
and `new_usize` (S262) were minting seams with tool none (one permission
per atomic was an assumption; both are verified since, with ghost
identities and the permission types' fields private to their modules,
and the `CountPerm` and `UsizePerm` type seams are retired); `Lock::cell_ref`, `Lock::cell_mut` (S176, S177; tool
Verus, kept `external_body` for the machine code of their callers) and
the lock axioms (S172-S175) are not weak-memory questions.
`LockGuard::drop` (S178) is discharged but stays `external_body`:
verifying it in Verus needs `RawLock::release` declared `opens_invariants
none` and `no_unwind`, and so `sys::futex_wake`, `sys::futex` and the
raw syscall below it.

The arena count model also checks, inside `create` under the creation
lock, that the count reads the lock's value and every explicit arena
below it is built: the frozen table that `api::count_held` and
`api::arena_held` now prove for the fork handlers in the SC model
(package P8, through `vspec/entry.rs`, whose wrappers the model
runs); the model covers only
`create`'s reads. Under stock loom (no DPOR patch) the arena count and
watermark models explore 36 executions instead of 4482, and their
`Relaxed`-store controls pass: two more models the patch is needed for.

### The `free` path's dispatch byte (package FFLAGS, 2026-10-07)

`Shared::free_flags` was an `external_body` load whose `ensures` (the
byte is `push_flags` up to `FREE_ROUTE`) was a writers' invariant read
off the code. It is now verified over `vspec/flags.rs`: the byte is a
`RouteByte`, whose `AtomicU8` is private to that module, so its only
operations are `new` (`init`'s `put_flags`), `fetch_or`
(`set_multi_arena`, bits within `ROUTE` only) and `load`, whose spec
names the byte's fixed bits (`flags::fixed`, a ghost function of the
value), which `Shared::wf` ties to `push_flags`. The model
(`tests/loom/src/flags.rs`) runs the real wrappers with copies of `init`'s
two writes and publication, `build`'s `set_multi_arena` and the free
path's load.

| Seam | Result | Why |
|---|---|---|
| `new`, `load`, `fetch_or` (`vspec/flags.rs`, S314-S316) | discharged | per-location coherence: every write is `new`'s or a `fetch_or` within `ROUTE`, so every value in the modification order keeps the made bits, for any ordering, for a reader that happens after `init` (the premise of the trusted seams that hand out a well-formed `&Shared`); the type confines the atomic operations to the wrappers and `run.sh` checks the calls and raw writes by name (heuristic); flags models (478 and 613414 executions) and two controls (a `Relaxed` `READY` store leaves the reader unordered after `init`'s write; a bit outside `ROUTE` fails the assertions) |
### Package OWNER (2026-10-07)

`load_owner`'s premise, that a reader who saw the watermark past a slab
sees its owner word, is now a ghost witness in its contract, so
`load_owner` and `ClassState::owner_locked` verify and the owner words'
memory-model half is two loom-discharged wrappers and the changed
`store_owner`:

| Seam | Result | Why |
|---|---|---|
| `load_owner` | retired (verified) | the watermark's invariant (`table::WatermarkPred`) holds the owner words' witness markers below it, which `load_usize_seen` hands the reader; the word's invariant (`table::OwnerPred`) holds the stored owner once they are persistent |
| `load_usize_seen` (S268) | discharged | per-atomic coherence gives the held permission's value, as `load_usize_in`; the copy of the persistent set is a ghost duplicate; watermark, free and bitmap models (the table's watermark loads) |
| `load_u16_at` (S269) | discharged | per-word coherence: the word's writes are its initial 0 and `store_owner`'s one store; a load ordered after the store reads it. The witness says which loads are so ordered under SC; under C11 an `Acquire` watermark load or the program's hand-off supplies the edge, and a free not ordered after its allocation can read 0 (P5's documented outcome) |
| `store_owner` (S86) | discharged (contract changed) | as before; it now stores through the word's permission inside the word's invariant and records the value (`u16_value`) |
| `load_usize_in` (S264) | discharged (now only the class line's copy) | the table's own watermark is read with `load_usize_seen`; free models |

The models are the second pass's, with the table's watermark loads
through `load_usize_seen` and the owner loads through `load_u16_at`
(`tests/loom/src/watermark.rs`, `free.rs`, `bitmap.rs`).


### Status on main (563886e6)

The seams #138 discharged with loom (or, for `RawLock::lock_slow`,
recorded loom evidence for), from its `verus/seams.overrides`, with
main's ledger entry. All 42 are `trusted` on main: 41 with tool
loom/GenMC, and `purge/background.rs`'s `main`, a `no_decreases`
(termination) entry, with tool none.
#138's `load_usize_in` is main's `load_usize`.

| main `file:line` | seam | in #138 | on main |
|---|---|---|---|
| `api/mod.rs:562` | `state_load` | discharged | trusted |
| `api/mod.rs:578` | `state_claim` | discharged | trusted |
| `api/mod.rs:593` | `state_publish` | discharged | trusted |
| `api/mod.rs:651` | `line_load` | discharged | trusted |
| `api/mod.rs:672` | `line_compare_exchange` | discharged | trusted |
| `api/mod.rs:697` | `line_store` | discharged | trusted |
| `api/mod.rs:1259` | `init_slow` | discharged | trusted |
| `class/table.rs:976` | `store_owner` | discharged | trusted |
| `lock.rs:113` | `RawLock::lock_slow` | evidence | trusted |
| `lock.rs:984` | `LockGuard::drop` | discharged | trusted |
| `purge/background.rs:758` | `main` | discharged | trusted |
| `slab.rs:340` | `SlabDesc::init` | discharged | trusted |
| `slab.rs:382` | `SlabDesc::state_byte_held` | discharged | trusted |
| `slab.rs:400` | `SlabDesc::dirty_held` | discharged | trusted |
| `slab.rs:415` | `SlabDesc::set_state` | discharged | trusted |
| `slab.rs:453` | `SlabDesc::arena_held` | discharged | trusted |
| `slab.rs:488` | `SlabDesc::sub_live` | discharged | trusted |
| `slab.rs:506` | `SlabDesc::set_dirty_at` | discharged | trusted |
| `vspec/atom.rs:109` | `store_usize` | discharged | trusted |
| `vspec/atom.rs:134` | `load_usize` | discharged | trusted |
| `vspec/atom.rs:178` | `store_usize_at` | discharged | trusted |
| `vspec/atom.rs:220` | `load_usize_seen` | discharged | trusted |
| `vspec/atom.rs:260` | `load_u16_at` | discharged | trusted |
| `vspec/byte.rs:92` | `load` | discharged | trusted |
| `vspec/byte.rs:110` | `store` | discharged | trusted |
| `vspec/byte.rs:132` | `compare_exchange` | discharged | trusted |
| `vspec/count.rs:81` | `fetch_add` | discharged | trusted |
| `vspec/count.rs:98` | `fetch_sub` | discharged | trusted |
| `vspec/count.rs:116` | `store` | discharged | trusted |
| `vspec/count.rs:135` | `load` | discharged | trusted |
| `vspec/count.rs:152` | `swap` | discharged | trusted |
| `vspec/count.rs:188` | `compare_exchange` | discharged | trusted |
| `vspec/entry.rs:75` | `store` | discharged | trusted |
| `vspec/entry.rs:94` | `load` | discharged | trusted |
| `vspec/flags.rs:60` | `new` | discharged | trusted |
| `vspec/flags.rs:73` | `load` | discharged | trusted |
| `vspec/flags.rs:89` | `fetch_or` | discharged | trusted |
| `vspec/word.rs:41` | `WordPerm` | discharged | trusted |
| `vspec/word.rs:57` | `fetch_or` | discharged | trusted |
| `vspec/word.rs:75` | `store` | discharged | trusted |
| `vspec/word.rs:96` | `load` | discharged | trusted |
| `vspec/word.rs:113` | `fetch_and` | discharged | trusted |
