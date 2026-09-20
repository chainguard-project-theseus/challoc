## Done

- Phase 2.5 task 8 (lane A, 2026-09-20), the rseq probe test hardened:
  done, commit 8ea835a (97ad715 before the rebase onto main 2a0fffb). `rseq::tests::probe_reads_the_registered_area`
  spawned as many spinners as CPUs and asserted more than one `mm_cid`
  after 50 ms, which depended on the scheduler spreading them: lane E read
  `1 distinct mm_cid values over 8 busy threads` on the first `make test`
  of every Chainguard OS cell (8 vCPUs, podman default profile; 45/45
  alone, 8/9 in the whole binary, 540/0 under `--privileged`:
  `bench/results/2026-09-20-chainguard-os-cells.md` section 3). Now one
  thread per CPU of the allowed mask (`sched_getaffinity`, at most 64),
  each pinned with `sched_setaffinity`, all reading `cpu_id` and `mm_cid`
  until every pinned thread has shown its own id or two seconds pass;
  deterministic on both `mm_cid` implementations (6.18's recycled ids,
  6.19+'s per-task/per-CPU rewrite): a pinned thread's `cpu_id` reads its
  CPU on every load (the hard assertion: the probe reads the registered,
  kernel-written area) and two pinned threads running at once hold two
  ids. Where the check cannot be made (one CPU allowed, a pin refused, or
  no two spinners overlapped in the window) the test prints the reason
  and skips that half, never passes it. Reference VMs: `16 distinct
  mm_cid values over 16 threads pinned to CPUs [0..15]` on both (`make
  test` 408 + 120 + 23, `EXIT 0`, both VMs); six whole-binary runs on the
  x86_64 VM restricted to CPUs 0-7 under 16 busy loops on the same CPUs
  (the cell's shape) read `8 distinct` 6/6; the Mac's Docker VM takes the
  `no-mm_cid` branch. The Chainguard OS cells themselves are lane E's to
  re-run (no cgos host was reachable from this lane).
- Phase 2.5 task 11 (lane A, 2026-09-20), the claim side and the plan's
  acceptance: done as far as the lane goes, commits ac31001 (the
  claim-side wake and the rate-sized prefault pipeline; be7e74c before
  the rebase onto main 2a0fffb, the hash the soak files carry) and
  c66048b (the pipeline takes the class lock once per slab); results
  `bench/results/2026-09-20-aarch64-task11-prefault-claim.md` with the
  soak files beside it (`...-task11-prefault-soak-{default,prefault}-
  {soak.md,soak.csv,faults.csv,perf.csv}` for aarch64 and x86_64). The
  claim side reviewed (Design decisions), both hooks kept on the burst
  probe's numbers, `quick` go on both archs for the default library
  against the merged base (aarch64 every row within 0.5%, x86_64 within
  its one-round spread with the per-operation instruction counts equal)
  and for the opt-in against the thread alone; the DURATION 1200 soak on
  both reference VMs, default against `prefault:true,background_thread:
  true`: the main thread's write-phase page faults -83% on both archs
  (157.5 k -> 27.0 k on the C4A, 153.2 k -> 26.2 k on the C4; process
  fault exceptions -82..-86%), write-phase p99 flat on x86_64 and +0.024
  ms (3 histogram steps) with valkey-benchmark -2.1% on aarch64 in the one
  run each (inside a soak's spread but the direction a long class-lock
  hold gives, hence c66048b; re-checked at DURATION 600 with c66048b on
  the C4A: throughput +0.1% / +0.3%, p99 1.063 / 0.959 ms both ways to the
  step, faults still -82%; the results file, section 6c), delete phase identical, peak RSS +0.7..0.8% (+4.2..
  4.6 MiB), RSS after settle +2.6..3.7%. The scorecard is the
  coordinator's (the default library is unchanged in behaviour and
  within noise on `quick` and `quick-perf`). The `prefault` default is
  the human's decision: the recommendation with the numbers is the
  results file's section 8.
- Phase 2.5 task 7 (lane A, 2026-09-20), the T2A `make test` SIGILL in
  `sys::pac::tests::sign_auth_round_trip` (lane D's acceptance, row 2a):
  done, commit ec20b39. The test's "not available" branch called `sign`,
  `auth` and `strip` to check the identity, which emit `pacda`/`autda`/
  `xpacd` on every aarch64 build (UNDEFINED on the Altra); the identity
  check is now compiled for the other architectures alone and the
  aarch64 branch prints the reason and asserts `fpac()` false. The
  library itself never reaches the three without the probe (`pac::mask`'s
  `xpacd` is behind `Shared::pac()`, checked by
  `heap::tests` forced-off test). Proven on the C4A and the Mac container
  (the available branch, unchanged, green) and on the C4 (compiled out);
  the T2A re-run is lane D's (no Altra was reachable from this lane).
- Phase 2.5 task 8 (lane A, 2026-09-20), `pac_pointers:true` with
  `percpu_arena:percpu` SIGILL on FEAT_FPAC (lane D's campaign, section
  1.2; acceptance row 4a): done, commit 11155f3. The fault was in
  `rseq::push_batch`'s aarch64 section: the modifier register was
  advanced to the write position (`slots + len * 8`) before the `pacda`,
  so a batch pushed onto a non-empty stack (a free on the same CPU
  between a refill's empty pop and its push, or a flush's push-back onto
  a stack another thread had pushed to) was signed under another
  modifier than the stack base every pop and `take_all` authenticate
  with; the section now keeps the base in its own register and stores
  through a second one. Tests: `heap::tests::percpu_batch_onto_a_non_
  empty_stack_authenticates` (the sections driven directly on a fresh
  `Shared` of the mode; skipped without `mm_cid`), `c_tls_threads_with_
  pac_pointers` now runs `tls_threads` under the key with and without
  `percpu_arena:percpu`, `c_percpu_both_ways` adds the mode with
  `pac_pointers:true`; `run_bin_env` clears `CHALLOC_CONF` for the C
  programs (section 1.1 of lane D's file: a suite run under the
  environment key turned every both-ways test one way). Verified on the
  C4A: `make test` green and `tests/c/percpu percpu` / `tls_threads`
  under both keys five and six times each (the results file, section 7);
  the thread path did not change (the section's other instructions are
  the same; the x86_64 section has no PAC). Follow-up 0823a00: the unit
  test builds its own `Shared` with `pac_pointers:true` (`shared_percpu`
  leaves the key off, so its first run drove the sections plain).

## Design decisions

- Phase 2.5 task 11 (lane A, 2026-09-20), the claim side reviewed against
  lane B's `Purged`-slab decision: confirmed, no claim-code change
  needed for correctness. `take` reports a prefaulted `Purged` slab zero
  under `purged_is_zero` (its pages are the kernel's fresh zero pages,
  populated or touched with a zero) and dirty under `lazy_free` (a
  `memset` in `calloc`, no error); `finish`'s `check_zero` covers the
  pick; the compaction claim (`pop_compact`, `claim_random`) goes
  through the same `plan`/`next_slab` and takes partial slabs first, so
  it only reaches a prefaulted slab when the class has no room, and it
  does not call `note_refill`, so a class in compact mode is never
  prefaulted (right: compaction consumes holes, not fresh slabs); the
  idle-group adoption moves a prefaulted slab as any `Purged` one; a
  sub-page slab group (`slabs_per_page > 1`, 64 KiB pages) can be
  discarded by `purge_group` with a prefaulted neighbour inside it, which
  undoes the prefault but keeps the memory zero (a cost, not an error).
  Two things the design cannot do without a descriptor bit, recorded
  rather than fixed: a truly purged (`MADV_DONTNEED`) slab and a
  prefaulted one are indistinguishable (`Purged`, `dirty_at` 0), so with
  both in the set `next_slab` takes the lower address, which may be the
  non-resident one; and `stats.purged` counts the resident prefaulted
  slabs as purged.
- Phase 2.5 task 11 (lane A, 2026-09-20), both claim-side hooks kept,
  behind the same key (`prefault:true` with the background thread), on
  the burst probe's numbers (the results file, section 3): the wake from
  `plan` when a refill takes the arena's last idle slab of a class
  (`purge::prefault::note_taken`, counted as `taken`), and the pipeline
  sized to the class's refills since the last pass (`prefault_delta` in
  slots, `prefault_ahead` keeps `ceil(delta / nslots)` slabs idle, at
  least 1 and at most `MAX_AHEAD` = 8). The plan's "one slab per class"
  is the floor; a class taking several slabs per millisecond in Valkey's
  fill burst (350 MiB in a few seconds) got every other one from lane
  B's pass (main thread 95.3 k -> 59.5 k faults over the fill), the
  pipeline alone gives 24.0 k, the wake alone 59.5 k (with one slab per
  pass the pass is already running as fast as it can), both together
  16.5 k (-83% against the default). The cost is bounded RSS during a
  burst: at most 8 slabs per `(arena, class)` that refills at that rate
  (+0.6 MiB VmHWM on the probe). `next_slab` was split into its sets
  half and `next_fresh_slab` so the prefault step never takes one of the
  arena's own idle slabs out of the sets (the first form spun). The
  pipeline takes the class lock once per slab (c66048b): the populate
  runs under the lock while the slab is unpublished (lane B's decision),
  and eight populates in one hold is a hold a refill of the class on the
  main thread waits through; per slab the hold is lane B's.
- Phase 2.5 task 11 (lane A, 2026-09-20), the burst probe as the hook
  attribution: `valkey-benchmark -t set -d 512 -r 500000 -n 5000000 -P 16
  -c 50` against a fresh server (the soak's first write run), the main
  thread's `minflt` over the run, three interleaved rounds per variant;
  the variants are quantum8 builds of the same tree with one constant
  changed (`MAX_AHEAD` 1; the `plan` wake compiled out), swapped in at
  the server's rpath. A finding on the way: the reference VM's Valkey
  build linked a quantum8 library from before lane B's commit (the soak
  script checks the build exists and never rebuilds it), so the first
  probe read no prefault at all; `make valkey STAGE=build` before any
  soak that follows a library change.

## Notes for the human

- Phase 2.5 task 11 (lane A, 2026-09-20), `prefault:true` alone does
  nothing in Valkey: the gate `Config::prefault_active()` is evaluated
  once at init from the configuration, so a server that turns the
  background thread on afterwards through `mallctl("background_thread")`
  (Valkey does at startup) leaves the pass off unless `MALLOC_CONF` also
  says `background_thread:true`. Every soak here used
  `prefault:true,background_thread:true`. If `prefault` becomes a
  default this is moot; if it stays an opt-in, the `background_thread`
  mallctl setter should re-evaluate the gate (`src/api/mallctl.rs` and
  `purge::prefault::configure`, lane B's files).
- Phase 2.5 task 11 (lane A, 2026-09-20), files touched outside the
  lane's partition, for the coordinator's eye: `src/purge.rs` (the
  prefault module: `MAX_AHEAD`, `note_taken`/`TAKEN`, the pass loop over
  `prefault_delta`/`prefault_ahead`, `Stats.taken`; `mod tests` made
  `pub(crate)` with `serial()` so the class test can hold the same lock;
  lane B's `wakes == 2` assertion relaxed to `>= 2`, other tests' fresh
  commits move the process-wide counter while the pass is on),
  `src/class.rs`'s commit-path half (`prefault_next` now the ahead-of-one
  form over `prefault_one`, plus `prefault_delta`, `prefault_ahead`),
  `src/api/print.rs` (the `Prefault:` line's `taken N` tail),
  `tests/c/prefault.c` (the pipeline check `1..8` where it asserted
  exactly one slab). `src/rseq.rs`, `src/class.rs`'s claim code, no
  `heap.rs` change.
