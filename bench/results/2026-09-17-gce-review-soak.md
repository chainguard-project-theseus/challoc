# challoc architecture review: Valkey soak with per-class occupancy, 2026-09-17, aarch64 (GCE reference VM)

Measurement for `docs/review-2026-09-17.md`. Commit `dacbedd` (working tree with
the address-ordered compacting cache flush, UNMEASURED before this run). A
600 s protocol (300 s write, 300 s delete, 60 s settle) rather than the
20-minute acceptance protocol: the question here is the *shape* of the
per-class slab occupancy over the delete phase, not the criterion. The two
allocators' CSVs are `2026-09-17-aarch64-valkey-soak-{challoc,jemalloc}.csv`.

- host: challoc-bench-arm64 (c4a-standard-16, Google Axion, Neoverse V2), kernel
  7.0.0-1011-gcp, `guard_install=yes`; container: Wolfi; Valkey 9.1.2 built
  by `contrib/valkey/build.sh` (challoc quantum8) and the unpatched tag with
  bundled jemalloc 5.3.0; `activedefrag yes`, `jemalloc-bg-thread yes`
- method: `bench/valkey-soak.sh` with `DURATION=600 SETTLE=60`, plus a
  sidecar that ran `MEMORY MALLOC-STATS` every 20 s and kept the bins table
  (`review-soak-inner.sh`, kept as `bench/probes/soak-occupancy.sh`)
- occupancy = `curregs / (curslabs * nregs)` per class from the allocator's
  own report; challoc's classes 640..1024 have 102..64 regions per 64 KiB
  slab, jemalloc's bins have 32, 16, 32 and 4 regions (640, 768, 896, 1024)

## Summary (from the soak script)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 864174 | 903607 | +4.6% |
| write: valkey-benchmark p99 ms | 1.135 | 1.087 | +4.4% |
| write: memtier req/s | 929443 | 955102 | +2.8% |
| write: memtier p99 ms | 0.999 | 0.959 | +4.2% |
| delete: valkey-benchmark req/s | 1417088 | 1477061 | +4.2% |
| delete: valkey-benchmark p99 ms | 0.711 | 0.687 | +3.5% |
| delete: memtier req/s | 1244897 | 1306207 | +4.9% |
| delete: memtier p99 ms | 0.767 | 0.727 | +5.5% |
| peak RSS MiB (VmHWM) | 568.6 | 576.8 | -1.4% |
| RSS MiB at end of delete phase | 161.7 | 135.3 | +19.5% |
| RSS MiB after settle | 161.7 | 134.3 | +20.4% |
| allocator_frag_ratio after settle | 2.36 | 1.86 | +26.9% |
| active_defrag_hits | 293377 | 236537 | -19.4% |
| keys after settle | 104077 | 102490 | - |

Positive percentages mean challoc did worse. The throughput and p99 rows are
4..5% behind jemalloc in every phase on this VM (the 2026-09-16 20-minute run
had 2..5%); they are the same single-arena sharing cost the larson counters
show, since Valkey's main thread, its io/bio threads and the load generators
run together.

## Per-class occupancy over time, challoc (compacting flush in the build)

`t` is seconds from the challoc server's start; the delete phase starts at
~311 s and defrag stops for good by ~371 s (hits constant at 293377).

```
     t rss_MiB act_MiB    frag | c320 occ/slabs c512 occ/slabs c640 occ/slabs c768 occ/slabs c896 occ/slabs c1024 occ/slabs 
   300   568.5   566.7    1.00 |  99.6%/117     99.5%/191    100.0%/5378    99.5%/572     99.7%/661     99.6%/759    
   320   568.6   567.3    2.03 |  99.4%/117     99.5%/191     20.3%/5378    99.5%/572     99.5%/661     99.9%/759    
   340   250.6   244.9    1.46 |  69.9%/117     69.7%/191     60.8%/542     69.5%/570     69.7%/658     69.3%/758    
   360   167.8   161.9    2.16 |  46.4%/79      48.5%/121     38.5%/373     48.8%/356     48.6%/412     47.6%/491    
   380   163.4   160.6    2.34 |  42.9%/78      44.4%/121     36.0%/365     44.6%/354     44.4%/410     44.3%/480    
   400   163.3   160.6    2.36 |  42.4%/78      43.9%/121     35.7%/365     44.7%/354     44.7%/409     43.7%/480    
   440   163.0   160.4    2.36 |  42.2%/78      43.7%/121     35.7%/365     44.8%/352     44.9%/409     43.9%/480    
   480   163.0   160.3    2.37 |  42.2%/78      43.7%/121     36.0%/365     44.8%/352     44.7%/409     43.7%/480    
   520   163.0   160.1    2.38 |  41.8%/78      43.9%/120     35.7%/365     44.3%/352     44.2%/409     43.6%/479    
   560   162.6   159.6    2.36 |  41.8%/78      43.8%/120     36.1%/363     44.5%/352     44.5%/409     43.6%/479    
   600   161.9   159.1    2.36 |  42.9%/76      44.5%/120     36.0%/362     44.3%/352     44.0%/409     44.0%/479    
   640   161.7   159.0    2.36 |  42.9%/76      43.7%/120     35.8%/362     44.3%/352     44.0%/409     43.9%/478    
   680   161.7   159.0    2.36 |  42.9%/76      43.7%/120     35.8%/362     44.3%/352     44.0%/409     43.9%/478    
```

