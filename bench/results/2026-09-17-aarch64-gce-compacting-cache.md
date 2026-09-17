# challoc compacting cache (scheduled address-ordered pops) A/B and soak, 2026-09-17, aarch64 (GCE reference VM)

Phase 1 acceptance step 3 (b)/(c), `docs/plan.md` "Acceptance remediation",
designed in `docs/review-2026-09-17.md` P1 and recorded in `docs/design.md`
6.4: one pop in `2^tcache_compact_shift` (default 8) of a class is served
from a run of low addresses at the bottom of the cache stack instead of
the top, gated per cache by the class's fragmentation and refill rate.
This file is the throughput row before and after per CLAUDE.md (any change
to the `src/heap.rs` hot paths), the churn probe, and the 20-minute Valkey
soak that decides the post-delete RSS criterion. The x86_64 rows are in
`2026-09-17-x86_64-gce-compacting-cache.md`. VM numbers are never
compared with Mac numbers.

- host: challoc-bench-arm64 (c4a-standard-16, Google Axion, Neoverse V2),
  16 vCPUs, Ubuntu 24.04 kernel 7.0.0-1011-gcp, 4 KiB pages,
  `guard_install=yes`; container: Wolfi (rustc 1.98.1, gcc 15.3.0);
  jemalloc = Wolfi's libjemalloc.so.2 (5.3.1)
- method (A/B): `bench/build.sh` programs; `old` = the pre-change library
  built from `git archive 9d92094` into `target/old`, `new` = the named
  commit into `target/new`, `new0` = the same `.so` as `new` with
  `MALLOC_CONF=tcache_compact_shift:0` (schedule off: isolates the
  fast-path counter test and the 48-byte cache header from the schedule).
  5 rounds plus one warm-up, variants interleaved per round, every run's
  score listed sorted; RSS is `rt`'s median maxrss in KiB. Load average at
  the start of each run noted.
- programs: `larson 5 8 1000 5000 100 4141 1` (larson1) and `... 10`
  (larson10), M ops/s, higher is better; `xmalloc-test -w 10 -t 5 -s 64`,
  M frees/s, higher is better; `alloc-test 1` and `alloc-test 16`,
  seconds, lower is better

## Run 1: the first form, rejected (commit cd11e48, `new`), `new5` = `tcache_compact_shift:5`

The first implementation left the refill batch ascending, kept a sorted
run with a cursor and rebuilt it by sorting the whole cache on every
exhaustion, unconditionally. Load 0.00 at the start.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 44.5 44.5 44.5 44.6 44.6 | 13584 |
| larson1 | new | 37.5 37.6 37.6 37.6 37.6 | 14604 |
| larson1 | new5 | 41.4 41.5 41.6 41.6 41.7 | 14596 |
| larson1 | jemalloc | 45.1 45.1 45.1 45.1 45.1 | 18612 |
| larson10 | old | 352.9 354.6 355.4 357.0 359.5 | 87952 |
| larson10 | new | 275.6 275.9 276.0 276.1 276.4 | 84372 |
| larson10 | new5 | 318.2 319.9 322.1 322.4 325.8 | 87828 |
| larson10 | jemalloc | 383.2 384.2 385.2 387.0 388.3 | 133856 |
| xmalloc-test | old | 354.9 380.5 396.9 420.4 426.2 | 39388 |
| xmalloc-test | new | 92.3 92.6 95.1 95.3 98.6 | 39820 |
| xmalloc-test | new5 | 253.0 254.0 270.3 272.0 289.0 | 39168 |
| xmalloc-test | jemalloc | 474.5 474.7 476.2 479.2 479.4 | 80492 |
| alloc-test1 | old | 2.468 2.534 2.534 2.610 2.637 | 13276 |
| alloc-test1 | new | 2.688 2.732 2.733 2.819 2.898 | 14560 |
| alloc-test1 | new5 | 2.551 2.623 2.658 2.670 2.730 | 14560 |
| alloc-test1 | jemalloc | 2.441 2.486 2.499 2.513 2.527 | 12620 |
| alloc-testN | old | 2.221 2.231 2.231 2.232 2.242 | 16476 |
| alloc-testN | new | 2.552 2.556 2.564 2.565 2.571 | 37216 |
| alloc-testN | new5 | 2.341 2.342 2.344 2.348 2.366 | 36960 |
| alloc-testN | jemalloc | 2.199 2.199 2.199 2.199 2.203 | 16716 |

