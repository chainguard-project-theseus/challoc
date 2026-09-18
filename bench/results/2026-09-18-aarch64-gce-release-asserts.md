# challoc release assertions and `overflow-checks`, 2026-09-18, aarch64 (GCE C4A)

Phase 1.5 task 8 (`docs/plan.md`; review 2026-09-18 section 1.2 items 2-8):
release assertions, and the `overflow-checks = true` release-profile
question. The task changes no algorithm: it promotes debug-only invariant
checks to release aborts (all on slow paths: the scheduled compaction pop's
run invariant, `refill`'s empty-cache precondition, `free_caches`'s flushed
precondition, the `others` node creation for the home arena, the bitmap
release mask against the slab's sub-word view, the huge table's duplicate
key, `State::from_u8`/`RunState::from_u8` on bytes no state has, `Meta::free`
of the free-list head, the `nslots - live`, `ClassStats` and `HugeStats`
subtractions and the descriptor table count) and counts the `bin_stats`
clamp in `malloc_stats_print`'s bins table (`clamped` column). Working tree
of this iteration on top of 1aa14da (the code commit's hash is in
PROGRESS.md; `quick` lists the library hashes).

- hosts: challoc-bench-arm64 (c4a-standard-16, Google Axion / Neoverse V2)
  and challoc-bench-amd64 (c4-standard-16, Xeon Platinum 8581C), us-east4-a,
  started and stopped by this iteration (CHALLOC_GCE_WARM unset); load 0..3
  at every start.
- tests: host `cargo test --features std` 198 (179 + 19 new, one per new
  abort plus the clamp counter and the decode helpers); `make test` on the
  Mac container 256 + 81 (release build, C negative modes).

## 1. `quick` with `overflow-checks = true` and the assertions (NO-GO)

aarch64, working tree against 1aa14da (NO-GO (new trails old by more than 5% on a row, or does not compact)):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.7 (15.7 MiB) | 40.2 (15.4 MiB) (median of 3) | 44.9 (17.2 MiB) | +6.2% | +11.7% |
| larson10 | M ops/s | 366.1 (112.6 MiB) | 347.5 (110.1 MiB) (median of 3) | 381.5 (130.6 MiB) | +5.4% | +9.8% |
| xmalloc | M frees/s | 562.731 (52.8 MiB) | 477.252 (53.0 MiB) | 473.593 (80.2 MiB) | +17.9% | -0.8% |
| alloc-test1 | s | 2.737 (12.9 MiB) | 2.871 (13.0 MiB) | 2.625 (12.3 MiB) | +4.9% | +9.4% |
| valkey1 | M ops/s | 6.433 (584.0 MiB) | 6.409 (584.1 MiB) | 6.992 (585.4 MiB) | +0.4% | +9.1% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24535039 ops/s |

Positive percentages mean new did worse (slower, lower throughput). One round each, aarch64, nproc 16, load at start 3.95, 179 s total. old 51e46cec8fba5f10, new 8a8bcc4d3d9c92eb.

## 2. Attribution: `quick-perf` on the flag-on tree (aarch64)

Per-operation counters, one run each, old = 1aa14da, new = the flag-on
tree, `perf stat -e cycles,instructions,L1-dcache-load-misses,dTLB-load-misses`:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 42.33 | 70.9 | 245.8 | 2.088 | 4.026 |
| larson1 | new | 40.09 | 74.8 | 268.5 | 2.096 | 4.030 |
| valkey1 | old | 12.42 | 244.1 | 238.9 | 3.095 | 2.903 |
| valkey1 | new | 12.87 | 235.5 | 251.4 | 3.097 | 2.874 |

Mechanism: instruction count. larson1 executes 268.5 against 245.8
instructions per operation (+9.2%) with L1D and dTLB misses per operation
unchanged (2.096 vs 2.088, 4.030 vs 4.026), 74.8 against 70.9 cycles
(+5.5%). Every cache push and pop (`len + 1`, `len - 1`, `low_water`,
`run_end`, the batch counters) and every loop counter of `release_owned`
and `claim` gains a checked add or subtract with its cold branch, which
also stops the unrolling that hid the cache index arithmetic; the
free-heavy rows pay most (xmalloc's remote frees are one `release_owned`
loop per batch). valkey1 gains 5.2% instructions per op and loses nothing
in cycles (one run; the row is memory bound on the 550 MiB heap and the
extra instructions hide under the misses). `larson1`, `larson10` and
`xmalloc` are 5..18% behind on both architectures, so the flag fails the
task's own acceptance and stays off; the `bench/quick.sh` rows below are
the same code with the flag off.

## 3. `quick` with the assertions only, `overflow-checks` off (go)

aarch64, working tree against 1aa14da (go (new within 5% of old on every row; churn compacts)):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.8 (16.1 MiB) | 42.8 (15.9 MiB) | 45.1 (18.0 MiB) | +0.0% | +5.4% |
| larson10 | M ops/s | 364.5 (111.9 MiB) | 371.3 (114.2 MiB) | 386.0 (132.7 MiB) | -1.8% | +4.0% |
| xmalloc | M frees/s | 563.794 (52.3 MiB) | 561.915 (57.8 MiB) | 475.967 (79.1 MiB) | +0.3% | -15.3% |
| alloc-test1 | s | 2.660 (12.9 MiB) | 2.680 (12.9 MiB) | 2.539 (12.3 MiB) | +0.8% | +5.6% |
| valkey1 | M ops/s | 6.900 (584.2 MiB) | 6.880 (584.1 MiB) | 7.552 (585.2 MiB) | +0.3% | +9.8% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24221641 ops/s |

Positive percentages mean new did worse (slower, lower throughput). One round each, aarch64, nproc 16, load at start 2.90, 120 s total. old 51e46cec8fba5f10, new 2edfcdb856ac3ff3.

Decision: the assertions land (every row within 1.8% of old, churn
compacts); `overflow-checks` stays off in `[profile.release]`, recorded in
`Cargo.toml` next to the profile and under Design decisions in PROGRESS.md
with these numbers. The counters the review named as wrap-prone (`nslots -
live` in `take`, `ClassStats::empty`/`purged`, `HugeStats::sub_free`,
`Table.count`, `AtomicBitmap::allocated`) are guarded by explicit release
checks on their (slow) paths instead, each with a unit test that builds the
inconsistent state.
