# challoc slab geometry (4 KiB slabs for the 512..1024-byte classes) A/B, 2026-09-17, x86_64 (GCE reference VM, us-east4-a)

Phase 1 acceptance step 3 (c), the slab-geometry step that the soak 3
diagnosis (`2026-09-17-aarch64-gce-compacting-cache-soak3.md`) pointed
to: the 512..1024-byte classes move from 64 KiB slabs to the smallest
power of two from 4 KiB up whose internal waste is at most a sixteenth
of the slab. Two forms were measured: `a16`, a waste bound of 1/16
(4 KiB for 512, 640, 768 and 1024 bytes, 8 KiB for 896; commit f27d736,
run 1's `new`), and `new`, a bound of 1/32 (4 KiB for 512 and 1024,
8 KiB for 896, 16 KiB for 640 and 768; commit e6b89c3, run 2), the
form adopted after the aarch64 soak of the first put 21 MiB of the 640
class's internal waste on the peak RSS. This is the x86_64 throughput
row per CLAUDE.md; the
aarch64 rows, the xmalloc-test rerun and the 20-minute Valkey soak are in
`2026-09-17-aarch64-gce-slab-geometry.md`. VM numbers are never compared
with Mac numbers.

- host: challoc-bench-amd64 (c4-standard-16, Intel Xeon Platinum 8581C),
  16 vCPUs, Ubuntu 24.04 kernel 7.0.0-1011-gcp, 4 KiB pages,
  `guard_install=yes`; container: Wolfi (rustc 1.98.1, gcc 15.3.0);
  jemalloc = Wolfi's libjemalloc.so.2 (5.3.1). Load average 0.01 at the
  start; the rounds raise it to about 8.
- method: `bench/build.sh` programs; `old` = the pre-change library built
  from `git archive 4021694` into `target/old`; `new` = the working tree
  that became f27d736 (`LG_MIN_SLAB` 12, `.tmp-new` copy) into
  `target/new` (run 1) or the tree that became e6b89c3 (`SLAB_WASTE_DIV`
  32, run 2); `s13` = run 1's tree with `LG_MIN_SLAB` 13 (8 KiB slabs for
  all five classes: 16, 12, 10, 9 and 8 slots) into `target/s13`; `a16` =
  run 2's tree with `SLAB_WASTE_DIV` 16, i.e. run 1's geometry, into
  `target/a16`.
  5 rounds plus one warm-up, variants interleaved per round, every run's
  score listed sorted; RSS is `rt`'s median maxrss in KiB. Driver:
  `.tmp-it15-ab.sh` in the iteration log (the iteration-13 recipe).
- programs: `larson 5 8 1000 5000 100 4141 1` (larson1) and `... 10`
  (larson10), M ops/s, higher is better; `xmalloc-test -w 10 -t 5 -s 64`,
  M frees/s, higher is better; `alloc-test 1` and `alloc-test 16`,
  seconds, lower is better.

## Run 1: old, new (waste bound 1/16, f27d736), s13 (8 KiB for all five classes), jemalloc

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 49.0 49.5 49.8 50.4 50.7 | 12992 |
| larson1 | new | 46.4 48.6 49.0 49.2 49.3 | 13148 |
| larson1 | s13 | 48.4 48.9 49.3 49.3 49.9 | 13080 |
| larson1 | jemalloc | 44.0 44.5 45.2 45.6 46.0 | 18280 |
| larson10 | old | 241.2 258.2 258.4 265.8 266.0 | 66800 |
| larson10 | new | 245.2 251.4 251.6 253.4 255.7 | 67056 |
| larson10 | s13 | 248.3 252.8 253.5 257.2 259.0 | 67376 |
| larson10 | jemalloc | 291.1 294.8 296.0 300.2 303.5 | 95468 |
| xmalloc-test | old | 504.0 510.2 515.1 515.2 515.8 | 39716 |
| xmalloc-test | new | 515.2 520.9 526.8 527.8 533.8 | 39856 |
| xmalloc-test | s13 | 518.8 521.3 525.4 527.4 527.7 | 39680 |
| xmalloc-test | jemalloc | 332.1 332.5 333.3 334.2 334.4 | 96132 |
| alloc-test1 | old | 3.061 3.096 3.166 3.180 3.230 | 13980 |
| alloc-test1 | new | 3.055 3.059 3.077 3.084 3.118 | 14028 |
| alloc-test1 | s13 | 3.078 3.106 3.106 3.129 3.151 | 14088 |
| alloc-test1 | jemalloc | 2.974 3.008 3.013 3.016 3.023 | 13464 |
| alloc-testN | old | 4.323 4.324 4.342 4.348 4.435 | 17444 |
| alloc-testN | new | 4.277 4.306 4.308 4.318 4.349 | 17700 |
| alloc-testN | s13 | 4.309 4.316 4.325 4.330 4.340 | 17792 |
| alloc-testN | jemalloc | 4.077 4.080 4.093 4.139 4.160 | 17156 |

