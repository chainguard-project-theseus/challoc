# Phase 1.5 task 4f: dense per-class slot bitmap, x86_64 (GCE C4)

Commit 5304cd4 (code) against 7efdae8 (task 4e landed; `old` in every
table). VM `challoc-bench-amd64`, c4-standard-16 (Xeon Platinum 8581C), zone
`us-east4-a`, Ubuntu 24.04 / 7.0.0-1011-gcp, 4 KiB pages; jemalloc rows are
Wolfi's 5.3 preloaded, same run. Companion files: the harness output of the
A/B (`-ab.md`), the soak summary and CSVs (`-soak-valkey-soak*.{md,csv}`).
aarch64 (the attribution, the soak and the reading of the mechanism) in `2026-09-18-aarch64-gce-dense-bitmap.md`.

## 1. `quick` (go/no-go, one round, working tree against 7efdae8)

```
| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 49.9 (16.0 MiB) | 49.6 (16.3 MiB) | 47.5 (18.3 MiB) | +0.6% | -4.2% |
| larson10 | M ops/s | 292.0 (98.8 MiB) | 294.4 (99.2 MiB) | 310.9 (95.2 MiB) | -0.8% | +5.6% |
| xmalloc | M frees/s | 354.958 (80.8 MiB) | 359.140 (81.7 MiB) | 340.386 (88.2 MiB) | -1.2% | -5.2% |
| alloc-test1 | s | 2.857 (13.7 MiB) | 2.872 (13.8 MiB) | 2.731 (13.0 MiB) | +0.5% | +5.2% |
| valkey1 | M ops/s | 16.334 (587.6 MiB) | 16.617 (585.4 MiB) | 16.144 (592.0 MiB) | -1.7% | -2.8% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6673 (49.8%) | new compacts (end occupancy >= 90%) | 24477393 ops/s |
quick: go (new within 5% of old on every row; churn compacts)
```

`make test` on the VM: 226 + 79 pass (the negative modes included).

## 2. `quick-perf`: per-operation counters (one run each, `sudo perf stat` from the host)

```
| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 200.00 | 16.6 | 54.0 | 0.462 | 0.000 |
| larson1 | new | 200.00 | 16.6 | 53.7 | 0.370 | 0.000 |
| larson1 | jemalloc | 200.00 | 16.5 | 45.5 | 0.487 | 0.000 |
| larson10 | old | 200.01 | 165.4 | 339.4 | 2.961 | 0.025 |
| larson10 | new | 200.03 | 165.6 | 340.2 | 2.275 | 0.023 |
| larson10 | jemalloc | 199.98 | 164.7 | 300.5 | 3.278 | 0.008 |
| valkey1 | old | 26.80 | 143.0 | 223.1 | 3.290 | 0.908 |
| valkey1 | new | 27.19 | 146.4 | 223.0 | 3.274 | 0.895 |
| valkey1 | jemalloc | 30.32 | 131.3 | 145.4 | 3.326 | 0.951 |
| valkey1-free | old | 25.44 | 154.0 | 204.9 | 4.645 | 1.084 |
| valkey1-free | new | 25.05 | 149.2 | 209.1 | 4.549 | 0.932 |
| valkey1-free | jemalloc | 24.74 | 138.0 | 149.4 | 4.678 | 1.181 |
```

`dTLB-load-misses` counts page walks on this core (the 96-entry L1 dTLB
and cheap STLB hits keep them near zero for larson, review 3.3).

