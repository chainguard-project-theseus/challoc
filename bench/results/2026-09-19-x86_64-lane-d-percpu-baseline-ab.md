# challoc per-CPU campaign (baseline), 2026-09-19, x86_64

- commit: 675f357
- date: 2026-09-19T20:40:15Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (GCE x86_64 lane VM), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 6.40 2.58 1.47; at end: 8.07 8.09 5.84; 886 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than default.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - default: working tree (HEAD 675f357) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | default | 45.5 47.0 47.6 49.7 50.4 | 17608 |
| larson1 | jemalloc | 44.3 46.3 47.5 49.7 49.9 | 18916 |
| larson10 | default | 280.6 284.8 284.9 286.6 294.0 | 98468 |
| larson10 | jemalloc | 291.6 292.1 297.1 298.5 303.2 | 95512 |
| larson16 | default | 312.0 312.3 313.5 315.3 320.7 | 121316 |
| larson16 | jemalloc | 342.5 343.1 344.2 346.3 354.8 | 132224 |
| larson64 | default | 341.4 341.6 344.3 344.7 346.3 | 487548 |
| larson64 | jemalloc | 369.2 372.0 373.0 373.9 374.3 | 423832 |
| xmalloc | default | 328.762 334.672 335.404 344.353 354.851 | 53800 |
| xmalloc | jemalloc | 332.812 333.062 333.781 334.531 334.879 | 93768 |
| alloc-test1 | default | 2.843 2.846 2.851 3.153 3.399 | 14216 |
| alloc-test1 | jemalloc | 2.694 2.700 2.703 2.786 3.036 | 13576 |
| alloc-testN | default | 4.424 4.427 4.430 4.450 4.490 | 17820 |
| alloc-testN | jemalloc | 4.090 4.096 4.097 4.106 4.109 | 17312 |
| cfrac | default | 3.620 3.629 3.650 3.762 4.129 | 3740 |
| cfrac | jemalloc | 3.453 3.458 3.461 3.558 3.921 | 5856 |
| espresso | default | 4.016 4.023 4.042 4.154 4.553 | 4232 |
| espresso | jemalloc | 4.031 4.032 4.047 4.172 4.551 | 6484 |
| mstress | default | 0.624 0.629 0.633 0.635 0.637 | 342000 |
| mstress | jemalloc | 1.046 1.062 1.065 1.066 1.067 | 293124 |
| glibc-simple | default | 1.817 1.826 1.852 1.881 2.091 | 2724 |
| glibc-simple | jemalloc | 1.811 1.816 1.824 1.880 2.067 | 5236 |
| redis | default | 645.2 694.9 705.2 723.6 750.8 | 30748 |
| redis | jemalloc | 510.5 571.4 576.7 581.7 590.7 | 33336 |
| valkey1 | default | 14.364 15.493 15.553 15.733 15.942 | 599684 |
| valkey1 | jemalloc | 15.034 15.347 15.454 15.455 15.486 | 606716 |
| valkey1-free | default | 14.868 16.047 16.053 16.114 16.344 | 599684 |
| valkey1-free | jemalloc | 13.104 13.498 13.623 13.662 13.841 | 606716 |

## Medians

| program | unit | default | default RSS MiB | jemalloc | jemalloc RSS MiB | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 47.6 | 17.2 | 47.5 | 18.5 | +0.2% |
| larson10 | M ops/s | 284.9 | 96.2 | 297.1 | 93.3 | -4.1% |
| larson16 | M ops/s | 313.5 | 118.5 | 344.2 | 129.1 | -8.9% |
| larson64 | M ops/s | 344.3 | 476.1 | 373.0 | 413.9 | -7.7% |
| xmalloc | M frees/s | 335.404 | 52.5 | 333.781 | 91.6 | +0.5% |
| alloc-test1 | s | 2.851 | 13.9 | 2.703 | 13.3 | -5.2% |
| alloc-testN | s | 4.430 | 17.4 | 4.097 | 16.9 | -7.5% |
| cfrac | s | 3.650 | 3.7 | 3.461 | 5.7 | -5.2% |
| espresso | s | 4.042 | 4.1 | 4.047 | 6.3 | +0.1% |
| mstress | s | 0.633 | 334.0 | 1.065 | 286.3 | +68.2% |
| glibc-simple | s | 1.852 | 2.7 | 1.824 | 5.1 | -1.5% |
| redis | k req/s | 705.2 | 30.0 | 576.7 | 32.6 | +22.3% |
| valkey1 | M ops/s | 15.553 | 585.6 | 15.454 | 592.5 | +0.6% |
| valkey1-free | M ops/s | 16.053 | 585.6 | 13.623 | 592.5 | +17.8% |

Positive percentages mean the variant did worse than default (slower or lower throughput).
