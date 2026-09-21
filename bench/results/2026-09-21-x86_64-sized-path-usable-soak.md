# challoc Valkey soak, 2026-09-21, x86_64

- commit: 63bd7ee
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc-old server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=24eab15631388e7a
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=5a357ef9223950b4
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=62b93a194144cea4
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup
- occupancy: MEMORY MALLOC-STATS every 20 s under /src/bench/results/2026-09-21-x86_64-valkey-soak-<alloc>-mstats/

## Summary

| measure | challoc-old | challoc | jemalloc | challoc vs jemalloc | challoc-old vs jemalloc |
|---|---:|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 750205 | 773389 | 708770 | -8.4% | -5.5% |
| write: valkey-benchmark p99 ms | 1.303 | 1.263 | 1.367 | -7.6% | -4.7% |
| write: memtier req/s | 925029 | 952707 | 917881 | -3.7% | -0.8% |
| write: memtier p99 ms | 1.03100 | 0.99100 | 1.00700 | -1.6% | +2.4% |
| delete: valkey-benchmark req/s | 1293452 | 1339362 | 1365599 | +2.0% | +5.6% |
| delete: valkey-benchmark p99 ms | 0.783 | 0.751 | 0.719 | +4.5% | +8.9% |
| delete: memtier req/s | 1198709 | 1242060 | 1230747 | -0.9% | +2.7% |
| delete: memtier p99 ms | 0.81500 | 0.79100 | 0.78300 | +1.0% | +4.1% |
| peak RSS MiB (VmHWM) | 577.3 | 579.6 | 573.0 | +1.2% | +0.8% |
| cgroup memory.peak MiB, write phase | - | - | - | - | - |
| cgroup memory.peak MiB, delete phase | - | - | - | - | - |
| cgroup memory.peak MiB, settle phase | - | - | - | - | - |
| peak sampled used_memory_rss MiB | 577.3 | 579.6 | 573.0 | +1.2% | +0.8% |
| RSS MiB at end of write phase | 577.0 | 579.6 | 572.8 | +1.2% | +0.7% |
| RSS MiB at end of delete phase | 131.1 | 130.3 | 130.4 | -0.1% | +0.5% |
| RSS MiB after settle | 130.9 | 130.3 | 130.1 | +0.2% | +0.6% |
| allocator_frag_ratio after settle | 1.79 | 1.79 | 1.78 | +0.6% | +0.6% |
| active_defrag_hits | 247124 | 245604 | 269608 | +9.8% | +9.1% |
| keys after settle | 105895 | 104943 | 103060 | - | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc-old: `/src/bench/results/2026-09-21-x86_64-valkey-soak-challoc-old.csv` (129 rows)
- challoc: `/src/bench/results/2026-09-21-x86_64-valkey-soak-challoc.csv` (127 rows)
- jemalloc: `/src/bench/results/2026-09-21-x86_64-valkey-soak-jemalloc.csv` (130 rows)

## Generator runs

