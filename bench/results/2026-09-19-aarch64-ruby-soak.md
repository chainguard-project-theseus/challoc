# Ruby soak, 2026-09-19, aarch64

- commit: a3ddd6d
- date: 2026-09-19T18:39:23Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: yes
- workload: bench/ruby/soak.rb 600 s, 16 threads, seed 4141, sample every 5 s, full GC every 30 s, GC.compact at 300 s, one full GC after the workers stop (the final row)
- load average at start: 0.60 3.74 4.77; at end: 1.00 1.00 1.25; 2408 s total
- allocators (run in this order):
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 4f6fe7d01645c6d9), default MALLOC_CONF
  - jemalloc: the same ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped
  - challoc-bg: as challoc, with MALLOC_CONF=background_thread:true
  - challoc-1arena: as challoc, with MALLOC_CONF=narenas:1 (one arena for all threads)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc-bg | challoc-1arena | challoc vs jemalloc | challoc-bg vs challoc | challoc-1arena vs challoc |
|---|---:|---:|---:|---:|---:|---:|---:|
| peak sampled VmRSS | 401.4 | 200.8 | 398.5 | 170.9 | +99.9% | -0.7% | -57.4% |
| VmHWM at the end | 401.7 | 203.2 | 399.3 | 170.3 | +97.7% | -0.6% | -57.6% |
| at minute 1 (t=60) | 286.0 | 99.7 | 285.5 | 43.6 | +186.9% | -0.2% | -84.8% |
| at minute 5, before GC.compact (t=295) | 314.4 | 130.5 | 314.9 | 78.1 | +140.9% | +0.2% | -75.2% |
| at minute 5, after GC.compact (t=300) | 321.5 | 130.0 | 320.7 | 78.7 | +147.3% | -0.2% | -75.5% |
| at minute 10 (t=600) | 393.5 | 160.3 | 393.6 | 167.3 | +145.5% | +0.0% | -57.5% |
| after the last full GC (final) | 393.5 | 160.3 | 393.6 | 167.3 | +145.5% | +0.0% | -57.5% |
| lowest right after a periodic full GC | 279.0 | 93.0 | 279.8 | 35.9 | +200.0% | +0.3% | -87.1% |

Positive percentages: the first-named allocator is larger.

## Allocator stats (MiB, mallctl stats.* after an epoch bump)

| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=300 | resident at t=600 | resident / VmRSS final |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 98.7 | 206.1 | 397.9 | 71.7 | 469.6 | 345.5 | 397.9 | 1.01 |
| jemalloc | 90.2 | 138.8 | 167.6 | 121.0 | 198.0 | 160.8 | 167.6 | 1.05 |
| challoc-bg | 99.3 | 206.5 | 398.3 | 69.3 | 467.6 | 345.2 | 398.3 | 1.01 |
| challoc-1arena | 88.6 | 112.8 | 167.5 | 22.1 | 189.6 | 92.6 | 167.5 | 1.00 |

## Work done and GC

| allocator | jobs | live objects | GC count | major | minor | heap pages final |
|---|---:|---:|---:|---:|---:|---:|
| challoc | 137537 | 68784 | 16859 | 147 | 16712 | 323 |
| jemalloc | 138560 | 69294 | 16969 | 143 | 16826 | 327 |
| challoc-bg | 137993 | 69012 | 16981 | 146 | 16835 | 326 |
| challoc-1arena | 138326 | 69176 | 16980 | 145 | 16835 | 325 |

The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread).

## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained | challoc-bg VmRSS | challoc-bg resident | challoc-bg retained | challoc-1arena VmRSS | challoc-1arena resident | challoc-1arena retained |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | start | 40.5 | 82.2 | 0.0 | 36.6 | 105.0 | 25.9 | 40.5 | 82.5 | 0.0 | 24.5 | 50.2 | 0.0 |
| 30 | fullgc | 279.0 | 307.8 | 10.6 | 93.0 | 131.9 | 88.4 | 279.8 | 308.1 | 10.6 | 35.9 | 59.6 | 5.7 |
| 60 | fullgc | 286.0 | 313.2 | 12.0 | 99.7 | 136.0 | 87.9 | 285.5 | 312.1 | 13.4 | 43.6 | 65.6 | 5.4 |
| 90 | fullgc | 289.9 | 313.4 | 19.5 | 104.4 | 139.4 | 84.4 | 290.8 | 314.0 | 20.3 | 46.3 | 64.7 | 10.2 |
| 120 | fullgc | 294.7 | 319.4 | 21.0 | 104.4 | 138.6 | 85.3 | 294.5 | 319.4 | 21.8 | 49.9 | 68.0 | 10.9 |
| 150 | fullgc | 302.7 | 328.0 | 20.9 | 116.3 | 153.6 | 82.3 | 299.3 | 325.0 | 24.2 | 58.8 | 76.1 | 9.7 |
| 180 | fullgc | 306.6 | 329.0 | 24.4 | 122.0 | 156.4 | 87.5 | 305.2 | 327.9 | 26.4 | 61.7 | 77.7 | 11.3 |
| 210 | fullgc | 310.7 | 340.7 | 24.7 | 124.0 | 159.1 | 96.8 | 310.8 | 341.0 | 26.6 | 64.1 | 80.9 | 12.6 |
| 240 | fullgc | 310.8 | 338.4 | 30.8 | 129.8 | 163.7 | 104.2 | 313.6 | 341.1 | 30.1 | 73.9 | 90.7 | 9.0 |
| 270 | fullgc | 317.0 | 341.3 | 34.0 | 132.8 | 163.5 | 120.4 | 319.1 | 343.9 | 32.6 | 77.6 | 91.7 | 12.0 |
| 300 | compact | 321.5 | 345.5 | 34.4 | 130.0 | 160.8 | 123.1 | 320.7 | 345.2 | 34.8 | 78.7 | 92.6 | 13.2 |
| 330 | fullgc | 356.0 | 383.7 | 29.0 | 151.1 | 175.5 | 112.4 | 352.5 | 381.7 | 30.6 | 111.0 | 121.3 | 22.6 |
| 360 | fullgc | 388.5 | 407.8 | 36.3 | 152.9 | 176.8 | 111.2 | 361.4 | 379.3 | 55.5 | 114.3 | 123.5 | 37.1 |
| 390 | fullgc | 361.3 | 378.8 | 67.9 | 167.4 | 192.2 | 95.7 | 356.5 | 374.4 | 63.2 | 142.6 | 151.3 | 14.5 |
| 420 | fullgc | 374.0 | 391.6 | 58.1 | 176.4 | 202.2 | 85.7 | 376.3 | 393.6 | 51.0 | 147.2 | 155.3 | 14.6 |
| 450 | fullgc | 379.9 | 396.3 | 56.7 | 177.4 | 198.8 | 89.2 | 380.4 | 396.8 | 52.4 | 138.5 | 145.3 | 27.5 |
| 480 | fullgc | 381.8 | 397.9 | 58.1 | 183.5 | 197.1 | 91.0 | 380.6 | 396.3 | 55.8 | 143.9 | 148.8 | 27.4 |
| 510 | fullgc | 382.2 | 390.1 | 70.1 | 187.0 | 200.5 | 87.8 | 386.7 | 394.7 | 62.0 | 153.7 | 157.6 | 22.0 |
| 540 | fullgc | 390.2 | 396.5 | 66.8 | 188.7 | 202.8 | 85.6 | 391.0 | 397.0 | 64.5 | 163.8 | 166.0 | 16.8 |
| 570 | fullgc | 384.0 | 387.7 | 78.8 | 197.5 | 214.1 | 74.4 | 394.2 | 399.1 | 66.2 | 150.1 | 151.4 | 34.7 |
| 600 | fullgc | 393.5 | 397.9 | 71.7 | 160.3 | 167.6 | 121.0 | 393.6 | 398.3 | 69.3 | 167.3 | 167.5 | 22.1 |
| 600 | final-fullgc | 393.5 | 397.9 | 71.7 | 160.3 | 167.6 | 121.0 | 393.6 | 398.3 | 69.3 | 167.3 | 167.5 | 22.1 |

