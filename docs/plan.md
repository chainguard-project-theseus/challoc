# challoc implementation plan

This plan is written to be executed incrementally by an autonomous loop. Each
phase lists tasks and acceptance criteria. A task is done when its criteria
pass inside the Linux dev container on both aarch64 and x86_64 unless the task
says otherwise. Do not start a phase before the previous phase's acceptance
criteria all pass. Within a phase, tasks are ordered but may be parallelised
when independent.

Ground rules for every phase:

- `cargo build --release` produces `libchalloc.so` and `libchalloc.a` with no
  warnings under `#![deny(warnings)]` and no `std` dependency in the library.
- Every entry point is `pub extern "C"` with `#[unsafe(no_mangle)]`, declared
  at module top level.
- `cargo test --features std` runs the pure-logic unit tests on the host.
- Linux integration tests run under `LD_PRELOAD=target/release/libchalloc.so`
  against small C programs in `tests/c/` compiled with the container's gcc.
- Benchmarks compare against glibc 2.44's malloc, jemalloc 5.3.1 from the
  Wolfi `jemalloc` package, and mimalloc 3.x built from source, all via
  `LD_PRELOAD` in the same container. Numbers are recorded in
  `bench/results/<date>-<arch>.md` with the commit hash.

## Phase 0: skeleton and core allocator

Goal: a correct, fast `malloc`/`free`/`calloc`/`realloc` with thread caches,
bitmap slabs, and huge allocations. No jemalloc API, no decay, no cgroup.

Tasks:

1. Convert the crate to a `no_std` library with `crate-type = ["cdylib", "staticlib"]`,
   `panic = "abort"` in all profiles, `opt-level = 3`, `lto = "fat"`,
   `codegen-units = 1`. Add the `libc` crate with default features off for
   type definitions only, or hand-declare the handful of types needed. Add a
   `std` dev feature for host unit tests. Add `Dockerfile.dev` building a
   Wolfi image with `rust`, `build-base`, `gcc`, `jemalloc`, `git`, `cmake`,
   and a `make dev` target that runs a shell in it with the repo mounted.
2. `src/sys`: raw syscall wrappers via `asm!` for x86_64 and aarch64, or via
   `libc::syscall`, for the list in design.md section 17. Include `abort`
   that writes a message with `write(2)` and calls `SYS_exit_group` after
   raising `SIGABRT`. Unit test error mapping on the host.
3. `src/sys/features.rs`: probe `MADV_GUARD_INSTALL`, `mseal`,
   `process_madvise` with `PIDFD_SELF`, `PR_SET_VMA_ANON_NAME`, page size.
   Integration test prints the probe result and it matches `uname -r`
   expectations for the container kernel.
4. `src/tls.rs`: initial-exec TLS slot via `global_asm!` for both
   architectures. Test: a C program creating 64 threads under `LD_PRELOAD`
   sees distinct heap pointers per thread; `readelf -r` on the `.so` shows
   `R_X86_64_GOTTPOFF` / `R_AARCH64_TLS_TPREL` style relocations and no
   `__tls_get_addr` import.
5. `src/layout.rs`: size class tables for `quantum16` and `quantum8`, region
   probing and reservation, pointer classification, slot arithmetic with the
   fixed-point reciprocal. Exhaustive host unit tests: every size in
   `0..=max_medium` maps to a class whose size is `>= n`, class sizes are
   monotonic, slot arithmetic round-trips for every slot of every class, and
   misaligned pointers are rejected.
6. `src/bitmap.rs`: `AtomicBitmap` claim/release and `TwoLevelBitmap` with
   `lowest_set`, `set`, `clear`. Host unit tests including a randomised
   model check against a `Vec<bool>` under the `std` feature.
7. `src/meta.rs`: metadata region bump allocator with size-classed free lists
   for `Heap`, `ClassCache`, and descriptor arrays; commit on demand.
