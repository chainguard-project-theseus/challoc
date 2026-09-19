# Valkey soak with `io-threads 4`, 2026-09-19, aarch64 (Phase 3 prerequisite (d))

Lane D (measurement only), library a3ddd6d unchanged, `quantum8` build from
`make valkey STAGE=build`. Host: `challoc-lane-d-arm64`, c4a-standard-16
(Neoverse V2), Ubuntu 24.04 kernel 7.0.0-1011-gcp, the dev image, the
reference VM recipe (`bench/gce/README.md`), nothing else running; the VM
was created for this lane and deleted afterwards. 17:54..18:46 UTC.

Protocol: `make valkey-soak IO_THREADS=4` (the new variable, `--io-threads 4`
on the server command line; `CONFIG GET io-threads` read back `4` under
both allocators, the sidecar saw threads `io_thd_1..3` beside the main
thread), otherwise the acceptance protocol: 600 s write, 600 s delete, 60
s settle, `activedefrag yes`, stock memtier key stream, Valkey 9.1.2,
challoc first then the bundled jemalloc 5.3.0 built by the soak script,
`bench/probes/valkey-perf-sidecar.sh ~/logs/d-sidecar 700 1205` attached
from the VM host (`perf stat -I 1000` on the whole server process for its
life, 60 s `perf record` windows at t=100 and t=700, snapshots around them
and at t=1205). The summary table is the soak script's own. Samples:
`2026-09-19-aarch64-gce-valkey-soak-iothreads4-{challoc,jemalloc}.csv`;
`MEMORY MALLOC-STATS` after the settle:
`...-iothreads4-challoc-mstats.txt` (whole) and
`...-iothreads4-jemalloc-mstats.txt` (general, merged and the arenas with
assigned threads; the 60 idle arenas dropped). Positive = challoc worse.

Reference for the same protocol at the default single io-thread: the
2026-09-18 soak at 2c8731f (`2026-09-18-aarch64-gce-decay-soak-valkey-soak.md`,
also with the sidecar), last column.

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc | 1 io-thread (2c8731f) |
|---|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 1568285 | 1663545 | +6.1% | -2.2% |
| write: valkey-benchmark p99 ms | 0.575 | 0.527 | +9.1% | -2.9% |
| write: memtier req/s | 1782439 | 1825461 | +2.4% | +0.1% |
| write: memtier p99 ms | 0.423 | 0.415 | +1.9% | -0.8% |
| delete: valkey-benchmark req/s | 2024597 | 2025318 | +0.0% | +2.8% |
| delete: valkey-benchmark p99 ms | 0.407 | 0.327 | +24.5% | +3.5% |
| delete: memtier req/s | 2362644 | 2653140 | +12.3% | +3.3% |
| delete: memtier p99 ms | 0.311 | 0.279 | +11.5% | +3.2% |
| peak RSS MiB (VmHWM) | 766.1 | 682.0 | +12.3% | +0.1% |
| peak sampled used_memory_rss MiB | 765.1 | 682.0 | +12.2% | |
| RSS MiB at end of write phase | 672.0 | 659.7 | +1.9% | |
| RSS MiB at end of delete phase | 126.9 | 122.2 | +3.8% | -2.4% |
| RSS MiB after settle | 126.6 | 122.3 | +3.5% | -2.4% |
| allocator_frag_ratio after settle | 1.50 | 1.55 | -3.2% | -1.1% |
| active_defrag_hits | 581479 | 338888 | -41.7% (more) | +2.2% |
| keys after settle | 102735 | 101272 | | |

Against the Phase 1 criteria: throughput and p99 "at least equal" FAIL
(write valkey-benchmark -6.1% throughput / +9.1% p99, delete memtier
-12.3% / +11.5%, delete valkey-benchmark p99 +24.5% at equal throughput);
peak RSS +12.3% FAIL (band 5%); RSS after settle +3.5% PASS (band 10%).
With one io-thread the same library passes all four; `io-threads 4`
exposes a cost the reference soak does not see.

Throughput here is about 1.7x the single-thread soak's (1.57..2.36 M
req/s against 0.9..1.4 M): the server is CPU-bound on its four threads
under both allocators (per-thread CPU ticks in the 60 s windows: main
6191, io_thd 6192 / 6193 / 6161 of 6200 possible under challoc, 6132 /
6126 / 6127 / 6110 under jemalloc; `task-clock` 4007 vs 3998 ms/s), so
the generators' rates are the server's cycles per command.

