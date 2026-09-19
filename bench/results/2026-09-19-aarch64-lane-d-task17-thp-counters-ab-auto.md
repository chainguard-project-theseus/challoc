# challoc THP counters (task 17, sysfs auto state), 2026-09-19, aarch64

- commit: 79e3931
- date: 2026-09-19T23:41:05Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (GCE aarch64 lane VM), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 0.84 0.87 0.89; at end: 1.01 0.91 0.92; 221 s total
- method: 2 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than never.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - never: 79e3931547d1 built into target/ab/79e3931547d1
  - auto: 79e3931547d1 built into target/ab/79e3931547d1, MALLOC_CONF=thp:auto
  - always: 79e3931547d1 built into target/ab/79e3931547d1, MALLOC_CONF=thp:always
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | never | 41.6 41.7 | 16268 |
| larson1 | auto | 41.1 41.1 | 18484 |
| larson1 | always | 41.1 41.3 | 18480 |
| larson1 | jemalloc | 44.7 45.0 | 17680 |
| valkey1 | never | 6.560 6.774 | 598480 |
| valkey1 | auto | 6.298 6.382 | 599896 |
| valkey1 | always | 6.759 6.785 | 600064 |
| valkey1 | jemalloc | 6.226 6.523 | 599636 |

## Medians

| program | unit | never | never RSS MiB | auto | auto RSS MiB | always | always RSS MiB | jemalloc | jemalloc RSS MiB | auto vs never | always vs never | jemalloc vs never |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.6 | 15.9 | 41.1 | 18.1 | 41.1 | 18.0 | 44.7 | 17.3 | +1.2% | +1.2% | -6.9% |
| valkey1 | M ops/s | 6.560 | 584.5 | 6.298 | 585.8 | 6.759 | 586.0 | 6.226 | 585.6 | +4.2% | -2.9% | +5.4% |

Positive percentages mean the variant did worse than never (slower or lower throughput).

## THP counters (CHALLOC_THP_COUNTERS=1)

Per-run deltas of /proc/vmstat thp_* and hugepages-<size>kB/stats/* around each run (bench/lib.sh thp_snapshot), mean over the 2 timed runs; only the counters that moved. Host-global counters: on a shared host the deltas include other processes' faults. sysfs at the end: enabled=madvise defrag=madvise per-size: 64kB=madvise (per-size stats/ present).

| program | variant | mean deltas per run |
|---|---|---|
| larson1 | never |  |
| larson1 | auto | 64kB/anon_fault_alloc=76.0 thp_collapse_alloc=1.0 |
| larson1 | always | 64kB/anon_fault_alloc=76.0 thp_collapse_alloc=1.0 |
| larson1 | jemalloc |  |
| valkey1 | never |  |
| valkey1 | auto | 64kB/anon_fault_alloc=2788.0 thp_collapse_alloc=1.0 |
| valkey1 | always | 64kB/anon_fault_alloc=3308.0 thp_collapse_alloc=1.0 |
| valkey1 | jemalloc |  |
