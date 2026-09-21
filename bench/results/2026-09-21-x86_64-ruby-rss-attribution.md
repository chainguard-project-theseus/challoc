# Ruby threaded RSS, attribution, 2026-09-21, x86_64

- lane ruby-rss, VM challoc-ruby-rss-amd64 (c4-standard-16, Xeon 8581C), Ubuntu 24.04 kernel 7.0.0-1011-gcp, the dev image (Wolfi), library 2078f67 (main at the lane's start; `df6d285` for the harness commits, whose `src/` is 2078f67's), `quantum16` `jemalloc-soname` stage, Ruby 3.4.10 `--with-jemalloc` from `contrib/ruby/build.sh`; jemalloc is Wolfi's 5.3.1 (`LD_PRELOAD`; on aarch64 a 64 KiB-page build, so its slabs are 64 KiB like challoc's; on x86_64 4 KiB pages, slabs 4..28 KiB).
- workload: `bench/ruby/soak.rb` (16 workers, bursts of 1..4 KiB strings and 100..400-element arrays, a growing long-lived set, a full GC every 30 s, `GC.compact` at the half), `bench/ruby-soak.sh` with the `VARIANTS` and `TRIM` knobs added in `1b724cd`/`df6d285`; every variant is the same binary with a different `MALLOC_CONF`.
- runs (harness tables and per-sample CSVs beside this file): `2026-09-21-x86_64-ruby-rss-attribution-base.md` (600 s: challoc, jemalloc, `narenas:1`), `-controls.md` (300 s: challoc, jemalloc, `dirty_decay_ms:0`, `dirty_decay_ms:1000`, `narenas:4`, `tcache_nslots_small_max:64`, `group_handoff:true`), `-trim.md` (300 s: challoc with `malloc_trim(0)` before every sample), `-onearena.md` (300 s: `narenas:1` alone and with `dirty_decay_ms:1000`, `:0`, `tcache_nslots_small_max:64`, `:8`), `-onearena-trim.md` (300 s: `narenas:1` with the trim). The `*-mstats.txt` files are each run's `malloc_stats_print` at the end.
- how to read the decomposition: `allocated` is every slot out of the bitmaps, in the program or in a thread cache (jemalloc counts its tcache the same way); `active - allocated` is the holes of partial slabs (T1 of the 2026-09-19 reading); `resident - active` is `Empty`, not yet purged slabs (T2). The periodic samples are taken right after a forced full GC with the workers running; the final row is taken after the workers were joined (their heaps destroyed) and one more full GC.

## 1. The 600 s baseline, reproduced on this tree

| variant | peak VmRSS MiB | final row MiB | jobs | process CPU s |
|---|---:|---:|---:|---:|
| challoc | 416.8 | 411.0 | 148159 | 593.43 |
| jemalloc | 182.0 | 136.2 | 148545 | 592.8 |
| challoc-1arena | 184.4 | 184.2 | 151802 | 593.78 |

| run | variant | sample | VmRSS | allocated | holes = active - allocated | dirty = resident - active | active | resident |
|---|---|---|---:|---:|---:|---:|---:|---:|
| base | challoc | t=60 fullgc | 311.4 | 262.6 | 45.6 | 31.2 | 308.1 | 339.3 |
| base | challoc | t=300 compact | 353.4 | 313.1 | 41.3 | 27.8 | 354.5 | 382.2 |
| base | challoc | t=570 fullgc | 404.0 | 331.5 | 38.6 | 37.2 | 370.1 | 407.3 |
| base | challoc | t=600 final-fullgc | 411.0 | 104.2 | 108.8 | 201.6 | 213.0 | 414.5 |
| base | jemalloc | t=60 fullgc | 72.3 | 44.8 | 7.9 | 26.0 | 52.7 | 78.7 |
| base | jemalloc | t=300 compact | 118.4 | 88.2 | 4.3 | 24.5 | 92.5 | 117.0 |
| base | jemalloc | t=570 fullgc | 178.7 | 118.9 | 5.6 | 44.2 | 124.4 | 168.7 |
| base | jemalloc | t=600 final-fullgc | 136.2 | 93.5 | 12.4 | 19.2 | 105.9 | 125.1 |
| base | challoc-1arena | t=60 fullgc | 60.3 | 50.0 | 14.9 | 17.7 | 64.9 | 82.6 |
| base | challoc-1arena | t=300 compact | 124.5 | 91.7 | 16.9 | 26.8 | 108.7 | 135.5 |
| base | challoc-1arena | t=570 fullgc | 165.3 | 128.0 | 15.1 | 20.4 | 143.1 | 163.5 |
| base | challoc-1arena | t=600 final-fullgc | 184.2 | 95.4 | 24.2 | 64.1 | 119.6 | 183.7 |

