# challoc thread-affine slab refills (review P2 form a) A/B, 2026-09-17, x86_64 (GCE reference VM, us-east4-a)

Phase 1.5 task 4a, the x86_64 half of `2026-09-17-aarch64-gce-thread-affinity.md`
(which has the description of the variants, the method and the
conclusion). `old` df0c38d, `new` 39acac8, `narenas16` = old with
`MALLOC_CONF=narenas:16`, jemalloc = Wolfi's 5.3.1. VM numbers are never
compared with Mac numbers.

- host: challoc-bench-amd64 (c4-standard-16, Intel Xeon Platinum 8581C,
  16 vCPUs = 8 cores x 2 hyperthreads), Ubuntu 24.04 kernel
  7.0.0-1011-gcp, 4 KiB pages, `guard_install=yes`; container: Wolfi
  (rustc 1.98.1, gcc 15.3.0). The container test suite (`make test`: 225 +
  76 tests, jemalloc-compat, fork_alloc) ran green on this VM with the
  `new` tree before the A/B; load average 0.1 at the start of the A/B, the
  rounds raise it to about 10.
- method: as the aarch64 file (5 rounds plus warm-up, interleaved, sorted
  scores, `rt` median maxrss). Driver `.tmp-it19-ab.sh`.

## Run 1: old, new, narenas16, jemalloc

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 47.6 48.3 48.5 49.0 50.1 | 13076 |
| larson1 | new | 48.7 48.8 49.0 49.0 49.0 | 13188 |
| larson1 | narenas16 | 40.8 45.3 45.9 46.0 46.3 | 66548 |
| larson1 | jemalloc | 45.0 45.1 45.3 45.6 46.9 | 18004 |
| larson10 | old | 249.6 253.0 257.2 257.7 259.1 | 66928 |
| larson10 | new | 237.5 237.6 243.0 243.4 244.3 | 81668 |
| larson10 | narenas16 | 267.3 278.0 283.7 286.8 291.9 | 141716 |
| larson10 | jemalloc | 298.2 300.7 301.2 305.5 306.0 | 95960 |
| larson16 | old | 264.4 264.5 266.5 266.5 266.5 | 90424 |
| larson16 | new | 251.8 252.7 252.9 254.7 258.7 | 102160 |
| larson16 | narenas16 | 293.4 295.0 296.3 298.2 300.1 | 180208 |
| larson16 | jemalloc | 338.0 342.9 345.6 349.4 353.9 | 131548 |
| xmalloc-test | old | 519.5 521.2 524.8 527.6 528.8 | 39732 |
| xmalloc-test | new | 511.4 515.1 517.3 519.0 520.2 | 43944 |
| xmalloc-test | narenas16 | 342.8 355.5 358.7 360.0 360.8 | 83248 |
| xmalloc-test | jemalloc | 334.3 335.1 335.1 335.5 336.0 | 94588 |
| alloc-test1 | old | 3.050 3.105 3.232 3.321 3.335 | 14104 |
| alloc-test1 | new | 2.950 3.167 3.324 3.375 3.399 | 14132 |
| alloc-test1 | narenas16 | 2.849 3.009 3.292 3.321 3.353 | 14164 |
| alloc-test1 | jemalloc | 2.803 2.936 3.177 3.207 3.211 | 13444 |
| alloc-testN | old | 4.309 4.324 4.327 4.333 4.348 | 17792 |
| alloc-testN | new | 4.974 4.989 5.000 5.047 5.049 | 21540 |
| alloc-testN | narenas16 | 4.289 4.329 4.336 4.368 4.369 | 22476 |
| alloc-testN | jemalloc | 4.097 4.101 4.102 4.110 4.115 | 17044 |

