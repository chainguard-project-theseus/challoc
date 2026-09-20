# challoc A/B, 2026-09-20, aarch64

- commit: 73d2003
- date: 2026-09-20T23:07:23Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 0.88 0.36 0.13; at end: 3.60 2.66 1.30; 409 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: cff7494c7072 built into target/ab/cff7494c7072
  - new: working tree (HEAD 73d2003) built into target/ab/tree

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| valkey1 | old | 6.270 6.356 6.447 6.489 6.528 | 598548 |
| valkey1 | new | 6.383 6.426 6.451 6.465 6.498 | 598612 |
| valkey1-free | old | 6.215 6.290 6.362 6.363 6.365 | 598612 |
| valkey1-free | new | 6.106 6.275 6.340 6.379 6.432 | 598612 |
| xmalloc | old | 572.972 574.234 574.509 575.134 576.605 | 57428 |
| xmalloc | new | 570.606 573.287 574.785 575.415 575.576 | 56068 |
| larson1 | old | 41.5 41.6 41.7 41.8 41.8 | 16256 |
| larson1 | new | 41.6 41.7 41.7 41.8 41.8 | 16252 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | new vs old |
|---|---|---:|---:|---:|---:|---:|
| valkey1 | M ops/s | 6.447 | 584.5 | 6.451 | 584.6 | -0.1% |
| valkey1-free | M ops/s | 6.362 | 584.6 | 6.340 | 584.6 | +0.3% |
| xmalloc | M frees/s | 574.509 | 56.1 | 574.785 | 54.8 | -0.0% |
| larson1 | M ops/s | 41.7 | 15.9 | 41.7 | 15.9 | +0.0% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

## Attribution (`challoc-bench quick-perf arm64 OLD=main`, one run per variant)

The change under test: `release_owned` compares the slab descriptor's
arena with the owner word once per slab switch of a flush (review
2026-09-18 item 16, the remaining half): +4 instructions on the executed
slab-switch path (`madd; ldrh; ldrh; cmp; b.ne` replacing `mul`), +8 of
cold abort setup; `release_owned` 208 -> 220 instructions (q16). A slab
switch happens once per slab in a batched release, so the cost is spread
over the cache's worth of frees.

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
| larson1 | old | 41.36 | 72.4 | 264.5 | 2.143 | 3.988 |
| larson1 | new | 41.17 | 72.7 | 264.5 | 2.109 | 3.990 |
| larson1 | jemalloc | 44.57 | 66.8 | 224.8 | 2.087 | 1.730 |
| valkey1 | old | 11.37 | 266.6 | 254.5 | 3.119 | 2.831 |
| valkey1 | new | 11.42 | 265.4 | 253.1 | 3.107 | 2.837 |
| valkey1 | jemalloc | 12.25 | 246.8 | 172.5 | 2.995 | 2.720 |

Reading: the per-operation cost of the compare is below the attribution's
own noise (valkey1 instr/op moved -1.4 between two runs of code that
differs by +4 per slab switch); the five-round A/B above has valkey1 at
-0.1% with overlapping sorted lists, valkey1-free +0.3%, xmalloc and
larson1 flat. The one-round quick tier's arm64 valkey1 +2.9% was that
row's spread. Landed.
