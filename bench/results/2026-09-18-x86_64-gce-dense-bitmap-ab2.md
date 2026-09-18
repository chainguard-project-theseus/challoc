# challoc A/B, 2026-09-18, x86_64

- commit: 5304cd4
- date: 2026-09-18T06:14:45Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 2.51 4.85 3.90; at end: 0.97 2.12 2.94; 345 s total
- method: 10 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - old: 7efdae89e74c built into target/ab/7efdae89e74c
  - new: 5304cd46bcd8 built into target/ab/5304cd46bcd8
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 20.8 41.9 45.3 45.4 45.6 45.7 46.5 47.7 48.0 48.8 | 16040 |
| larson1 | new | 43.5 44.7 44.9 45.0 45.0 45.0 45.5 46.8 47.2 48.0 | 16360 |
| larson1 | jemalloc | 40.7 41.7 42.7 43.6 43.7 44.0 44.4 45.0 45.1 47.8 | 18060 |
| alloc-test1 | old | 2.913 2.940 3.063 3.271 3.352 3.355 3.360 3.367 3.375 3.376 | 13964 |
| alloc-test1 | new | 2.976 2.983 3.009 3.367 3.382 3.405 3.406 3.414 3.418 3.430 | 14104 |
| alloc-test1 | jemalloc | 2.803 2.808 2.918 2.945 3.200 3.233 3.234 3.235 3.238 3.240 | 13328 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.6 | 15.7 | 45.0 | 16.0 | 43.7 | 17.6 | +1.3% | +4.3% |
| alloc-test1 | s | 3.352 | 13.6 | 3.382 | 13.8 | 3.200 | 13.0 | +0.9% | -4.5% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
