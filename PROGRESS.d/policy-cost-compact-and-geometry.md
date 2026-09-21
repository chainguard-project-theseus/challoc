## Done

- Policy cost rows (lane policy-cost, 2026-09-21): in progress. Two
  hot-path policies measured off versus on, same run, both lane VMs
  (`challoc-policy-cost-{arm64,amd64}`, kernel 7.0.0-1011-gcp), no
  default changed, nothing under `src/` changed: (1) the compacting
  cache, `tcache_compact_shift:0` against the default (quick, quick-perf
  on larson1/valkey1/valkey1-free, ab 5 rounds with the two model rows,
  the churn probe, the Valkey soak on aarch64, the Ruby soak on both);
  (2) 16 and 32 KiB slabs for the 1280..4096-byte classes
  (`bench/geometry-variant.sh` A/B copies; quick, ab 5 rounds,
  quick-perf, the Ruby soak with per-class occupancy on both archs, the
  Valkey soak default vs slab16 on aarch64). Results:
  `bench/results/2026-09-21-{aarch64,x86_64}-policy-cost-compact.md`,
  `bench/results/2026-09-21-{aarch64,x86_64}-policy-cost-geometry.md`.
  Harness: `bench/lib.sh` builds a `dir:.tmp-<v>` source directory as a
  variant (quick OLD=/NEW=, ab VARIANTS=), `bench/gce/quick-perf.sh`
  takes `LABEL@/path` variants, `bench/ruby-soak.sh` CHALLOC_SRC,
  `bench/valkey-soak.sh` CHALLOC_LIB (preloaded under the soname, mapped
  library checked and recorded), `bench/probes/mstats-classes.sh` (per-
  class totals over arenas from a stats report). Commits ed07b0a,
  0c8416a, 9cce8d6, f53418d (harness), results commits follow.

## Notes for the human

- Policy cost, trade 1 (compacting cache): pending the soaks.
- Policy cost, trade 2 (slab geometry 1280..4096 B): pending the soaks.
