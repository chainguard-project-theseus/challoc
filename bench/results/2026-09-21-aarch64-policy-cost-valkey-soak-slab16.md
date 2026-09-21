# challoc Valkey soak, 2026-09-21, aarch64

- commit: f53418d
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=4a882057cec55614
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- challoc allocator library (path, sha256 prefix): /src/target/valkey/challoc-slab16/release/libchalloc.so 593c65f724480f17 (CHALLOC_LIB=/src/target/valkey/challoc-slab16/release, an A/B build preloaded under the soname)
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup
- occupancy: MEMORY MALLOC-STATS every 20 s under target/valkey/pc-slab16/2026-09-21-aarch64-valkey-soak-<alloc>-mstats/

## Summary

| measure | challoc |
|---|---:|
| write: valkey-benchmark req/s | 938076 |
| write: valkey-benchmark p99 ms | 1.047 |
| write: memtier req/s | 977511 |
| write: memtier p99 ms | 0.93500 |
| delete: valkey-benchmark req/s | 1422139 |
| delete: valkey-benchmark p99 ms | 0.719 |
| delete: memtier req/s | 1266443 |
| delete: memtier p99 ms | 0.75100 |
| peak RSS MiB (VmHWM) | 580.8 |
| cgroup memory.peak MiB, write phase | - |
| cgroup memory.peak MiB, delete phase | - |
| cgroup memory.peak MiB, settle phase | - |
| peak sampled used_memory_rss MiB | 580.8 |
| RSS MiB at end of write phase | 580.6 |
| RSS MiB at end of delete phase | 131.1 |
| RSS MiB after settle | 131.1 |
| allocator_frag_ratio after settle | 1.80 |
| active_defrag_hits | 238419 |
| keys after settle | 105886 |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `target/valkey/pc-slab16/2026-09-21-aarch64-valkey-soak-challoc.csv` (131 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,21,21,941309,1.063
write,memtier,set,21,51,30,986215,0.93500
write,valkey-benchmark,set,51,73,22,941265,1.047
write,memtier,set,73,103,30,966499,0.95100
write,valkey-benchmark,set,103,124,21,934274,1.055
write,memtier,set,124,154,30,976712,0.94300
write,valkey-benchmark,set,154,176,22,934579,1.047
write,memtier,set,176,206,30,970627,0.95100
write,valkey-benchmark,set,206,227,21,937032,1.047
write,memtier,set,227,257,30,980812,0.93500
write,valkey-benchmark,set,257,278,21,941442,1.039
write,memtier,set,278,308,30,984334,0.93500
write,valkey-benchmark,set,308,330,22,939188,1.047
write,memtier,set,330,360,30,972808,0.94300
write,valkey-benchmark,set,360,381,21,937251,1.047
write,memtier,set,381,411,30,982821,0.93500
write,valkey-benchmark,set,411,432,21,945314,1.039
write,memtier,set,432,462,30,982650,0.93500
write,valkey-benchmark,set,462,483,21,941354,1.039
write,memtier,set,483,513,30,982601,0.93500
write,valkey-benchmark,set,513,535,22,929498,1.055
write,memtier,set,535,565,30,974054,0.94300
write,valkey-benchmark,set,565,586,21,934405,1.055
write,memtier,set,586,616,30,969999,0.94300
delete,valkey-benchmark,del,616,631,15,1402721,1.119
delete,memtier,del:set=4:1,631,661,30,1260282,0.77500
delete,valkey-benchmark,del,661,675,14,1406173,0.727
delete,memtier,del:set=4:1,675,705,30,1260482,0.75900
delete,valkey-benchmark,del,705,719,14,1419648,0.719
delete,memtier,del:set=4:1,719,749,30,1271724,0.75100
delete,valkey-benchmark,del,749,763,14,1438435,0.711
delete,memtier,del:set=4:1,763,793,30,1272699,0.75100
delete,valkey-benchmark,del,793,807,14,1423488,0.719
delete,memtier,del:set=4:1,807,837,30,1267190,0.74300
delete,valkey-benchmark,del,837,851,14,1407559,0.727
delete,memtier,del:set=4:1,851,881,30,1266693,0.75100
delete,valkey-benchmark,del,881,895,14,1428878,0.711
delete,memtier,del:set=4:1,895,925,30,1256368,0.75900
delete,valkey-benchmark,del,925,939,14,1424704,0.719
delete,memtier,del:set=4:1,939,969,30,1266439,0.75900
delete,valkey-benchmark,del,969,983,14,1423184,0.719
delete,memtier,del:set=4:1,983,1013,30,1267944,0.75900
delete,valkey-benchmark,del,1013,1027,14,1419547,0.719
delete,memtier,del:set=4:1,1027,1057,30,1270439,0.75100
delete,valkey-benchmark,del,1057,1071,14,1434823,0.711
delete,memtier,del:set=4:1,1071,1101,30,1262577,0.76700
delete,valkey-benchmark,del,1101,1115,14,1435338,0.711
delete,memtier,del:set=4:1,1115,1145,30,1270877,0.75100
delete,valkey-benchmark,del,1145,1159,14,1430820,0.711
delete,memtier,del:set=4:1,1159,1189,30,1274775,0.75100
delete,valkey-benchmark,del,1189,1204,15,1414627,0.727
delete,memtier,del:set=4:1,1204,1234,30,1261710,0.75900
```

