# Ruby soak, 2026-09-21, x86_64

- commit: df6d285
- date: 2026-09-21T04:03:14Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 300 s, 16 threads + 0 mostly idle threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 150 s, one full GC after the workers stop (the final row)
- load average at start: 0.98 0.99 1.00; at end: 1.00 1.00 1.00; 2112 s total
- cgroup memory.peak not reset per phase, VmHWM only: the container's cgroup tree is read-only (Docker's default): pass CGROUP PEAK
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 2ccf9f46d160cd96), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped
  - decay0: as challoc, with MALLOC_CONF=dirty_decay_ms:0
  - decay1000: as challoc, with MALLOC_CONF=dirty_decay_ms:1000
  - narenas4: as challoc, with MALLOC_CONF=narenas:4
  - tc64: as challoc, with MALLOC_CONF=tcache_nslots_small_max:64
  - handoff: as challoc, with MALLOC_CONF=group_handoff:true

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | decay0 | decay1000 | narenas4 | tc64 | handoff | challoc vs jemalloc | decay0 vs challoc | decay1000 vs challoc | narenas4 vs challoc | tc64 vs challoc | handoff vs challoc | decay0 vs jemalloc | decay1000 vs jemalloc | narenas4 vs jemalloc | tc64 vs jemalloc | handoff vs jemalloc |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| peak sampled VmRSS | 357.9 | 118.2 | 335.7 | 344.1 | 167.2 | 165.1 | 354.8 | +202.8% | -6.2% | -3.9% | -53.3% | -53.9% | -0.9% | +184.0% | +191.1% | +41.5% | +39.7% | +200.2% |
| VmHWM at the end | 357.9 | 123.8 | 342.3 | 346.9 | 166.6 | 165.2 | 354.8 | +189.1% | -4.4% | -3.1% | -53.5% | -53.8% | -0.9% | +176.5% | +180.2% | +34.6% | +33.4% | +186.6% |
| cgroup memory.peak, start..compact | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| cgroup memory.peak, compact..end | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| at minute 1 (t=60) | 307.9 | 72.6 | 292.6 | 299.9 | 113.4 | 111.6 | 310.5 | +324.1% | -5.0% | -2.6% | -63.2% | -63.8% | +0.8% | +303.0% | +313.1% | +56.2% | +53.7% | +327.7% |
| at minute 5, before GC.compact (t=145) | 323.6 | 92.3 | 305.2 | 313.2 | 133.4 | 133.9 | 323.5 | +250.6% | -5.7% | -3.2% | -58.8% | -58.6% | -0.0% | +230.7% | +239.3% | +44.5% | +45.1% | +250.5% |
| at minute 5, after GC.compact (t=150) | 330.7 | 92.4 | 311.9 | 320.8 | 133.8 | 134.0 | 330.3 | +257.9% | -5.7% | -3.0% | -59.5% | -59.5% | -0.1% | +237.6% | +247.2% | +44.8% | +45.0% | +257.5% |
| at minute 10 (t=300) | 357.9 | 84.3 | 166.1 | 344.1 | 165.4 | 163.0 | 354.8 | +324.6% | -53.6% | -3.9% | -53.8% | -54.5% | -0.9% | +97.0% | +308.2% | +96.2% | +93.4% | +320.9% |
| after the last full GC (final) | 357.9 | 84.3 | 166.1 | 344.1 | 165.4 | 163.0 | 354.8 | +324.6% | -53.6% | -3.9% | -53.8% | -54.5% | -0.9% | +97.0% | +308.2% | +96.2% | +93.4% | +320.9% |
| lowest right after a periodic full GC | 301.7 | 72.6 | 283.0 | 293.1 | 111.4 | 109.7 | 299.9 | +315.6% | -6.2% | -2.9% | -63.1% | -63.6% | -0.6% | +289.8% | +303.7% | +53.4% | +51.1% | +313.1% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=150 | resident at t=300 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 67.6 | 166.4 | 386.4 | 34.1 | 420.5 | 355.1 | 386.4 | 1.08 |
| jemalloc | 57.3 | 68.7 | 82.8 | 78.1 | 112.9 | 96.5 | 82.8 | 0.98 |
| decay0 | 68.9 | 170.3 | 181.0 | 238.2 | 419.2 | 332.3 | 181.0 | 1.09 |
| decay1000 | 68.0 | 169.0 | 372.3 | 44.0 | 416.3 | 342.5 | 372.3 | 1.08 |
| narenas4 | 60.9 | 102.8 | 178.1 | 17.5 | 195.6 | 150.3 | 178.1 | 1.08 |
| tc64 | 59.5 | 108.3 | 189.1 | 28.1 | 217.2 | 156.2 | 189.1 | 1.16 |
| handoff | 68.1 | 168.0 | 381.7 | 33.7 | 415.4 | 353.5 | 381.7 | 1.08 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final | process CPU s | CPU ms per job | trimmed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 72644 | 36336 | 13390 | 1196 | 12194 | 200 | 297.18 | 4.09 | 0 |
| jemalloc | 73291 | 36662 | 13433 | 1199 | 12234 | 202 | 297.18 | 4.05 | 0 |
| decay0 | 73680 | 36854 | 13603 | 1245 | 12358 | 204 | 297.87 | 4.04 | 0 |
| decay1000 | 73642 | 36838 | 13614 | 1259 | 12355 | 204 | 297.29 | 4.04 | 0 |
| narenas4 | 74747 | 37384 | 13760 | 1286 | 12474 | 204 | 296.92 | 3.97 | 0 |
| tc64 | 74437 | 37230 | 13667 | 1259 | 12408 | 204 | 297.26 | 3.99 | 0 |
| handoff | 73700 | 36866 | 13522 | 1225 | 12297 | 202 | 297.19 | 4.03 | 0 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread). Process CPU is user+system of the whole process (soak.rb's Process.times at the end), so CPU per job is the cost column; trimmed counts the samples at which malloc_trim(0) purged something when TRIM=1.

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained | decay0 VmRSS | decay0 resident | decay0 retained | decay1000 VmRSS | decay1000 resident | decay1000 retained | narenas4 VmRSS | narenas4 resident | narenas4 retained | tc64 VmRSS | tc64 resident | tc64 retained | handoff VmRSS | handoff resident | handoff retained |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | start | 40.8 | 83.8 | 0.0 | 39.0 | 56.5 | 14.9 | 38.2 | 79.2 | 3.7 | 40.9 | 83.8 | 0.0 | 34.1 | 71.8 | 0.0 | 38.0 | 69.3 | 0.0 | 41.0 | 82.9 | 0.0 |
| 30 | fullgc | 301.7 | 330.5 | 7.7 | 72.9 | 79.9 | 31.8 | 283.0 | 300.1 | 36.1 | 293.1 | 320.2 | 16.8 | 111.4 | 134.4 | 6.1 | 109.7 | 134.7 | 6.4 | 299.9 | 327.7 | 7.8 |
| 60 | fullgc | 307.9 | 336.1 | 15.2 | 72.6 | 78.9 | 32.9 | 292.6 | 313.7 | 38.0 | 299.9 | 322.9 | 27.9 | 113.4 | 137.0 | 9.9 | 111.6 | 136.8 | 11.5 | 310.5 | 338.2 | 13.3 |
| 90 | fullgc | 309.3 | 333.0 | 24.6 | 73.0 | 78.8 | 36.0 | 295.5 | 317.8 | 39.8 | 301.1 | 323.6 | 33.7 | 112.7 | 132.3 | 17.6 | 111.1 | 133.7 | 18.8 | 312.5 | 336.1 | 22.1 |
| 120 | fullgc | 325.3 | 347.3 | 20.7 | 92.6 | 97.8 | 62.7 | 305.3 | 325.0 | 41.5 | 311.3 | 331.9 | 33.6 | 131.8 | 150.9 | 9.5 | 131.2 | 153.0 | 12.1 | 328.1 | 351.1 | 16.4 |
| 150 | compact | 330.7 | 355.1 | 26.8 | 92.4 | 96.5 | 64.1 | 311.9 | 332.3 | 48.3 | 320.8 | 342.5 | 36.9 | 133.8 | 150.3 | 16.3 | 134.0 | 156.2 | 19.4 | 330.3 | 353.5 | 25.8 |
| 180 | fullgc | 332.7 | 356.2 | 29.8 | 91.1 | 94.4 | 66.2 | 316.1 | 337.1 | 47.1 | 321.0 | 341.0 | 42.2 | 132.7 | 147.9 | 20.7 | 131.7 | 151.3 | 25.8 | 332.6 | 355.9 | 27.6 |
| 210 | fullgc | 330.8 | 359.6 | 37.3 | 91.8 | 94.5 | 66.1 | 315.5 | 342.6 | 52.4 | 320.8 | 348.1 | 45.3 | 131.3 | 148.0 | 23.7 | 130.3 | 157.3 | 29.1 | 331.3 | 359.0 | 34.3 |
| 240 | fullgc | 335.7 | 359.6 | 39.8 | 98.6 | 100.5 | 60.2 | 319.4 | 341.8 | 56.6 | 325.9 | 350.5 | 46.2 | 138.5 | 155.0 | 20.9 | 139.2 | 163.9 | 28.1 | 335.3 | 359.2 | 36.1 |
| 270 | fullgc | 332.5 | 357.8 | 45.4 | 95.8 | 97.1 | 63.6 | 319.5 | 341.7 | 59.5 | 326.9 | 349.7 | 51.1 | 141.3 | 155.4 | 24.0 | 138.8 | 161.0 | 35.1 | 333.8 | 357.2 | 41.8 |
| 300 | fullgc | 357.5 | 386.2 | 34.2 | 117.6 | 116.3 | 44.6 | 335.7 | 362.7 | 56.5 | 343.7 | 371.5 | 44.7 | 165.1 | 177.9 | 17.7 | 163.1 | 189.2 | 28.0 | 354.8 | 381.9 | 33.5 |
| 300 | final-fullgc | 357.9 | 386.4 | 34.1 | 84.3 | 82.8 | 78.1 | 166.1 | 181.0 | 238.2 | 344.1 | 372.3 | 44.0 | 165.4 | 178.1 | 17.5 | 163.0 | 189.1 | 28.1 | 354.8 | 381.7 | 33.7 |

## Samples

- challoc: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-challoc.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-challoc-mstats.txt`; summary line: `jobs=72644 live_objects=36336 gc_count=13390 major=1196 minor=12194 heap_pages=200 rss_final_kb=366476 hwm_kb=366476 mallctl=yes alloc_allocated=70923280 alloc_active=174575616 alloc_resident=405217280 alloc_retained=35680256 alloc_mapped=440897536 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=297.18 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- jemalloc: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-jemalloc-mstats.txt`; summary line: `jobs=73291 live_objects=36662 gc_count=13433 major=1199 minor=12234 heap_pages=202 rss_final_kb=86340 hwm_kb=126776 mallctl=yes alloc_allocated=60067136 alloc_active=72032256 alloc_resident=86773760 alloc_retained=81928192 alloc_mapped=118349824 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=297.18 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- decay0: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-decay0.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-decay0-mstats.txt`; summary line: `jobs=73680 live_objects=36854 gc_count=13603 major=1245 minor=12358 heap_pages=204 rss_final_kb=170132 hwm_kb=350524 mallctl=yes alloc_allocated=72269520 alloc_active=178593792 alloc_resident=189841408 alloc_retained=249749504 alloc_mapped=439590912 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=297.87 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- decay1000: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-decay1000.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-decay1000-mstats.txt`; summary line: `jobs=73642 live_objects=36838 gc_count=13614 major=1259 minor=12355 heap_pages=204 rss_final_kb=352396 hwm_kb=355248 mallctl=yes alloc_allocated=71314816 alloc_active=177246208 alloc_resident=390443008 alloc_retained=46059520 alloc_mapped=436502528 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=297.29 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- narenas4: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-narenas4.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-narenas4-mstats.txt`; summary line: `jobs=74747 live_objects=37384 gc_count=13760 major=1286 minor=12474 heap_pages=204 rss_final_kb=169392 hwm_kb=170644 mallctl=yes alloc_allocated=63878736 alloc_active=107888640 alloc_resident=186843136 alloc_retained=18259968 alloc_mapped=205103104 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=296.92 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- tc64: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-tc64.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-tc64-mstats.txt`; summary line: `jobs=74437 live_objects=37230 gc_count=13667 major=1259 minor=12408 heap_pages=204 rss_final_kb=166948 hwm_kb=169196 mallctl=yes alloc_allocated=62345136 alloc_active=113565696 alloc_resident=198316032 alloc_retained=29429760 alloc_mapped=227745792 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=297.26 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
- handoff: `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-handoff.csv` (62 rows), allocator report `bench/results/2026-09-21-x86_64-ruby-rss-attribution-controls-handoff-mstats.txt`; summary line: `jobs=73700 live_objects=36866 gc_count=13522 major=1225 minor=12297 heap_pages=202 rss_final_kb=363308 hwm_kb=363308 mallctl=yes alloc_allocated=71461456 alloc_active=176214016 alloc_resident=400310272 alloc_retained=35254272 alloc_mapped=435564544 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cpu_s=297.19 trim=no trimmed=0 cgroup_peak=no(the_container's_cgroup_tree_is_read-only_(Docker's_default):_pass_CGROUP_PEAK=1_to_make) cgroup_peak_before_compact_kb=- cgroup_peak_after_compact_kb=-`
