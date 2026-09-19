# 2026-09-19: THP counters next to the dTLB column (Phase 2.5 task 17 item 2, lane D, aarch64)

- what: `CHALLOC_THP_COUNTERS=1` (bench/lib.sh `thp_snapshot`/`thp_delta`, commit faaadd4)
  snapshots every `thp_*` line of `/proc/vmstat` and every
  `/sys/kernel/mm/transparent_hugepage/hugepages-<size>kB/stats/*` counter before and after a
  row and prints the deltas: in `challoc-bench quick-perf` (host side, next to the per-operation
  counters), in `ab` (a "THP counters" section, mean per timed run) and in `quick` (first
  round). This file is the first run of the column, around `larson1` and `valkey1` under
  `thp:never` (the default, the `new` variant), `thp:auto` and `thp:always`, in the three sysfs
  states lane B's task 9 file used (`bench/results/2026-09-19-lane-b-mthp.md`, section 1), so
  task 9's dTLB story has the folio counts beside it.
- VM: `challoc-lane-d-arm64` (c4a-standard-16, Axion / Neoverse V2, Ubuntu 24.04, kernel
  `7.0.0-1011-gcp`, 4 KiB pages, PMU exposed), created for this task and deleted at its end;
  the dev container (Wolfi) for the rows, `perf` and the sysfs writes on the host. Library:
  HEAD 79e3931 (the lane's tree; `old` and `new` are the same build 58afede248f5ea5a, no
  `src/` change in this task). One `quick-perf` run per (row, variant, state): attribution, not
  a gate; `ab` with ROUNDS=2, a wiring check, not a protocol number.
- sysfs states (`~/logs/item2-chain.sh` on the VM): `default` = Ubuntu's table, `enabled
  [madvise]`, `hugepages-2048kB [inherit]`, every smaller size `[never]` (so `thp:auto` is
  inert by design and `thp:always` gets 2 MiB chunks); `auto` = `hugepages-64kB [madvise]`,
  `hugepages-2048kB [never]` (`thp:auto` gives 64 KiB folios; `thp:always` has no PMD and falls
  back to the same folios); `always` = `hugepages-64kB [madvise]`, `hugepages-2048kB
  [inherit]` (`thp:always` gets chunks; `thp:auto` refuses because the PMD is madvise-gated,
  lane B section 1). Restored to `default` at the end.
- the counters are host-global: no other load ran on the VM (load 0.6..1.0 on 16 CPUs); the
  `quick` run under `never` saw one stray `64kB/anon_fault_alloc=1` in one row, which is the
  noise level.

## 1. `quick-perf`: per-operation counters with the THP deltas beside them

`bash bench/gce/quick-perf.sh arm64 ROWS='larson1 valkey1' VARIANTS='new new:thp:auto
new:thp:always jemalloc' CHALLOC_THP_COUNTERS=1`. Deltas are per whole run (larson1 5 s,
valkey1 10 s with an 850k-key fill).

### default state (`enabled=madvise`, only `2048kB=inherit`)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op | THP deltas |
|---|---|---:|---:|---:|---:|---:|---|
| larson1 | new (never) | 41.10 | 72.8 | 264.5 | 2.099 | 4.069 | none |
| larson1 | new:thp:auto | 41.19 | 72.7 | 264.5 | 2.103 | 3.989 | thp_collapse_alloc=1 |
| larson1 | new:thp:always | 40.70 | 73.6 | 264.5 | 2.097 | 3.617 | thp_fault_alloc=25 2048kB/anon_fault_alloc=25 thp_split_pmd=5 thp_deferred_split_page=5 2048kB/split_deferred=5 thp_collapse_alloc=1 |
| larson1 | jemalloc | 44.65 | 66.7 | 224.8 | 2.073 | 1.727 | none |
| valkey1 | new (never) | 12.16 | 248.9 | 250.1 | 3.082 | 2.787 | none |
| valkey1 | new:thp:auto | 12.34 | 246.2 | 254.3 | 3.123 | 2.840 | thp_collapse_alloc=1 |
| valkey1 | new:thp:always | 13.04 | 230.3 | 239.6 | 3.065 | 1.074 | thp_fault_alloc=318 2048kB/anon_fault_alloc=318 thp_split_pmd=280 thp_deferred_split_page=280 2048kB/split_deferred=280 thp_collapse_alloc=1 |
| valkey1 | jemalloc | 13.22 | 229.0 | 169.4 | 2.955 | 2.692 | none |

