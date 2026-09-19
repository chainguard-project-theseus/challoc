# challoc Phase 2.5 task 10: two-advice purge with dirty_decay_ms:0 (every Empty slab purged at the next tick), lazy band on vs off, aarch64, docker --memory=8g, 2026-09-19, aarch64

- commit: 9a19632
- date: 2026-09-19T18:16:44Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 4.09 4.10 2.69; at end: 4.76 4.37 2.97; 127 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than eager0.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - eager0: 9a19632a15ff built into target/ab/9a19632a15ff, MALLOC_CONF=dirty_decay_ms:0,lazy_band:false
  - lazy0: 9a19632a15ff built into target/ab/9a19632a15ff, MALLOC_CONF=dirty_decay_ms:0

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| mstress | eager0 | 0.964 0.981 1.002 1.003 1.006 | 265804 |
| mstress | lazy0 | 0.511 0.513 0.516 0.517 0.524 | 321396 |
| alloc-testN | eager0 | 2.265 2.267 2.280 2.280 2.283 | 16516 |
| alloc-testN | lazy0 | 2.268 2.274 2.281 2.283 2.290 | 16824 |
| larson10 | eager0 | 347.4 348.1 348.1 352.2 353.5 | 99560 |
| larson10 | lazy0 | 357.6 359.5 359.9 361.2 361.8 | 115772 |

## Medians

| program | unit | eager0 | eager0 RSS MiB | lazy0 | lazy0 RSS MiB | lazy0 vs eager0 |
|---|---|---:|---:|---:|---:|---:|
| mstress | s | 1.002 | 259.6 | 0.516 | 313.9 | -48.5% |
| alloc-testN | s | 2.280 | 16.1 | 2.281 | 16.4 | +0.0% |
| larson10 | M ops/s | 348.1 | 97.2 | 359.9 | 113.1 | -3.3% |

Positive percentages mean the variant did worse than eager0 (slower or lower throughput).
