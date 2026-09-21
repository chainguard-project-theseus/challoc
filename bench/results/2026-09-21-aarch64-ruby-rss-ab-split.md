# challoc split A/B of the Ruby threaded RSS commits, 2026-09-21, aarch64

- recovered from the VM-side `~/logs/ab-split.log` (the results file the run wrote under `bench/results/` was removed by the next `challoc-bench sync`, an rsync `--delete`, before it was fetched); the tables below are the run's own output, verbatim
- host: challoc-ruby-rss-arm64 (c4a-standard-16, Neoverse V2), Ubuntu 24.04 kernel 7.0.0-1011-gcp, the dev image; the tree synced was 6e25c7a
- method: `make ab ROUNDS=5 BENCHES="larson10 larson16 mstress xmalloc"`, 5 timed rounds after one warm-up, variants interleaved; RSS is `rt`'s median maxrss
- variants: old = df6d285 (the base, `src/` of main 2078f67); f3 = 4dc352c (every plain free onto the home cache, the flush routing it); purge = 1a45278 (f3 plus the unlimited exit purge); new = 6e25c7a (f3 plus the exit purge rate-limited to once per decay period per arena); jemalloc = Wolfi's 5.3.1
- what it settles: the exit purge, unlimited, costs mstress +10.7% (refaults under thread churn: 800 thread exits in 0.4 s) and the rate limit removes it (-2.6%); the home-cache form costs nothing on this CPU (larson10 +1.0%, larson16 -1.4%), unlike the Xeon (`2026-09-21-x86_64-ruby-rss-ab-split.md`)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson10 | old | 360.7 361.5 367.6 369.3 369.7 | 114576 |
| larson10 | f3 | 361.3 362.0 364.1 366.7 370.8 | 102856 |
| larson10 | purge | 363.3 364.5 364.6 365.1 365.8 | 98072 |
| larson10 | new | 359.4 362.0 362.6 364.8 367.0 | 103044 |
| larson10 | jemalloc | 382.4 386.4 389.4 390.4 390.8 | 135264 |
| larson16 | old | 580.6 586.4 587.6 590.7 590.8 | 167440 |
| larson16 | f3 | 592.2 594.4 595.9 598.4 601.3 | 159644 |
| larson16 | purge | 584.3 587.3 588.9 591.9 595.1 | 151204 |
| larson16 | new | 592.3 595.1 595.6 595.6 601.7 | 159756 |
| larson16 | jemalloc | 609.5 612.2 616.0 616.1 619.0 | 211500 |
| mstress | old | 0.383 0.383 0.384 0.386 0.388 | 334912 |
| mstress | f3 | 0.370 0.372 0.372 0.373 0.375 | 323384 |
| mstress | purge | 0.424 0.424 0.425 0.428 0.430 | 286784 |
| mstress | new | 0.372 0.374 0.374 0.375 0.376 | 312788 |
| mstress | jemalloc | 0.651 0.652 0.656 0.659 0.661 | 299720 |
| xmalloc | old | 575.474 576.198 576.698 576.886 577.101 | 56036 |
| xmalloc | f3 | 962.091 986.251 986.981 988.453 989.398 | 68564 |
| xmalloc | purge | 965.191 975.802 982.310 997.518 1010.929 | 69628 |
| xmalloc | new | 965.213 970.172 989.948 992.083 998.300 | 69968 |
| xmalloc | jemalloc | 471.870 476.228 479.310 480.696 481.414 | 81412 |

## Medians

| program | unit | old | old RSS MiB | f3 | f3 RSS MiB | purge | purge RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | f3 vs old | purge vs old | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson10 | M ops/s | 367.6 | 111.9 | 364.1 | 100.4 | 364.6 | 95.8 | 362.6 | 100.6 | 389.4 | 132.1 | +1.0% | +0.8% | +1.4% | -5.6% |
| larson16 | M ops/s | 587.6 | 163.5 | 595.9 | 155.9 | 588.9 | 147.7 | 595.6 | 156.0 | 616.0 | 206.5 | -1.4% | -0.2% | -1.3% | -4.6% |
| mstress | s | 0.384 | 327.1 | 0.372 | 315.8 | 0.425 | 280.1 | 0.374 | 305.5 | 0.656 | 292.7 | -3.1% | +10.7% | -2.6% | +70.8% |
| xmalloc | M frees/s | 576.698 | 54.7 | 986.981 | 67.0 | 982.310 | 68.0 | 989.948 | 68.3 | 479.310 | 79.5 | -41.6% | -41.3% | -41.7% | +20.3% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