### auto state (`64kB=madvise`, `2048kB=never`)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op | THP deltas |
|---|---|---:|---:|---:|---:|---:|---|
| larson1 | new (never) | 41.42 | 72.3 | 264.5 | 2.101 | 3.983 | none |
| larson1 | new:thp:auto | 40.77 | 73.4 | 264.5 | 2.094 | 3.046 | 64kB/anon_fault_alloc=71 thp_collapse_alloc=1 |
| larson1 | new:thp:always | 40.84 | 73.3 | 264.5 | 2.106 | 3.045 | 64kB/anon_fault_alloc=73 thp_collapse_alloc=1 |
| larson1 | jemalloc | 44.60 | 66.7 | 224.8 | 2.072 | 1.770 | none |
| valkey1 | new (never) | 11.53 | 262.9 | 254.4 | 3.121 | 2.819 | none |
| valkey1 | new:thp:auto | 11.46 | 263.9 | 254.8 | 3.132 | 2.449 | 64kB/anon_fault_alloc=2788 thp_collapse_alloc=1 |
| valkey1 | new:thp:always | 11.95 | 253.1 | 250.4 | 3.100 | 2.366 | 64kB/anon_fault_alloc=3306 thp_collapse_alloc=1 |
| valkey1 | jemalloc | 12.52 | 241.5 | 172.6 | 2.994 | 2.742 | none |

### always state (`64kB=madvise`, `2048kB=inherit`)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op | THP deltas |
|---|---|---:|---:|---:|---:|---:|---|
| larson1 | new (never) | 41.17 | 72.7 | 264.5 | 2.101 | 4.078 | none |
| larson1 | new:thp:auto | 41.11 | 72.8 | 264.5 | 2.117 | 4.008 | thp_collapse_alloc=1 |
| larson1 | new:thp:always | 40.69 | 73.6 | 264.5 | 2.114 | 2.667 | thp_fault_alloc=25 2048kB/anon_fault_alloc=25 64kB/anon_fault_alloc=24 thp_split_pmd=5 thp_deferred_split_page=5 2048kB/split_deferred=5 thp_collapse_alloc=1 |
| larson1 | jemalloc | 44.71 | 66.6 | 224.8 | 2.073 | 1.695 | none |
| valkey1 | new (never) | 11.70 | 259.1 | 254.5 | 3.122 | 2.844 | none |
| valkey1 | new:thp:auto | 11.54 | 262.7 | 254.6 | 3.116 | 2.807 | thp_collapse_alloc=1 |
| valkey1 | new:thp:always | 12.87 | 233.2 | 241.7 | 3.079 | 0.664 | thp_fault_alloc=318 2048kB/anon_fault_alloc=318 64kB/anon_fault_alloc=28 thp_split_pmd=282 thp_deferred_split_page=282 2048kB/split_deferred=282 thp_collapse_alloc=1 |
| valkey1 | jemalloc | 12.31 | 245.8 | 172.2 | 2.984 | 2.739 | none |

## 2. `ab` and `quick` with the section (the wiring; ROUNDS=2)

`make ab VARIANTS='never=HEAD auto=HEAD:thp:auto always=HEAD:thp:always jemalloc' BENCHES='larson1
valkey1' ROUNDS=2 CHALLOC_THP_COUNTERS=1` in the auto state, full file
`2026-09-19-aarch64-lane-d-task17-thp-counters-ab-auto.md`; the always state with
`never=HEAD always=HEAD:thp:always`, `...-ab-always.md`. The "THP counters" section of each
(mean deltas per timed run):

