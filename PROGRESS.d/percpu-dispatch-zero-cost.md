## Done

- Per-CPU dispatch (lane percpu-dispatch, 2026-09-21): in progress. The
  per-CPU mode's dispatch leaves the entry points: a heap in the mode
  keeps its home caches `EMPTY`, so `malloc`/`free`/`sdallocx` fall
  through on the `len == 0` / `len == cap` test they already have and the
  slow paths route on `Heap::percpu` (shape (a) of the brief). Code
  commit da39e2e; measurements pending.

## Design decisions

## Notes for the human
