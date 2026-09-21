# Ruby soak, 2026-09-21, x86_64

- commit: df6d285
- date: 2026-09-21T05:03:39Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- TRIM=1: soak.rb calls malloc_trim(0) right before every sample (where the allocator exports it), so VmRSS and resident read Active plus metadata at the sample instant
- load average at start: 0.98 0.99 1.00; at end: 1.00 1.00 1.00; 309 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - n1: as challoc, with MALLOC_CONF=narenas:1

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | n1 |
|---|---:|
| peak sampled VmRSS | 105.6 |
| VmHWM at the end | 112.2 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 52.1 |
| at minute 5, before GC.compact (t=145) | 70.3 |
| at minute 5, after GC.compact (t=150) | 67.7 |
| at minute 10 (t=300) | 75.5 |
| after the last full GC (final) | 75.5 |
| lowest right after a periodic full GC | 49.4 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| n1 | 57.9 | 79.9 | 83.1 | 54.7 | 137.7 | 84.1 | 83.1 | 1.10 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| n1 | 74916 | 37472 | 13642 | 1237 | 12405 | 204 | 297.18 | 3.97 | 62 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | n1 VmRSS | n1 resident | n1 retained |
|---:|---|---:|---:|---:|
| 0 | start | 30.8 | 54.1 | 4.9 |
| 30 | fullgc | 49.4 | 70.1 | 18.0 |
| 60 | fullgc | 52.1 | 69.7 | 20.9 |
| 90 | fullgc | 51.6 | 71.8 | 21.7 |
| 120 | fullgc | 61.9 | 78.4 | 28.7 |
| 150 | compact | 67.7 | 84.1 | 26.1 |
| 180 | fullgc | 68.6 | 81.9 | 29.8 |
| 210 | fullgc | 71.5 | 88.4 | 26.4 |
| 240 | fullgc | 80.6 | 94.1 | 25.6 |
| 270 | fullgc | 80.7 | 95.7 | 25.7 |
| 300 | fullgc | 92.6 | 114.2 | 23.6 |
| 300 | final-fullgc | 75.5 | 83.1 | 54.7 |

## Samples

- n1: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-trim-n1.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-trim-n1-mstats.txt`; summary line: `jobs=74916 live_objects=37472 gc_count=13642 major=1237 minor=12405 heap_pages=204 rss_final_kb=77360 hwm_kb=114924 mallctl=yes alloc_allocated=60733584 alloc_active=83898368 alloc_resident=87162880 alloc_retained=57266176 alloc_mapped=144429056 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=297.18 trim=yes trimmed=62 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
