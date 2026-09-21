# Ruby soak, 2026-09-21, aarch64

- commit: df6d285
- date: 2026-09-21T04:38:26Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- load average at start: 1.00 1.00 1.00; at end: 1.00 1.00 1.00; 1511 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - n1: as challoc, with MALLOC_CONF=narenas:1
  - n1decay1000: as challoc, with MALLOC_CONF=narenas:1,dirty_decay_ms:1000
  - n1decay0: as challoc, with MALLOC_CONF=narenas:1,dirty_decay_ms:0
  - n1tc64: as challoc, with MALLOC_CONF=narenas:1,tcache_nslots_small_max:64
  - n1tc8: as challoc, with MALLOC_CONF=narenas:1,tcache_nslots_small_max:8

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | n1 | n1decay1000 | n1decay0 | n1tc64 | n1tc8 |
|---|---:|---:|---:|---:|---:|
| peak sampled VmRSS | 87.8 | 86.9 | 73.3 | 76.4 | 79.7 |
| VmHWM at the end | 87.8 | 86.9 | 80.0 | 76.4 | 79.7 |
| cgroup memory.peak, start..compact | - | - | - | - | - |
| cgroup memory.peak, compact..end | - | - | - | - | - |
| at minute 1 (t=60) | 41.8 | 39.6 | 37.9 | 31.4 | 31.4 |
| at minute 5, before GC.compact (t=145) | 53.8 | 53.0 | 50.9 | 44.4 | 44.4 |
| at minute 5, after GC.compact (t=150) | 54.8 | 54.5 | 51.2 | 45.0 | 45.1 |
| at minute 10 (t=300) | 87.8 | 86.9 | 68.1 | 76.4 | 79.7 |
| after the last full GC (final) | 87.8 | 86.9 | 68.1 | 76.4 | 79.7 |
| lowest right after a periodic full GC | 37.7 | 36.3 | 34.9 | 28.3 | 28.3 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| n1 | 55.2 | 74.1 | 100.1 | 10.5 | 110.6 | 71.8 | 100.1 | 1.14 |
| n1decay1000 | 55.2 | 74.2 | 98.4 | 12.1 | 110.5 | 71.6 | 98.4 | 1.13 |
| n1decay0 | 54.8 | 72.8 | 79.4 | 27.9 | 107.3 | 68.0 | 79.4 | 1.17 |
| n1tc64 | 54.3 | 69.5 | 88.5 | 7.1 | 95.7 | 61.1 | 88.5 | 1.16 |
| n1tc8 | 54.3 | 69.8 | 91.8 | 7.1 | 98.9 | 61.2 | 91.8 | 1.15 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| n1 | 69295 | 34660 | 13931 | 30 | 13901 | 168 | 301.18 | 4.35 | 0 |
| n1decay1000 | 69257 | 34642 | 13749 | 28 | 13721 | 168 | 301.2 | 4.35 | 0 |
| n1decay0 | 68506 | 34264 | 13993 | 37 | 13956 | 162 | 301.23 | 4.40 | 0 |
| n1tc64 | 68993 | 34506 | 13993 | 40 | 13953 | 162 | 301.17 | 4.37 | 0 |
| n1tc8 | 69105 | 34566 | 13929 | 28 | 13901 | 168 | 301.21 | 4.36 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | n1 VmRSS | n1 resident | n1 retained | n1decay1000 VmRSS | n1decay1000 resident | n1decay1000 retained | n1decay0 VmRSS | n1decay0 resident | n1decay0 retained | n1tc64 VmRSS | n1tc64 resident | n1tc64 retained | n1tc8 VmRSS | n1tc8 resident | n1tc8 retained |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | start | 28.5 | 54.9 | 0.0 | 28.6 | 55.0 | 0.0 | 27.8 | 50.4 | 4.6 | 23.7 | 45.3 | 0.0 | 23.6 | 45.2 | 0.0 |
| 30 | fullgc | 37.7 | 61.0 | 5.9 | 36.3 | 59.4 | 7.8 | 34.9 | 57.9 | 9.1 | 28.3 | 48.0 | 5.7 | 28.3 | 48.0 | 5.7 |
| 60 | fullgc | 41.8 | 63.9 | 6.6 | 39.6 | 59.3 | 11.2 | 37.9 | 55.1 | 15.0 | 31.4 | 52.4 | 4.3 | 31.4 | 52.4 | 4.5 |
| 90 | fullgc | 49.5 | 67.3 | 8.9 | 48.7 | 66.8 | 9.9 | 45.2 | 62.6 | 13.4 | 40.2 | 56.9 | 5.8 | 40.1 | 56.9 | 6.0 |
| 120 | fullgc | 51.6 | 68.7 | 10.8 | 50.6 | 66.8 | 13.4 | 48.2 | 64.5 | 15.3 | 42.1 | 58.4 | 7.7 | 42.1 | 58.4 | 7.7 |
| 150 | compact | 54.8 | 71.8 | 11.7 | 54.5 | 71.6 | 12.3 | 51.2 | 68.0 | 15.9 | 45.0 | 61.1 | 8.8 | 45.1 | 61.2 | 8.7 |
| 180 | fullgc | 65.5 | 80.7 | 10.5 | 64.1 | 79.3 | 11.3 | 58.7 | 75.6 | 15.3 | 55.9 | 70.5 | 6.4 | 56.0 | 70.6 | 6.7 |
| 210 | fullgc | 67.7 | 83.5 | 11.6 | 65.8 | 81.8 | 13.1 | 62.5 | 77.7 | 17.8 | 58.6 | 73.9 | 7.5 | 58.5 | 73.7 | 7.9 |
| 240 | fullgc | 70.3 | 86.9 | 11.5 | 69.6 | 83.3 | 14.7 | 66.8 | 80.4 | 18.0 | 61.1 | 76.9 | 7.5 | 61.3 | 77.1 | 7.7 |
| 270 | fullgc | 73.0 | 87.1 | 14.1 | 71.1 | 84.0 | 17.0 | 69.4 | 82.7 | 18.8 | 63.7 | 77.0 | 10.4 | 63.7 | 77.0 | 10.6 |
| 300 | fullgc | 87.6 | 100.1 | 10.5 | 86.7 | 98.5 | 12.0 | 73.3 | 85.3 | 22.0 | 76.2 | 88.5 | 7.1 | 76.2 | 88.7 | 7.1 |
| 300 | final-fullgc | 87.8 | 100.1 | 10.5 | 86.9 | 98.4 | 12.1 | 68.1 | 79.4 | 27.9 | 76.4 | 88.5 | 7.1 | 79.7 | 91.8 | 7.1 |

