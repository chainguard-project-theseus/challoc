# challoc Valkey soak, 2026-09-21, aarch64

- commit: 63bd7ee
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc-old server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=32c6e5d59aee899a
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=541d0e90fd9fdfe8
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=8c52a44cd725fd45
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup
- occupancy: MEMORY MALLOC-STATS every 20 s under /src/bench/results/2026-09-21-aarch64-valkey-soak-<alloc>-mstats/

## Summary

| measure | challoc-old | challoc | jemalloc | challoc vs jemalloc | challoc-old vs jemalloc |
|---|---:|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 889409 | 878237 | 813997 | -7.3% | -8.5% |
| write: valkey-benchmark p99 ms | 1.103 | 1.111 | 1.215 | -8.6% | -9.2% |
| write: memtier req/s | 914343 | 930342 | 827987 | -11.0% | -9.4% |
| write: memtier p99 ms | 1.03100 | 0.99900 | 1.34300 | -25.6% | -23.2% |
| delete: valkey-benchmark req/s | 1376540 | 1410508 | 1365400 | -3.2% | -0.8% |
| delete: valkey-benchmark p99 ms | 0.735 | 0.719 | 0.735 | -2.2% | +0.0% |
| delete: memtier req/s | 1195485 | 1230463 | 1100946 | -10.5% | -7.9% |
| delete: memtier p99 ms | 0.81500 | 0.77500 | 1.03900 | -25.4% | -21.6% |
| peak RSS MiB (VmHWM) | 576.3 | 577.9 | 562.8 | +2.7% | +2.4% |
| cgroup memory.peak MiB, write phase | - | - | - | - | - |
| cgroup memory.peak MiB, delete phase | - | - | - | - | - |
| cgroup memory.peak MiB, settle phase | - | - | - | - | - |
| peak sampled used_memory_rss MiB | 576.2 | 577.9 | 562.8 | +2.7% | +2.4% |
| RSS MiB at end of write phase | 576.2 | 577.8 | 562.6 | +2.7% | +2.4% |
| RSS MiB at end of delete phase | 128.4 | 127.5 | 126.9 | +0.5% | +1.2% |
| RSS MiB after settle | 128.4 | 127.3 | 126.9 | +0.3% | +1.2% |
| allocator_frag_ratio after settle | 1.79 | 1.78 | 1.75 | +1.7% | +2.3% |
| active_defrag_hits | 233997 | 234498 | 236005 | +0.6% | +0.9% |
| keys after settle | 104857 | 103938 | 103700 | - | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc-old: `/src/bench/results/2026-09-21-aarch64-valkey-soak-challoc-old.csv` (128 rows)
- challoc: `/src/bench/results/2026-09-21-aarch64-valkey-soak-challoc.csv` (128 rows)
- jemalloc: `/src/bench/results/2026-09-21-aarch64-valkey-soak-jemalloc.csv` (130 rows)

## Generator runs

