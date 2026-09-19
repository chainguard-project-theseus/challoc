## Done

- Phase 2.5 acceptance (lane D) DONE as a measurement campaign, no `src/`
  change, commits 6c2c495 (the driver), ed45135 (the kernel matrix re-run
  and the findings) and the results commit that follows. The checklist is
  `bench/results/2026-09-19-phase-2.5-acceptance.md`, one row per criterion
  of "Acceptance for the phase" with the evidence, the verdict and what
  remains, all at 675f357 on the lane's own VMs (`challoc-lane-d-*`, nine
  matrix cells and three lab cells, every one deleted). Verdicts: the floor
  **not yet** (two regressions, below); the ceiling table **not yet** (one
  red host, below; every probed feature otherwise has its both-ways run,
  cost row and design entry, with four precise gaps: no host exposes
  WAITPKG so `umwait` never ran, no cost row for the `process_madvise`
  batch against the loop, none for `anon_name` and the PSI trigger, task 8
  and task 6 pending); the reference scorecard **met** (every C4A row
  within 1.5 points of 3ed77b7; on the C4 larson16 +12.9% on the merge
  line, above the row's 20-line band, to watch); the CPU matrix records
  **met**; the four-way table **met with attribution** (challoc is not
  literally at or ahead on every Ruby row: startup +5.3% / +10.8%,
  erb_render +10.3% vs tcmalloc, small-process RSS +10..16% vs glibc, all
  attributed M1/M4/M6 in the files); per-CPU caches **blocked on task 8**
  (the measurement is ready: `bench/percpu-campaign.sh`, `make
  percpu-campaign MODE=baseline|percpu`, baseline half in
  `bench/results/2026-09-19-{aarch64,x86_64}-lane-d-percpu-baseline*`);
  tagged pointers **blocked on hardware** (task 6); the Phase 0..2 criteria
  with defaults **met on the reference kernel** (`make test` 513/0 on both
  VMs, the Phase 0 set within 10% on time with mstress RSS the accepted row
  at +16.7% x86_64, Valkey `memefficiency` 33 ok on both and the full suite
  5189 ok on x86_64, the aarch64 soak ahead in the write phase with the
  standing +1.4% delete residue and RSS +1.1% / +0.8%, Ruby `test-all` the
  same nine environmental failures, the container tests green), with the
  x86_64 soak's write phase 1.5..4.9% behind jemalloc on req/s as a first
  same-protocol reading on the C4.

## Design decisions

- Phase 2.5 acceptance (lane D): the task 8 campaign puts challoc alone
  under a key through `CHALLOC_CONF` (read by challoc after `MALLOC_CONF`,
  by no stock allocator), which `DOCKER_BASE` in the Makefile now forwards
  into every container when set, so the same suite (Valkey soak, Ruby
  rows, `test-all`, Ruby soak, container tests) runs with jemalloc's column
  untouched; only `ab` uses the variant's own `MALLOC_CONF`
  (`percpu=tree:percpu_arena:percpu` next to `default=tree` in one run).
  Reason: the suites take one environment for every allocator, and a
  `MALLOC_CONF` value would also reach jemalloc, which has a `percpu_arena`
  key of its own.
- Phase 2.5 acceptance (lane D): the "many idle threads" Ruby soak is a
  variant of `bench/ruby/soak.rb` (`IDLE_THREADS`, threads that allocate a
  burst once and then a tiny one every 5..10 s) rather than a new script,
  and `larson64` a new `lib.sh` row (64 threads on 16 CPUs), so the percpu
  half compares like with like against the baseline half. Baseline shape:
  64 threads make `Arenas: 64` and 415..463 MiB against jemalloc's
  115..192 MiB at t=300.
- Phase 2.5 acceptance (lane D): the kernel matrix re-run logs are named
  `bench/results/matrix/2026-09-19-lane-d-acceptance-<cell>.log`
  (`MATRIX_DATE` override) so task 3's records of the morning stay intact;
  lab cells run under `CHALLOC_MATRIX_PREFIX=challoc-lane-d-lab` so the
  driver's exit sweep of the matrix prefix does not delete them.

## Notes for the human

- Phase 2.5 acceptance (lane D), three regressions found at 675f357 that
  belong to other lanes, with the evidence in
  `bench/results/2026-09-19-phase-2.5-acceptance.md` sections 1.2, 1.3 and
  2.1: (1) lane B, task 10: the container churn test (`cgroup_churn purge
  400 100`, RSS under 100 MiB within 2 s) fails on the 5.10 and 6.1 Debian
  cells (deterministic on the 2-shared-core e2 x86 cells, 3 of 16 on a
  4-vCPU Axion), passed at d152a64 this morning; on a kept 6.1 cell
  `lazy_band:false` passes 24/24 and the pre-task-10 tree 12/12 against
  the default 13/16, THP setting irrelevant: the freed memory goes out as
  `MADV_FREE` (the slabs are `Purged`, `empty_bytes` 1..8 MiB, RSS stays
  at 216..349 MiB), although the churn frees at 79% of the limit, above
  the `Low` band; the suspect is the band mirror `purge::set_headroom`
  reading a sample from before the 400 MiB ramp on a slow host, but the
  larger point stands: no lazy purge can meet the churn criterion, so
  either the churn's flush must see the eager band or the criterion must
  read `pglazyfree`. (2) lane B, task 9: `api::ctests::c_thp_always` fails
  on the 64 KiB kernel, whose PMD is 512 MiB (`mthp=512M`): `thp:always`
  advises 1.3 GiB of PMD chunks for an 8 MiB touch, the kernel gives no
  folio, and the test asserts a 2 MiB PMD; `thp` is off by default, so the
  floor with defaults is untouched, but the suite is red at 64 KiB and
  `thp:always` should refuse or fall back where the PMD is not 2 MiB.
  (3) lane A, task 7: `make test` on an Ampere Altra (the arm64 host
  without PAC, `pac=no`) dies with `SIGILL` in
  `heap::linux_tests::calloc_flag_is_honest_about_memory`, the first heap
  test after the two PAC tests skip: the heap tests' `Shared` builders set
  `pac_pointers: true` without the probe and `pacda`/`autda` run on a CPU
  without FEAT_PAuth; the library's own init gates on the HWCAP probe (the
  features program and every earlier Altra run were fine), so this is a
  test bug, but it means the arm64-without-PAC direction of task 7 has no
  green run yet. Until (1) and (2) are fixed and the four cells re-run,
  the floor criterion is not met.
