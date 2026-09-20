# challoc comparison matrix (four-way), 2026-09-19, aarch64

- commit: 4864d4b
- date: 2026-09-19T22:55:59Z
- host: Neoverse-V2 (Axion), 8 CPUs visible to the container
- kernel: 6.18.49-r2-gcp-6.18 (Chainguard OS 6.18.49-r2-gcp-6.18, GCE c4a-standard-8 (Neoverse-V2 (Axion)), zone us-east4-a, 8 vCPU, podman --network=host --privileged), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- python-3.13-3.13.15_git20260918-r1
- rust-1.98-1.98.1-r0
- tcmalloc-minimal-2.18.1-r5
- allocators (same binaries, LD_PRELOAD only; challoc from the commit above):
  - challoc: 4864d4b3d673 built into target/ab/4864d4b3d673
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)
  - glibc: glibc malloc (no LD_PRELOAD, glibc 2.44)
  - mimalloc: /src/target/bench-bin/libmimalloc.so (v3.5.2)
  - tcmalloc: Wolfi's /usr/lib/libtcmalloc_minimal.so.4 (tcmalloc-minimal 2.18.1, gperftools without profiler hooks)
- Ruby rows: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh with Wolfi's flags, --enable-yjit configured but YJIT not enabled at run time), installed under target/ruby/install-{challoc,glibc}
- Python rows: Python 3.13.15 (Wolfi python-3.13 3.13.15_git20260918, pymalloc built in); modes: default = PYTHONMALLOC unset (pymalloc for objects under 512 bytes, the allocator sees the rest), malloc = PYTHONMALLOC=malloc (every object through the allocator)
- host: Chainguard OS (bootc, kernel 6.18.49-r2-gcp-6.18, glibc 2.44-r6 on the host, THP always), c4a-standard-8 (8 vCPU, half the reference pair's), inside `sudo podman run --network=host --privileged` (the container rows: podman's default profile, `--memory` limits); 3 timed rounds, not 5 (about 20 minutes for the ab step on this shape); Phase 2.5 task 14, lane E; the same rows natively and in the container, and the THP counters, are in section 4
- method: every cell comes from one run per VM on an idle VM (load under 1 at the start): the Phase 0 set and the Valkey model rows from one `bench/ab.sh` run (5 timed rounds after a warm-up, variants interleaved per round, median of the sorted scores, median peak RSS from bench/rt); the Ruby rows from one `bench/ruby-bench.sh` run and the Python rows from one `bench/python-bench.sh` run (same protocol, wall seconds); the container rows from `make test-cgroup` (challoc) and `make test-cgroup-compare CGROUP_ALLOCS='jemalloc mimalloc tcmalloc'` (one memory-limited container per run, RSS 2 s after freeing the churn, and the 300 s oscillation's verdict). Positive percentages mean challoc did worse than that allocator (slower, lower throughput, or more peak RSS); time is the score's direction (throughput rows compare throughput). The Valkey soak (`make valkey-soak`) is jemalloc-only: it drives Valkey's own `je_` allocator API (`MEMORY MALLOC-STATS`, active defrag, `jemalloc_purge`), which no other allocator has, so it is not a row here; its latest comparison is in the scorecard's soak columns.
- sources: `2026-09-19-aarch64-cgos-four-way-ab.md`, `2026-09-19-aarch64-cgos-ruby.md`, `2026-09-19-aarch64-cgos-python.md`, the container logs (OUTCOME lines quoted in section 3)

## 1. The table

| group | row | unit | challoc | jemalloc | glibc | mimalloc | tcmalloc | vs jemalloc (time / RSS) | vs glibc (time / RSS) | vs mimalloc (time / RSS) | vs tcmalloc (time / RSS) |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Phase 0 set | larsonN | M ops/s | 301.9 (89.4 MiB) | 315.6 (149.8 MiB) | 229.4 (96.8 MiB) | 295.5 (104.2 MiB) | 303.9 (76.1 MiB) | +4.5% / -40.3% | -24.0% / -7.6% | -2.1% / -14.2% | +0.7% / +17.5% |
| Phase 0 set | xmallocN | M frees/s | 344.172 (93.1 MiB) | 245.094 (131.7 MiB) | 17.237 (57.7 MiB) | 79.968 (65.7 MiB) | 2.519 (41.2 MiB) | -28.8% / -29.3% | -95.0% / +61.4% | -76.8% / +41.7% | -99.3% / +126.0% |
| Phase 0 set | cfrac | s | 4.464 (2.8 MiB) | 4.422 (7.9 MiB) | 4.301 (2.6 MiB) | 4.273 (3.8 MiB) | 4.385 (9.6 MiB) | +0.9% / -64.6% | +3.8% / +7.7% | +4.5% / -26.3% | +1.8% / -70.8% |
| Phase 0 set | espresso | s | 3.660 (3.6 MiB) | 3.739 (16.0 MiB) | 3.897 (4.4 MiB) | 3.553 (9.9 MiB) | 3.572 (10.5 MiB) | -2.1% / -77.5% | -6.1% / -18.2% | +3.0% / -63.6% | +2.5% / -65.7% |
| Phase 0 set | mstress | s | 0.342 (155.7 MiB) | 0.579 (195.2 MiB) | 0.435 (226.4 MiB) | 0.282 (281.8 MiB) | 0.378 (148.3 MiB) | -40.9% / -20.2% | -21.4% / -31.2% | +21.3% / -44.7% | -9.5% / +5.0% |
| Phase 0 set | alloc-test1 | s | 2.710 (15.0 MiB) | 2.563 (20.7 MiB) | 3.135 (15.2 MiB) | 2.569 (15.8 MiB) | 2.539 (19.5 MiB) | +5.7% / -27.5% | -13.6% / -1.3% | +5.5% / -5.1% | +6.7% / -23.1% |
| Phase 0 set | alloc-testN | s | 2.323 (17.0 MiB) | 2.228 (55.1 MiB) | 2.593 (19.8 MiB) | 2.175 (35.9 MiB) | 2.238 (20.7 MiB) | +4.3% / -69.1% | -10.4% / -14.1% | +6.8% / -52.6% | +3.8% / -17.9% |
| Phase 0 set | glibc-simple | s | 2.172 (2.3 MiB) | 2.518 (12.0 MiB) | 4.138 (3.3 MiB) | 1.742 (3.6 MiB) | 1.700 (8.9 MiB) | -13.7% / -80.8% | -47.5% / -30.3% | +24.7% / -36.1% | +27.8% / -74.2% |
| Phase 0 set | redis | k req/s | 509.7 (29.1 MiB) | 411.9 (35.7 MiB) | 537.1 (28.7 MiB) | 464.7 (39.4 MiB) | 528.5 (35.4 MiB) | -19.2% / -18.5% | +5.4% / +1.4% | -8.8% / -26.1% | +3.7% / -17.8% |
| Valkey model | valkey1 | M ops/s | 7.151 (584.7 MiB) | 7.436 (594.7 MiB) | 5.569 (569.1 MiB) | 4.167 (585.7 MiB) | 6.429 (589.5 MiB) | +4.0% / -1.7% | -22.1% / +2.7% | -41.7% / -0.2% | -10.1% / -0.8% |
| Valkey model | valkey1-free | M ops/s | 7.078 (584.7 MiB) | 7.333 (594.7 MiB) | 5.787 (568.5 MiB) | 4.103 (585.7 MiB) | 6.415 (589.1 MiB) | +3.6% / -1.7% | -18.2% / +2.8% | -42.0% / -0.2% | -9.4% / -0.7% |
| Valkey model | valkey1-usable | M ops/s | 5.998 (584.5 MiB) | 6.078 (594.7 MiB) | 2.609 (569.0 MiB) | 3.050 (585.7 MiB) | 4.678 (589.2 MiB) | +1.3% / -1.7% | -56.5% / +2.7% | -49.1% / -0.2% | -22.0% / -0.8% |
| Ruby 3.4 | ruby startup | s | 0.042 (13.2 MiB) | 0.039 (17.5 MiB) | 0.040 (11.6 MiB) | 0.039 (12.8 MiB) | 0.040 (20.0 MiB) | +7.7% / -24.6% | +5.0% / +13.8% | +7.7% / +3.1% | +5.0% / -34.0% |
| Ruby 3.4 | ruby so_binary_trees | s | 4.680 (26.9 MiB) | 4.664 (28.2 MiB) | 4.678 (25.3 MiB) | 4.676 (26.1 MiB) | 4.679 (31.1 MiB) | +0.3% / -4.6% | +0.0% / +6.3% | +0.1% / +3.1% | +0.0% / -13.5% |
| Ruby 3.4 | ruby hash_aref_str | s | 0.355 (9.8 MiB) | 0.349 (11.1 MiB) | 0.361 (8.9 MiB) | 0.353 (9.5 MiB) | 0.346 (14.3 MiB) | +1.7% / -11.7% | -1.7% / +10.1% | +0.6% / +3.2% | +2.6% / -31.5% |
| Ruby 3.4 | ruby array_sample_100k__1k | s | 0.428 (32.6 MiB) | 0.429 (34.0 MiB) | 0.440 (28.7 MiB) | 0.434 (49.1 MiB) | 0.427 (35.2 MiB) | -0.2% / -4.1% | -2.7% / +13.6% | -1.4% / -33.6% | +0.2% / -7.4% |
| Ruby 3.4 | ruby array_sort_block | s | 5.314 (29.9 MiB) | 5.311 (31.4 MiB) | 5.251 (27.9 MiB) | 5.298 (47.1 MiB) | 5.299 (34.2 MiB) | +0.1% / -4.8% | +1.2% / +7.2% | +0.3% / -36.5% | +0.3% / -12.6% |
| Ruby 3.4 | ruby so_nbody | s | 0.824 (10.1 MiB) | 0.820 (11.3 MiB) | 0.836 (9.0 MiB) | 0.820 (9.6 MiB) | 0.821 (14.6 MiB) | +0.5% / -10.6% | -1.4% / +12.2% | +0.5% / +5.2% | +0.4% / -30.8% |
| Ruby 3.4 | ruby app_fib | s | 0.292 (9.8 MiB) | 0.299 (11.2 MiB) | 0.289 (8.9 MiB) | 0.289 (9.5 MiB) | 0.292 (14.3 MiB) | -2.3% / -12.5% | +1.0% / +10.1% | +1.0% / +3.2% | +0.0% / -31.5% |
| Ruby 3.4 | ruby erb_render | s | 0.841 (11.4 MiB) | 0.974 (12.9 MiB) | 0.867 (9.8 MiB) | 0.851 (10.9 MiB) | 0.761 (15.7 MiB) | -13.7% / -11.6% | -3.0% / +16.3% | -1.2% / +4.6% | +10.5% / -27.4% |
| Ruby 3.4 | ruby gem_list | s | 0.120 (26.4 MiB) | 0.114 (30.1 MiB) | 0.120 (24.9 MiB) | 0.116 (25.9 MiB) | 0.115 (31.5 MiB) | +5.3% / -12.3% | +0.0% / +6.0% | +3.4% / +1.9% | +4.3% / -16.2% |
| Ruby 3.4 | ruby gc_churn | s | 1.661 (382.3 MiB) | 1.666 (364.5 MiB) | 1.579 (316.0 MiB) | 1.656 (354.4 MiB) | 1.611 (336.8 MiB) | -0.3% / +4.9% | +5.2% / +21.0% | +0.3% / +7.9% | +3.1% / +13.5% |
| Python 3.13 | python json-default | s | 4.074 (89.3 MiB) | 4.026 (95.2 MiB) | 4.004 (68.3 MiB) | 4.019 (113.6 MiB) | 4.066 (91.3 MiB) | +1.2% / -6.2% | +1.7% / +30.7% | +1.4% / -21.4% | +0.2% / -2.2% |
| Python 3.13 | python json-malloc | s | 4.188 (89.9 MiB) | 4.081 (84.9 MiB) | 4.410 (71.0 MiB) | 4.034 (89.1 MiB) | 4.215 (91.0 MiB) | +2.6% / +5.9% | -5.0% / +26.6% | +3.8% / +0.9% | -0.6% / -1.2% |
| Python 3.13 | python dict_list-default | s | 3.349 (241.8 MiB) | 3.317 (242.7 MiB) | 3.310 (217.7 MiB) | 3.330 (256.7 MiB) | 3.309 (236.0 MiB) | +1.0% / -0.4% | +1.2% / +11.1% | +0.6% / -5.8% | +1.2% / +2.5% |
| Python 3.13 | python dict_list-malloc | s | 4.425 (239.3 MiB) | 4.462 (249.3 MiB) | 4.790 (275.4 MiB) | 3.750 (260.9 MiB) | 4.041 (231.8 MiB) | -0.8% / -4.0% | -7.6% / -13.1% | +18.0% / -8.3% | +9.5% / +3.2% |
| Python 3.13 | python regex-default | s | 3.521 (83.2 MiB) | 3.493 (83.1 MiB) | 3.513 (63.7 MiB) | 3.506 (101.2 MiB) | 3.503 (77.6 MiB) | +0.8% / +0.1% | +0.2% / +30.6% | +0.4% / -17.8% | +0.5% / +7.2% |
| Python 3.13 | python regex-malloc | s | 3.512 (87.2 MiB) | 3.451 (76.3 MiB) | 3.568 (70.0 MiB) | 3.398 (89.1 MiB) | 3.485 (76.2 MiB) | +1.8% / +14.3% | -1.6% / +24.6% | +3.4% / -2.1% | +0.8% / +14.4% |
| Python 3.13 | python object_graph-default | s | 3.191 (210.2 MiB) | 3.119 (203.8 MiB) | 3.103 (193.0 MiB) | 3.052 (222.1 MiB) | 3.135 (202.0 MiB) | +2.3% / +3.1% | +2.8% / +8.9% | +4.6% / -5.4% | +1.8% / +4.1% |
| Python 3.13 | python object_graph-malloc | s | 3.249 (209.9 MiB) | 2.703 (202.1 MiB) | 2.923 (204.1 MiB) | 2.697 (201.1 MiB) | 3.217 (200.5 MiB) | +20.2% / +3.9% | +11.2% / +2.8% | +20.5% / +4.4% | +1.0% / +4.7% |
| container | churn 512m: RSS 2 s after freeing 400 MiB | MiB kept | 16 (403 MiB) | 195 (424 MiB) | - | 407 (407 MiB) | 411 (411 MiB) | -91.8% / -5.0% | - / - | -96.1% / -1.0% | -96.1% / -1.9% |
| container | churn 256m: RSS 2 s after freeing 200 MiB | MiB kept | 9 (202 MiB) | 129 (216 MiB) | - | 207 (207 MiB) | 211 (211 MiB) | -93.0% / -6.5% | - / - | -95.7% / -2.4% | -95.7% / -4.3% |
| container | oscillate 512m: 200..480 MiB live, 300 s | verdict | ok, 98 cycles (502 MiB) | OOM-killed | - | OOM-killed | OOM-killed | - / - | - / - | - / - | - / - |

Units: larsonN and valkey rows M ops/s, xmallocN M frees/s, redis k req/s (higher is better); the other Phase 0, Ruby and Python rows wall seconds (lower is better); container churn rows MiB of RSS kept 2 s after the frees (lower is better), the oscillation row its verdict and cycle count. RSS in parentheses is bench/rt's peak (MiB) of the measured process (the churn rows: the program's own RSS peak).

