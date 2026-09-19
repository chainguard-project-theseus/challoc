# Phase 2.5 task 15, lane C: rseq time slice extension for lock holders, x86_64

Date 2026-09-19 (23:00..23:50 UTC), lane C (`loop/lane-c`), base commit
299461b (the `old` of every A/B; `new` is the working tree that became the
task 15 code commit). VM `challoc-lane-c-amd64`, c4-standard-16 (Xeon
Platinum 8581C, Emerald Rapids, 16 vCPUs, 2 threads per core),
us-east4-a, `ubuntu-2404-lts-amd64`, the Wolfi container (glibc 2.44,
rust 1.98.1, jemalloc 5.3.1, mimalloc-bench 06a4c4f), deleted at the end.
The aarch64 side is `2026-09-19-aarch64-lane-c-task15-slice-ext.md`.

## 1. The kernel: what has the extension and what does not

The plan assumed the 7.0 reference VMs have the extension. They do not:

| host | kernel | `__rseq_size` | `flags` bit 4 | `prctl(79, SET, 1)` | `rseq_slice_yield` (471) |
|---|---|---|---|---|---|
| lane C4, as created | Ubuntu `7.0.0-1011-gcp` | 33 | clear | `ENOTSUPP` (524) | `ENOSYS` (the `COND_SYSCALL` stub; `W __x64_sys_rseq_slice_yield` in kallsyms) |
| lane C4A, as created | Ubuntu `7.0.0-1011-gcp` (arm64) | 33 | clear | `ENOTSUPP` | `ENOSYS` |
| lane C4, rebuilt | `7.0.14-sliceext` (below) | 33 | set (0x10) | 0, bit 5 set (0x30) | 0 with seccomp unconfined; `ENOSYS` under Docker's default profile |

Ubuntu's config has no `CONFIG_RSEQ_SLICE_EXTENSION` line at all (the
option is `default N`, "If unsure, say N"); x86_64 satisfies its
dependencies (`GENERIC_ENTRY=y`, `HAVE_GENERIC_TIF_BITS=y`,
`HIGH_RES_TIMERS=y`), arm64 satisfies none of the first two on any kernel
through 7.2 and master (`arch/arm64/Kconfig` selects `GENERIC_IRQ_ENTRY`
only), so the feature is x86_64-only for the foreseeable future. To
measure the `on` way the lane built `linux-7.0.14` on the C4 from the
running config (`make localmodconfig`, `CONFIG_RSEQ_SLICE_EXTENSION=y`,
module signing and BTF off, `-sliceext`): 257 s on 16 vCPUs, installed
and booted (`~/logs/kbuild.log` on the VM, gone with it). The raw probe
(`.tmp-probe/rseqprobe.c`, a C program reading `__rseq_size`, the area's
`flags`, `prctl` GET/SET, the syscall, a new thread, a fork child, and a
2 s request/grant loop with a 2000-iteration hold) on that kernel:

```
glibc 2.44 rseq_size=33 offset=-192 flags=0
main before: flags=0x10 (avail=1 enabled=0) prctl GET=0
main SET=0
main after: flags=0x30 (avail=1 enabled=1) prctl GET=1
syscall(471) -> 0                (seccomp unconfined; ENOSYS under Docker's default profile)
new thread before enable: flags=0x10 (avail=1 enabled=0) prctl GET=0
new thread SET=0; after: flags=0x30 (avail=1 enabled=1) prctl GET=1
fork child: flags=0x30 (avail=1 enabled=1) prctl GET=1
main loop: 458752 iters, 17 grants  (262144 iters, 9 grants unconfined)
```

