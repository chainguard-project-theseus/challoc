# Policy cost: 16 and 32 KiB slabs for the 1280..4096-byte classes, 2026-09-21, x86_64 (lane policy-cost)

The x86_64 half of `2026-09-21-aarch64-policy-cost-geometry.md`, which
has the question, the exact variants, the per-class table each build
produces and the joint reading; this file records the same rows on the
x86_64 VM. Measurement only, no default changed, nothing under `src/`
changed.

- host: `challoc-policy-cost-amd64` (c4-standard-16, Intel Xeon
  Platinum 8581C, Emerald Rapids), 16 vCPUs, Ubuntu 24.04 kernel
  `7.0.0-1011-gcp`, 4 KiB pages, THP `madvise`; container: Wolfi
  (rustc 1.98.1); jemalloc = Wolfi's libjemalloc.so.2 5.3.1-r3. Zone
  us-east4-a. Nothing else ran on the VM.
- commit: `f53418d` on `loop/policy-cost` (harness hooks only; `src/`
  is byte for byte main `2078f67`).
- builds (`quantum16`, sha256 prefixes): `default` = the tree,
  `d4f7099f2b37ad12`; `slab16` = `.tmp-slab16` (16 KiB slabs for
  1280..4096 B), `29124797761cb759`; `slab32` = `.tmp-slab32` (32 KiB),
  `324c10c34496c1d5`; the same one-line variants as on aarch64
  (`bench/geometry-variant.sh slab16 14`, `slab32 15`), the same
  per-class table.
- method: as the aarch64 file (`quick` for each variant, `ab` with 5
  rounds and four variants, `quick-perf`, the Ruby soak for all three
  builds and jemalloc; the Valkey soak of this topic ran on aarch64
  only).

## 1. `quick` (one round each; the 5% gate)

`slab16` (new) vs `default` (old), load 3.20 at start, 129 s, `old
d4f7099f2b37ad12, new 29124797761cb759`:

| row | unit | old (default) | new (slab16) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 46.1 (17.1 MiB) | 46.6 (17.3 MiB) | 44.2 (18.1 MiB) | -1.1% | -5.2% |
| larson10 | M ops/s | 284.7 (96.1 MiB) | 290.9 (97.6 MiB) | 288.9 (93.0 MiB) | -2.1% | -0.7% |
| xmalloc | M frees/s | 341.411 (53.1 MiB) | 338.835 (52.4 MiB) | 338.891 (93.8 MiB) | +0.8% | +0.0% |
| alloc-test1 | s | 3.094 (14.0 MiB) | 3.147 (14.0 MiB) | 2.900 (13.3 MiB) | +1.7% | +8.5% |
| valkey1 | M ops/s | 14.822 (585.1 MiB) | 14.940 (585.5 MiB) | 14.727 (592.0 MiB) | -0.8% | -1.4% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6669 (49.9%) | new compacts (end occupancy >= 90%) | 23069869 ops/s |

`quick: go`. `slab32` (new) vs `default` (old), load 3.22, 128 s, `new
324c10c34496c1d5`:

| row | unit | old (default) | new (slab32) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 44.9 (17.1 MiB) | 46.0 (16.7 MiB) | 36.4 (17.3 MiB) | -2.4% | -20.9% |
| larson10 | M ops/s | 272.4 (94.9 MiB) | 295.0 (97.1 MiB) | 297.5 (93.7 MiB) | -7.7% | +0.8% |
| xmalloc | M frees/s | 339.221 (53.0 MiB) | 323.586 (52.3 MiB) | 339.264 (91.8 MiB) | +4.8% | +4.8% |
| alloc-test1 | s | 3.070 (13.9 MiB) | 3.086 (14.0 MiB) | 2.890 (13.3 MiB) | +0.5% | +6.8% |
| valkey1 | M ops/s | 15.072 (585.4 MiB) | 15.089 (585.3 MiB) | 13.057 (592.1 MiB) | -0.1% | -13.5% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6678 (49.8%) | new compacts (end occupancy >= 90%) | 22749839 ops/s |

`quick: go` for both. The one-round x86_64 rows are noisier than
aarch64's (larson10 272 vs 295 with `slab32` ahead, xmalloc +4.8%, a
jemalloc larson1 of 36.4 in one round): the `ab` below settles them.

## 2. `ab` (5 rounds, interleaved): `default`, `slab16`, `slab32`, `jemalloc`

