# challoc four-way, 2026-09-19, x86_64

- commit: 4864d4b
- date: 2026-09-19T23:01:24Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 8 CPUs visible to the container
- kernel: 6.18.49-r2-gcp-6.18 (Chainguard OS 6.18.49-r2-gcp-6.18, GCE c4-standard-8 (INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz), zone us-east4-a, 8 vCPU, podman --network=host --privileged), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 5.10 3.00 2.44; at end: 2.40 3.04 2.94; 1293 s total
- method: 3 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than challoc.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - challoc: 4864d4b3d673 built into target/ab/4864d4b3d673
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)
  - glibc: glibc malloc (no LD_PRELOAD, glibc 2.44)
  - mimalloc: /src/target/bench-bin/libmimalloc.so (v3.5.2)
  - tcmalloc: Wolfi's /usr/lib/libtcmalloc_minimal.so.4 (tcmalloc-minimal 2.18.1, gperftools without profiler hooks)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larsonN | challoc | 155.7 163.8 164.4 | 65568 |
| larsonN | jemalloc | 178.1 180.1 181.6 | 96788 |
| larsonN | glibc | 144.6 155.9 156.7 | 79220 |
| larsonN | mimalloc | 155.3 157.3 159.6 | 84688 |
| larsonN | tcmalloc | 180.8 190.8 191.6 | 56472 |
| xmallocN | challoc | 176.907 177.272 177.940 | 85448 |
| xmallocN | jemalloc | 173.908 177.548 177.905 | 152292 |
| xmallocN | glibc | 43.111 50.018 50.215 | 47056 |
| xmallocN | mimalloc | 167.823 168.651 170.167 | 67776 |
| xmallocN | tcmalloc | 3.499 3.522 3.569 | 43084 |
| cfrac | challoc | 4.323 4.329 4.341 | 3576 |
| cfrac | jemalloc | 4.140 4.166 4.167 | 9184 |
| cfrac | glibc | 4.016 4.020 4.035 | 3400 |
| cfrac | mimalloc | 4.003 4.014 4.027 | 4700 |
| cfrac | tcmalloc | 4.055 4.069 4.071 | 10520 |
| espresso | challoc | 4.792 4.807 4.807 | 4128 |
| espresso | jemalloc | 4.835 4.842 4.872 | 8976 |
| espresso | glibc | 5.154 5.156 5.167 | 2960 |
| espresso | mimalloc | 4.717 4.717 4.719 | 10576 |
| espresso | tcmalloc | 4.662 4.685 4.686 | 11428 |
| mstress | challoc | 0.475 0.479 0.513 | 180096 |
| mstress | jemalloc | 0.957 0.959 1.008 | 184332 |
| mstress | glibc | 0.701 0.719 0.752 | 266256 |
| mstress | mimalloc | 0.447 0.450 0.464 | 289008 |
| mstress | tcmalloc | 0.444 0.450 0.492 | 147388 |
| alloc-test1 | challoc | 3.383 3.386 3.412 | 16268 |
| alloc-test1 | jemalloc | 3.190 3.201 3.209 | 20040 |
| alloc-test1 | glibc | 3.754 3.770 3.803 | 14840 |
| alloc-test1 | mimalloc | 3.131 3.138 3.139 | 16952 |
| alloc-test1 | tcmalloc | 3.114 3.115 3.177 | 18580 |
| alloc-testN | challoc | 4.526 4.567 4.708 | 18240 |
| alloc-testN | jemalloc | 4.147 4.156 4.237 | 41680 |
| alloc-testN | glibc | 4.772 4.804 4.808 | 16432 |
| alloc-testN | mimalloc | 4.011 4.025 4.033 | 37512 |
| alloc-testN | tcmalloc | 4.323 4.330 4.413 | 19844 |
| glibc-simple | challoc | 2.191 2.248 2.267 | 2524 |
| glibc-simple | jemalloc | 2.161 2.167 2.176 | 12916 |
| glibc-simple | glibc | 3.694 3.707 3.714 | 2564 |
| glibc-simple | mimalloc | 1.700 1.711 1.733 | 4156 |
| glibc-simple | tcmalloc | 1.760 1.780 1.790 | 10032 |
| redis | challoc | 629.3 640.6 646.4 | 30780 |
| redis | jemalloc | 492.9 514.1 521.6 | 35760 |
| redis | glibc | 679.8 680.7 684.9 | 30088 |
| redis | mimalloc | 619.6 625.4 628.5 | 41232 |
| redis | tcmalloc | 666.7 678.4 680.3 | 36872 |
| valkey1 | challoc | 13.785 15.523 15.558 | 599672 |
| valkey1 | jemalloc | 16.514 16.647 17.186 | 613936 |
| valkey1 | glibc | 6.965 8.623 9.091 | 582244 |
| valkey1 | mimalloc | 3.898 3.905 3.969 | 600348 |
| valkey1 | tcmalloc | 14.033 15.854 16.394 | 604760 |
| valkey1-free | challoc | 14.821 15.716 15.775 | 599644 |
| valkey1-free | jemalloc | 14.343 15.504 15.802 | 614000 |
| valkey1-free | glibc | 7.417 8.778 8.959 | 582228 |
| valkey1-free | mimalloc | 3.877 3.960 3.968 | 600180 |
| valkey1-free | tcmalloc | 13.873 15.749 16.263 | 604928 |
| valkey1-usable | challoc | 10.744 10.862 11.013 | 599352 |
| valkey1-usable | jemalloc | 8.626 9.262 9.480 | 613944 |
| valkey1-usable | glibc | 5.606 6.552 6.878 | 581728 |
| valkey1-usable | mimalloc | 3.331 3.349 3.376 | 600172 |
| valkey1-usable | tcmalloc | 10.354 12.291 12.341 | 604236 |

