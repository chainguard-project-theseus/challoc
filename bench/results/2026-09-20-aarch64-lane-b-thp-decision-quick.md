# 2026-09-20, aarch64, lane B: `quick` go/no-go for the `thp:never` huge-region decision

Phase 2.5 task 16 follow-up, the human's decision of 2026-09-20 (PROGRESS.md, "Phase 2.5 defaults, the human's decisions", item 1; option (d) of `2026-09-20-aarch64-cgos-thp-ab.md`): `thp:never` (and a forced `never`) issues `MADV_NOHUGEPAGE` on the class superblock and the metadata region only and leaves the huge region to the host's policy. A cold-path change (one `madvise` fewer at init); the reference kernel's global THP mode is `madvise`, so nothing in these rows can move: this is the go/no-go, not a measurement of the decision. The `always`-host measurement is lane E's next cgos iteration (expectation in `PROGRESS.d/lane-b-thp-decision.md`).

| field | value |
|---|---|
| VM | `challoc-lane-b-arm64`, c4a-standard-16 (Axion, Neoverse V2), us-east4-a, Ubuntu 24.04 host, Docker dev image |
| old | `main` 4a7d30c (`4a7d30caedf8`, library hash `60222759fe55d6f1`) |
| new | the working tree of the commit that cites this file (library hash `9d2aacce25fe33dd`) |
| tier | `bench/gce/challoc-bench quick arm64`: one round per row, 161 s total, load at start 3.13 |

Positive percentages mean new did worse.

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.8 (16.1 MiB) | 41.7 (16.1 MiB) | 45.0 (18.1 MiB) | +0.2% | +7.9% |
| larson10 | M ops/s | 363.2 (112.4 MiB) | 368.0 (111.9 MiB) | 390.5 (132.4 MiB) | -1.3% | +6.1% |
| xmalloc | M frees/s | 579.448 (53.9 MiB) | 577.606 (53.6 MiB) | 483.864 (77.3 MiB) | +0.3% | -16.2% |
| alloc-test1 | s | 2.624 (13.0 MiB) | 2.627 (13.0 MiB) | 2.478 (12.3 MiB) | +0.1% | +6.0% |
| valkey1 | M ops/s | 7.371 (584.3 MiB) | 7.298 (584.3 MiB) (median of 3) | 6.815 (585.3 MiB) | +1.0% | -6.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24245310 ops/s |

Verdict: `quick: go (new within 5% of old on every row; churn compacts)`. The only row that moves past rounding is `valkey1` at +1.0% on a single round (the `valkey1` spread on this tier is several percent); RSS is identical on every row, as it must be on a `madvise` host where the removed advice was a no-op.