Load 3.12 at start, 4.87 at the end.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | default | 44.4 45.0 46.3 46.3 46.6 | 17696 |
| larson1 | slab16 | 45.8 46.3 46.4 46.4 46.5 | 17652 |
| larson1 | slab32 | 43.9 44.3 45.9 45.9 46.0 | 17452 |
| larson1 | jemalloc | 46.1 46.1 46.3 46.4 46.6 | 18688 |
| larson10 | default | 271.9 286.4 289.7 293.2 294.1 | 99280 |
| larson10 | slab16 | 286.3 288.1 289.1 290.3 291.6 | 98904 |
| larson10 | slab32 | 283.0 283.6 287.0 288.5 294.9 | 98340 |
| larson10 | jemalloc | 301.1 302.9 303.3 309.3 312.9 | 96812 |
| larson16 | default | 304.6 311.9 313.3 315.5 316.8 | 121676 |
| larson16 | slab16 | 309.6 312.3 314.9 319.1 319.3 | 122936 |
| larson16 | slab32 | 308.8 309.0 310.2 316.4 316.4 | 119884 |
| larson16 | jemalloc | 333.0 341.7 344.0 351.6 351.7 | 131968 |
| xmalloc | default | 323.663 331.752 340.438 350.817 351.343 | 54824 |
| xmalloc | slab16 | 338.339 339.920 340.287 340.750 341.482 | 54256 |
| xmalloc | slab32 | 325.591 330.309 338.995 340.434 355.945 | 54088 |
| xmalloc | jemalloc | 335.700 338.964 339.527 339.843 340.806 | 89524 |
| alloc-test1 | default | 3.067 3.069 3.072 3.077 3.104 | 14320 |
| alloc-test1 | slab16 | 3.063 3.107 3.112 3.122 3.135 | 14356 |
| alloc-test1 | slab32 | 3.085 3.087 3.112 3.118 3.122 | 14276 |
| alloc-test1 | jemalloc | 2.882 2.890 2.894 2.897 2.909 | 13604 |
| alloc-testN | default | 4.335 4.376 4.390 4.395 4.419 | 17740 |
| alloc-testN | slab16 | 4.374 4.406 4.444 4.474 4.478 | 17644 |
| alloc-testN | slab32 | 4.417 4.453 4.463 4.538 4.550 | 17800 |
| alloc-testN | jemalloc | 4.015 4.027 4.058 4.066 4.129 | 17360 |
| valkey1 | default | 14.876 14.984 15.124 15.272 15.298 | 599700 |
| valkey1 | slab16 | 14.692 15.022 15.168 15.217 15.222 | 600024 |
| valkey1 | slab32 | 14.327 14.610 15.001 15.036 15.091 | 599812 |
| valkey1 | jemalloc | 14.641 14.855 14.868 14.953 14.999 | 606700 |
| valkey1-free | default | 14.463 14.958 15.072 15.147 15.175 | 599700 |
| valkey1-free | slab16 | 14.814 14.852 14.905 14.996 15.202 | 600068 |
| valkey1-free | slab32 | 14.753 14.850 14.954 15.070 15.101 | 599788 |
| valkey1-free | jemalloc | 13.024 13.135 13.163 13.223 13.289 | 606712 |

| program | unit | default | default RSS MiB | slab16 | slab16 RSS MiB | slab32 | slab32 RSS MiB | jemalloc | jemalloc RSS MiB | slab16 vs default | slab32 vs default | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 46.3 | 17.3 | 46.4 | 17.2 | 45.9 | 17.0 | 46.3 | 18.2 | -0.2% | +0.9% | +0.0% |
| larson10 | M ops/s | 289.7 | 97.0 | 289.1 | 96.6 | 287.0 | 96.0 | 303.3 | 94.5 | +0.2% | +0.9% | -4.5% |
| larson16 | M ops/s | 313.3 | 118.8 | 314.9 | 120.1 | 310.2 | 117.1 | 344.0 | 128.9 | -0.5% | +1.0% | -8.9% |
| xmalloc | M frees/s | 340.438 | 53.5 | 340.287 | 53.0 | 338.995 | 52.8 | 339.527 | 87.4 | +0.0% | +0.4% | +0.3% |
| alloc-test1 | s | 3.072 | 14.0 | 3.112 | 14.0 | 3.112 | 13.9 | 2.894 | 13.3 | +1.3% | +1.3% | -5.8% |
| alloc-testN | s | 4.390 | 17.3 | 4.444 | 17.2 | 4.463 | 17.4 | 4.058 | 17.0 | +1.2% | +1.7% | -7.6% |
| valkey1 | M ops/s | 15.124 | 585.6 | 15.168 | 586.0 | 15.001 | 585.8 | 14.868 | 592.5 | -0.3% | +0.8% | +1.7% |
| valkey1-free | M ops/s | 15.072 | 585.6 | 14.905 | 586.0 | 14.954 | 585.7 | 13.163 | 592.5 | +1.1% | +0.8% | +14.5% |

Positive percentages mean the variant did worse than `default`.

Reading: every row within 2% with interleaved lists (alloc-test's +1.3%
and +1.2..1.7% are 3.063..3.135 against 3.067..3.104, and 4.374..4.478
against 4.335..4.419: the widest of the table and overlapping). As on
aarch64, these programs do not allocate in the changed classes; the
table says the change is invisible to them.

## 3. `quick-perf`: per-operation counters (larson1, valkey1), three builds and jemalloc

PENDING (the step ran with a wrong path and is rerun after the soaks).

## 4. Valkey soak

Ran on aarch64 only (`2026-09-21-aarch64-policy-cost-geometry.md`,
section 4).

## 5. Ruby soak (600 s, 16 threads): `default`, `slab16`, `slab32`, jemalloc

PENDING.

## 6. Comparison and the trade

PENDING (the aarch64 file has the joint reading).
