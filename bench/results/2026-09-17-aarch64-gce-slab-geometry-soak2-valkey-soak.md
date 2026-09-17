# challoc Valkey soak, 2026-09-17, aarch64

- commit: e6b89c3
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
| write: valkey-benchmark req/s | 882514 | 944708 | +7.0% |
| write: valkey-benchmark p99 ms | 1.111 | 1.039 | +6.9% |
| write: memtier req/s | 940021 | 985322 | +4.8% |
| write: memtier p99 ms | 0.98300 | 0.92700 | +6.0% |
| delete: valkey-benchmark req/s | 1415585 | 1493155 | +5.5% |
| delete: valkey-benchmark p99 ms | 0.711 | 0.679 | +4.7% |
| delete: memtier req/s | 1242634 | 1316732 | +6.0% |
| delete: memtier p99 ms | 0.77500 | 0.71900 | +7.8% |
| peak RSS MiB (VmHWM) | 578.3 | 577.2 | +0.2% |
| peak sampled used_memory_rss MiB | 578.3 | 577.2 | +0.2% |
| RSS MiB at end of write phase | 578.1 | 576.9 | +0.2% |
| RSS MiB at end of delete phase | 128.6 | 130.5 | -1.5% |
| RSS MiB after settle | 128.6 | 130.5 | -1.5% |
| allocator_frag_ratio after settle | 1.78 | 1.81 | -1.7% |
| active_defrag_hits | 253993 | 268829 | +5.8% |
| keys after settle | 104069 | 103650 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `/src/target/soak-it15b/2026-09-17-aarch64-valkey-soak-challoc.csv` (128 rows)
- jemalloc: `/src/target/soak-it15b/2026-09-17-aarch64-valkey-soak-jemalloc.csv` (130 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,882924,1.127
write,memtier,set,22,52,30,935261,0.99900
write,valkey-benchmark,set,52,75,23,891902,1.103
write,memtier,set,75,105,30,938235,0.98300
write,valkey-benchmark,set,105,127,22,887430,1.111
write,memtier,set,127,157,30,939364,0.99100
write,valkey-benchmark,set,157,180,23,879430,1.119
write,memtier,set,180,210,30,943274,0.96700
write,valkey-benchmark,set,210,234,24,835841,1.199
write,memtier,set,234,264,30,943160,0.96700
write,valkey-benchmark,set,264,287,23,881096,1.111
write,memtier,set,287,317,30,946527,0.97500
write,valkey-benchmark,set,317,339,22,894534,1.095
write,memtier,set,339,369,30,938497,0.98300
write,valkey-benchmark,set,369,392,23,894254,1.095
write,memtier,set,392,422,30,936161,0.98300
write,valkey-benchmark,set,422,444,22,894855,1.095
write,memtier,set,444,474,30,940322,0.97500
write,valkey-benchmark,set,474,497,23,885034,1.111
write,memtier,set,497,527,30,933714,1.00700
write,valkey-benchmark,set,527,549,22,878387,1.119
write,memtier,set,549,579,30,943865,0.97500
write,valkey-benchmark,set,579,602,23,884486,1.111
write,memtier,set,602,632,30,941870,0.98300
delete,valkey-benchmark,del,632,647,15,1362676,1.095
delete,memtier,del:set=4:1,647,677,30,1233864,0.77500
delete,valkey-benchmark,del,677,691,14,1424907,0.711
delete,memtier,del:set=4:1,691,721,30,1239719,0.77500
delete,valkey-benchmark,del,721,735,14,1421565,0.711
delete,memtier,del:set=4:1,735,765,30,1249531,0.76700
delete,valkey-benchmark,del,765,779,14,1424400,0.711
delete,memtier,del:set=4:1,779,809,30,1246757,0.77500
delete,valkey-benchmark,del,809,823,14,1401149,0.727
delete,memtier,del:set=4:1,823,853,30,1239768,0.76700
delete,valkey-benchmark,del,853,867,14,1423184,0.711
delete,memtier,del:set=4:1,867,897,30,1242567,0.78300
delete,valkey-benchmark,del,897,911,14,1420757,0.711
delete,memtier,del:set=4:1,911,941,30,1250068,0.76700
delete,valkey-benchmark,del,941,956,15,1407262,0.719
delete,memtier,del:set=4:1,956,986,30,1248508,0.76700
delete,valkey-benchmark,del,986,1000,14,1413827,0.719
delete,memtier,del:set=4:1,1000,1030,30,1236629,0.78300
delete,valkey-benchmark,del,1030,1044,14,1416030,0.719
delete,memtier,del:set=4:1,1044,1074,30,1241389,0.78300
delete,valkey-benchmark,del,1074,1088,14,1430001,0.711
delete,memtier,del:set=4:1,1088,1118,30,1240238,0.78300
delete,valkey-benchmark,del,1118,1132,14,1431025,0.711
delete,memtier,del:set=4:1,1132,1162,30,1237961,0.76700
delete,valkey-benchmark,del,1162,1176,14,1425822,0.711
delete,memtier,del:set=4:1,1176,1206,30,1247248,0.77500
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,21,21,948767,1.039
write,memtier,set,21,51,30,991961,0.92700
write,valkey-benchmark,set,51,72,21,950932,1.031
write,memtier,set,72,102,30,989353,0.92700
write,valkey-benchmark,set,102,123,21,953107,1.023
write,memtier,set,123,153,30,986047,0.92700
write,valkey-benchmark,set,153,175,22,941841,1.047
write,memtier,set,175,205,30,988129,0.92700
write,valkey-benchmark,set,205,226,21,933315,1.055
write,memtier,set,226,256,30,979613,0.93500
write,valkey-benchmark,set,256,277,21,952880,1.023
write,memtier,set,277,307,30,985766,0.92700
write,valkey-benchmark,set,307,328,21,946611,1.039
write,memtier,set,328,358,30,983106,0.93500
write,valkey-benchmark,set,358,379,21,939452,1.039
write,memtier,set,379,409,30,986363,0.92700
write,valkey-benchmark,set,410,431,21,942463,1.039
write,memtier,set,431,461,30,977356,0.93500
write,valkey-benchmark,set,461,482,21,941974,1.039
write,memtier,set,482,512,30,982214,0.93500
write,valkey-benchmark,set,512,533,21,943886,1.039
write,memtier,set,533,563,30,987654,0.92700
write,valkey-benchmark,set,563,584,21,941265,1.047
write,memtier,set,584,614,30,986305,0.93500
delete,valkey-benchmark,del,614,628,14,1429695,1.079
delete,memtier,del:set=4:1,628,658,30,1297480,0.75900
delete,valkey-benchmark,del,658,672,14,1497566,0.679
delete,memtier,del:set=4:1,672,702,30,1312263,0.71900
delete,valkey-benchmark,del,702,715,13,1502291,0.671
delete,memtier,del:set=4:1,715,745,30,1313753,0.71900
delete,valkey-benchmark,del,745,759,14,1502742,0.679
delete,memtier,del:set=4:1,759,789,30,1320298,0.71900
delete,valkey-benchmark,del,789,802,13,1501840,0.679
delete,memtier,del:set=4:1,802,832,30,1318267,0.71900
delete,valkey-benchmark,del,832,845,13,1484230,0.687
delete,memtier,del:set=4:1,845,875,30,1324644,0.71100
delete,valkey-benchmark,del,875,889,14,1493652,0.679
delete,memtier,del:set=4:1,889,919,30,1323002,0.71100
delete,valkey-benchmark,del,919,932,13,1496334,0.679
delete,memtier,del:set=4:1,932,962,30,1318727,0.71100
delete,valkey-benchmark,del,962,975,13,1502968,0.671
delete,memtier,del:set=4:1,975,1005,30,1317557,0.71900
delete,valkey-benchmark,del,1005,1019,14,1501389,0.671
delete,memtier,del:set=4:1,1019,1049,30,1320945,0.72700
delete,valkey-benchmark,del,1049,1062,13,1507273,0.671
delete,memtier,del:set=4:1,1062,1092,30,1319238,0.72700
delete,valkey-benchmark,del,1092,1105,13,1506818,0.671
delete,memtier,del:set=4:1,1105,1135,30,1315035,0.71900
delete,valkey-benchmark,del,1135,1149,14,1495998,0.679
delete,memtier,del:set=4:1,1149,1179,30,1316711,0.71900
delete,valkey-benchmark,del,1179,1192,13,1481372,0.687
delete,memtier,del:set=4:1,1192,1222,30,1316335,0.72700
```

