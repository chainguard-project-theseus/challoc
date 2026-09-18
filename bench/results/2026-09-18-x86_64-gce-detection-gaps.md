# challoc detection gaps (flush boundary, sized path, double_free_scan), 2026-09-18, x86_64 (GCE C4)

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

x86_64, working tree (kept-half pass) against 48c845f:

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.7 (15.1 MiB) | 45.6 (15.0 MiB) | 45.0 (17.8 MiB) | +0.2% | -1.3% |
| larson10 | M ops/s | 278.7 (92.9 MiB) | 278.5 (94.4 MiB) | 297.4 (93.1 MiB) | +0.1% | +6.8% |
| xmalloc | M frees/s | 330.489 (52.9 MiB) | 308.034 (52.5 MiB) (median of 3) | 329.182 (96.2 MiB) | +7.3% | +6.9% |
| alloc-test1 | s | 3.409 (13.5 MiB) | 3.400 (13.6 MiB) | 3.227 (13.1 MiB) | -0.3% | +5.4% |
| valkey1 | M ops/s | 14.735 (585.0 MiB) | 14.523 (585.1 MiB) | 14.855 (591.9 MiB) | +1.5% | +2.3% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6662 (49.9%) | new compacts (end occupancy >= 90%) | 20847312 ops/s |

Positive percentages mean new did worse (slower, lower throughput). One round each, x86_64, nproc 16, load at start 6.68, 145 s total. old 67b42ef347cb3b81, new 3c8e71a1d32b85fa.

## 2. `quick`, landed variant: one compare at the split point

x86_64, working tree ((a) boundary compare, (b) guard test on the sized path
and the `Fresh`/guard checks at the release) against 48c845f (go):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.7 (15.0 MiB) | 45.5 (15.0 MiB) (median of 3) | 44.4 (17.9 MiB) | +0.4% | -2.4% |
| larson10 | M ops/s | 280.0 (93.3 MiB) | 287.9 (92.1 MiB) | 287.1 (92.9 MiB) | -2.7% | -0.3% |
| xmalloc | M frees/s | 320.836 (52.3 MiB) | 320.150 (52.5 MiB) | 329.444 (89.7 MiB) | +0.2% | +2.9% |
| alloc-test1 | s | 3.392 (13.6 MiB) | 3.423 (13.5 MiB) | 3.229 (13.0 MiB) | +0.9% | +6.0% |
| valkey1 | M ops/s | 14.752 (585.1 MiB) | 14.564 (585.1 MiB) | 14.332 (591.7 MiB) | +1.3% | -1.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6660 (49.9%) | new compacts (end occupancy >= 90%) | 20651555 ops/s |

Positive percentages mean new did worse (slower, lower throughput). One round each, x86_64, nproc 16, load at start 2.06, 150 s total. old 67b42ef347cb3b81, new 7d8248b2475b1720.

The gate is go on both VMs (this file's twin has the other). xmalloc is
back inside noise (+0.2%); valkey1, the only row that frees through
`sdallocx`, is +1.3% in this single round, which is what the guard test
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

## 4. (c) `double_free_scan`

(filled below)
