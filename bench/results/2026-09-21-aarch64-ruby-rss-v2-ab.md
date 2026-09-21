# challoc A/B, 2026-09-21, aarch64

- commit: c8a8e03
- date: 2026-09-21T07:18:24Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 4.76 2.13 1.64; at end: 6.05 5.88 4.43; 954 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: df6d28545d8b built into target/ab/df6d28545d8b
  - new: c8a8e0378ec1 built into target/ab/c8a8e0378ec1
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 41.7 41.8 41.9 41.9 41.9 | 16144 |
| larson1 | new | 41.9 41.9 42.0 42.0 42.0 | 16144 |
| larson1 | jemalloc | 44.9 45.0 45.0 45.1 45.1 | 19216 |
| larson10 | old | 363.1 363.7 365.6 366.3 369.5 | 114704 |
| larson10 | new | 360.5 361.5 364.7 365.2 366.7 | 114960 |
| larson10 | jemalloc | 381.0 382.1 387.7 390.1 390.4 | 135568 |
| larson16 | old | 586.9 587.0 587.4 589.1 589.3 | 167568 |
| larson16 | new | 587.0 590.4 591.1 591.4 593.9 | 169688 |
| larson16 | jemalloc | 613.1 614.7 615.0 615.4 625.2 | 211688 |
| xmalloc | old | 576.272 576.407 576.490 576.540 577.124 | 54668 |
| xmalloc | new | 684.031 684.924 684.969 685.059 685.116 | 59296 |
| xmalloc | jemalloc | 472.327 476.098 478.652 484.197 488.546 | 82604 |
| alloc-test1 | old | 2.710 2.711 2.712 2.720 2.750 | 13464 |
| alloc-test1 | new | 2.714 2.731 2.732 2.733 2.738 | 13020 |
| alloc-test1 | jemalloc | 2.541 2.546 2.564 2.565 2.575 | 12640 |
| alloc-testN | old | 2.289 2.305 2.309 2.310 2.325 | 16976 |
| alloc-testN | new | 2.292 2.292 2.309 2.311 2.326 | 16472 |
| alloc-testN | jemalloc | 2.199 2.200 2.200 2.201 2.202 | 16992 |
| mstress | old | 0.381 0.382 0.382 0.383 0.385 | 332992 |
| mstress | new | 0.373 0.374 0.374 0.376 0.376 | 314016 |
| mstress | jemalloc | 0.657 0.660 0.661 0.663 0.667 | 297420 |
| valkey1 | old | 6.639 6.850 7.037 7.066 7.076 | 598676 |
| valkey1 | new | 6.737 6.792 6.940 7.031 7.066 | 598736 |
| valkey1 | jemalloc | 6.986 7.013 7.016 7.046 7.057 | 599640 |
| valkey1-free | old | 6.648 6.651 6.808 6.910 6.971 | 598740 |
| valkey1-free | new | 6.767 6.809 6.837 6.921 6.943 | 598736 |
| valkey1-free | jemalloc | 6.678 6.694 6.822 6.863 6.897 | 599640 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.9 | 15.8 | 42.0 | 15.8 | 45.0 | 18.8 | -0.2% | -6.9% |
| larson10 | M ops/s | 365.6 | 112.0 | 364.7 | 112.3 | 387.7 | 132.4 | +0.2% | -5.7% |
| larson16 | M ops/s | 587.4 | 163.6 | 591.1 | 165.7 | 615.0 | 206.7 | -0.6% | -4.5% |
| xmalloc | M frees/s | 576.490 | 53.4 | 684.969 | 57.9 | 478.652 | 80.7 | -15.8% | +20.4% |
| alloc-test1 | s | 2.712 | 13.1 | 2.732 | 12.7 | 2.564 | 12.3 | +0.7% | -5.5% |
| alloc-testN | s | 2.309 | 16.6 | 2.309 | 16.1 | 2.200 | 16.6 | +0.0% | -4.7% |
| mstress | s | 0.382 | 325.2 | 0.374 | 306.7 | 0.661 | 290.4 | -2.1% | +73.0% |
| valkey1 | M ops/s | 7.037 | 584.6 | 6.940 | 584.7 | 7.016 | 585.6 | +1.4% | +0.3% |
| valkey1-free | M ops/s | 6.808 | 584.7 | 6.837 | 584.7 | 6.822 | 585.6 | -0.4% | -0.2% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
