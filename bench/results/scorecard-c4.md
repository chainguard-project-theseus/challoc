# challoc scorecard, x86_64

One line per (date, commit): the fixed row set under challoc and jemalloc, 3 interleaved rounds (bench/scorecard.sh, `make scorecard`, `challoc-bench scorecard <vm>`), as signed percentages against jemalloc (positive = challoc worse), RSS ratios challoc/jemalloc for larson10 and valkey1, and the most recent Valkey soak for the commit (challoc vs jemalloc: the worse of the two generators' throughput rows per phase, peak RSS, RSS after settle). Reference numbers are the GCE VMs' (bench/gce/README.md); a line from another machine says so in its note. `bench/scorecard.sh --render` prints this table sorted by date.

| date | commit | larson1 | larson10 | larson16 | xmalloc | alloc-test1 | alloc-testN | valkey1 | valkey1-free | cfrac | espresso | mstress | glibc-simple | redis | python1 | python1-malloc | RSS larson10 | RSS valkey1 | soak write | soak delete | soak peak RSS | soak post-delete RSS | note |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 2026-09-19 | 7b2c50b | -2.0% | +7.6% | +8.3% | +0.9% | +4.8% | +7.5% | +5.4% | -15.4% | +8.4% | +2.4% | -51.2% | +3.6% | -19.5% | +0.8% | +0.4% | 1.05 | 0.99 | - | - | - | - | 3 rounds, GCE c4-standard-8 (INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz), zone us-east4-a, 8 vCPU; no soak for 7b2c50b |
