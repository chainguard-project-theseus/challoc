# Ruby soak, 2026-09-21, aarch64

- commit: df6d285
- date: 2026-09-21T05:03:38Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- TRIM=1: soak.rb calls malloc_trim(0) right before every sample (where the allocator exports it), so VmRSS and resident read Active plus metadata at the sample instant
- load average at start: 1.00 1.00 1.00; at end: 1.01 1.01 1.00; 309 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - n1: as challoc, with MALLOC_CONF=narenas:1

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | n1 |
|---|---:|
| peak sampled VmRSS | 80.3 |
| VmHWM at the end | 85.9 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 38.2 |
| at minute 5, before GC.compact (t=145) | 51.4 |
| at minute 5, after GC.compact (t=150) | 52.8 |
| at minute 10 (t=300) | 65.8 |
| after the last full GC (final) | 65.8 |
| lowest right after a periodic full GC | 35.4 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| n1 | 54.6 | 72.9 | 75.9 | 34.1 | 110.0 | 68.2 | 75.9 | 1.15 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| n1 | 69300 | 34664 | 13929 | 33 | 13896 | 168 | 301.26 | 4.35 | 62 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | n1 VmRSS | n1 resident | n1 retained |
|---:|---|---:|---:|---:|
| 0 | start | 29.6 | 50.4 | 4.4 |
| 30 | fullgc | 35.4 | 57.1 | 9.5 |
| 60 | fullgc | 38.2 | 57.2 | 12.8 |
| 90 | fullgc | 46.0 | 64.2 | 11.3 |
| 120 | fullgc | 48.8 | 65.8 | 13.2 |
| 150 | compact | 52.8 | 68.2 | 14.7 |
| 180 | fullgc | 60.9 | 76.2 | 13.6 |
| 210 | fullgc | 63.5 | 79.4 | 14.8 |
| 240 | fullgc | 67.1 | 81.4 | 15.8 |
| 270 | fullgc | 69.6 | 83.5 | 16.9 |
| 300 | fullgc | 79.7 | 96.4 | 13.6 |
| 300 | final-fullgc | 65.8 | 75.9 | 34.1 |

## Samples

- n1: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-trim-n1.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-trim-n1-mstats.txt`; summary line: `jobs=69300 live_objects=34664 gc_count=13929 major=33 minor=13896 heap_pages=168 rss_final_kb=67376 hwm_kb=87984 mallctl=yes alloc_allocated=57254160 alloc_active=76464128 alloc_resident=79650816 alloc_retained=35704832 alloc_mapped=115355648 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.26 trim=yes trimmed=62 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
