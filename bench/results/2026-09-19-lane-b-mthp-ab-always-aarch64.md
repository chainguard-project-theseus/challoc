# A/B thp:always vs never, aarch64, 2026-09-19T19:08:26Z (Phase 2.5 task 9, lane B)

- `bench/gce/challoc-bench ab arm64 VARIANTS='never=HEAD always=HEAD:thp:always' ROUNDS=5 BENCHES='larson1 larson10 valkey1 redis mstress'`
  on challoc-lane-b-arm64 (c4a-standard-16 (Axion / Neoverse V2), us-east4-a, kernel 7.0.0-1011-gcp, 4 KiB pages, the dev container),
  HEAD cc4aa34; both variants the same library, only `MALLOC_CONF` differs.
- sysfs state on the VM during the run: hugepages-64kB/enabled=madvise, hugepages-2048kB/enabled=inherit under enabled=madvise (`mthp=64K,2M`); set by root before the run, restored to Ubuntu's default after it.
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted;
  RSS is bench/rt's median maxrss (KiB) over the timed runs. Load at start 3.04 2.16 1.31, nproc 16; round 0 done, load 3.57 2.40 1.44, 19:09:26; round 1 done, load 2.98 2.44 1.51, 19:10:19; round 2 done, load 3.58 2.72 1.65, 19:11:12; round 3 done, load 3.15 2.73 1.72, 19:12:06; round 4 done, load 2.92 2.77 1.79, 19:12:59; round 5 done, load 2.84 2.77 1.84, 19:13:52.
- Note: `bench/ab.sh` wrote this file on the VM under `bench/results/` and the following `challoc-bench quick`
  of the same chain synced the tree with `--delete` before it was fetched, so the tables below are copied
  from the run's printed output (identical bytes to the file's tables, `.tmp-logs` of the lane's worktree);
  the raw logs under `target/ab/out` on the VM were overwritten by the next run.

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | never | 42.3 42.3 42.4 42.5 42.5 | 16196 |
| larson1 | always | 41.6 41.7 41.8 41.8 41.9 | 60060 |
| larson10 | never | 364.8 368.0 369.0 369.2 370.8 | 115268 |
| larson10 | always | 356.3 362.3 362.8 365.2 366.5 | 171832 |
| valkey1 | never | 7.298 7.300 7.310 7.409 7.419 | 598664 |
| valkey1 | always | 8.040 8.123 8.128 8.140 8.206 | 656256 |
| redis | never | 498.8 500.5 502.0 503.5 504.3 | 29760 |
| redis | always | 507.1 507.4 507.6 507.9 511.5 | 106232 |
| mstress | never | 0.369 0.371 0.372 0.374 0.375 | 330604 |
| mstress | always | 0.341 0.342 0.344 0.344 0.347 | 536448 |

## Medians

| program | unit | never | never RSS MiB | always | always RSS MiB | always vs never |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.4 | 15.8 | 41.8 | 58.7 | +1.4% |
| larson10 | M ops/s | 369.0 | 112.6 | 362.8 | 167.8 | +1.7% |
| valkey1 | M ops/s | 7.310 | 584.6 | 8.128 | 640.9 | -10.1% |
| redis | k req/s | 502.0 | 29.1 | 507.6 | 103.7 | -1.1% |
| mstress | s | 0.372 | 322.9 | 0.344 | 523.9 | -7.5% |

Positive percentages mean the variant did worse than never (slower or lower throughput).
