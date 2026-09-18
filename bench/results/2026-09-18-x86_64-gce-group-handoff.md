# challoc group-handoff, 2026-09-18, x86_64

- commit: 1fc93f6
- date: 2026-09-18T20:27:54Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.41 0.96 0.56; at end: 4.47 4.46 3.01; 815 s total
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
| mstress | old | 0.616 0.618 0.624 0.633 0.643 | 345996 |
| mstress | new | 0.617 0.622 0.623 0.624 0.653 | 346564 |
| mstress | control | 0.630 0.634 0.636 0.640 0.644 | 349192 |
| mstress | jemalloc | 1.046 1.049 1.051 1.064 1.076 | 292164 |
| larson1 | old | 42.9 45.0 45.5 45.5 45.5 | 15356 |
| larson1 | new | 42.8 44.5 44.7 45.0 45.6 | 15392 |
| larson1 | control | 43.2 44.6 44.6 45.2 45.4 | 15168 |
| larson1 | jemalloc | 41.1 44.4 45.0 45.1 45.3 | 18116 |
| larson10 | old | 270.9 285.0 286.2 286.6 287.1 | 95840 |
| larson10 | new | 269.3 278.6 279.4 280.3 281.0 | 97864 |
| larson10 | control | 278.3 279.3 282.3 282.9 287.8 | 96004 |
| larson10 | jemalloc | 286.7 286.7 288.5 290.8 302.2 | 94676 |
| xmalloc | old | 325.348 344.436 344.774 351.303 356.552 | 54188 |
| xmalloc | new | 324.424 338.339 349.502 350.623 360.427 | 45832 |
| xmalloc | control | 317.456 331.557 356.743 358.369 365.039 | 53844 |
| xmalloc | jemalloc | 320.084 330.895 330.933 331.061 332.207 | 92048 |
| alloc-test1 | old | 3.403 3.404 3.408 3.423 3.445 | 13832 |
| alloc-test1 | new | 3.404 3.408 3.410 3.419 3.440 | 13896 |
| alloc-test1 | control | 3.409 3.410 3.414 3.416 3.430 | 13832 |
| alloc-test1 | jemalloc | 3.218 3.218 3.223 3.224 3.247 | 13340 |
| valkey1 | old | 13.968 14.183 14.252 14.315 14.579 | 599520 |
| valkey1 | new | 13.776 14.119 14.156 14.237 14.400 | 599484 |
| valkey1 | control | 13.903 14.322 14.385 14.498 14.500 | 599528 |
| valkey1 | jemalloc | 13.991 14.787 14.808 14.878 14.935 | 606520 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | control | control RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | control vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| mstress | s | 0.624 | 337.9 | 0.623 | 338.4 | 0.636 | 341.0 | 1.051 | 285.3 | -0.2% | +1.9% | +68.4% |
| larson1 | M ops/s | 45.5 | 15.0 | 44.7 | 15.0 | 44.6 | 14.8 | 45.0 | 17.7 | +1.8% | +2.0% | +1.1% |
| larson10 | M ops/s | 286.2 | 93.6 | 279.4 | 95.6 | 282.3 | 93.8 | 288.5 | 92.5 | +2.4% | +1.4% | -0.8% |
| xmalloc | M frees/s | 344.774 | 52.9 | 349.502 | 44.8 | 356.743 | 52.6 | 330.933 | 89.9 | -1.4% | -3.4% | +4.2% |
| alloc-test1 | s | 3.408 | 13.5 | 3.410 | 13.6 | 3.414 | 13.5 | 3.223 | 13.0 | +0.1% | +0.2% | -5.4% |
| valkey1 | M ops/s | 14.252 | 585.5 | 14.156 | 585.4 | 14.385 | 585.5 | 14.808 | 592.3 | +0.7% | -0.9% | -3.8% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

## Context (Phase 1.5 task 15, the whole-group handoff)

