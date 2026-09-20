## Done

- Phase 2.5 task 16 (lane E, 2026-09-20), the re-measurement of the
  human's `thp` default decision on the cgos cell: done, f1b45ef. The
  same-host A/B of `2026-09-20-aarch64-cgos-thp-ab.md` repeated on
  `cgos-arm64` (Axion c4a-standard-8, Chainguard OS 6.18.49, THP
  `always`, image `kirkland-server-hypervisor-arm64-20260916-1358`) at the
  merged tree 6ea842d with the shipped default (4678fcd: `thp:never`
  advising the class superblock and the metadata region only, the huge
  region following the host), `thp:auto`, `thp:always` and jemalloc, 7
  interleaved rounds, medians, natively under LD_PRELOAD:
  `bench/results/2026-09-20-aarch64-cgos-thp-decision.md`. The default
  reads within the spread of `thp:auto` on every row (-1.4% / -0.7% /
  -0.2%) and +5.1% / +6.4% / +6.0% behind jemalloc on valkey1 /
  valkey1-free / larson16 (this morning's every-reservation `never`:
  +16.1% / +9.0% / +5.5%), with 32 MiB of `AnonHugePages` at the Valkey
  model's peak (`thp_fault_alloc` 29 per run, up from 12) and peak RSS
  unchanged (584.8 MiB; larson16 173.7 MiB, 2 MiB of AnonHugePages that
  are the program's own mappings, as this morning's (a)); `thp:always`
  still buys parity (-0.2% / +0.8%) for +9.5% RSS on the 585 MiB heap
  and +39.9% on larson's retained memory. `tests/c/thp.c never` on the
  host printed `AnonHugePages 0 kB (class region), 8192 kB (huge run)`,
  the report line `nohuge 2 (classes, metadata; huge follows host)`. The
  cell's own steps once at 6ea842d (`matrix-cell.sh ... cgos` on the same
  VM): `make test` 411 + 120 + 23 passed, 0 failed, in both the default
  and the `--privileged` podman profile; `test-cgroup` PASS; the
  `scorecard-cgos-arm64.md` line appended (valkey1 +6.3%, valkey1-free
  +5.9%, back to the 2026-09-19 pre-task-16 level from +11.7% / +12.9%);
  cell log `bench/results/matrix/2026-09-20-cgos-arm64-thp-decision.log`.
  `bench/gce/thp-ab.sh` takes its setting list from `SETS` now (default
  unchanged). Nothing under `src/`. VM deleted (`matrix clean`).

## Notes for the human

- Phase 2.5 task 16 (lane E, 2026-09-20): lane B's expectations hold
  except one figure. valkey1 about +6% behind jemalloc: +5.1% (yes).
  larson16 unmoved at +5..6%: +6.0% (yes). AnonHugePages about 32 MiB at
  the Valkey peak and the program's 2..4 MiB on larson16: yes. RSS
  unchanged: yes. valkey1-free about +1%: measured +6.4%, but `thp:auto`
  itself reads +5.8% in the same run (+1.1% this morning) with the two
  sorted lists overlapping over their whole range, so the default does
  read as (b) as intended and the +1% was that morning VM's jemalloc
  spread on that row, not a property of the default. Caveat for anyone
  comparing the two files: the afternoon VM's absolute level was 23..26%
  below the morning's on the Valkey rows (jemalloc 7.09 vs 9.59 M ops/s
  on valkey1; larson16 only -2.5%), a different Axion host under the
  same name, so only within-run ratios compare between them.
- Phase 2.5 task 16 (lane E): the cell run adopted the lab VM
  (`MATRIX_ADOPT`), whose name matches the matrix prefix, so the
  driver's `cmd_matrix` exit trap (`matrix_clean`) would have deleted it
  the moment the driver ended; the driver was detached after launching
  the VM-side unit and the polling, `~/logs` fetch, the scorecard file
  fetch and the log assembly were done by hand with the driver's own
  rsync. A driver change worth making: `matrix_clean` should skip
  `MATRIX_ADOPT`, or `cmd_matrix` should not sweep when a cell was
  adopted; not done here (lane D's file).
