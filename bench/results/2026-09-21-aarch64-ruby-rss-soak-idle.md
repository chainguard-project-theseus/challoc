# Ruby soak, 2026-09-21, aarch64

- commit: 6b29fa9
- date: 2026-09-21T06:10:01Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 48 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- load average at start: 1.00 1.52 2.61; at end: 1.01 1.07 1.82; 611 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 6287c62a3cb3cc32), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 135.0 | 192.5 | -29.9% |
| VmHWM at the end | 134.5 | 193.3 | -30.4% |
| cgroup memory.peak, start..compact | - | - | - |
| cgroup memory.peak, compact..end | - | - | - |
| at minute 1 (t=60) | 97.0 | 164.9 | -41.2% |
| at minute 5, before GC.compact (t=145) | 110.9 | 176.3 | -37.1% |
| at minute 5, after GC.compact (t=150) | 111.9 | 180.5 | -38.0% |
| at minute 10 (t=300) | 135.0 | 186.4 | -27.6% |
| after the last full GC (final) | 95.2 | 135.1 | -29.5% |
| lowest right after a periodic full GC | 91.5 | 161.9 | -43.5% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 103.9 | 140.7 | 164.6 | 126.2 | 290.8 | 200.2 | 220.3 | 1.73 |
| jemalloc | 105.2 | 181.2 | 228.3 | 349.1 | 347.4 | 350.8 | 349.5 | 1.69 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 68482 | 34252 | 13804 | 29 | 13775 | 162 | 301.81 | 4.41 | 0 |
| jemalloc | 68702 | 34364 | 13799 | 30 | 13769 | 162 | 301.84 | 4.39 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 95.9 | 205.7 | 0.0 | 104.8 | 328.6 | 106.8 |
| 30 | fullgc | 91.5 | 188.7 | 48.5 | 161.9 | 344.0 | 162.4 |
| 60 | fullgc | 97.0 | 189.1 | 55.1 | 164.9 | 344.1 | 169.3 |
| 90 | fullgc | 103.1 | 191.1 | 60.1 | 170.9 | 350.5 | 162.9 |
| 120 | fullgc | 106.3 | 195.0 | 62.3 | 173.5 | 343.8 | 173.7 |
| 150 | compact | 111.9 | 200.2 | 65.6 | 180.5 | 350.8 | 178.7 |
| 180 | fullgc | 117.8 | 201.8 | 68.2 | 184.3 | 353.8 | 199.6 |
| 210 | fullgc | 122.4 | 213.2 | 68.0 | 182.8 | 352.8 | 216.6 |
| 240 | fullgc | 127.4 | 215.4 | 69.1 | 189.8 | 357.2 | 220.2 |
| 270 | fullgc | 129.1 | 214.2 | 73.0 | 192.1 | 357.9 | 219.6 |
| 300 | fullgc | 135.0 | 220.3 | 70.5 | 186.4 | 349.5 | 227.9 |
| 301 | final-fullgc | 95.2 | 164.6 | 126.2 | 135.1 | 228.3 | 349.1 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-ruby-rss-soak-idle-challoc.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-soak-idle-challoc-mstats.txt`; summary line: `jobs=68482 live_objects=34252 gc_count=13804 major=29 minor=13775 heap_pages=162 rss_final_kb=97512 hwm_kb=137688 mallctl=yes alloc_allocated=108985904 alloc_active=147599360 alloc_resident=172675072 alloc_retained=132296704 alloc_mapped=304971776 compacted=true threads=16 idle_threads=48 idle_bursts=2070 nproc=16 cpu_s=301.81 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-aarch64-ruby-rss-soak-idle-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-soak-idle-jemalloc-mstats.txt`; summary line: `jobs=68702 live_objects=34364 gc_count=13799 major=30 minor=13769 heap_pages=162 rss_final_kb=138316 hwm_kb=197924 mallctl=yes alloc_allocated=110358688 alloc_active=189988864 alloc_resident=239403008 alloc_retained=366084096 alloc_mapped=364249088 compacted=true threads=16 idle_threads=48 idle_bursts=2070 nproc=16 cpu_s=301.84 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
