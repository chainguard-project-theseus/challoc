# Ruby soak, 2026-09-21, aarch64

- commit: 6b29fa9
- date: 2026-09-21T05:11:41Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 3.10 1.98 1.36; at end: 1.03 1.02 1.03; 1810 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 6287c62a3cb3cc32), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped
  - challoc-1arena: as challoc, with MALLOC_CONF=narenas:1 (one arena for all threads)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc-1arena | challoc vs jemalloc | challoc-1arena vs challoc | challoc-1arena vs jemalloc |
|---|---:|---:|---:|---:|---:|---:|
| peak sampled VmRSS | 186.6 | 211.0 | 170.2 | -11.6% | -8.8% | -19.3% |
| VmHWM at the end | 186.4 | 210.9 | 170.7 | -11.6% | -8.4% | -19.1% |
| cgroup memory.peak, start..compact | - | - | - | - | - | - |
| cgroup memory.peak, compact..end | - | - | - | - | - | - |
| at minute 1 (t=60) | 72.0 | 95.4 | 41.9 | -24.5% | -41.8% | -56.1% |
| at minute 5, before GC.compact (t=295) | 101.5 | 126.6 | 74.5 | -19.8% | -26.6% | -41.2% |
| at minute 5, after GC.compact (t=300) | 107.0 | 127.1 | 83.8 | -15.8% | -21.7% | -34.1% |
| at minute 10 (t=600) | 146.1 | 171.4 | 168.3 | -14.8% | +15.2% | -1.8% |
| after the last full GC (final) | 146.1 | 171.4 | 168.3 | -14.8% | +15.2% | -1.8% |
| lowest right after a periodic full GC | 65.4 | 92.9 | 38.1 | -29.6% | -41.7% | -59.0% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 88.6 | 115.6 | 147.1 | 102.5 | 249.6 | 131.9 | 147.1 | 1.01 |
| jemalloc | 90.2 | 137.9 | 179.4 | 143.9 | 210.1 | 159.6 | 179.4 | 1.05 |
| challoc-1arena | 88.6 | 111.8 | 168.8 | 21.4 | 190.2 | 96.9 | 168.8 | 1.00 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 138257 | 69142 | 16510 | 160 | 16350 | 328 | 602.04 | 4.35 | 0 |
| jemalloc | 138299 | 69162 | 16217 | 150 | 16067 | 330 | 602.1 | 4.35 | 0 |
| challoc-1arena | 138344 | 69182 | 16278 | 147 | 16131 | 327 | 602.0 | 4.35 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained | challoc-1arena VmRSS | challoc-1arena resident | challoc-1arena retained |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | start | 37.4 | 71.0 | 0.0 | 41.6 | 107.9 | 28.5 | 28.6 | 54.9 | 0.0 |
| 30 | fullgc | 65.4 | 94.0 | 9.3 | 92.9 | 134.5 | 81.9 | 38.1 | 61.0 | 5.8 |
| 60 | fullgc | 72.0 | 99.7 | 11.0 | 95.4 | 132.9 | 90.5 | 41.9 | 64.1 | 6.5 |
| 90 | fullgc | 75.7 | 98.9 | 17.4 | 105.1 | 141.5 | 81.9 | 49.3 | 67.2 | 9.2 |
| 120 | fullgc | 79.0 | 102.7 | 18.9 | 104.8 | 142.2 | 85.2 | 51.4 | 68.6 | 11.3 |
| 150 | fullgc | 82.8 | 108.5 | 20.5 | 115.0 | 152.7 | 86.7 | 54.6 | 71.7 | 12.0 |
| 180 | fullgc | 90.4 | 113.8 | 20.5 | 122.5 | 157.9 | 93.4 | 65.0 | 80.4 | 10.1 |
| 210 | fullgc | 94.2 | 123.5 | 22.1 | 126.7 | 162.9 | 104.4 | 68.1 | 84.1 | 10.9 |
| 240 | fullgc | 97.3 | 123.8 | 25.8 | 128.5 | 167.3 | 112.1 | 70.5 | 86.8 | 10.9 |
| 270 | fullgc | 99.7 | 124.0 | 27.7 | 128.2 | 160.7 | 122.7 | 73.1 | 87.0 | 13.7 |
| 300 | compact | 107.0 | 131.9 | 24.3 | 127.1 | 159.6 | 123.8 | 83.8 | 96.9 | 10.4 |
| 330 | fullgc | 136.5 | 162.6 | 25.2 | 149.5 | 175.4 | 112.0 | 118.0 | 127.0 | 22.4 |
| 360 | fullgc | 169.6 | 187.4 | 38.8 | 205.9 | 233.7 | 69.6 | 143.2 | 152.1 | 11.9 |
| 390 | fullgc | 140.7 | 155.8 | 73.5 | 169.9 | 193.1 | 130.2 | 120.4 | 129.3 | 36.8 |
| 420 | fullgc | 159.0 | 175.5 | 55.4 | 171.5 | 194.2 | 129.1 | 148.1 | 155.4 | 15.3 |
| 450 | fullgc | 163.7 | 179.2 | 54.4 | 179.4 | 201.2 | 122.1 | 141.9 | 147.9 | 25.5 |
| 480 | fullgc | 168.6 | 181.4 | 55.2 | 183.9 | 201.8 | 121.6 | 148.4 | 151.7 | 25.2 |
| 510 | fullgc | 172.0 | 177.4 | 61.6 | 188.9 | 205.0 | 118.3 | 156.3 | 158.6 | 21.4 |
| 540 | fullgc | 177.0 | 181.2 | 61.6 | 192.0 | 207.8 | 115.5 | 163.7 | 166.1 | 17.6 |
| 570 | fullgc | 178.6 | 181.7 | 64.4 | 192.8 | 203.7 | 119.6 | 153.1 | 153.7 | 33.3 |
| 600 | fullgc | 186.6 | 190.3 | 59.3 | 196.1 | 207.6 | 115.8 | 170.2 | 170.5 | 19.7 |
| 600 | final-fullgc | 146.1 | 147.1 | 102.5 | 171.4 | 179.4 | 143.9 | 168.3 | 168.8 | 21.4 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-ruby-rss-soak-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-soak-challoc-mstats.txt`; summary line: `jobs=138257 live_objects=69142 gc_count=16510 major=160 minor=16350 heap_pages=328 rss_final_kb=149652 hwm_kb=190872 mallctl=yes alloc_allocated=92946080 alloc_active=121282560 alloc_resident=154341376 alloc_retained=107413504 alloc_mapped=261754880 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=602.04 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-aarch64-ruby-rss-soak-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-soak-jemalloc-mstats.txt`; summary line: `jobs=138299 live_objects=69162 gc_count=16217 major=150 minor=16067 heap_pages=330 rss_final_kb=175476 hwm_kb=216008 mallctl=yes alloc_allocated=94540944 alloc_active=144572416 alloc_resident=188153856 alloc_retained=150863872 alloc_mapped=220332032 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=602.1 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- challoc-1arena: `bench/results/2026-09-21-aarch64-ruby-rss-soak-challoc-1arena.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-soak-challoc-1arena-mstats.txt`; summary line: `jobs=138344 live_objects=69182 gc_count=16278 major=147 minor=16131 heap_pages=327 rss_final_kb=172352 hwm_kb=174792 mallctl=yes alloc_allocated=92962992 alloc_active=117297152 alloc_resident=177041408 alloc_retained=22421504 alloc_mapped=199462912 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=602.0 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