| state | program | variant | mean deltas per run | median M ops/s | RSS MiB |
|---|---|---|---|---:|---:|
| auto | larson1 | never | (none) | 41.6 | 15.9 |
| auto | larson1 | auto | 64kB/anon_fault_alloc=76.0 thp_collapse_alloc=1.0 | 41.1 | 18.1 |
| auto | larson1 | always | 64kB/anon_fault_alloc=76.0 thp_collapse_alloc=1.0 | 41.1 | 18.0 |
| auto | larson1 | jemalloc | (none) | 44.7 | 17.3 |
| auto | valkey1 | never | (none) | 6.560 | 584.5 |
| auto | valkey1 | auto | 64kB/anon_fault_alloc=2788.0 thp_collapse_alloc=1.0 | 6.298 | 585.8 |
| auto | valkey1 | always | 64kB/anon_fault_alloc=3308.0 thp_collapse_alloc=1.0 | 6.759 | 586.0 |
| auto | valkey1 | jemalloc | (none) | 6.226 | 585.6 |
| always | larson1 | never | (none) | 41.6 | 15.9 |
| always | larson1 | always | 2048kB/anon_fault_alloc=25.0 2048kB/split_deferred=5.0 64kB/anon_fault_alloc=24.0 thp_fault_alloc=25.0 thp_split_pmd=5.0 thp_deferred_split_page=5.0 thp_collapse_alloc=1.0 | 41.0 | 58.5 |
| always | valkey1 | never | 64kB/anon_fault_alloc=1.0 | 6.220 | 584.5 |
| always | valkey1 | always | 2048kB/anon_fault_alloc=318.0 2048kB/split_deferred=281.0 64kB/anon_fault_alloc=28.0 thp_fault_alloc=318.0 thp_split_pmd=281.0 thp_deferred_split_page=281.0 thp_collapse_alloc=1.0 | 7.164 | 640.7 |

(The `ab` valkey1 scores are the 10 s model with 2 rounds, 6.2..7.5 M ops/s, not the `quick`
5 s figure; two rounds are a wiring check, the throughput deltas here are not readings.)

`make quick CHALLOC_THP_COUNTERS=1 QUICK_BENCHES='larson1 valkey1'` in the auto state (old =
new = HEAD, no key): `quick: go`, and the section reads `none` for every row and variant but
one stray `64kB/anon_fault_alloc=1` on `valkey1 new`: the default never advises, and one folio
in ten seconds is the host's own noise.

## 3. Reading, next to task 9's dTLB column

- The dTLB figures reproduce lane B's within one run's noise (auto state: larson1 3.98 -> 3.05,
  valkey1 2.82 -> 2.45 under `thp:auto`; lane B 4.01 -> 3.05 and 2.81 -> 2.44). What the
  counters add is the folio count behind each figure:
  - `larson1` under `thp:auto` in the auto state: 71..76 folios of 64 KiB, 4.4..4.75 MiB, for
    a working set of a few MiB; that is the +2.2 MiB RSS lane B measured (15.9 -> 18.1 MiB
    here too) and the quarter of the dTLB refills it saves.
  - `valkey1` under `thp:auto`: 2788 folios = 174 MiB of a 585 MiB heap. Only the classes whose
    slab holds a 64 KiB folio are advised (the 16-, 64- and 256-byte classes have 64 KiB slabs;
    the 1024-byte class has a 4 KiB slab, `tests/c/pagemap_guard.c`'s geometry line), so a
    Valkey-shaped heap of 16..1024-byte values takes 64 KiB folios for under a third of its
    bytes; the remaining dTLB refills (2.45 against jemalloc's 2.74) are in the page-sized slabs
    and the metadata. Under `thp:always` in the same state (no PMD available) the count is
    3306..3308 (206 MiB): the always path advises the metadata and huge runs as well.
  - `valkey1` under `thp:always` with the PMD available: 318 PMD faults = 636 MiB, the whole
    heap in 2 MiB pages (dTLB 0.66..1.07), and 280..282 `thp_split_pmd` in the same run: the
    delete phase's purges (`MADV_DONTNEED` of 64 KiB slabs) split 88% of those pages before the
    run ends, with `split_deferred` counting the same pages. That is the RSS price (640.7 MiB
    against 584.5) and the mechanism lane B named ("a purge inside a 2 MiB page splits it");
    the counters now give the split fraction per row. larson1: 25 PMD faults, 5 splits.
  - `thp_collapse_alloc=1` on every run whose `thp` is not `never`: the `MADV_COLLAPSE` probe
    (`features.rs`, lane B section 1: probed only when `thp` is not `never`, costs one PMD for a
    moment). One 2 MiB collapse per process start, visible in the counters, absent under the
    default.
- In the default state the `thp:auto` rows show no folio at all (only the probe's collapse):
  the counters confirm `auto` is inert on Ubuntu's stock sysfs, which is the "documented
  `thp:always`" branch of task 9's acceptance; the host requirement (`hugepages-64kB=madvise`)
  is what turns the column on.
- What the counters cannot say: which class got the folios (they are per size, not per
  region); `/proc/<pid>/smaps` `AnonHugePages` per VMA, as `tests/c/thp.c` reads it, is the
  per-region form, and the two together make the attribution.
