## Done

- Phase 2.5 acceptance (lane D), second pass, 2026-09-20: done, 31e8a25, 9455485, e902399, 6ddaed8.
  `bench/results/2026-09-20-phase-2.5-acceptance.md` re-verdicts the plan's
  "Acceptance for the phase" list at 0c60b1a from today's evidence on the
  lane's own VMs (both lane VMs, ten kernel-matrix cells, one T2A lab),
  all deleted at the end. Row 1 (the floor) **met**: all nine kernel cells
  `tests=540/0 fuzz=PASS cgroup=PASS` at 0c60b1a with lane B's fixes in
  (the Debian churn and the 64 KiB `c_thp_always` closed; the two c3
  cells also privileged 540/0); gcp-amd64's first run lost
  `rseq::tests::probe_reads_the_registered_area` ("several running
  threads share one mm_cid": four spinners on 4 vCPUs beside the model
  checker, serialised) and its rerun was green (logs
  `bench/results/matrix/2026-09-20-lane-d-<cell>.log`, `-gcp-amd64-run1.log`).
  Row 2a **not yet**: on the Altra (T2A, `pac=no`) the shipped library's
  gate b43532f holds (`heap_off_pac_executes_no_pac_instruction` passes,
  `make test` 399 + 117 + 23 = 539/0 with one test skipped) but
  `sys::pac::tests::sign_auth_round_trip` still SIGILLs: its "not
  available" branch calls `sign`/`auth`/`strip` expecting the identity
  and they emit the PAC instructions unconditionally
  (`bench/results/2026-09-20-lane-d-t2a-make-test.log`, three parts);
  rseq `mm_cid` closed (both ways on the 7.0 / 6.8-64k cells with
  `mm_cid=yes` and the Debian cells with `rseq_size=20 mm_cid=no`). Row
  2b **met with one attributed shift** (the coordinator's
  `2026-09-20-aarch64-larson1-drift.md`). Rows 2c, 3, 4b unchanged
  (pointers). Row 4a **met with findings**, from the task 8 campaign's
  percpu half (below). Row 5 **met on the reference kernel**, the x86_64
  Valkey write-phase reading (yesterday's default run, +1.5 / +4.9%
  behind) still a single reading. The phase is not marked accepted by the
  lane. Also `bench/gce/challoc-bench` (9455485): the matrix driver's exit
  trap died with `pids: unbound variable` under `set -u` (a local of
  `cmd_matrix`, gone at script exit), so its sweep never ran; `${pids:-}`.

- Phase 2.5 task 8 campaign (lane D), the percpu half: done, e902399.
  `make percpu-campaign MODE=percpu LANE=lane-d DATE=2026-09-20` on both
  lane VMs at 0c60b1a, compared row by row with the 2026-09-19 baseline
  half in `bench/results/2026-09-20-lane-d-percpu-campaign.md` (per-step
  files `2026-09-20-{aarch64,x86_64}-lane-d-percpu-percpu*`). The mode
  moves the memory rows the way task 8 intended: mstress x86_64 peak RSS
  +18.8% (default) -> +11.5% vs jemalloc (aarch64 +9.9% -> +9.7%), still
  outside the 10% band; larson64 RSS -43% on both for +21% / +30%
  throughput; the Ruby idle soak (16 workers + 48 idle threads) from
  +117% / +301% to **-41% / -4%** of jemalloc's peak with `Arenas: 16` on
  16 CPUs (64 before); the 16-worker Ruby soak from +147% to +10.5% (C4A)
  / +48% (C4) at t=600. The time rows pay: larson +7..+30%, valkey1 +5.4%
  (C4A) / **+34.1%** (C4), xmalloc +12..18%, the Valkey soak's delete phase
  +3..+11% req/s behind jemalloc (aarch64 memtier delete p99 +30%), RSS
  criteria met; erb_render's lead 14% -> 8%. Correctness under the key:
  calloc-zero 100/100 and 10/10 standalone, container tests all ok, Valkey
  memefficiency (both) and full (x86_64) green, Ruby test-all the nine
  environmental, both Ruby soaks clean. Two findings: (1) **`pac_pointers:true`
  together with `percpu_arena:percpu` SIGILLs on the FEAT_FPAC C4A**
  (`tests/c/tls_threads` 3 of 6 runs, `tests/c/percpu percpu` 2 of 2;
  either key alone clean, x86_64 clean; `...-percpu-percpu-test.log` part
  1), lane A's; (2) `make test` cannot run under `CHALLOC_CONF`:
  `config::linux_tests::load_with_a_clean_environment_gives_the_defaults`
  asserts it unset and `api::ctests::c_percpu_both_ways`'s `off` direction
  is overridden by the environment (the campaign's `test` step stopped at
  the first; the supplement skipped it and read 399 + 115/2 + 23 aarch64,
  399 + 116/1 + 23 x86_64). The default is the human's decision; the rows
  are in the campaign file's section 0 and 6.

## Notes for the human

- Phase 2.5 acceptance (lane D, 2026-09-20): what stands between the
  checklist and "accepted", each with an owner: (a) lane A, the Altra
  test `sys::pac::tests::sign_auth_round_trip` (make `sign`/`auth`/`strip`
  the identity when `available()` is false, or skip the branch); (b) lane
  A, the `pac_pointers:true` + `percpu_arena:percpu` fault on FEAT_FPAC
  (a per-CPU cache entry authenticated under another modifier or
  unsigned); (c) lane A, `rseq::tests::probe_reads_the_registered_area`'s
  "more than one mm_cid" assertion on a small loaded host; (d) lane A or
  C, the two tests that assume no `CHALLOC_CONF` (clear it for the child
  / skip when set), or lane D runs the campaign's `test` step without the
  key; (e) you, the `percpu_arena` default with the campaign's rows in
  front of you (memory rows in, time rows out, section 6 of
  `2026-09-20-lane-d-percpu-campaign.md`); (f) coordinator, one more
  scorecard line to confirm the aarch64 larson1 band (+7.5..+8.5%) and the
  x86_64 larson16 row; (g) optional, lane D, a second default Valkey soak
  on the C4 for the write-phase reading (50 min). Rows 2a(ii), (vi) and 4b
  stay hardware-blocked (WAITPKG, MTE/LAM/UAI).
- Phase 2.5 task 8 campaign (lane D, 2026-09-20), a reading caveat in the
  idle soak tables: the jemalloc run's final sample is missing in both
  `...-ruby-idle.md` files (`0.0 | - | -` at t=300, and a stale "after
  the last full GC" row); `bench/ruby-soak.sh` is lane D's tool and the
  `peak` and `t=300` rows are the ones to read. Also today's C4 ran every
  timed row slower than yesterday's for both allocators (alloc-test1 2.85
  -> 3.40 s, jemalloc 2.70 -> 3.10), so only same-run ratios compare across
  the two days.
