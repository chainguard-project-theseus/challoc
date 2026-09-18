# challoc xmalloc-isolation, 2026-09-18, aarch64

- commit: 523ad84 (uncommitted changes in src/)
- date: 2026-09-18T08:55:07Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce-c4a), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 4.69 7.58 5.22; at end: 18.76 10.90 6.51; 68 s total
- method: 3 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than new.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - new: working tree (HEAD 523ad84 + uncommitted src changes) built into target/ab/tree
  - big: working tree (HEAD 523ad84 + uncommitted src changes) built into target/ab/tree, MALLOC_CONF=tcache_max:262144
  - old: 523ad843725b built into target/ab/523ad843725b

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| xmallocN | new | 416.640 422.182 422.503 | 106812 |
| xmallocN | big | 411.159 431.688 437.390 | 110764 |
| xmallocN | old | 486.512 489.251 490.740 | 191672 |

## Medians

| program | unit | new | new RSS MiB | big | big RSS MiB | old | old RSS MiB | big vs new | old vs new |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| xmallocN | M frees/s | 422.182 | 104.3 | 431.688 | 108.2 | 489.251 | 187.2 | -2.2% | -13.7% |

Positive percentages mean the variant did worse than new (slower or lower throughput).
