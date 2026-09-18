# challoc Dense cache headers, 2026-09-18, x86_64

- commit: 078cc0d
- date: 2026-09-18T02:54:36Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.00 0.88 2.45; at end: 9.91 8.27 5.73; 612 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; others: wall seconds
- variants:
  - old: 5b8bf5e7bbfb built into target/ab/5b8bf5e7bbfb
  - new: 078cc0d35ebc built into target/ab/078cc0d35ebc
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 46.2 46.3 48.0 48.6 48.6 | 16344 |
| larson1 | new | 46.2 46.2 46.6 47.2 48.4 | 16148 |
| larson1 | jemalloc | 43.7 43.8 44.4 45.1 47.0 | 18276 |
| larson10 | old | 271.9 273.3 276.9 281.6 290.7 | 99328 |
| larson10 | new | 279.2 284.2 285.2 286.1 291.0 | 103092 |
| larson10 | jemalloc | 288.7 294.4 295.0 304.2 305.1 | 93900 |
| larson16 | old | 299.2 302.8 305.1 309.8 312.8 | 125676 |
| larson16 | new | 308.7 309.5 313.1 313.9 321.1 | 126560 |
| larson16 | jemalloc | 331.2 337.0 340.6 341.0 345.1 | 130876 |
| xmalloc | old | 338.196 338.849 344.427 346.254 351.060 | 83272 |
| xmalloc | new | 335.265 342.184 345.509 345.751 360.940 | 84404 |
| xmalloc | jemalloc | 327.690 330.625 332.208 332.450 333.578 | 92236 |
| alloc-test1 | old | 2.951 3.001 3.238 3.354 3.360 | 14140 |
| alloc-test1 | new | 2.974 2.993 3.177 3.180 3.359 | 13992 |
| alloc-test1 | jemalloc | 2.845 2.851 2.913 2.958 3.223 | 13376 |
| alloc-testN | old | 4.305 4.347 4.350 4.399 4.469 | 22372 |
| alloc-testN | new | 4.356 4.359 4.364 4.415 4.417 | 21572 |
| alloc-testN | jemalloc | 4.047 4.066 4.074 4.174 4.273 | 17056 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 48.0 | 16.0 | 46.6 | 15.8 | 44.4 | 17.8 | +3.0% | +8.1% |
| larson10 | M ops/s | 276.9 | 97.0 | 285.2 | 100.7 | 295.0 | 91.7 | -2.9% | -6.1% |
| larson16 | M ops/s | 305.1 | 122.7 | 313.1 | 123.6 | 340.6 | 127.8 | -2.6% | -10.4% |
| xmalloc | M frees/s | 344.427 | 81.3 | 345.509 | 82.4 | 332.208 | 90.1 | -0.3% | +3.7% |
| alloc-test1 | s | 3.238 | 13.8 | 3.177 | 13.7 | 2.913 | 13.1 | -1.9% | -10.0% |
| alloc-testN | s | 4.350 | 21.8 | 4.364 | 21.1 | 4.074 | 16.7 | +0.3% | -6.3% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

- zone: us-east4-a (challoc-bench-amd64, c4-standard-16); Phase 1.5 task 5 (review P3); code commits 97b3d31 (inline headers, per-class stacks) and 078cc0d (heap built in place); rows above are HEAD = 078cc0d against HEAD~2 = 5b8bf5e.

## Hardware counters, larson 1 and 10 threads (`sudo perf stat --inherit -p <container init>`)

Per operation = per malloc+free pair (throughput x 5 s). On this core `dTLB-load-misses` counts page walks. old = 5b8bf5e, new = 97b3d31 (the header layout; 078cc0d only changes heap creation), two rounds each.

