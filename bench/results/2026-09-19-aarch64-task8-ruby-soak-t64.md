# Ruby soak, 2026-09-19, aarch64

- commit: fa27979 (uncommitted changes in src/)
- date: 2026-09-19T23:03:27Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 180 s, 64 threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 90 s, one full GC after the workers stop (the final row)
- load average at start: 1.00 2.37 3.63; at end: 1.00 1.38 2.74; 372 s total
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 5b91a86cd50588be), default MALLOC_CONF
  - challoc-percpu: as challoc, with MALLOC_CONF=percpu_arena:percpu (per-CPU caches instead of thread caches)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | challoc-percpu | challoc-percpu vs challoc |
|---|---:|---:|---:|
| peak sampled VmRSS | 1648.8 | 105.7 | -93.6% |
| VmHWM at the end | 1655.6 | 105.9 | -93.6% |
| at minute 1 (t=60) | 1567.7 | 73.7 | -95.3% |
| at minute 5, before GC.compact (t=85) | 1614.7 | 83.5 | -94.8% |
| at minute 5, after GC.compact (t=90) | 1632.3 | 86.6 | -94.7% |
| at minute 10 (t=180) | 1605.9 | 102.5 | -93.6% |
| after the last full GC (final) | 1615.7 | 105.5 | -93.5% |
| lowest right after a periodic full GC | 1575.8 | 73.4 | -95.3% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=90 | resident at t=180 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 101.3 | 529.4 | 1697.5 | 253.4 | 1950.9 | 1703.7 | 1677.6 | 1.05 |
| challoc-percpu | 98.2 | 128.2 | 176.3 | 14.6 | 190.9 | 155.9 | 173.1 | 1.67 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 40905 | 20494 | 9625 | 19 | 9606 | 121 |
| challoc-percpu | 42110 | 21110 | 9925 | 22 | 9903 | 123 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=90 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | challoc-percpu VmRSS | challoc-percpu resident | challoc-percpu retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 91 | compact | 1632.3 | 1725.3 | 173.2 | 86.6 | 158.3 | 16.3 |
| 182 | final-fullgc | 1615.7 | 1697.5 | 253.4 | 105.5 | 176.3 | 14.6 |

## Samples

- challoc: `bench/results/2026-09-19-aarch64-task8-ruby-soak-t64-challoc.csv` (38 rows), allocator report `bench/results/2026-09-19-aarch64-task8-ruby-soak-t64-challoc-mstats.txt`; summary line: `jobs=40905 live_objects=20494 gc_count=9625 major=19 minor=9606 heap_pages=121 rss_final_kb=1654440 hwm_kb=1695368 mallctl=yes alloc_allocated=106378112 alloc_active=555266048 alloc_resident=1780092928 alloc_retained=265539584 alloc_mapped=2045632512 compacted=true`
- challoc-percpu: `bench/results/2026-09-19-aarch64-task8-ruby-soak-t64-challoc-percpu.csv` (38 rows), allocator report `bench/results/2026-09-19-aarch64-task8-ruby-soak-t64-challoc-percpu-mstats.txt`; summary line: `jobs=42110 live_objects=21110 gc_count=9925 major=22 minor=9903 heap_pages=123 rss_final_kb=108068 hwm_kb=108468 mallctl=yes alloc_allocated=103034448 alloc_active=134647808 alloc_resident=185077760 alloc_retained=15216640 alloc_mapped=200294400 compacted=true`
