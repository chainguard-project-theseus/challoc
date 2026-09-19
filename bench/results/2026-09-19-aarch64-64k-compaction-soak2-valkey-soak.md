# challoc Valkey soak, 2026-09-19, aarch64

- commit: a3ddd6d
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 6.8.0-139-generic-64k, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=10ad49944226f7ec
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 150 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 150 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; INFO sampled every 1 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- occupancy: MEMORY MALLOC-STATS every 5 s under bench/results/tmp-4g-c/2026-09-19-aarch64-valkey-soak-<alloc>-mstats/

## Summary

| measure | challoc |
|---|---:|
| write: valkey-benchmark req/s | 1035622 |
| write: valkey-benchmark p99 ms | 0.943 |
| write: memtier req/s | 1067231 |
| write: memtier p99 ms | 0.83900 |
| delete: valkey-benchmark req/s | 1528551 |
| delete: valkey-benchmark p99 ms | 0.663 |
| delete: memtier req/s | 1382910 |
| delete: memtier p99 ms | 0.67100 |
| peak RSS MiB (VmHWM) | 590.0 |
| peak sampled used_memory_rss MiB | 590.0 |
| RSS MiB at end of write phase | 590.0 |
| RSS MiB at end of delete phase | 176.8 |
| RSS MiB after settle | 176.8 |
| allocator_frag_ratio after settle | 2.25 |
| active_defrag_hits | 287510 |
| keys after settle | 105855 |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/tmp-4g-c/2026-09-19-aarch64-valkey-soak-challoc.csv` (385 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,19,19,1031513,0.959
write,memtier,set,19,49,30,1072643,0.83900
write,valkey-benchmark,set,49,69,20,1040312,0.943
write,memtier,set,69,99,30,1068615,0.83900
write,valkey-benchmark,set,99,118,19,1034982,0.943
write,memtier,set,118,148,30,1069139,0.83900
write,valkey-benchmark,set,148,167,19,1035679,0.943
write,memtier,set,167,197,30,1058528,0.85500
delete,valkey-benchmark,del,197,211,14,1492649,1.031
delete,memtier,del:set=4:1,211,241,30,1376878,0.69500
delete,valkey-benchmark,del,241,254,13,1542615,0.663
delete,memtier,del:set=4:1,254,284,30,1380969,0.67100
delete,valkey-benchmark,del,284,297,13,1550388,0.655
delete,memtier,del:set=4:1,297,327,30,1390883,0.66300
```

