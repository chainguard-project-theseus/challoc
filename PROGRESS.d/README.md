# PROGRESS.d: per-lane progress notes

1. A lane iteration (scripts/ralph-prompt.md, "Lane mode") never edits PROGRESS.md; it writes `PROGRESS.d/<lane>-<task>.md` instead, for example `lane-b-10.md`.
2. The file has up to three sections with exactly these headers: `## Done`, `## Design decisions`, `## Notes for the human`; the same shapes as PROGRESS.md.
3. Each entry is one markdown bullet (`- Phase N task M ...`), continuation lines indented; a blank line ends an entry. The `Phase N` prefix is what places it.
4. Mark the task in progress in `## Done` (`- Phase N task M: in progress, ...`) and rewrite that bullet when it is done, with the commit hashes.
5. A blocked lane writes a `## Notes for the human` bullet starting with `BLOCKED:`; `STATUS:` lines exist only in PROGRESS.md.
6. Numbers referenced here come from `quick`, `quick-perf` and `ab` on the lane's VMs and live in `bench/results/<date>-<arch>-<lane>-<task>*.md`; never from a scorecard.
7. The coordinator's `scripts/ralph-lane.sh --merge <lane>` appends each section's bullets after the last PROGRESS.md bullet of the same phase under the matching header (or at the section's end), deletes the folded files and commits both in one commit.
8. After the merge the file is gone from `main`; the lane's next task starts a new file. Do not reuse a name.
9. Files here are never a place for results tables; they point at `bench/results/`.
10. This README and `.gitkeep` keep the directory present in every checkout.
