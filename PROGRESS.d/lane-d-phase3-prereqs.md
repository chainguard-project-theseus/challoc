# Lane D: Phase 3 prerequisites (c), (d), (e), measurement only

Branch `loop/lane-d` on a3ddd6d. No change under `src/`; every number is
from the lane's own VMs `challoc-lane-d-{arm64,amd64}` (c4a-standard-16 /
c4-standard-16, `us-east4-a`, the reference recipe), created 2026-09-19
17:3x UTC and deleted at the end of the iteration.

## Done

- (c) 0604bb3: `bench/ruby-soak.sh`, `make ruby-soak`, `bench/ruby/soak.rb` (16
  threads, bursts, growing long-lived set, full GC every 30 s, GC.compact at
  minute 5, 600 s; VmRSS/VmHWM, GC.stat, mallctl stats via fiddle every 5 s;
  malloc_stats_print at the end); results
  bench/results/2026-09-19-{x86_64,aarch64}-ruby-soak.md + CSVs + reports.
  challoc default 2.3..4.4x jemalloc's RSS for the whole run (t=600: 419 vs
  151 MiB x86_64, 393 vs 160 aarch64; peak 422 vs 181, 401 vs 201; after
  GC.compact 361 vs 129, 321 vs 130; lowest after a full GC 297 vs 68, 279
  vs 93); allocated within 10% (108 vs 97, 99 vs 90), active/allocated 2.0
  vs 1.1 (x86_64) and 2.1 vs 1.5 (aarch64, Wolfi's 64 KiB-page jemalloc),
  resident/active 1.9 vs 1.2..1.3. background_thread:true: within 2% (x86_64)
  and 0.7% (aarch64) of the default, allocator stats identical: no effect.
  Mechanism from the reports: `Arenas: 17 (64 automatic)`, one per thread,
  each ~12 MiB active + ~12 MiB dirty; 1280..2048 B at 47..52% occupancy in
  64 KiB slabs, 2560..4096 B at 14..28%, compaction `c_on 0` (gated).
  `narenas:1` on aarch64: 167 MiB at t=600 vs jemalloc 160 (+4%), peak 171
  vs 201 (-15%), floor after full GC 36 vs 93; active/allocated 1.27,
  resident-active 55 MiB vs 192. Reading and options in the aarch64 file.
- (e) 6d1f618: `scripts/check-needed.sh <binary> <soname>...` (readelf, exit 1
  when missing, prints NEEDED and RUNPATH), wired into contrib/valkey/build.sh
  (libchalloc.so.0) and contrib/ruby/build.sh (libjemalloc.so.2 on ruby and
  libruby.so); `make check-needed` = tests/as-needed.sh (tests/c/as_needed.c,
  as_needed_lib.c) on both lane VMs, green; the truth in design 13.6 and
  bench/results/2026-09-19-as-needed.md: Wolfi's gcc is --as-needed by
  default; an executable referencing malloc keeps DT_NEEDED (malloc resolves in
  libchalloc before libc); one referencing nothing drops it; the two-level
  shape (executable allocates only through a library that needs libchalloc)
  drops the executable's entry and the library's malloc binds to glibc for the
  whole process, silently (BFS search order puts libc.so.6 first); Ruby escapes
  it with -Wl,--no-as-needed in LDFLAGS (ruby/ruby#4627). Package rule: link
  with --no-as-needed (or reference the allocator) and run the checker in the
  package test on every executable and the allocating library.
- (d) 25021da: `bench/valkey-soak.sh` IO_THREADS (--io-threads N, CONFIG GET
  read back, header) and COMMIT; results
  bench/results/2026-09-19-aarch64-gce-valkey-soak-iothreads4.md (C4A lane VM,
  a3ddd6d, sidecar attached): throughput/p99 FAIL (write valkey-benchmark -6.1%
  / p99 +9.1%, delete memtier -12.3% / +11.5%, delete valkey-benchmark p99
  +24.5%), peak RSS 766 vs 682 MiB +12.3% FAIL, RSS after settle +3.5% PASS;
  cycles/cmd 6842 vs 6598 (+3.7%) write, 4698 vs 4231 (+11.0%) delete (one
  io-thread: +1.0/+2.6%); allocator code equal (7.80 vs 7.89%, 4.24 vs 4.58% of
  cycles; free_other + flush + RMWs under 1.5%); the deficit is +10 L1D and +7
  dTLB misses per command in Valkey's code and libc on memory placed across 4
  per-thread arenas (values live in the io threads' arenas, every DEL is a
  cross-arena free; main-thread arena at 17% occupancy after the delete phase,
  defrag hits 581k vs 339k). MALLOC-STATS after settle and CSVs beside it. The
  report does not print the `others` node count (bound from design 6.2: at most
  3 per heap, 12 in the process).

## Design decisions

- (e) as a shell test (tests/as-needed.sh, `make check-needed`), not a test in
  src/api/ctests.rs: the lane never edits src/. The test's `none-default` case
  is informational (records the toolchain default) since it depends on the gcc
  spec, and the two-level case asserts the glibc binding it found.
- (d) the soak's results go to RESULT_DIR under `.tmp-out/` on the VM (rsync
  --delete on sync would remove bench/results/ files written on the VM) and are
  copied into bench/results/ by hand with the `-iothreads4` name; the summary is
  the soak script's own table with a reference column added by hand.
- (c) allocator stats via fiddle -> mallctl (`mallctl`, then `je_mallctl`) and a
  malloc_stats_print dump to stderr at the end of each run, kept beside the CSV
  as `-mstats.txt`; jemalloc's 65-arena report trimmed to general + merged (+
  arenas with threads) before committing. `challoc-1arena` (MALLOC_CONF=
  narenas:1) added as an optional variant after the x86_64 run showed 17
  per-thread arenas; run on aarch64 only (one extra 10-minute run).