## Samples

- challoc: `.tmp-out/ruby-soak/2026-09-19-aarch64-ruby-soak-challoc.csv` (122 rows), allocator report `.tmp-out/ruby-soak/2026-09-19-aarch64-ruby-soak-challoc-mstats.txt`; summary line: `jobs=137537 live_objects=68784 gc_count=16859 major=147 minor=16712 heap_pages=323 rss_final_kb=402904 hwm_kb=411388 mallctl=yes alloc_allocated=103567088 alloc_active=216166400 alloc_resident=417345536 alloc_retained=75087872 alloc_mapped=492433408 compacted=true`
- jemalloc: `.tmp-out/ruby-soak/2026-09-19-aarch64-ruby-soak-jemalloc.csv` (122 rows), allocator report `.tmp-out/ruby-soak/2026-09-19-aarch64-ruby-soak-jemalloc-mstats.txt`; summary line: `jobs=138560 live_objects=69294 gc_count=16969 major=143 minor=16826 heap_pages=327 rss_final_kb=164128 hwm_kb=208112 mallctl=yes alloc_allocated=94615000 alloc_active=145555456 alloc_resident=175702016 alloc_retained=126877696 alloc_mapped=207618048 compacted=true`
- challoc-bg: `.tmp-out/ruby-soak/2026-09-19-aarch64-ruby-soak-challoc-bg.csv` (122 rows), allocator report `.tmp-out/ruby-soak/2026-09-19-aarch64-ruby-soak-challoc-bg-mstats.txt`; summary line: `jobs=137993 live_objects=69012 gc_count=16981 major=146 minor=16835 heap_pages=326 rss_final_kb=403020 hwm_kb=408900 mallctl=yes alloc_allocated=104224976 alloc_active=216612864 alloc_resident=417755136 alloc_retained=72564736 alloc_mapped=490319872 compacted=true`
- challoc-1arena: `.tmp-out/ruby-soak/2026-09-19-aarch64-ruby-soak-challoc-1arena.csv` (122 rows), allocator report `.tmp-out/ruby-soak/2026-09-19-aarch64-ruby-soak-challoc-1arena-mstats.txt`; summary line: `jobs=138326 live_objects=69176 gc_count=16980 major=145 minor=16835 heap_pages=325 rss_final_kb=171332 hwm_kb=174396 mallctl=yes alloc_allocated=92989584 alloc_active=118325248 alloc_resident=175734784 alloc_retained=23117824 alloc_mapped=198852608 compacted=true`

## Reading (lane D, 2026-09-19)

Host: `challoc-lane-d-arm64`, c4a-standard-16 (Neoverse V2), Ubuntu 24.04
kernel 7.0.0-1011-gcp, the dev image, 18:39..19:20 UTC, nothing else on
the VM after the io-threads soak ended. Library a3ddd6d, `quantum16`,
`jemalloc-soname`. The x86_64 file (`2026-09-19-x86_64-ruby-soak.md`) has
the same three variants; this run adds `challoc-1arena`
(`MALLOC_CONF=narenas:1`), the diagnostic the x86_64 report asked for.
Reports: `2026-09-19-aarch64-ruby-soak-{challoc,challoc-bg,challoc-1arena}-mstats.txt`
(whole), `-jemalloc-mstats.txt` (general and merged sections; per-arena
dropped, 343 KB). jemalloc here is Wolfi's aarch64 build with 64 KiB
pages (`Page size: 65536` in its report; the kernel page is 4 KiB), the
build the 2026-09-17 Ruby rows noted; its active/allocated is 1.54 on
this VM against 1.13 on x86_64.

