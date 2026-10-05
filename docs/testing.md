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
| cgroup container tests | `make test-cgroup-limit`, `make test-cgroup-churn`, `make test-cgroup-lazy` | ~1.5 min | The design 11 policy under a real `--memory` limit: the bands through `mallctl`, RSS falling after a 400 MiB churn with the policy on and not with `cgroup:false`, the `MADV_FREE` purge in the headroom band and its `lazy_band:false` control | The same without a limit is the `api::` layer's `cgroup_*` rows | Docker with cgroup2 memory limits (`scripts/cgroup-tests.sh`, one container per run, swap off) |
| Fuzz, long soaks, comparisons (opt-in) | `make fuzz` (10M ops), `make test-cgroup-oscillate` (5 min), `make check-needed` (`tests/as-needed.sh`), `make test-cgroup-compare` (other allocators, no criterion) | minutes | Long random workloads; the `-Wl,--as-needed` truth for consumers; the RSS curves of jemalloc, mimalloc and tcmalloc under the same limits | | `make bench-build` for mimalloc |
| Consumer suites | `make valkey`, `make ruby` (`tests/consumers/`) | tens of minutes | Valkey's and Ruby's own test suites built on challoc (Valkey with `contrib/valkey/*.patch`, Ruby `--with-jemalloc` against the jemalloc-compat stage) | | The `consumers` image stage |
| loom model checks (opt-in) | `sh verus/loom/run.sh` (loom 0.7.2 with a DPOR fix the script applies; it also checks the loom seam records) | ~15 min with the build | The weak-memory orderings of the lock, the slab bitmap's claim and release, the free path's bit test and the arena bound counts (over the real `vspec` wrappers), and (copies of) the background thread's wake-up and exit, the publication of the allocator state and the free path's watermark and owner reads, under every interleaving loom explores (`src/loom_tests/`, `verus/WEAKMEM.md`) | The rest of the crate, which this build leaves out; C11 behaviours loom does not model (`SeqCst` accesses, load buffering) | The dev container |
| Kani harnesses (opt-in) | `sh verus/kani/run.sh` (`verus/kani/setup.sh` once) | ~1 h per quantum | The seams `verus/SEAMS.md` marks discharged by Kani: each harness checks a trusted item's body against its Verus contract, exhaustively or within the bounds its record under `verus/seams/` names (section "Kani" below) | Everything outside the harnessed items; behaviour past a record's bounds | Kani 0.68.0 and its nightly; up to 20 GB per formatting harness |
| Benchmarks and hardware counters | `make quick`, `make ab` (`docs/benchmarking.md`, `bench/README.md`) | 2 min to hours | Performance | Correctness | The reference machines; never a dev-container number |

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
pass. There are two sources:

- The Rust driver's gate (`Gate::GuardInstall`: the kernel installs guard
  regions). A closed gate prints `SKIP <test> <reason>` on stderr.
- A program's own `skip_note(name, reason)` (`tests/c/common.h`), which
  prints `SKIP <program>[/<mode>] <reason>` on stdout; the driver echoes it
  as `SKIP <test> <program>/<mode> <reason>`. Today: `mseal` under seccomp,
  `thp/{always,auto}` where the sysfs knobs are missing,
  `thp/never-huge-run` when the kernel had no free huge page,
  `cgroup_policy/{bands,high}` and `cgroup/bg` without a limit or a
  writable cgroup mount, `guard_slab/fault` and `pagemap_guard` below
  6.13.

