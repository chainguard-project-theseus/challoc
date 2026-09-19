# Phase 2.5 task 4c: the sized path's division and instruction budget, x86_64

Date 2026-09-19. Reference VM `challoc-bench-amd64` (c4-standard-16, Intel
Xeon Platinum 8581C, Emerald Rapids, `--performance-monitoring-unit=standard`,
kernel 7.0.0-1011-gcp), plus a throwaway lab VM `challoc-matrix-cpu-c3`
(c3-standard-8, Xeon Platinum 8481C, Sapphire Rapids, no PMU) for the
write-phase rate the task is judged on. Parent commit 314938a. The
attribution this rests on is
`2026-09-19-8481c-valkey1-attribution.md` sections 7 and 8: the valkey1
gap on small-L3 hosts is the sized path's instruction window, and its
largest single term is `SlabTable::is_guard_slab`'s runtime `%`
(`divl %r11d`, 12.5% of `je_sdallocx`'s samples on the metal), twice per
Valkey SET.

## 1. Part (a): power-of-two `guard_slab_interval`, the guard test a mask

Change: `guard_slab_interval` must be a power of two (the default 8 is; a
configured value that is not rounds up to the next one and is reported as
`Issue::Rounded`, which `abort_conf` turns into an abort; `opt.guard_slab_interval`
returns the rounded value; 1 still clamps to 2 as `OutOfRange`).
`SlabTable` stores `guard_mask = interval - 1` and `is_guard_slab` is
`(page_group + 1) & guard_mask == 0`, with guards off the mask is all ones
and no page group a region can have matches. Guard density per byte for
the default 8 is unchanged; `sized_slot_rejects_guard_slabs`,
`tests/c/guard_slab.c` and `negative.c sized_guard` keep their semantics.

### 1.1 `quick-perf`, before (parent tree, one run each)

`bench/gce/challoc-bench quick-perf amd64 VARIANTS='new jemalloc' ROWS='valkey1
valkey1-free larson1' EVENTS=...,arith.div_active` at 314938a (`new` = the
parent; `quick-perf.sh` now prints one column per extra event).

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op | arith.div_active/op |
|---|---|---:|---:|---:|---:|---:|---:|
| valkey1 | new (parent) | 22.20 | 166.0 | 259.6 | 3.363 | 0.905 | 39.99 |
| valkey1 | jemalloc | 26.64 | 138.4 | 153.6 | 3.333 | 0.942 | 34.37 |
| valkey1-free | new (parent) | 22.94 | 160.2 | 228.8 | 4.595 | 0.926 | 33.15 |
| valkey1-free | jemalloc | 25.15 | 146.6 | 157.6 | 4.676 | 1.204 | 34.56 |
| larson1 | new (parent) | 46.20 | 72.0 | 234.3 | 1.611 | 0.002 | 20.51 |
| larson1 | jemalloc | 44.17 | 75.1 | 200.9 | 2.140 | 0.001 | 20.86 |

`arith.div_active` under jemalloc (34 per op on valkey1, 21 on larson1) is
the programs' own divisions (the model's `% n` key selection, larson's
`rand() %`); challoc's excess on the sized row is the 6 per op of
`is_guard_slab` (two `divl` per SET, none per DEL; `ops` counts both
phases' operations), and the plain-free row (`valkey1-free`) has none.

### 1.2 `quick-perf`, after (a) (same run: `old` = 314938a, `new` = (a), jemalloc)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op | arith.div_active/op |
|---|---|---:|---:|---:|---:|---:|---:|
| valkey1 | old | 23.60 | 155.7 | 255.4 | 3.330 | 0.905 | 40.11 |
| valkey1 | new (a) | 23.73 | 154.9 | 253.0 | 3.356 | 0.905 | 32.97 |
| valkey1 | jemalloc | 27.41 | 134.6 | 154.3 | 3.392 | 0.951 | 34.22 |
| valkey1-free | old | 23.56 | 155.5 | 227.9 | 4.601 | 0.931 | 33.20 |
| valkey1-free | new (a) | 24.40 | 150.9 | 225.5 | 4.565 | 0.922 | 33.16 |
| valkey1-free | jemalloc | 24.18 | 151.1 | 158.7 | 4.703 | 1.214 | 34.40 |
| larson1 | old | 45.76 | 72.4 | 234.2 | 1.538 | 0.002 | 20.52 |
| larson1 | new (a) | 45.93 | 72.5 | 234.1 | 1.583 | 0.002 | 20.50 |
| larson1 | jemalloc | 45.65 | 72.3 | 200.9 | 2.119 | 0.001 | 20.75 |

The divider column on valkey1 drops from 40.1 to 33.0 per op, at jemalloc's
level (34.2; the rest is the program's), the instruction count by 2.4 per
op (the `xorl`/`divl`/`testl` sequence against the `andl`), cycles within
noise on this 5 MiB-L3-per-core host, as the attribution predicted (the
reference C4 hides the window effect). `valkey1-free` and larson1 are
unchanged (no division on the plain path).

### 1.3 `quick` after (a) (go: within 5% on every row, churn compacts)

| row | unit | old | new (a) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.9 | 45.8 | 44.9 | +0.2% | -2.0% |
| larson10 | M ops/s | 291.6 | 285.3 | 296.9 | +2.2% | +4.1% |
| xmalloc | M frees/s | 341.041 | 341.327 | 330.346 | -0.1% | -3.2% |
| alloc-test1 | s | 3.111 | 3.122 | 2.969 | +0.4% | +5.2% |
| valkey1 | M ops/s | 14.307 | 15.050 | 14.877 | -4.9% | -1.1% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6646 (50.1%) | compacts | |

(The `new vs old` column is `quick.sh`'s sign convention: negative means
`new` is ahead on a higher-is-better row; valkey1 is +5.2% for (a).)

### 1.4 The C3 lab (8481C, 8 vCPU, no PMU): the write-phase rate

`valkey_model -n 850000 -w 8 -d 0 -s` in the dev container, three
interleaved runs per variant, M SET/s (`write_mops`); `parent` is the
314938a library copied out of the first `target/quick/new` build.

| run | parent | new (a) | jemalloc |
|---|---:|---:|---:|
| before, 1 | 7.911 | | 10.843 |
| before, 2 | 8.106 | | 10.862 |
| before, 3 | 7.930 | | 10.804 |
| after, 1 | 8.049 | 10.082 | 10.849 |
| after, 2 | 8.045 | 10.131 | 10.776 |
| after, 3 | 7.917 | 10.023 | 10.812 |
| median | 7.98 | 10.08 | 10.83 |

Parent -26.3% against jemalloc, (a) **-6.9%**: inside the task's 10%
acceptance band (the attribution's `guards:false` upper bound was -4%).
`quick` on the lab (go): larson1 40.4 vs jemalloc 39.8, larson10 152.6 vs
167.2, xmalloc 160.2 vs 159.1, alloc-test1 3.819 vs 3.603 s, valkey1
10.176 (old 8.202) vs 10.764, churn compacts; the lab's larson1 `old`
reading (19.2) is a single-round outlier of the busy 8-vCPU VM (the
`quick-perf` rows above and the `after` runs show larson unchanged).

## 2. Part (b): the sized path's instruction budget

(see the section appended by the part (b) commit, or PROGRESS.md In
progress if (b) did not land)
