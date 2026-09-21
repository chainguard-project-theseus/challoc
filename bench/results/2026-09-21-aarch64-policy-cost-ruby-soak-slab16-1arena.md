# Ruby soak, 2026-09-21, aarch64

- commit: 3b0ac63
- date: 2026-09-21T08:06:19Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 0.93 0.88 0.58; at end: 1.00 1.00 0.81; 608 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc-1arena: as challoc, with MALLOC_CONF=narenas:1 (one arena for all threads)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc-1arena |
|---|---:|
| peak sampled VmRSS | 175.7 |
| VmHWM at the end | 175.7 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 42.4 |
| at minute 5, before GC.compact (t=295) | 84.4 |
| at minute 5, after GC.compact (t=300) | 85.3 |
| at minute 10 (t=600) | 175.7 |
| after the last full GC (final) | 175.7 |
| lowest right after a periodic full GC | 38.0 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc-1arena | 88.4 | 109.8 | 176.3 | 18.7 | 195.0 | 98.4 | 176.3 | 1.00 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc-1arena | 138094 | 69060 | 16142 | 152 | 15990 | 326 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc-1arena VmRSS | challoc-1arena resident | challoc-1arena retained |
|---:|---|---:|---:|---:|
| 0 | start | 29.1 | 55.4 | 0.0 |
| 30 | fullgc | 38.0 | 61.3 | 6.0 |
| 60 | fullgc | 42.4 | 64.5 | 6.7 |
| 90 | fullgc | 50.1 | 67.9 | 9.5 |
| 120 | fullgc | 52.3 | 69.5 | 11.3 |
| 150 | fullgc | 55.2 | 72.1 | 12.6 |
| 180 | fullgc | 66.2 | 81.5 | 9.9 |
| 210 | fullgc | 68.9 | 85.1 | 10.9 |
| 240 | fullgc | 71.4 | 88.1 | 10.9 |
| 270 | fullgc | 73.7 | 87.9 | 14.0 |
| 300 | compact | 85.3 | 98.4 | 10.7 |
| 330 | fullgc | 111.6 | 122.0 | 14.9 |
| 360 | fullgc | 147.4 | 156.2 | 12.8 |
| 390 | fullgc | 118.5 | 125.8 | 43.7 |
| 420 | fullgc | 129.8 | 135.6 | 39.0 |
| 450 | fullgc | 146.7 | 153.0 | 24.9 |
| 480 | fullgc | 155.3 | 158.7 | 22.9 |
| 510 | fullgc | 162.8 | 165.6 | 19.0 |
| 540 | fullgc | 145.6 | 147.3 | 41.0 |
| 570 | fullgc | 165.3 | 166.1 | 25.6 |
| 600 | fullgc | 175.6 | 176.4 | 18.6 |
| 600 | final-fullgc | 175.7 | 176.3 | 18.7 |

## Samples

- challoc-1arena: `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-slab16-1arena-challoc-1arena.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-slab16-1arena-challoc-1arena-mstats.txt`; summary line: `jobs=138094 live_objects=69060 gc_count=16142 major=152 minor=15990 heap_pages=326 rss_final_kb=179920 hwm_kb=179920 mallctl=yes alloc_allocated=92818784 alloc_active=115261440 alloc_resident=184950784 alloc_retained=19488768 alloc_mapped=204439552 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