Reading (medians): larson1 -15.5%, larson10 -22%, xmalloc-test -76%,
alloc-test1 +8%, alloc-testN +15% with 2.3x the RSS; at `k = 5` still
-7/-9/-32/+5/+5%. Three causes, each fixed in the second form: (1) pops
from an ascending batch descend, so xmalloc-test's consumer pushes arrive
descending over an ascending kept half and the flush's `sort_unstable`
loses its O(n) sorted-input path (the mirror of the -41% pathology in
`2026-09-16-aarch64-gce-compacting-flush.md`); (2) the rebuild sorted the
cache and claimed on every exhaustion whether or not the class had holes,
and larson's slabs are dense, so it paid slab traffic and cold slots for
nothing; (3) under pure allocation the run ran out at a ninth of the batch
and the rebuild kept claiming, so caches never drained (the RSS and the
inventory).

## Run 2: second form (commit 32bb909), gate on holes only

Descending run (lowest on top, the pre-change refill order), flush
unchanged, rebuild by claiming a `cap / 4` block from the lowest partial
slab and swapping it into the bottom of the stack, schedule gated on the
class holding at least 4 slabs' worth and 1/32 of holes. Load 0.24 at the
start (the tail of a previous run).

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 44.6 44.6 44.6 44.7 44.7 | 13712 |
| larson1 | new | 44.2 44.2 44.3 44.3 44.4 | 13716 |
| larson1 | new0 | 44.1 44.3 44.3 44.3 44.4 | 13588 |
| larson1 | jemalloc | 44.9 45.0 45.1 45.2 45.2 | 19300 |
| larson10 | old | 354.1 355.0 357.1 358.0 360.8 | 88208 |
| larson10 | new | 348.3 350.0 351.0 353.2 353.3 | 87556 |
| larson10 | new0 | 348.9 351.1 352.9 354.0 354.0 | 87572 |
| larson10 | jemalloc | 382.9 383.9 384.5 389.2 393.2 | 134916 |
| xmalloc-test | old | 358.7 407.0 434.8 442.2 443.7 | 39140 |
| xmalloc-test | new | 369.1 410.3 418.9 437.9 449.4 | 38928 |
| xmalloc-test | new0 | 382.4 412.7 417.5 419.6 421.6 | 39184 |
| xmalloc-test | jemalloc | 469.9 471.9 475.3 479.1 479.7 | 82320 |
| alloc-test1 | old | 2.447 2.453 2.474 2.498 2.572 | 13276 |
| alloc-test1 | new | 2.477 2.505 2.559 2.596 2.601 | 13280 |
| alloc-test1 | new0 | 2.474 2.489 2.524 2.602 2.616 | 13280 |
| alloc-test1 | jemalloc | 2.405 2.405 2.406 2.415 2.419 | 12620 |
| alloc-testN | old | 2.223 2.232 2.233 2.233 2.253 | 16328 |
| alloc-testN | new | 2.232 2.234 2.243 2.244 2.255 | 16468 |
| alloc-testN | new0 | 2.232 2.233 2.234 2.238 2.239 | 16332 |
| alloc-testN | jemalloc | 2.198 2.199 2.199 2.199 2.200 | 16716 |

Reading (medians): larson1 -0.7%, larson10 -1.7%, xmalloc-test -3.7% on
medians with the ranges 359..444 and 369..449 overlapping (this row moved
between 361 and 443 for `old` across four runs on this VM yesterday),
alloc-test1 +3.4% with overlapping ranges (2.447..2.572 vs 2.477..2.601),
alloc-testN +0.4%; RSS unchanged. `new0` is indistinguishable from `new`:
the gate keeps every one of these programs out of compaction and what is
left is the fast-path counter test. On the x86_64 VM the same run showed
xmalloc-test -3.3% against `new0` -0.7%, i.e. the schedule did engage
there; `bench/probes/binsample.c` (an `LD_PRELOAD` sidecar printing one
bin row of `malloc_stats_print` per second) showed why: xmalloc-test's
64-byte class runs at 150..455 slabs with 0..8 slabs' worth of holes and
7..51 partial slabs, holes the producers re-claim within milliseconds.
That is the opposite of the frozen residue (468 of 469 slabs partial, no
refills for minutes), hence the refill-rate gate of the final form.

