# challoc Valkey soak, 2026-09-20, aarch64

- commit: be7e74c
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=29c2469f316c2d7b
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- challoc server MALLOC_CONF: prefault:true,background_thread:true (CHALLOC_CONF; the jemalloc server runs stock)
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup

## Summary

| measure | challoc |
|---|---:|
| write: valkey-benchmark req/s | 910173 |
| write: valkey-benchmark p99 ms | 1.079 |
| write: memtier req/s | 945549 |
| write: memtier p99 ms | 0.99900 |
| delete: valkey-benchmark req/s | 1403722 |
| delete: valkey-benchmark p99 ms | 0.727 |
| delete: memtier req/s | 1232707 |
| delete: memtier p99 ms | 0.79100 |
| peak RSS MiB (VmHWM) | 582.5 |
| cgroup memory.peak MiB, write phase | - |
| cgroup memory.peak MiB, delete phase | - |
| cgroup memory.peak MiB, settle phase | - |
| peak sampled used_memory_rss MiB | 582.5 |
| RSS MiB at end of write phase | 582.2 |
| RSS MiB at end of delete phase | 134.2 |
| RSS MiB after settle | 134.2 |
| allocator_frag_ratio after settle | 1.80 |
| active_defrag_hits | 233518 |
| keys after settle | 105359 |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `.tmp-out/prefault/2026-09-20-aarch64-valkey-soak-challoc.csv` (132 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,909835,1.095
write,memtier,set,22,52,30,954437,0.99900
write,valkey-benchmark,set,52,74,22,918105,1.063
write,memtier,set,74,104,30,949263,0.99900
write,valkey-benchmark,set,104,125,21,916128,1.063
write,memtier,set,125,155,30,947246,0.99100
write,valkey-benchmark,set,155,177,22,909008,1.079
write,memtier,set,177,207,30,948389,0.99900
write,valkey-benchmark,set,208,230,22,904691,1.095
write,memtier,set,230,260,30,948190,0.99900
write,valkey-benchmark,set,260,282,22,906742,1.087
write,memtier,set,282,312,30,940646,1.00700
write,valkey-benchmark,set,312,334,22,907729,1.079
write,memtier,set,334,364,30,937972,0.99900
write,valkey-benchmark,set,364,386,22,908348,1.079
write,memtier,set,386,416,30,949179,0.97500
write,valkey-benchmark,set,416,438,22,912492,1.071
write,memtier,set,438,468,30,940040,0.97500
write,valkey-benchmark,set,468,490,22,915709,1.063
write,memtier,set,490,520,30,945289,0.99900
write,valkey-benchmark,set,520,542,22,904364,1.095
write,memtier,set,542,572,30,934447,0.99900
write,valkey-benchmark,set,572,594,22,908926,1.071
write,memtier,set,594,624,30,951485,0.98300
delete,valkey-benchmark,del,624,639,15,1356760,1.135
delete,memtier,del:set=4:1,639,669,30,1224243,0.80700
delete,valkey-benchmark,del,669,683,14,1415829,0.719
delete,memtier,del:set=4:1,683,713,30,1238019,0.79900
delete,valkey-benchmark,del,713,727,14,1398797,0.727
delete,memtier,del:set=4:1,727,757,30,1230011,0.79100
delete,valkey-benchmark,del,757,771,14,1409741,0.727
delete,memtier,del:set=4:1,771,801,30,1242488,0.77500
delete,valkey-benchmark,del,801,815,14,1405185,0.727
delete,memtier,del:set=4:1,815,845,30,1228407,0.79100
delete,valkey-benchmark,del,845,860,15,1407064,0.727
delete,memtier,del:set=4:1,860,890,30,1229016,0.79900
delete,valkey-benchmark,del,890,904,14,1409741,0.727
delete,memtier,del:set=4:1,904,934,30,1231676,0.79900
delete,valkey-benchmark,del,934,948,14,1405679,0.727
delete,memtier,del:set=4:1,948,978,30,1233466,0.79100
delete,valkey-benchmark,del,978,992,14,1403115,0.727
delete,memtier,del:set=4:1,992,1022,30,1233277,0.78300
delete,valkey-benchmark,del,1022,1037,15,1408451,0.727
delete,memtier,del:set=4:1,1037,1067,30,1229125,0.79900
delete,valkey-benchmark,del,1067,1081,14,1408054,0.719
delete,memtier,del:set=4:1,1081,1111,30,1237053,0.79100
delete,valkey-benchmark,del,1111,1125,14,1403706,0.727
delete,memtier,del:set=4:1,1125,1155,30,1224965,0.80700
delete,valkey-benchmark,del,1155,1169,14,1411134,0.727
delete,memtier,del:set=4:1,1169,1199,30,1234483,0.79900
delete,valkey-benchmark,del,1199,1214,15,1408848,0.727
delete,memtier,del:set=4:1,1214,1244,30,1241663,0.78300
```