`grep '^SKIP ' <log>` lists every claim a run did not check. With
`CHALLOC_TEST_ALLOW_SKIP` set a skip passes with the note; unset, it fails
the test. `make test` sets it everywhere (`TEST_ALLOW_SKIP ?= 1`): in an
unprivileged container most skips are container or host facts (a
read-only cgroup tree, Docker's seccomp hiding `mseal`, no mTHP size below the PMD), not missing coverage.
`make test TEST_ALLOW_SKIP=` runs strict; real coverage of the cgroup
writes and `mseal` needs the privileged invocation above.

## The C test programs

`tests/c/` holds the programs the `api::` layer runs under
`LD_PRELOAD=target/release/libchalloc.so`, each from a row in
`src/api/c_programs.rs`; every one refuses to run bare (`require_challoc()`),
so a broken preload cannot pass.

| Group | Programs |
|---|---|
| Entry points | `libc_api.c` (the libc functions), `jemalloc_api.c` (the jemalloc and glibc extras, declared weak), `cxx_new.cpp` (every `operator new`/`delete` resolves into challoc), `usable_api.c` (the `challoc_*_usable` functions), `positive.c` (C23 `free_sized`, `free_aligned_sized` and other paths no other program reaches), `jemalloc_link.c` (a link-only `-ljemalloc` consumer of the jemalloc-compat stage) |
| Correctness under load | `align.c`, `calloc_zero.c`, `realloc_tiers.c`, `random_ops.c` (1M random operations on 8 threads), `producer_consumer.c` (pure cross-thread frees), `thread_exit.c`, `tls_threads.c`, `huge_hole.c` |
| Fork | `fork_alloc.c` (allocation in parent, child and grandchild), `fork_signal.c` (fork from a signal handler while the fork handlers hold every lock) |
| Abort conditions | `negative.c`: one invalid operation per mode (invalid, double and mis-sized frees, guard faults); the driver checks the signal and the `challoc:` line on stderr |
| Configuration and report | `conf.c` (every `malloc_conf` source), `tunables.c` (the hardening opt-ins and their controls), `features.c` (`challoc_features()` against the report) |
| Kernel features, both ways | `guard_slab.c`, `pagemap_guard.c` (guard slabs, by fault and through `PAGEMAP_SCAN`), `mseal.c`, `thp.c`, `decay.c` (decay, purge, the background thread) |
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
`reallocarray`. Any byte out of place, a misaligned or undersized result, a
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

## Kani

`verus/SEAMS.md` lists what the Verus proofs take on trust; the seams it
gives to Kani have bounded model-checking harnesses, `#[cfg(kani)]` only,
so no build, test or Verus run sees them: `src/kani/` for `core` and
crate-visible items, `kani_*` modules next to private ones. Each
discharged seam's record under `verus/seams/` names its harness, its
bounds and the gaps of the translation.

```sh
sh verus/kani/setup.sh          # once: Kani 0.68.0 and its toolchain
sh verus/kani/run.sh            # every harness, under both quanta
sh verus/kani/run.sh --harness slice_swap
```

Kani needs its own nightly (the setup installs it) and is not part of
`make test`. The run takes about an hour per quantum, one harness at a
time: the formatting harnesses need up to 20 GB each, so `KANI_JOBS=n`
runs `n` at once only on a machine with that much memory per job. Run
it when a change touches a harnessed item, its contract in
`verus/SEAMS.md` or `src/vspec/`, or a type whose layout an axiom
states. A failing harness names the check that failed; a changed
contract fails the script's first step (each record must still quote
its seam's contract) until the record and its harness follow.

Two Kani 0.68 (CBMC 6.11) modelling bugs shape the harnesses: a
`write_bytes`, `copy` or `copy_nonoverlapping` with a symbolic count
into an array of 8-byte elements writes one element too few (so the
memory harnesses run their lengths as constants), and `<` on a
symbolic `bool` holds of `false < false` (so `arena::key_less`, a
tuple comparison, has no harness). A harness that leans on either would check the model,
not the code.

## RefinedRust

The plain raw-pointer bodies `verus/SEAMS.md` gives to RefinedRust are
proved in Rocq against a RefinedRust specification on the function
itself: `#[cfg_attr(refinedrust, rr::...)]`, which only the
`--cfg refinedrust` translation reads (it registers the `rr` tool in
`src/lib.rs` and changes nothing else). `verus/refinedrust/` holds the
frontend settings, a patch to the frontend (named-constant array lengths,
array refinements, Verus's `Ghost`/`Tracked` as unit), the typing lemmas
the proofs add (`theories/`), the proofs (`proofs/`) and the stdlib
proofs of their callees (`callees/`). Each discharged seam's record
under `verus/seams/` names its proof and the gaps of the translation.

```sh
sh verus/refinedrust/run.sh     # every discharged seam, under both quanta
```

It needs the RefinedRust checkout and its opam switch (Rocq, Iris and
RefinedRust's theories; `RR_ENV`, `RR_ROOT`, default `/opt/rr`), builds
the patched frontend once under `target/refinedrust` (downloading its
nightly with `rustc-dev`), translates the whole crate for each quantum
and checks every proof with dune: about fifteen minutes cold, four
warm. It is not part of `make test`. Run it when a change touches an
annotated function, its contract in `verus/SEAMS.md` or a type it
reads. A changed contract or `rr::` specification fails the first step
(each record must still quote both); a changed body, or a change to the
struct fields a specification owns, makes the proof fail unless it
still holds; and the run fails unless each proof has its template's
statement and `Print Assumptions` of every proof names only
RefinedRust's two axioms (proof irrelevance and functional
extensionality). A newly annotated function needs its proof under
`proofs/`; the script points at the frontend's default.

## Vendored suites

`tests/glibc/malloc/` holds five glibc malloc tests, unmodified, from tag
`glibc-2.44` (LGPL 2.1+, `COPYING.LIB`); `tests/glibc/test-skeleton.c` and
`include/` replace the parts of glibc's tree they reach for.
`tests/mimalloc/test-stress.c` is mimalloc's stress test, unmodified, from
tag `v3.5.2` (MIT, `LICENSE`); `stress.c` wraps it. Each directory's
`README.md` records the tag and commit and how to bump them; the sources
are never edited, the shims are ours. Both are test-only (`NOTICE`).