## Run 3: final form (commit 97e0cf8): descending run, block rebuild, holes and refill-rate gate

Load 0.25 at the start.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 44.4 44.4 44.5 44.5 44.6 | 13700 |
| larson1 | new | 44.0 44.1 44.2 44.3 44.3 | 13828 |
| larson1 | new0 | 44.0 44.3 44.3 44.3 44.3 | 13588 |
| larson1 | jemalloc | 45.0 45.1 45.1 45.1 45.2 | 19056 |
| larson10 | old | 353.7 354.0 356.6 358.3 358.6 | 88208 |
| larson10 | new | 352.5 354.1 356.7 357.0 357.8 | 87828 |
| larson10 | new0 | 352.7 354.6 355.3 356.8 357.0 | 88080 |
| larson10 | jemalloc | 384.7 386.9 389.0 390.5 391.6 | 135096 |
| xmalloc-test | old | 358.6 398.2 424.4 436.1 442.9 | 39140 |
| xmalloc-test | new | 369.0 370.3 379.2 416.0 440.0 | 39272 |
| xmalloc-test | new0 | 368.2 398.6 424.8 444.0 445.4 | 39164 |
| xmalloc-test | jemalloc | 476.3 476.7 477.3 480.2 481.5 | 82224 |
| alloc-test1 | old | 2.523 2.525 2.598 2.610 2.657 | 13276 |
| alloc-test1 | new | 2.549 2.554 2.586 2.595 2.666 | 13280 |
| alloc-test1 | new0 | 2.552 2.553 2.568 2.659 2.700 | 13280 |
| alloc-test1 | jemalloc | 2.408 2.487 2.506 2.524 2.563 | 12620 |
| alloc-testN | old | 2.224 2.225 2.226 2.228 2.228 | 16348 |
| alloc-testN | new | 2.233 2.240 2.243 2.244 2.266 | 16472 |
| alloc-testN | new0 | 2.228 2.230 2.238 2.241 2.243 | 16480 |
| alloc-testN | jemalloc | 2.198 2.200 2.202 2.202 2.207 | 16716 |

Reading (medians): larson1 44.5 -> 44.2 (-0.7%), larson10 356.6 -> 356.7
(0%), alloc-test1 2.598 -> 2.586 s (-0.5%), alloc-testN 2.226 -> 2.243 s
(+0.8%), RSS unchanged. xmalloc-test 424.4 -> 379.2 on medians, but this
row's `old` spans 358..443 (23%) in this run alone and `new0` (same `.so`,
schedule off) sits at 424.8; see the xmalloc-only pass below for the
resolution. `new` and `new0` agree everywhere else, so the schedule is not
engaging on these programs and what remains is the fast-path counter test.

## Churn probe (`bench/probes/churn-run.sh`, commit 97e0cf8), full log in `2026-09-17-aarch64-gce-churn-probe.log`

30000 blocks of one class, a random 56% freed, then a strict
free-one/allocate-one churn for 20 s; `curslabs` is the class's non-empty
slab count. Acceptance: 469 -> under 230 slabs within 5 s at `k = 3`.
(The earlier run of this script on this VM, `.tmp-it13-churn-arm64.log`
in the iteration log, measured a stale `target/release` library from the
review and showed no movement; the script now builds what it measures.)

| library | class | after delete | 3 s of churn | end (22 s) | resident MiB start -> end | M ops/s |
|---|---|---|---|---|---|---:|
| challoc default (`k = 3`) | 1024 | 469 slabs, 44.6% | 216 slabs, 96.9% | 216, 97.0% | 35.0 -> 19.2 | 22.5 |
| challoc default | 640 | 295, 44.5% | 135, 97.3% | 135, 97.0% | 24.1 -> 14.1 | 23.0 |
| challoc `tcache_compact_shift:5` | 1024 | 469 | 216, 96.9% | 216 | 35.0 -> 19.2 | 22.5 |
| challoc `tcache_compact_shift:0` (off) | 1024 | 469 | 469, 44.6% | 469 | 35.0 -> 35.0 | 22.1 |
| challoc `tcache:false` | 1024 | 469 | 208, 99.9% | 208 | 35.0 -> 18.7 | 5.8 |
| jemalloc 5.3.1 | 1024 (this build: 64 regions/slab) | 469, 44.6% | 469, 44.3% | 469 | 33.9 -> 33.9 | 21.3 |
| jemalloc 5.3.1 | 640 | 59, 44.7% | 59, 44.0% | 59 | 22.5 -> 22.5 | 21.2 |

