# Rust codegen baseline A/B: Wolfi's C baseline vs rustc's default, 2026-09-17, both GCE VMs (us-east4-a)

Question from the human: is challoc leaving performance on the table by
compiling at rustc's default baseline (plain `x86-64` = SSE2; ARMv8.0 +
`neon`) when every C package in Wolfi is built at gcc's configured baseline
(`--with-arch=x86-64-v2 --with-tune=sapphirerapids` on x86_64,
`--with-arch=armv8-a+crc+crypto --with-tune=neoverse-v2` on aarch64, per
wolfi-dev/os `gcc.yaml`; `rust-1.98.yaml` sets no target-cpu, and both
`build-*.env` files set no CFLAGS)?

- commit e8e1c7e (src as of e6b89c3), hosts challoc-bench-arm64 (c4a-standard-16,
  Neoverse V2) and challoc-bench-amd64 (c4-standard-16, Xeon Platinum 8581C),
  kernel 7.0.0-1011-gcp, Wolfi container (rustc 1.98.1, gcc 15.3.0),
  jemalloc = Wolfi's libjemalloc.so.2 (5.3.1)
- variants: `base` = default `cargo build --release`; `v2` = the same source
  with `RUSTFLAGS="-C target-cpu=x86-64-v2"` (x86_64) or
  `RUSTFLAGS="-C target-feature=+crc,+aes,+sha2"` (aarch64, the Rust
  spelling of `armv8-a+crc+crypto`). Both built from copies of the tree into
  `target/base` and `target/v2`; the binaries differ (524104 vs 524648 bytes
  on x86_64, 585072 vs 585096 on aarch64). 5 interleaved rounds plus a
  warm-up, sorted scores, `rt` median maxrss. Load under 0.3 at the start.
- programs: `larson 5 8 1000 5000 100 4141 1|10` (M ops/s), `xmalloc-test -w
  10 -t 5 -s 64` (M frees/s), `alloc-test 1|16` (s)

## aarch64

| program | variant | sorted scores | maxrss KiB |
| larson1 | base | 43.1 43.3 43.3 43.3 43.5 | 13708 |
| larson1 | v2 | 42.8 43.2 43.2 43.3 43.3 | 13468 |
| larson1 | jemalloc | 44.9 45.0 45.0 45.0 45.1 | 17920 |
| larson10 | base | 351.1 351.2 351.5 351.5 351.7 | 87712 |
| larson10 | v2 | 344.1 344.4 346.3 347.2 348.2 | 87324 |
| larson10 | jemalloc | 384.3 386.5 388.5 388.8 389.1 | 134516 |
| xmalloc | base | 349.599 385.159 390.716 411.017 415.388 | 39028 |
| xmalloc | v2 | 350.365 354.836 365.495 382.808 386.813 | 39280 |
| xmalloc | jemalloc | 476.529 479.720 482.194 483.342 486.421 | 83436 |
| alloc-test1 | base | 2.653 2.654 2.656 2.656 2.668 | 13292 |
| alloc-test1 | v2 | 2.656 2.658 2.660 2.661 2.668 | 13288 |
| alloc-test1 | jemalloc | 2.549 2.558 2.561 2.569 2.575 | 12620 |
| alloc-testN | base | 2.236 2.241 2.244 2.244 2.244 | 16492 |
| alloc-testN | v2 | 2.241 2.247 2.248 2.249 2.250 | 16488 |
| alloc-testN | jemalloc | 2.200 2.201 2.202 2.203 2.208 | 16716 |

## x86_64

| program | variant | sorted scores | maxrss KiB |
| larson1 | base | 53.7 55.9 56.4 56.6 57.9 | 13576 |
| larson1 | v2 | 51.9 55.2 56.0 56.2 56.7 | 13592 |
| larson1 | jemalloc | 47.5 48.3 51.8 53.1 53.6 | 16852 |
| larson10 | base | 268.1 268.3 275.3 282.2 293.3 | 68244 |
| larson10 | v2 | 274.4 278.5 281.3 282.6 288.5 | 68720 |
| larson10 | jemalloc | 321.6 325.3 325.4 336.2 346.7 | 98880 |
| xmalloc | base | 556.921 587.554 587.840 589.738 601.035 | 39916 |
| xmalloc | v2 | 565.467 584.276 587.018 593.434 611.421 | 39876 |
| xmalloc | jemalloc | 357.005 364.888 370.038 372.779 379.505 | 96044 |
| alloc-test1 | base | 2.755 2.756 2.786 2.792 2.798 | 14052 |
| alloc-test1 | v2 | 2.759 2.760 2.764 2.768 2.816 | 14052 |
| alloc-test1 | jemalloc | 2.665 2.674 2.679 2.702 2.771 | 13464 |
| alloc-testN | base | 3.622 3.762 3.790 3.815 3.877 | 17568 |
| alloc-testN | v2 | 3.633 3.756 3.768 3.803 3.907 | 17820 |
| alloc-testN | jemalloc | 3.536 3.572 3.579 3.581 3.661 | 17056 |

## Reading

Neutral or slightly negative; not adopted.

- aarch64: larson1 43.3 -> 43.2 (unchanged), larson10 351.5 -> 346.3 (-1.5%,
  and the five `base` rounds sit within 0.6 of each other while every `v2`
  round is below them, so it is small but real), xmalloc-test inside its
  usual 350..415 spread, alloc-test1 and alloc-testN unchanged (2.656 vs
  2.660 s, 2.244 vs 2.248 s). Nothing on the allocator's paths uses CRC or
  crypto instructions; the only Arm feature that would matter, LSE atomics
  (`+lse`, ARMv8.1), is above Wolfi's baseline and stays outlined.
- x86_64: larson1 56.4 -> 56.0, larson10 275.3 -> 281.3, xmalloc-test 587.8
  -> 587.0, alloc-test1 2.786 -> 2.764 s, alloc-testN 3.790 -> 3.768 s: all
  inside the round-to-round spread (this VM's larson rows spread 4..9% in
  this run). x86-64-v2 adds SSE4.2 and POPCNT; `tzcnt`/`lzcnt` and BMI would
  need v3, which Wolfi does not assume.

So compiling at the C packages' baseline buys nothing here, and the
release profile stays at rustc's default. The compiler-side items left are
LSE atomics (not shippable under Wolfi's ARMv8.0 baseline) and PGO
(stable, untested, low priority behind review P2/P3). Recorded for the
question "are we taking full advantage of the compiler": the instruction
counts per operation match jemalloc's within 4% (`2026-09-17-gce-review-counters.md`),
so the remaining gaps are data placement and sharing, not code generation.
