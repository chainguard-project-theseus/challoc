# Policy cost: 16 and 32 KiB slabs for the 1280..4096-byte classes, 2026-09-21, x86_64 (lane policy-cost)

The x86_64 half of `2026-09-21-aarch64-policy-cost-geometry.md`, which
has the question, the exact variants, the per-class table each build
produces and the joint reading; this file records the same rows on the
x86_64 VM. Measurement only, no default changed, nothing under `src/`
changed.

- host: `challoc-policy-cost-amd64` (c4-standard-16, Intel Xeon
  Platinum 8581C, Emerald Rapids), 16 vCPUs, Ubuntu 24.04 kernel
  `7.0.0-1011-gcp`, 4 KiB pages, THP `madvise`; container: Wolfi
  (rustc 1.98.1); jemalloc = Wolfi's libjemalloc.so.2 5.3.1-r3. Zone
  us-east4-a. Nothing else ran on the VM.
- commit: `f53418d` on `loop/policy-cost` (harness hooks only; `src/`
  is byte for byte main `2078f67`).
- builds (`quantum16`, sha256 prefixes): `default` = the tree,
  `d4f7099f2b37ad12`; `slab16` = `.tmp-slab16` (16 KiB slabs for
  1280..4096 B), `29124797761cb759`; `slab32` = `.tmp-slab32` (32 KiB),
  `324c10c34496c1d5`; the same one-line variants as on aarch64
  (`bench/geometry-variant.sh slab16 14`, `slab32 15`), the same
  per-class table.
- method: as the aarch64 file (`quick` for each variant, `ab` with 5
  rounds and four variants, `quick-perf`, the Ruby soak for all three
  builds and jemalloc; the Valkey soak of this topic ran on aarch64
  only).

## 1. `quick` (one round each; the 5% gate)

`slab16` (new) vs `default` (old), load 3.20 at start, 129 s, `old
d4f7099f2b37ad12, new 29124797761cb759`:

| row | unit | old (default) | new (slab16) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 46.1 (17.1 MiB) | 46.6 (17.3 MiB) | 44.2 (18.1 MiB) | -1.1% | -5.2% |
| larson10 | M ops/s | 284.7 (96.1 MiB) | 290.9 (97.6 MiB) | 288.9 (93.0 MiB) | -2.1% | -0.7% |
| xmalloc | M frees/s | 341.411 (53.1 MiB) | 338.835 (52.4 MiB) | 338.891 (93.8 MiB) | +0.8% | +0.0% |
| alloc-test1 | s | 3.094 (14.0 MiB) | 3.147 (14.0 MiB) | 2.900 (13.3 MiB) | +1.7% | +8.5% |
| valkey1 | M ops/s | 14.822 (585.1 MiB) | 14.940 (585.5 MiB) | 14.727 (592.0 MiB) | -0.8% | -1.4% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6669 (49.9%) | new compacts (end occupancy >= 90%) | 23069869 ops/s |

`quick: go`. `slab32` (new) vs `default` (old), load 3.22, 128 s, `new
324c10c34496c1d5`:

| row | unit | old (default) | new (slab32) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 44.9 (17.1 MiB) | 46.0 (16.7 MiB) | 36.4 (17.3 MiB) | -2.4% | -20.9% |
| larson10 | M ops/s | 272.4 (94.9 MiB) | 295.0 (97.1 MiB) | 297.5 (93.7 MiB) | -7.7% | +0.8% |
| xmalloc | M frees/s | 339.221 (53.0 MiB) | 323.586 (52.3 MiB) | 339.264 (91.8 MiB) | +4.8% | +4.8% |
| alloc-test1 | s | 3.070 (13.9 MiB) | 3.086 (14.0 MiB) | 2.890 (13.3 MiB) | +0.5% | +6.8% |
| valkey1 | M ops/s | 15.072 (585.4 MiB) | 15.089 (585.3 MiB) | 13.057 (592.1 MiB) | -0.1% | -13.5% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6678 (49.8%) | new compacts (end occupancy >= 90%) | 22749839 ops/s |

`quick: go` for both. The one-round x86_64 rows are noisier than
aarch64's (larson10 272 vs 295 with `slab32` ahead, xmalloc +4.8%, a
jemalloc larson1 of 36.4 in one round): the `ab` below settles them.

