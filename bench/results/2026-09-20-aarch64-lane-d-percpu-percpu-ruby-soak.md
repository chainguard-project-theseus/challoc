# Ruby soak, 2026-09-20, aarch64

- commit: 0c60b1a
- date: 2026-09-20T06:22:06Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- CHALLOC_CONF=percpu_arena:percpu (read by challoc alone; jemalloc runs unchanged)
- load average at start: 1.01 1.42 1.68; at end: 1.02 1.01 1.16; 1208 s total
- cgroup memory.peak reset per phase by soak.rb (kernel 7.0.0-1011-gcp): the two cgroup rows below are the cgroup's peak for start..compact and compact..end; the cgroup counts the whole container
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 27db1d715962c256), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 187.8 | 199.0 | -5.6% |
| VmHWM at the end | 191.7 | 202.9 | -5.5% |
| cgroup memory.peak, start..compact | 119.9 | 137.2 | -12.6% |
| cgroup memory.peak, compact..end | 200.3 | 208.7 | -4.0% |
| at minute 1 (t=60) | 70.7 | 98.6 | -28.3% |
| at minute 5, before GC.compact (t=295) | 105.4 | 130.0 | -18.9% |
| at minute 5, after GC.compact (t=300) | 110.2 | 130.6 | -15.6% |
| at minute 10 (t=600) | 186.8 | 169.0 | +10.5% |
| after the last full GC (final) | 186.8 | 169.0 | +10.5% |
| lowest right after a periodic full GC | 66.6 | 91.8 | -27.5% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 96.8 | 150.9 | 211.6 | 49.8 | 261.4 | 140.0 | 211.6 | 1.13 |
| jemalloc | 90.7 | 139.6 | 176.7 | 121.7 | 207.3 | 160.4 | 176.7 | 1.05 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 138656 | 69342 | 16528 | 151 | 16377 | 330 |
| jemalloc | 138970 | 69496 | 16478 | 145 | 16333 | 326 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 37.2 | 70.1 | 0.0 | 41.0 | 108.6 | 30.8 |
| 30 | fullgc | 66.6 | 93.8 | 11.8 | 91.8 | 133.4 | 82.9 |
| 60 | fullgc | 70.7 | 97.2 | 14.1 | 98.6 | 136.4 | 87.0 |
| 90 | fullgc | 77.3 | 101.2 | 16.1 | 104.4 | 141.6 | 81.8 |
| 120 | fullgc | 78.9 | 102.9 | 19.1 | 102.8 | 139.2 | 96.2 |
| 150 | fullgc | 83.0 | 109.4 | 20.7 | 113.6 | 151.9 | 87.4 |
| 180 | fullgc | 90.8 | 116.4 | 18.9 | 123.1 | 158.9 | 96.4 |
| 210 | fullgc | 94.4 | 126.1 | 20.3 | 123.8 | 158.4 | 101.0 |
| 240 | fullgc | 99.8 | 132.7 | 20.3 | 126.4 | 162.2 | 121.1 |
| 270 | fullgc | 102.0 | 132.8 | 23.3 | 129.4 | 161.6 | 125.8 |
| 300 | compact | 110.2 | 140.0 | 19.8 | 130.6 | 160.4 | 126.9 |
| 330 | fullgc | 136.0 | 167.9 | 29.2 | 147.0 | 172.2 | 115.1 |
| 360 | fullgc | 153.6 | 183.6 | 26.5 | 184.2 | 213.7 | 73.8 |
| 390 | fullgc | 157.4 | 190.2 | 35.8 | 163.0 | 187.3 | 110.8 |
| 420 | fullgc | 163.6 | 192.6 | 39.5 | 174.5 | 197.6 | 100.5 |
| 450 | fullgc | 167.4 | 196.1 | 42.7 | 183.7 | 207.0 | 91.1 |
| 480 | fullgc | 169.3 | 196.4 | 48.5 | 181.9 | 201.4 | 96.8 |
| 510 | fullgc | 174.8 | 201.4 | 47.0 | 190.1 | 207.1 | 91.1 |
| 540 | fullgc | 178.2 | 203.3 | 48.6 | 187.9 | 203.6 | 94.7 |
| 570 | fullgc | 182.4 | 206.2 | 50.7 | 191.7 | 202.2 | 96.1 |
| 600 | fullgc | 186.8 | 211.6 | 49.8 | 169.0 | 176.7 | 121.7 |
| 600 | final-fullgc | 186.8 | 211.6 | 49.8 | 169.0 | 176.7 | 121.7 |

## Samples

- challoc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-soak-challoc.csv` (122 rows), allocator report `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-soak-challoc-mstats.txt`; summary line: `jobs=138656 live_objects=69342 gc_count=16528 major=151 minor=16377 heap_pages=330 rss_final_kb=191296 hwm_kb=196328 mallctl=yes alloc_allocated=101579280 alloc_active=158343168 alloc_resident=221966336 alloc_retained=52105216 alloc_mapped=274071552 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=122744 cgroup_peak_after_compact_kb=205124`
- jemalloc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-soak-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-soak-jemalloc-mstats.txt`; summary line: `jobs=138970 live_objects=69496 gc_count=16478 major=145 minor=16333 heap_pages=326 rss_final_kb=173016 hwm_kb=207800 mallctl=yes alloc_allocated=95118784 alloc_active=146407424 alloc_resident=185270272 alloc_retained=127598592 alloc_mapped=217382912 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=140448 cgroup_peak_after_compact_kb=213736`
