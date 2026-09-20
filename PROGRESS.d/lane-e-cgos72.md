## Done

- Phase 2.5 task 14 (lane E), second run 2026-09-20: the `cgos72-amd64`
  cell (kernel 7.2) and the merged tree on the two 6.18 cells. Code
  e0fdf71 (the cell: `kirkland-workstation-7dot2-amd64` in `wolfi-vm`,
  image `kirkland-workstation-7dot2-amd64-20260920-0028` created
  2026-09-20 00:50 UTC, joins `MATRIX_CGOS_CELLS`; the `cgos` mode's
  scorecard file takes the cell prefix, `scorecard-cgos72-amd64.md`, so
  kernels never share a trend file; README cell table, runbook image
  table) and a52c4d2 (a workstation image takes OS Login: the guest agent
  creates no user from `ssh-keys` metadata; `challoc-bench` decides by
  image family). Measurements 513bfbf:
  `bench/results/2026-09-20-chainguard-os-cells.md`, the matrix logs
  `bench/results/matrix/2026-09-20-cgos{-arm64,-amd64,72-amd64}.log` and
  `-extra.log`, scorecard lines in `scorecard-cgos-{arm64,amd64}.md` and
  the new `scorecard-cgos72-amd64.md`; `docs/research.md`'s three 7.2
  rows and the runbook's kernel table carry the measured clauses. Three
  VMs (`challoc-lane-e-cgos-{arm64,amd64}`, `challoc-lane-e-cgos72-amd64`,
  us-east4-a, 04:50..05:47 UTC), all deleted. Per cell: `make test`
  540/0 under podman `--privileged` on all three hosts, `make
  test-cgroup` PASS on all three, scorecard PASS; under the default
  seccomp profile 399/1 on all three, the one failure lane A's
  `rseq::tests::probe_reads_the_registered_area` (Notes). The 512m churn
  (`purge 400 100`, lane B's `f78601e`): 12/12 `ok` per 6.18 host (2 in
  the cell, 10 after), 2/2 on 7.2, against 11 stalls in 45 yesterday.
  The 7.2 items (section 6): (a) khugepaged mTHP collapse confirmed on
  challoc's regions, advised and unadvised, with a collapse/split loop
  against the purge path at the accelerated khugepaged pace; (b)
  cache-aware scheduling exposes `/sys/kernel/debug/sched/llc_balancing/`
  (root, `enabled=1` default), inert on the one-LLC shape, rows recorded
  on/off/on/off, no verdict; (c) `CONFIG_RSEQ_SLICE_EXTENSION` is on
  (`rseq/slice_ext_nsec=5000`, `__rseq_size` 33), `Slice extension: on`
  natively and in both podman modes, `slice_ext.c` green both ways. No
  metal.

## Design decisions

- Phase 2.5 task 14 (lane E): `cgos72-amd64` is in `MATRIX_CGOS_CELLS`
  (the three cells run concurrently under `MATRIX_JOBS=3`, so `matrix
  cgos` costs the same wall time) and writes its own
  `scorecard-cgos72-amd64.md` rather than the 6.18 file: one trend file
  per kernel, distinguishable without reading the note column.
- Phase 2.5 task 14 (lane E): the cell keeps the podman shim on the
  workstation image although that image ships Docker 29.8.1 with
  `dockerd` running, so the three Chainguard OS hosts run one container
  runtime; a Docker variant of the cell is a one-line provisioning change
  if wanted.

## Notes for the human

- Phase 2.5 task 14 (lane E): **`make test` under podman's default
  seccomp profile is red on every Chainguard OS host today** on lane A's
  `rseq::tests::probe_reads_the_registered_area` (`1 distinct mm_cid
  values over 8 busy threads`; 3/3 cells, once in nine whole-binary
  reruns, 45/45 when run alone, on 6.18 and on 7.2's per-task `mm_cid`
  alike): the assertion `n > 1` over eight 50 ms spinners loses to the
  other 399 tests' threads on 8 vCPUs. Not seccomp, not the kernel, not
  the library; a longer window, a retry or a serial run fixes the test.
- Phase 2.5 task 14 (lane E): the 7.2 khugepaged mTHP collapse is real
  and, at an accelerated khugepaged pace, costs every allocator 25..40%
  on the Valkey model through a collapse/split loop with the purge path
  (challoc `thp:always` 18.2 -> 9.0 M ops/s with only the 64 KiB order
  enabled; jemalloc 14.6 -> 8.9 under `64kB=always`). At the shipped pace
  (4096 pages per 10 s) it is 160x slower and was not measured over a
  soak; task 9's "dTLB gain without fault-time cost" needs that soak on
  a 7.2 host before it is a design input. Lane B's `thp.rs` `auto` mode
  is satisfiable on 7.2 by root (`64kB=madvise`, `2048kB=never`).
- Phase 2.5 task 14 (lane E): the 6.18 scorecard lines moved on the
  Valkey rows against yesterday's (`valkey1` +6.4 -> +11.7 arm64, +8.0 ->
  +12.9 amd64; `valkey1-free` +4.2 -> +12.9, -1.2 -> +6.9) while the 7.2
  host (THP `madvise`) reads -1.2% / -7.2%: the THP-`always` mechanism of
  yesterday's section 6, possibly widened by task 16's `MADV_NOHUGEPAGE`
  under `thp:never` on an `always` host (the one merged change on that
  mechanism); a same-host A/B on a 6.18 cell would settle it (lane B or
  D).
- Phase 2.5 task 14 (lane E): the workstation image differs from the
  server image in ways the runbook now records: OS Login only (no user
  from `ssh-keys` metadata), Docker + `make` + `gcc` + `git` on the host,
  THP `madvise`, `CONFIG_ANON_VMA_NAME` on (`anon_name=yes`), no
  `/proc/config.gz`, `/var` 19 GiB. Two rows were added to the human's
  image table in the runbook (marked "added 2026-09-20, lane E").
- Phase 2.5 task 14 (lane E): files touched outside `src/` and the lane's
  own list: `docs/research.md` (the three 7.2 rows, appended clauses
  only), `docs/runbooks/chainguard-os-gce.md` (a preamble bullet and two
  image-table rows), `docs/plan.md` (a paragraph appended to task 14).
  The two 6.18 `-extra.log` files are transcribed from the session's ssh
  reads (the VMs were deleted before a misnamed copy was noticed); their
  header says so.