## Sidecar: per-command counters (whole process, all threads)

Window t=100..162 (write, valkey-benchmark SET) and t=700..761 (delete,
valkey-benchmark DEL); `total_commands_processed` deltas over the `perf
stat -I 1000` sums between the snapshots.

| phase | allocator | commands | cycles/cmd | ins/cmd | IPC | L1D miss/cmd | dTLB miss/cmd | branch miss/cmd |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| write | challoc | 89.5 M | 6842 | 12566 | 1.84 | 92.3 | 46.8 | 9.5 |
| write | jemalloc | 91.2 M | 6598 | 12492 | 1.89 | 81.5 | 43.3 | 13.7 |
| write | delta | | +3.7% | +0.6% | | +13% | +8% | -31% |
| delete | challoc | 120.8 M | 4698 | 10111 | 2.15 | 54.1 | 33.3 | 6.4 |
| delete | jemalloc | 129.9 M | 4231 | 9790 | 2.31 | 43.9 | 26.0 | 7.0 |
| delete | delta | | +11.0% | +3.3% | | +23% | +28% | -9% |

Single io-thread reference (2c8731f): 3320 vs 3286 cycles/cmd (+1.0%)
write, 2314 vs 2256 (+2.6%) delete. Per command the four-thread server
spends twice the cycles of the single-thread one under both allocators
(the io threads busy-poll their queues: `spmcDequeue` is the top symbol,
8..9%, and Valkey's own `__aarch64_cas8_relax` / `__aarch64_ldadd8_rel`
queue atomics another 8.7%), and challoc's deficit grows from +1.0/+2.6%
to +3.7/+11.0%.

Where the cycles go (`perf report --sort dso`, share of the 60 s window;
jemalloc is linked into `valkey-server`, its internal symbols are summed
separately from the `perf report --sort dso,sym` list):

| window | allocator | valkey-server (own code) | allocator code | libc | kernel |
|---|---|---:|---:|---:|---:|
| write | challoc | 56.5% | 7.80% (libchalloc.so) | 18.0% | 17.6% |
| write | jemalloc | 55.5% | 7.89% (je_*, tcache, extent, rtree, sz_ in valkey-server) | 17.5% | 19.1% |
| delete | challoc | 46.1% | 4.24% | 27.5% | 22.1% |
| delete | jemalloc | 45.4% | 4.58% | 25.4% | 24.6% |

