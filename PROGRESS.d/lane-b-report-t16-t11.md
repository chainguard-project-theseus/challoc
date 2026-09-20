## Done

- Phase 2.5 task 15 (lane B, 2026-09-20), the `slice_ext=` key of the
  feature line: done, commit 7a42369 (code), results
  `bench/results/2026-09-20-lane-b-task15-16-report.md`. `sys::features::
  SliceExt` (`on`, `available`, `prctl-failed`, `kernel-disabled`,
  `no-slice_ctrl`, `no-rseq`) read through `slice::probe().verdict()` at
  `Features::probe` and folded with `slice::verdict()` after `config::load`
  (`Features::note_slice_ext`, one line in `api::init`); the word follows
  `mm_cid=`; `FeatureLine::CAP` 512 -> 640 (the longest line is 462 bytes
  with the three new keys). Both VMs (kernel 7.0.0-1011-gcp) read
  `slice_ext=kernel-disabled`, the Mac's 6.10 `no-slice_ctrl`; `on` and
  `available` are covered by the host tests (no kernel in reach has the
  option). Tests: `sys::features::tests::slice_ext_words_follow_the_verdicts`
  and the display/line tests, `sys::features::linux_tests` (against
  `slice::probe`), `api::print::tests::feature_line_slice_ext_agrees_with_
  the_slice_line`, `tests/c/features.c` (vocabulary, `rseq_size`, the
  `Slice extension:` line). README row and design 16/17 updated.

