# challoc A/B, 2026-09-19, aarch64

- commit: 299461b (uncommitted changes in src/)
- date: 2026-09-19T23:29:43Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 2.65 2.06 0.96; at end: 10.80 7.69 3.72; 311 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: 299461bc2235 built into target/ab/299461bc2235
  - new: working tree (HEAD 299461b + uncommitted src changes) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson10 | old | 363.0 363.2 365.7 369.7 370.0 | 115080 |
| larson10 | new | 360.8 362.4 363.1 366.6 367.1 | 114864 |
| larson10 | jemalloc | 382.5 383.9 385.5 386.2 390.4 | 134832 |
| larson16 | old | 582.5 583.1 589.5 589.8 592.1 | 168460 |
| larson16 | new | 588.6 590.2 591.0 592.8 599.2 | 169136 |
| larson16 | jemalloc | 612.9 615.6 615.9 617.4 618.3 | 211500 |
| alloc-testN | old | 2.288 2.301 2.302 2.302 2.314 | 16728 |
| alloc-testN | new | 2.287 2.297 2.303 2.305 2.308 | 16888 |
| alloc-testN | jemalloc | 2.200 2.201 2.202 2.204 2.207 | 16860 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson10 | M ops/s | 365.7 | 112.4 | 363.1 | 112.2 | 385.5 | 131.7 | +0.7% | -5.1% |
| larson16 | M ops/s | 589.5 | 164.5 | 591.0 | 165.2 | 615.9 | 206.5 | -0.3% | -4.3% |
| alloc-testN | s | 2.302 | 16.3 | 2.303 | 16.5 | 2.202 | 16.5 | +0.0% | -4.3% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
