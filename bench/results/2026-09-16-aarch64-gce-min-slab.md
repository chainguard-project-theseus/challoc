# challoc minimum-slab A/B, 2026-09-16, aarch64 (GCE reference VM)

Phase 1 acceptance step 2: the post-delete RSS floor of the 20-minute Valkey
soak (`2026-09-16-aarch64-valkey-soak.md`, +41% vs jemalloc) came from
64 KiB slabs holding 64..102 regions of 640..1024 bytes that never empty.
This file is the throughput row for the slab-geometry change, before and
after, per CLAUDE.md. Numbers are from the GCE arm64 VM and are never
compared with Mac numbers.

- host: challoc-bench-arm64, c4a-standard-16 (Google Axion, Neoverse V2),
  16 vCPUs, Ubuntu 24.04 kernel 7.0.0-1011-gcp, 4 KiB pages,
  `guard_install=yes`; container: Wolfi (rustc 1.98.1, gcc 15.3.0)
- method: `bench/build.sh` programs, one `.so` per variant built from a copy
  of the tree (`old` = HEAD 605b981, `s14`/`s13`/`new` = the working tree),
  jemalloc = Wolfi's libjemalloc.so.2 (5.3). 5 rounds plus one warm-up,
  variants interleaved per round, every run's score listed sorted; RSS is
  `rt`'s median maxrss. Load average under 2.3 at the start of each run.
- programs: `larson 5 8 1000 5000 100 4141 16` (M ops/s, higher is better),
  `xmalloc-test -w 16 -t 5 -s 64` (M frees/s, higher is better),
  `alloc-test 1` and `alloc-test 16` (seconds, lower is better)

## Run 1: 16 KiB (`s14`) and 8 KiB (`s13`) minimum slab for every class

`LG_MIN_SLAB` 16 -> 14 / 13 with `BIN_MAX` fixed at 4 KiB.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson | old | 549.2 549.3 552.1 555.3 555.3 | 135200 |
| larson | s14 | 535.5 539.4 540.9 544.2 545.0 | 135452 |
| larson | s13 | 534.9 536.4 537.6 540.9 541.1 | 135708 |
| larson | jemalloc | 611.9 612.0 613.2 620.8 620.9 | 211808 |
| xmalloc-test | old | 334.5 349.6 374.5 420.1 433.0 | 44388 |
| xmalloc-test | s14 | 161.1 173.2 184.6 188.5 198.2 | 44708 |
| xmalloc-test | s13 | 99.8 109.2 114.5 124.9 130.8 | 45192 |
| xmalloc-test | jemalloc | 482.9 483.9 484.5 487.3 487.5 | 108244 |
| alloc-test1 | old | 2.431 2.432 2.448 2.450 2.458 | 13292 |
| alloc-test1 | s14 | 2.434 2.436 2.458 2.464 2.491 | 13292 |
| alloc-test1 | s13 | 2.445 2.449 2.450 2.461 2.469 | 13416 |
| alloc-test1 | jemalloc | 2.407 2.412 2.412 2.414 2.426 | 12632 |
| alloc-testN | old | 2.222 2.222 2.229 2.232 2.232 | 16424 |
| alloc-testN | s14 | 2.222 2.229 2.229 2.232 2.237 | 16492 |
| alloc-testN | s13 | 2.221 2.223 2.224 2.229 2.230 | 16620 |
| alloc-testN | jemalloc | 2.197 2.197 2.197 2.198 2.198 | 16728 |

Reading: larson -2.0% (s14) / -2.6% (s13) against old, alloc-test unchanged,
but xmalloc-test halves at 16 KiB (374.5 -> 184.6 median) and loses 70% at
8 KiB. xmalloc-test frees 64-byte objects on threads other than the
allocating one; a 64-byte class has 256 regions per 16 KiB slab (1024 per
64 KiB), so every refill (`plan`, up to 8 slabs per lock hold) and every
sorted flush (`release`, `Reopen` of 8 slabs per lock hold) touches four
times as many slabs and descriptor lines for the same slots. A smaller slab
cannot pay for that on such a class: with 64 or more regions per slab it
never empties by chance (0.89^64 = 0.06%) either way. Rejected.

## Run 2: two-regime rule (`new`, adopted)

