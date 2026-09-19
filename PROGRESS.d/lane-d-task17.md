## Done

- Phase 2.5 task 17 (lane D) DONE, measurement hygiene from the kernel
  table, no `src/` change, three commits on `loop/lane-d`: a5a05c3
  (`memory.peak` reset per phase: `bench/probes/cgpeak.c` sidecar,
  `bench/lib.sh cgroup_peak_*`, Makefile `CGROUP_PEAK=1`, the Valkey soak's
  three per-phase rows beside VmHWM, the Ruby soak's start..compact and
  compact..end rows from `soak.rb`, the campaign's host-side per-step peak),
  faaadd4 (`CHALLOC_THP_COUNTERS=1`: `/proc/vmstat thp_*` and
  `hugepages-<size>kB/stats/*` deltas around every run in `quick`, `ab` and
  `quick-perf`), 26e897e (`tests/c/pagemap_guard.c` through
  `scripts/pagemap-guard-test.sh` and `make test-pagemap-guard` in `make
  test`: guard slabs of four classes checked both ways through
  `ioctl(PAGEMAP_SCAN)` without touching them). Runs on the lane's VMs
  (`challoc-lane-d-{arm64,amd64}`, kernel 7.0.0-1011-gcp, created for the
  task, deleted at its end) and the Mac container (6.10) for the fallback
  branches: `bench/results/2026-09-19-lane-d-task17-hygiene.md` (the three
  items, every run, the acceptance lines),
  `bench/results/2026-09-19-aarch64-lane-d-task17-thp-counters.md` (the
  counters next to task 9's dTLB column in three sysfs states, with the two
  `ab` files), `2026-09-19-aarch64-lane-d-task17-soak/` (and `-run1/`),
  `...-task17-ruby-soak*`, `...-task17-percpu-baseline.md`. `make test` on
  the amd64 VM with the new target: 523 passed, 0 failed, container tests
  ok, `pagemap_guard` on/off ok.

## Design decisions

- Phase 2.5 task 17 (lane D): the `memory.peak` reset lives in a sidecar
  (`bench/probes/cgpeak.c`) that holds one file description for the run,
  because the 6.12 reset is scoped to the description that wrote it (a
  shell's `echo reset > memory.peak` followed by `cat` shows the birth peak
  again; measured on 7.0), and it is driven by the phase file the Valkey soak
  already writes, with an acknowledgement so the phase's load starts after
  the reset. Reason: the alternative (a host-side reset triggered from the
  container) needs a second channel; the sidecar reuses the one the soak has.
- Phase 2.5 task 17 (lane D): the write happens inside the container, made
  possible by `make ... CGROUP_PEAK=1` (`--cgroupns=host` and a read-write
  bind mount of `/sys/fs/cgroup`), opt-in and probed by the write's success,
  never on by default: Docker mounts the tree read-only and the nsdelegate
  boundary refuses the write from inside a cgroup namespace, and the flags
  change the container's view of its cgroup path (`0::/system.slice/...`
  instead of `0::/`), which the container tests should not see unasked. The
  campaign, which runs on the VM host, reads the fresh containers'
  `memory.peak` from the host instead (no reset needed for a fresh cgroup).
- Phase 2.5 task 17 (lane D): the Valkey soak builds the jemalloc server
  before any soak runs and the sidecar records the cgroup's page cache
  (`memory.stat file`) at each phase end, after the first run of the column
  charged a ~300 MiB build cache to jemalloc's phases; the cgroup peak counts
  every process of the container and its page cache, which is the figure the
  plan asked for, and the header now says so with the numbers.
- Phase 2.5 task 17 (lane D): `tests/c/pagemap_guard.c` expects
  `PAGE_IS_SWAPPED` beside `PAGE_IS_GUARD` on a guard page (the kernel's
  `pagemap_page_category` classifies the guard marker as a swap-type PTE
  first: `0x112` on 7.0), and its 6.13..6.15 fallback checks "not present,
  a swap-type entry" rather than "neither present nor swapped"; every
  capability is probed by the ioctl's own answer (ENOTTY, EINVAL on an
  unknown category bit), never by a kernel version.
- Phase 2.5 task 17 (lane D): the guard test is a Makefile target
  (`test-pagemap-guard`, hooked into `make test` after the cgroup targets)
  rather than a `src/api/ctests.rs` driver, because lane D edits nothing
  under `src/`.

## Notes for the human

- Phase 2.5 task 17 (lane D), for the coordinator: folding
  `tests/c/pagemap_guard.c` into the cargo-driven C test set is one
  `run_c("pagemap_guard", &["on"])` driver plus a `run_bin_env` for `off`
  under `MALLOC_CONF=guards:false` in `src/api/ctests.rs`; the Makefile
  target can then stay as the standalone form or go.
- Phase 2.5 task 17 (lane D), a latent harness bug fixed in a5a05c3: a plain
  `make ruby-soak` (without `COMMIT=`) exited silently on a clean tree
  because a bare `COMMIT=...$(tree_dirty && echo ...)` assignment took the
  substitution's status under `set -e`; the campaign always passed
  `COMMIT=`, so the acceptance runs were unaffected.
- Phase 2.5 task 17 (lane D), two readings for task 9's file, not
  regressions: under `thp:auto` in the auto sysfs state valkey1 takes 2788
  64 KiB folios = 174 MiB of its 585 MiB heap, because only the classes with
  64 KiB slabs are advised (the 1024-byte class has a 4 KiB slab), which
  bounds what the mode can do for a Valkey-shaped heap; under `thp:always`
  with the PMD, 280..282 of the 318 PMD pages are split again within a 10 s
  run by the delete phase's purges (`thp_split_pmd`), the counter form of
  the RSS price lane B named. And `thp_collapse_alloc=1` per process start
  whenever `thp` is not `never` is the `MADV_COLLAPSE` probe.
- Phase 2.5 task 17 (lane D), not run: no 6.13..6.15 host exists in the
  lane's cells, so the guard test's "ioctl without the category" branch was
  written from the kernel source, not exercised; the 5.10 / 6.1 / 6.8 cells
  would take the "no ioctl" and "no guard slab" branches the 6.10 run covers
  in substance. A matrix pass with `make test` on task 3's cells will run
  the target on each.
