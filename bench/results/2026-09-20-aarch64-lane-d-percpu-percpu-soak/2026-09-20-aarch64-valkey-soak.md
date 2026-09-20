# challoc Valkey soak, 2026-09-20, aarch64

- commit: 0c60b1a
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=a5266dd2ce50b7a7
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=44c59d225740278a
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- challoc server MALLOC_CONF: percpu_arena:percpu (CHALLOC_CONF; the jemalloc server runs stock)
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak reset per phase through /sys/fs/cgroup/system.slice/docker-9af91a3f77c0859693acc78bd124831d5c942c261896e81fb4087894ce448aeb.scope/memory.peak (kernel 7.0.0-1011-gcp, bench/probes/cgpeak.c); the cgroup peak counts every process of the container and its page cache (memory.stat file, given per phase below), VmHWM the server alone
  - challoc: cgroup page cache (memory.stat file) at the end of the write / delete / settle phases: 304.0 / 304.0 / 304.0 MiB
  - jemalloc: cgroup page cache (memory.stat file) at the end of the write / delete / settle phases: 304.1 / 304.2 / 304.2 MiB

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 920471 | 908954 | -1.3% |
| write: valkey-benchmark p99 ms | 1.071 | 1.079 | -0.7% |
| write: memtier req/s | 924892 | 953954 | +3.1% |
| write: memtier p99 ms | 1.10300 | 0.96700 | +14.1% |
| delete: valkey-benchmark req/s | 1389863 | 1453507 | +4.6% |
| delete: valkey-benchmark p99 ms | 0.727 | 0.695 | +4.6% |
| delete: memtier req/s | 1153875 | 1281876 | +11.1% |
| delete: memtier p99 ms | 0.96700 | 0.74300 | +30.1% |
| peak RSS MiB (VmHWM) | 575.2 | 574.1 | +0.2% |
| cgroup memory.peak MiB, write phase | 915.4 | 912.1 | +0.4% |
| cgroup memory.peak MiB, delete phase | 895.6 | 893.5 | +0.2% |
| cgroup memory.peak MiB, settle phase | 452.2 | 448.8 | +0.8% |
| peak sampled used_memory_rss MiB | 575.1 | 574.1 | +0.2% |
| RSS MiB at end of write phase | 575.1 | 573.7 | +0.2% |
| RSS MiB at end of delete phase | 131.8 | 129.9 | +1.5% |
| RSS MiB after settle | 131.8 | 129.9 | +1.5% |
| allocator_frag_ratio after settle | 1.78 | 1.81 | -1.7% |
| active_defrag_hits | 248404 | 242153 | -2.5% |
| keys after settle | 108899 | 103085 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-soak/2026-09-20-aarch64-valkey-soak-challoc.csv` (132 rows)
- jemalloc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-soak/2026-09-20-aarch64-valkey-soak-jemalloc.csv` (131 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,915374,1.095
write,memtier,set,22,52,30,915114,1.14300
write,valkey-benchmark,set,52,74,22,926741,1.071
write,memtier,set,74,104,30,922088,1.08700
write,valkey-benchmark,set,104,125,21,923873,1.071
write,memtier,set,125,155,30,935476,1.08700
write,valkey-benchmark,set,155,177,22,924770,1.063
write,memtier,set,177,207,30,927748,1.12700
write,valkey-benchmark,set,207,229,22,923916,1.071
write,memtier,set,229,259,30,929077,1.11900
write,valkey-benchmark,set,259,280,21,923105,1.071
write,memtier,set,280,310,30,916408,1.11100
write,valkey-benchmark,set,310,332,22,918232,1.079
write,memtier,set,332,362,30,929467,1.10300
write,valkey-benchmark,set,362,384,22,916632,1.079
write,memtier,set,384,414,30,927836,1.11900
write,valkey-benchmark,set,414,436,22,919582,1.079
write,memtier,set,436,466,30,924422,1.10300
write,valkey-benchmark,set,466,488,22,917810,1.071
write,memtier,set,488,518,30,917007,1.08700
write,valkey-benchmark,set,518,540,22,913409,1.079
write,memtier,set,540,570,30,927949,1.10300
write,valkey-benchmark,set,570,591,21,922211,1.071
write,memtier,set,591,621,30,926112,1.12700
delete,valkey-benchmark,del,621,636,15,1349437,1.135
delete,memtier,del:set=4:1,636,666,30,1161525,0.95900
delete,valkey-benchmark,del,666,681,15,1398210,0.727
delete,memtier,del:set=4:1,681,711,30,1153646,0.95900
delete,valkey-benchmark,del,711,725,14,1401149,0.727
delete,memtier,del:set=4:1,725,755,30,1150203,0.96700
delete,valkey-benchmark,del,755,769,14,1385138,0.735
delete,memtier,del:set=4:1,769,799,30,1156927,0.96700
delete,valkey-benchmark,del,799,814,15,1387732,0.735
delete,memtier,del:set=4:1,814,844,30,1149941,0.94300
delete,valkey-benchmark,del,844,858,14,1402721,0.727
delete,memtier,del:set=4:1,858,888,30,1154884,0.96700
delete,valkey-benchmark,del,888,903,15,1375989,0.743
delete,memtier,del:set=4:1,903,933,30,1155600,0.95100
delete,valkey-benchmark,del,933,947,14,1397234,0.727
delete,memtier,del:set=4:1,947,977,30,1146967,0.95900
delete,valkey-benchmark,del,977,991,14,1387347,0.735
delete,memtier,del:set=4:1,991,1021,30,1154392,0.97500
delete,valkey-benchmark,del,1021,1036,15,1390724,0.735
delete,memtier,del:set=4:1,1036,1066,30,1160430,0.94300
delete,valkey-benchmark,del,1066,1080,14,1394311,0.727
delete,memtier,del:set=4:1,1080,1110,30,1149364,0.98300
delete,valkey-benchmark,del,1110,1125,15,1396453,0.727
delete,memtier,del:set=4:1,1125,1155,30,1150637,0.97500
delete,valkey-benchmark,del,1155,1169,14,1394214,0.735
delete,memtier,del:set=4:1,1169,1199,30,1151918,0.97500
delete,valkey-benchmark,del,1199,1213,14,1397429,0.727
delete,memtier,del:set=4:1,1213,1243,30,1157820,0.97500
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,912284,1.087
write,memtier,set,22,52,30,961766,0.95900
write,valkey-benchmark,set,52,74,22,918442,1.063
write,memtier,set,74,104,30,957734,0.96700
write,valkey-benchmark,set,104,126,22,897747,1.095
write,memtier,set,126,156,30,942196,0.98300
write,valkey-benchmark,set,156,178,22,904814,1.079
write,memtier,set,178,208,30,955740,0.95900
write,valkey-benchmark,set,208,230,22,905715,1.079
write,memtier,set,230,260,30,956157,0.96700
write,valkey-benchmark,set,260,282,22,909256,1.079
write,memtier,set,282,312,30,956221,0.96700
write,valkey-benchmark,set,312,334,22,909960,1.079
write,memtier,set,334,364,30,950386,0.96700
write,valkey-benchmark,set,364,386,22,914871,1.071
write,memtier,set,386,416,30,959714,0.96700
write,valkey-benchmark,set,416,438,22,909628,1.079
write,memtier,set,438,468,30,952922,0.97500
write,valkey-benchmark,set,468,490,22,907771,1.079
write,memtier,set,490,520,30,956058,0.96700
write,valkey-benchmark,set,520,542,22,913701,1.071
write,memtier,set,542,572,30,957223,0.96700
write,valkey-benchmark,set,572,594,22,903261,1.095
write,memtier,set,594,624,30,941325,0.99100
delete,valkey-benchmark,del,624,639,15,1379406,1.127
delete,memtier,del:set=4:1,639,669,30,1260076,0.76700
delete,valkey-benchmark,del,669,683,14,1455922,0.695
delete,memtier,del:set=4:1,683,713,30,1284122,0.73500
delete,valkey-benchmark,del,713,726,13,1452855,0.695
delete,memtier,del:set=4:1,726,756,30,1273884,0.75100
delete,valkey-benchmark,del,756,770,14,1443314,0.703
delete,memtier,del:set=4:1,770,800,30,1284968,0.75100
delete,valkey-benchmark,del,800,814,14,1461347,0.695
delete,memtier,del:set=4:1,814,844,30,1280336,0.74300
delete,valkey-benchmark,del,844,858,14,1461347,0.695
delete,memtier,del:set=4:1,858,888,30,1280201,0.74300
delete,valkey-benchmark,del,888,901,13,1473948,0.687
delete,memtier,del:set=4:1,901,931,30,1287646,0.73500
delete,valkey-benchmark,del,931,945,14,1463486,0.695
delete,memtier,del:set=4:1,945,975,30,1283414,0.75100
delete,valkey-benchmark,del,975,989,14,1452538,0.703
delete,memtier,del:set=4:1,989,1019,30,1280922,0.74300
delete,valkey-benchmark,del,1019,1033,14,1447806,0.703
delete,memtier,del:set=4:1,1033,1063,30,1290589,0.75100
delete,valkey-benchmark,del,1063,1076,13,1462095,0.695
delete,memtier,del:set=4:1,1076,1106,30,1285123,0.73500
delete,valkey-benchmark,del,1106,1120,14,1473405,0.687
delete,memtier,del:set=4:1,1120,1150,30,1289718,0.74300
delete,valkey-benchmark,del,1150,1164,14,1469724,0.687
delete,memtier,del:set=4:1,1164,1194,30,1286084,0.73500
delete,valkey-benchmark,del,1194,1207,13,1451906,0.703
delete,memtier,del:set=4:1,1207,1237,30,1279182,0.73500
```

