# Ruby soak, 2026-09-21, aarch64

- commit: f53418d
- date: 2026-09-21T07:12:42Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- CHALLOC_CONF=tcache_compact_shift:0 (read by challoc alone; jemalloc runs unchanged)
- load average at start: 0.94 0.98 1.00; at end: 1.00 1.00 1.00; 609 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 28d2ce5bde45aefe), default MALLOC_CONF plus CHALLOC_CONF=tcache_compact_shift:0

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc |
|---|---:|
| peak sampled VmRSS | 402.3 |
| VmHWM at the end | 403.5 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 286.0 |
| at minute 5, before GC.compact (t=295) | 312.1 |
| at minute 5, after GC.compact (t=300) | 324.0 |
| at minute 10 (t=600) | 389.0 |
| after the last full GC (final) | 389.0 |
| lowest right after a periodic full GC | 280.3 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 97.4 | 202.4 | 392.4 | 80.3 | 472.7 | 349.1 | 392.4 | 1.01 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 137779 | 68898 | 16418 | 151 | 16267 | 327 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained |
|---:|---|---:|---:|---:|
| 0 | start | 40.9 | 83.0 | 0.0 |
| 30 | fullgc | 280.3 | 308.4 | 10.4 |
| 60 | fullgc | 286.0 | 312.5 | 13.3 |
| 90 | fullgc | 291.3 | 314.2 | 18.7 |
| 120 | fullgc | 296.2 | 321.0 | 20.1 |
| 150 | fullgc | 300.5 | 326.0 | 23.7 |
| 180 | fullgc | 304.7 | 327.4 | 26.4 |
| 210 | fullgc | 311.4 | 341.6 | 25.6 |
| 240 | fullgc | 317.2 | 343.6 | 28.8 |
| 270 | fullgc | 318.6 | 342.7 | 32.5 |
| 300 | compact | 324.0 | 349.1 | 29.9 |
| 330 | fullgc | 359.2 | 387.3 | 30.1 |
| 360 | fullgc | 387.5 | 405.6 | 39.9 |
| 390 | fullgc | 362.5 | 380.2 | 68.4 |
| 420 | fullgc | 378.9 | 395.5 | 55.7 |
| 450 | fullgc | 382.6 | 399.4 | 55.9 |
| 480 | fullgc | 386.0 | 401.3 | 57.5 |
| 510 | fullgc | 386.2 | 393.3 | 68.4 |
| 540 | fullgc | 388.7 | 393.2 | 71.9 |
| 570 | fullgc | 396.4 | 402.1 | 67.5 |
| 600 | fullgc | 388.7 | 392.1 | 80.6 |
| 600 | final-fullgc | 389.0 | 392.4 | 80.3 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-compact0-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-compact0-challoc-mstats.txt`; summary line: `jobs=137779 live_objects=68898 gc_count=16418 major=151 minor=16267 heap_pages=327 rss_final_kb=398352 hwm_kb=413224 mallctl=yes alloc_allocated=102176992 alloc_active=212316160 alloc_resident=411570176 alloc_retained=84111360 alloc_mapped=495681536 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
