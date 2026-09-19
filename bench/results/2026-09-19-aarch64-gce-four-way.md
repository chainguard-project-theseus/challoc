# challoc comparison matrix (four-way), 2026-09-19, aarch64

- commit: 1339eb6
- date: 2026-09-19T03:44:43Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (GCE c4a-standard-16 (Google Axion, Neoverse V2), zone us-east4-a), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- allocators (same binaries, LD_PRELOAD only; challoc from the commit above):
  - challoc: 1339eb6ee395 built into target/ab/1339eb6ee395
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)
  - glibc: glibc malloc (no LD_PRELOAD, glibc 2.44)
  - mimalloc: /src/target/bench-bin/libmimalloc.so (v3.5.2)
  - tcmalloc: Wolfi's /usr/lib/libtcmalloc_minimal.so.4 (tcmalloc-minimal 2.18.1, gperftools without profiler hooks)
- Ruby rows: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh with Wolfi's flags, --enable-yjit configured but YJIT not enabled at run time), installed under target/ruby/install-{challoc,glibc}
- Python rows: Python 3.13.15 (Wolfi python-3.13 3.13.15_git20260918, pymalloc built in); modes: default = PYTHONMALLOC unset (pymalloc for objects under 512 bytes, the allocator sees the rest), malloc = PYTHONMALLOC=malloc (every object through the allocator)
- method: every cell comes from one run per VM on an idle VM (load under 1 at the start): the Phase 0 set and the Valkey model rows from one `bench/ab.sh` run (5 timed rounds after a warm-up, variants interleaved per round, median of the sorted scores, median peak RSS from bench/rt); the Ruby rows from one `bench/ruby-bench.sh` run and the Python rows from one `bench/python-bench.sh` run (same protocol, wall seconds); the container rows from `make test-cgroup` (challoc) and `make test-cgroup-compare CGROUP_ALLOCS='jemalloc mimalloc tcmalloc'` (one memory-limited container per run, RSS 2 s after freeing the churn, and the 300 s oscillation's verdict). Positive percentages mean challoc did worse than that allocator (slower, lower throughput, or more peak RSS); time is the score's direction (throughput rows compare throughput). The Valkey soak (`make valkey-soak`) is jemalloc-only: it drives Valkey's own `je_` allocator API (`MEMORY MALLOC-STATS`, active defrag, `jemalloc_purge`), which no other allocator has, so it is not a row here; its latest comparison is in the scorecard's soak columns.
- sources: `2026-09-19-aarch64-gce-four-way-ab.md`, `2026-09-19-aarch64-ruby.md`, `2026-09-19-aarch64-python.md`, the container logs (OUTCOME lines quoted in section 3)

## 1. The table

| group | row | unit | challoc | jemalloc | glibc | mimalloc | tcmalloc | vs jemalloc (time / RSS) | vs glibc (time / RSS) | vs mimalloc (time / RSS) | vs tcmalloc (time / RSS) |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Phase 0 set | larsonN | M ops/s | 586.6 (167.3 MiB) | 611.8 (206.2 MiB) | 477.6 (191.8 MiB) | 585.7 (189.3 MiB) | 595.5 (133.3 MiB) | +4.3% / -18.9% | -18.6% / -12.8% | -0.2% / -11.6% | +1.5% / +25.5% |
| Phase 0 set | xmallocN | M frees/s | 521.820 (106.1 MiB) | 478.178 (109.1 MiB) | 35.479 (76.2 MiB) | 167.386 (65.8 MiB) | 2.038 (42.6 MiB) | -8.4% / -2.7% | -93.2% / +39.2% | -67.9% / +61.2% | -99.6% / +149.1% |
| Phase 0 set | cfrac | s | 4.449 (2.8 MiB) | 4.425 (4.9 MiB) | 4.306 (2.6 MiB) | 4.269 (3.9 MiB) | 4.384 (8.0 MiB) | +0.5% / -42.9% | +3.3% / +7.7% | +4.2% / -28.2% | +1.5% / -65.0% |
| Phase 0 set | espresso | s | 3.668 (3.6 MiB) | 3.740 (6.2 MiB) | 3.897 (3.8 MiB) | 3.546 (9.9 MiB) | 3.576 (8.8 MiB) | -1.9% / -41.9% | -5.9% / -5.3% | +3.4% / -63.6% | +2.6% / -59.1% |
| Phase 0 set | mstress | s | 0.377 (317.0 MiB) | 0.647 (291.6 MiB) | 0.493 (528.3 MiB) | 0.290 (535.7 MiB) | 0.422 (280.2 MiB) | -41.7% / +8.7% | -23.5% / -40.0% | +30.0% / -40.8% | -10.7% / +13.1% |
| Phase 0 set | alloc-test1 | s | 2.614 (13.0 MiB) | 2.515 (12.3 MiB) | 3.025 (15.2 MiB) | 2.428 (15.8 MiB) | 2.492 (15.6 MiB) | +3.9% / +5.7% | -13.6% / -14.5% | +7.7% / -17.7% | +4.9% / -16.7% |
| Phase 0 set | alloc-testN | s | 2.254 (16.4 MiB) | 2.206 (16.5 MiB) | 2.541 (34.4 MiB) | 2.137 (61.7 MiB) | 2.197 (17.9 MiB) | +2.2% / -0.6% | -11.3% / -52.3% | +5.5% / -73.4% | +2.6% / -8.4% |
| Phase 0 set | glibc-simple | s | 2.291 (2.2 MiB) | 2.528 (4.4 MiB) | 4.158 (3.4 MiB) | 1.751 (3.5 MiB) | 1.729 (7.5 MiB) | -9.4% / -50.0% | -44.9% / -35.3% | +30.8% / -37.1% | +32.5% / -70.7% |
| Phase 0 set | redis | k req/s | 501.3 (29.0 MiB) | 410.7 (31.1 MiB) | 533.0 (31.4 MiB) | 465.8 (45.4 MiB) | 521.9 (34.1 MiB) | -18.1% / -6.8% | +6.3% / -7.6% | -7.1% / -36.1% | +4.1% / -15.0% |
| Valkey model | valkey1 | M ops/s | 7.110 (584.6 MiB) | 7.245 (585.6 MiB) | 6.037 (568.5 MiB) | 5.569 (585.7 MiB) | 8.506 (587.1 MiB) | +1.9% / -0.2% | -15.1% / +2.8% | -21.7% / -0.2% | +19.6% / -0.4% |
| Valkey model | valkey1-free | M ops/s | 7.162 (584.5 MiB) | 7.040 (585.6 MiB) | 6.151 (568.4 MiB) | 5.429 (585.7 MiB) | 8.547 (587.2 MiB) | -1.7% / -0.2% | -14.1% / +2.8% | -24.2% / -0.2% | +19.3% / -0.5% |
| Valkey model | valkey1-usable | M ops/s | 5.989 (584.4 MiB) | 5.712 (585.5 MiB) | 2.751 (568.9 MiB) | 3.746 (585.6 MiB) | 5.101 (586.7 MiB) | -4.6% / -0.2% | -54.1% / +2.7% | -37.5% / -0.2% | -14.8% / -0.4% |
| Ruby 3.4 | ruby startup | s | 0.040 (13.2 MiB) | 0.038 (14.6 MiB) | 0.038 (11.6 MiB) | 0.037 (12.8 MiB) | 0.039 (17.5 MiB) | +5.3% / -9.6% | +5.3% / +13.8% | +8.1% / +3.1% | +2.6% / -24.6% |
| Ruby 3.4 | ruby so_binary_trees | s | 4.686 (26.9 MiB) | 4.671 (28.2 MiB) | 4.677 (25.3 MiB) | 4.670 (26.1 MiB) | 4.682 (31.1 MiB) | +0.3% / -4.6% | +0.2% / +6.3% | +0.3% / +3.1% | +0.1% / -13.5% |
| Ruby 3.4 | ruby hash_aref_str | s | 0.353 (9.8 MiB) | 0.349 (11.2 MiB) | 0.344 (8.9 MiB) | 0.344 (9.5 MiB) | 0.353 (14.3 MiB) | +1.1% / -12.5% | +2.6% / +10.1% | +2.6% / +3.2% | +0.0% / -31.5% |
| Ruby 3.4 | ruby array_sample_100k__1k | s | 0.428 (32.6 MiB) | 0.427 (34.0 MiB) | 0.434 (28.7 MiB) | 0.434 (49.1 MiB) | 0.426 (35.2 MiB) | +0.2% / -4.1% | -1.4% / +13.6% | -1.4% / -33.6% | +0.5% / -7.4% |
| Ruby 3.4 | ruby array_sort_block | s | 5.311 (29.9 MiB) | 5.322 (31.5 MiB) | 5.256 (28.0 MiB) | 5.315 (47.1 MiB) | 5.316 (34.2 MiB) | -0.2% / -5.1% | +1.0% / +6.8% | -0.1% / -36.5% | -0.1% / -12.6% |
| Ruby 3.4 | ruby so_nbody | s | 0.824 (10.1 MiB) | 0.822 (11.3 MiB) | 0.852 (9.0 MiB) | 0.821 (9.6 MiB) | 0.825 (14.6 MiB) | +0.2% / -10.6% | -3.3% / +12.2% | +0.4% / +5.2% | -0.1% / -30.8% |
| Ruby 3.4 | ruby app_fib | s | 0.292 (9.8 MiB) | 0.291 (11.2 MiB) | 0.290 (8.9 MiB) | 0.290 (9.5 MiB) | 0.291 (14.3 MiB) | +0.3% / -12.5% | +0.7% / +10.1% | +0.7% / +3.2% | +0.3% / -31.5% |
| Ruby 3.4 | ruby erb_render | s | 0.835 (11.4 MiB) | 0.967 (12.8 MiB) | 0.862 (9.8 MiB) | 0.843 (10.9 MiB) | 0.757 (15.7 MiB) | -13.7% / -10.9% | -3.1% / +16.3% | -0.9% / +4.6% | +10.3% / -27.4% |
| Ruby 3.4 | ruby gem_list | s | 0.115 (26.3 MiB) | 0.112 (27.4 MiB) | 0.115 (24.9 MiB) | 0.110 (25.9 MiB) | 0.111 (30.5 MiB) | +2.7% / -4.0% | +0.0% / +5.6% | +4.5% / +1.5% | +3.6% / -13.8% |
| Ruby 3.4 | ruby gc_churn | s | 1.612 (382.2 MiB) | 1.630 (361.4 MiB) | 1.528 (316.0 MiB) | 1.637 (354.4 MiB) | 1.560 (334.2 MiB) | -1.1% / +5.8% | +5.5% / +20.9% | -1.5% / +7.8% | +3.3% / +14.4% |
| Python 3.13 | python json-default | s | 4.012 (74.7 MiB) | 4.046 (83.2 MiB) | 3.945 (68.1 MiB) | 3.977 (113.7 MiB) | 4.015 (90.1 MiB) | -0.8% / -10.2% | +1.7% / +9.7% | +0.9% / -34.3% | -0.1% / -17.1% |
| Python 3.13 | python json-malloc | s | 4.284 (75.5 MiB) | 4.226 (75.1 MiB) | 4.269 (71.3 MiB) | 3.987 (93.1 MiB) | 4.158 (89.6 MiB) | +1.4% / +0.5% | +0.4% / +5.9% | +7.4% / -18.9% | +3.0% / -15.7% |
| Python 3.13 | python dict_list-default | s | 3.310 (231.6 MiB) | 3.359 (236.4 MiB) | 3.290 (217.3 MiB) | 3.311 (256.8 MiB) | 3.289 (234.2 MiB) | -1.5% / -2.0% | +0.6% / +6.6% | -0.0% / -9.8% | +0.6% / -1.1% |
| Python 3.13 | python dict_list-malloc | s | 4.712 (232.4 MiB) | 4.581 (243.2 MiB) | 4.780 (276.2 MiB) | 3.666 (263.0 MiB) | 4.016 (231.3 MiB) | +2.9% / -4.4% | -1.4% / -15.9% | +28.5% / -11.6% | +17.3% / +0.5% |
| Python 3.13 | python regex-default | s | 3.495 (68.8 MiB) | 3.486 (71.9 MiB) | 3.500 (63.5 MiB) | 3.497 (101.4 MiB) | 3.483 (75.8 MiB) | +0.3% / -4.3% | -0.1% / +8.3% | -0.1% / -32.1% | +0.3% / -9.2% |
| Python 3.13 | python regex-malloc | s | 3.565 (72.6 MiB) | 3.536 (71.8 MiB) | 3.566 (70.4 MiB) | 3.384 (89.2 MiB) | 3.489 (74.8 MiB) | +0.8% / +1.1% | -0.0% / +3.1% | +5.3% / -18.6% | +2.2% / -2.9% |
| Python 3.13 | python object_graph-default | s | 2.868 (195.5 MiB) | 2.893 (195.5 MiB) | 2.928 (193.0 MiB) | 2.851 (222.2 MiB) | 2.852 (201.4 MiB) | -0.9% / +0.0% | -2.0% / +1.3% | +0.6% / -12.0% | +0.6% / -2.9% |
| Python 3.13 | python object_graph-malloc | s | 3.003 (195.2 MiB) | 2.818 (195.9 MiB) | 2.713 (203.9 MiB) | 2.452 (201.1 MiB) | 2.998 (197.6 MiB) | +6.6% / -0.4% | +10.7% / -4.3% | +22.5% / -2.9% | +0.2% / -1.2% |
| container | churn 512m: RSS 2 s after freeing 400 MiB | MiB kept | 12 (403 MiB) | 195 (409 MiB) | - | 407 (407 MiB) | 408 (408 MiB) | -93.8% / -1.5% | - / - | -97.1% / -1.0% | -97.1% / -1.2% |
| container | churn 256m: RSS 2 s after freeing 200 MiB | MiB kept | 8 (202 MiB) | 124 (206 MiB) | - | 207 (207 MiB) | 207 (207 MiB) | -93.5% / -1.9% | - / - | -96.1% / -2.4% | -96.1% / -2.4% |
| container | oscillate 512m: 200..480 MiB live, 300 s | verdict | ok, 98 cycles (501 MiB) | OOM-killed | - | OOM-killed | OOM-killed | - / - | - / - | - / - | - / - |

Units: larsonN and valkey rows M ops/s, xmallocN M frees/s, redis k req/s (higher is better); the other Phase 0, Ruby and Python rows wall seconds (lower is better); container churn rows MiB of RSS kept 2 s after the frees (lower is better), the oscillation row its verdict and cycle count. RSS in parentheses is bench/rt's peak (MiB) of the measured process (the churn rows: the program's own RSS peak).