### challoc-old

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,1,23,22,904854,1.119
write,memtier,set,23,53,30,939843,1.02300
write,valkey-benchmark,set,53,75,22,912534,1.095
write,memtier,set,75,105,30,912789,1.26300
write,valkey-benchmark,set,105,133,28,711896,1.535
write,memtier,set,133,163,30,741179,1.43100
write,valkey-benchmark,set,163,185,22,904936,1.103
write,memtier,set,185,215,30,915528,1.03100
write,valkey-benchmark,set,215,237,22,910581,1.095
write,memtier,set,237,267,30,936557,1.01500
write,valkey-benchmark,set,267,289,22,910705,1.095
write,memtier,set,289,319,30,944017,1.01500
write,valkey-benchmark,set,319,341,22,909877,1.095
write,memtier,set,341,371,30,927319,1.03100
write,valkey-benchmark,set,371,393,22,904568,1.103
write,memtier,set,393,423,30,917908,1.03100
write,valkey-benchmark,set,423,445,22,899159,1.111
write,memtier,set,445,475,30,934243,1.02300
write,valkey-benchmark,set,475,498,23,901917,1.103
write,memtier,set,498,528,30,935318,1.03100
write,valkey-benchmark,set,528,550,22,901266,1.103
write,memtier,set,550,580,30,925337,1.03100
write,valkey-benchmark,set,580,602,22,900617,1.111
write,memtier,set,602,632,30,942082,1.01500
delete,valkey-benchmark,del,632,647,15,1370708,1.143
delete,memtier,del:set=4:1,647,677,30,1224670,0.83100
delete,valkey-benchmark,del,677,691,14,1405877,0.735
delete,memtier,del:set=4:1,691,721,30,1061582,1.05500
delete,valkey-benchmark,del,721,738,17,1136557,0.983
delete,memtier,del:set=4:1,738,768,30,1047462,1.04700
delete,valkey-benchmark,del,768,783,15,1406866,0.727
delete,memtier,del:set=4:1,783,813,30,1227343,0.80700
delete,valkey-benchmark,del,813,827,14,1418138,0.727
delete,memtier,del:set=4:1,827,857,30,1233983,0.79900
delete,valkey-benchmark,del,857,871,14,1390917,0.743
delete,memtier,del:set=4:1,871,901,30,1217189,0.80700
delete,valkey-benchmark,del,901,916,15,1395284,0.735
delete,memtier,del:set=4:1,916,946,30,1225347,0.81500
delete,valkey-benchmark,del,946,960,14,1398210,0.735
delete,memtier,del:set=4:1,960,990,30,1218144,0.80700
delete,valkey-benchmark,del,990,1004,14,1393243,0.743
delete,memtier,del:set=4:1,1004,1034,30,1216045,0.81500
delete,valkey-benchmark,del,1034,1049,15,1397136,0.735
delete,memtier,del:set=4:1,1049,1079,30,1223635,0.80700
delete,valkey-benchmark,del,1079,1093,14,1390337,0.743
delete,memtier,del:set=4:1,1093,1123,30,1218213,0.80700
delete,valkey-benchmark,del,1123,1137,14,1404790,0.735
delete,memtier,del:set=4:1,1137,1167,30,1216328,0.81500
delete,valkey-benchmark,del,1167,1182,15,1386962,0.743
delete,memtier,del:set=4:1,1182,1212,30,1211359,0.82300
```

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,1,23,22,895816,1.135
write,memtier,set,23,53,30,953718,0.99100
write,valkey-benchmark,set,53,75,22,901551,1.103
write,memtier,set,75,105,30,921542,1.27100
write,valkey-benchmark,set,105,134,29,704771,1.551
write,memtier,set,134,164,30,769913,1.39100
write,valkey-benchmark,set,164,186,22,894574,1.111
write,memtier,set,186,216,30,956721,0.98300
write,valkey-benchmark,set,216,238,22,896901,1.111
write,memtier,set,238,268,30,943677,1.00700
write,valkey-benchmark,set,268,291,23,895937,1.111
write,memtier,set,291,321,30,937849,0.99900
write,valkey-benchmark,set,321,343,22,892857,1.111
write,memtier,set,343,373,30,935673,1.00700
write,valkey-benchmark,set,373,395,22,891544,1.111
write,memtier,set,395,425,30,957693,0.99900
write,valkey-benchmark,set,425,448,23,887233,1.127
write,memtier,set,448,478,30,935162,1.00700
write,valkey-benchmark,set,478,500,22,893376,1.111
write,memtier,set,500,530,30,953506,0.99900
write,valkey-benchmark,set,530,553,23,893615,1.111
write,memtier,set,553,583,30,949866,1.00700
write,valkey-benchmark,set,583,605,22,890670,1.119
write,memtier,set,605,635,30,948788,0.99900
delete,valkey-benchmark,del,635,650,15,1396746,1.135
delete,memtier,del:set=4:1,650,680,30,1247352,0.80700
delete,valkey-benchmark,del,680,694,14,1447283,0.711
delete,memtier,del:set=4:1,694,724,30,1062126,1.04700
delete,valkey-benchmark,del,724,741,17,1137010,0.975
delete,memtier,del:set=4:1,741,771,30,1089170,1.02300
delete,valkey-benchmark,del,771,785,14,1436988,0.719
delete,memtier,del:set=4:1,785,815,30,1257854,0.78300
delete,valkey-benchmark,del,815,829,14,1438952,0.711
delete,memtier,del:set=4:1,829,859,30,1255725,0.78300
delete,valkey-benchmark,del,859,873,14,1419044,0.727
delete,memtier,del:set=4:1,873,903,30,1260205,0.77500
delete,valkey-benchmark,del,903,917,14,1446759,0.711
delete,memtier,del:set=4:1,917,947,30,1257954,0.76700
delete,valkey-benchmark,del,947,961,14,1444043,0.711
delete,memtier,del:set=4:1,961,991,30,1251949,0.78300
delete,valkey-benchmark,del,991,1005,14,1437608,0.719
delete,memtier,del:set=4:1,1005,1035,30,1268676,0.76700
delete,valkey-benchmark,del,1035,1049,14,1431127,0.719
delete,memtier,del:set=4:1,1049,1079,30,1263117,0.77500
delete,valkey-benchmark,del,1079,1093,14,1419346,0.727
delete,memtier,del:set=4:1,1093,1123,30,1256643,0.77500
delete,valkey-benchmark,del,1123,1137,14,1447492,0.711
delete,memtier,del:set=4:1,1137,1167,30,1267009,0.76700
delete,valkey-benchmark,del,1167,1181,14,1434206,0.719
delete,memtier,del:set=4:1,1181,1211,30,1258237,0.77500
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,1,24,23,837731,1.207
write,memtier,set,24,54,30,863720,1.31100
write,valkey-benchmark,set,54,78,24,845952,1.175
write,memtier,set,78,108,30,800915,1.64700
write,valkey-benchmark,set,108,139,31,655458,1.727
write,memtier,set,139,169,30,707467,1.74300
write,valkey-benchmark,set,169,193,24,825525,1.215
write,memtier,set,193,223,30,841634,1.33500
write,valkey-benchmark,set,223,247,24,820951,1.215
write,memtier,set,247,277,30,847590,1.31100
write,valkey-benchmark,set,277,301,24,834794,1.199
write,memtier,set,301,331,30,837670,1.34300
write,valkey-benchmark,set,331,356,25,823452,1.215
write,memtier,set,356,386,30,843559,1.34300
write,valkey-benchmark,set,386,410,24,825321,1.215
write,memtier,set,410,440,30,842510,1.34300
write,valkey-benchmark,set,440,464,24,822098,1.215
write,memtier,set,464,494,30,844383,1.32700
write,valkey-benchmark,set,494,518,24,832709,1.199
write,memtier,set,518,548,30,840927,1.34300
write,valkey-benchmark,set,548,572,24,829979,1.207
write,memtier,set,572,602,30,837480,1.36700
delete,valkey-benchmark,del,602,617,15,1332090,1.191
delete,memtier,del:set=4:1,617,647,30,1105587,1.07900
delete,valkey-benchmark,del,647,662,15,1379596,0.743
delete,memtier,del:set=4:1,662,692,30,1121783,1.03900
delete,valkey-benchmark,del,692,708,16,1259049,0.967
delete,memtier,del:set=4:1,708,738,30,856525,1.41500
delete,valkey-benchmark,del,738,756,18,1103631,1.015
delete,memtier,del:set=4:1,756,786,30,1092716,1.07100
delete,valkey-benchmark,del,786,800,14,1407162,0.735
delete,memtier,del:set=4:1,800,830,30,1135050,1.00700
delete,valkey-benchmark,del,830,844,14,1408550,0.735
delete,memtier,del:set=4:1,844,874,30,1115677,1.04700
delete,valkey-benchmark,del,874,889,15,1397331,0.735
delete,memtier,del:set=4:1,889,919,30,1125026,1.03900
delete,valkey-benchmark,del,919,933,14,1396746,0.735
delete,memtier,del:set=4:1,933,963,30,1111851,1.05500
delete,valkey-benchmark,del,963,977,14,1404593,0.735
delete,memtier,del:set=4:1,977,1007,30,1134655,1.01500
delete,valkey-benchmark,del,1007,1022,15,1400168,0.735
delete,memtier,del:set=4:1,1022,1052,30,1129360,1.02300
delete,valkey-benchmark,del,1052,1066,14,1408451,0.727
delete,memtier,del:set=4:1,1066,1096,30,1116238,1.05500
delete,valkey-benchmark,del,1096,1110,14,1408153,0.727
delete,memtier,del:set=4:1,1110,1140,30,1118838,1.04700
delete,valkey-benchmark,del,1140,1154,14,1407955,0.727
delete,memtier,del:set=4:1,1154,1184,30,1124316,1.03100
delete,valkey-benchmark,del,1184,1199,15,1402131,0.735
delete,memtier,del:set=4:1,1199,1229,30,1125624,1.03100
```

