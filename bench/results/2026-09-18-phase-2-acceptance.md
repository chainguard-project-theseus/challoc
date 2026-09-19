# Phase 2 acceptance, 2026-09-18, commit 4c7ced6 (code 7087c44)

Every Phase 2 acceptance criterion from `docs/plan.md` (the three listed
plus the 2026-09-18 review's addition "no key in design 12 is inert"), at
the tree 4c7ced6 whose library code is 7087c44 (Phase 2 tasks 2, 1, 4, 5,
6, 7, 3 done in that order). Both GCE VMs (`challoc-bench-arm64`,
c4a-standard-16, Neoverse V2; `challoc-bench-amd64`, c4-standard-16, Xeon
Platinum 8581C; kernel 7.0.0-1011-gcp, Docker 29.1.3, Wolfi dev container),
zone `us-east4-a`, were started, synced, driven and stopped by this
iteration (`CHALLOC_GCE_WARM` not set): `challoc-bench start all` 01:58 UTC
(2026-09-19), `sync all`, one detached chain per VM over direct ssh
(`~/logs/acc-p2/chain.log` on each VM: `make test`, `make bench`, `perf
record` on larson, then the Valkey, soak, Ruby and cgroup stages listed
below), `stop amd64` at 03:01 UTC when its chain ended and `stop arm64` at 03:23 UTC, both `TERMINATED`. Nothing was relaxed.

| # | criterion | aarch64 (C4A) | x86_64 (C4) | verdict |
|---|---|---|---|---|
| 1a | Phase 0: set within 10% of jemalloc on time | worst +3.8% (larson 16 thr, alloc-test1); glibc-simple -9.2%, redis -18.2% | worst +8.7% (larson), alloc-testN +8.6%, glibc-simple +8.5%, alloc-test1 +5.8% | PASS (every row inside on both VMs; section 2) |
| 1a | Phase 0: set within 10% of jemalloc on RSS | mstress +9.8% (318.3 vs 289.8 MiB), alloc-test1 +5.8%, alloc-testN +1.2%, xmalloc-test +0.6% | mstress **+19.0%** (339.6 vs 285.5 MiB), alloc-test1 +4.6%, alloc-testN +1.6%, xmalloc-test -15.2% | PASS with the accepted mstress row (task 15's decision, design 13.3; PROGRESS.md design decisions: "accepted as the price of per-class regions at 16 arenas"); on aarch64 the row is inside on its own this run; every other row inside on both VMs |
| 1a | Phase 0: no lock or atomic RMW in larson's top 10 | `perf report`: outlined LSE helpers at ranks 5, 7, 11, 13 (0.18 / 0.08 / 0.05 / 0.04% of all samples), none in `je_malloc`/`je_free`; 563.8 M ops/s recorded | `perf report`: none in the top 15; 303.6 M ops/s recorded | PASS (x86_64 by rank; aarch64 on substance as settled in Phase 1.5 run 1 and kept in run 4; section 3) |
| 1b | Phase 1: Valkey suite with defrag | `unit/memefficiency` green, 33 `[ok]`, 420 s | `unit/memefficiency` 33 `[ok]` and the full suite: 164/164 units, 5227 `[ok]`, 0 `[err]`, "All tests passed without errors!" (1102 s) | PASS (section 4; the six failures of the 2026-09-17 full run do not recur) |
| 1b | Phase 1: the four Valkey controls | covered by the suite's `unit/memefficiency` (defrag on/off, thresholds) and `INFO memory` in the soak (see 5) | same | PASS |
| 1b | Phase 1: soak criteria (throughput and p99 at least equal, peak RSS within 5%, RSS after settle within 10%) | write ahead (-4.5% / -1.9% req/s, -4.4% / -3.3% p99), delete +1.4% / +1.2% req/s and +1.1% / +0.0% p99 behind (the smallest delete gap of any soak; attributed to the usable-size query in section 5), peak RSS +1.0%, settle +0.3% | not owed (Valkey runs single-arena; the soak is the aarch64 protocol) | PASS (section 5; the delete residue read as in the Phase 1 and 1.5 acceptances) |
| 1b | Phase 1: Ruby `test-all` | 32848 tests, 7 failures + 2 errors: the same nine environmental tests as at 38f1a16 (OpenSSL policy, root, `libc.so-2`), no abort or `challoc:` line | suite at 38f1a16 (`2026-09-17-ruby-test-all.md`, identical set); `ruby-bench` rows this run (section 6): every timed row within its previous band, RSS below jemalloc's on 9 of 10 rows | PASS (the failures are the ones shown unrelated to allocation) |
| 1 | `make test` green with defaults | 310 + 105 + 23 pass, 4 churn rows ok | 310 + 105 + 23 pass, 4 churn rows ok | PASS |
| 2 | default hardening costs no more than 3% on the Phase 0 set relative to Phase 0 results | attributed: +4 instructions per free (flag), +12 per Valkey op (guard test), cycles within noise; 5-round rows 0..1.9%; single-round sums up to 6.3% (valkey1) | attributed the same; 5-round rows 0..1.9% (valkey1 +4.6% `ab` off-state row read as noise by its counters); single-round sums up to 5.9% | PASS under the reading of section 8 (the literal baseline is Mac numbers; flagged for the human) |
| 3 | the two container tests pass; jemalloc's behaviour documented as the baseline | churn 403 -> 13 MiB in 49 ms (512m), 202 -> 8 in 81 ms (256m), `keep` 321 / 161 MiB (this run); oscillation alive 98 cycles, peak 502 MiB, no `memory.events`, low end 368..383 MiB, `stats.cgroup.current` 294 MiB at the end (control alive, 99 cycles, 492 MiB low end); jemalloc baseline task 7's file (190 / 124 MiB kept, killed at cycle 5) | churn 403 -> 13 MiB in 46 ms, 203 -> 8 in 79 ms, `keep` 316 / 162 MiB; oscillation alive 97 cycles, peak 502 MiB, no events, control alive; jemalloc keeps 197 / 127 MiB and is killed at cycle 24, mimalloc returns nothing and is killed at cycle 2 (this run) | PASS on both (section 7; jemalloc baseline on both architectures) |
| + | no key in design 12 is inert (`thp`, `stats_print` the documented exceptions) | `INERT_KEYS == ["thp", "stats_print"]`; every other key has a reader outside `config.rs` (section 9) | same source | PASS |

Every criterion passes on both VMs, with three rows the human should
read: the x86_64 mstress RSS row (+19.0%), accepted by task 15's decision
as before; criterion 2, which cannot be read literally (its baseline is
Mac numbers) and is passed under the reading of section 8, where the
worst-case sums of single-round point estimates exceed 3% on the `valkey1`
row while every attributed and 5-round figure is under 2%; and
`muzzy_decay_ms`, which no code reads and which sits in design 12's
"ignored, accepted for compatibility" class rather than in `INERT_KEYS`
(section 9). The Phase 1 soak's delete phase is +1.2..1.4% behind in
throughput (the smallest gap of any soak), read as the earlier acceptances
read it, with the residue attributed to the usable-size query.

## 1. Driver, tests

`challoc-bench start all` (new external IPs, stale host keys removed with
`ssh-keygen -R`), `sync all` (clean tree at 4c7ced6, no warning), then the
chain on each VM (`~/logs/chain-p2.sh`, shipped by base64, launched with
`setsid nohup` in a subshell over direct ssh; every step's log under
`~/logs/acc-p2/`). `make test` (the release build, `jemalloc-compat`, both
test groups, `test-cgroup-limit` under `--memory=512m`, `test-cgroup-churn`
at both limits):

```
aarch64: 310 passed; 105 passed; 23 passed (cgroup, --memory=512m); churn purge/keep at 512m and 256m: 4 x verdict=ok; rc=0 (56 s)
x86_64:  310 passed; 105 passed; 23 passed (cgroup, --memory=512m); churn purge/keep at 512m and 256m: 4 x verdict=ok; rc=0 (64 s)
```

## 2. Phase 0 set, protocol run (criterion 1, Phase 0 time and RSS)

`make bench HOST_KIND=gce-c4a RESULT=bench/results/2026-09-18-gce-c4a-5.md`
and `... HOST_KIND=gce-c4 RESULT=.../2026-09-18-gce-c4-5.md` (5 runs after
a discarded warm-up, medians, 16 threads for larson / mstress /
xmalloc-test / alloc-testN, jemalloc 5.3.1-r3 in the same run, glibc 2.44,
mimalloc 3.5.2), started 02:03 UTC on both VMs one minute after `make test`
(load 4.26 (1 min) / 1.30 (5 min), `make test` had just ended / 6.59 / 2.01 at the start), nothing else on either VM.
Positive = challoc worse; the run-4b columns are from
`2026-09-18-phase-1.5-acceptance-4.md` (code 2f218f9, the same VMs).

| benchmark | aarch64 time | aarch64 RSS | aarch64 run 4b | x86_64 time | x86_64 RSS | x86_64 run 4b |
|---|---:|---:|---|---:|---:|---|
| cfrac | +0.6% | -43.3% | +0.6% / -43.4% | +4.7% | -38.1% | +3.6% / -35.9% |
| espresso | -2.0% | -40.9% | -2.1% / -40.1% | -0.4% | -32.5% | +0.9% / -34.7% |
| larson (16 thr) | +3.8% | -19.4% | +3.8% / -19.9% | +8.7% | -6.1% | +6.9% / -5.8% |
| mstress | -42.6% | +9.8% | -42.0% / +9.6% | -41.2% | **+19.0%** | -41.3% / +18.4% |
| xmalloc-test | -7.9% | +0.6% | -6.2% / -3.0% | +3.3% | -15.2% | +0.7% / -10.7% |
| alloc-test1 | +3.8% | +5.8% | +5.5% / +4.0% | +5.8% | +4.6% | +6.3% / +5.0% |
| alloc-testN | +2.8% | +1.2% | +3.4% / -1.2% | +8.6% | +1.6% | +8.0% / +1.9% |
| glibc-simple | -9.2% | -48.1% | -4.8% / -47.8% | +8.5% | -46.8% | +7.0% / -46.6% |
| redis | -18.2% | -6.5% | -18.2% / -6.8% | -20.1% | -7.5% | -19.4% / -7.0% |

Absolute RSS (MiB, challoc / jemalloc): mstress 318.3 / 289.8 (aarch64),
339.6 / 285.5 (x86_64); alloc-test1 13.0 / 12.3 and 13.7 / 13.1;
alloc-testN 16.4 / 16.2 and 17.0 / 16.8; xmalloc-test 110.1 / 109.4 and
107.7 / 126.9; larson 166.6 / 206.8 and 119.8 / 127.6. Times (s, challoc /
jemalloc): glibc-simple x86_64 2.218 / 2.044, aarch64 2.292 / 2.523;
alloc-testN x86_64 4.533 / 4.174; larson x86_64 309.4 / 336.2 M ops/s.

Every time row is inside 10% on both VMs; the worst are x86_64's larson
(+8.7%; +6.9% and +7.8% in runs 4b / 4a), alloc-testN (+8.6%) and
glibc-simple (+8.5%, its stationary 6..8% band from acceptance run 4's
section 3.2: raw challoc 2.220 2.256 2.215 2.218 2.179 against jemalloc
2.058 2.044 2.032 2.041 2.060). RSS: mstress, maxrss KiB sorted, five timed
rounds:

| VM | challoc | jemalloc | median MiB | vs jemalloc |
|---|---|---|---:|---:|
| aarch64 | 319516 324244 325940 326292 331712 | 294572 296316 296736 297692 298048 | 318.3 / 289.8 | +9.8% |
| x86_64 | 335968 346340 347744 353016 355320 | 290164 291724 292336 293608 294824 | 339.6 / 285.5 | +19.0% |

The x86_64 row is where task 15 and acceptance run 4 left it (337.9 /
338.7 MiB against 284..285) and is accepted under task 15's decision
(PROGRESS.md design decisions, design 13.3: what remains of the 16-arena
excess is `Active` at the peak, one partial slab per class per arena, the
per-arena metadata and the 64 KiB-slab classes' current groups; attributed
with `arenasample` in `2026-09-18-x86_64-gce-group-handoff.md` and, for
aarch64, in acceptance run 4 section 3.1). The aarch64 row reads +9.8%
this run (its spread at unchanged code is +8.3..+11.0% over six
measurements), inside the band on its own here and covered by the same
decision where it is not. alloc-test1's RSS (+5.8% / +4.6%, 13.0 vs 12.3
MiB) and every other RSS row are inside on both VMs. Nothing in the Phase
2 code moved a Phase 0 row: the run-4b column (code 2f218f9, before Phase
2) and this run agree within each row's spread (largest differences:
aarch64 glibc-simple -4.8% -> -9.2% and xmalloc-test -6.2% -> -7.9%,
both in challoc's favour; x86_64 xmalloc-test +0.7% -> +3.3% and cfrac
+3.6% -> +4.7%, rows whose history spans -3.6..+4.7% and +0.3..+4.7%).

