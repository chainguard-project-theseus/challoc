## Done

- Phase 2.5 task 11 (lane B, test fix): the `populate_write` block of
  `sys::features::linux_tests::probe_prints_and_matches_kernel_version`
  maps its own fresh anonymous page instead of reusing the scratch page the
  `process_madvise` block has just written, dropped, read back and written
  again, so its "fresh scratch page resident before the populate"
  assertion (features.rs:1714, `left 1 right 0`; lane E's
  `bench/results/2026-09-20-chainguard-os-cells.md`, third run: 3/3 on both
  Chainguard OS 6.18 hosts under `--privileged` or `seccomp=unconfined`)
  no longer depends on block order; every assertion keeps its meaning and
  the page is unmapped in the block. Commit b17ed3a. Verified both ways on
  challoc-lane-b-arm64 (C4A, 7.0.0-1011-gcp): default seccomp `make test`
  410/0 + 120/0 + 23/0 with the branch skipped (`process_madvise=no`);
  `docker run --privileged` (`process_madvise=pidfd_self
  process_madvise_free=yes`) 410/0 + 120/0 with the fix, and the old test
  reproduced 2/1 at features.rs:1714 on the same VM before it. Mac dev
  container (6.10 linuxkit) `make test` 410/0 + 120/0 + 23/0 and the
  privileged features tests 15/0; 6.10 predates self-targeted
  `process_madvise(MADV_DONTNEED)` (6.13), so the branch cannot run there
  even privileged. VM deleted.

## Notes for the human

- Phase 2.5 task 11 (lane B): `challoc-bench sync`/`provision` exit 0 when
  ssh fails (a rsync "unexpected end of file" after a host-key mismatch for
  a reused NAT IP, and "Connection refused" from a VM that has not finished
  booting); the failure is only visible in the log. Not fixed here (lane B
  does not own `bench/gce/`); `ssh-keygen -R <ip> -f
  ~/.ssh/google_compute_known_hosts` and a second `provision` cleared it.
