# A/B thp:auto vs never, aarch64, 2026-09-19T19:02:53Z (Phase 2.5 task 9, lane B)

- `bench/gce/challoc-bench ab arm64 VARIANTS='never=HEAD auto=HEAD:thp:auto' ROUNDS=5 BENCHES='larson1 larson10 valkey1 redis mstress'`
  on challoc-lane-b-arm64 (c4a-standard-16 (Axion / Neoverse V2), us-east4-a, kernel 7.0.0-1011-gcp, 4 KiB pages, the dev container),
  HEAD cc4aa34; both variants the same library, only `MALLOC_CONF` differs.
- sysfs state on the VM during the run: hugepages-64kB/enabled=madvise, hugepages-2048kB/enabled=never (`mthp=64K`); set by root before the run, restored to Ubuntu's default after it.
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted;
  RSS is bench/rt's median maxrss (KiB) over the timed runs. Load at start 0.33 0.92 0.70, nproc 16; round 0 done, load 1.61 1.19 0.81, 19:03:53; round 1 done, load 2.08 1.40 0.90, 19:04:47; round 2 done, load 2.34 1.58 0.99, 19:05:40; round 3 done, load 2.36 1.72 1.08, 19:06:34; round 4 done, load 2.76 1.95 1.19, 19:07:27; round 5 done, load 3.31 2.20 1.32, 19:08:21.
- Note: `bench/ab.sh` wrote this file on the VM under `bench/results/` and the following `challoc-bench quick`
  of the same chain synced the tree with `--delete` before it was fetched, so the tables below are copied
  from the run's printed output (identical bytes to the file's tables, `.tmp-logs` of the lane's worktree);
  the raw logs under `target/ab/out` on the VM were overwritten by the next run.

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | never | 42.4 42.4 42.5 42.5 42.5 | 16320 |
| larson1 | auto | 41.7 41.7 41.8 41.9 41.9 | 18560 |
| larson10 | never | 366.9 367.6 369.1 370.5 370.9 | 115392 |
| larson10 | auto | 360.1 360.9 363.3 363.3 364.0 | 123864 |
| valkey1 | never | 7.361 7.363 7.376 7.423 7.452 | 598660 |
| valkey1 | auto | 7.147 7.327 7.383 7.391 7.420 | 600124 |
| redis | never | 501.0 501.3 503.0 503.5 505.3 | 29760 |
| redis | auto | 503.8 504.5 504.5 506.3 507.9 | 35416 |
| mstress | never | 0.370 0.372 0.374 0.375 0.376 | 329060 |
| mstress | auto | 0.358 0.360 0.361 0.362 0.364 | 332192 |

## Medians

| program | unit | never | never RSS MiB | auto | auto RSS MiB | auto vs never |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.5 | 15.9 | 41.8 | 18.1 | +1.7% |
| larson10 | M ops/s | 369.1 | 112.7 | 363.3 | 121.0 | +1.6% |
| valkey1 | M ops/s | 7.376 | 584.6 | 7.383 | 586.1 | -0.1% |
| redis | k req/s | 503.0 | 29.1 | 504.5 | 34.6 | -0.3% |
| mstress | s | 0.374 | 321.3 | 0.361 | 324.4 | -3.5% |

Positive percentages mean the variant did worse than never (slower or lower throughput).
