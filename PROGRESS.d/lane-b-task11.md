# Lane B: Phase 2.5 task 11 (prefaulted fresh slabs), state

## Done

- Nothing committed. Not started beyond the reading below; no code, no
  probe, no half-wired piece (lane rule: nothing lands that is not complete
  and tested on the lane's VM).

## Notes for the human

- The task as written ("the background thread commits and
  `MADV_POPULATE_WRITE`s the next fresh slab of each class that refilled
  since the last pass, bounded by one slab per class and by the cgroup
  headroom") needs three things lane B cannot touch: which slab a class
  takes next (`ClassState`'s lowest `Fresh` slab, `src/class.rs`), the
  commit of that slab under the class lock (`class.rs`), and a per-class
  "refilled since the last pass" mark set on the refill path
  (`class.rs`/`heap.rs`). The purge/background-thread side (a pass in
  `purge::background::main` after `decay`, the `MADV_POPULATE_WRITE` probe
  in `sys/features.rs` with a touch fallback, the headroom bound from
  `policy`) is lane B's and is small; the class side decides whether the
  prefault lands on the slab the next refill really takes.
- Suggested shape when a class-side owner exists: `ClassState::prefault_hint()
  -> Option<(addr, len)>` under the class lock (commit the lowest `Fresh`
  slab, mark it `Fresh`+prefaulted so `take` still reports it zero), a
  `refilled: AtomicBool` per class set by `plan`, cleared by the pass; the
  pass issues `MADV_POPULATE_WRITE` (probed at init, `EINVAL` on < 5.14 →
  fallback: write one byte per page) bounded by one slab per class and by
  `policy` (skip in the scaled and immediate bands). Measure: the Valkey
  soak's write-phase p99 and the perf sidecar's page-fault count on the
  main thread; scorecard within noise.
