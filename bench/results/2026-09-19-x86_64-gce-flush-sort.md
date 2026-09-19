# challoc flush sort (Phase 2.5 task 1b, mechanism M2), 2026-09-19, x86_64 (GCE C4)

Plan Phase 2.5 task 1b (`docs/plan.md`): replace the `sort_unstable` over
the whole slot array in `Heap::flush` (attributed as M2 in
`2026-09-19-x86_64-gce-four-way.md` section 2: 9.2% of glibc-simple,
15.6% of Python dict_list-malloc, 4.0% of mstress) with the cheapest form
that keeps the flush's two properties: the released half is the highest
addresses, and the release is grouped per slab. Base 1339eb6 (the library
sources are unchanged from there to 6ac6ae6, which is `old` below).

- host: challoc-bench-amd64 (c4-standard-16, Intel Xeon 6, Granite Rapids),
  us-east4-a, kernel 7.0.0-1011-gcp, started and stopped by this iteration
  (CHALLOC_GCE_WARM unset). Load under 0.3 at the start of every run unless
  noted (the candidate (b) run followed the calloc-zero campaign, load 3.95).
- tools: `bench/gce/quick-perf.sh` (per-operation counters; this iteration
  gave glibc-simple an operation count, 64M malloc+free, and made rows
  without one print whole-process totals) and the new
  `bench/gce/perf-record.sh` (`perf record -F 4000` attached to the
  container's process, symbols through `perf report --symfs` over the DSOs
  copied out of the container; the "sort share" is the sum of the
  libchalloc symbols matching `sort|median3|partition|pivot` plus, after the
  change, `select|flag_sort|group_by_slab|split_and_group|flush`, as a
  percentage of every sample of the process). Raw files under the VM's
  `~/logs/flush-sort/` and `~/logs/perf-record/`.

## 1. Before (old = 6ac6ae6 = 1339eb6 sources)

Per-operation counters (`quick-perf`, one run each):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| glibc-simple | old | - | 121.1 | 599.0 | 0.020 | 0.000 |
| python1-malloc | old | - | 16.20 G | 61.80 G | 69.7 M | 1.3 M |
| larson1 | old | 52.64 | 72.4 | 234.7 | 1.568 | 0.002 |

`perf record` shares (of the whole process):

| row | libchalloc | `free` | `malloc` | quicksort | ipnsort | median3_rec | sort total | release_owned |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| glibc-simple (7K samples, 7.67 G cycles) | 50.76% | 21.07% | 17.78% | 7.63% | 0.40% | 0.41% | **8.44%** | 2.45% |
| python1-malloc (16K samples, 16.18 G cycles) | 20.14% | 7.14% | 6.38% | 4.29% | - | - | **4.68%** | 0.95% |

## 2. Candidate (b) on this VM (`make quick OLD=HEAD`, extended row set, one round)

The candidates are described in the aarch64 file; (a) was measured only on
arm64 (xmalloc +50.9%, NO-GO). (b), selection plus a slab bucket pass over
the released half:

| row | unit | old | (b) new | (b) vs old |
|---|---|---:|---:|---:|
| larson1 | M ops/s | 49.0 | 50.9 | -3.7% |
| larson10 | M ops/s | 300.3 | 320.9 | -6.4% |
| xmalloc | M frees/s | 376.5 | 344.1 (median of 3) | **+9.4%** |
| alloc-test1 | s | 2.859 | 2.875 | +0.6% |
| valkey1 | M ops/s | 16.068 | 15.865 | +1.3% |
| glibc-simple | s | 1.975 | 1.863 | -5.7% |
| python1-malloc | s | 4.122 | 4.008 | -2.8% |
| churn 1024, 8 s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | compacts |

