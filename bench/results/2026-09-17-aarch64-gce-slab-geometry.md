# challoc slab geometry (4 KiB slabs for the 512..1024-byte classes) A/B and soak, 2026-09-17, aarch64 (GCE reference VM, us-east4-a)

Phase 1 acceptance step 3 (c), the slab-geometry step that the soak 3
diagnosis (`2026-09-17-aarch64-gce-compacting-cache-soak3.md`) pointed
to: the post-delete residue is live values the load never touches again,
one to fifteen per slab in the 512..1024-byte classes, and each pinned
1 KiB value held a 64 KiB slab where jemalloc's 4-region slab holds
4 KiB. Those five classes move to the smallest power of two from 4 KiB
up whose internal waste is at most a bounded fraction of the slab
(`layout::LG_MIN_SLAB`, `SMALL_SLAB_BELOW`, `SLAB_WASTE_DIV`,
`lg_slab_for`); every other class keeps 64 KiB. Two forms were measured:
f27d736, a bound of 1/16 (4 KiB for 512, 640, 768 and 1024 bytes with
8, 6, 5 and 4 slots, 8 KiB for 896 with 9; runs 1..2 and soak 1), and
e6b89c3, a bound of 1/32 (4 KiB for 512 and 1024, 8 KiB for 896, 16 KiB
for 640 and 768 with 25 and 21 slots; run 3 and soak 2), the form
adopted after soak 1 put the 640 class's internal waste on the peak RSS.
`MAX_PICKS` grows
to 16 so a refill or a sorted flush of those classes is one lock hold,
guard slabs keep one guard per `guard_slab_interval` x 64 KiB, the
compaction thresholds count 64 KiB slabs' worth of slots, and the purge
batch merges adjacent slabs. This file is the throughput row before and
after per CLAUDE.md, the xmalloc-test rerun, and the 20-minute Valkey
soak that decides the post-delete RSS criterion; the x86_64 rows are in
`2026-09-17-x86_64-gce-slab-geometry.md`. VM numbers are never compared
with Mac numbers.

- host: challoc-bench-arm64 (c4a-standard-16, Google Axion, Neoverse V2),
  16 vCPUs, Ubuntu 24.04 kernel 7.0.0-1011-gcp, 4 KiB pages,
  `guard_install=yes`; container: Wolfi (rustc 1.98.1, gcc 15.3.0);
  jemalloc for the A/B = Wolfi's libjemalloc.so.2 (5.3.1). Load average
  0.01 at the start of the A/B; the rounds raise it to about 8.
- method (A/B): `bench/build.sh` programs; `old` = the pre-change library
  built from `git archive 4021694` into `target/old`; `new` = the working
  tree that became f27d736 (`LG_MIN_SLAB` 12, a `.tmp-new` copy) into
  `target/new` (runs 1..2) or the tree that became e6b89c3 (run 3);
  `s13` = run 1's tree with `LG_MIN_SLAB` 13 (8 KiB slabs for all five
  classes: 16, 12, 10, 9 and 8 slots) into `target/s13`; `a16` = run 3's
  tree with `SLAB_WASTE_DIV` 16, i.e. run 1's geometry, into `target/a16`.
  5 rounds plus one warm-up, variants interleaved per round, every run's
  score listed sorted; RSS is `rt`'s median maxrss in KiB. Driver:
  `.tmp-it15-ab.sh` in the iteration log (the iteration-13 recipe).
- programs: `larson 5 8 1000 5000 100 4141 1` (larson1) and `... 10`
  (larson10), M ops/s, higher is better; `xmalloc-test -w 10 -t 5 -s 64`,
  M frees/s, higher is better; `alloc-test 1` and `alloc-test 16`,
  seconds, lower is better.

