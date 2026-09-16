# challoc Phase 0 benchmarks, 2026-09-16, aarch64

- commit: the step 6 sources (working tree over b710859, so bench/run.sh recorded b710859; committed as the "Phase 0 acceptance step 6" commit)
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
| cfrac | s | 2.281 | 2.5 | 2.384 | 9.8 | -4.3% | -74.9% |
| espresso | s | 2.344 | 3.2 | 2.574 | 15.0 | -8.9% | -78.5% |
| larson | M ops/s | 270.1 | 79.3 | 290.9 | 169.7 | +7.7% | -53.3% |
| mstress | s | 0.255 | 170.6 | 0.379 | 204.5 | -32.7% | -16.6% |
| xmalloc-test | M frees/s | 565.369 | 39.2 | 249.439 | 137.8 | -55.9% | -71.6% |
| alloc-test1 | s | 1.981 | 14.8 | 1.935 | 22.2 | +2.4% | -33.6% |
| alloc-testN | s | 2.755 | 16.9 | 2.726 | 87.8 | +1.1% | -80.8% |
| glibc-simple | s | 1.345 | 1.8 | 1.626 | 12.0 | -17.3% | -84.8% |
| redis | k req/s | 756.4 | 28.7 | 606.4 | 37.6 | -19.8% | -23.7% |

Positive percentages mean challoc did worse than jemalloc (slower, lower throughput, or more RSS).

## Raw runs

Columns: elapsed_s user_s sys_s maxrss_kb exit. First line of each block is the discarded warm-up. Throughput benches also list the per-run score.

### cfrac / challoc

```
2.273 2.271 0.002 2512 0
2.292 2.289 0.002 2516 0
2.424 2.423 0.001 2524 0
2.272 2.270 0.002 2524 0
2.281 2.278 0.003 2516 0
2.279 2.276 0.002 2524 0
```

### cfrac / jemalloc

```
2.393 2.392 0.001 10048 0
2.354 2.353 0.001 10032 0
2.384 2.383 0.001 10044 0
2.384 2.384 0.000 10044 0
2.355 2.355 0.000 10044 0
2.854 2.851 0.002 10056 0
```

### espresso / challoc

```
2.337 2.330 0.007 3300 0
2.353 2.348 0.004 3300 0
2.337 2.332 0.005 3300 0
2.352 2.340 0.012 3300 0
2.344 2.340 0.004 3300 0
2.344 2.336 0.008 3300 0
```

### espresso / jemalloc

```
2.584 2.573 0.011 15192 0
2.573 2.565 0.007 17296 0
2.608 2.595 0.013 17388 0
2.593 2.581 0.012 15216 0
2.574 2.566 0.007 15340 0
2.574 2.571 0.003 15212 0
```

### larson / challoc

```
7.024 48.875 0.300 80708 0
7.029 49.100 0.330 80380 0
7.033 49.271 0.261 81748 0
7.039 48.943 0.302 81172 0
7.030 49.091 0.301 82124 0
7.038 49.122 0.410 81004 0
score (M ops/s): 262.9 270.2 270.1 268.9 271.7 265.4 
```

### larson / jemalloc

```
7.036 49.307 0.375 171776 0
7.037 49.188 0.322 174864 0
7.039 49.301 0.290 179944 0
7.039 49.430 0.292 173804 0
7.032 49.245 0.332 172320 0
7.032 49.330 0.278 170424 0
score (M ops/s): 288.3 303.2 290.9 298.1 286.9 289.6 
```

### mstress / challoc

```
0.269 1.145 0.093 175396 0
0.256 1.125 0.057 175380 0
0.251 1.123 0.065 177076 0
0.258 1.103 0.067 173420 0
0.255 1.095 0.067 174656 0
0.255 1.123 0.060 171692 0
```

### mstress / jemalloc

```
0.374 1.031 0.530 228844 0
0.372 1.034 0.509 194952 0
0.379 1.049 0.516 209408 0
0.384 1.045 0.530 187148 0
0.377 1.040 0.548 215468 0
0.379 1.052 0.525 213336 0
```

### xmalloc-test / challoc

```
5.005 46.449 0.761 40520 0
5.004 46.744 0.738 40096 0
5.004 46.496 0.790 40380 0
5.003 46.647 0.726 39880 0
5.005 46.730 0.766 39864 0
5.006 46.258 0.795 40108 0
score (M frees/s): 588.459 573.079 571.020 563.252 565.369 551.741 
```

### xmalloc-test / jemalloc

```
5.015 43.626 0.957 144340 0
5.009 46.345 0.912 144704 0
5.009 46.369 0.924 141156 0
5.008 46.617 0.940 138672 0
5.008 46.486 0.879 142456 0
5.008 46.839 0.927 138900 0
score (M frees/s): 217.897 248.414 250.153 251.633 247.210 249.439 
```

### alloc-test1 / challoc

