# Ruby soak, 2026-09-20, aarch64

- commit: 0c60b1a
- date: 2026-09-20T06:42:15Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 48 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- CHALLOC_CONF=percpu_arena:percpu (read by challoc alone; jemalloc runs unchanged)
- load average at start: 1.02 1.01 1.16; at end: 1.00 1.00 1.06; 609 s total
- cgroup memory.peak reset per phase by soak.rb (kernel 7.0.0-1011-gcp): the two cgroup rows below are the cgroup's peak for start..compact and compact..end; the cgroup counts the whole container
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 27db1d715962c256), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 112.0 | 191.2 | -41.4% |
| VmHWM at the end | 112.7 | 191.8 | -41.2% |
| cgroup memory.peak, start..compact | 93.4 | 186.9 | -50.0% |
| cgroup memory.peak, compact..end | 120.4 | 197.7 | -39.1% |
| at minute 1 (t=60) | 72.7 | 166.1 | -56.2% |
| at minute 5, before GC.compact (t=145) | 85.2 | 177.8 | -52.1% |
| at minute 5, after GC.compact (t=150) | 86.1 | 178.9 | -51.9% |
| at minute 10 (t=300) | 111.3 | 179.7 | -38.1% |
| after the last full GC (final) | 111.3 | 127.0 | -12.4% |
| lowest right after a periodic full GC | 67.8 | 161.4 | -58.0% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 110.2 | 148.4 | 187.1 | 24.0 | 211.1 | 159.8 | 187.1 | 1.68 |
| jemalloc | 104.4 | 197.6 | 221.8 | 355.6 | 340.9 | 346.3 | 339.3 | 1.75 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 69044 | 34536 | 13681 | 31 | 13650 | 162 |
| jemalloc | 68995 | 34508 | 13824 | 29 | 13795 | 162 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 52.9 | 132.4 | 0.0 | 105.2 | 329.1 | 106.3 |
| 30 | fullgc | 67.8 | 142.3 | 14.1 | 161.4 | 344.5 | 154.9 |
| 60 | fullgc | 72.7 | 146.9 | 15.9 | 166.1 | 342.8 | 167.2 |
| 90 | fullgc | 78.9 | 149.7 | 20.1 | 173.4 | 350.9 | 162.5 |
| 120 | fullgc | 80.7 | 152.1 | 23.4 | 173.9 | 344.7 | 172.8 |
| 150 | compact | 86.1 | 159.8 | 22.8 | 178.9 | 346.3 | 175.1 |
| 180 | fullgc | 94.1 | 167.6 | 20.1 | 185.1 | 349.9 | 203.6 |
| 210 | fullgc | 98.0 | 176.6 | 21.3 | 184.6 | 350.6 | 214.8 |
| 240 | fullgc | 101.4 | 181.5 | 20.9 | 187.9 | 354.6 | 222.9 |
| 270 | fullgc | 105.9 | 183.6 | 23.1 | 188.8 | 348.9 | 228.6 |
| 300 | fullgc | 111.3 | 187.1 | 24.0 | 179.7 | 339.3 | 238.1 |
| 300 | final-fullgc | 111.3 | 187.1 | 24.0 | 0.0 | - | - |

## Samples

- challoc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-idle-challoc.csv` (62 rows), allocator report `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-idle-challoc-mstats.txt`; summary line: `jobs=69044 live_objects=34536 gc_count=13681 major=31 minor=13650 heap_pages=162 rss_final_kb=114008 hwm_kb=115372 mallctl=yes alloc_allocated=115653712 alloc_active=155766784 alloc_resident=196308992 alloc_retained=25067520 alloc_mapped=221376512 compacted=true threads=16 idle_threads=48 idle_bursts=2072 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=95604 cgroup_peak_after_compact_kb=123248`
- jemalloc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-idle-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-idle-jemalloc-mstats.txt`; summary line: `jobs=68995 live_objects=34508 gc_count=13824 major=29 minor=13795 heap_pages=162 rss_final_kb=130016 hwm_kb=196440 mallctl=yes alloc_allocated=109506144 alloc_active=207159296 alloc_resident=232587264 alloc_retained=372899840 alloc_mapped=357433344 compacted=true threads=16 idle_threads=48 idle_bursts=2070 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=191388 cgroup_peak_after_compact_kb=202444`
