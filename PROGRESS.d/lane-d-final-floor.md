## Done

- Phase 2.5 acceptance (lane D), the floor re-read at the final tree a999097
  (2026-09-20, afternoon): done, commits d17e106 (the `bench/ruby-soak.sh`
  final-fullgc row fix with its 60 s re-run) and 5bb5cdc (the nine kernel cells at
  once plus one rerun of each red cell, the Altra, the PAC + per-CPU
  combination on three hosts, the checklist re-verdicted). Rows at a999097
  (`bench/results/2026-09-20-phase-2.5-acceptance.md`, sections 0, 1b, 4.1,
  "At a999097"): row 1 **not yet** (fuzz and the container tests green on all
  nine kernels; `make test` 553/0 on six cells, k68-amd64 on its rerun after
  one `percpu_sections_abort_and_retry_under_signals` flake; 409/1 on
  k510-amd64 and 64k-arm64 both runs, `purge::tests::prefault_populate_and_wakes_are_counted`,
  and on hwe-amd64's privileged run both runs,
  `sys::features::linux_tests::probe_prints_and_matches_kernel_version`,
  reproduced under `--privileged` on both lane VMs at 7.0.0-1011-gcp; both
  tests are lane B's 7ab6916 and both readings say test, not library); row
  2a **met with findings** (T2A 553/0, the PAC test executes no PAC
  instruction, ec20b39; hardware rows open); row 2b **met with one attributed
  shift**, the a999097 line is the coordinator's; row 4a **met** (the
  combination 30/30 clean, 11155f3; `make test` 553/0 on both lane VMs).
  The phase is not marked accepted; the coordinator does that.

## Notes for the human

- Phase 2.5 acceptance (lane D, 2026-09-20 afternoon): what to check by hand:
  (1) files outside the lane's partition: none under `src/`; `bench/ruby-soak.sh`
  (lane D's tool, one expression) and the checklist, logs and soak files under
  `bench/results/`. (2) Lane B owns the two deterministic reds (the readings
  in section 1b.2 (a) and (b): a test that hard-codes `page = 4096` and forces
  `populate_write` on; a scratch page reused after `MADV_FREE` where
  `process_madvise` works); lane A the one flake (1b.2 (c)); lane D re-runs
  k510-amd64, 64k-arm64 and hwe-amd64 (and a privileged 7.0 cell) once the
  fix is on `main`. (3) The a999097 scorecard line against the band (aarch64
  larson1 +7.5..+8.5%, x86_64 larson16 +9.6..+12.9%) is the coordinator's
  reading for row 2b; this lane ran no scorecard and no `ab` at a999097.
  (4) A driver nit: `challoc-bench create all && provision all && sync all`
  reported rc=0 through a failed rsync (stale host keys for recycled IPs;
  `provision` also ran before sshd was up on fresh VMs), so a chained lane
  bring-up should check the sync's own output. (5) The morning's Ruby idle
  soak tables' jemalloc `final-fullgc` row was a table gap, not a missing
  sample: the `after the last full GC (final)` rows were right.
