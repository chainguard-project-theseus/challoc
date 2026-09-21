# Ruby soak, 2026-09-21, aarch64

- commit: df6d285
- date: 2026-09-21T04:03:12Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- load average at start: 0.92 0.98 0.99; at end: 1.00 1.00 1.00; 2112 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 28d2ce5bde45aefe), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped
  - decay0: as challoc, with MALLOC_CONF=dirty_decay_ms:0
  - decay1000: as challoc, with MALLOC_CONF=dirty_decay_ms:1000
  - narenas4: as challoc, with MALLOC_CONF=narenas:4
  - tc64: as challoc, with MALLOC_CONF=tcache_nslots_small_max:64
  - handoff: as challoc, with MALLOC_CONF=group_handoff:true

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | decay0 | decay1000 | narenas4 | tc64 | handoff | challoc vs jemalloc | decay0 vs challoc | decay1000 vs challoc | narenas4 vs challoc | tc64 vs challoc | handoff vs challoc | decay0 vs jemalloc | decay1000 vs jemalloc | narenas4 vs jemalloc | tc64 vs jemalloc | handoff vs jemalloc |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| peak sampled VmRSS | 321.0 | 130.5 | 310.1 | 312.3 | 135.5 | 129.1 | 322.9 | +146.0% | -3.4% | -2.7% | -57.8% | -59.8% | +0.6% | +137.6% | +139.3% | +3.8% | -1.1% | +147.4% |
| VmHWM at the end | 321.0 | 131.6 | 309.9 | 312.7 | 135.5 | 129.1 | 322.9 | +143.9% | -3.5% | -2.6% | -57.8% | -59.8% | +0.6% | +135.5% | +137.6% | +3.0% | -1.9% | +145.4% |
| cgroup memory.peak, start..compact | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| cgroup memory.peak, compact..end | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| at minute 1 (t=60) | 286.7 | 95.6 | 274.1 | 280.6 | 94.1 | 90.3 | 287.3 | +199.9% | -4.4% | -2.1% | -67.2% | -68.5% | +0.2% | +186.7% | +193.5% | -1.6% | -5.5% | +200.5% |
| at minute 5, before GC.compact (t=145) | 294.9 | 109.7 | 280.2 | 281.5 | 107.1 | 102.5 | 291.1 | +168.8% | -5.0% | -4.5% | -63.7% | -65.2% | -1.3% | +155.4% | +156.6% | -2.4% | -6.6% | +165.4% |
| at minute 5, after GC.compact (t=150) | 302.6 | 111.1 | 289.0 | 291.8 | 108.2 | 103.5 | 299.8 | +172.4% | -4.5% | -3.6% | -64.2% | -65.8% | -0.9% | +160.1% | +162.6% | -2.6% | -6.8% | +169.8% |
| at minute 10 (t=300) | 321.0 | 109.1 | 147.1 | 312.3 | 135.5 | 129.1 | 322.9 | +194.2% | -54.2% | -2.7% | -57.8% | -59.8% | +0.6% | +34.8% | +186.3% | +24.2% | +18.3% | +196.0% |
| after the last full GC (final) | 321.0 | 109.1 | 147.1 | 312.3 | 135.5 | 129.1 | 322.9 | +194.2% | -54.2% | -2.7% | -57.8% | -59.8% | +0.6% | +34.8% | +186.3% | +24.2% | +18.3% | +196.0% |
| lowest right after a periodic full GC | 281.4 | 94.7 | 267.1 | 274.8 | 90.5 | 85.7 | 282.4 | +197.1% | -5.1% | -2.3% | -67.8% | -69.5% | +0.4% | +182.0% | +190.2% | -4.4% | -9.5% | +198.2% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 62.3 | 156.5 | 344.7 | 32.6 | 377.4 | 328.1 | 344.7 | 1.07 |
| jemalloc | 56.6 | 107.3 | 132.9 | 154.5 | 164.5 | 152.1 | 132.9 | 1.22 |
| decay0 | 60.4 | 154.3 | 167.6 | 209.6 | 377.2 | 312.1 | 167.6 | 1.14 |
| decay1000 | 59.7 | 150.4 | 334.8 | 41.2 | 376.0 | 315.9 | 334.8 | 1.07 |
| narenas4 | 57.3 | 94.9 | 149.8 | 12.3 | 162.1 | 126.0 | 149.8 | 1.11 |
| tc64 | 56.1 | 98.5 | 152.2 | 20.2 | 172.4 | 127.7 | 152.2 | 1.18 |
| handoff | 59.9 | 152.1 | 346.3 | 29.2 | 375.5 | 325.4 | 346.3 | 1.07 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 68710 | 34368 | 13857 | 26 | 13831 | 162 | 301.22 | 4.38 | 0 |
| jemalloc | 69199 | 34612 | 13868 | 33 | 13835 | 168 | 301.28 | 4.35 | 0 |
| decay0 | 68353 | 34190 | 13607 | 25 | 13582 | 151 | 301.26 | 4.41 | 0 |
| decay1000 | 68413 | 34220 | 13946 | 27 | 13919 | 151 | 301.25 | 4.40 | 0 |
| narenas4 | 69168 | 34590 | 13871 | 34 | 13837 | 162 | 301.25 | 4.36 | 0 |
| tc64 | 68687 | 34354 | 13893 | 26 | 13867 | 162 | 301.28 | 4.39 | 0 |
| handoff | 68647 | 34336 | 14041 | 40 | 14001 | 162 | 301.25 | 4.39 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained | decay0 VmRSS | decay0 resident | decay0 retained | decay1000 VmRSS | decay1000 resident | decay1000 retained | narenas4 VmRSS | narenas4 resident | narenas4 retained | tc64 VmRSS | tc64 resident | tc64 retained | handoff VmRSS | handoff resident | handoff retained |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | start | 40.9 | 82.9 | 0.0 | 41.6 | 108.4 | 27.9 | 38.5 | 78.1 | 4.8 | 40.9 | 83.0 | 0.0 | 34.6 | 69.9 | 0.0 | 37.6 | 68.2 | 0.0 | 40.9 | 82.9 | 0.0 |
| 30 | fullgc | 281.4 | 309.5 | 9.8 | 94.7 | 133.9 | 86.0 | 267.1 | 286.8 | 32.1 | 274.8 | 301.4 | 18.2 | 90.5 | 114.3 | 7.4 | 85.7 | 110.4 | 7.2 | 282.4 | 310.2 | 8.2 |
| 60 | fullgc | 286.7 | 313.1 | 13.4 | 95.6 | 131.8 | 91.6 | 274.1 | 294.3 | 31.6 | 280.6 | 305.3 | 21.5 | 94.1 | 117.4 | 8.7 | 90.3 | 114.3 | 7.7 | 287.3 | 313.2 | 12.8 |
| 90 | fullgc | 291.4 | 315.6 | 18.3 | 102.2 | 141.2 | 82.1 | 280.7 | 301.9 | 31.1 | 284.1 | 306.8 | 26.9 | 103.2 | 122.4 | 10.0 | 95.2 | 116.3 | 11.1 | 291.4 | 314.3 | 18.4 |
| 120 | fullgc | 296.2 | 320.3 | 21.2 | 101.3 | 137.3 | 86.1 | 285.5 | 307.3 | 33.3 | 287.7 | 308.4 | 31.3 | 106.0 | 124.4 | 12.2 | 99.4 | 122.1 | 11.3 | 295.2 | 318.9 | 20.6 |
| 150 | compact | 302.6 | 328.1 | 22.5 | 111.1 | 152.1 | 83.3 | 289.0 | 312.1 | 36.7 | 291.8 | 315.9 | 31.8 | 108.2 | 126.0 | 13.7 | 103.5 | 127.7 | 13.5 | 299.8 | 325.4 | 22.0 |
| 180 | fullgc | 307.6 | 330.4 | 23.9 | 122.1 | 156.8 | 106.6 | 294.4 | 315.0 | 38.4 | 298.0 | 319.9 | 32.4 | 118.2 | 134.3 | 11.7 | 108.5 | 130.1 | 15.8 | 304.0 | 325.9 | 25.5 |
| 210 | fullgc | 312.4 | 341.8 | 25.6 | 124.6 | 159.6 | 119.8 | 299.8 | 326.9 | 39.3 | 303.0 | 331.9 | 33.4 | 121.6 | 138.8 | 12.7 | 112.7 | 140.5 | 17.8 | 310.0 | 339.7 | 24.5 |
| 240 | fullgc | 312.6 | 339.4 | 30.9 | 126.7 | 163.0 | 124.4 | 305.2 | 328.3 | 42.7 | 309.5 | 333.6 | 36.3 | 124.6 | 142.6 | 11.8 | 116.7 | 142.3 | 19.5 | 313.9 | 340.8 | 27.7 |
| 270 | fullgc | 317.0 | 341.2 | 32.9 | 128.1 | 159.8 | 127.6 | 306.6 | 329.3 | 44.9 | 312.2 | 336.4 | 37.0 | 127.3 | 142.5 | 15.0 | 120.9 | 143.5 | 21.3 | 318.0 | 342.4 | 30.1 |
| 300 | fullgc | 320.0 | 344.1 | 33.2 | 129.1 | 160.4 | 126.9 | 310.1 | 332.9 | 44.3 | 311.8 | 334.9 | 41.0 | 134.9 | 149.8 | 12.3 | 127.9 | 150.9 | 20.6 | 321.7 | 345.7 | 29.7 |
| 300 | final-fullgc | 321.0 | 344.7 | 32.6 | 109.1 | 132.9 | 154.5 | 147.1 | 167.6 | 209.6 | 312.3 | 334.8 | 41.2 | 135.5 | 149.8 | 12.3 | 129.1 | 152.2 | 20.2 | 322.9 | 346.3 | 29.2 |

