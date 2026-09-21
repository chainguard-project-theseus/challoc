# Ruby soak, 2026-09-21, aarch64

- commit: f53418d
- date: 2026-09-21T07:02:20Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 0.85 0.97 1.05; at end: 1.00 1.00 1.00; 608 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 e9f3db506e6a8997, built from the A/B source copy .tmp-slab32), default MALLOC_CONF

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc |
|---|---:|
| peak sampled VmRSS | 403.1 |
| VmHWM at the end | 405.3 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 287.5 |
| at minute 5, before GC.compact (t=295) | 311.4 |
| at minute 5, after GC.compact (t=300) | 319.4 |
| at minute 10 (t=600) | 391.2 |
| after the last full GC (final) | 391.2 |
| lowest right after a periodic full GC | 282.4 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 98.0 | 188.2 | 393.2 | 83.7 | 477.0 | 342.5 | 393.2 | 1.01 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 137619 | 68828 | 16528 | 161 | 16367 | 329 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained |
|---:|---|---:|---:|---:|
| 0 | start | 41.3 | 81.8 | 0.0 |
| 30 | fullgc | 282.4 | 309.1 | 10.5 |
| 60 | fullgc | 287.5 | 313.3 | 13.9 |
| 90 | fullgc | 292.7 | 314.7 | 19.7 |
| 120 | fullgc | 297.2 | 319.9 | 21.7 |
| 150 | fullgc | 300.7 | 325.1 | 24.8 |
| 180 | fullgc | 305.2 | 326.3 | 27.5 |
| 210 | fullgc | 311.9 | 340.2 | 26.8 |
| 240 | fullgc | 315.6 | 341.5 | 29.7 |
| 270 | fullgc | 317.5 | 340.5 | 35.0 |
| 300 | compact | 319.4 | 342.5 | 36.0 |
| 330 | fullgc | 360.8 | 386.5 | 31.2 |
| 360 | fullgc | 400.1 | 417.6 | 36.8 |
| 390 | fullgc | 362.6 | 378.2 | 81.2 |
| 420 | fullgc | 374.2 | 390.8 | 69.7 |
| 450 | fullgc | 373.3 | 387.9 | 74.2 |
| 480 | fullgc | 378.7 | 392.5 | 72.2 |
| 510 | fullgc | 377.3 | 383.7 | 82.4 |
| 540 | fullgc | 385.9 | 390.3 | 79.2 |
| 570 | fullgc | 390.9 | 394.7 | 78.7 |
| 600 | fullgc | 390.9 | 393.1 | 83.8 |
| 600 | final-fullgc | 391.2 | 393.2 | 83.7 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-slab32-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-slab32-challoc-mstats.txt`; summary line: `jobs=137619 live_objects=68828 gc_count=16528 major=161 minor=16367 heap_pages=329 rss_final_kb=400588 hwm_kb=415000 mallctl=yes alloc_allocated=102819712 alloc_active=197365760 alloc_resident=412405760 alloc_retained=87724032 alloc_mapped=500129792 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
