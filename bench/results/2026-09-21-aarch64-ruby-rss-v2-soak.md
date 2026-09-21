# Ruby soak, 2026-09-21, aarch64

- commit: c8a8e03
- date: 2026-09-21T06:47:05Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 2.88 1.89 3.06; at end: 1.00 1.00 1.27; 1810 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 3004ed9a44554cc8), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped
  - challoc-1arena: as challoc, with MALLOC_CONF=narenas:1 (one arena for all threads)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc-1arena | challoc vs jemalloc | challoc-1arena vs challoc | challoc-1arena vs jemalloc |
|---|---:|---:|---:|---:|---:|---:|
| peak sampled VmRSS | 199.0 | 202.7 | 172.7 | -1.8% | -13.2% | -14.8% |
| VmHWM at the end | 198.7 | 205.1 | 172.3 | -3.1% | -13.3% | -16.0% |
| cgroup memory.peak, start..compact | - | - | - | - | - | - |
| cgroup memory.peak, compact..end | - | - | - | - | - | - |
| at minute 1 (t=60) | 80.3 | 97.1 | 41.6 | -17.3% | -48.2% | -57.2% |
| at minute 5, before GC.compact (t=295) | 113.0 | 134.0 | 83.1 | -15.7% | -26.5% | -38.0% |
| at minute 5, after GC.compact (t=300) | 121.0 | 130.7 | 84.0 | -7.4% | -30.6% | -35.7% |
| at minute 10 (t=600) | 155.5 | 170.3 | 171.7 | -8.7% | +10.4% | +0.8% |
| after the last full GC (final) | 155.5 | 170.3 | 171.7 | -8.7% | +10.4% | +0.8% |
| lowest right after a periodic full GC | 76.0 | 93.1 | 38.1 | -18.4% | -49.9% | -59.1% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 88.5 | 120.9 | 155.5 | 103.0 | 258.5 | 144.6 | 155.5 | 1.00 |
| jemalloc | 90.2 | 137.7 | 178.1 | 135.1 | 208.9 | 159.9 | 178.1 | 1.05 |
| challoc-1arena | 88.5 | 111.9 | 171.1 | 20.3 | 191.5 | 97.1 | 171.1 | 1.00 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 138091 | 69056 | 16454 | 167 | 16287 | 327 | 602.02 | 4.36 | 0 |
| jemalloc | 138460 | 69246 | 16552 | 153 | 16399 | 329 | 602.1 | 4.35 | 0 |
| challoc-1arena | 138265 | 69144 | 16418 | 153 | 16265 | 329 | 602.05 | 4.35 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained | challoc-1arena VmRSS | challoc-1arena resident | challoc-1arena retained |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | start | 39.2 | 76.8 | 0.0 | 41.6 | 108.8 | 30.6 | 28.6 | 55.0 | 0.0 |
| 30 | fullgc | 76.0 | 103.3 | 10.3 | 93.1 | 132.0 | 84.4 | 38.1 | 61.0 | 5.8 |
| 60 | fullgc | 80.3 | 107.1 | 10.6 | 97.1 | 133.2 | 90.1 | 41.6 | 63.8 | 6.4 |
| 90 | fullgc | 86.2 | 109.4 | 14.8 | 103.2 | 138.2 | 85.2 | 49.3 | 67.1 | 8.8 |
| 120 | fullgc | 89.7 | 112.3 | 16.9 | 106.1 | 141.1 | 82.3 | 51.6 | 68.9 | 10.6 |
| 150 | fullgc | 93.7 | 118.3 | 18.6 | 113.2 | 151.2 | 76.1 | 55.0 | 72.1 | 11.5 |
| 180 | fullgc | 99.5 | 121.4 | 20.0 | 122.2 | 159.1 | 96.3 | 64.9 | 80.4 | 9.7 |
| 210 | fullgc | 102.8 | 131.7 | 21.3 | 126.7 | 160.9 | 110.4 | 68.0 | 83.9 | 10.7 |
| 240 | fullgc | 106.6 | 132.3 | 23.6 | 130.4 | 165.2 | 114.2 | 70.7 | 87.0 | 10.6 |
| 270 | fullgc | 110.3 | 134.0 | 25.5 | 131.5 | 163.6 | 119.8 | 73.0 | 87.1 | 13.3 |
| 300 | compact | 121.0 | 144.6 | 22.4 | 130.7 | 159.9 | 127.5 | 84.0 | 97.1 | 10.5 |
| 330 | fullgc | 150.7 | 177.2 | 22.9 | 151.7 | 173.6 | 113.8 | 118.6 | 127.7 | 21.6 |
| 360 | fullgc | 183.4 | 200.7 | 31.3 | 179.2 | 201.5 | 111.6 | 143.0 | 152.2 | 11.4 |
| 390 | fullgc | 156.6 | 171.7 | 65.2 | 164.1 | 190.3 | 122.8 | 120.0 | 127.1 | 39.8 |
| 420 | fullgc | 172.5 | 188.0 | 50.4 | 179.5 | 206.9 | 106.2 | 133.2 | 139.7 | 31.9 |
| 450 | fullgc | 178.3 | 193.9 | 47.9 | 178.8 | 200.8 | 112.4 | 146.3 | 151.1 | 23.8 |
| 480 | fullgc | 182.8 | 196.3 | 49.2 | 181.5 | 197.2 | 115.9 | 150.7 | 153.3 | 24.6 |
| 510 | fullgc | 186.1 | 191.2 | 57.3 | 183.3 | 202.1 | 111.1 | 159.3 | 162.7 | 18.7 |
| 540 | fullgc | 188.1 | 193.3 | 58.3 | 189.7 | 205.4 | 107.9 | 163.5 | 165.8 | 18.9 |
| 570 | fullgc | 192.7 | 197.1 | 57.8 | 186.9 | 201.2 | 112.1 | 157.7 | 157.3 | 30.5 |
| 600 | fullgc | 199.0 | 202.7 | 55.8 | 195.5 | 208.0 | 105.2 | 172.7 | 172.4 | 19.1 |
| 600 | final-fullgc | 155.5 | 155.5 | 103.0 | 170.3 | 178.1 | 135.1 | 171.7 | 171.1 | 20.3 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-ruby-rss-v2-soak-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-v2-soak-challoc-mstats.txt`; summary line: `jobs=138091 live_objects=69056 gc_count=16454 major=167 minor=16287 heap_pages=327 rss_final_kb=159212 hwm_kb=203512 mallctl=yes alloc_allocated=92860016 alloc_active=126889984 alloc_resident=163115008 alloc_retained=107929600 alloc_mapped=271044608 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=602.02 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-aarch64-ruby-rss-v2-soak-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-v2-soak-jemalloc-mstats.txt`; summary line: `jobs=138460 live_objects=69246 gc_count=16552 major=153 minor=16399 heap_pages=329 rss_final_kb=174432 hwm_kb=210072 mallctl=yes alloc_allocated=94575472 alloc_active=144375808 alloc_resident=186777600 alloc_retained=141688832 alloc_mapped=219021312 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=602.1 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- challoc-1arena: `bench/results/2026-09-21-aarch64-ruby-rss-v2-soak-challoc-1arena.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-v2-soak-challoc-1arena-mstats.txt`; summary line: `jobs=138265 live_objects=69144 gc_count=16418 major=153 minor=16265 heap_pages=329 rss_final_kb=175824 hwm_kb=176400 mallctl=yes alloc_allocated=92891504 alloc_active=117366784 alloc_resident=179507200 alloc_retained=21258240 alloc_mapped=200765440 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=602.05 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
