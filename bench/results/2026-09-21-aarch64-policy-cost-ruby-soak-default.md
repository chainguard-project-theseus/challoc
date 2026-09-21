# Ruby soak, 2026-09-21, aarch64

- commit: f53418d
- date: 2026-09-21T06:31:25Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 1.00 1.69 1.77; at end: 1.00 1.00 1.17; 1224 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 28d2ce5bde45aefe), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 402.1 | 203.3 | +97.8% |
| VmHWM at the end | 401.9 | 207.8 | +93.4% |
| cgroup memory.peak, start..compact | - | - | - |
| cgroup memory.peak, compact..end | - | - | - |
| at minute 1 (t=60) | 286.7 | 97.0 | +195.6% |
| at minute 5, before GC.compact (t=295) | 313.1 | 130.6 | +139.7% |
| at minute 5, after GC.compact (t=300) | 321.7 | 132.7 | +142.4% |
| at minute 10 (t=600) | 397.2 | 171.6 | +131.5% |
| after the last full GC (final) | 397.2 | 171.6 | +131.5% |
| lowest right after a periodic full GC | 280.8 | 93.5 | +200.3% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 100.0 | 210.9 | 403.4 | 68.4 | 471.8 | 345.9 | 403.4 | 1.02 |
| jemalloc | 90.6 | 139.7 | 179.6 | 118.7 | 210.3 | 162.2 | 179.6 | 1.05 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 137915 | 68968 | 16624 | 158 | 16466 | 327 |
| jemalloc | 138799 | 69410 | 16665 | 154 | 16511 | 331 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 40.9 | 83.0 | 0.0 | 41.7 | 108.6 | 30.8 |
| 30 | fullgc | 280.8 | 308.8 | 10.4 | 93.5 | 130.8 | 89.1 |
| 60 | fullgc | 286.7 | 313.4 | 13.8 | 97.0 | 133.7 | 89.7 |
| 90 | fullgc | 292.9 | 316.2 | 18.9 | 105.8 | 140.8 | 82.6 |
| 120 | fullgc | 296.8 | 320.4 | 21.6 | 103.7 | 139.4 | 87.9 |
| 150 | fullgc | 299.9 | 325.5 | 24.8 | 112.5 | 150.2 | 81.2 |
| 180 | fullgc | 304.9 | 327.7 | 26.2 | 123.2 | 158.2 | 89.2 |
| 210 | fullgc | 309.5 | 338.9 | 27.4 | 123.5 | 159.6 | 111.8 |
| 240 | fullgc | 313.5 | 340.4 | 29.7 | 127.6 | 162.7 | 124.7 |
| 270 | fullgc | 317.0 | 341.7 | 32.0 | 129.0 | 161.1 | 126.2 |
| 300 | compact | 321.7 | 345.9 | 32.4 | 132.7 | 162.2 | 125.1 |
| 330 | fullgc | 357.0 | 384.4 | 28.2 | 151.0 | 173.1 | 114.3 |
| 360 | fullgc | 387.4 | 405.2 | 41.9 | 173.1 | 197.3 | 90.1 |
| 390 | fullgc | 360.0 | 376.8 | 73.8 | 166.8 | 186.4 | 106.6 |
| 420 | fullgc | 377.4 | 394.0 | 58.9 | 181.1 | 201.1 | 91.9 |
| 450 | fullgc | 378.5 | 394.9 | 60.9 | 183.4 | 204.8 | 88.2 |
| 480 | fullgc | 381.5 | 397.3 | 61.8 | 189.7 | 207.2 | 85.9 |
| 510 | fullgc | 387.0 | 395.8 | 66.5 | 191.4 | 207.1 | 86.0 |
| 540 | fullgc | 385.8 | 391.8 | 74.3 | 191.0 | 202.8 | 90.3 |
| 570 | fullgc | 392.6 | 397.6 | 71.1 | 194.1 | 207.8 | 85.4 |
| 600 | fullgc | 395.0 | 400.3 | 71.3 | 203.3 | 215.2 | 83.1 |
| 600 | final-fullgc | 397.2 | 403.4 | 68.4 | 171.6 | 179.6 | 118.7 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-default-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-default-challoc-mstats.txt`; summary line: `jobs=137915 live_objects=68968 gc_count=16624 major=158 minor=16466 heap_pages=327 rss_final_kb=406712 hwm_kb=411544 mallctl=yes alloc_allocated=104955536 alloc_active=221184000 alloc_resident=423071744 alloc_retained=71684096 alloc_mapped=494755840 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-default-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-21-aarch64-policy-cost-ruby-soak-default-jemalloc-mstats.txt`; summary line: `jobs=138799 live_objects=69410 gc_count=16665 major=154 minor=16511 heap_pages=331 rss_final_kb=175732 hwm_kb=212748 mallctl=yes alloc_allocated=94956016 alloc_active=146472960 alloc_resident=188284928 alloc_retained=124452864 alloc_mapped=220528640 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
