# challoc Valkey soak, 2026-09-17, aarch64

- commit: 8408d72
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=29c2469f316c2d7b
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=c645b848879441b2
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; INFO sampled every 10 s
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 814534 | 876850 | +7.7% |
| write: valkey-benchmark p99 ms | 1.207 | 1.119 | +7.9% |
| write: memtier req/s | 906754 | 927095 | +2.2% |
| write: memtier p99 ms | 1.03900 | 1.01500 | +2.4% |
| delete: valkey-benchmark req/s | 1396823 | 1459053 | +4.5% |
| delete: valkey-benchmark p99 ms | 0.727 | 0.695 | +4.6% |
| delete: memtier req/s | 1225542 | 1278033 | +4.3% |
| delete: memtier p99 ms | 0.78300 | 0.75900 | +3.2% |
| peak RSS MiB (VmHWM) | 575.9 | 572.2 | +0.6% |
| peak sampled used_memory_rss MiB | 575.9 | 572.1 | +0.7% |
| RSS MiB at end of write phase | 575.6 | 571.8 | +0.7% |
| RSS MiB at end of delete phase | 125.6 | 126.6 | -0.8% |
| RSS MiB after settle | 125.6 | 126.4 | -0.6% |
| allocator_frag_ratio after settle | 1.75 | 1.77 | -1.1% |
| active_defrag_hits | 238407 | 234855 | -1.5% |
| keys after settle | 102703 | 101809 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `/src/bench/results/2026-09-17-aarch64-gce-narenas-default-valkey-soak-challoc.csv` (129 rows)
- jemalloc: `/src/bench/results/2026-09-17-aarch64-gce-narenas-default-valkey-soak-jemalloc.csv` (128 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,24,24,818364,1.207
write,memtier,set,24,54,30,923344,1.01500
write,valkey-benchmark,set,54,79,25,817795,1.207
write,memtier,set,79,109,30,916712,1.01500
write,valkey-benchmark,set,109,133,24,822774,1.191
write,memtier,set,133,163,30,891980,1.04700
write,valkey-benchmark,set,163,188,25,819504,1.191
write,memtier,set,188,218,30,906165,1.03100
write,valkey-benchmark,set,218,242,24,811622,1.215
write,memtier,set,242,272,30,923838,0.99900
write,valkey-benchmark,set,272,297,25,812117,1.207
write,memtier,set,297,327,30,901841,1.03900
write,valkey-benchmark,set,327,352,25,810340,1.207
write,memtier,set,352,382,30,895411,1.04700
write,valkey-benchmark,set,382,406,24,806452,1.215
write,memtier,set,406,437,31,901367,1.03900
write,valkey-benchmark,set,437,461,24,813935,1.207
write,memtier,set,461,491,30,920712,1.00700
write,valkey-benchmark,set,491,516,25,816860,1.199
write,memtier,set,516,546,30,899443,1.03900
write,valkey-benchmark,set,546,570,24,810110,1.207
write,memtier,set,570,600,30,893481,1.04700
delete,valkey-benchmark,del,600,615,15,1360452,1.143
delete,memtier,del:set=4:1,615,645,30,1207483,0.81500
delete,valkey-benchmark,del,645,659,14,1392079,0.735
delete,memtier,del:set=4:1,659,689,30,1217560,0.79100
delete,valkey-benchmark,del,689,704,15,1397234,0.727
delete,memtier,del:set=4:1,704,734,30,1219744,0.79100
delete,valkey-benchmark,del,734,748,14,1403410,0.727
delete,memtier,del:set=4:1,748,778,30,1224704,0.79100
delete,valkey-benchmark,del,778,792,14,1398895,0.727
delete,memtier,del:set=4:1,792,822,30,1216212,0.79100
delete,valkey-benchmark,del,822,837,15,1405481,0.719
delete,memtier,del:set=4:1,837,867,30,1229861,0.78300
delete,valkey-benchmark,del,867,881,14,1402328,0.727
delete,memtier,del:set=4:1,881,911,30,1240127,0.77500
delete,valkey-benchmark,del,911,925,14,1410835,0.719
delete,memtier,del:set=4:1,925,955,30,1221716,0.79900
delete,valkey-benchmark,del,955,969,14,1411034,0.719
delete,memtier,del:set=4:1,969,999,30,1233264,0.78300
delete,valkey-benchmark,del,999,1014,15,1408451,0.719
delete,memtier,del:set=4:1,1014,1044,30,1234470,0.78300
delete,valkey-benchmark,del,1044,1058,14,1389275,0.735
delete,memtier,del:set=4:1,1058,1088,30,1218223,0.79100
delete,valkey-benchmark,del,1088,1102,14,1400854,0.727
delete,memtier,del:set=4:1,1102,1132,30,1241490,0.77500
delete,valkey-benchmark,del,1132,1147,15,1390530,0.735
delete,memtier,del:set=4:1,1147,1177,30,1227904,0.78300
delete,valkey-benchmark,del,1177,1191,14,1384658,0.735
delete,memtier,del:set=4:1,1191,1221,30,1224832,0.78300
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,890234,1.103
write,memtier,set,22,52,30,944297,1.01500
write,valkey-benchmark,set,52,75,23,878619,1.119
write,memtier,set,75,105,30,928351,1.01500
write,valkey-benchmark,set,105,128,23,877501,1.119
write,memtier,set,128,158,30,943699,0.99900
write,valkey-benchmark,set,158,181,23,877001,1.119
write,memtier,set,181,211,30,923383,1.03100
write,valkey-benchmark,set,211,233,22,879662,1.119
write,memtier,set,233,263,30,917821,1.03100
write,valkey-benchmark,set,263,286,23,884838,1.111
write,memtier,set,286,316,30,925890,1.02300
write,valkey-benchmark,set,316,339,23,878349,1.119
write,memtier,set,339,369,30,924711,1.01500
write,valkey-benchmark,set,369,392,23,876885,1.111
write,memtier,set,392,422,30,917755,1.04700
write,valkey-benchmark,set,422,445,23,864940,1.143
write,memtier,set,445,475,30,904331,1.04700
write,valkey-benchmark,set,475,498,23,875350,1.127
write,memtier,set,498,528,30,932923,1.00700
write,valkey-benchmark,set,528,551,23,864230,1.143
write,memtier,set,551,581,30,935877,0.99900
write,valkey-benchmark,set,581,604,23,874585,1.127
write,memtier,set,604,634,30,926102,1.02300
delete,valkey-benchmark,del,634,648,14,1378835,1.135
delete,memtier,del:set=4:1,648,678,30,1260340,0.79900
delete,valkey-benchmark,del,678,692,14,1473622,0.687
delete,memtier,del:set=4:1,692,722,30,1283637,0.75900
delete,valkey-benchmark,del,722,735,13,1477760,0.687
delete,memtier,del:set=4:1,735,765,30,1281543,0.75900
delete,valkey-benchmark,del,765,779,14,1468213,0.695
delete,memtier,del:set=4:1,779,809,30,1265321,0.76700
delete,valkey-benchmark,del,809,822,13,1479618,0.687
delete,memtier,del:set=4:1,822,852,30,1282657,0.75900
delete,valkey-benchmark,del,852,866,14,1441026,0.711
delete,memtier,del:set=4:1,866,896,30,1280904,0.76700
delete,valkey-benchmark,del,896,910,14,1471129,0.695
delete,memtier,del:set=4:1,910,940,30,1277200,0.75900
delete,valkey-benchmark,del,940,954,14,1476015,0.687
delete,memtier,del:set=4:1,954,984,30,1281206,0.75900
delete,valkey-benchmark,del,984,997,13,1455075,0.703
delete,memtier,del:set=4:1,997,1027,30,1285999,0.75900
delete,valkey-benchmark,del,1027,1041,14,1462737,0.695
delete,memtier,del:set=4:1,1041,1071,30,1288209,0.75900
delete,valkey-benchmark,del,1071,1085,14,1456346,0.703
delete,memtier,del:set=4:1,1085,1115,30,1272701,0.76700
delete,valkey-benchmark,del,1115,1129,14,1460387,0.703
delete,memtier,del:set=4:1,1129,1159,30,1277081,0.77500
delete,valkey-benchmark,del,1159,1172,13,1466921,0.695
delete,memtier,del:set=4:1,1172,1202,30,1277627,0.76700
```

