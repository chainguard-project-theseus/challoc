## Done

- Phase 2.5 task 10 (lane B), floor fix: done, f78601e. Lane D's acceptance
  found the container churn (`purge 400 100` in 512 MiB) failing on the
  Debian cells (5.10 x86_64, 6.1 x86_64 and arm64; RSS 217..349 MiB at
  2 s). Reproduced on lane B's own lab cells at 9a9c635: 7 of 16 runs
  fail on 6.1 arm64 (c4a-standard-4), 5 of 16 on 5.10 x86_64
  (e2-standard-4), 0 of 16 on the 16-vCPU 7.0 lane VM. Mechanism, shown
  by the advice counters now on the churn's policy line (pre-fix library,
  new test: `lazy_purges 18 eager_purges 1` in a failing run, `22 / 3` in
  a passing one): the three classes' walks come due a few ms apart; when
  they straddle a periodic tick the first tick returns one class eagerly
  (the band was `Scaled` at 403 MiB), usage drops under 70%, the next
  sample puts the policy in `Low` and the remaining 200..300 MiB go out
  with `MADV_FREE`, resident and charged, the cap met in the allocator's
  accounting and not the cgroup's. Timing, not the kernel: `MADV_FREE`
  pages stay charged on every kernel. Fix: `Policy::eager_excess`, an
  excess over the retention cap first seen at 70% or above is returned
  eagerly until the retention is within the cap; `Policy::headroom` is
  what `purge::set_headroom` mirrors; `advice_reason` gains `returning
  the excess over the cap`. After: 24 of 24 on both lab cells, `make
  test-cgroup` (all limits, the lazy rows included) green on the 7.0 VM,
  the lazy test unchanged (an excess first seen under 70% stays lazy).
  Design 8 and 11. `bench/results/2026-09-19-lane-b-floor-fixes.md`
  sections 1, 2.

- Phase 2.5 task 9 (lane B), floor fix: done, 4b1e92d. `c_thp_always`
  failed on the 64 KiB cell because `thp:always` committed and advised
  512 MiB chunks (the PMD at 64 KiB pages; 1.7 GiB for an 8 MiB test
  heap, no folio from the kernel) and the test matched `2M` inside
  `512M`. `thp::decide` takes the chunk path only for a PMD of at most
  2 MiB (`LG_PMD_CHUNK_MAX`), else the folio path over the enabled sizes
  below the PMD, inert with reason `PmdTooLarge` when none; `tests/c/thp.c`
  reads `hpage_pmd_size` and compares tokens. 64k cell `make test` 379 +
  115 + 23, 0 failures; 4 KiB expectations unchanged (7.0 VM: mode 2,
  `AnonHugePages 10240 kB`). Results file section 3.

- Phase 2.5 task 16 (lane B), first half: done, 71f54e0. `thp:never`
  issues `MADV_NOHUGEPAGE` on the three reservations at
  `layout::Regions::reserve` (`thp::advise_reservation`; `force_never`
  advises the recorded reservations too), gated on the key, nothing on a
  kernel without THP; `auto` and `always` unchanged. Both-ways test in
  `tests/c/thp.c never` (sysfs global mode, `nh` on the class and huge
  regions, THPeligible, and on an `always` host AnonHugePages after 8 MiB
  of small blocks plus an 8 MiB huge run). Measured before/after on the
  native-`always` Debian 6.1 cell and on the 7.0 VM with the global mode
  set to `always` for the run: the huge run's AnonHugePages 8192 kB ->
  0 kB, THPeligible 1 -> 0, `nh` 0 -> 1 on both regions; the class
  region was 0 kB either way (groups are committed a few slabs at a time,
  no whole 2 MiB read-write range yet, THPeligible 1 before). Design 11,
  12, 17; README row. Results file section 4. The prctl half is below.

## Design decisions