## 2. Rows where challoc is more than 10% behind an allocator

- Phase 0 set / larsonN: challoc RSS +17.5% against tcmalloc
- Phase 0 set / xmallocN: challoc RSS +61.4% against glibc
- Phase 0 set / xmallocN: challoc RSS +41.7% against mimalloc
- Phase 0 set / xmallocN: challoc RSS +126.0% against tcmalloc
- Phase 0 set / mstress: challoc time +21.3% against mimalloc
- Phase 0 set / glibc-simple: challoc time +24.7% against mimalloc
- Phase 0 set / glibc-simple: challoc time +27.8% against tcmalloc
- Ruby 3.4 / ruby startup: challoc RSS +13.8% against glibc
- Ruby 3.4 / ruby hash_aref_str: challoc RSS +10.1% against glibc
- Ruby 3.4 / ruby array_sample_100k__1k: challoc RSS +13.6% against glibc
- Ruby 3.4 / ruby so_nbody: challoc RSS +12.2% against glibc
- Ruby 3.4 / ruby app_fib: challoc RSS +10.1% against glibc
- Ruby 3.4 / ruby erb_render: challoc RSS +16.3% against glibc
- Ruby 3.4 / ruby erb_render: challoc time +10.5% against tcmalloc
- Ruby 3.4 / ruby gc_churn: challoc RSS +21.0% against glibc
- Ruby 3.4 / ruby gc_churn: challoc RSS +13.5% against tcmalloc
- Python 3.13 / python json-default: challoc RSS +30.7% against glibc
- Python 3.13 / python json-malloc: challoc RSS +26.6% against glibc
- Python 3.13 / python dict_list-default: challoc RSS +11.1% against glibc
- Python 3.13 / python dict_list-malloc: challoc time +18.0% against mimalloc
- Python 3.13 / python regex-default: challoc RSS +30.6% against glibc
- Python 3.13 / python regex-malloc: challoc RSS +14.3% against jemalloc
- Python 3.13 / python regex-malloc: challoc RSS +24.6% against glibc
- Python 3.13 / python regex-malloc: challoc RSS +14.4% against tcmalloc
- Python 3.13 / python object_graph-malloc: challoc time +20.2% against jemalloc
- Python 3.13 / python object_graph-malloc: challoc time +11.2% against glibc
- Python 3.13 / python object_graph-malloc: challoc time +20.5% against mimalloc

