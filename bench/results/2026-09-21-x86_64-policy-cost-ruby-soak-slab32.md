# Ruby soak, 2026-09-21, x86_64

- commit: f53418d
- date: 2026-09-21T05:36:24Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 0.92 0.98 1.56; at end: 1.00 1.00 1.27; 608 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 64ef0a5c56549879, built from the A/B source copy .tmp-slab32), default MALLOC_CONF

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc |
|---|---:|
| peak sampled VmRSS | 414.4 |
| VmHWM at the end | 416.5 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 312.2 |
| at minute 5, before GC.compact (t=295) | 348.0 |
| at minute 5, after GC.compact (t=300) | 356.9 |
| at minute 10 (t=600) | 407.2 |
| after the last full GC (final) | 407.2 |
| lowest right after a periodic full GC | 304.1 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 105.7 | 196.1 | 409.7 | 71.3 | 481.0 | 383.0 | 409.7 | 1.01 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 151351 | 75688 | 16432 | 1367 | 15065 | 351 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained |
|---:|---|---:|---:|---:|
| 0 | start | 41.3 | 81.4 | 0.0 |
| 30 | fullgc | 304.1 | 331.2 | 8.9 |
| 60 | fullgc | 312.2 | 338.9 | 16.8 |
| 90 | fullgc | 313.8 | 336.2 | 25.6 |
| 120 | fullgc | 328.4 | 350.2 | 19.3 |
| 150 | fullgc | 328.3 | 351.0 | 29.6 |
| 180 | fullgc | 327.4 | 347.5 | 36.6 |
| 210 | fullgc | 329.8 | 356.9 | 37.6 |
| 240 | fullgc | 336.3 | 360.5 | 37.7 |
| 270 | fullgc | 335.7 | 357.3 | 45.4 |
| 300 | compact | 356.9 | 383.0 | 37.9 |
| 330 | fullgc | 375.0 | 391.7 | 46.9 |
| 360 | fullgc | 383.1 | 400.0 | 48.6 |
| 390 | fullgc | 385.9 | 401.3 | 52.3 |
| 420 | fullgc | 388.1 | 401.4 | 57.0 |
| 450 | fullgc | 386.1 | 398.0 | 63.9 |
| 480 | fullgc | 390.5 | 395.0 | 70.0 |
| 510 | fullgc | 403.0 | 405.9 | 64.9 |
| 540 | fullgc | 407.8 | 410.9 | 63.4 |
| 570 | fullgc | 401.9 | 404.1 | 72.7 |
| 600 | fullgc | 406.9 | 409.4 | 71.5 |
| 600 | final-fullgc | 407.2 | 409.7 | 71.3 |

## Samples

- challoc: `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-slab32-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-slab32-challoc-mstats.txt`; summary line: `jobs=151351 live_objects=75688 gc_count=16432 major=1367 minor=15065 heap_pages=351 rss_final_kb=416972 hwm_kb=426508 mallctl=yes alloc_allocated=110912912 alloc_active=205725696 alloc_resident=429617152 alloc_retained=74702848 alloc_mapped=504320000 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
