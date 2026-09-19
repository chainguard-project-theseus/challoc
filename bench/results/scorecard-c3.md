# challoc scorecard, x86_64

One line per (date, commit): the fixed row set under challoc and jemalloc, 3 interleaved rounds (bench/scorecard.sh, `make scorecard`, `challoc-bench scorecard <vm>`), as signed percentages against jemalloc (positive = challoc worse), RSS ratios challoc/jemalloc for larson10 and valkey1, and the most recent Valkey soak for the commit (challoc vs jemalloc: the worse of the two generators' throughput rows per phase, peak RSS, RSS after settle). Reference numbers are the GCE VMs' (bench/gce/README.md); a line from another machine says so in its note. `bench/scorecard.sh --render` prints this table sorted by date.

| date | commit | larson1 | larson10 | larson16 | xmalloc | alloc-test1 | alloc-testN | valkey1 | valkey1-free | cfrac | espresso | mstress | glibc-simple | redis | python1 | python1-malloc | RSS larson10 | RSS valkey1 | soak write | soak delete | soak peak RSS | soak post-delete RSS | note |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 2026-09-19 | 7b2c50b | +0.3% | +6.8% | +7.7% | +0.7% | +5.8% | +7.2% | +41.7% | -9.4% | +4.9% | -0.6% | -41.5% | +0.7% | -19.5% | -1.4% | +0.9% | 1.06 | 0.99 | - | - | - | - | 3 rounds, GCE c3-standard-8 (Intel(R) Xeon(R) Platinum 8481C CPU @ 2.70GHz), zone us-east4-a, 8 vCPU; no soak for 7b2c50b |
