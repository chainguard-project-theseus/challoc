# Ruby soak, 2026-09-21, aarch64

- commit: aeafc1e
- date: 2026-09-21T08:40:42Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 48 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- load average at start: 1.00 1.49 2.57; at end: 1.00 1.06 1.80; 611 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 c6c05a531b866b13), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 143.1 | 188.7 | -24.2% |
| VmHWM at the end | 142.9 | 189.7 | -24.7% |
| cgroup memory.peak, start..compact | - | - | - |
| cgroup memory.peak, compact..end | - | - | - |
| at minute 1 (t=60) | 99.7 | 161.7 | -38.3% |
| at minute 5, before GC.compact (t=145) | 116.5 | 176.7 | -34.1% |
| at minute 5, after GC.compact (t=150) | 117.7 | 176.5 | -33.3% |
| at minute 10 (t=300) | 93.9 | 129.3 | -27.4% |
| after the last full GC (final) | 93.9 | 129.3 | -27.4% |
| lowest right after a periodic full GC | 95.0 | 160.8 | -40.9% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 103.3 | 135.0 | 162.2 | 134.6 | 296.8 | 205.8 | 162.2 | 1.73 |
| jemalloc | 104.7 | 184.8 | 224.6 | 352.4 | 344.1 | 344.9 | 224.6 | 1.74 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 68366 | 34196 | 13665 | 27 | 13638 | 162 | 301.76 | 4.41 | 0 |
| jemalloc | 68613 | 34318 | 13864 | 31 | 13833 | 162 | 301.91 | 4.40 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 102.3 | 211.7 | 0.0 | 104.9 | 328.2 | 106.7 |
| 30 | fullgc | 95.0 | 191.8 | 46.4 | 160.8 | 343.5 | 158.9 |
| 60 | fullgc | 99.7 | 191.2 | 51.5 | 161.7 | 338.3 | 167.6 |
| 90 | fullgc | 107.3 | 194.9 | 55.6 | 174.1 | 349.4 | 167.5 |
| 120 | fullgc | 112.5 | 200.5 | 56.3 | 171.8 | 346.9 | 174.0 |
| 150 | compact | 117.7 | 205.8 | 59.4 | 176.5 | 344.9 | 188.1 |
| 180 | fullgc | 123.3 | 206.4 | 63.5 | 181.9 | 348.6 | 204.4 |
| 210 | fullgc | 127.2 | 218.0 | 64.1 | 181.7 | 346.5 | 226.4 |
| 240 | fullgc | 130.0 | 216.9 | 68.6 | 185.6 | 350.3 | 222.6 |
| 270 | fullgc | 132.8 | 216.8 | 71.6 | 187.4 | 352.7 | 224.2 |
| 300 | fullgc | 143.1 | 229.3 | 67.2 | 181.7 | 338.9 | 238.1 |
| 300 | final-fullgc | 93.9 | 162.2 | 134.6 | 129.3 | 224.6 | 352.4 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-idle-challoc.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-idle-challoc-mstats.txt`; summary line: `jobs=68366 live_objects=34196 gc_count=13665 major=27 minor=13638 heap_pages=162 rss_final_kb=96196 hwm_kb=146364 mallctl=yes alloc_allocated=108366192 alloc_active=141660160 alloc_resident=170168320 alloc_retained=141037568 alloc_mapped=311205888 compacted=true threads=16 idle_threads=48 idle_bursts=2068 nproc=16 cpu_s=301.76 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-idle-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-v3-soak-idle-jemalloc-mstats.txt`; summary line: `jobs=68613 live_objects=34318 gc_count=13864 major=31 minor=13833 heap_pages=162 rss_final_kb=132440 hwm_kb=194212 mallctl=yes alloc_allocated=109791312 alloc_active=193724416 alloc_resident=235470848 alloc_retained=369491968 alloc_mapped=360841216 compacted=true threads=16 idle_threads=48 idle_bursts=2070 nproc=16 cpu_s=301.91 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
