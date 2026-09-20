# challoc Valkey soak, 2026-09-20, aarch64

- commit: be7e74c
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=29c2469f316c2d7b
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup

## Summary

| measure | challoc |
|---|---:|
| write: valkey-benchmark req/s | 929829 |
| write: valkey-benchmark p99 ms | 1.055 |
| write: memtier req/s | 958027 |
| write: memtier p99 ms | 0.97500 |
| delete: valkey-benchmark req/s | 1411080 |
| delete: valkey-benchmark p99 ms | 0.727 |
| delete: memtier req/s | 1232363 |
| delete: memtier p99 ms | 0.79100 |
| peak RSS MiB (VmHWM) | 578.3 |
| cgroup memory.peak MiB, write phase | - |
| cgroup memory.peak MiB, delete phase | - |
| cgroup memory.peak MiB, settle phase | - |
| peak sampled used_memory_rss MiB | 578.3 |
| RSS MiB at end of write phase | 578.0 |
| RSS MiB at end of delete phase | 130.8 |
| RSS MiB after settle | 130.8 |
| allocator_frag_ratio after settle | 1.81 |
| active_defrag_hits | 238629 |
| keys after settle | 106112 |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `.tmp-out/default/2026-09-20-aarch64-valkey-soak-challoc.csv` (131 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,1,22,21,924684,1.087
write,memtier,set,22,52,30,962962,0.97500
write,valkey-benchmark,set,52,74,22,939761,1.047
write,memtier,set,74,104,30,954639,0.97500
write,valkey-benchmark,set,104,125,21,936549,1.039
write,memtier,set,125,155,30,957069,0.97500
write,valkey-benchmark,set,155,176,21,931012,1.055
write,memtier,set,176,206,30,959248,0.98300
write,valkey-benchmark,set,206,228,22,933489,1.047
write,memtier,set,228,258,30,949948,0.97500
write,valkey-benchmark,set,258,279,21,932966,1.047
write,memtier,set,279,309,30,954237,0.97500
write,valkey-benchmark,set,309,331,22,920429,1.079
write,memtier,set,331,361,30,960705,0.97500
write,valkey-benchmark,set,361,383,22,922978,1.071
write,memtier,set,383,413,30,953396,0.98300
write,valkey-benchmark,set,413,434,21,936461,1.047
write,memtier,set,434,464,30,965446,0.97500
write,valkey-benchmark,set,464,486,22,930103,1.071
write,memtier,set,486,516,30,962736,0.98300
write,valkey-benchmark,set,516,537,21,927343,1.063
write,memtier,set,537,567,30,959688,0.98300
write,valkey-benchmark,set,567,589,22,922169,1.079
write,memtier,set,589,619,30,956252,0.98300
delete,valkey-benchmark,del,619,634,15,1366400,1.135
delete,memtier,del:set=4:1,634,664,30,1226220,0.82300
delete,valkey-benchmark,del,664,678,14,1424400,0.719
delete,memtier,del:set=4:1,678,708,30,1239211,0.77500
delete,valkey-benchmark,del,708,722,14,1423386,0.719
delete,memtier,del:set=4:1,722,752,30,1231499,0.79100
delete,valkey-benchmark,del,752,766,14,1425110,0.719
delete,memtier,del:set=4:1,766,796,30,1237001,0.78300
delete,valkey-benchmark,del,796,810,14,1416631,0.727
delete,memtier,del:set=4:1,810,840,30,1235579,0.79100
delete,valkey-benchmark,del,840,854,14,1413328,0.727
delete,memtier,del:set=4:1,854,884,30,1228769,0.79100
delete,valkey-benchmark,del,884,898,14,1413428,0.727
delete,memtier,del:set=4:1,898,928,30,1230202,0.80700
delete,valkey-benchmark,del,928,943,15,1398601,0.735
delete,memtier,del:set=4:1,943,973,30,1238604,0.79900
delete,valkey-benchmark,del,973,987,14,1407757,0.727
delete,memtier,del:set=4:1,987,1017,30,1232323,0.79100
delete,valkey-benchmark,del,1017,1031,14,1424197,0.719
delete,memtier,del:set=4:1,1031,1061,30,1224861,0.79100
delete,valkey-benchmark,del,1061,1075,14,1415929,0.727
delete,memtier,del:set=4:1,1075,1105,30,1225362,0.79900
delete,valkey-benchmark,del,1105,1119,14,1412928,0.727
delete,memtier,del:set=4:1,1119,1149,30,1218419,0.79900
delete,valkey-benchmark,del,1149,1164,15,1399091,0.735
delete,memtier,del:set=4:1,1164,1194,30,1242850,0.78300
delete,valkey-benchmark,del,1194,1208,14,1413927,0.727
delete,memtier,del:set=4:1,1208,1238,30,1242176,0.79100
```

