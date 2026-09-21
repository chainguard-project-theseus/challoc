# challoc A/B, 2026-09-21, aarch64

- commit: cc547ed
- date: 2026-09-21T04:25:52Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 2.87 2.08 1.30; at end: 5.71 5.76 4.90; 1571 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - old: 2078f672bcdc built into target/ab/2078f672bcdc
  - new: cc547edbc8b1 built into target/ab/cc547edbc8b1
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)
  - new-percpu: cc547edbc8b1 built into target/ab/cc547edbc8b1, MALLOC_CONF=percpu_arena:percpu
  - old-percpu: 2078f672bcdc built into target/ab/2078f672bcdc, MALLOC_CONF=percpu_arena:percpu

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 41.7 41.9 41.9 41.9 42.0 | 16272 |
| larson1 | new | 42.1 42.3 42.3 42.4 42.4 | 16272 |
| larson1 | jemalloc | 45.0 45.0 45.0 45.1 45.1 | 18836 |
| larson1 | new-percpu | 25.0 25.0 25.1 25.1 25.2 | 14352 |
| larson1 | old-percpu | 39.5 39.7 39.8 39.8 39.8 | 16912 |
| larson10 | old | 358.1 359.8 362.0 362.2 365.2 | 114064 |
| larson10 | new | 366.6 368.1 368.8 369.7 371.8 | 115344 |
| larson10 | jemalloc | 380.6 385.8 387.0 387.5 387.6 | 135252 |
| larson10 | new-percpu | 242.2 242.4 243.1 245.2 245.6 | 106380 |
| larson10 | old-percpu | 328.3 328.3 329.4 330.9 331.7 | 125712 |
| larson16 | old | 587.0 587.6 587.7 587.9 590.8 | 169232 |
| larson16 | new | 591.3 592.4 593.3 594.9 597.5 | 168720 |
| larson16 | jemalloc | 614.6 615.2 615.5 616.6 617.2 | 212564 |
| larson16 | new-percpu | 391.6 392.3 392.4 393.0 395.1 | 144388 |
| larson16 | old-percpu | 526.8 528.0 529.3 532.1 532.4 | 172172 |
| xmalloc | old | 575.297 575.991 576.684 576.817 577.632 | 54492 |
| xmalloc | new | 577.401 578.021 579.657 579.833 579.900 | 54612 |
| xmalloc | jemalloc | 466.742 474.832 475.220 475.672 482.429 | 82548 |
| xmalloc | new-percpu | 391.293 392.182 392.689 393.643 394.984 | 63348 |
| xmalloc | old-percpu | 486.484 488.579 488.729 492.939 493.238 | 84120 |
| alloc-test1 | old | 2.652 2.670 2.672 2.673 2.688 | 13464 |
| alloc-test1 | new | 2.644 2.647 2.651 2.666 2.672 | 13464 |
| alloc-test1 | jemalloc | 2.526 2.530 2.533 2.538 2.556 | 12640 |
| alloc-test1 | new-percpu | 3.199 3.215 3.227 3.238 3.244 | 13464 |
| alloc-test1 | old-percpu | 2.824 2.843 2.871 2.888 2.890 | 13464 |
| alloc-testN | old | 2.289 2.291 2.305 2.309 2.310 | 16916 |
| alloc-testN | new | 2.266 2.269 2.287 2.287 2.287 | 16840 |
| alloc-testN | jemalloc | 2.198 2.198 2.199 2.200 2.208 | 16860 |
| alloc-testN | new-percpu | 2.760 2.760 2.783 2.787 2.788 | 16988 |
| alloc-testN | old-percpu | 2.426 2.431 2.431 2.456 2.457 | 16976 |
| valkey1 | old | 6.994 7.017 7.069 7.154 7.190 | 598736 |
| valkey1 | new | 6.885 6.917 7.122 7.196 7.214 | 598740 |
| valkey1 | jemalloc | 6.927 6.945 7.053 7.090 7.169 | 599768 |
| valkey1 | new-percpu | 5.480 5.495 5.551 5.625 5.677 | 598480 |
| valkey1 | old-percpu | 6.615 6.625 6.630 6.720 6.729 | 598736 |
| valkey1-free | old | 6.768 6.992 7.015 7.075 7.078 | 598740 |
| valkey1-free | new | 6.960 7.020 7.040 7.042 7.074 | 598740 |
| valkey1-free | jemalloc | 6.840 6.908 7.020 7.030 7.105 | 599764 |
| valkey1-free | new-percpu | 5.633 5.718 5.723 5.725 5.796 | 598484 |
| valkey1-free | old-percpu | 6.340 6.360 6.377 6.395 6.435 | 598736 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new-percpu | new-percpu RSS MiB | old-percpu | old-percpu RSS MiB | new vs old | jemalloc vs old | new-percpu vs old | old-percpu vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.9 | 15.9 | 42.3 | 15.9 | 45.0 | 18.4 | 25.1 | 14.0 | 39.8 | 16.5 | -0.9% | -6.9% | +66.9% | +5.3% |
| larson10 | M ops/s | 362.0 | 111.4 | 368.8 | 112.6 | 387.0 | 132.1 | 243.1 | 103.9 | 329.4 | 122.8 | -1.8% | -6.5% | +48.9% | +9.9% |
| larson16 | M ops/s | 587.7 | 165.3 | 593.3 | 164.8 | 615.5 | 207.6 | 392.4 | 141.0 | 529.3 | 168.1 | -0.9% | -4.5% | +49.8% | +11.0% |
| xmalloc | M frees/s | 576.684 | 53.2 | 579.657 | 53.3 | 475.220 | 80.6 | 392.689 | 61.9 | 488.729 | 82.1 | -0.5% | +21.4% | +46.9% | +18.0% |
| alloc-test1 | s | 2.672 | 13.1 | 2.651 | 13.1 | 2.533 | 12.3 | 3.227 | 13.1 | 2.871 | 13.1 | -0.8% | -5.2% | +20.8% | +7.4% |
| alloc-testN | s | 2.305 | 16.5 | 2.287 | 16.4 | 2.199 | 16.5 | 2.783 | 16.6 | 2.431 | 16.6 | -0.8% | -4.6% | +20.7% | +5.5% |
| valkey1 | M ops/s | 7.069 | 584.7 | 7.122 | 584.7 | 7.053 | 585.7 | 5.551 | 584.5 | 6.630 | 584.7 | -0.7% | +0.2% | +27.3% | +6.6% |
| valkey1-free | M ops/s | 7.015 | 584.7 | 7.040 | 584.7 | 7.020 | 585.7 | 5.723 | 584.5 | 6.377 | 584.7 | -0.4% | -0.1% | +22.6% | +10.0% |

Positive percentages mean the variant did worse than old (slower or lower throughput).
