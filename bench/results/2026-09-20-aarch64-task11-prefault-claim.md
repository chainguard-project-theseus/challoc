# 2026-09-20: prefaulted fresh slabs, the claim side and the acceptance (Phase 2.5 task 11, lane A), aarch64

- change: the claim-side half of the task, after lane B's commit-path half (7a42369, merged at
  8e2d201; `bench/results/2026-09-20-aarch64-lane-b-task11-prefault.md`). Reviewed: `take`,
  `next_slab`, `calloc`'s zero knowledge and the compaction paths against lane B's "a prefaulted
  slab is a `Purged` slab" decision (PROGRESS.d, Design decisions; confirmed, no correctness
  change). Added, behind the same key (`prefault:true` with the background thread), lane B's two
  hooks from its section 6: the wake from the claim path when a refill takes the arena's last
  idle slab of a class (`ClassState::plan` -> `purge::prefault::note_taken`, counted as `taken`),
  and a prefault pipeline sized to the class's burst rate (`prefault_delta`, `prefault_ahead`:
  `ceil(refilled slots since the last pass / nslots)` slabs kept idle, 1 at least, `MAX_AHEAD` = 8
  at most). Commit be7e74c, ac31001 after the rebase onto `main` 2a0fffb (the soak files carry
  be7e74c); the per-slab lock hold of section 6c is c66048b. Base f2d9558 (`main` with lanes B, C, E
  merged).
- what moved on the default path: `plan_locked` tracks where the fresh-slab picks start and,
  when the key is on, tests the idle count and the picks' states after the lock's work; off the
  key that is one relaxed load per refill that reached `next_slab` (never on the partial-slab
  path); `next_slab` is the same selection in two functions (`next_fresh_slab` is the group /
  adoption / fresh-group half). The pop, push and flush are byte-identical in source; the pass
  itself runs on the background thread.
- VMs: the reference pair, `challoc-bench-arm64` (c4a-standard-16, Axion / Neoverse V2) and
  `challoc-bench-amd64` (c4-standard-16, Xeon 6), `us-east4-a`, Ubuntu 24.04, kernel
  `7.0.0-1011-gcp`, 4 KiB pages, `populate_write=yes`; the dev container (Wolfi, glibc 2.44).
  Nobody else used the pair during these runs.

## 1. `quick`, the opt-in against the thread alone, the merged base (go, both archs)

Before any hook: the merged library, `challoc-bench quick <arch> OLD_CONF=background_thread:true
NEW_CONF=prefault:true,background_thread:true` (both sides with the thread, so the row prices lane
B's pass and not the thread; one round each; the driver noted "old and new are the same build").

aarch64 (load 3.11 at start, 127 s):

| row | unit | old (thread) | new (thread + prefault) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.9 (15.5 MiB) | 41.9 (19.5 MiB) | 45.1 (18.2 MiB) | +0.0% | +7.6% |
| larson10 | M ops/s | 360.5 (111.2 MiB) | 362.8 (136.7 MiB) | 384.4 (131.6 MiB) | -0.6% | +6.0% |
| xmalloc | M frees/s | 582.911 (52.8 MiB) | 584.155 (59.3 MiB) | 482.474 (80.5 MiB) | -0.2% | -17.4% |
| alloc-test1 | s | 2.657 (13.0 MiB) | 2.646 (15.0 MiB) | 2.501 (12.3 MiB) | -0.4% | +5.8% |
| valkey1 | M ops/s | 7.293 (584.3 MiB) | 7.407 (585.9 MiB) | 7.407 (585.4 MiB) | -1.5% | +0.0% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

x86_64 (load 3.22 at start, 129 s):

| row | unit | old (thread) | new (thread + prefault) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 44.1 (15.6 MiB) | 45.4 (19.5 MiB) | 45.5 (18.0 MiB) | -2.9% | +0.2% |
| larson10 | M ops/s | 287.2 (95.0 MiB) | 290.5 (122.4 MiB) | 283.8 (91.4 MiB) | -1.1% | -2.3% |
| xmalloc | M frees/s | 327.521 (52.8 MiB) | 339.362 (53.8 MiB) | 334.050 (92.8 MiB) | -3.5% | -1.6% |
| alloc-test1 | s | 3.268 (13.8 MiB) | 3.283 (15.7 MiB) | 3.073 (13.1 MiB) | +0.5% | +6.8% |
| valkey1 | M ops/s | 14.389 (585.4 MiB) | 14.206 (587.0 MiB) | 13.918 (591.8 MiB) | +1.3% | -2.0% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6666 (49.9%) | new compacts | |

`quick: go` on both. Positive = new worse. The same reading as lane B's: the opt-in costs RSS on
the fixed-working-set rows (one prefaulted slab per `(arena, class)` that refilled: larson1 +4
MiB, larson10 +25..27 MiB over ten arenas), not time; the x86_64 one-round spread on larson1 and
xmalloc is about 3% either way with identical binaries, which sets the noise floor for section 4.

## 2. The claim-side review (task 2a)

What the refill does with the prefaulted slab, read against lane B's decision (PROGRESS.md,
"what a committed-but-unclaimed prefaulted slab is to `next_slab`"):