- old = 8630ca2 (the parent), new = 01e15ad (the handoff, `group_handoff`
  default `true` at that commit; the tree synced was HEAD 1fc93f6, whose
  `src/` is 01e15ad's), control = the same library with
  `MALLOC_CONF=group_handoff:false`, jemalloc 5.3.1. VM
  `challoc-bench-amd64`, c4-standard-16, zone `us-east4-a`, started for
  this iteration and stopped after it. The aarch64 half is
  `2026-09-18-aarch64-gce-group-handoff.md`.
- The `ab` above is the mstress row the task is about, plus the fast tier's
  rows so that the control has five-round rows next to `new`.

## Fast tier (`challoc-bench quick amd64 OLD=8630ca2`, one round, 20:21Z)

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 20.6 (13.2 MiB) | 43.3 (15.0 MiB) | 44.0 (18.0 MiB) | -52.4% | +1.6% |
| larson10 | M ops/s | 286.0 (94.0 MiB) | 279.2 (94.8 MiB) | 292.0 (92.5 MiB) | +2.4% | +4.6% |
| xmalloc | M frees/s | 354.504 (52.6 MiB) | 356.346 (44.7 MiB) | 331.437 (90.4 MiB) | -0.5% | -7.0% |
| alloc-test1 | s | 3.402 (13.5 MiB) | 3.414 (13.5 MiB) | 3.216 (12.8 MiB) | +0.4% | +6.2% |
| valkey1 | M ops/s | 14.066 (585.0 MiB) | 13.589 (585.0 MiB) | 14.889 (591.9 MiB) | +3.5% | +9.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6668 (49.9%) | new compacts | |

`quick: go` (125 s). larson1's -52.4% is the known bimodal old run on this
VM (20.6 against 45.5 median over five rounds in the `ab`), in new's
favour, so no confirmation rounds were owed. xmalloc -0.5% here, -1.4%
(new) and -3.4% (control) over five rounds: the Mac's +6.0% does not
reproduce, no xmalloc attribution taken. The control's five-round rows
against old: larson1 +2.0%, larson10 +1.4%, xmalloc -3.4%, alloc-test1
+0.2%, valkey1 -0.9%.

## larson10 counters (`challoc-bench quick-perf amd64 OLD=8630ca2 ROWS=larson10`)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson10 | old | 289.39 | 114.2 | 235.9 | 1.518 | 0.016 |
| larson10 | new | 281.46 | 117.7 | 235.9 | 1.498 | 0.014 |
| larson10 | jemalloc | 285.20 | 115.3 | 201.1 | 2.186 | 0.004 |

L1D misses per operation 1.518 -> 1.498, instructions equal: the
line-ownership invariant holds.

## The mstress row

Five timed rounds, maxrss KiB per run, sorted:

| variant | maxrss KiB, sorted | median MiB | vs jemalloc |
|---|---|---:|---:|
| old | 338156 345356 345996 347916 356832 | 337.9 | +18.4% |
| new | 340816 345304 346564 346748 352624 | 338.4 | +18.6% |
| control | 347668 348384 349192 351120 353772 | 341.0 | +19.5% |
| jemalloc | 290148 291292 292164 292428 296012 | 285.3 | |

The three challoc lists overlap: the handoff changes nothing on this row
(new vs old +0.2%, control +0.9%), and the row stays where acceptance run 3
left it (+19.1% there).

## Probe: what moved (`bench/probes/arenasample.c`, this VM)

`mstress 16 50 25` under `bench/rt`, `ARENASAMPLE_MS=100`,
`ARENASAMPLE_SMAPS=1`, two runs per library right after the `ab`
(`~/logs/gh-probe.log`; load 4.4 from the `ab` just finished). The "peak"
sample is the probe's (largest small `active`); `narenas:1` under the new
library is the reference the four terms of acceptance run 3 were measured
against (the huge region is 156..160 MiB in every row and not part of the
gap). MiB.

| library | conf | VmRSS at peak | small: allocated / active / partial waste / Empty (dirty) | large classes: allocated / active / Empty (nominal) | metadata | smaps classes / meta |
|---|---|---:|---|---|---:|---|
| old 8630ca2 | default | 333.2, 334.7 | 31.2, 30.6 / 41.0, 40.4 / 9.8, 9.8 / **20.5, 20.8** | 202, 197 / 225, 223 / 74, 70 | 12.6 | 172.1, 166.5 / 10.8 |
| new 01e15ad | default (handoff on) | 335.0, 331.8 | 34.1, 32.2 / 43.8, 41.1 / 9.7, 9.0 / **9.8, 10.6** | 218, 215 / 238, 238 / 55, 67 | 12.5 | 173.8, 164.7 / 10.8 |
| new | `group_handoff:false` | 334.2, 344.5 | 28.4, 32.7 / 37.9, 41.7 / 9.6, 9.1 / **20.4, 24.5** | 162, 210 / 203, 232 / 88, 71 | 12.6 | 166.2, 179.5 / 10.8 |
| new | `narenas:1` | 280.5, 270.2 | 32.9, 25.1 / 33.5, 27.3 / 0.6, 2.2 / 1.0, 4.8 | 210, 162 / 224, 191 / 32, 56 | 2.4 | 125.6, 125.3 / 1.2 |

The four terms of the 17-arena excess over `narenas:1` (acceptance run 3:
Empty small slabs +18, partial small slabs +9, touched pages of Empty
64 KiB-slab classes +21, metadata +9.5):

| term | old | new (handoff on) | control | moved? |
|---|---:|---:|---:|---|
| Empty small slabs idle at the peak | +16..20 | +5..10 | +16..23 | yes, halved |
| partial small slabs (active - allocated) | +7..9 | +7..9 | +7..9 | no: a partial slab is `Active`, never idle |
| metadata | +10.2 | +10.1 | +10.2 | no: per-arena tables and states |
| classes RSS minus the two small terms (the 64 KiB-slab classes' touched pages) | +12..24 | +18..36 | +16..34 | no (noise band +-7) |
| `challoc:classes` RSS | +41..47 | +39..48 | +41..54 | no |
| maxrss (five rounds, above) | 337.9 | 338.4 | 341.0 | no |

Reading. The handoff fires on this row: the Empty small slabs waiting at
the peak halve (a small class's group is one slab, so every Empty small
slab is an idle group), and the large classes' Empty nominal bytes drop
(their donor's current group, mostly `Fresh`, is idle as a whole). The
row's peak RSS does not follow, within the probe's resolution (two samples
per variant of a 0.62 s random workload whose classes-region RSS varies by
+-7 MiB run to run, against a 10 MiB effect looked for) and, decisively,
within the five-round protocol (lists overlapping). The rest of the excess
is `Active` at the peak and out of a whole-group handoff's reach: one
partial slab per class per arena, the per-arena metadata, and the 64
KiB-slab classes' current groups (64..128 slabs each), which under churn
always hold an `Active` slab and so are never idle as a group; and since
every arena reaches its peak in the same phase of the run, an adopted
`Empty` slab mostly replaces a fresh commit that its donor's own next
refill would otherwise have avoided, so the sum over arenas of resident
bytes at the peak is the same. The task's acceptance text for this case
applies: the residual is attributed and the row is accepted as the price of
per-class regions at 16 arenas (the review's (c)); design 13.3 says so.

## Decision

See the aarch64 file: the default flips to `false` (the handoff does not
move mstress on either VM and costs the aarch64 row 2.4%); the code stays
as the opt-in for producer-consumer patterns (xmalloc RSS 52.9 -> 44.8 MiB
here). `unit/memefficiency` ran on the aarch64 VM (33 ok).