NO-GO on xmalloc here too. Counters: glibc-simple 112.8 cycles/op (old
119.8), 569.2 instr/op (old 599.0); python1-malloc 15.60 G cycles (old
16.12), 59.95 G instructions (old 61.80); larson1 72.5 / 234.0 (old 72.5 /
234.6). Shares: glibc-simple flush 4.08% + release_owned 3.30% (sort total
4.37%); python1-malloc flush 1.72% + release_owned 1.35% (1.80%). Unlike
aarch64, `do_benchmark` did not grow (45.99% -> 46.98% of a total 6%
smaller).

The later candidates on this VM (described in the aarch64 file; one round each):

| row | unit | (c) old / new / delta | (g) old / new / delta | (i) old / new / delta |
|---|---|---|---|---|
| larson1 | M ops/s | 50.1 / 49.8 / +0.6% | 51.6 / 50.7 / +1.8% | 51.1 / 51.0 / +0.2% |
| larson10 | M ops/s | 308.2 / 299.7 / +2.8% | 298.8 / 317.9 / -6.0% | 307.4 / 311.7 / -1.4% |
| xmalloc | M frees/s | 374.6 / 280.8 / **+33.4%** | 380.7 / 335.4 / **+13.5%** | 365.9 / 364.7 / +0.3% |
| alloc-test1 | s | 2.857 / 2.937 / +2.8% | 2.858 / 2.860 / +0.1% | 2.860 / 2.888 / +1.0% |
| valkey1 | M ops/s | 16.034 / 15.794 / +1.5% | 15.771 / 15.922 / -0.9% | 15.612 / 15.728 / -0.7% |
| glibc-simple | s | 1.992 / 1.873 / -6.0% | 1.975 / 1.863 / -5.7% | 1.970 / 1.905 / -3.3% |
| python1-malloc | s | 4.152 / 4.100 / -1.3% | 4.129 / 4.001 / -3.1% | 4.145 / 4.145 / +0.0% |
| churn 1024, 8 s | | compacts (97.0%) | compacts (97.0%) | compacts (97.0%) |
| verdict | | NO-GO | NO-GO | go, but no gain |

Shares: (c) glibc-simple `flush` 5.60%, xmalloc `flush` 20.57%; (g)
glibc-simple `flush` 3.40%, xmalloc `flush` 9.79%; (i) glibc-simple
quicksort 3.59% + `flush` 3.36%, xmalloc `flush` 3.44%. Counters,
glibc-simple cycles/op old -> new: (c) 122.5 -> 113.5, (g) 120.1 -> 115.4,
(i) 120.7 -> 115.4; python1-malloc G cycles: (c) 16.05 -> 15.90, (g) 15.97
-> 15.51, (i) 16.00 -> 16.04. `do_benchmark` stayed at 46..48% under every
variant here.

## 3. After: (j), the landed form (commit 3ed77b7 = the working tree of these runs)

`make quick OLD=HEAD` (HEAD = 6ac6ae6 sources = 1339eb6), extended row set, one round:

| row | unit | old | new | new vs old |
|---|---|---:|---:|---:|
| larson1 | M ops/s | 50.4 (17.3 MiB) | 50.5 (17.2 MiB) | -0.2% |
| larson10 | M ops/s | 300.8 (96.3 MiB) | 314.5 (100.7 MiB) | -4.4% |
| xmalloc | M frees/s | 390.858 (53.0 MiB) | 390.817 (53.3 MiB) | +0.0% |
| alloc-test1 | s | 2.890 (13.5 MiB) | 2.887 (13.7 MiB) | -0.1% |
| valkey1 | M ops/s | 15.841 (585.2 MiB) | 15.920 (585.2 MiB) | -0.5% |
| glibc-simple | s | 2.005 (2.5 MiB) | 1.833 (2.5 MiB) | -8.6% |
| python1-malloc | s | 4.160 (76.3 MiB) | 4.001 (76.3 MiB) | -3.8% |
| churn 1024, 8 s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3461 (97.0%) | new compacts |

quick: go (new within 5% of old on every row; churn compacts at 97.0%).

