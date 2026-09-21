# PostgreSQL 17 benchmark rows, 2026-09-20, aarch64

- commit: f419b88 (uncommitted changes in src/)
- date: 2026-09-20T23:39:15Z
- host: Neoverse-V2, 16 CPUs visible to the container; GCE c4a-standard-16, lane fork-vmas VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- server: postgres (PostgreSQL) 17.10, pgbench (PostgreSQL) 17.10 (Wolfi postgresql-17 17.10); PGDATA target/postgres/data (initdb --locale=C -E UTF8, OS and database user pgb, trust auth on a unix socket), started by `pg_ctl start` as pgb with the allocator in its environment (LD_PRELOAD), so the postmaster and every backend and auxiliary process it forks run on it; pgbench (the client, its -j threads) runs as root on glibc malloc, the same binary for every allocator. Options on the command line: block_size=8192 checkpoint_timeout=1800 huge_pages=off jit=on max_connections=600 max_wal_size=8192 shared_buffers=131072 synchronous_commit=off (postgresql.conf untouched otherwise)
- data: pgbench -i -s 50 (5000000 accounts rows), database size 755 MB, loaded once and shared by every allocator (the data files do not depend on the server's allocator)
- rows: select100 = `pgbench -S -c 100 -j 16 -T 60` (select-only); tpcb100 = `pgbench -c 100 -j 16 -T 60` (TPC-B); select500 = `pgbench -S -c 500 -j 16 -T 60` (select-only, max_connections 600); connect20 = `pgbench -C -c 20 -j 4 -T 30` (a new connection per transaction: fork, backend start-up, one TPC-B transaction, exit); all with pgbench's default pre-run vacuum of pgbench_branches/tellers and truncate of pgbench_history (outside the timed window) and -l --sampling-rate=0.05 for the latency percentiles
- load average at start: 2.78 1.78 0.99; at end: 167.80 174.84 170.31; 3993 s total
- method: 5 timed rounds after one discarded warm-up, allocators interleaved per round (one server start per allocator per round, the rows in the order above against it, then a fast stop), every run's tps listed sorted. tps and latency average are pgbench's lines (connect20's tps includes the reconnection times); p99 is of the transaction latencies pgbench logged (a 0.05 sample; excludes the connect in connect20, whose "average connection time" is the conn ms column). RSS is the sum of smaps_rollup Pss over every postgres process of user pgb (postmaster, backends, auxiliary processes; Pss counts a shared_buffers page once across them), sampled every 5 s plus the pass's own duration while pgbench runs (bench/probes/pg-rss-sampler.sh; the samples-per-run column says how many passes a run got): peak = the largest sample of the run, final = the last sample with at least 90% of the run's maximum backend count (a sample of 500 processes takes seconds and can straddle the row's end while the backends exit); Pss_Anon is the anonymous share of the same sums (the allocators' heaps and stacks; shared_buffers is Pss_Shmem and identical for every allocator); per-backend = the median over the backends of one backend's Pss / Pss_Anon at the final sample (the 100- and 500-connection points). Every RSS figure is the median over the timed runs, in MiB. Percentages are challoc against the other allocator: positive means challoc is slower (fewer tps), higher latency or larger.
- allocators (postmaster and one live backend checked in /proc/<pid>/maps before the rounds):
  - challoc: LD_PRELOAD=/src/target/postgres/lib/release/libchalloc.so (working tree, HEAD f419b88 + uncommitted src changes, sha256 eb0843ba41c1b511)
  - glibc: no LD_PRELOAD (glibc 2.44 malloc)
  - jemalloc: LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1)

## Sorted scores (tps, higher is better)

| row | allocator | sorted tps | latency avg ms | p99 ms | conn ms |
|---|---|---|---:|---:|---:|
| select100 | challoc | 304096 307746 313025 313045 315543 | 0.319 | 0.620 | - |
| select100 | glibc | 309954 314813 315296 315429 315635 | 0.317 | 0.613 | - |
| select100 | jemalloc | 307434 308318 308819 310120 312720 | 0.324 | 0.626 | - |
| tpcb100 | challoc | 41419 41832 42016 42146 42313 | 2.380 | 5.507 | - |
| tpcb100 | glibc | 42031 42332 42493 42550 42612 | 2.353 | 5.451 | - |
| tpcb100 | jemalloc | 41422 41998 42014 42036 42038 | 2.380 | 5.495 | - |
| select500 | challoc | 258976 261104 263186 264334 267051 | 1.900 | 3.006 | - |
| select500 | glibc | 268050 270220 271263 272003 273601 | 1.843 | 2.842 | - |
| select500 | jemalloc | 267925 267967 268103 270111 270739 | 1.865 | 2.883 | - |
| connect20 | challoc | 1513 1523 1523 1524 1536 | 13.128 | 16.365 | 2.489 |
| connect20 | glibc | 1927 1941 2181 2229 2347 | 9.171 | 12.158 | 1.693 |
| connect20 | jemalloc | 2066 2075 2089 2091 2094 | 9.573 | 12.134 | 1.777 |

## Medians: throughput and latency

| row | challoc tps | lat ms | p99 ms | glibc tps | lat ms | p99 ms | jemalloc tps | lat ms | p99 ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| select100 | 313025 | 0.319 | 0.620 | 315296 | 0.317 | 0.613 | 308819 | 0.324 | 0.626 |
| tpcb100 | 42016 | 2.380 | 5.507 | 42493 | 2.353 | 5.451 | 42014 | 2.380 | 5.495 |
| select500 | 263186 | 1.900 | 3.006 | 271263 | 1.843 | 2.842 | 268103 | 1.865 | 2.883 |
| connect20 | 1523 | 13.128 | 16.365 | 2181 | 9.171 | 12.158 | 2089 | 9.573 | 12.134 |

connect20, average connection time (ms, pgbench's line, median over the timed runs): challoc 2.489; glibc 1.693; jemalloc 1.777.

## Medians: RSS (MiB, sum of smaps_rollup Pss over every postgres process)

| row | allocator | peak Pss | final Pss | peak Pss_Anon | final Pss_Anon | backends at final | per-backend Pss | per-backend Pss_Anon | samples per run |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| select100 | challoc | 1028.4 | 1028.4 | 168.4 | 168.4 | 100 | 9.9 | 1.7 | 7 |
| select100 | glibc | 1119.2 | 1118.0 | 252.9 | 252.9 | 100 | 10.8 | 2.5 | 7 |
| select100 | jemalloc | 1004.2 | 1003.3 | 139.3 | 139.3 | 100 | 9.7 | 1.4 | 7 |
| tpcb100 | challoc | 1167.5 | 1167.5 | 182.3 | 179.2 | 100 | 11.3 | 1.8 | 11 |
| tpcb100 | glibc | 1247.5 | 1247.5 | 258.2 | 253.7 | 100 | 12.1 | 2.5 | 11 |
| tpcb100 | jemalloc | 1139.3 | 1139.3 | 161.0 | 148.1 | 100 | 11.1 | 1.5 | 11 |
| select500 | challoc | 1696.4 | 1692.7 | 825.7 | 825.7 | 500 | 3.3 | 1.6 | 3 |
| select500 | glibc | 2105.6 | 2105.6 | 1231.9 | 1231.9 | 500 | 4.1 | 2.5 | 3 |
| select500 | jemalloc | 1551.5 | 1551.5 | 677.7 | 677.7 | 500 | 3.0 | 1.4 | 3 |
| connect20 | challoc | 77.2 | 75.6 | 11.6 | 11.5 | 4 | 3.2 | 1.8 | 6 |
| connect20 | glibc | 81.7 | 77.2 | 14.1 | 14.1 | 5 | 4.0 | 2.5 | 6 |
| connect20 | jemalloc | 82.7 | 73.7 | 10.7 | 10.7 | 5 | 2.8 | 1.4 | 6 |

"samples per run" is the median number of sampler passes in a run: one pass reads smaps_rollup for every postgres process and, with 500 backends on a saturated host, takes tens of seconds, so the 500-client row has few complete samples and its peak and final are the same steady-state pass. The sampler records each pass's duration in its S lines (bench/probes/pg-rss-sampler.sh).

## challoc against glibc and jemalloc

| row | vs glibc: tps | latency | p99 | peak Pss | final Pss | backend Pss | vs jemalloc: tps | latency | p99 | peak Pss | final Pss | backend Pss |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| select100 | +0.7% | +0.6% | +1.1% | -8.1% | -8.0% | -8.0% | -1.3% | -1.5% | -1.0% | +2.4% | +2.5% | +2.6% |
| tpcb100 | +1.1% | +1.1% | +1.0% | -6.4% | -6.4% | -6.4% | -0.0% | +0.0% | +0.2% | +2.5% | +2.5% | +2.6% |
| select500 | +3.1% | +3.1% | +5.8% | -19.4% | -19.6% | -19.8% | +1.9% | +1.9% | +4.3% | +9.3% | +9.1% | +9.5% |
| connect20 | +43.2% | +43.1% | +34.6% | -5.4% | -2.1% | -19.8% | +37.2% | +37.1% | +34.9% | -6.7% | +2.6% | +15.0% |

Positive percentages mean challoc did worse than that allocator (fewer tps, higher latency, more RSS).

## The metadata region's mappings (lane fork-vmas, 2026-09-20)

What this file measures: challoc with the metadata region's small tables
carved from the object side (`meta::EAGER_MAX` = 8 MiB, design 2; the
commit that introduced it is the `meta:` commit on `loop/fork-vmas`), so
that every table of at most 8 MiB shares the one `rw-p` mapping of the
heaps and caches instead of being a `rw-p` line with a `---p` tail of its
own, against glibc and jemalloc on the lane's own VM, with the same
settings and rounds as `2026-09-20-aarch64-postgres.md` (five allocators, the
lane-g VM of the same machine type). Numbers across the two files are
different VMs on different days; the like-for-like before/after is the
same-VM pair below.

Same VM, same rounds, challoc alone (`-postgres-vmas-main.md`, challoc at
main f419b88, run right after this file's rounds under the same load):

| row | main f419b88 (sorted tps) | conn ms | lat / p99 ms | this tree (sorted tps) | conn ms | lat / p99 ms | tps |
|---|---|---:|---:|---|---:|---:|---:|
| connect20 | 1480 1486 1487 1487 1507 | 2.553 | 13.452 / 16.400 | 1513 1523 1523 1524 1536 | 2.489 | 13.128 / 16.365 | +2.4% (disjoint lists) |
| select100 | 306650 312751 314093 314548 315808 | | 0.318 / 0.620 | 304096 307746 313025 313045 315543 | | 0.319 / 0.620 | -0.3% |
| tpcb100 | 41765 41913 41974 42141 42278 | | 2.382 / 5.517 | 41419 41832 42016 42146 42313 | | 2.380 / 5.507 | +0.1% |
| select500 | 259717 261401 263654 264085 265580 | | 1.896 / 3.005 | 258976 261104 263186 264334 267051 | | 1.900 / 3.006 | -0.2% |

Per-backend Pss / Pss_Anon (MiB, medians at the final sample): connect20
3.5 / 1.8 -> 3.2 / 1.8, select100 10.0 / 1.7 -> 9.9 / 1.7, select500 3.3 /
1.6 -> 3.3 / 1.6; peak Pss on select500 1704.9 -> 1696.4 MiB.

connect20 against glibc: +43.2% (glibc 2181 tps, a bimodal list 1927 1941
2181 2229 2347, against challoc 1523) where the 2026-09-20 file on the
lane-g VM had +53.2% (2096 against 1368); against jemalloc +37.2% (was
+42.7%). Of that movement the same-VM pair above attributes +2.4% of tps
and -2.5% of connection time (2.553 -> 2.489 ms) to this change; the rest
is the day's VM (challoc at main itself scored 1487 here against 1368
there, and glibc's spread is 20% wide).

A real backend on this VM (the server started under each library, one idle
backend read as the postgres user): main 462
mapping lines (`challoc:classes` 79, `challoc:meta` 251 of which 125
`rw-p`, `challoc:huge` 1, other 131), VmPTE 1084 KiB, metadata Rss 588
KiB; this tree 384 lines (classes 79, meta 173 of which 86 `rw-p`, huge
1, other 131), VmPTE 1092 KiB, metadata Rss 564 KiB. The postmaster went
from 393 to 341 lines. A 4-client `pgbench -C` in the same session:
2.184 -> 2.152 ms per connection.

Probe (`bench/probes/fork_maps.c`, dev container on both lane VMs, kernel
7.0.0-1011-gcp, 4 KiB pages, `vm.overcommit_memory` 0, `thp` `never`; a
parent that touched every class from 16 bytes to 256 KiB plus three huge
runs, 40 forks, the child allocating 512 KiB over the same sizes; the same
mapping counts on both architectures):

| | before (f419b88) | after (8 MiB cap) | glibc | jemalloc |
|---|---:|---:|---:|---:|
| child `/proc/self/maps` lines (aarch64 / x86_64) | 469 / 477 | 357 / 365 | 25 / 28 | 41 / 51 |
| of which `[anon:challoc:meta]` (rw / none) | 337 (168 / 169) | 225 (112 / 113) | - | - |
| of which `[anon:challoc:classes]` (rw / none) | 105 (52 / 53) | 105 (52 / 53) | - | - |
| metadata `rw-p` bytes | 2512 KiB | 86240 KiB | - | - |
| child Rss in the metadata region | 784 KiB | 756 KiB | - | - |
| child VmPTE, aarch64 / x86_64 | 1324 / 1332 KiB | 1336 / 1324 KiB | 120 / 124 KiB | 156 / 148 KiB |
| `fork()` median, aarch64 | 664-709 us | 601-626 us | 157-198 us | 292 us |
| `fork()` median, x86_64 | 849-897 us | 847-877 us | 342-373 us | 475 us |
| fork-to-exit median, aarch64 | 3.71-4.14 ms | 3.65-4.07 ms | 1.55-1.97 ms | 2.84 ms |
| fork-to-exit median, x86_64 | 3.98-4.24 ms | 4.13-4.37 ms | 2.69-2.96 ms | 3.32 ms |
| parent `mprotect` / `madvise` (strace, whole run, 1 fork; aarch64, x86_64 281) | 288 / 21 | 288 / 21 | 14 / 7 | - |
| child `mprotect` / `madvise` | 2 / 2 | 2 / 2 | 0 / 2 | - |

The 167 tables the parent reserved, by size (the gaps between the committed
heads, the same on both architectures): 5 of 100..200 KiB, 27 of 268..280
KiB, 5 of 392..780 KiB, 4 of 1 MiB, 1 of 1.5 MiB, 4 of 2 MiB, 1 of 3 MiB, 12
of 4 MiB, 48 of 8 MiB, 2 of 12.2 MiB, 8 of 16 MiB, 8 of 32 MiB, 3 of 32.5..49
MiB, 12 of 64 MiB, 8 of 128 MiB, 17 of 256 MiB, 5 of 512 MiB, 1 of 768 MiB, 3
of 1 GiB, 1 of 2 GiB; 15.2 GiB together on the reservation side of the 64 GiB
region (the object side's bump area was 128 KiB). The cap curve, built as
five variants of the same tree and run under the probe on both VMs (the same
parent, so the same 167 tables):

| `EAGER_MAX` | tables left on the reservation side | metadata `rw-p` lines | writable bytes | `fork()` aarch64 (two runs) | `fork()` x86_64 | fork-to-exit aarch64 | x86_64 | VmPTE |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 (before) | 167 | 168 | 2.5 MiB | 709 / 698 us | 850 / 849 us | 4.14 / 4.05 ms | 4.22 / 3.98 ms | 1328 KiB |
| 8 MiB (this change) | 111 | 112 | 84 MiB | 605-626 us | 863-877 us | 3.80-4.07 ms | 4.13-4.37 ms | 1336 KiB |
| 16 MiB | 66 | 67 | 453 MiB | 596 / 649 us | 801 / 941 us | 3.90 / 3.91 ms | 3.98 / 3.99 ms | 1328 KiB |
| 64 MiB | 47 | 48 | 952 MiB | 617 / 647 us | 833 / 891 us | 3.89 / 3.84 ms | 4.11 / 4.01 ms | 1336 KiB |
| 256 MiB | 26 | 27 | 2.9 GiB | 621 / 595 us | 865 / 819 us | 3.89 / 3.81 ms | 4.15 / 4.04 ms | 1340 KiB |
| everything | 0 | 1 | 15.2 GiB | 624 / 588 us | 879 / 884 us | 3.92 / 3.72 ms | 3.99 / 3.95 ms | 1324 KiB |
| glibc | - | - | - | 165-198 us | 342-464 us | 1.55-1.97 ms | 2.69-2.96 ms | 120 KiB |

Reading: the metadata VMA count is a minor part of the fork cost. Taking the
region from 168 committed ranges to one (15 GiB writable) recovers 10-15% of
`fork()` on aarch64 and nothing on x86_64, and fork-to-exit moves within its
run-to-run band on both, while VmPTE (the page tables the child's `fork`
copies and its exit tears down) stays at 1.3 MiB against glibc's 120 KiB at
every cap: the touched pages of the table heads are where they were, one
page per table, 2 MiB or more apart, each in its own page-table page, and
the 105 class-region lines and their 52 touched slabs are untouched by this
change (the decision to keep the per-class regions with guards). The 8 MiB
cap takes the tables whose tails cost nothing to make writable (82 MiB per
process, 56 of 167 tables) and leaves the small classes' 64 MiB..2 GiB
tables lazy: the exposure of a fully writable region (core dumps of a
private writable mapping with a page in it are the whole mapping; `mlockall`
populates it; `RLIMIT_DATA` counts it; a `thp=always` host would back each
touched head with a 2 MiB page) is not worth a 10% `fork()` on one
architecture. Idle RSS: the change adds no page (the metadata region's child
Rss fell from 784 to 756 KiB in the probe, 588 to 564 KiB in a real
backend); `stats.metadata` (`MetaStats.committed`, the `resident` identity)
for a small program goes from 946176 to 991232 bytes, +44 KiB of
page-alignment slack around the carved tables, and the new `writable`
figure on the `Metadata:` line is 87343104 (83 MiB, the carved tables and
the 64 KiB chunks) against 946176 before.

Syscalls: the parent's `mprotect` count is unchanged (288 on aarch64, 281 on
x86_64) because each carved table is one chunk commit where it was one head
commit before; the child's 2 `mprotect` are its own slab commits. The
`connect20` backend's 58 `mprotect` per connection in the earlier
attribution are slab commits in the class regions (a heap whose classes
start empty in the child), which this change does not touch; attaching
strace to the postmaster from inside the container is refused (no ptrace
over another user's process), so that count was not re-read here.