```
2.018 2.015 0.003 15108 0
1.981 1.979 0.002 15104 0
1.969 1.966 0.001 15104 0
1.983 1.980 0.002 15104 0
1.995 1.992 0.003 15104 0
1.966 1.961 0.004 15104 0
```

### alloc-test1 / jemalloc

```
1.933 1.930 0.001 22788 0
1.935 1.933 0.001 22760 0
1.928 1.926 0.001 22744 0
1.935 1.934 0.001 22740 0
1.920 1.920 0.000 22764 0
1.935 1.932 0.002 22764 0
```

### alloc-testN / challoc

```
2.656 25.479 0.010 17460 0
2.622 25.822 0.016 17388 0
2.714 26.702 0.011 17336 0
2.769 26.561 0.039 17264 0
2.783 26.957 0.009 17204 0
2.755 26.700 0.010 17196 0
```

### alloc-testN / jemalloc

```
2.726 26.422 0.005 89952 0
2.737 26.456 0.023 89936 0
2.726 26.556 0.011 89944 0
2.701 26.347 0.007 89948 0
2.726 26.639 0.016 89948 0
2.714 26.520 0.016 89952 0
```

### glibc-simple / challoc

```
1.370 1.366 0.002 1908 0
1.355 1.352 0.002 1860 0
1.344 1.341 0.002 1908 0
1.354 1.353 0.000 1912 0
1.345 1.343 0.001 1796 0
1.342 1.339 0.001 1860 0
```

### glibc-simple / jemalloc

```
1.626 1.620 0.005 12252 0
1.626 1.622 0.002 12252 0
1.624 1.622 0.000 12232 0
1.635 1.632 0.001 12252 0
1.619 1.618 0.000 12256 0
1.630 1.627 0.001 12248 0
```

### redis / challoc

```
1.319 0.071 0.171 3284 0
1.326 0.079 0.159 4796 0
1.328 0.083 0.156 5008 0
1.323 0.077 0.162 3284 0
1.320 0.086 0.154 3268 0
1.324 0.087 0.153 3284 0
score (k req/s): 758.7 755.3 754.1 756.4 758.2 756.4 
server:
1.424 1.230 0.093 29432 0
1.440 1.237 0.092 29432 0
1.442 1.228 0.103 29304 0
1.430 1.224 0.101 29368 0
1.435 1.217 0.106 29304 0
1.432 1.240 0.087 29432 0
```

### redis / jemalloc

```
1.653 0.066 0.181 3264 0
1.640 0.084 0.159 3284 0
1.650 0.081 0.157 3284 0
1.651 0.074 0.168 3284 0
1.626 0.074 0.162 4896 0
1.664 0.087 0.158 3284 0
score (k req/s): 605.3 610.1 606.4 606.4 615.4 601.3 
server:
1.767 1.554 0.100 38336 0
1.753 1.539 0.099 38476 0
1.756 1.570 0.082 38464 0
1.758 1.554 0.099 38472 0
1.735 1.534 0.094 38472 0
1.778 1.573 0.093 38476 0
```


## larson profile under challoc (aarch64, 20 s run, SIGPROF sampler at 1000 Hz of CPU time)

```
total samples 712; in libchalloc.so 278 (39.0%)

by module:
      433  60.8%  main
      278  39.0%  libchalloc.so
        1   0.1%  libc.so.6

top 10 challoc symbols (rmw = samples on lock/atomic RMW instructions inside that symbol; outlined LSE helpers count whole):
      174  62.6%  rmw=0     free
       96  34.5%  rmw=0     operator new(unsigned long)
        4   1.4%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::claim
        2   0.7%  rmw=0     core[797e91ca1b8870b2]::slice::sort::unstable::ipnsort::<*mut u8, <*mut u8 as core[797e91ca1b8870b2]::cmp::PartialOrd>::lt>
        2   0.7%  rmw=0     <challoc[c0770b06928205b9]::class::ClassState>::release

samples on lock/atomic RMW instructions anywhere in challoc: 0 (0.00% of challoc samples)

top 15 challoc instructions:
       25   9.0%      b03c  lsr      free
       21   7.6%      b074  ret      free
       19   6.8%      b530  ldr      operator new(unsigned long)
       18   6.5%      b538  str      operator new(unsigned long)
       17   6.1%      b068  str      free
       14   5.0%      b4ac  cmp      operator new(unsigned long)
       12   4.3%      afe0  lsl      free
       12   4.3%      b020  add      free
       11   4.0%      b4d8  cmp      operator new(unsigned long)
       10   3.6%      b044  ldr      free
        9   3.2%      b524  ldr      operator new(unsigned long)
        7   2.5%      b054  add      free
        7   2.5%      b008  mov      free
        7   2.5%      af84  cmp      free
        6   2.2%      b018  cmp      free
```

larson output:

```
Throughput = 273101565 operations per second, relative time: 3.662s.
```
