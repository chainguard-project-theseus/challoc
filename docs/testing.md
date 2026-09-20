# Testing challoc

How the test suite is layered, what each layer can and cannot see, what
`make test` runs, how skips are reported, and how to add a test.

## The pyramid

| Layer | Command | Time | Sees | Cannot see | Needs |
|---|---|---|---|---|---|
| Host unit tests | `cargo test --features std` (or `make test-host`) | ~30 s | Pure logic: size classes, bitmaps, config parsing, the slab-protocol model (`model::`, exhaustive interleavings of `class.rs`'s steps) | Anything Linux: `src/api`, `src/sys`, the real heap; the compiled `.so` | Rust on the host (macOS is fine) |
| Container unit tests | `cargo test --features std -- --skip api::` in the dev container | ~1 min | The allocator in-process on real `mmap`/`madvise`/`rseq`: `heap`, `huge`, `class`, `purge`, `sys` (kernel features probed, syscall numbers cross-checked against the C headers, `tests/c/sysnr.c`) | The exported C ABI, `LD_PRELOAD`, another process's view | Docker (`Dockerfile.dev`) |
| API tests under the preload | `cargo test --features std api::` in the container, after `cargo build --release` and `scripts/jemalloc-compat.sh` | ~3 min | The shipped `libchalloc.so` from C: every entry point (`tests/c/libc_api.c`, `jemalloc_api.c`, `cxx_new.cpp`), the per-feature programs and the negative tests (`src/api/ctests.rs`, one table row each), the symbol tables (`readelf`), the vendored glibc and mimalloc suites, a short fuzz, the `jemalloc-soname` stage | Memory limits (the cgroup policy's bands), five-minute runs | Docker; some rows need a 6.13+ kernel (guard slabs), 6.16+ (`PAGE_IS_GUARD`), 6.18+ (`PR_THP_DISABLE_EXCEPT_ADVISED`), `mm_cid` in glibc's rseq area (per-CPU mode), a writable cgroup mount (PSI, `memory.high`), `mseal` outside Docker's seccomp profile; elsewhere they print `SKIP` |
| cgroup container tests | `make test-cgroup-limit`, `make test-cgroup-churn`, `make test-cgroup-lazy` | ~1.5 min | The design 11 policy under a real `--memory` limit: the bands through `mallctl`, RSS falling after a 400 MiB churn with the policy on and not with `cgroup:false`, the `MADV_FREE` purge in the headroom band and its `lazy_band:false` control | The same without a limit is the `api::` layer's `cgroup_*` rows | Docker with cgroup2 memory limits (`scripts/cgroup-tests.sh`, one container per run, swap off) |
| Guard pages seen from `/proc` | `make test-pagemap-guard` | ~10 s | The guard slabs of four classes through `ioctl(PAGEMAP_SCAN)` without touching them, both ways (`guards:false` as the control) | | Docker; 6.7+ for the scan, 6.16+ for `PAGE_IS_GUARD` (the program names the fallback it took) |
| Fuzz, long soaks, comparisons (opt-in) | `make fuzz` (10M ops), `make test-cgroup-oscillate` (5 min), `make check-needed` (`tests/as-needed.sh`), `make test-cgroup-compare` (other allocators, no criterion) | minutes | Long random workloads; the `-Wl,--as-needed` truth for consumers; the RSS curves of jemalloc, mimalloc and tcmalloc under the same limits | | Docker; `make bench-build` for mimalloc |
| Benchmarks and hardware counters | `make quick`, `make ab`, `challoc-bench` (`bench/README.md`) | 2 min to hours | Performance | Correctness | The GCE VMs; never a Mac number |

`make test` runs, in order: `cargo build --release`, `scripts/jemalloc-compat.sh`,
the container unit tests, the `api::` tests, then `test-cgroup-limit`,
`test-cgroup-churn`, `test-cgroup-lazy` and `test-pagemap-guard`. It does
not run `make fuzz` (the `api::` layer runs a 400k-operation slice of the
same driver, `c_fuzz_short`), `make test-cgroup-oscillate`, `make check-needed`
or `make test-cgroup-compare`. `make test-cgroup` runs the three cgroup
targets including the oscillation.

## One compiler line

Every C test program is compiled by `scripts/ctest-cc.sh`: `-O2 -g -Wall
-Wextra -Werror -pthread -DQUANTUM=<n> -Itests/c`, then the caller's flags,
the source, `-o`, and `-ldl` last. `QUANTUM` is read from the built library
(a probe reads `arenas.quantum` through `mallctl`, cached next to the `.so`),
so a `quantum8` build compiles its programs for 8 without anyone editing a
flag; `QUANTUM=8` in the environment overrides the probe, `CHALLOC_LIB`
names another library. `src/api/ctests.rs`, `scripts/cgroup-tests.sh`,
`scripts/pagemap-guard-test.sh`, `tests/as-needed.sh` and the Makefile's
`fuzz` target all go through it. The Rust driver compiles each distinct
(directory, program, flags) once per `cargo test` process into
`target/ctests/`.

## Skips

A test that cannot check its claim on this host is a *skip*, never a silent
pass. There are two sources:

- The Rust driver's gate (`Gate::GuardInstall`: the kernel installs guard
  regions). A closed gate prints `SKIP <test> <reason>` on stderr.
- A program's own `skip_note(name, reason)` (`tests/c/common.h`), which
  prints `SKIP <program>[/<mode>] <reason>` on stdout; the driver echoes it
  as `SKIP <test> <program>/<mode> <reason>`. Today: `mseal` under seccomp,
  `percpu/percpu` and `slice_ext/on` on kernels without the feature,
  `thp/{always,auto,process}` where the sysfs knobs or the 6.18 `prctl`
  are missing, `cgroup_policy/{bands,high}` and `cgroup/bg` without a
  limit or a writable cgroup mount, `guard_slab/fault` and `pagemap_guard`
  below 6.13.

`grep '^SKIP ' <log>` lists every claim a run did not check. With
`CHALLOC_TEST_ALLOW_SKIP` set (the Makefile sets it for the local targets)
a skip passes with the note; unset, it fails the test. The reference VMs
(6.13+ kernels, `mm_cid`, privileged containers) run without it, so a
feature that stops being checked there fails loudly.

## Adding a C test

1. Write `tests/c/<prog>.c` on `common.h`: `require_challoc()` first (the
   bare run must fail), `CHECK`/`REQUIRE` for the invariants, `skip_note`
   for a claim the host cannot support, `return finish("<prog>")` last
   (prints `ok <prog>`). Helpers there: `fill`/`check_fill`/`is_zero`,
   `rng_next`, `sleep_ms`, `now_ms`, `rss_bytes`, `fork_and_wait`,
   `expect_signal`, `mallctl_dl`, and every non-standard entry point
   declared weak (`mallctl`, `mallocx`, `sdallocx`, `free_sized`, ...) plus
   the `MALLOCX_*` flags.
2. Add one entry to a `ctests!` block in `src/api/ctests.rs`:
   `c_<prog> => [ CTest { prog: "<prog>", args: &["mode"], conf: Some("key:value"), ..BASE } ];`
   with a doc comment saying what the row proves. A row that must abort
   uses `Expect::Abort { signal, stderr_contains }` (the `negative!` macro
   for modes of `negative.c`); `stdout_has`/`stdout_lacks` pin the program's
   own output; `gate` names the host feature it needs. Several rows under
   one name run in order and share the compiled binary.
3. Run it: `make run CMD='cargo build --release && CHALLOC_TEST_ALLOW_SKIP=1 cargo test --features std api::ctests::c_<prog> -- --nocapture'`.

A test that needs a memory limit is a mode of `scripts/cgroup-tests.sh`
and a Makefile target instead (`test-cgroup-lazy` is the pattern).
Measurement programs with no pass/fail criterion live in `bench/probes/`
(`fork_cost.c`, `churn.c`), not here; a `report`/`--any-allocator` mode of a
test program is a bench probe too and says so in its header.

## Vendored suites

`tests/glibc/malloc/` holds five glibc malloc tests, unmodified, from tag
`glibc-2.44` (LGPL 2.1+, `COPYING.LIB`); `tests/glibc/test-skeleton.c` and
`include/` replace the parts of glibc's tree they reach for.
`tests/mimalloc/test-stress.c` is mimalloc's stress test, unmodified, from
tag `v3.5.2` (MIT, `LICENSE`); `stress.c` wraps it. Each directory's
`README.md` records the tag and commit and how to bump them; the sources
are never edited, the shims are ours.
