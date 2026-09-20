## Done

- Phase 2.5 task 15 (lane C, test fix): the `slice::tests` on a kernel
  with `CONFIG_RSEQ_SLICE_EXTENSION` on, `5c421f4`. Lane E's cgos72-amd64
  cell (Chainguard OS kernel 7.2.3, the first host in the matrix with the
  option; `bench/results/2026-09-20-chainguard-os-cells.md`, "At a999097
  (third run)") found two faults in the tests, invisible on the reference
  VMs (Ubuntu 7.0 has the option compiled out): the teardown
  `configure(before.0 != Key, false)` turned the module on when a test ran
  alone (module `Unprobed`, off, a state `configure` cannot reproduce), so
  `assert_eq!(on(), before.1)` failed 7 runs of 10 and only the whole
  binary, where `config::load` had already turned it on, hid it; and
  `grants_are_yielded_under_oversubscription` counted coincidences (32
  threads on one lock, 20 us holds, a 3 s window: 0..6 yields in ~10k
  holds, zero on 3 runs of 13) because an unlock that wakes a sleeper ends
  the grant with its own `FUTEX_WAKE` before `released` reads `granted`.
  Now the tests snapshot and restore the switches themselves (`STATE`,
  `VERDICT`, `FORCED`; `PROBED`, `OFFSET`, `YIELD_SCHED` stay by design)
  and are order-independent, and the grants test makes the scheduler want
  to preempt every holder (two hogs per CPU, a hold run past the slice on
  the thread's own lock, no syscall inside) while the holder watches its
  own `granted` byte and releases inside the grant; bounded in holds and
  work, not seconds; a kernel with the option that grants nothing prints a
  skip line, never a pass, and the request path is asserted on regardless.
  Verified on a `matrix lab cgos72-amd64` VM under the lane's prefix
  (`challoc-lane-c-matrix-cgos72-amd64`, c4-standard-8, 7.2.3-r0-desktop-7.2,
  `slice_ext_nsec=5000`, `preempt=(full) lazy`), inside podman under the
  default profile: the `slice::` tests alone 10 of 10 passed, each run 23
  holds, 22..23 granted during the hold (after 436..1920 rounds of 100
  multiply-adds, i.e. inside the first slice), 22..23 yielded via
  `sched_yield`, 0.06..0.47 s; the whole `make test` once with the same
  code 410 + 120 + 23 passed, 0 failed, every churn/lazy `verdict=ok`
  (the test inside the whole binary: 23 granted, 23 yielded); one
  `--privileged` run alone: 23 granted, 23 yielded via `rseq_slice_yield`.
  Mac container (6.10, no extension): `slice::` alone 3 passed and the
  whole `make test` 410 + 120 + 23 passed, 0 failed, the test reading
  `320 holds ... 0 with the request set, 0 granted ... state
  no-slice_ctrl`. Logs (not committed): `.tmp-matrix-lab/2026-09-20-cgos72-amd64-{slice10,maketest-summary}.log`
  in the lane worktree. VM deleted (`matrix clean`, `delete all`);
  `gcloud compute instances list --filter=name~challoc-lane-c` empty.

## Design decisions

- Phase 2.5 task 15 (lane C): the grants test proves the mechanism per
  hold rather than over a window. Two things the 7.2 host showed, both in
  the test's comments: the kernel grants once per syscall-free stretch of a
  hold (a hold that polled `granted` every 13 us, the debug-build cost of
  1000 dependent multiply-adds, missed its grant and was then preempted
  ~350 more times without another; the poll is now 100 multiply-adds,
  ~1.3 us in debug), and an unlock that wakes a futex sleeper ends the
  grant before `released` can see it, so the test holds the thread's own
  lock. The restore is a test-only snapshot of three statics, not another
  `configure`, because `Unprobed` cannot be reached through the API. The
  test-only diagnostics (`nonvoluntary_ctxt_switches` per capped hold, the
  rounds-to-grant range) stay in the printed line: they are what tells
  "preempted but not granted" from "not preempted" when the skip line
  fires on some other kernel.

## Notes for the human

- Phase 2.5 task 15 (lane C): one of four `make test` runs on the 7.2 host
  today failed `api::mallctl::tests::arena_keys_create_lookup_purge_decay_and_destroy`
  (`src/api/mallctl.rs:1599`, `arena.{narenas+1}.purge` returned 0 where
  `ENOENT` was expected: an arena existed one past `narenas`, i.e. another
  api test's `arenas.create` raced it); the api binary passed 120/0 on the
  two reruns and in the other whole runs. Not lane C's file; a
  parallel-test ordering race worth a look by the owner of `src/api/`.