Reading (medians, new vs old): larson1 48.5 -> 49.0 (+1%), larson10
257.2 -> 243.0 (-5.5%), larson16 266.5 -> 252.9 (-5.1%), xmalloc-test
524.8 -> 517.3 (-1.4%), alloc-test1 3.232 -> 3.324 s (inside the row's
wide spread on this VM), alloc-testN 4.327 -> 5.000 s (+15.6%). RSS:
larson10 +14.7 MiB, larson16 +11.7 MiB, alloc-testN +3.7 MiB. `narenas16`
in the same run: larson10 283.7 (+10.3% over old, 5.8% behind jemalloc),
larson16 296.3 (+11.2%, 14.3% behind jemalloc: the 16-thread row on 8
physical cores stays outside the Phase 1.5 10% criterion even with
arenas), larson1 45.9 (-5.4%: the `multi_arena` load and routing on the
`free` path), and xmalloc-test 358.7 (-32% against old's 524.8, still
ahead of jemalloc's 335.1: on this core the remote frees' routing into
per-arena caches and the per-arena releases cost what the shared slabs
saved; the review's "keeps its xmalloc-test number" held on aarch64 only,
where the same run reads +52%). RSS larson1 66.5 MiB against old's 13.1
and jemalloc's 18.0 (round-robin arena assignment under thread churn, see
the aarch64 file's conclusion), larson10 141.7 against jemalloc's 96.0
(+48%), larson16 180.2 against 131.5 (+37%): on x86_64 the `narenas16` RSS
rows are outside the 10% criterion too, so form (b) needs the least-loaded
assignment and a measurement of what is left.

Acceptance (plan 4b): the 10-thread larson row is 14.3% below the same
run's `narenas16` (criterion: within 3%) and 19.3% behind jemalloc
(criterion: within 10%). Form (a) misses.

## perf stat, larson 1 and 10 threads (old, new, narenas16, jemalloc)

Method of `2026-09-17-gce-review-counters.md`; all six events counted
without multiplexing on this PMU. Per operation with ops = throughput x
5 s. Driver `.tmp-it19-perf.sh`.

| program | variant | round | M ops/s | cyc/op | ins/op | L1D/op |
|---|---|---|---:|---:|---:|---:|
| larson1 | old | r1 | 48.8 | 67.4 | 215.6 | 2.09 |
| larson1 | old | r2 | 48.7 | 68.9 | 215.6 | 2.09 |
| larson1 | new | r1 | 48.9 | 67.2 | 215.6 | 2.09 |
| larson1 | new | r2 | 49.7 | 67.4 | 215.6 | 2.09 |
| larson1 | narenas16 | r1 | 45.5 | 72.7 | 223.1 | 2.01 |
| larson1 | narenas16 | r2 | 47.5 | 71.5 | 223.0 | 2.02 |
| larson1 | jemalloc | r1 | 45.4 | 72.2 | 200.9 | 2.12 |
| larson1 | jemalloc | r2 | 45.9 | 73.1 | 200.9 | 2.14 |
| larson10 | old | r1 | 264.7 | 124.9 | 216.5 | 2.82 |
| larson10 | old | r2 | 257.0 | 129.0 | 216.5 | 2.82 |
| larson10 | new | r1 | 242.9 | 136.3 | 226.9 | 2.80 |
| larson10 | new | r2 | 241.8 | 137.4 | 226.6 | 2.78 |
| larson10 | narenas16 | r1 | 277.6 | 119.1 | 223.7 | 2.12 |
| larson10 | narenas16 | r2 | 275.1 | 120.2 | 223.6 | 2.13 |
| larson10 | jemalloc | r1 | 292.1 | 113.0 | 201.1 | 2.20 |
| larson10 | jemalloc | r2 | 293.7 | 112.3 | 201.1 | 2.18 |

Reading: as on aarch64, `new` leaves the 10-thread L1D misses per
operation where `old` has them (2.79 against 2.82; `narenas16` 2.12,
jemalloc 2.19; criterion: within 10% of the `narenas16` figure) and adds
10 instructions and 10 cycles per operation. The threads' slabs are not
separated; the walk is pure cost. Conclusion and the consequences for
form (b) in the aarch64 file.
