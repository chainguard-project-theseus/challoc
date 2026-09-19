# challoc Valkey soak, 2026-09-19, aarch64

- commit: 79e3931
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=7d57a98ff6b37c9a
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=d835aa15467c5d27
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 60 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 60 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 30 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak reset per phase through /sys/fs/cgroup/system.slice/docker-8aac21aac6749b877f27a60d9e38ed87203162ba0cee515d3818666fa6b2dee0.scope/memory.peak (kernel 7.0.0-1011-gcp, bench/probes/cgpeak.c); the cgroup peak counts every process of the container and its page cache (memory.stat file, given per phase below), VmHWM the server alone
  - challoc: cgroup page cache (memory.stat file) at the end of the write / delete / settle phases: 0.1 / 0.1 / 0.1 MiB
  - jemalloc: cgroup page cache (memory.stat file) at the end of the write / delete / settle phases: 0.2 / 0.2 / 0.2 MiB

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 911186 | 855024 | -6.2% |
| write: valkey-benchmark p99 ms | 1.087 | 1.159 | -6.2% |
| write: memtier req/s | 945673 | 898122 | -5.0% |
| write: memtier p99 ms | 0.98300 | 1.03100 | -4.7% |
| delete: valkey-benchmark req/s | 1383892 | 1378835 | -0.4% |
| delete: valkey-benchmark p99 ms | 1.127 | 1.159 | -2.8% |
| delete: memtier req/s | 1236570 | 1220105 | -1.3% |
| delete: memtier p99 ms | 0.79900 | 0.83100 | -3.9% |
| peak RSS MiB (VmHWM) | 575.7 | 568.9 | +1.2% |
| cgroup memory.peak MiB, write phase | 598.4 | 587.8 | +1.8% |
| cgroup memory.peak MiB, delete phase | 578.4 | 570.7 | +1.3% |
| cgroup memory.peak MiB, settle phase | 140.1 | 148.3 | -5.5% |
| peak sampled used_memory_rss MiB | 575.6 | 568.6 | +1.2% |
| RSS MiB at end of write phase | 575.6 | 568.6 | +1.2% |
| RSS MiB at end of delete phase | 156.7 | 150.6 | +4.1% |
| RSS MiB after settle | 137.7 | 148.2 | -7.1% |
| allocator_frag_ratio after settle | 1.93 | 2.10 | -8.1% |
| active_defrag_hits | 229537 | 227222 | -1.0% |
| keys after settle | 104975 | 103345 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-19-aarch64-lane-d-task17-soak/2026-09-19-aarch64-valkey-soak-challoc.csv` (19 rows)
- jemalloc: `bench/results/2026-09-19-aarch64-lane-d-task17-soak/2026-09-19-aarch64-valkey-soak-jemalloc.csv` (20 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,22,22,913075,1.103
write,memtier,set,22,52,30,946818,0.98300
write,valkey-benchmark,set,52,74,22,909298,1.087
write,memtier,set,74,104,30,944528,0.98300
delete,valkey-benchmark,del,104,119,15,1383892,1.127
delete,memtier,del:set=4:1,119,149,30,1236570,0.79900
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,23,23,860215,1.159
write,memtier,set,23,53,30,917505,1.03100
write,valkey-benchmark,set,53,77,24,849834,1.159
write,memtier,set,77,107,30,878740,1.07100
delete,valkey-benchmark,del,107,121,14,1378835,1.159
delete,memtier,del:set=4:1,121,151,30,1220105,0.83100
```

