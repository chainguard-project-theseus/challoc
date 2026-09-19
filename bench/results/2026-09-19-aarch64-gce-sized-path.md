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

The change is described in the x86_64 file (section 2); the aarch64 static
count of the default `sdallocx` fast path goes from 88 instructions after
(a) to **69** after (b) (the superblock classification, the class
compare and the class-table-line address computation and loads are gone;
`stp/ldp x29,x30` stays for the scan path's `cached_twice` call).

### 2.1 `quick-perf`, after (b) (same run: `old` = (a), `new` = (b), jemalloc)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op | dp_spec/op |
|---|---|---:|---:|---:|---:|---:|---:|
| valkey1 | old (a) | 12.92 | 234.9 | 254.0 | 3.082 | 2.847 | 183.91 |
| valkey1 | new (b) | 13.01 | 233.0 | 232.3 | 3.068 | 2.830 | 168.30 |
| valkey1 | jemalloc | 13.58 | 222.9 | 171.1 | 2.972 | 2.726 | 127.72 |
| valkey1-free | old (a) | 12.81 | 236.9 | 237.4 | 4.062 | 3.930 | 169.12 |
| valkey1-free | new (b) | 12.69 | 238.9 | 236.9 | 4.045 | 3.935 | 169.33 |
| valkey1-free | jemalloc | 13.15 | 230.3 | 174.0 | 3.432 | 3.363 | 129.23 |
| larson1 | old (a) | 42.02 | 71.2 | 249.4 | 2.093 | 4.104 | 134.91 |
| larson1 | new (b) | 42.07 | 71.1 | 249.4 | 2.090 | 4.081 | 134.91 |
| larson1 | jemalloc | 44.63 | 66.7 | 224.8 | 2.084 | 1.782 | 123.74 |

valkey1: 21.7 fewer instructions per operation (254.0 -> 232.3), 15.6
fewer `dp_spec`, cycles 234.9 -> 233.0 (the Axion's window hides the rest,
as it hid the parent's gap); plain free and larson1 untouched.

### 2.2 `quick-perf`, the write phase alone (`valkey1-write`, per SET)

| row | variant | M SET/s | cycles/SET | instr/SET | L1D miss/SET | dTLB miss/SET | dp_spec/SET |
|---|---|---:|---:|---:|---:|---:|---:|
| valkey1-write | old (a) | 6.68 | 453.3 | 383.8 | 4.708 | 3.894 | 208.01 |
| valkey1-write | new (b) | 6.74 | 450.3 | 345.5 | 4.699 | 3.903 | 182.19 |
| valkey1-write | jemalloc | 6.81 | 444.6 | 307.7 | 4.742 | 3.761 | 164.59 |

Instructions per SET 383.8 -> 345.5 (jemalloc 307.7 here: its aarch64
build is longer than its x86_64 one, 268), cycles 453.3 -> 450.3 -> the
gap to jemalloc is 1.3% of cycles per SET on the Axion.

### 2.3 `quick` after (b) (go: within 5% on every row, churn compacts)

| row | unit | old (a) | new (b) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.4 | 42.5 | 44.9 | -0.2% | +5.6% |
| larson10 | M ops/s | 365.3 | 373.6 | 383.6 | -2.2% | +2.7% |
| xmalloc | M frees/s | 604.499 | 602.686 | 481.364 | +0.3% | -20.1% |
| alloc-test1 | s | 2.675 | 2.644 | 2.543 | -1.2% | +4.0% |
| valkey1 | M ops/s | 6.764 | 6.999 | 6.817 | -3.4% | -2.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | compacts | |

valkey1 on the C4A: (b) 6.999 against jemalloc's 6.817 in the same run
(+2.7%), from parity for (a).
