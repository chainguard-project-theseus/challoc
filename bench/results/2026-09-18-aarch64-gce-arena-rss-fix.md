# challoc arena-rss-fix, 2026-09-18, aarch64

- commit: 523ad84 (uncommitted changes in src/)
- date: 2026-09-18T08:46:41Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce-c4a), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 1.27 0.81 1.92; at end: 13.35 9.37 5.58; 445 s total
- method: 3 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - old: 523ad843725b built into target/ab/523ad843725b
  - new: working tree (HEAD 523ad84 + uncommitted src changes) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.3 42.5 42.6 | 16928 |
| larson1 | new | 42.4 42.6 42.7 | 16056 |
| larson1 | jemalloc | 45.0 45.1 45.2 | 18180 |
| larson10 | old | 370.9 372.3 372.8 | 118836 |
| larson10 | new | 366.3 370.9 374.5 | 116792 |
| larson10 | jemalloc | 382.6 386.7 387.3 | 134996 |
| larson16 | old | 597.9 598.6 599.1 | 184620 |
| larson16 | new | 589.9 597.6 600.1 | 177448 |
| larson16 | jemalloc | 612.3 613.9 616.3 | 211112 |
| xmalloc | old | 559.753 560.644 562.600 | 89080 |
| xmalloc | new | 536.409 538.397 547.469 | 56252 |
| xmalloc | jemalloc | 476.579 481.478 482.196 | 84460 |
| xmallocN | old | 485.902 489.085 489.805 | 183992 |
| xmallocN | new | 410.247 413.265 421.914 | 107572 |
| xmallocN | jemalloc | 484.763 486.820 488.054 | 113356 |
| mstress | old | 0.395 0.397 0.401 | 385308 |
| mstress | new | 0.382 0.384 0.384 | 353844 |
| mstress | jemalloc | 0.643 0.643 0.653 | 298572 |
| alloc-test1 | old | 2.606 2.615 2.641 | 13440 |
| alloc-test1 | new | 2.591 2.593 2.614 | 13176 |
| alloc-test1 | jemalloc | 2.488 2.492 2.511 | 12636 |
| alloc-testN | old | 2.284 2.325 2.415 | 21120 |
| alloc-testN | new | 2.266 2.268 2.277 | 16812 |
| alloc-testN | jemalloc | 2.207 2.216 2.216 | 16840 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.5 | 16.5 | 42.6 | 15.7 | 45.1 | 17.8 | -0.2% | -5.8% |
| larson10 | M ops/s | 372.3 | 116.1 | 370.9 | 114.1 | 386.7 | 131.8 | +0.4% | -3.7% |
| larson16 | M ops/s | 598.6 | 180.3 | 597.6 | 173.3 | 613.9 | 206.2 | +0.2% | -2.5% |
| xmalloc | M frees/s | 560.644 | 87.0 | 538.397 | 54.9 | 481.478 | 82.5 | +4.1% | +16.4% |
| xmallocN | M frees/s | 489.085 | 179.7 | 413.265 | 105.1 | 486.820 | 110.7 | +18.3% | +0.5% |
| mstress | s | 0.397 | 376.3 | 0.384 | 345.6 | 0.643 | 291.6 | -3.3% | +62.0% |
| alloc-test1 | s | 2.615 | 13.1 | 2.593 | 12.9 | 2.492 | 12.3 | -0.8% | -4.7% |
| alloc-testN | s | 2.325 | 20.6 | 2.268 | 16.4 | 2.216 | 16.4 | -2.5% | -4.7% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
