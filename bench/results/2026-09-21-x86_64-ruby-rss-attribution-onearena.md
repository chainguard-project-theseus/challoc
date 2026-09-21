# Ruby soak, 2026-09-21, x86_64

- commit: df6d285
- date: 2026-09-21T04:38:28Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- load average at start: 1.00 1.00 1.00; at end: 0.98 0.99 1.00; 1510 s total
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
| peak sampled VmRSS | 111.4 | 111.9 | 99.4 | 108.2 | 104.9 |
| VmHWM at the end | 112.9 | 113.7 | 110.7 | 107.1 | 106.3 |
| cgroup memory.peak, start..compact | - | - | - | - | - |
| cgroup memory.peak, compact..end | - | - | - | - | - |
| at minute 1 (t=60) | 60.6 | 58.8 | 51.2 | 51.8 | 51.2 |
| at minute 5, before GC.compact (t=145) | 80.4 | 78.4 | 70.3 | 71.7 | 71.2 |
| at minute 5, after GC.compact (t=150) | 80.4 | 78.9 | 68.8 | 71.5 | 71.3 |
| at minute 10 (t=300) | 111.4 | 109.2 | 77.1 | 105.1 | 104.9 |
| after the last full GC (final) | 111.4 | 109.2 | 77.1 | 105.1 | 104.9 |
| lowest right after a periodic full GC | 59.0 | 58.1 | 48.8 | 49.9 | 49.7 |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| n1 | 58.7 | 81.2 | 122.7 | 15.5 | 138.2 | 95.9 | 122.7 | 1.10 |
| n1decay1000 | 58.7 | 81.5 | 120.9 | 18.4 | 139.3 | 93.9 | 120.9 | 1.11 |
| n1decay0 | 58.3 | 81.2 | 84.6 | 53.5 | 138.1 | 82.9 | 84.6 | 1.10 |
| n1tc64 | 57.6 | 77.7 | 115.7 | 11.8 | 127.5 | 86.2 | 115.7 | 1.10 |
| n1tc8 | 57.7 | 77.7 | 116.0 | 11.5 | 127.5 | 86.3 | 116.0 | 1.11 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| n1 | 75398 | 37710 | 13694 | 1259 | 12435 | 202 | 296.88 | 3.94 | 0 |
| n1decay1000 | 75380 | 37700 | 13727 | 1277 | 12450 | 204 | 296.66 | 3.94 | 0 |
| n1decay0 | 74278 | 37150 | 13480 | 1193 | 12287 | 204 | 297.02 | 4.00 | 0 |
| n1tc64 | 74539 | 37282 | 13536 | 1220 | 12316 | 204 | 297.13 | 3.99 | 0 |
| n1tc8 | 74991 | 37504 | 13557 | 1224 | 12333 | 204 | 297.02 | 3.96 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | n1 VmRSS | n1 resident | n1 retained | n1decay1000 VmRSS | n1decay1000 resident | n1decay1000 retained | n1decay0 VmRSS | n1decay0 resident | n1decay0 retained | n1tc64 VmRSS | n1tc64 resident | n1tc64 retained | n1tc8 VmRSS | n1tc8 resident | n1tc8 retained |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | start | 28.8 | 57.6 | 0.0 | 28.6 | 57.7 | 0.0 | 27.4 | 54.8 | 3.3 | 24.1 | 48.1 | 0.0 | 24.0 | 47.9 | 0.0 |
| 30 | fullgc | 61.2 | 83.5 | 4.7 | 58.7 | 78.7 | 9.8 | 48.8 | 69.8 | 18.8 | 52.6 | 71.8 | 4.5 | 52.6 | 71.8 | 4.8 |
| 60 | fullgc | 60.6 | 82.7 | 8.6 | 58.8 | 78.6 | 12.9 | 51.2 | 67.3 | 24.2 | 51.8 | 72.4 | 5.8 | 51.2 | 72.1 | 6.5 |
| 90 | fullgc | 59.0 | 77.4 | 16.4 | 58.1 | 75.5 | 18.6 | 54.4 | 71.0 | 23.2 | 49.9 | 66.6 | 14.0 | 49.7 | 66.7 | 14.2 |
| 120 | fullgc | 80.6 | 98.7 | 8.8 | 77.8 | 91.4 | 16.7 | 65.5 | 81.2 | 26.7 | 72.5 | 89.4 | 5.6 | 72.3 | 89.4 | 5.2 |
| 150 | compact | 80.4 | 95.9 | 14.6 | 78.9 | 93.9 | 16.8 | 68.8 | 82.9 | 28.0 | 71.5 | 86.2 | 11.4 | 71.3 | 86.3 | 11.0 |
| 180 | fullgc | 79.3 | 93.9 | 18.3 | 77.5 | 91.3 | 21.2 | 70.9 | 83.5 | 29.1 | 70.6 | 84.4 | 15.0 | 69.8 | 83.9 | 15.1 |
| 210 | fullgc | 78.5 | 93.3 | 21.8 | 76.6 | 91.2 | 24.1 | 71.0 | 85.8 | 29.2 | 69.3 | 83.8 | 18.3 | 69.0 | 83.4 | 18.4 |
| 240 | fullgc | 85.2 | 100.0 | 19.2 | 84.6 | 97.6 | 22.1 | 78.7 | 92.0 | 26.7 | 76.3 | 90.5 | 16.1 | 76.5 | 90.9 | 15.3 |
| 270 | fullgc | 92.2 | 104.9 | 19.0 | 86.1 | 98.7 | 23.6 | 82.4 | 92.9 | 30.8 | 80.8 | 92.7 | 17.8 | 80.0 | 92.2 | 17.8 |
| 300 | fullgc | 111.2 | 122.7 | 15.5 | 109.7 | 121.4 | 17.9 | 88.7 | 111.5 | 26.6 | 104.9 | 115.8 | 11.7 | 104.8 | 116.0 | 11.5 |
| 300 | final-fullgc | 111.4 | 122.7 | 15.5 | 109.2 | 120.9 | 18.4 | 77.1 | 84.6 | 53.5 | 105.1 | 115.7 | 11.8 | 104.9 | 116.0 | 11.5 |

