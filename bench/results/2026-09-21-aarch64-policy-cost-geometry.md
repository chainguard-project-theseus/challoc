# Policy cost: 16 and 32 KiB slabs for the 1280..4096-byte classes, 2026-09-21, aarch64 (lane policy-cost)

Measurement only, no default changed, nothing under `src/` changed (the
variants are A/B copies of the tree in gitignored `.tmp-slab16` and
`.tmp-slab32`, made by `bench/geometry-variant.sh`). The question
(coordinator, 2026-09-21): the eight classes from 1280 to 4096 bytes
share 64 KiB slabs, a geometry tuned for Valkey
(`2026-09-17-*-gce-slab-geometry.md`); under the Ruby soak those
classes hold the long-lived objects at 48..52% occupancy and pin about
twice the bytes jemalloc pins with 4..28 KiB slabs
(`2026-09-19-{aarch64,x86_64}-ruby-soak.md`). What do 16 KiB (`slab16`)
and 32 KiB (`slab32`) slabs for exactly those classes cost on the hot
path, and what do they buy under Ruby and Valkey? The x86_64 rows are
in `2026-09-21-x86_64-policy-cost-geometry.md`; the compacting-cache
rows of the same lane in `2026-09-21-aarch64-policy-cost-compact.md`.
The lane `ruby-rss` attacks the same RSS term by arena policy in
parallel; the per-class occupancy tables below (section 5) are what
tells whether geometry is a second, independent lever.

- host: `challoc-policy-cost-arm64` (c4a-standard-16, Google Axion,
  Neoverse V2, CPU part 0xd4f), 16 vCPUs, Ubuntu 24.04 kernel
  `7.0.0-1011-gcp`, 4 KiB pages, THP `madvise`; container: Wolfi
  (rustc 1.98.1); jemalloc = Wolfi's libjemalloc.so.2 5.3.1-r3 (the
  aarch64 build with `Page size: 65536`). Zone us-east4-a. Nothing else
  ran on the VM.
- commit: `f53418d` on `loop/policy-cost` (harness hooks only; `src/`
  is byte for byte main `2078f67`).
- builds (`quantum16`, sha256 prefixes): `default` = the tree,
  `4d04e89158b5bcc4`; `slab16` = `.tmp-slab16`, `c29a3832eef652ca`;
  `slab32` = `.tmp-slab32`, `53cce574a16171cf`. The variant is one
  early return inserted in `layout::lg_slab_for` in front of the medium
  rule `max(64 KiB, 16 * size)`: `if size > 1024 && size <= 4096 {
  return 14; }` (`slab16`) or `return 15;` (`slab32`). Everything
  derived from the table follows (`nslots`, `slab_bits`,
  `slabs_per_line`, `compact_slots`, the guard interval, the report's
  `slab_size`/`nregs`, the utilization queries); the descriptor stride
  stays the 16-byte header (every affected slab has at most 25 slots,
  so at least 16 slabs share a bitmap line and
  `slab::strides_match_every_page` holds); `BIN_MAX` and the cache
  capacity ladder do not move.
- the per-class table each build produces (`arenas.bin.<j>.slab_size` /
  `nregs`; confirmed from the variants' `malloc_stats_print` in the Ruby
  soak, section 5). Waste is `slab - nregs * size` per slab:

| class | default: slab KiB / nregs / waste | slab16: slab KiB / nregs / waste | slab32: slab KiB / nregs / waste | jemalloc (x86_64 4 KiB pages): slab KiB / regs |
|---:|---|---|---|---|
| 1280 | 64 / 51 / 256 B (0.4%) | 16 / 12 / 1024 B (6.3%) | 32 / 25 / 768 B (2.3%) | 20 / 16 |
| 1536 | 64 / 42 / 1024 B (1.6%) | 16 / 10 / 1024 B (6.3%) | 32 / 21 / 512 B (1.6%) | 12 / 8 |
| 1792 | 64 / 36 / 1024 B (1.6%) | 16 / 9 / 256 B (1.6%) | 32 / 18 / 512 B (1.6%) | 28 / 16 |
| 2048 | 64 / 32 / 0 | 16 / 8 / 0 | 32 / 16 / 0 | 8 / 4 |
| 2560 | 64 / 25 / 1536 B (2.3%) | 16 / 6 / 1024 B (6.3%) | 32 / 12 / 2048 B (6.3%) | 20 / 8 |
| 3072 | 64 / 21 / 1024 B (1.6%) | 16 / 5 / 1024 B (6.3%) | 32 / 10 / 2048 B (6.3%) | 12 / 4 |
| 3584 | 64 / 18 / 1024 B (1.6%) | 16 / 4 / 2048 B (12.5%) | 32 / 9 / 512 B (1.6%) | 28 / 8 |
| 4096 | 64 / 16 / 0 | 16 / 4 / 0 | 32 / 8 / 0 | 4 / 1 |

  Every other class is unchanged (64 KiB below 512 bytes, 4..16 KiB for
  512..1024, `16 * size` from 5120 up).
