# challoc per-CPU campaign (baseline), 2026-09-19, aarch64

- commit: 675f357
- date: 2026-09-19T20:40:05Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (GCE aarch64 lane VM), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 4.26 1.79 1.03; at end: 7.88 8.19 5.66; 870 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than default.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - default: working tree (HEAD 675f357) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | default | 42.2 42.3 42.4 42.6 42.7 | 16524 |
| larson1 | jemalloc | 44.9 44.9 45.0 45.0 45.1 | 19344 |
| larson10 | default | 359.1 366.8 367.3 372.1 372.7 | 114956 |
| larson10 | jemalloc | 380.1 380.5 381.1 383.1 387.8 | 134304 |
| larson16 | default | 584.6 590.0 594.7 597.2 598.7 | 167948 |
| larson16 | jemalloc | 612.0 612.3 613.1 614.1 619.4 | 210972 |
| larson64 | default | 566.1 567.3 591.4 591.7 598.8 | 575504 |
| larson64 | jemalloc | 584.2 585.5 611.2 619.1 620.7 | 537904 |
| xmalloc | default | 585.036 585.907 586.022 586.240 587.002 | 55756 |
| xmalloc | jemalloc | 466.446 469.203 475.170 475.992 479.619 | 78576 |
| alloc-test1 | default | 2.510 2.559 2.571 2.771 2.798 | 13268 |
| alloc-test1 | jemalloc | 2.405 2.424 2.454 2.607 2.614 | 12636 |
| alloc-testN | default | 2.283 2.300 2.301 2.306 2.307 | 16728 |
| alloc-testN | jemalloc | 2.198 2.198 2.199 2.207 2.227 | 16984 |
| cfrac | default | 4.460 4.464 4.464 4.469 4.469 | 2808 |
| cfrac | jemalloc | 4.425 4.428 4.430 4.433 4.443 | 4860 |
| espresso | default | 3.648 3.649 3.652 3.654 3.656 | 3700 |
| espresso | jemalloc | 3.743 3.743 3.747 3.749 3.763 | 6328 |
| mstress | default | 0.370 0.370 0.374 0.400 0.400 | 326376 |
| mstress | jemalloc | 0.641 0.646 0.646 0.697 0.717 | 298272 |
| glibc-simple | default | 2.155 2.162 2.167 2.169 2.198 | 2316 |
| glibc-simple | jemalloc | 2.523 2.524 2.525 2.527 2.536 | 4488 |
| redis | default | 490.2 496.8 498.0 499.5 500.5 | 29832 |
| redis | jemalloc | 405.7 409.5 411.2 411.2 412.4 | 31884 |
| valkey1 | default | 6.114 6.217 7.857 7.902 8.627 | 598604 |
| valkey1 | jemalloc | 6.067 6.196 7.730 7.882 8.605 | 599744 |
| valkey1-free | default | 5.948 6.069 7.777 8.262 8.371 | 598608 |
| valkey1-free | jemalloc | 5.990 6.137 7.381 7.628 8.221 | 599764 |

## Medians

| program | unit | default | default RSS MiB | jemalloc | jemalloc RSS MiB | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.4 | 16.1 | 45.0 | 18.9 | -5.8% |
| larson10 | M ops/s | 367.3 | 112.3 | 381.1 | 131.2 | -3.6% |
| larson16 | M ops/s | 594.7 | 164.0 | 613.1 | 206.0 | -3.0% |
| larson64 | M ops/s | 591.4 | 562.0 | 611.2 | 525.3 | -3.2% |
| xmalloc | M frees/s | 586.022 | 54.4 | 475.170 | 76.7 | +23.3% |
| alloc-test1 | s | 2.571 | 13.0 | 2.454 | 12.3 | -4.6% |
| alloc-testN | s | 2.301 | 16.3 | 2.199 | 16.6 | -4.4% |
| cfrac | s | 4.464 | 2.7 | 4.430 | 4.7 | -0.8% |
| espresso | s | 3.652 | 3.6 | 3.747 | 6.2 | +2.6% |
| mstress | s | 0.374 | 318.7 | 0.646 | 291.3 | +72.7% |
| glibc-simple | s | 2.167 | 2.3 | 2.525 | 4.4 | +16.5% |
| redis | k req/s | 498.0 | 29.1 | 411.2 | 31.1 | +21.1% |
| valkey1 | M ops/s | 7.857 | 584.6 | 7.730 | 585.7 | +1.6% |
| valkey1-free | M ops/s | 7.777 | 584.6 | 7.381 | 585.7 | +5.4% |

Positive percentages mean the variant did worse than default (slower or lower throughput).
