# Ruby threaded RSS, attribution, 2026-09-21, aarch64

- lane ruby-rss, VM challoc-ruby-rss-arm64 (c4a-standard-16, Neoverse V2), Ubuntu 24.04 kernel 7.0.0-1011-gcp, the dev image (Wolfi), library 2078f67 (main at the lane's start; `df6d285` for the harness commits, whose `src/` is 2078f67's), `quantum16` `jemalloc-soname` stage, Ruby 3.4.10 `--with-jemalloc` from `contrib/ruby/build.sh`; jemalloc is Wolfi's 5.3.1 (`LD_PRELOAD`; on aarch64 a 64 KiB-page build, so its slabs are 64 KiB like challoc's; on x86_64 4 KiB pages, slabs 4..28 KiB).
- workload: `bench/ruby/soak.rb` (16 workers, bursts of 1..4 KiB strings and 100..400-element arrays, a growing long-lived set, a full GC every 30 s, `GC.compact` at the half), `bench/ruby-soak.sh` with the `VARIANTS` and `TRIM` knobs added in `1b724cd`/`df6d285`; every variant is the same binary with a different `MALLOC_CONF`.
- runs (harness tables and per-sample CSVs beside this file): `2026-09-21-aarch64-ruby-rss-attribution-base.md` (600 s: challoc, jemalloc, `narenas:1`), `-controls.md` (300 s: challoc, jemalloc, `dirty_decay_ms:0`, `dirty_decay_ms:1000`, `narenas:4`, `tcache_nslots_small_max:64`, `group_handoff:true`), `-trim.md` (300 s: challoc with `malloc_trim(0)` before every sample), `-onearena.md` (300 s: `narenas:1` alone and with `dirty_decay_ms:1000`, `:0`, `tcache_nslots_small_max:64`, `:8`), `-onearena-trim.md` (300 s: `narenas:1` with the trim). The `*-mstats.txt` files are each run's `malloc_stats_print` at the end.
- how to read the decomposition: `allocated` is every slot out of the bitmaps, in the program or in a thread cache (jemalloc counts its tcache the same way); `active - allocated` is the holes of partial slabs (T1 of the 2026-09-19 reading); `resident - active` is `Empty`, not yet purged slabs (T2). The periodic samples are taken right after a forced full GC with the workers running; the final row is taken after the workers were joined (their heaps destroyed) and one more full GC.

## 1. The 600 s baseline, reproduced on this tree

| variant | peak VmRSS MiB | final row MiB | jobs | process CPU s |
|---|---:|---:|---:|---:|
| challoc | 399.3 | 393.2 | 137218 | 601.98 |
| jemalloc | 203.0 | 167.7 | 138297 | 602.07 |
| challoc-1arena | 170.5 | 167.9 | 138312 | 602.01 |

| run | variant | sample | VmRSS | allocated | holes = active - allocated | dirty = resident - active | active | resident |
|---|---|---|---:|---:|---:|---:|---:|---:|
| base | challoc | t=60 fullgc | 286.8 | 246.8 | 41.8 | 25.0 | 288.6 | 313.6 |
| base | challoc | t=300 compact | 324.7 | 278.4 | 46.0 | 24.5 | 324.4 | 348.9 |
| base | challoc | t=570 fullgc | 387.8 | 324.8 | 32.5 | 36.5 | 357.3 | 393.8 |
| base | challoc | t=600 final-fullgc | 393.2 | 98.9 | 105.9 | 192.4 | 204.9 | 397.3 |
| base | jemalloc | t=60 fullgc | 99.5 | 49.7 | 53.9 | 30.7 | 103.6 | 134.3 |
| base | jemalloc | t=300 compact | 128.9 | 79.9 | 53.5 | 23.8 | 133.4 | 157.1 |
| base | jemalloc | t=570 fullgc | 194.4 | 119.4 | 48.4 | 37.8 | 167.9 | 205.6 |
| base | jemalloc | t=600 final-fullgc | 167.7 | 90.3 | 48.9 | 36.4 | 139.1 | 175.5 |
| base | challoc-1arena | t=60 fullgc | 41.7 | 40.0 | 13.8 | 10.1 | 53.8 | 63.9 |
| base | challoc-1arena | t=300 compact | 83.7 | 72.1 | 14.4 | 10.3 | 86.5 | 96.8 |
| base | challoc-1arena | t=570 fullgc | 151.2 | 114.5 | 16.4 | 21.7 | 130.9 | 152.6 |
| base | challoc-1arena | t=600 final-fullgc | 167.9 | 88.6 | 24.4 | 54.6 | 113.0 | 167.6 |

