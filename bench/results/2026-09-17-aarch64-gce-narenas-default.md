# challoc A/B, 2026-09-17, aarch64

Phase 1.5 task 4c (form b of the larson fix): `new` is 8408d72, where
`narenas` defaults to `min(4 * ncpus, 64)` (64 on these VMs) and
`Arenas::assign` binds a new thread heap to the automatic arena with the
fewest bound heaps (ties: a built arena, then the lowest index; the
binding is released at heap exit). `old` is cd9eda2, the previous default
(one arena); `narenas16` is `old` with `MALLOC_CONF=narenas:16`, the
review's target configuration with round-robin assignment; jemalloc is
Wolfi's 5.3.1. Same run, 5 rounds, protocol tier (`make ab`). The x86_64
half is `2026-09-17-x86_64-gce-narenas-default.md`. VM numbers are never
compared with Mac numbers.

- commit: 8408d72
- date: 2026-09-17T22:36:06Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.43 1.48 1.07; at end: 9.52 8.38 5.42; 753 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; others: wall seconds
- variants:
  - old: cd9eda201d90 built into target/ab/cd9eda201d90
  - new: 8408d7234f43 built into target/ab/8408d7234f43
  - narenas16: cd9eda201d90 built into target/ab/cd9eda201d90, MALLOC_CONF=narenas:16
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 43.2 43.4 43.4 43.5 43.5 | 13456 |
| larson1 | new | 43.0 43.0 43.1 43.1 43.1 | 17040 |
| larson1 | narenas16 | 42.0 42.2 42.3 42.3 42.3 | 67200 |
| larson1 | jemalloc | 45.0 45.1 45.1 45.1 45.1 | 18524 |
| larson10 | old | 346.4 348.2 350.0 352.2 352.3 | 87568 |
| larson10 | new | 367.3 368.5 369.3 370.3 371.2 | 120700 |
| larson10 | narenas16 | 363.1 364.1 364.8 365.0 367.0 | 127484 |
| larson10 | jemalloc | 382.6 384.2 384.9 389.4 390.5 | 134996 |
| larson16 | old | 540.1 541.8 545.5 548.2 550.8 | 135568 |
| larson16 | new | 593.5 594.4 596.6 599.5 601.0 | 184848 |
| larson16 | narenas16 | 567.3 568.3 571.9 572.4 573.6 | 226448 |
| larson16 | jemalloc | 609.5 611.5 614.8 616.1 616.5 | 210972 |
| xmalloc | old | 344.697 372.694 383.802 391.166 398.851 | 39408 |
| xmalloc | new | 563.176 565.079 567.330 568.061 568.925 | 83700 |
| xmalloc | narenas16 | 586.828 586.835 586.841 586.984 587.514 | 84916 |
| xmalloc | jemalloc | 475.923 476.579 479.721 480.601 483.179 | 82448 |
| alloc-test1 | old | 2.638 2.661 2.662 2.674 2.679 | 13272 |
| alloc-test1 | new | 2.656 2.657 2.658 2.661 2.682 | 13404 |
| alloc-test1 | narenas16 | 2.651 2.666 2.679 2.683 2.686 | 13404 |
| alloc-test1 | jemalloc | 2.548 2.552 2.553 2.560 2.567 | 12604 |
| alloc-testN | old | 2.238 2.238 2.239 2.247 2.267 | 16604 |
| alloc-testN | new | 2.245 2.246 2.246 2.247 2.253 | 21848 |
| alloc-testN | narenas16 | 2.242 2.243 2.243 2.247 2.274 | 21772 |
| alloc-testN | jemalloc | 2.198 2.199 2.200 2.207 2.208 | 16700 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | narenas16 | narenas16 RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | narenas16 vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 43.4 | 13.1 | 43.1 | 16.6 | 42.3 | 65.6 | 45.1 | 18.1 | +0.7% | +2.6% | -3.8% |
| larson10 | M ops/s | 350.0 | 85.5 | 369.3 | 117.9 | 364.8 | 124.5 | 384.9 | 131.8 | -5.2% | -4.1% | -9.1% |
| larson16 | M ops/s | 545.5 | 132.4 | 596.6 | 180.5 | 571.9 | 221.1 | 614.8 | 206.0 | -8.6% | -4.6% | -11.3% |
| xmalloc | M frees/s | 383.802 | 38.5 | 567.330 | 81.7 | 586.841 | 82.9 | 479.721 | 80.5 | -32.3% | -34.6% | -20.0% |
| alloc-test1 | s | 2.662 | 13.0 | 2.658 | 13.1 | 2.679 | 13.1 | 2.553 | 12.3 | -0.2% | +0.6% | -4.1% |
| alloc-testN | s | 2.239 | 16.2 | 2.246 | 21.3 | 2.243 | 21.3 | 2.200 | 16.3 | +0.3% | +0.2% | -1.7% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

## Reading (plan 4b/4c criteria, medians)

- larson10: `new` 369.3 against `narenas16` 364.8 in the same run (+1.2%;
  criterion: within 3%) and jemalloc 384.9 (-4.1%; criterion: within 10%).
  `old` was -9.1% behind jemalloc.
- larson16: 596.6, -3.0% behind jemalloc (614.8); `narenas16` -7.0%, `old`
  -11.3%.
