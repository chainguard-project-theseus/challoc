# challoc Phase 2.5 task 8 fast path: mode vs default vs old, 2026-09-20, aarch64

- commit: ee0ecd8 (uncommitted changes in src/)
- date: 2026-09-20T00:23:18Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 3.54 3.50 2.00; at end: 6.68 6.26 4.42; 774 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: dfe3f606134d built into target/ab/dfe3f606134d
  - new: working tree (HEAD ee0ecd8 + uncommitted src changes) built into target/ab/tree, MALLOC_CONF=percpu_arena:percpu
  - new-off: working tree (HEAD ee0ecd8 + uncommitted src changes) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 41.6 41.7 41.7 41.7 41.7 | 16252 |
| larson1 | new | 39.3 39.5 39.6 39.6 39.6 | 16664 |
| larson1 | new-off | 41.8 41.9 42.0 42.0 42.1 | 16400 |
| larson1 | jemalloc | 44.7 44.9 45.0 45.0 45.1 | 18928 |
| larson10 | old | 361.8 362.6 364.1 364.2 368.5 | 114292 |
| larson10 | new | 329.3 330.4 332.8 335.1 335.3 | 125720 |
| larson10 | new-off | 364.4 364.6 365.6 365.7 369.1 | 113304 |
| larson10 | jemalloc | 381.8 382.2 383.6 387.3 390.1 | 135284 |
| xmalloc | old | 580.142 580.620 582.076 582.555 582.657 | 54352 |
| xmalloc | new | 484.832 487.595 489.432 490.354 490.896 | 84092 |
| xmalloc | new-off | 578.869 578.948 578.982 580.003 580.535 | 55440 |
| xmalloc | jemalloc | 475.304 477.147 480.789 480.977 482.980 | 81000 |
| valkey1 | old | 7.100 7.152 7.204 7.209 7.215 | 598668 |
| valkey1 | new | 6.755 6.876 6.883 6.890 6.937 | 598660 |
| valkey1 | new-off | 7.141 7.175 7.227 7.258 7.295 | 598608 |
| valkey1 | jemalloc | 6.997 7.123 7.150 7.215 7.325 | 599620 |
| alloc-testN | old | 2.313 2.321 2.323 2.324 2.331 | 16828 |
| alloc-testN | new | 2.431 2.434 2.434 2.451 2.462 | 16596 |
| alloc-testN | new-off | 2.292 2.299 2.306 2.318 2.322 | 16740 |
| alloc-testN | jemalloc | 2.199 2.210 2.217 2.218 2.220 | 16972 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | new-off | new-off RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | new-off vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.7 | 15.9 | 39.6 | 16.3 | 42.0 | 16.0 | 45.0 | 18.5 | +5.3% | -0.7% | -7.3% |
| larson10 | M ops/s | 364.1 | 111.6 | 332.8 | 122.8 | 365.6 | 110.6 | 383.6 | 132.1 | +9.4% | -0.4% | -5.1% |
| xmalloc | M frees/s | 582.076 | 53.1 | 489.432 | 82.1 | 578.982 | 54.1 | 480.789 | 79.1 | +18.9% | +0.5% | +21.1% |
| valkey1 | M ops/s | 7.204 | 584.6 | 6.883 | 584.6 | 7.227 | 584.6 | 7.150 | 585.6 | +4.7% | -0.3% | +0.8% |
| alloc-testN | s | 2.323 | 16.4 | 2.434 | 16.2 | 2.306 | 16.3 | 2.217 | 16.6 | +4.8% | -0.7% | -4.6% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
