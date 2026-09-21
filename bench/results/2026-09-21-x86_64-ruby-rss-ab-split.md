# challoc A/B, 2026-09-21, x86_64

- commit: 6e25c7a
- date: 2026-09-21T06:25:55Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 0.25 1.15 1.99; at end: 12.31 10.50 6.66; 631 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: df6d28545d8b built into target/ab/df6d28545d8b
  - f3: 4dc352c96b48 built into target/ab/4dc352c96b48
  - purge: 1a452789b67a built into target/ab/1a452789b67a
  - new: 6e25c7aad4f1 built into target/ab/6e25c7aad4f1
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson10 | old | 280.9 284.6 286.8 288.8 289.8 | 98628 |
| larson10 | f3 | 258.6 259.2 259.9 266.4 267.1 | 83328 |
| larson10 | purge | 253.8 258.9 259.0 260.1 264.9 | 79852 |
| larson10 | new | 260.2 260.3 262.1 266.3 273.9 | 83684 |
| larson10 | jemalloc | 285.8 285.8 293.8 299.6 300.3 | 95156 |
| larson16 | old | 303.6 305.5 305.7 307.2 310.4 | 120368 |
| larson16 | f3 | 284.1 289.1 289.5 291.0 295.5 | 121464 |
| larson16 | purge | 285.0 286.5 287.2 290.5 291.8 | 118544 |
| larson16 | new | 286.4 286.5 288.8 289.1 290.9 | 121364 |
| larson16 | jemalloc | 332.8 334.7 335.6 336.5 355.8 | 130540 |
| mstress | old | 0.627 0.627 0.627 0.633 0.638 | 349652 |
| mstress | f3 | 0.626 0.627 0.629 0.634 0.637 | 337460 |
| mstress | purge | 0.693 0.696 0.697 0.701 0.703 | 297556 |
| mstress | new | 0.624 0.630 0.636 0.636 0.638 | 330412 |
| mstress | jemalloc | 1.071 1.071 1.075 1.080 1.092 | 290720 |
| xmalloc | old | 324.405 325.433 326.363 333.063 342.444 | 54252 |
| xmalloc | f3 | 502.305 503.260 505.989 510.321 513.424 | 58440 |
| xmalloc | purge | 504.768 508.439 510.459 511.328 514.046 | 59944 |
| xmalloc | new | 497.801 502.938 512.654 514.389 515.050 | 62728 |
| xmalloc | jemalloc | 328.485 328.793 330.937 331.492 332.748 | 101916 |

## Medians

| program | unit | old | old RSS MiB | f3 | f3 RSS MiB | purge | purge RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | f3 vs old | purge vs old | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson10 | M ops/s | 286.8 | 96.3 | 259.9 | 81.4 | 259.0 | 78.0 | 262.1 | 81.7 | 293.8 | 92.9 | +10.4% | +10.7% | +9.4% | -2.4% |
| larson16 | M ops/s | 305.7 | 117.5 | 289.5 | 118.6 | 287.2 | 115.8 | 288.8 | 118.5 | 335.6 | 127.5 | +5.6% | +6.4% | +5.9% | -8.9% |
| mstress | s | 0.627 | 341.5 | 0.629 | 329.6 | 0.697 | 290.6 | 0.636 | 322.7 | 1.075 | 283.9 | +0.3% | +11.2% | +1.4% | +71.5% |
| xmalloc | M frees/s | 326.363 | 53.0 | 505.989 | 57.1 | 510.459 | 58.5 | 512.654 | 61.3 | 330.937 | 99.5 | -35.5% | -36.1% | -36.3% | -1.4% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