Classes below 256 bytes keep the 64 KiB slab (`SMALL_SLAB_BELOW`,
`LG_SMALL_SLAB`); from 256 bytes up the slab is `max(16 KiB, 16 * size)`
(64..16 regions for 256..1024 bytes). `BIN_MAX` fixed at 4 KiB.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson | old | 547.8 551.1 551.8 552.2 557.6 | 135452 |
| larson | new | 534.0 538.6 539.8 540.9 543.2 | 134816 |
| larson | jemalloc | 611.5 612.0 615.2 619.0 619.7 | 212160 |
| xmalloc-test | old | 338.2 383.6 407.0 410.6 416.6 | 44256 |
| xmalloc-test | new | 340.4 345.8 361.3 364.0 367.8 | 44560 |
| xmalloc-test | jemalloc | 481.0 482.0 486.4 486.6 489.7 | 110016 |
| alloc-test1 | old | 2.434 2.435 2.447 2.451 2.484 | 13292 |
| alloc-test1 | new | 2.450 2.453 2.457 2.458 2.458 | 13288 |
| alloc-test1 | jemalloc | 2.395 2.399 2.403 2.406 2.424 | 12636 |
| alloc-testN | old | 2.231 2.232 2.232 2.240 2.264 | 16364 |
| alloc-testN | new | 2.220 2.227 2.227 2.228 2.251 | 16364 |
| alloc-testN | jemalloc | 2.196 2.197 2.197 2.198 2.201 | 16584 |

Reading (medians): larson 551.8 -> 539.8 M ops/s (-2.2%; the gap to
jemalloc goes from +10.3% to +12.3% on this VM), xmalloc-test 407.0 -> 361.3
(-11%, inside old's own 338..417 spread across the two runs; the 64-byte
class is untouched by the rule, so the difference is the 256-byte-and-up
classes xmalloc-test does not use plus noise), alloc-test1 2.447 -> 2.457 s
and alloc-testN 2.232 -> 2.227 s (unchanged), RSS unchanged. larson's
sizes are 8..1000 bytes, so about three quarters of its allocations now
live in 16 KiB slabs; the 2% is the extra slabs per refill and flush.

Note on the reference numbers: on this VM the *old* library is already
10.3% behind jemalloc on larson and 16% behind on xmalloc-test, where the
Mac's Docker VM had it at 7.7% behind and 2.2x ahead respectively. The
Phase 1.5 acceptance (within 10% on both VMs) is a separate item; this
change moves larson by 2 points of it.

## Run 2 on x86_64 (challoc-bench-amd64, c4-standard-16, Intel Xeon Platinum 8581C)

Same script and variants as run 2; native, not emulated. Load average 0.5
at the start.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson | old | 274.1 279.8 282.6 283.9 294.8 | 90780 |
| larson | new | 260.3 268.9 271.8 274.2 275.0 | 91056 |
| larson | jemalloc | 348.0 357.0 360.2 361.8 367.1 | 133564 |
| xmalloc-test | old | 528.2 535.1 535.7 537.0 537.2 | 45400 |
| xmalloc-test | new | 534.0 535.5 536.2 537.2 538.1 | 45376 |
| xmalloc-test | jemalloc | 347.0 347.1 347.4 349.1 349.5 | 126052 |
| alloc-test1 | old | 2.857 2.884 2.885 2.886 2.892 | 14244 |
| alloc-test1 | new | 2.846 2.847 2.854 2.865 2.875 | 14220 |
| alloc-test1 | jemalloc | 2.761 2.761 2.765 2.769 2.775 | 13616 |
| alloc-testN | old | 4.082 4.085 4.099 4.110 4.121 | 17424 |
| alloc-testN | new | 4.047 4.065 4.084 4.095 4.098 | 17448 |
| alloc-testN | jemalloc | 3.853 3.860 3.861 3.871 3.956 | 17304 |

Reading (medians): larson 282.6 -> 271.8 M ops/s (-3.8%; the gap to
jemalloc goes from +21.5% to +24.5%), xmalloc-test 535.7 -> 536.2
(unchanged, and 1.5x jemalloc here), alloc-test1 2.885 -> 2.854 s and
alloc-testN 4.099 -> 4.084 s (unchanged), RSS unchanged. The old library's
larson gap on this VM (+21.5%) is itself a Phase 1.5 acceptance item.

## Soaks with the two-regime geometry (arm64 VM), and the reading

`bench/valkey-soak.sh` on the arm64 VM (Valkey 9.1.2, quantum8 library from
the working tree vs the unpatched tree with bundled jemalloc 5.3.0; results
kept out of `bench/results/` as the geometry was not adopted; the
`STAGE=build`/soak driver is `.tmp-it11-soak.sh` in the iteration log).

240 s protocol (120 s write, 120 s delete, 60 s settle), RSS after settle:
old geometry 155.9 MiB (frag 2.24), new geometry 156.6 MiB (2.21), jemalloc
148.6 MiB (2.05). Nothing separates them at this length: jemalloc's drain
needs the long delete phase, so `DURATION=240` cannot decide the criterion
(the task 8 note claiming otherwise was wrong).