## 2. `ab` (5 rounds, interleaved): `default`, `slab16`, `slab32`, `jemalloc`

Load 3.12 at start, 4.87 at the end.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | default | 44.4 45.0 46.3 46.3 46.6 | 17696 |
| larson1 | slab16 | 45.8 46.3 46.4 46.4 46.5 | 17652 |
| larson1 | slab32 | 43.9 44.3 45.9 45.9 46.0 | 17452 |
| larson1 | jemalloc | 46.1 46.1 46.3 46.4 46.6 | 18688 |
| larson10 | default | 271.9 286.4 289.7 293.2 294.1 | 99280 |
| larson10 | slab16 | 286.3 288.1 289.1 290.3 291.6 | 98904 |
| larson10 | slab32 | 283.0 283.6 287.0 288.5 294.9 | 98340 |
| larson10 | jemalloc | 301.1 302.9 303.3 309.3 312.9 | 96812 |
| larson16 | default | 304.6 311.9 313.3 315.5 316.8 | 121676 |
| larson16 | slab16 | 309.6 312.3 314.9 319.1 319.3 | 122936 |
| larson16 | slab32 | 308.8 309.0 310.2 316.4 316.4 | 119884 |
| larson16 | jemalloc | 333.0 341.7 344.0 351.6 351.7 | 131968 |
| xmalloc | default | 323.663 331.752 340.438 350.817 351.343 | 54824 |
| xmalloc | slab16 | 338.339 339.920 340.287 340.750 341.482 | 54256 |
| xmalloc | slab32 | 325.591 330.309 338.995 340.434 355.945 | 54088 |
| xmalloc | jemalloc | 335.700 338.964 339.527 339.843 340.806 | 89524 |
| alloc-test1 | default | 3.067 3.069 3.072 3.077 3.104 | 14320 |
| alloc-test1 | slab16 | 3.063 3.107 3.112 3.122 3.135 | 14356 |
| alloc-test1 | slab32 | 3.085 3.087 3.112 3.118 3.122 | 14276 |
| alloc-test1 | jemalloc | 2.882 2.890 2.894 2.897 2.909 | 13604 |
| alloc-testN | default | 4.335 4.376 4.390 4.395 4.419 | 17740 |
| alloc-testN | slab16 | 4.374 4.406 4.444 4.474 4.478 | 17644 |
| alloc-testN | slab32 | 4.417 4.453 4.463 4.538 4.550 | 17800 |
| alloc-testN | jemalloc | 4.015 4.027 4.058 4.066 4.129 | 17360 |
| valkey1 | default | 14.876 14.984 15.124 15.272 15.298 | 599700 |
| valkey1 | slab16 | 14.692 15.022 15.168 15.217 15.222 | 600024 |
| valkey1 | slab32 | 14.327 14.610 15.001 15.036 15.091 | 599812 |
| valkey1 | jemalloc | 14.641 14.855 14.868 14.953 14.999 | 606700 |
| valkey1-free | default | 14.463 14.958 15.072 15.147 15.175 | 599700 |
| valkey1-free | slab16 | 14.814 14.852 14.905 14.996 15.202 | 600068 |
| valkey1-free | slab32 | 14.753 14.850 14.954 15.070 15.101 | 599788 |
| valkey1-free | jemalloc | 13.024 13.135 13.163 13.223 13.289 | 606712 |

