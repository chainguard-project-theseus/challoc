## Done

- Ruby threaded RSS (lane ruby-rss, 2026-09-21): done, commits `1b724cd`
  `df6d285` (harness), `4dc352c` `1a45278` `6e25c7a` `c8a8e03` `aeafc1e`
  (library), `82b8a52` `987a5cc` and the results commits after them.
  The 16-thread Ruby soak's 2x RSS against jemalloc was the caches a
  plain `free` filled per `(arena, class)` on the freeing thread and
  never popped from (`bench/results/2026-09-21-{aarch64,x86_64}-ruby-rss-attribution.md`:
  mid-run `allocated` 247 / 263 MiB against jemalloc's 50 / 45 for a
  ~55 MiB live set, holes and dirty at or below jemalloc's; the T1/T2 of
  the 2026-09-19 reading are the final row after the workers' exit
  flush; `tcache_nslots_small_max:64` alone took -66% off at no CPU
  cost, the configuration-only proof). The default now: a plain free of
  another arena's slot goes onto one flush-only cache per (thread,
  class) (half the ladder, released whole, its flush routing each slot to
  its slab's arena; `c8a8e03`, `aeafc1e`) instead of one per (thread,
  arena, class); the last thread heap of an arena purges its `Empty`
  slabs, at most once per decay period (`1a45278`, `6e25c7a`). `free`,
  `malloc`, `sdallocx`, `calloc`, `free_sized` are byte-identical to the
  base. The 600 s soak (`2026-09-21-<arch>-ruby-rss-v3-soak.md`):
  aarch64 peak 189.4 vs jemalloc 200.8 MiB (-5.7%), final 146.2 vs 169.2
  (-13.6%), from 399 / 393 (+97% / +134%); x86_64 peak 205.6 vs 182.1
  (+12.9%), final 163.4 vs 131.6 (+24.2%; +9.6% at the last periodic row,
  196.1 vs 179), from 417 / 411 (+129% / +202%); CPU per job equal
  (4.35 vs 4.35, 3.95 vs 4.01 ms), jobs within 2%. Acceptance on the
  lane's VMs: `make test` 482 + 138 + 26 passed, 0 failed, both archs
  (`-quick.md`); `quick` go on both; `ab` 5 rounds (`-v3-ab.md`): every
  row within 1.1% of the base on both archs, xmalloc -12.4% / -9.0%
  (faster), larson10 RSS 112 -> 102 / 96 -> 85 MiB; `ruby-bench`
  (`-v3-ruby.md`, `-v2-ruby.md`) startup, gem_list, erb_render, gc_churn
  within the 2026-09-20 rows; the idle shapes on aarch64
  (`-v3-soak-t64.md`, `-v3-soak-idle.md`); the Valkey soak guard on
  x86_64 (`bench/results/ruby-rss-valkey*/`); `quick-perf` in
  `-quick.md`. The split A/B of the commits (`-ab-split.md`): the
  jemalloc-shaped form (`4dc352c`) cost larson10 / larson16 +10% / +6%
  on the Xeon and was not kept; the unlimited exit purge cost mstress
  +11% on both and got the rate limit.

## Design decisions

- Ruby threaded RSS: the mechanism removed is the per-arena cache of
  cross-thread frees, not the arena policy. The candidates named for the
  task, against the attribution: (A) contention-driven arena assignment
  would hide the residue for GVL programs alone (the same residue exists
  for any multi-arena process whose frees cross threads: Valkey's
  io-threads, the JVM) and changes every consumer's arena policy; (B) a
  shared `Empty` pool and (D) the compaction gate attack terms that are
  not the mid-run gap (`dirty_decay_ms:0` -3..-6% at the peak,
  `malloc_trim` before every sample -4..-5%, `c_on 0`); (C) idle-heap
  release is what the cache GC already does every 10 s and the caches
  refill within a second.
- Ruby threaded RSS: jemalloc's exact shape (every foreign slot onto the
  home cache, the flush routing it, `4dc352c`) removes the residue and
  reads within 10% of jemalloc at the peak on both archs, but hands a
  thread that inherits another thread's blocks that arena's slots back
  for its own allocations: larson10 / larson16 on the Xeon -10% / -6%
  (`2026-09-21-x86_64-ruby-rss-ab-split.md`; the Axion did not move).
  The form kept leaves the home cache and the `free` fast path as they
  were and makes only the foreign side one cache per (thread, class):
  a live slot still never migrates between arenas, design 13.3 keeps
  its sentence, and the residue bound is below jemalloc's tcache's.
- Ruby threaded RSS: the exit purge fires when an arena's bound heap
  count reaches zero, at most once per decay period per arena and never
  with a background thread or decay off; unlimited (jemalloc's rule as
  written) it cost mstress +11% on both archs through refaults under
  thread churn (800 exits in 0.4 s).
- Ruby threaded RSS: the harness (`bench/ruby-soak.sh`) takes
  `VARIANTS="name=malloc_conf ..."` and `TRIM=1`, and records CPU per
  job; the attribution's decomposition (allocated / holes / dirty per
  sample) is the reading every RSS figure of this soak should get.

## Notes for the human

- Ruby threaded RSS: the criterion "RSS at t=600" is the harness's
  final row, taken after the workers were joined and one more full GC.
  jemalloc's figure there varies 131..168 MiB across today's four
  x86_64 runs (its per-arena force purge at the last thread's exit fires
  or not); challoc's is 146 (aarch64) / 163 (x86_64), the x86_64 residual
  being 64 KiB-slab holes across 17 arenas (30.6 vs 12.6 MiB; geometry,
  out of scope per the task) and the `Empty` slabs the later exits'
  flushes leave in arenas already purged. The steady-state comparison
  (last periodic row, workers running): aarch64 182 vs 199, x86_64 196
  vs 179 (+9.6%). The peak on x86_64 is +12.9% (aarch64 -5.7%): the
  goal's "about 10%" is met on aarch64 and missed by 3 points on x86_64
  against jemalloc's 4 KiB-page build; the jemalloc-shaped form that met
  it (+9.8%) costs larson10 10% on the Xeon, a trade the human may still
  want (`4dc352c` is one commit).
- Ruby threaded RSS: the lane touched `src/heap/*.rs`, `src/arena.rs`
  (lane A's partition, as the task assigned), `src/api/jemalloc.rs`
  (tests only), `docs/design.md` 6.2/6.3/13.3, `docs/tuning.md`, the
  Makefile (two forwarded variables), `bench/ruby-soak.sh`,
  `bench/ruby/soak.rb`. No new config key, no new lock (FORK_LOCKS
  unchanged). `narenas:1` stays the lower-peak configuration (170 / 184
  MiB) at one arena's class locks for every thread.
- Ruby threaded RSS: `bench/results/2026-09-21-aarch64-ruby-rss-ab-split.md`
  was reconstructed from the VM's run log after a `challoc-bench sync`
  (rsync `--delete`) removed the unfetched file; the tables are the run's
  own output.
