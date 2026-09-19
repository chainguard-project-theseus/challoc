# Ruby soak, 2026-09-19, aarch64

- commit: 675f357
- date: 2026-09-19T22:21:38Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 48 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- load average at start: 1.00 1.00 1.17; at end: 0.92 0.98 1.06; 608 s total
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 4e95eb3737dc3098), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 415.6 | 191.5 | +117.0% |
| VmHWM at the end | 415.6 | 191.5 | +117.0% |
| at minute 1 (t=60) | 352.1 | 170.1 | +107.0% |
| at minute 5, before GC.compact (t=145) | 369.6 | 177.2 | +108.6% |
| at minute 5, after GC.compact (t=150) | 378.3 | 180.9 | +109.1% |
| at minute 10 (t=300) | 415.6 | 187.8 | +121.3% |
| after the last full GC (final) | 415.6 | 141.1 | +194.5% |
| lowest right after a periodic full GC | 341.2 | 162.4 | +110.1% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 111.2 | 224.6 | 503.9 | 57.3 | 561.2 | 471.1 | 503.9 | 1.21 |
| jemalloc | 105.9 | 183.2 | 235.7 | 341.2 | 355.2 | 351.1 | 347.4 | 1.67 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 67747 | 33888 | 13861 | 34 | 13827 | 154 |
| jemalloc | 69341 | 34686 | 13813 | 29 | 13784 | 168 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 108.1 | 227.9 | 0.0 | 107.5 | 330.4 | 107.5 |
| 30 | fullgc | 341.2 | 440.0 | 23.6 | 162.4 | 343.8 | 162.2 |
| 60 | fullgc | 352.1 | 445.3 | 33.3 | 170.1 | 346.6 | 166.4 |
| 90 | fullgc | 364.8 | 457.9 | 34.9 | 178.4 | 355.8 | 157.1 |
| 120 | fullgc | 369.4 | 462.5 | 38.3 | 172.8 | 342.4 | 170.6 |
| 150 | compact | 378.3 | 471.1 | 41.9 | 180.9 | 351.1 | 173.8 |
| 180 | fullgc | 387.8 | 473.6 | 46.8 | 184.3 | 346.3 | 198.6 |
| 210 | fullgc | 402.3 | 497.2 | 44.8 | 186.5 | 348.5 | 208.4 |
| 240 | fullgc | 410.3 | 502.1 | 49.2 | 188.1 | 351.8 | 221.1 |
| 270 | fullgc | 414.3 | 504.7 | 52.4 | 188.5 | 349.2 | 227.7 |
| 300 | fullgc | 415.6 | 503.9 | 57.3 | 187.8 | 347.4 | 229.6 |
| 300 | final-fullgc | 415.6 | 503.9 | 57.3 | 0.0 | - | - |

## Samples

- challoc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-idle-challoc.csv` (62 rows), allocator report `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-idle-challoc-mstats.txt`; summary line: `jobs=67747 live_objects=33888 gc_count=13861 major=34 minor=13827 heap_pages=154 rss_final_kb=425608 hwm_kb=425608 mallctl=yes alloc_allocated=116664592 alloc_active=235646976 alloc_resident=528474112 alloc_retained=59961344 alloc_mapped=588435456 compacted=true threads=16 idle_threads=48 idle_bursts=2059 nproc=16`
- jemalloc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-idle-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-idle-jemalloc-mstats.txt`; summary line: `jobs=69341 live_objects=34686 gc_count=13813 major=29 minor=13784 heap_pages=168 rss_final_kb=144508 hwm_kb=196120 mallctl=yes alloc_allocated=111030232 alloc_active=192151552 alloc_resident=247136256 alloc_retained=357826560 alloc_mapped=372506624 compacted=true threads=16 idle_threads=48 idle_bursts=2073 nproc=16`
