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
   `libc::syscall`, for the list in design.md section 18. Include `abort`
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
4d. Valkey throughput and p99: the soak's remaining half of the Phase 1
   criterion (2..8% behind bundled jemalloc in every soak so far, unchanged by
   the `narenas` default of task 4, so it is not the slab sharing the review
   attributed it to). Diagnose on the arm64 VM with the soak's write and
   delete phases running: `sudo perf stat` and `sudo perf record` attached to
   `valkey-server` under challoc and under bundled jemalloc (per-op cycles,
   instructions, L1D and dTLB misses; cycles by DSO and symbol), the
   allocator's share of the server's cycles, and the fast-path annotations;
   compare the quantum8 build's class table and slab geometry with jemalloc's
   for Valkey's actual size mix (`MEMORY MALLOC-STATS` bins during the write
   phase), and check the generators' client-side p99 against the server's
   `INFO commandstats` so client noise is separated from server time. Record
   the attribution in `bench/results/<date>-aarch64-gce-valkey-throughput.md`,
   then implement the fix the data supports (A/B rows and the soak decide).
   Acceptance is the plan's Phase 1 soak criterion: throughput and p99 at least
   equal to bundled jemalloc; if the attribution shows the gap is outside the
   allocator (client saturation, kernel), record that with the evidence and
   say so under Notes for the human.
4e. Sized frees at jemalloc parity (human's decision, 2026-09-18, on task
   4d's attribution: 200 of Valkey's 273 extra cycles per SET are the exact
   `free` check's descriptor bitmap-word load on `je_sdallocx`, a cache miss
   per free on a 550 MiB heap). `sdallocx`/`dallocx` with a size hint check
   the hint arithmetically against the pointer's class (no memory access;
   a mismatch aborts) and push without the bitmap-word test; the batch
   release at cache flush already validates every bit it clears, so an
   invalid or double free through the sized path is caught there, up to a
   cache's worth of frees later. Plain `free` is unchanged. Record the
   guarantee precisely in README's security table and design 6.3/13.2.
   Acceptance: `quick` on both VMs, A/B rows (larson does not use `sdallocx`,
   so expect no change; add an `sdallocx` variant of the churn probe or a
   Valkey-shaped microbench so the row exists), the 20-minute soak on arm64
   with `bench/probes/valkey-perf-sidecar.sh`: cycles per SET in `free` from
   338 toward jemalloc's 71 and the plan's soak criterion (throughput and
   p99 at least equal to bundled jemalloc) met or the remaining gap
   attributed; negative tests updated for the deferred detection. The soak's
   throughput columns carry 3..5% of generator noise between runs; the
   tie-breaker for "at least equal" is cycles per command from
   `bench/probes/valkey-perf-sidecar.sh` (challoc 3611 vs jemalloc 3431 per
   SET at 078cc0d) and the `valkey1` row, both recorded next to the soak.
