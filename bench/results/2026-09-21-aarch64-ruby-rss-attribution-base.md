# Ruby soak, 2026-09-21, aarch64

- commit: 2078f67
- date: 2026-09-21T03:27:07Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 2.08 1.78 0.77; at end: 1.00 1.00 1.00; 1841 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 28d2ce5bde45aefe), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped
  - challoc-1arena: as challoc, with MALLOC_CONF=narenas:1 (one arena for all threads)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc-1arena | challoc vs jemalloc | challoc-1arena vs challoc | challoc-1arena vs jemalloc |
|---|---:|---:|---:|---:|---:|---:|
| peak sampled VmRSS | 399.3 | 203.0 | 170.5 | +96.7% | -57.3% | -16.0% |
| VmHWM at the end | 400.6 | 207.9 | 170.5 | +92.7% | -57.4% | -18.0% |
| cgroup memory.peak, start..compact | - | - | - | - | - | - |
| cgroup memory.peak, compact..end | - | - | - | - | - | - |
| at minute 1 (t=60) | 286.8 | 99.5 | 41.7 | +188.2% | -85.5% | -58.1% |
| at minute 5, before GC.compact (t=295) | 313.4 | 131.0 | 83.2 | +139.2% | -73.5% | -36.5% |
| at minute 5, after GC.compact (t=300) | 324.7 | 128.9 | 83.7 | +151.9% | -74.2% | -35.1% |
| at minute 10 (t=600) | 393.2 | 167.7 | 167.9 | +134.5% | -57.3% | +0.1% |
| after the last full GC (final) | 393.2 | 167.7 | 167.9 | +134.5% | -57.3% | +0.1% |
| lowest right after a periodic full GC | 281.4 | 95.1 | 38.4 | +195.9% | -86.4% | -59.6% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 98.9 | 204.9 | 397.3 | 72.2 | 469.5 | 348.9 | 397.3 | 1.01 |
| jemalloc | 90.3 | 139.1 | 175.5 | 132.8 | 206.2 | 157.1 | 175.5 | 1.05 |
| challoc-1arena | 88.6 | 113.0 | 167.6 | 23.3 | 190.9 | 96.8 | 167.6 | 1.00 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 137218 | 68618 | 16442 | 155 | 16287 | 324 | 601.98 | 4.39 | 0 |
| jemalloc | 138297 | 69158 | 16454 | 158 | 16296 | 331 | 602.08 | 4.35 | 0 |
| challoc-1arena | 138312 | 69166 | 16471 | 159 | 16312 | 327 | 602.01 | 4.35 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained | challoc-1arena VmRSS | challoc-1arena resident | challoc-1arena retained |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | start | 40.9 | 82.9 | 0.0 | 41.6 | 108.6 | 30.8 | 28.5 | 54.9 | 0.0 |
| 30 | fullgc | 281.4 | 310.1 | 10.0 | 95.1 | 136.8 | 79.6 | 38.4 | 61.2 | 6.0 |
| 60 | fullgc | 286.8 | 313.6 | 13.3 | 99.5 | 134.3 | 89.1 | 41.7 | 63.9 | 6.7 |
| 90 | fullgc | 290.7 | 313.9 | 19.8 | 104.0 | 138.9 | 84.5 | 49.4 | 68.8 | 7.5 |
| 120 | fullgc | 295.3 | 319.3 | 21.0 | 106.0 | 144.0 | 83.4 | 51.6 | 68.7 | 10.8 |
| 150 | fullgc | 299.8 | 325.0 | 24.0 | 111.5 | 151.2 | 80.2 | 54.5 | 71.7 | 11.7 |
| 180 | fullgc | 304.2 | 327.1 | 25.5 | 121.4 | 156.1 | 95.3 | 65.1 | 80.4 | 10.0 |
| 210 | fullgc | 310.3 | 339.5 | 26.7 | 125.8 | 160.8 | 102.6 | 68.1 | 84.0 | 11.0 |
| 240 | fullgc | 315.1 | 342.1 | 28.3 | 127.1 | 164.7 | 122.7 | 70.5 | 87.2 | 10.7 |
| 270 | fullgc | 318.3 | 342.5 | 31.3 | 132.2 | 165.8 | 121.6 | 73.0 | 87.2 | 13.8 |
| 300 | compact | 324.7 | 348.9 | 28.8 | 128.9 | 157.1 | 130.2 | 83.7 | 96.8 | 10.7 |
| 330 | fullgc | 350.9 | 378.7 | 28.2 | 154.6 | 176.3 | 111.1 | 118.1 | 126.8 | 23.0 |
| 360 | fullgc | 392.2 | 411.0 | 35.3 | 203.0 | 230.3 | 68.0 | 143.0 | 152.0 | 12.8 |
| 390 | fullgc | 357.8 | 374.4 | 73.2 | 169.1 | 190.4 | 112.9 | 121.0 | 129.6 | 37.1 |
| 420 | fullgc | 376.9 | 393.0 | 57.3 | 173.7 | 193.9 | 109.4 | 147.3 | 154.7 | 16.0 |
| 450 | fullgc | 382.6 | 399.4 | 54.9 | 183.9 | 201.2 | 102.1 | 142.1 | 147.7 | 26.5 |
| 480 | fullgc | 382.6 | 397.5 | 59.7 | 184.3 | 200.1 | 103.2 | 147.1 | 150.3 | 27.1 |
| 510 | fullgc | 386.9 | 394.0 | 67.0 | 187.1 | 202.4 | 100.9 | 156.1 | 158.7 | 21.9 |
| 540 | fullgc | 380.0 | 385.0 | 79.2 | 193.8 | 209.4 | 98.9 | 163.5 | 165.7 | 18.4 |
| 570 | fullgc | 387.8 | 393.8 | 73.1 | 194.4 | 205.6 | 102.7 | 151.2 | 152.6 | 35.0 |
| 600 | fullgc | 392.6 | 397.0 | 72.4 | 195.2 | 207.6 | 100.8 | 170.3 | 169.9 | 21.0 |
| 600 | final-fullgc | 393.2 | 397.3 | 72.2 | 167.7 | 175.5 | 132.8 | 167.9 | 167.6 | 23.3 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-base-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-base-challoc-mstats.txt`; summary line: `jobs=137218 live_objects=68618 gc_count=16442 major=155 minor=16287 heap_pages=324 rss_final_kb=402612 hwm_kb=410232 mallctl=yes alloc_allocated=103777264 alloc_active=214884352 alloc_resident=416653312 alloc_retained=75632640 alloc_mapped=492285952 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=601.98 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-base-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-base-jemalloc-mstats.txt`; summary line: `jobs=138297 live_objects=69158 gc_count=16454 major=158 minor=16296 heap_pages=331 rss_final_kb=171764 hwm_kb=212928 mallctl=yes alloc_allocated=94658464 alloc_active=145883136 alloc_resident=184025088 alloc_retained=139264000 alloc_mapped=216203264 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=602.08 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- challoc-1arena: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-base-challoc-1arena.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-base-challoc-1arena-mstats.txt`; summary line: `jobs=138312 live_objects=69166 gc_count=16471 major=159 minor=16312 heap_pages=327 rss_final_kb=171900 hwm_kb=174596 mallctl=yes alloc_allocated=92935808 alloc_active=118509568 alloc_resident=175812608 alloc_retained=24346624 alloc_mapped=200159232 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=602.01 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
