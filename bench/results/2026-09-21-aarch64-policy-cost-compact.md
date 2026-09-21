# Policy cost: the compacting cache off versus on, 2026-09-21, aarch64 (lane policy-cost)

Measurement only, no default changed, nothing under `src/` changed. The
question (coordinator, 2026-09-21): what the scheduled compacting pop of
design 6.4 (`tcache_compact_shift`, default 3: one pop in 8 of a class is
served from the lowest-addressed cached slot while the class is
fragmented and not being refilled) costs on the hot path, and what it
buys, against the same library with the schedule off
(`MALLOC_CONF=tcache_compact_shift:0`), in one run. The x86_64 rows are
in `2026-09-21-x86_64-policy-cost-compact.md`; the slab-geometry rows of
the same lane in `2026-09-21-aarch64-policy-cost-geometry.md`.

- host: `challoc-policy-cost-arm64` (c4a-standard-16, Google Axion,
  Neoverse V2, CPU part 0xd4f), 16 vCPUs, Ubuntu 24.04 kernel
  `7.0.0-1011-gcp`, 4 KiB pages, THP `madvise`; container: Wolfi
  (rustc 1.98.1); jemalloc = Wolfi's libjemalloc.so.2 5.3.1-r3 (the
  aarch64 build with `Page size: 65536`, so its slab counts below differ
  from x86_64's). Zone us-east4-a. Nothing else ran on the VM.
- commit: `f53418d` on `loop/policy-cost` (harness hooks only; `src/`
  is byte for byte main `2078f67`). The library is one build,
  `target/quick/{old,new}` and `target/ab/tree`, sha256 prefix
  `4d04e89158b5bcc4`, `quantum16`; both challoc variants run this same
  file and differ only in `MALLOC_CONF`.
- variants: `default` = MALLOC_CONF unset (`opt.tcache_compact_shift: 3`,
  the probe/compact schedule of design 6.4); `compact0` =
  `MALLOC_CONF=tcache_compact_shift:0` (the cache starts in `MODE_OFF`,
  `compact_mask` never matches, no scheduled pop, no probe, no rebuild);
  `jemalloc` as the reference.
- method: `challoc-bench quick` (one round, `OLD=HEAD NEW=tree
  NEW_CONF=tcache_compact_shift:0`, same build both sides), `challoc-bench
  quick-perf` (`sudo perf stat --inherit -p` on the container's process,
  one run per row and variant, counters divided by the program's own
  operation count over the whole run), `challoc-bench ab` (5 timed rounds
  after one warm-up, interleaved, `larson1 larson10 larson16 xmalloc
  alloc-test1 alloc-testN valkey1 valkey1-free`), then the Valkey soak
  and the Ruby soak (sections 5 and 6). A first `ab` attempt of this lane
  measured glibc for both challoc variants (a harness bug: the build
  directory name carried spaces and `LD_PRELOAD` split it; fixed in
  `f53418d`, which also makes `ab.sh` refuse such a path) and was
  discarded; the tables below are the rerun with the fix.

## 1. `quick` (one round, go/no-go shape): `default` (old) vs `compact0` (new)

| row | unit | old (default) | new (compact0) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.0 (15.9 MiB) | 42.1 (16.0 MiB) | 45.2 (18.4 MiB) | -0.2% | +7.4% |
| larson10 | M ops/s | 370.7 (112.2 MiB) | 370.3 (112.4 MiB) | 393.1 (133.3 MiB) | +0.1% | +6.2% |
| xmalloc | M frees/s | 573.293 (53.3 MiB) | 573.914 (53.0 MiB) | 481.490 (76.3 MiB) | -0.1% | -16.1% |
| alloc-test1 | s | 2.567 (13.1 MiB) | 2.534 (13.1 MiB) | 2.405 (12.3 MiB) | -1.3% | +5.4% |
| valkey1 | M ops/s | 7.866 (584.3 MiB) | 7.756 (584.3 MiB) | 8.267 (585.3 MiB) | +1.4% | +6.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 6801 (49.2%) | 469 -> 469 (44.3%) | new DOES NOT COMPACT (end occupancy >= 90%) | 23066384 ops/s |

