# challoc A/B, 2026-09-18, aarch64

- commit: 5304cd4
- date: 2026-09-18T05:57:29Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.25 1.00 1.08; at end: 5.55 5.54 4.06; 936 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - old: 7efdae89e74c built into target/ab/7efdae89e74c
  - new: 5304cd46bcd8 built into target/ab/5304cd46bcd8
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.9 43.0 43.0 43.0 43.0 | 16948 |
| larson1 | new | 42.2 42.3 42.5 42.5 42.6 | 16948 |
| larson1 | jemalloc | 45.1 45.1 45.1 45.1 45.1 | 18576 |
| larson10 | old | 370.7 371.4 371.6 373.7 374.6 | 119472 |
| larson10 | new | 365.5 366.7 369.8 370.7 373.1 | 119220 |
| larson10 | jemalloc | 381.5 385.4 386.5 386.8 387.3 | 135488 |
| larson16 | old | 594.7 598.6 599.0 601.0 605.6 | 183464 |
| larson16 | new | 593.4 594.1 595.7 596.5 597.6 | 186788 |
| larson16 | jemalloc | 609.3 609.4 609.7 612.5 615.3 | 211432 |
| xmalloc | old | 564.232 566.481 566.839 567.307 568.444 | 85428 |
| xmalloc | new | 554.446 556.111 556.606 557.047 557.413 | 85432 |
| xmalloc | jemalloc | 474.784 478.266 480.828 482.105 486.217 | 84940 |
| alloc-test1 | old | 2.650 2.651 2.655 2.664 2.678 | 13312 |
| alloc-test1 | new | 2.660 2.666 2.667 2.672 2.689 | 13440 |
| alloc-test1 | jemalloc | 2.535 2.544 2.545 2.550 2.556 | 12636 |
| alloc-testN | old | 2.258 2.258 2.258 2.262 2.262 | 21108 |
| alloc-testN | new | 2.276 2.277 2.281 2.286 2.286 | 21244 |
| alloc-testN | jemalloc | 2.206 2.207 2.208 2.214 2.220 | 16860 |
| valkey1 | old | 6.954 6.985 7.082 7.084 7.105 | 601208 |
| valkey1 | new | 6.959 7.051 7.095 7.099 7.101 | 598948 |
| valkey1 | jemalloc | 7.620 7.706 7.760 7.785 7.815 | 599884 |
| valkey1-free | old | 6.626 6.775 6.782 6.791 6.797 | 601208 |
| valkey1-free | new | 7.189 7.278 7.317 7.333 7.339 | 598952 |
| valkey1-free | jemalloc | 7.135 7.137 7.144 7.183 7.313 | 599636 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 43.0 | 16.6 | 42.5 | 16.6 | 45.1 | 18.1 | +1.2% | -4.7% |
| larson10 | M ops/s | 371.6 | 116.7 | 369.8 | 116.4 | 386.5 | 132.3 | +0.5% | -3.9% |
| larson16 | M ops/s | 599.0 | 179.2 | 595.7 | 182.4 | 609.7 | 206.5 | +0.6% | -1.8% |
| xmalloc | M frees/s | 566.839 | 83.4 | 556.606 | 83.4 | 480.828 | 82.9 | +1.8% | +17.9% |
| alloc-test1 | s | 2.655 | 13.0 | 2.667 | 13.1 | 2.545 | 12.3 | +0.5% | -4.1% |
| alloc-testN | s | 2.258 | 20.6 | 2.281 | 20.7 | 2.208 | 16.5 | +1.0% | -2.2% |
| valkey1 | M ops/s | 7.082 | 587.1 | 7.095 | 584.9 | 7.760 | 585.8 | -0.2% | -8.7% |
| valkey1-free | M ops/s | 6.782 | 587.1 | 7.317 | 584.9 | 7.144 | 585.6 | -7.3% | -5.1% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