Attribution (plan Phase 2.5 task 1 acceptance: every row above has one before anything is changed for it):

The reference for every row is the Ubuntu 7.0 four-way of task 1 (`2026-09-19-aarch64-gce-four-way.md`, commit `1339eb6`, c4a-standard-16), so two confounds sit under every cross-host delta: the commit (`4864d4b` has task 1b's flush sort and task 8's per-CPU dispatch that `1339eb6` lacks) and the shape (8 vCPU here; larsonN and xmallocN run 8 threads, not 16). The mechanisms are the task 1 file's M1..M7; what is new on this host is measured in section 4 (THP `always`) and in `2026-09-19-chainguard-os-cells.md`.

- larsonN RSS +17.5% and xmallocN RSS +42..126% against tcmalloc / mimalloc / glibc: M3 (retention until the 10 s decay on a 5 s run); the same rows on Ubuntu read +25.5% and +39..149%. Not THP: challoc's larson16 peak AnonHugePages is 4..6 MiB (section 4), the class regions take no huge pages.
- mstress time +21.3% against mimalloc (0.342 vs 0.282 s; Ubuntu +30.0%): M5 + M2 + M1 as attributed there; the gap narrowed with task 1b's flush sort. RSS is now 20% under jemalloc's where Ubuntu had +8.7%: jemalloc's mstress grew from 292 to 195..282 MiB-equivalents under `always` while challoc's fell (155.7 MiB, 8 threads).
- glibc-simple +24.7% / +27.8% against mimalloc / tcmalloc (Ubuntu +30.8% / +32.5%): M2 (what is left of the flush sort after task 1b) + M1.
- Ruby startup, hash_aref_str, array_sample, so_nbody, app_fib, erb_render RSS +10..16% against glibc: M4 (the 1..1.6 MiB fixed footprint on 9..13 MiB processes), the same numbers as Ubuntu to the decimal (13.2 vs 11.6 MiB on startup).
- Ruby erb_render time +10.5% against tcmalloc (Ubuntu +10.3%): M1 with the dTLB component, unchanged.
- Ruby gc_churn RSS +21.0% / +13.5% against glibc / tcmalloc: the `malloc_trim` row of Phase 1.5 task 4b plus M3, unchanged (382 vs 316 / 337 MiB).
- **Python json-default, json-malloc, regex-default, regex-malloc, dict_list-default RSS +11..31% against glibc (new: Ubuntu had +10% at most)**: THP `always` on the huge tier. challoc's json-default peak went from 74.7 MiB (Ubuntu) to 89.3 MiB here and jemalloc's from 83.2 to 95.2, while glibc (68.1 -> 68.3), mimalloc (113.7 -> 113.6) and tcmalloc (90.1 -> 91.3) did not move: the two allocators that serve the 7 MB JSON text buffers from 2 MiB-aligned mappings get them backed by huge pages under `always` (section 4: challoc's only AnonHugePages are the huge-tier runs, 32 MiB on the Valkey model), and a partially used huge page costs its full 2 MiB of RSS. The class regions are not affected (dict_list, object_graph: RSS within 4% of Ubuntu's).
- Python dict_list-malloc +18.0% against mimalloc (Ubuntu +28.5% / +17.3%) and object_graph-malloc +20.2% / +11.2% / +20.5% against jemalloc / glibc / mimalloc (Ubuntu +6.6% / +10.7% / +22.5%): M2 + M1, the small-object path; the jemalloc delta on object_graph grew by 14 points, which section 4's THP control does not cover (Python was not in the control set) and is left attributed to M1 plus jemalloc's THP gain on a 200 MiB heap, to be confirmed with the control if pursued.
- The Valkey model rows are within 4% of jemalloc and ahead of the other three; against tcmalloc challoc is now 10% ahead where Ubuntu had it 19.6% behind: tcmalloc's valkey1 fell from 8.5 to 6.4 M ops/s between the hosts while challoc held 7.1; tcmalloc takes no huge pages either (40 MiB), so this is the kernel or the 8-vCPU shape, not THP, and the cpu-c4a cell has no tcmalloc row to separate them.
- The THP `always` cost that the table does hide: section 4's control shows the challoc-vs-jemalloc gap on valkey1 is +0.1% with the host at `madvise` and +4.0% at `always` (jemalloc gains 14% from 584 MiB of huge pages, challoc 9% from 32 MiB), and on larson16 +3.9% against +5.9%.

