# Ruby soak, 2026-09-20, x86_64

- commit: 0c60b1a
- date: 2026-09-20T06:54:31Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 48 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- CHALLOC_CONF=percpu_arena:percpu (read by challoc alone; jemalloc runs unchanged)
- load average at start: 0.92 0.98 1.20; at end: 1.04 1.01 1.09; 609 s total
- cgroup memory.peak reset per phase by soak.rb (kernel 7.0.0-1011-gcp): the two cgroup rows below are the cgroup's peak for start..compact and compact..end; the cgroup counts the whole container
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 2798d6cdfc6b9264), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 158.5 | 165.7 | -4.3% |
| VmHWM at the end | 159.0 | 165.8 | -4.1% |
| cgroup memory.peak, start..compact | 123.0 | 154.7 | -20.5% |
| cgroup memory.peak, compact..end | 166.0 | 169.0 | -1.8% |
| at minute 1 (t=60) | 90.7 | 138.5 | -34.5% |
| at minute 5, before GC.compact (t=145) | 116.2 | 149.3 | -22.2% |
| at minute 5, after GC.compact (t=150) | 114.8 | 148.7 | -22.8% |
| at minute 10 (t=300) | 156.7 | 165.7 | -5.4% |
| after the last full GC (final) | 156.7 | 119.5 | +31.1% |
| lowest right after a periodic full GC | 77.1 | 124.4 | -38.0% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 115.1 | 162.2 | 233.2 | 39.5 | 272.7 | 186.2 | 233.2 | 1.49 |
| jemalloc | 106.6 | 119.0 | 167.1 | 157.8 | 290.7 | 202.9 | 214.8 | 1.40 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 72547 | 36284 | 13519 | 1268 | 12251 | 205 |
| jemalloc | 71108 | 35568 | 13495 | 1282 | 12213 | 204 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 47.2 | 124.1 | 0.0 | 101.6 | 181.6 | 57.8 |
| 30 | fullgc | 77.1 | 150.4 | 27.6 | 124.4 | 182.7 | 89.4 |
| 60 | fullgc | 90.7 | 164.4 | 33.8 | 138.5 | 195.1 | 80.2 |
| 90 | fullgc | 96.2 | 168.7 | 37.2 | 137.2 | 193.2 | 82.1 |
| 120 | fullgc | 93.3 | 166.6 | 43.2 | 133.8 | 189.4 | 86.0 |
| 150 | compact | 114.8 | 186.2 | 38.4 | 148.7 | 202.9 | 118.2 |
| 180 | fullgc | 117.2 | 190.1 | 44.1 | 146.8 | 200.9 | 120.3 |
| 210 | fullgc | 121.6 | 197.3 | 45.7 | 138.5 | 191.4 | 129.7 |
| 240 | fullgc | 115.3 | 189.5 | 55.2 | 133.6 | 186.4 | 134.8 |
| 270 | fullgc | 125.7 | 200.4 | 50.7 | 138.0 | 189.7 | 131.5 |
| 300 | fullgc | 156.7 | 233.2 | 39.5 | 165.7 | 214.8 | 110.1 |
| 300 | final-fullgc | 156.7 | 233.2 | 39.5 | 0.0 | - | - |

## Samples

- challoc: `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-idle-challoc.csv` (62 rows), allocator report `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-idle-challoc-mstats.txt`; summary line: `jobs=72547 live_objects=36284 gc_count=13519 major=1268 minor=12251 heap_pages=205 rss_final_kb=160496 hwm_kb=162816 mallctl=yes alloc_allocated=120735440 alloc_active=170192896 alloc_resident=244645888 alloc_retained=41377792 alloc_mapped=286023680 compacted=true threads=16 idle_threads=48 idle_bursts=2096 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=125936 cgroup_peak_after_compact_kb=169944`
- jemalloc: `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-idle-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-idle-jemalloc-mstats.txt`; summary line: `jobs=71108 live_objects=35568 gc_count=13495 major=1282 minor=12213 heap_pages=204 rss_final_kb=122396 hwm_kb=169792 mallctl=yes alloc_allocated=111758208 alloc_active=124821504 alloc_resident=175259648 alloc_retained=165445632 alloc_mapped=304840704 compacted=true threads=16 idle_threads=48 idle_bursts=2084 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=158408 cgroup_peak_after_compact_kb=173040`
