# Ruby soak, 2026-09-19, aarch64

- commit: 79e3931
- date: 2026-09-19T23:31:26Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 120 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 60 s, one full GC after the workers stop (the final row)
- load average at start: 0.73 0.96 0.88; at end: 1.00 0.99 0.92; 265 s total
- cgroup memory.peak reset per phase by soak.rb (kernel 7.0.0-1011-gcp): the two cgroup rows below are the cgroup's peak for start..compact and compact..end; the cgroup counts the whole container
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 96e2c8dde777c044), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 296.2 | 108.8 | +172.2% |
| VmHWM at the end | 296.2 | 109.9 | +169.5% |
| cgroup memory.peak, start..compact | 355.0 | 166.7 | +113.0% |
| cgroup memory.peak, compact..end | 366.5 | 177.2 | +106.8% |
| at minute 1 (t=60) | 284.1 | 98.6 | +188.1% |
| at minute 5, before GC.compact (t=55) | 277.4 | 96.5 | +187.5% |
| at minute 5, after GC.compact (t=60) | 284.1 | 98.6 | +188.1% |
| at minute 10 (t=120) | 296.2 | 86.0 | +244.4% |
| after the last full GC (final) | 296.2 | 86.0 | +244.4% |
| lowest right after a periodic full GC | 277.8 | 94.2 | +194.9% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=60 | resident at t=120 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 40.4 | 116.5 | 320.2 | 20.0 | 340.2 | 310.7 | 320.2 | 1.08 |
| jemalloc | 36.8 | 84.8 | 111.8 | 115.6 | 143.4 | 133.3 | 111.8 | 1.30 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 27474 | 13746 | 6700 | 17 | 6683 | 94 |
| jemalloc | 27781 | 13900 | 6708 | 12 | 6696 | 95 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=60 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 40.8 | 82.8 | 0.0 | 41.7 | 108.7 | 30.7 |
| 30 | fullgc | 279.3 | 307.4 | 11.6 | 94.2 | 133.1 | 86.8 |
| 60 | compact | 284.1 | 310.7 | 14.7 | 98.6 | 133.3 | 90.1 |
| 90 | fullgc | 292.1 | 315.4 | 18.1 | 104.4 | 140.9 | 82.4 |
| 120 | fullgc | 296.2 | 320.2 | 20.0 | 86.0 | 111.8 | 115.6 |
| 120 | final-fullgc | 296.2 | 320.2 | 20.0 | 86.0 | 111.8 | 115.6 |

## Samples

- challoc: `bench/results/2026-09-19-aarch64-lane-d-task17-ruby-soak-challoc.csv` (26 rows), allocator report `bench/results/2026-09-19-aarch64-lane-d-task17-ruby-soak-challoc-mstats.txt`; summary line: `jobs=27474 live_objects=13746 gc_count=6700 major=17 minor=6683 heap_pages=94 rss_final_kb=303272 hwm_kb=303272 mallctl=yes alloc_allocated=42339584 alloc_active=122200064 alloc_resident=335740928 alloc_retained=20967424 alloc_mapped=356708352 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=363532 cgroup_peak_after_compact_kb=375284`
- jemalloc: `bench/results/2026-09-19-aarch64-lane-d-task17-ruby-soak-jemalloc.csv` (26 rows), allocator report `bench/results/2026-09-19-aarch64-lane-d-task17-ruby-soak-jemalloc-mstats.txt`; summary line: `jobs=27781 live_objects=13900 gc_count=6708 major=12 minor=6696 heap_pages=95 rss_final_kb=88112 hwm_kb=112580 mallctl=yes alloc_allocated=38637536 alloc_active=88866816 alloc_resident=117243904 alloc_retained=121176064 alloc_mapped=150405120 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=170732 cgroup_peak_after_compact_kb=181408`