- Both soak scripts take COMMIT: a worktree synced to a VM has a `.git` file
  pointing at a Mac path, so git in the container says "unknown".
- The Makefile hunks were staged per prerequisite; the .PHONY line and the two
  help blocks (they name both ruby-soak and check-needed) went with (e).

## Notes for the human

- (c) decision: `background_thread` is not the lever for Ruby-shaped
  consumers (zero effect on both architectures); the automatic arena count
  is (16 idle-ish threads = 16 private slab sets). Measured option: ship
  `narenas:1` (or a small fixed count) for the Ruby package through
  `malloc_conf`/`MALLOC_CONF`, which the compat library honours today;
  src/ options (arena policy, idle-heap slab release) are listed in the
  aarch64 reading. The `narenas:1` run exists on aarch64 only.
- (d) `io-threads 4` FAILS two Phase 1 criteria on the same library that
  passes them at one io-thread (throughput/p99 up to -12.3% / +24.5%, peak
  RSS +12.3%). Attribution: allocator code equal, +10 L1D / +7 dTLB misses
  per command in Valkey and libc on memory placed across 4 per-thread
  arenas. Suggested next runs: the same soak with `narenas:1` under challoc,
  `io-threads 2` and `8`; Phase 2.5 task 12's CLDEMOTE has this soak as its
  workload. The `others` node count is not in the report (a print.rs line
  would help future runs).
- (e) Wolfi's gcc is --as-needed by default; a package must link executables
  with -Wl,--no-as-needed (or reference the allocator) and run
  scripts/check-needed.sh on every executable and the allocating library.
  Ruby already does the former (ruby/ruby#4627). Design 13.6 has the text.
- Both soak scripts' results on a VM go under `.tmp-out/` (sync-safe) and
  were copied into bench/results/ by hand; `commit:` in the headers comes
  from COMMIT=a3ddd6d (git says "unknown" in a synced worktree).
- Makefile help for `ruby-soak` and the .PHONY line landed with the (e)
  commit (shared hunks); harmless, noted for a reviewer of per-commit diffs.
- Lane VMs `challoc-lane-d-{arm64,amd64}` deleted (arm64 last, 19:2x UTC).