## Samples

- n1: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-n1.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-n1-mstats.txt`; summary line: `jobs=69295 live_objects=34660 gc_count=13931 major=30 minor=13901 heap_pages=168 rss_final_kb=89868 hwm_kb=89868 mallctl=yes alloc_allocated=57924336 alloc_active=77721600 alloc_resident=105021440 alloc_retained=10924032 alloc_mapped=115945472 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.18 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- n1decay1000: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-n1decay1000.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-n1decay1000-mstats.txt`; summary line: `jobs=69257 live_objects=34642 gc_count=13749 major=28 minor=13721 heap_pages=168 rss_final_kb=89012 hwm_kb=89012 mallctl=yes alloc_allocated=57976464 alloc_active=77864960 alloc_resident=103247872 alloc_retained=12652544 alloc_mapped=115900416 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.2 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- n1decay0: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-n1decay0.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-n1decay0-mstats.txt`; summary line: `jobs=68506 live_objects=34264 gc_count=13993 major=37 minor=13956 heap_pages=162 rss_final_kb=69732 hwm_kb=81940 mallctl=yes alloc_allocated=57492992 alloc_active=76378112 alloc_resident=83300352 alloc_retained=29192192 alloc_mapped=112492544 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.23 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- n1tc64: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-n1tc64.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-n1tc64-mstats.txt`; summary line: `jobs=68993 live_objects=34506 gc_count=13993 major=40 minor=13953 heap_pages=162 rss_final_kb=78204 hwm_kb=78204 mallctl=yes alloc_allocated=56912448 alloc_active=72904704 alloc_resident=92827648 alloc_retained=7479296 alloc_mapped=100306944 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.17 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- n1tc8: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-n1tc8.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-onearena-n1tc8-mstats.txt`; summary line: `jobs=69105 live_objects=34566 gc_count=13929 major=28 minor=13901 heap_pages=168 rss_final_kb=81628 hwm_kb=81628 mallctl=yes alloc_allocated=56956288 alloc_active=73175040 alloc_resident=96260096 alloc_retained=7413760 alloc_mapped=103673856 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.21 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