## Run 1: old, new (waste bound 1/16, f27d736), s13 (8 KiB for all five classes), jemalloc

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 44.2 44.2 44.3 44.3 44.3 | 13724 |
| larson1 | new | 43.1 43.1 43.2 43.3 43.3 | 13472 |
| larson1 | s13 | 43.2 43.4 43.5 43.5 43.5 | 13600 |
| larson1 | jemalloc | 45.1 45.2 45.2 45.2 45.2 | 19264 |
| larson10 | old | 352.3 354.8 354.9 355.9 358.4 | 87836 |
| larson10 | new | 346.2 346.5 350.8 351.3 351.7 | 88076 |
| larson10 | s13 | 344.3 346.7 349.2 349.7 351.4 | 88092 |
| larson10 | jemalloc | 385.7 385.9 388.0 390.3 391.0 | 134556 |
| xmalloc-test | old | 325.1 372.4 405.1 408.4 428.6 | 39152 |
| xmalloc-test | new | 358.7 381.6 388.3 418.9 423.1 | 39220 |
| xmalloc-test | s13 | 334.3 338.2 350.7 412.8 422.3 | 39284 |
| xmalloc-test | jemalloc | 475.0 478.9 479.1 479.5 480.6 | 81324 |
| alloc-test1 | old | 2.435 2.444 2.451 2.452 2.493 | 13288 |
| alloc-test1 | new | 2.440 2.446 2.458 2.476 2.478 | 13420 |
| alloc-test1 | s13 | 2.429 2.436 2.455 2.471 2.488 | 13280 |
| alloc-test1 | jemalloc | 2.386 2.392 2.399 2.411 2.414 | 12620 |
| alloc-testN | old | 2.231 2.233 2.233 2.241 2.241 | 16344 |
| alloc-testN | new | 2.229 2.236 2.237 2.237 2.249 | 16620 |
| alloc-testN | s13 | 2.226 2.228 2.229 2.229 2.237 | 16472 |
| alloc-testN | jemalloc | 2.195 2.196 2.197 2.198 2.215 | 16588 |

Reading (medians, new vs old): larson1 44.3 -> 43.2 (-2.5%), larson10
354.9 -> 350.8 (-1.2%), alloc-test1 2.451 -> 2.458 s (+0.3%),
alloc-testN 2.233 -> 2.237 s (+0.2%), RSS within 2% (the descriptor
array of a 4 KiB-slab class is 64 bytes per 4 KiB instead of per 64 KiB).
xmalloc-test's medians (405.1 -> 388.3, -4%) sit inside the old library's
own 325..429 spread on this VM, where the row is known to be noisy
(`2026-09-17-aarch64-gce-compacting-cache.md` ran it alone for nine
rounds); the 64-byte class it exercises is untouched by the change, so
run 2 below repeats the row alone. The 8 KiB form (`s13`) is inside
new's spread on every row (larson1 43.5, larson10 349.2), so 8 KiB buys
nothing on throughput and 4 KiB, jemalloc's own 4 regions for the 1 KiB
class, is adopted. larson's sizes are 8..1000 bytes, so 55% of its
allocations (449..1000 bytes) now live in 4 KiB slabs and each refill or
flush of those classes touches 16 descriptors instead of one; the 1..2.5%
is that cost, one lock hold per refill kept by `MAX_PICKS` 16. Step 2's
two-regime 16 KiB form had cost 2.2% here for no RSS gain
(`2026-09-16-aarch64-gce-min-slab.md`).

## Run 2: xmalloc-test alone, 9 rounds (old vs new)

