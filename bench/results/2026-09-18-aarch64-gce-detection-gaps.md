# challoc detection gaps (flush boundary, sized path, double_free_scan), 2026-09-18, aarch64 (GCE C4A)

Phase 1.5 task 9 (`docs/plan.md`; review 2026-09-18 section 1.2 items 9 and
10, section 5.1): the detection gaps. (a) `Heap::flush`: after the sort, one
compare at the split point aborts on a cached double free whose two copies
straddle the flush boundary (one would have been released, bit cleared, the
other kept and handed out over a slot a refill can claim again). (b) The
sized fast path (`Shared::sized_slot`) rejects a slot start in a guard slab
by arithmetic on the table line (`SlabTable::is_guard_slab`, now a shift and
one 32-bit modulo; `lg_slabs_per_page` added to the table's first line); the
exact slow path names it (`invalid free ...: guard slab`). Whether a
handed-out slab is below its arena's commit cursor is not one load on the
fast path (the cursor is under the class lock and the sized path does not
read the arena), so a slot start in an uncommitted slab of a handed-out
group is caught at the flush, where `release_owned` reads the descriptor
anyway (`the slab has never been allocated from`, a `Fresh` slab; a guard
slab reaching a release is named too). (c) `double_free_scan`: see section
4. Working tree of this iteration on top of 48c845f (the code commits'
hashes are in PROGRESS.md; `quick` lists the library hashes).

- hosts: challoc-bench-arm64 (c4a-standard-16, Google Axion / Neoverse V2)
  and challoc-bench-amd64 (c4-standard-16, Xeon Platinum 8581C), us-east4-a,
  kernel 7.0.0-1011-gcp (guard regions available, so the `sized_guard`
  negative mode runs for real there; the Mac container's 6.10 kernel has
  none and the driver skips it), started and stopped by this iteration
  (CHALLOC_GCE_WARM unset).

## 1. `quick`, first variant: a neighbour pass over the whole kept half (abandoned)

The first tree compared every neighbouring pair of the sorted kept half
(`rest_len` compares per flush, so a pair the split keeps together aborted
at once instead of when a later flush releases it). On the flush-heavy row
that is about one compare per free amortised, and xmalloc-test (10
threads, 64-byte blocks freed cross-thread through the small `others`
caches) paid for it: +4.9% on aarch64 (median of 3), +7.3% on x86_64
(NO-GO). Every other row is inside noise. The pass was dropped for the
plan's single compare; a pair kept together stays cached and consistent
(both copies in the cache, the bit set) until a flush releases it, where
`release_owned` catches the duplicate as before.

aarch64, working tree (kept-half pass) against 48c845f:

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.8 (16.1 MiB) | 42.7 (15.9 MiB) | 45.1 (18.0 MiB) | +0.2% | +5.6% |
| larson10 | M ops/s | 365.0 (112.2 MiB) | 363.1 (112.4 MiB) | 384.8 (130.5 MiB) | +0.5% | +6.0% |
| xmalloc | M frees/s | 560.972 (53.1 MiB) | 534.529 (52.3 MiB) (median of 3) | 472.739 (79.6 MiB) | +4.9% | -11.6% |
| alloc-test1 | s | 2.675 (12.9 MiB) | 2.660 (12.9 MiB) | 2.530 (12.3 MiB) | -0.6% | +5.1% |
| valkey1 | M ops/s | 6.833 (584.1 MiB) | 6.842 (584.2 MiB) | 7.535 (585.4 MiB) | -0.1% | +10.1% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24203418 ops/s |

Positive percentages mean new did worse (slower, lower throughput). One round each, aarch64, nproc 16, load at start 6.93, 142 s total. old 2edfcdb856ac3ff3, new 7c6fadeb0223deef.

## 2. `quick`, landed variant: one compare at the split point

aarch64, working tree ((a) boundary compare, (b) guard test on the sized path
and the `Fresh`/guard checks at the release) against 48c845f (go):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.9 (15.9 MiB) | 43.0 (15.8 MiB) | 45.0 (18.7 MiB) | -0.2% | +4.7% |
| larson10 | M ops/s | 361.7 (111.7 MiB) | 368.7 (114.3 MiB) | 384.6 (131.9 MiB) | -1.9% | +4.3% |
| xmalloc | M frees/s | 562.134 (52.4 MiB) | 549.714 (52.9 MiB) | 471.508 (79.3 MiB) | +2.3% | -14.2% |
| alloc-test1 | s | 2.656 (12.9 MiB) | 2.652 (12.9 MiB) | 2.537 (12.3 MiB) | -0.2% | +4.5% |
| valkey1 | M ops/s | 6.978 (584.2 MiB) | 6.849 (584.1 MiB) | 7.657 (585.6 MiB) | +1.9% | +11.8% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24199776 ops/s |

Positive percentages mean new did worse (slower, lower throughput). One round each, aarch64, nproc 16, load at start 2.88, 119 s total. old 2edfcdb856ac3ff3, new fa677728c634ccdd.

The gate is go on both VMs (this file's twin has the other). xmalloc is
back inside noise (+2.3%); valkey1, the only row that frees through
`sdallocx`, is +1.9% in this single round, which is what the guard test
costs (section 3).

## 3. Attribution of (b) on the sized path (`quick-perf`, arm64)

`bench/gce/challoc-bench quick-perf arm64 OLD=48c845f` (perf on the VM host,
`--inherit -p` on the container's init pid, one run per row and variant;
`ROWS="larson1 valkey1"`). valkey1 frees through `sdallocx`, larson1 through
`free`, so larson1 is the control for the sized path.

Sample 1:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 42.37 | 70.8 | 245.9 | 2.093 | 4.112 |
| larson1 | new | 42.47 | 70.6 | 246.0 | 2.094 | 4.032 |
| larson1 | jemalloc | 44.60 | 66.8 | 224.8 | 2.064 | 1.727 |
| valkey1 | old | 12.18 | 248.8 | 239.9 | 3.098 | 2.856 |
| valkey1 | new | 12.00 | 252.9 | 252.5 | 3.102 | 2.929 |
| valkey1 | jemalloc | 13.08 | 231.2 | 166.5 | 3.038 | 2.757 |

Sample 2 (`ROWS=valkey1`; a first rerun was lost to an ssh disconnect
mid-run):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1 | old | 12.58 | 241.0 | 240.1 | 3.106 | 2.878 |
| valkey1 | new | 12.70 | 239.0 | 250.9 | 3.098 | 2.916 |
| valkey1 | jemalloc | 13.87 | 218.2 | 166.3 | 3.022 | 2.690 |

Mechanism, named: the sized fast path (`Shared::sized_slot`, one per
`sdallocx`) gained `SlabTable::is_guard_slab`: loads of `lg_slabs_per_page`
and `guard_interval` from the table line the watermark load already brings
in, a shift, an add, `udiv` + `msub` (the interval is a runtime value), and
a branch, about 11..13 instructions per operation on valkey1 (239.9 ->
252.5 and 240.1 -> 250.9 instructions per op; larson1, which frees through
`free`, is unchanged at 245.9 -> 246.0). Cycles per op moved +1.6% and
-0.8% in the two samples (248.8 -> 252.9, 241.0 -> 239.0), inside the
run-to-run spread, because the divide feeds a predicted branch and sits off
the dependency chain to the cache store; L1D and dTLB misses per op are
unchanged (no new line: `lg_slabs_per_page` took a pad byte on the table's
first line). The `Fresh` and guard checks in `release_owned` run once per
slab switch of a flush batch and read the descriptor line `dec_live` reads
anyway; the `check_bit` guard test is on the abort path only.

Not done on the fast path: rejecting a slot start in a handed-out but
uncommitted slab. The arena's commit cursor (`Inner::group_next`) is under
the class lock and the sized path does not read the arena, so it is not
"one load"; publishing it would need a per-group word next to the owner
array (a second dense load per sized free), which is the bitmap-word
question of task 4e in another form and a human's call. Such a slot is
caught at the flush by name (`the slab has never been allocated from`), as
the plan allows ("or the flush does"), and its bits are clear so the
release's bit test was already a backstop.

## 4. (c) `double_free_scan`: the rows and the default decision

Code commit d039923 (default off): every cache push compares the slot
against the top four entries when the tunable is set; off, the flag is one
plain load from `Shared`'s first line (the line `superblock`/`lg_stride`
come from) and a predicted branch. `quick` against 9c3b154 with it off: go
on both VMs (aarch64 every row within 1.6%; x86_64 larson1 +3.2%, larson10
+3.0%, valkey1 +1.5%, single rounds).

`bench/gce/challoc-bench ab arm64 VARIANTS="base=9c3b154 off=d039923
on=d039923:double_free_scan:true jemalloc" BENCHES="valkey1 larson1 redis"
ROUNDS=5` (== A/B: 2026-09-18T15:24:41Z aarch64 nproc 16 load 0.80 1.22 0.92, HEAD d039923):

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| valkey1 | base | 6.493 6.590 6.604 6.853 6.911 | 598472 |
| valkey1 | off | 6.550 6.575 6.603 6.715 6.725 | 598480 |
| valkey1 | on | 6.649 6.690 6.752 6.791 6.797 | 598540 |
| valkey1 | jemalloc | 7.215 7.461 7.549 7.612 7.683 | 599808 |
| larson1 | base | 42.7 42.8 42.8 42.8 42.9 | 16184 |
| larson1 | off | 42.3 42.4 42.5 42.5 42.5 | 16252 |
| larson1 | on | 41.3 41.4 41.5 41.6 41.7 | 15868 |
| larson1 | jemalloc | 44.9 44.9 45.0 45.0 45.1 | 18784 |
| redis | base | 469.5 472.4 472.6 473.0 474.6 | 29752 |
| redis | off | 468.4 473.7 473.7 474.6 474.6 | 29704 |
| redis | on | 462.1 471.0 471.0 471.7 475.5 | 29704 |
| redis | jemalloc | 406.0 406.3 409.2 410.2 410.7 | 31804 |

Medians:

| program | unit | base | base RSS MiB | off | off RSS MiB | on | on RSS MiB | jemalloc | jemalloc RSS MiB | off vs base | on vs base | jemalloc vs base |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| valkey1 | M ops/s | 6.604 | 584.4 | 6.603 | 584.5 | 6.752 | 584.5 | 7.549 | 585.8 | +0.0% | -2.2% | -12.5% |
| larson1 | M ops/s | 42.8 | 15.8 | 42.5 | 15.9 | 41.5 | 15.5 | 45.0 | 18.3 | +0.7% | +3.1% | -4.9% |
| redis | k req/s | 472.6 | 29.1 | 473.7 | 29.0 | 471.0 | 29.0 | 409.2 | 31.1 | -0.2% | +0.3% | +15.5% |

Positive percentages mean the variant did worse than base (slower or lower throughput).

Decision (plan Phase 1.5 task 9c: on by default only if under 1% on every
row on both VMs): **off by default**. On, larson1 is +3.1% on aarch64 and
+3.7% on x86_64 and valkey1 +14.0% on x86_64 (aarch64 valkey1 -2.2% and
redis +0.3% / -1.4% are inside the rows' spread); four compares and their
loads per free are about 4% of larson's 246 instructions per op. The
tunable is implemented and documented as opt-in (README, design 6.3 and
12). The off state is at parity on aarch64 (+0.0% / +0.7% / -0.2%) and on
x86_64 larson1 (+0.4%) and redis (-1.8%); x86_64 valkey1 shows +4.6% for
the off state (medians 14.828 vs 14.174, the sorted lists overlap only
through base's low outlier), which section 5 takes to the counters. The
x86_64 file has the other VM's rows.

## 5. (c) attribution (`quick-perf`, base / off / on)

`bench/gce/challoc-bench quick-perf arm64 OLD=9c3b154 NEW=d039923
VARIANTS="old new new:double_free_scan:true jemalloc"` (`old` = base with
(a) and (b), `new` = the scan code with the default off, the third the same
library with `MALLOC_CONF=double_free_scan:true`), one run per cell:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 42.50 | 70.6 | 246.0 | 2.095 | 4.103 |
| larson1 | new | 42.24 | 71.0 | 250.0 | 2.102 | 4.011 |
| larson1 | new:double_free_scan:true | 41.36 | 72.6 | 261.1 | 2.113 | 4.055 |
| larson1 | jemalloc | 44.79 | 66.5 | 224.8 | 2.072 | 1.696 |
| valkey1 | old | 12.35 | 245.5 | 250.4 | 3.097 | 2.884 |
| valkey1 | new | 11.86 | 255.7 | 257.7 | 3.108 | 2.899 |
| valkey1 | new:double_free_scan:true | 12.19 | 248.8 | 267.9 | 3.102 | 2.923 |
| valkey1 | jemalloc | 13.39 | 226.0 | 166.5 | 3.025 | 2.729 |

Read with the other VM's file: the off state is +4.0 instructions per
larson1 op on both architectures (246.0 -> 250.0 aarch64, 231.1 -> 235.1
x86_64: the flag load and its branch on both the `free` and the push
paths), +0.6% / +2.4% cycles per op in these single runs; the scan is
+15.1 / +19.0 instructions and +2.8% / +9.0% cycles per larson1 op and
+17.5 / +17.9 instructions per valkey1 op (four loads, four compares and
their ORs per free). valkey1's cycles per op do not resolve the off state
at one run per cell: +4.2% on aarch64 and -10% on x86_64 (where the `ab`
medians said +4.6%; the row's throughput also moved 20.1 -> 22.3 M ops/s
between the two libraries in the same run), so the x86_64 off-state gap of
section 4 is inside what that row does between runs and is not an
instruction cost. L1D and dTLB misses per op are unchanged in every cell.