- `next_slab` takes the lowest `Empty` slab, else the lowest `Purged` one, before committing
  anything: the prefaulted slab (published `Purged`, `dirty_at` 0) is taken by the next refill of
  the class with no claim-code change. When a truly purged (`MADV_DONTNEED`) slab is in the set
  too, the lower address wins, which may be the non-resident one: the two are indistinguishable
  without a descriptor bit (recorded, not fixed; the pass does not prefault while the set is
  non-empty, so the case needs a release-and-purge after a prefault).
- `take` reports the pick zero when `state.is_zero() && (state != Purged || purged_is_zero())`:
  a prefaulted slab is zero (fresh anonymous pages written with `MADV_POPULATE_WRITE`, or a zero
  over a zero by the touch fallback), so `calloc` gets it as known zero under the default and, under
  `lazy_free` (`purged_is_zero` false), as dirty: a `memset` on resident pages, a cost, not an
  error. `claim_prefix` decides the zero slots under the lock as for any `Fresh`/`Purged` pick;
  `finish`'s `check_zero` mode verifies them.
- the compaction claim (`pop_compact` in `heap.rs`, `claim_random` under `slot_randomize`) uses
  the same `plan`: partial slabs first, `next_slab` only when the class has no room, so it reaches
  a prefaulted slab exactly when a refill would; it calls `note_compact`, not `note_refill`, so a
  class in compact mode is never prefaulted, which is right (compaction fills holes).
- the decay walks read the `empty` set only, so the pages stay resident until the refill. One
  exception: with sub-page slabs (`slabs_per_page > 1`, 64 KiB pages) `purge_group` discards a
  whole page group when every slab in it is `Fresh`/`Empty`/`Purged`, a prefaulted neighbour
  included: the prefault is undone, the memory stays zero.
