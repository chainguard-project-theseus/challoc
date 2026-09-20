# challoc Valkey soak, 2026-09-20, x86_64

- commit: be7e74c
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=cd6f6ca032ba885e
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- challoc server MALLOC_CONF: prefault:true,background_thread:true (CHALLOC_CONF; the jemalloc server runs stock)
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup

## Summary

| measure | challoc |
|---|---:|
| write: valkey-benchmark req/s | 765462 |
| write: valkey-benchmark p99 ms | 1.231 |
| write: memtier req/s | 944402 |
| write: memtier p99 ms | 0.97500 |
| delete: valkey-benchmark req/s | 1355158 |
| delete: valkey-benchmark p99 ms | 0.727 |
| delete: memtier req/s | 1226150 |
| delete: memtier p99 ms | 0.79100 |
| peak RSS MiB (VmHWM) | 583.6 |
| cgroup memory.peak MiB, write phase | - |
| cgroup memory.peak MiB, delete phase | - |
| cgroup memory.peak MiB, settle phase | - |
| peak sampled used_memory_rss MiB | 583.6 |
| RSS MiB at end of write phase | 583.6 |
| RSS MiB at end of delete phase | 136.1 |
| RSS MiB after settle | 136.1 |
| allocator_frag_ratio after settle | 1.80 |
| active_defrag_hits | 248634 |
| keys after settle | 105656 |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `.tmp-out/prefault/2026-09-20-x86_64-valkey-soak-challoc.csv` (127 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,27,27,744435,1.311
write,memtier,set,27,57,30,954837,0.95900
write,valkey-benchmark,set,57,84,27,756086,1.263
write,memtier,set,84,114,30,946688,0.96700
write,valkey-benchmark,set,114,140,26,772410,1.223
write,memtier,set,140,170,30,945991,0.96700
write,valkey-benchmark,set,170,196,26,760138,1.271
write,memtier,set,196,226,30,944058,0.97500
write,valkey-benchmark,set,226,252,26,771070,1.231
write,memtier,set,252,282,30,944044,0.96700
write,valkey-benchmark,set,282,308,26,757805,1.287
write,memtier,set,308,338,30,942595,0.97500
write,valkey-benchmark,set,338,364,26,773635,1.223
write,memtier,set,364,394,30,942160,0.99100
write,valkey-benchmark,set,394,421,27,761238,1.279
write,memtier,set,421,451,30,942798,0.97500
write,valkey-benchmark,set,451,477,26,769320,1.231
write,memtier,set,477,507,30,938642,0.97500
write,valkey-benchmark,set,507,532,25,777968,1.215
write,memtier,set,532,562,30,947094,0.96700
write,valkey-benchmark,set,562,588,26,775976,1.223
write,memtier,set,588,618,30,939517,1.01500
delete,valkey-benchmark,del,618,633,15,1329345,1.111
delete,memtier,del:set=4:1,633,663,30,1223144,0.81500
delete,valkey-benchmark,del,663,678,15,1372401,0.719
delete,memtier,del:set=4:1,678,708,30,1227088,0.79100
delete,valkey-benchmark,del,708,723,15,1320655,0.759
delete,memtier,del:set=4:1,723,753,30,1224401,0.79100
delete,valkey-benchmark,del,753,768,15,1357036,0.727
delete,memtier,del:set=4:1,768,798,30,1217252,0.79900
delete,valkey-benchmark,del,798,813,15,1369488,0.719
delete,memtier,del:set=4:1,813,843,30,1225776,0.79100
delete,valkey-benchmark,del,843,857,14,1363698,0.727
delete,memtier,del:set=4:1,857,887,30,1230789,0.78300
delete,valkey-benchmark,del,887,902,15,1350256,0.727
delete,memtier,del:set=4:1,902,932,30,1224910,0.79100
delete,valkey-benchmark,del,932,947,15,1362027,0.719
delete,memtier,del:set=4:1,947,977,30,1231951,0.78300
delete,valkey-benchmark,del,977,992,15,1360822,0.727
delete,memtier,del:set=4:1,992,1022,30,1215027,0.79100
delete,valkey-benchmark,del,1022,1037,15,1355197,0.727
delete,memtier,del:set=4:1,1037,1067,30,1224405,0.79100
delete,valkey-benchmark,del,1067,1081,14,1359434,0.727
delete,memtier,del:set=4:1,1081,1111,30,1225739,0.78300
delete,valkey-benchmark,del,1111,1126,15,1353180,0.727
delete,memtier,del:set=4:1,1126,1156,30,1235742,0.78300
delete,valkey-benchmark,del,1156,1171,15,1363512,0.719
delete,memtier,del:set=4:1,1171,1201,30,1233730,0.77500
```

