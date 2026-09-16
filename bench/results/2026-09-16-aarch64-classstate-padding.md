# challoc Phase 0 benchmarks, 2026-09-16, aarch64

- commit: 8752ac8 plus the ClassState/Shared layout change committed right after this run (Phase 0 acceptance remediation step 1); before-row: `2026-09-16-aarch64.md`
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
| cfrac | s | 2.412 | 2.5 | 2.386 | 9.9 | +1.1% | -75.1% |
| espresso | s | 2.433 | 3.2 | 2.628 | 16.6 | -7.4% | -80.6% |
| larson | M ops/s | 200.5 | 62.3 | 290.7 | 168.2 | +45.0% | -62.9% |
| mstress | s | 0.262 | 169.6 | 0.370 | 191.7 | -29.2% | -11.5% |
| xmalloc-test | M frees/s | 81.226 | 30.3 | 252.259 | 136.0 | +210.6% | -77.7% |
| alloc-test1 | s | 2.145 | 14.6 | 1.927 | 22.2 | +11.3% | -34.1% |
| alloc-testN | s | 3.086 | 16.2 | 2.710 | 87.8 | +13.9% | -81.5% |
| glibc-simple | s | 1.525 | 1.7 | 1.627 | 11.9 | -6.3% | -85.8% |
| redis | k req/s | 748.5 | 28.7 | 605.3 | 37.7 | -19.1% | -23.7% |

Positive percentages mean challoc did worse than jemalloc (slower, lower throughput, or more RSS).

## Raw runs

Columns: elapsed_s user_s sys_s maxrss_kb exit. First line of each block is the discarded warm-up. Throughput benches also list the per-run score.

### cfrac / challoc

```
2.412 2.411 0.001 2524 0
2.403 2.402 0.001 2524 0
2.412 2.412 0.000 2524 0
2.412 2.410 0.002 2524 0
2.428 2.425 0.002 2524 0
2.417 2.416 0.001 2524 0
```

### cfrac / jemalloc

```
2.601 2.599 0.001 10160 0
2.360 2.359 0.000 10148 0
2.386 2.384 0.002 10148 0
2.574 2.573 0.001 10160 0
2.368 2.367 0.001 10148 0
2.625 2.624 0.001 10160 0
```

### espresso / challoc

```
2.560 2.550 0.009 3288 0
2.441 2.432 0.009 3300 0
2.433 2.421 0.012 3300 0
2.433 2.427 0.006 3300 0
2.436 2.433 0.003 3280 0
2.433 2.424 0.009 3300 0
```

### espresso / jemalloc

```
2.599 2.591 0.008 17060 0
2.615 2.603 0.012 17492 0
2.631 2.627 0.004 16992 0
2.636 2.631 0.005 16904 0
2.595 2.587 0.008 16944 0
2.628 2.618 0.010 17068 0
```

### larson / challoc

```
7.029 49.176 0.335 67244 0
7.034 49.027 0.343 63820 0
7.036 49.312 0.317 64448 0
7.039 49.302 0.268 66420 0
7.037 49.034 0.319 63832 0
7.038 49.171 0.292 63800 0
score (M ops/s): 210.7 200.5 203.8 202.7 199.3 199.4 
```

### larson / jemalloc

```
7.032 49.458 0.261 172780 0
7.028 49.174 0.279 172196 0
7.030 49.018 0.344 171744 0
7.033 49.355 0.327 171860 0
7.037 49.352 0.313 182556 0
7.026 49.169 0.341 173208 0
score (M ops/s): 291.1 291.9 288.1 292.2 290.7 283.0 
```

### mstress / challoc

```
0.275 1.183 0.091 173848 0
0.259 1.163 0.061 173700 0
0.262 1.163 0.068 177132 0
0.259 1.163 0.055 170012 0
0.263 1.124 0.086 170224 0
0.262 1.177 0.060 175784 0
```

### mstress / jemalloc

```
0.371 1.056 0.466 226808 0
0.368 1.059 0.490 196340 0
0.370 1.031 0.527 208308 0
0.371 1.027 0.523 193848 0
0.374 1.000 0.556 208308 0
0.365 1.045 0.505 190168 0
```

### xmalloc-test / challoc

```
5.002 37.846 6.446 26056 0
5.002 37.850 6.451 27208 0
5.002 37.633 6.484 37700 0
5.005 38.004 6.334 29044 0
5.002 38.021 6.343 32072 0
5.002 37.727 6.575 31048 0
score (M frees/s): 79.712 81.226 81.305 81.559 80.329 80.757 
```

### xmalloc-test / jemalloc

```
5.007 46.490 0.859 142068 0
5.009 46.890 0.813 139212 0
5.009 46.097 0.879 177424 0
5.008 46.853 0.924 138196 0
5.007 46.234 0.863 143976 0
5.009 46.924 0.846 139228 0
score (M frees/s): 259.349 257.019 248.688 255.092 249.732 252.259 
```

### alloc-test1 / challoc