Reading. 10-thread L1D misses per operation 2.961 -> 2.275 (-23%, below
jemalloc's 3.278) and one-thread 0.462 -> 0.370, cycles per operation
unchanged (16.6 -> 16.6, 165.4 -> 165.6): the grouping gauge passes here
too. `valkey1-free` 154.0 -> 149.2 cycles per operation (-3%; jemalloc
138.0) with page walks 1.08 -> 0.93 per operation; `valkey1` within one
run's noise (143.0 -> 146.4, the M ops/s column moved the other way).
The x86_64 descriptor miss was cheaper to begin with (task 4e's
`valkey1` row was already 0.7% ahead of jemalloc), so the gain is smaller
than on aarch64.

## 3. A/B protocol rows, x86_64 (5 rounds plus warm-up, interleaved, same run; harness output in `-ab.md`)

Same invocation as aarch64, 05:57 UTC; load average at start: 0.61 0.87 0.62; at end: 5.61 5.71 4.12; 985 s total (the A/B itself is the load).

### Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.5 46.5 46.9 47.0 48.9 | 16104 |
| larson1 | new | 41.9 44.4 44.8 46.1 47.3 | 16300 |
| larson1 | jemalloc | 42.6 45.2 45.3 45.4 45.4 | 18148 |
| larson10 | old | 275.5 275.8 281.3 294.2 299.4 | 102756 |
| larson10 | new | 278.5 281.9 282.5 290.4 291.1 | 100444 |
| larson10 | jemalloc | 286.2 289.8 295.7 299.6 301.3 | 94124 |
| larson16 | old | 300.1 309.3 309.5 316.0 318.3 | 128656 |
| larson16 | new | 300.5 302.7 306.8 310.0 315.3 | 127592 |
| larson16 | jemalloc | 333.0 334.4 334.7 336.8 338.2 | 131212 |
| xmalloc | old | 335.416 343.249 345.986 353.889 361.442 | 83200 |
| xmalloc | new | 331.782 331.798 341.172 343.766 347.477 | 83024 |
| xmalloc | jemalloc | 320.819 328.530 329.231 330.536 330.871 | 98088 |
| alloc-test1 | old | 2.858 2.976 3.249 3.293 3.340 | 14004 |
| alloc-test1 | new | 3.294 3.314 3.320 3.333 3.388 | 14192 |
| alloc-test1 | jemalloc | 3.131 3.138 3.178 3.178 3.192 | 13328 |
| alloc-testN | old | 4.237 4.346 4.417 4.422 4.559 | 21520 |
| alloc-testN | new | 4.426 4.454 4.471 4.516 4.548 | 21944 |
| alloc-testN | jemalloc | 4.090 4.114 4.119 4.138 4.387 | 17080 |
| valkey1 | old | 15.297 15.313 15.317 15.478 16.283 | 602184 |
| valkey1 | new | 15.024 15.302 15.318 15.408 15.750 | 599824 |
| valkey1 | jemalloc | 14.240 14.515 14.993 15.132 15.345 | 606552 |
| valkey1-free | old | 14.025 14.044 14.443 14.516 14.951 | 602060 |
| valkey1-free | new | 14.874 15.058 15.340 15.461 15.521 | 599644 |
| valkey1-free | jemalloc | 12.048 12.996 13.082 13.152 13.195 | 606564 |

### Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 46.9 | 15.7 | 44.8 | 15.9 | 45.3 | 17.7 | +4.7% | +3.5% |
| larson10 | M ops/s | 281.3 | 100.3 | 282.5 | 98.1 | 295.7 | 91.9 | -0.4% | -4.9% |
| larson16 | M ops/s | 309.5 | 125.6 | 306.8 | 124.6 | 334.7 | 128.1 | +0.9% | -7.5% |
| xmalloc | M frees/s | 345.986 | 81.2 | 341.172 | 81.1 | 329.231 | 95.8 | +1.4% | +5.1% |
| alloc-test1 | s | 3.249 | 13.7 | 3.320 | 13.9 | 3.178 | 13.0 | +2.2% | -2.2% |
| alloc-testN | s | 4.417 | 21.0 | 4.471 | 21.4 | 4.119 | 16.7 | +1.2% | -6.7% |
| valkey1 | M ops/s | 15.317 | 588.1 | 15.318 | 585.8 | 14.993 | 592.3 | -0.0% | +2.2% |
| valkey1-free | M ops/s | 14.443 | 587.9 | 15.340 | 585.6 | 13.082 | 592.3 | -5.8% | +10.4% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

Reading. larson10 -0.4%, larson16 +0.9%, xmalloc +1.4%, alloc-testN +1.2%,
`valkey1` +0.0% (15.317 -> 15.318), `valkey1-free` 14.443 -> 15.340 M
ops/s (-5.8%, i.e. 6.2% faster; jemalloc 13.082, now 17% behind on this
row); RSS unchanged. larson1 (+4.7% on medians) and alloc-test1 (+2.2%)
have the bimodal, overlapping lists this VM has shown before (task 5's
file: "10 rounds, bimodal lists overlapping"; old's alloc-test1 list runs
2.858..3.340), so both were re-run with 10 rounds:

### Re-run of larson1 and alloc-test1, 10 rounds (`-ab2.md`)

load average at start: 2.51 4.85 3.90; at end: 0.97 2.12 2.94; 345 s total.

### Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 20.8 41.9 45.3 45.4 45.6 45.7 46.5 47.7 48.0 48.8 | 16040 |
| larson1 | new | 43.5 44.7 44.9 45.0 45.0 45.0 45.5 46.8 47.2 48.0 | 16360 |
| larson1 | jemalloc | 40.7 41.7 42.7 43.6 43.7 44.0 44.4 45.0 45.1 47.8 | 18060 |
| alloc-test1 | old | 2.913 2.940 3.063 3.271 3.352 3.355 3.360 3.367 3.375 3.376 | 13964 |
| alloc-test1 | new | 2.976 2.983 3.009 3.367 3.382 3.405 3.406 3.414 3.418 3.430 | 14104 |
| alloc-test1 | jemalloc | 2.803 2.808 2.918 2.945 3.200 3.233 3.234 3.235 3.238 3.240 | 13328 |

### Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.6 | 15.7 | 45.0 | 16.0 | 43.7 | 17.6 | +1.3% | +4.3% |
| alloc-test1 | s | 3.352 | 13.6 | 3.382 | 13.8 | 3.200 | 13.0 | +0.9% | -4.5% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

larson1 +1.3% (45.6 -> 45.0; old's list 20.8..48.8 with one outlier, new's
43.5..48.0, jemalloc's 40.7..47.8) and alloc-test1 +0.9% (3.352 -> 3.382,
both lists bimodal around 2.95 and 3.38): within 3% of old on every row.

## 4. Ruby rows (`make ruby-bench`, 5 rounds plus warm-up; `2026-09-18-x86_64-ruby.md`)

Ruby 3.4.10 `--with-jemalloc` frees through plain `free` (this task's
path). Medians, wall seconds, challoc against the same binary with Wolfi's
jemalloc 5.3.1 preloaded; the "before" column is the previous run of the
same rows on this VM (`2026-09-17-x86_64-ruby.md`, commit a10f012, task
4e's parent's parent; the machine was 2..5% faster across all four
allocators in today's run, so read the relative columns):

| row | challoc before | challoc now | jemalloc now | challoc vs jemalloc before -> now |
|---|---:|---:|---:|---:|
| so_binary_trees | 4.238 | 4.177 | 4.176 | +0.1% -> +0.0% |
| hash_aref_str | 0.310 | 0.311 | 0.306 | +2.0% -> +1.6% |
| array_sample_100k__1k | 0.484 | 0.478 | 0.484 | +0.0% -> -1.2% |
| array_sort_block | 4.472 | 4.489 | 4.518 | -0.7% -> -0.6% |
| so_nbody | 0.751 | 0.748 | 0.752 | +0.5% -> -0.5% |
| app_fib | 0.278 | 0.276 | 0.277 | -0.4% -> -0.4% |
| erb_render | 0.722 | 0.703 | 0.848 | -15.5% -> -17.1% |
| gem_list | 0.118 | 0.113 | 0.115 | +0.9% -> -1.7% |
| gc_churn | 2.031 | 1.929 | 1.980 | -1.4% -> -2.6% |

RSS: challoc below jemalloc on every row (so_binary_trees 27.7 against
38.0 MiB, gc_churn 384.6 against 380.1, +1.2%), as before. The Ruby rows
are not allocator-bound (the two allocators are within 3% everywhere but
`erb_render`); the plain-`free` gain shows on `gc_churn` (-1.4% -> -2.6%
against jemalloc) and `gem_list`, and nothing moved the other way.

## 5. Scorecard line (`bench/results/scorecard-x86_64.md`, 3 rounds, challoc against jemalloc, positive = challoc behind; code 73b6df1)

```
| 2026-09-18 | 27de567 | -3.0% | +6.1% | +6.2% | -5.8% | +3.9% | +4.6% | -0.9% | -10.6% | +3.5% | -0.3% | -37.9% | +2.8% | -15.6% | 1.04 | 0.99 | - | - | - | - | 3 rounds; no soak for 27de567 |
| 2026-09-18 | 73b6df1 | -2.6% | +1.3% | +7.0% | -4.4% | +4.5% | +8.8% | -3.3% | -14.4% | -5.6% | -10.6% | -38.1% | -5.8% | -24.9% | 1.06 | 0.99 | - | - | - | - | 3 rounds; no soak for 73b6df1 |
```

`valkey1-free` -10.6% -> -14.4% (challoc ahead of jemalloc by 14% on the
plain-`free` row; the 5-round A/B has 15.34 against 13.08 M ops/s, 17%),
`valkey1` -0.9% -> -3.3%; larson10 +6.1% -> +1.3%, alloc-testN +4.6% ->
+8.8% and the cfrac/espresso/redis columns moved 5..9% in challoc's favour,
all within this VM's 3-round spread (section 3's 5- and 10-round lists).
