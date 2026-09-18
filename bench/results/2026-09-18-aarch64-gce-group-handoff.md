# challoc group-handoff, 2026-09-18, aarch64

- commit: 1fc93f6
- date: 2026-09-18T20:27:50Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.16 0.71 0.49; at end: 4.98 4.76 3.14; 790 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - old: 8630ca27888a built into target/ab/8630ca27888a
  - new: 01e15adc789b built into target/ab/01e15adc789b
  - control: 01e15adc789b built into target/ab/01e15adc789b, MALLOC_CONF=group_handoff:false
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| mstress | old | 0.391 0.393 0.395 0.395 0.398 | 325680 |
| mstress | new | 0.392 0.394 0.396 0.396 0.397 | 336624 |
| mstress | control | 0.394 0.395 0.397 0.398 0.399 | 328824 |
| mstress | jemalloc | 0.655 0.658 0.659 0.660 0.664 | 300632 |
| larson1 | old | 42.5 42.6 42.6 42.6 42.7 | 16104 |
| larson1 | new | 42.2 42.2 42.3 42.3 42.4 | 16104 |
| larson1 | control | 42.2 42.2 42.4 42.4 42.4 | 16108 |
| larson1 | jemalloc | 45.0 45.0 45.0 45.0 45.1 | 18304 |
| larson10 | old | 364.2 366.3 369.4 371.0 371.9 | 115448 |
| larson10 | new | 364.6 365.1 366.1 367.3 369.4 | 116732 |
| larson10 | control | 362.6 366.0 366.2 366.9 367.4 | 114812 |
| larson10 | jemalloc | 382.4 386.5 387.6 389.6 390.3 | 135368 |
| xmalloc | old | 609.801 612.489 612.554 613.320 613.491 | 57488 |
| xmalloc | new | 603.673 604.452 604.509 606.349 606.789 | 45648 |
| xmalloc | control | 603.453 605.099 605.379 606.343 606.362 | 56288 |
| xmalloc | jemalloc | 470.623 474.898 477.817 482.098 483.042 | 81896 |
| alloc-test1 | old | 2.705 2.709 2.714 2.714 2.718 | 13316 |
| alloc-test1 | new | 2.689 2.703 2.705 2.736 2.744 | 13316 |
| alloc-test1 | control | 2.697 2.699 2.703 2.727 2.732 | 13316 |
| alloc-test1 | jemalloc | 2.538 2.555 2.565 2.567 2.579 | 12620 |
| valkey1 | old | 6.732 6.792 6.829 6.871 6.960 | 598608 |
| valkey1 | new | 6.779 6.808 6.834 6.839 6.863 | 598608 |
| valkey1 | control | 6.577 6.588 6.730 6.738 6.831 | 598524 |
| valkey1 | jemalloc | 6.778 6.809 6.930 6.936 6.961 | 599620 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | control | control RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | control vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| mstress | s | 0.395 | 318.0 | 0.396 | 328.7 | 0.397 | 321.1 | 0.659 | 293.6 | +0.3% | +0.5% | +66.8% |
| larson1 | M ops/s | 42.6 | 15.7 | 42.3 | 15.7 | 42.4 | 15.7 | 45.0 | 17.9 | +0.7% | +0.5% | -5.3% |
| larson10 | M ops/s | 369.4 | 112.7 | 366.1 | 114.0 | 366.2 | 112.1 | 387.6 | 132.2 | +0.9% | +0.9% | -4.7% |
| xmalloc | M frees/s | 612.554 | 56.1 | 604.509 | 44.6 | 605.379 | 55.0 | 477.817 | 80.0 | +1.3% | +1.2% | +28.2% |
| alloc-test1 | s | 2.714 | 13.0 | 2.705 | 13.0 | 2.703 | 13.0 | 2.565 | 12.3 | -0.3% | -0.4% | -5.5% |
| valkey1 | M ops/s | 6.829 | 584.6 | 6.834 | 584.6 | 6.730 | 584.5 | 6.930 | 585.6 | -0.1% | +1.5% | -1.5% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

## Context (Phase 1.5 task 15, the whole-group handoff)

