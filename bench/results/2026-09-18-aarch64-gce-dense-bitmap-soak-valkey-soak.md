# challoc Valkey soak, 2026-09-18, aarch64

- commit: 5304cd4
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
| write: valkey-benchmark req/s | 889639 | 855087 | -3.9% |
| write: valkey-benchmark p99 ms | 1.087 | 1.143 | -4.9% |
| write: memtier req/s | 929018 | 912194 | -1.8% |
| write: memtier p99 ms | 0.98300 | 1.02300 | -3.9% |
| delete: valkey-benchmark req/s | 1387341 | 1444188 | +4.1% |
| delete: valkey-benchmark p99 ms | 0.727 | 0.695 | +4.6% |
| delete: memtier req/s | 1210330 | 1259112 | +4.0% |
| delete: memtier p99 ms | 0.78300 | 0.75900 | +3.2% |
| peak RSS MiB (VmHWM) | 578.0 | 572.1 | +1.0% |
| peak sampled used_memory_rss MiB | 578.0 | 571.9 | +1.1% |
| RSS MiB at end of write phase | 577.8 | 571.7 | +1.1% |
| RSS MiB at end of delete phase | 128.8 | 126.3 | +2.0% |
| RSS MiB after settle | 128.8 | 126.3 | +2.0% |
| allocator_frag_ratio after settle | 1.78 | 1.78 | +0.0% |
| active_defrag_hits | 235472 | 237143 | +0.7% |
| keys after settle | 105030 | 101442 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `/src/bench/results/2026-09-18-aarch64-valkey-soak-challoc.csv` (128 rows)
- jemalloc: `/src/bench/results/2026-09-18-aarch64-valkey-soak-jemalloc.csv` (129 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,905305,1.103
write,memtier,set,22,52,30,955719,0.98300
write,valkey-benchmark,set,52,74,22,915961,1.079
write,memtier,set,74,104,30,948673,0.97500
write,valkey-benchmark,set,104,126,22,911743,1.079
write,memtier,set,126,156,30,947987,0.98300
write,valkey-benchmark,set,156,178,22,913284,1.079
write,memtier,set,178,208,30,958111,0.95900
write,valkey-benchmark,set,208,234,26,755173,1.495
write,memtier,set,234,265,31,740365,1.39900
write,valkey-benchmark,set,265,288,23,836540,1.431
write,memtier,set,288,318,30,947364,0.98300
write,valkey-benchmark,set,318,341,23,904445,1.087
write,memtier,set,341,371,30,938586,0.99900
write,valkey-benchmark,set,371,392,21,914202,1.079
write,memtier,set,392,422,30,954428,0.97500
write,valkey-benchmark,set,422,445,23,904200,1.095
write,memtier,set,445,475,30,945326,0.98300
write,valkey-benchmark,set,475,497,22,906659,1.087
write,memtier,set,497,527,30,933763,0.99900
write,valkey-benchmark,set,527,549,22,903179,1.095
write,memtier,set,549,579,30,935263,0.99900
write,valkey-benchmark,set,579,601,22,904977,1.095
write,memtier,set,601,631,30,942635,0.99900
delete,valkey-benchmark,del,631,646,15,1371084,1.135
delete,memtier,del:set=4:1,646,676,30,1208964,0.82300
delete,valkey-benchmark,del,676,690,14,1403115,0.727
delete,memtier,del:set=4:1,690,720,30,1071508,1.02300
delete,valkey-benchmark,del,720,737,17,1155602,0.951
delete,memtier,del:set=4:1,737,767,30,1049028,1.03100
delete,valkey-benchmark,del,767,781,14,1412429,0.719
delete,memtier,del:set=4:1,781,811,30,1234202,0.78300
delete,valkey-benchmark,del,811,826,15,1409344,0.719
delete,memtier,del:set=4:1,826,856,30,1230946,0.78300
delete,valkey-benchmark,del,856,870,14,1398406,0.727
delete,memtier,del:set=4:1,870,900,30,1248270,0.77500
delete,valkey-benchmark,del,900,914,14,1407658,0.727
delete,memtier,del:set=4:1,914,944,30,1247997,0.77500
delete,valkey-benchmark,del,944,958,14,1415829,0.719
delete,memtier,del:set=4:1,958,988,30,1232287,0.78300
delete,valkey-benchmark,del,988,1003,15,1402623,0.727
delete,memtier,del:set=4:1,1003,1033,30,1251519,0.76700
delete,valkey-benchmark,del,1033,1047,14,1415128,0.719
delete,memtier,del:set=4:1,1047,1077,30,1250952,0.76700
delete,valkey-benchmark,del,1077,1091,14,1398015,0.727
delete,memtier,del:set=4:1,1091,1121,30,1233952,0.77500
delete,valkey-benchmark,del,1121,1135,14,1427858,0.711
delete,memtier,del:set=4:1,1135,1165,30,1243409,0.76700
delete,valkey-benchmark,del,1165,1179,14,1418339,0.719
delete,memtier,del:set=4:1,1179,1209,30,1231256,0.78300
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,23,23,865464,1.159
write,memtier,set,23,53,30,944239,1.00700
write,valkey-benchmark,set,53,76,23,865314,1.143
write,memtier,set,76,106,30,877071,1.36700
write,valkey-benchmark,set,106,135,29,680921,1.599
write,memtier,set,135,165,30,779090,1.38300
write,valkey-benchmark,set,165,189,24,866664,1.143
write,memtier,set,189,219,30,930631,1.02300
write,valkey-benchmark,set,219,242,23,868998,1.143
write,memtier,set,242,272,30,908073,1.05500
write,valkey-benchmark,set,272,295,23,864566,1.143
write,memtier,set,295,325,30,920143,1.03900
write,valkey-benchmark,set,325,348,23,872829,1.135
write,memtier,set,348,378,30,916020,1.03900
write,valkey-benchmark,set,378,401,23,859106,1.159
write,memtier,set,401,431,30,917849,1.03900
write,valkey-benchmark,set,431,454,23,868131,1.143
write,memtier,set,454,484,30,928382,1.02300
write,valkey-benchmark,set,484,507,23,883470,1.119
write,memtier,set,507,537,30,933003,1.02300
write,valkey-benchmark,set,537,559,22,883900,1.127
write,memtier,set,559,589,30,941128,1.00700
write,valkey-benchmark,set,589,612,23,881679,1.127
write,memtier,set,612,642,30,950698,0.99100
delete,valkey-benchmark,del,642,656,14,1398895,1.119
delete,memtier,del:set=4:1,656,686,30,1272290,0.78300
delete,valkey-benchmark,del,686,700,14,1450958,0.735
delete,memtier,del:set=4:1,700,730,30,1031829,1.03100
delete,valkey-benchmark,del,730,747,17,1209482,0.903
delete,memtier,del:set=4:1,747,777,30,1188365,0.94300
delete,valkey-benchmark,del,777,790,13,1480604,0.687
delete,memtier,del:set=4:1,790,820,30,1284635,0.75900
delete,valkey-benchmark,del,820,834,14,1465309,0.695
delete,memtier,del:set=4:1,834,864,30,1273921,0.76700
delete,valkey-benchmark,del,864,878,14,1466276,0.695
delete,memtier,del:set=4:1,878,908,30,1290050,0.75900
delete,valkey-benchmark,del,908,921,13,1460387,0.703
delete,memtier,del:set=4:1,921,951,30,1291207,0.75900
delete,valkey-benchmark,del,951,965,14,1468752,0.695
delete,memtier,del:set=4:1,965,995,30,1302689,0.74300
delete,valkey-benchmark,del,995,1009,14,1476669,0.687
delete,memtier,del:set=4:1,1009,1039,30,1278695,0.75100
delete,valkey-benchmark,del,1039,1052,13,1471887,0.695
delete,memtier,del:set=4:1,1052,1082,30,1294163,0.75100
delete,valkey-benchmark,del,1082,1096,14,1474600,0.695
delete,memtier,del:set=4:1,1096,1126,30,1288968,0.75900
delete,valkey-benchmark,del,1126,1139,13,1477214,0.687
delete,memtier,del:set=4:1,1139,1169,30,1286028,0.75900
delete,valkey-benchmark,del,1169,1183,14,1473405,0.695
delete,memtier,del:set=4:1,1183,1213,30,1285621,0.75100
```

