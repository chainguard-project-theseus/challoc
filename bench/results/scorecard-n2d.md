# challoc scorecard, x86_64

One line per (date, commit): the fixed row set under challoc and jemalloc, 3 interleaved rounds (bench/scorecard.sh, `make scorecard`, `challoc-bench scorecard <vm>`), as signed percentages against jemalloc (positive = challoc worse), RSS ratios challoc/jemalloc for larson10 and valkey1, and the most recent Valkey soak for the commit (challoc vs jemalloc: the worse of the two generators' throughput rows per phase, peak RSS, RSS after settle). Reference numbers are the GCE VMs' (bench/gce/README.md); a line from another machine says so in its note. `bench/scorecard.sh --render` prints this table sorted by date.

| date | commit | larson1 | larson10 | larson16 | xmalloc | alloc-test1 | alloc-testN | valkey1 | valkey1-free | cfrac | espresso | mstress | glibc-simple | redis | python1 | python1-malloc | RSS larson10 | RSS valkey1 | soak write | soak delete | soak peak RSS | soak post-delete RSS | note |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 2026-09-19 | 7b2c50b | -14.0% | +11.3% | +6.7% | +9.7% | +6.2% | +6.1% | +21.0% | -5.8% | +3.9% | +0.2% | -58.9% | -37.0% | -21.9% | -0.5% | -3.4% | 1.02 | 0.99 | - | - | - | - | 3 rounds, GCE n2d-standard-8 (AMD EPYC 7B13), zone us-east4-a, 8 vCPU; no soak for 7b2c50b |
