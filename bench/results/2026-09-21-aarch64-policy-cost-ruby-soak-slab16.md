# Ruby soak, 2026-09-21, aarch64

- commit: f53418d
- date: 2026-09-21T06:51:59Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 0.85 0.97 1.16; at end: 1.00 1.00 1.07; 608 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 2703dd952f70e6db, built from the A/B source copy .tmp-slab16), default MALLOC_CONF

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc |
|---|---:|
| peak sampled VmRSS | 406.3 |
| VmHWM at the end | 411.3 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 292.8 |
| at minute 5, before GC.compact (t=295) | 314.1 |
| at minute 5, after GC.compact (t=300) | 324.5 |
| at minute 10 (t=600) | 399.7 |
| after the last full GC (final) | 399.7 |
| lowest right after a periodic full GC | 286.6 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 97.7 | 176.7 | 402.1 | 79.6 | 481.8 | 347.1 | 402.1 | 1.01 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 137044 | 68532 | 16128 | 147 | 15981 | 327 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained |
|---:|---|---:|---:|---:|
| 0 | start | 42.0 | 81.6 | 0.0 |
| 30 | fullgc | 286.6 | 312.5 | 13.3 |
| 60 | fullgc | 292.8 | 317.4 | 15.8 |
| 90 | fullgc | 297.6 | 318.4 | 21.7 |
| 120 | fullgc | 302.6 | 324.6 | 22.9 |
| 150 | fullgc | 305.7 | 329.2 | 27.0 |
| 180 | fullgc | 310.1 | 330.5 | 29.2 |
| 210 | fullgc | 317.2 | 344.9 | 28.5 |
| 240 | fullgc | 320.3 | 345.2 | 31.6 |
| 270 | fullgc | 323.1 | 345.7 | 34.8 |
| 300 | compact | 324.5 | 347.1 | 36.1 |
| 330 | fullgc | 362.0 | 387.3 | 31.4 |
| 360 | fullgc | 394.0 | 409.7 | 45.9 |
| 390 | fullgc | 364.1 | 379.4 | 81.2 |
| 420 | fullgc | 382.3 | 396.5 | 65.7 |
| 450 | fullgc | 380.9 | 396.0 | 67.4 |
| 480 | fullgc | 391.6 | 405.9 | 61.7 |
| 510 | fullgc | 398.6 | 404.9 | 66.8 |
| 540 | fullgc | 393.8 | 397.9 | 76.5 |
| 570 | fullgc | 397.3 | 403.6 | 73.8 |
| 600 | fullgc | 399.5 | 402.5 | 79.2 |
| 600 | final-fullgc | 399.7 | 402.1 | 79.6 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-slab16-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-slab16-challoc-mstats.txt`; summary line: `jobs=137044 live_objects=68532 gc_count=16128 major=147 minor=15981 heap_pages=327 rss_final_kb=409280 hwm_kb=421152 mallctl=yes alloc_allocated=102467200 alloc_active=185393152 alloc_resident=421724160 alloc_retained=83443712 alloc_mapped=505167872 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