| program | unit | default | default RSS MiB | slab16 | slab16 RSS MiB | slab32 | slab32 RSS MiB | jemalloc | jemalloc RSS MiB | slab16 vs default | slab32 vs default | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 46.3 | 17.3 | 46.4 | 17.2 | 45.9 | 17.0 | 46.3 | 18.2 | -0.2% | +0.9% | +0.0% |
| larson10 | M ops/s | 289.7 | 97.0 | 289.1 | 96.6 | 287.0 | 96.0 | 303.3 | 94.5 | +0.2% | +0.9% | -4.5% |
| larson16 | M ops/s | 313.3 | 118.8 | 314.9 | 120.1 | 310.2 | 117.1 | 344.0 | 128.9 | -0.5% | +1.0% | -8.9% |
| xmalloc | M frees/s | 340.438 | 53.5 | 340.287 | 53.0 | 338.995 | 52.8 | 339.527 | 87.4 | +0.0% | +0.4% | +0.3% |
| alloc-test1 | s | 3.072 | 14.0 | 3.112 | 14.0 | 3.112 | 13.9 | 2.894 | 13.3 | +1.3% | +1.3% | -5.8% |
| alloc-testN | s | 4.390 | 17.3 | 4.444 | 17.2 | 4.463 | 17.4 | 4.058 | 17.0 | +1.2% | +1.7% | -7.6% |
| valkey1 | M ops/s | 15.124 | 585.6 | 15.168 | 586.0 | 15.001 | 585.8 | 14.868 | 592.5 | -0.3% | +0.8% | +1.7% |
| valkey1-free | M ops/s | 15.072 | 585.6 | 14.905 | 586.0 | 14.954 | 585.7 | 13.163 | 592.5 | +1.1% | +0.8% | +14.5% |

Positive percentages mean the variant did worse than `default`.

Reading: every row within 2% with interleaved lists (alloc-test's +1.3%
and +1.2..1.7% are 3.063..3.135 against 3.067..3.104, and 4.374..4.478
against 4.335..4.419: the widest of the table and overlapping). As on
aarch64, these programs do not allocate in the changed classes; the
table says the change is invisible to them.

## 3. `quick-perf`: per-operation counters (larson1, valkey1), three builds and jemalloc

