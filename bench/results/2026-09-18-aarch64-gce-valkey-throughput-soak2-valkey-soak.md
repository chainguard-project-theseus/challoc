# challoc Valkey soak, 2026-09-18, aarch64

- commit: c18979d; the challoc half ran the uncommitted `usable_size` variant of task 4d (region and slot-start validation, no descriptor bit test), measured and reverted; see `2026-09-18-aarch64-gce-valkey-throughput.md`
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=29c2469f316c2d7b
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=c645b848879441b2
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; INFO sampled every 10 s
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- occupancy: MEMORY MALLOC-STATS every 20 s under target/valkey/soak-4d-run2/2026-09-18-aarch64-valkey-soak-<alloc>-mstats/

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 788364 | 858890 | +8.9% |
| write: valkey-benchmark p99 ms | 1.231 | 1.127 | +9.2% |
| write: memtier req/s | 880783 | 907860 | +3.1% |
| write: memtier p99 ms | 1.07100 | 1.03900 | +3.1% |
| delete: valkey-benchmark req/s | 1361444 | 1436033 | +5.5% |
| delete: valkey-benchmark p99 ms | 0.735 | 0.703 | +4.6% |
| delete: memtier req/s | 1207960 | 1240007 | +2.7% |
| delete: memtier p99 ms | 0.79100 | 0.77500 | +2.1% |
| peak RSS MiB (VmHWM) | 573.9 | 571.4 | +0.4% |
| peak sampled used_memory_rss MiB | 573.9 | 571.4 | +0.4% |
| RSS MiB at end of write phase | 573.6 | 571.2 | +0.4% |
| RSS MiB at end of delete phase | 123.4 | 126.5 | -2.5% |
| RSS MiB after settle | 123.3 | 126.4 | -2.5% |
| allocator_frag_ratio after settle | 1.74 | 1.77 | -1.7% |
| active_defrag_hits | 233438 | 233596 | +0.1% |
| keys after settle | 101400 | 101852 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-18-aarch64-gce-valkey-throughput-soak2-valkey-soak-challoc.csv` (131 rows)
- jemalloc: `bench/results/2026-09-18-aarch64-gce-valkey-throughput-soak2-valkey-soak-jemalloc.csv` (129 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,25,25,802407,1.255
write,memtier,set,25,55,30,903112,1.05500
write,valkey-benchmark,set,55,80,25,808081,1.223
write,memtier,set,80,110,30,848475,1.42300
write,valkey-benchmark,set,110,141,31,639304,1.751
write,memtier,set,141,171,30,777188,1.40700
write,valkey-benchmark,set,171,196,25,805477,1.223
write,memtier,set,196,226,30,903609,1.04700
write,valkey-benchmark,set,226,251,25,807298,1.223
write,memtier,set,251,281,30,903228,1.05500
write,valkey-benchmark,set,281,306,25,803245,1.231
write,memtier,set,306,336,30,889637,1.07100
write,valkey-benchmark,set,336,361,25,802600,1.231
write,memtier,set,361,391,30,888820,1.07100
write,valkey-benchmark,set,391,416,25,800608,1.231
write,memtier,set,416,446,30,905714,1.05500
write,valkey-benchmark,set,446,471,25,801539,1.231
write,memtier,set,471,501,30,898071,1.05500
write,valkey-benchmark,set,501,526,25,802311,1.231
write,memtier,set,526,556,30,891322,1.07100
write,valkey-benchmark,set,556,581,25,799137,1.239
write,memtier,set,581,611,30,879441,1.08700
delete,valkey-benchmark,del,611,626,15,1356760,1.151
delete,memtier,del:set=4:1,626,656,30,1212532,0.81500
delete,valkey-benchmark,del,656,670,14,1389564,0.735
delete,memtier,del:set=4:1,670,700,30,1211973,0.81500
delete,valkey-benchmark,del,700,718,18,1141422,0.967
delete,memtier,del:set=4:1,718,748,30,963846,1.08700
delete,valkey-benchmark,del,748,764,16,1188142,0.967
delete,memtier,del:set=4:1,764,795,31,1226691,0.79900
delete,valkey-benchmark,del,795,809,14,1399874,0.727
delete,memtier,del:set=4:1,809,839,30,1234321,0.79100
delete,valkey-benchmark,del,839,853,14,1384370,0.743
delete,memtier,del:set=4:1,853,883,30,1224117,0.79100
delete,valkey-benchmark,del,883,898,15,1400462,0.727
delete,memtier,del:set=4:1,898,928,30,1222010,0.79900
delete,valkey-benchmark,del,928,942,14,1395576,0.735
delete,memtier,del:set=4:1,942,972,30,1236061,0.79100
delete,valkey-benchmark,del,972,986,14,1404198,0.727
delete,memtier,del:set=4:1,986,1016,30,1223240,0.79900
delete,valkey-benchmark,del,1016,1031,15,1391788,0.735
delete,memtier,del:set=4:1,1031,1061,30,1233701,0.79100
delete,valkey-benchmark,del,1061,1075,14,1401836,0.727
delete,memtier,del:set=4:1,1075,1105,30,1233066,0.79100
delete,valkey-benchmark,del,1105,1119,14,1403016,0.727
delete,memtier,del:set=4:1,1119,1149,30,1231015,0.78300
delete,valkey-benchmark,del,1149,1163,14,1405975,0.727
delete,memtier,del:set=4:1,1163,1193,30,1222499,0.80700
delete,valkey-benchmark,del,1193,1208,15,1397234,0.735
delete,memtier,del:set=4:1,1208,1238,30,1236370,0.79100
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,23,23,881018,1.119
write,memtier,set,23,53,30,934090,1.01500
write,valkey-benchmark,set,53,75,22,886682,1.103
write,memtier,set,75,105,30,893340,1.31900
write,valkey-benchmark,set,105,134,29,692665,1.567
write,memtier,set,134,164,30,762253,1.39900
write,valkey-benchmark,set,164,187,23,872106,1.127
write,memtier,set,187,217,30,909373,1.04700
write,valkey-benchmark,set,217,240,23,869716,1.127
write,memtier,set,240,270,30,917472,1.03900
write,valkey-benchmark,set,270,293,23,865351,1.143
write,memtier,set,293,323,30,937837,1.00700
write,valkey-benchmark,set,323,346,23,871802,1.127
write,memtier,set,346,376,30,921401,1.03100
write,valkey-benchmark,set,376,399,23,879624,1.111
write,memtier,set,399,429,30,934295,1.01500
write,valkey-benchmark,set,429,452,23,871270,1.119
write,memtier,set,452,482,30,915828,1.03900
write,valkey-benchmark,set,482,505,23,873782,1.119
write,memtier,set,505,535,30,913324,1.03900
write,valkey-benchmark,set,535,558,23,869301,1.135
write,memtier,set,558,588,30,938654,1.00700
write,valkey-benchmark,set,588,611,23,873362,1.127
write,memtier,set,611,641,30,916455,1.03900
delete,valkey-benchmark,del,641,655,14,1377316,1.135
delete,memtier,del:set=4:1,656,686,30,1242738,0.81500
delete,valkey-benchmark,del,686,699,13,1461988,0.695
delete,memtier,del:set=4:1,699,729,30,1029536,1.03100
delete,valkey-benchmark,del,729,746,17,1217063,0.903
delete,memtier,del:set=4:1,746,776,30,1146967,0.98300
delete,valkey-benchmark,del,776,789,13,1459215,0.703
delete,memtier,del:set=4:1,789,819,30,1271695,0.76700
delete,valkey-benchmark,del,819,833,14,1458576,0.703
delete,memtier,del:set=4:1,833,863,30,1274451,0.77500
delete,valkey-benchmark,del,863,877,14,1460814,0.703
delete,memtier,del:set=4:1,877,907,30,1272793,0.76700
delete,valkey-benchmark,del,907,921,14,1465202,0.695
delete,memtier,del:set=4:1,921,951,30,1266208,0.77500
delete,valkey-benchmark,del,951,964,13,1464987,0.695
delete,memtier,del:set=4:1,964,994,30,1271307,0.76700
delete,valkey-benchmark,del,994,1008,14,1464880,0.695
delete,memtier,del:set=4:1,1008,1038,30,1271381,0.76700
delete,valkey-benchmark,del,1038,1052,14,1456452,0.703
delete,memtier,del:set=4:1,1052,1082,30,1265217,0.77500
delete,valkey-benchmark,del,1082,1095,13,1464343,0.695
delete,memtier,del:set=4:1,1095,1125,30,1266850,0.77500
delete,valkey-benchmark,del,1125,1139,14,1454969,0.703
delete,memtier,del:set=4:1,1139,1169,30,1268510,0.76700
delete,valkey-benchmark,del,1169,1183,14,1462630,0.695
delete,memtier,del:set=4:1,1183,1213,30,1272434,0.76700
```

