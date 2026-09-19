# Ruby soak, 2026-09-19, aarch64

- commit: fa27979 (uncommitted changes in src/)
- date: 2026-09-19T22:57:18Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 180 s, 16 threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 90 s, one full GC after the workers stop (the final row)
- load average at start: 1.21 5.70 4.91; at end: 1.00 2.37 3.63; 368 s total
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 5b91a86cd50588be), default MALLOC_CONF
  - challoc-percpu: as challoc, with MALLOC_CONF=percpu_arena:percpu (per-CPU caches instead of thread caches)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | challoc-percpu | challoc-percpu vs challoc |
|---|---:|---:|---:|
| peak sampled VmRSS | 305.4 | 89.7 | -70.6% |
| VmHWM at the end | 305.4 | 89.7 | -70.6% |
| at minute 1 (t=60) | 285.0 | 70.3 | -75.3% |
| at minute 5, before GC.compact (t=85) | 282.6 | 74.6 | -73.6% |
| at minute 5, after GC.compact (t=90) | 290.6 | 74.6 | -74.3% |
| at minute 10 (t=180) | 305.4 | 89.7 | -70.6% |
| after the last full GC (final) | 305.4 | 89.7 | -70.6% |
| lowest right after a periodic full GC | 279.2 | 65.1 | -76.7% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=90 | resident at t=180 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 48.0 | 130.4 | 327.5 | 25.9 | 353.3 | 313.9 | 327.5 | 1.07 |
| challoc-percpu | 52.5 | 85.2 | 115.2 | 19.4 | 134.7 | 97.8 | 115.2 | 1.28 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 41485 | 20756 | 10183 | 19 | 10164 | 118 |
| challoc-percpu | 41677 | 20848 | 10215 | 18 | 10197 | 118 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=90 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | challoc-percpu VmRSS | challoc-percpu resident | challoc-percpu retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 40.6 | 82.2 | 0.0 | 33.6 | 66.9 | 0.0 |
| 30 | fullgc | 279.2 | 308.0 | 11.2 | 65.1 | 92.1 | 12.5 |
| 60 | fullgc | 285.0 | 312.1 | 13.3 | 70.3 | 96.8 | 13.2 |
| 90 | compact | 290.6 | 313.9 | 19.2 | 74.6 | 97.8 | 18.7 |
| 120 | fullgc | 296.6 | 320.5 | 20.8 | 79.9 | 104.3 | 17.8 |
| 150 | fullgc | 302.1 | 327.8 | 21.2 | 86.5 | 112.0 | 18.7 |
| 180 | fullgc | 305.4 | 327.5 | 25.9 | 89.7 | 115.2 | 19.4 |
| 180 | final-fullgc | 305.4 | 327.5 | 25.9 | 89.7 | 115.2 | 19.4 |

## Samples

- challoc: `bench/results/2026-09-19-aarch64-task8-ruby-soak-t16-challoc.csv` (38 rows), allocator report `bench/results/2026-09-19-aarch64-task8-ruby-soak-t16-challoc-mstats.txt`; summary line: `jobs=41485 live_objects=20756 gc_count=10183 major=19 minor=10164 heap_pages=118 rss_final_kb=312720 hwm_kb=312720 mallctl=yes alloc_allocated=50455776 alloc_active=136835072 alloc_resident=343502848 alloc_retained=27009024 alloc_mapped=370511872 compacted=true`
- challoc-percpu: `bench/results/2026-09-19-aarch64-task8-ruby-soak-t16-challoc-percpu.csv` (38 rows), allocator report `bench/results/2026-09-19-aarch64-task8-ruby-soak-t16-challoc-percpu-mstats.txt`; summary line: `jobs=41677 live_objects=20848 gc_count=10215 major=18 minor=10197 heap_pages=118 rss_final_kb=91820 hwm_kb=91856 mallctl=yes alloc_allocated=55155584 alloc_active=89513984 alloc_resident=121020416 alloc_retained=20312064 alloc_mapped=141332480 compacted=true`