Three facts the design took from this: the enable is per task (a new
thread starts at bit 5 clear and `GET=0`; `rseq_fork` resets a `CLONE_VM`
child's rseq state), a fork child inherits it, and Docker's default
seccomp profile returns `ENOSYS` for the new syscall while passing the
`prctl`, so in a container the grant has to be ended with another syscall
(`sched_yield`; any syscall entry inside a grant revokes it).

## 2. What landed

- `src/slice.rs` (new): the probe (`rseq::probe` for the symbols and the
  size, bit 4 of this thread's `flags` when `__rseq_size >= 32`, the
  `prctl` and bit 5, the syscall once), `configure` from `config::load`
  on the init thread, `acquired` / `releasing` / `released` for the lock,
  the lazy per-thread enable on a thread's first hold (one `flags` load
  per hold, one `prctl` per thread lifetime; a failing `prctl` switches
  the process off), the `rseq_slice_yield` (471, unified table) and
  `sched_yield` (24 / 124) syscalls in `asm!`, the counters (threads
  enabled, grants yielded), and the report words (`slice_ext:off`,
  `no-rseq`, `no-slice_ctrl`, `kernel-disabled`, `prctl-failed`,
  `enabled`). Only `slice_ctrl.request` is written, only for the calling
  thread.
- `src/lock.rs`: `RawLock::lock` and `try_lock` call `slice::acquired`
  after the CAS (never during the wait), `unlock` calls `slice::releasing`
  before the release swap and `slice::released` after it. Every
  `RawLock` in the allocator (class, table, huge, metadata, statistics,
  background), none on the `malloc`/`free` fast path. Off: one relaxed
  load per hold. On: one `flags` load, two byte stores, one byte load.
- `src/config.rs`: `slice_ext:auto|on|off` (default `auto`), `KEYS` 27,
  `Config::apply_slice_ext` at the end of `load`; `src/api/mallctl.rs`
  `cstr_of` knows `on`; `src/api/print.rs` prints `Slice extension: on (N
  threads enabled, M grants yielded via <syscall>)` or `off (<word>)`
  with the `slice_ext:on requested, unavailable` note.
- Tests: `slice::tests::{probe_both_ways_and_off_never_requests,
  grants_are_yielded_under_oversubscription, layout_constants}`,
  `config::tests::slice_ext_values`, `tests/c/slice_ext.c` under
  `slice_ext:on` and `off` (`api::ctests::c_slice_ext_both_ways`).

## 3. Both ways, tests

| host | `make test` | the slice tests said |
|---|---|---|
| C4, `7.0.14-sliceext`, Docker default seccomp | 393 + 117 + 23 passed, `EXIT 0` | `available=true ... verdict=enabled`, `grants end with sched_yield`; `slice_ext.c on`: "main thread enabled 1 (flags bit 5: 1)", "32 threads: 0 enabled before their first allocation, 32 after", fork child inherited, `Slice extension: on (33 threads enabled, ...)`; `off`: nothing enabled, `Slice extension: off (slice_ext:off)`; 64 lock threads: 12 grants yielded in 3 s (the run without CPU hogs; the hogs were added after) |
| C4, same, `--security-opt seccomp=unconfined`, `slice::` serial | `yield_syscall=true`, `grants end with rseq_slice_yield`; the grant test found 0 grants in 3 s with only sleeping futex waiters as competition, which is why it now runs a CPU hog per CPU (the hog version was verified on the C4A fallback path and on the host; the C4 was measuring by then) |
| C4A, Ubuntu `7.0.0-1011-gcp` | 393 + 117 + 23 passed, `EXIT 0`, twice (before and after the test change) | `available=false ... verdict=kernel-disabled`, "64 lock threads and 16 hogs on 16 CPUs, 13974 holds, 0 grants yielded"; `slice_ext.c on`: "main thread enabled -1", "0 enabled before, 0 after", `Slice extension: off (kernel-disabled); slice_ext:on requested, unavailable` |
| Mac host (`cargo test --features std`, no Linux) | 306 passed | `probe()` is `NONE`, the module is off, the config tests cover the key |

## 4. Attribution: the lock-wait share before, and the grants

Sampler (`bench/sampler.c`, SIGPROF at 1000 Hz, 3 runs pooled per cell,
`.tmp-probe/sample.sh` in the container, old = HEAD and new = tree from
`target/quick/{old,new}`), the share of all samples in
`RawLock::lock_slow_with` (the spin and the futex sleep; the `slice::`
functions are inlined into `lock`/`unlock`, and the regex's `slice::`
column matched `core::slice::sort`, not this module, so it is not
reported):