Full 20-minute protocol, new geometry vs jemalloc, same VM, same session:

| measure | challoc (16 KiB from 256 B) | jemalloc | vs |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 864055 | 903695 | +4.6% |
| write: valkey-benchmark p99 ms | 1.135 | 1.095 | +3.7% |
| write: memtier req/s | 924493 | 941188 | +1.8% |
| write: memtier p99 ms | 0.991 | 1.007 | -1.6% |
| delete: valkey-benchmark req/s | 1394438 | 1449342 | +3.9% |
| delete: valkey-benchmark p99 ms | 0.727 | 0.703 | +3.4% |
| delete: memtier req/s | 1220909 | 1269901 | +4.0% |
| delete: memtier p99 ms | 0.783 | 0.767 | +2.1% |
| peak RSS MiB (VmHWM) | 576.4 | 572.9 | +0.6% |
| RSS MiB at end of delete phase | 157.4 | 128.5 | +22.5% |
| RSS MiB after settle | 157.4 | 128.3 | +22.7% |
| allocator_frag_ratio after settle | 2.25 | 1.79 | +25.7% |
| active_defrag_hits | 276090 | 257184 | -6.8% |

The 10% post-delete criterion still fails (+22.7%; the Mac run of the
64 KiB geometry was +41%, but against a jemalloc that drained to 1.28 there
versus 1.79 here, so the two runs do not measure the same gap).

Timeline of the delete phase (10 s samples; `used`, `rss`, `active` in MiB):

```
elapsed  challoc: used  rss    active frag  hits     jemalloc: used  rss    active frag  hits
 721     63.4  158.0  153.5  2.22  276090             62.4  148.6  130.7  2.09  257184
 841     63.1  157.9  153.3  2.25  276090             62.2  139.4  121.0  1.94  257184
 961     63.0  157.6  153.4  2.25  276090             62.1  132.9  115.0  1.85  257184
1081     63.0  157.5  153.1  2.24  276090             62.0  129.9  112.4  1.81  257184
1201     62.8  157.4  152.9  2.24  276090             62.1  128.7  111.5  1.79  257184
1262 st  62.8  157.4  152.9  2.25  276090             62.0  128.3  111.3  1.79  257184
```

Both defragmenters stop for good before 721 s (the bins' waste is under
Valkey's 100 MB `active-defrag-ignore-bytes` floor in both servers). From
then on the data is constant (memtier's DEL:SET 4:1 equilibrium, about
104k keys; memtier's `key:<n>` keys and valkey-benchmark's zero-padded
`key:000000000123` keys are disjoint, so valkey-benchmark's `del` sweeps
never touch memtier's population and that population is never re-laid
out). Over those eight minutes of churn jemalloc's active memory drains
130.7 -> 111.3 MiB; challoc's stays at 153 MiB.

Where challoc's residue sits (`MEMORY MALLOC-STATS` after settle):

```
size  nregs  curregs  curslabs  occupancy   |  jemalloc bin: regs  curregs  curslabs  util
 320    51     6620      292      44%       |
 512    32     6681      464      45%       |
 640    25    13268     1240      43%       |              32    13036      859     0.474
 768    21    13237     1425      44%       |              16    13003     1465     0.554
 896    18    13176     1625      45%       |              32    12945      843     0.479
1024    16    13511     1850      46%       |               4    13227     3820     0.865
```

The occupancy of challoc's surviving slabs is 43..48% in every class, the
same as the 48..51% the 64 KiB geometry left on the Mac; slab size did not
move it. jemalloc's own 32-region bins (640, 896) end at 47..48% as well
and its 16-region bin (768) at 55%; only the 4-region 1024 bin reaches
86%. So the residue is a function of regions per slab under this churn,
but the curve is flat between 16 and 64 regions and only bends at 4..8:
matching jemalloc's total would need 4..8-region slabs (4..8 KiB) for the
512..1024-byte classes, which run 1 priced at a 2..4% larson loss for
16 KiB and 70% of xmalloc-test for 8 KiB everywhere. The second
ingredient is how holes reach the slabs: challoc's per-thread cache holds
a freed slot for LIFO reuse until the cache is full (128 slots at 1 KiB),
so under a balanced alloc/free churn almost every free is reused by the
next allocation in place and the slab layout the defragmenter left is
frozen; jemalloc's tcache GC returns idle cached slots to their slabs on a
byte-count schedule, so holes accumulate in the high slabs, first-fit
refills never revisit them, and they drain and empty (visibly faster for
small slabs). Both levers belong to acceptance step 3.
