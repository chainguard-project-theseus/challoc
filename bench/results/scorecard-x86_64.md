# challoc scorecard, x86_64

One line per (date, commit): the fixed row set under challoc and jemalloc, 3 interleaved rounds (bench/scorecard.sh, `make scorecard`, `challoc-bench scorecard <vm>`), as signed percentages against jemalloc (positive = challoc worse), RSS ratios challoc/jemalloc for larson10 and valkey1, and the most recent Valkey soak for the commit (challoc vs jemalloc: the worse of the two generators' throughput rows per phase, peak RSS, RSS after settle). Reference numbers are the GCE VMs' (bench/gce/README.md); a line from another machine says so in its note. `bench/scorecard.sh --render` prints this table sorted by date.

Seeded 2026-09-18 from the GCE results files that contain the rows (the challoc variant named in the note, medians against the same file's jemalloc column; 5 rounds there, not 3); the 2026-09-16 Mac numbers are not included. larson16 from the Phase 0 set is its 16-thread larson row; that set's xmalloc-test is `-w 16`, not this table's `-w 10` row, so it reads "-". No Valkey soak has run on x86_64, so the soak columns are "-".

| date | commit | larson1 | larson10 | larson16 | xmalloc | alloc-test1 | alloc-testN | valkey1 | valkey1-free | cfrac | espresso | mstress | glibc-simple | redis | RSS larson10 | RSS valkey1 | soak write | soak delete | soak peak RSS | soak post-delete RSS | note |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 2026-09-17 | 166a464 | - | - | +29.8% | - | +2.7% | +4.9% | - | - | +3.8% | -0.9% | -41.3% | -0.3% | -16.8% | - | - | - | - | - | - | (from 2026-09-17-gce-c4.md, the Phase 1.5 reference run, C4) |
| 2026-09-17 | e6b89c3 | -5.8% | +14.5% | - | -36.2% | +3.1% | +5.5% | - | - | - | - | - | - | - | 0.72 | - | - | - | - | - | (from 2026-09-17-x86_64-gce-slab-geometry.md run 2 `new`) |
| 2026-09-17 | cd9eda2 | -8.8% | +16.0% | +27.5% | -36.1% | +3.8% | +4.9% | - | - | - | - | - | - | - | 0.70 | - | - | - | - | - | (from 2026-09-17-x86_64-gce-narenas-default.md `old`) |
| 2026-09-17 | 8408d72 | -6.2% | +4.9% | +10.8% | -2.4% | +3.0% | +6.3% | - | - | - | - | - | - | - | 1.08 | - | - | - | - | - | (from 2026-09-17-x86_64-gce-narenas-default.md `new`, task 4c) |
| 2026-09-18 | c18979d | -4.9% | +4.0% | +12.5% | -3.7% | +4.4% | +5.4% | - | - | - | - | - | - | - | 1.06 | - | - | - | - | - | (from 2026-09-18-x86_64-gce-usable-size.md `old`) |
| 2026-09-18 | e1419fd | -0.6% | +3.5% | +10.0% | -4.4% | +4.5% | +6.7% | +12.2% | -5.5% | +4.0% | -0.1% | -36.5% | +2.3% | -15.3% | 1.08 | 0.99 | - | - | - | - | 3 rounds; no soak for e1419fd |
