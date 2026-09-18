# challoc Valkey soak, 2026-09-18, aarch64

- commit: c18979d, `DURATION=600`, perf sidecar attached (task 4d run 1); see `2026-09-18-aarch64-gce-valkey-throughput.md`
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=29c2469f316c2d7b
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=c645b848879441b2
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 300 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 300 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; INFO sampled every 10 s
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- occupancy: MEMORY MALLOC-STATS every 20 s under target/valkey/soak-4d/2026-09-18-aarch64-valkey-soak-<alloc>-mstats/

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 765834 | 850492 | +11.1% |
| write: valkey-benchmark p99 ms | 1.255 | 1.127 | +11.4% |
| write: memtier req/s | 862606 | 899876 | +4.3% |
| write: memtier p99 ms | 1.05500 | 1.02300 | +3.1% |
| delete: valkey-benchmark req/s | 1335273 | 1419519 | +6.3% |
| delete: valkey-benchmark p99 ms | 0.735 | 0.695 | +5.8% |
| delete: memtier req/s | 1152770 | 1236664 | +7.3% |
| delete: memtier p99 ms | 0.81500 | 0.76700 | +6.3% |
| peak RSS MiB (VmHWM) | 573.7 | 574.1 | -0.1% |
| peak sampled used_memory_rss MiB | 573.7 | 574.1 | -0.1% |
| RSS MiB at end of write phase | 573.5 | 574.0 | -0.1% |
| RSS MiB at end of delete phase | 124.4 | 134.5 | -7.5% |
| RSS MiB after settle | 124.4 | 133.6 | -6.9% |
| allocator_frag_ratio after settle | 1.75 | 1.87 | -6.4% |
| active_defrag_hits | 229115 | 234660 | +2.4% |
| keys after settle | 102390 | 102808 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-18-aarch64-gce-valkey-throughput-soak1-valkey-soak-challoc.csv` (68 rows)
- jemalloc: `bench/results/2026-09-18-aarch64-gce-valkey-throughput-soak1-valkey-soak-jemalloc.csv` (71 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,25,25,800544,1.247
write,memtier,set,25,55,30,903933,1.03900
write,valkey-benchmark,set,55,80,25,803406,1.231
write,memtier,set,80,110,30,831347,1.44700
write,valkey-benchmark,set,110,142,32,625900,1.799
write,memtier,set,142,172,30,776146,1.41500
write,valkey-benchmark,set,172,197,25,788861,1.255
write,memtier,set,197,227,30,899356,1.05500
write,valkey-benchmark,set,227,253,26,788519,1.255
write,memtier,set,253,283,30,871369,1.09500
write,valkey-benchmark,set,283,308,25,787774,1.255
write,memtier,set,308,338,30,893485,1.05500
delete,valkey-benchmark,del,338,353,15,1329345,1.159
delete,memtier,del:set=4:1,353,383,30,1195005,0.83100
delete,valkey-benchmark,del,383,398,15,1391498,0.735
delete,memtier,del:set=4:1,398,428,30,997307,1.06300
delete,valkey-benchmark,del,428,445,17,1131222,0.967
delete,memtier,del:set=4:1,445,475,30,1089799,1.02300
delete,valkey-benchmark,del,475,490,15,1391498,0.735
delete,memtier,del:set=4:1,490,520,30,1216851,0.79900
delete,valkey-benchmark,del,520,534,14,1385521,0.735
delete,memtier,del:set=4:1,534,564,30,1208046,0.81500
delete,valkey-benchmark,del,564,579,15,1382552,0.735
delete,memtier,del:set=4:1,579,609,30,1209612,0.81500
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,882652,1.135
write,memtier,set,22,52,30,962526,0.98300
write,valkey-benchmark,set,52,75,23,886486,1.119
write,memtier,set,75,105,30,889900,1.32700
write,valkey-benchmark,set,105,134,29,695676,1.567
write,memtier,set,134,164,30,763960,1.39100
write,valkey-benchmark,set,164,186,22,878928,1.127
write,memtier,set,186,216,30,933334,1.01500
write,valkey-benchmark,set,216,239,23,875235,1.135
write,memtier,set,239,269,30,927851,1.02300
write,valkey-benchmark,set,269,292,23,883978,1.111
write,memtier,set,292,322,30,921688,1.03100
delete,valkey-benchmark,del,322,336,14,1407955,1.127
delete,memtier,del:set=4:1,336,366,30,1286073,0.78300
delete,valkey-benchmark,del,366,380,14,1483130,0.687
delete,memtier,del:set=4:1,380,410,30,1192404,0.95900
delete,valkey-benchmark,del,410,426,16,1202284,0.911
delete,memtier,del:set=4:1,426,456,30,1029173,1.03100
delete,valkey-benchmark,del,456,470,14,1423690,0.807
delete,memtier,del:set=4:1,470,500,30,1288645,0.75100
delete,valkey-benchmark,del,500,514,14,1476451,0.687
delete,memtier,del:set=4:1,514,544,30,1293703,0.74300
delete,valkey-benchmark,del,544,558,14,1470805,0.695
delete,memtier,del:set=4:1,558,588,30,1288225,0.75900
delete,valkey-benchmark,del,588,601,13,1472320,0.687
delete,memtier,del:set=4:1,601,631,30,1278425,0.76700
```

