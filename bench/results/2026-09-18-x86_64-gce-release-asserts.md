# challoc release assertions and `overflow-checks`, 2026-09-18, x86_64 (GCE C4)

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

x86_64, working tree against 1aa14da (NO-GO (new trails old by more than 5% on a row, or does not compact)):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.9 (15.1 MiB) | 42.5 (14.8 MiB) (median of 3) | 41.1 (17.4 MiB) | +8.0% | -3.3% |
| larson10 | M ops/s | 279.1 (93.3 MiB) | 268.2 (94.4 MiB) | 298.6 (93.9 MiB) | +4.1% | +11.3% |
| xmalloc | M frees/s | 333.086 (52.0 MiB) | 290.175 (52.7 MiB) | 328.434 (87.4 MiB) | +14.8% | +13.2% |
| alloc-test1 | s | 3.402 (13.7 MiB) | 3.490 (13.5 MiB) | 3.222 (13.1 MiB) | +2.6% | +8.3% |
| valkey1 | M ops/s | 14.936 (585.0 MiB) | 14.269 (584.9 MiB) | 13.660 (592.0 MiB) | +4.7% | -4.3% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6658 (49.9%) | new compacts (end occupancy >= 90%) | 20633826 ops/s |

Positive percentages mean new did worse (slower, lower throughput). One round each, x86_64, nproc 16, load at start 1.96, 154 s total. old b0bb3ed275c7de8e, new 32db2464d8901570.

## 2. Attribution

The per-operation attribution ran on aarch64
(`2026-09-18-aarch64-gce-release-asserts.md` section 2): +9.2% instructions
per larson1 operation at unchanged misses, an instruction-count mechanism,
the same shape as this architecture's rows above (xmalloc +14.8%, larson1
+8.0% median of 3, valkey1 +4.7%).

An intermediate run of this tree with the flag off but the `others`-node
assertion (`assert_ne!(arena, self.arena)`) still at the top of
`Heap::other_caches`, which `free_other` calls once per remote free
(xmalloc's path), had xmalloc at +6.0% (median of 3; 337.720 -> 318.615 M
frees/s) with every other row within 1.6%. The assertion was moved to the
node-creation branch (the invariant it guards, a home-arena node splitting
the home caches, can only be violated there) and the final run below has
xmalloc 3.6% ahead of old, so the +6.0% was that compare on the per-free
path, noise, or both; either way the final tree carries nothing on it.

## 3. `quick` with the assertions only, `overflow-checks` off (go)

x86_64, working tree against 1aa14da (go (new within 5% of old on every row; churn compacts)):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 43.5 (14.9 MiB) | 44.2 (15.1 MiB) | 42.8 (17.4 MiB) | -1.6% | -3.2% |
| larson10 | M ops/s | 280.5 (93.4 MiB) | 286.8 (93.3 MiB) | 290.7 (92.2 MiB) | -2.2% | +1.4% |
| xmalloc | M frees/s | 327.979 (52.6 MiB) | 340.238 (52.3 MiB) | 328.788 (87.5 MiB) | -3.6% | -3.4% |
| alloc-test1 | s | 3.412 (13.6 MiB) | 3.426 (13.5 MiB) | 3.237 (13.1 MiB) | +0.4% | +5.8% |
| valkey1 | M ops/s | 14.795 (585.1 MiB) | 14.960 (585.0 MiB) | 14.535 (591.8 MiB) | -1.1% | -2.8% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6664 (49.9%) | new compacts (end occupancy >= 90%) | 20785778 ops/s |

Positive percentages mean new did worse (slower, lower throughput). One round each, x86_64, nproc 16, load at start 2.96, 122 s total. old b0bb3ed275c7de8e, new 67b42ef347cb3b81.

Decision: the assertions land (every row within 3.6% of old, churn
compacts); `overflow-checks` stays off in `[profile.release]`, recorded in
`Cargo.toml` next to the profile and under Design decisions in PROGRESS.md
with these numbers. The counters the review named as wrap-prone (`nslots -
live` in `take`, `ClassStats::empty`/`purged`, `HugeStats::sub_free`,
`Table.count`, `AtomicBitmap::allocated`) are guarded by explicit release
checks on their (slow) paths instead, each with a unit test that builds the
inconsistent state.