From 380 s to 680 s (240 s of memtier DEL:SET 4:1 churn at ~1.2 M ops/s over a
constant ~104k keys, then 60 s idle) nothing moves: 362 slabs at 36% in the
640 class, 352 at 44%, 409 at 44%, 478 at 44%. RSS 163.3 -> 161.7 MiB.

## Per-class occupancy over time, jemalloc

`t` continues from the run's start (the jemalloc server starts at ~700 s);
its delete phase starts at ~1011 s and defrag stops by ~1041 s (hits constant
at 236537).

```
     t rss_MiB act_MiB    frag | c320 occ/slabs c512 occ/slabs c640 occ/slabs c768 occ/slabs c896 occ/slabs c1024 occ/slabs 
  1001   576.4   557.8    1.00 |  99.3%/372     99.2%/3066    99.9%/17155   99.6%/3064    99.5%/1523    99.7%/12245  
  1021   253.2   232.4    1.17 |  86.0%/371     86.0%/3062    81.7%/1608    86.0%/3052    86.1%/1513    86.0%/12237  
  1041   165.6   144.0    1.74 |  54.0%/228     54.0%/1893    52.0%/975     55.4%/1846    53.8%/940     61.4%/6684   
  1061   154.9   136.0    2.17 |  43.7%/227     45.3%/1852    42.2%/972     44.7%/1840    43.6%/936     52.5%/6367   
  1081   154.4   132.9    2.12 |  43.8%/225     46.2%/1788    42.2%/964     44.8%/1834    44.4%/926     55.9%/5956   
  1121   148.5   128.9    2.08 |  43.6%/225     49.9%/1640    42.7%/949     44.6%/1813    44.1%/921     60.5%/5400   
  1161   145.0   125.5    2.03 |  44.2%/225     54.3%/1518    43.1%/941     45.3%/1782    44.2%/916     66.4%/4936   
  1201   142.6   122.6    1.98 |  44.7%/222     58.2%/1411    43.6%/930     46.5%/1742    44.5%/911     71.4%/4605   
  1241   139.4   119.9    1.93 |  45.0%/220     61.9%/1336    44.2%/923     47.8%/1694    45.2%/902     75.6%/4335   
  1281   136.6   117.3    1.89 |  45.4%/218     64.5%/1282    44.6%/908     49.7%/1631    45.4%/889     79.9%/4149   
  1321   134.5   115.5    1.86 |  45.0%/218     66.7%/1246    45.0%/900     51.5%/1581    46.3%/872     81.4%/4037   
  1361   134.3   115.5    1.86 |  45.0%/218     66.7%/1246    45.0%/900     51.4%/1581    46.3%/872     81.4%/4037   
```

jemalloc's per-bin fill and flush counters at the end (`nfills`, `nflushes`
are cumulative, the rates are the report's per-second figures over the run):

```
    320  regs  64  nfills   56212 (  84/s)  nflushes   60294 (  90/s)  nreslabs   97611
    512  regs   8  nfills   58551 (  87/s)  nflushes   60025 (  89/s)  nreslabs  113648
    640  regs  32  nfills   83626 ( 125/s)  nflushes   81239 ( 121/s)  nreslabs  161440
    768  regs  16  nfills   69058 ( 103/s)  nflushes   64256 (  96/s)  nreslabs  138569
    896  regs  32  nfills   68078 ( 101/s)  nflushes   64804 (  96/s)  nreslabs  132378
   1024  regs   4  nfills   73082 ( 109/s)  nflushes   64293 (  96/s)  nreslabs  151138
```

## Reading

1. Both allocators leave the delete phase at the same place: 42..45%
   occupancy in every class of interest at 380 s (challoc) and 1061 s
   (jemalloc), the layout the defragmenter left when it stopped under the
   100 MB `active-defrag-ignore-bytes` floor.
