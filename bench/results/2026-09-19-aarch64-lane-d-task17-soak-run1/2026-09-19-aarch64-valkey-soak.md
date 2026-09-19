# challoc Valkey soak, 2026-09-19, aarch64

- commit: 79e3931
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=7d57a98ff6b37c9a
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=d835aa15467c5d27
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 60 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 60 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 30 s idle; activedefrag yes; io-threads 1; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase
- cgroup memory.peak reset per phase through /sys/fs/cgroup/system.slice/docker-73dc9d95818102604c8289b2111305d2c6268f1046056c518b172e742df66c31.scope/memory.peak (kernel 7.0.0-1011-gcp, bench/probes/cgpeak.c); the cgroup peak counts every process of the container, VmHWM the server alone

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 889102 | 863991 | -2.8% |
| write: valkey-benchmark p99 ms | 1.095 | 1.127 | -2.8% |
| write: memtier req/s | 926387 | 929258 | +0.3% |
| write: memtier p99 ms | 1.02300 | 1.00700 | +1.6% |
| delete: valkey-benchmark req/s | 1371178 | 1392855 | +1.6% |
| delete: valkey-benchmark p99 ms | 1.151 | 1.135 | +1.4% |
| delete: memtier req/s | 1222873 | 1252766 | +2.4% |
| delete: memtier p99 ms | 0.81500 | 0.79900 | +2.0% |
| peak RSS MiB (VmHWM) | 574.2 | 570.7 | +0.6% |
| cgroup memory.peak MiB, write phase | 596.3 | 907.5 | -34.3% |
| cgroup memory.peak MiB, delete phase | 576.9 | 889.9 | -35.2% |
| cgroup memory.peak MiB, settle phase | 141.9 | 467.8 | -69.7% |
| peak sampled used_memory_rss MiB | 574.0 | 570.4 | +0.6% |
| RSS MiB at end of write phase | 574.0 | 570.4 | +0.6% |
| RSS MiB at end of delete phase | 156.1 | 152.2 | +2.6% |
| RSS MiB after settle | 135.6 | 149.3 | -9.2% |
| allocator_frag_ratio after settle | 1.91 | 2.13 | -10.3% |
| active_defrag_hits | 224671 | 231353 | +3.0% |
| keys after settle | 104365 | 102696 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-19-aarch64-lane-d-task17-soak/2026-09-19-aarch64-valkey-soak-challoc.csv` (19 rows)
- jemalloc: `bench/results/2026-09-19-aarch64-lane-d-task17-soak/2026-09-19-aarch64-valkey-soak-jemalloc.csv` (20 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,23,23,879894,1.127
write,memtier,set,23,53,30,930375,1.02300
write,valkey-benchmark,set,53,75,22,898311,1.095
write,memtier,set,75,105,30,922399,1.03100
delete,valkey-benchmark,del,105,120,15,1371178,1.151
delete,memtier,del:set=4:1,120,150,30,1222873,0.81500
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,23,23,865913,1.135
write,memtier,set,23,53,30,932783,1.00700
write,valkey-benchmark,set,53,76,23,862069,1.127
write,memtier,set,76,106,30,925733,1.00700
delete,valkey-benchmark,del,107,121,14,1392855,1.135
delete,memtier,del:set=4:1,121,151,30,1252766,0.79900
```

