# challoc xmalloc-boxed, 2026-09-18, aarch64

- commit: 523ad84 (uncommitted changes in src/)
- date: 2026-09-18T08:59:32Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce-c4a), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 1.97 6.44 5.58; at end: 12.19 8.88 6.55; 104 s total
- method: 3 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than boxed.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - boxed: working tree (HEAD 523ad84 + uncommitted src changes) built into target/ab/tree
  - old: 523ad843725b built into target/ab/523ad843725b

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| xmallocN | boxed | 420.193 427.005 435.147 | 108924 |
| xmallocN | old | 485.829 486.563 489.191 | 183288 |
| larson10 | boxed | 369.0 371.9 373.0 | 116404 |
| larson10 | old | 366.7 370.1 370.5 | 120244 |

## Medians

| program | unit | boxed | boxed RSS MiB | old | old RSS MiB | old vs boxed |
|---|---|---:|---:|---:|---:|---:|
| xmallocN | M frees/s | 427.005 | 106.4 | 486.563 | 179.0 | -12.2% |
| larson10 | M ops/s | 371.9 | 113.7 | 370.1 | 117.4 | +0.5% |

Positive percentages mean the variant did worse than boxed (slower or lower throughput).