| bench | variant | round | cycles | instr | L1D-miss | dTLB-miss | cyc/op | ins/op | L1D/op | dTLB/op | maxrss MiB |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | old | r1 | 1.65e+10 | 5.15e+10 | 4.68e+08 | 3.51e+05 | 71.0 | 222.0 | 2.02 | 0.00 | 15.7 |
| larson1 | new | r1 | 1.65e+10 | 5.19e+10 | 4.46e+08 | 3.42e+05 | 72.6 | 228.7 | 1.97 | 0.00 | 17.6 |
| larson1 | jemalloc | r1 | 1.63e+10 | 4.52e+10 | 4.82e+08 | 1.97e+05 | 72.3 | 200.9 | 2.14 | 0.00 | 15.7 |
| larson1 | old | r2 | 1.63e+10 | 5.16e+10 | 4.67e+08 | 3.47e+05 | 70.0 | 221.7 | 2.01 | 0.00 | 15.9 |
| larson1 | new | r2 | 1.63e+10 | 5.29e+10 | 4.53e+08 | 3.44e+05 | 70.5 | 228.5 | 1.96 | 0.00 | 17.8 |
| larson1 | jemalloc | r2 | 1.61e+10 | 4.28e+10 | 4.56e+08 | 1.94e+05 | 75.3 | 200.9 | 2.14 | 0.00 | 15.2 |
| larson10 | old | r1 | 1.64e+11 | 3.19e+11 | 3.06e+09 | 2.84e+07 | 114.4 | 222.7 | 2.13 | 0.02 | 102.3 |
| larson10 | new | r1 | 1.63e+11 | 3.13e+11 | 2.72e+09 | 2.11e+07 | 120.0 | 229.7 | 2.00 | 0.02 | 107.6 |
| larson10 | jemalloc | r1 | 1.61e+11 | 2.81e+11 | 3.06e+09 | 6.08e+06 | 115.6 | 201.1 | 2.19 | 0.00 | 87.7 |
| larson10 | old | r2 | 1.61e+11 | 3.09e+11 | 2.99e+09 | 2.72e+07 | 115.7 | 222.7 | 2.16 | 0.02 | 97.8 |
| larson10 | new | r2 | 1.59e+11 | 3.12e+11 | 2.7e+09 | 2.02e+07 | 117.0 | 229.7 | 1.99 | 0.01 | 110.3 |
| larson10 | jemalloc | r2 | 1.58e+11 | 2.81e+11 | 3.07e+09 | 6.1e+06 | 112.9 | 201.1 | 2.20 | 0.00 | 88.0 |

Reading: +7 instructions per operation (222 -> 229) that aarch64 does not show (242.7 -> 242.7): x86 addressing scales an index by at most 8, so `heap + 128 + 64 * c` costs a shift and an add on each of the pop and push paths where aarch64 folds the `lsl #6` into the add, and the `slots` load is new on both. L1D misses per operation fall 2.02 -> 1.97 (one thread) and 2.15 -> 2.00 (ten); page walks are already negligible here (96-entry L1 dTLB, cheap STLB hits, review 3.3). Cycles per operation are within noise at one thread; the A/B medians above are the number that counts.

Final code (078cc0d, the `ab` builds of the rows above), two rounds:

