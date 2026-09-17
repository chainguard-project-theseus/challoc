# challoc Valkey soak, 2026-09-17, aarch64

- commit: 4021694
- host: Google Axion (Neoverse V2), c4a-standard-16, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=29c2469f316c2d7b
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=c645b848879441b2
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; INFO sampled every 10 s
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 867555 | 954256 | +10.0% |
| write: valkey-benchmark p99 ms | 1.127 | 1.031 | +9.3% |
| write: memtier req/s | 954465 | 994011 | +4.1% |
| write: memtier p99 ms | 0.95900 | 0.91900 | +4.4% |
| delete: valkey-benchmark req/s | 1424450 | 1499258 | +5.3% |
| delete: valkey-benchmark p99 ms | 0.711 | 0.671 | +6.0% |
| delete: memtier req/s | 1260010 | 1315712 | +4.4% |
| delete: memtier p99 ms | 0.75100 | 0.72700 | +3.3% |
| peak RSS MiB (VmHWM) | 601.8 | 579.1 | +3.9% |
| peak sampled used_memory_rss MiB | 601.8 | 579.1 | +3.9% |
| RSS MiB at end of write phase | 601.6 | 578.7 | +4.0% |
| RSS MiB at end of delete phase | 122.6 | 133.0 | -7.8% |
| RSS MiB after settle | 122.6 | 132.7 | -7.6% |
| allocator_frag_ratio after settle | 1.58 | 1.83 | -13.7% |
| active_defrag_hits | 204168 | 272182 | +33.3% |
| keys after settle | 105103 | 104715 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `/src/target/soak-it15/2026-09-17-aarch64-valkey-soak-challoc.csv` (128 rows)
- jemalloc: `/src/target/soak-it15/2026-09-17-aarch64-valkey-soak-jemalloc.csv` (129 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,24,24,864416,1.143
write,memtier,set,24,54,30,970585,0.93500
write,valkey-benchmark,set,54,77,23,861846,1.135
write,memtier,set,77,107,30,960434,0.95100
write,valkey-benchmark,set,107,130,23,869868,1.119
write,memtier,set,130,160,30,962502,0.94300
write,valkey-benchmark,set,160,183,23,869981,1.127
write,memtier,set,183,213,30,954264,0.95900
write,valkey-benchmark,set,213,236,23,871194,1.119
write,memtier,set,236,266,30,947774,0.95100
write,valkey-benchmark,set,266,289,23,876847,1.119
write,memtier,set,289,319,30,947738,0.97500
write,valkey-benchmark,set,319,342,23,864678,1.135
write,memtier,set,342,372,30,948713,0.97500
write,valkey-benchmark,set,372,395,23,855615,1.151
write,memtier,set,395,425,30,954910,0.95100
write,valkey-benchmark,set,425,448,23,876117,1.111
write,memtier,set,448,478,30,952545,0.95900
write,valkey-benchmark,set,478,501,23,870966,1.127
write,memtier,set,501,531,30,952614,0.95900
write,valkey-benchmark,set,531,554,23,866138,1.127
write,memtier,set,554,584,30,949729,0.95900
write,valkey-benchmark,set,584,607,23,862999,1.135
write,memtier,set,607,637,30,951770,0.96700
delete,valkey-benchmark,del,637,652,15,1391982,1.071
delete,memtier,del:set=4:1,652,682,30,1253588,0.76700
delete,valkey-benchmark,del,682,696,14,1430922,0.711
delete,memtier,del:set=4:1,696,726,30,1259875,0.74300
delete,valkey-benchmark,del,726,740,14,1431025,0.711
delete,memtier,del:set=4:1,740,770,30,1260458,0.75100
delete,valkey-benchmark,del,770,784,14,1423488,0.711
delete,memtier,del:set=4:1,784,814,30,1260194,0.75100
delete,valkey-benchmark,del,814,828,14,1417234,0.719
delete,memtier,del:set=4:1,828,858,30,1256627,0.75900
delete,valkey-benchmark,del,858,872,14,1432254,0.703
delete,memtier,del:set=4:1,872,902,30,1267568,0.75100
delete,valkey-benchmark,del,902,916,14,1421363,0.711
delete,memtier,del:set=4:1,916,946,30,1262389,0.75100
delete,valkey-benchmark,del,946,960,14,1418037,0.711
delete,memtier,del:set=4:1,960,990,30,1252272,0.75100
delete,valkey-benchmark,del,990,1004,14,1427756,0.711
delete,memtier,del:set=4:1,1004,1034,30,1260924,0.75100
delete,valkey-benchmark,del,1034,1048,14,1437401,0.703
delete,memtier,del:set=4:1,1048,1078,30,1261986,0.75900
delete,valkey-benchmark,del,1078,1092,14,1436266,0.703
delete,memtier,del:set=4:1,1092,1122,30,1260559,0.75100
delete,valkey-benchmark,del,1122,1136,14,1433897,0.703
delete,memtier,del:set=4:1,1136,1166,30,1261725,0.74300
delete,valkey-benchmark,del,1166,1180,14,1416230,0.711
delete,memtier,del:set=4:1,1180,1210,30,1261968,0.75900
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,21,21,953925,1.039
write,memtier,set,21,51,30,1011178,0.91100
write,valkey-benchmark,set,51,72,21,962603,1.015
write,memtier,set,72,102,30,998095,0.91900
write,valkey-benchmark,set,102,123,21,948137,1.039
write,memtier,set,123,153,30,994661,0.91900
write,valkey-benchmark,set,153,174,21,962001,1.015
write,memtier,set,174,204,30,989722,0.92700
write,valkey-benchmark,set,204,225,21,944778,1.039
write,memtier,set,225,255,30,996591,0.92700
write,valkey-benchmark,set,255,276,21,952971,1.031
write,memtier,set,276,306,30,985659,0.93500
write,valkey-benchmark,set,306,327,21,958405,1.023
write,memtier,set,327,357,30,1002424,0.91100
write,valkey-benchmark,set,357,378,21,958773,1.023
write,memtier,set,378,408,30,995306,0.91900
write,valkey-benchmark,set,408,429,21,950390,1.047
write,memtier,set,429,459,30,986929,0.92700
write,valkey-benchmark,set,459,480,21,957900,1.023
write,memtier,set,480,510,30,986644,0.92700
write,valkey-benchmark,set,510,531,21,950751,1.031
write,memtier,set,531,561,30,989794,0.91900
write,valkey-benchmark,set,561,582,21,950435,1.031
write,memtier,set,582,612,30,991132,0.92700
delete,valkey-benchmark,del,612,626,14,1440300,1.071
delete,memtier,del:set=4:1,626,656,30,1309978,0.74300
delete,valkey-benchmark,del,656,669,13,1507386,0.671
delete,memtier,del:set=4:1,669,699,30,1318131,0.71900
delete,valkey-benchmark,del,699,712,13,1505684,0.671
delete,memtier,del:set=4:1,712,742,30,1329579,0.71100
delete,valkey-benchmark,del,742,756,14,1506138,0.671
delete,memtier,del:set=4:1,756,786,30,1310839,0.72700
delete,valkey-benchmark,del,786,799,13,1506364,0.671
delete,memtier,del:set=4:1,799,829,30,1319814,0.72700
delete,valkey-benchmark,del,829,842,13,1500150,0.671
delete,memtier,del:set=4:1,842,872,30,1315328,0.72700
delete,valkey-benchmark,del,872,885,13,1510802,0.671
delete,memtier,del:set=4:1,885,915,30,1317300,0.72700
delete,valkey-benchmark,del,915,929,14,1517105,0.663
delete,memtier,del:set=4:1,929,959,30,1315295,0.71900
delete,valkey-benchmark,del,959,972,13,1506478,0.671
delete,memtier,del:set=4:1,972,1002,30,1308106,0.73500
delete,valkey-benchmark,del,1002,1015,13,1479399,0.687
delete,memtier,del:set=4:1,1015,1045,30,1311396,0.71900
delete,valkey-benchmark,del,1045,1059,14,1489758,0.679
delete,memtier,del:set=4:1,1059,1089,30,1309597,0.71900
delete,valkey-benchmark,del,1089,1102,13,1499812,0.679
delete,memtier,del:set=4:1,1102,1132,30,1317295,0.72700
delete,valkey-benchmark,del,1132,1146,14,1507727,0.671
delete,memtier,del:set=4:1,1146,1176,30,1313982,0.72700
delete,valkey-benchmark,del,1176,1189,13,1512516,0.671
delete,memtier,del:set=4:1,1189,1219,30,1323333,0.71900
```

