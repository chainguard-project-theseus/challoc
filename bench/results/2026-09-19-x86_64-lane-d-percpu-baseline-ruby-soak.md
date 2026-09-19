# Ruby soak, 2026-09-19, x86_64

- commit: 675f357
- date: 2026-09-19T22:12:56Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 4.57 2.27 1.79; at end: 1.00 1.00 1.18; 1207 s total
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 d74436810319c034), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 419.1 | 184.1 | +127.6% |
| VmHWM at the end | 421.8 | 185.9 | +126.9% |
| at minute 1 (t=60) | 308.9 | 72.5 | +326.1% |
| at minute 5, before GC.compact (t=295) | 347.2 | 115.3 | +201.1% |
| at minute 5, after GC.compact (t=300) | 362.8 | 116.2 | +212.2% |
| at minute 10 (t=600) | 413.0 | 167.0 | +147.3% |
| after the last full GC (final) | 413.0 | 167.0 | +147.3% |
| lowest right after a periodic full GC | 302.0 | 72.5 | +316.6% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 106.4 | 218.1 | 414.4 | 71.3 | 485.7 | 389.7 | 414.4 | 1.00 |
| jemalloc | 93.9 | 106.5 | 156.4 | 61.2 | 185.8 | 115.7 | 156.4 | 0.94 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 152220 | 76120 | 16396 | 1353 | 15043 | 352 |
| jemalloc | 149536 | 74782 | 16348 | 1377 | 14971 | 346 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 40.7 | 83.0 | 0.0 | 39.2 | 55.9 | 15.5 |
| 30 | fullgc | 302.0 | 330.3 | 8.1 | 72.7 | 79.5 | 32.7 |
| 60 | fullgc | 308.9 | 337.4 | 15.1 | 72.5 | 79.3 | 36.0 |
| 90 | fullgc | 313.0 | 337.0 | 24.5 | 74.1 | 80.4 | 34.9 |
| 120 | fullgc | 329.1 | 353.5 | 19.4 | 89.8 | 94.8 | 59.7 |
| 150 | fullgc | 333.0 | 359.3 | 25.3 | 92.8 | 97.1 | 63.5 |
| 180 | fullgc | 331.3 | 352.4 | 35.5 | 91.4 | 94.8 | 65.8 |
| 210 | fullgc | 328.9 | 359.7 | 38.4 | 90.6 | 93.7 | 67.0 |
| 240 | fullgc | 335.5 | 360.8 | 40.5 | 97.6 | 99.4 | 61.3 |
| 270 | fullgc | 332.8 | 356.9 | 47.4 | 96.4 | 97.7 | 63.0 |
| 300 | compact | 362.8 | 389.7 | 34.1 | 116.2 | 115.7 | 45.1 |
| 330 | fullgc | 398.0 | 416.4 | 40.8 | 163.1 | 159.2 | 58.1 |
| 360 | fullgc | 377.4 | 396.6 | 61.8 | 129.9 | 125.8 | 91.5 |
| 390 | fullgc | 381.2 | 398.3 | 62.2 | 147.5 | 142.9 | 74.5 |
| 420 | fullgc | 390.3 | 405.5 | 58.3 | 147.0 | 141.6 | 75.7 |
| 450 | fullgc | 390.8 | 400.2 | 68.3 | 157.0 | 150.9 | 66.5 |
| 480 | fullgc | 398.0 | 402.7 | 69.0 | 159.9 | 152.9 | 64.5 |
| 510 | fullgc | 395.9 | 399.3 | 75.4 | 166.6 | 158.5 | 58.9 |
| 540 | fullgc | 406.6 | 409.5 | 67.9 | 171.0 | 162.2 | 55.3 |
| 570 | fullgc | 409.6 | 411.7 | 69.4 | 172.4 | 163.0 | 54.5 |
| 600 | fullgc | 413.0 | 414.4 | 71.3 | 167.0 | 156.4 | 61.2 |
| 600 | final-fullgc | 413.0 | 414.4 | 71.3 | 167.0 | 156.4 | 61.2 |

## Samples

- challoc: `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-soak-challoc.csv` (122 rows), allocator report `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-soak-challoc-mstats.txt`; summary line: `jobs=152220 live_objects=76120 gc_count=16396 major=1353 minor=15043 heap_pages=352 rss_final_kb=422888 hwm_kb=431936 mallctl=yes alloc_allocated=111660096 alloc_active=228794368 alloc_resident=434630656 alloc_retained=74698752 alloc_mapped=509329408 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16`
- jemalloc: `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-soak-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-soak-jemalloc-mstats.txt`; summary line: `jobs=149536 live_objects=74782 gc_count=16348 major=1377 minor=14971 heap_pages=346 rss_final_kb=171044 hwm_kb=190368 mallctl=yes alloc_allocated=98489672 alloc_active=111685632 alloc_resident=164020224 alloc_retained=64139264 alloc_mapped=194859008 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16`
