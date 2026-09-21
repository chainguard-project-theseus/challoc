# Ruby soak, 2026-09-21, aarch64

- commit: 3b0ac63
- date: 2026-09-21T07:56:00Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 0.00 0.09 0.16; at end: 1.00 0.89 0.58; 609 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc-1arena: as challoc, with MALLOC_CONF=narenas:1 (one arena for all threads)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc-1arena |
|---|---:|
| peak sampled VmRSS | 172.0 |
| VmHWM at the end | 172.0 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 41.5 |
| at minute 5, before GC.compact (t=295) | 83.2 |
| at minute 5, after GC.compact (t=300) | 87.8 |
| at minute 10 (t=600) | 172.0 |
| after the last full GC (final) | 172.0 |
| lowest right after a periodic full GC | 38.3 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc-1arena | 88.9 | 112.6 | 171.6 | 18.4 | 190.1 | 100.3 | 171.6 | 1.00 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc-1arena | 138905 | 69468 | 16624 | 159 | 16465 | 331 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc-1arena VmRSS | challoc-1arena resident | challoc-1arena retained |
|---:|---|---:|---:|---:|
| 0 | start | 28.6 | 55.0 | 0.0 |
| 30 | fullgc | 38.3 | 61.6 | 5.4 |
| 60 | fullgc | 41.5 | 63.9 | 6.6 |
| 90 | fullgc | 49.5 | 67.7 | 8.9 |
| 120 | fullgc | 51.2 | 68.7 | 11.1 |
| 150 | fullgc | 54.6 | 72.0 | 12.1 |
| 180 | fullgc | 65.0 | 80.3 | 9.9 |
| 210 | fullgc | 68.0 | 84.0 | 10.9 |
| 240 | fullgc | 70.4 | 87.1 | 10.8 |
| 270 | fullgc | 72.8 | 87.1 | 13.9 |
| 300 | compact | 87.8 | 100.3 | 10.9 |
| 330 | fullgc | 106.1 | 116.9 | 17.3 |
| 360 | fullgc | 140.9 | 148.7 | 11.5 |
| 390 | fullgc | 118.9 | 127.8 | 37.8 |
| 420 | fullgc | 139.6 | 145.9 | 24.2 |
| 450 | fullgc | 151.8 | 157.3 | 16.0 |
| 480 | fullgc | 156.5 | 161.6 | 15.2 |
| 510 | fullgc | 159.8 | 163.0 | 17.1 |
| 540 | fullgc | 147.2 | 148.6 | 34.6 |
| 570 | fullgc | 165.8 | 165.9 | 20.7 |
| 600 | fullgc | 171.8 | 171.6 | 18.4 |
| 600 | final-fullgc | 172.0 | 171.6 | 18.4 |

## Samples

- challoc-1arena: `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-1arena-challoc-1arena.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-1arena-challoc-1arena-mstats.txt`; summary line: `jobs=138905 live_objects=69468 gc_count=16624 major=159 minor=16465 heap_pages=331 rss_final_kb=176088 hwm_kb=176088 mallctl=yes alloc_allocated=93236976 alloc_active=118112256 alloc_resident=180002816 alloc_retained=19279872 alloc_mapped=199282688 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
