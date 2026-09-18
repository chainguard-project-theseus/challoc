# challoc sized frees at jemalloc parity, 2026-09-18, x86_64 (GCE C4)

Phase 1.5 task 4e (`docs/plan.md`); the aarch64 file
(`2026-09-18-aarch64-gce-sized-free.md`) carries the mechanism, the `quick`
and `quick-perf` tables for both architectures and the Valkey soak. This
file is the x86_64 A/B protocol run: `challoc-bench ab amd64
VARIANTS='old=HEAD new=tree jemalloc' BENCHES='larson1 larson10 larson16
xmalloc alloc-test1 alloc-testN valkey1 valkey1-free'`, host
challoc-bench-amd64 (c4-standard-16, Xeon Platinum 8581C, us-east4-a,
kernel 7.0.0-1011-gcp), working tree of iteration 24 on top of c258815 (the
code commit's hash is in PROGRESS.md). `quick` was go (section 1 of the
aarch64 file: `valkey1` 14.150 -> 16.191 M ops/s, the other rows within
5%); `quick-perf` put `valkey1` at 5.20 -> 3.33 L1D misses per operation
(jemalloc 3.33) and 164 -> 143 cycles (jemalloc 129).

Reading of the medians below: `valkey1` (frees through `sdallocx`) 14.219
-> 15.930 M ops/s (+12.0%; jemalloc 15.822, so challoc is now 0.7% ahead of
jemalloc on this row after being 10.1% behind), `valkey1-free` (plain
`free`) 15.027 -> 15.015 (unchanged, as the plan expects: plain `free`
keeps its check), larson1/10/16, xmalloc and alloc-test within their
sorted-list overlaps (larson10's lists 273.8..293.0 against 285.8..301.0
overlap, the row's usual bimodal spread on this VM). RSS unchanged on every
row. A duplicate `make ab` launched by a stuck ssh loop was refused by the
bench lock (`target/bench.lock`) and did not run.

## A/B protocol run (harness output)
- commit: c258815 (uncommitted changes in src/; the tree is what 27de567 committed, variant `new`)
- date: 2026-09-18T04:01:38Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.05 0.97 1.45; at end: 5.78 5.84 4.38; 976 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - old: c2588157b55e built into target/ab/c2588157b55e
  - new: working tree (HEAD c258815 + uncommitted src changes) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 48.8 48.9 48.9 49.0 49.1 | 16228 |
| larson1 | new | 46.8 48.5 48.5 49.0 49.1 | 16332 |
| larson1 | jemalloc | 43.3 45.5 46.5 47.0 47.3 | 18536 |
| larson10 | old | 285.8 290.3 295.9 297.5 301.0 | 101388 |
| larson10 | new | 273.8 285.0 291.3 292.4 293.0 | 100472 |
| larson10 | jemalloc | 295.0 296.7 300.4 301.2 309.4 | 95440 |
| larson16 | old | 318.6 321.0 321.1 322.9 324.2 | 126244 |
| larson16 | new | 313.2 317.3 319.8 325.5 328.3 | 130588 |
| larson16 | jemalloc | 335.9 342.0 342.0 343.1 344.8 | 131152 |
| xmalloc | old | 347.508 348.381 354.795 357.592 363.946 | 83144 |
| xmalloc | new | 342.917 347.531 352.361 352.700 363.458 | 83536 |
| xmalloc | jemalloc | 329.701 330.473 333.891 335.279 336.846 | 93124 |
| alloc-test1 | old | 2.916 2.943 2.944 2.947 2.955 | 13992 |
| alloc-test1 | new | 2.935 2.937 2.951 2.954 2.971 | 13992 |
| alloc-test1 | jemalloc | 2.813 2.842 2.859 2.868 3.003 | 13292 |
| alloc-testN | old | 4.264 4.277 4.293 4.295 4.300 | 21644 |
| alloc-testN | new | 4.263 4.307 4.339 4.355 4.397 | 21648 |
| alloc-testN | jemalloc | 4.030 4.045 4.056 4.126 4.165 | 17080 |
| valkey1 | old | 13.688 14.187 14.219 14.381 14.569 | 602008 |
| valkey1 | new | 15.709 15.902 15.930 15.977 16.514 | 602132 |
| valkey1 | jemalloc | 15.026 15.801 15.822 15.965 16.255 | 606532 |
| valkey1-free | old | 14.568 14.648 15.027 15.092 15.564 | 602084 |
| valkey1-free | new | 14.979 15.004 15.015 15.031 15.172 | 602052 |
| valkey1-free | jemalloc | 13.299 13.605 13.720 13.976 14.056 | 606508 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 48.9 | 15.8 | 48.5 | 15.9 | 46.5 | 18.1 | +0.8% | +5.2% |
| larson10 | M ops/s | 295.9 | 99.0 | 291.3 | 98.1 | 300.4 | 93.2 | +1.6% | -1.5% |
| larson16 | M ops/s | 321.1 | 123.3 | 319.8 | 127.5 | 342.0 | 128.1 | +0.4% | -6.1% |
| xmalloc | M frees/s | 354.795 | 81.2 | 352.361 | 81.6 | 333.891 | 90.9 | +0.7% | +6.3% |
| alloc-test1 | s | 2.944 | 13.7 | 2.951 | 13.7 | 2.859 | 13.0 | +0.2% | -2.9% |
| alloc-testN | s | 4.293 | 21.1 | 4.339 | 21.1 | 4.056 | 16.7 | +1.1% | -5.5% |
| valkey1 | M ops/s | 14.219 | 587.9 | 15.930 | 588.0 | 15.822 | 592.3 | -10.7% | -10.1% |
| valkey1-free | M ops/s | 15.027 | 588.0 | 15.015 | 587.9 | 13.720 | 592.3 | +0.1% | +9.5% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