| row | old: `lock_slow_with` | new: `lock_slow_with` | challoc share of samples (old / new) | top challoc symbols |
|---|---|---|---|---|
| larson16 | 9 of 18915 = 0.05% | 18 of 14641 = 0.12% | 39.5% / 38.6% | `free` 61%, `operator new` 33%, `claim_with` 1.6..2.1%, `release_owned` 1.0%, `release_slab` 0.8..1.0% |
| larson10 | 0.05% | 0.03% | 37.9% / 38.2% | `free` 67%, `operator new` 26%, `claim_with` 1.2..1.7%, `release_slab` 1.2..1.3% |
| alloc-testN | 3 of 26825 = 0.01% | 3 of 27560 = 0.01% | 24.7% / 24.9% | `free` 69%, `operator new` 31%, `claim_with` 0.3% |

The mechanism this task attacks, a lock holder preempted mid-hold, has
almost nothing to work on in these rows: the lock-wait path is 0.01..0.12%
of the samples, the slow-path hold is a few dozen instructions, and the
rows run at or below one thread per vCPU. The grant counter (one run per
row under new, read at exit through a preloaded destructor calling
`malloc_stats_print`, `.tmp-probe/atexit_stats.c`) counts the holder
preemptions the kernel deferred:

| row | threads | threads enabled (one `prctl` each) | grants yielded | operations |
|---|---|---|---|---|
| larson16 | 16 | 3095 (larson spawns a thread per round) | 17 | 1.0 G in 3.2 s |
| larson10 | 10 | 2783 | 0 | 1.0 G |
| alloc-testN | 16 | 17 | 0 | - |
| larson32 (2x vCPUs) | 32 | 3353 | 34 | 1.0 G in 3.0 s |
| larson64 (4x vCPUs) | 64 | 3330 | 58 | 1.0 G in 3.0 s |

Even four times oversubscribed, larson's holds are short enough that a
tick lands inside one about once per 17 M operations.

## 5. `quick-perf`: per-operation counters, old vs new (one run each)

`challoc-bench quick-perf amd64 ROWS='larson16 alloc-testN larson10'`, perf
7.0.14 on the host attached to the container (the Ubuntu `perf` wrapper
needed `/usr/lib/linux-tools/7.0.14-sliceext` symlinked to the 7.0.0
tools), load 2.3:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson16 | old | 303.42 | 172.1 | 244.9 | 2.291 | 0.048 |
| larson16 | new | 303.36 | 172.5 | 244.8 | 2.234 | 0.040 |
| larson16 | jemalloc | 335.29 | 155.9 | 201.3 | 2.851 | 0.019 |
| alloc-testN (totals) | old | - | 235.69 G | 268.60 G | 754.4 M | 0.3 M |
| alloc-testN (totals) | new | - | 236.31 G | 268.61 G | 767.5 M | 0.4 M |
| alloc-testN (totals) | jemalloc | - | 213.73 G | 225.68 G | 684.0 M | 0.3 M |
| larson10 | old | 277.27 | 119.2 | 246.5 | 1.559 | 0.015 |
| larson10 | new | 279.22 | 118.3 | 246.5 | 1.541 | 0.014 |
| larson10 | jemalloc | 270.69 | 121.4 | 201.1 | 2.178 | 0.005 |

Instructions per operation are the same to the tenth (244.9 vs 244.8,
246.5 vs 246.5): the per-hold accesses do not register against a refill
or flush. Cycles per operation are within a quarter of a percent.

## 6. `quick`: go

`challoc-bench quick amd64` (old = HEAD 299461b, new = tree, one round,
load 6.75 at start):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---|---|---|---|---|
| larson1 | M ops/s | 43.9 (16.8 MiB) | 46.3 (17.0 MiB) | 46.1 (15.7 MiB) | -5.2% | -0.4% |
| larson10 | M ops/s | 280.8 (96.1 MiB) | 291.8 (96.7 MiB) | 298.1 (93.6 MiB) | -3.8% | +2.2% |
| xmalloc | M frees/s | 333.415 (52.2 MiB) | 329.033 (53.3 MiB) | 329.210 (89.7 MiB) | +1.3% | +0.1% |
| alloc-test1 | s | 3.416 (13.8 MiB) | 3.416 (13.7 MiB) | 3.178 (13.0 MiB) | +0.0% | +7.5% |
| valkey1 | M ops/s | 14.506 (585.2 MiB) | 14.529 (585.1 MiB) | 13.660 (591.9 MiB) | -0.2% | -6.0% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6673 (49.8%) | new compacts | |

