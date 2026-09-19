# Phase 2 tasks 6 and 7: the cgroup container tests, aarch64 (GCE C4A)

Library at e7dfb95 (unchanged by these tasks; the programs, driver and
Makefile targets are this iteration's). VM `challoc-bench-arm64`,
c4a-standard-16 (Neoverse V2, 16 vCPUs, 62 GiB), zone `us-east4-a`,
Ubuntu 24.04 / 7.0.0-1011-gcp, 4 KiB pages, Docker 29.1.3, cgroup2 at
`/sys/fs/cgroup` mounted `rw,nsdelegate,memory_recursiveprot`. One
container per run: `docker run --memory=L --memory-swap=L` (swap off) on
the Wolfi dev image, `memory.max` read back as L, `memory.swap.max` 0.
Allocators, all `LD_PRELOAD`, the same binaries: challoc e7dfb95 (release,
quantum 16, defaults; `cgroup:true`, `dirty_decay_ms` 10000, no
background thread), challoc with `MALLOC_CONF=cgroup:false,abort_conf:true`
(the control), Wolfi's jemalloc 5.3.1-r3 (`/usr/lib/libjemalloc.so.2`),
mimalloc v3.5.2 (`target/bench-bin/libmimalloc.so`, bench/build.sh).
Programs: `tests/c/cgroup_churn.c` and `tests/c/cgroup_oscillate.c`
through `scripts/cgroup-tests.sh`; `make test-cgroup` (challoc, both
limits, the oscillation with `CONTROL=1`) and `make test-cgroup-compare`
(jemalloc, mimalloc). Section 4 has the same runs on Docker Desktop
(Mac, 6.10.14-linuxkit) for the outcomes only.

Both programs are single-threaded, call no `malloc_trim`, and run no
background thread; the only purge path is the periodic tick of the slow
path (design 6.4, every 64 slow paths). "Idle" is a sleep in 10 ms steps
with 4096 `malloc(48)`/`free` pairs per step (an idle event loop's
transient allocations, ~16 slow paths, so one tick per ~40 ms): without
it a process that frees and then allocates nothing at all never runs a
decay walk under either setting, because a walk waits for the oldest
`Empty` member to be one curve step old and only a tick can run it. The
trickle is identical in every run below.

## 1. Task 6 (a): churn, allocate, free, idle 2 s

RSS from `/proc/self/statm`; `memory.current` is the cgroup's, which also
charges the page cache of the compile that the first container of each
`make` target runs (later containers reuse the binary), hence 69 MiB
against 13 MiB of RSS in the first row and 10 against 8 in the third.
Sizes at 256m are scaled to the limit (200 MiB churn, 50 MiB threshold:
400 MiB cannot be touched in a 256 MiB cgroup without swap). The mix is
half 1 MiB runs (huge allocator), a quarter 32 KiB (medium class), a
quarter 1 KiB (slab class), every byte written.

| limit | churn / threshold | allocator | RSS peak | RSS right after the frees | RSS at 2 s | RSS under the threshold after | memory.current peak / at 2 s | criterion | result |
|---|---|---|---:|---:|---:|---:|---:|---|---|
| 512m | 400 / 100 MiB | challoc | 403 | 403 | 13 | 46 ms | 460 / 69 | `purge`: under 100 MiB within 2 s | pass |
| 512m | 400 / 100 MiB | challoc `cgroup:false` | 403 | 403 | 319 | never | 405 / 321 | `keep`: not under 100 MiB at 2 s | pass |
| 256m | 200 / 50 MiB | challoc | 202 | 202 | 8 | 79 ms | 204 / 10 | `purge` | pass |
| 256m | 200 / 50 MiB | challoc `cgroup:false` | 202 | 202 | 161 | never | 205 / 163 | `keep` | pass |
| 512m | 400 / 100 MiB | jemalloc 5.3.1 | 409 | 205 | 190 | never | 411 / 192 | (recorded) | 190 MiB kept |
| 256m | 200 / 50 MiB | jemalloc 5.3.1 | 206 | 132 | 124 | never | 207 / 124 | (recorded) | 124 MiB kept |
| 512m | 400 / 100 MiB | mimalloc 3.5.2 | 407 | 407 | 407 | never | 410 / 409 | (recorded) | nothing returned |
| 256m | 200 / 50 MiB | mimalloc 3.5.2 | 207 | 207 | 207 | never | 211 / 209 | (recorded) | nothing returned |

challoc's `stats.cgroup.*` at 2 s: 512m `limit 512 MiB decay_ms 10000
retention_cap 25 MiB empty_bytes 9 MiB full_purges 0` (usage 460 MiB at
the peak, 89.8%: the scaled band, then back under 70% after the purge);
256m `decay_ms 8296` at the peak (204 of 256 MiB, 79.7%) and `10000`,
`retention_cap 12 MiB`, `empty_bytes 4 MiB` at 2 s. The `keep` rows say
`stats.cgroup.* ENOENT (cgroup not tracked)`.

What the rows show. With the policy the first decay walk after the frees
(the first tick of the idle trickle, 46 and 79 ms after the last free)
finds retention (400 or 200 MiB `Empty`) over the 5% cap and purges the
coalesced huge extent and the slab classes' `Empty` slabs whole; what is
left (13 and 8 MiB) is the tick's own slabs and the process. With
`cgroup:false` the same ticks run the 10 s curve: 21% gone at 2 s (403 to
319 MiB), 20% at 256m. jemalloc returns about half at the `free` itself
(the 1 MiB runs; RSS 409 to 205 MiB in the frees) and then 15 MiB in 2 s
of its own decay (`dirty_decay_ms` 10 s, `muzzy` 0): 190 MiB kept.
mimalloc 3 returns nothing in 2 s at either limit.

## 2. Task 6 (b): oscillation 200..480 MiB live, 5 minutes, 512 MiB

Requested bytes swing between 200 and 480 MiB (the mix by bytes: 15% 1 MiB
runs, 20% each 64 KiB, 16 KiB and 4 KiB, 15% 1 KiB, 5% each 256 and 64 B;
about 600k live objects at the top, allocated in 64 KiB batches of one
class, freed at random), each end held 1.5 s with the idle trickle; a cycle
is about 3.1 s. The program's object table (static, ~10 MiB touched) is
part of the RSS. Watchdog 360 s (never fired). `memory.events` counts are
after the run; all were 0 before.

| allocator | outcome | cycles in 300 s | peak `memory.current` | RSS peak | `memory.events` after | RSS at the low end of a cycle |
|---|---|---:|---:|---:|---|---|
| challoc | alive, exit 0 | 98 | 502 MiB | 501 MiB | max 0, high 0, oom_kill 0 | 369..399 MiB right after the frees; settled at 293 MiB (`stats.cgroup.current` at the end; `empty_bytes 20 MiB` under the 25 MiB cap, `full_purges 0`, `pressure_events 0`) |
| challoc `cgroup:false` (control) | alive, exit 0 | 99 | 507 MiB | 506 MiB | max 0, high 0, oom_kill 0 | 485..501 MiB: nothing returned, the `Empty` memory is reused in place by the next climb |
| jemalloc 5.3.1 | OOM-killed at cycle 5 (12 s), exit 137 | 4 | 512 MiB (`memory.peak`) | 507 MiB | max 36, oom 1, oom_kill 1 | 501..507 MiB (nothing returned within the 3 s cycle) |
| mimalloc 3.5.2 | OOM-killed at cycle 2 (6 s), exit 137 | 1 | 512 MiB (`memory.peak`) | 492 MiB | max 54, oom 1, oom_kill 1 | 492 MiB after the first descent |

Criterion (challoc, policy on, not OOM-killed over 5 minutes): pass. RSS
at the top of the swing is 485..498 MiB for 480 MiB requested plus the
10 MiB table, so challoc's own overhead at the peak is 5..8 MiB in a
cgroup with 32 MiB of headroom; the policy's part is the low end, where
retention is brought under the cap during the hold (the frees themselves
tick and purge some, the trickle's ticks the rest) so the next climb
starts from ~293 MiB resident instead of ~490.

The control is not a criterion and it survives too: challoc's `Empty`
slabs and extents stay in their classes and are reused by the next climb,
so the peak is the same with the policy off (507 vs 502 MiB) and the
difference is only what is resident at the bottom (490 vs 293 MiB). The
same reuse does not save jemalloc or mimalloc here: both hold ~500 MiB at
the low end after the first descent and the next climb's requests do not
all fit into what they hold (partially freed slabs of the wrong classes,
metadata), so `memory.current` reaches the limit, the kernel reclaims
(the `max` count) and kills them, at the fifth climb for jemalloc and the
second for mimalloc. Under jemalloc the oscillation is only survivable in
512 MiB with a shorter decay or a `memory.high` below `memory.max`.

## 3. Gates

`make test` in the VM container: 304 + 100 + 23 (default profile;
`--memory=512m` 23), then `make test-cgroup-churn` (the four rows above,
all pass). `challoc-bench quick arm64 OLD=e7dfb95` (the library is
unchanged; the run is the plumbing check): go, larson1 +0.2%, larson10
-0.8%, xmalloc -0.2%, alloc-test1 +0.8%, valkey1 -1.3%, churn compacts
(97.0%). Mac: `make test` 304 + 100 + 23 and the four churn rows.

## 4. The same runs on Docker Desktop (Mac, 6.10.14-linuxkit, aarch64), outcomes only

Not GCE numbers and not compared with them; recorded because it is the
second host the tests run on (`make test` on the Mac includes
`test-cgroup-churn`). Docker Desktop's VM has 1 GiB of swap, which is why
the targets pass `--memory-swap` equal to `--memory`.

| test | allocator | outcome |
|---|---|---|
| churn 512m 400/100 | challoc | RSS 402 -> 13 MiB, under 100 MiB after 48 ms; `memory.current` 453 -> 63 MiB; pass |
| churn 512m 400/100 | challoc `cgroup:false` | RSS 402 -> 329 MiB at 2 s; pass (`keep`) |
| churn 256m 200/50 | challoc | RSS 202 -> 8 MiB after 82 ms; pass |
| churn 256m 200/50 | challoc `cgroup:false` | RSS 202 -> 169 MiB at 2 s; pass (`keep`) |
| churn 512m / 256m | jemalloc 5.3.1 | 424 -> 213 (frees) -> 197 MiB at 2 s; 216 -> 140 -> 131 MiB |
| churn 512m / 256m | mimalloc 3.5.2 | 409 -> 409 MiB; 209 -> 209 MiB (nothing returned) |
| oscillation 512m | challoc | alive, 97 cycles, peak `memory.current` 506 MiB (`memory.peak` 512), `max 1` (one reclaim at the limit: the compile's page cache in the same container), oom_kill 0; `stats.cgroup.current` 304 MiB at the end, `empty_bytes` 25 MiB |
| oscillation 512m | challoc `cgroup:false` | alive, 98 cycles, peak 507 MiB, events 0 |
| oscillation 512m | jemalloc 5.3.1 | OOM-killed at cycle 5, `max 36 oom_kill 1` |
| oscillation 512m | mimalloc 3.5.2 | OOM-killed at cycle 2, `max 53 oom_kill 1` |
