# challoc xmalloc-boxed, 2026-09-18, x86_64

- commit: 523ad84 (uncommitted changes in src/)
- date: 2026-09-18T09:00:26Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce-c4), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.19 4.21 4.95; at end: 13.48 8.57 6.50; 152 s total
- method: 3 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than boxed.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - boxed: working tree (HEAD 523ad84 + uncommitted src changes) built into target/ab/tree
  - old: 523ad843725b built into target/ab/523ad843725b
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| xmallocN | boxed | 242.431 243.371 245.612 | 108972 |
| xmallocN | old | 324.200 325.232 325.730 | 181144 |
| xmallocN | jemalloc | 332.731 334.170 334.775 | 126964 |
| larson10 | boxed | 269.9 282.0 284.8 | 95720 |
| larson10 | old | 282.6 282.8 285.2 | 99592 |
| larson10 | jemalloc | 287.7 292.7 302.1 | 95704 |

## Medians

| program | unit | boxed | boxed RSS MiB | old | old RSS MiB | jemalloc | jemalloc RSS MiB | old vs boxed | jemalloc vs boxed |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| xmallocN | M frees/s | 243.371 | 106.4 | 325.232 | 176.9 | 334.170 | 124.0 | -25.2% | -27.2% |
| larson10 | M ops/s | 282.0 | 93.5 | 282.8 | 97.3 | 292.7 | 93.5 | -0.3% | -3.7% |

Positive percentages mean the variant did worse than boxed (slower or lower throughput).
