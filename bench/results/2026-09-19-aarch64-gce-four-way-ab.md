# challoc four-way, 2026-09-19, aarch64

- commit: 1339eb6
- date: 2026-09-19T03:44:43Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (GCE c4a-standard-16 (Google Axion, Neoverse V2), zone us-east4-a), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 0.00 0.02 0.03; at end: 4.14 5.01 4.34; 1824 s total
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
| larsonN | challoc | 581.9 584.9 586.6 589.4 594.5 | 171288 |
| larsonN | jemalloc | 604.0 604.4 611.8 612.2 613.4 | 211128 |
| larsonN | glibc | 471.7 475.1 477.6 478.2 480.8 | 196392 |
| larsonN | mimalloc | 584.5 585.4 585.7 586.4 589.4 | 193860 |
| larsonN | tcmalloc | 591.3 594.5 595.5 596.4 598.6 | 136492 |
| xmallocN | challoc | 521.097 521.249 521.820 523.337 524.286 | 108672 |
| xmallocN | jemalloc | 476.321 477.440 478.178 479.488 481.560 | 111704 |
| xmallocN | glibc | 28.247 34.786 35.479 35.562 35.830 | 78072 |
| xmallocN | mimalloc | 164.346 166.685 167.386 168.734 170.409 | 67404 |
| xmallocN | tcmalloc | 1.843 1.955 2.038 2.102 2.115 | 43632 |
| cfrac | challoc | 4.446 4.448 4.449 4.450 4.455 | 2836 |
| cfrac | jemalloc | 4.422 4.424 4.425 4.426 4.428 | 4988 |
| cfrac | glibc | 4.302 4.303 4.306 4.324 4.328 | 2680 |
| cfrac | mimalloc | 4.267 4.267 4.269 4.270 4.271 | 3968 |
| cfrac | tcmalloc | 4.384 4.384 4.384 4.385 4.391 | 8216 |
| espresso | challoc | 3.665 3.668 3.668 3.675 3.694 | 3728 |
| espresso | jemalloc | 3.740 3.740 3.740 3.743 3.744 | 6308 |
| espresso | glibc | 3.886 3.892 3.897 3.903 3.921 | 3920 |
| espresso | mimalloc | 3.545 3.546 3.546 3.547 3.549 | 10172 |
| espresso | tcmalloc | 3.575 3.576 3.576 3.576 3.576 | 8976 |
| mstress | challoc | 0.375 0.376 0.377 0.379 0.379 | 324636 |
| mstress | jemalloc | 0.645 0.646 0.647 0.651 0.658 | 298548 |
| mstress | glibc | 0.490 0.492 0.493 0.496 0.497 | 540944 |
| mstress | mimalloc | 0.289 0.290 0.290 0.290 0.290 | 548524 |
| mstress | tcmalloc | 0.417 0.421 0.422 0.422 0.424 | 286976 |
| alloc-test1 | challoc | 2.609 2.611 2.614 2.624 2.627 | 13360 |
| alloc-test1 | jemalloc | 2.498 2.512 2.515 2.517 2.523 | 12636 |
| alloc-test1 | glibc | 3.017 3.021 3.025 3.048 3.053 | 15580 |
| alloc-test1 | mimalloc | 2.421 2.425 2.428 2.432 2.436 | 16204 |
| alloc-test1 | tcmalloc | 2.486 2.488 2.492 2.503 2.503 | 15992 |
| alloc-testN | challoc | 2.246 2.251 2.254 2.257 2.268 | 16808 |
| alloc-testN | jemalloc | 2.199 2.202 2.206 2.210 2.235 | 16860 |
| alloc-testN | glibc | 2.540 2.541 2.541 2.542 2.555 | 35180 |
| alloc-testN | mimalloc | 2.118 2.126 2.137 2.139 2.141 | 63196 |
| alloc-testN | tcmalloc | 2.196 2.197 2.197 2.197 2.198 | 18352 |
| glibc-simple | challoc | 2.286 2.290 2.291 2.293 2.303 | 2280 |
| glibc-simple | jemalloc | 2.525 2.525 2.528 2.531 2.540 | 4484 |
| glibc-simple | glibc | 4.124 4.134 4.158 4.162 4.163 | 3464 |
| glibc-simple | mimalloc | 1.748 1.749 1.751 1.791 1.792 | 3612 |
| glibc-simple | tcmalloc | 1.704 1.715 1.729 1.740 1.741 | 7720 |
| redis | challoc | 498.8 500.8 501.3 504.8 505.8 | 29732 |
| redis | jemalloc | 410.0 410.2 410.7 411.5 412.2 | 31880 |
| redis | glibc | 528.5 532.2 533.0 536.5 538.2 | 32108 |
| redis | mimalloc | 463.8 465.5 465.8 466.4 467.1 | 46476 |
| redis | tcmalloc | 517.6 520.3 521.9 522.5 526.6 | 34920 |
| valkey1 | challoc | 6.998 7.021 7.110 7.121 7.201 | 598636 |
| valkey1 | jemalloc | 7.174 7.240 7.245 7.255 7.431 | 599636 |
| valkey1 | glibc | 6.011 6.027 6.037 6.262 6.268 | 582172 |
| valkey1 | mimalloc | 5.358 5.397 5.569 5.574 5.670 | 599708 |
| valkey1 | tcmalloc | 8.427 8.445 8.506 8.867 8.878 | 601200 |
| valkey1-free | challoc | 7.136 7.136 7.162 7.179 7.197 | 598544 |
| valkey1-free | jemalloc | 6.923 7.032 7.040 7.061 7.151 | 599636 |
| valkey1-free | glibc | 6.024 6.133 6.151 6.153 6.195 | 582088 |
| valkey1-free | mimalloc | 5.326 5.349 5.429 5.535 5.538 | 599772 |
| valkey1-free | tcmalloc | 8.520 8.541 8.547 8.562 8.722 | 601328 |
| valkey1-usable | challoc | 5.931 5.982 5.989 6.033 6.038 | 598380 |
| valkey1-usable | jemalloc | 5.680 5.695 5.712 5.715 5.720 | 599508 |
| valkey1-usable | glibc | 2.736 2.741 2.751 2.780 2.787 | 582552 |
| valkey1-usable | mimalloc | 3.664 3.666 3.746 3.791 3.820 | 599692 |
| valkey1-usable | tcmalloc | 4.969 5.099 5.101 5.111 5.176 | 600816 |

