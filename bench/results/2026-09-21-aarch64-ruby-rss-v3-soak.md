# Ruby soak, 2026-09-21, aarch64

- commit: aeafc1e
- date: 2026-09-21T07:42:33Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 2.46 2.67 3.25; at end: 1.00 1.00 1.28; 1810 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 c6c05a531b866b13), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped
  - challoc-1arena: as challoc, with MALLOC_CONF=narenas:1 (one arena for all threads)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc-1arena | challoc vs jemalloc | challoc-1arena vs challoc | challoc-1arena vs jemalloc |
|---|---:|---:|---:|---:|---:|---:|
| peak sampled VmRSS | 189.4 | 200.8 | 170.0 | -5.7% | -10.2% | -15.3% |
| VmHWM at the end | 189.9 | 206.5 | 170.8 | -8.0% | -10.1% | -17.3% |
| cgroup memory.peak, start..compact | - | - | - | - | - | - |
| cgroup memory.peak, compact..end | - | - | - | - | - | - |
| at minute 1 (t=60) | 70.3 | 99.0 | 41.7 | -29.0% | -40.7% | -57.9% |
| at minute 5, before GC.compact (t=295) | 104.1 | 131.5 | 83.4 | -20.8% | -19.9% | -36.6% |
| at minute 5, after GC.compact (t=300) | 111.3 | 129.2 | 84.0 | -13.9% | -24.5% | -35.0% |
| at minute 10 (t=600) | 146.2 | 169.2 | 165.2 | -13.6% | +13.0% | -2.4% |
| after the last full GC (final) | 146.2 | 169.2 | 165.2 | -13.6% | +13.0% | -2.4% |
| lowest right after a periodic full GC | 65.7 | 92.9 | 37.9 | -29.3% | -42.3% | -59.2% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 88.1 | 117.8 | 147.0 | 100.8 | 247.8 | 135.5 | 147.0 | 1.01 |
| jemalloc | 90.2 | 140.7 | 177.7 | 150.6 | 208.4 | 157.6 | 177.7 | 1.05 |
| challoc-1arena | 88.7 | 112.0 | 165.8 | 25.1 | 190.8 | 97.0 | 165.8 | 1.00 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 138286 | 69154 | 16452 | 150 | 16302 | 330 | 602.06 | 4.35 | 0 |
| jemalloc | 138390 | 69210 | 16378 | 152 | 16226 | 330 | 602.09 | 4.35 | 0 |
| challoc-1arena | 138409 | 69218 | 16449 | 157 | 16292 | 326 | 602.05 | 4.35 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained | challoc-1arena VmRSS | challoc-1arena resident | challoc-1arena retained |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | start | 38.4 | 72.7 | 0.0 | 41.6 | 108.1 | 28.3 | 28.6 | 54.9 | 0.0 |
| 30 | fullgc | 65.7 | 93.0 | 7.1 | 92.9 | 132.3 | 87.6 | 37.9 | 60.9 | 5.9 |
| 60 | fullgc | 70.3 | 96.7 | 8.1 | 99.0 | 135.3 | 88.1 | 41.7 | 64.2 | 6.7 |
| 90 | fullgc | 76.1 | 98.3 | 13.0 | 105.1 | 142.2 | 81.1 | 49.3 | 67.3 | 8.9 |
| 120 | fullgc | 79.5 | 102.2 | 14.4 | 106.4 | 144.2 | 83.2 | 51.0 | 67.9 | 11.5 |
| 150 | fullgc | 83.8 | 108.3 | 16.1 | 114.7 | 153.2 | 86.2 | 54.6 | 71.6 | 12.0 |
| 180 | fullgc | 89.6 | 111.2 | 17.2 | 122.3 | 155.6 | 99.8 | 65.4 | 80.7 | 9.7 |
| 210 | fullgc | 93.6 | 122.4 | 18.0 | 126.1 | 161.0 | 98.4 | 68.0 | 84.0 | 10.8 |
| 240 | fullgc | 96.7 | 122.5 | 21.2 | 128.9 | 163.9 | 111.4 | 70.7 | 87.3 | 10.7 |
| 270 | fullgc | 100.4 | 123.6 | 23.6 | 130.4 | 161.2 | 122.1 | 73.1 | 87.2 | 13.7 |
| 300 | compact | 111.3 | 135.5 | 20.3 | 129.2 | 157.6 | 125.8 | 84.0 | 97.0 | 10.8 |
| 330 | fullgc | 139.7 | 166.0 | 22.7 | 154.7 | 180.4 | 106.9 | 117.7 | 127.4 | 23.2 |
| 360 | fullgc | 172.4 | 189.9 | 33.7 | 183.8 | 207.1 | 100.9 | 144.9 | 154.0 | 11.4 |
| 390 | fullgc | 143.8 | 159.1 | 66.8 | 167.4 | 192.4 | 135.8 | 121.0 | 129.7 | 37.5 |
| 420 | fullgc | 161.5 | 176.6 | 51.2 | 178.7 | 204.9 | 123.2 | 147.2 | 155.6 | 15.8 |
| 450 | fullgc | 166.7 | 182.0 | 48.7 | 179.5 | 202.5 | 125.7 | 142.8 | 148.2 | 26.5 |
| 480 | fullgc | 171.3 | 186.0 | 47.8 | 184.8 | 202.6 | 125.6 | 146.2 | 149.5 | 28.4 |
| 510 | fullgc | 175.0 | 179.5 | 58.1 | 186.5 | 201.9 | 126.2 | 154.6 | 156.8 | 24.0 |
| 540 | fullgc | 180.4 | 184.7 | 56.4 | 191.0 | 205.6 | 122.6 | 163.9 | 165.5 | 18.8 |
| 570 | fullgc | 182.2 | 185.3 | 58.8 | 192.3 | 204.1 | 124.1 | 148.8 | 149.5 | 38.3 |
| 600 | fullgc | 189.4 | 192.1 | 55.7 | 194.7 | 206.5 | 121.8 | 169.2 | 168.7 | 22.1 |
| 600 | final-fullgc | 146.2 | 147.0 | 100.8 | 169.2 | 177.7 | 150.6 | 165.2 | 165.8 | 25.1 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-challoc-mstats.txt`; summary line: `jobs=138286 live_objects=69154 gc_count=16452 major=150 minor=16302 heap_pages=330 rss_final_kb=149692 hwm_kb=194436 mallctl=yes alloc_allocated=92383872 alloc_active=123572224 alloc_resident=154238976 alloc_retained=105598976 alloc_mapped=259837952 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=602.06 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-jemalloc-mstats.txt`; summary line: `jobs=138390 live_objects=69210 gc_count=16378 major=152 minor=16226 heap_pages=330 rss_final_kb=173292 hwm_kb=211476 mallctl=yes alloc_allocated=94542144 alloc_active=147521536 alloc_resident=186318848 alloc_retained=157876224 alloc_mapped=218562560 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=602.09 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- challoc-1arena: `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-challoc-1arena.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-challoc-1arena-mstats.txt`; summary line: `jobs=138409 live_objects=69218 gc_count=16449 major=157 minor=16292 heap_pages=326 rss_final_kb=169192 hwm_kb=174884 mallctl=yes alloc_allocated=93073008 alloc_active=117563392 alloc_resident=173936640 alloc_retained=26157056 alloc_mapped=200093696 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=602.05 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
