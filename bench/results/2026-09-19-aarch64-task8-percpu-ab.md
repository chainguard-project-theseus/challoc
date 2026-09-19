# challoc A/B, 2026-09-19, aarch64

- commit: 14c7fad (uncommitted changes in src/)
- date: 2026-09-19T21:31:44Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 5.64 3.73 2.24; at end: 4.73 5.00 4.47; 1521 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: 9351eeb4b827 built into target/ab/9351eeb4b827
  - new: working tree (HEAD 14c7fad + uncommitted src changes) built into target/ab/tree
  - new-off: working tree (HEAD 14c7fad + uncommitted src changes) built into target/ab/tree, MALLOC_CONF=percpu_arena:off
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.5 42.6 42.6 42.6 42.7 | 16364 |
| larson1 | new | 36.6 36.9 37.0 37.0 37.1 | 19828 |
| larson1 | new-off | 41.7 41.7 41.7 41.8 41.8 | 19832 |
| larson1 | jemalloc | 44.9 45.0 45.1 45.1 45.1 | 19052 |
| larson10 | old | 366.2 367.8 370.2 370.4 370.5 | 114940 |
| larson10 | new | 309.7 311.5 312.3 313.3 315.4 | 138236 |
| larson10 | new-off | 358.3 359.1 359.3 364.6 365.3 | 142844 |
| larson10 | jemalloc | 385.0 388.4 388.7 390.1 392.4 | 136128 |
| larson16 | old | 596.0 596.7 598.9 601.9 602.4 | 170992 |
| larson16 | new | 490.5 491.2 492.1 494.0 500.7 | 193524 |
| larson16 | new-off | 584.9 585.2 585.8 586.5 590.6 | 216044 |
| larson16 | jemalloc | 613.7 614.7 616.3 616.4 619.9 | 211472 |
| xmalloc | old | 584.580 584.936 586.510 587.037 587.310 | 55056 |
| xmalloc | new | 465.962 468.943 468.965 470.094 472.753 | 78672 |
| xmalloc | new-off | 574.955 578.112 578.211 579.607 580.190 | 55824 |
| xmalloc | jemalloc | 482.265 482.821 483.308 485.676 489.977 | 81176 |
| alloc-test1 | old | 2.648 2.664 2.665 2.668 2.680 | 13256 |
| alloc-test1 | new | 2.915 2.935 2.935 2.950 2.957 | 13248 |
| alloc-test1 | new-off | 2.663 2.670 2.671 2.672 2.691 | 13256 |
| alloc-test1 | jemalloc | 2.500 2.514 2.519 2.526 2.526 | 12620 |
| alloc-testN | old | 2.281 2.281 2.282 2.302 2.311 | 16840 |
| alloc-testN | new | 2.517 2.519 2.520 2.536 2.554 | 16836 |
| alloc-testN | new-off | 2.288 2.290 2.307 2.307 2.309 | 16692 |
| alloc-testN | jemalloc | 2.198 2.199 2.200 2.202 2.212 | 16844 |
| mstress | old | 0.373 0.375 0.376 0.377 0.378 | 330488 |
| mstress | new | 0.387 0.388 0.388 0.389 0.393 | 328136 |
| mstress | new-off | 0.374 0.374 0.375 0.378 0.379 | 322092 |
| mstress | jemalloc | 0.641 0.641 0.646 0.658 0.665 | 298612 |
| cfrac | old | 4.457 4.458 4.463 4.469 4.477 | 2860 |
| cfrac | new | 4.971 4.975 4.976 4.976 4.977 | 2872 |
| cfrac | new-off | 4.474 4.474 4.476 4.477 4.482 | 2864 |
| cfrac | jemalloc | 4.422 4.424 4.425 4.426 4.443 | 4960 |
| espresso | old | 3.648 3.648 3.650 3.655 3.657 | 3692 |
| espresso | new | 3.844 3.844 3.845 3.849 3.975 | 3696 |
| espresso | new-off | 3.657 3.658 3.659 3.659 3.660 | 3700 |
| espresso | jemalloc | 3.743 3.744 3.745 3.748 3.756 | 6304 |
| glibc-simple | old | 2.160 2.165 2.165 2.167 2.168 | 2300 |
| glibc-simple | new | 2.794 2.795 2.795 2.797 2.832 | 2316 |
| glibc-simple | new-off | 2.175 2.177 2.183 2.186 2.195 | 2316 |
| glibc-simple | jemalloc | 2.521 2.526 2.528 2.533 2.555 | 4468 |
| valkey1 | old | 6.995 7.155 7.182 7.237 7.272 | 598608 |
| valkey1 | new | 6.393 6.396 6.483 6.588 6.619 | 598608 |
| valkey1 | new-off | 6.986 7.017 7.090 7.094 7.252 | 598652 |
| valkey1 | jemalloc | 7.096 7.111 7.151 7.178 7.263 | 599492 |
| valkey1-free | old | 6.968 6.977 7.010 7.048 7.182 | 598668 |
| valkey1-free | new | 6.073 6.079 6.080 6.096 6.225 | 598656 |
| valkey1-free | new-off | 6.874 6.882 6.986 6.986 7.052 | 598608 |
| valkey1-free | jemalloc | 6.820 6.844 6.893 6.909 7.021 | 599820 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | new-off | new-off RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | new-off vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.6 | 16.0 | 37.0 | 19.4 | 41.7 | 19.4 | 45.1 | 18.6 | +15.1% | +2.2% | -5.5% |
| larson10 | M ops/s | 370.2 | 112.2 | 312.3 | 135.0 | 359.3 | 139.5 | 388.7 | 132.9 | +18.5% | +3.0% | -4.8% |
| larson16 | M ops/s | 598.9 | 167.0 | 492.1 | 189.0 | 585.8 | 211.0 | 616.3 | 206.5 | +21.7% | +2.2% | -2.8% |
| xmalloc | M frees/s | 586.510 | 53.8 | 468.965 | 76.8 | 578.211 | 54.5 | 483.308 | 79.3 | +25.1% | +1.4% | +21.4% |
| alloc-test1 | s | 2.665 | 12.9 | 2.935 | 12.9 | 2.671 | 12.9 | 2.519 | 12.3 | +10.1% | +0.2% | -5.5% |
| alloc-testN | s | 2.282 | 16.4 | 2.520 | 16.4 | 2.307 | 16.3 | 2.200 | 16.4 | +10.4% | +1.1% | -3.6% |
| mstress | s | 0.376 | 322.7 | 0.388 | 320.4 | 0.375 | 314.5 | 0.646 | 291.6 | +3.2% | -0.3% | +71.8% |
| cfrac | s | 4.463 | 2.8 | 4.976 | 2.8 | 4.476 | 2.8 | 4.425 | 4.8 | +11.5% | +0.3% | -0.9% |
| espresso | s | 3.650 | 3.6 | 3.845 | 3.6 | 3.659 | 3.6 | 3.745 | 6.2 | +5.3% | +0.2% | +2.6% |
| glibc-simple | s | 2.165 | 2.2 | 2.795 | 2.3 | 2.183 | 2.3 | 2.528 | 4.4 | +29.1% | +0.8% | +16.8% |
| valkey1 | M ops/s | 7.182 | 584.6 | 6.483 | 584.6 | 7.090 | 584.6 | 7.151 | 585.4 | +10.8% | +1.3% | +0.4% |
| valkey1-free | M ops/s | 7.010 | 584.6 | 6.080 | 584.6 | 6.986 | 584.6 | 6.893 | 585.8 | +15.3% | +0.3% | +1.7% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