8. `src/slab.rs`, `src/class.rs`: descriptors, claim/release/check, partial
   and empty sets, lowest-address slab selection, slab commit with guard
   slabs.
9. `src/heap.rs`: `Heap`, `ClassCache`, fast paths, refill and flush,
   thread-exit key with recursion guard.
10. `src/huge.rs`: page bitmap, descriptor hash table, guards, alignment.
11. `src/api/libc.rs`: `malloc`, `free`, `calloc`, `realloc`,
    `aligned_alloc`, `posix_memalign`, `memalign`, `valloc`, `pvalloc`,
    `free_sized`, `free_aligned_sized`, `reallocarray`,
    `malloc_usable_size`. `errno` preserved by `free`. Lazy init state
    machine. Fork handlers.
12. `tests/c/`: programs covering alignment for every power of two up to
    64 KiB, `realloc` growth and shrink across tiers, `calloc` zeroing
    including reused slots, 1 million random alloc/free operations with
    pattern verification across 8 threads, producer/consumer cross-thread
    frees, fork with allocation in both parent and child, and a thread that
    exits with a full cache. Each is also run under `valgrind --tool=none` is
    not required; instead each program checks its own invariants and exits
    non-zero on failure.
13. `fuzz/`: a seeded random driver in C that keeps a table of live
    allocations with fill patterns and performs weighted random operations,
    running for a configurable count; a `make fuzz` target runs it for
    10 million operations with 4 threads.
14. Negative tests: a program that frees a pointer twice must abort with the
    challoc message; freeing a pointer into the middle of a slot must abort;
    freeing a stack pointer must abort; writing past a huge allocation must
    fault.
15. Run the glibc `malloc/tst-malloc.c`, `tst-calloc.c`, `tst-mallocfork.c`,
    `tst-posix_memalign.c`, and `tst-realloc.c` sources under `LD_PRELOAD`;
    they are in `tests/glibc/` with their licence.
16. Run mimalloc's `test-stress.c` with `USE_STD_MALLOC` under `LD_PRELOAD`
    at `THREADS=8 SCALE=50 ITER=10`.

Acceptance:

- All tests and negative tests above pass on both architectures.
- mimalloc-bench `cfrac`, `espresso`, `larson`, `mstress`, `xmalloc-test`,
  `alloc-test`, `glibc-simple`, `redis` run to completion under challoc and
  are within 10% of jemalloc 5.3.1 on time and RSS. Record results. Regressions
  worse than 10% block the phase; improvements are noted but not required yet.
- `perf stat` on `larson` shows no lock or atomic RMW in the top 10 symbols of
  challoc's profile.

## Phase 1: jemalloc compatibility and Valkey

Goal: Valkey links against `libchalloc.so` in place of bundled jemalloc with
active defrag, purge, stats, background thread, and Lua arena all working.

Tasks:

1. `src/config.rs`: `malloc_conf` parser covering design.md section 12;
   weak `malloc_conf` symbol, `/etc/challoc.conf`, environment. Host unit
   tests for every key and for malformed input.
2. `src/purge.rs`: decay with `dirty_decay_ms`, `Empty` to `Purged`
   transitions, batched `process_madvise`, slow-path driver with per-class
   deadlines, `malloc_trim`. Background thread with futex sleep, created only
   on request, absent after fork in the child.
3. `src/arena.rs`: arenas, `arenas.create`, per-arena caches keyed by
   descriptor arena, explicit caches, `MALLOCX_TCACHE_NONE` direct path.
4. `src/stats.rs`: per-thread counters, epoch merge, `allocated`, `active`,
   `resident`, `retained`, `mapped`, `metadata`, per-class `curregs`,
   `curslabs`, `nonfull_slabs`. Host unit tests for the accounting identities
   `resident >= active >= allocated` and `mapped = resident + retained`.
