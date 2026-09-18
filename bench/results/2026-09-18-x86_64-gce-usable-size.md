# challoc A/B, 2026-09-18, x86_64

- commit: c18979d (uncommitted changes in src/)
- date: 2026-09-18T00:51:37Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 4.30 1.40 0.65; at end: 9.35 7.79 4.50; 610 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; others: wall seconds
- variants:
  - old: c18979da50b7 built into target/ab/c18979da50b7
  - new: working tree (HEAD c18979d + uncommitted src changes) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 46.3 46.9 47.2 48.7 48.9 | 16344 |
| larson1 | new | 45.6 46.5 46.7 47.2 48.6 | 16156 |
| larson1 | jemalloc | 42.6 43.3 44.9 47.2 47.5 | 18628 |
| larson10 | old | 276.1 276.6 279.8 282.5 282.5 | 99672 |
| larson10 | new | 283.9 288.2 288.3 290.8 292.5 | 101584 |
| larson10 | jemalloc | 278.9 283.4 291.0 295.5 306.9 | 94428 |
| larson16 | old | 296.9 299.6 302.4 302.5 312.4 | 126204 |
| larson16 | new | 300.5 300.7 303.9 304.9 311.9 | 127204 |
| larson16 | jemalloc | 338.4 338.9 340.3 340.8 341.0 | 130928 |
| xmalloc | old | 341.151 343.798 345.384 351.721 356.311 | 83428 |
| xmalloc | new | 331.749 333.105 336.586 343.291 349.082 | 83016 |
| xmalloc | jemalloc | 330.942 330.972 332.487 333.838 334.880 | 94332 |
| alloc-test1 | old | 2.890 2.923 3.092 3.141 3.338 | 14056 |
| alloc-test1 | new | 2.898 3.110 3.245 3.346 3.347 | 14120 |
| alloc-test1 | jemalloc | 2.799 2.804 2.963 3.221 3.235 | 13392 |
| alloc-testN | old | 4.292 4.333 4.337 4.359 4.377 | 22432 |
| alloc-testN | new | 4.286 4.334 4.334 4.337 4.361 | 22488 |
| alloc-testN | jemalloc | 4.050 4.076 4.115 4.129 4.215 | 17056 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 47.2 | 16.0 | 46.7 | 15.8 | 44.9 | 18.2 | +1.1% | +5.1% |
| larson10 | M ops/s | 279.8 | 97.3 | 288.3 | 99.2 | 291.0 | 92.2 | -2.9% | -3.8% |
| larson16 | M ops/s | 302.4 | 123.2 | 303.9 | 124.2 | 340.3 | 127.9 | -0.5% | -11.1% |
| xmalloc | M frees/s | 345.384 | 81.5 | 336.586 | 81.1 | 332.487 | 92.1 | +2.6% | +3.9% |
| alloc-test1 | s | 3.092 | 13.7 | 3.245 | 13.8 | 2.963 | 13.1 | +4.9% | -4.2% |
| alloc-testN | s | 4.337 | 21.9 | 4.334 | 22.0 | 4.115 | 16.7 | -0.1% | -5.1% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
