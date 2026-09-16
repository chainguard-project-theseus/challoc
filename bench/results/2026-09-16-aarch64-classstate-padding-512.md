# challoc Phase 0 benchmarks, 2026-09-16, aarch64

- commit: 8752ac8 plus the uncommitted ClassState padding at its first, 512-byte stride (superseded; kept as the evidence for the alloc-test regression described in `src/class.rs`)
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
| cfrac | s | 2.434 | 2.5 | 2.581 | 9.9 | -5.7% | -75.2% |
| espresso | s | 2.462 | 3.2 | 2.610 | 16.7 | -5.7% | -80.7% |
| larson | M ops/s | 203.7 | 63.0 | 286.9 | 168.5 | +40.8% | -62.6% |
| mstress | s | 0.270 | 166.9 | 0.373 | 208.2 | -27.6% | -19.8% |
| xmalloc-test | M frees/s | 81.720 | 36.7 | 253.442 | 136.0 | +210.1% | -73.0% |
| alloc-test1 | s | 2.551 | 14.6 | 1.925 | 22.2 | +32.5% | -34.1% |
| alloc-testN | s | 3.295 | 16.2 | 2.754 | 87.8 | +19.6% | -81.6% |
| glibc-simple | s | 1.523 | 1.7 | 1.622 | 11.9 | -6.1% | -85.8% |
| redis | k req/s | 749.1 | 28.7 | 609.8 | 37.7 | -18.6% | -23.7% |

Positive percentages mean challoc did worse than jemalloc (slower, lower throughput, or more RSS).

## Raw runs

Columns: elapsed_s user_s sys_s maxrss_kb exit. First line of each block is the discarded warm-up. Throughput benches also list the per-run score.

### cfrac / challoc

```
2.399 2.398 0.000 2520 0
2.887 2.885 0.002 2524 0
2.395 2.393 0.001 2524 0
2.434 2.433 0.001 2524 0
2.401 2.400 0.001 2524 0
2.438 2.437 0.001 2524 0
```

### cfrac / jemalloc

```
2.373 2.373 0.000 10136 0
2.582 2.580 0.002 10144 0
2.592 2.589 0.002 10160 0
2.570 2.569 0.000 10140 0
2.572 2.572 0.000 10160 0
2.581 2.579 0.001 10160 0
```

### espresso / challoc

```
2.443 2.439 0.004 3300 0
2.465 2.453 0.011 3300 0
2.439 2.427 0.011 3300 0
2.480 2.474 0.006 3300 0
2.462 2.451 0.010 3300 0
2.439 2.430 0.009 3300 0
```

### espresso / jemalloc

```
2.600 2.590 0.009 15460 0
2.615 2.611 0.004 17428 0
2.630 2.620 0.010 16884 0
2.592 2.585 0.007 17096 0
2.610 2.604 0.006 15208 0
2.596 2.590 0.006 17212 0
```

### larson / challoc

```
7.022 49.281 0.345 66324 0
7.025 49.104 0.333 64428 0
7.034 49.159 0.326 64440 0
7.029 49.351 0.252 66568 0
7.030 49.026 0.364 64652 0
7.032 49.144 0.353 64468 0
score (M ops/s): 203.3 201.8 203.9 204.1 203.7 201.8 
```

### larson / jemalloc

```
7.032 49.440 0.302 173604 0
7.040 49.430 0.301 170044 0
7.033 49.030 0.348 170864 0
7.034 49.475 0.268 180144 0
7.034 49.293 0.414 172784 0
7.033 49.250 0.386 172576 0
score (M ops/s): 298.2 276.1 286.9 289.0 289.8 285.8 
```

### mstress / challoc

```
0.282 1.232 0.088 175128 0
0.269 1.192 0.059 170300 0
0.273 1.210 0.061 173276 0
0.266 1.177 0.052 172536 0
0.270 1.186 0.050 170864 0
0.271 1.167 0.075 170904 0
```

### mstress / jemalloc

```
0.371 1.022 0.514 228172 0
0.372 1.037 0.514 208984 0
0.377 1.023 0.539 194152 0
0.378 1.046 0.522 221816 0
0.365 1.060 0.495 237220 0
0.373 1.052 0.542 213148 0
```

### xmalloc-test / challoc

```
5.002 37.978 6.230 20036 0
5.003 37.917 6.330 36680 0
5.003 37.300 6.871 37960 0
5.003 38.002 6.328 37704 0
5.003 37.775 6.384 34500 0
5.002 37.704 6.301 37576 0
score (M frees/s): 79.661 82.011 81.338 81.134 81.720 82.791 
```

### xmalloc-test / jemalloc

```
5.008 46.359 0.880 141152 0
5.006 46.886 0.893 141440 0
5.011 46.665 0.809 139228 0
5.006 46.834 0.929 137564 0
5.007 46.262 0.838 142148 0
5.007 46.668 0.910 138736 0
score (M frees/s): 259.076 257.260 253.442 254.601 249.105 250.968 
```

### alloc-test1 / challoc

