# Phase 2.5 floor fixes (lane B): the churn on 5.10/6.1, `c_thp_always` at 64 KiB, `thp:never` on `always` hosts

Base 9a9c635 (`loop/lane-b`); fixes f78601e (task 10), 4b1e92d (task 9),
71f54e0 (task 16). Lane D's acceptance campaign
(`bench/results/2026-09-19-phase-2.5-acceptance.md` sections 1.2 and 1.3,
lane D's worktree) found the two floor failures; this file records their
mechanism, the fixes and the re-runs. VMs, all under `CHALLOC_GCE_PREFIX=
challoc-lane-b`, zone `us-east4-a`, created and deleted this iteration:
lab cells `challoc-lane-b-matrix-{k61-arm64,k510-amd64,64k-arm64}` (the
matrix cells' images and machines: Debian 12 arm64 on c4a-standard-4 with
6.1.0-53-cloud-arm64, Debian 11 on e2-standard-4 with 5.10.0-46-cloud-amd64,
Ubuntu 24.04 arm64 on c4a-standard-4 with 6.8.0-139-generic-64k and 64 KiB
pages), the lane VM `challoc-lane-b-arm64` (c4a-standard-16, Ubuntu 24.04,
7.0.0-1011-gcp, 4 KiB pages), and the four throwaway cells of section 5.
Debian's kernels boot with THP `always`; Ubuntu's with `madvise`.

## 1. The churn on the Debian kernels: mechanism

`tests/c/cgroup_churn.c purge 400 100` in a `--memory=512m` container:
400 MiB allocated (200 x 1 MiB huge runs, 3200 x 32 KiB, 102400 x 1 KiB),
freed, and RSS must be under 100 MiB within 2 s of an idle trickle. Usage
at the frees is 403 MiB of 512 (79%): the policy's `Scaled` band.

The churn's policy line now prints the two-advice counters
(`purge_advice`, `lazy_purges`, `eager_purges`; `tests/c/cgroup_churn.c`),
and the pre-fix library run with the new test program shows the
mechanism directly (the `mechanism` rows below): a failing run has
`eager_purges 1 lazy_purges 18` and RSS 309 MiB at 2 s with
`empty_bytes 8 MiB` (the memory *is* purged, lazily); a passing run has
`eager_purges 3` and RSS 16 MiB.

What happens: the three classes of the churn go `Empty` a few
milliseconds apart (small objects are freed first, then the medium
class, then the huge runs), so their decay walks come due a few
milliseconds apart. A periodic tick (every 64 slow paths, every fourth
trickle step, about 40 ms) runs every walk that is due. When the three
walks fall in one tick, all are issued with the advice of that tick's
sample (403 MiB, `Scaled`: `MADV_DONTNEED`) and RSS drops to 16 MiB in 49
ms. When the frees straddle a tick boundary, the first tick returns the
small class eagerly (100 MiB), `memory.current` falls to 303 MiB (59%),
the next sample puts the band at `Low`, `policy::evaluate` mirrors it
into `purge::set_headroom`, and the remaining walks (the medium class
and the huge extent, 300 MiB; or the huge extent alone, 200 MiB) go out
with `MADV_FREE`: resident and charged until the kernel reclaims them,
which a cgroup at 60% never asks for. The allocator's `empty_bytes` says
the cap is met; the cgroup's `memory.current` says 310 MiB. Which tick a
walk lands in depends on how long the frees take relative to the tick,
so a 4-vCPU host splits them some of the time (7 of 16 on the C4A-4, 5
of 16 on the e2) and the 16-vCPU lane VM never did (16 of 16 pass at the
same tree, section 3): the kernel version is not the term, and lane D's
`lazy_band:false` bisect (24 of 24) and pre-task-10 tree (12 of 12) are
the same fact seen from the other side. The kernel rows lane D pointed
at (6.11 lazyfree THP, 6.14 PTE-page free, 6.17 per-VMA locks) change how
fast an eager purge drops RSS and how a lazy THP is reclaimed, not
whether `MADV_FREE`d pages stay charged: they do on every kernel
(`docs/research.md`, cgroups).

Neither of the coordinator's readings of candidate (a) held as written:
the mirror was not stale (the sample that flipped it to `Low` was fresh
and correct: usage really was under 70%) and `memory.current` was not
above the band after the lazy pass. The rule that follows from the
mechanism is about the cap, not the mirror: the retention cap is a
promise about resident, charged memory, and a lazy purge does not keep
it, so an excess over the cap must be returned with the advice of the
band it was first seen in. Candidates (b) and (c) (probe whether
`MADV_FREE`d pages are uncharged promptly, or gate the band on
`memory.stat`'s `lazyfree` line) were not taken: no kernel uncharges
them before reclaim, so the probe would disable the band everywhere, and
`memory.stat`'s `pglazyfree` exists since 4.12, on 5.10 and 6.1 alike, so
its presence does not distinguish the cells.

## 2. The fix (f78601e): the excess is returned with the advice it was first seen in

`policy::Policy::eager_excess`: set by `apply` when the retention is
over the cap and the band is not `Low` (70% and above, or unlimited);
cleared when the retention is back within the cap. `Policy::headroom` is
`band == Low && !eager_excess`, and `evaluate` mirrors that (not the bare
band) into `purge::set_headroom`; `advice_reason` reads `returning the
excess over the cap` while it lasts. An excess first seen under 70% (the
lazy container test, 200 MiB at 39%) stays lazy throughout; within the
cap the band alone decides, as before. Design 8 and 11 (a new row in the
policy table). The change is in the policy evaluation (every 10 ms at
most, slow path) and the batch flush's advice read: no fast path moved;
`quick` in section 6 is the go/no-go for that.

### 2.1 The churn, per cell, 16 runs before and 24 after

Each row is `bash ~/logs/churn-batch.sh`: the exact `docker run
--memory=512m --memory-swap=512m ... sh scripts/cgroup-tests.sh churn
challoc purge 400 100` command of `make test-cgroup-churn` (from `make
-n`), repeated. `before` is the pre-fix tree 9a9c635 with its own test
program; `mechanism` is the pre-fix library with the new test program
(the counters); `after` is the fixed tree. Logs fetched to
`.tmp-logs/cells/` (not committed; the per-run lines are summarised
here).

| cell | kernel, machine, THP | tree | ok / runs | failing runs: RSS at 2 s (`lazy_purges`/`eager_purges`) | passing runs: RSS at 2 s, `below_ms` |
|---|---|---|---:|---|---|
| k61-arm64 | 6.1.0-53-cloud-arm64, c4a-standard-4, `always` | before 9a9c635 | 9 / 16 | 309 MiB x6, 216 MiB x1 | 16 MiB, 47..50 ms |
| k61-arm64 | | mechanism (old lib, new test) | 6 / 8 | 309 MiB x2 (18/1) | 16 MiB (20..22/3), 47..49 ms |
| k61-arm64 | | after 71f54e0 | **24 / 24** | | 12 MiB x11 (13..16/4..5, `below_ms` 98..101: the split-tick runs, now eager to the end), 16 MiB x12 (20..23/3, 48..50 ms: one-tick runs), 4 MiB x1 |
| k510-amd64 | 5.10.0-46-cloud-amd64, e2-standard-4, `always` | before 9a9c635 | 11 / 16 | 329 MiB x2, 321, 320, 217 MiB | 16 MiB, 54..57 ms |
| k510-amd64 | | mechanism (old lib, new test) | 5 / 8 | 337 (18/1), 316 (9/1), 217 MiB (25/2, `empty_bytes` 0) | 16 MiB (29/3), 54..56 ms |
| k510-amd64 | | after 71f54e0 | **24 / 24** | | 12 MiB x9 (12..15/5, `below_ms` 102..108), 16 MiB x15 (29/3, 55..56 ms) |
| arm64 lane VM | 7.0.0-1011-gcp, c4a-standard-16, `madvise` | before 9a9c635 | 16 / 16 | | 16 MiB |

The 217 MiB failures are the huge extent alone going lazy (the small and
medium classes were returned in the first tick); 309..337 MiB is the
medium class and the huge extent. After the fix the split-tick runs are
still about half of the runs on both cells (the `below_ms` of 98..108
ms rows) and every one of them returns the whole excess eagerly.

### 2.2 `make test-cgroup` (every limit) on the 7.0 lane VM, fixed tree

| row | verdict | RESULT |
|---|---|---|
| churn `purge 400 100` at 512m | ok | rss 403 -> 16 MiB at 2 s, under 100 MiB after 48 ms |
| churn `keep 400 100` at 512m (`cgroup:false`) | ok | rss 317 MiB at 2 s (kept) |
| churn `purge 200 50` at 256m | ok | rss 202 -> 9 MiB, after 80 ms |
| churn `keep 200 50` at 256m | ok | rss 160 MiB at 2 s |
| lazy `lazy 200` at 512m (defaults, 39%: the `Low` band from the start) | ok | rss 202 / 202 MiB, `lazy_purges 35 eager_purges 0`, `pglazyfree` +48568 pages, reuse 5 ms (first allocation 59 ms), pressure set fits (`current` 512 MiB, `pglazyfreed` +17973) |
| lazy `eager 200` at 512m (`lazy_band:false`) | ok | rss 202 -> 12 MiB, `eager_purges 35`, reuse 47 ms |
| oscillate 300 s at 512m (200..480 MiB live) | ok | 98 cycles, `current_peak` 502 MiB, `max 0 oom_kill 0` |

`make test` on the same VM at the fixed tree: 379 + 115 + 23 passed, 0
failed (the 379 includes the new `policy::eager_return_of_the_excess_
holds_until_within_the_cap` and `thp::always_refuses_chunks_on_a_pmd_
over_2m`).

## 3. `c_thp_always` at 64 KiB pages: mechanism and fix (4b1e92d)

Reproduced on the 64k lab cell at 9a9c635 (`api::ctests::c_thp_always`
under `MALLOC_CONF=thp:always`): `sysfs thp=madvise mthp=512M`, mode 2,
`class_ranges 5 class_bytes 1698693120` (1.7 GiB of `MADV_HUGEPAGE`d,
committed chunks for the test's 8 MiB of 64-byte blocks), `VMA hg=1
THPeligible=0 AnonHugePages=0 kB`, and the test's two 2 MiB assertions
fail. Two mistakes: `thp::decide` took `Mode::Chunks` for any enabled PMD
size, and at 64 KiB pages the PMD is 512 MiB (`hugepages-524288kB` on
`inherit` under a `madvise` top level; `page * page / 8`, 32 MiB at 16
KiB), so `commit_slab` committed and advised a 512 MiB chunk around every
slab; and the test matched `2M` as a substring of `512M` and expected a
2 MiB page.

Fix: `LG_PMD_CHUNK_MAX = 21`: the chunk path needs the PMD enabled *and*
no larger than 2 MiB; otherwise `always` is the folio path over the
enabled sizes below the PMD (2 MiB slabs and up when `hugepages-2048kB`
is enabled at 64 KiB) and inert, reason `PmdTooLarge`, when none is. The
test reads `hpage_pmd_size`, compares mthp tokens, and expects the folio
path or the inert mode on a PMD over 2 MiB; the 4 KiB expectations are
unchanged and pass on the 7.0 lane VM (`always`: mode 2, `class_ranges 9`,
`AnonHugePages 10240 kB`, THPeligible 1). On the 64k cell with the fix:
`make test` 379 + 115 + 23 passed, 0 failed; `always`: `mode 0
class_ranges 0 ... inert on this host (the PMD is over 2 MiB and no
smaller size is enabled)`; `auto`: inert (no size below the PMD);
`never`: `nh=1`. Host-side `thp::always_refuses_chunks_on_a_pmd_over_2m`
covers the 64 KiB and 16 KiB tables and the 2 MiB slab folio case.

## 4. `thp:never` on an `always` host (71f54e0, plan task 16, first half)

`tests/c/thp.c never` gained: the global mode read from sysfs (checked
against the feature line), the `nh` flag of the VMA holding the small
blocks and of an 8 MiB huge run, THPeligible of both, and on an `always`
host their AnonHugePages after the touch. Before and after, same test
program, pre-fix library (`.tmp-before.so`, the tree at 9a9c635) against
the fixed one:

| host | global THP | library | class region (8 MiB of 64 B blocks): hg / nh / THPeligible / AnonHugePages | huge run (8 MiB): hg / nh / THPeligible / AnonHugePages | verdict |
|---|---|---|---|---|---|
| k61-arm64 (6.1, Debian) | `always` (native) | 9a9c635 | 0 / 0 / 1 / 0 kB | 0 / 0 / 1 / **8192 kB** | 4 failures (no `nh`, eligible, huge run backed by 2 MiB pages under `never`) |
| k61-arm64 | `always` | 71f54e0 | 0 / **1** / 0 / 0 kB | 0 / **1** / 0 / **0 kB** | ok |
| arm64 lane VM (7.0, Ubuntu) | `always` (set with `echo always > .../enabled` for the run, `madvise` restored after) | 9a9c635 | 0 / 0 / 1 / 0 kB | 0 / 0 / 1 / **8192 kB** | 4 failures |
| arm64 lane VM | `always` | 71f54e0 | 0 / 1 / 0 / 0 kB | 0 / 1 / 0 / 0 kB | ok |
| arm64 lane VM | `madvise` (the VM's own) | 71f54e0 | 0 / 1 / 0 / 0 kB | 0 / 1 / 0 / 0 kB | ok ("AnonHugePages 0 / 0 kB is 0 with or without the advice; the nh flags are the check here") |
| 64k-arm64 (6.8, 64 KiB pages) | `madvise` | 71f54e0 | 0 / 1 / 0 / 0 kB | (the huge-run row was added after this cell's run; section 5's cell covers it) | ok |

The class region shows 0 kB even before the fix because its groups are
committed a few slabs at a time and no whole aligned 2 MiB range is
read-write yet (THPeligible 1 says it would be, and khugepaged collapses
such ranges later); the huge run is committed whole and the kernel backs
its interior with 2 MiB pages at the fault. That is the row that moves.
`always` and `auto` are unchanged by the fix (on the `always` host,
`auto`'s inert regions still show the huge run at 8192 kB: the process-wide
`PR_THP_DISABLE_EXCEPT_ADVISED` prctl of 6.18 is the second half of the
task and waits for its key, `PROGRESS.d/lane-b-floor-fixes.md`).

## 5. The four matrix cells from the fixed tree (`challoc-bench matrix`, throwaway VMs, the full cell script)

`MATRIX_DATE=2026-09-19-lane-b-floor-fixes bench/gce/challoc-bench matrix
k510-amd64 k61-amd64 k61-arm64 64k-arm64` at 71f54e0; logs
`bench/results/matrix/2026-09-19-lane-b-floor-fixes-<cell>.log`. `tests`
counts every `test result:` line of `make test`.

| cell | kernel | page | machine | make test | fuzz | test-cgroup | wall | outcome | lane D at 675f357 |
|---|---|---|---|---|---|---|---|---|---|
| k510-amd64 | 5.10.0-46-cloud-amd64 (Debian 11) | 4096 | e2-standard-4 | 517/0 | PASS | PASS (326 s: churn x4, lazy x2, oscillation) | 1161 s | **PASS** | FAIL (churn) |
| k61-amd64 | 6.1.0-53-cloud-amd64 (Debian 12) | 4096 | e2-standard-4 | 517/0 | PASS | PASS | 1164 s | **PASS** | FAIL (churn) |
| k61-arm64 | 6.1.0-53-cloud-arm64 (Debian 12) | 4096 | c4a-standard-4 | 517/0 | PASS | PASS | 582 s | **PASS** | FAIL (churn) |
| 64k-arm64 | 6.8.0-139-generic-64k (Ubuntu 24.04) | 65536 | c4a-standard-4 | 517/0 | PASS | PASS | 601 s | **PASS** | FAIL (`c_thp_always`) |

Probe lines (the `PROBE features:` line of each cell): the three Debian
cells `page_size=4096 guard_install=no mseal=no process_madvise=no
process_madvise_free=no anon_name=no thp=always mthp=2M collapse=unprobed
rseq=yes rseq_size=20 mm_cid=no` (x86_64: `lam=no uai=no la57=no
la57_enabled=no cldemote=no waitpkg=no erms=yes fsrm=no avx512f=no
cacheline=64 l1d=32K l2=256K`; arm64: `mte=no mte3=no pac=yes lse=yes
lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M`); the 64 KiB cell
`page_size=65536 ... anon_name=yes thp=madvise mthp=512M collapse=unprobed
rseq=yes rseq_size=28 mm_cid=yes ...`: the same as lane D's lines, so the
fixes changed no probe. `make test` is 379 + 115 + 23 on every cell (513
at 675f357: two unit tests from lane B's earlier commits fc081b1 and
9a9c635, and this iteration's two). The four cells lane D marked FAIL are green; the floor criterion
of the phase ("green on every kernel cell of task 3, including 5.10 and
64 KiB pages") holds on them at 71f54e0. The five cells lane D marked
PASS were not re-run here (the changes are the policy's advice under a
limit, the `always` chunk bound and `never`'s advice; the 7.0 lane VM's
`make test`, `make test-cgroup` and `quick` above cover the reference
kernel).

## 6. `quick` on the arm64 lane VM (fast-path go/no-go)

`challoc-bench quick arm64 OLD=9a9c635` (old = the pre-fix tree, new =
71f54e0; the purge change is slow-path only, this is the check that
nothing on the fast paths moved).

One round each, aarch64, nproc 16, load at start 3.05, 124 s; positive
is new worse. Libraries `b04b5bb826ecd6cd` (old) and `9d722043d0bb74ce`
(new).

| row | unit | old 9a9c635 | new 71f54e0 | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.1 (15.9 MiB) | 42.6 (15.9 MiB) | 45.1 (18.1 MiB) | -1.2% | +5.9% |
| larson10 | M ops/s | 362.4 (111.9 MiB) | 367.8 (114.6 MiB) | 384.4 (132.0 MiB) | -1.5% | +4.5% |
| xmalloc | M frees/s | 589.031 (55.4 MiB) | 585.777 (53.2 MiB) | 471.833 (81.4 MiB) | +0.6% | -19.5% |
| alloc-test1 | s | 2.700 (13.0 MiB) | 2.723 (13.0 MiB) | 2.562 (12.3 MiB) | +0.9% | +6.3% |
| valkey1 | M ops/s | 6.796 (584.2 MiB) | 6.873 (584.2 MiB) | 6.924 (585.4 MiB) | -1.1% | +0.7% |
| churn 1024, 8 s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go` (new within 5% of old on every row; the churn probe compacts).
No fast path changed in the three commits (`src/heap.rs`, `src/class.rs`,
`src/slab.rs`, `src/api/*.rs` untouched; the `thp` and `policy` changes
are init-time and slow-path), so no attribution is owed and none was
taken.