## 2. The controls (300 s, 17 arenas unless the variant says otherwise)

| run | variant | sample | VmRSS | allocated | holes = active - allocated | dirty = resident - active | active | resident |
|---|---|---|---:|---:|---:|---:|---:|---:|
| controls | challoc | t=60 fullgc | 307.9 | 256.3 | 47.6 | 32.2 | 303.9 | 336.1 |
| controls | challoc | t=150 compact | 330.7 | 276.0 | 48.8 | 30.3 | 324.8 | 355.1 |
| controls | challoc | t=270 fullgc | 332.5 | 283.2 | 48.8 | 25.9 | 332.0 | 357.8 |
| controls | challoc | t=300 final-fullgc | 357.9 | 67.6 | 98.8 | 220.0 | 166.4 | 386.4 |
| controls | jemalloc | t=60 fullgc | 72.6 | 43.9 | 8.0 | 27.1 | 51.9 | 78.9 |
| controls | jemalloc | t=150 compact | 92.4 | 55.1 | 8.7 | 32.7 | 63.8 | 96.5 |
| controls | jemalloc | t=270 fullgc | 95.8 | 61.0 | 8.7 | 27.4 | 69.7 | 97.1 |
| controls | jemalloc | t=300 final-fullgc | 84.3 | 57.3 | 11.4 | 14.1 | 68.7 | 82.8 |
| controls | decay0 | t=60 fullgc | 292.6 | 260.3 | 45.5 | 7.9 | 305.8 | 313.7 |
| controls | decay0 | t=150 compact | 311.9 | 273.2 | 50.9 | 8.1 | 324.1 | 332.3 |
| controls | decay0 | t=270 fullgc | 319.5 | 287.8 | 45.7 | 8.2 | 333.4 | 341.7 |
| controls | decay0 | t=300 final-fullgc | 166.1 | 68.9 | 101.4 | 10.7 | 170.3 | 181.0 |
| controls | decay1000 | t=60 fullgc | 299.9 | 258.9 | 45.5 | 18.4 | 304.4 | 322.9 |
| controls | decay1000 | t=150 compact | 320.8 | 272.7 | 53.4 | 16.4 | 326.1 | 342.5 |
| controls | decay1000 | t=270 fullgc | 326.9 | 286.2 | 47.7 | 15.8 | 333.9 | 349.7 |
| controls | decay1000 | t=300 final-fullgc | 344.1 | 68.0 | 101.0 | 203.3 | 169.0 | 372.3 |
| controls | narenas4 | t=60 fullgc | 113.4 | 91.9 | 21.1 | 24.0 | 112.9 | 137.0 |
| controls | narenas4 | t=150 compact | 133.8 | 103.9 | 25.6 | 20.7 | 129.5 | 150.3 |
| controls | narenas4 | t=270 fullgc | 141.3 | 115.3 | 23.3 | 16.8 | 138.6 | 155.4 |
| controls | narenas4 | t=300 final-fullgc | 165.4 | 60.9 | 42.0 | 75.3 | 102.8 | 178.1 |
| controls | tc64 | t=60 fullgc | 111.6 | 76.5 | 40.8 | 19.6 | 117.3 | 136.8 |
| controls | tc64 | t=150 compact | 134.0 | 88.1 | 43.1 | 24.9 | 131.2 | 156.2 |
| controls | tc64 | t=270 fullgc | 138.8 | 103.6 | 41.9 | 15.5 | 145.5 | 161.0 |
| controls | tc64 | t=300 final-fullgc | 163.0 | 59.5 | 48.9 | 80.8 | 108.3 | 189.1 |
| controls | handoff | t=60 fullgc | 310.5 | 261.2 | 47.2 | 29.8 | 308.4 | 338.2 |
| controls | handoff | t=150 compact | 330.3 | 275.7 | 47.9 | 29.9 | 323.6 | 353.5 |
| controls | handoff | t=270 fullgc | 333.8 | 288.3 | 45.4 | 23.6 | 333.6 | 357.2 |
| controls | handoff | t=300 final-fullgc | 354.8 | 68.1 | 99.9 | 213.7 | 168.0 | 381.7 |

| run | variant | sample | VmRSS | allocated | holes = active - allocated | dirty = resident - active | active | resident |
|---|---|---|---:|---:|---:|---:|---:|---:|
| trim | challoc | t=60 fullgc | 289.2 | 254.8 | 44.7 | 11.0 | 299.5 | 310.5 |
| trim | challoc | t=150 compact | 303.3 | 267.3 | 47.1 | 10.5 | 314.4 | 324.9 |
| trim | challoc | t=270 fullgc | 311.3 | 281.0 | 42.2 | 10.1 | 323.3 | 333.3 |
| trim | challoc | t=300 final-fullgc | 132.4 | 56.9 | 81.4 | 8.3 | 138.3 | 146.5 |

