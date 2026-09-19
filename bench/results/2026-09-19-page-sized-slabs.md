# Page-sized slabs when the page is larger than the slab (Phase 2.5 task 4e)

Date 2026-09-19. Base dd5f5c8; the candidate is this iteration's tree (the
commit hash is in PROGRESS.md). Reference VMs: the C4A (Neoverse-V2, 16 vCPU)
and the C4 (8581C, 16 vCPU) in `us-east4-a`, kernel 7.0.0-1011-gcp, 4 KiB
pages, so everything measured there is the cost of the mechanism while it is
*inactive*. The 64 KiB acceptance is in
`2026-09-19-aarch64-64k-valkey-soak-2.md`.

## The change

Design 3: when the kernel page is larger than a class's compile-time slab,
the class takes the page as its slab size, up to 64 KiB, once at init
(`layout::adopt_page_geometry`, called by `api::init` right after the page
size probe). The 512..1024-byte classes are the only ones with slabs below
64 KiB, so the geometry per page size is:

| class | 4 KiB pages (the table) | 16 KiB pages | 64 KiB pages | jemalloc at `LG_PAGE 16` (`regs`) |
|---:|---|---|---|---|
| 512 | 4 KiB, 8 slots | 16 KiB, 32 | 64 KiB, 128 | 128 |
| 640 | 16 KiB, 25 | 16 KiB, 25 | 64 KiB, 102 | 512 (320 KiB slabs) |
| 768 | 16 KiB, 21 | 16 KiB, 21 | 64 KiB, 85 | 256 (192 KiB) |
| 896 | 8 KiB, 9 | 16 KiB, 18 | 64 KiB, 73 | 512 (448 KiB) |
| 1024 | 4 KiB, 4 | 16 KiB, 16 | 64 KiB, 64 | 64 |

Every other class keeps its table slab (64 KiB or more). Consequences that
follow from the table entry, checked on the host
(`layout::tests::page_sized_geometry_for_large_pages`,
`slab::tests::strides_are_the_same_for_every_page_geometry`) and in the
container (`api::print::tests::slabs_are_never_smaller_than_the_page`):
`nslots`, `slab_bits` (128 bits for the first four, 64 for 1024: four or
eight slabs per bitmap line), `slabs_per_line`, the descriptor stride (the
16-byte header for these classes at every page size, so `slab::DESC_STRIDE`
stays compile-time, asserted at compile time), `compact_slots == nslots`,
the guard interval (unchanged: the guard unit is 64 KiB of slabs either way),
`SlabTable::slabs_per_page == 1` for every class (the page groups of design 5
are dormant), `arenas.bin.<j>.slab_size` / `nregs` and the utilization
queries report the active geometry, `BIN_MAX` and the cache ladder do not
move. The report prints `Page-sized slabs: classes 19..23 (512..1024 bytes)
at 65536 bytes (4096..16384 in the table)` after `Page size:` when the rule
is active (nothing at 4 KiB).

## The form, and why

The plan offered three forms: a pointer to one of two compile-time tables on
`Shared`'s first line, the shift folded into the per-class geometry the fast
paths already load, or a runtime shift where `lg_slab` is read. Chosen: the
second, as the table itself. `layout::CLASSES` is the compile-time table
(`BASE_CLASSES`) in a `Sync` `UnsafeCell` at a fixed address, and init
rewrites `lg_slab`, `nslots` and `slab_bits` of the raised classes in place
before the allocator is published; `size` and `recip` are never written, so
the one field pre-init callers read (`nallocx`) is constant. The `free` and
`sdallocx` fast paths (`Regions::slab_slot`, `Regions::slot_in_class`) load
the class line exactly as before: `adrp`/`add` to a link-time constant plus
the class index, one load per field. A table pointer would have put a
dependent load in front of the sized path's only geometry load (its class
comes from the size hint, so today nothing precedes that load); a runtime
shift is a compare-and-select per operation.