5. `src/api/jemalloc.rs`: `*allocx`, `nallocx`, `sallocx`, `je_` aliases,
   `malloc_message`. `src/api/mallctl.rs`: the full key list in design.md
   section 13.4 including `experimental.utilization.batch_query`.
   `src/api/print.rs`: `malloc_stats_print`, `malloc_info`, `malloc_stats`.
   `src/api/glibc.rs`: `mallopt`, `mallinfo`, `mallinfo2`.
6. `jemalloc-soname` Cargo feature and a `make jemalloc-compat` target that
   produces `libjemalloc.so.2` plus a `jemalloc.pc` pointing at it.
7. Valkey integration: a patch in `contrib/valkey/` modelled on Debian's
   `USE_SYSTEM_JEMALLOC` patch that links `-lchalloc`, keeps
   `USE_JEMALLOC` and `HAVE_DEFRAG` defined, and maps `je_*` to our exports.
   A script `contrib/valkey/build.sh` clones the tag Wolfi ships, applies the
   patch, builds with `quantum8`, and runs `./runtest --single unit/memefficiency`
   and the full `./runtest` suite.
8. A soak script `bench/valkey-soak.sh` that runs `valkey-benchmark` and
   `memtier_benchmark` write-heavy then delete-heavy phases for 20 minutes,
   sampling `INFO memory` every 10 s into a CSV: `used_memory`,
   `used_memory_rss`, `mem_fragmentation_ratio`, `allocator_frag_ratio`,
   `allocator_rss_ratio`, `active_defrag_hits`, and p99 latency.

Acceptance:

- Valkey test suite passes with challoc, including defrag tests, on both
  architectures.
- `MEMORY PURGE`, `MEMORY MALLOC-STATS`, `CONFIG SET activedefrag yes`, and
  `jemalloc-bg-thread yes` all work; `INFO memory` reports a real
  `allocator_frag_ratio`.
- Soak: throughput and p99 at least equal to bundled jemalloc 5.3.0, peak RSS
  no more than 5% above it, and post-delete RSS returns to within 10% of
  jemalloc's after the decay window.
- Ruby 3.4 built `--with-jemalloc` against `libjemalloc.so.2` from challoc
  passes `make test-all` in the container, or the failures are shown to be
  unrelated to allocation.

Acceptance remediation (the review `docs/review-2026-09-17.md` says why; its
evidence is in `bench/results/2026-09-17-gce-review-*.md`):

- Step 3, post-delete RSS: a compacting thread cache (review P1).
  a. Close the compacting-flush experiment with the review's reading (soak
     and `bench/probes/churn.c`: the flush never runs under a balanced churn,
     so its selection policy cannot compact); keep the highest-address flush
     form.
  b. Scheduled address-ordered pops: every `2^k` pops of a class (`k = 3` to
     start, tunable `tcache_compact_shift`, per class if useful) serve the
     lowest-address cached slot instead of the top of the stack. Keep a sorted
     run at the bottom of the stack with a cursor: refills write their claimed
     slots ascending and reset the cursor, flushes keep the lowest half
     ascending; the scheduled pop returns `slots[cursor]`, moves the top entry
     into that position and advances the cursor; when the run is exhausted the
     scheduled pop claims a small batch from the lowest partial slab into a new
     run. Every `2^k` pushes of a class (or when a scheduled pop finds the
     cache more than half full) flush the highest-address `cap / 2` entries
     even when the cache is not full. The fast-path cost is one counter test
     and a well-predicted branch. Host tests: the sorted-run invariant, the
     scheduled pop returns the cache minimum, a churn model empties the high
     slabs.
  c. Acceptance: `bench/probes/churn.c` under challoc compacts 469 -> under
     230 slabs within 5 s at `k = 3` on either VM (`bench/probes/churn-run.sh`);
     larson (1 and 10 threads), xmalloc-test and alloc-test rows on both VMs
     within 3% of the pre-change rows measured in the same run; the 20-minute
     soak on the arm64 VM meets this phase's post-delete criterion (within 10%
     of jemalloc) with the per-class occupancy sampler
     (`bench/probes/soak-occupancy.sh`) recorded in `bench/results/`.