Load 3.40 at start (the tail of the previous step), 125 s. `old and new
are the same build (4d04e89158b5bcc4)`. The gate's verdict is NO-GO on
the churn row alone, which is the point: with the schedule off the
churn probe's class stays at 6801 slabs and 49% occupancy for the whole
8 s (jemalloc likewise, 469 slabs of 64 KiB pages at 44%), where the
default drains it to 3462 slabs at 97% in the same time. The throughput
rows are within a round's noise.

## 2. `quick-perf`: per-operation counters, whole run

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | default | 41.75 | 71.8 | 264.5 | 2.106 | 3.985 |
| larson1 | compact0 | 41.75 | 71.7 | 264.5 | 2.099 | 4.008 |
| larson1 | jemalloc | 44.81 | 66.5 | 224.8 | 2.071 | 1.730 |
| valkey1 | default | 14.00 | 216.4 | 244.0 | 3.071 | 2.792 |
| valkey1 | compact0 | 16.49 | 183.7 | 204.1 | 2.896 | 2.669 |
| valkey1 | jemalloc | 14.76 | 204.9 | 170.0 | 2.974 | 2.688 |
| valkey1-free | default | 13.78 | 219.8 | 244.8 | 4.066 | 3.894 |
| valkey1-free | compact0 | 14.87 | 203.8 | 207.5 | 3.916 | 3.752 |
| valkey1-free | jemalloc | 14.29 | 211.6 | 172.5 | 3.413 | 3.297 |

Load 4.82 at start. The M ops/s column is the program's total count
over its wall time (both phases of the model), not the `write_mops` the
`quick`/`ab` rows score.

The same runs, per phase of the model (`valkey_model -n 850000 -t 5`:
fill, 5 s of SET over the full 850k-key heap, 5 s of DEL:SET 4:1 that
drains the keyspace to 170k):

| row | variant | write M ops/s | delete M ops/s | RSS MiB after delete (live 98 MiB) | VmHWM MiB |
|---|---|---:|---:|---:|---:|
| valkey1 | default | 7.574 | 20.422 | 187.2 | 584.7 |
| valkey1 | compact0 | 8.158 | 24.826 | 407.8 | 584.8 |
| valkey1 | jemalloc | 7.553 | 21.965 | 585.0 | 585.7 |
| valkey1-free | default | 7.589 | 19.978 | 187.1 | 584.7 |
| valkey1-free | compact0 | 7.432 | 22.309 | 424.9 | 584.7 |
| valkey1-free | jemalloc | 7.266 | 21.319 | 585.1 | 585.6 |

