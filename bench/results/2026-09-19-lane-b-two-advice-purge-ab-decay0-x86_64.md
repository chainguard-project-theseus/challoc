# challoc Phase 2.5 task 10: two-advice purge with dirty_decay_ms:0 (every Empty slab purged at the next tick), lazy band on vs off, x86_64, docker --memory=8g, 2026-09-19, x86_64

- commit: 9a19632
- date: 2026-09-19T18:16:46Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 4.86 4.64 3.00; at end: 8.54 6.64 4.03; 156 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than eager0.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - eager0: 9a19632a15ff built into target/ab/9a19632a15ff, MALLOC_CONF=dirty_decay_ms:0,lazy_band:false
  - lazy0: 9a19632a15ff built into target/ab/9a19632a15ff, MALLOC_CONF=dirty_decay_ms:0

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| mstress | eager0 | 1.074 1.079 1.087 1.088 1.093 | 270384 |
| mstress | lazy0 | 0.908 0.914 0.915 0.918 0.919 | 331292 |
| alloc-testN | eager0 | 4.363 4.424 4.449 4.481 4.500 | 17556 |
| alloc-testN | lazy0 | 4.358 4.377 4.380 4.397 4.398 | 17700 |
| larson10 | eager0 | 233.5 235.8 236.1 236.7 238.9 | 80888 |
| larson10 | lazy0 | 235.4 235.7 238.6 239.5 244.4 | 96528 |

## Medians

| program | unit | eager0 | eager0 RSS MiB | lazy0 | lazy0 RSS MiB | lazy0 vs eager0 |
|---|---|---:|---:|---:|---:|---:|
| mstress | s | 1.087 | 264.0 | 0.915 | 323.5 | -15.8% |
| alloc-testN | s | 4.449 | 17.1 | 4.380 | 17.3 | -1.6% |
| larson10 | M ops/s | 236.1 | 79.0 | 238.6 | 94.3 | -1.0% |

Positive percentages mean the variant did worse than eager0 (slower or lower throughput).
