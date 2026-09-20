# challoc per-CPU campaign (percpu), 2026-09-20, x86_64

- commit: 0c60b1a
- date: 2026-09-20T04:52:37Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (GCE x86_64 lane VM), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- load average at start: 5.85 3.43 1.46; at end: 6.91 8.02 6.65; 1367 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than default.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; python1 / python1-malloc: bench/probes/python/json_bench.py on python3 with PYTHONMALLOC unset / =malloc, wall seconds; others: wall seconds
- variants:
  - default: working tree (HEAD 0c60b1a) built into target/ab/tree
  - percpu: working tree (HEAD 0c60b1a) built into target/ab/tree, MALLOC_CONF=percpu_arena:percpu
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | default | 44.0 44.4 45.1 45.1 45.2 | 17408 |
| larson1 | percpu | 42.9 43.0 43.1 43.2 43.3 | 18264 |
| larson1 | jemalloc | 43.2 44.6 44.9 45.3 45.3 | 18404 |
| larson10 | default | 279.6 280.7 281.2 284.3 290.0 | 97864 |
| larson10 | percpu | 220.1 231.7 233.3 239.6 239.9 | 96448 |
| larson10 | jemalloc | 289.4 289.8 289.9 290.5 291.1 | 94816 |
| larson16 | default | 302.6 305.2 307.3 308.3 312.8 | 122500 |
| larson16 | percpu | 239.5 250.8 252.4 255.6 257.4 | 110924 |
| larson16 | jemalloc | 331.4 332.1 335.1 335.4 339.9 | 130860 |
| larson64 | default | 330.7 339.0 339.1 342.0 342.2 | 485656 |
| larson64 | percpu | 252.0 261.6 261.6 261.7 261.8 | 276136 |
| larson64 | jemalloc | 357.7 370.3 372.7 372.9 373.3 | 424876 |
| xmalloc | default | 320.122 321.805 330.647 331.448 332.630 | 54164 |
| xmalloc | percpu | 287.952 287.967 296.144 296.193 296.464 | 59100 |
| xmalloc | jemalloc | 321.933 327.092 328.042 331.067 331.854 | 94048 |
| alloc-test1 | default | 3.353 3.394 3.401 3.434 3.450 | 14244 |
| alloc-test1 | percpu | 3.238 3.393 3.406 3.419 3.553 | 14124 |
| alloc-test1 | jemalloc | 3.083 3.089 3.097 3.226 3.228 | 13600 |
| alloc-testN | default | 4.493 4.538 4.563 4.652 4.656 | 17740 |
| alloc-testN | percpu | 4.728 4.757 4.761 4.777 4.939 | 17780 |
| alloc-testN | jemalloc | 4.107 4.131 4.132 4.136 4.326 | 17312 |
| cfrac | default | 4.123 4.166 4.180 4.316 4.325 | 3660 |
| cfrac | percpu | 4.206 4.347 4.359 4.410 4.596 | 3748 |
| cfrac | jemalloc | 3.924 3.968 3.977 4.147 4.158 | 5848 |
| espresso | default | 4.611 4.671 4.744 4.819 4.829 | 4224 |
| espresso | percpu | 4.586 4.717 4.742 4.771 4.927 | 4272 |
| espresso | jemalloc | 4.645 4.650 4.661 4.831 4.848 | 6436 |
| mstress | default | 0.632 0.632 0.638 0.643 0.645 | 345740 |
| mstress | percpu | 0.638 0.639 0.640 0.646 0.655 | 324440 |
| mstress | jemalloc | 1.072 1.075 1.078 1.078 1.094 | 290884 |
| glibc-simple | default | 2.105 2.109 2.182 2.191 2.199 | 2664 |
| glibc-simple | percpu | 2.248 2.268 2.292 2.301 2.366 | 2768 |
| glibc-simple | jemalloc | 2.084 2.086 2.092 2.180 2.204 | 5000 |
| redis | default | 632.1 632.5 637.3 637.8 638.6 | 30828 |
| redis | percpu | 621.9 622.7 623.4 625.4 627.0 | 30880 |
| redis | jemalloc | 493.1 503.0 507.9 512.3 513.1 | 33124 |
| valkey1 | default | 14.089 14.133 14.488 14.613 14.975 | 599700 |
| valkey1 | percpu | 10.667 10.706 10.803 10.939 10.990 | 599432 |
| valkey1 | jemalloc | 14.240 14.580 14.667 14.753 14.788 | 606696 |
| valkey1-free | default | 14.241 14.322 14.361 14.830 14.843 | 599672 |
| valkey1-free | percpu | 11.345 11.568 11.572 11.633 11.646 | 599480 |
| valkey1-free | jemalloc | 12.832 12.937 13.046 13.115 13.125 | 606680 |

## Medians

| program | unit | default | default RSS MiB | percpu | percpu RSS MiB | jemalloc | jemalloc RSS MiB | percpu vs default | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.1 | 17.0 | 43.1 | 17.8 | 44.9 | 18.0 | +4.6% | +0.4% |
| larson10 | M ops/s | 281.2 | 95.6 | 233.3 | 94.2 | 289.9 | 92.6 | +20.5% | -3.0% |
| larson16 | M ops/s | 307.3 | 119.6 | 252.4 | 108.3 | 335.1 | 127.8 | +21.8% | -8.3% |
| larson64 | M ops/s | 339.1 | 474.3 | 261.6 | 269.7 | 372.7 | 414.9 | +29.6% | -9.0% |
| xmalloc | M frees/s | 330.647 | 52.9 | 296.144 | 57.7 | 328.042 | 91.8 | +11.7% | +0.8% |
| alloc-test1 | s | 3.401 | 13.9 | 3.406 | 13.8 | 3.097 | 13.3 | +0.1% | -8.9% |
| alloc-testN | s | 4.563 | 17.3 | 4.761 | 17.4 | 4.132 | 16.9 | +4.3% | -9.4% |
| cfrac | s | 4.180 | 3.6 | 4.359 | 3.7 | 3.977 | 5.7 | +4.3% | -4.9% |
| espresso | s | 4.744 | 4.1 | 4.742 | 4.2 | 4.661 | 6.3 | -0.0% | -1.7% |
| mstress | s | 0.638 | 337.6 | 0.640 | 316.8 | 1.078 | 284.1 | +0.3% | +69.0% |
| glibc-simple | s | 2.182 | 2.6 | 2.292 | 2.7 | 2.092 | 4.9 | +5.0% | -4.1% |
| redis | k req/s | 637.3 | 30.1 | 623.4 | 30.2 | 507.9 | 32.3 | +2.2% | +25.5% |
| valkey1 | M ops/s | 14.488 | 585.6 | 10.803 | 585.4 | 14.667 | 592.5 | +34.1% | -1.2% |
| valkey1-free | M ops/s | 14.361 | 585.6 | 11.572 | 585.4 | 13.046 | 592.5 | +24.1% | +10.1% |

Positive percentages mean the variant did worse than default (slower or lower throughput).
