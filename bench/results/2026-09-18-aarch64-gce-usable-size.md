# challoc A/B, 2026-09-18, aarch64

- commit: c18979d (uncommitted changes in src/)
- date: 2026-09-18T01:34:30Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.83 1.81 1.80; at end: 10.42 8.38 5.28; 568 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; others: wall seconds
- variants:
  - old: c18979da50b7 built into target/ab/c18979da50b7
  - new: working tree (HEAD c18979d + uncommitted src changes) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.9 43.0 43.0 43.0 43.1 | 17068 |
| larson1 | new | 42.7 42.9 43.0 43.1 43.1 | 16932 |
| larson1 | jemalloc | 44.9 45.0 45.1 45.1 45.1 | 18824 |
| larson10 | old | 365.4 372.0 372.3 372.6 373.1 | 121124 |
| larson10 | new | 365.9 369.3 372.4 372.5 372.9 | 120240 |
| larson10 | jemalloc | 383.6 383.7 386.3 387.7 387.8 | 134452 |
| larson16 | old | 594.8 595.7 600.5 601.0 602.5 | 183472 |
| larson16 | new | 592.5 596.5 597.7 599.7 603.8 | 185392 |
| larson16 | jemalloc | 609.5 612.8 613.0 615.3 617.1 | 211440 |
| xmalloc | old | 563.135 566.444 567.049 567.250 567.813 | 86388 |
| xmalloc | new | 565.255 565.413 565.487 565.707 566.567 | 86432 |
| xmalloc | jemalloc | 477.354 479.598 480.094 481.470 484.224 | 82396 |
| alloc-test1 | old | 2.653 2.654 2.663 2.677 2.689 | 13432 |
| alloc-test1 | new | 2.644 2.648 2.656 2.656 2.663 | 13424 |
| alloc-test1 | jemalloc | 2.542 2.547 2.557 2.561 2.576 | 12636 |
| alloc-testN | old | 2.250 2.252 2.253 2.253 2.259 | 22012 |
| alloc-testN | new | 2.237 2.244 2.246 2.249 2.250 | 22012 |
| alloc-testN | jemalloc | 2.206 2.206 2.207 2.207 2.397 | 16852 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 43.0 | 16.7 | 43.0 | 16.5 | 45.1 | 18.4 | +0.0% | -4.7% |
| larson10 | M ops/s | 372.3 | 118.3 | 372.4 | 117.4 | 386.3 | 131.3 | -0.0% | -3.6% |
| larson16 | M ops/s | 600.5 | 179.2 | 597.7 | 181.0 | 613.0 | 206.5 | +0.5% | -2.0% |
| xmalloc | M frees/s | 567.049 | 84.4 | 565.487 | 84.4 | 480.094 | 80.5 | +0.3% | +18.1% |
| alloc-test1 | s | 2.663 | 13.1 | 2.656 | 13.1 | 2.557 | 12.3 | -0.3% | -4.0% |
| alloc-testN | s | 2.253 | 21.5 | 2.246 | 21.5 | 2.207 | 16.5 | -0.3% | -2.0% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
