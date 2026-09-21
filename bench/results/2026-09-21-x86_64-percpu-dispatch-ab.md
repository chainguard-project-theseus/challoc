# challoc A/B, 2026-09-21, x86_64

- commit: cc547ed
- date: 2026-09-21T04:25:53Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 2.49 2.36 1.68; at end: 5.50 5.80 5.16; 1661 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: 2078f672bcdc built into target/ab/2078f672bcdc
  - new: cc547edbc8b1 built into target/ab/cc547edbc8b1
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)
  - new-percpu: cc547edbc8b1 built into target/ab/cc547edbc8b1, MALLOC_CONF=percpu_arena:percpu
  - old-percpu: 2078f672bcdc built into target/ab/2078f672bcdc, MALLOC_CONF=percpu_arena:percpu

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.9 43.1 43.5 45.1 45.8 | 17220 |
| larson1 | new | 43.9 44.7 44.8 45.3 46.9 | 17536 |
| larson1 | jemalloc | 41.5 42.1 43.8 44.2 45.3 | 18472 |
| larson1 | new-percpu | 24.3 25.2 25.2 25.4 27.0 | 14948 |
| larson1 | old-percpu | 42.2 42.6 42.6 42.8 43.7 | 18084 |
| larson10 | old | 265.1 274.6 277.2 281.5 282.0 | 97604 |
| larson10 | new | 266.9 271.3 278.9 280.3 286.3 | 97240 |
| larson10 | jemalloc | 287.4 294.8 297.2 297.5 302.8 | 93176 |
| larson10 | new-percpu | 177.9 183.4 187.5 189.1 190.6 | 88696 |
| larson10 | old-percpu | 232.7 235.9 238.7 240.2 248.6 | 97884 |
| larson16 | old | 294.7 304.3 305.9 307.0 309.1 | 122284 |
| larson16 | new | 293.1 297.9 305.2 306.7 311.4 | 120000 |
| larson16 | jemalloc | 330.8 334.0 336.0 338.5 339.6 | 130612 |
| larson16 | new-percpu | 197.2 201.1 205.2 206.7 207.3 | 102632 |
| larson16 | old-percpu | 250.1 254.9 260.1 260.8 266.6 | 109748 |
| xmalloc | old | 314.987 321.918 322.706 330.470 343.136 | 54276 |
| xmalloc | new | 320.939 322.592 324.697 332.859 343.195 | 54480 |
| xmalloc | jemalloc | 324.171 328.896 329.034 332.131 333.210 | 91732 |
| xmalloc | new-percpu | 231.656 237.353 239.663 244.309 251.363 | 58368 |
| xmalloc | old-percpu | 286.713 287.796 291.431 295.226 299.269 | 59008 |
| alloc-test1 | old | 3.028 3.308 3.335 3.420 3.475 | 14312 |
| alloc-test1 | new | 3.048 3.323 3.416 3.465 3.490 | 14304 |
| alloc-test1 | jemalloc | 2.843 3.179 3.193 3.257 3.258 | 13604 |
| alloc-test1 | new-percpu | 3.423 3.739 3.870 3.886 3.940 | 14256 |
| alloc-test1 | old-percpu | 3.049 3.061 3.339 3.490 3.504 | 14252 |
| alloc-testN | old | 4.403 4.451 4.504 4.817 4.874 | 17736 |
| alloc-testN | new | 4.412 4.452 4.484 4.598 4.803 | 17888 |
| alloc-testN | jemalloc | 4.088 4.143 4.187 4.227 4.234 | 17316 |
| alloc-testN | new-percpu | 5.354 5.389 5.449 5.536 5.614 | 18232 |
| alloc-testN | old-percpu | 4.643 4.655 4.677 4.697 4.945 | 17952 |
| valkey1 | old | 11.608 14.290 15.716 15.805 16.720 | 599704 |
| valkey1 | new | 11.192 11.615 15.720 15.943 16.159 | 599576 |
| valkey1 | jemalloc | 11.408 15.084 15.480 15.508 16.526 | 606780 |
| valkey1 | new-percpu | 5.891 8.169 8.525 9.444 9.526 | 599400 |
| valkey1 | old-percpu | 7.399 11.202 11.621 11.962 11.993 | 599572 |
| valkey1-free | old | 11.570 14.839 15.447 15.549 16.460 | 599760 |
| valkey1-free | new | 10.073 11.293 15.282 16.205 16.819 | 599692 |
| valkey1-free | jemalloc | 9.759 12.970 13.467 13.612 14.396 | 606612 |
| valkey1-free | new-percpu | 6.809 9.198 9.671 10.691 10.727 | 599460 |
| valkey1-free | old-percpu | 8.116 11.380 12.114 12.547 12.999 | 599632 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new-percpu | new-percpu RSS MiB | old-percpu | old-percpu RSS MiB | new vs old | jemalloc vs old | new-percpu vs old | old-percpu vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 43.5 | 16.8 | 44.8 | 17.1 | 43.8 | 18.0 | 25.2 | 14.6 | 42.6 | 17.7 | -2.9% | -0.7% | +72.6% | +2.1% |
| larson10 | M ops/s | 277.2 | 95.3 | 278.9 | 95.0 | 297.2 | 91.0 | 187.5 | 86.6 | 238.7 | 95.6 | -0.6% | -6.7% | +47.8% | +16.1% |
| larson16 | M ops/s | 305.9 | 119.4 | 305.2 | 117.2 | 336.0 | 127.6 | 205.2 | 100.2 | 260.1 | 107.2 | +0.2% | -9.0% | +49.1% | +17.6% |
| xmalloc | M frees/s | 322.706 | 53.0 | 324.697 | 53.2 | 329.034 | 89.6 | 239.663 | 57.0 | 291.431 | 57.6 | -0.6% | -1.9% | +34.6% | +10.7% |
| alloc-test1 | s | 3.335 | 14.0 | 3.416 | 14.0 | 3.193 | 13.3 | 3.870 | 13.9 | 3.339 | 13.9 | +2.4% | -4.3% | +16.0% | +0.1% |
| alloc-testN | s | 4.504 | 17.3 | 4.484 | 17.5 | 4.187 | 16.9 | 5.449 | 17.8 | 4.677 | 17.5 | -0.4% | -7.0% | +21.0% | +3.8% |
| valkey1 | M ops/s | 15.716 | 585.6 | 15.720 | 585.5 | 15.480 | 592.6 | 8.525 | 585.4 | 11.621 | 585.5 | -0.0% | +1.5% | +84.4% | +35.2% |
| valkey1-free | M ops/s | 15.447 | 585.7 | 15.282 | 585.6 | 13.467 | 592.4 | 9.671 | 585.4 | 12.114 | 585.6 | +1.1% | +14.7% | +59.7% | +27.5% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