4f. Dense per-class slot bitmap (keeps the exact check cheap for plain
   `free`): move the allocation bits out of the per-slab descriptors into one
   contiguous per-class bitmap indexed by `slab * nslots + slot`, committed
   on demand like the descriptor tables, so the `free` check touches a
   structure of one bit per slot (70 KB for a 550 MiB heap of 1 KiB values,
   L2-resident) instead of one 64-byte descriptor line per slab (9 MB);
   descriptors keep state/arena/live/dirty_at and shrink, which also
   addresses the descriptor-page residue noted in iteration 15. Claim and
   release operate on bit ranges of shared words (`fetch_or`/`fetch_and`
   with masks; a slab's bits may straddle words for classes whose `nslots`
   is not a power of two). Trap to design around first: slabs go to arenas in
   address order as they commit, so in a dense bitmap adjacent slabs of
   different threads' arenas would share a cache line and the `free` check
   would see the coherence traffic the `narenas` default removed; hand slabs
   to an arena in line-sized groups (16 or 64 consecutive slabs, so a bitmap
   line belongs to one arena) or lay the bitmap out per arena. Measure the
   10-thread L1D misses per op before and after; a rise means the grouping is
   wrong. Acceptance: all tests and negative tests; `quick`
   then A/B rows on both VMs (larson/xmalloc/alloc-test within 3% of old or
   better); the Valkey soak with the perf sidecar (plain-`free` cycles and
   L1D misses per command) and the Ruby rows (Ruby frees through plain
   `free`), both recorded. If the sized path's check becomes cheap enough,
   revisit 4e's deferral and prefer the immediate check.
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
5c. Harness: target-shaped rows and scorecard (delivered 2026-09-18, outside
   the loop; validated on the Mac container for correctness, the VM parts
   pending their first VM session). The loop's feedback measured proxies
   (larson, xmalloc-test, alloc-test: working sets of a few MiB) while the
   criteria are Valkey and Ruby, and progress was only visible file by
   file. Delivered: `bench/probes/valkey_model.c` and the `bench/lib.sh`
   rows `valkey1` (frees through `sdallocx`, as Valkey does) and
   `valkey1-free` (plain `free`): a single-thread model of the soak's
   server, 850k keys of entry + key + object + 16..1024-byte value (about
   500 MiB live), a SET-only write phase over random keys and a DEL:SET 4:1
   delete phase, scored by write-phase M ops/s, RSS and `mallctl` stats per
   phase in the log; runs under challoc, jemalloc, glibc and mimalloc.
   `redis` and `ruby-churn` (gc_churn.rb on the glibc Ruby build) as
   `lib.sh` rows. `quick` runs `valkey1` by default (5 s) and, with
   `PERF=1` where perf exists, prints per-operation cycles, instructions,
   L1D and dTLB misses for `larson1` and `valkey1`; on the VMs
   `challoc-bench quick-perf <vm>` (`bench/gce/quick-perf.sh`) prints the
   same table with perf attached from the host. `bench/scorecard.sh` /
   `make scorecard` / `challoc-bench scorecard <vm>`: the fixed row set
   under challoc and jemalloc, 3 rounds through `ab.sh`, one line per
   (date, commit, arch) in `bench/results/scorecard-<arch>.md` with the
   commit's latest soak, `--render` for the trend; both files seeded from
   the GCE results files. Noise: `PIN=1` (`bench/pin.c`) as an experiment
   for the next VM session, `KEY_PATTERN=uniform` for the soak's pinned-key
   artifact (default unchanged). Process: the attribution rule and the
   scorecard step in `scripts/ralph-prompt.md`. Acceptance for the VM
   session that first uses it: `quick` under 2.5 minutes with `valkey1`,
   `quick-perf` prints the four counters per op for both rows under all
   three variants, `scorecard` appends one line per VM in about 8 minutes,
   and the `valkey1` row under challoc shows the free-path gap the soak
   attributed (the row moving when task 4e lands is the test of the row).
6. `bench/gce/README.md` and a CLAUDE.md environment bullet: how to create,
   sync, run, and stop the VMs; the cost of leaving one running; and the
   rule that Mac numbers and GCE numbers are never compared with each other.

7. Release readiness, part 1: the calloc race (`docs/review-2026-09-18.md`
   sections 1.1 and 1.4; the review calls tasks 7-13 "Phase 1.75"). In this
   session's `make test` on the Mac container at 6a9016e, mimalloc's stress
   test aborted once because `calloc` returned memory containing a non-zero
   word (`alloc_items: Assertion p[i] == 0 failed`); 150 standalone and
   parallel reruns did not reproduce it. (a) Fix the confirmed compaction
   wrap first: `Heap::pop_compact_run` calls `pop_top(cache, 0)` when
   `rebuild_run`'s `tick` ran the cache GC, which empties a cache with
   `low_water == len <= 3`, and the rebuild returned early (no longer
   fragmented, or the claim got nothing); `len - 1` wraps in release. Guard
   it the way the probe branch does (refill, or return null) and add a
   unit test that puts a compact-mode cache at one entry with the GC due
   and pops. (b) Make the failure reproducible: a `check_zero` debug mode
   (a `MALLOC_CONF` key, off by default, or `cfg(debug_assertions)`) that,
   in `ClassState::claim` for a pick flagged zero and in `Huge::take` when
   returning `zeroed = true`, reads the first and last word of each claimed
   slot or page and aborts with class, slab, prior state, arena, discard
   backend and the offending word. Run `stress 8 50 10` under the full
   concurrent `cargo test api::` load on both VMs (20 runs each) and on the
   Mac container, plus `stress 16 100 20` standalone, with `check_zero` on;
   then the configuration bisect under the same load (`dirty_decay_ms:-1`,
   `tcache_compact_shift:0`, `narenas:1`, several hundred iterations each,
   `uname -r`, the probe line and the THP setting recorded). (c) Name the
   mechanism in PROGRESS.md with the interleaving, add a test that fails on
   the pre-fix code, land the fix. Acceptance: (a) landed with its test;
   the mechanism named and fixed, or, after the full protocol of (b) with
   no reproduction, the evidence recorded under Notes for the human and the
   `check_zero` mode kept; `make test` passes 10 consecutive times on both
   VMs and on the Mac container.
