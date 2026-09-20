# 2026-09-20: prefaulted fresh slabs behind `prefault` (Phase 2.5 task 11, lane B), aarch64

- change: the background-thread and commit-path half of the task (lane B; the claim side is
  lane A's after the merge): `purge::prefault::pass` after each decay pass of the background
  thread, `ClassState::prefault_next` (the refill's own `next_slab`, `MADV_POPULATE_WRITE` or a
  touch per page under the class lock, published `Purged`), `prefault_due` over
  `ClassState::refilled`, `Policy::prefault_allowed` (the cgroup band bound), the
  `populate_write` probe and feature-line key, `stats.prefault.*`, the `Prefault:` report line,
  `tests/c/prefault.c`; design 8.2. Key `prefault:true` (lane C's, default `false`), active only
  with `background_thread:true`. Base 48af80c (main with lane C's keys).
- VM: `challoc-lane-b-arm64` (c4a-standard-16, Axion / Neoverse V2, `us-east4-a`, Ubuntu 24.04,
  kernel `7.0.0-1011-gcp`, 4 KiB pages, `populate_write=yes`), the dev container (Wolfi, glibc
  2.44); created for this task and deleted at its end. The tree measured is the working tree
  that became the task's code commit.
- what moved: cold paths only. `SlabTable::commit_slab` gained one relaxed load per fresh
  slab commit (`prefault::note_commit`; a `futex_wake` when the key is on); `ClassState` gained
  two `AtomicU64`s in its tail line (the pass's `refilled` snapshot and its count; the struct's
  size and the hot lines are unchanged, the `const` asserts hold). The pop, push, refill
  selection and flush are byte-identical in source. The pass itself runs on the background
  thread.

## 1. `quick`, the library change under the default configuration (go)

`challoc-bench quick arm64` (old = HEAD 48af80c, new = tree, one round, load 1.54 at start):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.9 (16.1 MiB) | 42.0 (15.9 MiB) | 45.2 (18.3 MiB) | -0.2% | +7.6% |
| larson10 | M ops/s | 363.5 (112.3 MiB) | 361.3 (110.1 MiB) | 389.3 (133.3 MiB) | +0.6% | +7.7% |
| xmalloc | M frees/s | 583.055 (53.8 MiB) | 581.923 (55.3 MiB) | 489.139 (80.5 MiB) | +0.2% | -15.9% |
| alloc-test1 | s | 2.669 (13.0 MiB) | 2.650 (13.0 MiB) | 2.500 (12.3 MiB) | -0.7% | +6.0% |
| valkey1 | M ops/s | 7.340 (584.2 MiB) | 7.354 (584.3 MiB) | 7.204 (585.3 MiB) | -0.2% | -2.0% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3461 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go (new within 5% of old on every row; churn compacts)`. Positive = new worse. An
earlier run of the same tier on the tree before the pass's poll and two test fixes (cold code
only) read +0.2 / +0.1 / +0.4 / -0.5 / -2.7% on the same rows, also go.

## 2. `quick`, the opt-in against the thread alone (go)

`challoc-bench quick arm64 OLD_CONF=background_thread:true NEW_CONF=prefault:true,background_thread:true`
(both libraries with the background thread, so the row prices the pass and not the thread; one
round, load 2.65 at start):

| row | unit | old (thread) | new (thread + prefault) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.8 (16.1 MiB) | 41.9 (19.8 MiB) | 45.1 (17.9 MiB) | -0.2% | +7.6% |
| larson10 | M ops/s | 366.9 (112.3 MiB) | 365.5 (134.7 MiB) | 387.2 (131.4 MiB) | +0.4% | +5.9% |
| xmalloc | M frees/s | 582.587 (58.9 MiB) | 584.228 (56.0 MiB) | 491.647 (80.4 MiB) | -0.3% | -15.8% |
| alloc-test1 | s | 2.665 (13.0 MiB) | 2.655 (15.0 MiB) | 2.505 (12.3 MiB) | -0.4% | +6.0% |
| valkey1 | M ops/s | 7.208 (584.2 MiB) | 7.056 (585.9 MiB) | 7.047 (585.5 MiB) | +2.2% | -0.1% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go`. What the opt-in costs on these rows is RSS, not time: one prefaulted slab per
`(arena, class)` that refilled sits resident ahead of the refill (larson1 +3.7 MiB, larson10
+22 MiB over 10 threads' arenas, alloc-test1 +2 MiB); the valkey1 +2.2% is a single round
(section 1 read -0.2% for the same binaries without the key, and the row's one-round spread is
of that size). These rows are not the workload the change is for: larson and alloc-test hold a
fixed working set and refault nothing after the warm-up; the Valkey soak below is.

## 3. The both-ways test on this kernel

`tests/c/prefault.c` (`api::ctests::c_prefault_both_ways`) under `make test` on the VM: see
section 5. On the Mac's Docker VM (linuxkit 6.10, `populate_write=yes`) the same program read
`on`: 16 of 16 pages of the next fresh 64 KiB slab resident before any allocation touched it,
the one after it 0, `stats.prefault passes 10 (active 8) slabs 12 bytes 786432 wakes 210
fallbacks 0 skipped 0`, and `1024 of the next 2048 allocations came from the prefaulted slab`
(the whole slab); `off`: 0 of 16 resident, every counter 0, `Prefault: off (prefault:false)`;
`inert` (`prefault:true` without the thread): 0 of 16, `Prefault: off (prefault:true without
background_thread)`.

## 4. The Valkey soak, default against `prefault:true` (the first reading; DURATION 600)

Two soaks of challoc alone on the lane's C4A, back to back, the same tree and container:
`make valkey-soak ALLOCS=challoc DURATION=600 SETTLE=60` (write phase 300 s: valkey-benchmark
`-t set -d 512 -r 500000 -P 16` alternating with memtier SET-only 16..1024 B; delete phase 300
s; settle 60 s; activedefrag on; the acceptance protocol's key stream), once stock and once
with `CHALLOC_CONF=prefault:true,background_thread:true`. Valkey itself turns the allocator's
background thread on at startup (`jemalloc-bg-thread yes`, `mallctl("background_thread")`), so
the stock server has the thread too and the two runs differ by the `prefault` key alone. The
full 20-minute protocol (DURATION 1200) would not have fitted the iteration with the tests and
the two `quick` tiers on the same VM, so this is the 10-minute shape; the phases are the same
and only the drain rows are shorter. Sidecar on the VM host: `/proc/<pid>/task/<tid>/stat`
`minflt` per thread every 5 s (the CSVs `...-prefault-{default,prefault}-faults.csv` beside
this file) and `sudo perf stat -e page-faults -p <pid> -I 5000` (the fault exceptions the CPU
took; a page installed by `MADV_POPULATE_WRITE` is accounted to the caller's `minflt` but is
not an exception, so the two figures differ by construction). The soak's own summary tables
and sample CSVs (`...-{default,prefault}-soak.csv`) are the source of the first block.

| measure | default | `prefault:true` | prefault vs default |
|---|---:|---:|---:|
| write: valkey-benchmark req/s (mean of 6 runs) | 925268 | 924518 | -0.1% |
| write: valkey-benchmark p99 ms (median of 6) | 1.063 | 1.063 | 0 |
| write: memtier req/s | 960394 | 963014 | +0.3% |
| write: memtier p99 ms | 0.959 | 0.951 | -0.8% |
| delete: valkey-benchmark req/s | 1401377 | 1424801 | +1.7% |
| delete: valkey-benchmark p99 ms | 0.735 | 0.711 | -3.3% |
| delete: memtier req/s | 1245785 | 1255571 | +0.8% |
| delete: memtier p99 ms | 0.775 | 0.759 | -2.1% |
| peak RSS MiB (VmHWM) | 578.4 | 582.3 | +0.7% |
| RSS MiB at end of write phase | 578.2 | 582.0 | +0.7% |
| RSS MiB at end of delete phase | 129.4 | 131.6 | +1.7% |
| RSS MiB after settle | 129.3 | 131.4 | +1.6% |
| allocator_frag_ratio after settle | 1.80 | 1.78 | |
| active_defrag_hits | 240194 | 240008 | |

The page faults (minor; no major fault in either run), `minflt` of the server's threads from
the sidecar's samples nearest the instants, `t` in seconds from the server's start (the write
phase ends at 310 s, the run at about 625 s):

| thread, instant | default | `prefault:true` | delta |
|---|---:|---:|---:|
| main (`valkey-server`), t=1 (startup, before the load) | 29857 | 23251 | |
| main, t=6 (the first valkey-benchmark run has filled the keyspace) | 96550 | 60643 | -37% |
| main, end of the write phase (t=313) | 157236 | 96130 | -39% |
| main, end of the run (t=624) | 158636 | 97528 | -39% |
| main, write phase alone (t=313 minus t=1) | 127379 | 72879 | -43% |
| `challoc-bg`, end of the write phase | 2 | 58576 | |
| `challoc-bg`, end of the run | 2 | 58576 | |
| main + `challoc-bg`, end of the run | 158638 | 156104 | -1.6% |
| every other thread (bio_*, io), end of the run | 5 | 5 | |
| perf `page-faults` (exceptions), whole process, through t=312 | 123625 | 71196 | -42% |
| perf `page-faults`, whole process, whole run | 125104 | 72676 | -42% |

Reading: the pass moved 58.6 k of the write phase's page installs from the main thread to the
background thread, 43% of the main thread's write-phase faults (39% of its total), and the
process took 42% fewer fault exceptions; the sum of the two threads' `minflt` is within 1.6%
of the default's, so the pages are the same pages, installed by another thread ahead of the
refill. The 57% that stayed on the main thread are of two kinds this reading does not
separate: the huge runs (Valkey's dict tables and client buffers are `malloc`s of megabytes,
which the pass never touches: it prefaults slabs) and the slabs a burst takes faster than one
pass per millisecond can commit them (the first valkey-benchmark run fills 350 MiB in about
ten seconds, several classes at once; the pass commits one slab per class per pass, so a class
that consumes slabs faster than that gets every other one prefaulted; the `t=6` row shows the
burst's share). The write-phase p99 did not move at the tools' resolution (8 us steps on the
valkey-benchmark histogram: 1.063 both ways; memtier 0.959 -> 0.951, one step), which is the
expected size of the effect: 61 k faults of a few microseconds each spread over 300 s of a
920 k req/s stream are a few tens of microseconds per second of CPU, well under the p99 of
any single request. The RSS cost is one prefaulted slab per `(arena, class)` ahead of the
refill (+3.9 MiB at the peak, 0.7%) and the delete-phase and settle rows are within their
run-to-run spread (one run each).

## 5. Tests on this VM

`make test` on the C4A with the final tree: 408 + 120 + 23 passed, `EXIT 0`, every container row `verdict=ok`; `tests/c/prefault.c on`: `16 of 16 pages resident (the one after: 0); stats.prefault passes 2 (active 1) slabs 6 bytes 393216 wakes 216 fallbacks 0 skipped 0`, `1024 of the next 2048 allocations came from the prefaulted slab`; `off` and `inert` 0 of 16 with the counters at 0. On the C4 (x86_64) `make test` read 408 + 120 + 23 passed, `EXIT 0`, with `prefault on: 16 of 16 pages resident ... passes 4 (active 2) slabs 8 bytes 524288 wakes 216`.

## 6. What is left for the plan's acceptance (lane A, after the merge)

- The claim side: nothing is required for the pass to work (the refill takes a `Purged` slab
  first, so it takes the prefaulted one), but two hooks would raise the hit rate in bursts:
  a wake from the refill that takes a prefaulted slab (`take` sees `State::Purged` with
  `dirty_at == 0` and could call `purge::prefault::note_commit()`-like `note_taken()`), so the
  next slab is committed at once rather than at the next poll or the next fresh commit; and,
  where a class's refilled slots per pass exceed a slab, more than one slab per class per pass
  (the bound is the constant in `prefault::pass`).
- The plan's acceptance is the soak's write-phase p99 and the main thread's page faults with
  the scorecard within noise: the p99 is unchanged and the faults are -39%; the scorecard is
  the coordinator's, from `main`, once the lane is merged. The full DURATION 1200 protocol and
  the x86_64 soak were not run this iteration.
- Whether `prefault:true` should become the default is the human's decision with these numbers
  in front of them: no time cost measured on `quick` or the soak, +0.7% peak RSS on Valkey
  (one slab per class ahead of use: more on small heaps, +3.7 MiB on larson1's 16 MiB), and
  the pass needs the background thread, which the default configuration does not run.
