# challoc Phase 2.5 task 10: two-advice purge, lazy band on vs off, aarch64, docker --memory=8g, 2026-09-19, aarch64

- commit: 089c465
- date: 2026-09-19T18:08:26Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 4.28 1.80 0.92; at end: 10.71 5.49 2.51; 202 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than base.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - base: a3ddd6d094f7 built into target/ab/a3ddd6d094f7
  - eager: 089c4652cdf4 built into target/ab/089c4652cdf4, MALLOC_CONF=lazy_band:false
  - lazy: 089c4652cdf4 built into target/ab/089c4652cdf4

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| mstress | base | 0.371 0.372 0.377 0.377 0.379 | 325852 |
| mstress | eager | 0.373 0.374 0.377 0.378 0.378 | 331252 |
| mstress | lazy | 0.371 0.372 0.375 0.376 0.381 | 324464 |
| alloc-testN | base | 2.272 2.274 2.277 2.281 2.286 | 16832 |
| alloc-testN | eager | 2.272 2.279 2.282 2.284 2.308 | 16824 |
| alloc-testN | lazy | 2.266 2.280 2.281 2.282 2.285 | 16708 |
| larson10 | base | 367.4 369.1 370.2 370.4 371.1 | 115896 |
| larson10 | eager | 365.6 367.7 367.9 370.9 372.8 | 114876 |
| larson10 | lazy | 367.2 367.7 368.4 369.3 373.6 | 116284 |

## Medians

| program | unit | base | base RSS MiB | eager | eager RSS MiB | lazy | lazy RSS MiB | eager vs base | lazy vs base |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| mstress | s | 0.377 | 318.2 | 0.377 | 323.5 | 0.375 | 316.9 | +0.0% | -0.5% |
| alloc-testN | s | 2.277 | 16.4 | 2.282 | 16.4 | 2.281 | 16.3 | +0.2% | +0.2% |
| larson10 | M ops/s | 370.2 | 113.2 | 367.9 | 112.2 | 368.4 | 113.6 | +0.6% | +0.5% |

Positive percentages mean the variant did worse than base (slower or lower throughput).