## 2. The controls (300 s, 17 arenas unless the variant says otherwise)

| run | variant | sample | VmRSS | allocated | holes = active - allocated | dirty = resident - active | active | resident |
|---|---|---|---:|---:|---:|---:|---:|---:|
| controls | challoc | t=60 fullgc | 286.7 | 246.2 | 42.9 | 24.0 | 289.1 | 313.1 |
| controls | challoc | t=150 compact | 302.6 | 258.7 | 48.8 | 20.6 | 307.5 | 328.1 |
| controls | challoc | t=270 fullgc | 317.0 | 271.7 | 49.7 | 19.8 | 321.4 | 341.2 |
| controls | challoc | t=300 final-fullgc | 321.0 | 62.3 | 94.3 | 188.2 | 156.5 | 344.7 |
| controls | jemalloc | t=60 fullgc | 95.6 | 50.0 | 52.2 | 29.5 | 102.2 | 131.8 |
| controls | jemalloc | t=150 compact | 111.1 | 62.4 | 54.5 | 35.2 | 116.9 | 152.1 |
| controls | jemalloc | t=270 fullgc | 128.1 | 72.7 | 56.4 | 30.8 | 129.1 | 159.8 |
| controls | jemalloc | t=300 final-fullgc | 109.1 | 56.6 | 50.7 | 25.6 | 107.3 | 132.9 |
| controls | decay0 | t=60 fullgc | 274.1 | 245.3 | 41.3 | 7.7 | 286.6 | 294.3 |
| controls | decay0 | t=150 compact | 289.0 | 257.5 | 46.6 | 8.0 | 304.1 | 312.1 |
| controls | decay0 | t=270 fullgc | 306.6 | 271.2 | 49.8 | 8.2 | 321.1 | 329.3 |
| controls | decay0 | t=300 final-fullgc | 147.1 | 60.4 | 93.9 | 13.3 | 154.3 | 167.6 |
| controls | decay1000 | t=60 fullgc | 280.6 | 244.3 | 43.7 | 17.3 | 288.0 | 305.3 |
| controls | decay1000 | t=150 compact | 291.8 | 255.1 | 48.7 | 12.1 | 303.8 | 315.9 |
| controls | decay1000 | t=270 fullgc | 312.2 | 271.4 | 50.7 | 14.4 | 322.0 | 336.4 |
| controls | decay1000 | t=300 final-fullgc | 312.3 | 59.7 | 90.7 | 184.4 | 150.4 | 334.8 |
| controls | narenas4 | t=60 fullgc | 94.1 | 80.8 | 19.5 | 17.1 | 100.3 | 117.4 |
| controls | narenas4 | t=150 compact | 108.2 | 91.8 | 21.2 | 13.0 | 113.0 | 126.0 |
| controls | narenas4 | t=270 fullgc | 127.3 | 105.4 | 22.4 | 14.8 | 127.8 | 142.5 |
| controls | narenas4 | t=300 final-fullgc | 135.5 | 57.3 | 37.6 | 54.9 | 94.9 | 149.8 |
| controls | tc64 | t=60 fullgc | 90.3 | 65.4 | 34.7 | 14.1 | 100.1 | 114.3 |
| controls | tc64 | t=150 compact | 103.5 | 75.4 | 40.4 | 12.0 | 115.7 | 127.7 |
| controls | tc64 | t=270 fullgc | 120.9 | 92.8 | 39.8 | 10.9 | 132.6 | 143.5 |
| controls | tc64 | t=300 final-fullgc | 129.1 | 56.1 | 42.5 | 53.7 | 98.5 | 152.2 |
| controls | handoff | t=60 fullgc | 287.3 | 244.8 | 42.6 | 25.8 | 287.4 | 313.2 |
| controls | handoff | t=150 compact | 299.8 | 258.8 | 47.5 | 19.2 | 306.2 | 325.4 |
| controls | handoff | t=270 fullgc | 318.0 | 273.2 | 50.2 | 19.0 | 323.4 | 342.4 |
| controls | handoff | t=300 final-fullgc | 322.9 | 59.9 | 92.2 | 194.2 | 152.1 | 346.3 |