- Phase 2.5 acceptance (lane D), two readings to weigh: the x86_64 Valkey
  soak at 675f357 (`bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-soak/`)
  has the write phase 1.5% (valkey-benchmark) and 4.9% (memtier) behind
  bundled jemalloc on req/s with p99 -1.3% / +1.7%, peak RSS +0.4% and
  post-settle RSS -3.6%; it is the first 20-minute soak under the
  acceptance protocol on the C4 in the records (the Phase 2 acceptance
  soaked the C4A), so it is a reading, not a regression call; a second run
  would say. And the C4 scorecard line of the merge (0f5fea8) has larson16
  at +12.9%, 2.1 points above the row's highest of twenty lines; lane C's
  `ab` on the lock change read that row within noise; the coordinator's
  next scorecard decides.
- Phase 2.5 acceptance (lane D), what only lands after task 8 merges: the
  percpu half, `make percpu-campaign MODE=percpu` on both lane VMs (about
  two hours each; the baseline half saved the jemalloc soak as the cached
  baseline in the lane VMs' target volumes, which the deletion discarded,
  so the percpu half will soak both allocators again unless the VMs are
  recreated and `make soak-baseline` is run first), then the comparison of
  the rows task 8 names (mstress x86_64 RSS +16.7%, alloc-testN, larson64,
  the calloc-zero campaign under the key, the Valkey and Ruby suites, the
  idle-thread soak's arena count and RSS). Tagged pointers stay the
  hardware item of task 6 (no GCP CPU exposes MTE, LAM or UAI).
