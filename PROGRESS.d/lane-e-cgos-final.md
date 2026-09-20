## Done

- Phase 2.5 task 14 (lane E, 2026-09-20, third run): the re-verification
  lane A asked for after `8ea835a`: the three Chainguard OS cells
  (`cgos-arm64`, `cgos-amd64`, `cgos72-amd64`) at `a999097`, commit
  e609926 (`bench/results/2026-09-20-chainguard-os-cells.md`, section
  "At a999097 (third run)"; logs
  `bench/results/matrix/2026-09-20-<cell>-final.log`; one line each in
  `bench/results/scorecard-cgos-{arm64,amd64}.md` and
  `scorecard-cgos72-amd64.md`). `rseq::tests::probe_reads_the_registered_area`
  passed in every whole-binary run on the three hosts under podman's
  default profile and under `--privileged` (`8 distinct mm_cid values
  over 8 threads pinned to CPUs [0..7] of 8 possible`, 319..382964 reads)
  and 10/10 alone on cgos-amd64 (1348..1765 reads), never skipped: the
  morning's open item is closed. `make test` under the default profile
  553/0 on both 6.18 hosts; churn 0 red of 34 (10x 512m re-check
  included). Two other tests fail, neither lane E's nor the library's:
  `sys::features::linux_tests::probe_prints_and_matches_kernel_version`
  under `--privileged` and `seccomp=unconfined` on all three hosts
  (deterministic, a test-ordering bug: the `process_madvise` branch
  dirties the page the `populate_write` block asserts fresh, lane B),
  and `slice::tests::grants_are_yielded_under_oversubscription` on the
  7.2 host (a 0-grants-in-3-s flake 3/13 in the binary and alone, and
  alone always its teardown `assert_eq!(on(), before.1)`, lane C).

## Notes for the human

- Phase 2.5 task 14 (lane E, third run): the `outcome=FAIL` of all three
  cells at `a999097` is the features test under `--privileged` (3/3
  hosts) and, on cgos72, the slice test's 0-grant window in the default
  profile `make test` (its rerun read 553/0). Both are visible only
  without the default seccomp profile (a) or on a kernel with
  `CONFIG_RSEQ_SLICE_EXTENSION` (b), so the reference Docker cells never
  see them; the 7.2 cell and any native or metal run will until lane B
  (`src/sys/features.rs`, the `populate_write` block after the
  `process_madvise` branch) and lane C (`src/slice.rs`, the 3 s window
  and the teardown's `before.1`) fix them. Details and every run's line
  in the results file.
- Phase 2.5 task 14 (lane E, third run): a floor reading from the 7.2
  host: podman's default profile filters the `rseq_slice_yield` syscall
  (`yield_syscall=false`), the module's `sched_yield` fallback carries
  the grants (`6 grants yielded via sched_yield` in the green rerun), and
  under `--privileged` / `seccomp=unconfined` the syscall itself is used.
- Phase 2.5 task 14 (lane E, third run): the x86_64 `larson10` column
  moved +9.7 -> +12.3 (6.18) and +9.1 -> +15.7 (7.2) between `e0fdf71`
  and `a999097` on the same hosts and images, arm64 +4.3 -> +4.8; the
  3-round tier does not separate that from its spread; one look at the
  reference `larson10` row for `a999097` is the check, not a cgos action.
- Phase 2.5 task 14 (lane E, third run): what to check by hand: files
  touched are the results file, the three `-final.log` files, the three
  cgos scorecard files and this note; nothing under `src/` or
  `bench/gce/`; the reference scorecards untouched. The VMs were created
  by hand and adopted as in the morning (so they outlived the driver for
  the follow-up runs); all three deleted at 09:01 UTC, `matrix clean` and
  `delete all` confirm none left.