- Phase 2.5 task 10 (lane B): the coordinator's candidate (a) (switch to
  `MADV_DONTNEED` when `memory.current` stays above the band after a lazy
  pass) did not match the mechanism: the band really was `Low` after the
  first eager walk and the mirror was fresh. The rule taken is about the
  cap: an excess over the retention cap is returned with the advice of
  the band it was first seen in, eager until the retention is within the
  cap, because the cap is a promise about resident, charged memory that
  a lazy purge does not keep; an excess first seen under 70% (the lazy
  test) stays lazy. Alternatives not taken: "the cap is always eager"
  (would have made the lazy container test and the plan's task 10
  wording wrong: 200 MiB freed at 39% would return eagerly); candidates
  (b)/(c) (probe or gate on `memory.stat` `lazyfree`), because no kernel
  uncharges `MADV_FREE`d pages before reclaim and the line exists on 5.10
  and 6.1 too, so neither distinguishes the cells. Task 10's results
  file (`2026-09-19-lane-b-two-advice-purge.md`) said the 512m churn ran
  "at 89.8% usage, scaled band, eager": it runs at 79% and its eager
  passes were one-tick luck on the 16-vCPU VMs; the new file says so.

- Phase 2.5 task 9 (lane B): `always` on a PMD over 2 MiB (16 and 64 KiB
  pages) is the folio path, not a refusal of the key: an operator asking
  for `always` on a 64 KiB host with `hugepages-2048kB` enabled gets
  2 MiB folios on 2 MiB slabs and up, which is the useful reading; with
  nothing below the PMD enabled the mode is off and the report says why
  (`PmdTooLarge`). Huge runs of 512 MiB or more are not advised in that
  case (mode off): a 512 MiB THP per run is not a saving anyone asked
  for. `auto` is unchanged at 64 KiB (the PMD gating rule applies as at
  4 KiB).

- Phase 2.5 task 16 (lane B): the never-advice is gated on the *key*
  (`never`, or the forced `never` of design 11), not on `Mode::Off`: an
  `auto` the kernel's sysfs table made inert (no sub-PMD size, or the
  PMD madvise-gated) keeps its regions eligible for the folios the host
  may enable later, since `MADV_NOHUGEPAGE` forbids mTHP folios as well
  as PMD pages. Consequence, recorded in design 17: on an `always` host
  `auto`'s inert regions and `always`'s unadvised ones still get the
  host's PMD pages (the huge run under `auto` shows 8192 kB there). The
  second half of the task, designed here and not implemented (it needs a
  config key, lane C's `config.rs`): a key `thp_process` with values
  `default` (nothing) and `advised`; under `advised` init calls
  `prctl(PR_SET_THP_DISABLE, 1, PR_THP_DISABLE_EXCEPT_ADVISED)` (6.18+,
  probed by the call's return: `EINVAL` below) before the reservations,
  so the process gets huge pages only where challoc `MADV_HUGEPAGE`d
  (`auto`'s folios, `always`'s chunks, huge runs), whatever the host's
  global mode; the feature line gains `thp_process=<mode>` and the
  report's `thp:` line shows the host's global mode (`sys::features::Thp
  .enabled`, already read) and whether the prctl is in force; the
  both-ways container test flips the global mode as `tests/c/thp.c`
  does and checks AnonHugePages of an unadvised region under `auto`.
  Off by default because it is process-wide (it changes what the host's
  `always` means for every mapping of the process, libc's and the
  program's own included), and `never` does not need it: the
  `MADV_NOHUGEPAGE` half covers challoc's regions on every kernel.
  Chainguard OS ships `always` with the 2 MiB size on `inherit`: with
  this commit `thp:never` (the default) means never there; the cgos
  four-way RSS rows (plan task 14, lane E) should be re-read at or after
  71f54e0.

## Notes for the human

- Phase 2.5 task 16 (lane B): `src/layout.rs` gained the three-line call
  site of `thp::advise_reservation` in `Regions::reserve` (after the
  anon-name calls). `layout.rs` is in no lane's partition list; the
  coordinator should check it at merge. `thp::Stats` gained
  `nohuge_ranges` (the struct only): the `stats.thp.*` mallctl key and
  the report's `thp:` line live in `src/api/` (lane A), so the counter
  is not exported yet.

- Phase 2.5 task 10 (lane B): the churn's failure rate depends on the
  host's speed (0 of 16 on 16 vCPUs, 7 of 16 on 4), so a floor cell
  with few vCPUs is the right place to keep running it; `make
  test-cgroup-churn` runs it once per limit. `tests/c/cgroup_churn.c`
  now prints the advice counters on its policy line so a future failure
  says whether the memory was purged lazily or not at all.

- Phase 2.5 task 9 (lane B): the 64k cell's `make test` count is 379 +
  115 + 23 = 517 at 71f54e0 (513 at 675f357, plus two unit tests from lane B's earlier
  commits fc081b1 and 9a9c635 and this iteration's two: policy 1, thp 1).
