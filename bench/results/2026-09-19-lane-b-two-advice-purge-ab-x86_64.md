# challoc Phase 2.5 task 10: two-advice purge, lazy band on vs off, x86_64, docker --memory=8g, 2026-09-19, x86_64

- commit: 089c465
- date: 2026-09-19T18:07:59Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (gce), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 0.05 0.65 0.62; at end: 10.59 5.88 2.74; 246 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than base.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - base: a3ddd6d094f7 built into target/ab/a3ddd6d094f7
  - eager: 089c4652cdf4 built into target/ab/089c4652cdf4, MALLOC_CONF=lazy_band:false
  - lazy: 089c4652cdf4 built into target/ab/089c4652cdf4

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| mstress | base | 0.579 0.590 0.598 0.601 0.606 | 337692 |
| mstress | eager | 0.587 0.594 0.596 0.601 0.610 | 347256 |
| mstress | lazy | 0.584 0.590 0.596 0.597 0.603 | 343872 |
| alloc-testN | base | 4.386 4.417 4.421 4.461 4.531 | 17520 |
| alloc-testN | eager | 4.384 4.399 4.403 4.421 4.422 | 17724 |
| alloc-testN | lazy | 4.342 4.364 4.368 4.386 4.407 | 17584 |
| larson10 | base | 280.8 282.1 285.6 288.7 290.1 | 98756 |
| larson10 | eager | 282.5 285.6 293.4 295.1 297.7 | 99636 |
| larson10 | lazy | 282.5 284.9 289.4 294.9 297.4 | 98752 |

## Medians

| program | unit | base | base RSS MiB | eager | eager RSS MiB | lazy | lazy RSS MiB | eager vs base | lazy vs base |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| mstress | s | 0.598 | 329.8 | 0.596 | 339.1 | 0.596 | 335.8 | -0.3% | -0.3% |
| alloc-testN | s | 4.421 | 17.1 | 4.403 | 17.3 | 4.368 | 17.2 | -0.4% | -1.2% |
| larson10 | M ops/s | 285.6 | 96.4 | 293.4 | 97.3 | 289.4 | 96.4 | -2.7% | -1.3% |

Positive percentages mean the variant did worse than base (slower or lower throughput).
