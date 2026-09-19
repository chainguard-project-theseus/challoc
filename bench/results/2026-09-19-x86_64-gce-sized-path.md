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

Change (`Shared::sized_slot`, design 6.3 step 1): the size hint names the
class, so the pointer is checked against that class's region alone
(`Regions::slot_in_class`: one subtraction and a compare against the region
length, then the reciprocal slot test) instead of being classified from
the superblock and compared with the hint; the guard test is one 8-byte
load from a dense per-class pair (`Shared::guard_test[c]`, `(slab + spp) &
mask == 0`, `is_guard_slab` folded); the class table line is not touched,
and the watermark is not read: a slot start in a slab past it is caught at
the flush, which aborts by name (`SlabTable::owner`), the same way a slot
in a `Fresh` slab already was (new test
`sized_free_past_the_watermark_aborts_at_the_flush`;
`guard_test_agrees_with_the_tables` checks the folded form against every
class). Also `sdallocx_routed` takes the raw flags (the `Flags` struct
went by pointer to a stack copy) and `class_for`'s alignment loop indexes
with `get` (no bounds-check panic call in the inlined body). Every abort
the negative tests expect is kept (`sized_double_free`, `sized_mismatch`,
`free_sized_mismatch`, `sized_guard`, `double_free_straddle`,
`usable_size_freed` pass on the Mac and both VMs).

Static count of the default `sdallocx` fast path (flags 0, small size,
existing heap, no hardening), from `objdump` of the release build: x86_64
parent about 102 instructions (the metal annotate of
`2026-09-19-4b-c3metal-annotate-sdallocx.txt`), after (a) 101, after (b)
**78**; aarch64 after (a) 88, after (b) **69**. The frame (`push
r14/rbx/rax` and the pops; `stp/ldp x29,x30` on aarch64) remains: the
`double_free_scan` path's `cached_twice` is a real call inside the inlined
push, so the function keeps a frame it does not need on the default path
(a further 7 instructions on x86_64 if that abort became a tail call; not
done here).

### 2.1 `quick-perf`, after (b) (same run: `old` = (a), `new` = (b), jemalloc)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op | arith.div_active/op |
|---|---|---:|---:|---:|---:|---:|---:|
| valkey1 | old (a) | 24.94 | 153.2 | 247.4 | 3.324 | 0.906 | 33.56 |
| valkey1 | new (b) | 25.59 | 149.1 | 225.4 | 3.280 | 0.899 | 33.15 |
| valkey1 | jemalloc | 28.12 | 135.2 | 152.3 | 3.339 | 0.950 | 34.57 |
| valkey1-free | old (a) | 25.24 | 150.3 | 225.4 | 4.588 | 0.927 | 33.05 |
| valkey1-free | new (b) | 25.00 | 152.2 | 224.2 | 4.532 | 0.924 | 33.07 |
| valkey1-free | jemalloc | 24.74 | 153.9 | 157.8 | 4.683 | 1.203 | 34.52 |
| larson1 | old (a) | 46.75 | 72.5 | 234.3 | 1.588 | 0.002 | 20.51 |
| larson1 | new (b) | 46.51 | 72.4 | 234.5 | 1.619 | 0.002 | 20.53 |
| larson1 | jemalloc | 43.29 | 77.9 | 200.9 | 2.085 | 0.001 | 20.88 |

valkey1: 22 fewer instructions per operation (247.4 -> 225.4; the row
mixes the write phase, two `sdallocx` per SET, with the delete phase), 4
fewer cycles, misses equal; the plain-free row and larson1 are untouched
(the plain path did not change).

### 2.2 `quick-perf`, the write phase alone (`valkey1-write`, per SET)

`valkey1-write` is the new row (`-w SECS -d 0`, `bench/lib.sh`), so the
per-operation counters are per SET: two `malloc`, two `sdallocx`, the
program's hash-table work.

