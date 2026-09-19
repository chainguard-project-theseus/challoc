# challoc Flush sort A/B, 2026-09-19, x86_64

- commit: 3ed77b7
- date: 2026-09-19T06:16:56Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (GCE c4-standard-16 (Intel Xeon 6, Granite Rapids), zone us-east4-a), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 0.35 1.75 2.21; at end: 3.46 2.67 2.48; 262 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: 1339eb6ee395 built into target/ab/1339eb6ee395
  - new: 3ed77b760093 built into target/ab/3ed77b760093

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| glibc-simple | old | 2.036 2.059 2.092 2.122 2.123 | 2708 |
| glibc-simple | new | 1.887 1.890 1.904 1.905 1.916 | 2600 |
| python1-malloc | old | 4.231 4.239 4.242 4.427 4.483 | 78240 |
| python1-malloc | new | 4.144 4.151 4.155 4.159 4.230 | 78180 |
| larson1 | old | 46.0 46.1 47.9 48.4 48.6 | 17576 |
| larson1 | new | 46.1 46.6 47.9 48.0 48.8 | 17376 |
| larson10 | old | 282.8 283.3 289.9 290.3 298.4 | 98828 |
| larson10 | new | 282.6 286.1 288.0 291.4 292.8 | 98812 |
| mstress | old | 0.611 0.611 0.619 0.632 0.638 | 342920 |
| mstress | new | 0.602 0.603 0.608 0.620 0.622 | 351696 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | new vs old |
|---|---|---:|---:|---:|---:|---:|
| glibc-simple | s | 2.092 | 2.6 | 1.904 | 2.5 | -9.0% |
| python1-malloc | s | 4.242 | 76.4 | 4.155 | 76.3 | -2.1% |
| larson1 | M ops/s | 47.9 | 17.2 | 47.9 | 17.0 | +0.0% |
| larson10 | M ops/s | 289.9 | 96.5 | 288.0 | 96.5 | +0.7% |
| mstress | s | 0.619 | 334.9 | 0.608 | 343.5 | -1.8% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
