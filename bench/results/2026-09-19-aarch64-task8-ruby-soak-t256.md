# Ruby soak, 2026-09-19, aarch64

- commit: fa27979 (uncommitted changes in src/)
- date: 2026-09-19T23:09:40Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 180 s, 256 threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 90 s, one full GC after the workers stop (the final row)
- load average at start: 1.00 1.38 2.74; at end: 1.00 1.10 2.14; 382 s total
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 5b91a86cd50588be), default MALLOC_CONF
  - challoc-percpu: as challoc, with MALLOC_CONF=percpu_arena:percpu (per-CPU caches instead of thread caches)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | challoc-percpu | challoc-percpu vs challoc |
|---|---:|---:|---:|
| peak sampled VmRSS | 2426.0 | 115.0 | -95.3% |
| VmHWM at the end | 2439.0 | 114.9 | -95.3% |
| at minute 1 (t=60) | 2423.0 | 82.6 | -96.6% |
| at minute 5, before GC.compact (t=85) | 2391.7 | 93.2 | -96.1% |
| at minute 5, after GC.compact (t=90) | 2348.1 | 96.1 | -95.9% |
| at minute 10 (t=180) | 2322.5 | 115.0 | -95.0% |
| after the last full GC (final) | 2388.1 | 108.6 | -95.5% |
| lowest right after a periodic full GC | 2308.5 | 83.5 | -96.4% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=90 | resident at t=180 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 294.2 | 811.6 | 2662.8 | 336.4 | 2999.2 | 2653.0 | 2627.8 | 1.12 |
| challoc-percpu | 296.1 | 326.1 | 372.5 | 19.4 | 391.9 | 356.5 | 374.6 | 3.43 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 42113 | 21250 | 9147 | 22 | 9125 | 126 |
| challoc-percpu | 42092 | 21208 | 9208 | 20 | 9188 | 126 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=90 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | challoc-percpu VmRSS | challoc-percpu resident | challoc-percpu retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 60 | sample | 2423.0 | 2689.4 | 189.7 | 0.0 | - | - |
| 104 | compact | 2348.1 | 2659.9 | 290.8 | 0.0 | - | - |
| 189 | final-fullgc | 2388.1 | 2662.8 | 336.4 | 0.0 | - | - |

## Samples

- challoc: `bench/results/2026-09-19-aarch64-task8-ruby-soak-t256-challoc.csv` (18 rows), allocator report `bench/results/2026-09-19-aarch64-task8-ruby-soak-t256-challoc-mstats.txt`; summary line: `jobs=42113 live_objects=21250 gc_count=9147 major=22 minor=9125 heap_pages=126 rss_final_kb=2445460 hwm_kb=2497504 mallctl=yes alloc_allocated=308529040 alloc_active=851050496 alloc_resident=2792181760 alloc_retained=352727040 alloc_mapped=3144908800 compacted=true`
- challoc-percpu: `bench/results/2026-09-19-aarch64-task8-ruby-soak-t256-challoc-percpu.csv` (18 rows), allocator report `bench/results/2026-09-19-aarch64-task8-ruby-soak-t256-challoc-percpu-mstats.txt`; summary line: `jobs=42092 live_objects=21208 gc_count=9208 major=20 minor=9188 heap_pages=126 rss_final_kb=111220 hwm_kb=117632 mallctl=yes alloc_allocated=310509504 alloc_active=341975040 alloc_resident=390688768 alloc_retained=20340736 alloc_mapped=411029504 compacted=true`
