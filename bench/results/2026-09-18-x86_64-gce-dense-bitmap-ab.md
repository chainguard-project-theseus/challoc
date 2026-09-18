# challoc A/B, 2026-09-18, x86_64

- commit: 5304cd4
- date: 2026-09-18T05:57:33Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.61 0.87 0.62; at end: 5.61 5.71 4.12; 985 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - old: 7efdae89e74c built into target/ab/7efdae89e74c
  - new: 5304cd46bcd8 built into target/ab/5304cd46bcd8
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.5 46.5 46.9 47.0 48.9 | 16104 |
| larson1 | new | 41.9 44.4 44.8 46.1 47.3 | 16300 |
| larson1 | jemalloc | 42.6 45.2 45.3 45.4 45.4 | 18148 |
| larson10 | old | 275.5 275.8 281.3 294.2 299.4 | 102756 |
| larson10 | new | 278.5 281.9 282.5 290.4 291.1 | 100444 |
| larson10 | jemalloc | 286.2 289.8 295.7 299.6 301.3 | 94124 |
| larson16 | old | 300.1 309.3 309.5 316.0 318.3 | 128656 |
| larson16 | new | 300.5 302.7 306.8 310.0 315.3 | 127592 |
| larson16 | jemalloc | 333.0 334.4 334.7 336.8 338.2 | 131212 |
| xmalloc | old | 335.416 343.249 345.986 353.889 361.442 | 83200 |
| xmalloc | new | 331.782 331.798 341.172 343.766 347.477 | 83024 |
| xmalloc | jemalloc | 320.819 328.530 329.231 330.536 330.871 | 98088 |
| alloc-test1 | old | 2.858 2.976 3.249 3.293 3.340 | 14004 |
| alloc-test1 | new | 3.294 3.314 3.320 3.333 3.388 | 14192 |
| alloc-test1 | jemalloc | 3.131 3.138 3.178 3.178 3.192 | 13328 |
| alloc-testN | old | 4.237 4.346 4.417 4.422 4.559 | 21520 |
| alloc-testN | new | 4.426 4.454 4.471 4.516 4.548 | 21944 |
| alloc-testN | jemalloc | 4.090 4.114 4.119 4.138 4.387 | 17080 |
| valkey1 | old | 15.297 15.313 15.317 15.478 16.283 | 602184 |
| valkey1 | new | 15.024 15.302 15.318 15.408 15.750 | 599824 |
| valkey1 | jemalloc | 14.240 14.515 14.993 15.132 15.345 | 606552 |
| valkey1-free | old | 14.025 14.044 14.443 14.516 14.951 | 602060 |
| valkey1-free | new | 14.874 15.058 15.340 15.461 15.521 | 599644 |
| valkey1-free | jemalloc | 12.048 12.996 13.082 13.152 13.195 | 606564 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 46.9 | 15.7 | 44.8 | 15.9 | 45.3 | 17.7 | +4.7% | +3.5% |
| larson10 | M ops/s | 281.3 | 100.3 | 282.5 | 98.1 | 295.7 | 91.9 | -0.4% | -4.9% |
| larson16 | M ops/s | 309.5 | 125.6 | 306.8 | 124.6 | 334.7 | 128.1 | +0.9% | -7.5% |
| xmalloc | M frees/s | 345.986 | 81.2 | 341.172 | 81.1 | 329.231 | 95.8 | +1.4% | +5.1% |
| alloc-test1 | s | 3.249 | 13.7 | 3.320 | 13.9 | 3.178 | 13.0 | +2.2% | -2.2% |
| alloc-testN | s | 4.417 | 21.0 | 4.471 | 21.4 | 4.119 | 16.7 | +1.2% | -6.7% |
| valkey1 | M ops/s | 15.317 | 588.1 | 15.318 | 585.8 | 14.993 | 592.3 | -0.0% | +2.2% |
| valkey1-free | M ops/s | 14.443 | 587.9 | 15.340 | 585.6 | 13.082 | 592.3 | -5.8% | +10.4% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