Per-operation counters (`quick-perf`, same run):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| glibc-simple | old | - | 120.9 | 599.0 | 0.021 | 0.000 |
| glibc-simple | new | - | 110.4 (-8.7%) | 569.9 (-4.9%) | 0.016 | 0.000 |
| python1-malloc | old | - | 15.99 G | 61.75 G | 67.7 M | 1.2 M |
| python1-malloc | new | - | 15.51 G (-3.0%) | 59.94 G (-2.9%) | 70.1 M | 1.2 M |
| larson1 | old | 50.14 | 73.3 | 235.1 | 1.583 | 0.002 |
| larson1 | new | 50.72 | 72.4 | 234.3 | 1.574 | 0.002 |

`perf record` shares, two runs of each library for glibc-simple:

| row | variant | cycles | libchalloc | `free` | `malloc` | flush + sort | of which quicksort | release_owned | do_benchmark |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| glibc-simple | old, run 1 | 7.65 G | 50.55% | 20.74% | 17.76% | **9.00%** | 8.03% | 1.97% | 46.19% |
| glibc-simple | old, run 2 | 7.69 G | 49.63% | 19.87% | 18.06% | **8.44%** | 7.73% | 1.97% | 47.00% |
| glibc-simple | new, run 1 | 7.12 G | 46.87% | 21.29% | 17.49% | **3.67%** | 0.13% | 3.35% | 49.87% |
| glibc-simple | new, run 2 | 7.12 G | 46.18% | 21.59% | 16.56% | **3.68%** | 0.06% | 3.13% | 50.39% |
| python1-malloc | new | 15.66 G | 17.91% | 6.83% | 6.66% | **1.90%** (before 4.68%) | - | 1.53% (before 0.95%) | |
| xmalloc | new | 230.9 G | 85.79% | 14.48% | 11.51% | **3.44%** | - | 16.16% | |

The term removed: the sort's 8.4..9.0% of glibc-simple becomes `flush`
3.4% plus 0.1% of kept-half sorts, a 5.3-point saving of which 8.7% shows
in cycles per operation (the whole run is 7% shorter); `release_owned`
grew from 2.0% to 3.2% of a shorter run. On python1-malloc 4.7% becomes
1.9%.

## 4. `ab`, five rounds, same run: old = 1339eb6, new = 3ed77b7

`make ab VARIANTS='old=1339eb6 new=HEAD' BENCHES='glibc-simple python1-malloc larson1 larson10 mstress' ROUNDS=5`
(`2026-09-19-x86_64-gce-flush-sort-ab.md` has the full output; load 0.3 at the start, both libraries built in the run).

| program | unit | old median (sorted scores) | old RSS MiB | new median (sorted scores) | new RSS MiB | new vs old |
|---|---|---:|---:|---:|---:|---:|
| glibc-simple | s | 2.092 (2.036 2.059 2.092 2.122 2.123) | 2.6 | 1.904 (1.887 1.890 1.904 1.905 1.916) | 2.5 | **-9.0%** |
| python1-malloc | s | 4.242 (4.231 4.239 4.242 4.427 4.483) | 76.4 | 4.155 (4.144 4.151 4.155 4.159 4.230) | 76.3 | **-2.1%** |
| larson1 | M ops/s | 47.9 (46.0 46.1 47.9 48.4 48.6) | 17.2 | 47.9 (46.1 46.6 47.9 48.0 48.8) | 17.0 | +0.0% |
| larson10 | M ops/s | 289.9 (282.8 283.3 289.9 290.3 298.4) | 96.5 | 288.0 (282.6 286.1 288.0 291.4 292.8) | 96.5 | +0.7% |
| mstress | s | 0.619 (0.611 0.611 0.619 0.632 0.638) | 334.9 | 0.608 (0.602 0.603 0.608 0.620 0.622) | 343.5 | -1.8% |

