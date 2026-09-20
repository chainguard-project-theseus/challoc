## Done

- Phase 2.5 task 14 (lane E, 2026-09-20, second iteration), the same-host
  THP A/B the morning file asked for: done, results
  `bench/results/2026-09-20-aarch64-cgos-thp-ab.md`, script
  `bench/gce/thp-ab.sh` (`prep|run`), commit 13f3b9e. One cell,
  `cgos-arm64` (`challoc-lane-e-matrix-cgos-arm64`, c4a-standard-8 Axion,
  `kirkland-server-hypervisor-arm64-20260916-1358`, kernel 6.18.49, THP
  `enabled [always]` / `defrag [madvise]` as shipped and not written),
  tree `f2d9558`, natively, valkey1 / valkey1-free / larson16 under
  challoc default (`thp:never` = `MADV_NOHUGEPAGE`), `thp:auto` (no advice,
  the pre-task-16 regions), `thp:always`, `thp:never,thp_process:advised`
  and jemalloc, 7 interleaved timed rounds after a warm-up, per run the
  peak Rss / AnonHugePages (smaps_rollup at 200 ms), THP_enabled and the
  `thp_*` vmstat deltas; 120/120 runs exit 0. Medians (M ops/s) valkey1:
  a 8.048, b 8.983, c 9.288, d 8.418, jemalloc 9.592; valkey1-free: a
  8.251, b 8.975, c 9.352, d 8.036, j 9.072; larson16: a 307.9, b 307.2,
  c 307.0, d 303.6, j 325.8. The VM was deleted at the end.

## Design decisions

- Phase 2.5 task 16 (lane E, 2026-09-20), what the A/B says about the
  `thp` default on an `always` host, for the human's item 2 of the
  2026-09-20 decisions bullet (numbers, not a verdict): the task 16
  advice alone (`thp:never` vs `thp:auto`, same binary, same host) costs
  the Valkey model -10.4% (valkey1) / -8.1% (valkey1-free) and nothing on
  larson16, for no RSS change (584.9 MiB either way; the 32 MiB of huge
  pages the pre-task-16 tree took were the model's tables in challoc's
  huge region, 20 more PMD faults per run), and it is the whole
  +6.4 -> +11.7 / +4.2 -> +12.9 movement between the two cell days
  (against jemalloc here: `thp:auto` +6.3% / +1.1%, the default +16.1% /
  +9.0%). `thp:always` is parity with jemalloc (+3.2% / -3.1%, inside the
  spread) for +9.5% peak RSS on that heap (640 MiB, 636 MiB of
  AnonHugePages, 281 deferred splits per run) and +39.8% on larson16's
  retained memory (175 -> 245 MiB; jemalloc 270). `thp_process:advised`
  on top of the default is nil (+4.6% / -2.6% / -1.4%, the lists overlap
  over their whole range): the kernel takes the prctl (`PR_GET_THP_DISABLE`
  3, `thp_except_advised=yes`, the report line `host always process
  advised: PR_THP_DISABLE_EXCEPT_ADVISED in force`), `THP_enabled` in
  `/proc/<pid>/status` still reads 1 on 6.18, and with (a) already
  advising every challoc region it only removes the 2..4 MiB the program's
  own mappings took. A fourth option is visible in the pair: `never` on
  the class regions and the huge runs left eligible (the 32 MiB that moved
  are huge runs), lane B's file.

## Notes for the human

- Phase 2.5 task 14 (lane E, 2026-09-20): what to check by hand: (1)
  `bench/gce/challoc-bench` `matrix_lab_dest` now picks the login user by
  image family as `matrix_cell` does (`matrix ssh cgos-arm64` used to try
  the OS Login user on the server-hypervisor image and was refused); a
  shared file, the change is 13 lines in one function, the Docker cells'
  path unchanged; (2) two C tests report failures under a non-default
  `MALLOC_CONF` and are the tests' own default-conf expectations, not the
  library: `tests/c/thp.c process` asserts `!nh` because it assumes
  `thp:auto` next to the prctl (this run paired it with `thp:never`), and
  `tests/c/features.c` checks `collapse=unprobed` (under `thp:auto|always`
  the line reads `collapse=2M`) and `PR_GET_THP_DISABLE == 0` (3 with the
  prctl in force); `make test` runs both under the default conf and is not
  affected; lane B's tests, a note only; (3) the 7-round Valkey lists span
  12..18% on this 8-vCPU shape, so the (a)/(b) gap is read from the
  interleaved medians, not any one pair; the cells' 3-round tier could not
  have read it; (4) files touched outside `src/`: `bench/gce/challoc-bench`
  (the fix), `bench/gce/thp-ab.sh` (new), the results file, one paragraph
  of `bench/results/2026-09-20-chainguard-os-cells.md` section 5 (the
  pointer); nothing under `src/`, no scorecard file.
