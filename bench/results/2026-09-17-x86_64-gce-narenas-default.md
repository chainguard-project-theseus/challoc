# challoc A/B, 2026-09-17, x86_64

Phase 1.5 task 4c (form b), the x86_64 half of
`2026-09-17-aarch64-gce-narenas-default.md`, which describes the variants.
Same run, 5 rounds, protocol tier (`make ab`).

- commit: 8408d72
- date: 2026-09-17T22:37:54Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 3.62 2.59 1.70; at end: 10.11 8.75 5.96; 820 s total
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
| larson1 | old | 47.5 47.9 48.0 48.3 48.5 | 12936 |
| larson1 | new | 43.2 46.7 46.7 46.8 46.8 | 16204 |
| larson1 | narenas16 | 34.8 43.8 45.6 46.1 46.4 | 66328 |
| larson1 | jemalloc | 41.7 43.8 43.8 44.1 44.5 | 18032 |
| larson10 | old | 241.1 245.0 250.5 250.9 254.1 | 65832 |
| larson10 | new | 267.6 273.4 277.0 277.1 282.3 | 101524 |
| larson10 | narenas16 | 261.8 274.8 278.6 283.9 288.0 | 139656 |
| larson10 | jemalloc | 280.1 285.1 290.7 296.6 296.8 | 93824 |
| larson16 | old | 255.5 259.8 262.0 262.6 263.3 | 89668 |
| larson16 | new | 291.8 296.8 301.6 303.6 304.6 | 129468 |
| larson16 | narenas16 | 271.2 285.0 292.0 292.8 293.9 | 179448 |
| larson16 | jemalloc | 323.1 329.1 334.1 336.5 338.8 | 130392 |
| xmalloc | old | 499.049 505.802 511.915 517.970 518.475 | 39628 |
| xmalloc | new | 325.522 332.948 335.070 341.013 341.030 | 83484 |
| xmalloc | narenas16 | 329.916 353.219 353.793 354.917 365.077 | 83200 |
| xmalloc | jemalloc | 318.257 325.192 327.022 329.401 329.831 | 95624 |
| alloc-test1 | old | 3.299 3.306 3.323 3.336 3.376 | 13940 |
| alloc-test1 | new | 3.271 3.294 3.295 3.326 3.413 | 14012 |
| alloc-test1 | narenas16 | 3.291 3.296 3.320 3.333 3.424 | 14132 |
| alloc-test1 | jemalloc | 3.189 3.198 3.200 3.204 3.282 | 13352 |
| alloc-testN | old | 4.385 4.395 4.401 4.412 4.520 | 17708 |
| alloc-testN | new | 4.374 4.410 4.459 4.521 4.694 | 22316 |
| alloc-testN | narenas16 | 4.398 4.416 4.418 4.563 4.630 | 22340 |
| alloc-testN | jemalloc | 4.108 4.153 4.194 4.237 4.444 | 16880 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | narenas16 | narenas16 RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | narenas16 vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 48.0 | 12.6 | 46.7 | 15.8 | 45.6 | 64.8 | 43.8 | 17.6 | +2.8% | +5.3% | +9.6% |
| larson10 | M ops/s | 250.5 | 64.3 | 277.0 | 99.1 | 278.6 | 136.4 | 290.7 | 91.6 | -9.6% | -10.1% | -13.8% |
| larson16 | M ops/s | 262.0 | 87.6 | 301.6 | 126.4 | 292.0 | 175.2 | 334.1 | 127.3 | -13.1% | -10.3% | -21.6% |
| xmalloc | M frees/s | 511.915 | 38.7 | 335.070 | 81.5 | 353.793 | 81.2 | 327.022 | 93.4 | +52.8% | +44.7% | +56.5% |
| alloc-test1 | s | 3.323 | 13.6 | 3.295 | 13.7 | 3.320 | 13.8 | 3.200 | 13.0 | -0.8% | -0.1% | -3.7% |
| alloc-testN | s | 4.401 | 17.3 | 4.459 | 21.8 | 4.418 | 21.8 | 4.194 | 16.5 | +1.3% | +0.4% | -4.7% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

## Reading (plan 4b/4c criteria, medians)

- larson10: `new` 277.0 against `narenas16` 278.6 in the same run (-0.6%;
  criterion: within 3%) and jemalloc 290.7 (-4.7%; criterion: within 10%).
  `old` was -13.8% behind jemalloc.
- larson16: 301.6, -9.7% behind jemalloc (334.1), inside the 10% criterion
  that round-robin `narenas16` missed (-12.6%; iteration 19 read it as
  out of reach on 8 physical cores) and `old` missed by far (-21.6%).