```
2.228 2.226 0.001 14972 0
2.150 2.149 0.000 14968 0
2.138 2.138 0.000 14972 0
2.140 2.137 0.002 14972 0
2.145 2.143 0.001 14972 0
2.147 2.144 0.003 14972 0
```

### alloc-test1 / jemalloc

```
1.930 1.929 0.001 22696 0
1.917 1.916 0.001 22692 0
1.924 1.921 0.002 22692 0
1.927 1.927 0.000 22716 0
1.932 1.932 0.000 22708 0
1.932 1.930 0.001 22708 0
```

### alloc-testN / challoc

```
2.924 27.898 0.007 16624 0
2.943 28.787 0.006 16756 0
3.131 29.709 0.047 16368 0
3.118 29.656 0.012 16624 0
3.019 29.770 0.014 16748 0
3.086 30.089 0.022 16496 0
```

### alloc-testN / jemalloc

```
2.767 26.781 0.006 89872 0
2.710 26.503 0.009 89852 0
2.710 26.406 0.005 89868 0
2.885 26.766 0.021 89868 0
2.732 26.626 0.006 89888 0
2.708 26.663 0.005 89848 0
```

### glibc-simple / challoc

```
1.542 1.538 0.002 1732 0
1.529 1.525 0.002 1732 0
1.523 1.520 0.002 1732 0
1.526 1.523 0.002 1728 0
1.521 1.521 0.000 1732 0
1.525 1.517 0.006 1784 0
```

### glibc-simple / jemalloc

```
1.617 1.616 0.000 12160 0
1.627 1.624 0.001 12176 0
1.619 1.617 0.002 12180 0
1.623 1.619 0.003 12308 0
1.630 1.627 0.001 12180 0
1.630 1.627 0.002 12176 0
```

### redis / challoc

```
1.334 0.081 0.158 3264 0
1.337 0.079 0.159 4876 0
1.342 0.063 0.181 3284 0
1.338 0.071 0.171 3284 0
1.328 0.090 0.152 3284 0
1.335 0.070 0.169 4876 0
score (k req/s): 750.2 748.5 743.5 747.9 753.6 749.6 
server:
1.448 1.244 0.094 29304 0
1.452 1.235 0.105 29304 0
1.447 1.245 0.100 29432 0
1.452 1.255 0.087 29428 0
1.442 1.235 0.097 29432 0
1.443 1.238 0.100 29424 0
```

### redis / jemalloc

```
1.629 0.086 0.157 3272 0
1.642 0.073 0.165 4872 0
1.646 0.071 0.172 4872 0
1.686 0.074 0.167 3148 0
1.676 0.068 0.169 3276 0
1.653 0.071 0.167 3264 0
score (k req/s): 614.6 609.4 608.3 593.8 597.0 605.3 
server:
1.742 1.539 0.091 38560 0
1.756 1.547 0.097 38560 0
1.756 1.562 0.086 38572 0
1.793 1.594 0.094 38564 0
1.789 1.578 0.100 38580 0
1.767 1.572 0.083 38576 0
```


## larson profile under challoc (aarch64, 20 s run, SIGPROF sampler at 1000 Hz of CPU time)

```
total samples 2758; in libchalloc.so 1224 (44.4%)

by module:
     1297  47.0%  main
     1224  44.4%  libchalloc.so
      224   8.1%  libstdc++.so.6
       12   0.4%  libc.so.6
        1   0.0%  ld-linux-aarch64.so.1

top 10 challoc symbols (rmw = samples on lock/atomic RMW instructions inside that symbol; outlined LSE helpers count whole):
      753  61.5%  rmw=0     free
      275  22.5%  rmw=0     malloc
       50   4.1%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::claim
       36   2.9%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::release_word
       20   1.6%  rmw=0     <challoc[c0770b06928205b9]::lock::RawLock>::lock_slow
       17   1.4%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::release
       14   1.1%  rmw=14    __aarch64_cas4_acq
       12   1.0%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::dec_live
       11   0.9%  rmw=11    __aarch64_swp4_rel
       11   0.9%  rmw=11    __aarch64_ldclr8_rel

samples on lock/atomic RMW instructions anywhere in challoc: 46 (3.76% of challoc samples)

top 15 challoc instructions:
       87   7.1%      a35c  ldr      free
       74   6.0%      a344  lsr      free
       55   4.5%      a34c  ldr      free
       53   4.3%      a39c  add      free
       46   3.8%      a2b4  lsl      free
       44   3.6%      acb0  str      malloc
       40   3.3%      acc4  ret      malloc
       37   3.0%      a290  sub      free
       36   2.9%      aca8  ldr      malloc
       27   2.2%      ac0c  cmp      malloc
       27   2.2%      a324  adrp     free
       25   2.0%      a2f8  cmp      free
       25   2.0%      a270  sub      free
       23   1.9%      a310  add      free
       22   1.8%      a240  cmp      free
```

larson output:

```
Throughput = 202924890 operations per second, relative time: 4.928s.
```
