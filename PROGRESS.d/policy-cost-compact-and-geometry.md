## Done

- Policy cost rows (lane policy-cost, 2026-09-21): done. Two hot-path
  policies measured off versus on in one run on the lane's VMs
  (`challoc-policy-cost-{arm64,amd64}`, c4a/c4-standard-16, kernel
  7.0.0-1011-gcp, both deleted at the end), no default changed, nothing
  under `src/` changed (`src/` is byte for byte main 2078f67; the
  geometry variants are gitignored `.tmp-slab16`/`.tmp-slab32` copies
  made by `bench/geometry-variant.sh`). (1) The compacting cache,
  `tcache_compact_shift:0` against the default: quick, quick-perf on
  larson1/valkey1/valkey1-free with the model's per-phase lines, ab 5
  rounds with the two model rows, the churn probe, the Valkey soak on
  aarch64 (default, compact0, jemalloc), the Ruby soak on both archs.
  (2) 16 and 32 KiB slabs for the 1280..4096-byte classes: quick (GO
  both variants both archs), ab 5 rounds, quick-perf, the Ruby soak with
  per-class occupancy for the three builds and jemalloc on both archs
  plus the default/slab16 pair under `narenas:1`, the Valkey soak
  default vs slab16 on aarch64. Results:
  `bench/results/2026-09-21-{aarch64,x86_64}-policy-cost-compact.md`,
  `bench/results/2026-09-21-{aarch64,x86_64}-policy-cost-geometry.md`
  (each with the VM, kernel, commit, the builds' sha256 and a trade
  section at the end), raw soak files beside them
  (`...-policy-cost-ruby-soak-*`, `...-policy-cost-valkey-soak-*`).
  Harness (committed): `bench/lib.sh` builds a `dir:.tmp-<v>` source
  directory as a variant (quick OLD=/NEW=, ab VARIANTS=; `rev_dirname`
  for build directories, ab.sh refuses a path LD_PRELOAD would split),
  `bench/gce/quick-perf.sh` takes `LABEL@/path[:conf]` variants,
  `bench/ruby-soak.sh` CHALLOC_SRC, `bench/valkey-soak.sh` CHALLOC_LIB
  (preloaded under the soname, mapped library and sha256 recorded),
  `bench/geometry-variant.sh`, `bench/probes/mstats-classes.sh`
  (per-class totals over arenas from a stats report). Commits ed07b0a,
  0c8416a, 9cce8d6, f53418d, 56efa27 (harness), fd00bf7, 5c9d083,
  39272c3, 3b0ac63 and the final results commit.

## Notes for the human

- Policy cost, trade 1 (the compacting cache, design 6.4): turning
  `tcache_compact_shift` off buys 1..3% of a Valkey server's
  delete-phase throughput and p99 (aarch64 soak: 1423k -> 1465k
  valkey-benchmark req/s, p99 0.719 -> 0.687 ms; nothing in the write
  phase, nothing on larson/xmalloc/alloc-test, nothing under Ruby where
  the schedule never engages) and costs 18% of its post-delete RSS and
  the criterion with it (130.3 MiB after settle, +1.4% over jemalloc,
  against 153.1, +19%; the criterion is +10%; frag ratio 1.81 -> 2.18,
  the 512..1024 classes 57% -> 47% occupancy; Valkey's defragmenter did
  15% more moves and left the residue). The allocator's own price where
  the schedule works is a fifth of the delete phase in the model (20.4
  -> 24.8 M ops/s aarch64, 34.2 -> 37.6 x86_64; 40 / 24 fewer
  instructions per op over the run) for 220 MiB given back in 5 s; when
  it does not engage the mode test costs nothing (larson 264.5 / 264.5
  instructions). Recommendation: keep the default (shift 3). If the
  delete-phase price is ever the target, measure `tcache_compact_shift:4`
  on the model's per-phase row, not the switch.
- Policy cost, trade 2 (16/32 KiB slabs for 1280..4096 B): the change
  buys 10 points of occupancy in those classes under Ruby (44% -> 54%,
  25..30 MiB less pinned, `active` down by the same) and no RSS at all
  (397 -> 400 MiB at t=600 aarch64, 414 -> 419 x86_64: the bytes move
  into the arenas' Empty inventory, which is what the 17-arena RSS is
  made of); under `narenas:1` (the ruby-rss lane's lever, 397 -> 172
  MiB = jemalloc's 172) it gives back 2 MiB of pinned bytes and costs
  5..21 MiB of dirty ones (RSS +2% aarch64, +11% x86_64); under Valkey
  nothing moves (+0.6% RSS after settle, criteria unchanged). It costs
  nothing on the fast tier (quick GO, ab within 2%, instructions per op
  identical) and 6..12% internal waste per slab in five classes.
  Recommendation: do not change the geometry; it is not an independent
  lever beside the arena policy on the Ruby shape. Per-class tables for
  every build are in the geometry files (section 5 and 5b).
- Two incidents the coordinator should know about, both harness-side:
  the first compact `ab` measured glibc for both variants because a
  duplicate `rev_label` put spaces in the build directory (fixed in
  f53418d, guard added, rerun); and local-to-VM transfers above a few
  MB stalled from about 07:20 UTC on the Mac's network (a 6 MB pack
  over ssh stdin timed out), which broke the arm64 VM's synced `.git`;
  the last two aarch64 Ruby soaks ran on a tree re-initialised as one
  local commit of the same files (checksums checked) with
  `COMMIT=3b0ac63` in their headers. Everything else ran on synced
  trees at f53418d.
