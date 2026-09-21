# Ruby soak, 2026-09-21, x86_64

- commit: fd00bf7
- date: 2026-09-21T06:10:05Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 1.05 0.98 1.00; at end: 1.00 1.00 1.00; 608 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc-1arena: as challoc, with MALLOC_CONF=narenas:1 (one arena for all threads)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc-1arena |
|---|---:|
| peak sampled VmRSS | 191.6 |
| VmHWM at the end | 191.6 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 61.4 |
| at minute 5, before GC.compact (t=295) | 128.8 |
| at minute 5, after GC.compact (t=300) | 139.2 |
| at minute 10 (t=600) | 191.6 |
| after the last full GC (final) | 191.6 |
| lowest right after a periodic full GC | 59.8 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc-1arena | 96.4 | 118.3 | 189.7 | 18.0 | 207.7 | 148.1 | 189.7 | 0.99 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc-1arena | 153775 | 76894 | 16457 | 1348 | 15109 | 355 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc-1arena VmRSS | challoc-1arena resident | challoc-1arena retained |
|---:|---|---:|---:|---:|
| 0 | start | 28.8 | 58.6 | 0.0 |
| 30 | fullgc | 62.2 | 84.6 | 5.0 |
| 60 | fullgc | 61.4 | 83.4 | 8.9 |
| 90 | fullgc | 59.8 | 78.4 | 17.1 |
| 120 | fullgc | 82.6 | 100.8 | 9.6 |
| 150 | fullgc | 81.5 | 96.7 | 16.3 |
| 180 | fullgc | 81.0 | 95.4 | 19.7 |
| 210 | fullgc | 79.5 | 94.2 | 23.7 |
| 240 | fullgc | 87.5 | 102.2 | 20.5 |
| 270 | fullgc | 91.8 | 104.6 | 22.0 |
| 300 | compact | 139.2 | 148.1 | 12.0 |
| 330 | fullgc | 120.8 | 128.4 | 41.0 |
| 360 | fullgc | 144.0 | 154.4 | 20.8 |
| 390 | fullgc | 145.0 | 153.6 | 25.5 |
| 420 | fullgc | 141.4 | 146.7 | 37.1 |
| 450 | fullgc | 166.5 | 169.9 | 18.3 |
| 480 | fullgc | 155.9 | 156.0 | 35.9 |
| 510 | fullgc | 164.5 | 166.3 | 29.6 |
| 540 | fullgc | 179.7 | 179.0 | 20.8 |
| 570 | fullgc | 178.6 | 176.1 | 27.2 |
| 600 | fullgc | 191.4 | 189.8 | 17.9 |
| 600 | final-fullgc | 191.6 | 189.7 | 18.0 |

## Samples

- challoc-1arena: `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-slab16-1arena-challoc-1arena.csv` (122 rows), allocator report `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-slab16-1arena-challoc-1arena-mstats.txt`; summary line: `jobs=153775 live_objects=76894 gc_count=16457 major=1348 minor=15109 heap_pages=355 rss_final_kb=196156 hwm_kb=196156 mallctl=yes alloc_allocated=101099408 alloc_active=124153856 alloc_resident=199016448 alloc_retained=18821120 alloc_mapped=217837568 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