| run | variant | sample | VmRSS | allocated | holes = active - allocated | dirty = resident - active | active | resident |
|---|---|---|---:|---:|---:|---:|---:|---:|
| trim | challoc | t=60 fullgc | 274.6 | 244.0 | 40.9 | 11.3 | 284.8 | 296.1 |
| trim | challoc | t=150 compact | 286.5 | 255.1 | 46.8 | 8.8 | 301.9 | 310.7 |
| trim | challoc | t=270 fullgc | 302.2 | 266.9 | 48.7 | 9.9 | 315.6 | 325.5 |
| trim | challoc | t=300 final-fullgc | 125.3 | 54.3 | 80.7 | 8.1 | 135.0 | 143.1 |

## 3. The residual under one arena (300 s; `n1` = `narenas:1`, the rest add one key to it)

| run | variant | sample | VmRSS | allocated | holes = active - allocated | dirty = resident - active | active | resident |
|---|---|---|---:|---:|---:|---:|---:|---:|
| onearena | n1 | t=60 fullgc | 41.8 | 39.5 | 12.5 | 11.8 | 52.1 | 63.9 |
| onearena | n1 | t=150 compact | 54.8 | 51.6 | 13.7 | 6.6 | 65.3 | 71.8 |
| onearena | n1 | t=270 fullgc | 73.0 | 64.7 | 14.7 | 7.7 | 79.4 | 87.1 |
| onearena | n1 | t=300 final-fullgc | 87.8 | 55.2 | 18.9 | 26.0 | 74.1 | 100.1 |
| onearena | n1decay1000 | t=60 fullgc | 39.6 | 39.4 | 11.9 | 8.0 | 51.3 | 59.3 |
| onearena | n1decay1000 | t=150 compact | 54.5 | 52.4 | 12.5 | 6.7 | 64.9 | 71.6 |
| onearena | n1decay1000 | t=270 fullgc | 71.1 | 63.2 | 14.4 | 6.3 | 77.6 | 84.0 |
| onearena | n1decay1000 | t=300 final-fullgc | 86.9 | 55.2 | 19.0 | 24.2 | 74.2 | 98.4 |
| onearena | n1decay0 | t=60 fullgc | 37.9 | 39.3 | 12.5 | 3.3 | 51.8 | 55.1 |
| onearena | n1decay0 | t=150 compact | 51.2 | 50.9 | 14.0 | 3.0 | 65.0 | 68.0 |
| onearena | n1decay0 | t=270 fullgc | 69.4 | 64.6 | 14.7 | 3.4 | 79.3 | 82.7 |
| onearena | n1decay0 | t=300 final-fullgc | 68.1 | 54.8 | 18.0 | 6.6 | 72.8 | 79.4 |
| onearena | n1tc64 | t=60 fullgc | 31.4 | 31.1 | 12.2 | 9.1 | 43.3 | 52.4 |
| onearena | n1tc64 | t=150 compact | 45.0 | 44.0 | 11.6 | 5.5 | 55.6 | 61.1 |
| onearena | n1tc64 | t=270 fullgc | 63.7 | 53.3 | 16.4 | 7.3 | 69.7 | 77.0 |
| onearena | n1tc64 | t=300 final-fullgc | 76.4 | 54.3 | 15.3 | 19.0 | 69.5 | 88.5 |
| onearena | n1tc8 | t=60 fullgc | 31.4 | 32.4 | 10.9 | 9.2 | 43.2 | 52.4 |
| onearena | n1tc8 | t=150 compact | 45.1 | 43.9 | 12.9 | 4.4 | 56.8 | 61.2 |
| onearena | n1tc8 | t=270 fullgc | 63.7 | 58.0 | 12.1 | 7.0 | 70.0 | 77.0 |
| onearena | n1tc8 | t=300 final-fullgc | 79.7 | 54.3 | 15.5 | 22.0 | 69.8 | 91.8 |

