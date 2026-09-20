## Done

- Phase 2.5 acceptance (lane D), the closing floor check at e3e5fc3
  (2026-09-20, 09:50..10:15 UTC): done, commit 8d18f1d. Measurement only, no
  `src/` change. `bench/gce/challoc-bench matrix hwe-amd64 k68-amd64` at
  e3e5fc3 (`MATRIX_JOBS=2`, `MATRIX_DATE=2026-09-20-lane-d-closing`), the
  two cells nobody had re-run at the fixed tree: hwe-amd64 (7.0.0-31-generic)
  `tests=553/0 fuzz=PASS cgroup=PASS privileged=PASS:553/0 outcome=PASS`
  (909 s), the a999097 privileged red
  `sys::features::linux_tests::probe_prints_and_matches_kernel_version`
  now ok with the branch executing (that run's features line
  `mseal=yes process_madvise=pidfd_self process_madvise_free=yes`);
  k68-amd64 (6.8.0-139-generic) `tests=553/0 fuzz=PASS cgroup=PASS
  privileged=PASS:553/0 outcome=PASS` (910 s), the a999097 flake
  `heap::linux_tests::percpu_sections_abort_and_retry_under_signals` ok
  in both runs with a witnessed abort, its lines read off the cell's
  `~/logs/test.log` and `test-privileged.log` through `matrix ssh` before
  deletion: `percpu: 510 sections aborted by the signal and retried, 510
  process-wide, under 30520 signals (22736 delivered) over 400 ms on 4
  allowed CPUs (4 usable, 4 possible), the worker ran 333 ms for 1235712
  operations` (default) and `242 ... 243 process-wide, under 26791 signals
  (10321 delivered)` (privileged); no skip line on either cell. Logs
  `bench/results/matrix/2026-09-20-lane-d-closing-{hwe-amd64,k68-amd64}.log`.
  The library since a999097 checked identical two ways: every `src/` hunk
  of a999097..e3e5fc3 is inside a `#[cfg(test)]` module, and the release
  `libchalloc.so` built in the dev container from a999097, 51f2cae and
  e3e5fc3 (same path, same target dir, rustc 1.98.1) is byte-identical
  (sha256 192901b0...4f39, 1 219 424 bytes). The checklist
  `bench/results/2026-09-20-phase-2.5-acceptance.md` gains the section
  "At e3e5fc3 (closing)": per cell of the nine, the run that proves it
  green (five at a999097 from this lane's afternoon logs, k510-amd64 and
  64k-arm64 at 51f2cae from lane B's logs, hwe-amd64 and k68-amd64 today),
  the T2A at a999097 with the note that only tests changed since, the
  three Chainguard OS cells at a999097 (lane E's third run) with lane C's
  7.2 slice-test verification on top. Row 1 (the floor) re-verdicted
  **met** at e3e5fc3: every cell green at a tree whose library equals the
  accepted one and every test that was red has a green run of its fixed
  version on the cell that showed it; row 2a stays **met with findings**
  (nothing probed changed; the cost rows (iii), (iv) and the hardware
  directions (ii), (vi) open). The phase is not marked accepted; the
  coordinator does that. VMs: the two matrix cells deleted by the driver
  at 10:12 UTC, `matrix clean` none, `delete all` none, `gcloud compute
  instances list --filter=name~challoc-lane-d` empty; the reference pair
  not touched, no scorecard run.

## Notes for the human

- Phase 2.5 acceptance (lane D): what stays open after the closing check,
  with owners, in the checklist's "What remains, with an owner": row 2a
  (iii), (iv) cost rows (lane B, a privileged 7.0 cell); row 2a (ii), (vi)
  and row 4b hardware (WAITPKG `-metal` C3, MTE AmpereOne, LAM/UAI Granite
  Rapids or bare-metal Genoa; the human); the Chainguard OS cells'
  `--privileged` column at e3e5fc3, one `matrix cgos` run (lane E or the
  coordinator; task 14, outside row 1's criterion). The five cells cited
  from a999097 ran that tree's versions of the four changed tests on the
  identical library, not e3e5fc3's; recorded in the section, not re-run.
