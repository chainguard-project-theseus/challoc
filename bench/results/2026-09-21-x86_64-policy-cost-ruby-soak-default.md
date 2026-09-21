# Ruby soak, 2026-09-21, x86_64

- commit: f53418d
- date: 2026-09-21T05:05:43Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 3.61 5.34 5.49; at end: 1.00 1.04 2.15; 1225 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 2ccf9f46d160cd96), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 417.9 | 179.1 | +133.3% |
| VmHWM at the end | 419.1 | 186.9 | +124.2% |
| cgroup memory.peak, start..compact | - | - | - |
| cgroup memory.peak, compact..end | - | - | - |
| at minute 1 (t=60) | 309.1 | 73.6 | +320.0% |
| at minute 5, before GC.compact (t=295) | 349.8 | 119.2 | +193.5% |
| at minute 5, after GC.compact (t=300) | 361.9 | 127.0 | +185.0% |
| at minute 10 (t=600) | 414.0 | 133.9 | +209.2% |
| after the last full GC (final) | 414.0 | 133.9 | +209.2% |
| lowest right after a periodic full GC | 302.4 | 73.5 | +311.4% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 106.0 | 212.9 | 417.3 | 66.3 | 483.6 | 390.6 | 417.3 | 1.01 |
| jemalloc | 94.8 | 107.3 | 123.4 | 94.7 | 152.3 | 125.2 | 123.4 | 0.92 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 151624 | 75826 | 16211 | 1301 | 14910 | 350 |
| jemalloc | 151262 | 75644 | 16319 | 1358 | 14961 | 349 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 41.5 | 84.4 | 0.0 | 38.9 | 56.2 | 15.6 |
| 30 | fullgc | 302.4 | 330.3 | 8.6 | 73.9 | 83.9 | 31.3 |
| 60 | fullgc | 309.1 | 335.2 | 18.1 | 73.6 | 80.4 | 34.9 |
| 90 | fullgc | 312.4 | 336.4 | 24.3 | 73.5 | 79.6 | 35.8 |
| 120 | fullgc | 326.2 | 349.6 | 20.9 | 90.9 | 95.7 | 61.8 |
| 150 | fullgc | 330.7 | 354.6 | 27.9 | 91.9 | 96.0 | 64.6 |
| 180 | fullgc | 331.2 | 352.5 | 32.8 | 92.4 | 95.9 | 64.7 |
| 210 | fullgc | 331.0 | 359.6 | 35.9 | 91.2 | 93.8 | 66.8 |
| 240 | fullgc | 336.4 | 361.9 | 37.6 | 99.4 | 100.9 | 59.8 |
| 270 | fullgc | 333.8 | 356.4 | 46.2 | 98.1 | 99.4 | 61.3 |
| 300 | compact | 361.9 | 390.6 | 32.1 | 127.0 | 125.2 | 39.3 |
| 330 | fullgc | 376.9 | 394.1 | 54.8 | 127.0 | 124.2 | 93.5 |
| 360 | fullgc | 379.7 | 399.1 | 51.5 | 141.6 | 138.1 | 79.6 |
| 390 | fullgc | 383.1 | 399.4 | 55.4 | 147.4 | 142.9 | 74.9 |
| 420 | fullgc | 393.6 | 408.1 | 52.4 | 157.0 | 151.8 | 66.0 |
| 450 | fullgc | 393.7 | 402.7 | 61.5 | 157.9 | 151.9 | 65.9 |
| 480 | fullgc | 395.9 | 401.6 | 65.9 | 159.6 | 152.6 | 65.2 |
| 510 | fullgc | 394.1 | 400.0 | 70.6 | 163.8 | 156.2 | 61.8 |
| 540 | fullgc | 406.4 | 410.0 | 66.4 | 176.6 | 168.3 | 49.7 |
| 570 | fullgc | 406.0 | 409.2 | 70.8 | 179.1 | 169.7 | 48.3 |
| 600 | fullgc | 413.6 | 417.3 | 66.3 | 177.8 | 167.2 | 50.9 |
| 600 | final-fullgc | 414.0 | 417.3 | 66.3 | 133.9 | 123.4 | 94.7 |

## Samples

- challoc: `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-default-challoc.csv` (122 rows), allocator report `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-default-challoc-mstats.txt`; summary line: `jobs=151624 live_objects=75826 gc_count=16211 major=1301 minor=14910 heap_pages=350 rss_final_kb=423976 hwm_kb=429112 mallctl=yes alloc_allocated=111177200 alloc_active=223301632 alloc_resident=437628928 alloc_retained=69431296 alloc_mapped=507060224 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-default-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-21-x86_64-policy-cost-ruby-soak-default-jemalloc-mstats.txt`; summary line: `jobs=151262 live_objects=75644 gc_count=16319 major=1358 minor=14961 heap_pages=349 rss_final_kb=137096 hwm_kb=191336 mallctl=yes alloc_allocated=99375072 alloc_active=112508928 alloc_resident=129347584 alloc_retained=99336192 alloc_mapped=159662080 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
