# challoc A/B, 2026-09-19, aarch64

- commit: 4864d4b
- date: 2026-09-19T22:39:01Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 4.03 2.47 2.54; at end: 5.12 4.94 3.98; 792 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: 9351eeb4b827 built into target/ab/9351eeb4b827
  - new: working tree (HEAD 4864d4b) built into target/ab/tree, MALLOC_CONF=percpu_arena:percpu
  - new-off: working tree (HEAD 4864d4b) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.4 42.5 42.6 42.6 42.6 | 16252 |
| larson1 | new | 36.9 37.1 37.1 37.2 37.2 | 16372 |
| larson1 | new-off | 41.6 41.7 41.7 41.7 41.8 | 16252 |
| larson1 | jemalloc | 45.0 45.1 45.2 45.2 45.2 | 19072 |
| larson10 | old | 364.6 366.7 366.8 368.3 370.2 | 114940 |
| larson10 | new | 309.1 309.7 310.4 311.1 311.9 | 121212 |
| larson10 | new-off | 360.0 360.1 362.4 366.0 366.4 | 115056 |
| larson10 | jemalloc | 385.5 385.8 385.9 387.9 389.4 | 135436 |
| xmalloc | old | 586.003 586.652 586.686 587.221 587.639 | 54224 |
| xmalloc | new | 466.936 469.068 469.201 469.326 474.509 | 75592 |
| xmalloc | new-off | 576.571 577.495 578.143 578.589 580.323 | 54608 |
| xmalloc | jemalloc | 483.083 483.128 484.482 485.043 487.920 | 82176 |
| alloc-test1 | old | 2.646 2.653 2.661 2.665 2.666 | 13256 |
| alloc-test1 | new | 2.924 2.932 2.944 2.950 2.953 | 13256 |
| alloc-test1 | new-off | 2.661 2.667 2.676 2.690 2.691 | 13256 |
| alloc-test1 | jemalloc | 2.503 2.505 2.509 2.519 2.521 | 12616 |
| mstress | old | 0.375 0.376 0.377 0.378 0.379 | 329140 |
| mstress | new | 0.388 0.389 0.389 0.390 0.392 | 328176 |
| mstress | new-off | 0.375 0.376 0.376 0.377 0.378 | 332660 |
| mstress | jemalloc | 0.645 0.646 0.646 0.656 0.656 | 298384 |
| valkey1 | old | 7.099 7.135 7.170 7.259 7.277 | 598660 |
| valkey1 | new | 6.479 6.511 6.515 6.518 6.603 | 598660 |
| valkey1 | new-off | 7.100 7.108 7.121 7.173 7.212 | 598608 |
| valkey1 | jemalloc | 6.990 7.090 7.262 7.267 7.275 | 599492 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | new-off | new-off RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | new-off vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.6 | 15.9 | 37.1 | 16.0 | 41.7 | 15.9 | 45.2 | 18.6 | +14.8% | +2.2% | -5.8% |
| larson10 | M ops/s | 366.8 | 112.2 | 310.4 | 118.4 | 362.4 | 112.4 | 385.9 | 132.3 | +18.2% | +1.2% | -4.9% |
| xmalloc | M frees/s | 586.686 | 53.0 | 469.201 | 73.8 | 578.143 | 53.3 | 484.482 | 80.2 | +25.0% | +1.5% | +21.1% |
| alloc-test1 | s | 2.661 | 12.9 | 2.944 | 12.9 | 2.676 | 12.9 | 2.509 | 12.3 | +10.6% | +0.6% | -5.7% |
| mstress | s | 0.377 | 321.4 | 0.389 | 320.5 | 0.376 | 324.9 | 0.646 | 291.4 | +3.2% | -0.3% | +71.4% |
| valkey1 | M ops/s | 7.170 | 584.6 | 6.515 | 584.6 | 7.121 | 584.6 | 7.262 | 585.4 | +10.1% | +0.7% | -1.3% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
