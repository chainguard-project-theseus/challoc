# challoc Flush sort A/B, 2026-09-19, aarch64

- commit: 3ed77b7
- date: 2026-09-19T06:16:55Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (GCE c4a-standard-16 (Google Axion, Neoverse V2), zone us-east4-a), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 0.32 1.52 1.89; at end: 3.58 2.62 2.25; 262 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: 1339eb6ee395 built into target/ab/1339eb6ee395
  - new: 3ed77b760093 built into target/ab/3ed77b760093

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| glibc-simple | old | 2.286 2.288 2.298 2.302 2.314 | 2344 |
| glibc-simple | new | 2.185 2.188 2.189 2.207 2.209 | 2332 |
| python1-malloc | old | 4.253 4.260 4.260 4.261 4.278 | 77312 |
| python1-malloc | new | 4.117 4.122 4.131 4.134 4.139 | 77316 |
| larson1 | old | 42.4 42.4 42.4 42.4 42.4 | 16024 |
| larson1 | new | 42.3 42.3 42.4 42.4 42.4 | 16268 |
| larson10 | old | 361.0 362.7 364.7 365.8 368.0 | 115096 |
| larson10 | new | 362.2 363.7 365.7 365.8 366.1 | 114312 |
| mstress | old | 0.379 0.379 0.380 0.380 0.381 | 328504 |
| mstress | new | 0.370 0.372 0.372 0.373 0.374 | 327956 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | new vs old |
|---|---|---:|---:|---:|---:|---:|
| glibc-simple | s | 2.298 | 2.3 | 2.189 | 2.3 | -4.7% |
| python1-malloc | s | 4.260 | 75.5 | 4.131 | 75.5 | -3.0% |
| larson1 | M ops/s | 42.4 | 15.6 | 42.4 | 15.9 | +0.0% |
| larson10 | M ops/s | 364.7 | 112.4 | 365.7 | 111.6 | -0.3% |
| mstress | s | 0.380 | 320.8 | 0.372 | 320.3 | -2.1% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
