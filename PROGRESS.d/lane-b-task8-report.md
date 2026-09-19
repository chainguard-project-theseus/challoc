## Done

- Phase 2.5 task 8 (lane B, the rseq / `mm_cid` line of the feature report): done. `src/sys/features.rs` gains `Rseq` (`size`, `flags`, `offset`, `cpu_id`; `registered()` = size >= 20 and `cpu_id >= 0`, `mm_cid()` = registered and size >= 28) and `Features::rseq`, probed at init from glibc's weak `__rseq_size`/`__rseq_offset`/`__rseq_flags` through GOT loads (the `je_malloc_conf` recipe) plus the area's kernel-written `cpu_id` at `tp + __rseq_offset + 4`; the line gains `rseq=yes|no rseq_size=<n> mm_cid=yes|no` after `collapse`. Both ways: `rseq=yes rseq_size=33 mm_cid=yes` on both lane VMs (7.0, glibc 2.44; `__rseq_offset` 320 aarch64 / -384 x86_64 in the test binary against the note's 192 / -192, so the symbol read is what makes it right), `rseq=yes rseq_size=20 mm_cid=no` on the Mac's 6.10 linuxkit VM. `make test` 376 + 115 + 23 on all three hosts. Results: `bench/results/2026-09-19-lane-b-task8-rseq-report.md`; design 15, 16, 17 and README updated; a note on top of `bench/results/2026-09-19-feature-report.md`. Lane A reads `features().rseq.mm_cid()` for `percpu_arena:auto` and the value through its own accessor.

## Design decisions

- Phase 2.5 task 8 (lane B): `rseq=yes` requires the kernel to have written the area (`cpu_id >= 0`), not just a non-zero `__rseq_size`, so a registration glibc believes in but the kernel refused (`-2`) reads as no rseq and no `mm_cid`; `rseq_size` still prints the symbol's value so the two can be told apart. The three keys sit in the kernel section (after `collapse`), since the registration is a kernel-plus-libc fact rather than a CPU one, and `tests/c/features.c` (a task 2 test file, outside the lane's `src/` partition) was extended with the three keys so its every-key-once count keeps passing.

## Notes for the human

- Phase 2.5 task 8 (lane B): no cost row was taken (an init-only read, no fast path); if the coordinator wants one, the task 2 probe-cost harness (`sys::cpu::linux_tests::probe_cost_is_small`) is the place.