- Phase 2.5 task 16 (lane B, 2026-09-20), the prctl half: done, commit
  7a42369, results `bench/results/2026-09-20-lane-b-task15-16-report.md`.
  `thp_process:advised` (lane C's key) issues `prctl(PR_SET_THP_DISABLE, 1,
  PR_THP_DISABLE_EXCEPT_ADVISED)` once at init (`thp::configure_process`,
  called from `api::init` after `thp::configure`) where `sys::features::
  probe_thp_disable` found the kernel takes it (the call and its undo, only
  on a process whose `PR_GET_THP_DISABLE` reads 0; `EINVAL` before 6.18 is
  `no`; a parent's policy is `inherited` and kept); the feature line gains
  `thp_except_advised=yes|no|inherited` after `collapse`; the `thp:` report
  line ends `; host <global mode> process <default (nothing issued) |
  advised: PR_THP_DISABLE_EXCEPT_ADVISED in force | advised: skipped,
  <reason>>`, the JSON object gains `host` and `process`, `stats.thp.
  process` reads `thp::Process` (0..4). Both ways: the 7.0 VMs accept
  (`PR_GET_THP_DISABLE` 3, `stats.thp.process` 1, `THP_enabled` 1); the
  Mac's 6.10 refuses (`stats.thp.process` 2, the line says skipped with
  the reason, `PR_GET_THP_DISABLE` stays 0). `tests/c/thp.c process` under
  `thp_process:advised,thp:auto` (`api::ctests::c_thp_process`), every
  other mode checks nothing was issued; `thp::tests::process_outcome_
  follows_the_key_and_the_probe`; the Linux features test takes and undoes
  the flag once more where accepted. The `always`-host row (AnonHugePages 0
  in the unadvised regions with the prctl in force) needs a 6.18+ host
  whose global mode is `always`: the note below names the command for
  lane E's cgos cells; the Mac's 6.10 `always` host without the prctl shows
  the before figure (8192 kB in the unadvised huge run). README row, design
  11/16/17 updated.

- Phase 2.5 task 11 (lane B, 2026-09-20), the background-thread and
  commit-path half: done as far as lane B's files go, commit 7a42369,
  results `bench/results/2026-09-20-aarch64-lane-b-task11-prefault.md`
  (with four CSVs beside it). `purge::prefault::pass` after each decay
  pass of the background thread, re-armed every 1 ms while a pass finds a
  class that refilled (`prefault::POLL_NS`); `ClassState::prefault_next`
  (under the class lock: nothing when the arena has an `Empty` or `Purged`
  slab of the class; else the refill's own `next_slab`, `MADV_POPULATE_
  WRITE` or a touch per page over the slab while it is unpublished, then
  published `Purged` in the `purged` set; an adopted `Empty`/`Purged` slab
  goes back untouched); `prefault_due` over `ClassState::refilled` (the
  snapshot in the state's tail line); `Policy::prefault_allowed` (no limit
  or the `Low` band without an eager return in progress); the
  `populate_write` probe (`MADV_POPULATE_WRITE` + `mincore` on a scratch
  page) and key after `anon_name`; `SlabTable::commit_slab` wakes the
  thread per usable fresh slab (`prefault::note_commit`, a relaxed load
  when off); `stats.prefault.{enabled,populate_write,passes,slabs,bytes,
  skipped,fallbacks,wakes,active}`; the `Prefault:` report line; design
  8.2. Gate: `Config::prefault_active()` (lane C's key with the thread).
  Tests: `class::tests::prefault_next_commits_what_the_next_refill_takes`
  and `prefault_next_skips_guards_and_adopts_before_committing`, `policy::
  tests::prefault_allowed_only_with_headroom`, `purge::tests::prefault_
  populate_and_wakes_are_counted`, `tests/c/prefault.c` on/off/inert
  (`api::ctests::c_prefault_both_ways`: 16 of 16 pages of the next fresh
  64 KiB slab resident before any allocation touched it, the one after it
  0, zero, and the next 1024 allocations from it; nothing under the
  default). Numbers on the C4A: `quick` go for the library under the
  default (every row within 0.7% of HEAD) and for the opt-in against the
  thread alone (within 0.4% but valkey1 +2.2% one round, RSS +2..22 MiB:
  one resident slab per (arena, class) ahead of use); the Valkey soak
  (DURATION 600, default vs `prefault:true,background_thread:true`; Valkey
  turns the thread on itself): write-phase p99 unchanged (1.063 ms
  valkey-benchmark both ways, memtier 0.959 -> 0.951), throughput within
  0.3%, the main thread's write-phase minor faults 127.4 k -> 72.9 k
  (-43%; 157.2 k -> 96.1 k in total, -39%), 58.6 k of them taken by
  `challoc-bg`, the process's fault exceptions (perf) 123.6 k -> 71.2 k
  (-42%), peak RSS +0.7% (3.9 MiB). `make test` green on both VMs (408 +
  120 + 23 each) and the C tests in the Mac container. The plan's
  acceptance (scorecard within noise, the full protocol) is lane A's and
  the coordinator's after the merge; the claim-side hooks that would raise
  the burst hit rate are listed in the results file, section 6.

## Design decisions

- Phase 2.5 task 11 (lane B, 2026-09-20), what a committed-but-unclaimed
  prefaulted slab is to `next_slab` (lane C asked, for the model op): it is
  a `Purged` slab in the arena's `purged` set, state byte `Purged`,
  `dirty_at` 0, populated before it is published and while the class lock
  is held. Reasons, in order: (1) `next_slab` already takes the lowest
  `Purged` slab right after the lowest `Empty` one and before committing
  anything, so the refill takes the prefaulted slab with no change to the
  claim code; (2) a `Purged` slab reads zero, which a fresh populated slab
  does, so `take` keeps reporting the claim zero for `calloc` (under
  `purged_is_zero`; under `lazy_free` it is treated as dirty, a cost, not
  an error); (3) the decay walks read the `empty` set only, so the pages
  stay resident until the refill; (4) `transfer_group`'s consistency check
  (`Purged` <=> in the donor's `purged` set) and `next_idle_group`'s
  cursor rule both hold: the slab is below the cursor (`next_slab` moved
  `group_next` past it) and in the set, so an idle-group scan sees it as
  idle and another arena may adopt it, which is the same memory serving
  the same class one arena over. Not chosen: the `empty` set with state
  `Fresh` (`purge_empty` asserts `Empty` for every member of that set and
  the walks would purge it); `Empty` (a purge would undo the prefault and
  `take` would report it dirty); a new state (touches `slab.rs`, the
  model, `is_zero` and every match on `State`, for a word that only
  changes the bins table's `purged` column). The one inaccuracy accepted:
  the `purged` count and the RSS derived from slab counts do not know the
  slab is resident until it is taken. A slab the adoption yields as
  `Empty` or `Purged` is put back into its set untouched and nothing is
  prefaulted (the refill takes it). The model op for lane C: a background
  step "if no Empty/Purged: commit next (as the refill would), mark
  Purged, add to purged set", with the invariant that a `Purged` slab
  reads zero unchanged.

- Phase 2.5 task 11 (lane B, 2026-09-20), the populate happens under the
  class lock before the slab is published, not after: with the slab
  unpublished no refill can be writing into it, so the touch fallback is a
  plain write of zero over zero and `MADV_POPULATE_WRITE` never races a
  claim; the cost is one lock hold of a populate (a 64 KiB slab: one
  syscall installing 16 pages) on a class whose next refill would have
  taken those faults itself.

- Phase 2.5 task 11 (lane B, 2026-09-20), the wake: `SlabTable::commit_slab`
  wakes the background thread once per usable fresh slab it commits (a
  relaxed load when the key is off; a `futex_wake` on the commit path,
  which just did an `mprotect`), so the pass runs right after the refill
  that consumed the previous prefaulted slab and a thread sleeping with no
  decay deadline still runs it; `note_refill` was not made to wake (one
  syscall per refill would not be free). The pass's own commits wake
  nobody (`IN_PASS`).

- Phase 2.5 task 16 (lane B, 2026-09-20), the prctl probe is the call and
  its undo, only on a process whose `PR_GET_THP_DISABLE` reads 0: a
  process whose parent set a THP policy (inherited across fork and exec)
  is reported `inherited` and left alone, and `thp_process:advised` then
  issues nothing (the parent's policy wins over a library's key). Under
  `advised` the probe's set/undo pair is followed by the real set: three
  `prctl`s at init, none later.

- Phase 2.5 task 15 (lane B, 2026-09-20), the `slice_ext=` word is the
  kernel's answer, not the key's: `available` when the kernel has the
  option and `slice_ext:off` turned it off, `on` only when lock holders
  request it, and the probe's own reason (`kernel-disabled` on the 7.0
  GCE kernels, `no-slice_ctrl` before 7.0, `no-rseq`) otherwise; the same
  vocabulary as the `Slice extension:` line, checked against it by
  `tests/c/features.c` and `api::print`.

## Notes for the human

- Phase 2.5 task 16 (lane B, 2026-09-20), for lane E on the cgos cells
  (kernel 6.18, THP `always`): the `always`-host measurement of the prctl
  is one command per cell, from the cell's checkout in the container:
  `cargo test --features std -- --nocapture c_thp_process` (or `make test`,
  which includes it), and read the `thp process:` lines: `thp_except_advised=yes
  stats.thp.process=1 PR_GET_THP_DISABLE=3`, then `thp process: host
  always, prctl in force: AnonHugePages 0 kB (class region), 0 kB (huge
  run) ...: the both-ways row`. The same program without the key
  (`c_thp_auto`, `thp:auto` alone) prints the huge run's `AnonHugePages`
  under the host's `always` for the before figure (8192 kB on the Mac's
  6.10 `always` host, where the prctl is refused).

- Phase 2.5 task 11 (lane B, 2026-09-20), for the human and lane A: the
  first reading says the mechanism works as designed (39% of the Valkey
  main thread's page faults moved to the background thread, p99 flat,
  +0.7% peak RSS) and that the remaining 57% are the huge runs (which the
  pass does not touch) and the bursts a 1 ms poll cannot keep up with (a
  class consuming slabs faster than one per pass gets every other one).
  Two knobs are in lane A's and the coordinator's hands: a wake from the
  refill that takes a prefaulted slab (`take`, claim code) and more than
  one slab per class per pass; and whether `prefault:true` becomes a
  default, which would also mean running the background thread by default
  (Valkey and Ruby ask for it themselves; the `quick` rows do not). The
  DURATION 1200 protocol and the x86_64 soak were not run.

- Phase 2.5 tasks 15/16/11 (lane B, 2026-09-20), files touched outside the
  lane's partition, all wiring, for the coordinator's eye: `src/api/mod.rs`
  (`init`: `features.note_slice_ext()` after `config::load`, `thp::
  configure_process`, `purge::prefault::configure`), `src/api/print.rs`
  (the `slice_line` comment, the `thp:` line's tail, the JSON `thp` object,
  the `Prefault:` line and its test needle, one new test), `src/api/
  mallctl.rs` (`stats.thp.process`, `stats.prefault.*`), `src/api/
  ctests.rs` (the drivers `c_thp_process`, `c_prefault_both_ways`),
  `src/sys/mod.rs` and `src/sys/nr.rs` (`MADV_POPULATE_WRITE`, the three
  `PR_*THP_DISABLE*` constants, a `mincore` wrapper and its syscall
  numbers), `src/class.rs` outside the commit path: two `AtomicU64` fields
  carved from `ClassState::_tail` (the struct's size and asserts are
  unchanged) and `prefault_next` calling the private `next_slab`
  (unchanged) so the pass selects exactly what the refill would; `src/
  lock.rs`, `src/config.rs`, `src/heap.rs`, `src/arena.rs`, `src/slab.rs`,
  `src/slice.rs`, `src/model.rs`, `bench/gce/*` untouched. `FeatureLine::
  CAP` grew from 512 to 640 bytes (a static in `api/mod.rs` grows with
  it). Lane C's model op for the prefaulted slab is described in the
  Design decision above.