## 3. larson: locks and atomics (criterion 1, Phase 0)

`larson 5 8 1000 5000 100 4141 16` in the dev container under
`LD_PRELOAD=libchalloc.so` (this tree's release build), `sudo perf record
-F 999 -a -- sleep 7` on the VM host, `perf report --symfs` over the
target volume (`~/perf-larson-p2.sh`, the acceptance-4 recipe with a new
output directory; `~/logs/acc-p2/perf-larson.txt`).

Share of all samples by DSO, then challoc's symbols (perf names the fast
paths by their `operator new[]`/`operator delete[]` aliases; they are
`je_malloc`/`je_free`). Load before the record: 2.70 / 3.62 (1 min; the
protocol run had ended 10 s earlier), nothing else running.

aarch64: larson 53.91%, libchalloc.so 45.31%, kernel 0.73%; 563.8 M
ops/s in the recorded run.

```
32.69%  operator delete[](void*)            (je_free)
10.94%  operator new[](unsigned long)       (je_malloc)
 0.45%  core::slice::sort::unstable::quicksort   (cache compaction sort)
 0.22%  ClassState::claim_with
 0.18%  __aarch64_ldadd4_acq_rel            (outlined LSE atomic add)
 0.15%  ClassState::release_owned
 0.08%  __aarch64_ldclr8_rel                (outlined LSE atomic clear)
 0.07%  challoc::api::libc::free_slow
 0.06%  ClassState::bin_stats
 0.06%  Arena::decay_with
 0.05%  __aarch64_cas4_acq                  (outlined LSE CAS)
 0.04%  core::slice::sort::unstable::ipnsort
 0.04%  __aarch64_ldset8_acq
 0.04%  ClassState::dec_live
 0.03%  ClassState::release_word
```

x86_64: larson 63.18%, libchalloc.so 36.07%, kernel 0.69%; 303.6 M ops/s.

```
21.36%  operator delete[](void*)            (je_free)
13.08%  operator new[](unsigned long)       (je_malloc)
 0.48%  core::slice::sort::unstable::quicksort
 0.28%  ClassState::claim_with
 0.24%  ClassState::release_owned
 0.11%  ClassState::release_word
 0.09%  challoc::api::libc::free_slow
 0.08%  ClassState::dec_live
 0.05%  Arena::decay_with
 0.04%  ClassState::bin_stats
 0.03%  ClassState::reopen
 0.03%  ClassState::take
 0.03%  core::slice::sort::shared::pivot::median3_rec
 0.03%  Heap::free_other
 0.03%  core::slice::sort::unstable::ipnsort
```

Reading: as in Phase 1.5 runs 1 and 4. On x86_64 no lock or atomic RMW
symbol is in the top 15 (the atomics are inline `lock` instructions inside
`claim_with` / `release_owned` / `dec_live`, together 0.60% of samples). On
aarch64 Rust outlines the atomics into `__aarch64_*` helpers, so four of
them are separate symbols at ranks 5, 7, 11 and 13 (0.18 / 0.08 / 0.05 /
0.04%; run 4: 0.15 / 0.10 / 0.06% at ranks 6, 7, 10), all called from the
slab claim / release paths (`claim_with` is task 5's renamed `claim`),
none from `je_malloc` or `je_free`: PASS on substance, as settled in run 1
(the literal-rank reading and the `-C target-feature=+lse` remedy are
recorded there). The profile's shape is unchanged by Phase 2: `je_free`
32.7% / `je_malloc` 10.9% against 32.9% / 10.5% (run 4, aarch64), 21.4% /
13.1% against 21.7% / 13.4% (x86_64); the `push_flags` byte test of task
3 replaced the `double_free_scan` bool at the same place and `free` is
byte-identical (`2026-09-18-*-gce-tunables.md` section 5).

## 4. Valkey suite (criterion 1, Phase 1)

`make valkey STAGE=memefficiency` on the C4A VM (`~/logs/acc-p2/memeff.log`,
02:15..02:22 UTC, right after the protocol run; the stage relinks Valkey
9.1.2 against the tree's quantum-8 `libchalloc.so` first, `malloc=jemalloc-
5.3.0-challoc`, `DT_NEEDED libchalloc.so.0`) and, on the C4 VM, `make valkey
STAGE=all TEST_FLAGS='--clients 8'` (`unit/memefficiency` alone, then the
whole suite; `~/logs/acc-p2/valkey-all.log`).

`unit/memefficiency` (the defrag tests with `activedefrag` on): green on
both, "All tests passed without errors!", 33 `[ok]`: the five `Memory
efficiency with values in range` cases (32 .. 16384), `Active defrag - AOF
loading`, and the `Active Defrag` cases for the main dictionary, eval
scripts, big hash / set / list / zset (random and fixed score), pubsub and
stream, each standalone and in cluster mode. 420 s on aarch64, 434 s on
x86_64.

The full suite on x86_64 (`./runtest --dump-logs --clients 8`, 164 units,
02:24..02:43 UTC, 1102 s): **"All tests passed without errors!"**, 5227
`[ok]` over the two stages (33 + 5194), 0 `[err]`, 0 `[exception]`, no
"Killed by signal" or "Timed out"; the long units are `defrag` (434 s),
`integration/replication-psync` (238 s), `integration/replication` (179 s)
and `integration/dual-channel-replication` (131 s). The last full run
(2732f6f / 6fed468, 2026-09-17, PROGRESS.md Phase 1 task 7 entry) had 6
failures: the four `Active Defrag` "didn't stop" cases, the `mptcp yes`
fixture and the dual-channel "backlog refcount" case; none of them
recurs at 7087c44 (the defrag cases passed standalone and in cluster mode
in both stages, the MPTCP fixture binds on the GCE kernel, the
dual-channel unit passed with the defrag unit running alongside as
before). The 26 `[ignore]`/`[skip]` lines are the harness's own 22 x `large memory flag not provided`, 4 x `Other server path not provided`), none allocator-related.

The four Valkey controls of the Phase 1 acceptance (`activedefrag`,
`active-defrag-threshold-*`, `active-defrag-cycle-*`, `MEMORY PURGE` /
`MEMORY MALLOC-STATS`) are what `unit/memefficiency` exercises through
`CONFIG SET` and `INFO memory` (`frag_bytes`, `allocator_frag_ratio`,
`active_defrag_running`), green above; the soak (section 5) drives
`activedefrag yes` for 40 minutes and samples the same `INFO memory`
lines.

## 5. The 20-minute Valkey soak with the perf sidecar (criterion 1, Phase 1)

`make valkey-soak DURATION=1200 SETTLE=60` on the C4A VM right after the
`memefficiency` stage had relinked Valkey 9.1.2 against the tree's
`libchalloc.so` (the full protocol: `ALLOCS="challoc jemalloc"`, 600 s
write then 600 s delete per allocator, 60 s settle, `activedefrag yes`,
stock memtier key pattern, the same tag with bundled jemalloc 5.3.0 as the
baseline), 02:22..03:05 UTC, nothing else on the VM, with
`bench/probes/valkey-perf-sidecar.sh ~/logs/acc-p2/sidecar 700 1205`
started with it on the VM host (both servers attached from their second
second: challoc pid 163067 t0 1789784569, perf from 1789784571; jemalloc
pid 169616 t0 1789785870, perf from 1789785874). Summary file
`2026-09-18-aarch64-gce-phase-2-acceptance-soak-valkey-soak.md` with its
two `-challoc.csv` / `-jemalloc.csv` sample files (renamed from the
harness's `2026-09-19-aarch64-valkey-soak.*`; it says `commit: 4c7ced6`).
The last soak was task 14's at 75bdbc5 (`2026-09-18-aarch64-gce-usable-
size-chain-soak-valkey-soak.md`); since it the free path's flag test
(tasks 9c, 3), the cgroup sample in the tick (task 4) and the policy's
loads (task 5) landed on the paths Valkey exercises, and Valkey runs
challoc with `background_thread:true` (`challoc-bg` in the thread list),
so the whole 20-minute protocol was rerun. Positive = challoc worse.

| measure | challoc | jemalloc | challoc vs jemalloc | 75bdbc5 (task 14) |
|---|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 925256 | 883937 | **-4.5%** | -3.6% |
| write: valkey-benchmark p99 ms | 1.047 | 1.095 | -4.4% | -4.4% |
| write: memtier req/s | 957372 | 938846 | -1.9% | -0.0% |
| write: memtier p99 ms | 0.943 | 0.975 | -3.3% | +1.6% |
| delete: valkey-benchmark req/s | 1415462 | 1434899 | **+1.4%** | +3.0% |
| delete: valkey-benchmark p99 ms | 0.711 | 0.703 | +1.1% | +3.5% |
| delete: memtier req/s | 1241246 | 1256101 | +1.2% | +3.0% |
| delete: memtier p99 ms | 0.759 | 0.759 | +0.0% | +4.3% |
| peak RSS MiB (VmHWM) | 579.9 | 574.3 | +1.0% (band 5%) | +0.3% |
| RSS MiB at end of delete phase | 129.8 | 129.5 | +0.2% | -2.0% |
| RSS MiB after settle | 129.8 | 129.4 | +0.3% (band 10%) | -2.1% |
| allocator_frag_ratio after settle | 1.81 | 1.80 | +0.6% | -0.6% |
| active_defrag_hits | 243433 | 246200 | +1.1% | +1.3% |
| keys after settle | 105262 | 103066 | | |

Write phase: ahead on both generators in throughput and p99 (the Phase 1
criterion as written). Delete phase: +1.4% / +1.2% behind in throughput
and +1.1% / +0.0% in p99, the smallest delete gap of any soak (task 14's
+3.0%, 4f's +3.2..4.6%, 4e's +3.8%; scorecard soak-delete column
+3.0..+7.3%). Both RSS criteria met. Cycles per command from the sidecar
(`perf stat -I 1000` on the server, task-clock plus five PMU events,
divided by `total_commands_processed` between the sidecar's snapshots;
`.tmp-percmd.py`, perf's start aligned to the CSV's mtime minus its last
interval, +-1 s on a 60 s window):

| window | alloc | commands/s | cyc/cmd | ins/cmd | CPI | L1D miss/cmd | dTLB refill/cmd | br-miss/cmd | main thread |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| write, unrecorded, t=30..100 | challoc | 965k | **3022** | 9527 | 0.32 | 44.8 | 53.8 | 7.3 | 100% |
| write, unrecorded, t=30..100 | jemalloc | 933k | 3122 | 9837 | 0.32 | 42.3 | 38.5 | 7.4 | 100% |
| write, under `perf record`, t=100..162 | challoc | 771k | 3195 | 9744 | 0.33 | 45.1 | 59.0 | 6.5 | 100% |
| write, under `perf record`, t=100..161 | jemalloc | 753k | 3258 | 9721 | 0.34 | 45.1 | 42.3 | 6.8 | 100% |
| write, unrecorded, t=162..700 | challoc | 1013k | **2880** | 9079 | 0.32 | 39.5 | 50.6 | 6.2 | 100% |
| write, unrecorded, t=161..700 | jemalloc | 980k | 2970 | 9275 | 0.32 | 38.4 | 38.8 | 6.4 | 100% |
| delete, under `perf record`, t=700..761 | challoc | 1099k | **2238** | 7167 | 0.31 | 24.4 | 25.3 | 5.2 | 100% |
| delete, under `perf record`, t=700..762 | jemalloc | 1083k | 2272 | 7432 | 0.31 | 25.4 | 24.0 | 5.2 | 100% |
| delete, unrecorded, t=761..1205 (into the settle) | challoc | 1321k | 2207 | 7186 | 0.31 | 23.9 | 25.2 | 5.0 | 100% |
| delete, unrecorded, t=762..1205 | jemalloc | 1337k | 2178 | 7266 | 0.30 | 24.8 | 23.9 | 5.1 | 100% |
| whole life to t=1205 | challoc | | 2647 | 8442 | | 33.2 | 39.6 | 5.8 | task-clock 1240 s |
| whole life to t=1205 | jemalloc | | 2596 | 8319 | | 32.0 | 31.4 | 5.8 | task-clock 1202 s |

Against task 14's run (challoc 3046 / jemalloc 3141 in the first write
window, 2922 / 2967 in the long one, 2292 / 2280 in the recorded delete
window): the write phase is 3.2% and 3.0% fewer cycles per command than
jemalloc, the recorded delete window 1.5% fewer (2238 vs 2272; it was +0.5%
at task 14 and +2.9% at 4f), the last unrecorded stretch +1.3% (2207 vs
2178); instructions per command are below jemalloc's in every window
(the sized free path and the dense bitmap), L1D misses per command at
parity, dTLB refills 1.3..1.5x jemalloc's (the per-class regions' address
spread, as in every soak). Both servers are main-thread bound (100% of one
CPU), so cycles per command is the throughput; the whole-life figure
includes the settle, where challoc's server spends 38 s more task-clock
(1240 vs 1202 s: the background thread's decay walks and the defrag
cycles on a heap that ends at the same 129.8 MiB). `perf report` over the
records (`--symfs` on the target volume; challoc's fast paths are
`je_malloc_usable_size`, `je_sdallocx`, `je_malloc`): write phase
`libchalloc.so` 11.27% of the server's samples (`je_malloc_usable_size`
7.35%, `je_sdallocx` 2.37%, `je_malloc` 1.41%) against jemalloc's in-binary
`je_malloc_usable_size` 7.78% + `je_sdallocx` 2.01% + `je_malloc` 1.44% =
11.23%; delete phase 11.45% (6.88 / 2.25 / 1.34) against 4.19 + 2.15 +
1.46 = 7.80%: the usable-size query is where challoc spends more in the
delete phase (Valkey's `zfree` calls it before every `sdallocx`, task 14's
chain), and it is the residual the +1.2..1.4% throughput gap is made of.

Phase 1 soak criteria: throughput and p99 ahead in the write phase and
+0.0..+1.4% behind in the delete phase (the same reading as the Phase 1
and 1.5 acceptances, which took the delete residue with its attribution;
this run's residue is the smallest so far and attributed above), peak RSS
+1.0% (band 5%), RSS after settle +0.3% (band 10%): met.

## 6. Ruby (criterion 1, Phase 1)

`make ruby` on the C4A VM (`contrib/ruby/build.sh` STAGE=all: Ruby 3.4.10
`--with-jemalloc` against challoc's `libjemalloc.so.2` re-staged from the
tree, the DT_NEEDED / rpath / `HAVE_MALLOC_CONF` / mapped-library checks,
then `make test-all TESTOPTS="-j16 -q --tty=no"`; `~/logs/acc-p2/ruby.log`,
03:05..03:11 UTC, 338 s for the suite):

```
32848 tests, 6585481 assertions, 7 failures, 2 errors, 417 skips   (rc=2, as any failure count above 0 gives)
```

The nine failing tests, sorted, are exactly the nine of the 38f1a16 run
(`2026-09-17-ruby-test-all.md` run 2: 32827 tests, 7 failures, 2 errors)
that were shown there to fail identically under glibc malloc:
`OpenSSL::TestProvider#test_openssl_legacy_provider`,
`OpenSSL::TestSSL#test_connect_works_when_setting_dh_callback_to_nil`,
`OpenSSL::TestSSL#test_tlsext_hostname`, `OpenSSL::TestSSL#test_tmp_dh`,
`OpenSSL::TestSSL#test_tmp_dh_callback` (Wolfi's OpenSSL policy and the
missing legacy provider), `RDocParserTest#test_class_for_forbidden`,
`TestFileUtils#test_rm_r_no_permissions` (error),
`TestTmpdir#test_world_writable` (root in the container),
`TestFiddle#test_dlopen_linker_script_group_linux` (Wolfi's `libc.so`
linker script). No crash, abort, hang, `challoc:` line or memory
assertion in 32.8k tests; the acceptance line ("passes `make test-all`,
or the failures are shown to be unrelated to allocation") is met in its
second form, as before. Not rerun on x86_64: the suite's outcome was
identical on both architectures at 38f1a16 and the `ruby-bench` rows
below cover x86_64 at this code.

`make ruby-bench HOST_KIND=gce-c4` on x86_64 (`make ruby STAGE=build` for
both allocators first, 9 s: the builds under `target/ruby/` were current;
the script re-stages challoc's `libjemalloc.so.2` from the tree),
`2026-09-18-x86_64-ruby-phase-2-acceptance.md`, 5 rounds after a warm-up,
allocators interleaved, load 0.39 at the start, 310 s. challoc against
jemalloc (time / RSS; the 2026-09-18 run at 5304cd4,
`2026-09-18-x86_64-ruby.md`, in brackets):

| row | challoc s | jemalloc s | time | RSS | 5304cd4 time / RSS |
|---|---:|---:|---:|---:|---|
| startup | 0.042 | 0.040 | +5.0% (2 ms at 1 ms resolution: 0.041 0.041 0.042 0.042 0.042 vs 0.040 0.040 0.040 0.041 0.041) | -7.2% | +2.4% / -4.9% |
| so_binary_trees | 3.859 | 3.837 | +0.6% | -4.1% | +0.0% / -27.1% |
| hash_aref_str | 0.289 | 0.281 | +2.8% | -10.3% | +1.6% / -9.6% |
| array_sample_100k__1k | 0.441 | 0.445 | -0.9% | -2.2% | -1.2% / -2.0% |
| array_sort_block | 4.104 | 4.136 | -0.8% | -4.2% | -0.6% / -9.2% |
| so_nbody (control) | 0.688 | 0.683 | +0.7% | -9.4% | -0.5% / -11.0% |
| app_fib (control) | 0.266 | 0.255 | +4.3% (0.255 0.255 0.266 0.266 0.268 vs 0.249 0.253 0.255 0.259 0.264: overlapping lists on a row that allocates nothing) | -9.8% | -0.4% / -9.2% |
| erb_render | 0.631 | 0.778 | -18.9% | -8.1% | -17.1% / -7.1% |
| gem_list | 0.111 | 0.111 | +0.0% | -2.5% | -1.7% / -2.0% |
| gc_churn | 1.811 | 1.807 | +0.2% | +7.5% (383.0 vs 352.6 MiB: challoc's RSS is unchanged from the 5304cd4 run's 384.7; jemalloc's run had 52 GCs instead of 54 and 356 MiB VmHWM instead of 380) | -2.6% / +1.2% |

Every timed row is within its previous band (the two rows above 3%,
`startup` and `app_fib`, are at the timer's resolution and on the
no-allocation control), RSS is below jemalloc's on every row but
`gc_churn`, and `erb_render` stays 19% ahead. Ruby is not a Phase 2 target
(no hardening item touches what Ruby exercises beyond the flag byte); the
rows are the Phase 1 acceptance's own measure kept current.

## 7. Container tests (criterion 3)

`make test-cgroup CONTROL=1` (the churn at both limits with the policy on
and off, the 300 s oscillation, then the same oscillation under
`cgroup:false`, recorded not judged) on both VMs, and `make
test-cgroup-compare` (jemalloc 5.3.1-r3, mimalloc 3.5.2, `report` churn
at both limits and the oscillation) on x86_64, so that both architectures
have the baseline: aarch64's is task 7's run in
`2026-09-18-aarch64-gce-cgroup-container-tests.md` (library e7dfb95,
unchanged by tasks 6 and 7; the churn rows also ran at 7087c44 inside
`make test` on both VMs this run, section 1). One `docker run --memory=L
--memory-swap=L` container per run, `scripts/cgroup-tests.sh`,
`~/logs/acc-p2/cgroup.log` and `cgroup-compare.log`.

Churn (allocate, free, idle 2 s; RSS MiB from `/proc/self/statm`):

| VM | limit | churn / threshold | allocator | RSS peak | RSS at 2 s | under the threshold after | `memory.current` peak / 2 s | criterion | result |
|---|---|---|---|---:|---:|---:|---|---|---|
| x86_64 | 512m | 400 / 100 | challoc | 403 | 13 | 46 ms | 405 / 15 | `purge` | pass |
| x86_64 | 512m | 400 / 100 | challoc `cgroup:false` | 403 | 316 | never | 405 / 318 | `keep` | pass |
| x86_64 | 256m | 200 / 50 | challoc | 203 | 8 | 79 ms | 203 / 9 | `purge` | pass |
| x86_64 | 256m | 200 / 50 | challoc `cgroup:false` | 202 | 162 | never | 204 / 163 | `keep` | pass |
| x86_64 | 512m | 400 / 100 | jemalloc 5.3.1 | 423 | 197 | never | 423 / 195 | (baseline) | 197 MiB kept |
| x86_64 | 256m | 200 / 50 | jemalloc 5.3.1 | 214 | 127 | never | 213 / 126 | (baseline) | 127 MiB kept |
| x86_64 | 512m | 400 / 100 | mimalloc 3.5.2 | 408 | 408 | never | 408 / 408 | (baseline) | nothing returned |
| x86_64 | 256m | 200 / 50 | mimalloc 3.5.2 | 208 | 208 | never | 209 / 208 | (baseline) | nothing returned |
| aarch64 | 512m | 400 / 100 | challoc (this run: `make test`, then `make test-cgroup`) | 403 | 13, 12 | 49 ms, 50 ms | 409 / 18 | `purge` | pass |
| aarch64 | 512m | 400 / 100 | challoc `cgroup:false` | 403 | 321, 317 | never | 406 / 324 | `keep` | pass |
| aarch64 | 256m | 200 / 50 | challoc | 202 | 8, 8 | 81 ms, 80 ms | 204 / 8 | `purge` | pass |
| aarch64 | 256m | 200 / 50 | challoc `cgroup:false` | 202 | 161, 160 | never | 204 / 161 | `keep` | pass |
| aarch64 | 512m / 256m | | jemalloc 5.3.1 (task 7) | 409 / 206 | 190 / 124 | never | | (baseline) | 190 / 124 MiB kept |
| aarch64 | 512m / 256m | | mimalloc 3.5.2 (task 7) | 407 / 207 | 407 / 207 | never | | (baseline) | nothing returned |

`stats.cgroup.*` at 2 s on x86_64: 512m `limit 512 MiB decay_ms 10000
retention_cap 25 MiB empty_bytes 9 MiB full_purges 0` (the peak sample
read `decay_ms 6530`, the scaled band at 405 of 512 MiB); 256m `decay_ms
6425` at the peak, `retention_cap 12 MiB empty_bytes 5 MiB` at 2 s; the
`keep` rows say `ENOENT (cgroup not tracked)`. `make test`'s churn rows
this run also ran on x86_64 (section 1: 13 / 316 / 8 / 161 MiB at 2 s).

Oscillation (200..480 MiB live, 5 minutes, 512 MiB, `memory.events` after
the run, all 0 before):

| VM | allocator | outcome | cycles | peak `memory.current` | `memory.events` after | RSS at the low end |
|---|---|---|---:|---:|---|---|
| x86_64 | challoc | alive, exit 0 | 97 in 303 s | 502 MiB | max 0, high 0, oom_kill 0 | 346..385 MiB right after the frees (cycles 1..5 and 95..97); `stats.cgroup.current` 297 MiB at the end, `empty_bytes 25 MiB` at the 25 MiB cap, `full_purges 0`, `pressure_events 0` |
| x86_64 | challoc `cgroup:false` (control) | alive, exit 0 | 98 in 301 s | 506 MiB | max 0, high 0, oom_kill 0 | 489 MiB: nothing returned, `Empty` memory reused in place |
| x86_64 | jemalloc 5.3.1 | **OOM-killed at cycle 24 (70 s), exit 137** | 23 | 512 MiB (`memory.peak`) | max 35, oom 1, oom_kill 1 | 503..508 MiB from the first descent |
| x86_64 | mimalloc 3.5.2 | **OOM-killed at cycle 2 (6 s), exit 137** | 1 | 512 MiB (`memory.peak`) | max 53, oom 1, oom_kill 1 | 492 MiB after the first descent |
| aarch64 (this run) | challoc | alive, exit 0 | 98 in 301 s | 502 MiB | max 0, high 0, oom_kill 0 | 368..383 MiB right after the frees (cycles 1..3 and 98); `stats.cgroup.current` 294 MiB at the end, `empty_bytes 22 MiB`, `full_purges 0`, `pressure_events 0` |
| aarch64 (this run) | challoc `cgroup:false` (control) | alive, exit 0 | 99 in 301 s | 506 MiB | max 0, high 0, oom_kill 0 | 492 MiB |
| aarch64 (task 7) | challoc | alive | 98 | 502 MiB | 0 / 0 / 0 | settled 293 MiB |
| aarch64 (task 7) | challoc `cgroup:false` | alive | 99 | 507 MiB | 0 / 0 / 0 | 485..501 MiB |
| aarch64 (task 7) | jemalloc 5.3.1 | OOM-killed at cycle 5 (12 s) | 4 | 512 MiB | max 36, oom_kill 1 | 501..507 MiB |
| aarch64 (task 7) | mimalloc 3.5.2 | OOM-killed at cycle 2 (6 s) | 1 | 512 MiB | max 54, oom_kill 1 | 492 MiB |

Criterion 3 on both architectures: the churn is under 100 MiB (50 MiB at
256m) within 2 s with the policy on (46..81 ms) and not with it forced
off (316..324 / 161..163 MiB at 2 s), and the oscillation is not
OOM-killed over 5 minutes; jemalloc's behaviour is the baseline in both
tables (it keeps about half the churn, 190..197 MiB / 124..127 MiB, and is
OOM-killed in the oscillation, at cycle 5 on aarch64 and cycle 24 on
x86_64, where its 16 arenas' slabs happened to fit the requests for
longer before `memory.current` reached the limit 35 times); mimalloc 3
returns nothing in 2 s and dies at the second climb on both. Task 6's
two readings (PROGRESS.md, Notes for the human) stand: the control
survives as well, and the criterion separates challoc from the other
allocators rather than the policy from its absence.

## 8. Default hardening cost (criterion 2)

The criterion reads "no more than 3% on the Phase 0 benchmark set relative
to Phase 0 results". The Phase 0 results (`bench/results/2026-09-1[456]-*.md`)
are Docker Desktop numbers on the Mac, and a Mac number is never compared
with a GCE number (CLAUDE.md), so the literal baseline is unusable. **This
run reads the criterion as: the cost, on the Phase 0 set's rows and the
`quick`/`ab` rows, of the default-on hardening that landed since the Phase
1.5 reference protocol run (`2026-09-17-gce-c4a.md`, `-c4.md`, commit
166a464, the first GCE numbers), each item priced in its own same-run
`quick`, `ab` or `quick-perf` on the reference VMs, and their worst case
summed per row.** This is an interpretation, flagged for the human in
PROGRESS.md; no Phase-0-relative number is fabricated.

The default-on hardening since 166a464, with each item's same-run figures
(positive = challoc slower than before the item; the source file names the
run):

| item | what is on the default path | aarch64 same-run | x86_64 same-run | attribution |
|---|---|---|---|---|
| Phase 1.5 task 8: release assertions (`2026-09-18-{aarch64,x86_64}-gce-release-asserts.md` section 3) | slow-path aborts only; `overflow-checks` rejected (5..18%, section 1) | `quick` larson1 +0.0%, larson10 -1.8%, xmalloc +0.3%, alloc-test1 +0.8%, valkey1 +0.3% | `quick` larson1 -1.6%, larson10 -2.2%, xmalloc -3.6%, alloc-test1 +0.4%, valkey1 -1.1% | no fast-path instruction added (every check is on a slow path) |
| Phase 1.5 task 9 (a)+(b): flush-boundary compare, guard-slab test on the sized path, `Fresh`/guard at the release (`2026-09-18-*-gce-detection-gaps.md` sections 2, 3) | one compare per flush; a shift and a 32-bit modulo per sized free | `quick` larson1 -0.2%, larson10 -1.9%, xmalloc +2.3%, alloc-test1 -0.2%, valkey1 +1.9% | `quick` larson1 +0.4%, larson10 -2.7%, xmalloc +0.2%, alloc-test1 +0.9%, valkey1 +1.3% | `quick-perf` arm64: valkey1 +12.6 / +10.8 instructions per op (239.9 -> 252.5, 240.1 -> 250.9), cycles per op +1.6% / -0.8% (two samples); larson1 245.9 -> 246.0 |
| Phase 1.5 task 9 (c): `double_free_scan` flag, off (`2026-09-18-*-gce-detection-gaps.md` sections 4, 5) | one byte load and a predicted branch per push | `ab` 5 rounds: larson1 +0.7%, valkey1 +0.0%, redis -0.2% | `ab` 5 rounds: larson1 +0.4%, valkey1 +4.6% (lists overlap through base's low outlier; the counters say it is not an instruction cost), redis -1.8% | `quick-perf`: +4.0 instructions per larson1 op on both (246.0 -> 250.0, 231.1 -> 235.1), L1D and dTLB misses unchanged |
| Phase 2 task 2: `mseal` (`2026-09-18-aarch64-gce-mseal.md`) | init only, 57 `mseal` calls | first `malloc` 100 -> 188 us (+88 us); `quick` go, every row within 5% | not run (init-only, sealing is skipped under Docker's default seccomp on both VMs) | no fast path touched |
| Phase 2 task 4: the cgroup sample in the periodic tick (PROGRESS.md task 4 entry) | one rate-limited sample per 10 ms in the slow-path tick | `quick` OLD=6c9b0e2: larson1 +0.5%, larson10 -0.8%, xmalloc -0.2%, alloc-test1 +0.2%, valkey1 +1.9% | not run (arm64 gate only; slow path) | no default fast path changed |
| Phase 2 task 5: the design 11 policy (PROGRESS.md task 5 entry) | one load on a line the tick reads; two loads and a return without a limit | `quick` OLD=13c434d: larson1 +0.0%, larson10 +0.7%, xmalloc +0.3%, alloc-test1 -0.6%, valkey1 -0.4% | not run (arm64 gate only; slow path) | no default fast path changed |
| Phase 2 task 3: `push_flags` byte (`2026-09-18-{aarch64,x86_64}-gce-tunables.md` sections 2, 4, 5) | the byte replaces the `double_free_scan` bool at the same place | `quick` larson1 +0.2%, larson10 -1.4%, xmalloc -0.7%, alloc-test1 -0.6%, valkey1 +2.2%; `ab` 5 rounds off vs base: larson1 +0.0%, larson10 +0.9%, valkey1 -0.9% | `quick` larson1 -7.9%, larson10 +2.6%, xmalloc -3.0%, alloc-test1 -0.6%, valkey1 -1.6%; `ab` off vs base: larson1 +4.8% (lists 44.0..48.7 vs 45.0..51.4), larson10 +1.3%, valkey1 -6.7% | `quick-perf`: larson1 249.8 -> 249.8 and 234.7 -> 234.8 instructions per op; `free` byte-identical in `objdump` |

Not counted: Phase 2 task 1 (guard slabs were already on by default; the
task added the tests), task 6 and 7 (tests and a comparison run, library
unchanged), and the Phase 1.5 performance tasks (4a..4f, 5, 12, 14, 15),
which are not hardening.

Worst-case sum per row, taking each item's positive same-run figure at face
value and 0 for a negative one (single-round `quick` deltas included, so
this is an upper bound made mostly of noise; the `valkey1` row's one-round
spread on the same code is about 5%, see the mseal and task 6 gates at
-5.2% and -1.3% with an unchanged library):

| row | aarch64 worst-case sum | x86_64 worst-case sum | of which attributed to an instruction on a default path |
|---|---:|---:|---|
| larson1 | 0.0 + 0 + 0.7 + 0.5 + 0.0 + 0.2 = **1.4%** | 0 + 0.4 + 0.4 + 4.8 = **5.6%** (the 4.8 is the task 3 `ab` row whose lists overlap; `quick` had it at -7.9% and the instruction count is identical) | +4.0 instructions per op (the `double_free_scan` flag load and branch, task 9c), 1.6% of 246 |
| larson10 | 0 + 0 + 0 + 0.7 + 0.9 = **1.6%** | 0 + 0 + 2.6 + 1.3 = **3.9%** (task 3 `quick` single round and `ab`; instruction count identical) | the same +4 instructions |
| xmalloc | 0.3 + 2.3 + 0 + 0.3 + 0 = **2.9%** | 0 + 0.2 + 0 + 0 = **0.2%** | none beyond the flag load |
| alloc-test1 | 0.8 + 0 + 0.2 + 0 + 0 = **1.0%** | 0.4 + 0.9 + 0 + 0 = **1.3%** | none beyond the flag load |
| valkey1 | 0.3 + 1.9 + 0.0 + 1.9 + 0 + 2.2 = **6.3%** (`ab` figures instead of `quick` where both exist: 0.3 + 1.6 + 0.0 + 1.9 + 0 + 0 = **3.8%**) | 0 + 1.3 + 4.6 + 0 = **5.9%** (with the counters' reading of the 4.6: 1.3%) | +12.6 / +10.8 instructions per op (task 9b's guard test), +1.6% / -0.8% cycles per op; +4 for the flag |
| redis | 0 + 0 = **0.0%** | 0 | none |

The attributed cost, the part that is an instruction on a default path, is
+4 instructions per free (the `double_free_scan` flag) and +11..13
instructions per Valkey operation (the guard-slab test on the sized path),
about 1.6% and 5% of the respective instruction counts, and within noise
in cycles per operation on both architectures in every same-run
measurement. The worst-case sums of the single-round point estimates
exceed 3% on the `valkey1` row (both VMs) and on x86_64 larson rows; each
of those sums is dominated by one single-round `quick` delta or one
overlapping-lists `ab` row whose instruction count is unchanged, and the
same rows measured with 5 rounds sit at 0..1.9%.

The scorecard (`bench/scorecard.sh --render`, challoc against jemalloc in
the same run, 3 rounds, the same VMs) bounds everything that landed since
the reference run, hardening and performance work together:

| row | aarch64 166a464 -> 7087c44 | x86_64 166a464 -> 7087c44 |
|---|---|---|
| larson16 | +11.9% -> +3.1% | +29.8% -> +8.9% |
| alloc-test1 | +4.0% -> +5.1% | +2.7% -> +4.1% |
| alloc-testN | +1.9% -> +3.0% | +4.9% -> +6.4% |
| cfrac | +1.0% -> +0.5% | +3.8% -> -1.3% |
| espresso | -1.8% -> -2.0% | -0.9% -> -5.7% |
| mstress (time) | -43.6% -> -40.4% | -41.3% -> -39.4% |
| glibc-simple | +0.0% -> -8.0% | -0.3% -> +1.9% |
| redis | -13.4% -> -18.5% | -16.8% -> -22.6% |

No row's gap to jemalloc widened by more than 3.2 points (mstress time on
aarch64, where challoc is 40% ahead; alloc-test1/-testN +1.1..1.5 points;
glibc-simple x86_64 +2.2 points), and the larson, cfrac, glibc-simple
(aarch64) and redis rows narrowed. This run's protocol tables (section 2)
against the reference run are in the same band: this run against the reference run (`2026-09-17-gce-c4a.md` / `-c4.md`,
challoc vs jemalloc time gap, points): aarch64 cfrac +1.0 -> +0.6, espresso
-1.8 -> -2.0, larson +11.9 -> +3.8, mstress -43.6 -> -42.6, xmalloc-test
+29.8 -> -7.9, alloc-test1 +4.0 -> +3.8, alloc-testN +1.9 -> +2.8,
glibc-simple +0.0 -> -9.2, redis -13.4 -> -18.2; x86_64 cfrac +3.8 -> +4.7,
espresso -0.9 -> -0.4, larson +29.8 -> +8.7, mstress -41.3 -> -41.2,
xmalloc-test -32.7 -> +3.3, alloc-test1 +2.7 -> +5.8, alloc-testN +4.9 ->
+8.6, glibc-simple -0.3 -> +8.5, redis -16.8 -> -20.1; RSS mstress +4.7 ->
+9.8 and +7.1 -> +19.0. The x86_64 rows that widened by more than 3 points
(xmalloc-test, glibc-simple, alloc-testN, alloc-test1) and the mstress RSS
rows did so in the Phase 1.5 design tasks, not in hardening: xmalloc-test
and mstress RSS with the 16-arena default (task 4c, 8408d72: xmalloc-test
-36.1% -> -2.4% in its own A/B, `2026-09-17-x86_64-gce-narenas-default.md`;
mstress RSS attributed and accepted by task 15), glibc-simple with the
sized-free / dense-bitmap chain (scorecard: -0.3% at 166a464, +2.3..+2.8%
at e1419fd / 27de567, +6.0% at c4a0772, with tasks 8 and 9 at 50ec6fc /
3dcb16e reading +6.1% / +4.4%, no step; acceptance run 4 section 3.2 reads
the row as an instruction-count constant of the code since then), and
alloc-test1 / alloc-testN in the same span (scorecard +4.5% / +6.7% at
e1419fd already). Phase 1.5's acceptance (run 4, 2f218f9) accepted every
one of those rows on these VMs, and this run's protocol tables agree with
run 4b within each row's spread (section 2), which is the direct
measurement of what Phase 2's default-on code (tasks 2, 4, 5, 3) cost on
the Phase 0 set: nothing resolvable.

Verdict under the reading: **PASS under the stated reading.** The default hardening's attributable
cost is +4 instructions per free (task 9c's flag) and +11..13 instructions
per Valkey operation (task 9b's guard test), within noise in cycles per
operation and in every 5-round `ab` row on both VMs (0..1.9%); Phase 2's
own default-on code moved no Phase 0 row against run 4b; the sums of
single-round point estimates exceed 3% on `valkey1` (6.3% / 5.9%) and on
x86_64 larson1 / larson10 (5.6% / 3.9%), each dominated by one single-round
`quick` delta or one overlapping-lists `ab` row at an unchanged
instruction count. The human should read this as an interpretation
(PROGRESS.md, Notes for the human): the literal criterion has no usable
baseline, and a literal "3% of the sum of the point estimates" would fail
on the valkey1 row on noise alone.

## 9. Inert keys (the review's addition)

`src/config.rs`: `pub const INERT_KEYS: &[&str] = &["thp", "stats_print"];`
(line 457). The review's list (review 2026-09-18 section 3, item 12) was
`thp`, `zero_on_free`, `slot_randomize`, `double_free_scan`, `cgroup`,
`stats_print`; four left it (task 9c, Phase 2 tasks 4 and 3). For every
key of the design 12 table, the reader outside `src/config.rs` (`grep -rnE
'\.<field>\b' src` minus `config.rs`; the field is the key's name):

| key | reader outside `config.rs` (first hit) | state |
|---|---|---|
| `background_thread` | `src/api/mod.rs:211` `purge::background::request(config.background_thread)` | acts |
| `dirty_decay_ms` | `src/heap.rs:641` `purge::decay_ns_from_ms(config.dirty_decay_ms)` (16 hits) | acts |
| `muzzy_decay_ms` | none; `Config` doc: "Accepted for compatibility; nothing reads it" | ignored by design (design 12: default "ignored", "accepted for compatibility": the jemalloc-compatibility class of `junk`, `zero`, `retain`, ..., with an `opt.muzzy_decay_ms` value because jemalloc has one); not in `INERT_KEYS` and not reported as an issue, on purpose: a jemalloc consumer setting it must not be refused under `abort_conf`. Recorded for the human (Notes). |
| `narenas` | `src/heap.rs:638` `config.effective_narenas(os::ncpus())`, `:673` `Arenas::init(.., config.narenas)` | acts |
| `tcache` | `src/config.rs:362` inside `Config::cap_for`, called from `src/heap.rs:1976` (`shared.config.cap_for(self.arena, arena, class)`) | acts |
| `tcache_max` | `src/config.rs:342` (`shared_class`, used by `cap_for`) from `src/heap.rs:1976` | acts |
| `tcache_nslots_small_max` | `src/config.rs:365` (`cap_for` -> `ladder`) from `src/heap.rs:1976` | acts |
| `lg_tcache_nslots_mul` | `src/config.rs:365` (`cap_for` -> `ladder`) from `src/heap.rs:1976` | acts |
| `tcache_gc_interval_ms` | `src/config.rs:376` `gc_interval_ns()` from `src/heap.rs:672` (`Shared.gc_interval_ns`), read at `src/heap.rs:2187` | acts |
| `tcache_compact_shift` | `src/heap.rs:2012` (`MODE_OFF` when 0), `:3174` | acts |
| `group_handoff` | `src/api/mod.rs:210` `class::set_group_handoff(config.group_handoff)` | acts (opt-in) |
| `thp` | `src/heap.rs:680..683` (task 5 forces it to `never` under a limit below 1 GiB, written and reported, `MADV_HUGEPAGE` not wired), `src/api/print.rs:690` "accepted, no effect" | **inert, documented exception** (design 12 "inert (planned)", design 10) |
| `lazy_free` | `src/api/mod.rs:208` `purge::configure(features.process_madvise, config.lazy_free)` | acts |
| `guards` | `src/heap.rs:640` `guard_install && config.guards` | acts |
| `guard_slab_interval` | `src/heap.rs:2865` `guard_interval_for(..)` | acts |
| `huge_retain_max` | `src/heap.rs:667` `h.set_retain_max(config.huge_retain_max)` | acts |
| `zero_on_free` | `src/heap.rs:650` (`push_flags`), `:1637`, `:1885` (`shared.zero_on_free()`) | acts (opt-in, Phase 2 task 3) |
| `slot_randomize` | `src/heap.rs:1762` `if shared.config.slot_randomize` | acts (opt-in, Phase 2 task 3) |
| `double_free_scan` | `src/heap.rs:650` (`push_flags`), `:1902` `push_cache(cache, p, shared.double_free_scan())` | acts (opt-in, Phase 1.5 task 9c) |
| `abort_conf` | `src/config.rs:702` `if cfg.abort_conf && issues > 0` inside `Config::load`, called from `api::init`; C tests `src/api/ctests.rs:344..377` | acts (at load; its effect is the abort) |
| `cgroup` | `src/purge.rs:437` `shared.cgroup.sample(now)`, `:604` `arm_pressure()` (45 hits; `cgroup:false` leaves `Shared.cgroup` untracked) | acts (Phase 2 tasks 4, 5) |
| `check_zero` | `src/api/mod.rs:209` `purge::set_check_zero(config.check_zero)`, `src/class.rs:1377`, `src/huge.rs:738` | acts |
| `stats_print` | `src/api/print.rs:694` "accepted, no effect"; `src/api/mallctl.rs:1286` `opt.stats_print` is `ENOENT` | **inert, documented exception** (nothing to print at exit) |
| `junk`, `zero`, `retain`, `oversize_threshold`, `percpu_arena`, `metadata_thp`, `confirm_conf` | none by design | ignored jemalloc keys (design 12's last row) |

The inert set is exactly `{thp, stats_print}`: PASS. `muzzy_decay_ms` is
read by nothing, as its design row says; it is the compatibility class,
not the inert class (it is not reported and `abort_conf` accepts it), and
whether it should join `INERT_KEYS` is a question for the human, since
doing so would make `abort_conf` refuse a jemalloc configuration that sets
it.

## 10. VMs

`challoc-bench start all` at 01:58 UTC (2026-09-19; the iteration's date
in the file names is the 18th, the iteration's start on the host).
`challoc-bench stop amd64` at 03:01 UTC as soon as its chain ended (its
last stage was `ruby-bench`); `stop arm64` at 03:23 UTC after the cgroup
stage; `challoc-bench status` shows both `TERMINATED`; `docker ps` on the
arm64 VM was empty before the stop. Nothing else was started. Running
time about 63 minutes (amd64) and 85 minutes (arm64).

## Scorecards

Not rerun: the lines for 7087c44 on both VMs exist (task 3's iteration,
`bench/results/scorecard-{aarch64,x86_64}.md`, dated 2026-09-19 UTC) and
this run's library is that code.
