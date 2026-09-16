# challoc Phase 0 benchmarks, 2026-09-16, aarch64

- commit: 9b9bb8f plus the one-lock-per-refill `ClassState::plan` / batched `release` reopen change committed right after this run (Phase 0 acceptance remediation step 2); before-row: `2026-09-16-aarch64-classstate-padding.md`
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
| cfrac | s | 2.414 | 2.5 | 2.374 | 9.9 | +1.7% | -75.1% |
| espresso | s | 2.444 | 3.2 | 2.622 | 14.8 | -6.8% | -78.3% |
| larson | M ops/s | 205.4 | 63.2 | 287.4 | 168.5 | +39.9% | -62.5% |
| mstress | s | 0.262 | 170.8 | 0.372 | 199.3 | -29.6% | -14.3% |
| xmalloc-test | M frees/s | 198.847 | 38.1 | 248.584 | 139.3 | +25.0% | -72.7% |
| alloc-test1 | s | 2.143 | 14.6 | 1.926 | 22.2 | +11.3% | -34.0% |
| alloc-testN | s | 3.049 | 16.1 | 2.722 | 87.8 | +12.0% | -81.7% |
| glibc-simple | s | 1.530 | 1.7 | 1.623 | 12.0 | -5.7% | -85.9% |
| redis | k req/s | 746.8 | 28.7 | 606.4 | 37.6 | -18.8% | -23.5% |

Positive percentages mean challoc did worse than jemalloc (slower, lower throughput, or more RSS).

## Raw runs

Columns: elapsed_s user_s sys_s maxrss_kb exit. First line of each block is the discarded warm-up. Throughput benches also list the per-run score.

### cfrac / challoc

```
2.412 2.412 0.000 2512 0
2.414 2.412 0.001 2512 0
2.429 2.426 0.003 2512 0
2.423 2.422 0.001 2524 0
2.398 2.398 0.000 2524 0
2.413 2.412 0.001 2524 0
```

### cfrac / jemalloc

```
2.574 2.572 0.000 10028 0
2.363 2.362 0.001 10148 0
2.374 2.374 0.000 10148 0
2.395 2.395 0.000 10132 0
2.352 2.352 0.000 10148 0
2.574 2.573 0.001 10160 0
```

### espresso / challoc

```
2.463 2.453 0.008 3280 0
2.500 2.491 0.007 3300 0
2.444 2.433 0.011 3300 0
2.432 2.428 0.004 3284 0
2.444 2.438 0.006 3300 0
2.433 2.425 0.008 3300 0
```

### espresso / jemalloc

```
2.632 2.628 0.004 17228 0
2.622 2.616 0.006 17488 0
2.622 2.614 0.008 15204 0
2.609 2.600 0.009 15204 0
2.645 2.634 0.011 14948 0
2.620 2.606 0.012 14948 0
```

### larson / challoc

```
7.027 49.211 0.262 66620 0
7.028 48.928 0.327 64572 0
7.030 49.261 0.261 66492 0
7.030 49.194 0.286 66676 0
7.027 49.001 0.289 64668 0
7.037 49.080 0.289 64292 0
score (M ops/s): 203.8 204.3 205.6 206.8 205.4 203.1 
```

### larson / jemalloc

```
7.031 49.443 0.300 172256 0
7.029 49.221 0.302 172060 0
7.034 48.978 0.375 173128 0
7.039 49.401 0.351 172588 0
7.033 49.350 0.363 171792 0
7.029 49.290 0.297 172848 0
score (M ops/s): 294.8 294.2 288.4 284.9 286.8 287.4 
```

### mstress / challoc

```
0.278 1.227 0.073 169524 0
0.262 1.152 0.066 176120 0
0.265 1.147 0.071 174892 0
0.269 1.168 0.059 176396 0
0.260 1.178 0.051 174416 0
0.256 1.162 0.060 169836 0
```

### mstress / jemalloc

```
0.368 1.105 0.454 204424 0
0.376 1.068 0.522 201428 0
0.372 1.037 0.491 201208 0
0.363 1.033 0.502 224568 0
0.359 1.068 0.477 217816 0
0.376 1.045 0.491 204080 0
```

### xmalloc-test / challoc

```
5.005 36.528 6.648 39168 0
5.004 35.988 7.083 39160 0
5.005 36.812 6.670 38980 0
5.005 36.146 6.981 38964 0
5.005 36.449 6.898 39112 0
5.005 36.685 6.868 38856 0
score (M frees/s): 200.120 200.241 198.847 199.901 198.069 197.062 
```

### xmalloc-test / jemalloc

```
5.006 46.174 0.919 141996 0
5.008 46.777 0.908 143008 0
5.008 46.486 0.896 142640 0
5.009 46.770 0.900 142860 0
5.009 46.582 0.896 137104 0
5.008 46.776 0.867 140244 0
score (M frees/s): 249.812 252.974 246.958 248.916 247.300 248.584 
```