| run | variant | sample | VmRSS | allocated | holes = active - allocated | dirty = resident - active | active | resident |
|---|---|---|---:|---:|---:|---:|---:|---:|
| onearena-trim | n1 | t=60 fullgc | 38.2 | 39.0 | 13.3 | 4.8 | 52.4 | 57.2 |
| onearena-trim | n1 | t=150 compact | 52.8 | 50.8 | 13.3 | 4.0 | 64.1 | 68.2 |
| onearena-trim | n1 | t=270 fullgc | 69.6 | 66.2 | 12.2 | 5.1 | 78.4 | 83.5 |
| onearena-trim | n1 | t=300 final-fullgc | 65.8 | 54.6 | 18.3 | 3.0 | 72.9 | 75.9 |

| variant | peak VmRSS MiB | final row MiB | jobs | process CPU s |
|---|---:|---:|---:|---:|
| n1 | 87.8 | 87.8 | 69295 | 301.18 |
| n1decay1000 | 86.9 | 86.9 | 69257 | 301.19 |
| n1decay0 | 73.3 | 68.1 | 68506 | 301.23 |
| n1tc64 | 76.4 | 76.4 | 68993 | 301.17 |
| n1tc8 | 79.7 | 79.7 | 69105 | 301.21 |

## Reading (lane ruby-rss, 2026-09-21)

The numbers below are from the tables above; where a figure is quoted for
both architectures it is written aarch64 / x86_64.

1. **The gap is neither slab under-occupancy nor a decay under-run. It is
   slots held in thread caches.** At every periodic sample while the
   workers run, challoc's excess over jemalloc sits in `allocated`: at
   t=60, 246.8 / 262.6 MiB against jemalloc's 49.7 / 44.8 for the same
   program state (the live set is ~55 MiB at the end of a 300 s run and
   ~90 MiB at the end of 600 s, see the final rows' `allocated`, which
   are within 10% of jemalloc's). The holes are *smaller* than
   jemalloc's on aarch64 (41.8 vs 53.9 MiB) and the dirty inventory is
   about the same (25.0 vs 30.7; on x86_64 31.2 vs 26.0). Since
   `allocated` counts every slot out of the bitmaps, the ~200 MiB that
   the program does not hold are in caches. Which caches: a thread's home
   cache holds at most the ladder's capacity per class (32..128 slots for
   the 1..4 KiB classes, ~1 MiB per thread), so 16 threads cannot hold
   200 MiB there; the per-`(arena, class)` caches a thread kept for
   every *other* arena it freed into (`Heap::others`, design 6.3 step 4
   as it was) can: Ruby's GC sweep frees every thread's garbage from
   whichever thread runs the sweep, so every worker frees into all 16
   other arenas, 16 x 16 = 256 (thread, arena) pairs x 10 classes x
   32..128 slots, about 1 MiB per pair, 272 MiB at capacity; the default
   path never pops from such a cache (only `MALLOCX_ARENA` does), so a
   slot leaves it only when the cache is full (the flush releases half)
   or at the cache GC (three quarters every 10 s), and the sawtooth
   between half and full puts the residue at three quarters of capacity:
   ~200 MiB. That is the whole of the mid-run gap.

2. **T1 and T2 of the 2026-09-19 reading are the final row, an artifact
   of the workers' exit.** `soak.rb` joins the workers before its last
   sample; their heaps are destroyed and every cache flushed:
   `allocated` falls from 278.4 / 313.1 at `GC.compact` to 98.9 / 104.2
   at the final row while `resident` does not move (348.9 -> 397.3 /
   382.2 -> 414.5): the released slots become `Empty` slabs (dirty 24.5
   -> 192.4 / 27.8 -> 201.6 MiB) and leave the long-lived objects alone
   in theirs (holes 46.0 -> 105.9 / 41.3 -> 108.8). Nothing in that row
   is older than the decay window, so it is not a purge under-run: the
   same run under `dirty_decay_ms:0` reads 147.1 / 166.1 at the final row
   against 321.0 / 357.9 (the exit flush purged at once) and only -3.4% /
   -6.2% at the peak; `dirty_decay_ms:1000` -2.7% / -3.9%; `malloc_trim(0)`
   before every sample -4..-5% mid-run (RSS reads Active plus metadata,
   and Active is the cached slots). jemalloc's final row is lower for a
   second reason: its `tcache_destroy` force-purges an arena when its
   last thread leaves (`arena_nthreads == 0`), and with one arena per
   worker its resident drops 46 MiB at that instant (x86_64: 171.4 ->
   125.1 while `retained` rises 46.2 -> 92.5); challoc had no such rule.

