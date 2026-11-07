# Testing challoc

How the test suite is layered, where it runs, what `make test` runs, how
skips are reported, and how to add a test.

## Where tests run

Every test runs inside the Wolfi dev container (`Dockerfile.dev`), the
unit tests included: there is no host-side test run and no `std` feature.
The crate is `no_std`; under `cfg(test)` it links `std` for the test
harness only. The commands below are make targets, or
`make run CMD='...'` for a cargo command, from any machine with Docker.

Both architectures are supported. `make test` runs natively;
`make test PLATFORM=linux/amd64` runs the x86_64 container, emulated on
Apple silicon and much slower (correctness only, never timing). The
`api::` tests run in a second `cargo test` process because the real
allocator's reservations plus a second full layout (the layout tests)
exceed what the x86_64 emulator can track in one process.

## The layers

| Layer | Command | Time | Sees | Cannot see | Needs |
|---|---|---|---|---|---|
| Unit tests | `cargo test -- --skip api::` | ~1 min | Pure logic: size classes, bitmaps, config parsing, the slab-protocol model (`model::`, exhaustive interleavings of the claim, release and purge steps of `src/class/`); and the allocator in-process on real `mmap`/`madvise`/`rseq`: `heap`, `huge`, `class`, `purge`, `sys` (kernel features probed, syscall numbers cross-checked against the C headers through `tests/c/sysnr.c`) | The exported C ABI, `LD_PRELOAD`, another process's view | The dev container |
| API tests under the preload | `cargo test api::`, after `cargo build --release` and `scripts/jemalloc-compat.sh` | ~3 min | The shipped `libchalloc.so` from C: every entry point, the per-feature programs, the negative tests (`src/api/c_programs.rs`, one table row each), the symbol tables (`readelf`), the vendored glibc and mimalloc suites, a short fuzz, the `jemalloc-soname` stage | Memory limits (the cgroup policy's bands), five-minute runs | The dev container; some rows need a 6.13+ kernel (guard slabs), 6.16+ (`PAGE_IS_GUARD`), a writable cgroup mount (`memory.high`), `mseal` outside Docker's seccomp profile; elsewhere they print `SKIP` |
| cgroup container tests | `make test-cgroup-limit`, `make test-cgroup-churn`, `make test-cgroup-lazy` | ~1.5 min | The design 11 policy under a real `--memory` limit: the bands through `mallctl`, held memory (RSS less `LazyFree`) falling after a 400 MiB churn, with an eager purge, with the policy on, and RSS staying with `cgroup:false`, the `MADV_FREE` purge in the headroom band and its `lazy_band:false` control | The same without a limit is the `api::` layer's `cgroup_*` rows | Docker with cgroup2 memory limits (`scripts/cgroup-tests.sh`, one container per run, swap off) |
| loom models | `sh tests/loom/run.sh` (its own crate, `tests/loom`; CI runs it) | ~7 min | Weak-memory (C11) outcomes of copies of the atomics protocols, which call the real `src/vspec` atomic wrappers (compiled over loom's atomics through `src/sync.rs`), every execution loom reaches, under loom 0.7.2 with a DPOR patch (`tests/loom/loom-0.7.2-dpor.patch`): the background thread's wake-up, exit and publication (`background.rs`, `exit.rs`, `publish.rs`), `RawLock` (`lock.rs`), the slab descriptor's lock-protected fields (`slab.rs`), the slab bitmap and `live` (`bitmap.rs`), the free path's reads (`free.rs`), the hand-out watermark and its copy (`watermark.rs`), the arena bound counts (`count.rs`), the arena count and table (`narenas.rs`), the policy position (`position.rs`), the free path's flag byte (`flags.rs`), `Shared`'s publication (`init.rs`), the feature line and the deferred cgroup record (`oncecell.rs`), in `tests/loom/src/`; `tests/loom/tests/pins.rs` checks the source still has the copied lines, and `tests/loom/tests/access.rs` that the shipped source writes the atomics the loom-cited seams name only through their wrappers, with one permission per atomic | The real code but those wrappers (the statics are built in `const` context, which loom cannot construct); C11 beyond the part loom models (stale `Relaxed` and `Acquire` loads within a bounded store history, and no load buffering, `verus/WEAKMEM.md`) | Network for the first `cargo fetch`; `patch` |
| Weak-memory stress (opt-in) | `make run CMD='CHALLOC_STRESS_SECS=60 cargo test --release --lib -- --ignored --test-threads=1 --nocapture stress_'`: `CHALLOC_STRESS_SECS` per test (5 by default) and `CHALLOC_STRESS_THREADS` (the CPUs by default) go inside `CMD`, since the container gets no other variables; CI's on-request `weakmem-stress` check (`.chainguard/ci/weakmem.yaml`) runs them 60 s each on aarch64 | 15 s; 3 min at 60 s each | The shipped orderings on real hardware, in the real code rather than a copy: `RawLock`'s release-to-acquire edge (`lock::tests::stress_lock_edges`), the slab bitmap's release-to-claim edge (`bitmap::tests::stress_bitmap_claim_release_edge`), and the class protocol end to end with cross-thread releases and purges (`class::tests::stress_class_cross_thread_release`); each prints a `STRESS <test> ...` line of counts | A reordering the hardware does not make: on x86_64 (TSO) no weakened ordering tried was caught, and on aarch64 `verus/WEAKMEM.md` ("Hardware stress tests") lists the ones that were not | aarch64 hardware for the reorderings |
| TLS check build | `RUSTFLAGS="--cfg challoc_tls_check"` on the release build, `scripts/jemalloc-compat.sh` and both cargo test groups, then `sh tests/tlscheck/run.sh` (CI: `tlscheck`, `.chainguard/ci/tlscheck.yaml`, quantum 16 on both arches) | ~4 min | Every slot-to-heap use (`heap_from_slot`) checked against a registry of the heaps each thread published (`src/heap/tlscheck.rs`): another thread's heap, a destroyed one, a parent thread's heap in a `fork` child or a forged slot value aborts; at publication, exit and every 64th use, the set-free conjuncts of `Heap::wf`, through the code `heap::heap_at_tests::check` runs for them (`src/heap/heap_facts.rs`). Under it the unit and `api::` tests, then `tests/tlscheck/lifecycle.c` (fork from a busy thread, exit racing cross-thread frees, 20k short threads, `dlopen` of a library with dynamic TLS, a key destructor after challoc's) and `tests/tlscheck/fuzz.c` (120 s by default, `TLSCHECK_FUZZ_SECS`, seed printed, `TLSCHECK_FUZZ_SEED`) | The shipped build (the cfg is off there; the hooks compile out); the set-based `wf` conjuncts and the zero prefix; more than 4096 thread heaps live at once (the registry aborts) | The dev container, or the guest packages of `tlscheck.yaml` |
| GenMC harnesses | `sh tests/genmc/run.sh` (`tests/genmc`; CI runs it on changes to the wrappers, `src/sync.rs`, the files the harnesses copy steps from, `Cargo.toml`, `tests/genmc/` or `tests/loom/src/vspec.rs`) | ~1 min | Weak-memory outcomes of GenMC ports of loom models, which call the real `src/vspec` atomic wrappers (compiled over `core`'s atomics through `src/sync.rs`), every execution, with no bound on store history, under RC11, IMM and ARMv8 (each case in `tests/genmc/cases` names its models): the free path's flag byte (`flags.rs`), the hand-out watermark and its copy (`watermark.rs`), the arena bound counts (`count.rs`), the arena count and table (`narenas.rs`), the slot state byte (`slotstate.rs`), the slab bitmap and `live` (`bitmap.rs`), two shapes loom cannot produce, load buffering and a read past loom's 7-store history (`litmus.rs`), and, over copies of the code's own steps, publishing `Shared` with up to two waiters at once (`init.rs`) and the background thread's wake-up, exit and publication through a futex stand-in, with spurious wake-ups (`background.rs`, `exit.rs`, `publish.rs`; a lost wake-up or a waiter left spinning is a reported liveness violation); each control must fail with its expected report; `tests/loom/tests/pins.rs` (in the loom check, every pull request) checks that each copied source line and the harness's copy of it still match, and that every `pass` case sets the orderings and switches its `GENMC_SHIPPED` entry lists to the source's | The real code but those wrappers (the pins match whole lines, so a step that is not a line-for-line copy, and the order of steps split across files, rest on review); the futex (`RawLock` is a `Mutex` here, and the background thread's wait is a stand-in that assumes no lost wake-up given the value check), timeouts (`verus/WEAKMEM.md`, "GenMC") | The `genmc` package (it brings rust-1.99, whose LLVM 22 GenMC is built against); network for the first `cargo build` |
| Fuzz, long soaks, comparisons (opt-in) | `make fuzz` (10M ops), `make test-cgroup-oscillate` (5 min), `make check-needed` (`tests/as-needed.sh`), `make test-cgroup-compare` (other allocators, no criterion) | minutes | Long random workloads; the `-Wl,--as-needed` truth for consumers; the RSS curves of jemalloc, mimalloc and tcmalloc under the same limits | | `make bench-build` for mimalloc |
| Consumer suites | `make valkey`, `make ruby` (`tests/consumers/`) | tens of minutes | Valkey's and Ruby's own test suites built on challoc (Valkey with `contrib/valkey/*.patch`, Ruby `--with-jemalloc` against the jemalloc-compat stage) | | The `consumers` image stage |
| Benchmarks and hardware counters | `make quick`, `make bench-launch SUITE=full` (`docs/benchmarking.md`, `bench/README.md`) | 2 min to hours | Performance | Correctness | The reference machines; never a dev-container number |

`make test` runs, in order: `cargo build --release`,
`scripts/jemalloc-compat.sh`, the unit tests, the `api::` tests, then
`test-cgroup-limit`, `test-cgroup-churn` and `test-cgroup-lazy`. It does
not run `make fuzz` (the `api::` layer runs a 400k-operation slice of the
same driver, `c_fuzz_short`), `make test-cgroup-oscillate`,
`make check-needed` or `make test-cgroup-compare`. `make test-cgroup`
runs the three cgroup targets including the oscillation, and
`make test-all` runs `test`, `fuzz`, `test-cgroup-oscillate` and
`check-needed`: everything that runs locally.

To run one test: `make run CMD='cargo test <filter> -- --nocapture'`, or,
for an `api::` row, build the library first:
`make run CMD='cargo build --release && CHALLOC_TEST_ALLOW_SKIP=1 cargo test api::c_programs::c_<prog> -- --nocapture'`.

## Both quanta

Every cargo build and test in the Makefile takes the quantum feature from
`QUANTUM` (`16` by default, the system library; `8`, Valkey's private
build). `make test QUANTUM=8` runs the whole suite against the 8-byte
class table: the fixtures are quantum-aware, the C programs are compiled
with `-DQUANTUM=8` (below), and the cgroup tests keep one binary directory
per quantum (`target/cgroup-tests/q<n>/`), so the two runs never reuse
each other's binaries. Run both when a change touches size classes,
`src/class/` or the jemalloc-compat stage. `make fuzz QUANTUM=8` is the
long fuzz on the same table.

## Privileged runs

The default container hides some features from the tests: Docker's
seccomp profile filters `mseal`, and the container's
cgroup tree is mounted read-only (no `memory.high` write). `EXTRA_DOCKER_FLAGS` reaches every container, the memory-limited
ones included, so one invocation runs the whole suite with them visible:

```sh
make test EXTRA_DOCKER_FLAGS='--privileged --security-opt seccomp=unconfined --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw'
```

The `Kernel and CPU features:` line in the log then reads `mseal=yes`. On
a host whose cgroup2 is mounted `nsdelegate` (systemd hosts), a
container's root cgroup still refuses the `memory.high` write
(`EPERM`) even under `--privileged`; those rows keep their skip there.

## Skips

A test that cannot check its claim on this host is a *skip*, never a silent
pass. There are three sources:

- The Rust driver's gate (`Gate::GuardInstall`: the kernel installs guard
  regions). A closed gate prints `SKIP <test> <reason>` on stderr.
- A program's own `skip_note(name, reason)` (`tests/c/common.h`), which
  prints `SKIP <program>[/<mode>] <reason>` on stdout; the driver echoes it
  as `SKIP <test> <program>/<mode> <reason>`. Today: `mseal` under seccomp,
  `thp/{always,auto}` where the sysfs knobs are missing,
  `thp/never-huge-run` when the kernel had no free huge page,
  `resource/{mlock_huge,mlockall,map_count}` when `mlock`/`mlockall` is
  refused, `resource/map_count` without guard slabs (below 6.13 or
  `guards:false`), `cgroup_policy/{bands,high}` and `cgroup/bg` without a limit or a
  writable cgroup mount, `mlock_purge/<mode>` when `mlock` is refused (a
  `RLIMIT_MEMLOCK` under 1 MiB, 2 MiB for the decay modes) and
  `mlock_purge/{decay,background}` when the host is too slow to see the
  back-off (a 400 ms wait taking over 900 ms),
  `guard_slab/fault` and `pagemap_guard` below 6.13.
- A unit test whose host lacks a kernel feature or a privilege
  (`testutil::mlock_or_skip` and `testutil::skip`: the purge tests over
  locked memory, `mseal` below 6.10 or under seccomp, `MADV_GUARD_INSTALL`
  below 6.13, `PR_SET_VMA_ANON_NAME` without `CONFIG_ANON_VMA_NAME`, the
  cgroup sampling tests without a tracked cgroup, the rseq pinned check
  on one CPU) prints `SKIP <test> <reason>` on stderr, and fails unless
  `CHALLOC_TEST_ALLOW_SKIP` is set. A unit test whose claim needs a CPU
  feature the fallback path's tests cover (`testutil::print_skip`: the
  pointer-authentication tests on a CPU without it) prints the same line
  and passes.

`grep '^SKIP ' <log>` lists every claim a run did not check. With
`CHALLOC_TEST_ALLOW_SKIP` set a skip passes with the note; unset, it fails
the test. `make test` sets it everywhere (`TEST_ALLOW_SKIP ?= 1`): in an
unprivileged container most skips are container or host facts (a
read-only cgroup tree, Docker's seccomp hiding `mseal`, no mTHP size below the PMD), not missing coverage.
`make test TEST_ALLOW_SKIP=` runs strict; real coverage of the cgroup
writes and `mseal` needs the privileged invocation above.

## Process-wide state

libtest runs the tests of one process in parallel, so a test must not
assume nobody else touches what it reads. Two tools in `src/testutil.rs`:

- `global_state()`, a guard for the `api::` tests that write or assert a
  setting of the process-wide allocator (decay, the background thread
  request, the stats epoch). Tests that only allocate and free do not
  take it.
- `in_fork_child(what, timeout_ms, f)`, for a check that must see no
  other thread (every lock free after the fork handlers) or that writes
  a static the rest of the process must never see (the THP decision and
  reservation slots, the purge advice mirror). The child has one thread
  and its writes die with it.

The placement of `malloc` and `free` (the `global_asm!`s in
`src/api/libc.rs`: 0/0 mod 64 on `x86_64`, 16/28 on `aarch64`) is
checked on the shipped library by
`api::libc::tests::malloc_and_free_entries_are_pinned` (`nm -D`).

## The C test programs

`tests/c/` holds the programs the `api::` layer runs under
`LD_PRELOAD=target/release/libchalloc.so`, each from a row in
`src/api/c_programs.rs`; every one refuses to run bare (`require_challoc()`),
so a broken preload cannot pass.

| Group | Programs |
|---|---|
| Entry points | `libc_api.c` (the libc functions), `jemalloc_api.c` (the jemalloc and glibc extras, declared weak), `cxx_new.cpp` (every `operator new`/`delete` resolves into challoc), `usable_api.c` (the `challoc_*_usable` functions), `export_results.c` (every nullable export returns its entry point's result: NULL with its `errno` for an impossible request, a block for a small one), `positive.c` (C23 `free_sized`, `free_aligned_sized` and other paths no other program reaches), `jemalloc_link.c` (a link-only `-ljemalloc` consumer of the jemalloc-compat stage), `small_stack.c` (the entry points, the statistics among them, on a `PTHREAD_STACK_MIN` thread with 6 KiB of it used, each in a fresh process; with the frame-size test `every_frame_fits_the_budget`, design 13.8) |
| Correctness under load | `align.c`, `calloc_zero.c`, `zero_trust.c` (every zeroing entry point after a heap bug wrote memory not yet handed out), `realloc_tiers.c`, `random_ops.c` (1M random operations on 8 threads), `producer_consumer.c` (pure cross-thread frees), `thread_exit.c`, `tls_threads.c`, `huge_hole.c` |
| Resource limits | `resource.c` (`RLIMIT_DATA` and `RLIMIT_AS` from the start, `arenas.create` exhaustion, `narenas:1000`, `mlock`, `mlockall`, an exhausted `vm.max_map_count` and a thread cancelled inside a background-thread restart or `malloc_info`: NULL/`ENOMEM` past a limit, never an abort, and recovery once the pressure lifts; design 14.1) |
| Fork | `fork_alloc.c` (allocation in parent, child and grandchild), `fork_signal.c` (fork from a signal handler while the fork handlers hold every lock), `atfork_early.c` (atfork handlers that allocate, registered from `.preinit_array` before challoc's constructor, so they run inside the fork window; with `atfork_lib.c`, a linked library's, in each link order), `dlopen_late.c` (challoc loaded by `dlopen` into a glibc-malloc process does not initialise; run bare), `fork_window.c` (the background thread is refused in the fork window, through `mallctl("background_thread")`), `fork_bg_start.c` (a fork child of a process with a background thread has none, and turning it on there starts exactly one), `fork_window_lazy.c` (a child handler's 96 MiB burst in the fork window starts no thread there through the lazy start; never more than one after), `fork_spin.c` (a child handler reads the feature line and the cgroup record another parent thread was building at the fork, and does not hang), `fork_arena.c` (no arena is built in the fork window), `atfork_pool.c` (a prepare handler from a constructor stops and joins allocating workers: challoc's constructor registered first, so it runs before challoc locks; a pool in a linked library with challoc preloaded still deadlocks, design 9), `fork_init.c` (a fork while another thread initialises a late-loaded challoc: every child's first call completes; and a child handler registered before challoc's that makes the first allocation in the child passes the gate; run bare), `fork_first.c` (a fork before the first allocation, whose `.preinit_array` prepare or child handler initialises challoc: the forking thread passes the gate, in the child too) |
| Abort conditions | `negative.c`: one invalid operation per mode (invalid, double and mis-sized frees, guard faults); the driver checks the signal and the `challoc:` line on stderr |
| Configuration and report | `conf.c` (every `malloc_conf` source), `tunables.c` (the hardening opt-ins and their controls), `features.c` (`challoc_features()` against the report) |
| Kernel features, both ways | `guard_slab.c`, `pagemap_guard.c` (guard slabs, by fault and through `PAGEMAP_SCAN`), `mseal.c`, `thp.c`, `decay.c` (decay, purge, the background thread), `setxid.c` (the setuid-family calls glibc broadcasts to every thread, while the background thread starts and stops), `bg_restart.c` (a restart of the background thread from a thread with no heap), `mlock_purge.c` (purges that `madvise` refuses on `mlock`ed memory) |
| cgroup | `cgroup.c`, `cgroup_policy.c` (the `api::` rows and `test-cgroup-limit`); `cgroup_churn.c`, `cgroup_lazy.c`, `cgroup_oscillate.c` (the memory-limited container runs, `scripts/cgroup-tests.sh`) |
| Linking | `as_needed.c`, `as_needed_lib.c` (`tests/as-needed.sh`, `make check-needed`) |
| Unit-test helper | `sysnr.c` (prints the C headers' syscall numbers for the `sys::nr` test) |

### One compiler line

Every C test program is compiled by `scripts/ctest-cc.sh`: `-O2 -g -Wall
-Wextra -Werror -pthread -DQUANTUM=<n> -Itests/c`, then the caller's flags,
the source, `-o`, and `-ldl` last. `QUANTUM` is read from the built library
(a probe reads `arenas.quantum` through `mallctl`, cached next to the `.so`),
so a `quantum8` build compiles its programs for 8 without anyone editing a
flag; `QUANTUM=8` in the environment overrides the probe, `CHALLOC_LIB`
names another library. `src/api/c_programs.rs`, `scripts/cgroup-tests.sh`,
`tests/as-needed.sh` and the Makefile's `fuzz` target all go through it.
The Rust driver compiles each distinct (directory, program, flags) once
per `cargo test` process into `target/ctests/`.

### Adding a C test

1. Write `tests/c/<prog>.c` on `common.h`: `require_challoc()` first (the
   bare run must fail), `CHECK`/`REQUIRE` for the invariants, `skip_note`
   for a claim the host cannot support, `return finish("<prog>")` last
   (prints `ok <prog>`). Helpers there: `fill`/`check_fill`/`is_zero`,
   `rng_next`, `sleep_ms`, `now_ms`, `rss_bytes`, `fork_and_wait`,
   `expect_signal`, `mallctl_dl`, and every non-standard entry point
   declared weak (`mallctl`, `mallocx`, `sdallocx`, `free_sized`, ...) plus
   the `MALLOCX_*` flags.
2. Add one entry to a `ctests!` block in `src/api/c_programs.rs`:
   `c_<prog> => [ CTest { prog: "<prog>", args: &["mode"], conf: Some("key:value"), ..BASE } ];`
   with a doc comment saying what the row proves. A row that must abort
   uses `Expect::Abort { signal, stderr_contains }` (the `negative!` macro
   for modes of `negative.c`); `stdout_has`/`stdout_lacks` pin the program's
   own output; `gate` names the host feature it needs. Several rows under
   one name run in order and share the compiled binary.
3. Run it: `make run CMD='cargo build --release && CHALLOC_TEST_ALLOW_SKIP=1 cargo test api::c_programs::c_<prog> -- --nocapture'`.

A probed kernel or CPU feature gets a row on a host that has it and a row
(or the same row's other branch) on a host that lacks it. A test that
needs a memory limit is a mode of `scripts/cgroup-tests.sh` and a
Makefile target instead (`test-cgroup-lazy` is the pattern). Measurement
programs with no pass/fail criterion live in `bench/probes/`, not here; a
`report`/`--any-allocator` mode of a test program is a bench probe too and
says so in its header.

## Fuzz

`fuzz/fuzz.c` is a seeded, weighted random exerciser of the libc entry
points: every thread keeps a table of live blocks filled with a pattern
derived from a per-block seed, verified before every free, realloc,
cross-thread hand-off, random inspection and at the end. Sizes cover
`malloc(0)`, the small and medium classes and multi-megabyte huge runs;
allocation rotates through `malloc`, `calloc`, `aligned_alloc`,
`posix_memalign` and `memalign` (alignments up to 64 KiB), and `realloc`
covers grow, shrink, `realloc(p, 0)`, `realloc(NULL, n)` and
`reallocarray`. Half the table's and mailbox's frees are sized
(`free_sized`, `free_aligned_sized` and `sdallocx`, with the block's own
size or, unaligned, one from it up to the usable size), so each cell's
configuration also reaches its sized-free route. Any byte out of place, a
misaligned or undersized result, a
non-zero `calloc` or lost accounting fails the run with its seed printed.

```sh
make fuzz                                    # 10M operations on 4 threads, seed 0xC4A110C
make fuzz FUZZ_OPS=100000000 FUZZ_THREADS=16 FUZZ_SEED=random
make fuzz QUANTUM=8 CHALLOC_CONF=zero_on_free:true,slot_randomize:true
```

`FUZZ_TABLE` (2048) is the live blocks per thread; `FUZZ_SEED=random`
draws a seed from `getrandom`, and the seed in use is always printed
first, so a failure replays with `FUZZ_SEED=<seed>` and the same thread
count. `make test` runs 400k-operation slices of the same driver, once
under the defaults and once under each hardening key (`c_fuzz_short*`).

## Vendored suites

`tests/glibc/malloc/` holds five glibc malloc tests, unmodified, from tag
`glibc-2.44` (LGPL 2.1+, `COPYING.LIB`); `tests/glibc/test-skeleton.c` and
`include/` replace the parts of glibc's tree they reach for.
`tests/mimalloc/test-stress.c` is mimalloc's stress test, unmodified, from
tag `v3.5.2` (MIT, `LICENSE`); `stress.c` wraps it. Each directory's
`README.md` records the tag and commit and how to bump them; the sources
are never edited, the shims are ours. Both are test-only (`NOTICE`).