- `transfer_group` moves a prefaulted slab as any `Purged` one (its consistency check `Purged` <=>
  in the donor's `purged` set holds); `next_idle_group` sees it as idle. `stats.purged` counts it
  as purged until it is taken (lane B's accepted inaccuracy).

## 3. The hooks, attributed on the fill burst (task 2b)

The burst probe (`.tmp-burst.sh` on the arm64 VM, in the dev container): a fresh `valkey-server`
(the challoc build, `--activedefrag yes`, as the soak starts it), the soak's first write run
(`valkey-benchmark -t set -d 512 -r 500000 -n 5000000 -P 16 -c 50`: the 500 k-key, 350 MiB fill
in about six seconds), the main thread's `minflt` from `/proc/<pid>/task/<pid>/stat` before and
after, `challoc-bg`'s `minflt`, VmHWM and the `Prefault:` line of `MEMORY MALLOC-STATS`. The
variants are quantum8 builds of the same tree with one constant changed, swapped in at the
server's rpath: `base` = lane B's pass (the merged library rebuilt), `wake` = the `plan` wake
with `MAX_AHEAD` 1, `ahead` = the pipeline with the wake compiled out, `hooks` = both (the
commit). Three interleaved rounds; `default` = no key (Valkey turns the thread on itself).

| variant | main `minflt` over the fill | `challoc-bg` `minflt` | vs default | passes (active) | slabs prefaulted | wakes / taken | rps | p99 ms | VmHWM MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| default (no key), merged lib | 95290 / 95289 / 95293 / 95290 / 95292 | 2 | | | | | 908..923 k | 1.263..1.295 | 353.7 |
| default (no key), hooks lib | 95288 | 2 | 0 | | | | 923 k | 1.287 | 353.7 |
| base: lane B's pass | 60050 / 59210 / 59818 | 34943 / 35781 / 35173 | -37% | 12613 (7842) | 7472 (142 MB) | 12062 / - | 907..912 k | 1.279..1.319 | 357.9 |
| wake alone | 59488 / 59708 / 59038 | 35551 / 35331 / 36001 | -38% | 14141 (8322) | 7608 | 12393 / 7572 | 905..909 k | 1.287..1.335 | 358.0 |
| pipeline alone | 24002 / 23806 / 23980 | 71109 / 71303 / 71130 | -75% | 4053 (3294) | 16481 (290 MB) | 3215 / 0 | 920..924 k | 1.255..1.263 | 358.3 |
| both (be7e74c) | 16482 / 16828 / 16820 | 78674 / 78329 / 78337 | -83% | 4667 (3921) | 18353 (321 MB) | 1706 / 2691 | 918..925 k | 1.247..1.287 | 358.5 |

(The first block of section 4 of lane B's file is the same fill inside the soak: main thread
96.5 k -> 60.6 k at `t=6`, the same -37%.)

Reading. Lane B's pass gets every other slab of a hot class during the fill: 7.5 k slabs
prefaulted over 12.6 k passes, one per active pass, while the classes take about 12 k slabs
(the 95 k default faults are 372 MiB of pages, 5.8 k of them in the pipeline's 64 KiB slabs per
second at the peak). The wake alone changes nothing: with one slab per pass the pass already runs
as often as the commit path wakes it (12.4 k wakes, 14.1 k passes), so waking it once more per
take only adds passes (the `taken` count, 7.6 k, is the number of prefaulted slabs consumed).
The pipeline alone does most of the work (16.5 k slabs, 4.0 k passes: four slabs per active pass,
-75%), and with the wake the class whose pipeline just emptied gets its next pass at once rather
than at the next commit or poll, which is the remaining -8 points (18.4 k slabs; the commit path
wakes 1.7 k times because there are few fresh commits left to wake on; the takes wake 2.7 k
times). The main thread's remaining 16.5 k faults are the huge runs (the dict table, client
buffers) and the pipeline's own start-up per class. The bg thread takes 78.7 k faults, so the
pages are the same pages installed by another thread ahead of the refill: main + bg = 95.2 k
against the default's 95.3 k. Throughput and p99 of the fill move inside their spread (rps
+1.3%, p99 -0.03 ms with both hooks, both within the rounds' range). VmHWM +0.6 MiB over base
(+4.8 MiB over the default: the pipeline's resident slabs ahead of use).

Decision: both hooks kept. The cost is bounded RSS during a burst: at most `MAX_AHEAD` (8) slabs
per `(arena, class)` refilling at that rate, nothing for a class that does not, and the pass
still stops when the class has an `Empty` or `Purged` slab of its own.

## 4. `quick`, the default library against the merged base (the hooks off the key)

`challoc-bench quick <arch> OLD=f2d9558` (old = the merged base built from the rev, new = the
tree at be7e74c, no `MALLOC_CONF`: the hooks compiled in, the key off; one round each). This is
the go/no-go for the one default-path change (the `plan_locked` flag) and the `next_slab` split.

aarch64 (load 2.95 at start, 126 s; builds 72883877099446c8 -> 976c09d33b90d92f):

| row | unit | old (f2d9558) | new (be7e74c) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.7 (16.0 MiB) | 41.9 (15.8 MiB) | 45.1 (19.9 MiB) | -0.5% | +7.6% |
| larson10 | M ops/s | 362.8 (111.4 MiB) | 364.8 (111.8 MiB) | 384.6 (131.3 MiB) | -0.5% | +5.4% |
| xmalloc | M frees/s | 581.378 (53.8 MiB) | 578.421 (53.4 MiB) | 479.714 (79.8 MiB) | +0.5% | -17.1% |
| alloc-test1 | s | 2.673 (13.0 MiB) | 2.664 (13.0 MiB) | 2.493 (12.3 MiB) | -0.3% | +6.9% |
| valkey1 | M ops/s | 7.265 (584.3 MiB) | 7.377 (584.3 MiB) | 7.352 (585.4 MiB) | -1.5% | -0.3% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3461 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

x86_64 (load 2.05 at start, 156 s; builds d5fcb9cf24e3eebc -> 072a3de4516660e7):

| row | unit | old (f2d9558) | new (be7e74c) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 44.6 (16.6 MiB) | 44.9 (16.7 MiB) (median of 3) | 45.5 (15.8 MiB) | -0.7% | +1.3% |
| larson10 | M ops/s | 283.7 (96.7 MiB) | 282.0 (95.6 MiB) | 301.1 (93.2 MiB) | +0.6% | +6.8% |
| xmalloc | M frees/s | 329.671 (52.8 MiB) | 318.272 (52.5 MiB) | 334.424 (94.2 MiB) | +3.6% | +5.1% |
| alloc-test1 | s | 3.288 (13.7 MiB) | 3.283 (13.8 MiB) | 3.069 (12.9 MiB) | -0.2% | +7.0% |
| valkey1 | M ops/s | 14.246 (585.2 MiB) | 13.848 (585.1 MiB) | 13.639 (592.0 MiB) | +2.9% | -1.5% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6668 (49.9%) | new compacts | |

`quick: go` on both (every row within 5%). aarch64 reads every row within 0.5% either way with
valkey1 -1.5%. x86_64 reads xmalloc +3.6% and valkey1 +2.9% in its one round: the same size as
section 1's x86_64 spread with identical binaries (larson1 -2.9%, xmalloc -3.5%, valkey1 +1.3%),
and the added code is one relaxed load per refill that reached `next_slab`, not per operation;
section 4b puts the per-operation counters next to it.

### 4b. Per-operation counters, x86_64 (`challoc-bench quick-perf amd64 OLD=f2d9558 ROWS='larson1 valkey1'`)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old (f2d9558) | 42.19 | 78.2 | 244.5 | 1.586 | 0.002 |
| larson1 | new (be7e74c) | 45.07 | 73.1 | 244.0 | 1.615 | 0.002 |
| larson1 | jemalloc | 43.33 | 75.6 | 200.9 | 2.148 | 0.001 |
| valkey1 | old (f2d9558) | 22.73 | 154.5 | 241.2 | 3.338 | 0.907 |
| valkey1 | new (be7e74c) | 23.33 | 150.9 | 239.4 | 3.308 | 0.905 |
| valkey1 | jemalloc | 26.39 | 133.4 | 154.1 | 3.367 | 0.952 |

The instruction stream per operation is the same to within half an instruction on both rows
(244.5 -> 244.0, 241.2 -> 239.4: the refill's share, smaller if anything since the fresh half of
`next_slab` is its own function), misses unchanged to the second digit, cycles 3..7% lower in
this pair of runs. The x86_64 `quick` deltas of section 4 (xmalloc +3.6%, valkey1 +2.9%) are
therefore the row's one-round spread, not the change: the default library is within noise on
both archs.

## 5. `quick`, the opt-in against the thread alone, with the hooks

`challoc-bench quick <arch> OLD_CONF=background_thread:true NEW_CONF=prefault:true,background_thread:true`
on the tree at be7e74c (the same build both sides; one round each): what the key with both hooks
costs on the fixed-working-set rows.

x86_64 (load 3.11 at start, 128 s):

| row | unit | old (thread) | new (thread + prefault, hooks) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 44.4 (15.9 MiB) | 44.9 (21.4 MiB) | 44.8 (17.5 MiB) | -1.1% | -0.2% |
| larson10 | M ops/s | 287.3 (96.2 MiB) | 277.8 (129.2 MiB) | 278.9 (90.8 MiB) | +3.4% | +0.4% |
| xmalloc | M frees/s | 319.646 (55.5 MiB) | 341.931 (57.2 MiB) | 334.184 (97.4 MiB) | -6.5% | -2.3% |
| alloc-test1 | s | 3.267 (13.8 MiB) | 3.266 (16.1 MiB) | 3.067 (13.0 MiB) | -0.0% | +6.5% |
| valkey1 | M ops/s | 14.490 (585.3 MiB) | 14.499 (587.7 MiB) | 14.541 (591.7 MiB) | -0.1% | +0.3% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6655 (50.0%) | new compacts | |

aarch64 (load 3.17 at start, 125 s):

| row | unit | old (thread) | new (thread + prefault, hooks) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.9 (15.7 MiB) | 42.0 (21.7 MiB) | 45.1 (19.1 MiB) | -0.2% | +7.4% |
| larson10 | M ops/s | 369.6 (112.4 MiB) | 368.3 (146.7 MiB) | 381.4 (131.2 MiB) | +0.4% | +3.6% |
| xmalloc | M frees/s | 579.312 (53.1 MiB) | 581.579 (56.9 MiB) | 483.588 (81.6 MiB) | -0.4% | -16.8% |
| alloc-test1 | s | 2.654 (13.0 MiB) | 2.669 (15.4 MiB) | 2.502 (12.3 MiB) | +0.6% | +6.7% |
| valkey1 | M ops/s | 7.165 (584.3 MiB) | 7.172 (586.4 MiB) | 7.254 (585.6 MiB) | -0.1% | +1.1% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3461 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go` on both. Time within the rows' spread on both archs (aarch64 every row within 0.6%;
x86_64's larson10 +3.4% and xmalloc -6.5% are its one-round spread, section 1). What the pipeline
adds over lane B's one slab is RSS on the rows that burst at start-up: larson1 15.7 -> 21.7 MiB
(+6 MiB against +4 MiB with one slab), larson10 112 -> 147 MiB (+34 MiB over ten arenas against
+25), alloc-test1 13.0 -> 15.4 MiB (+2.4 against +2.0). These rows fill their working set once and
refault nothing afterwards, so the resident slabs ahead of use are pure cost there; the row the
change is for is section 6.

## 6. The Valkey soak, DURATION 1200, default against `prefault:true` (the acceptance)

The protocol run, both reference VMs in parallel (one per VM, nothing else on either): `make
valkey-soak ALLOCS=challoc DURATION=1200 SETTLE=60` twice back to back in the dev container, stock
and with `CHALLOC_CONF=prefault:true,background_thread:true` (Valkey turns the allocator's
background thread on itself at startup, so the stock server has the thread too and the runs differ
by the `prefault` key alone; the key needs `background_thread:true` beside it because the gate is
evaluated at init, PROGRESS.d Notes), the tree at be7e74c, the Valkey-side quantum8 library
rebuilt from it (`make valkey STAGE=build`; the soak script does not rebuild it, see PROGRESS.d).
Write phase 600 s (valkey-benchmark `-t set -d 512 -r 500000 -n 20000000 -c 50 -P 16` alternating
with memtier SET-only 16..1024 B), delete phase 600 s, settle 60 s, activedefrag on. Sidecar on
the VM host (`.tmp-soak-sidecar.sh`, root): `/proc/<pid>/task/<tid>/stat` `minflt` per thread and
VmHWM every 5 s (the CSVs `2026-09-20-<arch>-task11-prefault-soak-{default,prefault}-faults.csv`
beside this file), and `perf stat -I 5000 -e page-faults,minor-faults,major-faults,task-clock,
cycles,instructions,L1-dcache-load-misses,dTLB-load-misses,branch-misses -p <pid>` for the
server's life (the `...-perf.csv` files; a page installed by `MADV_POPULATE_WRITE` is charged to
`challoc-bg`'s `minflt` but is not an exception the main thread takes). The soak's own summary
tables (`...-soak.md`) and sample CSVs are the source of the first block. `t` is seconds from the
server's process start; the write phase ends at about t=610.

### 6a. aarch64 (C4A)

| measure | default | `prefault:true` (be7e74c) | prefault vs default |
|---|---:|---:|---:|
| write: valkey-benchmark req/s (mean of the phase's runs) | 929829 | 910173 | -2.1% |
| write: valkey-benchmark p99 ms (median of runs) | 1.055 | 1.079 | +0.024 (3 steps of 8 us) |
| write: memtier req/s | 958027 | 945549 | -1.3% |
| write: memtier p99 ms | 0.975 | 0.999 | +0.024 |
| delete: valkey-benchmark req/s | 1411080 | 1403722 | -0.5% |
| delete: valkey-benchmark p99 ms | 0.727 | 0.727 | 0 |
| delete: memtier req/s | 1232363 | 1232707 | +0.0% |
| delete: memtier p99 ms | 0.791 | 0.791 | 0 |
| peak RSS MiB (VmHWM) | 578.3 | 582.5 | +0.7% (+4.2 MiB) |
| RSS MiB at end of write phase | 578.0 | 582.2 | +0.7% |
| RSS MiB at end of delete phase | 130.8 | 134.2 | +2.6% (+3.4 MiB) |
| RSS MiB after settle | 130.8 | 134.2 | +2.6% |
| allocator_frag_ratio after settle | 1.81 | 1.80 | |
| active_defrag_hits | 238629 | 233518 | |

| thread, instant | default | `prefault:true` | delta |
|---|---:|---:|---:|
| main (`valkey-server`), first sample (t=2 / t=3, the fill under way) | 81199 | 17694 | |
| main, t=12 (the first valkey-benchmark run has filled the keyspace) | 96612 | 18237 | -81% |
| main, t=303 | 157095 | 26667 | -83% |
| main, end of the write phase (t=614 / t=610) | 157538 | 27047 | **-83%** |
| main, end of the run (t=1301 / t=1307) | 159393 | 28902 | -82% |
| `challoc-bg`, end of the write phase | 2 | 128021 | |
| `challoc-bg`, end of the run | 2 | 128029 | |
| main + `challoc-bg`, end of the run | 159395 | 156931 | -1.5% |
| perf `page-faults` (exceptions), whole process, attached at t=2, through t~612 | 66172 | 9540 | -86% |
| perf `page-faults`, whole process, whole run | 68105 | 11473 | -83% |
| perf `task-clock` ms, whole process, whole run | 1238152 | 1244823 | +0.5% |

### 6b. x86_64 (C4, Xeon 6 8581C)

| measure | default | `prefault:true` (be7e74c) | prefault vs default |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 769744 | 765462 | -0.6% |
| write: valkey-benchmark p99 ms | 1.239 | 1.231 | -0.008 |
| write: memtier req/s | 946081 | 944402 | -0.2% |
| write: memtier p99 ms | 0.967 | 0.975 | +0.008 |
| delete: valkey-benchmark req/s | 1360088 | 1355158 | -0.4% |
| delete: valkey-benchmark p99 ms | 0.727 | 0.727 | 0 |
| delete: memtier req/s | 1231992 | 1226150 | -0.5% |
| delete: memtier p99 ms | 0.783 | 0.791 | +0.008 |
| peak RSS MiB (VmHWM) | 579.0 | 583.6 | +0.8% (+4.6 MiB) |
| RSS MiB at end of write phase | 578.7 | 583.6 | +0.8% |
| RSS MiB at end of delete phase | 131.3 | 136.1 | +3.7% (+4.8 MiB) |
| RSS MiB after settle | 131.3 | 136.1 | +3.7% |
| allocator_frag_ratio after settle | 1.80 | 1.80 | |
| active_defrag_hits | 249848 | 248634 | |

| thread, instant | default | `prefault:true` | delta |
|---|---:|---:|---:|
| main, first sample (t=2) | 74499 | 11897 | |
| main, t=12 | 95201 | 17464 | -82% |
| main, t=303 | 152896 | 25835 | -83% |
| main, end of the write phase (t=614 / t=610) | 153227 | 26162 | **-83%** |
| main, end of the run (t=1312 / t=1262) | 155066 | 27955 | -82% |
| `challoc-bg`, end of the write phase | 2 | 128547 | |
| `challoc-bg`, end of the run | 2 | 128555 | |
| main + `challoc-bg`, end of the run | 155068 | 156510 | +0.9% |
| perf `page-faults`, whole process, attached at t=2, through t~612 | 78146 | 14431 | -82% |
| perf `page-faults`, whole process, whole run | 80012 | 16252 | -80% |
| perf `task-clock` ms, whole process, whole run | 1241695 | 1199725 | -3.4% |

Reading. The plan's two measures: the main thread's page faults over the write phase fall by 83%
on both architectures (lane B's one slab per pass read -39..-43% on the same phase; the pipeline
and the wake take the fill burst's slabs too), the process's fault exceptions by 82..86%, and the
sum of the two threads' faults is within 1.5% of the default's, so the pages are the same pages
installed by `challoc-bg` ahead of the refill. The write-phase p99 is flat on x86_64 (-0.008 /
+0.008 ms, one histogram step each way) and +0.024 ms (3 steps of 8 us) on aarch64 with
throughput -2.1% (valkey-benchmark) / -1.3% (memtier) there, against -0.6% / -0.2% on x86_64 and
lane B's -0.1% / +0.3% (DURATION 600, one slab per pass). One run per side, so the aarch64 write
rows are inside the spread one soak can show (lane B's default read 925268 req/s and 1.063 ms
where this one read 929829 and 1.055), but they are also the direction a longer class-lock hold
would give: the pipeline populates up to eight slabs under one hold of the class lock while the
main thread's refill of that class waits. be7e74c's follow-up commit takes the lock once per slab
(`prefault_ahead`), which bounds the hold to one slab's populate, lane B's hold; section 6c
re-checks the aarch64 write phase with it (DURATION 600). The delete phase is identical both
ways on both archs (the pass has nothing to do while classes hold idle slabs). RSS: +4.2..4.6 MiB
at the peak (+0.7..0.8%: the pipeline's resident slabs ahead of use) and +3.4..4.8 MiB after the
settle (+2.6..3.7%: prefaulted slabs the delete phase never took stay resident until a refill
does; the decay walks do not purge the `purged` set).

### 6c. aarch64 re-check with the per-slab lock hold (c66048b), DURATION 600

The same pair on the C4A with the tree at c66048b (`prefault_ahead` takes the class lock once per
slab), the Valkey-side library rebuilt from it, `DURATION=600 SETTLE=60` (the 10-minute shape, as
lane B's first reading; write phase 300 s), the same sidecar (the CSVs `2026-09-20-aarch64-task11-
prefault-soak600-*` beside this file):

| measure | default | `prefault:true` (c66048b) | prefault vs default |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 928630 | 929971 | +0.1% |
| write: valkey-benchmark p99 ms (median of 6; per run 1.063/1.039/1.063/1.063/1.071/1.063 vs 1.063/1.039/1.063/1.063/1.055/1.063) | 1.063 | 1.063 | 0 |
| write: memtier req/s | 964821 | 967478 | +0.3% |
| write: memtier p99 ms | 0.959 | 0.959 | 0 |
| delete: valkey-benchmark req/s | 1416298 | 1425669 | +0.7% |
| delete: valkey-benchmark p99 ms | 0.719 | 0.711 | -0.008 |
| delete: memtier req/s | 1248957 | 1258953 | +0.8% |
| delete: memtier p99 ms | 0.775 | 0.767 | -0.008 |
| peak RSS MiB (VmHWM) | 579.1 | 584.8 | +1.0% (+5.7 MiB) |
| RSS MiB after settle | 129.8 | 135.4 | +4.3% (+5.6 MiB) |
| main `minflt`, end of the write phase (t=614) | 158812 | 27850 | **-82%** |
| main `minflt`, end of the run | 158812 | 27850 | -82% |
| `challoc-bg` `minflt`, end of the run | 2 | 128852 | |
| perf `page-faults`, whole process, through t~612 | 75831 | 15251 | -80% |

With the hold bounded to one slab's populate the aarch64 write phase reads the same both ways
(throughput +0.1% / +0.3%, p99 equal to the histogram step on both tools, run by run), which is
what x86_64 read in 6b and lane B in its first reading; the -2.1% / +0.024 ms of 6a was the
eight-slab hold, or the run's spread, and either way it is not there with c66048b. The fault
reduction is unchanged (-82%). RSS is the cost, as before.

## 7. Tests

- `make test` on both reference VMs at 97ad715 (the rseq test, before the hooks): 408 + 120 + 23
  passed, `EXIT 0`, every container row `verdict=ok`, on the C4A and the C4; the hardened rseq
  test read `16 distinct mm_cid values over 16 threads pinned to CPUs [0..15] of 16 possible`
  on both (255 820 / 249 854 reads).
- the Mac's dev container (Docker VM, linuxkit, no `mm_cid`) at be7e74c: `make test` 409 + 120 +
  23 passed, `EXIT 0`, every container row `ok` (`prefault` on: `16 of 16 pages resident ...
  pipeline 2 slabs`, `taken 3`; off and inert 0 of 16).
- the rseq test under the Chainguard OS cell's shape on the x86_64 VM (`--cpuset-cpus=0-7`, 16
  busy loops on the same CPUs, the whole unit-test binary as `make test` runs it): 6 of 6 runs
  `8 distinct mm_cid values over 8 threads pinned to CPUs [0..7]`, 408 passed each.
- `make test` on both reference VMs at the final tree (c66048b: the hooks, the per-slab lock, the
  PAC test and `push_batch` fixes of tasks 7 and 8): 410 + 120 + 23 passed, `EXIT 0`, every
  container row `verdict=ok`, on the C4A and the C4. On the C4A the new
  `heap::tests::percpu_batch_onto_a_non_empty_stack_authenticates` ran the sections on the mode
  (`pac=true ... 1 + 3 pushed, 4 taken, 4 of them the fakes (pacda/autda authenticated)` at
  0823a00; its first run at 11155f3 had the key off), `c_tls_threads_with_pac_pointers` both ways
  and `c_percpu_both_ways` with the key passed, and the two-key C programs were then run by hand
  (section 7b).

### 7b. The two-key C programs by hand (tasks 7 and 8, lane D's rows 2a and 4a)

`tests/c/percpu.c` and `tests/c/tls_threads.c` compiled as the drivers compile them, run against
the tree's release library with `LD_PRELOAD`, on the VM host's container after `make test`:

| program, `MALLOC_CONF=abort_conf:true,...` | C4A (aarch64, FEAT_FPAC) | C4 (x86_64, PAC compiled out) |
|---|---|---|
| `percpu percpu`, `percpu_arena:percpu,pac_pointers:true` | rc=0 `ok percpu` 5/5 | rc=0 `ok percpu` 5/5 |
| `percpu percpu`, `pac_pointers:true,percpu_arena:percpu` | rc=0 5/5 | rc=0 5/5 |
| `percpu percpu`, `percpu_arena:percpu` | rc=0 5/5 | rc=0 5/5 |
| `tls_threads`, `pac_pointers:true,percpu_arena:percpu` | rc=0 `ok 64 threads, distinct heaps` 6/6 | rc=0 `ok 64 threads, distinct heaps` 6/6 |
| `tls_threads`, `pac_pointers:true` | rc=0 6/6 | rc=0 6/6 |

(Lane D's table read the first two rows 132, 132 and 0, 132, 0 / 132, 132, 0 on the C4A before the
fix. A fifth row of the script, `percpu percpu` under `pac_pointers:true` alone, reads rc=1 on
both hosts by construction: the program asserts the mode it was told, and without the key the
mode is `auto`, thread caches; not a fault.)
- the unit test of the hooks: `class::tests::prefault_ahead_keeps_the_burst_rate_ready_and_the_
  last_take_wakes` (the delta, a pipeline of three, `Empty` counted as idle, the wake flag on the
  last idle slab only, off the key nothing); lane B's `prefault_next_*` tests unchanged and green.

## 8. For the human: the `prefault` default

The mechanism works as designed and its cost is known:

- what it buys: on the Valkey soak's write phase the main thread takes 83% fewer page faults
  on both architectures (about 130 k faults of a few microseconds each moved to `challoc-bg`
  over a 350 MiB fill), the process 82..86% fewer fault exceptions; on the fill burst itself
  (section 3) the main thread's faults fall from 95 k to 16.5 k. The p99 does not move at the
  tools' resolution on x86_64; on aarch64 the one DURATION 1200 run read +0.024 ms and -2.1%
  valkey-benchmark throughput (section 6a), inside a soak's spread but in the direction a long
  lock hold gives, which c66048b removes (section 6c is the re-check). The delete phase and the
  settle are identical either way.
- what it costs: RSS. One pipeline of resident slabs per `(arena, class)` that refills: +4.2..4.6
  MiB (+0.7..0.8%) at Valkey's 580 MiB peak, +3.4..4.8 MiB (+2.6..3.7%) after the settle, +4..6
  MiB on a 16 MiB larson heap, +25..34 MiB over ten arenas on larson10 (section 5: the rows that
  fill once and refault nothing). And the background thread: `prefault:true` is inert without
  `background_thread:true`, so a default `prefault:true` is also a default background thread,
  which the default configuration does not run today and the `quick` rows do not ask for (Valkey
  and Ruby turn it on themselves).
- the recommendation: keep `prefault` an opt-in (`prefault:true,background_thread:true`) and
  document it for Valkey-shaped services (a fill burst on a latency-sensitive main thread), rather
  than a default that would turn the background thread on for every process and pay the RSS on
  the small heaps; revisit if the background thread becomes a default on its own merits (then the
  pass is a few microseconds per millisecond of refilling and the RSS is the only cost). The
  numbers to weigh are sections 3, 5 and 6; nothing here changes a default.