## Samples

- challoc: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-challoc.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-challoc-mstats.txt`; summary line: `jobs=68710 live_objects=34368 gc_count=13857 major=26 minor=13831 heap_pages=162 rss_final_kb=328732 hwm_kb=328732 mallctl=yes alloc_allocated=65410032 alloc_active=164278272 alloc_resident=361623552 alloc_retained=34058240 alloc_mapped=395681792 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.22 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-jemalloc-mstats.txt`; summary line: `jobs=69199 live_objects=34612 gc_count=13868 major=33 minor=13835 heap_pages=168 rss_final_kb=111744 hwm_kb=134804 mallctl=yes alloc_allocated=59400176 alloc_active=112525312 alloc_resident=139329536 alloc_retained=162004992 alloc_mapped=172490752 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.28 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- decay0: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-decay0.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-decay0-mstats.txt`; summary line: `jobs=68353 live_objects=34190 gc_count=13607 major=25 minor=13582 heap_pages=151 rss_final_kb=150596 hwm_kb=317300 mallctl=yes alloc_allocated=63347344 alloc_active=161873920 alloc_resident=175792128 alloc_retained=219742208 alloc_mapped=395534336 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.26 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- decay1000: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-decay1000.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-decay1000-mstats.txt`; summary line: `jobs=68413 live_objects=34220 gc_count=13946 major=27 minor=13919 heap_pages=151 rss_final_kb=319808 hwm_kb=320220 mallctl=yes alloc_allocated=62656720 alloc_active=157786112 alloc_resident=351141888 alloc_retained=43098112 alloc_mapped=394240000 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.25 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- narenas4: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-narenas4.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-narenas4-mstats.txt`; summary line: `jobs=69168 live_objects=34590 gc_count=13871 major=34 minor=13837 heap_pages=162 rss_final_kb=138708 hwm_kb=138708 mallctl=yes alloc_allocated=60128176 alloc_active=99590144 alloc_resident=157192192 alloc_retained=12812288 alloc_mapped=170004480 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.25 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- tc64: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-tc64.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-tc64-mstats.txt`; summary line: `jobs=68687 live_objects=34354 gc_count=13893 major=26 minor=13867 heap_pages=162 rss_final_kb=132184 hwm_kb=132184 mallctl=yes alloc_allocated=58788048 alloc_active=103313408 alloc_resident=159617024 alloc_retained=21176320 alloc_mapped=180793344 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.28 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- handoff: `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-handoff.csv` (62 rows), allocator report `bench/results/2026-09-21-aarch64-ruby-rss-attribution-controls-handoff-mstats.txt`; summary line: `jobs=68647 live_objects=34336 gc_count=14041 major=40 minor=14001 heap_pages=162 rss_final_kb=330656 hwm_kb=330656 mallctl=yes alloc_allocated=62815248 alloc_active=159551488 alloc_resident=363220992 alloc_retained=30523392 alloc_mapped=393744384 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=301.25 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
