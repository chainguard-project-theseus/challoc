# challoc flush sort (Phase 2.5 task 1b, mechanism M2), 2026-09-19, aarch64 (GCE C4A)

Plan Phase 2.5 task 1b (`docs/plan.md`): replace the `sort_unstable` over
the whole slot array in `Heap::flush` (attributed as M2 in
`2026-09-19-aarch64-gce-four-way.md` section 2: 9.2% of glibc-simple,
15.6% of Python dict_list-malloc, 4.0% of mstress) with the cheapest form
that keeps the flush's two properties: the released half is the highest
addresses, and the release is grouped per slab. Base 1339eb6 (the library
sources are unchanged from there to 6ac6ae6, which is `old` below).

- host: challoc-bench-arm64 (c4a-standard-16, Google Axion / Neoverse V2),
  us-east4-a, kernel 7.0.0-1011-gcp, started and stopped by this iteration
  (CHALLOC_GCE_WARM unset). Load under 0.3 at the start of every run unless
  noted.
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
| glibc-simple | old | - | 107.5 | 586.6 | 0.016 | 0.010 |
| python1-malloc | old | - | 12.76 G | 57.45 G | 95.6 M | 106.0 M |
| larson1 | old | 42.12 | 71.2 | 249.8 | 2.105 | 3.941 |

`perf record` shares (of the whole process):

| row | libchalloc | `free` | `malloc` | quicksort | ipnsort | median3_rec | sort total | release_owned |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| glibc-simple (9K samples, 6.92 G cycles) | 46.11% | 17.77% | 17.11% | 8.24% | 0.31% | 0.26% | **8.81%** | 1.32% |
| python1-malloc (18K samples, 12.82 G cycles) | 18.76% | 5.86% | 5.87% | 4.91% | - | - | **5.26%** | - |
| xmalloc (326K samples, 151.8 G cycles, 10 threads) | 84.58% | 12.45% + free_other 22.04% + free_slow 15.25% | 9.94% | | | | **~3.6%** (12.41% incl. release_owned 8.82%) | 8.82% |

