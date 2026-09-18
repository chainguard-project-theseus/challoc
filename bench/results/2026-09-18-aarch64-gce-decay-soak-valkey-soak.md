# challoc Valkey soak, 2026-09-18, aarch64

- commit: 2c8731f (iteration 28: the progressive-decay change plus iteration 27's cache changes, both due a soak); sidecar `bench/probes/valkey-perf-sidecar.sh` attached on the host, cycles per command in `2026-09-18-phase-1.5-acceptance-3.md` section 5
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
| write: valkey-benchmark req/s | 896952 | 877474 | -2.2% |
| write: valkey-benchmark p99 ms | 1.079 | 1.111 | -2.9% |
| write: memtier req/s | 930947 | 931708 | +0.1% |
| write: memtier p99 ms | 0.99100 | 0.99900 | -0.8% |
| delete: valkey-benchmark req/s | 1400346 | 1439237 | +2.8% |
| delete: valkey-benchmark p99 ms | 0.719 | 0.695 | +3.5% |
| delete: memtier req/s | 1218731 | 1258978 | +3.3% |
| delete: memtier p99 ms | 0.78300 | 0.75900 | +3.2% |
| peak RSS MiB (VmHWM) | 577.7 | 577.0 | +0.1% |
| peak sampled used_memory_rss MiB | 577.7 | 577.0 | +0.1% |
| RSS MiB at end of write phase | 577.5 | 576.7 | +0.1% |
| RSS MiB at end of delete phase | 128.8 | 131.9 | -2.4% |
| RSS MiB after settle | 128.8 | 131.9 | -2.4% |
| allocator_frag_ratio after settle | 1.79 | 1.81 | -1.1% |
| active_defrag_hits | 232210 | 237207 | +2.2% |
| keys after settle | 104906 | 103191 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-18-aarch64-gce-decay-soak-valkey-soak-challoc.csv` (128 rows)
- jemalloc: `bench/results/2026-09-18-aarch64-gce-decay-soak-valkey-soak-jemalloc.csv` (128 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,904159,1.111
write,memtier,set,22,52,30,955404,0.98300
write,valkey-benchmark,set,52,74,22,919667,1.071
write,memtier,set,74,104,30,926247,1.21500
write,valkey-benchmark,set,104,131,27,730034,1.463
write,memtier,set,131,161,30,760836,1.39100
write,valkey-benchmark,set,161,183,22,918189,1.071
write,memtier,set,183,213,30,954670,0.98300
write,valkey-benchmark,set,213,235,22,914369,1.079
write,memtier,set,235,265,30,948322,0.99100
write,valkey-benchmark,set,265,287,22,911037,1.087
write,memtier,set,287,317,30,947562,0.99100
write,valkey-benchmark,set,317,339,22,910871,1.079
write,memtier,set,339,369,30,953665,0.99100
write,valkey-benchmark,set,369,391,22,915877,1.079
write,memtier,set,391,421,30,952127,0.97500
write,valkey-benchmark,set,421,443,22,911286,1.079
write,memtier,set,443,473,30,939923,0.99900
write,valkey-benchmark,set,473,495,22,909628,1.079
write,memtier,set,495,525,30,940629,0.99100
write,valkey-benchmark,set,525,547,22,908884,1.087
write,memtier,set,547,577,30,945943,0.99100
write,valkey-benchmark,set,577,599,22,909422,1.079
write,memtier,set,599,629,30,946035,0.98300
delete,valkey-benchmark,del,629,643,14,1374098,1.127
delete,memtier,del:set=4:1,643,673,30,1234807,0.80700
delete,valkey-benchmark,del,673,687,14,1433178,0.711
delete,memtier,del:set=4:1,687,717,30,1098339,1.01500
delete,valkey-benchmark,del,717,734,17,1169659,0.935
delete,memtier,del:set=4:1,734,764,30,1043467,1.03100
delete,valkey-benchmark,del,764,779,15,1416230,0.719
delete,memtier,del:set=4:1,779,809,30,1246637,0.78300
delete,valkey-benchmark,del,809,823,14,1427144,0.711
delete,memtier,del:set=4:1,823,853,30,1250153,0.77500
delete,valkey-benchmark,del,853,867,14,1427348,0.711
delete,memtier,del:set=4:1,867,897,30,1247970,0.77500
delete,valkey-benchmark,del,897,911,14,1419346,0.719
delete,memtier,del:set=4:1,911,941,30,1238076,0.78300
delete,valkey-benchmark,del,941,955,14,1428061,0.711
delete,memtier,del:set=4:1,955,985,30,1249150,0.77500
delete,valkey-benchmark,del,985,999,14,1427042,0.711
delete,memtier,del:set=4:1,999,1029,30,1241358,0.79100
delete,valkey-benchmark,del,1029,1043,14,1428674,0.711
delete,memtier,del:set=4:1,1043,1073,30,1250371,0.77500
delete,valkey-benchmark,del,1073,1087,14,1416832,0.719
delete,memtier,del:set=4:1,1087,1117,30,1248955,0.78300
delete,valkey-benchmark,del,1117,1131,14,1420858,0.719
delete,memtier,del:set=4:1,1131,1161,30,1244990,0.77500
delete,valkey-benchmark,del,1161,1175,14,1416030,0.719
delete,memtier,del:set=4:1,1175,1205,30,1249235,0.77500
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,23,23,896499,1.119
write,memtier,set,23,53,30,971825,0.95900
write,valkey-benchmark,set,53,75,22,893096,1.111
write,memtier,set,75,105,30,919185,1.27100
write,valkey-benchmark,set,105,134,29,699496,1.575
write,memtier,set,134,164,30,785386,1.36700
write,valkey-benchmark,set,164,186,22,890631,1.111
write,memtier,set,186,216,30,941309,0.99900
write,valkey-benchmark,set,216,239,23,889878,1.111
write,memtier,set,239,269,30,939943,0.99900
write,valkey-benchmark,set,269,291,22,895375,1.111
write,memtier,set,291,321,30,942034,0.99100
write,valkey-benchmark,set,321,344,23,890313,1.111
write,memtier,set,344,374,30,955936,0.99100
write,valkey-benchmark,set,374,396,22,899726,1.087
write,memtier,set,396,426,30,951811,0.98300
write,valkey-benchmark,set,426,448,22,901835,1.087
write,memtier,set,448,478,30,958006,0.97500
write,valkey-benchmark,set,478,500,22,893256,1.111
write,memtier,set,500,531,31,948336,0.99900
write,valkey-benchmark,set,531,553,22,892738,1.111
write,memtier,set,553,583,30,927338,1.01500
write,valkey-benchmark,set,583,605,22,886839,1.119
write,memtier,set,605,636,31,939386,1.00700
delete,valkey-benchmark,del,636,650,14,1399482,1.127
delete,memtier,del:set=4:1,650,680,30,1273198,0.78300
delete,valkey-benchmark,del,680,694,14,1461882,0.695
delete,memtier,del:set=4:1,694,724,30,1081292,1.01500
delete,valkey-benchmark,del,724,740,16,1191895,0.927
delete,memtier,del:set=4:1,740,770,30,1102379,1.00700
delete,valkey-benchmark,del,770,784,14,1468860,0.695
delete,memtier,del:set=4:1,784,814,30,1284401,0.75900
delete,valkey-benchmark,del,814,828,14,1454969,0.703
delete,memtier,del:set=4:1,828,858,30,1294199,0.75100
delete,valkey-benchmark,del,858,871,13,1470913,0.695
delete,memtier,del:set=4:1,871,901,30,1281035,0.75900
delete,valkey-benchmark,del,901,915,14,1450011,0.703
delete,memtier,del:set=4:1,915,945,30,1295701,0.74300
delete,valkey-benchmark,del,945,959,14,1478197,0.687
delete,memtier,del:set=4:1,959,989,30,1285200,0.75900
delete,valkey-benchmark,del,989,1003,14,1452011,0.703
delete,memtier,del:set=4:1,1003,1033,30,1286300,0.75900
delete,valkey-benchmark,del,1033,1046,13,1480385,0.687
delete,memtier,del:set=4:1,1046,1076,30,1293725,0.75100
delete,valkey-benchmark,del,1076,1090,14,1459960,0.695
delete,memtier,del:set=4:1,1090,1120,30,1302817,0.75100
delete,valkey-benchmark,del,1120,1133,13,1466813,0.695
delete,memtier,del:set=4:1,1133,1163,30,1296293,0.74300
delete,valkey-benchmark,del,1163,1177,14,1474709,0.687
delete,memtier,del:set=4:1,1177,1207,30,1290170,0.74300
```

