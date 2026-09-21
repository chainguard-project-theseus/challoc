# Ruby soak, 2026-09-21, x86_64

- commit: f53418d
- date: 2026-09-21T05:46:38Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- CHALLOC_CONF=tcache_compact_shift:0 (read by challoc alone; jemalloc runs unchanged)
- load average at start: 0.92 0.98 1.27; at end: 1.00 1.00 1.12; 608 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 2ccf9f46d160cd96), default MALLOC_CONF plus CHALLOC_CONF=tcache_compact_shift:0

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc |
|---|---:|
| peak sampled VmRSS | 417.4 |
| VmHWM at the end | 419.2 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 310.0 |
| at minute 5, before GC.compact (t=295) | 351.7 |
| at minute 5, after GC.compact (t=300) | 363.1 |
| at minute 10 (t=600) | 415.6 |
| after the last full GC (final) | 415.6 |
| lowest right after a periodic full GC | 301.1 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 106.4 | 212.1 | 418.8 | 65.2 | 484.0 | 391.0 | 418.8 | 1.01 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 151776 | 75902 | 16558 | 1421 | 15137 | 354 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained |
|---:|---|---:|---:|---:|
| 0 | start | 40.9 | 83.6 | 0.0 |
| 30 | fullgc | 301.1 | 329.5 | 8.7 |
| 60 | fullgc | 310.0 | 337.8 | 14.6 |
| 90 | fullgc | 312.4 | 336.9 | 23.2 |
| 120 | fullgc | 325.0 | 349.2 | 17.3 |
| 150 | fullgc | 330.6 | 354.7 | 25.8 |
| 180 | fullgc | 329.8 | 351.0 | 32.7 |
| 210 | fullgc | 328.8 | 357.5 | 36.3 |
| 240 | fullgc | 336.7 | 362.6 | 34.7 |
| 270 | fullgc | 336.5 | 359.6 | 41.5 |
| 300 | compact | 363.1 | 391.0 | 31.2 |
| 330 | fullgc | 390.9 | 408.3 | 41.9 |
| 360 | fullgc | 375.6 | 394.5 | 56.8 |
| 390 | fullgc | 386.4 | 402.8 | 53.5 |
| 420 | fullgc | 388.9 | 403.4 | 57.6 |
| 450 | fullgc | 387.6 | 396.7 | 66.1 |
| 480 | fullgc | 395.3 | 400.8 | 67.0 |
| 510 | fullgc | 403.0 | 409.5 | 62.7 |
| 540 | fullgc | 401.3 | 404.2 | 70.7 |
| 570 | fullgc | 404.3 | 407.0 | 71.5 |
| 600 | fullgc | 415.1 | 418.7 | 65.3 |
| 600 | final-fullgc | 415.6 | 418.8 | 65.2 |

## Samples

- challoc: `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-compact0-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-compact0-challoc-mstats.txt`; summary line: `jobs=151776 live_objects=75902 gc_count=16558 major=1421 minor=15137 heap_pages=354 rss_final_kb=425540 hwm_kb=429272 mallctl=yes alloc_allocated=111581600 alloc_active=222433280 alloc_resident=439234560 alloc_retained=68292608 alloc_mapped=507527168 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
