# challoc Valkey soak, 2026-09-18, aarch64

- commit: 75bdbc5 (the soak wrote d6e0ca4, the VM's HEAD at the time: it ran on the synced working tree, 75bdbc5's sources, library hash 9134bbad90c53460 as in the `quick` gate, committed as 75bdbc5 while the soak was starting)
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
| write: valkey-benchmark req/s | 920682 | 887593 | -3.6% |
| write: valkey-benchmark p99 ms | 1.047 | 1.095 | -4.4% |
| write: memtier req/s | 936306 | 936172 | -0.0% |
| write: memtier p99 ms | 0.99100 | 0.97500 | +1.6% |
| delete: valkey-benchmark req/s | 1410044 | 1452457 | +3.0% |
| delete: valkey-benchmark p99 ms | 0.711 | 0.687 | +3.5% |
| delete: memtier req/s | 1225905 | 1262722 | +3.0% |
| delete: memtier p99 ms | 0.78300 | 0.75100 | +4.3% |
| peak RSS MiB (VmHWM) | 579.0 | 577.0 | +0.3% |
| peak sampled used_memory_rss MiB | 579.0 | 577.0 | +0.3% |
| RSS MiB at end of write phase | 578.8 | 576.8 | +0.3% |
| RSS MiB at end of delete phase | 129.7 | 132.4 | -2.0% |
| RSS MiB after settle | 129.5 | 132.3 | -2.1% |
| allocator_frag_ratio after settle | 1.80 | 1.81 | -0.6% |
| active_defrag_hits | 238585 | 241685 | +1.3% |
| keys after settle | 105352 | 103701 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `/src/bench/results/2026-09-18-aarch64-gce-usable-size-chain-soak-valkey-soak-challoc.csv` (131 rows)
- jemalloc: `/src/bench/results/2026-09-18-aarch64-gce-usable-size-chain-soak-valkey-soak-jemalloc.csv` (128 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,943530,1.047
write,memtier,set,22,52,30,969873,0.96700
write,valkey-benchmark,set,52,73,21,950254,1.031
write,memtier,set,73,103,30,935876,1.15100
write,valkey-benchmark,set,103,129,26,755972,1.399
write,memtier,set,129,159,30,758903,1.37500
write,valkey-benchmark,set,159,181,22,916842,1.271
write,memtier,set,181,211,30,951679,0.98300
write,valkey-benchmark,set,211,232,21,933140,1.047
write,memtier,set,232,262,30,953237,0.97500
write,valkey-benchmark,set,262,284,22,941221,1.039
write,memtier,set,284,314,30,948391,0.99100
write,valkey-benchmark,set,314,335,21,938791,1.039
write,memtier,set,335,365,30,950188,0.99900
write,valkey-benchmark,set,365,387,22,928936,1.047
write,memtier,set,387,417,30,954535,0.98300
write,valkey-benchmark,set,417,438,21,936856,1.039
write,memtier,set,438,468,30,954584,0.99100
write,valkey-benchmark,set,468,489,21,938703,1.039
write,memtier,set,489,519,30,961821,0.97500
write,valkey-benchmark,set,519,541,22,933663,1.047
write,memtier,set,541,571,30,945931,0.99100
write,valkey-benchmark,set,571,592,21,930276,1.047
write,memtier,set,592,622,30,950660,0.99100
delete,valkey-benchmark,del,622,636,14,1405481,1.111
delete,memtier,del:set=4:1,636,666,30,1247565,0.79100
delete,valkey-benchmark,del,666,680,14,1453911,0.695
delete,memtier,del:set=4:1,680,710,30,1166222,0.99100
delete,valkey-benchmark,del,710,727,17,1170001,0.943
delete,memtier,del:set=4:1,727,757,30,994309,1.04700
delete,valkey-benchmark,del,757,772,15,1373060,0.847
delete,memtier,del:set=4:1,772,802,30,1262346,0.77500
delete,valkey-benchmark,del,802,816,14,1447178,0.703
delete,memtier,del:set=4:1,816,846,30,1244498,0.78300
delete,valkey-benchmark,del,846,860,14,1451695,0.703
delete,memtier,del:set=4:1,860,890,30,1246506,0.77500
delete,valkey-benchmark,del,890,903,13,1436678,0.711
delete,memtier,del:set=4:1,903,934,31,1245336,0.78300
delete,valkey-benchmark,del,934,948,14,1416330,0.719
delete,memtier,del:set=4:1,948,978,30,1245145,0.79100
delete,valkey-benchmark,del,978,992,14,1427960,0.711
delete,memtier,del:set=4:1,992,1022,30,1245681,0.78300
delete,valkey-benchmark,del,1022,1036,14,1436782,0.711
delete,memtier,del:set=4:1,1036,1066,30,1257785,0.77500
delete,valkey-benchmark,del,1066,1080,14,1432767,0.711
delete,memtier,del:set=4:1,1080,1110,30,1253593,0.78300
delete,valkey-benchmark,del,1110,1124,14,1434206,0.711
delete,memtier,del:set=4:1,1124,1154,30,1248096,0.77500
delete,valkey-benchmark,del,1154,1168,14,1419850,0.719
delete,memtier,del:set=4:1,1168,1198,30,1250728,0.76700
delete,valkey-benchmark,del,1198,1212,14,1434720,0.711
delete,memtier,del:set=4:1,1212,1242,30,1254866,0.78300
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,898957,1.111
write,memtier,set,22,52,30,962730,0.97500
write,valkey-benchmark,set,52,74,22,903261,1.095
write,memtier,set,74,104,30,919372,1.26300
write,valkey-benchmark,set,104,132,28,721293,1.479
write,memtier,set,132,162,30,788628,1.35900
write,valkey-benchmark,set,162,184,22,900090,1.095
write,memtier,set,184,214,30,973468,0.95100
write,valkey-benchmark,set,214,236,22,912575,1.079
write,memtier,set,236,266,30,954163,0.98300
write,valkey-benchmark,set,266,289,23,893056,1.103
write,memtier,set,289,319,30,953602,0.97500
write,valkey-benchmark,set,319,341,22,909339,1.087
write,memtier,set,341,371,30,941526,0.99900
write,valkey-benchmark,set,371,393,22,905223,1.087
write,memtier,set,393,423,30,955481,0.97500
write,valkey-benchmark,set,423,445,22,903342,1.095
write,memtier,set,445,475,30,949193,0.97500
write,valkey-benchmark,set,475,497,22,898392,1.095
write,memtier,set,497,527,30,940123,0.99900
write,valkey-benchmark,set,527,549,22,902894,1.095
write,memtier,set,549,579,30,945574,0.98300
write,valkey-benchmark,set,579,602,23,902690,1.095
write,memtier,set,602,632,30,950200,0.97500
delete,valkey-benchmark,del,632,646,14,1408550,1.119
delete,memtier,del:set=4:1,646,676,30,1292187,0.76700
delete,valkey-benchmark,del,676,690,14,1459002,0.695
delete,memtier,del:set=4:1,690,720,30,1121399,0.99900
delete,valkey-benchmark,del,720,736,16,1214403,0.895
delete,memtier,del:set=4:1,736,766,30,1093120,0.99100
delete,valkey-benchmark,del,766,779,13,1485553,0.679
delete,memtier,del:set=4:1,779,809,30,1292177,0.75100
delete,valkey-benchmark,del,809,823,14,1479728,0.687
delete,memtier,del:set=4:1,823,853,30,1290235,0.75100
delete,valkey-benchmark,del,853,867,14,1470588,0.687
delete,memtier,del:set=4:1,867,897,30,1287764,0.75100
delete,valkey-benchmark,del,897,910,13,1473622,0.687
delete,memtier,del:set=4:1,910,940,30,1293670,0.75100
delete,valkey-benchmark,del,940,954,14,1475035,0.687
delete,memtier,del:set=4:1,954,984,30,1294900,0.75100
delete,valkey-benchmark,del,984,997,13,1473948,0.687
delete,memtier,del:set=4:1,997,1027,30,1288395,0.75100
delete,valkey-benchmark,del,1027,1041,14,1489647,0.679
delete,memtier,del:set=4:1,1041,1071,30,1288157,0.75100
delete,valkey-benchmark,del,1071,1084,13,1476778,0.687
delete,memtier,del:set=4:1,1084,1114,30,1292993,0.75100
delete,valkey-benchmark,del,1114,1128,14,1486657,0.679
delete,memtier,del:set=4:1,1128,1158,30,1289279,0.75900
delete,valkey-benchmark,del,1158,1171,13,1488428,0.679
delete,memtier,del:set=4:1,1171,1201,30,1291105,0.75900
```