The xmalloc row is the one to remember: its consumer caches are pushed in
ascending address order (a producer's refill batch, handed over in order),
so the old sort ran on already-sorted input and took ipnsort's O(n) path.
Any replacement that costs more than O(n) with a small constant on sorted
input loses there.

## 2. Candidates (`make quick OLD=HEAD` with the extended row set, one round each, arm64)

(a) `select_nth_unstable` at the split, then `sort_unstable` of the released
half (the plan's first candidate): the selection scrambles both halves, so
the released half is a full sort of 512 and the kept half loses its order.

(b) `select_nth_unstable` at the split, then a counting pass by slab
number over the released half (American flag sort on `slab - lowest slab`
when the half spans at most 256 slabs, nothing when it is one slab, a
sort beyond that); `release_owned` accumulates each slab's bits in a
scratch bitmap so the order inside a slab is free.

| row | unit | old | (a) new | (a) vs old | old | (b) new | (b) vs old |
|---|---|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.4 | 42.0 | +1.0% | 42.4 | 42.3 | +0.2% |
| larson10 | M ops/s | 358.4 | 364.7 | -1.7% | 365.1 | 369.1 | -1.1% |
| xmalloc | M frees/s | 612.3 | 405.9 (median of 3) | **+50.9%** | 613.6 | 505.8 (median of 3) | **+21.3%** |
| alloc-test1 | s | 2.589 | 2.570 | -0.7% | 2.574 | 2.584 | +0.4% |
| valkey1 | M ops/s | 7.428 | 7.461 | -0.4% | 7.144 | 7.378 | -3.2% |
| glibc-simple | s | 2.288 | 2.278 | -0.4% | 2.302 | 2.229 | -3.2% |
| python1-malloc | s | 4.249 | 4.178 | -1.7% | 4.251 | 4.136 | -2.7% |
| churn 1024, 8 s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | compacts | same | same | compacts |

Both NO-GO on xmalloc. Counters and shares:

| row | (a) cycles/op or G | (a) instr | (b) cycles/op or G | (b) instr | (a) flush-side share | (b) flush-side share |
|---|---:|---:|---:|---:|---:|---:|
| glibc-simple | 107.2 (old 107.2) | 577.0 | 104.3 (old 107.8) | 562.3 | quicksort 3.66% + flush 2.73% + median3 0.37% = 6.79% | flush 3.17% + median3 0.26% = 3.43% |
| python1-malloc | 12.53 G (old 12.73) | 56.73 G | 12.27 G (old 12.76) | 56.02 G | 3.81% | 2.02% |
| larson1 | 71.3 (old 71.3) | 249.9 | 71.2 (old 71.5) | 249.2 | | |

Two findings decided the final form. (1) xmalloc under (b): `flush` 13.01%
of the run (the selection) where the old sort was about 3.6%, `release_owned`
10.0% vs 8.82%, 393 vs 448 M frees/s in the `perf record` run: a
selection is not O(n)-cheap on sorted input, and it destroys the order the
next flush would have benefited from. (2) glibc-simple under (b):
libchalloc fell from 46.1% to 40.4% but the program's own `do_benchmark`
rose from 52.24% to 57.56% of a total that shrank only 2.9% (3.61 -> 3.87 G
cycles, +7%): the kept half used to be ascending, so pops after a flush
walked down through memory; a selection leaves it in partition order and
the benchmark's per-block writes lose that locality (L1D misses per
operation 0.017 -> 0.025, tiny in absolute terms; the effect is not seen on
x86_64, where the same variant gained 5.7%). Hence (c).

(c) a bucket pass over the whole array by address (American flag sort on
`(p - min) >> shift` with the width chosen to fit 256 buckets), the exact
selection only inside the boundary bucket; the released half is grouped
by slab when a bucket is no wider than a slab. Expected to be O(n) with a
near-sorted kept half; measured, the in-place bucket pass costs about 10
cycles per entry (a serial chain of key, cursor load, swap), the same as
the old sort on random input and ten times its cost on sorted input.

(g) = (b) plus an `is_sorted` fast path: a slot array already in address
order is split directly (the old sort's O(n) case) and its kept half stays
sorted. (i) = (g) plus an unconditional `sort_unstable` of the kept half in
the selection path.

| row | unit | (c) old / new / delta | (g) old / new / delta | (i) old / new / delta |
|---|---|---|---|---|
| larson1 | M ops/s | 42.4 / 42.4 / +0.0% | 42.4 / 42.5 / -0.2% | 42.3 / 42.0 / +0.7% |
| larson10 | M ops/s | 368.1 / 365.8 / +0.6% | 367.2 / 364.0 / +0.9% | 362.9 / 364.5 / -0.4% |
| xmalloc | M frees/s | 614.4 / 411.1 / **+49.4%** | 613.8 / 527.8 / **+16.3%** | 611.4 / 595.7 / +2.7% |
| alloc-test1 | s | 2.560 / 2.553 / -0.3% | 2.572 / 2.569 / -0.1% | 2.560 / 2.565 / +0.2% |
| valkey1 | M ops/s | 7.390 / 7.333 / +0.8% | 7.528 / 7.526 / +0.0% | 7.204 / 7.276 / -1.0% |
| glibc-simple | s | 2.305 / 2.341 / +1.6% | 2.288 / 2.301 / +0.6% | 2.314 / 2.423 / +4.7% |
| python1-malloc | s | 4.279 / 4.186 / -2.2% | 4.277 / 4.121 / -3.6% | 4.287 / 4.238 / -1.1% |
| churn 1024, 8 s | | compacts (97.0%) | compacts (97.0%) | compacts (97.0%) |
| verdict | | NO-GO | NO-GO | go, but no gain |

Shares: (c) glibc-simple `flush` 4.22%, xmalloc `flush` 23.63% (11k cycles
per flush against the old sort's 1.25k there); (g) glibc-simple `flush`
3.31%, xmalloc `flush` 10.13%; (i) glibc-simple quicksort 3.26% (the kept
half) + `flush` 3.24%, xmalloc `flush` 1.90%. Counters, glibc-simple
cycles/op old -> new: (c) 107.8 -> 109.5, (g) 106.8 -> 108.7, (i) 108.3 ->
113.3; python1-malloc G cycles: (c) 12.78 -> 12.43, (g) 12.69 -> 12.26, (i)
12.75 -> 12.71.

What the xmalloc column says: under (g) one unsorted flush (the cache's
first fill, or a producer's refill moving to a lower slab) scrambles the
kept half and every later flush of that cache takes the selection, where
the old sort re-established the order every time (self-healing); (i) heals
but pays a kept-half sort on every flush of glibc-simple and Python, which
is most of what the change saved. Hence (j), the landed form: (g) plus a
kept-half sort only on the first two consecutive unsorted flushes of a
cache and every 32nd after that (a one-byte counter in the cache header),
so a cache pushed in order heals after one bad flush and a cache pushed in
arbitrary order pays two sorts and then one in 32.

The `do_benchmark` share of glibc-simple on this VM (52.2% before; 53.6%
(a), 57.6% (b), 59.2% (c), 61.1% (g), 58.0% (i), single `perf record` runs)
grew under every variant that groups the released half instead of sorting
it, including (i) whose kept half is sorted, so it is not pop order; see
section 3 for repeated runs.

## 3. After: (j), the landed form (commit 3ed77b7 = the working tree of these runs)

`make quick OLD=HEAD` (HEAD = 6ac6ae6 sources = 1339eb6), extended row set, one round:

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.1 (15.8 MiB) | 42.2 (15.9 MiB) | | -0.2% | |
| larson10 | M ops/s | 360.0 (113.0 MiB) | 357.6 (114.3 MiB) | | +0.7% | |
| xmalloc | M frees/s | 613.707 (55.7 MiB) | 598.373 (57.2 MiB) | | +2.6% | |
| alloc-test1 | s | 2.666 (13.0 MiB) | 2.680 (13.0 MiB) | | +0.5% | |
| valkey1 | M ops/s | 6.838 (584.1 MiB) | 6.748 (584.1 MiB) | | +1.3% | |
| glibc-simple | s | 2.293 (2.2 MiB) | 2.199 (2.2 MiB) | | -4.1% | |
| python1-malloc | s | 4.310 (75.5 MiB) | 4.175 (75.5 MiB) | | -3.1% | |
| churn 1024, 8 s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

quick: go (new within 5% of old on every row; churn compacts at 97.0%).

Per-operation counters (`quick-perf`, same run):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| glibc-simple | old | - | 107.2 | 586.6 | 0.016 | 0.010 |
| glibc-simple | new | - | 102.3 (-4.6%) | 562.8 (-4.1%) | 0.022 | 0.010 |
| python1-malloc | old | - | 12.64 G | 57.41 G | 95.6 M | 109.3 M |
| python1-malloc | new | - | 12.30 G (-2.7%) | 55.93 G (-2.6%) | 95.3 M | 105.2 M |
| larson1 | old | 41.91 | 71.6 | 249.8 | 2.095 | 4.055 |
| larson1 | new | 42.01 | 71.3 | 249.4 | 2.089 | 4.048 |

`perf record` shares, two runs of each library for glibc-simple:

| row | variant | cycles | libchalloc | `free` | `malloc` | flush + sort | of which quicksort | release_owned | do_benchmark |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| glibc-simple | old, run 1 | 6.90 G | 45.87% | 17.48% | 17.27% | **8.88%** | 8.41% | 1.20% | 52.15% |
| glibc-simple | old, run 2 | 6.93 G | 45.46% | 16.94% | 17.02% | **9.07%** | 8.39% | 1.36% | 52.49% |
| glibc-simple | new, run 1 | 6.98 G | 45.78% | 22.31% | 16.90% | **3.54%** | 0.13% | 1.67% | 52.47% |
| glibc-simple | new, run 2 | 6.59 G | 42.91% | 18.56% | 17.84% | **3.97%** | 0.15% | 1.65% | 55.29% |
| python1-malloc | new | 12.40 G | 17.49% | 6.20% | 7.06% | **1.98%** (before 5.26%) | - | 1.03% | |
| xmalloc | new | 153.3 G | 84.90% | 12.46% | 9.85% | **1.95%** (before about 3.6%) | - | 10.89% (before 8.82%) | |

The term removed: the sort's 8.9% of glibc-simple becomes `flush` 3.3..3.5%
(the selection and the bucket pass; `select_nth_unstable` and
`group_by_slab` are inlined) plus 0.13% of quicksort (the kept-half sorts
on the first two unsorted flushes of a cache and every 32nd after), a
5.2-point saving of which 4.6% shows in cycles per operation. On
python1-malloc 5.3% becomes 2.0%. xmalloc's flush is on the sorted path
(1.95%, below the old sort's share there); its `release_owned` grew by two
points, the scratch-bitmap form's per-pointer work on a 10-thread run
whose flushes are mostly of other arenas' slots. glibc-simple's
`do_benchmark` at 52.15 / 52.49 (old) vs 52.47 / 55.29 (new) puts the
single-run swings of section 2 (57..61%) mostly down to noise.

## 4. `ab`, five rounds, same run: old = 1339eb6, new = 3ed77b7

`make ab VARIANTS='old=1339eb6 new=HEAD' BENCHES='glibc-simple python1-malloc larson1 larson10 mstress' ROUNDS=5`
(`2026-09-19-aarch64-gce-flush-sort-ab.md` has the full output; load 0.3 at the start, both libraries built in the run).

| program | unit | old median (sorted scores) | old RSS MiB | new median (sorted scores) | new RSS MiB | new vs old |
|---|---|---:|---:|---:|---:|---:|
| glibc-simple | s | 2.298 (2.286 2.288 2.298 2.302 2.314) | 2.3 | 2.189 (2.185 2.188 2.189 2.207 2.209) | 2.3 | **-4.7%** |
| python1-malloc | s | 4.260 (4.253 4.260 4.260 4.261 4.278) | 75.5 | 4.131 (4.117 4.122 4.131 4.134 4.139) | 75.5 | **-3.0%** |
| larson1 | M ops/s | 42.4 (42.4 x5) | 15.6 | 42.4 (42.3 42.3 42.4 42.4 42.4) | 15.9 | +0.0% |
| larson10 | M ops/s | 364.7 (361.0 362.7 364.7 365.8 368.0) | 112.4 | 365.7 (362.2 363.7 365.7 365.8 366.1) | 111.6 | -0.3% |
| mstress | s | 0.380 (0.379 0.379 0.380 0.380 0.381) | 320.8 | 0.372 (0.370 0.372 0.372 0.373 0.374) | 320.3 | -2.1% |

The sorted lists of glibc-simple, python1-malloc and mstress do not overlap. Acceptance: glibc-simple needs at least half of the 8.81% share, 4.4%: -4.7%, met; python1-malloc needs half of 5.26%, 2.6%: -3.0%, met.

## 5. Tests

- Host: `cargo test --features std`: 248 passed (new: `group_by_slab_groups_a_scrambled_batch`, `group_by_slab_single_slab_and_wide_span`, `flag_sort_permutes_into_ascending_buckets`, `release_takes_slab_groups_in_any_order`, `duplicate_apart_in_one_slab_group_aborts`; `flush_releases_the_highest_addresses_and_keeps_the_lowest` now checks the kept half's content, not its order).
- Mac container `make test` (3ed77b7): `test result: ok. 315 passed`, `ok. 105 passed`, `ok. 23 passed`, exit 0.
- This VM `make test` (3ed77b7, in the chain after `ab`): `test result: ok. 315 passed; 0 failed`, then the api set with the negative modes; `c_negative_double_free_straddle ... ok` (`negative double_free_straddle: challoc: double free of 0x... (class 3): both copies cached, one at the flush boundary [src/heap.rs:2545]`) and `c_negative_double_free_cached ... ok` (the back-to-back pair now lands on the same boundary compare, `both copies cached, one at the flush boundary`; with `double_free_scan:true` it aborts at the call as before, `the slot is already in the thread cache`), exit 0.
- calloc-zero campaign: run on the x86_64 VM (see that file).

## 6. dict_list-malloc, the scorecard and the acceptance reading

The plan names `python dict_list-malloc` (share 15.6% on this VM in the
four-way run). It is a `bench/python-bench.sh` row, not an `ab` row, so two
runs of `make python-bench WORKLOADS=dict_list MODES=malloc ALLOCS=challoc
ROUNDS=5`, one at `REV=1339eb6` and one at `REV=HEAD` (3ed77b7), back to
back on the idle VM (not the same run):

| row | old median (sorted) | new median (sorted) | new vs old |
|---|---:|---:|---:|
| dict_list-malloc | 4.714 s (4.689 4.708 4.714 4.719 4.738 4.749) | 4.399 s (4.394 4.394 4.399 4.411 4.418 4.423) | **-6.7%** |

Scorecard line (3 rounds) appended to `scorecard-aarch64.md`:

`| 2026-09-19 | 3ed77b7 | +6.6% | +3.9% | +3.2% | -20.9% | +4.7% | +3.2% | -0.0% | -1.5% | +0.7% | -2.5% | -42.1% | -13.3% | -18.0% | -0.8% | -1.2% | 0.86 | 1.00 | - | - | - | - | ... |`
(glibc-simple column -8.9% -> -13.3% against jemalloc, python1-malloc +0.8% -> -1.2%; the rest within its history).

Acceptance (half of the sort's measured share, same-run `ab`): glibc-simple
needs 4.4%, reads -4.7% (met, lists disjoint); python1-malloc (the `ab`
row) needs 2.6%, reads -3.0% (met); dict_list-malloc needs 7.8% and reads
-6.7% in the two-run comparison, 1.1 points short. What remains of the term
is the selection itself: `flush` is 3.3..3.5% of glibc-simple after the
change (section 3), and every O(n) form tried that avoids it (section 2)
cost more than it saved.