## 2. Rows where challoc is more than 10% behind an allocator

- Phase 0 set / larsonN: challoc RSS +25.5% against tcmalloc
- Phase 0 set / xmallocN: challoc RSS +39.2% against glibc
- Phase 0 set / xmallocN: challoc RSS +61.2% against mimalloc
- Phase 0 set / xmallocN: challoc RSS +149.1% against tcmalloc
- Phase 0 set / mstress: challoc time +30.0% against mimalloc
- Phase 0 set / mstress: challoc RSS +13.1% against tcmalloc
- Phase 0 set / glibc-simple: challoc time +30.8% against mimalloc
- Phase 0 set / glibc-simple: challoc time +32.5% against tcmalloc
- Valkey model / valkey1: challoc time +19.6% against tcmalloc
- Valkey model / valkey1-free: challoc time +19.3% against tcmalloc
- Ruby 3.4 / ruby startup: challoc RSS +13.8% against glibc
- Ruby 3.4 / ruby hash_aref_str: challoc RSS +10.1% against glibc
- Ruby 3.4 / ruby array_sample_100k__1k: challoc RSS +13.6% against glibc
- Ruby 3.4 / ruby so_nbody: challoc RSS +12.2% against glibc
- Ruby 3.4 / ruby app_fib: challoc RSS +10.1% against glibc
- Ruby 3.4 / ruby erb_render: challoc RSS +16.3% against glibc
- Ruby 3.4 / ruby erb_render: challoc time +10.3% against tcmalloc
- Ruby 3.4 / ruby gc_churn: challoc RSS +20.9% against glibc
- Ruby 3.4 / ruby gc_churn: challoc RSS +14.4% against tcmalloc
- Python 3.13 / python dict_list-malloc: challoc time +28.5% against mimalloc
- Python 3.13 / python dict_list-malloc: challoc time +17.3% against tcmalloc
- Python 3.13 / python object_graph-malloc: challoc time +10.7% against glibc
- Python 3.13 / python object_graph-malloc: challoc time +22.5% against mimalloc