- method: `challoc-bench quick` (`OLD=tree NEW=.tmp-slab16`, then
  `.tmp-slab32`; the 5% gate, recorded, nothing landed), `challoc-bench
  ab` (5 rounds after a warm-up, interleaved, four variants, `larson1
  larson10 larson16 xmalloc alloc-test1 alloc-testN valkey1
  valkey1-free`), `challoc-bench quick-perf` on `larson1` and `valkey1`
  with the three builds and jemalloc, the Ruby soak (`bench/ruby-soak.sh`,
  600 s, 16 threads, `CHALLOC_SRC=.tmp-slab16|.tmp-slab32`) for all three
  builds and jemalloc, and the Valkey soak (`default` vs `slab16`, this
  architecture only).

## 1. `quick` (one round each; the 5% gate)

`slab16` (new) vs `default` (old), load 3.53 at start, 127 s, `old
4d04e89158b5bcc4, new c29a3832eef652ca`:

| row | unit | old (default) | new (slab16) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.1 (15.9 MiB) | 42.1 (15.9 MiB) | 45.0 (18.4 MiB) | +0.0% | +6.9% |
| larson10 | M ops/s | 366.8 (111.5 MiB) | 365.7 (111.0 MiB) | 381.6 (131.6 MiB) | +0.3% | +4.3% |
| xmalloc | M frees/s | 575.495 (60.5 MiB) | 574.980 (58.1 MiB) | 471.848 (76.5 MiB) | +0.1% | -17.9% |
| alloc-test1 | s | 2.542 (13.1 MiB) | 2.508 (13.1 MiB) | 2.423 (12.3 MiB) | -1.3% | +3.5% |
| valkey1 | M ops/s | 7.819 (584.3 MiB) | 8.559 (584.7 MiB) | 8.297 (585.5 MiB) | -8.6% | -3.1% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24073379 ops/s |

`quick: go`. `slab32` (new) vs `default` (old), load 3.48, 128 s, `new
53cce574a16171cf`:

| row | unit | old (default) | new (slab32) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.9 (15.9 MiB) | 42.0 (15.9 MiB) | 45.1 (19.0 MiB) | -0.2% | +7.4% |
| larson10 | M ops/s | 368.5 (110.9 MiB) | 363.2 (111.4 MiB) | 386.3 (132.2 MiB) | +1.5% | +6.4% |
| xmalloc | M frees/s | 576.718 (53.9 MiB) | 574.030 (53.0 MiB) | 481.928 (75.4 MiB) | +0.5% | -16.0% |
| alloc-test1 | s | 2.762 (13.1 MiB) | 2.754 (13.1 MiB) | 2.571 (12.3 MiB) | -0.3% | +7.1% |
| valkey1 | M ops/s | 6.600 (584.3 MiB) | 6.515 (584.5 MiB) | 6.599 (585.3 MiB) | +1.3% | +1.3% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3461 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24376063 ops/s |

`quick: go`. Both variants pass the gate on every row; the churn row
(class 1024, untouched by the change) compacts as before. larson's
sizes are 8..1000 bytes and the churn probe's is 1024, so none of the
fast-tier rows allocates in a changed class except the model (values
16..1024 bytes plus the 40/24/16-byte objects: none either; its
1280..4096-byte traffic is nil). The rows say "nothing else moved".

## 2. `ab` (5 rounds, interleaved): `default`, `slab16`, `slab32`, `jemalloc`