2. Over the next 300 s of balanced churn jemalloc drains *as a function of
   regions per slab*: its 4-region 1024 bin goes 6367 -> 4037 slabs (52% ->
   81% occupancy), the 8-region 512 bin 1852 -> 1246 (45% -> 67%), the
   16-region 768 bin 1840 -> 1581 (45% -> 51%), the 32-region 640 and 896
   bins 972 -> 900 and 936 -> 872 (42% -> 45%, 44% -> 46%), and the 64-region
   320 bin 227 -> 218 (flat at 44..45%). The drain is what its
   allocation-scheduled GC forces: about 100 fills and 100 flushes per bin
   per second (`nfills`/`nflushes` above), each moving a few slots from a
   high slab to `slabcur`, and a slab is freed when its last region moves,
   which is 16 times more likely with 4 regions than with 64.
3. challoc's slabs have 64..102 regions in these classes and see *no*
   boundary events under the churn (the cache's length does not move under
   balanced alternation; `bench/probes/churn.c` in the counters file shows
   the same with 128-slot and 8-slot caches), so nothing migrates and the
   layout is exactly frozen. The compacting flush changes which slots a
   flush would return, but the flush does not run. jemalloc's 64-region bin
   is as frozen as challoc's classes even with its GC, so copying jemalloc's
   GC is not sufficient on challoc's slab sizes; the cache has to migrate
   allocations toward low addresses on a schedule of its own (review P1).
4. After both settle: challoc 161.7 MiB, jemalloc 134.3 MiB, +20.4%. The
   whole difference is the 640..1024 classes: challoc holds 13.2..13.4k live
   regions per class in 352..478 slabs (22..30 MiB per class at 36..44%),
   jemalloc holds the same live regions in 4037 x 4 KiB (1024: 15.8 MiB at
   81%), 1581 x 12 KiB (768), 900 x 20 KiB (640) and 872 x 28 KiB (896)
   slabs.

## Control: the same soak with the pre-compacting flush (8df225f, oldest half flushed)

Same VM, same protocol, challoc only, `src` of 8df225f built quantum8 into
`target/old` and swapped into the Valkey build's rpath directory
(`review-oldflush-vm.sh`); CSV `2026-09-17-aarch64-valkey-soak-challoc-oldflush.csv`.

| measure | old flush (8df225f) | compacting flush (dacbedd) |
|---|---:|---:|
| peak RSS MiB (VmHWM) | 568.7 | 568.6 |
| RSS MiB after settle | 161.5 | 161.7 |
| allocator_frag_ratio after settle | 2.34 | 2.36 |
| active_defrag_hits | 302480 | 293377 |
| write: memtier req/s | 940396 | 929443 |
| delete: memtier req/s | 1248523 | 1244897 |

```
     t rss_MiB act_MiB    frag | c320 occ/slabs c512 occ/slabs c640 occ/slabs c768 occ/slabs c896 occ/slabs c1024 occ/slabs 
   300   568.5   566.6    1.00 |  99.6%/117     99.4%/191    100.0%/5378    99.6%/572     99.7%/661     99.6%/759    
   340   252.4   243.5    1.58 |  63.5%/117     63.5%/190     56.8%/527     62.9%/570     63.4%/658     62.8%/759    
   360   166.2   160.8    2.25 |  45.8%/76      45.4%/125     38.5%/356     46.5%/353     45.5%/417     45.3%/489    
   380   162.4   159.6    2.35 |  43.9%/76      42.8%/125     37.0%/354     44.5%/353     43.5%/417     43.0%/488    
   440   162.3   159.4    2.36 |  43.4%/75      42.3%/125     36.5%/354     44.3%/353     43.7%/417     43.1%/486    
   520   162.1   159.2    2.34 |  44.0%/75      42.1%/125     36.9%/354     44.2%/353     43.9%/417     43.3%/484    
   600   161.8   158.9    2.35 |  44.3%/75      42.9%/125     37.0%/353     44.2%/353     43.6%/416     43.7%/482    
   680   161.5   158.8    2.34 |  44.0%/74      42.1%/125     37.1%/352     44.2%/353     43.6%/416     43.7%/482    
```

The two flush policies are indistinguishable: 161.5 against 161.7 MiB after
settle, the same 37..44% occupancies, the same 352..488 slabs per class, and
the same zero movement over the churn (class 640: 354 -> 352 slabs, 1024:
488 -> 482). Acceptance step 3's hypothesis (flush selection compacts under
churn) is refuted; the flush does not run often enough to matter, as the
churn probe predicts.