| row | variant | M SET/s | cycles/SET | instr/SET | L1D miss/SET | dTLB miss/SET | arith.div_active/SET |
|---|---|---:|---:|---:|---:|---:|---:|
| valkey1-write | old (a) | 14.95 | 254.9 | 384.6 | 4.673 | 0.949 | 20.27 |
| valkey1-write | new (b) | 14.83 | 255.7 | 344.6 | 4.604 | 0.950 | 20.32 |
| valkey1-write | jemalloc | 14.82 | 256.9 | 268.2 | 4.848 | 1.028 | 20.97 |

**Instructions per SET: 384.6 after (a), 344.6 after (b)**, jemalloc 268.2
(the metal's parent reading was 391 to 401 against 266). The task's target
of under 330 is not reached: 344.6 is 15 above it, and the terms left on
the path are the frame the `cached_twice` call keeps (7 instructions per
`sdallocx`, 14 per SET, which alone would land at about 331), the eager
`1 << lg_align` of `Flags::align()` (3 per call) and the two-register
`Option<usize>` hint. The divider column is the program's own 20 per SET
under all three (challoc's two `divl` are gone since (a)); cycles per SET
and the rate are equal for the three on this host, which is what the
attribution said of the reference C4 (its L3 holds the table, so the
window does not bind and the extra instructions cost nothing measurable
here); the C3 lab is where they cost, section 2.4.

### 2.3 `quick` after (b) (go: within 5% on every row, churn compacts)

| row | unit | old (a) | new (b) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 46.3 | 46.7 | 45.6 | -0.9% | -2.4% |
| larson10 | M ops/s | 284.7 | 295.5 | 292.5 | -3.7% | -1.0% |
| xmalloc | M frees/s | 343.810 | 335.979 | 334.012 | +2.3% | -0.6% |
| alloc-test1 | s | 2.998 | 3.030 | 2.874 | +1.1% | +5.4% |
| valkey1 | M ops/s | 15.030 | 14.581 | 15.690 | +3.1% | +7.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6658 (49.9%) | compacts | |

(valkey1's single round reads 3% behind (a) here while the same-run
`quick-perf` row above reads 2.6% ahead at 22 fewer instructions and 4
fewer cycles per op; the C4 row's band is about 5% between rounds, and
the C3 lab below is the measure the task is judged on.)

### 2.4 The C3 lab (8481C, 8 vCPU): the write-phase rate after (b)

Three interleaved runs of `valkey_model -n 850000 -w 8 -d 0 -s`, M SET/s;
the `a` library is the (a) build kept from section 1.4.

| run | parent | (a) | new (b) | jemalloc |
|---|---:|---:|---:|---:|
| 1 | 8.148 | 10.129 | 11.136 | 10.773 |
| 2 | 7.648 | 9.785 | 10.825 | 10.485 |
| 3 | 7.367 | 9.750 | 10.676 | 10.426 |
| median | 7.65 | 9.79 | 10.83 | 10.49 |

Parent -27%, (a) -6.7%, (b) **+3.2%** against jemalloc in the same run
(the VM read about 3% slower for every variant than in section 1.4; the
lab is a shared 8-vCPU host and the comparison is within a run). `quick`
on the lab (go): larson1 39.6 vs jemalloc 39.9, larson10 153.4 vs 164.2,
xmalloc 158.6 vs 160.2, alloc-test1 3.858 vs 3.646 s, valkey1 10.180 (old
(a) 9.068) vs 9.887, churn compacts.

## 3. Method

- `quick-perf.sh` prints one column per event beyond the four standard
  ones (`EVENTS=...,arith.div_active`; a raw `rXXXX` form for an unnamed
  event). On the C4A there is no divider event (`dp_spec` recorded as the
  nearest, see the aarch64 file).
- The lab VM: `challoc-bench matrix lab cpu-c3` (created in `us-east4-a`,
  provisioned, tree synced, kept), `make image` and `make quick
  QUICK_BUILD_ONLY=1` once, then the parent, (a) and (b) libraries copied
  out of `target/quick/new/release/` inside the target volume after each
  sync and rebuild, and a rate script shipped by base64 that interleaves
  the variants run by run. `matrix clean` deleted it at the end.
- Static instruction counts: `objdump -d` of `je_sdallocx` in the native
  aarch64 container and the emulated x86_64 one (`PLATFORM=linux/amd64`),
  counting the taken path for flags 0.