Attribution (plan Phase 2.5 task 1 acceptance: every row above has one before anything is changed for it):

The mechanisms, named once and referenced per row (all measured on this VM after the table's run, allocators under the same binaries; counters are `perf stat` on the host attached to the container's process, symbol shares are `perf record -F 4000` resolved against the build's `nm` map, RSS breakdowns are the peak sample of `/proc/PID/smaps` grouped by mapping name, challoc's regions carrying their `PR_SET_VMA_ANON_NAME` names on this 7.0 kernel; raw files under the VM's `~/logs/attr/`, copied to the iteration's `.tmp-vmlogs/`):

- M1, fast-path instruction count: challoc's `malloc`/`free`/`sdallocx` execute more instructions per operation than jemalloc, tcmalloc and mimalloc with equal or fewer L1D and dTLB misses (the dependent-load chain GOT -> TLS slot -> heap -> cache -> len -> slot, the sized-free and usable-size chains; known since Phase 1 and Phase 1.5).
- M2, the compacting flush's slot sort: when a per-class cache overflows, the flush sorts the slot array by address (`core::slice::sort::unstable::quicksort::<*mut u8>` with `median3_rec`/`ipnsort`) before handing the slots back (Phase 1.5's compacting cache). For tiny objects at a high malloc/free rate this is 8..16% of the whole process, a third of challoc's own time. This is the one mechanism in the list that is new to the record.
- M3, retention until decay: freed memory stays committed until `dirty_decay_ms` (10 s) has elapsed and a tick runs; a process or phase shorter than that (larsonN and xmallocN run 5 s, mstress 0.4..0.6 s, gc_churn 1.6 s, one json round 0.1 s) never purges, and cross-thread frees leave the freeing thread's cache full of the allocating thread's slots. The huge tier keeps freed runs the same way, where glibc `munmap`s at once above its mmap threshold and tcmalloc returns spans to its page heap.
- M4, fixed footprint: the metadata region (superblock, per-arena class state, tables: 0.6 MiB touched in a 3 MiB process) plus one touched page per (arena, class) slab: 0.8..1.6 MiB, visible only on 3..14 MiB processes.
- M5, kernel time: commit (`mprotect`) and purge (`madvise`) under `mmap_lock` on a short multi-threaded process; equal cycles with a longer wall time is idle time.
- M6, init: the reservation (three `mmap`s of 53 TiB, 57 `mseal` calls, the superblock and 17 arenas' state) on a 40 ms process.
- M7, the 17 arenas' partial and Empty slabs on a 0.4..0.6 s process (mstress): the accepted design decision of Phase 1.5 task 15 and acceptance run 3, unchanged here.

Rows on this VM (aarch64):

- larsonN RSS +25.5% against tcmalloc (167 vs 133 MiB): M3. Peak smaps: `challoc:classes` 146.9 MiB + `challoc:meta` 6.8 MiB (allocator-owned 154 MiB) against tcmalloc's `[heap]` 58.5 MiB; the 58..60 MiB of unnamed anonymous memory is larson's exited threads' stacks and is the same under every allocator (glibc holds 180 MiB, mimalloc 122, jemalloc ~134 of allocator memory). Live data is under 16 MiB (16 threads x 1000 blocks x <= 1000 B); the rest is remote-freed slots in 16 caches and Empty slabs that a 5 s run never decays.
- xmallocN RSS +39.2% / +61.2% / +149.1% against glibc / mimalloc / tcmalloc (106 vs 76 / 66 / 43 MiB): M3 with cross-thread frees (xmalloc-test is producer/consumer). `challoc:classes` 107.9 MiB + `meta` 5.1 against glibc 71 (62.3 anon + 8.9 heap), mimalloc 72, tcmalloc 38.4; jemalloc holds 106.9 MiB, the same as challoc.
- mstress time +30.0% against mimalloc (0.377 vs 0.290 s): M5 + M2 + M1. Whole-run counters: 8.55 vs 7.15 G cycles, 18.4 vs 14.9 G instructions, L1D misses 90 vs 101 M, dTLB misses 52 vs 2 M. perf record: libchalloc 17.7% of samples (1.5 G cycles) against libmimalloc 16.2% (1.16 G); kernel 7.6% (0.65 G) against 1.7% (0.12 G); inside challoc the slot sort 4.0%, `free` 3.7%, `malloc` 1.9%, LSE atomics (`ldclr8_rel`, `ldadd4_acq_rel`, `swp4_rel`) 1.8%.
- mstress RSS +13.1% against tcmalloc (317 vs 280 MiB; +8.7% against jemalloc): M7. Peak smaps: `classes` 163 MiB + `huge` 157 MiB + `meta` 11 MiB.
- glibc-simple time +30.8% / +32.5% against mimalloc / tcmalloc (2.291 vs 1.751 / 1.729 s): M2 + M1. Whole-run counters: 6.89 vs 5.24 vs 5.10 G cycles, 37.5 vs 28.0 vs 31.6 G instructions, misses negligible for all three (L1D 1..10 M, dTLB <= 1 M). perf record: libchalloc 46.4% of samples (3.2 G cycles) against libmimalloc 33.0% (1.7 G); inside challoc `free` 17.4%, `malloc` 17.1%, the slot sort with its pivot and ipnsort helpers 9.2% of the whole process.
- valkey1 +19.6% and valkey1-free +19.3% against tcmalloc (7.110 / 7.162 vs 8.506 / 8.547 M ops/s): M1. Per-operation counters over the whole run (both phases): valkey1 229.8 vs 215.2 cycles/op, 256 vs 175 instructions/op, L1D 3.09 vs 4.32 misses/op, dTLB 2.90 vs 4.14; valkey1-free 223.1 vs 215.7 cycles/op, 236 vs 175 instructions/op. challoc executes 60..80 more instructions per SET (malloc of the new value and object, free of the old pair) with fewer misses; the whole-run per-op gap (3..7%) is smaller than the write-phase gap the table scores, so the difference sits in the write phase (heap full, every SET an allocate-and-free pair); the delete phase is at parity (valkey1-usable: challoc ahead).
- Ruby startup, hash_aref_str, array_sample_100k__1k, so_nbody, app_fib, erb_render RSS +10..16% against glibc (0.9..1.6 MiB on 9..13 MiB processes): M4. Peak smaps of `ruby -e 1` on the same binary: `challoc:classes` 3.4 MiB + `meta` against glibc's `[heap]` 3.1 MiB; 13.0 vs 11.5 MiB in total.
- Ruby erb_render time +10.3% against tcmalloc (0.835 vs 0.757 s; the five-run lists 0.825..0.839 and 0.750..0.759 do not overlap): M1 with a dTLB component. One `perf stat` over benchmark-driver plus the measured ruby: 2.63 vs 2.60 G cycles, 10.5 vs 9.8 G instructions (+7%), L1D misses 15 vs 13 M, dTLB misses 78 vs 62 M (+26%). erb_render grows short strings (`realloc` on `String#<<`) at a high rate; tcmalloc is also 12% ahead of jemalloc and mimalloc on this row. The counter run's cycle gap (1%) is smaller than the wall gap, so this row wants a dedicated `perf record` before anything is changed for it.
- Ruby gc_churn RSS +20.9% / +14.4% against glibc / tcmalloc (382 vs 316 / 334 MiB): known from Phase 1.5 task 4b (the `--without-jemalloc` Ruby calls `malloc_trim` after every full GC, which only glibc honours) plus M3: peak smaps `challoc:classes` 372 MiB; the bursts' short-lived buffers are freed by sweep and their slabs wait out the 10 s decay in a 1.6 s process.
- Python dict_list-malloc +28.5% / +17.3% against mimalloc / tcmalloc, object_graph-malloc +22.5% / +10.7% against mimalloc / glibc: M2 + M1 (every Python object through the allocator; 16..64-byte objects at a few million operations per second). Whole-run counters, dict_list-malloc, challoc / mimalloc / tcmalloc / jemalloc / glibc: 13.87 / 10.96 / 11.85 / 13.57 / 14.20 G cycles, 65.0 / 46.0 / 52.2 / 60.6 / 64.3 G instructions, L1D misses 155 / 306 / 209 / 153 / 236 M (mimalloc misses twice as often and still wins on instructions). perf record: libchalloc 42.4% of samples (5.9 G cycles) against libmimalloc 25.1% (2.75 G); inside challoc the slot sort 14.6% + pivot/ipnsort 1.0% of the whole process (2.2 G cycles), `free` 12.7%, `malloc` 7.2%, `realloc` 0.4%. object_graph-malloc: 9.12 / 7.29 (mimalloc) / 8.07 (glibc) G cycles, 9.6 / 8.0 / 9.0 G instructions. The pymalloc rows are at parity (dict_list-default 9.81 vs 9.87 G cycles), so the whole gap is the small-object path.

What follows (no code changes in this task, per the plan): M2 is the actionable finding, a fast-path candidate for a later task with the rule of CLAUDE.md (a per-operation attribution on the workload it helps, here `glibc-simple`, `python dict_list-malloc` and `mstress`); M1 is the standing instruction-count gap; M3, M4, M6 and M7 are consequences of design decisions already on record (decay window, reservation layout, arena count), each with its absolute size given above; M5 is a candidate for the batch-purge and prefault items of Phase 2.5.

## 3. Container tests, OUTCOME lines

```
RESULT churn mode=purge mib=400 threshold=100 rss_peak=403 rss_2s=12 rss_min=12 below_ms=50 current_peak=405 current_2s=14
OUTCOME churn alloc=challoc args='purge 400 100' status=0 verdict=ok oom_kill=0
RESULT churn mode=keep mib=400 threshold=100 rss_peak=403 rss_2s=317 rss_min=317 below_ms=-1 current_peak=406 current_2s=320
RESULT churn mode=purge mib=200 threshold=50 rss_peak=202 rss_2s=8 rss_min=8 below_ms=80 current_peak=205 current_2s=10
OUTCOME churn alloc=challoc args='purge 200 50' status=0 verdict=ok oom_kill=0
RESULT churn mode=keep mib=200 threshold=50 rss_peak=202 rss_2s=160 rss_min=160 below_ms=-1 current_peak=205 current_2s=163
RESULT oscillate cycles=98 seconds=301 low=200 high=480 current_peak=504 rss_peak=501 max_before=0 max_after=0 high_before=0 high_after=0 oom_kill_before=0 oom_kill_after=0
OUTCOME oscillate alloc=challoc args='300' status=0 verdict=ok oom_kill=0
RESULT churn mode=report mib=400 threshold=100 rss_peak=409 rss_2s=195 rss_min=195 below_ms=-1 current_peak=409 current_2s=194
OUTCOME churn alloc=jemalloc args='report 400 100' status=0 verdict=ok oom_kill=0
RESULT churn mode=report mib=200 threshold=50 rss_peak=206 rss_2s=124 rss_min=124 below_ms=-1 current_peak=207 current_2s=124
OUTCOME churn alloc=jemalloc args='report 200 50' status=0 verdict=ok oom_kill=0
OUTCOME oscillate alloc=jemalloc args='300' status=137 verdict=OOM-killed oom_kill=1
RESULT churn mode=report mib=400 threshold=100 rss_peak=407 rss_2s=407 rss_min=407 below_ms=-1 current_peak=410 current_2s=409
OUTCOME churn alloc=mimalloc args='report 400 100' status=0 verdict=ok oom_kill=0
RESULT churn mode=report mib=200 threshold=50 rss_peak=207 rss_2s=207 rss_min=207 below_ms=-1 current_peak=210 current_2s=210
OUTCOME churn alloc=mimalloc args='report 200 50' status=0 verdict=ok oom_kill=0
OUTCOME oscillate alloc=mimalloc args='300' status=137 verdict=OOM-killed oom_kill=1
RESULT churn mode=report mib=400 threshold=100 rss_peak=408 rss_2s=408 rss_min=408 below_ms=-1 current_peak=412 current_2s=412
OUTCOME churn alloc=tcmalloc args='report 400 100' status=0 verdict=ok oom_kill=0
RESULT churn mode=report mib=200 threshold=50 rss_peak=207 rss_2s=207 rss_min=207 below_ms=-1 current_peak=212 current_2s=211
OUTCOME churn alloc=tcmalloc args='report 200 50' status=0 verdict=ok oom_kill=0
OUTCOME oscillate alloc=tcmalloc args='300' status=137 verdict=OOM-killed oom_kill=1
```

