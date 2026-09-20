# challoc Valkey soak, 2026-09-20, x86_64

- commit: be7e74c
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=cd6f6ca032ba885e
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup

## Summary

| measure | challoc |
|---|---:|
| write: valkey-benchmark req/s | 769744 |
| write: valkey-benchmark p99 ms | 1.239 |
| write: memtier req/s | 946081 |
| write: memtier p99 ms | 0.96700 |
| delete: valkey-benchmark req/s | 1360088 |
| delete: valkey-benchmark p99 ms | 0.727 |
| delete: memtier req/s | 1231992 |
| delete: memtier p99 ms | 0.78300 |
| peak RSS MiB (VmHWM) | 579.0 |
| cgroup memory.peak MiB, write phase | - |
| cgroup memory.peak MiB, delete phase | - |
| cgroup memory.peak MiB, settle phase | - |
| peak sampled used_memory_rss MiB | 578.7 |
| RSS MiB at end of write phase | 578.7 |
| RSS MiB at end of delete phase | 131.3 |
| RSS MiB after settle | 131.3 |
| allocator_frag_ratio after settle | 1.80 |
| active_defrag_hits | 249848 |
| keys after settle | 105762 |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `.tmp-out/default/2026-09-20-x86_64-valkey-soak-challoc.csv` (132 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,26,26,760196,1.255
write,memtier,set,26,56,30,958363,0.94300
write,valkey-benchmark,set,56,82,26,765990,1.231
write,memtier,set,82,112,30,941876,0.96700
write,valkey-benchmark,set,112,139,27,766518,1.255
write,memtier,set,139,169,30,949224,0.95900
write,valkey-benchmark,set,169,194,25,776940,1.215
write,memtier,set,194,224,30,946763,0.96700
write,valkey-benchmark,set,224,251,27,766665,1.263
write,memtier,set,251,281,30,948221,0.95900
write,valkey-benchmark,set,281,307,26,768108,1.231
write,memtier,set,307,337,30,937386,0.98300
write,valkey-benchmark,set,337,363,26,769142,1.239
write,memtier,set,363,393,30,942500,0.96700
write,valkey-benchmark,set,393,419,26,773425,1.231
write,memtier,set,419,449,30,942772,0.96700
write,valkey-benchmark,set,449,475,26,769527,1.247
write,memtier,set,475,505,30,951963,0.95900
write,valkey-benchmark,set,505,531,26,771367,1.239
write,memtier,set,531,561,30,939426,0.99100
write,valkey-benchmark,set,561,586,25,779302,1.215
write,memtier,set,586,616,30,948401,0.96700
delete,valkey-benchmark,del,616,631,15,1340393,1.095
delete,memtier,del:set=4:1,631,661,30,1228322,0.79900
delete,valkey-benchmark,del,661,676,15,1368457,0.719
delete,memtier,del:set=4:1,676,706,30,1231875,0.78300
delete,valkey-benchmark,del,706,721,15,1366307,0.727
delete,memtier,del:set=4:1,721,751,30,1231744,0.78300
delete,valkey-benchmark,del,751,765,14,1364722,0.727
delete,memtier,del:set=4:1,765,795,30,1227545,0.78300
delete,valkey-benchmark,del,795,810,15,1361563,0.727
delete,memtier,del:set=4:1,810,840,30,1229059,0.78300
delete,valkey-benchmark,del,840,855,15,1367708,0.719
delete,memtier,del:set=4:1,855,885,30,1237234,0.77500
delete,valkey-benchmark,del,885,900,15,1362398,0.727
delete,memtier,del:set=4:1,900,930,30,1237518,0.78300
delete,valkey-benchmark,del,930,944,14,1375894,0.719
delete,memtier,del:set=4:1,944,974,30,1239986,0.77500
delete,valkey-benchmark,del,974,989,15,1344900,0.743
delete,memtier,del:set=4:1,989,1019,30,1231646,0.77500
delete,valkey-benchmark,del,1019,1034,15,1360637,0.727
delete,memtier,del:set=4:1,1034,1064,30,1228351,0.78300
delete,valkey-benchmark,del,1064,1079,15,1356668,0.727
delete,memtier,del:set=4:1,1079,1109,30,1219663,0.79100
delete,valkey-benchmark,del,1109,1123,14,1360082,0.727
delete,memtier,del:set=4:1,1123,1153,30,1238645,0.76700
delete,valkey-benchmark,del,1153,1168,15,1354738,0.727
delete,memtier,del:set=4:1,1168,1198,30,1234234,0.77500
delete,valkey-benchmark,del,1198,1213,15,1356760,0.727
delete,memtier,del:set=4:1,1213,1243,30,1232069,0.77500
```

