# challoc Valkey soak, 2026-09-19, aarch64

- commit: a3ddd6d
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 6.8.0-139-generic-64k, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=10ad49944226f7ec
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=d6d1e327ffc2e084
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 150 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 150 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; INFO sampled every 1 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- occupancy: MEMORY MALLOC-STATS every 5 s under bench/results/tmp-4g/2026-09-19-aarch64-valkey-soak-<alloc>-mstats/

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 1026472 | 1008642 | -1.7% |
| write: valkey-benchmark p99 ms | 0.951 | 0.967 | -1.7% |
| write: memtier req/s | 1060692 | 1060695 | +0.0% |
| write: memtier p99 ms | 0.83900 | 0.83900 | +0.0% |
| delete: valkey-benchmark req/s | 1513651 | 1531395 | +1.2% |
| delete: valkey-benchmark p99 ms | 0.671 | 0.663 | +1.2% |
| delete: memtier req/s | 1363478 | 1381015 | +1.3% |
| delete: memtier p99 ms | 0.69500 | 0.67900 | +2.4% |
| peak RSS MiB (VmHWM) | 592.0 | 588.0 | +0.7% |
| peak sampled used_memory_rss MiB | 592.0 | 588.0 | +0.7% |
| RSS MiB at end of write phase | 592.0 | 584.0 | +1.4% |
| RSS MiB at end of delete phase | 180.9 | 155.8 | +16.1% |
| RSS MiB after settle | 180.9 | 153.8 | +17.6% |
| allocator_frag_ratio after settle | 2.27 | 2.18 | +4.1% |
| active_defrag_hits | 289809 | 368527 | +27.2% |
| keys after settle | 106997 | 106590 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/tmp-4g/2026-09-19-aarch64-valkey-soak-challoc.csv` (386 rows)
- jemalloc: `bench/results/tmp-4g/2026-09-19-aarch64-valkey-soak-jemalloc.csv` (387 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,19,19,1033004,0.951
write,memtier,set,19,49,30,1072875,0.83900
write,valkey-benchmark,set,49,69,20,1036001,0.943
write,memtier,set,69,99,30,1067425,0.83900
write,valkey-benchmark,set,99,118,19,1021346,0.959
write,memtier,set,118,148,30,1053634,0.85500
write,valkey-benchmark,set,148,168,20,1015538,0.967
write,memtier,set,168,198,30,1048835,0.87100
delete,valkey-benchmark,del,198,211,13,1479071,1.031
delete,memtier,del:set=4:1,211,241,30,1358593,0.71100
delete,valkey-benchmark,del,241,254,13,1535980,0.663
delete,memtier,del:set=4:1,254,284,30,1361958,0.69500
delete,valkey-benchmark,del,284,298,14,1525902,0.671
delete,memtier,del:set=4:1,298,328,30,1369883,0.68700
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,19,19,1008776,0.975
write,memtier,set,19,49,30,1080540,0.82300
write,valkey-benchmark,set,49,69,20,1015486,0.959
write,memtier,set,69,99,30,1066365,0.83900
write,valkey-benchmark,set,99,119,20,1004218,0.967
write,memtier,set,119,149,30,1049498,0.86300
write,valkey-benchmark,set,149,169,20,1006087,0.967
write,memtier,set,169,199,30,1046378,0.87100
delete,valkey-benchmark,del,199,212,13,1492537,1.015
delete,memtier,del:set=4:1,212,242,30,1367130,0.71900
delete,valkey-benchmark,del,242,255,13,1545953,0.663
delete,memtier,del:set=4:1,255,285,30,1388437,0.67900
delete,valkey-benchmark,del,285,298,13,1555694,0.655
delete,memtier,del:set=4:1,298,328,30,1387479,0.67900
```

