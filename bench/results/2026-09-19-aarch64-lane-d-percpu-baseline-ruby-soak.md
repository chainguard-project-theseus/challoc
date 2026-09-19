# Ruby soak, 2026-09-19, aarch64

- commit: 675f357
- date: 2026-09-19T22:01:29Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 1.01 1.42 1.72; at end: 1.00 1.00 1.17; 1208 s total
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 4e95eb3737dc3098), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 403.6 | 198.4 | +103.4% |
| VmHWM at the end | 403.4 | 201.1 | +100.6% |
| at minute 1 (t=60) | 285.2 | 96.7 | +194.9% |
| at minute 5, before GC.compact (t=295) | 315.6 | 131.4 | +140.2% |
| at minute 5, after GC.compact (t=300) | 323.4 | 130.3 | +148.2% |
| at minute 10 (t=600) | 397.0 | 160.6 | +147.2% |
| after the last full GC (final) | 397.0 | 160.6 | +147.2% |
| lowest right after a periodic full GC | 279.3 | 93.5 | +198.7% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 99.1 | 207.8 | 401.7 | 66.0 | 467.7 | 348.3 | 401.7 | 1.01 |
| jemalloc | 90.7 | 138.6 | 169.6 | 119.2 | 199.8 | 160.8 | 169.6 | 1.06 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 136950 | 68486 | 16521 | 139 | 16382 | 326 |
| jemalloc | 139314 | 69670 | 16510 | 145 | 16365 | 330 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 40.4 | 82.8 | 0.0 | 41.7 | 111.2 | 31.6 |
| 30 | fullgc | 279.3 | 306.9 | 11.2 | 93.5 | 133.6 | 86.8 |
| 60 | fullgc | 285.2 | 312.1 | 13.0 | 96.7 | 135.8 | 88.1 |
| 90 | fullgc | 290.9 | 314.1 | 19.8 | 101.2 | 140.7 | 83.2 |
| 120 | fullgc | 296.2 | 320.8 | 19.8 | 105.4 | 142.4 | 81.4 |
| 150 | fullgc | 300.8 | 326.5 | 23.2 | 115.5 | 151.9 | 83.9 |
| 180 | fullgc | 306.2 | 329.0 | 24.8 | 122.4 | 158.8 | 89.1 |
| 210 | fullgc | 308.9 | 338.7 | 27.5 | 124.2 | 160.6 | 111.3 |
| 240 | fullgc | 313.5 | 340.5 | 30.1 | 127.8 | 161.7 | 122.2 |
| 270 | fullgc | 315.5 | 341.5 | 32.0 | 129.0 | 161.2 | 122.6 |
| 300 | compact | 323.4 | 348.3 | 31.3 | 130.3 | 160.8 | 123.1 |
| 330 | fullgc | 346.6 | 375.3 | 34.5 | 145.3 | 169.9 | 114.0 |
| 360 | fullgc | 367.0 | 387.1 | 38.5 | 159.2 | 179.6 | 104.3 |
| 390 | fullgc | 368.5 | 385.3 | 49.5 | 169.3 | 198.3 | 89.6 |
| 420 | fullgc | 364.9 | 381.2 | 60.0 | 170.6 | 194.9 | 93.0 |
| 450 | fullgc | 376.5 | 393.3 | 53.3 | 175.9 | 200.1 | 88.1 |
| 480 | fullgc | 383.7 | 398.5 | 54.5 | 182.6 | 199.3 | 89.0 |
| 510 | fullgc | 386.0 | 393.3 | 63.9 | 183.5 | 200.2 | 88.1 |
| 540 | fullgc | 392.0 | 399.7 | 61.5 | 189.4 | 203.3 | 85.2 |
| 570 | fullgc | 390.7 | 396.9 | 67.6 | 195.2 | 210.6 | 78.0 |
| 600 | fullgc | 397.0 | 401.7 | 66.0 | 160.6 | 169.6 | 119.2 |
| 600 | final-fullgc | 397.0 | 401.7 | 66.0 | 160.6 | 169.6 | 119.2 |

## Samples

- challoc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-soak-challoc.csv` (122 rows), allocator report `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-soak-challoc-mstats.txt`; summary line: `jobs=136950 live_objects=68486 gc_count=16521 major=139 minor=16382 heap_pages=326 rss_final_kb=406488 hwm_kb=413108 mallctl=yes alloc_allocated=103981600 alloc_active=217980928 alloc_resident=421232640 alloc_retained=69160960 alloc_mapped=490393600 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16`
- jemalloc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-soak-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-soak-jemalloc-mstats.txt`; summary line: `jobs=139314 live_objects=69670 gc_count=16510 major=145 minor=16365 heap_pages=330 rss_final_kb=164500 hwm_kb=205964 mallctl=yes alloc_allocated=95063688 alloc_active=145293312 alloc_resident=177799168 alloc_retained=124977152 alloc_mapped=209518592 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16`
