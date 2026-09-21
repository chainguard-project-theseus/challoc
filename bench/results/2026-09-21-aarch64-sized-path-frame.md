# The frame of the sized path (lane sized-path, task 2), aarch64

Date 2026-09-21. Lane `sized-path`, lane VM `challoc-sized-path-arm64`
(c4a-standard-16, Neoverse V2, PMU exposed, kernel 7.0.0-1011-gcp). The
x86_64 file (`2026-09-21-x86_64-sized-path-frame.md`) has the mechanism,
the change and the static shape on both architectures; on aarch64 every
one of `malloc`, `free`, `sdallocx`, `challoc_malloc_usable` and
`challoc_free_usable` loses its `stp x29, x30` frame and its one call
(`malloc` 70 -> 62 instructions, `free` 149 -> 145, `sdallocx` 192 ->
182, `challoc_malloc_usable` 84 -> 68, `challoc_free_usable` 146 -> 143).


## 1. `quick-perf` on the C4A (`old` = 63bd7ee, jemalloc; one run per cell)

At 97859aa (the scan hit and the scheduled pop decline alone; the x86_64
file's section 4 explains the two states):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1-write | old | 6.49 | 468.4 | 452.1 | 4.712 | 3.897 |
| valkey1-write | new | 6.60 | 460.1 | 444.1 | 4.703 | 3.898 |
| valkey1-write | jemalloc | 6.65 | 455.7 | 390.0 | 4.748 | 3.773 |
| valkey1-write-pair | old | 6.07 | 501.5 | 488.8 | 6.595 | 5.852 |
| valkey1-write-pair | new | 6.01 | 505.5 | 479.4 | 6.594 | 5.860 |
| valkey1 | old | 11.17 | 272.0 | 304.7 | 3.175 | 2.865 |
| valkey1 | new | 10.83 | 280.4 | 305.2 | 3.172 | 2.915 |
| valkey1 | jemalloc | 11.93 | 253.4 | 218.9 | 3.027 | 2.727 |
| valkey1-free | old | 10.61 | 286.1 | 306.3 | 4.179 | 3.949 |
| valkey1-free | new | 10.71 | 283.2 | 308.7 | 4.171 | 4.017 |
| valkey1-free | jemalloc | 12.17 | 248.9 | 220.0 | 3.464 | 3.375 |
| larson1 | old | 41.40 | 72.3 | 264.5 | 2.101 | 4.016 |
| larson1 | new | 42.12 | 71.1 | 259.5 | 2.096 | 4.003 |
| larson1 | jemalloc | 44.73 | 66.6 | 224.8 | 2.067 | 1.730 |

At 9113e9c (the declined pop straight to `alloc_slow`):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1-write | old | 6.66 | 455.6 | 450.0 | 4.706 | 3.902 |
| valkey1-write | new | 6.41 | 474.2 | 445.0 | 4.707 | 3.879 |
| valkey1-write | jemalloc | 6.63 | 457.1 | 390.1 | 4.747 | 3.788 |
| valkey1-write-pair | old | 6.09 | 498.7 | 487.1 | 6.587 | 5.838 |
| valkey1-write-pair | new | 6.02 | 505.3 | 479.3 | 6.592 | 5.795 |
| valkey1 | old | 11.21 | 270.9 | 304.7 | 3.172 | 2.868 |
| valkey1 | new | 11.21 | 270.8 | 303.1 | 3.167 | 2.912 |
| valkey1 | jemalloc | 12.45 | 243.2 | 217.9 | 3.005 | 2.739 |
| valkey1-free | old | 10.68 | 284.2 | 306.0 | 4.171 | 3.985 |
| valkey1-free | new | 10.71 | 283.2 | 307.0 | 4.161 | 4.012 |
| valkey1-free | jemalloc | 12.15 | 249.2 | 220.2 | 3.465 | 3.347 |
| larson1 | old | 41.57 | 72.0 | 264.5 | 2.101 | 4.002 |
| larson1 | new | 42.10 | 71.1 | 259.5 | 2.104 | 4.023 |
| larson1 | jemalloc | 44.71 | 66.6 | 224.8 | 2.064 | 1.691 |

Instructions per operation, the exact column: `valkey1-write` -8.0
then -5.0 per SET (two `malloc` and two `sdallocx`, every one of the
four frameless now: the `stp`/`ldp` pair and `mov x29, sp` per call less
the tail call's argument moves), `larson1` -5.0 in both states,
`valkey1-write-pair` -9.4 / -7.8, and the mixed `valkey1` row +0.5 at
97859aa (the scheduled pop's re-dispatch eating the frame gain) then
**-1.6** at 9113e9c, `valkey1-free` +2.4 then +1.0. The cycle columns:
`larson1` -1.7% / -1.2%, `valkey1` +3.1% then 0.0%, `valkey1-write`
-1.8% then +4.1% between the two single runs of the same code shape,
which is the row's band on this host (the 09-19 file's section 2.2 read
453 -> 450 for a 38-instruction cut); `ab` below is the measure.

## 2. `quick` on the C4A (`OLD=63bd7ee`, one round, the gate)

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.8 (16.1 MiB) | 42.6 (15.9 MiB) | 45.2 (17.9 MiB) | -1.9% | +6.1% |
| larson10 | M ops/s | 360.1 (111.4 MiB) | 368.8 (112.4 MiB) | 387.9 (132.6 MiB) | -2.4% | +5.2% |
| xmalloc | M frees/s | 571.221 (53.6 MiB) | 582.400 (53.3 MiB) | 479.886 (79.0 MiB) | -1.9% | -17.6% |
| alloc-test1 | s | 2.736 (13.1 MiB) | 2.699 (13.1 MiB) | 2.571 (12.3 MiB) | -1.4% | +5.0% |
| valkey1 | M ops/s | 6.371 (584.2 MiB) | 6.385 (584.3 MiB) | 6.589 (585.3 MiB) | -0.2% | +3.2% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24378422 ops/s |

`quick: go`; every row reads new ahead of old on its one round
(`valkey1` +0.2%, `larson1` +1.9%, `larson10` +2.4%, `xmalloc` +1.9%,
`alloc-test1` +1.4%).

## 3. `ab`, the protocol tier (C4A, 5 rounds + warm-up, interleaved, `old=63bd7ee new=9113e9c jemalloc`)

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.9 | 15.9 | 42.5 | 16.0 | 45.0 | 18.0 | -1.4% | -6.9% |
| larson10 | M ops/s | 363.4 | 111.7 | 368.4 | 112.3 | 384.9 | 132.3 | -1.4% | -5.6% |
| larson16 | M ops/s | 584.4 | 166.9 | 592.5 | 165.9 | 612.4 | 206.6 | -1.4% | -4.6% |
| xmalloc | M frees/s | 571.999 | 54.6 | 584.892 | 55.6 | 476.433 | 82.3 | -2.2% | +20.1% |
| alloc-test1 | s | 2.734 | 13.1 | 2.734 | 13.1 | 2.565 | 12.3 | +0.0% | -6.2% |
| alloc-testN | s | 2.314 | 16.5 | 2.305 | 16.5 | 2.201 | 16.5 | -0.4% | -4.9% |
| valkey1 | M ops/s | 6.637 | 584.7 | 6.602 | 584.6 | 6.527 | 585.6 | +0.5% | +1.7% |
| valkey1-free | M ops/s | 6.220 | 584.6 | 6.078 | 584.6 | 6.313 | 585.6 | +2.3% | -1.5% |
| valkey1-write | M ops/s | 6.565 | 585.1 | 6.658 | 585.1 | 6.601 | 586.1 | -1.4% | -0.5% |

(`new vs old`: positive = new worse.) The larson family reads new ahead
by 1.4% on all three rows with the five-round lists not overlapping
(`larson1` 41.8..41.9 against 42.5 x5; `larson10` 359.6..368.0 against
366.1..370.9; `larson16` 582.8..589.4 against 590.0..596.2), `xmalloc`
+2.2% ahead (571.6..573.3 against 581.1..585.6, no overlap),
`alloc-test1` 0.0%, `alloc-testN` -0.4%; the Valkey rows are within
their band (`valkey1` +0.5% behind, `valkey1-free` +2.3% behind with
overlapping lists 6.04..6.26 against 6.08..6.26, `valkey1-write` -1.4%
ahead). The sorted lists:

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 41.8 41.8 41.9 41.9 41.9 | 16272 |
| larson1 | new | 42.5 42.5 42.5 42.5 42.5 | 16400 |
| larson1 | jemalloc | 45.0 45.0 45.0 45.0 45.1 | 18448 |
| larson10 | old | 359.6 360.4 363.4 365.8 368.0 | 114428 |
| larson10 | new | 366.1 367.1 368.4 370.7 370.9 | 114960 |
| larson10 | jemalloc | 380.7 383.3 384.9 385.0 391.2 | 135508 |
| larson16 | old | 582.8 584.2 584.4 588.8 589.4 | 170880 |
| larson16 | new | 590.0 591.4 592.5 594.7 596.2 | 169872 |
| larson16 | jemalloc | 610.7 612.3 612.4 614.1 615.6 | 211548 |
| xmalloc | old | 571.583 571.967 571.999 572.860 573.310 | 55892 |
| xmalloc | new | 581.064 583.622 584.892 585.004 585.577 | 56916 |
| xmalloc | jemalloc | 475.089 475.172 476.433 479.098 485.291 | 84288 |
| alloc-test1 | old | 2.732 2.733 2.734 2.735 2.755 | 13464 |
| alloc-test1 | new | 2.718 2.725 2.734 2.749 2.751 | 13464 |
| alloc-test1 | jemalloc | 2.546 2.559 2.565 2.571 2.580 | 12640 |
| alloc-testN | old | 2.294 2.314 2.314 2.328 2.332 | 16920 |
| alloc-testN | new | 2.287 2.302 2.305 2.305 2.306 | 16920 |
| alloc-testN | jemalloc | 2.199 2.200 2.201 2.202 2.203 | 16864 |
| valkey1 | old | 6.598 6.604 6.637 6.639 6.677 | 598740 |
| valkey1 | new | 6.547 6.556 6.602 6.604 6.664 | 598616 |
| valkey1 | jemalloc | 6.453 6.507 6.527 6.547 6.618 | 599640 |
| valkey1-free | old | 6.082 6.210 6.220 6.237 6.256 | 598616 |
| valkey1-free | new | 6.040 6.044 6.078 6.254 6.259 | 598612 |
| valkey1-free | jemalloc | 6.174 6.238 6.313 6.489 6.493 | 599644 |
| valkey1-write | old | 6.494 6.547 6.565 6.634 6.651 | 599128 |
| valkey1-write | new | 6.349 6.408 6.658 6.666 6.679 | 599128 |
| valkey1-write | jemalloc | 6.536 6.595 6.601 6.642 6.672 | 600156 |

## 4. Conclusion (aarch64)

The frame change delivers what task 4c predicted on this architecture:
the `stp`/`ldp` of `x29`, `x30` and the frame pointer move are gone from
`malloc`, `free`, `sdallocx` and the two new entry points, `valkey1-write`
runs 5 fewer instructions per SET (450.0 -> 445.0; jemalloc 390) and
`larson1` 5 fewer per operation, and the protocol tier reads the larson
family +1.4% and `xmalloc` +2.2% with separated lists; the mixed
`valkey1` row, which paid for the scheduled pop's re-dispatch at
97859aa, is back to -1.6 instructions per operation at 9113e9c. `quick`
go; no criterion moves the wrong way beyond its band.
