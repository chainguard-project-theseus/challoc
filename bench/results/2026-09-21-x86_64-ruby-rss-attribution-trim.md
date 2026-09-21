# Ruby soak, 2026-09-21, x86_64

- commit: 2078f67
- date: 2026-09-21T03:58:03Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- TRIM=1: soak.rb calls malloc_trim(0) right before every sample (where the allocator exports it), so VmRSS and resident read Active plus metadata at the sample instant
- load average at start: 0.99 1.00 1.00; at end: 0.98 0.99 1.00; 309 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 2ccf9f46d160cd96), default MALLOC_CONF

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc |
|---|---:|
| peak sampled VmRSS | 333.2 |
| VmHWM at the end | 352.0 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 289.2 |
| at minute 5, before GC.compact (t=145) | 303.3 |
| at minute 5, after GC.compact (t=150) | 303.3 |
| at minute 10 (t=300) | 132.4 |
| after the last full GC (final) | 132.4 |
| lowest right after a periodic full GC | 279.3 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 56.9 | 138.3 | 146.5 | 267.3 | 413.8 | 324.9 | 146.5 | 1.11 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 72834 | 36430 | 13722 | 1279 | 12443 | 204 | 297.47 | 4.08 | 62 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained |
|---:|---|---:|---:|---:|
| 0 | start | 45.6 | 83.3 | 6.2 |
| 30 | fullgc | 279.3 | 302.6 | 30.1 |
| 60 | fullgc | 289.2 | 310.5 | 36.2 |
| 90 | fullgc | 292.4 | 314.2 | 37.9 |
| 120 | fullgc | 292.5 | 313.2 | 41.7 |
| 150 | compact | 303.3 | 324.9 | 47.9 |
| 180 | fullgc | 305.7 | 324.4 | 51.6 |
| 210 | fullgc | 308.9 | 335.1 | 51.2 |
| 240 | fullgc | 311.3 | 334.2 | 54.9 |
| 270 | fullgc | 311.3 | 333.3 | 59.9 |
| 300 | fullgc | 328.5 | 355.3 | 58.5 |
| 300 | final-fullgc | 132.4 | 146.5 | 267.3 |

## Samples

- challoc: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-trim-challoc.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-trim-challoc-mstats.txt`; summary line: `jobs=72834 live_objects=36430 gc_count=13722 major=1279 minor=12443 heap_pages=204 rss_final_kb=135628 hwm_kb=360432 mallctl=yes alloc_allocated=59680848 alloc_active=145043456 alloc_resident=153731072 alloc_retained=280215552 alloc_mapped=433946624 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=297.47 trim=yes trimmed=62 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
