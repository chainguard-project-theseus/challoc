# A/B thp:always vs never, x86_64, 2026-09-19T19:01:52Z (Phase 2.5 task 9, lane B)

- `bench/gce/challoc-bench ab amd64 VARIANTS='never=HEAD always=HEAD:thp:always' ROUNDS=5 BENCHES='larson1 larson10 valkey1 redis mstress'`
  on challoc-lane-b-amd64 (c4-standard-16 (Xeon Platinum 8581C), us-east4-a, kernel 7.0.0-1011-gcp, 4 KiB pages, the dev container),
  HEAD cc4aa34; both variants the same library, only `MALLOC_CONF` differs.
- sysfs state on the VM during the run: hugepages-64kB/enabled=madvise, hugepages-2048kB/enabled=inherit under enabled=madvise (`mthp=64K,2M`); set by root before the run, restored to Ubuntu's default after it.
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted;
  RSS is bench/rt's median maxrss (KiB) over the timed runs. Load at start 1.01 1.23 0.90, nproc 16; round 0 done, load 1.84 1.43 1.00, 19:02:52; round 1 done, load 2.28 1.64 1.09, 19:03:45; round 2 done, load 2.43 1.79 1.17, 19:04:38; round 3 done, load 2.44 1.91 1.25, 19:05:31; round 4 done, load 2.50 2.02 1.32, 19:06:24; round 5 done, load 3.51 2.32 1.46, 19:07:17.
- Note: `bench/ab.sh` wrote this file on the VM under `bench/results/` and the following `challoc-bench quick`
  of the same chain synced the tree with `--delete` before it was fetched, so the tables below are copied
  from the run's printed output (identical bytes to the file's tables, `.tmp-logs` of the lane's worktree);
  the raw logs under `target/ab/out` on the VM were overwritten by the next run.

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | never | 44.1 44.7 45.3 45.6 45.9 | 17428 |
| larson1 | always | 44.6 45.5 45.7 45.9 46.1 | 61688 |
| larson10 | never | 267.9 282.6 285.4 291.2 293.6 | 99308 |
| larson10 | always | 284.1 287.8 295.2 295.8 298.0 | 156856 |
| valkey1 | never | 15.051 15.119 15.214 15.256 15.312 | 599708 |
| valkey1 | always | 17.693 17.891 17.950 18.015 18.286 | 657008 |
| redis | never | 651.5 655.3 656.6 664.0 681.2 | 30872 |
| redis | always | 660.1 660.1 660.5 667.1 668.9 | 108088 |
| mstress | never | 0.619 0.620 0.621 0.625 0.626 | 351360 |
| mstress | always | 0.596 0.605 0.606 0.608 0.616 | 553592 |

## Medians

| program | unit | never | never RSS MiB | always | always RSS MiB | always vs never |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.3 | 17.0 | 45.7 | 60.2 | -0.9% |
| larson10 | M ops/s | 285.4 | 97.0 | 295.2 | 153.2 | -3.3% |
| valkey1 | M ops/s | 15.214 | 585.7 | 17.950 | 641.6 | -15.2% |
| redis | k req/s | 656.6 | 30.1 | 660.5 | 105.6 | -0.6% |
| mstress | s | 0.621 | 343.1 | 0.606 | 540.6 | -2.4% |

Positive percentages mean the variant did worse than never (slower or lower throughput).
