# PostgreSQL 17 benchmark rows, 2026-09-20, aarch64

- commit: b01cd10
- date: 2026-09-20T13:18:29Z
- host: Neoverse-V2, 16 CPUs visible to the container; GCE c4a-standard-16 (Google Axion, Neoverse V2), zone us-east4-a
- kernel: 7.0.0-1011-gcp, container: Wolfi
- server: postgres (PostgreSQL) 17.10, pgbench (PostgreSQL) 17.10 (Wolfi postgresql-17 17.10); PGDATA target/postgres/data (initdb --locale=C -E UTF8, OS and database user pgb, trust auth on a unix socket), started by `pg_ctl start` as pgb with the allocator in its environment (LD_PRELOAD), so the postmaster and every backend and auxiliary process it forks run on it; pgbench (the client, its -j threads) runs as root on glibc malloc, the same binary for every allocator. Options on the command line: block_size=8192 checkpoint_timeout=1800 huge_pages=off jit=on max_connections=600 max_wal_size=8192 shared_buffers=131072 synchronous_commit=off (postgresql.conf untouched otherwise)
- data: pgbench -i -s 50 (5000000 accounts rows), database size 810 MB, loaded once and shared by every allocator (the data files do not depend on the server's allocator)
- rows: select100 = `pgbench -S -c 100 -j 16 -T 60` (select-only); tpcb100 = `pgbench -c 100 -j 16 -T 60` (TPC-B); select500 = `pgbench -S -c 500 -j 16 -T 60` (select-only, max_connections 600); connect20 = `pgbench -C -c 20 -j 4 -T 30` (a new connection per transaction: fork, backend start-up, one TPC-B transaction, exit); all with pgbench's default pre-run vacuum of pgbench_branches/tellers and truncate of pgbench_history (outside the timed window) and -l --sampling-rate=0.05 for the latency percentiles
- load average at start: 0.03 0.13 0.07; at end: 170.35 175.80 172.29; 6651 s total; tables re-rendered from the run's samples on 2026-09-20T15:34:28Z (REPORT_ONLY)
- method: 5 timed rounds after one discarded warm-up, allocators interleaved per round (one server start per allocator per round, the rows in the order above against it, then a fast stop), every run's tps listed sorted. tps and latency average are pgbench's lines (connect20's tps includes the reconnection times); p99 is of the transaction latencies pgbench logged (a 0.05 sample; excludes the connect in connect20, whose "average connection time" is the conn ms column). RSS is the sum of smaps_rollup Pss over every postgres process of user pgb (postmaster, backends, auxiliary processes; Pss counts a shared_buffers page once across them), sampled every 5 s plus the pass's own duration while pgbench runs (bench/probes/pg-rss-sampler.sh; the samples-per-run column says how many passes a run got): peak = the largest sample of the run, final = the last sample with at least 90% of the run's maximum backend count (a sample of 500 processes takes seconds and can straddle the row's end while the backends exit); Pss_Anon is the anonymous share of the same sums (the allocators' heaps and stacks; shared_buffers is Pss_Shmem and identical for every allocator); per-backend = the median over the backends of one backend's Pss / Pss_Anon at the final sample (the 100- and 500-connection points). Every RSS figure is the median over the timed runs, in MiB. Percentages are challoc against the other allocator: positive means challoc is slower (fewer tps), higher latency or larger.
- allocators (postmaster and one live backend checked in /proc/<pid>/maps before the rounds):
  - challoc: LD_PRELOAD=/src/target/postgres/lib/release/libchalloc.so (working tree, HEAD 1b3a6c5, sha256 9d2aacce25fe33dd)
  - jemalloc: LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1)
  - glibc: no LD_PRELOAD (glibc 2.44 malloc)
  - mimalloc: LD_PRELOAD=/src/target/bench-bin/libmimalloc.so (mimalloc v3.5.2, bench/build.sh)
  - tcmalloc: LD_PRELOAD=/usr/lib/libtcmalloc_minimal.so.4 (Wolfi tcmalloc-minimal 2.18.1, gperftools without profiler hooks)

## Sorted scores (tps, higher is better)

| row | allocator | sorted tps | latency avg ms | p99 ms | conn ms |
|---|---|---|---:|---:|---:|
| select100 | challoc | 309001 309395 311977 312875 313044 | 0.321 | 0.624 | - |
| select100 | jemalloc | 303052 306150 306653 307703 307784 | 0.326 | 0.630 | - |
| select100 | glibc | 309713 310971 310979 313045 313915 | 0.322 | 0.623 | - |
| select100 | mimalloc | 305114 307835 311863 312544 313764 | 0.321 | 0.621 | - |
| select100 | tcmalloc | 308795 309755 310768 311156 312143 | 0.322 | 0.623 | - |
| tpcb100 | challoc | 40366 40540 40553 41204 41448 | 2.466 | 5.785 | - |
| tpcb100 | jemalloc | 40203 40611 40688 40699 41402 | 2.458 | 5.781 | - |
| tpcb100 | glibc | 40832 41285 41492 41554 41841 | 2.410 | 5.732 | - |
| tpcb100 | mimalloc | 40709 41308 41330 41479 41803 | 2.420 | 5.717 | - |
| tpcb100 | tcmalloc | 40708 40944 41042 41092 41158 | 2.437 | 5.794 | - |
| select500 | challoc | 212481 213081 213132 218003 232648 | 2.346 | 3.954 | - |
| select500 | jemalloc | 222900 228826 229300 232147 248112 | 2.181 | 3.508 | - |
| select500 | glibc | 219885 222508 233111 233375 243403 | 2.145 | 3.534 | - |
| select500 | mimalloc | 222846 232586 238048 246010 253287 | 2.100 | 3.330 | - |
| select500 | tcmalloc | 230469 232214 234992 247532 262191 | 2.128 | 3.447 | - |
| connect20 | challoc | 1349 1365 1368 1377 1388 | 14.618 | 18.235 | 2.771 |
| connect20 | jemalloc | 1914 1926 1952 1994 1998 | 10.248 | 13.276 | 1.899 |
| connect20 | glibc | 1789 1815 2096 2117 2150 | 9.542 | 12.205 | 1.752 |
| connect20 | mimalloc | 1491 1505 1527 1567 1601 | 13.097 | 16.234 | 2.459 |
| connect20 | tcmalloc | 1896 1908 1935 1955 1993 | 10.337 | 12.735 | 1.921 |

## Medians: throughput and latency

| row | challoc tps | lat ms | p99 ms | jemalloc tps | lat ms | p99 ms | glibc tps | lat ms | p99 ms | mimalloc tps | lat ms | p99 ms | tcmalloc tps | lat ms | p99 ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| select100 | 311977 | 0.321 | 0.624 | 306653 | 0.326 | 0.630 | 310979 | 0.322 | 0.623 | 311863 | 0.321 | 0.621 | 310768 | 0.322 | 0.623 |
| tpcb100 | 40553 | 2.466 | 5.785 | 40688 | 2.458 | 5.781 | 41492 | 2.410 | 5.732 | 41330 | 2.420 | 5.717 | 41042 | 2.437 | 5.794 |
| select500 | 213132 | 2.346 | 3.954 | 229300 | 2.181 | 3.508 | 233111 | 2.145 | 3.534 | 238048 | 2.100 | 3.330 | 234992 | 2.128 | 3.447 |
| connect20 | 1368 | 14.618 | 18.235 | 1952 | 10.248 | 13.276 | 2096 | 9.542 | 12.205 | 1527 | 13.097 | 16.234 | 1935 | 10.337 | 12.735 |

connect20, average connection time (ms, pgbench's line, median over the timed runs): challoc 2.771; jemalloc 1.899; glibc 1.752; mimalloc 2.459; tcmalloc 1.921.

## Medians: RSS (MiB, sum of smaps_rollup Pss over every postgres process)

| row | allocator | peak Pss | final Pss | peak Pss_Anon | final Pss_Anon | backends at final | per-backend Pss | per-backend Pss_Anon | samples per run |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| select100 | challoc | 1036.4 | 1036.4 | 170.0 | 170.0 | 100 | 10.0 | 1.7 | 6 |
| select100 | jemalloc | 1007.7 | 1006.7 | 138.8 | 138.8 | 100 | 9.7 | 1.4 | 7 |
| select100 | glibc | 995.2 | 995.2 | 123.7 | 123.7 | 100 | 9.6 | 1.2 | 7 |
| select100 | mimalloc | 1788.2 | 1787.5 | 918.4 | 918.4 | 100 | 17.4 | 9.0 | 7 |
| select100 | tcmalloc | 1017.3 | 1015.9 | 145.8 | 145.8 | 100 | 9.8 | 1.4 | 6 |
| tpcb100 | challoc | 1171.2 | 1171.2 | 192.5 | 179.9 | 100 | 11.4 | 1.8 | 11 |
| tpcb100 | jemalloc | 1139.1 | 1139.1 | 148.1 | 148.0 | 100 | 11.1 | 1.4 | 11 |
| tpcb100 | glibc | 1265.0 | 1265.0 | 280.5 | 280.5 | 100 | 12.3 | 2.8 | 11 |
| tpcb100 | mimalloc | 1923.4 | 1923.4 | 923.3 | 923.3 | 100 | 18.8 | 9.1 | 11 |
| tpcb100 | tcmalloc | 1151.9 | 1151.9 | 158.1 | 156.9 | 100 | 11.2 | 1.5 | 11 |
| select500 | challoc | 1704.5 | 1704.5 | 833.6 | 833.6 | 500 | 3.3 | 1.7 | 3 |
| select500 | jemalloc | 1552.9 | 1552.9 | 675.4 | 675.4 | 500 | 3.0 | 1.3 | 3 |
| select500 | glibc | 1478.1 | 1478.1 | 596.1 | 596.1 | 500 | 2.9 | 1.2 | 3 |
| select500 | mimalloc | 5379.0 | 5379.0 | 4506.2 | 4506.2 | 500 | 10.6 | 9.0 | 3 |
| select500 | tcmalloc | 1575.2 | 1575.2 | 696.6 | 696.6 | 500 | 3.0 | 1.4 | 3 |
| connect20 | challoc | 77.9 | 77.3 | 13.3 | 13.1 | 5 | 3.3 | 1.8 | 6 |
| connect20 | jemalloc | 83.0 | 77.3 | 12.4 | 12.4 | 6 | 2.8 | 1.4 | 6 |
| connect20 | glibc | 84.0 | 80.1 | 13.2 | 13.2 | 6 | 2.8 | 1.3 | 6 |
| connect20 | mimalloc | 156.5 | 156.5 | 89.2 | 89.2 | 8 | 10.6 | 9.3 | 6 |
| connect20 | tcmalloc | 80.3 | 77.1 | 14.0 | 14.0 | 6 | 2.9 | 1.6 | 6 |

"samples per run" is the median number of sampler passes in a run: one pass reads smaps_rollup for every postgres process and, with 500 backends on a saturated host, takes tens of seconds, so the 500-client row has few complete samples and its peak and final are the same steady-state pass. The sampler records each pass's duration in its S lines (bench/probes/pg-rss-sampler.sh).

## challoc against glibc and jemalloc

| row | vs glibc: tps | latency | p99 | peak Pss | final Pss | backend Pss | vs jemalloc: tps | latency | p99 | peak Pss | final Pss | backend Pss | vs mimalloc: tps | latency | p99 | peak Pss | final Pss | backend Pss | vs tcmalloc: tps | latency | p99 | peak Pss | final Pss | backend Pss |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| select100 | -0.3% | -0.3% | +0.2% | +4.1% | +4.1% | +4.2% | -1.7% | -1.5% | -1.0% | +2.8% | +2.9% | +3.0% | -0.0% | +0.0% | +0.5% | -42.0% | -42.0% | -42.6% | -0.4% | -0.3% | +0.2% | +1.9% | +2.0% | +2.1% |
| tpcb100 | +2.3% | +2.3% | +0.9% | -7.4% | -7.4% | -7.6% | +0.3% | +0.3% | +0.1% | +2.8% | +2.8% | +2.9% | +1.9% | +1.9% | +1.2% | -39.1% | -39.1% | -39.5% | +1.2% | +1.2% | -0.2% | +1.7% | +1.7% | +1.7% |
| select500 | +9.4% | +9.4% | +11.9% | +15.3% | +15.3% | +15.9% | +7.6% | +7.6% | +12.7% | +9.8% | +9.8% | +10.3% | +11.7% | +11.7% | +18.7% | -68.3% | -68.3% | -68.9% | +10.3% | +10.2% | +14.7% | +8.2% | +8.2% | +8.9% |
| connect20 | +53.2% | +53.2% | +49.4% | -7.2% | -3.5% | +18.1% | +42.7% | +42.6% | +37.4% | -6.1% | +0.0% | +14.9% | +11.6% | +11.6% | +12.3% | -50.2% | -50.6% | -69.2% | +41.4% | +41.4% | +43.2% | -3.0% | +0.3% | +11.7% |

Positive percentages mean challoc did worse than that allocator (fewer tps, higher latency, more RSS).

## Reading

- Time, 100 clients: challoc is within 2.3% of every allocator on `select100` (312.0k tps against glibc 311.0k, jemalloc 306.7k) and `tpcb100` (40.6k against glibc 41.5k, -2.3%; jemalloc 40.7k). Latency and p99 follow (p99 within 1%). The lists overlap on both rows.
- `connect20` (a new backend per transaction), the row more than 10% behind: 1368 tps against glibc 2096 (-34.7%, i.e. glibc +53.2% over challoc) and jemalloc 1952 (-29.9%); the connection itself takes 2.771 ms against 1.752 (glibc) and 1.899 (jemalloc), and the first transaction of the fresh backend 14.6 ms against 9.5 / 10.2 (p99 18.2 against 12.2 / 13.3). mimalloc is in the same place (1527 tps, 2.459 ms), so it is the shape of a per-process heap, not a challoc bug. Attributed (mechanism: the fresh backend's heap is spread over 333 separate anonymous mappings in a 54 TiB span, and every connection pays for creating, faulting, protecting and tearing them down in the kernel):
  - A fresh challoc backend has 464 mappings (`[anon:challoc:classes]` 79, `[anon:challoc:meta]` 251, `[anon:challoc:huge]` 3, plus libchalloc.so) and a VmSize of 54.3 TiB (56,978,701,448 kB, the reserved class and metadata regions); the glibc backend has 128 mappings, one `[heap]`, VmSize 1.1 GiB. RssAnon is about the same (3.8 against 4.1 MiB).
  - Kernel work per connection (perf stat on the postmaster tree, 8 s of the `-C` run): challoc 51.05 s of task-clock over 9671 connections = 5.28 ms of CPU per connection and 1136 page faults per connection; glibc 59.78 s over 15,392 = 3.88 ms and 701 faults. +36% CPU and +62% page faults per connection, all of it kernel time: `libchalloc.so` is 2.24% of the postgres samples (glibc's whole libc.so.6, malloc included, is 4.03%; under challoc libc is 1.75%), the kernel 70.7% against 66.0%.
  - Syscalls per connection (strace -c, 5 s): challoc makes 58 `mprotect` calls per backend (138,685 in 2392 forks; glibc 0: its heap grows with `brk` and a handful of `mmap`), the slab commits of a heap whose classes start empty in the child; each `mprotect` takes the mmap write lock and splits or merges a VMA in the maple tree (`__vma_start_exclude_readers` 0.73%, `down_write`/`up_write` 1.0%, `mas_walk` 0.78% under challoc; absent or half under glibc). The `clone` (fork) itself takes 579 us against 237 us per call under strace (2.4x): `dup_mmap` copies 333 more VMAs and their anon_vma chains (`anon_vma_interval_tree_insert` 0.71% against 0.43%, `__anon_vma_interval_tree_remove` 0.67 against 0.35, `unlink_anon_vmas` 0.67 against none; kernel slab traffic `__memcg_slab_free_hook`/`post_alloc_hook` 2.05% against 1.32%).
  - Page faults and their teardown: the backend's first allocations zero-fill fresh slab pages in ~58 classes and copy-on-write the inherited metadata lines (`__pi_clear_page` 2.49% against 2.00, `__pi_copy_page` 1.12 against 0.94, `__rmqueue_pcplist` 1.04 against 0.47; scale challoc's shares by 1.36 for the absolute per-connection cost), and at exit the sparse page tables of 333 regions are torn down (`zap_pte_range` 1.52% against 0.64, `zap_pmd_range` 0.88 and `free_pud_range` 0.83 against nothing visible under glibc: each region sits in its own PTE/PMD/PUD pages, glibc's heap in one). `next_uptodate_folio` (fault-around on the postgres binary and libraries, the same absolute work) is 7.24% against 11.14% only because challoc's sample base per connection is bigger.
  - Not the fork handlers: `challoc::api::fork_release` is 0.17% of the samples (the walk over every class lock in `fork_prepare`/`fork_release` is a few microseconds; the child's `fork_child` closes two descriptors and clears the background-thread state). The top libchalloc symbols are `ClassState::release_owned` 0.81%, `operator delete[]` 0.76% (ICU's C++ in backend start-up), `release_slab` 0.36%, the flush's `partition_at_index`/`quicksort` 0.6%, `claim_with` 0.26%, `je_malloc` 0.22%: slab churn of a heap that starts empty in every backend, not the fast path.
- `select500`, the other row more than 10% behind on one measure: 213.1k tps against glibc 233.1k (glibc +9.4%; latency +9.4%, p99 +11.9%) and jemalloc 229.3k (+7.6%); on x86_64 the same row is +3.5% / +1.9%. Sorted lists overlap (challoc 212.5k .. 232.6k, glibc 219.9k .. 243.4k, jemalloc 222.9k .. 248.1k); the row is client-bound (pgbench's 16 threads and 500 backends on 16 CPUs, load 170-180, `vmstat` idle 0% under both allocators). Three same-session 30 s A/B pairs after the protocol: 209.3k against glibc 220.7k (-5.2%), 218.9k against 234.1k (-6.5%), and with the order swapped glibc 216.7k against challoc 221.5k (+2.2%). System-wide counters in the swapped pair (8 s): cycles 378.0G (challoc) against 394.7G (glibc), instructions 287.7G against 292.0G (162k against 168k instructions per transaction), L1-dcache-load-misses 3.55G against 3.62G, dTLB-load-misses 1.67G against 1.88G, page faults 1.75M against 1.72M, context switches 233k/s both, `madvise` 0 both, `mprotect` 4 against 110, `mmap` 20 against 195: the steady state costs the same instructions and misses per transaction under both, so no CPU-efficiency mechanism was found in the backends. What is allocator-attributable on this row is (a) the start of the 500 backends, 274 ms of "initial connection time" against 121 ms (the `connect20` mechanism, outside the timed window) and (b) memory: peak Pss 1704.5 MiB against 1478.1 (+15.3%), per-backend Pss 3.3 against 2.9 MiB (+16.2%), per-backend Pss_Anon 1.7 against 1.2 MiB (+0.5 MiB per backend: the fresh backend's `challoc:classes` regions hold 1.5 MiB of Rss and `challoc:meta` 0.6 MiB, against glibc's single 1.1 MiB `[heap]`; measured on x86_64, the same layout on both). Open item, owner: the coordinator with lane A (heap/arena layout): re-measure the 500-client row server-bound (fewer pgbench threads or a second machine for the client) before treating the 9.4% median as a throughput regression; the memory part is attributed above.
- RSS: on the select rows and `connect20` challoc's per-backend Pss_Anon is 1.7-1.8 MiB against glibc's 1.2-1.3 and jemalloc's 1.3-1.4 (the same +0.4-0.5 MiB per process as above: one committed slab per touched class in ~80 class regions plus the metadata lines), which is +4% of the Pss sum at 100 backends (1036 against 995 MiB on `select100`, shared_buffers dominating) and +15% at 500 (`select500`). On `tpcb100` glibc's peak is the largest (1265 MiB, per-backend Pss_Anon 2.8 MiB: glibc's arenas retain the TPC-B backends' freed memory; challoc 1171, jemalloc 1139). `connect20`'s Pss sums (78-84 MiB) are the postmaster and auxiliary processes plus the 4-6 backends alive at a sample. mimalloc's backends carry 9 MiB of Pss_Anon each (its per-thread segment reservation touched at start-up), 5.4 GiB of Pss at 500 backends.
- Sampling: a sampler pass reads `smaps_rollup` for every postgres process and takes tens of seconds at 500 backends on a saturated host, so the 500-client row has 3 passes per run (its peak and final are the same steady-state pass), the 100-client rows about 6 (every 10 s), `connect20` 6 (every 5 s). The first render of this file took the last pass, which straddled the backends' exit at the end of the row (301 of 500 backends counted); the tables were re-rendered from the run's raw samples with the last full pass (`REPORT_ONLY=1`, the harness commit that fixed it is 97f84f6) on 2026-09-20T15:34Z; the tps, latency and p99 columns are the original scores.

Attribution method (2026-09-20, on the VM host, both architectures): after the protocol run, one row at a time was replayed in the same container image over the same PGDATA (`pgbench -C -c 20 -j 4 -T 30` under challoc and under glibc; `pgbench -S -c 500 -j 16 -T 30` on aarch64) while the host attached `sudo perf stat --inherit -p <postmaster>` (task-clock, page-faults, context-switches, 8 s), `sudo perf record -F 1999 -g -a` (8 s; symbols through `--symfs` filled with the container's `libchalloc.so`, `postgres` and `libc.so.6`, as `bench/gce/perf-record.sh` does; kernel symbols from the host's kallsyms), `sudo strace -f -c -p <postmaster>` (5 s; counts the syscalls of the postmaster and of every backend it forked in the window) and, for the 500-client row, `perf stat -a` over the whole machine (software events and the `syscalls:sys_enter_*` tracepoints, then cycles/instructions/L1-dcache/dTLB misses) beside `vmstat 1`. A fresh backend's `/proc/<pid>/maps` and `smaps` were read as the postgres user (`challoc:classes|meta|huge` are challoc's named anonymous regions). `MALLOC_CONF=stats_print:true` is accepted by challoc but "has no effect (not implemented)", so the heap shape comes from smaps. Percentages below are shares of all samples of comm `postgres` (postmaster, backends, auxiliary processes) in the 8 s window; to compare absolute per-connection cost, scale challoc's shares by its higher CPU per connection (the factor given).
