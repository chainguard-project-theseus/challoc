# Ruby soak, 2026-09-20, x86_64

- commit: 0c60b1a
- date: 2026-09-20T06:34:19Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- CHALLOC_CONF=percpu_arena:percpu (read by challoc alone; jemalloc runs unchanged)
- load average at start: 4.71 2.41 1.84; at end: 1.00 1.00 1.20; 1209 s total
- cgroup memory.peak reset per phase by soak.rb (kernel 7.0.0-1011-gcp): the two cgroup rows below are the cgroup's peak for start..compact and compact..end; the cgroup counts the whole container
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 2798d6cdfc6b9264), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 209.9 | 179.1 | +17.2% |
| VmHWM at the end | 213.1 | 183.7 | +16.0% |
| cgroup memory.peak, start..compact | 155.0 | 118.3 | +31.0% |
| cgroup memory.peak, compact..end | 218.7 | 185.1 | +18.2% |
| at minute 1 (t=60) | 93.1 | 73.2 | +27.2% |
| at minute 5, before GC.compact (t=295) | 148.2 | 111.4 | +33.0% |
| at minute 5, after GC.compact (t=300) | 146.9 | 116.5 | +26.1% |
| at minute 10 (t=600) | 207.7 | 140.2 | +48.1% |
| after the last full GC (final) | 207.7 | 140.2 | +48.1% |
| lowest right after a periodic full GC | 91.8 | 71.2 | +28.9% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 100.3 | 152.1 | 223.8 | 90.5 | 314.3 | 173.0 | 223.8 | 1.08 |
| jemalloc | 91.9 | 104.5 | 129.8 | 87.7 | 159.3 | 115.8 | 129.8 | 0.93 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 147838 | 73930 | 16222 | 1362 | 14860 | 342 |
| jemalloc | 145121 | 72570 | 15986 | 1317 | 14669 | 342 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 31.2 | 61.8 | 0.0 | 39.0 | 56.5 | 17.3 |
| 30 | fullgc | 99.3 | 124.1 | 14.4 | 71.2 | 78.7 | 33.0 |
| 60 | fullgc | 93.1 | 117.9 | 36.5 | 73.2 | 79.9 | 34.9 |
| 90 | fullgc | 91.8 | 114.0 | 45.8 | 73.2 | 79.0 | 35.8 |
| 120 | fullgc | 107.7 | 131.5 | 34.4 | 89.5 | 93.8 | 57.2 |
| 150 | fullgc | 113.7 | 136.5 | 42.7 | 91.9 | 95.9 | 64.2 |
| 180 | fullgc | 116.4 | 140.3 | 41.8 | 91.1 | 94.2 | 65.9 |
| 210 | fullgc | 111.5 | 138.2 | 50.5 | 91.6 | 94.2 | 66.0 |
| 240 | fullgc | 120.5 | 145.7 | 46.5 | 97.4 | 99.0 | 61.2 |
| 270 | fullgc | 116.8 | 141.8 | 53.5 | 95.6 | 96.2 | 64.0 |
| 300 | compact | 146.9 | 173.0 | 41.2 | 116.5 | 115.8 | 44.5 |
| 330 | fullgc | 194.0 | 217.2 | 37.9 | 145.5 | 142.0 | 75.2 |
| 360 | fullgc | 169.8 | 190.5 | 81.3 | 130.8 | 126.8 | 90.6 |
| 390 | fullgc | 184.3 | 205.0 | 73.7 | 145.5 | 141.3 | 76.1 |
| 420 | fullgc | 182.3 | 202.0 | 85.3 | 154.4 | 149.3 | 68.1 |
| 450 | fullgc | 185.5 | 204.1 | 87.5 | 154.3 | 148.3 | 69.1 |
| 480 | fullgc | 190.2 | 207.2 | 92.5 | 165.1 | 157.9 | 59.6 |
| 510 | fullgc | 195.4 | 211.7 | 90.8 | 168.1 | 160.6 | 56.9 |
| 540 | fullgc | 197.4 | 213.5 | 94.7 | 168.0 | 159.4 | 58.0 |
| 570 | fullgc | 202.9 | 218.2 | 92.5 | 170.9 | 161.8 | 55.7 |
| 600 | fullgc | 207.7 | 223.8 | 90.5 | 140.2 | 129.8 | 87.7 |
| 600 | final-fullgc | 207.7 | 223.8 | 90.5 | 140.2 | 129.8 | 87.7 |

## Samples

- challoc: `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-soak-challoc.csv` (122 rows), allocator report `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-soak-challoc-mstats.txt`; summary line: `jobs=147838 live_objects=73930 gc_count=16222 major=1362 minor=14860 heap_pages=342 rss_final_kb=212668 hwm_kb=218216 mallctl=yes alloc_allocated=105330368 alloc_active=159567872 alloc_resident=234766336 alloc_retained=94855168 alloc_mapped=329621504 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=158748 cgroup_peak_after_compact_kb=223984`
- jemalloc: `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-soak-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-soak-jemalloc-mstats.txt`; summary line: `jobs=145121 live_objects=72570 gc_count=15986 major=1317 minor=14669 heap_pages=342 rss_final_kb=143548 hwm_kb=188068 mallctl=yes alloc_allocated=96348128 alloc_active=109551616 alloc_resident=136126464 alloc_retained=92000256 alloc_mapped=166998016 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=121116 cgroup_peak_after_compact_kb=189532`