1. Default challoc against jemalloc: the x86_64 picture again. 279 vs 93
   MiB at the first forced full GC, 321 vs 130 after `GC.compact`, 393 vs
   160 at t=600 (+145%), peak 401 vs 201 (+100%). Final allocator stats:
   allocated 98.7 vs 90.2 MiB, active 206 vs 139 (active/allocated 2.09 vs
   1.54), resident 398 vs 168 (resident/active 1.93 vs 1.21). `Arenas: 17
   (64 automatic)`: one per thread. Per class, the long-lived classes
   1280..2048 B at 47..50% occupancy in 64 KiB slabs (11.6 MiB live in
   24.1 MiB of slabs, 13.7 in 29.0, 16.3 in 34.5, 18.7 in 37.1); the burst
   classes 2560..4096 B at 18..27% in 89..101 slabs each (1.0..1.7 MiB live
   in 5.6..6.3 MiB of slabs); compaction never engaged (`c_on 0`, `gated`
   28..1155 per class).
2. `background_thread:true` (challoc-bg): -0.7..+0.3% against the default
   on every row, allocator stats within 0.5 MiB (resident 398.3 vs 397.9).
   Same as on x86_64: no effect at all. The decay is already driven by the
   slow-path ticks; the thread has nothing to purge that the ticks do not.
3. `narenas:1` (challoc-1arena): 35.9 MiB at the first forced full GC
   (jemalloc 93.0), 78.7 after `GC.compact` (130.0), 167.3 at t=600
   (160.3, +4.4%), peak 170.9 (200.8, -15%); VmHWM 170 vs 203. Final
   allocator stats: allocated 88.6, active 112.8 (active/allocated 1.27),
   resident 167.5 (resident/active 1.49), retained 22.1, mapped 190.
   `Arenas: 1 (1 automatic)`. Per class: 1280..2048 B at 80..84%
   occupancy in 215..332 slabs (13.4..20.8 MiB of slabs for 10.8..17.3 MiB
   live); the burst classes hold under 0.5 MiB live each (below the
   report cut). Every thread's frees refill every thread's cache from the
   same slabs, so a slab's dead slots are reused by the next job on any
   thread instead of waiting for the one thread whose arena owns them;
   the dirty term shrinks with it (resident - active 55 MiB against 192
   with 17 arenas) because the one arena's Empty inventory per class is
   one peak, not sixteen. The same run on x86_64 was not made (the amd64
   VM was gone by the time the x86_64 report had been read); the mechanism
   is architecture-independent and the 17-arena reports match on both.
4. Decision material for the human (plan Phase 3 prerequisite (c),
   "`background_thread` for Ruby-shaped consumers"): not `background_thread`;
   its effect on this shape is zero on both architectures. The term is the
   automatic arena count. challoc's default (`narenas` 64 automatic, one
   arena per thread until 64, as jemalloc's `narenas` = 4 x ncpu would
   also give) hands a mostly-idle 16-thread Ruby process 16 private slab
   sets whose dead slots only their owner refills, and the result is
   2.3..4.4x jemalloc's RSS for the whole life of the process; with one
   arena challoc is at jemalloc's RSS (+4% at t=600) with a 15% lower
   peak and a 2.6x lower floor after a full GC. Options, none taken here:
   (a) ship `narenas:1` (or a small fixed count) in the Ruby package's
   `malloc_conf` or `MALLOC_CONF`, which the compat library honours today;
   (b) change the automatic arena policy (jemalloc's own is by thread
   count too, but its extents are class-agnostic and its slabs 4..28 KiB,
   so the per-arena inventory costs it less); (c) let idle heaps release
   their arenas' partial slabs back to a shared pool (the compaction
   machinery, gated out here). (a) is a one-line packaging decision and
   measured; (b) and (c) are src/ work with the fast tier and this soak
   as the acceptance rows.
