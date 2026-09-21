# Ruby soak, 2026-09-21, x86_64

- commit: fd00bf7
- date: 2026-09-21T05:59:53Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 0.39 0.65 0.95; at end: 1.05 0.98 1.00; 609 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc-1arena: as challoc, with MALLOC_CONF=narenas:1 (one arena for all threads)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc-1arena |
|---|---:|
| peak sampled VmRSS | 186.2 |
| VmHWM at the end | 186.2 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 59.8 |
| at minute 5, before GC.compact (t=295) | 119.4 |
| at minute 5, after GC.compact (t=300) | 130.9 |
| at minute 10 (t=600) | 172.8 |
| after the last full GC (final) | 172.8 |
| lowest right after a periodic full GC | 58.9 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc-1arena | 96.9 | 120.6 | 170.8 | 32.4 | 203.3 | 139.9 | 170.8 | 0.99 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc-1arena | 154902 | 77466 | 16523 | 1356 | 15167 | 356 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc-1arena VmRSS | challoc-1arena resident | challoc-1arena retained |
|---:|---|---:|---:|---:|
| 0 | start | 28.1 | 58.0 | 0.0 |
| 30 | fullgc | 60.9 | 83.4 | 5.0 |
| 60 | fullgc | 59.8 | 80.3 | 11.1 |
| 90 | fullgc | 58.9 | 77.6 | 16.8 |
| 120 | fullgc | 81.8 | 99.9 | 8.4 |
| 150 | fullgc | 80.5 | 95.8 | 15.5 |
| 180 | fullgc | 79.4 | 94.0 | 19.0 |
| 210 | fullgc | 78.4 | 93.1 | 22.7 |
| 240 | fullgc | 86.3 | 101.0 | 19.5 |
| 270 | fullgc | 95.2 | 108.2 | 18.2 |
| 300 | compact | 130.9 | 139.9 | 17.3 |
| 330 | fullgc | 136.0 | 142.6 | 24.6 |
| 360 | fullgc | 144.9 | 154.6 | 17.5 |
| 390 | fullgc | 149.3 | 156.1 | 19.7 |
| 420 | fullgc | 149.4 | 153.0 | 26.7 |
| 450 | fullgc | 163.2 | 165.1 | 18.8 |
| 480 | fullgc | 165.7 | 167.0 | 20.5 |
| 510 | fullgc | 156.5 | 155.6 | 36.0 |
| 540 | fullgc | 163.5 | 162.9 | 32.7 |
| 570 | fullgc | 182.3 | 181.6 | 17.4 |
| 600 | fullgc | 176.1 | 174.4 | 28.9 |
| 600 | final-fullgc | 172.8 | 170.8 | 32.4 |

## Samples

- challoc-1arena: `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-1arena-challoc-1arena.csv` (122 rows), allocator report `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-1arena-challoc-1arena-mstats.txt`; summary line: `jobs=154902 live_objects=77466 gc_count=16523 major=1356 minor=15167 heap_pages=356 rss_final_kb=176904 hwm_kb=190708 mallctl=yes alloc_allocated=101696480 alloc_active=126590976 alloc_resident=179265536 alloc_retained=33878016 alloc_mapped=213143552 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
