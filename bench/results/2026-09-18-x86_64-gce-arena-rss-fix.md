# challoc arena-rss-fix, 2026-09-18, x86_64

- commit: 523ad84 (uncommitted changes in src/)
- date: 2026-09-18T08:48:15Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce-c4), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 3.44 3.22 3.36; at end: 12.33 9.75 6.50; 478 s total
- method: 3 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - old: 523ad843725b built into target/ab/523ad843725b
  - new: working tree (HEAD 523ad84 + uncommitted src changes) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 44.2 46.4 47.4 | 16304 |
| larson1 | new | 46.8 46.9 47.1 | 15576 |
| larson1 | jemalloc | 43.8 45.4 46.0 | 15908 |
| larson10 | old | 272.1 298.1 298.4 | 101908 |
| larson10 | new | 282.5 283.1 291.0 | 95892 |
| larson10 | jemalloc | 295.5 305.6 308.1 | 95064 |
| larson16 | old | 317.8 318.7 321.6 | 130496 |
| larson16 | new | 318.2 320.7 321.0 | 127000 |
| larson16 | jemalloc | 329.2 336.8 337.5 | 130180 |
| xmalloc | old | 338.670 349.272 363.619 | 83216 |
| xmalloc | new | 327.644 331.181 351.608 | 56208 |
| xmalloc | jemalloc | 333.409 333.567 335.122 | 94816 |
| xmallocN | old | 327.905 328.498 329.505 | 185116 |
| xmallocN | new | 245.922 246.836 248.322 | 106204 |
| xmallocN | jemalloc | 335.991 336.382 337.283 | 127776 |
| mstress | old | 0.653 0.664 0.679 | 397756 |
| mstress | new | 0.643 0.649 0.651 | 385844 |
| mstress | jemalloc | 1.051 1.063 1.070 | 292276 |
| alloc-test1 | old | 2.999 3.015 3.029 | 14240 |
| alloc-test1 | new | 3.055 3.073 3.166 | 13984 |
| alloc-test1 | jemalloc | 2.850 2.880 2.886 | 13400 |
| alloc-testN | old | 4.371 4.374 4.398 | 22072 |
| alloc-testN | new | 4.361 4.368 4.377 | 17580 |
| alloc-testN | jemalloc | 4.080 4.086 4.089 | 17136 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 46.4 | 15.9 | 46.9 | 15.2 | 45.4 | 15.5 | -1.1% | +2.2% |
| larson10 | M ops/s | 298.1 | 99.5 | 283.1 | 93.6 | 305.6 | 92.8 | +5.3% | -2.5% |
| larson16 | M ops/s | 318.7 | 127.4 | 320.7 | 124.0 | 336.8 | 127.1 | -0.6% | -5.4% |
| xmalloc | M frees/s | 349.272 | 81.3 | 331.181 | 54.9 | 333.567 | 92.6 | +5.5% | +4.7% |
| xmallocN | M frees/s | 328.498 | 180.8 | 246.836 | 103.7 | 336.382 | 124.8 | +33.1% | -2.3% |
| mstress | s | 0.664 | 388.4 | 0.649 | 376.8 | 1.063 | 285.4 | -2.3% | +60.1% |
| alloc-test1 | s | 3.015 | 13.9 | 3.073 | 13.7 | 2.880 | 13.1 | +1.9% | -4.5% |
| alloc-testN | s | 4.374 | 21.6 | 4.368 | 17.2 | 4.086 | 16.7 | -0.1% | -6.6% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
