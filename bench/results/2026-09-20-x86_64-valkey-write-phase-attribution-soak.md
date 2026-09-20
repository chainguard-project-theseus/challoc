# challoc Valkey soak, 2026-09-20, x86_64

- commit: e3e5fc3
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=cd6f6ca032ba885e
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=c62dee354779685
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup
- occupancy: MEMORY MALLOC-STATS every 20 s; the t=580 and t=1180 samples are kept as bench/results/2026-09-20-x86_64-valkey-write-phase-attribution-mstats-<alloc>-t{580,1180}.txt (the rest of the 63 per allocator were not committed); files renamed from the soak's <date>-x86_64-valkey-soak* by the attribution iteration

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 761992 | 731845 | -4.0% |
| write: valkey-benchmark p99 ms | 1.255 | 1.287 | -2.5% |
| write: memtier req/s | 912526 | 916554 | +0.4% |
| write: memtier p99 ms | 0.99900 | 1.00700 | -0.8% |
| delete: valkey-benchmark req/s | 1331038 | 1381126 | +3.8% |
| delete: valkey-benchmark p99 ms | 0.743 | 0.711 | +4.5% |
| delete: memtier req/s | 1200051 | 1246357 | +3.9% |
| delete: memtier p99 ms | 0.79900 | 0.76700 | +4.2% |
| peak RSS MiB (VmHWM) | 575.9 | 571.8 | +0.7% |
| cgroup memory.peak MiB, write phase | - | - | - |
| cgroup memory.peak MiB, delete phase | - | - | - |
| cgroup memory.peak MiB, settle phase | - | - | - |
| peak sampled used_memory_rss MiB | 575.9 | 571.7 | +0.7% |
| RSS MiB at end of write phase | 575.7 | 571.5 | +0.7% |
| RSS MiB at end of delete phase | 127.9 | 129.5 | -1.2% |
| RSS MiB after settle | 127.9 | 129.2 | -1.0% |
| allocator_frag_ratio after settle | 1.77 | 1.79 | -1.1% |
| active_defrag_hits | 248765 | 267469 | +7.5% |
| keys after settle | 104997 | 102480 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-20-x86_64-valkey-write-phase-attribution-soak-challoc.csv` (128 rows)
- jemalloc: `bench/results/2026-09-20-x86_64-valkey-write-phase-attribution-soak-jemalloc.csv` (128 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,26,26,752644,1.271
write,memtier,set,26,56,30,904768,1.01500
write,valkey-benchmark,set,56,83,27,755972,1.255
write,memtier,set,83,113,30,896801,1.03100
write,valkey-benchmark,set,113,140,27,739973,1.287
write,memtier,set,140,170,30,885204,1.04700
write,valkey-benchmark,set,170,196,26,759590,1.255
write,memtier,set,196,226,30,904389,1.00700
write,valkey-benchmark,set,226,252,26,766930,1.255
write,memtier,set,252,282,30,922404,0.99100
write,valkey-benchmark,set,282,309,27,762108,1.239
write,memtier,set,309,339,30,916160,0.99900
write,valkey-benchmark,set,339,365,26,775314,1.223
write,memtier,set,365,395,30,914562,0.99900
write,valkey-benchmark,set,395,420,25,776940,1.215
write,memtier,set,420,450,30,930540,0.99100
write,valkey-benchmark,set,450,477,27,756630,1.255
write,memtier,set,477,507,30,931567,0.98300
write,valkey-benchmark,set,507,533,26,766078,1.239
write,memtier,set,533,563,30,914763,0.99900
write,valkey-benchmark,set,563,589,26,769734,1.231
write,memtier,set,589,619,30,916630,0.99900
delete,valkey-benchmark,del,619,634,15,1317176,1.111
delete,memtier,del:set=4:1,634,664,30,1188468,0.82300
delete,valkey-benchmark,del,664,679,15,1348254,0.727
delete,memtier,del:set=4:1,679,709,30,1180850,0.81500
delete,valkey-benchmark,del,709,725,16,1311475,0.751
delete,memtier,del:set=4:1,725,755,30,1166194,0.83100
delete,valkey-benchmark,del,755,770,15,1292240,0.783
delete,memtier,del:set=4:1,770,800,30,1204562,0.79100
delete,valkey-benchmark,del,800,815,15,1326084,0.759
delete,memtier,del:set=4:1,815,845,30,1199568,0.79900
delete,valkey-benchmark,del,845,860,15,1342372,0.735
delete,memtier,del:set=4:1,860,890,30,1198628,0.79100
delete,valkey-benchmark,del,890,905,15,1340393,0.735
delete,memtier,del:set=4:1,905,935,30,1210627,0.79100
delete,valkey-benchmark,del,935,950,15,1341112,0.735
delete,memtier,del:set=4:1,950,980,30,1215861,0.79100
delete,valkey-benchmark,del,980,995,15,1336273,0.743
delete,memtier,del:set=4:1,995,1025,30,1210228,0.79100
delete,valkey-benchmark,del,1025,1040,15,1345623,0.735
delete,memtier,del:set=4:1,1040,1070,30,1209453,0.79100
delete,valkey-benchmark,del,1070,1085,15,1336362,0.735
delete,memtier,del:set=4:1,1085,1115,30,1201191,0.79900
delete,valkey-benchmark,del,1115,1130,15,1332356,0.743
delete,memtier,del:set=4:1,1130,1160,30,1207132,0.79900
delete,valkey-benchmark,del,1160,1175,15,1333778,0.743
delete,memtier,del:set=4:1,1175,1205,30,1207899,0.79900
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,27,27,730594,1.319
write,memtier,set,27,57,30,922907,0.99900
write,valkey-benchmark,set,57,84,27,731449,1.295
write,memtier,set,84,115,31,903963,1.05500
write,valkey-benchmark,set,115,142,27,718417,1.343
write,memtier,set,142,172,30,911795,1.03900
write,valkey-benchmark,set,172,200,28,737626,1.279
write,memtier,set,200,230,30,911813,1.00700
write,valkey-benchmark,set,230,257,27,736269,1.287
write,memtier,set,257,287,30,911261,1.01500
write,valkey-benchmark,set,287,314,27,733783,1.287
write,memtier,set,314,344,30,917607,1.00700
write,valkey-benchmark,set,344,372,28,729581,1.287
write,memtier,set,372,402,30,918450,1.00700
write,valkey-benchmark,set,402,429,27,736892,1.287
write,memtier,set,429,459,30,906748,1.02300
write,valkey-benchmark,set,459,486,27,735646,1.287
write,memtier,set,486,516,30,916802,1.00700
write,valkey-benchmark,set,516,544,28,727008,1.303
write,memtier,set,544,574,30,931312,0.99100
write,valkey-benchmark,set,574,601,27,733030,1.287
write,memtier,set,601,631,30,929434,0.99100
delete,valkey-benchmark,del,631,646,15,1346076,1.079
delete,memtier,del:set=4:1,646,676,30,1240949,0.79100
delete,valkey-benchmark,del,676,690,14,1384370,0.711
delete,memtier,del:set=4:1,690,720,30,1225370,0.79900
delete,valkey-benchmark,del,720,735,15,1358326,0.727
delete,memtier,del:set=4:1,735,765,30,1223278,0.79100
delete,valkey-benchmark,del,765,780,15,1393146,0.703
delete,memtier,del:set=4:1,780,810,30,1255072,0.76700
delete,valkey-benchmark,del,810,824,14,1381406,0.719
delete,memtier,del:set=4:1,824,854,30,1252741,0.76700
delete,valkey-benchmark,del,854,869,15,1386866,0.711
delete,memtier,del:set=4:1,869,899,30,1252162,0.76700
delete,valkey-benchmark,del,899,913,14,1386578,0.711
delete,memtier,del:set=4:1,913,943,30,1255610,0.75100
delete,valkey-benchmark,del,943,958,15,1385234,0.711
delete,memtier,del:set=4:1,958,988,30,1246206,0.77500
delete,valkey-benchmark,del,988,1002,14,1387347,0.711
delete,memtier,del:set=4:1,1002,1032,30,1252346,0.76700
delete,valkey-benchmark,del,1032,1047,15,1386770,0.711
delete,memtier,del:set=4:1,1047,1077,30,1247056,0.77500
delete,valkey-benchmark,del,1077,1091,14,1381120,0.711
delete,memtier,del:set=4:1,1091,1121,30,1255915,0.76700
delete,valkey-benchmark,del,1121,1136,15,1389854,0.711
delete,memtier,del:set=4:1,1136,1166,30,1244490,0.77500
delete,valkey-benchmark,del,1166,1180,14,1387540,0.711
delete,memtier,del:set=4:1,1180,1210,30,1251444,0.76700
```

