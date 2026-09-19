# challoc four-way, 2026-09-19, x86_64

- commit: 1339eb6
- date: 2026-09-19T03:44:40Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (GCE c4-standard-16 (Intel Xeon Platinum 8581C), zone us-east4-a), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 0.00 0.06 0.08; at end: 4.07 5.17 4.64; 1868 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than challoc.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - challoc: 1339eb6ee395 built into target/ab/1339eb6ee395
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)
  - glibc: glibc malloc (no LD_PRELOAD, glibc 2.44)
  - mimalloc: /src/target/bench-bin/libmimalloc.so (v3.5.2)
  - tcmalloc: Wolfi's /usr/lib/libtcmalloc_minimal.so.4 (tcmalloc-minimal 2.18.1, gperftools without profiler hooks)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larsonN | challoc | 306.2 311.2 311.4 313.5 318.8 | 123012 |
| larsonN | jemalloc | 339.6 339.7 343.7 349.2 352.1 | 132300 |
| larsonN | glibc | 306.0 311.3 312.1 312.8 315.2 | 155064 |
| larsonN | mimalloc | 298.8 299.1 304.2 304.3 306.2 | 147528 |
| larsonN | tcmalloc | 340.3 341.5 341.9 343.8 356.2 | 92024 |
| xmallocN | challoc | 323.706 324.218 325.514 325.705 326.010 | 113172 |
| xmallocN | jemalloc | 336.918 337.465 337.749 337.750 338.295 | 121912 |
| xmallocN | glibc | 56.584 82.264 98.066 100.790 109.248 | 54536 |
| xmallocN | mimalloc | 321.270 321.604 321.936 321.940 322.156 | 67916 |
| xmallocN | tcmalloc | 2.767 3.026 3.039 3.152 3.222 | 45376 |
| cfrac | challoc | 3.599 3.713 3.715 3.729 3.783 | 3696 |
| cfrac | jemalloc | 3.461 3.554 3.564 3.577 3.591 | 5876 |
| cfrac | glibc | 3.389 3.478 3.491 3.503 3.607 | 3396 |
| cfrac | mimalloc | 3.351 3.354 3.475 3.485 3.487 | 4704 |
| cfrac | tcmalloc | 3.391 3.447 3.493 3.526 3.612 | 9144 |
| espresso | challoc | 4.037 4.180 4.180 4.188 4.195 | 4148 |
| espresso | jemalloc | 4.032 4.172 4.199 4.209 4.226 | 6412 |
| espresso | glibc | 4.313 4.443 4.460 4.484 4.559 | 2884 |
| espresso | mimalloc | 3.941 3.941 4.070 4.074 4.084 | 10588 |
| espresso | tcmalloc | 3.910 3.915 4.017 4.023 4.025 | 9776 |
| mstress | challoc | 0.619 0.628 0.633 0.636 0.646 | 346248 |
| mstress | jemalloc | 1.062 1.067 1.076 1.080 1.089 | 292196 |
| mstress | glibc | 0.979 1.017 1.021 1.028 1.044 | 525868 |
| mstress | mimalloc | 0.550 0.558 0.559 0.566 0.568 | 549312 |
| mstress | tcmalloc | 0.597 0.600 0.606 0.609 0.613 | 285848 |
| alloc-test1 | challoc | 2.848 2.924 2.928 2.937 2.955 | 14080 |
| alloc-test1 | jemalloc | 2.701 2.764 2.778 2.784 2.791 | 13524 |
| alloc-test1 | glibc | 3.198 3.322 3.345 3.362 3.406 | 14828 |
| alloc-test1 | mimalloc | 2.636 2.638 2.720 2.726 2.742 | 16964 |
| alloc-test1 | tcmalloc | 2.637 2.651 2.715 2.723 2.741 | 16852 |
| alloc-testN | challoc | 4.413 4.423 4.443 4.461 4.505 | 17748 |
| alloc-testN | jemalloc | 4.074 4.109 4.110 4.111 4.114 | 17360 |
| alloc-testN | glibc | 4.645 4.671 4.682 4.692 4.792 | 18580 |
| alloc-testN | mimalloc | 3.958 3.961 3.963 3.967 3.982 | 62076 |
| alloc-testN | tcmalloc | 4.260 4.281 4.289 4.301 4.304 | 18848 |
| glibc-simple | challoc | 1.987 2.014 2.035 2.044 2.063 | 2588 |
| glibc-simple | jemalloc | 1.824 1.864 1.868 1.875 1.886 | 5136 |
| glibc-simple | glibc | 3.093 3.129 3.202 3.270 3.401 | 2508 |
| glibc-simple | mimalloc | 1.427 1.438 1.455 1.489 1.493 | 4224 |
| glibc-simple | tcmalloc | 1.437 1.464 1.514 1.517 1.582 | 8496 |
| redis | challoc | 693.5 698.3 701.3 714.3 737.5 | 30792 |
| redis | jemalloc | 531.3 547.6 554.3 555.6 583.4 | 33212 |
| redis | glibc | 706.7 736.4 739.1 749.6 788.6 | 30356 |
| redis | mimalloc | 674.8 675.2 676.1 695.9 702.7 | 47548 |
| redis | tcmalloc | 701.8 726.7 744.6 750.2 769.8 | 36128 |
| valkey1 | challoc | 14.663 14.820 15.093 15.100 15.349 | 599688 |
| valkey1 | jemalloc | 15.310 15.578 15.594 15.657 15.702 | 606676 |
| valkey1 | glibc | 8.583 8.901 9.103 9.314 9.433 | 582256 |
| valkey1 | mimalloc | 4.060 4.126 4.182 4.219 4.222 | 600332 |
| valkey1 | tcmalloc | 14.782 15.004 15.642 15.650 15.732 | 602704 |
| valkey1-free | challoc | 15.316 15.399 16.133 16.283 16.376 | 599688 |
| valkey1-free | jemalloc | 13.630 13.706 13.802 13.892 14.123 | 606712 |
| valkey1-free | glibc | 8.585 9.231 9.235 9.236 9.297 | 582240 |
| valkey1-free | mimalloc | 4.077 4.136 4.193 4.212 4.237 | 600208 |
| valkey1-free | tcmalloc | 14.066 15.566 15.631 15.759 15.904 | 602704 |
| valkey1-usable | challoc | 8.286 10.663 10.734 10.882 11.211 | 599320 |
| valkey1-usable | jemalloc | 8.184 8.241 8.346 8.663 8.683 | 606524 |
| valkey1-usable | glibc | 6.356 6.623 6.641 6.779 6.942 | 581756 |
| valkey1-usable | mimalloc | 3.267 3.503 3.595 3.601 3.648 | 600276 |
| valkey1-usable | tcmalloc | 11.606 12.168 12.212 12.495 12.647 | 602272 |