- Step 4, Valkey defrag tests: rerun `make valkey STAGE=memefficiency` and the
  full suite on both architectures at the step-3 library. For any remaining
  `Active Defrag` failure, first try allocator-side answers
  (`experimental.utilization.batch_query` at 4 KiB page-group granularity;
  `MALLOCX_TCACHE_NONE` allocations placed in the fullest partial slab;
  smaller slabs for the classes Lua's internals use), then, only where a
  Valkey constant encodes jemalloc's slab geometry (the test suite's 2 MB /
  5% / frag < 1.05 defrag thresholds; `active-defrag-ignore-bytes` only if the
  residue is provably pinned objects), a `USE_CHALLOC` value in the contrib
  patch with a PROGRESS.md design decision naming the jemalloc constant it
  replaces and the measurement behind it. The soak RSS criterion is never met
  this way.

## Phase 1.5: GCE performance lab

Goal: reference benchmark numbers and hardware performance counters come
from dedicated Linux VMs on Google Compute Engine, one per architecture,
instead of the shared Docker VM on the Mac. The Mac container stays the
development and correctness environment; its numbers are only ever compared
with other numbers from the same Mac session.

Background: Compute Engine exposes the CPU's performance monitoring unit to
C4A (Google Axion, aarch64) and C4 (Intel Xeon, x86_64) instances created
with `--performance-monitoring-unit=standard`; that level includes L1/L2
cache and dTLB/iTLB miss events on both architectures. No bare-metal
instance is needed. Sources: https://docs.cloud.google.com/compute/docs/pmu-overview
and https://docs.cloud.google.com/compute/docs/enable-pmu-in-vms.

Tasks:

1. `bench/gce/challoc-bench`: a bash driver modelled on the `mvmdev`
   pattern (subcommands `create`, `provision`, `sync`, `run`, `ssh`,
   `status`, `stop`, `delete`) with two targets, `arm64` and `amd64`.
   Defaults: project from the active `gcloud` configuration
   (`CHALLOC_GCE_PROJECT` overrides; the active default is `dlorenc-dev`), zone `us-east4-a` (since 2026-09-17; `us-central1-b` before, abandoned for capacity failures), which offers both series
   (`CHALLOC_GCE_ZONE` overrides; verified 2026-09-16 with `gcloud compute machine-types list`),
   `c4a-standard-16` and `c4-standard-16`, `--performance-monitoring-unit=standard`,
   Ubuntu 24.04 LTS images, 100 GB `pd-balanced`, OS Login, labels
   `owner=$USER,purpose=challoc-bench`. `provision` installs Docker,
   `linux-tools-$(uname -r)` for `perf`, `rsync`, and sets
   `kernel.perf_event_paranoid=-1` and `kernel.kptr_restrict=0`
   persistently. `sync` rsyncs the working tree (excluding `target/`,
   `.ralph/`, `.tmp-*`). `run` executes a command inside the repo's
   `Dockerfile.dev` image on the VM with `--cap-add=PERFMON
   --cap-add=SYS_ADMIN` (or `--privileged` if `perf` still refuses) so the
   same pinned toolchain is used everywhere; the results file records the
   VM's CPU model, kernel, and machine type. `stop` suspends the VM; every
   iteration that starts a VM must stop it before exiting, and `status`
   must be checked at the start of each iteration so an orphaned running VM
   is stopped.
2. PMU verification: on each VM, `perf stat -e cycles,instructions,
   branch-misses,L1-dcache-load-misses,dTLB-load-misses,iTLB-load-misses`
   on a trivial loop returns non-zero counts for every event, and
   `perf record` + `perf report` resolves symbols in `libchalloc.so`
   (unstripped build). Record the exact event names that work per
   architecture in `bench/gce/README.md`; Arm event names may differ from
   the x86 aliases.
