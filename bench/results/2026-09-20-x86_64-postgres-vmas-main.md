# PostgreSQL 17 benchmark rows, 2026-09-21, x86_64

- commit: f419b88 (uncommitted changes in src/)
- date: 2026-09-21T00:47:39Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container; GCE c4-standard-16, lane fork-vmas VM; challoc at main f419b88 for the same-VM before/after
- kernel: 7.0.0-1011-gcp, container: Wolfi
- server: postgres (PostgreSQL) 17.10, pgbench (PostgreSQL) 17.10 (Wolfi postgresql-17 17.10); PGDATA target/postgres/data (initdb --locale=C -E UTF8, OS and database user pgb, trust auth on a unix socket), started by `pg_ctl start` as pgb with the allocator in its environment (LD_PRELOAD), so the postmaster and every backend and auxiliary process it forks run on it; pgbench (the client, its -j threads) runs as root on glibc malloc, the same binary for every allocator. Options on the command line: block_size=8192 checkpoint_timeout=1800 huge_pages=off jit=on max_connections=600 max_wal_size=8192 shared_buffers=131072 synchronous_commit=off (postgresql.conf untouched otherwise)
- data: pgbench -i -s 50 (5000000 accounts rows), database size 809 MB, loaded once and shared by every allocator (the data files do not depend on the server's allocator)
- rows: select100 = `pgbench -S -c 100 -j 16 -T 60` (select-only); tpcb100 = `pgbench -c 100 -j 16 -T 60` (TPC-B); select500 = `pgbench -S -c 500 -j 16 -T 60` (select-only, max_connections 600); connect20 = `pgbench -C -c 20 -j 4 -T 30` (a new connection per transaction: fork, backend start-up, one TPC-B transaction, exit); all with pgbench's default pre-run vacuum of pgbench_branches/tellers and truncate of pgbench_history (outside the timed window) and -l --sampling-rate=0.05 for the latency percentiles
- load average at start: 63.02 125.88 136.91; at end: 147.92 154.45 147.07; 1357 s total
- method: 5 timed rounds after one discarded warm-up, allocators interleaved per round (one server start per allocator per round, the rows in the order above against it, then a fast stop), every run's tps listed sorted. tps and latency average are pgbench's lines (connect20's tps includes the reconnection times); p99 is of the transaction latencies pgbench logged (a 0.05 sample; excludes the connect in connect20, whose "average connection time" is the conn ms column). RSS is the sum of smaps_rollup Pss over every postgres process of user pgb (postmaster, backends, auxiliary processes; Pss counts a shared_buffers page once across them), sampled every 5 s plus the pass's own duration while pgbench runs (bench/probes/pg-rss-sampler.sh; the samples-per-run column says how many passes a run got): peak = the largest sample of the run, final = the last sample with at least 90% of the run's maximum backend count (a sample of 500 processes takes seconds and can straddle the row's end while the backends exit); Pss_Anon is the anonymous share of the same sums (the allocators' heaps and stacks; shared_buffers is Pss_Shmem and identical for every allocator); per-backend = the median over the backends of one backend's Pss / Pss_Anon at the final sample (the 100- and 500-connection points). Every RSS figure is the median over the timed runs, in MiB. Percentages are challoc against the other allocator: positive means challoc is slower (fewer tps), higher latency or larger.
- allocators (postmaster and one live backend checked in /proc/<pid>/maps before the rounds):
  - challoc: LD_PRELOAD=/src/target/postgres/lib/release/libchalloc.so (f419b88, sha256 9b9c9d89cb8039f3)

## Sorted scores (tps, higher is better)

| row | allocator | sorted tps | latency avg ms | p99 ms | conn ms |
|---|---|---|---:|---:|---:|
| select100 | challoc | 412953 424566 429952 431141 433159 | 0.233 | 0.448 | - |
| tpcb100 | challoc | 50999 51434 51590 53016 53176 | 1.938 | 4.631 | - |
| select500 | challoc | 345352 349530 359518 361161 363591 | 1.391 | 2.425 | - |
| connect20 | challoc | 1407 1438 1446 1448 1450 | 13.828 | 17.230 | 2.637 |

## Medians: throughput and latency

| row | challoc tps | lat ms | p99 ms |
|---|---:|---:|---:|
| select100 | 429952 | 0.233 | 0.448 |
| tpcb100 | 51590 | 1.938 | 4.631 |
| select500 | 359518 | 1.391 | 2.425 |
| connect20 | 1446 | 13.828 | 17.230 |

connect20, average connection time (ms, pgbench's line, median over the timed runs): challoc 2.637.

## Medians: RSS (MiB, sum of smaps_rollup Pss over every postgres process)

| row | allocator | peak Pss | final Pss | peak Pss_Anon | final Pss_Anon | backends at final | per-backend Pss | per-backend Pss_Anon | samples per run |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| select100 | challoc | 1042.0 | 1042.0 | 168.0 | 168.0 | 100 | 10.0 | 1.6 | 6 |
| tpcb100 | challoc | 1215.0 | 1215.0 | 180.5 | 180.4 | 100 | 11.8 | 1.8 | 11 |
| select500 | challoc | 1703.0 | 1700.5 | 823.8 | 823.8 | 500 | 3.3 | 1.6 | 4 |
| connect20 | challoc | 83.8 | 75.9 | 11.1 | 11.1 | 4 | 3.3 | 1.8 | 6 |

"samples per run" is the median number of sampler passes in a run: one pass reads smaps_rollup for every postgres process and, with 500 backends on a saturated host, takes tens of seconds, so the 500-client row has few complete samples and its peak and final are the same steady-state pass. The sampler records each pass's duration in its S lines (bench/probes/pg-rss-sampler.sh).

## challoc against glibc and jemalloc

| row |
|---|
| select100 |
| tpcb100 |
| select500 |
| connect20 |

Positive percentages mean challoc did worse than that allocator (fewer tps, higher latency, more RSS).

## Reading

challoc at main f419b88 on this VM, run right after `2026-09-20-x86_64-postgres-vmas.md` (the same tree with the metadata region's small tables carved from the object side) under the same load: the same-VM before/after of that file's appendix "The metadata region's mappings".