8. Release readiness, part 2: release assertions (review 1.2 items 2-8).
   Promote to `assert!`: the cache run invariant (checked in
   `pop_compact_run` as `at < len`), `refill`'s empty-cache precondition
   (heap.rs:1767), `free_caches`'s flushed precondition (992),
   `other_caches`'s not-home precondition (1648), `AtomicBitmap::release`'s
   mask-inside-view check (bitmap.rs:174), the huge table's duplicate-key
   check (huge.rs:297); `State::from_u8` aborts on 4..255; `Meta::free`
   rejects a block equal to its free-list head; `bin_stats` counts clamp
   events and reports them in `malloc_stats_print`. Set `overflow-checks =
   true` in `[profile.release]`. Acceptance: `make quick` go on both VMs
   (the arithmetic on the fast paths is a handful of adds; if a row moves
   more than 5%, attribute it and decide); all tests green; each new abort
   has a unit test.
9. Release readiness, part 3: detection gaps (review items 9, 10; 5.1).
   (a) `Heap::flush`: after the sort, abort if `all[rest_len - 1] ==
   all[rest_len]` (a cached double free whose copies straddle the flush
   boundary); `tests/c/negative.c` mode `double_free_straddle` that primes
   the cache so the pair straddles `cap / 2`. (b) The sized fast path
   rejects slot starts in guard slabs and in slabs at or above the arena's
   committed watermark (arithmetic plus one load), or the flush does;
   negative mode `sized_guard`: `sdallocx` of a guard slab body then
   `malloc` must abort, not return the guard memory. (c) `double_free_scan`
   implemented as a compare against the top 4 cache entries on plain
   `free` and the sized path; benchmark rows (`valkey1`, `larson1`, `redis`
   via `ab`), and if the cost is under 1% on every row it becomes the
   default and README's table and footnote say so. Acceptance: negative
   modes pass on both VMs; `quick` go; rows recorded.
10. Release readiness, part 4: lifecycle (review items 11-14). (a)
    `fork_prepare`/`fork_release` take `Shared.stats`, `Arena.stats` and
    `Tcaches.lock` in nesting order, and the lock test enumerates every
    `Lock` in the crate so a new one cannot be missed; `tests/c/fork_alloc.c`
    gains a mode that forks while a second thread loops on
    `mallctl("epoch")` and `tcache.create`/`destroy`. (b) `heap_destructor`
    writes `TLS_EXITED` before `Heap::destroy`. (c) `errno` is saved and
    restored around the background thread's `pthread_create`, or the
    thread is started only from allocation-side ticks; `tests/c/libc_api.c`
    `test_errno` covers a `free` that starts the thread. (d) One background
    stack is reserved once and reused across stop/start. Acceptance: tests
    for each; `make test` green on both VMs.
11. Release readiness, part 5: truthful docs and inert keys (review items
    17-22, section 3). README's security table and kernel table, design 14
    and design 10: `mseal` "planned (Phase 2 task 1)", `zero_on_free` and
    `slot_randomize` "planned (Phase 2 task 4)", guard slabs "kernel 6.13+
    only"; README's Status line replaced by the current state; the design
    edit list of review section 3 applied (14 stale statements, 6 never
    built, design 11 headed "Planned"); `abort_conf` rejects the inert
    keys (`thp`, `zero_on_free`, `slot_randomize`, `double_free_scan`,
    `cgroup`, `stats_print`) and without it `malloc_stats_print` lists them
    as "accepted, no effect"; `huge_retain_max` becomes a `malloc_conf`
    key (design 7 says it is one) or design 7 says it is a constant.
    Acceptance: every claim in README and design.md is true of the code
    or marked planned; config tests updated; all tests green.
12. Release readiness, part 6: seams (review items 23-34). One
    `Config::cap_for(home, arena, class)` replacing the four encodings of
    "which classes are cached where"; `alloc_in` misses go to
    `alloc_slow_in(arena)` so `MALLOCX_ARENA(home)` keeps its arena for
    classes above `tcache_max`; `compact_mask` derived from `mode`; the
    decay step gate lifted from `ClassState` and `Huge` into `purge.rs`;
    `set_arena` folds the thread counters before freeing caches; the
    unused `Heap::rng` and its per-heap `getrandom` removed; `curslabs`
    and `uncached` accessors; owner word checked against `desc.arena` in
    `take_group`; stale prose and phase history removed from `src/` module
    docs (review 1.3 and items 32-33). Acceptance: tests green; `quick` go
    on both VMs (heap.rs changes); no benchmark row moves outside noise.
13. Release readiness, part 7: packaging decisions recorded (review items
    35-42), each as a design decision with the reason: SONAME
    `libchalloc.so.0` on the plain build; one library or two for
    quantum8/quantum16; `challoc_tls_get`/`challoc_tls_set` replaced by a
    `challoc_version()` probe (tests updated); `je_pvalloc` added; sized
    `operator delete` routed through the sized path or documented as
    ignoring its size. Acceptance: decisions in PROGRESS.md, design 13 and
    16 updated, tests green.
