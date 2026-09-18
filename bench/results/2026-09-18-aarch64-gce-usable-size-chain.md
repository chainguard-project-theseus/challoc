# challoc usable-size chain, 2026-09-18, aarch64 (GCE C4A)

Phase 1.5 task 14 (`docs/plan.md`; `docs/review-2026-09-18.md` 4.2): Valkey's
unsized `zfree(ptr)` calls `zmalloc_size(ptr)` = `je_malloc_usable_size`
before `je_sdallocx`, and after the dense bitmap (task 4f) the query stayed at
206 cycles per command in the soak's delete phase against jemalloc's 96
(`2026-09-18-aarch64-gce-dense-bitmap.md` 4.3), attributed there to "a chain
of short misses" rather than one miss: the query's slow-path form has a longer
dependent-load chain than the `free` fast path.

Mechanism attacked, named from the `perf annotate` of section 1.2 (before):
`api::libc::usable_size` ran `Regions::classify` (class, slab and slot from
the dense `superblock`/`class_base`/`CLASSES` arrays, the `free` fast path's
arithmetic), threw the slab and slot away, and called
`SlabTable::check_live(p)`, which was `locate(p)` then the bit test.
`locate` re-derived slab and slot from the class *table line*: the table's
`class` field (with a bounds check on `CLASSES[class]`, `panic_bounds_check`
was in the function), `base` and `nslabs`, then `CLASSES[class]`'s `lg_slab`,
reciprocal and size a second time, a multiply and two compares; only then the
watermark (an `ldar`, acquire, so the bitmap-word load could not issue before
it completed), the guard-slab test (`udiv`), and the bitmap word. So between
the pointer and the word there were two dependent chains where `free` has
one, plus a frame (`sub sp; stp x29, x30`) for the `abort_fmt` arguments of
the invalid-pointer message.