In cycles per command (share x cycles/cmd), delete: Valkey's own code
2165 vs 1921 (+244), libc (the two hot `memcpy`/`memset`-shaped
addresses at 0x154c80 / 0x154f20, 17% of the window) 1293 vs 1075
(+218), allocator 199 vs 194 (+5), kernel 1040 vs 1041. The allocator's
own code costs the same; the extra cycles are Valkey and libc running on
challoc-placed memory with 10 more L1D misses and 7 more dTLB misses per
command. The allocator's symbols: `je_malloc_usable_size` 4.01% / 2.77%
(write / delete; jemalloc's 3.29% / 1.94%), `je_malloc` 0.81 / 0.59,
`je_sdallocx` 0.70 / 0.62, then atomics (`ldadd4_acq_rel` 0.37,
`ldclr8_rel` 0.30, `ldset8_acq` 0.23, `swp4_rel` 0.21: the bitmap and
slot-count RMWs of the cross-thread path), `ClassState::claim_with` 0.32,
`release_owned` 0.16, `Heap::flush` 0.12, `free_sized_slow` 0.10,
`Heap::free_other` 0.05. The cross-arena free path itself (`free_other`,
`flush`, the RMWs) is under 1.5% of cycles; jemalloc's counterpart
(`tcache_bin_flush_small` 0.66, `arena_cache_bin_fill_small` 0.41,
`edata_heap_remove_first` 0.97, `tcache_bin_flush_edatas_lookup` 0.32) is
about 2.4%.

## `MALLOC-STATS` after the settle (the state the delete phase leaves)

challoc (`Arenas: 4 (64 automatic)`: the main thread and the three
`io_thd` threads each built one; the background thread is present because
Valkey's `jemalloc-bg-thread yes` sets `background_thread` through
`mallctl`); `Allocated: 79.5 MB, active 135.3 MB, resident 140.8 MB,
mapped 936 MB, retained 795 MB`, `pdirty 0` everywhere (the settle purged
the delete phase's Empty slabs):

| arena | small allocated | small active | slabs with live regions (bins) | occupancy | large |
|---|---:|---:|---:|---:|---:|
| arenas[0] (main thread) | 6.8 MB | 39.1 MB | 6.5 MiB live in 37.3 MiB of slabs, 31 classes | 17% | 5.5 MB allocated, 19.3 MB active (2 huge runs) |
| arenas[1] | 11.0 MB | 14.6 MB | 10.5 in 13.9, 24 classes | 75% | 20 KB |
| arenas[2] | 54.3 MB | 57.4 MB | 51.8 in 54.7, 24 classes | 95% | 20 KB |
| arenas[3] | 1.8 MB | 3.3 MB | 1.8 in 3.2, 20 classes | 56% | 20 KB |

jemalloc: `Arenas: 65`, four with one assigned thread each (small
allocated 4.0 / 3.2 / 15.3 / 39.5 MB), `Allocated: 67.8 MB, active 105.1
MB, resident 120.2 MB, retained 784 MB`.

Reading of the layout: with io-threads the io threads parse the query
buffer and allocate the argv objects (key and value `robj`/`sds`), the
main thread executes the command and stores the value object it was
handed, and frees are offloaded back (`tryOffloadFreeArgvToIOThreads`,
`ioThreadFreeArgv`, both in the delete window's top symbols). So the
values live in the arenas of the io threads that parsed them (arenas[2]
holds 52 MiB of the 71 MiB live), and every DEL is a cross-arena free
from the main thread or a third io thread: the `others` path of design
6.2 (a cache node per foreign arena the heap touches, up to three per
heap here, twelve in the process; the report does not print the node
count, so that bound is from the design, not measured) and the sized
path's flush routing. The main thread's arena ends the delete phase at
17% occupancy (6.5 MiB live in 37 MiB of slabs): its own allocations
(`tryObjectEncodingEx` re-encodings, dict entries, client buffers) are
what DEL leaves sparse, and active defrag worked 1.7x harder on it
(581k hits against 339k) for the same end state (+3.5% RSS after
settle, frag ratio 1.50 vs 1.55).

## Reading

1. Criteria: two of the four FAIL with `io-threads 4` (throughput/p99,
   peak RSS +12.3%); post-delete RSS passes (+3.5%). The single-thread
   soak on the same library passes all four, so the io-threads
   configuration is where challoc is behind Valkey's bundled jemalloc
   today, and by more than xmalloc-test predicted.
2. Cost of the cross-thread free path: its own instructions are cheap
   (allocator code 7.8% vs 7.9% of cycles in the write window, 4.2% vs
   4.6% in delete; `free_other` + `flush` + the RMW atomics under 1.5%).
   The deficit is +10 L1D and +7 dTLB misses per command taken by
   Valkey's own code and libc's memcpy on the objects challoc placed:
   with four arenas each class region is touched from four arenas' slabs,
   and a key's `robj`, `sds` and dict entry come from three regions of
   three arenas. This is the same placement term the single-thread soak
   attributed to `malloc_usable_size` before `sdallocx` (4e/4f), now
   multiplied by the thread count. A fix belongs to the fast path or the
   arena assignment (one arena for the io threads, as jemalloc's
   `narenas` would effectively give with percpu; or values allocated in
   the executing thread's arena), which this lane does not touch.
3. Peak RSS +12.3% (766 vs 682 MiB): the write phase ends at +1.9% (672
   vs 660), so the peak is inside the delete phase's DEL:SET churn, where
   freed values in the four arenas' caches and the `others` nodes hold
   memory that the single-thread server returned to its one arena's
   slabs at once. The delete-phase curve is in the CSVs.
4. What to run next (for the human): the same soak with `narenas:1`
   under challoc (one arena, the `others` path never taken) to split the
   placement term from the cross-arena term; `io-threads 2` and `8` for
   the trend; the per-CPU cache work of Phase 2.5 task 12 (CLDEMOTE on
   cross-thread frees) has this soak as its intended workload.

## Samples

- challoc: `bench/results/2026-09-19-aarch64-gce-valkey-soak-iothreads4-challoc.csv` (131 rows)
- jemalloc: `bench/results/2026-09-19-aarch64-gce-valkey-soak-iothreads4-jemalloc.csv` (130 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,13,13,1650982,0.855
write,memtier,set,13,43,30,1826223,0.42300
write,valkey-benchmark,set,43,55,12,1583531,0.943
write,memtier,set,55,85,30,1817189,0.80700
write,valkey-benchmark,set,85,98,13,1591090,0.567
write,memtier,set,98,128,30,1497205,0.58300
write,valkey-benchmark,set,128,143,15,1314406,0.759
write,memtier,set,143,173,30,1612754,0.55900
write,valkey-benchmark,set,173,186,13,1570475,0.575
write,memtier,set,186,216,30,1823094,0.41500
write,valkey-benchmark,set,216,228,12,1588562,0.567
write,memtier,set,228,258,30,1813816,0.42300
write,valkey-benchmark,set,258,271,13,1597189,0.567
write,memtier,set,271,301,30,1817546,0.41500
write,valkey-benchmark,set,301,314,13,1579280,0.575
write,memtier,set,314,344,30,1818608,0.41500
write,valkey-benchmark,set,344,356,12,1577038,0.567
write,memtier,set,356,386,30,1815450,0.42300
write,valkey-benchmark,set,386,399,13,1588058,0.567
write,memtier,set,399,429,30,1825882,0.41500
write,valkey-benchmark,set,429,442,13,1579155,0.575
write,memtier,set,442,472,30,1828106,0.41500
write,valkey-benchmark,set,472,485,13,1562500,0.575
write,memtier,set,485,515,30,1827764,0.41500
write,valkey-benchmark,set,515,527,12,1587932,0.575
write,memtier,set,527,557,30,1816257,0.42300
write,valkey-benchmark,set,557,570,13,1585791,0.567
write,memtier,set,570,600,30,1814254,0.42300
delete,valkey-benchmark,del,600,610,10,1988467,0.631
delete,memtier,del:set=4:1,610,640,30,2362879,0.35900
delete,valkey-benchmark,del,640,650,10,2054021,0.391
delete,memtier,del:set=4:1,650,680,30,2405071,0.31100
delete,valkey-benchmark,del,680,689,9,2055287,0.399
delete,memtier,del:set=4:1,689,719,30,2117825,0.43900
delete,valkey-benchmark,del,719,729,10,2049180,0.527
delete,memtier,del:set=4:1,729,759,30,1950555,0.44700
delete,valkey-benchmark,del,759,769,10,2044781,0.399
delete,memtier,del:set=4:1,769,799,30,2402783,0.31100
delete,valkey-benchmark,del,799,809,10,2008435,0.391
delete,memtier,del:set=4:1,809,839,30,2380833,0.31100
delete,valkey-benchmark,del,839,849,10,1999400,0.399
delete,memtier,del:set=4:1,849,879,30,2397622,0.31100
delete,valkey-benchmark,del,879,889,10,2028809,0.391
delete,memtier,del:set=4:1,889,919,30,2439073,0.31100
delete,valkey-benchmark,del,919,929,10,2014910,0.407
delete,memtier,del:set=4:1,929,959,30,2429501,0.31100
delete,valkey-benchmark,del,959,969,10,2051282,0.407
delete,memtier,del:set=4:1,969,999,30,2444246,0.30300
delete,valkey-benchmark,del,999,1008,9,2041858,0.407
delete,memtier,del:set=4:1,1008,1038,30,2406127,0.31100
delete,valkey-benchmark,del,1038,1049,11,1963672,0.407
delete,memtier,del:set=4:1,1049,1079,30,2416521,0.31100
delete,valkey-benchmark,del,1079,1088,9,2033967,0.415
delete,memtier,del:set=4:1,1088,1118,30,2414357,0.31100
delete,valkey-benchmark,del,1118,1128,10,2001601,0.415
delete,memtier,del:set=4:1,1128,1159,31,2422589,0.31100
delete,valkey-benchmark,del,1159,1168,9,2037075,0.399
delete,memtier,del:set=4:1,1168,1198,30,2395868,0.31100
delete,valkey-benchmark,del,1198,1208,10,2020814,0.415
delete,memtier,del:set=4:1,1208,1238,30,2416453,0.31100
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,12,12,1623772,0.967
write,memtier,set,12,42,30,1849721,0.42300
write,valkey-benchmark,set,42,54,12,1680108,0.535
write,memtier,set,54,84,30,1858442,0.41500
write,valkey-benchmark,set,84,96,12,1688619,0.527
write,memtier,set,96,126,30,1553247,0.57500
write,valkey-benchmark,set,126,140,14,1401345,0.703
write,memtier,set,140,170,30,1610332,0.57500
write,valkey-benchmark,set,170,182,12,1694341,0.527
write,memtier,set,182,212,30,1860433,0.41500
write,valkey-benchmark,set,212,224,12,1657138,0.543
write,memtier,set,224,254,30,1861908,0.41500
write,valkey-benchmark,set,254,266,12,1661682,0.543
write,memtier,set,266,296,30,1858510,0.41500
write,valkey-benchmark,set,296,308,12,1691904,0.527
write,memtier,set,308,338,30,1882453,0.40700
write,valkey-benchmark,set,338,350,12,1720578,0.519
write,memtier,set,350,380,30,1871628,0.41500
write,valkey-benchmark,set,380,391,11,1689047,0.527
write,memtier,set,391,421,30,1864710,0.41500
write,valkey-benchmark,set,421,433,12,1693624,0.527
write,memtier,set,433,463,30,1862379,0.41500
write,valkey-benchmark,set,463,475,12,1682935,0.535
write,memtier,set,475,505,30,1863072,0.41500
write,valkey-benchmark,set,505,517,12,1681379,0.535
write,memtier,set,517,547,30,1853836,0.41500
write,valkey-benchmark,set,547,559,12,1698514,0.527
write,memtier,set,559,589,30,1863224,0.41500
write,valkey-benchmark,set,589,601,12,1688191,0.527
write,memtier,set,601,631,30,1868022,0.41500
delete,valkey-benchmark,del,631,641,10,1986492,0.623
delete,memtier,del:set=4:1,641,671,30,2687270,0.28700
delete,valkey-benchmark,del,671,681,10,2039568,0.311
delete,memtier,del:set=4:1,681,711,30,2521838,0.39900
delete,valkey-benchmark,del,711,721,10,2003807,0.543
delete,memtier,del:set=4:1,721,751,30,2160048,0.42300
delete,valkey-benchmark,del,751,761,10,2005214,0.535
delete,memtier,del:set=4:1,761,791,30,2688597,0.28700
delete,valkey-benchmark,del,791,801,10,1960400,0.327
delete,memtier,del:set=4:1,801,831,30,2706720,0.27900
delete,valkey-benchmark,del,831,841,10,2023268,0.319
delete,memtier,del:set=4:1,841,871,30,2719722,0.27900
delete,valkey-benchmark,del,871,881,10,2036038,0.327
delete,memtier,del:set=4:1,881,911,30,2687901,0.28700
delete,valkey-benchmark,del,911,920,9,2034795,0.319
delete,memtier,del:set=4:1,920,950,30,2715486,0.27900
delete,valkey-benchmark,del,950,960,10,2042692,0.311
delete,memtier,del:set=4:1,960,990,30,2703983,0.27900
delete,valkey-benchmark,del,990,1000,10,2021836,0.327
delete,memtier,del:set=4:1,1000,1030,30,2700073,0.27900
delete,valkey-benchmark,del,1030,1040,10,2057402,0.319
delete,memtier,del:set=4:1,1040,1070,30,2705360,0.27900
delete,valkey-benchmark,del,1070,1080,10,2010252,0.327
delete,memtier,del:set=4:1,1080,1110,30,2695914,0.27900
delete,valkey-benchmark,del,1110,1120,10,2064410,0.327
delete,memtier,del:set=4:1,1120,1150,30,2699179,0.27900
delete,valkey-benchmark,del,1150,1160,10,2030457,0.311
delete,memtier,del:set=4:1,1160,1190,30,2706231,0.27900
delete,valkey-benchmark,del,1190,1199,9,2063132,0.335
delete,memtier,del:set=4:1,1199,1229,30,2698780,0.27900
```