Reading: larson never engages the schedule (its classes are dense; the
probe every 4096 pops answers `Dense` and the cache stays in probe
mode), and its counters are identical to the instruction: the mode word
test on the pop path costs nothing measurable. The model's write phase
is the same story (7.57 vs 8.16 and 7.59 vs 7.43 M ops/s, one run each,
inside the row's spread). The whole cost is in the delete phase, where
the DEL:SET churn leaves holes, the probe answers `Fragmented` and one
pop in 8 becomes a compacting pop with its run rebuilds (a `cap / 4`
claim from the class's lowest partial slab under the class lock, the
flush that makes room for it, the displaced frees returned high): 20.4
vs 24.8 M ops/s (-18%) through `sdallocx`, 20.0 vs 22.3 (-10%) through
plain `free`, and over the whole run 40 fewer instructions and 33 fewer
cycles per operation with the schedule off (244 -> 204, 216 -> 184).
What the same 5 s buys: the default ends the delete phase at 187 MiB
resident for 98 MiB of live values, the schedule-off run at 408..425
MiB, jemalloc at 585 (its decay has not returned anything yet in that
window). That is the trade in miniature: about a fifth of the delete
phase's allocator throughput for 220 MiB (55% of the peak) given back
within five seconds.

## 3. `ab` (5 rounds, interleaved): `default`, `compact0`, `jemalloc`

Load 0.53 at start, 5.25 at the end, 933 s.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | default | 41.8 41.8 42.0 42.0 42.1 | 16400 |
| larson1 | compact0 | 42.0 42.0 42.0 42.1 42.1 | 16272 |
| larson1 | jemalloc | 45.1 45.1 45.1 45.1 45.1 | 18832 |
| larson10 | default | 364.6 367.4 367.8 368.4 371.9 | 114576 |
| larson10 | compact0 | 364.6 365.2 365.8 366.0 366.5 | 114960 |
| larson10 | jemalloc | 383.3 386.6 386.8 387.8 388.9 | 135560 |
| larson16 | default | 585.6 585.9 587.3 588.3 591.2 | 170256 |
| larson16 | compact0 | 589.0 592.0 592.4 593.0 595.0 | 169612 |
| larson16 | jemalloc | 613.2 615.9 616.8 620.9 621.5 | 211940 |
| xmalloc | default | 575.150 575.581 576.040 576.580 577.210 | 54536 |
| xmalloc | compact0 | 574.245 576.402 576.640 576.886 576.887 | 54484 |
| xmalloc | jemalloc | 475.672 477.058 478.655 481.498 483.812 | 80604 |
| alloc-test1 | default | 2.513 2.533 2.548 2.582 2.598 | 13464 |
| alloc-test1 | compact0 | 2.492 2.500 2.548 2.559 2.594 | 13460 |
| alloc-test1 | jemalloc | 2.406 2.419 2.440 2.458 2.460 | 12640 |
| alloc-testN | default | 2.305 2.306 2.306 2.307 2.309 | 16972 |
| alloc-testN | compact0 | 2.302 2.304 2.304 2.304 2.304 | 16912 |
| alloc-testN | jemalloc | 2.196 2.197 2.199 2.199 2.207 | 16864 |
| valkey1 | default | 7.609 8.467 8.729 8.749 8.826 | 598868 |
| valkey1 | compact0 | 7.644 8.213 8.241 8.358 8.684 | 598864 |
| valkey1 | jemalloc | 8.237 8.300 8.336 8.378 9.048 | 599764 |
| valkey1-free | default | 7.039 7.619 8.424 8.546 8.687 | 598800 |
| valkey1-free | compact0 | 6.660 6.713 7.334 8.029 8.533 | 598740 |
| valkey1-free | jemalloc | 7.526 8.107 8.142 8.367 8.727 | 599764 |

| program | unit | default | default RSS MiB | compact0 | compact0 RSS MiB | jemalloc | jemalloc RSS MiB | compact0 vs default | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.0 | 16.0 | 42.0 | 15.9 | 45.1 | 18.4 | +0.0% | -6.9% |
| larson10 | M ops/s | 367.8 | 111.9 | 365.8 | 112.3 | 386.8 | 132.4 | +0.5% | -4.9% |
| larson16 | M ops/s | 587.3 | 166.3 | 592.4 | 165.6 | 616.8 | 207.0 | -0.9% | -4.8% |
| xmalloc | M frees/s | 576.040 | 53.3 | 576.640 | 53.2 | 478.655 | 78.7 | -0.1% | +20.3% |
| alloc-test1 | s | 2.548 | 13.1 | 2.548 | 13.1 | 2.440 | 12.3 | +0.0% | -4.2% |
| alloc-testN | s | 2.306 | 16.6 | 2.304 | 16.5 | 2.199 | 16.5 | -0.1% | -4.6% |
| valkey1 | M ops/s | 8.729 | 584.8 | 8.241 | 584.8 | 8.336 | 585.7 | +5.9% | +4.7% |
| valkey1-free | M ops/s | 8.424 | 584.8 | 7.334 | 584.7 | 8.142 | 585.7 | +14.9% | +3.5% |

Positive percentages mean the variant did worse than `default`.

Reading: on the six mimalloc-bench rows the two variants are the same
library to within 1% with the sorted lists interleaved; nothing on the
`malloc`/`free` path is measurably different when the schedule never
fires. The two model rows score the write phase, in which neither
variant compacts (section 2), and their sorted lists overlap end to end
(default 7.6..8.8, compact0 7.6..8.7; 7.0..8.7 against 6.7..8.5): the
medians' +6% and +15% are this VM's spread on a 5 s single-threaded row
over a 585 MiB heap, not a policy effect (the one-run counters of
section 2 put the schedule-off write phase ahead, the other way round).
The RSS column is the model's peak, the write-phase heap, identical by
construction. The `ab` tier does not see the delete phase; section 2's
per-phase table is where the cost and the benefit both are.

## 4. The churn probe (the benefit the quick tier checks)

`bench/probes/churn.c 1024 30000 8 0.56`: 30000 blocks of 1 KiB, a
random 56% freed, then 8 s of balanced free/alloc churn. From section
1: `default` 6801 -> 3462 slabs (97.0% occupancy at the end; the ideal
is 3300), `compact0` 6801 -> 6801 (49.2%, frozen at the post-delete
layout, exactly the LIFO steady state design 6.4 describes), jemalloc
469 -> 469 (44.3%; this jemalloc's 64 KiB-page build packs 128 regions
per 1 KiB slab and its layout freezes too). The compaction is the only
mechanism in the row that moves the live set; without it the residue of
a delete phase stays where the delete left it until a defragmenter or a
full drain does the work.

## 5. Valkey soak (20 min load + 60 s settle, `activedefrag yes`): `default`, `compact0`, jemalloc

`bench/valkey-soak.sh` (Valkey 9.1.2 from `contrib/valkey/build.sh`,
`quantum8`, `io-threads 1`, `activedefrag yes`, stock `active-defrag-*`;
600 s write phase, 600 s DEL:SET 4:1, 60 s settle; `OCCUPANCY=1`, so
`MEMORY MALLOC-STATS` every 20 s), two invocations back to back on the
idle VM: `ALLOCS='challoc jemalloc'` (the acceptance shape; challoc
first, then the unpatched tag on its bundled jemalloc 5.3.0), then
`ALLOCS=challoc CHALLOC_CONF=tcache_compact_shift:0`. The challoc server
mapped `target/valkey/challoc/release/libchalloc.so` (sha256
`ddfc69c202484485`, the quantum8 build of the same tree) in both runs;
the load generators are the same binaries throughout. Raw files:
`target/valkey/pc-{default,compact0}/` on the VM (summaries, 10 s
samples, the per-20 s stats prints, the servers' final `INFO`); the two
summaries' tables are reproduced here.

| measure | default | compact0 | jemalloc | compact0 vs default | default vs jemalloc |
|---|---:|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 920986 | 933092 | 878208 | +1.3% | +4.9% |
| write: valkey-benchmark p99 ms | 1.071 | 1.055 | 1.119 | -1.5% | -4.3% |
| write: memtier req/s | 964505 | 971213 | 936581 | +0.7% | +3.0% |
| write: memtier p99 ms | 0.959 | 0.951 | 0.983 | -0.8% | -2.4% |
| delete: valkey-benchmark req/s | 1423429 | 1464692 | 1452604 | +2.9% | -2.0% |
| delete: valkey-benchmark p99 ms | 0.719 | 0.687 | 0.703 | -4.5% | +2.3% |
| delete: memtier req/s | 1258879 | 1277187 | 1273913 | +1.5% | -1.2% |
| delete: memtier p99 ms | 0.767 | 0.751 | 0.759 | -2.1% | +1.1% |
| peak RSS MiB (VmHWM) | 579.3 | 580.8 | 573.5 | +0.3% | +1.0% |
| RSS MiB at end of write phase | 579.0 | 580.7 | 573.1 | +0.3% | +1.0% |
| RSS MiB at end of delete phase | 130.3 | 153.4 | 128.6 | +17.7% | +1.3% |
| RSS MiB after settle | 130.3 | 153.1 | 128.5 | +17.5% | +1.4% |
| allocator_frag_ratio after settle | 1.81 | 2.18 | 1.79 | | |
| active_defrag_hits | 229711 | 264450 | 240548 | +15.1% | -4.5% |
| active_defrag_misses | 1034113 | 1025011 | 997207 | | |
| total_active_defrag_time ms (INFO stats, whole run) | 2344 | 2403 | 2546 | +2.5% | -7.9% |
| keys after settle | 105416 | 105352 | 102818 | | |

(In this table's last two columns positive means the first-named
variant is larger: faster on a req/s row, slower on a p99 row, more
RSS on an RSS row.)

Per class after the settle (`MEMORY MALLOC-STATS` at t=1281, summed by
`bench/probes/mstats-classes.sh`; the residue lives in the 512..1024
classes, the soak's values are 16..1024 bytes plus valkey-benchmark's
512):

| class (slab KiB / nregs) | default: occupancy, live / slab / pinned MiB, c_on | compact0: occupancy, live / slab / pinned MiB, c_on | jemalloc util (regs / pgs) |
|---|---|---|---|
| 512 (4 / 8) | 64.5%, 3.4 / 5.3 / 1.9, 1 | 46.2%, 3.4 / 7.3 / 3.9, 0 | 0.699 (8 / 1) |
| 640 (16 / 25) | 45.3%, 8.2 / 18.5 / 10.4, 1 | 41.4%, 8.2 / 20.2 / 12.0, 0 | 0.465 (32 / 5) |
| 768 (16 / 21) | 48.4%, 9.8 / 20.7 / 10.8, 2 (c_off 1) | 44.9%, 9.8 / 22.2 / 12.4, 0 | 0.555 (16 / 3) |
| 896 (8 / 9) | 60.1%, 11.4 / 19.3 / 7.9, 1 | 45.3%, 11.4 / 25.6 / 14.2, 0 | 0.484 (32 / 7) |
| 1024 (4 / 4) | 79.7%, 13.3 / 16.7 / 3.4, 1 | 56.7%, 13.4 / 23.6 / 10.2, 0 | 0.862 (4 / 1) |
| total 512..1024 | 57.4%, 46.2 / 80.5 / 34.3 | 46.7%, 46.1 / 98.8 / 52.7 | |

`allocator_active` after settle: 127.0 MiB (default) against 149.9
(compact0); the 1280..4096 classes hold 0.9 MiB live in 2.2 MiB of
slabs in both.

Reading:

1. The benefit. With the schedule the five residue classes end the
   soak at 57% occupancy and 34 MiB pinned, and the server at 130.3 MiB
   resident, 1.4% above jemalloc's 128.5 (the post-delete criterion is
   +10%); without it they end at 47% and 53 MiB pinned, the server at
   153.1 MiB, +17.5% over the default and +19% over jemalloc, outside
   the criterion, with `allocator_frag_ratio` 2.18 against 1.81. The
   896 and 1024 classes show it most (60% -> 45%, 80% -> 57%): the
   compaction's low-address preference is what packs the churn's
   survivors into the low slabs so the high ones empty and are purged;
   with LIFO caches alone the delete phase's layout is frozen (the
   churn probe of section 4, at soak scale). Valkey's own defragmenter
   is the alternative supplier of this benefit and it did work harder
   without the schedule (264k hits against 230k, 15% more moves, in the
   same 2.4 s of defrag time), but its stop rule (`frag_smallbins` under
   `active-defrag-ignore-bytes`, 100 MiB) is satisfied long before the
   residue of a class of 4..25-slot slabs is gone: the 23 MiB difference
   is what the defragmenter leaves and the compaction removes.
2. The cost. In the server, the delete phase, where the schedule works,
   ran 1.5..2.9% faster in req/s and 2..4.5% better in p99 without it
   (1423k -> 1465k valkey-benchmark, 0.719 -> 0.687 ms); the write
   phase, where it does not engage, is within 1.3% and 1.5% (one run
   each; the soak's run-to-run spread on these rows has been 1..3%).
   That is the model's -18% delete-phase allocator throughput (section
   2) diluted by everything else a Valkey command does: the allocator
   is a few percent of the server's cycles, so a fifth of it is 1..3% of
   the server. The peak RSS is unchanged (+0.3%).
3. Both runs' default-versus-jemalloc rows meet the soak's acceptance
   shape: throughput and p99 ahead of jemalloc in the write phase and
   within 2.3% behind in the delete phase, peak RSS +1.0%, RSS after
   settle +1.4%.

## 6. Ruby soak (600 s, 16 threads): `default` vs `compact0`

PENDING.

## 7. Comparison and the trade

PENDING.
