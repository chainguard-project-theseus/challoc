# 2026-09-19: multi-size THP for the slab regions behind `thp` (Phase 2.5 task 9, lane B)

- change: cc4aa34 (`thp` stops being inert: `thp:auto` advises 64 KiB folios on the slab groups
  and the metadata, `thp:always` commits and advises 2 MiB chunks, huge runs and the metadata;
  `Features` reads the THP sysfs table and probes `MADV_COLLAPSE`; `thp.rs`, design 3, 5, 7, 10,
  11, 12, 16, 17). Base 3a6da06 (lane B after task 10; main's a3ddd6d plus the lane's commits).
- VMs: `challoc-lane-b-arm64` (c4a-standard-16, Axion / Neoverse V2) and `challoc-lane-b-amd64`
  (c4-standard-16, Xeon Platinum 8581C), zone `us-east4-a`, Ubuntu 24.04 with the GCE kernel
  `7.0.0-1011-gcp`, 4 KiB pages, the PMU exposed; the dev container (Wolfi, glibc 2.44) for every
  row, `perf` on the VM host. Both VMs created for this task and deleted at its end.
- what moved: the commit paths only (`SlabTable::take_group`, `SlabTable::commit_slab`,
  `Lazy::reserve`/`ensure`, `Huge::prepare`, `meta::Inner::commit`/`commit_bump_to`), each behind
  a load-and-compare of the mode; the fast paths are untouched (`heap.rs`, `slab.rs`, `api/*.rs`
  unchanged bar the report and `mallctl` leaves). No scorecard (a lane never runs it).

## 1. The kernel's table on the reference kernel (7.0.0-1011-gcp, both architectures)

`/sys/kernel/mm/transparent_hugepage`: `enabled` `always [madvise] never`, `defrag` `[madvise]`,
`hpage_pmd_size` 2097152; per-size directories `hugepages-8kB` (no `enabled` file: order 1 is
for file folios only), `hugepages-16kB` .. `hugepages-1024kB` all `[never]`, `hugepages-2048kB`
`[inherit]`; `khugepaged/defrag` 1, `pages_to_scan` 4096, `scan_sleep_millisecs` 10000,
`max_ptes_none` 511. Identical on the C4A and the C4.

The probe's line under `thp:never` (the default; `tests/c/features.c` under `make test`, the same
bytes in `malloc_stats_print`):

```
C4A: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
C4:  page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
```

