# Ruby soak, 2026-09-21, x86_64

- commit: f53418d
- date: 2026-09-21T05:26:11Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 1.00 1.04 2.15; at end: 1.00 1.00 1.57; 608 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 338bc875a8f3194f, built from the A/B source copy .tmp-slab16), default MALLOC_CONF

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc |
|---|---:|
| peak sampled VmRSS | 418.9 |
| VmHWM at the end | 423.9 |
| cgroup memory.peak, start..compact | - |
| cgroup memory.peak, compact..end | - |
| at minute 1 (t=60) | 314.3 |
| at minute 5, before GC.compact (t=295) | 347.9 |
| at minute 5, after GC.compact (t=300) | 362.9 |
| at minute 10 (t=600) | 418.9 |
| after the last full GC (final) | 418.9 |
| lowest right after a periodic full GC | 307.9 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 106.0 | 188.1 | 421.4 | 70.6 | 492.0 | 388.9 | 421.4 | 1.01 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 151025 | 75518 | 16413 | 1382 | 15031 | 353 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained |
|---:|---|---:|---:|---:|
| 0 | start | 42.0 | 82.4 | 0.0 |
| 30 | fullgc | 307.9 | 333.6 | 9.0 |
| 60 | fullgc | 314.3 | 340.6 | 17.7 |
| 90 | fullgc | 315.7 | 337.8 | 26.3 |
| 120 | fullgc | 330.7 | 351.7 | 22.0 |
| 150 | fullgc | 333.3 | 355.5 | 31.6 |
| 180 | fullgc | 336.8 | 356.7 | 34.9 |
| 210 | fullgc | 336.9 | 363.6 | 39.3 |
| 240 | fullgc | 341.3 | 364.6 | 41.4 |
| 270 | fullgc | 341.4 | 362.8 | 47.7 |
| 300 | compact | 362.9 | 388.9 | 38.5 |
| 330 | fullgc | 383.2 | 398.1 | 58.7 |
| 360 | fullgc | 375.1 | 391.0 | 67.0 |
| 390 | fullgc | 390.6 | 404.8 | 57.9 |
| 420 | fullgc | 391.9 | 405.4 | 62.0 |
| 450 | fullgc | 400.9 | 412.6 | 58.7 |
| 480 | fullgc | 399.4 | 402.7 | 71.8 |
| 510 | fullgc | 400.0 | 404.3 | 73.3 |
| 540 | fullgc | 406.2 | 407.9 | 75.0 |
| 570 | fullgc | 417.3 | 418.1 | 70.7 |
| 600 | fullgc | 418.4 | 421.3 | 70.7 |
| 600 | final-fullgc | 418.9 | 421.4 | 70.6 |

## Samples

- challoc: `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-slab16-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-slab16-challoc-mstats.txt`; summary line: `jobs=151025 live_objects=75518 gc_count=16413 major=1382 minor=15031 heap_pages=353 rss_final_kb=428908 hwm_kb=434076 mallctl=yes alloc_allocated=111191952 alloc_active=197251072 alloc_resident=441966592 alloc_retained=73936896 alloc_mapped=515903488 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
