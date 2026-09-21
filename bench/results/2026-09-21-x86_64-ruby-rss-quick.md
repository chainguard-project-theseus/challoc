# Ruby threaded RSS, fast tier, tests and code-generation record, 2026-09-21, x86_64

- lane ruby-rss, VM challoc-ruby-rss-amd64 (c4-standard-16, Intel Xeon 8581C), Ubuntu 24.04 kernel 7.0.0-1011-gcp, the dev image (Wolfi, rustc 1.98.1). Base = df6d285 (its `src/` is main 2078f67's). jemalloc = Wolfi's 5.3.1.
- the three trees measured, in the order they were built (the split A/B in `2026-09-21-x86_64-ruby-rss-ab-split.md` names what each costs):
  - v1 = 6b29fa9: every plain free onto the home cache (jemalloc's tcache shape) plus the unlimited exit purge (later split into 4dc352c and 1a45278);
  - v2 = c8a8e03: plain frees of other arenas' slots onto one flush-only FOREIGN cache per class (the ladder's capacity, half released when full) plus the exit purge rate-limited to once per decay period per arena (6e25c7a);
  - v3 = aeafc1e, the tree merged: v2 with the FOREIGN cache at half the ladder, released whole when full.
- `make quick OLD=df6d285`, one round each, from the acceptance chains' logs (`~/logs/accept*-quick.log` on the VM). Positive percentages mean new did worse.

## quick, v3 (aeafc1e vs df6d285): go

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
| larson1 | M ops/s | 44.6 (16.7 MiB) | 44.0 (16.8 MiB) (median of 3) | 45.2 (17.9 MiB) | +1.4% | +2.7% |
| larson10 | M ops/s | 285.2 (96.6 MiB) | 279.9 (88.1 MiB) | 293.6 (93.3 MiB) | +1.9% | +4.9% |
| xmalloc | M frees/s | 323.693 (52.9 MiB) | 349.772 (45.7 MiB) | 324.485 (88.1 MiB) | -7.5% | -7.2% |
| alloc-test1 | s | 3.417 (14.1 MiB) | 3.443 (13.6 MiB) | 3.227 (13.3 MiB) | +0.8% | +6.7% |
| valkey1 | M ops/s | 15.408 (585.3 MiB) | 15.346 (585.4 MiB) | 14.705 (591.9 MiB) | +0.4% | -4.2% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6666 (49.9%) | new compacts (end occupancy >= 90%) | 20707316 ops/s |
Positive percentages mean new did worse (slower, lower throughput). One round each, x86_64, nproc 16, load at start 2.12, 158 s total. old d4f7099f2b37ad12, new 27c637832f147b72.
quick: go (new within 5% of old on every row; churn compacts)

## quick, v2 (c8a8e03 vs df6d285): go

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
| larson1 | M ops/s | 44.9 (17.0 MiB) | 44.7 (17.1 MiB) | 45.2 (18.2 MiB) | +0.4% | +1.1% |
| larson10 | M ops/s | 277.1 (95.4 MiB) | 281.6 (95.8 MiB) | 288.5 (89.6 MiB) | -1.6% | +2.5% |
| xmalloc | M frees/s | 331.905 (54.8 MiB) | 362.735 (45.7 MiB) | 332.387 (91.4 MiB) | -8.5% | -8.4% |
| alloc-test1 | s | 3.235 (13.9 MiB) | 3.256 (13.5 MiB) | 3.024 (13.3 MiB) | +0.6% | +7.7% |
| valkey1 | M ops/s | 15.568 (585.4 MiB) | 15.307 (585.4 MiB) | 15.279 (592.1 MiB) | +1.7% | -0.2% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6668 (49.9%) | new compacts (end occupancy >= 90%) | 22282430 ops/s |
Positive percentages mean new did worse (slower, lower throughput). One round each, x86_64, nproc 16, load at start 2.69, 130 s total. old d4f7099f2b37ad12, new 668a33da50b80647.
quick: go (new within 5% of old on every row; churn compacts)

## quick, v1 (6b29fa9 vs df6d285): NO-GO on larson10 (+5.5%, median of 3), the row the split A/B attributes to the home-cache form

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
| larson1 | M ops/s | 44.7 (17.1 MiB) | 44.2 (16.5 MiB) | 44.9 (18.0 MiB) | +1.1% | +1.6% |
| larson10 | M ops/s | 276.6 (95.3 MiB) | 262.2 (78.3 MiB) (median of 3) | 287.8 (89.7 MiB) | +5.5% | +9.8% |
| xmalloc | M frees/s | 323.207 (53.4 MiB) | 498.940 (54.1 MiB) | 328.586 (89.1 MiB) | -35.2% | -34.1% |
| alloc-test1 | s | 3.436 (14.1 MiB) | 3.376 (13.7 MiB) | 3.160 (13.2 MiB) | -1.7% | +6.8% |
| valkey1 | M ops/s | 15.043 (585.4 MiB) | 15.398 (585.2 MiB) | 15.530 (592.2 MiB) | -2.3% | +0.9% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6656 (50.0%) | new compacts (end occupancy >= 90%) | 22524087 ops/s |
Positive percentages mean new did worse (slower, lower throughput). One round each, x86_64, nproc 16, load at start 4.42, 159 s total. old d4f7099f2b37ad12, new 51d98cfcdec97727.
quick: NO-GO (new trails old by more than 5% on a row, or does not compact)

## make test on the VM (v3 tree)

`make test` on the v3 tree (aeafc1e), exit 0: `test result: ok. 482 passed; 0 failed` (the host-style unit tests with the C harness), `138 passed; 0 failed` (the Linux tests: reservations, TLS, threads), `26 passed; 0 failed` (the container tests), wall 6.33s / 18.16s / 8.37s; the C steps the target logs: `cgroup_churn purge 400 100`, `cgroup_churn keep 400 100`, `cgroup_churn purge 200 50`, `cgroup_churn keep 200 50`, `cgroup_lazy lazy 200`, `cgroup_lazy eager 200`, `pagemap_guard on` (kernel 7.0.0-1011-gcp x86_64, default configuration), `pagemap_guard off` (`guards:false`). The same three counts passed on the v1 (6b29fa9) and v2 (c8a8e03) trees. Host: `cargo test --features std` in the aarch64 dev container, everything but the C harness (`--skip ctests:: --skip jemalloc_compat_stage`, whose kernel features the Mac's VM lacks): 529 passed, 0 failed.

## Code generation (`scripts/codegen-diff.sh`, aarch64 container build, baseline captured at 2078f67)

- v3 against the baseline: `.text` differs (functions 541 -> 541: exact 88, reloc-only 440, changed 13; instructions 63141 -> 63309 for `quantum16`; q8: changed 14). The exported entry points `free`, `malloc`, `sdallocx`, `calloc` and `free_sized` are byte-identical to the baseline (disassembly with addresses and immediates masked: 149, 70, 192, 222 and 151 instructions). The changed functions are cold: `Heap::free_other` (the FOREIGN push), `Heap::free_slow_in`, `Heap::ensure_cache`, `Heap::flush` (the FOREIGN batch's first owner), `Heap::destroy` (the exit purge), `Arena::init` (the stamp), `api::libc::free_slow`, `free_sized_slow`, `realloc`, `heap::mallocx::free`, plus three toolchain artifacts (`core::slice::sort::...::panic_on_ord_violation`, `Display::fmt`, `__aarch64_ldset8_acq`: layout shifts).
- v1 (the home-cache form) had `free` 10 instructions shorter (the `multi_arena` test and the owner-word load gone); that form was not kept (the split A/B).

## Per-operation counters (`challoc-bench quick-perf amd64 OLD=df6d285`, v3)

QUICKPERF_x86_64