The sorted lists of glibc-simple do not overlap; python1-malloc's overlap at one value (old 4.231..4.483 with two high outliers, new 4.144..4.230). Acceptance: glibc-simple needs at least half of the 8.44% share, 4.2%: -9.0%, met; python1-malloc needs half of 4.68%, 2.3%: the median reads -2.1% (the means -3.6%), 0.2 points short on the median; see section 6 for the dict_list-malloc row the plan names.

## 5. Tests

- Host: `cargo test --features std`: 248 passed (new: `group_by_slab_groups_a_scrambled_batch`, `group_by_slab_single_slab_and_wide_span`, `flag_sort_permutes_into_ascending_buckets`, `release_takes_slab_groups_in_any_order`, `duplicate_apart_in_one_slab_group_aborts`; `flush_releases_the_highest_addresses_and_keeps_the_lowest` now checks the kept half's content, not its order).
- Mac container `make test` (3ed77b7): `test result: ok. 315 passed`, `ok. 105 passed`, `ok. 23 passed`, exit 0.
- This VM `make test` (3ed77b7, in the chain after `ab`): `test result: ok. 315 passed; 0 failed`, then the api set with the negative modes; `c_negative_double_free_straddle ... ok` (`negative double_free_straddle: challoc: double free of 0x... (class 3): both copies cached, one at the flush boundary [src/heap.rs:2545]`) and `c_negative_double_free_cached ... ok` (the back-to-back pair now lands on the same boundary compare, `both copies cached, one at the flush boundary`; with `double_free_scan:true` it aborts at the call as before, `the slot is already in the thread cache`), exit 0.
- calloc-zero campaign, default configuration, on this VM at 3ed77b7 (`ROUNDS=200 CONFIGS=default:check_zero:true SKIP_SUITE=1`, after `make test` had built the tree): `RESULT default: 200 ok, 0 failed, 200 runs of stress 8 50 10, 84 s, conf=check_zero:true`; `RESULT load: 5 runs of cargo test api::, 0 failed`; `RESULT standalone: 20 ok, 0 failed, 20 runs of stress 16 100 20, 15 s`. An earlier run of the same campaign on the candidate (b) tree (same `release_owned`, the flush before the sorted path and the kept sort) was clean too: 200 ok, suite 315 + 105 passed.

## 6. dict_list-malloc, the scorecard and the acceptance reading

Two runs of `make python-bench WORKLOADS=dict_list MODES=malloc
ALLOCS=challoc ROUNDS=5`, `REV=1339eb6` then `REV=HEAD` (3ed77b7), back to
back on the idle VM (not the same run; the four-way share of the sort on
this row here was 13.9% + 1.4% = 15.3%):

| row | old median (sorted) | new median (sorted) | new vs old |
|---|---:|---:|---:|
| dict_list-malloc | 4.390 s (4.363 4.384 4.390 4.390 4.412 4.454) | 4.076 s (4.067 4.071 4.076 4.088 4.091 4.119) | **-7.2%** |

Scorecard line (3 rounds) appended to `scorecard-x86_64.md`:

`| 2026-09-19 | 3ed77b7 | +0.4% | +1.7% | +7.4% | -2.6% | +7.3% | +8.8% | +3.9% | -13.8% | +1.1% | -1.5% | -41.9% | -0.4% | -19.6% | -0.8% | -0.7% | 1.05 | 0.99 | - | - | - | - | ... |`
(glibc-simple column against jemalloc at -0.4%, from +6.5..+7.5% in its history; python1-malloc -0.7%).

Acceptance (half of the sort's measured share, same-run `ab`): glibc-simple
needs 4.2%, reads -9.0% (met, lists disjoint); python1-malloc (the `ab`
row) needs 2.3%, reads -2.1% on the median (0.2 short; the means give
-3.6%, the old list carrying two high outliers); dict_list-malloc needs
7.7% and reads -7.2% in the two-run comparison, 0.5 points short.