3. **The controls, and which term each moves.** `narenas:4`: 16 x 3
   pairs instead of 16 x 16, RSS -67% / -63% at t=60, peak 135.5 / 167.2
   against jemalloc's 130.5 / 118.2. `tcache_nslots_small_max:64` (the
   1..4 KiB classes at the 8-slot floor): -68% / -64% at t=60, peak
   129.1 / 165.1, at no CPU cost (4.39 vs 4.38 ms per job / 3.99 vs
   4.09): the configuration-only proof that the cache residue is the
   mechanism, since it changes neither the arena count nor the slabs.
   `narenas:1` (600 s): peak 170.5 / 184.4 against jemalloc's 203.0 /
   182.0, at every periodic sample of the last minute at or below
   jemalloc on both architectures (aarch64 t=570: 151.2 vs 194.4; x86_64
   165.3 vs 178.7), final row +0.1% / +35.2% (the exit dirty, 54.6 /
   64.1 MiB, that jemalloc's rule purges and challoc's decay would take
   10 s over). `group_handoff:true`: nothing (+0.6% / -0.9%); the groups
   are not idle while the caches pin their slots. `background_thread`
   was already measured at nothing on 2026-09-19. The compacting cache
   never engaged (`c_on 0` in every report) and could not have helped:
   occupancy is not the mid-run term, and the per-class occupancy
   figures of the 2026-09-19 reading (48..52% for 1280..2048 B) are the
   final row's, after the exit flush.

4. **Cost.** Every variant is within 1.5% of the others on CPU per job
   (4.35..4.41 ms aarch64, 3.97..4.09 x86_64) and within 3% on jobs
   done; the residue costs nothing to hold and nothing to not hold.

5. **What it means for the candidates.** (A) a contention-driven arena
   policy would put a GVL program on one arena and so remove the
   residue for Ruby and Python, but the residue is a property of every
   multi-arena process whose frees cross threads (Valkey's io-threads,
   the JVM's 200 threads on 64 arenas), and it changes the arena policy
   for every consumer; (B) a shared `Empty` pool and (D) the compaction
   gate attack terms that are not the mid-run gap; (C) an idle-heap
   release is what the cache GC already does every 10 s, and the caches
   refill within a second. The mechanism to remove is the per-arena
   cache a plain `free` fills and never pops. Two forms were built and
   measured: jemalloc's exact shape, every foreign slot onto the home
   cache with the flush routing it (`4dc352c`), removes the residue but
   hands a thread that inherits another thread's blocks that arena's
   slots back for its own allocations, and larson10 / larson16 on the
   Xeon lost 10% / 6% (`2026-09-21-x86_64-ruby-rss-ab-split.md`, `f3`
   column; the Axion did not move); the form kept (`c8a8e03`) leaves the
   home cache and the `free` fast path as they were (byte-identical) and
   makes the foreign side one flush-only cache per (thread, class)
   instead of one per (thread, arena, class), the same bound as
   jemalloc's tcache. Plus jemalloc's exit rule for the final row
   (`1a45278`, rate-limited to once per decay period per arena by
   `6e25c7a` after the unlimited form cost mstress +11% through refaults
   under thread churn, `-ab-split.md` `purge` column). The numbers are in
   `2026-09-21-<arch>-ruby-rss-v3-soak.md`, `-v3-ab.md`, `-v3-ruby.md`,
   `-quick.md` (the v2 files are the form before the foreign cache was
   halved, `c8a8e03`).