```
2.622 2.619 0.002 14964 0
2.549 2.548 0.001 14964 0
2.550 2.550 0.000 14964 0
2.553 2.552 0.001 14964 0
2.569 2.568 0.001 14964 0
2.551 2.550 0.001 14964 0
```

### alloc-test1 / jemalloc

```
1.928 1.927 0.000 22668 0
1.923 1.923 0.000 22692 0
1.938 1.936 0.002 22720 0
1.925 1.925 0.000 22716 0
1.923 1.922 0.000 22676 0
1.929 1.926 0.002 22672 0
```

### alloc-testN / challoc

```
3.104 30.488 0.005 16800 0
3.327 32.246 0.043 16556 0
3.255 31.881 0.009 16556 0
3.255 32.176 0.012 16528 0
3.351 32.661 0.033 16484 0
3.295 32.435 0.006 16552 0
```

### alloc-testN / jemalloc

```
2.740 26.490 0.006 89868 0
2.754 26.455 0.022 89872 0
2.788 27.275 0.013 89856 0
2.730 26.766 0.005 89872 0
2.796 26.940 0.005 89852 0
2.738 26.558 0.029 89868 0
```

### glibc-simple / challoc

```
1.556 1.552 0.002 1732 0
1.519 1.513 0.004 1732 0
1.520 1.517 0.002 1728 0
1.523 1.519 0.002 1776 0
1.525 1.520 0.003 1732 0
1.553 1.549 0.002 1732 0
```

### glibc-simple / jemalloc

```
1.621 1.617 0.002 12176 0
1.622 1.621 0.000 12176 0
1.622 1.620 0.001 12176 0
1.622 1.618 0.002 12176 0
1.620 1.617 0.001 12180 0
1.651 1.644 0.006 12176 0
```

### redis / challoc

```
1.365 0.079 0.168 4816 0
1.344 0.067 0.176 5132 0
1.336 0.088 0.147 4956 0
1.328 0.090 0.147 4956 0
1.329 0.076 0.164 4956 0
1.340 0.079 0.167 3220 0
score (k req/s): 733.1 744.6 749.1 754.1 753.6 746.8 
server:
1.479 1.265 0.103 29432 0
1.449 1.258 0.089 29432 0
1.450 1.250 0.089 29368 0
1.435 1.258 0.073 29432 0
1.443 1.240 0.092 29428 0
1.445 1.237 0.106 29424 0
```

### redis / jemalloc

```
1.657 0.078 0.160 3284 0
1.637 0.083 0.158 3284 0
1.641 0.075 0.166 5260 0
1.612 0.081 0.157 4860 0
1.661 0.071 0.167 3264 0
1.646 0.077 0.166 3284 0
score (k req/s): 603.9 611.6 609.8 621.1 602.8 607.9 
server:
1.770 1.559 0.100 38392 0
1.745 1.534 0.105 38580 0
1.746 1.540 0.103 38576 0
1.720 1.534 0.080 38576 0
1.771 1.576 0.087 38580 0
1.759 1.560 0.088 38556 0
```


## larson profile under challoc (aarch64, 20 s run, SIGPROF sampler at 1000 Hz of CPU time)

```
total samples 464; in libchalloc.so 210 (45.3%)

by module:
      215  46.3%  main
      210  45.3%  libchalloc.so
       36   7.8%  libstdc++.so.6
        3   0.6%  libc.so.6

top 10 challoc symbols (rmw = samples on lock/atomic RMW instructions inside that symbol; outlined LSE helpers count whole):
      116  55.2%  rmw=0     free
       59  28.1%  rmw=0     malloc
       13   6.2%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::claim
       10   4.8%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::release_word
        3   1.4%  rmw=0     <challoc[c0770b06928205b9]::lock::RawLock>::lock_slow
        2   1.0%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::dec_live
        2   1.0%  rmw=2     __aarch64_ldclr8_rel
        1   0.5%  rmw=1     __aarch64_cas4_acq
        1   0.5%  rmw=1     __aarch64_swp4_rel
        1   0.5%  rmw=0     <challoc[c0770b06928205b9]::heap::Heap>::alloc_slow

samples on lock/atomic RMW instructions anywhere in challoc: 4 (1.90% of challoc samples)

top 15 challoc instructions:
       12   5.7%      a21c  ldr      free
       11   5.2%      a204  lsr      free
       11   5.2%      a154  sub      free
       10   4.8%      ab5c  str      malloc
        9   4.3%      a178  lsl      free
        8   3.8%      aab8  cmp      malloc
        8   3.8%      aa8c  cmp      malloc
        7   3.3%      ab54  ldr      malloc
        6   2.9%      a20c  ldr      free
        5   2.4%      a254  ldp      free
        5   2.4%      aa6c  stp      malloc
        5   2.4%      ab70  ret      malloc
        5   2.4%      a10c  adrp     free
        4   1.9%      a104  cmp      free
        3   1.4%      a1e4  adrp     free
```

larson output:

```
Throughput = 204320280 operations per second, relative time: 4.894s.
```