(`mseal=no process_madvise=no` are Docker's default seccomp profile, as in the feature report.)
So on Ubuntu's default sysfs the only folio size a `MADV_HUGEPAGE`d range can get is the PMD's
2 MiB, `madvise`-gated: `thp:auto` is inert there by design (section 3) and says so in the report
(`thp: auto -> off (no folio size below the PMD is enabled in sysfs)`).

Host requirement for the multi-size path, root only, recorded here as the task asked:

```
echo madvise | sudo tee /sys/kernel/mm/transparent_hugepage/hugepages-64kB/enabled
echo never   | sudo tee /sys/kernel/mm/transparent_hugepage/hugepages-2048kB/enabled
```

after which the line reads `thp=madvise mthp=64K` and `thp:auto` is `folios`. With
`hugepages-64kB=madvise` and `hugepages-2048kB` left at `inherit` the line reads `mthp=64K,2M`,
`auto` refuses (`the PMD size is madvise-gated in sysfs, the advice would enable 2 MiB pages`)
and `always` is `chunks`. The measurement states below are exactly these two, set by
`~/logs/sysfs.sh auto|always|default` on the VM between runs.

`MADV_COLLAPSE`: probed with a populated, 2 MiB-aligned, 2 MiB range of a fresh mapping (the
syscall acts on whole PMD ranges only; a request over a 64 KiB range returns 0 having collapsed
nothing, `mm/khugepaged.c` `madvise_collapse` rounds the range to PMD boundaries and the loop
runs zero times, so the smallest enabled size cannot be probed as the task first proposed), and
only when `thp` is not `never`, since the probe costs a PMD of memory for a moment: `collapse=2M`
on both VMs under `thp:auto` and `thp:always` (`tests/c/features.c` run by hand with
`MALLOC_CONF=thp:auto|always` in the container: `thp=madvise mthp=64K collapse=2M` on the C4A in
the auto state, `thp=madvise mthp=64K,2M collapse=2M` on the C4 in the always state), `unprobed`
by default. Reported, not used: the advice at commit gives the regions their folios at the first
touch, and the metadata takes its folio through the rounded, folio-aligned commit rather than a
copy afterwards (section 2).

## 2. The mechanism, and what the first form got wrong

`thp:auto`: `take_group` issues one `MADV_HUGEPAGE` over the group's range (still `PROT_NONE`; the
flag survives the commit's split and merges with the neighbouring committed groups) for every
class whose slab holds an enabled folio below the PMD; every 64 KiB slab then faults in as one
64 KiB folio (`hugepages-64kB/stats/anon_fault_alloc` +128 for 8 MiB of 64-byte blocks, exactly
one per slab, `tests/c/thp.c auto`). Metadata tables (`Lazy`) start on a folio boundary and their
commits are rounded to the folio, the bump area's 64 KiB chunks are advised as they are.

`thp:always` with the PMD enabled: `commit_slab` commits and advises the PMD-sized chunk around
the slab (clipped to the region; every slab of the chunk re-issues the two calls, no-ops after the
first, since groups are 1..16 slabs and arenas commit theirs in any order). The first form
installed the guard slabs' markers as usual and every 2 MiB page was gone by the time the slab
was in use: `AnonHugePages` 0 kB on the class VMA after 8 MiB touched, `thp_fault_alloc` +8 and
`thp_split_pmd` +8 per run, a `MADV_GUARD_INSTALL` inside a PMD-mapped page splits it. Under
chunks a guard slab is now left unused and unguarded; `AnonHugePages` 10240 kB of a 10240 kB VMA
after the fix. A purge (`MADV_DONTNEED` of one 64 KiB slab) inside a 2 MiB page splits it too;
that is the kernel's price of the mode and the decay pays it.

## 3. `quick-perf`: per-operation counters (one run each, attribution, not a gate)

`challoc-bench quick-perf <vm> OLD=3a6da06 ROWS='larson1 valkey1 valkey1-free'`; `old` is
3a6da06, `new` the tree at cc4aa34, `new:thp:auto` and `new:thp:always` the same library with
the key; two sysfs states (section 1). `valkey1` figures are over the whole process (fill and
both phases) divided by the two phases' operation count, as `bench/README.md` says.

C4A (aarch64), auto state (`mthp=64K`):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 42.15 | 70.9 | 249.4 | 2.093 | 4.020 |
| larson1 | new | 42.07 | 71.1 | 249.4 | 2.095 | 4.008 |
| larson1 | new:thp:auto | 41.66 | 71.8 | 249.4 | 2.091 | **3.049** |
| larson1 | jemalloc | 44.67 | 66.6 | 224.8 | 2.069 | 1.749 |
| valkey1 | old | 13.66 | 222.1 | 230.7 | 3.070 | 2.820 |
| valkey1 | new | 13.65 | 222.1 | 230.0 | 3.062 | 2.805 |
| valkey1 | new:thp:auto | 13.55 | 223.3 | 228.7 | 3.062 | **2.444** |
| valkey1 | jemalloc | 14.40 | 210.1 | 170.8 | 2.972 | 2.692 |
| valkey1-free | old | 13.70 | 221.2 | 232.5 | 4.037 | 3.912 |
| valkey1-free | new | 13.34 | 227.3 | 234.9 | 4.045 | 3.908 |
| valkey1-free | new:thp:auto | 13.38 | 226.3 | 233.6 | 4.051 | **2.900** |
| valkey1-free | jemalloc | 13.86 | 218.3 | 173.1 | 3.425 | 3.354 |

C4A, always state (`mthp=64K,2M`):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | new | 42.10 | 71.1 | 249.3 | 2.100 | 3.985 |
| larson1 | new:thp:always | 41.40 | 72.2 | 249.4 | 2.095 | 2.675 |
| valkey1 | new | 13.66 | 221.9 | 230.7 | 3.066 | 2.819 |
| valkey1 | new:thp:always | 14.90 | 201.4 | 219.3 | 3.025 | 0.654 |
| valkey1-free | new | 13.18 | 229.9 | 235.7 | 4.046 | 3.920 |
| valkey1-free | new:thp:always | 14.79 | 202.9 | 223.2 | 4.001 | 1.275 |

C4 (x86_64), auto state:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 43.82 | 75.0 | 234.3 | 1.575 | 0.002 |
| larson1 | new | 45.22 | 72.7 | 234.2 | 1.574 | 0.002 |
| larson1 | new:thp:auto | 44.97 | 73.0 | 234.2 | 1.641 | 0.002 |
| larson1 | jemalloc | 45.10 | 72.4 | 200.9 | 2.106 | 0.001 |
| valkey1 | old | 22.81 | 148.0 | 231.2 | 3.312 | 0.902 |
| valkey1 | new | 23.26 | 145.0 | 230.5 | 3.282 | 0.897 |
| valkey1 | new:thp:auto | 23.09 | 145.5 | 230.2 | 3.304 | 0.907 |
| valkey1 | jemalloc | 25.69 | 131.0 | 154.9 | 3.370 | 0.954 |
| valkey1-free | old | 22.39 | 151.2 | 232.1 | 4.615 | 0.930 |
| valkey1-free | new | 23.26 | 151.0 | 228.8 | 4.597 | 0.927 |
| valkey1-free | new:thp:auto | 23.32 | 149.6 | 228.2 | 4.589 | 0.925 |
| valkey1-free | jemalloc | 24.09 | 145.6 | 158.8 | 4.686 | 1.186 |

C4, always state:

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | new | 45.01 | 73.1 | 234.1 | 1.560 | 0.002 |
| larson1 | new:thp:always | 45.61 | 72.0 | 233.9 | 1.652 | 0.001 |
| valkey1 | new | 23.79 | 147.4 | 229.3 | 3.281 | 0.900 |
| valkey1 | new:thp:always | 27.54 | 126.2 | 224.5 | 1.438 | 0.004 |
| valkey1-free | new | 23.77 | 147.5 | 228.6 | 4.582 | 0.928 |
| valkey1-free | new:thp:always | 26.88 | 129.2 | 224.3 | 2.569 | 0.004 |

Reading:

- `old` against `new` under the default is the byte-for-byte claim in counters: within a run's
  noise on every column on both VMs (larson1 dTLB 4.020 vs 4.008, valkey1 2.820 vs 2.805 on the
  C4A; the C4's larson1 4% throughput gap between old and new is run noise, the two builds
  differ in cold code only and the always-state `new` run lands between them).
- The aarch64 dTLB column, the gap the task names: `larson1` 4.01 -> 3.05 refills per operation
  under `auto` (jemalloc 1.75: the gap 2.26 -> 1.30, 42% of it closed), `valkey1` 2.81 -> 2.44
  (jemalloc 2.69: below it), `valkey1-free` 3.91 -> 2.90 (jemalloc 3.35: below it). Under
  `always` (2 MiB pages) 2.68 / 0.65 / 1.28. The rest of larson1's gap to jemalloc is not the
  page geometry: at 2 MiB pages challoc still refills 2.68 per operation against jemalloc's 1.75
  at 4 KiB pages, so jemalloc's larson footprint touches fewer pages per operation (its 12
  metadata pages against the dense descriptor, bitmap and set tables of design 4 and 5, and its
  tcache in the thread's own arena chunk).
- Cycles and throughput under `auto`: within one run's noise (larson1 -1.0%, valkey1 -0.7%,
  valkey1-free +0.3% on the C4A); the 64 KiB folio's TLB saving on a 2 KiB TLB (Neoverse V2:
  48-entry L1 dTLB, 2048-entry L2 TLB) is a quarter of the refills, not a quarter of the cycles,
  because a refill from the L2 TLB is cheap and the misses that cost are the descriptor and slot
  loads the L1D column shows unchanged. Under `always` valkey1 gains 9% (C4A) and 16% (C4) of
  throughput and drops 9% / 14% of cycles per operation, the L1D column falls on the C4 (3.28 ->
  1.44: page-table lines no longer walk through L1D), the RSS price is in sections 4 and 5.
