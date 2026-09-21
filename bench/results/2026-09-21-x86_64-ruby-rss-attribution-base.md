# Ruby soak, 2026-09-21, x86_64

- commit: 2078f67
- date: 2026-09-21T03:27:11Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 4.99 2.76 1.16; at end: 0.98 1.00 1.00; 1842 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 2ccf9f46d160cd96), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped
  - challoc-1arena: as challoc, with MALLOC_CONF=narenas:1 (one arena for all threads)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc-1arena | challoc vs jemalloc | challoc-1arena vs challoc | challoc-1arena vs jemalloc |
|---|---:|---:|---:|---:|---:|---:|
| peak sampled VmRSS | 416.8 | 182.0 | 184.4 | +129.0% | -55.8% | +1.3% |
| VmHWM at the end | 418.2 | 185.1 | 184.2 | +125.9% | -56.0% | -0.5% |
| cgroup memory.peak, start..compact | - | - | - | - | - | - |
| cgroup memory.peak, compact..end | - | - | - | - | - | - |
| at minute 1 (t=60) | 311.4 | 72.3 | 60.3 | +330.7% | -80.6% | -16.6% |
| at minute 5, before GC.compact (t=295) | 339.4 | 117.2 | 120.5 | +189.6% | -64.5% | +2.8% |
| at minute 5, after GC.compact (t=300) | 353.4 | 118.4 | 124.5 | +198.5% | -64.8% | +5.2% |
| at minute 10 (t=600) | 411.0 | 136.2 | 184.2 | +201.8% | -55.2% | +35.2% |
| after the last full GC (final) | 411.0 | 136.2 | 184.2 | +201.8% | -55.2% | +35.2% |
| lowest right after a periodic full GC | 302.0 | 71.8 | 59.0 | +320.6% | -80.5% | -17.8% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 104.2 | 213.0 | 414.5 | 68.7 | 483.2 | 382.2 | 414.5 | 1.01 |
| jemalloc | 93.5 | 105.9 | 125.1 | 92.5 | 154.5 | 117.0 | 125.1 | 0.92 |
| challoc-1arena | 95.4 | 119.6 | 183.7 | 16.3 | 200.1 | 135.5 | 183.7 | 1.00 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 148159 | 74090 | 16403 | 1409 | 14994 | 345 | 593.43 | 4.01 | 0 |
| jemalloc | 148545 | 74284 | 16439 | 1425 | 15014 | 349 | 592.8 | 3.99 | 0 |
| challoc-1arena | 151802 | 75916 | 16491 | 1387 | 15104 | 353 | 593.78 | 3.91 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained | challoc-1arena VmRSS | challoc-1arena resident | challoc-1arena retained |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | start | 41.5 | 83.8 | 0.0 | 39.0 | 56.3 | 15.0 | 28.6 | 57.8 | 0.0 |
| 30 | fullgc | 302.0 | 329.9 | 8.0 | 71.8 | 78.9 | 32.8 | 60.8 | 83.5 | 5.0 |
| 60 | fullgc | 311.4 | 339.3 | 15.2 | 72.3 | 78.7 | 36.1 | 60.3 | 82.6 | 8.7 |
| 90 | fullgc | 311.8 | 335.6 | 25.8 | 72.5 | 78.2 | 36.6 | 59.0 | 77.5 | 16.4 |
| 120 | fullgc | 313.8 | 335.9 | 27.6 | 90.4 | 94.5 | 59.5 | 80.8 | 99.0 | 8.6 |
| 150 | fullgc | 331.6 | 355.8 | 26.0 | 91.5 | 95.2 | 64.9 | 80.4 | 95.7 | 14.8 |
| 180 | fullgc | 329.6 | 353.5 | 31.0 | 92.0 | 95.0 | 65.1 | 78.9 | 93.6 | 18.5 |
| 210 | fullgc | 330.3 | 358.9 | 36.2 | 92.0 | 94.4 | 65.8 | 78.1 | 93.1 | 22.1 |
| 240 | fullgc | 335.8 | 359.9 | 38.4 | 97.6 | 99.0 | 61.2 | 85.4 | 100.5 | 19.1 |
| 270 | fullgc | 332.8 | 358.0 | 44.1 | 96.8 | 97.5 | 62.7 | 86.6 | 99.4 | 22.3 |
| 300 | compact | 353.4 | 382.2 | 34.5 | 118.4 | 117.0 | 43.4 | 124.5 | 135.5 | 12.1 |
| 330 | fullgc | 384.8 | 403.4 | 37.9 | 148.7 | 144.1 | 73.1 | 120.3 | 127.6 | 37.5 |
| 360 | fullgc | 366.4 | 381.9 | 73.4 | 131.0 | 127.0 | 90.4 | 148.3 | 157.4 | 12.2 |
| 390 | fullgc | 381.7 | 398.4 | 58.8 | 150.4 | 145.6 | 71.7 | 152.5 | 162.8 | 11.3 |
| 420 | fullgc | 387.0 | 403.6 | 57.9 | 145.9 | 140.0 | 77.3 | 143.0 | 147.3 | 30.2 |
| 450 | fullgc | 393.7 | 408.4 | 56.6 | 154.6 | 148.1 | 69.3 | 151.7 | 153.2 | 28.4 |
| 480 | fullgc | 396.9 | 401.2 | 67.6 | 159.4 | 151.9 | 65.4 | 161.4 | 161.9 | 23.3 |
| 510 | fullgc | 396.3 | 400.9 | 70.4 | 169.0 | 160.9 | 56.5 | 171.6 | 174.6 | 14.6 |
| 540 | fullgc | 401.7 | 405.7 | 69.8 | 170.6 | 161.4 | 56.1 | 169.1 | 170.5 | 22.1 |
| 570 | fullgc | 404.0 | 407.3 | 70.7 | 178.7 | 168.7 | 48.8 | 165.3 | 163.5 | 33.5 |
| 600 | fullgc | 410.6 | 414.4 | 68.7 | 182.0 | 171.4 | 46.2 | 184.4 | 184.1 | 15.9 |
| 600 | final-fullgc | 411.0 | 414.5 | 68.7 | 136.2 | 125.1 | 92.5 | 184.2 | 183.7 | 16.3 |

## Samples

- challoc: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-base-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-base-challoc-mstats.txt`; summary line: `jobs=148159 live_objects=74090 gc_count=16403 major=1409 minor=14994 heap_pages=345 rss_final_kb=420884 hwm_kb=428244 mallctl=yes alloc_allocated=109316496 alloc_active=223453184 alloc_resident=434814976 alloc_retained=71856128 alloc_mapped=506671104 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=593.43 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-base-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-base-jemalloc-mstats.txt`; summary line: `jobs=148545 live_objects=74284 gc_count=16439 major=1425 minor=15014 heap_pages=349 rss_final_kb=139500 hwm_kb=189556 mallctl=yes alloc_allocated=98034880 alloc_active=111063040 alloc_resident=131166208 alloc_retained=96980992 alloc_mapped=162017280 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=592.8 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- challoc-1arena: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-base-challoc-1arena.csv` (122 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-base-challoc-1arena-mstats.txt`; summary line: `jobs=151802 live_objects=75916 gc_count=16491 major=1387 minor=15104 heap_pages=353 rss_final_kb=188572 hwm_kb=188572 mallctl=yes alloc_allocated=100105520 alloc_active=125472768 alloc_resident=192733184 alloc_retained=17039360 alloc_mapped=209772544 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=593.78 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
