# challoc Phase 1 task 3 follow-up (2): larson gap is per-thread, 2026-09-16, aarch64

Third file on the larson regression of the arena tree (after
`2026-09-16-aarch64-arenas.md` and `2026-09-16-aarch64-arenas-lines.md`).
Same host (Apple M4, Docker Desktop, kernel 6.10.14-linuxkit, 10 CPUs),
same binaries: `old` = 8449c3a, `fix2` = 9cb48ad (HEAD before this file).
The host carried other sessions' load again (1-minute load average 3 to
14 during the runs), so the 10-thread numbers below are far under the
quiet-host 250/220 of the previous file and only the interleaved
ordering means anything; the 1-thread numbers were stable to about 3%.

## 1. Hypothesis (a), slot placement: rejected

`plan`, `take`, `claim`, `refill`, `flush`, `gc`, `tick`, `create` and
`destroy` are textually identical between 8449c3a and HEAD apart from
the arena parameter; only `commit_next` moved into `SlabTable` with the
same body. Measured, not assumed: `/proc/<pid>/maps` 5 s into larson,
bucketed by 1 TiB stride (one class per stride):

| | old | cur |
|---|---|---|
| rw-p VMAs total | 2529 | 2335 |
| of which 8 MiB + 64 KiB guard pairs (pthread stacks) | 2323 | 2129 |
| committed slab bytes per class, classes 0..44 | 128K 256K 320K 448K 576K 640K 768K 960K ... 6.5M 6.3M 2M | same within one slab per class |
| RSS | 77.2 MiB | 72.8 MiB |

The 9% fewer VMAs and 4% less RSS of the arena tree are larson's own
thread stacks: larson relaunches a thread after every round and never
joins the old one, so exited threads' 8 MiB stacks stay mapped and their
count is proportional to throughput. Each class region is one merged
`rw-p` VMA of the same size under both libraries. Slot placement is the
same; nothing to fix there.

## 2. `malloc` is identical, the gap is single-threaded

`objdump` of the exported `malloc`: the 46 instructions are the same in
old and fix2 apart from addresses. `free` differs by the `tables[c]`
line (8 loads from one per-class line instead of `class_base[c]`,
`lg_region`, `CLASSES[c]` and 3 loads from the `ClassState` line), the
`multi_arena` byte load plus taken `cbz`, and a 4-byte-aligned `ldp` of
`nslots`/`recip`.

larson with 1 thread (`larson 4 8 1000 5000 100 4141 1`, M ops/s):

| variant | rounds | sorted |
|---|---|---|
| old | 57.6 55.0 / 54.7 55.1 53.7 / 52.0 53.7 52.5 54.4 / 58.1 54.2 54.8 54.0 52.7 55.0 | median about 54.5 |
| fix2 (HEAD) | 55.2 50.7 / 48.9 50.1 51.4 / 49.5 49.8 49.2 51.7 / 52.8 54.0 50.5 51.1 50.8 50.5 | median about 50.7 |

Four separate interleaved runs, old ahead in each: the arena tree is
5 to 7% slower with one thread, where no line is shared with another
core. The 10-thread gap of 12% therefore has a single-thread component
of about 1.4 ns per malloc+free pair and a multi-thread remainder.

## 3. Fast-path variants (1 thread, interleaved)

| variant | change | rounds | verdict |
|---|---|---|---|
| va | `Class` `repr(C)` with `lg_slab` first so the `ldp` is 8-byte aligned | 51.3 48.2 50.5 | neutral |
| vc | arena check removed entirely (bound, not shippable) | 52.2 52.9 50.2 | neutral to +2%, inside noise |
| vd | branchless `multi_arena & (desc.arena != heap.arena)` | 51.9 53.3 49.7 / 49.1 49.2 50.1 51.6 | neutral (second run) |
| vg | `region_len` moved next to `base`, every fast-path field in the first 64 bytes | 53.6 54.4 52.1 51.5 51.1 52.6 vs fix2 52.8 54.0 50.5 51.1 50.8 50.5 in the same run | +2%, about half the 1-thread gap |

`core::hint::unlikely` is unstable on rustc 1.98 (`likely_unlikely`),
so the taken-branch variant could not be built without a call.

10-thread rounds from the same sessions, for the record (load average
4 to 13): old 152-214, fix2 121-200, vd 139-193, vc 109-176; not usable.

## 4. Landed

vg: `SlabTable` field order `desc_stride, base, region_len, desc_base,
committed_slabs, geom` in the first 64 bytes, `nslabs` after. Zero cost,
consistent with a 64-byte L1 line (the old `ClassState` line kept its
three fast-path fields in its first 64 bytes; fix2's `tables[c]` line
had `region_len` at offset 64), and the only variant that moved the
1-thread number. Tests: `make test` on aarch64 (see PROGRESS.md).

## 5. Open

About 3% single-threaded and the multi-thread remainder. Not: slab
placement, `malloc`, the arena flag or branch, the `ldp` alignment, the
number of 128-byte lines. The remaining structural difference is that
`free` now depends on one per-class `.bss` line for both geometry and
descriptor base, where old read geometry from lines shared by all
classes (`class_base`, `CLASSES`) and only the descriptor fields from a
per-class line. A variant that keeps `CLASSES[c]` in `.rodata` for the
slot arithmetic and reads only `base`/`region_len`/`desc_base`/
`committed_slabs` from the table line is the next thing to try; after
that, `perf stat` on a bare-metal box. The sampler is not usable for
this any more: under the current VM it delivers 500 to 57000 samples
for the same 10 s run, so per-instruction shares of a 500-sample run
are noise.