### alloc-test1 / challoc

```
2.236 2.232 0.003 14952 0
2.143 2.140 0.002 14964 0
2.151 2.148 0.002 14972 0
2.136 2.130 0.005 14972 0
2.133 2.132 0.001 14956 0
2.154 2.153 0.001 14972 0
```

### alloc-test1 / jemalloc

```
1.924 1.923 0.000 22664 0
1.932 1.932 0.000 22704 0
1.926 1.925 0.000 22700 0
1.924 1.924 0.000 22668 0
1.940 1.940 0.000 22656 0
1.925 1.921 0.003 22684 0
```

### alloc-testN / challoc

```
2.819 27.717 0.004 16692 0
2.957 28.771 0.004 16564 0
3.064 29.425 0.033 16548 0
3.007 29.328 0.006 16496 0
3.049 29.628 0.013 16492 0
3.050 29.895 0.015 16180 0
```

### alloc-testN / jemalloc

```
2.736 26.679 0.008 89868 0
2.722 26.398 0.011 89872 0
2.702 26.708 0.008 89952 0
2.745 26.779 0.017 89948 0
2.747 26.626 0.008 89940 0
2.706 26.522 0.006 89948 0
```

### glibc-simple / challoc

```
1.551 1.546 0.003 1728 0
1.530 1.528 0.000 1732 0
1.529 1.525 0.002 1776 0
1.535 1.531 0.002 1732 0
1.529 1.526 0.001 1732 0
1.533 1.530 0.002 1796 0
```

### glibc-simple / jemalloc

```
1.627 1.623 0.002 12236 0
1.627 1.624 0.001 12252 0
1.619 1.616 0.001 12252 0
1.670 1.668 0.001 12252 0
1.623 1.619 0.002 12240 0
1.614 1.611 0.001 12252 0
```

### redis / challoc

```
1.339 0.067 0.174 3284 0
1.344 0.078 0.163 3284 0
1.342 0.079 0.162 5128 0
1.339 0.077 0.163 3284 0
1.341 0.083 0.155 5292 0
1.338 0.085 0.155 3268 0
score (k req/s): 747.9 744.6 746.3 747.4 746.8 748.5 
server:
1.453 1.238 0.105 29432 0
1.458 1.263 0.083 29432 0
1.447 1.250 0.094 29432 0
1.454 1.244 0.099 29416 0
1.455 1.258 0.086 29304 0
1.450 1.245 0.095 29432 0
```

### redis / jemalloc

```
1.691 0.085 0.154 5236 0
1.639 0.079 0.158 3284 0
1.648 0.070 0.173 3272 0
1.672 0.073 0.164 3264 0
1.651 0.077 0.161 4956 0
1.668 0.077 0.158 4524 0
score (k req/s): 592.1 610.5 607.5 598.8 606.4 599.9 
server:
1.800 1.591 0.102 38472 0
1.748 1.546 0.095 38472 0
1.762 1.556 0.093 38468 0
1.783 1.580 0.093 38472 0
1.765 1.570 0.083 38472 0
1.773 1.582 0.088 38596 0
```


## larson profile under challoc (aarch64, 20 s run, SIGPROF sampler at 1000 Hz of CPU time)

```
total samples 2666; in libchalloc.so 1161 (43.5%)

by module:
     1267  47.5%  main
     1161  43.5%  libchalloc.so
      234   8.8%  libstdc++.so.6
        4   0.2%  libc.so.6

top 10 challoc symbols (rmw = samples on lock/atomic RMW instructions inside that symbol; outlined LSE helpers count whole):
      732  63.0%  rmw=0     free
      266  22.9%  rmw=0     malloc
       54   4.7%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::claim
       22   1.9%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::release
       22   1.9%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::reopen
       15   1.3%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::release_word
        9   0.8%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::take
        9   0.8%  rmw=0     <challoc[c0770b06928205b9]::bitmap::TwoLevelBitmap>::lowest_set_from
        6   0.5%  rmw=6     __aarch64_ldclr8_rel
        5   0.4%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::dec_live

samples on lock/atomic RMW instructions anywhere in challoc: 16 (1.38% of challoc samples)

top 15 challoc instructions:
       72   6.2%      a8d0  lsr      free
       59   5.1%      a8d8  ldr      free
       56   4.8%      a8e8  ldr      free
       52   4.5%      a840  lsl      free
       46   4.0%      b23c  str      malloc
       44   3.8%      a928  add      free
       37   3.2%      b198  cmp      malloc
       36   3.1%      b250  ret      malloc
       35   3.0%      a81c  sub      free
       33   2.8%      a91c  ldr      free
       29   2.5%      a8b0  adrp     free
       28   2.4%      a7fc  sub      free
       25   2.2%      a7cc  cmp      free
       19   1.6%      a89c  add      free
       19   1.6%      a88c  cmp      free
```

larson output:

```
Throughput = 202403194 operations per second, relative time: 4.941s.
```
