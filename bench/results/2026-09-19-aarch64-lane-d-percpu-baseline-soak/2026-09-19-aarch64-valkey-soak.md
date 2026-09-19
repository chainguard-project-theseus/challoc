# challoc Valkey soak, 2026-09-19, aarch64

- commit: 675f357
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=aa1de31696114f97
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=f5ef81748d7c385a
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 938347 | 893210 | -4.8% |
| write: valkey-benchmark p99 ms | 1.047 | 1.103 | -5.1% |
| write: memtier req/s | 960765 | 938768 | -2.3% |
| write: memtier p99 ms | 0.95900 | 0.99900 | -4.0% |
| delete: valkey-benchmark req/s | 1422029 | 1441230 | +1.4% |
| delete: valkey-benchmark p99 ms | 0.719 | 0.703 | +2.3% |
| delete: memtier req/s | 1246858 | 1265327 | +1.5% |
| delete: memtier p99 ms | 0.77500 | 0.76700 | +1.0% |
| peak RSS MiB (VmHWM) | 579.3 | 573.2 | +1.1% |
| peak sampled used_memory_rss MiB | 579.2 | 573.2 | +1.0% |
| RSS MiB at end of write phase | 579.2 | 573.1 | +1.1% |
| RSS MiB at end of delete phase | 130.9 | 129.8 | +0.8% |
| RSS MiB after settle | 130.9 | 129.8 | +0.8% |
| allocator_frag_ratio after settle | 1.81 | 1.80 | +0.6% |
| active_defrag_hits | 247337 | 255712 | +3.4% |
| keys after settle | 105949 | 103443 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-soak/2026-09-19-aarch64-valkey-soak-challoc.csv` (131 rows)
- jemalloc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-soak/2026-09-19-aarch64-valkey-soak-jemalloc.csv` (127 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,910954,1.103
write,memtier,set,22,52,30,949192,0.97500
write,valkey-benchmark,set,52,74,22,931359,1.047
write,memtier,set,74,104,30,963524,0.95900
write,valkey-benchmark,set,104,125,21,944242,1.039
write,memtier,set,125,155,30,962805,0.95900
write,valkey-benchmark,set,155,176,21,946432,1.039
write,memtier,set,176,206,30,972732,0.95100
write,valkey-benchmark,set,206,227,21,940557,1.047
write,memtier,set,227,257,30,947855,0.97500
write,valkey-benchmark,set,257,279,22,928591,1.055
write,memtier,set,279,309,30,952723,0.95900
write,valkey-benchmark,set,309,330,21,928031,1.063
write,memtier,set,330,360,30,962540,0.96700
write,valkey-benchmark,set,360,382,22,935935,1.047
write,memtier,set,382,412,30,957951,0.96700
write,valkey-benchmark,set,412,433,21,950254,1.031
write,memtier,set,433,463,30,966832,0.95900
write,valkey-benchmark,set,463,484,21,948767,1.039
write,memtier,set,484,514,30,964289,0.95100
write,valkey-benchmark,set,514,535,21,953153,1.031
write,memtier,set,535,565,30,964786,0.95900
write,valkey-benchmark,set,565,586,21,941886,1.047
write,memtier,set,586,616,30,963946,0.95900
delete,valkey-benchmark,del,616,631,15,1377031,1.111
delete,memtier,del:set=4:1,631,661,30,1228555,0.80700
delete,valkey-benchmark,del,661,675,14,1431127,0.711
delete,memtier,del:set=4:1,675,705,30,1249970,0.77500
delete,valkey-benchmark,del,705,719,14,1438021,0.703
delete,memtier,del:set=4:1,719,749,30,1254976,0.76700
delete,valkey-benchmark,del,749,763,14,1428571,0.711
delete,memtier,del:set=4:1,763,793,30,1249262,0.76700
delete,valkey-benchmark,del,793,807,14,1421565,0.719
delete,memtier,del:set=4:1,807,837,30,1250180,0.77500
delete,valkey-benchmark,del,837,851,14,1426432,0.719
delete,memtier,del:set=4:1,851,881,30,1243815,0.77500
delete,valkey-benchmark,del,881,895,14,1416832,0.719
delete,memtier,del:set=4:1,895,925,30,1248671,0.77500
delete,valkey-benchmark,del,925,939,14,1415829,0.719
delete,memtier,del:set=4:1,939,969,30,1245554,0.77500
delete,valkey-benchmark,del,969,983,14,1435338,0.711
delete,memtier,del:set=4:1,983,1013,30,1249690,0.78300
delete,valkey-benchmark,del,1013,1027,14,1428265,0.711
delete,memtier,del:set=4:1,1027,1057,30,1247670,0.76700
delete,valkey-benchmark,del,1057,1071,14,1434617,0.711
delete,memtier,del:set=4:1,1071,1101,30,1250367,0.77500
delete,valkey-benchmark,del,1101,1115,14,1420757,0.719
delete,memtier,del:set=4:1,1115,1145,30,1252986,0.76700
delete,valkey-benchmark,del,1145,1159,14,1427348,0.719
delete,memtier,del:set=4:1,1159,1189,30,1243707,0.77500
delete,valkey-benchmark,del,1189,1203,14,1406668,0.727
delete,memtier,del:set=4:1,1203,1233,30,1240610,0.77500
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,900495,1.111
write,memtier,set,22,52,30,955636,0.97500
write,valkey-benchmark,set,52,74,22,907441,1.087
write,memtier,set,74,104,30,938125,1.00700
write,valkey-benchmark,set,104,127,23,892578,1.127
write,memtier,set,127,157,30,938120,0.99900
write,valkey-benchmark,set,157,180,23,875848,1.127
write,memtier,set,180,210,30,936436,0.99900
write,valkey-benchmark,set,210,232,22,898553,1.095
write,memtier,set,232,262,30,934565,0.99900
write,valkey-benchmark,set,262,285,23,888376,1.103
write,memtier,set,285,315,30,936174,0.99900
write,valkey-benchmark,set,315,337,22,894494,1.103
write,memtier,set,337,367,30,932293,0.99900
write,valkey-benchmark,set,367,389,22,896982,1.095
write,memtier,set,389,419,30,941895,0.98300
write,valkey-benchmark,set,419,442,23,883665,1.111
write,memtier,set,442,472,30,931735,0.99900
write,valkey-benchmark,set,472,495,23,882613,1.111
write,memtier,set,495,525,30,938013,0.99900
write,valkey-benchmark,set,525,547,22,897989,1.095
write,memtier,set,547,577,30,942379,0.99100
write,valkey-benchmark,set,577,599,22,899483,1.095
write,memtier,set,599,629,30,939847,0.98300
delete,valkey-benchmark,del,629,644,15,1387732,1.119
delete,memtier,del:set=4:1,644,674,30,1250657,0.79100
delete,valkey-benchmark,del,674,688,14,1433897,0.711
delete,memtier,del:set=4:1,688,718,30,1261689,0.77500
delete,valkey-benchmark,del,718,732,14,1431639,0.711
delete,memtier,del:set=4:1,732,762,30,1274237,0.75900
delete,valkey-benchmark,del,762,775,13,1444878,0.703
delete,memtier,del:set=4:1,775,805,30,1259472,0.77500
delete,valkey-benchmark,del,805,819,14,1458470,0.695
delete,memtier,del:set=4:1,819,849,30,1265002,0.76700
delete,valkey-benchmark,del,849,863,14,1451906,0.703
delete,memtier,del:set=4:1,863,893,30,1265268,0.76700
delete,valkey-benchmark,del,893,907,14,1450221,0.703
delete,memtier,del:set=4:1,907,937,30,1265158,0.77500
delete,valkey-benchmark,del,937,951,14,1448646,0.703
delete,memtier,del:set=4:1,951,981,30,1274530,0.75900
delete,valkey-benchmark,del,981,994,13,1457089,0.695
delete,memtier,del:set=4:1,994,1024,30,1265246,0.76700
delete,valkey-benchmark,del,1024,1038,14,1448226,0.703
delete,memtier,del:set=4:1,1038,1068,30,1272487,0.75900
delete,valkey-benchmark,del,1068,1082,14,1446655,0.703
delete,memtier,del:set=4:1,1082,1112,30,1265537,0.75900
delete,valkey-benchmark,del,1112,1126,14,1445714,0.703
delete,memtier,del:set=4:1,1126,1156,30,1262512,0.76700
delete,valkey-benchmark,del,1156,1170,14,1430922,0.719
delete,memtier,del:set=4:1,1170,1200,30,1267457,0.77500
```