## Medians

| program | unit | challoc | challoc RSS MiB | jemalloc | jemalloc RSS MiB | glibc | glibc RSS MiB | mimalloc | mimalloc RSS MiB | tcmalloc | tcmalloc RSS MiB | jemalloc vs challoc | glibc vs challoc | mimalloc vs challoc | tcmalloc vs challoc |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larsonN | M ops/s | 311.4 | 120.1 | 343.7 | 129.2 | 312.1 | 151.4 | 304.2 | 144.1 | 341.9 | 89.9 | -9.4% | -0.2% | +2.4% | -8.9% |
| xmallocN | M frees/s | 325.514 | 110.5 | 337.749 | 119.1 | 98.066 | 53.3 | 321.936 | 66.3 | 3.039 | 44.3 | -3.6% | +231.9% | +1.1% | +10611.2% |
| cfrac | s | 3.715 | 3.6 | 3.564 | 5.7 | 3.491 | 3.3 | 3.475 | 4.6 | 3.493 | 8.9 | -4.1% | -6.0% | -6.5% | -6.0% |
| espresso | s | 4.180 | 4.1 | 4.199 | 6.3 | 4.460 | 2.8 | 4.070 | 10.3 | 4.017 | 9.5 | +0.5% | +6.7% | -2.6% | -3.9% |
| mstress | s | 0.633 | 338.1 | 1.076 | 285.3 | 1.021 | 513.5 | 0.559 | 536.4 | 0.606 | 279.1 | +70.0% | +61.3% | -11.7% | -4.3% |
| alloc-test1 | s | 2.928 | 13.8 | 2.778 | 13.2 | 3.345 | 14.5 | 2.720 | 16.6 | 2.715 | 16.5 | -5.1% | +14.2% | -7.1% | -7.3% |
| alloc-testN | s | 4.443 | 17.3 | 4.110 | 17.0 | 4.682 | 18.1 | 3.963 | 60.6 | 4.289 | 18.4 | -7.5% | +5.4% | -10.8% | -3.5% |
| glibc-simple | s | 2.035 | 2.5 | 1.868 | 5.0 | 3.202 | 2.4 | 1.455 | 4.1 | 1.514 | 8.3 | -8.2% | +57.3% | -28.5% | -25.6% |
| redis | k req/s | 701.3 | 30.1 | 554.3 | 32.4 | 739.1 | 29.6 | 676.1 | 46.4 | 744.6 | 35.3 | +26.5% | -5.1% | +3.7% | -5.8% |
| valkey1 | M ops/s | 15.093 | 585.6 | 15.594 | 592.5 | 9.103 | 568.6 | 4.182 | 586.3 | 15.642 | 588.6 | -3.2% | +65.8% | +260.9% | -3.5% |
| valkey1-free | M ops/s | 16.133 | 585.6 | 13.802 | 592.5 | 9.235 | 568.6 | 4.193 | 586.1 | 15.631 | 588.6 | +16.9% | +74.7% | +284.8% | +3.2% |
| valkey1-usable | M ops/s | 10.734 | 585.3 | 8.346 | 592.3 | 6.641 | 568.1 | 3.595 | 586.2 | 12.212 | 588.2 | +28.6% | +61.6% | +198.6% | -12.1% |

Positive percentages mean the variant did worse than challoc (slower or lower throughput).