14. The usable-size chain (review 4.2; `bench/results/2026-09-18-aarch64-
    gce-sized-free.md` 4.3 and `-dense-bitmap.md`): `malloc_usable_size` on
    a slab pointer reads its geometry from the dense arrays as the `free`
    fast path does (`Regions::slab_slot`, then the class bitmap word),
    keeps the bit test, returns the class size. Attribution first
    (`quick-perf` on arm64, the sidecar in the soak's delete phase), as the
    rule requires. Acceptance: the query at or under jemalloc's cycles per
    command in the delete phase; the 20-minute soak on arm64 with the
    sidecar meets the Phase 1 criterion (throughput and p99 at least equal
    in both phases) or the residual is attributed; `valkey1` on aarch64
    within 5% of jemalloc; all tests green.
15. mstress RSS, the row Phase 1.5 acceptance is blocked on (review 4.4;
    `bench/results/2026-09-18-phase-1.5-acceptance-3.md` 3.1). The human
    accepted the review's recommendation on 2026-09-18 ("Go for it"): (d)
    in whole-group form. `SlabTable::take_group` first offers an idle group
    (every slab `Fresh`, `Empty` or `Purged`) of the same class owned by
    another arena, taken under both arenas' class locks in index order,
    owner word and descriptors' arena rewritten, set bits moved; per-arena
    statistics follow the group; design 13.3 amended ("an arena owns its
    groups until it lends an idle one"). The line-ownership invariant of
    task 4f is the point: whole groups only. Acceptance: mstress RSS on
    both VMs within the 10% band, or the residual attributed with
    `bench/probes/arenasample.c` and recorded, in which case the row is
    accepted as the price of per-class regions at 16 arenas (the review's
    (c)) and design 13.3 says so; larson10 L1D misses per operation
    unchanged; `quick` go on both VMs; `unit/memefficiency` green; the soak
    unchanged; scorecard lines.
16. Acceptance rerun as written below; then the phase is done.

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

Order (2026-09-18 review, section 5.1): task 2 (`mseal`, already claimed
by README and design 14) first, then task 1's end-to-end guard-slab fault
test and the kernel dependency in README's table, then tasks 4 to 7
(cgroup awareness: goal 4 and the only one with no code), then task 3
(the opt-in tunables with their cost rows; `zero_on_free` and
`slot_randomize` stay opt-in because a `memset` per free is more than the
3% budget on Valkey's 1 KiB values and randomisation fights the compaction
that passed Phase 1). Acceptance gains: no key in design 12 is inert.

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

## Phase 2.5: floor and ceiling

Goal (the human's direction, 2026-09-18): the floor is a compatibility
promise (glibc 2.44, kernel 5.10, Wolfi's compiler baseline), proven on
real kernels rather than claimed; the ceiling is the product: the same
binary lights every kernel and CPU feature the host offers, at runtime,
without a rebuild, and challoc is the best allocator on the hosts
Chainguard ships (a recent kernel, a growing CPU matrix on GCP). Every
probed feature ships with a test on a host that has it and a test on a
host that lacks it, a cost row, and a line in the feature report.

Rules for the phase: the compiler baseline stays where Wolfi sets it (the
ceiling is reached by runtime probes and `asm!`, never by `-C target-cpu`);
a feature that is on by default when present must be within noise on the
reference VMs' scorecard when absent and must not change observable
behaviour (allocation results, abort conditions) between hosts, only cost,
memory and detection; tagged pointers are opt-in per consumer until the
Valkey and Ruby suites pass with them on.

Tasks, in order:

1. Comparison matrix (the human's request, 2026-09-18): tcmalloc
   (Wolfi's `tcmalloc-minimal` from gperftools 2.18.1) added to the dev
   image and to `bench/lib.sh` as a fourth stock allocator (`is_stock`,
   the library path, `bench/baseline.sh`, `CGROUP_ALLOCS`); Python rows:
   Wolfi's `python-3.13`, a `bench/python-bench.sh` and `make python-bench`
   over pyperformance's standard set if it installs on the VM, else a fixed
   set of stdlib scripts (JSON encode/decode, dict and list churn, regex, an
   object-graph builder), each workload run two ways, `PYTHONMALLOC` unset
   (pymalloc for objects under 512 bytes; the allocator sees the rest) and
   `PYTHONMALLOC=malloc` (every object through the allocator), plus the
   free-threaded build as a third variant if Wolfi packages it; five rounds,
   wall time and peak RSS, both VMs, under glibc, jemalloc, mimalloc,
   tcmalloc and challoc via `LD_PRELOAD`. Then one same-run four-way table on
   both VMs: the Phase 0 set, the Ruby rows, the Valkey model rows, the
   Python rows, the container tests at 512 and 256 MiB (the Valkey soak is
   jemalloc-only, no other allocator has the API; say so). Results in
   `bench/results/<date>-<arch>-gce-four-way.md`; the scorecard gains a
   `python1` row (default) and `python1-malloc` row. Acceptance: the table
   exists with every cell from one run per VM; any row where challoc is
   more than 10% behind any allocator has an attribution before anything
   is changed for it.
1b. The flush sort (attributed in task 1: `bench/results/2026-09-19-{aarch64,x86_64}-gce-four-way.md` section 2, mechanism M2). When a per-class cache overflows, `Heap::flush` sorts the whole slot array by address (`sort_unstable` over up to 1024 pointers) to release the highest-address half grouped by slab; on tiny-object churn that sort is 9..15% of the whole process (glibc-simple 9.2%, Python `dict_list-malloc` 15.6%, mstress 4.0%) and a third of challoc's own time, the largest single term in every row where challoc trails mimalloc or tcmalloc by more than 10% on time. Replace it with the cheapest form that keeps the flush's two properties (the released half is the highest addresses, so holes drain to the high slabs, and the release is grouped per slab so `release_owned` does one word update per slab): candidates, in order of expected cost, a selection (`select_nth_unstable` on the address, O(n)) followed by a sort of only the released half, or a bucket pass keyed by slab number (the class geometry gives the slab in one shift; slabs per flush are few), or a threshold on the address distribution that skips the sort when the cache holds slots of one or two slabs. The kept half's order does not matter (the run is dropped at a flush and rebuilt by the scheduled pop, design 6.4). The flush-boundary double-free check of Phase 1.5 task 9 (a) must survive in the new form (duplicates within the released batch abort in `release_owned`; the boundary compare becomes a check that no pointer of the kept half equals one of the released half, which a per-slab bucket pass gives for free). Attribution rule: before and after `quick-perf` on `glibc-simple`, `python1-malloc` and `larson1` on both VMs, the sort's share by `perf record` recorded next to the counters; the change names the term it removes. Acceptance: glibc-simple and `python dict_list-malloc` improve by at least half of the sort's measured share on both VMs (same-run `ab`, five rounds, against 1339eb6); `quick` go on every row; the churn probe still compacts (the compacting cache's high-address flush is what it measures); `negative.c` `double_free_straddle` and `double_free_cached` pass; the calloc-zero campaign's default configuration clean; scorecard on both VMs.
2. The feature report. `challoc_features()` (C-callable, a static string)
   and the `Kernel features` line of `malloc_stats_print` report the
   kernel probes (existing) and the CPU: arm64 from `AT_HWCAP`/`AT_HWCAP2`
   (MTE, PAC, LSE, LSE2, BTI, SVE), x86_64 from `cpuid` (LAM, LA57 enabled,
   CLDEMOTE, WAITPKG, ERMS/FSRM, AVX-512), both with L1D/L2 sizes and the
   cache-line size from the CPU (`CTR_EL0`, leaf 4). Design gains a
   section "Floor and ceiling" listing every probed feature, the kernel
   or CPU that provides it, what it turns on, and its fallback; README's
   kernel table becomes a kernel-and-CPU table. Acceptance: the line on
   both VMs and in the Mac container matches `/proc/cpuinfo`; a host test
   parses captured `cpuid`/HWCAP values.
3. The floor, proven: `challoc-bench matrix` creates throwaway VMs from a
   list of images and runs `make test`, the probe line, `make fuzz` and
   `make test-cgroup` on each, then deletes them: kernel cells 5.10
   (Debian 11), 6.1 (Debian 12), 6.8 (Ubuntu 24.04 GA), 6.13+ and the GCE
   7.0 (the reference), and the 64 KiB-page aarch64 cell (Ubuntu arm64
   `linux-generic-64k`; the `huge.rs` tests use the runtime page size first);
   one bare-metal or nested-KVM cell for the `process_madvise` batch path
   if a host can be found, else the GCE Docker `--privileged` run. Results
   in `bench/results/<date>-matrix.md`, one line per cell with the probe
   line and the outcome. This absorbs Phase 3 prerequisites (a) and (b).
   Acceptance: green on every cell, including 5.10 and 64 KiB pages; every
   fallback path has now run on a kernel that needs it.
4. The CPU matrix on GCP: the same `matrix` subcommand over machine types:
   C4A (Axion, Neoverse V2), T2A (Ampere Altra, Neoverse N1), C4 (Xeon
   8581C), C3 (Sapphire Rapids), C3D or N2D (AMD Genoa, Zen 4), C2
   (Cascade Lake). Per CPU: the probe line, `make test`, and one scorecard
   run (3 rounds, challoc vs jemalloc) recorded as `scorecard-<cpu>.md`
   next to the two reference files (the reference for acceptance stays
   C4A and C4). Acceptance: the feature report per CPU is recorded; any
   CPU where a default-on feature costs more than 3% on a scorecard row
   has the feature gated on that CPU or the cost recorded as accepted.
4b. The Valkey model row on non-reference CPUs (found by task 4,
   `bench/results/2026-09-19-cpu-matrix.md`): `valkey1` against jemalloc
   reads +42% on Sapphire Rapids (8481C), +43% on Genoa (9B14), +21% on
   Milan (7B13) and +34% on Cascade Lake, against +4..5% on the reference
   Emerald Rapids (8581C) at both 8 and 16 vCPUs; `valkey1-free` and
   `xmalloc` are 8..22 points off on Genoa, Milan and Cascade Lake; the
   T2A Altra larson rows are 9..19 points off Axion. The SIGPROF sampler
   on C3 and C3D lab VMs put two thirds of the valkey1 gap in the model
   program's own time per operation under challoc (61 vs 48 samples per
   Mop on the 8481C; the working set is 147 MiB active under challoc
   against 298 under jemalloc for the same 141 MiB allocated) and one
   third in the allocator, with challoc's sized and plain paths at the
   same speed while jemalloc's `sdallocx` is 12..23% faster than its
   `free` on these CPUs. None of these cells has a PMU (GCE refuses
   `--performance-monitoring-unit=standard` on T2A, C3 and C3D), so the
   mechanism is not attributed. Attribute it: (a) a PMU host for one
   session, `c3-standard-192-metal` (bare metal exposes the PMU) for at
   most two hours, or, if quota refuses it, a `c3-standard-8` with
   `--performance-monitoring-unit` retried in another zone, or the
   sampler's per-instruction annotation on the lab VM; (b) `quick-perf`
   for `valkey1`, `valkey1-free` and `larson1` under challoc and jemalloc
   (cycles, instructions, L1D, L2, dTLB misses per operation, and the
   L2/L3 miss split if the PMU has it) and `perf record` with the
   annotation of the model program's own hot loop, since two thirds of the
   gap is there: the hypothesis to test first is placement, challoc's
   denser layout (per-class regions, address-ordered slabs, 147 vs 298
   MiB) putting the program's hot objects into fewer cache sets or onto
   shared lines on CPUs whose L2 is 2 MiB but whose L3 slice, prefetcher
   or set-index hashing differs from the 8581C's (the same shape as the
   descriptor-aliasing finding of 2026-09-16: compare hot object
   addresses modulo the L2 way size and the L3 set index under both
   allocators); the second hypothesis is the sized path (jemalloc's
   `sdallocx` advantage on these CPUs and challoc's flat sized/plain cost).
   Record it in `bench/results/2026-09-19-<cpu>-valkey1-attribution.md`
   with the counters, the annotation and the mechanism named; propose the
   fix (a placement change such as a per-class or per-slab random offset
   in slot order, or a sized-path change) as the next task with its
   expected effect, but do not implement it in this task unless it is one
   line and its own `quick` on the affected CPU and on both reference VMs
   is go. Acceptance: the mechanism is named with per-operation counters
   on at least one affected CPU; the T2A larson rows have a first reading
   (the N1 core's L1/L2 and the outlined LSE helpers are the candidates);
   the PMU host, if used, is deleted at the end.
5. Tagged pointers, part 1, the probes and the design: whether Axion
   exposes MTE (`HWCAP2_MTE`), whether the C4/C3/C3D CPUs expose LAM or
   UAI and the kernel `arch_prctl(ARCH_ENABLE_TAGGED_ADDR)` accepts it;
   design section "Tagged pointers": on MTE hosts, class regions mapped
   `PROT_MTE`, a random 4-bit tag per allocation set with `stg`/`st2g`
   over the slot on allocation, retagged on free, tag-check faults
   reported through the existing abort path, asynchronous mode by default;
   on LAM/UAI hosts, the top bits carry a class-and-generation tag that
   `free`, `realloc`, `usable_size` and the sized path verify (a mismatch
   aborts; loads are not checked); the `tag_pointers` key (`off`, `mte`,
   `lam`, `auto`) default `off` in this phase. Acceptance: the probe results
   on every CPU of task 4 in the matrix file; the design section; a note
   for the human on whether the reference VMs can test MTE at all (if not,
   an AmpereOne or Graviton4 box is the prerequisite for part 2).
6. Tagged pointers, part 2, implementation behind the key, on whichever
   of MTE and LAM a reachable host has: negative tests (read through a
   freed pointer faults under MTE; `free` of a pointer with a stale tag
   aborts under both), the fuzzer and the Valkey and Ruby suites under the
   key, cost rows (`valkey1`, `larson1`, `redis`, the Ruby rows; MTE's
   retag on free is the `zero_on_free` store pattern in hardware, so record
   both next to each other), fork behaviour with tagged pages measured.
   Acceptance: suites green with the key on; the cost recorded; the default
   stays `off` unless the cost is under 3% on every target row, in which
   case the human decides.
7. PAC-signed cache pointers on arm64 (`pacda`/`autda` on the slot pointers
   in the thread cache and the heap pointer in TLS, keyed per process),
   probed by `HWCAP_PACA`, on by default when present if `quick` on the
   C4A is within noise. Acceptance: a corrupted cache entry faults (test
   through the `check_zero`-style debug hook), `quick` go, scorecard line.
8. Per-CPU caches through rseq `mm_cid` (design 15, promoted from Phase 4):
   caches keyed by the running CPU's `mm_cid` read from glibc's registered
   rseq area, pop and push inside rseq critical sections (`asm!` on both
   architectures, kernel-owned fields never written), arenas per CPU rather
   than per thread when the mode is on, thread caches as the fallback on
   kernels or glibc without `mm_cid`; `percpu_arena:percpu` selects it,
   `auto` when the probe passes. Acceptance: `mstress`, `alloc-testN`, a
   64-thread larson and a Sidekiq-shaped Ruby soak with many idle threads
   (RSS bounded by cores, not threads); `quick` and the scorecard within
   noise or better on the reference VMs; the Valkey and Ruby suites green;
   the calloc-zero campaign clean under the mode; the mstress x86_64 RSS row
   re-measured (this is the mechanism that could bring it inside the band).
9. Multi-size THP for slab regions (kernel 6.8+): probe
   `/sys/kernel/mm/transparent_hugepage/hugepages-64kB/enabled` and friends,
   `MADV_HUGEPAGE` on class regions whose slab size matches an enabled folio
   size, `MADV_COLLAPSE` for hot metadata pages; measure the aarch64 dTLB
   column (3.2 refills per operation against jemalloc's 1.7) and fork time
   under a Valkey BGSAVE shape (the copy-on-write cost that forbids 2 MiB
   pages must stay absent at 64 KiB). Acceptance: dTLB column moves toward
   jemalloc's on the C4A, fork time within 5% of today, scorecard within
   noise; default on only if both hold, else `thp:always` documented.
10. The two-advice purge: under the cgroup policy's 70% band, `Empty`
    slabs are returned with `MADV_FREE` (lazy, no refault cost while memory
    is plentiful) and above it with `MADV_DONTNEED`; `stats.cgroup.*` and
    the report show which advice is in force; `lazy_free` becomes the
    override. Acceptance: the container churn test still passes at both
    limits; mstress and alloc-test wall time with the lazy band on versus
    off recorded (the refault saving); RSS-as-seen-by-the-cgroup documented
    (`MADV_FREE` pages stay charged until reclaim).
11. Prefaulted fresh slabs: the background thread commits and
    `MADV_POPULATE_WRITE`s (5.14+, probed; fallback: touch) the next fresh
    slab of each class that refilled since the last pass, bounded by one
    slab per class and by the cgroup headroom. Acceptance: the Valkey
    soak's write-phase p99 and page-fault count on the main thread (perf
    sidecar), scorecard within noise.
12. Small ceiling items, each behind its probe: spin-wait instructions in
    `lock.rs` (`wfe`, `tpause`) with the larson16 x86_64 row as the test;
    cache-geometry-sized defaults (capacity ladder and `tcache_max` derived
    from measured L2, with the current constants as the fallback), measured
    on every CPU of task 4; CLDEMOTE on cross-thread frees, measured on
    xmalloc-test and the io-threads soak (Phase 3 prerequisite (d)) and
    dropped if it does not pay.
13. Model-checked slab protocol: the claim, release, `on_live_zero` and
    purge protocol over the bitmap, live count and state byte, in a
    host-side exhaustive interleaving model (Loom-style over the existing
    test scaffolding, or Kani over `bitmap.rs`/the `class.rs` state
    machine), including the task 7 (c) fix and the task 15 group handoff.
    Acceptance: the model reproduces the pre-fix calloc race and passes on
    the current code; it runs in `cargo test --features std`.

Acceptance for the phase:

- The floor: `make test`, `make fuzz` and the container tests are green on
  every kernel cell of task 3, including 5.10 and 64 KiB pages.
- The ceiling: every probed kernel and CPU feature in the feature report
  has a both-ways test, a cost row and a design entry; on the reference
  VMs no default-on feature moves a scorecard row outside noise, and on
  every CPU of task 4 the feature report and the scorecard are recorded.
- The four-way table of task 1 exists and challoc is at or ahead of every
  allocator on the Valkey soak, the Ruby rows and the container tests, and
  within 10% on every other row or attributed.
- Per-CPU caches and tagged pointers are implemented behind their keys with
  suites green and costs recorded; their defaults are the human's decision
  with the numbers in front of them.
- All Phase 0, 1, 1.5 and 2 acceptance criteria still pass with defaults.

Phase 3's prerequisites (c), (d) and (e) stay as written; (a) and (b) are
task 3's cells.

## Phase 3: packaging and rollout

Prerequisites (2026-09-18 review, section 4.3), before any package: (a) one
aarch64 test run on a 64 KiB-page kernel (below), on a throwaway C4A VM
with Ubuntu's `linux-generic-64k` kernel, not on the reference VM, running
`make test`, `make fuzz` and `make valkey STAGE=memefficiency`, with the
`huge.rs` tests using the runtime page size first; (b) one bare-metal 6.14+
run of the `process_madvise` batch path (Docker's seccomp hides it); (c) a
Ruby soak row (minutes, RSS sampled) and a decision on `background_thread`
for Ruby-shaped consumers; (d) one Valkey soak with `io-threads 4`; (e) a
melange pipeline check that `DT_NEEDED` names the library (Wolfi's
`--as-needed`). The 64 KiB run: one aarch64 test run on a 64 KiB-page kernel. Wolfi's
`build-aarch64.env` builds jemalloc with `JEMALLOC_SYS_WITH_LG_PAGE=16`, so
Wolfi expects 64 KiB-page aarch64 hosts; the GCE VMs and the Mac container
are 4 KiB. challoc reads the page size at runtime and its smallest slabs
(4 KiB since 2026-09-17) use the page-group commit/guard/purge path there,
which only the faked-page unit test has exercised.

Tasks:

1. Wolfi package definition `challoc.yaml` producing `challoc`, `challoc-dev`,
   and `challoc-jemalloc-compat` with `libjemalloc.so.2` and `jemalloc.pc`.
2. Valkey package change in Wolfi using the Phase 1 patch and a private
   `quantum8` build of challoc under `/usr/lib/valkey/` (design 13.6: the
   system `libchalloc.so.0` is `quantum16`, like Wolfi's jemalloc), with
   the package test asserting `valkey-server --version` reports challoc
   and that `CONFIG SET activedefrag yes` succeeds.
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
  larson 1/10, xmalloc-test, alloc-test 1, the Valkey model `valkey1`
  (`bench/probes/valkey_model.c`, task 5c) and the churn probe under old,
  new and jemalloc, about two and a half minutes) is for iteration
  decisions only: it says whether a variant is worth the protocol run, and
  its numbers are never recorded in `bench/results/` or used against a
  criterion. The protocol tier (`bench/ab.sh`, `make ab`, `challoc-bench
  ab`: 5 rounds plus warm-up, interleaved variants, sorted scores, median
  RSS) is what acceptance and the results files use, with every variant
  measured in the same run; the criteria above are unchanged. Cached
  stock-allocator baselines (`bench/baseline.sh`, `BASELINES=cached`) save
  time in iteration runs and are revalidated by a jemalloc sanity round
  each time; acceptance runs do not use them.
- The scorecard is the trend record (`bench/scorecard.sh`, `make
  scorecard`, `challoc-bench scorecard <vm>`, task 5c): the fixed row set
  (larson 1/10/16, xmalloc-test, alloc-test 1/N, valkey1, valkey1-free,
  cfrac, espresso, mstress, glibc-simple, redis) under challoc and jemalloc,
  3 interleaved rounds, one line per (date, commit) in
  `bench/results/scorecard-<arch>.md` as percentages against jemalloc plus
  the RSS ratios and the commit's most recent soak. After a change lands it
  runs on both VMs and the two lines are committed with the PROGRESS
  commit. It is a trend, not an acceptance measurement: criteria are still
  judged from same-run protocol files.
- Attribution before fast-path changes: a change to `src/heap.rs`,
  `src/class.rs`, `src/slab.rs` or `src/api/*.rs` that is meant to help a
  target workload is preceded by a per-operation cycle attribution on that
  workload (`perf stat`/`perf annotate` on the VM, `challoc-bench
  quick-perf`, the perf sidecar for the soak, or the sampler), recorded in
  the results file, and the change names the mechanism it attacks. larson
  counters do not stand in for Valkey or Ruby (their working sets are a
  few MiB; Valkey's is 550 MiB).