The change (code commit named in PROGRESS.md), the smallest that gives the
query the `free` fast path's loads: `check_live(slab, slot, p)` takes the
located slab and slot (it is now the bit test `check_located` is built on,
so `free`'s slow path and the queries share one function); the watermark is
loaded `Relaxed` under `desc_if_committed`'s argument, exactly as
`Shared::checked_slot` does; the guard test moves into the cold abort path
(a guard slab's bits are never set, so the bit test catches it and the
message is unchanged); the invalid-pointer abort is an out-of-line `#[cold]`
function so the query has no frame; `usable_size`/`malloc_usable_size` are
`#[inline(always)]` into the exports. `realloc` and `rallocx` take the same
bit test. Nothing about what is checked changes: a size query of a freed
slot still aborts (`tests/c/negative.c` `usable_size_freed`, new).

- host: challoc-bench-arm64 (c4a-standard-16, Google Axion / Neoverse V2),
  us-east4-a, Ubuntu 24.04, perf on the host; container Wolfi. VM started
  for this iteration, load 0.1..0.3 at the start of every run, `docker ps`
  empty apart from the runs.
- rows: `bench/lib.sh` `valkey1` (sized frees, no query), `valkey1-free`
  (plain `free`), and the new `valkey1-usable` (`valkey_model -u`: a
  `malloc_usable_size` query before every `sdallocx`, which is what
  `zfree` does; added in this iteration because neither existing model row
  calls the query, so neither could see this change). One op is one SET in
  the write phase and one DEL-or-SET in the delete phase; a SET is two
  frees, so two queries.
- old = d6e0ca4 (HEAD), new = the working tree; jemalloc = Wolfi 5.3.1.
  `quick-perf` numbers are one run each (`sudo perf stat` on the container's
  process; attribution, not a gate).

## 1. Before the change

### 1.1 `quick-perf` (challoc at d6e0ca4 as `new`, jemalloc)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1 | challoc d6e0ca4 | 13.60 | 222.8 | 255.2 | 3.063 | 2.880 |
| valkey1 | jemalloc | 13.89 | 217.8 | 169.4 | 2.974 | 2.739 |
| valkey1-free | challoc d6e0ca4 | 13.38 | 226.6 | 237.2 | 4.054 | 3.883 |
| valkey1-free | jemalloc | 13.93 | 217.2 | 172.0 | 3.428 | 3.354 |
| valkey1-usable | challoc d6e0ca4 | 11.00 | 275.1 | 385.3 | 4.076 | 3.941 |
| valkey1-usable | jemalloc | 11.18 | 270.5 | 331.8 | 4.353 | 4.690 |

The query's price is the `valkey1-usable` minus `valkey1` difference: challoc
+52.3 cycles and +130 instructions per op (26 cycles, 65 instructions per
query); jemalloc +52.7 cycles and +162 instructions per op. Per query the
two are level in cycles here (the model's queried slot is the one about to
be freed, so the word the query misses on is the one `sdallocx`... does not
load any more: the miss moved from the free to the query, task 4e's
finding); challoc executes fewer instructions per query than jemalloc but
they sit on a longer dependent chain.

### 1.2 `perf annotate` of the query (`perf record -F 4000` on `valkey1-usable`, challoc d6e0ca4, 5697 samples in the symbol)

`perf report`: `usable_size` 13.95% of the process's cycles (`je_malloc`
6.48%, `je_sdallocx` 4.19%, the sort of the flush 1.75%; `set_overwrite` +
`main`, the model's own key-table walk, 60.9%). `je_malloc_usable_size` is
a separate 0.30% symbol: null test, `ldar` of the state word, `b usable_size`.

Inside `usable_size` (percent of the symbol's samples on the instruction
that consumes each load; addresses of the d6e0ca4 build):

| step | instructions | share |
|---|---|---:|
| frame | `sub sp, sp, #0x30; stp x29, x30` | 1.2% |
| `slab_slot`: superblock, `lg_stride`, `class_base[c]`, `CLASSES[c]` (`lg_slab`, reciprocal, size), slot check | `ldr`/`lsr`/`cmp`/`ldp`/`mul`/`umull` | 3.0 + 2.7 + 4.0 + 2.7% |
| `tables[class]` (`madd` by 0x180), table `class` field, bounds check | `ldrh w1, [x9, #0x29a]; cmp x1, #0x33; b.hi panic_bounds_check` | 0.8% |
| `locate`: table `base`, `nslabs`, `CLASSES[class]` again, slot check again | `ldp x13, x10, [x9, #0x38]; ldrb; lsl; sub; cmp; ldp w15, w14; mul; lsr; cmp; ldr w11; umull; cmp` | 0.4 + 0.7 + 0.8 + 1.4 + 5.2 + 3.4% |
| watermark, acquire | `ldar x10, [x11]; cmp; b.hs` | 0.6% |
| guard-slab test | `ldr w10, [x9, #0x20]; ldrb; lsr; add; udiv; msub; cbz` | 1.5% |
| `slab_bits`, `bits_base`, word address | `ldrh w10, [x9, #0x18]` (7.2%, first use after the `ldar`), `ldr x12, [x9]`, `madd`, `lsr`, `and` | 8.5% |
| **bitmap word** | `ldr x11, [x11, x12]; lsr x10, x11, x10` | **45.8%** |
| class size, return | `ldr w0, [x8]; ldp; add sp; ret` | 5.8% |

The word load is the miss (45.8%), as in the soak's write record (40%); in
front of it sit the `locate` recomputation (about 12% of the symbol's
samples on its own consumers and 26 instructions), the acquire that
serialises the word load behind the table line, and a `udiv`. That is the
"chain of short misses" of the dense-bitmap file: two passes over the
geometry, one from the dense arrays and one from the table line, before the
word address exists.

## 2. After the change

### 2.1 `quick-perf`, same run: old = d6e0ca4, new = the working tree, jemalloc

An intermediate build with only the `locate` removal (the located slab and
slot passed to the bit test, `ldar` and guard test still in place) was
measured on the way: `valkey1-usable` 268.2 -> 263.6 cycles and 386.2 ->
357.1 instructions per op against old in its run (-14.5 instructions per
query). The final build adds the relaxed watermark, the guard test in the
cold path and the frameless invalid abort:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1 | old d6e0ca4 | 13.34 | 227.4 | 257.7 | 3.081 | 2.876 |
| valkey1 | new | 13.52 | 224.2 | 257.4 | 3.077 | 2.897 |
| valkey1 | jemalloc | 14.19 | 213.3 | 169.4 | 2.977 | 2.686 |
| valkey1-free | old d6e0ca4 | 13.41 | 226.1 | 235.3 | 4.039 | 3.897 |
| valkey1-free | new | 13.69 | 221.6 | 234.2 | 4.040 | 3.916 |
| valkey1-free | jemalloc | 13.87 | 218.3 | 171.9 | 3.424 | 3.357 |
| valkey1-usable | old d6e0ca4 | 11.43 | 265.3 | 383.4 | 4.067 | 3.964 |
| valkey1-usable | new | 11.89 | **255.0** | **343.3** | 4.032 | 3.929 |
| valkey1-usable | jemalloc | 11.10 | 272.5 | 331.7 | 4.351 | 4.653 |

Delta on the row that calls the query, old -> new, same run: -10.3 cycles
and -40.1 instructions per op, i.e. -5.2 cycles and -20 instructions per
query (-3.9% cycles, -10.5% instructions on the row); `valkey1` and
`valkey1-free`, which never call it, move -1.4% / -2.0% cycles with
identical instruction counts (the one-run spread of these rows is 2..3%,
see the three `valkey1 old`/`new` pairs of this iteration: 222.8, 222.6 /
228.1, 227.4 / 224.2). The query's own price, row minus `valkey1` in the
same run: challoc 37.9 -> 30.8 cycles and 125.7 -> 85.9 instructions per
op (19.0 -> 15.4 cycles, 63 -> 43 instructions per query); jemalloc 59.2
cycles and 162 instructions per op (29.6 cycles, 81 instructions per
query). The plan's acceptance for the model row ("the query at or under
jemalloc's cycles") holds here; the soak's delete phase is section 3.

### 2.2 `perf annotate` after (`perf record -F 4000` on `valkey1-usable`, new, 4652 samples in the symbol)

`je_malloc_usable_size` is now one symbol, 11.39% of the process's cycles
(13.95% + 0.30% before; `je_malloc` 6.60%, `je_sdallocx` 4.29%, unchanged).
Its hot path, in order:

| step | instructions | share |
|---|---|---:|
| frame, null test, state word (`ldar`, the readiness check) | `stp x29, x30; cbz; ldar; cmp` | 3.0% |
| `slab_slot`: superblock, `lg_stride`, `class_base[c]`, `CLASSES[c]` (`lg_slab`, reciprocal, size), slot check | `ldr`/`lsr`/`ldp`/`mul`/`umull`/`cmp` | 4.4 + 3.7 + 4.2 + 2.5% |
| `tables[class]` (`madd`), watermark (`ldr`, relaxed), compare | `madd; ldr x10, [x8, #0x290]; cmp; b.hs` | 1.6% |
| `slab_bits`, `bits_base`, word address | `ldrh w9, [x8, #0x18]; ldr x11, [x8]; umaddl; lsr; and` | 1.4% |
| **bitmap word** | `ldr x10, [x10, x11]; lsr; tbz` | **61.4%** |
| return (the class size is the `ldr w0, [x12]` the slot check already loaded) | `ldp x29, x30; ret` | 5.7% |

Gone from the hot path: the second pass over the geometry from the table
line (`class` field with its bounds check, `base`, `nslabs`, `CLASSES`
again, the second multiply and two compares: 26 instructions), the
acquire on the watermark (the word load no longer waits for it: the
`ldrh slab_bits` that carried 7.2% behind the `ldar` is at 0.5%), the
guard test's `udiv` (7 instructions, now in `SlabTable::not_live`, the
cold abort), the `sub sp`/`add sp` frame adjustment and the
`b usable_size` tail call from the export. The loads that depend on the
pointer are the `free` fast path's: dense arrays, table line, bitmap word.
What remains is the word miss itself (61% of the symbol), the same miss
jemalloc's rtree leaf takes in `je_malloc_usable_size`.

