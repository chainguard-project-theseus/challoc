# Per-CPU dispatch at zero cost on the default path, x86_64

Lane percpu-dispatch, 2026-09-21, VM `challoc-percpu-dispatch-amd64`
(c4-standard-16, Intel Xeon Platinum 8581C, kernel 7.0.0-1011-gcp, glibc
2.44, `rseq=yes rseq_size=33 mm_cid=yes`, 16 possible CPUs). `old` is
`main` 2078f67, `new` the lane's tree (da39e2e, cc547ed), jemalloc Wolfi's
5.3.1. The shape, the rejected shapes and the mechanism are in the
aarch64 file, `2026-09-21-aarch64-percpu-dispatch.md`, section 0; this
file is the C4's numbers.

## 1. Codegen (`scripts/codegen-diff.sh`, q16, x86_64)

`.text` bfd1d78157884485 -> differs; functions 499 -> 493; exact 38,
reloc-only 422, changed 33.

| function | old (2078f67) | new (tree) | note |
|---|---:|---:|---|
| `malloc` (`je_malloc`) | 59 | 55 | the flag test and tail call gone |
| `free` (`_ZdaPv`) | 141 | 130 | |
| `sdallocx` (`je_sdallocx`) | 198 | 180 | |
| `free_sized` (`_ZdaPvm`) | 155 | 141 | |
| `calloc` | 195 | 128 | |
| `operator new[]` (`_Znam`) | 128 | 60 | |
| `malloc_usable_size` | 87 | 87 | unchanged |
| `malloc_slow` | 175 | 111 | |
| `free_slow` / `free_sized_slow` | 176 / 177 | 176 / 178 | |
| `percpu_malloc` / `percpu_free` / `percpu_free_sized` | 53 / 104 / 93 | gone | |
| `Heap::percpu_alloc` / `Heap::percpu_free` | - | 50 / 126 | new |

The rest of the changed list is the same set as on aarch64 (the per-CPU
slow paths, `ensure_cache`, `alloc_aligned` 259 -> 203, `alloc_with` 476
-> 375, `realloc` 480 -> 476, `free_aligned_sized` 174 -> 164, `cxx::oom`,
`init`, `create_heap_for_thread`, `set_arena`, `set_tcache_enabled`,
`STATS_PERCPU`).

## 2. Per-operation counters (`challoc-bench quick-perf amd64 OLD=2078f67`)

Two runs, dd7f9b5 / cc547ed as in the aarch64 file.

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old (2078f67) | 44.87 / 45.28 | 73.2 / 73.0 | 244.4 / 244.2 | 1.606 / 1.608 | 0.002 / 0.002 |
| larson1 | new | 44.43 / 43.82 | 74.1 / 75.7 | 236.3 / 236.2 | 1.579 / 1.595 | 0.002 / 0.002 |
| larson1 | jemalloc | 45.39 / 45.51 | 72.2 / 72.9 | 200.9 / 200.9 | 2.112 / 2.138 | 0.001 / 0.001 |
| larson1 | new:percpu_arena:percpu | 24.88 / 26.00 | 131.9 / 130.6 | 400.4 / 388.2 | 1.933 / 1.935 | 0.006 / 0.004 |
| larson1 | old:percpu_arena:percpu | 43.06 / 43.75 | 76.0 / 75.9 | 276.9 / 277.0 | 1.770 / 1.774 | 0.006 / 0.006 |
| valkey1 | old | 24.09 / 25.79 | 141.2 / 143.7 | 239.6 / 234.1 | 3.314 / 3.274 | 0.907 / 0.901 |
| valkey1 | new | 24.08 / 25.65 | 141.7 / 144.6 | 232.5 / 227.9 | 3.309 / 3.301 | 0.908 / 0.904 |
| valkey1 | jemalloc | 28.38 / 28.50 | 127.5 / 130.1 | 152.5 / 152.9 | 3.356 / 3.360 | 0.950 / 0.951 |
| valkey1 | new:percpu_arena:percpu | 15.70 / 16.96 | 226.8 / 222.9 | 467.8 / 450.0 | 4.956 / 4.979 | 0.924 / 0.930 |
| valkey1 | old:percpu_arena:percpu | 20.04 / 21.68 | 174.4 / 177.0 | 296.3 / 292.6 | 3.307 / 3.263 | 0.906 / 0.906 |
| valkey1-free | old | 24.48 / 25.66 | 146.0 / 148.2 | 235.0 / 234.1 | 4.575 / 4.538 | 0.920 / 0.923 |
| valkey1-free | new | 23.25 / 25.18 | 145.9 / 149.3 | 234.8 / 229.1 | 4.643 / 4.593 | 0.928 / 0.926 |
| valkey1-free | jemalloc | 24.86 / 25.59 | 136.0 / 144.2 | 159.5 / 158.4 | 4.738 / 4.701 | 1.207 / 1.201 |
| valkey1-free | new:percpu_arena:percpu | 16.03 / 17.82 | 210.8 / 207.0 | 416.1 / 400.1 | 4.976 / 4.989 | 0.933 / 0.933 |
| valkey1-free | old:percpu_arena:percpu | 19.53 / 21.09 | 173.6 / 175.3 | 288.5 / 284.0 | 4.591 / 4.573 | 0.922 / 0.927 |

Reading, the default path: larson1 244.4 -> 236.2 instructions per
operation (-8.1; the drift file measured the same +9 on both archs), the
cycles +1.2% then +3.7% in the two single runs, against the drift file's
observation that the C4 "pays the same instructions and reads -1.8%" for
the tail-call dispatch (a wider front end hides the instructions either
way). valkey1 -6..-7 instructions, cycles within 1%; valkey1-free flat.
The protocol tier below is the judge of the C4's larson1 cycles.

Reading, the mode: larson1 277 -> 388 instructions, 75.9 -> 130.6 cycles
(+72%); valkey1 293 -> 450, 177 -> 223 cycles (+26%). The mechanism is
the aarch64 file's section 2.

## 3. Fast tier (`challoc-bench quick amd64 OLD=2078f67`)

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.6 (17.1 MiB) | 46.2 (17.1 MiB) | 45.9 (18.0 MiB) | -1.3% | -0.6% |
| larson10 | M ops/s | 287.4 (96.4 MiB) | 284.3 (96.0 MiB) | 293.5 (93.0 MiB) | +1.1% | +3.2% |
| xmalloc | M frees/s | 334.153 (53.2 MiB) | 332.554 (53.3 MiB) | 333.105 (95.2 MiB) | +0.5% | +0.2% |
| alloc-test1 | s | 2.990 (13.9 MiB) | 3.015 (14.0 MiB) | 2.818 (13.4 MiB) | +0.8% | +7.0% |
| valkey1 | M ops/s | 15.927 (585.3 MiB) | 16.146 (585.4 MiB) | 15.073 (592.0 MiB) | -1.4% | -6.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6667 (49.9%) | new compacts (end occupancy >= 90%) | 21774428 ops/s |

`quick: go` (new within 5% of old on every row; churn compacts). Tree cc547ed, one round, 2026-09-21T04:23Z, load 0.45.

## 4. Protocol tier (`challoc-bench ab amd64`, 5 rounds, same run, `2026-09-21-x86_64-percpu-dispatch-ab.md`)

AB_PLACEHOLDER

## 5. Tests

`make test` on the VM at dd7f9b5 and cc547ed: 481 + 138 + 26 passed, 0
failed; churn, lazy and pagemap-guard steps `ok`; `c_percpu_both_ways`
with the mode on (`enabled=1 ids=16 headers=219 cached=54124 aborts=0`).
