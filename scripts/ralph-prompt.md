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
4. Build and run the task's tests inside the Linux dev container (see
   CLAUDE.md). Nothing runs on the macOS host except `cargo test --features std`
   for pure logic. If Dockerfile.dev does not exist yet, creating it is part
   of Phase 0 task 1.
5. When the task's criteria pass, mark it done in PROGRESS.md with the commit
   hash, and commit everything with a message "Phase N task M: <summary>".
   Never leave uncommitted work. Do not commit .ralph/ or target/.
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