3. Reference run: the full Phase 0 benchmark set (`cfrac`, `espresso`,
   `larson`, `mstress`, `xmalloc-test`, `alloc-test`, `glibc-simple`,
   `redis`) under challoc, jemalloc 5.3.1, glibc 2.44, and mimalloc 3.x on
   both VMs following the benchmark protocol, plus `perf stat` counters for
   `larson` 1-thread and 10-thread under challoc and jemalloc. Results in
   `bench/results/<date>-gce-c4a.md` and `<date>-gce-c4.md`. These
   supersede the Mac numbers as the reference for the Phase 0 and Phase 1
   criteria.
4. Close the larson gap: thread-affine slab refills (review P2). The
   counters attribute the whole 10-thread gap to cross-thread sharing of
   slabs under the single default arena (`bench/results/2026-09-17-gce-review-counters.md`:
   `narenas:16` closes it on both VMs).
   a. `ClassState::plan` prefers the slab this heap last claimed from (a
      per-cache hint slab number) while it is still partial, then the class's
      lowest partial slab; remote frees unchanged.
   b. Acceptance: larson 1/10/16 threads, xmalloc-test and alloc-test on both
      VMs; the 10-thread larson row within 3% of challoc `narenas:16` measured
      in the same run and within 10% of jemalloc; L1D misses per operation
      within 10% of the `narenas:16` figure (perf stat as in the review's
      counters file: `sudo perf` attached to the container); Valkey
      `unit/memefficiency` green.
   c. If (a) misses (b), set the default `narenas` to `min(4 * ncpus, 64)` and
      rerun the full Valkey suite and the soak. Measured 2026-09-17
      (`bench/results/2026-09-17-{aarch64,x86_64}-gce-thread-affinity.md`):
      (a) misses on both VMs (the 10-thread L1D misses per operation do not
      move; a hint cannot keep a thread's refills and flushes inside its own
      slabs, only a partitioned set can), so (c) is the path, with two
      requirements from the same run: `Arenas::assign` must pick the arena
      with the fewest bound heaps (decremented at heap exit, jemalloc's
      `arena_choose`) instead of round robin, because thread churn otherwise
      materialises every arena and leaves a partial slab per class in each
      (larson1 67 MiB against 13.6 today and jemalloc's 18); and the A/B must
      include xmalloc-test and RSS on x86_64, where `narenas:16` costs
      xmalloc-test 32% (still ahead of jemalloc) and the larson10/16 RSS
      rows read +48% / +37% against jemalloc before the assignment fix.
4b. Ruby benchmark rows (after task 4, before task 5): a
   `bench/ruby-bench.sh` and `make ruby-bench` over the `contrib/ruby/build.sh`
   stage: a fixed subset of ruby/benchmark run with `benchmark-driver`
   (`so_binary_trees`, `hash_aref_str`, `array_*`, `nbody`, `fib` as controls)
   plus one real script (`gem list`, or ERB rendering), the same `ruby` binary
   under challoc's `libjemalloc.so.2`, Wolfi's jemalloc via `LD_PRELOAD`, the
   `--without-jemalloc` glibc build and mimalloc; wall time and max RSS from
   `bench/rt`, plus `GC.stat` counts and RSS after a fixed number of GC cycles
   (Ruby's GC calls `malloc_trim`). Five rounds, sorted scores, both VMs,
   `bench/results/<date>-<arch>-ruby.md`. Acceptance: within 10% of jemalloc on
   time and RSS, as for the Phase 0 set; regressions are recorded, not fixed,
   in this task.
5. Fast-path residue on aarch64: dense cache headers in `Heap` (review P3).
   Inline `[CacheHeader; NCLASSES]` and one contiguous slot block per heap so
   `malloc` reaches the header without a pointer load and a thread's 52 cache
   headers share a few lines instead of 52 pages. Acceptance: 1-thread larson
   on both VMs not slower, aarch64 dTLB refills per operation down toward
   jemalloc's (3.4 -> ~2 per operation), all tests green.
5b. Harness: two tiers (delivered 2026-09-17, outside the loop). A fast tier
   `bench/quick.sh` / `make quick` / `challoc-bench quick <vm> [OLD=rev]`
   that builds the working tree and its base (`OLD`, default HEAD) from
   `git archive` into `target/quick/{new,old}`, checks the builds differ
   exactly when the sources do, and runs one round of larson 1/10,
   xmalloc-test, alloc-test 1 and the churn probe under old, new and
   jemalloc with a go/no-go exit status (`QUICK_TOLERANCE` 5%, churn end
   occupancy at least 90%); a protocol tier `bench/ab.sh` / `make ab` /
   `challoc-bench ab <vm> VARIANTS='name=rev[:MALLOC_CONF] ...'` (5
   interleaved rounds plus warm-up, sorted scores, median RSS, load average,
   the results-file table shape, `ADAPTIVE=1` stop after 3 separated
   rounds) replacing the per-iteration `.tmp-itNN-inner.sh` scripts; cached
   stock-allocator baselines (`bench/baseline.sh`, `BASELINES=cached`, one
   jemalloc sanity round within 3% or the cache is stale); `make soak-quick`
   (challoc 600 s with the occupancy sampler folded into `valkey-soak.sh`
   as `OCCUPANCY=1`, compared with a cached jemalloc soak from `make
   soak-baseline`, the summary saying so); and driver support (`quick`,
   `ab`, `session start|stop` with `CHALLOC_GCE_WARM=1`, `fetch` that never
   overwrites newer local files, `sync` warning about uncommitted `src/`).
   Acceptance rows and results files keep using the protocol tier,
   same-run, unchanged criteria.
6. `bench/gce/README.md` and a CLAUDE.md environment bullet: how to create,
   sync, run, and stop the VMs; the cost of leaving one running; and the
   rule that Mac numbers and GCE numbers are never compared with each other.

Acceptance:

- Both VMs create, provision, sync, run the container test suite (`make test`
  equivalent) green, and stop, from a single driver invocation each.
- PMU counters return non-zero for the six events above on both VMs.
- The Phase 0 benchmark set is within 10% of jemalloc 5.3.1 on time and
  RSS on both VMs, measured per the protocol; regressions worse than 10%
  block the phase.
- `perf stat`/`perf report` on `larson` shows no lock or atomic RMW in the
  top 10 symbols of challoc's profile, and the counter attribution of any
  remaining larson gap is written down with the event data.
- No VM is left running at the end of the phase.

## Phase 2: hardening extras and cgroup awareness

Tasks:

1. Guard slabs with `MADV_GUARD_INSTALL` and `guard_slab_interval`;
   `PROT_NONE` fallback for huge runs only; verify with a test that touching
   the guard faults and that `smaps` shows no VMA explosion for 10,000 slabs.
2. `mseal` of the region table and reservations; test that `munmap` of a
   reserved range fails with `EPERM` on kernels 6.10 and newer.
3. `zero_on_free`, `slot_randomize`, `double_free_scan` tunables with tests,
   plus a benchmark row for each showing its cost on `larson` and `redis`.
4. `src/sys/cgroup.rs`: discovery, effective limit walk, `memory.current`
   sampling, `memory.events` diffing, PSI trigger in the background thread.
   Host unit tests parse captured `/proc/self/cgroup`, `mountinfo`, and
   `memory.*` file contents including `max`, nested limits, and cgroup v1
   absent files.
5. Policy table from design.md section 11 wired into decay and cache GC; THP
   suppression under small limits; `stats.cgroup.*` keys.
6. Container tests with `docker run --memory=512m` and `--memory=256m`:
   a churn program that allocates 400 MiB, frees it, and idles must see RSS
   fall below 100 MiB within 2 s with cgroup awareness on and must not with
   it forced off; a program that oscillates between 200 MiB and 480 MiB live
   must not be OOM-killed in the 512 MiB container over 5 minutes.
7. A comparison run of the same container tests with jemalloc 5.3.1 and
   mimalloc 3.x recorded alongside.

Acceptance:

- All Phase 0 and Phase 1 acceptance criteria still pass with defaults.
- Default hardening costs no more than 3% on the Phase 0 benchmark set
  relative to Phase 0 results.
- The two container tests pass; jemalloc's behaviour in the same tests is
  documented as the baseline.

## Phase 3: packaging and rollout

Prerequisite: one aarch64 test run on a 64 KiB-page kernel. Wolfi's
`build-aarch64.env` builds jemalloc with `JEMALLOC_SYS_WITH_LG_PAGE=16`, so
Wolfi expects 64 KiB-page aarch64 hosts; the GCE VMs and the Mac container
are 4 KiB. challoc reads the page size at runtime and its smallest slabs
(4 KiB since 2026-09-17) use the page-group commit/guard/purge path there,
which only the faked-page unit test has exercised.

Tasks:

1. Wolfi package definition `challoc.yaml` producing `challoc`, `challoc-dev`,
   and `challoc-jemalloc-compat` with `libjemalloc.so.2` and `jemalloc.pc`.
2. Valkey package change in Wolfi using the Phase 1 patch and depending on
   `challoc`, with the package test asserting `valkey-server --version`
   reports challoc and that `CONFIG SET activedefrag yes` succeeds.
3. Ruby, KeyDB, RethinkDB switched to `challoc-jemalloc-compat`, one at a
   time, each with its package tests passing.
4. `docs/tuning.md` explaining every tunable and the cgroup policy for
   operators.

Acceptance: packages build and pass tests in Wolfi CI. This phase is executed
in the Wolfi repository, not here; this repository only tracks the patches and
package definition drafts under `contrib/`.

## Phase 4: experiments

Each is optional, behind a feature or tunable, and must not change default
behaviour or default performance.

1. Per-CPU mode via rseq `mm_cid` (design.md section 15). Benchmark on a
   64-thread `larson` and `mstress` against the default mode.
2. Type-isolated heaps: an `__alloc_token_malloc(size, token)` family of entry
   points for Clang's `-fsanitize=alloc-token`, mapping tokens to arenas.
   Build one Wolfi package with it and measure.
3. MTE on AmpereOne: `PROT_MTE` slab regions with random tags per allocation,
   detected at runtime, off elsewhere.
4. Sampling-based guarded allocations in the style of GWP-ASan for crash
   reports in production.

## Benchmark protocol

- Container: Wolfi base, pinned toolchain versions recorded in the results
  file, kernel version recorded, CPU model recorded.
- Warm-up run discarded; report median of 5 runs for time and max RSS from
  `/usr/bin/time -v`.
- Same binary of each benchmark program for every allocator; only
  `LD_PRELOAD` changes.
- Emulated x86_64 on Apple silicon is never used for numbers.
- Reference numbers come from the GCE VMs in Phase 1.5 (C4A for aarch64,
  C4 for x86_64, PMU enabled). The Docker VM on the Mac is for development
  A/B rounds only; a Mac number and a GCE number are never compared.
- Two tiers (`bench/README.md`, Phase 1.5 task 5b). The fast tier
  (`bench/quick.sh`, `make quick`, `challoc-bench quick`: one round of
  larson 1/10, xmalloc-test, alloc-test 1 and the churn probe under old,
  new and jemalloc, about two minutes) is for iteration decisions only: it
  says whether a variant is worth the protocol run, and its numbers are
  never recorded in `bench/results/` or used against a criterion. The
  protocol tier (`bench/ab.sh`, `make ab`, `challoc-bench ab`: 5 rounds plus
  warm-up, interleaved variants, sorted scores, median RSS) is what
  acceptance and the results files use, with every variant measured in the
  same run; the criteria above are unchanged. Cached stock-allocator
  baselines (`bench/baseline.sh`, `BASELINES=cached`) save time in
  iteration runs and are revalidated by a jemalloc sanity round each time;
  acceptance runs do not use them.
