# Ruby soak, 2026-09-20, aarch64

- commit: a999097
- date: 2026-09-20T08:39:22Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 60 s, 16 threads + 48 mostly idle threads, seed 4141, sample every 5 s, full GC every 15 s, GC.compact at 30 s, one full GC after the workers stop (the final row)
- load average at start: 6.27 2.99 1.17; at end: 1.52 2.23 1.14; 144 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 fc48800a58f20921), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 357.7 | 170.4 | +109.9% |
| VmHWM at the end | 357.1 | 170.6 | +109.3% |
| cgroup memory.peak, start..compact | - | - | - |
| cgroup memory.peak, compact..end | - | - | - |
| at minute 1 (t=60) | 357.7 | 109.1 | +227.9% |
| at minute 5, before GC.compact (t=25) | 338.2 | 165.2 | +104.7% |
| at minute 5, after GC.compact (t=30) | 346.8 | 159.8 | +117.0% |
| at minute 10 (t=60) | 357.7 | 109.1 | +227.9% |
| after the last full GC (final) | 356.7 | 109.1 | +226.9% |
| lowest right after a periodic full GC | 334.9 | 157.6 | +112.5% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=30 | resident at t=60 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 79.7 | 155.5 | 450.4 | 33.2 | 483.5 | 445.7 | 451.3 | 1.26 |
| jemalloc | 78.8 | 153.3 | 197.3 | 315.6 | 316.9 | 341.6 | 197.3 | 1.81 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 13685 | 6858 | 3660 | 10 | 3650 | 69 |
| jemalloc | 13893 | 6962 | 3739 | 11 | 3728 | 69 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 15th second is a forced full GC, the row at t=30 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 108.6 | 228.2 | 0.0 | 105.3 | 329.1 | 108.8 |
| 30 | compact | 346.8 | 445.7 | 22.3 | 159.8 | 341.6 | 171.3 |
| 60 | fullgc | 357.7 | 451.3 | 32.1 | 170.4 | 346.2 | 166.7 |
| 61 | final-fullgc | 356.7 | 450.4 | 33.2 | 109.1 | 197.3 | 315.6 |

## Samples

- challoc: `bench/results/2026-09-20-aarch64-lane-d-final-ruby-idle-short-challoc.csv` (14 rows), allocator report `bench/results/2026-09-20-aarch64-lane-d-final-ruby-idle-short-challoc-mstats.txt`; summary line: `jobs=13685 live_objects=6858 gc_count=3660 major=10 minor=3650 heap_pages=69 rss_final_kb=365252 hwm_kb=365620 mallctl=yes alloc_allocated=83564880 alloc_active=163061760 alloc_resident=472248320 alloc_retained=34787328 alloc_mapped=507035648 compacted=true threads=16 idle_threads=48 idle_bursts=433 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-20-aarch64-lane-d-final-ruby-idle-short-jemalloc.csv` (14 rows), allocator report `bench/results/2026-09-20-aarch64-lane-d-final-ruby-idle-short-jemalloc-mstats.txt`; summary line: `jobs=13893 live_objects=6962 gc_count=3739 major=11 minor=3728 heap_pages=69 rss_final_kb=111692 hwm_kb=174736 mallctl=yes alloc_allocated=82628840 alloc_active=160759808 alloc_resident=206897152 alloc_retained=330956800 alloc_mapped=332267520 compacted=true threads=16 idle_threads=48 idle_bursts=434 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
