# challoc Valkey soak, 2026-09-19, aarch64

- commit: 4c7ced6
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=29c2469f316c2d7b
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=c645b848879441b2
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 925256 | 883937 | -4.5% |
| write: valkey-benchmark p99 ms | 1.047 | 1.095 | -4.4% |
| write: memtier req/s | 957372 | 938846 | -1.9% |
| write: memtier p99 ms | 0.94300 | 0.97500 | -3.3% |
| delete: valkey-benchmark req/s | 1415462 | 1434899 | +1.4% |
| delete: valkey-benchmark p99 ms | 0.711 | 0.703 | +1.1% |
| delete: memtier req/s | 1241246 | 1256101 | +1.2% |
| delete: memtier p99 ms | 0.75900 | 0.75900 | +0.0% |
| peak RSS MiB (VmHWM) | 579.9 | 574.3 | +1.0% |
| peak sampled used_memory_rss MiB | 579.9 | 574.3 | +1.0% |
| RSS MiB at end of write phase | 579.7 | 573.9 | +1.0% |
| RSS MiB at end of delete phase | 129.8 | 129.5 | +0.2% |
| RSS MiB after settle | 129.8 | 129.4 | +0.3% |
| allocator_frag_ratio after settle | 1.81 | 1.80 | +0.6% |
| active_defrag_hits | 243433 | 246200 | +1.1% |
| keys after settle | 105262 | 103066 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `/src/bench/results/2026-09-18-aarch64-gce-phase-2-acceptance-soak-valkey-soak-challoc.csv` (131 rows)
- jemalloc: `/src/bench/results/2026-09-18-aarch64-gce-phase-2-acceptance-soak-valkey-soak-jemalloc.csv` (128 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,934274,1.071
write,memtier,set,22,52,30,977696,0.93500
write,valkey-benchmark,set,52,73,21,950074,1.023
write,memtier,set,73,103,30,948532,1.12700
write,valkey-benchmark,set,103,129,26,764380,1.399
write,memtier,set,129,159,30,790516,1.32700
write,valkey-benchmark,set,159,181,22,923446,1.215
write,memtier,set,181,211,30,973815,0.94300
write,valkey-benchmark,set,211,232,21,946701,1.039
write,memtier,set,232,262,30,969756,0.94300
write,valkey-benchmark,set,262,283,21,939055,1.047
write,memtier,set,283,313,30,972358,0.94300
write,valkey-benchmark,set,313,334,21,937383,1.047
write,memtier,set,334,364,30,965612,0.95100
write,valkey-benchmark,set,364,386,22,936110,1.055
write,memtier,set,386,416,30,982047,0.93500
write,valkey-benchmark,set,416,437,21,944956,1.039
write,memtier,set,437,467,30,980246,0.93500
write,valkey-benchmark,set,467,488,21,939805,1.047
write,memtier,set,488,518,30,979728,0.92700
write,valkey-benchmark,set,518,540,22,945582,1.039
write,memtier,set,540,570,30,972527,0.94300
write,valkey-benchmark,set,570,591,21,941309,1.047
write,memtier,set,591,621,30,975635,0.94300
delete,valkey-benchmark,del,621,635,14,1388600,1.119
delete,memtier,del:set=4:1,635,665,30,1246714,0.79100
delete,valkey-benchmark,del,665,679,14,1459215,0.695
delete,memtier,del:set=4:1,679,709,30,1191735,0.94300
delete,valkey-benchmark,del,709,726,17,1200048,0.903
delete,memtier,del:set=4:1,726,756,30,1028455,1.00700
delete,valkey-benchmark,del,756,770,14,1369769,0.839
delete,memtier,del:set=4:1,770,800,30,1261078,0.76700
delete,valkey-benchmark,del,800,814,14,1432870,0.711
delete,memtier,del:set=4:1,814,844,30,1278956,0.75100
delete,valkey-benchmark,del,844,858,14,1438538,0.711
delete,memtier,del:set=4:1,858,888,30,1269499,0.75900
delete,valkey-benchmark,del,888,902,14,1433281,0.711
delete,memtier,del:set=4:1,902,932,30,1253425,0.76700
delete,valkey-benchmark,del,932,946,14,1437298,0.711
delete,memtier,del:set=4:1,946,976,30,1252975,0.76700
delete,valkey-benchmark,del,976,990,14,1462523,0.695
delete,memtier,del:set=4:1,990,1020,30,1268527,0.75900
delete,valkey-benchmark,del,1020,1034,14,1447911,0.703
delete,memtier,del:set=4:1,1034,1064,30,1262365,0.76700
delete,valkey-benchmark,del,1064,1078,14,1421565,0.719
delete,memtier,del:set=4:1,1078,1108,30,1260590,0.75900
delete,valkey-benchmark,del,1108,1122,14,1443002,0.703
delete,memtier,del:set=4:1,1122,1152,30,1263632,0.75100
delete,valkey-benchmark,del,1152,1166,14,1438538,0.703
delete,memtier,del:set=4:1,1166,1196,30,1272913,0.75100
delete,valkey-benchmark,del,1196,1209,13,1443314,0.703
delete,memtier,del:set=4:1,1209,1239,30,1266583,0.75100
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,905674,1.103
write,memtier,set,22,52,30,961417,0.97500
write,valkey-benchmark,set,52,74,22,910291,1.079
write,memtier,set,74,104,30,932232,1.23100
write,valkey-benchmark,set,104,132,28,723327,1.447
write,memtier,set,132,162,30,780986,1.36700
write,valkey-benchmark,set,162,184,22,900212,1.087
write,memtier,set,184,214,30,954738,0.98300
write,valkey-benchmark,set,214,236,22,901510,1.087
write,memtier,set,236,266,30,957327,0.98300
write,valkey-benchmark,set,266,288,22,899078,1.103
write,memtier,set,288,318,30,957662,0.97500
write,valkey-benchmark,set,318,341,23,900901,1.095
write,memtier,set,341,371,30,950926,0.98300
write,valkey-benchmark,set,371,393,22,895776,1.095
write,memtier,set,393,423,30,957897,0.97500
write,valkey-benchmark,set,423,445,22,887705,1.103
write,memtier,set,445,475,30,959013,0.97500
write,valkey-benchmark,set,475,498,23,897706,1.095
write,memtier,set,498,528,30,956195,0.97500
write,valkey-benchmark,set,528,550,22,887233,1.111
write,memtier,set,550,580,30,938471,1.00700
write,valkey-benchmark,set,580,603,23,897827,1.095
write,memtier,set,603,633,30,959284,0.97500
delete,valkey-benchmark,del,633,647,14,1377221,1.119
delete,memtier,del:set=4:1,647,677,30,1281503,0.78300
delete,valkey-benchmark,del,677,691,14,1434823,0.719
delete,memtier,del:set=4:1,691,721,30,1114085,0.99900
delete,valkey-benchmark,del,721,738,17,1210434,0.903
delete,memtier,del:set=4:1,738,768,30,1114606,0.98300
delete,valkey-benchmark,del,768,781,13,1460067,0.703
delete,memtier,del:set=4:1,781,811,30,1283514,0.75900
delete,valkey-benchmark,del,811,825,14,1459215,0.695
delete,memtier,del:set=4:1,825,855,30,1286184,0.75900
delete,valkey-benchmark,del,855,869,14,1448855,0.711
delete,memtier,del:set=4:1,869,899,30,1277686,0.76700
delete,valkey-benchmark,del,899,913,14,1446445,0.703
delete,memtier,del:set=4:1,913,943,30,1292548,0.74300
delete,valkey-benchmark,del,943,957,14,1450432,0.703
delete,memtier,del:set=4:1,957,987,30,1292581,0.75900
delete,valkey-benchmark,del,987,1000,13,1477650,0.687
delete,memtier,del:set=4:1,1000,1030,30,1280852,0.75900
delete,valkey-benchmark,del,1030,1044,14,1484450,0.687
delete,memtier,del:set=4:1,1044,1074,30,1267128,0.77500
delete,valkey-benchmark,del,1074,1087,13,1451168,0.703
delete,memtier,del:set=4:1,1087,1117,30,1268657,0.76700
delete,valkey-benchmark,del,1117,1131,14,1471779,0.687
delete,memtier,del:set=4:1,1131,1161,30,1277352,0.75900
delete,valkey-benchmark,del,1161,1175,14,1481152,0.687
delete,memtier,del:set=4:1,1175,1205,30,1292611,0.74300
```

