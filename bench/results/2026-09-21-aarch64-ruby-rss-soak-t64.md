# Ruby soak, 2026-09-21, aarch64

- commit: 6b29fa9
- date: 2026-09-21T05:59:46Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 64 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- load average at start: 3.12 5.09 4.16; at end: 1.00 1.52 2.61; 615 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 6287c62a3cb3cc32), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 220.2 | 313.8 | -29.8% |
| VmHWM at the end | 220.6 | 315.8 | -30.1% |
| cgroup memory.peak, start..compact | - | - | - |
| cgroup memory.peak, compact..end | - | - | - |
| at minute 1 (t=60) | 186.5 | 268.4 | -30.5% |
| at minute 5, before GC.compact (t=145) | 199.7 | 291.2 | -31.4% |
| at minute 5, after GC.compact (t=150) | 200.0 | 294.0 | -32.0% |
| at minute 10 (t=300) | 219.9 | 296.2 | -25.8% |
| after the last full GC (final) | 130.5 | 214.7 | -39.2% |
| lowest right after a periodic full GC | 186.1 | 263.3 | -29.3% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 104.4 | 168.2 | 201.1 | 163.9 | 365.0 | 277.3 | 300.4 | 1.54 |
| jemalloc | 105.8 | 253.7 | 328.8 | 493.1 | 444.9 | 448.6 | 440.0 | 1.53 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 69198 | 34662 | 13950 | 27 | 13923 | 162 | 303.79 | 4.39 | 0 |
| jemalloc | 69231 | 34678 | 13971 | 29 | 13942 | 162 | 303.82 | 4.39 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 151 | compact | 200.0 | 277.2 | 60.8 | 294.0 | 449.5 | 365.4 |
| 303 | final-fullgc | 130.5 | 201.1 | 163.9 | 214.7 | 328.8 | 493.1 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-ruby-rss-soak-t64-challoc.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-soak-t64-challoc-mstats.txt`; summary line: `jobs=69198 live_objects=34662 gc_count=13950 major=27 minor=13923 heap_pages=162 rss_final_kb=133640 hwm_kb=225844 mallctl=yes alloc_allocated=109534496 alloc_active=176504832 alloc_resident=210919424 alloc_retained=171778048 alloc_mapped=382697472 compacted=true threads=64 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=303.79 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-aarch64-ruby-rss-soak-t64-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-soak-t64-jemalloc-mstats.txt`; summary line: `jobs=69231 live_objects=34678 gc_count=13971 major=29 minor=13942 heap_pages=162 rss_final_kb=219856 hwm_kb=323416 mallctl=yes alloc_allocated=110921104 alloc_active=266010624 alloc_resident=344784896 alloc_retained=517013504 alloc_mapped=466550784 compacted=true threads=64 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=303.82 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