- old = 8630ca2 (the parent), new = 01e15ad (the handoff, `group_handoff`
  default `true` at that commit; the tree synced was HEAD 1fc93f6, whose
  `src/` is 01e15ad's), control = the same library with
  `MALLOC_CONF=group_handoff:false`, jemalloc 5.3.1. VM
  `challoc-bench-arm64`, c4a-standard-16, zone `us-east4-a`, started for
  this iteration and stopped after it. The x86_64 half is
  `2026-09-18-x86_64-gce-group-handoff.md`, which also has the
  `arenasample` probe.
- The `ab` above is the mstress row the task is about, plus the fast tier's
  rows so that the control has five-round rows next to `new`.

## Fast tier (`challoc-bench quick arm64 OLD=8630ca2`, one round, 20:19Z)

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.6 (15.7 MiB) | 42.4 (15.7 MiB) | 45.2 (17.4 MiB) | +0.5% | +6.6% |
| larson10 | M ops/s | 372.0 (112.5 MiB) | 362.5 (112.6 MiB) | 387.5 (132.6 MiB) | +2.6% | +6.9% |
| xmalloc | M frees/s | 609.370 (53.9 MiB) | 606.405 (44.6 MiB) | 477.648 (79.0 MiB) | +0.5% | -21.2% |
| alloc-test1 | s | 2.712 (13.0 MiB) | 2.696 (13.0 MiB) | 2.571 (12.2 MiB) | -0.6% | +4.9% |
| valkey1 | M ops/s | 6.876 (584.1 MiB) | 6.841 (584.1 MiB) | 6.880 (585.4 MiB) | +0.5% | +0.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go` (every row within 5% of old, churn compacts; 122 s). The Mac
plumbing tier's xmalloc +6.0% (PROGRESS, task 15) does not reproduce here:
+0.5% in this round, +1.3% (new) and +1.2% (control) over five rounds in
the `ab` above, so the attribution the task owed for a failing xmalloc row
is not taken. The control's five-round rows against old: larson1 +0.5%,
larson10 +0.9%, xmalloc +1.2%, alloc-test1 -0.4%, valkey1 +1.5% (Medians
table above).

## larson10 counters (`challoc-bench quick-perf arm64 OLD=8630ca2 ROWS=larson10`)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson10 | old | 364.75 | 82.0 | 249.6 | 2.245 | 4.234 |
| larson10 | new | 363.24 | 82.4 | 249.7 | 2.240 | 4.266 |
| larson10 | jemalloc | 379.62 | 78.5 | 224.8 | 2.226 | 1.867 |

L1D misses per operation 2.245 -> 2.240: the line-ownership invariant of
task 4f holds (whole groups only, no slab shares lines between arenas).

## The mstress row

Five timed rounds, maxrss KiB per run (the `ab`'s raw `.scores`, warm-up
dropped), sorted:

| variant | maxrss KiB, sorted | median MiB | vs jemalloc |
|---|---|---:|---:|
| old | 321016 321632 325680 327784 328752 | 318.0 | +8.3% |
| new | 329608 335996 336624 337140 339580 | 328.7 | +12.0% |
| control | 325564 328560 328824 331500 336364 | 321.1 | +9.4% |
| jemalloc | 298880 300116 300632 301440 304516 | 293.6 | |

Four of new's five runs sit above the control's largest: with the handoff
on, this row's RSS is 2.4% higher than with it off on the same library
(+3.4% against old), and the row moves from inside the 10% band (old
+8.3%, control +9.4%) to outside (+12.0%). Time is unchanged (0.395 /
0.396 / 0.397 s; jemalloc 0.659). The mechanism of the aarch64 cost is
not attributed here (the probe ran on x86_64, where the lists overlap and
nothing moved; see that file); it is recorded as the row's own measurement.

## Valkey (`make valkey STAGE=build`, then `STAGE=memefficiency`, this VM)

`unit/memefficiency` at the synced tree (handoff on): 33 `[ok]`, "All
tests passed without errors!" (`~/logs/gh-memeff.log`, the suite's log
`target/valkey/logs/memefficiency-aarch64.log`). The build check: `Valkey
server v=9.1.2 ... malloc=jemalloc-5.3.0-challoc`, `DT_NEEDED
libchalloc.so.0`. The soak is not rerun: with the default flipped to
`false` (below) the soak's code path is the parent's, whose soak stands
(`2026-09-18-aarch64-gce-decay-soak-valkey-soak.md`).

## Decision

The handoff does not move the row it was built for on either VM (x86_64
+0.2% against old with fully overlapping lists; aarch64 +3.4% against old,
+2.4% against its own control) and takes the aarch64 row outside the band,
so the `group_handoff` default is flipped to `false` in the commit that
follows this file; the code stays as the opt-in for the pattern it does
serve (producer-consumer: xmalloc RSS 56.1 -> 44.6 MiB here, 52.9 -> 44.8
on x86_64, throughput within noise). The mstress row is accepted as the
review's (c), the price of per-class regions at 16 arenas, with the
attribution in the x86_64 file; design 13.3 says so.
