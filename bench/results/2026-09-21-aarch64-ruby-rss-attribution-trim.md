# Ruby soak, 2026-09-21, aarch64

- commit: 2078f67
- date: 2026-09-21T03:57:58Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- TRIM=1: soak.rb calls malloc_trim(0) right before every sample (where the allocator exports it), so VmRSS and resident read Active plus metadata at the sample instant
- load average at start: 1.00 1.00 1.00; at end: 1.00 1.00 1.00; 309 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 28d2ce5bde45aefe), default MALLOC_CONF

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc |
|---|---:|
| peak sampled VmRSS | 306.2 |
| VmHWM at the end | 313.0 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 274.6 |
| at minute 5, before GC.compact (t=145) | 278.2 |
| at minute 5, after GC.compact (t=150) | 286.5 |
| at minute 10 (t=300) | 125.3 |
| after the last full GC (final) | 125.3 |
| lowest right after a periodic full GC | 269.9 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 54.3 | 135.0 | 143.1 | 229.6 | 372.7 | 310.7 | 143.1 | 1.14 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 68785 | 34404 | 13867 | 26 | 13841 | 162 | 301.38 | 4.38 | 62 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained |
|---:|---|---:|---:|---:|
| 0 | start | 44.8 | 83.5 | 4.7 |
| 30 | fullgc | 269.9 | 294.2 | 24.8 |
| 60 | fullgc | 274.6 | 296.1 | 28.0 |
| 90 | fullgc | 278.7 | 301.5 | 30.3 |
| 120 | fullgc | 283.4 | 303.9 | 34.1 |
| 150 | compact | 286.5 | 310.7 | 35.3 |
| 180 | fullgc | 290.5 | 310.5 | 39.5 |
| 210 | fullgc | 295.8 | 324.2 | 37.1 |
| 240 | fullgc | 298.5 | 322.6 | 42.3 |
| 270 | fullgc | 302.2 | 325.5 | 42.5 |
| 300 | fullgc | 304.8 | 327.0 | 45.5 |
| 300 | final-fullgc | 125.3 | 143.1 | 229.6 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-trim-challoc.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-trim-challoc-mstats.txt`; summary line: `jobs=68785 live_objects=34404 gc_count=13867 major=26 minor=13841 heap_pages=162 rss_final_kb=128280 hwm_kb=320528 mallctl=yes alloc_allocated=56989840 alloc_active=141623296 alloc_resident=150159360 alloc_retained=240689152 alloc_mapped=390848512 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.38 trim=yes trimmed=62 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
