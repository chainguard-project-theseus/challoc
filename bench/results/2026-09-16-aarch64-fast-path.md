# challoc Phase 0 benchmarks, 2026-09-16, aarch64

- commit: 7da335d (fast paths trimmed and `heap::MAX_CAP` 1024 in a1976ef, fork-handler signal blocking in 7da335d; Phase 0 acceptance remediation step 4); before-row: `2026-09-16-aarch64-meta-line-128.md`
- host: Apple M4, 10 CPUs visible to the container
- kernel: 6.10.14-linuxkit (Docker Desktop VM), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- redis 6.2.7
- mimalloc v3.5.2
- gcc-15-15.3.0-r1
- gcc-15-default-15.3.0-r1
- glibc-2.44-2.44-r6
- glibc-2.44-dev-2.44-r6
- glibc-2.44-locale-posix-2.44-r6
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- cmake 4.4.3
- protocol: 5 runs after one discarded warm-up; median wall time (s) and max RSS (MiB) from wait4 rusage via bench/rt; identical binaries, only LD_PRELOAD changes
- larson, mstress, xmalloc-test and alloc-testN use 10 threads
- scores: wall time in seconds (lower is better) except larson (M ops/s), xmalloc-test (M frees/s) and redis (k requests/s from redis-benchmark -n 1000000 -P 16; RSS is redis-server's), which are higher-is-better

## Medians

| benchmark | unit | challoc score | challoc RSS MiB | jemalloc score | jemalloc RSS MiB | score vs jemalloc | RSS vs jemalloc |
|---|---|---:|---:|---:|---:|---:|---:|
| cfrac | s | 2.284 | 2.5 | 2.389 | 9.8 | -4.4% | -74.9% |
| espresso | s | 2.334 | 3.2 | 2.558 | 16.8 | -8.8% | -80.8% |
| larson | M ops/s | 243.4 | 75.1 | 291.5 | 167.5 | +19.8% | -55.2% |
| mstress | s | 0.254 | 173.7 | 0.371 | 200.9 | -31.5% | -13.6% |
| xmalloc-test | M frees/s | 553.674 | 38.9 | 251.156 | 141.0 | -54.6% | -72.4% |
| alloc-test1 | s | 2.082 | 14.7 | 1.929 | 22.2 | +7.9% | -33.7% |
| alloc-testN | s | 2.880 | 16.9 | 2.722 | 87.8 | +5.8% | -80.8% |
| glibc-simple | s | 1.355 | 1.8 | 1.623 | 12.0 | -16.5% | -84.8% |
| redis | k req/s | 754.1 | 28.7 | 578.4 | 37.6 | -23.3% | -23.5% |

Positive percentages mean challoc did worse than jemalloc (slower, lower throughput, or more RSS).

## Raw runs

Columns: elapsed_s user_s sys_s maxrss_kb exit. First line of each block is the discarded warm-up. Throughput benches also list the per-run score.

### cfrac / challoc

```
2.277 2.275 0.001 2524 0
2.286 2.285 0.001 2512 0
2.280 2.280 0.000 2524 0
2.280 2.279 0.001 2524 0
2.284 2.283 0.001 2524 0
2.285 2.282 0.003 2516 0
```

### cfrac / jemalloc

```
2.580 2.580 0.000 10040 0
2.356 2.354 0.001 10048 0
2.368 2.367 0.001 10048 0
2.576 2.575 0.001 10056 0
2.570 2.569 0.001 10056 0
2.389 2.389 0.000 10044 0
```

### espresso / challoc

```
2.325 2.318 0.006 3300 0
2.353 2.346 0.007 3300 0
2.334 2.327 0.007 3300 0
2.330 2.325 0.005 3300 0
2.375 2.365 0.010 3300 0
2.331 2.323 0.008 3300 0
```

### espresso / jemalloc

```
2.548 2.535 0.013 17072 0
2.530 2.522 0.008 17212 0
2.574 2.568 0.006 17568 0
2.558 2.553 0.005 17728 0
2.566 2.559 0.006 15084 0
2.558 2.552 0.006 15084 0
```

### larson / challoc

```
7.033 48.892 0.358 76668 0
7.026 49.182 0.320 76572 0
7.032 49.380 0.252 76424 0
7.033 49.088 0.316 77756 0
7.035 49.190 0.296 76868 0
7.040 49.414 0.256 78264 0
score (M ops/s): 246.5 243.4 244.3 239.2 232.6 243.8 
```

### larson / jemalloc

```
7.032 49.398 0.270 174928 0
7.044 49.311 0.286 174596 0
7.030 49.304 0.336 172172 0
7.039 49.387 0.307 170124 0
7.032 49.116 0.396 171560 0
7.032 49.176 0.343 171356 0
score (M ops/s): 294.7 292.5 291.5 291.3 288.6 294.2 
```

### mstress / challoc

```
0.270 1.126 0.090 174996 0
0.251 1.122 0.056 179680 0
0.253 1.104 0.070 180200 0
0.254 1.110 0.072 175116 0
0.257 1.093 0.072 176088 0
0.260 1.114 0.081 177828 0
```

### mstress / jemalloc

```
0.369 1.061 0.507 215616 0
0.371 1.035 0.536 223296 0
0.387 1.072 0.558 205712 0
0.373 1.037 0.510 203376 0
0.369 1.035 0.526 207376 0
0.371 1.037 0.521 186796 0
```

### xmalloc-test / challoc

```
5.004 44.392 0.827 39936 0
5.003 46.719 0.744 40052 0
5.004 46.720 0.721 40120 0
5.005 46.783 0.697 39860 0
5.004 46.580 0.660 39880 0
5.004 46.975 0.639 39676 0
score (M frees/s): 512.990 561.695 553.528 556.392 551.282 553.674 
```

### xmalloc-test / jemalloc

```
5.010 46.022 0.903 138152 0
5.008 46.719 0.886 136248 0
5.010 46.310 0.877 145516 0
5.003 46.560 0.888 144348 0
5.008 46.437 0.873 145984 0
5.011 46.771 0.824 143792 0
score (M frees/s): 248.988 253.036 251.156 252.144 250.787 250.936 
```

### alloc-test1 / challoc

```
2.132 2.129 0.003 15104 0
2.146 2.144 0.001 15100 0
2.082 2.081 0.001 15100 0
2.065 2.062 0.002 15084 0
2.072 2.067 0.004 15084 0
2.083 2.080 0.002 15084 0
```

### alloc-test1 / jemalloc

```
1.938 1.936 0.001 22760 0
1.927 1.926 0.001 22780 0
1.929 1.928 0.001 22748 0
1.923 1.921 0.001 22748 0
1.929 1.927 0.001 22796 0
1.934 1.933 0.001 22748 0
```

### alloc-testN / challoc

```
2.742 26.335 0.017 17380 0
2.835 27.303 0.009 17388 0
2.835 27.667 0.013 17204 0
2.981 28.283 0.047 17264 0
2.923 28.186 0.014 17388 0
2.880 28.077 0.005 17264 0
```

### alloc-testN / jemalloc

```
2.692 26.366 0.019 89936 0
2.722 26.323 0.016 89964 0
2.710 26.613 0.008 89952 0
2.828 26.814 0.006 89944 0
2.736 26.495 0.030 89952 0
2.685 26.359 0.012 89932 0
```

### glibc-simple / challoc

```
1.371 1.366 0.003 1860 0
1.365 1.361 0.002 1732 0
1.353 1.350 0.001 1724 0
1.353 1.351 0.000 1860 0
1.359 1.356 0.001 1860 0
1.355 1.352 0.001 1860 0
```

### glibc-simple / jemalloc

```
1.622 1.620 0.000 12252 0
1.618 1.616 0.001 12252 0
1.620 1.618 0.001 12248 0
1.623 1.620 0.002 12380 0
1.625 1.622 0.002 12252 0
1.623 1.621 0.001 12252 0
```

### redis / challoc

```
1.334 0.077 0.163 4956 0
1.326 0.084 0.154 3284 0
1.328 0.072 0.166 4876 0
1.327 0.090 0.149 4532 0
1.330 0.094 0.142 4860 0
1.328 0.083 0.158 4956 0
score (k req/s): 750.8 755.3 753.6 754.1 753.0 754.1 
server:
1.449 1.254 0.084 29432 0
1.437 1.242 0.087 29432 0
1.436 1.247 0.084 29432 0
1.431 1.243 0.086 29432 0
1.444 1.244 0.090 29432 0
1.432 1.224 0.106 29432 0
```

### redis / jemalloc

```
1.685 0.076 0.164 3264 0
1.639 0.071 0.164 4664 0
1.680 0.078 0.160 4956 0
1.731 0.083 0.160 3284 0
1.745 0.075 0.170 3264 0
1.831 0.080 0.160 4880 0
score (k req/s): 593.8 610.5 595.9 578.4 573.7 546.4 
server:
1.790 1.609 0.077 38472 0
1.750 1.553 0.088 38468 0
1.789 1.588 0.094 38528 0
1.838 1.654 0.079 38472 0
1.854 1.655 0.091 38452 0
1.944 1.741 0.092 38472 0
```


## larson profile under challoc (aarch64, 20 s run, SIGPROF sampler at 1000 Hz of CPU time)

```
total samples 746; in libchalloc.so 217 (29.1%)

by module:
      420  56.3%  main
      217  29.1%  libchalloc.so
      106  14.2%  libstdc++.so.6
        3   0.4%  libc.so.6

top 10 challoc symbols (rmw = samples on lock/atomic RMW instructions inside that symbol; outlined LSE helpers count whole):
      146  67.3%  rmw=0     free
       58  26.7%  rmw=0     malloc
        7   3.2%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::claim
        2   0.9%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::release_word
        1   0.5%  rmw=0     <challoc[c0770b06928205b9]::heap::Heap>::ensure_cache
        1   0.5%  rmw=1     __aarch64_ldadd4_acq_rel
        1   0.5%  rmw=0     <challoc[c0770b06928205b9]::bitmap::TwoLevelBitmap>::lowest_set_from
        1   0.5%  rmw=0     core[797e91ca1b8870b2]::slice::sort::unstable::quicksort::quicksort::<*mut u8, <*mut u8 as core[797e91ca1b8870b2]::cmp::PartialOrd>::lt>

samples on lock/atomic RMW instructions anywhere in challoc: 1 (0.46% of challoc samples)

top 15 challoc instructions:
       23  10.6%      ab78  lsr      free
       17   7.8%      ab88  cmp      free
       17   7.8%      ab80  ldr      free
       14   6.5%      abbc  str      free
       12   5.5%      aec4  str      malloc
       11   5.1%      ab18  lsl      free
       11   5.1%      ae34  cmp      malloc
       10   4.6%      aad4  sub      free
       10   4.6%      ae08  cmp      malloc
        8   3.7%      aeb8  ldr      malloc
        7   3.2%      ab90  ldr      free
        7   3.2%      ab30  ldr      free
        6   2.8%      aaf4  sub      free
        6   2.8%      ae80  ldr      malloc
        5   2.3%      aae4  adrp     free
```

larson output:

```
Throughput = 243698967 operations per second, relative time: 4.103s.
```
