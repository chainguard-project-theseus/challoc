# challoc four-way, 2026-09-19, aarch64

- commit: 4864d4b
- date: 2026-09-19T22:55:59Z
- host: Neoverse-V2 (Axion), 8 CPUs visible to the container
- kernel: 6.18.49-r2-gcp-6.18 (Chainguard OS 6.18.49-r2-gcp-6.18, GCE c4a-standard-8 (Neoverse-V2 (Axion)), zone us-east4-a, 8 vCPU, podman --network=host --privileged), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 3.72 2.29 1.97; at end: 2.29 2.68 2.55; 1219 s total
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
| larsonN | challoc | 301.1 301.9 304.4 | 91572 |
| larsonN | jemalloc | 309.8 315.6 316.8 | 153436 |
| larsonN | glibc | 226.9 229.4 229.8 | 99072 |
| larsonN | mimalloc | 290.4 295.5 300.4 | 106660 |
| larsonN | tcmalloc | 302.2 303.9 309.0 | 77948 |
| xmallocN | challoc | 344.156 344.172 344.359 | 95348 |
| xmallocN | jemalloc | 241.087 245.094 246.062 | 134864 |
| xmallocN | glibc | 14.944 17.237 17.350 | 59064 |
| xmallocN | mimalloc | 77.900 79.968 81.228 | 67228 |
| xmallocN | tcmalloc | 2.391 2.519 2.792 | 42140 |
| cfrac | challoc | 4.464 4.464 4.465 | 2868 |
| cfrac | jemalloc | 4.419 4.422 4.438 | 8128 |
| cfrac | glibc | 4.299 4.301 4.310 | 2676 |
| cfrac | mimalloc | 4.264 4.273 4.296 | 3900 |
| cfrac | tcmalloc | 4.384 4.385 4.388 | 9876 |
| espresso | challoc | 3.658 3.660 3.661 | 3696 |
| espresso | jemalloc | 3.737 3.739 3.747 | 16372 |
| espresso | glibc | 3.883 3.897 3.904 | 4464 |
| espresso | mimalloc | 3.548 3.553 3.558 | 10152 |
| espresso | tcmalloc | 3.566 3.572 3.578 | 10744 |
| mstress | challoc | 0.339 0.342 0.342 | 159468 |
| mstress | jemalloc | 0.578 0.579 0.589 | 199916 |
| mstress | glibc | 0.431 0.435 0.447 | 231884 |
| mstress | mimalloc | 0.281 0.282 0.285 | 288552 |
| mstress | tcmalloc | 0.374 0.378 0.381 | 151840 |
| alloc-test1 | challoc | 2.695 2.710 2.712 | 15360 |
| alloc-test1 | jemalloc | 2.556 2.563 2.610 | 21196 |
| alloc-test1 | glibc | 3.134 3.135 3.183 | 15516 |
| alloc-test1 | mimalloc | 2.552 2.569 2.591 | 16200 |
| alloc-test1 | tcmalloc | 2.526 2.539 2.568 | 19928 |
| alloc-testN | challoc | 2.322 2.323 2.338 | 17364 |
| alloc-testN | jemalloc | 2.228 2.228 2.374 | 56392 |
| alloc-testN | glibc | 2.582 2.593 2.597 | 20320 |
| alloc-testN | mimalloc | 2.169 2.175 2.184 | 36724 |
| alloc-testN | tcmalloc | 2.236 2.238 2.239 | 21200 |
| glibc-simple | challoc | 2.165 2.172 2.201 | 2376 |
| glibc-simple | jemalloc | 2.515 2.518 2.523 | 12304 |
| glibc-simple | glibc | 4.138 4.138 4.139 | 3404 |
| glibc-simple | mimalloc | 1.741 1.742 1.776 | 3716 |
| glibc-simple | tcmalloc | 1.697 1.700 1.710 | 9100 |
| redis | challoc | 507.4 509.7 513.1 | 29812 |
| redis | jemalloc | 404.2 411.9 413.2 | 36588 |
| redis | glibc | 533.9 537.1 540.0 | 29380 |
| redis | mimalloc | 464.3 464.7 465.3 | 40312 |
| redis | tcmalloc | 527.4 528.5 529.1 | 36228 |
| valkey1 | challoc | 7.146 7.151 7.183 | 598696 |
| valkey1 | jemalloc | 7.138 7.436 7.516 | 608984 |
| valkey1 | glibc | 5.559 5.569 5.841 | 582720 |
| valkey1 | mimalloc | 3.915 4.167 4.230 | 599756 |
| valkey1 | tcmalloc | 6.281 6.429 6.439 | 603600 |
| valkey1-free | challoc | 6.984 7.078 7.078 | 598692 |
| valkey1-free | jemalloc | 7.111 7.333 7.392 | 608980 |
| valkey1-free | glibc | 5.757 5.787 5.793 | 582168 |
| valkey1-free | mimalloc | 4.030 4.103 4.208 | 599764 |
| valkey1-free | tcmalloc | 6.321 6.415 6.507 | 603284 |
| valkey1-usable | challoc | 5.925 5.998 6.061 | 598504 |
| valkey1-usable | jemalloc | 5.992 6.078 6.206 | 608980 |
| valkey1-usable | glibc | 2.533 2.609 2.619 | 582672 |
| valkey1-usable | mimalloc | 3.019 3.050 3.074 | 599756 |
| valkey1-usable | tcmalloc | 4.668 4.678 4.687 | 603344 |

