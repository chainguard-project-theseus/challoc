# challoc arena-rss-fix2, 2026-09-18, x86_64

- commit: 523ad84 (uncommitted changes in src/)
- date: 2026-09-18T09:09:03Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce-c4), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 6.75 4.46 5.06; at end: 12.30 9.98 7.52; 478 s total
- method: 3 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - old: 523ad843725b built into target/ab/523ad843725b
  - new: working tree (HEAD 523ad84 + uncommitted src changes) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 46.2 47.7 48.0 | 16316 |
| larson1 | new | 45.8 48.3 48.3 | 15644 |
| larson1 | jemalloc | 46.5 46.7 47.3 | 18632 |
| larson10 | old | 287.3 294.0 298.1 | 100872 |
| larson10 | new | 282.2 289.8 297.8 | 96336 |
| larson10 | jemalloc | 296.6 299.4 308.4 | 95932 |
| larson16 | old | 315.8 318.7 322.5 | 129092 |
| larson16 | new | 313.8 315.3 322.2 | 121576 |
| larson16 | jemalloc | 331.9 339.6 340.6 | 130748 |
| xmalloc | old | 347.196 348.349 350.223 | 83092 |
| xmalloc | new | 323.522 339.224 354.393 | 54780 |
| xmalloc | jemalloc | 335.610 336.354 337.875 | 99580 |
| xmallocN | old | 326.754 331.309 332.679 | 186212 |
| xmallocN | new | 322.516 327.657 327.878 | 112204 |
| xmallocN | jemalloc | 336.101 338.168 339.581 | 121084 |
| mstress | old | 0.654 0.657 0.660 | 404568 |
| mstress | new | 0.621 0.623 0.629 | 346064 |
| mstress | jemalloc | 1.044 1.064 1.072 | 292000 |
| alloc-test1 | old | 2.969 3.002 3.234 | 14208 |
| alloc-test1 | new | 2.974 2.995 3.121 | 13944 |
| alloc-test1 | jemalloc | 2.795 2.801 2.853 | 13480 |
| alloc-testN | old | 4.387 4.437 4.492 | 22200 |
| alloc-testN | new | 4.326 4.332 4.453 | 17532 |
| alloc-testN | jemalloc | 3.988 4.002 4.060 | 17064 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 47.7 | 15.9 | 48.3 | 15.3 | 46.7 | 18.2 | -1.2% | +2.1% |
| larson10 | M ops/s | 294.0 | 98.5 | 289.8 | 94.1 | 299.4 | 93.7 | +1.4% | -1.8% |
| larson16 | M ops/s | 318.7 | 126.1 | 315.3 | 118.7 | 339.6 | 127.7 | +1.1% | -6.2% |
| xmalloc | M frees/s | 348.349 | 81.1 | 339.224 | 53.5 | 336.354 | 97.2 | +2.7% | +3.6% |
| xmallocN | M frees/s | 331.309 | 181.8 | 327.657 | 109.6 | 338.168 | 118.2 | +1.1% | -2.0% |
| mstress | s | 0.657 | 395.1 | 0.623 | 338.0 | 1.064 | 285.2 | -5.2% | +61.9% |
| alloc-test1 | s | 3.002 | 13.9 | 2.995 | 13.6 | 2.801 | 13.2 | -0.2% | -6.7% |
| alloc-testN | s | 4.437 | 21.7 | 4.332 | 17.1 | 4.002 | 16.7 | -2.4% | -9.8% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