Load 3.18 at start, 4.87 at the end, 1259 s.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | default | 41.9 41.9 42.0 42.0 42.0 | 16268 |
| larson1 | slab16 | 41.9 41.9 42.0 42.0 42.0 | 16272 |
| larson1 | slab32 | 41.7 41.9 41.9 41.9 42.0 | 16400 |
| larson1 | jemalloc | 44.9 45.1 45.1 45.1 45.2 | 19092 |
| larson10 | default | 361.4 363.1 369.1 369.2 369.8 | 114192 |
| larson10 | slab16 | 361.4 365.4 367.0 367.0 368.5 | 114812 |
| larson10 | slab32 | 359.2 360.9 363.5 368.6 369.2 | 114316 |
| larson10 | jemalloc | 378.6 379.5 380.5 385.3 387.8 | 134616 |
| larson16 | default | 590.4 590.7 594.7 595.5 595.8 | 169868 |
| larson16 | slab16 | 581.9 582.5 587.5 593.6 595.6 | 168464 |
| larson16 | slab32 | 585.8 586.4 588.8 589.4 591.6 | 169744 |
| larson16 | jemalloc | 612.5 614.2 616.9 618.2 621.1 | 211756 |
| xmalloc | default | 574.368 576.583 576.606 576.867 577.706 | 54868 |
| xmalloc | slab16 | 576.287 576.422 576.536 576.899 576.953 | 54480 |
| xmalloc | slab32 | 576.079 576.417 576.694 577.019 577.670 | 56788 |
| xmalloc | jemalloc | 474.439 474.774 475.547 479.613 481.666 | 80404 |
| alloc-test1 | default | 2.556 2.615 2.624 2.627 2.646 | 13464 |
| alloc-test1 | slab16 | 2.607 2.611 2.624 2.633 2.634 | 13464 |
| alloc-test1 | slab32 | 2.601 2.617 2.618 2.637 2.652 | 13464 |
| alloc-test1 | jemalloc | 2.467 2.483 2.491 2.494 2.516 | 12636 |
| alloc-testN | default | 2.288 2.292 2.296 2.305 2.309 | 16848 |
| alloc-testN | slab16 | 2.299 2.299 2.300 2.300 2.301 | 17040 |
| alloc-testN | slab32 | 2.279 2.280 2.281 2.301 2.319 | 16968 |
| alloc-testN | jemalloc | 2.197 2.200 2.201 2.207 2.245 | 16864 |
| valkey1 | default | 7.280 7.404 7.445 7.471 7.957 | 598740 |
| valkey1 | slab16 | 7.197 7.280 7.388 7.388 7.855 | 599104 |
| valkey1 | slab32 | 7.074 7.328 7.339 7.446 7.683 | 598864 |
| valkey1 | jemalloc | 7.313 7.332 7.357 7.452 7.563 | 599768 |
| valkey1-free | default | 6.861 7.208 7.210 7.292 7.742 | 598740 |
| valkey1-free | slab16 | 6.849 7.156 7.211 7.261 7.275 | 599124 |
| valkey1-free | slab32 | 6.851 6.887 7.072 7.363 7.513 | 598868 |
| valkey1-free | jemalloc | 7.018 7.160 7.170 7.185 7.259 | 599640 |

| program | unit | default | default RSS MiB | slab16 | slab16 RSS MiB | slab32 | slab32 RSS MiB | jemalloc | jemalloc RSS MiB | slab16 vs default | slab32 vs default | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.0 | 15.9 | 42.0 | 15.9 | 41.9 | 16.0 | 45.1 | 18.6 | +0.0% | +0.2% | -6.9% |
| larson10 | M ops/s | 369.1 | 111.5 | 367.0 | 112.1 | 363.5 | 111.6 | 380.5 | 131.5 | +0.6% | +1.5% | -3.0% |
| larson16 | M ops/s | 594.7 | 165.9 | 587.5 | 164.5 | 588.8 | 165.8 | 616.9 | 206.8 | +1.2% | +1.0% | -3.6% |
| xmalloc | M frees/s | 576.606 | 53.6 | 576.536 | 53.2 | 576.694 | 55.5 | 475.547 | 78.5 | +0.0% | -0.0% | +21.3% |
| alloc-test1 | s | 2.624 | 13.1 | 2.624 | 13.1 | 2.618 | 13.1 | 2.491 | 12.3 | +0.0% | -0.2% | -5.1% |
| alloc-testN | s | 2.296 | 16.5 | 2.300 | 16.6 | 2.281 | 16.6 | 2.201 | 16.5 | +0.2% | -0.7% | -4.1% |
| valkey1 | M ops/s | 7.445 | 584.7 | 7.388 | 585.1 | 7.339 | 584.8 | 7.357 | 585.7 | +0.8% | +1.4% | +1.2% |
| valkey1-free | M ops/s | 7.210 | 584.7 | 7.211 | 585.1 | 7.072 | 584.8 | 7.170 | 585.6 | -0.0% | +2.0% | +0.6% |

Positive percentages mean the variant did worse than `default`.