## 3. Container tests, OUTCOME lines

The challoc churn lines below include the post-run repetition (`post.log`: ten runs of the 512m churn in the default configuration and ten with `lazy_band:false`, then `make test-cgroup-churn`); the runs that read `rss_2s=309..310` are the intermittent stall described in `2026-09-19-chainguard-os-cells.md` section 3 (the table above takes the cell's protocol run).

```
RESULT churn mode=purge mib=400 threshold=100 rss_peak=403 rss_2s=16 rss_min=16 below_ms=56 current_peak=404 current_2s=17
OUTCOME churn alloc=challoc args='purge 400 100' status=0 verdict=ok oom_kill=0
RESULT churn mode=keep mib=400 threshold=100 rss_peak=403 rss_2s=334 rss_min=334 below_ms=-1 current_peak=404 current_2s=334
RESULT churn mode=purge mib=200 threshold=50 rss_peak=202 rss_2s=9 rss_min=9 below_ms=86 current_peak=203 current_2s=10
OUTCOME churn alloc=challoc args='purge 200 50' status=0 verdict=ok oom_kill=0
RESULT churn mode=keep mib=200 threshold=50 rss_peak=202 rss_2s=170 rss_min=170 below_ms=-1 current_peak=203 current_2s=171
RESULT oscillate cycles=98 seconds=301 low=200 high=480 current_peak=503 rss_peak=502 max_before=0 max_after=0 high_before=0 high_after=0 oom_kill_before=0 oom_kill_after=0
OUTCOME oscillate alloc=challoc args='300' status=0 verdict=ok oom_kill=0
RESULT churn mode=report mib=400 threshold=100 rss_peak=424 rss_2s=195 rss_min=195 below_ms=-1 current_peak=429 current_2s=207
OUTCOME churn alloc=jemalloc args='report 400 100' status=0 verdict=ok oom_kill=0
RESULT churn mode=report mib=200 threshold=50 rss_peak=216 rss_2s=129 rss_min=129 below_ms=-1 current_peak=221 current_2s=138
OUTCOME churn alloc=jemalloc args='report 200 50' status=0 verdict=ok oom_kill=0
OUTCOME oscillate alloc=jemalloc args='300' status=137 verdict=OOM-killed oom_kill=1
RESULT churn mode=report mib=400 threshold=100 rss_peak=407 rss_2s=407 rss_min=407 below_ms=-1 current_peak=410 current_2s=408
OUTCOME churn alloc=mimalloc args='report 400 100' status=0 verdict=ok oom_kill=0
RESULT churn mode=report mib=200 threshold=50 rss_peak=207 rss_2s=207 rss_min=207 below_ms=-1 current_peak=209 current_2s=208
OUTCOME churn alloc=mimalloc args='report 200 50' status=0 verdict=ok oom_kill=0
OUTCOME oscillate alloc=mimalloc args='300' status=137 verdict=OOM-killed oom_kill=1
RESULT churn mode=report mib=400 threshold=100 rss_peak=411 rss_2s=411 rss_min=411 below_ms=-1 current_peak=417 current_2s=417
OUTCOME churn alloc=tcmalloc args='report 400 100' status=0 verdict=ok oom_kill=0
RESULT churn mode=report mib=200 threshold=50 rss_peak=211 rss_2s=211 rss_min=211 below_ms=-1 current_peak=216 current_2s=216
OUTCOME churn alloc=tcmalloc args='report 200 50' status=0 verdict=ok oom_kill=0
OUTCOME oscillate alloc=tcmalloc args='300' status=137 verdict=OOM-killed oom_kill=1
```

## 4. Host-native and in-container rows, and THP under `always`

The same four rows, the same protocol (one warm-up, 3 interleaved timed rounds, `bench/rt`'s peak RSS), run by one script (`bench/gce/native-rows.sh`) inside the podman container (`ROOT=/src`) and natively on the Chainguard OS host (the binaries copied out of the container into `~/native`, the host's own glibc 2.44-r6 underneath, every allocator's library confirmed mapped). The percentages agree to a point or two in every cell, so the container adds nothing to the numbers on this host; the one visible difference is the wider spread of the native valkey1 list on arm64 (6.66..7.21) in a run that overlapped nothing.

container:

# rows (container) on challoc-lane-e-cgos-arm64: aarch64 6.18.49-r2-gcp-6.18, 8 CPUs, glibc 2.442.44, 2026-09-19T23:32:27Z
- rows: larson1 larson16 valkey1 valkey1-free; allocators: challoc glibc jemalloc mimalloc tcmalloc; 3 timed rounds after one warm-up, interleaved
- load at start: 0.64 0.96 1.49
- challoc: /src/target/release/libchalloc.so mapped=4
- glibc: no LD_PRELOAD (the host's libc malloc)
- jemalloc: /usr/lib/libjemalloc.so.2 mapped=4
- mimalloc: /src/target/bench-bin/libmimalloc.so mapped=4
- tcmalloc: /usr/lib/libtcmalloc_minimal.so.4 mapped=4

#### Sorted scores (container)
| program | variant | sorted scores | median | maxrss MiB |
|---|---|---|---:|---:|
| larson1 | challoc | 41.9 42.0 42.0 | 42.0 | 20.5 |
| larson1 | glibc | 36.2 36.3 36.5 | 36.3 | 20.9 |
| larson1 | jemalloc | 44.8 45.0 45.1 | 45.0 | 35.2 |
| larson1 | mimalloc | 42.2 42.2 42.2 | 42.2 | 20.5 |
| larson1 | tcmalloc | 46.0 46.0 46.3 | 46.0 | 20.8 |
| larson16 | challoc | 301.1 303.1 303.2 | 303.1 | 174.1 |
| larson16 | glibc | 157.0 159.0 160.3 | 159.0 | 143.3 |
| larson16 | jemalloc | 318.5 320.9 322.6 | 320.9 | 266.5 |
| larson16 | mimalloc | 305.3 307.1 307.1 | 307.1 | 193.3 |
| larson16 | tcmalloc | 313.5 315.4 318.0 | 315.4 | 102.6 |
| valkey1 | challoc | 7.109 7.244 7.245 | 7.244 | 584.7 |
| valkey1 | glibc | 5.766 5.836 5.847 | 5.836 | 568.6 |
| valkey1 | jemalloc | 7.516 7.557 7.615 | 7.557 | 596.7 |
| valkey1 | mimalloc | 4.094 4.293 4.342 | 4.293 | 585.7 |
| valkey1 | tcmalloc | 6.440 6.484 6.501 | 6.484 | 589.5 |
| valkey1-free | challoc | 6.985 7.057 7.115 | 7.057 | 584.7 |
| valkey1-free | glibc | 5.787 5.827 5.880 | 5.827 | 569.3 |
| valkey1-free | jemalloc | 7.314 7.361 7.411 | 7.361 | 596.7 |
| valkey1-free | mimalloc | 4.127 4.189 4.213 | 4.189 | 585.7 |
| valkey1-free | tcmalloc | 6.460 6.535 6.566 | 6.535 | 589.4 |

#### challoc against each (positive = challoc worse), container
| program | vs glibc | vs jemalloc | vs mimalloc | vs tcmalloc |
|---|---:|---:|---:|---:|
| larson1 | -13.6% | +7.1% | +0.5% | +9.5% |
| larson16 | -47.5% | +5.9% | +1.3% | +4.1% |
| valkey1 | -19.4% | +4.3% | -40.7% | -10.5% |
| valkey1-free | -17.4% | +4.3% | -40.6% | -7.4% |

native:

rows (native) control run: on challoc-lane-e-cgos-arm64: aarch64 6.18.49-r2-gcp-6.18, 8 CPUs, glibc 2.44, 2026-09-19T23:43:54Z
- rows: larson1 larson16 valkey1 valkey1-free; allocators: challoc glibc jemalloc mimalloc tcmalloc; 3 timed rounds after one warm-up, interleaved
- load at start: 1.85 2.76 2.35
- challoc: /home/dlorenc/native/target/bench-bin/libchalloc.so mapped=4
- glibc: no LD_PRELOAD (the host's libc malloc)
- jemalloc: /home/dlorenc/native/target/bench-bin/libjemalloc.so.2 mapped=4
- mimalloc: /home/dlorenc/native/target/bench-bin/libmimalloc.so mapped=4
- tcmalloc: /home/dlorenc/native/target/bench-bin/libtcmalloc_minimal.so.4 mapped=4

#### Sorted scores (native)
| program | variant | sorted scores | median | maxrss MiB |
|---|---|---|---:|---:|
| larson1 | challoc | 41.5 41.9 42.1 | 41.9 | 20.4 |
| larson1 | glibc | 35.6 35.6 36.0 | 35.6 | 20.9 |
| larson1 | jemalloc | 44.5 45.0 45.2 | 45.0 | 37.4 |
| larson1 | mimalloc | 41.8 41.9 42.1 | 41.9 | 20.5 |
| larson1 | tcmalloc | 45.9 46.1 46.3 | 46.1 | 20.6 |
| larson16 | challoc | 298.6 299.5 300.8 | 299.5 | 178.9 |
| larson16 | glibc | 149.0 153.2 157.4 | 153.2 | 145.8 |
| larson16 | jemalloc | 315.9 316.9 321.9 | 316.9 | 266.1 |
| larson16 | mimalloc | 302.2 302.6 307.7 | 302.6 | 194.3 |
| larson16 | tcmalloc | 311.8 314.7 316.3 | 314.7 | 105.1 |
| valkey1 | challoc | 6.657 6.916 7.205 | 6.916 | 584.7 |
| valkey1 | glibc | 5.216 5.611 5.801 | 5.611 | 569.0 |
| valkey1 | jemalloc | 7.085 7.212 7.539 | 7.212 | 596.6 |
| valkey1 | mimalloc | 3.568 3.706 4.269 | 3.706 | 585.7 |
| valkey1 | tcmalloc | 6.038 6.067 6.181 | 6.067 | 589.5 |
| valkey1-free | challoc | 6.354 6.558 6.938 | 6.558 | 584.5 |
| valkey1-free | glibc | 5.389 5.534 5.579 | 5.534 | 568.4 |
| valkey1-free | jemalloc | 6.746 7.010 7.207 | 7.010 | 594.6 |
| valkey1-free | mimalloc | 3.429 3.715 3.798 | 3.715 | 585.7 |
| valkey1-free | tcmalloc | 5.931 6.052 6.093 | 6.052 | 589.5 |

#### challoc against each (positive = challoc worse), native
| program | vs glibc | vs jemalloc | vs mimalloc | vs tcmalloc |
|---|---:|---:|---:|---:|
| larson1 | -15.0% | +7.4% | +0.0% | +10.0% |
| larson16 | -48.8% | +5.8% | +1.0% | +5.1% |
| valkey1 | -18.9% | +4.3% | -46.4% | -12.3% |
| valkey1-free | -15.6% | +6.9% | -43.4% | -7.7% |

THP under `always` (the coordinator's request): one run of valkey1 and larson16 per allocator with `AnonHugePages` and `Rss` sampled from `/proc/<pid>/smaps_rollup` every 200 ms (the peak kept) and the deltas of the `thp_*` counters of `/proc/vmstat` around the run (the host's counters; nothing else ran). `khugepaged/max_ptes_none` is 511 and `defrag` is `madvise` on this host, so a huge page is taken at fault time when the 2 MiB-aligned range lies inside one VMA, never by compaction stalls.

container:

#### THP under always (container): peak AnonHugePages per allocator, /proc/vmstat deltas
- thp enabled: [always] madvise never; defrag: always defer defer+madvise [madvise] never; khugepaged/max_ptes_none: 511; hugepages-2048kB: always [inherit] madvise never
| program | variant | score | peak Rss MiB | peak AnonHugePages MiB | thp_fault_alloc | thp_fault_fallback | thp_split_page | thp_deferred_split_page |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| valkey1 | challoc | 7.046 | 584.7 | 32.0 | 17 | 0 | 0 | 1 |
| valkey1 | glibc | 5.697 | 569.7 | 564.0 | 285 | 0 | 0 | 0 |
| valkey1 | jemalloc | 7.495 | 596.7 | 584.0 | 294 | 0 | 0 | 6 |
| valkey1 | mimalloc | 3.852 | 585.7 | 584.0 | 292 | 0 | 0 | 0 |
| valkey1 | tcmalloc | 6.388 | 589.5 | 40.0 | 22 | 0 | 0 | 0 |
| larson16 | challoc | 299.4 | 176.5 | 6.0 | 3 | 0 | 0 | 0 |
| larson16 | glibc | 157.9 | 143.9 | 120.0 | 67 | 0 | 0 | 0 |
| larson16 | jemalloc | 320.6 | 269.8 | 172.0 | 104 | 0 | 0 | 44 |
| larson16 | mimalloc | 305.3 | 190.3 | 154.0 | 83 | 0 | 0 | 13 |
| larson16 | tcmalloc | 315.0 | 103.2 | 4.0 | 3 | 0 | 0 | 0 |

native:

#### THP under always (native): peak AnonHugePages per allocator, /proc/vmstat deltas
- thp enabled: [always] madvise never; defrag: always defer defer+madvise [madvise] never; khugepaged/max_ptes_none: 511; hugepages-2048kB: always [inherit] madvise never
| program | variant | score | peak Rss MiB | peak AnonHugePages MiB | thp_fault_alloc | thp_fault_fallback | thp_split_page | thp_deferred_split_page |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| valkey1 | challoc | 7.104 | 584.7 | 32.0 | 23 | 0 | 0 | 1 |
| valkey1 | glibc | 5.910 | 568.4 | 564.0 | 299 | 0 | 0 | 0 |
| valkey1 | jemalloc | 7.455 | 596.7 | 586.0 | 304 | 0 | 0 | 2 |
| valkey1 | mimalloc | 4.233 | 585.7 | 584.0 | 306 | 0 | 0 | 0 |
| valkey1 | tcmalloc | 6.515 | 587.4 | 36.0 | 27 | 0 | 0 | 1 |
| larson16 | challoc | 301.2 | 177.0 | 4.0 | 11 | 0 | 0 | 0 |
| larson16 | glibc | 156.4 | 147.2 | 124.0 | 70 | 0 | 0 | 0 |
| larson16 | jemalloc | 320.0 | 269.2 | 164.0 | 104 | 0 | 0 | 35 |
| larson16 | mimalloc | 305.7 | 194.0 | 152.0 | 88 | 0 | 0 | 21 |
| larson16 | tcmalloc | 313.4 | 105.6 | 6.0 | 10 | 0 | 0 | 0 |

Reading it: **challoc's class regions take no huge pages under `always`**: 32 MiB of AnonHugePages on a 585 MiB Valkey-model heap (the huge-tier runs, i.e. the model's own tables) and 4..6 MiB on larson16, against the whole heap for jemalloc (584..588 MiB), mimalloc (584) and, on arm64, glibc (564; on x86_64 glibc's brk heap took 32..46 MiB). challoc commits slabs by `mprotect` inside a `PROT_NONE` reservation, so the VMA around a faulting address is only as large as the committed run and the kernel's `always` policy finds no 2 MiB-aligned range inside one VMA to fill; tcmalloc (36..40 MiB) is in the same position for its own reasons. Consequences measured here: no THP RSS inflation for challoc's small processes (cfrac 2.8 MiB, espresso 3.6), THP inflation only on the huge tier (the Python json/regex RSS rows above), and no TLB gain on the class regions where jemalloc gets the full one. `thp_deferred_split_page` under jemalloc (269 on x86_64 valkey1, 35..48 on larson16) is jemalloc's purges splitting its huge pages, memory that stays charged to the cgroup until the shrinker runs; challoc splits 0..1.

The control (`thpctl.log`): the same two rows natively under challoc and jemalloc with the host switched to `madvise` (what the Ubuntu reference hosts run), then restored to `always`:

rows (native) control run: on challoc-lane-e-cgos-arm64: aarch64 6.18.49-r2-gcp-6.18, 8 CPUs, glibc 2.44, 2026-09-20T00:07:25Z
- rows: valkey1 larson16; allocators: challoc jemalloc; 3 timed rounds after one warm-up, interleaved
- load at start: 0.02 0.84 1.85
- challoc: /home/dlorenc/native/target/bench-bin/libchalloc.so mapped=4
- jemalloc: /home/dlorenc/native/target/bench-bin/libjemalloc.so.2 mapped=4
#### Sorted scores (native)
| program | variant | sorted scores | median | maxrss MiB |
|---|---|---|---:|---:|
| valkey1 | challoc | 6.467 6.522 6.795 | 6.522 | 584.5 |
| valkey1 | jemalloc | 6.424 6.528 6.757 | 6.528 | 585.7 |
| larson16 | challoc | 295.2 297.1 297.6 | 297.1 | 173.4 |
| larson16 | jemalloc | 307.6 308.6 310.4 | 308.6 | 167.6 |
#### challoc against each (positive = challoc worse), native
| program | vs jemalloc |
|---|---:|
| valkey1 | +0.1% |
| larson16 | +3.9% |
- load at end: 5.87 2.63 2.36
#### THP under always (native): peak AnonHugePages per allocator, /proc/vmstat deltas
- thp enabled: always [madvise] never; defrag: always defer defer+madvise [madvise] never; khugepaged/max_ptes_none: 511; hugepages-2048kB: always [inherit] madvise never
| program | variant | score | peak Rss MiB | peak AnonHugePages MiB | thp_fault_alloc | thp_fault_fallback | thp_split_page | thp_deferred_split_page |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| valkey1 | challoc | 6.588 | 584.6 | 0.0 | 23 | 0 | 0 | 1 |
| valkey1 | jemalloc | 6.310 | 585.5 | 2.0 | 14 | 0 | 0 | 2 |
| larson16 | challoc | 292.6 | 173.0 | 0.0 | 9 | 0 | 0 | 0 |
| larson16 | jemalloc | 308.1 | 167.9 | 0.0 | 11 | 0 | 0 | 0 |

And challoc's own key: the same two rows natively with `MALLOC_CONF=thp:always` (`challoc-thp`) next to the default and jemalloc, host at `always` (`thpopt.log`):

rows (native), challoc `thp:always`: on challoc-lane-e-cgos-arm64: aarch64 6.18.49-r2-gcp-6.18, 8 CPUs, glibc 2.44, 2026-09-20T00:11:07Z
- rows: valkey1 larson16; allocators: challoc challoc-thp jemalloc; 3 timed rounds after one warm-up, interleaved
- load at start: 2.77 2.56 2.36
- challoc: /home/dlorenc/native/target/bench-bin/libchalloc.so mapped=4
- challoc-thp: /home/dlorenc/native/target/bench-bin/libchalloc.so mapped=4
- jemalloc: /home/dlorenc/native/target/bench-bin/libjemalloc.so.2 mapped=4
#### Sorted scores (native)
| program | variant | sorted scores | median | maxrss MiB |
|---|---|---|---:|---:|
| valkey1 | challoc | 6.978 7.014 7.120 | 7.014 | 584.7 |
| valkey1 | challoc-thp | 7.342 7.389 7.407 | 7.389 | 640.1 |
| valkey1 | jemalloc | 7.372 7.382 7.426 | 7.382 | 594.8 |
| larson16 | challoc | 301.1 302.8 304.4 | 302.8 | 176.0 |
| larson16 | challoc-thp | 301.7 302.6 302.8 | 302.6 | 242.3 |
| larson16 | jemalloc | 320.5 322.3 322.4 | 322.3 | 272.1 |
#### challoc against each (positive = challoc worse), native
| program | vs challoc-thp | vs jemalloc |
|---|---:|---:|
| valkey1 | +5.3% | +5.2% |
| larson16 | -0.1% | +6.4% |
- load at end: 6.81 4.27 3.05
#### THP under always (native): peak AnonHugePages per allocator, /proc/vmstat deltas
- thp enabled: [always] madvise never; defrag: always defer defer+madvise [madvise] never; khugepaged/max_ptes_none: 511; hugepages-2048kB: always [inherit] madvise never
| program | variant | score | peak Rss MiB | peak AnonHugePages MiB | thp_fault_alloc | thp_fault_fallback | thp_split_page | thp_deferred_split_page |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| valkey1 | challoc | 7.081 | 584.7 | 32.0 | 36 | 0 | 0 | 1 |
| valkey1 | challoc-thp | 7.408 | 640.1 | 636.0 | 328 | 0 | 0 | 280 |
| valkey1 | jemalloc | 7.485 | 596.7 | 586.0 | 305 | 0 | 0 | 3 |
| larson16 | challoc | 296.5 | 174.8 | 4.0 | 21 | 0 | 0 | 2 |
| larson16 | challoc-thp | 298.2 | 243.6 | 190.0 | 110 | 0 | 0 | 36 |
| larson16 | jemalloc | 318.7 | 269.3 | 160.0 | 106 | 0 | 0 | 36 |

Reading it: challoc's own `thp:always` key (task 9's `MADV_HUGEPAGE` on the class regions, off by default) takes the whole heap into huge pages on this host (636 MiB of AnonHugePages on the Valkey model, 190 MiB on larson16) and brings valkey1 from +5.3% behind jemalloc to parity (7.39 vs 7.38 M ops/s), for +9.5% RSS on the model (585 -> 640 MiB) and +38% on larson16 (176 -> 242 MiB, no throughput change); the 280 deferred splits are challoc's purges splitting its huge pages, as jemalloc's do. So on a THP `always` host the default `thp:never` leaves the TLB gain to jemalloc; whether to turn the key on where the report reads `thp=always` is a task 9 decision for the human with these two rows in front of them.