## Medians

| program | unit | challoc | challoc RSS MiB | jemalloc | jemalloc RSS MiB | glibc | glibc RSS MiB | mimalloc | mimalloc RSS MiB | tcmalloc | tcmalloc RSS MiB | jemalloc vs challoc | glibc vs challoc | mimalloc vs challoc | tcmalloc vs challoc |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larsonN | M ops/s | 163.8 | 64.0 | 180.1 | 94.5 | 155.9 | 77.4 | 157.3 | 82.7 | 190.8 | 55.1 | -9.1% | +5.1% | +4.1% | -14.2% |
| xmallocN | M frees/s | 177.272 | 83.4 | 177.548 | 148.7 | 50.018 | 46.0 | 168.651 | 66.2 | 3.522 | 42.1 | -0.2% | +254.4% | +5.1% | +4933.3% |
| cfrac | s | 4.329 | 3.5 | 4.166 | 9.0 | 4.020 | 3.3 | 4.014 | 4.6 | 4.069 | 10.3 | -3.8% | -7.1% | -7.3% | -6.0% |
| espresso | s | 4.807 | 4.0 | 4.842 | 8.8 | 5.156 | 2.9 | 4.717 | 10.3 | 4.685 | 11.2 | +0.7% | +7.3% | -1.9% | -2.5% |
| mstress | s | 0.479 | 175.9 | 0.959 | 180.0 | 0.719 | 260.0 | 0.450 | 282.2 | 0.450 | 143.9 | +100.2% | +50.1% | -6.1% | -6.1% |
| alloc-test1 | s | 3.386 | 15.9 | 3.201 | 19.6 | 3.770 | 14.5 | 3.138 | 16.6 | 3.115 | 18.1 | -5.5% | +11.3% | -7.3% | -8.0% |
| alloc-testN | s | 4.567 | 17.8 | 4.156 | 40.7 | 4.804 | 16.0 | 4.025 | 36.6 | 4.330 | 19.4 | -9.0% | +5.2% | -11.9% | -5.2% |
| glibc-simple | s | 2.248 | 2.5 | 2.167 | 12.6 | 3.707 | 2.5 | 1.711 | 4.1 | 1.780 | 9.8 | -3.6% | +64.9% | -23.9% | -20.8% |
| redis | k req/s | 640.6 | 30.1 | 514.1 | 34.9 | 680.7 | 29.4 | 625.4 | 40.3 | 678.4 | 36.0 | +24.6% | -5.9% | +2.4% | -5.6% |
| valkey1 | M ops/s | 15.523 | 585.6 | 16.647 | 599.5 | 8.623 | 568.6 | 3.905 | 586.3 | 15.854 | 590.6 | -6.8% | +80.0% | +297.5% | -2.1% |
| valkey1-free | M ops/s | 15.716 | 585.6 | 15.504 | 599.6 | 8.778 | 568.6 | 3.960 | 586.1 | 15.749 | 590.8 | +1.4% | +79.0% | +296.9% | -0.2% |
| valkey1-usable | M ops/s | 10.862 | 585.3 | 9.262 | 599.6 | 6.552 | 568.1 | 3.349 | 586.1 | 12.291 | 590.1 | +17.3% | +65.8% | +224.3% | -11.6% |

Positive percentages mean the variant did worse than challoc (slower or lower throughput).
