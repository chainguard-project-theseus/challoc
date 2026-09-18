# challoc arena-rss-fix2, 2026-09-18, aarch64

- commit: 523ad84 (uncommitted changes in src/)
- date: 2026-09-18T09:09:01Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce-c4a), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 3.40 3.20 4.54; at end: 11.87 9.85 7.28; 445 s total
- method: 3 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - old: 523ad843725b built into target/ab/523ad843725b
  - new: working tree (HEAD 523ad84 + uncommitted src changes) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.3 42.6 42.6 | 16948 |
| larson1 | new | 42.7 42.8 42.8 | 16188 |
| larson1 | jemalloc | 45.0 45.1 45.1 | 18316 |
| larson10 | old | 364.3 366.8 367.7 | 118688 |
| larson10 | new | 371.6 372.2 372.4 | 116156 |
| larson10 | jemalloc | 381.5 382.6 387.9 | 135172 |
| larson16 | old | 592.6 592.9 595.7 | 181300 |
| larson16 | new | 593.4 594.7 599.5 | 178748 |
| larson16 | jemalloc | 611.6 614.7 616.7 | 210876 |
| xmalloc | old | 559.597 560.223 561.942 | 84396 |
| xmalloc | new | 564.294 565.231 565.974 | 57024 |
| xmalloc | jemalloc | 474.899 481.473 485.937 | 80688 |
| xmallocN | old | 482.339 488.921 489.249 | 182968 |
| xmallocN | new | 498.221 500.578 500.836 | 109056 |
| xmallocN | jemalloc | 483.740 488.214 489.346 | 113648 |
| mstress | old | 0.397 0.399 0.413 | 383904 |
| mstress | new | 0.379 0.380 0.389 | 326324 |
| mstress | jemalloc | 0.643 0.644 0.662 | 299612 |
| alloc-test1 | old | 2.609 2.673 2.709 | 13440 |
| alloc-test1 | new | 2.596 2.606 2.640 | 13192 |
| alloc-test1 | jemalloc | 2.505 2.508 2.551 | 12636 |
| alloc-testN | old | 2.276 2.286 2.291 | 21376 |
| alloc-testN | new | 2.262 2.276 2.279 | 16696 |
| alloc-testN | jemalloc | 2.206 2.207 2.211 | 16856 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.6 | 16.6 | 42.8 | 15.8 | 45.1 | 17.9 | -0.5% | -5.5% |
| larson10 | M ops/s | 366.8 | 115.9 | 372.2 | 113.4 | 382.6 | 132.0 | -1.5% | -4.1% |
| larson16 | M ops/s | 592.9 | 177.1 | 594.7 | 174.6 | 614.7 | 205.9 | -0.3% | -3.5% |
| xmalloc | M frees/s | 560.223 | 82.4 | 565.231 | 55.7 | 481.473 | 78.8 | -0.9% | +16.4% |
| xmallocN | M frees/s | 488.921 | 178.7 | 500.578 | 106.5 | 488.214 | 111.0 | -2.3% | +0.1% |
| mstress | s | 0.399 | 374.9 | 0.380 | 318.7 | 0.644 | 292.6 | -4.8% | +61.4% |
| alloc-test1 | s | 2.673 | 13.1 | 2.606 | 12.9 | 2.508 | 12.3 | -2.5% | -6.2% |
| alloc-testN | s | 2.286 | 20.9 | 2.276 | 16.3 | 2.207 | 16.5 | -0.4% | -3.5% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