`new` is the default build (`target/quick/new`, `d4f7099f2b37ad12`),
`slab16` and `slab32` the `ab` builds under `target/ab/.tmp-slab16` and
`.tmp-slab32` (quick-perf's `LABEL@PATH` form); load 0.68 at start, one
run each, whole-run counters.

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | default | 44.09 | 76.8 | 243.9 | 1.628 | 0.002 |
| larson1 | slab16 | 44.60 | 76.3 | 244.1 | 1.612 | 0.002 |
| larson1 | slab32 | 46.22 | 73.5 | 244.2 | 1.630 | 0.002 |
| larson1 | jemalloc | 46.47 | 72.3 | 200.9 | 2.137 | 0.001 |
| valkey1 | default | 24.91 | 150.3 | 234.8 | 3.274 | 0.896 |
| valkey1 | slab16 | 25.32 | 148.5 | 234.8 | 3.278 | 0.906 |
| valkey1 | slab32 | 24.67 | 151.7 | 236.6 | 3.292 | 0.905 |
| valkey1 | jemalloc | 27.96 | 133.9 | 151.9 | 3.333 | 0.950 |

Per phase of the model: write 15.05 / 15.34 / 15.21 M ops/s (default /
slab16 / slab32), delete 34.77 / 35.30 / 34.14, RSS after the delete
phase 165.1 / 163.6 / 164.9 MiB (jemalloc 14.71, 41.21, 414.9).

Reading: the instruction count per operation is the same to a tenth
(243.9 / 244.1 / 244.2 and 234.8 / 234.8 / 236.6): the geometry is a
table entry, and neither program allocates in the changed classes, so
nothing the fast paths execute differs; the cycle spread (73.5..76.8,
148.5..151.7) is one run's noise and the `ab` of section 2 puts the
three builds together. No per-operation cost to record on these rows.

## 4. Valkey soak

Ran on aarch64 only (`2026-09-21-aarch64-policy-cost-geometry.md`,
section 4).

## 5. Ruby soak (600 s, 16 threads): `default`, `slab16`, `slab32`, jemalloc

`bench/ruby-soak.sh` (Ruby 3.4.10 `--with-jemalloc` on the compat
library, `bench/ruby/soak.rb`, 16 threads, seed 4141, full GC every
30 s, `GC.compact` at 300 s), one process per allocator in sequence on
the idle VM (load 1.00 throughout: the GVL). `default` and jemalloc from
one invocation (`2026-09-21-x86_64-policy-cost-ruby-soak-default.md`),
`slab16` and `slab32` from one each with `CHALLOC_SRC=.tmp-slab16` /
`.tmp-slab32` (`...-ruby-soak-slab16.md`, `...-ruby-soak-slab32.md`;
the stage compiled from the copy each time, `Compiling challoc`
checked). The per-allocator reports are the `-challoc-mstats.txt` files
beside them.

| measure (MiB) | default | slab16 | slab32 | jemalloc | slab16 vs default | slab32 vs default |
|---|---:|---:|---:|---:|---:|---:|
| peak sampled VmRSS | 417.9 | 418.9 | 414.4 | 179.1 | +0.2% | -0.8% |
| VmHWM at the end | 419.1 | 423.9 | 416.5 | 186.9 | +1.1% | -0.6% |
| at t=60 | 309.1 | 314.3 | 312.2 | 73.6 | +1.7% | +1.0% |
| at t=295 (before GC.compact) | 349.8 | 347.9 | 348.0 | 119.2 | -0.5% | -0.5% |
| at t=300 (after GC.compact) | 361.9 | 362.9 | 356.9 | 127.0 | +0.3% | -1.4% |
| at t=600 | 414.0 | 418.9 | 407.2 | 133.9 | +1.2% | -1.6% |
| lowest right after a periodic full GC | 302.4 | 307.9 | 304.1 | 73.5 | +1.8% | +0.6% |
| allocator: allocated / active / resident / retained, final | 106.0 / 212.9 / 417.3 / 66.3 | 106.0 / 188.1 / 421.4 / 70.6 | 105.7 / 196.1 / 409.7 / 71.3 | 94.8 / 107.3 / 123.4 / 94.7 | active -24.8 | active -16.8 |
| resident - active (Empty slabs + metadata), final | 204.4 | 233.3 | 213.6 | 16.1 | +28.9 | +9.2 |
| jobs (600 s) | 151624 | 151025 | 151351 | 151262 | -0.4% | -0.2% |
| GC count (major) | 16211 (1301) | 16413 (1382) | 16432 (1367) | 16319 (1358) | | |

The soak samples no CPU time; at a fixed 600 s and load 1.00 the job
count is the throughput, and it is within 0.4% across the three builds.

Per class over the 17 arenas (`bench/probes/mstats-classes.sh FILE`, the
final report; `pinned` = slab bytes minus live bytes):

`default` (64 KiB slabs):

| class | nregs | slab KiB | curregs | curslabs | nonfull | occupancy | live MiB | slab MiB | pinned MiB | c_on | gated |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1280 | 51 | 64 | 10301 | 412 | 412 | 49.0% | 12.6 | 25.8 | 13.2 | 0 | 231 |
| 1536 | 42 | 64 | 10268 | 490 | 488 | 49.9% | 15.0 | 30.6 | 15.6 | 0 | 352 |
| 1792 | 36 | 64 | 10365 | 583 | 579 | 49.4% | 17.7 | 36.4 | 18.7 | 0 | 500 |
| 2048 | 32 | 64 | 10456 | 628 | 625 | 52.0% | 20.4 | 39.2 | 18.8 | 0 | 606 |
| 2560 | 25 | 64 | 386 | 84 | 84 | 18.4% | 0.9 | 5.2 | 4.3 | 0 | 1422 |
| 3072 | 21 | 64 | 439 | 79 | 79 | 26.5% | 1.3 | 4.9 | 3.7 | 0 | 1501 |
| 3584 | 18 | 64 | 399 | 91 | 91 | 24.4% | 1.4 | 5.7 | 4.3 | 0 | 821 |
| 4096 | 16 | 64 | 423 | 91 | 91 | 29.1% | 1.7 | 5.7 | 4.0 | 0 | 616 |
| total | | | | | | 46.2% | 71.0 | 153.6 | 82.6 | | |

`slab16` (16 KiB slabs for these eight classes; the `slab KiB` and
`nregs` columns are the variant's table as the report prints it):

| class | nregs | slab KiB | curregs | curslabs | nonfull | occupancy | live MiB | slab MiB | pinned MiB | c_on | gated |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1280 | 12 | 16 | 10344 | 1536 | 1451 | 56.1% | 12.6 | 24.0 | 11.4 | 0 | 92 |
| 1536 | 10 | 16 | 10180 | 1787 | 1667 | 57.0% | 14.9 | 27.9 | 13.0 | 0 | 113 |
| 1792 | 9 | 16 | 10274 | 1972 | 1784 | 57.9% | 17.6 | 30.8 | 13.3 | 0 | 189 |
| 2048 | 8 | 16 | 10426 | 2159 | 1872 | 60.4% | 20.4 | 33.7 | 13.4 | 0 | 195 |
| 2560 | 6 | 16 | 379 | 160 | 152 | 39.5% | 0.9 | 2.5 | 1.6 | 0 | 25 |
| 3072 | 5 | 16 | 389 | 164 | 148 | 47.4% | 1.1 | 2.6 | 1.4 | 0 | 21 |
| 3584 | 4 | 16 | 418 | 204 | 174 | 51.2% | 1.4 | 3.2 | 1.8 | 0 | 7 |
| 4096 | 4 | 16 | 412 | 189 | 158 | 54.5% | 1.6 | 3.0 | 1.3 | 0 | 10 |
| total | | | | | | 55.3% | 70.6 | 127.7 | 57.1 | | |

`slab32` (32 KiB):

| class | nregs | slab KiB | curregs | curslabs | nonfull | occupancy | live MiB | slab MiB | pinned MiB | c_on | gated |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1280 | 25 | 32 | 10373 | 784 | 764 | 52.9% | 12.7 | 24.5 | 11.8 | 0 | 144 |
| 1536 | 21 | 32 | 10219 | 916 | 885 | 53.1% | 15.0 | 28.6 | 13.7 | 0 | 224 |
| 1792 | 18 | 32 | 10388 | 1064 | 1013 | 54.2% | 17.8 | 33.2 | 15.5 | 0 | 321 |
| 2048 | 16 | 32 | 10373 | 1173 | 1114 | 55.3% | 20.3 | 36.7 | 16.4 | 0 | 407 |
| 2560 | 12 | 32 | 435 | 123 | 119 | 29.5% | 1.1 | 3.8 | 2.8 | 0 | 509 |
| 3072 | 10 | 32 | 406 | 114 | 110 | 35.6% | 1.2 | 3.6 | 2.4 | 0 | 553 |
| 3584 | 9 | 32 | 371 | 119 | 115 | 34.6% | 1.3 | 3.7 | 2.5 | 0 | 322 |
| 4096 | 8 | 32 | 455 | 135 | 133 | 42.1% | 1.8 | 4.2 | 2.4 | 0 | 215 |
| total | | | | | | 51.3% | 70.9 | 138.4 | 67.4 | | |

jemalloc's merged bins for the same sizes (its report, `Merged arenas
stats`; `pgs` are 4 KiB pages):

| class | regs | slab KiB | curregs | curslabs | nonfull | util |
|---:|---:|---:|---:|---:|---:|---:|
| 1280 | 16 | 20 | 9598 | 730 | 457 | 0.821 |
| 1536 | 8 | 12 | 9460 | 1407 | 629 | 0.840 |
| 1792 | 16 | 28 | 9627 | 725 | 438 | 0.829 |
| 2048 | 2 | 4 | 9649 | 5320 | 985 | 0.906 |
| 2560 | 8 | 20 | 23 | 8 | 7 | 0.359 |
| 3072 | 4 | 12 | 31 | 11 | 6 | 0.704 |
| 3584 | 8 | 28 | 15 | 4 | 1 | 0.468 |
| 4096 | 1 | 4 | 35 | 35 | 0 | 1 |

Reading:

1. The geometry does what it was asked for on the slabs themselves:
   the eight classes go from 46% occupancy to 55% (`slab16`) and 51%
   (`slab32`), the bytes pinned in partially live slabs from 82.6 MiB to
   57.1 (-25.5) and 67.4 (-15.2), and `active` falls by the same 25 and
   17 MiB. jemalloc's 82..91% for the four long-lived classes in 4..28
   KiB slabs is not reached at 16 KiB: with 17 arenas each slab still
   sees one thread's frees, and a 16 KiB slab of 8..12 slots keeps one
   or two survivors per slab about as often as a 64 KiB one keeps five.
2. The process does not get any of it back. RSS at t=600 is 414 -> 419
   -> 407 MiB (+1.2% / -1.6%, the run-to-run spread of this soak) and
   `resident` 417 -> 421 -> 410; what left `active` moved into `resident
   - active`, the Empty-slab inventory, which grew by 29 MiB (`slab16`)
   and 9 MiB (`slab32`). A 64 KiB slab with one survivor is one partial
   slab (pinned, active); cut into four 16 KiB slabs it is one partial
   and three Empty, and the Empty ones sit in their arena's per-class
   inventory until the next burst reuses them or the 10 s decay purges
   them, exactly as the `2026-09-19-x86_64-ruby-soak.md` reading of the
   dirty term describes (each worker arena's inventory is the sum of its
   per-class peaks). With the default arena policy the RSS of this
   process is that inventory, and smaller slabs only change which
   column the same bytes are in. Section 5b below (the same pair under
   `narenas:1`) asks whether geometry is a lever once the inventory is
   gone.
3. Throughput: jobs within 0.4%, GC counts within 1.4%; the refill and
   flush work of 4..12-slot slabs is invisible at this allocation rate.

### 5b. The same pair under `narenas:1` (is geometry a lever once the arena inventory is gone?)

Two more soaks, `ALLOCS=challoc-1arena` (`MALLOC_CONF=narenas:1`, the
diagnostic of `2026-09-19-aarch64-ruby-soak.md` and the lever lane
`ruby-rss` works on), `default` build and `slab16` build
(`...-ruby-soak-1arena.md`, `...-ruby-soak-slab16-1arena.md`):

| measure (MiB) | default, narenas:1 | slab16, narenas:1 | slab16 vs default | (jemalloc, 17 arenas, from above) |
|---|---:|---:|---:|---:|
| peak sampled VmRSS | 186.2 | 191.6 | +2.9% | 179.1 |
| at t=60 | 59.8 | 61.4 | +2.7% | 73.6 |
| at t=300 (after GC.compact) | 130.9 | 139.2 | +6.3% | 127.0 |
| at t=600 | 172.8 | 191.6 | +10.9% | 133.9 |
| lowest right after a periodic full GC | 58.9 | 59.8 | +1.5% | 73.5 |
| allocator: allocated / active / resident / retained, final | 96.9 / 120.6 / 170.8 / 32.4 | 96.4 / 118.3 / 189.7 / 18.0 | active -2.3, resident +18.9 | 94.8 / 107.3 / 123.4 / 94.7 |
| resident - active, final | 50.2 | 71.4 | +21.2 | 16.1 |
| 1280..4096 B: occupancy, live / slab / pinned MiB | 82.2%, 63.0 / 76.6 / 13.6 | 84.0%, 62.5 / 74.4 / 11.9 | pinned -1.7 | util 0.82..0.91 (1280..2048) |
| jobs (600 s) | 154902 | 153775 | -0.7% | 151262 |

Per class with one arena, `default`: 1280 B 81.7% (236 slabs), 1536
84.6% (274), 1792 83.1% (330), 2048 85.0% (364), the burst classes
under 0.5 MiB each; `slab16`: 86.2% (945 slabs of 12), 86.8% (1112),
86.9% (1254), 87.4% (1407). Reading: once every thread's frees refill
every thread's cache from the same slabs, the eight classes are already
at 82% in 64 KiB slabs, jemalloc's own 0.82..0.91, and 16 KiB slabs add
two points and 1.7 MiB; meanwhile `resident - active` grows by 21 MiB
(more slabs fall Empty between bursts and wait in the inventory, fewer
reach the purge: retained 32 -> 18) and RSS at t=600 is 11% higher. On
this shape the geometry is not a second lever beside the arena count:
the arena count is the whole Ruby gap (414 -> 173 MiB), and on top of
it the smaller slabs give back 1.7 MiB of pinned bytes and cost 19 MiB
of dirty ones.

## 6. Comparison and the trade

Both architectures agree on every row; the joint reading and the
recommendation (do not change the geometry: the change buys slab
occupancy the Ruby process never sees as RSS, is redundant with the
arena lever, and leaves Valkey untouched) are section 6 of
`2026-09-21-aarch64-policy-cost-geometry.md`. This VM's own numbers for
it: no cost on the fast tier (`quick` GO for both variants, `ab` within
2%, instructions per operation identical), 10 points of occupancy and
25.5 MiB less pinned in the eight classes under Ruby for +1.2% RSS
(414 -> 419 MiB at t=600; `resident - active` +29 MiB), and under
`narenas:1` 1.7 MiB less pinned for +11% RSS (173 -> 192 MiB).
