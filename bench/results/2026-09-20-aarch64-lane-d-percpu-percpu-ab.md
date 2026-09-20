# challoc per-CPU campaign (percpu), 2026-09-20, aarch64

- commit: 0c60b1a
- date: 2026-09-20T04:52:05Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (GCE aarch64 lane VM), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 5.14 2.58 1.06; at end: 6.89 8.26 6.54; 1305 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than default.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - default: working tree (HEAD 0c60b1a) built into target/ab/tree
  - percpu: working tree (HEAD 0c60b1a) built into target/ab/tree, MALLOC_CONF=percpu_arena:percpu
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | default | 41.6 41.7 41.8 41.8 41.9 | 16440 |
| larson1 | percpu | 38.9 39.0 39.0 39.1 39.1 | 16804 |
| larson1 | jemalloc | 45.0 45.1 45.1 45.1 45.1 | 19332 |
| larson10 | default | 360.4 363.6 364.6 367.8 368.3 | 115108 |
| larson10 | percpu | 323.3 326.2 326.3 326.7 327.0 | 124852 |
| larson10 | jemalloc | 383.3 383.8 387.7 388.0 391.6 | 135016 |
| larson16 | default | 586.2 587.7 588.2 588.3 590.0 | 170420 |
| larson16 | percpu | 517.2 519.3 521.0 521.5 522.3 | 171804 |
| larson16 | jemalloc | 608.1 608.1 612.4 614.3 617.2 | 211316 |
| larson64 | default | 585.9 586.1 587.2 587.5 588.8 | 574500 |
| larson64 | percpu | 484.9 485.7 486.1 486.9 486.9 | 324592 |
| larson64 | jemalloc | 613.1 614.1 614.5 615.7 616.4 | 539688 |
| xmalloc | default | 575.352 576.646 576.838 577.036 577.708 | 54776 |
| xmalloc | percpu | 480.444 486.850 488.121 488.818 489.326 | 80352 |
| xmalloc | jemalloc | 473.542 476.130 477.874 482.688 483.579 | 79900 |
| alloc-test1 | default | 2.603 2.614 2.618 2.622 2.635 | 13316 |
| alloc-test1 | percpu | 2.713 2.742 2.766 2.766 2.783 | 13316 |
| alloc-test1 | jemalloc | 2.441 2.460 2.462 2.470 2.474 | 12636 |
| alloc-testN | default | 2.289 2.290 2.290 2.309 2.310 | 16900 |
| alloc-testN | percpu | 2.414 2.414 2.414 2.415 2.449 | 16896 |
| alloc-testN | jemalloc | 2.199 2.200 2.206 2.217 2.243 | 16856 |
| cfrac | default | 4.461 4.461 4.461 4.462 4.462 | 2852 |
| cfrac | percpu | 4.790 4.791 4.792 4.792 4.793 | 2852 |
| cfrac | jemalloc | 4.421 4.423 4.423 4.424 4.431 | 4860 |
| espresso | default | 3.653 3.653 3.654 3.654 3.658 | 3744 |
| espresso | percpu | 3.776 3.776 3.776 3.777 3.780 | 3740 |
| espresso | jemalloc | 3.740 3.742 3.743 3.745 3.746 | 6324 |
| mstress | default | 0.369 0.372 0.373 0.373 0.375 | 328168 |
| mstress | percpu | 0.376 0.379 0.379 0.380 0.380 | 327532 |
| mstress | jemalloc | 0.642 0.642 0.642 0.643 0.645 | 298680 |
| glibc-simple | default | 2.169 2.174 2.179 2.179 2.180 | 2424 |
| glibc-simple | percpu | 2.542 2.544 2.544 2.545 2.551 | 2424 |
| glibc-simple | jemalloc | 2.519 2.525 2.526 2.552 2.612 | 4492 |
| redis | default | 497.5 497.8 499.8 502.0 503.3 | 29876 |
| redis | percpu | 484.7 488.3 491.6 492.4 493.6 | 29872 |
| redis | jemalloc | 407.0 409.8 411.0 411.0 412.5 | 31884 |
| valkey1 | default | 7.254 7.281 7.611 7.660 7.757 | 598608 |
| valkey1 | percpu | 6.906 7.210 7.218 7.279 7.410 | 598608 |
| valkey1 | jemalloc | 7.335 7.528 7.705 7.728 7.732 | 599764 |
| valkey1-free | default | 7.113 7.152 7.226 7.316 7.407 | 598604 |
| valkey1-free | percpu | 6.519 6.525 6.606 6.622 6.684 | 598608 |
| valkey1-free | jemalloc | 7.122 7.122 7.182 7.217 7.222 | 599636 |

## Medians

| program | unit | default | default RSS MiB | percpu | percpu RSS MiB | jemalloc | jemalloc RSS MiB | percpu vs default | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.8 | 16.1 | 39.0 | 16.4 | 45.1 | 18.9 | +7.2% | -7.3% |
| larson10 | M ops/s | 364.6 | 112.4 | 326.3 | 121.9 | 387.7 | 131.9 | +11.7% | -6.0% |
| larson16 | M ops/s | 588.2 | 166.4 | 521.0 | 167.8 | 612.4 | 206.4 | +12.9% | -4.0% |
| larson64 | M ops/s | 587.2 | 561.0 | 486.1 | 317.0 | 614.5 | 527.0 | +20.8% | -4.4% |
| xmalloc | M frees/s | 576.838 | 53.5 | 488.121 | 78.5 | 477.874 | 78.0 | +18.2% | +20.7% |
| alloc-test1 | s | 2.618 | 13.0 | 2.766 | 13.0 | 2.462 | 12.3 | +5.7% | -6.0% |
| alloc-testN | s | 2.290 | 16.5 | 2.414 | 16.5 | 2.206 | 16.5 | +5.4% | -3.7% |
| cfrac | s | 4.461 | 2.8 | 4.792 | 2.8 | 4.423 | 4.7 | +7.4% | -0.9% |
| espresso | s | 3.654 | 3.7 | 3.776 | 3.7 | 3.743 | 6.2 | +3.3% | +2.4% |
| mstress | s | 0.373 | 320.5 | 0.379 | 319.9 | 0.642 | 291.7 | +1.6% | +72.1% |
| glibc-simple | s | 2.179 | 2.4 | 2.544 | 2.4 | 2.526 | 4.4 | +16.8% | +15.9% |
| redis | k req/s | 499.8 | 29.2 | 491.6 | 29.2 | 411.0 | 31.1 | +1.7% | +21.6% |
| valkey1 | M ops/s | 7.611 | 584.6 | 7.218 | 584.6 | 7.705 | 585.7 | +5.4% | -1.2% |
| valkey1-free | M ops/s | 7.226 | 584.6 | 6.606 | 584.6 | 7.182 | 585.6 | +9.4% | +0.6% |

Positive percentages mean the variant did worse than default (slower or lower throughput).