- x86_64 has the 64 KiB folio too (the per-size knobs are architecture independent since 6.8)
  but no TLB entry for it: `auto` is a no-op for the counters there (dTLB 0.907 vs 0.897,
  cycles +0.3%), as the task predicted; only fewer faults at commit (section 4).

## 4. Fork cost (`tests/c/fork_cost.c 500`, five runs per mode, medians with min..max)

A 500 MiB Valkey-shaped heap (values uniform 16..1024 bytes behind 16-byte objects, 972k values),
`fork()`, the parent writes one byte per value while the child is alive (every page copied), the
child exits when the burst is over. `fork_us` is the `fork()` call, `burst` the copy-on-write
burst, `total` fork to the child's exit, RSS the parent's peak. Each mode in its sysfs state
(`~/logs/forkcost.sh`, `MALLOC_CONF=thp:<mode>`, the container's `libchalloc.so` at cc4aa34).

C4A (aarch64):

| mode | fork() ms | burst ms | burst faults | total ms | maxrss MiB |
|---|---:|---:|---:|---:|---:|
| never | 18.89 (18.45..19.49) | 381 (358..401) | 136690 | 458 (431..480) | 556.3 |
| auto | 15.48 (15.09..15.50) | 370 (361..392) | 136690 | 431 (426..458) | 556.8 |
| always | 0.66 (0.64..0.74) | 346 (335..366) | 136689 | 348 (337..368) | 601.5 |

C4 (x86_64):

| mode | fork() ms | burst ms | burst faults | total ms | maxrss MiB |
|---|---:|---:|---:|---:|---:|
| never | 7.91 (7.45..12.76) | 254 (254..259) | 136689 | 291 (286..300) | 556.7 |
| auto | 10.21 (9.07..10.39) | 260 (255..266) | 136689 | 298 (288..301) | 557.3 |
| always | 0.73 (0.71..0.82) | 263 (257..271) | 136689 | 265 (259..273) | 602.1 |

Reading:

- The copy-on-write cost that made Redis and Valkey forbid huge pages is not amplified at 64 KiB:
  the burst faults are identical in every mode (one per page written; the kernel copies one 4 KiB
  page per write fault whatever the folio behind it, 6.8+ `wp_page_copy` falls back to order 0)
  and the burst time is within noise (C4A 370 vs 381 ms, C4 260 vs 254). Under `always` the first
  write into each 2 MiB page splits it (`thp_split_pmd`) and the copies are 4 KiB as well; the
  fork-time story of the Redis documentation is about the 2 MiB copy of kernels before 5.8.
- `fork()` itself: `auto` is 18% faster on the C4A (15.5 vs 18.9 ms: the contiguous-PTE batching
  of `copy_present_ptes` on arm64 copies a 64 KiB folio's 16 entries as one) and 29% slower on
  the C4 (10.2 vs 7.9 ms, 2.3 ms on a 500 MiB heap: the same batching without the hardware hint
  costs the per-folio rmap work and saves nothing), `always` is 25x faster on both (512 fewer
  entries per 2 MiB). The criterion the plan states, fork time within 5% of today, holds for the
  whole fork-to-exit figure on both (C4A -6%, C4 +2.5%) and for the `fork()` call on the C4A;
  the C4's `fork()` call alone is outside it, which the decision in section 7 takes into account.
- RSS: `auto` +0.5 MiB (+0.1%: the metadata tables' rounded first folios), `always` +45 MiB
  (+8.1%: the chunks committed ahead and touched as whole 2 MiB pages, the guard slabs inside
  them now resident, and jemalloc-shaped 2 MiB retention until a split).

## 5. `ab`, five rounds (the protocol tier)

`challoc-bench ab <vm> VARIANTS='never=HEAD <mode>=HEAD:thp:<mode>' ROUNDS=5 BENCHES='larson1
larson10 valkey1 redis mstress'`, HEAD cc4aa34, the same library under two `MALLOC_CONF`s, five
timed rounds after a warm-up, interleaved; full tables with every run's score in
`2026-09-19-lane-b-mthp-ab-auto-aarch64.md`, `-ab-always-aarch64.md`, `-ab-always-x86_64.md`.
Positive percentages: the variant did worse than `never`.

C4A, `never` vs `auto` (auto state), the row that can win:

| program | unit | never | never RSS MiB | auto | auto RSS MiB | auto vs never | lists |
|---|---|---:|---:|---:|---:|---:|---|
| larson1 | M ops/s | 42.5 | 15.9 | 41.8 | 18.1 | +1.7% | disjoint (42.4..42.5 vs 41.7..41.9) |
| larson10 | M ops/s | 369.1 | 112.7 | 363.3 | 121.0 | +1.6% | disjoint (366.9..370.9 vs 360.1..364.0) |
| valkey1 | M ops/s | 7.376 | 584.6 | 7.383 | 586.1 | -0.1% | overlapping |
| redis | k req/s | 503.0 | 29.1 | 504.5 | 34.6 | -0.3% | overlapping |
| mstress | s | 0.374 | 321.3 | 0.361 | 324.4 | -3.5% | disjoint (auto faster) |

C4A, `never` vs `always` (always state), the row that can lose RSS:

| program | unit | never | never RSS MiB | always | always RSS MiB | always vs never |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.4 | 15.8 | 41.8 | 58.7 | +1.4% |
| larson10 | M ops/s | 369.0 | 112.6 | 362.8 | 167.8 | +1.7% |
| valkey1 | M ops/s | 7.310 | 584.6 | 8.128 | 640.9 | -10.1% |
| redis | k req/s | 502.0 | 29.1 | 507.6 | 103.7 | -1.1% |
| mstress | s | 0.372 | 322.9 | 0.344 | 523.9 | -7.5% |

C4, `never` vs `always` (always state):

| program | unit | never | never RSS MiB | always | always RSS MiB | always vs never |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.3 | 17.0 | 45.7 | 60.2 | -0.9% |
| larson10 | M ops/s | 285.4 | 97.0 | 295.2 | 153.2 | -3.3% |
| valkey1 | M ops/s | 15.214 | 585.7 | 17.950 | 641.6 | -15.2% |
| redis | k req/s | 656.6 | 30.1 | 660.5 | 105.6 | -0.6% |
| mstress | s | 0.621 | 343.1 | 0.606 | 540.6 | -2.4% |

Reading:

- `auto` on the C4A: the Valkey-shaped rows are flat (valkey1 -0.1%, redis -0.3%, both
  overlapping), mstress is 3.5% faster (its churn commits and purges slabs all the time, and a
  64 KiB folio is one fault and one zeroing pass per slab instead of sixteen), and the two larson
  rows lose 1.6..1.7% with disjoint sorted lists, a real loss. larson's working set is a few
  MiB across 8..10 threads: the folio saves it a quarter of its dTLB refills (section 3) but the
  refills it saves are the cheap L2-TLB kind, while every slab a thread touches now costs a
  64 KiB zeroing pass up front whether or not the thread uses more than its first page. That is
  also the RSS column: larson1 +2.2 MiB (+14%), larson10 +8.3 MiB (+7%), redis +5.5 MiB (+19%),
  valkey1 +1.5 MiB (+0.3%), mstress +3 MiB (+1%): a folio per touched slab and per metadata table
  where a page was enough. A large heap does not see it; a 16 MiB one does.
- `always`: the throughput row the task named, valkey1 +10% on the C4A and +15% on the C4 (the
  dTLB column of section 3 at 2 MiB pages), mstress 7.5% / 2.4% faster, larson within 2%; the
  RSS row it named too: valkey1 +56 MiB (+9.6%), larson1 3.7x, redis 3.6x, mstress +62%: every
  class region touched costs 2 MiB at once and a purge of one slab splits a 2 MiB page rather
  than freeing it. The mode is what jemalloc's `thp:always` is, priced.

## 6. `quick`, the go/no-go

`challoc-bench quick <vm> OLD=HEAD~1`, `old` 3a6da06 (the lane before this task, `thp` inert),
`new` cc4aa34 under the default `thp:never`; then the same with `thp:auto` in `/etc/challoc.conf`
inside the container (the old library reports the unknown value and stays inert) in the auto
sysfs state. One round each; the gate is 5%.

| VM | mode | larson1 | larson10 | xmalloc | alloc-test1 | valkey1 | churn | verdict |
|---|---|---:|---:|---:|---:|---:|---|---|
| C4A | never | +0.5% | +1.3% | +0.6% | -0.7% | +0.4% | 6801 -> 3462 (97.0%) | go |
| C4A | auto | +1.4% | +1.0% | +0.2% | -0.1% | +1.0% | 6801 -> 3462 (97.0%) | go |
| C4 | never | +1.8% | -1.5% | -5.5% | -1.0% | -2.6% | 6801 -> 3462 (97.0%) | go |
| C4 | auto | +0.0% | -1.4% | -1.7% | +0.7% | -2.5% | 6801 -> 3462 (97.0%) | go |

(`new vs old`, positive worse; RSS under `auto` on the C4A: larson1 15.9 -> 17.9 MiB, larson10
112.1 -> 121.6, alloc-test1 13.1 -> 14.9, valkey1 584.2 -> 585.7.) The default row is the
byte-for-byte claim at the tier's resolution: the two libraries differ (`thp.rs`, the probe, the
report; hashes `ab7ebcdd5145b098` -> `5754d8d111e5aa8c` on aarch64) and the rows do not.

## 7. Decision

The plan's rule: `auto` on by default only if the aarch64 dTLB column moves toward jemalloc's,
fork time stays within 5% of today, and the scorecard-shaped rows do not lose; else `never`
stays and `auto` is a documented opt-in.

- dTLB: holds. larson1 4.01 -> 3.05 refills per operation (jemalloc 1.75), valkey1 2.81 -> 2.44
  (below jemalloc's 2.69), valkey1-free 3.91 -> 2.90 (below 3.35) on the C4A (section 3).
- fork: holds for the whole fork-to-exit figure (C4A -6%, C4 +2.5%) and the copy-on-write burst
  (identical fault counts, time within noise); the `fork()` call alone is 18% faster on the C4A
  and 29% slower (2.3 ms per 500 MiB) on the C4, where the 64 KiB folio has no hardware meaning
  (section 4).
- rows: does not hold. larson1 and larson10 lose 1.6..1.7% with disjoint lists on the C4A, and
  small heaps grow 7..19% in RSS (a folio per touched slab and metadata table); the target rows
  (valkey1, redis) are flat and mstress wins (section 5).

**Decision: `thp:never` stays the default; `thp:auto` is the documented opt-in** (design 12 and
17, README): recommended for aarch64 hosts that enable `hugepages-64kB` (and keep the PMD size
out of `madvise`) for Valkey-shaped heaps, where it is neutral on throughput and RSS, takes a
quarter of the dTLB refills away and makes `fork()` 18% cheaper; on x86_64 it buys nothing but
fewer faults and costs `fork()` time, so it is not recommended there (the mode stays generic: the
kernel offers the folio on both, the decision to enable the knob is the host's). `thp:always` is
documented as jemalloc's `thp:always`: +10..15% on valkey1, +10% RSS on a 585 MiB heap and 2..4x
on small ones, `fork()` 25x cheaper, the copy-on-write burst unchanged. On Ubuntu's default sysfs
both opt-ins are inert or PMD-only as section 1 says, and the default configuration issues no
`madvise(MADV_HUGEPAGE)` at all.

What the measurement also says about the gap the task named: at 2 MiB pages challoc's larson1
still refills the dTLB 2.68 times per operation against jemalloc's 1.75 at 4 KiB pages, so most
of that gap is how many distinct pages an operation touches (descriptor, bitmap, set table,
cache, slab), not the page size; the review's `metadata_thp` idea is in (metadata takes the
folio under both modes) and moves the column by what the class-region folio does, no more.

## 8. Tests

`make test` at cc4aa34 on both VMs (Ubuntu's default sysfs, `~/logs/finaltest.sh`), exit 0:

```
C4A: test result: ok. 355 passed (host and container tests, --skip api::); ok. 113 passed (api::); ok. 23 passed (test-cgroup-limit, --memory=512m)
     OUTCOME churn alloc=challoc args='purge 400 100' status=0 verdict=ok oom_kill=0   (and 'purge 200 50', the two challoc-off 'keep' controls)
     OUTCOME lazy alloc=challoc args='lazy 200' status=0 verdict=ok oom_kill=0   OUTCOME lazy alloc=challoc-eager args='eager 200' status=0 verdict=ok
     thp never: mode 0 class_ranges 0 class_bytes 0 failures 0; VMA hg=0 THPeligible=0 AnonHugePages=0 kB
     thp always: mode 2 class_ranges 9 class_bytes 18874368 failures 0; VMA hg=1 THPeligible=1 AnonHugePages=10240 kB
     thp auto: mode 0 class_ranges 0 class_bytes 0 failures 0; VMA hg=0 THPeligible=0 AnonHugePages=0 kB   (inert on this host: no size below 2M enabled)
     fork_cost: fork_us=2173 burst_ms=42.8 burst_faults=17523 pages=16384 total_ms=51.1 maxrss_kb=74672   (64 MiB, thp:never)
C4:  test result: ok. 355 passed; ok. 113 passed; ok. 23 passed; the same six OUTCOME lines, verdict=ok
     thp always: mode 2 class_ranges 9 class_bytes 18874368 failures 0; VMA hg=1 THPeligible=1 AnonHugePages=10240 kB
     fork_cost: fork_us=1507 burst_ms=32.2 burst_faults=17523 pages=16384 total_ms=37.6 maxrss_kb=75156
```

The sysfs-gated tests in the auto state (`hugepages-64kB=madvise`, `hugepages-2048kB=never`;
`CHALLOC_THP_TESTS=1 cargo test --features std api::ctests::c_thp` and `c_fork_cost`), both VMs:

```
C4A: thp auto: mode 1 class_ranges 148 class_bytes 11730944 failures 0; VMA hg=1 THPeligible=1 AnonHugePages=0 kB
     thp auto: /sys/kernel/mm/transparent_hugepage/hugepages-64kB/stats/anon_fault_alloc 134368 -> 134625 (+257, 128 folios touched)
     thp always: mode 1 class_ranges 148 class_bytes 11730944 failures 0; VMA hg=1 THPeligible=1 AnonHugePages=0 kB   (no PMD size enabled: always is folios)
     test result: ok. 3 passed (c_thp_never, c_thp_always, c_thp_auto)
     fork_cost (thp:auto, 256 MiB):   fork_us=6522 burst_ms=162.6 burst_faults=70009 pages=65536 total_ms=186.5 maxrss_kb=293984
     fork_cost (thp:always, 256 MiB): fork_us=7187 burst_ms=156.7 burst_faults=70009 pages=65536 total_ms=182.9 maxrss_kb=293812   (folios here, the PMD size being never)
     test result: ok. 2 passed (c_fork_cost, c_fork_cost_thp_modes)
C4:  thp auto: mode 1 class_ranges 148 class_bytes 11730944 failures 0; VMA hg=1 THPeligible=1 AnonHugePages=0 kB
     thp auto: hugepages-64kB/stats/anon_fault_alloc 38538 -> 38798 (+260, 128 folios touched)
     test result: ok. 3 passed;  fork_cost (auto) fork_us=4203 burst_ms=120.5 ... maxrss_kb=294632;  test result: ok. 2 passed
```

Host: `cargo test --features std` 283 passed (the sysfs fixtures: `thp_setting_parses_the_bracketed_word`,
`thp_table_gce_default_lists_eight_sizes_and_enables_the_pmd_alone`, `thp_table_with_64k_madvise_and_pmd_never`,
`thp_table_before_6_8_has_the_pmd_alone`, `thp_table_missing_directory_is_none`; `thp::tests` for `decide`, the chunk
arithmetic and the statics; `config::tests::thp_values`). `tests/c/features.c` checks the three new keys; the
`always` mode is in `make test` because the reference kernel enables the PMD size without root; `auto` runs in the
suite as the inert check on a default host and does the folio checks where the knobs are set.