## Medians

| program | unit | challoc | challoc RSS MiB | jemalloc | jemalloc RSS MiB | glibc | glibc RSS MiB | mimalloc | mimalloc RSS MiB | tcmalloc | tcmalloc RSS MiB | jemalloc vs challoc | glibc vs challoc | mimalloc vs challoc | tcmalloc vs challoc |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larsonN | M ops/s | 301.9 | 89.4 | 315.6 | 149.8 | 229.4 | 96.8 | 295.5 | 104.2 | 303.9 | 76.1 | -4.3% | +31.6% | +2.2% | -0.7% |
| xmallocN | M frees/s | 344.172 | 93.1 | 245.094 | 131.7 | 17.237 | 57.7 | 79.968 | 65.7 | 2.519 | 41.2 | +40.4% | +1896.7% | +330.4% | +13563.0% |
| cfrac | s | 4.464 | 2.8 | 4.422 | 7.9 | 4.301 | 2.6 | 4.273 | 3.8 | 4.385 | 9.6 | -0.9% | -3.7% | -4.3% | -1.8% |
| espresso | s | 3.660 | 3.6 | 3.739 | 16.0 | 3.897 | 4.4 | 3.553 | 9.9 | 3.572 | 10.5 | +2.2% | +6.5% | -2.9% | -2.4% |
| mstress | s | 0.342 | 155.7 | 0.579 | 195.2 | 0.435 | 226.4 | 0.282 | 281.8 | 0.378 | 148.3 | +69.3% | +27.2% | -17.5% | +10.5% |
| alloc-test1 | s | 2.710 | 15.0 | 2.563 | 20.7 | 3.135 | 15.2 | 2.569 | 15.8 | 2.539 | 19.5 | -5.4% | +15.7% | -5.2% | -6.3% |
| alloc-testN | s | 2.323 | 17.0 | 2.228 | 55.1 | 2.593 | 19.8 | 2.175 | 35.9 | 2.238 | 20.7 | -4.1% | +11.6% | -6.4% | -3.7% |
| glibc-simple | s | 2.172 | 2.3 | 2.518 | 12.0 | 4.138 | 3.3 | 1.742 | 3.6 | 1.700 | 8.9 | +15.9% | +90.5% | -19.8% | -21.7% |
| redis | k req/s | 509.7 | 29.1 | 411.9 | 35.7 | 537.1 | 28.7 | 464.7 | 39.4 | 528.5 | 35.4 | +23.7% | -5.1% | +9.7% | -3.6% |
| valkey1 | M ops/s | 7.151 | 584.7 | 7.436 | 594.7 | 5.569 | 569.1 | 4.167 | 585.7 | 6.429 | 589.5 | -3.8% | +28.4% | +71.6% | +11.2% |
| valkey1-free | M ops/s | 7.078 | 584.7 | 7.333 | 594.7 | 5.787 | 568.5 | 4.103 | 585.7 | 6.415 | 589.1 | -3.5% | +22.3% | +72.5% | +10.3% |
| valkey1-usable | M ops/s | 5.998 | 584.5 | 6.078 | 594.7 | 2.609 | 569.0 | 3.050 | 585.7 | 4.678 | 589.2 | -1.3% | +129.9% | +96.7% | +28.2% |

Positive percentages mean the variant did worse than challoc (slower or lower throughput).
