## Done

- Phase 2.5 acceptance (lane A, 2026-09-20), the x86_64 Valkey write-phase
  reading of rows 3 and 5 attributed, commit 4a9cbae
  (`bench/results/2026-09-20-x86_64-valkey-write-phase-attribution.md`,
  soak files `-soak.md`/`-soak-{challoc,jemalloc}.csv`, perf appendix
  `-perf-report.txt`, sidecar `-perf-stat-*.csv`, `-snapshots-*.txt`,
  `-generator-cpu.log`, `-mstats-*-t{580,1180}.txt`). The acceptance
  protocol (`make valkey-soak ALLOCS="challoc jemalloc" DURATION=1200
  OCCUPANCY=1`, e3e5fc3, the reference C4 after `make valkey STAGE=build`
  relinked the quantum8 library) under `bench/probes/valkey-perf-sidecar.sh`
  plus an unprivileged per-process generator sampler. Verdict in the plan's
  words: the write-phase gap is outside the allocator. Both main threads
  saturated (99.9% / 100.7%), valkey-benchmark at 0.4 of a CPU and memtier's
  four workers at 1.0 together under both allocators, so throughput is clock
  over cycles per command; challoc's cycles per SET are lower (4251 vs 4464
  at the same 3.26 GHz on valkey-benchmark, 3458 vs 3506 on memtier, 3770
  vs 3884 in the 60 s window) and the vCPU's effective clock moved
  3.08..3.27 GHz within the one soak (the two halves up to 4% apart in the
  same minute of the protocol), which is the amplitude of the three
  same-protocol readings (-4.0..+5.8%); this run read challoc ahead in the
  write phase (761,992 vs 731,845 req/s, 912,526 vs 916,554; p99 -2.5% /
  -0.8%), RSS criteria met. Inside the allocator the one entry point above
  jemalloc's cost on this CPU is `je_malloc_usable_size` (Valkey's
  `zmalloc_size`): 346 vs 229 cycles per valkey-benchmark SET (+117), 250
  vs 187 per memtier SET, 153 vs 86 per DEL, the bitmap-word load of the
  exact liveness check (since task 4e's sized free no longer touches the
  word, the query is the slot's first touch: +1.0 L1D miss per SET) on a
  six-load classify chain; `je_malloc` (89 vs 76) and `je_sdallocx` (88 vs
  95) at parity, `calloc`/`realloc`/plain `free`/refill/flush below 0.05%,
  the kernel identical work (TCP family 12.89% vs 12.88%). The excess is
  compensated in the write phase by Valkey's own code, libc and the kernel
  (-332 cycles per SET); the delete phase, met yesterday, read +3.8% / +3.9%
  today, which is that term (+3.3% cycles per DEL) plus the clock. No code
  changed (the tree under acceptance stays byte-identical).

## Design decisions

- Phase 2.5 acceptance (lane A, 2026-09-20): the fix the attribution supports
  is recorded as a design note, not implemented (results file section 8):
  (1) `usable_size` on a slab pointer answers from `Regions::classify` alone
  without `check_live` (the 09-18 aarch64 variant, rejected then because
  `free` had just loaded the same line; tasks 4e/4f removed that premise),
  which trades the abort of a use-after-free `malloc_usable_size` query for
  up to 117 cycles per SET on the C4, a human's decision like 4e; or (2)
  keep the check and put `bits_base[c]` beside `class_base[c]` in the dense
  `Regions` arrays so the word is three dependent loads away, like
  jemalloc's rtree. Measurement plan named (`valkey1-usable` under
  `quick-perf` on the C4, `quick` on both reference VMs and a C3 lab cell,
  then this soak protocol with the sidecar).

## Notes for the human

- Phase 2.5 acceptance (lane A, 2026-09-20): two things the coordinator
  should weigh. The x86_64 soak's throughput rows carry the vCPU's clock
  ratio directly (3.08..3.27 GHz within one soak, the halves 21 minutes
  apart), so "at least equal" on the C4 is decidable only on the sidecar's
  cycles per command (the plan's tie-breaker), which read challoc ahead in
  the write phase in this run; the results file records the clock series
  per minute so a future soak can check the same. And the delete phase,
  which met yesterday, read 3.8..3.9% behind today with +3.3% cycles per
  DEL in `je_malloc_usable_size`; whether to open the design note as a task
  is the human's call (it changes what a use-after-free query does).
