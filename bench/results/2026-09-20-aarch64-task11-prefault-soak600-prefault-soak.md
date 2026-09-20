# challoc Valkey soak, 2026-09-20, aarch64

- commit: HEAD
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=29c2469f316c2d7b
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- challoc server MALLOC_CONF: prefault:true,background_thread:true (CHALLOC_CONF; the jemalloc server runs stock)
- workload: write phase 300 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 300 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup

## Summary

| measure | challoc |
|---|---:|
| write: valkey-benchmark req/s | 929971 |
| write: valkey-benchmark p99 ms | 1.063 |
| write: memtier req/s | 967478 |
| write: memtier p99 ms | 0.95900 |
| delete: valkey-benchmark req/s | 1425669 |
| delete: valkey-benchmark p99 ms | 0.711 |
| delete: memtier req/s | 1258953 |
| delete: memtier p99 ms | 0.76700 |
| peak RSS MiB (VmHWM) | 584.8 |
| cgroup memory.peak MiB, write phase | - |
| cgroup memory.peak MiB, delete phase | - |
| cgroup memory.peak MiB, settle phase | - |
| peak sampled used_memory_rss MiB | 584.8 |
| RSS MiB at end of write phase | 584.6 |
| RSS MiB at end of delete phase | 135.4 |
| RSS MiB after settle | 135.4 |
| allocator_frag_ratio after settle | 1.81 |
| active_defrag_hits | 236672 |
| keys after settle | 105714 |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `.tmp-out600/prefault/2026-09-20-aarch64-valkey-soak-challoc.csv` (69 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,1,22,21,932705,1.063
write,memtier,set,22,52,30,977132,0.95100
write,valkey-benchmark,set,52,73,21,941841,1.039
write,memtier,set,73,103,30,964365,0.95900
write,valkey-benchmark,set,103,125,22,922211,1.063
write,memtier,set,125,155,30,967157,0.95900
write,valkey-benchmark,set,155,177,22,927472,1.063
write,memtier,set,177,207,30,964984,0.95900
write,valkey-benchmark,set,207,228,21,929541,1.055
write,memtier,set,228,258,30,967118,0.95900
write,valkey-benchmark,set,258,280,22,926055,1.063
write,memtier,set,280,310,30,964109,0.96700
delete,valkey-benchmark,del,310,324,14,1380548,1.119
delete,memtier,del:set=4:1,324,354,30,1248415,0.79100
delete,valkey-benchmark,del,354,368,14,1444252,0.703
delete,memtier,del:set=4:1,368,398,30,1260141,0.77500
delete,valkey-benchmark,del,398,412,14,1447911,0.703
delete,memtier,del:set=4:1,412,442,30,1258082,0.77500
delete,valkey-benchmark,del,442,456,14,1436782,0.711
delete,memtier,del:set=4:1,456,486,30,1261448,0.76700
delete,valkey-benchmark,del,486,500,14,1423082,0.719
delete,memtier,del:set=4:1,500,530,30,1263189,0.76700
delete,valkey-benchmark,del,530,544,14,1436369,0.711
delete,memtier,del:set=4:1,544,574,30,1260766,0.75900
delete,valkey-benchmark,del,574,588,14,1410736,0.727
delete,memtier,del:set=4:1,588,618,30,1260632,0.76700
```