- larson1: 43.1 against `old` 43.4 (-0.7%, inside the row's spread) and
  jemalloc 45.1 (-4.4%). `narenas16` (round robin) read -2.6% against old;
  the least-loaded assignment keeps two arenas in rotation for larson1's
  one worker lane instead of sixteen.
- xmalloc-test: 567.3, +18% ahead of jemalloc (479.7) and +48% over old, as
  `narenas16` (586.8).
- alloc-test 1/N: equal to old (2.658 / 2.246 s), 4.1% / 2.1% behind
  jemalloc as before.
- RSS: larson1 16.6 MiB against jemalloc's 18.1 (round-robin `narenas16`
  read 65.6: every arena materialised under thread churn), larson10 117.9
  against 131.8, larson16 180.5 against 206.0, xmalloc-test 81.7 against
  80.5 (+1.5%), alloc-test1 13.1 against 12.3. alloc-testN is the one RSS
  row outside 10%: 21.3 MiB against jemalloc's 16.3 (+31%, +5.0 MiB),
  identical under `narenas16`: sixteen threads each hold a partial slab per
  class they touch, and challoc's slabs are 4..64 KiB where jemalloc's bins
  use 4..20 KiB. It is a geometry cost of per-thread arenas, not of the
  assignment policy (see PROGRESS.md, notes for the human).

The go/no-go tier (`make quick`, one round) passed on this VM before the
run: larson1 +0.9%, larson10 -7.8%, xmalloc -25.6%, alloc-test1 +0.0%,
churn compacts. A 3-round probe of larson10/larson1 under old,
old+narenas:16, new, new+narenas:16 and new+narenas:1 on this VM (driver in
the iteration-20 log) read larson10 349..353 / 360..370 / 366..371 /
364..373 / 347..350 and larson1 43.4 / 42.2 / 42.8..43.1 / 43.1 / 43.9..44.0:
`new` at `narenas:1` equals `old`, so the assignment code costs nothing in
single-arena mode.

## perf stat, larson 10 and 1 threads (new, narenas16, jemalloc, old)

Method as in the x86_64 file (`sudo perf stat` on the host attached to
the container before larson's first thread exists, six events, one
multiplexed out on this core; per operation with ops = throughput x 5 s;
this run's `target/ab/*` builds; driver `.tmp-it20-perf.sh`). Run after
the soak, on the quiet VM.

| program | variant | M ops/s | cyc/op | ins/op | L1D/op | dTLB/op | br-miss/op |
|---|---|---:|---:|---:|---:|---:|---:|
| larson10 | new | 343.5 | 80.9 | 241.0 | 2.64 | 3.90 | 0.042 |
| larson10 | narenas16 | 334.0 | 83.1 | 244.0 | 2.65 | 3.90 | 0.049 |
| larson10 | jemalloc | 365.3 | 76.0 | 223.4 | 2.22 | 1.86 | 0.024 |
| larson10 | new | 341.5 | 81.3 | 241.1 | 2.64 | 3.96 | 0.043 |
| larson10 | narenas16 | 335.0 | 82.8 | 244.1 | 2.65 | 3.91 | 0.050 |
| larson10 | jemalloc | 360.1 | 77.0 | 223.5 | 2.23 | 1.88 | 0.024 |
| larson10 | old | 322.1 | 86.1 | 236.8 | 3.10 | 4.04 | 0.023 |
| larson1 | old | 40.8 | 69.1 | 235.9 | 2.49 | 3.53 | 0.021 |
| larson1 | new | 40.3 | 70.1 | 241.1 | 2.47 | 3.52 | 0.043 |
| larson1 | jemalloc | 42.4 | 66.2 | 222.9 | 2.07 | 1.73 | 0.023 |

Reading: larson10 L1D misses per operation `new` 2.64 / 2.64 against
`narenas16` 2.65 / 2.65 (criterion: within 10%), jemalloc 2.22 / 2.23,
`old` 3.10: the partitioning the review measured for `narenas:16` is what
the default now does; the remaining 0.4 miss per operation against
jemalloc is the single-thread residue (larson1 2.47 against 2.07, review
P3, task 5), not thread sharing. Cycles per operation 80.9..81.3 against
82.8..83.1 (`narenas16`) and 86.1 (`old`); dTLB refills 3.90..3.96 per
operation against jemalloc's 1.86..1.88 are unchanged from `old` (4.04)
and are task 5's subject. larson1: 5.2 instructions and 1 cycle per
operation over `old` for the `free` routing (241.1 / 70.1 against 235.9 /
69.1), L1D misses unchanged (2.47 against 2.49).

## Valkey with the new default (plan 4c: full suite and soak rerun)

- x86_64 VM, `make valkey` (build, `unit/memefficiency`, full `./runtest`)
  at 8408d72: memefficiency 33 tests, all pass; full suite 5192 tests, all
  pass, 957 s (the two failures of the Docker Desktop runs were
  environmental and do not appear on the VM kernel).
- aarch64 VM, `make valkey-soak` (20 minutes of load per allocator, same
  run, `2026-09-17-aarch64-gce-narenas-default-valkey-soak.md`): peak RSS
  575.9 against jemalloc's 572.2 MiB (+0.6%; criterion at most +5%), RSS
  after settle 125.6 against 126.4 (-0.6%; within 10%),
  `allocator_frag_ratio` 1.75 against 1.77, defrag hits 238k against 235k.
  Throughput 2.2..7.7% behind jemalloc and p99 2.4..7.9% higher, which is
  the pre-existing P2 throughput item: against the previous soak
  (e6b89c3, `2026-09-17-aarch64-gce-slab-geometry-soak2-valkey-soak.md`)
  both halves moved together (write valkey-benchmark 815k against 877k
  now, 883k against 945k then) and the gaps are the same or smaller
  (memtier write +2.2% against +4.8%, delete +4.5% / +4.3% against +5.5% /
  +6.0%); RSS peak 575.9 against 578.3 and post-settle 125.6 against 128.6
  then. Valkey's main thread plus its bio threads each get an arena; the
  bio threads' remote frees route through per-arena caches as before.