## 3. `quick` (go/no-go, one round, working tree against d6e0ca4)

aarch64 (load 0.39 at the start, 119 s; old 147978f1a53ed72f, new 9134bbad90c53460):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.6 (15.9 MiB) | 42.7 (15.9 MiB) | 45.1 (17.6 MiB) | -0.2% | +5.6% |
| larson10 | M ops/s | 366.0 (117.7 MiB) | 366.9 (114.5 MiB) | 388.8 (132.1 MiB) | -0.2% | +6.0% |
| xmalloc | M frees/s | 589.969 (54.5 MiB) | 611.986 (52.8 MiB) | 478.250 (80.0 MiB) | -3.6% | -21.9% |
| alloc-test1 | s | 2.606 (13.0 MiB) | 2.614 (13.0 MiB) | 2.473 (12.3 MiB) | +0.3% | +5.7% |
| valkey1 | M ops/s | 7.311 (584.2 MiB) | 7.373 (584.2 MiB) | 7.338 (585.4 MiB) | -0.8% | -0.5% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go`. The plan's `valkey1` criterion ("on aarch64 within 5% of
jemalloc"): -0.5%, ahead, in this round.

x86_64 (load 0.08 at the start, 124 s; old 1530d9982487856e, new 0da14315e002072e):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 57.8 (16.3 MiB) | 58.2 (16.1 MiB) | 55.9 (19.3 MiB) | -0.7% | -4.0% |
| larson10 | M ops/s | 335.4 (98.4 MiB) | 320.6 (95.6 MiB) | 349.2 (98.7 MiB) | +4.6% | +8.9% |
| xmalloc | M frees/s | 408.265 (53.1 MiB) | 451.564 (57.2 MiB) | 353.559 (82.1 MiB) | -9.6% | -21.7% |
| alloc-test1 | s | 2.646 (13.6 MiB) | 2.668 (13.5 MiB) | 2.500 (13.0 MiB) | +0.8% | +6.7% |
| valkey1 | M ops/s | 13.227 (585.0 MiB) | 13.329 (584.9 MiB) | 13.639 (591.7 MiB) | -0.8% | +2.3% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6643 (50.1%) | new compacts | |

`quick: go`. larson10 +4.6% in one round is inside the gate but close, so
two more rounds of `QUICK_BENCHES="larson1 larson10"` followed: larson10
old 313.9 / 339.6, new 322.8 / 325.2 (-2.8% / +4.4%); larson1 -0.2% /
+2.3%. New's three larson10 results (320.6, 322.8, 325.2) lie inside old's
(313.9..339.6): the row's known bimodality on this VM, not a shift; the
change touches no line of `malloc` or `free`'s fast path (`check_live` is
`free`'s slow-path bit test and the queries').

## 4. The 20-minute Valkey soak with the perf sidecar (acceptance run, code of 75bdbc5)

`make valkey STAGE=build && make valkey-soak DURATION=1200 SETTLE=60` (the
full protocol: `ALLOCS="challoc jemalloc"`, 600 s write then 600 s delete
per allocator, Valkey 9.1.2 relinked against the tree's `libchalloc.so`,
the same tag with bundled jemalloc 5.3.0), 18:24..19:07 UTC, with
`bench/probes/valkey-perf-sidecar.sh ~/logs/usable-soak 700 1205` started
together with the soak on the VM host, so both servers were attached from
their first second (challoc t0 1789755867, jemalloc t0 1789757168). The
summary is `2026-09-18-aarch64-gce-usable-size-chain-soak-valkey-soak.md`
(with the two `-challoc.csv` / `-jemalloc.csv` sample files) next to this
file; it records `commit: d6e0ca4` because the VM's HEAD was that when the
synced working tree ran (the line now says so).

### 4.1 Soak summary (challoc vs jemalloc, positive = challoc worse)

| measure | challoc | jemalloc | challoc vs jemalloc | task 4f run (73b6df1's soak) |
|---|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 920682 | 887593 | **-3.6%** | -3.9% |
| write: valkey-benchmark p99 ms | 1.047 | 1.095 | -4.4% | -4.9% |
| write: memtier req/s | 936306 | 936172 | -0.0% | -1.8% |
| write: memtier p99 ms | 0.991 | 0.975 | +1.6% | -3.9% |
| delete: valkey-benchmark req/s | 1410044 | 1452457 | **+3.0%** | +3.2..4.6% |
| delete: valkey-benchmark p99 ms | 0.711 | 0.687 | +3.5% | |
| delete: memtier req/s | 1225905 | 1262722 | +3.0% | |
| delete: memtier p99 ms | 0.783 | 0.751 | +4.3% | |
| peak RSS MiB (VmHWM) | 579.0 | 577.0 | +0.3% | +1.0% |
| RSS MiB after settle | 129.5 | 132.3 | -2.1% | +2.0% |

Write phase: at parity or ahead on both generators (the Phase 1 criterion
holds). Delete phase: 3.0% behind on both generators' throughput and
3.5..4.3% on p99, the same band as tasks 4e (+3.8% / +3.6%) and 4f
(+3.2..4.6%). RSS within the criteria. Seen run to run, the delete gap
did not move with this change; the tie-breaker below says why.

### 4.2 Cycles per command (`perf stat -I 1000` on the server, divided by `total_commands_processed` between snapshots; `percmd.py`)

| window | alloc | commands/s | cyc/cmd | ins/cmd | CPI | L1D miss/cmd | dTLB refill/cmd | br-miss/cmd | main thread | SET us/call |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| write, unrecorded, t=30..100 | challoc | 957k | **3046** | 9541 | 0.32 | 43.2 | 54.2 | 7.1 | 100.2% | 0.188 |
| write, unrecorded, t=30..100 | jemalloc | 928k | 3141 | 9843 | 0.32 | 39.5 | 39.0 | 7.5 | 100.1% | 0.182 |
| write, under `perf record`, t=100..162 | challoc | 751k | 3270 | 9763 | 0.33 | 47.8 | 58.3 | 6.6 | 100.2% | 0.223 |
| write, under `perf record`, t=100..161 | jemalloc | 753k | 3230 | 9726 | 0.33 | 40.8 | 40.7 | 6.7 | 100.1% | 0.220 |
| write, unrecorded, t=162..700 | challoc | 999k | **2922** | 9098 | 0.32 | 40.6 | 51.1 | 6.3 | 100.2% | 0.187 |
| write, unrecorded, t=161..700 | jemalloc | 984k | 2967 | 9293 | 0.32 | 36.5 | 39.4 | 6.5 | 100.1% | 0.181 |
| delete, under `perf record`, t=700..761 | challoc | 1063k | **2292** | 7165 | 0.32 | 24.6 | 25.9 | 5.2 | 100.3% | 0.251 |
| delete, under `perf record`, t=700..762 | jemalloc | 1078k | 2280 | 7447 | 0.31 | 24.1 | 24.5 | 5.3 | 100.1% | 0.232 |

Against task 4f's soak (challoc 3313 / jemalloc 3407 recorded write, 2335
/ 2270 delete) and 4e's (3430 / 3343, 2330 / 2237): the write phase is
3.0% fewer cycles per command than jemalloc unrecorded (3046 vs 3141) and
1.5% fewer over the long unrecorded window (2922 vs 2967), and the delete
phase is at parity, +0.5% (2292 vs 2280; it was +2.9% at 4f and +4.2% at
4e). Both servers are main-thread bound (100%), so cycles per command is
the throughput; in the delete window challoc processed 1.4% fewer
commands per second with 0.5% more cycles per command, the difference
being the servers' cycles per second (2.436 vs 2.458 G), i.e. clock, not
code. Instructions per command are below jemalloc's in every window.

### 4.3 Where the query's cycles land (`perf report`/`annotate` on the records; challoc's while the pid was alive and again over `--symfs` on the target volume)

| phase | symbol | challoc share | jemalloc share | challoc cyc/cmd | jemalloc cyc/cmd | 4f (challoc / jemalloc) |
|---|---|---:|---:|---:|---:|---:|
| write | usable-size query (`je_malloc_usable_size`, one symbol now) | 7.47% | 8.06% | **244** | 260 | 327 / 327 |
| write | free path `je_sdallocx` | 2.35% | 2.06% | 77 | 67 | 64 / 66 |
| write | `je_malloc` | 1.44% | 1.44% | 47 | 47 | 48 / 50 |
| write | `libchalloc.so` (whole DSO) | 11.38% | | 372 | | 13.58% |
| delete | usable-size query | 6.63% | 4.19% | **152** | 96 | 206 / 96 |
| delete | free path | 2.17% | 2.18% | 50 | 50 | 35 / 49 |
| delete | `je_malloc` | 1.41% | 1.49% | 32 | 34 | 28 / |
| delete | `libchalloc.so` | 11.17% | | 256 | | 12.77% |

The query is under jemalloc's in the write phase (244 vs 260 cycles per
SET; 327 vs 327 at 4f) and 152 vs 96 in the delete phase (206 at 4f): the
change took 83 cycles per SET and 54 per delete-phase command out of it,
and the remaining +56 in the delete phase is what the plan's "at or under
jemalloc's cycles per command in the delete phase" still misses. Where it
sits, from the annotate of the delete record (share of the symbol's
samples on each load's consumer): the bitmap word 16.7% + 7.8%
(`tbz`/`lsr`), `CLASSES[c]` 11.2% + 8.2% (`ldr w0, [x12]` and the
instruction after the slot check), `class_base[c]`/region 9.3% + 6.6% +
3.4%, the readiness `ldar` and the null test 1.1% each, the return 8.5%.
In the write record the same function has 34.2% + 10.5% on the word and
the rest smaller. So in the delete phase the query is no longer one miss
plus a second geometry pass: it is one short chain (dense arrays, table
line, word) whose every line comes from beyond L1, because a DEL frees
four objects of four classes (four bitmaps, four table lines) and the
server runs 38% of its cycles in the kernel between allocator calls, where
jemalloc's query resolves through its `rtree_ctx` cache in the TSD (a
hot TLS line) and the extent's `edata`, lines the frees of the same key
share. The lever left on this path is not another removal from the chain
(there is nothing redundant in it now) but where the per-slot metadata
lives; the bit test itself stays (task 4d's decision).

## 5. Acceptance against the plan's task 14 text

- "the query at or under jemalloc's cycles per command in the delete
  phase": not met, 152 vs 96 (from 206), residual attributed above.
- "the 20-minute soak on arm64 with the sidecar meets the Phase 1
  criterion (throughput and p99 at least equal in both phases) or the
  residual is attributed": write phase met; delete phase +3.0% / +3.5%
  with cycles per command at parity (+0.5%), residual attributed in 4.2
  and 4.3.
- "`valkey1` on aarch64 within 5% of jemalloc": -0.5% (ahead) in the
  `quick` round, +5.1% behind in the `quick-perf` run (13.52 vs 14.19 M
  ops/s, one run each), and the scorecard line (3 rounds) in PROGRESS.md.
- "all tests green": `make test` 271 + 90 (the new `usable_size_freed`
  mode included), host 212.