## Samples

- n1: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-n1.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-n1-mstats.txt`; summary line: `jobs=75398 live_objects=37710 gc_count=13694 major=1259 minor=12435 heap_pages=202 rss_final_kb=114100 hwm_kb=115608 mallctl=yes alloc_allocated=61562656 alloc_active=85196800 alloc_resident=128761856 alloc_retained=16142336 alloc_mapped=144904192 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=296.88 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- n1decay1000: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-n1decay1000.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-n1decay1000-mstats.txt`; summary line: `jobs=75380 live_objects=37700 gc_count=13727 major=1277 minor=12450 heap_pages=204 rss_final_kb=111808 hwm_kb=116444 mallctl=yes alloc_allocated=61650736 alloc_active=85549056 alloc_resident=126824448 alloc_retained=19243008 alloc_mapped=146067456 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=296.66 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- n1decay0: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-n1decay0.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-n1decay0-mstats.txt`; summary line: `jobs=74278 live_objects=37150 gc_count=13480 major=1193 minor=12287 heap_pages=204 rss_final_kb=78988 hwm_kb=113360 mallctl=yes alloc_allocated=61163440 alloc_active=85221376 alloc_resident=88813568 alloc_retained=55996416 alloc_mapped=144809984 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=297.02 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- n1tc64: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-n1tc64.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-n1tc64-mstats.txt`; summary line: `jobs=74539 live_objects=37282 gc_count=13536 major=1220 minor=12316 heap_pages=204 rss_final_kb=107572 hwm_kb=109660 mallctl=yes alloc_allocated=60360624 alloc_active=81457152 alloc_resident=121307136 alloc_retained=12337152 alloc_mapped=133644288 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=297.13 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- n1tc8: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-n1tc8.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-onearena-n1tc8-mstats.txt`; summary line: `jobs=74991 live_objects=37504 gc_count=13557 major=1224 minor=12333 heap_pages=204 rss_final_kb=107428 hwm_kb=108856 mallctl=yes alloc_allocated=60536960 alloc_active=81477632 alloc_resident=121679872 alloc_retained=12042240 alloc_mapped=133722112 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=297.02 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