Reading: every row within 2% of the default with the sorted lists
interleaved (larson16's +1.2% for `slab16` is 581.9..595.6 against
590.4..595.8, the widest gap in the table and inside the row's spread);
the three challoc builds are indistinguishable on these programs, which
is what the section 1 note predicts: none of them allocates in the
changed classes to speak of. The cost of the change is therefore not on
this table; what it could cost is the refill and flush work of a class
whose slab holds 4..12 slots instead of 16..51 (a refill of `cap / 2`
slots touches more descriptors, one lock hold per `MAX_PICKS` 16 slabs,
as `2026-09-17-*-gce-slab-geometry.md` priced for the 512..1024 classes
at 1..2.5% of larson, whose sizes did live there), which only a consumer
allocating those sizes shows; section 5's Ruby jobs and CPU time and
section 6's Valkey rows are the workloads where it would.

## 3. `quick-perf`: per-operation counters (larson1, valkey1), three builds and jemalloc

PENDING (the step ran with a wrong path and is rerun after the soaks;
the numbers land here).

## 4. Valkey soak (20 min load + 60 s settle): `default` vs `slab16`

`bench/valkey-soak.sh` as in the compact file (600 s write, 600 s
DEL:SET 4:1, 60 s settle, `activedefrag yes`, `OCCUPANCY=1`); `default`
and jemalloc are the compact file's section 5 run (the same afternoon,
the same VM), `slab16` a third invocation with `ALLOCS=challoc
CHALLOC_LIB=/src/target/valkey/challoc-slab16/release`: the `.tmp-slab16`
tree built with `--features quantum8` (sha256 `593c65f724480f17`;
the default quantum8 build is `ddfc69c202484485`) and preloaded under
the `libchalloc.so.0` soname in front of the server binary's RUNPATH;
the script checked that `/proc/<pid>/maps` shows the variant and
recorded it. Raw files in `target/valkey/pc-slab16/` on the VM.

| measure | default | slab16 | jemalloc | slab16 vs default |
|---|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 920986 | 938076 | 878208 | +1.9% |
| write: valkey-benchmark p99 ms | 1.071 | 1.047 | 1.119 | -2.2% |
| write: memtier req/s | 964505 | 977511 | 936581 | +1.3% |
| write: memtier p99 ms | 0.959 | 0.935 | 0.983 | -2.5% |
| delete: valkey-benchmark req/s | 1423429 | 1422139 | 1452604 | -0.1% |
| delete: valkey-benchmark p99 ms | 0.719 | 0.719 | 0.703 | +0.0% |
| delete: memtier req/s | 1258879 | 1266443 | 1273913 | +0.6% |
| delete: memtier p99 ms | 0.767 | 0.751 | 0.759 | -2.1% |
| peak RSS MiB (VmHWM) | 579.3 | 580.8 | 573.5 | +0.3% |
| RSS MiB at end of write phase | 579.0 | 580.6 | 573.1 | +0.3% |
| RSS MiB at end of delete phase | 130.3 | 131.1 | 128.6 | +0.6% |
| RSS MiB after settle | 130.3 | 131.1 | 128.5 | +0.6% |
| allocator_frag_ratio after settle | 1.81 | 1.80 | 1.79 | |
| active_defrag_hits | 229711 | 238419 | 240548 | +3.8% |
| total_active_defrag_time ms | 2344 | 2241 | 2546 | -4.4% |
| keys after settle | 105416 | 105886 | 102818 | |

(Positive: `slab16` larger.) Per class after the settle: the 512..1024
classes, untouched by the variant, end at 57.2% occupancy and 34.7 MiB
pinned (default 57.4%, 34.3); the changed classes hold 0.9 MiB live in
1.6 MiB of slabs (default 2.2 MiB): 1280 B at 56.5% in 98 slabs of 12
against 45.3% in 29 slabs of 51, the other seven classes one slab each
with 1..4 live values, 16 KiB instead of 64.

Reading: the soak's values are 16..1024 bytes (memtier) and 512
(valkey-benchmark), so the 1280..4096 classes carry 0.9 MiB of it and
the variant has 0.6 MiB to give back, which is what it gives: RSS +0.6%
after settle (one run's spread), peak +0.3%, every throughput and p99
row within the soak's 1..3% run-to-run spread with `slab16` ahead in
the write phase. The change is invisible to Valkey either way, which is
the criterion for a geometry change aimed at another consumer: the
Valkey criteria (write-phase p99 and RSS) do not move.

## 5. Ruby soak (600 s, 16 threads): `default`, `slab16`, `slab32`, jemalloc

PENDING.

## 6. Comparison and the trade

PENDING.
