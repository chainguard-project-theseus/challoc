## Done

- Phase 2.5 task 8: the section-abort test proves the retry or skips with
  the host's shape (2e5bec6). Lane D's k68-amd64 flake
  (`bench/results/2026-09-20-phase-2.5-acceptance.md` 1b.2 (c), "no
  section was aborted under 23636 signals") was a probabilistic bound: the
  test inferred a signal inside a section from the process-wide
  `rseq::aborts()` delta after a fixed 400 ms. Now the `SIGUSR1` handler
  (`SA_SIGINFO`) reads the interrupted program counter from its ucontext,
  filled after the kernel's rseq fixup, and counts a signal as an aborted
  section exactly when the four bytes before that counter are `rseq::SIG`
  (the word before every `abort_ip`): the worker's own witness, no
  contribution from other tests or plain preemptions. The signaller keeps
  the 400 ms stress window, extends it only while no abort was witnessed
  (cap 4 s), then prints `percpu: skip: no abort in N signals (D
  delivered) over M ms on K allowed CPUs (U usable, P possible), the
  worker ran W ms for O operations: cannot prove the retry path here` and
  returns; slot conservation is asserted on every run, and where aborts
  are witnessed the abort handlers' counter (94fa9aa) must have moved.
  Same asm on both `mm_cid` implementations; the fixup at signal delivery
  predates both. Verified on the C4 (7.0-1011-gcp): `make test` green
  (410 + 120 unit/api, cgroup, churn, lazy, pagemap), the test alone ten
  times in a container held to 2 CPUs (2064..2281 signal-aborted
  sections each, equal to the process-wide count), and once on 1 CPU
  showing the flake's shape (68 of 39138 signals delivered, the worker
  ran 200 of 400 ms, 2 sections caught). Also in 2e5bec6, from lane C's
  7.2 Chainguard OS run:
  `api::mallctl::tests::arena_keys_create_lookup_purge_decay_and_destroy`
  raced another api test's `arenas.create` between the `arenas.narenas`
  read and `arena.{narenas+1}.purge` (the moved count's new all-arenas
  alias answers 0); the pair is retried until the count did not move
  across it and asserts ENOENT then, the far-range probes unchanged.

## Design decisions

- Phase 2.5 task 8: the abort test's witness is the signal handler's
  `ucontext` program counter compared with the signature word, not a
  test-only looped section: it keeps proving the production sections'
  retry paths (the kernel moved the worker to their own `abort_ip`), and
  it costs no asm. The counter check is `rseq::aborts() >= 1`, not `>=
  in_section`: a signal that finds the worker already at an abort
  handler's entry (moved there by an earlier fixup, the increment not
  yet run) counts once more than the handler runs (716 against 712 in
  one `make test` on the C4), and the aarch64 increment is not atomic.