Evidence, from the disassembly of the two builds in the dev container
(aarch64, `objdump -d`, addresses and page-offset immediates normalised,
`.tmp-4e-disasm*.sh`): the exported fast paths `je_malloc` (43
instructions), `calloc` (127), `dallocx` (35), `je_mallocx` (43),
`aligned_alloc`, `je_nallocx`, `je_sallocx` are instruction for instruction
identical; `je_sdallocx` is 159 -> 160 with the fast path (the size-hint
classification, the guard mask, the cache push) identical and the extra
`add` inside the `MALLOCX_LG_ALIGN` class scan, a slow path where LLVM now
materialises the table's page offset in two steps. Of 362 functions, 262
are byte-identical after normalisation and 14 change their instruction
count, all slow paths: `pop_compact` 150 -> 154, `free_direct` 136 -> 139,
`alloc_with` 379 -> 383, `claim_with` 2361 -> 2365, `fragmented` 69 -> 73,
`ensure_cache` 163 -> 183, `init` (the new writer), `malloc_stats_print`
(the new line). The pattern in each is a `cbz` on `nslots` (or `size`)
before a `udiv`, plus the panic stub: with the table in writable memory LLVM
can no longer prove the divisors non-zero, so Rust's division check stays.
The old table was `r` (rodata), the new is `d` (data); it is 832 bytes.

## The price of the inactive mechanism (`quick-perf`, 4 KiB reference VMs)

