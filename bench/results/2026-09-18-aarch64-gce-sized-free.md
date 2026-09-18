# challoc sized frees at jemalloc parity, 2026-09-18, aarch64 (GCE C4A)

Phase 1.5 task 4e (`docs/plan.md`), the fix task 4d's attribution supports
(`2026-09-18-aarch64-gce-valkey-throughput.md`: 200 of Valkey's 273 extra
cycles per SET are the exact `free` check's slab-descriptor bitmap-word load
on `je_sdallocx`, a cache miss per free on a 550 MiB heap; the human chose
jemalloc parity on 2026-09-18, PROGRESS.md notes).

Mechanism attacked: the descriptor line load in the free path of
`sdallocx`. `sdallocx`/`free_sized`/`free_aligned_sized` now verify the size
hint against the pointer's class arithmetically (`class_for(size, align)`
must equal the class the address falls in; a mismatch aborts) and push onto
the home cache without touching the slab descriptor (neither the bitmap word
nor the arena). The flush's `release` validates every bit it clears, so an
invalid or double free through the sized path aborts there, up to a cache's
worth of frees later; the batch is routed to each slab's owning arena
(`Shared::release_routed` over `ClassState::release_owned`). Plain `free`
and `dallocx` are unchanged. Working tree of iteration 24 on top of c258815
(the code commit's hash is in PROGRESS.md; `quick` lists the library hashes).

- hosts: challoc-bench-arm64 (c4a-standard-16, Google Axion / Neoverse V2)
  and challoc-bench-amd64 (c4-standard-16, Xeon Platinum 8581C), us-east4-a,
  Ubuntu 24.04 kernel 7.0.0-1011-gcp, perf 7.0.14 on the host; container
  Wolfi (rustc 1.98.1). Both VMs a warm session; load 2..3 at every start
  (the VMs' own background, checked with `uptime` and `docker ps`).
- tests: `make test` 223 + 79 on both VMs (three new negative modes:
  `sized_double_free` returns from the second `sdallocx` and aborts at the
  flush with `double free of 0x...`; `sized_mismatch` and
  `free_sized_mismatch` abort at the call with `size hint N does not match
  the allocation`); host 169.

## 1. `quick` (go/no-go, one round, working tree against HEAD c258815)

aarch64 (`quick: go`, 119 s, old a5153f74425bf587, new a84da39114482df0):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 43.1 (16.5 MiB) | 42.8 (16.4 MiB) | 45.0 (18.1 MiB) | +0.7% | +5.1% |
| larson10 | M ops/s | 372.3 (116.8 MiB) | 373.3 (117.5 MiB) | 385.2 (131.5 MiB) | -0.3% | +3.2% |
| xmalloc | M frees/s | 571.743 (82.1 MiB) | 566.871 (84.7 MiB) | 478.322 (77.0 MiB) | +0.9% | -15.6% |
| alloc-test1 | s | 2.656 (13.0 MiB) | 2.656 (13.0 MiB) | 2.552 (12.3 MiB) | +0.0% | +4.1% |
| valkey1 | M ops/s | 6.529 (586.7 MiB) | 7.053 (586.8 MiB) | 7.731 (585.5 MiB) | -7.4% | +9.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3461 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

x86_64 (`quick: go`, 119 s, old b48474fe2621feec, new 499124e11a8247eb):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 48.4 (16.0 MiB) | 48.8 (15.8 MiB) | 44.9 (17.8 MiB) | -0.8% | -8.0% |
| larson10 | M ops/s | 270.9 (101.8 MiB) | 285.4 (97.2 MiB) | 300.4 (91.4 MiB) | -5.1% | +5.3% |
| xmalloc | M frees/s | 350.664 (80.8 MiB) | 360.557 (83.4 MiB) | 336.574 (97.0 MiB) | -2.7% | -6.7% |
| alloc-test1 | s | 2.933 (13.6 MiB) | 2.924 (13.8 MiB) | 2.834 (13.1 MiB) | -0.3% | +3.2% |
| valkey1 | M ops/s | 14.150 (587.7 MiB) | 16.191 (587.6 MiB) | 15.653 (592.0 MiB) | -12.6% | -3.3% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6655 (50.0%) | new compacts | |

Positive percentages mean new did worse. The `valkey1` row (frees through
`sdallocx`) is the one the change targets and the only one that moves:
+8.0% throughput on aarch64 (the gap to jemalloc +18.4% -> +9.6%), +14.4%
on x86_64 (now 3.3% ahead of jemalloc). The larson10 x86_64 -5.1% is that
row's usual spread (bimodal lists, see the A/B below).

## 2. `quick-perf`: per-operation counters (first VM run of the tool)

`sudo perf stat -x, -e cycles,instructions,L1-dcache-load-misses,dTLB-load-misses
--inherit -p <container init pid>` over the whole program, divided by its
own operation count; one run each.

aarch64:

| row | variant | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|
| larson1 | old | 15.0 | 51.8 | 0.526 | 0.688 |
| larson1 | new | 15.0 | 51.8 | 0.527 | 0.680 |
| larson1 | jemalloc | 14.9 | 50.3 | 0.463 | 0.378 |
| valkey1 | old | 257.9 | 256.9 | 4.300 | 4.096 |
| valkey1 | new | 230.5 | 227.8 | 3.033 | 2.851 |
| valkey1 | jemalloc | 224.8 | 166.2 | 3.026 | 2.747 |

x86_64:

| row | variant | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|
| larson1 | old | 17.2 | 56.2 | 0.482 | 0.000 |
| larson1 | new | 17.2 | 56.0 | 0.479 | 0.000 |
| larson1 | jemalloc | 17.1 | 47.1 | 0.500 | 0.000 |
| valkey1 | old | 164.1 | 241.6 | 5.200 | 1.092 |
| valkey1 | new | 143.3 | 224.1 | 3.332 | 0.909 |
| valkey1 | jemalloc | 128.6 | 145.9 | 3.333 | 0.952 |

Reading: `valkey1` loses exactly the miss the attribution named. L1D misses
per operation fall 4.30 -> 3.03 (aarch64) and 5.20 -> 3.33 (x86_64), in
both cases to jemalloc's figure to the second decimal, and dTLB misses fall
with them (the descriptor page per free; the residue against jemalloc on
aarch64 is task 4f's, the 52 descriptor pages plain `free` still touches).
Cycles per operation 258 -> 231 against jemalloc's 225 on aarch64, 164 ->
143 against 129 on x86_64; the rest of the x86_64 gap is instructions (224
against 146 per operation, `valkey_model`'s `malloc` + `malloc_usable_size`
+ `sdallocx` per key: challoc runs about 80 more instructions there, none of
them a miss). larson1 does not free through `sdallocx` and is unchanged to
the tenth of a cycle on both VMs. (The tool's `M ops/s` column for larson1
divides larson's own operation count by the container's wall time and is not
meaningful; the per-operation columns are.)

## 3. A/B protocol rows, aarch64 (5 rounds, same run; harness output)

Reading: `valkey1` (frees through `sdallocx`) 6.471 -> 7.070 M ops/s
(+9.3%; jemalloc 7.677, so the gap on this row goes from +18.6% to +8.6%
behind), `valkey1-free` (plain `free`) 6.733 -> 6.757 (unchanged, as
expected: plain `free` keeps its check), larson1 42.9 -> 42.9, larson10
366.0 -> 370.3 and larson16 589.9 -> 599.8 (lists overlap; larson does not
call `sdallocx`), xmalloc -0.7%, alloc-test1 +0.3%, alloc-testN -0.5%, RSS
unchanged on every row. Under jemalloc the sized row is faster than the
plain one (7.677 against 7.253); under challoc it now is too (7.070 against
6.757) where before it was slower (6.471 against 6.733): the descriptor
miss made the sized path the more expensive of the two. The x86_64 rows are
in `2026-09-18-x86_64-gce-sized-free.md` (`valkey1` 14.219 -> 15.930, now
0.7% ahead of jemalloc; `valkey1-free` unchanged).

- commit: c258815 (uncommitted changes in src/; the tree is what 27de567 committed, variant `new`)
- date: 2026-09-18T03:59:06Z
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp (host kernel), container: Wolfi
- mimalloc-bench 06a4c4f99971df2f0ba7bc36cab7d9706e515bd7
- jemalloc-5.3.1-r3
- rust-1.98-1.98.1-r0
- load average at start: 0.55 1.47 1.60; at end: 5.03 5.48 4.23; 935 s total
- method: 5 timed rounds after one discarded warm-up, variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than old.
- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n 850000 -t 10 (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds
- variants:
  - old: c2588157b55e built into target/ab/c2588157b55e
  - new: working tree (HEAD c258815 + uncommitted src changes) built into target/ab/tree
  - jemalloc: Wolfi's /usr/lib/libjemalloc.so.2 (5.3.1)

## Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.8 42.9 42.9 43.0 43.0 | 16936 |
| larson1 | new | 42.8 42.9 42.9 42.9 43.0 | 16948 |
| larson1 | jemalloc | 45.1 45.1 45.1 45.1 45.2 | 18832 |
| larson10 | old | 365.5 366.0 366.0 373.7 374.2 | 119856 |
| larson10 | new | 369.3 369.4 370.3 370.4 372.6 | 121764 |
| larson10 | jemalloc | 376.8 383.1 383.6 384.9 387.7 | 134924 |
| larson16 | old | 586.6 588.0 589.9 593.1 598.2 | 185136 |
| larson16 | new | 595.5 598.7 599.8 600.1 606.1 | 185012 |
| larson16 | jemalloc | 608.1 613.7 614.1 615.1 617.6 | 211660 |
| xmalloc | old | 568.555 569.553 571.448 572.317 572.404 | 84008 |
| xmalloc | new | 567.044 567.202 567.305 567.364 568.488 | 84912 |
| xmalloc | jemalloc | 473.707 481.969 483.311 484.359 487.312 | 82048 |
| alloc-test1 | old | 2.660 2.661 2.662 2.664 2.666 | 13308 |
| alloc-test1 | new | 2.656 2.658 2.670 2.678 2.689 | 13312 |
| alloc-test1 | jemalloc | 2.544 2.544 2.547 2.547 2.564 | 12636 |
| alloc-testN | old | 2.263 2.265 2.272 2.272 2.275 | 21100 |
| alloc-testN | new | 2.258 2.260 2.261 2.263 2.263 | 21040 |
| alloc-testN | jemalloc | 2.204 2.207 2.214 2.214 2.220 | 16856 |
| valkey1 | old | 6.246 6.398 6.471 6.519 6.521 | 601140 |
| valkey1 | new | 6.906 7.039 7.070 7.074 7.076 | 601268 |
| valkey1 | jemalloc | 7.433 7.674 7.677 7.720 7.754 | 599764 |
| valkey1-free | old | 6.672 6.688 6.733 6.751 6.757 | 601204 |
| valkey1-free | new | 6.743 6.750 6.757 6.764 6.768 | 601208 |
| valkey1-free | jemalloc | 7.233 7.245 7.253 7.264 7.269 | 599764 |

## Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.9 | 16.5 | 42.9 | 16.6 | 45.1 | 18.4 | +0.0% | -4.9% |
| larson10 | M ops/s | 366.0 | 117.0 | 370.3 | 118.9 | 383.6 | 131.8 | -1.2% | -4.6% |
| larson16 | M ops/s | 589.9 | 180.8 | 599.8 | 180.7 | 614.1 | 206.7 | -1.7% | -3.9% |
| xmalloc | M frees/s | 571.448 | 82.0 | 567.305 | 82.9 | 483.311 | 80.1 | +0.7% | +18.2% |
| alloc-test1 | s | 2.662 | 13.0 | 2.670 | 13.0 | 2.547 | 12.3 | +0.3% | -4.3% |
| alloc-testN | s | 2.272 | 20.6 | 2.261 | 20.5 | 2.214 | 16.5 | -0.5% | -2.6% |
| valkey1 | M ops/s | 6.471 | 587.1 | 7.070 | 587.2 | 7.677 | 585.7 | -8.5% | -15.7% |
| valkey1-free | M ops/s | 6.733 | 587.1 | 6.757 | 587.1 | 7.253 | 585.7 | -0.4% | -7.2% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

## 4. The 20-minute Valkey soak with the perf sidecar (acceptance run)

`make valkey-soak` (the full protocol: `ALLOCS="challoc jemalloc"`,
`DURATION=1200`, 600 s write phase then 600 s delete phase, `SETTLE=60`),
Valkey 9.1.2 rebuilt by `make valkey STAGE=build` against the tree's quantum8
`libchalloc.so` (the code of commit 27de567) and the same tag with bundled
jemalloc 5.3.0, on the arm64 VM, 04:21..05:04 UTC, with
`bench/probes/valkey-perf-sidecar.sh ~/logs/4e-soak 700 1205` on the VM host
(`perf stat -I 1000` over each server's life, 60 s `perf record` at t=100
and t=700, snapshots of `INFO commandstats`/`INFO stats`/per-thread CPU
around each window). Per-command figures below divide the `perf stat` rows
between two snapshots by that window's `total_commands_processed` delta
(the sidecar's perf start is taken as t0 + 2 s), the method of task 4d's
"window" rows. The soak summary is in
`2026-09-18-aarch64-gce-sized-free-soak-valkey-soak.md` (with the two
`-challoc.csv` / `-jemalloc.csv` sample files) next to this file.

### 4.1 Cycles per command (the plan's tie-breaker)

| window | alloc | commands/s | cyc/cmd | ins/cmd | L1D miss/cmd | dTLB miss/cmd | main thread |
|---|---|---:|---:|---:|---:|---:|---:|
| write, valkey-benchmark, t=30..100 | challoc | 921k | 3165 | 9902 | 42.8 | 53.6 | 100.4% |
| write, valkey-benchmark, t=30..100 | jemalloc | 919k | 3170 | 9691 | 39.8 | 38.5 | 100.4% |
| write, under `perf record`, t=100..162 | challoc | 715k | 3430 | 10135 | 47.2 | 58.2 | 98.9% |
| write, under `perf record`, t=100..161 | jemalloc | 728k | 3343 | 9729 | 42.7 | 43.0 | 100.5% |
| delete, under `perf record`, t=700..761 | challoc | 1047k | 2330 | 7453 | 25.0 | 26.4 | 100.5% |
| delete, under `perf record`, t=700..761 | jemalloc | 1093k | 2237 | 7304 | 24.7 | 23.9 | 100.5% |

Against task 4d (same method, `2026-09-18-aarch64-gce-valkey-throughput.md`
run 1 window rows: write 3704 against 3431, delete 2417 against 2268; task
5's challoc-only soak: write 3611): the write window goes from +8.0% to
+2.6% per command and the delete window from +6.6% to +4.2%. The first 70 s
of the write phase (valkey-benchmark's first run, no `perf record`
attached) are at parity: 3165 against 3170 cycles per command, 921k against
919k commands per second. Both servers are main-thread bound in every
window (98.9..100.5% of one CPU), so throughput is still cycles per command.
SET server time (`INFO commandstats`, write window): 0.20 us per call
against jemalloc's 0.19 (task 4d: 0.323 against 0.223).

### 4.2 Soak summary (`2026-09-18-aarch64-gce-sized-free-soak-valkey-soak.md`)

| measure | challoc | jemalloc | challoc vs jemalloc | task 4d run 1 | task 4d run 2 |
|---|---:|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 861831 | 860585 | -0.1% | +11.1% | +8.9% |
| write: valkey-benchmark p99 ms | 1.127 | 1.127 | +0.0% | +11.4% | +9.2% |
| write: memtier req/s | 915042 | 908176 | -0.8% | +4.3% | +3.1% |
| write: memtier p99 ms | 1.007 | 1.023 | -1.6% | +3.1% | +3.1% |
| delete: valkey-benchmark req/s | 1383823 | 1436061 | +3.8% | +6.3% | +5.5% |
| delete: valkey-benchmark p99 ms | 0.727 | 0.695 | +4.6% | +5.8% | +4.6% |
| delete: memtier req/s | 1206980 | 1250917 | +3.6% | +7.3% | +2.7% |
| delete: memtier p99 ms | 0.783 | 0.759 | +3.2% | +6.3% | +2.1% |
| peak RSS MiB (VmHWM) | 578.0 | 574.5 | +0.6% | -0.1% | +0.4% |
| RSS MiB after settle | 127.7 | 130.5 | -2.1% | -6.9% | -2.5% |

Positive means challoc did worse. The write phase, the target of this task,
is at parity with bundled jemalloc on throughput and p99 (both generators);
the delete phase is 3.6..3.8% behind on throughput and 3.2..4.6% on p99,
which the tie-breaker confirms is real (+4.2% cycles per command in the
delete window, outside the generators' 3..5% run-to-run noise only on the
counter, not on the throughput rows). Peak RSS +0.6% and RSS after settle
-2.1% are within the Phase 1 criteria as before.

### 4.3 Where the cycles land now (`perf report` on the live pid)

DSO shares of the 60 s records:

| phase | alloc | valkey-server (own code) | allocator | kernel | libc |
|---|---|---:|---:|---:|---:|
| write | challoc | 44.52% | 18.34% (`libchalloc.so`) | 32.58% | 4.55% |
| write | jemalloc | 60.37% (incl. `je_*`, about 13.5%) | | 33.28% | 6.34% |
| delete | challoc | 43.21% | 13.28% | 38.17% | 5.32% |
| delete | jemalloc | 54.49% (incl. `je_*`, about 8.4%) | | 38.23% | 7.26% |

Symbol shares of all server samples, and in cycles per command (share times
the window's cycles per command):

| phase | symbol | challoc | jemalloc | challoc cyc/cmd | jemalloc cyc/cmd |
|---|---|---:|---:|---:|---:|
| write | free path: `je_sdallocx` (challoc: the sized path inlined; `arena::free` is gone) | 2.31% | 1.97% | 79 | 66 |
| write | usable-size query: `<SlabTable>::check` + `api::libc::usable_size` / `je_malloc_usable_size` | 11.16% + 3.19% | 9.78% | 492 | 327 |
| write | `je_malloc` | 1.36% | 1.50% | 47 | 50 |
| delete | free path | 2.16% | 2.19% | 50 | 49 |
| delete | usable-size query | 4.65% + 3.93% | 4.27% | 200 | 96 |
| delete | `je_malloc` | 1.40% | (below 1.4%) | 33 | |

Task 4d had the free path at 338 cycles per SET against jemalloc's 71 and
the usable-size query at 288 against 353. Now the free path is 79 against
66 (the plan's "from 338 toward 71": done, `je_sdallocx` is a size check
and a cache push) and the usable-size query is 492 against 327. The
descriptor miss did not disappear from the SET: it moved. Valkey's
`zfree(ptr)` (`src/zmalloc.c` line 473 of 9.1.2) calls `zmalloc_size(ptr)`
= `je_malloc_usable_size` before `je_sdallocx(ptr, size, 0)`, and that
query runs `SlabTable::check`, the same bitmap-word load on the same cold
descriptor line; in task 4d's run 2 that check hit L1 only because `free`
had just missed on the line. Frees through `zfree_with_size` (sds strings,
objects whose size the caller knows) no longer touch the descriptor at all,
which is the write-phase gain (a SET frees one of each kind). jemalloc pays
its own metadata miss in `je_malloc_usable_size` (the rtree leaf, 58% of
that symbol's cycles on one instruction in task 4d), so the two allocators
now miss in the same place; challoc's query costs more because it misses
twice per `zfree` where the line is cold (the check in `usable_size`, then
nothing in `sdallocx`) against jemalloc's once, and because `check` is
reached through `usable_size` as a call (two symbols).

The remaining gap, +87 cycles per command in the write window and +93 in
the delete window, is therefore the usable-size query's descriptor miss on
`zfree`'s path, plus the +15 dTLB refills per command (58 against 43;
task 4f's descriptor pages) and 4% more instructions. Closing it means
`malloc_usable_size` returning the class size for a slab pointer without
the bit test, the variant task 4d built, gated and reverted for lack of
gain; with the sized free landed it would gain, and it changes what a
use-after-free query does (returns a size instead of aborting), so it is a
decision for the human (PROGRESS.md, notes). 4f makes the same check cheap
for plain `free` and `usable_size` alike without changing what is checked.

### 4.4 Acceptance of task 4e

- `quick` go on both VMs (section 1); `quick-perf` shows the `valkey1`
  row's L1D misses per operation at jemalloc's figure on both (section 2).
- A/B rows on both VMs (section 3 and the x86_64 file): `valkey1` +9.3%
  (aarch64) and +12.0% (x86_64) throughput; `valkey1-free`, larson1/10/16,
  xmalloc, alloc-test1/N unchanged within their spreads; RSS unchanged.
- The 20-minute soak with the sidecar: cycles per SET in the free path 338
  -> 79 against jemalloc's 66..71; write phase throughput and p99 at parity
  (the criterion met for the phase the task targets); delete phase 3.6..3.8%
  behind with the gap attributed above (the usable-size query's descriptor
  miss on Valkey's `zfree`), the tie-breaker 3430 against 3343 (write,
  under `perf record`; 3165 against 3170 without), 2330 against 2237
  (delete).
- Negative tests updated for the deferred detection (`sized_double_free`
  returns from the second call and aborts at the flush).

## 5. Scorecard lines (`bench/results/scorecard-<arch>.md`, 3 rounds, challoc against jemalloc, positive = challoc behind)

| arch | commit | larson1 | larson10 | larson16 | xmalloc | alloc-test1 | alloc-testN | valkey1 | valkey1-free | soak write | soak delete | soak peak RSS | soak post-delete RSS |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| aarch64 | e1419fd | +4.7% | +4.1% | +3.3% | -16.3% | +4.1% | +2.7% | +18.5% | +5.3% | - | - | - | - |
| aarch64 | 27de567 | +5.4% | +4.9% | +3.2% | -15.5% | +3.9% | +2.1% | +10.3% | +7.2% | -0.1% | +3.8% | +0.6% | -2.1% |
| x86_64 | e1419fd | -0.6% | +3.5% | +10.0% | -4.4% | +4.5% | +6.7% | +12.2% | -5.5% | - | - | - | - |
| x86_64 | 27de567 | -3.0% | +6.1% | +6.2% | -5.8% | +3.9% | +4.6% | -0.9% | -10.6% | - | - | - | - |

The `valkey1` column is the one the task targets and the one that moves
(+18.5% -> +10.3%, +12.2% -> -0.9%); `valkey1-free` and the larson columns
move within the 3-round spread in both directions (the 5-round A/B above
has them unchanged). The first aarch64 run of the scorecard at this code
was labelled `c258815+` because the VM's tree was synced before the code
commit; it was discarded and rerun at 27de567 (same library hash
a84da39114482df0); the run's own line is the one recorded.