| bench | variant | round | cycles | instr | L1D-miss | dTLB-miss | cyc/op | ins/op | L1D/op | dTLB/op | maxrss MiB |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | old 5b8bf5e | r1 | 1.72e+10 | 5.4e+10 | 4.91e+08 | 3.73e+05 | 70.7 | 221.8 | 2.02 | 0.00 | 15.8 |
| larson1 | new 078cc0d | r1 | 1.7e+10 | 5.5e+10 | 4.69e+08 | 3.8e+05 | 70.7 | 228.6 | 1.95 | 0.00 | 16.1 |
| larson1 | jemalloc | r1 | 1.72e+10 | 4.76e+10 | 5.01e+08 | 2.02e+05 | 72.4 | 200.9 | 2.11 | 0.00 | 15.7 |
| larson1 | old 5b8bf5e | r2 | 1.71e+10 | 5.36e+10 | 4.93e+08 | 3.47e+05 | 70.5 | 221.4 | 2.03 | 0.00 | 16.0 |
| larson1 | new 078cc0d | r2 | 1.72e+10 | 5.57e+10 | 4.8e+08 | 3.75e+05 | 70.5 | 228.4 | 1.97 | 0.00 | 16.0 |
| larson1 | jemalloc | r2 | 1.7e+10 | 4.71e+10 | 5.03e+08 | 2.09e+05 | 72.5 | 200.9 | 2.14 | 0.00 | 16.0 |
| larson10 | old 5b8bf5e | r1 | 1.68e+11 | 3.28e+11 | 3.17e+09 | 3.17e+07 | 114.1 | 222.7 | 2.15 | 0.02 | 103.2 |
| larson10 | new 078cc0d | r1 | 1.68e+11 | 3.42e+11 | 2.96e+09 | 2.41e+07 | 112.8 | 229.7 | 1.99 | 0.02 | 99.5 |
| larson10 | jemalloc | r1 | 1.67e+11 | 2.89e+11 | 3.15e+09 | 6.66e+06 | 116.1 | 201.1 | 2.19 | 0.00 | 88.9 |
| larson10 | old 5b8bf5e | r2 | 1.68e+11 | 3.19e+11 | 3.08e+09 | 2.72e+07 | 117.0 | 222.7 | 2.15 | 0.02 | 98.1 |
| larson10 | new 078cc0d | r2 | 1.68e+11 | 3.27e+11 | 2.84e+09 | 2.02e+07 | 117.9 | 229.7 | 2.00 | 0.01 | 98.8 |
| larson10 | jemalloc | r2 | 1.67e+11 | 2.91e+11 | 3.18e+09 | 6.48e+06 | 115.4 | 201.1 | 2.20 | 0.00 | 91.9 |

Cycles per larson1 operation identical to old (70.7 / 70.5 in both rounds), L1D misses per operation 2.02 -> 1.96 (one thread) and 2.15 -> 2.00 (ten), page walks negligible on both; maxrss back at old's.

## larson1 with ten rounds

The five-round larson1 row above (+3.0%) is bimodal on this VM: both variants' sorted lists split into a 46.2..46.6 and a 48.0..48.6 group (old `46.2 46.3 48.0 48.6 48.6`, new `46.2 46.2 46.6 47.2 48.4`), so the median lands on whichever group the third sample falls in, and the first protocol run of the header commit read +0.4%, `quick` -0.2%. Ten timed rounds of larson1 alone, same builds, same session (`make ab BENCHES=larson1 ROUNDS=10`, log `~/logs/ab-amd64-it23c.log`):

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 37.6 46.9 46.9 47.0 47.3 47.3 47.4 48.4 49.2 49.4 | 16228 |
| larson1 | new | 45.6 46.5 47.1 47.2 47.2 48.0 48.9 49.1 49.2 49.2 | 16236 |
| larson1 | jemalloc | 40.5 44.0 44.6 44.8 45.4 45.5 47.1 47.2 47.4 47.5 | 18252 |

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 47.3 | 15.8 | 47.2 | 15.9 | 45.4 | 17.8 | +0.2% | +4.2% |

The lists overlap over their whole range; larson1 is not slower on this VM (the acceptance row), and the counters agree (cycles per operation equal).

## The single slot block and the RSS attribution

See `2026-09-18-aarch64-gce-dense-headers.md` for the full account (the variants v1/v3/v11/v13 were built and measured on this VM: block 1.78e9 L1D misses on xmalloc against old's 4.19e8, v1 4.12e8, v3 1.64e9, v11 1.79e9, v13 1.76e9; `perf record` attribution and `--per-thread` counts are from this VM). `quick` of the block build here: larson1 +2.4%, larson10 +4.8%, xmalloc +2.4%, alloc-test1 +0.2% (go, but every row worse); of the shipped layout: larson1 -0.2%, larson10 -1.0%, xmalloc +1.3%, alloc-test1 +1.4%. The larson RSS of the header-layout commit (+1.8 MiB at one thread, +13 MiB at ten, above) was one extra stack page per churned thread, fixed by 078cc0d.