Reading (medians, new vs old): larson1 49.8 -> 49.0 (-1.6%), larson10
258.4 -> 251.6 (-2.6%; old's own spread is 241..266), xmalloc-test
515.1 -> 526.8 (+2.3%; the 64-byte class is untouched), alloc-test1
3.166 -> 3.077 s (-2.8%, faster), alloc-testN 4.342 -> 4.308 s (-0.8%).
RSS moves by under 1.5% (the descriptor array of a 4 KiB-slab class is
64 bytes per 4 KiB instead of per 64 KiB). Every row is within the
plan's 3% of the pre-change library. The 8 KiB form (`s13`) is not
measurably cheaper than 4 KiB on any row (larson1 49.3, larson10 253.5,
inside new's spread), so the geometry that gives the soak the most room
(4 slots per slab for the 1 KiB class, jemalloc's own count) is the one
adopted. larson's sizes are 8..1000 bytes, so 55% of its allocations
(449..1000 bytes) now live in 4 KiB slabs and each refill or flush of
those classes touches 16 descriptors instead of one; the 1.6..2.6% is
that cost, one lock hold per refill kept by `MAX_PICKS` 16.

## Run 2: old, new (waste bound 1/32, adopted), a16 (waste bound 1/16, run 1's geometry), jemalloc

Same VM, freshly started (load 0.01 at the start), same programs and
rounds.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 48.2 48.2 49.2 49.6 51.2 | 12840 |
| larson1 | new | 48.0 48.1 48.6 48.9 49.6 | 12832 |
| larson1 | a16 | 47.9 48.2 48.3 48.8 50.7 | 12840 |
| larson1 | jemalloc | 44.6 44.9 45.8 46.2 46.6 | 15732 |
| larson10 | old | 250.0 252.7 260.1 263.7 266.7 | 66876 |
| larson10 | new | 248.2 249.1 259.2 261.0 261.2 | 66832 |
| larson10 | a16 | 242.5 246.4 249.6 250.6 252.5 | 66552 |
| larson10 | jemalloc | 290.8 290.9 296.7 298.5 300.8 | 93360 |
| xmalloc-test | old | 480.6 505.1 507.8 510.6 514.7 | 39740 |
| xmalloc-test | new | 506.9 519.4 520.5 521.6 522.5 | 39708 |
| xmalloc-test | a16 | 513.1 520.3 521.3 533.8 536.7 | 39740 |
| xmalloc-test | jemalloc | 329.8 332.1 332.1 332.4 337.1 | 96128 |
| alloc-test1 | old | 2.984 3.013 3.242 3.280 3.325 | 13828 |
| alloc-test1 | new | 3.017 3.035 3.160 3.304 3.334 | 13992 |
| alloc-test1 | a16 | 2.906 2.946 3.058 3.179 3.319 | 13944 |
| alloc-test1 | jemalloc | 2.816 2.917 3.065 3.151 3.221 | 13368 |
| alloc-testN | old | 4.319 4.370 4.395 4.409 4.410 | 17288 |
| alloc-testN | new | 4.270 4.321 4.376 4.399 4.434 | 17832 |
| alloc-testN | a16 | 4.325 4.333 4.334 4.370 4.505 | 17548 |
| alloc-testN | jemalloc | 4.083 4.106 4.147 4.191 4.199 | 16948 |

Reading (medians, new vs old): larson1 49.2 -> 48.6 (-1.2%), larson10
260.1 -> 259.2 (-0.3%), xmalloc-test 507.8 -> 520.5 (+2.5%), alloc-test1
3.242 -> 3.160 s (-2.5%, faster), alloc-testN 4.395 -> 4.376 s (-0.4%);
every row within the plan's 3%. The 1/16 form (`a16`) costs more on
larson10 here (249.6, -4.0% in this run, -2.6% in run 1): with 640 and
768 at 4 KiB, 55% of larson's allocations were in 4..8 KiB slabs; at
1/32 only the 512 and 1024 classes (449..512 and 897..1000 bytes, 17%)
are, and the 640 and 768 classes have 25 and 21 slots per 16 KiB slab.
