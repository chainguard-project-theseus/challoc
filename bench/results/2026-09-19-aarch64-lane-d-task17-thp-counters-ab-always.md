# challoc THP counters (task 17, sysfs always state), 2026-09-19, aarch64

- commit: 79e3931
- date: 2026-09-19T23:44:47Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (GCE aarch64 lane VM), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 1.01 0.91 0.92; at end: 0.90 0.90 0.91; 113 s total
- method: 2 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than never.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - never: 79e3931547d1 built into target/ab/79e3931547d1
  - always: 79e3931547d1 built into target/ab/79e3931547d1, MALLOC_CONF=thp:always

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | never | 41.6 41.6 | 16264 |
| larson1 | always | 41.0 41.0 | 59916 |
| valkey1 | never | 6.220 6.666 | 598480 |
| valkey1 | always | 7.164 7.522 | 656080 |

## Medians

| program | unit | never | never RSS MiB | always | always RSS MiB | always vs never |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.6 | 15.9 | 41.0 | 58.5 | +1.5% |
| valkey1 | M ops/s | 6.220 | 584.5 | 7.164 | 640.7 | -13.2% |

Positive percentages mean the variant did worse than never (slower or lower throughput).

## THP counters (CHALLOC_THP_COUNTERS=1)

Per-run deltas of /proc/vmstat thp_* and hugepages-<size>kB/stats/* around each run (bench/lib.sh thp_snapshot), mean over the 2 timed runs; only the counters that moved. Host-global counters: on a shared host the deltas include other processes' faults. sysfs at the end: enabled=madvise defrag=madvise per-size: 2048kB=inherit 64kB=madvise (per-size stats/ present).

| program | variant | mean deltas per run |
|---|---|---|
| larson1 | never |  |
| larson1 | always | 2048kB/anon_fault_alloc=25.0 2048kB/split_deferred=5.0 64kB/anon_fault_alloc=24.0 thp_collapse_alloc=1.0 thp_deferred_split_page=5.0 thp_fault_alloc=25.0 thp_split_pmd=5.0 |
| valkey1 | never | 64kB/anon_fault_alloc=1.0 |
| valkey1 | always | 2048kB/anon_fault_alloc=318.0 2048kB/split_deferred=281.0 64kB/anon_fault_alloc=28.0 thp_collapse_alloc=1.0 thp_deferred_split_page=281.0 thp_fault_alloc=318.0 thp_split_pmd=281.0 |