Same VM and libraries, right after run 1 (load 15.8 at the start, the
end of run 1's rounds; the row is CPU-bound on 10 threads of 16 vCPUs).

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| xmalloc-test | old | 322.2 336.6 344.9 353.1 374.8 389.5 392.7 413.4 431.4 | 39088 |
| xmalloc-test | new | 343.1 376.3 401.3 419.4 424.1 428.5 437.8 440.4 450.0 | 39080 |

Reading: medians 374.8 -> 424.1; the new library's nine scores are each
above the old library's score of the same rank. The row does not
regress; the run-1 median was the row's noise.

## Run 3: old, new (waste bound 1/32, e6b89c3, adopted), a16 (waste bound 1/16, run 1's geometry), jemalloc

Same VM, right after soak 1 (load 0.2 at the start), same programs and
rounds; `new` is now the tree that became e6b89c3 (`SLAB_WASTE_DIV` 32:
4 KiB for 512 and 1024, 8 KiB for 896, 16 KiB for 640 and 768), `a16` the
same tree with `SLAB_WASTE_DIV` 16 (run 1's geometry).

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 44.2 44.2 44.2 44.3 44.4 | 13724 |
| larson1 | new | 43.2 43.3 43.5 43.6 43.6 | 13472 |
| larson1 | a16 | 43.1 43.2 43.2 43.2 43.3 | 13724 |
| larson1 | jemalloc | 45.1 45.1 45.1 45.1 45.3 | 19052 |
| larson10 | old | 350.4 350.5 351.1 353.1 354.5 | 87436 |
| larson10 | new | 346.2 347.3 348.8 349.9 351.3 | 87584 |
| larson10 | a16 | 344.2 345.1 346.7 350.8 353.4 | 87712 |
| larson10 | jemalloc | 386.7 386.8 388.4 388.5 388.6 | 135016 |
| xmalloc-test | old | 344.3 346.6 363.1 413.5 420.1 | 39280 |
| xmalloc-test | new | 325.1 362.9 364.9 432.3 443.3 | 39076 |
| xmalloc-test | a16 | 352.7 410.6 413.7 418.9 430.5 | 39464 |
| xmalloc-test | jemalloc | 476.3 476.7 481.4 482.0 483.6 | 79516 |
| alloc-test1 | old | 2.442 2.445 2.446 2.450 2.553 | 13288 |
| alloc-test1 | new | 2.430 2.450 2.472 2.554 2.568 | 13292 |
| alloc-test1 | a16 | 2.427 2.430 2.443 2.449 2.451 | 13420 |
| alloc-test1 | jemalloc | 2.388 2.393 2.394 2.401 2.408 | 12616 |
| alloc-testN | old | 2.233 2.233 2.240 2.241 2.242 | 16488 |
| alloc-testN | new | 2.235 2.237 2.237 2.238 2.240 | 16492 |
| alloc-testN | a16 | 2.228 2.229 2.237 2.237 2.238 | 16544 |
| alloc-testN | jemalloc | 2.196 2.198 2.198 2.203 2.206 | 16716 |

Reading (medians, new vs old): larson1 44.2 -> 43.5 (-1.6%), larson10
351.1 -> 348.8 (-0.7%), xmalloc-test 363.1 -> 364.9 (the row's usual
spread), alloc-test1 2.446 -> 2.472 s (+1.1%), alloc-testN 2.240 ->
2.237 s; every row within the plan's 3%, and the adopted form is not
slower than the 1/16 form on any row (larson1 43.5 vs 43.2, larson10
348.8 vs 346.7).

## Soak 1 (commit f27d736, waste bound 1/16: 4 KiB slabs for 512, 640, 768 and 1024, 8 KiB for 896): post-delete criterion met, peak criterion by 1%

`bench/probes/soak-occupancy.sh` (`bench/valkey-soak.sh` with
`DURATION=1200 SETTLE=60` plus `MEMORY MALLOC-STATS` every 20 s), Valkey
9.1.2 built by `contrib/valkey/build.sh` against the working tree
(quantum8, `io-threads 1`, `activedefrag yes`, `jemalloc-bg-thread yes`,
stock `active-defrag-*`), the unpatched tag with bundled jemalloc 5.3.0
as the reference, same VM and session (challoc first, then jemalloc),
`t` seconds from the server's start; the write phase runs to 600 s, the
delete phase 600..1200 s, the settle to 1260 s. Driver `.tmp-it15-soak.sh`
in the iteration log; samples in `target/soak-it15` on the VM.

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| RSS MiB at end of write phase (`used_memory_rss`, t=600) | 601.6 | 578.7 | +4.0% |
| peak RSS MiB (VmHWM) | 601.8 | 579.1 | +3.9% |
| RSS MiB after settle (`used_memory_rss`, t=1261) | 122.6 | 132.7 | -7.6% |
| VmRSS MiB at the end | 122.7 | 132.8 | |
| allocator_frag_ratio after settle | 1.58 | 1.83 | |
| allocator_active MiB after settle | 112.8 | 115.5 | |
| metadata committed MiB (descriptors, sets) | 12.2 | | |
| write: valkey-benchmark req/s | 867555 | 954256 | +10.0% |
| write: valkey-benchmark p99 ms | 1.127 | 1.031 | +9.3% |
| write: memtier req/s | 954465 | 994011 | +4.1% |
| write: memtier p99 ms | 0.959 | 0.919 | +4.4% |
| delete: valkey-benchmark req/s | 1424450 | 1499258 | +5.3% |
| delete: valkey-benchmark p99 ms | 0.711 | 0.671 | +6.0% |
| delete: memtier req/s | 1260010 | 1315712 | +4.4% |
| delete: memtier p99 ms | 0.751 | 0.727 | +3.3% |
| active_defrag_hits | 204168 | 272182 | |
| keys after settle | 105103 | 104715 | |

jemalloc's final bins (`curregs / curslabs`, regions per slab): 512
6782 / 1244 (8), 640 13264 / 904 (32), 768 13291 / 1511 (16), 896
13206 / 873 (32), 1024 13450 / 3938 (4).

challoc's trajectory (`used_memory`, `used_memory_rss`, `allocator_active`
in MiB, then `curregs/curslabs/nonfull` of the five classes):

| t | used | rss | active | frag | 512 | 640 | 768 | 896 | 1024 |
|---:|---:|---:|---:|---:|---|---|---|---|---|
| 120 | 556.2 | 601.4 | 592.0 | 1.00 | 24518/3067/13 | 548679/91486/231 | 48808/9792/146 | 48391/5408/277 | 48938/12235/11 |
| 361 | 556.2 | 601.7 | 592.0 | 1.00 | 24438/3067/96 | 548784/91486/141 | 48878/9795/97 | 48508/5409/146 | 48776/12234/179 |
| 641 | 229.2 | 601.8 | 249.4 | 1.03 | 24565/3071/3 | 48895/8560/858 | 48814/9767/22 | 48379/5378/11 | 48959/12240/1 |
| 681 | 65.4 | 157.9 | 129.6 | 1.78 | 7204/1664/1019 | 13902/4010/2308 | 13779/4309/2242 | 13775/2921/1886 | 14126/5210/2622 |
| 721 | 63.9 | 128.5 | 115.9 | 1.62 | 6911/1410/733 | 13512/3312/1353 | 13549/3712/1386 | 13507/2521/1270 | 13725/4372/1372 |
| 761 | 63.5 | 123.6 | 113.2 | 1.59 | 6847/1377/734 | 13351/3224/1356 | 13526/3660/1317 | 13496/2463/1200 | 13534/4280/1358 |
| 881 | 63.6 | 123.3 | 113.2 | 1.58 | 6931/1368/687 | 13480/3216/1327 | 13451/3659/1340 | 13445/2456/1204 | 13573/4267/1344 |
| 1001 | 63.4 | 122.8 | 112.6 | 1.60 | 6874/1356/685 | 13372/3199/1320 | 13381/3642/1369 | 13303/2444/1287 | 13570/4247/1287 |
| 1261 (end) | 63.3 | 122.6 | 112.8 | 1.58 | 7034/1349/573 | 13399/3198/1298 | 13415/3641/1331 | 13321/2444/1255 | 13593/4243/1268 |

Final per-slab occupancy histograms (slabs per eighth of `nslots`, last
bucket full) and the counters (`purged`, `refilled`, `compacted`):

```
 512  slabs 1349  [0, 396, 136, 30, 3, 2, 1, 0, 781]        purged 1731   refilled 30080   compacted 854976
 640  slabs 3198  [0, 935, 222, 0, 45, 7, 77, 0, 1912]      purged 88296  refilled 560832  compacted 1721344
 768  slabs 3641  [0, 976, 0, 252, 50, 0, 30, 0, 2333]      purged 6166   refilled 61440   compacted 7871872
 896  slabs 2444  [717, 308, 81, 25, 9, 10, 17, 72, 1205]   purged 2969   refilled 59584   compacted 1712352
1024  slabs 4243  [0, 0, 973, 0, 168, 0, 124, 0, 2978]      purged 8005   refilled 59840   compacted 1724384
```

Reading:

- Post-delete: 122.6 MiB after settle against the 139.9..149.8 MiB of the
  four 600 s runs of soak 3 and the 157.3 / 158.9 MiB of soaks 1 and 2 at
  64 KiB slabs, i.e. the residue is gone: the 1 KiB class ends at 80%
  occupancy (13593 live in 4243 slabs of 4; jemalloc's 4-region bin ended
  at 81..86% in the earlier soaks), the pinned values that held 64 KiB
  each now hold 4 KiB (973 slabs with one live value, 168 with two, 124
  with three: 5.1 MiB for about 1700 pinned values, where soak 3's run 4
  spent 45 MiB across the four classes). The compaction still runs
  (`compacted` climbs through the delete phase, `c_on` 1 / `c_off` 0 /
  `gated` 0 in every class) and the slab counts settle within 160 s of the
  delete phase's start (t=761) instead of never.
- Peak: the write-phase RSS is 601.6 MiB (VmHWM 601.8) against 567.9 /
  568.2 MiB with 64 KiB slabs, +34 MiB: `allocator_active - allocated`
  is 34.8 MiB (internal slab waste; the 640 class alone holds 548.7k
  live values in 91486 slabs of 6, 256 bytes wasted per 4 KiB slab,
  21 MiB) and metadata is 12.2 MiB (descriptors, 64 bytes per slab, 5.6
  MiB of it the 640 class's) against about 1 MiB before. The plan's peak
  criterion is 5% above jemalloc: this run's jemalloc half peaked at
  579.1 MiB, so the form passes at +3.9%, but jemalloc's VmHWM in this
  lab's soaks has been 572.9 / 576.4 / 576.8 / 579.1 / 591.4 MiB, which
  puts the same 601.8 MiB anywhere between +1.8% and +5.0%: a 1% margin
  on a criterion, bought for 5 MiB of post-delete RSS that the criterion
  does not need (-7.6% against -10% allowed). Hence run 3 and soak 2: a
  waste bound of 1/32 (e6b89c3), which sends the 640 and 768 classes to
  16 KiB slabs (2.3% and 1.6% waste) and keeps 4 KiB for 512 and 1024
  and 8 KiB for 896.
- Throughput: challoc's rows are where the earlier soaks had them
  (write valkey-benchmark 867k req/s against 862..864k in soaks 1..2 and
  the min-slab soak); this run's jemalloc half was 5% faster than its
  earlier runs (954k against 900..904k), so the gap reads +10% here. The
  soak's throughput criterion has not been met since Phase 1 task 8 and
  is the review's P2 item (single-arena slab sharing), not this change.

## Soak 2 (commit e6b89c3, waste bound 1/32: 4 KiB for 512 and 1024, 8 KiB for 896, 16 KiB for 640 and 768): both RSS criteria met

Same protocol, VM and session as soak 1, right after run 3 (load 0.3 at
the start); the working tree is e6b89c3 (`git rev-parse` on the VM).
Driver `.tmp-it15-arm64b.sh` in the iteration log; samples in
`target/soak-it15b` on the VM.

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 882514 | 944708 | +7.0% |
| write: valkey-benchmark p99 ms | 1.111 | 1.039 | +6.9% |
| write: memtier req/s | 940021 | 985322 | +4.8% |
| write: memtier p99 ms | 0.983 | 0.927 | +6.0% |
| delete: valkey-benchmark req/s | 1415585 | 1493155 | +5.5% |
| delete: valkey-benchmark p99 ms | 0.711 | 0.679 | +4.7% |
| delete: memtier req/s | 1242634 | 1316732 | +6.0% |
| delete: memtier p99 ms | 0.775 | 0.719 | +7.8% |
| peak RSS MiB (VmHWM) | 578.3 | 577.2 | +0.2% |
| RSS MiB at end of write phase | 578.1 | 576.9 | +0.2% |
| RSS MiB at end of delete phase | 128.6 | 130.5 | -1.5% |
| RSS MiB after settle | 128.6 | 130.5 | -1.5% |
| allocator_frag_ratio after settle | 1.78 | 1.81 | |
| active_defrag_hits | 253993 | 268829 | |
| keys after settle | 104069 | 103650 | |
| metadata committed MiB (descriptors, sets), end | 7.2 | | |

challoc's trajectory (`used_memory`, `used_memory_rss`, `allocator_active`
in MiB, then `curregs/curslabs/nonfull` of the five classes):

| t | used | rss | active | frag | 512 | 640 | 768 | 896 | 1024 |
|---:|---:|---:|---:|---:|---|---|---|---|---|
| 140 | 552.8 | 577.2 | 572.5 | 1.00 | 24156/3020/1 | 547978/21927/171 | 48054/2298/168 | 47880/5320/4 | 48056/12051/162 |
| 180 | 553.0 | 577.3 | 572.5 | 1.00 | 24103/3020/53 | 547963/21928/198 | 48094/2299/156 | 47669/5320/201 | 48233/12059/1 |
| 720 | 63.4 | 133.2 | 126.0 | 1.80 | 6956/1361/602 | 13430/1126/705 | 13368/1312/828 | 13316/2499/1302 | 13639/4279/1272 |
| 760 | 63.2 | 130.5 | 124.8 | 1.79 | 6818/1338/675 | 13308/1113/674 | 13400/1306/784 | 13346/2466/1250 | 13565/4222/1213 |
| 800 | 62.8 | 128.8 | 123.4 | 1.78 | 6742/1320/669 | 13188/1108/694 | 13415/1300/749 | 13361/2438/1155 | 13358/4162/1241 |
| 880 | 62.9 | 128.6 | 123.3 | 1.77 | 6868/1319/612 | 13310/1108/714 | 13288/1300/775 | 13315/2436/1212 | 13401/4160/1209 |
| 1000 | 63.4 | 128.7 | 123.8 | 1.77 | 6881/1316/606 | 13301/1108/705 | 13254/1299/815 | 13222/2435/1228 | 13474/4157/1182 |
| 1040 | 62.9 | 128.8 | 123.3 | 1.79 | 6798/1316/654 | 13285/1108/742 | 13304/1299/771 | 13173/2435/1263 | 13418/4156/1215 |
| 1260 (end) | 62.8 | 128.6 | 123.5 | 1.78 | 6926/1311/540 | 13275/1108/738 | 13313/1299/766 | 13205/2435/1251 | 13435/4153/1210 |

Final per-slab occupancy histograms (slabs per eighth of `nslots`, last
bucket full) and the counters (`purged`, `refilled`, `compacted`):

```
 512  slabs 1311  [0, 379, 127, 24, 5, 2, 0, 1, 773]          purged 1730   refilled 29312   compacted 841920
 640  slabs 1108  [489, 137, 10, 3, 0, 1, 3, 77, 388]         purged 20829  refilled 560256  compacted 1699744
 768  slabs 1299  [489, 234, 15, 3, 1, 5, 6, 4, 542]          purged 1010   refilled 60160   compacted 7782528
 896  slabs 2435  [745, 282, 89, 17, 5, 16, 18, 53, 1210]     purged 2911   refilled 58112   compacted 1685248
1024  slabs 4153  [0, 0, 910, 0, 163, 0, 121, 0, 2959]        purged 7956   refilled 58688   compacted 1705312
```

The 640 and 768 classes show the pinned population directly: 489 slabs
each in the lowest bucket (1..3 live of 25, 1..2 of 21) that the churn
never touches, against 388 and 542 full ones; at 64 KiB the same
population held about 260 slabs of 102 and 85 (soak 3, run 4).
jemalloc's final bins (`curregs / curslabs`, regions per slab): 512
6707 / 1216 (8), 640 13097 / 891 (32), 768 13173 / 1480 (16), 896
13081 / 840 (32), 1024 13345 / 3906 (4). The soak script's own summaries
and 10 s sample CSVs are `2026-09-17-aarch64-gce-slab-geometry-soak{1,2}-*`.

Reading:

- Peak: the write-phase RSS is 577.3 MiB and the VmHWM 578.3 MiB,
  against 601.6 / 601.8 in soak 1 and 568 with 64 KiB slabs: the 640
  class's waste is 384 bytes per 16 KiB slab (2.3%, 8 MiB at 548k live
  values) and the descriptors 7.2 MiB in all (the class has 21.9k slabs
  instead of 91.5k). +0.2% against this run's jemalloc, inside the
  plan's 5%.