`quick: go (new within 5% of old on every row; churn compacts)`. The
single-thread rows' -5.2% / -3.8% are one-round larson spread (the
protocol rows below show the same binaries within noise), not the
extension: larson1 and alloc-test1 take no contended lock.

## 7. `ab`, 5 rounds: within noise

`challoc-bench ab amd64 VARIANTS='old=HEAD new=tree jemalloc'
BENCHES='larson10 larson16 alloc-testN' ROUNDS=5`, the full file with the
header is `2026-09-19-x86_64-lane-c-task15-ab.md`:

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson10 | old | 263.8 274.3 276.2 277.9 285.8 | 97972 |
| larson10 | new | 266.1 269.3 271.2 273.8 279.3 | 96492 |
| larson10 | jemalloc | 274.6 282.5 284.9 289.8 291.0 | 92828 |
| larson16 | old | 292.9 294.0 295.9 306.7 307.6 | 117516 |
| larson16 | new | 293.7 298.5 301.2 302.0 302.3 | 119972 |
| larson16 | jemalloc | 316.3 322.5 323.8 330.1 343.1 | 129156 |
| alloc-testN | old | 4.485 4.544 4.710 4.759 4.801 | 17496 |
| alloc-testN | new | 4.714 4.741 4.746 4.747 4.758 | 17660 |
| alloc-testN | jemalloc | 4.121 4.252 4.344 4.360 4.373 | 17188 |

| program | unit | old | new | jemalloc | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|
| larson10 | M ops/s | 276.2 | 271.2 | 284.9 | +1.8% | -3.1% |
| larson16 | M ops/s | 295.9 | 301.2 | 323.8 | -1.8% | -8.6% |
| alloc-testN | s | 4.710 | 4.746 | 4.344 | +0.8% | -7.8% |

Every old/new pair of sorted lists overlaps (positive = new worse).

## 8. The oversubscribed rows, and `auto` against `off` on one binary

`.tmp-probe/oversub.sh` in the container: larson with 32 and 64 threads
on 16 vCPUs, 5 interleaved rounds after a warm-up, `old`, `new`
(`slice_ext:auto`, on) and `new-off` (the same new binary with
`MALLOC_CONF=slice_ext:off`), M ops/s sorted; load 6.7 at start:

| row | old | new (auto, on) | new (`slice_ext:off`) |
|---|---|---|---|
| larson32 | 314.8 322.6 327.3 331.5 331.8 | 314.6 324.2 328.6 331.5 334.6 | 312.7 326.3 331.8 332.7 332.8 |
| larson64 | 333.0 334.5 338.5 340.9 341.8 | 326.5 332.4 338.2 338.8 339.0 | 321.5 334.3 335.8 337.5 341.0 |

All three lists overlap in both rows; `auto` against `off` on the same
binary is within noise, so what the request costs and what the 34 / 58
grants gained are both below the round-to-round spread.

## 9. Verdict and the default

- Cost: none measurable (instructions per operation identical, cycles
  within 0.3%, `quick` go, `ab` within noise on both architectures, the
  same binary on and off within noise oversubscribed).
- Benefit: none measurable on the rows the plan named, because holder
  preemption is rare there (17 grants per billion larson16 operations;
  0 on larson10 and alloc-testN); the extension can only pay where holds
  are long and the host oversubscribed, and nothing in the suite is.
- Default: `auto` (on where the probe passes), by the plan's rule
  ("within noise or better on both VMs"). Since no GCE Ubuntu kernel and
  no arm64 kernel passes the probe today, `auto` changes nothing on the
  reference VMs or Chainguard OS; it would switch on for an x86_64 host
  whose kernel enables the option, where the same binary measured within
  noise. The human can turn it `off` with the numbers above in front of
  them; `on` exists to make a refused request visible in the report.