Same picture as x86_64: complete inside the first two seconds, stopped by
the gate at 97% (427 holes, under 1/32 of the capacity), the emptied slabs
purged on the decay window, throughput not below the schedule-off run.
The second-form run of this probe (commit 32bb909, before the heartbeat)
reached the same 216 slabs but stayed at 35.0 MiB resident for the whole
20 s: nothing ticked once the class was dense.

## xmalloc-test alone, 9 rounds (final form 97e0cf8; the row does not engage the schedule, so it stands for 4c7c69f too)

Load 0.00 at the start.

| variant | sorted M frees/s | median |
|---|---|---:|
| old | 348.5 350.6 355.0 370.8 372.0 375.7 378.5 390.0 430.5 | 372.0 |
| new | 356.5 373.8 383.7 392.9 396.7 397.4 404.8 421.4 426.6 | 396.7 |
| new0 | 375.2 381.4 384.5 397.1 397.4 401.3 416.9 417.2 419.2 | 397.4 |
| jemalloc | 474.7 476.1 476.7 476.8 479.6 479.8 480.3 481.1 483.5 | 479.6 |

Reading: the arm64 xmalloc-test row spreads 348..430 for `old` within one
run; `new` (396.7) and `new0` (397.4) are indistinguishable and not below
`old`. With this pass every A/B row on both VMs is within the plan's 3%
of `old` (or better) in the same run.

## Soak 1 (commit 97e0cf8): 20-minute protocol, negative, the gate never opened

`bench/probes/soak-occupancy.sh` (`bench/valkey-soak.sh` with
`DURATION=1200 SETTLE=60` plus `MEMORY MALLOC-STATS` every 20 s), Valkey
9.1.2 built by `contrib/valkey/build.sh` against the working tree, the
unpatched tag with bundled jemalloc 5.3.0 as the reference;
`activedefrag yes`, `jemalloc-bg-thread yes`. Files:
`2026-09-17-aarch64-valkey-soak-1200.md` and
`2026-09-17-aarch64-valkey-soak-1200-{challoc,jemalloc}.csv`.

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 862304 | 900908 | +4.5% |
| write: valkey-benchmark p99 ms | 1.135 | 1.087 | +4.4% |
| write: memtier req/s | 924092 | 962030 | +4.1% |
| write: memtier p99 ms | 1.015 | 0.959 | +5.8% |
| delete: valkey-benchmark req/s | 1394747 | 1463751 | +4.9% |
| delete: valkey-benchmark p99 ms | 0.735 | 0.695 | +5.8% |
| delete: memtier req/s | 1216197 | 1295974 | +6.6% |
| delete: memtier p99 ms | 0.815 | 0.743 | +9.7% |
| peak RSS MiB (VmHWM) | 568.2 | 576.4 | -1.4% |
| RSS MiB at end of delete phase | 157.3 | 131.1 | +20.0% |
| RSS MiB after settle | 157.3 | 131.2 | +19.9% |
| allocator_frag_ratio after settle | 2.24 | 1.81 | +23.8% |
| active_defrag_hits | 269714 | 253105 | -6.2% |
| keys after settle | 105196 | 104036 | - |