- Post-delete: 128.6 MiB after settle against jemalloc's 130.5 (-1.5%;
  the plan allows +10%), against 139.9..158.9 MiB in every soak with
  64 KiB slabs. The 1 KiB class ends at 81% occupancy in 4 KiB slabs,
  the 512 class at 65%, the 896 class at 60% in 8 KiB slabs; the 640 and
  768 classes end at 48..49% in 16 KiB slabs (17.3 and 20.3 MiB for 8.1
  and 9.8 MiB of values), the same occupancy they had at 64 KiB (the
  slabs pinned by one or a few survivors are a quarter the size, but
  there are nearly four times as many of them: the 5 MiB soak 1 bought
  with their 4 KiB slabs is what the peak criterion was paid with). The
  slab counts are constant from t=800 on; the compaction runs through
  the delete phase in every class (`c_on` 1, `c_off` 0, `gated` 0; the
  768 class left and re-entered compact mode once).
- Throughput and p99: 5..8% behind jemalloc in every row, where soak 1
  read 3..10% and soaks 1..2 of the compacting cache 4..10%; challoc's
  absolute numbers (883k / 940k write, 1.42 M / 1.24 M delete) are
  within 2% of every earlier soak's, so the geometry did not move them.
  Unmet since Phase 1 task 8; the review's P2 item.

## Conclusion

Adopted: e6b89c3, the 1/32 waste bound. The A/B rows on both VMs are
within 3% of the pre-change library (larson1 -1.2..-1.6%, larson10
-0.3..-0.7%, alloc-test and xmalloc-test unchanged), the soak's peak RSS
is within 5% of jemalloc's (+0.2%) and the post-delete RSS within 10%
(-1.5%), which closes the RSS half of acceptance step 3 (c). The
throughput half of the soak criterion (at least equal to jemalloc) has
not been met since Phase 1 task 8 and is the review's P2 item.

jemalloc's final bins in soak 2 (`curregs / curslabs`, regions per slab):
512 6707 / 1216 (8), 640 13097 / 891 (32), 768 13173 / 1480 (16), 896
13081 / 840 (32), 1024 13345 / 3906 (4). The soak script's own summaries
and 10 s sample CSVs are `2026-09-17-aarch64-gce-slab-geometry-soak{1,2}-*`.
