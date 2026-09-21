# challoc Valkey soak, 2026-09-21, aarch64

- commit: f53418d
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=4a882057cec55614
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=ce0a86985b3697c1
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- challoc allocator library (path, sha256 prefix): /src/target/valkey/challoc/release/libchalloc.so ddfc69c202484485
- jemalloc allocator library (path, sha256 prefix): bundled (static)
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup
- occupancy: MEMORY MALLOC-STATS every 20 s under target/valkey/pc-default/2026-09-21-aarch64-valkey-soak-<alloc>-mstats/

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 920986 | 878208 | -4.6% |
| write: valkey-benchmark p99 ms | 1.071 | 1.119 | -4.3% |
| write: memtier req/s | 964505 | 936581 | -2.9% |
| write: memtier p99 ms | 0.95900 | 0.98300 | -2.4% |
| delete: valkey-benchmark req/s | 1423429 | 1452604 | +2.0% |
| delete: valkey-benchmark p99 ms | 0.719 | 0.703 | +2.3% |
| delete: memtier req/s | 1258879 | 1273913 | +1.2% |
| delete: memtier p99 ms | 0.76700 | 0.75900 | +1.1% |
| peak RSS MiB (VmHWM) | 579.3 | 573.5 | +1.0% |
| cgroup memory.peak MiB, write phase | - | - | - |
| cgroup memory.peak MiB, delete phase | - | - | - |
| cgroup memory.peak MiB, settle phase | - | - | - |
| peak sampled used_memory_rss MiB | 579.2 | 573.5 | +1.0% |
| RSS MiB at end of write phase | 579.0 | 573.1 | +1.0% |
| RSS MiB at end of delete phase | 130.3 | 128.6 | +1.3% |
| RSS MiB after settle | 130.3 | 128.5 | +1.4% |
| allocator_frag_ratio after settle | 1.81 | 1.79 | +1.1% |
| active_defrag_hits | 229711 | 240548 | +4.7% |
| keys after settle | 105416 | 102818 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `target/valkey/pc-default/2026-09-21-aarch64-valkey-soak-challoc.csv` (131 rows)
- jemalloc: `target/valkey/pc-default/2026-09-21-aarch64-valkey-soak-jemalloc.csv` (128 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,1,22,21,912201,1.095
write,memtier,set,22,52,30,966780,0.96700
write,valkey-benchmark,set,52,74,22,930189,1.055
write,memtier,set,74,104,30,974044,0.95100
write,valkey-benchmark,set,104,125,21,931532,1.055
write,memtier,set,125,155,30,971788,0.94300
write,valkey-benchmark,set,155,177,22,919540,1.071
write,memtier,set,177,207,30,962941,0.96700
write,valkey-benchmark,set,207,229,22,928893,1.055
write,memtier,set,229,259,30,957207,0.96700
write,valkey-benchmark,set,259,281,22,920938,1.071
write,memtier,set,281,311,30,968774,0.95100
write,valkey-benchmark,set,311,332,21,926484,1.063
write,memtier,set,332,362,30,966636,0.95900
write,valkey-benchmark,set,362,384,22,918949,1.071
write,memtier,set,384,414,30,970901,0.95900
write,valkey-benchmark,set,414,436,22,919836,1.071
write,memtier,set,436,466,30,974983,0.94300
write,valkey-benchmark,set,466,487,21,924300,1.063
write,memtier,set,487,517,30,961527,0.96700
write,valkey-benchmark,set,517,539,22,910208,1.087
write,memtier,set,539,569,30,941945,0.99900
write,valkey-benchmark,set,569,591,22,908760,1.087
write,memtier,set,591,621,30,956534,0.96700
delete,valkey-benchmark,del,621,636,15,1380643,1.143
delete,memtier,del:set=4:1,636,666,30,1245855,0.79100
delete,valkey-benchmark,del,666,680,14,1445296,0.703
delete,memtier,del:set=4:1,680,710,30,1261791,0.76700
delete,valkey-benchmark,del,710,724,14,1425110,0.719
delete,memtier,del:set=4:1,724,754,30,1258644,0.76700
delete,valkey-benchmark,del,754,768,14,1436472,0.711
delete,memtier,del:set=4:1,768,798,30,1262689,0.76700
delete,valkey-benchmark,del,798,812,14,1421868,0.719
delete,memtier,del:set=4:1,812,842,30,1246549,0.77500
delete,valkey-benchmark,del,842,856,14,1421767,0.719
delete,memtier,del:set=4:1,856,886,30,1248000,0.77500
delete,valkey-benchmark,del,886,900,14,1418339,0.719
delete,memtier,del:set=4:1,900,930,30,1259172,0.76700
delete,valkey-benchmark,del,930,944,14,1425720,0.719
delete,memtier,del:set=4:1,944,974,30,1263866,0.76700
delete,valkey-benchmark,del,974,988,14,1429490,0.711
delete,memtier,del:set=4:1,988,1018,30,1258833,0.77500
delete,valkey-benchmark,del,1018,1032,14,1417937,0.719
delete,memtier,del:set=4:1,1032,1062,30,1264861,0.76700
delete,valkey-benchmark,del,1062,1076,14,1428674,0.711
delete,memtier,del:set=4:1,1076,1106,30,1267292,0.75900
delete,valkey-benchmark,del,1106,1120,14,1426635,0.719
delete,memtier,del:set=4:1,1120,1150,30,1256791,0.77500
delete,valkey-benchmark,del,1150,1164,14,1430001,0.711
delete,memtier,del:set=4:1,1164,1194,30,1264393,0.76700
delete,valkey-benchmark,del,1194,1208,14,1420051,0.719
delete,memtier,del:set=4:1,1208,1238,30,1265564,0.76700
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,897022,1.087
write,memtier,set,22,52,30,957074,0.97500
write,valkey-benchmark,set,52,75,23,889561,1.103
write,memtier,set,75,105,30,940591,0.98300
write,valkey-benchmark,set,105,128,23,883314,1.111
write,memtier,set,128,158,30,943096,0.98300
write,valkey-benchmark,set,158,180,22,875964,1.119
write,memtier,set,180,210,30,917191,1.01500
write,valkey-benchmark,set,210,234,24,854555,1.159
write,memtier,set,234,264,30,930734,0.99900
write,valkey-benchmark,set,264,287,23,880398,1.111
write,memtier,set,287,317,30,939929,0.99100
write,valkey-benchmark,set,317,339,22,878426,1.119
write,memtier,set,339,369,30,946893,0.98300
write,valkey-benchmark,set,369,392,23,878773,1.119
write,memtier,set,392,422,30,938756,0.99900
write,valkey-benchmark,set,422,445,23,876347,1.119
write,memtier,set,445,475,30,914840,1.02300
write,valkey-benchmark,set,475,498,23,868206,1.135
write,memtier,set,498,528,30,918171,1.02300
write,valkey-benchmark,set,528,551,23,878966,1.111
write,memtier,set,551,581,30,946462,0.98300
write,valkey-benchmark,set,581,604,23,876962,1.119
write,memtier,set,604,634,30,945237,0.98300
delete,valkey-benchmark,del,634,648,14,1382170,1.127
delete,memtier,del:set=4:1,648,678,30,1262052,0.79100
delete,valkey-benchmark,del,678,692,14,1451484,0.703
delete,memtier,del:set=4:1,692,722,30,1274631,0.75900
delete,valkey-benchmark,del,722,736,14,1475035,0.687
delete,memtier,del:set=4:1,736,766,30,1280786,0.75900
delete,valkey-benchmark,del,766,779,13,1464665,0.695
delete,memtier,del:set=4:1,779,809,30,1279876,0.75900
delete,valkey-benchmark,del,809,823,14,1464772,0.695
delete,memtier,del:set=4:1,823,853,30,1277607,0.74300
delete,valkey-benchmark,del,853,867,14,1453172,0.703
delete,memtier,del:set=4:1,867,897,30,1278388,0.76700
delete,valkey-benchmark,del,897,910,13,1455922,0.703
delete,memtier,del:set=4:1,910,941,31,1263523,0.77500
delete,valkey-benchmark,del,941,954,13,1445818,0.703
delete,memtier,del:set=4:1,954,984,30,1273154,0.75900
delete,valkey-benchmark,del,984,998,14,1453066,0.703
delete,memtier,del:set=4:1,998,1028,30,1268402,0.76700
delete,valkey-benchmark,del,1028,1042,14,1450642,0.703
delete,memtier,del:set=4:1,1042,1072,30,1266133,0.75900
delete,valkey-benchmark,del,1072,1086,14,1449065,0.703
delete,memtier,del:set=4:1,1086,1116,30,1286614,0.75100
delete,valkey-benchmark,del,1116,1129,13,1467351,0.695
delete,memtier,del:set=4:1,1129,1159,30,1279088,0.75900
delete,valkey-benchmark,del,1159,1173,14,1470696,0.695
delete,memtier,del:set=4:1,1173,1203,30,1270612,0.76700
```