- larson1: 46.7 against `old` 48.0 (-2.7%) and jemalloc 43.8 (+6.6%
  ahead). This is the `multi_arena` routing on `free` once a second arena
  exists (7..8 instructions per operation, iteration 19); `narenas16` read
  -5.3% against old with sixteen arenas in rotation.
- xmalloc-test: 335.1 against jemalloc 327.0 (+2.5% ahead) and old 511.9
  (-34.5%): the cost plan 4c anticipates on this core (remote frees routed
  into per-arena caches and per-arena releases; `narenas16` -30.9%).
- alloc-test 1/N: 3.295 / 4.459 s, equal to old within noise, 3.0% / 6.3%
  behind jemalloc (old: 3.8% / 4.9%).
- RSS: larson1 15.8 MiB against jemalloc's 17.6 (`narenas16` 64.8),
  larson10 99.1 against 91.6 (+8.2%; `narenas16` +49%), larson16 126.4
  against 127.3 (`narenas16` +38%), xmalloc-test 81.5 against 93.4,
  alloc-test1 13.7 against 13.0 (+5%). alloc-testN 21.8 against 16.5 (+32%,
  +5.3 MiB), identical under `narenas16`: the per-thread partial slab cost
  described in the aarch64 file.

The go/no-go tier on this VM (one round, right after a `make test` on the
same VM) said NO-GO on larson1 (+5.1%) and xmalloc (+57.0% against old, at
parity with jemalloc: 333.2 against 330.6) with larson10 flat (255.0
against 257.0). A 3-round probe on the quiet VM (larson10: old 249.7..251.8,
old+narenas:16 282.6..286.6, new 273.1..275.5, new+narenas:16 278.9..280.9,
new+narenas:1 251.1..257.3; larson1: old 48.4..48.8, new 47.2 twice and one
37.9 outlier, new+narenas:1 48.1..49.2) showed the flat larson10 was a
one-round outlier and larson1 at about -3%; the xmalloc row cannot pass a
5%-against-old gate under any `narenas > 1` on this core and is the row
the plan's 4c text asks to be judged against jemalloc, so the variant was
taken to this protocol run on that basis (recorded in PROGRESS.md).

## perf stat, larson 10 and 1 threads (new, narenas16, jemalloc, old)

Method of `2026-09-17-gce-review-counters.md` (`sudo perf stat` on the
host attached to the container, six events, all counted without
multiplexing on this PMU), with one change: the container runs `sh -c
"sleep 1; exec ... larson"` so perf is attached before larson's first
threads exist (a thread that exits between perf's enumeration and open is
lost together with the lane it spawns; the first attempt of this run lost
five of ten lanes that way, and `challoc-bench perf-stat` also had to stop
appending `-d` after the container's arguments). Per operation with ops =
throughput x 5 s; libraries are this run's `target/ab/*` builds. Driver
`.tmp-it20-perf.sh` in the iteration log.

| program | variant | M ops/s | cyc/op | ins/op | L1D/op | dTLB/op | br-miss/op |
|---|---|---:|---:|---:|---:|---:|---:|
| larson10 | new | 270.1 | 116.9 | 222.7 | 2.16 | 0.02 | 0.043 |
| larson10 | narenas16 | 263.5 | 119.8 | 223.6 | 2.12 | 0.02 | 0.049 |
| larson10 | jemalloc | 287.5 | 109.5 | 201.1 | 2.19 | 0.01 | 0.095 |
| larson10 | new | 270.3 | 117.1 | 222.7 | 2.14 | 0.02 | 0.043 |
| larson10 | narenas16 | 268.5 | 117.4 | 223.7 | 2.11 | 0.02 | 0.049 |
| larson10 | jemalloc | 280.2 | 111.5 | 201.1 | 2.20 | 0.00 | 0.095 |
| larson10 | old | 241.5 | 130.8 | 216.5 | 2.85 | 0.08 | 0.024 |
| larson1 | old | 47.1 | 67.4 | 215.6 | 2.08 | 0.00 | 0.024 |
| larson1 | new | 45.3 | 71.6 | 221.6 | 2.03 | 0.00 | 0.037 |
| larson1 | jemalloc | 44.6 | 72.2 | 200.9 | 2.12 | 0.00 | 0.099 |

Reading: larson10 L1D misses per operation `new` 2.16 / 2.14 against
`narenas16` 2.12 / 2.11 (+1.9%; criterion: within 10%), jemalloc 2.19 /
2.20, `old` 2.85: the default now partitions the threads' slabs as the
review's `narenas:16` did. Cycles per operation 117 against 118..120
(`narenas16`) and 131 (`old`); instructions 222.7 against 223.6 and 216.5.
larson1: the `multi_arena` routing on `free` is 6.0 instructions and 4
cycles per operation over `old` (221.6 / 71.6 against 215.6 / 67.4) with
L1D misses unchanged (2.03 against 2.08).
