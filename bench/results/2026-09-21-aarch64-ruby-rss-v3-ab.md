# challoc A/B, 2026-09-21, aarch64

- commit: aeafc1e
- date: 2026-09-21T08:13:50Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 4.57 2.11 1.64; at end: 5.10 5.51 4.24; 954 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: df6d28545d8b built into target/ab/df6d28545d8b
  - new: aeafc1e91160 built into target/ab/aeafc1e91160
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 41.8 41.9 41.9 41.9 42.0 | 16400 |
| larson1 | new | 41.8 41.8 41.8 41.9 41.9 | 16268 |
| larson1 | jemalloc | 45.0 45.0 45.0 45.0 45.1 | 18836 |
| larson10 | old | 365.9 367.2 367.4 367.7 369.2 | 114960 |
| larson10 | new | 361.0 362.3 363.6 366.1 366.8 | 104848 |
| larson10 | jemalloc | 381.3 383.6 387.9 388.1 391.5 | 134516 |
| larson16 | old | 587.2 588.6 588.7 588.8 595.4 | 166928 |
| larson16 | new | 584.5 585.7 590.1 591.2 592.4 | 161140 |
| larson16 | jemalloc | 611.9 615.1 615.4 616.7 618.6 | 211780 |
| xmalloc | old | 575.063 575.860 576.015 577.106 577.224 | 57424 |
| xmalloc | new | 655.031 656.363 657.230 658.438 658.532 | 54100 |
| xmalloc | jemalloc | 466.180 470.559 476.368 478.365 480.970 | 80452 |
| alloc-test1 | old | 2.719 2.727 2.731 2.733 2.738 | 13464 |
| alloc-test1 | new | 2.733 2.736 2.750 2.755 2.777 | 13020 |
| alloc-test1 | jemalloc | 2.550 2.552 2.555 2.560 2.567 | 12640 |
| alloc-testN | old | 2.308 2.309 2.311 2.311 2.328 | 16920 |
| alloc-testN | new | 2.300 2.301 2.318 2.319 2.320 | 16476 |
| alloc-testN | jemalloc | 2.199 2.199 2.200 2.201 2.203 | 16864 |
| mstress | old | 0.383 0.384 0.385 0.386 0.386 | 335532 |
| mstress | new | 0.373 0.376 0.376 0.376 0.378 | 313412 |
| mstress | jemalloc | 0.657 0.662 0.663 0.664 0.668 | 298404 |
| valkey1 | old | 6.845 6.847 6.923 6.959 6.998 | 598736 |
| valkey1 | new | 6.681 6.778 6.915 6.931 6.993 | 598740 |
| valkey1 | jemalloc | 6.692 6.885 7.022 7.045 7.059 | 599764 |
| valkey1-free | old | 6.546 6.739 6.823 6.837 6.897 | 598740 |
| valkey1-free | new | 6.756 6.808 6.833 6.838 6.894 | 598740 |
| valkey1-free | jemalloc | 6.750 6.816 6.835 6.855 6.855 | 599768 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.9 | 16.0 | 41.8 | 15.9 | 45.0 | 18.4 | +0.2% | -6.9% |
| larson10 | M ops/s | 367.4 | 112.3 | 363.6 | 102.4 | 387.9 | 131.4 | +1.0% | -5.3% |
| larson16 | M ops/s | 588.7 | 163.0 | 590.1 | 157.4 | 615.4 | 206.8 | -0.2% | -4.3% |
| xmalloc | M frees/s | 576.015 | 56.1 | 657.230 | 52.8 | 476.368 | 78.6 | -12.4% | +20.9% |
| alloc-test1 | s | 2.731 | 13.1 | 2.750 | 12.7 | 2.555 | 12.3 | +0.7% | -6.4% |
| alloc-testN | s | 2.311 | 16.5 | 2.318 | 16.1 | 2.200 | 16.5 | +0.3% | -4.8% |
| mstress | s | 0.385 | 327.7 | 0.376 | 306.1 | 0.663 | 291.4 | -2.3% | +72.2% |
| valkey1 | M ops/s | 6.923 | 584.7 | 6.915 | 584.7 | 7.022 | 585.7 | +0.1% | -1.4% |
| valkey1-free | M ops/s | 6.823 | 584.7 | 6.833 | 584.7 | 6.835 | 585.7 | -0.1% | -0.2% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
