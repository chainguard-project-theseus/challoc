# challoc A/B, 2026-09-18, x86_64

- commit: e18fbb4
- date: 2026-09-18T22:03:31Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce-c4), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 2.26 6.49 6.03; at end: 1.82 5.98 5.88; 29 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than new.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - new: e18fbb4890b5 built into target/ab/e18fbb4890b5
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| glibc-simple | new | 2.044 2.061 2.067 2.071 2.084 | 2544 |
| glibc-simple | jemalloc | 1.924 1.939 1.940 1.950 1.952 | 5028 |

## Medians

| program | unit | new | new RSS MiB | jemalloc | jemalloc RSS MiB | jemalloc vs new |
|---|---|---:|---:|---:|---:|---:|
| glibc-simple | s | 2.067 | 2.5 | 1.940 | 4.9 | -6.1% |

Positive percentages mean the variant did worse than new (slower or lower throughput).
