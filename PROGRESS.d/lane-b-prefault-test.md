## Done

- Phase 2.5 task 11 (lane B, test fix): `purge::tests::prefault_populate_and_wakes_are_counted`
  follows the kernel's page size and the `MADV_POPULATE_WRITE` probe (51f2cae).
  Lane D's reading (bench/results/2026-09-20-phase-2.5-acceptance.md, "At a999097",
  row 1) confirmed in the code: the test hard-coded `page = 4096` and
  `reset_for_test(true, true)` forced the populate word on whatever the probe
  said, so on k510-amd64 (5.10, no `MADV_POPULATE_WRITE`, which is 5.14) and on
  64k-arm64 (a 4 KiB-aligned address is not page-aligned) the `madvise` failed
  with EINVAL and `fallbacks` read 1 against the asserted 0. The library is
  right (`api::init` hands the probe's `features.populate_write` to
  `prefault::configure`; the pass populates page-aligned slabs; the touch
  fallback counts and completes), so purge.rs changes only in its tests: a
  cfg'd `prefault_env()` reads `sys::features::page_size()` and runs the real
  `probe_populate_write(page)` on Linux (4 KiB and "no" off Linux, where
  `os::populate_write` always fails), the pass is configured in the probe's
  direction and asserts no fallback there (the advice works, or the touch is
  the populate), then a third leg forces the populate word and asserts one
  fallback with the bytes still counted exactly where the probe said no (the
  touch fallback contract), a zero buffer zero throughout; the wake
  assertions are unchanged; the test prints the probed direction and the
  forced leg's count. The advice test's misplaced doc comment moved back
  onto `advice_follows_headroom_band_key_and_explicit_purges`.
  Verified: host `cargo test --features std` 318/0 (`page_size=4096
  populate_write=no (probed)`, forced `fallbacks=1 (expected 1)`); Mac
  container `make test` 553/0 (410 + 120 + 23; 6.10, `page_size=4096
  populate_write=yes (probed)`, forced `fallbacks=0 (expected 0)`);
  `challoc-bench matrix k510-amd64 64k-arm64` under `challoc-lane-b`:
  k510-amd64 (Debian 11, 5.10.0-46-cloud-amd64, 4 KiB) `tests=553/0 fuzz=PASS
  cgroup=PASS outcome=PASS`, the test printing `page_size=4096
  populate_write=no (probed)` and forced `fallbacks=1 (expected 1)`;
  64k-arm64 (Ubuntu 24.04, 6.8.0-139-generic-64k, 64 KiB) `tests=553/0
  fuzz=PASS cgroup=PASS outcome=PASS`, printing `page_size=65536
  populate_write=yes (probed)` and forced `fallbacks=0 (expected 0)`
  (bench/results/matrix/2026-09-20-lane-b-{k510-amd64,64k-arm64}.log; the
  printed lines were read off the cells' `~/logs/test.log` before deletion,
  the fetched log keeps only the tail). Cells deleted, no lane-b VM remains.

## Notes for the human

- Phase 2.5 task 11 (lane B): the second test fix lane D named, (b)
  `sys::features::linux_tests::probe_prints_and_matches_kernel_version`'s
  `populate_write` block under `--privileged` on 6.16+ (the `process_madvise`
  block's `MADV_FREE`d scratch page still resident), is not in this commit;
  it is the next lane B iteration. Lane D's re-run of k510-amd64 and 64k-arm64
  is done here under the lane-b prefix (553/0 both); hwe-amd64 privileged
  waits for (b).
