# Ruby threaded RSS, fast tier, tests and code-generation record, 2026-09-21, aarch64

- lane ruby-rss, VM challoc-ruby-rss-arm64 (c4a-standard-16, Google Axion, Neoverse V2), Ubuntu 24.04 kernel 7.0.0-1011-gcp, the dev image (Wolfi, rustc 1.98.1). Base = df6d285 (its `src/` is main 2078f67's). jemalloc = Wolfi's 5.3.1.
- the three trees measured, in the order they were built (the split A/B in `2026-09-21-aarch64-ruby-rss-ab-split.md` names what each costs):
  - v1 = 6b29fa9: every plain free onto the home cache (jemalloc's tcache shape) plus the unlimited exit purge (later split into 4dc352c and 1a45278);
  - v2 = c8a8e03: plain frees of other arenas' slots onto one flush-only FOREIGN cache per class (the ladder's capacity, half released when full) plus the exit purge rate-limited to once per decay period per arena (6e25c7a);
  - v3 = aeafc1e, the tree merged: v2 with the FOREIGN cache at half the ladder, released whole when full.
- `make quick OLD=df6d285`, one round each, from the acceptance chains' logs (`~/logs/accept*-quick.log` on the VM). Positive percentages mean new did worse.

## quick, v3 (aeafc1e vs df6d285): go

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
| larson1 | M ops/s | 41.9 (16.0 MiB) | 42.0 (15.6 MiB) | 45.0 (18.4 MiB) | -0.2% | +7.1% |
| larson10 | M ops/s | 368.1 (111.6 MiB) | 361.3 (102.5 MiB) | 388.6 (133.0 MiB) | +1.9% | +7.6% |
| xmalloc | M frees/s | 577.168 (53.5 MiB) | 657.021 (50.0 MiB) | 485.613 (83.5 MiB) | -12.2% | -26.1% |
| alloc-test1 | s | 2.729 (13.1 MiB) | 2.740 (12.7 MiB) | 2.540 (12.3 MiB) | +0.4% | +7.9% |
| valkey1 | M ops/s | 7.037 (584.3 MiB) | 7.010 (584.3 MiB) (median of 3) | 6.596 (585.3 MiB) | +0.4% | -5.9% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24253415 ops/s |
Positive percentages mean new did worse (slower, lower throughput). One round each, aarch64, nproc 16, load at start 2.46, 149 s total. old 4d04e89158b5bcc4, new 042d820e4ed1d3ab.
quick: go (new within 5% of old on every row; churn compacts)

## quick, v2 (c8a8e03 vs df6d285): go

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
| larson1 | M ops/s | 41.7 (15.8 MiB) | 41.8 (16.0 MiB) | 45.1 (17.4 MiB) | -0.2% | +7.9% |
| larson10 | M ops/s | 368.0 (113.1 MiB) | 366.6 (112.3 MiB) | 386.2 (132.0 MiB) | +0.4% | +5.3% |
| xmalloc | M frees/s | 576.661 (54.0 MiB) | 682.825 (54.7 MiB) | 477.633 (77.8 MiB) | -15.5% | -30.1% |
| alloc-test1 | s | 2.755 (13.1 MiB) | 2.730 (12.7 MiB) | 2.550 (12.3 MiB) | -0.9% | +7.1% |
| valkey1 | M ops/s | 6.978 (584.3 MiB) | 6.907 (584.3 MiB) | 6.947 (585.5 MiB) | +1.0% | +0.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3461 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 23422629 ops/s |
Positive percentages mean new did worse (slower, lower throughput). One round each, aarch64, nproc 16, load at start 2.96, 128 s total. old 4d04e89158b5bcc4, new 5f71d437a450a604.
quick: go (new within 5% of old on every row; churn compacts)

## quick, v1 (6b29fa9 vs df6d285): go

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
| larson1 | M ops/s | 41.9 (15.9 MiB) | 44.5 (16.0 MiB) | 45.1 (19.0 MiB) | -5.8% | +1.3% |
| larson10 | M ops/s | 363.8 (111.9 MiB) | 365.4 (96.0 MiB) | 383.4 (131.6 MiB) | -0.4% | +4.9% |
| xmalloc | M frees/s | 576.668 (53.2 MiB) | 999.704 (67.3 MiB) | 480.440 (78.4 MiB) | -42.3% | -51.9% |
| alloc-test1 | s | 2.718 (13.1 MiB) | 2.678 (12.7 MiB) | 2.558 (12.3 MiB) | -1.5% | +4.7% |
| valkey1 | M ops/s | 7.009 (584.3 MiB) | 6.992 (584.3 MiB) | 7.030 (585.3 MiB) | +0.2% | +0.5% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24403747 ops/s |
Positive percentages mean new did worse (slower, lower throughput). One round each, aarch64, nproc 16, load at start 3.10, 130 s total. old 4d04e89158b5bcc4, new db5fab67308f8931.
quick: go (new within 5% of old on every row; churn compacts)

## make test on the VM (v3 tree)

`make test` on the v3 tree (aeafc1e), exit 0: `test result: ok. 482 passed; 0 failed` (the host-style unit tests with the C harness), `138 passed; 0 failed` (the Linux tests: reservations, TLS, threads), `26 passed; 0 failed` (the container tests), wall 9.23s / 12.18s / 8.26s; the C steps the target logs: `cgroup_churn purge 400 100`, `cgroup_churn keep 400 100`, `cgroup_churn purge 200 50`, `cgroup_churn keep 200 50`, `cgroup_lazy lazy 200`, `cgroup_lazy eager 200`, `pagemap_guard on` (kernel 7.0.0-1011-gcp aarch64, default configuration), `pagemap_guard off` (`guards:false`). The same three counts passed on the v1 (6b29fa9) and v2 (c8a8e03) trees. Host: `cargo test --features std` in the aarch64 dev container, everything but the C harness (`--skip ctests:: --skip jemalloc_compat_stage`, whose kernel features the Mac's VM lacks): 529 passed, 0 failed.

## Code generation (`scripts/codegen-diff.sh`, aarch64 container build, baseline captured at 2078f67)

- v3 against the baseline: `.text` differs (functions 541 -> 541: exact 88, reloc-only 440, changed 13; instructions 63141 -> 63309 for `quantum16`; q8: changed 14). The exported entry points `free`, `malloc`, `sdallocx`, `calloc` and `free_sized` are byte-identical to the baseline (disassembly with addresses and immediates masked: 149, 70, 192, 222 and 151 instructions). The changed functions are cold: `Heap::free_other` (the FOREIGN push), `Heap::free_slow_in`, `Heap::ensure_cache`, `Heap::flush` (the FOREIGN batch's first owner), `Heap::destroy` (the exit purge), `Arena::init` (the stamp), `api::libc::free_slow`, `free_sized_slow`, `realloc`, `heap::mallocx::free`, plus three toolchain artifacts (`core::slice::sort::...::panic_on_ord_violation`, `Display::fmt`, `__aarch64_ldset8_acq`: layout shifts).
- v1 (the home-cache form) had `free` 10 instructions shorter (the `multi_arena` test and the owner-word load gone); that form was not kept (the split A/B).

## Per-operation counters (`challoc-bench quick-perf arm64 OLD=df6d285`, v3)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 41.54 | 72.1 | 264.5 | 2.102 | 3.998 |
| larson1 | new | 41.47 | 72.1 | 264.4 | 2.106 | 3.986 |
| larson1 | jemalloc | 44.84 | 66.4 | 224.8 | 2.066 | 1.694 |
| valkey1 | old | 12.60 | 240.8 | 248.9 | 3.104 | 2.829 |
| valkey1 | new | 12.39 | 244.5 | 249.5 | 3.104 | 2.827 |
| valkey1 | jemalloc | 13.66 | 221.2 | 170.8 | 2.973 | 2.723 |

old = df6d285, new = aeafc1e. larson1 72.1 vs 72.1 cycles per operation, valkey1 240.8 vs 244.5 (+1.5%, one run each; instructions and misses per operation equal to the third digit): the fast paths are the base's code, as the disassembly says.