## Medians

| program | unit | challoc | challoc RSS MiB | jemalloc | jemalloc RSS MiB | glibc | glibc RSS MiB | mimalloc | mimalloc RSS MiB | tcmalloc | tcmalloc RSS MiB | jemalloc vs challoc | glibc vs challoc | mimalloc vs challoc | tcmalloc vs challoc |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larsonN | M ops/s | 586.6 | 167.3 | 611.8 | 206.2 | 477.6 | 191.8 | 585.7 | 189.3 | 595.5 | 133.3 | -4.1% | +22.8% | +0.2% | -1.5% |
| xmallocN | M frees/s | 521.820 | 106.1 | 478.178 | 109.1 | 35.479 | 76.2 | 167.386 | 65.8 | 2.038 | 42.6 | +9.1% | +1370.8% | +211.7% | +25504.5% |
| cfrac | s | 4.449 | 2.8 | 4.425 | 4.9 | 4.306 | 2.6 | 4.269 | 3.9 | 4.384 | 8.0 | -0.5% | -3.2% | -4.0% | -1.5% |
| espresso | s | 3.668 | 3.6 | 3.740 | 6.2 | 3.897 | 3.8 | 3.546 | 9.9 | 3.576 | 8.8 | +2.0% | +6.2% | -3.3% | -2.5% |
| mstress | s | 0.377 | 317.0 | 0.647 | 291.6 | 0.493 | 528.3 | 0.290 | 535.7 | 0.422 | 280.2 | +71.6% | +30.8% | -23.1% | +11.9% |
| alloc-test1 | s | 2.614 | 13.0 | 2.515 | 12.3 | 3.025 | 15.2 | 2.428 | 15.8 | 2.492 | 15.6 | -3.8% | +15.7% | -7.1% | -4.7% |
| alloc-testN | s | 2.254 | 16.4 | 2.206 | 16.5 | 2.541 | 34.4 | 2.137 | 61.7 | 2.197 | 17.9 | -2.1% | +12.7% | -5.2% | -2.5% |
| glibc-simple | s | 2.291 | 2.2 | 2.528 | 4.4 | 4.158 | 3.4 | 1.751 | 3.5 | 1.729 | 7.5 | +10.3% | +81.5% | -23.6% | -24.5% |
| redis | k req/s | 501.3 | 29.0 | 410.7 | 31.1 | 533.0 | 31.4 | 465.8 | 45.4 | 521.9 | 34.1 | +22.1% | -5.9% | +7.6% | -3.9% |
| valkey1 | M ops/s | 7.110 | 584.6 | 7.245 | 585.6 | 6.037 | 568.5 | 5.569 | 585.7 | 8.506 | 587.1 | -1.9% | +17.8% | +27.7% | -16.4% |
| valkey1-free | M ops/s | 7.162 | 584.5 | 7.040 | 585.6 | 6.151 | 568.4 | 5.429 | 585.7 | 8.547 | 587.2 | +1.7% | +16.4% | +31.9% | -16.2% |
| valkey1-usable | M ops/s | 5.989 | 584.4 | 5.712 | 585.5 | 2.751 | 568.9 | 3.746 | 585.6 | 5.101 | 586.7 | +4.8% | +117.7% | +59.9% | +17.4% |

Positive percentages mean the variant did worse than challoc (slower or lower throughput).
