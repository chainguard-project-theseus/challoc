# challoc A/B, 2026-09-21, aarch64

- commit: 6b29fa9
- date: 2026-09-21T05:43:09Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 4.49 2.18 1.43; at end: 5.23 5.71 4.30; 954 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: df6d28545d8b built into target/ab/df6d28545d8b
  - new: 6b29fa95ef88 built into target/ab/6b29fa95ef88
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 41.8 41.9 41.9 41.9 41.9 | 16272 |
| larson1 | new | 44.2 44.2 44.4 44.4 44.5 | 16120 |
| larson1 | jemalloc | 45.0 45.0 45.0 45.1 45.2 | 19040 |
| larson10 | old | 363.7 364.6 367.3 367.3 368.6 | 114572 |
| larson10 | new | 363.5 364.1 366.3 366.4 367.1 | 98228 |
| larson10 | jemalloc | 381.1 384.2 386.9 388.3 391.3 | 135780 |
| larson16 | old | 586.9 587.0 588.3 590.2 594.2 | 171408 |
| larson16 | new | 587.2 589.6 592.6 594.0 594.3 | 150756 |
| larson16 | jemalloc | 607.4 609.4 610.0 613.7 617.8 | 211576 |
| xmalloc | old | 573.566 575.946 576.231 576.308 576.565 | 55128 |
| xmalloc | new | 962.308 970.948 985.998 990.008 1003.936 | 68436 |
| xmalloc | jemalloc | 472.459 474.069 477.209 477.809 478.477 | 79492 |
| alloc-test1 | old | 2.708 2.711 2.715 2.718 2.735 | 13464 |
| alloc-test1 | new | 2.676 2.678 2.684 2.705 2.711 | 13016 |
| alloc-test1 | jemalloc | 2.547 2.549 2.552 2.560 2.563 | 12640 |
| alloc-testN | old | 2.292 2.305 2.307 2.313 2.327 | 16860 |
| alloc-testN | new | 2.264 2.265 2.288 2.288 2.288 | 16468 |
| alloc-testN | jemalloc | 2.200 2.207 2.209 2.210 2.214 | 16736 |
| mstress | old | 0.380 0.382 0.383 0.383 0.384 | 332236 |
| mstress | new | 0.426 0.427 0.428 0.428 0.429 | 287052 |
| mstress | jemalloc | 0.654 0.661 0.661 0.663 0.663 | 299304 |
| valkey1 | old | 6.902 6.915 7.044 7.060 7.132 | 598740 |
| valkey1 | new | 6.691 6.857 6.980 6.988 7.068 | 598740 |
| valkey1 | jemalloc | 6.699 6.747 6.888 6.949 6.982 | 599640 |
| valkey1-free | old | 6.620 6.788 6.870 6.935 6.975 | 598740 |
| valkey1-free | new | 6.890 6.962 6.981 6.983 6.985 | 598736 |
| valkey1-free | jemalloc | 6.734 6.744 6.808 6.869 6.911 | 599640 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.9 | 15.9 | 44.4 | 15.7 | 45.0 | 18.6 | -5.6% | -6.9% |
| larson10 | M ops/s | 367.3 | 111.9 | 366.3 | 95.9 | 386.9 | 132.6 | +0.3% | -5.1% |
| larson16 | M ops/s | 588.3 | 167.4 | 592.6 | 147.2 | 610.0 | 206.6 | -0.7% | -3.6% |
| xmalloc | M frees/s | 576.231 | 53.8 | 985.998 | 66.8 | 477.209 | 77.6 | -41.6% | +20.8% |
| alloc-test1 | s | 2.715 | 13.1 | 2.684 | 12.7 | 2.552 | 12.3 | -1.1% | -6.0% |
| alloc-testN | s | 2.307 | 16.5 | 2.288 | 16.1 | 2.209 | 16.3 | -0.8% | -4.2% |
| mstress | s | 0.383 | 324.4 | 0.428 | 280.3 | 0.661 | 292.3 | +11.7% | +72.6% |
| valkey1 | M ops/s | 7.044 | 584.7 | 6.980 | 584.7 | 6.888 | 585.6 | +0.9% | +2.3% |
| valkey1-free | M ops/s | 6.870 | 584.7 | 6.981 | 584.7 | 6.808 | 585.6 | -1.6% | +0.9% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
