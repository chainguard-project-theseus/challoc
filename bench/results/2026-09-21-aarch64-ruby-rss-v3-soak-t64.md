# Ruby soak, 2026-09-21, aarch64

- commit: aeafc1e
- date: 2026-09-21T08:30:27Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 64 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- load average at start: 2.93 4.88 4.08; at end: 1.00 1.49 2.57; 615 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 c6c05a531b866b13), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 232.3 | 311.7 | -25.5% |
| VmHWM at the end | 232.0 | 313.2 | -25.9% |
| cgroup memory.peak, start..compact | - | - | - |
| cgroup memory.peak, compact..end | - | - | - |
| at minute 1 (t=60) | 196.4 | 265.0 | -25.9% |
| at minute 5, before GC.compact (t=145) | 210.3 | 286.1 | -26.5% |
| at minute 5, after GC.compact (t=150) | 210.6 | 289.3 | -27.2% |
| at minute 10 (t=300) | 232.3 | 287.7 | -19.3% |
| after the last full GC (final) | 143.5 | 217.9 | -34.1% |
| lowest right after a periodic full GC | 195.1 | 262.1 | -25.6% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 104.4 | 167.0 | 213.9 | 142.5 | 356.4 | 282.4 | 310.5 | 1.49 |
| jemalloc | 105.8 | 252.3 | 333.5 | 487.9 | 450.1 | 448.4 | 431.1 | 1.53 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 69188 | 34644 | 13943 | 28 | 13915 | 162 | 303.8 | 4.39 | 0 |
| jemalloc | 69407 | 34750 | 13868 | 26 | 13842 | 162 | 303.81 | 4.38 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 151 | compact | 210.6 | 282.5 | 45.8 | 289.3 | 444.2 | 373.6 |
| 303 | final-fullgc | 143.5 | 213.9 | 142.5 | 217.9 | 333.5 | 487.9 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-t64-challoc.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-t64-challoc-mstats.txt`; summary line: `jobs=69188 live_objects=34644 gc_count=13943 major=28 minor=13915 heap_pages=162 rss_final_kb=146980 hwm_kb=237608 mallctl=yes alloc_allocated=109531376 alloc_active=175161344 alloc_resident=224362496 alloc_retained=149377024 alloc_mapped=373739520 compacted=true threads=64 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=303.8 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-t64-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-t64-jemalloc-mstats.txt`; summary line: `jobs=69407 live_objects=34750 gc_count=13868 major=26 minor=13842 heap_pages=162 rss_final_kb=223152 hwm_kb=320676 mallctl=yes alloc_allocated=110983536 alloc_active=264568832 alloc_resident=349700096 alloc_retained=511574016 alloc_mapped=471990272 compacted=true threads=64 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=303.81 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
