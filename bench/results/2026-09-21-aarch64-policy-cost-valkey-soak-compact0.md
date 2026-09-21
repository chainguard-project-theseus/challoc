# challoc Valkey soak, 2026-09-21, aarch64

- commit: f53418d
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=4a882057cec55614
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- challoc server MALLOC_CONF: tcache_compact_shift:0 (CHALLOC_CONF; the jemalloc server runs stock)
- challoc allocator library (path, sha256 prefix): /src/target/valkey/challoc/release/libchalloc.so ddfc69c202484485
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup
- occupancy: MEMORY MALLOC-STATS every 20 s under target/valkey/pc-compact0/2026-09-21-aarch64-valkey-soak-<alloc>-mstats/

## Summary

| measure | challoc |
|---|---:|
| write: valkey-benchmark req/s | 933092 |
| write: valkey-benchmark p99 ms | 1.055 |
| write: memtier req/s | 971213 |
| write: memtier p99 ms | 0.95100 |
| delete: valkey-benchmark req/s | 1464692 |
| delete: valkey-benchmark p99 ms | 0.687 |
| delete: memtier req/s | 1277187 |
| delete: memtier p99 ms | 0.75100 |
| peak RSS MiB (VmHWM) | 580.8 |
| cgroup memory.peak MiB, write phase | - |
| cgroup memory.peak MiB, delete phase | - |
| cgroup memory.peak MiB, settle phase | - |
| peak sampled used_memory_rss MiB | 580.8 |
| RSS MiB at end of write phase | 580.7 |
| RSS MiB at end of delete phase | 153.4 |
| RSS MiB after settle | 153.1 |
| allocator_frag_ratio after settle | 2.18 |
| active_defrag_hits | 264450 |
| keys after settle | 105352 |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `target/valkey/pc-compact0/2026-09-21-aarch64-valkey-soak-challoc.csv` (130 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,21,21,945671,1.055
write,memtier,set,21,51,30,987683,0.93500
write,valkey-benchmark,set,51,72,21,942685,1.047
write,memtier,set,72,102,30,975910,0.94300
write,valkey-benchmark,set,102,124,22,926612,1.063
write,memtier,set,124,154,30,963485,0.95900
write,valkey-benchmark,set,154,176,22,921532,1.071
write,memtier,set,176,206,30,967584,0.95100
write,valkey-benchmark,set,206,227,21,939452,1.047
write,memtier,set,227,257,30,960983,0.95900
write,valkey-benchmark,set,257,279,22,927300,1.063
write,memtier,set,279,309,30,975089,0.94300
write,valkey-benchmark,set,309,330,21,934318,1.055
write,memtier,set,330,360,30,977173,0.94300
write,valkey-benchmark,set,360,381,21,931966,1.063
write,memtier,set,381,411,30,972502,0.95100
write,valkey-benchmark,set,411,433,22,933925,1.055
write,memtier,set,433,463,30,968766,0.95900
write,valkey-benchmark,set,463,484,21,931836,1.055
write,memtier,set,484,514,30,969507,0.95100
write,valkey-benchmark,set,514,536,22,929584,1.063
write,memtier,set,536,566,30,969604,0.95100
write,valkey-benchmark,set,566,587,21,932227,1.055
write,memtier,set,587,617,30,966268,0.95900
delete,valkey-benchmark,del,617,632,15,1383317,1.111
delete,memtier,del:set=4:1,632,662,30,1266744,0.78300
delete,valkey-benchmark,del,662,676,14,1472320,0.687
delete,memtier,del:set=4:1,676,706,30,1276923,0.75100
delete,valkey-benchmark,del,706,719,13,1480604,0.687
delete,memtier,del:set=4:1,719,749,30,1282714,0.74300
delete,valkey-benchmark,del,749,763,14,1449065,0.703
delete,memtier,del:set=4:1,763,793,30,1267791,0.75100
delete,valkey-benchmark,del,793,807,14,1466706,0.695
delete,memtier,del:set=4:1,807,837,30,1280521,0.74300
delete,valkey-benchmark,del,837,850,13,1476124,0.687
delete,memtier,del:set=4:1,850,880,30,1278137,0.74300
delete,valkey-benchmark,del,880,894,14,1480166,0.687
delete,memtier,del:set=4:1,894,924,30,1271195,0.75900
delete,valkey-benchmark,del,924,937,13,1471129,0.695
delete,memtier,del:set=4:1,937,967,30,1281681,0.75100
delete,valkey-benchmark,del,967,981,14,1471995,0.687
delete,memtier,del:set=4:1,981,1011,30,1279993,0.75900
delete,valkey-benchmark,del,1011,1025,14,1462844,0.695
delete,memtier,del:set=4:1,1025,1055,30,1282204,0.73500
delete,valkey-benchmark,del,1055,1068,13,1469400,0.695
delete,memtier,del:set=4:1,1068,1098,30,1277689,0.75100
delete,valkey-benchmark,del,1098,1112,14,1461882,0.695
delete,memtier,del:set=4:1,1112,1142,30,1280869,0.75100
delete,valkey-benchmark,del,1142,1155,13,1484560,0.679
delete,memtier,del:set=4:1,1155,1185,30,1280155,0.74300
delete,valkey-benchmark,del,1185,1199,14,1475579,0.687
delete,memtier,del:set=4:1,1199,1229,30,1274000,0.75900
```

