# challoc Phase 2.5 task 8 fast path: mode vs default vs old, 2026-09-20, x86_64

- commit: ee0ecd8 (uncommitted changes in src/)
- date: 2026-09-20T00:23:45Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 6.94 4.15 2.17; at end: 7.41 6.58 4.79; 828 s total
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
| larson1 | old | 44.0 44.1 44.5 44.7 45.0 | 17044 |
| larson1 | new | 40.1 42.1 42.9 42.9 43.2 | 17752 |
| larson1 | new-off | 44.5 44.5 44.9 45.0 45.1 | 17048 |
| larson1 | jemalloc | 43.2 43.7 44.9 45.0 45.3 | 18152 |
| larson10 | old | 274.6 274.9 275.3 286.6 288.8 | 96952 |
| larson10 | new | 230.2 235.2 236.1 239.6 244.3 | 96816 |
| larson10 | new-off | 275.6 277.1 277.8 278.9 278.9 | 97584 |
| larson10 | jemalloc | 284.3 285.3 290.5 291.4 293.9 | 94328 |
| xmalloc | old | 317.153 317.299 329.693 333.833 335.332 | 54332 |
| xmalloc | new | 280.506 283.660 284.095 284.144 287.692 | 59608 |
| xmalloc | new-off | 324.245 324.653 330.236 331.202 331.382 | 54068 |
| xmalloc | jemalloc | 327.968 328.299 329.211 330.391 332.051 | 95736 |
| valkey1 | old | 14.361 14.803 15.030 15.196 15.211 | 599676 |
| valkey1 | new | 10.862 10.890 10.908 10.976 10.993 | 599476 |
| valkey1 | new-off | 14.411 14.966 14.980 15.117 15.330 | 599744 |
| valkey1 | jemalloc | 14.134 14.953 14.965 14.966 15.112 | 606544 |
| alloc-testN | old | 4.524 4.541 4.544 4.589 4.631 | 17540 |
| alloc-testN | new | 4.714 4.714 4.722 4.749 4.792 | 17480 |
| alloc-testN | new-off | 4.534 4.566 4.607 4.641 4.642 | 17544 |
| alloc-testN | jemalloc | 4.160 4.168 4.169 4.178 4.199 | 17064 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | new-off | new-off RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | new-off vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 44.5 | 16.6 | 42.9 | 17.3 | 44.9 | 16.6 | 44.9 | 17.7 | +3.7% | -0.9% | -0.9% |
| larson10 | M ops/s | 275.3 | 94.7 | 236.1 | 94.5 | 277.8 | 95.3 | 290.5 | 92.1 | +16.6% | -0.9% | -5.2% |
| xmalloc | M frees/s | 329.693 | 53.1 | 284.095 | 58.2 | 330.236 | 52.8 | 329.211 | 93.5 | +16.1% | -0.2% | +0.1% |
| valkey1 | M ops/s | 15.030 | 585.6 | 10.908 | 585.4 | 14.980 | 585.7 | 14.965 | 592.3 | +37.8% | +0.3% | +0.4% |
| alloc-testN | s | 4.544 | 17.1 | 4.722 | 17.1 | 4.607 | 17.1 | 4.169 | 16.7 | +3.9% | +1.4% | -8.3% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
