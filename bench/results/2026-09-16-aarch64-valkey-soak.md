# challoc Valkey soak, 2026-09-16, aarch64

- commit: c00d85b
- host: Apple M4, 10 CPUs visible to the container
- kernel: 6.10.14-linuxkit, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=346278e31797ee3f
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=e802589cdf2eaf73
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; INFO sampled every 10 s
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 1504090 | 1512011 | +0.5% |
| write: valkey-benchmark p99 ms | 0.703 | 0.679 | +3.5% |
| write: memtier req/s | 1639031 | 1649140 | +0.6% |
| write: memtier p99 ms | 0.57500 | 0.51100 | +12.5% |
| delete: valkey-benchmark req/s | 2738114 | 2675955 | -2.3% |
| delete: valkey-benchmark p99 ms | 0.343 | 0.367 | -6.5% |
| delete: memtier req/s | 2086527 | 1889214 | -9.5% |
| delete: memtier p99 ms | 0.44700 | 0.51900 | -13.9% |
| peak RSS MiB (VmHWM) | 626.3 | 634.1 | -1.2% |
| peak sampled used_memory_rss MiB | 626.3 | 634.1 | -1.2% |
| RSS MiB at end of write phase | 626.1 | 633.5 | -1.2% |
| RSS MiB at end of delete phase | 150.3 | 106.3 | +41.4% |
| RSS MiB after settle | 150.1 | 106.3 | +41.2% |
| allocator_frag_ratio after settle | 2.10 | 1.28 | +64.1% |
| active_defrag_hits | 373697 | 363833 | -2.6% |
| keys after settle | 104962 | 108204 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `/src/bench/results/2026-09-16-aarch64-valkey-soak-challoc.csv` (128 rows)
- jemalloc: `/src/bench/results/2026-09-16-aarch64-valkey-soak-jemalloc.csv` (128 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,14,14,1503872,0.919
write,memtier,set,14,44,30,1785648,0.48700
write,valkey-benchmark,set,44,57,13,1505911,0.743
write,memtier,set,57,87,30,1758142,0.59900
write,valkey-benchmark,set,87,100,13,1526019,0.719
write,memtier,set,100,130,30,1768339,0.48700
write,valkey-benchmark,set,130,144,14,1531980,0.631
write,memtier,set,144,174,30,1676760,0.60700
write,valkey-benchmark,set,174,187,13,1460387,0.975
write,memtier,set,187,217,30,1556614,0.83100
write,valkey-benchmark,set,217,231,14,1467351,0.799
write,memtier,set,231,261,30,1676980,0.57500
write,valkey-benchmark,set,261,274,13,1512402,0.767
write,memtier,set,274,304,30,1752356,0.48700
write,valkey-benchmark,set,304,317,13,1547389,0.599
write,memtier,set,317,347,30,1658100,0.73500
write,valkey-benchmark,set,347,361,14,1515726,0.687
write,memtier,set,361,391,30,1709238,0.51100
write,valkey-benchmark,set,391,404,13,1518834,0.631
write,memtier,set,404,434,30,1550812,0.57500
write,valkey-benchmark,set,434,447,13,1521954,0.703
write,memtier,set,447,477,30,1634790,0.54300
write,valkey-benchmark,set,477,490,13,1526834,0.687
write,memtier,set,490,520,30,1467631,0.88700
write,valkey-benchmark,set,520,534,14,1416531,1.167
write,memtier,set,534,565,31,1364100,0.99900
write,valkey-benchmark,set,565,578,13,1502065,0.671
write,memtier,set,578,608,30,1586926,0.57500
delete,valkey-benchmark,del,608,617,9,2312941,0.911
delete,memtier,del:set=4:1,617,648,31,1961245,0.59100
delete,valkey-benchmark,del,648,656,8,2406739,0.647
delete,memtier,del:set=4:1,656,686,30,1997111,0.45500
delete,valkey-benchmark,del,686,694,8,2635046,0.599
delete,memtier,del:set=4:1,694,724,30,1996450,0.45500
delete,valkey-benchmark,del,724,731,7,2795639,0.343
delete,memtier,del:set=4:1,731,761,30,1980323,0.45500
delete,valkey-benchmark,del,761,768,7,2836075,0.335
delete,memtier,del:set=4:1,768,799,31,1995704,0.44700
delete,valkey-benchmark,del,799,806,7,2826456,0.335
delete,memtier,del:set=4:1,806,836,30,2073028,0.40700
delete,valkey-benchmark,del,836,843,7,2802690,0.343
delete,memtier,del:set=4:1,843,873,30,2038192,0.46300
delete,valkey-benchmark,del,873,880,7,2826855,0.335
delete,memtier,del:set=4:1,880,910,30,2137484,0.40700
delete,valkey-benchmark,del,910,917,7,2814919,0.343
delete,memtier,del:set=4:1,917,947,30,2108400,0.40700
delete,valkey-benchmark,del,947,955,8,2773925,0.375
delete,memtier,del:set=4:1,955,985,30,2113736,0.44700
delete,valkey-benchmark,del,985,992,7,2720348,0.383
delete,memtier,del:set=4:1,992,1022,30,2080920,0.47100
delete,valkey-benchmark,del,1022,1029,7,2832460,0.335
delete,memtier,del:set=4:1,1029,1059,30,2227249,0.39100
delete,valkey-benchmark,del,1059,1066,7,2825657,0.335
delete,memtier,del:set=4:1,1066,1096,30,2211557,0.38300
delete,valkey-benchmark,del,1096,1104,8,2765869,0.367
delete,memtier,del:set=4:1,1104,1134,30,2134009,0.46300
delete,valkey-benchmark,del,1134,1141,7,2817298,0.335
delete,memtier,del:set=4:1,1141,1171,30,2207905,0.42300
delete,valkey-benchmark,del,1171,1178,7,2816902,0.335
delete,memtier,del:set=4:1,1178,1208,30,2121114,0.51100
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,13,13,1500938,0.743
write,memtier,set,13,43,30,1783860,0.49500
write,valkey-benchmark,set,43,57,14,1530808,0.655
write,memtier,set,57,87,30,1727520,0.51100
write,valkey-benchmark,set,87,100,13,1532332,0.631
write,memtier,set,100,130,30,1748776,0.50300
write,valkey-benchmark,set,130,143,13,1557996,0.575
write,memtier,set,143,173,30,1759453,0.48700
write,valkey-benchmark,set,173,186,13,1558118,0.583
write,memtier,set,186,216,30,1743132,0.48700
write,valkey-benchmark,set,216,229,13,1528585,0.679
write,memtier,set,229,259,30,1585477,0.72700
write,valkey-benchmark,set,259,273,14,1409543,0.967
write,memtier,set,273,303,30,1522039,0.75900
write,valkey-benchmark,set,303,316,13,1519295,0.711
write,memtier,set,316,346,30,1668112,0.50300
write,valkey-benchmark,set,346,360,14,1444774,1.063
write,memtier,set,360,390,30,1574376,0.59100
write,valkey-benchmark,set,390,404,14,1507386,0.687
write,memtier,set,404,434,30,1623082,0.55900
write,valkey-benchmark,set,434,447,13,1547389,0.623
write,memtier,set,447,477,30,1628835,0.57500
write,valkey-benchmark,set,477,490,13,1560428,0.575
write,memtier,set,490,520,30,1642442,0.51100
write,valkey-benchmark,set,520,533,13,1529637,0.679
write,memtier,set,533,563,30,1527538,0.67100
write,valkey-benchmark,set,563,577,14,1440922,1.039
write,memtier,set,577,607,30,1553311,0.61500
delete,valkey-benchmark,del,607,616,9,2285453,0.863
delete,memtier,del:set=4:1,616,646,30,1941292,0.55900
delete,valkey-benchmark,del,646,653,7,2918004,0.335
delete,memtier,del:set=4:1,653,683,30,2043328,0.43100
delete,valkey-benchmark,del,683,690,7,2870676,0.367
delete,memtier,del:set=4:1,690,720,30,1855721,0.73500
delete,valkey-benchmark,del,720,727,7,2811753,0.351
delete,memtier,del:set=4:1,727,757,30,1928195,0.49500
delete,valkey-benchmark,del,757,764,7,2822068,0.367
delete,memtier,del:set=4:1,764,794,30,1945908,0.48700
delete,valkey-benchmark,del,794,801,7,2873150,0.375
delete,memtier,del:set=4:1,801,831,30,1963581,0.47900
delete,valkey-benchmark,del,831,838,7,2904444,0.335
delete,memtier,del:set=4:1,838,868,30,1725969,0.78300
delete,valkey-benchmark,del,868,877,9,2185076,0.607
delete,memtier,del:set=4:1,877,907,30,1717315,0.81500
delete,valkey-benchmark,del,907,915,8,2499063,0.471
delete,memtier,del:set=4:1,915,946,31,1845094,0.58300
delete,valkey-benchmark,del,946,953,7,2625361,0.431
delete,memtier,del:set=4:1,953,983,30,1879034,0.55900
delete,valkey-benchmark,del,983,990,7,2846165,0.343
delete,memtier,del:set=4:1,990,1020,30,1953791,0.47100
delete,valkey-benchmark,del,1020,1028,8,2725167,0.391
delete,memtier,del:set=4:1,1028,1058,30,1959330,0.45500
delete,valkey-benchmark,del,1058,1065,7,2885586,0.335
delete,memtier,del:set=4:1,1065,1095,30,1824349,0.55900
delete,valkey-benchmark,del,1095,1103,8,2388630,0.615
delete,memtier,del:set=4:1,1103,1133,30,1872150,0.51900
delete,valkey-benchmark,del,1133,1140,7,2837684,0.343
delete,memtier,del:set=4:1,1140,1171,31,1873681,0.51900
delete,valkey-benchmark,del,1171,1179,8,2336995,0.495
delete,memtier,del:set=4:1,1179,1209,30,1898690,0.51100
```


## Acceptance reading (Phase 1 acceptance step 1)

Against docs/plan.md Phase 1 "Soak" (throughput and p99 at least equal, peak
RSS at most +5%, post-delete RSS within 10% of jemalloc after the decay window):

| criterion | result | verdict |
|---|---|---|
| throughput | write -0.5% / -0.6%, delete +2.3% / +9.5% (challoc ahead) | pass (write within run-to-run noise) |
| p99 | write +3.5% (valkey-benchmark) / +12.5% (memtier) behind, delete -6.5% / -13.9% ahead | inconclusive on this host: the task 8 smoke run put memtier p99 at +-7% and valkey-benchmark p99 at 0.6-1.6 ms run to run on the shared 10-CPU Docker VM; decide on the GCE VMs (Phase 1.5) |
| peak RSS | VmHWM 641380 vs 649336 kB, sampled 626.3 vs 634.1 MiB, -1.2% | pass |
| RSS after settle | 150.1 vs 106.3 MiB, +41% (allocator_frag_ratio 2.10 vs 1.28) | FAIL |

Where the 44 MiB sit (challoc `MEMORY MALLOC-STATS` after settle, `target/valkey/soak/challoc/malloc-stats.txt`):
`allocated 69937920, active 155123712, resident 158887936`; small bins
`allocated 64925440, active 141819904`, so 76.9 MB of the 85 MB gap is
partially filled small slabs. The four 640..1024-byte classes hold 39 MB of it:

```
   size  nregs  curregs  curslabs  nonfull    live/slab   active MiB  allocated MiB
    640    102    13553      261      201        52         16.3         8.3
    768     85    13565      328      253        41         20.5         9.9
    896     73    13347      366      269        36         22.9        11.4
   1024     64    13441      435      387        31         27.2        13.1
```

and the 320..512 classes another 17 MB (425 slabs, 26.6 MiB active, 11.2 MiB
allocated). jemalloc's server ends at `allocated 70457056, active 89907200`.

Timeline (10 s samples, `*-valkey-soak-{challoc,jemalloc}.csv`): the first
`valkey-benchmark del` run of the delete phase removes 89% of the data in
about 40 s (used_memory 608 -> 68 MiB). Active defrag runs during that window
and stops in both servers: challoc's `active_defrag_hits` reach 373697 by
631 s (used 106 MiB, active 185 MiB) and never move again; jemalloc's reach
363833 by 641 s. From 631 s to the end challoc's `allocator_active` falls only
185 -> 148 MiB while the data falls 106 -> 65 MiB; jemalloc's falls 182 -> 86
MiB over the same deletes. Nothing changes during the 60 s settle for either
allocator: there are no dirty pages left to decay (`pdirty: 0`), the residue
is live slabs, so a longer decay window would not help.

Why it stops there: Valkey's defragmenter (`defrag.c` `activeDefragCycle`)
stops when `frag_bytes < active-defrag-ignore-bytes` (default 100 MB) or
`frag_pct < active-defrag-threshold-lower` (10%), where `frag_bytes` is
`allocatorDefragGetFragSmallbins()`, the sum over *bins* of
`(curslabs * nregs - curregs) * size`. challoc's bins carry 76.9 MB of that
waste, under the 100 MB floor, so defrag does not restart although the ratio
is 2.10. jemalloc sits under the same floor but its residue is 19 MB, because
its slabs for these classes are 4..20 KiB (4..32 regions: 1024 B -> 4 regs,
640 B -> 32 regs) and a random deletion that leaves 11% of the objects live
empties a 4-region slab with probability 0.89^4 = 63% and a 32-region slab
with probability 2.4%; those slabs are freed outright and decay. challoc's
64 KiB slabs hold 64..102 regions of these sizes (empty with probability
0.89^64 = 0.06%), so every slab survives with 30..50 live regions and only
the defragmenter can drain it, and the defragmenter has stopped. Placement
policy is not the difference: both allocators fill the lowest-address
non-full slab first, on refills and on the `MALLOCX_TCACHE_NONE` path Valkey
moves objects through. The per-thread caches pin at most 1024 slots per
class (about 1 MiB per class at these sizes), a small part of the residue.

This is the same mechanism as the four `Active Defrag` "didn't stop" failures
in `unit/memefficiency` (Phase 1 task 7 notes): Valkey's stop thresholds
(2.5 MB / 5% in the tests, 100 MB / 10% in production) are absolute figures
calibrated to jemalloc's few-page slabs, and challoc's 64 KiB minimum slab
(design section 3) leaves a residue an order of magnitude larger per class.
The allocator-side options are a smaller minimum slab for the small classes
(`layout::LG_MIN_SLAB`, with `BIN_MAX`, refill batching and the Phase 0
throughput criterion all depending on it), or accepting a different
post-delete floor; remediation is the next acceptance step.
