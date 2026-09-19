# Ruby soak, 2026-09-19, x86_64

- commit: 675f357
- date: 2026-09-19T22:33:04Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 48 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- load average at start: 1.00 1.00 1.18; at end: 1.07 1.05 1.12; 609 s total
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 d74436810319c034), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 462.8 | 156.0 | +196.7% |
| VmHWM at the end | 462.8 | 160.2 | +188.9% |
| at minute 1 (t=60) | 378.5 | 135.6 | +179.1% |
| at minute 5, before GC.compact (t=145) | 404.2 | 144.2 | +180.3% |
| at minute 5, after GC.compact (t=150) | 413.6 | 144.8 | +185.6% |
| at minute 10 (t=300) | 462.8 | 115.3 | +301.4% |
| after the last full GC (final) | 462.8 | 115.3 | +301.4% |
| lowest right after a periodic full GC | 361.3 | 132.5 | +172.7% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 115.8 | 238.4 | 556.8 | 52.2 | 608.9 | 506.1 | 556.8 | 1.20 |
| jemalloc | 107.9 | 121.1 | 162.5 | 158.4 | 286.6 | 198.7 | 162.5 | 1.41 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 71993 | 36008 | 13496 | 1296 | 12200 | 204 |
| jemalloc | 73898 | 36962 | 13517 | 1259 | 12258 | 204 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 108.2 | 231.9 | 0.0 | 100.8 | 179.8 | 54.6 |
| 30 | fullgc | 361.3 | 461.3 | 20.3 | 135.8 | 193.2 | 82.0 |
| 60 | fullgc | 378.5 | 476.8 | 29.3 | 135.6 | 192.4 | 82.9 |
| 90 | fullgc | 379.4 | 474.2 | 41.0 | 133.4 | 189.2 | 86.1 |
| 120 | fullgc | 397.6 | 489.1 | 37.6 | 148.6 | 203.0 | 114.6 |
| 150 | compact | 413.6 | 506.1 | 41.0 | 144.8 | 198.7 | 121.9 |
| 180 | fullgc | 424.8 | 513.3 | 45.5 | 140.3 | 193.8 | 126.8 |
| 210 | fullgc | 428.1 | 521.7 | 51.3 | 132.5 | 185.3 | 135.3 |
| 240 | fullgc | 439.8 | 528.9 | 52.7 | 137.4 | 189.0 | 131.7 |
| 270 | fullgc | 435.0 | 524.6 | 62.1 | 135.6 | 186.7 | 134.0 |
| 300 | fullgc | 462.8 | 556.8 | 52.2 | 115.3 | 162.5 | 158.4 |
| 300 | final-fullgc | 462.8 | 556.8 | 52.2 | 115.3 | 162.5 | 158.4 |

## Samples

- challoc: `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-idle-challoc.csv` (62 rows), allocator report `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-idle-challoc-mstats.txt`; summary line: `jobs=71993 live_objects=36008 gc_count=13496 major=1296 minor=12200 heap_pages=204 rss_final_kb=473908 hwm_kb=473908 mallctl=yes alloc_allocated=121486432 alloc_active=250077184 alloc_resident=583872512 alloc_retained=54636544 alloc_mapped=638509056 compacted=true threads=16 idle_threads=48 idle_bursts=2093 nproc=16`
- jemalloc: `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-idle-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-idle-jemalloc-mstats.txt`; summary line: `jobs=73898 live_objects=36962 gc_count=13517 major=1259 minor=12258 heap_pages=204 rss_final_kb=118108 hwm_kb=164076 mallctl=yes alloc_allocated=113160472 alloc_active=126980096 alloc_resident=170426368 alloc_retained=166084608 alloc_mapped=300531712 compacted=true threads=16 idle_threads=48 idle_bursts=2105 nproc=16`