`bench/gce/challoc-bench quick-perf <vm> OLD=dd5f5c8 ROWS="larson1 valkey1
valkey1-free" VARIANTS="old new"`; `sudo perf stat` on the container, one run
per (row, variant), then a second invocation with the variants interleaved
three times. (`bench/gce/quick-perf.sh` had exited silently right after its
header whenever `EVENTS` was the four default events: the `grep -v` that
finds the extra columns matched nothing under `set -e -o pipefail`; fixed
in this commit, so the first run's empty table is not a result.)

### First run, one each

C4A (aarch64):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 42.07 | 71.2 | 249.4 | 2.096 | 4.077 |
| larson1 | new | 41.70 | 71.8 | 249.4 | 2.095 | 4.096 |
| valkey1 | old | 12.97 | 233.6 | 232.9 | 3.074 | 2.810 |
| valkey1 | new | 12.92 | 234.3 | 234.3 | 3.084 | 2.787 |
| valkey1-free | old | 12.63 | 239.6 | 236.4 | 4.050 | 3.909 |
| valkey1-free | new | 11.91 | 254.7 | 239.8 | 4.063 | 3.900 |

C4 (x86_64):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 44.44 | 74.0 | 234.3 | 1.597 | 0.002 |
| larson1 | new | 45.35 | 72.5 | 234.1 | 1.585 | 0.002 |
| valkey1 | old | 23.22 | 153.7 | 231.1 | 3.323 | 0.909 |
| valkey1 | new | 24.36 | 147.9 | 226.7 | 3.274 | 0.902 |
| valkey1-free | old | 24.17 | 149.3 | 227.9 | 4.592 | 0.928 |
| valkey1-free | new | 24.00 | 150.3 | 226.8 | 4.570 | 0.927 |

### Second run, variants interleaved three times

C4A (aarch64), load at start 0.92:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1-free | old | 12.46 | 243.1 | 236.8 | 4.049 | 3.932 |
| valkey1-free | new | 12.65 | 239.3 | 238.1 | 4.065 | 3.907 |
| valkey1-free | old | 12.60 | 240.5 | 236.9 | 4.053 | 3.965 |
| valkey1-free | new | 12.54 | 241.8 | 237.8 | 4.060 | 3.897 |
| valkey1-free | old | 12.53 | 242.0 | 238.3 | 4.065 | 3.922 |
| valkey1-free | new | 12.15 | 249.5 | 240.1 | 4.071 | 3.912 |
| valkey1 | old | 12.37 | 245.0 | 233.7 | 3.077 | 2.853 |
| valkey1 | new | 12.66 | 239.7 | 235.9 | 3.089 | 2.833 |
| valkey1 | old | 12.75 | 237.8 | 234.4 | 3.085 | 2.818 |
| valkey1 | new | 12.70 | 238.8 | 233.2 | 3.078 | 2.794 |
| valkey1 | old | 12.51 | 242.4 | 235.4 | 3.086 | 2.852 |
| valkey1 | new | 12.61 | 240.4 | 234.1 | 3.081 | 2.797 |
| larson1 | old | 42.05 | 71.2 | 249.4 | 2.096 | 4.120 |
| larson1 | new | 41.97 | 71.4 | 249.4 | 2.101 | 4.077 |
| larson1 | old | 42.10 | 71.1 | 249.4 | 2.094 | 4.074 |
| larson1 | new | 41.93 | 71.4 | 249.4 | 2.103 | 4.015 |
| larson1 | old | 42.13 | 71.1 | 249.4 | 2.090 | 4.135 |
| larson1 | new | 41.92 | 71.4 | 249.4 | 2.096 | 4.022 |

C4 (x86_64), load at start 0.41:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1-free | old | 23.77 | 151.2 | 227.6 | 4.593 | 0.928 |
| valkey1-free | new | 24.26 | 148.4 | 227.5 | 4.583 | 0.928 |
| valkey1-free | old | 24.15 | 150.5 | 226.6 | 4.570 | 0.928 |
| valkey1-free | new | 24.04 | 149.7 | 228.4 | 4.614 | 0.927 |
| valkey1-free | old | 22.95 | 157.4 | 222.9 | 4.484 | 0.926 |
| valkey1-free | new | 23.88 | 151.2 | 227.9 | 4.599 | 0.930 |
| valkey1 | old | 24.23 | 148.8 | 227.8 | 3.292 | 0.896 |
| valkey1 | new | 24.26 | 147.8 | 227.1 | 3.305 | 0.905 |
| valkey1 | old | 23.70 | 151.9 | 229.9 | 3.301 | 0.909 |
| valkey1 | new | 24.40 | 147.9 | 226.0 | 3.263 | 0.906 |
| valkey1 | old | 24.00 | 150.1 | 228.4 | 3.297 | 0.906 |
| valkey1 | new | 24.09 | 149.6 | 228.1 | 3.289 | 0.906 |
| larson1 | old | 45.58 | 72.1 | 234.1 | 1.592 | 0.002 |
| larson1 | new | 45.42 | 72.5 | 234.2 | 1.595 | 0.002 |
| larson1 | old | 44.68 | 73.7 | 234.1 | 1.607 | 0.002 |
| larson1 | new | 45.39 | 72.5 | 234.1 | 1.587 | 0.002 |
| larson1 | old | 45.30 | 72.7 | 234.2 | 1.585 | 0.002 |
| larson1 | new | 45.34 | 72.6 | 234.2 | 1.597 | 0.002 |

### Reading (means of the three interleaved pairs; positive = new worse)

| row | VM | cycles/op old -> new | instr/op old -> new | verdict |
|---|---|---|---|---|
| larson1 | C4A | 71.1 -> 71.4 (+0.4%) | 249.4 -> 249.4 (0.0, all six runs) | zero |
| valkey1 | C4A | 241.7 -> 239.6 (-0.9%) | 234.5 -> 234.4 (0.0%) | zero |
| valkey1-free | C4A | 241.9 -> 243.5 (+0.7%) | 237.3 -> 238.7 (+0.6%, +1.3/op; the three old runs span 1.5 by themselves) | zero within noise |
| larson1 | C4 | 72.8 -> 72.5 (-0.4%) | 234.1 -> 234.2 (0.0%) | zero |
| valkey1 | C4 | 150.3 -> 148.4 (-1.2%) | 228.7 -> 227.1 (-0.7%) | zero |
| valkey1-free | C4 | 153.0 -> 149.8 (-2.1%) | 225.7 -> 227.9 (+1.0%; old's third run, 222.9, is the outlier) | zero within noise |

The first run's C4A `valkey1-free` +6.3% in cycles (254.7 against 239.6)
did not repeat (239.3, 241.8, 249.5 against 243.1, 240.5, 242.0): a
placement effect of one run, as `2026-09-19-*-page-aliasing` style
differences are, not the mechanism. No row moves in instructions by more
than the spread of its own old runs, and `larson1`'s count is identical to
the tenth of an instruction, as the disassembly says it must be. The slow
paths' new zero checks (above) do not show at this resolution: they are per
refill or per compaction rebuild, not per operation.

## `quick` (the go/no-go, one round, OLD=dd5f5c8)

C4A (aarch64), load at start 2.74, 124 s:

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.5 (15.6 MiB) | 42.4 (16.0 MiB) | 45.0 (18.2 MiB) | +0.2% | +6.1% |
| larson10 | M ops/s | 368.0 (111.9 MiB) | 360.4 (111.9 MiB) | 386.9 (129.9 MiB) | +2.1% | +7.4% |
| xmalloc | M frees/s | 599.767 (53.7 MiB) | 601.979 (54.6 MiB) | 472.458 (78.7 MiB) | -0.4% | -21.5% |
| alloc-test1 | s | 2.718 (13.0 MiB) | 2.698 (13.0 MiB) | 2.550 (12.3 MiB) | -0.7% | +5.8% |
| valkey1 | M ops/s | 6.851 (584.2 MiB) | 6.854 (584.2 MiB) | 6.840 (585.4 MiB) | -0.0% | -0.2% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go`.

C4 (x86_64), load at start 2.93, 124 s:

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.8 (16.5 MiB) | 45.6 (16.8 MiB) | 44.0 (17.6 MiB) | +0.4% | -3.5% |
| larson10 | M ops/s | 286.8 (95.9 MiB) | 280.0 (96.4 MiB) | 292.1 (92.6 MiB) | +2.4% | +4.3% |
| xmalloc | M frees/s | 337.941 (51.9 MiB) | 349.956 (52.5 MiB) | 334.548 (92.0 MiB) | -3.4% | -4.4% |
| alloc-test1 | s | 3.187 (13.6 MiB) | 3.156 (13.8 MiB) | 3.006 (13.1 MiB) | -1.0% | +5.0% |
| valkey1 | M ops/s | 14.944 (585.0 MiB) | 15.008 (585.0 MiB) | 13.760 (591.7 MiB) | -0.4% | -8.3% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6668 (49.9%) | new compacts | |

`quick: go`. The churn row is identical old and new (same slab counts, same
occupancy): on 4 KiB pages the geometry is byte for byte the table's.

## Tests

- Host (`cargo test --features std`, macOS): 271 passed, 0 failed (two new
  tests in `layout`, one in `slab`).
- Mac dev container, 4 KiB pages (`make test`): 343 + 108 + 23 = 474 passed,
  0 failed; `test-cgroup-churn` all four OUTCOME lines `verdict=ok`
  (`purge 400 100`, `keep 400 100`, `purge 200 50`, `keep 200 50`); run twice,
  before and after the report line's wording fix.
- 64 KiB lab cell (`6.8.0-139-generic-64k`, `make test`): 474 passed, 0
  failed, the four churn OUTCOME lines `verdict=ok`; at the run's sources
  (59 s) and again at 2a1919b after the soak. The container test
  `api::print::tests::slabs_are_never_smaller_than_the_page` is the one that
  differs between the two hosts: on the lab it asserts the raised geometry
  and the report line, on 4 KiB hosts the table's equality with
  `BASE_CLASSES`.
- Probe line on the lab (the quantum8 library under `LD_PRELOAD`):
  `page_size=65536 guard_install=no mseal=no process_madvise=no anon_name=yes
  ... l1d=64K l2=2M`, then the report's `Page size: 65536` and `Page-sized
  slabs: classes 19..23 (512..1024 bytes) at 65536 bytes`.

## Verdict

The mechanism is priced at zero on both reference VMs: the fast paths are
the same instructions, `larson1` counts the same instructions to the tenth,
and no row moves in cycles or instructions beyond its own run-to-run spread.
`quick` is go on both. The code lands (2a1919b); the 64 KiB acceptance
(`2026-09-19-aarch64-64k-valkey-soak-2.md`) closes the `Empty`-slab term it
was aimed at (79.5 MiB to 0) and leaves the post-delete criterion failing on
a different term (`active`, the survivors' compaction), which is plan task
4f.
