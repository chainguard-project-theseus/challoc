You are one iteration of an unattended implementation loop for challoc. Nobody
is watching. Do exactly one unit of work, leave the repo in a committed,
consistent state, and exit.

Procedure:

1. Read CLAUDE.md, PROGRESS.md, and docs/plan.md. Do not read docs/research.md
   unless a task needs a source. Do not redo research.
2. Find the first task in docs/plan.md, in phase order, that PROGRESS.md does
   not list as done. If PROGRESS.md shows a task "in progress", continue it.
   Record it as "in progress" in PROGRESS.md before starting.
3. Implement that one task following docs/design.md. If the design is wrong
   or ambiguous on a point you hit, make the smallest reasonable decision,
   apply it, and record it under "Design decisions" in PROGRESS.md with a
   one-line reason. Amend docs/design.md only to fix an actual error.
   Before changing any fast path in src/heap.rs, src/class.rs, src/slab.rs
   or src/api/*.rs, the iteration must have a per-operation cycle
   attribution on the workload the change is meant to help (perf
   stat/annotate on the VM: `bench/gce/challoc-bench quick-perf <vm>` for
   larson1 and the Valkey model, `bench/probes/valkey-perf-sidecar.sh` for
   the soak, `make ruby-bench` rows for Ruby; or the sampler), recorded in
   the results file, and the change must name the mechanism it attacks
   (which load misses, which line, how many cycles per operation). larson
   counters do not stand in for Valkey or Ruby: larson's working set is a
   few MiB, Valkey's 550 MiB.
4. Build and run the task's tests inside the Linux dev container (see
   CLAUDE.md). Nothing runs on the macOS host except `cargo test --features std`
   for pure logic. If Dockerfile.dev does not exist yet, creating it is part
   of Phase 0 task 1. For any change that touches performance (src/heap.rs,
   src/class.rs, src/slab.rs, layout constants, cache policy) run the fast
   tier first: `make quick` on the Mac for plumbing, then
   `bench/gce/challoc-bench quick <arm64|amd64>` on the reference VM
   (bench/README.md). A variant that fails it (more than 5% behind its base
   on any row, or the churn probe not compacting) is abandoned or reworked;
   never take it to the protocol tier. Only a variant that passes `quick`
   gets the protocol run (`challoc-bench ab ...`, 5 rounds, same-run
   comparison) whose table goes into bench/results/. If CHALLOC_GCE_WARM is
   set the VMs are already running and belong to the driving session: do not
   start or stop them. The fast tier includes the Valkey model row
   (`valkey1`); a change aimed at Valkey or Ruby is judged on that row and
   on the target's own rows (`make valkey-soak`/`soak-quick`, `make
   ruby-bench`), not on larson alone.
5. When the task's criteria pass, mark it done in PROGRESS.md with the commit
   hash, and commit everything with a message "Phase N task M: <summary>".
   Never leave uncommitted work. Do not commit .ralph/ or target/. After
   landing a change that touches the library, run the scorecard on both VMs
   (`bench/gce/challoc-bench scorecard arm64` and `... amd64`, or `make
   scorecard` in a container) and commit the appended lines of
   `bench/results/scorecard-<arch>.md` with the PROGRESS commit; `bench/scorecard.sh
   --render` shows the trend.
6. If you finish a phase, run that phase's acceptance criteria. Record the
   results, including benchmark numbers in bench/results/, and only then move
   the phase to done.
7. Stop conditions. If Phases 0 through 2 are all done with acceptance
   criteria passing, set the first line of PROGRESS.md to "STATUS: DONE". If
   you are blocked by something only a human can resolve, such as a missing
   credential, a design question with materially different outcomes, or a
   test that fails for reasons outside this repo after two honest attempts,
   write the specifics under "Blocked" in PROGRESS.md and set the first line
   to "STATUS: BLOCKED". Otherwise leave it as "STATUS: RUNNING".

Rules: one task per iteration, no scope creep, no rewriting completed work
unless a test now fails because of it, no skipping tests, no fabricated
benchmark numbers. Prefer finishing a small piece correctly over starting a
large one.

Lane mode. The procedure above is the default (main) mode: one iteration
at a time on `main`, the reference VMs, PROGRESS.md. When the iteration is
told it is a lane (the launcher names the lane, its worktree, branch, task
list and environment; `scripts/ralph-lane.sh <lane>` prints them), the
following replaces the parts of steps 2, 4, 5 and 7 that name PROGRESS.md,
the reference VMs or the scorecard; everything else (design first,
attribution before a fast-path change, quick before ab, no fabricated
numbers, one task per iteration) holds unchanged.

- Work only in the lane's worktree `../challoc-wt/<lane>` on branch
  `loop/<lane>`. Never touch the main checkout or another lane's worktree.
  Commit on the branch as usual ("Phase N task M: <summary>"); the
  coordinator merges it into `main`, you do not.
- Take tasks from the lane's own list in docs/plan.md ("Execution in
  lanes"), in that order. A change that needs a file another lane owns
  waits for the coordinator's merge rather than editing that file.
- Progress goes to `PROGRESS.d/<lane>-<task>.md` (see
  `PROGRESS.d/README.md`: the same three shapes as PROGRESS.md, `## Done`,
  `## Design decisions`, `## Notes for the human`, one bullet per entry,
  each starting with `Phase N task M`), never to PROGRESS.md. Mark a task
  in progress and done in that file. `STATUS:` lines live in PROGRESS.md
  only; a lane that is blocked writes a `## Notes for the human` bullet
  starting with `BLOCKED:` in its file and says so in its final message.
- Never run the scorecard and never touch `bench/results/scorecard-*.md`;
  the coordinator runs it once per merge batch on the reference VMs from
  `main`. A lane's numbers come from `quick`, `quick-perf` and `ab` on the
  lane's own VMs and go into `bench/results/<date>-<arch>-<lane>-<task>*.md`.
- Export the lane's environment before anything runs: `CHALLOC_GCE_PREFIX`
  (`challoc-<lane>`, so `challoc-bench` names only the lane's VMs) and
  `TARGET_VOLUME` (`challoc-target-<lane>-$ARCH`, so the Mac container's
  `target/` is the lane's own). `CHALLOC_GCE_WARM` is not set for a lane.
- Build and test on the lane's VMs: `bench/gce/challoc-bench create
  arm64|amd64|all` and `provision` once (a throwaway `matrix lab` cell
  when the task calls for one), then `challoc-bench run <vm> 'make test'`,
  `challoc-bench quick|quick-perf|ab <vm> ...`, `challoc-bench fetch`. The
  Mac container is for host-side plumbing only (`make -n`, a compile
  check, `cargo test --features std` on the host), never for numbers.
- Before the turn ends, stop or delete every VM the lane started
  (`challoc-bench stop all` or `delete all`, `matrix clean`; `challoc-bench
  status` must show none of the lane's VMs RUNNING). Leave the worktree
  clean and committed.
- The final message is the merge request, for the coordinator: the lane
  and branch, every commit (hash and one line), the tests run and where
  (which VM, `make test` counts), the numbers with the results file paths
  (`quick` go/no-go, `ab` rows, attributions), the `PROGRESS.d` file
  written, the VMs' final state, and what the coordinator must check by
  hand (files touched outside the lane's partition, a design decision with
  more than one reasonable outcome, a criterion left open).