Per-class occupancy, challoc (`t` seconds from the server's start; the
delete phase runs from ~620 s, the settle from ~1220 s; columns are
`curslabs/curregs (occupancy)` from the allocator's bins table):

| t | RSS MiB | frag | 640 | 768 | 896 | 1024 | 320 |
|---:|---:|---:|---|---|---|---|---|
| 620 | 567.9 | 1.00 | 5376/548128 (100%) | 570/48193 (99%) | 658/47975 (100%) | 756/48171 (100%) | 117/23693 (99%) |
| 660 | 253.8 | 1.51 | 607/32808 (53%) | 569/32778 (68%) | 656/32639 (68%) | 756/32800 (68%) | 116/16323 (69%) |
| 680 | 164.7 | 2.13 | 402/14616 (36%) | 350/14746 (50%) | 406/14576 (49%) | 482/14922 (48%) | 77/7440 (47%) |
| 740 | 157.4 | 2.24 | 388/13531 (34%) | 345/13604 (46%) | 402/13415 (46%) | 468/13746 (46%) | 70/6811 (48%) |
| 981 | 157.5 | 2.24 | 388/13449 (34%) | 345/13515 (46%) | 402/13460 (46%) | 468/13553 (45%) | 70/6809 (48%) |
| 1201 | 157.3 | 2.24 | 388/13399 (34%) | 345/13461 (46%) | 402/13436 (46%) | 468/13514 (45%) | 70/6860 (48%) |
| 1281 | 157.3 | 2.24 | 388/13386 (34%) | 345/13425 (46%) | 402/13346 (45%) | 468/13613 (45%) | 70/6809 (48%) |

Reading: the same frozen residue as the review's soak (`2026-09-17-gce-
review-soak.md`): from 740 s to the end not one slab of the 640..1024
classes moved, while the churn probe on the same library compacted the
same shape in two seconds. The difference is the gate. Valkey's active
defragmenter (269714 hits) allocates its copies with `MALLOCX_TCACHE_NONE`,
which claims straight from the lowest partial slab and advanced the
`claimed` counter the refill-rate gate was reading by hundreds of slots
per second, i.e. the one-slab threshold in every 4096-pop probe window;
the random-size churn's own refills (64 slots each for the 1 KiB class)
sat at the same threshold. Commit 4c7c69f counts cache refills separately
(`ClassState::refilled`), ignores rebuild and direct claims, and skips a
class only at two slabs' worth of refills per window. Soak 2 below is the
re-run with it.

## Soak 2 (commit 4c7c69f): 20-minute protocol with the refill-counter gate, still +19.6%

Same protocol and VM as soak 1 (the ssh session that drove it dropped;
the container finished and the numbers below were read from the VM's
`target/valkey/soak/{challoc,jemalloc}/samples.csv`, `vm.txt` and
`malloc-stats.txt`; the script's summary `.md` was not written to
`bench/results`).

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| used_memory_rss MiB after settle | 158.9 | 132.9 | +19.6% |
| allocator_active MiB after settle | 155.9 | 115.5 | +35.0% |
| allocator_frag_ratio after settle | 2.27 | 1.83 | +24.0% |
| active_defrag_hits | 274463 | 260431 | +5.4% |
| VmHWM kB | 583204 | 591448 | -1.4% |
| VmRSS kB at the end | 162844 | 136068 | +19.7% |

challoc's final bins table (`size nregs curregs curslabs nonfull purged`):

```
 320  204   6871   72   44    46
 512  128   6950  118   90    74
 640  102  13406  378  323  5002
 768   85  13440  354  242   220
 896   73  13338  413  294   251
1024   64  13575  475  358   287
```

jemalloc's: 640: 13230 regs / 901 slabs; 768: 13255 / 1539; 896:
13155 / 879; 1024: 13426 / 3922.

Reading: the class 1024 row of the review's soak was 411 non-full of 478
slabs; here 117 of 475 are full, so the schedule did engage this time and
packed live data into full low slabs, but the 358 non-full slabs average
about 17 live regions (27%) and did not empty, so the slab count and the
RSS did not move. Candidates for the next iteration, to be measured with
`bench/probes/soak-occupancy.sh` (`MEMORY MALLOC-STATS` every 20 s) and
`bench/probes/binsample.c` so the per-class trajectory and the cache mode
over time are visible: (1) with 7 of 8 pops still LIFO, a slot freed in a
high slab is reused in place by the next allocation unless a flush
intervenes, so the outflow depends on the cache filling; (2) whether the
highest-half flush ever ran under Valkey (the cache must reach `cap`; a
compact-mode rebuild flushes only when it has no room for its block);
(3) whether the probe/compact gate flapped (a rebuild drops back to
probing on the holes check alone, and the probe re-checks refills);
(4) whether Valkey's frees land in other threads' caches (bio or io
threads) so the main thread's cache never fills; (5) whether the
defragmenter's `MALLOCX_TCACHE_NONE` copies (274k hits) interfere with
the migration. The churn probe, a single thread with one class and no
defragmenter, compacts the same shape in two seconds on this library, so
the difference is in what Valkey does around the cache.