### challoc-old

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,27,27,737898,1.319
write,memtier,set,27,57,30,940330,0.96700
write,valkey-benchmark,set,57,84,27,754660,1.247
write,memtier,set,84,114,30,922940,1.00700
write,valkey-benchmark,set,114,141,27,742528,1.311
write,memtier,set,141,171,30,923385,1.00700
write,valkey-benchmark,set,171,197,26,766842,1.231
write,memtier,set,197,227,30,929618,0.99900
write,valkey-benchmark,set,227,254,27,749204,1.303
write,memtier,set,254,284,30,922459,1.03100
write,valkey-benchmark,set,284,310,26,751230,1.303
write,memtier,set,310,340,30,922830,1.03900
write,valkey-benchmark,set,340,367,27,755258,1.287
write,memtier,set,367,397,30,923644,1.01500
write,valkey-benchmark,set,397,424,27,748027,1.303
write,memtier,set,424,454,30,923399,1.03100
write,valkey-benchmark,set,454,481,27,743163,1.319
write,memtier,set,481,511,30,922746,1.03900
write,valkey-benchmark,set,511,537,26,750920,1.303
write,memtier,set,537,567,30,922041,1.03100
write,valkey-benchmark,set,567,594,27,752530,1.303
write,memtier,set,594,624,30,921929,1.03100
delete,valkey-benchmark,del,624,639,15,1300052,1.111
delete,memtier,del:set=4:1,639,669,30,1184625,0.87100
delete,valkey-benchmark,del,669,685,16,1314147,0.767
delete,memtier,del:set=4:1,685,715,30,1184324,0.84700
delete,valkey-benchmark,del,715,731,16,1260160,0.807
delete,memtier,del:set=4:1,731,762,31,1169482,0.85500
delete,valkey-benchmark,del,762,777,15,1286670,0.783
delete,memtier,del:set=4:1,777,807,30,1208366,0.79900
delete,valkey-benchmark,del,807,823,16,1300306,0.783
delete,memtier,del:set=4:1,823,853,30,1213571,0.79100
delete,valkey-benchmark,del,853,868,15,1290239,0.783
delete,memtier,del:set=4:1,868,898,30,1199843,0.81500
delete,valkey-benchmark,del,898,914,16,1292574,0.783
delete,memtier,del:set=4:1,914,944,30,1207580,0.80700
delete,valkey-benchmark,del,944,959,15,1295085,0.775
delete,memtier,del:set=4:1,959,989,30,1205114,0.80700
delete,valkey-benchmark,del,989,1005,16,1287664,0.783
delete,memtier,del:set=4:1,1005,1035,30,1201452,0.80700
delete,valkey-benchmark,del,1035,1050,15,1296596,0.775
delete,memtier,del:set=4:1,1050,1080,30,1193875,0.83100
delete,valkey-benchmark,del,1080,1096,16,1296428,0.775
delete,memtier,del:set=4:1,1096,1126,30,1207405,0.83100
delete,valkey-benchmark,del,1126,1141,15,1307617,0.775
delete,memtier,del:set=4:1,1141,1171,30,1202407,0.81500
delete,valkey-benchmark,del,1171,1187,16,1287333,0.783
delete,memtier,del:set=4:1,1187,1217,30,1205168,0.80700
```

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,26,26,769497,1.271
write,memtier,set,26,56,30,960570,0.99100
write,valkey-benchmark,set,56,82,26,773934,1.263
write,memtier,set,82,112,30,952131,0.99900
write,valkey-benchmark,set,112,138,26,760630,1.295
write,memtier,set,138,168,30,937273,1.03900
write,valkey-benchmark,set,168,194,26,771694,1.263
write,memtier,set,194,224,30,959048,0.98300
write,valkey-benchmark,set,224,250,26,775134,1.263
write,memtier,set,250,280,30,948332,0.99900
write,valkey-benchmark,set,280,306,26,774054,1.263
write,memtier,set,306,336,30,955979,0.99100
write,valkey-benchmark,set,336,362,26,774024,1.263
write,memtier,set,362,392,30,944576,1.02300
write,valkey-benchmark,set,392,418,26,771962,1.263
write,memtier,set,418,448,30,957512,0.98300
write,valkey-benchmark,set,448,473,25,787247,1.239
write,memtier,set,473,503,30,952148,1.02300
write,valkey-benchmark,set,503,529,26,773545,1.263
write,memtier,set,529,559,30,956486,0.99100
write,valkey-benchmark,set,559,585,26,775554,1.255
write,memtier,set,585,615,30,955720,0.96700
delete,valkey-benchmark,del,615,630,15,1338151,1.079
delete,memtier,del:set=4:1,630,660,30,1229420,0.83900
delete,valkey-benchmark,del,660,675,15,1346801,0.751
delete,memtier,del:set=4:1,675,705,30,1243926,0.77500
delete,valkey-benchmark,del,705,720,15,1303356,0.775
delete,memtier,del:set=4:1,720,750,30,1218127,0.81500
delete,valkey-benchmark,del,750,765,15,1317870,0.767
delete,memtier,del:set=4:1,765,795,30,1243669,0.79100
delete,valkey-benchmark,del,795,810,15,1342823,0.751
delete,memtier,del:set=4:1,810,841,31,1239736,0.79900
delete,valkey-benchmark,del,841,856,15,1362955,0.735
delete,memtier,del:set=4:1,856,886,30,1239258,0.79100
delete,valkey-benchmark,del,886,901,15,1359065,0.743
delete,memtier,del:set=4:1,901,931,30,1247293,0.77500
delete,valkey-benchmark,del,931,946,15,1354738,0.743
delete,memtier,del:set=4:1,946,976,30,1253384,0.77500
delete,valkey-benchmark,del,976,990,14,1348982,0.743
delete,memtier,del:set=4:1,990,1020,30,1237364,0.79100
delete,valkey-benchmark,del,1020,1036,16,1327757,0.759
delete,memtier,del:set=4:1,1036,1066,30,1244323,0.76700
delete,valkey-benchmark,del,1066,1081,15,1324152,0.767
delete,memtier,del:set=4:1,1081,1111,30,1246780,0.79100
delete,valkey-benchmark,del,1111,1126,15,1336988,0.751
delete,memtier,del:set=4:1,1126,1156,30,1248603,0.76700
delete,valkey-benchmark,del,1156,1171,15,1348072,0.751
delete,memtier,del:set=4:1,1171,1201,30,1254893,0.77500
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,1,29,28,697934,1.399
write,memtier,set,29,59,30,930524,0.99100
write,valkey-benchmark,set,59,88,29,700673,1.391
write,memtier,set,88,118,30,903399,1.03900
write,valkey-benchmark,set,118,147,29,696258,1.399
write,memtier,set,147,177,30,911680,1.02300
write,valkey-benchmark,set,177,205,28,714745,1.351
write,memtier,set,205,235,30,925139,0.99900
write,valkey-benchmark,set,235,263,28,718597,1.319
write,memtier,set,263,293,30,916494,0.99900
write,valkey-benchmark,set,293,321,28,697520,1.399
write,memtier,set,321,351,30,914367,1.03100
write,valkey-benchmark,set,351,379,28,723144,1.311
write,memtier,set,379,409,30,919057,1.00700
write,valkey-benchmark,set,409,438,29,703581,1.383
write,memtier,set,438,468,30,926285,0.99900
write,valkey-benchmark,set,468,495,27,718365,1.311
write,memtier,set,495,525,30,908299,1.08700
write,valkey-benchmark,set,525,554,29,707189,1.367
write,memtier,set,554,584,30,924243,1.02300
write,valkey-benchmark,set,584,612,28,718468,1.319
write,memtier,set,612,642,30,917200,1.00700
delete,valkey-benchmark,del,642,657,15,1335559,1.087
delete,memtier,del:set=4:1,657,687,30,1223715,0.80700
delete,valkey-benchmark,del,687,701,14,1375989,0.719
delete,memtier,del:set=4:1,701,731,30,1205663,0.80700
delete,valkey-benchmark,del,731,746,15,1339674,0.751
delete,memtier,del:set=4:1,746,776,30,1216536,0.79900
delete,valkey-benchmark,del,776,791,15,1388310,0.703
delete,memtier,del:set=4:1,791,821,30,1233847,0.78300
delete,valkey-benchmark,del,821,835,14,1380548,0.711
delete,memtier,del:set=4:1,835,865,30,1229645,0.78300
delete,valkey-benchmark,del,865,880,15,1328021,0.759
delete,memtier,del:set=4:1,880,910,30,1238717,0.78300
delete,valkey-benchmark,del,910,925,15,1374004,0.719
delete,memtier,del:set=4:1,925,955,30,1229070,0.79100
delete,valkey-benchmark,del,955,970,15,1367241,0.727
delete,memtier,del:set=4:1,970,1000,30,1241496,0.77500
delete,valkey-benchmark,del,1000,1014,14,1378360,0.711
delete,memtier,del:set=4:1,1014,1044,30,1230183,0.78300
delete,valkey-benchmark,del,1044,1059,15,1377885,0.719
delete,memtier,del:set=4:1,1059,1089,30,1244856,0.77500
delete,valkey-benchmark,del,1089,1104,15,1370051,0.719
delete,memtier,del:set=4:1,1104,1134,30,1232387,0.79100
delete,valkey-benchmark,del,1134,1148,14,1373721,0.719
delete,memtier,del:set=4:1,1148,1178,30,1234507,0.78300
delete,valkey-benchmark,del,1178,1193,15,1363420,0.727
delete,memtier,del:set=4:1,1193,1223,30,1239094,0.78300
```

