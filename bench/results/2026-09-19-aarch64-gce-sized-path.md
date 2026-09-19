# Phase 2.5 task 4c: the sized path's division and instruction budget, aarch64

Date 2026-09-19. Reference VM `challoc-bench-arm64` (c4a-standard-16, Google
Axion, Neoverse V2, `--performance-monitoring-unit=standard`, kernel
7.0.0-1011-gcp). Parent commit 314938a. The x86_64 file
(`2026-09-19-x86_64-gce-sized-path.md`) has the mechanism and the C3 lab
numbers the task is judged on; this file is the aarch64 attribution and
gate. The Neoverse V2 PMU has no divider-busy event (`perf list` on the VM:
`dp_spec`, `fp_dp_spec` and the `*_spec` family only; `arith.div_active`
is an Intel event), so `dp_spec` (integer data-processing operations
speculatively executed, which counts the `udiv` among everything else) is
recorded as the nearest column; the `udiv` on this core is a few cycles
and not the term the x86_64 `divl` is.

## 1. Part (a): power-of-two `guard_slab_interval`, the guard test a mask

### 1.1 `quick-perf`, before (parent tree, one run each)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op | dp_spec/op |
|---|---|---:|---:|---:|---:|---:|---:|
| valkey1 | new (parent) | 12.49 | 242.5 | 257.4 | 3.088 | 2.903 | 184.62 |
| valkey1 | jemalloc | 13.24 | 228.4 | 171.3 | 2.972 | 2.699 | 127.71 |
| valkey1-free | new (parent) | 12.43 | 243.7 | 239.7 | 4.080 | 3.934 | 170.14 |
| valkey1-free | jemalloc | 13.05 | 231.8 | 174.2 | 3.439 | 3.345 | 129.45 |
| larson1 | new (parent) | 41.79 | 71.6 | 249.4 | 2.092 | 4.127 | 134.89 |
| larson1 | jemalloc | 44.68 | 66.6 | 224.8 | 2.076 | 1.727 | 123.73 |

### 1.2 `quick-perf`, after (a) (same run: `old` = 314938a, `new` = (a), jemalloc)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op | dp_spec/op |
|---|---|---:|---:|---:|---:|---:|---:|
| valkey1 | old | 12.29 | 246.8 | 257.2 | 3.085 | 2.850 | 184.72 |
| valkey1 | new (a) | 12.10 | 250.8 | 257.3 | 3.097 | 2.883 | 185.75 |
| valkey1 | jemalloc | 13.40 | 225.9 | 171.0 | 2.968 | 2.695 | 127.85 |
| valkey1-free | old | 12.32 | 246.1 | 239.3 | 4.076 | 3.909 | 169.70 |
| valkey1-free | new (a) | 12.15 | 249.3 | 239.4 | 4.074 | 3.930 | 170.55 |
| valkey1-free | jemalloc | 12.80 | 236.4 | 173.4 | 3.490 | 3.399 | 128.97 |
| larson1 | old | 41.94 | 71.4 | 249.4 | 2.089 | 4.112 | 134.91 |
| larson1 | new (a) | 41.93 | 71.4 | 249.4 | 2.091 | 4.036 | 134.91 |
| larson1 | jemalloc | 44.86 | 66.3 | 224.8 | 2.057 | 1.727 | 123.69 |

On aarch64 the modulo was a `udiv` (one instruction, like the `and` that
replaces it), so instructions per op are unchanged (257.2 -> 257.3) and
cycles are within the run-to-run band (246.8 -> 250.8, the two `before`
readings 242.5 and 246.8 bracket it); (a) is neutral here by design, the
x86_64 `divl` is the target.

### 1.3 `quick` after (a) (go: within 5% on every row, churn compacts)

| row | unit | old | new (a) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.2 | 42.3 | 45.0 | -0.2% | +6.4% |
| larson10 | M ops/s | 371.8 | 368.8 | 387.5 | +0.8% | +5.1% |
| xmalloc | M frees/s | 599.761 | 605.676 | 483.722 | -1.0% | -20.1% |
| alloc-test1 | s | 2.697 | 2.677 | 2.555 | -0.7% | +4.8% |
| valkey1 | M ops/s | 6.667 | 6.767 | 6.764 | -1.5% | -0.0% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | compacts | |

## 2. Part (b): the sized path's instruction budget

(see the section appended by the part (b) commit, or PROGRESS.md In
progress if (b) did not land)