## 3. The residual under one arena (300 s; `n1` = `narenas:1`, the rest add one key to it)

| run | variant | sample | VmRSS | allocated | holes = active - allocated | dirty = resident - active | active | resident |
|---|---|---|---:|---:|---:|---:|---:|---:|
| onearena | n1 | t=60 fullgc | 60.6 | 51.6 | 13.9 | 17.2 | 65.5 | 82.7 |
| onearena | n1 | t=150 compact | 80.4 | 61.9 | 17.2 | 16.7 | 79.2 | 95.9 |
| onearena | n1 | t=270 fullgc | 92.2 | 75.2 | 14.4 | 15.3 | 89.6 | 104.9 |
| onearena | n1 | t=300 final-fullgc | 111.4 | 58.7 | 22.5 | 41.5 | 81.2 | 122.7 |
| onearena | n1decay1000 | t=60 fullgc | 58.8 | 49.9 | 15.1 | 13.6 | 65.0 | 78.6 |
| onearena | n1decay1000 | t=150 compact | 78.9 | 61.6 | 17.7 | 14.5 | 79.4 | 93.9 |
| onearena | n1decay1000 | t=270 fullgc | 86.1 | 76.5 | 13.1 | 9.2 | 89.5 | 98.7 |
| onearena | n1decay1000 | t=300 final-fullgc | 109.2 | 58.7 | 22.8 | 39.4 | 81.5 | 120.9 |
| onearena | n1decay0 | t=60 fullgc | 51.2 | 48.7 | 15.6 | 3.0 | 64.3 | 67.3 |
| onearena | n1decay0 | t=150 compact | 68.8 | 63.3 | 16.6 | 3.0 | 79.9 | 82.9 |
| onearena | n1decay0 | t=270 fullgc | 82.4 | 72.6 | 17.1 | 3.1 | 89.8 | 92.9 |
| onearena | n1decay0 | t=300 final-fullgc | 77.1 | 58.3 | 22.9 | 3.4 | 81.2 | 84.6 |
| onearena | n1tc64 | t=60 fullgc | 51.8 | 43.1 | 12.5 | 16.9 | 55.6 | 72.4 |
| onearena | n1tc64 | t=150 compact | 71.5 | 52.0 | 17.2 | 17.0 | 69.2 | 86.2 |
| onearena | n1tc64 | t=270 fullgc | 80.8 | 71.6 | 13.3 | 7.8 | 84.9 | 92.7 |
| onearena | n1tc64 | t=300 final-fullgc | 105.1 | 57.6 | 20.1 | 38.0 | 77.7 | 115.7 |
| onearena | n1tc8 | t=60 fullgc | 51.2 | 42.7 | 13.7 | 15.7 | 56.4 | 72.1 |
| onearena | n1tc8 | t=150 compact | 71.3 | 54.0 | 15.6 | 16.7 | 69.6 | 86.3 |
| onearena | n1tc8 | t=270 fullgc | 80.0 | 68.9 | 12.8 | 10.6 | 81.6 | 92.2 |
| onearena | n1tc8 | t=300 final-fullgc | 104.9 | 57.7 | 20.0 | 38.3 | 77.7 | 116.0 |

| run | variant | sample | VmRSS | allocated | holes = active - allocated | dirty = resident - active | active | resident |
|---|---|---|---:|---:|---:|---:|---:|---:|
| onearena-trim | n1 | t=60 fullgc | 52.1 | 51.6 | 13.8 | 4.3 | 65.4 | 69.7 |
| onearena-trim | n1 | t=150 compact | 67.7 | 60.2 | 17.7 | 6.2 | 77.9 | 84.1 |
| onearena-trim | n1 | t=270 fullgc | 80.7 | 69.3 | 18.2 | 8.2 | 87.5 | 95.7 |
| onearena-trim | n1 | t=300 final-fullgc | 75.5 | 57.9 | 22.1 | 3.1 | 79.9 | 83.1 |

| variant | peak VmRSS MiB | final row MiB | jobs | process CPU s |
|---|---:|---:|---:|---:|
| n1 | 111.4 | 111.4 | 75398 | 296.88 |
| n1decay1000 | 111.9 | 109.2 | 75380 | 296.66 |
| n1decay0 | 99.4 | 77.1 | 74278 | 297.02 |
| n1tc64 | 108.2 | 105.1 | 74539 | 297.13 |
| n1tc8 | 104.9 | 104.9 | 74991 | 297.02 |

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
